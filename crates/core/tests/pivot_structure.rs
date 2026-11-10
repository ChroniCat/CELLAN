use sheetcore::{Cell, Sheet, Value, Workbook};
use sheetcore::pivot::{Agg, PivotSpec};
use sheetcore::structural::{Structural, apply_structural_wb};
use sheetcore::undo::{UndoStack, snapshot_structure};

fn number(n: f64) -> Cell { Cell { value: Value::Number(n), formula: None } }

fn apply(wb: &mut Workbook, undo: &mut UndoStack, op: Structural) {
    let before = snapshot_structure(wb, 0, &op);
    apply_structural_wb(wb, 0, &op);
    let after = before.capture_after(wb);
    undo.push_structure(op.label(), before, after, (0, 0, 0));
}

fn coordinates(wb: &Workbook, idx: usize) -> ((u32, u32, u32, u32), u32, Option<u32>, u32, (u32, u32, u32, u32)) {
    let (_, spec, _, rect, _) = &wb.pivots_pending[idx];
    (spec.source, spec.row_field, spec.col_field, spec.value_field, *rect)
}

fn seed() -> Workbook {
    let mut wb = Workbook::new_blank();
    let spec = PivotSpec {
        source: (0, 1, 4, 4), row_field: 1, col_field: Some(2),
        value_field: 4, agg: Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
};
    wb.pivots_pending.push((0, spec.clone(), Default::default(), (8, 1, 11, 3), "Value".into()));
    wb.sheets.push(Sheet::new("Unchanged"));
    wb.pivots_pending.push((1, spec, Default::default(), (8, 1, 11, 3), "Other".into()));
    wb
}

#[test]
fn inserting_columns_moves_fields_and_output_and_undo_restores_them() {
    let mut wb = seed();
    let original = coordinates(&wb, 0);
    let other = coordinates(&wb, 1);
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertCols { at: 0, n: 2 });
    let moved = ((0, 3, 4, 6), 3, Some(4), 6, (8, 3, 11, 5));
    assert_eq!(coordinates(&wb, 0), moved);
    assert_eq!(coordinates(&wb, 1), other);
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(coordinates(&wb, 0), original);
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(coordinates(&wb, 0), moved);
}

#[test]
fn deleting_an_unused_source_column_preserves_field_identity_and_output() {
    let mut wb = seed();
    let original = coordinates(&wb, 0);
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::DeleteCols { at: 3, n: 1 });
    assert_eq!(coordinates(&wb, 0), ((0, 1, 4, 3), 1, Some(2), 3, (8, 1, 11, 2)));
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(coordinates(&wb, 0), original);
}

#[test]
fn row_edits_move_output_and_source_independently_with_undo() {
    let mut wb = seed();
    let original = coordinates(&wb, 0);
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 2, n: 2 });
    assert_eq!(coordinates(&wb, 0), ((0, 1, 6, 4), 1, Some(2), 4, (10, 1, 13, 3)));
    apply(&mut wb, &mut undo, Structural::DeleteRows { at: 7, n: 1 });
    assert_eq!(coordinates(&wb, 0), ((0, 1, 6, 4), 1, Some(2), 4, (9, 1, 12, 3)));
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(coordinates(&wb, 0), original);
}

#[test]
fn deleting_an_essential_field_or_complete_region_removes_only_that_attachment_and_undo_restores_it() {
    for op in [
        Structural::DeleteCols { at: 1, n: 1 },
        Structural::DeleteCols { at: 2, n: 1 },
        Structural::DeleteCols { at: 4, n: 1 },
        Structural::DeleteRows { at: 0, n: 5 },
        Structural::DeleteRows { at: 8, n: 4 },
    ] {
        let mut wb = seed();
        let original = coordinates(&wb, 0);
        let other = coordinates(&wb, 1);
        let mut undo = UndoStack::new();
        apply(&mut wb, &mut undo, op);
        assert_eq!(wb.pivots_pending.len(), 1);
        assert_eq!(wb.pivots_pending[0].0, 1);
        assert_eq!(coordinates(&wb, 0), other);
        undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
        assert_eq!(wb.pivots_pending.len(), 2);
        assert_eq!(coordinates(&wb, 0), original);
        assert_eq!(coordinates(&wb, 1), other);
        undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
        assert_eq!(wb.pivots_pending.len(), 1);
        assert_eq!(wb.pivots_pending[0].0, 1);
    }
}

#[test]
fn saved_pending_pivot_moves_source_fields_and_output_then_undo_restores_both() {
    let dir = std::env::temp_dir().join(format!("pivot_structure_save_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pivot.xlsx");
    let mut wb = Workbook::new_blank();
    for row in 0..4 {
        wb.sheets[0].set(row, 0, number(row as f64));
        wb.sheets[0].set(row, 1, number(10.0));
    }
    let spec = PivotSpec {
        source: (0, 0, 3, 1), row_field: 0, col_field: None,
        value_field: 1, agg: Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
};
    let result = sheetcore::pivot::compute(&wb.sheets[0], &spec);
    let rect = sheetcore::pivot::write_to(&mut wb.sheets[0], (6, 3), &result);
    wb.pivots_pending.push((0, spec, result, rect, "Value".into()));
    let save = |wb: &mut Workbook| {
        sheetcore::xlsx::save_atomic_with_receipt(&path, wb).unwrap().apply_to(wb);
        wb.path = Some(path.clone());
    };
    let source_and_location = || {
        let parts = sheetcore::fixups::parts(&path);
        let read = |prefix: &str| {
            let part = parts.iter().find(|p| p.starts_with(prefix) && p.ends_with(".xml")).unwrap();
            String::from_utf8(sheetcore::fixups::read_part_file(&path, part).unwrap()).unwrap()
        };
        (read("xl/pivotCache/pivotCacheDefinition"), read("xl/pivotTables/pivotTable"))
    };
    save(&mut wb);
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::InsertCols { at: 0, n: 2 });
    apply(&mut wb, &mut undo, Structural::InsertRows { at: 0, n: 1 });
    for _ in 0..2 {
        save(&mut wb);
        let (source, table) = source_and_location();
        assert!(source.contains("ref=\"C2:D5\""), "{source}");
        assert!(table.contains("<location ref=\"F8:G12\""), "{table}");
        assert!(table.contains("<field x=\"0\"/>"), "{table}");
        assert!(table.contains("fld=\"1\""), "{table}");
    }
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    for _ in 0..2 {
        save(&mut wb);
        let (source, table) = source_and_location();
        assert!(source.contains("ref=\"A1:B4\""), "{source}");
        assert!(table.contains("<location ref=\"D7:E11\""), "{table}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn deleting_a_value_field_saves_without_attachment_and_undo_restores_it_on_disk() {
    let dir = std::env::temp_dir().join(format!("pivot_structure_delete_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pivot.xlsx");
    let mut wb = Workbook::new_blank();
    for row in 0..4 {
        wb.sheets[0].set(row, 0, number(row as f64));
        wb.sheets[0].set(row, 1, number(10.0));
    }
    let spec = PivotSpec {
        source: (0, 0, 3, 1), row_field: 0, col_field: None,
        value_field: 1, agg: Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
};
    let result = sheetcore::pivot::compute(&wb.sheets[0], &spec);
    let rect = sheetcore::pivot::write_to(&mut wb.sheets[0], (6, 3), &result);
    wb.pivots_pending.push((0, spec, result, rect, "Value".into()));
    let save = |wb: &mut Workbook| {
        sheetcore::xlsx::save_atomic_with_receipt(&path, wb).unwrap().apply_to(wb);
        wb.path = Some(path.clone());
    };
    let tables = || sheetcore::fixups::parts(&path).into_iter()
        .filter(|p| p.starts_with("xl/pivotTables/pivotTable") && p.ends_with(".xml"))
        .count();
    save(&mut wb);
    assert_eq!(tables(), 1);
    let mut undo = UndoStack::new();
    apply(&mut wb, &mut undo, Structural::DeleteCols { at: 1, n: 1 });
    assert!(wb.pivots_pending.is_empty());
    assert_ne!(wb.sheets[0].value(7, 2), Value::Empty, "remaining ordinary summary values stay visible");
    save(&mut wb);
    assert_eq!(tables(), 0);
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.pivots_pending.len(), 1);
    assert_eq!(wb.pivots_pending[0].3, rect);
    for _ in 0..2 { save(&mut wb); assert_eq!(tables(), 1); }
    let parts = sheetcore::fixups::parts(&path);
    let cache = parts.iter().find(|p| p.starts_with("xl/pivotCache/pivotCacheDefinition") && p.ends_with(".xml")).unwrap();
    let xml = String::from_utf8(sheetcore::fixups::read_part_file(&path, cache).unwrap()).unwrap();
    assert!(xml.contains("ref=\"A1:B4\""), "{xml}");
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    save(&mut wb);
    assert_eq!(tables(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}
