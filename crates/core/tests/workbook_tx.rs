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
    RecalcScope, SheetTabIntent, TxDelta, TxError, TxIntent, TxOverlay, TxPatch, WorkbookTx,
};
use sheetcore::undo::UndoStack;

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

    wb.sheets[0].charts.push(Chart {
        kind: ChartKind::Column,
        series: vec![Series {
            val_ref: "S1!$A$2:$A$4".into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    assert_eq!(
        WorkbookTx::execute(&mut wb, intent.clone(), (0, 1, 0)).unwrap_err(),
        TxError::Conflict(CellShiftConflict::Chart)
    );
    wb.sheets[0].charts.clear();

    wb.sheets[0].sparklines.push(Sparkline {
        r: 2,
        c: 0,
        source_sheet: None,
        rect: (0, 1, 0, 3),
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
