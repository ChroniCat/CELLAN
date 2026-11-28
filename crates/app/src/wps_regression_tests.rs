//! Regressions covering WPS-style UX/UI Categories 1-4 behaviors.

use super::*;

fn wps_app(ctx: &egui::Context) -> SheetApp {
    crate::clear_test_clipboard();
    let mut app = SheetApp::new_with_context(ctx, None);
    app.show_start = false;
    app
}

fn compact_dialog_frame(
    app: &mut SheetApp, ctx: &egui::Context, dialog: fn(&mut SheetApp, &egui::Context),
    screen: Rect, events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
    input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(1.5);
    ctx.run(input, |ctx| dialog(app, ctx))
}

fn dialog_control(out: &egui::FullOutput, role: egui::accesskit::Role, label: &str) -> Rect {
    let update = out.platform_output.accesskit_update.as_ref().unwrap();
    let node = update.nodes.iter().map(|(_, n)| n).find(|n| n.role() == role && n.label() == Some(label))
        .unwrap_or_else(|| panic!("找不到 {role:?} 控件 {label}"));
    let b = node.bounds().unwrap();
    Rect::from_min_max(egui::pos2(b.x0 as f32, b.y0 as f32), egui::pos2(b.x1 as f32, b.y1 as f32))
}

#[test]
fn compact_validation_pages_preserve_the_draft_and_cancel_resets_the_next_session() {
    use egui::accesskit::Role;
    let _env = crate::test_env::TestEnv::new("validation-dialog-pages");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    ctx.enable_accesskit();
    let rule = sheetcore::dv::DataValidation {
        kind: sheetcore::dv::DvKind::Whole, f1: "1".into(), f2: "100".into(),
        prompt_title: "提示".into(), error_msg: "超出范围".into(), ..Default::default()
    };
    app.dv_edit = Some(rule.clone());
    let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(600.0, 420.0));
    let frame = |app: &mut SheetApp, events| compact_dialog_frame(app, &ctx, SheetApp::dv_dialog, screen, events);
    let click = |app: &mut SheetApp, rect: Rect| {
        let pos = rect.center();
        for pressed in [true, false] {
            let _ = frame(app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
            }]);
        }
        frame(app, vec![])
    };
    let _ = frame(&mut app, vec![]);
    let mut out = frame(&mut app, vec![]);
    let mut expected = rule.clone();
    assert!(screen.contains_rect(dialog_control(&out, Role::TextInput, "最大值")));
    for (page, field) in [("输入提示", "提示内容"), ("出错警告", "出错说明")] {
        out = click(&mut app, dialog_control(&out, Role::Tab, page));
        let input = dialog_control(&out, Role::TextInput, field);
        assert!(screen.contains_rect(input));
        if page == "输入提示" {
            let _ = click(&mut app, input);
            out = frame(&mut app, vec![egui::Event::Text("新提示".into())]);
            expected.prompt_msg = "新提示".into();
        }
        assert!(screen.contains_rect(dialog_control(&out, Role::Button, "确定")));
        assert_eq!(app.dv_edit.as_ref().unwrap(), &expected);
    }
    let _ = click(&mut app, dialog_control(&out, Role::Button, "取消"));
    assert!(app.dv_edit.is_none());
    assert!(app.wb.sheet(0).dv.is_empty());
    app.dv_edit = Some(rule);
    let _ = frame(&mut app, vec![]);
    let out = frame(&mut app, vec![]);
    assert!(screen.contains_rect(dialog_control(&out, Role::TextInput, "最小值")));
}

#[test]
fn compact_cf_manager_keeps_long_formulas_and_rule_actions_inside_the_window() {
    use egui::accesskit::Role;
    use sheetcore::cf::{CfBlock, CfKind, CfRule};
    let _env = crate::test_env::TestEnv::new("cf-manager-long-formula");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    ctx.enable_accesskit();
    let formula = format!("AND({})", ["A1>AVERAGE($A$1:$D$20)"; 6].join(","));
    app.wb.sheet_mut(0).cf.blocks = vec![CfBlock {
        ranges: vec![(0, 0, 2, 2)],
        rules: vec![CfRule { kind: CfKind::Expr { formula, dxf: 0 }, stop: false, priority: 1 }],
    }];
    app.cf_mgr = true;
    let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(520.0, 380.0));
    let _ = compact_dialog_frame(&mut app, &ctx, SheetApp::cf_manager, screen, vec![]);
    let out = compact_dialog_frame(&mut app, &ctx, SheetApp::cf_manager, screen, vec![]);
    let formula = out.shapes.iter().find_map(|s| match &s.shape {
        egui::Shape::Text(t) if t.galley.text().contains("AVERAGE") => Some(t),
        _ => None,
    }).expect("条件公式应可见");
    assert!(formula.galley.rows.len() > 1 && !formula.galley.elided);
    assert!(screen.contains_rect(formula.visual_bounding_rect()));
    for label in ["新建规则", "编辑", "删除"] {
        let rect = dialog_control(&out, Role::Button, label);
        assert!(screen.contains_rect(rect), "{label}: {rect:?}");
    }
    let tree = out.platform_output.accesskit_update.unwrap();
    for label in ["上移", "下移"] {
        let node = tree.nodes.iter().map(|(_, n)| n).find(|n| n.label() == Some(label)).unwrap();
        assert!(node.is_disabled(), "仅一块规则时应禁用{label}");
    }
}

#[test]
fn compact_right_click_opens_the_row_column_and_sheet_menus_at_native_scale() {
    let _env = crate::test_env::TestEnv::new("compact-context-menu-pointers");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(600.0, 580.0 / 1.5));
    let mut host = eframe::Frame::_new_kittest();
    let mut render = |app: &mut SheetApp, events| {
        let mut input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(1.5);
        ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut host))
    };
    let _ = render(&mut app, vec![]);
    let out = render(&mut app, vec![]);
    let center_of = |text: &str| out.shapes.iter().find_map(|s| match &s.shape {
        egui::Shape::Text(t) if t.galley.text() == text => Some(t.visual_bounding_rect().center()),
        _ => None,
    }).unwrap_or_else(|| panic!("未绘制 {text}"));
    let sheet = center_of(&app.wb.sheet(0).name);
    for (pos, expected) in [(center_of("2"), CtxKind::Rows), (center_of("F"), CtxKind::Cols), (sheet, CtxKind::Tab)] {
        let _ = render(&mut app, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            let _ = render(&mut app, vec![egui::Event::PointerButton {
                pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE,
            }]);
        }
        let _ = render(&mut app, vec![]);
        let menu = app.ctx_menu.as_ref().expect("右键应打开菜单");
        assert_eq!(menu.kind, expected);
        assert!(app.ctx_rects.iter().all(|rect| screen.contains_rect(*rect)));
        assert_eq!(app.ctx_rects.len(), 1, "行、列和工作表菜单不应附加迷你工具栏");
        if expected == CtxKind::Tab {
            for key in [egui::Key::End, egui::Key::ArrowUp, egui::Key::ArrowRight, egui::Key::Escape] {
                let _ = render(&mut app, vec![egui::Event::Key {
                    key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                }]);
                let menu = app.ctx_menu.as_ref().expect("离开颜色子菜单后仍应显示工作表菜单");
                let rows = ctx_tab_rows();
                if key == egui::Key::End {
                    assert_eq!(menu.key, rows.len() - 1);
                } else {
                    assert_eq!(rows[menu.key].label, "工作表标签颜色");
                    assert_eq!(menu.sub.is_some(), key == egui::Key::ArrowRight);
                }
            }
        }
        app.ctx_menu = None;
        let _ = render(&mut app, vec![]);
    }
}

fn wps_frame(app: &mut SheetApp, ctx: &egui::Context, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1440.0, 900.0),
        )),
        modifiers,
        events,
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    let _ = ctx.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame));
}

#[test]
fn command_search_exposes_later_matches_and_executes_the_current_input() {
    let _env = crate::test_env::TestEnv::new("command-search-results");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.palette_open = true;
    let key = |key| egui::Event::Key {
        key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
    };
    let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(760.0, 420.0));
    let frame = |app: &mut SheetApp, events| ctx.run(egui::RawInput {
        screen_rect: Some(screen), events, ..Default::default()
    }, |ctx| app.command_palette(ctx));
    let mut matches = palette::filter(&palette::entries(&app), "");
    cmdusage::rank(&mut matches, &cmdusage::load());
    let _ = frame(&mut app, vec![]);
    let _ = frame(&mut app, vec![]);
    for _ in 0..43 {
        let _ = frame(&mut app, vec![key(Key::ArrowDown)]);
    }
    let after = frame(&mut app, vec![]);
    assert_eq!(app.palette_idx, 43);
    assert!(after.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(t)
        if t.galley.job.text == matches[43].label && s.clip_rect.contains(t.visual_bounding_rect().center()))),
        "选中的命令应滚入视口：{}", matches[43].label);

    let _ = frame(&mut app, vec![key(Key::Enter), egui::Event::Text("冻结首行".into())]);
    assert_eq!(app.palette_q, "冻结首行", "搜索栏应收到本帧的新查询");
    assert_eq!(app.wb.sheet(0).freeze, Some((1, 0)), "已执行的动作：{:?}", cmdusage::load());
    assert!(!app.palette_open);

    app.palette_open = true;
    app.palette_q.clear();
    let _ = frame(&mut app, vec![]);
    let _ = frame(&mut app, vec![egui::Event::Text("About".into()), key(Key::Enter)]);
    assert!(app.about_open && !app.palette_open, "输入与 Enter 同帧应执行当前查询命令");
}

#[test]
fn find_results_show_last_match_beyond_five_hundred_and_only_report_current_count() {
    let _env = crate::test_env::TestEnv::new("find-results-list");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    for row in 0..525 {
        app.wb.sheet_mut(0).set(row, 0, Cell { value: Value::Text("同名".into()), formula: None });
    }
    app.find.find = "同名".into();
    app.find_summary(true);
    assert_eq!(app.current_find_count(), Some(525));
    app.find.find = "其他".into();
    assert_eq!(app.current_find_count(), None);
    let frame = |app: &mut SheetApp, events| ctx.run(egui::RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(900.0, 700.0))),
        events, ..Default::default()
    }, |ctx| app.find_all_window(ctx));
    let _ = frame(&mut app, vec![]);
    let initial = frame(&mut app, vec![]);
    let locate = |output: &egui::FullOutput, label: &str| output.shapes.iter().find_map(|s| match &s.shape {
        egui::Shape::Text(t) if t.galley.job.text == label && s.clip_rect.contains_rect(t.visual_bounding_rect()) => {
            Some(t.visual_bounding_rect().center())
        }
        _ => None,
    });
    let first = locate(&initial, "A1").expect("首条匹配应可见");
    let mut after = frame(&mut app, vec![egui::Event::PointerMoved(first), egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -30_000.0), modifiers: egui::Modifiers::NONE,
    }]);
    for _ in 0..60 {
        if locate(&after, "A525").is_some() { break; }
        after = frame(&mut app, vec![]);
    }
    assert!(locate(&after, "A525").is_some(), "最后一条结果必须能滚进视口");
}

#[test]
fn cat1_sparse_whole_col_row_sheet_styles_and_clear() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);

    // Whole column C (col = 2)
    app.sel = (0, 2);
    app.sel_end = (1_048_575, 2);
    app.apply_fill_color(Some([255, 240, 200]));
    app.ribbon_action("Bold", &ctx);

    {
        let sh = app.wb.sheet(0);
        assert!(sh.col_styles.contains_key(&2), "整列样式应写入 col_styles");
        assert!(sh.is_empty(), "整列设色不得实例化 104 万格");
        let st = app.wb.styles.get(sh.style_of(500_000, 2));
        assert_eq!(st.fill, Some([255, 240, 200]));
        assert!(st.bold);
    }

    // Whole row 6 (row = 5)
    app.sel = (5, 0);
    app.sel_end = (5, 16_383);
    app.ribbon_action("Italic", &ctx);
    {
        let sh = app.wb.sheet(0);
        assert!(sh.row_styles.contains_key(&5), "整行样式应写入 row_styles");
        assert!(sh.is_empty(), "整行设斜体不得实例化 1.6 万格");
        let st = app.wb.styles.get(sh.style_of(5, 10_000));
        assert!(st.italic);
    }

    // Whole sheet clear formats must clear col_styles/row_styles sparsely
    app.sel = (0, 0);
    app.sel_end = (1_048_575, 16_383);
    app.clear_rects(ClearPart::Formats);
    {
        let sh = app.wb.sheet(0);
        assert!(sh.col_styles.is_empty(), "全表清格式应清空 col_styles");
        assert!(sh.row_styles.is_empty(), "全表清格式应清空 row_styles");
        assert_eq!(sh.style_of(500_000, 2), 0);
    }
}

#[test]
fn cat1_remembered_colors_black_borders_percent_and_alignment() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);

    // Remembered fill & font color
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.apply_fill_color(Some([12, 34, 56]));
    assert_eq!(app.last_fill_color, [12, 34, 56]);
    assert!(app.recent_colors.contains(&[12, 34, 56]));

    app.sel = (1, 1);
    app.sel_end = (1, 1);
    app.ribbon_action("FillColor", &ctx);
    let st_b2 = app.wb.styles.get(app.wb.sheet(0).style_of(1, 1));
    assert_eq!(st_b2.fill, Some([12, 34, 56]), "主按钮直接复用记忆填充色");

    app.apply_font_color([200, 40, 20]);
    assert_eq!(app.last_font_color, [200, 40, 20]);
    app.sel = (2, 2);
    app.sel_end = (2, 2);
    app.ribbon_action("FontColor", &ctx);
    let st_c3 = app.wb.styles.get(app.wb.sheet(0).style_of(2, 2));
    assert_eq!(st_c3.color, [200, 40, 20], "主按钮直接复用记忆字体色");

    // Black default border & BordersDefault
    app.ribbon_action("BordersAll", &ctx);
    assert_eq!(app.last_border_action, "BordersAll");
    let st_b = app.wb.styles.get(app.wb.sheet(0).style_of(2, 2));
    assert_eq!(st_b.border[0].color, [0, 0, 0], "默认边框色应为纯黑");
    assert!(st_b.border[0].style > 0);

    app.sel = (3, 3);
    app.sel_end = (3, 3);
    app.ribbon_action("BordersDefault", &ctx);
    let st_d4 = app.wb.styles.get(app.wb.sheet(0).style_of(3, 3));
    assert!(st_d4.border.iter().all(|b| b.style > 0 && b.color == [0, 0, 0]));

    // Percent 0%, CenterAcross (halign=4), StrikeThrough, DoubleUnderline, TextVertical
    app.ribbon_action("PercentStyle", &ctx);
    app.ribbon_action("CenterAcross", &ctx);
    app.ribbon_action("StrikeThrough", &ctx);
    app.ribbon_action("DoubleUnderline", &ctx);
    app.ribbon_action("TextVertical", &ctx);
    let st_d4 = app.wb.styles.get(app.wb.sheet(0).style_of(3, 3));
    assert_eq!(st_d4.format, "0%");
    assert_eq!(st_d4.halign, 4);
    assert!(st_d4.strikethrough);
    assert!(st_d4.double_underline);
    assert!(st_d4.vertical_text);
}

#[test]
fn cat1_backspace_edits_immediately_and_autosum_probes_left() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Text("旧文本".into()), formula: None });
    app.sel = (0, 0);
    app.sel_end = (0, 0);

    // Warm frame so grid has focus/bounds, then send Backspace
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    let bksp = egui::Event::Key {
        key: egui::Key::Backspace,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    wps_frame(&mut app, &ctx, vec![bksp], egui::Modifiers::NONE);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Empty, "Backspace 应清除原值");
    assert!(app.editing.is_some(), "Backspace 后应立刻进入编辑态");
    assert_eq!(app.editing.as_ref().unwrap().buf, "");
    app.editing = None;

    // AutoSum left-probe: B1=10, C1=20, D1 empty (nothing above) -> =SUM(B1:C1) in edit preview
    app.wb.sheet_mut(0).set(0, 1, Cell { value: Value::Number(10.0), formula: None });
    app.wb.sheet_mut(0).set(0, 2, Cell { value: Value::Number(20.0), formula: None });
    app.sel = (0, 3);
    app.sel_end = (0, 3);
    app.auto_sum();
    let ed = app.editing.as_ref().expect("单格自动求和应先进入公式确认编辑态");
    assert_eq!(ed.buf.to_uppercase(), "=SUM(B1:C1)");
}

#[test]
fn cat1_four_way_smart_fill_and_reapply_fill_mode() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);

    // Downward weekday sequence
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Text("星期一".into()), formula: None });
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.smart_fill_to_mode(3, true, fill_seq::FillMode::Smart, true);
    assert_eq!(app.wb.sheet(0).value(1, 0).display(), "星期二");
    assert_eq!(app.wb.sheet(0).value(2, 0).display(), "星期三");
    assert_eq!(app.wb.sheet(0).value(3, 0).display(), "星期四");

    // Floating fill options switch to CopyCells -> reverts sequence to copies
    app.reapply_last_fill(fill_seq::FillMode::CopyCells);
    assert_eq!(app.wb.sheet(0).value(2, 0).display(), "星期一");
    assert_eq!(app.wb.sheet(0).value(3, 0).display(), "星期一");

    // A lease acquired after the first fill must leave the filled cells intact.
    let now = app.lock_now();
    app.locks.request(0, 0, 0, 93, "协作者", now);
    app.reapply_last_fill(fill_seq::FillMode::Smart);
    assert_eq!(app.wb.sheet(0).value(3, 0).display(), "星期一");
    assert!(app.status.contains("协作者"));

    // Upward fill reverse sequence: row 5="第2组", row 6="第3组" -> fill up to row 3
    app.wb.sheet_mut(0).set(5, 1, Cell { value: Value::Text("第2组".into()), formula: None });
    app.wb.sheet_mut(0).set(6, 1, Cell { value: Value::Text("第3组".into()), formula: None });
    app.sel = (5, 1);
    app.sel_end = (6, 1);
    app.smart_fill_to_mode(4, true, fill_seq::FillMode::Smart, true);
    assert_eq!(app.wb.sheet(0).value(4, 1).display(), "第1组");

    // Rightward & Leftward fill
    app.wb.sheet_mut(0).set(8, 2, Cell { value: Value::Number(10.0), formula: None });
    app.wb.sheet_mut(0).set(8, 3, Cell { value: Value::Number(20.0), formula: None });
    app.sel = (8, 2);
    app.sel_end = (8, 3);
    app.smart_fill_to_mode(4, false, fill_seq::FillMode::Smart, true);
    assert_eq!(app.wb.sheet(0).value(8, 4), Value::Number(30.0));
    app.sel = (8, 2);
    app.sel_end = (8, 3);
    app.smart_fill_to_mode(1, false, fill_seq::FillMode::Smart, true);
    assert_eq!(app.wb.sheet(0).value(8, 1), Value::Number(0.0));
}

#[test]
fn cat1_wheel_zoom_vs_horizontal_scroll() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    let inside = egui::pos2(500.0, 500.0);
    wps_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(inside)], egui::Modifiers::NONE);
    let z0 = app.zoom;
    let s0 = app.scroll;

    // Ctrl+wheel: zoom changes, scroll stays unchanged
    let ctrl_mod = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    wps_frame(
        &mut app,
        &ctx,
        vec![
            egui::Event::PointerMoved(inside),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 120.0),
                modifiers: ctrl_mod,
            },
        ],
        ctrl_mod,
    );
    assert!(app.zoom > z0, "Ctrl+滚轮应放大视图");
    assert_eq!(app.scroll, s0, "Ctrl+滚轮松手前不得连带滚动网格");

    // Shift+wheel: horizontal scroll changes
    let shift_mod = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    wps_frame(
        &mut app,
        &ctx,
        vec![
            egui::Event::PointerMoved(inside),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -40.0),
                modifiers: shift_mod,
            },
        ],
        shift_mod,
    );
    assert!(app.scroll.x > 0.0, "Shift+滚轮应触发横向滚动：scroll={:?}", app.scroll);
}

#[test]
fn cat2_and_cat3_chrome_tokens_ribbon_structure_and_format_dialog() {
    assert_eq!(ui_kit::TITLE_H, 38.0);
    assert_eq!(ui_kit::TAB_H, 30.0);
    assert_eq!(tokens::size::SHEET_TAB_H, 32.0);

    let rib = ribbon::load();
    let home = rib.tabs.iter().find(|t| t.id == "TabHome").expect("TabHome");
    let group_ids: Vec<&str> = home.groups.iter().map(|g| g.id.as_str()).collect();
    assert!(group_ids.contains(&"GroupFreeze"), "开始页应含 GroupFreeze");
    assert!(!group_ids.contains(&"GroupAddIns"), "开始页应已移除 GroupAddIns");

    let mut ids = Vec::new();
    fn walk(nodes: &[ribbon::Node], out: &mut Vec<String>) {
        for n in nodes {
            match n {
                ribbon::Node::Button(c)
                | ribbon::Node::Toggle(c)
                | ribbon::Node::Split { ctl: c, .. }
                | ribbon::Node::DropDown { ctl: c, .. }
                | ribbon::Node::Gallery { ctl: c, .. }
                | ribbon::Node::ComboBox { ctl: c, .. }
                | ribbon::Node::EditBox { ctl: c, .. }
                | ribbon::Node::Slider { ctl: c, .. } => out.push(c.id.clone()),
                ribbon::Node::Menu(m) => out.push(m.ctl.id.clone()),
                ribbon::Node::Box { children, .. } => walk(children, out),
                _ => {}
            }
        }
    }
    for t in &rib.tabs {
        for g in &t.groups {
            walk(&g.children, &mut ids);
        }
    }
    for req in [
        "StrikeThrough",
        "HighlightDuplicates",
        "RejectDuplicates",
        "MergeSameCells",
        "ToggleSpotlight",
        "HomeSplitPanes",
        "ViewSplitPanes",
    ] {
        assert!(ids.iter().any(|id| id == req), "Ribbon 缺少控件 {req}");
    }
    assert!(!ids.iter().any(|id| id == "PhoneticSplit"), "应已移除 PhoneticSplit");

    // Format dialog: [DBNum2], shrink_to_fit, halign=4, preserves untouched border color
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    let mut init_st = sheetcore::style::CellStyle::default();
    init_st.border[2] = sheetcore::style::Border { style: 2, color: [220, 30, 30] };
    let sid = app.wb.styles.intern(init_st);
    app.wb.sheet_mut(0).set_style(0, 0, sid);
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(123.0), formula: None });
    app.sel = (0, 0);
    app.sel_end = (0, 0);

    app.open_format_dialog();
    app.fmt_state.format = "[DBNum2]G/通用格式".into();
    app.fmt_state.shrink_to_fit = true;
    app.fmt_state.halign = 4;
    app.apply_format_dialog();

    let applied = app.wb.styles.get(app.wb.sheet(0).style_of(0, 0));
    assert_eq!(applied.border[2].color, [220, 30, 30], "未改动边框应保留原色与粗细");
    assert!(applied.shrink_to_fit);
    assert_eq!(applied.halign, 4);
    let shown = sheetcore::style::format_value_in(
        app.wb.date_system,
        &Value::Number(123.0),
        &applied.format,
    );
    assert_eq!(shown, "壹佰贰拾叁");
}

#[test]
fn cat4_spotlight_split_duplicates_merge_same_datetime_and_paste_reapply() {
    let _env = crate::test_env::TestEnv::new("wps_cat4");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.prefs.spotlight = false;
    app.prefs.save();

    // Spotlight toggles & persists into UiPrefs
    app.toggle_spotlight();
    assert!(app.prefs.spotlight);
    assert!(ui_prefs::UiPrefs::load().spotlight, "聚光灯状态应持久化到 ui.ini");
    app.toggle_spotlight();
    assert!(!app.prefs.spotlight);

    // Split Panes <-> Freeze Panes mutual exclusion
    app.sel = (3, 2);
    app.sel_end = (3, 2);
    app.toggle_split_panes();
    assert_eq!(app.split_panes, Some((3, 2)));
    assert_eq!(app.wb.sheet(0).freeze, None);

    app.ribbon_action("FreezePanes", &ctx);
    assert_eq!(app.split_panes, None, "冻结窗格应互斥关闭拆分窗格");
    assert_eq!(app.wb.sheet(0).freeze, Some((3, 2)));

    app.toggle_split_panes();
    assert_eq!(app.split_panes, Some((3, 2)));
    assert_eq!(app.wb.sheet(0).freeze, None, "拆分窗格应互斥取消冻结");
    app.toggle_split_panes();

    // Duplicates highlight & reject + MergeSameCells
    for (r, v) in ["甲", "甲", "乙", "甲"].iter().enumerate() {
        app.wb.sheet_mut(0).set(r as u32, 0, Cell { value: Value::Text((*v).into()), formula: None });
    }
    app.sel = (0, 0);
    app.sel_end = (3, 0);
    app.highlight_duplicates();
    assert!(!app.wb.sheet(0).cf.blocks.is_empty(), "应添加重复值条件格式规则");
    app.reject_duplicates();
    assert!(
        app.wb.sheet(0).dv.iter().any(|d| d.f1.contains("COUNTIF")),
        "拒绝录入重复项应写入 COUNTIF 自定义数据验证"
    );

    // Merge consecutive identical rows: A1:A2 ("甲") -> merge (0,0,1,0)
    app.merge_same_cells();
    assert!(
        app.wb.sheet(0).merges.contains(&(0, 0, 1, 0)),
        "相同连续值 A1:A2 应合并：{:?}",
        app.wb.sheet(0).merges
    );

    // Insert current date (Ctrl+;) & time (Ctrl+Shift+;)
    app.sel = (5, 0);
    app.sel_end = (5, 0);
    app.insert_current_datetime(false);
    assert_eq!(app.wb.styles.get(app.wb.sheet(0).style_of(5, 0)).format, "yyyy-mm-dd");
    app.sel = (5, 1);
    app.sel_end = (5, 1);
    app.insert_current_datetime(true);
    assert_eq!(app.wb.styles.get(app.wb.sheet(0).style_of(5, 1)).format, "hh:mm");

    // Post-paste floating options switch (values vs formulas)
    app.wb.sheet_mut(0).set(
        6,
        0,
        Cell {
            value: Value::Number(42.0),
            formula: Some("40+2".into()),
        },
    );
    app.sel = (6, 0);
    app.sel_end = (6, 0);
    app.clip_copy(false, &ctx);
    app.clip_stamp = None;
    app.sel = (7, 0);
    app.sel_end = (7, 0);
    app.clip_paste(false, None);
    assert_eq!(
        app.wb.sheet(0).formula_of(7, 0),
        Some("40+2")
    );
    app.reapply_last_paste(1);
    let pasted_val_only = app.wb.sheet(0).get(7, 0).map(|c| c.into_owned()).unwrap();
    assert!(pasted_val_only.formula.is_none(), "切换为仅值应剥离公式");
    assert_eq!(pasted_val_only.value, Value::Number(42.0));
    app.reapply_last_paste(2);
    assert_eq!(
        app.wb.sheet(0).formula_of(7, 0),
        Some("40+2"),
        "切回仅公式应恢复公式"
    );
}

#[test]
fn terminology_has_no_colleague_or_slang_in_ribbon() {
    let rib = ribbon::load();
    fn check_ctl(c: &ribbon::Ctl) {
        for field in [c.label.as_deref(), c.screentip.as_deref(), c.supertip.as_deref()]
            .into_iter()
            .flatten()
        {
            assert!(
                !field.contains("同事") && !field.contains("开房"),
                "功能区控件 {} 文案包含禁用词：{field}",
                c.id
            );
        }
    }
    fn walk_menu_nodes(nodes: &[ribbon::MenuNode]) {
        for n in nodes {
            match n {
                ribbon::MenuNode::Button(c) | ribbon::MenuNode::Toggle(c) => check_ctl(c),
                ribbon::MenuNode::Menu(m) => {
                    check_ctl(&m.ctl);
                    walk_menu_nodes(&m.items);
                }
                ribbon::MenuNode::Separator => {}
            }
        }
    }
    fn walk_nodes(nodes: &[ribbon::Node]) {
        for n in nodes {
            match n {
                ribbon::Node::Button(c)
                | ribbon::Node::Toggle(c)
                | ribbon::Node::Split { ctl: c, .. }
                | ribbon::Node::DropDown { ctl: c, .. }
                | ribbon::Node::Gallery { ctl: c, .. }
                | ribbon::Node::ComboBox { ctl: c, .. }
                | ribbon::Node::EditBox { ctl: c, .. }
                | ribbon::Node::Slider { ctl: c, .. } => {
                    check_ctl(c);
                    if let ribbon::Node::Split { menu, .. } = n {
                        walk_menu_nodes(&menu.items);
                    }
                }
                ribbon::Node::Menu(m) => {
                    check_ctl(&m.ctl);
                    walk_menu_nodes(&m.items);
                }
                ribbon::Node::Box { children, .. } => walk_nodes(children),
                _ => {}
            }
        }
    }
    for t in &rib.tabs {
        assert!(!t.label.contains("同事") && !t.label.contains("开房"));
        for g in &t.groups {
            assert!(!g.label.contains("同事") && !g.label.contains("开房"));
            walk_nodes(&g.children);
        }
    }
}

#[test]
fn ai_config_defaults_and_roundtrip() {
    let def = ai_agent::AiConfig::default();
    assert_eq!(def.provider_id, "deepseek-local");
    assert_eq!(def.display_name, "DeepSeek Local H200");
    assert_eq!(def.base_url, "https://deepseek.hollantex.net/v1");
    assert_eq!(def.model, "deepseek-v4.1-flash");
    assert!(def.native_tools);
    assert!(!def.allow_unframed_text);
    assert!(def.api_key.is_empty());

    let dir = crate::test_env::scratch("ai-cfg-test");
    let builtin_file = dir.join("builtin_ai.ini");
    std::fs::write(&builtin_file, "api_key=@builtin\n").unwrap();
    assert_eq!(ai_agent::AiConfig::load_from(&builtin_file), def);

    let file = dir.join("ai.ini");
    let custom = ai_agent::AiConfig {
        provider_id: "custom".into(),
        display_name: "测试节点".into(),
        base_url: "http://127.0.0.1:8080/v1".into(),
        api_key: "secret-key".into(),
        model: "deepseek-v4".into(),
        native_tools: false,
        allow_unframed_text: true,
    };
    custom.save_to(&file);
    let loaded = ai_agent::AiConfig::load_from(&file);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(loaded, custom);
}

#[test]
fn ai_micro_harness_executes_multi_turn_sse_tool_loop_and_supports_undo() {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let srv = std::thread::spawn(move || {
        for round_idx in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            let mut header_end = None;
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                if header_end.is_none() {
                    if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                    }
                }
                if let Some(hend) = header_end {
                    let hdrs = String::from_utf8_lossy(&raw[..hend]);
                    let clen = hdrs
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            if k.trim().eq_ignore_ascii_case("Content-Length") {
                                v.trim().parse::<usize>().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    if raw.len() >= hend + clen {
                        break;
                    }
                }
            }
            let body_txt = header_end
                .map(|hend| String::from_utf8_lossy(&raw[hend..]).to_string())
                .unwrap_or_default();
            if round_idx == 1 {
                let body: serde_json::Value = serde_json::from_str(&body_txt).unwrap();
                let messages = body["messages"].as_array().unwrap();
                let observation = messages.last().unwrap();
                let call = &messages[messages.len() - 2]["tool_calls"][0];
                assert_eq!(observation["role"], "tool");
                assert_eq!(call["function"]["name"], "fill_formula_range");
                assert_eq!(observation["tool_call_id"], call["id"]);
                assert!(body["tools"].as_array().is_some_and(|tools| !tools.is_empty()));
                assert!(body_txt.contains("120"), "工具返回应包含 C3 重算结果 120：{body_txt}");
            }
            let sse_payload = if round_idx == 0 {
                let c1 = serde_json::json!({"choices":[{"delta":{"reasoning_content":"先计算C列乘积","content":""}}]}).to_string();
                let c2 = serde_json::json!({"choices":[{"delta":{"content":"<tool_call>{\"name\":\"fill_formula_range\",\"arguments\":{\"range\":\"C1:C3\",\"top_left_formula\":\"=A1*B1\"}}</tool_call>"}}]}).to_string();
                format!("data: {c1}\n\ndata: {c2}\n\ndata: [DONE]\n\n")
            } else {
                let c1 = serde_json::json!({"choices":[{"delta":{"content":"已完成 C1:C3 公式填充，计算结果分别为 20、60、120。"}}]}).to_string();
                format!("data: {c1}\n\ndata: [DONE]\n\n")
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                sse_payload.len(),
                sse_payload
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Both);
        }
    });

    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    for (r, a, b) in [(0u32, 10.0, 2.0), (1, 20.0, 3.0), (2, 30.0, 4.0)] {
        app.wb.sheet_mut(0).set(r, 0, Cell { value: Value::Number(a), formula: None });
        app.wb.sheet_mut(0).set(r, 1, Cell { value: Value::Number(b), formula: None });
    }
    app.ai.cfg.base_url = format!("http://127.0.0.1:{port}/v1");
    app.ai.cfg.api_key = "test-key".into();
    app.start_ai_prompt("把 C1:C3 设为 A*B".into());

    let start = std::time::Instant::now();
    while app.ai.busy && start.elapsed() < std::time::Duration::from_secs(6) {
        app.poll_ai(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    assert!(!app.ai.busy, "AI 多轮闭环未在 6 秒内结束：status={}", app.ai.status_hint);
    let last = app.ai.messages.last().expect("assistant msg");
    assert_eq!(last.error, None, "不应有错误：{:?}", last.error);
    assert!(last.reasoning.contains("先计算C列乘积"), "应保留思考链：{}", last.reasoning);
    assert!(last.content.contains("已完成 C1:C3 公式填充"), "应返回汇总回复：{}", last.content);
    assert_eq!(last.undo_ids.len(), 1);

    assert_eq!(app.wb.sheet(0).value(0, 2), Value::Number(20.0));
    assert_eq!(app.wb.sheet(0).value(1, 2), Value::Number(60.0));
    assert_eq!(app.wb.sheet(0).value(2, 2), Value::Number(120.0));
    assert_eq!(app.wb.sheet(0).formula_of(1, 2), Some("A2*B2"));

    app.undo_last_ai_change();
    assert_eq!(app.wb.sheet(0).value(0, 2), Value::Empty, "一键撤销应恢复 C1");
    assert_eq!(app.wb.sheet(0).value(2, 2), Value::Empty, "一键撤销应恢复 C3");

    let _ = srv.join();
}

#[test]
fn menu_enrichment_paste_suite() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.add_sheet();
    app.switch_sheet_view(0);

    // 准备源格 A1: 数值 1234.5，带公式 =1000+234.5，货币格式 + 蓝色底纹 + 红色字体 + 四周边框，列宽 118
    let src_sid = app.wb.styles.intern(sheetcore::style::CellStyle {
        format: "¥#,##0.00".into(),
        fill: Some([220, 235, 250]),
        color: [200, 30, 30],
        border: [sheetcore::style::Border { style: 1, color: [0, 0, 0] }; 4],
        ..Default::default()
    }) as u16;
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Number(1234.5), formula: Some("1000+234.5".into()) });
        sh.set_style(0, 0, src_sid);
        sh.cols.set(0, 118.0);
    }

    // 目标 B1 预先设定灰色填充（用于验证 ValuesNumFmt 只复制数字格式而保留目标填充）
    let dst_sid = app.wb.styles.intern(sheetcore::style::CellStyle {
        fill: Some([240, 240, 240]),
        ..Default::default()
    }) as u16;
    app.wb.sheet_mut(0).set_style(0, 1, dst_sid);

    // 复制 A1
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.clip = Some(Clip { deferred: None,
        source_rows: None,
        source_cols: None,
        sheet: 0,
        rect: (0, 0, 0, 0),
        cells: vec![vec![(app.wb.sheet(0).get(0, 0).unwrap().into_owned(), src_sid)]],
        merges: Vec::new(),
        cut: false,
    });
    app.clip_stamp = None;

    // 1. 值和数字格式 -> B1
    app.sel = (0, 1);
    app.sel_end = (0, 1);
    app.ribbon_action("PasteValuesNumFmt", &ctx);
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(1234.5));
    assert_eq!(app.wb.sheet(0).formula_of(0, 1), None);
    let st_b1 = app.wb.styles.get(app.wb.sheet(0).style_of(0, 1));
    assert_eq!(st_b1.format, "¥#,##0.00");
    assert_eq!(st_b1.fill, Some([240, 240, 240]), "ValuesNumFmt 应保留目标原有底纹");

    // 2. 值和源格式 -> C1
    app.sel = (0, 2);
    app.sel_end = (0, 2);
    app.ribbon_action("PasteValuesSourceFmt", &ctx);
    assert_eq!(app.wb.sheet(0).value(0, 2), Value::Number(1234.5));
    assert_eq!(app.wb.sheet(0).formula_of(0, 2), None);
    let st_c1 = app.wb.styles.get(app.wb.sheet(0).style_of(0, 2));
    assert_eq!(st_c1.fill, Some([220, 235, 250]));
    assert_eq!(st_c1.color, [200, 30, 30]);

    // 3. 无边框粘贴 -> D1
    app.sel = (0, 3);
    app.sel_end = (0, 3);
    app.ribbon_action("PasteNoBorders", &ctx);
    let st_d1 = app.wb.styles.get(app.wb.sheet(0).style_of(0, 3));
    assert_eq!(st_d1.fill, Some([220, 235, 250]));
    assert!(st_d1.border.iter().all(|b| b.is_none()), "无边框粘贴必须清除边框");

    // 4. 保留源列宽 -> E1
    app.sel = (0, 4);
    app.sel_end = (0, 4);
    app.ribbon_action("PasteColWidths", &ctx);
    assert!((app.wb.sheet(0).cols.size(4) - 118.0).abs() < 0.1, "应复制源列宽 118px");

    // 5. 公式和数字格式 -> F1
    app.sel = (0, 5);
    app.sel_end = (0, 5);
    app.ribbon_action("PasteFormulasNumFmt", &ctx);
    assert_eq!(app.wb.sheet(0).formula_of(0, 5), Some("1000+234.5"));
    assert_eq!(app.wb.styles.get(app.wb.sheet(0).style_of(0, 5)).format, "¥#,##0.00");

    // 6. 粘贴链接（同表 & 跨表）
    app.sel = (1, 0);
    app.sel_end = (1, 0);
    app.ribbon_action("PasteLink", &ctx);
    assert_eq!(app.wb.sheet(0).formula_of(1, 0), Some("=A1"));
    app.switch_sheet_view(1);
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.ribbon_action("PasteLink", &ctx);
    let cross_f = app.wb.sheet(1).formula_of(0, 0).unwrap_or("");
    assert!(cross_f.ends_with("!A1"), "跨表粘贴链接应带工作表名：{cross_f}");

    // 7. 跳过空单元格（ps_skip_blanks）
    app.switch_sheet_view(0);
    app.wb.sheet_mut(0).set(2, 0, Cell { value: Value::Text("保留原字".into()), formula: None });
    app.clip = Some(Clip { deferred: None,
        source_rows: None,
        source_cols: None,
        sheet: 0,
        rect: (0, 0, 0, 0),
        cells: vec![vec![(Cell::default(), 0)]],
        merges: Vec::new(),
        cut: false,
    });
    app.clip_stamp = None;
    app.ps_kind = 0;
    app.ps_transpose = false;
    app.ps_op = 0;
    app.ps_skip_blanks = true;
    app.sel = (2, 0);
    app.paste_special_apply(&ctx);
    assert_eq!(app.wb.sheet(0).value(2, 0), Value::Text("保留原字".into()), "开启跳过空单元格时不得覆盖目标值");
    app.ps_skip_blanks = false;

    // 8. 复制为图片 & 粘贴为图片
    app.sel = (0, 0);
    app.sel_end = (0, 2);
    app.ribbon_action("CopyAsPicture", &ctx);
    let (pw, ph, ref png_bytes) = app.clip_pic.clone().expect("复制为图片应生成 PNG 缓存");
    assert!(pw > 40 && ph > 20, "图片尺寸异常：{pw}x{ph}");
    let decoded = image::load_from_memory(png_bytes).expect("生成的 PNG 必须可被标准图像库解码");
    assert_eq!(decoded.width(), pw);
    assert_eq!(decoded.height(), ph);

    app.sel = (4, 0);
    app.sel_end = (4, 0);
    app.ribbon_action("PasteAsPicture", &ctx);
    assert_eq!(app.wb.sheet(0).pictures.len(), 1, "粘贴为图片应写入工作表 pictures");
    app.do_undo(false);
    assert_eq!(app.wb.sheet(0).pictures.len(), 0, "撤销应移除刚粘贴的图片");
}

#[test]
fn menu_enrichment_fill_goto_cf_table_and_styles() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);

    // 1. FillUp & FillLeft
    app.wb.sheet_mut(0).set(2, 0, Cell { value: Value::Number(99.0), formula: None });
    app.sel = (0, 0);
    app.sel_end = (2, 0);
    app.ribbon_action("FillUp", &ctx);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(99.0));

    app.wb.sheet_mut(0).set(0, 3, Cell { value: Value::Number(77.0), formula: None });
    app.sel = (0, 0);
    app.sel_end = (0, 3);
    app.ribbon_action("FillLeft", &ctx);
    assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(77.0));

    // 2. 快速序列填充 (Linear & Weekday)
    app.wb.sheet_mut(0).set(3, 0, Cell { value: Value::Number(10.0), formula: None });
    app.sel = (3, 0);
    app.sel_end = (6, 0);
    app.ribbon_action("FillSeqLinear", &ctx);
    assert_eq!(app.wb.sheet(0).value(4, 0), Value::Number(11.0));
    assert_eq!(app.wb.sheet(0).value(6, 0), Value::Number(13.0));

    // 3. 定位条件 (Blanks / Formulas / Errors / Comments)
    app.wb.sheet_mut(0).set(8, 0, Cell { value: Value::Number(10.0), formula: None });
    app.wb.sheet_mut(0).set(8, 1, Cell::default()); // 空值
    app.wb.sheet_mut(0).set(8, 2, Cell { value: Value::Number(20.0), formula: Some("=A9*2".into()) });
    app.wb.sheet_mut(0).set(8, 3, Cell { value: Value::Error(sheetcore::formula::ErrKind::Div0), formula: None });
    app.wb.sheet_mut(0).notes.insert(
        sheetcore::model::CellRef::new(8, 0),
        sheetcore::notes::Note { r: 8, c: 0, author: "u".into(), text: "备注".into() },
    );

    app.sel = (8, 0);
    app.sel_end = (8, 3);
    app.ribbon_action("SelectFormulas", &ctx);
    assert_eq!(app.sel, (8, 2));

    app.sel = (8, 0);
    app.sel_end = (8, 3);
    app.ribbon_action("SelectErrors", &ctx);
    assert_eq!(app.sel, (8, 3));

    app.sel = (8, 0);
    app.sel_end = (8, 3);
    app.ribbon_action("SelectBlanks", &ctx);
    assert_eq!(app.sel, (8, 1));

    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.ribbon_action("SelectComments", &ctx);
    assert_eq!(app.sel, (8, 0));

    // 4. 条件格式三色阶阈值（最高点必须是 Max，不能错写成 Min）
    app.sel = (3, 0);
    app.sel_end = (6, 0);
    app.ribbon_action("CfScaleGYR", &ctx);
    let blk = app.wb.sheet(0).cf.blocks.last().expect("应生成色阶规则");
    if let sheetcore::cf::CfKind::Scale { points } = &blk.rules[0].kind {
        assert_eq!(points[0].0.kind, sheetcore::cf::CfvoKind::Min);
        assert_eq!(points[2].0.kind, sheetcore::cf::CfvoKind::Max);
    } else {
        panic!("不是色阶规则");
    }

    // 5. 套用表格主题 + 清除批注/超链接 + 单元格样式预设
    app.wb.sheet_mut(0).set(10, 0, Cell { value: Value::Text("列A".into()), formula: None });
    app.wb.sheet_mut(0).set(10, 1, Cell { value: Value::Text("列B".into()), formula: None });
    app.wb.sheet_mut(0).set(11, 0, Cell { value: Value::Text("甲".into()), formula: None });
    app.wb.sheet_mut(0).set(11, 1, Cell { value: Value::Number(42.0), formula: None });
    app.sel = (10, 0);
    app.sel_end = (11, 1);
    app.ribbon_action("TableThemeGreen", &ctx);
    assert_eq!(app.wb.sheet(0).tables.len(), 1);
    assert_eq!(app.wb.styles.get(app.wb.sheet(0).style_of(10, 0)).fill, Some([33, 115, 70]));

    app.sel = (8, 0);
    app.sel_end = (8, 0);
    app.ribbon_action("ClearComments", &ctx);
    assert!(app.wb.sheet(0).notes.is_empty(), "清除批注应移除选中单元格批注");

    app.ribbon_action("StyleInput", &ctx);
    assert_eq!(app.wb.styles.get(app.wb.sheet(0).style_of(8, 0)).fill, Some([255, 204, 153]));
    assert!(app.unhandled_action.is_none(), "不应存在未处理动作");
}

#[test]
fn ai_requires_own_key_and_secure_remote_endpoint_before_sending() {
    let env = crate::test_env::TestEnv::new("ai_key_consent");

    // 旧版本写下的「@builtin」占位符不再换成任何内置密钥
    let ini = env.path("cfg").join("ai.ini");
    std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
    std::fs::write(&ini, "base_url=https://example.invalid/v1\napi_key=@builtin\n").unwrap();
    let legacy = ai_agent::AiConfig::load_from(&ini);
    assert!(!legacy.has_api_key(), "旧占位符必须读成「未填写」");

    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.ai.open = true;

    // 没有密钥：不发请求，打开设置
    app.ai.cfg = ai_agent::AiConfig::default();
    app.ai.cfg.api_key.clear();
    app.start_ai_prompt("hi".into());
    assert!(!app.ai.busy && app.ai.settings_open && app.ai.messages.is_empty());

    // 非本机地址的明文 http 必须被拒绝（密钥与表格内容不能裸传）
    app.ai.settings_open = false;
    app.ai.cfg.api_key = "test-key".into();
    app.ai.cfg.base_url = "http://10.0.0.5:8080/v1".into();
    app.start_ai_prompt("hi".into());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.ai.busy && std::time::Instant::now() < deadline {
        app.poll_ai(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!app.ai.busy);
    let err = app.ai.messages.last().and_then(|m| m.error.clone()).unwrap_or_default();
    assert!(err.contains("https"), "应提示必须用 https：{err:?}");
}

#[test]
fn ai_aborts_tool_calls_if_user_switches_tab_mid_round() {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let (accepted, request_arrived) = std::sync::mpsc::channel();
    let srv = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        accepted.send(()).unwrap();
        let sse = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            serde_json::json!({
                "choices": [{
                    "delta": {
                        "content": "<tool_call>{\"name\":\"set_cells\",\"arguments\":{\"edits\":[{\"cell\":\"A1\",\"value\":999}]}}</tool_call>"
                    }
                }]
            })
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            sse.len(),
            sse
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Both);
    });

    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.ai.cfg.base_url = format!("http://127.0.0.1:{port}/v1");
    app.ai.cfg.api_key = "test".into();
    app.start_ai_prompt("把 A1 改成 999".into());

    // 在模型推理尚未返回工具执行阶段前切换到新标签页
    request_arrived.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    app.open_in_new_tab(PathBuf::new());

    let start = std::time::Instant::now();
    while app.ai.busy && start.elapsed() < std::time::Duration::from_secs(6) {
        app.poll_ai(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    assert!(!app.ai.busy, "切换标签页后 AI 回合必须及时终止");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Empty, "新切入的标签页不得被误改");
    let last = app.ai.messages.last().expect("assistant message");
    assert!(
        last.error.as_deref().is_some_and(|e| e.contains("已切换")),
        "应提示工作簿已切换并中止写入：{:?}",
        last.error
    );
    let _ = srv.join();
}

#[test]
fn ai_composer_ime_commit_enter_does_not_prematurely_send() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.ai.open = true;
    app.ai.cfg.base_url = "http://127.0.0.1:9/v1".into();
    app.ai.cfg.api_key = "test-key".into();
    app.ai.draft = "请帮我用 ".into();

    // 第 0 帧渲染出输入框并赋予焦点
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    ctx.memory_mut(|m| m.request_focus(egui::Id::new("wps_ai_draft_input")));

    // 第 1 帧：中文输入法组字态 Preedit
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Ime(egui::ImeEvent::Preedit("vlookup".into()))],
        egui::Modifiers::NONE,
    );

    // 第 2 帧：输入法按 Enter 确认上屏（Commit + Enter），不得误发送请求
    wps_frame(
        &mut app,
        &ctx,
        vec![
            egui::Event::Ime(egui::ImeEvent::Commit("vlookup".into())),
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        egui::Modifiers::NONE,
    );
    assert!(!app.ai.busy);
    assert!(app.ai.messages.is_empty(), "IME 上屏时的 Enter 不得触发发送");
    assert!(app.ai.draft.contains("vlookup"), "上屏文本应留在输入框：{}", app.ai.draft);

    // 第 3 帧：非输入法状态直接按 Enter，正常发送
    ctx.memory_mut(|m| m.request_focus(egui::Id::new("wps_ai_draft_input")));
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.ai.messages.len(), 2, "普通 Enter 应正常提交消息");
    assert!(app.ai.draft.is_empty());
    app.stop_ai_generation();
}

#[test]
fn ai_composer_retains_focus_and_types_even_when_cell_was_in_edit_mode() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.begin_edit(Some("123".into()));

    // 第 1 帧：单元格格内编辑器 grid_edit 持有焦点
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    assert!(
        ctx.memory(|m| m.has_focus(egui::Id::new("grid_edit"))),
        "进入格内编辑后 grid_edit 应获得焦点"
    );

    // 1) 通过入口打开 AI 助手：应自动提交单元格并把焦点安全交给 AI 输入框
    app.toggle_ai_assistant(Some(true));
    assert!(app.editing.is_none(), "打开 AI 助手应提交当前单元格草稿");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0));

    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    let ai_id = egui::Id::new("wps_ai_draft_input");
    assert!(
        ctx.memory(|m| m.has_focus(ai_id)),
        "打开 AI 助手后输入框应获得焦点"
    );

    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Text("你好".into())],
        egui::Modifiers::NONE,
    );
    assert!(ctx.memory(|m| m.has_focus(ai_id)), "打字第 1 帧后不得失焦");
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Text("AI".into())],
        egui::Modifiers::NONE,
    );
    assert!(ctx.memory(|m| m.has_focus(ai_id)), "打字第 2 帧后仍不得失焦");
    assert_eq!(app.ai.draft, "你好AI");
    assert!(app.editing.is_none(), "向 AI 助手打字不得误触表格输入");
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(123.0));

    // 2) AI 面板已打开时先双击编辑 A2，再点击 AI 面板区域：不得被 grid_edit 抢回焦点
    app.sel = (1, 0);
    app.sel_end = (1, 0);
    app.begin_edit(Some("456".into()));
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    assert!(ctx.memory(|m| m.has_focus(egui::Id::new("grid_edit"))));

    let p = egui::pos2(1260.0, 260.0);
    wps_frame(
        &mut app,
        &ctx,
        vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        egui::Modifiers::NONE,
    );
    assert!(app.editing.is_none(), "点击 AI 面板时应完成单元格编辑");
    assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(456.0));
}

#[test]
fn home_page_clean_minimal_layout_without_subtitle_sidecards_or_preset_templates() {
    let _env = crate::test_env::TestEnv::new("home-page-layout");
    fn collect_texts(shapes: &[egui::epaint::Shape], out: &mut Vec<String>) {
        for s in shapes {
            match s {
                egui::epaint::Shape::Text(ts) => out.push(ts.galley.job.text.clone()),
                egui::epaint::Shape::Vec(inner) => collect_texts(inner, out),
                _ => {}
            }
        }
    }

    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.show_start = true;

    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1440.0, 900.0),
        )),
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    let out = ctx.run(raw, |ctx| eframe::App::update(&mut app, ctx, &mut frame));

    let mut texts = Vec::new();
    for cs in &out.shapes {
        collect_texts(std::slice::from_ref(&cs.shape), &mut texts);
    }
    let joined = texts.join("\n");

    assert!(joined.contains("开始使用"), "主页应保留『开始使用』标题");
    assert!(joined.contains("空白工作簿"), "主页应保留『空白工作簿』入口卡片");
    assert!(joined.contains("打开本地文件"), "主页应包含『打开本地文件』快捷卡片");
    assert!(joined.contains("最近打开"), "主页应保留『最近打开』列表卡片");

    for removed in [
        "从商用级财务与项目管理模板快速创建",
        "从干净的工作表开始",
        "支持直接拖入本地或共享盘",
        "Local-first",
        "工作区",
        "点击新建空白工作簿",
        "本地优先 · 无锁异步协同",
        "常用快捷键",
        "销售回款台账",
        "部门经营预算",
        "项目里程碑排期",
        "精选模板",
    ] {
        assert!(
            !joined.contains(removed),
            "主页不应再渲染已删除文案 `{removed}`，实际文本：\n{joined}"
        );
    }

    // 验证超过 5 个最近打开文件时不会被硬截断为 5 行
    app.recent_cache = Some(
        (1..=8)
            .map(|k| HomeRecentEntry {
                path: PathBuf::from(format!("F:\\docs\\report_{k}.xlsx")),
                lower_haystack: format!("f:\\docs\\report_{k}.xlsx"),
                name: format!("report_{k}.xlsx"),
                short_dir: "F:\\docs".to_string(),
                size_txt: "12.0 KB".to_string(),
                mtime_txt: "1 小时前".to_string(),
                pinned: false,
            })
            .collect(),
    );
    let raw2 = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1440.0, 900.0),
        )),
        ..Default::default()
    };
    let out2 = ctx.run(raw2, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    let mut texts2 = Vec::new();
    for cs in &out2.shapes {
        collect_texts(std::slice::from_ref(&cs.shape), &mut texts2);
    }
    let joined2 = texts2.join("\n");
    assert!(
        joined2.contains("report_8.xlsx"),
        "900p 高度下最近打开卡片应自适应铺满并显示第 8 条记录，实际：\n{joined2}"
    );
    // 主页保持极简（只展示『空白工作簿』+『打开本地文件』），后台「新建」内置 1 个空白 + 6 套财务模板
    assert_eq!(templates::ALL.len(), 7);
    assert_eq!(templates::ALL[0].name, "空白工作簿");
}

#[test]
fn compact_home_keeps_recent_file_visible_and_fits_long_text_in_its_column() {
    let _env = crate::test_env::TestEnv::new("compact-home");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.show_start = true;
    app.prefs.theme = theme::AppTheme::Dark as u8;
    let name = format!("{}.xlsx", "跨部门年度财务计划".repeat(8));
    let path = PathBuf::from(r"F:\一组名称很长的财务目录\新年度预算执行情况跟踪").join(&name);
    app.recent_cache = Some(vec![HomeRecentEntry {
        path, name: name.clone(), lower_haystack: name.to_lowercase(),
        short_dir: String::new(), size_txt: "共享盘".into(), mtime_txt: String::new(), pinned: false,
    }]);
    let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 420.0));
    let raw = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
    let mut frame = eframe::Frame::_new_kittest();
    let output = ctx.run(raw, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    let drawn_text = |label: &str| {
        output.shapes.iter().find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(t) if t.galley.job.text == label => Some((clipped.clip_rect, t)),
            _ => None,
        }).unwrap_or_else(|| panic!("缺少文字 {label}"))
    };
    let (clip, filename) = drawn_text(&name);
    assert!(filename.galley.elided, "超长文件名应以省略号结尾");
    assert!(screen.contains_rect(filename.visual_bounding_rect()), "窄窗口也必须显示最近文件的第一行");
    assert!(clip.right() <= drawn_text("所在位置").1.pos.x + 1.0, "文件名不能占用目录列");
    assert!(drawn_text("局域网协作").1.pos.y < filename.pos.y);
    assert_eq!(filename.galley.job.sections[0].format.color, theme::palette_for(theme::AppTheme::Dark).text);
}

#[test]
fn workbook_chrome_exposes_formula_controls_and_the_active_view_at_high_dpi() {
    use egui::accesskit::{Role, Toggled};
    let _env = crate::test_env::TestEnv::new("workbook-chrome");
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut app = wps_app(&ctx);
    let mut frame = eframe::Frame::_new_kittest();
    let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(640.0, 400.0));
    let mut render = || {
        let mut input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(1.5);
        ctx.run(input, |ctx| eframe::App::update(&mut app, ctx, &mut frame))
    };
    render();
    let output = render();
    for label in ["名称列表", "插入函数", "展开编辑栏", "切换主题"] {
        assert!(screen.contains_rect(dialog_control(&output, Role::Button, label)), "{label}");
    }
    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
    let view = |label| tree.nodes.iter().map(|(_, n)| n)
        .find(|n| n.role() == Role::Button && n.label() == Some(label)).unwrap();
    assert_eq!(view("普通").toggled(), Some(Toggled::True));
    assert_eq!(view("分页预览").toggled(), Some(Toggled::False));
}

#[test]
fn narrow_status_message_fits_between_mode_and_selection_statistics() {
    let _env = crate::test_env::TestEnv::new("narrow-status");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(123.0), formula: None });
    app.status = "已打开并重新计算完成，工作簿包含多张表格和多个非空单元格".into();
    let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 400.0));
    let mut frame = eframe::Frame::_new_kittest();
    let mut render = || ctx.run(egui::RawInput { screen_rect: Some(screen), ..Default::default() },
        |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    render();
    let output = render();
    let visible_bounds = |predicate: &dyn Fn(&str) -> bool| {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if predicate(&text.galley.job.text) => {
                Some(text.visual_bounding_rect().intersect(shape.clip_rect))
            }
            _ => None,
        }).unwrap_or_else(|| panic!("状态栏文字应可见：{:?}", output.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.pos.y > screen.bottom() - 26.0 => Some((&t.galley.job.text, t.pos, s.clip_rect)),
            _ => None
        }).collect::<Vec<_>>()))
    };
    let mode = visible_bounds(&|s| s == "就绪");
    let message = visible_bounds(&|s| s == app.status);
    let stats = visible_bounds(&|s| s.contains("计数"));
    assert!(message.is_positive() && stats.is_positive());
    assert!(mode.right() <= message.left() && message.right() <= stats.left());
}

#[test]
fn right_click_insert_comment_no_click_through_autofocus_and_callout_visible() {
    fn collect_texts(shapes: &[egui::epaint::Shape], out: &mut Vec<String>) {
        for sh in shapes {
            match sh {
                egui::epaint::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::epaint::Shape::Vec(v) => collect_texts(v, out),
                _ => {}
            }
        }
    }

    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    let mut frame = eframe::Frame::_new_kittest();

    // 先跑一帧完成常规版面测量
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);

    // 计算 B2 (1, 1) 在屏幕上的真实像素中心（标题栏 38 + 功能区 147 + 编辑栏 40 + 列头 19 ≈ 244 起）
    let b2_pos = egui::pos2(8.0 + crate::HEAD_W + 72.0 + 30.0, 38.0 + 147.0 + 40.0 + 19.0 + 19.0 + 9.0);

    // 先悬停到 B2 再右键单击打开单元格菜单（与真实鼠标事件序列一致）
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(b2_pos)],
        egui::Modifiers::NONE,
    );
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::PointerButton {
            pos: b2_pos,
            button: egui::PointerButton::Secondary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    );
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::PointerButton {
            pos: b2_pos,
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.sel, (1, 1), "右键 B2 应选中 B2");
    assert!(app.ctx_menu.is_some(), "右键后菜单应打开");

    // 跑一帧让 ctx_menu_window 画出菜单并登记 ctx_rects 与各菜单行响应区域
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    let menu_rect = app.ctx_rects.first().copied().expect("右键菜单矩形必须已登记");

    // 定位菜单中「插入/编辑批注」行的真实屏幕中心点（该位置位于菜单下半部，正压在底层第 18~20 行网格之上）
    let rows = app.cell_ctx_menu_rows();
    let comment_idx = rows
        .iter()
        .position(|r| r.label == "插入/编辑批注")
        .expect("必须存在插入/编辑批注菜单项");
    let row_y = crate::ctx_row_top(&rows, comment_idx, menu_rect.top() + 4.0) + crate::CTX_ROW_H * 0.5;
    let click_comment_pos = egui::pos2(menu_rect.center().x, row_y);

    // 模拟真实鼠标移入菜单项 -> 按下左键 -> 松开左键：验证底层网格绝不穿透抢走选区！
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(click_comment_pos)],
        egui::Modifiers::NONE,
    );
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::PointerButton {
            pos: click_comment_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        app.sel,
        (1, 1),
        "在右键菜单上按下左键绝不能穿透到底层单元格篡改选区"
    );
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::PointerButton {
            pos: click_comment_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.sel, (1, 1), "点击菜单项松开后选区仍为 B2");
    assert_eq!(
        app.note_edit.as_ref().map(|(r, c, _)| (*r, *c)),
        Some((1, 1)),
        "批注对话框必须精确绑定到右键的 B2 单元格"
    );

    // 渲染一帧使批注对话框挂载并自动获得输入焦点
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    assert!(app.editing.is_none(), "弹窗打开期间不应触发底层单元格编辑");

    // 直接打字（不额外用鼠标点输入框）并按 Ctrl+Enter 提交
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Text("核对本笔往来款".into())],
        egui::Modifiers::NONE,
    );
    assert!(
        app.editing.is_none(),
        "批注弹窗开启时打字必须落入批注框，绝不能泄漏到背景单元格"
    );
    assert_eq!(
        app.wb.sheet(0).value(1, 1),
        sheetcore::model::Value::Empty,
        "背景 B2 单元格内容不得被污染"
    );

    let ctrl_mod = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    let gen_before = app.doc_gen;
    let out_saved = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1440.0, 900.0),
            )),
            modifiers: ctrl_mod,
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed: true,
                repeat: false,
                modifiers: ctrl_mod,
            }],
            ..Default::default()
        },
        |ctx| eframe::App::update(&mut app, ctx, &mut frame),
    );
    let _ = out_saved;
    assert!(app.note_edit.is_none(), "Ctrl+Enter 后应保存并关闭批注窗口");
    let saved = app
        .wb
        .sheet(0)
        .notes
        .get(&sheetcore::model::CellRef::new(1, 1))
        .expect("B2 必须成功写入批注");
    assert_eq!(saved.text, "核对本笔往来款");
    assert_eq!((saved.r, saved.c), (1, 1));
    assert!(app.wb.sheet(0).notes_dirty);
    assert!(app.doc_gen > gen_before, "保存批注必须递增 doc_gen 防止后台重算回写冲掉批注");

    // 下一帧即使鼠标未移回 B2，由于 B2 处于选中态，屏幕必须立即可见批注浮签内容！
    let out_vis = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1440.0, 900.0),
            )),
            ..Default::default()
        },
        |ctx| eframe::App::update(&mut app, ctx, &mut frame),
    );
    let mut texts = Vec::new();
    for cs in &out_vis.shapes {
        collect_texts(std::slice::from_ref(&cs.shape), &mut texts);
    }
    let joined = texts.join("\n");
    assert!(
        joined.contains("核对本笔往来款"),
        "新增批注保存后界面应立即显示批注浮签内容，实际渲染文本：\n{joined}"
    );

    // 合并单元格 C3:D4 上的批注锚点归一化与右键删除测试
    app.sel = (2, 2);
    app.sel_end = (3, 3);
    app.toggle_merge(true);
    app.sel = (3, 3);
    app.sel_end = (3, 3);
    app.open_note_editor(Some(&ctx));
    assert_eq!(
        app.note_edit.as_ref().map(|(r, c, _)| (*r, *c)),
        Some((2, 2)),
        "合并区域内部任意坐标新建批注都必须归一到左上角锚点 C3"
    );
}

#[test]
fn unmerge_disabled_when_unmerged_and_zero_lag_noop() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);

    // 种入 2000 条公式，放大一旦误触发同步 Deps::build + recalc_all 的耗时特征
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(
            0,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Number(10.0),
                formula: None,
            },
        );
        for r in 1..=2000u32 {
            sh.set(
                r,
                0,
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::Number(r as f64),
                    formula: Some(format!("=A{}+1", r).into()),
                },
            );
        }
    }
    app.wb.dirty = false;
    let gen0 = app.doc_gen;
    let undo0 = app.undo.depth();

    // 1) 单格未合并时：右键菜单「取消合并」「合并单元格」「删除批注」均应置灰禁用，功能区「取消合并」亦禁用
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    let rows = app.cell_ctx_menu_rows();
    let row_merge = rows.iter().find(|r| r.label == "合并单元格").unwrap();
    let row_unmerge = rows.iter().find(|r| r.label == "取消合并").unwrap();
    let row_del_note = rows.iter().find(|r| r.label == "删除批注").unwrap();
    assert!(!row_merge.enabled, "单格选区下『合并单元格』应置灰禁用");
    assert!(!row_unmerge.enabled, "无合并选区下『取消合并』必须置灰禁用");
    assert!(!row_del_note.enabled, "无批注选区下『删除批注』应置灰禁用");
    assert!(!app.ctl_state("UnmergeCellsItem").enabled, "功能区下拉『取消合并』也必须置灰禁用");

    // 在右键菜单中真实点击已禁用的「取消合并」行：不得关闭菜单触发动作，不得标脏
    app.ctx_menu = Some(CtxMenuState {
        pos: egui::pos2(200.0, 200.0),
        frame: 0,
        sub: None,
        kind: CtxKind::Cell,
        idx: 0,
        key: 0,
        sub_key: 0,
    });
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    let menu_rect = app.ctx_rects.first().copied().expect("菜单已绘制");
    let unmerge_idx = rows.iter().position(|r| r.label == "取消合并").unwrap();
    let unmerge_pt = egui::pos2(
        menu_rect.center().x,
        crate::ctx_row_top(&rows, unmerge_idx, menu_rect.top() + 4.0) + crate::CTX_ROW_H * 0.5,
    );
    wps_frame(
        &mut app,
        &ctx,
        vec![
            egui::Event::PointerMoved(unmerge_pt),
            egui::Event::PointerButton {
                pos: unmerge_pt,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: unmerge_pt,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        egui::Modifiers::NONE,
    );
    assert!(!app.wb.dirty, "点击禁用的『取消合并』绝不能标脏工作簿");
    assert_eq!(app.doc_gen, gen0, "点击禁用的『取消合并』绝不能递增 doc_gen 引发全表扫描");
    assert_eq!(app.undo.depth(), undo0);
    app.ctx_menu = None;

    // 2) 选中整列 1,048,576 格（无合并）直接调用 toggle_merge(false)：必须微秒级零分配返回！
    app.sel = (0, 2);
    app.sel_end = (1_048_575, 2);
    let t0 = std::time::Instant::now();
    app.toggle_merge(false);
    let elapsed = t0.elapsed();
    assert!(!app.wb.dirty, "无合并选区执行取消合并不得标脏");
    assert_eq!(app.doc_gen, gen0, "无合并选区执行取消合并不得递增 doc_gen");
    assert_eq!(app.undo.depth(), undo0);
    assert!(
        elapsed.as_millis() < 25,
        "整列无合并执行 toggle_merge(false) 必须零重算瞬间返回，实际耗时 {:?}",
        elapsed
    );

    // 3) 真实合并 B2:C3 后，单击锚点 B2 (1,1)：取消合并必须亮起、MergeCenter 勾选，且取消合并无卡顿
    app.sel = (1, 1);
    app.sel_end = (2, 2);
    app.toggle_merge(true);
    assert_eq!(app.wb.sheet(0).merges, vec![(1, 1, 2, 2)]);
    app.sel = (1, 1);
    app.sel_end = (1, 1);
    assert!(app.selection_has_merge());
    assert!(app.ctl_state("MergeCenter").checked, "单击合并单元格锚点时『合并后居中』应处于激活勾选态");
    let rows_merged = app.cell_ctx_menu_rows();
    assert!(
        rows_merged.iter().find(|r| r.label == "取消合并").unwrap().enabled,
        "选中合并单元格时右键『取消合并』必须可用"
    );
    let t1 = std::time::Instant::now();
    app.toggle_merge(false);
    let elapsed_unmerge = t1.elapsed();
    assert!(app.wb.sheet(0).merges.is_empty(), "取消合并后合并区应清空");
    assert_eq!((app.sel, app.sel_end), ((1, 1), (2, 2)), "取消合并后选区应保留原合并块范围");
    assert!(
        elapsed_unmerge.as_millis() < 25,
        "含 2000 条公式的工作表取消合并不得阻塞 UI 线程全量重算，实际耗时 {:?}",
        elapsed_unmerge
    );
}

#[test]
fn bottom_horizontal_scrollbar_mouse_wheel_moves_scroll_and_thumb() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    let mut frame = eframe::Frame::_new_kittest();

    // 首帧布局（1440×900 窗口下：底部状态栏 22px，倒数第二栏 32px 合体底栏 y ∈ [846, 878]，右侧横向滚动条 x ∈ [800, 1420]）
    let out0 = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1440.0, 900.0),
            )),
            ..Default::default()
        },
        |ctx| eframe::App::update(&mut app, ctx, &mut frame),
    );
    assert_eq!(app.scroll.x, 0.0);

    let find_hsb_thumb_left = |shapes: &[egui::epaint::ClippedShape]| -> f32 {
        let mut candidates = Vec::new();
        for cs in shapes {
            if let egui::epaint::Shape::Rect(r) = &cs.shape {
                // 滑块位于底栏内；空闲时 5 点，悬停时平滑扩展至 8 点。
                if r.rect.top() >= 850.0
                    && r.rect.bottom() <= 876.0
                    && (4.9..=8.1).contains(&r.rect.height())
                    && r.rect.width() >= 24.0
                    && r.rect.width() < 520.0
                    && r.rect.left() > 700.0
                {
                    candidates.push(r.rect.left());
                }
            }
        }
        *candidates.last().expect("应能找到底部横向滚动条滑块矩形")
    };

    let thumb_x0 = find_hsb_thumb_left(&out0.shapes);
    let hsb_hover_pos = egui::pos2(1100.0, 862.0);

    // 不按 Shift，直接把鼠标放在下方横向滚动条上向下滚轮：scroll.x 与滑块视觉位置都必须向右移动！
    let out1 = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1440.0, 900.0),
            )),
            events: vec![
                egui::Event::PointerMoved(hsb_hover_pos),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -80.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        },
        |ctx| eframe::App::update(&mut app, ctx, &mut frame),
    );
    let thumb_x1 = find_hsb_thumb_left(&out1.shapes);
    assert!(
        app.scroll.x > 100.0,
        "光标悬停横向滚动条滚动滚轮应推动横向偏移，实际 scroll.x={}",
        app.scroll.x
    );
    assert!(
        thumb_x1 > thumb_x0 + 5.0,
        "横向滚动条滑块也必须同步向右显著移动：before={thumb_x0}, after={thumb_x1}"
    );

    // 反向向上滚轮回到最左端
    let out2 = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1440.0, 900.0),
            )),
            events: vec![
                egui::Event::PointerMoved(hsb_hover_pos),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, 200.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        },
        |ctx| eframe::App::update(&mut app, ctx, &mut frame),
    );
    let thumb_x2 = find_hsb_thumb_left(&out2.shapes);
    assert_eq!(app.scroll.x, 0.0, "向上滚轮滚到底应回到 0");
    assert!(
        (thumb_x2 - thumb_x0).abs() < 1.0,
        "回到最左端后滑块应归位：init={thumb_x0}, now={thumb_x2}"
    );
}

#[test]
fn external_links_auto_sync_on_open_disk_change_and_open_tabs() {
    let env = crate::test_env::TestEnv::new("auto_xlinks_e2e");
    let dir = env.path("proj");
    let sub_dir = dir.join("分表");
    std::fs::create_dir_all(&sub_dir).unwrap();

    // 1) 创建源工作簿放在子文件夹 `./分表/260509_总表.xlsx`（测试子目录自动发现能力！）
    let src_path = sub_dir.join("260509_总表.xlsx");
    let write_src = |val: f64| {
        let mut s = sheetcore::model::Workbook::new_blank();
        s.sheets[0].name = "总表".into();
        s.sheets[0].set(
            0,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Number(val),
                formula: None,
            },
        );
        sheetcore::xlsx::save(&src_path, &s).unwrap();
    };
    write_src(10.0);

    // 创建汇总表（仅记录文件名，旧缓存值 10.0）
    let main_path = dir.join("汇总.xlsx");
    {
        let mut wb = sheetcore::model::Workbook::new_blank();
        wb.path = Some(main_path.clone());
        wb.sheets[0].name = "汇总".into();
        let mut link = sheetcore::xlinks::ExternalBook {
            index: 1,
            target: "260509_总表.xlsx".into(),
            sheets: vec!["总表".into()],
            ..Default::default()
        };
        link.cache.insert((0, 0, 0), sheetcore::model::Value::Number(10.0));
        wb.links.push(link);
        wb.sheets[0].set(
            0,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Number(10.0),
                formula: Some("=[1]总表!A1*2".into()),
            },
        );
        sheetcore::xlsx::save(&main_path, &wb).unwrap();
    }

    // 在打开汇总表前，外部修改源工作簿为 88.0 -> 打开汇总表应全自动同步算出 176.0！
    write_src(88.0);
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    app.open_path(main_path.clone());
    for _ in 0..80 {
        wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
        if app.tasks.open_rx.is_none() && !app.tasks.busy {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    assert_eq!(
        app.wb.sheet(0).value(0, 0),
        sheetcore::model::Value::Number(176.0),
        "打开工作簿时应全自动定位子目录源文件并更新重算"
    );
    assert!(
        !app.wb.dirty,
        "仅打开工作簿并同步外部链接不得将未编辑的原文件误标为脏"
    );

    // 2) 工作簿保持打开状态下，磁盘源文件在外部被修改保存为 250.0 -> 后台监控应自动感知并重算为 500.0！
    write_src(250.0);
    std::thread::sleep(std::time::Duration::from_millis(180));
    app.xlink_last_check = None;
    for _ in 0..80 {
        wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
        if app.wb.sheet(0).value(0, 0) == sheetcore::model::Value::Number(500.0) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    assert_eq!(
        app.wb.sheet(0).value(0, 0),
        sheetcore::model::Value::Number(500.0),
        "打开期间外部磁盘修改源文件后应零点击全自动同步并更新公式结果"
    );

    // 3) 在同一窗口第二个标签页打开源工作簿，不保存直接把内存值改为 600.0，切回汇总表标签页 -> 立即实时联动为 1200.0！
    app.open_in_new_tab(src_path.clone());
    for _ in 0..80 {
        wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
        if app.tasks.open_rx.is_none() && !app.tasks.busy {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("600".into()));
    app.commit_edit(None);
    assert!(app.wb.dirty, "源表在标签页内修改但尚未落盘");

    app.switch_tab(0);
    for _ in 0..80 {
        wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
        if app.wb.sheet(0).value(0, 0) == sheetcore::model::Value::Number(1200.0) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    assert_eq!(
        app.wb.sheet(0).value(0, 0),
        sheetcore::model::Value::Number(1200.0),
        "同窗口多标签页修改源工作簿（未保存）切回汇总表时应内存级实时自动联动"
    );
    drop(env);
}

#[test]
fn vlookup_full_column_wide_table_materializes_only_first_column_and_shares_index() {
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets.push(sheetcore::model::Sheet::new("Sheet2"));
    // Sheet2 造一张 500 行 × 26 列（A:Z）宽表
    for r in 0..500u32 {
        wb.sheet_mut(1).set(
            r,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Text(format!("K{r}").into()),
                formula: None,
            },
        );
        for c in 1..26u32 {
            wb.sheet_mut(1).set(
                r,
                c,
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::Number((r * 100 + c) as f64),
                    formula: None,
                },
            );
        }
    }
    // Sheet1 同时查 A:B, A:C, ..., A:Z 共 25 列
    let mut calc = sheetcore::formula::Calc::new(&wb);
    for c in 2..=26usize {
        let expr = format!("VLOOKUP(\"K499\",Sheet2!A:Z,{c},FALSE)");
        let node = sheetcore::formula::parse(&expr).unwrap();
        let v = calc.eval_node(&node, 0);
        assert_eq!(
            v.to_number().unwrap(),
            (499 * 100 + (c as u32 - 1)) as f64,
            "第 {c} 列 VLOOKUP 返回值应精确匹配"
        );
    }
    assert_eq!(
        calc.range_reads, 500,
        "25 条跨 26 列 VLOOKUP 只应物化首列 500 格，绝不能展开 26 列（13000 格）或重复建索引"
    );
}

#[test]
fn giant_ctrl_a_and_whole_column_operations_do_not_oom_or_freeze() {
    let ctx = egui::Context::default();
    let mut app = SheetApp::new_with_context(&ctx, None);
    for (r, txt, num) in [(0u32, "部门", 0.0), (1, "财务", 10.0), (2, "财务", 10.0), (3, "研发", 20.0)] {
        let val = if r == 0 {
            sheetcore::model::Value::Text(txt.into())
        } else {
            sheetcore::model::Value::Text(txt.into())
        };
        app.wb.sheet_mut(0).set(r, 0, sheetcore::model::Cell { value: val, formula: None });
        app.wb.sheet_mut(0).set(
            r,
            1,
            sheetcore::model::Cell {
                value: if r == 0 {
                    sheetcore::model::Value::Text("金额".into())
                } else {
                    sheetcore::model::Value::Number(num)
                },
                formula: None,
            },
        );
    }

    let t0 = std::time::Instant::now();

    // 1) Ctrl+A 全选点「格式刷」绝不能触发 34GB OOM，应自动裁剪到实际使用范围
    app.sel = (0, 0);
    app.sel_end = (1_048_575, 16_383);
    app.ribbon_action("FormatPainter", &ctx);
    assert!(app.format_painter.is_some());
    assert_eq!(app.format_painter.as_ref().unwrap().rect, (0, 0, 3, 1));
    app.format_painter = None;

    // 2) 整列选中（A:B = 0..=1_048_575）执行「去重」：绝不展开百万格，且正确删掉重复行
    app.sel = (0, 0);
    app.sel_end = (1_048_575, 1);
    app.run_dedup();
    assert!(app.status.contains("删除了 1 行重复"), "status={}", app.status);
    assert_eq!(app.wb.sheet(0).value(2, 0).display(), "研发");

    // 3) 整列选中（B 列 = 0..=1_048_575）执行「自动求和」：绝不越界到 1048576 行，应汇总到数据下方 B4
    app.sel = (1, 1);
    app.sel_end = (1_048_575, 1);
    app.auto_func_wps("SUM");
    assert_eq!(app.sel, (3, 1));
    assert_eq!(app.wb.sheet(0).value(3, 1), sheetcore::model::Value::Number(30.0));

    // 4) 部分超大列选区（B2:B1048576, r1=1 > 0）按清除内容：稀疏清除，不漏删也不分配百万格
    app.sel = (1, 1);
    app.sel_end = (1_048_575, 1);
    app.clear_values();
    assert!(app.wb.sheet(0).value(1, 1).is_empty());
    assert!(app.wb.sheet(0).value(3, 1).is_empty());

    // 5) 序列填充（工作日 SeriesPreset::Weekday）在 Ctrl+A 下秒级拦截，正常区域 O(1) 精准推算工作日
    app.sel = (0, 0);
    app.sel_end = (1_048_575, 16_383);
    app.fill_quick_series(crate::wps_menus::SeriesPreset::Weekday);
    assert!(app.status.contains("20 万格"), "status={}", app.status);

    // 验证 O(1) 工作日整周跳过：2025-01-03(周五, 45660) 向下填 4 格应得 周五(45660), 周一(45663), 周二(45664), 周三(45665)
    app.wb.sheet_mut(0).set(
        0,
        2,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Number(45660.0),
            formula: None,
        },
    );
    app.sel = (0, 2);
    app.sel_end = (3, 2);
    app.fill_quick_series(crate::wps_menus::SeriesPreset::Weekday);
    assert_eq!(app.wb.sheet(0).value(0, 2), sheetcore::model::Value::Number(45660.0));
    assert_eq!(app.wb.sheet(0).value(1, 2), sheetcore::model::Value::Number(45663.0));
    assert_eq!(app.wb.sheet(0).value(2, 2), sheetcore::model::Value::Number(45664.0));
    assert_eq!(app.wb.sheet(0).value(3, 2), sheetcore::model::Value::Number(45665.0));

    // 6) ExternalBook.cache 的 Arc CoW 零拷贝克隆验证
    let mut link = sheetcore::xlinks::ExternalBook::default();
    link.sheets = vec!["Sheet1".into()];
    link.cache.insert((0, 0, 0), sheetcore::model::Value::Number(99.0));
    app.wb.links.push(link);
    let cloned_wb = app.wb.clone();
    assert!(
        std::sync::Arc::ptr_eq(&app.wb.links[0].cache.0, &cloned_wb.links[0].cache.0),
        "Workbook::clone() 应零拷贝共享 ExternalBook.cache"
    );

    assert!(
        t0.elapsed().as_millis() < 500,
        "全套超大选区操作应在毫秒级完成，实际耗时 {:?}",
        t0.elapsed()
    );
}

#[test]
fn frozen_grid_background_follows_theme_switches() {
    let _env = crate::test_env::TestEnv::new("frozen-grid-theme");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.new_document();
    app.wb.sheet_mut(0).freeze = Some((1, 1));

    for theme in crate::theme::AppTheme::ALL {
        app.prefs.theme = theme as u8;
        wps_frame(&mut app, &ctx, Vec::new(), egui::Modifiers::NONE);
        let background = crate::theme::palette_for(theme).grid_bg;
        let mut panels = 0;
        for (clip, shapes) in &app.cf_inputs.grid.viewport.frozen_batches {
            let Some(egui::Shape::Mesh(mesh)) = shapes.first() else { continue };
            let Some(quad) = mesh.vertices.get(..4) else { continue };
            let corners = [clip.left_top(), clip.right_top(), clip.left_bottom(), clip.right_bottom()];
            if corners.iter().all(|corner| quad.iter().any(|v| v.pos == *corner)) {
                assert!(quad.iter().all(|v| v.color == background), "{theme:?} {clip:?}");
                panels += 1;
            }
        }
        assert_eq!(panels, 3, "expected frozen row, column and corner in {theme:?}");
    }
}

#[test]
fn grid_rulers_use_physical_pixel_lines_and_selected_range_tracks_the_theme() {
    let _env = crate::test_env::TestEnv::new("sheet-grid-chrome");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.new_document();
    app.wb.sheet_mut(0).freeze = Some((1, 1));
    app.sel = (1, 1);
    app.sel_end = (3, 2);
    for theme in crate::theme::AppTheme::ALL {
        app.prefs.theme = theme as u8;
        let pal = crate::theme::palette_for(theme);
        let line = if theme == crate::theme::AppTheme::HighContrast {
            pal.grid_line
        } else {
            pal.grid_bg.lerp_to_gamma(pal.grid_line, 0.82)
        };
        for ppp in [1.0_f32, 1.5] {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(720.0, 440.0))),
                ..Default::default()
            };
            input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(ppp);
            let mut frame = eframe::Frame::_new_kittest();
            let output = ctx.run(input, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
            assert!(output.shapes.iter().any(|s| matches!(
                &s.shape, egui::Shape::Rect(r) if r.fill == pal.accent.gamma_multiply(0.13)
            )), "missing range fill in {theme:?}");
            assert!(output.shapes.iter().any(|s| matches!(
                &s.shape, egui::Shape::Rect(r) if r.stroke == Stroke::new(2.0, pal.accent)
            )), "missing selection border in {theme:?}");
            let mut saw_row = false;
            let mut saw_col = false;
            for shape in &output.shapes {
                let egui::Shape::Mesh(mesh) = &shape.shape else { continue };
                for quad in mesh.vertices.chunks_exact(4) {
                    if !quad.iter().all(|v| v.color == line) { continue; }
                    let size = quad[3].pos - quad[0].pos;
                    let edge = if size.x < size.y { &mut saw_col } else { &mut saw_row };
                    let width = size.x.min(size.y);
                    assert!((width * ppp - 1.0).abs() < 0.001, "{theme:?} @{ppp} {width}");
                    let start = if size.x < size.y { quad[0].pos.x } else { quad[0].pos.y };
                    assert!((start * ppp - (start * ppp).round()).abs() < 0.001);
                    *edge = true;
                }
            }
            assert!(saw_row && saw_col, "{theme:?} @{ppp}");
            let viewport = app.cf_inputs.grid.viewport.key.as_ref().unwrap();
            assert_eq!(viewport.pixels_per_point_bits, ppp.to_bits());
        }
    }
}

#[test]
fn frozen_corner_and_cross_freeze_merge_never_overlap_scrolled_rows() {
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);

    {
        let mut big_hdr = app.wb.styles.get(0).clone();
        big_hdr.size = 22.0;
        big_hdr.bold = true;
        let big_sid = app.wb.styles.intern(big_hdr);
        let sh = app.wb.sheet_mut(0);
        sh.freeze = Some((11, 2));
        sh.rows.set(1, 36.0);
        sh.set(
            0,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Text("预收款".into()),
                formula: None,
            },
        );
        sh.set(
            0,
            1,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Text("4296800,万元".into()),
                formula: None,
            },
        );
        sh.set(
            1,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Text("货柜跟踪表-客户销售明细".into()),
                formula: None,
            },
        );
        sh.set_style(1, 0, big_sid);
        sh.merges.push((1, 0, 1, 4)); // A2:E2 跨越第 2 列竖向冻结线

        for r in 195..210u32 {
            sh.set(
                r,
                1,
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::Text(format!("愉悦-F{}", 6800 + r).into()),
                    formula: None,
                },
            );
        }
    }
    app.sel = (202, 2);
    app.sel_end = (202, 2);
    app.scroll = egui::vec2(0.0, 4200.0);

    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1440.0, 900.0),
        )),
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    let out = ctx.run(raw, |ctx| eframe::App::update(&mut app, ctx, &mut frame));

    fn collect_texts(
        shapes: &[egui::epaint::ClippedShape],
        acc: &mut Vec<(String, egui::Rect, egui::Rect)>,
    ) {
        for cs in shapes {
            match &cs.shape {
                egui::Shape::Text(t) => {
                    acc.push((
                        t.galley.job.text.clone(),
                        t.visual_bounding_rect(),
                        cs.clip_rect,
                    ));
                }
                egui::Shape::Vec(v) => {
                    let sub: Vec<egui::epaint::ClippedShape> = v
                        .iter()
                        .cloned()
                        .map(|s| egui::epaint::ClippedShape {
                            clip_rect: cs.clip_rect,
                            shape: s,
                        })
                        .collect();
                    collect_texts(&sub, acc);
                }
                _ => {}
            }
        }
    }

    let mut texts = Vec::new();
    collect_texts(&out.shapes, &mut texts);

    let (_, corner_b1, corner_clip) = texts
        .iter()
        .find(|(s, _, _)| s == "4296800,万元")
        .expect("左上冻结角 B1 应正常绘制");
    let body_y_bound = corner_clip.max.y;
    assert!(corner_b1.top() < body_y_bound);

    // 核心不变量 1：滚动到 ~200 行的货柜号绝不能出现在冻结行上沿区域 (y < body_y_bound)
    for (txt, bounds, clip) in &texts {
        if txt.starts_with("愉悦-F") {
            let visible = bounds.intersect(*clip);
            if visible.width() > 0.5 && visible.height() > 0.5 {
                assert!(
                    visible.top() >= body_y_bound - 0.5,
                    "滚动行文本 {txt:?} 泄漏到了冻结顶栏内部！visible={visible:?}, body_y={body_y_bound}"
                );
            }
        }
    }

    // 核心不变量 2：横跨冻结线的 A2:E2 合并标题必须在左上冻结角 (clip_corner) 与上方冻结栏 (clip_top) 两侧无缝衔接绘制
    let title_clips: Vec<(egui::Rect, egui::Rect)> = texts
        .iter()
        .filter(|(txt, _, _)| txt == "货柜跟踪表-客户销售明细")
        .map(|(_, bounds, clip)| (*bounds, *clip))
        .collect();
    assert!(
        title_clips.len() >= 2,
        "跨冻结线合并标题应在左上冻结角与上方冻结栏两个象限同时绘制，实际仅绘制 {} 次",
        title_clips.len()
    );
    // 左半象限裁剪右边界即竖向冻结分界线 body_x，右半象限裁剪左边界紧接同一条 body_x
    let freeze_x = title_clips
        .iter()
        .map(|(_, clip)| clip.max.x)
        .fold(f32::INFINITY, f32::min);
    let title_spans_left = title_clips.iter().any(|(bounds, clip)| {
        let vis = bounds.intersect(*clip);
        vis.width() > 1.0 && vis.height() > 1.0 && vis.left() < freeze_x - 2.0 && (clip.max.x - freeze_x).abs() < 1.0
    });
    let title_spans_right = title_clips.iter().any(|(bounds, clip)| {
        let vis = bounds.intersect(*clip);
        vis.width() > 1.0 && vis.height() > 1.0 && vis.right() > freeze_x + 2.0 && (clip.min.x - freeze_x).abs() < 1.0
    });
    assert!(
        title_spans_left && title_spans_right,
        "跨冻结线合并标题应完整跨越冻结分界线 x={freeze_x} 无缝渲染：left={title_spans_left}, right={title_spans_right}, clips={title_clips:?}"
    );
}

#[test]
fn top_left_home_and_tab_close_preserve_edits_and_save_silently() {
    let env = crate::test_env::TestEnv::new("top_left_tab_close_save_prompt");
    let path = env.path("愉悦定金.xlsx");
    {
        let mut wb = sheetcore::model::Workbook::new_blank();
        wb.sheets[0].set(
            0,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Number(100.0),
                formula: None,
            },
        );
        sheetcore::xlsx::save_atomic(&path, &wb).unwrap();
    }

    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    app.open_path(path.clone());
    for _ in 0..80 {
        wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
        if app.tasks.open_rx.is_none() && !app.tasks.busy {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // 1) 编辑单元格 A1 为 777（尚未按 Ctrl+S 保存）
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.begin_edit(Some("777".into()));
    assert!(app.commit_edit(None));
    assert!(app.wb.dirty);

    // 2) 点左上角「主页」切到开始页：顶栏必须仍然绘制已打开的「愉悦定金.xlsx」文档标签，
    //    且在开始页「最近打开」再次点击同一文件时必须直接切回内存中的脏标签而非重读磁盘旧文件！
    app.show_start = true;
    fn collect_shape_texts(shapes: &[egui::epaint::Shape], out: &mut Vec<String>) {
        for s in shapes {
            match s {
                egui::epaint::Shape::Text(t) => out.push(t.galley.job.text.clone()),
                egui::epaint::Shape::Vec(v) => collect_shape_texts(v, out),
                _ => {}
            }
        }
    }
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1440.0, 900.0),
        )),
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    let out = ctx.run(raw, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
    let mut texts = Vec::new();
    for cs in &out.shapes {
        collect_shape_texts(std::slice::from_ref(&cs.shape), &mut texts);
    }
    assert!(
        texts.iter().any(|t| t.contains("愉悦定金.xlsx")),
        "切到主页时左上角顶栏仍须显示已打开的文档标签，不得像被关闭一样消失"
    );

    app.open_any(path.clone());
    assert!(!app.show_start, "再次点击已打开文件应直接回到该工作簿界面");
    assert_eq!(app.tabs.len(), 1, "不得为同一路径重复新开第二个磁盘副本标签");
    assert_eq!(
        app.wb.sheet(0).value(0, 0),
        sheetcore::model::Value::Number(777.0),
        "回到已打开文档后未保存的内存修改必须完好无损"
    );

    // 点击含未保存修改文档的 × 后先弹出「保存 / 不保存 / 取消」确认框，用户确认保存（回车）后后台写完再关闭。
    app.request_close_tab(0);
    assert!(app.has_open_workbook, "有未保存修改时点击标签 × 不得直接关闭文件");
    assert_eq!(app.tab_close_confirm, Some(app.doc_uid), "关闭脏工作簿应先询问保存/不保存/取消");
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    );
    assert!(app.tab_close_confirm.is_none(), "确认保存后应关闭确认框");
    for _ in 0..80 {
        wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
        if !app.has_open_workbook && app.tasks.saving.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    assert!(!app.has_open_workbook && app.show_start, "保存完成后应干净关闭标签页回到主页");
    let reloaded = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(
        reloaded.sheets[0].value(0, 0),
        sheetcore::model::Value::Number(777.0),
        "确认保存关闭后再打开文件，修改内容必须已落盘保留"
    );
    drop(env);
}

#[test]
fn audit_fixes_ai_config_dpapi_redaction_and_html_rowspan_paste() {
    let env = crate::test_env::TestEnv::new("audit_app_fixes");
    let cfg_path = env.path("ai.ini");
    let cfg = ai_agent::AiConfig {
        provider_id: "p".into(),
        display_name: "Test".into(),
        base_url: "http://[::1]:8080/v1?api-version=2024#frag".into(),
        api_key: "sk-super-secret-value-12345".into(),
        model: "m".into(),
        native_tools: true,
        allow_unframed_text: false,
    };
    cfg.try_save_to(&cfg_path).expect("atomic save must succeed");
    let raw_disk = std::fs::read_to_string(&cfg_path).unwrap();
    #[cfg(windows)]
    assert!(
        !raw_disk.contains("sk-super-secret-value-12345") && raw_disk.contains("api_key=dpapi:"),
        "api_key must be DPAPI-encrypted on disk, got: {raw_disk}"
    );
    assert!(
        !format!("{cfg:?}").contains("sk-super-secret-value-12345"),
        "Debug output must redact api_key"
    );
    let loaded = ai_agent::AiConfig::load_from(&cfg_path);
    assert_eq!(loaded, cfg);

    // HTML rowspan="2" paste must shift the second row's cell to col B instead of overwriting col A
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    let block = crate::office_clipboard::parse_html(
        "<table><tr><td rowspan=\"2\">MergedA</td><td>B1</td></tr><tr><td>B2</td></tr></table>",
    );
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    assert!(app.paste_office_block(&block));
    assert_eq!(app.wb.sheet(0).value(0, 0).display(), "MergedA");
    assert_eq!(app.wb.sheet(0).value(0, 1).display(), "B1");
    assert_eq!(app.wb.sheet(0).value(1, 0).display(), "");
    assert_eq!(app.wb.sheet(0).value(1, 1).display(), "B2");
    assert!(app.wb.sheet(0).merges.contains(&(0, 0, 1, 0)));
    drop(env);
}

fn mk_test_thread(r: u32, c: u32, author: &str, text: &str) -> sheetcore::notes::ThreadedComment {
    sheetcore::notes::ThreadedComment {
        r,
        c,
        resolved: false,
        entries: vec![sheetcore::notes::ThreadEntry {
            id: sheetcore::notes::new_ooxml_guid(),
            author: author.into(),
            timestamp: "2026-11-26T08:00:00Z".into(),
            text: text.into(),
        }],
    }
}

fn collect_shape_texts_with_pos(
    shapes: &[egui::epaint::Shape],
    out: &mut Vec<(String, egui::Pos2)>,
) {
    for sh in shapes {
        match sh {
            egui::epaint::Shape::Text(t) => out.push((t.galley.text().to_owned(), t.pos)),
            egui::epaint::Shape::Vec(v) => collect_shape_texts_with_pos(v, out),
            _ => {}
        }
    }
}

#[test]
fn comments_phase1_copy_cut_paste_tile_clear_move_and_copy_sheet() {
    use sheetcore::model::{Cell, CellRef, Value};
    use sheetcore::notes::Note;

    let env = crate::test_env::TestEnv::new("comments_p1_copy_cut_clear_sheet");
    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);

    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        sh.set(0, 1, Cell { value: Value::Number(200.0), formula: None });
        sh.notes.insert(
            CellRef::new(0, 0),
            Note { r: 0, c: 0, text: "A1传统批注".into(), author: "财务甲".into() },
        );
        sh.threads.insert(CellRef::new(0, 1), mk_test_thread(0, 1, "审阅乙", "B1线程批注"));
        // 目标区已有旧批注，复制覆盖后撤销必须完整还原
        sh.notes.insert(
            CellRef::new(2, 0),
            Note { r: 2, c: 0, text: "目标原批注".into(), author: "原主".into() },
        );
    }

    // 1) Ctrl+C A1:B1 -> 平铺粘贴到 A3:B4（2x2 块中平铺 2 行）
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.clip_copy(false, &ctx);
    app.sel = (2, 0);
    app.sel_end = (3, 1);
    app.clip_paste(false, None);
    {
        let sh = app.wb.sheet(0);
        assert_eq!(sh.notes.get(&CellRef::new(2, 0)).map(|n| (n.r, n.c, n.text.as_str())), Some((2, 0, "A1传统批注")));
        assert_eq!(sh.notes.get(&CellRef::new(3, 0)).map(|n| (n.r, n.c, n.text.as_str())), Some((3, 0, "A1传统批注")));
        assert_eq!(sh.threads.get(&CellRef::new(2, 1)).map(|t| (t.r, t.c, t.entries[0].text.as_str())), Some((2, 1, "B1线程批注")));
        assert_eq!(sh.threads.get(&CellRef::new(3, 1)).map(|t| (t.r, t.c, t.entries[0].text.as_str())), Some((3, 1, "B1线程批注")));
    }
    // 单步撤销：恢复 A3 原批注并移除平铺产生的新批注/线程
    assert!(app.apply_undo(false));
    {
        let sh = app.wb.sheet(0);
        assert_eq!(sh.notes.get(&CellRef::new(2, 0)).map(|n| n.text.as_str()), Some("目标原批注"));
        assert!(sh.notes.get(&CellRef::new(3, 0)).is_none());
        assert!(sh.threads.get(&CellRef::new(2, 1)).is_none());
        assert!(sh.threads.get(&CellRef::new(3, 1)).is_none());
    }

    // 2) Ctrl+X A1:B1 -> Ctrl+V D1：源格批注与线程清空、目标格到位，单步撤销同时还原源与目标
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.clip_copy(true, &ctx);
    app.sel = (0, 3);
    app.sel_end = (0, 3);
    app.clip_paste(false, None);
    {
        let sh = app.wb.sheet(0);
        assert!(sh.notes.get(&CellRef::new(0, 0)).is_none(), "剪切后源批注应清除");
        assert!(sh.threads.get(&CellRef::new(0, 1)).is_none(), "剪切后源线程应清除");
        assert_eq!(sh.notes.get(&CellRef::new(0, 3)).map(|n| (n.r, n.c, n.text.as_str())), Some((0, 3, "A1传统批注")));
        assert_eq!(sh.threads.get(&CellRef::new(0, 4)).map(|t| (t.r, t.c, t.entries[0].text.as_str())), Some((0, 4, "B1线程批注")));
    }
    assert!(app.apply_undo(false));
    {
        let sh = app.wb.sheet(0);
        assert_eq!(sh.notes.get(&CellRef::new(0, 0)).map(|n| n.text.as_str()), Some("A1传统批注"));
        assert!(sh.threads.get(&CellRef::new(0, 1)).is_some());
        assert!(sh.notes.get(&CellRef::new(0, 3)).is_none());
        assert!(sh.threads.get(&CellRef::new(0, 4)).is_none());
    }

    // 3) 拖拽移动 move_selection_by 与 Ctrl 拖拽复制 copy_selection_by：同时携带传统批注与线程，且可撤销
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.move_selection_by(5, 0);
    {
        let sh = app.wb.sheet(0);
        assert!(sh.notes.get(&CellRef::new(0, 0)).is_none());
        assert!(sh.threads.get(&CellRef::new(0, 1)).is_none());
        assert_eq!(sh.notes.get(&CellRef::new(5, 0)).map(|n| (n.r, n.c, n.text.as_str())), Some((5, 0, "A1传统批注")));
        assert_eq!(sh.threads.get(&CellRef::new(5, 1)).map(|t| (t.r, t.c)), Some((5, 1)));
    }
    assert!(app.apply_undo(false));
    assert!(app.wb.sheet(0).notes.contains_key(&CellRef::new(0, 0)));
    assert!(app.wb.sheet(0).threads.contains_key(&CellRef::new(0, 1)));

    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.copy_selection_by(6, 0);
    assert!(app.wb.sheet(0).notes.contains_key(&CellRef::new(6, 0)));
    assert!(app.wb.sheet(0).threads.contains_key(&CellRef::new(6, 1)));
    assert!(app.apply_undo(false));
    assert!(!app.wb.sheet(0).notes.contains_key(&CellRef::new(6, 0)));
    assert!(!app.wb.sheet(0).threads.contains_key(&CellRef::new(6, 1)));

    // 4) ClearAll 同时清除批注与线程且可撤销
    app.sel = (0, 0);
    app.sel_end = (0, 1);
    app.clear_rects(ClearPart::All);
    assert!(app.wb.sheet(0).notes.get(&CellRef::new(0, 0)).is_none());
    assert!(app.wb.sheet(0).threads.get(&CellRef::new(0, 1)).is_none());
    assert!(app.apply_undo(false));
    assert!(app.wb.sheet(0).notes.get(&CellRef::new(0, 0)).is_some());
    assert!(app.wb.sheet(0).threads.get(&CellRef::new(0, 1)).is_some());

    // 5) 复制工作表 copy_sheet_to 克隆批注与线程并标记 dirty，落盘重载不丢失
    assert!(app.copy_sheet_to(0, 1));
    assert_eq!(app.wb.sheets.len(), 2);
    assert!(app.wb.sheet(1).notes_dirty, "复制带批注的工作表必须标记 notes_dirty");
    assert!(app.wb.sheet(1).threads_dirty, "复制带线程批注的工作表必须标记 threads_dirty");
    let out_path = env.path("copied_sheet_comments.xlsx");
    sheetcore::xlsx::save(&out_path, &app.wb).expect("保存复制工作表后的文件");
    let reloaded = sheetcore::xlsx::load(&out_path).expect("重新加载");
    assert_eq!(
        reloaded.sheet(1).notes.get(&CellRef::new(0, 0)).map(|n| n.text.as_str()),
        Some("A1传统批注")
    );
    assert_eq!(
        reloaded.sheet(1).threads.get(&CellRef::new(0, 1)).map(|t| t.entries[0].text.as_str()),
        Some("B1线程批注")
    );
    drop(env);
}

#[test]
fn comments_phase1_delete_and_clear_comments_cover_threads_and_remote_meta() {
    use sheetcore::model::CellRef;
    use sheetcore::notes::Note;
    use sheetcore::sync::SheetMetaKind;

    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    {
        let sh = app.wb.sheet_mut(0);
        sh.notes.insert(
            CellRef::new(1, 1),
            Note { r: 1, c: 1, text: "B2旧批注".into(), author: "甲".into() },
        );
        sh.threads.insert(CellRef::new(1, 2), mk_test_thread(1, 2, "乙", "C2纯线程"));
    }

    // 框选 B2:C2 执行清除批注：传统批注与纯线程必须一并清除且可单步撤销
    app.sel = (1, 1);
    app.sel_end = (1, 2);
    app.clear_comments_in_sel();
    assert!(app.wb.sheet(0).notes.is_empty());
    assert!(app.wb.sheet(0).threads.is_empty());
    assert!(app.wb.sheet(0).notes_dirty);
    assert!(app.wb.sheet(0).threads_dirty);
    assert!(app.apply_undo(false));
    assert!(app.wb.sheet(0).notes.contains_key(&CellRef::new(1, 1)));
    assert!(app.wb.sheet(0).threads.contains_key(&CellRef::new(1, 2)));

    // 远端 SheetMetaKind::Thread 写入与删除应用
    let remote_tc = mk_test_thread(4, 4, "远端", "远端同步线程");
    app.apply_remote_sheet_meta(
        0,
        SheetMetaKind::Thread {
            row: 4,
            col: 4,
            thread: Some(remote_tc.clone()),
        },
        false,
        false,
        false,
    );
    assert_eq!(app.wb.sheet(0).threads.get(&CellRef::new(4, 4)), Some(&remote_tc));
    app.apply_remote_sheet_meta(
        0,
        SheetMetaKind::Thread {
            row: 4,
            col: 4,
            thread: None,
        },
        false,
        false,
        false,
    );
    assert!(app.wb.sheet(0).threads.get(&CellRef::new(4, 4)).is_none());
}

#[test]
fn comments_phase1_sort_moves_notes_and_threads_end_to_end() {
    use sheetcore::model::{Cell, CellRef, Value};
    use sheetcore::notes::Note;

    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("金额".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("摘要".into()), formula: None });
        for (r, amt, desc) in [(1u32, 300.0, "第三"), (2, 100.0, "第一"), (3, 200.0, "第二")] {
            sh.set(r, 0, Cell { value: Value::Number(amt), formula: None });
            sh.set(r, 1, Cell { value: Value::Text(desc.into()), formula: None });
        }
        sh.notes.insert(
            CellRef::new(1, 1),
            Note { r: 1, c: 1, text: "300元批注".into(), author: "财务".into() },
        );
        sh.threads.insert(CellRef::new(2, 0), mk_test_thread(2, 0, "审计", "100元线程"));
    }

    app.sort_region((0, 0, 3, 1), 0, true);
    {
        let sh = app.wb.sheet(0);
        // 升序后：行1=100(原行2), 行2=200(原行3), 行3=300(原行1)
        assert_eq!(sh.value(1, 0), Value::Number(100.0));
        assert_eq!(sh.value(3, 0), Value::Number(300.0));
        assert_eq!(
            sh.notes.get(&CellRef::new(3, 1)).map(|n| (n.r, n.c, n.text.as_str())),
            Some((3, 1, "300元批注"))
        );
        assert!(sh.notes.get(&CellRef::new(1, 1)).is_none());
        assert_eq!(
            sh.threads.get(&CellRef::new(1, 0)).map(|t| (t.r, t.c, t.entries[0].text.as_str())),
            Some((1, 0, "100元线程"))
        );
        assert!(sh.threads.get(&CellRef::new(2, 0)).is_none());
    }

    // 撤销与重做排序：单元格、传统批注、线程批注必须完全同步回滚/重放
    assert!(app.apply_undo(false));
    {
        let sh = app.wb.sheet(0);
        assert_eq!(sh.value(1, 0), Value::Number(300.0));
        assert_eq!(sh.notes.get(&CellRef::new(1, 1)).map(|n| n.text.as_str()), Some("300元批注"));
        assert!(sh.notes.get(&CellRef::new(3, 1)).is_none());
        assert!(sh.threads.get(&CellRef::new(2, 0)).is_some());
        assert!(sh.threads.get(&CellRef::new(1, 0)).is_none());
    }
    assert!(app.apply_undo(true));
    assert_eq!(app.wb.sheet(0).notes.get(&CellRef::new(3, 1)).map(|n| n.text.as_str()), Some("300元批注"));
    assert!(app.wb.sheet(0).threads.get(&CellRef::new(1, 0)).is_some());
}

#[test]
fn comments_phase1_editor_shortcuts_reply_and_continuous_review() {
    use sheetcore::model::CellRef;
    use sheetcore::notes::Note;

    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    {
        let sh = app.wb.sheet_mut(0);
        sh.notes.insert(
            CellRef::new(0, 0),
            Note { r: 0, c: 0, text: "第一条批注".into(), author: "甲".into() },
        );
        sh.threads.insert(CellRef::new(2, 1), mk_test_thread(2, 1, "乙", "第二条纯线程"));
    }

    // 右键菜单应带 Shift+F2 快捷键提示；导航列表必须覆盖纯线程格 B3
    let rows = app.cell_ctx_menu_rows();
    let comment_row = rows.iter().find(|r| r.label == "插入/编辑批注").expect("批注菜单项");
    assert_eq!(comment_row.shortcut, "Shift+F2");
    assert_eq!(app.sheet_comment_keys(0), vec![(0, 0), (2, 1)]);

    // 1) 在 A1 打开批注编辑器，输入草稿后按 Esc 取消 -> 不保存、关闭窗口
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.open_note_editor(Some(&ctx));
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    if let Some((_, _, ref mut buf)) = app.note_edit {
        buf.push_str("-临时草稿");
    }
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    );
    assert!(app.note_edit.is_none(), "Esc 应直接取消并关闭批注编辑器");
    assert_eq!(
        app.wb.sheet(0).notes.get(&CellRef::new(0, 0)).map(|n| n.text.as_str()),
        Some("第一条批注"),
        "Esc 取消不得保存临时修改"
    );

    // 2) 重新打开 A1，修改内容后按 Alt+↓：保存 A1 并连续跳转到下一条批注 B3
    app.open_note_editor(Some(&ctx));
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    if let Some((_, _, ref mut buf)) = app.note_edit {
        *buf = "第一条已更新".into();
    }
    let alt_mod = egui::Modifiers {
        alt: true,
        ..Default::default()
    };
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Key {
            key: egui::Key::ArrowDown,
            physical_key: Some(egui::Key::ArrowDown),
            pressed: true,
            repeat: false,
            modifiers: alt_mod,
        }],
        alt_mod,
    );
    assert_eq!(
        app.wb.sheet(0).notes.get(&CellRef::new(0, 0)).map(|n| n.text.as_str()),
        Some("第一条已更新"),
        "Alt+↓ 切换前必须自动保存当前批注"
    );
    assert_eq!(
        app.note_edit.as_ref().map(|(r, c, _)| (*r, *c)),
        Some((2, 1)),
        "Alt+↓ 应无缝连续打开下一条批注 B3"
    );

    // 3) 在 B3 的线程回复框中输入回复并按 Enter 直接发送，且可撤销
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    app.note_reply_buf = "收到，已核验".into();
    ctx.memory_mut(|m| m.request_focus(egui::Id::new("note_dialog_reply")));
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
    let gen_before = app.doc_gen;
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    );
    let tc_b3 = app.wb.sheet(0).threads.get(&CellRef::new(2, 1)).expect("B3 线程存在");
    assert_eq!(tc_b3.entries.len(), 2, "回复框按 Enter 必须追加新回复");
    assert_eq!(tc_b3.entries[1].text, "收到，已核验");
    assert!(app.doc_gen > gen_before);
    assert!(app.note_reply_buf.is_empty());

    // 撤销回复：B3 线程恢复为 1 条
    assert!(app.apply_undo(false));
    assert_eq!(
        app.wb.sheet(0).threads.get(&CellRef::new(2, 1)).map(|t| t.entries.len()),
        Some(1)
    );
}

#[test]
fn comments_phase1_pin_toggle_and_auto_show_selected_pref() {
    use sheetcore::model::CellRef;
    use sheetcore::notes::Note;

    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    let mut frame = eframe::Frame::_new_kittest();
    {
        let sh = app.wb.sheet_mut(0);
        sh.notes.insert(
            CellRef::new(0, 0),
            Note { r: 0, c: 0, text: "A1固定常显批注".into(), author: "甲".into() },
        );
        sh.notes.insert(
            CellRef::new(2, 2),
            Note { r: 2, c: 2, text: "C3按需批注".into(), author: "乙".into() },
        );
    }

    // 选中 A1 点击「显示/隐藏批注」（note_show）：固定常显，不弹出编辑窗
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.note_show();
    assert!(app.note_edit.is_none(), "ShowComment 应切换固定显示而非强制打开编辑弹窗");
    let sid = app.wb.sheet(0).identity;
    assert!(app.pinned_comments.contains(&(sid, CellRef::new(0, 0))));

    // 把选区移到无批注的 B5，A1 的固定浮签仍持续显示
    app.sel = (4, 1);
    app.sel_end = (4, 1);
    let out = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 900.0))),
            ..Default::default()
        },
        |ctx| eframe::App::update(&mut app, ctx, &mut frame),
    );
    let mut items = Vec::new();
    for cs in &out.shapes {
        collect_shape_texts_with_pos(std::slice::from_ref(&cs.shape), &mut items);
    }
    assert!(
        items.iter().any(|(t, _)| t.contains("A1固定常显批注")),
        "固定常显批注在选区移走后必须继续显示"
    );

    // 再次切换取消固定，并关闭「选中自动展开」后选中 C3：C3 不应自动弹出浮签
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.note_show();
    assert!(!app.pinned_comments.contains(&(sid, CellRef::new(0, 0))));

    app.auto_show_selected_comment = false;
    app.sel = (2, 2);
    app.sel_end = (2, 2);
    let out2 = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 900.0))),
            ..Default::default()
        },
        |ctx| eframe::App::update(&mut app, ctx, &mut frame),
    );
    let mut items2 = Vec::new();
    for cs in &out2.shapes {
        collect_shape_texts_with_pos(std::slice::from_ref(&cs.shape), &mut items2);
    }
    assert!(
        !items2.iter().any(|(t, _)| t.contains("C3按需批注")),
        "关闭选中自动展开后，普通选中未固定的批注格不应自动弹出浮签"
    );
}

#[test]
fn comments_phase1_frozen_pane_callout_no_drift_and_no_bleed() {
    use sheetcore::model::CellRef;
    use sheetcore::notes::Note;

    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    let mut frame = eframe::Frame::_new_kittest();
    {
        let sh = app.wb.sheet_mut(0);
        sh.freeze = Some((1, 1));
        sh.notes.insert(
            CellRef::new(0, 0),
            Note { r: 0, c: 0, text: "冻结标题批注".into(), author: "头".into() },
        );
        sh.notes.insert(
            CellRef::new(1, 1),
            Note { r: 1, c: 1, text: "正文滚动批注".into(), author: "身".into() },
        );
    }
    let sid = app.wb.sheet(0).identity;
    app.pinned_comments.insert((sid, CellRef::new(0, 0)));
    app.pinned_comments.insert((sid, CellRef::new(1, 1)));
    app.sel = (5, 5);
    app.sel_end = (5, 5);

    let run_once = |app: &mut SheetApp, ctx: &egui::Context, frame: &mut eframe::Frame| {
        let out = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 900.0))),
                ..Default::default()
            },
            |ctx| eframe::App::update(app, ctx, frame),
        );
        let mut items = Vec::new();
        for cs in &out.shapes {
            collect_shape_texts_with_pos(std::slice::from_ref(&cs.shape), &mut items);
        }
        items
    };

    // scroll=(0,0) 跑两帧（首帧建 Area 状态，次帧稳定布局）
    let _ = run_once(&mut app, &ctx, &mut frame);
    let items0 = run_once(&mut app, &ctx, &mut frame);
    let frozen_pos0 = items0
        .iter()
        .find(|(t, _)| t.contains("冻结标题批注"))
        .map(|(_, p)| *p)
        .expect("未滚动时冻结格 A1 批注浮签必须可见");
    assert!(
        items0.iter().any(|(t, _)| t.contains("正文滚动批注")),
        "未滚动时正文格 B2 批注浮签必须可见"
    );

    // 大幅向右下滚动：B2 被卷入冻结区下方不可见，A1 仍固定在冻结区左上角原位
    app.scroll = egui::vec2(600.0, 600.0);
    let _ = run_once(&mut app, &ctx, &mut frame);
    let items1 = run_once(&mut app, &ctx, &mut frame);
    let frozen_pos1 = items1
        .iter()
        .find(|(t, _)| t.contains("冻结标题批注"))
        .map(|(_, p)| *p)
        .expect("大幅滚动后冻结格 A1 批注浮签仍必须保持可见");
    assert!(
        (frozen_pos1 - frozen_pos0).length() < 1.5,
        "冻结窗格末尾格 A1 的批注卡片在正文滚动后不得漂移: before={frozen_pos0:?}, after={frozen_pos1:?}"
    );
    assert!(
        !items1.iter().any(|(t, _)| t.contains("正文滚动批注")),
        "已滚入冻结窗格下方的正文格 B2 不得穿模继续显示浮签"
    );
}

#[test]
fn comments_phase2_sidebar_filter_reply_batch_ops_and_render() {
    use sheetcore::model::{Cell, CellRef, Value};
    use sheetcore::notes::Note;

    let _env = crate::test_env::TestEnv::new("comments_p2_sidebar");
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    let mut frame = eframe::Frame::_new_kittest();
    {
        let sh = app.wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Number(500.0), formula: None });
        sh.notes.insert(
            CellRef::new(0, 0),
            Note { r: 0, c: 0, text: "A1往来差异".into(), author: "财务甲".into() },
        );
        sh.threads.insert(CellRef::new(2, 1), mk_test_thread(2, 1, "审计乙", "B3需补发票"));
    }

    // 通过 Ribbon 按钮打开批注管理侧边栏并渲染验证
    assert!(!app.ctl_state("CommentsPanel").checked);
    app.ribbon_action("CommentsPanel", &ctx);
    assert!(app.comments_panel.open);
    assert!(app.ctl_state("CommentsPanel").checked);

    let out = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 900.0))),
            ..Default::default()
        },
        |ctx| eframe::App::update(&mut app, ctx, &mut frame),
    );
    let mut items = Vec::new();
    for cs in &out.shapes {
        collect_shape_texts_with_pos(std::slice::from_ref(&cs.shape), &mut items);
    }
    let joined: String = items.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join("\n");
    assert!(joined.contains("批注管理 (2)"), "侧边栏应显示批注总数:\n{joined}");
    assert!(joined.contains("A1往来差异") && joined.contains("B3需补发票"), "侧边栏应列出全部批注与线程:\n{joined}");

    // 行内快速回复（将传统批注平滑升级为线程）+ 单步撤销
    let cnt = app.reply_comment_cell(0, 0, 0, "已联系客户核对", Some("财务甲")).unwrap();
    assert_eq!(cnt, 2);
    assert!(app.wb.sheet(0).threads.contains_key(&CellRef::new(0, 0)));
    assert!(app.apply_undo(false));
    assert!(!app.wb.sheet(0).threads.contains_key(&CellRef::new(0, 0)));

    // 批量标记已解决 + 单步撤销
    let changed = app.batch_set_comments_resolved(&[(0, 0, 0), (0, 2, 1)], true);
    assert_eq!(changed, 2);
    assert!(app.wb.sheet(0).threads.get(&CellRef::new(0, 0)).unwrap().resolved);
    assert!(app.wb.sheet(0).threads.get(&CellRef::new(2, 1)).unwrap().resolved);
    assert!(app.apply_undo(false));
    assert!(!app.wb.sheet(0).threads.get(&CellRef::new(2, 1)).unwrap().resolved);

    // 批量删除 + 单步撤销
    let deleted = app.batch_delete_comments(&[(0, 0, 0), (0, 2, 1)], "批量删除批注");
    assert_eq!(deleted, 2);
    assert!(app.wb.sheet(0).notes.is_empty() && app.wb.sheet(0).threads.is_empty());
    assert!(app.apply_undo(false));
    assert_eq!(app.wb.sheet(0).notes.len(), 1);
    assert_eq!(app.wb.sheet(0).threads.len(), 1);
}

#[test]
fn comments_phase2_find_and_replace_in_comments_with_undo() {
    use sheetcore::model::CellRef;
    use sheetcore::notes::Note;

    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    {
        let sh = app.wb.sheet_mut(0);
        sh.notes.insert(
            CellRef::new(0, 0),
            Note { r: 0, c: 0, text: "旧口径金额待核对".into(), author: "张会计".into() },
        );
        sh.threads.insert(CellRef::new(1, 2), mk_test_thread(1, 2, "李经理", "按旧口径入账"));
    }

    app.find.in_comments = true;
    app.find.find = "旧口径".into();
    app.find.replace = "新口径".into();
    let hits = app.find_hits();
    assert_eq!(hits.len(), 2, "搜批注必须同时命中传统批注 A1 与线程批注 C2");

    // 单格替换 A1
    app.sel = (0, 0);
    app.sel_end = (0, 0);
    app.replace_current();
    assert_eq!(
        app.wb.sheet(0).notes.get(&CellRef::new(0, 0)).map(|n| n.text.as_str()),
        Some("新口径金额待核对")
    );

    // 全部替换剩余的线程批注 C2，且可单步撤销
    app.replace_all();
    assert_eq!(
        app.wb.sheet(0).threads.get(&CellRef::new(1, 2)).unwrap().entries[0].text,
        "按新口径入账"
    );
    assert!(app.apply_undo(false));
    assert_eq!(
        app.wb.sheet(0).threads.get(&CellRef::new(1, 2)).unwrap().entries[0].text,
        "按旧口径入账"
    );
}

#[test]
fn comments_phase2_editor_timestamp_and_custom_phrases_persistence() {
    use sheetcore::model::CellRef;

    let _env = crate::test_env::TestEnv::new("comments_p2_phrases");
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    app.my_name = "王财务".into();

    // 自定义常用语持久化（完全隔离在 TestEnv 的 LOCALAPPDATA 内）
    assert!(app.comments_panel.add_custom_phrase("已附电子回单"));
    assert!(!app.comments_panel.add_custom_phrase("已附电子回单"), "重复短语应去重");
    let mut fresh = crate::comments_panel::CommentsPanelState::default();
    fresh.ensure_phrases_loaded();
    assert_eq!(fresh.custom_phrases, vec!["已附电子回单".to_string()]);
    assert!(fresh.remove_custom_phrase(0));
    let mut fresh2 = crate::comments_panel::CommentsPanelState::default();
    fresh2.ensure_phrases_loaded();
    assert!(fresh2.custom_phrases.is_empty());

    // 在 B2 打开批注编辑器，按 Ctrl+; 插入时间戳，再按 Ctrl+Enter 保存
    app.sel = (1, 1);
    app.sel_end = (1, 1);
    app.open_note_editor(Some(&ctx));
    wps_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);

    let ctrl_mod = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Key {
            key: egui::Key::Semicolon,
            physical_key: Some(egui::Key::Semicolon),
            pressed: true,
            repeat: false,
            modifiers: ctrl_mod,
        }],
        ctrl_mod,
    );
    wps_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: ctrl_mod,
        }],
        ctrl_mod,
    );
    let saved = app.wb.sheet(0).notes.get(&CellRef::new(1, 1)).expect("应保存含时间戳的批注");
    assert!(
        saved.text.contains("王财务") && saved.text.starts_with('[') && saved.text.contains(':'),
        "Ctrl+; 必须插入含日期时间与用户名的时间戳前缀，实际：{:?}",
        saved.text
    );
}

#[test]
fn comments_phase2_export_comments_summary_sheet_with_hyperlinks_and_undo() {
    use sheetcore::model::{Cell, CellRef, Value};
    use sheetcore::notes::Note;

    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.add_sheet();
    assert_eq!(app.wb.sheets.len(), 2);
    {
        let s0 = app.wb.sheet_mut(0);
        s0.set(1, 1, Cell { value: Value::Number(888.0), formula: None });
        s0.notes.insert(
            CellRef::new(1, 1),
            Note { r: 1, c: 1, text: "表1应收核对".into(), author: "财务甲".into() },
        );
        let s1 = app.wb.sheet_mut(1);
        let mut tc = mk_test_thread(2, 2, "审计乙", "表2已确认结案");
        tc.resolved = true;
        s1.threads.insert(CellRef::new(2, 2), tc);
    }

    app.sheet_idx = 0;
    app.sel = (1, 1);
    app.ribbon_action("ExportCommentsSheet", &ctx);
    assert_eq!(app.wb.sheets.len(), 3, "应新增「批注汇总」工作表");
    assert_eq!(app.sheet_idx, 2);
    {
        let sum = app.wb.sheet(2);
        assert_eq!(sum.name, "批注汇总");
        assert_eq!(sum.freeze, Some((1, 0)));
        assert!(sum.filter.is_some());
        assert_eq!(sum.value(1, 2).display(), "B2");
        assert_eq!(sum.value(1, 3).display(), "888");
        assert_eq!(sum.value(1, 4).display(), "传统批注");
        assert_eq!(sum.value(1, 8).display(), "表1应收核对");
        assert_eq!(sum.hyperlinks.get(&(1, 2)).map(|h| h.location.as_str()), Some("Sheet1!B2"));

        assert_eq!(sum.value(2, 2).display(), "C3");
        assert_eq!(sum.value(2, 4).display(), "线程批注");
        assert_eq!(sum.value(2, 5).display(), "已解决");
        assert_eq!(sum.hyperlinks.get(&(2, 2)).map(|h| h.location.as_str()), Some("Sheet2!C3"));
    }

    // 单步撤销应移除新建的「批注汇总」工作表
    assert!(app.apply_undo(false));
    assert_eq!(app.wb.sheets.len(), 2);
}

#[test]
fn comments_phase2_ai_agent_inspect_and_manage_comments_with_confirmation_and_undo() {
    use serde_json::json;
    use sheetcore::model::CellRef;
    use sheetcore::notes::Note;

    let ctx = egui::Context::default();
    let mut app = wps_app(&ctx);
    app.ai.bound_doc_uid = Some(app.doc_uid);
    app.ai.messages.push(crate::ai_agent::AiMessage {
        role: crate::ai_agent::AiRole::Assistant,
        content: String::new(),
        reasoning: String::new(),
        reasoning_open: false,
        tool_logs: Vec::new(),
        error: None,
        undo_ids: Vec::new(),
    });
    {
        let sh = app.wb.sheet_mut(0);
        sh.notes.insert(
            CellRef::new(0, 0),
            Note { r: 0, c: 0, text: "A1初始批注".into(), author: "张会计".into() },
        );
        sh.threads.insert(CellRef::new(1, 1), mk_test_thread(1, 1, "李经理", "B2待处理"));
    }

    // 1) AI 读取工具 inspect_comments（按状态筛选未解决）
    let inspect = crate::ai_agent::ToolCallSpec {
        name: "inspect_comments".into(),
        arguments: json!({"status": "open"}),
    };
    let crate::ai_agent::AiToolOutput::Ready(_, res) = app.execute_ai_tool(&inspect, &ctx) else {
        panic!("expected immediate inspect_comments result");
    };
    assert_eq!(res["ok"], json!(true));
    assert_eq!(res["total"], json!(2));

    // 2) AI 批量管理工具 manage_comments：同时新建 C3、回复 A1、标记 B2 已解决（原子撤销步）
    let manage = crate::ai_agent::ToolCallSpec {
        name: "manage_comments".into(),
        arguments: json!({
            "sheet": "Sheet1",
            "ops": [
                {"action": "set", "cell": "C3", "text": "AI自动复核标记", "author": "AI助手"},
                {"action": "reply", "cell": "A1", "text": "已核验凭证号 #2026-01", "author": "AI助手"},
                {"action": "resolve", "cell": "B2"}
            ]
        }),
    };
    assert!(
        app.stage_ai_confirmation(&mut manage.clone()).unwrap().is_none(),
        "非删除批注操作无需阻断式确认"
    );
    let crate::ai_agent::AiToolOutput::Ready(_, mres) = app.execute_ai_tool(&manage, &ctx) else {
        panic!("expected immediate manage_comments result");
    };
    assert_eq!(mres["ok"], json!(true));
    assert_eq!(mres["applied"], json!(3));
    assert_eq!(
        app.wb.sheet(0).notes.get(&CellRef::new(2, 2)).map(|n| n.text.as_str()),
        Some("AI自动复核标记")
    );
    assert_eq!(
        app.wb.sheet(0).threads.get(&CellRef::new(0, 0)).map(|t| t.entries.len()),
        Some(2)
    );
    assert!(app.wb.sheet(0).threads.get(&CellRef::new(1, 1)).unwrap().resolved);

    // 3) 一键「撤销上次 AI 操作」必须完整回滚整批 3 项批注改动
    app.undo_last_ai_change();
    assert!(app.wb.sheet(0).notes.get(&CellRef::new(2, 2)).is_none());
    assert!(app.wb.sheet(0).threads.get(&CellRef::new(0, 0)).is_none());
    assert!(!app.wb.sheet(0).threads.get(&CellRef::new(1, 1)).unwrap().resolved);

    // 4) 删除已有批注必须触发待确认提示
    let mut del_call = crate::ai_agent::ToolCallSpec {
        name: "manage_comments".into(),
        arguments: json!({
            "sheet": "Sheet1",
            "ops": [{"action": "delete", "cell": "A1"}]
        }),
    };
    let confirm = app.stage_ai_confirmation(&mut del_call).unwrap();
    assert!(
        confirm.as_deref().is_some_and(|s| s.contains("删除") && s.contains("A1")),
        "AI 删除已有批注必须请求用户确认: {confirm:?}"
    );
}

#[test]
fn comments_editor_window_width_stable_and_sidebar_close_no_tofu() {
    use sheetcore::model::CellRef;
    use sheetcore::notes::{Note, ThreadEntry};

    let _env = crate::test_env::TestEnv::new("comments_ui_width_and_close");
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    let mut frame = eframe::Frame::_new_kittest();
    {
        let sh = app.wb.sheet_mut(0);
        sh.notes.insert(
            CellRef::new(0, 0),
            Note { r: 0, c: 0, text: "A1 传统批注".into(), author: "财务甲".into() },
        );
        let mut tc = mk_test_thread(
            20,
            11,
            "Administrator@设计部-AI",
            "34分批退火炉\newfrgwe\nrfegtew\n[2026-11-26 18:55 Administrator@设计部-AI]",
        );
        tc.entries.push(ThreadEntry {
            id: sheetcore::notes::new_ooxml_guid(),
            author: "Administrator@设计部-AI".into(),
            timestamp: "2026-11-26T10:55:54Z".into(),
            text: "2未归口".into(),
        });
        tc.resolved = true;
        sh.threads.insert(CellRef::new(20, 11), tc);
    }

    let run_frame = |app: &mut SheetApp, ctx: &egui::Context, frame: &mut eframe::Frame, events: Vec<egui::Event>| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1440.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| eframe::App::update(app, ctx, frame),
        )
    };

    // 1) 打开 L21 线程批注编辑窗口，连续渲染 20 帧，宽度不得自行递增漂移
    app.sel = (20, 11);
    app.sel_end = (20, 11);
    app.open_note_editor(Some(&ctx));
    let win_id = egui::Id::new(("note_sticky_editor", 0usize, 20u32, 11u32));
    let mut widths = Vec::new();
    for _ in 0..20 {
        let _ = run_frame(&mut app, &ctx, &mut frame, vec![]);
        let w = ctx
            .memory(|m| m.area_rect(win_id))
            .expect("批注窗口应有有效布局矩形")
            .width();
        widths.push(w);
    }
    let w_settled = widths[3];
    let w_last = *widths.last().unwrap();
    assert!(
        w_last <= 480.0,
        "批注编辑窗口不应无限变宽: widths={widths:?}"
    );
    assert!(
        (w_last - w_settled).abs() < 1.0,
        "批注编辑窗口稳定后多帧不得继续变长: w3={w_settled}, w_last={w_last}, widths={widths:?}"
    );

    // 2) 展开「自定义常用语」管理行，再连续渲染 20 帧，宽度仍不得自行递增
    app.comments_panel.phrases_manage_open = true;
    let mut mgr_widths = Vec::new();
    for _ in 0..20 {
        let _ = run_frame(&mut app, &ctx, &mut frame, vec![]);
        let w = ctx
            .memory(|m| m.area_rect(win_id))
            .expect("批注窗口应保持可见")
            .width();
        mgr_widths.push(w);
    }
    let mw_settled = mgr_widths[3];
    let mw_last = *mgr_widths.last().unwrap();
    assert!(
        mw_last <= 480.0 && (mw_last - mw_settled).abs() < 1.0,
        "展开自定义常用语后批注窗口仍不得自行变宽: mw3={mw_settled}, mw_last={mw_last}, mgr_widths={mgr_widths:?}"
    );

    // 3) 关闭批注窗口并打开右侧批注管理侧边栏：不得渲染缺字方框字符 ✕，且矢量关闭按钮可点击关闭
    app.note_edit = None;
    app.comments_panel.open = true;
    let _ = run_frame(&mut app, &ctx, &mut frame, vec![]);
    let out = run_frame(&mut app, &ctx, &mut frame, vec![]);
    let mut items = Vec::new();
    for cs in &out.shapes {
        collect_shape_texts_with_pos(std::slice::from_ref(&cs.shape), &mut items);
    }
    assert!(
        !items.iter().any(|(t, _)| t.contains('✕')),
        "批注侧边栏不得使用易缺字呈方框的 ✕ 文本符号"
    );

    // 验证批注窗口导航箭头等字符在当前字体下是否存在缺字（与 U+FFFD 替换符对比）
    let tofu_uv = ctx.fonts(|f| {
        let g = f.layout_no_wrap("\u{FFFD}".into(), egui::FontId::proportional(11.0), Color32::BLACK);
        g.rows.first().and_then(|r| r.glyphs.first()).map(|gl| gl.uv_rect)
    });
    for ch in ['×', '◀', '▶'] {
        let ch_uv = ctx.fonts(|f| {
            let g = f.layout_no_wrap(ch.to_string(), egui::FontId::proportional(11.0), Color32::BLACK);
            g.rows.first().and_then(|r| r.glyphs.first()).map(|gl| gl.uv_rect)
        });
        assert_ne!(
            ch_uv, tofu_uv,
            "字符 {ch:?} (U+{:04X}) 在当前 UI 字体中缺失（会渲染成方框）",
            ch as u32
        );
    }

    // 定位标题「批注管理 (2)」同行最右侧的关闭按钮并点击验证可关闭面板
    let hdr_pos = items
        .iter()
        .find(|(t, _)| t.contains("批注管理 (2)"))
        .map(|(_, p)| *p)
        .expect("侧边栏标题应可见");
    // 面板贴屏幕右侧（right margin=8，header inner_margin.x=12，24×24 按钮中心在 1440 - 8 - 12 - 12 = 1408）
    let close_pt = egui::pos2(1408.0, hdr_pos.y + 8.0);
    let _ = run_frame(
        &mut app,
        &ctx,
        &mut frame,
        vec![
            egui::Event::PointerMoved(close_pt),
            egui::Event::PointerButton {
                pos: close_pt,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let _ = run_frame(
        &mut app,
        &ctx,
        &mut frame,
        vec![
            egui::Event::PointerMoved(close_pt),
            egui::Event::PointerButton {
                pos: close_pt,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    assert!(!app.comments_panel.open, "点击批注侧边栏矢量关闭按钮应关闭面板");
}

#[test]
fn shell_geometry_and_focus_anchors_align_across_themes_and_sizes() {
    fn collect_rects(shapes: &[egui::epaint::Shape], out: &mut Vec<egui::epaint::RectShape>) {
        for s in shapes {
            match s {
                egui::epaint::Shape::Rect(r) => out.push(r.clone()),
                egui::epaint::Shape::Vec(v) => collect_rects(v, out),
                _ => {}
            }
        }
    }

    let _env = crate::test_env::TestEnv::new("shell-geometry-186");
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut app = wps_app(&ctx);
    let mut host = eframe::Frame::_new_kittest();

    for theme in theme::AppTheme::ALL {
        app.prefs.theme = theme as u8;
        app.theme_applied = None;
        for (w, h) in [(760.0, 420.0), (1440.0, 900.0), (1920.0, 1080.0)] {
            let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(w, h));
            let mut render = |app: &mut SheetApp| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ctx| eframe::App::update(app, ctx, &mut host),
                )
            };
            let _ = render(&mut app);
            let out = render(&mut app);
            let pal = theme::palette();
            let mut rects = Vec::new();
            for cs in &out.shapes {
                collect_rects(std::slice::from_ref(&cs.shape), &mut rects);
            }

            // Grid canvas must be edge-to-edge full-width (no 8px left/right gutter).
            assert!(
                app.last_pane.x.abs() <= 1.0,
                "grid should align flush with left edge at {w}x{h} ({theme:?}), got last_pane={:?}",
                app.last_pane
            );
            assert!(
                (app.last_view.x + SB - w).abs() <= 2.0,
                "grid + vertical scrollbar should span full viewport width {w} ({theme:?}), got last_view={:?}",
                app.last_view
            );
            // Floating Ribbon body card uses 8px side margins and 8px radius.
            assert!(
                rects.iter().any(|r| {
                    (r.rect.left() - 8.0).abs() <= 1.0
                        && (r.rect.right() - (w - 8.0)).abs() <= 1.0
                        && (r.rect.height() - ui_kit::RIBBON_BODY_H).abs() <= 2.0
                        && r.fill == pal.ribbon_bg
                }),
                "ribbon body floating card missing at {w}x{h} ({theme:?})"
            );
            // Both active Ribbon tab and active Sheet tab paint a refined 2px accent indicator bar.
            let accent_bars = rects
                .iter()
                .filter(|r| {
                    r.fill == pal.accent && r.rect.width() >= 14.0 && r.rect.height() <= 2.5
                })
                .count();
            assert!(
                accent_bars >= 2,
                "expected active Ribbon tab + active Sheet tab accent indicators at {w}x{h} ({theme:?}), got {accent_bars}"
            );
        }
    }
}
