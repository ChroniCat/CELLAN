use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use sheetcore::{
    Cell, Sheet, Value, Workbook,
    cell_shift_patch::{CellShiftOverlay, CellShiftPatch},
    cells::Shift,
    charts::{Chart, ChartKind, Series},
    external::{Kind, Source},
    participant::{CellShiftMode, CoordParticipant},
    pivot::{Agg, GroupBy, PivotResult, PivotSpec},
    pivot_engine::PivotTableModel,
    pivot_multi::{AxisField, PivotLayout, PivotSpec2, ShowAs, ValueField},
    refs::Axis,
    sparkline::{EmptyAs, SparkKind, Sparkline},
    structural::{CellShift, CellShiftConflict},
    style::CellStyle,
    sync::{
        BaselineBook, CellShiftPhase, LockOutcome, LockTable, MAX_COLS, MAX_ROWS, Msg, Op, Peer,
        ValueWire, permute_axis_coord, permute_axis_merge_rect, permute_cell_map,
        permute_coord_set,
    },
    transform::CoordTransform,
    tx::{
        AxisPermutePatch, DefinedNameIntent, DefinedNamesPatch, RoomTxPatch, TxIntent, TxPatch,
        WorkbookTx,
    },
    undo::UndoStack,
};

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

fn fml(f: &str) -> Cell {
    Cell {
        value: Value::Empty,
        formula: Some(f.into()),
    }
}

fn allow_all(_: usize, _: u32, _: u32, _: &Cell) -> bool {
    true
}

// ---------------------------------------------------------------------------
// Improvement #4: Unified CoordParticipant subsystem table & read-only accessors
// ---------------------------------------------------------------------------

#[test]
fn coord_participant_contract_completeness_and_model_accessors() {
    assert_eq!(CoordParticipant::ALL.len(), 16);
    let unique: HashSet<_> = CoordParticipant::ALL.iter().copied().collect();
    assert_eq!(unique.len(), 16);

    for p in CoordParticipant::ALL {
        let mode = p.cell_shift_mode();
        assert_eq!(
            p.participates_in_cell_shift_patch(),
            mode == CellShiftMode::Transform,
            "patch participation must match CellShiftMode::Transform for {p:?}"
        );
        assert_eq!(
            p.tracks_cell_shift_metadata(),
            mode == CellShiftMode::Transform && p != CoordParticipant::CellsAndStyles,
            "metadata tracking mismatch for {p:?}"
        );
    }

    // Charts and Sparklines are upgraded to CellShiftMode::Transform.
    assert_eq!(CoordParticipant::Charts.cell_shift_mode(), CellShiftMode::Transform);
    assert_eq!(CoordParticipant::Sparklines.cell_shift_mode(), CellShiftMode::Transform);
    assert!(CoordParticipant::Charts.participates_in_workbook_refs());
    assert!(CoordParticipant::Sparklines.participates_in_workbook_refs());
    assert!(CoordParticipant::Charts.tracks_cell_shift_metadata());
    assert!(CoordParticipant::Sparklines.tracks_cell_shift_metadata());

    // Guarded subsystems remain RejectOnRayOverlap; layout/page remains AxisOnly.
    for guarded in [
        CoordParticipant::Tables,
        CoordParticipant::Pivots,
        CoordParticipant::ArraysAndSpills,
        CoordParticipant::AutoFilter,
        CoordParticipant::ScenariosAndProtectedRanges,
    ] {
        assert_eq!(guarded.cell_shift_mode(), CellShiftMode::RejectOnRayOverlap);
        assert!(!guarded.participates_in_cell_shift_patch());
    }
    assert_eq!(CoordParticipant::LayoutAndPage.cell_shift_mode(), CellShiftMode::AxisOnly);

    // Read-only accessors on Workbook & Sheet reflect live state cleanly.
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Finance".into();
    wb.names.push(("Tax".into(), None, "0.13".into()));
    wb.sheets[0].merges.push((0, 0, 1, 1));
    wb.sheets[0].charts.push(Chart::default());
    wb.sheets[0].sparklines.push(Sparkline::default());

    assert_eq!(wb.sheet_count(), 1);
    assert!(wb.sheet_opt(0).is_some());
    assert!(wb.sheet_opt(1).is_none());
    assert_eq!(wb.defined_names().len(), 1);
    assert_eq!(wb.defined_names()[0].0, "Tax");
    assert!(wb.external_sources().is_empty());
    assert!(wb.pivot_tables().is_empty());

    let sh = wb.sheet_opt(0).unwrap();
    assert_eq!(sh.name(), "Finance");
    assert_eq!(sh.identity(), wb.sheets[0].identity);
    assert_eq!(sh.charts().len(), 1);
    assert_eq!(sh.sparklines().len(), 1);
    assert_eq!(sh.merges(), &[(0, 0, 1, 1)]);
    assert!(sh.tables().is_empty());

    assert!(CoordParticipant::any_cell_shift_metadata(&wb, 0));
    assert!(!CoordParticipant::any_cell_shift_metadata(&wb, 99));

    // Direct per-participant preflight via CoordTransform.
    let tx = CoordTransform::from_cell_shift(
        0,
        &CellShift::new((2, 0, 2, 0), Shift::Vertical, true),
    );
    assert_eq!(CoordParticipant::Charts.preflight(&wb, &tx), Ok(()));
    assert_eq!(CoordParticipant::Sparklines.preflight(&wb, &tx), Ok(()));
    assert_eq!(CoordParticipant::preflight_all(&wb, &tx), Ok(()));
}

// ---------------------------------------------------------------------------
// Improvement #5: CellShift upgraded for Chart & Sparkline (local, undo, patch, overlay)
// ---------------------------------------------------------------------------

#[test]
fn cell_shift_chart_refs_transform_local_undo_patch_overlay_and_cas_guards() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    // Intentionally leave target sheet with ZERO cells to prove chart-only CellShift
    // promotes to StructureSnapshot and replays cleanly!
    wb.sheets[0].charts.push(Chart {
        kind: ChartKind::Bubble,
        part: "xl/charts/chart1.xml".into(),
        anchor: (1, 5, 10, 12),
        source_range: "Data!$B$2:$B$5".into(),
        series: vec![Series {
            name_ref: "Data!$B$1".into(),
            cat_ref: "$B$2:$B$5".into(),
            val_ref: "Data!$B$2:$B$5".into(),
            size_ref: "Data!$B$3:$B$5".into(),
            ..Series::default()
        }],
        ..Chart::default()
    });
    let mut report = Sheet::new("Report");
    report.charts.push(Chart {
        kind: ChartKind::Line,
        part: "xl/charts/chart2.xml".into(),
        anchor: (2, 1, 12, 8),
        source_range: "Data!$B$2:$B$5".into(),
        series: vec![
            Series {
                name_ref: "Data!$B$1".into(),
                val_ref: "Data!$B$2:$B$5".into(),
                ..Series::default()
            },
            // Multi-column range straddling col B stays untouched under vertical col-B shift (Excel parity).
            Series {
                val_ref: "Data!$A$2:$C$5".into(),
                ..Series::default()
            },
        ],
        ..Chart::default()
    });
    wb.sheets.push(report);

    // 1. Local WorkbookTx execute + Undo + Redo on chart-only workbook.
    let mut local_wb = wb.clone();
    let mut undo = UndoStack::new();
    let ins_op = CellShift::new((1, 1, 2, 1), Shift::Vertical, true); // insert 2 rows at B2:B3
    let commit = WorkbookTx::execute(&mut local_wb, TxIntent::from_cell_shift(0, &ins_op), (0, 1, 1))
        .expect("chart CellShift must succeed");
    assert!(undo.push_tx(commit) > 0);
    assert!(undo.undo_is_structure(), "chart-only CellShift must promote to StructureSnapshot");

    let t_ch = &local_wb.sheets[0].charts[0];
    assert_eq!(t_ch.anchor, (1, 5, 10, 12), "floating anchor must not shift on partial CellShift");
    assert_eq!(t_ch.source_range, "Data!$B$4:$B$7");
    assert_eq!(t_ch.series[0].name_ref, "Data!$B$1");
    assert_eq!(t_ch.series[0].cat_ref, "$B$4:$B$7");
    assert_eq!(t_ch.series[0].val_ref, "Data!$B$4:$B$7");
    assert_eq!(t_ch.series[0].size_ref, "Data!$B$5:$B$7");
    assert_eq!(t_ch.ref_fixes.len(), 3);

    let f_ch = &local_wb.sheets[1].charts[0];
    assert_eq!(f_ch.anchor, (2, 1, 12, 8));
    assert_eq!(f_ch.source_range, "Data!$B$4:$B$7");
    assert_eq!(f_ch.series[0].val_ref, "Data!$B$4:$B$7");
    assert_eq!(f_ch.series[1].val_ref, "Data!$A$2:$C$5");

    // Undo restores exact refs on BOTH target and foreign sheets!
    undo.undo_workbook(&mut local_wb, allow_all).unwrap();
    assert_eq!(local_wb.sheets[0].charts[0].source_range, "Data!$B$2:$B$5");
    assert_eq!(local_wb.sheets[0].charts[0].series[0].cat_ref, "$B$2:$B$5");
    assert_eq!(local_wb.sheets[0].charts[0].series[0].size_ref, "Data!$B$3:$B$5");
    assert_eq!(local_wb.sheets[1].charts[0].source_range, "Data!$B$2:$B$5");
    assert_eq!(local_wb.sheets[1].charts[0].series[0].val_ref, "Data!$B$2:$B$5");

    // Redo re-applies cleanly.
    undo.redo_workbook(&mut local_wb, allow_all).unwrap();
    assert_eq!(local_wb.sheets[0].charts[0].series[0].val_ref, "Data!$B$4:$B$7");
    assert_eq!(local_wb.sheets[1].charts[0].series[0].val_ref, "Data!$B$4:$B$7");

    // 2. Delete wholly covering chart ref turns ref into #REF!, and Undo restores original ref!
    let del_op = CellShift::new((3, 1, 6, 1), Shift::Vertical, false); // delete B4:B7
    let del_commit =
        WorkbookTx::execute(&mut local_wb, TxIntent::from_cell_shift(0, &del_op), (0, 3, 1))
            .unwrap();
    assert_eq!(local_wb.sheets[0].charts[0].series[0].val_ref, "Data!#REF!");
    assert_eq!(local_wb.sheets[1].charts[0].series[0].val_ref, "Data!#REF!");
    assert!(undo.push_tx(del_commit) > 0);
    undo.undo_workbook(&mut local_wb, allow_all).unwrap();
    assert_eq!(local_wb.sheets[0].charts[0].series[0].val_ref, "Data!$B$4:$B$7");
    assert_eq!(local_wb.sheets[1].charts[0].series[0].val_ref, "Data!$B$4:$B$7");

    // 3. Room CellShiftPatch + canonical_forward (guest without base-file charts) + Overlay + CAS!
    let mut host = wb.clone();
    let baseline = CellShiftOverlay::capture(&host);
    let patch = CellShiftPatch::prepare(&host, 0, ins_op).unwrap();
    assert_eq!(patch.charts.len(), 2);
    assert!(patch.wire_valid());
    let wire = serde_json::to_string(&patch).unwrap();
    let patch: CellShiftPatch = serde_json::from_str(&wire).unwrap();

    // Guest joining without base-file charts prepares a request without `charts`,
    // and host derives canonical chart changes automatically!
    let mut guest_no_charts = host.clone();
    guest_no_charts.sheets[0].charts.clear();
    guest_no_charts.sheets[1].charts.clear();
    let guest_req = CellShiftPatch::prepare(&guest_no_charts, 0, ins_op).unwrap();
    assert!(guest_req.charts.is_empty());
    assert_eq!(guest_req.canonical_forward(&host).unwrap(), patch);

    // Apply forward on host and guest (both with and without charts).
    let mut replica_with_charts = host.clone();
    assert!(patch.apply(&mut host, true));
    assert!(patch.apply_committed(&mut guest_no_charts, true));
    assert!(patch.matches_replica(&guest_no_charts, false));
    assert!(!patch.matches(&guest_no_charts, false));

    // Overlay roundtrip updates a joining guest with base-file charts cleanly.
    let overlay = CellShiftOverlay::capture(&host);
    let overlay_wire = serde_json::to_string(&overlay).unwrap();
    let overlay: CellShiftOverlay = serde_json::from_str(&overlay_wire).unwrap();
    assert!(overlay.wire_valid());
    assert!(overlay.apply_to(&mut replica_with_charts));
    assert_eq!(replica_with_charts.sheets[0].charts[0].series[0].val_ref, "Data!$B$4:$B$7");
    assert_eq!(replica_with_charts.sheets[1].charts[0].series[0].val_ref, "Data!$B$4:$B$7");

    // Late-added conflicting chart (referencing moving band) MUST block undo!
    host.sheets[1].charts.push(Chart {
        part: "xl/charts/chart3.xml".into(),
        series: vec![Series {
            val_ref: "Data!$B$10:$B$12".into(),
            ..Series::default()
        }],
        ..Chart::default()
    });
    assert!(!patch.apply(&mut host, false));
    host.sheets[1].charts.pop();

    // Late-added unrelated chart (col E outside col B band) MUST allow undo and be preserved!
    host.sheets[1].charts.push(Chart {
        part: "xl/charts/chart_unrelated.xml".into(),
        series: vec![Series {
            val_ref: "Data!$E$2:$E$5".into(),
            ..Series::default()
        }],
        ..Chart::default()
    });
    assert!(patch.apply(&mut host, false));
    assert_eq!(host.sheets[0].charts[0].series[0].val_ref, "Data!$B$2:$B$5");
    assert_eq!(host.sheets[1].charts[0].series[0].val_ref, "Data!$B$2:$B$5");
    assert_eq!(host.sheets[1].charts.len(), 2);
    assert_eq!(host.sheets[1].charts[1].series[0].val_ref, "Data!$E$2:$E$5");

    // Remove unrelated chart and verify overlay matches baseline.
    host.sheets[1].charts.pop();
    assert_eq!(CellShiftOverlay::capture(&host), baseline);
}

#[test]
fn cell_shift_sparklines_anchor_and_data_rect_parity_and_preflight_guards() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    // Sparkline-only workbook (ZERO cells!) testing:
    // (a) anchor + data rect BOTH in moving band (must shift ONCE, never double-shift!)
    wb.sheets[0].sparklines.push(Sparkline {
        r: 2,
        c: 1,
        source_sheet: None,
        rect: (4, 1, 6, 1),
        kind: SparkKind::Column,
        empty: EmptyAs::Zero,
        ..Default::default()
    });
    // (b) anchor outside band, data rect wholly in band (shrink/expand test)
    wb.sheets[0].sparklines.push(Sparkline {
        r: 0,
        c: 5,
        source_sheet: None,
        rect: (1, 1, 5, 1),
        ..Default::default()
    });
    // (c) cross-sheet sparkline matching "Data" case-insensitively ("DATA")
    let mut dash = Sheet::new("Dash");
    dash.sparklines.push(Sparkline {
        r: 1,
        c: 1,
        source_sheet: Some("DATA".into()),
        rect: (4, 1, 6, 1),
        ..Default::default()
    });
    wb.sheets.push(dash);

    // 1. Insert 2 rows at B3:B4 (rows 2..=3, col 1):
    //    - (a) anchor (2,1) -> (4,1), rect (4,1,6,1) -> (6,1,8,1)
    //    - (b) anchor (0,5) unchanged, rect (1,1,5,1) expands -> (1,1,7,1)
    //    - (c) foreign anchor (1,1) unchanged, rect (4,1,6,1) -> (6,1,8,1)
    let ins_op = CellShift::new((2, 1, 3, 1), Shift::Vertical, true);
    let mut local_wb = wb.clone();
    let mut undo = UndoStack::new();
    let commit = WorkbookTx::execute(&mut local_wb, TxIntent::from_cell_shift(0, &ins_op), (0, 2, 1))
        .expect("sparkline CellShift must succeed");
    assert!(undo.push_tx(commit) > 0);
    assert!(undo.undo_is_structure(), "sparkline-only CellShift must promote to StructureSnapshot");

    assert_eq!((local_wb.sheets[0].sparklines[0].r, local_wb.sheets[0].sparklines[0].c), (4, 1));
    assert_eq!(local_wb.sheets[0].sparklines[0].rect, (6, 1, 8, 1));
    assert_eq!((local_wb.sheets[0].sparklines[1].r, local_wb.sheets[0].sparklines[1].c), (0, 5));
    assert_eq!(local_wb.sheets[0].sparklines[1].rect, (1, 1, 7, 1));
    assert_eq!((local_wb.sheets[1].sparklines[0].r, local_wb.sheets[1].sparklines[0].c), (1, 1));
    assert_eq!(local_wb.sheets[1].sparklines[0].rect, (6, 1, 8, 1));

    // Undo restores exact sparklines on both sheets.
    undo.undo_workbook(&mut local_wb, allow_all).unwrap();
    assert_eq!(local_wb.sheets[0].sparklines, wb.sheets[0].sparklines);
    assert_eq!(local_wb.sheets[1].sparklines, wb.sheets[1].sparklines);

    // 2. Room CellShiftPatch + Overlay + CAS guards for sparklines!
    let mut host = wb.clone();
    let baseline = CellShiftOverlay::capture(&host);
    let patch = CellShiftPatch::prepare(&host, 0, ins_op).unwrap();
    assert!(patch.wire_valid());
    let wire = serde_json::to_string(&patch).unwrap();
    let patch: CellShiftPatch = serde_json::from_str(&wire).unwrap();
    assert_eq!(patch.canonical_forward(&host).unwrap(), patch);
    assert!(patch.apply(&mut host, true));
    let mut actual_sp = host.sheets[0].sparklines.clone();
    actual_sp.sort_by_key(|sp| (sp.r, sp.c));
    let mut expected_sp = local_wb
        .sheets[0]
        .sparklines
        .iter()
        .cloned()
        .map(|mut sp| {
            if (sp.r, sp.c) == (2, 1) {
                sp.r = 4;
                sp.rect = (6, 1, 8, 1);
            } else {
                sp.rect = (1, 1, 7, 1);
            }
            sp
        })
        .collect::<Vec<_>>();
    expected_sp.sort_by_key(|sp| (sp.r, sp.c));
    assert_eq!(actual_sp, expected_sp);

    // Overlay roundtrip on a fresh guest matches host.
    let overlay = CellShiftOverlay::capture(&host);
    let mut guest = wb.clone();
    assert!(overlay.apply_to(&mut guest));
    assert_eq!(CellShiftOverlay::capture(&guest), overlay);

    // Late-added conflicting sparkline (anchor in ray) blocks undo.
    host.sheets[0].sparklines.push(Sparkline {
        r: 10,
        c: 1,
        source_sheet: None,
        rect: (10, 4, 10, 6),
        ..Default::default()
    });
    assert!(!patch.apply(&mut host, false));
    host.sheets[0].sparklines.pop();

    // Late-added conflicting foreign sparkline (data rect in ray) blocks undo.
    host.sheets[1].sparklines.push(Sparkline {
        r: 9,
        c: 9,
        source_sheet: Some("data".into()),
        rect: (8, 1, 9, 1),
        ..Default::default()
    });
    assert!(!patch.apply(&mut host, false));
    host.sheets[1].sparklines.pop();

    // Late-added unrelated sparkline (outside band) allows undo and is preserved!
    let unrelated = Sparkline {
        r: 9,
        c: 9,
        source_sheet: None,
        rect: (9, 4, 9, 6),
        ..Default::default()
    };
    host.sheets[0].sparklines.push(unrelated.clone());
    assert!(patch.apply(&mut host, false));
    assert!(host.sheets[0].sparklines.contains(&unrelated));
    host.sheets[0].sparklines.retain(|sp| *sp != unrelated);
    assert_eq!(CellShiftOverlay::capture(&host), baseline);

    // 3. Anchor delete drops sparkline cleanly (even if its own data rect straddles band) + Undo restores!
    let mut wb_drop = Workbook::new_blank();
    wb_drop.sheets[0].name = "Data".into();
    let dropped_sp = Sparkline {
        r: 2,
        c: 1,
        source_sheet: None,
        rect: (2, 0, 2, 2), // straddles col B, but anchor (2,1) itself is deleted!
        ..Default::default()
    };
    wb_drop.sheets[0].sparklines.push(dropped_sp.clone());
    let del_anchor = CellShift::new((2, 1, 2, 1), Shift::Vertical, false);
    let drop_patch = CellShiftPatch::prepare(&wb_drop, 0, del_anchor).unwrap();
    assert!(drop_patch.apply(&mut wb_drop, true));
    assert!(wb_drop.sheets[0].sparklines.is_empty());
    assert!(drop_patch.apply(&mut wb_drop, false));
    assert_eq!(wb_drop.sheets[0].sparklines, vec![dropped_sp]);

    // 4. Preflight rejections:
    //    (a) Cross-sheet band-straddling sparkline -> CellShiftConflict::Sparkline
    let mut wb_err = Workbook::new_blank();
    wb_err.sheets[0].name = "Data".into();
    let mut other = Sheet::new("Other");
    other.sparklines.push(Sparkline {
        r: 0,
        c: 0,
        source_sheet: Some("data".into()),
        rect: (2, 0, 2, 2),
        ..Default::default()
    });
    wb_err.sheets.push(other);
    assert_eq!(
        CellShift::new((1, 1, 1, 1), Shift::Vertical, true).preflight(&wb_err, 0),
        Err(CellShiftConflict::Sparkline)
    );
    //    (b) Wholly-deleted/emptied data rect (surviving anchor) -> CellShiftConflict::Sparkline
    wb_err.sheets[1].sparklines[0].rect = (2, 1, 2, 1);
    assert_eq!(
        CellShift::new((2, 1, 2, 1), Shift::Vertical, false).preflight(&wb_err, 0),
        Err(CellShiftConflict::Sparkline)
    );
    //    (c) Anchor insert overflow -> CellShiftConflict::Overflow
    wb_err.sheets[1].sparklines.clear();
    wb_err.sheets[0].sparklines.push(Sparkline {
        r: MAX_ROWS - 1,
        c: 1,
        source_sheet: None,
        rect: (0, 3, 0, 5),
        ..Default::default()
    });
    assert_eq!(
        CellShift::new((1, 1, 1, 1), Shift::Vertical, true).preflight(&wb_err, 0),
        Err(CellShiftConflict::Overflow)
    );
    //    (d) Wholly-in-band data rect insert overflow -> CellShiftConflict::Overflow
    wb_err.sheets[0].sparklines[0] = Sparkline {
        r: 0,
        c: 5,
        source_sheet: None,
        rect: (MAX_ROWS - 2, 1, MAX_ROWS - 1, 1),
        ..Default::default()
    };
    assert_eq!(
        CellShift::new((1, 1, 1, 1), Shift::Vertical, true).preflight(&wb_err, 0),
        Err(CellShiftConflict::Overflow)
    );

    // 5. Unnormalized (reversed) Sparkline.rect parity between preflight and apply:
    //    wholly-in-band reversed rect shifts cleanly; outside reversed rect stays untouched.
    let mut wb_rev = Workbook::new_blank();
    wb_rev.sheets[0].name = "Data".into();
    wb_rev.sheets[0].sparklines.push(Sparkline {
        r: 0,
        c: 5,
        source_sheet: None,
        rect: (6, 1, 4, 1), // reversed rows, wholly inside col B band
        ..Default::default()
    });
    wb_rev.sheets[0].sparklines.push(Sparkline {
        r: 0,
        c: 6,
        source_sheet: None,
        rect: (6, 4, 4, 4), // reversed rows, outside col B band
        ..Default::default()
    });
    let rev_op = CellShift::new((2, 1, 3, 1), Shift::Vertical, true);
    assert_eq!(rev_op.preflight(&wb_rev, 0), Ok(()));
    assert!(rev_op.apply(&mut wb_rev, 0));
    assert_eq!(wb_rev.sheets[0].sparklines[0].rect, (6, 1, 8, 1));
    assert_eq!(wb_rev.sheets[0].sparklines[1].rect, (6, 4, 4, 4));
}

#[test]
fn cell_shift_patch_and_overlay_preserve_legacy_json_wire_compatibility() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S1".into();
    wb.sheets[0].set(1, 0, num(5.0));
    let op = CellShift::new((1, 0, 1, 0), Shift::Vertical, true);
    let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    let mut patch_val = serde_json::to_value(&patch).unwrap();
    // Strip new optional fields to simulate an older peer's JSON payload.
    let obj = patch_val.as_object_mut().unwrap();
    obj.remove("charts");
    obj.remove("sparklines");
    let decoded_patch: CellShiftPatch = serde_json::from_value(patch_val).unwrap();
    assert!(decoded_patch.wire_valid());
    assert_eq!(decoded_patch, patch);

    let overlay = CellShiftOverlay::capture(&wb);
    let mut ov_val = serde_json::to_value(&overlay).unwrap();
    for sh in ov_val["sheets"].as_array_mut().unwrap() {
        let m = sh.as_object_mut().unwrap();
        m.remove("chart_refs");
        m.remove("sparklines");
    }
    let decoded_overlay: CellShiftOverlay = serde_json::from_value(ov_val).unwrap();
    assert!(decoded_overlay.wire_valid());
    assert_eq!(decoded_overlay, overlay);
}

// ---------------------------------------------------------------------------
// Improvement #3 Core: DefinedNamesPatch, AxisPermutePatch, RoomTxPatch, sync.rs
// ---------------------------------------------------------------------------

#[test]
fn defined_names_patch_lifecycle_private_exclusion_cas_and_wire_validation() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Sales".into();
    wb.sheets.push(Sheet::new("Summary"));
    // Private legacy import definition MUST be excluded from wire and preserved intact!
    wb.names.push((
        "_sc_src_local".into(),
        None,
        "\"C:\\secret\\local.csv\"".into(),
    ));

    // 1. Define global name + sheet-scoped name via TxPatch::prepare / RoomTxPatch::prepare.
    let define_intent = TxIntent::from_defined_names(DefinedNameIntent::Define {
        name: "Margin".into(),
        scope: Some(1),
        formula: "Sales!$B$1".into(),
    });
    let tx_patch = TxPatch::prepare(&wb, &define_intent).unwrap();
    assert_eq!(tx_patch.label(), "定义名称");
    assert_eq!(tx_patch.target_sheet(), 1);
    assert_eq!(tx_patch.referenced_sheets(), vec![1]);

    // Wire JSON MUST NOT leak `_sc_src_local` or its path!
    let tx_json = serde_json::to_string(&tx_patch).unwrap();
    assert!(!tx_json.contains("_sc_src_local"));
    assert!(!tx_json.contains("secret"));
    let tx_roundtrip: TxPatch = serde_json::from_str(&tx_json).unwrap();
    assert_eq!(tx_roundtrip, tx_patch);

    // Tagged RoomTxPatch roundtrip carries explicit `"kind":"DefinedNames"`.
    let room_patch = RoomTxPatch::from(tx_patch.clone());
    let room_json = serde_json::to_string(&room_patch).unwrap();
    assert!(room_json.contains("\"kind\":\"DefinedNames\""));
    let room_roundtrip: RoomTxPatch = serde_json::from_str(&room_json).unwrap();
    assert_eq!(room_roundtrip, room_patch);
    assert_eq!(room_roundtrip.cell_op_count(), 0);

    // Apply forward -> preserves `_sc_src_local`, adds `Margin`.
    let mut replica = wb.clone();
    replica.names.clear(); // guest lacks host's private `_sc_src_local`
    assert!(tx_patch.apply(&mut wb, true));
    assert!(tx_patch.apply(&mut replica, true));
    assert_eq!(wb.names.len(), 2);
    assert_eq!(wb.names[0].0, "_sc_src_local");
    assert_eq!(wb.names[1], ("Margin".into(), Some(1), "Sales!$B$1".into()));
    assert_eq!(replica.names, vec![("Margin".into(), Some(1), "Sales!$B$1".into())]);

    // CAS guard: concurrent edit blocks undo; sheet rename blocks can_install.
    wb.names.push(("Concurrent".into(), None, "1".into()));
    assert!(!tx_patch.matches(&wb, false));
    assert!(!tx_patch.apply(&mut wb, false));
    wb.names.pop();

    wb.sheets[1].name = "RenamedSummary".into();
    assert!(!tx_patch.can_install(&wb));
    wb.sheets[1].name = "Summary".into();
    assert!(tx_patch.can_install(&wb));

    // UndoStack integration via push_room_tx_patch + confirm_room_tx_patch.
    let mut undo = UndoStack::new();
    let patch_arc = Arc::new(tx_patch.clone());
    assert!(undo.push_room_tx_patch(42, patch_arc.clone(), &wb) > 0);
    let (id, peeked) = undo.peek_room_tx_patch(false).unwrap();
    assert_eq!(id, 42);
    assert_eq!(*peeked, *patch_arc);
    assert!(undo.peek_room_cell_shift(false).is_none());
    assert!(tx_patch.apply(&mut wb, false));
    assert!(undo.confirm_room_tx_patch(42, false));
    assert_eq!(undo.depth(), (0, 1));
    assert_eq!(wb.names.len(), 1);
    assert_eq!(wb.names[0].0, "_sc_src_local");

    // 2. Update, Remove, ReplaceAll + wire_valid rejection of bad payloads.
    wb.names.push(("Rate".into(), None, "0.05".into()));
    let upd = DefinedNamesPatch::prepare(
        &wb,
        &DefinedNameIntent::Update {
            index: 1,
            name: "Rate".into(),
            scope: None,
            formula: "0.08".into(),
        },
    )
    .unwrap();
    assert_eq!(upd.label(), "编辑名称");
    assert!(upd.apply(&mut wb, true));
    assert_eq!(wb.names[1].2, "0.08");

    let rem = DefinedNamesPatch::prepare(&wb, &DefinedNameIntent::Remove { index: 1 }).unwrap();
    assert_eq!(rem.label(), "删除名称");
    assert!(rem.apply(&mut wb, true));
    assert_eq!(wb.names.len(), 1); // `_sc_src_local` still preserved!

    let rep = DefinedNamesPatch::prepare(
        &wb,
        &DefinedNameIntent::ReplaceAll {
            names: vec![("A".into(), None, "1".into()), ("B".into(), Some(0), "2".into())],
        },
    )
    .unwrap();
    assert!(rep.apply(&mut wb, true));
    assert_eq!(wb.names.len(), 3);

    // Invalid wire payloads rejected cleanly.
    let mut bad_dup = rep.clone();
    bad_dup.after.push(("a".into(), None, "9".into())); // case-insensitive duplicate
    assert!(!bad_dup.wire_valid());

    let mut bad_priv = rep.clone();
    bad_priv.after.push(("_sc_src_x".into(), None, "1".into()));
    assert!(!bad_priv.wire_valid());

    let mut bad_scope = rep;
    bad_scope.sheet_names.clear(); // missing scope 0 in sheet_names table
    assert!(!bad_scope.wire_valid());
}

#[test]
fn axis_permute_patch_rows_and_cols_roundtrip_all_subsystems_and_undo_stack() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let data_id = wb.sheets[0].identity;
    for r in 0..6u32 {
        wb.sheets[0].set(r, 0, txt(&format!("R{r}")));
        wb.sheets[0].set(r, 1, num((r + 1) as f64 * 10.0));
    }
    let st = wb.styles.intern(CellStyle {
        bold: true,
        ..CellStyle::default_dark()
    });
    wb.sheets[0].set_style(1, 1, st);
    wb.sheets[0].charts.push(Chart {
        kind: ChartKind::Column,
        anchor: (1, 4, 2, 7),
        source_range: "Data!$A$2:$B$3".into(),
        series: vec![Series {
            val_ref: "Data!$B$2:$B$3".into(),
            ..Series::default()
        }],
        ..Chart::default()
    });
    wb.sheets[0].sparklines.push(Sparkline {
        r: 1,
        c: 3,
        source_sheet: None,
        rect: (1, 1, 2, 1),
        ..Default::default()
    });
    let mut summary = Sheet::new("Summary");
    summary.set(0, 0, fml("=SUM(Data!B2:B3)+Data!B5"));
    summary.sparklines.push(Sparkline {
        r: 0,
        c: 1,
        source_sheet: Some("data".into()),
        rect: (1, 1, 2, 1),
        ..Default::default()
    });
    wb.sheets.push(summary);
    wb.names.push(("Block".into(), None, "Data!$B$2:$B$3".into()));
    wb.sources.push(Source {
        name: "s1".into(),
        kind: Kind::Internal {
            sheet: "Data".into(),
            rect: (1, 0, 2, 1),
        },
        at: (0, 0),
        target_sheet: Some("Summary".into()),
        rows: 2,
        cols: 2,
        csv_opts: None,
    });
    let spec2 = PivotSpec2 {
        source: (1, 0, 2, 1),
        rows: vec![AxisField::plain(0)],
        cols: Vec::new(),
        values: vec![ValueField {
            col: 1,
            agg: Agg::Sum,
            label: None,
            show_as: ShowAs::Normal,
            base: None,
        }],
        filters: Vec::new(),
        layout: PivotLayout::default(),
    };
    let mut piv = PivotTableModel::new("P1".into(), "Data".into(), Some(data_id), 1, (5, 0), spec2);
    piv.rect = (5, 0, 6, 1);
    wb.pivots.push(piv);
    wb.pivots_pending.push((
        0,
        PivotSpec {
            source: (1, 0, 2, 1),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        },
        PivotResult::default(),
        (1, 8, 2, 9),
        "Pend".into(),
    ));

    // Move rows 1..=2 down to drop=6 (final at=4 -> rows 4..=5).
    let patch = AxisPermutePatch::from_drop(&wb, 0, Axis::Row, 1, 2, 6).unwrap();
    assert_eq!((patch.from, patch.n, patch.at), (1, 2, 4));
    assert_eq!(patch.affected_band(), (1, 6));
    assert_eq!(patch.label(), "移动行");
    assert_eq!(patch.inverse().effective_move(true), patch.effective_move(false));

    // TxPatch untagged & RoomTxPatch tagged roundtrip.
    let tx_intent = TxIntent::from_axis_move(0, Axis::Row, 1, 2, 6).unwrap();
    let tx_patch = TxPatch::prepare(&wb, &tx_intent).unwrap();
    assert_eq!(tx_patch, TxPatch::AxisPermute(patch.clone()));
    let tx_json = serde_json::to_string(&tx_patch).unwrap();
    assert_eq!(serde_json::from_str::<TxPatch>(&tx_json).unwrap(), tx_patch);

    let room_patch = RoomTxPatch::from(tx_patch.clone());
    let room_json = serde_json::to_string(&room_patch).unwrap();
    assert!(room_json.contains("\"kind\":\"AxisPermute\""));
    assert_eq!(serde_json::from_str::<RoomTxPatch>(&room_json).unwrap(), room_patch);

    let epoch0 = wb.coord_epoch();
    assert!(patch.apply(&mut wb, true));
    assert!(wb.coord_epoch() > epoch0);

    // Verify cells, styles, cross-sheet formula, chart, sparklines, names, sources, pivots!
    assert_eq!(wb.sheets[0].value(4, 0), Value::Text("R1".into()));
    assert_eq!(wb.sheets[0].value(5, 0), Value::Text("R2".into()));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Text("R3".into()));
    assert_eq!(wb.sheets[0].explicit_style_of(4, 1), Some(st));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=SUM(Data!B5:B6)+Data!B3"));
    assert_eq!(wb.sheets[0].charts[0].anchor, (4, 4, 5, 7));
    assert_eq!(wb.sheets[0].charts[0].series[0].val_ref, "Data!$B$5:$B$6");
    assert_eq!((wb.sheets[0].sparklines[0].r, wb.sheets[0].sparklines[0].c), (4, 3));
    assert_eq!(wb.sheets[0].sparklines[0].rect, (4, 1, 5, 1));
    assert_eq!(wb.sheets[1].sparklines[0].rect, (4, 1, 5, 1));
    assert_eq!(wb.names[0].2, "Data!$B$5:$B$6");
    assert!(matches!(&wb.sources[0].kind, Kind::Internal { rect: (4, 0, 5, 1), .. }));
    assert_eq!(wb.pivots[0].spec.source, (4, 0, 5, 1));
    assert_eq!(wb.pivots_pending[0].1.source, (4, 0, 5, 1));

    // UndoStack integration + inverse replay restores original workbook state!
    let mut undo = UndoStack::new();
    assert!(undo.push_room_tx_patch(7, Arc::new(tx_patch.clone()), &wb) > 0);
    assert!(patch.apply(&mut wb, false));
    assert!(undo.confirm_room_tx_patch(7, false));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Text("R1".into()));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Text("R2".into()));
    assert_eq!(wb.sheets[0].explicit_style_of(1, 1), Some(st));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=SUM(Data!B2:B3)+Data!B5"));
    assert_eq!(wb.sheets[0].charts[0].series[0].val_ref, "Data!$B$2:$B$3");
    assert_eq!(wb.sheets[0].sparklines[0].rect, (1, 1, 2, 1));
    assert_eq!(wb.names[0].2, "Data!$B$2:$B$3");
    assert_eq!(wb.pivots[0].spec.source, (1, 0, 2, 1));

    // Column move + sheet rename guard + wire_valid bounds checks.
    let col_patch = AxisPermutePatch::prepare(&wb, 0, Axis::Col, 0, 1, 1).unwrap();
    assert_eq!(col_patch.label(), "移动列");
    assert!(col_patch.apply(&mut wb, true));
    assert_eq!(wb.sheets[0].value(1, 1), Value::Text("R1".into()));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(20.0));
    assert!(col_patch.apply(&mut wb, false));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Text("R1".into()));

    wb.sheets[0].name = "Changed".into();
    assert!(!col_patch.can_install(&wb));
    wb.sheets[0].name = "Data".into();

    let mut bad = col_patch;
    bad.at = MAX_COLS;
    assert!(!bad.wire_valid());
    bad.at = bad.from;
    assert!(!bad.wire_valid());
    bad.n = 0;
    assert!(!bad.wire_valid());
}

#[test]
fn sync_room_tx_messages_peer_axis_permute_and_lock_table() {
    // 1. Wire roundtrip + is_within_limits for Msg::Hello/Welcome `room_tx` and all 5 Msg::RoomTx*.
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S1".into();
    let dn_patch = RoomTxPatch::DefinedNames(
        DefinedNamesPatch::prepare(
            &wb,
            &DefinedNameIntent::Define {
                name: "K".into(),
                scope: None,
                formula: "100".into(),
            },
        )
        .unwrap(),
    );
    let perm_patch =
        RoomTxPatch::AxisPermute(AxisPermutePatch::prepare(&wb, 0, Axis::Row, 2, 3, 10).unwrap());

    let frames = [
        Msg::Hello {
            site: 1,
            name: "Alice".into(),
            doc: "doc-1".into(),
            proof: "ok".into(),
            auth: String::new(),
            base_hash: String::new(),
            cell_shifts: true,
            sort_permutation: true,
            palette_styles: true,
            resume_room: String::new(),
            resume_watermark: 0,
            resume_struct_rev: 0,
            binary_v3: true,
            compact_v3: true,
            room_tx: true,
            full_tx_overlay: true,
        },
        Msg::Welcome {
            site: 1,
            doc: "doc-1".into(),
            peers: vec![],
            delta_sync: true,
            batch_ops: true,
            cell_shifts: true,
            sort_permutation: true,
            structural_ack: true,
            thread_changes: true,
            clock: 0,
            palette_styles: true,
            room_id: "r1".into(),
            watermark: 0,
            structure_revision: 0,
            binary_v3: true,
            compact_v3: true,
            acked_op_ts: 0,
            room_tx: true,
            full_tx_overlay: true,
        },
        Msg::RoomTxRequest {
            site: 2,
            id: 11,
            phase: CellShiftPhase::Forward,
            revision: 3,
            patch: Arc::new(dn_patch.clone()),
        },
        Msg::RoomTxCommitted {
            site: 2,
            id: 11,
            phase: CellShiftPhase::Undo,
            first_ts: 50,
            seq: 9,
            patch: Arc::new(perm_patch.clone()),
        },
        Msg::RoomTxRejected {
            id: 11,
            phase: CellShiftPhase::Forward,
            reason: "conflict".into(),
        },
        Msg::RoomTxAck { site: 2, seq: 9 },
        Msg::RoomTxReceipt {
            site: 2,
            id: 11,
            phase: CellShiftPhase::Redo,
            first_ts: 51,
            seq: 10,
            patch: Arc::new(dn_patch),
        },
    ];
    for frame in &frames {
        assert!(frame.is_within_limits(), "frame should pass limits: {frame:?}");
        let json = serde_json::to_string(frame).unwrap();
        let decoded: Msg = serde_json::from_str(&json).unwrap();
        assert!(decoded.is_within_limits());
        assert_eq!(serde_json::to_value(&decoded).unwrap(), serde_json::to_value(frame).unwrap());
    }

    // Invalid RoomTxRequest patch rejected by is_within_limits.
    let mut bad_perm = AxisPermutePatch::prepare(&wb, 0, Axis::Row, 2, 3, 10).unwrap();
    bad_perm.n = 0;
    let bad_msg = Msg::RoomTxRequest {
        site: 2,
        id: 1,
        phase: CellShiftPhase::Forward,
        revision: 0,
        patch: Arc::new(RoomTxPatch::AxisPermute(bad_perm)),
    };
    assert!(!bad_msg.is_within_limits());

    // 2. O(occupied_in_band) coordinate / merge / sparse-map / coord-set helpers on million-row coordinates.
    assert_eq!(permute_axis_coord(2, 5, Axis::Row, 2, 2, 10), Some((10, 5)));
    assert_eq!(permute_axis_coord(4, 5, Axis::Row, 2, 2, 10), Some((2, 5)));
    assert_eq!(permute_axis_coord(999_999, 5, Axis::Row, 2, 2, 10), Some((999_999, 5)));

    assert_eq!(
        permute_axis_merge_rect((2, 1, 3, 2), Axis::Row, 2, 2, 10),
        Some((10, 1, 11, 2))
    );
    assert_eq!(
        permute_axis_merge_rect((0, 1, 15, 2), Axis::Row, 2, 2, 10),
        Some((0, 1, 15, 2))
    );
    // 2x2 merge partially losing row 2 shrinks to surviving 1x2 merge (matching Sheet::move_axis_inner).
    assert_eq!(
        permute_axis_merge_rect((1, 1, 2, 2), Axis::Row, 2, 2, 10),
        Some((1, 1, 1, 2))
    );
    // 2x1 vertical merge collapsing to a single cell returns None.
    assert_eq!(
        permute_axis_merge_rect((1, 1, 2, 1), Axis::Row, 2, 2, 10),
        None
    );

    let mut sparse: BTreeMap<(usize, u32, u32), i32> = BTreeMap::from([
        ((0, 1, 0), 10),
        ((0, 2, 0), 20),
        ((0, 3, 0), 30),
        ((0, 5, 0), 50),
        ((0, 999_999, 0), 99),
        ((1, 2, 0), 77),
    ]);
    permute_cell_map(&mut sparse, 0, Axis::Row, 2, 2, 5, |_, _, _| {});
    assert_eq!(sparse.get(&(0, 1, 0)), Some(&10));
    assert_eq!(sparse.get(&(0, 5, 0)), Some(&20));
    assert_eq!(sparse.get(&(0, 6, 0)), Some(&30));
    assert_eq!(sparse.get(&(0, 3, 0)), Some(&50));
    assert_eq!(sparse.get(&(0, 999_999, 0)), Some(&99));
    assert_eq!(sparse.get(&(1, 2, 0)), Some(&77));
    // Inverse restores exact map!
    permute_cell_map(&mut sparse, 0, Axis::Row, 5, 2, 2, |_, _, _| {});
    assert_eq!(sparse.get(&(0, 2, 0)), Some(&20));
    assert_eq!(sparse.get(&(0, 3, 0)), Some(&30));
    assert_eq!(sparse.get(&(0, 5, 0)), Some(&50));

    let mut set: BTreeSet<(usize, u32, u32)> =
        BTreeSet::from([(0, 0, 1), (0, 0, 3), (0, 500_000, 2), (1, 0, 1)]);
    permute_coord_set(&mut set, 0, Axis::Col, 1, 1, 3);
    assert!(set.contains(&(0, 0, 3)));
    assert!(set.contains(&(0, 0, 2)));
    assert!(set.contains(&(0, 500_000, 1)));
    assert!(set.contains(&(1, 0, 1)));

    // 3. Peer::permute_axis_with_formulas + BaselineBook::permute_axis (both baseline & overlay + cross-sheet).
    let mut book = Workbook::new_blank();
    book.sheets[0].name = "Main".into();
    book.sheets[0].set(1, 0, num(100.0));
    book.sheets[0].set(2, 0, num(200.0));
    book.sheets[0].set(4, 0, fml("=A2+A3"));
    let mut s2 = Sheet::new("Calc");
    s2.set(0, 0, fml("=Main!A2*2"));
    book.sheets.push(s2);

    let mut peer = Peer::new(1);
    let _base: BaselineBook = peer.seed_workbook(&book);
    // Add an overlay cell edit from another site referencing Main!A2.
    peer.apply(Op {
        sheet: 1,
        row: 1,
        col: 0,
        value: ValueWire::Empty,
        formula: Some(Arc::from("=Main!A2+1")),
        ts: 5,
        site: 2,
    });
    let names = vec!["Main".to_string(), "Calc".to_string()];
    let before_snap = peer.snapshot();

    // Move row 1 down to row 3 (from=1, n=1, at=3).
    peer.permute_axis_with_formulas(&names, 0, Axis::Row, 1, 1, 3);
    assert_eq!(peer.cells.get(&(0, 3, 0)).unwrap().value, ValueWire::Number(100.0));
    assert_eq!(peer.cells.get(&(0, 1, 0)).unwrap().value, ValueWire::Number(200.0));
    assert_eq!(peer.cells.get(&(0, 4, 0)).unwrap().formula.as_deref(), Some("=A4+A2"));
    assert_eq!(peer.cells.get(&(1, 0, 0)).unwrap().formula.as_deref(), Some("=Main!A4*2"));
    assert_eq!(peer.cells.get(&(1, 1, 0)).unwrap().formula.as_deref(), Some("=Main!A4+1"));

    // Inverse restores exact snapshot!
    peer.permute_axis_with_formulas(&names, 0, Axis::Row, 3, 1, 1);
    assert_eq!(peer.snapshot(), before_snap);

    // 4. LockTable::conflicts_axis_permute & LockTable::permute_axis.
    let mut locks = LockTable::new(30_000);
    assert_eq!(locks.request(0, 2, 0, 10, "PeerA", 1_000), LockOutcome::Granted); // inside band [1, 5)
    assert_eq!(locks.request(0, 8, 0, 11, "PeerB", 1_000), LockOutcome::Granted); // outside band [1, 5)
    assert_eq!(locks.request(1, 2, 0, 12, "PeerC", 1_000), LockOutcome::Granted); // other sheet

    // Conflict for other site when lock is inside [1, 5); NO conflict for own site 10 or expired lock!
    assert!(locks.conflicts_axis_permute(0, Axis::Row, 1, 2, 3, 99, 5_000).is_some());
    assert!(locks.conflicts_axis_permute(0, Axis::Row, 1, 2, 3, 10, 5_000).is_none());
    assert!(locks.conflicts_axis_permute(0, Axis::Row, 1, 2, 3, 99, 35_000).is_none());
    // Band [6, 8) misses row 8 and row 2 -> NO conflict!
    assert!(locks.conflicts_axis_permute(0, Axis::Row, 6, 1, 7, 99, 5_000).is_none());

    // Permute locks for rows [1, 3) -> [3, 5): row 2 -> row 4, row 8 unchanged, sheet 1 unchanged!
    locks.permute_axis(0, Axis::Row, 1, 2, 3);
    assert!(locks.holder(0, 2, 0).is_none());
    assert_eq!(locks.holder(0, 4, 0).map(|l| l.site), Some(10));
    assert_eq!(locks.holder(0, 8, 0).map(|l| l.site), Some(11));
    assert_eq!(locks.holder(1, 2, 0).map(|l| l.site), Some(12));
}

// ---------------------------------------------------------------------------
// Phase 6 A1: CoordParticipant Lifecycle Matrix & Full TxOverlay Coverage
// ---------------------------------------------------------------------------

#[test]
fn coord_participant_lifecycle_matrix_covers_every_mutated_subsystem() {
    assert_eq!(CoordParticipant::ALL.len(), 16);
    for p in CoordParticipant::ALL {
        assert_eq!(
            p.mutated_by_cell_shift(),
            p.cell_shift_mode() == CellShiftMode::Transform,
            "mutated_by_cell_shift mismatch for {p:?}"
        );
        assert!(
            p.mutated_by_axis_permute(),
            "all 16 participants participate in AxisPermute: {p:?}"
        );
        if p.mutated_by_axis_permute() || p.mutated_by_cell_shift() {
            assert!(
                p.captured_in_tx_overlay(),
                "every participant mutated by AxisPermute or CellShift must be captured in TxOverlay: {p:?}"
            );
        }
    }
}

#[test]
fn tx_overlay_axis_permute_restores_tables_layout_filter_arrays_scenarios_pivots_and_dirty_flags() {
    use sheetcore::model::CellRef;
    use sheetcore::pic::Picture;
    use sheetcore::pivot_engine::PivotSlicerSpec;
    use sheetcore::pivot_multi::PivotGrid;
    use sheetcore::protection::ProtectedRange;
    use sheetcore::scenarios::Scenario;
    use sheetcore::sort_filter::Filter;
    use sheetcore::table::Table;
    use sheetcore::textbox::TextBox;
    use sheetcore::tx::TxOverlay;

    let mut host = Workbook::new_blank();
    host.sheets[0].name = "Data".into();
    let host_data_id = host.sheets[0].identity;
    {
        let s = &mut host.sheets[0];
        s.set(0, 0, txt("HeaderA"));
        s.set(0, 1, txt("HeaderB"));
        s.set(1, 0, txt("Row1"));
        s.set(1, 1, num(100.0));
        s.set(2, 0, txt("Row2"));
        s.set(2, 1, num(200.0));

        // 1. Tables (exact A1 repro: Table1 at A1:B2)
        s.tables.push(Table {
            name: "Table1".into(),
            display: "Table1".into(),
            range: (0, 0, 1, 1),
            columns: vec!["HeaderA".into(), "HeaderB".into()],
            ..Table::default()
        });

        // 2. LayoutAndPage (exact A1 repro: row 0 height 60.0 + col width + hidden + outline + freeze + page + picture/textbox)
        s.rows.set(0, 60.0);
        s.cols.set(1, 125.0);
        s.hidden_rows.insert(1);
        s.hidden_rows_user.insert(4);
        s.hidden_cols.insert(2);
        s.row_levels.insert(1, 1);
        s.col_levels.insert(1, 2);
        s.freeze = Some((1, 1));
        s.page.print_area = Some((0, 0, 5, 3));
        s.page.title_rows = Some((0, 1));
        s.page.row_breaks = vec![3];
        s.pictures.push(Picture {
            part: "xl/media/image1.png".into(),
            data: Some(Arc::new(vec![0x89, 0x50, 0x4E, 0x47, 1, 2, 3, 4])),
            rect: (0, 3, 1, 4),
            off: [10, 20, 30, 40],
            name: "Logo".into(),
            ..Picture::default()
        });
        s.textboxes.push(TextBox {
            from: (4, 11, 0, 22),
            to: (5, 33, 1, 44),
            text: "Callout".into(),
            ..TextBox::default()
        });

        // 3. AutoFilter
        let mut flt = Filter::new(0, 0, 1, 1);
        flt.allows.insert(0, HashSet::from(["Row1".into()]));
        s.filter = Some(flt);

        // 4. ArraysAndSpills
        s.arrays.push((CellRef::new(0, 6), (0, 6, 1, 6)));
        s.spilled.insert(CellRef::new(1, 6));
        s.spill_shapes.insert(CellRef::new(0, 6), (2, 1));

        // 5. ScenariosAndProtectedRanges
        let mut sc = Scenario::new("Budget");
        sc.cells.push(((0, 1), "999".into()));
        s.scenarios.push(sc);
        s.scenario_settings.current = Some(0);
        s.scenario_settings.sqref = Some("$B$1:$B$2".into());
        s.protected_ranges
            .push(ProtectedRange::new("PR1", vec![(0, 0, 1, 1)], ""));

        // 6. Charts (anchor + data ref)
        s.charts.push(Chart {
            kind: ChartKind::Column,
            part: "xl/charts/chart1.xml".into(),
            anchor: (0, 7, 1, 9),
            source_range: "Data!$B$1:$B$2".into(),
            series: vec![Series {
                val_ref: "Data!$B$1:$B$2".into(),
                ..Series::default()
            }],
            ..Chart::default()
        });
    }
    host.sheets.push(Sheet::new("PivotSheet"));

    // 7. Workbook Pivots (intact pivot + pivot that splits & gets removed on host + pending + pending2)
    let spec2 = PivotSpec2 {
        source: (0, 0, 1, 1),
        rows: vec![AxisField::plain(0)],
        cols: Vec::new(),
        values: vec![ValueField {
            col: 1,
            agg: Agg::Sum,
            label: None,
            show_as: ShowAs::Normal,
            base: None,
        }],
        filters: Vec::new(),
        layout: PivotLayout::default(),
    };
    let mut piv_intact = PivotTableModel::new(
        "PIntact".into(),
        "Data".into(),
        Some(host_data_id),
        1,
        (2, 0),
        spec2.clone(),
    );
    piv_intact.rect = (2, 0, 4, 2);
    piv_intact.last_meta.rect = (2, 0, 4, 2);
    piv_intact.imported_part = Some("xl/pivotTables/pivotTable1.xml".into());
    piv_intact.options.slicers.push(PivotSlicerSpec {
        id: 1,
        col: 1,
        title: "Amt".into(),
        pos: [0.0, 0.0],
        width: 100.0,
        columns: 1,
    });
    host.pivots.push(piv_intact);

    let mut piv_split = PivotTableModel::new(
        "PSplit".into(),
        "Data".into(),
        Some(host_data_id),
        1,
        (6, 0),
        PivotSpec2 {
            source: (0, 0, 2, 1), // rows 0..=2 will split when rows 0..=1 move to 2..=3!
            ..spec2.clone()
        },
    );
    piv_split.rect = (6, 0, 8, 2);
    piv_split.imported_part = Some("xl/pivotTables/pivotTable2.xml".into());
    host.pivots.push(piv_split);

    host.pivots_pending.push((
        0,
        PivotSpec {
            source: (0, 0, 1, 1),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        },
        PivotResult::default(),
        (0, 10, 1, 11),
        "Pend1".into(),
    ));
    host.pivots_pending2
        .push((0, spec2, PivotGrid::default(), (0, 12, 1, 13)));

    // Simulate a late-joining guest opening the exact same disk baseline,
    // but with DIFFERENT process-local Sheet::identity and PivotTableModel::id!
    let mut guest = host.clone();
    guest.sheets[0].identity = host_data_id.wrapping_add(10_000);
    for p in &mut guest.pivots {
        p.id = p.id.wrapping_add(500);
        p.source_sheet_id = Some(guest.sheets[0].identity);
    }

    // Online host performs AxisPermute: move rows 0..=1 to rows 2..=3 (drop=4), then swap cols 0 and 1!
    let row_perm = AxisPermutePatch::from_drop(&host, 0, Axis::Row, 0, 2, 4).unwrap();
    assert!(row_perm.apply(&mut host, true));
    let col_perm = AxisPermutePatch::from_drop(&host, 0, Axis::Col, 0, 1, 2).unwrap();
    assert!(col_perm.apply(&mut host, true));

    // Host has NOT saved; capture TxOverlay and wire-roundtrip it as JSON.
    let overlay = TxOverlay::capture(&host);
    assert!(overlay.wire_valid());
    let wire_json = serde_json::to_string(&overlay).unwrap();
    // Raw PNG image bytes MUST NOT be serialized on the wire!
    assert!(
        !wire_json.contains("[137,80,78,71"),
        "TxOverlay must project only picture geometry, never raw bytes"
    );
    let decoded: TxOverlay = serde_json::from_str(&wire_json).unwrap();
    assert!(decoded.wire_valid());
    assert_eq!(decoded, overlay);

    // Clear dirty flags on guest before applying overlay.
    let clear_dirty = |s: &mut Sheet| {
        s.tables_dirty = false;
        s.layout_dirty = false;
        s.freeze_dirty = false;
        s.filter_dirty = false;
        s.scenarios_dirty = false;
        s.protected_ranges_dirty = false;
        s.page_dirty = false;
        s.pictures_dirty = false;
        s.textboxes_dirty = false;
        s.charts_dirty = false;
    };
    clear_dirty(&mut guest.sheets[0]);

    assert!(decoded.apply_to(&mut guest));

    let hs = &host.sheets[0];
    let gs = &guest.sheets[0];

    // All 6 newly covered participant groups + chart anchors match online host!
    assert_eq!(gs.tables, hs.tables);
    assert_eq!(gs.tables[0].range, (2, 0, 3, 1));
    assert_eq!(gs.tables[0].columns, vec!["HeaderB", "HeaderA"]);
    assert!((gs.rows.size(2) - 60.0).abs() < 1e-3);
    assert!((gs.rows.size(0) - 18.0).abs() < 1e-3);
    assert!((gs.cols.size(0) - 125.0).abs() < 1e-3);
    assert_eq!(gs.hidden_rows, hs.hidden_rows);
    assert_eq!(gs.hidden_rows_user, hs.hidden_rows_user);
    assert_eq!(gs.hidden_cols, hs.hidden_cols);
    assert_eq!(gs.row_levels, hs.row_levels);
    assert_eq!(gs.col_levels, hs.col_levels);
    assert_eq!(gs.freeze, hs.freeze);
    assert_eq!(gs.page, hs.page);
    assert_eq!(gs.pictures[0].rect, hs.pictures[0].rect);
    assert_eq!(gs.pictures[0].off, hs.pictures[0].off);
    assert_eq!(
        gs.pictures[0].data.as_deref(),
        Some(&vec![0x89, 0x50, 0x4E, 0x47, 1, 2, 3, 4]),
        "local picture bytes must stay intact"
    );
    assert_eq!(gs.textboxes[0].from, hs.textboxes[0].from);
    assert_eq!(gs.textboxes[0].to, hs.textboxes[0].to);
    assert_eq!(gs.charts[0].anchor, hs.charts[0].anchor);
    assert_eq!(gs.charts[0].series[0].val_ref, hs.charts[0].series[0].val_ref);
    assert_eq!(gs.filter, hs.filter);
    assert_eq!(gs.arrays, hs.arrays);
    assert_eq!(gs.spilled, hs.spilled);
    assert_eq!(gs.spill_shapes, hs.spill_shapes);
    assert_eq!(gs.scenarios, hs.scenarios);
    assert_eq!(gs.scenario_settings.current, hs.scenario_settings.current);
    assert_eq!(gs.scenario_settings.sqref, hs.scenario_settings.sqref);
    assert_eq!(gs.protected_ranges, hs.protected_ranges);

    // Pivots matched by imported_part/name, split pivot removed, source_sheet_id mapped to guest's local identity!
    assert_eq!(guest.pivots.len(), 1);
    assert_eq!(guest.pivots[0].name, "PIntact");
    assert_eq!(guest.pivots[0].source_sheet_id, Some(guest.sheets[0].identity));
    assert_eq!(guest.pivots[0].spec.source, host.pivots[0].spec.source);
    assert_eq!(guest.pivots[0].spec.rows[0].col, host.pivots[0].spec.rows[0].col);
    assert_eq!(guest.pivots[0].spec.values[0].col, host.pivots[0].spec.values[0].col);
    assert_eq!(guest.pivots[0].options.slicers[0].col, host.pivots[0].options.slicers[0].col);
    assert_eq!(guest.pivots[0].rect, host.pivots[0].rect);
    assert!(guest.pivots_removed_parts.contains(&"xl/pivotTables/pivotTable2.xml".to_string()));
    assert_eq!(guest.pivots_pending[0].1.source, host.pivots_pending[0].1.source);
    assert_eq!(guest.pivots_pending[0].1.row_field, host.pivots_pending[0].1.row_field);
    assert_eq!(guest.pivots_pending[0].3, host.pivots_pending[0].3);
    assert_eq!(guest.pivots_pending2[0].1.source, host.pivots_pending2[0].1.source);
    assert_eq!(guest.pivots_pending2[0].1.rows[0].col, host.pivots_pending2[0].1.rows[0].col);
    assert_eq!(guest.pivots_pending2[0].3, host.pivots_pending2[0].3);

    // Dirty flags WERE set because values changed.
    assert!(gs.tables_dirty);
    assert!(gs.layout_dirty);
    assert!(gs.filter_dirty);
    assert!(gs.scenarios_dirty);
    assert!(gs.protected_ranges_dirty);
    assert!(gs.page_dirty);
    assert!(gs.pictures_dirty);
    assert!(gs.textboxes_dirty);
    assert!(gs.charts_dirty);

    // Re-applying identical overlay MUST NOT mark any *_dirty flag!
    clear_dirty(&mut guest.sheets[0]);
    assert!(decoded.apply_to(&mut guest));
    let gs2 = &guest.sheets[0];
    assert!(!gs2.tables_dirty);
    assert!(!gs2.layout_dirty);
    assert!(!gs2.freeze_dirty);
    assert!(!gs2.filter_dirty);
    assert!(!gs2.scenarios_dirty);
    assert!(!gs2.protected_ranges_dirty);
    assert!(!gs2.page_dirty);
    assert!(!gs2.pictures_dirty);
    assert!(!gs2.textboxes_dirty);
    assert!(!gs2.charts_dirty);
}

#[test]
fn tx_overlay_legacy_json_preserves_baseline_and_wire_valid_bounds_new_fields() {
    use sheetcore::table::Table;
    use sheetcore::tx::TxOverlay;

    // 1. Legacy JSON payload without `format_version` or any Phase 6 fields
    //    must deserialize cleanly, validate, and NOT wipe baseline tables/layout!
    let mut wb = Workbook::new_blank();
    wb.sheets[0].tables.push(Table {
        name: "BaselineT".into(),
        display: "BaselineT".into(),
        range: (0, 0, 3, 2),
        columns: vec!["A".into(), "B".into(), "C".into()],
        ..Table::default()
    });
    wb.sheets[0].rows.set(0, 48.0);

    let mut legacy_json = serde_json::to_value(TxOverlay::capture(&Workbook::new_blank())).unwrap();
    let root = legacy_json.as_object_mut().unwrap();
    root.remove("format_version");
    root.remove("pivots");
    root.remove("pivots_pending");
    root.remove("pivots_pending2");
    root.remove("pivot_shifts");
    for sh in root["sheets"].as_array_mut().unwrap() {
        let m = sh.as_object_mut().unwrap();
        for key in [
            "chart_anchors",
            "tables",
            "filter",
            "arrays",
            "spilled",
            "spill_shapes",
            "scenarios",
            "scenario_settings",
            "protected_ranges",
            "layout",
        ] {
            m.remove(key);
        }
    }
    let legacy: TxOverlay = serde_json::from_value(legacy_json).unwrap();
    assert!(legacy.wire_valid());
    assert!(legacy.apply_to(&mut wb));
    assert_eq!(wb.sheets[0].tables.len(), 1);
    assert_eq!(wb.sheets[0].tables[0].name, "BaselineT");
    assert!((wb.sheets[0].rows.size(0) - 48.0).abs() < 1e-3);

    // 2. Strict wire_valid rejection across malformed new Phase 6 fields.
    let base = serde_json::to_value(TxOverlay::capture(&wb)).unwrap();

    // Invalid table range (r2 == MAX_ROWS)
    let mut bad_table = base.clone();
    bad_table["sheets"][0]["tables"][0]["range"] = serde_json::json!([0, 0, MAX_ROWS, 1]);
    assert!(!serde_json::from_value::<TxOverlay>(bad_table).unwrap().wire_valid());

    // Invalid row height (negative / excessive)
    let mut bad_height = base.clone();
    bad_height["sheets"][0]["layout"]["rows"]["spans"] = serde_json::json!([[0, 0, -5.0]]);
    assert!(!serde_json::from_value::<TxOverlay>(bad_height).unwrap().wire_valid());

    let mut huge_height = base.clone();
    huge_height["sheets"][0]["layout"]["rows"]["spans"] = serde_json::json!([[0, 0, 999_999.0]]);
    assert!(!serde_json::from_value::<TxOverlay>(huge_height).unwrap().wire_valid());

    // Invalid hidden row interval (hi >= MAX_ROWS)
    let mut bad_hidden = base.clone();
    bad_hidden["sheets"][0]["layout"]["hidden_rows"] = serde_json::json!([[0, MAX_ROWS]]);
    assert!(!serde_json::from_value::<TxOverlay>(bad_hidden).unwrap().wire_valid());

    // Invalid textbox anchor (col >= MAX_COLS)
    let mut bad_tb = base.clone();
    bad_tb["sheets"][0]["layout"]["textboxes"] = serde_json::json!([{
        "id": 2,
        "from": [MAX_COLS, 0, 0, 0],
        "to": [MAX_COLS, 0, 1, 0]
    }]);
    assert!(!serde_json::from_value::<TxOverlay>(bad_tb).unwrap().wire_valid());

    // Invalid autofilter col >= MAX_COLS
    let mut bad_flt = base.clone();
    bad_flt["sheets"][0]["filter"] = serde_json::json!({
        "range": [0, 0, 2, 2],
        "allows": [[MAX_COLS, ["x"]]]
    });
    assert!(!serde_json::from_value::<TxOverlay>(bad_flt).unwrap().wire_valid());

    // Invalid pivot target_sheet out of bounds
    let mut bad_piv = base;
    bad_piv["pivots"] = serde_json::json!([{
        "name": "P1",
        "source_sheet": "Sheet1",
        "target_sheet": 99,
        "anchor": [0, 0],
        "rect": [0, 0, 1, 1],
        "last_meta_anchor": [0, 0],
        "last_meta_rect": [0, 0, 1, 1],
        "source": [0, 0, 1, 1]
    }]);
    assert!(!serde_json::from_value::<TxOverlay>(bad_piv).unwrap().wire_valid());
}

#[test]
fn sync_full_tx_overlay_negotiation_wire_compat_and_future_overlay_version_rejected() {
    use sheetcore::cell_shift_patch::CURRENT_OVERLAY_FORMAT_VERSION;
    use sheetcore::table::Table;
    use sheetcore::tx::TxOverlay;

    // 1. Legacy JSON lacking `full_tx_overlay` defaults to `false` while preserving
    //    `room_tx` and `cell_shifts` (`cell_shifts_v2`).
    let legacy_hello: Msg = serde_json::from_str(
        r#"{"t":"Hello","site":3,"name":"Peer","doc":"book","proof":"ok","cell_shifts_v2":true,"room_tx":true}"#,
    )
    .unwrap();
    assert!(matches!(
        legacy_hello,
        Msg::Hello {
            cell_shifts: true,
            room_tx: true,
            full_tx_overlay: false,
            ..
        }
    ));
    assert!(legacy_hello.is_within_limits());

    let legacy_welcome: Msg = serde_json::from_str(
        r#"{"t":"Welcome","site":3,"doc":"book","peers":[],"cell_shifts_v2":true,"room_tx":true}"#,
    )
    .unwrap();
    assert!(matches!(
        legacy_welcome,
        Msg::Welcome {
            cell_shifts: true,
            room_tx: true,
            full_tx_overlay: false,
            ..
        }
    ));
    assert!(legacy_welcome.is_within_limits());

    // 2. New JSON carrying `full_tx_overlay: true` roundtrips cleanly.
    let new_hello = Msg::Hello {
        site: 4,
        name: "NewPeer".into(),
        doc: "book".into(),
        proof: "ok".into(),
        auth: String::new(),
        base_hash: String::new(),
        cell_shifts: true,
        sort_permutation: true,
        palette_styles: true,
        resume_room: String::new(),
        resume_watermark: 0,
        resume_struct_rev: 0,
        binary_v3: true,
        compact_v3: true,
        room_tx: true,
        full_tx_overlay: true,
    };
    let hello_json = serde_json::to_string(&new_hello).unwrap();
    let decoded_hello: Msg = serde_json::from_str(&hello_json).unwrap();
    assert!(matches!(
        decoded_hello,
        Msg::Hello {
            cell_shifts: true,
            room_tx: true,
            full_tx_overlay: true,
            ..
        }
    ));
    assert!(decoded_hello.is_within_limits());

    let new_welcome = Msg::Welcome {
        site: 4,
        doc: "book".into(),
        peers: vec![],
        delta_sync: true,
        batch_ops: true,
        cell_shifts: true,
        sort_permutation: true,
        structural_ack: true,
        thread_changes: true,
        clock: 0,
        palette_styles: true,
        room_id: "r1".into(),
        watermark: 0,
        structure_revision: 0,
        binary_v3: true,
        compact_v3: true,
        acked_op_ts: 0,
        room_tx: true,
        full_tx_overlay: true,
    };
    let welcome_json = serde_json::to_string(&new_welcome).unwrap();
    let decoded_welcome: Msg = serde_json::from_str(&welcome_json).unwrap();
    assert!(matches!(
        decoded_welcome,
        Msg::Welcome {
            cell_shifts: true,
            room_tx: true,
            full_tx_overlay: true,
            ..
        }
    ));
    assert!(decoded_welcome.is_within_limits());

    // 3. TxOverlay format_version checks:
    //    - current version == CURRENT_OVERLAY_FORMAT_VERSION: valid & full state
    //    - legacy version == 0: valid & NOT full state
    //    - unknown future versions (> CURRENT_OVERLAY_FORMAT_VERSION): strictly rejected by
    //      `wire_valid()` AND `apply_to()` without mutating `wb`.
    let mut wb = Workbook::new_blank();
    wb.sheets[0].tables.push(Table {
        name: "T1".into(),
        display: "T1".into(),
        range: (0, 0, 2, 1),
        columns: vec!["A".into(), "B".into()],
        ..Table::default()
    });
    wb.sheets[0].rows.set(0, 60.0);
    wb.dirty = false;
    let epoch_before = wb.coord_epoch();

    let current = TxOverlay::capture(&wb);
    assert_eq!(current.format_version(), CURRENT_OVERLAY_FORMAT_VERSION);
    assert!(current.is_full_state());
    assert!(current.wire_valid());

    let mut legacy_json = serde_json::to_value(&current).unwrap();
    legacy_json.as_object_mut().unwrap().remove("format_version");
    let legacy: TxOverlay = serde_json::from_value(legacy_json).unwrap();
    assert_eq!(legacy.format_version(), 0);
    assert!(!legacy.is_full_state());
    assert!(legacy.wire_valid());

    for future_ver in [
        CURRENT_OVERLAY_FORMAT_VERSION + 1,
        CURRENT_OVERLAY_FORMAT_VERSION.saturating_add(5),
        u8::MAX,
    ] {
        let mut future_json = serde_json::to_value(&TxOverlay::capture(&Workbook::new_blank())).unwrap();
        future_json["format_version"] = serde_json::json!(future_ver);
        let future: TxOverlay = serde_json::from_value(future_json).unwrap();
        assert_eq!(future.format_version(), future_ver);
        assert!(!future.is_full_state());
        assert!(
            !future.wire_valid(),
            "future format_version {future_ver} must be rejected by wire_valid()"
        );
        assert!(
            !future.apply_to(&mut wb),
            "future format_version {future_ver} must be rejected by apply_to()"
        );
        assert_eq!(wb.coord_epoch(), epoch_before);
        assert!(!wb.dirty);
        assert_eq!(wb.sheets[0].tables.len(), 1);
        assert_eq!(wb.sheets[0].tables[0].name, "T1");
        assert!((wb.sheets[0].rows.size(0) - 60.0).abs() < 1e-3);
    }
}
