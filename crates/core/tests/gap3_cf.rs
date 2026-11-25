//! Gap #3: Conditional Formatting engine, OOXML fidelity, negative bars, theme dxfs,
//! cross-sheet dxf remapping, CellIs cell-ref/formula evaluation, Cfvo::Formula,
//! TextMatch, TimePeriod, Blanks/Errors, range subtraction, and Undo/Redo tests.

use sheetcore::cf::{
    compute_stats, overlay_at_with_eval, overlay_at_with_eval_on_day, parse_cf_blocks,
    parse_dxfs_with_theme, subtract_rect, CfBlock, CfKind, CfOp, CfRule, CfSheet, CfStats, Cfvo,
    CfvoKind, Dxf, TextOp, TimePeriod,
};
use sheetcore::model::{Cell, Sheet, Value, Workbook};
use sheetcore::style_theme::ThemeBook;
use sheetcore::undo::{MetaEdit, UndoStack};

fn num_cell(v: f64) -> Cell {
    Cell { value: Value::Number(v), formula: None }
}

fn txt_cell(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

#[test]
fn cf_text_match_blanks_errors_and_time_period_rules() {
    let mut sh = Sheet::new("CF");
    sh.set(0, 0, txt_cell("北京分公司"));
    sh.set(1, 0, txt_cell("上海总部"));
    sh.set(2, 0, txt_cell("深圳研发中心"));
    // row 3 empty (blank)
    sh.set(4, 0, Cell { value: Value::Error(sheetcore::formula::ErrKind::Div0), formula: None });
    // Dates in col B around 2026-06-15 (serial = 46188)
    let today = sheetcore::formula::date_to_serial_pub(2026, 6, 15).unwrap() as i64; // Mon
    sh.set(0, 1, num_cell(today as f64)); // today
    sh.set(1, 1, num_cell((today - 1) as f64)); // yesterday
    sh.set(2, 1, num_cell((today + 1) as f64)); // tomorrow
    sh.set(3, 1, num_cell((today - 5) as f64)); // last7Days
    sh.set(4, 1, num_cell(sheetcore::formula::date_to_serial_pub(2026, 5, 20).unwrap())); // lastMonth

    let red = Dxf { fill: Some([255, 199, 206]), color: Some([156, 0, 6]), ..Default::default() };
    let green = Dxf { fill: Some([198, 239, 206]), color: Some([0, 97, 0]), ..Default::default() };
    let yellow = Dxf { fill: Some([255, 235, 156]), color: Some([156, 101, 0]), ..Default::default() };
    let blue = Dxf { fill: Some([217, 225, 242]), color: Some([31, 78, 121]), ..Default::default() };

    let cf = CfSheet {
        dxfs: vec![red, green, yellow, blue],
        blocks: vec![
            CfBlock {
                ranges: vec![(0, 0, 4, 0)],
                rules: vec![
                    CfRule { kind: CfKind::TextMatch { op: TextOp::BeginsWith, text: "北京".into(), dxf: 1 }, stop: true, priority: 1 },
                    CfRule { kind: CfKind::TextMatch { op: TextOp::EndsWith, text: "中心".into(), dxf: 3 }, stop: true, priority: 2 },
                    CfRule { kind: CfKind::Blanks { blank: true, dxf: 2 }, stop: true, priority: 3 },
                    CfRule { kind: CfKind::Errors { error: true, dxf: 0 }, stop: true, priority: 4 },
                    CfRule { kind: CfKind::TextMatch { op: TextOp::NotContains, text: "北京".into(), dxf: 0 }, stop: false, priority: 5 },
                ],
            },
            CfBlock {
                ranges: vec![(0, 1, 4, 1)],
                rules: vec![
                    CfRule { kind: CfKind::TimePeriod { period: TimePeriod::Today, dxf: 1 }, stop: true, priority: 1 },
                    CfRule { kind: CfKind::TimePeriod { period: TimePeriod::Yesterday, dxf: 2 }, stop: true, priority: 2 },
                    CfRule { kind: CfKind::TimePeriod { period: TimePeriod::Tomorrow, dxf: 3 }, stop: true, priority: 3 },
                    CfRule { kind: CfKind::TimePeriod { period: TimePeriod::LastMonth, dxf: 0 }, stop: true, priority: 4 },
                ],
            },
        ],
        ..Default::default()
    };
    let mut st = CfStats::default();
    compute_stats(&cf, &sh, &mut st);

    assert_eq!(overlay_at_with_eval_on_day(&cf, &st, &sh, 0, 0, |_, _, _| None, today).fill, Some([198, 239, 206]));
    assert_eq!(overlay_at_with_eval_on_day(&cf, &st, &sh, 1, 0, |_, _, _| None, today).fill, Some([255, 199, 206]));
    assert_eq!(overlay_at_with_eval_on_day(&cf, &st, &sh, 2, 0, |_, _, _| None, today).fill, Some([217, 225, 242]));
    assert_eq!(overlay_at_with_eval_on_day(&cf, &st, &sh, 3, 0, |_, _, _| None, today).fill, Some([255, 235, 156]));
    assert_eq!(overlay_at_with_eval_on_day(&cf, &st, &sh, 4, 0, |_, _, _| None, today).fill, Some([255, 199, 206]));

    assert_eq!(overlay_at_with_eval_on_day(&cf, &st, &sh, 0, 1, |_, _, _| None, today).fill, Some([198, 239, 206]));
    assert_eq!(overlay_at_with_eval_on_day(&cf, &st, &sh, 1, 1, |_, _, _| None, today).fill, Some([255, 235, 156]));
    assert_eq!(overlay_at_with_eval_on_day(&cf, &st, &sh, 2, 1, |_, _, _| None, today).fill, Some([217, 225, 242]));
    assert_eq!(overlay_at_with_eval_on_day(&cf, &st, &sh, 4, 1, |_, _, _| None, today).fill, Some([255, 199, 206]));
}

#[test]
fn cf_cell_is_ref_and_cfvo_formula_evaluation() {
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        // Threshold in $C$1 = 50, $C$2 = 100
        sh.set(0, 2, num_cell(50.0));
        sh.set(1, 2, num_cell(100.0));
        sh.set(0, 0, num_cell(25.0));
        sh.set(1, 0, num_cell(75.0));
        sh.set(2, 0, num_cell(125.0));
        sh.cf.dxfs.push(Dxf { fill: Some([198, 239, 206]), italic: true, ..Default::default() });
        sh.cf.blocks.push(CfBlock {
            ranges: vec![(0, 0, 2, 0)],
            rules: vec![
                CfRule {
                    kind: CfKind::CellIs {
                        op: CfOp::Between,
                        a: "=$C$1".into(),
                        b: "=$C$2".into(),
                        dxf: 0,
                    },
                    stop: false,
                    priority: 1,
                },
                CfRule {
                    kind: CfKind::Bar {
                        color: [99, 142, 198],
                        neg_color: Some([220, 53, 69]),
                        min: Cfvo { kind: CfvoKind::Formula, val: 0.0, formula: "=$C$1-50".into() },
                        max: Cfvo { kind: CfvoKind::Formula, val: 0.0, formula: "=$C$2".into() },
                        show_value: false,
                    },
                    stop: false,
                    priority: 2,
                },
            ],
        });
    }
    let sh = wb.sheet(0);
    let mut st = CfStats::default();
    compute_stats(&sh.cf, sh, &mut st);
    let mut calc = sheetcore::formula::Calc::new(&wb);
    let mut formulas = sheetcore::formula::RuleFormulas::default();
    let mut eval_cell = |r: u32| {
        overlay_at_with_eval(
            &sh.cf,
            &st,
            sh,
            r,
            0,
            |f, dr, dc| {
                formulas.shifted(f, dr, dc).ok().map(|n| match calc.eval_node(&n, 0) {
                    sheetcore::formula::FValue::Number(v) => Value::Number(v),
                    sheetcore::formula::FValue::Text(s) => Value::Text(s.into()),
                    sheetcore::formula::FValue::Bool(b) => Value::Bool(b),
                    _ => Value::Empty,
                })
            },
        )
    };
    let o0 = eval_cell(0);
    let o1 = eval_cell(1);
    let o2 = eval_cell(2);
    assert!(o0.fill.is_none());
    assert_eq!(o1.fill, Some([198, 239, 206]));
    assert!(o1.italic, "dxf italic must propagate to Overlay");
    assert!(o2.fill.is_none());
    // Cfvo::Formula min=0, max=100 => 25 -> 0.25, 75 -> 0.75, 125 -> 1.0
    assert!((o0.bar_ext.unwrap().end - 0.25).abs() < 1e-4);
    assert!((o1.bar_ext.unwrap().end - 0.75).abs() < 1e-4);
    assert!(!o1.bar_ext.unwrap().show_value);
}

#[test]
fn cf_bidirectional_negative_data_bars() {
    let mut sh = Sheet::new("NegBar");
    sh.set(0, 0, num_cell(-50.0));
    sh.set(1, 0, num_cell(0.0));
    sh.set(2, 0, num_cell(150.0));
    sh.cf.blocks.push(CfBlock {
        ranges: vec![(0, 0, 2, 0)],
        rules: vec![CfRule {
            kind: CfKind::bar_default([99, 142, 198], Cfvo::min(), Cfvo::max()),
            stop: false,
            priority: 1,
        }],
    });
    let mut st = CfStats::default();
    compute_stats(&sh.cf, &sh, &mut st);
    let neg = overlay_at_with_eval(&sh.cf, &st, &sh, 0, 0, |_, _, _| None)
        .bar_ext
        .unwrap();
    let pos = overlay_at_with_eval(&sh.cf, &st, &sh, 2, 0, |_, _, _| None)
        .bar_ext
        .unwrap();
    // Range -50..150 => zero axis at 50/200 = 0.25
    assert!((neg.axis.unwrap() - 0.25).abs() < 1e-4);
    assert!(neg.is_neg);
    assert!((neg.start - 0.0).abs() < 1e-4 && (neg.end - 0.25).abs() < 1e-4);
    assert_eq!(neg.color, [220, 53, 69]);
    assert!(!pos.is_neg);
    assert!((pos.start - 0.25).abs() < 1e-4 && (pos.end - 1.0).abs() < 1e-4);
    assert_eq!(pos.color, [99, 142, 198]);
}

#[test]
fn cf_subtract_rect_preserves_unselected_regions() {
    // Clearing B2:C3 inside A1:D4 should leave 4 non-overlapping strips:
    // Top A1:D1, Bottom A4:D4, Left A2:A3, Right D2:D3
    let rem = subtract_rect(&[(0, 0, 3, 3)], (1, 1, 2, 2));
    assert_eq!(rem, vec![(0, 0, 0, 3), (3, 0, 3, 3), (1, 0, 2, 0), (1, 3, 2, 3)]);
    // Full cover removes completely
    assert!(subtract_rect(&[(1, 1, 2, 2)], (0, 0, 5, 5)).is_empty());
}

#[test]
fn cf_theme_and_indexed_dxf_parsing_and_cross_sheet_save_roundtrip() {
    let styles_xml = r#"<styleSheet>
      <dxfs count="2">
        <dxf><font><color theme="4" tint="-0.25"/><i/></font><fill><patternFill patternType="solid"><bgColor indexed="10"/></patternFill></fill></dxf>
        <dxf><font><color rgb="FF112233"/></font><fill><patternFill patternType="solid"><bgColor theme="5" tint="0.6"/></patternFill></fill></dxf>
      </dxfs>
    </styleSheet>"#;
    let dxfs = parse_dxfs_with_theme(styles_xml, &ThemeBook::default());
    assert_eq!(dxfs.len(), 2);
    assert!(dxfs[0].color.is_some(), "theme+tint font color must resolve");
    assert!(dxfs[0].italic);
    assert_eq!(dxfs[0].fill, Some([255, 0, 0]), "indexed=10 must resolve to red");
    assert!(dxfs[1].fill.is_some(), "theme+tint fill color must resolve");

    // Cross-sheet distinct dxfs save & reload without dxfId out-of-bounds or collision
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Sheet2"));
    {
        let s0 = wb.sheet_mut(0);
        s0.set(0, 0, txt_cell("Alpha"));
        s0.cf.dxfs.push(Dxf { fill: Some([255, 0, 0]), ..Default::default() });
        s0.cf.blocks.push(CfBlock {
            ranges: vec![(0, 0, 3, 0)],
            rules: vec![CfRule {
                kind: CfKind::TextMatch { op: TextOp::NotContains, text: "Beta".into(), dxf: 0 },
                stop: true,
                priority: 1,
            }],
        });
        s0.cf_dirty = true;
    }
    {
        let s1 = wb.sheet_mut(1);
        s1.set(0, 0, num_cell(46000.0));
        s1.cf.dxfs.push(Dxf { fill: Some([0, 200, 100]), bold: true, ..Default::default() });
        s1.cf.blocks.push(CfBlock {
            ranges: vec![(0, 0, 3, 0)],
            rules: vec![
                CfRule {
                    kind: CfKind::TimePeriod { period: TimePeriod::ThisMonth, dxf: 0 },
                    stop: false,
                    priority: 1,
                },
                CfRule {
                    kind: CfKind::Bar {
                        color: [99, 142, 198],
                        neg_color: Some([220, 53, 69]),
                        min: Cfvo { kind: CfvoKind::Formula, val: 0.0, formula: "=$B$1".into() },
                        max: Cfvo::max(),
                        show_value: false,
                    },
                    stop: false,
                    priority: 2,
                },
            ],
        });
        s1.cf_dirty = true;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cf_cross_sheet.xlsx");
    sheetcore::xlsx::save(&path, &wb).unwrap();
    let loaded = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(loaded.sheet(0).cf.blocks.len(), 1);
    assert_eq!(loaded.sheet(1).cf.blocks.len(), 1);
    let r0 = &loaded.sheet(0).cf.blocks[0].rules[0];
    let d0 = loaded.sheet(0).cf.dxfs[r0.kind.dxf_id().unwrap()].fill;
    assert_eq!(d0, Some([255, 0, 0]));
    let r1 = &loaded.sheet(1).cf.blocks[0].rules[0];
    assert!(matches!(r1.kind, CfKind::TimePeriod { period: TimePeriod::ThisMonth, .. }));
    let d1 = loaded.sheet(1).cf.dxfs[r1.kind.dxf_id().unwrap()].fill;
    assert_eq!(d1, Some([0, 200, 100]), "Sheet2 dxfId must remap to Sheet2's distinct fill");
    match &loaded.sheet(1).cf.blocks[0].rules[1].kind {
        CfKind::Bar { min, show_value, .. } => {
            assert_eq!(min.kind, CfvoKind::Formula);
            assert_eq!(min.formula, "=$B$1");
            assert!(!*show_value);
        }
        other => panic!("expected Bar, got {other:?}"),
    }

    // Parse self-closing cfRule (e.g. duplicateValues) cleanly
    let self_closing = r#"<worksheet><conditionalFormatting sqref="A1:A5"><cfRule type="duplicateValues" dxfId="0" priority="1"/></conditionalFormatting></worksheet>"#;
    let parsed = parse_cf_blocks(self_closing);
    assert_eq!(parsed.len(), 1);
    assert!(matches!(parsed[0].rules[0].kind, CfKind::Dup { unique: false, dxf: 0 }));
}

#[test]
fn cf_meta_edit_undo_redo_roundtrip() {
    let mut wb = Workbook::new_blank();
    let mut undo = UndoStack::new();
    let before = wb.sheet(0).cf.clone();
    wb.sheet_mut(0).cf.blocks.push(CfBlock {
        ranges: vec![(0, 0, 5, 0)],
        rules: vec![CfRule { kind: CfKind::Blanks { blank: true, dxf: 0 }, stop: false, priority: 1 }],
    });
    let after = wb.sheet(0).cf.clone();
    undo.push_meta("添加空值规则", (0, 0, 0), MetaEdit { sheet: 0, cf: Some((before, after)), ..Default::default() });
    assert_eq!(wb.sheet(0).cf.blocks.len(), 1);
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.sheet(0).cf.blocks.len(), 0);
    assert!(undo.redo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.sheet(0).cf.blocks.len(), 1);
}
