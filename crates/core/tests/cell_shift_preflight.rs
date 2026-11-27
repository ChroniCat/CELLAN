use sheetcore::cell_shift_patch::{CellShiftOverlay, CellShiftPatch};
use sheetcore::cells::Shift;
use sheetcore::model::{Cell, CellRef, Sheet, Value, Workbook};
use sheetcore::pivot::{Agg, GroupBy, PivotResult, PivotSpec};
use sheetcore::pivot_engine::PivotTableModel;
use sheetcore::pivot_multi::{AxisField, PivotGrid, PivotLayout, PivotSpec2, ShowAs, ValueField};
use sheetcore::sort_filter::Filter;
use sheetcore::structural::{CellShift, CellShiftConflict};
use sheetcore::table::Table;
use sheetcore::undo::{UndoStack, snapshot_cell_shift_if_metadata};

fn table(name: &str, range: (u32, u32, u32, u32)) -> Table {
    Table {
        name: name.into(),
        display: name.into(),
        range,
        ..Table::default()
    }
}

fn autofilter(range: (u32, u32, u32, u32)) -> Filter {
    Filter {
        range,
        allows: Default::default(),
        criteria: Default::default(),
        date_groups: Default::default(),
        color_filters: Default::default(),
        top_filters: Default::default(),
        dynamic_filters: Default::default(),
    }
}

fn num(v: f64) -> Cell {
    Cell {
        value: Value::Number(v),
        formula: None,
    }
}

fn formula(f: &str) -> Cell {
    Cell {
        value: Value::Empty,
        formula: Some(f.into()),
    }
}

fn assert_blocked(wb: &mut Workbook, sheet: usize, op: CellShift, expected: CellShiftConflict) {
    assert_eq!(op.preflight(wb, sheet), Err(expected));
    assert!(CellShiftPatch::prepare(wb, sheet, op).is_none());
    let before_overlay = CellShiftOverlay::capture(wb);
    let before_names = wb.names.clone();
    let before_tables = wb.sheets.get(sheet).map(|s| s.tables.clone());
    let before_filter = wb.sheets.get(sheet).map(|s| s.filter.clone());
    let before_arrays = wb.sheets.get(sheet).map(|s| s.arrays.clone());
    assert!(!op.apply(wb, sheet));
    assert_eq!(CellShiftOverlay::capture(wb), before_overlay);
    assert_eq!(wb.names, before_names);
    assert_eq!(wb.sheets.get(sheet).map(|s| s.tables.clone()), before_tables);
    assert_eq!(wb.sheets.get(sheet).map(|s| s.filter.clone()), before_filter);
    assert_eq!(wb.sheets.get(sheet).map(|s| s.arrays.clone()), before_arrays);
}

#[test]
fn preflight_conflict_messages_are_non_empty_and_display_matches() {
    let all = [
        CellShiftConflict::InvalidTarget,
        CellShiftConflict::Overflow,
        CellShiftConflict::PartialMerge,
        CellShiftConflict::Table,
        CellShiftConflict::Pivot,
        CellShiftConflict::ArrayOrSpill,
        CellShiftConflict::AutoFilter,
    ];
    for c in all {
        assert!(!c.message().trim().is_empty());
        assert_eq!(format!("{c}"), c.message());
    }
}

#[test]
fn snapshot_cell_shift_if_metadata_triggers_when_only_defined_names_exist() {
    let mut wb = Workbook::new_blank();
    wb.names.push(("Target".into(), None, "Sheet1!$A$3".into()));

    let invalid_op = CellShift::new((0, 0, 0, 0), Shift::Vertical, true);
    assert!(snapshot_cell_shift_if_metadata(&wb, 99, &invalid_op).is_none());

    // Shifting an unrelated column captures a snapshot (because wb.names is non-empty),
    // but reports that no metadata actually changed.
    let miss_op = CellShift::new((0, 2, 0, 2), Shift::Vertical, true);
    let before_miss = snapshot_cell_shift_if_metadata(&wb, 0, &miss_op).unwrap();
    assert!(miss_op.apply(&mut wb, 0));
    assert!(!before_miss.cell_shift_changed_metadata(&wb));
    assert_eq!(wb.names[0].2, "Sheet1!$A$3");
    assert!(!wb.names_dirty);

    // Shifting col A rewrites wb.names and marks metadata changed even when sheet has no other metadata.
    let hit_op = CellShift::new((0, 0, 0, 0), Shift::Vertical, true);
    let before_hit = snapshot_cell_shift_if_metadata(&wb, 0, &hit_op).unwrap();
    assert!(hit_op.apply(&mut wb, 0));
    assert!(before_hit.cell_shift_changed_metadata(&wb));
    assert!(wb.names_dirty);
    assert_eq!(wb.names[0].2, "Sheet1!$A$4");
}

#[test]
fn cell_shift_defined_names_scopes_cross_sheet_and_ref_error_roundtrip() {
    let mut seed = Workbook::new_blank();
    seed.sheets.push(Sheet::new("Report"));
    seed.sheets[0].set(0, 0, num(10.0));
    seed.sheets[0].set(1, 0, num(20.0));
    seed.names = vec![
        ("Rate".into(), None, "Sheet1!$A$2".into()),
        ("Rate".into(), Some(0), "$A$2".into()),
        ("Rate".into(), Some(1), "$A$2".into()),
        ("CrossLocal".into(), Some(1), "Sheet1!$A$2".into()),
        ("GlobalUnqual".into(), None, "$A$2".into()),
        ("OtherSheet".into(), None, "Report!$A$2".into()),
        ("DeadGlobal".into(), None, "Sheet1!$A$1".into()),
        ("DeadLocal".into(), Some(0), "$A$1".into()),
    ];
    let op = CellShift::new((0, 0, 0, 0), Shift::Vertical, false);

    // 1. StructureSnapshot + UndoStack roundtrip (including #REF! recovery)
    let mut wb = seed.clone();
    let before = snapshot_cell_shift_if_metadata(&wb, 0, &op).unwrap();
    assert!(op.apply(&mut wb, 0));
    assert!(before.cell_shift_changed_metadata(&wb));
    let after = before.capture_after(&wb);
    let mut history = UndoStack::new();
    history.push_structure("delete", before, after, (0, 0, 0));

    let expect_deleted = [
        "Sheet1!$A$1",
        "$A$1",
        "$A$2",
        "Sheet1!$A$1",
        "$A$2",
        "Report!$A$2",
        "Sheet1!#REF!",
        "#REF!",
    ];
    let got_after: Vec<&str> = wb.names.iter().map(|(_, _, t)| t.as_str()).collect();
    assert_eq!(got_after, expect_deleted);

    assert!(history.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.names, seed.names);
    assert!(history.redo_workbook(&mut wb, |_, _, _, _| true).is_some());
    let got_redo: Vec<&str> = wb.names.iter().map(|(_, _, t)| t.as_str()).collect();
    assert_eq!(got_redo, expect_deleted);

    // 2. CellShiftPatch + canonical_forward (guest lacks names, including scoped name on Report with no cells changed on Report)
    let mut host = seed.clone();
    let mut guest = seed.clone();
    guest.names.clear();
    let guest_req = CellShiftPatch::prepare(&guest, 0, op).unwrap();
    let accepted = guest_req.canonical_forward(&host).unwrap();
    assert_eq!(accepted.referenced_sheets(), vec![0, 1]);

    // Room undo history must track Sheet 1 (Report) identity because CrossLocal is scoped to Sheet 1.
    let mut room_undo = UndoStack::new();
    room_undo.push_room_cell_shift(1, std::sync::Arc::new(accepted.clone()), &host);
    assert!(room_undo.room_shift_history_matches(&host, false, false));

    assert!(accepted.apply(&mut host, true));
    assert!(accepted.apply_committed(&mut guest, true));
    assert!(CellShiftOverlay::capture(&host).apply_to(&mut guest));
    assert_eq!(host.names, wb.names);
    assert_eq!(guest.names, wb.names);

    // Renaming Report invalidates can_install because CrossLocal is scoped to Report.
    let mut renamed = host.clone();
    renamed.sheets[1].name = "RenamedReport".into();
    assert!(!accepted.can_install(&renamed));
    assert!(!accepted.apply(&mut renamed, false));

    // Replacing Report sheet identity invalidates room_shift_history_matches.
    let mut replaced = host.clone();
    replaced.sheets[1] = Sheet::new("Report");
    assert!(!room_undo.room_shift_history_matches(&replaced, false, false));

    // Undo and redo via CellShiftPatch recover #REF! cleanly.
    assert!(accepted.apply(&mut host, false));
    assert_eq!(host.names, seed.names);
    assert!(accepted.apply(&mut host, true));
    assert_eq!(host.names, wb.names);
}

#[test]
fn partial_cell_shift_keeps_cross_band_and_whole_axis_ranges_intact() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 4, formula("=SUM(A2:A5)+SUM(A2:C5)+SUM(2:5)+A2"));
    wb.names = vec![
        ("InBandV".into(), None, "Sheet1!$A$2:$A$5".into()),
        ("CrossBandV".into(), None, "Sheet1!$A$2:$C$5".into()),
        ("WholeRows".into(), None, "Sheet1!2:5".into()),
        ("SingleV".into(), None, "Sheet1!$A$2".into()),
    ];
    let v_op = CellShift::new((0, 0, 0, 0), Shift::Vertical, true);
    assert!(v_op.apply(&mut wb, 0));
    assert_eq!(
        wb.sheets[0].formula_of(0, 4),
        Some("=SUM(A3:A6)+SUM(A2:C5)+SUM(2:5)+A3")
    );
    assert_eq!(wb.names[0].2, "Sheet1!$A$3:$A$6");
    assert_eq!(wb.names[1].2, "Sheet1!$A$2:$C$5");
    assert_eq!(wb.names[2].2, "Sheet1!2:5");
    assert_eq!(wb.names[3].2, "Sheet1!$A$3");

    let mut wb_h = Workbook::new_blank();
    wb_h.sheets[0].set(4, 0, formula("=SUM(B1:D1)+SUM(B1:D3)+SUM(B:D)+B1"));
    wb_h.names = vec![
        ("InBandH".into(), None, "Sheet1!$B$1:$D$1".into()),
        ("CrossBandH".into(), None, "Sheet1!$B$1:$D$3".into()),
        ("WholeCols".into(), None, "Sheet1!B:D".into()),
        ("SingleH".into(), None, "Sheet1!$B$1".into()),
    ];
    let h_op = CellShift::new((0, 0, 0, 0), Shift::Horizontal, true);
    assert!(h_op.apply(&mut wb_h, 0));
    assert_eq!(
        wb_h.sheets[0].formula_of(4, 0),
        Some("=SUM(C1:E1)+SUM(B1:D3)+SUM(B:D)+C1")
    );
    assert_eq!(wb_h.names[0].2, "Sheet1!$C$1:$E$1");
    assert_eq!(wb_h.names[1].2, "Sheet1!$B$1:$D$3");
    assert_eq!(wb_h.names[2].2, "Sheet1!B:D");
    assert_eq!(wb_h.names[3].2, "Sheet1!$C$1");
}

#[test]
fn cell_shift_patch_rejects_duplicate_and_oversized_defined_names() {
    let mut wb = Workbook::new_blank();
    wb.names = vec![
        ("Dup".into(), None, "Sheet1!$A$2".into()),
        ("Dup".into(), None, "Sheet1!$Z$9".into()),
    ];
    let op = CellShift::new((0, 0, 0, 0), Shift::Vertical, true);
    assert!(CellShiftPatch::prepare(&wb, 0, op).is_none());

    // Valid workbook -> prepare succeeds, then duplicate live name must fail names_match on apply.
    wb.names.pop();
    let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    wb.names.push(("Dup".into(), None, "Sheet1!$Z$9".into()));
    assert!(!patch.apply(&mut wb, true));

    // Tampered wire JSON: duplicate name entry, no-op name entry, and oversized formula.
    let mut json: serde_json::Value = serde_json::to_value(&patch).unwrap();
    let entry = json["names"][0].clone();
    json["names"].as_array_mut().unwrap().push(entry);
    let dup_patch: CellShiftPatch = serde_json::from_value(json).unwrap();
    assert!(!dup_patch.wire_valid());

    let mut noop_json: serde_json::Value = serde_json::to_value(&patch).unwrap();
    noop_json["names"][0]["after"] = noop_json["names"][0]["before"].clone();
    let noop_patch: CellShiftPatch = serde_json::from_value(noop_json).unwrap();
    assert!(!noop_patch.wire_valid());

    let huge = "A".repeat(sheetcore::sync::MAX_THREAD_ENTRY_TEXT_BYTES + 1);
    let mut big_json: serde_json::Value = serde_json::to_value(&patch).unwrap();
    big_json["names"][0]["after"] = serde_json::Value::String(huge.clone());
    let big_patch: CellShiftPatch = serde_json::from_value(big_json).unwrap();
    assert!(!big_patch.wire_valid());

    wb.names.pop();
    let mut overlay_json: serde_json::Value =
        serde_json::to_value(CellShiftOverlay::capture(&wb)).unwrap();
    overlay_json["names"][0][2] = serde_json::Value::String(huge);
    let big_overlay: CellShiftOverlay = serde_json::from_value(overlay_json).unwrap();
    assert!(!big_overlay.wire_valid());
    assert!(!big_overlay.apply_to(&mut wb));
}

#[test]
fn preflight_invalid_target_and_overflow() {
    let mut wb = Workbook::new_blank();
    assert_blocked(
        &mut wb,
        5,
        CellShift::new((0, 0, 0, 0), Shift::Vertical, true),
        CellShiftConflict::InvalidTarget,
    );
    assert_blocked(
        &mut wb,
        0,
        CellShift::new((3, 0, 1, 0), Shift::Vertical, true),
        CellShiftConflict::InvalidTarget,
    );
    assert_blocked(
        &mut wb,
        0,
        CellShift::new((0, 3, 0, 1), Shift::Horizontal, false),
        CellShiftConflict::InvalidTarget,
    );
    assert_blocked(
        &mut wb,
        0,
        CellShift::new(
            (sheetcore::sync::MAX_ROWS, 0, sheetcore::sync::MAX_ROWS, 0),
            Shift::Vertical,
            true,
        ),
        CellShiftConflict::InvalidTarget,
    );
    assert_blocked(
        &mut wb,
        0,
        CellShift::new(
            (0, sheetcore::sync::MAX_COLS, 0, sheetcore::sync::MAX_COLS),
            Shift::Horizontal,
            true,
        ),
        CellShiftConflict::InvalidTarget,
    );

    // Vertical overflow from last row
    wb.sheets[0].set(sheetcore::sync::MAX_ROWS - 1, 0, num(1.0));
    assert_blocked(
        &mut wb,
        0,
        CellShift::new((0, 0, 0, 0), Shift::Vertical, true),
        CellShiftConflict::Overflow,
    );
    // Horizontal overflow from last col
    let mut wb_h = Workbook::new_blank();
    wb_h.sheets[0].set(0, sheetcore::sync::MAX_COLS - 1, num(2.0));
    assert_blocked(
        &mut wb_h,
        0,
        CellShift::new((0, 0, 0, 0), Shift::Horizontal, true),
        CellShiftConflict::Overflow,
    );
}

#[test]
fn preflight_partial_merge_blocks_and_contained_merge_allows() {
    // Vertical partial merges
    for (merge, op) in [
        // Crosses right band boundary
        ((2, 0, 3, 1), CellShift::new((0, 0, 0, 0), Shift::Vertical, true)),
        ((2, 0, 3, 1), CellShift::new((0, 0, 0, 0), Shift::Vertical, false)),
        // Crosses left band boundary
        ((2, 0, 3, 1), CellShift::new((0, 1, 0, 1), Shift::Vertical, true)),
        // Straddles insert top boundary
        ((0, 0, 2, 0), CellShift::new((1, 0, 1, 0), Shift::Vertical, true)),
        // Straddles delete start boundary
        ((0, 0, 2, 0), CellShift::new((1, 0, 2, 0), Shift::Vertical, false)),
        // Straddles delete end boundary
        ((1, 0, 3, 0), CellShift::new((1, 0, 2, 0), Shift::Vertical, false)),
    ] {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].merges.push(merge);
        assert_blocked(&mut wb, 0, op, CellShiftConflict::PartialMerge);
    }

    // Vertical allowed cases
    let mut wb = Workbook::new_blank();
    wb.sheets[0].merges = vec![(0, 0, 1, 0), (3, 0, 4, 0), (0, 2, 4, 3)].into();
    let v_ins = CellShift::new((2, 0, 2, 0), Shift::Vertical, true);
    assert_eq!(v_ins.preflight(&wb, 0), Ok(()));
    assert!(v_ins.apply(&mut wb, 0));
    assert_eq!(wb.sheets[0].merges.to_vec(), vec![(0, 0, 1, 0), (4, 0, 5, 0), (0, 2, 4, 3)]);

    // Delete removes wholly contained merge and shifts merge below it
    let mut wb_del = Workbook::new_blank();
    wb_del.sheets[0].merges = vec![(1, 0, 2, 0), (3, 0, 4, 0)].into();
    let v_del = CellShift::new((1, 0, 2, 0), Shift::Vertical, false);
    assert_eq!(v_del.preflight(&wb_del, 0), Ok(()));
    assert!(v_del.apply(&mut wb_del, 0));
    assert_eq!(wb_del.sheets[0].merges.to_vec(), vec![(1, 0, 2, 0)]);

    // Horizontal partial merges
    for (merge, op) in [
        // Crosses bottom band boundary
        ((0, 2, 1, 3), CellShift::new((0, 0, 0, 0), Shift::Horizontal, true)),
        ((0, 2, 1, 3), CellShift::new((0, 0, 0, 0), Shift::Horizontal, false)),
        // Crosses top band boundary
        ((0, 2, 1, 3), CellShift::new((1, 0, 1, 0), Shift::Horizontal, true)),
        // Straddles insert left boundary
        ((0, 1, 0, 3), CellShift::new((0, 2, 0, 2), Shift::Horizontal, true)),
        // Straddles delete start boundary
        ((0, 1, 0, 3), CellShift::new((0, 2, 0, 3), Shift::Horizontal, false)),
        // Straddles delete end boundary
        ((0, 2, 0, 4), CellShift::new((0, 2, 0, 3), Shift::Horizontal, false)),
    ] {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].merges.push(merge);
        assert_blocked(&mut wb, 0, op, CellShiftConflict::PartialMerge);
    }

    // Horizontal allowed cases
    let mut wb_h = Workbook::new_blank();
    wb_h.sheets[0].merges = vec![(0, 0, 0, 1), (0, 3, 0, 4), (2, 0, 3, 4)].into();
    let h_ins = CellShift::new((0, 2, 0, 2), Shift::Horizontal, true);
    assert_eq!(h_ins.preflight(&wb_h, 0), Ok(()));
    assert!(h_ins.apply(&mut wb_h, 0));
    assert_eq!(
        wb_h.sheets[0].merges.to_vec(),
        vec![(0, 0, 0, 1), (0, 4, 0, 5), (2, 0, 3, 4)]
    );

    let mut wb_hdel = Workbook::new_blank();
    wb_hdel.sheets[0].merges = vec![(0, 2, 0, 3), (0, 4, 0, 5)].into();
    let h_del = CellShift::new((0, 2, 0, 3), Shift::Horizontal, false);
    assert_eq!(h_del.preflight(&wb_hdel, 0), Ok(()));
    assert!(h_del.apply(&mut wb_hdel, 0));
    assert_eq!(wb_hdel.sheets[0].merges.to_vec(), vec![(0, 2, 0, 3)]);
}

#[test]
fn preflight_table_conflict_and_non_overlapping_allowed() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].tables.push(table("Table1", (1, 1, 4, 3)));

    for op in [
        CellShift::new((0, 1, 0, 1), Shift::Vertical, true),
        CellShift::new((2, 2, 2, 2), Shift::Vertical, false),
        CellShift::new((2, 0, 2, 0), Shift::Horizontal, true),
        CellShift::new((3, 2, 3, 2), Shift::Horizontal, false),
    ] {
        assert_blocked(&mut wb, 0, op, CellShiftConflict::Table);
    }

    for op in [
        CellShift::new((5, 1, 5, 1), Shift::Vertical, true),
        CellShift::new((0, 0, 0, 0), Shift::Vertical, true),
        CellShift::new((0, 4, 0, 4), Shift::Vertical, false),
        CellShift::new((2, 4, 2, 4), Shift::Horizontal, true),
        CellShift::new((0, 0, 0, 0), Shift::Horizontal, true),
        CellShift::new((5, 0, 5, 0), Shift::Horizontal, false),
    ] {
        assert_eq!(op.preflight(&wb, 0), Ok(()));
        assert!(CellShiftPatch::prepare(&wb, 0, op).is_some());
    }
}

#[test]
fn preflight_pivot_conflict_all_three_stores_and_non_overlapping_allowed() {
    // 1. First-class wb.pivots (source on Sheet 0, output on Sheet 1)
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("PivotOut"));
    let src_id = wb.sheets[0].identity;
    let spec2 = PivotSpec2 {
        source: (0, 0, 3, 2),
        rows: vec![AxisField::plain(0)],
        cols: Vec::new(),
        values: vec![ValueField {
            col: 2,
            agg: Agg::Sum,
            label: None,
            show_as: ShowAs::Normal,
            base: None,
        }],
        filters: Vec::new(),
        layout: PivotLayout::default(),
    };
    let mut p = PivotTableModel::new("Pivot1".into(), "Sheet1".into(), Some(src_id), 1, (1, 4), spec2.clone());
    p.rect = (1, 4, 5, 6);
    wb.pivots.push(p);

    // Source ray blocked on Sheet 0
    assert_blocked(
        &mut wb,
        0,
        CellShift::new((0, 1, 0, 1), Shift::Vertical, true),
        CellShiftConflict::Pivot,
    );
    assert_blocked(
        &mut wb,
        0,
        CellShift::new((2, 0, 2, 0), Shift::Horizontal, false),
        CellShiftConflict::Pivot,
    );
    // Outside source ray on Sheet 0 allowed
    assert_eq!(CellShift::new((0, 3, 0, 3), Shift::Vertical, true).preflight(&wb, 0), Ok(()));
    assert_eq!(CellShift::new((4, 0, 4, 0), Shift::Vertical, true).preflight(&wb, 0), Ok(()));

    // Output ray blocked on Sheet 1
    assert_blocked(
        &mut wb,
        1,
        CellShift::new((0, 4, 0, 4), Shift::Vertical, true),
        CellShiftConflict::Pivot,
    );
    assert_blocked(
        &mut wb,
        1,
        CellShift::new((2, 0, 2, 0), Shift::Horizontal, true),
        CellShiftConflict::Pivot,
    );
    // Outside output ray on Sheet 1 allowed
    assert_eq!(CellShift::new((0, 0, 0, 0), Shift::Horizontal, true).preflight(&wb, 1), Ok(()));
    assert_eq!(CellShift::new((0, 3, 0, 3), Shift::Vertical, true).preflight(&wb, 1), Ok(()));
    assert_eq!(CellShift::new((6, 4, 6, 4), Shift::Vertical, false).preflight(&wb, 1), Ok(()));

    // 2. wb.pivots_pending
    let mut wb_p1 = Workbook::new_blank();
    wb_p1.sheets.push(Sheet::new("Other"));
    wb_p1.pivots_pending.push((
        0,
        PivotSpec {
            source: (0, 0, 2, 1),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        },
        PivotResult::default(),
        (4, 4, 7, 5),
        "P1".into(),
    ));
    assert_blocked(
        &mut wb_p1,
        0,
        CellShift::new((0, 0, 0, 0), Shift::Vertical, true),
        CellShiftConflict::Pivot,
    );
    assert_blocked(
        &mut wb_p1,
        0,
        CellShift::new((5, 0, 5, 0), Shift::Horizontal, true),
        CellShiftConflict::Pivot,
    );
    assert_eq!(CellShift::new((0, 2, 0, 2), Shift::Vertical, true).preflight(&wb_p1, 0), Ok(()));
    assert_eq!(CellShift::new((0, 0, 0, 0), Shift::Vertical, true).preflight(&wb_p1, 1), Ok(()));

    // 3. wb.pivots_pending2
    let mut wb_p2 = Workbook::new_blank();
    wb_p2.pivots_pending2.push((0, spec2, PivotGrid::default(), (5, 5, 8, 7)));
    assert_blocked(
        &mut wb_p2,
        0,
        CellShift::new((1, 2, 1, 2), Shift::Vertical, false),
        CellShiftConflict::Pivot,
    );
    assert_blocked(
        &mut wb_p2,
        0,
        CellShift::new((6, 0, 6, 0), Shift::Horizontal, true),
        CellShiftConflict::Pivot,
    );
    assert_eq!(CellShift::new((0, 4, 0, 4), Shift::Vertical, true).preflight(&wb_p2, 0), Ok(()));
}

#[test]
fn preflight_array_and_spill_conflict_and_scalar_allowed() {
    // CSE array
    let mut wb_cse = Workbook::new_blank();
    wb_cse.sheets[0].arrays.push((CellRef::new(1, 1), (1, 1, 2, 2)));
    assert_blocked(
        &mut wb_cse,
        0,
        CellShift::new((0, 1, 0, 1), Shift::Vertical, true),
        CellShiftConflict::ArrayOrSpill,
    );
    assert_blocked(
        &mut wb_cse,
        0,
        CellShift::new((2, 0, 2, 0), Shift::Horizontal, false),
        CellShiftConflict::ArrayOrSpill,
    );
    assert_eq!(CellShift::new((0, 0, 0, 0), Shift::Vertical, true).preflight(&wb_cse, 0), Ok(()));
    assert_eq!(CellShift::new((3, 1, 3, 1), Shift::Vertical, true).preflight(&wb_cse, 0), Ok(()));

    // Dynamic array spill_shapes (active 2x2 and blocked 0x0 anchor)
    let mut wb_dyn = Workbook::new_blank();
    wb_dyn.sheets[0].spill_shapes.insert(CellRef::new(1, 1), (2, 2));
    wb_dyn.sheets[0].spill_shapes.insert(CellRef::new(4, 4), (0, 0));
    assert_blocked(
        &mut wb_dyn,
        0,
        CellShift::new((0, 2, 0, 2), Shift::Vertical, true),
        CellShiftConflict::ArrayOrSpill,
    );
    assert_blocked(
        &mut wb_dyn,
        0,
        CellShift::new((4, 0, 4, 0), Shift::Horizontal, true),
        CellShiftConflict::ArrayOrSpill,
    );
    assert_eq!(CellShift::new((0, 3, 0, 3), Shift::Vertical, true).preflight(&wb_dyn, 0), Ok(()));

    // Spilled cells without spill_shapes (including formula anchor adjacent to spilled cell)
    let mut wb_spilled = Workbook::new_blank();
    wb_spilled.sheets[0].set(1, 2, formula("=SEQUENCE(2,1)"));
    wb_spilled.sheets[0].spilled.insert(CellRef::new(2, 2));
    assert_blocked(
        &mut wb_spilled,
        0,
        CellShift::new((1, 0, 1, 0), Shift::Horizontal, true),
        CellShiftConflict::ArrayOrSpill,
    );
    assert_blocked(
        &mut wb_spilled,
        0,
        CellShift::new((0, 2, 0, 2), Shift::Vertical, false),
        CellShiftConflict::ArrayOrSpill,
    );
    assert_eq!(
        CellShift::new((0, 0, 0, 0), Shift::Vertical, true).preflight(&wb_spilled, 0),
        Ok(())
    );

    // Ordinary scalar formula is unaffected and allowed
    let mut wb_scalar = Workbook::new_blank();
    wb_scalar.sheets[0].set(1, 1, formula("=1+2"));
    let op = CellShift::new((0, 1, 0, 1), Shift::Vertical, true);
    assert_eq!(op.preflight(&wb_scalar, 0), Ok(()));
    assert!(op.apply(&mut wb_scalar, 0));
    assert_eq!(wb_scalar.sheets[0].formula_of(2, 1), Some("=1+2"));
}

#[test]
fn preflight_autofilter_conflict_and_non_overlapping_allowed() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].filter = Some(autofilter((1, 1, 4, 3)));

    for op in [
        CellShift::new((0, 1, 0, 1), Shift::Vertical, true),
        CellShift::new((2, 2, 2, 2), Shift::Vertical, false),
        CellShift::new((2, 0, 2, 0), Shift::Horizontal, true),
        CellShift::new((3, 2, 3, 2), Shift::Horizontal, false),
    ] {
        assert_blocked(&mut wb, 0, op, CellShiftConflict::AutoFilter);
    }

    for op in [
        CellShift::new((0, 0, 0, 0), Shift::Vertical, true),
        CellShift::new((0, 4, 0, 4), Shift::Vertical, false),
        CellShift::new((5, 1, 5, 1), Shift::Vertical, true),
        CellShift::new((0, 0, 0, 0), Shift::Horizontal, true),
        CellShift::new((5, 0, 5, 0), Shift::Horizontal, false),
        CellShift::new((2, 4, 2, 4), Shift::Horizontal, true),
    ] {
        assert_eq!(op.preflight(&wb, 0), Ok(()));
        assert!(CellShiftPatch::prepare(&wb, 0, op).is_some());
    }
}

#[test]
fn room_shift_matches_rejects_late_added_unsupported_objects_in_ray() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(1, 0, num(42.0));
    let op = CellShift::new((0, 0, 0, 0), Shift::Vertical, true);
    let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    assert!(patch.apply(&mut wb, true));

    // Late AutoFilter intersecting ray blocks undo
    wb.sheets[0].filter = Some(autofilter((1, 0, 3, 1)));
    assert!(!patch.apply(&mut wb, false));
    // Moving AutoFilter outside col A allows undo
    wb.sheets[0].filter = Some(autofilter((1, 1, 3, 2)));
    assert!(patch.apply(&mut wb, false));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(42.0));

    // Late Table intersecting ray blocks redo
    wb.sheets[0].tables.push(table("LateTable", (2, 0, 4, 1)));
    assert!(!patch.apply(&mut wb, true));
    wb.sheets[0].tables.clear();
    assert!(patch.apply(&mut wb, true));
}

#[test]
fn room_shift_matches_rejects_late_partial_and_contained_merges_in_ray() {
    // Repro 1: late partial merge A1:B1 and boundary-straddling merge B1:B2 on undo
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, num(50.0));
    wb.sheets[0].set(0, 1, num(10.0));
    let op = CellShift::new((0, 1, 0, 1), Shift::Vertical, true);
    let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    assert!(patch.apply(&mut wb, true));
    assert_eq!(wb.sheets[0].value(0, 1), Value::Empty);
    assert_eq!(wb.sheets[0].value(1, 1), Value::Number(10.0));

    // Late partial merge A1:B1 must block undo instead of writing B1=10 into covered cell
    wb.sheets[0].merges.push((0, 0, 0, 1));
    assert!(!patch.apply(&mut wb, false));
    assert_eq!(wb.sheets[0].value(0, 1), Value::Empty);
    wb.sheets[0].merges.clear();

    // Repro 1b: boundary-straddling merge B1:B2 (with B1=Empty, B2=Empty, B3=10)
    let mut wb_straddle = Workbook::new_blank();
    wb_straddle.sheets[0].set(1, 1, num(10.0));
    let p_straddle = CellShiftPatch::prepare(&wb_straddle, 0, op).unwrap();
    assert!(p_straddle.apply(&mut wb_straddle, true));
    wb_straddle.sheets[0].merges.push((0, 1, 1, 1));
    assert!(!p_straddle.apply(&mut wb_straddle, false));
    wb_straddle.sheets[0].merges.clear();
    assert!(p_straddle.apply(&mut wb_straddle, false));
    assert_eq!(wb_straddle.sheets[0].value(1, 1), Value::Number(10.0));

    // Repro 2: late contained merge B5:B6 in the moving ray when patch.merges is empty
    let mut wb2 = Workbook::new_blank();
    wb2.sheets[0].set(3, 1, num(10.0));
    wb2.sheets[0].set(5, 1, num(99.0));
    let patch2 = CellShiftPatch::prepare(&wb2, 0, op).unwrap();
    assert!(patch2.merges.is_empty());
    assert!(patch2.apply(&mut wb2, true));
    // Now B5=10, B6=Empty, B7=99. Merge B5:B6 (4,1,5,1) without changing values.
    wb2.sheets[0].merges.push((4, 1, 5, 1));
    assert!(!patch2.apply(&mut wb2, false));
    assert_eq!(wb2.sheets[0].value(5, 1), Value::Empty);

    // Replacing it with an outside merge (col D) must allow undo and preserve the outside merge
    wb2.sheets[0].merges = vec![(4, 3, 5, 3)].into();
    assert!(patch2.apply(&mut wb2, false));
    assert_eq!(wb2.sheets[0].value(3, 1), Value::Number(10.0));
    assert_eq!(wb2.sheets[0].value(5, 1), Value::Number(99.0));
    assert_eq!(wb2.sheets[0].merges.to_vec(), vec![(4, 3, 5, 3)]);
}

#[test]
fn room_shift_matches_allows_identical_merge_chains_and_outside_merges_through_undo_redo() {
    // Insert chain: (1,0,2,0) + (3,0,4,0) shifted down by 2 -> (3,0,4,0) + (5,0,6,0)
    let mut wb_ins = Workbook::new_blank();
    wb_ins.sheets[0].merges = vec![(1, 0, 2, 0), (3, 0, 4, 0)].into();
    let op_ins = CellShift::new((1, 0, 2, 0), Shift::Vertical, true);
    let patch_ins = CellShiftPatch::prepare(&wb_ins, 0, op_ins).unwrap();
    assert!(patch_ins.apply(&mut wb_ins, true));
    assert_eq!(wb_ins.sheets[0].merges.to_vec(), vec![(3, 0, 4, 0), (5, 0, 6, 0)]);

    // Late merge inside the inserted gap (1,0,2,0) must block undo
    wb_ins.sheets[0].merges.push((1, 0, 2, 0));
    assert!(!patch_ins.apply(&mut wb_ins, false));
    wb_ins.sheets[0].merges.pop();

    // Outside merge added after insert is preserved through undo & redo
    wb_ins.sheets[0].merges.push((0, 4, 1, 4));
    assert!(patch_ins.apply(&mut wb_ins, false));
    assert_eq!(
        wb_ins.sheets[0].merges.to_vec(),
        vec![(1, 0, 2, 0), (3, 0, 4, 0), (0, 4, 1, 4)]
    );
    assert!(patch_ins.apply(&mut wb_ins, true));
    assert_eq!(
        wb_ins.sheets[0].merges.to_vec(),
        vec![(3, 0, 4, 0), (5, 0, 6, 0), (0, 4, 1, 4)]
    );

    // Delete chain: (1,0,2,0) deleted, (3,0,4,0) shifts up to (1,0,2,0)
    let mut wb_del = Workbook::new_blank();
    wb_del.sheets[0].merges = vec![(1, 0, 2, 0), (3, 0, 4, 0)].into();
    let op_del = CellShift::new((1, 0, 2, 0), Shift::Vertical, false);
    let patch_del = CellShiftPatch::prepare(&wb_del, 0, op_del).unwrap();
    assert!(patch_del.apply(&mut wb_del, true));
    assert_eq!(wb_del.sheets[0].merges.to_vec(), vec![(1, 0, 2, 0)]);

    // Late contained merge below in ray blocks undo of delete
    wb_del.sheets[0].merges.push((4, 0, 5, 0));
    assert!(!patch_del.apply(&mut wb_del, false));
    wb_del.sheets[0].merges.pop();

    // Outside merge added after delete is allowed and preserved on undo & redo
    wb_del.sheets[0].merges.push((1, 5, 2, 5));
    assert!(patch_del.apply(&mut wb_del, false));
    assert_eq!(
        wb_del.sheets[0].merges.to_vec(),
        vec![(1, 0, 2, 0), (3, 0, 4, 0), (1, 5, 2, 5)]
    );
    assert!(patch_del.apply(&mut wb_del, true));
    assert_eq!(wb_del.sheets[0].merges.to_vec(), vec![(1, 0, 2, 0), (1, 5, 2, 5)]);
}
