//! 审阅报告（CELLAN v1.80 / 641c8b4）核心引擎 9 项缺陷定向回归测试。

use sheetcore::charts::{Chart, ChartKind, Series};
use sheetcore::formula;
use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::notes::Note;
use sheetcore::pivot::Agg;
use sheetcore::pivot_engine::{self, PivotTableModel};
use sheetcore::pivot_multi::{AxisField, PivotLayout, PivotSpec2, ShowAs, ValueField};
use sheetcore::print::{
    comment_summary_pages, export_pdf_sheet_with_pack, paginate, parse_page_ranges, PdfFontPack,
};
use sheetcore::sort_filter::advanced_filter;
use sheetcore::undo::{MetaEdit, UndoStack};
use sheetcore::xlsx;

fn num(v: Value) -> f64 {
    match v {
        Value::Number(n) => n,
        other => panic!("expected number, got {other:?}"),
    }
}

fn temp_xlsx(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cellan_v180_core_{}_{}",
        name,
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{name}.xlsx"))
}

/// A2 (P1): 高级筛选复制到与源区重叠的位置，不能读到刚覆写的表头/单元格。
#[test]
fn a2_advanced_filter_overlapping_copy_preserves_source_rows() {
    let mut wb = Workbook::new_blank();
    let sh = wb.sheet_mut(0);
    sh.set(0, 0, Cell { value: Value::Text("编号".into()), formula: None });
    sh.set(1, 0, Cell { value: Value::Number(1.0), formula: None });
    sh.set(2, 0, Cell { value: Value::Number(2.0), formula: None });
    sh.set(3, 0, Cell { value: Value::Number(3.0), formula: None });
    // C1="编号", C2=""（匹配全部记录）
    sh.set(0, 2, Cell { value: Value::Text("编号".into()), formula: None });

    let rep = advanced_filter(sh, (0, 0, 3, 0), Some((0, 2, 1, 2)), Some((1, 0)), false)
        .expect("advanced filter overlapping copy");
    assert_eq!(rep.matched, 3);
    assert_eq!(sh.value(1, 0), Value::Text("编号".into()));
    assert_eq!(sh.value(2, 0), Value::Number(1.0));
    assert_eq!(sh.value(3, 0), Value::Number(2.0));
    assert_eq!(sh.value(4, 0), Value::Number(3.0));
}

/// A3 (P1): 删除含透视表的工作表再撤销，透视表对象应完整恢复且保存重开不丢失。
#[test]
fn a3_undo_delete_sheet_restores_pivot_table_and_persists_to_xlsx() {
    let path = temp_xlsx("a3_undo_sheet_pivot");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".into();
    {
        let sh = wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("分类".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("金额".into()), formula: None });
        sh.set(1, 0, Cell { value: Value::Text("甲".into()), formula: None });
        sh.set(1, 1, Cell { value: Value::Number(15.0), formula: None });
    }
    let mut report = sheetcore::model::Sheet::new("报表");
    report.set(0, 0, Cell { value: Value::Text("分类".into()), formula: None });
    wb.sheets.push(report);

    let spec = PivotSpec2 {
        source: (0, 0, 1, 1),
        rows: vec![AxisField::plain(0)],
        cols: vec![],
        filters: vec![],
        values: vec![ValueField {
            col: 1,
            agg: Agg::Sum,
            show_as: ShowAs::Normal,
            label: Some("金额合计".into()),
            base: None,
        }],
        layout: PivotLayout::default(),
    };
    let mut model = PivotTableModel::new(
        "透视表1".into(),
        "数据".into(),
        Some(wb.sheets[0].identity),
        1,
        (0, 0),
        spec,
    );
    let computed = pivot_engine::compute_table(&wb.sheets[0], &model);
    let rect = pivot_engine::write_table_styled(
        &mut wb.sheets[1],
        &mut wb.styles,
        model.anchor,
        &computed,
        &model,
    );
    model.rect = rect;
    wb.pivots.push(model);

    xlsx::save_atomic(&path, &wb).expect("initial save with pivot");
    let mut loaded = xlsx::load(&path).expect("reload initial xlsx");
    assert_eq!(loaded.pivots.len(), 1, "初始重开应含 1 个透视表");

    // 删除「报表」页再立即撤销
    let mut undo = UndoStack::new();
    assert!(undo.delete_sheet(&mut loaded, 1, (1, 0, 0)));
    assert_eq!(loaded.sheets.len(), 1);
    assert_eq!(loaded.pivots.len(), 0);

    let applied = undo
        .undo_workbook(&mut loaded, |_, _, _, _| true)
        .expect("undo delete sheet");
    assert!(applied.structural);
    assert_eq!(loaded.sheets.len(), 2);
    assert_eq!(loaded.sheets[1].name, "报表");
    assert_eq!(loaded.pivots.len(), 1, "撤销删表后透视表对象必须恢复");
    assert_eq!(loaded.pivots[0].target_sheet, 1);

    // 再次保存并重新加载，确认透视表依然完整持久化
    xlsx::save_atomic(&path, &loaded).expect("save after undo");
    let reloaded = xlsx::load(&path).expect("reload after undo save");
    assert_eq!(reloaded.pivots.len(), 1, "保存重开后透视表数量仍应为 1");
    let _ = std::fs::remove_file(&path);
}

/// A4 (P1): 后台保存快照期间再次修改已有图表，旧保存回执绝不能清除新编辑的脏标。
#[test]
fn a4_stale_background_save_receipt_preserves_newer_existing_chart_edit() {
    let path = temp_xlsx("a4_chart_save_receipt");
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("月".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("值".into()), formula: None });
        sh.set(1, 0, Cell { value: Value::Text("1月".into()), formula: None });
        sh.set(1, 1, Cell { value: Value::Number(10.0), formula: None });
        sh.set(2, 0, Cell { value: Value::Text("2月".into()), formula: None });
        sh.set(2, 1, Cell { value: Value::Number(20.0), formula: None });
        let mut ch = Chart::default();
        ch.kind = ChartKind::Column;
        ch.title = "初版标题".into();
        ch.anchor = (0, 3, 12, 9);
        ch.series.push(Series {
            name: "值".into(),
            cats: vec!["1月".into(), "2月".into()],
            vals: vec![10.0, 20.0],
            val_ref: "Sheet1!$B$2:$B$3".into(),
            cat_ref: "Sheet1!$A$2:$A$3".into(),
            ..Default::default()
        });
        ch.content_dirty = true;
        sh.charts.push(ch);
        sh.charts_dirty = true;
    }
    xlsx::save_atomic(&path, &wb).expect("save v1");
    let mut live = xlsx::load(&path).expect("load v1");
    assert_eq!(live.sheets[0].charts.len(), 1);
    assert_eq!(live.sheets[0].charts[0].title, "初版标题");

    // 第一次修改图表标题并拍摄后台保存快照
    live.sheets[0].charts[0].title = "保存中标题".into();
    live.sheets[0].charts[0].content_dirty = true;
    live.sheets[0].charts_dirty = true;
    live.dirty = true;
    let snap = live.clone();

    // 在后台保存完成前，用户再次把标题改为「最新标题」
    live.sheets[0].charts[0].title = "最新标题".into();
    live.sheets[0].charts[0].content_dirty = true;
    live.sheets[0].charts_dirty = true;

    // 后台保存完成并回贴旧快照回执
    let (receipt, _) = xlsx::save_snapshot_with_receipt(&path, snap).expect("bg save");
    receipt.adopt_document(&mut live, &path, false);

    assert!(
        live.sheets[0].charts[0].content_dirty,
        "旧保存回执不能清掉后一次图表编辑的 content_dirty"
    );
    assert!(
        live.sheets[0].charts_dirty,
        "旧保存回执不能清掉后一次图表编辑的 charts_dirty"
    );

    // 用户再手动保存一次并重开，应看到「最新标题」
    xlsx::save_atomic(&path, &live).expect("save v2");
    let reopened = xlsx::load(&path).expect("reopen v2");
    assert_eq!(reopened.sheets[0].charts[0].title, "最新标题");
    let _ = std::fs::remove_file(&path);
}

/// B1 (P1): 15 位有效数字内的大额对账（如 2 万亿差一分钱）必须判不等且 `<` 为 TRUE。
#[test]
fn b1_large_reconciliation_amounts_detect_one_cent_difference() {
    let mut wb = Workbook::new_blank();
    let set_f = |wb: &mut Workbook, r: u32, f: &str| {
        wb.sheets[0].set(r, 0, Cell { value: Value::Empty, formula: Some(f.into()) });
    };
    set_f(&mut wb, 0, "=2000000000000=2000000000000.01");
    set_f(&mut wb, 1, "=2000000000000<2000000000000.01");
    set_f(&mut wb, 2, "=2000000000000<>2000000000000.01");
    set_f(&mut wb, 3, "=9999999999999.98<9999999999999.99");
    set_f(&mut wb, 4, "=(0.1+0.2)=0.3");
    formula::recalc_all(&mut wb);

    assert_eq!(wb.sheets[0].value(0, 0), Value::Bool(false), "2万亿 vs 2万亿+0.01 不能相等");
    assert_eq!(wb.sheets[0].value(1, 0), Value::Bool(true), "2万亿 < 2万亿+0.01 应为 TRUE");
    assert_eq!(wb.sheets[0].value(2, 0), Value::Bool(true), "2万亿 <> 2万亿+0.01 应为 TRUE");
    assert_eq!(wb.sheets[0].value(3, 0), Value::Bool(true), "近10万亿一分钱差异也应可区分");
    assert_eq!(wb.sheets[0].value(4, 0), Value::Bool(true), "0.1+0.2=0.3 浮点噪声仍应兼容");
}

/// B2 (P1): ACCRINT 正确遵循 first_interest、frequency、basis 与 calc_method。
#[test]
fn b2_accrint_respects_first_interest_and_calc_method() {
    let mut wb = Workbook::new_blank();
    let eval = |wb: &mut Workbook, f: &str| {
        wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some(f.into()) });
        formula::recalc_all(wb);
        wb.sheets[0].value(0, 0)
    };
    // 审阅报告复现场景：1月1日发行、7月1日首次付息、10月1日结算，年息10%、面值1000、半年付息、30/360
    let v_false = num(eval(
        &mut wb,
        "=ACCRINT(DATE(2024,1,1),DATE(2024,7,1),DATE(2024,10,1),0.1,1000,2,0,FALSE)",
    ));
    let v_true = num(eval(
        &mut wb,
        "=ACCRINT(DATE(2024,1,1),DATE(2024,7,1),DATE(2024,10,1),0.1,1000,2,0,TRUE)",
    ));
    let v_default = num(eval(
        &mut wb,
        "=ACCRINT(DATE(2024,1,1),DATE(2024,7,1),DATE(2024,10,1),0.1,1000,2,0)",
    ));
    assert!((v_false - 25.0).abs() < 1e-9, "calc_method=FALSE 应为 25，实际 {v_false}");
    assert!((v_true - 75.0).abs() < 1e-9, "calc_method=TRUE 应为 75，实际 {v_true}");
    assert!((v_default - 75.0).abs() < 1e-9, "省略 calc_method 默认 TRUE 应为 75，实际 {v_default}");

    // 微软官方文档样例：2008-03-01 发行，2008-08-31 首付，2008-05-01 结算
    let ms_b0 = num(eval(
        &mut wb,
        "=ACCRINT(DATE(2008,3,1),DATE(2008,8,31),DATE(2008,5,1),0.1,1000,2,0)",
    ));
    assert!((ms_b0 - 16.6666666667).abs() < 1e-6, "MS basis=0 应为 16.666667，实际 {ms_b0}");

    // basis=1 (Actual/Actual)：准付息期 [2008-02-29, 2008-08-31] = 184 天，应计 61 天 -> 1000*0.05*61/184 = 16.5760869565
    let ms_b1 = num(eval(
        &mut wb,
        "=ACCRINT(DATE(2008,3,1),DATE(2008,8,31),DATE(2008,5,1),0.1,1000,2,1)",
    ));
    let expected_b1 = 1000.0 * 0.05 * (61.0 / 184.0);
    assert!(
        (ms_b1 - expected_b1).abs() < 1e-6,
        "MS basis=1 应为 {expected_b1}，实际 {ms_b1}"
    );
}

/// B3 (P1): 透视表“平均值 → 占总计百分比”使用整表真实平均值作为分母，总计行应为 100%。
#[test]
fn b3_pivot_average_percent_of_total_uses_true_aggregate_denominator() {
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("分组".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("金额".into()), formula: None });
        sh.set(1, 0, Cell { value: Value::Text("A".into()), formula: None });
        sh.set(1, 1, Cell { value: Value::Number(100.0), formula: None });
        sh.set(2, 0, Cell { value: Value::Text("B".into()), formula: None });
        sh.set(2, 1, Cell { value: Value::Number(100.0), formula: None });
    }
    let spec = PivotSpec2 {
        source: (0, 0, 2, 1),
        rows: vec![AxisField::plain(0)],
        cols: vec![],
        filters: vec![],
        values: vec![ValueField {
            col: 1,
            agg: Agg::Avg,
            show_as: ShowAs::PercentOfTotal,
            label: Some("平均占比".into()),
            base: None,
        }],
        layout: PivotLayout::default(),
    };
    let mut model = PivotTableModel::new(
        "透视".into(),
        "Sheet1".into(),
        Some(wb.sheets[0].identity),
        0,
        (0, 4),
        spec,
    );
    let computed = pivot_engine::compute_table(&wb.sheets[0], &model);
    let g_idx = computed.grid.grand_total_row.expect("grand total row");
    assert!((num(computed.grid.rows[0][1].clone()) - 1.0).abs() < 1e-9, "A 组应为 100%");
    assert!((num(computed.grid.rows[1][1].clone()) - 1.0).abs() < 1e-9, "B 组应为 100%");
    assert!((num(computed.grid.rows[g_idx][1].clone()) - 1.0).abs() < 1e-9, "总计行应为 100%");

    // 非等权重分组：A=[100, 200] (均 150), B=[300] (均 300), 整表均值 = 200 -> A=75%, B=150%, 总计=100%
    wb.sheet_mut(0).set(3, 0, Cell { value: Value::Text("A".into()), formula: None });
    wb.sheet_mut(0).set(3, 1, Cell { value: Value::Number(200.0), formula: None });
    wb.sheet_mut(0).set(2, 1, Cell { value: Value::Number(300.0), formula: None });
    model.spec.source = (0, 0, 3, 1);
    let c2 = pivot_engine::compute_table(&wb.sheets[0], &model);
    let g2 = c2.grid.grand_total_row.expect("grand total row 2");
    assert!((num(c2.grid.rows[0][1].clone()) - 0.75).abs() < 1e-9, "A 组应为 75%");
    assert!((num(c2.grid.rows[1][1].clone()) - 1.50).abs() < 1e-9, "B 组应为 150%");
    assert!((num(c2.grid.rows[g2][1].clone()) - 1.0).abs() < 1e-9, "总计行应为 100%");
}

/// C1 (P1): 仅导出/打印选区时，工作表末尾批注汇总页绝不能泄露选区外的批注。
#[test]
fn c1_comment_summary_pages_filters_out_of_selection_notes() {
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("公开数据".into()), formula: None });
        sh.set(3, 3, Cell { value: Value::Text("内部敏感".into()), formula: None });
        sh.notes.insert(
            sheetcore::CellRef::new(3, 3),
            Note {
                r: 3,
                c: 3,
                author: "审计".into(),
                text: "绝密批注不得外发".into(),
            },
        );
        sh.page.print_comments = 1; // AtEnd
        sh.page.print_area = Some((0, 0, 0, 0)); // 仅 A1
    }
    let layout = paginate(wb.sheet(0), &wb.sheet(0).page);
    let summary = comment_summary_pages(&wb, 0, &layout);
    assert!(summary.is_empty(), "选区 A1 内无批注时不应生成包含 D4 批注的汇总页");

    // 给 A1 也加批注后，只应出现 A1，仍不能包含 D4
    wb.sheet_mut(0).notes.insert(
        sheetcore::CellRef::new(0, 0),
        Note {
            r: 0,
            c: 0,
            author: "对外".into(),
            text: "A1公开说明".into(),
        },
    );
    let summary2 = comment_summary_pages(&wb, 0, &layout);
    assert_eq!(summary2.len(), 1);
    let all_text: String = summary2[0].texts.iter().map(|t| t.text.as_str()).collect();
    assert!(all_text.contains("A1公开说明"));
    assert!(!all_text.contains("绝密批注不得外发"), "选区外 D4 批注绝不能出现");
}

/// C2 (P2): 自定义页码全部无效/越界时返回空列表而非回退全部页，PDF 导出也拒绝空页集。
#[test]
fn c2_parse_page_ranges_returns_empty_when_all_custom_pages_invalid() {
    assert!(parse_page_ranges("4", 3).is_empty(), "3页文档指定第4页应返回空");
    assert!(parse_page_ranges("4-6", 3).is_empty(), "3页文档指定4-6页应返回空");
    assert!(parse_page_ranges("0", 3).is_empty(), "0页无效");
    assert!(parse_page_ranges("abc", 3).is_empty(), "非数字无效");
    assert_eq!(parse_page_ranges("", 3), vec![0, 1, 2], "空串仍表示全部页");
    assert_eq!(parse_page_ranges("2-5", 3), vec![1, 2], "部分有效区间截断至有效页");

    let wb = Workbook::new_blank();
    let pdf_path = temp_xlsx("c2_empty_pages").with_extension("pdf");
    if let Ok(font) = sheetcore::pdf::Font::load(std::path::Path::new(r"C:\Windows\Fonts\arial.ttf")) {
        let pack = PdfFontPack {
            regular: font,
            bold: None,
            fallback: None,
            fallback_bold: None,
        };
        let res = export_pdf_sheet_with_pack(&wb, 0, &pdf_path, pack, None, Some(&[]));
        assert!(res.is_err(), "传入空页码列表时应报错拒绝导出");
        assert!(!pdf_path.exists());
    }
}

/// D1 (P1): 富文本 MetaEdit 撤销/重做必须返回 touched 单元格，使依赖公式能重新计算。
#[test]
fn d1_undo_redo_rich_text_edit_reports_touched_cells_for_recalc() {
    let mut wb = Workbook::new_blank();
    let before_cell = Cell { value: Value::Text("旧文本".into()), formula: None };
    let after_cell = Cell { value: Value::Text("新文本内容".into()), formula: None };
    wb.sheets[0].set(0, 0, after_cell.clone());
    wb.sheets[0].set(0, 1, Cell { value: Value::Empty, formula: Some("=LEN(A1)".into()) });
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 1), Value::Number(5.0));

    let mut undo = UndoStack::new();
    undo.push_meta(
        "富文本格式",
        (0, 0, 0),
        MetaEdit {
            sheet: 0,
            rich: vec![((0, 0), before_cell, after_cell, None, None)],
            ..Default::default()
        },
    );

    let applied_undo = undo.undo_workbook(&mut wb, |_, _, _, _| true).expect("undo rich");
    assert_eq!(applied_undo.touched, vec![(0, 0, 0)], "富文本撤销必须上报受影响格");
    assert_eq!(applied_undo.touched_count(), 1);
    let mut deps = formula::deps::Deps::build_after_recalc(&wb);
    formula::recalc_dirty(&mut wb, &mut deps, &applied_undo.touched);
    assert_eq!(wb.sheets[0].value(0, 1), Value::Number(3.0), "依赖公式应刷新为旧文本长度 3");

    let applied_redo = undo.redo_workbook(&mut wb, |_, _, _, _| true).expect("redo rich");
    assert_eq!(applied_redo.touched, vec![(0, 0, 0)], "富文本重做必须上报受影响格");
    assert_eq!(applied_redo.touched_count(), 1);
    formula::recalc_dirty(&mut wb, &mut deps, &applied_redo.touched);
    assert_eq!(wb.sheets[0].value(0, 1), Value::Number(5.0), "依赖公式应刷新为新文本长度 5");
}
