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
/// 禁用态：Excel 的禁用图标是中性灰 #A6A6A6，比正文浅、比分隔线深
pub const DISABLED: Color32 = Color32::from_rgb(166, 166, 166);
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
pub const HOVER: Color32 = Color32::from_rgb(243, 242, 241);
pub const PRESSED: Color32 = Color32::from_rgb(237, 235, 233);
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
        (egui::TextStyle::Heading, FontId::proportional(13.0)),
        (egui::TextStyle::Body, FontId::proportional(12.0)),
        (egui::TextStyle::Button, FontId::proportional(12.0)),
        (egui::TextStyle::Small, FontId::proportional(11.0)),
        (egui::TextStyle::Monospace, FontId::monospace(12.0)),
    ]
    .into();
    base.spacing.item_spacing = Vec2::new(4.0, 3.0);
    base.spacing.button_padding = Vec2::new(7.0, 2.0);
    base.spacing.interact_size.y = 24.0;
    base.spacing.scroll.floating = false;
    base.spacing.scroll.bar_width = 12.0;
    base.spacing.scroll.bar_inner_margin = 2.0;
    base.spacing.scroll.bar_outer_margin = 0.0;
    base.spacing.scroll.handle_min_length = 24.0;
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
        v.widgets.inactive.bg_fill = Color32::from_rgb(243, 242, 241);
        v.widgets.inactive.weak_bg_fill = Color32::WHITE;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(171, 169, 167));
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.corner_radius = egui::CornerRadius::same(2);
        v.widgets.hovered.bg_fill = Color32::from_rgb(237, 235, 233);
        v.widgets.hovered.weak_bg_fill = HOVER;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(140, 138, 136));
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.corner_radius = egui::CornerRadius::same(2);
        v.widgets.active.bg_fill = Color32::from_rgb(225, 223, 221);
        v.widgets.active.weak_bg_fill = PRESSED;
        v.widgets.active.bg_stroke = Stroke::new(1.5, ACCENT);
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
        // Excel 的组是紧挨着的：分隔线画在两组交界处，不额外占宽
        let x = ui.cursor().left();
        let y = ui.cursor().top();
        let v = ui.visuals().clone();
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(x, y + crate::tokens::space::GROUP_PAD_TOP),
                Vec2::new(crate::tokens::stroke::HAIR, GROUP_BODY_H),
            ),
            0.0,
            if v.dark_mode {
                Color32::from_rgb(58, 64, 74)
            } else {
                SEP
            },
        );
    }
    let out = ui
        .vertical(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(crate::tokens::space::HAIR, 0.0);
            // Excel 的命令区从分组顶往下 4px 才开始
            ui.add_space(crate::tokens::space::GROUP_PAD_TOP);
            let row = ui.horizontal_top(|ui| {
                ui.add_space(crate::tokens::space::GROUP_PAD_X);
                ui.set_height(GROUP_BODY_H);
                add(ui);
                ui.allocate_exact_size(
                    Vec2::new(crate::tokens::space::GROUP_PAD_X, 0.0),
                    Sense::hover(),
                );
            });
            let w = row.response.rect.width().max(crate::tokens::space::XXXL);
            // Excel 的组名紧贴功能区底边：4(顶) + 76(命令) + 7(间隙) + 14(字) = 101
            ui.add_space(crate::tokens::space::GROUP_LABEL_GAP);
            ui.allocate_ui_with_layout(
                Vec2::new(w, GROUP_LABEL_H),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    let c = if ui.visuals().dark_mode {
                        ui.visuals().weak_text_color()
                    } else {
                        Color32::from_rgb(89, 89, 89)
                    };
                    ui.label(RichText::new(title).size(crate::tokens::text::BODY).color(c));
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
    ui.add_space(crate::tokens::space::HAIR);
    group_rect
}

/// 分组的“对话框启动器”：Excel 在分组右下角的斜箭头
pub fn launcher_at(ui: &mut egui::Ui, group_rect: Rect, id: &str) -> bool {
    let s = crate::tokens::icon::XS;
    let size = Vec2::new(s, s);
    let rect = Rect::from_min_size(
        egui::pos2(
            group_rect.right() - size.x - crate::tokens::stroke::HAIR,
            group_rect.bottom() - size.y - crate::tokens::stroke::HAIR,
        ),
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
        ui.painter()
        .rect_filled(rect, crate::tokens::radius::MD, bg);
    }
    focus_ring(ui, &resp, rect, 3.0);
    let col = if enabled {
        text_color(&v)
    } else {
        DISABLED
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
    // Excel 的大 split 是底部一条 12px 的箭头带，不是右侧竖条
    let arrow_rect = Rect::from_min_size(
        egui::pos2(rect.left(), rect.bottom() - 12.0),
        Vec2::new(rect.width(), 12.0),
    );
    let aresp = ui.interact(
        arrow_rect,
        egui::Id::new(("bigsplit", rect.left().to_bits(), rect.top().to_bits())),
        Sense::click(),
    );
    let over_arrow = arrow_rect.contains(resp.hover_pos().unwrap_or(rect.center()));
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if resp.is_pointer_button_down_on() || aresp.is_pointer_button_down_on() {
        if v.dark_mode { v.selection.bg_fill } else { PRESSED }
    } else if (resp.hovered() && !over_arrow) || aresp.hovered() {
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
        DISABLED
    };
    let icon_size = 32.0_f32.min(size.y - 22.0).max(16.0);
    let icon_rect = Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 6.0 + icon_size / 2.0),
        Vec2::splat(icon_size),
    );
    ui_icons::draw_excel(ui.painter(), icon_rect, icon, col, enabled);
    ui.painter().text(
        egui::pos2(rect.center().x, rect.bottom() - 22.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.0),
        col,
    );
    ui.painter().line_segment(
        [
            egui::pos2(rect.left() + 2.0, rect.bottom() - 12.0),
            egui::pos2(rect.right() - 2.0, rect.bottom() - 12.0),
        ],
        Stroke::new(1.0, BORDER),
    );
    caret(
        ui,
        egui::pos2(rect.center().x, rect.bottom() - 6.0),
        col,
    );
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
        DISABLED
    } else {
        text_color(&v)
    };
    ui_icons::draw_excel(ui.painter(), rect.shrink(4.0), icon, col, enabled);
    resp.on_hover_text(tip)
}

/// 键盘焦点圈：功能区自绘按钮此前完全没有焦点指示（QA-05.3 五态里缺 focus）。
/// 只在拿到焦点时画，1.5px 主题绿内描边，和 hover/pressed 的底色互不冲突。
pub fn focus_ring(ui: &egui::Ui, resp: &egui::Response, rect: egui::Rect, radius: f32) {
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            radius,
            Stroke::new(1.5, ACCENT),
            egui::StrokeKind::Inside,
        );
    }
}

/// 勾选行：自绘复选框（白底/勾选填主题绿+白勾），不再用「✓」字符
pub fn check_row(ui: &mut egui::Ui, label: &str, checked: bool, enabled: bool, width: f32) -> Response {
    let v = ui.visuals().clone();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::click());
    let bg = state_bg(enabled, false, resp.hovered(), resp.is_pointer_button_down_on(), v.dark_mode);
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 2.0, bg);
    }
    let box_rect = Rect::from_center_size(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        Vec2::splat(13.0),
    );
    if checked && enabled {
        ui.painter().rect_filled(box_rect, 2.0, ACCENT);
        let p = ui.painter();
        let st = Stroke::new(1.8, Color32::WHITE);
        p.line_segment(
            [
                egui::pos2(box_rect.left() + 3.0, box_rect.center().y),
                egui::pos2(box_rect.left() + 5.6, box_rect.bottom() - 3.4),
            ],
            st,
        );
        p.line_segment(
            [
                egui::pos2(box_rect.left() + 5.6, box_rect.bottom() - 3.4),
                egui::pos2(box_rect.right() - 2.6, box_rect.top() + 3.6),
            ],
            st,
        );
    } else {
        ui.painter().rect(
            box_rect,
            2.0,
            Color32::WHITE,
            Stroke::new(1.0, if enabled { Color32::from_rgb(171, 169, 167) } else { Color32::from_rgb(210, 210, 210) }),
            egui::StrokeKind::Inside,
        );
    }
    let fg = if enabled { text_color(&v) } else { DISABLED };
    ui.painter().text(
        egui::pos2(rect.left() + 26.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        fg,
    );
    resp
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
        DISABLED
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
    // 细 V 形折线（Excel 下拉箭头），1px 描边
    let st = Stroke::new(1.0, color);
    let (w, h) = (2.5, 1.5);
    ui.painter()
        .line_segment([center + Vec2::new(-w, -h), center + Vec2::new(0.0, h)], st);
    ui.painter()
        .line_segment([center + Vec2::new(0.0, h), center + Vec2::new(w, -h)], st);
}

/// 右向细折线箭头（子菜单）
pub fn caret_right(ui: &mut egui::Ui, center: egui::Pos2, color: Color32) {
    let st = Stroke::new(1.0, color);
    ui.painter()
        .line_segment([center + Vec2::new(-1.5, -2.5), center + Vec2::new(1.5, 0.0)], st);
    ui.painter()
        .line_segment([center + Vec2::new(1.5, 0.0), center + Vec2::new(-1.5, 2.5)], st);
}



/// 控件状态底色：禁用不给底、按下/选中用 PRESSED、悬停用 HOVER
pub fn state_bg(enabled: bool, on: bool, hovered: bool, down: bool, dark: bool) -> Color32 {
    if !enabled {
        return Color32::TRANSPARENT;
    }
    if down || on {
        if dark { Color32::from_rgb(62, 72, 86) } else { PRESSED }
    } else if hovered {
        if dark { Color32::from_rgb(55, 62, 72) } else { HOVER }
    } else {
        Color32::TRANSPARENT
    }
}

/// 拆分按钮右侧的小箭头按钮：悬停底色 + 细折线箭头
pub fn arrow_glyph_button(ui: &mut egui::Ui, size: Vec2, color: Color32) -> Response {
    let v = ui.visuals().clone();
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let bg = state_bg(
        enabled,
        false,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        v.dark_mode,
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect.shrink(1.0), 2.0, bg);
    }
    let c = if enabled { color } else { DISABLED };
    caret(ui, rect.center(), c);
    resp
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
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    let bg = state_bg(
        enabled,
        false,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        v.dark_mode,
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 2.0, bg);
    }
    focus_ring(ui, &resp, rect, 2.0);
    let fg = if enabled { text_color(&v) } else { DISABLED };
    let top = Rect::from_min_size(rect.min + Vec2::new(2.0, 1.0), Vec2::new(20.0, 18.0));
    if let Some(t) = letter {
        ui.painter().text(
            egui::pos2(top.center().x, top.center().y - 1.0),
            Align2::CENTER_CENTER,
            t,
            FontId::proportional(13.0),
            fg,
        );
    }
    if let Some(ic) = icon {
        ui_icons::draw_excel(ui.painter(), top.shrink(2.0), ic, fg, enabled);
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
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    let bg = state_bg(
        enabled,
        on,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        v.dark_mode,
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    focus_ring(ui, &resp, rect, 3.0);
    let col = if enabled { text_color(&v) } else { DISABLED };
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

/// 同 `a11y_name`，但可访问矩形单独给（拆分按钮的主体不能把箭头区也算进去）
pub fn a11y_name_rect(
    resp: &egui::Response,
    label: &str,
    enabled: bool,
    rect: egui::Rect,
    ctx: &egui::Context,
) {
    a11y_name(resp, label, enabled, None);
    // rect 是 egui 的逻辑点：accesskit 树由 egui 统一乘 pixels_per_point，
    // 这里再乘一次就会在 150%/200% 下把可访问矩形放大 ppp 倍（QA-05.2 只测 100% 时看不见）。
    let b = egui::accesskit::Rect {
        x0: rect.min.x as f64,
        y0: rect.min.y as f64,
        x1: rect.max.x as f64,
        y1: rect.max.y as f64,
    };
    let _ = ctx.accesskit_node_builder(resp.id, |node| {
        node.set_bounds(b);
    });
}

/// UI-24.2：给自绘控件指定可访问角色（Tab 页、菜单项等）
pub fn a11y_role(
    resp: &egui::Response,
    label: &str,
    role: egui::accesskit::Role,
    selected: Option<bool>,
) {
    let text = label.to_string();
    let text2 = text.clone();
    resp.widget_info(move || {
        egui::WidgetInfo::labeled(egui::WidgetType::SelectableLabel, true, text2.clone())
    });
    let _ = resp.ctx.accesskit_node_builder(resp.id, |node| {
        node.set_role(role);
        node.set_label(text);
        if let Some(v) = selected {
            node.set_selected(v);
        }
    });
}

/// 只拖动、不参与 Tab 焦点的 Sense（标题栏、缩放手柄、模态遮挡层都该用它）
pub fn drag_only_sense() -> egui::Sense {
    egui::Sense::from_bits_truncate(
        egui::Sense::click_and_drag().bits() & !egui::Sense::FOCUSABLE.bits(),
    )
}

/// UI-24.2：给自绘控件补可访问名字/角色/状态。
/// 自绘控件（pill、功能区大按钮）在 UIA 里默认只是个空图元，Narrator 读不到名字。
pub fn a11y_name(resp: &egui::Response, label: &str, enabled: bool, toggled: Option<bool>) {
    let text = label.to_string();
    let text2 = text.clone();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text2.clone()));
    let _ = resp.ctx.accesskit_node_builder(resp.id, |node| {
        node.set_role(egui::accesskit::Role::Button);
        node.set_label(text);
        if !enabled {
            node.set_disabled();
        }
        if let Some(t) = toggled {
            node.set_toggled(if t {
                egui::accesskit::Toggled::True
            } else {
                egui::accesskit::Toggled::False
            });
        }
    });
}

/// 主按钮（确定/应用）：Excel 绿
pub struct BtnResp {
    pub clicked: bool,
}
impl BtnResp {
    pub fn clicked(&self) -> bool {
        self.clicked
    }
}

pub fn primary_button(ui: &mut egui::Ui, label: &str) -> BtnResp {
    let btn = egui::Button::new(
        RichText::new(label)
            .color(ACCENT)
            .size(crate::tokens::text::BODY),
    )
    .fill(Color32::WHITE)
    .stroke(Stroke::new(crate::tokens::stroke::HAIR, ACCENT))
    .corner_radius(crate::tokens::radius::SM)
    .min_size(Vec2::new(
        crate::tokens::size::BTN_MIN_W,
        crate::tokens::size::BTN_H,
    ));
    let resp = ui.add(btn);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            resp.rect.shrink(crate::tokens::stroke::FOCUS),
            crate::tokens::stroke::FOCUS,
            Stroke::new(
                crate::tokens::stroke::HAIR,
                ACCENT.gamma_multiply(0.6),
            ),
            egui::StrokeKind::Inside,
        );
    }
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter))
        && ui.ctx().memory(|m| m.focused().is_none())
        && !ui.ctx().memory(|m| m.any_popup_open());
    BtnResp {
        clicked: resp.clicked() || enter,
    }
}

/// 次要按钮（取消/关闭）：白底灰边，和 Excel 对话框一致
pub fn secondary_button(ui: &mut egui::Ui, label: &str) -> Response {
    let btn = egui::Button::new(
        RichText::new(label)
            .color(TEXT)
            .size(crate::tokens::text::BODY),
    )
    .fill(Color32::WHITE)
    .stroke(Stroke::new(
        crate::tokens::stroke::HAIR,
        Color32::from_rgb(179, 177, 175),
    ))
    .corner_radius(crate::tokens::radius::SM)
    .min_size(Vec2::new(
        crate::tokens::size::BTN_MIN_W,
        crate::tokens::size::BTN_H,
    ));
    ui.add(btn)
}

/// 紧凑文字按钮（组里放"插入行"这种短命令）
pub fn pill(ui: &mut egui::Ui, label: &str, tip: &str) -> Response {
    let v = ui.visuals().clone();
    let font = FontId::proportional(crate::tokens::text::BODY);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::PLACEHOLDER);
    let size = Vec2::new(
        galley.size().x + crate::tokens::space::XL,
        crate::tokens::size::BTN_H,
    );
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
    a11y_name(&resp, if tip.is_empty() { label } else { tip }, true, None);
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
    let font = FontId::proportional(crate::tokens::text::BODY);
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font.clone(), Color32::PLACEHOLDER);
    let size = Vec2::new(
        galley.size().x + crate::tokens::size::SHEET_TAB_PAD,
        crate::tokens::size::SHEET_TAB_H,
    );
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    // UI-24.2：工作表标签在 UIA 里是 Tab 页，名字就是工作表名
    a11y_role(&resp, name, egui::accesskit::Role::Tab, Some(active));
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

/// 统一样式的对话框窗口：自绘标题栏（左对齐标题 + 关闭按钮 + 可拖动），
/// 白底、1px 边框、圆角、投影；不再使用 egui 自带标题栏。
/// 本帧是否画过 ui_kit 模态对话框。命令层用它判断「模态对话框是否占着键盘」，
/// 不用逐个枚举 50 个对话框字段——只要对话框还开着，`Dlg::show` 每帧都会打标。
const DIALOG_SHOWN: &str = "ui_kit_dialog_shown";

fn dialog_mark() -> egui::Id {
    egui::Id::new(DIALOG_SHOWN)
}

/// 读一次即清：对话框关掉后标记不会残留到下一帧。
pub fn take_dialog_shown(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(dialog_mark()).unwrap_or(false))
}

pub struct Dlg {
    title: String,
    width: f32,
    height: f32,
    open: Option<*mut bool>,
    resizable: bool,
    anchor: Option<(egui::Align2, [f32; 2])>,
}

/// UI-13.3：窗口宽度不超过可用屏幕（左右各留 8px），避免小窗口下对话框被裁掉。
pub fn dialog_width(width: f32, screen: egui::Rect) -> f32 {
    width.clamp(200.0, (screen.width() - 16.0).max(200.0))
}

pub fn window(_ctx: &egui::Context, title: impl Into<String>, width: f32) -> Dlg {
    Dlg {
        title: title.into(),
        width,
        height: 0.0,
        open: None,
        resizable: false,
        anchor: None,
    }
}

impl Dlg {
    pub fn open(mut self, open: &mut bool) -> Self {
        self.open = Some(open as *mut bool);
        self
    }
    pub fn default_width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }
    pub fn min_width(self, _w: f32) -> Self {
        self
    }
    pub fn default_size(mut self, s: [f32; 2]) -> Self {
        self.width = s[0];
        self.height = s[1];
        self
    }
    pub fn collapsible(self, _v: bool) -> Self {
        self
    }
    pub fn resizable(mut self, v: bool) -> Self {
        self.resizable = v;
        self
    }
    pub fn anchor(mut self, a: egui::Align2, offset: [f32; 2]) -> Self {
        self.anchor = Some((a, offset));
        self
    }
    pub fn frame(self, _f: egui::Frame) -> Self {
        self
    }

    pub fn show<R>(
        mut self,
        ctx: &egui::Context,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<R>> {
        let id = egui::Id::new(("dlgwin", &self.title));
        // UI-24.1：记住打开前的焦点，关闭时还回去
        let now0 = ctx.input(|i| i.time);
        let seen = ctx.data(|d| d.get_temp::<f64>(id.with("shown"))).unwrap_or(-10.0);
        if now0 - seen > 0.5 {
            let before = ctx.memory(|m| m.focused());
            ctx.data_mut(|d| {
                if let Some(f) = before {
                    d.insert_temp(id.with("focus_before"), f);
                } else {
                    d.remove::<egui::Id>(id.with("focus_before"));
                }
            });
        }
        // UI-24.2：对话框在可访问树里要有 Dialog 角色和标题
        let title = self.title.clone();
        let _ = ctx.accesskit_node_builder(id.with("a11y"), |node| {
            node.set_role(egui::accesskit::Role::Dialog);
            node.set_label(title);
        });
        ctx.data_mut(|d| d.insert_temp(dialog_mark(), true));
        let screen = ctx.screen_rect();
        let mut pos = ctx
            .data(|d| d.get_temp::<egui::Pos2>(id.with("pos")))
            .unwrap_or_else(|| {
                let (a, off) = self
                    .anchor
                    .unwrap_or((egui::Align2::CENTER_CENTER, [0.0, 0.0]));
                let p = a.align_size_within_rect(
                    egui::vec2(self.width, 240.0),
                    screen,
                );
                p.min + egui::vec2(off[0], off[1])
            });
        let w = ctx
            .data(|d| d.get_temp::<f32>(id.with("w")))
            .unwrap_or(self.width);
        let mut w = dialog_width(w, screen);
        pos.x = pos.x.clamp(screen.left() + 4.0, (screen.right() - w - 4.0).max(screen.left() + 4.0));
        pos.y = pos.y.clamp(screen.top() + 4.0, (screen.bottom() - 40.0).max(screen.top() + 4.0));
        let mut h = ctx
            .data(|d| d.get_temp::<f32>(id.with("h")))
            .unwrap_or(self.height);
        let mut closed = false;
        let now = ctx.input(|i| i.time);
        let last = ctx
            .data(|d| d.get_temp::<f64>(id.with("shown")))
            .unwrap_or(-10.0);
        let born = if now - last > 0.5 {
            now
        } else {
            ctx.data(|d| d.get_temp::<f64>(id.with("born")))
                .unwrap_or(now)
        };
        ctx.data_mut(|d| {
            d.insert_temp(id.with("born"), born);
            d.insert_temp(id.with("shown"), now);
            d.insert_temp(egui::Id::new("dlg_born"), born);
        });
        // 模态遮挡：对话框打开时吞掉背景点击
        egui::Area::new(id.with("block"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let r = egui::Rect::from_min_size(screen.min, screen.size());
                // 遮挡层只吞点击，不参与 Tab 焦点（否则 Tab 会卡在它上面，进不到对话框里）
                let resp = ui.interact(r, id.with("blocki"), drag_only_sense());
                let _ = resp;
            });
        let out = egui::Area::new(id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                let frame = egui::Frame::window(&ctx.style())
                    .fill(Color32::WHITE)
                    .stroke(Stroke::NONE)
                    .corner_radius(4.0)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 18,
                        spread: 0,
                        color: Color32::from_black_alpha(32),
                    })
                    .inner_margin(egui::Margin::same(0));
                let fr = frame
                    .show(ui, |ui| {
                        ui.set_width(w);
                        // 标题栏
                        // 标题栏只用来拖动，不该占 Tab 焦点
                        let (trect, tresp) = ui.allocate_exact_size(
                            egui::vec2(w, 30.0),
                            drag_only_sense(),
                        );
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                trect.min,
                                egui::vec2(w, 30.0),
                            ),
                            0.0,
                            Color32::WHITE,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(trect.left(), trect.bottom() - 0.5),
                                egui::pos2(trect.right(), trect.bottom() - 0.5),
                            ],
                            Stroke::new(1.0 / ctx.pixels_per_point(), Color32::from_rgb(225, 223, 221)),
                        );
                        ui.painter().text(
                            egui::pos2(trect.left() + 12.0, trect.center().y),
                            egui::Align2::LEFT_CENTER,
                            &self.title,
                            FontId::proportional(12.5),
                            TEXT,
                        );
                        // 关闭按钮
                        let crect = egui::Rect::from_min_size(
                            egui::pos2(trect.right() - 46.0, trect.top()),
                            egui::vec2(46.0, 30.0),
                        );
                        let cresp = ui.interact(
                            crect,
                            id.with("close"),
                            egui::Sense::click(),
                        );
                        if cresp.hovered() {
                            ui.painter()
                                .rect_filled(crect, 0.0, Color32::from_rgb(232, 17, 35));
                        }
                        let cc = cresp.hovered().then_some(Color32::WHITE).unwrap_or(TEXT);
                        let k = 4.5;
                        let st = Stroke::new(1.1, cc);
                        ui.painter().line_segment(
                            [crect.center() + egui::vec2(-k, -k), crect.center() + egui::vec2(k, k)],
                            st,
                        );
                        ui.painter().line_segment(
                            [crect.center() + egui::vec2(k, -k), crect.center() + egui::vec2(-k, k)],
                            st,
                        );
                        let cresp = cresp.on_hover_text("关闭");
                        if cresp.clicked() {
                            closed = true;
                        }
                        if tresp.dragged() {
                            pos += tresp.drag_delta();
                        }
                        // 内容
                        let body = egui::Frame::none().inner_margin(egui::Margin {
                            left: 16,
                            right: 16,
                            top: 10,
                            bottom: 12,
                        });
                        let inner = body
                            .show(ui, |ui| {
                                ui.set_width(w - 32.0);
                                ui.set_max_width(w - 32.0);
                                ui.spacing_mut().interact_size.y = 20.0;
                                if h > 0.0 {
                                    egui::ScrollArea::vertical()
                                        .max_height(h)
                                        .show(ui, add)
                                        .inner
                                } else {
                                    add(ui)
                                }
                            })
                            .inner;
                        if self.resizable {
                            let bottom = ui.min_rect().bottom();
                            let gr = egui::Rect::from_min_size(
                                egui::pos2(trect.right() - 14.0, bottom - 14.0),
                                egui::vec2(14.0, 14.0),
                            );
                            let g = ui.interact(gr, id.with("grip"), drag_only_sense());
                            let gc = Color32::from_rgb(160, 158, 156);
                            for k in 0..3 {
                                let o = 3.0 + k as f32 * 3.0;
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(gr.right() - o, gr.bottom() - 1.0),
                                        egui::pos2(gr.right() - 1.0, gr.bottom() - o),
                                    ],
                                    Stroke::new(1.0, gc),
                                );
                            }
                            if g.dragged() {
                                let d = g.drag_delta();
                                w = (w + d.x).max(220.0);
                                let cur_h = if h > 0.0 { h } else { bottom - trect.bottom() };
                                h = (cur_h + d.y).max(120.0);
                            }
                        }
                        (inner, tresp)
                    });
                ui.painter().rect_stroke(
                    fr.response.rect.shrink(0.5),
                    4.0,
                    Stroke::new(1.0 / ctx.pixels_per_point(), Color32::from_rgb(179, 177, 175)),
                    egui::StrokeKind::Inside,
                );
                // UI-24.1：Tab/Shift+Tab 只在对话框这一层里转，不会跑到背景网格。
                // 关闭这一帧要把模态层降回背景，否则下一帧背景控件仍被判“被模态挡住”，
                // 会把刚还回去的焦点又 surrender 掉。
                let closing = closed || ctx.input(|i| i.key_pressed(egui::Key::Escape));
                let layer = ui.layer_id();
                ui.memory_mut(|m| {
                    if closing {
                        m.set_modal_layer(egui::LayerId::background());
                    } else {
                        m.set_modal_layer(layer);
                    }
                });
                fr.inner
            });
        if closed || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Some(p) = self.open {
                unsafe { *p = false; }
            }
            // UI-24.1：关闭后焦点回到触发它的控件
            if let Some(f) = ctx
                .data(|d| d.get_temp::<egui::Id>(id.with("focus_before")))
            {
                ctx.memory_mut(|m| m.request_focus(f));
                ctx.data_mut(|d| d.remove::<egui::Id>(id.with("focus_before")));
            }
        }
        ctx.data_mut(|d| {
            d.insert_temp(id.with("pos"), pos);
            d.insert_temp(id.with("w"), w);
            d.insert_temp(id.with("h"), h);
        });
        let _ = self.resizable;
        Some(egui::InnerResponse::new(out.inner.0, out.response))
    }
}

/// 对话框刚打开时把焦点给第一个输入框
pub fn autofocus(ui: &mut egui::Ui, resp: &Response) {
    let now = ui.input(|i| i.time);
    let born = ui
        .ctx()
        .data(|d| d.get_temp::<f64>(egui::Id::new("dlg_born")))
        .unwrap_or(-10.0);
    if now - born < 0.6 && ui.ctx().memory(|m| m.focused().is_none()) {
        resp.request_focus();
    }
}

/// 带常驻上下微调箭头的数字框（20px 高）
pub fn spin(ui: &mut egui::Ui, v: &mut f64, min: f64, max: f64, step: f64, width: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 20.0), Sense::click());
    let v0 = *v;
    let a = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 12.0, rect.top()),
        Vec2::new(12.0, 10.0),
    );
    let b = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 12.0, rect.top() + 10.0),
        Vec2::new(12.0, 10.0),
    );
    let ar = ui.interact(a, ui.id().with(("spinup", rect.left() as i32)), Sense::click());
    let br = ui.interact(b, ui.id().with(("spindn", rect.left() as i32)), Sense::click());
    if ar.clicked() {
        *v = (*v + step).min(max);
    }
    if br.clicked() {
        *v = (*v - step).max(min);
    }
    ui.painter().rect(
        rect,
        2.0,
        Color32::WHITE,
        Stroke::new(1.0, Color32::from_rgb(171, 169, 167)),
        egui::StrokeKind::Inside,
    );
    let txt = if (step.fract()).abs() < 1e-9 {
        format!("{:.0}", *v)
    } else {
        format!("{:.1}", *v)
    };
    ui.painter().text(
        egui::pos2(rect.left() + 6.0, rect.center().y),
        Align2::LEFT_CENTER,
        txt,
        FontId::proportional(12.0),
        TEXT,
    );
    let cc = Color32::from_rgb(68, 68, 68);
    let st = Stroke::new(1.0, cc);
    for (r, up) in [(a, true), (b, false)] {
        if (up && ar.hovered()) || (!up && br.hovered()) {
            ui.painter().rect_filled(r, 0.0, HOVER);
        }
        let c = r.center();
        let dy = if up { 1.5 } else { -1.5 };
        ui.painter().line_segment(
            [egui::pos2(c.x - 3.0, c.y + dy), egui::pos2(c.x + 3.0, c.y - dy)],
            st,
        );
    }
    ui.painter().line_segment(
        [egui::pos2(a.left(), a.bottom()), egui::pos2(a.right(), a.bottom())],
        Stroke::new(1.0, Color32::from_rgb(200, 198, 196)),
    );
    let _ = resp;
    (*v - v0).abs() > 1e-9
}

/// 对话框标签页（Excel 的文件夹式页签）
pub fn dialog_tabs(ui: &mut egui::Ui, sel: &mut u8, tabs: &[&str]) {
    let top = ui.cursor().top();
    let left = ui.cursor().left();
    let right = left + ui.available_width();
    ui.painter().line_segment(
        [egui::pos2(left, top + 25.5), egui::pos2(right, top + 25.5)],
        Stroke::new(1.0, Color32::from_rgb(179, 177, 175)),
    );
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, t) in tabs.iter().enumerate() {
            let galley = ui.painter().layout_no_wrap(
                (*t).to_owned(),
                FontId::proportional(12.0),
                Color32::PLACEHOLDER,
            );
            let w = galley.size().x + 26.0;
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::click());
            let on = *sel as usize == i;
            if on {
                ui.painter().rect(
                    rect,
                    0.0,
                    Color32::WHITE,
                    Stroke::new(1.0, Color32::from_rgb(179, 177, 175)),
                    egui::StrokeKind::Inside,
                );
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(rect.left() + 1.0, rect.bottom() - 2.0),
                        egui::pos2(rect.right() - 1.0, rect.bottom()),
                    ),
                    0.0,
                    Color32::WHITE,
                );
            } else if resp.hovered() {
                ui.painter()
                    .rect_filled(rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            let col = if on {
                TEXT
            } else {
                ui.visuals().weak_text_color()
            };
            ui.painter()
                .text(rect.center(), Align2::CENTER_CENTER, *t, FontId::proportional(12.0), col);
            if resp.clicked() {
                *sel = i as u8;
            }
        }
    });
    ui.add_space(2.0);
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

#[cfg(test)]
mod state_tests {
    use super::*;

    #[test]
    fn disabled_has_no_background() {
        assert_eq!(state_bg(false, false, true, true, false), Color32::TRANSPARENT);
        assert_eq!(state_bg(false, true, true, true, true), Color32::TRANSPARENT);
    }

    #[test]
    fn pressed_beats_hover_and_checked_beats_hover() {
        assert_eq!(state_bg(true, false, true, true, false), PRESSED);
        assert_eq!(state_bg(true, true, true, false, false), PRESSED);
        assert_eq!(state_bg(true, false, true, false, false), HOVER);
        assert_eq!(state_bg(true, false, false, false, false), Color32::TRANSPARENT);
    }
}
