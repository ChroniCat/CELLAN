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
fn bottom_row_insertion_records_only_vacated_cells() {
    let mut wb = Workbook::new_blank();
    for r in 0..1024 {
        wb.sheets[0].set(r, 0, number(r.into()));
    }
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 1022, n: 1 });
    assert_eq!(wb.sheets[0].cleared.len(), 1);
    assert!(wb.sheets[0].cleared.contains(&CellRef::new(1022, 0)));
    let restored = undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(restored.touched, vec![(0, 1022, 0), (0, 1023, 0), (0, 1024, 0)]);
    assert_eq!(wb.sheets[0].value(1023, 0), Value::Number(1023.0));
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
    wb.sheets[1].charts[0].title = "later chart title".into();
    wb.sheets[1].charts[0].series[0].vals = vec![f64::NAN];
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(format!("{:?}", wb.sheets[1].cf), original_cf);
    assert_eq!(wb.sheets[1].dv[0].f1, "Sheet1!$A$1");
    assert_eq!(wb.sheets[1].charts[0].series[0].val_ref, original_chart);
    assert_eq!(wb.sheets[1].charts[0].title, "later chart title");
    assert!(wb.sheets[1].charts[0].series[0].vals[0].is_nan());
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
fn structural_history_refuses_untracked_target_writes_in_either_direction() {
    let mut wb = seed();
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 0, n: 1 });
    // A received edit or a solver writes directly to the model without a new undo entry.
    wb.sheets[0].set(9, 7, number(88.0));
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(undo.depth(), (1, 0));
    assert_eq!(wb.sheets[0].value(9, 7), Value::Number(88.0));
    assert_eq!(wb.sheets[1].get(0, 0).unwrap().formula.as_deref(), Some("=Sheet1!A2"));
    wb.sheets[0].remove(9, 7);
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    wb.sheets[0].set(0, 0, number(92.0));
    assert!(undo.redo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(undo.depth(), (0, 1));
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(92.0));
}

#[test]
fn metadata_and_foreign_reference_conflicts_leave_all_sheets_and_history_intact() {
    let mut wb = seed();
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 0, n: 1 });
    let sheet_id = wb.sheets[1].identity;
    wb.sheets[1].identity += 9000;
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    wb.sheets[1].identity = sheet_id;
    wb.sheets[0].rows.set(22, 44.0);
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(wb.sheets[0].rows.size(22), 44.0);
    let default_height = wb.sheets[0].rows.default_size();
    wb.sheets[0].rows.set(22, default_height);
    let structural_name = std::mem::replace(&mut wb.names[0].2, "123".into());
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(wb.names[0].2, "123");
    wb.names[0].2 = structural_name;
    wb.names.insert(0, ("Unrelated".into(), None, "123".into()));
    wb.sheets[1].set(0, 0, f("=42"));
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(wb.sheets[1].get(0, 0).unwrap().formula.as_deref(), Some("=42"));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(10.0));
    assert_eq!(undo.depth(), (1, 0));
    wb.sheets[1].set(0, 0, f("=Sheet1!A2"));
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.names[0].2, "123");
    assert_eq!(wb.names[1].2, "Sheet1!$A$1:$A$2");
}

#[test]
fn sequential_undo_redo_survives_save_receipts_recalculation_and_restored_inputs() {
    let fixture = Fixture::new();
    let mut wb = seed();
    wb.sheets[0].set(3, 1, f("=RAND()"));
    wb.sheets[0].rows.set(3, 48.0);
    wb.sheets[0].layout_dirty = true;
    let path = fixture.file();
    xlsx::save_atomic(&path, &wb).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 1, n: 1 });
    apply(&mut wb, &mut undo, Structural::InsertCols { at: 1, n: 1 });
    wb.sheets[0].set(2, 0, number(200.0));
    wb.sheets[0].set(2, 0, number(20.0));
    for redo in [false, false, true, true] {
        formula::recalc_all(&mut wb);
        wb.dirty = false;
        xlsx::save_atomic_with_receipt(&path, &wb).unwrap().adopt_document(&mut wb, &path, true);
        assert_eq!(xlsx::load(&path).unwrap().sheets[0].rows.custom_pairs(), wb.sheets[0].rows.custom_pairs());
        let step = if redo { undo.redo_workbook(&mut wb, |_, _, _, _| true) }
            else { undo.undo_workbook(&mut wb, |_, _, _, _| true) };
        assert!(step.is_some());
    }
    assert_eq!(undo.depth(), (2, 0));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(20.0));
    assert_eq!(wb.sheets[0].get(4, 2).unwrap().formula.as_deref(), Some("=RAND()"));
}

#[test]
fn a_literal_override_of_a_dynamic_spill_tail_is_not_treated_as_a_cache() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(2)"));
    formula::recalc_all(&mut wb);
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertCols { at: 2, n: 1 });
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(2.0));
    wb.sheets[0].set(1, 0, number(2.0));
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert!(!wb.sheets[0].spilled.contains(&CellRef::new(1, 0)));
    wb.sheets[0].remove(1, 0);
    formula::recalc_all(&mut wb);
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
}

#[test]
fn cse_cache_loaded_from_disk_can_recalculate_between_undo_and_redo() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(2)"));
    wb.sheets[0].set(1, 0, number(2.0));
    wb.sheets[0].arrays.push((CellRef::new(0, 0), (0, 0, 1, 0)));
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertCols { at: 1, n: 1 });
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    formula::recalc_all(&mut wb);
    assert!(wb.sheets[0].spilled.contains(&CellRef::new(1, 0)));
    assert!(undo.redo_workbook(&mut wb, |_, _, _, _| true).is_some());
}

#[test]
fn changed_foreign_validation_and_pivot_payloads_are_protected() {
    let mut wb = seed();
    wb.sheets[1].dv.push(sheetcore::dv::DataValidation {
        f1: "Sheet1!$A$1".into(), ..Default::default()
    });
    let spec = sheetcore::pivot::PivotSpec {
        source: (0, 0, 2, 1), row_field: 0, col_field: None, value_field: 1,
        agg: sheetcore::pivot::Agg::Sum, group: sheetcore::pivot::GroupBy::None, exclude: vec![]
    };
    wb.pivots_pending.push((0, spec, Default::default(), (4, 0, 5, 1), "old".into()));
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 0, n: 1 });
    wb.pivots_pending[0].4 = "new".into();
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(wb.pivots_pending[0].4, "new");
    wb.pivots_pending[0].4 = "old".into();
    wb.sheets[1].dv[0].error_title = "later edit".into();
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(wb.sheets[1].dv[0].error_title, "later edit");
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(10.0));
    assert_eq!(undo.depth(), (1, 0));
}

#[test]
fn new_and_rewritten_references_cannot_silently_retarget_across_undo_or_redo() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(1, 0, number(99.0));
    wb.sheets.push(Sheet::new("Report"));
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 0, n: 1 });
    wb.sheets[1].set(0, 3, f("=Data!A3"));
    formula::recalc_all(&mut wb);
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(wb.sheets[1].value(0, 3), Value::Number(99.0));
    wb.sheets[1].set(0, 3, f("=Report!A3"));
    wb.sheets[1].set(4, 3, number(123.0));
    wb.names.push(("New".into(), None, "Data!A3".into()));
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    wb.names[0].2 = "42".into();
    wb.sheets[1].dv.push(sheetcore::dv::DataValidation {
        f1: "Data!A3".into(), ..Default::default()
    });
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    wb.sheets[1].dv[0].f1 = "42".into();
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.names[0].2, "42");
    assert_eq!(wb.sheets[1].value(4, 3), Value::Number(123.0));
    wb.sheets[1].set(0, 3, f("='Data'!A2"));
    assert!(undo.redo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(undo.depth(), (0, 1));
}

#[test]
fn new_reference_metadata_uses_the_actual_affected_coordinate_range() {
    let mut wb = seed();
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 4, n: 1 });
    let new_link = sheetcore::hyperlinks::Hyperlink { location: "Sheet1!A6".into(), ..Default::default() };
    wb.sheets[1].hyperlinks.insert((3, 2), new_link);
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    wb.sheets[1].hyperlinks.get_mut(&(3, 2)).unwrap().location = "Sheet1!A1".into();
    let chart = sheetcore::chart_write::chart_from_selection(&wb.sheets[0], "Sheet1", (6, 0, 8, 1), "new", sheetcore::charts::ChartKind::Column);
    wb.sheets[1].charts.push(chart);
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    wb.sheets[1].charts.clear();
    wb.sheets[1].sparklines.push(sheetcore::sparkline::Sparkline {
        source_sheet: Some("Sheet1".into()), rect: (5, 0, 8, 0), ..Default::default()
    });
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    wb.sheets[1].sparklines[0].rect = (0, 0, 1, 0);
    wb.sheets[1].set(3, 0, f("=Sheet1!A1"));
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.sheets[1].sparklines[0].rect, (0, 0, 1, 0));
    assert_eq!(wb.sheets[1].hyperlinks[&(3, 2)].location, "Sheet1!A1");
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
