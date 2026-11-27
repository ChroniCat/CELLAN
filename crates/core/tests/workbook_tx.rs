//! Phase 3 integration tests for the unified workbook transaction layer
//! (`sheetcore::tx`), converged `UndoStack` `Entry::Tx`, and wire-compatible
//! `TxPatch` / `TxOverlay`.

use std::sync::Arc;

use sheetcore::cell_shift_patch::CellShiftPatch;
use sheetcore::cells::Shift as CellShiftDir;
use sheetcore::cf::{CfBlock, CfKind, CfRule, Cfvo, CfvoKind};
use sheetcore::charts::{Chart, ChartKind, Series};
use sheetcore::dv::{DataValidation, DvKind};
use sheetcore::external::{Kind as SourceKind, Source};
use sheetcore::hyperlinks::Hyperlink;
use sheetcore::model::{Cell, CellRef, Sheet, Value, Workbook};
use sheetcore::notes::{Note, ThreadEntry, ThreadedComment};
use sheetcore::protection::ProtectedRange;
use sheetcore::refs::Axis;
use sheetcore::richtext::RichRun;
use sheetcore::scenarios::Scenario;
use sheetcore::sort_filter::Filter;
use sheetcore::sparkline::Sparkline;
use sheetcore::structural::{CellShift, CellShiftConflict, Structural};
use sheetcore::table::Table;
use sheetcore::transform::CoordTransform;
use sheetcore::tx::{
    CellBatchIntent, CellWrite, DefinedNameIntent, RecalcScope, SheetTabIntent, TxCommit, TxDelta,
    TxEffects, TxError, TxIntent, TxOverlay, TxPatch, WorkbookTx,
};
use sheetcore::undo::{ChartUndoEdit, MetaEdit, PivotUndoEdit, UndoStack};

fn num(v: f64) -> Cell {
    Cell {
        value: Value::Number(v),
        formula: None,
    }
}

fn txt(s: &str) -> Cell {
    Cell {
        value: Value::Text(s.into()),
        formula: None,
    }
}

fn fml(s: &str) -> Cell {
    Cell {
        value: Value::Empty,
        formula: Some(s.into()),
    }
}

fn allow_all(_: usize, _: u32, _: u32, _: &Cell) -> bool {
    true
}

#[test]
fn structural_tx_executes_and_roundtrips_undo_redo() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(0, 0, num(10.0));
    wb.sheets[0].set(1, 0, num(20.0));
    wb.sheets[0].set(2, 0, num(30.0));
    wb.sheets[0].set(0, 1, fml("=SUM(A1:A3)"));

    let mut report = Sheet::new("Report");
    report.set(0, 0, fml("=Data!A2+Data!A3"));
    report.cf.blocks.push(CfBlock {
        ranges: vec![(0, 0, 2, 0)],
        rules: vec![CfRule {
            kind: CfKind::Bar {
                min: Cfvo {
                    kind: CfvoKind::Formula,
                    val: 0.0,
                    formula: "=Data!$A$2".into(),
                },
                max: Cfvo {
                    kind: CfvoKind::Formula,
                    val: 0.0,
                    formula: "=Data!$A$3".into(),
                },
                color: [0x33, 0x66, 0xCC],
                neg_color: None,
                show_value: true,
            },
            stop: false,
            priority: 1,
        }],
    });
    report.sparklines.push(Sparkline {
        r: 1,
        c: 0,
        source_sheet: Some("Data".into()),
        rect: (0, 0, 2, 0),
        ..Default::default()
    });
    report.charts.push(Chart {
        kind: ChartKind::Column,
        source_range: "Data!$A$1:$A$3".into(),
        series: vec![Series {
            val_ref: "Data!$A$1:$A$3".into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    wb.sheets.push(report);
    wb.names.push(("Total".into(), None, "Data!$A$1:$A$3".into()));

    let mut undo = UndoStack::new();
    let intent = TxIntent::from_structural(0, &Structural::InsertRows { at: 1, n: 2 });
    let commit = WorkbookTx::execute(&mut wb, intent, (0, 1, 0)).unwrap();

    assert!(commit.effects.structural);
    assert!(commit.effects.coord_changed);
    assert!(commit.effects.invalidate_deps);
    assert!(commit.effects.invalidate_render_caches);
    assert_eq!(commit.effects.recalc, RecalcScope::Full);
    assert!(!commit.effects.detached_sources);
    assert!(commit.effects.touched_sheets.contains(&0));
    assert!(commit.effects.touched_sheets.contains(&1));
    assert_eq!(commit.effects.new_focus, None);
    assert!(matches!(commit.delta, TxDelta::Structure(_)));

    let entry_id = undo.push_tx(commit);
    assert!(entry_id > 0);
    assert_eq!(undo.top_undo_id(), Some(entry_id));
    assert!(undo.undo_is_structure());

    assert_eq!(wb.sheets[0].value(3, 0), Value::Number(20.0));
    assert_eq!(wb.sheets[0].value(4, 0), Value::Number(30.0));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=Data!A4+Data!A5"));
    assert_eq!(wb.names[0].2, "Data!$A$1:$A$5");
    assert_eq!(wb.sheets[1].sparklines[0].rect, (0, 0, 4, 0));
    assert_eq!(wb.sheets[1].charts[0].series[0].val_ref, "Data!$A$1:$A$5");

    let undone = undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert!(undone.structural);
    assert!(undone.coord_changed);
    assert_eq!(undo.top_redo_id(), Some(entry_id));
    assert!(undo.redo_is_structure());
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(20.0));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(30.0));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=Data!A2+Data!A3"));
    assert_eq!(wb.names[0].2, "Data!$A$1:$A$3");
    assert_eq!(wb.sheets[1].sparklines[0].rect, (0, 0, 2, 0));
    assert_eq!(wb.sheets[1].charts[0].series[0].val_ref, "Data!$A$1:$A$3");

    let redone = undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert!(redone.structural);
    assert!(redone.coord_changed);
    assert_eq!(wb.sheets[0].value(3, 0), Value::Number(20.0));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=Data!A4+Data!A5"));
    assert_eq!(wb.names[0].2, "Data!$A$1:$A$5");

    // Also test DeleteCols via WorkbookTx.
    let del_col = TxIntent::from_structural(0, &Structural::DeleteCols { at: 1, n: 1 });
    let c2 = WorkbookTx::execute(&mut wb, del_col, (0, 0, 1)).unwrap();
    assert!(undo.push_tx(c2) > entry_id);
    assert!(wb.sheets[0].get(0, 1).is_none());
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(wb.sheets[0].formula_of(0, 1), Some("=SUM(A1:A5)"));
}

#[test]
fn sparse_cell_shift_tx_revision_deltas_coord_rev_and_conflict_guard() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(1, 0, num(11.0));
    wb.sheets[0].set(2, 0, num(22.0));
    wb.sheets[0].set_style(2, 0, 3);
    let mut other = Sheet::new("Other");
    other.set(0, 0, fml("=S!A2+S!A3"));
    wb.sheets.push(other);

    let rev0 = wb.sheets[0].coordinate_revision();
    let op = CellShift::new((1, 0, 1, 0), CellShiftDir::Vertical, true);
    let commit = WorkbookTx::execute(&mut wb, TxIntent::from_cell_shift(0, &op), (0, 1, 0)).unwrap();

    assert!(matches!(commit.delta, TxDelta::SparseShift { .. }));
    assert!(!commit.effects.structural);
    assert!(commit.effects.coord_changed);
    assert_eq!(commit.effects.recalc, RecalcScope::Full);
    assert_eq!(commit.effects.touched_sheets, vec![0, 1]);
    // Revision deltas must include moved target cells AND rewritten cross-sheet formula.
    assert!(
        commit
            .effects
            .revision_deltas
            .iter()
            .any(|d| d.sheet == 0 && d.row == 3 && d.col == 0 && d.after.value == Value::Number(22.0) && d.after_style == 3)
    );
    assert!(
        commit
            .effects
            .revision_deltas
            .iter()
            .any(|d| d.sheet == 1 && d.row == 0 && d.col == 0 && d.after.formula.as_deref() == Some("=S!A3+S!A4"))
    );

    let rev1 = wb.sheets[0].coordinate_revision();
    assert_ne!(rev1, rev0);

    let mut undo = UndoStack::new();
    let id = undo.push_tx(commit);
    assert!(id > 0);
    assert!(undo.next_is_cell_shift(false));
    assert!(!undo.undo_is_structure());

    // Conflicting concurrent modification blocks undo atomically without dropping history.
    wb.sheets[0].set(3, 0, num(999.0));
    let blocked = undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert!(blocked.skipped > 0);
    assert!(blocked.touched.is_empty());
    assert_eq!(undo.depth(), (1, 0));
    assert_eq!(wb.sheets[0].value(3, 0), Value::Number(999.0));

    // Restore expected value and undo cleanly.
    wb.sheets[0].set(3, 0, num(22.0));
    let undone = undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(undone.skipped, 0);
    assert!(!undone.structural);
    assert!(undone.coord_changed);
    assert_ne!(wb.sheets[0].coordinate_revision(), rev1);
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(11.0));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(22.0));
    assert_eq!(wb.sheets[0].style_of(2, 0), 3);
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A2+S!A3"));
    assert!(undo.next_is_cell_shift(true));

    let redone = undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(redone.skipped, 0);
    assert!(redone.coord_changed);
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(11.0));
    assert_eq!(wb.sheets[0].value(3, 0), Value::Number(22.0));
    assert_eq!(wb.sheets[0].style_of(3, 0), 3);
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A3+S!A4"));

    // Empty-band CellShift returns Ok with empty SparseShift; push_tx returns 0 and does not push.
    let empty_op = CellShift::new((50, 5, 50, 5), CellShiftDir::Horizontal, true);
    let empty_commit =
        WorkbookTx::execute(&mut wb, TxIntent::from_cell_shift(0, &empty_op), (0, 50, 5)).unwrap();
    assert!(empty_commit.delta.is_empty());
    assert!(empty_commit.effects.revision_deltas.is_empty());
    let depth_before = undo.depth();
    assert_eq!(undo.push_tx(empty_commit), 0);
    assert_eq!(undo.depth(), depth_before);
}

#[test]
fn metadata_bearing_cell_shift_promotes_to_structure_and_reports_detached_sources() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(2, 0, num(5.0));
    wb.sheets[0].set(2, 1, num(6.0));
    wb.sheets[0].notes.insert(
        CellRef::new(2, 0),
        Note {
            r: 2,
            c: 0,
            author: "tester".into(),
            text: "memo".into(),
        },
    );
    wb.sheets[0].threads.insert(
        CellRef::new(3, 0),
        ThreadedComment {
            r: 3,
            c: 0,
            resolved: false,
            entries: vec![ThreadEntry {
                id: "t1".into(),
                author: "tester".into(),
                timestamp: "2026-11-27".into(),
                text: "comment".into(),
            }],
        },
    );
    let mut bold_run = RichRun::plain("5");
    bold_run.bold = true;
    wb.sheets[0].rich.insert((2, 0), vec![bold_run]);
    wb.sheets[0].hyperlinks.insert(
        (2, 0),
        Hyperlink {
            location: "Data!A3".into(),
            ..Default::default()
        },
    );
    wb.sheets[0].ignored_errors.insert((2, 0));
    wb.names.push(("RefA3".into(), None, "Data!$A$3".into()));

    // Internal source spanning A3:B3 will be split when shifting only col A down -> detached!
    wb.sources.push(Source {
        name: "Src1".into(),
        kind: SourceKind::Internal {
            sheet: "Data".into(),
            rect: (2, 0, 2, 1),
        },
        at: (10, 0),
        target_sheet: Some("Data".into()),
        rows: 1,
        cols: 2,
        csv_opts: None,
    });

    let mut rules = Sheet::new("Rules");
    rules.cf.blocks.push(CfBlock {
        ranges: vec![(0, 0, 1, 0)],
        rules: vec![CfRule {
            kind: CfKind::Bar {
                min: Cfvo {
                    kind: CfvoKind::Formula,
                    val: 0.0,
                    formula: "=Data!$A$3".into(),
                },
                max: Cfvo {
                    kind: CfvoKind::Formula,
                    val: 0.0,
                    formula: "=Data!$A$5".into(),
                },
                color: [0x22, 0x88, 0x33],
                neg_color: None,
                show_value: true,
            },
            stop: false,
            priority: 1,
        }],
    });
    wb.sheets.push(rules);

    let op = CellShift::new((1, 0, 1, 0), CellShiftDir::Vertical, true);
    let commit = WorkbookTx::execute(&mut wb, TxIntent::from_cell_shift(0, &op), (0, 1, 0)).unwrap();

    assert!(matches!(commit.delta, TxDelta::Structure(_)));
    assert!(commit.effects.structural);
    assert!(commit.effects.detached_sources);
    assert!(!commit.effects.revision_deltas.is_empty());
    assert!(commit.effects.touched_sheets.contains(&0));
    assert!(commit.effects.touched_sheets.contains(&1));

    let mut undo = UndoStack::new();
    let id = undo.push_tx(commit);
    assert!(id > 0);
    assert!(undo.undo_is_structure());

    // Forward checks
    assert!(wb.sheets[0].notes.contains_key(&CellRef::new(3, 0)));
    assert!(wb.sheets[0].threads.contains_key(&CellRef::new(4, 0)));
    assert!(wb.sheets[0].rich.contains_key(&(3, 0)));
    assert!(wb.sheets[0].ignored_errors.contains(&(3, 0)));
    assert_eq!(wb.sheets[0].hyperlinks.get(&(3, 0)).unwrap().location, "Data!A4");
    assert_eq!(wb.names[0].2, "Data!$A$4");
    assert!(matches!(wb.sources[0].kind, SourceKind::Unbound(_)));
    if let CfKind::Bar { min, .. } = &wb.sheets[1].cf.blocks[0].rules[0].kind {
        assert_eq!(min.formula, "=Data!$A$4");
    } else {
        panic!("expected bar rule");
    }

    // Undo restores all metadata + bound source + foreign CFVO.
    let undone = undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert!(undone.structural);
    assert!(undone.coord_changed);
    assert!(wb.sheets[0].notes.contains_key(&CellRef::new(2, 0)));
    assert!(wb.sheets[0].threads.contains_key(&CellRef::new(3, 0)));
    assert!(wb.sheets[0].rich.contains_key(&(2, 0)));
    assert!(wb.sheets[0].ignored_errors.contains(&(2, 0)));
    assert_eq!(wb.sheets[0].hyperlinks.get(&(2, 0)).unwrap().location, "Data!A3");
    assert_eq!(wb.names[0].2, "Data!$A$3");
    assert!(!matches!(wb.sources[0].kind, SourceKind::Unbound(_)));
    if let CfKind::Bar { min, .. } = &wb.sheets[1].cf.blocks[0].rules[0].kind {
        assert_eq!(min.formula, "=Data!$A$3");
    }

    // Redo reapplies cleanly.
    undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert!(wb.sheets[0].notes.contains_key(&CellRef::new(3, 0)));
    assert!(matches!(wb.sources[0].kind, SourceKind::Unbound(_)));
}

#[test]
fn metadata_only_cell_shift_with_zero_cells_still_records_structure_tx() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    // No cells on Data at all; only a note on an empty cell and a foreign CFVO rule.
    wb.sheets[0].notes.insert(
        CellRef::new(2, 0),
        Note {
            r: 2,
            c: 0,
            author: "a".into(),
            text: "empty-cell-note".into(),
        },
    );
    let op = CellShift::new((1, 0, 1, 0), CellShiftDir::Vertical, true);
    let commit = WorkbookTx::execute(&mut wb, TxIntent::from_cell_shift(0, &op), (0, 1, 0)).unwrap();
    assert!(commit.effects.revision_deltas.is_empty());
    assert!(commit.effects.structural);
    assert!(matches!(commit.delta, TxDelta::Structure(_)));

    let mut undo = UndoStack::new();
    assert!(undo.push_tx(commit) > 0);
    assert!(wb.sheets[0].notes.contains_key(&CellRef::new(3, 0)));

    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert!(wb.sheets[0].notes.contains_key(&CellRef::new(2, 0)));
    assert!(!wb.sheets[0].notes.contains_key(&CellRef::new(3, 0)));

    undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert!(wb.sheets[0].notes.contains_key(&CellRef::new(3, 0)));
}

#[test]
fn axis_permute_rows_and_cols_tx_roundtrip() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Grid".into();
    for r in 0..4u32 {
        wb.sheets[0].set(r, 0, txt(&format!("R{r}")));
        wb.sheets[0].set(r, 1, num((r + 1) as f64 * 10.0));
    }
    let mut summary = Sheet::new("Summary");
    summary.set(0, 0, fml("=Grid!B1"));
    summary.set(1, 0, fml("=Grid!B4"));
    wb.sheets.push(summary);
    wb.names.push(("FirstVal".into(), None, "Grid!$B$1".into()));

    // Move row 0..1 to drop line 3 (final row 2).
    let row_intent = TxIntent::from_axis_move(0, Axis::Row, 0, 1, 3).unwrap();
    let c1 = WorkbookTx::execute(&mut wb, row_intent, (0, 0, 0)).unwrap();
    assert!(c1.effects.structural);
    assert!(c1.effects.coord_changed);
    assert_eq!(c1.label, "移动行");
    let mut undo = UndoStack::new();
    assert!(undo.push_tx(c1) > 0);

    assert_eq!(wb.sheets[0].value(2, 0), Value::Text("R0".into()));
    assert_eq!(wb.sheets[0].value(0, 0), Value::Text("R1".into()));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=Grid!B3"));
    assert_eq!(wb.names[0].2, "Grid!$B$3");

    // Move col 1 to final col 0 using `from_axis_permute`.
    let col_intent = TxIntent::from_axis_permute(0, Axis::Col, 1, 1, 0).unwrap();
    let c2 = WorkbookTx::execute(&mut wb, col_intent, (0, 0, 1)).unwrap();
    assert_eq!(c2.label, "移动列");
    assert!(undo.push_tx(c2) > 0);
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(10.0));
    assert_eq!(wb.sheets[0].value(2, 1), Value::Text("R0".into()));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=Grid!A3"));
    assert_eq!(wb.names[0].2, "Grid!$A$3");

    // Undo col move, then undo row move.
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=Grid!B3"));
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(wb.sheets[0].value(0, 0), Value::Text("R0".into()));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=Grid!B1"));
    assert_eq!(wb.names[0].2, "Grid!$B$1");

    // Redo both.
    undo.redo_workbook(&mut wb, allow_all).unwrap();
    undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(10.0));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=Grid!A3"));
}

#[test]
fn multi_insert_rows_and_cols_tx_roundtrip() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    for r in 0..4u32 {
        wb.sheets[0].set(r, 0, num((r + 1) as f64));
        wb.sheets[0].set(r, 1, num((r + 1) as f64 * 10.0));
    }
    let mut other = Sheet::new("Ref");
    other.set(0, 0, fml("=S!A2+S!A4"));
    wb.sheets.push(other);

    let mut undo = UndoStack::new();
    let row_tx = TxIntent::from_multi_insert(0, Axis::Row, vec![3, 1]);
    let c1 = WorkbookTx::execute(&mut wb, row_tx, (0, 1, 0)).unwrap();
    assert!(c1.effects.structural);
    assert!(undo.push_tx(c1) > 0);
    // Old row 1 -> row 2, old row 3 -> row 5.
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(2.0));
    assert_eq!(wb.sheets[0].value(5, 0), Value::Number(4.0));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A3+S!A6"));

    // Multi-insert cols at pre-insertion cols [0, 2].
    let col_tx = TxIntent::from_multi_insert(0, Axis::Col, vec![0, 2]);
    let c2 = WorkbookTx::execute(&mut wb, col_tx, (0, 0, 0)).unwrap();
    assert!(undo.push_tx(c2) > 0);
    assert_eq!(wb.sheets[0].value(2, 1), Value::Number(2.0));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!B3+S!B6"));

    // Undo both and verify original state.
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A3+S!A6"));
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(2.0));
    assert_eq!(wb.sheets[0].value(3, 0), Value::Number(4.0));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!A2+S!A4"));

    // Redo both.
    undo.redo_workbook(&mut wb, allow_all).unwrap();
    undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(wb.sheets[0].value(2, 1), Value::Number(2.0));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=S!B3+S!B6"));
}

#[test]
fn all_five_sheet_tab_intents_roundtrip_and_focus_tracking() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Main".into();
    wb.sheets[0].set(0, 0, num(42.0));
    let mut undo = UndoStack::new();

    // 1) Append
    let c_append = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_sheet_tab(SheetTabIntent::Append {
            sheet: Sheet::new("Sales"),
        }),
        (0, 2, 3),
    )
    .unwrap();
    assert_eq!(c_append.effects.new_focus, Some((1, 0, 0)));
    assert!(c_append.effects.structural);
    assert!(c_append.effects.coord_changed);
    assert!(undo.push_tx(c_append) > 0);
    assert_eq!(wb.sheets.len(), 2);
    assert_eq!(wb.sheets[1].name, "Sales");

    // Add cross-sheet formula on Main referencing Sales.
    wb.sheets[0].set(1, 0, fml("=Sales!A1+1"));
    wb.sheets[1].set(0, 0, num(100.0));

    // 2) Insert at index 1 with custom label
    let c_insert = WorkbookTx::execute_with_label(
        &mut wb,
        TxIntent::from_sheet_tab(SheetTabIntent::Insert {
            index: 1,
            sheet: Sheet::new("Mid"),
        }),
        Some("复制工作表".into()),
        (0, 0, 0),
    )
    .unwrap();
    assert_eq!(c_insert.label, "复制工作表");
    assert_eq!(c_insert.effects.new_focus, Some((1, 0, 0)));
    assert!(undo.push_tx(c_insert) > 0);
    assert_eq!(
        wb.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["Main", "Mid", "Sales"]
    );

    // 3) Rename Sales (index 2) -> Revenue
    let c_rename = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_sheet_tab(SheetTabIntent::Rename {
            index: 2,
            name: " Revenue ".into(),
        }),
        (2, 4, 1),
    )
    .unwrap();
    assert_eq!(c_rename.effects.new_focus, None);
    assert!(undo.push_tx(c_rename) > 0);
    assert_eq!(wb.sheets[2].name, "Revenue");
    assert_eq!(wb.sheets[0].formula_of(1, 0), Some("='Revenue'!A1+1"));

    // 4) Move Revenue (2 -> 0) while active sheet is Main (0 -> shifts to 1)
    let c_move = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_sheet_tab(SheetTabIntent::Move { from: 2, to: 0 }),
        (0, 1, 1),
    )
    .unwrap();
    assert_eq!(c_move.effects.new_focus, Some((1, 1, 1)));
    assert!(undo.push_tx(c_move) > 0);
    assert_eq!(
        wb.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["Revenue", "Main", "Mid"]
    );

    // 5) Delete Revenue (index 0) while active sheet is Main (index 1 -> shifts to 0)
    let c_del = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_sheet_tab(SheetTabIntent::Delete { index: 0 }),
        (1, 1, 1),
    )
    .unwrap();
    assert_eq!(c_del.effects.new_focus, Some((0, 1, 1)));
    assert!(undo.push_tx(c_del) > 0);
    assert_eq!(
        wb.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["Main", "Mid"]
    );
    assert!(wb.sheets[0].formula_of(1, 0).unwrap().contains("#REF!"));

    // Undo all 5 steps in reverse order.
    let u5 = undo.undo_workbook(&mut wb, allow_all).unwrap(); // undo Delete
    assert!(u5.structural && u5.coord_changed);
    assert_eq!(
        wb.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["Revenue", "Main", "Mid"]
    );
    assert_eq!(wb.sheets[1].formula_of(1, 0), Some("='Revenue'!A1+1"));

    undo.undo_workbook(&mut wb, allow_all).unwrap(); // undo Move
    assert_eq!(
        wb.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["Main", "Mid", "Revenue"]
    );

    undo.undo_workbook(&mut wb, allow_all).unwrap(); // undo Rename
    assert_eq!(wb.sheets[2].name, "Sales");
    assert_eq!(wb.sheets[0].formula_of(1, 0), Some("=Sales!A1+1"));

    undo.undo_workbook(&mut wb, allow_all).unwrap(); // undo Insert
    assert_eq!(
        wb.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["Main", "Sales"]
    );

    undo.undo_workbook(&mut wb, allow_all).unwrap(); // undo Append
    assert_eq!(
        wb.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["Main"]
    );

    // Redo all 5 steps back.
    for _ in 0..5 {
        let r = undo.redo_workbook(&mut wb, allow_all).unwrap();
        assert!(r.structural && r.coord_changed);
    }
    assert_eq!(
        wb.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["Main", "Mid"]
    );
    assert!(wb.sheets[0].formula_of(1, 0).unwrap().contains("#REF!"));

    // Check Delete focus when deleting active sheet vs right-hand sheet.
    let c_del_active = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_sheet_tab(SheetTabIntent::Delete { index: 1 }),
        (1, 5, 5),
    )
    .unwrap();
    assert_eq!(c_del_active.effects.new_focus, Some((0, 0, 0)));
}

#[test]
fn preflight_errors_leave_workbook_and_undo_untouched() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S1".into();
    wb.sheets[0].set(0, 0, num(7.0));
    wb.dirty = false;
    let rev = wb.sheets[0].coordinate_revision();
    let undo = UndoStack::new();

    let assert_untouched = |wb: &Workbook, undo: &UndoStack| {
        assert!(!wb.dirty);
        assert_eq!(wb.sheets.len(), 1);
        assert_eq!(wb.sheets[0].name, "S1");
        assert_eq!(wb.sheets[0].value(0, 0), Value::Number(7.0));
        assert_eq!(wb.sheets[0].coordinate_revision(), rev);
        assert_eq!(undo.depth(), (0, 0));
    };

    // InvalidSheet
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_structural(9, &Structural::InsertRows { at: 0, n: 1 }),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::InvalidSheet
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_sheet_tab(SheetTabIntent::Insert {
                index: 5,
                sheet: Sheet::new("X")
            }),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::InvalidSheet
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_sheet_tab(SheetTabIntent::Delete { index: 3 }),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::InvalidSheet
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_sheet_tab(SheetTabIntent::Move { from: 0, to: 2 }),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::InvalidSheet
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_sheet_tab(SheetTabIntent::Rename {
                index: 2,
                name: "New".into()
            }),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::InvalidSheet
    );

    // CannotDeleteLastSheet
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_sheet_tab(SheetTabIntent::Delete { index: 0 }),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::CannotDeleteLastSheet
    );

    // InvalidSheetName
    for bad in ["", "S1", "s1", "Bad/Name", "'Quoted'", &"A".repeat(32)] {
        let err = WorkbookTx::execute(
            &mut wb,
            TxIntent::from_sheet_tab(SheetTabIntent::Append {
                sheet: Sheet::new(bad),
            }),
            (0, 0, 0),
        )
        .unwrap_err();
        assert_eq!(err, TxError::InvalidSheetName, "name={bad:?}");
        assert!(!err.message().is_empty());
    }

    // NoOp
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_structural(0, &Structural::InsertRows { at: 0, n: 0 }),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::NoOp
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::Transform(CoordTransform::AxisPermute {
                sheet_idx: 0,
                axis: Axis::Row,
                from: 1,
                n: 2,
                at: 1,
            }),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::NoOp
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_multi_insert(0, Axis::Row, vec![]),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::NoOp
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_sheet_tab(SheetTabIntent::Move { from: 0, to: 0 }),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::NoOp
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_sheet_tab(SheetTabIntent::Rename {
                index: 0,
                name: " S1 ".into()
            }),
            (0, 0, 0)
        )
        .unwrap_err(),
        TxError::NoOp
    );
    assert_untouched(&wb, &undo);

    // CellShift conflicts: PartialMerge, Table, AutoFilter, Chart, Sparkline, ProtectedRange, Scenario, Overflow
    let op = CellShift::new((1, 0, 1, 0), CellShiftDir::Vertical, true);
    let intent = TxIntent::from_cell_shift(0, &op);

    wb.sheets[0].merges.push((1, 0, 1, 1));
    assert_eq!(
        WorkbookTx::execute(&mut wb, intent.clone(), (0, 1, 0)).unwrap_err(),
        TxError::Conflict(CellShiftConflict::PartialMerge)
    );
    wb.sheets[0].merges.clear();

    wb.sheets[0].tables.push(Table {
        name: "T1".into(),
        display: "T1".into(),
        range: (1, 0, 3, 1),
        header_row: true,
        totals_row: false,
        columns: vec!["A".into(), "B".into()],
        ..Default::default()
    });
    assert_eq!(
        WorkbookTx::execute(&mut wb, intent.clone(), (0, 1, 0)).unwrap_err(),
        TxError::Conflict(CellShiftConflict::Table)
    );
    wb.sheets[0].tables.clear();

    wb.sheets[0].filter = Some(Filter::new(1, 0, 3, 1));
    assert_eq!(
        WorkbookTx::execute(&mut wb, intent.clone(), (0, 1, 0)).unwrap_err(),
        TxError::Conflict(CellShiftConflict::AutoFilter)
    );
    wb.sheets[0].filter = None;

    wb.sheets[0].sparklines.push(Sparkline {
        r: 0,
        c: 5,
        source_sheet: None,
        // Straddles the col-A moving band (A3:B3) -> still blocked by preflight
        rect: (2, 0, 2, 1),
        ..Default::default()
    });
    assert_eq!(
        WorkbookTx::execute(&mut wb, intent.clone(), (0, 1, 0)).unwrap_err(),
        TxError::Conflict(CellShiftConflict::Sparkline)
    );
    wb.sheets[0].sparklines.clear();

    wb.sheets[0]
        .protected_ranges
        .push(ProtectedRange::new("R1", vec![(2, 0, 3, 0)], ""));
    assert_eq!(
        WorkbookTx::execute(&mut wb, intent.clone(), (0, 1, 0)).unwrap_err(),
        TxError::Conflict(CellShiftConflict::ProtectedRange)
    );
    wb.sheets[0].protected_ranges.clear();

    let mut sc = Scenario::new("Best");
    sc.cells.push(((2, 0), "1".into()));
    wb.sheets[0].scenarios.push(sc);
    assert_eq!(
        WorkbookTx::execute(&mut wb, intent.clone(), (0, 1, 0)).unwrap_err(),
        TxError::Conflict(CellShiftConflict::Scenario)
    );
    wb.sheets[0].scenarios.clear();

    wb.sheets[0].arrays.push((CellRef::new(1, 0), (1, 0, 2, 0)));
    assert_eq!(
        WorkbookTx::execute(&mut wb, intent.clone(), (0, 1, 0)).unwrap_err(),
        TxError::Conflict(CellShiftConflict::ArrayOrSpill)
    );
    wb.sheets[0].arrays.clear();

    wb.sheets[0].hyperlinks.insert(
        (1, 0),
        Hyperlink {
            target: "https://example.com".into(),
            span: (0, 1),
            ..Default::default()
        },
    );
    assert_eq!(
        WorkbookTx::execute(&mut wb, intent.clone(), (0, 1, 0)).unwrap_err(),
        TxError::Conflict(CellShiftConflict::PartialHyperlink)
    );
    wb.sheets[0].hyperlinks.clear();

    wb.sheets[0].set(sheetcore::sync::MAX_ROWS - 1, 0, num(1.0));
    assert_eq!(
        WorkbookTx::execute(&mut wb, intent, (0, 1, 0)).unwrap_err(),
        TxError::Conflict(CellShiftConflict::Overflow)
    );
    wb.sheets[0].remove(sheetcore::sync::MAX_ROWS - 1, 0);
    wb.dirty = false;
    assert_untouched(&wb, &undo);
}

#[test]
fn tx_patch_wire_compatibility_and_room_patch_undo_entry() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Main".into();
    wb.sheets[0].set(1, 0, num(10.0));
    wb.sheets[0].set(2, 0, num(20.0));
    wb.sheets[0].dv.push(DataValidation {
        ranges: vec![(1, 0, 3, 0)],
        kind: DvKind::List,
        f1: "\"A,B\"".into(),
        ..Default::default()
    });
    wb.names.push(("V".into(), None, "Main!$A$2".into()));

    let op = CellShift::new((1, 0, 1, 0), CellShiftDir::Vertical, true);
    let cs_patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    let tx_patch = TxPatch::prepare(&wb, &TxIntent::from_cell_shift(0, &op)).unwrap();

    // Byte-for-byte identical JSON wire serialization with CellShiftPatch.
    let cs_json = serde_json::to_string(&cs_patch).unwrap();
    let tx_json = serde_json::to_string(&tx_patch).unwrap();
    assert_eq!(tx_json, cs_json);

    let decoded_tx: TxPatch = serde_json::from_str(&cs_json).unwrap();
    let decoded_cs: CellShiftPatch = serde_json::from_str(&tx_json).unwrap();
    assert_eq!(decoded_tx, tx_patch);
    assert_eq!(decoded_cs, cs_patch);

    assert!(tx_patch.wire_valid());
    assert!(tx_patch.can_install(&wb));
    assert!(!tx_patch.is_empty());
    assert_eq!(tx_patch.target_sheet(), 0);
    assert_eq!(tx_patch.referenced_sheets(), vec![0]);
    assert_eq!(tx_patch.label(), "插入单元格");

    let canonical = tx_patch.canonical_forward(&wb).unwrap();
    assert_eq!(canonical, tx_patch);
    assert!(tx_patch.matches(&wb, true));
    assert!(tx_patch.matches_replica(&wb, true));

    // Apply committed forward and test overlay + room undo entry lifecycle.
    let mut replica = wb.clone();
    assert!(tx_patch.apply_committed(&mut wb, true));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(10.0));
    assert_eq!(wb.sheets[0].value(3, 0), Value::Number(20.0));
    assert_eq!(wb.names[0].2, "Main!$A$3");

    let overlay = TxOverlay::capture(&wb);
    assert!(overlay.apply_to(&mut replica));
    assert_eq!(replica.names[0].2, "Main!$A$3");

    let mut undo = UndoStack::new();
    undo.set_site(7);
    let patch_arc = Arc::new(tx_patch);
    let entry_id = undo.push_room_tx_patch(99, patch_arc.clone(), &wb);
    assert!(entry_id > 0);
    assert_eq!(undo.top_undo_id(), Some(entry_id));
    assert!(!undo.undo_is_structure());
    assert!(!undo.next_is_cell_shift(false));
    assert!(undo.room_shift_history_matches(&wb, false, false));

    // Local undo/undo_workbook must NOT consume RoomPatch directly (requires host confirmation).
    assert!(undo.undo(&mut wb.sheets, allow_all).is_none());
    assert!(undo.undo_workbook(&mut wb, allow_all).is_none());

    // Peek via both generalized and legacy compatibility accessors.
    let (tx_id, peeked_tx) = undo.peek_room_tx_patch(false).unwrap();
    assert_eq!(tx_id, 99);
    assert_eq!(*peeked_tx, *patch_arc);
    let (legacy_id, peeked_cs) = undo.peek_room_cell_shift(false).unwrap();
    assert_eq!(legacy_id, 99);
    assert_eq!(*peeked_cs, cs_patch);

    // Replay backward & confirm transition to redo stack.
    assert!(patch_arc.matches(&wb, false));
    assert!(patch_arc.apply(&mut wb, false));
    assert!(undo.confirm_room_cell_shift(99, false));
    assert_eq!(undo.depth(), (0, 1));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(10.0));
    assert_eq!(wb.names[0].2, "Main!$A$2");

    // Preserve ownership conflict when another peer wrote a moved cell before leaving room.
    let mut peer = sheetcore::sync::Peer::new(7);
    peer.apply(sheetcore::sync::Op {
        sheet: 0,
        row: 1,
        col: 0,
        value: sheetcore::sync::ValueWire::Number(10.0),
        formula: None,
        ts: 100,
        site: 8,
    });
    undo.preserve_room_shift_ownership(&peer);
    assert!(undo.room_shift_history_matches(&wb, true, false));
    assert!(!undo.room_shift_history_matches(&wb, true, true));
}

#[test]
fn large_structural_tx_background_compaction_and_history_limit() {
    let mut wb = Workbook::new_blank();
    for r in 0..17_000u32 {
        wb.sheets[0].set(r, 0, num(r as f64));
    }

    let mut undo = UndoStack::new();
    undo.limit = 2;

    let c1 = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_structural(0, &Structural::InsertRows { at: 1, n: 1 }),
        (0, 1, 0),
    )
    .unwrap();
    assert!(undo.push_tx(c1) > 0);

    let c2 = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_structural(0, &Structural::DeleteRows { at: 5, n: 2 }),
        (0, 5, 0),
    )
    .unwrap();
    assert!(undo.push_tx(c2) > 0);

    let c3 = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_axis_move(0, Axis::Row, 2, 2, 10).unwrap(),
        (0, 2, 0),
    )
    .unwrap();
    assert!(undo.push_tx(c3) > 0);

    // Limit = 2 evicted the oldest Entry::Tx while retaining the newest 2.
    assert_eq!(undo.depth(), (2, 0));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while undo.poll_compaction() {
        assert!(
            std::time::Instant::now() < deadline,
            "background compaction timed out"
        );
        std::thread::yield_now();
    }

    // Undo the 2 retained large structural transactions and verify exact cell restoration.
    assert!(undo.undo_workbook(&mut wb, allow_all).is_some());
    assert!(undo.undo_workbook(&mut wb, allow_all).is_some());
    assert!(!undo.can_undo());
    // After undoing c3 and c2, state matches after c1 (row 0 = 0, row 1 = empty, row 2 = 1, row 17000 = 16999).
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(0.0));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Empty);
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(1.0));
    assert_eq!(wb.sheets[0].value(17_000, 0), Value::Number(16_999.0));

    // Redo both and verify again.
    assert!(undo.redo_workbook(&mut wb, allow_all).is_some());
    assert!(undo.redo_workbook(&mut wb, allow_all).is_some());
    assert_eq!(undo.depth(), (2, 0));
}

#[test]
fn defined_name_intent_lifecycle_validation_and_cas_undo_redo() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Sales".into();
    wb.sheets.push(Sheet::new("Summary"));
    wb.dirty = false;
    wb.names_dirty = false;
    let epoch0 = wb.coord_epoch();
    let mut undo = UndoStack::new();

    // 1. Preflight rejections leave workbook untouched.
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_defined_names(DefinedNameIntent::Define {
                name: "   ".into(),
                scope: None,
                formula: "Sales!$A$1".into(),
            }),
            (0, 0, 0),
        )
        .unwrap_err(),
        TxError::InvalidDefinedName
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_defined_names(DefinedNameIntent::Define {
                name: "BadScope".into(),
                scope: Some(5),
                formula: "Sales!$A$1".into(),
            }),
            (0, 0, 0),
        )
        .unwrap_err(),
        TxError::InvalidSheet
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_defined_names(DefinedNameIntent::Remove { index: 0 }),
            (0, 0, 0),
        )
        .unwrap_err(),
        TxError::InvalidDefinedName
    );
    assert!(!wb.dirty && !wb.names_dirty && wb.names.is_empty());

    // 2. Define global name and sheet-scoped name with same identifier (allowed across distinct scopes).
    let c1 = WorkbookTx::execute(
        &mut wb,
        TxIntent::from(DefinedNameIntent::Define {
            name: " Rate_Month ".into(),
            scope: None,
            formula: " 0.05 ".into(),
        }),
        (0, 0, 0),
    )
    .unwrap();
    assert_eq!(c1.label, "定义名称 Rate_Month");
    assert_eq!(c1.effects.recalc, RecalcScope::Full);
    assert!(c1.effects.invalidate_deps);
    assert!(c1.effects.invalidate_render_caches);
    assert!(!c1.effects.coord_changed);
    assert!(!c1.effects.structural);
    assert!(wb.names_dirty && wb.dirty);
    assert_eq!(wb.coord_epoch(), epoch0);
    assert_eq!(
        wb.names,
        vec![("Rate_Month".into(), None, "0.05".into())]
    );
    assert!(undo.push_tx(c1) > 0);
    assert!(!undo.undo_is_structure());

    // Case-insensitive duplicate in same scope rejected with exact Chinese message.
    let dup_err = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_defined_names(DefinedNameIntent::Define {
            name: "rate_month".into(),
            scope: None,
            formula: "Sales!$A$1".into(),
        }),
        (0, 0, 0),
    )
    .unwrap_err();
    assert_eq!(dup_err, TxError::DuplicateDefinedName);
    assert_eq!(dup_err.message(), "名称已存在");

    // Same name in local scope 0 succeeds.
    let c2 = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_defined_names(DefinedNameIntent::Define {
            name: "Rate_Month".into(),
            scope: Some(0),
            formula: "Sales!$B$1".into(),
        }),
        (0, 0, 1),
    )
    .unwrap();
    assert!(undo.push_tx(c2) > 0);
    assert_eq!(wb.names.len(), 2);

    // 3. Update existing index keeping same name (different formula) succeeds; identical is NoOp; colliding with other index fails.
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_defined_names(DefinedNameIntent::Update {
                index: 0,
                name: "Rate_Month".into(),
                scope: None,
                formula: "0.05".into(),
            }),
            (0, 0, 0),
        )
        .unwrap_err(),
        TxError::NoOp
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_defined_names(DefinedNameIntent::Update {
                index: 1,
                name: "rate_month".into(),
                scope: None,
                formula: "0.08".into(),
            }),
            (0, 0, 0),
        )
        .unwrap_err(),
        TxError::DuplicateDefinedName
    );
    let c3 = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_defined_names(DefinedNameIntent::Update {
            index: 0,
            name: "Rate_Month".into(),
            scope: None,
            formula: "0.06".into(),
        }),
        (0, 0, 0),
    )
    .unwrap();
    assert!(undo.push_tx(c3) > 0);
    assert_eq!(wb.names[0], ("Rate_Month".into(), None, "0.06".into()));

    // 4. Remove & ReplaceAll
    let c4 = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_defined_names(DefinedNameIntent::Remove { index: 1 }),
        (0, 0, 0),
    )
    .unwrap();
    assert!(undo.push_tx(c4) > 0);
    assert_eq!(wb.names.len(), 1);

    // ReplaceAll rejects duplicates & NoOp.
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_defined_names(DefinedNameIntent::ReplaceAll {
                names: vec![
                    ("Dup".into(), None, "1".into()),
                    ("dup".into(), None, "2".into()),
                ],
            }),
            (0, 0, 0),
        )
        .unwrap_err(),
        TxError::DuplicateDefinedName
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_defined_names(DefinedNameIntent::ReplaceAll {
                names: vec![("Rate_Month".into(), None, "0.06".into())],
            }),
            (0, 0, 0),
        )
        .unwrap_err(),
        TxError::NoOp
    );
    let c5 = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_defined_names(DefinedNameIntent::ReplaceAll {
            names: vec![
                ("Q1_Sales".into(), None, "Sales!$A$1:$A$4".into()),
                ("LocalTax".into(), Some(1), "0.13".into()),
            ],
        }),
        (0, 0, 0),
    )
    .unwrap();
    assert!(undo.push_tx(c5) > 0);
    assert_eq!(wb.names.len(), 2);

    // 5. CAS guard: out-of-band modification rejects undo cleanly without losing history.
    let saved_names = wb.names.clone();
    wb.names.push(("Concurrent".into(), None, "99".into()));
    assert!(undo.undo_workbook(&mut wb, allow_all).is_none());
    assert_eq!(wb.names.len(), 3);
    assert_eq!(undo.depth(), (5, 0));

    // Restore expected live names -> all 5 undo & redo steps succeed cleanly with needs_recalc = true.
    wb.names = saved_names;
    wb.names_dirty = false;
    for _ in 0..5 {
        let applied = undo.undo_workbook(&mut wb, allow_all).unwrap();
        assert!(!applied.structural && !applied.coord_changed);
        assert!(undo.take_needs_recalc());
        assert!(wb.names_dirty);
    }
    assert!(wb.names.is_empty());
    assert_eq!(wb.coord_epoch(), epoch0);

    for _ in 0..5 {
        let applied = undo.redo_workbook(&mut wb, allow_all).unwrap();
        assert!(!applied.structural && !applied.coord_changed);
        assert!(undo.take_needs_recalc());
    }
    assert_eq!(
        wb.names,
        vec![
            ("Q1_Sales".into(), None, "Sales!$A$1:$A$4".into()),
            ("LocalTax".into(), Some(1), "0.13".into()),
        ]
    );
    assert_eq!(wb.coord_epoch(), epoch0);
}

#[test]
fn cell_batch_intent_values_formulas_styles_noop_and_guarded_undo_redo() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, num(10.0));
    wb.sheets[0].set(3, 0, txt("to-clear"));
    wb.sheets[0].set_style(0, 0, 2);
    wb.dirty = false;
    let epoch0 = wb.coord_epoch();

    // Preflight errors leave workbook untouched.
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_cell_batch(4, vec![CellWrite::cell(0, 0, num(1.0))]),
            (0, 0, 0),
        )
        .unwrap_err(),
        TxError::InvalidSheet
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_cell_batch(0, vec![CellWrite::cell(sheetcore::sync::MAX_ROWS, 0, num(1.0))]),
            (0, 0, 0),
        )
        .unwrap_err(),
        TxError::InvalidCellCoord
    );
    assert_eq!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_cell_batch(0, vec![CellWrite::style(0, sheetcore::sync::MAX_COLS, 1)]),
            (0, 0, 0),
        )
        .unwrap_err(),
        TxError::InvalidCellCoord
    );
    assert!(!wb.dirty);

    let mut undo = UndoStack::new();

    // Empty writes and pure no-op writes return Ok with empty delta; push_tx returns 0 and leaves dirty false.
    let empty_commit = WorkbookTx::execute(
        &mut wb,
        TxIntent::from(CellBatchIntent {
            sheet: 0,
            writes: Vec::new(),
        }),
        (0, 0, 0),
    )
    .unwrap();
    assert!(empty_commit.delta.is_empty());
    assert_eq!(empty_commit.effects.recalc, RecalcScope::None);
    assert!(empty_commit.effects.revision_deltas.is_empty());
    assert_eq!(undo.push_tx(empty_commit), 0);
    assert!(!wb.dirty);

    let noop_commit = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_cell_batch(0, vec![CellWrite::cell_and_style(0, 0, num(10.0), 2)]),
        (0, 0, 0),
    )
    .unwrap();
    assert!(noop_commit.delta.is_empty());
    assert_eq!(noop_commit.effects.recalc, RecalcScope::None);
    assert_eq!(undo.push_tx(noop_commit), 0);
    assert!(!wb.dirty);

    // Mixed batch: duplicate coordinate applied in order, formula, style-only, blank removal, and unchanged cell filtered out.
    let commit = WorkbookTx::execute_with_label(
        &mut wb,
        TxIntent::from_cell_batch(
            0,
            vec![
                CellWrite::cell(0, 0, num(10.0)), // unchanged -> filtered
                CellWrite::cell(1, 0, num(20.0)),
                CellWrite::cell_and_style(1, 0, num(25.0), 5), // overrides (1,0)
                CellWrite::cell(2, 0, fml("=A1+A2")),
                CellWrite::style(0, 0, 7),
                CellWrite::cell(3, 0, Cell::default()), // removes (3,0)
            ],
        ),
        Some("AI 写入".into()),
        (0, 1, 0),
    )
    .unwrap();
    assert_eq!(commit.label, "AI 写入");
    assert!(wb.dirty);
    assert_eq!(wb.coord_epoch(), epoch0);
    assert!(!commit.effects.structural && !commit.effects.coord_changed);
    assert!(!commit.effects.invalidate_deps);
    assert!(commit.effects.invalidate_render_caches);
    assert_eq!(commit.effects.revision_deltas.len(), 4);
    assert_eq!(
        commit.effects.recalc,
        RecalcScope::Cells(vec![(0, 0, 0), (0, 1, 0), (0, 2, 0), (0, 3, 0)])
    );
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(25.0));
    assert_eq!(wb.sheets[0].style_of(1, 0), 5);
    assert_eq!(wb.sheets[0].formula_of(2, 0), Some("=A1+A2"));
    assert_eq!(wb.sheets[0].style_of(0, 0), 7);
    assert!(wb.sheets[0].get(3, 0).is_none());

    let id1 = undo.push_tx(commit);
    assert!(id1 > 0);
    assert!(!undo.undo_is_structure());

    // Push a second non-overlapping CellBatch and verify prepare_group_undo commutes them cleanly.
    let c2 = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_cell_batch(0, vec![CellWrite::cell(10, 2, num(99.0))]),
        (0, 10, 2),
    )
    .unwrap();
    let id2 = undo.push_tx(c2);
    assert!(id2 > id1);
    assert!(undo.prepare_group_undo(&[id1]));
    assert_eq!(undo.top_undo_id(), Some(id1));

    // Guarded Undo: reject (1, 0) (simulating another peer's lock/edit), allow the other 3 cells.
    let undone = undo
        .undo_workbook(&mut wb, |_, r, c, _| !(r == 1 && c == 0))
        .unwrap();
    assert_eq!(undone.skipped, 1);
    assert_eq!(undone.touched.len(), 3);
    assert_eq!(undone.style_changes, vec![(0, 0, 0)]);
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(25.0)); // kept untouched
    assert_eq!(wb.sheets[0].style_of(1, 0), 5);
    assert_eq!(wb.sheets[0].style_of(0, 0), 2);
    assert!(wb.sheets[0].get(2, 0).is_none());
    assert_eq!(wb.sheets[0].value(3, 0), Value::Text("to-clear".into()));

    // Redo replays only the 3 non-excluded cells.
    let redone = undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(redone.skipped, 0);
    assert_eq!(redone.touched.len(), 3);
    assert_eq!(redone.style_changes, vec![(0, 0, 0)]);
    assert_eq!(wb.sheets[0].style_of(0, 0), 7);
    assert_eq!(wb.sheets[0].formula_of(2, 0), Some("=A1+A2"));
    assert!(wb.sheets[0].get(3, 0).is_none());
    assert_eq!(wb.coord_epoch(), epoch0);
}

#[test]
fn converged_entry_tx_meta_chart_pivot_and_from_structure_roundtrip() {
    use sheetcore::pivot::Agg;
    use sheetcore::pivot_engine::PivotTableModel;
    use sheetcore::pivot_multi::{PivotSpec2, ShowAs, ValueField};
    use sheetcore::sync::SheetMetaKind;

    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Main".into();
    wb.sheets[0].set(0, 0, txt("Hello"));
    wb.sheets[0].set(0, 1, txt("KeepMe"));
    let mut undo = UndoStack::new();
    let epoch0 = wb.coord_epoch();

    // 1. Empty MetaEdit via TxCommit::from_meta is not pushed.
    let empty_meta = TxCommit::from_meta("noop", (0, 0, 0), MetaEdit::default());
    assert!(empty_meta.delta.is_empty());
    assert_eq!(undo.push_tx(empty_meta), 0);

    // 2. Converged MetaEdit: col widths + filter + thread + rich text + hyperlink -> Entry::Tx.
    let thread_before: Option<ThreadedComment> = None;
    let thread_after = Some(ThreadedComment {
        r: 2,
        c: 0,
        resolved: false,
        entries: vec![ThreadEntry {
            id: "e1".into(),
            author: "Alice".into(),
            timestamp: "2026-11-27T00:00:00Z".into(),
            text: "initial".into(),
        }],
    });
    let summary_after = thread_after.as_ref().map(ThreadedComment::summary_note);
    wb.sheets[0].cols.set(0, 140.0);
    wb.sheets[0].filter = Some(Filter::new(0, 0, 5, 2));
    wb.sheets[0]
        .threads
        .insert(CellRef::new(2, 0), thread_after.clone().unwrap());
    wb.sheets[0]
        .notes
        .insert(CellRef::new(2, 0), summary_after.clone().unwrap());
    let bold_runs = vec![RichRun {
        bold: true,
        ..RichRun::plain("Hello")
    }];
    wb.sheets[0].rich.insert((0, 0), bold_runs.clone());
    wb.sheets[0].rich.insert((0, 1), vec![RichRun::plain("KeepMe")]);
    let link = Hyperlink {
        target: "https://example.com".into(),
        tooltip: "Example".into(),
        ..Default::default()
    };
    wb.sheets[0].hyperlinks.insert((1, 0), link.clone());

    let meta_edit = MetaEdit {
        sheet: 0,
        col_widths: vec![(0, None, Some(140.0))],
        notes: vec![((2, 0), None, summary_after)],
        threads: vec![((2, 0), thread_before, thread_after.clone())],
        filter: Some((
            None,
            Some(Filter::new(0, 0, 5, 2)),
            Default::default(),
            Default::default(),
        )),
        rich: vec![
            ((0, 0), txt("Hello"), txt("Hello"), None, Some(bold_runs.clone())),
            (
                (0, 1),
                txt("KeepMe"),
                txt("KeepMe"),
                None,
                Some(vec![RichRun::plain("KeepMe")]),
            ),
        ],
        hyperlinks: vec![((1, 0), None, Some(link))],
        ..Default::default()
    };
    let meta_commit = TxCommit::from_meta("元数据批量更新", (0, 0, 0), meta_edit);
    assert_eq!(meta_commit.effects.recalc, RecalcScope::Full);
    assert!(undo.push_tx(meta_commit) > 0);
    assert!(!undo.undo_is_structure());
    // next_thread_sync_error inspects TxDelta::Meta threads!
    assert!(undo.next_thread_sync_error(false, false).is_some());
    assert!(undo.next_thread_sync_error(false, true).is_none());

    // Simulate a conflicting reply on thread (2, 0) where baseline mismatches (replaced by unrelated thread)
    // AND guard blocks rich cell (0, 1).
    wb.sheets[0].threads.insert(
        CellRef::new(2, 0),
        ThreadedComment {
            r: 2,
            c: 0,
            resolved: false,
            entries: vec![ThreadEntry {
                id: "other-peer".into(),
                author: "Bob".into(),
                timestamp: "2026-11-27T01:00:00Z".into(),
                text: "conflicting".into(),
            }],
        },
    );
    let undone_meta = undo
        .undo_workbook(&mut wb, |_, r, c, _| !(r == 0 && c == 1))
        .unwrap();
    assert!(!undone_meta.structural && !undone_meta.coord_changed);
    assert_eq!(undone_meta.skipped, 2); // 1 rich skipped + 1 thread conflict
    assert_eq!(undone_meta.touched, vec![(0, 0, 0)]);
    assert!(undo.take_needs_recalc()); // filter triggered recalc
    let syncs = undo.take_meta_sync();
    assert!(syncs.iter().any(|(_, k)| matches!(k, SheetMetaKind::ColWidths(_))));
    assert!(syncs.iter().any(|(_, k)| matches!(k, SheetMetaKind::AutoFilter { range: None })));
    assert!(syncs.iter().any(|(_, k)| matches!(k, SheetMetaKind::Hyperlink { target: None, .. })));
    // Thread conflict was NOT broadcast and Bob's thread stayed intact.
    assert!(!syncs.iter().any(|(_, k)| matches!(k, SheetMetaKind::ThreadChange { .. })));
    assert_eq!(
        wb.sheets[0].threads.get(&CellRef::new(2, 0)).unwrap().entries[0].id,
        "other-peer"
    );
    assert!(wb.sheets[0].filter.is_none());
    assert!(!wb.sheets[0].rich.contains_key(&(0, 0)));
    assert!(wb.sheets[0].rich.contains_key(&(0, 1)));
    assert!(wb.sheets[0].hyperlinks.is_empty());
    assert_eq!(wb.coord_epoch(), epoch0);

    // Redo meta restores non-conflicted items cleanly.
    let redone_meta = undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(redone_meta.skipped, 0);
    assert!(wb.sheets[0].filter.is_some());
    assert!(wb.sheets[0].rich.contains_key(&(0, 0)));
    assert!(wb.sheets[0].hyperlinks.contains_key(&(1, 0)));

    // 3. Converged Chart edit via TxCommit::from_chart.
    let charts_before = wb.sheets[0].charts.clone();
    let deleted_before = wb.sheets[0].deleted_charts.clone();
    wb.sheets[0].charts.push(Chart {
        kind: ChartKind::Column,
        title: "Revenue".into(),
        ..Default::default()
    });
    wb.sheets[0]
        .deleted_charts
        .push(("xl/drawings/d1.xml".into(), "xl/charts/c1.xml".into()));
    let chart_edit = ChartUndoEdit {
        sheet: 0,
        charts_before,
        charts_after: wb.sheets[0].charts.clone(),
        deleted_before,
        deleted_after: wb.sheets[0].deleted_charts.clone(),
    };
    let chart_commit = TxCommit::from_chart("插入柱形图", (0, 0, 0), chart_edit);
    assert!(undo.push_tx(chart_commit) > 0);
    assert!(undo.undo_is_structure());
    let u_chart = undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert!(!u_chart.structural && !u_chart.coord_changed);
    assert!(wb.sheets[0].charts.is_empty());
    assert!(wb.sheets[0].deleted_charts.is_empty());
    assert!(wb.sheets[0].charts_dirty);
    assert_eq!(wb.coord_epoch(), epoch0);
    let r_chart = undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert!(!r_chart.structural && !r_chart.coord_changed);
    assert_eq!(wb.sheets[0].charts.len(), 1);
    assert_eq!(wb.sheets[0].charts[0].title, "Revenue");
    assert_eq!(wb.coord_epoch(), epoch0);

    // 4. Converged Pivot edit: (a) in-place does NOT bump coord_epoch; (b) inserted_sheet DOES bump coord_epoch.
    let spec = PivotSpec2 {
        source: (0, 0, 5, 1),
        rows: Vec::new(),
        cols: Vec::new(),
        values: vec![ValueField {
            col: 1,
            agg: Agg::Sum,
            label: None,
            show_as: ShowAs::Normal,
            base: None,
        }],
        filters: Vec::new(),
        layout: Default::default(),
    };
    let pivot_model = PivotTableModel::new(
        "Pivot1".into(),
        "Main".into(),
        Some(wb.sheets[0].identity),
        1,
        (0, 0),
        spec,
    );
    let pivots_before = wb.pivots.clone();
    let mut pivot_sh = Sheet::new("透视表1");
    pivot_sh.set(0, 0, txt("汇总"));
    wb.sheets.push(pivot_sh.clone());
    wb.pivots.push(pivot_model);
    let pivot_insert_edit = PivotUndoEdit {
        target_sheet: 1,
        inserted_sheet: Some((1, pivot_sh)),
        deltas: Vec::new(),
        col_widths: Vec::new(),
        pivots_before,
        pivots_after: wb.pivots.clone(),
        removed_before: Vec::new(),
        removed_after: vec!["xl/pivotTables/p1.xml".into()],
    };
    let p_commit = TxCommit::from_pivot("创建透视表", (0, 0, 0), pivot_insert_edit);
    assert!(p_commit.effects.structural && p_commit.effects.coord_changed);
    assert!(undo.push_tx(p_commit) > 0);
    assert!(undo.undo_is_structure());

    let e_before_pivot_undo = wb.coord_epoch();
    let u_pivot = undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert!(u_pivot.structural && u_pivot.coord_changed);
    assert_eq!(wb.sheets.len(), 1);
    assert!(wb.pivots.is_empty());
    assert!(wb.coord_epoch() > e_before_pivot_undo);

    let e_after_pivot_undo = wb.coord_epoch();
    let r_pivot = undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert!(r_pivot.structural && r_pivot.coord_changed);
    assert_eq!(wb.sheets.len(), 2);
    assert_eq!(wb.pivots.len(), 1);
    assert!(wb.coord_epoch() > e_after_pivot_undo);

    // In-place pivot refresh (inserted_sheet: None) preserves coord_epoch.
    let e_inplace = wb.coord_epoch();
    let d_before = sheetcore::undo::capture(&wb.sheets[1], 1, &[(0, 0)]);
    wb.sheets[1].set(0, 0, num(123.0));
    let p_deltas = sheetcore::undo::finish(d_before, &wb.sheets[1]);
    let inplace_edit = PivotUndoEdit {
        target_sheet: 1,
        inserted_sheet: None,
        deltas: p_deltas,
        col_widths: vec![(0, None, Some(120.0))],
        pivots_before: wb.pivots.clone(),
        pivots_after: wb.pivots.clone(),
        removed_before: wb.pivots_removed_parts.clone(),
        removed_after: wb.pivots_removed_parts.clone(),
    };
    let inplace_commit = TxCommit::from_pivot("刷新透视表", (1, 0, 0), inplace_edit);
    assert!(!inplace_commit.effects.structural && !inplace_commit.effects.coord_changed);
    assert!(undo.push_tx(inplace_commit) > 0);
    let u_inplace = undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert!(!u_inplace.structural && !u_inplace.coord_changed);
    assert_eq!(wb.sheets[1].value(0, 0), Value::Text("汇总".into()));
    assert_eq!(wb.coord_epoch(), e_inplace);
    let r_inplace = undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert!(!r_inplace.structural && !r_inplace.coord_changed);
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(123.0));
    assert_eq!(wb.coord_epoch(), e_inplace);

    // 5. TxCommit::from_structure + with_effects helper works cleanly via push_tx.
    let noop_struct = Structural::InsertRows { at: 0, n: 1 };
    let snap_before = sheetcore::undo::snapshot_structure(&wb, 0, &noop_struct);
    sheetcore::structural::apply_structural_wb(&mut wb, 0, &noop_struct);
    let snap_after = snap_before.capture_after(&wb);
    let s_commit = TxCommit::from_structure(
        "自定义结构",
        (0, 0, 0),
        snap_before,
        snap_after,
        TxEffects {
            recalc: RecalcScope::Full,
            structural: true,
            coord_changed: true,
            ..Default::default()
        },
    )
    .with_effects(TxEffects {
        recalc: RecalcScope::Full,
        invalidate_deps: true,
        invalidate_render_caches: true,
        structural: true,
        coord_changed: true,
        ..Default::default()
    });
    assert!(s_commit.effects.invalidate_deps);
    assert!(undo.push_tx(s_commit) > 0);
    let e_struct = wb.coord_epoch();
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert!(wb.coord_epoch() > e_struct);
}

#[test]
fn workbook_coord_epoch_advances_monotonically_across_all_coord_and_sheet_ops() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S1".into();
    for r in 0..8u32 {
        wb.sheets[0].set(r, 0, num((r + 1) as f64));
        wb.sheets[0].set(r, 1, num((r + 1) as f64 * 10.0));
    }
    assert_eq!(wb.coord_epoch(), 0);
    let mut last_epoch = wb.coord_epoch();
    let assert_bumped = |wb: &Workbook, last: &mut u64, ctx: &str| {
        let now = wb.coord_epoch();
        assert!(now > *last, "{ctx}: expected coord_epoch > {}, got {}", *last, now);
        *last = now;
    };

    let mut undo = UndoStack::new();

    // 1. Structural InsertRows + Undo + Redo
    let c = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_structural(0, &Structural::InsertRows { at: 1, n: 2 }),
        (0, 1, 0),
    )
    .unwrap();
    assert_bumped(&wb, &mut last_epoch, "Structural InsertRows");
    undo.push_tx(c);
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_bumped(&wb, &mut last_epoch, "Structural Undo");
    undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert_bumped(&wb, &mut last_epoch, "Structural Redo");

    // 2. Sparse CellShift + blocked Undo (must NOT bump) + clean Undo + Redo
    let cs = CellShift::new((2, 0, 2, 0), CellShiftDir::Vertical, true);
    let c = WorkbookTx::execute(&mut wb, TxIntent::from_cell_shift(0, &cs), (0, 2, 0)).unwrap();
    assert_bumped(&wb, &mut last_epoch, "Sparse CellShift");
    undo.push_tx(c);
    let saved_cell = wb.sheets[0].get(4, 0).map(|c| c.into_owned()).unwrap();
    wb.sheets[0].set(4, 0, num(777.0));
    let blocked = undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert!(blocked.skipped > 0);
    assert_eq!(wb.coord_epoch(), last_epoch, "blocked SparseShift undo must not bump coord_epoch");
    wb.sheets[0].set(4, 0, saved_cell);
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_bumped(&wb, &mut last_epoch, "SparseShift Undo");
    undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert_bumped(&wb, &mut last_epoch, "SparseShift Redo");

    // 3. AxisPermute + Undo + Redo
    let c = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_axis_move(0, Axis::Row, 0, 1, 4).unwrap(),
        (0, 0, 0),
    )
    .unwrap();
    assert_bumped(&wb, &mut last_epoch, "AxisPermute");
    undo.push_tx(c);
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_bumped(&wb, &mut last_epoch, "AxisPermute Undo");
    undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert_bumped(&wb, &mut last_epoch, "AxisPermute Redo");

    // 4. MultiInsert + Undo + Redo
    let c = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_multi_insert(0, Axis::Row, vec![1, 3]),
        (0, 1, 0),
    )
    .unwrap();
    assert_bumped(&wb, &mut last_epoch, "MultiInsert");
    undo.push_tx(c);
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_bumped(&wb, &mut last_epoch, "MultiInsert Undo");
    undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert_bumped(&wb, &mut last_epoch, "MultiInsert Redo");

    // 5. Multi-interval delete_row_intervals_wb
    sheetcore::structural::delete_row_intervals_wb(&mut wb, 0, &[(0, 0), (2, 2)]);
    assert_bumped(&wb, &mut last_epoch, "delete_row_intervals_wb");

    // 6. All 5 SheetTab intents + Undo + Redo
    let tabs = vec![
        SheetTabIntent::Append {
            sheet: Sheet::new("S2"),
        },
        SheetTabIntent::Insert {
            index: 1,
            sheet: Sheet::new("Mid"),
        },
        SheetTabIntent::Rename {
            index: 2,
            name: "Renamed".into(),
        },
        SheetTabIntent::Move { from: 2, to: 0 },
        SheetTabIntent::Delete { index: 0 },
    ];
    for tab in tabs {
        let c = WorkbookTx::execute(&mut wb, TxIntent::from_sheet_tab(tab), (0, 0, 0)).unwrap();
        assert_bumped(&wb, &mut last_epoch, "SheetTab execute");
        undo.push_tx(c);
        undo.undo_workbook(&mut wb, allow_all).unwrap();
        assert_bumped(&wb, &mut last_epoch, "SheetTab undo");
        undo.redo_workbook(&mut wb, allow_all).unwrap();
        assert_bumped(&wb, &mut last_epoch, "SheetTab redo");
    }

    // 7. CellShiftPatch + CellShiftOverlay
    let patch_op = CellShift::new((0, 0, 0, 0), CellShiftDir::Vertical, true);
    let patch = CellShiftPatch::prepare(&wb, 0, patch_op).unwrap();
    let mut replica = wb.clone();
    let mut rep_epoch = replica.coord_epoch();
    assert!(patch.apply(&mut wb, true));
    assert_bumped(&wb, &mut last_epoch, "CellShiftPatch forward");
    assert!(patch.apply(&mut wb, false));
    assert_bumped(&wb, &mut last_epoch, "CellShiftPatch backward");

    let overlay = TxOverlay::capture(&wb);
    assert!(overlay.apply_to(&mut replica));
    assert_bumped(&replica, &mut rep_epoch, "CellShiftOverlay apply_to");

    // 8. Non-coord mutations (CellBatch, DefinedNames, failed preflight) do NOT bump coord_epoch.
    let c_batch = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_cell_batch(0, vec![CellWrite::cell(0, 0, num(1234.0))]),
        (0, 0, 0),
    )
    .unwrap();
    assert_eq!(wb.coord_epoch(), last_epoch);
    undo.push_tx(c_batch);
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(wb.coord_epoch(), last_epoch);

    let c_name = WorkbookTx::execute(
        &mut wb,
        TxIntent::from_defined_names(DefinedNameIntent::Define {
            name: "Tax".into(),
            scope: None,
            formula: "0.1".into(),
        }),
        (0, 0, 0),
    )
    .unwrap();
    assert_eq!(wb.coord_epoch(), last_epoch);
    undo.push_tx(c_name);
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(wb.coord_epoch(), last_epoch);

    assert!(
        WorkbookTx::execute(
            &mut wb,
            TxIntent::from_structural(99, &Structural::InsertRows { at: 0, n: 1 }),
            (0, 0, 0),
        )
        .is_err()
    );
    assert_eq!(wb.coord_epoch(), last_epoch);
}
