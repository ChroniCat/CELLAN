use sheetcore::{Cell, CellRef, Sheet, Value, Workbook, formula, structural::{Structural, apply_structural_wb}, undo::{UndoStack, snapshot_structure}, xlsx};

fn number(n: f64) -> Cell { Cell { value: Value::Number(n), formula: None } }
fn f(text: &str) -> Cell { Cell { value: Value::Empty, formula: Some(text.into()) } }
fn apply(wb: &mut Workbook, undo: &mut UndoStack, op: Structural) {
    let before = snapshot_structure(wb, 0, &op);
    apply_structural_wb(wb, 0, &op);
    let after = before.capture_after(wb);
    undo.push_structure(op.label(), before, after, (0, 0, 0));
    formula::recalc_all(wb);
}
fn seed() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, number(10.0));
    wb.sheets[0].set(1, 0, number(20.0));
    wb.sheets.push(Sheet::new("Other"));
    wb.sheets[1].set(0, 0, f("=Sheet1!A1"));
    wb.sheets[1].set(1, 0, f("=SUM(Sheet1!A1:A2)"));
    wb.names.push(("Total".into(), None, "Sheet1!$A$1:$A$2".into()));
    wb.names_dirty = true;
    formula::recalc_all(&mut wb);
    wb
}

#[test]
fn structure_undo_restores_foreign_formulas_and_names_without_replacing_other_cells() {
    let mut wb = seed();
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::DeleteRows { at: 0, n: 1 });
    assert_eq!(wb.sheets[1].value(0, 0).display(), "#REF!");
    // An unrelated value cell is not part of the structural snapshot.
    wb.sheets[1].set(500, 10, number(99.0));
    assert!(undo.undo_is_structure());
    let applied = undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert!(applied.structural);
    assert!(applied.touched.contains(&(1, 0, 0)));
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(10.0));
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(10.0));
    assert_eq!(wb.sheets[1].value(1, 0), Value::Number(30.0));
    assert_eq!(wb.sheets[1].value(500, 10), Value::Number(99.0));
    assert_eq!(wb.names[0].2, "Sheet1!$A$1:$A$2");
    assert!(wb.pivot_shifts.is_empty());
    assert!(undo.redo_is_structure());
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[1].value(0, 0).display(), "#REF!");
    assert_eq!(wb.sheets[1].value(500, 10), Value::Number(99.0));
    assert_eq!(wb.pivot_shifts.len(), 1);
}

#[test]
fn structure_undo_restores_axes_notes_arrays_blank_cell_styles_and_validation() {
    use sheetcore::{cf::{CfBlock, CfRule, CfKind}, dv::DataValidation};
    let mut wb = seed();
    let sid = wb.styles.intern(sheetcore::style::CellStyle { bold: true, ..Default::default() });
    let sheet = &mut wb.sheets[0];
    sheet.rows.set(2, 41.0);
    sheet.cols.set(2, 177.0);
    sheet.set_style(4, 3, sid); // a styled blank cell must survive undo too
    sheet.merge_range(2, 0, 3, 1);
    sheet.notes.insert(CellRef::new(2, 0), sheetcore::notes::Note { r: 2, c: 0, text: "note".into(), author: "test".into() });
    sheet.arrays.push((CellRef::new(4, 0), (4, 0, 5, 0)));
    sheet.cf.blocks.push(CfBlock { ranges: vec![(2, 0, 3, 0)], rules: vec![CfRule { kind: CfKind::Expr { formula: "A3>0".into(), dxf: 0 }, stop: false, priority: 1 }] });
    sheet.dv.push(DataValidation { ranges: vec![(2, 0, 3, 0)], f1: "A3".into(), ..Default::default() });
    sheet.freeze = Some((2, 2));
    let original = sheet.clone();
    let op = Structural::DeleteRows { at: 1, n: 2 };
    let before = snapshot_structure(&wb, 0, &op);
    apply_structural_wb(&mut wb, 0, &op);
    let after = before.capture_after(&wb);
    let mut undo = UndoStack::new();
    undo.push_structure("delete", before, after, (0, 0, 0));
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    let restored = &wb.sheets[0];
    assert_eq!(restored.rows.custom_pairs(), original.rows.custom_pairs());
    assert_eq!(restored.cols.custom_pairs(), original.cols.custom_pairs());
    assert_eq!(restored.styles, original.styles);
    assert_eq!(restored.style_of(4, 3), sid);
    assert_eq!(restored.merges, original.merges);
    assert_eq!(restored.notes, original.notes);
    assert_eq!(restored.arrays, original.arrays);
    assert_eq!(restored.freeze, original.freeze);
    assert_eq!(format!("{:?}", restored.cf), format!("{:?}", original.cf));
    assert_eq!(format!("{:?}", restored.dv), format!("{:?}", original.dv));
    assert!(restored.edited && restored.merges_dirty && restored.notes_dirty && restored.cf_dirty && restored.dv_dirty);
}

#[test]
fn structure_undo_restores_foreign_reference_metadata_and_pending_pivot_specifications() {
    use sheetcore::{cf::{CfBlock, CfRule, CfKind}, dv::DataValidation};
    let mut wb = seed();
    wb.sheets[1].cf.blocks.push(CfBlock { ranges: vec![(0, 0, 1, 0)], rules: vec![CfRule { kind: CfKind::Expr { formula: "Sheet1!A1>0".into(), dxf: 0 }, stop: false, priority: 1 }] });
    wb.sheets[1].dv.push(DataValidation { f1: "Sheet1!$A$1".into(), ..Default::default() });
    let mut chart = sheetcore::chart_write::chart_from_selection(&wb.sheets[0], "Sheet1", (0, 0, 1, 1), "chart", sheetcore::charts::ChartKind::Column);
    chart.part = "xl/charts/chart1.xml".into();
    wb.sheets[1].charts.push(chart);
    let original_cf = format!("{:?}", wb.sheets[1].cf);
    let original_chart = wb.sheets[1].charts[0].series[0].val_ref.clone();
    let spec = sheetcore::pivot::PivotSpec { source: (0, 0, 1, 1), row_field: 0, col_field: None, value_field: 1, agg: sheetcore::pivot::Agg::Sum, group: sheetcore::pivot::GroupBy::None, exclude: Vec::new() };
    wb.pivots_pending.push((0, spec, Default::default(), (3, 0, 4, 1), "value".into()));
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 0, n: 2 });
    let changed_chart = wb.sheets[1].charts[0].series[0].val_ref.clone();
    assert_ne!(changed_chart, original_chart);
    assert_eq!(wb.pivots_pending[0].1.source, (2, 0, 3, 1));
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(format!("{:?}", wb.sheets[1].cf), original_cf);
    assert_eq!(wb.sheets[1].dv[0].f1, "Sheet1!$A$1");
    assert_eq!(wb.sheets[1].charts[0].series[0].val_ref, original_chart);
    assert!(wb.sheets[1].charts[0].ref_fixes.contains(&(changed_chart, original_chart)));
    assert_eq!(wb.pivots_pending[0].1.source, (0, 0, 1, 1));
    assert!(wb.pivot_shifts.is_empty());
}

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("structure_undo_{}_{seq}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self) -> std::path::PathBuf { self.0.join("book.xlsx") }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

#[test]
fn undo_after_save_keeps_part_identity_and_removes_obsolete_coordinates_on_disk() {
    let fixture = Fixture::new();
    let path = fixture.file();
    let mut wb = seed();
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 0, n: 2 });
    assert!(wb.sheets[0].source_part.is_none());
    let receipt = xlsx::save_atomic_with_receipt(&path, &wb).unwrap();
    receipt.apply_to(&mut wb);
    wb.path = Some(path.clone());
    let part = wb.sheets[0].source_part.clone();
    let identity = wb.sheets[0].identity;
    assert!(part.is_some());
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].source_part, part);
    assert_eq!(wb.sheets[0].identity, identity);
    assert!(wb.sheets[0].cleared.contains(&CellRef::new(3, 0)));
    xlsx::save_atomic(&path, &wb).unwrap();
    let back = xlsx::load(&path).unwrap();
    assert_eq!(back.sheets[0].value(0, 0), Value::Number(10.0));
    assert_eq!(back.sheets[0].value(1, 0), Value::Number(20.0));
    assert_eq!(back.sheets[0].value(2, 0), Value::Empty);
    assert_eq!(back.sheets[0].value(3, 0), Value::Empty);
    assert_eq!(back.sheets[1].get(0, 0).unwrap().formula.as_deref(), Some("=Sheet1!A1"));
    assert_eq!(back.names[0].2, "Sheet1!$A$1:$A$2");
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    formula::recalc_all(&mut wb);
    xlsx::save_atomic(&path, &wb).unwrap();
    let back = xlsx::load(&path).unwrap();
    assert_eq!(back.sheets[0].value(0, 0), Value::Empty);
    assert_eq!(back.sheets[0].value(2, 0), Value::Number(10.0));
    assert_eq!(back.sheets[0].value(3, 0), Value::Number(20.0));
}

#[test]
fn slice_only_undo_does_not_silently_consume_a_workbook_transaction() {
    let mut wb = seed();
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertCols { at: 0, n: 1 });
    assert!(undo.undo(&mut wb.sheets, |_, _, _, _| true).is_none());
    assert_eq!(undo.depth(), (1, 0));
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(undo.depth(), (0, 1));
}

#[test]
fn saved_pivot_delete_undo_and_redo_restore_exact_source_bounds() {
    let fixture = Fixture::new();
    let path = fixture.file();
    let mut wb = Workbook::new_blank();
    for r in 0..4 {
        wb.sheets[0].set(r, 0, number(r as f64));
        wb.sheets[0].set(r, 1, number(10.0));
    }
    xlsx::save_atomic(&path, &wb).unwrap();
    let spec = sheetcore::pivot::PivotSpec {
        source: (0, 0, 3, 1), row_field: 0, col_field: None,
        value_field: 1, agg: sheetcore::pivot::Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
};
    let result = sheetcore::pivot::compute(&wb.sheets[0], &spec);
    sheetcore::pivot_xml::attach(&path, "Sheet1", &spec, &result, (6, 0, 10, 1), "Sum").unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::DeleteRows { at: 0, n: 2 });
    let save_and_source = |wb: &mut Workbook| {
        xlsx::save_atomic_with_receipt(&path, wb).unwrap().apply_to(wb);
        let part = sheetcore::fixups::parts(&path).into_iter()
            .find(|p| p.contains("pivotCacheDefinition") && p.ends_with(".xml")).unwrap();
        let xml = String::from_utf8(sheetcore::fixups::read_part_file(&path, &part).unwrap()).unwrap();
        xml.split("<worksheetSource").nth(1).unwrap().split("/>").next().unwrap().to_string()
    };
    for _ in 0..2 { assert!(save_and_source(&mut wb).contains("ref=\"A1:B2\"")); }
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    for _ in 0..2 { assert!(save_and_source(&mut wb).contains("ref=\"A1:B4\"")); }
    assert_eq!(xlsx::load(&path).unwrap().sheets[0].value(0, 0), Value::Number(0.0));
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    for _ in 0..2 { assert!(save_and_source(&mut wb).contains("ref=\"A1:B2\"")); }
    assert_eq!(xlsx::load(&path).unwrap().sheets[0].value(0, 0), Value::Number(2.0));
}

#[test]
fn saved_chart_references_follow_undo_and_redo_on_another_sheet() {
    let fixture = Fixture::new();
    let path = fixture.file();
    let mut wb = seed();
    for r in 0..3 { wb.sheets[0].set(r, 1, number((r + 1) as f64)); }
    xlsx::save_atomic(&path, &wb).unwrap();
    let chart = sheetcore::chart_write::chart_from_selection(
        &wb.sheets[0], "Sheet1", (0, 0, 2, 1), "Foreign source",
        sheetcore::charts::ChartKind::Column,
    );
    sheetcore::chart_write::insert_chart(&path, "Other", &chart).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    let original = wb.sheets[1].charts[0].series[0].val_ref.clone();
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 0, n: 2 });
    let moved = wb.sheets[1].charts[0].series[0].val_ref.clone();
    assert_ne!(original, moved);
    let save_and_ref = |wb: &mut Workbook| {
        xlsx::save_atomic_with_receipt(&path, wb).unwrap().apply_to(wb);
        sheetcore::charts::load_charts(&path, "Other").unwrap()[0].series[0].val_ref.clone()
    };
    // 工作表名是否带引号不影响语义；第二次保存由 umya 重写图表部件时会去掉引号。
    let norm = |s: &str| s.replace('\'', "");
    assert_eq!(norm(&save_and_ref(&mut wb)), norm(&moved));
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    for _ in 0..2 { assert_eq!(norm(&save_and_ref(&mut wb)), norm(&original)); }
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    for _ in 0..2 { assert_eq!(norm(&save_and_ref(&mut wb)), norm(&moved)); }
}
