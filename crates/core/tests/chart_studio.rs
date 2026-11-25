use sheetcore::chart_engine::{
    analyze_selection, build_smart_chart, compute_box_whisker, compute_error_bar_deltas,
    compute_funnel, compute_histogram, compute_treemap, compute_trendline, compute_waterfall,
    switch_row_column,
};
use sheetcore::chart_svg::render_chart_svg;
use sheetcore::chart_write::chart_xml_for_test;
use sheetcore::charts::{
    parse_chart, refresh_chart_from_sheets, AxisConfig, BlankPolicy, Chart, ChartKind,
    ChartThemeId, ErrorBar, ErrorBarDir, ErrorBarKind, LineDash, MarkerStyle, RefLineAxis,
    RefLineMode, ReferenceLine, Series, Trendline, TrendlineKind,
};
use sheetcore::model::{Cell, DateSystem, Sheet, Value, Workbook};
use sheetcore::print::{page_ops, paginate};
use sheetcore::undo::{ChartUndoEdit, UndoStack};
use sheetcore::xlsx;

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

fn sample_series() -> Vec<Series> {
    vec![
        Series {
            name: "营收".into(),
            cats: vec!["1月".into(), "2月".into(), "3月".into(), "4月".into(), "5月".into()],
            vals: vec![120.0, 155.0, 142.0, 188.0, 220.0],
            xv: vec![1.0, 2.0, 3.0, 4.0, 5.0],
            sizes: vec![10.0, 18.0, 15.0, 26.0, 35.0],
            name_ref: "Sheet1!$B$1".into(),
            cat_ref: "Sheet1!$A$2:$A$6".into(),
            val_ref: "Sheet1!$B$2:$B$6".into(),
            size_ref: "Sheet1!$C$2:$C$6".into(),
            color: Some([37, 99, 235]),
            smooth: true,
            marker: MarkerStyle::Diamond,
            line_width: 2.5,
            dash: LineDash::Solid,
            trendlines: vec![Trendline {
                kind: TrendlineKind::Linear,
                forward: 2.0,
                backward: 0.0,
                show_equation: true,
                show_r2: true,
                color: Some([220, 76, 62]),
                dash: LineDash::Dash,
                ..Default::default()
            }],
            error_bars: Some(ErrorBar {
                kind: ErrorBarKind::Percent(8.0),
                dir: ErrorBarDir::Both,
                color: None,
            }),
            ..Default::default()
        },
        Series {
            name: "毛利率".into(),
            cats: vec!["1月".into(), "2月".into(), "3月".into(), "4月".into(), "5月".into()],
            vals: vec![0.24, 0.27, 0.26, 0.31, 0.34],
            xv: vec![1.0, 2.0, 3.0, 4.0, 5.0],
            name_ref: "Sheet1!$C$1".into(),
            cat_ref: "Sheet1!$A$2:$A$6".into(),
            val_ref: "Sheet1!$C$2:$C$6".into(),
            kind: Some(ChartKind::Line),
            secondary: true,
            ..Default::default()
        },
    ]
}

#[test]
fn all_22_chart_kinds_ooxml_write_and_parse_roundtrip() {
    for &kind in ChartKind::all_creatable() {
        let mut ch = Chart {
            kind,
            title: format!("测试-{}", kind.label()),
            subtitle: "2026 Q1 经营复盘".into(),
            series: sample_series(),
            legend: true,
            legend_pos: "b".into(),
            anchor: (2, 3, 18, 12),
            show_labels: true,
            source_range: "Sheet1!$A$1:$C$6".into(),
            ..Default::default()
        };
        ch.style.theme = ChartThemeId::Financial;
        ch.style.rounded_bars = true;
        ch.style.hole_size = 62;
        ch.style.first_slice_ang = 45;
        ch.style.pie_explosion = 8;
        ch.style.blank_policy = BlankPolicy::Span;
        ch.y_axis = AxisConfig {
            title: "金额(万元)".into(),
            min: Some(0.0),
            max: Some(300.0),
            major_unit: Some(50.0),
            log_base: Some(10.0),
            reverse: true,
            num_fmt: "¥#,##0".into(),
            show_major_grid: true,
            show_minor_grid: true,
            hidden: false,
        };
        ch.sync_axis_compat();
        ch.ref_lines.push(ReferenceLine {
            label: "目标线".into(),
            axis: RefLineAxis::YPrimary,
            mode: RefLineMode::Fixed(180.0),
            color: [16, 185, 129],
            dash: LineDash::Dash,
        });
        ch.ref_lines.push(ReferenceLine {
            label: "安全带".into(),
            axis: RefLineAxis::YPrimary,
            mode: RefLineMode::Band {
                low: 130.0,
                high: 210.0,
            },
            color: [59, 130, 246],
            dash: LineDash::Solid,
        });
        ch.analytics.highlight_extrema = true;
        ch.analytics.show_growth_badge = true;
        ch.analytics.show_summary_bar = true;
        ch.analytics.show_pareto_line = true;
        ch.analytics.hist_bins = Some(6);
        ch.analytics.waterfall_totals = vec![4];
        ch.analytics.show_center_kpi = true;
        ch.analytics.center_kpi_title = "总营收".into();

        let xml = chart_xml_for_test(&ch);
        // 核对 <c:min> 与 <c:max> 为独立的合法 OOXML 元素（修复旧版 malformed XML 缺陷）
        if kind.is_cartesian() {
            assert!(
                xml.contains(r#"<c:min val="0"/>"#) && xml.contains(r#"<c:max val="300"/>"#),
                "{kind:?}: scaling min/max must be separate valid elements"
            );
            assert!(
                !xml.contains(r#"min val="0" max"#),
                "{kind:?}: must not emit malformed combined min/max tag"
            );
        }

        let parsed = parse_chart(&xml).unwrap_or_else(|| panic!("parse_chart failed for {kind:?}"));
        assert_eq!(parsed.kind, kind, "kind mismatch after XML roundtrip");
        assert_eq!(parsed.title, ch.title);
        assert_eq!(parsed.subtitle, ch.subtitle);
        assert_eq!(parsed.style.theme, ChartThemeId::Financial);
        assert_eq!(parsed.style.blank_policy, BlankPolicy::Span);
        assert_eq!(parsed.source_range, ch.source_range);
        assert_eq!(parsed.analytics.highlight_extrema, true);
        assert_eq!(parsed.analytics.show_growth_badge, true);
        assert_eq!(parsed.analytics.show_summary_bar, true);
        assert_eq!(parsed.analytics.hist_bins, Some(6));
        assert_eq!(parsed.analytics.waterfall_totals, vec![4]);
        assert_eq!(parsed.ref_lines.len(), 2);
        assert!(!parsed.series.is_empty());
        if !matches!(kind, ChartKind::Pie | ChartKind::Doughnut) {
            assert_eq!(parsed.series[0].trendlines.len(), 1, "{kind:?} trendline");
            assert!(parsed.series[0].error_bars.is_some(), "{kind:?} error_bars");
        }
        if kind.is_cartesian() {
            let eff_y = parsed.effective_y_axis();
            assert_eq!(eff_y.min, Some(0.0));
            assert_eq!(eff_y.max, Some(300.0));
            assert_eq!(eff_y.log_base, Some(10.0));
            assert!(eff_y.reverse);
        }
    }
}

#[test]
fn blank_workbook_multi_sheet_chart_insert_edit_delete_save_reload_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "cellan_chart_studio_rt_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("fresh_charts.xlsx");
    let _ = std::fs::remove_file(&path);

    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("利润桥"));

    // Sheet1 销售数据 + 组合图
    {
        let sh0 = &mut wb.sheets[0];
        sh0.set(0, 0, txt("月份"));
        sh0.set(0, 1, txt("销售额"));
        sh0.set(0, 2, txt("增长率"));
        for (i, (m, v, r)) in [
            ("1月", 1000.0, 0.10),
            ("2月", 1450.0, 0.14),
            ("3月", 1800.0, 0.19),
            ("4月", 2150.0, 0.22),
        ]
        .iter()
        .enumerate()
        {
            let rr = (i + 1) as u32;
            sh0.set(rr, 0, txt(m));
            sh0.set(rr, 1, num(*v));
            sh0.set(rr, 2, num(*r));
        }
        let mut ch0 = build_smart_chart(
            sh0,
            &sh0.name,
            (0, 0, 4, 2),
            "月度经营双轴图",
            ChartKind::Combo,
            false,
            None,
            DateSystem::D1900,
        );
        ch0.anchor = (1, 5, 16, 13);
        ch0.style.theme = ChartThemeId::DarkBi;
        ch0.ref_lines.push(ReferenceLine::default());
        sh0.charts.push(ch0);
        sh0.charts_dirty = true;
    }

    // Sheet2 财务瀑布图
    {
        let sh1 = &mut wb.sheets[1];
        sh1.set(0, 0, txt("科目"));
        sh1.set(0, 1, txt("金额"));
        for (i, (k, v)) in [
            ("期初", 500.0),
            ("主营收入", 320.0),
            ("成本支出", -180.0),
            ("费用", -90.0),
            ("期末合计", 550.0),
        ]
        .iter()
        .enumerate()
        {
            let rr = (i + 1) as u32;
            sh1.set(rr, 0, txt(k));
            sh1.set(rr, 1, num(*v));
        }
        let mut wf = build_smart_chart(
            sh1,
            &sh1.name,
            (0, 0, 5, 1),
            "利润瀑布图",
            ChartKind::Waterfall,
            false,
            None,
            DateSystem::D1900,
        );
        wf.anchor = (2, 4, 15, 11);
        sh1.charts.push(wf);
        sh1.charts_dirty = true;
    }

    // 1) 未保存的全新工作簿直接保存 → 两张表图表均落地
    xlsx::save(&path, &wb).expect("save fresh workbook with charts");
    let mut loaded = xlsx::load(&path).expect("reload workbook with charts");
    assert_eq!(loaded.sheets[0].charts.len(), 1);
    assert_eq!(loaded.sheets[1].charts.len(), 1);
    assert_eq!(loaded.sheets[0].charts[0].kind, ChartKind::Combo);
    assert_eq!(loaded.sheets[0].charts[0].style.theme, ChartThemeId::DarkBi);
    assert_eq!(loaded.sheets[0].charts[0].anchor, (1, 5, 16, 13));
    assert_eq!(loaded.sheets[0].charts[0].ref_lines.len(), 1);
    assert_eq!(loaded.sheets[1].charts[0].kind, ChartKind::Waterfall);
    assert_eq!(loaded.sheets[1].charts[0].anchor, (2, 4, 15, 11));

    // 2) 编辑 Sheet1 图表标题并删除 Sheet2 图表 → 二次保存并验证
    loaded.sheets[0].charts[0].title = "月度经营双轴图-修订版".into();
    loaded.sheets[0].charts[0].content_dirty = true;
    loaded.sheets[0].charts_dirty = true;

    let removed = loaded.sheets[1].charts.remove(0);
    assert!(!removed.part.is_empty() && !removed.drawing.is_empty());
    loaded.sheets[1]
        .deleted_charts
        .push((removed.drawing, removed.part));
    loaded.sheets[1].charts_dirty = true;

    xlsx::save(&path, &loaded).expect("resave after chart edit and delete");
    let loaded2 = xlsx::load(&path).expect("reload after edit and delete");
    assert_eq!(loaded2.sheets[0].charts.len(), 1);
    assert_eq!(loaded2.sheets[0].charts[0].title, "月度经营双轴图-修订版");
    assert!(loaded2.sheets[1].charts.is_empty(), "deleted chart must be gone");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn chart_undo_edit_preserves_prior_cell_undo_history() {
    let mut wb = Workbook::new_blank();
    let mut undo = UndoStack::default();

    // 先做一次单元格编辑
    let sh_before = wb.sheets[0].clone();
    wb.sheets[0].set(0, 0, num(42.0));
    undo.push_cow_batch("输入数值", 0, &sh_before, &wb.sheets[0], (0, 0, 0));

    // 再插入图表
    let before = wb.sheets[0].charts.clone();
    let del_before = wb.sheets[0].deleted_charts.clone();
    wb.sheets[0].charts.push(Chart {
        kind: ChartKind::Waterfall,
        title: "损益桥".into(),
        ..Default::default()
    });
    undo.push_chart_edit(
        "插入财务瀑布图",
        (0, 0, 0),
        ChartUndoEdit {
            sheet: 0,
            charts_before: before,
            charts_after: wb.sheets[0].charts.clone(),
            deleted_before: del_before,
            deleted_after: wb.sheets[0].deleted_charts.clone(),
        },
    );

    // 撤销图表插入
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert!(wb.sheets[0].charts.is_empty());
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(42.0));

    // 之前单元格的历史仍然完好可撤销！
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.sheets[0].value(0, 0), Value::Empty);

    // 依次重做回来
    assert!(undo.redo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(42.0));
    assert!(undo.redo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.sheets[0].charts.len(), 1);
    assert_eq!(wb.sheets[0].charts[0].kind, ChartKind::Waterfall);
}

#[test]
fn analytics_solvers_trendlines_error_bars_and_special_layouts_are_accurate() {
    // 1. Linear OLS: y = 2x + 3
    let pts: Vec<(f64, f64)> = (1..=6).map(|x| (x as f64, 2.0 * x as f64 + 3.0)).collect();
    let tl = Trendline {
        kind: TrendlineKind::Linear,
        forward: 2.0,
        show_equation: true,
        show_r2: true,
        ..Default::default()
    };
    let ct = compute_trendline(&pts, &tl).expect("linear fit");
    assert!((ct.r_squared.unwrap() - 1.0).abs() < 1e-9);
    let last_pt = ct.points.last().unwrap();
    assert!((last_pt.0 - 8.0).abs() < 1e-6);
    assert!((last_pt.1 - 19.0).abs() < 1e-6);

    // 2. Quadratic Polynomial: y = x^2 - 2x + 5
    let qpts: Vec<(f64, f64)> = (1..=7)
        .map(|x| {
            let xf = x as f64;
            (xf, xf * xf - 2.0 * xf + 5.0)
        })
        .collect();
    let qtl = Trendline {
        kind: TrendlineKind::Polynomial(2),
        ..Default::default()
    };
    let qct = compute_trendline(&qpts, &qtl).expect("poly2 fit");
    assert!((qct.r_squared.unwrap() - 1.0).abs() < 1e-6);

    // 3. Exponential, Log, Power, MovingAverage
    for kind in [
        TrendlineKind::Exponential,
        TrendlineKind::Logarithmic,
        TrendlineKind::Power,
        TrendlineKind::MovingAverage(2),
    ] {
        let res = compute_trendline(&pts, &Trendline { kind, ..Default::default() });
        assert!(res.is_some(), "trendline {kind:?} should succeed");
    }

    // 4. Error bars
    let vals = vec![10.0, 20.0, 30.0, 40.0];
    let fixed = compute_error_bar_deltas(
        &vals,
        &ErrorBar {
            kind: ErrorBarKind::Fixed(3.0),
            ..Default::default()
        },
    );
    assert_eq!(fixed, vec![3.0; 4]);
    let pct = compute_error_bar_deltas(
        &vals,
        &ErrorBar {
            kind: ErrorBarKind::Percent(10.0),
            ..Default::default()
        },
    );
    assert!((pct[1] - 2.0).abs() < 1e-9);

    // 5. Waterfall + Funnel + Treemap + Histogram + BoxWhisker
    let wf_s = Series {
        cats: vec!["期初".into(), "收入".into(), "费用".into(), "期末合计".into()],
        vals: vec![100.0, 60.0, -35.0, 125.0],
        ..Default::default()
    };
    let bars = compute_waterfall(&wf_s, &[]);
    assert_eq!(bars.len(), 4);
    assert!((bars[2].y_end - 125.0).abs() < 1e-9);

    let fn_s = Series {
        cats: vec!["曝光".into(), "点击".into(), "加购".into(), "成交".into()],
        vals: vec![1000.0, 400.0, 100.0, 50.0],
        ..Default::default()
    };
    let stages = compute_funnel(&fn_s);
    assert_eq!(stages.len(), 4);
    assert!((stages[1].step_conv.unwrap() - 0.4).abs() < 1e-9);
    assert!((stages[3].total_conv.unwrap() - 0.05).abs() < 1e-9);

    let tiles = compute_treemap(&fn_s, 0.0, 0.0, 400.0, 200.0);
    assert_eq!(tiles.len(), 4);
    let area: f32 = tiles.iter().map(|t| t.w * t.h).sum();
    assert!((area - 80_000.0).abs() < 50.0);

    let hist_s = Series {
        vals: (0..40).map(|i| (i % 10) as f64 * 5.0 + 1.0).collect(),
        ..Default::default()
    };
    let bins = compute_histogram(&hist_s, Some(5), true);
    assert_eq!(bins.len(), 5);
    assert!((bins.last().unwrap().cum_pct - 1.0).abs() < 1e-6);

    let bw_chart = Chart {
        kind: ChartKind::BoxWhisker,
        series: vec![Series {
            name: "延迟(ms)".into(),
            vals: vec![10.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 120.0],
            ..Default::default()
        }],
        ..Default::default()
    };
    let boxes = compute_box_whisker(&bw_chart);
    assert_eq!(boxes.len(), 1);
    assert_eq!(boxes[0].outliers, vec![120.0], "120.0 must be detected as Tukey outlier");
}

#[test]
fn smart_advisor_switch_row_col_and_plot_vis_only_refresh() {
    let mut sh = Sheet::new("数据");
    sh.set(0, 0, txt("阶段"));
    sh.set(0, 1, txt("人数"));
    for (r, (k, v)) in [
        ("访问", 10000.0),
        ("注册", 3200.0),
        ("试算", 900.0),
        ("付费", 240.0),
    ]
    .iter()
    .enumerate()
    {
        sh.set((r + 1) as u32, 0, txt(k));
        sh.set((r + 1) as u32, 1, num(*v));
    }
    let (prof, recs) = analyze_selection(&sh, (0, 0, 4, 1), None, DateSystem::D1900);
    assert!(prof.monotonic_decreasing);
    assert!(recs.iter().any(|r| r.kind == ChartKind::Funnel));

    // 行列互换
    let mut ch = build_smart_chart(
        &sh,
        "数据",
        (0, 0, 4, 1),
        "转化分析",
        ChartKind::Column,
        false,
        None,
        DateSystem::D1900,
    );
    assert_eq!(ch.series.len(), 1);
    assert_eq!(ch.series[0].vals.len(), 4);
    assert!(switch_row_column(&mut ch, &[sh.clone()], 0, None, DateSystem::D1900));
    assert!(ch.style.switch_row_col);
    assert_eq!(ch.series.len(), 4);

    // 切回按列并验证 plot_vis_only 过滤筛选隐藏行
    assert!(switch_row_column(&mut ch, &[sh.clone()], 0, None, DateSystem::D1900));
    assert!(!ch.style.switch_row_col);
    sh.hidden_rows.insert(2); // 隐藏 "注册" 行
    ch.style.plot_vis_only = true;
    refresh_chart_from_sheets(&mut ch, &[sh.clone()], 0, None, DateSystem::D1900);
    assert_eq!(ch.series[0].vals.len(), 3, "hidden row should be filtered out");
    assert!(!ch.series[0].cats.contains(&"注册".to_string()));
}

#[test]
fn svg_export_and_pdf_print_support_all_advanced_chart_kinds() {
    for kind in [
        ChartKind::Column,
        ChartKind::ColumnStacked100,
        ChartKind::Bar,
        ChartKind::Line,
        ChartKind::Area,
        ChartKind::Combo,
        ChartKind::Waterfall,
        ChartKind::Funnel,
        ChartKind::Treemap,
        ChartKind::Histogram,
        ChartKind::BoxWhisker,
        ChartKind::Pie,
        ChartKind::Doughnut,
        ChartKind::Scatter,
        ChartKind::Bubble,
        ChartKind::Radar,
    ] {
        let mut ch = Chart {
            kind,
            title: format!("SVG-{}", kind.label()),
            subtitle: "自动导出验证".into(),
            series: sample_series(),
            legend: true,
            anchor: (0, 0, 12, 8),
            show_labels: true,
            ..Default::default()
        };
        ch.style.theme = if kind == ChartKind::Waterfall {
            ChartThemeId::DarkBi
        } else {
            ChartThemeId::CellanModern
        };
        ch.analytics.show_summary_bar = true;
        ch.analytics.show_center_kpi = true;
        ch.ref_lines.push(ReferenceLine::default());

        let svg = render_chart_svg(&ch, 800.0, 480.0, DateSystem::D1900);
        assert!(svg.starts_with("<?xml ") && svg.contains("<svg "), "{kind:?} svg header");
        assert!(svg.trim_end().ends_with("</svg>"), "{kind:?} svg footer");
        assert!(svg.contains(&ch.title), "{kind:?} svg title");

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, txt("x"));
        wb.sheets[0].charts.push(ch);
        let layout = paginate(&wb.sheets[0], &wb.sheets[0].page);
        let ops = page_ops(&wb, 0, &layout, 0);
        assert!(
            !ops.rects.is_empty() || !ops.lines.is_empty() || !ops.polys.is_empty() || !ops.texts.is_empty(),
            "{kind:?} PDF page_ops should emit drawing ops"
        );
    }
}
