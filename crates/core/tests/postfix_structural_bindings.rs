use sheetcore::{
    Cell, Sheet, Value, Workbook,
    cf::{CfBlock, CfKind, CfRule},
    dv::DataValidation,
    external::{self, Kind, Source},
    formula::recalc_all_force,
    refs::Axis,
    structural::{Structural, apply_structural_wb, move_cols_wb, move_rows_wb},
    undo::{UndoStack, snapshot_axis_move, snapshot_structure},
    xlsx,
};

fn number(value: f64) -> Cell {
    Cell {
        value: Value::Number(value),
        formula: None,
    }
}

fn structure(wb: &mut Workbook, undo: &mut UndoStack, op: Structural) {
    let before = snapshot_structure(wb, 0, &op);
    apply_structural_wb(wb, 0, &op);
    let after = before.capture_after(wb);
    undo.push_structure(op.label(), before, after, (0, 0, 0));
}

#[test]
fn axis_move_undo_redo_tracks_displaced_foreign_cells_names_cf_and_validation() {
    for (axis, from, drop, old, new, pos) in [
        (Axis::Row, 2, 0, "A1", "A2", (0, 0)),
        (Axis::Row, 0, 3, "A3", "A2", (2, 0)),
        (Axis::Col, 2, 0, "A1", "B1", (0, 0)),
        (Axis::Col, 0, 3, "C1", "B1", (0, 2)),
    ] {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "Data".into();
        wb.sheets[0].set(pos.0, pos.1, number(17.0));
        let mut other = Sheet::new("Summary");
        let old = format!("=Data!{old}");
        let new = format!("=Data!{new}");
        other.set(
            0,
            0,
            Cell {
                value: Value::Empty,
                formula: Some(old.as_str().into()),
            },
        );
        other.dv.push(DataValidation {
            f1: old.clone(),
            ..Default::default()
        });
        other.cf.blocks.push(CfBlock {
            ranges: vec![(0, 0, 0, 0)],
            rules: vec![CfRule {
                kind: CfKind::Expr {
                    formula: old.clone(),
                    dxf: 0,
                },
                stop: false,
                priority: 1,
            }],
        });
        wb.sheets.push(other);
        wb.names.push(("Tracked".into(), None, old.clone()));
        let mut undo = UndoStack::new();
        let before = snapshot_axis_move(&wb, 0, axis, from, 1, drop).unwrap();
        match axis {
            Axis::Row => move_rows_wb(&mut wb, 0, from, 1, drop),
            Axis::Col => move_cols_wb(&mut wb, 0, from, 1, drop),
        }
        let after = before.capture_after(&wb);
        undo.push_structure("move", before, after, (0, 0, 0));
        for (step, expected) in [&new, &old, &new].into_iter().enumerate() {
            if step == 1 {
                undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
            }
            if step == 2 {
                undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
            }
            recalc_all_force(&mut wb);
            let sheet = wb.sheet(1);
            assert_eq!(sheet.formula_of(0, 0), Some(expected.as_str()));
            assert_eq!(sheet.value(0, 0), Value::Number(17.0));
            assert_eq!(&sheet.dv[0].f1, expected);
            assert!(
                matches!(&sheet.cf.blocks[0].rules[0].kind, CfKind::Expr { formula, .. } if formula == expected)
            );
            assert_eq!(&wb.names[0].2, expected);
        }
    }
}

fn csv(wb: &Workbook, path: &std::path::Path) -> Source {
    Source {
        name: "_sc_src_1".into(),
        kind: Kind::Csv {
            path: path.display().to_string(),
            delim: ',',
            header: false,
        },
        at: (4, 2),
        target_sheet: Some(wb.sheet(0).name.clone()),
        rows: 2,
        cols: 1,
        csv_opts: None,
    }
}

#[test]
fn band_insert_replays_only_foreign_conditional_formatting_and_validation() {
    use sheetcore::{cells::Shift, structural::CellShift, undo::snapshot_cell_shift_if_metadata};
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let mut rules = Sheet::new("Rules");
    let cf_range = (5, 3, 8, 4);
    let dv_range = (1, 1, 6, 2);
    rules.cf.blocks.push(CfBlock {
        ranges: vec![cf_range],
        rules: vec![CfRule {
            kind: CfKind::Expr {
                formula: "Data!A3>0".into(),
                dxf: 0,
            },
            stop: false,
            priority: 1,
        }],
    });
    rules.dv.push(DataValidation {
        ranges: vec![dv_range],
        f1: "Data!A3".into(),
        f2: "Data!B7".into(),
        ..Default::default()
    });
    wb.sheets.push(rules);
    let check = |wb: &Workbook, reference: &str| {
        assert!(wb.sources.is_empty() && wb.sheets.iter().all(Sheet::is_empty));
        let cf = &wb.sheet(1).cf.blocks[0];
        assert_eq!(cf.ranges, [cf_range]);
        let CfKind::Expr { formula, .. } = &cf.rules[0].kind else {
            panic!("expected expression")
        };
        assert_eq!(formula, &format!("{reference}>0"));
        let dv = &wb.sheet(1).dv[0];
        assert_eq!(dv.ranges, [dv_range]);
        assert_eq!(dv.f1, reference);
        assert_eq!(dv.f2, "Data!B7");
    };
    check(&wb, "Data!A3");
    let op = CellShift::new((1, 0, 2, 0), Shift::Vertical, true);
    let before = snapshot_cell_shift_if_metadata(&wb, 0, &op).unwrap();
    assert!(op.apply(&mut wb, 0));
    check(&wb, "Data!A5");
    assert!(before.cell_shift_changed_metadata(&wb));
    let after = before.capture_after(&wb);
    let mut undo = UndoStack::new();
    undo.push_structure("insert", before, after, (0, 1, 0));
    let undone = undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert!(undone.structural && undone.touched.is_empty());
    check(&wb, "Data!A3");
    let redone = undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert!(redone.structural && redone.touched.is_empty());
    check(&wb, "Data!A5");
}

#[test]
fn csv_binding_follows_row_and_column_edits_and_refresh_after_save() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.csv");
    std::fs::write(&file, "1\n2\n").unwrap();
    let mut wb = Workbook::new_blank();
    let source = csv(&wb, &file);
    external::refresh(&mut wb, 0, &source).unwrap();
    wb.sources.push(source.clone());
    wb.sheets[0].set(3, 1, number(99.0));
    let mut undo = UndoStack::new();
    structure(&mut wb, &mut undo, Structural::InsertRows { at: 1, n: 1 });
    structure(&mut wb, &mut undo, Structural::InsertCols { at: 0, n: 1 });
    assert_eq!(wb.sources[0].at, (5, 3));
    assert!(external::refresh(&mut wb, 0, &source).is_err());
    let mut unrelated = source.clone();
    unrelated.name = "Later".into();
    unrelated.target_sheet = Some("Other".into());
    wb.sheets.push(Sheet::new("Other"));
    wb.sources.insert(0, unrelated.clone());
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sources[1].at, (5, 2));
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sources.remove(0), unrelated);
    let package = dir.path().join("import.xlsx");
    xlsx::save_atomic(&package, &wb).unwrap();
    let mut back = xlsx::load(&package).unwrap();
    let registered = back.sources[0].clone();
    assert_eq!(registered.at, (5, 3));
    std::fs::write(&file, "7\n").unwrap();
    external::refresh(&mut back, 0, &registered).unwrap();
    assert_eq!(back.sheet(0).value(4, 2), Value::Number(99.0));
    assert_eq!(back.sheet(0).value(5, 3), Value::Number(7.0));
    assert_eq!(back.sheet(0).value(6, 3), Value::Empty);
}

#[test]
fn detached_csv_binding_survives_save_and_refuses_refresh_until_undo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.csv");
    std::fs::write(&path, "11\n22\n").unwrap();
    for delete_count in [1, 2] {
        let mut wb = Workbook::new_blank();
        let src = csv(&wb, &path);
        external::refresh(&mut wb, 0, &src).unwrap();
        wb.sources.push(src.clone());
        wb.sheets[0].set(6, 2, number(77.0));
        let mut undo = UndoStack::new();
        structure(
            &mut wb,
            &mut undo,
            Structural::DeleteRows {
                at: 4,
                n: delete_count,
            },
        );
        let detached = wb.sources[0].clone();
        assert!(matches!(detached.kind, Kind::Unbound(_)));
        assert!(external::refresh(&mut wb, 0, &detached).is_err());
        let pkg = dir.path().join(format!("detached-{delete_count}.xlsx"));
        xlsx::save_atomic(&pkg, &wb).unwrap();
        let mut back = xlsx::load(&pkg).unwrap();
        assert_eq!(back.sources, wb.sources);
        assert!(external::refresh(&mut back, 0, &detached).is_err());
        assert_eq!(
            back.sheet(0).value(6 - delete_count, 2),
            Value::Number(77.0)
        );
        undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
        assert_eq!(wb.sources, [src]);
        undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
        assert_eq!(wb.sources, [detached]);
    }
}

#[test]
fn source_binding_moves_only_contiguous_sets_and_keeps_legacy_targets_unambiguous() {
    let mut wb = Workbook::new_blank();
    let source = csv(&wb, std::path::Path::new("data.csv"));
    wb.sources.push(source.clone());
    move_rows_wb(&mut wb, 0, 4, 2, 1);
    assert_eq!(wb.sources[0].at, (1, 2));
    move_rows_wb(&mut wb, 0, 1, 1, 5);
    assert!(matches!(wb.sources[0].kind, Kind::Unbound(_)));
    let mut source = source;
    source.target_sheet = None;
    wb.sources = vec![source.clone()];
    apply_structural_wb(&mut wb, 0, &Structural::DeleteRows { at: 1, n: 1 });
    apply_structural_wb(&mut wb, 0, &Structural::DeleteCols { at: 1, n: 1 });
    assert_eq!(wb.sources[0].at, (3, 1));
    assert_eq!(wb.sources[0].target_sheet.as_ref(), Some(&wb.sheet(0).name));
    move_cols_wb(&mut wb, 0, 1, 1, 4);
    assert_eq!(wb.sources[0].at, (3, 3));
    wb.sources = vec![source.clone()];
    wb.sheets.push(Sheet::new("Other"));
    apply_structural_wb(&mut wb, 1, &Structural::InsertRows { at: 0, n: 1 });
    assert!(matches!(wb.sources[0].kind, Kind::Unbound(_)));
    assert_eq!(wb.sources[0].target_sheet, None);
    source.target_sheet = Some("Other".into());
    source.kind = Kind::Internal {
        sheet: wb.sheet(0).name.clone(),
        rect: (8, 2, 10, 4),
    };
    wb.sources = vec![source];
    apply_structural_wb(&mut wb, 0, &Structural::DeleteRows { at: 9, n: 1 });
    assert!(matches!(
        wb.sources[0].kind,
        Kind::Internal {
            rect: (8, 2, 9, 4),
            ..
        }
    ));
    assert_eq!(wb.sources[0].at, (4, 2));
}

#[test]
fn structural_undo_refuses_a_later_binding_in_the_affected_rows() {
    let mut wb = Workbook::new_blank();
    let mut undo = UndoStack::new();
    structure(&mut wb, &mut undo, Structural::InsertRows { at: 3, n: 1 });
    let source = csv(&wb, std::path::Path::new("later.csv"));
    wb.sources.push(source.clone());
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(wb.sources, [source]);
    assert_eq!(undo.depth(), (1, 0));
    wb.sources[0].at = (0, 0);
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sources[0].at, (0, 0));
}

#[test]
fn cell_shift_undo_with_only_a_foreign_delta_requires_the_original_source_sheet() {
    use sheetcore::{
        cells::Shift,
        formula::ErrKind,
        structural::CellShift,
        undo::{capture, finish},
    };
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Data"));
    let formula = "=7+Data!A1";
    wb.sheet_mut(0).set(
        0,
        0,
        Cell {
            value: Value::Empty,
            formula: Some(formula.into()),
        },
    );
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheet(0).value(0, 0), Value::Number(7.));
    let foreign = capture(wb.sheet(0), 0, &[(0, 0)]);
    let source = capture(wb.sheet(1), 1, &[(0, 0)]);
    assert!(CellShift::new((0, 0, 0, 0), Shift::Vertical, false).apply(&mut wb, 1));
    assert!(finish(source, wb.sheet(1)).is_empty());
    let deltas = finish(foreign, wb.sheet(0));
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].sheet, 0);
    let deleted = deltas[0].after.formula.clone().unwrap();
    assert!(deleted.contains("#REF!"));
    let mut undo = UndoStack::new();
    undo.push_cell_shift("delete", deltas, (1, 0, 0), &wb.sheets);
    recalc_all_force(&mut wb);
    let original = wb.sheets.pop().unwrap();
    for replacement in [None, Some(Sheet::new("Data"))] {
        if let Some(other) = replacement {
            assert_ne!(other.identity, original.identity);
            wb.sheets.push(other);
        }
        let blocked = undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
        assert_eq!(blocked.skipped, 1);
        assert!(blocked.touched.is_empty());
        assert_eq!(undo.depth(), (1, 0));
        assert_eq!(wb.sheet(0).formula_of(0, 0), Some(deleted.as_ref()));
        assert_eq!(wb.sheet(0).value(0, 0), Value::Error(ErrKind::Ref));
    }
    wb.sheets[1] = original;
    let applied = undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(applied.skipped, 0);
    assert_eq!(applied.touched, [(0, 0, 0)]);
    assert_eq!(undo.depth(), (0, 1));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheet(0).formula_of(0, 0), Some(formula));
    assert_eq!(wb.sheet(0).value(0, 0), Value::Number(7.));
}

#[test]
fn cell_shift_snapshot_restores_split_source_and_leaves_other_sheet_coordinates_alone() {
    use sheetcore::{cells::Shift, structural::CellShift, undo::snapshot_cell_shift};
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Other"));
    let coords = wb.sheet(0).coordinate_revision();
    let other_coords = wb.sheet(1).coordinate_revision();
    let mut source = csv(&wb, std::path::Path::new("file.csv"));
    source.cols = 2;
    wb.sources.push(source.clone());
    wb.sheet_mut(0).set(4, 2, number(42.));
    let op = CellShift::new((0, 2, 1, 2), Shift::Vertical, true);
    let before = snapshot_cell_shift(&wb, 0, &op);
    assert!(op.apply(&mut wb, 0));
    let moved = wb.sheet(0).coordinate_revision();
    assert_ne!(coords, moved);
    assert_eq!(wb.sheet(0).value(6, 2), Value::Number(42.));
    assert_eq!(other_coords, wb.sheet(1).coordinate_revision());
    assert!(before.detached_source(&wb));
    let after = before.capture_after(&wb);
    let mut undo = UndoStack::new();
    undo.push_structure("shift", before, after, (0, 0, 2));
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    let restored = wb.sheet(0).coordinate_revision();
    assert!(![coords, moved].contains(&restored));
    assert_eq!(wb.sources[0], source);
    assert_eq!(wb.sheet(0).value(4, 2), Value::Number(42.));
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert!(matches!(wb.sources[0].kind, Kind::Unbound(_)));
    assert_eq!(wb.sheet(0).value(6, 2), Value::Number(42.));
    assert!(![coords, moved, restored].contains(&wb.sheet(0).coordinate_revision()));
    assert_eq!(wb.sheet(1).coordinate_revision(), other_coords);
}

#[test]
fn band_undo_guards_changed_replies_and_preserves_later_unrelated_threads() {
    use sheetcore::{
        CellRef,
        cells::Shift,
        notes::{Note, ThreadEntry, ThreadedComment},
        structural::CellShift,
        undo::snapshot_cell_shift,
    };
    let entry = |text: &str| ThreadEntry {
        id: sheetcore::notes::new_ooxml_guid(),
        text: text.into(),
        author: "test".into(),
        timestamp: "2026-11-27T00:00:00Z".into(),
    };
    let mut wb = Workbook::new_blank();
    let key = CellRef::new(1, 0);
    let dest = CellRef::new(3, 0);
    let original = ThreadedComment {
        r: key.row,
        c: key.col,
        resolved: false,
        entries: vec![entry("root")],
    };
    wb.sheet_mut(0).threads.insert(key, original.clone());
    let op = CellShift::new((0, 0, 1, 0), Shift::Vertical, true);
    let before = snapshot_cell_shift(&wb, 0, &op);
    assert!(op.apply(&mut wb, 0));
    let after = before.capture_after(&wb);
    let mut undo = UndoStack::new();
    undo.push_structure("shift", before, after, (0, 0, 0));
    wb.sheet_mut(0)
        .threads
        .get_mut(&dest)
        .unwrap()
        .entries
        .push(entry("reply"));
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(undo.depth(), (1, 0));
    assert_eq!(wb.sheet(0).threads[&dest].entries.len(), 2);
    wb.sheet_mut(0)
        .threads
        .get_mut(&dest)
        .unwrap()
        .entries
        .pop();
    let elsewhere = CellRef::new(8, 2);
    let other = ThreadedComment {
        r: elsewhere.row,
        c: elsewhere.col,
        resolved: true,
        entries: vec![entry("later")],
    };
    let note = Note {
        r: elsewhere.row,
        c: elsewhere.col,
        text: "later".into(),
        author: "test".into(),
    };
    wb.sheet_mut(0).threads.insert(elsewhere, other.clone());
    wb.sheet_mut(0).notes.insert(elsewhere, note.clone());
    wb.sheet_mut(0).notes_dirty = true;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("comments.xlsx");
    xlsx::save_atomic_with_receipt(&path, &wb)
        .unwrap()
        .apply_to(&mut wb);
    wb.sheet_mut(0).threads_dirty = false;
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sheet(0).threads[&key], original);
    assert!(wb.sheet(0).threads_dirty);
    assert_eq!(wb.sheet(0).threads[&elsewhere], other);
    assert_eq!(wb.sheet(0).notes[&elsewhere], note);
    xlsx::save_atomic(&path, &wb).unwrap();
    let reopened = xlsx::load(&path).unwrap();
    assert_eq!(reopened.sheet(0).threads[&key].entries, original.entries);
    assert_eq!(reopened.sheet(0).threads[&elsewhere].entries, other.entries);
    assert!(!reopened.sheet(0).threads.contains_key(&dest));
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sheet(0).threads[&dest].entries, original.entries);
    assert_eq!(wb.sheet(0).threads[&elsewhere], other);
    assert_eq!(wb.sheet(0).notes[&elsewhere], note);
}
