//! 界面基座：照 Excel 365 的尺寸与配色重做。
//!
//! 参考值来自本机 Excel 16.0 的 UIA 实测（96 DPI，窗口客户区坐标）：
//! 标题栏 48、页签行 30、功能区 102、编辑栏 44、列头 19、行头 29、
//! 默认列宽 72、默认行高 19、工作表标签条 32、状态栏 22。
//!
//! 所有尺寸集中在这里，别再往各处写魔法数字。

use eframe::egui::{
    self, Align2, Color32, FontId, Rect, Response, RichText, Sense, Stroke, Vec2,
};
use crate::ui_icons::{self, Icon};
use std::cell::Cell;

/// Excel 绿色（选项卡下划线、工作表标签下划线、选中框）。
/// 本机 Excel 16.0 实测取色：#217346，选中行列头底线、页签下的线、单元格选中框都是它。
pub const ACCENT: Color32 = Color32::from_rgb(33, 115, 70);
/// 分区分隔线 / 控件边框
pub const BORDER: Color32 = Color32::from_rgb(208, 208, 208);
/// 列头/行头与网格的分界线：Excel 实测 #ABABAB，比普通控件边框深
pub const HEAD_LINE: Color32 = Color32::from_rgb(171, 171, 171);
/// 功能区分组之间的竖线
pub const SEP: Color32 = Color32::from_rgb(224, 224, 224);
pub const TEXT: Color32 = Color32::from_rgb(29, 29, 29);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(96, 96, 96);
pub const HEADER_BG: Color32 = Color32::from_rgb(240, 240, 240);
pub const HEADER_SEL: Color32 = Color32::from_rgb(225, 225, 225);
pub const GRID_LINE: Color32 = Color32::from_rgb(217, 217, 217);
/// 页签行底色（Excel 是 #F3F2F1，活动页签白底盖在上面）
pub const TABBAR_BG: Color32 = Color32::from_rgb(243, 242, 241);
/// 标题栏底色：Excel 实测 240,240,240
pub const TITLE_BG: Color32 = Color32::from_rgb(240, 240, 240);
pub const HOVER: Color32 = Color32::from_rgb(233, 233, 233);
pub const PRESSED: Color32 = Color32::from_rgb(217, 217, 217);
pub const STATUS_BG: Color32 = Color32::from_rgb(245, 245, 245);

/// 标题栏（自绘，含快速访问工具栏和窗口按钮）
pub const TITLE_H: f32 = 48.0;
/// 页签行
pub const TAB_H: f32 = 30.0;
/// 功能区（不含页签行）
pub const RIBBON_BODY_H: f32 = 102.0;
/// 功能区总高
pub const RIBBON_H: f32 = TAB_H + RIBBON_BODY_H;
/// 编辑栏（名称框 + fx + 输入行）
pub const FX_H: f32 = 44.0;
/// 命令区高度：Excel 的大按钮就是 76 高，命令区从分组顶 +4 开始
pub const GROUP_BODY_H: f32 = 76.0;
/// 分组名那一行（文字贴底，和 Excel 一样离功能区底边 0~2px）
pub const GROUP_LABEL_H: f32 = 14.0;

thread_local! {
    static GROUP_FIRST: Cell<bool> = const { Cell::new(true) };
}

/// 每组 ribbon 内容开始前调用：第一组不画左侧分隔线
pub fn reset_groups() {
    GROUP_FIRST.with(|c| c.set(true));
}

pub fn accent(_v: &egui::Visuals) -> Color32 {
    ACCENT
}

fn text_color(v: &egui::Visuals) -> Color32 {
    v.override_text_color.unwrap_or(TEXT)
}

/// Excel 浅色主题；深色沿用原来的深灰配色（Excel 也有深色模式）。
/// 两套主题都要显式写入：egui 切主题时会用「该主题已保存的样式」，
/// 只改当前主题会让系统深色模式下功能区变成默认灰。
pub fn apply_theme(ctx: &egui::Context) {
    let mut base = (*ctx.style()).clone();
    base.text_styles = [
        (egui::TextStyle::Heading, FontId::proportional(15.0)),
        (egui::TextStyle::Body, FontId::proportional(12.5)),
        (egui::TextStyle::Button, FontId::proportional(12.0)),
        (egui::TextStyle::Small, FontId::proportional(11.0)),
        (egui::TextStyle::Monospace, FontId::monospace(12.0)),
    ]
    .into();
    base.spacing.item_spacing = Vec2::new(4.0, 3.0);
    base.spacing.button_padding = Vec2::new(7.0, 2.0);
    base.spacing.interact_size.y = 24.0;
    base.spacing.menu_margin = egui::Margin {
        left: 4,
        right: 4,
        top: 4,
        bottom: 4,
    };

    let mut light = base.clone();
    light.visuals = {
        let mut v = egui::Visuals::light();
        v.panel_fill = Color32::WHITE;
        v.window_fill = Color32::WHITE;
        v.extreme_bg_color = Color32::WHITE;
        v.faint_bg_color = Color32::from_rgb(247, 247, 247);
        v.widgets.noninteractive.bg_fill = Color32::WHITE;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.bg_fill = Color32::WHITE;
        v.widgets.inactive.weak_bg_fill = Color32::WHITE;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.corner_radius = egui::CornerRadius::same(2);
        v.widgets.hovered.bg_fill = HOVER;
        v.widgets.hovered.weak_bg_fill = HOVER;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.corner_radius = egui::CornerRadius::same(2);
        v.widgets.active.bg_fill = PRESSED;
        v.widgets.active.weak_bg_fill = PRESSED;
        v.widgets.active.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.active.corner_radius = egui::CornerRadius::same(2);
        v.widgets.open = v.widgets.active;
        v.selection.bg_fill = Color32::from_rgb(204, 228, 215);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = Color32::from_rgb(5, 99, 193);
        v
    };

    let mut dark = base;
    dark.visuals = {
        let mut v = egui::Visuals::dark();
        v.panel_fill = Color32::from_rgb(30, 33, 38);
        v.window_fill = Color32::from_rgb(38, 42, 49);
        v.extreme_bg_color = Color32::from_rgb(24, 27, 31);
        v.faint_bg_color = Color32::from_rgb(36, 40, 46);
        v.widgets.noninteractive.bg_fill = Color32::from_rgb(38, 42, 49);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(58, 64, 74));
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(228, 231, 236));
        v.widgets.inactive.bg_fill = Color32::from_rgb(44, 49, 57);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(44, 49, 57);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(62, 68, 79));
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(224, 227, 233));
        v.widgets.hovered.bg_fill = Color32::from_rgb(55, 62, 72);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(55, 62, 72);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(86, 95, 110));
        v.widgets.active.bg_fill = Color32::from_rgb(62, 72, 86);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(62, 72, 86);
        v.widgets.open = v.widgets.active;
        v.selection.bg_fill = Color32::from_rgb(45, 74, 112);
        v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(147, 197, 253));
        v.hyperlink_color = Color32::from_rgb(96, 165, 250);
        v
    };

    ctx.set_style_of(egui::Theme::Dark, dark);
    ctx.set_style_of(egui::Theme::Light, light);
    // Excel 默认浅色；界面按浅色做像素对齐
    ctx.set_theme(egui::ThemePreference::Light);
}

/// 一个功能区小组：内容 + 底部组名；组间是一条竖细线（Excel 无外框）
pub fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) -> Rect {
    let first = GROUP_FIRST.with(|c| c.replace(false));
    if !first {
        ui.add_space(5.0);
        let v = ui.visuals().clone();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, GROUP_BODY_H), Sense::hover());
        // 分隔线从命令区顶（+4）画到命令区底，和 Excel 一样不顶到功能区边
        ui.painter().rect_filled(
            rect.translate(Vec2::new(0.0, 4.0)),
            0.0,
            if v.dark_mode {
                Color32::from_rgb(58, 64, 74)
            } else {
                SEP
            },
        );
        ui.add_space(5.0);
    }
    let out = ui
        .vertical(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
            // Excel 的命令区从分组顶往下 4px 才开始
            ui.add_space(4.0);
            let row = ui.horizontal_top(|ui| {
                ui.add_space(6.0);
                ui.set_height(GROUP_BODY_H);
                add(ui);
                ui.allocate_exact_size(Vec2::new(6.0, 0.0), Sense::hover());
            });
            let w = row.response.rect.width().max(24.0);
            // Excel 的组名紧贴功能区底边：4(顶) + 76(命令) + 8(间隙) + 14(字) = 102
            ui.add_space(7.0);
            ui.allocate_ui_with_layout(
                Vec2::new(w, GROUP_LABEL_H),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    let c = if ui.visuals().dark_mode {
                        ui.visuals().weak_text_color()
                    } else {
                        Color32::from_rgb(102, 102, 102)
                    };
                    ui.label(RichText::new(title).size(12.0).color(c));
                },
            );
        })
        .response;
    // 布局自证：SC_UI_LOG=1 时打出每个分组框的几何
    let group_rect = out.rect;
    if std::env::var("SC_UI_LOG").is_ok() {
        let r = out.rect;
        eprintln!(
            "[ui] 组 {:<8} x={:6.1} y={:6.1} w={:6.1} h={:5.1}",
            title,
            r.min.x,
            r.min.y,
            r.width(),
            r.height()
        );
    }
    ui.add_space(2.0);
    group_rect
}

/// 分组的“对话框启动器”：Excel 在分组右下角的斜箭头
pub fn launcher_at(ui: &mut egui::Ui, group_rect: Rect, id: &str) -> bool {
    let size = Vec2::new(12.0, 12.0);
    let rect = Rect::from_min_size(
        egui::pos2(group_rect.right() - size.x - 1.0, group_rect.bottom() - size.y - 1.0),
        size,
    );
    let resp = ui.interact(rect, egui::Id::new(("grp_launch", id)), Sense::click());
    let c = if resp.hovered() {
        ACCENT
    } else {
        Color32::from_rgb(120, 120, 120)
    };
    let p = ui.painter();
    let st = Stroke::new(1.0, c);
    p.line_segment(
        [
            egui::pos2(rect.left() + 3.0, rect.bottom() - 3.0),
            egui::pos2(rect.right() - 3.0, rect.top() + 3.0),
        ],
        st,
    );
    p.line_segment(
        [
            egui::pos2(rect.right() - 6.0, rect.top() + 3.0),
            egui::pos2(rect.right() - 3.0, rect.top() + 3.0),
        ],
        st,
    );
    p.line_segment(
        [
            egui::pos2(rect.right() - 3.0, rect.top() + 6.0),
            egui::pos2(rect.right() - 3.0, rect.top() + 3.0),
        ],
        st,
    );
    resp.clicked()
}

/// 带对话框启动器的分组
pub fn group_l(ui: &mut egui::Ui, title: &str, id: &str, add: impl FnOnce(&mut egui::Ui)) -> bool {
    let r = group(ui, title, add);
    launcher_at(ui, r, id)
}
pub fn big_sized(ui: &mut egui::Ui, icon: Icon, label: &str, size: Vec2) -> Response {
    let v = ui.visuals().clone();
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if resp.is_pointer_button_down_on() {
        if v.dark_mode { v.selection.bg_fill } else { PRESSED }
    } else if resp.hovered() {
        if v.dark_mode { v.widgets.hovered.weak_bg_fill } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let col = if enabled {
        text_color(&v)
    } else {
        v.weak_text_color().gamma_multiply(0.6)
    };
    let icon_size = 32.0_f32.min(size.y - 22.0).max(16.0);
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 6.0 + icon_size / 2.0),
        Vec2::splat(icon_size),
    );
    ui_icons::draw_excel(ui.painter(), icon_rect, icon, col, enabled);
    ui.painter().text(
        egui::pos2(rect.center().x, rect.bottom() - 7.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.0),
        col,
    );
    resp
}

/// 同上，可指定按钮尺寸（样式组的 66×76 也用 split）
pub fn big_sized_split(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    size: Vec2,
) -> (Response, Response) {
    let v = ui.visuals().clone();
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let arrow_rect = Rect::from_min_size(
        egui::pos2(rect.right() - 9.0, rect.top()),
        Vec2::new(9.0, rect.height()),
    );
    let aresp = ui.interact(
        arrow_rect,
        egui::Id::new(("bigsplit", rect.left().to_bits(), rect.top().to_bits())),
        Sense::click(),
    );
    let hover = resp.hovered() && !arrow_rect.contains(resp.hover_pos().unwrap_or(rect.center()));
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if resp.is_pointer_button_down_on() || aresp.is_pointer_button_down_on() {
        if v.dark_mode { v.selection.bg_fill } else { PRESSED }
    } else if hover || aresp.hovered() {
        if v.dark_mode { v.widgets.hovered.weak_bg_fill } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let col = if enabled {
        text_color(&v)
    } else {
        v.weak_text_color().gamma_multiply(0.6)
    };
    // 图标和文字居中于「主区」（去掉右侧箭头条），否则整体重心向右偏
    let main_cx = rect.left() + (rect.width() - 9.0) / 2.0;
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(main_cx, rect.top() + 6.0 + 16.0),
        Vec2::splat(32.0),
    );
    ui_icons::draw_excel(ui.painter(), icon_rect, icon, col, enabled);
    ui.painter().text(
        egui::pos2(main_cx, rect.bottom() - 7.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.0),
        col,
    );
    // 右侧分隔条 + 下拉箭头：箭头在按钮垂直中线，像 Excel 的 split
    ui.painter().line_segment(
        [
            egui::pos2(arrow_rect.left(), rect.top() + 6.0),
            egui::pos2(arrow_rect.left(), rect.bottom() - 6.0),
        ],
        Stroke::new(1.0, BORDER),
    );
    let ac = egui::pos2(arrow_rect.center().x, rect.center().y);
    caret(ui, ac, col);
    (resp, aresp)
}

pub fn small_on(ui: &mut egui::Ui, icon: Icon, on: bool, tip: &str) -> Response {
    let v = ui.visuals().clone();
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if on {
        if v.dark_mode { v.selection.bg_fill } else { PRESSED }
    } else if resp.hovered() {
        if v.dark_mode { v.widgets.hovered.weak_bg_fill } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let col = if !enabled {
        v.weak_text_color().gamma_multiply(0.6)
    } else {
        text_color(&v)
    };
    ui_icons::draw_excel(ui.painter(), rect.shrink(4.0), icon, col, enabled);
    resp.on_hover_text(tip)
}

/// 固定宽度的图标文字按钮：Excel 的「剪切/复制」是 81×24，「插入」也是 81×24
pub fn labeled_w(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    width: f32,
    tip: &str,
) -> Response {
    labeled_w_on(ui, icon, label, width, false, tip)
}

/// 固定宽度的图标文字按钮，可显示按下状态
pub fn labeled_w_on(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    width: f32,
    on: bool,
    tip: &str,
) -> Response {
    let v = ui.visuals().clone();
    let enabled = ui.is_enabled();
    let font = FontId::proportional(12.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click());
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if on || resp.is_pointer_button_down_on() {
        if v.dark_mode { v.selection.bg_fill } else { PRESSED }
    } else if resp.hovered() {
        if v.dark_mode { v.widgets.hovered.weak_bg_fill } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let col = if enabled {
        text_color(&v)
    } else {
        v.weak_text_color().gamma_multiply(0.6)
    };
    let ir = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 4.0, rect.center().y - 8.0),
        Vec2::splat(16.0),
    );
    ui_icons::draw_excel(ui.painter(), ir, icon, col, enabled);
    ui.painter().text(
        egui::pos2(rect.left() + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        col,
    );
    resp.on_hover_text(tip)
}

/// 固定宽度的图标+文字按钮，指定文字/图标颜色（Excel 的“共享”是绿色）
pub fn labeled_tint(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    width: f32,
    tint: Color32,
    tip: &str,
) -> Response {
    let v = ui.visuals().clone();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            2.0,
            if v.dark_mode { v.widgets.hovered.weak_bg_fill } else { HOVER },
        );
    }
    let ir = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 4.0, rect.center().y - 8.0),
        Vec2::splat(16.0),
    );
    ui_icons::draw(ui.painter(), ir, icon, tint);
    ui.painter().text(
        egui::pos2(rect.left() + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        tint,
    );
    resp.on_hover_text(tip)
}

/// 选项右侧的小三角（下拉菜单按钮用）
pub fn caret(ui: &mut egui::Ui, center: egui::Pos2, color: Color32) {
    let st = Stroke::new(1.0, color);
    ui.painter()
        .line_segment([center + Vec2::new(-3.0, -1.5), center + Vec2::new(0.0, 1.5)], st);
    ui.painter()
        .line_segment([center + Vec2::new(0.0, 1.5), center + Vec2::new(3.0, -1.5)], st);
}

/// 字体颜色 / 填充颜色按钮：图标（或字母）+ 下方 3px 色条，Excel 的样子
pub fn color_button(
    ui: &mut egui::Ui,
    letter: Option<&str>,
    icon: Option<Icon>,
    color: Color32,
    tip: &str,
) -> Response {
    let v = ui.visuals().clone();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            2.0,
            if v.dark_mode { v.widgets.hovered.weak_bg_fill } else { HOVER },
        );
    }
    let top = Rect::from_min_size(rect.min + Vec2::new(2.0, 1.0), Vec2::new(20.0, 18.0));
    if let Some(t) = letter {
        ui.painter().text(
            egui::pos2(top.center().x, top.center().y - 1.0),
            Align2::CENTER_CENTER,
            t,
            FontId::proportional(13.0),
            text_color(&v),
        );
    }
    if let Some(ic) = icon {
        ui_icons::draw_excel(ui.painter(), top.shrink(2.0), ic, text_color(&v), true);
    }
    // 色条左收 + 右下角小三角：Excel 的颜色按钮是「图标 + 色条 + 下拉箭头」
    let bar = Rect::from_min_size(
        egui::pos2(rect.left() + 4.0, rect.bottom() - 4.0),
        Vec2::new(12.0, 3.0),
    );
    ui.painter().rect_filled(bar, 1.0, color);
    let ac = Color32::from_rgb(68, 68, 68);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(rect.right() - 7.0, rect.bottom() - 6.0),
            egui::pos2(rect.right() - 2.0, rect.bottom() - 6.0),
            egui::pos2(rect.right() - 2.0, rect.bottom() - 2.0),
        ],
        ac,
        Stroke::NONE,
    ));
    resp.on_hover_text(tip)
}

/// 字体类开关（B / I / U）：文字本身就是图标
pub fn text_toggle(
    ui: &mut egui::Ui,
    label: &str,
    on: bool,
    tip: &str,
    strong: bool,
    italic: bool,
    underline: bool,
) -> Response {
    let v = ui.visuals().clone();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    let bg = if on {
        if v.dark_mode { v.selection.bg_fill } else { PRESSED }
    } else if resp.hovered() {
        if v.dark_mode { v.widgets.hovered.weak_bg_fill } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let col = text_color(&v);
    let _ = italic;
    let pos = rect.center() - Vec2::new(0.0, 0.5);
    ui.painter()
        .text(pos, Align2::CENTER_CENTER, label, FontId::proportional(13.5), col);
    if strong {
        // 小字号下用 0.55px 重描模拟加粗，比换字体更稳定
        ui.painter().text(
            pos + Vec2::new(0.55, 0.0),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(13.5),
            col,
        );
    }
    if underline {
        let uy = pos.y + 7.5;
        ui.painter().line_segment(
            [egui::pos2(pos.x - 5.0, uy), egui::pos2(pos.x + 5.0, uy)],
            Stroke::new(1.4, col),
        );
    }
    resp.on_hover_text(tip)
}

/// 文件页签常驻绿色文字
pub fn tab_ex(ui: &mut egui::Ui, name: &str, active: bool, green: bool) -> Response {
    let v = ui.visuals().clone();
    let font = FontId::proportional(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font.clone(), Color32::PLACEHOLDER);
    let size = Vec2::new(galley.size().x + 20.0, TAB_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    // Excel 的活动页签是白底：和下面的功能区连成一片，只有底部 2px 绿线
    if active && !green {
        ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
    }
    if resp.hovered() && !active {
        ui.painter().rect_filled(
            rect.shrink2(Vec2::new(4.0, 3.0)),
            2.0,
            v.widgets.hovered.weak_bg_fill,
        );
    }
    let col = if green {
        ACCENT
    } else if active {
        TEXT
    } else if v.dark_mode {
        v.weak_text_color()
    } else {
        Color32::from_rgb(68, 68, 68)
    };
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, name, font, col);
    if active {
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.bottom() - 2.0),
            Vec2::new(rect.width(), 2.0),
        );
        ui.painter().rect_filled(bar, 0.0, ACCENT);
    }
    resp
}

/// 分组之间的竖细线（功能区内手动用）
pub fn hairline(ui: &mut egui::Ui) {
    let v = ui.visuals().clone();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, GROUP_BODY_H), Sense::hover());
    ui.painter().rect_filled(
        rect,
        0.0,
        if v.dark_mode {
            v.widgets.noninteractive.bg_stroke.color
        } else {
            SEP
        },
    );
}

/// 对话框底部的按钮行（右对齐）
pub fn footer(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(4.0);
    ui.separator();
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        add(ui);
    });
}

/// 主按钮（确定/应用）：Excel 绿
pub fn primary_button(ui: &mut egui::Ui, label: &str) -> Response {
    let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE).size(12.0))
        .fill(ACCENT)
        .corner_radius(2.0)
        .min_size(Vec2::new(64.0, 24.0));
    ui.add(btn)
}

/// 紧凑文字按钮（组里放"插入行"这种短命令）
pub fn pill(ui: &mut egui::Ui, label: &str, tip: &str) -> Response {
    let v = ui.visuals().clone();
    let font = FontId::proportional(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::PLACEHOLDER);
    let size = Vec2::new(galley.size().x + 12.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let bg = if resp.is_pointer_button_down_on() {
        if v.dark_mode { v.selection.bg_fill } else { PRESSED }
    } else if resp.hovered() {
        if v.dark_mode { v.widgets.hovered.weak_bg_fill } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 3.0, bg);
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

/// 工作表标签：Excel 365 —— 32 高，活动页白底 + 底部 2px 绿线
pub fn sheet_tab(
    ui: &mut egui::Ui,
    name: &str,
    active: bool,
    color: Option<[u8; 3]>,
) -> Response {
    let v = ui.visuals().clone();
    let font = FontId::proportional(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font.clone(), Color32::PLACEHOLDER);
    let size = Vec2::new(galley.size().x + 26.0, 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let tab_rgb = color.map(|c| Color32::from_rgb(c[0], c[1], c[2]));
    if let Some(fill) = tab_rgb {
        // 有颜色的标签：整块铺色；深色配白字
        ui.painter().rect_filled(rect, 0.0, fill);
        let lum = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
        let mut color = if lum > 165.0 {
            Color32::from_rgb(30, 30, 30)
        } else {
            Color32::WHITE
        };
        if !active {
            color = color.gamma_multiply(0.92);
        }
        ui.painter()
            .text(rect.center(), Align2::CENTER_CENTER, name, font, color);
        return resp;
    }
    if active {
        ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.bottom() - 2.0),
            Vec2::new(rect.width(), 2.0),
        );
        ui.painter().rect_filled(bar, 0.0, ACCENT);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, v.widgets.hovered.weak_bg_fill);
    }
    let color = if active {
        TEXT
    } else if v.dark_mode {
        v.weak_text_color()
    } else {
        Color32::from_rgb(68, 68, 68)
    };
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, name, font, color);
    resp
}

/// 统一样式的对话框窗口：固定内边距、首次出现在屏幕中间、可拖动、
/// 不折叠。宽度由调用方给（同一类对话框用同一个宽度）。
pub fn window<'a>(
    ctx: &egui::Context,
    title: impl Into<String>,
    width: f32,
) -> egui::Window<'a> {
    let title = title.into();
    let screen = ctx.screen_rect();
    let pos = screen.center() - egui::vec2(width / 2.0, 200.0);
    egui::Window::new(title.clone())
        .id(egui::Id::new(("dlg", title)))
        .collapsible(false)
        .default_width(width)
        .default_pos(pos)
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(Color32::WHITE)
                .stroke(Stroke::new(1.0, BORDER))
                .inner_margin(egui::Margin {
                    left: 14,
                    right: 14,
                    top: 8,
                    bottom: 12,
                }),
        )
}

/// 表单的一行：定宽字段名 + 内容
pub fn field(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(64.0, 22.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.label(RichText::new(label).size(12.0).weak());
            },
        );
        add(ui);
    });
}
