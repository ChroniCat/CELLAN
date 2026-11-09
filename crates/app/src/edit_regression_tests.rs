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
    app.editing = Some(EditState { buf: "99".into() });
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
    app.editing = Some(EditState { buf: "123".into() });
    app.open_in_new_tab(PathBuf::new());
    app.editing = Some(EditState { buf: "456".into() });
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
    app.editing = Some(EditState { buf: "1".into() });
    app.close_tab(1);
    assert_eq!(app.tabs.len(), 2);
    app.editing = Some(EditState { buf: "2".into() });
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
        app.editing = Some(EditState { buf: "3".into() });
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
    app.editing = Some(EditState { buf: "30".into() });
    app.commit_edit(None);
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Number(30.0));
}
