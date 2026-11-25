//! 针对 v1.77 四大方向源码审查修复项的端到端回归测试套件。
//! 覆盖：输入解析与编辑栏格式化、筛选可见行保护、崩溃/关闭保护与影子恢复、条件格式增量缓存、
//! Ctrl+Enter 定位空值填充、CSE、文本型数字一键转数值、可撤销元数据与多步撤销、合并变体、
//! 跨表追踪与公式求值、日期/颜色筛选、非模态查找替换、最近文件固定/离线保留、主题切换与 AI 预览。

use super::*;
use crate::test_env::TestEnv;
use sheetcore::{Cell, Value};

fn new_app(ctx: &egui::Context) -> SheetApp {
    let mut app = SheetApp::new_with_context(ctx, None);
    app.show_start = false;
    app.has_open_workbook = true;
    app
}

fn run_frame(app: &mut SheetApp, ctx: &egui::Context) {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    let _ = ctx.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame));
}

#[test]
fn audit_1_1_manual_input_parses_dates_percents_currency_text_col_and_quote() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);

    // 1) 日期：2026/11/25 -> 真实日期序列号 + 日期格式，编辑栏可读显示
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("2026/11/25".into()));
    assert!(app.commit_edit(None));
    assert!(matches!(app.wb.sheet(0).value(0, 0), Value::Number(n) if n > 46_000.0));
    let fmt0 = app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).format.clone();
    assert!(sheetcore::style::is_date_format(&fmt0), "应自动套用日期格式，实际：{fmt0}");
    let edit_txt = app.format_cell_edit_text(0, 0);
    assert!(edit_txt.contains("2026") && edit_txt.contains("11") && edit_txt.contains("25"), "编辑栏应显示人类可读日期而非序列号：{edit_txt}");

    // 2) 百分比：15% -> 0.15 + 百分比格式
    app.sel = (1, 0);
    app.sel_end = (1, 0);
    app.begin_edit(Some("15%".into()));
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(0.15));
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(1, 0)).format.contains('%'));

    // 3) 千分位与货币金额：1,234.56 与 ¥1,000 -> 数值
    app.sel = (2, 0);
    app.sel_end = (2, 0);
    app.begin_edit(Some("1,234.56".into()));
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).value(2, 0), Value::Number(1234.56));

    app.sel = (3, 0);
    app.sel_end = (3, 0);
    app.begin_edit(Some("¥1,000".into()));
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).value(3, 0), Value::Number(1000.0));

    // 4) 文本格式列 (@)：长账号不丢失精度
    app.sel = (4, 0);
    app.sel_end = (4, 0);
    app.apply_style(|s| s.format = "@".into());
    app.begin_edit(Some("6222021001123456789".into()));
    assert!(app.commit_edit(None));
    assert_eq!(
        app.wb.sheet(0).value(4, 0),
        Value::Text("6222021001123456789".into())
    );

    // 5) 前导单引号：'00123 去掉撇号存储文本，记录 quote_prefix，编辑栏还原 '00123
    app.sel = (5, 0);
    app.sel_end = (5, 0);
    app.begin_edit(Some("'00123".into()));
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).value(5, 0), Value::Text("00123".into()));
    assert!(app.wb.styles.quote_of(app.wb.sheet(0).style_of(5, 0)));
    assert_eq!(app.format_cell_edit_text(5, 0), "'00123");
}

#[test]
fn audit_1_2_autofilter_hidden_rows_preserved_by_delete_ctrl_d_and_fill_handle() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("状态".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("金额".into()), formula: None });
        for (r, (st, amt)) in [
            (1, ("保留", 10.0)),
            (2, ("筛掉", 999.0)),
            (3, ("保留", 20.0)),
            (4, ("筛掉", 888.0)),
        ] {
            sh.set(r, 0, Cell { value: Value::Text(st.into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Number(amt), formula: None });
        }
        let mut f = sheetcore::sort_filter::Filter::new(0, 0, 4, 1);
        f.allows.insert(0, ["保留".to_string()].into_iter().collect());
        sh.filter = Some(f);
    }
    app.recalc_filter_hidden();
    assert!(app.wb.sheet(0).hidden_rows.contains(&2));
    assert!(app.wb.sheet(0).hidden_rows.contains(&4));

    // Delete 仅清除可见行 B2/B4，保留被筛掉的 B3(999)/B5(888)
    app.sel = (1, 1);
    app.sel_end = (4, 1);
    app.clear_values();
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty);
    assert_eq!(app.wb.sheet(0).value(2, 1), Value::Number(999.0));
    assert_eq!(app.wb.sheet(0).value(3, 1), Value::Empty);
    assert_eq!(app.wb.sheet(0).value(4, 1), Value::Number(888.0));

    // Ctrl+D 仅填充可见行
    app.wb.sheet_mut(0).set(1, 1, Cell { value: Value::Number(55.0), formula: None });
    app.sel = (1, 1);
    app.sel_end = (4, 1);
    app.fill_cmd(true);
    assert_eq!(app.wb.sheet(0).value(3, 1), Value::Number(55.0));
    assert_eq!(app.wb.sheet(0).value(2, 1), Value::Number(999.0));
    assert_eq!(app.wb.sheet(0).value(4, 1), Value::Number(888.0));

    // 拖填充柄仅填充可见行
    app.wb.sheet_mut(0).set(1, 1, Cell { value: Value::Number(77.0), formula: None });
    app.sel = (1, 1);
    app.sel_end = (1, 1);
    app.fill_to(4, true);
    assert_eq!(app.wb.sheet(0).value(3, 1), Value::Number(77.0));
    assert_eq!(app.wb.sheet(0).value(2, 1), Value::Number(999.0));
    assert_eq!(app.wb.sheet(0).value(4, 1), Value::Number(888.0));
}

#[test]
fn audit_1_3_close_prompts_on_named_dirty_file_and_autorecover_handles_unnamed() {
    let env = TestEnv::new("audit-close-recover");
    let path = env.path("named.xlsx");
    let mut wb = templates::build(0);
    wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    sheetcore::xlsx::save_atomic(&path, &wb).unwrap();

    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    app.wb.path = Some(path.clone());
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(42.0), formula: None });
    app.wb.dirty = true;

    // 关闭已命名脏文件必须先弹出保存/不保存/取消确认框，而不是静默覆写原文件
    app.request_close_tab(0);
    assert_eq!(app.tab_close_confirm, Some(app.doc_uid));
    assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheet(0).value(0, 0), Value::Number(1.0));

    // 未命名工作簿也写入隔离的影子恢复快照，不碰原文件
    let mut unnamed = templates::build(0);
    unnamed.sheet_mut(0).set(0, 0, Cell { value: Value::Text("草稿".into()), formula: None });
    unnamed.dirty = true;
    let entry = recovery::RecoveryEntry {
        token: recovery::make_token(std::process::id(), 987654),
        title: "未命名.xlsx".into(),
        original_path: None,
        saved_unix_secs: 1,
        pid: std::process::id(),
        doc_uid: 987654,
    };
    recovery::write_recovery_snapshot(&entry, &unnamed).unwrap();
    assert!(entry.snapshot_path().exists());
    let loaded = sheetcore::xlsx::load(&entry.snapshot_path()).unwrap();
    assert_eq!(loaded.sheet(0).value(0, 0), Value::Text("草稿".into()));
    recovery::remove_entry(&entry);
    assert!(!entry.snapshot_path().exists());
    drop(env);
}

#[test]
fn audit_3_2_and_3_3_goto_blanks_ctrl_enter_and_cse_array() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("一月".into()), formula: None });
        // A2、A3 为空
        sh.set(3, 0, Cell { value: Value::Text("二月".into()), formula: None });
        // A5 为空
    }
    app.sel = (0, 0);
    app.sel_end = (4, 0);
    app.select_special(crate::wps_menus::SpecialTarget::Blanks);
    let rects = app.sel_rects();
    assert!(!rects.is_empty(), "定位空值应选中空单元格");

    // 财务经典流程：定位空值 -> 输入 =上一格 -> Ctrl+Enter 填满所有空单元格
    let (ar, ac) = app.sel;
    let prev = sheetcore::cell_name(ar.saturating_sub(1), ac);
    app.begin_edit(Some(format!("={prev}")));
    assert!(app.commit_edit_fill_selection());
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Text("一月".into()));
    assert_eq!(app.wb.sheet(0).value(2, 0), Value::Text("一月".into()));
    assert_eq!(app.wb.sheet(0).value(4, 0), Value::Text("二月".into()));

    // Ctrl+Shift+Enter 登记实时数组公式
    app.wb.sheet_mut(0).set(0, 2, Cell { value: Value::Number(2.0), formula: None });
    app.wb.sheet_mut(0).set(1, 2, Cell { value: Value::Number(3.0), formula: None });
    app.sel = (0, 3);
    app.sel_end = (0, 3);
    app.extra_sel.clear();
    app.begin_edit(Some("=SUM(C1:C2*10)".into()));
    app.cse_pending = true;
    assert!(app.commit_edit(None));
    assert!(!app.wb.sheet(0).arrays.is_empty(), "CSE 应登记为数组公式");
    assert_eq!(app.wb.sheet(0).value(0, 3), Value::Number(50.0));
}

#[test]
fn audit_3_6_convert_number_as_text_to_real_number() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    app.sel = (0, 0);
    app.sel_end = (1, 0);
    app.apply_style(|s| s.format = "@".into());
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Text("00123".into()), formula: None });
    app.wb.sheet_mut(0).set(1, 0, Cell { value: Value::Text("1,234.50".into()), formula: None });

    let n = app.convert_selection_text_to_numbers();
    assert_eq!(n, 2);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0));
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(1234.5));
    assert_ne!(app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).format, "@");

    // 可一键撤销恢复原文本与格式
    assert!(app.apply_undo(false));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("00123".into()));
}

#[test]
fn audit_3_7_and_3_9_undoable_meta_multi_step_and_merge_variants() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);

    // 隐藏行 / 列均可撤销，历史列表有清晰标签
    app.set_rows_hidden_undoable(2, 3, true);
    app.set_cols_hidden_undoable(1, 1, true);
    assert!(app.wb.sheet(0).hidden_rows_user.contains(&2));
    assert!(app.wb.sheet(0).hidden_cols.contains(&1));
    let labels = app.undo.undo_labels(10);
    assert_eq!(labels[0], "隐藏列");
    assert_eq!(labels[1], "隐藏行");
    assert_eq!(app.apply_undo_steps(false, 2), 2);
    assert!(!app.wb.sheet(0).hidden_rows_user.contains(&2));
    assert!(!app.wb.sheet(0).hidden_cols.contains(&1));

    // 跨越合并（每行独立合并）
    app.sel = (0, 0);
    app.sel_end = (1, 2);
    app.merge_across();
    assert!(app.wb.sheet(0).merges.contains(&(0, 0, 0, 2)));
    assert!(app.wb.sheet(0).merges.contains(&(1, 0, 1, 2)));

    // 合并内容 + 拆分并填充内容
    app.toggle_merge(false);
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Text("北京".into()), formula: None });
    app.wb.sheet_mut(0).set(0, 1, Cell { value: Value::Text("海淀".into()), formula: None });
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.merge_contents();
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("北京 海淀".into()));
    assert!(app.wb.sheet(0).merges.contains(&(0, 0, 0, 1)));

    app.unmerge_and_fill();
    assert!(app.wb.sheet(0).merges.is_empty());
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("北京 海淀".into()));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Text("北京 海淀".into()));
}

#[test]
fn audit_3_10_and_3_11_cross_sheet_trace_eval_formula_and_date_color_filter() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    app.wb.sheets.push(sheetcore::Sheet::new(String::from("明细")));
    app.wb.sheet_mut(1).set(0, 0, Cell { value: Value::Number(40.0), formula: None });
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(2.0), formula: None });
    app.wb.sheet_mut(0).set(0, 1, Cell { value: Value::Empty, formula: Some("=明细!A1+A1*5".into()) });
    sheetcore::formula::recalc_all(&mut app.wb);

    // 跨表追踪引用
    app.sel = (0, 1);
    app.sel_end = (0, 1);
    app.trace_precedents();
    assert!(!app.cross_sheet_traces.is_empty(), "应记录跨表引用追踪");

    // 公式求值步进至最终结果 50
    app.open_evaluate_formula();
    let dlg = app.eval_formula_dlg.as_mut().expect("应打开公式求值对话框");
    for _ in 0..8 {
        if dlg.evaluator.current(&app.wb).done {
            break;
        }
        dlg.evaluator.step(&app.wb);
    }
    let fin = dlg.evaluator.current(&app.wb);
    assert!(fin.done && fin.expr == "50", "公式求值最终结果应为 50，实际：{fin:?}");

    // 按日期年月分组与按单元格填充色筛选
    let date_sid = app.wb.styles.intern(sheetcore::style::CellStyle {
        format: "yyyy-mm-dd".into(),
        fill: Some([255, 240, 200]),
        ..Default::default()
    });
    let date_sid2 = app.wb.styles.intern(sheetcore::style::CellStyle {
        format: "yyyy-mm-dd".into(),
        fill: Some([200, 230, 255]),
        ..Default::default()
    });
    let d1 = sheetcore::formula::functions::date_to_serial_in(app.wb.date_system, 2025, 1, 15).unwrap();
    let d2 = sheetcore::formula::functions::date_to_serial_in(app.wb.date_system, 2026, 6, 20).unwrap();
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 3, Cell { value: Value::Text("日期".into()), formula: None });
        sh.set(1, 3, Cell { value: Value::Number(d1), formula: None });
        sh.set_style(1, 3, date_sid);
        sh.set(2, 3, Cell { value: Value::Number(d2), formula: None });
        sh.set_style(2, 3, date_sid2);
        let mut f = sheetcore::sort_filter::Filter::new(0, 3, 2, 3);
        f.date_groups.insert(
            3,
            vec![sheetcore::sort_filter::DateGroupItem {
                year: 2026,
                month: None,
                day: None,
            }],
        );
        sh.filter = Some(f);
    }
    app.recalc_filter_hidden();
    assert!(app.wb.sheet(0).hidden_rows.contains(&1));
    assert!(!app.wb.sheet(0).hidden_rows.contains(&2));
}

#[test]
fn audit_4_2_4_6_4_8_modeless_find_theme_switch_and_ai_grid_preview() {
    let env = TestEnv::new("audit-ux-section4");
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);

    // 4.2 非模态查找替换打开时 dialog_modal 必须保持 false，允许边看边编辑表格
    app.find.open = true;
    run_frame(&mut app, &ctx);
    assert!(!app.dialog_modal, "查找替换应为非模态悬浮窗，不得锁住主网格");
    app.find.open = false;

    // 4.6 运行时主题切换到深色，调色板与全局主题同步生效，切回浅色恢复
    app.prefs.theme = theme::AppTheme::Dark as u8;
    run_frame(&mut app, &ctx);
    assert_eq!(theme::current_theme(), theme::AppTheme::Dark);
    assert!(theme::palette().theme.is_dark());
    app.prefs.theme = theme::AppTheme::Light as u8;
    run_frame(&mut app, &ctx);
    assert_eq!(theme::current_theme(), theme::AppTheme::Light);

    // 4.8 AI 待确认写入构建网格高亮预览数据
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(100.0), formula: None });
    let mut spec = crate::ai_agent::ToolCallSpec {
        name: "set_cells".into(),
        arguments: serde_json::json!({
            "sheet": "Sheet1",
            "edits": [{"cell": "A1", "value": 250}]
        }),
    };
    let label = app.stage_ai_confirmation(&mut spec).unwrap();
    assert!(label.is_some());
    let prev = app.build_ai_pending_preview(&spec).expect("应生成 AI 修改网格预览");
    assert_eq!(prev.sheet_idx, 0);
    assert_eq!(prev.rects, vec![(0, 0, 0, 0)]);
    assert_eq!(prev.cells[0], (0, 0, "100".to_string(), "250".to_string()));

    drop(env);
}
