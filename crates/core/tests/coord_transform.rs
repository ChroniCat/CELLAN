use std::collections::HashSet;

use sheetcore::{
    Cell, Sheet, Value, Workbook,
    cell_shift_patch::{CellShiftOverlay, CellShiftPatch},
    cells::Shift,
    cf::{CfBlock, CfKind, CfOp, CfRule, Cfvo, CfvoKind},
    charts::{Chart, ChartKind, Series},
    external::{Kind, Source},
    hyperlinks::Hyperlink,
    pivot::{Agg, GroupBy, PivotResult, PivotSpec},
    pivot_engine::{PivotSlicerSpec, PivotTableModel},
    pivot_multi::{AxisField, FilterField, PivotGrid, PivotLayout, PivotSpec2, ShowAs, ValueField},
    protection::ProtectedRange,
    refs::Axis,
    scenarios::Scenario,
    sheet_ops::{delete_sheet, rename_sheet},
    sort_filter::Filter,
    sparkline::Sparkline,
    structural::{
        CellShift, CellShiftConflict, Structural, apply_structural_wb, delete_row_intervals_wb,
        move_cols_wb, move_rows_wb,
    },
    subtotal::{SubFn, SubtotalOpts, subtotal_workbook, try_subtotal_workbook_opts},
    table::Table,
    transform::CoordTransform,
};

fn num(v: f64) -> Cell {
    Cell {
        value: Value::Number(v),
        formula: None,
    }
}

fn formula_cell(f: &str) -> Cell {
    Cell {
        value: Value::Empty,
        formula: Some(f.into()),
    }
}

fn cfvo_formula(f: &str) -> Cfvo {
    Cfvo {
        kind: CfvoKind::Formula,
        val: 0.0,
        formula: f.into(),
    }
}

fn cfvo_block(min_f: &str, mid_f: &str, max_f: &str, bar_min: &str, bar_max: &str, icon_f: &str) -> CfBlock {
    CfBlock {
        ranges: vec![(0, 0, 5, 0)],
        rules: vec![
            CfRule {
                kind: CfKind::Scale {
                    points: vec![
                        (cfvo_formula(min_f), [255, 0, 0]),
                        (cfvo_formula(mid_f), [255, 255, 0]),
                        (cfvo_formula(max_f), [0, 255, 0]),
                    ],
                },
                stop: false,
                priority: 1,
            },
            CfRule {
                kind: CfKind::Bar {
                    color: [0, 120, 215],
                    neg_color: None,
                    min: cfvo_formula(bar_min),
                    max: cfvo_formula(bar_max),
                    show_value: true,
                },
                stop: false,
                priority: 2,
            },
            CfRule {
                kind: CfKind::IconSet {
                    set: "3Arrows".into(),
                    reverse: false,
                    thresholds: vec![Cfvo::min(), cfvo_formula(icon_f)],
                    show_value: true,
                },
                stop: false,
                priority: 3,
            },
            CfRule {
                kind: CfKind::Expr {
                    formula: format!("={}>0", min_f.trim_start_matches('=')),
                    dxf: 0,
                },
                stop: false,
                priority: 4,
            },
            CfRule {
                kind: CfKind::CellIs {
                    op: CfOp::Between,
                    a: min_f.into(),
                    b: max_f.into(),
                    dxf: 0,
                },
                stop: false,
                priority: 5,
            },
        ],
    }
}

fn collect_cf_formulas(block: &CfBlock) -> Vec<String> {
    let mut out = Vec::new();
    for rule in &block.rules {
        rule.kind.visit_formulas(|_, f| out.push(f.to_string()));
    }
    out
}

#[test]
fn coord_transform_geometry_and_inverse_all_variants() {
    let wb = Workbook::new_blank();

    // 1. Whole-axis AxisShift (Row insert & delete)
    let ins_row = CoordTransform::from_structural(0, &Structural::InsertRows { at: 3, n: 2 });
    assert_eq!(ins_row.target_sheet(), 0);
    assert_eq!(ins_row.axis(), Axis::Row);
    assert_eq!(ins_row.band(), None);
    assert_eq!(ins_row.preflight(&wb), Ok(()));
    assert_eq!(ins_row.map_index(2), Some(2));
    assert_eq!(ins_row.map_index(3), Some(5));
    assert_eq!(ins_row.map_cell(3, 4), Some((5, 4)));
    assert_eq!(ins_row.map_rect((1, 0, 4, 2)), Some((1, 0, 6, 2)));
    assert!(!ins_row.formula_may_be_affected("=A1+B2"));
    assert!(ins_row.formula_may_be_affected("=A1+B4"));
    assert_eq!(ins_row.rewrite_formula("=SUM(A2:A5)+$B$4", "Sheet1", "Sheet1"), "=SUM(A2:A7)+$B$6");
    let inv_row = ins_row.inverse().unwrap();
    for r in 0..20 {
        let mapped = ins_row.map_index(r).unwrap();
        assert_eq!(inv_row.map_index(mapped), Some(r));
    }

    // 2. Partial-band AxisShift (CellShift Vertical & Horizontal)
    let cs_v = CellShift::new((2, 1, 3, 2), Shift::Vertical, true);
    let band_v = CoordTransform::from_cell_shift(0, &cs_v);
    assert_eq!(band_v.axis(), Axis::Row);
    assert_eq!(band_v.band(), Some((1, 2)));
    assert_eq!(band_v.preflight(&wb), Ok(()));
    assert_eq!(band_v.map_cell(2, 0), Some((2, 0))); // outside band
    assert_eq!(band_v.map_cell(2, 1), Some((4, 1))); // inside band
    assert_eq!(band_v.map_cell(1, 1), Some((1, 1))); // before at
    assert_eq!(band_v.map_rect((2, 1, 4, 2)), Some((4, 1, 6, 2))); // wholly in band
    assert_eq!(band_v.map_rect((2, 0, 4, 2)), None); // crosses band boundary in moving ray
    assert_eq!(band_v.map_rect((0, 0, 1, 3)), Some((0, 0, 1, 3))); // crosses band but before at
    assert_eq!(band_v.map_rect((2, 3, 4, 4)), Some((2, 3, 4, 4))); // wholly outside band
    let inv_band_v = band_v.inverse().unwrap();
    for r in 0..10 {
        for c in 0..5 {
            let (nr, nc) = band_v.map_cell(r, c).unwrap();
            assert_eq!(inv_band_v.map_cell(nr, nc), Some((r, c)));
        }
    }

    let cs_h = CellShift::new((1, 2, 2, 3), Shift::Horizontal, false);
    let band_h = CoordTransform::from_cell_shift(0, &cs_h);
    assert_eq!(band_h.axis(), Axis::Col);
    assert_eq!(band_h.band(), Some((1, 2)));
    assert_eq!(band_h.map_cell(0, 2), Some((0, 2)));
    assert_eq!(band_h.map_cell(1, 2), None);
    assert_eq!(band_h.map_cell(1, 4), Some((1, 2)));
    assert_eq!(band_h.rewrite_formula("=E2+E1+C2", "Sheet1", "Sheet1"), "=C2+E1+#REF!");

    // 3. AxisPermute (forward & backward moves, bijective inverse & contiguous range)
    let perm_fwd = CoordTransform::from_axis_move(0, Axis::Row, 1, 2, 6).unwrap();
    assert_eq!(
        perm_fwd,
        CoordTransform::AxisPermute {
            sheet_idx: 0,
            axis: Axis::Row,
            from: 1,
            n: 2,
            at: 4,
        }
    );
    let perm_inv = perm_fwd.inverse().unwrap();
    for r in 0..20 {
        let nr = perm_fwd.map_index(r).unwrap();
        assert_eq!(perm_inv.map_index(nr), Some(r), "inverse failed for row {r}");
    }
    assert_eq!(perm_fwd.map_rect((1, 0, 2, 3)), Some((4, 0, 5, 3))); // moved block
    assert_eq!(perm_fwd.map_rect((3, 0, 5, 3)), Some((1, 0, 3, 3))); // displaced block
    assert_eq!(perm_fwd.map_rect((0, 0, 6, 3)), Some((0, 0, 6, 3))); // encloses perm band
    assert_eq!(perm_fwd.map_rect((0, 0, 1, 3)), None); // split non-contiguously
    assert_eq!(perm_fwd.map_rect((7, 0, 9, 3)), Some((7, 0, 9, 3))); // outside perm band
    assert_eq!(perm_fwd.rewrite_formula("=A2+A4+A8", "Sheet1", "Sheet1"), "=A5+A2+A8");

    // 4. MultiInsert (unsorted + duplicate positions)
    let mi = CoordTransform::from_multi_insert(0, Axis::Row, vec![5, 2, 5]);
    assert_eq!(mi.map_index(1), Some(1));
    assert_eq!(mi.map_index(2), Some(3));
    assert_eq!(mi.map_index(4), Some(5));
    assert_eq!(mi.map_index(5), Some(8));
    assert_eq!(mi.map_rect((2, 0, 5, 1)), Some((3, 0, 8, 1)));
    assert_eq!(mi.rewrite_formula("=SUM(A3:A6)+A2", "Sheet1", "Sheet1"), "=SUM(A4:A9)+A2");
    assert_eq!(mi.inverse(), None); // non-contiguous multi-insert has no single AxisShift inverse

    let mi_single = CoordTransform::from_multi_insert(0, Axis::Col, vec![3, 3]);
    assert_eq!(
        mi_single.inverse(),
        Some(CoordTransform::AxisShift {
            sheet_idx: 0,
            axis: Axis::Col,
            at: 3,
            n: 2,
            delete: true,
            band: None,
        })
    );
}

#[test]
fn cf_cfvo_formula_rewritten_across_all_transforms_and_sheet_ops() {
    let make_wb = || {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "Data".into();
        wb.sheets[0].cf.blocks.push(cfvo_block(
            "=$B$3",
            "=$B$5",
            "=$B$7",
            "=$B$3",
            "=$B$7",
            "=$B$5",
        ));
        let mut other = Sheet::new("Summary");
        other.cf.blocks.push(cfvo_block(
            "=Data!$B$3",
            "=Data!$B$5",
            "=Data!$B$7",
            "=Data!$B$3",
            "=Data!$B$7",
            "=Data!$B$5",
        ));
        wb.sheets.push(other);
        wb
    };

    // 1. Structural InsertRows & DeleteCols
    {
        let mut wb = make_wb();
        apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 2, n: 2 });
        assert!(wb.sheets[0].cf_dirty && wb.sheets[1].cf_dirty);
        assert_eq!(
            collect_cf_formulas(&wb.sheets[0].cf.blocks[0]),
            vec!["=$B$5", "=$B$7", "=$B$9", "=$B$5", "=$B$9", "=$B$7", "=$B$5>0", "=$B$5", "=$B$9"]
        );
        assert_eq!(
            collect_cf_formulas(&wb.sheets[1].cf.blocks[0]),
            vec![
                "=Data!$B$5", "=Data!$B$7", "=Data!$B$9",
                "=Data!$B$5", "=Data!$B$9", "=Data!$B$7",
                "=Data!$B$5>0", "=Data!$B$5", "=Data!$B$9"
            ]
        );
        wb.sheets[0].cf_dirty = false;
        wb.sheets[1].cf_dirty = false;
        apply_structural_wb(&mut wb, 0, &Structural::InsertCols { at: 0, n: 1 });
        assert!(wb.sheets[0].cf_dirty && wb.sheets[1].cf_dirty);
        assert_eq!(collect_cf_formulas(&wb.sheets[1].cf.blocks[0])[0], "=Data!$C$5");
    }

    // 2. AxisPermute (move_rows_wb & move_cols_wb)
    {
        let mut wb = make_wb();
        // Move row 2 (0-based, i.e. row 3 in A1: $B$3) down past row 7 (drop=7 -> at=6 -> $B$7).
        // Old rows 4..=7 shift up by 1 ($B$5 -> $B$4, $B$7 -> $B$6).
        move_rows_wb(&mut wb, 0, 2, 1, 7);
        assert!(wb.sheets[0].cf_dirty && wb.sheets[1].cf_dirty);
        assert_eq!(
            collect_cf_formulas(&wb.sheets[0].cf.blocks[0])[..6],
            ["=$B$7", "=$B$4", "=$B$6", "=$B$7", "=$B$6", "=$B$4"]
        );
        assert_eq!(
            collect_cf_formulas(&wb.sheets[1].cf.blocks[0])[..6],
            ["=Data!$B$7", "=Data!$B$4", "=Data!$B$6", "=Data!$B$7", "=Data!$B$6", "=Data!$B$4"]
        );

        wb.sheets[0].cf_dirty = false;
        wb.sheets[1].cf_dirty = false;
        // Move col B (1) to drop 4 (at=3 -> col D)
        move_cols_wb(&mut wb, 0, 1, 1, 4);
        assert!(wb.sheets[0].cf_dirty && wb.sheets[1].cf_dirty);
        assert_eq!(collect_cf_formulas(&wb.sheets[0].cf.blocks[0])[0], "=$D$7");
        assert_eq!(collect_cf_formulas(&wb.sheets[1].cf.blocks[0])[0], "=Data!$D$7");
    }

    // 3. CellShift (apply + CellShiftPatch + CellShiftOverlay)
    {
        let mut wb = make_wb();
        let op = CellShift::new((1, 1, 2, 1), Shift::Vertical, true);
        let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
        let mut via_apply = wb.clone();
        assert!(op.apply(&mut via_apply, 0));
        assert!(patch.apply(&mut wb, true));
        assert_eq!(wb.sheets[0].cf, via_apply.sheets[0].cf);
        assert_eq!(wb.sheets[1].cf, via_apply.sheets[1].cf);
        assert_eq!(
            collect_cf_formulas(&wb.sheets[1].cf.blocks[0])[..6],
            ["=Data!$B$5", "=Data!$B$7", "=Data!$B$9", "=Data!$B$5", "=Data!$B$9", "=Data!$B$7"]
        );

        let overlay = CellShiftOverlay::capture(&wb);
        let mut guest = make_wb();
        assert!(overlay.apply_to(&mut guest));
        assert_eq!(guest.sheets[0].cf, wb.sheets[0].cf);
        assert_eq!(guest.sheets[1].cf, wb.sheets[1].cf);

        // Undo via patch restores original CFVO formulas cleanly
        assert!(patch.apply(&mut wb, false));
        assert_eq!(collect_cf_formulas(&wb.sheets[1].cf.blocks[0])[0], "=Data!$B$3");
    }

    // 4. MultiInsert (subtotal_workbook)
    {
        let mut wb = make_wb();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("Dept".into()), formula: None });
        wb.sheets[0].set(0, 1, Cell { value: Value::Text("Amt".into()), formula: None });
        wb.sheets[0].set(1, 0, Cell { value: Value::Text("A".into()), formula: None });
        wb.sheets[0].set(1, 1, num(10.0));
        wb.sheets[0].set(2, 0, Cell { value: Value::Text("B".into()), formula: None });
        wb.sheets[0].set(2, 1, num(20.0));
        // Inserts 1 subtotal row after row 1 (at=2), 1 after row 2 (at=3), and grand total (at=3):
        // original row 2 ($B$3) shifts by +1 (to $B$4), rows 4/6 ($B$5/$B$7) shift by +3 (to $B$8/$B$10).
        let report = subtotal_workbook(&mut wb, 0, (0, 0, 2, 1), 0, &[1], SubFn::Sum);
        assert_eq!(report.rows, 3);
        assert_eq!(
            collect_cf_formulas(&wb.sheets[1].cf.blocks[0])[..6],
            ["=Data!$B$4", "=Data!$B$8", "=Data!$B$10", "=Data!$B$4", "=Data!$B$10", "=Data!$B$8"]
        );
    }

    // 5. Multi-interval delete (delete_row_intervals_wb)
    {
        let mut wb = make_wb();
        // Delete rows 0..=0 and 3..=3: row 2 ($B$3) -> $B$2; row 4 ($B$5) -> $B$3; row 6 ($B$7) -> $B$5
        delete_row_intervals_wb(&mut wb, 0, &[(0, 0), (3, 3)]);
        assert_eq!(
            collect_cf_formulas(&wb.sheets[1].cf.blocks[0])[..6],
            ["=Data!$B$2", "=Data!$B$3", "=Data!$B$5", "=Data!$B$2", "=Data!$B$5", "=Data!$B$3"]
        );
    }

    // 6. Sheet rename / delete also updates CFVO formulas
    {
        let mut wb = make_wb();
        assert!(rename_sheet(&mut wb, 0, "Sales Data"));
        assert_eq!(collect_cf_formulas(&wb.sheets[1].cf.blocks[0])[0], "='Sales Data'!$B$3");
        assert!(delete_sheet(&mut wb, 0));
        assert_eq!(collect_cf_formulas(&wb.sheets[0].cf.blocks[0])[0], "=#REF!");
    }
}

#[test]
fn axis_permute_updates_charts_sparklines_pivots_names_sources_and_sheet_metadata() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let data_id = wb.sheets[0].identity;

    // Seed target sheet with rich coordinate-bound metadata
    {
        let s = &mut wb.sheets[0];
        for r in 0..8u32 {
            s.set(r, 0, Cell { value: Value::Text(format!("K{r}").into()), formula: None });
            s.set(r, 1, num((r + 1) as f64 * 10.0));
            s.set(r, 2, num((r + 1) as f64));
        }
        // Target chart referencing Data!$B$2:$B$3 and bubble size Data!$C$2:$C$3
        let mut ch = Chart {
            kind: ChartKind::Bubble,
            anchor: (1, 5, 2, 8),
            source_range: "Data!$A$2:$C$3".into(),
            series: vec![Series {
                name_ref: "Data!$B$1".into(),
                cat_ref: "Data!$A$2:$A$3".into(),
                val_ref: "Data!$B$2:$B$3".into(),
                size_ref: "Data!$C$2:$C$3".into(),
                ..Series::default()
            }],
            ..Chart::default()
        };
        ch.ref_fixes.clear();
        s.charts.push(ch);
        // Same-sheet sparklines (both source_sheet: None and Some("data"))
        s.sparklines.push(Sparkline {
            r: 1,
            c: 4,
            source_sheet: None,
            rect: (1, 1, 2, 1),
            ..Default::default()
        });
        s.sparklines.push(Sparkline {
            r: 7,
            c: 4,
            source_sheet: Some("data".into()),
            rect: (1, 1, 2, 1),
            ..Default::default()
        });
        // Protected range inside moved block + straddling range
        s.protected_ranges.push(ProtectedRange::new("P1", vec![(1, 0, 2, 2), (0, 0, 2, 0)], ""));
        // Ignored errors inside moved block
        s.ignored_errors.insert((1, 1));
        s.ignored_errors.insert((7, 1));
        // Scenario inside moved block
        let mut sc = Scenario::new("Base");
        sc.cells.push(((1, 1), "99".into()));
        sc.cells.push(((7, 1), "88".into()));
        s.scenarios.push(sc);
        s.scenario_settings.sqref = Some("$B$2:$B$3".into());
        // Internal hyperlinks (inside moved block & outside pointing into moved block)
        s.hyperlinks.insert(
            (1, 0),
            Hyperlink {
                location: "#Data!$B$2".into(),
                ..Default::default()
            },
        );
        s.hyperlinks.insert(
            (7, 0),
            Hyperlink {
                location: "B2:B3".into(),
                ..Default::default()
            },
        );
        // Hidden rows: keep filter hidden_rows vs manual hidden_rows_user distinct!
        s.hidden_rows.insert(1);
        s.hidden_rows_user.insert(2);
    }

    // Foreign sheet with chart & cross-sheet sparklines (case-insensitive sheet match!)
    let mut foreign = Sheet::new("Report");
    foreign.charts.push(Chart {
        kind: ChartKind::Column,
        source_range: "Data!$A$2:$B$3".into(),
        series: vec![Series {
            name_ref: "Data!$B$1".into(),
            cat_ref: "Data!$A$2:$A$3".into(),
            val_ref: "Data!$B$2:$B$3".into(),
            ..Series::default()
        }],
        ..Chart::default()
    });
    // Contiguous cross-sheet sparkline (lowercase "data")
    foreign.sparklines.push(Sparkline {
        r: 0,
        c: 0,
        source_sheet: Some("data".into()),
        rect: (1, 1, 2, 1),
        ..Default::default()
    });
    // Non-contiguous split sparkline: covers rows 0..=2 while rows 1..=2 move to 5..=6 -> split!
    foreign.sparklines.push(Sparkline {
        r: 1,
        c: 0,
        source_sheet: Some("DATA".into()),
        rect: (0, 1, 2, 1),
        ..Default::default()
    });
    wb.sheets.push(foreign);

    // Workbook names & sources
    wb.names.push(("MovedVals".into(), None, "Data!$B$2:$B$3".into()));
    wb.sources.push(Source {
        name: "src1".into(),
        kind: Kind::Internal {
            sheet: "Data".into(),
            rect: (1, 0, 2, 2),
        },
        at: (0, 0),
        target_sheet: Some("Report".into()),
        rows: 2,
        cols: 3,
        csv_opts: None,
    });

    // Pivots: wb.pivots (intact & split), wb.pivots_pending, wb.pivots_pending2
    let spec2_intact = PivotSpec2 {
        source: (1, 0, 2, 2),
        rows: vec![AxisField::plain(0)],
        cols: vec![AxisField::plain(1)],
        values: vec![ValueField {
            col: 2,
            agg: Agg::Sum,
            label: None,
            show_as: ShowAs::Normal,
            base: Some((1, "x".into())),
        }],
        filters: vec![FilterField { col: 1, keep: Vec::new() }],
        layout: PivotLayout::default(),
    };
    let mut p_intact = PivotTableModel::new("PIntact".into(), "Data".into(), Some(data_id), 0, (1, 10), spec2_intact.clone());
    p_intact.rect = (1, 10, 2, 12);
    p_intact.options.slicers.push(PivotSlicerSpec {
        id: 1,
        col: 2,
        title: "S".into(),
        pos: [0.0, 0.0],
        width: 100.0,
        columns: 1,
    });
    wb.pivots.push(p_intact);

    let mut p_split = PivotTableModel::new(
        "PSplit".into(),
        "Data".into(),
        Some(data_id),
        1,
        (0, 0),
        PivotSpec2 {
            source: (0, 0, 2, 2), // rows 0..=2 split when rows 1..=2 move to 5..=6!
            ..spec2_intact.clone()
        },
    );
    p_split.rect = (0, 0, 3, 2);
    p_split.imported_part = Some("xl/pivotTables/pivotTable9.xml".into());
    wb.pivots.push(p_split);

    wb.pivots_pending.push((
        0,
        PivotSpec {
            source: (1, 0, 2, 2),
            row_field: 0,
            col_field: Some(1),
            value_field: 2,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        },
        PivotResult::default(),
        (1, 14, 2, 15),
        "Pending1".into(),
    ));
    wb.pivots_pending2.push((0, spec2_intact, PivotGrid::default(), (1, 17, 2, 18)));

    // Move rows 1..=2 (2 rows) to drop 7 -> final at = 5 (rows 5..=6)!
    move_rows_wb(&mut wb, 0, 1, 2, 7);

    // Charts updated on BOTH target & foreign sheets with ref_fixes!
    let target_ch = &wb.sheets[0].charts[0];
    assert!(wb.sheets[0].charts_dirty);
    assert_eq!(target_ch.anchor, (5, 5, 6, 8));
    assert_eq!(target_ch.source_range, "Data!$A$6:$C$7");
    assert_eq!(target_ch.series[0].name_ref, "Data!$B$1");
    assert_eq!(target_ch.series[0].cat_ref, "Data!$A$6:$A$7");
    assert_eq!(target_ch.series[0].val_ref, "Data!$B$6:$B$7");
    assert_eq!(target_ch.series[0].size_ref, "Data!$C$6:$C$7");
    assert_eq!(target_ch.ref_fixes.len(), 3);

    let foreign_ch = &wb.sheets[1].charts[0];
    assert!(wb.sheets[1].charts_dirty);
    assert_eq!(foreign_ch.source_range, "Data!$A$6:$B$7");
    assert_eq!(foreign_ch.series[0].val_ref, "Data!$B$6:$B$7");
    assert_eq!(foreign_ch.ref_fixes.len(), 2);

    // Sparklines updated on BOTH target (None & Some) and foreign (case-insensitive) + split -> #REF!
    assert!(wb.sheets[0].sparklines_dirty);
    assert_eq!((wb.sheets[0].sparklines[0].r, wb.sheets[0].sparklines[0].c), (5, 4));
    assert_eq!(wb.sheets[0].sparklines[0].rect, (5, 1, 6, 1));
    assert_eq!(wb.sheets[0].sparklines[1].rect, (5, 1, 6, 1));
    assert!(wb.sheets[1].sparklines_dirty);
    assert_eq!(wb.sheets[1].sparklines[0].rect, (5, 1, 6, 1));
    assert_eq!(wb.sheets[1].sparklines[1].data_ref("Report"), "#REF!");

    // Pivots updated cleanly; split pivot removed and recorded in pivots_removed_parts!
    assert_eq!(wb.pivots.len(), 1);
    assert_eq!(wb.pivots[0].spec.source, (5, 0, 6, 2));
    assert_eq!(wb.pivots[0].rect, (5, 10, 6, 12));
    assert_eq!(wb.pivots[0].anchor, (5, 10));
    assert_eq!(wb.pivots[0].last_meta.rect, (5, 10, 6, 12));
    assert_eq!(wb.pivots_removed_parts, vec!["xl/pivotTables/pivotTable9.xml".to_string()]);
    assert_eq!(wb.pivots_pending[0].1.source, (5, 0, 6, 2));
    assert_eq!(wb.pivots_pending[0].3, (5, 14, 6, 15));
    assert_eq!(wb.pivots_pending2[0].1.source, (5, 0, 6, 2));
    assert_eq!(wb.pivots_pending2[0].3, (5, 17, 6, 18));

    // Names & sources
    assert_eq!(wb.names[0].2, "Data!$B$6:$B$7");
    assert!(matches!(&wb.sources[0].kind, Kind::Internal { rect: (5, 0, 6, 2), .. }));

    // Target-sheet metadata preserved without data loss
    let s = &wb.sheets[0];
    assert!(s.protected_ranges_dirty);
    assert_eq!(s.protected_ranges[0].sqref, vec![(5, 0, 6, 2), (0, 0, 0, 0), (5, 0, 6, 0)]);
    assert!(s.ignored_errors_dirty);
    assert!(s.ignored_errors.contains(&(5, 1)) && s.ignored_errors.contains(&(7, 1)));
    assert!(!s.ignored_errors.contains(&(1, 1)));
    assert!(s.scenarios_dirty);
    assert_eq!(s.scenarios[0].cells, vec![((5, 1), "99".into()), ((7, 1), "88".into())]);
    assert_eq!(s.scenario_settings.sqref.as_deref(), Some("$B$6:$B$7"));
    assert!(s.hyperlinks_dirty);
    assert_eq!(s.hyperlinks[&(5, 0)].location, "#Data!$B$6");
    assert_eq!(s.hyperlinks[&(7, 0)].location, "B6:B7");
    assert!(s.hidden_rows.contains(&5) && !s.hidden_rows.contains(&1));
    assert!(s.hidden_rows_user.contains(&6) && !s.hidden_rows_user.contains(&5));

    // Now test Axis::Col permutation inside the pivot source (swap col 0 and col 2 via move col 0 to drop 3 -> at=2)!
    wb.sheets[0].tables.push(Table {
        name: "T1".into(),
        display: "T1".into(),
        range: (5, 0, 6, 2),
        columns: vec!["C0".into(), "C1".into(), "C2".into()],
        ..Table::default()
    });
    let mut flt = Filter {
        range: (5, 0, 6, 2),
        allows: Default::default(),
        criteria: Default::default(),
        date_groups: Default::default(),
        color_filters: Default::default(),
        top_filters: Default::default(),
        dynamic_filters: Default::default(),
    };
    flt.allows.insert(0, HashSet::from(["K1".into()]));
    wb.sheets[0].filter = Some(flt);

    move_cols_wb(&mut wb, 0, 0, 1, 3);
    // Col 0 -> 2, Col 1 -> 0, Col 2 -> 1! Source (5,0,6,2) stays contiguous (5,0,6,2)!
    assert_eq!(wb.pivots[0].spec.source, (5, 0, 6, 2));
    assert_eq!(wb.pivots[0].spec.rows[0].col, 2);
    assert_eq!(wb.pivots[0].spec.cols[0].col, 0);
    assert_eq!(wb.pivots[0].spec.values[0].col, 1);
    assert_eq!(wb.pivots[0].spec.values[0].base.as_ref().map(|(c, _)| *c), Some(0));
    assert_eq!(wb.pivots[0].spec.filters[0].col, 0);
    assert_eq!(wb.pivots[0].options.slicers[0].col, 1);
    assert_eq!(wb.pivots_pending[0].1.row_field, 2);
    assert_eq!(wb.pivots_pending[0].1.col_field, Some(0));
    assert_eq!(wb.pivots_pending[0].1.value_field, 1);
    assert_eq!(wb.pivots_pending2[0].1.rows[0].col, 2);
    assert_eq!(wb.pivots_pending2[0].1.values[0].col, 1);
    assert_eq!(wb.sheets[0].tables[0].columns, vec!["C1", "C2", "C0"]);
    assert!(wb.sheets[0].filter.as_ref().unwrap().allows.contains_key(&2));
}

#[test]
fn cell_shift_parity_apply_patch_overlay_for_cfvo_hyperlinks_and_ignored_errors() {
    let make_wb = || {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "Data".into();
        wb.sheets[0].set(1, 1, num(42.0));
        // Hyperlink inside moving ray AND outside ray pointing into moving ray
        wb.sheets[0].hyperlinks.insert(
            (1, 1),
            Hyperlink {
                location: "#Data!$B$2".into(),
                ..Default::default()
            },
        );
        wb.sheets[0].hyperlinks.insert(
            (8, 5),
            Hyperlink {
                location: "B2".into(),
                ..Default::default()
            },
        );
        // Ignored errors inside and outside moving ray
        wb.sheets[0].ignored_errors.insert((1, 1));
        wb.sheets[0].ignored_errors.insert((8, 5));
        // CFVO Formula thresholds on both target and foreign sheets
        wb.sheets[0].cf.blocks.push(cfvo_block(
            "=$B$2", "=$B$2", "=$B$2", "=$B$2", "=$B$2", "=$B$2",
        ));
        let mut other = Sheet::new("Summary");
        other.cf.blocks.push(cfvo_block(
            "=Data!$B$2", "=Data!$B$2", "=Data!$B$2", "=Data!$B$2", "=Data!$B$2", "=Data!$B$2",
        ));
        wb.sheets.push(other);
        wb
    };

    let op = CellShift::new((0, 1, 1, 1), Shift::Vertical, true);
    let mut via_apply = make_wb();
    assert!(op.apply(&mut via_apply, 0));

    let mut via_patch = make_wb();
    let baseline = CellShiftOverlay::capture(&via_patch);
    let patch = CellShiftPatch::prepare(&via_patch, 0, op).unwrap();
    let wire = serde_json::to_string(&patch).unwrap();
    let patch: CellShiftPatch = serde_json::from_str(&wire).unwrap();
    assert!(patch.wire_valid());
    assert_eq!(patch.canonical_forward(&via_patch).unwrap(), patch);
    assert!(patch.apply(&mut via_patch, true));

    // 100% parity between CellShift::apply and CellShiftPatch::apply
    assert_eq!(via_patch.sheets[0].hyperlinks, via_apply.sheets[0].hyperlinks);
    assert_eq!(via_patch.sheets[0].ignored_errors, via_apply.sheets[0].ignored_errors);
    assert_eq!(via_patch.sheets[0].cf, via_apply.sheets[0].cf);
    assert_eq!(via_patch.sheets[1].cf, via_apply.sheets[1].cf);
    assert_eq!(via_patch.sheets[0].hyperlinks[&(3, 1)].location, "#Data!$B$4");
    assert_eq!(via_patch.sheets[0].hyperlinks[&(8, 5)].location, "B4");
    assert!(via_patch.sheets[0].ignored_errors.contains(&(3, 1)));
    assert!(!via_patch.sheets[0].ignored_errors.contains(&(1, 1)));
    assert!(via_patch.sheets[0].ignored_errors.contains(&(8, 5)));
    assert_eq!(collect_cf_formulas(&via_patch.sheets[1].cf.blocks[0])[0], "=Data!$B$4");

    // CellShiftOverlay roundtrip parity
    let overlay = CellShiftOverlay::capture(&via_patch);
    let overlay_wire = serde_json::to_string(&overlay).unwrap();
    let overlay: CellShiftOverlay = serde_json::from_str(&overlay_wire).unwrap();
    assert!(overlay.wire_valid());
    let mut guest = make_wb();
    assert!(overlay.apply_to(&mut guest));
    assert_eq!(guest.sheets[0].hyperlinks, via_patch.sheets[0].hyperlinks);
    assert_eq!(guest.sheets[0].ignored_errors, via_patch.sheets[0].ignored_errors);
    assert_eq!(guest.sheets[0].cf, via_patch.sheets[0].cf);
    assert_eq!(guest.sheets[1].cf, via_patch.sheets[1].cf);

    // Late-added target hyperlink with location targeting moving ray blocks undo!
    via_patch.sheets[0].hyperlinks.insert(
        (9, 9),
        Hyperlink {
            location: "Data!B4".into(),
            ..Default::default()
        },
    );
    assert!(!patch.apply(&mut via_patch, false));
    via_patch.sheets[0].hyperlinks.remove(&(9, 9));

    // Late-added ignored error in moving ray blocks undo!
    via_patch.sheets[0].ignored_errors.insert((5, 1));
    assert!(!patch.apply(&mut via_patch, false));
    via_patch.sheets[0].ignored_errors.remove_if_present(&(5, 1));

    // Clean undo restores exact baseline
    assert!(patch.apply(&mut via_patch, false));
    assert_eq!(CellShiftOverlay::capture(&via_patch), baseline);
}

#[test]
fn large_sheet_cow_sharing_and_prefilter_preserved_across_transforms() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Big".into();
    // Populate page 0 (rows 0..10) and page 2 (rows 600..610)
    for r in 0..10u32 {
        wb.sheets[0].set(r, 0, num(r as f64));
        wb.sheets[0].set(r, 1, formula_cell(&format!("=A{}", r + 1)));
    }
    for r in 600..610u32 {
        wb.sheets[0].set(r, 0, num(r as f64));
        wb.sheets[0].set(r, 1, formula_cell(&format!("=A{}", r + 1)));
    }
    let before = wb.sheets[0].clone();
    // Moving rows inside page 2 must leave page 0 completely CoW-shared!
    move_rows_wb(&mut wb, 0, 600, 2, 605);
    assert!(wb.sheets[0].same_page(&before, 0), "page 0 should remain CoW-shared on high-row AxisPermute");
    assert!(!wb.sheets[0].same_page(&before, 2));
    assert_eq!(wb.sheets[0].formula_of(603, 1), Some("=A604"));

    // Inserting rows in page 2 must also leave page 0 CoW-shared!
    let before_ins = wb.sheets[0].clone();
    apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 600, n: 3 });
    assert!(wb.sheets[0].same_page(&before_ins, 0), "page 0 should remain CoW-shared on high-row InsertRows");

    // Preflight on CoordTransform rejects invalid targets cleanly
    assert_eq!(
        CoordTransform::from_structural(9, &Structural::InsertRows { at: 0, n: 1 }).preflight(&wb),
        Err(CellShiftConflict::InvalidTarget)
    );
}

#[test]
fn cell_shift_patch_blocks_late_hyperlink_targeting_deleted_or_band_crossing_region() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let op = CellShift::new((1, 1, 1, 1), Shift::Vertical, true);
    let patch = CellShiftPatch::prepare(&wb, 0, op).unwrap();
    assert!(patch.apply(&mut wb, true));

    // Late-added hyperlinks outside col B pointing into the deleted cell (B2) or
    // crossing the moving band on undo MUST block undo without mutating wb.
    for blocked_loc in ["Data!B2", "#Data!$B$2", "B2", "Data!B2:B2", "Data!A2:C2", "Data!B1:B3"] {
        wb.sheets[0].hyperlinks.insert(
            (8, 5),
            Hyperlink {
                location: blocked_loc.into(),
                ..Default::default()
            },
        );
        let before = wb.clone();
        assert!(
            !patch.apply(&mut wb, false),
            "late hyperlink `{blocked_loc}` must block CellShiftPatch undo"
        );
        assert_eq!(wb.sheets[0].hyperlinks, before.sheets[0].hyperlinks);
        wb.sheets[0].hyperlinks.remove(&(8, 5));
    }

    // Unrelated locations (different column, different sheet, or strictly above `at`)
    // must still allow undo cleanly.
    for allowed_loc in ["Data!E10", "OtherSheet!B2", "Data!A1:C1"] {
        let mut copy = wb.clone();
        copy.sheets[0].hyperlinks.insert(
            (8, 5),
            Hyperlink {
                location: allowed_loc.into(),
                ..Default::default()
            },
        );
        assert!(
            patch.apply(&mut copy, false),
            "unrelated hyperlink `{allowed_loc}` should allow CellShiftPatch undo"
        );
        assert_eq!(copy.sheets[0].hyperlinks[&(8, 5)].location, allowed_loc);
    }
}

#[test]
fn nested_single_group_subtotal_with_intersecting_table_keeps_cells_and_refs_in_sync() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    {
        let s = &mut wb.sheets[0];
        s.set(0, 0, Cell { value: Value::Text("Dept".into()), formula: None });
        s.set(0, 1, Cell { value: Value::Text("Amt".into()), formula: None });
        s.set(0, 2, Cell { value: Value::Text("Double".into()), formula: None });
        s.set(1, 0, Cell { value: Value::Text("A".into()), formula: None });
        s.set(1, 1, num(10.0));
        s.set(1, 2, formula_cell("=[@Amt]*2"));
        s.set(2, 0, Cell { value: Value::Text("A".into()), formula: None });
        s.set(2, 1, num(20.0));
        s.set(2, 2, formula_cell("=[@Amt]*2"));
        // Sentinel row immediately below the subtotal region (original row 3 = A4:B4)
        s.set(3, 0, Cell { value: Value::Text("After".into()), formula: None });
        s.set(3, 1, num(999.0));
        // Existing outline level makes `replace_existing: false` a nested subtotal (no grand total),
        // so 1 group produces `positions = [3]` (len == 1).
        s.row_levels.insert(1, 1);
        s.row_levels.insert(2, 1);
        s.tables.push(Table {
            name: "TSub".into(),
            display: "TSub".into(),
            range: (0, 0, 2, 2),
            columns: vec!["Dept".into(), "Amt".into(), "Double".into()],
            ..Table::default()
        });
    }
    let mut summary = Sheet::new("Summary");
    summary.set(0, 0, formula_cell("=Data!B4"));
    summary.cf.blocks.push(cfvo_block(
        "=Data!$B$4",
        "=Data!$B$4",
        "=Data!$B$4",
        "=Data!$B$4",
        "=Data!$B$4",
        "=Data!$B$4",
    ));
    wb.sheets.push(summary);
    wb.names.push(("SentinelAmt".into(), None, "Data!$B$4".into()));

    let report = try_subtotal_workbook_opts(
        &mut wb,
        0,
        (0, 0, 2, 1),
        0,
        &[1],
        SubFn::Sum,
        SubtotalOpts {
            replace_existing: false,
            ..Default::default()
        },
    )
    .expect("nested single-group subtotal should succeed");

    assert_eq!(report.rows, 1, "must insert exactly 1 subtotal row");
    assert_eq!(wb.sheets[0].value(3, 0).display(), "A 汇总");
    assert_eq!(wb.sheets[0].formula_of(3, 1), Some("=SUBTOTAL(9,B2:B3)"));
    assert_eq!(wb.sheets[0].formula_of(3, 2), Some("=[@Amt]*2"));
    assert_eq!(wb.sheets[0].value(4, 0).display(), "After");
    assert_eq!(wb.sheets[0].value(4, 1), Value::Number(999.0));
    assert_eq!(wb.sheets[0].value(5, 0), Value::Empty);
    assert_eq!(wb.sheets[0].tables[0].range, (0, 0, 3, 2));
    assert_eq!(wb.sheets[1].formula_of(0, 0), Some("=Data!B5"));
    assert_eq!(collect_cf_formulas(&wb.sheets[1].cf.blocks[0])[0], "=Data!$B$5");
    assert_eq!(wb.names[0].2, "Data!$B$5");
}
