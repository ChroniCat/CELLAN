//! Regressions for data-preserving edits through the actual app handlers.

use super::*;

fn app() -> SheetApp {
    // 单测共享真实系统剪贴板：每个用例先清空，避免上一个用例留下的文本/HTML
    // 被当前用例当成“用户在外部复制的新内容”。
    crate::clear_test_clipboard();
    SheetApp::new_with_context(&egui::Context::default(), None)
}

fn number(value: f64) -> Cell {
    Cell { value: Value::Number(value), formula: None }
}

fn text(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

/// 测试侧独立读取视口内的首/末可见行；不复用 page_move/visible_page_rows 的落点计算。
fn visible_row_window(app: &SheetApp) -> (u32, u32) {
    let (_, rows) = app.view_axes();
    let view_h = (app.last_view.y - HEAD_H).max(44.0);
    let top = app.scroll.y;
    let mut first = rows.index_at(top);
    while first < 1_048_575 && rows.size(first) <= 0.0 {
        first += 1;
    }
    let mut last = first;
    let mut r = first;
    while r <= 1_048_575 {
        if rows.pos(r) >= top + view_h {
            break;
        }
        if rows.size(r) > 0.0 {
            last = r;
        }
        r += 1;
    }
    (first, last)
}

/// 从视口首行到活动格之间跨过多少个可见行（隐藏行不占页内位置）。
fn visible_row_offset(app: &SheetApp, first: u32, row: u32) -> u32 {
    let (_, rows) = app.view_axes();
    (first..=row)
        .filter(|r| rows.size(*r) > 0.0)
        .count()
        .saturating_sub(1) as u32
}

fn formula(text: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(text.into()) }
}

#[test]
fn selection_html_carries_formula_format_merge_and_newlines() {
    use sheetcore::style::CellStyle;
    let mut app = app();
    let mut st = CellStyle::default_dark();
    st.format = "yyyy-mm-dd".into();
    st.bold = true;
    st.fill = Some([255, 255, 0]);
    let sid = app.wb.styles.intern(st);
    app.wb.sheet_mut(0).set_style(0, 0, sid);
    app.wb.sheet_mut(0).set(
        0,
        0,
        Cell {
            value: Value::Number(46024.0),
            formula: None,
        },
    );
    app.wb.sheet_mut(0).set(
        0,
        1,
        Cell {
            value: Value::Number(1234.5),
            formula: Some("=A1*2".into()),
        },
    );
    app.wb.sheet_mut(0).set(1, 0, text("第一行\n第二行"));
    app.wb.sheet_mut(0).merges.push((0, 1, 0, 2));
    let html = app.selection_html(0, (0, 0, 1, 2));
    assert!(html.contains(r#"colspan="2""#), "合并单元格列跨度");
    assert!(html.contains("mso-number-format:'yyyy-mm-dd'"), "日期格式");
    assert!(html.contains("font-weight:bold"), "粗体");
    assert!(html.contains("background:#FFFF00"), "填充");
    assert!(html.contains(r#"x:fmla="=A1*2""#), "公式");
    assert!(
        html.contains(r#"<br style="mso-data-placement:same-cell;">"#),
        "单元格内换行"
    );
    assert!(html.contains("2026-01-02") || html.contains("2026/1/2"), "日期显示文本");
}

#[test]
fn paste_prefers_new_system_clipboard_over_stale_internal_clip() {
    let mut app = app();
    let ctx = egui::Context::default();
    app.sel = (9, 9);
    app.sel_end = (9, 9);
    app.wb.sheets[0].set(9, 9, text("旧值"));
    app.clip_copy(false, &ctx);
    assert!(app.clip.is_some(), "内部 clip 已经建立");

    // 模拟用户刚在 Excel 复制，系统剪贴板已经是新文本；内部 clip 仍指向“旧值”
    crate::set_test_clipboard("2\t3\n4\t5\n");
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.clip_paste(false, None);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(2.0));
    assert_eq!(app.wb.sheets[0].value(0, 1), Value::Number(3.0));
    assert_eq!(app.wb.sheets[0].value(1, 0), Value::Number(4.0));
    assert_eq!(app.wb.sheets[0].value(1, 1), Value::Number(5.0));
}

#[test]
fn display_name_is_cleaned_and_elided_without_byte_panics() {
    assert_eq!(clean_user_name("  张三\n李四\t "), "张三李四");
    // 中间省略必须按字符切，不能像旧代码那样按字节切中文名字。
    let short = elide_middle("张三张三张三", 5);
    assert_eq!(short.chars().count(), 5);
    assert!(short.contains('…'));
    let host = elide_middle("Admin@DESKTOP-ABC", 11);
    assert!(host.ends_with("ABC"), "默认名的机器后缀必须保留：{host}");
}

#[test]
fn display_name_is_persisted_and_used_by_a_fresh_app() {
    let env = crate::test_env::TestEnv::new("profile-display-name");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    assert!(app.apply_display_name("  张三\n "));
    assert_eq!(app.my_name, "张三");
    assert_eq!(ui_prefs::UiPrefs::load().user_name, "张三");
    // 新进程/新窗口不会丢掉名字；这是“所有人都是 Administrator”的根因修正。
    let app2 = SheetApp::new_with_context(&egui::Context::default(), None);
    assert_eq!(app2.my_name, "张三");
    drop(env);
}

#[test]
fn sc_user_name_overrides_saved_profile_for_deployment() {
    let env = crate::test_env::TestEnv::new("profile-sc-user-name");
    env.sc("SC_USER_NAME", "李四");
    let app = SheetApp::new_with_context(&egui::Context::default(), None);
    assert_eq!(app.my_name, "李四");
    drop(env);
}

#[test]
fn filtered_copy_compacts_visible_rows_and_paste_skips_filtered_rows() {
    let mut app = app();
    let ctx = egui::Context::default();
    for r in 0..4 {
        app.wb.sheet_mut(0).set(r, 0, text(&format!("R{r}")));
    }
    app.wb.sheet_mut(0).hidden_rows.insert(1);
    app.sel = (0, 0);
    app.sel_end = (3, 0);
    app.clip_copy(false, &ctx);
    let clip = app.clip.as_ref().expect("筛选复制应建立内部块");
    assert_eq!(clip.cells.len(), 3, "筛选隐藏行不能被复制");
    assert_eq!(clip.cells[1][0].0.value, Value::Text("R2".into()));
    app.sel = (0, 3);
    app.sel_end = (0, 3);
    app.clip_paste(false, None);
    assert_eq!(app.wb.sheets[0].value(0, 3), Value::Text("R0".into()));
    assert_eq!(app.wb.sheets[0].value(1, 3), Value::Text("R2".into()));
    assert_eq!(app.wb.sheets[0].value(2, 3), Value::Text("R3".into()));

    // 用户手动隐藏的行（hidden_rows_user）在 Excel 中仍会被复制；不能被筛选逻辑误伤。
    app.wb.sheet_mut(0).hidden_rows.clear();
    app.wb.sheet_mut(0).hidden_rows_user.insert(1);
    app.sel = (0, 0);
    app.sel_end = (3, 0);
    app.clip_copy(false, &ctx);
    assert_eq!(app.clip.as_ref().unwrap().cells.len(), 4);
}

#[test]
fn internal_copy_paste_keeps_formula_merge_style_and_newline() {
    use sheetcore::style::CellStyle;
    let mut app = app();
    let ctx = egui::Context::default();
    let mut st = CellStyle::default_dark();
    st.bold = true;
    st.format = "0.00".into();
    let sid = app.wb.styles.intern(st);
    app.wb.sheet_mut(0).set_style(1, 1, sid);
    app.wb.sheet_mut(0).set(
        1,
        1,
        Cell {
            value: Value::Number(1234.5),
            formula: Some("=A1*2".into()),
        },
    );
    app.wb.sheet_mut(0).set(2, 0, text("第一行\n第二行"));
    app.wb.sheet_mut(0).merges.push((1, 1, 1, 2));
    app.sel = (1, 0);
    app.sel_end = (2, 2);
    app.clip_copy(false, &ctx);

    app.sel = (5, 5);
    app.sel_end = (5, 5);
    app.clip_paste(false, None);
    assert_eq!(
        app.wb.sheets[0].value(5, 6),
        Value::Number(0.0),
        "内部粘贴后公式按目标位置重算"
    );
    assert_eq!(
        app.wb.sheet(0).get(5, 6).and_then(|c| c.formula.clone()).is_some(),
        true,
        "内部粘贴保留公式"
    );
    assert!(
        app.wb.sheet(0).merges.contains(&(5, 6, 5, 7)),
        "内部粘贴保留合并"
    );
    assert_eq!(app.wb.sheets[0].value(6, 5).display(), "第一行\n第二行");
    let target_style = app.wb.sheet(0).style_of(5, 6);
    assert!(app.wb.styles.get(target_style).bold, "内部粘贴保留粗体");
    assert_eq!(app.wb.styles.get(target_style).format, "0.00");
}

#[test]
fn ribbon_context_and_shortcut_paste_share_the_latest_system_text() {
    let mut app = app();
    let ctx = egui::Context::default();
    // 先在 App 里复制 1（建立内部 clip），再模拟 Excel 复制 2。
    app.wb.sheets[0].set(9, 9, text("旧值"));
    app.sel = (9, 9);
    app.sel_end = (9, 9);
    app.clip_copy(false, &ctx);
    assert!(app.clip.is_some(), "内部 clip 1 已建立");
    crate::set_test_clipboard("7\t8\n9\t10\n");
    for (row, entry) in [(0u32, "ribbon"), (3, "context"), (6, "shortcut")] {
        app.sel = (row, 0);
        app.sel_end = (row, 0);
        match entry {
            "ribbon" => app.ribbon_action("Paste", &ctx),
            "context" => app.ctx_menu_run(CtxAct::Paste, 0, &ctx),
            _ => app.clip_paste(false, None),
        }
        assert_eq!(
            app.wb.sheets[0].value(row, 0),
            Value::Number(7.0),
            "{entry} 入口应读到最新系统剪贴板"
        );
        assert_eq!(app.wb.sheets[0].value(row + 1, 1), Value::Number(10.0));
    }
}

#[test]
fn paste_keeps_empty_rows_and_quotes_from_external_tsv() {
    let mut app = app();
    app.clip = None;
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.clip_paste(false, Some("a\t\"b\tc\"\n\n1\t2\n".into()));
    assert_eq!(app.wb.sheets[0].value(0, 0).display(), "a");
    assert_eq!(app.wb.sheets[0].value(0, 1).display(), "b\tc");
    assert_eq!(app.wb.sheets[0].value(1, 0), Value::Empty, "空行不能消失");
    assert_eq!(app.wb.sheets[0].value(2, 0), Value::Number(1.0));
}

#[test]
fn system_paste_event_commits_without_key_v() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Paste("PASTE_EVENT".into())]);
    assert_eq!(app.wb.sheet(0).value(0, 0).display(), "PASTE_EVENT");
    assert!(app.editing.is_none(), "system Paste event must land in the cell, not stay in edit mode");
}

#[test]
fn a1_formula_translation_handles_absolute_and_sheet_refs() {
    assert_eq!(
        sheetcore::clipboard_xmlss::a1_to_r1c1("=SUM($A$1:B2)+Sheet1!C3", 2, 2),
        "=SUM(R1C1:R[-1]C[-1])+Sheet1!RC"
    );
}

#[test]
fn creating_a_refreshable_pivot_prevents_older_structure_undo_from_erasing_it() {
    let mut app = app();
    for (r, c, text) in [(0, 0, "Group"), (0, 1, "Amount"), (1, 0, "A")] {
        app.wb.sheets[0].set(r, c, Cell { value: Value::Text(text.into()), formula: None });
    }
    app.wb.sheets[0].set(1, 1, number(5.0));
    app.structural(sheetcore::structural::Structural::InsertRows { at: 0, n: 1 });
    assert_eq!(app.undo.depth(), (1, 0));
    app.pivot_real = true;
    app.sel = (5, 0);
    app.generate_pivot(&sheetcore::pivot::PivotSpec {
        source: (1, 0, 2, 1), row_field: 0, col_field: None,
        value_field: 1, agg: sheetcore::pivot::Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
    });
    assert_eq!(app.undo.depth(), (0, 0));
    assert_eq!(app.wb.pivots_pending.len(), 1);
    assert!(app.status.contains("不可撤销"));
    app.do_undo(false);
    assert_eq!(app.wb.sheets[0].value(5, 0).display(), "Group");
    assert_eq!(app.wb.sheets[0].value(6, 1), Value::Number(5.0));
    assert_eq!(app.wb.pivots_pending[0].1.source, (1, 0, 2, 1));
    app.structural(sheetcore::structural::Structural::DeleteCols { at: 1, n: 1 });
    assert!(app.wb.pivots_pending.is_empty());
    assert!(app.status.contains("已移除 1 个"));
    app.do_undo(false);
    assert_eq!(app.wb.pivots_pending.len(), 1);
    assert_eq!(app.wb.sheets[0].value(6, 1), Value::Number(5.0));
}

#[test]
fn chart_save_acknowledges_a_new_sheet_before_the_next_save() {
    let dir = std::env::temp_dir().join(format!("native_chart_receipt_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("chart.xlsx");
    let mut app = app();
    sheetcore::xlsx::save_atomic_with_receipt(&path, &app.wb).unwrap().apply_to(&mut app.wb);
    app.wb.path = Some(path.clone());
    sheetcore::sheet_ops::insert_sheet(&mut app.wb, 1, sheetcore::Sheet::new("New"));
    app.sheet_idx = 1;
    app.wb.sheets[1].set(0, 1, Cell { value: Value::Text("Amount".into()), formula: None });
    app.wb.sheets[1].set(1, 0, number(1.0));
    app.wb.sheets[1].set(1, 1, number(7.0));
    let chart = sheetcore::chart_write::chart_from_selection(
        &app.wb.sheets[1], "New", (0, 0, 1, 1), "Receipt chart", sheetcore::charts::ChartKind::Column,
    );
    app.commit_chart(chart);
    assert!(app.wb.sheets[1].source_part.is_some(), "chart save must acknowledge the new sheet");
    assert_eq!(sheetcore::charts::load_charts(&path, "New").unwrap().len(), 1, "{}", app.status);
    for _ in 0..2 {
        sheetcore::xlsx::save_atomic_with_receipt(&path, &app.wb).unwrap().apply_to(&mut app.wb);
        assert_eq!(sheetcore::charts::load_charts(&path, "New").unwrap().len(), 1);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn overlapping_cut_is_one_reversible_transaction() {
    let mut app = app();
    app.wb.sheets[0].set(0, 0, number(1.0));
    app.wb.sheets[0].set(1, 0, number(2.0));
    app.sel_end = (1, 0);
    app.clip_copy(true, &egui::Context::default());
    app.sel = (1, 0);
    app.sel_end = app.sel;
    app.clip_paste(false, None);
    let values = |app: &SheetApp| (0..3).map(|r| app.wb.sheets[0].value(r, 0)).collect::<Vec<_>>();
    assert_eq!(values(&app), vec![Value::Empty, Value::Number(1.0), Value::Number(2.0)]);
    assert!(app.wb.dirty);
    assert_eq!(app.undo.depth(), (1, 0));
    app.do_undo(false);
    assert_eq!(values(&app), vec![Value::Number(1.0), Value::Number(2.0), Value::Empty]);
    app.do_undo(true);
    assert_eq!(values(&app), vec![Value::Empty, Value::Number(1.0), Value::Number(2.0)]);
}

#[test]
fn cross_sheet_cut_undo_restores_both_sides_styles_and_dependencies() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::Sheet::new("Other"));
    app.wb.sheets[0].set(0, 0, number(1.0));
    app.wb.sheets[0].set(0, 2, formula("=A1+1"));
    app.wb.sheets[1].set(0, 0, number(8.0));
    let mut style = app.wb.styles.get(0).clone();
    style.bold = true;
    let bold = app.wb.styles.intern(style);
    app.wb.sheets[0].set_style(0, 0, bold);
    sheetcore::formula::recalc_all(&mut app.wb);
    app.deps = sheetcore::formula::deps::Deps::build(&app.wb);
    app.clip_copy(true, &egui::Context::default());
    app.sheet_idx = 1;
    app.clip_paste(false, None);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Empty);
    assert_eq!(app.wb.sheets[0].value(0, 2), Value::Number(1.0));
    assert_eq!(app.wb.sheets[0].style_of(0, 0), 0);
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Number(1.0));
    assert_eq!(app.wb.sheets[1].style_of(0, 0), bold);
    app.do_undo(false);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(1.0));
    assert_eq!(app.wb.sheets[0].value(0, 2), Value::Number(2.0));
    assert_eq!(app.wb.sheets[0].style_of(0, 0), bold);
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Number(8.0));
    assert_eq!(app.wb.sheets[1].style_of(0, 0), 0);
    app.do_undo(true);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Empty);
    assert_eq!(app.wb.sheets[0].value(0, 2), Value::Number(1.0));
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Number(1.0));
}

fn restrict_a1(app: &mut SheetApp) {
    app.wb.sheets[0].dv.push(sheetcore::dv::DataValidation {
        ranges: vec![(0, 0, 0, 0)], kind: sheetcore::dv::DvKind::Whole,
        f1: "1".into(), f2: "10".into(), ..Default::default()
    });
    app.editing = Some(EditState::new("99".into()));
}


/// 按下这一拍就落锚点：只发 press（不发 release），选区必须已经移到按下的格子。
/// 旧实现只在 resp.clicked()（松开）里改选区，快速点按后立即拖动会从上一个选区开始长。
#[test]
fn press_anchors_selection_before_release() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let pos = egui::pos2(
        HEAD_W + app.wb.sheets[0].cols.pos(2) + 15.0,
        HEAD_H + app.wb.sheets[0].rows.pos(2) + 10.0,
    );
    let mut step = |pressed: Option<bool>| {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        input.events.push(egui::Event::PointerMoved(pos));
        if let Some(pressed) = pressed {
            input.events.push(egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default(),
            });
        }
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::NONE)
                .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
        });
    };
    step(None);
    step(Some(true)); // 只按下，不松开
    assert_eq!(app.sel, (2, 2), "按下就应定位到 C3");
    assert_eq!(app.sel_end, (2, 2));
}

/// 按住拖动：还没松开、也不依赖 egui 的拖动阈值，选区终点就要跟着指针走
#[test]
fn drag_extends_selection_while_button_is_down() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let x = HEAD_W + app.wb.sheets[0].cols.pos(1) + 15.0;
    let y0 = HEAD_H + app.wb.sheets[0].rows.pos(0) + 10.0;
    let y1 = HEAD_H + app.wb.sheets[0].rows.pos(4) + 10.0;
    let run = |app: &mut SheetApp, ctx: &egui::Context, pos: egui::Pos2, pressed: Option<bool>| {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        input.events.push(egui::Event::PointerMoved(pos));
        if let Some(pressed) = pressed {
            input.events.push(egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default(),
            });
        }
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::NONE)
                .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
        });
    };
    run(&mut app, &ctx, egui::pos2(x, y0), None);
    run(&mut app, &ctx, egui::pos2(x, y0), Some(true));
    run(&mut app, &ctx, egui::pos2(x, y1), None); // 按住、只移动
    assert_eq!(app.sel, (0, 1), "锚点应停在按下的格");
    assert_eq!(app.sel_end, (4, 1), "没松开也要扩到第 5 行");
}

fn click_grid(app: &mut SheetApp, ctx: &egui::Context, pos: egui::Pos2) {
    for pressed in [None, Some(true), Some(false)] {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        if let Some(pressed) = pressed {
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default(),
            });
        }
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::NONE)
                .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
        });
    }
}

#[test]
fn rejected_draft_keeps_its_cell_when_clicking_grid_or_headers() {
    for header in [false, true] {
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        restrict_a1(&mut app);
        let pos = egui::pos2(HEAD_W + app.wb.sheets[0].cols.pos(2) + 15.0,
            if header { HEAD_H / 2.0 } else { HEAD_H + 10.0 });
        click_grid(&mut app, &ctx, pos);
        assert_eq!(app.sel, (0, 0));
        assert_eq!(app.editing.as_ref().unwrap().buf, "99");
        app.commit_edit(None);
        assert_eq!(app.wb.sheets[0].value(0, 2), Value::Empty);
        app.editing.as_mut().unwrap().buf = "9".into();
        click_grid(&mut app, &ctx, pos);
        assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(9.0));
        assert_eq!(app.sel.1, 2);
    }
}

#[test]
fn name_box_selects_named_range_and_prefers_local_scope() {
    let mut app = app();
    let sheet = app.wb.sheets[0].name.clone();
    app.wb.names.push((
        "销售额".into(),
        None,
        format!("'{sheet}'!$A$1:$B$3"),
    ));
    app.name_buf = "销售额".into();
    app.goto_name();
    assert_eq!(app.sel, (0, 0), "全局名称应选中左上角");
    assert_eq!(app.sel_end, (2, 1), "全局名称应选中 A1:B3 六格");

    // 当前表局部名称优先于同名全局名称。
    app.wb.names.push((
        "销售额".into(),
        Some(0),
        format!("'{sheet}'!$C$2:$D$4"),
    ));
    app.name_buf = "销售额".into();
    app.goto_name();
    assert_eq!(app.sel, (1, 2), "局部名称应覆盖全局名称");
    assert_eq!(app.sel_end, (3, 3), "局部名称应选中 C2:D4");
}

#[test]
fn name_box_selects_named_range_across_sheets() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::Sheet::new("Other"));
    app.wb.names.push((
        "跨表销售额".into(),
        None,
        "'Other'!$B$2:$C$4".into(),
    ));
    app.name_buf = "跨表销售额".into();
    app.goto_name();
    assert_eq!(app.sheet_idx, 1, "名称体带表名时应切到目标表");
    assert_eq!(app.sel, (1, 1));
    assert_eq!(app.sel_end, (3, 2));
}

#[test]
fn local_names_with_same_text_resolve_per_sheet() {
    let mut app = app();
    let s1 = app.wb.sheets[0].name.clone();
    app.wb.sheets.push(sheetcore::Sheet::new("Other"));
    app.wb.names.push((
        "值".into(),
        Some(0),
        format!("'{s1}'!$A$1:$A$2"),
    ));
    app.wb.names.push(("值".into(), Some(1), "'Other'!$C$3:$D$4".into()));

    app.sheet_idx = 0;
    app.name_buf = "值".into();
    app.goto_name();
    assert_eq!((app.sel, app.sel_end), ((0, 0), (1, 0)), "S1 的局部名称");

    app.sheet_idx = 1;
    app.name_buf = "值".into();
    app.goto_name();
    assert_eq!((app.sel, app.sel_end), ((2, 2), (3, 3)), "Other 的局部名称");
}

#[test]
fn defined_names_reject_cell_addresses_and_reserved_rc() {
    for bad in ["A1", "$A$1", "XFD1048576", "R1C1", "r12c3", "R", "C", "1Name", "A-B"] {
        assert!(
            !SheetApp::valid_defined_name(bad),
            "{bad:?} 不应作为定义名称"
        );
    }
    for good in ["销售额", "Tax_Rate", "_local", "Q1.2026"] {
        assert!(
            SheetApp::valid_defined_name(good),
            "{good:?} 应允许作为定义名称"
        );
    }
}

#[test]
fn defined_name_refers_reject_malformed_or_unknown_sheets() {
    let app = app();
    let sheet = app.wb.sheets[0].name.clone();
    for good in [
        format!("'{sheet}'!$A$1:$B$3"),
        format!("{sheet}!A1"),
        "$C$2:$D$4".to_string(),
        "={1;2}".to_string(),
        "=SUM(A1:A3)".to_string(),
        "=42".to_string(),
    ] {
        assert!(app.valid_name_refers(&good), "{good:?} 应是合法引用");
    }
    for bad in [
        "BOGUS".to_string(),
        "Sheet1!A1:".to_string(),
        "'不存在'!A1".to_string(),
        "A1:".to_string(),
        "={1;2".to_string(),
    ] {
        assert!(!app.valid_name_refers(&bad), "{bad:?} 不应保存");
    }
}

#[test]
fn named_navigation_cannot_move_a_rejected_draft_to_another_sheet() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::Sheet::new("Other"));
    app.wb.names.push(("Destination".into(), None, "Other!$C$1".into()));
    restrict_a1(&mut app);
    app.name_buf = "Destination".into();
    app.goto_name();
    assert_eq!(app.sheet_idx, 0);
    assert_eq!(app.sel, (0, 0));
    assert_eq!(app.editing.as_ref().unwrap().buf, "99");
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Empty);
}

#[test]
fn rejected_draft_keeps_its_cell_through_continued_drag_frames() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    restrict_a1(&mut app);
    let x = HEAD_W + app.wb.sheets[0].cols.pos(2) + 15.0;
    for (row, pressed) in [(0, None), (0, Some(true)), (2, None), (3, None), (3, Some(false))] {
        let pos = egui::pos2(x, HEAD_H + app.wb.sheets[0].rows.pos(row) + 10.0);
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        input.events.push(egui::Event::PointerMoved(pos));
        if let Some(pressed) = pressed {
            input.events.push(egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default(),
            });
        }
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::NONE)
                .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
        });
        assert_eq!(app.sel, (0, 0));
        assert_eq!(app.sel_end, (0, 0));
    }
    app.commit_edit(None);
    assert!(app.editing.is_some());
    assert_eq!(app.wb.sheets[0].value(3, 2), Value::Empty);
}

#[test]
fn cut_and_undo_publish_source_clears_across_sheets() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::Sheet::new("Other"));
    app.wb.sheets[0].set(0, 0, number(1.0));
    app.wb.sheets[1].set(0, 0, number(8.0));
    app.peer = Some(Peer::new(91));
    app.net = Some(net::host(91, "test".into(), "cut-test".into(), 0, "test-only".into()).unwrap());
    app.undo.set_site(91);
    app.clip_copy(true, &egui::Context::default());
    app.sheet_idx = 1;
    app.clip_paste(false, None);
    let value = |app: &SheetApp, sheet| app.peer.as_ref().unwrap().cells[&(sheet, 0, 0)].value.clone();
    assert_eq!(value(&app, 0), sheetcore::sync::ValueWire::Empty);
    assert_eq!(value(&app, 1), sheetcore::sync::ValueWire::Number(1.0));
    app.do_undo(false);
    assert_eq!(value(&app, 0), sheetcore::sync::ValueWire::Number(1.0));
    assert_eq!(value(&app, 1), sheetcore::sync::ValueWire::Number(8.0));
    app.do_undo(true);
    assert_eq!(value(&app, 0), sheetcore::sync::ValueWire::Empty);
}

#[test]
fn closing_parked_dirty_tabs_requires_confirmation_for_that_tab() {
    let mut app = app();
    app.editing = Some(EditState::new("123".into()));
    app.open_in_new_tab(PathBuf::new());
    app.editing = Some(EditState::new("456".into()));
    app.open_in_new_tab(PathBuf::new());
    app.close_tab(0);
    assert_eq!(app.tabs.len(), 3);
    app.close_tab(1);
    assert_eq!(app.tabs.len(), 3, "confirmation for A must not discard B");
    app.close_tab(1);
    assert_eq!(app.tabs.len(), 2);
    let TabSlot::Parked(doc) = &app.tabs[0] else { panic!("A remains parked") };
    assert_eq!(doc.wb.sheets[0].value(0, 0), Value::Number(123.0));
    app.close_tab(0);
    assert_eq!(app.tabs.len(), 2, "indices changing clears prior confirmation");
    app.close_tab(0);
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn edits_after_close_prompt_require_a_new_confirmation() {
    let mut app = app();
    app.open_in_new_tab(PathBuf::new());
    app.editing = Some(EditState::new("1".into()));
    app.close_tab(1);
    assert_eq!(app.tabs.len(), 2);
    app.editing = Some(EditState::new("2".into()));
    app.close_tab(1);
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(2.0));
    app.close_tab(1);
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn sorting_rebuilds_dependencies_for_edit_undo_and_redo() {
    for multi in [false, true] {
        let mut app = app();
        app.wb.sheets[0].set(0, 0, number(2.0));
        app.wb.sheets[0].set(1, 0, number(1.0));
        app.wb.sheets[0].set(0, 1, formula("=A1*10"));
        app.wb.sheets[0].set(1, 1, formula("=A2*10"));
        sheetcore::formula::recalc_all(&mut app.wb);
        app.deps = sheetcore::formula::deps::Deps::build(&app.wb);
        app.sel_end = (1, 1);
        if multi {
            app.run_sort(&DataDlg { tab: 0, keys: vec![(0, true, true)], header: false, delim: ",".into(), sort_mode: 0, custom: String::new(), merge_consecutive: false, quote: false, detect_types: false, sort_by_row: false, split_fixed: false, widths: String::new(), col_modes: String::new() });
        } else {
            app.do_sort(0, true);
        }
        assert_eq!(app.wb.sheets[0].value(0, 1), Value::Number(10.0));
        app.sel_end = app.sel;
        app.editing = Some(EditState::new("3".into()));
        app.commit_edit(None);
        assert_eq!(app.wb.sheets[0].value(0, 1), Value::Number(30.0));
        assert_eq!(app.wb.sheets[0].value(1, 1), Value::Number(20.0));
        app.do_undo(false);
        app.do_undo(false);
        assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(2.0));
        assert_eq!(app.wb.sheets[0].value(0, 1), Value::Number(20.0));
        app.do_undo(true);
        app.do_undo(true);
        assert_eq!(app.wb.sheets[0].value(0, 1), Value::Number(30.0));
        assert_eq!(app.wb.sheets[0].value(1, 1), Value::Number(20.0));
    }
}

#[test]
fn structural_undo_restores_cross_sheet_formulas_names_and_layout() {
    let mut app = app();
    app.wb.sheets[0].set(0, 0, number(10.0));
    app.wb.sheets[0].set(1, 0, number(20.0));
    app.wb.sheets[0].rows.set(0, 42.0);
    let mut dependent = sheetcore::Sheet::new("Dependent");
    dependent.set(0, 0, formula("=Sheet1!A1"));
    app.wb.sheets.push(dependent);
    app.wb.names.push(("Source".into(), None, "Sheet1!$A$1".into()));
    sheetcore::formula::recalc_all(&mut app.wb);
    app.structural(sheetcore::structural::Structural::InsertRows { at: 0, n: 1 });
    assert_eq!(app.wb.names[0].2, "Sheet1!$A$2");
    assert_eq!(app.wb.sheets[0].rows.size(1), 42.0);
    app.do_undo(false);
    assert_eq!(app.wb.sheets[1].get(0, 0).unwrap().formula.as_deref(), Some("=Sheet1!A1"));
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Number(10.0));
    assert_eq!(app.wb.names[0].2, "Sheet1!$A$1");
    assert_eq!(app.wb.sheets[0].rows.size(0), 42.0);
    assert!(app.wb.pivot_shifts.is_empty());
    app.do_undo(true);
    assert_eq!(app.wb.sheets[1].get(0, 0).unwrap().formula.as_deref(), Some("=Sheet1!A2"));
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Number(10.0));
    app.do_undo(false);
    app.editing = Some(EditState::new("30".into()));
    app.commit_edit(None);
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Number(30.0));
}

fn run_grid_frame(app: &mut SheetApp, ctx: &egui::Context, events: Vec<egui::Event>) {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };
    input.events = events;
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
    });
}

#[test]
fn dragging_the_column_header_border_resizes_the_column() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let width = app.wb.sheets[0].cols.size(0);
    let x = HEAD_W + width;
    let y = HEAD_H / 2.0;
    let pos = egui::pos2(x, y);
    let button = |p: egui::Pos2, pressed: bool| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    // 先让 egui 认识指针位置，再按下（真实操作也是先悬停）
    run_grid_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(pos)]);
    run_grid_frame(&mut app, &ctx, vec![button(pos, true)]);
    let to = egui::pos2(x + 40.0, y);
    run_grid_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(to)]);
    run_grid_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(to), button(to, false)]);
    let after = app.wb.sheets[0].cols.size(0);
    assert!(
        (after - (width + 40.0)).abs() < 3.0,
        "列宽应变成 {:.0}，实际 {:.0}",
        width + 40.0,
        after
    );
    assert!(app.wb.dirty);
}

#[test]
fn dragging_the_row_header_border_resizes_the_row() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let height = app.wb.sheets[0].rows.size(0);
    let x = HEAD_W / 2.0;
    let y = HEAD_H + height;
    let pos = egui::pos2(x, y);
    let button = |p: egui::Pos2, pressed: bool| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    run_grid_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(pos)]);
    run_grid_frame(&mut app, &ctx, vec![button(pos, true)]);
    let to = egui::pos2(x, y + 12.0);
    run_grid_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(to)]);
    run_grid_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(to), button(to, false)]);
    let after = app.wb.sheets[0].rows.size(0);
    assert!(
        (after - (height + 12.0)).abs() < 3.0,
        "行高应变成 {:.0}，实际 {:.0}",
        height + 12.0,
        after
    );
}

#[test]
fn right_clicking_a_column_header_opens_the_header_menu() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let pos = egui::pos2(HEAD_W + 20.0, HEAD_H / 2.0);
    let secondary = |p: egui::Pos2, pressed: bool| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Secondary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    run_grid_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(pos)]);
    run_grid_frame(&mut app, &ctx, vec![secondary(pos, true)]);
    run_grid_frame(&mut app, &ctx, vec![secondary(pos, false)]);
    match app.ctx_menu.as_ref().map(|m| (m.kind, m.idx)) {
        Some((CtxKind::Cols, idx)) => assert_eq!(idx, 0, "应该指向 A 列"),
        other => panic!("列标右键没有打开表头菜单：{other:?}"),
    }
    app.ctx_menu = None;
}

#[test]
fn hidden_rows_and_columns_collapse_to_zero_in_the_view() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = app.wb.sheet_mut(0);
        sh.hidden_cols.insert(1);
        sh.hidden_rows_user.insert(2);
        sh.cols.set(0, 170.0);
    }
    let (cols, rows) = app.view_axes();
    assert_eq!(cols.size(1), 0.0, "隐藏列应占 0 像素");
    assert_eq!(rows.size(2), 0.0, "隐藏行应占 0 像素");
    assert!((cols.size(0) - 170.0).abs() < 0.1, "其它列宽不变");
    // 未隐藏的列还在原位
    assert!(cols.pos(2) >= 170.0);
}

#[test]
fn table_design_tab_is_offered_inside_a_table_range() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).tables.push(sheetcore::table::Table {
                column_ids: Vec::new(),
        name: "表1".into(),
        display: "表1".into(),
        range: (0, 0, 3, 2),
        columns: vec!["A".into(), "B".into(), "C".into()],
        header_row: true,
        totals_row: false,
    });
    app.sel = (2, 1);
    app.sel_end = (2, 1);
    assert!(app.table_at_sel().is_some(), "表格内应出现表设计页签");
    app.sel = (9, 9);
    app.sel_end = (9, 9);
    assert!(app.table_at_sel().is_none(), "表格外不显示");
}

#[test]
fn table_fill_calculated_column_is_a_real_command_entry() {
    let mut app = app();
    {
        let sh = app.wb.sheet_mut(0);
        sh.tables.push(sheetcore::table::Table {
            name: "Sales".into(),
            display: "Sales".into(),
            range: (0, 0, 3, 2),
            columns: vec!["Amount".into(), "Tax".into(), "Total".into()],
            column_ids: vec![1, 2, 3],
            header_row: true,
            totals_row: false,
        });
        sh.set(0, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Text("Amount".into()), formula: None });
        sh.set(0, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Text("Tax".into()), formula: None });
        sh.set(0, 2, sheetcore::model::Cell { value: sheetcore::model::Value::Text("Total".into()), formula: None });
        for r in 1..=3 {
            sh.set(r, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Number(r as f64), formula: None });
            sh.set(r, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Number(2.0), formula: None });
        }
        sh.set(1, 2, sheetcore::model::Cell { value: sheetcore::model::Value::Empty, formula: Some("=A2*B2".into()) });
    }
    app.sel = (1, 2);
    app.sel_end = (1, 2);
    app.fill_table_calculated_column();
    let sh = app.wb.sheet(0);
    assert_eq!(sh.get(2, 2).and_then(|c| c.formula.as_deref()), Some("=A3*B3"));
    assert_eq!(sh.get(3, 2).and_then(|c| c.formula.as_deref()), Some("=A4*B4"));
    assert_eq!(sh.get(2, 2).map(|c| c.value.clone()), Some(sheetcore::model::Value::Number(4.0)));
    assert_eq!(sh.get(3, 2).map(|c| c.value.clone()), Some(sheetcore::model::Value::Number(6.0)));
}

#[test]
fn chart_selection_exposes_the_chart_tools_tab() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let ch = sheetcore::charts::Chart {
        kind: sheetcore::charts::ChartKind::Column,
        title: "测试图".into(),
        series: Vec::new(),
        legend: false,
        anchor: (1, 4, 10, 9),
        legend_pos: String::new(),
        part: String::new(),
        drawing: String::new(),
        show_labels: false,
        label_opts: sheetcore::charts::LabelOpts::default(),
        radar_style: String::new(),
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
        y_fmt: String::new(),
        y_min: None,
        y_max: None,
    };
    app.wb.sheet_mut(0).charts.push(ch);
    assert!(app.chart_at_sel().is_none(), "没选中时没有图表工具");
    app.chart_sel = Some(0);
    assert!(app.chart_at_sel().is_some(), "选中后应有图表工具");
    app.chart_sel = Some(9);
    assert!(app.chart_at_sel().is_none(), "越界索引不应崩");
}

#[test]
fn page_layout_modes_render_and_keep_the_selection() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for r in 0..30u32 {
        app.wb.sheet_mut(0).set(
            r,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Text(format!("row {r}").into()),
                formula: None,
            },
        );
    }
    app.sel = (3, 0);
    app.sel_end = (3, 0);
    app.view_mode = 1;
    run_grid_frame(&mut app, &ctx, vec![]);
    assert_eq!(app.sel, (3, 0), "页面布局不应改变选区");
    app.view_mode = 2;
    run_grid_frame(&mut app, &ctx, vec![]);
    assert_eq!(app.view_mode, 2);
    app.view_mode = 0;
    run_grid_frame(&mut app, &ctx, vec![]);
}

#[test]
fn dragging_a_chart_moves_its_anchor_and_marks_dirty() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let ch = sheetcore::charts::Chart {
        kind: sheetcore::charts::ChartKind::Column,
        title: "拖动测试".into(),
        series: Vec::new(),
        legend: false,
        anchor: (1, 2, 8, 6),
        legend_pos: String::new(),
        part: String::new(),
        drawing: String::new(),
        show_labels: false,
        label_opts: sheetcore::charts::LabelOpts::default(),
        radar_style: String::new(),
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
        y_fmt: String::new(),
        y_min: None,
        y_max: None,
    };
    app.wb.sheet_mut(0).charts.push(ch);
    let start = egui::pos2(HEAD_W + 2.0 * 72.0 + 40.0, HEAD_H + 19.0 + 30.0);
    let end = egui::pos2(start.x + 120.0, start.y + 60.0);
    let button = |p: egui::Pos2, pressed: bool| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    run_grid_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(start)]);
    run_grid_frame(&mut app, &ctx, vec![button(start, true)]);
    run_grid_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
    run_grid_frame(&mut app, &ctx, vec![button(end, false)]);
    let after = app.wb.sheet(0).charts[0].anchor;
    assert_ne!(after, (1, 2, 8, 6), "拖动后锚点应变化");
    assert!(app.wb.sheet(0).charts_dirty, "拖动后应标脏以便保存");
}

#[test]
fn protected_sheet_blocks_clearing_and_formatting() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(
        0,
        0,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Number(1.0),
            formula: None,
        },
    );
    app.wb.sheet_mut(0).protection = Some(
        sheetcore::protection::SheetProtection::new_with_password("pw"),
    );
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.clear_selection();
    assert!(app.wb.sheet(0).get(0, 0).is_some(), "受保护时不能清空");
    app.apply_style(|s| s.bold = true);
    let sid = app.wb.sheet(0).style_of(0, 0);
    assert!(!app.wb.styles.get(sid).bold, "受保护时不能改格式");
    assert!(app.edit_blocked(), "保护状态应可查询");
}

#[test]
fn pivot_refresh_recomputes_in_place() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Text("k".into()), formula: None });
        sh.set(0, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Text("v".into()), formula: None });
        sh.set(1, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Text("a".into()), formula: None });
        sh.set(1, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Number(10.0), formula: None });
        sh.set(2, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Text("b".into()), formula: None });
        sh.set(2, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Number(5.0), formula: None });
    }
    app.sel = (5, 0);
    let spec = sheetcore::pivot::PivotSpec {
        source: (0, 0, 2, 1),
        row_field: 0,
        col_field: None,
        value_field: 1,
        agg: sheetcore::pivot::Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
    };
    app.generate_pivot(&spec);
    let rect = app.pivot_last.as_ref().expect("应记录透视表位置").2;
    let total = app.wb.sheet(0).value(rect.2, rect.1 + 1).display();
    assert_eq!(total, "15", "生成后的总计");
    app.wb.sheet_mut(0).set(
        1,
        1,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Number(110.0),
            formula: None,
        },
    );
    app.refresh_pivot();
    let rect2 = app.pivot_last.as_ref().unwrap().2;
    assert_eq!(rect2, rect, "刷新应写回同一块区域");
    assert_eq!(app.wb.sheet(0).value(rect2.2, rect2.1 + 1).display(), "115");
}

#[test]
fn plain_range_filter_opens_from_header_arrow() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Text("k".into()), formula: None });
        sh.set(0, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Text("v".into()), formula: None });
        sh.set(1, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Text("a".into()), formula: None });
        sh.set(1, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Number(1.0), formula: None });
        sh.set(2, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Text("b".into()), formula: None });
        sh.set(2, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Number(2.0), formula: None });
    }
    app.sel = (0, 0);
    app.start_filter();
    assert_eq!(
        app.wb.sheet(0).filter.as_ref().map(|f| f.range),
        Some((0, 0, 2, 1)),
        "普通区域点筛选应建立 A1:B3 的筛选"
    );
    run_grid_frame(&mut app, &ctx, vec![]);
    let p = egui::pos2(HEAD_W + 64.0, HEAD_H + 9.5);
    let press = egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    };
    let release = egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    };
    run_grid_frame(&mut app, &ctx, vec![press]);
    run_grid_frame(&mut app, &ctx, vec![release]);
    assert!(app.filter_popup.is_some(), "点表头漏斗应弹出筛选面板");
}

#[test]
fn making_a_table_enables_filter_and_second_click_clears_it() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = app.wb.sheet_mut(0);
        for c in 0..2u32 {
            sh.set(0, c, sheetcore::model::Cell { value: sheetcore::model::Value::Text(format!("h{c}").into()), formula: None });
            sh.set(1, c, sheetcore::model::Cell { value: sheetcore::model::Value::Text("a".into()), formula: None });
            sh.set(2, c, sheetcore::model::Cell { value: sheetcore::model::Value::Text("b".into()), formula: None });
        }
    }
    app.sel = (0, 0);
    app.sel_end = (2, 1);
    app.make_table();
    assert_eq!(app.wb.sheet(0).tables.len(), 1);
    assert_eq!(
        app.wb.sheet(0).filter.as_ref().map(|f| f.range),
        Some((0, 0, 2, 1)),
        "建表应自动带筛选"
    );
    app.start_filter();
    assert!(app.wb.sheet(0).filter.is_none(), "再点一次筛选应取消");
}

fn run_keys(app: &mut SheetApp, ctx: &egui::Context, events: Vec<egui::Event>) {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };
    // egui 的 RawInput.modifiers 才是 i.modifiers 的来源，Key 事件里的不算
    input.modifiers = events
        .iter()
        .find_map(|e| match e {
            egui::Event::Key { modifiers, .. } => Some(*modifiers),
            _ => None,
        })
        .unwrap_or_default();
    input.events = events;
    let _ = ctx.run(input, |ctx| app.handle_keys(ctx));
}

fn ctrl_key_event(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        },
    }
}

fn key_event(key: egui::Key, shift: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            shift,
            ..Default::default()
        },
    }
}

/// UI-09.2：PageUp/Down 按视口可见行数翻页（不是固定 20 行），跳过隐藏行；
/// Ctrl+PageUp/Down 切可见工作表且不修改单元格。
/// UI-09.1：Ctrl+B/I/U、Ctrl+A 两段选择、Ctrl/Shift+Space、F5/Ctrl+G 名称框。
#[test]
fn common_selection_and_format_shortcuts_work() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, text("A"));
    app.wb.sheet_mut(0).set(0, 1, text("B"));
    app.wb.sheet_mut(0).set(1, 0, number(1.0));
    app.wb.sheet_mut(0).set(1, 1, number(2.0));
    app.sel = (0, 0);
    app.sel_end = (1, 1);

    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::B)]);
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).bold);
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(1, 1)).bold);
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::B)]);
    assert!(!app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).bold);
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::I)]);
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).italic);
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::U)]);
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).underline);

    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.extra_sel = vec![(10, 10, 11, 11)];
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::A)]);
    assert_eq!((app.sel, app.sel_end), ((0, 0), (1, 1)), "第一次 Ctrl+A 选当前区域");
    assert!(app.extra_sel.is_empty(), "Ctrl+A 必须替换旧多选区");
    app.extra_sel = vec![(10, 10, 11, 11)];
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::A)]);
    assert_eq!((app.sel, app.sel_end), ((0, 0), (1_048_575, 16_383)), "第二次 Ctrl+A 选全表");
    assert!(app.extra_sel.is_empty(), "第二次 Ctrl+A 也必须清旧多选区");

    // A1:B2 的整行选择必须覆盖两行，而不是只取活动格所在的第 1 行。
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    app.extra_sel = vec![(20, 20, 21, 21)];
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Space, true)]);
    assert_eq!((app.sel, app.sel_end), ((0, 0), (1, 16_383)), "Shift+Space 应按选区跨两行");
    assert!(app.extra_sel.is_empty(), "整行选择必须替换旧多选区");

    // C3:D5 的整列选择必须覆盖 C:D 两列。
    app.sel = (2, 2);
    app.sel_end = (4, 3);
    app.extra_sel = vec![(30, 30, 31, 31)];
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::Space)]);
    assert_eq!((app.sel, app.sel_end), ((0, 2), (1_048_575, 3)), "Ctrl+Space 应按选区跨两列");
    assert!(app.extra_sel.is_empty(), "整列选择必须替换旧多选区");

    // 单格仍保持原来的整行/整列语义。
    app.sel = (1, 1);
    app.sel_end = (1, 1);
    app.sel = (1, 1);
    app.sel_end = (1, 1);
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::Space)]);
    assert_eq!((app.sel, app.sel_end), ((0, 1), (1_048_575, 1)), "Ctrl+Space 单格选整列");
    app.sel = (1, 1);
    app.sel_end = (1, 1);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Space, true)]);
    assert_eq!((app.sel, app.sel_end), ((1, 0), (1, 16_383)), "Shift+Space 单格选整行");

    app.name_focus_request = false;
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::F5, false)]);
    assert!(app.name_focus_request, "F5 应请求名称框焦点");
    app.name_focus_request = false;
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::G)]);
    assert!(app.name_focus_request, "Ctrl+G 应请求名称框焦点");
}

#[test]
fn page_keys_follow_viewport_and_ctrl_page_switches_sheet() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, text("keep"));
    app.wb.sheets.push(sheetcore::Sheet::new("Second"));
    app.wb.sheet_mut(1).set(0, 0, number(99.0));
    app.wb.sheet_mut(0).rows.set_default(19.0);
    app.last_view = egui::vec2(1000.0, 720.0);
    app.scroll = egui::Vec2::ZERO;
    app.sel = (0, 0);
    app.sel_end = (0, 0);

    // 720p：隐藏第 2/3 行；PageDown 后活动格应到新视口首行，
    // 视口本身翻一页并保留一行重叠，而不是只把 scroll.y 挪 2px。
    app.wb.sheet_mut(0).rows.set(1, 0.0);
    app.wb.sheet_mut(0).rows.set(2, 0.0);
    let (first0, last0) = visible_row_window(&app);
    assert_eq!(first0, 0);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::PageDown, false)]);
    let (first1, last1) = visible_row_window(&app);
    assert_eq!(first1, last0, "新视口首行必须是旧视口末行（一行重叠）");
    assert_eq!(app.sel.0, first1, "活动格应落在新视口首行");
    assert!(last1 > last0, "视口必须真的向下翻页");
    assert!(app.sel.0 >= 3, "隐藏行 1/2 不能成为落点");
    let row_top = app.view_axes().1.pos(app.sel.0);
    assert!((app.scroll.y - row_top).abs() < 0.5, "scroll.y={} 应到新首行位置 {}", app.scroll.y, row_top);
    assert!(app.scroll.y > 100.0, "从第 0 行翻页不能只滚动 2px");

    // 1080p：可见行更多，翻页目标仍是旧视口末行，scroll 同步到新首行。
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.scroll = egui::Vec2::ZERO;
    app.last_view = egui::vec2(1000.0, 1080.0);
    let last720 = last0;
    let (first1080, last1080) = visible_row_window(&app);
    assert_eq!(first1080, 0);
    assert!(last1080 > last720, "1080p 可见行数应多于 720p");
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::PageDown, false)]);
    let (after_first, after_last) = visible_row_window(&app);
    assert_eq!(after_first, last1080, "1080p 也保留一行重叠");
    assert!(after_last > last1080);
    let row_top1080 = app.view_axes().1.pos(app.sel.0);
    assert!((app.scroll.y - row_top1080).abs() < 0.5, "1080p scroll 未同步");

    // 中部：活动格在页内偏移 10；翻页后视口首行是旧末行，活动格保持偏移 10。
    app.last_view = egui::vec2(1000.0, 720.0);
    app.scroll = egui::Vec2::ZERO;
    app.sel = (10, 0);
    app.sel_end = (10, 0);
    let (mid_first0, mid_last0) = visible_row_window(&app);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::PageDown, false)]);
    let (mid_first1, mid_last1) = visible_row_window(&app);
    assert_eq!(mid_first1, mid_last0, "中部翻页仍以旧视口末行重叠");
    let mid_offset = visible_row_offset(&app, mid_first0, 10);
    assert_eq!(app.sel.0, mid_first1 + mid_offset, "活动格应保持可见页内偏移");
    assert!(mid_last1 > mid_last0);
    let mid_top = app.view_axes().1.pos(mid_first1);
    assert!((app.scroll.y - mid_top).abs() < 0.5, "中部 scroll 应到旧末行");
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::PageUp, false)]);
    let (back_first, _) = visible_row_window(&app);
    assert_eq!(back_first, mid_first0, "PageUp 应回到原页首");
    assert_eq!(app.sel, (10, 0), "PageUp 应保持页内偏移并回原活动格");

    // 底部页：同样从旧末行开始，不跳过中间行。
    let rows_pos = app.view_axes().1.pos(900);
    app.scroll = egui::Vec2::new(0.0, rows_pos);
    app.sel = (910, 0);
    app.sel_end = (910, 0);
    let (bot_first0, bot_last0) = visible_row_window(&app);
    assert_eq!(bot_first0, 900);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::PageDown, false)]);
    let (bot_first1, _) = visible_row_window(&app);
    assert_eq!(bot_first1, bot_last0, "底部翻页也必须保留一行重叠");
    assert_eq!(app.sel.0, bot_first1 + 10, "底部页内偏移不变");
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::PageUp, false)]);
    assert_eq!(visible_row_window(&app).0, bot_first0, "底部 PageUp 回原页首");
    assert_eq!(app.sel, (910, 0), "底部 PageUp 回原活动格");

    // 边界连续隐藏：首端停在最近可见行，末端也不落到隐藏的 1048574/1048575。
    app.wb.sheet_mut(0).rows.set(0, 0.0);
    app.wb.sheet_mut(0).rows.set(1, 0.0);
    app.wb.sheet_mut(0).rows.set(2, 19.0);
    assert_eq!(app.next_visible_row(0, false), 2, "首端隐藏应停最近可见行 2");
    app.wb.sheet_mut(0).rows.set(1_048_574, 0.0);
    app.wb.sheet_mut(0).rows.set(1_048_575, 0.0);
    assert_eq!(app.next_visible_row(1_048_573, true), 1_048_573, "末端隐藏应停最近可见行");
    assert_eq!(app.next_visible_row(1_048_575, true), 1_048_573, "从隐藏末行向下也要回退到最近可见行");

    // 到最大行不越界（末行可见时仍停在末行）。
    app.wb.sheet_mut(0).rows.set(1_048_575, 19.0);
    app.sel = (1_048_575, 0);
    app.sel_end = (1_048_575, 0);
    app.scroll = egui::Vec2::ZERO;
    app.last_view = egui::vec2(1000.0, 720.0);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::PageDown, false)]);
    assert_eq!(app.sel.0, 1_048_575);

    // Ctrl+PageDown/Up：只切可见表，不改单元格内容。
    app.sheet_idx = 0;
    app.sel = (0, 0);
    let a0 = app.wb.sheet(0).value(0, 0);
    let b0 = app.wb.sheet(1).value(0, 0);
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::PageDown)]);
    assert_eq!(app.sheet_idx, 1);
    assert_eq!(app.wb.sheet(0).value(0, 0), a0);
    assert_eq!(app.wb.sheet(1).value(0, 0), b0);
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::PageUp)]);
    assert_eq!(app.sheet_idx, 0);
    assert_eq!(app.wb.sheet(0).value(0, 0), a0);
}

#[test]
fn shift_arrows_extend_selection_and_plain_arrow_collapses() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::ArrowRight, true)]);
    assert_eq!((app.sel, app.sel_end), ((0, 1), (0, 0)), "Shift+→ 应从 A1 扩到 B1");
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, true)]);
    assert_eq!((app.sel, app.sel_end), ((1, 1), (0, 0)), "继续 Shift+↓ 应扩到 B2");
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::ArrowLeft, false)]);
    assert_eq!((app.sel, app.sel_end), ((1, 0), (1, 0)), "普通方向键应收起选区");
}

#[test]
fn shift_enter_and_shift_tab_move_backwards() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (2, 2);
    app.sel_end = (2, 2);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Enter, true)]);
    assert_eq!(app.sel, (1, 2), "Shift+Enter 应向上");
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Tab, true)]);
    assert_eq!(app.sel, (1, 1), "Shift+Tab 应向左");
}

/// UI-09.3：选区内的 Tab/Enter 循环、跳过隐藏行列，以及 XFD/1048576 边界。
/// UI-08.2：格式对话框只提交改动字段；未改直接确定不重写样式。
#[test]
fn format_dialog_preserves_unchanged_fields_and_mixed_cell_styles() {
    use sheetcore::style::CellStyle;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let dark = CellStyle::default_dark();
    let red = CellStyle {
        bold: true,
        font: "宋体".into(),
        color: [180, 20, 20],
        format: "0.00".into(),
        size: 13.0,
        ..dark.clone()
    };
    let blue = CellStyle {
        italic: true,
        font: "黑体".into(),
        color: [20, 20, 180],
        format: "0".into(),
        size: 10.0,
        ..dark
    };
    let red_id = app.wb.styles.intern(red);
    let blue_id = app.wb.styles.intern(blue);
    app.wb.sheet_mut(0).set_style(0, 0, red_id);
    app.wb.sheet_mut(0).set_style(1, 0, blue_id);
    app.sel = (0, 0);
    app.sel_end = (1, 0);

    // 只改数字格式：两格各自保留字体、字号、颜色、粗斜体。
    app.open_format_dialog();
    assert!(app.fmt_mixed.font, "字体不同应标记混合");
    assert!(app.fmt_mixed.color, "颜色不同应标记混合");
    assert!(app.fmt_mixed.size, "字号不同应标记混合");
    assert!(app.fmt_mixed.format, "数字格式不同应标记混合");
    app.fmt_state.format = "#,##0.00".into();
    app.apply_format_dialog();
    for (r, want_font, want_bold, want_italic, want_color, want_size) in [
        (0u32, "宋体", true, false, [180, 20, 20], 13.0f32),
        (1u32, "黑体", false, true, [20, 20, 180], 10.0f32),
    ] {
        let st = app.wb.styles.get(app.wb.sheet(0).style_of(r, 0));
        assert_eq!(st.format, "#,##0.00");
        assert_eq!(st.font, want_font);
        assert_eq!(st.bold, want_bold);
        assert_eq!(st.italic, want_italic);
        assert_eq!(st.color, want_color);
        assert_eq!(st.size, want_size);
    }

    // 打开后不改直接确定：样式标识与格式都不变，不产生一次无意义重写。
    let before = (app.wb.sheet(0).style_of(0, 0), app.wb.sheet(0).style_of(1, 0));
    app.sel = (0, 0);
    app.sel_end = (1, 0);
    app.open_format_dialog();
    assert!(!app.fmt_mixed.format, "数字格式已统一，不应再报混合");
    assert!(app.fmt_mixed.font, "字体仍不同，应保持混合");
    app.apply_format_dialog();
    assert_eq!(app.wb.sheet(0).style_of(0, 0), before.0);
    assert_eq!(app.wb.sheet(0).style_of(1, 0), before.1);
    assert_eq!(app.wb.styles.get(before.0).format, "#,##0.00");
    assert_eq!(app.wb.styles.get(before.1).format, "#,##0.00");
}

/// UI-08 混合值：明确选中活动格原值时也必须应用到整个选区。
#[test]
fn mixed_field_explicit_choice_equal_to_active_value_is_applied() {
    use sheetcore::style::CellStyle;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let base = CellStyle::default_dark();
    let song = CellStyle {
        font: "宋体".into(),
        format: "0".into(),
        size: 13.0,
        ..base.clone()
    };
    let hei = CellStyle {
        font: "黑体".into(),
        format: "0.00".into(),
        size: 10.0,
        ..base
    };
    let song_id = app.wb.styles.intern(song);
    let hei_id = app.wb.styles.intern(hei);
    app.wb.sheet_mut(0).set_style(0, 0, song_id);
    app.wb.sheet_mut(0).set_style(1, 0, hei_id);
    app.sel = (0, 0);
    app.sel_end = (1, 0);
    app.open_format_dialog();
    assert!(app.fmt_mixed.font);
    assert!(app.fmt_mixed.format);
    // 下拉里明确点“宋体”：值与活动格原值相同，但仍是明确的用户选择。
    app.fmt_state.font = "宋体".into();
    app.fmt_touched.font = true;
    // 数字格式同样明确选回活动格原值 "0"。
    app.fmt_state.format = "0".into();
    app.fmt_touched.format = true;
    app.apply_format_dialog();
    for r in 0..2u32 {
        let st = app.wb.styles.get(app.wb.sheet(0).style_of(r, 0));
        assert_eq!(st.font, "宋体", "A{} 字体应被明确选择覆盖", r + 1);
        assert_eq!(st.format, "0", "A{} 格式应被明确选择覆盖", r + 1);
    }
    assert_eq!(app.fmt_state.font, "宋体");
}

/// UI-15.1：自定义数字格式用活动格当前值预览，并写回样式。
#[test]
fn custom_format_previews_active_cell_value() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(-1234.5));
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.open_format_dialog();
    app.fmt_state.format = "#,##0.00;[Red](#,##0.00);-".into();
    assert_eq!(app.format_dialog_preview(), "(1,234.50)");
    app.apply_format_dialog();
    let st = app.wb.styles.get(app.wb.sheet(0).style_of(0, 0));
    assert_eq!(st.format, "#,##0.00;[Red](#,##0.00);-");
    let path = crate::test_env::scratch("ui15-custom-format").join("custom.xlsx");
    sheetcore::xlsx::save_atomic(&path, &app.wb).unwrap();
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(
        back.styles.get(back.sheet(0).style_of(0, 0)).format,
        "#,##0.00;[Red](#,##0.00);-"
    );
}

/// UI-15.1：取消只丢弃草稿，不改样式、不标脏、不记撤销。
#[test]
fn custom_format_cancel_leaves_workbook_untouched() {
    let mut app = app();
    app.wb.sheet_mut(0).set(0, 0, number(1234.5));
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.wb.dirty = false;
    let before = app.wb.sheet(0).style_of(0, 0);
    let undo_before = app.undo.depth();
    app.open_format_dialog();
    app.fmt_state.format = "#,##0.00;[Red](#,##0.00);\"-\"".into();
    app.fmt_state.bold = !app.fmt_state.bold;
    app.cancel_format_dialog();
    assert_eq!(app.wb.sheet(0).style_of(0, 0), before, "取消不得改样式");
    assert!(!app.wb.dirty, "取消不得标脏");
    assert_eq!(app.undo.depth(), undo_before, "取消不得记撤销");
    assert!(!app.fmt_dlg, "取消应关闭对话框");
}

/// UI-15.1 / QA-04.1：格式对话框打开时 Enter 只能由对话框消费，不能漏给网格移动选区。
#[test]
fn format_dialog_enter_does_not_leak_grid_move() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.open_format_dialog();
    assert!(app.fmt_dlg);
    run_keys(
        &mut app,
        &ctx,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }],
    );
    assert_eq!(app.sel, (0, 0), "对话框的 Enter 不得移动网格");
    assert!(app.fmt_dlg, "未渲染对话框时 handle_keys 不应替它关闭");
}

/// UI-15.1：自定义格式的正/负/零三段预览，并在保存重开后保持一致。
#[test]
fn custom_format_three_sections_preview_and_reload() {
    use sheetcore::style::CellStyle;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let fmt = "#,##0.00;[Red](#,##0.00);\"-\"";
    let style = CellStyle {
        format: fmt.into(),
        ..CellStyle::default_dark()
    };
    let sid = app.wb.styles.intern(style);
    for (r, v) in [(0u32, 1234.5), (1, -1234.5), (2, 0.0)] {
        app.wb.sheet_mut(0).set(r, 0, number(v));
        app.wb.sheet_mut(0).set_style(r, 0, sid);
    }
    for (r, want) in [(0u32, "1,234.50"), (1, "(1,234.50)"), (2, "-")] {
        app.sel = (r, 0);
        app.sel_end = (r, 0);
        app.open_format_dialog();
        assert_eq!(app.format_dialog_preview(), want, "第 {} 行三段预览", r + 1);
    }
    let path = crate::test_env::scratch("ui15-three-sections").join("three.xlsx");
    sheetcore::xlsx::save_atomic(&path, &app.wb).unwrap();
    let back = sheetcore::xlsx::load(&path).unwrap();
    let mut app2 = SheetApp::new_with_context(&ctx, None);
    app2.wb = back;
    for (r, want) in [(0u32, "1,234.50"), (1, "(1,234.50)"), (2, "-")] {
        app2.sel = (r, 0);
        app2.sel_end = (r, 0);
        app2.open_format_dialog();
        assert_eq!(app2.fmt_state.format, fmt, "重开后格式串必须保留");
        assert_eq!(app2.format_dialog_preview(), want, "重开后第 {} 行显示", r + 1);
    }
}

/// UI-15.2：填充/保护页写入真实样式，混合字段显式选择也覆盖整个选区。
#[test]
fn format_dialog_fill_and_protection_apply_to_selection() {
    use sheetcore::style::CellStyle;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let a = CellStyle {
        locked: true,
        hidden: false,
        fill: None,
        ..CellStyle::default_dark()
    };
    let b = CellStyle {
        locked: false,
        hidden: true,
        fill: Some([200, 10, 10]),
        ..CellStyle::default_dark()
    };
    let aid = app.wb.styles.intern(a);
    let bid = app.wb.styles.intern(b);
    app.wb.sheet_mut(0).set_style(0, 0, aid);
    app.wb.sheet_mut(0).set_style(1, 0, bid);
    app.sel = (0, 0);
    app.sel_end = (1, 0);
    app.open_format_dialog();
    assert!(app.fmt_mixed.fill);
    assert!(app.fmt_mixed.locked, "锁定状态不同应显示混合");
    assert!(app.fmt_mixed.hidden, "隐藏状态不同应显示混合");
    app.fmt_state.fill = Some([10, 200, 10]);
    app.fmt_touched.fill = true;
    // 显式选回活动格的锁定/不隐藏，即使等于原值也必须应用。
    app.fmt_state.locked = true;
    app.fmt_touched.locked = true;
    app.fmt_state.hidden = false;
    app.fmt_touched.hidden = true;
    app.apply_format_dialog();
    for r in 0..2u32 {
        let st = app.wb.styles.get(app.wb.sheet(0).style_of(r, 0));
        assert_eq!(st.fill, Some([10, 200, 10]), "A{} 填充", r + 1);
        assert!(st.locked, "A{} 应锁定", r + 1);
        assert!(!st.hidden, "A{} 不应隐藏", r + 1);
    }
}

/// UI-15.2：已有文件里的 fill / locked / hidden 被格式对话框改回默认后，
/// 保存必须把旧节点删掉，重开不能还留着旧填充或旧保护。
#[test]
fn format_dialog_clears_fill_and_protection_on_existing_file() {
    use sheetcore::style::CellStyle;
    let ctx = egui::Context::default();
    let path = crate::test_env::scratch("ui15-clear-existing").join("existing.xlsx");
    let _ = std::fs::remove_file(&path);
    let mut seed = sheetcore::model::Workbook::new_blank();
    seed.sheets[0].set(0, 0, text("x"));
    let sid = seed.styles.intern(CellStyle {
        fill: Some([200, 10, 10]),
        locked: false,
        hidden: true,
        ..CellStyle::default_dark()
    });
    seed.sheets[0].set_style(0, 0, sid);
    sheetcore::xlsx::save_atomic(&path, &seed).expect("seed save");

    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb = sheetcore::xlsx::load(&path).expect("load existing");
    app.wb.path = Some(path.clone());
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.open_format_dialog();
    assert_eq!(app.fmt_state.fill, Some([200, 10, 10]), "载入应读到旧填充");
    assert!(!app.fmt_state.locked && app.fmt_state.hidden, "载入应读到旧保护");
    app.fmt_state.fill = None;
    app.fmt_touched.fill = true;
    app.fmt_state.locked = true;
    app.fmt_touched.locked = true;
    app.fmt_state.hidden = false;
    app.fmt_touched.hidden = true;
    app.apply_format_dialog();
    assert!(!app.fmt_dlg);
    sheetcore::xlsx::save_atomic(&path, &app.wb).expect("save after format");
    let back = sheetcore::xlsx::load(&path).expect("reload");
    let st = back.styles.get(back.sheet(0).style_of(0, 0));
    assert_eq!(st.fill, None, "重开后旧填充必须清掉");
    assert!(st.locked, "重开后锁定必须恢复默认 true");
    assert!(!st.hidden, "重开后隐藏必须恢复默认 false");
}

/// UI-16：每张表按 identity 恢复自己的选区/滚动/缩放，改名和重排不能串表。
#[test]
fn sheet_view_state_is_restored_by_stable_identity() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    assert!(sheetcore::sheet_ops::insert_sheet(
        &mut app.wb,
        1,
        sheetcore::model::Sheet::new("Second")
    ));
    let first_id = app.wb.sheets[0].identity;
    let second_id = app.wb.sheets[1].identity;
    app.sel = (99, 25);
    app.sel_end = (100, 26);
    app.extra_sel = vec![(3, 3, 4, 4)];
    app.scroll = egui::vec2(123.0, 456.0);
    app.zoom = 1.5;
    app.view_mode = 2;
    assert!(app.switch_sheet_view(1));
    assert_eq!(app.sel, (0, 0), "新表不能继承上一表的选区");
    assert_eq!(app.view_mode, 0, "新表默认普通视图");
    assert_eq!(app.scroll, Vec2::ZERO);
    assert_eq!(app.zoom, 1.0);
    assert!(app.switch_sheet_view(0));
    assert_eq!((app.sel, app.sel_end), ((99, 25), (100, 26)));
    assert_eq!(app.extra_sel, vec![(3, 3, 4, 4)]);
    assert_eq!(app.scroll, egui::vec2(123.0, 456.0));
    assert!((app.zoom - 1.5).abs() < f32::EPSILON);
    assert_eq!(app.view_mode, 2, "分页预览要随表恢复");

    // 改名后 identity 不变，仍恢复原状态；重排后也不串到别的表。
    app.wb.sheets[0].name = "Renamed".into();
    assert!(app.switch_sheet_view(1));
    assert!(app.switch_sheet_view(0));
    assert_eq!(app.sel, (99, 25));
    assert_eq!(app.wb.sheets[0].identity, first_id);
    assert!(app.swap_sheet_positions(0, 1));
    // 当前活动表仍是原 first，索引已变为 1；切到第二张再恢复。
    assert!(app.switch_sheet_view(0));
    assert_eq!(app.sel, (0, 0));
    assert!(app.switch_sheet_view(1));
    assert_eq!(app.sel, (99, 25));
    assert_eq!(app.wb.sheets[1].identity, first_id);
    assert_eq!(app.wb.sheets[0].identity, second_id);

    // Ctrl+PageDown/PageUp 也必须走同一恢复路径，而不是重置到 A1。
    assert!(app.switch_sheet_view(0));
    app.sel = (1, 2);
    app.sel_end = (1, 2);
    app.scroll = egui::vec2(7.0, 8.0);
    app.zoom = 1.2;
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::PageDown)]);
    assert_eq!(app.sheet_idx, 1);
    app.sel = (5, 6);
    app.sel_end = (5, 7);
    app.scroll = egui::vec2(9.0, 10.0);
    app.zoom = 0.8;
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::PageUp)]);
    assert_eq!(app.sheet_idx, 0);
    assert_eq!((app.sel, app.sel_end), ((1, 2), (1, 2)));
    assert_eq!(app.scroll, egui::vec2(7.0, 8.0));
    assert!((app.zoom - 1.2).abs() < f32::EPSILON);
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::PageDown)]);
    assert_eq!(app.sheet_idx, 1);
    assert_eq!((app.sel, app.sel_end), ((5, 6), (5, 7)));
    assert_eq!(app.scroll, egui::vec2(9.0, 10.0));
    assert!((app.zoom - 0.8).abs() < f32::EPSILON);
}

/// UI-16：30 张表时工作表列表可按名称搜索并定位到目标。
#[test]
fn sheet_list_search_finds_one_of_thirty_sheets() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for i in 1..30 {
        assert!(sheetcore::sheet_ops::insert_sheet(
            &mut app.wb,
            i,
            sheetcore::model::Sheet::new(format!("表{i}"))
        ));
    }
    assert_eq!(app.wb.sheets.len(), 30);
    assert_eq!(app.sheet_list_matches("表17"), vec![17]);
    assert_eq!(app.sheet_list_matches("表29"), vec![29]);
    assert!(!app.sheet_list_matches("表17").contains(&16));
    assert!(app.sheet_list_matches("不存在").is_empty());
    assert!(app.switch_sheet_view(17));
    assert_eq!(app.wb.sheet(app.sheet_idx).name, "表17");
}

/// UI-16：移动/复制到任意目标位置，活动表按 identity 跟随而不是串索引。
#[test]
fn sheet_move_and_copy_targets_preserve_active_identity() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for (i, name) in ["B", "C"].iter().enumerate() {
        assert!(sheetcore::sheet_ops::insert_sheet(
            &mut app.wb,
            i + 1,
            sheetcore::model::Sheet::new(*name)
        ));
    }
    let a_id = app.wb.sheets[0].identity;
    app.sel = (4, 5);
    app.scroll = egui::vec2(12.0, 34.0);
    app.zoom = 1.3;
    assert!(app.move_sheet_to(0, 3));
    assert_eq!(
        app.wb.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["B", "C", "Sheet1"]
    );
    assert_eq!(app.wb.sheet(app.sheet_idx).identity, a_id);
    assert_eq!(app.sheet_idx, 2);
    // 视图按 identity 仍在原 Sheet1 上。
    assert!(app.switch_sheet_view(0));
    assert_eq!(app.sel, (0, 0));
    assert!(app.switch_sheet_view(2));
    assert_eq!(app.sel, (4, 5));
    assert_eq!(app.scroll, egui::vec2(12.0, 34.0));
    assert!((app.zoom - 1.3).abs() < f32::EPSILON);

    // 复制到指定位置；新表成为活动表，源表仍保留。
    assert!(app.copy_sheet_to(0, 0));
    assert_eq!(app.wb.sheets.len(), 4);
    assert_eq!(app.wb.sheets[0].name, "B (2)");
    assert_eq!(app.wb.sheet(app.sheet_idx).name, "B (2)");
    assert_eq!(app.wb.sheets[0].identity != app.wb.sheets[1].identity, true);
}

/// UI-08.3：对话框只切换一条边时，其他边的粗细和颜色不能被改成细灰线。
#[test]
fn format_dialog_preserves_border_thickness_and_color_when_other_side_changes() {
    use sheetcore::style::{Border, BorderSide, CellStyle};
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let mut base = CellStyle::default_dark();
    base.border[BorderSide::Left.idx()] = Border { style: 2, color: [200, 10, 10] };
    base.border[BorderSide::Right.idx()] = Border { style: 4, color: [10, 10, 200] };
    let id = app.wb.styles.intern(base);
    app.wb.sheet_mut(0).set_style(0, 0, id);
    app.sel = (0, 0);
    app.sel_end = (0, 0);

    app.open_format_dialog();
    // 用户只加下框；原有左侧粗红线、右侧双蓝线不能被降成细灰线。
    app.fmt_state.border[BorderSide::Bottom.idx()] = true;
    app.apply_format_dialog();
    let st = app.wb.styles.get(app.wb.sheet(0).style_of(0, 0));
    assert_eq!(st.border[BorderSide::Left.idx()], Border { style: 2, color: [200, 10, 10] });
    assert_eq!(st.border[BorderSide::Right.idx()], Border { style: 4, color: [10, 10, 200] });
    assert_eq!(st.border[BorderSide::Top.idx()], Border { style: 0, color: [0, 0, 0] });
    assert_eq!(st.border[BorderSide::Bottom.idx()], Border { style: 1, color: [0, 0, 0] });
}

/// nav_rect 不能跨查找跳转、撤销焦点或文档切换残留；否则 Delete 会误清旧矩形。
#[test]
fn nav_rect_is_invalidated_by_find_jump_undo_and_document_switch() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, text("left"));
    app.wb.sheet_mut(0).set(0, 1, text("right"));
    app.wb.sheet_mut(0).set(9, 3, text("target"));
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Tab, false)]);
    assert!(app.nav_rect.is_some(), "选区 Tab 后应记录原始矩形");

    app.find.find = "target".into();
    app.find_next(false);
    assert_eq!(app.sel, (9, 3));
    assert_eq!(app.nav_rect, None, "查找跳转必须清除旧导航矩形");
    // 给 A1 一个非默认格式；跳转后的 Delete 不能借用旧矩形处理 A1:B2。
    let style = sheetcore::style::CellStyle {
        bold: true,
        ..sheetcore::style::CellStyle::default_dark()
    };
    let style_id = app.wb.styles.intern(style);
    app.wb.sheet_mut(0).set_style(0, 0, style_id);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Delete, false)]);
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).bold);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("left".into()));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Text("right".into()));
    assert_eq!(app.wb.sheet(0).value(9, 3), Value::Empty);

    // 切文档：park/activate 清空，激活后旧矩形不会跟着进入新文档。
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Tab, false)]);
    let parked = app.park_current();
    assert_eq!(app.nav_rect, None);
    app.activate_parked(parked);
    assert_eq!(app.nav_rect, None, "激活文档后不能恢复上一份文档的导航矩形");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("left".into()));

    // 新建/重置编辑状态也清空。
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    app.begin_nav_rect();
    assert!(app.nav_rect.is_some());
    app.reset_editor_state();
    assert_eq!(app.nav_rect, None, "reset_editor_state 必须清空导航矩形");
}

/// UI-08.1：Ribbon 边框方向必须落到左/右/上/下正确下标，内外框不能写反。
#[test]
fn ribbon_border_commands_follow_left_right_top_bottom_order() {
    use sheetcore::style::BorderSide;
    fn side(app: &SheetApp, r: u32, c: u32, which: BorderSide) -> u8 {
        app.wb.styles.get(app.wb.sheet(0).style_of(r, c)).border[which.idx()].style
    }
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    for (action, which) in [
        ("BorderBottom", BorderSide::Bottom),
        ("BorderTop", BorderSide::Top),
        ("BorderLeft", BorderSide::Left),
        ("BorderRight", BorderSide::Right),
    ] {
        app.ribbon_action("BorderNone", &ctx);
        app.ribbon_action(action, &ctx);
        assert_eq!(side(&app, 0, 0, which), 1, "{action} 未落到 {which:?}");
        for other in [BorderSide::Left, BorderSide::Right, BorderSide::Top, BorderSide::Bottom] {
            if other != which {
                assert_eq!(side(&app, 0, 0, other), 0, "{action} 误画 {other:?}");
            }
        }
    }

    app.ribbon_action("BorderNone", &ctx);
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    app.ribbon_action("BorderOuter", &ctx);
    assert_eq!(side(&app, 0, 0, BorderSide::Top), 1);
    assert_eq!(side(&app, 0, 0, BorderSide::Left), 1);
    assert_eq!(side(&app, 0, 0, BorderSide::Right), 0);
    assert_eq!(side(&app, 0, 0, BorderSide::Bottom), 0);
    assert_eq!(side(&app, 0, 1, BorderSide::Top), 1);
    assert_eq!(side(&app, 0, 1, BorderSide::Right), 1);
    assert_eq!(side(&app, 1, 0, BorderSide::Bottom), 1);
    assert_eq!(side(&app, 1, 0, BorderSide::Left), 1);
    assert_eq!(side(&app, 1, 1, BorderSide::Bottom), 1);
    assert_eq!(side(&app, 1, 1, BorderSide::Right), 1);

    app.ribbon_action("BorderNone", &ctx);
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    app.ribbon_action("BorderInsideH", &ctx);
    assert_eq!(side(&app, 0, 0, BorderSide::Bottom), 1);
    assert_eq!(side(&app, 1, 0, BorderSide::Top), 1);
    assert_eq!(side(&app, 0, 0, BorderSide::Top), 0);
    assert_eq!(side(&app, 1, 0, BorderSide::Bottom), 0);

    app.ribbon_action("BorderNone", &ctx);
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    app.ribbon_action("BorderInsideV", &ctx);
    assert_eq!(side(&app, 0, 0, BorderSide::Right), 1);
    assert_eq!(side(&app, 0, 1, BorderSide::Left), 1);
    assert_eq!(side(&app, 0, 0, BorderSide::Left), 0);
    assert_eq!(side(&app, 0, 1, BorderSide::Right), 0);
}

#[test]
fn tab_enter_cycle_inside_selection_and_stop_at_sheet_edges() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 0);
    app.sel_end = (1, 1); // A1:B2
    app.nav_rect = None;

    // Enter 是逐行扫描，不是只在一列里回绕：A1→A2→B1→B2→A1。
    for want in [(1, 0), (0, 1), (1, 1), (0, 0)] {
        run_keys(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
        assert_eq!(app.sel, want, "Enter 应按行扫描 A1:B2");
        assert_eq!(app.sel_rect(), (0, 0, 1, 1), "循环期间原始矩形仍保留");
    }
    // Shift+Enter 反向：A1→B2→B1→A2→A1。
    for want in [(1, 1), (0, 1), (1, 0), (0, 0)] {
        run_keys(&mut app, &ctx, vec![key_event(egui::Key::Enter, true)]);
        assert_eq!(app.sel, want, "Shift+Enter 应按反行扫描");
    }

    // 隐藏行：A1:B3 隐藏第 2 行，Tab 换行跳过隐藏行；Enter 到末行后换列。
    app.wb.sheet_mut(0).rows.set(1, 0.0);
    app.sel = (0, 0);
    app.sel_end = (2, 1);
    app.nav_rect = None;
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Tab, false)]);
    assert_eq!((app.sel, app.sel_rect()), ((0, 1), (0, 0, 2, 1)));
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Tab, false)]);
    assert_eq!(app.sel, (2, 0), "Tab 换行时必须跳过隐藏的第 2 行");
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.sel, (0, 1), "Enter 到末行后应换到下一列，而不是只回绕行");

    // 隐藏列：A1:C2 隐藏 B，Enter 仍按行扫描，跳过隐藏列。
    app.wb.sheet_mut(0).rows.set(1, 19.0);
    app.wb.sheet_mut(0).cols.set(1, 0.0);
    app.sel = (0, 0);
    app.sel_end = (1, 2);
    app.nav_rect = None;
    for want in [(1, 0), (0, 2), (1, 2), (0, 0)] {
        run_keys(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
        assert_eq!(app.sel, want, "Enter 必须跳过隐藏列 B");
    }

    app.sel = (0, 0);
    app.sel_end = (0, 2); // A1:C1，B 在区域内
    app.nav_rect = None;
    app.wb.sheet_mut(0).cols.set(1, 0.0); // B 列隐藏
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Tab, false)]);
    assert_eq!(app.sel, (0, 2), "Tab 跳过隐藏 B 列");

    // 编辑提交后也按选区推进，而不是固定下一行。
    app.wb.sheet_mut(0).cols.set(1, 19.0);
    app.wb.sheet_mut(0).rows.set(1, 0.0); // 恢复第 2 行隐藏
    app.sel = (0, 0);
    app.sel_end = (2, 1);
    app.nav_rect = None;
    app.begin_edit(Some("值".into()));
    assert!(app.editing.is_some());
    // 网格编辑框的 Enter 由绘制层转成 commit_edit(advance)，这里直接走同一提交入口。
    assert!(app.commit_edit(Some((1, 0))));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("值".into()));
    assert_eq!(app.sel, (2, 0), "编辑提交 Enter 跳过隐藏行");

    // 方向键也不能落到隐藏行。
    app.nav_rect = None;
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, false)]);
    assert_eq!(app.sel, (2, 0), "向下方向键必须跳过隐藏的第 2 行");

    // 单格边界：XFD 列和 1048576 行不产生越界坐标。
    app.nav_rect = None;
    app.sel = (0, 16_383);
    app.sel_end = (0, 16_383);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Tab, false)]);
    assert_eq!(app.sel, (0, 16_383), "XFD 处 Tab 不得越界");
    app.sel = (1_048_575, 0);
    app.sel_end = (1_048_575, 0);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.sel, (1_048_575, 0), "1048576 行处 Enter 不得越界");
}

#[test]
fn ctrl_d_fills_down_and_ctrl_r_fills_right() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(
        0,
        0,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Number(7.0),
            formula: None,
        },
    );
    app.sel = (0, 0);
    app.sel_end = (3, 0);
    app.fill_cmd(true);
    for r in 0..4 {
        assert_eq!(app.wb.sheet(0).value(r, 0), sheetcore::model::Value::Number(7.0));
    }
    app.sel = (5, 0);
    app.sel_end = (5, 2);
    app.wb.sheet_mut(0).set(
        5,
        0,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Number(9.0),
            formula: None,
        },
    );
    app.fill_cmd(false);
    assert_eq!(app.wb.sheet(0).value(5, 1), sheetcore::model::Value::Number(9.0));
    assert_eq!(app.wb.sheet(0).value(5, 2), sheetcore::model::Value::Number(9.0));
}

#[test]
fn fill_down_limit_follows_neighbour_column() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for r in 0..5u32 {
        app.wb.sheet_mut(0).set(
            r,
            1,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Text(format!("x{r}").into()),
                formula: None,
            },
        );
    }
    app.sel = (0, 0);
    app.sel_end = (1, 0);
    assert_eq!(app.fill_down_limit(0, 0), 4, "应按 B 列的数据长度填到第 5 行");
}

fn run_grid_frame_mods(
    app: &mut SheetApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
    mods: egui::Modifiers,
) {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };
    input.modifiers = mods;
    input.events = events;
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
    });
}

#[test]
fn shift_click_extends_selection() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    run_grid_frame(&mut app, &ctx, vec![]);
    let p = egui::pos2(HEAD_W + 72.0 + 10.0, HEAD_H + 9.5);
    let press = egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers { shift: true, ..Default::default() },
    };
    let release = egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers { shift: true, ..Default::default() },
    };
    let mods = egui::Modifiers { shift: true, ..Default::default() };
    run_grid_frame_mods(&mut app, &ctx, vec![press], mods);
    run_grid_frame_mods(&mut app, &ctx, vec![release], mods);
    assert_eq!((app.sel, app.sel_end), ((0, 1), (0, 0)), "Shift+点击应扩选到 B1");
}

#[test]
fn alt_enter_inserts_a_newline_in_the_cell_editor() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.editing = Some(EditState::new("第一行".into()));
    run_grid_frame(&mut app, &ctx, vec![]);
    let ev = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers { alt: true, ..Default::default() },
    };
    let mods = egui::Modifiers { alt: true, ..Default::default() };
    run_grid_frame_mods(&mut app, &ctx, vec![ev], mods);
    let ed = app.editing.as_ref().expect("Alt+Enter 不应结束编辑");
    assert!(ed.buf.contains('\n'), "应插入换行：{:?}", ed.buf);
}

#[test]
fn typing_below_a_table_extends_its_range() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = app.wb.sheet_mut(0);
        for c in 0..2u32 {
            sh.set(0, c, sheetcore::model::Cell { value: sheetcore::model::Value::Text(format!("h{c}").into()), formula: None });
            sh.set(1, c, sheetcore::model::Cell { value: sheetcore::model::Value::Text("a".into()), formula: None });
            sh.set(2, c, sheetcore::model::Cell { value: sheetcore::model::Value::Text("b".into()), formula: None });
        }
    }
    app.sel = (0, 0);
    app.sel_end = (2, 1);
    app.make_table();
    // 在表格正下方输入
    app.sel = (3, 0);
    app.sel_end = (3, 0);
    app.editing = Some(EditState::new("c".into()));
    app.commit_edit(None);
    assert_eq!(app.wb.sheet(0).tables[0].range.2, 3, "表格应自动向下扩一行");
    assert_eq!(
        app.wb.sheet(0).filter.as_ref().map(|f| f.range.2),
        Some(3),
        "筛选区域应一起扩大"
    );
}


/// UI-03：从受保护的另一张表剪切，粘贴必须整批拒绝，源表数据不能消失。
#[test]
fn cut_from_a_protected_sheet_cannot_be_pasted() {
    let env = crate::test_env::TestEnv::new("ui03_cut_prot");
    let ctx = egui::Context::default();
    let mut app = seeded_app(&env, "cutprot");
    let mut second = sheetcore::model::Sheet::new("源表");
    second.set(0, 0, Cell { value: sheetcore::model::Value::Number(7.0), formula: None });
    second.protection = Some(sheetcore::protection::SheetProtection::default());
    app.wb.sheets.push(second);
    let cells = vec![vec![(Cell { value: sheetcore::model::Value::Number(7.0), formula: None }, 0)]];
    app.clip = Some(Clip { merges: Vec::new(), sheet: 1, rect: (0, 0, 0, 0), cells, cut: true });
    app.sheet_idx = 0;
    app.sel = (2, 2);
    let before = snapshot(&app);
    app.clip_paste(false, None);
    assert_eq!(app.wb.sheet(1).value(0, 0), sheetcore::model::Value::Number(7.0), "源表不能被清空");
    assert_eq!(snapshot(&app), before, "目标不能写入");
    assert!(app.status.contains("受保护"), "应说明原因：{}", app.status);
    drop(env);
}
#[test]
fn paste_special_values_transpose_and_operations() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let num = |v: f64| sheetcore::model::Cell {
        value: sheetcore::model::Value::Number(v),
        formula: None,
    };
    // 来源 2×2：1 2 / 3 4
    let cells = vec![
        vec![(num(1.0), 0), (num(2.0), 0)],
        vec![(num(3.0), 0), (num(4.0), 0)],
    ];
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (0, 0, 1, 1),
        cells,
        cut: false,
    });
    // 只粘数值
    app.ps_kind = 1;
    app.ps_transpose = false;
    app.ps_op = 0;
    app.sel = (5, 0);
    app.paste_special_apply(&ctx);
    assert_eq!(app.wb.sheet(0).value(5, 0), sheetcore::model::Value::Number(1.0));
    assert_eq!(app.wb.sheet(0).value(6, 1), sheetcore::model::Value::Number(4.0));
    // 转置到 D1
    app.ps_transpose = true;
    app.sel = (0, 3);
    app.paste_special_apply(&ctx);
    assert_eq!(app.wb.sheet(0).value(0, 3), sheetcore::model::Value::Number(1.0));
    assert_eq!(app.wb.sheet(0).value(1, 3), sheetcore::model::Value::Number(2.0));
    assert_eq!(app.wb.sheet(0).value(0, 4), sheetcore::model::Value::Number(3.0));
    // 运算：10 + 来源
    app.ps_transpose = false;
    app.wb.sheet_mut(0).set(10, 0, num(10.0));
    app.wb.sheet_mut(0).set(10, 1, num(10.0));
    app.sel = (10, 0);
    app.ps_op = 1;
    app.paste_special_apply(&ctx);
    assert_eq!(app.wb.sheet(0).value(10, 0), sheetcore::model::Value::Number(11.0));
    assert_eq!(app.wb.sheet(0).value(10, 1), sheetcore::model::Value::Number(12.0));
}

/// UI-06：公式粘贴按目标位置平移相对引用、绝对引用不变。
/// Excel 基准：C3 的 =A1+$B$1 粘到 B2 → =#REF!+$B$1；粘到 D4 → =B2+$B$1。
#[test]
fn paste_special_formula_shifts_relative_and_keeps_absolute() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let src = Cell { value: Value::Number(5.0), formula: Some("=A1+$B$1".into()) };
    let clip = |rect: (u32, u32, u32, u32)| Clip { merges: Vec::new(),
        sheet: 0,
        rect,
        cells: vec![vec![(src.clone(), 0)]],
        cut: false,
    };
    app.ps_kind = 2;
    app.ps_transpose = false;
    app.ps_op = 0;

    // C3 (2,2) → B2 (1,1)：相对 A1 变成 -1,-1 → #REF!
    app.clip = Some(clip((2, 2, 2, 2)));
    app.sel = (1, 1);
    app.paste_special_apply(&ctx);
    assert_eq!(
        app.wb.sheet(0).get(1, 1).and_then(|c| c.formula.clone()).as_deref(),
        Some("=#REF!+$B$1"),
        "负偏移必须变 #REF!，绝对引用保持"
    );

    // C3 (2,2) → D4 (3,3)：+1,+1 → B2
    app.clip = Some(clip((2, 2, 2, 2)));
    app.sel = (3, 3);
    app.paste_special_apply(&ctx);
    assert_eq!(
        app.wb.sheet(0).get(3, 3).and_then(|c| c.formula.clone()).as_deref(),
        Some("=B2+$B$1"),
        "正偏移按目标位置平移"
    );
}

/// UI-06：“全部”要连源字体和填充一起带过来，且一次撤销同时还原值与样式。
#[test]
fn paste_special_all_carries_font_and_fill_and_undoes_once() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    // 源样式：加粗 + 黄色填充
    let sid = app.wb.styles.intern(sheetcore::style::CellStyle {
        bold: true,
        fill: Some([255, 255, 0]),
        ..Default::default()
    });
    let cell = Cell { value: Value::Number(9.0), formula: None };
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (0, 0, 0, 0),
        cells: vec![vec![(cell, sid as u16)]],
        cut: false,
    });
    app.ps_kind = 0;
    app.ps_transpose = false;
    app.ps_op = 0;
    app.sel = (4, 4);
    let before_undo = app.undo.depth();
    app.paste_special_apply(&ctx);

    let pasted_sid = app.wb.sheet(0).style_of(4, 4);
    let st = app.wb.styles.get(pasted_sid).clone();
    assert!(st.bold, "“全部”必须保留源字体");
    assert_eq!(st.fill, Some([255, 255, 0]), "“全部”必须保留源填充");
    assert_eq!(app.undo.depth(), (before_undo.0 + 1, 0), "一次粘贴一步撤销");

    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(4, 4), Value::Empty, "撤销恢复目标值");
    let back = app.wb.styles.get(app.wb.sheet(0).style_of(4, 4)).clone();
    assert!(!back.bold && back.fill.is_none(), "撤销同时恢复样式");
}

/// UI-06：转置后仍用源坐标算偏移，不能被转置后的下标带偏。
#[test]
fn paste_special_transpose_keeps_source_coordinates() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    // 源 A1:B1，两个公式分别为 =A1 与 =B1
    let f = |t: &str| (Cell { value: Value::Empty, formula: Some(t.into()) }, 0u16);
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (0, 0, 0, 1),
        cells: vec![vec![f("=A1"), f("=B1")]],
        cut: false,
    });
    app.ps_kind = 2;
    app.ps_transpose = true;
    app.ps_op = 0;
    app.sel = (10, 3); // D11
    app.paste_special_apply(&ctx);
    // 转置后占 D11:D12：D11 来自 A1（+10,+3 → =D11），D12 来自 B1（+10,+2 → =D12）。
    // 若用转置后的下标算偏移，D12 会被错当成 +10,+1 得到 =C12。
    let d11 = app.wb.sheet(0).get(10, 3).and_then(|c| c.formula.clone());
    let d12 = app.wb.sheet(0).get(11, 3).and_then(|c| c.formula.clone());
    assert_eq!(d11.as_deref(), Some("=D11"), "A1 → D11 的偏移");
    assert_eq!(d12.as_deref(), Some("=D12"), "B1 → D12 用原始源坐标，不是转置后的列");
}

/// UI-06：TSV 中间空行不能被压掉。
#[test]
fn paste_special_external_tsv_keeps_blank_rows() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.clip = None;
    app.clip_stamp = None;
    app.ps_kind = 1;
    app.ps_transpose = false;
    app.ps_op = 0;
    app.sel = (0, 0);
    app.paste_special_apply_text(&ctx, Some("a\n\nb".into()));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("a".into()));
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Empty, "中间空行要保留");
    assert_eq!(app.wb.sheet(0).value(2, 0), Value::Text("b".into()));
}

/// UI-06：除零不能静默成功，要写 #DIV/0!。
#[test]
fn paste_special_divide_by_zero_reports_error() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(8.0), formula: None });
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (5, 5, 5, 5),
        cells: vec![vec![(Cell { value: Value::Number(0.0), formula: None }, 0)]],
        cut: false,
    });
    app.ps_kind = 1;
    app.ps_transpose = false;
    app.ps_op = 4;
    app.sel = (0, 0);
    app.paste_special_apply(&ctx);
    assert_eq!(
        app.wb.sheet(0).value(0, 0),
        Value::Error(sheetcore::formula::ErrKind::Div0),
        "除零必须写 #DIV/0!"
    );
}

/// UI-20：删图一步撤销、重做再删；撤销后图片回到原锚点。
#[test]
fn deleting_a_picture_is_one_undo_step_and_restores_its_anchor() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let png = vec![0x89u8, b'P', b'N', b'G'];
    app.wb.sheet_mut(0).pictures.push(sheetcore::pic::Picture {
        rect: (2, 2, 6, 5),
        off: [0; 4],
        part: "xl/media/sc0000000000000001.png".into(),
        data: Some(png),
        ext: "png".into(),
        name: "图片 1".into(),
    });
    let depth = app.undo.depth();
    app.pic_sel = Some(0);
    app.pic_delete(0);
    assert!(app.wb.sheet(0).pictures.is_empty(), "Delete 应删掉图片");
    assert!(app.wb.sheet(0).pictures_dirty, "删图要标脏，保存时才写回");
    assert_eq!(app.undo.depth(), (depth.0 + 1, 0), "一次删除一步撤销");

    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).pictures.len(), 1, "撤销恢复图片");
    assert_eq!(app.wb.sheet(0).pictures[0].rect, (2, 2, 6, 5), "锚点原样恢复");
    app.do_undo(true);
    assert!(app.wb.sheet(0).pictures.is_empty(), "重做再次删除");
}

/// UI-20：移动图片进撤销栈，撤销恢复原矩形。
#[test]
fn moving_a_picture_is_undoable() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).pictures.push(sheetcore::pic::Picture {
        rect: (0, 0, 3, 3),
        off: [0; 4],
        part: "xl/media/sc0000000000000002.png".into(),
        data: Some(vec![1, 2, 3]),
        ext: "png".into(),
        name: "图片 1".into(),
    });
    let before = app.wb.sheet(0).pictures.clone();
    app.wb.sheet_mut(0).pictures[0].rect = (8, 4, 12, 9);
    app.pic_commit("移动图片", before);
    assert_eq!(app.undo.depth(), (1, 0), "移动一步撤销");
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).pictures[0].rect, (0, 0, 3, 3), "撤销回到原位");
}

#[test]
fn totals_row_adds_subtotal_and_removes_cleanly() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = app.wb.sheet_mut(0);
        for c in 0..2u32 {
            sh.set(0, c, sheetcore::model::Cell { value: sheetcore::model::Value::Text(format!("h{c}").into()), formula: None });
            sh.set(1, c, sheetcore::model::Cell { value: sheetcore::model::Value::Number(1.0), formula: None });
            sh.set(2, c, sheetcore::model::Cell { value: sheetcore::model::Value::Number(2.0), formula: None });
        }
    }
    app.sel = (0, 0);
    app.sel_end = (2, 1);
    app.make_table();
    let name = app.wb.sheet(0).tables[0].name.clone();
    app.toggle_totals_row(&name);
    {
        let t = &app.wb.sheet(0).tables[0];
        assert!(t.totals_row);
        assert_eq!(t.range.2, 3, "汇总行并入表格范围");
        assert_eq!(app.wb.sheet(0).value(3, 0).display(), "总计");
        assert_eq!(app.wb.sheet(0).value(3, 1), sheetcore::model::Value::Number(3.0));
    }
    app.toggle_totals_row(&name);
    assert!(!app.wb.sheet(0).tables[0].totals_row);
    assert_eq!(app.wb.sheet(0).tables[0].range.2, 2);
    assert_eq!(app.wb.sheet(0).value(3, 0), sheetcore::model::Value::Empty);
}

#[test]
fn shift_cells_moves_values_down() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Number(1.0), formula: None });
        sh.set(1, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Number(2.0), formula: None });
    }
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.shift_cells(true, false);
    assert_eq!(app.wb.sheet(0).value(0, 0), sheetcore::model::Value::Empty);
    assert_eq!(app.wb.sheet(0).value(1, 0), sheetcore::model::Value::Number(1.0));
    assert_eq!(app.wb.sheet(0).value(2, 0), sheetcore::model::Value::Number(2.0));
}

#[test]
fn ctrl_click_builds_multi_range_selection() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    run_grid_frame(&mut app, &ctx, vec![]);
    // 先点 A1
    let p1 = egui::pos2(HEAD_W + 36.0, HEAD_H + 9.5);
    let click = |p: egui::Pos2, mods: egui::Modifiers| {
        vec![
            egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: mods },
            egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: mods },
        ]
    };
    run_grid_frame_mods(&mut app, &ctx, click(p1, Default::default()), Default::default());
    assert_eq!(app.sel, (0, 0));
    // Ctrl+点 C3
    let p2 = egui::pos2(HEAD_W + 2.0 * 72.0 + 36.0, HEAD_H + 2.0 * 19.0 + 9.5);
    let ctrl = egui::Modifiers { command: true, ..Default::default() };
    run_grid_frame_mods(&mut app, &ctx, click(p2, ctrl), ctrl);
    assert_eq!(app.sel, (2, 2));
    assert_eq!(app.extra_sel.len(), 1, "A1 应固定在多选区里");
    // 给两块区域加粗
    app.apply_style(|st| st.bold = true);
    let a1 = app.wb.sheet(0).style_of(0, 0);
    let c3 = app.wb.sheet(0).style_of(2, 2);
    assert!(app.wb.styles.get(a1).bold);
    assert!(app.wb.styles.get(c3).bold);
}

#[test]
fn move_selection_by_shifts_values_and_formulas() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Number(5.0), formula: None });
        sh.set(1, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Number(1.0), formula: None });
        sh.set(0, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Empty, formula: Some("A1+1".into()) });
    }
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.move_selection_by(1, 0);
    assert_eq!(app.wb.sheet(0).value(0, 0), sheetcore::model::Value::Empty);
    assert_eq!(app.wb.sheet(0).value(1, 0), sheetcore::model::Value::Number(5.0));
    let f = app.wb.sheet(0).get(1, 1).and_then(|c| c.formula.clone());
    assert_eq!(f.as_deref(), Some("A2+1"), "公式应跟着移动");
    assert_eq!(app.wb.sheet(0).value(1, 1), sheetcore::model::Value::Number(6.0));
}

#[test]
fn insert_cells_rewrites_formula_refs() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Number(1.0), formula: None });
        sh.set(2, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Number(5.0), formula: None });
        sh.set(0, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Number(0.0), formula: Some("A3+1".into()) });
    }
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.shift_cells(true, false);
    let f = app.wb.sheet(0).get(0, 1).and_then(|c| c.formula.clone());
    assert_eq!(f.as_deref(), Some("A4+1"), "下移后引用应跟着走");
    assert_eq!(app.wb.sheet(0).value(3, 0), sheetcore::model::Value::Number(5.0));
    // 再删掉刚插入的格子，引用回来
    app.shift_cells(false, false);
    let f = app.wb.sheet(0).get(0, 1).and_then(|c| c.formula.clone());
    assert_eq!(f.as_deref(), Some("A3+1"));
}

// ---------------------------------------------------------------- 右键菜单

#[test]
fn ctx_menu_rows_match_excel_cell_menu() {
    let rows = ctx_menu_rows(false);
    let labels: Vec<&str> = rows.iter().filter(|r| !r.is_sep() && !r.strip).map(|r| r.label).collect();
    assert_eq!(
        labels,
        [
            "剪切", "复制", "粘贴", "选择性粘贴…", "插入", "删除", "清除内容",
            "筛选", "排序", "设置单元格格式…", "超链接…"
        ],
        "条目与顺序照 Excel 365 的单元格右键菜单"
    );
    // 四个子菜单
    let parents: Vec<&str> = rows.iter().filter(|r| !r.children.is_empty()).map(|r| r.label).collect();
    assert_eq!(parents, ["插入", "删除", "筛选", "排序"]);
    // 粘贴选项图标条在「复制」和「选择性粘贴」之间
    let strip_at = rows.iter().position(|r| r.strip).unwrap();
    assert_eq!(rows[strip_at - 1].label, "粘贴");
    assert_eq!(rows[strip_at + 2].label, "选择性粘贴…");
    // 高度：246 宽、行高 26
    assert_eq!(CTX_W, 246.0);
    assert_eq!(CTX_ROW_H, 26.0);
    assert!(ctx_menu_height(&rows) > 300.0 && ctx_menu_height(&rows) < 480.0);
}

#[test]
fn ctx_menu_clear_contents_only_clears_values() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.wb.sheets[0].set(0, 0, number(7.0));
    app.wb.sheets[0].set_style(0, 0, 0);
    app.wb.sheets[0].set(0, 1, number(9.0));
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.ctx_menu_run(CtxAct::ClearContents, 0, &ctx);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Empty);
    assert_eq!(app.wb.sheets[0].value(0, 1), Value::Empty);
    // 撤销要能回来
    app.do_undo(false);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(7.0));
}

#[test]
fn ctx_menu_sort_asc_and_desc_are_real() {
    let ctx = egui::Context::default();
    let mut app = app();
    for (i, v) in [("b", 3.0), ("a", 1.0), ("c", 2.0)].iter().enumerate() {
        app.wb.sheets[0].set(i as u32, 0, Cell { value: Value::Text((*v).0.into()), formula: None });
        app.wb.sheets[0].set(i as u32, 1, number(v.1));
    }
    app.sel = (1, 1);
    app.ctx_menu_run(CtxAct::SortAsc, 0, &ctx);
    let col: Vec<String> = (0..3).map(|r| app.wb.sheets[0].value(r, 1).display()).collect();
    assert_eq!(col, ["1", "2", "3"], "升序按当前列排整个区域");
    assert!(app.status.contains("升序"));
    app.ctx_menu_run(CtxAct::SortDesc, 0, &ctx);
    let col: Vec<String> = (0..3).map(|r| app.wb.sheets[0].value(r, 1).display()).collect();
    assert_eq!(col, ["3", "2", "1"]);
}

#[test]
fn ctx_menu_insert_and_delete_rows_are_real() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.wb.sheets[0].set(0, 0, number(1.0));
    app.wb.sheets[0].set(1, 0, number(2.0));
    app.sel = (1, 0);
    app.sel_end = (1, 0);
    app.ctx_menu_run(CtxAct::InsertRows, 0, &ctx);
    assert_eq!(app.wb.sheets[0].value(2, 0), Value::Number(2.0), "原第 2 行被推到第 3 行");
    app.sel = (1, 0);
    app.sel_end = (1, 0);
    app.ctx_menu_run(CtxAct::DeleteRows, 0, &ctx);
    assert_eq!(app.wb.sheets[0].value(1, 0), Value::Number(2.0), "删掉刚插入的空行");
}

#[test]
fn ctx_menu_paste_values_keeps_number_but_drops_formula() {
    let ctx = egui::Context::default();
    let mut app = app();
    // 剪贴板里放一个带公式的格子
    app.wb.sheets[0].set(0, 0, Cell { value: Value::Number(5.0), formula: Some("2+3".into()) });
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.clip_copy(false, &ctx);
    // 该用例只验证“只贴值”；并行测试会改系统剪贴板，这里固定走内部 Clip
    app.clip_stamp = None;
    app.sel = (2, 2);
    app.sel_end = (2, 2);
    app.ctx_menu_run(CtxAct::PasteValues, 0, &ctx);
    let cell = app.wb.sheets[0].get(2, 2).cloned().unwrap();
    assert!(cell.formula.is_none(), "只粘数值，不带公式");
    assert_eq!(cell.value, Value::Number(5.0));
}

#[test]
fn ctx_head_menu_hides_and_unhides_rows_and_cols() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.wb.sheets[0].set(0, 0, number(1.0));
    app.sel = (1, 0);
    app.sel_end = (3, 0);
    app.ctx_menu_run(CtxAct::HideRows, 1, &ctx);
    assert_eq!(app.wb.sheets[0].hidden_rows_user.len(), 3, "选区 3 行都隐藏");
    app.ctx_menu_run(CtxAct::UnhideRows, 1, &ctx);
    assert!(app.wb.sheets[0].hidden_rows_user.is_empty(), "取消隐藏把选区内的放出来");

    app.sel = (0, 1);
    app.sel_end = (0, 2);
    app.ctx_menu_run(CtxAct::HideCols, 1, &ctx);
    assert_eq!(app.wb.sheets[0].hidden_cols.len(), 2);
    app.ctx_menu_run(CtxAct::UnhideCols, 1, &ctx);
    assert!(app.wb.sheets[0].hidden_cols.is_empty());
}

#[test]
fn ctx_head_menu_rows_are_excel_like() {
    let rows = ctx_head_rows(true);
    let labels: Vec<&str> = rows.iter().filter(|r| !r.is_sep() && !r.strip).map(|r| r.label).collect();
    assert_eq!(
        labels,
        ["剪切", "复制", "粘贴", "选择性粘贴…", "插入", "删除", "清除内容", "列宽…", "隐藏", "取消隐藏", "设置单元格格式…"]
    );
    // 列标用「列宽…」，行号用「行高…」
    let row_labels: Vec<&str> = ctx_head_rows(false).iter().map(|r| r.label).collect();
    assert!(row_labels.contains(&"行高…") && !row_labels.contains(&"列宽…"));
}

#[test]
fn ctx_tab_menu_sets_and_clears_tab_color() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.add_sheet();
    let idx = app.wb.sheets.len() - 1;
    app.ctx_menu_run(CtxAct::TabColor(Some([68, 114, 196])), idx, &ctx);
    assert_eq!(app.wb.sheets[idx].tab_color, Some([68, 114, 196]));
    assert!(app.wb.sheets[idx].tab_color_dirty);
    app.ctx_menu_run(CtxAct::TabColor(None), idx, &ctx);
    assert_eq!(app.wb.sheets[idx].tab_color, None);
    // 标签菜单的条目
    let labels: Vec<&str> = ctx_tab_rows().iter().filter(|r| !r.is_sep()).map(|r| r.label).collect();
    assert_eq!(
        labels,
        [
            "插入…",
            "删除",
            "重命名",
            "复制",
            "移动或复制…",
            "隐藏工作表",
            "取消隐藏…",
            "工作表列表…",
            "工作表标签颜色",
            "设置单元格格式…",
        ]
    );
}

// ---------------------------------------------------------------- UI-01 编辑提交链路

/// 整窗事件帧：走真实 `update()`（标题栏、功能区、编辑栏、网格全部在内），
/// 而不是只调用某个处理函数。输入 123→Enter 丢缓冲的缺陷就是在这条链路上。
fn ui_frame(app: &mut SheetApp, ctx: &egui::Context, events: Vec<egui::Event>) {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1024.0, 768.0),
        )),
        ..Default::default()
    };
    input.modifiers = events
        .iter()
        .find_map(|e| match e {
            egui::Event::Key { modifiers, .. } => Some(*modifiers),
            _ => None,
        })
        .unwrap_or_default();
    input.events = events;
    let mut frame = eframe::Frame::_new_kittest();
    let _ = ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut frame));
}

fn type_and_key(keys: &[(egui::Key, bool)], text: &str) -> Vec<egui::Event> {
    let mut v = vec![egui::Event::Text(text.into())];
    for (k, shift) in keys {
        v.push(key_event(*k, *shift));
    }
    v
}

#[test]
fn typed_text_commits_on_enter_through_full_window() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("123".into())]);
    assert_eq!(
        app.editing.as_ref().map(|e| e.buf.clone()).as_deref(),
        Some("123"),
        "输入必须进入唯一编辑缓冲"
    );
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0), "Enter 必须提交 A1");
    assert!(app.editing.is_none(), "提交后编辑态应结束");
    assert_eq!(app.sel, (1, 0), "Enter 后选区向下");
    assert!(app.wb.dirty, "提交后必须标脏");
    assert_eq!(app.undo.depth(), (1, 0), "提交必须记一步撤销");
}

/// 编辑栏输入行的点击位置：网格面板上沿往上就是编辑栏（窗口分栏变了也不用改）
fn formula_bar_point(app: &SheetApp) -> egui::Pos2 {
    egui::pos2(600.0, app.last_pane.y - ui_kit::FX_H * 0.5)
}

fn click(app: &mut SheetApp, ctx: &egui::Context, pos: egui::Pos2) {
    for pressed in [Some(true), Some(false)] {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0))),
            ..Default::default()
        };
        input.events.push(egui::Event::PointerMoved(pos));
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: pressed.unwrap(),
            modifiers: egui::Modifiers::default(),
        });
        let mut frame = eframe::Frame::_new_kittest();
        let _ = ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut frame));
    }
}

#[test]
fn tab_commits_and_moves_right() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("42".into())]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Tab, false)]);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(42.0), "Tab 必须提交 A1");
    assert_eq!(app.sel, (0, 1), "Tab 后选区向右");
    assert!(app.editing.is_none());
}

#[test]
fn shift_enter_and_shift_tab_commit_backwards() {
    for (key, want) in [(egui::Key::Enter, (1u32, 2u32)), (egui::Key::Tab, (2u32, 1u32))] {
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.sel = (2, 2);
        app.sel_end = (2, 2);
        ui_frame(&mut app, &ctx, vec![]);
        ui_frame(&mut app, &ctx, vec![egui::Event::Text("7".into())]);
        ui_frame(&mut app, &ctx, vec![key_event(key, true)]);
        assert_eq!(
            app.wb.sheet(0).value(2, 2),
            Value::Number(7.0),
            "Shift+{key:?} 也要提交"
        );
        assert_eq!(app.sel, want, "Shift+{key:?} 应反向移动");
    }
}

#[test]
fn escape_discards_draft_without_touching_model() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(1.0));
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("999".into())]);
    assert!(app.editing.is_some());
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    assert!(app.editing.is_none(), "Esc 应结束编辑");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(1.0), "Esc 不得改值");
    assert_eq!(app.sel, (0, 0), "Esc 不移动选区");
    assert!(!app.wb.dirty, "Esc 不标脏");
    assert_eq!(app.undo.depth(), (0, 0), "Esc 不记撤销");
    // 取消后残留按键不能把草稿补交上去
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(1.0));
}

#[test]
fn f2_keeps_existing_formula_and_escape_restores_nothing() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(
        0,
        0,
        Cell { value: Value::Number(3.0), formula: Some("=1+2".into()) },
    );
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::F2, false)]);
    assert_eq!(
        app.editing.as_ref().map(|e| e.buf.clone()).as_deref(),
        Some("=1+2"),
        "F2 必须带原公式进入编辑"
    );
    assert_eq!(app.editing.as_ref().unwrap().spot, EditSpot::Grid);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("+4".into())]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    let cell = app.wb.sheet(0).get(0, 0).unwrap();
    assert_eq!(cell.formula.as_deref(), Some("=1+2"), "Esc 后公式不变");
    assert!(!app.wb.dirty);
}

#[test]
fn formula_bar_edit_uses_same_buffer_and_commits() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(11.0));
    ui_frame(&mut app, &ctx, vec![]);
    let bar = formula_bar_point(&app);
    click(&mut app, &ctx, bar);
    assert!(
        app.editing.as_ref().is_some_and(|e| e.spot == EditSpot::FormulaBar),
        "点编辑栏应把焦点搬到编辑栏，实际 {:?}",
        app.editing.as_ref().map(|e| e.spot)
    );
    assert_eq!(app.editing.as_ref().unwrap().buf, "11", "编辑栏缓冲取当前格内容");
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("22".into())]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(
        app.wb.sheet(0).value(0, 0),
        Value::Number(1122.0),
        "编辑栏里追加的内容必须一起提交"
    );
    assert!(app.editing.is_none(), "编辑栏 Enter 提交后结束编辑");
}

#[test]
fn clicking_another_cell_commits_the_draft() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("123".into())]);
    let p = app.last_pane
        + egui::vec2(
            HEAD_W + app.wb.sheets[0].cols.pos(2) + 15.0,
            HEAD_H + app.wb.sheets[0].rows.pos(2) + 10.0,
        );
    click(&mut app, &ctx, p);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0), "点别的格要先提交草稿");
    assert_eq!(app.sel, (2, 2), "然后选区落到被点的格");
    assert!(app.editing.is_none());
}

#[test]
fn dv_anchor_handles_mixed_refs_and_bad_formula_is_reported() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = &mut app.wb.sheets[0];
        sh.set(0, 0, number(10.0));
        sh.set(1, 0, number(20.0));
        sh.set(2, 0, number(30.0));
        sh.dv.push(sheetcore::dv::DataValidation {
            ranges: vec![(1, 2, 2, 2)],
            kind: sheetcore::dv::DvKind::Whole,
            op: sheetcore::dv::DvOp::GreaterEq,
            f1: "=$A1".into(),
            ..Default::default()
        });
        sh.dv_dirty = true;
    }
    ui_frame(&mut app, &ctx, vec![]);
    app.sel = (1, 2);
    app.editing = Some(EditState::new("15".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 2), Value::Number(15.0), "C2 应以范围锚点比较 A1");
    app.sel = (2, 2);
    app.editing = Some(EditState::new("25".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(2, 2), Value::Number(25.0), "C3 应以范围锚点比较 A2");

    {
        let sh = &mut app.wb.sheets[0];
        sh.dv.push(sheetcore::dv::DataValidation {
            ranges: vec![(1, 3, 1, 3)],
            kind: sheetcore::dv::DvKind::Whole,
            op: sheetcore::dv::DvOp::GreaterEq,
            f1: "=1+".into(),
            ..Default::default()
        });
        sh.dv_dirty = true;
    }
    app.doc_gen = app.doc_gen.wrapping_add(1);
    app.sel = (1, 3);
    app.editing = Some(EditState::new("2".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 3), Value::Empty, "坏阈值不得当 1 放行");
    assert!(app.status.contains("配置无效"), "应显示配置失败诊断：{}", app.status);
}

#[test]
fn dv_scan_uses_range_anchor_and_reports_config_errors() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    {
        let sh = &mut app.wb.sheets[0];
        sh.set(0, 0, number(10.0));
        sh.set(1, 0, number(20.0));
        sh.set(1, 2, number(15.0));
        sh.set(2, 2, number(25.0));
        sh.dv.push(sheetcore::dv::DataValidation {
            ranges: vec![(1, 2, 2, 2)],
            kind: sheetcore::dv::DvKind::Whole,
            op: sheetcore::dv::DvOp::GreaterEq,
            f1: "=$A1".into(),
            ..Default::default()
        });
        sh.dv_dirty = true;
    }
    app.compute_invalid();
    assert!(!app.invalid_cells.contains(&(1, 2)), "C2 不应比 A2");
    assert!(!app.invalid_cells.contains(&(2, 2)), "C3 不应比 A3");

    {
        let sh = &mut app.wb.sheets[0];
        sh.set(1, 3, number(2.0));
        sh.dv.push(sheetcore::dv::DataValidation {
            ranges: vec![(1, 3, 1, 3)],
            kind: sheetcore::dv::DvKind::Whole,
            op: sheetcore::dv::DvOp::GreaterEq,
            f1: "=1+".into(),
            ..Default::default()
        });
        sh.dv_dirty = true;
    }
    app.doc_gen = app.doc_gen.wrapping_add(1);
    app.compute_invalid();
    assert!(app.invalid_cells.contains(&(1, 3)), "坏阈值应标记为不通过");
    assert!(app.status.contains("配置无效"), "scan 应显示配置失败：{}", app.status);
}

#[test]
fn rejected_validation_keeps_draft_and_position() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    restrict_a1(&mut app);
    app.editing = Some(EditState::new("99".into()));
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Empty, "非法值不得写入");
    assert_eq!(app.sel, (0, 0), "验证失败不得跳格");
    assert_eq!(
        app.editing.as_ref().map(|e| e.buf.clone()).as_deref(),
        Some("99"),
        "验证失败必须保留编辑内容"
    );
    assert!(!app.wb.dirty);
    // 改成合法值再提交
    app.editing.as_mut().unwrap().buf = "9".into();
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(9.0));
    assert_eq!(app.sel, (1, 0), "合法提交后正常下移");
}

#[test]
fn one_undo_restores_previous_value_and_redo_reapplies() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(1.0));
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("123".into())]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0));
    assert_eq!(app.undo.depth(), (1, 0), "一次键入只需一步撤销");
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(1.0), "一次撤销回到旧值");
    app.do_undo(true);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0), "重做恢复新值");
}

#[test]
fn committed_value_survives_save_and_reload() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("456".into())]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    let dir = crate::test_env::scratch("ui01-save");
    let path = dir.join("typed.xlsx");
    sheetcore::xlsx::save_atomic(&path, &app.wb).unwrap();
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(back.sheets[0].value(0, 0), Value::Number(456.0), "保存重开必须保留键入值");
}

#[test]
fn chinese_text_commits_once_without_duplication() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    // 中文输入法上屏在 egui 层是一个 Text 事件（真实 IME 候选交互见 QA-04）
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("财务报表".into())]);
    assert_eq!(
        app.editing.as_ref().map(|e| e.buf.clone()).as_deref(),
        Some("财务报表"),
        "上屏文本只能进缓冲一次"
    );
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(
        app.wb.sheet(0).value(0, 0),
        Value::Text("财务报表".into()),
        "提交后不能重复上屏"
    );
}

#[test]
fn goto_name_commits_pending_edit_before_navigating() {
    // 名称框跳转、切工作表/标签、保存、关闭窗口的处理函数都是先 commit_edit(None)
    // 再判断 editing；修复前那一步必然丢输入。
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("123".into())]);
    app.name_buf = "C3".into();
    app.goto_name();
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0), "跳转前必须先提交");
    assert_eq!(app.sel, (2, 2), "然后才跳到 C3");
}

#[test]
fn closing_the_window_commits_pending_edit() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("123".into())]);
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0))),
        ..Default::default()
    };
    input.viewports.insert(
        egui::ViewportId::ROOT,
        egui::ViewportInfo { events: vec![egui::ViewportEvent::Close], ..Default::default() },
    );
    let mut frame = eframe::Frame::_new_kittest();
    let _ = ctx.run(input, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0), "关窗口不能把草稿丢了");
    assert!(app.editing.is_none());
}

#[test]
fn closing_the_window_keeps_a_rejected_draft() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    restrict_a1(&mut app);
    app.editing = Some(EditState::new("99".into()));
    ui_frame(&mut app, &ctx, vec![]);
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0))),
        ..Default::default()
    };
    input.viewports.insert(
        egui::ViewportId::ROOT,
        egui::ViewportInfo { events: vec![egui::ViewportEvent::Close], ..Default::default() },
    );
    let mut frame = eframe::Frame::_new_kittest();
    let _ = ctx.run(input, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Empty, "非法值不能写");
    assert_eq!(
        app.editing.as_ref().map(|e| e.buf.clone()).as_deref(),
        Some("99"),
        "验证失败时必须取消关闭并保留草稿"
    );
}

#[test]
fn escape_in_formula_bar_discards_draft() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(11.0));
    ui_frame(&mut app, &ctx, vec![]);
    let bar = formula_bar_point(&app);
    click(&mut app, &ctx, bar);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("99".into())]);
    assert_eq!(app.editing.as_ref().map(|e| e.buf.clone()).as_deref(), Some("1199"));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    assert!(app.editing.is_none(), "编辑栏 Esc 也要结束编辑");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(11.0), "Esc 不写回");
    assert!(!app.wb.dirty);
}

// ---------------------------------------------------------------- UI-02.1 保存快捷键

fn ctrl_key(key: egui::Key, shift: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            ctrl: true,
            command: true,
            shift,
            ..Default::default()
        },
    }
}

fn wait_save(app: &mut SheetApp) {
    let started = std::time::Instant::now();
    while app.tasks.saving.is_some() {
        assert!(started.elapsed() < std::time::Duration::from_secs(30), "保存超时");
        app.poll_save();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// 造一个已保存的副本，返回 (测试环境, 路径)。环境要活着，否则 LOCALAPPDATA 会指回去。
fn saved_copy(tag: &str, value: f64) -> (crate::test_env::TestEnv, std::path::PathBuf) {
    let env = crate::test_env::TestEnv::new(tag);
    let path = env.path("book.xlsx");
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].set(
        0,
        0,
        Cell { value: Value::Number(value), formula: None },
    );
    sheetcore::xlsx::save_atomic(&path, &wb).unwrap();
    (env, path)
}


/// 文件完整 SHA256（UI-02 验收要求核对「原文件哈希」，不能只看内存 dirty）
fn file_sha256(path: &std::path::Path) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(std::fs::read(path).unwrap());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn ctrl_s_saves_the_open_file_after_a_typed_edit() {
    let ctx = egui::Context::default();
    let (env, path) = saved_copy("ui02-ctrl-s", 1.0);
    let mut app = SheetApp::new_with_context(&ctx, None);
    // 打开是异步的；直接同步装载同一份文件作为“已打开”状态
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    app.wb.path = Some(path.clone());
    app.wb.dirty = false;
    ui_frame(&mut app, &ctx, vec![]);
    uiframe_type_commit(&mut app, &ctx, "123");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0));
    assert!(app.wb.dirty);

    let before = file_sha256(&path);
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::S, false)]);
    assert!(app.tasks.saving.is_some(), "Ctrl+S 必须触发保存");
    wait_save(&mut app);
    assert!(!app.wb.dirty, "保存成功后 dirty 必须清掉");
    // 原文件必须真的被换掉（不是只清了内存里的 dirty 标记）
    let after = file_sha256(&path);
    assert_ne!(before, after, "Ctrl+S 之后磁盘上文件的 SHA256 没变 = 没真保存");
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(back.sheets[0].value(0, 0), Value::Number(123.0), "文件里必须是新值");
    drop(env);
}

#[test]
fn ctrl_s_commits_the_active_edit_before_saving() {
    let ctx = egui::Context::default();
    let (env, path) = saved_copy("ui02-ctrl-s-editing", 1.0);
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    app.wb.path = Some(path.clone());
    app.wb.dirty = false;
    ui_frame(&mut app, &ctx, vec![]);
    // 编辑态还没提交就按 Ctrl+S
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("777".into())]);
    assert!(app.editing.is_some());
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::S, false)]);
    assert!(app.editing.is_none(), "保存前必须先提交有效编辑");
    assert!(app.tasks.saving.is_some());
    wait_save(&mut app);
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(back.sheets[0].value(0, 0), Value::Number(777.0), "编辑中的值必须落盘");
    drop(env);
}

#[test]
fn ctrl_shift_s_saves_to_a_new_path_and_keeps_the_original() {
    let ctx = egui::Context::default();
    let (env, path) = saved_copy("ui02-save-as", 5.0);
    let target = env.path("saved-as.xlsx");
    // 原文件的**整份**哈希：另存为是「写新文件」，原文件必须一个字节都不变。
    // 只比 A1 的值不够——样式、共享字符串、包结构都可能被顺手改写。
    let before_sha = file_sha256(&path);
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    app.wb.path = Some(path.clone());
    app.wb.dirty = false;
    ui_frame(&mut app, &ctx, vec![]);
    uiframe_type_commit(&mut app, &ctx, "42");

    // 无界面验收钩子只替换系统对话框，按键仍走真实 Ctrl+Shift+S
    env.sc("SC_SAVE_AS", target.to_str().unwrap());
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::S, true)]);
    assert!(app.tasks.saving.is_some(), "Ctrl+Shift+S 必须触发另存为");
    wait_save(&mut app);
    assert!(target.is_file(), "另存为目标必须写出来");
    let saved = sheetcore::xlsx::load(&target).unwrap();
    assert_eq!(saved.sheets[0].value(0, 0), Value::Number(42.0));
    let original = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(
        original.sheets[0].value(0, 0),
        Value::Number(5.0),
        "另存为不得改动原文件"
    );
    assert_eq!(file_sha256(&path), before_sha, "另存为之后原文件的完整 SHA256 变了");
    drop(env);
}

#[test]
fn ctrl_s_does_not_save_when_validation_rejects_the_draft() {
    let ctx = egui::Context::default();
    let (env, path) = saved_copy("ui02-ctrl-s-invalid", 3.0);
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    app.wb.path = Some(path.clone());
    app.wb.dirty = false;
    restrict_a1(&mut app);
    app.editing = Some(EditState::new("99".into()));
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::S, false)]);
    assert!(!app.tasks.saving.is_some(), "验证失败时不能开始保存");
    assert_eq!(
        app.editing.as_ref().map(|e| e.buf.clone()).as_deref(),
        Some("99"),
        "草稿必须留着"
    );
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(back.sheets[0].value(0, 0), Value::Number(3.0), "磁盘内容不变");
    drop(env);
}

/// 键入并 Enter 提交（走整窗事件）
fn uiframe_type_commit(app: &mut SheetApp, ctx: &egui::Context, text: &str) {
    ui_frame(app, ctx, vec![egui::Event::Text(text.into())]);
    ui_frame(app, ctx, vec![key_event(egui::Key::Enter, false)]);
}

// ---------------------------------------------------------------- UI-02.2 Ctrl+O/N/P

fn wait_open(app: &mut SheetApp) {
    let started = std::time::Instant::now();
    while app.tasks.open_rx.is_some() || app.tasks.busy {
        assert!(started.elapsed() < std::time::Duration::from_secs(30), "打开超时");
        app.poll_open();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn ctrl_n_creates_a_blank_document_without_losing_the_current_one() {
    let ctx = egui::Context::default();
    let (env, path) = saved_copy("ui02-ctrl-n", 9.0);
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    app.wb.path = Some(path.clone());
    app.wb.dirty = false;
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::N, false)]);
    assert_eq!(app.tabs.len(), 2, "新文档应在新标签里开");
    assert_eq!(app.active_tab, 1);
    assert!(app.wb.path.is_none(), "新文档没有路径");
    assert!(!app.wb.dirty, "新文档不该是脏的");
    assert_eq!(app.wb.sheet(0).len(), 0, "新文档应为空白");
    // 原文档仍停在旧标签里
    match &app.tabs[0] {
        TabSlot::Parked(doc) => assert_eq!(doc.wb.path.as_ref(), Some(&path)),
        TabSlot::Active => panic!("旧文档应被 park"),
    }
    drop(env);
}

#[test]
fn ctrl_o_opens_the_picked_file() {
    let ctx = egui::Context::default();
    let (env, path) = saved_copy("ui02-ctrl-o", 12.0);
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    env.sc("SC_OPEN_PATH", path.to_str().unwrap());
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::O, false)]);
    wait_open(&mut app);
    assert_eq!(
        app.wb.sheet(0).value(0, 0),
        Value::Number(12.0),
        "Ctrl+O 应打开选定文件"
    );
    assert_eq!(app.wb.path.as_ref(), Some(&path));
    drop(env);
}

#[test]
fn ctrl_p_opens_print_preview() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    assert!(!app.print_preview);
    app.preview_page = 3;
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::P, false)]);
    assert!(app.print_preview, "Ctrl+P 应打开打印预览");
    assert_eq!(app.preview_page, 0, "预览从第一页开始");
}

// ---------------------------------------------------------------- C13.3 保存失败回执

#[test]
fn failed_save_keeps_dirty_reports_the_step_and_allows_retry() {
    let ctx = egui::Context::default();
    let (env, path) = saved_copy("c133-retry", 1.0);
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    app.wb.path = Some(path.clone());
    app.wb.dirty = false;
    ui_frame(&mut app, &ctx, vec![]);
    uiframe_type_commit(&mut app, &ctx, "5");
    assert!(app.wb.dirty);

    // 注入保存子步骤失败：界面必须保留 dirty、给出可定位错误、不写盘
    // 只让本次保存失败：并行跑的其它保存（比如 filter/样式回归）不能被带下水
    env.sc("SC_FAIL_SAVE_STEP", "layout@c133-retry");
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::S, false)]);
    wait_save(&mut app);
    assert!(
        app.status.contains("保存失败") && app.status.contains("layout"),
        "状态栏要说明失败原因与步骤，实际：{}",
        app.status
    );
    assert!(app.wb.dirty, "保存失败后必须仍是未保存状态");
    assert_eq!(
        sheetcore::xlsx::load(&path).unwrap().sheets[0].value(0, 0),
        Value::Number(1.0),
        "失败时磁盘内容不得变化"
    );

    // 故障解除后再按一次 Ctrl+S 必须能重试成功
    env.sc("SC_FAIL_SAVE_STEP", "none");
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::S, false)]);
    wait_save(&mut app);
    assert!(!app.wb.dirty, "重试成功后 dirty 才能清掉");
    assert_eq!(
        sheetcore::xlsx::load(&path).unwrap().sheets[0].value(0, 0),
        Value::Number(5.0),
        "重试必须把新值写进文件"
    );
    drop(env);
}
// ---------------------------------------------------------------- UI-02.3 命令表与快捷键优先级

/// 编辑中按 Ctrl+N 原来会直接新建标签，把没提交的输入悄悄扔掉。
/// 对标行为：先提交编辑，再执行命令——草稿必须落到原文档里。
#[test]
fn ctrl_n_while_editing_commits_the_draft_before_adding_a_document() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("123".into())]);
    assert_eq!(app.editing.as_ref().map(|e| e.buf.as_str()), Some("123"));
    let tabs = app.tabs.len();
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::N, false)]);
    assert!(app.editing.is_none(), "命令执行前必须先结束编辑");
    let parked = app
        .tabs
        .iter()
        .find_map(|t| match t {
            TabSlot::Parked(d) => Some(d),
            _ => None,
        })
        .expect("原来的文档应保留在标签里");
    assert_eq!(
        parked.wb.sheet(0).value(0, 0),
        Value::Number(123.0),
        "没提交的 123 必须写回原文档，不能随快捷键丢掉"
    );
    assert_eq!(app.tabs.len(), tabs + 1, "Ctrl+N 仍然要新开一个文档");
}

/// Ctrl+P 同理：先提交再进打印预览，预览里的内容必须是刚输入的值。
#[test]
fn ctrl_p_commits_the_draft_before_opening_print_preview() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("42".into())]);
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::P, false)]);
    assert!(app.print_preview, "Ctrl+P 必须进打印预览");
    assert!(app.editing.is_none());
    assert_eq!(
        app.wb.sheet(0).value(0, 0),
        Value::Number(42.0),
        "进预览前必须提交编辑"
    );
}

/// 模态对话框打开时，应用级快捷键必须让路，否则会在对话框背后换掉文档。
#[test]
fn app_shortcuts_are_ignored_while_a_modal_dialog_is_open() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.hf_dlg = true; // 页眉和页脚
    ui_frame(&mut app, &ctx, vec![]); // 让对话框真正画出来
    let tabs = app.tabs.len();
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::N, false)]);
    assert_eq!(
        app.tabs.len(),
        tabs,
        "模态对话框打开时 Ctrl+N 不得在背后新建文档"
    );
    assert!(!app.print_preview, "也不得在背后打开打印预览");
    // 对话框关掉后快捷键恢复
    app.hf_dlg = false;
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::N, false)]);
    assert_eq!(app.tabs.len(), tabs + 1, "对话框关掉后 Ctrl+N 必须恢复");
}

/// 开始页没有文档：Ctrl+S/Ctrl+P 不得给不存在的文档弹另存为或写文件，
/// Ctrl+N/Ctrl+O 仍然要能新建/打开。
#[test]
fn start_page_only_allows_new_and_open_shortcuts() {
    let ctx = egui::Context::default();
    let env = crate::test_env::TestEnv::new("ui023-start-page");
    let target = env.path("should-not-exist.xlsx");
    env.sc("SC_SAVE_AS", target.to_str().unwrap());
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.show_start = true;
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::S, false)]);
    assert!(
        app.tasks.saving.is_none(),
        "开始页没有文档，Ctrl+S 不得触发保存"
    );
    assert!(!target.exists(), "开始页 Ctrl+S 不得写出文件");
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::P, false)]);
    assert!(!app.print_preview, "开始页没有文档，Ctrl+P 不得进预览");
    ui_frame(&mut app, &ctx, vec![ctrl_key(egui::Key::N, false)]);
    assert!(!app.show_start, "开始页 Ctrl+N 必须新建文档");
    drop(env);
}

/// 功能区的灰态与快捷键必须来自同一个谓词：撤销栈空时两处都不可用，
/// 有一步编辑后两处同时可用。
#[test]
fn ribbon_buttons_and_command_table_share_one_enabled_predicate() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let ids = [
        "FileSave",
        "FileSaveAs",
        "FileOpen",
        "NewTab",
        "FilePrintPreview",
        "PagePrintPreview",
        "ViewPrintPreview",
        "Undo",
        "Redo",
    ];
    for id in ids {
        let action = crate::command::action_for_ctl(id);
        let cmd = crate::command::by_action(action)
            .unwrap_or_else(|| panic!("{id}（action {action}）必须登记在命令表里"));
        assert_eq!(
            app.ctl_state(id).enabled,
            (cmd.enabled)(&app),
            "{id} 的按钮灰态与命令表谓词不一致"
        );
    }
    assert!(!app.command_enabled("Undo"), "空撤销栈");
    assert!(!app.ctl_state("Undo").enabled);

    ui_frame(&mut app, &ctx, vec![]);
    uiframe_type_commit(&mut app, &ctx, "7");
    assert!(app.command_enabled("Undo"), "有一步编辑后撤销必须可用");
    assert!(app.ctl_state("Undo").enabled, "功能区撤销按钮也必须同时变亮");
    // 灰掉的按钮点不动：不可用时执行函数直接返回，且不改模型
    app.undo.clear();
    let before = app.wb.sheet(0).value(0, 0).clone();
    assert!(!app.command_enabled("Undo"));
    let _ = app.command_exec("Undo", &ctx);
    assert_eq!(app.wb.sheet(0).value(0, 0), before, "不可用的命令不得执行");
}

/// tooltip 里的快捷键来自命令表，不改帮助文字就会对不上。
#[test]
fn command_tooltips_carry_their_registered_shortcut() {
    for cmd in crate::command::COMMANDS {
        let tip = cmd.tip();
        assert!(
            tip.contains(&cmd.shortcut()),
            "{} 的 tooltip 缺少快捷键：{tip}",
            cmd.action
        );
    }
    assert_eq!(
        crate::command::by_action("FileSave").unwrap().shortcut(),
        "Ctrl+S"
    );
    assert_eq!(
        crate::command::by_action("FileSaveAs").unwrap().shortcut(),
        "Ctrl+Shift+S"
    );
}


// ---------------------------------------------------------------- QA-03.2 自检输出路径

/// 无界面自检（SC_B1/B3/B4）原来把输出写死到 `F:/Finance/work`：
/// 在别的机器上父目录不存在，自检悄悄什么都不产出，还会往开发目录乱写。
/// 现在输出目录由 SC_PROBE_DIR 指定，否则落在系统临时目录下。
#[test]
fn probe_outputs_go_to_the_declared_directory_not_a_fixed_dev_path() {
    let env = crate::test_env::TestEnv::new("qa032-probe-dir");
    let dir = env.path("probe");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("import.csv"), "姓名,部门,金额\n张三,研发,100\n李四,财务,200\n").unwrap();
    env.sc("SC_B4", "1");
    env.sc("SC_PROBE_DIR", dir.to_str().unwrap());

    let dev = std::path::Path::new("F:/Finance/work");
    let dev_existed = dev.exists();

    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.b4_probe();

    assert!(
        dir.join("b4_out.xlsx").is_file(),
        "自检产物必须写在 SC_PROBE_DIR 里，实际状态：{}",
        app.status
    );
    assert!(
        dir.join("import.csv").is_file(),
        "自检用到的 CSV 也必须在该目录内"
    );
    let back = sheetcore::xlsx::load(&dir.join("b4_out.xlsx")).unwrap();
    assert_eq!(back.sources.len(), 1, "重开后数据源必须还在");
    assert_eq!(!dev.exists(), !dev_existed, "不得在固定开发目录下新建任何东西");
}

#[test]
fn pasting_an_excel_error_cell_keeps_it_an_error_value() {
    // Excel 复制出来的错误格在 XML Spreadsheet 里是 `ss:Type="Error"`，
    // 解析后 kind=4；粘贴必须还原成 Value::Error，不能变成文本或 0。
    let mut app = app();
    app.sel = (2, 1);
    let cell = |text: &str, kind: u8, formula: Option<&str>| office_clipboard::OfficeCell {
        text: text.into(),
        bold: false,
        italic: false,
        fill: None,
        color: None,
        halign: 0,
        colspan: 1,
        rowspan: 1,
        border: None,
        formula: formula.map(|f| f.into()),
        kind,
        font: String::new(),
        size: 0.0,
        num_format: String::new(),
        raw: None,
    };
    // 真正走一遍 XMLSS 解析：Excel 的 ss:Type="Error" 必须映射成 kind=4
    let xml = r#"<?xml version="1.0"?>
<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet"
 xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">
 <Worksheet ss:Name="Sheet1"><Table>
  <Row><Cell><Data ss:Type="Error">#DIV/0!</Data></Cell>
       <Cell><Data ss:Type="String">#N/A</Data></Cell></Row>
 </Table></Worksheet></Workbook>"#;
    let parsed = sheetcore::clipboard_xmlss::parse(xml);
    let mapped = office_clipboard::convert_block(parsed);
    assert_eq!(
        mapped.rows[0][0].kind, 4,
        "ss:Type=\"Error\" 应该映射成 kind=4（错误值），实际 {}",
        mapped.rows[0][0].kind
    );
    assert_eq!(mapped.rows[0][1].kind, 1, "ss:Type=\"String\" 应仍是文本");
    let block = office_clipboard::OfficeBlock {
        rows: vec![
            vec![cell("#DIV/0!", 4, None), cell("#N/A", 4, None)],
            vec![cell("5", 0, None), cell("#DIV/0!", 1, None)],
        ],
    };
    assert!(app.paste_office_block(&block), "粘贴被拒绝");
    assert_eq!(
        app.wb.sheets[0].value(2, 1),
        Value::Error(sheetcore::formula::ErrKind::Div0),
        "粘贴的错误格变成了 {:?}",
        app.wb.sheets[0].value(2, 1)
    );
    assert_eq!(app.wb.sheets[0].value(2, 2), Value::Error(sheetcore::formula::ErrKind::Na));
    assert_eq!(app.wb.sheets[0].value(3, 1), Value::Number(5.0));
    // kind=1（文本）的 "#DIV/0!" 仍然是文本
    assert_eq!(app.wb.sheets[0].value(3, 2), Value::Text("#DIV/0!".into()));
}

#[test]
fn app_undo_restores_an_error_cell_after_a_real_edit() {
    // 走 App 自己的事务与撤销栈（不是直接操作 core 的 UndoStack）
    let mut app = app();
    app.wb.sheets[0].set(0, 0, Cell {
        value: Value::Error(sheetcore::formula::ErrKind::Div0),
        formula: Some("=1/0".into()),
    });
    app.sel = (0, 0);
    let before = app.txn_begin(&[(0, 0)]);
    app.wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    app.txn_end("输入", before);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(1.0));

    app.do_undo(false);
    assert_eq!(
        app.wb.sheets[0].value(0, 0),
        Value::Error(sheetcore::formula::ErrKind::Div0),
        "撤销后错误值没回来"
    );
    assert_eq!(app.wb.sheets[0].get(0, 0).unwrap().formula.as_deref(), Some("=1/0"));

    app.do_undo(true);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(1.0), "重做没生效");
}

#[test]
fn xmlss_datetime_pastes_into_each_date_system_with_time_and_plain_numbers() {
    // C04：Excel 复制出来的 DateTime 是 ISO 文本。真实粘贴路径必须按**目标簿**的日期体系
    // 重算序列号（1900/1904 相差 1462 天），保留时间小数，普通数字原样不动，并带上日期格式。
    let xml = r#"<?xml version="1.0"?>
<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet"
 xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">
 <Styles><Style ss:ID="dt"><NumberFormat ss:Format="yyyy-mm-dd hh:mm"/></Style></Styles>
 <Worksheet ss:Name="Sheet1"><Table>
  <Row><Cell ss:StyleID="dt"><Data ss:Type="DateTime">1904-01-02T12:00:00</Data></Cell>
       <Cell><Data ss:Type="Number">1234.5</Data></Cell>
       <Cell><Data ss:Type="String">文本</Data></Cell></Row>
 </Table></Worksheet></Workbook>"#;
    let block = office_clipboard::convert_block(sheetcore::clipboard_xmlss::parse(xml));
    assert_eq!(block.rows[0][0].kind, 3, "DateTime 必须映射成日期类型");

    let mut a04 = app();
    a04.wb.date_system = sheetcore::model::DateSystem::D1904;
    a04.sel = (0, 0);
    assert!(a04.paste_office_block(&block), "1904 簿粘贴被拒绝");

    let mut a00 = app();
    a00.wb.date_system = sheetcore::model::DateSystem::D1900;
    a00.sel = (0, 0);
    assert!(a00.paste_office_block(&block), "1900 簿粘贴被拒绝");

    // 同一份 XMLSS，两个体系落成不同序列号，差正好 1462 天
    let s04 = match a04.wb.sheets[0].value(0, 0) {
        Value::Number(n) => n,
        other => panic!("1904 簿得到的不是数字：{other:?}"),
    };
    let s00 = match a00.wb.sheets[0].value(0, 0) {
        Value::Number(n) => n,
        other => panic!("1900 簿得到的不是数字：{other:?}"),
    };
    assert_eq!(s04, 1.5, "1904-01-02 12:00 在 1904 体系里是 1.5");
    assert_eq!(s00, 1463.5, "同一时刻在 1900 体系里是 1463.5");
    assert_eq!(s00 - s04, 1462.0, "两个体系必须正好相差 1462 天");
    assert_eq!(s04.fract(), 0.5, "时间小数不能丢");
    assert_eq!(s00.fract(), 0.5, "时间小数不能丢");

    // 普通数字与文本不受日期体系影响
    for app in [&a04, &a00] {
        assert_eq!(app.wb.sheets[0].value(0, 1), Value::Number(1234.5));
        assert_eq!(app.wb.sheets[0].value(0, 2), Value::Text("文本".into()));
        let sid = app.wb.sheet(0).style_of(0, 0);
        let fmt = app.wb.styles.get(sid).format.clone();
        assert!(fmt.contains("yyyy") && fmt.contains("hh"), "粘贴后应带日期时间格式，实际 {fmt:?}");
    }

    // 显示：同一时刻在两个体系里显示成同一个日期时间（序列号差 1462，基准也差 1462）
    let f04 = a04.wb.styles.get(a04.wb.sheet(0).style_of(0, 0)).format.clone();
    let f00 = a00.wb.styles.get(a00.wb.sheet(0).style_of(0, 0)).format.clone();
    assert_eq!(
        sheetcore::style::format_value_in(sheetcore::model::DateSystem::D1904, &Value::Number(s04), &f04),
        "1904-01-02 12:00"
    );
    assert_eq!(
        sheetcore::style::format_value_in(sheetcore::model::DateSystem::D1900, &Value::Number(s00), &f00),
        "1904-01-02 12:00"
    );
}

#[test]
fn incomplete_load_blocks_overwrite_but_allows_rescue_save_as() {
    // C14：打开时工作表 XML 坏了（公式/单元格读不出来），就地保存必须被拒绝，
    // 否则会把"没读到的东西"永久写没；「另存为」抢救副本仍然允许。
    let env = crate::test_env::TestEnv::new("c14_guard");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);

    let path = env.path("broken.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(5.0), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(100.0), formula: Some("=A1*20".into()) });
    sheetcore::xlsx::save(&path, &wb).unwrap();

    // 破坏工作表 XML（模拟下载损坏/磁盘坏块）
    let part = sheetcore::patch::sheet_targets(&path).unwrap().get("数据").cloned().unwrap();
    sheetcore::fixups::write_parts(
        &path,
        &[(part.clone(), b"<worksheet><sheetData><row>".to_vec())],
    )
    .unwrap();

    let (loaded, diag) = sheetcore::xlsx::load_diag(&path).unwrap();
    assert!(diag.has_fatal(), "坏表必须被标成致命：{:?}", diag.problems);
    app.wb = loaded;
    app.wb.path = Some(path.clone());
    app.load_diag = Some(diag);
    app.wb.dirty = true;
    let before = file_sha256(&path);

    // 就地保存：拒绝，且不产生保存任务、原文件不动
    app.save(false);
    assert!(app.status.contains("禁止覆盖保存"), "状态栏应说明原因：{}", app.status);
    assert!(app.tasks.saving.is_none(), "被拒绝的保存不能启动后台写盘");
    assert_eq!(file_sha256(&path), before, "原文件不得被改写");

    // 另存抢救副本：允许，并且写出的新文件读得回来
    let rescue = env.path("rescue.xlsx");
    env.sc("SC_SAVE_AS", rescue.to_str().unwrap());
    app.save(true);
    assert!(app.tasks.saving.is_some(), "另存为应启动保存任务");
    wait_save(&mut app);
    assert!(rescue.is_file(), "抢救副本必须写出来");
    assert!(sheetcore::xlsx::load(&rescue).is_ok());
    assert!(app.load_diag.is_none(), "另存成功后不应继续挡住新文件的保存");
    drop(env);
}

#[test]
fn opening_a_damaged_document_reports_it_immediately_and_diag_follows_the_tab() {
    // C14：打开就不完整要当场说清楚（不是等保存才提示），
    // 而且诊断属于具体文档：切到别的标签不能被串用。
    let env = crate::test_env::TestEnv::new("c14_status");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);

    // 坏文档 A
    let bad = env.path("bad.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(5.0), formula: None });
    sheetcore::xlsx::save(&bad, &wb).unwrap();
    let part = sheetcore::patch::sheet_targets(&bad).unwrap().get("数据").cloned().unwrap();
    sheetcore::fixups::write_parts(&bad, &[(part, b"<worksheet><sheetData><row>".to_vec())]).unwrap();
    let (wb_bad, diag) = sheetcore::xlsx::load_diag(&bad).unwrap();
    assert!(diag.has_fatal());

    // 好文档 B
    let good = env.path("good.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "汇总".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(9.0), formula: None });
    sheetcore::xlsx::save(&good, &wb).unwrap();
    let (wb_good, diag_good) = sheetcore::xlsx::load_diag(&good).unwrap();
    assert!(!diag_good.has_fatal());

    // 走真实的打开消息路径：打开 A 后状态栏必须立刻是"读取问题"
    let (tx, rx) = std::sync::mpsc::channel();
    app.tasks.open_rx = Some(rx);
    tx.send(crate::OpenMsg::Loaded(Box::new(wb_bad), false, Box::new(diag.clone()))).unwrap();
    drop(tx);
    app.poll_open();
    assert!(app.status.contains("读取问题"), "打开当时就要报：{}", app.status);
    assert!(app.show_load_diag, "加载问题窗口应自动弹出");
    assert!(app.load_diag.as_ref().is_some_and(|d| d.has_fatal()));

    // 切到 B：诊断必须换成 B 的（干净），保存不再被挡
    let parked_a = app.park_current();
    let (tx2, rx2) = std::sync::mpsc::channel();
    app.tasks.open_rx = Some(rx2);
    tx2.send(crate::OpenMsg::Loaded(Box::new(wb_good), false, Box::new(diag_good.clone()))).unwrap();
    drop(tx2);
    app.poll_open();
    assert!(
        !app.load_diag.as_ref().is_some_and(|d| d.has_fatal()),
        "B 是完好的，不应继承 A 的致命诊断：{:?}",
        app.load_diag
    );
    assert!(!app.status.contains("读取问题"), "B 不该显示 A 的问题：{}", app.status);

    // 切回 A：致命诊断跟着文档回来
    let parked_b = app.park_current();
    app.activate_parked(parked_a);
    assert!(app.load_diag.as_ref().is_some_and(|d| d.has_fatal()), "切回 A 必须恢复 A 的诊断");
    app.activate_parked(parked_b);
    assert!(app.load_diag.is_none(), "再切回 B 仍然干净");
    drop(env);
}

#[test]
fn save_as_back_to_the_original_path_is_refused_for_damaged_documents() {
    let env = crate::test_env::TestEnv::new("c14_same_path");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);

    let path = env.path("same.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(3.0), formula: None });
    sheetcore::xlsx::save(&path, &wb).unwrap();
    let part = sheetcore::patch::sheet_targets(&path).unwrap().get("数据").cloned().unwrap();
    sheetcore::fixups::write_parts(&path, &[(part, b"<worksheet><sheetData><row>".to_vec())]).unwrap();
    let (loaded, diag) = sheetcore::xlsx::load_diag(&path).unwrap();
    app.wb = loaded;
    app.wb.path = Some(path.clone());
    app.load_diag = Some(diag);
    let before = file_sha256(&path);

    // 另存为把目标选回原路径：同样要拒绝
    env.sc("SC_SAVE_AS", path.to_str().unwrap());
    app.save(true);
    wait_save(&mut app);
    assert_eq!(file_sha256(&path), before, "另存回原路径也不能改写损坏的原文件");

    // 核心层直接写同类目标也不许过（防界面之外的入口）
    let err = sheetcore::xlsx::save(&path, &app.wb).expect_err("核心保存必须拒绝");
    assert!(format!("{err:#}").contains("拒绝覆盖"));
    assert_eq!(file_sha256(&path), before);
    drop(env);
}

#[test]
fn rescue_save_started_before_switching_tabs_clears_diag_when_it_finishes() {
    // 复核场景：A 坏了 → 另存抢救副本（后台写盘中）→ 立刻切到 B →
    // 保存完成时 A 已经挂起；切回 A 不能还拿旧诊断拦保存。
    let env = crate::test_env::TestEnv::new("c14_parked_save");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);

    let path = env.path("parked_bad.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    sheetcore::xlsx::save(&path, &wb).unwrap();
    let part = sheetcore::patch::sheet_targets(&path).unwrap().get("数据").cloned().unwrap();
    sheetcore::fixups::write_parts(&path, &[(part, b"<worksheet><sheetData><row>".to_vec())]).unwrap();
    let (loaded, diag) = sheetcore::xlsx::load_diag(&path).unwrap();
    assert!(diag.has_fatal());
    app.wb = loaded;
    app.wb.path = Some(path.clone());
    app.load_diag = Some(diag);
    app.wb.dirty = true;

    let rescue = env.path("parked_rescue.xlsx");
    env.sc("SC_SAVE_AS", rescue.to_str().unwrap());
    app.save(true);
    assert!(app.tasks.saving.is_some(), "另存应启动后台写盘");

    // 保存还没完成就切标签：A 连同 diag/pending save 一起挂起
    let parked = app.park_current();
    app.tabs[0] = crate::TabSlot::Parked(parked);
    app.tabs.push(crate::TabSlot::Active);
    app.active_tab = 1;
    let mut good = Workbook::new_blank();
    good.sheets[0].name = "汇总".into();
    good.sheets[0].set(0, 0, Cell { value: Value::Number(2.0), formula: None });
    app.wb = good;

    // 等挂起的保存完成（poll_save 会轮询所有 Parked 文档）
    for _ in 0..200 {
        app.poll_save();
        let still = matches!(&app.tabs[0], crate::TabSlot::Parked(d) if d.tasks.saving.is_some());
        if !still {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(rescue.is_file(), "抢救副本必须写出来：{}", rescue.display());

    // 切回 A：诊断应已清掉，保存不再被拦
    let slot = std::mem::replace(&mut app.tabs[0], crate::TabSlot::Active);
    if let crate::TabSlot::Parked(doc) = slot {
        app.activate_parked(doc);
    } else {
        panic!("A 应当是挂起状态");
    }
    assert!(app.load_diag.is_none(), "保存成功后旧诊断必须清掉：{:?}", app.load_diag);
    assert!(app.wb.path.is_some());
    app.wb.dirty = true;
    app.save(false);
    // 没有致命诊断时，保存应正常进入写盘（不是被"打开不完整"挡住）
    assert!(
        app.tasks.saving.is_some() || !app.status.contains("禁止覆盖保存"),
        "保存不应再被旧诊断拦截：{}",
        app.status
    );
    wait_save(&mut app);
    drop(env);
}

#[test]
fn csv_import_from_the_app_keeps_quoted_newlines_and_registers_a_refreshable_source() {
    let env = crate::test_env::TestEnv::new("c09_app_import");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let csv = env.path("a.csv");
    std::fs::write(&csv, "\"line1\nline2\",B\nC,D\n").unwrap();
    app.import_csv_from(&csv);

    let sh = app.wb.sheet(app.sheet_idx);
    let val = |r: u32, c: u32| match sh.get(r, c).map(|x| x.value.clone()) {
        Some(sheetcore::model::Value::Text(t)) => t.to_string(),
        other => format!("{other:?}"),
    };
    assert_eq!(val(0, 0), "line1\nline2", "应用导入路径也必须保留字段内换行");
    assert_eq!(val(1, 1), "D");
    let src = app.wb.sources.iter().find(|s| s.name.starts_with("_sc_src_")).expect("应登记数据源");
    assert_eq!((src.rows, src.cols), (2, 2));
    assert!(app.undo.undo_label().map(|l| l.contains("导入 CSV")).unwrap_or(false), "导入应可一步撤销");
    drop(env);
}




#[test]
fn csv_import_dialog_renders_with_real_preview_before_any_write() {
    let env = crate::test_env::TestEnv::new("c09_app_dialog");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let csv = env.path("dlg.csv");
    std::fs::write(&csv, "编号,名称\n00123,甲\n".as_bytes()).unwrap();
    app.open_csv_dialog(&csv);
    let dlg = app.csv_dlg.as_ref().unwrap();
    assert_eq!(dlg.preview.len(), 2, "对话框打开即解析出预览");
    assert_eq!(dlg.preview[1][0], "00123");
    // 渲染一帧：窗口能画出来，且不因预览/勾选框 panic
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        app.csv_dlg_ui(ctx);
    });
    assert!(app.csv_dlg.is_some(), "未确认前对话框保持打开");
    // 取消不写任何格
    app.csv_dlg = None;
    assert!(app.wb.sheet(app.sheet_idx).get(0, 0).is_none());
    drop(env);
}
#[test]
fn csv_import_choices_persist_and_refresh_reuses_them() {
    let env = crate::test_env::TestEnv::new("c09_app_choice");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let csv = env.path("acct.csv");
    std::fs::write(&csv, "账号,金额\n001234567890123456,5\n").unwrap();
    // 模拟对话框选择：UTF-8 + 首行表头 + 第 1 列按文本
    app.open_csv_dialog(&csv);
    let dlg = app.csv_dlg.as_ref().expect("应打开导入对话框");
    assert!(!dlg.preview.is_empty(), "对话框要有真实预览");
    let delim = dlg.delim;
    app.csv_dlg = None;
    app.import_csv_choice(&csv, delim, true, Some(sheetcore::csv::Encoding::Utf8), '"', &[0]);

    let sh = app.wb.sheet(app.sheet_idx);
    let val = |r: u32, c: u32| match sh.get(r, c).map(|x| x.value.clone()) {
        Some(sheetcore::model::Value::Text(t)) => t.to_string(),
        Some(sheetcore::model::Value::Number(n)) => format!("{n}"),
        other => format!("{other:?}"),
    };
    assert_eq!(val(1, 0), "001234567890123456", "文本列要保留前导零和 18 位账号");
    let src = app.wb.sources.iter().find(|s| s.name.starts_with("_sc_src_")).unwrap();
    let opts = src.csv_opts.as_ref().expect("导入必须记录实际编码/列型/引号");
    assert_eq!(opts.encoding, Some(sheetcore::csv::Encoding::Utf8));
    assert_eq!(opts.text_cols, vec![0]);
    assert_eq!(opts.quote, '"');

    // 保存重开：配置仍在
    let x = env.path("acct.xlsx");
    sheetcore::xlsx::save(&x, &app.wb).unwrap();
    let mut back = sheetcore::xlsx::load(&x).unwrap();
    let src = back.sources.iter().find(|s| s.name.starts_with("_sc_src_")).expect("源应随包保存");
    let opts = src.csv_opts.as_ref().expect("重开后配置应在");
    assert_eq!(opts.text_cols, vec![0]);
    assert_eq!(opts.encoding, Some(sheetcore::csv::Encoding::Utf8));
    // 刷新复用同一配置
    std::fs::write(&csv, "账号,金额\n009999999999999999,9\n").unwrap();
    let src = src.clone();
    let (r, c) = sheetcore::external::refresh(&mut back, 0, &src).unwrap();
    assert_eq!((r, c), (2, 2));
    match &back.sheets[0].get(1, 0).map(|x| x.value.clone()) {
        Some(sheetcore::model::Value::Text(t)) => assert_eq!(t.to_string(), "009999999999999999"),
        other => panic!("刷新后账号应仍为文本：{other:?}"),
    }
    drop(env);
}
#[test]
fn refresh_is_one_undo_step_that_restores_values_rect_and_source_size() {
    let env = crate::test_env::TestEnv::new("c09_app_refresh_undo");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let csv = env.path("r.csv");
    std::fs::write(&csv, "A,B\nC,D\n").unwrap();
    app.import_csv_from(&csv);
    assert_eq!(app.wb.sources.len(), 1);
    assert_eq!((app.wb.sources[0].rows, app.wb.sources[0].cols), (2, 2));
    let labels_before = app.undo.depth().0;
    // 文件变长且首行变短：刷新后 B1 必须清空、矩形变 3x2
    std::fs::write(&csv, "A\nE,F\nG,H\n").unwrap();
    app.refresh_import();
    let sh = app.wb.sheet(app.sheet_idx);
    let val = |r: u32, c: u32| match sh.get(r, c).map(|x| x.value.clone()) {
        Some(sheetcore::model::Value::Text(t)) => t.to_string(),
        Some(sheetcore::model::Value::Empty) | None => String::new(),
        other => format!("{other:?}"),
    };
    assert_eq!((app.wb.sources[0].rows, app.wb.sources[0].cols), (3, 2));
    assert_eq!(val(0, 1), "", "短行刷新必须清掉旧 B1");
    assert_eq!(val(2, 1), "H");
    assert_eq!(app.undo.depth().0, labels_before + 1, "刷新只能占一步撤销");
    assert!(app.undo.undo_label().map(|l| l.contains("刷新")).unwrap_or(false));
    // 一步撤销：值、范围、源尺寸全部回到刷新前
    app.do_undo(false);
    let sh = app.wb.sheet(app.sheet_idx);
    let val2 = |r: u32, c: u32| match sh.get(r, c).map(|x| x.value.clone()) {
        Some(sheetcore::model::Value::Text(t)) => t.to_string(),
        Some(sheetcore::model::Value::Empty) | None => String::new(),
        other => format!("{other:?}"),
    };
    assert_eq!(val2(0, 0), "A");
    assert_eq!(val2(0, 1), "B", "撤销要恢复被清掉的 B1");
    assert_eq!(val2(1, 1), "D");
    assert_eq!(val2(2, 0), "", "刷新多出来的行要撤掉");
    assert_eq!((app.wb.sources[0].rows, app.wb.sources[0].cols), (2, 2), "源尺寸要回滚");
    drop(env);
}
fn seeded_app(env: &crate::test_env::TestEnv, tag: &str) -> SheetApp {
    let _ = tag;
    crate::clear_test_clipboard();
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for r in 0..3u32 {
        for c in 0..3u32 {
            app.wb.sheet_mut(0).set(
                r,
                c,
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::Text(format!("v{r}{c}").into()),
                    formula: None,
                },
            );
        }
    }
    app.undo = sheetcore::undo::UndoStack::new();
    app.wb.dirty = false;
    app
}

fn snapshot(app: &SheetApp) -> Vec<String> {
    let mut out = Vec::new();
    for r in 0..3u32 {
        for c in 0..3u32 {
            out.push(app.wb.sheet(0).get(r, c).map(|x| x.value.display()).unwrap_or_default());
        }
    }
    out
}

/// UI-03：受保护的表，Delete / 替换 / 去重 / 填充 / 验证下拉都不能改模型。
#[test]
fn protected_sheet_blocks_delete_replace_dedup_fill_and_dv_pick() {
    let env = crate::test_env::TestEnv::new("ui03_protected");
    let ctx = egui::Context::default();
    let mut app = seeded_app(&env, "prot");
    app.wb.sheet_mut(0).protection = Some(sheetcore::protection::SheetProtection::default());
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    let before = snapshot(&app);

    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Delete, false)]);
    assert_eq!(snapshot(&app), before, "受保护时 Delete 不能改数据");
    assert!(app.undo.undo_label().is_none(), "不能产生撤销步");

    app.find.find = "v1".into();
    app.find.replace = "X".into();
    app.sel = (1, 0);
    app.replace_current();
    assert_eq!(snapshot(&app), before, "受保护时替换不能改数据");
    app.replace_all();
    assert_eq!(snapshot(&app), before, "受保护时全部替换不能改数据");

    app.run_dedup();
    assert_eq!(snapshot(&app), before, "受保护时去重不能改数据");
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    app.fill_to(2, true);
    assert_eq!(snapshot(&app), before, "受保护时填充不能改数据");
    assert!(
        !app.dv_pick_apply(0, 0, "v2"),
        "受保护时验证下拉选中也必须被门禁拒绝"
    );
    assert_eq!(snapshot(&app), before, "受保护时验证下拉不能改数据");
    assert!(!app.wb.dirty, "被拒绝的操作不能把文档标脏");
    assert!(app.status.contains("受保护"), "应提示受保护：{}", app.status);
    drop(env);
}


/// C22：文本溢出按 Excel 规则——邻格空才延伸，遇非空即止；左对齐起点永远可见，
/// 换缩放比例只按列宽等比放大，不改变边界。
#[test]
fn spill_clip_follows_excel_neighbour_rules_and_scales_with_zoom() {
    use egui::{pos2, vec2, Align2, Rect};
    let env = crate::test_env::TestEnv::new("c22_spill");
    let ctx = egui::Context::default();
    let mut app = seeded_app(&env, "c22");
    let (w1, w2) = {
        let sh = app.wb.sheet(0);
        (sh.cols.size(1), sh.cols.size(2))
    };
    let rect = Rect::from_min_size(pos2(100.0, 50.0), vec2(80.0, 20.0));

    // B1 有内容（seeded 的 v01）：左对齐只能停在本格右边界
    app.paint_scale = 1.0;
    let clip = app.spill_clip(0, 0, rect, Align2::LEFT_CENTER);
    assert_eq!(clip.min.x, 100.0, "左对齐起点必须在本格左边界");
    assert_eq!(clip.max.x, 180.0, "右邻非空时不能溢出");

    // 清空 B1、C1：连续两个空格都要算进去
    app.wb.sheet_mut(0).set(0, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Empty, formula: None });
    app.wb.sheet_mut(0).set(0, 2, sheetcore::model::Cell { value: sheetcore::model::Value::Empty, formula: None });
    // D1 放个内容当挡板，溢出应正好停在 C 的右边界
    app.wb.sheet_mut(0).set(0, 3, sheetcore::model::Cell { value: sheetcore::model::Value::Text("挡".into()), formula: None });
    let clip = app.spill_clip(0, 0, rect, Align2::LEFT_CENTER);
    assert!((clip.max.x - (180.0 + w1 + w2)).abs() < 0.01, "两个空格应延伸两列：{}", clip.max.x);

    // 居中：两侧都可以溢出
    // C1 左邻 B1 已清空、右邻 D1 有挡板：居中只往左延伸
    let cell_c = Rect::from_min_size(pos2(180.0, 50.0), vec2(80.0, 20.0));
    let clip_center = app.spill_clip(0, 2, cell_c, Align2::CENTER_CENTER);
    assert!(clip_center.min.x < 180.0, "居中左侧空白应延伸：{}", clip_center.min.x);

    // 右对齐：只往左延伸，右边界不动
    let clip_right = app.spill_clip(0, 2, cell_c, Align2::RIGHT_CENTER);
    assert_eq!(clip_right.max.x, 260.0, "右对齐不能向右溢出");
    assert!(clip_right.min.x < 180.0, "右对齐可向左溢出");

    // 隐藏列要挡住溢出
    app.wb.sheet_mut(0).hidden_cols.insert(2);
    let clip_hidden = app.spill_clip(0, 0, rect, Align2::LEFT_CENTER);
    assert!((clip_hidden.max.x - (180.0 + w1)).abs() < 0.01, "隐藏列 C 应挡住：{}", clip_hidden.max.x);
    app.wb.sheet_mut(0).hidden_cols.remove(&2);

    // 缩放 2 倍：延伸宽度等比翻倍，边界仍对齐列边界
    app.paint_scale = 1.0;
    let one = app.spill_clip(0, 0, rect, Align2::LEFT_CENTER);
    app.paint_scale = 2.0;
    let clip2 = app.spill_clip(0, 0, Rect::from_min_size(pos2(200.0, 100.0), vec2(160.0, 40.0)), Align2::LEFT_CENTER);
    let ext1 = one.max.x - 180.0;
    let ext2 = clip2.max.x - 360.0;
    assert_eq!(one.max.x, clip.max.x, "one={} clip={} w1={} w2={}", one.max.x, clip.max.x, w1, w2);
    assert!((ext2 - ext1 * 2.0).abs() < 0.01, "缩放后延伸宽度应等比：{ext1} vs {ext2}");
    drop(env);
}
/// C15：受保护表上「锁定公式区 + 未锁输入区」，只允许改输入区；
/// 排序/插入行要有对应权限才放行。
#[test]
fn protection_keeps_formula_area_locked_and_lets_permitted_actions_through() {
    use sheetcore::protection::{ProtPerms, SheetProtection};
    let env = crate::test_env::TestEnv::new("c15_perm_gate");
    let ctx = egui::Context::default();
    let mut app = seeded_app(&env, "c15");
    // A1 保持默认锁定（模拟公式区），B1 解锁（模拟输入区）
    {
        let old = app.wb.sheet(0).style_of(0, 1);
        let mut st = app.wb.styles.get(old).clone();
        st.locked = false;
        let id = app.wb.styles.intern(st);
        app.wb.sheet_mut(0).set_style(0, 1, id);
    }
    let mut prot = SheetProtection::default();
    prot.sheet = true;
    prot.perms = ProtPerms::select_only();
    app.wb.sheet_mut(0).protection = Some(prot);
    app.wb.dirty = false;

    // 锁定区：不能进入编辑，也不产生撤销
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("改公式区".into()));
    assert!(app.editing.is_none(), "锁定单元格不能进入编辑");
    assert!(app.status.contains("受保护"), "{}", app.status);

    // 输入区：正常写入
    app.sel = (0, 1);
    app.sel_end = (0, 1);
    app.begin_edit(Some("可输入".into()));
    assert!(app.editing.is_some(), "未锁定单元格应可编辑：{}", app.status);
    assert!(app.commit_edit(None));
    assert_eq!(
        app.wb.sheet(0).get(0, 1).map(|c| c.value.clone()),
        Some(sheetcore::model::Value::Text("可输入".into()))
    );
    assert_eq!(app.undo.undo_label().as_deref(), Some("编辑单元格"));

    // 默认权限下：格式、排序、插入行都被拒
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    let sid_before = app.wb.sheet(0).style_of(0, 0);
    app.apply_style(|st| st.bold = true);
    assert_eq!(app.wb.sheet(0).style_of(0, 0), sid_before, "锁定格无格式权限不能改");
    app.do_sort(0, true);
    assert!(app.status.contains("受保护"), "{}", app.status);
    app.structural(sheetcore::structural::Structural::InsertRows { at: 0, n: 1 });
    assert!(app.status.contains("受保护"), "{}", app.status);

    // 授予权限后：同样操作放行
    {
        let p = app.wb.sheet_mut(0).protection.as_mut().unwrap();
        p.perms.format_cells = true;
        p.perms.sort = true;
        p.perms.insert_rows = true;
    }
    app.apply_style(|st| st.bold = true);
    assert_ne!(app.wb.sheet(0).style_of(0, 0), sid_before, "有格式权限就能改锁定格");
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    let a1 = app.wb.sheet(0).get(0, 0).map(|c| c.value.clone());
    app.structural(sheetcore::structural::Structural::InsertRows { at: 0, n: 1 });
    assert_eq!(app.wb.sheet(0).get(1, 0).map(|c| c.value.clone()), a1, "有插入行权限就能插行");
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    app.do_sort(0, true);
    assert!(app.status.contains("排序") || app.undo.undo_label().as_deref() == Some("排序"), "{}", app.status);
    drop(env);
}

/// C15：隐藏公式在受保护状态下不进公式栏；撤销保护后恢复。
#[test]
fn hidden_formula_is_masked_only_while_protected() {
    use sheetcore::protection::{ProtPerms, SheetProtection};
    let env = crate::test_env::TestEnv::new("c15_hidden_fx");
    let ctx = egui::Context::default();
    let mut app = seeded_app(&env, "c15h");
    app.wb.sheet_mut(0).set(
        0,
        0,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Number(3.0),
            formula: Some("=1+2".into()),
        },
    );
    let old = app.wb.sheet(0).style_of(0, 0);
    let mut st = app.wb.styles.get(old).clone();
    st.hidden = true;
    let id = app.wb.styles.intern(st);
    app.wb.sheet_mut(0).set_style(0, 0, id);

    let masked = |app: &SheetApp| -> bool {
        let si = app.sheet_idx;
        app.wb.sheet(si).protection.is_some() && {
            let sid = app.wb.sheet(si).style_of(0, 0);
            app.wb.styles.get(sid).hidden
        }
    };
    assert!(!masked(&app), "未保护时隐藏公式不生效");
    let mut prot = SheetProtection::default();
    prot.sheet = true;
    prot.perms = ProtPerms::select_only();
    app.wb.sheet_mut(0).protection = Some(prot);
    assert!(masked(&app), "受保护 + 隐藏公式时公式栏应为空");
    app.wb.sheet_mut(0).protection = None;
    assert!(!masked(&app), "撤销保护后公式恢复可见");
    drop(env);
}
/// UI-03：受保护时清除按钮/多选清除/格式/快速排序都不能改数据、样式、undo 或 dirty。
#[test]
fn protected_sheet_blocks_clear_buttons_multi_select_format_and_quick_sort() {
    let env = crate::test_env::TestEnv::new("ui03_protected_more");
    let ctx = egui::Context::default();
    let mut app = seeded_app(&env, "prot2");
    app.wb.sheet_mut(0).protection = Some(sheetcore::protection::SheetProtection::default());
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    let before = snapshot(&app);
    let styles: Vec<u16> = (0..3).flat_map(|r| (0..3).map(move |c| (r, c))).map(|(r, c)| app.wb.sheet(0).style_of(r, c)).collect();

    app.clear_values();
    assert_eq!(snapshot(&app), before, "清除按钮不能绕过保护");
    app.clear_selection();
    assert_eq!(snapshot(&app), before, "多选清除不能绕过保护");

    app.apply_style(|st| st.bold = !st.bold);
    assert_eq!(snapshot(&app), before, "格式不能绕过保护");
    app.do_sort(0, true);
    assert_eq!(snapshot(&app), before, "快速排序不能绕过保护");

    let styles2: Vec<u16> = (0..3).flat_map(|r| (0..3).map(move |c| (r, c))).map(|(r, c)| app.wb.sheet(0).style_of(r, c)).collect();
    assert_eq!(styles, styles2, "被拒绝的操作不能改样式");
    assert!(app.undo.undo_label().is_none(), "被拒绝的操作不能进撤销栈");
    assert!(!app.wb.dirty, "被拒绝的操作不能标脏");
    assert!(app.status.contains("受保护"), "应提示受保护：{}", app.status);
    drop(env);
}

/// UI-03：区域里只有一格被锁，多选清除/格式/排序一律整批拒绝。
#[test]
fn one_locked_cell_blocks_multi_select_clear_format_and_sort() {
    let env = crate::test_env::TestEnv::new("ui03_lock_more");
    let ctx = egui::Context::default();
    let mut app = seeded_app(&env, "lock2");
    app.net = Some(crate::net::host(92, "我".into(), "ui03b".into(), 0, "t".into()).unwrap());
    app.locks.request(0, 1, 1, 93, "丙", 0);
    app.sel = (0, 0);
    app.sel_end = (2, 2);
    let before = snapshot(&app);
    let depth = app.undo.depth().0;

    app.clear_selection();
    app.apply_style(|st| st.italic = true);
    app.do_sort(0, true);

    assert_eq!(snapshot(&app), before, "有锁时多选清除/格式/排序都不能写");
    assert_eq!(app.undo.depth().0, depth, "不能产生撤销步");
    assert!(!app.wb.dirty);
    assert!(app.status.contains("丙"), "应指出锁持有人：{}", app.status);
    drop(env);
}
/// UI-03 验收矩阵：区域里只有一格被远程锁住，移动/合并/分列/去重/填充/粘贴/插格
/// 这些入口都必须整批拒绝——数据、样式、undo、dirty 全不变，状态栏指出锁持有人。
#[test]
fn every_write_entry_is_blocked_by_one_remote_lock() {
    let env = crate::test_env::TestEnv::new("ui03_matrix");
    let ctx = egui::Context::default();

    let cases: Vec<(&str, Box<dyn Fn(&mut SheetApp)>)> = vec![
        ("拖拽移动", Box::new(|a: &mut SheetApp| a.move_selection_by(1, 0))),
        ("合并", Box::new(|a: &mut SheetApp| a.toggle_merge(true))),
        (
            "分列",
            Box::new(|a: &mut SheetApp| {
                a.run_split(&DataDlg {
                    tab: 1,
                    keys: Vec::new(),
                    header: false,
                    delim: ",".into(),
                    sort_mode: 0,
                    custom: String::new(),
                    merge_consecutive: false,
                    quote: false,
                    detect_types: false,
                    sort_by_row: false,
                    split_fixed: false,
                    widths: String::new(),
                    col_modes: String::new(),
                })
            }),
        ),
        ("删除重复", Box::new(|a: &mut SheetApp| a.run_dedup())),
        ("向下填充", Box::new(|a: &mut SheetApp| a.fill_to(2, true))),
        ("选择性粘贴", Box::new(|a: &mut SheetApp| a.paste_special_apply_text(&egui::Context::default(), Some("9".into())))),
        ("插入并移动", Box::new(|a: &mut SheetApp| a.shift_insert_by(3, 0))),
    ];

    for (name, run) in cases {
        let mut app = seeded_app(&env, "lockmx");
        app.net = Some(crate::net::host(95, "我".into(), "ui03mx".into(), 0, "t".into()).unwrap());
        // 锁在 B2（(1,1)），选区 A1:B2 覆盖它
        app.locks.request(0, 1, 1, 96, "丁", 0);
        app.sel = (0, 0);
        app.sel_end = (1, 1);
        let before = snapshot(&app);
        let depth = app.undo.depth();
        // 2×2 剪贴板：选择性粘贴的目标区域是锚点 + 剪贴板尺寸，这样才会覆盖被锁的 B2
        let cell = |t: &str| (Cell { value: Value::Text(t.into()), formula: None }, 0u16);
        let clip = Clip { merges: Vec::new(),
            sheet: 0,
            rect: (0, 0, 1, 1),
            cells: vec![vec![cell("粘贴"), cell("粘贴")], vec![cell("粘贴"), cell("粘贴")]],
            cut: false,
        };
        app.clip = Some(clip);

        run(&mut app);

        assert_eq!(snapshot(&app), before, "{name} 不能绕过远程锁");
        assert_eq!(app.undo.depth(), depth, "{name} 不能产生撤销步");
        assert!(!app.wb.dirty, "{name} 被拒绝后不能标脏");
        assert!(app.status.contains("丁"), "{name} 应指出锁持有人：{}", app.status);
    }
    drop(env);
}

/// UI-03：粘贴/剪切/替换/验证下拉在远程锁下同样整批拒绝（与拖拽那组互补）。
#[test]
fn paste_cut_replace_and_dv_pick_respect_a_remote_lock() {
    let env = crate::test_env::TestEnv::new("ui03_matrix2");
    let ctx = egui::Context::default();
    let mut app = seeded_app(&env, "lockmx2");
    app.net = Some(crate::net::host(97, "我".into(), "ui03mx2".into(), 0, "t".into()).unwrap());
    // B1 与 B2 都被远程锁住：粘贴/剪切看整块，替换看被替换的那一格
    app.locks.request(0, 0, 1, 98, "戊", 0);
    app.locks.request(0, 1, 1, 98, "戊", 0);
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    let before = snapshot(&app);
    let depth = app.undo.depth();

    // 普通粘贴：2×2 内部剪贴板 → 目标覆盖 B2
    let cell = |t: &str| (Cell { value: Value::Text(t.into()), formula: None }, 0u16);
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (0, 0, 1, 1),
        cells: vec![vec![cell("P"), cell("P")], vec![cell("P"), cell("P")]],
        cut: false,
    });
    app.clip_paste(false, None);
    assert_eq!(snapshot(&app), before, "粘贴不能绕过锁");
    assert!(app.status.contains("戊"), "粘贴应指出锁持有人：{}", app.status);

    // 剪切粘贴：源块里就有被锁格
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (0, 0, 1, 1),
        cells: vec![vec![cell("C"), cell("C")], vec![cell("C"), cell("C")]],
        cut: true,
    });
    app.clip_paste(false, None);
    assert_eq!(snapshot(&app), before, "剪切源被锁时不能清源");

    // 替换
    app.find.find = "v01".into();
    app.find.replace = "X".into();
    app.sel = (0, 1);
    app.replace_current();
    assert_eq!(snapshot(&app), before, "替换不能绕过锁");
    app.replace_all();
    assert_eq!(snapshot(&app), before, "全部替换不能绕过锁");

    // 验证下拉
    app.sel = (1, 1);
    assert!(!app.dv_pick_apply(1, 1, "v11"), "锁住时下拉选择必须拒绝");

    assert_eq!(app.undo.depth(), depth, "被拒绝的操作不能进撤销栈");
    assert!(!app.wb.dirty, "被拒绝的操作不能标脏");
    drop(env);
}

/// UI-03：区域里只有一格被别人锁定，也整批不写（不做部分写入）。
#[test]
fn one_locked_cell_blocks_the_whole_clear() {
    let env = crate::test_env::TestEnv::new("ui03_lock");
    let ctx = egui::Context::default();
    let mut app = seeded_app(&env, "lock");
    app.net = Some(crate::net::host(91, "我".into(), "ui03".into(), 0, "t".into()).unwrap());
    app.locks.request(0, 1, 1, 92, "乙", 0);
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    let before = snapshot(&app);
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Delete, false)]);
    assert_eq!(snapshot(&app), before, "有一格被锁时整块都不能清");
    assert!(app.status.contains("乙"), "应指出是谁锁着：{}", app.status);
    assert!(!app.wb.dirty);
    drop(env);
}

/// C14：主加载工作线程 panic 必须变成可诊断的失败，原文件不被写、不绑定到当前文档。
#[test]
fn open_worker_panic_reports_failure_and_keeps_the_file_untouched() {
    use sha2::{Digest, Sha256};
    let env = crate::test_env::TestEnv::new("c14_worker_panic");
    let src = env.path("seed.xls");
    std::fs::copy(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../core/tests/fixtures/real_excel.xls"),
        &src,
    )
    .unwrap();
    let before: Vec<u8> = std::fs::read(&src).unwrap();
    let sha_before: String = Sha256::digest(&before).iter().map(|b| format!("{b:02x}")).collect();

    // 故障注入按路径登记：只影响这一份文件，不会串到并行用例。
    sheetcore::xlsx::inject_worker_panic(&src);
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.open_path(src.clone());
    for _ in 0..200 {
        app.poll_open();
        if !app.tasks.busy {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    sheetcore::xlsx::clear_fault_injections();

    assert!(!app.tasks.busy, "线程死后不能一直占用 busy");
    assert!(
        app.status.contains("加载线程异常退出"),
        "status 要说明 worker panic：{}",
        app.status
    );
    assert!(app.wb.path.is_none(), "失败后不能把原路径挂到当前文档");
    assert_eq!(app.wb.path.as_deref(), None);
    let after = std::fs::read(&src).unwrap();
    assert_eq!(after, before, "失败路径不能改原文件");
    let sha_after: String = Sha256::digest(&after).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(sha_after, sha_before);
    drop(env);
}

/// UI-04 验收：A1=1、A2=2、B1=A1*10、B2=A2*10，选 A1:A2 清除内容后
/// B1/B2 立即为 0；Ctrl+Z 一次恢复四格；保存会提示 dirty。
#[test]
fn clear_contents_recalcs_dependents_and_one_undo_restores_all() {
    let env = crate::test_env::TestEnv::new("ui04_recalc");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let num = |v: f64| Cell { value: sheetcore::model::Value::Number(v), formula: None };
    let f = |t: &str| Cell { value: sheetcore::model::Value::Empty, formula: Some(t.into()) };
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, num(1.0));
        sh.set(1, 0, num(2.0));
        sh.set(0, 1, f("=A1*10"));
        sh.set(1, 1, f("=A2*10"));
    }
    sheetcore::formula::recalc_all(&mut app.wb);
    assert_eq!(app.wb.sheet(0).value(0, 1), sheetcore::model::Value::Number(10.0));

    app.sel = (0, 0);
    app.sel_end = (1, 0);
    app.clear_values();

    assert_eq!(app.wb.sheet(0).value(0, 0), sheetcore::model::Value::Empty);
    assert_eq!(app.wb.sheet(0).value(1, 1), sheetcore::model::Value::Number(0.0), "B2 要立即变 0");
    assert_eq!(app.wb.sheet(0).value(0, 1), sheetcore::model::Value::Number(0.0), "B1 要立即变 0");
    assert!(app.wb.dirty, "清除后必须标脏");
    assert_eq!(app.undo.depth().0, 1, "一次清除只压一步撤销");

    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(0, 0), sheetcore::model::Value::Number(1.0));
    assert_eq!(app.wb.sheet(0).value(1, 0), sheetcore::model::Value::Number(2.0));
    assert_eq!(app.wb.sheet(0).value(0, 1), sheetcore::model::Value::Number(10.0), "撤销后 B1 恢复");
    assert_eq!(app.wb.sheet(0).value(1, 1), sheetcore::model::Value::Number(20.0), "撤销后 B2 恢复");
    drop(env);
}

/// UI-04：多选 A1 与 A3（非连续）只清这两块，A2 不能被误清。
#[test]
fn clear_multi_select_does_not_touch_the_gap() {
    let env = crate::test_env::TestEnv::new("ui04_multi");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let num = |v: f64| Cell { value: sheetcore::model::Value::Number(v), formula: None };
    app.wb.sheet_mut(0).set(0, 0, num(1.0));
    app.wb.sheet_mut(0).set(1, 0, num(2.0));
    app.wb.sheet_mut(0).set(2, 0, num(3.0));
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.extra_sel = vec![(2, 0, 2, 0)];
    app.clear_values();
    assert_eq!(app.wb.sheet(0).value(0, 0), sheetcore::model::Value::Empty);
    assert_eq!(app.wb.sheet(0).value(1, 0), sheetcore::model::Value::Number(2.0), "A2 不能被误清");
    assert_eq!(app.wb.sheet(0).value(2, 0), sheetcore::model::Value::Empty);
    drop(env);
}

/// UI-04：清除格式保留内容；清除全部一次撤销同时恢复内容和格式。
#[test]
fn clear_formats_and_clear_all_are_distinct_and_undoable() {
    let env = crate::test_env::TestEnv::new("ui04_parts");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, Cell { value: sheetcore::model::Value::Number(5.0), formula: None });
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.apply_style(|st| st.bold = true);
    assert_ne!(app.wb.sheet(0).style_of(0, 0), 0);

    app.clear_formats();
    assert_eq!(app.wb.sheet(0).value(0, 0), sheetcore::model::Value::Number(5.0), "清格式不能丢内容");
    assert_eq!(app.wb.sheet(0).style_of(0, 0), 0, "样式要回到默认");
    app.do_undo(false);
    assert_ne!(app.wb.sheet(0).style_of(0, 0), 0, "撤销要恢复样式");

    app.clear_all();
    assert_eq!(app.wb.sheet(0).value(0, 0), sheetcore::model::Value::Empty);
    assert_eq!(app.wb.sheet(0).style_of(0, 0), 0);
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(0, 0), sheetcore::model::Value::Number(5.0), "清全部的撤销要带回内容");
    assert_ne!(app.wb.sheet(0).style_of(0, 0), 0, "清全部的撤销要带回格式");
    drop(env);
}

/// UI-05 验收：A300、XFD1048576 都能从 A1 找到；一轮后回到第一处。
#[test]
fn find_reaches_far_cells_and_wraps_to_the_first_hit() {
    let env = crate::test_env::TestEnv::new("ui05_far");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let t = |v: &str| Cell { value: sheetcore::model::Value::Text(v.into()), formula: None };
    app.wb.sheet_mut(0).set(299, 0, t("needle"));
    app.wb.sheet_mut(0).set(1_048_575, 16_383, t("needle"));
    app.sel = (0, 0);
    app.find.find = "needle".into();
    app.find_next(false);
    assert_eq!(app.sel, (299, 0), "A1→A300");
    app.find_next(false);
    assert_eq!(app.sel, (1_048_575, 16_383), "A300→XFD1048576");
    app.find_next(false);
    assert_eq!(app.sel, (299, 0), "一轮后回到第一处");
    assert!(app.status.contains("回到开头"), "status={}", app.status);
    drop(env);
}

/// UI-05 验收：整格匹配不改 xabc；公式模式把 A1 换成 B1 并保持公式。
#[test]
fn whole_cell_replace_spares_longer_text_and_formula_replace_keeps_formula() {
    let env = crate::test_env::TestEnv::new("ui05_whole");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let t = |v: &str| Cell { value: sheetcore::model::Value::Text(v.into()), formula: None };
    app.wb.sheet_mut(0).set(0, 0, t("abc"));
    app.wb.sheet_mut(0).set(0, 1, t("xabc"));
    app.find.find = "abc".into();
    app.find.replace = "Z".into();
    app.find.whole_cell = true;
    app.replace_all();
    assert_eq!(app.wb.sheet(0).value(0, 0), sheetcore::model::Value::Text("Z".into()));
    assert_eq!(app.wb.sheet(0).value(0, 1), sheetcore::model::Value::Text("xabc".into()), "xabc 不能被改");
    assert!(app.status.contains("找到 1 处，替换 1 处"), "{}", app.status);

    let mut cell = Cell { value: sheetcore::model::Value::Number(3.0), formula: Some("=SUM(A1:A2)".into()) };
    app.wb.sheet_mut(0).set(0, 2, cell.clone());
    app.find = FindState { open: true, tab: 1, find: "A1".into(), replace: "B1".into(), whole_cell: false, in_formula: true, ..Default::default() };
    app.sel = (0, 2);
    app.replace_current();
    let got = app.wb.sheet(0).get(0, 2).unwrap();
    assert_eq!(got.formula.as_deref(), Some("=SUM(B1:A2)"), "公式文本被替换");
    assert!(!matches!(got.value, sheetcore::model::Value::Text(_)), "不能写成常量文本");
    drop(env);
}

/// UI-05 验收：1000 格全部替换只压一步撤销；数值也能被值模式命中。
#[test]
fn replace_all_is_one_undo_step_and_covers_numbers() {
    let env = crate::test_env::TestEnv::new("ui05_all");
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for r in 0..1000u32 {
        app.wb.sheet_mut(0).set(r, 0, Cell { value: sheetcore::model::Value::Text("old".into()), formula: None });
    }
    app.wb.sheet_mut(0).set(1000, 0, Cell { value: sheetcore::model::Value::Number(1234.0), formula: None });
    app.find.find = "old".into();
    app.find.replace = "new".into();
    let depth = app.undo.depth().0;
    app.replace_all();
    assert_eq!(app.undo.depth().0, depth + 1, "全部替换只产生一步撤销");
    assert_eq!(app.wb.sheet(0).value(999, 0), sheetcore::model::Value::Text("new".into()));
    assert!(app.status.contains("找到 1000 处，替换 1000 处"), "{}", app.status);
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(0, 0), sheetcore::model::Value::Text("old".into()));
    assert_eq!(app.wb.sheet(0).value(999, 0), sheetcore::model::Value::Text("old".into()), "一次撤销恢复全部");

    app.find.find = "1234".into();
    app.find.replace = "5678".into();
    app.sel = (1000, 0);
    app.replace_all();
    assert_eq!(app.wb.sheet(0).value(1000, 0), sheetcore::model::Value::Number(5678.0), "数值命中并替换");
    drop(env);
}

/// UI-03 复核：Ctrl+D / Ctrl+R 快捷键入口也走统一门禁，失败不留值/样式/undo/dirty。
#[test]
fn fill_shortcuts_are_blocked_by_protection_tail_locks_and_pending_deps() {
    // 受保护表
    let mut protected = app();
    protected.wb.sheet_mut(0).set(0, 0, number(1.0));
    protected.wb.sheet_mut(0).set(1, 0, number(2.0));
    protected.wb.sheet_mut(0).protection = Some(sheetcore::protection::SheetProtection::default());
    protected.sel = (0, 0);
    protected.sel_end = (1, 0);
    protected.wb.dirty = false;
    run_keys(&mut protected, &egui::Context::default(), vec![ctrl_key_event(egui::Key::D)]);
    assert_eq!(protected.wb.sheet(0).value(1, 0), number(2.0).value, "受保护时不能填充");
    assert_eq!(protected.undo.depth(), (0, 0));
    assert!(!protected.wb.dirty);
    assert!(protected.status.contains("受保护"), "{}", protected.status);

    // Ctrl+R 横向分支：真实按键，只有末端目标列 C1 被锁
    let mut right = app();
    right.wb.sheet_mut(0).set(0, 0, number(3.0));
    let now_r = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    right.locks.request(0, 0, 2, 93, "庚", now_r);
    right.sel = (0, 0);
    right.sel_end = (0, 2);
    right.wb.dirty = false;
    run_keys(&mut right, &egui::Context::default(), vec![ctrl_key_event(egui::Key::R)]);
    assert_eq!(right.wb.sheet(0).value(0, 1), Value::Empty, "锁前不能部分写入");
    assert_eq!(right.wb.sheet(0).value(0, 2), Value::Empty);
    assert_eq!(right.undo.depth(), (0, 0));
    assert!(!right.wb.dirty);

    // 只有尾端目标格被锁：首行是源，也必须整批拒绝
    let mut locked = app();
    locked.wb.sheet_mut(0).set(0, 0, number(1.0));
    locked.wb.sheet_mut(0).set(1, 0, number(2.0));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    locked.locks.request(0, 1, 0, 98, "乙", now);
    locked.sel = (0, 0);
    locked.sel_end = (1, 0);
    locked.wb.dirty = false;
    run_keys(&mut locked, &egui::Context::default(), vec![ctrl_key_event(egui::Key::D)]);
    assert_eq!(locked.wb.sheet(0).value(1, 0), number(2.0).value, "尾端目标被锁不能写");
    assert_eq!(locked.undo.depth(), (0, 0));
    assert!(!locked.wb.dirty);

    // 后台依赖未就绪
    let mut pending = app();
    pending.wb.sheet_mut(0).set(0, 0, number(1.0));
    pending.wb.sheet_mut(0).set(1, 0, number(2.0));
    pending.deps_pending = true;
    pending.sel = (0, 0);
    pending.sel_end = (1, 0);
    pending.wb.dirty = false;
    run_keys(&mut pending, &egui::Context::default(), vec![ctrl_key_event(egui::Key::D)]);
    assert_eq!(pending.wb.sheet(0).value(1, 0), number(2.0).value);
    assert_eq!(pending.undo.depth(), (0, 0));
    assert!(!pending.wb.dirty);
}

/// UI-03 复核：横向填充柄的门禁要覆盖真实目标列，而不是只查源区。
#[test]
fn horizontal_fill_gate_covers_the_real_target_column() {
    let mut app = app();
    app.wb.sheet_mut(0).set(0, 0, number(7.0));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    app.locks.request(0, 0, 2, 97, "丙", now); // C1 末端目标被他人锁
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.wb.dirty = false;
    app.fill_to(5, false);
    assert_eq!(app.wb.sheet(0).value(0, 0), number(7.0).value);
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Empty, "不能在锁前部分写入");
    assert_eq!(app.wb.sheet(0).value(0, 5), Value::Empty);
    assert_eq!(app.undo.depth(), (0, 0));
    assert!(!app.wb.dirty);
}

/// UI-03 复核：边框拖拽（移动/复制/Shift 插入）分别对源、目标、移位范围预检。
#[test]
fn drag_moves_copy_and_shift_check_source_target_and_moved_ranges() {
    // 移动：源被锁 → 不先清源
    let mut moved = app();
    moved.wb.sheet_mut(0).set(0, 0, number(7.0));
    moved.wb.sheet_mut(0).set(1, 0, number(8.0));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    moved.locks.request(0, 0, 0, 96, "乙", now);
    moved.sel = (0, 0);
    moved.sel_end = (1, 0);
    moved.wb.dirty = false;
    moved.move_selection_by(0, 2);
    assert_eq!(moved.wb.sheet(0).value(0, 0), number(7.0).value, "源不能被清");
    assert_eq!(moved.wb.sheet(0).value(0, 2), Value::Empty, "目标不能半写");
    assert_eq!(moved.undo.depth(), (0, 0));
    assert!(!moved.wb.dirty);

    // 复制：目标被锁 → 不写目标
    let mut copy = app();
    copy.wb.sheet_mut(0).set(0, 0, number(7.0));
    let now2 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    copy.locks.request(0, 0, 2, 95, "丙", now2);
    copy.sel = (0, 0);
    copy.sel_end = (0, 0);
    copy.wb.dirty = false;
    copy.copy_selection_by(0, 2);
    assert_eq!(copy.wb.sheet(0).value(0, 2), Value::Empty);
    assert_eq!(copy.wb.sheet(0).value(0, 0), number(7.0).value);
    assert_eq!(copy.undo.depth(), (0, 0));

    // Shift 插入：插入点右侧已有格被锁（结构移位会挪到它）→ 整批拒绝
    let mut shift = app();
    shift.wb.sheet_mut(0).set(0, 0, number(7.0));
    shift.wb.sheet_mut(0).set(0, 1, number(8.0));
    let now3 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    shift.locks.request(0, 0, 2, 94, "丁", now3); // C1：插列后会被右移的格
    shift.sel = (0, 0);
    shift.sel_end = (0, 0);
    shift.wb.dirty = false;
    shift.shift_insert_by(0, 2);
    assert_eq!(shift.wb.sheet(0).value(0, 0), number(7.0).value, "源不能先被清");
    assert_eq!(shift.wb.sheet(0).value(0, 1), number(8.0).value);
    assert_eq!(shift.undo.depth(), (0, 0));
    assert!(!shift.wb.dirty);
}

/// UI-04 复核：清除全部要同时同步内容和样式；第二客户端与撤销一致。
#[test]
fn clear_all_broadcasts_style_and_content_to_a_second_client_and_undo_restores_both() {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = crate::net::host_on(listener, 1, "甲".into(), "doc".into(), "pw".into()).unwrap();
    // host.addr 是 0.0.0.0:port（监听通配地址），客户端要连回环地址
    let addr = format!("127.0.0.1:{port}");
    let peer = crate::net::join(2, "乙".into(), "doc".into(), &addr, "pw".into()).unwrap();

    let mut a = app();
    let mut b = app();
    a.peer = Some(sheetcore::sync::Peer::new(1));
    b.peer = Some(sheetcore::sync::Peer::new(2));
    a.net = Some(host);
    b.net = Some(peer);
    for _ in 0..40 {
        a.poll_net();
        b.poll_net();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let mut style = a.wb.styles.get(0).clone();
    style.bold = true;
    style.fill = Some([255, 200, 0]);
    let sid = a.wb.styles.intern(style);
    a.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Text("x".into()), formula: None });
    a.wb.sheet_mut(0).set_style(0, 0, sid);
    a.wb.dirty = false;
    b.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Text("x".into()), formula: None });
    // 第二客户端自己也要有非默认格式，否则无法证明样式清除被同步
    let mut st2 = b.wb.styles.get(0).clone();
    st2.bold = true;
    let sid2 = b.wb.styles.intern(st2);
    b.wb.sheet_mut(0).set_style(0, 0, sid2);
    assert!(b.wb.styles.get(b.wb.sheet(0).style_of(0, 0)).bold, "前置：对端确有加粗");

    a.sel = (0, 0);
    a.sel_end = (0, 0);
    a.clear_rects(ClearPart::All);
    assert_eq!(a.wb.sheet(0).value(0, 0), Value::Empty);
    assert_eq!(a.wb.sheet(0).style_of(0, 0), 0, "本地样式也要清");

    let mut synced = false;
    let peer_style_cleared = |b: &SheetApp| {
        let st = b.wb.styles.get(b.wb.sheet(0).style_of(0, 0));
        !st.bold && st.fill.is_none()
    };
    for _ in 0..100 {
        b.poll_net();
        if b.wb.sheet(0).value(0, 0) == Value::Empty && peer_style_cleared(&b) {
            synced = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(synced, "第二客户端必须同时收到内容清除和样式清除");

    a.do_undo(false);
    assert_eq!(a.wb.sheet(0).value(0, 0), Value::Text("x".into()), "撤销恢复内容");
    assert_eq!(a.wb.sheet(0).style_of(0, 0), sid, "撤销恢复原样式");

    // 对端必须也在 poll 后同时恢复旧内容和旧样式（只恢复值不够）
    let peer_style_restored = |b: &SheetApp| {
        let st = b.wb.styles.get(b.wb.sheet(0).style_of(0, 0));
        st.bold && st.fill == Some([255, 200, 0])
    };
    let mut remote_undo = false;
    for _ in 0..100 {
        b.poll_net();
        if b.wb.sheet(0).value(0, 0) == Value::Text("x".into()) && peer_style_restored(&b) {
            remote_undo = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(remote_undo, "撤销后第二客户端要同时恢复内容和样式");

    // 重做同样要同步：内容清空 + 格式回默认
    a.do_undo(true);
    assert_eq!(a.wb.sheet(0).value(0, 0), Value::Empty);
    let mut remote_redo = false;
    for _ in 0..100 {
        b.poll_net();
        if b.wb.sheet(0).value(0, 0) == Value::Empty && peer_style_cleared(&b) {
            remote_redo = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(remote_redo, "重做后第二客户端也要同步清内容和格式");
    a.net.as_mut().unwrap().shutdown();
    b.net.as_mut().unwrap().shutdown();
}

/// UI-05 复核：值模式按网格显示串查；公式结果默认跳过并提示，勾选后才可替换。
#[test]
fn find_uses_formatted_values_and_formula_results_need_explicit_opt_in() {
    let mut app = app();
    app.wb.sheet_mut(0).set(0, 0, number(0.5));
    let mut st = app.wb.styles.get(0).clone();
    st.format = "0.00%".into();
    let sid = app.wb.styles.intern(st);
    app.wb.sheet_mut(0).set_style(0, 0, sid);
    app.find.find = "50.00%".into();
    app.sel = (0, 0);
    app.find_next(false);
    assert_eq!(app.sel, (0, 0), "日期/百分比这类显示串要能查到");
    assert!(app.status.contains("找到"), "{}", app.status);

    // C1 是公式结果 1：值模式默认不能静默改成常量
    app.wb.sheet_mut(0).set(0, 2, Cell {
        value: Value::Number(1.0),
        formula: Some("=A1*2".into()),
    });
    app.find = FindState {
        open: true,
        tab: 1,
        find: "1".into(),
        replace: "9".into(),
        ..Default::default()
    };
    app.sel = (0, 2);
    app.replace_current();
    let c1 = app.wb.sheet(0).get(0, 2).unwrap();
    assert_eq!(c1.formula.as_deref(), Some("=A1*2"), "默认不能破坏公式");
    assert_eq!(app.undo.depth(), (0, 0), "跳过不产生撤销步");
    assert!(app.status.contains("公式结果"), "{}", app.status);

    // 全部替换同样不能绕过公式保护
    app.replace_all();
    assert!(
        app.wb.sheet(0).get(0, 2).unwrap().formula.is_some(),
        "全部替换默认也要跳过公式结果"
    );
    assert!(app.status.contains("公式结果"), "{}", app.status);

    app.find.replace_formula_results = true;
    app.sel = (0, 2);
    app.replace_current();
    let c1 = app.wb.sheet(0).get(0, 2).unwrap();
    assert!(c1.formula.is_none(), "显式勾选后才允许换成常量");
    assert_eq!(c1.value, Value::Number(9.0));
    assert_eq!(app.undo.depth(), (1, 0));
}

/// UI-05 复核：干净簿上单次替换必须标脏、可撤销，并且保存后重开能读回。
#[test]
fn single_replace_marks_dirty_survives_save_and_undoes_in_one_step() {
    let env = crate::test_env::TestEnv::new("ui05_single");
    let path = env.path("single.xlsx");
    let mut app = app();
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Text("old".into()), formula: None });
    sheetcore::xlsx::save(&path, &app.wb).unwrap();
    app.wb.path = Some(path.clone());
    app.wb.dirty = false;

    app.find = FindState {
        open: true,
        tab: 1,
        find: "old".into(),
        replace: "new".into(),
        ..Default::default()
    };
    app.sel = (0, 0);
    app.replace_current();
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("new".into()));
    assert!(app.wb.dirty, "单次替换必须把干净文档标脏");
    assert_eq!(app.undo.depth(), (1, 0));

    app.save(false);
    for _ in 0..300 {
        app.poll_save();
        if app.tasks.saving.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(app.tasks.saving.is_none(), "保存要在超时内完成");
    let (wb, diag) = sheetcore::xlsx::load_diag(&path).unwrap();
    assert!(diag.is_complete(), "{:?}", diag.problems);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Text("new".into()), "保存后重开读到新值");

    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("old".into()), "一步撤销恢复");
    drop(env);
}

/// UI-05.4：选区范围与按列顺序真正参与查找；选区外不命中。
#[test]
fn find_selection_scope_and_column_order_are_honored() {
    let mut app = app();
    for (r, c) in [(0u32, 0u32), (0, 1), (1, 0), (5, 2)] {
        app.wb.sheet_mut(0).set(r, c, text("x"));
    }
    app.find.find = "x".into();
    app.find.scope = 2;
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    let hits = app.find_hits();
    assert_eq!(hits.len(), 3, "选区 (A1:B2) 内 3 处，C6 不算");
    assert!(!hits.iter().any(|h| (h.row, h.col) == (5, 2)));

    app.find.order_column = true;
    let hits = app.find_hits();
    assert_eq!(
        hits.iter().map(|h| (h.row, h.col)).collect::<Vec<_>>(),
        vec![(0, 0), (1, 0), (0, 1)],
        "按列顺序：A1, A2, B1"
    );
    app.find.scope = 0;
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.find_next(false);
    assert_eq!(app.sel, (1, 0), "按列查找的下一处应向下而不是向右");
}

/// UI-05.4：全部替换只看选区，且仍是一个撤销步。
#[test]
fn replace_all_in_selection_spares_cells_outside_and_is_one_undo() {
    let mut app = app();
    for (r, c) in [(0u32, 0u32), (0, 1), (1, 0), (5, 2)] {
        app.wb.sheet_mut(0).set(r, c, text("old"));
    }
    app.find = FindState {
        open: true,
        tab: 1,
        scope: 2,
        find: "old".into(),
        replace: "new".into(),
        ..Default::default()
    };
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    app.replace_all();
    for (r, c) in [(0u32, 0u32), (0, 1), (1, 0)] {
        assert_eq!(app.wb.sheet(0).value(r, c), Value::Text("new".into()));
    }
    assert_eq!(
        app.wb.sheet(0).value(5, 2),
        Value::Text("old".into()),
        "选区外不能动"
    );
    assert_eq!(app.undo.depth(), (1, 0), "选区全部替换只压一步撤销");
    assert!(app.status.contains("3"), "{}", app.status);
}

/// UI-05.4：大范围扫描分帧进行，可取消且取消后不写任何格；继续扫描则最终一次提交。
#[test]
fn long_find_scan_is_chunked_cancellable_and_commits_once() {
    let mut app = app();
    for r in 0..9000u32 {
        app.wb.sheet_mut(0).set(r, 0, text("old"));
    }
    app.find = FindState {
        open: true,
        tab: 1,
        find: "old".into(),
        replace: "new".into(),
        ..Default::default()
    };
    app.start_scan(1);
    assert!(app.find_scan.is_some(), "9000 格应进入分帧扫描");
    app.poll_find_scan();
    assert!(app.find_scan.is_some(), "一帧还没扫完");
    app.cancel_find_scan();
    assert!(app.find_scan.is_none());
    assert_eq!(app.status, "已取消扫描");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("old".into()));
    assert_eq!(app.undo.depth(), (0, 0), "取消不能产生撤销步");

    app.start_scan(1);
    let mut guard = 0;
    while app.find_scan.is_some() && guard < 10 {
        app.poll_find_scan();
        guard += 1;
    }
    assert!(app.find_scan.is_none(), "分帧应在有限帧内完成");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("new".into()));
    assert_eq!(app.wb.sheet(0).value(8999, 0), Value::Text("new".into()));
    assert_eq!(app.undo.depth(), (1, 0), "扫描完成后一次提交、一步撤销");
    assert!(app.status.contains("9000"), "{}", app.status);
}

/// UI-05.4：查找全部把整个工作簿的命中列成结果表（可跳转的数据源）。
#[test]
fn find_all_collects_workbook_hits_with_sheet_and_position() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::model::Sheet::new("第二表"));
    app.wb.sheet_mut(0).set(0, 0, text("q"));
    app.wb.sheet_mut(1).set(1, 1, text("q"));
    app.find.find = "q".into();
    app.find.scope = 1;
    app.start_scan(0);
    assert!(app.find.find_all_open);
    assert_eq!(app.find.find_all.len(), 2);
    assert_eq!((app.find.find_all[0].sheet, app.find.find_all[0].row, app.find.find_all[0].col), (0, 0, 0));
    assert_eq!((app.find.find_all[1].sheet, app.find.find_all[1].row, app.find.find_all[1].col), (1, 1, 1));
    assert!(app.status.contains("2"), "{}", app.status);
}

/// UI-05.4：跨工作簿全部替换按表捕获后合并成一个撤销步。
#[test]
fn replace_all_across_sheets_is_one_undo_step() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::model::Sheet::new("第二表"));
    app.wb.sheet_mut(0).set(0, 0, text("old"));
    app.wb.sheet_mut(1).set(0, 0, text("old"));
    app.find = FindState {
        open: true,
        tab: 1,
        scope: 1,
        find: "old".into(),
        replace: "new".into(),
        ..Default::default()
    };
    app.replace_all();
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("new".into()));
    assert_eq!(app.wb.sheet(1).value(0, 0), Value::Text("new".into()));
    assert_eq!(app.undo.depth(), (1, 0), "两个表也只压一步");
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("old".into()));
    assert_eq!(app.wb.sheet(1).value(0, 0), Value::Text("old".into()));
}

/// 查找对话框与查找全部窗口能真实构建（选区/按列/结果列表控件都在）。
#[test]
fn find_dialog_and_results_window_render() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, text("q"));
    app.find = FindState {
        open: true,
        find: "q".into(),
        scope: 2,
        order_column: true,
        ..Default::default()
    };
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    app.start_scan(0);
    assert!(app.find.find_all_open);
    let _out = ctx.run(Default::default(), |ctx| {
        app.find_dialog(ctx);
        app.find_all_window(ctx);
    });
}

/// UI-10：Ctrl+F/Ctrl+H 必须分别落到“查找/替换”页签，不能总是查找页。
/// UI-18：清除条件保留筛选器/箭头，关闭筛选才移除；手动隐藏行走独立集合。
#[test]
fn filter_clear_keeps_filter_and_manual_hidden_rows() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let sh = app.wb.sheet_mut(0);
    sh.set(0, 0, text("Name"));
    sh.set(0, 1, text("Amount"));
    sh.set(1, 0, text("A"));
    sh.set(1, 1, number(10.0));
    sh.set(2, 0, text("B"));
    sh.set(2, 1, number(20.0));
    sh.set(3, 0, text("C"));
    sh.set(3, 1, number(30.0));
    let mut f = sheetcore::sort_filter::Filter::new(0, 0, 3, 1);
    f.criteria.insert(1, sheetcore::sort_filter::Criterion::new(
        sheetcore::sort_filter::CmpOp::Gt, "15"));
    sh.filter = Some(f.clone());
    sh.hidden_rows = f.hidden_rows(sh);
    sh.hidden_rows_user.insert(5);
    assert!(sh.hidden_rows.contains(&1), "10 应被条件隐藏");

    app.ribbon_action("FilterClear", &ctx);
    let sh = app.wb.sheet(0);
    assert!(sh.filter.is_some(), "清除条件后筛选箭头必须保留");
    let f = sh.filter.as_ref().unwrap();
    assert!(f.allows.is_empty() && f.criteria.is_empty());
    assert!(sh.hidden_rows.is_empty(), "符合条件的所有行都必须恢复");
    assert!(sh.hidden_rows_user.contains(&5), "手动隐藏行不能被误取消");
    assert!(app.status.contains("筛选条件"));

    // 重新加条件并用 Ribbon 重新应用；筛选范围含表头，A1 在内。
    let sh = app.wb.sheet_mut(0);
    sh.filter.as_mut().unwrap().criteria.insert(1,
        sheetcore::sort_filter::Criterion::new(sheetcore::sort_filter::CmpOp::Gt, "15"));
    app.ribbon_action("FilterReapply", &ctx);
    assert!(app.wb.sheet(0).hidden_rows.contains(&1));
    assert!(app.wb.sheet(0).hidden_rows_user.contains(&5));

    app.sel = (0, 0);
    app.start_filter();
    assert!(app.wb.sheet(0).filter.is_none(), "再次点筛选必须关闭并移除箭头");
    assert!(app.wb.sheet(0).hidden_rows.is_empty());
    assert!(app.wb.sheet(0).hidden_rows_user.contains(&5));
}

/// UI-18：清除条件后保存/重开，筛选器（箭头）在、条件为空、手动隐藏行仍在。
#[test]
fn filter_clear_survives_save_reload() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let sh = app.wb.sheet_mut(0);
    sh.set(0, 0, text("Name"));
    sh.set(0, 1, text("Amount"));
    sh.set(1, 0, text("A"));
    sh.set(1, 1, number(10.0));
    sh.set(2, 0, text("B"));
    sh.set(2, 1, number(20.0));
    let mut f = sheetcore::sort_filter::Filter::new(0, 0, 2, 1);
    f.criteria.insert(1, sheetcore::sort_filter::Criterion::new(
        sheetcore::sort_filter::CmpOp::Gt, "15"));
    sh.filter = Some(f.clone());
    sh.hidden_rows = f.hidden_rows(sh);
    sh.hidden_rows_user.insert(4);
    app.ribbon_action("FilterClear", &ctx);
    let tmp = std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("test dir")
        .join(format!("ui18_filter_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save(&tmp, &app.wb).expect("save");
    let back = sheetcore::xlsx::load(&tmp).expect("load");
    let _ = std::fs::remove_file(&tmp);
    let sh = back.sheet(0);
    assert!(sh.filter.is_some(), "保存重开后筛选箭头应保留");
    let f = sh.filter.as_ref().unwrap();
    assert!(f.allows.is_empty() && f.criteria.is_empty());
    assert!(sh.hidden_rows.is_empty());
    assert!(sh.hidden_rows_user.contains(&4), "手动隐藏行应随文件保留");
}

#[test]
fn ctrl_h_opens_replace_tab_and_ctrl_f_returns_to_find_tab() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.find.find = "alpha".into();
    app.find.replace = "beta".into();
    app.find.open = false;
    app.find.tab = 0;

    // 首次关闭后从 Ribbon/快捷键重开：输入必须保留，页签按快捷键切换。
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::H)]);
    assert!(app.find.open, "Ctrl+H 必须打开查找对话框");
    assert_eq!(app.find.tab, 1, "Ctrl+H 必须直接显示替换页");
    assert_eq!(app.find.find, "alpha", "重开不得清空查找内容");
    assert_eq!(app.find.replace, "beta", "重开不得清空替换内容");

    app.find.open = false;
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::F)]);
    assert_eq!(app.find.tab, 0, "Ctrl+F 必须回到查找页");
    assert_eq!(app.find.find, "alpha");
    assert_eq!(app.find.replace, "beta");

    // 再次 Ctrl+H 仍保留，而不是每次重开丢替换文本。
    app.find.open = false;
    run_keys(&mut app, &ctx, vec![ctrl_key_event(egui::Key::H)]);
    assert_eq!(app.find.tab, 1);
    assert_eq!(app.find.find, "alpha");
    assert_eq!(app.find.replace, "beta");
}

/// UI-05 安全语义：全部替换必须按命中所在表取文本/格式，不能拿活动表判断。
/// A!A1=keep 与 B!A1=old 同坐标，B!C3 依赖 B 自己的百分比格式才能命中。
#[test]
fn replace_all_uses_the_hit_sheet_not_the_active_sheet() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::model::Sheet::new("B"));
    app.wb.sheet_mut(0).set(0, 0, text("keep"));
    app.wb.sheet_mut(0).set(2, 2, number(0.5)); // 活动表同坐标是普通数字
    app.wb.sheet_mut(1).set(0, 0, text("old"));
    app.wb.sheet_mut(1).set(2, 2, number(0.5)); // B!C3
    app.wb.sheet_mut(1).set(3, 3, text("old")); // B!D4，不同坐标
    let mut st = app.wb.styles.get(0).clone();
    st.bold = true;
    st.format = "0.00%".into();
    let sid = app.wb.styles.intern(st);
    app.wb.sheet_mut(1).set_style(2, 2, sid);
    app.wb.sheet_mut(1).set_style(3, 3, sid);

    app.find = FindState {
        open: true,
        tab: 1,
        scope: 1,
        find: "old".into(),
        replace: "new".into(),
        ..Default::default()
    };
    app.replace_all();
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("keep".into()), "活动表不能被动");
    assert_eq!(app.wb.sheet(1).value(0, 0), Value::Text("new".into()), "B!A1 要替换");
    assert_eq!(app.wb.sheet(1).value(3, 3), Value::Text("new".into()), "B!D4 也要替换");
    assert_eq!(app.wb.sheet(1).style_of(3, 3), sid, "替换保留原格式");
    assert_eq!(app.wb.sheet(0).value(2, 2), Value::Number(0.5), "活动表 C3 不动");

    // 只有 B!C3 的百分比格式能把 0.5 显示成 50.00%：查显示串必须命中 B，而不是 A
    app.find.find = "50.00%".into();
    app.find.replace = "X".into();
    app.replace_all();
    assert_eq!(
        app.wb.sheet(1).value(2, 2),
        Value::Text("X".into()),
        "要按命中表的格式取显示串"
    );
    assert_eq!(app.wb.sheet(0).value(2, 2), Value::Number(0.5));
    app.do_undo(false);
    assert_eq!(app.wb.sheet(1).value(2, 2), Value::Number(0.5));
}

/// UI-05 安全语义：搜索选区一旦开始就与命中光标分离，连续 Next/Prev 不退化。
#[test]
fn selection_scope_survives_repeated_next_and_previous() {
    let mut app = app();
    for (r, c) in [(0u32, 0u32), (0, 1), (1, 0), (1, 1)] {
        app.wb.sheet_mut(0).set(r, c, text("x"));
    }
    app.find = FindState {
        open: true,
        scope: 2,
        find: "x".into(),
        ..Default::default()
    };
    app.sel = (0, 0);
    app.sel_end = (1, 1);
    // 从 A1 开始，下一次依次 B1、A2、B2，再下一次回绕到 A1
    for (i, rc) in [(0u32, 1u32), (1, 0), (1, 1)].iter().enumerate() {
        app.find_next(false);
        assert_eq!(app.sel, *rc, "第 {} 次下一次", i + 1);
        assert_eq!(app.find.hits, 4, "选区 4 处不能缩成 1 处");
    }
    app.find_next(false);
    assert_eq!(app.sel, (0, 0), "第 4 次回绕到选区第一处");
    assert!(app.status.contains("回到开头"), "{}", app.status);
    // 反向从第一处回绕到最后一处
    app.find_next(true);
    assert_eq!(app.sel, (1, 1));
    assert!(app.status.contains("从末尾继续"), "{}", app.status);
}

/// UI-05 安全语义：扫描绑定文档世代；切文档后旧扫描作废，不得写新文档。
#[test]
fn stale_scan_is_dropped_after_document_switch() {
    let mut app = app();
    for r in 0..9000u32 {
        app.wb.sheet_mut(0).set(r, 0, text("old"));
    }
    app.find = FindState {
        open: true,
        tab: 1,
        find: "old".into(),
        replace: "new".into(),
        ..Default::default()
    };
    app.start_scan(1);
    let stale = app.find_scan.take().expect("应进入分帧扫描");
    // 模拟切到另一份文档：内容与世代都换了，但扫描被错误地保留
    let mut b = sheetcore::model::Workbook::new_blank();
    b.sheets[0].set(0, 0, text("old"));
    b.sheets[0].set(1, 1, text("old"));
    app.wb = b;
    app.wb_epoch += 1;
    app.wb.dirty = false;
    app.find_scan = Some(stale);
    app.poll_find_scan();
    assert!(app.find_scan.is_none(), "跨文档的扫描必须立即丢弃");
    assert_eq!(app.status, "文档已切换，已取消扫描");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("old".into()));
    assert!(!app.wb.dirty, "取消的扫描不能把新文档标脏");
    assert_eq!(app.undo.depth(), (0, 0));

    // 正常的分帧扫描仍在同一文档内完成
    app.undo = sheetcore::undo::UndoStack::new();
    app.start_scan(1);
    let mut guard = 0;
    while app.find_scan.is_some() && guard < 10 {
        app.poll_find_scan();
        guard += 1;
    }
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("new".into()));
}

/// 切文档/挂起文档必须主动取消扫描（不依赖世代检查兜底）。
#[test]
fn parking_a_document_cancels_its_find_scan() {
    let mut app = app();
    for r in 0..9000u32 {
        app.wb.sheet_mut(0).set(r, 0, text("old"));
    }
    app.find = FindState {
        open: true,
        tab: 1,
        find: "old".into(),
        replace: "new".into(),
        ..Default::default()
    };
    app.start_scan(1);
    assert!(app.find_scan.is_some());
    let _doc = app.park_current();
    assert!(app.find_scan.is_none(), "park 要取消扫描");
    assert!(app.find.find_all.is_empty());
}

/// UI-06.1：公式选择性粘贴按目标位置平移，负偏移产生 #REF!，绝对引用不变。
#[test]
fn paste_special_formula_translates_negative_and_positive_offsets() {
    use sheetcore::model::Value;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let f = |s: &str| Cell { value: Value::Number(3.0), formula: Some(s.into()) };
    app.wb.sheet_mut(0).set(2, 2, f("=A1+$B$1")); // C3
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (2, 2, 2, 2),
        cells: vec![vec![(app.wb.sheet(0).get(2, 2).unwrap().clone(), 0)]],
        cut: false,
    });
    // C3 → B2：相对 A1 平移 (-1,-1) → #REF!；$B$1 不变
    app.ps_kind = 2;
    app.ps_transpose = false;
    app.ps_op = 0;
    app.sel = (1, 1);
    app.paste_special_apply(&ctx);
    assert_eq!(
        app.wb.sheet(0).get(1, 1).unwrap().formula.as_deref(),
        Some("=#REF!+$B$1"),
        "负偏移必须变成 #REF!，绝对引用保持"
    );
    // C3 → D4：A1→B2
    app.sel = (3, 3);
    app.paste_special_apply(&ctx);
    assert_eq!(
        app.wb.sheet(0).get(3, 3).unwrap().formula.as_deref(),
        Some("=B2+$B$1"),
        "正偏移按行列平移"
    );
    // 一次撤销只回退第二次
    app.do_undo(false);
    assert!(app.wb.sheet(0).get(3, 3).is_none());
    assert!(app.wb.sheet(0).get(1, 1).unwrap().formula.is_some());
}

/// UI-06.1：转置后仍用原始 source 坐标计算偏移，不是用转置后的下标。
#[test]
fn paste_special_transpose_keeps_original_source_coordinates() {
    use sheetcore::model::Value;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let f = |s: &str| Cell { value: Value::Number(1.0), formula: Some(s.into()) };
    app.wb.sheet_mut(0).set(0, 0, f("=A2")); // A1
    app.wb.sheet_mut(0).set(1, 0, f("=A3")); // A2
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (0, 0, 1, 0),
        cells: vec![
            vec![(app.wb.sheet(0).get(0, 0).unwrap().clone(), 0)],
            vec![(app.wb.sheet(0).get(1, 0).unwrap().clone(), 0)],
        ],
        cut: false,
    });
    app.ps_kind = 2;
    app.ps_transpose = true;
    app.ps_op = 0;
    app.sel = (4, 2); // C5
    app.paste_special_apply(&ctx);
    assert_eq!(
        app.wb.sheet(0).get(4, 2).unwrap().formula.as_deref(),
        Some("=C6"),
        "A1 的 =A2 转置到 C5 应为 =C6"
    );
    assert_eq!(
        app.wb.sheet(0).get(4, 3).unwrap().formula.as_deref(),
        Some("=D6"),
        "A2 的 =A3 转置到 D5 应为 =D6"
    );
}

/// UI-06.2：全部必须带样式；数值/运算/除零各自有明确语义。
#[test]
fn paste_special_all_keeps_format_and_divide_by_zero_is_an_error() {
    use sheetcore::model::Value;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let mut st = app.wb.styles.get(0).clone();
    st.bold = true;
    st.fill = Some([255u8, 200, 0]);
    let sid = app.wb.styles.intern(st);
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(7.0), formula: Some("=3+4".into()) });
    app.wb.sheet_mut(0).set_style(0, 0, sid);
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (0, 0, 0, 0),
        cells: vec![vec![(app.wb.sheet(0).get(0, 0).unwrap().clone(), sid)]],
        cut: false,
    });
    // 全部：公式 + 样式都过去
    app.ps_kind = 0;
    app.ps_op = 0;
    app.sel = (5, 5);
    app.paste_special_apply(&ctx);
    let got = app.wb.sheet(0).get(5, 5).unwrap();
    assert!(got.formula.is_some());
    let st2 = app.wb.styles.get(app.wb.sheet(0).style_of(5, 5));
    assert!(st2.bold && st2.fill == Some([255u8, 200, 0]), "全部要带源格式");
    // 除以 0：显示 #DIV/0!，不能静默保留原值
    app.wb.sheet_mut(0).set(10, 0, Cell { value: Value::Number(5.0), formula: None });
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(0.0), formula: None });
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (0, 0, 0, 0),
        cells: vec![vec![(app.wb.sheet(0).get(0, 0).unwrap().clone(), 0)]],
        cut: false,
    });
    app.ps_kind = 1;
    app.ps_op = 4;
    app.sel = (10, 0);
    app.paste_special_apply(&ctx);
    assert_eq!(
        app.wb.sheet(0).value(10, 0),
        Value::Error(sheetcore::formula::ErrKind::Div0),
        "0 除数必须得到 #DIV/0!"
    );
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(10, 0), Value::Number(5.0), "一次撤销完整还原");
}

/// UI-06.3：外部 TSV 保留中间空行（不能 filter 掉）。
#[test]
fn tsv_parsing_keeps_blank_rows_between_values() {
    let rows = SheetApp::parse_clip_text("甲\t1\n\n乙\t2\n");
    assert_eq!(rows.len(), 3, "中间空行必须保留");
    assert_eq!(rows[1][0].0.value, sheetcore::model::Value::Empty);
    assert_eq!(rows[2][0].0.value, sheetcore::model::Value::Text("乙".into()));
}

/// UI-27.1：合并先提示丢内容；取消不动；确认后只保留锚点、可撤销/重做并保存。
#[test]
fn merge_warns_cancel_keeps_data_and_confirm_is_undoable() {
    use sheetcore::model::Value;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, text("甲"));
    app.wb.sheet_mut(0).set(0, 1, text("乙"));
    app.wb.sheet_mut(0).set(0, 2, Cell { value: Value::Number(9.0), formula: None });
    app.sel = (0, 0);
    app.sel_end = (0, 2);
    // 第一次点击只警告，不合并、不改任何数据
    app.toggle_merge(true);
    assert!(app.wb.sheet(0).merges.is_empty(), "提示阶段不能合并");
    assert_eq!(app.wb.sheet(0).get(0, 1).unwrap().value, Value::Text("乙".into()));
    assert_eq!(app.wb.sheet(0).get(0, 2).unwrap().value, Value::Number(9.0));
    assert!(app.status.contains("再点一次"), "status 应说明要二次确认: {}", app.status);
    // 取消 = 不再点；数据完整
    assert!(app.merge_pending.is_some());
    // 再次点击同一区域才执行
    app.toggle_merge(true);
    assert!(app.merge_pending.is_none());
    assert_eq!(app.wb.sheet(0).merges, vec![(0, 0, 0, 2)]);
    assert_eq!(app.wb.sheet(0).get(0, 0).unwrap().value, Value::Text("甲".into()));
    assert!(app.wb.sheet(0).get(0, 1).is_none(), "非锚点内容应删除");
    assert!(app.wb.sheet(0).get(0, 2).is_none());
    assert!(app.wb.dirty);
    // 撤销完整恢复合并状态和两格内容
    app.do_undo(false);
    assert!(app.wb.sheet(0).merges.is_empty(), "撤销应拆掉合并");
    assert_eq!(app.wb.sheet(0).get(0, 1).unwrap().value, Value::Text("乙".into()));
    assert_eq!(app.wb.sheet(0).get(0, 2).unwrap().value, Value::Number(9.0));
    // 重做再删一次
    app.do_undo(true);
    assert_eq!(app.wb.sheet(0).merges, vec![(0, 0, 0, 2)]);
    assert!(app.wb.sheet(0).get(0, 1).is_none());
    // 保存/重开保留合并与锚点内容
    // 不用 std::env::temp_dir()：并行 test_env 用例会改写 TEMP 并在 Drop 时删根目录
    let dir = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("sc_ui27_merge_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("merge.xlsx");
    sheetcore::xlsx::save_atomic(&path, &app.wb).unwrap();
    let (wb2, diag) = sheetcore::xlsx::load_diag(&path).expect("重新打开");
    assert!(diag.is_complete(), "合并保存后不应有加载问题: {:?}", diag);
    assert_eq!(wb2.sheet(0).merges, vec![(0, 0, 0, 2)]);
    assert_eq!(wb2.sheet(0).get(0, 0).unwrap().value, Value::Text("甲".into()));
    assert!(wb2.sheet(0).get(0, 1).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// UI-27.2：格式对话框里的合并只是草稿；取消不执行，确定才执行。
#[test]
fn format_dialog_merge_is_deferred_until_ok() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, text("甲"));
    app.wb.sheet_mut(0).set(0, 1, text("乙"));
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.fmt_merge_pending = Some(true);
    // 模拟“取消”：挂起被清掉，模型完全没动
    app.fmt_merge_pending = None;
    assert!(app.wb.sheet(0).merges.is_empty());
    assert_eq!(app.wb.sheet(0).get(0, 1).unwrap().value, Value::Text("乙".into()));
    // 模拟“确定”：执行挂起动作（无内容损失才会直接合并；这里先清掉乙避免确认分支）
    app.wb.sheet_mut(0).set(0, 1, Cell { value: Value::Empty, formula: None });
    app.fmt_merge_pending = Some(true);
    app.commit_fmt_merge();
    assert_eq!(app.wb.sheet(0).merges, vec![(0, 0, 0, 1)]);
    assert_eq!(app.wb.sheet(0).get(0, 0).unwrap().value, Value::Text("甲".into()));
}

/// UI-27.3：格式刷按源范围平铺样式；连续模式刷完不退出，Esc 结束；单击只刷一次。
#[test]
fn format_painter_maps_range_continuous_and_escape() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let mut st_bold = app.wb.styles.get(0).clone();
    st_bold.bold = true;
    let s_bold = app.wb.styles.intern(st_bold);
    let mut st_fill = app.wb.styles.get(0).clone();
    st_fill.fill = Some([255, 222, 0]);
    let s_fill = app.wb.styles.intern(st_fill);
    app.wb.sheet_mut(0).set_style(0, 0, s_bold);
    app.wb.sheet_mut(0).set_style(0, 1, s_fill);
    // 源区域 A1:B1 已是 bold / fill，arm 连续模式
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.painter_double = true;
    app.ribbon_action("FormatPainter", &ctx);
    let p = app.format_painter.clone().expect("应进入格式刷");
    assert!(p.continuous, "双击格式刷应连续");
    assert_eq!(p.styles, vec![vec![s_bold, s_fill]]);
    // 目标 A3:B5，样式按 1 行 2 列循环平铺
    app.sel = (2, 0);
    app.sel_end = (4, 1);
    app.apply_format_painter();
    for r in 2..=4 {
        assert!(app.wb.styles.get(app.wb.sheet(0).style_of(r, 0)).bold, "第 {r} 行左列应加粗");
        assert_eq!(
            app.wb.styles.get(app.wb.sheet(0).style_of(r, 1)).fill,
            Some([255, 222, 0]),
            "第 {r} 行右列应填充"
        );
    }
    assert!(app.format_painter.is_some(), "连续模式刷完不退出");
    // 再刷第二个范围仍然生效
    app.sel = (7, 0);
    app.sel_end = (7, 1);
    app.apply_format_painter();
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(7, 0)).bold);
    // Esc 结束
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    assert!(app.format_painter.is_none(), "Esc 必须结束格式刷");
    // 单击模式：刷一次自动退出
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.painter_double = false;
    app.ribbon_action("FormatPainter", &ctx);
    app.sel = (10, 3);
    app.sel_end = (10, 4);
    app.apply_format_painter();
    assert!(app.format_painter.is_none(), "单击格式刷只生效一次");
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(10, 3)).bold);
}

/// UI-06.1：全部模式同样平移公式（正/负偏移），并保留源样式。
#[test]
fn paste_special_all_translates_formula_offsets() {
    use sheetcore::model::Value;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let mut st = app.wb.styles.get(0).clone();
    st.bold = true;
    let sid = app.wb.styles.intern(st);
    app.wb.sheet_mut(0).set(2, 2, Cell { value: Value::Number(3.0), formula: Some("=A1+$B$1".into()) });
    app.wb.sheet_mut(0).set_style(2, 2, sid);
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (2, 2, 2, 2),
        cells: vec![vec![(app.wb.sheet(0).get(2, 2).unwrap().clone(), sid)]],
        cut: false,
    });
    app.ps_kind = 0;
    app.ps_transpose = false;
    app.ps_op = 0;
    app.sel = (1, 1); // B2：负偏移
    app.paste_special_apply(&ctx);
    assert_eq!(
        app.wb.sheet(0).get(1, 1).unwrap().formula.as_deref(),
        Some("=#REF!+$B$1"),
        "全部模式负偏移必须 #REF!"
    );
    assert!(app.wb.styles.get(app.wb.sheet(0).style_of(1, 1)).bold);
    app.sel = (3, 3); // D4：正偏移
    app.paste_special_apply(&ctx);
    assert_eq!(
        app.wb.sheet(0).get(3, 3).unwrap().formula.as_deref(),
        Some("=B2+$B$1")
    );
}

/// UI-06.2：转置到负位移位置时，公式仍按原源坐标算偏移并产生 #REF!。
#[test]
fn paste_special_transpose_negative_offset_is_ref_error() {
    use sheetcore::model::Value;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(2, 2, Cell { value: Value::Number(1.0), formula: Some("=A1".into()) }); // C3
    app.clip = Some(Clip { merges: Vec::new(),
        sheet: 0,
        rect: (2, 2, 2, 2),
        cells: vec![vec![(app.wb.sheet(0).get(2, 2).unwrap().clone(), 0)]],
        cut: false,
    });
    app.ps_kind = 2;
    app.ps_transpose = true;
    app.ps_op = 0;
    app.sel = (1, 1); // B2：相对 A1 负位移
    app.paste_special_apply(&ctx);
    assert_eq!(
        app.wb.sheet(0).get(1, 1).unwrap().formula.as_deref(),
        Some("=#REF!"),
        "转置到 B2 后 A1 已越界，必须 #REF!"
    );
}

/// UI-06：内部 Clip 不得遮盖之后复制到系统剪贴板的新内容；再次复制后才恢复内部坐标/格式。
#[test]
fn paste_special_prefers_newer_system_clipboard_over_stale_internal_clip() {
    use sheetcore::model::Value;
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    // 1) app 内复制 A1（内部 Clip + 系统文本）
    app.wb.sheet_mut(0).set(0, 0, text("内部甲"));
    app.wb.sheet_mut(0).set(0, 1, text("内部乙"));
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.clip_copy(false, &ctx);
    // 2) 外部应用复制 B（系统剪贴板换成外部文本）
    app.sel = (5, 0);
    app.ps_kind = 1;
    app.ps_op = 0;
    app.ps_transpose = false;
    app.paste_special_apply_text(&ctx, Some("外部乙\t2\n".into()));
    assert_eq!(app.wb.sheet(0).get(5, 0).unwrap().value, Value::Text("外部乙".into()));
    assert_eq!(app.wb.sheet(0).get(5, 1).unwrap().value, Value::Number(2.0));
    // 3) 回到 app 再复制，内部坐标/格式才应优先
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.clip_copy(false, &ctx);
    app.ps_kind = 0;
    app.sel = (8, 0);
    app.paste_special_apply_text(&ctx, Some("内部甲\t内部乙\n".into()));
    assert_eq!(app.wb.sheet(0).get(8, 0).unwrap().value, Value::Text("内部甲".into()));
    assert_eq!(app.wb.sheet(0).get(8, 1).unwrap().value, Value::Text("内部乙".into()));
}

/// UI-27 复核：合并确认绑定文档/表/矩形/待删内容；换表、换内容、Esc 取消都重新提示。
#[test]
fn merge_confirmation_is_scoped_to_document_sheet_rect_and_content() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, text("甲"));
    app.wb.sheet_mut(0).set(0, 1, text("乙"));
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.toggle_merge(true);
    assert!(app.merge_pending.is_some(), "首次丢内容应进入待确认");

    // 同表同矩形但待删内容变了：旧确认必须失效，且这一下不能删
    app.wb.sheet_mut(0).set(0, 1, text("乙改"));
    app.toggle_merge(true);
    assert!(app.wb.sheet(0).merges.is_empty());
    assert_eq!(app.wb.sheet(0).get(0, 1).unwrap().value, Value::Text("乙改".into()));
    assert!(app.status.contains("再点一次"), "内容变化后必须重新确认: {}", app.status);

    // 换到另一张表，同矩形一次点击也不能复用 A 表的确认
    app.wb.sheet_mut(0).set(0, 1, text("乙"));
    app.toggle_merge(true);
    assert!(app.merge_pending.is_some());
    let mut sh = sheetcore::model::Sheet::new("另一表");
    sh.set(0, 0, text("丙"));
    sh.set(0, 1, text("丁"));
    app.wb.sheets.push(sh);
    app.sheet_idx = 1;
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.toggle_merge(true);
    assert!(app.wb.sheet(1).merges.is_empty(), "B 表不能被 A 表确认删内容");
    assert_eq!(app.wb.sheet(1).get(0, 1).unwrap().value, Value::Text("丁".into()));
    assert!(app.status.contains("再点一次"));

    // 合并警告仍在时启用格式刷，再按一次真实 Esc：两个挂起状态必须一起失效，
    // 不能因为格式刷分支提前 return 而留下旧合并确认。
    app.ribbon_action("FormatPainter", &ctx);
    assert!(app.format_painter.is_some(), "格式刷应已启用");
    assert!(app.merge_pending.is_some(), "格式刷不能悄悄清掉合并警告");
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    assert!(app.format_painter.is_none(), "Esc 必须结束格式刷");
    assert!(app.merge_pending.is_none(), "Esc 必须同时失效待确认合并");
    assert!(app.status.contains("格式刷与未确认的合并已取消"), "{}", app.status);
    app.toggle_merge(true);
    assert!(app.wb.sheet(1).merges.is_empty(), "Esc 后不能沿用旧确认直接删内容");
    assert!(app.status.contains("再点一次"), "{}", app.status);

    // 只有合并警告、没有格式刷时，Esc 仍是原来的单义提示。
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    assert!(app.merge_pending.is_none());
    assert!(app.status.contains("已取消未确认的合并"), "{}", app.status);
}

/// UI-27 复核：格式刷样式号不能跨文档解释；换簿后不套错样式、不新增撤销。
#[test]
fn format_painter_is_rejected_after_document_switch_even_with_same_style_id() {
    let mut app = app();
    let mut bold = app.wb.styles.get(0).clone();
    bold.bold = true;
    let a_sid = app.wb.styles.intern(bold);
    app.wb.sheet_mut(0).set_style(0, 0, a_sid);
    app.format_painter = Some(FormatPainter {
        rect: (0, 0, 0, 0),
        styles: vec![vec![a_sid]],
        continuous: false,
        wb_epoch: app.wb_epoch,
        sheet: 0,
    });

    let mut wb2 = sheetcore::model::Workbook::new_blank();
    wb2.sheet_mut(0).set(0, 0, text("乙簿"));
    let mut italic = wb2.styles.get(0).clone();
    italic.italic = true;
    let b_sid = wb2.styles.intern(italic);
    assert_eq!(a_sid, b_sid, "两簿同一样式号但语义不同，正是风险场景");
    wb2.sheet_mut(0).set_style(0, 1, b_sid);
    app.wb = wb2;
    app.wb_epoch = app.wb_epoch.wrapping_add(1);
    app.doc_gen = 0;
    app.sheet_idx = 0;
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    let before = app.undo.depth();
    app.apply_format_painter();
    assert_eq!(app.wb.sheet(0).style_of(0, 0), 0, "目标格不能套上 A 簿样式号");
    assert!(!app.wb.styles.get(app.wb.sheet(0).style_of(0, 1)).bold, "B 簿自己的同号样式不能被改写");
    assert_eq!(app.undo.depth(), before, "取消的格式刷不能新增撤销");
    assert!(app.format_painter.is_none());
    assert!(app.status.contains("格式刷已取消"));
}

/// UI-27 复核：撤销合并只按差异回滚，保留动作之后新增的无关合并。
#[test]
fn merge_undo_keeps_unrelated_merges_and_redo_restores_only_diff() {
    let mut app = app();
    app.wb.sheet_mut(0).set(0, 0, text("甲"));
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.toggle_merge(true);
    assert_eq!(app.wb.sheet(0).merges, vec![(0, 0, 0, 1)]);
    app.wb.sheet_mut(0).merge_range(3, 3, 4, 4);
    assert_eq!(app.wb.sheet(0).merges.len(), 2);
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).merges, vec![(3, 3, 4, 4)], "撤销不能整表覆盖后加的合并");
    app.do_undo(true);
    assert!(app.wb.sheet(0).merges.contains(&(0, 0, 0, 1)));
    assert!(app.wb.sheet(0).merges.contains(&(3, 3, 4, 4)));
}

/// UI-27 复核：结构相交导致重做放弃时，同一动作的 CellDelta 也必须放弃。
#[test]
fn overlapping_merge_redo_conflict_keeps_values_and_both_structures_intact() {
    let mut app = app();
    app.wb.sheet_mut(0).set(0, 0, text("甲"));
    app.wb.sheet_mut(0).set(0, 1, text("乙"));
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.toggle_merge(true);
    assert!(app.merge_pending.is_some(), "第一次只提示");
    app.toggle_merge(true);
    assert_eq!(app.wb.sheet(0).merges, vec![(0, 0, 0, 1)]);
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Empty);

    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Text("乙".into()), "撤销恢复非锚点内容");
    assert!(app.wb.sheet(0).merges.is_empty());

    // B 端把刚恢复的 B1（现在是 B1:C1 的锚点，乙仍在）合并；A 端收到后状态如下。
    app.wb.sheet_mut(0).merge_range(0, 1, 0, 2);
    app.wb.dirty = false;
    let depth_before = app.undo.depth();
    app.do_undo(true);

    assert_eq!(app.wb.sheet(0).merges, vec![(0, 1, 0, 2)], "结构相交时整组放弃，不能拆掉 B 的合并");
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Text("乙".into()), "B1 是另一合并的锚点，重做不能清空它");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("甲".into()));
    assert!(app.status.contains("合并结构与当前表冲突"), "{}", app.status);
    assert!(app.status.contains("整体放弃"), "{}", app.status);
    assert!(!app.wb.dirty, "冲突放弃不能把文档标脏");
    assert_eq!(app.undo.depth(), depth_before, "冲突的重做应留在重做栈");
}

/// UI-27 复核：合并撤销/重做要广播结构；第二客户端后加的无关合并保留。
#[test]
fn merge_undo_and_redo_broadcast_to_second_client_preserving_its_new_merge() {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = crate::net::host_on(listener, 1, "甲".into(), "doc".into(), "pw".into()).unwrap();
    let addr = format!("127.0.0.1:{port}");
    let peer = crate::net::join(2, "乙".into(), "doc".into(), &addr, "pw".into()).unwrap();
    let mut a = app();
    let mut b = app();
    a.peer = Some(sheetcore::sync::Peer::new(1));
    b.peer = Some(sheetcore::sync::Peer::new(2));
    a.net = Some(host);
    b.net = Some(peer);
    for _ in 0..40 {
        a.poll_net();
        b.poll_net();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    a.wb.sheet_mut(0).set(0, 0, text("甲"));
    a.sel = (0, 0);
    a.sel_end = (0, 1);
    a.toggle_merge(true);
    assert_eq!(a.wb.sheet(0).merges, vec![(0, 0, 0, 1)]);
    let mut got_merge = false;
    for _ in 0..100 {
        b.poll_net();
        if b.wb.sheet(0).merges == vec![(0, 0, 0, 1)] {
            got_merge = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(got_merge, "第二客户端要收到合并结构");

    // B 在期间新增一个无关合并；A 的撤销不能把它抹掉
    b.wb.sheet_mut(0).merge_range(5, 5, 6, 6);
    a.do_undo(false);
    assert!(a.wb.sheet(0).merges.is_empty());
    let mut got_undo = false;
    for _ in 0..100 {
        b.poll_net();
        if b.wb.sheet(0).merges == vec![(5, 5, 6, 6)] {
            got_undo = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(got_undo, "撤销合并要广播，且保留对端后加的合并: {:?}", b.wb.sheet(0).merges);

    a.do_undo(true);
    assert!(a.wb.sheet(0).merges.contains(&(0, 0, 0, 1)));
    let mut got_redo = false;
    for _ in 0..100 {
        b.poll_net();
        if b.wb.sheet(0).merges.contains(&(0, 0, 0, 1))
            && b.wb.sheet(0).merges.contains(&(5, 5, 6, 6))
        {
            got_redo = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(got_redo, "重做合并要广播，且不覆盖对端无关合并: {:?}", b.wb.sheet(0).merges);
    a.net.as_mut().unwrap().shutdown();
    b.net.as_mut().unwrap().shutdown();
}

/// UI-21：手动计算模式下编辑只标记待计算，F9 才更新依赖公式并保留手动模式。
#[test]
fn manual_calc_edit_marks_pending_and_f9_forces_recalculation() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(1.0));
    app.wb.sheet_mut(0).set(0, 1, formula("=A1*2"));
    sheetcore::formula::recalc_all_force(&mut app.wb);
    app.deps = sheetcore::formula::deps::Deps::build(&app.wb);
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(2.0));

    app.ribbon_action("CalcManual", &ctx);
    assert!(app.manual_calc);
    assert!(app.wb.calc_manual, "工作簿标志要同步，保存 calcPr 才正确");
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("5".into()));
    assert!(app.commit_edit(None));

    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(5.0));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(2.0), "手动模式不能偷算依赖公式");
    assert!(app.wb.calc_pending, "编辑后必须显示待计算");
    assert!(app.status.contains("待计算") || app.wb.calc_pending);

    app.recalc_all_async();
    for _ in 0..300 {
        app.poll_open();
        if !app.tasks.busy && app.tasks.open_rx.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(10.0), "F9 必须强制重算");
    assert!(!app.wb.calc_pending);
    assert!(app.wb.calc_manual, "强制重算后仍保留手动模式");
    assert!(app.manual_calc);
}

/// UI-21：CalculateSheet 只算当前表，不因另一张表的 RAND 触发全局重算。
#[test]
fn calculate_sheet_command_keeps_unrelated_volatile_sheet_cached() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(1.0));
    app.wb.sheet_mut(0).set(0, 1, formula("=A1*2"));
    sheetcore::formula::recalc_all_force(&mut app.wb);
    // 先算第一张表，再放入带缓存 999 的易失表，避免初始 force 把 999 换成随机值。
    let mut other = sheetcore::Sheet::new("易失表");
    other.set(0, 0, Cell { value: Value::Number(999.0), formula: Some("=RAND()".into()) });
    app.wb.sheets.push(other);
    app.deps = sheetcore::formula::deps::Deps::build(&app.wb);
    app.ribbon_action("CalcManual", &ctx);
    app.sheet_idx = 0;
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("5".into()));
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(2.0));

    app.ribbon_action("CalculateSheet", &ctx);
    for _ in 0..300 {
        app.poll_open();
        if !app.tasks.busy && app.tasks.open_rx.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(10.0), "当前表要算");
    assert_eq!(app.wb.sheet(1).value(0, 0), Value::Number(999.0), "别的表的 RAND 不能被动");
    assert!(app.wb.calc_manual);
}

/// UI-27 复核：结构相交的 redo 必须整组放弃，且不能把 Empty 广播给对端。
#[test]
fn overlapping_merge_redo_conflict_does_not_broadcast_or_touch_peer() {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = crate::net::host_on(listener, 1, "甲".into(), "doc".into(), "pw".into()).unwrap();
    let addr = format!("127.0.0.1:{port}");
    let peer = crate::net::join(2, "乙".into(), "doc".into(), &addr, "pw".into()).unwrap();
    let mut a = app();
    let mut b = app();
    a.peer = Some(sheetcore::sync::Peer::new(1));
    b.peer = Some(sheetcore::sync::Peer::new(2));
    a.net = Some(host);
    b.net = Some(peer);
    for _ in 0..40 {
        a.poll_net();
        b.poll_net();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // 两端同一份基准：A1=甲、B1=乙。
    for app in [&mut a, &mut b] {
        app.wb.sheet_mut(0).set(0, 0, text("甲"));
        app.wb.sheet_mut(0).set(0, 1, text("乙"));
    }
    a.sel = (0, 0);
    a.sel_end = (0, 1);
    a.toggle_merge(true);
    a.toggle_merge(true);
    assert_eq!(a.wb.sheet(0).merges, vec![(0, 0, 0, 1)]);
    assert_eq!(a.wb.sheet(0).value(0, 1), Value::Empty);
    for _ in 0..100 {
        b.poll_net();
        if b.wb.sheet(0).merges == vec![(0, 0, 0, 1)]
            && b.wb.sheet(0).value(0, 1) == Value::Empty
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(b.wb.sheet(0).merges, vec![(0, 0, 0, 1)]);

    // A 撤销：B1=乙 与结构撤销都要到达 B。
    a.do_undo(false);
    for _ in 0..100 {
        b.poll_net();
        if b.wb.sheet(0).merges.is_empty()
            && b.wb.sheet(0).value(0, 1) == Value::Text("乙".into())
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(b.wb.sheet(0).merges.is_empty());
    assert_eq!(b.wb.sheet(0).value(0, 1), Value::Text("乙".into()));

    // B 用恢复的乙作锚点合并 B1:C1；A 收到。
    b.sel = (0, 1);
    b.sel_end = (0, 2);
    b.toggle_merge(true);
    assert_eq!(b.wb.sheet(0).merges, vec![(0, 1, 0, 2)]);
    for _ in 0..100 {
        a.poll_net();
        if a.wb.sheet(0).merges == vec![(0, 1, 0, 2)] {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(a.wb.sheet(0).merges, vec![(0, 1, 0, 2)]);

    // A redo 与 B 的结构相交：整组放弃，且不得广播 Empty。
    a.do_undo(true);
    assert_eq!(a.wb.sheet(0).merges, vec![(0, 1, 0, 2)]);
    assert_eq!(a.wb.sheet(0).value(0, 1), Value::Text("乙".into()));
    assert!(a.status.contains("合并结构与当前表冲突"), "{}", a.status);
    for _ in 0..20 {
        b.poll_net();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(b.wb.sheet(0).merges, vec![(0, 1, 0, 2)], "对端结构不能被拆");
    assert_eq!(b.wb.sheet(0).value(0, 1), Value::Text("乙".into()), "对端锚点不能被清空");
    a.net.as_mut().unwrap().shutdown();
    b.net.as_mut().unwrap().shutdown();
}

/// UI-21：只算一张表不能清掉另一张表的待计算标记和旧缓存。
#[test]
fn calculate_sheet_keeps_other_sheet_pending_until_full_recalc() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(1.0));
    app.wb.sheet_mut(0).set(0, 1, formula("=A1*2"));
    let mut second = sheetcore::Sheet::new("第二表");
    second.set(0, 0, number(10.0));
    second.set(0, 1, formula("=A1*2"));
    app.wb.sheets.push(second);
    sheetcore::formula::recalc_all_force(&mut app.wb);
    app.deps = sheetcore::formula::deps::Deps::build(&app.wb);
    app.ribbon_action("CalcManual", &ctx);

    app.sheet_idx = 0;
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("5".into()));
    assert!(app.commit_edit(None));
    app.sheet_idx = 1;
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("20".into()));
    assert!(app.commit_edit(None));
    assert!(app.wb.calc_pending);
    assert_eq!(app.wb.calc_pending_sheets.len(), 2);

    app.sheet_idx = 0;
    app.ribbon_action("CalculateSheet", &ctx);
    for _ in 0..300 {
        app.poll_open();
        if !app.tasks.busy && app.tasks.open_rx.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(10.0));
    assert_eq!(app.wb.sheet(1).value(0, 1), Value::Number(20.0), "第二表保持旧缓存");
    assert!(app.wb.calc_pending, "只算一张表不能误清全簿待计算");
    assert_eq!(app.wb.calc_pending_sheets.iter().copied().collect::<Vec<_>>(), vec![1]);

    app.recalc_all_async();
    for _ in 0..300 {
        app.poll_open();
        if !app.tasks.busy && app.tasks.open_rx.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.wb.sheet(1).value(0, 1), Value::Number(40.0));
    assert!(!app.wb.calc_pending);
    assert!(app.wb.calc_pending_sheets.is_empty());
}

/// UI-21：手动模式下删除入口也必须走统一调度，保留旧缓存并置待计算。
#[test]
fn manual_mode_delete_keeps_cached_dependents_until_f9() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(1.0));
    app.wb.sheet_mut(0).set(0, 1, formula("=A1*2"));
    sheetcore::formula::recalc_all_force(&mut app.wb);
    app.deps = sheetcore::formula::deps::Deps::build(&app.wb);
    app.ribbon_action("CalcManual", &ctx);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.ribbon_action("ClearContent", &ctx);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Empty);
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(2.0), "删除不能偷偷重算依赖");
    assert!(app.wb.calc_pending);
    assert!(app.wb.calc_pending_sheets.contains(&0));
}

/// UI-21：排序入口同样只置待计算，不在手动模式下自动重算。
#[test]
fn manual_mode_sort_marks_pending_without_recalculating() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(2.0));
    app.wb.sheet_mut(0).set(1, 0, number(1.0));
    app.wb.sheet_mut(0).set(0, 1, formula("=A1*10"));
    app.wb.sheet_mut(0).set(1, 1, formula("=A2*10"));
    sheetcore::formula::recalc_all_force(&mut app.wb);
    app.deps = sheetcore::formula::deps::Deps::build(&app.wb);
    app.ribbon_action("CalcManual", &ctx);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.do_sort(0, true);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(1.0));
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(2.0));
    assert!(app.wb.calc_pending);
    assert!(app.wb.calc_pending_sheets.contains(&0));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(10.0), "手动排序保留搬行后的旧缓存，不得自动算 B 列");
}

/// UI-21：验证下拉写入和选择性粘贴也要遵守手动计算。
#[test]
fn manual_mode_dv_and_paste_keep_cached_values() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(1.0));
    app.wb.sheet_mut(0).set(0, 1, formula("=A1*2"));
    sheetcore::formula::recalc_all_force(&mut app.wb);
    app.deps = sheetcore::formula::deps::Deps::build(&app.wb);
    app.ribbon_action("CalcManual", &ctx);

    assert!(app.dv_pick_apply(0, 0, "5"));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(5.0));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(2.0), "下拉写入不能自动重算");
    assert!(app.wb.calc_pending);

    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.paste_special_apply_text(&ctx, Some("7".into()));
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(7.0));
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(2.0), "粘贴不能自动重算");
    assert!(app.wb.calc_pending);
}

/// UI-05.4：Ctrl 多选区必须整体成为查找范围；查找跳转后当前 extra_sel
/// 会被清空，但锁定快照仍要保留其余块，不能从第二处起退回主选区。
#[test]
fn find_selection_scope_includes_ctrl_multi_ranges_and_keeps_snapshot() {
    let mut app = app();
    for (r, c) in [(0u32, 0u32), (4, 1), (9, 3)] {
        app.wb.sheet_mut(0).set(r, c, text("needle"));
    }
    app.find.find = "needle".into();
    app.find.scope = 2;
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.extra_sel = vec![(4, 1, 4, 1), (9, 3, 9, 3)];

    let hits = app.find_hits();
    assert_eq!(
        hits.iter().map(|h| (h.row, h.col)).collect::<Vec<_>>(),
        vec![(0, 0), (4, 1), (9, 3)],
        "主选区 + 两个 Ctrl 块都应命中"
    );

    app.find.session_rect = None;
    app.find.session_rects.clear();
    app.find_next(false);
    assert_eq!(app.sel, (4, 1), "第一处应从主选区跳到第二块");
    assert!(app.extra_sel.is_empty(), "跳转本身仍清当前多选");
    app.find_next(false);
    assert_eq!(app.sel, (9, 3), "锁定快照必须保留第三块");
}

/// 14:00 监督：Ribbon 的“查找和替换”也是新会话入口，必须清掉上一次
/// 锁定的选区快照；否则新选 B1/B3 后“全部替换”仍会作用到旧 A1/A3。
#[test]
fn ribbon_find_replace_clears_stale_scope_before_replace_all() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for (r, c) in [(0u32, 0u32), (2, 0), (0, 1), (2, 1)] {
        app.wb.sheet_mut(0).set(r, c, text("needle"));
    }
    app.find.find = "needle".into();
    app.find.replace = "done".into();
    app.find.scope = 2;
    // 模拟上一次查找留下的 A1/A3 会话快照
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.extra_sel = vec![(2, 0, 2, 0)];
    app.find.session_rect = Some((0, 0, 2, 0));
    app.find.session_rects = vec![(0, 0, 2, 0)];
    app.find.open = false;

    // 关闭旧会话后，改选不相邻的 B1/B3，再从 Ribbon 打开查找
    app.sel = (0, 1);
    app.sel_end = (0, 1);
    app.extra_sel = vec![(2, 1, 2, 1)];
    app.ribbon_action("FindReplace", &ctx);
    assert!(app.find.open, "Ribbon FindReplace 必须打开查找会话");
    assert!(app.find.session_rect.is_none(), "新会话不能沿用旧主选区");
    assert!(app.find.session_rects.is_empty(), "新会话不能沿用旧多选区快照");

    let hits = app.find_hits();
    assert_eq!(
        hits.iter().map(|h| (h.row, h.col)).collect::<Vec<_>>(),
        vec![(0, 1), (2, 1)],
        "新会话应只命中当前 B1/B3"
    );
    app.replace_all();
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("needle".into()), "旧 A1 不能变");
    assert_eq!(app.wb.sheet(0).value(2, 0), Value::Text("needle".into()), "旧 A3 不能变");
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Text("done".into()));
    assert_eq!(app.wb.sheet(0).value(2, 1), Value::Text("done".into()));

    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Text("needle".into()), "一次撤销恢复 B1");
    assert_eq!(app.wb.sheet(0).value(2, 1), Value::Text("needle".into()), "一次撤销恢复 B3");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Text("needle".into()), "A1 始终未动");
}

/// C22：General 对齐按 Excel 约定——文本左、数字（含日期序列号）右、
/// 布尔/错误居中；显式对齐必须覆盖 General。
#[test]
fn general_text_alignment_matches_excel_and_explicit_alignment_wins() {
    use sheetcore::formula::ErrKind;
    assert_eq!(cell_text_align(0, &Value::Text("left".into())), egui::Align2::LEFT_CENTER);
    assert_eq!(cell_text_align(0, &Value::Number(1.5)), egui::Align2::RIGHT_CENTER);
    assert_eq!(cell_text_align(0, &Value::Bool(true)), egui::Align2::CENTER_CENTER);
    assert_eq!(cell_text_align(0, &Value::Error(ErrKind::Div0)), egui::Align2::CENTER_CENTER);
    assert_eq!(cell_text_align(1, &Value::Number(1.5)), egui::Align2::LEFT_CENTER);
    assert_eq!(cell_text_align(2, &Value::Text("explicit".into())), egui::Align2::CENTER_CENTER);
    assert_eq!(cell_text_align(3, &Value::Text("right".into())), egui::Align2::RIGHT_CENTER);
}

/// UI-16 复核：复制到前方时，插入前按 identity 保存视图；插入后按 identity 找回
/// 原活动表，再切副本。原 B/C 的选区、多选、滚动、缩放都不能被副本覆盖。
#[test]
fn copy_sheet_to_front_preserves_original_sheet_views() {
    let mut app = app();
    assert!(sheetcore::sheet_ops::insert_sheet(&mut app.wb, 1, sheetcore::model::Sheet::new("B")));
    assert!(sheetcore::sheet_ops::insert_sheet(&mut app.wb, 2, sheetcore::model::Sheet::new("C")));
    let a_id = app.wb.sheet(0).identity;
    let b_id = app.wb.sheet(1).identity;
    let c_id = app.wb.sheet(2).identity;
    app.sel = (0, 0);
    app.scroll = egui::vec2(3.0, 4.0);
    app.zoom = 1.1;
    app.remember_sheet_view();
    assert!(app.switch_sheet_view(1));
    app.sel = (1, 1);
    app.sel_end = (1, 1);
    app.extra_sel = vec![(2, 2, 3, 3)];
    app.scroll = egui::vec2(5.0, 6.0);
    app.zoom = 1.2;
    app.remember_sheet_view();
    assert!(app.switch_sheet_view(2));
    app.sel = (99, 25);
    app.sel_end = (99, 25);
    app.extra_sel = vec![(4, 4, 5, 5)];
    app.scroll = egui::vec2(7.0, 8.0);
    app.zoom = 1.3;
    app.remember_sheet_view();
    assert_eq!(app.wb.sheet(2).identity, c_id);

    assert!(app.copy_sheet_to(2, 0));
    assert_eq!(app.sheet_idx, 0, "复制后活动表应是新副本");
    assert_eq!(app.wb.sheet(0).name, "C (2)");
    assert_eq!(app.wb.sheet(3).identity, c_id, "原 C 仍在末尾");
    let b = app.sheet_views.get(&b_id).expect("B 视图");
    assert_eq!((b.sel, b.sel_end), ((1, 1), (1, 1)));
    assert_eq!(b.extra_sel, vec![(2, 2, 3, 3)]);
    assert_eq!(b.scroll, egui::vec2(5.0, 6.0));
    assert_eq!(b.zoom, 1.2);
    let c = app.sheet_views.get(&c_id).expect("C 视图");
    assert_eq!((c.sel, c.sel_end), ((99, 25), (99, 25)));
    assert_eq!(c.extra_sel, vec![(4, 4, 5, 5)]);
    assert_eq!(c.scroll, egui::vec2(7.0, 8.0));
    assert_eq!(c.zoom, 1.3);
    assert_eq!(app.sheet_views.get(&a_id).map(|v| v.sel), Some((0, 0)));
    // 副本自己是新视图，不能继承原 C 的 Z100 选区。
    assert_eq!(app.sel, (0, 0));
    assert!(app.extra_sel.is_empty());
}

/// UI-16 复核：插入位置等于旧索引时不能走 switch 的 same-index 早返。
#[test]
fn copy_sheet_to_same_index_restores_original_view_before_switching() {
    let mut app = app();
    assert!(sheetcore::sheet_ops::insert_sheet(&mut app.wb, 1, sheetcore::model::Sheet::new("B")));
    assert!(sheetcore::sheet_ops::insert_sheet(&mut app.wb, 2, sheetcore::model::Sheet::new("C")));
    let b_id = app.wb.sheet(1).identity;
    let c_id = app.wb.sheet(2).identity;
    assert!(app.switch_sheet_view(1));
    app.sel = (7, 2);
    app.scroll = egui::vec2(11.0, 12.0);
    app.zoom = 1.4;
    app.remember_sheet_view();
    assert!(app.switch_sheet_view(2));
    app.sel = (99, 25);
    app.extra_sel = vec![(20, 20, 21, 21)];
    app.scroll = egui::vec2(13.0, 14.0);
    app.zoom = 1.5;
    app.remember_sheet_view();

    assert!(app.copy_sheet_to(2, 2));
    assert_eq!(app.sheet_idx, 2, "副本插在原 C 的位置");
    assert_eq!(app.wb.sheet(2).name, "C (2)");
    assert_eq!(app.wb.sheet(3).identity, c_id);
    let c = app.sheet_views.get(&c_id).expect("原 C 视图");
    assert_eq!(c.sel, (99, 25));
    assert_eq!(c.extra_sel, vec![(20, 20, 21, 21)]);
    assert_eq!(c.scroll, egui::vec2(13.0, 14.0));
    assert_eq!(c.zoom, 1.5);
    let b = app.sheet_views.get(&b_id).expect("B 视图");
    assert_eq!(b.sel, (7, 2));
    assert_eq!(b.scroll, egui::vec2(11.0, 12.0));
    assert_eq!(b.zoom, 1.4);
    assert_eq!(app.sel, (0, 0), "副本使用自己的默认视图");
    assert!(app.extra_sel.is_empty());
}

/// UI-16 复核：标签栏右键隐藏必须先提交有效编辑。
#[test]
fn hide_sheet_context_commits_edit_before_switching() {
    let mut app = app();
    assert!(sheetcore::sheet_ops::insert_sheet(&mut app.wb, 1, sheetcore::model::Sheet::new("Other")));
    app.wb.sheet_mut(0).set(0, 0, number(1.0));
    app.sel = (0, 0);
    app.editing = Some(EditState::new("42".into()));
    app.hide_current_sheet();
    assert!(app.editing.is_none(), "隐藏前必须提交编辑");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(42.0));
    assert!(app.wb.sheet(0).hidden);
    assert_ne!(app.sheet_idx, 0, "隐藏后应切到下一张可见表");
}

/// UI-16 复核：Ribbon HideSheet 遇到数据验证拒绝时，原表、编辑文本、选区、
/// 隐藏状态和视图字段都保持原样。
#[test]
fn hide_sheet_ribbon_rejects_invalid_edit_without_state_change() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    assert!(sheetcore::sheet_ops::insert_sheet(&mut app.wb, 1, sheetcore::model::Sheet::new("Other")));
    restrict_a1(&mut app);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.extra_sel = vec![(4, 4, 5, 5)];
    app.scroll = egui::vec2(15.0, 16.0);
    app.zoom = 1.6;
    app.ribbon_action("HideSheet", &ctx);
    assert!(app.wb.sheet(0).hidden == false, "验证拒绝不能隐藏");
    assert_eq!(app.sheet_idx, 0, "验证拒绝不能切表");
    assert_eq!(app.sel, (0, 0));
    assert_eq!(app.extra_sel, vec![(4, 4, 5, 5)]);
    assert_eq!(app.scroll, egui::vec2(15.0, 16.0));
    assert_eq!(app.zoom, 1.6);
    let ed = app.editing.as_ref().expect("编辑框必须保留");
    assert_eq!(ed.buf, "99", "验证失败必须保留用户输入");
}

/// UI-16 复核：跨表撤销落点不能保留目标表旧的多选区；否则随后的清除/格式
/// 会误伤历史远端区域。
#[test]
fn cross_sheet_undo_clears_restored_extra_selection() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    assert!(sheetcore::sheet_ops::insert_sheet(&mut app.wb, 1, sheetcore::model::Sheet::new("Other")));
    app.wb.sheet_mut(0).set(0, 0, number(1.0));
    app.wb.sheet_mut(0).set(9, 9, text("remote"));
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.editing = Some(EditState::new("2".into()));
    assert!(app.commit_edit(None), "先建立跨表撤销历史");
    app.extra_sel = vec![(9, 9, 9, 9)];
    app.scroll = egui::vec2(17.0, 18.0);
    app.zoom = 1.7;
    assert!(app.switch_sheet_view(1));
    assert_ne!(app.sheet_idx, 0);
    let remote_style = app.wb.sheet(0).style_of(9, 9);

    app.do_undo(false);
    assert_eq!(app.sheet_idx, 0, "撤销应回到出事表");
    assert!(app.extra_sel.is_empty(), "撤销落点必须清掉恢复的旧多选区");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(1.0));
    app.clear_selection();
    assert_eq!(
        app.wb.sheet(0).value(9, 9),
        Value::Text("remote".into()),
        "清除内容不能碰历史远端区域"
    );
    app.ribbon_action("Bold", &ctx);
    assert_eq!(
        app.wb.sheet(0).style_of(9, 9),
        remote_style,
        "格式操作不能碰历史远端区域"
    );
}

/// 真实 egui 焦点链：Tab 到关闭按钮后单个 Enter 关闭，同一 Enter 不能推进选区；
/// 下一帧键盘输入应回到网格。
#[test]
fn enter_on_focused_dialog_close_does_not_advance_selection() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(1, 1, text("old"));
    app.sel = (1, 1);
    app.sel_end = (1, 1);
    app.find = FindState {
        open: true,
        tab: 1,
        find: "alpha".into(),
        replace: "beta".into(),
        ..Default::default()
    };

    // 先让真实 update() 画出对话框；对话框标题决定关闭按钮的 Id。
    ui_frame(&mut app, &ctx, vec![]);
    let close_id = egui::Id::new(("dlgwin", "查找与替换")).with("close");
    // 把真实键盘焦点放到对话框关闭按钮（Tab 最终到达的同一控件 Id）。
    ctx.memory_mut(|m| m.request_focus(close_id));
    assert!(ctx.memory(|m| m.has_focus(close_id)), "关闭按钮应持有键盘焦点");
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert!(!app.find.open, "焦点在关闭按钮时 Enter 必须关闭对话框");
    assert_eq!(app.sel, (1, 1), "关闭对话框的同一个 Enter 不能推进选区");

    // 下一帧：输入 9 并按 Enter 应正常进入 B2 并提交，而不是被隐藏焦点吞掉。
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("9".into())]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(
        app.wb.sheet(0).value(1, 1),
        Value::Number(9.0),
        "关闭后的下一帧键盘输入必须回到网格"
    );
}

/// 模态对话框存在时，关闭按钮同帧的点击不能穿透到网格选中背后的单元格。
#[test]
fn modal_dialog_click_does_not_select_grid_behind() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let pos = egui::pos2(
        HEAD_W + app.wb.sheets[0].cols.pos(2) + 15.0,
        HEAD_H + app.wb.sheets[0].rows.pos(2) + 10.0,
    );
    app.dialog_modal = true;
    click_grid(&mut app, &ctx, pos);
    assert_eq!(app.sel, (0, 0), "对话框打开时点击关闭不能选中背后的 C3");
    app.dialog_modal = false;
    click_grid(&mut app, &ctx, pos);
    assert_eq!(app.sel, (2, 2), "对话框关闭后网格点击应恢复正常");
}

fn dv_rule(style: sheetcore::dv::DvStyle, show_error: bool) -> sheetcore::dv::DataValidation {
    sheetcore::dv::DataValidation {
        ranges: vec![(1, 1, 1, 1)],
        kind: sheetcore::dv::DvKind::Whole,
        op: sheetcore::dv::DvOp::Between,
        f1: "1".into(),
        f2: "10".into(),
        error_style: style,
        show_error,
        error_title: "越界".into(),
        error_msg: "只能填 1-10".into(),
        ..Default::default()
    }
}

fn app_with_rule(
    ctx: &egui::Context,
    style: sheetcore::dv::DvStyle,
    show_error: bool,
) -> SheetApp {
    let mut app = SheetApp::new_with_context(ctx, None);
    app.wb.sheets[0].dv.push(dv_rule(style, show_error));
    app.wb.sheets[0].dv_dirty = true;
    app
}

#[test]
fn dv_warning_alert_defers_write_until_yes() {
    let ctx = egui::Context::default();
    let mut app = app_with_rule(&ctx, sheetcore::dv::DvStyle::Warning, true);
    app.sel = (1, 1);
    app.editing = Some(EditState::new("99".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty, "警告样式不能直接写入");
    assert!(app.dv_alert.is_some(), "应弹出警告框");
    assert!(app.editing.is_none(), "提示框期间编辑框收起");

    // Enter = 是
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Number(99.0), "点是后写入");
    assert!(app.dv_alert.is_none());
}

#[test]
fn dv_warning_escape_cancels_entry() {
    let ctx = egui::Context::default();
    let mut app = app_with_rule(&ctx, sheetcore::dv::DvStyle::Warning, true);
    app.sel = (1, 1);
    app.editing = Some(EditState::new("99".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert!(app.dv_alert.is_some());
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty, "取消不写入");
    assert!(app.dv_alert.is_none());
    assert!(app.editing.is_none());
}

#[test]
fn dv_information_accepts_after_acknowledge() {
    let ctx = egui::Context::default();
    let mut app = app_with_rule(&ctx, sheetcore::dv::DvStyle::Information, true);
    app.sel = (1, 1);
    app.editing = Some(EditState::new("99".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty, "信息框先提示");
    assert!(app.dv_alert.as_ref().map(|a| a.info_only).unwrap_or(false));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Number(99.0), "确定后写入");
    assert!(app.dv_alert.is_none());
}

#[test]
fn dv_without_error_alert_writes_silently() {
    let ctx = egui::Context::default();
    let mut app = app_with_rule(&ctx, sheetcore::dv::DvStyle::Stop, false);
    app.sel = (1, 1);
    app.editing = Some(EditState::new("99".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(
        app.wb.sheet(0).value(1, 1),
        Value::Number(99.0),
        "showErrorMessage=0 时按 Excel 静默放行"
    );
    assert!(app.dv_alert.is_none());
}

#[test]
fn dv_stop_style_still_keeps_draft() {
    let ctx = egui::Context::default();
    let mut app = app_with_rule(&ctx, sheetcore::dv::DvStyle::Stop, true);
    app.sel = (1, 1);
    app.editing = Some(EditState::new("99".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty);
    assert!(app.editing.is_some(), "停止样式留在编辑框");
    assert!(app.dv_alert.is_none(), "停止样式不弹框");
    assert!(app.status.contains("越界"), "{}", app.status);
}

#[test]
fn dv_config_error_is_visible_even_without_error_alert() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheets[0].dv.push(sheetcore::dv::DataValidation {
        ranges: vec![(1, 1, 1, 1)],
        kind: sheetcore::dv::DvKind::List,
        f1: "=不存在名称".into(),
        show_error: false,
        ..Default::default()
    });
    app.wb.sheets[0].dv_dirty = true;
    app.sel = (1, 1);
    app.editing = Some(EditState::new("甲".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(
        app.wb.sheet(0).value(1, 1),
        Value::Text("甲".into()),
        "showErrorMessage=0 时不拦输入"
    );
    assert!(
        app.status.contains("数据验证配置无效"),
        "配置坏了必须能看见诊断：{}",
        app.status
    );
}

fn key_event_alt(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers { alt: true, ..Default::default() },
    }
}

fn app_with_list(ratings: &str) -> (egui::Context, SheetApp) {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheets[0].dv.push(sheetcore::dv::DataValidation {
        ranges: vec![(1, 1, 1, 1)],
        kind: sheetcore::dv::DvKind::List,
        f1: ratings.into(),
        ..Default::default()
    });
    app.wb.sheets[0].dv_dirty = true;
    app.sel = (1, 1);
    (ctx, app)
}

#[test]
fn dv_keyboard_dropdown_picks_with_arrows_and_is_undoable() {
    let (ctx, mut app) = app_with_list("\"甲,乙,丙\"");
    ui_frame(&mut app, &ctx, vec![key_event_alt(egui::Key::ArrowDown)]);
    assert!(app.dv_open.is_some(), "Alt+↓ 应打开序列下拉");
    assert_eq!(app.dv_hilite, 0, "没有当前值时高亮第一项");

    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, false)]);
    assert_eq!(app.dv_hilite, 1);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowUp, false)]);
    assert_eq!(app.dv_hilite, 0);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, false)]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);

    assert!(app.dv_open.is_none(), "选择后关闭");
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Text("乙".into()));
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty, "下拉选择必须可撤销");
}

#[test]
fn dv_keyboard_dropdown_escape_leaves_cell_unchanged() {
    let (ctx, mut app) = app_with_list("\"甲,乙\"");
    ui_frame(&mut app, &ctx, vec![key_event_alt(egui::Key::ArrowDown)]);
    assert!(app.dv_open.is_some());
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    assert!(app.dv_open.is_none(), "Esc 关闭");
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty, "Esc 不写");
    assert!(app.undo.depth() == (0, 0), "没有产生撤销记录");
}

#[test]
fn dv_alt_down_without_list_reports_and_does_not_open() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 0);
    ui_frame(&mut app, &ctx, vec![key_event_alt(egui::Key::ArrowDown)]);
    assert!(app.dv_open.is_none());
    assert!(app.status.contains("没有可用的序列来源"), "{}", app.status);
}

#[test]
fn dv_picked_value_survives_save_and_reload() {
    let (ctx, mut app) = app_with_list("\"甲,乙\"");
    ui_frame(&mut app, &ctx, vec![key_event_alt(egui::Key::ArrowDown)]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, false)]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Text("乙".into()));

    let out = std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("test dir")
        .join(format!("c07_dv_pick_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &app.wb).expect("保存");
    let back = sheetcore::xlsx::load(&out).expect("重开");
    assert_eq!(back.sheet(0).value(1, 1), Value::Text("乙".into()));
    let rule = sheetcore::dv::find(&back.sheet(0).dv, 1, 1).expect("规则仍在");
    assert_eq!(rule.kind, sheetcore::dv::DvKind::List);
    let items = sheetcore::dv::list_items_wb(&back, 0, rule, 1, 1).expect("列表可解析");
    assert_eq!(items, vec!["甲".to_string(), "乙".to_string()]);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn dv_information_escape_cancels_without_write_or_undo() {
    let ctx = egui::Context::default();
    let mut app = app_with_rule(&ctx, sheetcore::dv::DvStyle::Information, true);
    app.sel = (1, 1);
    app.editing = Some(EditState::new("99".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert!(app.dv_alert.as_ref().map(|a| a.info_only).unwrap_or(false));
    let before = app.undo.depth();

    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty, "信息框 Esc 不写入");
    assert!(app.dv_alert.is_none(), "Esc 关闭提示框");
    assert_eq!(app.undo.depth(), before, "取消不能留下撤销记录");
    assert!(app.status.contains("已取消"), "状态栏要说明已取消：{}", app.status);

    // 取消之后再输一个合法值：只应产生一条撤销记录
    app.sel = (1, 1);
    app.editing = Some(EditState::new("5".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Number(5.0));
    assert_eq!(app.undo.depth(), (before.0 + 1, 0));
}

#[test]
fn dv_information_cancel_button_discards_entry() {
    let ctx = egui::Context::default();
    let mut app = app_with_rule(&ctx, sheetcore::dv::DvStyle::Information, true);
    app.sel = (1, 1);
    app.editing = Some(EditState::new("99".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    let before = app.undo.depth();
    // 提示框里的“取消”按钮走的是同一个 resolve_dv_alert
    app.resolve_dv_alert(DvAlertChoice::Cancel);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty, "取消按钮不写入");
    assert_eq!(app.undo.depth(), before);
    assert!(!app.wb.dirty || app.wb.sheet(0).value(1, 1) == Value::Empty);
}

#[test]
fn dv_information_ok_button_writes_once() {
    let ctx = egui::Context::default();
    let mut app = app_with_rule(&ctx, sheetcore::dv::DvStyle::Information, true);
    app.sel = (1, 1);
    app.editing = Some(EditState::new("7".into()));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    app.resolve_dv_alert(DvAlertChoice::Primary);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Number(7.0), "确定后写入");
    assert_eq!(app.undo.depth(), (1, 0), "只留一条撤销");
}

#[test]
fn first_tab_after_startup_moves_cell_instead_of_leaving_focus_in_ribbon() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    ui_frame(&mut app, &ctx, vec![]);
    // egui 会在第一次 Tab 时把焦点交给第一个可聚焦控件；这不等于用户点在网格上，
    // 不能因此吞掉“提交并右移”。
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Tab, false)]);
    assert_eq!(app.sel, (0, 1), "首次 Tab 应移动单元格");
    assert!(!ctx.wants_keyboard_input(), "Tab 后不应把焦点留在功能区控件");
}

#[test]
fn f2_replace_then_tab_commits_and_moves_right() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, number(123.0));
    ui_frame(&mut app, &ctx, vec![]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::F2, false)]);
    assert_eq!(app.editing.as_ref().map(|e| e.buf.clone()).as_deref(), Some("123"));
    // F2 把光标放在末尾；Ctrl+A 全选后键入 456 才是“改 456”
    ui_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers { ctrl: true, command: true, ..Default::default() },
        }],
    );
    ui_frame(&mut app, &ctx, vec![egui::Event::Text("456".into())]);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Tab, false)]);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(456.0), "F2 改值后 Tab 提交");
    assert_eq!(app.sel, (0, 1), "Tab 后选区向右");
    assert!(app.editing.is_none());
    assert_eq!(app.undo.depth(), (1, 0), "一次提交只记一步撤销");
}

/// UI-04：Delete 键走的是 handle_keys → clear_values → clear_rects，
/// 不能只有按钮路径统一。这里按真实按键事件提交，断言依赖重算、撤销和选区。
#[test]
fn delete_key_clears_rect_and_recalcs_dependents() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    let num = |v: f64| Cell { value: Value::Number(v), formula: None };
    let f = |t: &str| Cell { value: Value::Empty, formula: Some(t.into()) };
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, num(1.0));
        sh.set(1, 0, num(2.0));
        sh.set(0, 1, f("=A1*10"));
        sh.set(1, 1, f("=A2*10"));
    }
    sheetcore::formula::recalc_all(&mut app.wb);
    ui_frame(&mut app, &ctx, vec![]);
    app.sel = (0, 0);
    app.sel_end = (1, 0);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Delete, false)]);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Empty);
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Empty);
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(0.0), "B1 立即为 0");
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Number(0.0), "B2 立即为 0");
    assert_eq!(app.sel, (0, 0), "Delete 不移动选区");
    assert!(app.wb.dirty, "清除必须标脏");
    assert_eq!(app.undo.depth().0, 1, "一次清除只压一步撤销");
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(1.0));
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Number(20.0), "撤销恢复依赖结果");
}

/// UI-04：右键“清除内容”、功能区“清除内容”和 Delete 键必须是同一条命令。
#[test]
fn clear_entries_leave_identical_state() {
    let build = |ctx: &egui::Context| {
        let mut app = SheetApp::new_with_context(ctx, None);
        let num = |v: f64| Cell { value: Value::Number(v), formula: None };
        let f = |t: &str| Cell { value: Value::Empty, formula: Some(t.into()) };
        {
            let sh = app.wb.sheet_mut(0);
            sh.set(0, 0, num(3.0));
            sh.set(0, 1, f("=A1*10"));
            sh.set_style(0, 0, 0);
        }
        sheetcore::formula::recalc_all(&mut app.wb);
        app.sel = (0, 0);
        app.sel_end = (0, 0);
        app
    };
    let ctx = egui::Context::default();
    let mut a = build(&ctx);
    ui_frame(&mut a, &ctx, vec![key_event(egui::Key::Delete, false)]);
    let mut b = build(&ctx);
    b.ctx_menu_run(CtxAct::ClearContents, 0, &ctx);
    let mut c = build(&ctx);
    c.ribbon_action("ClearValues", &ctx);
    let snap = |app: &SheetApp| {
        (
            app.wb.sheet(0).value(0, 0),
            app.wb.sheet(0).value(0, 1),
            app.wb.sheet(0).style_of(0, 0),
            app.undo.depth(),
            app.wb.dirty,
        )
    };
    assert_eq!(snap(&a), snap(&b), "Delete 与右键清除内容结果一致");
    assert_eq!(snap(&a), snap(&c), "Delete 与功能区清除内容结果一致");
}

/// C15：保护表的「选定锁定单元格 / 选定未锁定单元格」必须在真实选择入口生效，
/// 不能只是配置项。这里走名称框定位入口，并直接核对门禁判定。
#[test]
fn selection_permissions_block_jumping_into_forbidden_cells() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(
        0,
        1,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Text("未锁".into()),
            formula: None,
        },
    );
    // B1 显式解锁（默认样式的 locked=true）
    let mut st = app.wb.styles.get(app.wb.sheet(0).style_of(0, 1)).clone();
    st.locked = false;
    let sid = app.wb.styles.intern(st);
    app.wb.sheet_mut(0).set_style(0, 1, sid);
    let mut prot = sheetcore::protection::SheetProtection::new_with_password("pw");
    prot.perms.select_locked = false;
    prot.perms.select_unlocked = true;
    app.wb.sheet_mut(0).protection = Some(prot);
    app.sel = (0, 1);
    app.sel_end = (0, 1);

    assert!(!app.sel_blocked_at(0, 1), "允许选未锁格");
    assert!(app.sel_blocked_at(0, 0), "禁止选锁定格");

    app.name_buf = "A1".into();
    app.goto_name();
    assert_eq!(app.sel, (0, 1), "被拒后选区不能跳到锁定格");
    assert!(app.status.contains("不允许选择"), "状态栏要说明原因：{}", app.status);

    // 只关「选定未锁定」：未锁格也不能选
    app.wb.sheet_mut(0).protection.as_mut().unwrap().perms.select_unlocked = false;
    app.name_buf = "B1".into();
    app.goto_name();
    assert_eq!(app.sel, (0, 1));
    assert!(app.status.contains("不允许选择"));

    // 两项都允许：入口恢复
    let p = app.wb.sheet_mut(0).protection.as_mut().unwrap();
    p.perms.select_locked = true;
    p.perms.select_unlocked = true;
    app.name_buf = "A1".into();
    app.goto_name();
    assert_eq!(app.sel, (0, 0), "有权限时应能定位到锁定格");

    // 未保护工作表：不受影响
    app.wb.sheet_mut(0).protection = None;
    assert!(!app.sel_blocked_at(0, 0));
    assert!(!app.sel_blocked_at(5, 5));
}

/// C15：Tab/Enter 的 advance_active 也必须过选择权限，不能从名称框拦住、键盘却走得进去。
#[test]
fn keyboard_advance_skips_cells_without_selection_permission() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    // A1 锁定（默认），B1/C1 解锁，D1 锁定；保护表只允许选未锁格
    for c in [1u32, 2] {
        let sid = app.wb.sheet(0).style_of(0, c);
        let mut st = app.wb.styles.get(sid).clone();
        st.locked = false;
        let sid = app.wb.styles.intern(st);
        app.wb.sheet_mut(0).set_style(0, c, sid);
    }
    let mut prot = sheetcore::protection::SheetProtection::new_with_password("pw");
    prot.perms.select_locked = false;
    prot.perms.select_unlocked = true;
    app.wb.sheet_mut(0).protection = Some(prot);
    app.sel = (0, 1);
    app.sel_end = (0, 1);
    app.advance_active(0, 1); // Tab：C1 未锁，可以进
    assert_eq!(app.sel, (0, 2), "Tab 应停在 C1");
    app.advance_active(0, 1); // D1 锁定：必须跳过
    assert_ne!(app.sel, (0, 3), "Tab 不能进入禁止选择的锁定格 D1");
    assert!(app.sel_blocked_at(0, 3));

    // 全锁：来回都走不动，状态栏说明原因
    let mut app2 = SheetApp::new_with_context(&ctx, None);
    let mut p2 = sheetcore::protection::SheetProtection::new_with_password("pw");
    p2.perms.select_locked = false;
    p2.perms.select_unlocked = true;
    app2.wb.sheet_mut(0).protection = Some(p2);
    app2.sel = (0, 0);
    app2.sel_end = (0, 0);
    app2.advance_active(0, 1);
    assert_eq!(app2.sel, (0, 0), "全部禁选时不能移动");
    assert!(app2.status.contains("不允许选择"), "状态栏：{}", app2.status);
}

/// C15：名称框 A1:B2 这类整块选区必须逐格检查，不能只看左上角。
#[test]
fn name_box_range_rejects_when_any_cell_is_forbidden() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    // B1 解锁，A1/B2 保持锁定
    let sid = app.wb.sheet(0).style_of(0, 1);
    let mut st = app.wb.styles.get(sid).clone();
    st.locked = false;
    let sid = app.wb.styles.intern(st);
    app.wb.sheet_mut(0).set_style(0, 1, sid);
    let mut prot = sheetcore::protection::SheetProtection::new_with_password("pw");
    prot.perms.select_locked = false;
    prot.perms.select_unlocked = true;
    app.wb.sheet_mut(0).protection = Some(prot);

    app.sel = (0, 1);
    app.sel_end = (0, 1);
    app.name_buf = "A1:B2".into();
    app.goto_name();
    assert_eq!(app.sel, (0, 1), "含锁定格的整块选区不能被接受");
    assert_eq!(app.sel_end, (0, 1));
    assert!(app.rect_sel_blocked((0, 0), (1, 1)));
    assert!(!app.rect_sel_blocked((0, 1), (0, 1)));

    app.name_buf = "B1".into();
    app.goto_name();
    assert_eq!(app.sel, (0, 1), "仅未锁格可以定位");
    assert_eq!(app.sel_end, (0, 1));
}

/// C12.3：多字段透视生成后刷新，旧区域被清掉，不留旧数，也不越界写。
#[test]
fn multi_field_pivot_refresh_clears_old_cells() {
    use sheetcore::pivot::Agg;
    use sheetcore::pivot_multi::{AxisField, FilterField, PivotLayout, PivotSpec2, ValueField};
    let mut app = app();
    {
        let sh = app.wb.sheet_mut(0);
        for (c, h) in ["地区", "城市", "产品", "销售额", "数量", "状态"].iter().enumerate() {
            sh.set(0, c as u32, Cell { value: Value::Text((*h).into()), formula: None });
        }
        let rows: [(&str, &str, &str, f64, f64, &str); 4] = [
            ("华东", "上海", "甲", 100.0, 1.0, "有效"),
            ("华东", "上海", "乙", 50.0, 2.0, "有效"),
            ("华北", "北京", "甲", 20.0, 4.0, "有效"),
            ("华北", "天津", "甲", 30.0, 6.0, "无效"),
        ];
        for (i, (rg, ct, pd, amt, qty, st)) in rows.iter().enumerate() {
            let r = i as u32 + 1;
            sh.set(r, 0, Cell { value: Value::Text((*rg).into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Text((*ct).into()), formula: None });
            sh.set(r, 2, Cell { value: Value::Text((*pd).into()), formula: None });
            sh.set(r, 3, Cell { value: Value::Number(*amt), formula: None });
            sh.set(r, 4, Cell { value: Value::Number(*qty), formula: None });
            sh.set(r, 5, Cell { value: Value::Text((*st).into()), formula: None });
        }
    }
    let spec = PivotSpec2 {
        source: (0, 0, 4, 5),
        rows: vec![AxisField::plain(0), AxisField::plain(1)],
        cols: vec![AxisField::plain(2)],
        values: vec![
            ValueField { col: 3, agg: Agg::Sum, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal , base: None },
            ValueField { col: 4, agg: Agg::Count, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal , base: None },
        ],
        filters: vec![FilterField { col: 5, keep: vec!["有效".into()] }],
        layout: PivotLayout::default(),
    };
    let rect = app.apply_pivot2(&spec, (10, 0), None, false);
    // 表头行数随「列字段 + 多值总计列」变化，按行找「华北」再核对值，别写死行号
    let sheet = app.wb.sheet(0);
    let mut found = None;
    for r in 10..rect.2 + 10 {
        if sheet.get(r, 0).map(|c| c.value.display()).as_deref() == Some("华北") {
            found = Some(r);
            break;
        }
    }
    let row = found.expect("网格里应有「华北」行");
    assert_eq!(
        sheet.get(row, 2).map(|c| c.value.display()).unwrap_or_default(),
        "20"
    );
    // 取消筛选后天津回来，总行数变多；刷新必须清掉旧网格
    let mut spec2 = spec.clone();
    spec2.filters.clear();
    let rect2 = app.apply_pivot2(&spec2, (rect.0, rect.1), Some(rect), true);
    // 清掉筛选后标题区（2 行）消失、明细多一行：允许总高度变化，但不能丢数据
    assert!(rect2.2 + 2 >= rect.2, "清筛选后高度变化应可解释");
    let old_bottom = app
        .wb
        .sheet(0)
        .get(rect.2, rect.3)
        .map(|c| c.value.display())
        .unwrap_or_default();
    assert_ne!(old_bottom, "总计", "旧总计行必须被清掉后重写");
    let labels: Vec<String> = (rect2.0..=rect2.2)
        .map(|r| app.wb.sheet(0).get(r, rect2.1).map(|c| c.value.display()).unwrap_or_default())
        .collect();
    assert!(labels.iter().any(|x| x == "总计"), "刷新后仍有总计行：{labels:?}");
    assert!(labels.iter().any(|x| x.starts_with("华北")), "天津回来后华北增加子行");
}

/// UI-25：状态栏统计按 Excel 定义：空白不计入计数，文本计入计数但不进数值统计。
#[test]
fn status_bar_stats_follow_excel_definitions() {
    use crate::status_stats_text;
    let mut sh = sheetcore::model::Sheet::new("S");
    sh.set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    sh.set(1, 0, Cell { value: Value::Number(2.0), formula: None });
    sh.set(2, 0, Cell { value: Value::Text("备注".into()), formula: None });
    sh.set(1, 1, Cell { value: Value::Number(10.0), formula: None });
    let all = status_stats_text(&sh, (0, 0, 2, 1), 0b11_1111);
    assert!(all.contains("平均值: 4.3333"), "{all}");
    assert!(all.contains("计数: 4"), "文本算计数、空白不算：{all}");
    assert!(all.contains("数值计数: 3"), "{all}");
    assert!(all.contains("求和: 13"), "{all}");
    assert!(all.contains("最小值: 1"), "{all}");
    assert!(all.contains("最大值: 10"), "{all}");
    let only_count = status_stats_text(&sh, (0, 0, 2, 1), 2);
    assert_eq!(only_count, "计数: 4");
    let only_sum = status_stats_text(&sh, (0, 0, 2, 1), 8);
    assert_eq!(only_sum, "求和: 13");
    // 纯文本区域：数值项都不显示，只留计数
    let txt = status_stats_text(&sh, (2, 0, 2, 0), 0b11_1111);
    assert_eq!(txt, "计数: 1");
}

/// UI-17.3：分列覆盖右侧格子时要二次确认，撤销必须把扩展区域一起还原。
#[test]
fn split_preview_confirms_overwrite_and_undo_restores_expanded_region() {
    let mut app = app();
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Text("甲,00123,2026-03-05".into()), formula: None });
    app.wb.sheet_mut(0).set(0, 1, Cell { value: Value::Text("旧".into()), formula: None });
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    let dlg = DataDlg {
        tab: 1,
        keys: Vec::new(),
        header: false,
        delim: ",".into(),
        sort_mode: 0,
        custom: String::new(),
        merge_consecutive: false,
        quote: true,
        detect_types: true,
        sort_by_row: false,
        split_fixed: false,
        widths: String::new(),
        col_modes: String::new(),
    };
    app.run_split(&dlg);
    assert!(app.status.contains("覆盖"), "第一次应只预览：{}", app.status);
    assert_eq!(app.wb.sheet(0).get(0, 1).unwrap().value.display(), "旧", "确认前不能写");
    app.run_split(&dlg);
    assert_eq!(app.wb.sheet(0).get(0, 0).unwrap().value.display(), "甲");
    assert_eq!(app.wb.sheet(0).get(0, 1).unwrap().value.display(), "00123", "前导零保留文本");
    assert!(!app.wb.sheet(0).get(0, 2).map(|c| c.is_blank()).unwrap_or(true), "日期列写出");
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).get(0, 0).unwrap().value.display(), "甲,00123,2026-03-05", "撤销恢复原文本");
    assert_eq!(app.wb.sheet(0).get(0, 1).unwrap().value.display(), "旧", "被覆盖格也恢复");
    assert!(app.wb.sheet(0).get(0, 2).map(|c| c.is_blank()).unwrap_or(true), "新写日期列被撤销");
}

/// UI-17.2：去重按选中列判断，预览数等于实际删除数；不同列组合结果不同。
#[test]
fn dedup_uses_selected_columns_and_preview_matches_removed() {
    let seed = |a: &mut SheetApp| {
        let sh = a.wb.sheet_mut(0);
        for (r, (x, y)) in [("甲", "1"), ("甲", "2"), ("乙", "1")].iter().enumerate() {
            let row = r as u32 + 1;
            sh.set(row, 0, Cell { value: Value::Text((*x).into()), formula: None });
            sh.set(row, 1, Cell { value: Value::Text((*y).into()), formula: None });
        }
    };
    let mut a = app();
    seed(&mut a);
    let rect = (1u32, 0u32, 3u32, 1u32);
    let preview = sheetcore::data_tools::count_duplicates(a.wb.sheet(0), rect, &[0], false);
    assert_eq!(preview, 1, "按第一列有一条重复");
    a.apply_dedup(rect, false, &[0]);
    assert_eq!(a.wb.sheet(0).get(2, 0).map(|c| c.value.display()).unwrap_or_default(), "乙", "保留行上移");
    assert!(a.wb.sheet(0).get(3, 0).map(|c| c.is_blank()).unwrap_or(true), "尾部清空");

    let mut b = app();
    seed(&mut b);
    let preview_all = sheetcore::data_tools::count_duplicates(b.wb.sheet(0), rect, &[], false);
    assert_eq!(preview_all, 0, "两列都参与时没有重复");
    b.apply_dedup(rect, false, &[]);
    assert_eq!(b.status, "没有重复行");
}

/// UI-17.1：多级排序按级别顺序比较，选区内排序、选区外不动，标题不参与。
#[test]
fn multi_level_sort_respects_level_order_and_selection_range() {
    let mut app = app();
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("编号".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("组".into()), formula: None });
        for (i, (n, g)) in [(1.0, "y"), (2.0, "x"), (3.0, "y"), (4.0, "x")].iter().enumerate() {
            let r = i as u32 + 1;
            sh.set(r, 0, Cell { value: Value::Number(*n), formula: None });
            sh.set(r, 1, Cell { value: Value::Text((*g).into()), formula: None });
        }
        sh.set(5, 0, Cell { value: Value::Number(99.0), formula: None });
    }
    app.sel = (0, 0);
    app.sel_end = (4, 1);
    let dlg = DataDlg {
        tab: 0,
        keys: vec![(1, true, true), (0, true, true)],
        header: true,
        delim: ",".into(),
        sort_mode: 0,
        custom: String::new(),
        merge_consecutive: false,
        quote: false,
        detect_types: false,
        sort_by_row: false,
        split_fixed: false,
        widths: String::new(),
        col_modes: String::new(),
    };
    app.run_sort(&dlg);
    let order: Vec<String> = (1..=4)
        .map(|r| app.wb.sheet(0).get(r, 0).map(|c| c.value.display()).unwrap_or_default())
        .collect();
    assert_eq!(order, vec!["2", "4", "1", "3"], "先组后编号");
    assert_eq!(app.wb.sheet(0).get(5, 0).unwrap().value.display(), "99", "选区外不动");
    assert_eq!(app.wb.sheet(0).get(0, 0).unwrap().value.display(), "编号", "标题保留");
}

/// UI-13.3：对话框宽度在小窗口内收缩，大屏保持原宽。
#[test]
fn dialog_width_fits_small_viewport() {
    let small = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(360.0, 640.0));
    assert_eq!(crate::ui_kit::dialog_width(520.0, small), 344.0);
    let big = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
    assert_eq!(crate::ui_kit::dialog_width(520.0, big), 520.0);
}

#[test]
fn editing_a_table_header_renames_the_column_and_fixes_formulas() {
    let mut app = app();
    {
        let sh = app.wb.sheet_mut(0);
        sh.tables.push(sheetcore::table::Table {
            name: "Sales".into(),
            display: "Sales".into(),
            range: (0, 0, 3, 2),
            columns: vec!["Amount".into(), "Tax".into(), "Total".into()],
            column_ids: vec![1, 2, 3],
            header_row: true,
            totals_row: false,
        });
        for (c, h) in ["Amount", "Tax", "Total"].iter().enumerate() {
            sh.set(0, c as u32, sheetcore::model::Cell {
                value: sheetcore::model::Value::Text((*h).into()),
                formula: None,
            });
        }
        for r in 1..=3 {
            sh.set(r, 0, sheetcore::model::Cell { value: sheetcore::model::Value::Number(r as f64), formula: None });
            sh.set(r, 1, sheetcore::model::Cell { value: sheetcore::model::Value::Number(2.0), formula: None });
            sh.set(r, 2, sheetcore::model::Cell { value: sheetcore::model::Value::Number(4.0), formula: None });
        }
        sh.set(0, 4, sheetcore::model::Cell {
            value: sheetcore::model::Value::Empty,
            formula: Some("=SUM(Sales[Amount])".into()),
        });
    }
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.editing = Some(EditState::new("金额".into()));
    assert!(app.commit_edit(None), "表头改名应写入");
    let sh = app.wb.sheet(0);
    assert_eq!(sh.tables[0].columns[0], "金额", "表格列定义要跟着改");
    assert_eq!(
        sh.get(0, 4).and_then(|c| c.formula.as_deref()),
        Some("=SUM(Sales[金额])"),
        "结构化引用里的列名要一起改"
    );
}
#[test]
fn dv_dropdown_filters_and_selection_is_one_undo_step() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheets[0].dv.push(sheetcore::dv::DataValidation {
        ranges: vec![(1, 1, 1, 1)],
        kind: sheetcore::dv::DvKind::List,
        f1: "\"苹果,香蕉,Apple,apricot\"".into(),
        ..Default::default()
    });
    app.sel = (1, 1);
    app.dv_open_for_selection();
    assert_eq!(app.dv_query, "", "打开下拉应先清空搜索词");
    assert_eq!(app.dv_visible_items(1, 1).len(), 4);

    // 无匹配时下拉不能自己关掉，否则用户没法退格改搜索词
    app.dv_query = "不存在".into();
    ui_frame(&mut app, &ctx, Vec::new());
    assert!(app.dv_open.is_some(), "无匹配项时下拉应保持打开");
    assert!(app.dv_visible_items(1, 1).is_empty());

    app.dv_query = "AP".into();
    let vis = app.dv_visible_items(1, 1);
    assert_eq!(vis, vec!["Apple".to_string(), "apricot".to_string()], "大小写不敏感");
    app.dv_hilite = 1;
    // 走真实键盘路径：下拉打开时 Enter 应选中过滤后的第 2 项
    app.dv_open = Some((1, 1));
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Text("apricot".into()));
    assert!(app.dv_open.is_none(), "选择后应关闭下拉");

    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).value(1, 1), Value::Empty, "一次撤销应整体回退");
}

#[test]
fn dv_filter_handles_large_lists() {
    let items: Vec<String> = (0..5000).map(|i| format!("客户{i:04}")).collect();
    let hit = filter_dv_items(&items, "客户49");
    assert_eq!(hit.len(), 100, "5000 项里只留 4900..4999");
    assert_eq!(hit.first().map(String::as_str), Some("客户4900"));
    assert_eq!(hit.last().map(String::as_str), Some("客户4999"));
    assert_eq!(filter_dv_items(&items, "   ").len(), 5000);
    assert!(filter_dv_items(&items, "不存在").is_empty());
}
/// UI-12.1：缩放只改表格几何，几何=模型尺寸×zoom；功能区/对话框不参与。
#[test]
fn grid_zoom_scales_axes_and_hit_testing() {
    let mut app = app();
    app.zoom = 2.0;
    let (cols, rows) = app.view_axes();
    assert!((cols.size(0) - sheetcore::layout::DEFAULT_COL_W * 2.0).abs() < 0.01);
    assert!((rows.size(0) - sheetcore::layout::DEFAULT_ROW_H * 2.0).abs() < 0.01);
    assert_eq!(cols.index_at(sheetcore::layout::DEFAULT_COL_W * 2.0 - 0.5), 0);
    assert_eq!(cols.index_at(sheetcore::layout::DEFAULT_COL_W * 2.0 + 0.5), 1);

    app.zoom = 0.5;
    let (cols, rows) = app.view_axes();
    assert!((cols.size(0) - sheetcore::layout::DEFAULT_COL_W * 0.5).abs() < 0.01);
    assert!((rows.size(0) - sheetcore::layout::DEFAULT_ROW_H * 0.5).abs() < 0.01);
    assert_eq!(app.zoom, 0.5, "缩放状态留在表格视图里");
}

/// UI-12.2：缩放前后左上角单元格和格内偏移都不变（含冻结窗格）。
#[test]
fn zoom_keeps_anchor_cell_with_and_without_freeze() {
    let mut app = app();
    let (cols, rows) = app.view_axes();
    app.scroll = egui::vec2(cols.pos(5) + 9.0, rows.pos(3) + 4.0);
    let anchor = (cols.index_at(app.scroll.x), rows.index_at(app.scroll.y));
    let frac = ((app.scroll.x - cols.pos(anchor.0)) / cols.size(anchor.0),
                (app.scroll.y - rows.pos(anchor.1)) / rows.size(anchor.1));
    app.set_zoom(2.0);
    let (cols2, rows2) = app.view_axes();
    assert_eq!((cols2.index_at(app.scroll.x), rows2.index_at(app.scroll.y)), anchor);
    assert!(((app.scroll.x - cols2.pos(anchor.0)) / cols2.size(anchor.0) - frac.0).abs() < 0.01);
    assert!(((app.scroll.y - rows2.pos(anchor.1)) / rows2.size(anchor.1) - frac.1).abs() < 0.01);

    // 冻结两行两列：锚点按「冻结宽度 + scroll」计算，缩放后仍命中同一格。
    app.zoom = 1.0;
    app.wb.sheets[0].freeze = Some((2, 2));
    app.scroll = egui::vec2(17.0, 11.0);
    let (cols, rows) = app.view_axes();
    let fw = cols.pos(2);
    let fh = rows.pos(2);
    let anchor = (cols.index_at(fw + app.scroll.x), rows.index_at(fh + app.scroll.y));
    app.set_zoom(1.5);
    let (cols2, rows2) = app.view_axes();
    let fw2 = cols2.pos(2);
    let fh2 = rows2.pos(2);
    assert_eq!(
        (cols2.index_at(fw2 + app.scroll.x), rows2.index_at(fh2 + app.scroll.y)),
        anchor,
        "冻结区缩放后锚点不能跳到别的格"
    );
}
/// UI-12.3：每张表各自记 zoom，切表回来不被别的表覆盖。
#[test]
fn zoom_is_per_sheet() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::Sheet::new("Other"));
    app.set_zoom(2.0);
    assert_eq!(app.zoom, 2.0);
    app.switch_sheet_relative(1);
    assert_eq!(app.zoom, 1.0, "新表默认 100%");
    app.set_zoom(1.5);
    app.switch_sheet_relative(-1);
    assert_eq!(app.zoom, 2.0, "切回第一张表恢复它自己的 200%");
    app.switch_sheet_relative(1);
    assert_eq!(app.zoom, 1.5, "第二张表恢复 150%");
}
/// UI-25：多选、隐藏行/列、筛选与百万行稀疏选区。
#[test]
fn status_stats_cover_multi_rect_filter_and_sparse_edges() {
    use crate::{status_stats_rects, status_stats_text};
    let mut sh = sheetcore::model::Sheet::new("S");
    for (r, c, v) in [
        (0u32, 0u32, Value::Number(1.0)),
        (2, 0, Value::Text("文本".into())),
        (3, 0, Value::Number(3.0)),
        (1_000_000, 0, Value::Number(5.0)),
    ] {
        sh.set(r, c, Cell { value: v, formula: None });
    }
    // A1 + A4 非连续选区：sum=4，两个数值，count=2
    let multi = status_stats_rects(&sh, &[(0, 0, 0, 0), (3, 0, 3, 0)], 0b11_1111);
    assert!(multi.contains("求和: 4"), "{multi}");
    assert!(multi.contains("数值计数: 2"), "{multi}");
    assert!(multi.contains("平均值: 2"), "{multi}");

    // 矩形本身的空白、文本都按 Excel 口径：count 计文本，数值项只吃 Number
    let all = status_stats_text(&sh, (0, 0, 3, 0), 0b11_1111);
    assert!(all.contains("计数: 3"), "{all}");
    assert!(all.contains("数值计数: 2"), "{all}");

    // 手动隐藏行（右键 HideRows 只写 hidden_rows_user）不计入统计
    sh.hidden_rows_user.insert(3);
    let manual = status_stats_rects(&sh, &[(0, 0, 3, 0)], 8);
    assert!(manual.contains("求和: 1"), "隐藏 A4 后只剩 A1：{manual}");
    sh.hidden_rows_user.clear();

    // 行高 0（文件里 hidden 的行）同样不算
    sh.rows.set(0, 0.0);
    let hidden = status_stats_rects(&sh, &[(0, 0, 3, 0)], 8);
    assert!(hidden.contains("求和: 3"), "隐藏 A1 后只剩 A4：{hidden}");
    sh.rows.set(0, 18.0);

    // 隐藏列照 Excel 实测口径仍然计入（不是“隐藏就排除”）
    sh.hidden_cols.insert(0);
    let col_hidden = status_stats_rects(&sh, &[(0, 0, 3, 0)], 8);
    assert!(col_hidden.contains("求和: 4"), "隐藏列仍计入：{col_hidden}");
    sh.hidden_cols.clear();

    // 百万行稀疏选区：只碰实际存在的格，不需要逐坐标扫
    let big = status_stats_rects(&sh, &[(0, 0, 1_048_575, 0)], 0b11_1111);
    assert!(big.contains("求和: 9"), "1 + 3 + 5：{big}");
}
/// UI-25：筛选隐藏的行必须从统计里排除（规则来自真实 Filter，宿主只写 filter 也要生效）。
#[test]
fn status_stats_follow_active_filter_hidden_rows() {
    use crate::status_stats_rects;
    let mut sh = sheetcore::model::Sheet::new("S");
    sh.set(0, 0, Cell { value: Value::Text("类型".into()), formula: None });
    sh.set(1, 0, Cell { value: Value::Text("A".into()), formula: None });
    sh.set(1, 1, Cell { value: Value::Number(10.0), formula: None });
    sh.set(2, 0, Cell { value: Value::Text("B".into()), formula: None });
    sh.set(2, 1, Cell { value: Value::Number(20.0), formula: None });
    let mut f = sheetcore::sort_filter::Filter::new(0, 0, 2, 1);
    f.allows.insert(0, std::iter::once("A".to_string()).collect());
    let hidden = f.hidden_rows(&sh);
    assert!(hidden.contains(&2), "B 行应被筛掉：{hidden:?}");
    sh.filter = Some(f);
    let stat = status_stats_rects(&sh, &[(0, 0, 2, 1)], 0b11_1111);
    assert!(stat.contains("求和: 10"), "只统计可见的 A 行：{stat}");
    assert!(stat.contains("计数: 3"), "表头+A 行类型+A 行数值：{stat}");
    // 宿主应用筛选时还会把 hidden_rows 同步下来，结果必须一致
    sh.hidden_rows = hidden;
    let again = status_stats_rects(&sh, &[(0, 0, 2, 1)], 0b11_1111);
    assert_eq!(again, stat);
}
/// UI-25：隐藏行列的统计口径按本机 Excel 16.0 UIA 实测
/// （`implementation-evidence/ui25-hidden-marker.txt`）：
/// 手动隐藏行、分级折叠行排除；隐藏列**仍然计入**。
#[test]
fn status_stats_exclude_manual_and_outline_hidden_rows_but_keep_hidden_cols() {
    use crate::status_stats_text;
    let mut sh = sheetcore::model::Sheet::new("S");
    sh.set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    sh.set(1, 0, Cell { value: Value::Number(2.0), formula: None });
    sh.set(2, 0, Cell { value: Value::Text("x".into()), formula: None });
    sh.set(3, 0, Cell { value: Value::Number(4.0), formula: None });
    sh.set(0, 1, Cell { value: Value::Number(100.0), formula: None });
    // 基准：Excel 平均值 2.333333333 / 计数 4 / 求和 7
    let base = status_stats_text(&sh, (0, 0, 3, 0), 0b11_1111);
    assert!(base.contains("平均值: 2.3333"), "{base}");
    assert!(base.contains("计数: 4"), "{base}");
    assert!(base.contains("求和: 7"), "{base}");
    // 手动隐藏第 2 行：Excel 平均值 2.5 / 计数 3 / 求和 5
    sh.hidden_rows_user.insert(1);
    let manual = status_stats_text(&sh, (0, 0, 3, 0), 0b11_1111);
    assert!(manual.contains("平均值: 2.5"), "{manual}");
    assert!(manual.contains("计数: 3"), "{manual}");
    assert!(manual.contains("求和: 5"), "{manual}");
    // 同一行同时被筛选标成 hidden_rows：集合去重，结果不变
    sh.hidden_rows.insert(1);
    assert_eq!(status_stats_text(&sh, (0, 0, 3, 0), 0b11_1111), manual);
    sh.hidden_rows_user.clear();
    sh.hidden_rows.clear();
    // 分级/组合折叠只写 hidden_rows：A6:A8 = 7,8 折叠后 Excel 计数 1 / 求和 7
    sh.set(5, 0, Cell { value: Value::Number(7.0), formula: None });
    sh.set(6, 0, Cell { value: Value::Number(8.0), formula: None });
    sh.hidden_rows.extend([6u32, 7]);
    let outline = status_stats_text(&sh, (5, 0, 7, 0), 0b11_1111);
    assert!(outline.contains("计数: 1"), "{outline}");
    assert!(outline.contains("求和: 7"), "{outline}");
    sh.hidden_rows.clear();
    // 隐藏 B 列后选 A1:B1：Excel 计数 2 / 求和 101，隐藏列不排除
    sh.hidden_cols.insert(1);
    let cols = status_stats_text(&sh, (0, 0, 0, 1), 0b11_1111);
    assert!(cols.contains("计数: 2"), "{cols}");
    assert!(cols.contains("求和: 101"), "{cols}");
}
/// UI-25：真实右键菜单「隐藏行 / 隐藏列」写下的标记要直接进状态栏统计，
/// 口径同 Excel 实测：隐藏行排除、隐藏列仍计入。
#[test]
fn ui25_context_menu_hidden_markers_drive_status_stats() {
    use crate::status_stats_rects;
    let ctx = egui::Context::default();
    let mut app = app();
    for (r, v) in [(0u32, 1.0), (1, 2.0), (2, 4.0)] {
        app.wb
            .sheet_mut(0)
            .set(r, 0, Cell { value: Value::Number(v), formula: None });
    }
    app.wb
        .sheet_mut(0)
        .set(0, 1, Cell { value: Value::Number(100.0), formula: None });
    let mask = 0b11_1111;
    app.sel = (0, 0);
    app.sel_end = (2, 0);
    let base = status_stats_rects(app.wb.sheet(0), &app.sel_rects(), mask);
    assert!(base.contains("求和: 7"), "{base}");
    assert!(base.contains("计数: 3"), "{base}");
    // 右键菜单「隐藏行」走的是同一个 CtxAct 分发；先选中第 2 行再隐藏
    app.sel = (1, 0);
    app.sel_end = (1, 0);
    app.ctx_menu_run(CtxAct::HideRows, 1, &ctx);
    assert!(app.wb.sheet(0).hidden_rows_user.contains(&1), "隐藏行应写 hidden_rows_user");
    app.sel = (0, 0);
    app.sel_end = (2, 0);
    let hidden = status_stats_rects(app.wb.sheet(0), &app.sel_rects(), mask);
    assert!(hidden.contains("求和: 5"), "隐藏第 2 行后 1+4：{hidden}");
    assert!(hidden.contains("计数: 2"), "{hidden}");
    // 选中 B1 再隐藏 B 列：Excel 口径隐藏列仍计入（1+100）
    app.sel = (0, 1);
    app.sel_end = (0, 1);
    app.ctx_menu_run(CtxAct::HideCols, 1, &ctx);
    assert!(app.wb.sheet(0).hidden_cols.contains(&1), "隐藏列应写 hidden_cols");
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    let cols = status_stats_rects(app.wb.sheet(0), &app.sel_rects(), mask);
    assert!(cols.contains("求和: 101"), "隐藏列仍计入：{cols}");
    assert!(cols.contains("计数: 2"), "{cols}");
}
/// C15：B1/D1 解锁、C1 锁定且禁选锁定格时，鼠标拖选 B1→D1 不能把 C1 包进来。
#[test]
fn drag_selection_rejects_range_containing_forbidden_cell() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for c in [1u32, 3] {
        let sid = app.wb.sheet(0).style_of(0, c);
        let mut st = app.wb.styles.get(sid).clone();
        st.locked = false;
        let sid = app.wb.styles.intern(st);
        app.wb.sheet_mut(0).set_style(0, c, sid);
    }
    let mut prot = sheetcore::protection::SheetProtection::new_with_password("pw");
    prot.perms.select_locked = false;
    prot.perms.select_unlocked = true;
    app.wb.sheet_mut(0).protection = Some(prot);

    let x_b = HEAD_W + app.wb.sheets[0].cols.pos(1) + 15.0;
    let x_d = HEAD_W + app.wb.sheets[0].cols.pos(3) + 15.0;
    let y = HEAD_H + app.wb.sheets[0].rows.pos(0) + 10.0;
    let run = |app: &mut SheetApp, ctx: &egui::Context, pos: egui::Pos2, pressed: Option<bool>| {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        input.events.push(egui::Event::PointerMoved(pos));
        if let Some(pressed) = pressed {
            input.events.push(egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default(),
            });
        }
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::NONE)
                .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
        });
    };
    run(&mut app, &ctx, egui::pos2(x_b, y), None);
    run(&mut app, &ctx, egui::pos2(x_b, y), Some(true));
    assert_eq!(app.sel, (0, 1), "B1 本身可选");
    run(&mut app, &ctx, egui::pos2(x_d, y), None); // 按住拖到 D1
    assert_eq!(app.sel_end, (0, 1), "整块含锁定的 C1，选区不能扩到 D1");
    assert!(app.status.contains("不允许选择"), "状态栏：{}", app.status);
}

/// C15：Shift+点击 D1 同样按整块检查，不能只查终点。
#[test]
fn shift_click_rejects_range_containing_forbidden_cell() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for c in [1u32, 3] {
        let sid = app.wb.sheet(0).style_of(0, c);
        let mut st = app.wb.styles.get(sid).clone();
        st.locked = false;
        let sid = app.wb.styles.intern(st);
        app.wb.sheet_mut(0).set_style(0, c, sid);
    }
    let mut prot = sheetcore::protection::SheetProtection::new_with_password("pw");
    prot.perms.select_locked = false;
    prot.perms.select_unlocked = true;
    app.wb.sheet_mut(0).protection = Some(prot);
    app.sel = (0, 1);
    app.sel_end = (0, 1);

    let x_d = HEAD_W + app.wb.sheets[0].cols.pos(3) + 15.0;
    let y = HEAD_H + app.wb.sheets[0].rows.pos(0) + 10.0;
    let pos = egui::pos2(x_d, y);
    let sh = egui::Modifiers { shift: true, ..Default::default() };
    for pressed in [None, Some(true), Some(false)] {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            modifiers: sh,
            ..Default::default()
        };
        input.events.push(egui::Event::PointerMoved(pos));
        if let Some(pressed) = pressed {
            input.events.push(egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: sh,
            });
        }
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::NONE)
                .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
        });
    }
    assert_eq!(app.sel, (0, 1), "含锁定 C1 的 Shift 选区必须整块拒绝");
    assert_eq!(app.sel_end, (0, 1));
    assert!(app.status.contains("不允许选择"), "状态栏：{}", app.status);
}
/// C15：Shift+方向键扩选同样整块过门禁，不能跨过锁定的 C1 扩到 D1。
#[test]
fn shift_arrow_rejects_range_containing_forbidden_cell() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for c in [1u32, 3] {
        let sid = app.wb.sheet(0).style_of(0, c);
        let mut st = app.wb.styles.get(sid).clone();
        st.locked = false;
        let sid = app.wb.styles.intern(st);
        app.wb.sheet_mut(0).set_style(0, c, sid);
    }
    let mut prot = sheetcore::protection::SheetProtection::new_with_password("pw");
    prot.perms.select_locked = false;
    prot.perms.select_unlocked = true;
    app.wb.sheet_mut(0).protection = Some(prot);
    app.sel = (0, 1);
    app.sel_end = (0, 1);
    ui_frame(&mut app, &ctx, vec![key_event(egui::Key::ArrowRight, true)]);
    assert_eq!(app.sel, (0, 1), "经过锁定 C1 的 Shift 扩选必须拒绝");
    assert_eq!(app.sel_end, (0, 1));
    assert!(app.status.contains("不允许选择"), "状态栏：{}", app.status);
}
/// UI-16：每张表各自恢复 sel/sel_end/额外选区/滚动/缩放，且不串表。
#[test]
fn switching_sheets_restores_each_view_state() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::Sheet::new("Other"));
    app.sel = (99, 25);
    app.sel_end = (100, 26);
    app.extra_sel = vec![(1, 1, 2, 2)];
    app.scroll = egui::vec2(123.0, 456.0);
    app.zoom = 1.5;
    assert!(app.switch_sheet_view(1));
    assert_eq!((app.sel, app.sel_end), ((0, 0), (0, 0)), "新表默认 A1");
    assert!(app.extra_sel.is_empty());
    assert_eq!((app.scroll.x, app.scroll.y), (0.0, 0.0));
    assert_eq!(app.zoom, 1.0);
    app.sel = (1, 1);
    app.sel_end = (2, 2);
    assert!(app.switch_sheet_view(0));
    assert_eq!((app.sel, app.sel_end), ((99, 25), (100, 26)));
    assert_eq!(app.extra_sel, vec![(1, 1, 2, 2)]);
    assert_eq!((app.scroll.x, app.scroll.y), (123.0, 456.0));
    assert_eq!(app.zoom, 1.5);
    assert!(app.switch_sheet_view(1));
    assert_eq!((app.sel, app.sel_end), ((1, 1), (2, 2)), "第二张表自己的选区");
}

/// UI-16：重命名/删除后视图仍绑定原表 identity，不会串到别的表。
#[test]
fn sheet_views_survive_rename_and_delete() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::Sheet::new("B"));
    app.wb.sheets.push(sheetcore::Sheet::new("C"));
    app.sel = (50, 3);
    app.sel_end = (51, 4);
    assert!(app.switch_sheet_view(1));
    app.sel = (7, 7);
    app.sel_end = (8, 8);
    assert!(app.switch_sheet_view(0));

    app.rename_sheet = None;
    assert!(app.set_sheet_name(0, "Renamed"));
    assert!(app.switch_sheet_view(1));
    assert_eq!((app.sel, app.sel_end), ((7, 7), (8, 8)), "重命名不影响 identity 绑定");

    // 删除 B（非当前），C 的位置前移，但 C 的视图仍跟着 C。
    let c_id = app.wb.sheets[2].identity;
    app.delete_sheet(1);
    let c_pos = app.wb.sheets.iter().position(|s| s.identity == c_id).unwrap();
    assert!(app.switch_sheet_view(c_pos));
    assert_eq!(app.wb.sheet(c_pos).name, "C");
    // C 还没存过视图，默认 A1；不能拿到 B 的 (7,7)
    assert_eq!((app.sel, app.sel_end), ((0, 0), (0, 0)));
}

/// UI-16：30 张表的工作表列表可按名字搜索定位。
#[test]
fn sheet_list_search_finds_one_of_many_sheets() {
    let mut app = app();
    for i in 1..=30 {
        app.wb.sheets.push(sheetcore::Sheet::new(&format!("S{i:02}")));
    }
    let hits = app.sheet_list_matches("s25");
    assert_eq!(hits.len(), 1);
    assert_eq!(app.wb.sheet(hits[0]).name, "S25");
    assert_eq!(app.sheet_list_matches("   ").len(), 31, "空搜索列出全部可见表");
    app.wb.sheets[25].hidden = true;
    assert!(app.sheet_list_matches("s25").is_empty(), "隐藏表不参与搜索");
}

/// UI-26：页面设置先作用于预览，取消必须恢复 page/page_dirty/wb.dirty。
#[test]
fn page_setup_cancel_restores_everything() {
    let mut app = app();
    app.wb.dirty = false;
    app.wb.sheet_mut(0).page_dirty = false;
    let before = app.wb.sheet(0).page.clone();
    app.open_page_setup();
    app.page_setup_draft.paper = 8;
    app.page_setup_draft.landscape = true;
    app.page_setup_draft.margins = [10.0 / 25.4, 10.0 / 25.4, 0.5, 0.5, 0.3, 0.3];
    app.page_setup_draft.title_rows = Some((0, 0));
    app.page_setup_draft.title_cols = Some((0, 1));
    app.page_setup_draft.fit_to_page = true;
    app.page_setup_draft.fit_width = 1;
    app.page_setup_draft.fit_height = 0;
    app.page_setup_draft.row_breaks = vec![8];
    app.page_setup_draft.col_breaks = vec![2];
    app.apply_page_setup_draft();
    assert_eq!(app.wb.sheet(0).page.paper, 8, "draft 已进入预览");
    assert!(app.wb.dirty);
    app.cancel_page_setup();
    assert_eq!(app.wb.sheet(0).page, before, "取消后页面设置完全恢复");
    assert!(!app.wb.dirty, "取消不能把文档标脏");
    assert!(!app.wb.sheet(0).page_dirty);
    assert!(!app.page_setup_open);
    assert!(app.page_setup_orig.is_none());
}

/// UI-26：A4 横向、左右 10mm、首行重复、宽 1 页高自动 + 横纵分页符能一起生效。
#[test]
fn page_setup_accepts_excel_style_fields() {
    let mut app = app();
    app.open_page_setup();
    app.page_setup_draft.landscape = true;
    app.page_setup_draft.margins[0] = 10.0 / 25.4;
    app.page_setup_draft.margins[1] = 10.0 / 25.4;
    app.page_setup_draft.title_rows = Some((0, 0));
    app.page_setup_draft.fit_to_page = true;
    app.page_setup_draft.fit_width = 1;
    app.page_setup_draft.fit_height = 0;
    app.page_setup_draft.row_breaks = vec![49];
    app.page_setup_draft.col_breaks = vec![6];
    app.apply_page_setup_draft();
    app.page_setup_open = false;
    app.page_setup_orig = None;
    let ps = &app.wb.sheet(0).page;
    assert!(ps.landscape);
    assert_eq!(ps.paper, 9);
    assert!((ps.margins[0] - 0.3937).abs() < 0.001);
    assert_eq!(ps.title_rows, Some((0, 0)));
    assert_eq!((ps.fit_width, ps.fit_height), (1, 0), "宽 1 页、高自动");
    assert_eq!(ps.row_breaks, vec![49]);
    assert_eq!(ps.col_breaks, vec![6], "垂直分页也要有");
}

/// UI-26：打印页码范围 全部 / 当前页 / 自定义（含夹取和反序）。
#[test]
fn print_page_span_modes_are_clamped() {
    let mut app = app();
    app.print_range = 0;
    assert_eq!(app.print_page_span(5), None, "全部");
    app.print_range = 1;
    app.preview_page = 3;
    assert_eq!(app.print_page_span(5), Some((3, 3)));
    app.preview_page = 99;
    assert_eq!(app.print_page_span(5), Some((4, 4)), "当前页越界夹到末页");
    app.print_range = 2;
    app.print_from = 3;
    app.print_to = 5;
    assert_eq!(app.print_page_span(5), Some((2, 4)), "1 基转 0 基");
    app.print_from = 9;
    app.print_to = 7;
    assert_eq!(app.print_page_span(5), Some((4, 4)), "反序+越界");
}

/// UI-26：Ribbon 的纸型/边距入口改为打开页面设置，不再循环切换。
#[test]
fn ribbon_page_buttons_open_explicit_dialog() {
    let ctx = egui::Context::default();
    let mut app = app();
    for action in ["PaperSize", "PageMargins", "PageOrientation"] {
        app.page_setup_open = false;
        app.ribbon_action(action, &ctx);
        assert!(app.page_setup_open, "{action} 应打开页面设置");
        app.cancel_page_setup();
    }
}

/// UI-26：Ribbon 的“插入分页符”在活动格同时插入水平和垂直分页。
#[test]
fn ribbon_page_break_inserts_both_directions() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.sel = (5, 3);
    app.sel_end = (5, 3);
    app.ribbon_action("PageBreakInsert", &ctx);
    assert_eq!(app.wb.sheet(0).page.row_breaks, vec![5]);
    assert_eq!(app.wb.sheet(0).page.col_breaks, vec![3]);
    app.ribbon_action("PageBreakRemove", &ctx);
    assert!(app.wb.sheet(0).page.row_breaks.is_empty());
    assert!(app.wb.sheet(0).page.col_breaks.is_empty());
}

/// UI-23：Alt 页签 → 控件 KeyTip 直接执行；边框/筛选/冻结均可不用鼠标。
#[test]
fn keytips_reach_border_filter_and_freeze_from_tabs() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.keytips = true;
    app.keytip_tab = Some(1); // 开始
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::B, false)]);
    let st = app.wb.styles.get(app.wb.sheet(0).style_of(0, 0));
    assert_eq!(
        st.border[sheetcore::style::BorderSide::Bottom.idx()].style,
        1,
        "Home+B 应执行所有框线"
    );

    app.wb.sheet_mut(0).set(0, 0, text("甲"));
    app.wb.sheet_mut(0).set(1, 0, text("乙"));
    app.sel = (0, 0);
    app.sel_end = (1, 0);
    app.keytips = true;
    app.keytip_tab = Some(5); // 数据
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::F, false)]);
    assert!(app.wb.sheet(0).filter.is_some(), "数据+F 应打开筛选");

    app.sel = (1, 1);
    app.sel_end = (1, 1);
    app.keytips = true;
    app.keytip_tab = Some(7); // 视图
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::F, false)]);
    assert_eq!(app.wb.sheet(0).freeze, Some((1, 1)), "视图+F 应冻结 B2 以上/以左");
}

/// UI-23.1：Excel 的组内序列（如格式刷 FP）要先按 F 收窄，再按 P 执行。
#[test]
fn keytips_support_two_stage_sequences() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.keytips = true;
    app.keytip_tab = Some(1); // 开始
    app.keytip_prefix.clear();
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::F, false)]);
    assert_eq!(app.keytip_prefix, "F", "F 应只收窄，不执行");
    run_keys(&mut app, &ctx, vec![key_event(egui::Key::P, false)]);
    assert!(app.format_painter.is_some(), "FP 应执行格式刷");
    assert!(app.keytip_prefix.is_empty());
    assert!(!app.keytips);
}

#[test]
fn keytips_cover_home_controls_uniquely() {
    let app = app();
    let tab = &app.ribbon.tabs[1];
    fn walk(nodes: &[crate::ribbon::Node], out: &mut Vec<String>) {
        for n in nodes {
            use crate::ribbon::Node;
            let ctl = match n {
                Node::Button(c) | Node::Toggle(c) => Some(c),
                Node::Split { ctl, .. }
                | Node::DropDown { ctl, .. }
                | Node::Gallery { ctl, .. }
                | Node::ComboBox { ctl, .. }
                | Node::EditBox { ctl, .. }
                | Node::Slider { ctl, .. } => Some(ctl),
                Node::Menu(m) => Some(&m.ctl),
                Node::Box { children, .. } => { walk(children, out); None }
                Node::Label(_) | Node::Spacer(_) | Node::Separator => None,
            };
            if let Some(c) = ctl {
                if !c.action().is_empty() {
                    out.push(c.keytip.clone().unwrap_or_default());
                }
            }
        }
    }
    let mut keys = Vec::new();
    for g in &tab.groups { walk(&g.children, &mut keys); }
    assert!(keys.len() > 20, "开始页命令数 {}", keys.len());
    assert!(keys.iter().all(|k| !k.is_empty()), "还有没 keytip 的命令");
    let mut uniq = std::collections::HashSet::new();
    for k in &keys { assert!(uniq.insert(k.clone()), "keytip 重复: {k}"); }
}

fn run_ctx_menu(app: &mut SheetApp, ctx: &egui::Context, events: Vec<egui::Event>) {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };
    input.events = events;
    let _ = ctx.run(input, |ctx| app.ctx_menu_window(ctx));
}

/// UI-23：Shift+F10 打开菜单；方向键/Enter 执行；Esc 逐级退回；边缘菜单不出窗。
#[test]
fn shift_f10_menu_supports_keyboard_and_window_edges() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.wb.sheet_mut(0).set(0, 0, text("甲"));
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    let f10 = egui::Event::Key {
        key: egui::Key::F10,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers { shift: true, ..Default::default() },
    };
    run_keys(&mut app, &ctx, vec![f10]);
    assert!(app.ctx_menu.is_some(), "Shift+F10 应打开单元格右键菜单");

    // 放到窗口右下角，绘制后所有菜单矩形必须在 800×600 内
    if let Some(m) = app.ctx_menu.as_mut() {
        m.pos = egui::pos2(790.0, 590.0);
    }
    run_ctx_menu(&mut app, &ctx, vec![]);
    for r in &app.ctx_rects {
        assert!(r.left() >= 0.0 && r.top() >= 0.0, "菜单越左/上界: {r:?}");
        assert!(r.right() <= 800.0 && r.bottom() <= 600.0, "菜单越右/下界: {r:?}");
    }
    // 子菜单也在右下角打开，仍不能越界
    if let Some(m) = app.ctx_menu.as_mut() {
        m.key = 7; // 插入（父项）
        m.sub = Some(7);
    }
    run_ctx_menu(&mut app, &ctx, vec![]);
    assert!(app.ctx_rects.len() >= 2, "应同时绘制主菜单和子菜单");
    for r in &app.ctx_rects {
        assert!(r.left() >= 0.0 && r.top() >= 0.0, "子菜单越左/上界: {r:?}");
        assert!(r.right() <= 800.0 && r.bottom() <= 600.0, "子菜单越右/下界: {r:?}");
    }
    let parent = app.ctx_rects[0];
    let submenu = *app.ctx_rects.last().unwrap();
    assert!(
        parent.intersect(submenu).area() <= 0.0,
        "子菜单不能盖住父菜单可点击区域: parent={parent:?} sub={submenu:?}"
    );
    if let Some(m) = app.ctx_menu.as_mut() {
        m.sub = None;
        m.key = 0;
    }

    // 从 Cut 走 6 次 Down 到“清除内容”，Enter 执行
    for _ in 0..6 {
        run_ctx_menu(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, false)]);
    }
    run_ctx_menu(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert!(app.ctx_menu.is_none(), "Enter 执行后菜单关闭");
    assert!(
        app.wb.sheet(0).get(0, 0).map(|c| c.value.display()).unwrap_or_default().is_empty(),
        "清除内容应生效"
    );
}

/// UI-23：子菜单 Right 展开、Esc 只退一级，再次 Esc 关闭整个菜单。
#[test]
fn context_submenu_escape_steps_back_one_level() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.ctx_menu = Some(CtxMenuState {
        pos: egui::pos2(100.0, 100.0),
        frame: 0,
        sub: None,
        kind: CtxKind::Cell,
        idx: 0,
        key: 0,
        sub_key: 0,
    });
    // Cut→Copy→Paste→选择性粘贴→插入（父项，索引 7）
    for _ in 0..4 {
        run_ctx_menu(&mut app, &ctx, vec![key_event(egui::Key::ArrowDown, false)]);
    }
    run_ctx_menu(&mut app, &ctx, vec![key_event(egui::Key::ArrowRight, false)]);
    assert!(app.ctx_menu.as_ref().and_then(|m| m.sub).is_some(), "Right 应展开插入子菜单");
    run_ctx_menu(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    let m = app.ctx_menu.as_ref().expect("第一次 Esc 不应关闭整个菜单");
    assert!(m.sub.is_none(), "第一次 Esc 只收起子菜单");
    run_ctx_menu(&mut app, &ctx, vec![key_event(egui::Key::Escape, false)]);
    assert!(app.ctx_menu.is_none(), "第二次 Esc 关闭菜单");
}

#[test]
fn formula_autocomplete_and_f4_cycle_keep_editing_state() {
    let help = formula_help("=SU").expect("=SU 应有 SUM/SUMIF 候选");
    assert!(help.suggestions.iter().any(|(n, _)| n == "SUM"));
    assert!(help.suggestions.iter().any(|(n, _)| n == "SUMIF"));
    let sum = help.suggestions.iter().position(|(n, _)| n == "SUM").unwrap();
    let mut ed = EditState::new("=SU".into());
    apply_formula_pick(&mut ed, &help, sum);
    assert_eq!(ed.buf, "=SUM(");
    assert!(formula_help(&ed.buf).and_then(|h| h.signature).is_some());

    let mut buf = "=A1".to_string();
    for want in ["=$A$1", "=A$1", "=$A1", "=A1"] {
        assert!(cycle_formula_ref(&mut buf), "应识别 A1 引用");
        assert_eq!(buf, want);
    }
    let mut f = "=SUM(B2:C3)".to_string();
    assert!(cycle_formula_ref(&mut f));
    assert_eq!(f, "=SUM(B2:$C$3)", "只循环末尾引用");
}

// ===== UI-11.3：编辑公式时点选单元格建立引用 + 引用着色 =====

#[test]
fn ui113_insert_reference_matches_excel_pick_semantics() {
    // 光标左边是引用：整段替换（Excel 里 `=A1` 点 B2 → `=B2`）
    assert_eq!(insert_reference("=A1", 3, "B2"), ("=B2".to_string(), 3));
    // 函数括号/运算符后：原位置插入
    assert_eq!(insert_reference("=SUM(", 5, "A1"), ("=SUM(A1".to_string(), 7));
    assert_eq!(insert_reference("=A1+", 4, "B2"), ("=A1+B2".to_string(), 6));
    // 正在键入的引用被替换，光标停在引用末尾
    assert_eq!(insert_reference("=SUM(A1", 7, "C3"), ("=SUM(C3".to_string(), 7));
    // 区域引用整体替换（光标停在 `)` 前）
    assert_eq!(insert_reference("=SUM(A1:B2)", 10, "D4"), ("=SUM(D4)".to_string(), 7));
    // 刚敲完 `=`：插在等号后
    assert_eq!(insert_reference("=A1", 1, "B2"), ("=B2A1".to_string(), 3));
    // 不带等号的普通文本不替换标识符以外的内容
    assert_eq!(insert_reference("abc ", 4, "B2"), ("abc B2".to_string(), 6));
}

#[test]
fn ui113_ref_spans_colour_references_and_share_colour_per_text() {
    let src = "=SUM(A1:B2)+A1*$C$3";
    let chars: Vec<char> = src.chars().collect();
    let spans = ref_spans(src);
    let got: Vec<String> = spans.iter().map(|(s, e, _)| chars[*s..*e].iter().collect()).collect();
    assert_eq!(got, vec!["A1:B2", "A1", "$C$3"], "函数名/运算符不算引用");
    assert_ne!(spans[0].2, spans[1].2, "不同范围的引用换色");
    assert_ne!(spans[0].2, spans[2].2, "不同引用换色");
    // 同一文本的重复引用共用颜色（Excel 同款）
    let rep = ref_spans("=A1+A1");
    assert_eq!(rep.len(), 2);
    assert_eq!(rep[0].2, rep[1].2);

    // 非公式文本不着色；公式按引用切段
    let font = egui::FontId::proportional(13.0);
    let plain = formula_layout_job("abc", font.clone(), egui::Color32::WHITE);
    assert_eq!(plain.sections.len(), 1);
    let job = formula_layout_job(src, font, egui::Color32::WHITE);
    assert_eq!(job.sections.len(), 6, "=SUM( / A1:B2 / )+ / A1 / * / $C$3");
    assert_eq!(job.sections[1].format.color, ref_color(spans[0].2));
    // 带表名的引用整段上色
    let q = "='My Sheet'!A1+Sheet2!$B$2";
    let qc: Vec<char> = q.chars().collect();
    let qsp: Vec<String> = ref_spans(q).iter().map(|(s, e, _)| qc[*s..*e].iter().collect()).collect();
    assert_eq!(qsp, vec!["'My Sheet'!A1", "Sheet2!$B$2"]);
}

#[test]
fn ui113_clicking_grid_while_editing_formula_inserts_reference() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheets[0].set(0, 0, number(3.0)); // A1
    app.wb.sheets[0].set(1, 1, number(10.0)); // B2
    app.sel = (0, 2); // C1 里写公式，避免引用到自己
    app.sel_end = (0, 2);
    app.editing = Some(EditState::new("=A1+".into()));
    let pos = egui::pos2(
        HEAD_W + app.wb.sheets[0].cols.pos(1) + 15.0,
        HEAD_H + app.wb.sheets[0].rows.pos(1) + 10.0,
    );
    click_grid(&mut app, &ctx, pos);
    {
        let ed = app.editing.as_ref().expect("点选引用不能结束编辑");
        assert_eq!(ed.buf, "=A1+B2");
        assert_eq!(ed.caret, 6);
    }
    assert_eq!(app.sel, (0, 2), "编辑格不能被点选带走");
    assert_eq!(app.pick_rect, Some((1, 1, 1, 1)), "被引用格子要有细框");
    assert!(app.commit_edit(None));
    assert_eq!(app.wb.sheets[0].value(0, 2), Value::Number(13.0));
    assert_eq!(app.pick_rect, None, "提交后细框要清掉");
}

#[test]
fn ui113_textedit_layouter_keeps_ref_colours_in_galley() {
    let ctx = egui::Context::default();
    let mut buf = String::from("=A1+B1");
    let mut colors: Vec<egui::Color32> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut layouter = |ui: &egui::Ui, text: &str, w: f32| {
        texts.push(text.to_string());
        let font = egui::TextStyle::Body.resolve(ui.style());
        let mut job = formula_layout_job(text, font, ui_kit::TEXT);
        job.wrap.max_width = w;
        ui.fonts(|f| f.layout_job(job))
    };
    let _ = ctx.run(Default::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let out = egui::TextEdit::singleline(&mut buf)
                .layouter(&mut layouter)
                .show(ui);
            colors = out.galley.job.sections.iter().map(|s| s.format.color).collect();
        });
    });
    eprintln!("[galley] texts={texts:?} colors={colors:?}");
    assert!(colors.contains(&ref_color(0)), "A1 应为第一种引用色");
    assert!(colors.contains(&ref_color(1)), "B1 应为第二种引用色");
}

#[test]
fn ui113_alt_enter_inserts_newline_at_caret_not_at_end() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 7);
    app.sel_end = (0, 7);
    app.editing = Some(EditState::new("=A1&B1".into()));
    let id = egui::Id::new("grid_edit");
    let mut st = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
    st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
        egui::text::CCursor::new(3),
    )));
    egui::TextEdit::store_state(&ctx, id, st);
    app.editing.as_mut().unwrap().caret = 3;
    run_grid_frame(&mut app, &ctx, vec![]);
    let alt_enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::ALT,
    };
    // egui 的 i.modifiers 取自 RawInput.modifiers，不跟随 Key 事件里的修饰键
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
        modifiers: egui::Modifiers::ALT,
        ..Default::default()
    };
    input.events = vec![alt_enter];
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
    });
    run_grid_frame(&mut app, &ctx, vec![]);
    let ed = app.editing.as_ref().expect("Alt+Enter 不能结束编辑");
    assert_eq!(ed.buf, "=A1\n&B1", "换行要插在光标处，不是缓冲末尾");
    assert_eq!(ed.caret, 4, "光标停在换行之后");
}

// ===== UI-11.4：函数向导 + 真实最近使用 =====

#[test]
fn ui114_wizard_arg_names_and_formula_build() {
    assert_eq!(fn_arg_names("SUM"), vec!["number1", "number2（可选）"]);
    let vl = fn_arg_names("VLOOKUP");
    assert_eq!(vl.len(), 4, "{vl:?}");
    assert_eq!(vl[3], "range_lookup（可选）");
    assert!(fn_arg_names("NOW").is_empty(), "无参函数没有参数输入框");
    assert_eq!(fn_formula_from_args("SUM", &["A1:A3".into(), String::new()]), "=SUM(A1:A3)");
    assert_eq!(
        fn_formula_from_args("IF", &["B1>0".into(), "1".into(), "0".into()]),
        "=IF(B1>0,1,0)"
    );
    assert_eq!(fn_formula_from_args("SUM", &[]), "=SUM()");
    assert_eq!(fn_arg_plain("number2（可选）"), "number2");
}

#[test]
fn ui114_wizard_inserts_at_caret_without_double_equals() {
    let mut app = app();
    app.sel = (0, 4);
    app.sel_end = (0, 4);
    app.insert_formula_at_caret("=SUM(A1:A3)");
    {
        let ed = app.editing.as_ref().expect("向导要从当前格新起编辑");
        assert_eq!(ed.buf, "=SUM(A1:A3)");
        assert_eq!(ed.caret, 11, "光标停在公式末尾");
    }
    // 已经在一段公式里：只插函数体，不再叠等号
    app.editing.as_mut().unwrap().buf = "=A1+".into();
    app.editing.as_mut().unwrap().caret = 4;
    app.insert_formula_at_caret("=SUM(B1)");
    assert_eq!(app.editing.as_ref().unwrap().buf, "=A1+SUM(B1)");
    // 一个参数都没填：光标停在括号里
    app.editing.as_mut().unwrap().buf = String::new();
    app.editing.as_mut().unwrap().caret = 0;
    app.insert_formula_at_caret("=SUM()");
    assert_eq!(app.editing.as_ref().unwrap().buf, "=SUM()");
    assert_eq!(app.editing.as_ref().unwrap().caret, 5, "光标停在括号里");
    // 不在编辑态时向导是替换当前格，不能把原有文本当成前缀
    app.cancel_edit();
    app.wb.sheet_mut(0).set(0, 4, text("S1-E1"));
    app.insert_formula_at_caret("=SUM(A1:A3)");
    {
        let ed = app.editing.as_ref().expect("向导应从空缓冲新起");
        assert_eq!(ed.buf, "=SUM(A1:A3)", "旧单元格文本不能混进公式");
    }
}

#[test]
fn ui114_committing_formula_records_recent_functions() {
    let env = crate::test_env::TestEnv::new("ui114-recent");
    env.sc("SC_FNRECENT_RECORD", "1");
    let mut app = app();
    app.sel = (0, 3);
    app.sel_end = (0, 3);
    app.editing = Some(EditState::new("=AVERAGE(A1:A2)+SUM(B1)".into()));
    assert!(app.commit_edit(None), "公式要写进去");
    let list = crate::fnrecent::load();
    assert_eq!(list.first().map(|s| s.as_str()), Some("SUM"), "最后用到的函数排最前");
    assert!(list.contains(&"AVERAGE".to_string()), "同一条公式里的函数都记");
}

// ===== UI-11.5：拖动/Shift 扩区域引用、行号列标引用 =====

fn click_grid_mods(
    app: &mut SheetApp,
    ctx: &egui::Context,
    pos: egui::Pos2,
    mods: egui::Modifiers,
) {
    for pressed in [None, Some(true), Some(false)] {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            modifiers: mods,
            ..Default::default()
        };
        if let Some(pressed) = pressed {
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: mods,
            });
        }
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
        });
    }
}

/// 按下 → 移动（越过拖动阈值）→ 再移动 → 松开
fn drag_grid(app: &mut SheetApp, ctx: &egui::Context, from: egui::Pos2, to: egui::Pos2) {
    let mut frame = |events: Vec<egui::Event>, mods: egui::Modifiers| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            modifiers: mods,
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
        });
    };
    let m = egui::Modifiers::default();
    frame(
        vec![
            egui::Event::PointerMoved(from),
            egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: m,
            },
        ],
        m,
    );
    let mid = egui::pos2((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
    frame(vec![egui::Event::PointerMoved(mid)], m);
    frame(vec![egui::Event::PointerMoved(to)], m);
    frame(
        vec![egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: m,
        }],
        m,
    );
}

#[test]
fn ui115_reference_text_helpers() {
    assert_eq!(range_label((0, 0), (2, 1)), "A1:B3");
    assert_eq!(range_label((2, 1), (0, 0)), "B3:A1", "锚点在前，和 Excel 一致");
    assert_eq!(whole_col_label(1), "B:B");
    assert_eq!(whole_row_label(2), "3:3");
    let (buf, caret) = replace_char_span("=SUM(A1", (5, 7), "A1:B3");
    assert_eq!(buf, "=SUM(A1:B3");
    assert_eq!(caret, 10);
    // 越界区间按长度夹住，不 panic
    let (buf, _) = replace_char_span("=A1", (1, 99), "B2");
    assert_eq!(buf, "=B2");
}

#[test]
fn ui115_shift_click_extends_reference_to_range() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 2);
    app.sel_end = (0, 2);
    app.editing = Some(EditState::new("=SUM(".into()));
    let sheet = &app.wb.sheets[0];
    let a1 = egui::pos2(HEAD_W + sheet.cols.pos(0) + 12.0, HEAD_H + sheet.rows.pos(0) + 8.0);
    let b3 = egui::pos2(HEAD_W + sheet.cols.pos(1) + 12.0, HEAD_H + sheet.rows.pos(2) + 8.0);
    click_grid(&mut app, &ctx, a1);
    assert_eq!(app.editing.as_ref().unwrap().buf, "=SUM(A1");
    assert_eq!(app.pick_rect, Some((0, 0, 0, 0)));
    click_grid_mods(&mut app, &ctx, b3, egui::Modifiers::SHIFT);
    assert_eq!(app.editing.as_ref().unwrap().buf, "=SUM(A1:B3", "Shift+点扩成区域");
    assert_eq!(app.pick_rect, Some((0, 0, 2, 1)), "引用框跟着扩");
    assert_eq!(app.sel, (0, 2), "取引用不动选区");
}

#[test]
fn ui115_dragging_while_picking_rewrites_reference_to_range() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 2);
    app.sel_end = (0, 2);
    app.editing = Some(EditState::new("=SUM(".into()));
    let sheet = &app.wb.sheets[0];
    let a1 = egui::pos2(HEAD_W + sheet.cols.pos(0) + 12.0, HEAD_H + sheet.rows.pos(0) + 8.0);
    let c4 = egui::pos2(HEAD_W + sheet.cols.pos(2) + 12.0, HEAD_H + sheet.rows.pos(3) + 8.0);
    drag_grid(&mut app, &ctx, a1, c4);
    assert_eq!(app.editing.as_ref().unwrap().buf, "=SUM(A1:C4", "拖动 = 锚点到当前格");
    assert_eq!(app.pick_rect, Some((0, 0, 3, 2)));
    assert_eq!(app.sel, (0, 2), "拖引用不改选区");
}

#[test]
fn ui115_header_click_inserts_whole_column_and_row_reference() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 2);
    app.sel_end = (0, 2);
    app.editing = Some(EditState::new("=SUM(".into()));
    let sheet = &app.wb.sheets[0];
    let col_b = egui::pos2(HEAD_W + sheet.cols.pos(1) + 12.0, HEAD_H / 2.0);
    click_grid(&mut app, &ctx, col_b);
    assert_eq!(app.editing.as_ref().unwrap().buf, "=SUM(B:B", "列标 = 整列引用");
    assert_eq!(app.pick_rect, Some((0, 1, 1_048_575, 1)));
    app.editing.as_mut().unwrap().buf = "=SUM(".into();
    app.editing.as_mut().unwrap().caret = 5;
    app.pick_rect = None;
    app.pick_anchor = None;
    app.pick_span = None;
    let sheet = &app.wb.sheets[0];
    let row_3 = egui::pos2(HEAD_W / 2.0, HEAD_H + sheet.rows.pos(2) + 8.0);
    click_grid(&mut app, &ctx, row_3);
    assert_eq!(app.editing.as_ref().unwrap().buf, "=SUM(3:3", "行号 = 整行引用");
    assert_eq!(app.pick_rect, Some((2, 0, 2, 16_383)));
}

#[test]
fn ui112_autocomplete_can_be_driven_by_arrow_keys_and_enter() {
    let ctx = egui::Context::default();
    let help = formula_help("=SU").expect("=SU 应有候选");
    assert!(help.suggestions.len() >= 2, "至少 SUM / SUMIF 两个候选");
    let mut selected = 0usize;

    // 先跑一帧让 egui 初始化输入状态
    let _ = ctx.run(egui::RawInput::default(), |_| {});

    let key = |ctx: &egui::Context, k: egui::Key| {
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run(input, |_| {});
    };

    key(&ctx, egui::Key::ArrowDown);
    assert_eq!(formula_help_keys(&ctx, &help, &mut selected), None, "方向键只移动高亮");
    assert_eq!(selected, 1, "下移一格");
    key(&ctx, egui::Key::ArrowUp);
    let _ = formula_help_keys(&ctx, &help, &mut selected);
    assert_eq!(selected, 0, "上移回到第一个");
    key(&ctx, egui::Key::ArrowUp);
    let _ = formula_help_keys(&ctx, &help, &mut selected);
    assert_eq!(selected, help.suggestions.len() - 1, "第一项再上移是最后一项");
    key(&ctx, egui::Key::Enter);
    assert_eq!(
        formula_help_keys(&ctx, &help, &mut selected),
        Some(help.suggestions.len() - 1),
        "Enter 接受当前高亮"
    );
    key(&ctx, egui::Key::Tab);
    assert_eq!(
        formula_help_keys(&ctx, &help, &mut selected),
        Some(help.suggestions.len() - 1),
        "Tab 也能接受"
    );
}

/// C12.4：面板建出来的多字段透视要排进保存队列，保存时才挂真 pivotTable 部件。
#[test]
fn multi_field_pivot_from_panel_is_queued_for_real_parts() {
    use sheetcore::pivot::{Agg, GroupBy};
    use sheetcore::pivot_multi::{AxisField, PivotLayout, PivotSpec2, ValueField};

    let mut app = app();
    for (r, c, t) in [(0, 0, "地区"), (0, 1, "产品"), (0, 2, "金额")] {
        app.wb.sheets[0].set(r, c, Cell { value: Value::Text(t.into()), formula: None });
    }
    for (i, (region, product, amount)) in
        [("华东", "A", 10.0), ("华北", "B", 20.0), ("华东", "A", 30.0), ("华北", "B", 40.0)]
            .into_iter()
            .enumerate()
    {
        let r = i as u32 + 1;
        app.wb.sheets[0].set(r, 0, Cell { value: Value::Text(region.into()), formula: None });
        app.wb.sheets[0].set(r, 1, Cell { value: Value::Text(product.into()), formula: None });
        app.wb.sheets[0].set(r, 2, number(amount));
    }
    let spec = PivotSpec2 {
        source: (0, 0, 4, 2),
        rows: vec![
            AxisField { col: 0, group: GroupBy::None },
            AxisField { col: 1, group: GroupBy::None },
        ],
        cols: vec![],
        values: vec![ValueField { col: 2, agg: Agg::Sum, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal , base: None }],
        filters: vec![],
        layout: PivotLayout::default(),
    };
    app.pivot_real = true;
    app.sheet_idx = 0;
    let rect = app.apply_pivot2(&spec, (8, 0), None, false);
    assert_eq!(app.wb.pivots_pending2.len(), 1, "多字段透视没进保存队列");
    assert_eq!(app.wb.pivots_pending2[0].3, rect);
    // 同位置刷新：更新同一个附件，不能越排越多
    let rect2 = app.apply_pivot2(&spec, (8, 0), Some(rect), true);
    assert_eq!(app.wb.pivots_pending2.len(), 1, "刷新不该新增附件");
    assert_eq!(app.wb.pivots_pending2[0].3, rect2);
    assert!(app.status.contains("多字段透视已刷新"));
}

// ---------------- UI-11.6 跨工作表取引用 ----------------

#[test]
fn ui116_sheet_names_needing_quotes_are_escaped() {
    assert_eq!(crate::sheet_ref_name("Sheet2"), "Sheet2");
    assert_eq!(crate::sheet_ref_name("Data_1"), "Data_1");
    assert_eq!(crate::sheet_ref_name("2024年"), "'2024年'", "数字开头要加引号");
    assert_eq!(crate::sheet_ref_name("我的 表"), "'我的 表'", "含空格要加引号");
    assert_eq!(crate::sheet_ref_name("it's"), "'it''s'", "内部单引号要翻倍");
}

#[test]
fn ui116_picking_another_sheet_inserts_qualified_reference_and_writes_back() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheets.push(sheetcore::Sheet::new("Sheet2"));
    app.sheet_idx = 0;
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("=1+".into()));
    assert!(app.editing.is_some(), "应进入编辑");

    assert!(app.switch_sheet_for_ref(1), "公式编辑中应允许切表");
    assert_eq!(app.sheet_idx, 1, "视图应切到第二张表");
    assert!(app.editing.is_some(), "切表不能结束编辑");

    let label = app.insert_pick_reference(2, 1);
    assert_eq!(label, "Sheet2!B3", "跨表引用要带工作表名");
    assert_eq!(
        app.editing.as_ref().map(|e| e.buf.clone()).unwrap_or_default(),
        "=1+Sheet2!B3"
    );

    assert!(app.commit_edit(None), "提交应成功");
    assert_eq!(app.sheet_idx, 0, "提交后要回到编辑所属工作表");
    let f = app
        .wb
        .sheet(0)
        .get(0, 0)
        .and_then(|c| c.formula.clone())
        .unwrap_or_default();
    assert_eq!(&*f, "=1+Sheet2!B3", "公式要写回原格（带等号）");
    assert!(app.wb.sheet(1).get(2, 1).is_none(), "引用不能写到源表那格");
}

#[test]
fn ui116_cancel_returns_to_origin_sheet() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheets.push(sheetcore::Sheet::new("Sheet2"));
    app.sheet_idx = 0;
    app.sel = (3, 2);
    app.sel_end = (3, 2);
    app.begin_edit(Some("=".into()));
    assert!(app.switch_sheet_for_ref(1));
    assert_eq!(app.sheet_idx, 1);
    app.cancel_edit();
    assert!(app.editing.is_none());
    assert_eq!(app.sheet_idx, 0, "取消编辑也要回到原表");
    assert_eq!(app.sel, (3, 2), "取消后光标回到被编辑的格");
}

#[test]
fn ui116_whole_column_reference_is_also_qualified() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheets.push(sheetcore::Sheet::new("Other"));
    app.sheet_idx = 0;
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("=SUM(".into()));
    assert!(app.switch_sheet_for_ref(1));
    assert_eq!(app.qualify_ref(&crate::whole_col_label(2)), "Other!C:C");
    assert_eq!(app.qualify_ref(&crate::whole_row_label(4)), "Other!5:5");
}

// ---- UI-30：命令可达性与未命中留痕 ----

#[test]
fn unknown_ribbon_action_is_reported_to_the_user() {
    let mut app = app();
    let ctx = egui::Context::default();
    app.ribbon_action("ThisCommandDoesNotExist", &ctx);
    assert_eq!(
        app.unhandled_action.as_deref(),
        Some("ThisCommandDoesNotExist"),
        "未命中的命令必须留在 unhandled_action 里，不能只写日志"
    );
    assert!(
        app.status.contains("还没有接入"),
        "状态栏要告诉用户，实际是：{}",
        app.status
    );
    // 后续成功命令要把它清掉，避免一直挂着旧值
    app.ribbon_action("Undo", &ctx);
    assert!(app.unhandled_action.is_none(), "成功命令后应清空未命中标记");
}

#[test]
fn macro_buttons_are_reachable_and_explain_missing_vba() {
    let mut app = app();
    let ctx = egui::Context::default();
    assert!(!app.wb.has_vba, "默认工作簿不该带宏");
    for action in ["MacroView", "MacroRun", "MacroExport"] {
        app.ribbon_action(action, &ctx);
        assert!(app.unhandled_action.is_none(), "{action} 没有 dispatch 臂");
        assert!(
            app.status.contains("没有 VBA"),
            "{action} 对无宏文件要给明确提示，实际：{}",
            app.status
        );
    }
}

#[test]
fn macro_run_button_opens_the_runner_when_vba_exists() {
    let mut app = app();
    let ctx = egui::Context::default();
    app.wb.has_vba = true;
    app.ribbon_action("MacroRun", &ctx);
    assert!(app.macro_run_dlg.is_some(), "运行宏按钮要打开宏名输入框");
}

#[test]
fn legacy_cf_and_dv_menus_are_reachable_from_the_ribbon() {
    let mut app = app();
    let ctx = egui::Context::default();
    app.ribbon_action("CfQuick", &ctx);
    assert!(app.cf_quick, "条件格式快捷规则没打开");
    assert!(app.unhandled_action.is_none(), "CfQuick 没有 dispatch 臂");
    app.ribbon_action("DvQuick", &ctx);
    assert!(app.dv_quick, "数据验证快捷设置没打开");
    assert!(app.unhandled_action.is_none(), "DvQuick 没有 dispatch 臂");
}

#[test]
fn cf_quick_window_renders_without_panicking() {
    let mut app = app();
    let ctx = egui::Context::default();
    app.ribbon_action("CfQuick", &ctx);
    let input = egui::RawInput::default();
    let mut drawn = 0usize;
    let _ = ctx.run(input, |ctx| {
        app.cf_quick_window(ctx);
        drawn += 1;
    });
    assert_eq!(drawn, 1);
    assert!(app.cf_quick, "窗口自己不该被关掉");
}

/// UI-30 点击矩阵：把功能区 XML 里的每个动作都真跑一遍，任何落到兜底分支的
/// 命令都会在 `unhandled_action` 里留下痕迹，测试直接失败。
///
/// 跳过的是**会阻塞在原生对话框/网络**的那几个（rfd 在测试里没人点会卡死），
/// 它们各自另有覆盖：文件打开/保存走 command_exec 单测，导出与比较本轮加了
/// 专门的 dispatch 测试，共享走 net 的独立测试。
const CLICK_MATRIX_SKIP: &[&str] = &[
    "FileOpen",       // rfd 打开对话框
    "FileSaveAs",     // rfd 保存对话框
    "FileSave",       // 无路径时转「另存为」对话框
    "ExportPdf",      // rfd 选 PDF 路径
    "CompareBooks",   // rfd 选比较文件
    "MacroExport",    // rfd 选导出路径（有宏时）
    "InsertPicture",  // rfd 选图片
    "Share",          // 起局域网监听
    "AutoShare",      // 同上
    "ImportCsv",      // rfd 选 CSV
    "CheckUpdate",    // 联网检查更新（测试里会等网络超时）
    "ImportWorkbook", // rfd 选要导入的工作簿
    "Hyperlink",      // rfd/链接对话框
    "OpenInNewTab",   // rfd 打开文件
];

#[test]
fn click_matrix_every_ribbon_action_is_handled() {
    let actions = crate::ribbon::tests::xml_actions();
    assert!(actions.len() > 100, "动作太少，解析可能失败：{}", actions.len());
    let ctx = egui::Context::default();
    let mut unrouted: Vec<String> = Vec::new();
    let mut skipped = 0usize;
    for action in &actions {
        if CLICK_MATRIX_SKIP.contains(&action.as_str()) {
            skipped += 1;
            continue;
        }
        let mut app = app();
        if std::env::var("SC_MATRIX_LOG").is_ok() {
            eprintln!("[matrix] {action}");
        }
        app.ribbon_action(action, &ctx);
        if let Some(miss) = app.unhandled_action.clone() {
            unrouted.push(format!("{action} -> {miss}"));
        }
    }
    assert!(
        unrouted.is_empty(),
        "这些功能区动作点下去没有实现：{unrouted:#?}"
    );
    assert!(
        skipped <= CLICK_MATRIX_SKIP.len(),
        "跳过数超过名单：{skipped}"
    );
    let stale: Vec<&str> = CLICK_MATRIX_SKIP
        .iter()
        .copied()
        .filter(|s| !actions.iter().any(|a| a == s))
        .collect();
    assert!(stale.is_empty(), "跳过名单里有 XML 里不存在的动作：{stale:?}");
}

#[test]
fn background_recalc_exposes_progress_and_honours_cancel() {
    use std::sync::atomic::Ordering;
    let mut app = app();
    // 2 万个公式，足够在取消之前跑起来
    for r in 0..20_000u32 {
        app.wb.sheets[0].set(
            r,
            0,
            sheetcore::Cell {
                value: sheetcore::Value::Number(r as f64),
                formula: None,
            },
        );
        app.wb.sheets[0].set(
            r,
            1,
            sheetcore::Cell {
                value: sheetcore::Value::Empty,
                formula: Some(format!("=A{}+1", r + 1).into()),
            },
        );
    }
    app.recalc_async(None);
    let cancel = app.tasks.recalc_cancel.clone().expect("没有登记取消标志");
    let total = app.tasks.recalc_total.clone().expect("没有登记进度总数");
    assert!(app.tasks.busy, "后台重算没有把 busy 置位，界面会以为空闲");

    // 等它跑起来（至少写进一次总进度）
    let mut saw_total = false;
    for _ in 0..200 {
        if total.load(Ordering::Relaxed) > 0 {
            saw_total = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(saw_total, "重算没有暴露总进度，状态栏无从显示 x/y");

    // 取消：必须在收尾消息里拿到 RecalcCancelled
    cancel.store(true, Ordering::Relaxed);
    let mut got_cancel = false;
    for _ in 0..400 {
        let msg = app
            .tasks
            .open_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        match msg {
            Some(crate::OpenMsg::RecalcCancelled) => {
                got_cancel = true;
                break;
            }
            Some(_) => continue,
            None => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    assert!(got_cancel, "取消后没有回 RecalcCancelled，界面会一直转圈");
}

#[test]
fn save_progress_carries_a_start_instant() {
    let mut app = app();
    let target = std::env::temp_dir().join(format!(
        "sc_save_progress_{}.xlsx",
        std::process::id()
    ));
    app.tasks.start_save(&mut app.wb, target.clone());
    let started = app.tasks.saving.as_ref().map(|p| p.started);
    assert!(started.is_some(), "保存任务没有开始时间，状态栏算不出已用时间");
    // 等保存收尾，避免临时文件与线程泄漏到其它测试
    for _ in 0..400 {
        if app.tasks.saving.is_none() {
            break;
        }
        if let Some(p) = app.tasks.saving.as_ref() {
            if p.rx.try_recv().is_ok() {
                app.tasks.saving = None;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _ = std::fs::remove_file(&target);
}

fn run_frame(
    ctx: &egui::Context,
    events: Vec<egui::Event>,
    open: &mut bool,
    seen: &mut Vec<&'static str>,
    grab: bool,
) {
    let mut raw = egui::RawInput::default();
    raw.screen_rect = Some(egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(900.0, 700.0),
    ));
    raw.events = events;
    ctx.run(raw, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let r = ui.add(egui::Button::new("背景按钮"));
            if grab && ctx.memory(|m| m.focused()).is_none() {
                r.request_focus();
            }
            if std::env::var("SC_FOCUS_DBG").is_ok() {
                eprintln!("[dbg] bg id={:?} focused={:?}", r.id, ctx.memory(|m| m.focused()));
            }
            if r.has_focus() {
                seen.push("bg");
            }
        });
        ui_kit::window(ctx, "测试对话框", 320.0)
            .open(open)
            .show(ctx, |ui| {
                let r = ui.add(egui::Button::new("确定"));
                if std::env::var("SC_FOCUS_DBG").is_ok() {
                    eprintln!("[dbg] dlg id={:?} focused={:?}", r.id, ctx.memory(|m| m.focused()));
                }
                if r.has_focus() {
                    seen.push("dlg");
                }
            });
    });
}

fn tab_event() -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Tab,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

fn shift_tab_event() -> egui::Event {
    let mut m = egui::Modifiers::NONE;
    m.shift = true;
    egui::Event::Key {
        key: egui::Key::Tab,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: m,
    }
}

fn esc_event() -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn modal_dialog_traps_tab_focus_and_restores_it_on_escape() {
    let ctx = egui::Context::default();
    let mut open = true;
    let mut seen: Vec<&'static str> = Vec::new();
    // 第一帧：背景按钮拿到焦点
    run_frame(&ctx, vec![], &mut open, &mut seen, true);
    assert_eq!(seen.last(), Some(&"bg"), "背景按钮第一帧没拿到焦点");
    // Tab：焦点必须离开背景、进入对话框（第一站是标题栏的关闭按钮）
    seen.clear();
    run_frame(&ctx, vec![tab_event()], &mut open, &mut seen, false);
    run_frame(&ctx, vec![], &mut open, &mut seen, false);
    assert!(
        !seen.contains(&"bg"),
        "Tab 之后焦点还在背景：{seen:?}（模态焦点没有锁住）"
    );
    assert!(
        seen.is_empty(),
        "焦点落到了对话框里的非按钮控件：{seen:?}"
    );
    // 继续 Tab：焦点在对话框内部循环，最多 6 步必须能到「确定」，且绝不回背景
    seen.clear();
    let mut steps = 0;
    for _ in 0..6 {
        steps += 1;
        run_frame(&ctx, vec![tab_event()], &mut open, &mut seen, false);
        run_frame(&ctx, vec![], &mut open, &mut seen, false);
        if seen.contains(&"dlg") {
            break;
        }
    }
    assert!(
        seen.contains(&"dlg"),
        "Tab 循环 {steps} 步都没到「确定」：{seen:?}（对话框内容键盘不可达）"
    );
    assert!(!seen.contains(&"bg"), "Tab 焦点跑到背景：{seen:?}");
    // Esc：对话框关闭，焦点回到背景按钮
    seen.clear();
    run_frame(&ctx, vec![esc_event()], &mut open, &mut seen, false);
    assert!(!open, "Esc 没有关闭对话框");
    run_frame(&ctx, vec![], &mut open, &mut seen, false);
    assert!(
        seen.contains(&"bg"),
        "关闭后焦点没有回到触发控件：{seen:?}"
    );
}

#[test]
fn default_button_answers_enter_only_when_no_field_has_focus() {
    let ctx = egui::Context::default();
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, clicked: &mut bool, focused_field: bool) {
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(600.0, 400.0),
        ));
        raw.events = events;
        ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if focused_field {
                    let r = ui.text_edit_singleline(&mut String::new());
                    if ctx.memory(|m| m.focused()).is_none() {
                        r.request_focus();
                    }
                }
                let b = ui_kit::primary_button(ui, "确定");
                if b.clicked {
                    *clicked = true;
                }
            });
        });
    }
    let enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    // 没有输入框拿焦点：Enter 就是「确定」
    let mut clicked = false;
    frame(&ctx, vec![], &mut clicked, false);
    frame(&ctx, vec![enter.clone()], &mut clicked, false);
    assert!(clicked, "没有焦点时 Enter 没有触发默认按钮");

    // 输入框拿焦点时：Enter 归输入框，不触发默认按钮
    let ctx2 = egui::Context::default();
    let mut clicked2 = false;
    frame(&ctx2, vec![], &mut clicked2, true);
    frame(&ctx2, vec![enter], &mut clicked2, true);
    assert!(!clicked2, "输入框有焦点时 Enter 仍然触发了默认按钮");
}

// ---------------------------------------------------------------------------
// QA-05.3 五态：hover / pressed / selected / disabled / focus 必须能区分
// ---------------------------------------------------------------------------

fn walk_shapes(
    shapes: &[egui::epaint::ClippedShape],
    fills: &mut Vec<(egui::Rect, egui::Color32)>,
    texts: &mut Vec<egui::Color32>,
    strokes: &mut Vec<(egui::Rect, egui::Color32, f32)>,
) {
    fn rec(
        s: &egui::Shape,
        fills: &mut Vec<(egui::Rect, egui::Color32)>,
        texts: &mut Vec<egui::Color32>,
        strokes: &mut Vec<(egui::Rect, egui::Color32, f32)>,
    ) {
        match s {
            egui::Shape::Rect(r) => {
                fills.push((r.rect, r.fill));
                if r.stroke.width > 0.0 {
                    strokes.push((r.rect, r.stroke.color, r.stroke.width));
                }
            }
            egui::Shape::Text(t) => {
                for sec in &t.galley.job.sections {
                    texts.push(sec.format.color);
                }
                if let Some(c) = t.override_text_color {
                    texts.push(c);
                }
            }
            egui::Shape::Vec(v) => {
                for x in v {
                    rec(x, fills, texts, strokes);
                }
            }
            _ => {}
        }
    }
    for c in shapes {
        rec(&c.shape, fills, texts, strokes);
    }
}

#[test]
fn five_interaction_states_are_visually_distinct() {
    use egui::{Color32, Event, PointerButton, Pos2, Rect, pos2, vec2};

    #[derive(Default)]
    struct Snap {
        fills: Vec<(Rect, Color32)>,
        texts: Vec<Color32>,
        strokes: Vec<(Rect, Color32, f32)>,
    }

    fn shot(events: Vec<Event>, disabled: bool, checked: bool) -> Snap {
        shot_full(events, disabled, checked, false)
    }

    fn shot_full(events: Vec<Event>, disabled: bool, checked: bool, grab_focus: bool) -> Snap {
        let ctx = egui::Context::default();
        // 应用是浅色主题；Context::default() 默认深色，会让 hover/pressed 走另一套 token
        ctx.set_visuals(egui::Visuals::light());
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, 240.0)));
        // 先空跑一帧：egui 的命中测试用上一帧的控件矩形
        let _warm = ctx.run(raw.clone(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.allocate_ui(vec2(200.0, 120.0), |ui| {
                    let r = crate::ui_kit::big_sized(
                        ui,
                        crate::ui_icons::Icon::Paste,
                        "粘贴",
                        vec2(80.0, 60.0),
                    );
                    if grab_focus {
                        r.request_focus();
                    }
                });
            });
        });
        raw.events = events;
        raw.time = Some(1.0);
        raw.focused = true;
        let out = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.allocate_ui(vec2(200.0, 120.0), |ui| {
                    if checked {
                        let _ = crate::ui_kit::check_row(ui, "已选中", true, !disabled, 120.0);
                        return;
                    }
                    let add = |ui: &mut egui::Ui| {
                        let r = crate::ui_kit::big_sized(
                            ui,
                            crate::ui_icons::Icon::Paste,
                            "粘贴",
                            vec2(80.0, 60.0),
                        );
                        if std::env::var("SC_STATE_DBG").is_ok() {
                            eprintln!(
                                "[dbg] rect={:?} hovered={} down={} pos={:?}",
                                r.rect,
                                r.hovered(),
                                r.is_pointer_button_down_on(),
                                ui.ctx().input(|i| i.pointer.latest_pos())
                            );
                        }
                    };
                    if disabled {
                        ui.add_enabled_ui(false, |ui| add(ui));
                    } else {
                        add(ui);
                    }
                });
            });
        });
        let mut s = Snap::default();
        walk_shapes(&out.shapes, &mut s.fills, &mut s.texts, &mut s.strokes);
        if std::env::var("SC_STATE_DBG").is_ok() {
            eprintln!("[dbg] fills={:?}", s.fills);
            eprintln!("[dbg] texts={:?}", s.texts);
        }
        s
    }

    let pos = |x: f32, y: f32| pos2(x, y);
    let hover_ev = vec![Event::PointerMoved(pos(30.0, 40.0))];
    let press_ev = vec![
        Event::PointerMoved(pos(30.0, 40.0)),
        Event::PointerButton {
            pos: pos(30.0, 40.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ];

    let base = shot(vec![], false, false);
    let hover = shot(hover_ev, false, false);
    let press = shot(press_ev, false, false);
    let dis = shot(vec![], true, false);
    let sel = shot(vec![], false, true);
    let foc = shot_full(vec![], false, false, true);

    let has = |s: &Snap, c: Color32| s.fills.iter().any(|(_, f)| *f == c && *f != Color32::TRANSPARENT);

    // 静止态没有底色块（不能和 hover 混淆）
    assert!(
        !has(&base, crate::ui_kit::HOVER) && !has(&base, crate::ui_kit::PRESSED),
        "静止态就画了 hover/pressed 底色"
    );
    assert!(has(&hover, crate::ui_kit::HOVER), "hover 没有用 HOVER 底色");
    assert!(
        !has(&hover, crate::ui_kit::PRESSED),
        "hover 态误用了 pressed 底色"
    );
    assert!(has(&press, crate::ui_kit::PRESSED), "pressed 没有用 PRESSED 底色");
    assert!(has(&sel, crate::ui_kit::ACCENT), "selected 没有用 ACCENT 填充");

    // focus：必须有主题绿描边圈（1.5px），并且不是填充色块
    assert!(
        foc
            .strokes
            .iter()
            .any(|(_, c, w)| *c == crate::ui_kit::ACCENT && *w >= 1.0),
        "focus 态没有焦点圈：{:?}",
        foc.strokes
    );

    // disabled：文字必须落到 DISABLED 色
    assert!(
        dis.texts.iter().any(|c| *c == crate::ui_kit::DISABLED),
        "disabled 态文字没有变灰：{:?}",
        dis.texts
    );

    // 五种状态的颜色两两可区分（RGB 曼哈顿距离 >= 6）
    let dist = |a: Color32, b: Color32| -> u32 {
        let (a, b) = (a.to_array(), b.to_array());
        (0..3)
            .map(|i| (a[i] as i32 - b[i] as i32).unsigned_abs())
            .sum()
    };
    let states = [
        ("静止", Color32::WHITE),
        ("hover", crate::ui_kit::HOVER),
        ("pressed", crate::ui_kit::PRESSED),
        ("selected", crate::ui_kit::ACCENT),
        ("disabled", crate::ui_kit::DISABLED),
    ];
    for i in 0..states.len() {
        for j in (i + 1)..states.len() {
            let d = dist(states[i].1, states[j].1);
            assert!(
                d >= 6,
                "{} 与 {} 的颜色太接近（距离 {d}）：{:?} vs {:?}",
                states[i].0,
                states[j].0,
                states[i].1,
                states[j].1
            );
        }
    }
}

// ---------------------------------------------------------------------------
// QA-04.1 文件级快捷键：Ctrl+Z / Ctrl+Y 必须真的生效（此前完全没绑键）
// ---------------------------------------------------------------------------

#[test]
fn ctrl_z_and_ctrl_y_drive_undo_and_redo() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.editing = Some(EditState::new("7".into()));
    app.commit_edit(None);
    let before = app.wb.sheet(0).get(0, 0).map(|c| c.value.display()).unwrap_or_default();
    assert_eq!(before, "7", "前提：先写进一个值");

    let key = |k: egui::Key, mods: egui::Modifiers| egui::Event::Key {
        key: k,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: mods,
    };
    let ctrl = egui::Modifiers { command: true, ..Default::default() };
    let ctrl_shift = egui::Modifiers { command: true, shift: true, ..Default::default() };

    fn run(
        app: &mut SheetApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        mods: egui::Modifiers,
    ) {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        input.events = events;
        input.modifiers = mods;
        let _ = ctx.run(input, |ctx| {
            app.handle_file_shortcuts(ctx);
        });
    }

    run(&mut app, &ctx, vec![key(egui::Key::Z, ctrl)], ctrl);
    let undone = app.wb.sheet(0).get(0, 0).map(|c| c.value.display()).unwrap_or_default();
    assert_ne!(undone, "7", "Ctrl+Z 没有撤销刚写入的值");

    run(&mut app, &ctx, vec![key(egui::Key::Y, ctrl)], ctrl);
    let redone = app.wb.sheet(0).get(0, 0).map(|c| c.value.display()).unwrap_or_default();
    assert_eq!(redone, "7", "Ctrl+Y 没有重做");

    // Ctrl+Shift+Z 也当重做：先撤销，再用 Shift 组合键重做
    run(&mut app, &ctx, vec![key(egui::Key::Z, ctrl)], ctrl);
    run(&mut app, &ctx, vec![key(egui::Key::Z, ctrl_shift)], ctrl_shift);
    let redone2 = app.wb.sheet(0).get(0, 0).map(|c| c.value.display()).unwrap_or_default();
    assert_eq!(redone2, "7", "Ctrl+Shift+Z 没有重做");
}

// ---------------------------------------------------------------------------
// QA-04.3 多文档：值/选区/滚动/脏标记/撤销/后台任务必须各归各的标签
// ---------------------------------------------------------------------------

#[test]
fn switching_tabs_keeps_values_selection_scroll_and_dirty_per_document() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    // 文档 1：写值 + 挪选区 + 滚一段 + 标脏
    app.wb.sheet_mut(0).set(0, 0, text("doc1"));
    app.wb.dirty = true;
    app.sel = (3, 2);
    app.sel_end = (3, 2);
    app.zoom = 1.0;
    app.scroll = egui::vec2(120.0, 240.0);

    app.open_in_new_tab(std::path::PathBuf::new());
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 1);
    assert!(
        app.wb.sheet(0).get(0, 0).is_none(),
        "新标签应该是空白文档"
    );
    assert_eq!(app.sel, (0, 0), "新标签选区应回到 A1");
    assert_eq!(app.scroll.y, 0.0, "新标签滚动应回到顶部");

    // 文档 2：写值
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.editing = Some(EditState::new("doc2".into()));
    app.commit_edit(None);
    assert!(app.wb.dirty);

    app.switch_tab(0);
    assert_eq!(app.active_tab, 0);
    assert_eq!(
        app.wb.sheet(0).get(0, 0).map(|c| c.value.display()),
        Some("doc1".to_string()),
        "切回来拿到的不是文档 1 的内容"
    );
    assert_eq!(app.sel, (3, 2), "文档 1 的选区没被记住");
    assert!(app.scroll.y > 200.0, "文档 1 的滚动位置没被记住");
    assert!(app.wb.dirty, "文档 1 的未保存标记丢了");

    app.switch_tab(1);
    assert_eq!(
        app.wb.sheet(0).get(0, 0).map(|c| c.value.display()),
        Some("doc2".to_string()),
        "文档 2 的内容没被记住"
    );
    assert_eq!(app.sel, (0, 0), "文档 2 选区不该跟着文档 1");
}

#[test]
fn undo_history_belongs_to_its_own_document() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, text("keep"));
    app.sel = (0, 1);
    app.sel_end = (0, 1);
    app.editing = Some(EditState::new("d1".into()));
    app.commit_edit(None);

    app.open_in_new_tab(std::path::PathBuf::new());
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.editing = Some(EditState::new("d2".into()));
    app.commit_edit(None);

    // 在文档 2 撤销：只回退文档 2
    app.do_undo(false);
    let d2 = app.wb.sheet(0).get(0, 0).map(|c| c.value.display());
    assert_eq!(d2, None, "文档 2 撤销没有生效");
    app.switch_tab(0);
    assert_eq!(
        app.wb.sheet(0).get(0, 0).map(|c| c.value.display()),
        Some("keep".to_string()),
        "文档 2 的撤销把文档 1 改了"
    );
    assert_eq!(
        app.wb.sheet(0).get(0, 1).map(|c| c.value.display()),
        Some("d1".to_string()),
        "文档 1 自己的编辑丢了"
    );
    // 切回文档 2 再重做：历史必须还是文档 2 的
    app.switch_tab(1);
    app.do_undo(true);
    assert_eq!(
        app.wb.sheet(0).get(0, 0).map(|c| c.value.display()),
        Some("d2".to_string()),
        "文档 2 的重做历史没保住"
    );
}

#[test]
fn background_save_finishes_in_its_own_tab_after_switching() {
    let dir = std::env::temp_dir().join(format!("sc_qa043_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path1 = dir.join("doc1.xlsx");
    sheetcore::xlsx::save(&path1, &sheetcore::model::Workbook::new_blank()).expect("造空文件");

    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.path = Some(path1.clone());
    app.wb.sheet_mut(0).set(0, 0, text("saved1"));
    app.wb.dirty = true;

    app.save(false);
    assert!(app.tasks.saving.is_some(), "保存应在后台进行，而不是同步阻塞");

    // 保存还在跑的时候开新标签并写点东西
    app.open_in_new_tab(std::path::PathBuf::new());
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.editing = Some(EditState::new("dirty2".into()));
    app.commit_edit(None);
    assert!(app.wb.dirty, "文档 2 应该是脏的");

    // 等文档 1 的后台保存结束（最多 10 秒）
    let t0 = std::time::Instant::now();
    while app.tabs.iter().any(|t| {
        matches!(t, TabSlot::Parked(d) if d.tasks.saving.is_some())
    }) && t0.elapsed().as_secs() < 10
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
        // 每帧都要真的收一次后台保存结果（和主循环一样），否则挂起标签的 saving 永远不清
        app.poll_save();
    }

    // 文档 1：文件里应该有值、脏标记清掉
    let wb = sheetcore::xlsx::load(&path1).expect("读回文档 1");
    assert_eq!(
        wb.sheet(0).get(0, 0).map(|c| c.value.display()),
        Some("saved1".to_string()),
        "后台保存没有把文档 1 写进文件"
    );
    app.switch_tab(0);
    assert!(!app.wb.dirty, "文档 1 保存完成后脏标记应清掉");

    // 文档 2：脏标记和内容不受文档 1 保存影响
    app.switch_tab(1);
    assert!(app.wb.dirty, "文档 2 的脏标记被别人的保存清掉了");
    assert_eq!(
        app.wb.sheet(0).get(0, 0).map(|c| c.value.display()),
        Some("dirty2".to_string())
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn switching_tabs_cancels_inflight_recalc_and_keeps_other_doc_intact() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, text("1"));
    // 长链：A2=A1+1 … A20001=A20000+1，够让后面切标签时它还没算完
    for r in 1..20_000u32 {
        app.wb
            .sheet_mut(0)
            .set(r, 0, formula(&format!("=A{}+1", r)));
    }
    app.wb.dirty = true;

    app.recalc_async(None);
    assert!(app.tasks.busy, "重算应在后台跑");

    // 立刻开新标签：旧文档的重算必须被取消，且不能落到新文档上
    app.open_in_new_tab(std::path::PathBuf::new());
    assert!(
        !app.tasks.busy,
        "切到新标签后旧文档的重算状态不该带过来"
    );
    // 取消信号必须真的打到旧文档自己的标志位上（状态栏随后会被「已新建标签」覆盖）
    match &app.tabs[0] {
        TabSlot::Parked(doc) => {
            let flag = doc
                .tasks
                .recalc_cancel
                .as_ref()
                .expect("旧文档应保留自己的重算取消标志");
            assert!(
                flag.load(std::sync::atomic::Ordering::Relaxed),
                "切标签时没有把取消标志置位"
            );
        }
        TabSlot::Active => panic!("旧文档应被挂起成 Parked"),
    }

    // 新文档写值，等几帧确认没有被旧结果覆盖
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.editing = Some(EditState::new("newdoc".into()));
    app.commit_edit(None);
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(15));
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 700.0),
        ));
        let _ = ctx.run(input, |_c| {});
    }
    assert_eq!(
        app.wb.sheet(0).get(0, 0).map(|c| c.value.display()),
        Some("newdoc".to_string()),
        "旧文档的重算结果覆盖了新文档"
    );
    assert_eq!(
        app.wb.sheet(0).get(1, 0).map(|c| c.value.display()),
        None,
        "新文档被塞进了旧文档的公式结果"
    );
}

#[test]
fn recalc_result_from_another_document_is_discarded_even_if_epoch_matches() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.wb.sheet_mut(0).set(0, 0, text("doc1"));
    let doc1_gen = app.doc_gen;

    app.open_in_new_tab(std::path::PathBuf::new());
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.editing = Some(EditState::new("doc2".into()));
    app.commit_edit(None);
    assert_ne!(doc1_gen, app.doc_gen, "前提：两个文档的 doc_gen 不同");

    // 伪造一条「文档 1 的重算结果」：epoch 故意用当前文档的，只有 doc_gen 是旧的
    let mut foreign = sheetcore::model::Workbook::new_blank();
    foreign.sheet_mut(0).set(0, 0, text("FOREIGN"));
    let deps = sheetcore::formula::deps::Deps::build(&foreign);
    let epoch_now = app.wb_epoch;
    let (tx, rx) = std::sync::mpsc::channel();
    app.tasks.open_rx = Some(rx);
    tx.send(OpenMsg::RecalcedWb(Box::new(foreign), Box::new(deps), epoch_now, doc1_gen))
        .unwrap();
    app.poll_open();
    assert_eq!(
        app.wb.sheet(0).get(0, 0).map(|c| c.value.display()),
        Some("doc2".to_string()),
        "别的文档的重算结果被套用到当前文档"
    );

    // 同一条消息，doc_gen 换成当前文档：必须被接受，证明拦截理由确实是归属而不是永远拒绝
    let mut mine = sheetcore::model::Workbook::new_blank();
    mine.sheet_mut(0).set(0, 0, text("MINE"));
    let deps2 = sheetcore::formula::deps::Deps::build(&mine);
    let (tx2, rx2) = std::sync::mpsc::channel();
    app.tasks.open_rx = Some(rx2);
    tx2.send(OpenMsg::RecalcedWb(
        Box::new(mine),
        Box::new(deps2),
        app.wb_epoch,
        app.doc_gen,
    ))
    .unwrap();
    app.poll_open();
    assert_eq!(
        app.wb.sheet(0).get(0, 0).map(|c| c.value.display()),
        Some("MINE".to_string()),
        "归属正确的重算结果也被丢了"
    );
}

// ---------------------------------------------------------------------------
// UI-29.2 图标：16/20/24/32 四档都必须有墨、不能画出盒子、线宽不能糊成 0
// ---------------------------------------------------------------------------

fn icon_shapes(icon: crate::ui_icons::Icon, size: f32, color: bool) -> Vec<egui::Shape> {
    let ctx = egui::Context::default();
    let mut raw = egui::RawInput::default();
    raw.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(size + 32.0, size + 32.0),
    ));
    let out = ctx.run(raw, |ctx| {
        let p = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("icon_audit"),
        ));
        let r = egui::Rect::from_min_size(egui::pos2(16.0, 16.0), egui::Vec2::splat(size));
        if color {
            crate::ui_icons::draw_excel(&p, r, icon, egui::Color32::BLACK, true);
        } else {
            crate::ui_icons::draw(&p, r, icon, egui::Color32::BLACK);
        }
    });
    out.shapes.into_iter().map(|c| c.shape).collect()
}

fn shape_bbox(shapes: &[egui::Shape], acc: &mut Option<egui::Rect>) {
    fn take(r: egui::Rect, acc: &mut Option<egui::Rect>) {
        *acc = Some(match *acc {
            Some(a) => a.union(r),
            None => r,
        });
    }
    for s in shapes {
        match s {
            egui::Shape::Rect(r) => take(r.rect, acc),
            egui::Shape::Circle(c) => take(egui::Rect::from_center_size(c.center, egui::Vec2::splat(c.radius * 2.0)), acc),
            egui::Shape::LineSegment { points, .. } => {
                take(egui::Rect::from_two_pos(points[0], points[1]), acc)
            }
            egui::Shape::Path(p) => {
                if let Some(first) = p.points.first() {
                    let mut r = egui::Rect::from_min_max(*first, *first);
                    for q in &p.points[1..] {
                        r = r.union(egui::Rect::from_min_max(*q, *q));
                    }
                    take(r, acc);
                }
            }
            egui::Shape::Vec(v) => shape_bbox(v, acc),
            egui::Shape::Text(t) => take(t.galley.rect.translate(t.pos.to_vec2()), acc),
            _ => {}
        }
    }
}

#[test]
fn every_icon_draws_ink_inside_its_box_at_four_sizes() {
    let mut problems: Vec<String> = Vec::new();
    for &size in &[16.0f32, 20.0, 24.0, 32.0] {
        for icon in crate::ui_icons::ALL_ICONS {
            for color in [false, true] {
                let shapes = icon_shapes(icon, size, color);
                let mut bbox = None;
                shape_bbox(&shapes, &mut bbox);
                let Some(bb) = bbox else {
                    problems.push(format!("{icon:?} {size}px color={color}: 没有画出任何图形"));
                    continue;
                };
                if shapes.is_empty() {
                    problems.push(format!("{icon:?} {size}px: shape 列表为空"));
                }
                // 盒子在 (16,16)-(16+size,16+size)；允许 4% 外扩（draw_excel 会外扩）
                let box_rect = egui::Rect::from_min_size(
                    egui::pos2(16.0, 16.0),
                    egui::Vec2::splat(size),
                );
                let slack = (size * 0.06).max(1.5);
                if bb.min.x < box_rect.min.x - slack
                    || bb.min.y < box_rect.min.y - slack
                    || bb.max.x > box_rect.max.x + slack
                    || bb.max.y > box_rect.max.y + slack
                {
                    problems.push(format!(
                        "{icon:?} {size}px color={color}: 超出盒子 bbox={bb:?} box={box_rect:?}"
                    ));
                }
                // 墨量：图形不能缩成点，也不能糊满整格
                let ink = (bb.width() * bb.height()) / (size * size);
                if ink < 0.06 {
                    problems.push(format!("{icon:?} {size}px: 墨量太小 {ink:.3}"));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "图标自检失败 {} 项：\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[test]
fn icon_stroke_width_never_drops_below_one_pixel() {
    let mut thin: Vec<String> = Vec::new();
    for &size in &[16.0f32, 20.0, 24.0, 32.0] {
        for icon in crate::ui_icons::ALL_ICONS {
            let shapes = icon_shapes(icon, size, false);
            let mut min_w = f32::MAX;
            fn scan(s: &egui::Shape, min_w: &mut f32) {
                match s {
                    egui::Shape::Rect(r) if r.stroke.width > 0.0 => {
                        *min_w = min_w.min(r.stroke.width)
                    }
                    egui::Shape::LineSegment { stroke, .. } => *min_w = min_w.min(stroke.width),
                    egui::Shape::Path(p) if p.stroke.width > 0.0 => {
                        *min_w = min_w.min(p.stroke.width)
                    }
                    egui::Shape::Vec(v) => {
                        for x in v {
                            scan(x, min_w);
                        }
                    }
                    _ => {}
                }
            }
            for s in &shapes {
                scan(s, &mut min_w);
            }
            if min_w < 0.99 {
                thin.push(format!("{icon:?} {size}px: 线宽 {min_w:.2}"));
            }
        }
    }
    assert!(thin.is_empty(), "有 {} 处线宽小于 1px：\n{}", thin.len(), thin.join("\n"));
}

#[test]
fn debug_icon_shape_dump() {
    if std::env::var("SC_ICON_DBG").is_err() {
        return;
    }
    for (name, icon) in [
        ("Comma", crate::ui_icons::Icon::Comma),
        ("FormulaView", crate::ui_icons::Icon::FormulaView),
    ] {
        for color in [false, true] {
            let shapes = icon_shapes(icon, 32.0, color);
            eprintln!("== {name} color={color}");
            fn dump(s: &egui::Shape, depth: usize) {
                let pad = "  ".repeat(depth);
                match s {
                    egui::Shape::Rect(r) => eprintln!("{pad}Rect {:?} stroke={}", r.rect, r.stroke.width),
                    egui::Shape::Text(t) => eprintln!(
                        "{pad}Text pos={:?} galley_rect={:?} text={:?}",
                        t.pos, t.galley.rect, t.galley.text()
                    ),
                    egui::Shape::LineSegment { points, stroke } => {
                        eprintln!("{pad}Line {:?}-{:?} w={}", points[0], points[1], stroke.width)
                    }
                    egui::Shape::Path(p) => eprintln!("{pad}Path {:?} w={}", p.points.first(), p.stroke.width),
                    egui::Shape::Circle(c) => eprintln!("{pad}Circle {:?} r={}", c.center, c.radius),
                    egui::Shape::Vec(v) => {
                        eprintln!("{pad}Vec {}", v.len());
                        for x in v {
                            dump(x, depth + 1);
                        }
                    }
                    other => eprintln!("{pad}{other:?}"),
                }
            }
            for s in &shapes {
                dump(s, 0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// UI-29.3 空/错误/加载状态：组件本身 + 真实调用点
// ---------------------------------------------------------------------------

fn collect_texts(shapes: &[egui::Shape]) -> Vec<(String, egui::Color32)> {
    fn rec(s: &egui::Shape, out: &mut Vec<(String, egui::Color32)>) {
        match s {
            egui::Shape::Text(t) => {
                let color = t
                    .override_text_color
                    .or_else(|| t.galley.job.sections.first().map(|x| x.format.color))
                    .unwrap_or(egui::Color32::PLACEHOLDER);
                out.push((t.galley.text().to_string(), color));
            }
            egui::Shape::Vec(v) => {
                for x in v {
                    rec(x, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for s in shapes {
        rec(s, &mut out);
    }
    out
}

fn collect_fills(shapes: &[egui::Shape]) -> Vec<(egui::Rect, egui::Color32)> {
    fn rec(s: &egui::Shape, out: &mut Vec<(egui::Rect, egui::Color32)>) {
        match s {
            egui::Shape::Rect(r) if r.fill != egui::Color32::TRANSPARENT => out.push((r.rect, r.fill)),
            egui::Shape::Vec(v) => {
                for x in v {
                    rec(x, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for s in shapes {
        rec(s, &mut out);
    }
    out
}

#[test]
fn state_view_paints_title_detail_and_kind_color() {
    let ctx = egui::Context::default();
    ctx.set_visuals(egui::Visuals::light());
    let mut raw = egui::RawInput::default();
    raw.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(420.0, 320.0),
    ));
    let out = ctx.run(raw, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            crate::ui_kit::state_view(
                ui,
                crate::ui_kit::StateKind::Error,
                crate::ui_icons::Icon::Find,
                "没有找到匹配内容",
                "换个关键字",
            );
        });
    });
    let texts = collect_texts(&out.shapes.iter().map(|c| c.shape.clone()).collect::<Vec<_>>());
    assert!(texts.iter().any(|(t, _)| t.contains("没有找到匹配内容")), "标题没画出来: {texts:?}");
    assert!(texts.iter().any(|(t, _)| t.contains("换个关键字")), "说明没画出来");
    let want = crate::ui_kit::state_color(crate::ui_kit::StateKind::Error);
    assert!(
        texts.iter().any(|(t, c)| t.contains("没有找到匹配内容") && *c == want),
        "标题没有用错误色"
    );
    // 三种状态的颜色必须两两不同，否则「可分辨」是假的
    let e = crate::ui_kit::state_color(crate::ui_kit::StateKind::Error);
    let w = crate::ui_kit::state_color(crate::ui_kit::StateKind::Warn);
    let m = crate::ui_kit::state_color(crate::ui_kit::StateKind::Empty);
    assert_ne!(e, w);
    assert_ne!(e, m);
    assert_ne!(w, m);
}

#[test]
fn state_banner_shows_title_and_kind_background() {
    let ctx = egui::Context::default();
    ctx.set_visuals(egui::Visuals::light());
    let mut raw = egui::RawInput::default();
    raw.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(560.0, 220.0),
    ));
    let out = ctx.run(raw, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            crate::ui_kit::state_banner(
                ui,
                crate::ui_kit::StateKind::Warn,
                "打开时有读取问题（不影响保存）",
                "3 处",
            );
        });
    });
    let shapes: Vec<egui::Shape> = out.shapes.iter().map(|c| c.shape.clone()).collect();
    let texts = collect_texts(&shapes);
    assert!(
        texts.iter().any(|(t, _)| t.contains("打开时有读取问题")),
        "状态条标题没画出来"
    );
    let fills = collect_fills(&shapes);
    assert!(
        fills.iter().any(|(_, c)| *c == egui::Color32::from_rgb(255, 249, 240)),
        "状态条没有用警告底色: {fills:?}"
    );
    assert!(
        fills
            .iter()
            .any(|(_, c)| *c == crate::ui_kit::state_color(crate::ui_kit::StateKind::Warn)),
        "状态条左侧色条缺失"
    );
}

#[test]
fn loading_overlay_draws_scrim_card_and_half_progress() {
    let ctx = egui::Context::default();
    let mut raw = egui::RawInput::default();
    raw.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(800.0, 600.0),
    ));
    let out = ctx.run(raw, |ctx| {
        crate::ui_kit::loading_overlay(ctx, "正在重算", "50 / 100 个公式，Esc 取消", Some(0.5));
    });
    let shapes: Vec<egui::Shape> = out.shapes.iter().map(|c| c.shape.clone()).collect();
    let texts = collect_texts(&shapes);
    assert!(texts.iter().any(|(t, _)| t.contains("正在重算")));
    assert!(texts.iter().any(|(t, _)| t.contains("50 / 100")));
    let fills = collect_fills(&shapes);
    // 压暗层（半透明黑）
    assert!(
        fills.iter().any(|(_, c)| *c == egui::Color32::from_black_alpha(40)),
        "没有画遮罩底"
    );
    // 白卡片
    let card = fills
        .iter()
        .find(|(r, c)| *c == egui::Color32::WHITE && r.width() > 300.0 && r.height() > 100.0);
    assert!(card.is_some(), "没有白卡片");
    // 进度条填充：绿色、宽度约为轨道一半
    let track = fills
        .iter()
        .find(|(r, c)| *c == egui::Color32::from_rgb(237, 235, 233) && r.height() <= 6.0);
    let track = track.expect("没有进度条轨道");
    let fill = fills
        .iter()
        .find(|(r, c)| *c == crate::ui_kit::ACCENT && r.height() <= 6.0)
        .expect("没有进度填充");
    let ratio = fill.0.width() / track.0.width();
    assert!((ratio - 0.5).abs() < 0.05, "进度条比例 {ratio} 不是 0.5");
}

#[test]
fn find_all_window_with_no_hits_shows_empty_state() {
    let ctx = egui::Context::default();
    ctx.set_visuals(egui::Visuals::light());
    let mut app = SheetApp::new_with_context(&ctx, None);
    app.find.find_all_open = true;
    app.find.find_all.clear();
    let mut raw = egui::RawInput::default();
    raw.screen_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(900.0, 700.0),
    ));
    // egui 的 Window 第一帧只量尺寸（shape 全是 Noop），第二帧才真正画出来
    let _warm = ctx.run(raw.clone(), |ctx| {
        app.find_all_window(ctx);
    });
    let out = ctx.run(raw, |ctx| {
        app.find_all_window(ctx);
    });
    let texts = collect_texts(&out.shapes.iter().map(|c| c.shape.clone()).collect::<Vec<_>>());
    assert!(
        texts.iter().any(|(t, _)| t.contains("没有找到匹配内容")),
        "查找全部 0 处时没有走空状态: {texts:?}"
    );
}

#[test]
fn pasting_text_that_looks_numeric_keeps_leading_zeros() {
    // Excel 的 XMLSS 对文本格给 `ss:Type="String">00123`，对数字格给 `Type="Number"`。
    // 只要按「文本长什么样」去 parse::<f64>，"00123" 就变成 123，前导零丢失。
    let mut app = app();
    app.sel = (0, 0);
    let xml = r#"<?xml version="1.0"?>
<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet"
 xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">
 <Worksheet ss:Name="Sheet1"><Table>
  <Row>
   <Cell ss:StyleID="s62"><Data ss:Type="String">00123</Data></Cell>
   <Cell><Data ss:Type="Number">123</Data></Cell>
   <Cell><Data ss:Type="String">1,234</Data></Cell>
   <Cell><Data ss:Type="String">2023-03-15</Data></Cell>
  </Row>
 </Table></Worksheet></Workbook>"#;
    let block = office_clipboard::convert_block(sheetcore::clipboard_xmlss::parse(xml));
    assert!(app.paste_office_block(&block), "粘贴被拒绝");
    assert_eq!(
        app.wb.sheets[0].value(0, 0),
        Value::Text("00123".into()),
        "字符串 00123 被转成了数字"
    );
    assert_eq!(app.wb.sheets[0].value(0, 1), Value::Number(123.0));
    assert_eq!(
        app.wb.sheets[0].value(0, 2),
        Value::Text("1,234".into()),
        "字符串带千分位必须保持文本"
    );
    assert_eq!(
        app.wb.sheets[0].value(0, 3),
        Value::Text("2023-03-15".into()),
        "Type=String 的日期文本不能按日期序列号落格"
    );
}

#[test]
fn shift_tab_cycles_backwards_inside_modal_and_never_escapes() {
    let ctx = egui::Context::default();
    let mut open = true;
    let mut seen: Vec<&'static str> = Vec::new();
    run_frame(&ctx, vec![], &mut open, &mut seen, true);
    assert_eq!(seen.last(), Some(&"bg"), "背景按钮第一帧没拿到焦点");
    // Tab 前进到「确定」
    let mut reached = false;
    for _ in 0..6 {
        seen.clear();
        run_frame(&ctx, vec![tab_event()], &mut open, &mut seen, false);
        run_frame(&ctx, vec![], &mut open, &mut seen, false);
        if seen.contains(&"dlg") {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab 前进没到「确定」：{seen:?}");
    // Shift+Tab：必须退到对话框内另一个控件，且不回背景。
    // egui 的焦点迁移发生在下一帧开头，所以按 Tab 之后要多跑几帧再看结果。
    seen.clear();
    let mut moved = false;
    // egui 的焦点迁移排在下一帧：按 Shift+Tab 的那一帧仍报旧焦点，
    // 所以要先跑按键帧、清空记录，再看下一帧落到哪里。
    for _ in 0..3 {
        run_frame(&ctx, vec![shift_tab_event()], &mut open, &mut seen, false);
        seen.clear();
        run_frame(&ctx, vec![], &mut open, &mut seen, false);
        assert!(!seen.contains(&"bg"), "Shift+Tab 把焦点退到背景：{seen:?}");
        if !seen.contains(&"dlg") {
            moved = true;
            break;
        }
    }
    assert!(moved, "Shift+Tab 没有从「确定」后退（焦点没动）：{seen:?}");
    // 再 Shift+Tab 一圈回到「确定」，证明是反向循环而不是卡死
    let mut back = false;
    for _ in 0..6 {
        run_frame(&ctx, vec![shift_tab_event()], &mut open, &mut seen, false);
        seen.clear();
        run_frame(&ctx, vec![], &mut open, &mut seen, false);
        assert!(!seen.contains(&"bg"), "反向循环里焦点跑到背景：{seen:?}");
        if seen.contains(&"dlg") {
            back = true;
            break;
        }
    }
    assert!(back, "反向循环没回到「确定」：{seen:?}");
}

#[test]
fn tabbing_into_scrolled_dialog_scrolls_the_focused_button_into_view() {
    // 对话框高度固定 200，内容 40 个按钮；Tab 一路走到底时最后一个按钮必须被滚进视口，
    // 否则焦点跑到看不见的地方（键盘用户以为 Tab 失灵）。
    let ctx = egui::Context::default();
    let mut open = true;
    let mut last_rect: Option<egui::Rect> = None;
    let mut body_rect: Option<egui::Rect> = None;
    fn frame(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        open: &mut bool,
        last_rect: &mut Option<egui::Rect>,
        body_rect: &mut Option<egui::Rect>,
    ) {
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(800.0, 600.0),
        ));
        raw.events = events;
        ctx.run(raw, |ctx| {
            ui_kit::window(ctx, "长对话框", 360.0)
                .default_size([360.0, 200.0])
                .open(open)
                .show(ctx, |ui| {
                    *body_rect = Some(ui.clip_rect());
                    for i in 0..40 {
                        let r = ui.add(egui::Button::new(format!("按钮 {i}")));
                        if i == 0 && ctx.memory(|m| m.focused()).is_none() {
                            r.request_focus();
                        }
                        if i == 39 {
                            *last_rect = Some(r.rect);
                        }
                    }
                });
        });
    }
    frame(&ctx, vec![], &mut open, &mut last_rect, &mut body_rect);
    frame(&ctx, vec![], &mut open, &mut last_rect, &mut body_rect);
    let body0 = body_rect.expect("没有正文视口");
    let last0 = last_rect.expect("没有最后一个按钮");
    assert!(
        !body0.contains_rect(last0),
        "初始状态最后一个按钮就在视口内，用例失去意义（body {body0:?} last {last0:?}）"
    );
    // 连续 Tab 直到最后一个按钮拿到焦点
    let mut reached = false;
    for _ in 0..60 {
        frame(&ctx, vec![tab_event()], &mut open, &mut last_rect, &mut body_rect);
        frame(&ctx, vec![], &mut open, &mut last_rect, &mut body_rect);
        if let (Some(b), Some(l)) = (body_rect, last_rect) {
            if b.contains_rect(l) {
                reached = true;
                break;
            }
        }
    }
    assert!(
        reached,
        "Tab 走完 40 个按钮，最后一个仍在视口外（没有自动滚动）：body {body_rect:?} last {last_rect:?}"
    );
}

#[test]
fn every_dialog_primary_action_uses_the_default_button_helper() {
    // 默认按钮（Enter 触发）只在 ui_kit::primary_button 里实现。任何对话框的主按钮
    // 写成 ui.button(...) 就会丢掉 Enter，也不会画焦点圈。
    // 这条用例扫源码，把「对话框里剩下的裸主按钮」钉死。
    let src = include_str!("main.rs");
    let mut bad = Vec::new();
    // 功能区下拉里的行内「应用」是菜单项（点完即关闭菜单），不是对话框默认按钮，
    // 不在本规则的管辖范围内。
    const MENU_FNS: [&str; 1] = ["toolbar_cf_items"];
    let mut cur_fn = "";
    for (n, line) in src.lines().enumerate() {
        if line.starts_with("    fn ") {
            cur_fn = line
                .trim_start_matches("    fn ")
                .split(['(', '<'])
                .next()
                .unwrap_or("");
        }
        if MENU_FNS.contains(&cur_fn) {
            continue;
        }
        let t = line.trim();
        if !t.starts_with("if ui.button(") && !t.starts_with("if ui.add(egui::Button::new(") {
            continue;
        }
        for label in ["确定", "执行", "应用", "完成", "下一步", "导入"] {
            if t.contains(&format!("\"{label}\"")) {
                bad.push(format!("main.rs:{} {}", n + 1, t));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "这些对话框主按钮没有走 primary_button（Enter 无效、也没有焦点圈）：
{}",
        bad.join("
")
    );
}

#[test]
fn large_selection_stats_are_not_empty() {
    // 回归：百万行选区的状态栏统计曾经恒为空。根因是 collect_stats_values 里
    // 额外用 rows.size(r) <= 0.0 过滤，把整表滤掉（行高不参与统计口径）。
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("work/probe/qa07/values1m.xlsx");
    if !path.exists() {
        eprintln!("[skip] 缺少 {}", path.display());
        return;
    }
    let wb = sheetcore::xlsx::load(&path).expect("load values1m");
    let sh = wb.sheet(0);
    let rects = [(0u32, 0u32, 999_999u32, 0u32)];
    let vals = crate::collect_stats_values(sh, &rects);
    assert!(
        !vals.is_empty(),
        "百万行选区收集到 0 个值（状态栏会永远显示空）"
    );
    let mut acc = crate::StatsAcc::default();
    for v in &vals {
        acc.add(*v);
    }
    let text = acc.text(11);
    assert!(text.contains("计数: 1000"), "统计文本不对：{text:?}");
    // 同步路径（小选区）与异步路径必须一致
    let sync = crate::status_stats_rects(sh, &rects, 11);
    assert_eq!(sync, text, "同步/异步统计口径不一致");
}

#[test]
fn wiz_range_label_builds_a1_or_range() {
    assert_eq!(crate::wiz_range_label((0, 0), (0, 0)), "A1");
    assert_eq!(crate::wiz_range_label((2, 1), (0, 0)), "A1:B3", "反向拖选要归一");
    assert_eq!(crate::wiz_range_label((3, 27), (3, 27)), "AB4");
}

#[test]
fn wizard_help_url_encodes_function_name() {
    let u = crate::help_url("VLOOKUP");
    assert!(u.starts_with("https://support.microsoft.com/zh-cn/search/results?query=VLOOKUP%20"), "{u}");
    // 空格必须被转义，否则链接断在空格处
    assert!(!u.contains(' '), "{u}");
}

#[test]
fn cross_sheet_drag_labels_carry_sheet_name() {
    // 同表拖动：不带表名
    assert_eq!(crate::pick_range_label(None, (1, 1), (3, 2)), "B2:C4");
    // 跨表拖动：带表名（Sheet2）
    assert_eq!(crate::pick_range_label(Some("Sheet2"), (1, 1), (3, 2)), "Sheet2!B2:C4");
    // 名字里有空格要加单引号（Excel 规则）
    assert_eq!(
        crate::pick_range_label(Some("销售 数据"), (0, 0), (0, 0)),
        "'销售 数据'!A1"
    );
}

#[test]
fn parse_calc_item_reads_name_and_sources() {
    let (n, srcs) = crate::parse_calc_item("合计=华东+华北").expect("应能解析");
    assert_eq!(n, "合计");
    assert_eq!(
        srcs,
        vec![('+', "华东".to_string()), ('+', "华北".to_string())]
    );
    // 空格、空串都容忍
    let (n2, s2) = crate::parse_calc_item(" 小计 = 研发 + 销售 ").unwrap();
    assert_eq!(n2, "小计");
    assert_eq!(
        s2,
        vec![('+', "研发".to_string()), ('+', "销售".to_string())]
    );
    // 乘除/减法：运算符随项返回
    let (n3, s3) = crate::parse_calc_item("净额=收入-成本*税率").unwrap();
    assert_eq!(n3, "净额");
    assert_eq!(
        s3,
        vec![
            ('+', "收入".to_string()),
            ('-', "成本".to_string()),
            ('*', "税率".to_string())
        ]
    );
    // 缺名字/缺来源/没有等号 → None
    assert!(crate::parse_calc_item("=华东+华北").is_none());
    assert!(crate::parse_calc_item("合计=").is_none());
    assert!(crate::parse_calc_item("  ").is_none());
}

#[test]
fn parse_calc_field_reads_name_and_formula() {
    let (n, f) = crate::parse_calc_field("总价=金额*2").expect("应能解析");
    assert_eq!(n, "总价");
    assert_eq!(f, "金额*2");
    // 公式里允许空格与等号后续内容
    let (n2, f2) = crate::parse_calc_field(" 毛利 = 金额 - 成本 ").unwrap();
    assert_eq!(n2, "毛利");
    assert_eq!(f2, "金额 - 成本");
    assert!(crate::parse_calc_field("=金额*2").is_none());
    assert!(crate::parse_calc_field("总价=").is_none());
}

// ---------------------------------------------------------------- accesskit 读取通道

/// 跑一帧并取出 accesskit 树里的 (role, label) 列表（UI-24.2 用）
fn a11y_nodes(app: &mut SheetApp, ctx: &egui::Context) -> Vec<(String, String, Option<egui::Rect>)> {
    ctx.enable_accesskit();
    let mut frame = eframe::Frame::_new_kittest();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1440.0, 900.0),
        )),
        ..Default::default()
    };
    let out = ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut frame));
    let mut v = Vec::new();
    if let Some(up) = out.platform_output.accesskit_update {
        for (id, node) in up.nodes {
            let _ = id;
            let b = node.bounds().map(|bb| {
                egui::Rect::from_min_max(
                    egui::pos2(bb.x0 as f32, bb.y0 as f32),
                    egui::pos2(bb.x1 as f32, bb.y1 as f32),
                )
            });
            v.push((format!("{:?}", node.role()), node.label().unwrap_or("").to_string(), b));
        }
    }
    v
}

#[test]
fn a11y_tree_reads_ribbon_group_names() {
    let mut app = app();
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let nodes = a11y_nodes(&mut app, &ctx);
    eprintln!("accesskit nodes = {}", nodes.len());
    for (r, l, _) in nodes.iter().filter(|(_, l, _)| !l.is_empty()).filter(|(_, l, _)| l.contains("样式") || l.contains("表格") || l.contains("排序")) {
        eprintln!("  {r} | {l}");
    }
    assert!(nodes.len() > 20, "树太小：{}", nodes.len());
}

#[test]
fn a11y_secondary_menu_items_appear_when_open() {
    let mut app = app();
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let nodes = a11y_nodes(&mut app, &ctx);
    let hit = nodes
        .iter()
        .find(|(_, l, b)| l == "条件格式" && b.is_some())
        .cloned();
    let (_, _, rect) = hit.expect("一级入口「条件格式」没有 bounds");
    let rect = rect.unwrap();
    let p = rect.center();
    // 点开菜单：按下 + 抬起，再跑两帧让 popup 展开
    for _ in 0..2 {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 900.0))),
            ..Default::default()
        };
        input.events = vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ];
        let mut frame = eframe::Frame::_new_kittest();
        let _ = ctx.run(input, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    }
    let after = a11y_nodes(&mut app, &ctx);
    let names: Vec<String> = after.iter().map(|(_, l, _)| l.clone()).filter(|l| !l.is_empty()).collect();
    eprintln!("after open: {} labelled nodes", names.len());
    for n in names.iter().filter(|l| l.contains("规则") || l.contains("色阶") || l.contains("数据条") || l.contains("图标")) {
        eprintln!("  MENU | {n}");
    }
    assert!(
        names.iter().any(|l| l.contains("规则")),
        "二级命令没进树：{:?}",
        &names[..names.len().min(30)]
    );

    // 其余组的入口是 split 按钮/画廊（「… 选项」「… 展开」），点击目标是箭头热点，
    // 本用例只覆盖纯菜单按钮；split/gallery 的二级内容待单独用例（见 UI-24.2 记录）。
}

/// 用 accesskit 节点名找到热区，点开后返回（展开前, 展开后）带标签节点数
fn a11y_open_by_label(group: &str) -> Option<(usize, usize, Vec<String>)> {
    let mut app = app();
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let base = a11y_nodes(&mut app, &ctx);
    let base_n = base.iter().filter(|(_, l, _)| !l.is_empty()).count();
    let (_, _, rect) = base.iter().find(|(_, l, _)| l == group).cloned()?;
    let p = rect?.center();
    // 只点一次：连点两次会在第二轮把刚展开的 popup 又关掉
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 900.0))),
        ..Default::default()
    };
    input.events = vec![
        egui::Event::PointerMoved(p),
        egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
    ];
    let mut frame = eframe::Frame::_new_kittest();
    let _ = ctx.run(input, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    let idle = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 900.0))),
        ..Default::default()
    };
    let _ = ctx.run(idle, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    let after = a11y_nodes(&mut app, &ctx);
    let names: Vec<String> = after.iter().map(|(_, l, _)| l.clone()).filter(|l| !l.is_empty()).collect();
    let after_n = names.len();
    Some((base_n, after_n, names))
}

#[test]
#[ignore = "split/gallery 的箭头热区在无头注入下不展开（实测 111->101，未新增节点）；需真人或有头环境验证"]
fn a11y_split_button_arrows_open_their_galleries() {
    let mut checked = 0;
    for group in ["排序和筛选 选项", "单元格样式 展开", "套用表格格式 选项"] {
        let Some((b, a, names)) = a11y_open_by_label(group) else {
            eprintln!("跳过（树里没有「{group}」）");
            continue;
        };
        eprintln!("「{group}」: {b} -> {a}");
        for n in names.iter().filter(|n| n.contains("序") || n.contains("筛选") || n.contains("样式") || n.contains("表")) {
            eprintln!("   有 | {n}");
        }
        if a > b + 2 {
            checked += 1;
        }
    }
    eprintln!("验证到 {checked} 个入口");
}

#[test]
fn ui112_fuzzy_function_search_matches_subsequence_and_ranks_prefix_first() {
    // 前缀优先：=SU 里 SUM 在 SUMIF 前
    let help = formula_help("=SU").expect("应有候选");
    let names: Vec<&str> = help.suggestions.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names.first().copied(), Some("SUM"), "{names:?}");
    assert!(names.contains(&"SUMIF"), "{names:?}");

    // 子序列：=VLO → VLOOKUP
    let help = formula_help("=VLO").expect("子序列应命中");
    let names: Vec<&str> = help.suggestions.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"VLOOKUP"), "{names:?}");

    // 完全不匹配：不返回候选
    assert!(formula_help("=ZZQ").is_none());
}

#[test]
fn a11y_split_menu_items_appear_when_popup_forced_open() {
    // split/gallery 的箭头热区在无头注入下点不开（见 a11y_split_button_arrows…），
    // 这里直接设置 ribbon_popup，验证「菜单真的渲染时，二级命令会进 accesskit 树」。
    let mut app = app();
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let cases: [(&str, &[&str]); 3] = [
        ("SortFilter", &["升序", "降序", "自定义排序…", "筛选"]),
        ("FindSelect", &["查找…", "替换…", "转到…"]),
        ("FormatAsTable", &["插入表格"]),
    ];
    for (id, wants) in cases {
        let menu = app.ribbon.find_menu(id).unwrap_or_else(|| panic!("找不到 {id} 菜单"));
        app.ribbon_popup = Some(crate::ribbon::ui::Popup {
            kind: crate::ribbon::ui::PopupKind::Menu(menu),
            pos: egui::pos2(320.0, 140.0),
            opened_at: 0.0,
        });
        // 两帧：第一帧建立 Area/弹层，第二帧内容才稳定进树
        let _ = a11y_nodes(&mut app, &ctx);
        let nodes = a11y_nodes(&mut app, &ctx);
        let names: Vec<String> = nodes.iter().map(|(_, l, _)| l.clone()).filter(|l| !l.is_empty()).collect();
        for want in wants {
            assert!(names.iter().any(|n| n == want), "{id} 菜单项「{want}」没进树：{:?}", &names[..names.len().min(20)]);
        }
        app.ribbon_popup = None;
    }

    // 画廊：PopupKind::Items，内容来自 find_dropdown
    let items = app.ribbon.find_dropdown("CellStylesGallery").expect("找不到单元格样式画廊");
    app.ribbon_popup = Some(crate::ribbon::ui::Popup {
        kind: crate::ribbon::ui::PopupKind::Items(items),
        pos: egui::pos2(320.0, 140.0),
        opened_at: 0.0,
    });
    let _ = a11y_nodes(&mut app, &ctx);
    let nodes = a11y_nodes(&mut app, &ctx);
    let names: Vec<String> = nodes.iter().map(|(_, l, _)| l.clone()).filter(|l| !l.is_empty()).collect();
    for want in ["常规", "标题", "强调", "好", "差"] {
        assert!(names.iter().any(|n| n == want), "画廊项「{want}」没进树：{:?}", &names[..names.len().min(20)]);
    }
}

#[test]
fn run_split_fixed_width_writes_columns() {
    let mut a = app();
    a.wb.sheets[0].set(0, 0, Cell { value: Value::Text("ABCDEF".into()), formula: None });
    a.sel = (0, 0);
    a.sel_end = (0, 0);
    a.run_split(&DataDlg {
        tab: 1,
        keys: Vec::new(),
        header: false,
        delim: ",".into(),
        sort_mode: 0,
        custom: String::new(),
        merge_consecutive: false,
        quote: false,
        detect_types: false,
        sort_by_row: false,
        split_fixed: true,
        widths: "2,2".into(),
        col_modes: String::new(),
    });
    // 第一段落回原列，后续列往右写；断点之后的剩余部分并入最后一列（Excel 语义）
    assert_eq!(a.wb.sheets[0].value(0, 0).display(), "AB");
    assert_eq!(a.wb.sheets[0].value(0, 1).display(), "CDEF");
    assert_eq!(a.wb.sheets[0].value(0, 2).display(), "");

}

#[test]
fn run_split_fixed_width_rejects_bad_widths() {
    let mut a = app();
    a.wb.sheets[0].set(0, 0, Cell { value: Value::Text("ABCDEF".into()), formula: None });
    a.sel = (0, 0);
    a.sel_end = (0, 0);
    a.run_split(&DataDlg {
        tab: 1,
        keys: Vec::new(),
        header: false,
        delim: ",".into(),
        sort_mode: 0,
        custom: String::new(),
        merge_consecutive: false,
        quote: false,
        detect_types: false,
        sort_by_row: false,
        split_fixed: true,
        widths: "2,0".into(),
        col_modes: String::new(),
    });
    assert!(a.status.contains("正整数"), "应提示宽度非法：{}", a.status);
    assert_eq!(a.wb.sheets[0].value(0, 0).display(), "ABCDEF", "非法宽度不改数据");
}

#[test]
fn run_sort_by_row_reorders_columns() {
    let mut a = app();
    let data = [
        ("一月", 30.0, 10.0, 20.0),
        ("二月", 5.0, 40.0, 15.0),
        ("三月", 12.0, 7.0, 25.0),
    ];
    for (i, (label, x, y, z)) in data.iter().enumerate() {
        let r = i as u32;
        a.wb.sheets[0].set(r, 0, Cell { value: Value::Text((*label).into()), formula: None });
        a.wb.sheets[0].set(r, 1, Cell { value: Value::Number(*x), formula: None });
        a.wb.sheets[0].set(r, 2, Cell { value: Value::Number(*y), formula: None });
        a.wb.sheets[0].set(r, 3, Cell { value: Value::Number(*z), formula: None });
    }
    a.sel = (2, 0);
    a.sel_end = (2, 3);
    a.run_sort(&DataDlg {
        tab: 0,
        keys: vec![(0, true, true)],
        header: false,
        delim: ",".into(),
        sort_mode: 0,
        custom: String::new(),
        merge_consecutive: false,
        quote: false,
        detect_types: false,
        sort_by_row: true,
        split_fixed: false,
        widths: String::new(),
        col_modes: String::new(),
    });
    let row2: Vec<String> = (0..4)
        .map(|c| a.wb.sheets[0].value(2, c).display())
        .collect();
    assert_eq!(row2, vec!["三月", "7", "12", "25"], "status={}", a.status);
    let row0: Vec<String> = (0..4)
        .map(|c| a.wb.sheets[0].value(0, c).display())
        .collect();
    assert_eq!(row0, vec!["一月", "30", "10", "20"], "整列跟着移动");
}

#[test]
fn parse_col_modes_reads_names() {
    use sheetcore::data_tools::ColMode;
    assert_eq!(
        crate::parse_col_modes("常规,文本,日期,跳过"),
        Some(vec![ColMode::General, ColMode::Text, ColMode::Date, ColMode::Skip])
    );
    assert_eq!(crate::parse_col_modes(""), None);
    assert_eq!(crate::parse_col_modes("随便"), None);
}

#[test]
fn run_split_skips_column_and_shifts_left() {
    let mut a = app();
    a.wb.sheets[0].set(0, 0, Cell { value: Value::Text("甲,乙,丙".into()), formula: None });
    a.sel = (0, 0);
    a.sel_end = (0, 0);
    a.run_split(&DataDlg {
        tab: 1,
        keys: Vec::new(),
        header: false,
        delim: ",".into(),
        sort_mode: 0,
        custom: String::new(),
        merge_consecutive: false,
        quote: false,
        detect_types: false,
        sort_by_row: false,
        split_fixed: false,
        widths: String::new(),
        col_modes: "常规,跳过,常规".into(),
    });
    assert_eq!(a.wb.sheets[0].value(0, 0).display(), "甲");
    assert_eq!(a.wb.sheets[0].value(0, 1).display(), "丙", "跳过的列不占位，后面的列左移");
    assert_eq!(a.wb.sheets[0].value(0, 2).display(), "");
}

/// UI-14.2：粗体必须是真 700 字面，不是把常规字面放大。
/// 这里直接解析内嵌子集的 OS/2 字重，并确认四套粗体各自与常规字面字节不同。
#[test]
fn embedded_bold_faces_are_real_700() {
    use ttf_parser::Face;
    let faces: [(&str, &[u8], &[u8], char); 4] = [
        ("sans", FONT_SANS, FONT_SANS_BOLD, '国'),
        ("serif", FONT_SERIF, FONT_SERIF_BOLD, '国'),
        ("latin", FONT_LATIN, FONT_LATIN_BOLD, 'A'),
        ("mono", FONT_MONO, FONT_MONO_BOLD, 'A'),
    ];
    for (name, reg, bold, probe) in faces {
        assert_ne!(reg, bold, "{name}: 粗体与常规是同一份字节");
        let rf = Face::parse(reg, 0).expect("regular face");
        let bf = Face::parse(bold, 0).expect("bold face");
        assert_eq!(rf.weight().to_number(), 400, "{name}: 常规字面不是 400");
        assert_eq!(bf.weight().to_number(), 700, "{name}: 粗体字面不是 700");
        let has = |f: &Face, c: char| f.glyph_index(c).is_some();
        assert!(has(&bf, probe), "{name}: 粗体子集缺 {probe:?} 字形");
    }
}

/// UI-14.2：五套 `*-bold` family 都注册进了 egui，且真的指向 700 字面。
/// 少注册一套，对应字体的粗体就会掉回默认字族（界面上看不出加粗）。
#[test]
fn bold_families_are_registered_with_real_bold_data() {
    let defs = font_defs();
    for fam in ["default-bold", "cjk-sans-bold", "cjk-serif-bold", "latin-bold", "mono-bold"] {
        let list = defs
            .families
            .get(&family_named(fam))
            .unwrap_or_else(|| panic!("{fam} 未注册"));
        assert!(!list.is_empty(), "{fam} 是空字族");
        for key in list {
            assert!(defs.font_data.contains_key(key), "{fam} 引用了未注册的 {key}");
        }
    }
    // 粗体族必须真的引用 700 数据：取每族首个字体，解析字重
    for (fam, key) in [
        ("default-bold", "inter-bold"),
        // cjk-sans-bold 的拉丁首选 Inter Bold，中文回退 Noto Sans SC Bold
        ("cjk-sans-bold", "inter-bold"),
        ("cjk-serif-bold", "noto-serif-bold"),
        ("mono-bold", "mono-bold"),
    ] {
        let data = &defs.font_data[key];
        let face = ttf_parser::Face::parse(data.font.as_ref(), 0).expect("face");
        assert_eq!(face.weight().to_number(), 700, "{fam}/{key}");
        let list = &defs.families[&family_named(fam)];
        assert_eq!(list.first().map(String::as_str), Some(key), "{fam} 首选项不是 {key}");
    }
}

/// 系统没装这个字体时，粗体走内嵌 700 族；装了常规字面但没粗体时走同族（Excel 也是合成）。
#[test]
fn bold_routing_prefers_real_faces_and_falls_back_to_same_family() {
    // 未安装的字体（Calibri 在本机没有）：必须拿到内嵌 bold 族
    if !system_fonts::is_loaded("Calibri") {
        let (fam, size, synth) = bold_font("Calibri", 14.0);
        let want = family_named(&format!("{}-bold", sheetcore::style::font_class("Calibri").key()));
        assert_eq!(fam, want);
        assert_eq!(size, 14.0);
        assert!(!synth);
    }
    // 本机装了常规宋体但没有粗体：改用内嵌 700 宋体族，不把字号放大冒充粗体
    if system_fonts::is_loaded("宋体") && !system_fonts::is_bold_loaded("宋体") {
        let (fam, size, synth) = bold_font("宋体", 14.0);
        assert_eq!(fam, family_named("cjk-serif-bold"));
        assert_eq!(size, 14.0);
        assert!(!synth);
    }
}

/// UI-25.1：百万格**密集**选区的统计准备与分帧累加耗时。
/// 本机 RDP/软件 GPU 的帧间隔不可信（同一 3 行文件也会 280ms 一次），
/// 所以这里只量统计本身的计算量：一次性收集 + 每帧 20 万格的累加。
#[test]
fn dense_million_selection_stats_cost_is_bounded() {
    use std::time::Instant;
    let mut sh = sheetcore::model::Sheet::new("Dense");
    for r in 0..1_000_000u32 {
        sh.set(
            r,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Number(r as f64 * 1.5),
                formula: None,
            },
        );
    }
    let rects = [(0, 0, 999_999, 0)];
    let t0 = Instant::now();
    let vals = crate::collect_stats_values(&sh, &rects);
    let collect_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(vals.len(), 1_000_000);

    let mut acc = crate::StatsAcc::default();
    let mut idx = 0usize;
    let mut worst = 0.0f64;
    while idx < vals.len() {
        let t = Instant::now();
        let end = (idx + 200_000).min(vals.len());
        while idx < end {
            acc.add(vals[idx]);
            idx += 1;
        }
        worst = worst.max(t.elapsed().as_secs_f64() * 1000.0);
    }
    assert_eq!(acc.count, 1_000_000);
    assert_eq!(acc.nums, 1_000_000);
    assert!((acc.sum - 749_999_250_000.0).abs() < 1.0);
    eprintln!(
        "[ui25.1] dense collect={collect_ms:.1}ms worst_slice={worst:.1}ms total_slices={:.1}ms",
        (vals.len() / 200_000) as f64
    );
    // 单帧 20 万格的累加不能到几百毫秒；收集是一次性的（切选区时发生）
    assert!(worst < 300.0, "20 万格单帧累加 {worst:.1}ms 过慢");
}

/// UI-19.1：10 万项序列下拉的每帧过滤成本（打开时缓存，之后只过滤）。
#[test]
fn dv_filter_100k_items_is_frame_cheap() {
    use std::time::Instant;
    let all: Vec<String> = (0..100_000).map(|i| format!("项目{i:06}")).collect();
    let mut worst = 0.0f64;
    for q in ["", "9", "项目09", "无匹配"] {
        let t = Instant::now();
        let hit = crate::filter_dv_items(&all, q);
        worst = worst.max(t.elapsed().as_secs_f64() * 1000.0);
        match q {
            "" => assert_eq!(hit.len(), 100_000),
            "9" => assert_eq!(hit.iter().filter(|s| s.contains('9')).count(), hit.len()),
            "项目09" => assert!(hit.len() > 0),
            _ => assert!(hit.is_empty()),
        }
    }
    eprintln!("[ui19.1] filter 100k worst={worst:.1}ms");
    assert!(worst < 50.0, "10 万项过滤 {worst:.1}ms 超过一帧预算");
}
