//! Regressions for data-preserving edits through the actual app handlers.

use super::*;

fn app() -> SheetApp {
    SheetApp::new_with_context(&egui::Context::default(), None)
}

fn number(value: f64) -> Cell {
    Cell { value: Value::Number(value), formula: None }
}

fn formula(text: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(text.into()) }
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
            app.run_sort(&DataDlg { tab: 0, keys: vec![(0, true, true)], header: false, delim: ",".into() });
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
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
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
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
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
    app.clip = Some(Clip {
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
    assert_eq!(labels, ["插入…", "删除", "重命名", "复制", "工作表标签颜色", "设置单元格格式…"]);
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
    env.sc("SC_FAIL_SAVE_STEP", "layout");
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
