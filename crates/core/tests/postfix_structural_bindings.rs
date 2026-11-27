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

#[test]
fn foreign_cfvo_scale_bar_iconset_formulas_undo_redo_across_structural_axis_move_and_cell_shift() {
    use sheetcore::{
        cells::Shift,
        cf::{Cfvo, CfvoKind},
        structural::CellShift,
        undo::snapshot_cell_shift_if_metadata,
    };
    let cfvo = |formula: &str| Cfvo {
        kind: CfvoKind::Formula,
        val: 0.0,
        formula: formula.into(),
    };
    let make_wb = || {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "Data".into();
        let mut rules = Sheet::new("Rules");
        rules.cf.blocks.push(CfBlock {
            ranges: vec![(0, 0, 4, 0)],
            rules: vec![
                CfRule {
                    kind: CfKind::Scale {
                        points: vec![
                            (cfvo("=Data!$A$3"), [255, 0, 0]),
                            (cfvo("=Data!$A$5"), [0, 255, 0]),
                        ],
                    },
                    stop: false,
                    priority: 1,
                },
                CfRule {
                    kind: CfKind::bar_default(
                        [0, 120, 215],
                        cfvo("=Data!$A$3"),
                        cfvo("=Data!$A$5"),
                    ),
                    stop: false,
                    priority: 2,
                },
                CfRule {
                    kind: CfKind::IconSet {
                        set: "3Arrows".into(),
                        reverse: false,
                        thresholds: vec![cfvo("=Data!$A$3"), cfvo("=Data!$A$5")],
                        show_value: true,
                    },
                    stop: false,
                    priority: 3,
                },
            ],
        });
        wb.sheets.push(rules);
        wb
    };
    let assert_cfvos = |wb: &Workbook, lo: &str, hi: &str| {
        let rules = &wb.sheet(1).cf.blocks[0].rules;
        let CfKind::Scale { points } = &rules[0].kind else {
            panic!("expected scale");
        };
        assert_eq!(points[0].0.formula, lo);
        assert_eq!(points[1].0.formula, hi);
        let CfKind::Bar { min, max, .. } = &rules[1].kind else {
            panic!("expected bar");
        };
        assert_eq!(min.formula, lo);
        assert_eq!(max.formula, hi);
        let CfKind::IconSet { thresholds, .. } = &rules[2].kind else {
            panic!("expected iconset");
        };
        assert_eq!(thresholds[0].formula, lo);
        assert_eq!(thresholds[1].formula, hi);
    };

    // 1) Structural row insert + Undo/Redo
    let mut wb = make_wb();
    let mut undo = UndoStack::new();
    structure(&mut wb, &mut undo, Structural::InsertRows { at: 1, n: 2 });
    assert_cfvos(&wb, "=Data!$A$5", "=Data!$A$7");
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_cfvos(&wb, "=Data!$A$3", "=Data!$A$5");
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_cfvos(&wb, "=Data!$A$5", "=Data!$A$7");

    // 2) AxisMove (move row 0 after row 3 -> drop = 4; rows 1..=3 shift up 1, row 4 stays)
    let mut wb = make_wb();
    let mut undo = UndoStack::new();
    let before = snapshot_axis_move(&wb, 0, Axis::Row, 0, 1, 4).unwrap();
    move_rows_wb(&mut wb, 0, 0, 1, 4);
    let after = before.capture_after(&wb);
    undo.push_structure("move", before, after, (0, 0, 0));
    assert_cfvos(&wb, "=Data!$A$2", "=Data!$A$5");
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_cfvos(&wb, "=Data!$A$3", "=Data!$A$5");
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_cfvos(&wb, "=Data!$A$2", "=Data!$A$5");

    // 3) CellShift with ONLY foreign CFVO rules (no cells on either sheet) + CAS conflict check
    let mut wb = make_wb();
    let op = CellShift::new((1, 0, 2, 0), Shift::Vertical, true);
    let before = snapshot_cell_shift_if_metadata(&wb, 0, &op).unwrap();
    assert!(op.apply(&mut wb, 0));
    assert!(before.cell_shift_changed_metadata(&wb));
    assert_cfvos(&wb, "=Data!$A$5", "=Data!$A$7");
    let after = before.capture_after(&wb);
    let mut undo = UndoStack::new();
    undo.push_structure("shift", before, after, (0, 1, 0));
    // Mutating a foreign CFVO formula after the shift must reject Undo via CAS.
    if let CfKind::Scale { points } = &mut wb.sheets[1].cf.blocks[0].rules[0].kind {
        points[0].0.formula = "=Data!$A$99".into();
    }
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    if let CfKind::Scale { points } = &mut wb.sheets[1].cf.blocks[0].rules[0].kind {
        points[0].0.formula = "=Data!$A$5".into();
    }
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_cfvos(&wb, "=Data!$A$3", "=Data!$A$5");
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_cfvos(&wb, "=Data!$A$5", "=Data!$A$7");
}

#[test]
fn axis_move_undo_redo_restores_charts_case_insensitive_sparklines_and_pivots_with_cas() {
    use sheetcore::{
        charts::{Chart, ChartKind, Series},
        pivot::{Agg, GroupBy, PivotSpec},
        pivot_engine::{PivotSlicerSpec, PivotTableModel},
        pivot_multi::{AxisField, PivotLayout, PivotSpec2, ShowAs, ValueField},
        sparkline::Sparkline,
    };

    let make_pivot_model = |src_id: u64, target_sheet: usize, src_rect: (u32, u32, u32, u32), out_rect: (u32, u32, u32, u32), row_col: u32, val_col: u32| {
        let spec = PivotSpec2 {
            source: src_rect,
            rows: vec![AxisField::plain(row_col)],
            cols: Vec::new(),
            values: vec![ValueField {
                col: val_col,
                agg: Agg::Sum,
                label: None,
                show_as: ShowAs::Normal,
                base: None,
            }],
            filters: Vec::new(),
            layout: PivotLayout::default(),
        };
        let mut p = PivotTableModel::new(
            "SalesPivot".into(),
            "Data".into(),
            Some(src_id),
            target_sheet,
            (out_rect.0, out_rect.1),
            spec,
        );
        p.rect = out_rect;
        p.last_meta.anchor = p.anchor;
        p.last_meta.rect = out_rect;
        p.options.slicers.push(PivotSlicerSpec {
            id: 1,
            col: row_col,
            title: "Region".into(),
            pos: [0.0, 0.0],
            width: 120.0,
            columns: 1,
        });
        p
    };

    // 1) Row move: target + foreign charts (with size_ref/source_range), case-insensitive foreign sparklines, wb.pivots, pivots_pending, pivots_pending2
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let data_id = wb.sheets[0].identity;
    for r in 0..8u32 {
        wb.sheets[0].set(r, 0, number(f64::from(r + 1)));
        wb.sheets[0].set(r, 1, number(f64::from((r + 1) * 10)));
    }
    let mut target_chart = Chart {
        kind: ChartKind::Bubble,
        title: "TargetChart".into(),
        anchor: (1, 4, 3, 8),
        source_range: "Data!$A$2:$B$3".into(),
        series: vec![Series {
            name_ref: "Data!$A$2".into(),
            cat_ref: "Data!$A$2:$A$3".into(),
            val_ref: "Data!$B$2:$B$3".into(),
            size_ref: "Data!$C$2:$C$3".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    target_chart.part = "xl/charts/chart1.xml".into();
    wb.sheets[0].charts.push(target_chart);

    let mut dash = Sheet::new("Dash");
    let mut foreign_chart = Chart {
        kind: ChartKind::Bubble,
        title: "ForeignChart".into(),
        anchor: (0, 0, 8, 5),
        source_range: "Data!$A$2:$B$3".into(),
        series: vec![Series {
            name_ref: "Data!$A$2".into(),
            cat_ref: "Data!$A$2:$A$3".into(),
            val_ref: "Data!$B$2:$B$3".into(),
            size_ref: "Data!$C$2:$C$3".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    foreign_chart.part = "xl/charts/chart2.xml".into();
    dash.charts.push(foreign_chart);
    // Lowercase "data" tests case-insensitive cross-sheet sparkline tracking & restoration.
    dash.sparklines.push(Sparkline {
        r: 0,
        c: 0,
        source_sheet: Some("data".into()),
        rect: (1, 0, 2, 0),
        ..Default::default()
    });
    wb.sheets.push(dash);

    let orig_pivot = make_pivot_model(data_id, 1, (1, 0, 2, 1), (0, 3, 3, 5), 0, 1);
    wb.pivots.push(orig_pivot.clone());
    let legacy_spec = PivotSpec {
        source: (1, 0, 2, 1),
        row_field: 0,
        col_field: None,
        value_field: 1,
        agg: Agg::Sum,
        group: GroupBy::None,
        exclude: Vec::new(),
    };
    wb.pivots_pending.push((0, legacy_spec.clone(), Default::default(), (10, 0, 12, 1), "Val".into()));
    wb.pivots_pending2.push((0, PivotSpec2::from_legacy(&legacy_spec), Default::default(), (14, 0, 16, 1)));

    let orig_target_chart = wb.sheets[0].charts[0].clone();
    let orig_foreign_chart = wb.sheets[1].charts[0].clone();
    let orig_spark = wb.sheets[1].sparklines[0].clone();
    let orig_pending = wb.pivots_pending[0].1.source;
    let orig_pending2 = wb.pivots_pending2[0].1.source;

    // Move rows 1..=2 down before row 6 (drop = 6 -> final at = 4, rows 1..=2 move to 4..=5).
    let mut undo = UndoStack::new();
    let before = snapshot_axis_move(&wb, 0, Axis::Row, 1, 2, 6).unwrap();
    move_rows_wb(&mut wb, 0, 1, 2, 6);
    let after = before.capture_after(&wb);
    undo.push_structure("移动行", before, after, (0, 1, 0));

    assert_eq!(wb.sheets[0].charts[0].series[0].val_ref, "Data!$B$5:$B$6");
    assert_eq!(wb.sheets[0].charts[0].series[0].size_ref, "Data!$C$5:$C$6");
    assert_eq!(wb.sheets[0].charts[0].source_range, "Data!$A$5:$B$6");
    assert_eq!(wb.sheets[1].charts[0].series[0].val_ref, "Data!$B$5:$B$6");
    assert_eq!(wb.sheets[1].charts[0].series[0].size_ref, "Data!$C$5:$C$6");
    assert_eq!(wb.sheets[1].charts[0].source_range, "Data!$A$5:$B$6");
    assert_eq!(wb.sheets[1].sparklines[0].rect, (4, 0, 5, 0));
    assert_eq!(wb.pivots[0].spec.source, (4, 0, 5, 1));
    assert_eq!(wb.pivots_pending[0].1.source, (4, 0, 5, 1));
    assert_eq!(wb.pivots_pending2[0].1.source, (4, 0, 5, 1));

    // Foreign chart presentation edit survives undo while refs & ref_fixes restore.
    wb.sheets[1].charts[0].title = "EditedTitle".into();
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sheets[0].charts[0].anchor, orig_target_chart.anchor);
    assert_eq!(wb.sheets[0].charts[0].series[0].val_ref, orig_target_chart.series[0].val_ref);
    assert_eq!(wb.sheets[0].charts[0].series[0].size_ref, orig_target_chart.series[0].size_ref);
    assert_eq!(wb.sheets[0].charts[0].source_range, orig_target_chart.source_range);
    assert_eq!(wb.sheets[1].charts[0].title, "EditedTitle");
    assert_eq!(wb.sheets[1].charts[0].series[0].val_ref, orig_foreign_chart.series[0].val_ref);
    assert_eq!(wb.sheets[1].charts[0].series[0].size_ref, orig_foreign_chart.series[0].size_ref);
    assert_eq!(wb.sheets[1].charts[0].source_range, orig_foreign_chart.source_range);
    assert!(wb.sheets[1].charts[0].ref_fixes.contains(&("Data!$C$5:$C$6".into(), "Data!$C$2:$C$3".into())));
    assert_eq!(wb.sheets[1].sparklines[0], orig_spark);
    assert_eq!(wb.pivots[0].spec.source, orig_pivot.spec.source);
    assert_eq!(wb.pivots_pending[0].1.source, orig_pending);
    assert_eq!(wb.pivots_pending2[0].1.source, orig_pending2);

    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sheets[1].charts[0].series[0].size_ref, "Data!$C$5:$C$6");
    assert_eq!(wb.sheets[1].sparklines[0].rect, (4, 0, 5, 0));
    assert_eq!(wb.pivots[0].spec.source, (4, 0, 5, 1));

    // 2) Column move: wb.pivots col fields & slicers + CAS rejection for later-added wb.pivots referencing moved area
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let data_id = wb.sheets[0].identity;
    wb.sheets.push(Sheet::new("Dash"));
    wb.pivots.push(make_pivot_model(data_id, 1, (0, 0, 4, 1), (0, 0, 3, 2), 0, 1));
    let mut undo = UndoStack::new();
    let before = snapshot_axis_move(&wb, 0, Axis::Col, 0, 2, 5).unwrap();
    move_cols_wb(&mut wb, 0, 0, 2, 5);
    let after = before.capture_after(&wb);
    undo.push_structure("移动列", before, after, (0, 0, 0));
    assert_eq!(wb.pivots[0].spec.source, (0, 3, 4, 4));
    assert_eq!(wb.pivots[0].spec.rows[0].col, 3);
    assert_eq!(wb.pivots[0].spec.values[0].col, 4);
    assert_eq!(wb.pivots[0].options.slicers[0].col, 3);
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.pivots[0].spec.source, (0, 0, 4, 1));
    assert_eq!(wb.pivots[0].spec.rows[0].col, 0);
    assert_eq!(wb.pivots[0].spec.values[0].col, 1);
    assert_eq!(wb.pivots[0].options.slicers[0].col, 0);

    // 3) CAS rejection when a LATER wb.pivots entry is added referencing the moved area (starting from zero pivots!)
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let data_id = wb.sheets[0].identity;
    wb.sheets.push(Sheet::new("Dash"));
    wb.sheets[0].set(2, 0, number(5.0));
    let mut undo = UndoStack::new();
    let before = snapshot_axis_move(&wb, 0, Axis::Row, 2, 1, 0).unwrap();
    move_rows_wb(&mut wb, 0, 2, 1, 0);
    let after = before.capture_after(&wb);
    undo.push_structure("移动行", before, after, (0, 2, 0));
    // Add a late pivot referencing row 0 (inside the permuted band [0..3)) -> CAS MUST reject Undo!
    let displaced = make_pivot_model(data_id, 1, (0, 0, 1, 1), (0, 0, 2, 2), 0, 1);
    wb.pivots.push(displaced);
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(undo.depth(), (1, 0));
    // Replace with an unrelated pivot outside [0..3) (rows 10..=12) -> Undo MUST succeed and preserve it!
    wb.pivots[0].spec.source = (10, 0, 12, 1);
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.pivots.len(), 1);
    assert_eq!(wb.pivots[0].spec.source, (10, 0, 12, 1));
}

#[test]
fn structure_snapshot_cas_detects_protected_ranges_very_hidden_and_cell_shift_ignored_errors() {
    use sheetcore::{
        cells::Shift,
        protection::ProtectedRange,
        structural::CellShift,
        undo::snapshot_cell_shift_if_metadata,
    };

    // 1) protected_ranges & very_hidden mutations rejected by StructureSnapshot CAS
    let mut wb = Workbook::new_blank();
    wb.sheets[0]
        .protected_ranges
        .push(ProtectedRange::new("Editable", vec![(3, 0, 5, 2)], ""));
    let mut undo = UndoStack::new();
    structure(&mut wb, &mut undo, Structural::InsertRows { at: 1, n: 2 });
    assert_eq!(wb.sheets[0].protected_ranges[0].sqref, vec![(5, 0, 7, 2)]);

    // Mutate protected_ranges directly -> CAS rejects Undo.
    wb.sheets[0]
        .protected_ranges
        .push(ProtectedRange::new("Extra", vec![(10, 0, 10, 0)], ""));
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    assert_eq!(undo.depth(), (1, 0));
    wb.sheets[0].protected_ranges.pop();

    // Mutate very_hidden directly -> CAS rejects Undo.
    wb.sheets[0].very_hidden = true;
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_none());
    wb.sheets[0].very_hidden = false;

    wb.sheets[0].protected_ranges_dirty = false;
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sheets[0].protected_ranges[0].sqref, vec![(3, 0, 5, 2)]);
    assert!(wb.sheets[0].protected_ranges_dirty);

    // 2) CellShift with ONLY ignored_errors captures metadata snapshot and restores cleanly
    let mut wb = Workbook::new_blank();
    wb.sheets[0].ignored_errors.insert((3, 0));
    let op = CellShift::new((1, 0, 2, 0), Shift::Vertical, true);
    let before = snapshot_cell_shift_if_metadata(&wb, 0, &op).expect("ignored_errors must trigger metadata snapshot");
    assert!(op.apply(&mut wb, 0));
    assert!(wb.sheets[0].ignored_errors.contains(&(5, 0)));
    assert!(!wb.sheets[0].ignored_errors.contains(&(3, 0)));
    assert!(before.cell_shift_changed_metadata(&wb));
    let after = before.capture_after(&wb);
    let mut undo = UndoStack::new();
    undo.push_structure("shift", before, after, (0, 1, 0));
    wb.sheets[0].ignored_errors_dirty = false;
    undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert!(wb.sheets[0].ignored_errors.contains(&(3, 0)));
    assert!(!wb.sheets[0].ignored_errors.contains(&(5, 0)));
    assert!(wb.sheets[0].ignored_errors_dirty);
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert!(wb.sheets[0].ignored_errors.contains(&(5, 0)));
}
