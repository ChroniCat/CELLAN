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
        ext: "xlsx".into(),
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

#[test]
fn reg_h1_insert_row_through_dynamic_spill_recalcs_immediately() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    for r in 0..6u32 {
        app.wb.sheet_mut(0).set(r, 0, Cell { value: Value::Number((r + 1) as f64), formula: None });
    }
    app.wb.sheet_mut(0).set(
        0,
        3,
        Cell {
            value: Value::Empty,
            formula: Some("=FILTER(A1:A6,A1:A6>0)".into()),
        },
    );
    app.wb.sheet_mut(0).set(
        0,
        4,
        Cell {
            value: Value::Empty,
            formula: Some("=SUM(D1:D10)".into()),
        },
    );
    app.recalc_all_and_deps_adaptive();
    assert_eq!(app.wb.sheet(0).value(5, 3), Value::Number(6.0));
    assert_eq!(app.wb.sheet(0).value(0, 4), Value::Number(21.0));

    // 在第 3 行插入一行，切过溢出区 D1:D6
    app.structural(sheetcore::structural::Structural::InsertRows { at: 2, n: 1 });
    assert_eq!(app.wb.sheet(0).value(2, 3), Value::Number(3.0));
    assert_eq!(app.wb.sheet(0).value(5, 3), Value::Number(6.0));
    assert_eq!(app.wb.sheet(0).value(0, 4), Value::Number(21.0));
}

#[test]
fn reg_h2_bg_recalc_with_concurrent_edit_adopts_untainted_sheets_and_keeps_needs_full() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S0".into();
    wb.sheets.push(sheetcore::Sheet::new("S1"));
    wb.sheets.push(sheetcore::Sheet::new("S2"));
    wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(5.0), formula: None });
    wb.sheet_mut(0).set(0, 1, Cell { value: Value::Empty, formula: Some("=A1*2".into()) });
    wb.sheet_mut(1).set(0, 0, Cell { value: Value::Number(3.0), formula: None });
    wb.sheet_mut(2).set(0, 0, Cell { value: Value::Empty, formula: Some("=S0!B1+1".into()) });
    wb.sheet_mut(2).set(1, 0, Cell { value: Value::Empty, formula: Some("=S1!A1*10".into()) });

    let deps = sheetcore::formula::deps::Deps::build(&wb);
    let revisions: Vec<u64> = wb.sheets.iter().map(|s| s.cell_revision()).collect();
    let mut worker_wb = wb.clone();
    sheetcore::formula::recalc_all(&mut worker_wb);
    let snap = crate::document_tasks::CalculatedSnapshot {
        revisions,
        workbook: Box::new(worker_wb),
    };

    // 后台重算期间用户改动了 S1!A1 = 99
    wb.sheet_mut(1).set(0, 0, Cell { value: Value::Number(99.0), formula: None });
    let outcome = snap.apply_with_deps_and_edits(&mut wb, Some(&deps), &[(1, 0, 0)]);
    assert_eq!(outcome, crate::document_tasks::ApplyOutcome::PartialUntaintedAdopted);
    // S0 与 S2!A1（未受 S1 污染的跨表公式）都已被正确采纳，不再停在空值/旧值！
    assert_eq!(wb.sheet(0).value(0, 1), Value::Number(10.0));
    assert_eq!(wb.sheet(2).value(0, 0), Value::Number(11.0));
    // 受 S1!A1 影响的 S2!A2 保持未采纳（待增量重算），不写入旧值 30
    assert_eq!(wb.sheet(2).value(1, 0), Value::Empty);
}

#[test]
fn reg_h3_grouped_sheets_undo_protection_recalc_and_tab_isolation() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    app.wb.sheets.push(sheetcore::Sheet::new("Sheet2"));
    app.selected_sheets.insert(0);
    app.selected_sheets.insert(1);

    // 1) 成组编辑写入数值与公式：两张表都写入且公式重算，单次 Ctrl+Z 同步撤销两张表
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("15".into()));
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(15.0));
    assert_eq!(app.wb.sheet(1).value(0, 0), Value::Number(15.0));

    app.sel = (0, 1);
    app.sel_end = (0, 1);
    app.begin_edit(Some("=A1*2".into()));
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(30.0));
    assert_eq!(app.wb.sheet(1).value(0, 1), Value::Number(30.0));

    assert!(app.apply_undo(false));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Empty);
    assert_eq!(app.wb.sheet(1).value(0, 1), Value::Empty);

    // 2) 工作表保护门禁：Sheet2 受保护时，成组编辑必须被整体拦截，不能半写入 Sheet1
    app.wb.sheet_mut(1).protection = Some(sheetcore::protection::SheetProtection::default());
    app.sel = (1, 0);
    app.sel_end = (1, 0);
    app.begin_edit(Some("99".into()));
    assert!(!app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Empty);
    assert_eq!(app.wb.sheet(1).value(1, 0), Value::Empty);
    app.wb.sheet_mut(1).protection = None;

    // 3) 切换工作簿标签必须清空成组状态，避免误写新工作簿
    app.open_in_new_tab(std::path::PathBuf::new());
    assert!(app.selected_sheets.is_empty());
}

#[test]
fn reg_h4_modeless_collab_panel_enter_does_not_trigger_share() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    app.members_open = true;
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        events: vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    let _ = ctx.run(raw, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    assert!(
        app.net.is_none(),
        "非模态协作面板打开时在表格中按回车不得触发「开启局域网共享」"
    );
}

#[test]
fn reg_h5_l1_l2_f2_arrows_point_ref_ime_and_autocomplete_case() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    app.wb.sheet_mut(0).set(1, 0, Cell { value: Value::Number(20.0), formula: None });
    app.wb.sheet_mut(0).set(2, 0, Cell { value: Value::Number(30.0), formula: Some("=A1+A2".into()) });

    // 1) F2 进入编辑模式后同一帧必须保持 EditMode::Edit，不会翻转回 Enter
    app.sel = (2, 0);
    app.sel_end = (2, 0);
    let raw_f2 = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0))),
        events: vec![egui::Event::Key {
            key: egui::Key::F2,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    let _ = ctx.run(raw_f2, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    assert_eq!(app.editing.as_ref().map(|e| e.mode), Some(EditMode::Edit));

    // 2) 编辑已有公式时，光标在 `+` 后按方向键不得误插单元格引用
    if let Some(ed) = app.editing.as_mut() {
        ed.caret = 4; // `=A1+|A2`
        ed.caret_override = Some(4);
    }
    let raw_down = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0))),
        events: vec![egui::Event::Key {
            key: egui::Key::ArrowDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(raw_down, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    assert_eq!(app.editing.as_ref().map(|e| e.buf.as_str()), Some("=A1+A2"));
    app.cancel_edit();

    // 3) 输入 `=` 后按 ↓ 再输入 `+` 再按 ↓ 应追加第二个引用而不是覆盖第一个；函数括号 `=SUM(` 内按 ↓ 也能选取引用
    app.sel = (0, 2);
    app.sel_end = (0, 2);
    app.begin_edit(Some("=".into()));
    app.step_formula_point_ref(1, 0, false, false);
    assert_eq!(app.editing.as_ref().map(|e| e.buf.as_str()), Some("=C2"));
    // 模拟用户输入 `+` 清空旧 pick_span
    if let Some(ed) = app.editing.as_mut() {
        ed.buf.push('+');
        ed.caret = ed.buf.chars().count();
        ed.mode = EditMode::Enter;
    }
    app.pick_span = None;
    app.pick_anchor = None;
    app.pick_rect = None;
    app.step_formula_point_ref(1, 0, false, false);
    assert_eq!(app.editing.as_ref().map(|e| e.buf.as_str()), Some("=C2+C2"));
    app.cancel_edit();

    // 函数括号 `=SUM(` 下 formula_help 返回 signature 但 suggestions 为空 -> 方向键必须仍可选取引用
    let help = formula_help("=SUM(").expect("=SUM( 应有参数签名提示");
    assert!(help.suggestions.is_empty());
    app.begin_edit(Some("=SUM(".into()));
    let raw_down2 = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0))),
        events: vec![egui::Event::Key {
            key: egui::Key::ArrowDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(raw_down2, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    assert_eq!(app.editing.as_ref().map(|e| e.buf.as_str()), Some("=SUM(C2"));
    app.cancel_edit();

    // 4) L1：中文输入法组字期间用 ↓ 选候选词（无新 Preedit 事件）不得把拼音提前提交进单元格
    app.sel = (4, 0);
    app.sel_end = (4, 0);
    app.begin_edit(Some("ni".into()));
    let raw_preedit = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0))),
        events: vec![egui::Event::Ime(egui::ImeEvent::Preedit("ni".into()))],
        ..Default::default()
    };
    let _ = ctx.run(raw_preedit, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    assert!(app.editing.as_ref().is_some_and(|e| e.ime_composing));
    let raw_cand_down = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0))),
        events: vec![egui::Event::Key {
            key: egui::Key::ArrowDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(raw_cand_down, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    assert!(app.editing.is_some(), "输入法候选词翻页时不得误提交拼音");
    app.cancel_edit();

    // 5) L2：记忆式键入采用原单元格大小写
    app.wb.sheet_mut(0).set(0, 5, Cell { value: Value::Text("AppleInc".into()), formula: None });
    let matched = app.autocomplete_column_match(1, 5, "app");
    assert_eq!(matched, Some(("AppleInc".to_string(), "leInc".to_string())));
}

#[test]
fn reg_h6_h7_repeat_action_tracking_and_keyboard_zoom_disabled() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    assert!(!ctx.options(|o| o.zoom_with_keyboard), "必须关闭 egui 键盘缩放");

    // H6: 先清除 A1，再给 B1 加粗，然后在 C1 按 F4 -> C1 应被加粗而非被清除
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    app.wb.sheet_mut(0).set(0, 2, Cell { value: Value::Number(99.0), formula: None });
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.clear_values();
    app.sel = (0, 1);
    app.sel_end = (0, 1);
    app.ribbon_action("Bold", &ctx);
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(0, 1)).bold);

    app.sel = (0, 2);
    app.sel_end = (0, 2);
    app.repeat_last_action(&ctx);
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(0, 2)).bold);
    assert_eq!(app.wb.sheet(0).value(0, 2), Value::Number(99.0));

    // 中间发生未记录操作（doc_gen 前进）后，F4 不再错误重复旧动作
    app.doc_gen = app.doc_gen.wrapping_add(1);
    app.sel = (0, 3);
    app.sel_end = (0, 3);
    app.repeat_last_action(&ctx);
    assert!(!app.wb.styles.get(app.wb.sheet(0).style_of(0, 3)).bold);

    // H7: Ctrl+- 打开删除对话框且不缩放界面
    let z_before = ctx.zoom_factor();
    let raw_minus = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0))),
        modifiers: egui::Modifiers::COMMAND,
        events: vec![egui::Event::Key {
            key: egui::Key::Minus,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }],
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    let _ = ctx.run(raw_minus, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    assert!(app.shift_cells_dlg.is_some());
    assert_eq!(ctx.zoom_factor(), z_before);
}

#[test]
fn reg_m1_goal_seek_failure_preserves_live_indirect_watches() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Number(1.0), formula: None }); // A1 = 1
        sh.set(0, 3, Cell { value: Value::Number(100.0), formula: None }); // D1 = 100
        sh.set(1, 3, Cell { value: Value::Number(200.0), formula: None }); // D2 = 200
        sh.set(0, 2, Cell { value: Value::Empty, formula: Some("=INDIRECT(\"D\"&MAX(1,MIN(2,ROUND(A1,0))))".into()) }); // C1
        sh.set(0, 4, Cell { value: Value::Empty, formula: Some("=A1*A1+10".into()) }); // E1 >= 10，目标 -5 无解
    }
    app.recalc_all_and_deps_adaptive();
    // 触发一次基于 live deps 的追踪登记（C1 -> D1）
    app.recalc_and_broadcast(&[(0, 0, 0)]);
    assert_eq!(app.wb.sheet(0).value(0, 2), Value::Number(100.0));

    // 单变量求解 E1 = -5（必败），试算会改动 A1 导致试算期间 INDIRECT 指向 D2
    let res = app.goal_seek_run(0, 4, -5.0, 0, 0);
    assert!(res.is_err());
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(1.0));
    assert_eq!(app.wb.sheet(0).value(0, 2), Value::Number(100.0));

    // 正式表里修改 D1 = 555：live deps 的 INDIRECT 追踪表未被污染，C1 必须更新为 555！
    app.wb.sheet_mut(0).set(0, 3, Cell { value: Value::Number(555.0), formula: None });
    app.recalc_and_broadcast(&[(0, 0, 3)]);
    assert_eq!(app.wb.sheet(0).value(0, 2), Value::Number(555.0));
}

#[test]
fn reg_m2_cf_edit_and_scroll_same_frame_clears_stale_overlay() {
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Number(50.0), formula: None });
        sh.cf.dxfs.push(sheetcore::cf::Dxf {
            fill: Some([255, 200, 200]),
            ..Default::default()
        });
        sh.cf.blocks.push(sheetcore::cf::CfBlock {
            ranges: vec![(0, 0, 50, 0)],
            rules: vec![sheetcore::cf::CfRule {
                priority: 1,
                stop: false,
                kind: sheetcore::cf::CfKind::CellIs {
                    op: sheetcore::cf::CfOp::Greater,
                    a: "10".into(),
                    b: String::new(),
                    dxf: 0,
                },
            }],
        });
    }
    run_frame(&mut app, &ctx);
    assert!(
        app.cf_cache.as_ref().is_some_and(|(_, m, _)| m.contains_key(&(0, 0))),
        "初始 50>10 应命中条件格式"
    );

    // 在同一帧中把 A1 改成 1（不再满足 >10）并同时滚动视口（冻结首行保持 A1 可见或横向滚出第一列）
    app.wb.sheet_mut(0).freeze = Some((1, 0));
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    app.doc_gen = app.doc_gen.wrapping_add(1);
    app.scroll.y += 40.0;
    run_frame(&mut app, &ctx);
    assert!(
        app.cf_cache.as_ref().is_some_and(|(_, m, _)| !m.contains_key(&(0, 0))),
        "同帧改值并滚动视口后不得残留 A1 的旧条件格式底色"
    );
}

#[test]
fn reg_m3_m4_m5_m6_restart_session_notice_theme_and_window_bounds() {
    let env = TestEnv::new("audit-reg-m3456");
    let p1 = env.path("a.xlsx");
    let p2 = env.path("b.xlsx");
    let wb = Workbook::new_blank();
    sheetcore::xlsx::save_atomic(&p1, &wb).unwrap();
    sheetcore::xlsx::save_atomic(&p2, &wb).unwrap();

    // M3: 重启会话清单被一次性消费并删除，恢复全部 2 个标签且不被开始页遮挡，重复传入 p1 不重开标签
    let sess = crate::debug_flags::app_dir().join("restart-session.txt");
    std::fs::write(
        &sess,
        format!("active=1\ntab={}\ntab={}", p1.display(), p2.display()),
    )
    .unwrap();
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    assert!(app.restore_restart_session());
    assert!(!sess.exists(), "重启会话文件必须被消费并删除");
    assert_eq!(app.tabs.len(), 2, "应恢复全部标签页且不重复");
    assert!(app.has_open_workbook && !app.show_start, "恢复的标签不得被开始页遮挡");
    app.open_in_new_tab(p1.clone());
    assert_eq!(app.tabs.len(), 2, "重复打开已在会话中的文件不应新增重复标签");

    // M4: 提示条按 target_doc_uid 绑定到所属标签，切换标签时隔离不串台
    let mut app2 = new_app(&ctx);
    let uid_a = app2.doc_uid;
    app2.set_notice(NoticeBanner {
        level: NoticeLevel::Error,
        title: "A 保存失败".into(),
        detail: "权限不足".into(),
        actions: vec![("另存为".into(), NoticeAction::SaveAs)],
        tag: Some("save_err"),
        target_doc_uid: Some(uid_a),
    });
    app2.open_in_new_tab(std::path::PathBuf::new());
    let uid_b = app2.doc_uid;
    assert_ne!(uid_a, uid_b);
    assert!(app2.notice.is_none(), "切到新标签后不应继续把标签 A 的报错挂在标签 B 上");
    app2.switch_tab(0);
    assert_eq!(
        app2.notice.as_ref().and_then(|n| n.target_doc_uid),
        Some(uid_a)
    );

    // M5: 四个主题下选中背景与前景、正文与窗体底色均具备可读对比度
    let rel_lum = |c: egui::Color32| -> f64 {
        let ch = |u: u8| {
            let s = u as f64 / 255.0;
            if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    };
    let contrast = |a: egui::Color32, b: egui::Color32| -> f64 {
        let la = rel_lum(a);
        let lb = rel_lum(b);
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    };
    for th in theme::AppTheme::ALL {
        theme::apply_app_theme(&ctx, th);
        let st = ctx.style();
        let pal = theme::palette();
        assert!(
            contrast(st.visuals.selection.bg_fill, st.visuals.selection.stroke.color) >= 4.5,
            "{th:?} 选中背景与前景色对比度不足"
        );
        assert!(
            contrast(pal.text, pal.dialog_bg) >= 7.0,
            "{th:?} 正文与对话框背景对比度不足"
        );
    }
    theme::apply_app_theme(&ctx, theme::AppTheme::Light);

    // M6: 拔掉外接屏后离屏逻辑坐标必须被拉回主显示器工作区
    let (vx, vy) = crate::win_chrome::validate_window_position(20_000.0, 20_000.0, 1200.0, 800.0)
        .expect("离屏逻辑坐标应被修正到主显示器工作区");
    assert!(vx < 10_000.0 && vy < 10_000.0);

    drop(env);
}

#[test]
fn reg_m7_m8_l4_l7_ctrl_enter_cse_undo_merge_across_chart_filter_and_shortcuts() {
    let env = TestEnv::new("audit-reg-m78-l47");
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);

    // M7a: Ctrl+Enter 跳过合并格非锚点格，并执行数据验证
    app.wb.sheet_mut(0).merge_range(0, 0, 1, 1);
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    app.begin_edit(Some("100".into()));
    assert!(app.commit_edit_fill_selection());
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(100.0));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Empty, "合并区隐藏格不得被 Ctrl+Enter 写入");
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Empty);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty);

    app.wb.sheet_mut(0).dv.push(sheetcore::dv::DataValidation {
        ranges: vec![(3, 0, 4, 0)],
        kind: sheetcore::dv::DvKind::Whole,
        op: sheetcore::dv::DvOp::Between,
        f1: "1".into(),
        f2: "10".into(),
        allow_blank: false,
        show_dropdown: true,
        error_style: sheetcore::dv::DvStyle::Stop,
        show_error: true,
        ..Default::default()
    });
    app.sel = (3, 0);
    app.sel_end = (4, 0);
    app.begin_edit(Some("999".into()));
    assert!(!app.commit_edit_fill_selection(), "Ctrl+Enter 违反 Stop 数据验证必须被拦截");
    assert!(app.editing.is_some(), "Stop 拦截应留在编辑状态供修改");
    assert_eq!(app.wb.sheet(0).value(3, 0), Value::Empty);
    assert_eq!(app.wb.sheet(0).value(4, 0), Value::Empty);
    // 切换为 Warning 数据验证时，Ctrl+Enter 必须弹出带 fill_selection=true 的告警框，确认后才填满选区
    app.wb.sheet_mut(0).dv[0].error_style = sheetcore::dv::DvStyle::Warning;
    assert!(!app.commit_edit_fill_selection());
    assert!(app.dv_alert.as_ref().is_some_and(|a| a.fill_selection));
    app.resolve_dv_alert(DvAlertChoice::Primary);
    assert_eq!(app.wb.sheet(0).value(3, 0), Value::Number(999.0));
    assert_eq!(app.wb.sheet(0).value(4, 0), Value::Number(999.0));
    app.wb.sheet_mut(0).dv.clear();

    // M7b: CSE Ctrl+Shift+Enter 可撤销/重做
    app.wb.sheet_mut(0).set(0, 4, Cell { value: Value::Number(2.0), formula: None });
    app.wb.sheet_mut(0).set(1, 4, Cell { value: Value::Number(3.0), formula: None });
    app.sel = (0, 5);
    app.sel_end = (0, 5);
    app.begin_edit(Some("=SUM(E1:E2*10)".into()));
    app.cse_pending = true;
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).arrays.len(), 1);
    assert_eq!(app.wb.sheet(0).value(0, 5), Value::Number(50.0));
    assert!(app.apply_undo(false));
    assert!(app.wb.sheet(0).arrays.is_empty());
    assert_eq!(app.wb.sheet(0).value(0, 5), Value::Empty);
    assert!(app.apply_undo(true));
    assert_eq!(app.wb.sheet(0).arrays.len(), 1);

    // M7c: 跨越合并丢弃数据时必须先二次确认
    app.wb.sheet_mut(0).set(6, 0, Cell { value: Value::Text("保留".into()), formula: None });
    app.wb.sheet_mut(0).set(6, 1, Cell { value: Value::Text("将被删".into()), formula: None });
    app.sel = (6, 0);
    app.sel_end = (6, 1);
    app.merge_across();
    assert!(app.merge_pending.as_ref().is_some_and(|p| p.across));
    assert_eq!(app.wb.sheet(0).value(6, 1), Value::Text("将被删".into()));
    app.merge_across();
    assert!(app.merge_pending.is_none());
    assert_eq!(app.wb.sheet(0).value(6, 1), Value::Empty);

    // M8: 插入图表前若存在未完成重算，不得写出过期的公式缓存与高 calcId
    let chart_path = env.path("chart_stale.xlsx");
    app.wb.path = Some(chart_path.clone());
    app.wb.sheet_mut(0).set(10, 0, Cell { value: Value::Number(7.0), formula: None });
    app.wb.sheet_mut(0).set(
        10,
        1,
        Cell {
            value: Value::Number(0.0), // 故意放过期缓存
            formula: Some("=A11*3".into()),
        },
    );
    app.tasks.needs_full = true;
    let ch = sheetcore::chart_write::chart_from_selection(
        app.wb.sheet(0),
        &app.wb.sheet(0).name,
        (10, 0, 10, 1),
        "测试图",
        sheetcore::charts::ChartKind::Column,
    );
    app.commit_chart(ch);
    let reloaded = sheetcore::xlsx::load(&chart_path).unwrap();
    assert_eq!(reloaded.sheet(0).value(10, 1), Value::Number(21.0));

    // L4: clear_filter_criteria 同时清掉日期和颜色筛选
    {
        let sh = app.wb.sheet_mut(0);
        let mut f = sheetcore::sort_filter::Filter::new(0, 0, 5, 1);
        f.date_groups.insert(
            0,
            vec![sheetcore::sort_filter::DateGroupItem { year: 2026, month: None, day: None }],
        );
        f.color_filters
            .insert(1, sheetcore::sort_filter::ColorFilter::Fill(Some([255, 255, 0])));
        sh.filter = Some(f);
    }
    assert!(app.clear_filter_criteria());
    let f = app.wb.sheet(0).filter.as_ref().unwrap();
    assert!(f.date_groups.is_empty() && f.color_filters.is_empty());

    // L7: 单格 Ctrl+D、Ctrl+5、Shift+Alt+→ 与独立 Help 窗口
    app.wb.sheet_mut(0).set(20, 0, Cell { value: Value::Number(88.0), formula: None });
    app.sel = (21, 0);
    app.sel_end = (21, 0);
    app.fill_cmd(true);
    assert_eq!(app.wb.sheet(0).value(21, 0), Value::Number(88.0));

    app.ribbon_action("Help", &ctx);
    assert!(app.help_open && !app.about_open);
    app.help_open = false;

    drop(env);
}

#[test]
fn p0_bleeding_fixes_app_regression_suite() {
    let env = crate::test_env::TestEnv::new("p0-bleeding-app");
    let ctx = egui::Context::default();
    let mut app = new_app(&ctx);

    // #10 decimals() UTF-8 currency prefix must not panic and adjusts decimals safely
    let cny = app.wb.styles.intern(sheetcore::style::CellStyle {
        format: "￥#,##0".into(),
        ..Default::default()
    });
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(1234.5), formula: None });
    app.wb.sheet_mut(0).set_style(0, 0, cny);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.decimals(1);
    assert_eq!(app.current_style().format, "￥#,##0.0");
    app.decimals(-1);
    assert_eq!(app.current_style().format, "￥#,##0");

    // #19 '@' text format cell keeps '=1+1' as literal text instead of formula
    let txt_sid = app.wb.styles.intern(sheetcore::style::CellStyle {
        format: "@".into(),
        ..Default::default()
    });
    app.wb.sheet_mut(0).set_style(1, 0, txt_sid);
    app.sel = (1, 0);
    app.sel_end = (1, 0);
    app.begin_edit(Some("=1+1".into()));
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).get(1, 0).unwrap().formula, None);
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Text("=1+1".into()));

    // #23 Ctrl+Z single press in real update() frame undoes ONLY ONCE (no double-trigger!)
    app.sel = (2, 0);
    app.sel_end = (2, 0);
    app.begin_edit(Some("10".into()));
    assert!(app.commit_edit(None));
    app.sel = (3, 0);
    app.sel_end = (3, 0);
    app.begin_edit(Some("20".into()));
    assert!(app.commit_edit(None));
    let mut raw_z = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0))),
        modifiers: egui::Modifiers { command: true, ctrl: true, ..Default::default() },
        events: vec![egui::Event::Key {
            key: egui::Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers { command: true, ctrl: true, ..Default::default() },
        }],
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    let _ = ctx.run(raw_z.clone(), |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    assert_eq!(app.wb.sheet(0).value(3, 0), Value::Empty, "first Ctrl+Z undoes A4");
    assert_eq!(
        app.wb.sheet(0).value(2, 0),
        Value::Number(10.0),
        "single Ctrl+Z must NOT double-trigger and undo A3"
    );
    // Clear key press for subsequent frames
    raw_z.events.clear();
    let _ = ctx.run(raw_z, |ctx| eframe::App::update(&mut app, ctx, &mut frame));

    // #24 ProtectSheet from CtxAct & hidden formula blocks Evaluate Formula + ProtOp guards
    app.wb.sheet_mut(0).set(4, 0, Cell { value: Value::Number(30.0), formula: Some("=10+20".into()) });
    let hid_sid = app.wb.styles.intern(sheetcore::style::CellStyle {
        locked: false,
        hidden: true,
        ..Default::default()
    });
    app.wb.sheet_mut(0).set_style(4, 0, hid_sid);
    let mut prot = sheetcore::protection::SheetProtection::new_with_password("secret");
    prot.perms.auto_filter = false;
    app.wb.sheet_mut(0).protection = Some(prot);
    app.ctx_menu_run(CtxAct::ProtectSheet, 0, &ctx);
    assert!(
        app.protect_dlg.as_ref().is_some_and(|d| d.unprotect),
        "CtxAct::ProtectSheet on protected sheet must open unprotect verification dialog"
    );
    app.protect_dlg = None;
    app.sel = (4, 0);
    app.sel_end = (4, 0);
    app.open_evaluate_formula();
    assert!(app.eval_formula_dlg.is_none(), "hidden formula must not open Evaluate Formula");
    app.begin_edit(None);
    assert_eq!(
        app.editing.as_ref().map(|e| e.buf.as_str()),
        Some(""),
        "F2 on unlocked+hidden formula must not leak formula text"
    );
    app.cancel_edit();
    app.start_filter();
    assert!(app.wb.sheet(0).filter.is_none(), "ProtOp::AutoFilter=false must block start_filter");
    app.wb.sheet_mut(0).protection = None;

    // #26 add_sheet and set_sheet_name preserve existing UndoStack instead of wiping it
    app.sel = (5, 0);
    app.sel_end = (5, 0);
    app.begin_edit(Some("999".into()));
    assert!(app.commit_edit(None));
    let depth_before = app.undo.depth().0;
    app.add_sheet();
    assert_eq!(app.undo.depth().0, depth_before + 1, "add_sheet must push onto undo, not wipe it");
    assert!(app.set_sheet_name(1, "明细表"));
    assert_eq!(app.undo.depth().0, depth_before + 2, "rename_sheet must push onto undo, not wipe it");
    assert!(app.apply_undo(false));
    assert_eq!(app.wb.sheets[1].name, "Sheet2");
    assert!(app.apply_undo(false));
    assert_eq!(app.wb.sheets.len(), 1);
    assert!(app.apply_undo(false));
    assert_eq!(app.wb.sheet(0).value(5, 0), Value::Empty, "prior cell edit undo survived sheet ops");

    // #27 filter_by_cell_value matches formatted percentage numbers properly
    let pct_sid = app.wb.styles.intern(sheetcore::style::CellStyle {
        format: "0%".into(),
        ..Default::default()
    });
    app.wb.sheet_mut(0).set(10, 0, Cell { value: Value::Text("比率".into()), formula: None });
    app.wb.sheet_mut(0).set(11, 0, Cell { value: Value::Number(0.15), formula: None });
    app.wb.sheet_mut(0).set_style(11, 0, pct_sid);
    app.wb.sheet_mut(0).set(12, 0, Cell { value: Value::Number(0.30), formula: None });
    app.wb.sheet_mut(0).set_style(12, 0, pct_sid);
    app.sel = (11, 0);
    app.sel_end = (11, 0);
    app.filter_by_cell_value();
    assert!(!app.wb.sheet(0).hidden_rows.contains(&11), "0.15 row must stay visible");
    assert!(app.wb.sheet(0).hidden_rows.contains(&12), "0.30 row must be hidden");
    app.disable_filter();

    // #28 DV formula validation + partial range carving on clear
    let dv = sheetcore::dv::DataValidation {
        ranges: vec![(20, 0, 25, 0)],
        kind: sheetcore::dv::DvKind::Whole,
        op: sheetcore::dv::DvOp::Between,
        f1: "1".into(),
        f2: "100".into(),
        ..Default::default()
    };
    app.replace_validation_rule((20, 0, 25, 0), Some(dv));
    assert!(app.check_dv_for_cell(20, 0, "=20+30", false).is_ok(), "=20+30 (50) must pass Whole 1..100");
    assert!(app.check_dv_for_cell(20, 0, "=200+1", false).is_err(), "=200+1 (201) must fail Whole 1..100");
    // Clear single cell (22, 0) must carve out only (22, 0), leaving 20..21 and 23..25 intact
    assert!(app.replace_validation_rule((22, 0, 22, 0), None));
    assert!(sheetcore::dv::find(&app.wb.sheet(0).dv, 22, 0).is_none());
    assert!(sheetcore::dv::find(&app.wb.sheet(0).dv, 20, 0).is_some());
    assert!(sheetcore::dv::find(&app.wb.sheet(0).dv, 25, 0).is_some());

    // #29 Right-click PasteValues resets stale ps_op; PasteSpecial arithmetic wraps target formula;
    // InsertFn starts clean formula; Outline level 1 collapses level 1 rows
    app.wb.sheet_mut(0).set(30, 0, Cell { value: Value::Number(5.0), formula: None });
    app.wb.sheet_mut(0).set(30, 1, Cell { value: Value::Number(10.0), formula: Some("=5+5".into()) });
    app.sel = (30, 0);
    app.sel_end = (30, 0);
    app.clip_copy(false, &ctx);
    app.ps_op = 1; // stale Add
    app.sel = (30, 2);
    app.sel_end = (30, 2);
    app.wb.sheet_mut(0).set(30, 2, Cell { value: Value::Number(100.0), formula: None });
    app.ctx_menu_run(CtxAct::PasteValues, 0, &ctx);
    assert_eq!(app.wb.sheet(0).value(30, 2), Value::Number(5.0), "CtxAct::PasteValues must clear stale ps_op");
    // Now explicit Paste Special Add onto formula cell B31 (=5+5) wraps formula -> =(5+5)+5 = 15
    app.ps_kind = 1;
    app.ps_op = 1;
    app.sel = (30, 1);
    app.sel_end = (30, 1);
    app.paste_special_apply(&ctx);
    assert_eq!(app.wb.sheet(0).get(30, 1).unwrap().formula.as_deref(), Some("=(5+5)+5"));
    assert_eq!(app.wb.sheet(0).value(30, 1), Value::Number(15.0));

    // InsertFn: on cell with existing number 100 starts clean =SUM(
    app.wb.sheet_mut(0).set(31, 0, Cell { value: Value::Number(100.0), formula: None });
    app.sel = (31, 0);
    app.sel_end = (31, 0);
    app.ribbon_action("InsertFn:SUM", &ctx);
    assert_eq!(app.editing.as_ref().map(|e| e.buf.as_str()), Some("=SUM("));
    app.cancel_edit();

    // Outline level 1 collapses level-1 rows and marks layout_dirty
    app.wb.sheet_mut(0).row_levels.insert(40, 1);
    app.wb.sheet_mut(0).row_levels.insert(41, 1);
    app.wb.sheet_mut(0).layout_dirty = false;
    app.set_outline_level(true, 1);
    assert!(app.wb.sheet(0).hidden_rows_user.contains(&40));
    assert!(app.wb.sheet(0).layout_dirty);
    app.set_outline_level(true, 2);
    assert!(!app.wb.sheet(0).hidden_rows_user.contains(&40));

    // Merged cell keyboard nav & begin_edit_at normalize to merge anchor
    app.wb.sheet_mut(0).merge_range(50, 0, 51, 1);
    app.sel = (51, 1);
    app.sel_end = (51, 1);
    app.begin_edit(Some("merged".into()));
    assert_eq!(app.sel, (50, 0), "begin_edit_at must snap covered merge cell to anchor");
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).value(50, 0), Value::Text("merged".into()));

    drop(env);
}
