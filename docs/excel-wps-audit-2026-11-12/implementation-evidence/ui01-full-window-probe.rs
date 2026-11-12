// UI-01 失败复现探针：把这一块追加到修复前的 crates/app/src/edit_regression_tests.rs，
// 只跑这一项，即可在旧代码上看到 A1 仍为空。
//
//   cargo test -p app --no-default-features --features glow --offline --bin app -- \
//       ui01_probe_full_window_enter --nocapture --test-threads=1
//
// 依赖文件里已有的 key_event()。修复后同一断言应当通过。
fn ui01_frame(app: &mut SheetApp, ctx: &egui::Context, events: Vec<egui::Event>) {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0))),
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

#[test]
fn ui01_probe_full_window_enter() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    ui01_frame(&mut app, &ctx, vec![]);
    ui01_frame(&mut app, &ctx, vec![egui::Event::Text("123".into())]);
    assert_eq!(app.editing.as_ref().map(|e| e.buf.clone()).as_deref(), Some("123"));
    ui01_frame(&mut app, &ctx, vec![key_event(egui::Key::Enter, false)]);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0), "Enter 必须提交 A1");
    assert_eq!(app.sel, (1, 0));
    assert_eq!(app.undo.depth(), (1, 0));
}
