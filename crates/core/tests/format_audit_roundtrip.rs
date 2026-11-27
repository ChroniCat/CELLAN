use sheetcore::{Cell, DateSystem, Sheet, SheetVisibility, Value, Workbook, formula, xlsx};
use std::io::{Read, Write};
use std::path::Path;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

fn num(n: f64) -> Cell {
    Cell {
        value: Value::Number(n),
        formula: None,
    }
}

#[test]
fn biff_macro_storage_blocks_lossy_save_on_every_output_format() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("macros.xls");
    xlsx::save_atomic(&source, &Workbook::new_blank()).unwrap();
    {
        let mut cfb = cfb::open_rw(&source).unwrap();
        cfb.create_storage_all("/_VBA_PROJECT_CUR/VBA").unwrap();
        cfb.create_stream("/_VBA_PROJECT_CUR/VBA/dir")
            .unwrap()
            .write_all(b"project")
            .unwrap();
    }
    let original = std::fs::read(&source).unwrap();
    let (mut book, diag) = xlsx::load_diag(&source).unwrap();
    assert!(book.has_vba);
    assert!(sheetcore::xls_read::load(&source).unwrap().has_vba);
    assert!(!diag.has_fatal());
    book.sheets[0].set(0, 0, num(123.));
    for target in [
        &source,
        &dir.path().join("new.xls"),
        &dir.path().join("new.xlsx"),
        &dir.path().join("new.xlsm"),
    ] {
        let err = xlsx::save_atomic(target, &book).unwrap_err();
        assert!(format!("{err:#}").contains("VBA"));
        if target != &source {
            assert!(!target.exists());
        }
    }
    assert_eq!(std::fs::read(source).unwrap(), original);
}

#[test]
fn biff_visibility_survives_loading_resaving_and_restoring_custom_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, num(42.));
    wb.sheets[0].rows.set(1, 40.);
    wb.sheets[0].cols.set(1, 125.);
    wb.sheets[0].hidden_rows_user.insert(1);
    wb.sheets[0].hidden_rows_user.insert(65_535);
    wb.sheets[0].hidden_cols.insert(1);
    wb.sheets[0].hidden_cols.insert(255);
    for (name, vis) in [
        ("Hidden", SheetVisibility::Hidden),
        ("VeryHidden", SheetVisibility::VeryHidden),
    ] {
        let mut s = Sheet::new(name);
        s.set_visibility(vis);
        wb.sheets.push(s);
    }
    let p = dir.path().join("hidden.xls");
    for _ in 0..2 {
        xlsx::save_atomic(&p, &wb).unwrap();
        wb = xlsx::load(&p).unwrap();
        assert_eq!(wb.sheets[1].visibility(), SheetVisibility::Hidden);
        assert_eq!(wb.sheets[2].visibility(), SheetVisibility::VeryHidden);
        let s = &wb.sheets[0];
        assert!(s.hidden_rows_user.contains(&1) && s.hidden_rows_user.contains(&65_535));
        assert!(s.hidden_cols.contains(&1) && s.hidden_cols.contains(&255));
        assert!(!s.hidden_rows_user.contains(&0) && !s.hidden_cols.contains(&0));
        assert!((s.rows.size(1) - 40.).abs() < 0.1);
        assert!((s.cols.size(1) - 125.).abs() < 0.1);
    }
    wb.sheets[0].hidden_rows_user.insert(65_536);
    assert!(xlsx::save_atomic(&p, &wb).is_err());
}

fn ods_with_formula(path: &Path, expression: &str) {
    let mut zip = ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts = SimpleFileOptions::default();
    zip.start_file("mimetype", opts).unwrap();
    zip.write_all(b"application/vnd.oasis.opendocument.spreadsheet")
        .unwrap();
    zip.start_file("META-INF/manifest.xml", opts).unwrap();
    zip.write_all(br#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#).unwrap();
    zip.start_file("content.xml", opts).unwrap();
    write!(zip, r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:of="urn:oasis:names:tc:opendocument:xmlns:of:1.2" office:version="1.2"><office:body><office:spreadsheet><table:table table:name="Sales"><table:table-row><table:table-cell office:value-type="float" office:value="11"/><table:table-cell office:value-type="float" office:value="22" table:formula="{expression}"/></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#).unwrap();
    zip.finish().unwrap();
}

#[test]
fn ods_formulas_recalculate_and_export_without_openformula_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("formula.ods");
    ods_with_formula(&p, "of:=SUM([.A1:.A1];[$Sales.$A$1])");
    let (mut wb, diag) = xlsx::load_diag(&p).unwrap();
    assert!(diag.is_complete(), "{:?}", diag.problems);
    let expected = "=SUM(A1:A1,Sales!$A$1)";
    assert_eq!(
        wb.sheets[0].get(0, 1).unwrap().formula.as_deref(),
        Some(expected)
    );
    wb.sheets[0].set(0, 0, num(12.));
    formula::recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 1), Value::Number(24.));
    let converted = dir.path().join("converted.xlsx");
    xlsx::save_atomic(&converted, &wb).unwrap();
    let back = xlsx::load(&converted).unwrap();
    assert_eq!(
        back.sheets[0].get(0, 1).unwrap().formula.as_deref(),
        Some(expected)
    );
    ods_with_formula(&p, "other:=SOMETHING([.A1])");
    let (unsupported, diag) = xlsx::load_diag(&p).unwrap();
    assert!(diag.has_fatal());
    assert_eq!(unsupported.sheets[0].value(0, 1), Value::Number(22.));
}

#[test]
fn iso_dates_count_as_numbers_in_both_workbook_calendars() {
    let dir = tempfile::tempdir().unwrap();
    for (sys, serial) in [(DateSystem::D1900, 46174.), (DateSystem::D1904, 44712.)] {
        let p = dir.path().join(format!("date-{sys:?}.xlsx"));
        let mut wb = Workbook::new_blank();
        wb.date_system = sys;
        wb.sheets[0].set(0, 0, num(42.));
        xlsx::save_atomic(&p, &wb).unwrap();
        let part = "xl/worksheets/sheet1.xml";
        let xml = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="d"><v>2026-06-01T12:00:00</v></c><c r="B1"><f>COUNT(A1)</f><v>0</v></c><c r="C1" t="d"><v>06:30:00</v></c><c r="D1" t="d"><v>2026-06-01T18:00:00Z</v></c></row></sheetData></worksheet>"#;
        sheetcore::fixups::write_parts(&p, &[(part.into(), xml.as_bytes().to_vec())]).unwrap();
        let (mut wb, diag) = xlsx::load_diag(&p).unwrap();
        assert!(!diag.has_fatal());
        formula::recalc_all_force(&mut wb);
        assert_eq!(wb.sheets[0].value(0, 0), Value::Number(serial + 0.5));
        assert_eq!(wb.sheets[0].value(0, 1), Value::Number(1.));
        assert_eq!(wb.sheets[0].value(0, 2), Value::Number(6.5 / 24.));
        assert_eq!(wb.sheets[0].value(0, 3), Value::Number(serial + 0.75));
        wb.sheets[0].set(1, 0, num(1.));
        xlsx::save_atomic(&p, &wb).unwrap();
        assert_eq!(
            xlsx::load(&p).unwrap().sheets[0].value(0, 0),
            Value::Number(serial + 0.5)
        );
        let mut zip = ZipArchive::new(std::fs::File::open(&p).unwrap()).unwrap();
        let mut saved = String::new();
        zip.by_name(part)
            .unwrap()
            .read_to_string(&mut saved)
            .unwrap();
        assert!(!saved.contains("inlineStr"));
    }
}

#[test]
fn iso_end_of_day_requires_exact_midnight() {
    let dir = tempfile::tempdir().unwrap();
    for (sys, tomorrow) in [(DateSystem::D1900, 46175.), (DateSystem::D1904, 44713.)] {
        let p = dir.path().join(format!("midnight-{sys:?}.xlsx"));
        let mut wb = Workbook::new_blank();
        wb.date_system = sys;
        xlsx::save_atomic(&p, &wb).unwrap();
        let cases = [
            ("2026-06-01T24:00:00", Value::Number(tomorrow)),
            ("24:00:00", Value::Number(1.)),
            ("24:00", Value::Number(1.)),
            ("2026-06-01T24:00:00.000Z", Value::Number(tomorrow)),
            ("24:00:00.000000", Value::Number(1.)),
            (
                "2026-06-01T24:01:00",
                Value::Text("2026-06-01T24:01:00".into()),
            ),
            ("24:00:01", Value::Text("24:00:01".into())),
            ("24:00:00.001", Value::Text("24:00:00.001".into())),
            ("24:00:00+08:00", Value::Text("24:00:00+08:00".into())),
        ];
        let mut sheet = String::from(
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
        );
        for (index, (iso, _)) in cases.iter().enumerate() {
            let row = index + 1;
            sheet.push_str(&format!(
                r#"<row r="{row}"><c r="A{row}" t="d"><v>{iso}</v></c></row>"#
            ));
        }
        sheet.push_str("</sheetData></worksheet>");
        sheetcore::fixups::write_parts(
            &p,
            &[("xl/worksheets/sheet1.xml".into(), sheet.into_bytes())],
        )
        .unwrap();
        let (wb, diag) = xlsx::load_diag(&p).unwrap();
        assert!(diag.is_complete(), "{:?}", diag.problems);
        for (row, (iso, expected)) in cases.into_iter().enumerate() {
            assert_eq!(wb.sheets[0].value(row as u32, 0), expected, "{iso}");
        }
    }
}
