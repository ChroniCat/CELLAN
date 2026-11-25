//! Gap #4 综合回归测试：自动筛选、排序、ListObject 表格与数据工具全套深度能力验证。

use sheetcore::data_tools::{
    plan_sort_multi_with_styles, split_fields, text_to_columns_ex, SortKey, SortMode, SplitOpts,
};
use sheetcore::forecast::{detect_seasonality, ets};
use sheetcore::formula::{eval_data_tables_in_workbook, recalc_all, recalc_dirty, deps::Deps};
use sheetcore::model::{Cell, CellRef, DateSystem, Value, Workbook};
use sheetcore::search::{scoped_hits, SearchOpts, SearchOrder};
use sheetcore::sort_filter::{
    advanced_filter, cascading_column_choices, wildcard_match_ci, CmpOp, Criterion, DynamicFilter,
    Filter, TopFilter,
};
use sheetcore::style::CellStyle;
use sheetcore::subtotal::{try_subtotal_workbook_opts, SubFn, SubtotalOpts};
use sheetcore::table::{table_style_palette, totals_fn_code, Table};
use sheetcore::undo::{MetaEdit, UndoStack};

fn cell_txt(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}
fn cell_num(n: f64) -> Cell {
    Cell { value: Value::Number(n), formula: None }
}
fn cell_fml(f: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(f.into()) }
}

#[test]
fn quick_sort_uses_chinese_pinyin_collation() {
    let mut wb = Workbook::new_blank();
    let sh = &mut wb.sheets[0];
    sh.set(0, 0, cell_txt("城市"));
    sh.set(1, 0, cell_txt("上海")); // shanghai
    sh.set(2, 0, cell_txt("北京")); // beijing
    sh.set(3, 0, cell_txt("广州")); // guangzhou
    let changes = sheetcore::sort_filter::plan_sort_axis(sh, 0, (1, 0, 3, 0), 0, true, false).unwrap();
    changes.apply(sh);
    let got: Vec<String> = (1..=3).map(|r| sh.value(r, 0).display()).collect();
    assert_eq!(got, vec!["北京", "广州", "上海"]);
}

#[test]
fn autofilter_wildcards_compound_top10_dynamic_cascading_and_xlsx_roundtrip() {
    assert!(wildcard_match_ci("北京分公司", "北京*"));
    assert!(wildcard_match_ci("A1B", "a?b"));
    assert!(wildcard_match_ci("a*b", "a~*b"));
    assert!(!wildcard_match_ci("axxb", "a~*b"));

    let mut wb = Workbook::new_blank();
    let mut fmt_st = CellStyle::default();
    fmt_st.format = "#,##0.00".into();
    let fmt_id = wb.styles.intern(fmt_st);
    {
        let sh = &mut wb.sheets[0];
        sh.set(0, 0, cell_txt("地区"));
        sh.set(0, 1, cell_txt("产品"));
        sh.set(0, 2, cell_txt("销售额"));
        let rows = [
            ("华东", "手机Pro", 100.0),
            ("华东", "电脑Air", 400.0),
            ("华北", "手机Mini", 200.0),
            ("华东", "耳机Max", 50.0),
            ("华南", "手表Pro", 500.0),
        ];
        for (i, (reg, prod, amt)) in rows.iter().enumerate() {
            let r = i as u32 + 1;
            sh.set(r, 0, cell_txt(reg));
            sh.set(r, 1, cell_txt(prod));
            sh.set(r, 2, cell_num(*amt));
            sh.set_style(r, 2, fmt_id);
        }
    }

    // 1) 级联选项 + 格式化展示：地区筛选为“华东”后，产品列仅剩华东的 3 个产品，销售额列展示格式化文本
    let mut f = Filter::new(0, 0, 5, 2);
    f.allows.insert(0, ["华东".to_string()].into_iter().collect());
    let (prod_choices, _) = cascading_column_choices(&wb.sheets[0], &f, 1, &wb.styles, DateSystem::default());
    let prod_raw: Vec<&str> = prod_choices.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(prod_raw, vec!["手机Pro", "电脑Air", "耳机Max"]);
    let (amt_choices, numeric) = cascading_column_choices(&wb.sheets[0], &f, 2, &wb.styles, DateSystem::default());
    assert!(numeric);
    assert!(amt_choices.iter().any(|(raw, lbl)| raw == "400" && lbl == "400.00"));

    // 2) 复合条件 (OR) + 通配符
    let mut f2 = Filter::new(0, 0, 5, 2);
    let mut crit = Criterion::new(CmpOp::Eq, "*Pro");
    crit.second = Some((false, CmpOp::Begins, "耳机".into()));
    f2.criteria.insert(1, crit);
    let hidden = f2.hidden_row_intervals_with_styles(&wb.sheets[0], &wb.styles, DateSystem::default());
    // row 1 (手机Pro), row 4 (耳机Max), row 5 (手表Pro) kept; row 2, 3 hidden
    assert!(!hidden.contains(&1) && hidden.contains(&2) && hidden.contains(&3) && !hidden.contains(&4) && !hidden.contains(&5));

    // 3) Top-2 & AboveAverage
    let mut f3 = Filter::new(0, 0, 5, 2);
    f3.top_filters.insert(2, TopFilter { top: true, percent: false, count: 2 });
    let h_top = f3.hidden_row_intervals_with_styles(&wb.sheets[0], &wb.styles, DateSystem::default());
    assert!(!h_top.contains(&2) && !h_top.contains(&5) && h_top.contains(&1) && h_top.contains(&3) && h_top.contains(&4));

    let mut f4 = Filter::new(0, 0, 5, 2);
    f4.dynamic_filters.insert(2, DynamicFilter::AboveAverage); // avg = 250 -> keep 400, 500
    let h_avg = f4.hidden_row_intervals_with_styles(&wb.sheets[0], &wb.styles, DateSystem::default());
    assert!(!h_avg.contains(&2) && !h_avg.contains(&5) && h_avg.contains(&1) && h_avg.contains(&3) && h_avg.contains(&4));

    // 4) OOXML 往返保存：同时携带复合 customFilters (OR 非 Between)、top10、dynamicFilter
    let mut f_rt = Filter::new(0, 0, 5, 2);
    let mut c_or = Criterion::new(CmpOp::Lt, "100");
    c_or.second = Some((false, CmpOp::Gt, "450".into()));
    f_rt.criteria.insert(0, c_or);
    f_rt.top_filters.insert(1, TopFilter { top: true, percent: true, count: 25 });
    f_rt.dynamic_filters.insert(2, DynamicFilter::AboveAverage);
    wb.sheets[0].filter = Some(f_rt);
    wb.sheets[0].filter_dirty = true;

    let tmp = std::env::temp_dir().join(format!("gap4_filter_rt_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save_atomic(&tmp, &wb).unwrap();
    let back = sheetcore::xlsx::load(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    let bf = back.sheets[0].filter.as_ref().expect("filter roundtrip");
    let bc = bf.criteria.get(&0).expect("compound criteria roundtrip");
    assert_eq!(bc.op, CmpOp::Lt);
    assert!(bc.second.as_ref().is_some_and(|(is_and, op2, val2)| !*is_and && *op2 == CmpOp::Gt && val2 == "450"));
    assert_eq!(bf.top_filters.get(&1), Some(&TopFilter { top: true, percent: true, count: 25 }));
    assert_eq!(bf.dynamic_filters.get(&2), Some(&DynamicFilter::AboveAverage));
}

#[test]
fn advanced_filter_supports_and_or_wildcards_copy_and_unique() {
    let mut wb = Workbook::new_blank();
    let sh = &mut wb.sheets[0];
    sh.set(0, 0, cell_txt("部门"));
    sh.set(0, 1, cell_txt("金额"));
    let data = [
        ("华东A", 100.0),
        ("华东B", 300.0),
        ("华东B", 300.0), // duplicate
        ("华北A", 500.0),
        ("华南A", 50.0),
    ];
    for (i, (d, m)) in data.iter().enumerate() {
        sh.set(i as u32 + 1, 0, cell_txt(d));
        sh.set(i as u32 + 1, 1, cell_num(*m));
    }
    // 条件区域 D1:E3: (部门 = "华东*" AND 金额 >= 200) OR (金额 >= 500)
    sh.set(0, 3, cell_txt("部门"));
    sh.set(0, 4, cell_txt("金额"));
    sh.set(1, 3, cell_txt("华东*"));
    sh.set(1, 4, cell_txt(">=200"));
    sh.set(2, 4, cell_txt(">=500"));

    let rep = advanced_filter(sh, (0, 0, 5, 1), Some((0, 3, 2, 4)), Some((0, 6)), true).unwrap();
    assert_eq!(rep.matched, 2); // 华东B/300 (deduped) + 华北A/500
    assert_eq!(sh.value(0, 6).display(), "部门");
    assert_eq!(sh.value(1, 6).display(), "华东B");
    assert_eq!(sh.value(2, 6).display(), "华北A");
    assert_eq!(sh.value(3, 6), Value::Empty);
}

#[test]
fn multi_sort_supports_color_case_sensitive_and_horizontal() {
    let mut wb = Workbook::new_blank();
    let mut red_st = CellStyle::default();
    red_st.fill = Some([255, 0, 0]);
    let red_id = wb.styles.intern(red_st);
    {
        let sh = &mut wb.sheets[0];
        sh.set(0, 0, cell_txt("a"));
        sh.set(1, 0, cell_txt("A"));
        sh.set(2, 0, cell_txt("b"));
        sh.set_style(2, 0, red_id);
    }
    // 按填充色把红色行置顶
    let keys = vec![SortKey {
        col: 0,
        asc: true,
        ignore_case: true,
        mode: SortMode::FillColor(Some([255, 0, 0])),
    }];
    let changes = plan_sort_multi_with_styles(&wb.sheets[0], Some(&wb.styles), 0, (0, 0, 2, 0), &keys, false, false).unwrap();
    changes.apply(&mut wb.sheets[0]);
    assert_eq!(wb.sheets[0].value(0, 0).display(), "b");

    // 区分大小写排序：ASCII 中 'A' < 'a'
    let keys_cs = vec![SortKey { col: 0, asc: true, ignore_case: false, mode: SortMode::Default }];
    let ch_cs = plan_sort_multi_with_styles(&wb.sheets[0], Some(&wb.styles), 0, (0, 0, 2, 0), &keys_cs, false, false).unwrap();
    ch_cs.apply(&mut wb.sheets[0]);
    let got: Vec<String> = (0..=2).map(|r| wb.sheets[0].value(r, 0).display()).collect();
    assert_eq!(got, vec!["A", "a", "b"]);

    // 水平多键排序（重排列）
    let mut wb2 = Workbook::new_blank();
    let sh = &mut wb2.sheets[0];
    for (c, (k1, k2)) in [(2.0, 20.0), (1.0, 30.0), (2.0, 10.0)].iter().enumerate() {
        sh.set(0, c as u32, cell_num(*k1));
        sh.set(1, c as u32, cell_num(*k2));
    }
    let hkeys = vec![SortKey::text(0, true), SortKey::text(1, true)];
    let hch = plan_sort_multi_with_styles(sh, None, 0, (0, 0, 1, 2), &hkeys, false, true).unwrap();
    hch.apply(sh);
    assert_eq!(sh.value(0, 0), Value::Number(1.0));
    assert_eq!(sh.value(1, 1), Value::Number(10.0));
    assert_eq!(sh.value(1, 2), Value::Number(20.0));
}

#[test]
fn table_style_and_totals_fns_roundtrip_and_palette() {
    assert_eq!(totals_fn_code("sum"), Some(109));
    assert_eq!(totals_fn_code("average"), Some(101));
    assert_eq!(totals_fn_code("none"), None);
    let (hdr, _, _, _, _) = table_style_palette("TableStyleMedium7");
    assert_eq!(hdr, [33, 115, 70]);

    let mut wb = Workbook::new_blank();
    {
        let sh = &mut wb.sheets[0];
        sh.set(0, 0, cell_txt("项目"));
        sh.set(0, 1, cell_txt("金额"));
        sh.set(1, 0, cell_txt("甲"));
        sh.set(1, 1, cell_num(100.0));
        sh.set(2, 0, cell_txt("总计"));
        sh.set(2, 1, cell_fml("=SUBTOTAL(101,B2:B2)"));
        sh.tables.push(Table {
            name: "TStyle".into(),
            display: "TStyle".into(),
            range: (0, 0, 2, 1),
            columns: vec!["项目".into(), "金额".into()],
            column_ids: vec![1, 2],
            header_row: true,
            totals_row: true,
            style_name: "TableStyleMedium7".into(),
            totals_fns: vec!["none".into(), "average".into()],
        });
        sh.tables_dirty = true;
    }
    let tmp = std::env::temp_dir().join(format!("gap4_table_rt_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save_atomic(&tmp, &wb).unwrap();
    let back = sheetcore::xlsx::load(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    let bt = &back.sheets[0].tables[0];
    assert_eq!(bt.style_name, "TableStyleMedium7");
    assert_eq!(bt.totals_fns, vec!["none".to_string(), "average".to_string()]);
}

#[test]
fn subtotal_writes_to_key_col_supports_replace_and_page_breaks() {
    let mut wb = Workbook::new_blank();
    {
        let sh = &mut wb.sheets[0];
        sh.set(0, 0, cell_txt("序号"));
        sh.set(0, 1, cell_txt("部门"));
        sh.set(0, 2, cell_txt("金额"));
        for (i, (id, dept, val)) in [
            (1.0, "研发", 10.0),
            (2.0, "研发", 20.0),
            (3.0, "销售", 30.0),
        ].iter().enumerate() {
            let r = i as u32 + 1;
            sh.set(r, 0, cell_num(*id));
            sh.set(r, 1, cell_txt(dept));
            sh.set(r, 2, cell_num(*val));
        }
    }
    let opts = SubtotalOpts {
        replace_existing: true,
        page_break_between_groups: true,
        summary_below: true,
    };
    let rep = try_subtotal_workbook_opts(&mut wb, 0, (0, 0, 3, 2), 1, &[2], SubFn::Sum, opts).unwrap();
    assert_eq!(rep.groups, 2);
    recalc_all(&mut wb);
    // 汇总标签必须写在分组列 B (col=1) 而不是 A (col=0)！
    assert_eq!(wb.sheets[0].value(3, 1).display(), "研发 汇总");
    assert_eq!(wb.sheets[0].value(3, 2), Value::Number(30.0));
    assert_eq!(wb.sheets[0].value(6, 1).display(), "总计");
    assert_eq!(wb.sheets[0].value(6, 2), Value::Number(60.0));
    assert!(!wb.sheets[0].page.row_breaks.is_empty(), "应插入组间分页符");

    // 再次以 replace_existing=true 按平均值替换：旧 SUBTOTAL 行应被替换而非叠加
    let rep2 = try_subtotal_workbook_opts(&mut wb, 0, (0, 0, 6, 2), 1, &[2], SubFn::Average, opts).unwrap();
    assert_eq!(rep2.groups, 2);
    recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(3, 2), Value::Number(15.0));
    assert_eq!(wb.sheets[0].value(6, 2), Value::Number(20.0));
}

#[test]
fn ets_forecast_detects_seasonality_and_computes_confidence_bounds() {
    // 明确 4 季度季节性 + 上升趋势：10, 20, 30, 15, 14, 24, 34, 19, 18, 28, 38, 23
    let ys = vec![10.0, 20.0, 30.0, 15.0, 14.0, 24.0, 34.0, 19.0, 18.0, 28.0, 38.0, 23.0];
    assert_eq!(detect_seasonality(&ys), 4);
    let fc = ets(&ys, 4, None, 0.95).unwrap();
    assert_eq!(fc.season_len, 4);
    assert_eq!(fc.future.len(), 4);
    assert_eq!(fc.lower.len(), 4);
    assert_eq!(fc.upper.len(), 4);
    // 下一周期大约为 22, 32, 42, 27
    assert!((fc.future[0] - 22.0).abs() < 4.0, "got {}", fc.future[0]);
    assert!((fc.future[2] - 42.0).abs() < 4.0, "got {}", fc.future[2]);
    for i in 0..4 {
        assert!(fc.lower[i] <= fc.future[i] && fc.future[i] <= fc.upper[i]);
    }
    assert!(fc.r2 > 0.85, "r2={}", fc.r2);
}

#[test]
fn dynamic_data_table_recalculates_live_on_input_and_formula_changes() {
    let mut wb = Workbook::new_blank();
    {
        let sh = &mut wb.sheets[0];
        // A1 = price (10), A2 = qty (5), A3 = margin (2)
        sh.set(0, 0, cell_num(10.0));
        sh.set(1, 0, cell_num(5.0));
        sh.set(2, 0, cell_num(2.0));
        // 2-variable data table in C2:E4:
        // Master formula in C2: =A1*A2+A3
        // Row inputs in D2=20, E2=30 -> substituted into A1
        // Col inputs in C3=2, C4=4   -> substituted into A2
        sh.set(1, 2, cell_fml("=A1*A2+A3"));
        sh.set(1, 3, cell_num(20.0));
        sh.set(1, 4, cell_num(30.0));
        sh.set(2, 2, cell_num(2.0));
        sh.set(3, 2, cell_num(4.0));
        for r in 2..=3 {
            for c in 3..=4 {
                sh.set(r, c, cell_fml("=TABLE(A1,A2)"));
            }
        }
        sh.arrays.push((CellRef::new(2, 3), (2, 3, 3, 4)));
        // Downstream formula referencing the Data Table output: F2 = SUM(D3:E4)
        sh.set(1, 5, cell_fml("=SUM(D3:E4)"));
    }
    recalc_all(&mut wb);
    // D3 = 20*2+2 = 42, E3 = 30*2+2 = 62, D4 = 20*4+2 = 82, E4 = 30*4+2 = 122
    assert_eq!(wb.sheets[0].value(2, 3), Value::Number(42.0));
    assert_eq!(wb.sheets[0].value(2, 4), Value::Number(62.0));
    assert_eq!(wb.sheets[0].value(3, 3), Value::Number(82.0));
    assert_eq!(wb.sheets[0].value(3, 4), Value::Number(122.0));
    assert_eq!(wb.sheets[0].value(1, 5), Value::Number(308.0));
    // 输入格 A1/A2 必须无损恢复原值
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(10.0));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(5.0));

    // 修改 A3 (margin = 10) 并增量重算：模拟运算表和下游 F2 必须自动联动更新！
    let deps = Deps::build_after_recalc(&wb);
    wb.sheets[0].set(2, 0, cell_num(10.0));
    recalc_dirty(&mut wb, &deps, &[(0, 2, 0)]);
    assert_eq!(wb.sheets[0].value(2, 3), Value::Number(50.0));
    assert_eq!(wb.sheets[0].value(3, 4), Value::Number(130.0));
    assert_eq!(wb.sheets[0].value(1, 5), Value::Number(340.0));
    let _ = eval_data_tables_in_workbook(&mut wb, None);
}

#[test]
fn text_to_columns_multi_delims_and_destination() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, cell_txt("甲;乙,丙 丁"));
    let opts = SplitOpts {
        delim: ',',
        extra_delims: vec![';', ' '],
        destination: Some((0, 3)),
        merge_consecutive: true,
        quote: true,
        detect_types: false,
        date_system: DateSystem::default(),
        fixed_widths: None,
        col_modes: None,
    };
    assert_eq!(split_fields("甲;乙,丙 丁", &opts), vec!["甲", "乙", "丙", "丁"]);
    let rep = text_to_columns_ex(&mut wb.sheets[0], (0, 0, 0, 0), &opts, false);
    assert_eq!(rep.width, 4);
    assert_eq!(wb.sheets[0].value(0, 0).display(), "甲;乙,丙 丁", "指定目标区域时源列保留不变");
    assert_eq!(wb.sheets[0].value(0, 3).display(), "甲");
    assert_eq!(wb.sheets[0].value(0, 6).display(), "丁");
}

#[test]
fn search_supports_wildcards_comments_and_format() {
    let mut wb = Workbook::new_blank();
    let mut bold_st = CellStyle::default();
    bold_st.bold = true;
    let bold_id = wb.styles.intern(bold_st);
    {
        let sh = &mut wb.sheets[0];
        sh.set(0, 0, cell_txt("北京研发"));
        sh.set_style(0, 0, bold_id);
        sh.set(1, 0, cell_txt("北京销售"));
        sh.notes.insert(
            CellRef::new(2, 0),
            sheetcore::notes::Note { r: 2, c: 0, author: "A".into(), text: "需复核北京数据".into() },
        );
    }
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let hits_wild = scoped_hits(
        &wb,
        Some(0),
        &[],
        "北京*发",
        &SearchOpts { use_wildcards: true, ..Default::default() },
        SearchOrder::RowMajor,
        &cancel,
    );
    assert_eq!(hits_wild.len(), 1);
    assert_eq!((hits_wild[0].row, hits_wild[0].col), (0, 0));

    let hits_note = scoped_hits(
        &wb,
        Some(0),
        &[],
        "复核",
        &SearchOpts { in_comments: true, ..Default::default() },
        SearchOrder::RowMajor,
        &cancel,
    );
    assert_eq!(hits_note.len(), 1);
    assert_eq!((hits_note[0].row, hits_note[0].col), (2, 0));

    let hits_bold = scoped_hits(
        &wb,
        Some(0),
        &[],
        "北京",
        &SearchOpts { match_bold: Some(true), ..Default::default() },
        SearchOrder::RowMajor,
        &cancel,
    );
    assert_eq!(hits_bold.len(), 1);
    assert_eq!((hits_bold[0].row, hits_bold[0].col), (0, 0));
}

#[test]
fn meta_undo_restores_filter_tables_and_outline() {
    let mut wb = Workbook::new_blank();
    let mut undo = UndoStack::default();
    let mut f = Filter::new(0, 0, 5, 1);
    f.allows.insert(0, ["A".to_string()].into_iter().collect());
    let mut h = sheetcore::layout::HiddenIndices::default();
    h.insert(2);
    let t = Table { name: "T1".into(), display: "T1".into(), range: (0, 0, 2, 1), columns: vec!["A".into(), "B".into()], ..Default::default() };
    let mut rl = std::collections::HashMap::new();
    rl.insert(1, 1u8);

    wb.sheets[0].filter = Some(f.clone());
    wb.sheets[0].hidden_rows = h.clone();
    wb.sheets[0].tables.push(t.clone());
    wb.sheets[0].row_levels = rl.clone();
    undo.push_meta(
        "元数据变更",
        (0, 0, 0),
        MetaEdit {
            sheet: 0,
            filter: Some((None, Some(f.clone()), Default::default(), h.clone())),
            tables: Some((Vec::new(), vec![t.clone()])),
            outline: Some((Default::default(), rl.clone(), Default::default(), Default::default())),
            ..Default::default()
        },
    );
    let _ = undo.undo_workbook(&mut wb, |_, _, _, _| true);
    assert!(wb.sheets[0].filter.is_none());
    assert!(wb.sheets[0].hidden_rows.is_empty());
    assert!(wb.sheets[0].tables.is_empty());
    assert!(wb.sheets[0].row_levels.is_empty());

    let _ = undo.redo_workbook(&mut wb, |_, _, _, _| true);
    assert_eq!(wb.sheets[0].filter, Some(f));
    assert!(wb.sheets[0].hidden_rows.contains(&2));
    assert_eq!(wb.sheets[0].tables.len(), 1);
    assert_eq!(wb.sheets[0].row_levels.get(&1), Some(&1));
}
