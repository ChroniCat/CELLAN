//! Regressions for data-preserving edits through the actual app handlers.

use super::*;

fn app() -> SheetApp {
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
    app.clip = Some(Clip { sheet: 1, rect: (0, 0, 0, 0), cells, cut: true });
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
    app.clip = Some(Clip {
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
    app.clip = Some(Clip {
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
    app.clip = Some(Clip {
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
    app.clip = Some(Clip {
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
    app.clip = Some(Clip {
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
    app.clip = Some(Clip {
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
