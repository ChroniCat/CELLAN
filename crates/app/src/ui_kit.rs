//! 界面基座：一套配色、一套间距、分组框和图标按钮。
//!
//! 目标是 Excel 的骨架（页签 + 分组框 + 图标文字）+ 现代的配色和留白。
//! 所有尺寸集中在这里，别再往各处写魔法数字。

use eframe::egui::{
    self, Align2, Color32, FontId, Response, RichText, Sense, Stroke, Vec2,
};
use crate::ui_icons::{self, Icon};

/// 功能区总高（页签行 + 分组行）
pub const RIBBON_H: f32 = 96.0;
pub const TAB_H: f32 = 30.0;
/// 分组内容高度：图标大按钮按这个留
pub const GROUP_BODY_H: f32 = 48.0;

pub fn accent(v: &egui::Visuals) -> Color32 {
    if v.dark_mode {
        Color32::from_rgb(96, 165, 250)
    } else {
        Color32::from_rgb(37, 99, 235)
    }
}

fn text_color(v: &egui::Visuals) -> Color32 {
    v.override_text_color
        .unwrap_or(v.widgets.noninteractive.fg_stroke.color)
}

/// 浅色：白底、灰分隔、蓝选中；深色：深灰底、低对比描边
pub fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let dark = style.visuals.dark_mode;
    style.text_styles = [
        (egui::TextStyle::Heading, FontId::proportional(15.0)),
        (egui::TextStyle::Body, FontId::proportional(13.0)),
        (egui::TextStyle::Button, FontId::proportional(12.5)),
        (egui::TextStyle::Small, FontId::proportional(10.5)),
        (egui::TextStyle::Monospace, FontId::monospace(12.0)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(6.0, 5.0);
    style.spacing.button_padding = Vec2::new(8.0, 3.0);
    style.spacing.interact_size.y = 22.0;
    style.spacing.menu_margin = egui::Margin {
        left: 6,
        right: 6,
        top: 5,
        bottom: 5,
    };

    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    if dark {
        v.panel_fill = Color32::from_rgb(30, 33, 38);
        v.window_fill = Color32::from_rgb(38, 42, 49);
        v.extreme_bg_color = Color32::from_rgb(24, 27, 31);
        v.faint_bg_color = Color32::from_rgb(36, 40, 46);
        v.widgets.noninteractive.bg_fill = Color32::from_rgb(38, 42, 49);
        v.widgets.noninteractive.bg_stroke =
            Stroke::new(1.0, Color32::from_rgb(58, 64, 74));
        v.widgets.noninteractive.fg_stroke =
            Stroke::new(1.0, Color32::from_rgb(228, 231, 236));
        v.widgets.inactive.bg_fill = Color32::from_rgb(44, 49, 57);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(44, 49, 57);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(62, 68, 79));
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(224, 227, 233));
        v.widgets.hovered.bg_fill = Color32::from_rgb(55, 62, 72);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(55, 62, 72);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(86, 95, 110));
        v.widgets.active.bg_fill = Color32::from_rgb(62, 72, 86);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(62, 72, 86);
        v.selection.bg_fill = Color32::from_rgb(45, 74, 112);
        v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(147, 197, 253));
        v.hyperlink_color = Color32::from_rgb(96, 165, 250);
    } else {
        v.panel_fill = Color32::from_rgb(252, 252, 253);
        v.window_fill = Color32::from_rgb(255, 255, 255);
        v.extreme_bg_color = Color32::from_rgb(255, 255, 255);
        v.faint_bg_color = Color32::from_rgb(247, 248, 250);
        v.widgets.noninteractive.bg_fill = Color32::from_rgb(255, 255, 255);
        v.widgets.noninteractive.bg_stroke =
            Stroke::new(1.0, Color32::from_rgb(223, 226, 232));
        v.widgets.noninteractive.fg_stroke =
            Stroke::new(1.0, Color32::from_rgb(32, 36, 42));
        v.widgets.inactive.bg_fill = Color32::from_rgb(255, 255, 255);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(255, 255, 255);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(214, 218, 226));
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(32, 36, 42));
        v.widgets.hovered.bg_fill = Color32::from_rgb(236, 242, 251);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(236, 242, 251);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(160, 178, 204));
        v.widgets.active.bg_fill = Color32::from_rgb(219, 232, 251);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(219, 232, 251);
        v.selection.bg_fill = Color32::from_rgb(206, 224, 251);
        v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(37, 99, 235));
        v.hyperlink_color = Color32::from_rgb(37, 99, 235);
    }
    style.visuals = v;
    ctx.set_style(style);
}

/// 一个功能区小组：内容 + 底部组名，浅底圆角框
pub fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    let v = ui.visuals().clone();
    let out = egui::Frame::NONE
        .fill(v.faint_bg_color)
        .stroke(Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color))
        .corner_radius(6.0)
        .inner_margin(egui::Margin {
            left: 6,
            right: 6,
            top: 4,
            bottom: 2,
        })
        .show(ui, |ui| {
            // Frame 会继承父级布局（功能区是横向的），这里必须显式竖排，
            // 否则组名会跑到内容右边、分组框宽度翻倍。
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
                let row = ui.horizontal_top(|ui| {
                    ui.set_height(GROUP_BODY_H);
                    add(ui);
                });
                let w = row.response.rect.width();
                ui.add_space(3.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(w, 12.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        ui.label(
                            RichText::new(title)
                                .size(9.5)
                                .color(v.weak_text_color().gamma_multiply(0.85)),
                        );
                    },
                );
            });
        });
    // 布局自证：SC_UI_LOG=1 时打出每个分组框的几何，比截图更适合做断言
    if std::env::var("SC_UI_LOG").is_ok() {
        let r = out.response.rect;
        eprintln!(
            "[ui] 组 {:<8} x={:6.1} y={:6.1} w={:6.1} h={:5.1}",
            title,
            r.min.x,
            r.min.y,
            r.width(),
            r.height()
        );
    }
}

/// 图标在上、文字在下的大按钮（粘贴、图表这种）
pub fn big(ui: &mut egui::Ui, icon: Icon, label: &str) -> Response {
    big_sized(ui, icon, label, Vec2::new(52.0, GROUP_BODY_H))
}

pub fn big_sized(ui: &mut egui::Ui, icon: Icon, label: &str, size: Vec2) -> Response {
    let v = ui.visuals().clone();
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if resp.is_pointer_button_down_on() {
        v.selection.bg_fill
    } else if resp.hovered() {
        v.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 5.0, bg);
    }
    let col = if enabled {
        text_color(&v)
    } else {
        v.weak_text_color().gamma_multiply(0.7)
    };
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 19.0),
        Vec2::splat(20.0),
    );
    ui_icons::draw(ui.painter(), icon_rect, icon, col);
    ui.painter().text(
        egui::pos2(rect.center().x, rect.bottom() - 10.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.5),
        col,
    );
    resp
}

/// 只有图标的小按钮（撤销、对齐这类）
pub fn small(ui: &mut egui::Ui, icon: Icon, tip: &str) -> Response {
    small_on(ui, icon, false, tip)
}

pub fn small_on(ui: &mut egui::Ui, icon: Icon, on: bool, tip: &str) -> Response {
    let v = ui.visuals().clone();
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(26.0, 22.0), Sense::click());
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if on {
        v.selection.bg_fill
    } else if resp.hovered() {
        v.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 4.0, bg);
    }
    let col = if !enabled {
        v.weak_text_color().gamma_multiply(0.7)
    } else if on {
        accent(&v)
    } else {
        text_color(&v)
    };
    ui_icons::draw(ui.painter(), rect.shrink(3.0), icon, col);
    resp.on_hover_text(tip)
}

/// 字体类开关（B / I / U）：文字本身就是图标
pub fn text_toggle(ui: &mut egui::Ui, label: &str, on: bool, tip: &str, strong: bool, italic: bool) -> Response {
    let v = ui.visuals().clone();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 22.0), Sense::click());
    let bg = if on {
        v.selection.bg_fill
    } else if resp.hovered() {
        v.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 4.0, bg);
    }
    let col = if on { accent(&v) } else { text_color(&v) };
    let mut rt = RichText::new(label).size(14.0).color(col);
    if strong {
        rt = rt.strong();
    }
    if italic {
        rt = rt.italics();
    }
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(14.0), col);
    let _ = rt;
    resp.on_hover_text(tip)
}

/// 页签：悬停有底色，选中时下面一条蓝线
pub fn tab(ui: &mut egui::Ui, name: &str, active: bool) -> Response {
    let v = ui.visuals().clone();
    let font = FontId::proportional(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font.clone(), Color32::PLACEHOLDER);
    let size = Vec2::new(galley.size().x + 26.0, TAB_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.hovered() && !active {
        ui.painter()
            .rect_filled(rect.shrink2(Vec2::new(3.0, 3.0)), 5.0, v.widgets.hovered.weak_bg_fill);
    }
    let col = if active { text_color(&v) } else { v.weak_text_color() };
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, name, font, col);
    if active {
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 8.0, rect.bottom() - 2.0),
            Vec2::new(rect.width() - 16.0, 2.0),
        );
        ui.painter().rect_filled(bar, 1.0, accent(&v));
    }
    resp
}

/// 分组之间的竖细线
pub fn hairline(ui: &mut egui::Ui) {
    let v = ui.visuals().clone();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, GROUP_BODY_H), Sense::hover());
    ui.painter().rect_filled(
        rect,
        0.0,
        v.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.9),
    );
}

/// 对话框统一外框：标题栏 + 内容 + 底部按钮区
pub fn dialog(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
    ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
    body(ui);
}

/// 对话框底部的按钮行（右对齐）
pub fn footer(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(4.0);
    ui.separator();
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        add(ui);
    });
}

/// 主按钮（确定/应用）
pub fn primary_button(ui: &mut egui::Ui, label: &str) -> Response {
    let v = ui.visuals().clone();
    let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE))
        .fill(accent(&v))
        .corner_radius(4.0);
    ui.add(btn)
}

/// 紧凑文字按钮（组里放"插入行"这种短命令）
pub fn pill(ui: &mut egui::Ui, label: &str, tip: &str) -> Response {
    let v = ui.visuals().clone();
    let font = FontId::proportional(11.5);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::PLACEHOLDER);
    let size = Vec2::new(galley.size().x + 14.0, 22.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let bg = if resp.is_pointer_button_down_on() {
        v.selection.bg_fill
    } else if resp.hovered() {
        v.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font,
        text_color(&v),
    );
    if std::env::var("SC_UI_LOG").map(|v| v == "2").unwrap_or(false) {
        eprintln!("[ui]   pill {} w={:.1} x={:.1}", label, rect.width(), rect.min.x);
    }
    resp.on_hover_text(tip)
}

/// 工作表标签：活动页白底 + 顶部蓝条，未选中悬停变浅
pub fn sheet_tab(ui: &mut egui::Ui, name: &str, active: bool) -> Response {
    let v = ui.visuals().clone();
    let font = FontId::proportional(if active { 13.0 } else { 12.5 });
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font.clone(), Color32::PLACEHOLDER);
    let size = Vec2::new(galley.size().x + 24.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if active {
        ui.painter().rect_filled(rect, 4.0, v.faint_bg_color);
        ui.painter().rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color),
            egui::StrokeKind::Inside,
        );
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 3.0, rect.top()),
            Vec2::new(rect.width() - 6.0, 2.5),
        );
        ui.painter().rect_filled(bar, 1.0, accent(&v));
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, v.widgets.hovered.weak_bg_fill);
    }
    let color = if active { text_color(&v) } else { v.weak_text_color() };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        if active { name.to_string() } else { name.to_owned() },
        font.clone(),
        color,
    );
    if active {
        // 再描一遍加粗：小字号下比改字体权重更明显
        ui.painter().text(
            rect.center() + Vec2::new(0.4, 0.0),
            Align2::CENTER_CENTER,
            name,
            font,
            color,
        );
    }
    resp
}
