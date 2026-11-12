// BEGIN AUDIT UI EDIT PROBE 20261112
// Reproduce by temporarily appending this block inside edit_regression_tests.rs.
// Run only: cargo test -p app --no-default-features --features glow audit_ui_edit_commit_20261112 -- --nocapture --test-threads=1
// This drives the real handle_keys -> draw_grid_pane event sequence; no native window.
#[test]
fn audit_ui_edit_commit_20261112() {
    fn frame(app: &mut SheetApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                app.handle_keys(ctx);
                app.draw_grid_pane(ui, ctx, false);
            });
        });
    }
    let mut failures = Vec::new();
    for key in [egui::Key::Enter, egui::Key::Tab] {
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.sel = (0, 0);
        app.sel_end = (0, 0);
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![egui::Event::Text("123".into())]);
        frame(&mut app, &ctx, vec![]);
        let before = app.editing.as_ref().map(|ed| ed.buf.clone());
        eprintln!("AUDIT before {:?}: editing={:?}, A1={:?}, sel={:?}", key, before, app.wb.sheet(0).value(0, 0), app.sel);
        assert_eq!(before.as_deref(), Some("123"), "text input must reach the real editor before submit");
        frame(&mut app, &ctx, vec![key_event(key, false)]);
        frame(&mut app, &ctx, vec![]);
        let value = app.wb.sheet(0).value(0, 0);
        let editing = app.editing.as_ref().map(|ed| ed.buf.clone());
        eprintln!("AUDIT after {:?}: editing={:?}, A1={:?}, sel={:?}, dirty={}, undo={:?}", key, editing, value, app.sel, app.wb.dirty, app.undo.depth());
        let expected_sel = if key == egui::Key::Enter { (1, 0) } else { (0, 1) };
        if value != Value::Number(123.0) || editing.is_some() || app.sel != expected_sel {
            failures.push(format!("{:?}: expected A1=123, editing=None, sel={:?}; got A1={:?}, editing={:?}, sel={:?}", key, expected_sel, value, editing, app.sel));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
// END AUDIT UI EDIT PROBE 20261112
