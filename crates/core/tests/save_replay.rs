use sheetcore::{Cell, Sheet, Value, Workbook, xlsx, structural::{apply_structural_wb, Structural}};
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("native_save_replay_{}_{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn file(&self, name: &str) -> PathBuf { self.0.join(name) }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
fn number(n: f64) -> Cell { Cell { value: Value::Number(n), formula: None } }
fn save(path: &Path, wb: &mut Workbook) {
    xlsx::save_atomic_with_receipt(path, wb).unwrap().apply_to(wb);
    wb.path = Some(path.to_owned());
}
fn xml(path: &Path, part: &str) -> String {
    String::from_utf8(sheetcore::fixups::read_part_file(path, part).unwrap()).unwrap()
}
fn add_picture(path: &Path, sheet: &str) {
    let png = [0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a,0,0,0,0x0d,0x49,0x48,0x44,0x52,0,0,0,1,0,0,0,1,8,4,0,0,0,0xb5,0x1c,0x0c,2,0,0,0,0x0b,0x49,0x44,0x41,0x54,0x78,0xda,0x63,0xfc,0xff,0x1f,0,3,3,2,0,0xef,0xb2,0x47,0x9b,0,0,0,0,0x49,0x45,0x4e,0x44,0xae,0x42,0x60,0x82];
    sheetcore::pic_write::insert_picture(path, sheet, &png, "png", 0, 1).unwrap();
}

#[test]
fn first_save_receipt_preserves_a_picture_inserted_before_later_saves() {
    let f = Fixture::new();
    let path = f.file("pictures.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, number(1.));
    save(&path, &mut wb);
    assert!(wb.sheets[0].source_part.is_some());
    add_picture(&path, "Sheet1");
    // Match the native app: inserting a picture refreshes only its metadata.
    wb.sheets[0].pictures = sheetcore::pic::load(&path).remove("Sheet1").unwrap();
    for _ in 0..3 {
        save(&path, &mut wb);
        assert_eq!(xlsx::load(&path).unwrap().sheets[0].pictures.len(), 1);
    }
    let copy = f.file("save-as.xlsx");
    save(&copy, &mut wb);
    save(&copy, &mut wb);
    assert_eq!(xlsx::load(&copy).unwrap().sheets[0].pictures.len(), 1);
}

#[test]
fn receipt_matches_identity_after_rename_reorder_delete_and_edits() {
    let f = Fixture::new();
    let path = f.file("in-flight.xlsx");
    let mut live = Workbook::new_blank();
    live.sheets[0].set(0, 0, number(1.));
    live.sheets[0].set(1, 0, number(2.));
    live.sheets.push(Sheet::new("Removed"));
    let snapshot = live.clone();
    assert!(sheetcore::sheet_ops::rename_sheet(&mut live, 0, "Renamed"));
    assert!(sheetcore::sheet_ops::delete_sheet(&mut live, 1));
    assert!(sheetcore::sheet_ops::insert_sheet(&mut live, 0, Sheet::new("Sheet1")));
    live.sheets[1].set(0, 0, Cell::default());
    apply_structural_wb(&mut live, 1, &Structural::InsertRows { at: 1, n: 1 });
    let receipt = xlsx::save_atomic_with_receipt(&path, &snapshot).unwrap();
    receipt.apply_to(&mut live);
    live.path = Some(path.clone());
    assert!(live.sheets[0].source_part.is_none(), "new name reuse must not inherit a saved part");
    assert_eq!(live.sheets[1].source_part, sheetcore::patch::sheet_targets(&path).unwrap().get("Sheet1").cloned());
    assert!(live.dirty);
    assert!(live.sheets[1].cleared.contains(&sheetcore::model::CellRef::new(0, 0)));
    for _ in 0..2 {
        save(&path, &mut live);
        let back = xlsx::load(&path).unwrap();
        assert_eq!(back.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Sheet1", "Renamed"]);
        assert_eq!(back.sheets[1].value(0, 0), Value::Empty);
        assert_eq!(back.sheets[1].value(1, 0), Value::Empty);
        assert_eq!(back.sheets[1].value(2, 0), Value::Number(2.));
        assert_eq!(back.sheets[0].value(0, 0), Value::Empty);
    }
}

fn pivot_source(path: &Path) -> String {
    let part = sheetcore::fixups::parts(path).into_iter().find(|p| p.contains("pivotCacheDefinition") && p.ends_with(".xml")).unwrap();
    xml(path, &part).split("<worksheetSource").nth(1).unwrap().split("/>").next().unwrap().to_string()
}
fn pivot_book(path: &Path) -> Workbook {
    let mut wb = Workbook::new_blank();
    for r in 0..4 { wb.sheets[0].set(r, 0, number(r as f64)); wb.sheets[0].set(r, 1, number(10.)); }
    save(path, &mut wb);
    let spec = sheetcore::pivot::PivotSpec { source: (0, 0, 3, 1), row_field: 0, col_field: None, value_field: 1, agg: sheetcore::pivot::Agg::Sum, group: sheetcore::pivot::GroupBy::None, exclude: Vec::new() };
    let result = sheetcore::pivot::compute(&wb.sheets[0], &spec);
    sheetcore::pivot_xml::attach(path, "Sheet1", &spec, &result, (6, 0, 10, 1), "Sum").unwrap();
    xlsx::load(path).unwrap()
}

#[test]
fn pivot_shifts_replay_from_imported_base_across_save_save_as_and_undo() {
    let f = Fixture::new();
    let path = f.file("pivot.xlsx");
    let mut wb = pivot_book(&path);
    let before = wb.clone();
    apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 0, n: 1 });
    for _ in 0..3 {
        save(&path, &mut wb);
        assert!(pivot_source(&path).contains("ref=\"A2:B5\""));
    }
    assert!(sheetcore::sheet_ops::rename_sheet(&mut wb, 0, "Renamed"));
    let copy = f.file("pivot-copy.xlsx");
    save(&copy, &mut wb);
    assert!(pivot_source(&copy).contains("sheet=\"Renamed\""));
    assert!(pivot_source(&copy).contains("ref=\"A2:B5\""));
    // Undo uses the complete logical journal, even after an earlier save.
    wb.sheets[0] = before.sheets[0].clone();
    wb.pivot_shifts = before.pivot_shifts.clone();
    save(&copy, &mut wb);
    assert!(pivot_source(&copy).contains("ref=\"A1:B4\""));
    assert!(pivot_source(&copy).contains("sheet=\"Sheet1\""));
    let mut reopened = xlsx::load(&copy).unwrap();
    apply_structural_wb(&mut reopened, 0, &Structural::DeleteRows { at: 1, n: 1 });
    for _ in 0..2 {
        save(&copy, &mut reopened);
        assert!(pivot_source(&copy).contains("ref=\"A1:B3\""));
    }
}

#[test]
fn newly_created_pivot_sources_do_not_drift_across_repeated_saves() {
    // This checks source-coordinate replay only. Registration of newly attached
    // pivots in an already-saved package has separate, pre-existing limitations.
    for save_empty_first in [false, true] {
        let f = Fixture::new();
        let path = f.file("new-pivot.xlsx");
        let mut wb = Workbook::new_blank();
        for r in 0..4 { wb.sheets[0].set(r, 0, number(r as f64)); wb.sheets[0].set(r, 1, number(10.)); }
        if save_empty_first { save(&path, &mut wb); wb = xlsx::load(&path).unwrap(); }
        let spec = sheetcore::pivot::PivotSpec { source: (0, 0, 3, 1), row_field: 0, col_field: None, value_field: 1, agg: sheetcore::pivot::Agg::Sum, group: sheetcore::pivot::GroupBy::None, exclude: Vec::new() };
        let result = sheetcore::pivot::compute(&wb.sheets[0], &spec);
        wb.pivots_pending.push((0, spec, result, (6, 0, 10, 1), "Sum".into()));
        save(&path, &mut wb);
        assert!(pivot_source(&path).contains("ref=\"A1:B4\""));
        apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 0, n: 1 });
        for _ in 0..3 {
            save(&path, &mut wb);
            assert!(pivot_source(&path).contains("ref=\"A2:B5\""));
        }
    }
}

#[test]
fn sheet_insertions_keep_print_scopes_and_workbook_extensions() {
    for index in 0..=2 {
        let f = Fixture::new();
        let path = f.file("print.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Second"));
        for (i, sheet) in wb.sheets.iter_mut().enumerate() {
            sheet.set(0, 0, number(i as f64));
            sheet.page.print_area = Some((i as u32 * 3, 0, i as u32 * 3 + 2, 2));
            sheet.page.title_rows = Some((0, i as u32));
            sheet.page_dirty = true;
        }
        save(&path, &mut wb);
        sheetcore::patch::rewrite_zip(&path, |name, bytes| {
            if name != "xl/workbook.xml" { return bytes; }
            String::from_utf8(bytes).unwrap().replace("</workbook>", "<extLst><ext uri=\"save-replay-marker\"><marker xmlns=\"urn:test\"/></ext></extLst></workbook>").into_bytes()
        }).unwrap();
        let mut wb = xlsx::load(&path).unwrap();
        assert!(sheetcore::sheet_ops::insert_sheet(&mut wb, index, Sheet::new("Inserted")));
        for _ in 0..2 {
            save(&path, &mut wb);
            let back = xlsx::load(&path).unwrap();
            assert!(xml(&path, "xl/workbook.xml").contains("save-replay-marker"));
            for sheet in &back.sheets {
                let expected = match sheet.name.as_str() { "Sheet1" => Some((0, 0, 2, 2)), "Second" => Some((3, 0, 5, 2)), _ => None };
                assert_eq!(sheet.page.print_area, expected, "scope for {}", sheet.name);
            }
        }
    }
}

#[test]
fn new_default_and_duplicated_sheets_have_distinct_identities() {
    let mut wb = Workbook::new_blank();
    let id = wb.sheets[0].identity;
    assert_eq!(wb.clone().sheets[0].identity, id, "snapshot retains identity");
    assert_ne!(Sheet::default().identity, Sheet::default().identity);
    let mut copy = wb.sheets[0].clone();
    copy.name = "Copy".into();
    copy.source_part = Some("xl/worksheets/sheet1.xml".into());
    assert!(sheetcore::sheet_ops::insert_sheet(&mut wb, 1, copy));
    assert_ne!(wb.sheets[1].identity, id);
    assert!(wb.sheets[1].source_part.is_none());
}
