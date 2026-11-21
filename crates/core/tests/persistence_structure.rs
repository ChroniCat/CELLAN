use sheetcore::{model::{Cell, CellRef, Sheet, Value, Workbook}, structural::{apply_structural_wb, Structural}, xlsx};
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("native_persistence_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn file(&self, name: &str) -> PathBuf { self.0.join(name) }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}
fn number(n: f64) -> Cell { Cell { value: Value::Number(n), formula: None } }

#[test]
fn foreign_structural_fast_path_agrees_with_the_reference_rewriter() {
    let original = [
        "=SUM(数据!A2:A5)", "='O''Brien!明细'!$D$3", "=sAlEs!2:4",
        "='Unused'!B1+数据!E2", "=INDIRECT(\"Sales!A1\")", "='[1]Sales'!B3",
        "=SUM(Orders[Sales])+A2", "=SalesTax!A2", "=SUM(数据!A:A)",
    ];
    for name in ["数据", "O'Brien!明细", "Sales", "Not present"] {
        let mut sheet = Sheet::new("Other");
        for (i, src) in original.iter().enumerate() {
            // Both set() and an existing cell mutated via get_mut() can add a reference.
            let r = 300 * i as u32;
            let mut cell = number(1.0);
            if i % 2 == 0 { cell.formula = Some((*src).into()); }
            sheet.set(r, 0, cell);
            if i % 2 == 1 { sheet.get_mut(r, 0).unwrap().formula = Some((*src).into()); }
        }
        let shift = sheetcore::refs::Shift {
            target_sheet: name.into(), own_sheet: sheet.name.clone(), n: 2, ..Default::default()
        };
        sheet.rewrite_references(&shift);
        for (i, src) in original.iter().enumerate() {
            assert_eq!(sheet.get(300 * i as u32, 0).unwrap().formula.as_deref(),
                       Some(sheetcore::refs::rewrite(src, &shift).as_str()));
        }
    }
}
fn names(wb: &Workbook) -> Vec<&str> { wb.sheets.iter().map(|s| s.name.as_str()).collect() }
fn assert_cells(expected: &Sheet, actual: &Sheet) {
    for r in 0..8 {
        for c in 0..8 {
            assert_eq!(actual.value(r, c), expected.value(r, c), "{} ({r},{c})", expected.name);
            assert_eq!(actual.get(r, c).and_then(|cell| cell.formula.as_deref()), expected.get(r, c).and_then(|cell| cell.formula.as_deref()), "formula ({r},{c})");
        }
    }
}
fn seed(path: &Path) {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Original".into();
    for r in 0..4 { for c in 0..4 { wb.sheets[0].set(r, c, number((10 * r + c + 1) as f64)); } }
    wb.sheets.push(Sheet::new("Keep"));
    wb.sheets[1].set(0, 0, number(100.0));
    wb.sheets[1].merge_range(1, 1, 2, 2);
    wb.sheets.push(Sheet::new("Third"));
    wb.sheets[2].set(0, 0, number(300.0));
    xlsx::save_atomic(path, &wb).unwrap();
}

#[test]
fn structural_cells_survive_save_reopen_and_repeated_save() {
    for op in [Structural::DeleteRows { at: 1, n: 1 }, Structural::InsertRows { at: 1, n: 2 }, Structural::DeleteCols { at: 1, n: 1 }, Structural::InsertCols { at: 1, n: 2 }] {
        let fixture = Fixture::new();
        let path = fixture.file("cells.xlsx");
        seed(&path);
        let mut wb = xlsx::load(&path).unwrap();
        wb.sheets[0].set(0, 0, Cell::default()); // earlier deletion must remain a tombstone
        apply_structural_wb(&mut wb, 0, &op);
        for _ in 0..2 {
            xlsx::save_atomic(&path, &wb).unwrap();
            let back = xlsx::load(&path).unwrap();
            assert_cells(&wb.sheets[0], &back.sheets[0]);
            assert_eq!(back.sheets[1].merges, vec![(1, 1, 2, 2)]);
        }
        let mut back = xlsx::load(&path).unwrap();
        apply_structural_wb(&mut back, 0, &Structural::DeleteRows { at: 0, n: 1 });
        xlsx::save_atomic(&path, &back).unwrap();
        assert_cells(&back.sheets[0], &xlsx::load(&path).unwrap().sheets[0]);
    }
}

#[test]
fn rename_delete_reorder_preserve_sheet_identity_and_extensions() {
    let fixture = Fixture::new();
    let path = fixture.file("sheets.xlsx");
    seed(&path);
    let part = sheetcore::patch::sheet_targets(&path).unwrap()["Keep"].clone();
    sheetcore::patch::rewrite_zip(&path, |name, bytes| {
        if name == "xl/workbook.xml" {
            return String::from_utf8(bytes).unwrap().replace("</workbook>", "<extLst><ext uri=\"native-workbook-marker\"><marker xmlns=\"urn:sheetflow:test\"/></ext></extLst></workbook>").into_bytes();
        }
        if name != part { return bytes; }
        String::from_utf8(bytes).unwrap().replace("</worksheet>", "<extLst><ext uri=\"native-preserve-marker\"><marker xmlns=\"urn:sheetflow:test\"/></ext></extLst></worksheet>").into_bytes()
    }).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    wb.sheets[1].name = "重命名 & 保留".into();
    wb.sheets.remove(0);
    wb.sheets.swap(0, 1);
    for _ in 0..2 {
        xlsx::save_atomic(&path, &wb).unwrap();
        let back = xlsx::load(&path).unwrap();
        assert_eq!(names(&back), vec!["Third", "重命名 & 保留"]);
        assert_eq!(back.sheets[0].value(0, 0), Value::Number(300.0));
        assert_eq!(back.sheets[1].value(0, 0), Value::Number(100.0));
        assert_eq!(back.sheets[1].merges, vec![(1, 1, 2, 2)]);
        let new_part = sheetcore::patch::sheet_targets(&path).unwrap()["重命名 & 保留"].clone();
        assert_eq!(new_part, part);
        let xml = String::from_utf8(sheetcore::fixups::read_part_file(&path, &part).unwrap()).unwrap();
        assert!(xml.contains("native-preserve-marker"));
        let workbook_xml = String::from_utf8(sheetcore::fixups::read_part_file(&path, "xl/workbook.xml").unwrap()).unwrap();
        assert!(workbook_xml.contains("native-workbook-marker"));
    }
    let mut back = xlsx::load(&path).unwrap();
    back.sheets[1].name = "再改名".into();
    xlsx::save_atomic(&path, &back).unwrap();
    assert_eq!(names(&xlsx::load(&path).unwrap()), vec!["Third", "再改名"]);
}

#[test]
fn swapped_names_and_new_sheet_do_not_reuse_another_sheets_part() {
    let fixture = Fixture::new();
    let path = fixture.file("swapped.xlsx");
    seed(&path);
    let mut wb = xlsx::load(&path).unwrap();
    wb.sheets[0].name = "Keep".into();
    wb.sheets[1].name = "Original".into();
    wb.sheets.remove(2);
    xlsx::save_atomic(&path, &wb).unwrap();
    let back = xlsx::load(&path).unwrap();
    assert_eq!(names(&back), vec!["Keep", "Original"]);
    assert_eq!(back.sheets[0].value(0, 0), Value::Number(1.0));
    assert_eq!(back.sheets[1].value(0, 0), Value::Number(100.0));

    let mut wb = back;
    wb.sheets.remove(0); // retained source sheet2 now serializes as sheet1
    let mut new = Sheet::new("Added");
    new.set(0, 0, number(900.0));
    wb.sheets.push(new); // serializes as sheet2: must not collide on rehome
    for _ in 0..2 {
        xlsx::save_atomic(&path, &wb).unwrap();
        let back = xlsx::load(&path).unwrap();
        assert_eq!(names(&back), vec!["Original", "Added"]);
        assert_eq!(back.sheets[0].value(0, 0), Value::Number(100.0));
        assert_eq!(back.sheets[1].value(0, 0), Value::Number(900.0));
    }
}

#[test]
fn replace_all_original_sheets_and_insert_new_sheet_in_middle() {
    let fixture = Fixture::new();
    let path = fixture.file("replacement.xlsx");
    seed(&path);
    let mut wb = xlsx::load(&path).unwrap();
    wb.sheets = vec![Sheet::new("Fresh")];
    wb.sheets[0].set(0, 0, number(77.0));
    xlsx::save_atomic(&path, &wb).unwrap();
    assert_eq!(names(&xlsx::load(&path).unwrap()), vec!["Fresh"]);
    seed(&path);
    let mut wb = xlsx::load(&path).unwrap();
    wb.sheets.insert(1, Sheet::new("Middle"));
    xlsx::save_atomic(&path, &wb).unwrap();
    assert_eq!(names(&xlsx::load(&path).unwrap()), vec!["Original", "Middle", "Keep", "Third"]);
}

#[test]
fn shifted_styles_and_notes_use_new_coordinates_after_reopen() {
    let fixture = Fixture::new();
    let path = fixture.file("metadata.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(2, 1, number(42.0));
    let style = wb.styles.intern(sheetcore::style::CellStyle { bold: true, ..Default::default() });
    wb.sheets[0].set_style(2, 1, style);
    wb.sheets[0].notes.insert(CellRef::new(2, 1), sheetcore::notes::Note { r: 2, c: 1, author: "Test".into(), text: "retain note".into() });
    wb.sheets[0].notes_dirty = true;
    xlsx::save_atomic(&path, &wb).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    apply_structural_wb(&mut wb, 0, &Structural::DeleteRows { at: 0, n: 1 });
    xlsx::save_atomic(&path, &wb).unwrap();
    let back = xlsx::load(&path).unwrap();
    assert!(back.styles.get(back.sheets[0].style_of(1, 1)).bold);
    assert!(!back.styles.get(back.sheets[0].style_of(2, 1)).bold);
    assert_eq!(back.sheets[0].notes[&CellRef::new(1, 1)].text, "retain note");
    assert!(!back.sheets[0].notes.contains_key(&CellRef::new(2, 1)));
}

#[test]
fn saving_never_creates_history_or_sidecar_files_next_to_workbook() {
    let fixture = Fixture::new();
    let path = fixture.file("clean_dir.xlsx");
    seed(&path);
    let mut wb = xlsx::load(&path).unwrap();
    wb.sheets[0].set(0, 0, number(999.0));
    xlsx::save_atomic(&path, &wb).unwrap();
    let entries: Vec<String> = std::fs::read_dir(&fixture.0)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["clean_dir.xlsx"], "原文件目录不得残留 .~sheetflow-history 或 .saving-* 附加文件");
}

#[test]
fn new_sheet_reusing_a_removed_or_renamed_name_has_no_old_content() {
    for rename in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.file("reused-name.xlsx");
        seed(&path);
        let mut wb = xlsx::load(&path).unwrap();
        if rename { wb.sheets[0].name = "Renamed".into(); } else { wb.sheets.remove(0); }
        wb.sheets.push(Sheet::new("Original"));
        for _ in 0..2 {
            xlsx::save_atomic(&path, &wb).unwrap();
            let back = xlsx::load(&path).unwrap();
            let new_sheet = back.sheets.iter().find(|s| s.name == "Original").unwrap();
            assert!(new_sheet.is_empty(), "new sheet adopted deleted sheet's contents");
            if rename { assert_eq!(back.sheets[0].value(0, 0), Value::Number(1.0)); }
        }
    }
}

#[test]
fn rewritten_cross_sheet_reference_is_not_restored_from_old_package() {
    let fixture = Fixture::new();
    let path = fixture.file("references.xlsx");
    seed(&path);
    let mut wb = xlsx::load(&path).unwrap();
    wb.sheets[1].set(0, 0, Cell { value: Value::Number(21.0), formula: Some("=Original!A3".into()) });
    xlsx::save_atomic(&path, &wb).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    apply_structural_wb(&mut wb, 0, &Structural::DeleteRows { at: 0, n: 1 });
    xlsx::save_atomic(&path, &wb).unwrap();
    let back = xlsx::load(&path).unwrap();
    assert_eq!(back.sheets[1].get(0, 0).unwrap().formula.as_deref(), Some("=Original!A2"));
}

#[test]
fn reorder_retains_each_sheets_comment_relationships() {
    let fixture = Fixture::new();
    let path = fixture.file("relationships.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Second"));
    for (i, sheet) in wb.sheets.iter_mut().enumerate() {
        sheet.set(0, 0, number(i as f64 + 1.0));
        sheet.notes.insert(CellRef::new(0, 0), sheetcore::notes::Note {
            r: 0, c: 0, author: "Test".into(), text: format!("note-{i}"),
        });
        sheet.notes_dirty = true;
    }
    xlsx::save_atomic(&path, &wb).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    wb.sheets.swap(0, 1);
    wb.sheets[0].name = "Renamed".into();
    for _ in 0..2 {
        xlsx::save_atomic(&path, &wb).unwrap();
        let back = xlsx::load(&path).unwrap();
        assert_eq!(names(&back), vec!["Renamed", "Sheet1"]);
        assert_eq!(back.sheets[0].notes[&CellRef::new(0, 0)].text, "note-1");
        assert_eq!(back.sheets[1].notes[&CellRef::new(0, 0)].text, "note-0");
    }
}

#[test]
fn changed_named_ranges_preserve_unmodeled_workbook_metadata() {
    let fixture = Fixture::new();
    let path = fixture.file("names-and-metadata.xlsx");
    seed(&path);
    sheetcore::patch::rewrite_zip(&path, |name, bytes| {
        if name != "xl/workbook.xml" { return bytes; }
        String::from_utf8(bytes).unwrap().replace("</workbook>", "<extLst><ext uri=\"keep-workbook-extension\"><marker xmlns=\"urn:sheetflow:test\"/></ext></extLst></workbook>").into_bytes()
    }).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    wb.sheets[0].name = "Renamed".into();
    wb.names = vec![("LocalName".into(), Some(0), "Renamed!$A$1".into())];
    wb.names_dirty = true;
    xlsx::save_atomic(&path, &wb).unwrap();
    let back = xlsx::load(&path).unwrap();
    assert_eq!(back.names, wb.names);
    let xml = String::from_utf8(sheetcore::fixups::read_part_file(&path, "xl/workbook.xml").unwrap()).unwrap();
    assert!(xml.contains("keep-workbook-extension"));
}

#[test]
fn removing_all_user_names_keeps_builtin_print_names() {
    let fixture = Fixture::new();
    let path = fixture.file("remove-names.xlsx");
    seed(&path);
    let mut wb = xlsx::load(&path).unwrap();
    wb.names = vec![
        ("First".into(), None, "'Original'!$A$1&\"<&\"".into()),
        ("Second".into(), Some(1), "$A$1".into()),
    ];
    wb.names_dirty = true;
    wb.sheets[0].page.print_area = Some((0, 0, 2, 2));
    wb.sheets[0].page_dirty = true;
    xlsx::save_atomic(&path, &wb).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    assert_eq!(wb.names[0].2, "'Original'!$A$1&\"<&\"");
    wb.names.remove(0);
    wb.names_dirty = true;
    xlsx::save_atomic(&path, &wb).unwrap();
    assert_eq!(xlsx::load(&path).unwrap().names, wb.names);
    wb.names.clear();
    xlsx::save_atomic(&path, &wb).unwrap();
    let back = xlsx::load(&path).unwrap();
    assert!(back.names.is_empty());
    assert_eq!(back.sheets[0].page.print_area, Some((0, 0, 2, 2)));
}

#[test]
fn renamed_print_names_and_existing_pivot_source_follow_sheet_identity() {
    let fixture = Fixture::new();
    let path = fixture.file("rename-print-pivot.xlsx");
    seed(&path);
    let mut wb = xlsx::load(&path).unwrap();
    wb.sheets[0].page.print_area = Some((0, 0, 2, 2));
    wb.sheets[0].page_dirty = true;
    xlsx::save_atomic(&path, &wb).unwrap();
    let spec = sheetcore::pivot::PivotSpec {
        source: (0, 0, 3, 1), row_field: 0, col_field: None, value_field: 1,
        agg: sheetcore::pivot::Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
};
    let result = sheetcore::pivot::compute(&wb.sheets[0], &spec);
    sheetcore::pivot_xml::attach(&path, "Original", &spec, &result, (6, 0, 9, 1), "Sum").unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    assert!(sheetcore::sheet_ops::rename_sheet(&mut wb, 0, "Renamed"));
    for _ in 0..2 {
        xlsx::save_atomic(&path, &wb).unwrap();
        let xml = String::from_utf8(sheetcore::fixups::read_part_file(&path, "xl/workbook.xml").unwrap()).unwrap();
        assert!(sheetcore::model::xml_unescape(&xml).contains("'Renamed'!$A$1:$C$3"), "{xml}");
        let cache = sheetcore::fixups::parts(&path).into_iter().find(|p| p.contains("pivotCacheDefinition") && p.ends_with(".xml")).unwrap();
        let xml = String::from_utf8(sheetcore::fixups::read_part_file(&path, &cache).unwrap()).unwrap();
        assert!(xml.contains("sheet=\"Renamed\""), "{xml}");
    }
}
