//! WPS 风格顶栏（38px 文档标签整合标题栏）、轻量「文件」下拉菜单、
//! 锚定调色盘、以及底栏（工作表标签 + 可拖动分割条 + 横向滚动条 32px 单行合一）。

use eframe::egui::{self, Align2, Color32, FontId, Key, Rect, Sense, Stroke, Vec2};
use std::path::PathBuf;

use crate::ui_icons::{self, Icon};
use crate::{tokens, ui_kit, CtxKind, CtxMenuState, SheetApp, TabSlot, SB};

/// WPS 标准 10 个主题基色（浅→深自动插值生成 6×10 矩阵）
const WPS_THEME_BASES: [[u8; 3]; 10] = [
    [255, 255, 255], // 白
    [0, 0, 0],       // 黑
    [231, 230, 230], // 浅灰
    [68, 84, 106],   // 深蓝灰
    [68, 114, 196],  // 主题蓝
    [237, 125, 49],  // 橙
    [165, 165, 165], // 灰
    [255, 192, 0],   // 金黄
    [91, 155, 213],  // 浅蓝
    [112, 173, 71],  // 绿
];

/// WPS 10 个标准色（底排固定）
const WPS_STANDARD_COLORS: [[u8; 3]; 10] = [
    [192, 0, 0],     // 深红
    [255, 0, 0],     // 红
    [255, 192, 0],   // 橙黄
    [255, 255, 0],   // 黄
    [146, 208, 80],  // 浅绿
    [0, 176, 80],    // 绿
    [0, 176, 240],   // 浅蓝
    [0, 112, 192],   // 蓝
    [0, 32, 96],     // 深蓝
    [112, 48, 160],  // 紫
];

fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t).round() as u8,
    ]
}

/// 生成 WPS 6×10 主题色调色矩阵
pub(crate) fn wps_theme_matrix() -> [[[u8; 3]; 10]; 6] {
    let mut out = [[[0u8; 3]; 10]; 6];
    // 第 0 行：基色；1..=3：与白混色变浅；4..=5：与黑混色变深
    // 第 0 列（白）和第 1 列（黑）用经典灰度梯度
    let white_col = [[255, 255, 255], [242, 242, 242], [217, 217, 217], [191, 191, 191], [166, 166, 166], [128, 128, 128]];
    let black_col = [[0, 0, 0], [128, 128, 128], [89, 89, 89], [64, 64, 64], [38, 38, 38], [13, 13, 13]];
    for col in 0..10 {
        for row in 0..6 {
            out[row][col] = if col == 0 {
                white_col[row]
            } else if col == 1 {
                black_col[row]
            } else {
                let base = WPS_THEME_BASES[col];
                match row {
                    0 => base,
                    1 => lerp_rgb(base, [255, 255, 255], 0.80),
                    2 => lerp_rgb(base, [255, 255, 255], 0.60),
                    3 => lerp_rgb(base, [255, 255, 255], 0.40),
                    4 => lerp_rgb(base, [0, 0, 0], 0.25),
                    _ => lerp_rgb(base, [0, 0, 0], 0.50),
                }
            };
        }
    }
    out
}

/// 调色板中的“无填充”或“自动颜色”按钮。
pub(crate) fn palette_reset(ui: &mut egui::Ui, lbl: &str, selected: bool) -> egui::Response {
    let pal = crate::theme::palette();
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width().min(216.0), 24.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 4.0, pal.hover);
    }
    let sw = Rect::from_min_size(egui::pos2(r.left() + 6.0, r.center().y - 7.0), Vec2::splat(14.0));
    ui.painter().rect(
        sw,
        2.0,
        Color32::WHITE,
        Stroke::new(1.0, pal.card_border),
        egui::StrokeKind::Inside,
    );
    if lbl.contains("无") {
        ui.painter().line_segment(
            [sw.left_bottom(), sw.right_top()],
            Stroke::new(1.2, Color32::from_rgb(200, 50, 50)),
        );
    } else {
        ui.painter().rect_filled(sw.shrink(2.0), 1.0, Color32::BLACK);
    }
    ui.painter().text(
        egui::pos2(sw.right() + 8.0, r.center().y),
        Align2::LEFT_CENTER,
        lbl,
        FontId::proportional(12.0),
        pal.text,
    );
    if selected {
        ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
    }
    ui_kit::focus_ring(ui, &resp, r, 4.0);
    ui_kit::a11y_name(&resp, lbl, ui.is_enabled(), Some(selected));
    resp
}

/// 绘制可复用的 WPS 主题色、标准色与最近使用颜色。
pub(crate) fn paint_wps_color_palette(
    ui: &mut egui::Ui,
    selected: Option<[u8; 3]>,
    recent: &[[u8; 3]],
    none_label: Option<&str>,
    clear_clicked: &mut bool,
    picked: &mut Option<[u8; 3]>,
) {
    if let Some(label) = none_label {
        let selected_reset = label.contains("无") && selected.is_none();
        if palette_reset(ui, label, selected_reset).clicked() {
            *clear_clicked = true;
        }
        ui.add_space(4.0);
    }
    let pal = crate::theme::palette();
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.spacing_mut().interact_size.y = 17.0;
        let accessible = ui_kit::is_accesskit_active(ui.ctx());
        let mut color_row = |ui: &mut egui::Ui, colors: &[[u8; 3]]| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for &rgb in colors.iter().take(10) {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                    let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                    let current = selected == Some(rgb);
                    let border = if current || resp.has_focus() || resp.hovered() { pal.accent } else { pal.card_border };
                    ui.painter().rect(rect, 2.0, color, Stroke::new(1.0, border), egui::StrokeKind::Inside);
                    if current {
                        let mark = if egui::Rgba::from(color).intensity() > 0.18 { Color32::BLACK } else { Color32::WHITE };
                        let mid = rect.center();
                        ui.painter().add(egui::Shape::line(vec![
                            mid + egui::vec2(-4.0, 0.0), mid + egui::vec2(-1.0, 3.0), mid + egui::vec2(4.0, -3.0),
                        ], Stroke::new(1.6, mark)));
                    }
                    if resp.has_focus() {
                        ui.painter().rect_stroke(rect.expand(1.0), 3.0, Stroke::new(1.5, pal.accent), egui::StrokeKind::Outside);
                    }
                    if resp.clicked() {
                        *picked = Some(rgb);
                    }
                    if resp.hovered() || accessible {
                        let label = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
                        ui_kit::a11y_name(&resp, &label, ui.is_enabled(), Some(current));
                        resp.on_hover_text(label);
                    }
                }
            });
        };
        ui.label(egui::RichText::new("主题颜色").size(ui_kit::T_SMALL).color(pal.text_weak));
        for (i, row) in wps_theme_matrix().iter().enumerate() {
            if i == 1 { ui.add_space(3.0); }
            color_row(ui, row);
        }
        ui.add_space(5.0);
        ui.label(egui::RichText::new("标准色").size(ui_kit::T_SMALL).color(pal.text_weak));
        color_row(ui, &WPS_STANDARD_COLORS);
        if !recent.is_empty() {
            ui.add_space(5.0);
            ui.label(egui::RichText::new("最近使用").size(ui_kit::T_SMALL).color(pal.text_weak));
            color_row(ui, recent);
        }
    });
}

fn check_outside_dismiss(ctx: &egui::Context, pass_id: egui::Id, rects: &[Rect]) -> bool {
    let cur_pass = ctx.cumulative_pass_nr();
    let opened_pass = ctx.data_mut(|d| {
        let v = d.get_temp::<u64>(pass_id);
        if v.is_none() {
            d.insert_temp::<u64>(pass_id, cur_pass);
        }
        v
    });
    if opened_pass.is_none() || opened_pass == Some(cur_pass) {
        return false;
    }
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        return true;
    }
    if ctx.input(|i| i.pointer.primary_pressed()) {
        if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
            if !rects.iter().any(|r| r.contains(p)) {
                return true;
            }
        }
    }
    false
}

fn clear_popup_pass(ctx: &egui::Context, pass_id: egui::Id) {
    ctx.data_mut(|d| {
        d.remove_temp::<u64>(pass_id);
    });
}

enum FileMenuAction {
    NewBlank,
    Open,
    OpenPath(PathBuf),
    TogglePinRecent(PathBuf),
    RemoveRecent(PathBuf),
    PruneRecent,
    Save,
    SaveAs,
    PrintPreview,
    ExportPdf,
    ExportCleanCopy,
    AuditLog,
    Encrypt,
    Protect,
    Share,
    BackstageInfo,
    GoHome,
    CloseDoc,
    Options,
    CheckUpdate,
    DefaultApp,
}

fn menu_sep_line(ui: &mut egui::Ui) {
    let pal = crate::theme::palette();
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(16.0), 7.0), Sense::hover());
    ui.painter().line_segment(
        [egui::pos2(r.left() + 6.0, r.center().y), egui::pos2(r.right() - 6.0, r.center().y)],
        Stroke::new(1.0, pal.card_border),
    );
}

fn scrollable_file_menu(ui: &mut egui::Ui, width: f32, max_height: f32, content: impl FnOnce(&mut egui::Ui)) {
    let frame = ui_kit::popup_frame(ui.style());
    let margin = frame.total_margin().sum();
    frame.show(ui, |ui| {
        ui.set_width(width - margin.x);
        egui::ScrollArea::vertical()
            .max_height((max_height - margin.y).max(28.0))
            .auto_shrink([false, true])
            .show(ui, content);
    });
}

const DOC_TAB_MIN: f32 = 84.0;
const DOC_TAB_GAP: f32 = 3.0;
const DOC_ACTION_SLOT: f32 = 28.0;

struct SheetTabDrag {
    doc_uid: u64,
    sheet_id: u64,
}

struct SheetTabLayout {
    index: usize,
    label: std::sync::Arc<egui::Galley>,
    start: f32,
}

impl SheetTabLayout {
    fn width(&self) -> f32 {
        self.label.size().x + tokens::size::SHEET_TAB_PAD
    }
}

/// Reserve the new-document button and, when needed, the document switcher.
/// Keep the active tab in the visible run even when it is the last of many.
fn document_tab_layout(count: usize, active: usize, available: f32) -> (std::ops::Range<usize>, f32) {
    let mut budget = (available - DOC_ACTION_SLOT).max(0.0);
    if count as f32 * (DOC_TAB_MIN + DOC_TAB_GAP) > budget {
        budget = (budget - DOC_ACTION_SLOT).max(0.0);
    }
    let slots = count.min((budget / (DOC_TAB_MIN + DOC_TAB_GAP)) as usize);
    if slots == 0 {
        return (0..0, DOC_TAB_MIN);
    }
    let end = (active.saturating_add(1)).clamp(slots, count);
    let max_width = if count <= 2 { 210.0 } else { 180.0 };
    (end - slots..end, (budget / slots as f32 - DOC_TAB_GAP).clamp(DOC_TAB_MIN, max_width))
}

#[cfg(test)]
#[test]
fn document_tabs_keep_active_workbook_and_actions_in_bounds() {
    for count in 1..=16 {
        for active in 0..count {
            for available in [90.0, 160.0, 260.0, 430.0, 960.0] {
                let (visible, width) = document_tab_layout(count, active, available);
                assert!(visible.is_empty() || visible.contains(&active));
                let actions = if visible.len() < count { 2.0 } else { 1.0 };
                let used = visible.len() as f32 * (width + DOC_TAB_GAP) + actions * DOC_ACTION_SLOT;
                assert!(used <= available + 0.01);
                if count as f32 * (DOC_TAB_MIN + DOC_TAB_GAP) + DOC_ACTION_SLOT <= available {
                    assert_eq!(visible, 0..count);
                }
            }
        }
    }
}

#[cfg(test)]
mod sheet_tab_tests {
    use super::*;

    fn frame(ctx: &egui::Context, app: &mut SheetApp, events: Vec<egui::Event>) -> egui::FullOutput {
        ctx.run(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(760.0, 420.0))),
            events, ..Default::default()
        }, |ctx| app.draw_wps_bottom_bar(ctx))
    }

    fn button_event(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
            pos, pressed, button: egui::PointerButton::Primary, modifiers: egui::Modifiers::NONE,
        }]
    }

    #[test]
    fn title_search_opens_from_its_icon_and_keyboard_without_stealing_tab_focus() {
        let _env = crate::test_env::TestEnv::new("title-command-search");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.show_start = false;
        let draw = |app: &mut SheetApp, events| {
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1024.0, 690.0))),
                events, ..Default::default()
            }, |ctx| app.draw_wps_title_bar(ctx));
        };
        draw(&mut app, vec![]);
        let id = egui::Id::new("title_command_search");
        let rect = ctx.read_response(id).unwrap().rect;
        for pos in [rect.left_center() + Vec2::new(15.0, 0.0), rect.right_center() - Vec2::new(9.0, 0.0)] {
            draw(&mut app, button_event(pos, true));
            draw(&mut app, button_event(pos, false));
            assert!(app.palette_open);
            app.palette_open = false;
        }
        ctx.memory_mut(|m| m.request_focus(id));
        draw(&mut app, vec![]);
        assert!(!app.palette_open);
        draw(&mut app, vec![egui::Event::Key {
            key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
        }]);
        assert!(app.palette_open);
    }

    fn text_rect(out: &egui::FullOutput, label: &str) -> Option<(Rect, Rect, bool)> {
        out.shapes.iter().find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(t) if t.galley.job.text == label => {
                Some((Rect::from_min_size(t.pos, t.galley.size()), clipped.clip_rect, t.galley.elided))
            }
            _ => None,
        })
    }

    #[test]
    fn sheet_tab_strip_reveals_active_sheet_and_keeps_overflow_controls_reachable() {
        let _env = crate::test_env::TestEnv::new("sheet-tab-strip");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.new_document();
        let first = frame(&ctx, &mut app, Vec::new());
        let name = &app.wb.sheets[0].name;
        let (first_text, _, _) = text_rect(&first, name).unwrap();
        let plus = ctx.read_response(egui::Id::new("newsheet")).unwrap().rect;
        assert!((0.0..=20.0).contains(&(plus.left() - first_text.right())));
        assert!(ctx.read_response(egui::Id::new("sheet_list_button")).is_none());

        for i in 1..12 {
            app.wb.sheets.push(sheetcore::Sheet::new(&format!("长表名用于核对不同地区每月收入与支出明细归档工作表{i}")));
        }
        app.wb.sheets[10].hidden = true;
        app.switch_sheet_relative(-1);
        assert_eq!(app.sheet_idx, 11);
        let last = frame(&ctx, &mut app, Vec::new());
        let (text, clip, elided) = text_rect(&last, &app.wb.sheets[11].name).unwrap();
        assert!(elided && clip.contains_rect(text));
        assert!(app.tab_scroll > 0.0);
        assert!(text_rect(&last, &app.wb.sheets[10].name).is_none());
        let list = ctx.read_response(egui::Id::new("sheet_list_button")).unwrap().rect;
        let left = ctx.read_response(egui::Id::new(("tabscroll", 0usize))).unwrap().rect.center();
        let old_offset = app.tab_scroll;
        frame(&ctx, &mut app, button_event(left, true));
        frame(&ctx, &mut app, button_event(left, false));
        frame(&ctx, &mut app, Vec::new());
        assert!(app.tab_scroll < old_offset, "manual browsing must survive the next frame");
        assert_eq!(app.sheet_idx, 11);
        frame(&ctx, &mut app, button_event(list.center(), true));
        frame(&ctx, &mut app, button_event(list.center(), false));
        assert!(app.sheet_list_open);
        app.sheet_list_open = false;
        app.switch_sheet_relative(1);
        let home = frame(&ctx, &mut app, Vec::new());
        let (text, clip, _) = text_rect(&home, &app.wb.sheets[0].name).unwrap();
        assert!(clip.contains_rect(text));
        assert_eq!(app.tab_scroll, 0.0);
        let count = app.wb.sheets.len();
        let plus = ctx.read_response(egui::Id::new("newsheet")).unwrap().rect.center();
        frame(&ctx, &mut app, button_event(plus, true));
        frame(&ctx, &mut app, button_event(plus, false));
        assert_eq!(app.wb.sheets.len(), count + 1);
    }

    #[test]
    fn sheet_tab_list_click_preserves_formula_picking_and_plain_click_group_behavior() {
        let env = crate::test_env::TestEnv::new("sheet-tab-list-pick");
        env.sc("SC_AUTO_SHARE", "0");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.new_document();
        app.wb.sheets = ["一月", "二月", "三月"].map(sheetcore::Sheet::new).into();
        let pick = |app: &mut SheetApp, name: &str| {
            let draw = |app: &mut SheetApp, events| {
                ctx.run(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(760.0, 420.0))),
                    events, ..Default::default()
                }, |ctx| eframe::App::update(app, ctx, &mut eframe::Frame::_new_kittest()))
            };
            app.sheet_list_open = true;
            draw(app, vec![]);
            let out = draw(app, vec![]);
            let text = out.shapes.iter().find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == name && t.pos.y < 350.0 => {
                    Some(Rect::from_min_size(t.pos, t.galley.size()))
                }
                _ => None,
            }).expect("visible dialog row");
            // Click well past the short label, inside the row.
            let pos = egui::pos2(text.left() + 160.0, text.center().y);
            draw(app, button_event(pos, true));
            draw(app, button_event(pos, false));
            assert!(!app.sheet_list_open);
            draw(app, vec![]);
        };

        app.selected_sheets.extend([0, 1]);
        pick(&mut app, "二月");
        assert_eq!(app.sheet_idx, 1);
        assert_eq!(app.selected_sheets.len(), 2);
        pick(&mut app, "三月");
        assert_eq!(app.sheet_idx, 2);
        assert!(app.selected_sheets.is_empty());

        app.begin_edit(Some("=1+".into()));
        pick(&mut app, "一月");
        assert_eq!(app.sheet_idx, 0);
        assert_eq!(app.edit_sheet, Some(2));
        assert_eq!(app.editing.as_ref().unwrap().buf, "=1+");
        assert!(app.status.contains("点选引用"));
        app.insert_pick_reference(4, 2);
        assert!(app.editing.as_ref().unwrap().buf.ends_with("!C5"));
    }

    #[test]
    fn grouped_sheet_tabs_mark_the_active_sheet_separately_from_other_members() {
        let _env = crate::test_env::TestEnv::new("sheet-tab-group");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.new_document();
        app.wb.sheets = ["一月", "二月", "三月"].map(sheetcore::Sheet::new).into();
        app.selected_sheets.extend([0, 1]);
        app.wb.sheets[1].tab_color = Some([230, 81, 0]);
        for active_custom in [None, Some([255, 255, 255])] {
            app.wb.sheets[0].tab_color = active_custom;
            for theme in crate::theme::AppTheme::ALL {
                crate::theme::apply_app_theme(&ctx, theme);
                let pal = crate::theme::palette();
                let (active_fill, active_ink) = ui_kit::active_tab_colors(pal);
                let out = frame(&ctx, &mut app, Vec::new());
                let tab = |name| text_rect(&out, name).unwrap().0;
                let has_face = |name, color| out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r)
                    if r.fill == color && r.rect.contains_rect(tab(name)) && r.rect.height() <= tokens::size::SHEET_TAB_H));
                assert!(has_face("一月", active_fill));
                assert!(has_face("二月", pal.accent_tint));
                assert!(!has_face("三月", active_fill) && !has_face("三月", pal.accent_tint));
                assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(t)
                    if t.galley.job.text == "一月" && t.override_text_color == Some(active_ink))));
                let expected_bar = active_custom
                    .map(|[r, g, b]| Color32::from_rgb(r, g, b))
                    .unwrap_or(pal.accent);
                let t1 = tab("一月");
                assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r)
                    if r.fill == expected_bar
                        && r.rect.width() >= 14.0
                        && r.rect.height() <= 2.5
                        && r.rect.x_range().contains(t1.center().x))));
                // Grouped inactive member must not get the active bottom bar.
                let t2 = tab("二月");
                assert!(!out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r)
                    if r.fill == pal.accent
                        && r.rect.width() >= 14.0
                        && r.rect.height() <= 2.5
                        && r.rect.x_range().contains(t2.center().x))));
                for color in active_custom
                    .into_iter()
                    .map(|[r, g, b]| Color32::from_rgb(r, g, b))
                    .chain(std::iter::once(Color32::from_rgb(230, 81, 0)))
                {
                    assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r)
                        if r.fill == color && r.rect.size() == Vec2::splat(8.0))));
                }
            }
        }
    }

    #[test]
    fn sheet_tab_drag_moves_once_on_drop_and_respects_structure_protection() {
        let _env = crate::test_env::TestEnv::new("sheet-tab-drop");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.new_document();
        app.wb.sheets = ["A", "B", "C", "D"].map(sheetcore::Sheet::new).into();
        let before = frame(&ctx, &mut app, Vec::new());
        let center = |out: &egui::FullOutput, name: &str| text_rect(out, name).unwrap().0.center();
        let a = center(&before, "A");
        let c = center(&before, "C");
        let names = |app: &SheetApp| app.wb.sheets.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        let drag_to_c = |app: &mut SheetApp| {
            frame(&ctx, app, button_event(a, true));
            frame(&ctx, app, vec![egui::Event::PointerMoved(a + Vec2::new(9.0, 0.0))]);
            frame(&ctx, app, vec![egui::Event::PointerMoved(c)]);
        };
        drag_to_c(&mut app);
        assert_eq!(names(&app), ["A", "B", "C", "D"]);
        frame(&ctx, &mut app, button_event(egui::pos2(20.0, 60.0), false));
        assert_eq!(names(&app), ["A", "B", "C", "D"], "drop outside the strip cancels");
        drag_to_c(&mut app);
        frame(&ctx, &mut app, button_event(c, false));
        assert_eq!(names(&app), ["B", "C", "A", "D"]);

        app.wb.protection = Some(sheetcore::protection::WorkbookProtection { lock_structure: true, ..Default::default() });
        let locked = frame(&ctx, &mut app, Vec::new());
        let from = center(&locked, "B");
        let to = center(&locked, "A");
        frame(&ctx, &mut app, button_event(from, true));
        frame(&ctx, &mut app, vec![egui::Event::PointerMoved(to)]);
        frame(&ctx, &mut app, button_event(to, false));
        let plus = ctx.read_response(egui::Id::new("newsheet")).unwrap().rect.center();
        frame(&ctx, &mut app, button_event(plus, true));
        frame(&ctx, &mut app, button_event(plus, false));
        assert_eq!(names(&app), ["B", "C", "A", "D"]);
    }
}

impl SheetApp {
    /// WPS 轻量级「文件」下拉菜单（点「文件」弹出，不遮挡整窗；含文档信息入口进 Backstage）
    pub(crate) fn draw_file_menu(&mut self, ctx: &egui::Context) {
        let pass_id = egui::Id::new("wps_file_menu_pass");
        let sub_id = egui::Id::new("wps_file_menu_sub");
        if !self.file_menu_open {
            clear_popup_pass(ctx, pass_id);
            ctx.data_mut(|d| d.remove_temp::<u8>(sub_id));
            return;
        }

        let screen = ctx.screen_rect();
        let anchor = self
            .file_menu_anchor
            .unwrap_or_else(|| egui::pos2(screen.left() + 12.0, ui_kit::TITLE_H + ui_kit::TAB_H));
        let menu_w = 248.0;
        let pos = egui::pos2(
            anchor.x.clamp(screen.left() + 4.0, (screen.right() - menu_w - 8.0).max(4.0)),
            anchor.y.clamp(screen.top() + 4.0, (screen.bottom() - 56.0).max(screen.top() + 4.0)),
        );

        let mut act: Option<FileMenuAction> = None;
        let mut active_sub = ctx.data_mut(|d| d.get_temp::<u8>(sub_id).unwrap_or(0));
        let mut rects = Vec::new();
        let mut sub_anchor_y = pos.y + 42.0;

        let pal = crate::theme::palette();
        let main_resp = egui::Area::new(egui::Id::new("wps_file_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                scrollable_file_menu(ui, menu_w, screen.bottom() - pos.y - 6.0, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);

                        let row = |ui: &mut egui::Ui,
                                   icon: Icon,
                                   label: &str,
                                   shortcut: &str,
                                   has_sub: bool,
                                   sub_on: bool|
                         -> egui::Response {
                            let (r, resp) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 28.0),
                                Sense::click(),
                            );
                            if resp.hovered() || sub_on {
                                ui.painter().rect_filled(r, 5.0, pal.hover);
                            }
                            ui_icons::draw_excel(
                                ui.painter(),
                                Rect::from_center_size(egui::pos2(r.left() + 14.0, r.center().y), Vec2::splat(15.0)),
                                icon,
                                pal.text,
                                true,
                            );
                            ui.painter().text(
                                egui::pos2(r.left() + 30.0, r.center().y),
                                Align2::LEFT_CENTER,
                                label,
                                FontId::proportional(ui_kit::T_BODY),
                                pal.text,
                            );
                            if !shortcut.is_empty() {
                                ui.painter().text(
                                    egui::pos2(r.right() - 10.0, r.center().y),
                                    Align2::RIGHT_CENTER,
                                    shortcut,
                                    FontId::proportional(11.0),
                                    pal.text_weak,
                                );
                            }
                            if has_sub {
                                ui_kit::caret_right(
                                    ui,
                                    egui::pos2(r.right() - 10.0, r.center().y),
                                    pal.text_weak,
                                );
                            }
                            ui_kit::focus_ring(ui, &resp, r, 5.0);
                            ui_kit::a11y_role(&resp, label, egui::accesskit::Role::MenuItem, None);
                            resp
                        };

                        if row(ui, Icon::NewFile, "新建空白工作簿", "Ctrl+N", false, false).clicked() {
                            act = Some(FileMenuAction::NewBlank);
                        }
                        if row(ui, Icon::Open, "打开…", "Ctrl+O", false, false).clicked() {
                            act = Some(FileMenuAction::Open);
                        }
                        let rec_r = row(ui, Icon::FolderPlus, "最近打开", "", true, active_sub == 2);
                        sub_anchor_y = rec_r.rect.top().max(ui.clip_rect().top());
                        if rec_r.hovered() || rec_r.clicked() {
                            active_sub = 2;
                        } else if !ui.is_rect_visible(rec_r.rect)
                            || ui.rect_contains_pointer(ui.clip_rect()) && ui.ctx().pointer_hover_pos().is_some_and(|p| !rec_r.rect.y_range().contains(p.y)) {
                            active_sub = 0;
                        }

                        menu_sep_line(ui);

                        if row(ui, Icon::Save, "保存", "Ctrl+S", false, false).clicked() {
                            act = Some(FileMenuAction::Save);
                        }
                        if row(ui, Icon::SaveAs, "另存为…", "F12", false, false).clicked() {
                            act = Some(FileMenuAction::SaveAs);
                        }

                        menu_sep_line(ui);

                        if row(ui, Icon::Print, "打印 / 打印预览", "Ctrl+P", false, false).clicked() {
                            act = Some(FileMenuAction::PrintPreview);
                        }
                        if row(ui, Icon::PdfExport, "导出为 PDF…", "", false, false).clicked() {
                            act = Some(FileMenuAction::ExportPdf);
                        }
                        if row(ui, Icon::SaveAs, "导出对外干净副本 (.xlsx)…", "", false, false).clicked() {
                            act = Some(FileMenuAction::ExportCleanCopy);
                        }

                        menu_sep_line(ui);

                        if row(ui, Icon::Lock, "防篡改审计记录…", "", false, false).clicked() {
                            act = Some(FileMenuAction::AuditLog);
                        }
                        if row(ui, Icon::Lock, "加密文档", "", false, false).clicked() {
                            act = Some(FileMenuAction::Encrypt);
                        }
                        if row(ui, Icon::Pin, "保护工作表", "", false, false).clicked() {
                            act = Some(FileMenuAction::Protect);
                        }
                        if row(ui, Icon::Share, "共享 / 局域网协作", "", false, false).clicked() {
                            act = Some(FileMenuAction::Share);
                        }

                        menu_sep_line(ui);

                        if row(ui, Icon::About, "文档信息与属性…", "", false, false).clicked() {
                            act = Some(FileMenuAction::BackstageInfo);
                        }
                        if row(ui, Icon::Home, "返回开始主页", "", false, false).clicked() {
                            act = Some(FileMenuAction::GoHome);
                        }
                        if row(ui, Icon::Close, "关闭当前文档", "Ctrl+W", false, false).clicked() {
                            act = Some(FileMenuAction::CloseDoc);
                        }

                        menu_sep_line(ui);

                        if row(ui, Icon::UpdateCheck, "检查更新", "", false, false).clicked() {
                            act = Some(FileMenuAction::CheckUpdate);
                        }
                        if row(ui, Icon::PivotOptions, "选项…", "", false, false).clicked() {
                            act = Some(FileMenuAction::Options);
                        }
                        if row(ui, Icon::AppDefault, "设为默认电子表格应用", "", false, false).clicked() {
                            act = Some(FileMenuAction::DefaultApp);
                        }
                    });
            });
        rects.push(main_resp.response.rect);

        // 级联子菜单（最近打开：支持固定/取消固定、移除单条与手动清理失效记录）
        if active_sub == 2 {
            let sub_w = 340.0;
            let sub_x = if pos.x + menu_w + sub_w + 8.0 <= screen.right() {
                pos.x + menu_w - 2.0
            } else {
                (pos.x - sub_w + 2.0).max(4.0)
            };
            let sub_y = sub_anchor_y.clamp(screen.top() + 4.0, (screen.bottom() - 280.0).max(4.0));
            let rec_entries = self.ensure_recent_cache().to_vec();
            let pal = crate::theme::palette();
            let sub_resp = egui::Area::new(egui::Id::new("wps_file_menu_cascade"))
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(sub_x, sub_y))
                .show(ctx, |ui| {
                    scrollable_file_menu(ui, sub_w, screen.bottom() - sub_y - 6.0, |ui| {
                            if rec_entries.is_empty() {
                                ui.label(egui::RichText::new("暂无最近打开的文件").size(12.0).color(pal.text_weak));
                            } else {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                for (idx, ent) in rec_entries.into_iter().take(12).enumerate() {
                                    let row_w = ui.available_width();
                                    let (row, _) = ui.allocate_exact_size(Vec2::new(row_w, 28.0), Sense::hover());
                                    let rm_btn = Rect::from_center_size(
                                        egui::pos2(row.right() - 14.0, row.center().y),
                                        Vec2::splat(22.0),
                                    );
                                    let pin_btn = Rect::from_center_size(
                                        egui::pos2(row.right() - 38.0, row.center().y),
                                        Vec2::splat(22.0),
                                    );
                                    let open_rect = Rect::from_min_max(
                                        row.min,
                                        egui::pos2((pin_btn.left() - 4.0).max(row.left() + 16.0), row.bottom()),
                                    );
                                    let open_resp = ui.interact(open_rect, ui.id().with(("recent_open", idx)), Sense::click());
                                    let pin_resp = ui.interact(pin_btn, ui.id().with(("recent_pin", idx)), Sense::click());
                                    let rm_resp = ui.interact(rm_btn, ui.id().with(("recent_rm", idx)), Sense::click());
                                    if open_resp.is_pointer_button_down_on() {
                                        ui.painter().rect_filled(row, 5.0, pal.pressed);
                                    } else if open_resp.hovered() || pin_resp.hovered() || rm_resp.hovered() {
                                        ui.painter().rect_filled(row, 5.0, pal.hover);
                                    }
                                    let label = format!("{}  ({})", ent.name, ent.short_dir);
                                    let text_rect = Rect::from_min_max(
                                        egui::pos2(open_rect.left() + 8.0, open_rect.top()),
                                        egui::pos2((open_rect.right() - 4.0).max(open_rect.left() + 8.0), open_rect.bottom()),
                                    );
                                    let mut job = egui::text::LayoutJob::simple_singleline(
                                        label,
                                        FontId::proportional(ui_kit::T_BODY),
                                        pal.text,
                                    );
                                    job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width().max(1.0));
                                    let galley = ui.painter().layout_job(job);
                                    let tpos = Align2::LEFT_CENTER
                                        .anchor_size(egui::pos2(text_rect.left(), text_rect.center().y), galley.size())
                                        .min;
                                    ui.painter_at(text_rect).galley(tpos, galley, pal.text);
                                    ui_kit::focus_ring(ui, &open_resp, open_rect, 4.0);
                                    ui_kit::a11y_name(&open_resp, &ent.name, true, None);
                                    if open_resp.on_hover_text(ent.path.display().to_string()).clicked() {
                                        act = Some(FileMenuAction::OpenPath(ent.path.clone()));
                                    }

                                    let pin_tip = if ent.pinned { "取消固定" } else { "固定到顶部" };
                                    if pin_resp.hovered() {
                                        ui.painter().rect_filled(pin_btn, 4.0, pal.pressed);
                                    }
                                    ui_icons::draw_mono(
                                        ui.painter(),
                                        Rect::from_center_size(pin_btn.center(), Vec2::splat(14.0)),
                                        Icon::Pin,
                                        if ent.pinned { pal.accent } else { pal.text_weak },
                                    );
                                    ui_kit::focus_ring(ui, &pin_resp, pin_btn, 4.0);
                                    ui_kit::a11y_name(&pin_resp, pin_tip, true, Some(ent.pinned));
                                    if pin_resp.on_hover_text(pin_tip).clicked() {
                                        act = Some(FileMenuAction::TogglePinRecent(ent.path.clone()));
                                    }

                                    if rm_resp.hovered() {
                                        ui.painter().rect_filled(rm_btn, 4.0, pal.err_bg);
                                    }
                                    ui_icons::draw_mono(
                                        ui.painter(),
                                        Rect::from_center_size(rm_btn.center(), Vec2::splat(12.0)),
                                        Icon::Close,
                                        if rm_resp.hovered() { pal.err_fg } else { pal.text_weak },
                                    );
                                    ui_kit::focus_ring(ui, &rm_resp, rm_btn, 4.0);
                                    ui_kit::a11y_name(&rm_resp, "从最近列表移除", true, None);
                                    if rm_resp.on_hover_text("从最近列表移除").clicked() {
                                        act = Some(FileMenuAction::RemoveRecent(ent.path.clone()));
                                    }
                                }
                                menu_sep_line(ui);
                                if ui_kit::popup_item(ui, "清理已删除的本地文件记录", true, ui.available_width()).clicked() {
                                    act = Some(FileMenuAction::PruneRecent);
                                }
                            }
                        });
                });
            rects.push(sub_resp.response.rect);
        }

        ctx.data_mut(|d| d.insert_temp::<u8>(sub_id, active_sub));

        if let Some(a) = act {
            match a {
                FileMenuAction::TogglePinRecent(p) => {
                    crate::recent::toggle_pin(&p);
                    self.recent_cache = None;
                    self.tree.set_recent(crate::recent::load());
                    return;
                }
                FileMenuAction::RemoveRecent(p) => {
                    crate::recent::forget(&p);
                    self.recent_cache = None;
                    self.tree.set_recent(crate::recent::load());
                    return;
                }
                FileMenuAction::PruneRecent => {
                    let removed = crate::recent::prune_missing_manual();
                    self.recent_cache = None;
                    self.tree.set_recent(crate::recent::load());
                    self.status = if removed > 0 {
                        format!("已清理 {removed} 条失效的本地文件记录（网络共享盘路径已保留）")
                    } else {
                        "没有可清理的失效本地文件记录".into()
                    };
                    return;
                }
                _ => {}
            }
            self.file_menu_open = false;
            match a {
                FileMenuAction::NewBlank => self.new_document(),
                FileMenuAction::Open => self.open_dialog(),
                FileMenuAction::OpenPath(p) => {
                    self.show_start = false;
                    self.open_any(p);
                }
                FileMenuAction::Save => self.save(false),
                FileMenuAction::SaveAs => self.save(true),
                FileMenuAction::PrintPreview => {
                    self.preview_page = 0;
                    self.print_preview = true;
                }
                FileMenuAction::ExportPdf => self.export_pdf_dialog(),
                FileMenuAction::ExportCleanCopy => self.export_clean_copy_dialog(),
                FileMenuAction::AuditLog => self.open_audit_panel(),
                FileMenuAction::Encrypt => {
                    self.enc_dlg = Some((String::new(), String::new(), String::new()));
                }
                FileMenuAction::Protect => {
                    self.open_protect_sheet_dialog();
                }
                FileMenuAction::Share => {
                    if self.net.is_none() {
                        self.start_host();
                    }
                    self.members_open = true;
                }
                FileMenuAction::BackstageInfo => {
                    self.backstage = true;
                    self.backstage_page = 0;
                }
                FileMenuAction::GoHome => {
                    if self.finish_pending_edit() {
                        self.show_start = true;
                    }
                }
                FileMenuAction::CloseDoc => {
                    let idx = self.active_tab;
                    self.request_close_tab(idx);
                }
                FileMenuAction::Options => {
                    self.options_dlg_open = true;
                }
                FileMenuAction::CheckUpdate => self.check_update(),
                FileMenuAction::DefaultApp => self.register_file_association(true),
                FileMenuAction::TogglePinRecent(_)
                | FileMenuAction::RemoveRecent(_)
                | FileMenuAction::PruneRecent => {}
            }
            return;
        }

        if check_outside_dismiss(ctx, pass_id, &rects) {
            self.file_menu_open = false;
        }
    }

    /// WPS 锚定调色盘：10×6 主题色 + 10 标准色 + 最近使用 + 无填充/自动黑 + 自定义 RGB
    pub(crate) fn draw_wps_color_popup(&mut self, ctx: &egui::Context) {
        let pass_id = egui::Id::new("wps_color_popup_pass");
        let custom_id = egui::Id::new("wps_color_custom_rgb");
        let Some(for_font) = self.color_popup else {
            clear_popup_pass(ctx, pass_id);
            ctx.data_mut(|d| d.remove_temp::<bool>(custom_id));
            return;
        };

        let screen = ctx.screen_rect();
        let w = 228.0;
        let anchor = self
            .color_popup_pos
            .unwrap_or_else(|| egui::pos2(screen.center().x - w * 0.5, ui_kit::TITLE_H + ui_kit::RIBBON_H));
        let pos = egui::pos2(
            anchor.x.clamp(screen.left() + 6.0, (screen.right() - w - 8.0).max(6.0)),
            anchor.y.clamp(screen.top() + 6.0, (screen.bottom() - 320.0).max(6.0)),
        );

        let cur_rgb = if for_font {
            Some(self.current_style().color)
        } else {
            self.current_style().fill
        };
        let mut clear_clicked = false;
        let mut picked: Option<[u8; 3]> = None;
        let mut show_custom = ctx.data_mut(|d| d.get_temp::<bool>(custom_id).unwrap_or(false));
        let recent = self.recent_colors.clone();

        let pal = crate::theme::palette();
        let area_resp = egui::Area::new(egui::Id::new("wps_color_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(pal.dialog_bg)
                    .stroke(Stroke::new(1.0, pal.card_border))
                    .corner_radius(8.0)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 6],
                        blur: 16,
                        spread: 0,
                        color: Color32::from_black_alpha(30),
                    })
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(w - 20.0);
                        paint_wps_color_palette(
                            ui,
                            cur_rgb,
                            &recent,
                            Some(if for_font { "自动颜色（黑色）" } else { "无填充颜色" }),
                            &mut clear_clicked,
                            &mut picked,
                        );
                        ui.add_space(6.0);
                        menu_sep_line(ui);
                        if ui_kit::popup_item(
                            ui,
                            if show_custom { "- 收起自定义颜色" } else { "+ 更多自定义颜色…" },
                            true,
                            ui.available_width(),
                        )
                        .clicked()
                        {
                            show_custom = !show_custom;
                            ctx.data_mut(|d| d.insert_temp::<bool>(custom_id, show_custom));
                        }
                        if show_custom {
                            let rgb_state_id = egui::Id::new("wps_color_custom_val");
                            let mut cur = ctx.data_mut(|d| {
                                d.get_temp::<[u8; 3]>(rgb_state_id)
                                    .unwrap_or(cur_rgb.unwrap_or([255, 242, 0]))
                            });
                            let mut c32 = Color32::from_rgb(cur[0], cur[1], cur[2]);
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("自选：").size(12.0));
                                if ui.color_edit_button_srgba(&mut c32).changed() {
                                    cur = [c32.r(), c32.g(), c32.b()];
                                    ctx.data_mut(|d| d.insert_temp::<[u8; 3]>(rgb_state_id, cur));
                                }
                                if ui_kit::secondary_button(ui, "应用").clicked() {
                                    picked = Some(cur);
                                }
                            });
                        }
                    });
            });

        if clear_clicked {
            if for_font {
                self.apply_font_color([0, 0, 0]);
            } else {
                self.apply_fill_color(None);
            }
            self.color_popup = None;
            return;
        }
        if let Some(rgb) = picked {
            if for_font {
                self.apply_font_color(rgb);
            } else {
                self.apply_fill_color(Some(rgb));
            }
            self.color_popup = None;
            return;
        }
        if check_outside_dismiss(ctx, pass_id, &[area_resp.response.rect]) {
            self.color_popup = None;
        }
    }

    /// WPS 38px 第 1 排顶栏：Logo + 单一「⌂ 主页」胶囊 + 文档标签条（+ 新建）+ 命令搜索 + 账户 + 窗口按钮
    pub(crate) fn draw_wps_title_bar(&mut self, ctx: &egui::Context) {
        let pal = crate::theme::palette();
        let bg = pal.title_bg;
        let fg = pal.text;
        let hover = pal.hover_on_page;

        let mut tab_metas: Vec<(String, bool, bool)> = Vec::new();
        if self.has_open_workbook {
            for i in 0..self.tabs.len() {
                let mut title = self.tab_title(i);
                if i == self.active_tab && self.selected_sheets.len() > 1 {
                    title.push_str(" [工作组]");
                }
                let dirty = match &self.tabs[i] {
                    TabSlot::Active => self.wb.dirty || self.tasks.saving.is_some() || self.editing.is_some(),
                    TabSlot::Parked(d) => d.wb.dirty || d.tasks.saving.is_some(),
                };
                tab_metas.push((title, dirty, !self.show_start && i == self.active_tab));
            }
        }
        let mut do_switch: Option<usize> = None;
        let mut do_close: Option<usize> = None;
        let mut new_tab = false;

        egui::TopBottomPanel::top("titlebar")
            .show_separator_line(false)
            .exact_height(ui_kit::TITLE_H)
            .frame(egui::Frame::NONE.fill(bg))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter().clone();
                if self.show_start && !self.has_open_workbook {
                    let rail_w = crate::home_rail_width(rect.width());
                    let canvas_bg = pal.page_bg;
                    painter.rect_filled(
                        Rect::from_min_max(egui::pos2(rect.left() + rail_w, rect.top()), rect.right_bottom()),
                        0.0,
                        canvas_bg,
                    );
                    painter.rect_filled(
                        Rect::from_min_max(egui::pos2(rect.left() + rail_w - 1.0, rect.top()), egui::pos2(rect.left() + rail_w, rect.bottom())),
                        0.0,
                        pal.card_border,
                    );
                }

                // 1. Logo + 「⌂ 主页」页签（只要有已打开工作簿，即使正在浏览主页也始终保留主页与全部文档标签！）
                let logo_cx = if self.show_start && !self.has_open_workbook {
                    rect.left() + 31.0
                } else {
                    rect.left() + 20.0
                };
                let ir = Rect::from_center_size(egui::pos2(logo_cx, rect.center().y), Vec2::splat(20.0));
                ui_kit::draw_brand_logo(&painter, ir);

                let mut left_end = rect.left() + 46.0;
                if self.has_open_workbook {
                    let home_active = self.show_start;
                    let hb = if home_active {
                        Rect::from_min_size(egui::pos2(rect.left() + 38.0, rect.top() + 6.0), Vec2::new(62.0, rect.height() - 6.0))
                    } else {
                        Rect::from_min_size(egui::pos2(rect.left() + 38.0, rect.center().y - 12.0), Vec2::new(58.0, 24.0))
                    };
                    let hresp = ui.interact(hb, egui::Id::new("title_home_btn"), Sense::click());
                    ui_kit::a11y_role(&hresp, "主页", egui::accesskit::Role::Tab, Some(home_active));
                    if home_active {
                        let rad = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
                        painter.rect(
                            hb,
                            rad,
                            pal.card_bg,
                            Stroke::new(1.0, pal.card_border),
                            egui::StrokeKind::Inside,
                        );
                    } else if hresp.hovered() {
                        painter.rect_filled(hb, 4.0, if hresp.is_pointer_button_down_on() { pal.pressed } else { hover });
                    }
                    let hic = if home_active || hresp.hovered() { fg } else { pal.text_weak };
                    ui_kit::draw_clean_icon(
                        &painter,
                        Rect::from_min_size(egui::pos2(hb.left() + 8.0, hb.center().y - 7.0), Vec2::splat(14.0)),
                        0,
                        hic,
                    );
                    ui_kit::paint_medium(&painter, egui::pos2(hb.left() + 25.0, hb.center().y), Align2::LEFT_CENTER, "主页", ui_kit::T_BODY, hic);
                    ui_kit::focus_ring(ui, &hresp, hb, 5.0);
                    let tip = if home_active {
                        "返回当前已打开的工作簿"
                    } else {
                        "查看开始主页（当前文档保持打开，可随时点右侧文档标签返回）"
                    };
                    if hresp.on_hover_text(tip).clicked() {
                        if home_active {
                            self.show_start = false;
                        } else if self.finish_pending_edit() {
                            self.show_start = true;
                        }
                    }
                    left_end = hb.right() + 6.0;
                }

                // 2. 右侧控件尺寸预计算：窗口三键 + 账户 + 搜索框
                let user = &self.my_name;
                let acc_limit = if rect.width() >= 1200.0 { 184.0 } else { 140.0 };
                let mut name_job = egui::text::LayoutJob::simple_singleline(user.clone(), FontId::proportional(ui_kit::T_BODY), fg);
                name_job.wrap = egui::text::TextWrapping::truncate_at_width(acc_limit - 36.0);
                let name_galley = painter.layout_job(name_job);
                let acc_w = (36.0 + name_galley.size().x).clamp(84.0, acc_limit);
                let acc = Rect::from_min_size(
                    egui::pos2(rect.right() - 46.0 * 3.0 - acc_w - 12.0, rect.center().y - 12.0),
                    Vec2::new(acc_w, 24.0),
                );
                let show_search = rect.width() >= 960.0 && !self.show_start;
                let search_w = (rect.width() * 0.19).clamp(188.0, 260.0);
                let sx = acc.left() - search_w - 10.0;
                let right_limit = if show_search { sx - 10.0 } else { acc.left() - 10.0 };

                // 3. 文档标签直接融入第 1 排（WPS 风格：即使切换到主页也始终保留全部已打开文档标签 + `+`）
                let mut cur_x = left_end;
                if self.has_open_workbook {
                    let (visible, width_limit) = document_tab_layout(tab_metas.len(), self.active_tab, right_limit - cur_x);
                    let overflow = visible.len() < tab_metas.len();
                    for i in visible {
                        let (title, dirty, active) = &tab_metas[i];
                        let font = if *active { ui_kit::medium_in(ctx, ui_kit::T_BODY) } else { FontId::proportional(ui_kit::T_BODY) };
                        let tc = if *active { pal.text } else { pal.text_weak };
                        let mut job = egui::text::LayoutJob::simple_singleline(title.clone(), font, tc);
                        let padding = if *dirty { 42.0 } else { 34.0 };
                        job.wrap = egui::text::TextWrapping::truncate_at_width(width_limit - padding);
                        let galley = painter.layout_job(job);
                        let tw = (galley.size().x + padding).clamp(DOC_TAB_MIN, width_limit);
                        let tab = Rect::from_min_size(egui::pos2(cur_x, rect.top() + 6.0), Vec2::new(tw, rect.height() - 6.0));
                        let close = Rect::from_center_size(egui::pos2(tab.right() - 12.0, tab.center().y), Vec2::splat(22.0));
                        let tab_hit = Rect::from_min_max(tab.min, egui::pos2(close.left() - 2.0, tab.bottom()));
                        let tr = ui.interact(tab_hit, egui::Id::new(("wps_doctab", i)), Sense::click());
                        let cr = ui.interact(close, egui::Id::new(("wps_doctabx", i)), Sense::click());
                        let rad = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
                        if *active {
                            painter.rect(
                                tab,
                                rad,
                                pal.card_bg,
                                Stroke::new(1.0, pal.card_border),
                                egui::StrokeKind::Inside,
                            );
                        } else if tr.hovered() || cr.hovered() {
                            painter.rect_filled(tab, rad, hover);
                        }
                        let text_pos = egui::pos2(tab.left() + 10.0, tab.center().y - galley.size().y / 2.0);
                        painter.galley(text_pos, galley, tc);
                        if *dirty {
                            painter.circle_filled(egui::pos2(tab.right() - 26.0, tab.center().y), 3.0, pal.warn_fg);
                        }
                        if cr.hovered() {
                            let fill = if cr.is_pointer_button_down_on() { pal.pressed } else { pal.hover };
                            painter.rect_filled(close.shrink(2.0), 4.0, fill);
                        }
                        let cc = if cr.hovered() { pal.text } else { pal.text_weak };
                        let cc0 = close.center();
                        painter.line_segment([egui::pos2(cc0.x - 3.2, cc0.y - 3.2), egui::pos2(cc0.x + 3.2, cc0.y + 3.2)], Stroke::new(1.1, cc));
                        painter.line_segment([egui::pos2(cc0.x + 3.2, cc0.y - 3.2), egui::pos2(cc0.x - 3.2, cc0.y + 3.2)], Stroke::new(1.1, cc));
                        let tab_tip = if *dirty {
                            format!("{title}（有未保存的修改）")
                        } else {
                            title.clone()
                        };
                        ui_kit::a11y_role(&tr, &tab_tip, egui::accesskit::Role::Tab, Some(*active));
                        if ui_kit::is_accesskit_active(ctx) {
                            ui_kit::a11y_name(&cr, &format!("关闭 {title}"), true, None);
                        }
                        ui_kit::focus_ring(ui, &tr, tab_hit, 5.0);
                        ui_kit::focus_ring(ui, &cr, close, 4.0);
                        if cr.on_hover_text("关闭此文档 (Ctrl+W)").clicked() {
                            do_close = Some(i);
                        } else if tr.on_hover_text(tab_tip).clicked() {
                            do_switch = Some(i);
                        }
                        cur_x += tw + DOC_TAB_GAP;
                    }
                    let switcher_id = egui::Id::new("title_document_switcher");
                    if overflow {
                        let button = Rect::from_center_size(egui::pos2(cur_x + 12.0, rect.center().y), Vec2::splat(24.0));
                        let resp = ui.interact(button, switcher_id.with("button"), Sense::click()).on_hover_text("切换工作簿");
                        ui_kit::a11y_name(&resp, "切换工作簿", true, None);
                        if resp.clicked() {
                            ui.memory_mut(|m| m.toggle_popup(switcher_id));
                        }
                        if ui.memory(|m| m.is_popup_open(switcher_id)) || resp.hovered() {
                            painter.rect_filled(button, 5.0, hover);
                        }
                        ui_kit::caret(ui, button.center(), fg);
                        ui_kit::focus_ring(ui, &resp, button, 5.0);
                        egui::popup_below_widget(ui, switcher_id, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                            ui.set_width(260.0);
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                                for (i, (title, dirty, active)) in tab_metas.iter().enumerate() {
                                    let text = if *dirty { format!("{title}（未保存）") } else { title.clone() };
                                    let row = egui::SelectableLabel::new(*active, &text);
                                    if ui.add(row).on_hover_text(text).clicked() {
                                        do_switch = Some(i);
                                        ui.memory_mut(|m| m.close_popup());
                                    }
                                }
                            });
                        });
                        cur_x += DOC_ACTION_SLOT;
                    } else if ui.memory(|m| m.is_popup_open(switcher_id)) {
                        ui.memory_mut(|m| m.close_popup());
                    }
                    let plus = Rect::from_min_size(egui::pos2(cur_x + 2.0, rect.center().y - 11.0), Vec2::new(24.0, 22.0));
                    if plus.right() <= right_limit {
                        let pr = ui.interact(plus, egui::Id::new("wps_tab_plus"), Sense::click());
                        ui_kit::a11y_name(&pr, "新建空白工作簿", true, None);
                        if pr.hovered() {
                            painter.rect_filled(plus, 5.0, hover);
                        }
                        let pc = plus.center();
                        painter.line_segment([egui::pos2(pc.x - 4.5, pc.y), egui::pos2(pc.x + 4.5, pc.y)], Stroke::new(1.3, pal.text_weak));
                        painter.line_segment([egui::pos2(pc.x, pc.y - 4.5), egui::pos2(pc.x, pc.y + 4.5)], Stroke::new(1.3, pal.text_weak));
                        ui_kit::focus_ring(ui, &pr, plus, 5.0);
                        if pr.on_hover_text("新建空白标签页 (Ctrl+N)").clicked() {
                            new_tab = true;
                        }
                        cur_x = plus.right() + 6.0;
                    }
                } else {
                    painter.text(
                        egui::pos2(rect.left() + 50.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        &self.last_title,
                        ui_kit::ui_bold_in(ctx, 14.0),
                        fg,
                    );
                    cur_x = rect.left() + 216.0;
                }

                // 4. 搜索框
                if show_search {
                    let sr = Rect::from_min_size(egui::pos2(sx, rect.center().y - 13.0), Vec2::new(search_w, 26.0));
                    let search = ui.interact(sr, egui::Id::new("title_command_search"), Sense::click());
                    let fill = if search.is_pointer_button_down_on() {
                        pal.pressed
                    } else if search.hovered() {
                        pal.hover
                    } else {
                        pal.card_bg
                    };
                    painter.rect(sr, 6.0, fill, Stroke::new(1.0, pal.card_border), egui::StrokeKind::Inside);
                    ui_icons::draw_mono(
                        &painter,
                        Rect::from_center_size(egui::pos2(sr.left() + 15.0, sr.center().y), Vec2::splat(13.0)),
                        Icon::Find,
                        pal.text_weak,
                    );
                    painter.text(sr.left_center() + Vec2::new(28.0, 0.0), Align2::LEFT_CENTER,
                        "搜索命令", FontId::proportional(ui_kit::T_BODY), fg);
                    painter.text(sr.right_center() - Vec2::new(9.0, 0.0), Align2::RIGHT_CENTER,
                        "Ctrl+Q", FontId::proportional(ui_kit::T_SMALL), pal.text_weak);
                    ui_kit::a11y_name(&search, "搜索命令", true, None);
                    ui_kit::focus_ring(ui, &search, sr, 6.0);
                    if search.clicked() {
                        self.palette_q.clear();
                        self.palette_open = true;
                    }
                }

                // 5. 账户胶囊
                let aresp = ui.interact(acc, egui::Id::new("account"), Sense::click());
                if aresp.hovered() {
                    painter.rect_filled(acc, 4.0, if aresp.is_pointer_button_down_on() { pal.pressed } else { hover });
                }
                let ac = egui::pos2(acc.left() + 13.0, acc.center().y);
                painter.circle_filled(ac, 9.5, pal.info_bg);
                painter.circle_stroke(ac, 9.5, Stroke::new(1.0, pal.info_fg));
                painter.text(
                    ac,
                    Align2::CENTER_CENTER,
                    user.chars().next().unwrap_or('U').to_uppercase().to_string(),
                    ui_kit::ui_bold_in(ctx, ui_kit::T_SMALL),
                    pal.info_fg,
                );
                painter.galley(
                    egui::pos2(ac.x + 14.0, acc.center().y - name_galley.size().y / 2.0),
                    name_galley, fg,
                );
                ui_kit::focus_ring(ui, &aresp, acc, 4.0);
                if ui_kit::is_accesskit_active(ctx) {
                    ui_kit::a11y_name(&aresp, &format!("账户：{user}"), true, None);
                }
                if aresp.on_hover_text(format!("账户：{user}")).clicked() {
                    self.profile_draft = self.my_name.clone();
                    self.profile_msg.clear();
                    self.profile_open = true;
                }

                // 6. Windows 11 控制三键（最大化按钮上报客户区坐标以启用 Win11 Snap Layouts 贴靠布局浮层）
                let bw = 46.0;
                let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                let ppp = ctx.pixels_per_point();
                for (name, x) in [("close", rect.right() - bw), ("max", rect.right() - bw * 2.0), ("min", rect.right() - bw * 3.0)] {
                    let br = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(bw, ui_kit::TITLE_H));
                    if name == "max" {
                        crate::win_chrome::update_caption_metrics(br, ppp, ctx.native_pixels_per_point().unwrap_or(ppp));
                    }
                    let resp = ui.interact(br, egui::Id::new(("winbtn", name)), Sense::click());
                    let is_hovered = resp.hovered()
                        || (name == "max" && crate::win_chrome::maximize_button_nc_hovered());
                    let pressed = resp.is_pointer_button_down_on()
                        || (name == "max" && crate::win_chrome::maximize_button_nc_pressed());
                    if is_hovered {
                        let fill = match (name, pressed) {
                            ("close", true) => Color32::from_rgb(157, 31, 24),
                            ("close", false) => Color32::from_rgb(196, 43, 28),
                            (_, true) => pal.pressed,
                            _ => hover,
                        };
                        painter.rect_filled(br, 0.0, fill);
                    }
                    let col = if name == "close" && is_hovered { Color32::WHITE } else { fg };
                    // Align outline edges to physical pixels, including fractional monitor scaling.
                    let snap = |v: f32| (v * ppp).round() / ppp;
                    let line = snap(1.0).max(1.0 / ppp);
                    let size = snap(10.0);
                    let c = br.center();
                    let glyph = Rect::from_min_size(
                        egui::pos2(snap(c.x - size * 0.5), snap(c.y - size * 0.5)),
                        Vec2::splat(size),
                    );
                    let stroke = Stroke::new(line, col);
                    match name {
                        "close" => {
                            let r = glyph.shrink(line * 0.5);
                            painter.line_segment([r.left_top(), r.right_bottom()], stroke);
                            painter.line_segment([r.right_top(), r.left_bottom()], stroke);
                        }
                        "min" => {
                            let min = egui::pos2(glyph.left(), snap(c.y - line * 0.5));
                            painter.rect_filled(Rect::from_min_size(min, Vec2::new(size, line)), 0.0, col);
                        }
                        _ if maximized => {
                            let offset = snap(2.0);
                            let back = Rect::from_min_max(glyph.min + Vec2::new(offset, 0.0),
                                glyph.max - Vec2::new(0.0, offset));
                            let front = back.translate(Vec2::new(-offset, offset));
                            let edge = back.shrink(line * 0.5);
                            painter.add(egui::Shape::line(vec![
                                egui::pos2(edge.left(), front.top()), edge.left_top(),
                                edge.right_top(), edge.right_bottom(), egui::pos2(front.right(), edge.bottom()),
                            ], stroke));
                            painter.rect_stroke(front, 0.0, stroke, egui::StrokeKind::Inside);
                        }
                        _ => {
                            painter.rect_stroke(glyph, 0.0, stroke, egui::StrokeKind::Inside);
                        }
                    }
                    let label = match name {
                        "close" => "关闭窗口",
                        "min" => "最小化",
                        _ if maximized => "还原窗口",
                        _ => "最大化",
                    };
                    ui_kit::a11y_name(&resp, label, true, None);
                    ui_kit::focus_ring(ui, &resp, br.shrink(3.0), 3.0);
                    if resp.clicked() || (name == "max" && crate::win_chrome::take_maximize_button_click()) {
                        match name {
                            "close" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                            "max" => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                            _ => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                        }
                    }
                }

                // 7. 空白拖动条
                if cur_x < right_limit - 8.0 {
                    let drag = Rect::from_min_max(egui::pos2(cur_x, rect.top() + 5.0), egui::pos2(right_limit, rect.bottom() - 2.0));
                    let dr = ui.interact(drag, egui::Id::new("title_drag"), Sense::click_and_drag());
                    if dr.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if dr.double_clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }
                }
            });

        if let Some(i) = do_close {
            self.request_close_tab(i);
        }
        if let Some(i) = do_switch {
            self.show_start = false;
            self.switch_tab(i);
        }
        if new_tab {
            self.command_exec("NewTab", ctx);
        }
    }

    /// WPS 32px 单行合体底栏：左侧 `< >` + 工作表标签 + `+` | 中间可拖分割条 | 右侧表格横向滚动条
    pub(crate) fn draw_wps_bottom_bar(&mut self, ctx: &egui::Context) {
        let (cols, _) = self.view_axes();
        let view_w = (self.last_view.x - crate::HEAD_W).max(1.0);
        let hard_max_x = (cols.pos(16_384) - view_w).max(0.0);
        let mut sx = self.scroll.x;
        self.cf_inputs
            .merges
            .ensure(self.wb.sheet(self.sheet_idx), self.doc_gen, self.wb_epoch);
        let calc_total_w = |app: &SheetApp, cur_sx: f32| -> f32 {
            let sh = app.wb.sheet(app.sheet_idx);
            let used_c2 = sh.used_bounds().map(|(_, _, _, c2)| c2).unwrap_or(0);
            let merge_c2 = app.cf_inputs.merges.max_col();
            let sel_c2 = app.sel.1.max(app.sel_end.1);
            let view_c2 = cols.index_at((cur_sx + view_w).max(0.0));
            let eff_cols = (used_c2.max(merge_c2).max(sel_c2).max(view_c2) + 16).clamp(32, 16_384);
            cols.pos(eff_cols).max(view_w + 1.0)
        };
        let ratio = self.sheet_tab_split_ratio.clamp(0.2, 0.8);

        let pal = crate::theme::palette();
        egui::TopBottomPanel::bottom("tabs")
            .show_separator_line(false)
            .exact_height(if self.show_start { 0.0 } else { tokens::size::SHEET_TAB_H })
            .frame(egui::Frame::NONE.fill(pal.page_bg))
            .show(ctx, |ui| {
                if self.show_start {
                    return;
                }
                let rect = ui.max_rect();
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                let hairline = if pal.theme == crate::theme::AppTheme::HighContrast {
                    1.0
                } else {
                    (1.0 / ctx.pixels_per_point()).min(1.0)
                };
                ui.painter().hline(
                    rect.x_range(),
                    rect.top() + hairline * 0.5,
                    Stroke::new(hairline, pal.chrome_separator()),
                );
                let mut goto: Option<usize> = None;
                let mut ren: Option<(usize, String)> = None;
                let mut move_sheet = None;
                let structure_unavailable = self.sheet_structure_unavailable_reason();

                let split_x = (rect.left() + rect.width() * ratio)
                    .clamp(rect.left() + 180.0, (rect.right() - 160.0).max(rect.left() + 180.0));
                let left_rect = Rect::from_min_max(rect.min, egui::pos2(split_x - 4.0, rect.bottom()));
                let split_rect = Rect::from_min_max(egui::pos2(split_x - 4.0, rect.top()), egui::pos2(split_x + 4.0, rect.bottom()));
                let hsb_right = (rect.right() - SB).max(split_x + 24.0);
                let hsb_rect = Rect::from_min_max(egui::pos2(split_x + 4.0, rect.top()), egui::pos2(hsb_right, rect.bottom()));

                let content_left = left_rect.left() + 8.0;
                let content_right = left_rect.right() - 32.0;
                let tab_space = (content_right - content_left).max(1.0);
                let label_limit = tab_space.min(192.0);
                let mut sheet_tabs: Vec<_> = self.wb.sheets.iter().enumerate()
                    .filter(|(_, sheet)| !sheet.hidden)
                    .map(|(index, sheet)| SheetTabLayout {
                        index,
                        label: ui_kit::sheet_tab_label(ui, &sheet.name, index == self.sheet_idx, label_limit),
                        start: 0.0,
                    })
                    .collect();
                let total: f32 = sheet_tabs.iter().map(SheetTabLayout::width).sum();
                let overflow = total > tab_space;
                let nav_width = if overflow { 76.0 } else { 0.0 };
                let scroll_rect = Rect::from_min_max(
                    egui::pos2(content_left + nav_width, left_rect.top()),
                    egui::pos2(content_right, left_rect.bottom()),
                );
                let mut tab_end = 0.0;
                for tab in &mut sheet_tabs {
                    let sheet = &self.wb.sheets[tab.index];
                    let active = tab.index == self.sheet_idx;
                    if tab.width() > scroll_rect.width() {
                        tab.label = ui_kit::sheet_tab_label(ui, &sheet.name, active, scroll_rect.width());
                    }
                    tab.start = tab_end;
                    tab_end += tab.width();
                    if active {
                        // 仅在切表或标签几何变化时显露活动表，手动浏览其他标签时保留滚动位置。
                        let key = (self.doc_uid, sheet.identity, tab.start.to_bits(), tab_end.to_bits(), scroll_rect.width().to_bits());
                        let id = egui::Id::new("visible_active_sheet_tab");
                        let reveal = ctx.data_mut(|data| {
                            if data.get_temp(id) == Some(key) { return false; }
                            data.insert_temp(id, key);
                            true
                        });
                        if reveal {
                            self.tab_scroll = self.tab_scroll.max(tab_end - scroll_rect.width()).min(tab.start);
                        }
                    }
                }
                let max_tab_scroll = (tab_end - scroll_rect.width()).max(0.0);
                self.tab_scroll = self.tab_scroll.clamp(0.0, max_tab_scroll);
                ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
                    let area = egui::ScrollArea::horizontal()
                        .id_salt("sheet_tabs")
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, false])
                        .horizontal_scroll_offset(self.tab_scroll);
                    let out = area.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for tab in &sheet_tabs {
                                let i = tab.index;
                                let name = &self.wb.sheets[i].name;
                                let tab_color = self.wb.sheets[i].tab_color;
                                let is_grouped = self.selected_sheets.len() > 1
                                    && self.selected_sheets.contains(&i);
                                let resp = ui_kit::sheet_tab(ui, name, tab.label.clone(), self.sheet_idx == i, is_grouped, tab_color);
                                if resp.gained_focus() {
                                    resp.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
                                }
                                if structure_unavailable.is_none() {
                                    if resp.drag_started_by(egui::PointerButton::Primary) {
                                        resp.dnd_set_drag_payload(SheetTabDrag { doc_uid: self.doc_uid, sheet_id: self.wb.sheets[i].identity });
                                    }
                                    if let Some(source) = resp.dnd_hover_payload::<SheetTabDrag>().filter(|s| s.doc_uid == self.doc_uid) {
                                        if let Some(from) = self.wb.sheets.iter().position(|sheet| sheet.identity == source.sheet_id).filter(|&from| from != i) {
                                            let x = if from < i { resp.rect.right() - 1.0 } else { resp.rect.left() + 1.0 };
                                            ui.painter().line_segment(
                                                [egui::pos2(x, resp.rect.top() + 4.0), egui::pos2(x, resp.rect.bottom() - 4.0)],
                                                Stroke::new(2.0, pal.accent),
                                            );
                                            if ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary))
                                                && resp.dnd_release_payload::<SheetTabDrag>().is_some() {
                                                move_sheet = Some((from, i));
                                            }
                                        }
                                    }
                                }
                                if resp.clicked() {
                                    let (ctrl, shift) = ui.input(|ip| (ip.modifiers.command, ip.modifiers.shift));
                                    if (!ctrl && !shift) || self.editing.as_ref().is_some_and(|ed| ed.buf.starts_with('=')) {
                                        self.activate_sheet_tab(i);
                                    } else if !self.finish_pending_edit() {
                                        continue;
                                    } else if ctrl {
                                        if self.selected_sheets.is_empty() {
                                            self.selected_sheets.insert(self.sheet_idx);
                                        }
                                        if self.selected_sheets.contains(&i) {
                                            if self.selected_sheets.len() > 1 {
                                                self.selected_sheets.remove(&i);
                                                if i == self.sheet_idx {
                                                    if let Some(&first) = self.selected_sheets.iter().next() {
                                                        goto = Some(first);
                                                    }
                                                }
                                            }
                                        } else {
                                            self.selected_sheets.insert(i);
                                            goto = Some(i);
                                        }
                                        if self.selected_sheets.len() > 1 {
                                            self.status = format!(
                                                "[工作组] 已选中 {} 张工作表（同步编辑）",
                                                self.selected_sheets.len()
                                            );
                                        } else {
                                            self.selected_sheets.clear();
                                        }
                                    } else {
                                        self.selected_sheets.clear();
                                        let lo = self.sheet_idx.min(i);
                                        let hi = self.sheet_idx.max(i);
                                        for s in lo..=hi {
                                            self.selected_sheets.insert(s);
                                        }
                                        goto = Some(i);
                                        if self.selected_sheets.len() > 1 {
                                            self.status = format!(
                                                "[工作组] 已选中 {} 张工作表（同步编辑）",
                                                self.selected_sheets.len()
                                            );
                                        }
                                    }
                                }
                                if resp.double_clicked() && structure_unavailable.is_none() {
                                    ren = Some((i, self.wb.sheets[i].name.clone()));
                                }
                                if resp.secondary_clicked() {
                                    if self.editing.is_some() && !self.finish_pending_edit() {
                                        return;
                                    }
                                    if let Some(p) = resp.interact_pointer_pos() {
                                        self.ctx_menu = Some(CtxMenuState {
                                            pos: p,
                                            frame: ui.ctx().cumulative_pass_nr(),
                                            sub: None,
                                            kind: CtxKind::Tab,
                                            idx: i,
                                            key: 0,
                                            sub_key: 0,
                                        });
                                        ui.ctx().request_repaint();
                                    }
                                }
                            }
                        });
                    });
                    self.tab_scroll = out.state.offset.x;
                });

                if overflow {
                    for (i, (d, label)) in [(-1.0, "工作表标签左移"), (1.0, "工作表标签右移")].into_iter().enumerate() {
                        let b = Rect::from_min_size(egui::pos2(content_left + i as f32 * 24.0, left_rect.top() + 1.0), Vec2::new(24.0, 30.0));
                        let can = if d < 0.0 { self.tab_scroll > 0.5 } else { self.tab_scroll + 0.5 < max_tab_scroll };
                        let sense = if can { Sense::click() } else { Sense::hover() };
                        let resp = ui.interact(b, egui::Id::new(("tabscroll", i)), sense);
                        ui_kit::a11y_name(&resp, label, can, None);
                        if resp.hovered() && can {
                            ui.painter().rect_filled(b.shrink(4.0), 3.0, pal.hover_on_page);
                        }
                        if resp.clicked() && can {
                            let boundary = if d < 0.0 {
                                sheet_tabs.iter().rev().find(|tab| tab.start < self.tab_scroll - 0.5)
                            } else {
                                sheet_tabs.iter().find(|tab| tab.start > self.tab_scroll + 0.5)
                            };
                            let edge = if d < 0.0 { 0.0 } else { max_tab_scroll };
                            self.tab_scroll = boundary.map_or(edge, |tab| tab.start).clamp(0.0, max_tab_scroll);
                            ctx.request_repaint();
                        }
                        let c = b.center();
                        let st = Stroke::new(1.1, if can { pal.text_weak } else { pal.text_muted });
                        if can {
                            ui_kit::focus_ring(ui, &resp, b.shrink(3.0), 3.0);
                            resp.on_hover_text(label);
                        }
                        ui.painter().line_segment([egui::pos2(c.x - d * 2.5, c.y - 4.5), egui::pos2(c.x + d * 2.5, c.y)], st);
                        ui.painter().line_segment([egui::pos2(c.x + d * 2.5, c.y), egui::pos2(c.x - d * 2.5, c.y + 4.5)], st);
                    }
                    let list = Rect::from_min_size(egui::pos2(content_left + 48.0, left_rect.top() + 1.0), Vec2::new(24.0, 30.0));
                    let resp = ui.interact(list, egui::Id::new("sheet_list_button"), Sense::click());
                    ui_kit::a11y_name(&resp, "工作表列表", true, None);
                    if resp.hovered() {
                        ui.painter().rect_filled(list.shrink(2.0), 4.0, pal.hover_on_page);
                    }
                    for dx in [-4.0, 0.0, 4.0] {
                        ui.painter().circle_filled(list.center() + Vec2::new(dx, 0.0), 1.3, pal.text);
                    }
                    ui_kit::focus_ring(ui, &resp, list.shrink(2.0), 4.0);
                    if resp.on_hover_text("打开工作表列表，可搜索表名").clicked() {
                        self.sheet_list_query.clear();
                        self.sheet_list_open = true;
                    }
                }

                let plus_x = (scroll_rect.left() + tab_end).min(content_right);
                let nb = Rect::from_min_size(egui::pos2(plus_x, left_rect.top() + 1.0), Vec2::new(28.0, 30.0));
                let can_add = structure_unavailable.is_none();
                let nresp = ui.interact(nb, egui::Id::new("newsheet"), if can_add { Sense::click() } else { Sense::hover() });
                ui_kit::a11y_name(&nresp, "新建工作表", can_add, None);
                ui_kit::focus_ring(ui, &nresp, nb.shrink(3.0), 3.0);
                if nresp.hovered() && can_add {
                    ui.painter().rect_filled(nb.shrink2(Vec2::new(2.0, 4.0)), 5.0, pal.hover_on_page);
                }
                let c = nb.center();
                let st = Stroke::new(1.2, if can_add { pal.text } else { pal.text_muted });
                ui.painter().line_segment([egui::pos2(c.x - 4.5, c.y), egui::pos2(c.x + 4.5, c.y)], st);
                ui.painter().line_segment([egui::pos2(c.x, c.y - 4.5), egui::pos2(c.x, c.y + 4.5)], st);
                if nresp.on_hover_text(structure_unavailable.unwrap_or("新建工作表")).clicked() && can_add {
                    self.add_sheet();
                }

                // 中央可拖动分割条
                let sresp = ui.interact(split_rect, egui::Id::new("sheet_tab_splitter"), Sense::drag());
                if sresp.hovered() || sresp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                let sc = split_rect.center();
                let dot_col = if sresp.hovered() || sresp.dragged() {
                    pal.accent
                } else {
                    pal.text_muted
                };
                for dy in [-4.0f32, 0.0, 4.0] {
                    ui.painter().circle_filled(egui::pos2(sc.x, sc.y + dy), 1.3, dot_col);
                }
                if sresp.dragged() {
                    if let Some(p) = sresp.interact_pointer_pos() {
                        self.sheet_tab_split_ratio = ((p.x - rect.left()) / rect.width().max(1.0)).clamp(0.2, 0.8);
                    }
                }

                // 右侧表格横向滚动条（居中 14px 纤细导轨；光标悬停在下方横向滚动条槽区时滚动滚轮同步推动滚动条与网格，对齐 WPS 行为）
                let hover_hsb = !self.dialog_modal
                    && self.ctx_menu.is_none()
                    && ui
                        .ctx()
                        .input(|ip| ip.pointer.hover_pos().is_some_and(|p| hsb_rect.contains(p)));
                if hover_hsb {
                    let (ctrl, d) = ui.input(|ip| (ip.modifiers.command, ip.raw_scroll_delta));
                    if !ctrl && (d.x.abs() > 0.1 || d.y.abs() > 0.1) {
                        let delta = if d.x.abs() > d.y.abs() {
                            -d.x
                        } else {
                            -d.y * 1.8
                        };
                        sx = (sx + delta).clamp(0.0, hard_max_x);
                        ctx.request_repaint();
                    }
                }
                let total_w = calc_total_w(self, sx);
                let hsb_bar = Rect::from_center_size(
                    hsb_rect.center(),
                    Vec2::new((hsb_rect.width() - 10.0).max(16.0), 14.0),
                );
                crate::scrollbar(ui, "hsb", hsb_bar, true, &mut sx, total_w, view_w);
                self.scroll.x = sx.clamp(0.0, hard_max_x);

                if let Some((from, to)) = move_sheet {
                    self.swap_sheet_positions(from, to);
                }
                if let Some(i) = goto {
                    if i != self.sheet_idx {
                        self.switch_sheet_view(i);
                    }
                }
                if let Some((i, n)) = ren {
                    self.rename_sheet = Some((i, n));
                }
            });
    }
}
