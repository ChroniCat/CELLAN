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
