//! 界面基座：照 Excel 365 的尺寸与配色重做。
//!
//! 参考值来自本机 Excel 16.0 的 UIA 实测（96 DPI，窗口客户区坐标）：
//! 标题栏 48、页签行 30、功能区 102、编辑栏 44、列头 19、行头 29、
//! 默认列宽 72、默认行高 19、工作表标签条 32、状态栏 22。
//!
//! 所有尺寸集中在这里，别再往各处写魔法数字。

use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Rect, Response, RichText, Sense, Stroke, Vec2,
};
use crate::ui_icons::{self, Icon};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// Excel 绿色（选项卡下划线、工作表标签下划线、选中框）。
/// 本机 Excel 16.0 实测取色：#217346，选中行列头底线、页签下的线、单元格选中框都是它。
pub const ACCENT: Color32 = Color32::from_rgb(33, 115, 70);
/// 分区分隔线 / 控件边框
pub const BORDER: Color32 = Color32::from_rgb(208, 208, 208);

/// CELLAN 品牌双色：外圈 8 节点深海蓝 + 中心协同核心翡翠绿（取自官方矢量标志）。
pub const BRAND_NAVY: Color32 = Color32::from_rgb(27, 53, 129);
pub const BRAND_MINT: Color32 = Color32::from_rgb(9, 189, 141);

/// 悬浮卡片：纯白四圆角卡片浮于冷灰蓝 PAGE_BG 之上，配清晰利落的 1px CARD_BORDER。
pub const PAGE_BG: Color32 = Color32::from_rgb(240, 243, 248);
pub const CARD_BORDER: Color32 = Color32::from_rgb(205, 213, 224);
/// 页签栏底色（Fluent 架构下直接与 PAGE_BG 融为一体，消除双拼色断层边框）
pub const TABSTRIP_BG: Color32 = Color32::from_rgb(240, 243, 248);
pub const TABSTRIP_LINE: Color32 = Color32::from_rgb(205, 213, 224);
/// 灰底上的悬停色：与冷灰底拉开明确明度差
pub const HOVER_ON_PAGE: Color32 = Color32::from_rgb(225, 231, 240);
/// 卡片圆角：功能区/网格 8px，编辑栏 6px
pub const CARD_R: u8 = 8;

/// 纯矢量绘制 CELLAN 品牌标志（240×240 归一化坐标系，任意尺寸亚像素圆角与抗锯齿）。
/// 结构：3×3 圆角矩阵，外圈 8 格深蓝正交环连，左上独立蓝触点、左下斜向蓝桥接，
/// 中心翡翠绿核心向右上下伸出双绿触点接入右列节点。
pub fn draw_brand_logo(painter: &egui::Painter, rect: Rect) {
    let side = rect.width().min(rect.height());
    if side < 6.0 {
        return;
    }
    let sq = Rect::from_center_size(rect.center(), Vec2::splat(side));
    let tx = |x: f32| sq.left() + x / 240.0 * side;
    let ty = |y: f32| sq.top() + y / 240.0 * side;
    let ts = |v: f32| v / 240.0 * side;

    // 22~28px 小尺寸下略增连线墨量，避免亚像素笔画变淡
    let boost = if side <= 24.0 { 1.22 } else if side <= 32.0 { 1.10 } else { 1.0 };
    let w_orth = ts(8.2 * boost).max(1.1);
    let w_diag = ts(8.4 * boost).max(1.15);
    let r_node = ts(10.4 * if side <= 24.0 { 1.10 } else { 1.0 }).max(1.35);
    let r_cell = ts(13.5);

    let fill_rrect = |p: &egui::Painter, x0: f32, y0: f32, w: f32, h: f32, r: f32, col: Color32| {
        let rx = r.min(w * 0.45).min(h * 0.45);
        let corners = [
            (x0 + w - rx, y0 + rx, -std::f32::consts::FRAC_PI_2),
            (x0 + w - rx, y0 + h - rx, 0.0),
            (x0 + rx, y0 + h - rx, std::f32::consts::FRAC_PI_2),
            (x0 + rx, y0 + rx, std::f32::consts::PI),
        ];
        const SEG: usize = 5;
        let mut pts = Vec::with_capacity(4 * (SEG + 1));
        for (cx, cy, a0) in corners {
            for k in 0..=SEG {
                let a = a0 + std::f32::consts::FRAC_PI_2 * (k as f32 / SEG as f32);
                pts.push(egui::pos2(cx + rx * a.cos(), cy + rx * a.sin()));
            }
        }
        p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
    };

    // 1. 中心翡翠绿斜向双触点与核心格（先画，使右上下斜线自然藏入右列深蓝圆角格底下）
    let st_green = Stroke::new(w_diag, BRAND_MINT);
    painter.line_segment([egui::pos2(tx(136.0), ty(104.0)), egui::pos2(tx(190.0), ty(50.0))], st_green);
    painter.line_segment([egui::pos2(tx(136.0), ty(136.0)), egui::pos2(tx(190.0), ty(190.0))], st_green);
    painter.circle_filled(egui::pos2(tx(163.0), ty(77.0)), r_node, BRAND_MINT);
    painter.circle_filled(egui::pos2(tx(163.0), ty(163.0)), r_node, BRAND_MINT);
    fill_rrect(painter, tx(86.0), ty(86.0), ts(68.0), ts(68.0), r_cell, BRAND_MINT);

    // 2. 外圈正交蓝桥 + 左上触点 + 左下斜桥
    let st_orth = Stroke::new(w_orth, BRAND_NAVY);
    let st_diag = Stroke::new(w_diag, BRAND_NAVY);
    for (x1, y1, x2, y2) in [
        (34.0, 34.0, 206.0, 34.0),
        (34.0, 206.0, 206.0, 206.0),
        (34.0, 34.0, 34.0, 206.0),
        (206.0, 34.0, 206.0, 206.0),
    ] {
        painter.line_segment([egui::pos2(tx(x1), ty(y1)), egui::pos2(tx(x2), ty(y2))], st_orth);
    }
    painter.line_segment([egui::pos2(tx(50.0), ty(50.0)), egui::pos2(tx(77.0), ty(77.0))], st_diag);
    painter.line_segment([egui::pos2(tx(50.0), ty(136.0)), egui::pos2(tx(104.0), ty(190.0))], st_diag);
    painter.circle_filled(egui::pos2(tx(77.0), ty(77.0)), r_node, BRAND_NAVY);
    painter.circle_filled(egui::pos2(tx(77.0), ty(163.0)), r_node, BRAND_NAVY);

    // 3. 外圈 8 枚深蓝圆角单元
    for r in 0..3 {
        for c in 0..3 {
            if (r, c) == (1, 1) {
                continue;
            }
            fill_rrect(
                painter,
                tx(c as f32 * 86.0),
                ty(r as f32 * 86.0),
                ts(68.0),
                ts(68.0),
                r_cell,
                BRAND_NAVY,
            );
        }
    }
}

/// 把一块已经画满（直角）的区域收成圆角卡片：四角先用页面底色「抠」掉，再描 1px 边。
/// egui 的裁切只有矩形，网格、表头、滚动条又各自直角铺色，只能事后盖住四个角。
pub fn card_outline(painter: &egui::Painter, rect: Rect, r: f32, bg: Color32, border: Color32) {
    use egui::epaint::{Mesh, Shape};
    let mut mesh = Mesh::default();
    // (角点, 圆心)：每个角从角点向四分之一圆弧做扇形
    let corners = [
        (rect.left_top(), egui::pos2(rect.left() + r, rect.top() + r), 180.0f32),
        (rect.right_top(), egui::pos2(rect.right() - r, rect.top() + r), 270.0),
        (rect.right_bottom(), egui::pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (rect.left_bottom(), egui::pos2(rect.left() + r, rect.bottom() - r), 90.0),
    ];
    const SEG: usize = 8;
    for (corner, c, start) in corners {
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(corner, bg);
        for k in 0..=SEG {
            let a = (start + 90.0 * k as f32 / SEG as f32).to_radians();
            mesh.colored_vertex(egui::pos2(c.x + r * a.cos(), c.y + r * a.sin()), bg);
        }
        for k in 0..SEG as u32 {
            mesh.add_triangle(base, base + 1 + k, base + 2 + k);
        }
    }
    painter.add(Shape::mesh(mesh));
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(r as u8),
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
}

/// 界面字号只留四档。10.5 / 12.5 / 13.5 这类半像素字号会把中文糊成一层灰，
/// 同一屏里字号档位越多，越像「随手拼出来的页面」。
pub const T_TITLE: f32 = 20.0;
pub const T_H2: f32 = 14.0;
pub const T_BODY: f32 = 12.0;
pub const T_SMALL: f32 = 11.0;

static UI_FONTS_GEN: AtomicU64 = AtomicU64::new(1);

/// 字体集（内嵌字体 / 异步系统字体）重新安装后递增，驱动 `ui_kit` 字族存在缓存失效。
pub fn notify_fonts_changed() {
    UI_FONTS_GEN.fetch_add(1, Ordering::AcqRel);
}

fn fam_default_bold() -> FontFamily {
    static F: OnceLock<FontFamily> = OnceLock::new();
    F.get_or_init(|| FontFamily::Name("default-bold".into())).clone()
}

fn fam_ui_bold() -> FontFamily {
    static F: OnceLock<FontFamily> = OnceLock::new();
    F.get_or_init(|| FontFamily::Name("ui-bold".into())).clone()
}

fn fam_ui_medium() -> FontFamily {
    static F: OnceLock<FontFamily> = OnceLock::new();
    F.get_or_init(|| FontFamily::Name("ui-medium".into())).clone()
}

const MASK_DEFAULT_BOLD: u8 = 1 << 0;
const MASK_UI_BOLD: u8 = 1 << 1;
const MASK_UI_MEDIUM: u8 = 1 << 2;

/// 按 `(UI_FONTS_GEN, system_fonts::generation())` 缓存三个常用 UI 字族是否存在，
/// 消除每次调用 `ctx.fonts(|f| f.families())` 产生的 `Vec<FontFamily>` 堆分配。
fn font_family_mask(ctx: &egui::Context) -> u8 {
    let key = (
        UI_FONTS_GEN.load(Ordering::Acquire),
        crate::system_fonts::generation(),
    );
    let id = egui::Id::new("ui_kit_font_family_mask");
    if let Some((k, mask)) = ctx.data(|d| d.get_temp::<((u64, u64), u8)>(id)) {
        if k == key {
            return mask;
        }
    }
    let mask = ctx.fonts(|f| {
        let fams = f.families();
        let mut m = 0u8;
        if fams.contains(&fam_default_bold()) {
            m |= MASK_DEFAULT_BOLD;
        }
        if fams.contains(&fam_ui_bold()) {
            m |= MASK_UI_BOLD;
        }
        if fams.contains(&fam_ui_medium()) {
            m |= MASK_UI_MEDIUM;
        }
        m
    });
    ctx.data_mut(|d| d.insert_temp(id, (key, mask)));
    mask
}

/// 真 700 字重（内嵌 Noto Sans SC Bold / Inter Bold）。
/// egui 的 `RichText::strong()` 只改颜色、不换字形，标题看起来和正文一样细；
/// 这也是「界面没有层次」的直接原因。
pub fn bold(size: f32) -> FontId {
    FontId::new(size, fam_default_bold())
}

/// 单测若直接构造裸 `egui::Context::default()`（未调用 `install_fonts`），回落到 Proportional，避免缺失字族 panic。
pub fn bold_in(ctx: &egui::Context, size: f32) -> FontId {
    if font_family_mask(ctx) & MASK_DEFAULT_BOLD != 0 {
        FontId::new(size, fam_default_bold())
    } else {
        FontId::proportional(size)
    }
}

/// 系统原生 UI 粗体（拉丁走 Segoe UI Bold 700，中文走 Microsoft YaHei UI Bold 700，回退内嵌 Bold）。
/// 与正文 `Microsoft YaHei UI` + `Segoe UI` 同属一套字族骨架，彻底消除混用思源黑体造成的字面大小跳变。
pub fn ui_bold_in(ctx: &egui::Context, size: f32) -> FontId {
    let m = font_family_mask(ctx);
    if m & MASK_UI_BOLD != 0 {
        FontId::new(size, fam_ui_bold())
    } else if m & MASK_DEFAULT_BOLD != 0 {
        FontId::new(size, fam_default_bold())
    } else {
        FontId::proportional(size)
    }
}

/// 500~600 中黑字重（拉丁走 Segoe UI Semibold 600，中文走 Microsoft YaHei UI）。
/// 小字号（12~13px）按钮/页签/文件名使用单遍原生渲染，彻底杜绝亚像素双描造成的竖画重影发虚。
pub fn medium_in(ctx: &egui::Context, size: f32) -> FontId {
    if font_family_mask(ctx) & MASK_UI_MEDIUM != 0 {
        FontId::new(size, fam_ui_medium())
    } else {
        FontId::proportional(size)
    }
}

/// 绘制商用 UI 层级强调文本：17px 及以上大字号主标题走同族真 Bold（`ui-bold`），
/// 16px 及以下章节标题/按钮/文件名走单遍通透 `ui-medium`（拉丁 Semibold + 雅黑 UI），避免小字号中文粗体笔画粘连。
pub fn paint_medium(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: Align2,
    text: &str,
    size: f32,
    col: Color32,
) {
    let font = if size >= 17.0 {
        ui_bold_in(painter.ctx(), size)
    } else {
        medium_in(painter.ctx(), size)
    };
    painter.text(pos, align, text, font, col);
}

/// 统一 1.45px 线性几何图标（0=主页, 1=新建工作簿, 2=打开文件夹），纯单色线条无杂色填充，适配深浅按钮与侧栏。
pub fn draw_clean_icon(painter: &egui::Painter, rect: Rect, kind: u8, col: Color32) {
    let s = rect.width().min(rect.height());
    let sq = Rect::from_center_size(rect.center(), Vec2::splat(s));
    let px = |u: f32| sq.left() + u * s;
    let py = |v: f32| sq.top() + v * s;
    let st = Stroke::new(1.45, col);
    match kind {
        0 => {
            // 主页：对称屋脊 + 墙体 + 门框
            painter.line_segment([egui::pos2(px(0.14), py(0.48)), egui::pos2(px(0.50), py(0.17))], st);
            painter.line_segment([egui::pos2(px(0.50), py(0.17)), egui::pos2(px(0.86), py(0.48))], st);
            painter.line_segment([egui::pos2(px(0.24), py(0.43)), egui::pos2(px(0.24), py(0.84))], st);
            painter.line_segment([egui::pos2(px(0.76), py(0.43)), egui::pos2(px(0.76), py(0.84))], st);
            painter.line_segment([egui::pos2(px(0.24), py(0.84)), egui::pos2(px(0.76), py(0.84))], st);
            painter.line_segment([egui::pos2(px(0.42), py(0.84)), egui::pos2(px(0.42), py(0.58))], st);
            painter.line_segment([egui::pos2(px(0.42), py(0.58)), egui::pos2(px(0.58), py(0.58))], st);
            painter.line_segment([egui::pos2(px(0.58), py(0.58)), egui::pos2(px(0.58), py(0.84))], st);
        }
        1 => {
            // 新建工作簿：折角文档 + 居中十字加号
            let l = px(0.21);
            let r = px(0.79);
            let t = py(0.12);
            let b = py(0.88);
            let fold = s * 0.21;
            painter.line_segment([egui::pos2(l, t), egui::pos2(r - fold, t)], st);
            painter.line_segment([egui::pos2(r - fold, t), egui::pos2(r, t + fold)], st);
            painter.line_segment([egui::pos2(r, t + fold), egui::pos2(r, b)], st);
            painter.line_segment([egui::pos2(r, b), egui::pos2(l, b)], st);
            painter.line_segment([egui::pos2(l, b), egui::pos2(l, t)], st);
            painter.line_segment([egui::pos2(r - fold, t), egui::pos2(r - fold, t + fold)], st);
            painter.line_segment([egui::pos2(r - fold, t + fold), egui::pos2(r, t + fold)], st);
            let cx = px(0.50);
            let cy = py(0.58);
            let arm = s * 0.14;
            painter.line_segment([egui::pos2(cx - arm, cy), egui::pos2(cx + arm, cy)], st);
            painter.line_segment([egui::pos2(cx, cy - arm), egui::pos2(cx, cy + arm)], st);
        }
        _ => {
            // 打开文件夹：现代平顶耳标文件夹
            let l = px(0.14);
            let r = px(0.86);
            let t = py(0.22);
            let tab_b = py(0.35);
            let b = py(0.82);
            painter.line_segment([egui::pos2(l, t), egui::pos2(px(0.41), t)], st);
            painter.line_segment([egui::pos2(px(0.41), t), egui::pos2(px(0.49), tab_b)], st);
            painter.line_segment([egui::pos2(px(0.49), tab_b), egui::pos2(r, tab_b)], st);
            painter.line_segment([egui::pos2(r, tab_b), egui::pos2(r, b)], st);
            painter.line_segment([egui::pos2(r, b), egui::pos2(l, b)], st);
            painter.line_segment([egui::pos2(l, b), egui::pos2(l, t)], st);
            painter.line_segment(
                [egui::pos2(l, py(0.44)), egui::pos2(r, py(0.44))],
                Stroke::new(1.0, col.gamma_multiply(0.55)),
            );
        }
    }
}
/// 禁用态：Excel 的禁用图标是中性灰 #A6A6A6，比正文浅、比分隔线深
pub const DISABLED: Color32 = Color32::from_rgb(166, 166, 166);
/// 列头/行头与网格的分界线：Excel 实测 #ABABAB，比普通控件边框深
pub const HEAD_LINE: Color32 = Color32::from_rgb(171, 171, 171);
/// 功能区分组之间的竖线
pub const SEP: Color32 = Color32::from_rgb(228, 232, 240);
pub const TEXT: Color32 = Color32::from_rgb(23, 31, 46);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(100, 116, 139);
pub const HEADER_BG: Color32 = Color32::from_rgb(245, 247, 250);
pub const HEADER_SEL: Color32 = Color32::from_rgb(226, 232, 240);
pub const GRID_LINE: Color32 = Color32::from_rgb(220, 225, 232);
/// 页签行底色
pub const TABBAR_BG: Color32 = Color32::from_rgb(240, 243, 248);
/// 标题栏底色
pub const TITLE_BG: Color32 = Color32::from_rgb(240, 243, 248);
pub const HOVER: Color32 = Color32::from_rgb(239, 243, 249);
pub const PRESSED: Color32 = Color32::from_rgb(228, 234, 243);
pub const STATUS_BG: Color32 = Color32::from_rgb(245, 247, 250);

/// 标题栏（自绘，含文档标签与窗口按钮）
pub const TITLE_H: f32 = 38.0;
/// 页签行（直接铺在 PAGE_BG 上）
pub const TAB_H: f32 = 30.0;
/// 功能区纯白悬浮卡片高度（含顶部 6px 与底部 8px 呼吸留白，杜绝组名贴底边）
pub const RIBBON_BODY_H: f32 = 110.0;
/// 功能区总高
pub const RIBBON_H: f32 = TAB_H + RIBBON_BODY_H;
/// 编辑栏（名称框 + fx + 输入行）
pub const FX_H: f32 = 34.0;
/// 命令区高度：大按钮 76 高
pub const GROUP_BODY_H: f32 = 76.0;
/// 分组名行高（居中绘制，下方仍留 8px 卡片呼吸边距）
pub const GROUP_LABEL_H: f32 = 16.0;

thread_local! {
    static GROUP_FIRST: Cell<bool> = const { Cell::new(true) };
    static GROUP_PAD: Cell<f32> = const { Cell::new(8.0) };
    /// 当前正在绘制的 `Dlg` 是否为非模态浮动面板（协作面板、查找替换、监视窗口等）。
    /// 非模态面板内的 `primary_button` 绝不在表格无控件聚焦时抢吃回车键。
    static CURRENT_DLG_NON_MODAL: Cell<bool> = const { Cell::new(false) };
}

struct NonModalDlgGuard(bool);
impl Drop for NonModalDlgGuard {
    fn drop(&mut self) {
        CURRENT_DLG_NON_MODAL.with(|c| c.set(self.0));
    }
}

/// 每组 ribbon 内容开始前调用：第一组不画左侧分隔线
pub fn reset_groups() {
    GROUP_FIRST.with(|c| c.set(true));
}

/// 设置当前功能区每组左右内边距
pub fn set_group_pad(pad: f32) {
    GROUP_PAD.with(|c| c.set(pad));
}

pub fn accent(_v: &egui::Visuals) -> Color32 {
    crate::theme::palette().accent
}

pub(crate) fn text_color(v: &egui::Visuals) -> Color32 {
    v.override_text_color
        .unwrap_or_else(|| crate::theme::palette().text)
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
    base.spacing.scroll.foreground_color = false;
    base.spacing.scroll.bar_width = 8.0;
    base.spacing.scroll.bar_inner_margin = 2.0;
    base.spacing.scroll.bar_outer_margin = 0.0;
    base.spacing.scroll.handle_min_length = 24.0;
    base.spacing.menu_margin = egui::Margin::same(6);

    let mut light = base.clone();
    light.visuals = {
        let mut v = egui::Visuals::light();
        v.panel_fill = Color32::WHITE;
        v.window_fill = Color32::WHITE;
        v.window_corner_radius = egui::CornerRadius::same(8);
        v.menu_corner_radius = egui::CornerRadius::same(8);
        v.window_stroke = Stroke::new(1.0, CARD_BORDER);
        v.popup_shadow = egui::epaint::Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(26),
        };
        v.window_shadow = egui::epaint::Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(32),
        };
        v.extreme_bg_color = Color32::from_rgb(249, 250, 251);
        v.faint_bg_color = Color32::from_rgb(246, 247, 249);
        v.widgets.noninteractive.bg_fill = Color32::WHITE;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(232, 235, 240));
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.bg_fill = Color32::from_rgb(245, 247, 250);
        v.widgets.inactive.weak_bg_fill = Color32::WHITE;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(214, 218, 224));
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
        v.widgets.hovered.bg_fill = Color32::from_rgb(238, 241, 246);
        v.widgets.hovered.weak_bg_fill = HOVER;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(180, 186, 196));
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
        v.widgets.active.bg_fill = Color32::from_rgb(229, 233, 240);
        v.widgets.active.weak_bg_fill = PRESSED;
        v.widgets.active.bg_stroke = Stroke::new(1.5, BRAND_NAVY);
        v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.active.corner_radius = egui::CornerRadius::same(6);
        v.widgets.open = v.widgets.active;
        v.selection.bg_fill = Color32::from_rgb(204, 228, 215);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.text_cursor.stroke = Stroke::new(1.25, TEXT);
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
        v.text_cursor.stroke = Stroke::new(1.25, Color32::from_rgb(228, 231, 236));
        v.hyperlink_color = Color32::from_rgb(96, 165, 250);
        v
    };

    ctx.set_style_of(egui::Theme::Dark, dark);
    ctx.set_style_of(egui::Theme::Light, light);
    // 禁用 egui 内置 Ctrl+-/Ctrl+= 整窗缩放，防止与 Excel 删除/插入单元格快捷键冲突
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    // Excel 默认浅色；界面按浅色做像素对齐
    ctx.set_theme(egui::ThemePreference::Light);
}

/// 统一商用下拉菜单 / 气泡浮层外壳（8px 圆角、柔和弥散投影、1px CARD_BORDER）
pub fn popup_frame(style: &egui::Style) -> egui::Frame {
    let pal = crate::theme::palette();
    let dark = style.visuals.dark_mode || pal.theme.is_dark();
    egui::Frame::NONE
        .fill(pal.dialog_bg)
        .stroke(Stroke::new(1.0, pal.card_border))
        .corner_radius(egui::CornerRadius::same(8))
        .shadow(egui::epaint::Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(if dark { 80 } else { 26 }),
        })
        .inner_margin(egui::Margin::same(6))
}

/// 浮层菜单标准行（27px 舒适行高、左对齐、5px 圆角悬停高亮）
pub fn popup_item(ui: &mut egui::Ui, label: &str, enabled: bool, min_w: f32) -> Response {
    let col = if !enabled {
        DISABLED
    } else {
        text_color(ui.visuals())
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(12.0), col);
    let w = (galley.size().x + 24.0).max(min_w).max(ui.available_width());
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 27.0), sense);
    let pal = crate::theme::palette();
    if enabled && resp.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, 5.0, pal.pressed);
    } else if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, 5.0, pal.hover);
    }
    let pos = Align2::LEFT_CENTER
        .anchor_size(egui::pos2(rect.left() + 10.0, rect.center().y), galley.size())
        .min;
    ui.painter().galley(pos, galley, col);
    a11y_name(&resp, label, enabled, None);
    resp
}

/// 一个功能区小组：内容 + 底部组名（上下充裕留白，绝不紧贴悬浮卡片边线）
pub fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) -> Rect {
    let first = GROUP_FIRST.with(|c| c.replace(false));
    if !first {
        // 分隔线画在两组交界处，上下收进卡片内留出呼吸感
        let x = ui.cursor().left();
        let y = ui.cursor().top();
        let dark = ui.visuals().dark_mode;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(x, y + 10.0),
                Vec2::new(crate::tokens::stroke::HAIR, RIBBON_BODY_H - 20.0),
            ),
            0.0,
            if dark {
                Color32::from_rgb(58, 64, 74)
            } else {
                SEP
            },
        );
    }
    let out = ui
        .vertical(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 0.0);
            // 顶部留 6px 呼吸间距，使首行下拉框/大按钮不贴卡片上边线
            ui.add_space(6.0);
            let pad = GROUP_PAD.with(|c| c.get());
            let row = ui.horizontal_top(|ui| {
                ui.add_space(pad);
                ui.set_height(GROUP_BODY_H);
                add(ui);
                ui.allocate_exact_size(Vec2::new(pad, 0.0), Sense::hover());
            });
            let w = row.response.rect.width().max(crate::tokens::space::XXXL);
            // 6(顶) + 76(命令) + 4(间隙) + 16(组名居中行) = 102，距 110 高卡片底边留足 8px 白边！
            ui.add_space(4.0);
            let (lbl_rect, _) = ui.allocate_exact_size(Vec2::new(w, GROUP_LABEL_H), Sense::hover());
            let c = if ui.visuals().dark_mode {
                ui.visuals().weak_text_color()
            } else {
                Color32::from_rgb(100, 116, 139)
            };
            ui.painter().text(
                lbl_rect.center(),
                Align2::CENTER_CENTER,
                title,
                FontId::proportional(11.0),
                c,
            );
        })
        .response;
    // 布局自证：SC_UI_LOG=1 时打出每个分组框的几何
    let group_rect = out.rect;
    if crate::debug_flags::ui_log() {
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

/// 分组的“对话框启动器”：与底部组名水平居中对齐，离右分隔线与底边线均留出充足边距
pub fn launcher_at(ui: &mut egui::Ui, group_rect: Rect, id: &str) -> bool {
    let s = 14.0;
    let size = Vec2::new(s, s);
    let rect = Rect::from_center_size(
        egui::pos2(
            group_rect.right() - 10.0,
            group_rect.bottom() - GROUP_LABEL_H * 0.5,
        ),
        size,
    );
    let resp = ui.interact(rect, egui::Id::new(("grp_launch", id)), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, HOVER);
    }
    let c = if resp.hovered() {
        BRAND_NAVY
    } else {
        Color32::from_rgb(148, 163, 184)
    };
    let p = ui.painter();
    let st = Stroke::new(1.1, c);
    p.line_segment(
        [
            egui::pos2(rect.left() + 3.5, rect.bottom() - 3.5),
            egui::pos2(rect.right() - 3.5, rect.top() + 3.5),
        ],
        st,
    );
    p.line_segment(
        [
            egui::pos2(rect.right() - 7.0, rect.top() + 3.5),
            egui::pos2(rect.right() - 3.5, rect.top() + 3.5),
        ],
        st,
    );
    p.line_segment(
        [
            egui::pos2(rect.right() - 3.5, rect.top() + 7.0),
            egui::pos2(rect.right() - 3.5, rect.top() + 3.5),
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
    let col = if ui.is_enabled() {
        text_color(ui.visuals())
    } else {
        DISABLED
    };
    let galley = (!label.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(label.to_owned(), FontId::proportional(12.0), col)
    });
    big_sized_galley(ui, icon, galley, size)
}

pub fn big_sized_galley(
    ui: &mut egui::Ui,
    icon: Icon,
    galley: Option<Arc<egui::Galley>>,
    size: Vec2,
) -> Response {
    let (dark, sel_bg, hov_bg, txt_col) = {
        let v = ui.visuals();
        (
            v.dark_mode,
            v.selection.bg_fill,
            v.widgets.hovered.weak_bg_fill,
            text_color(v),
        )
    };
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if resp.is_pointer_button_down_on() {
        if dark { sel_bg } else { PRESSED }
    } else if resp.hovered() {
        if dark { hov_bg } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 5.0, bg);
    }
    focus_ring(ui, &resp, rect, 5.0);
    let col = if enabled { txt_col } else { DISABLED };
    let icon_size = 30.0_f32.min(size.y - 26.0).max(16.0);
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 7.0 + icon_size / 2.0),
        Vec2::splat(icon_size),
    );
    ui_icons::draw_excel(ui.painter(), icon_rect, icon, col, enabled);
    if let Some(g) = galley {
        let pos = Align2::CENTER_CENTER
            .anchor_size(egui::pos2(rect.center().x, rect.bottom() - 12.0), g.size())
            .min;
        ui.painter().galley(pos, g, col);
    }
    resp
}

/// 同上，可指定按钮尺寸（大号拆分/下拉按钮，仅悬停时显现分割细线，消除常态碎线杂乱感）
#[allow(dead_code)]
pub fn big_sized_split(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    size: Vec2,
) -> (Response, Response) {
    let col = if ui.is_enabled() {
        text_color(ui.visuals())
    } else {
        DISABLED
    };
    let galley = (!label.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(label.to_owned(), FontId::proportional(12.0), col)
    });
    big_sized_split_galley(ui, icon, galley, size)
}

pub fn big_sized_split_galley(
    ui: &mut egui::Ui,
    icon: Icon,
    galley: Option<Arc<egui::Galley>>,
    size: Vec2,
) -> (Response, Response) {
    let (dark, sel_bg, hov_bg, txt_col) = {
        let v = ui.visuals();
        (
            v.dark_mode,
            v.selection.bg_fill,
            v.widgets.hovered.weak_bg_fill,
            text_color(v),
        )
    };
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let arrow_rect = Rect::from_min_size(
        egui::pos2(rect.left(), rect.bottom() - 14.0),
        Vec2::new(rect.width(), 14.0),
    );
    let aresp = ui.interact(
        arrow_rect,
        egui::Id::new(("bigsplit", rect.left().to_bits(), rect.top().to_bits())),
        Sense::click(),
    );
    let hovered_any = resp.hovered() || aresp.hovered();
    let over_arrow = arrow_rect.contains(resp.hover_pos().unwrap_or(rect.center()));
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if resp.is_pointer_button_down_on() || aresp.is_pointer_button_down_on() {
        if dark { sel_bg } else { PRESSED }
    } else if (resp.hovered() && !over_arrow) || aresp.hovered() {
        if dark { hov_bg } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 5.0, bg);
    }
    let col = if enabled { txt_col } else { DISABLED };
    let icon_size = 28.0_f32.min(size.y - 28.0).max(16.0);
    let icon_rect = Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 6.0 + icon_size / 2.0),
        Vec2::splat(icon_size),
    );
    ui_icons::draw_excel(ui.painter(), icon_rect, icon, col, enabled);
    if let Some(g) = galley {
        let pos = Align2::CENTER_CENTER
            .anchor_size(egui::pos2(rect.center().x, rect.bottom() - 21.0), g.size())
            .min;
        ui.painter().galley(pos, g, col);
    }
    if hovered_any && enabled {
        ui.painter().line_segment(
            [
                egui::pos2(rect.left() + 4.0, rect.bottom() - 13.5),
                egui::pos2(rect.right() - 4.0, rect.bottom() - 13.5),
            ],
            Stroke::new(1.0, Color32::from_rgb(203, 213, 225)),
        );
    }
    caret(
        ui,
        egui::pos2(rect.center().x, rect.bottom() - 7.0),
        if enabled { Color32::from_rgb(100, 116, 139) } else { DISABLED },
    );
    (resp, aresp)
}
pub fn small_on(ui: &mut egui::Ui, icon: Icon, on: bool, tip: &str) -> Response {
    let (dark, sel_bg, hov_bg, txt_col) = {
        let v = ui.visuals();
        (
            v.dark_mode,
            v.selection.bg_fill,
            v.widgets.hovered.weak_bg_fill,
            text_color(v),
        )
    };
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if on {
        if dark { sel_bg } else { PRESSED }
    } else if resp.hovered() {
        if dark { hov_bg } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let col = if !enabled { DISABLED } else { txt_col };
    ui_icons::draw_excel(ui.painter(), rect.shrink(4.0), icon, col, enabled);
    if !tip.is_empty() && (resp.hovered() || resp.contains_pointer()) {
        resp.on_hover_text(tip)
    } else {
        resp
    }
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
    let (dark, txt_col) = {
        let v = ui.visuals();
        (v.dark_mode, text_color(v))
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::click());
    let bg = state_bg(enabled, false, resp.hovered(), resp.is_pointer_button_down_on(), dark);
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
    let fg = if enabled { txt_col } else { DISABLED };
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
    let col = if ui.is_enabled() {
        text_color(ui.visuals())
    } else {
        DISABLED
    };
    let galley = (!label.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(label.to_owned(), FontId::proportional(12.0), col)
    });
    labeled_w_on_galley(ui, icon, galley, width, on, tip)
}

pub fn labeled_w_on_galley(
    ui: &mut egui::Ui,
    icon: Icon,
    galley: Option<Arc<egui::Galley>>,
    width: f32,
    on: bool,
    tip: &str,
) -> Response {
    let (dark, sel_bg, hov_bg, txt_col) = {
        let v = ui.visuals();
        (
            v.dark_mode,
            v.selection.bg_fill,
            v.widgets.hovered.weak_bg_fill,
            text_color(v),
        )
    };
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click());
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if on || resp.is_pointer_button_down_on() {
        if dark { sel_bg } else { PRESSED }
    } else if resp.hovered() {
        if dark { hov_bg } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let col = if enabled { txt_col } else { DISABLED };
    let ir = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 4.0, rect.center().y - 8.0),
        Vec2::splat(16.0),
    );
    ui_icons::draw_excel(ui.painter(), ir, icon, col, enabled);
    if let Some(g) = galley {
        let pos = Align2::LEFT_CENTER
            .anchor_size(egui::pos2(rect.left() + 24.0, rect.center().y), g.size())
            .min;
        ui.painter().galley(pos, g, col);
    }
    if !tip.is_empty() && (resp.hovered() || resp.contains_pointer()) {
        resp.on_hover_text(tip)
    } else {
        resp
    }
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
    let (dark, hov_bg) = {
        let v = ui.visuals();
        (v.dark_mode, v.widgets.hovered.weak_bg_fill)
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            2.0,
            if dark { hov_bg } else { HOVER },
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
    if !tip.is_empty() && (resp.hovered() || resp.contains_pointer()) {
        resp.on_hover_text(tip)
    } else {
        resp
    }
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
    let dark = ui.visuals().dark_mode;
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let bg = state_bg(
        enabled,
        false,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        dark,
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
    let (dark, txt_col) = {
        let v = ui.visuals();
        (v.dark_mode, text_color(v))
    };
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    let bg = state_bg(
        enabled,
        false,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        dark,
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 2.0, bg);
    }
    focus_ring(ui, &resp, rect, 2.0);
    let fg = if enabled { txt_col } else { DISABLED };
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
    if !tip.is_empty() && (resp.hovered() || resp.contains_pointer()) {
        resp.on_hover_text(tip)
    } else {
        resp
    }
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
    let (dark, txt_col) = {
        let v = ui.visuals();
        (v.dark_mode, text_color(v))
    };
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    let bg = state_bg(
        enabled,
        on,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        dark,
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    focus_ring(ui, &resp, rect, 3.0);
    let col = if enabled { txt_col } else { DISABLED };
    let _ = italic;
    let pos = rect.center() - Vec2::new(0.0, 0.5);
    let g = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(13.0), col);
    let gp = Align2::CENTER_CENTER.anchor_size(pos, g.size()).min;
    ui.painter().galley(gp, g.clone(), col);
    if strong {
        // 小字号下用 0.55px 重描模拟加粗，比换字体更稳定（复用同一 Galley，零二次排版）
        ui.painter().galley(gp + Vec2::new(0.55, 0.0), g, col);
    }
    if underline {
        let uy = pos.y + 7.5;
        ui.painter().line_segment(
            [egui::pos2(pos.x - 5.0, uy), egui::pos2(pos.x + 5.0, uy)],
            Stroke::new(1.4, col),
        );
    }
    if !tip.is_empty() && (resp.hovered() || resp.contains_pointer()) {
        resp.on_hover_text(tip)
    } else {
        resp
    }
}

/// Fluent 风格功能区页签：直接浮在 PAGE_BG 上，活动页签用 2.5px 主题绿圆角短横条标记，
/// 下方命令区独立收为完整四圆角纯白卡片，彻底告别「半灰半白双拼断层框」。
pub fn tab_ex(ui: &mut egui::Ui, name: &str, active: bool, green: bool) -> Response {
    let (dark, weak_col) = {
        let v = ui.visuals();
        (v.dark_mode, v.weak_text_color())
    };
    let font = if active || green {
        medium_in(ui.ctx(), 12.5)
    } else {
        FontId::proportional(12.5)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font, Color32::PLACEHOLDER);
    let text_w = galley.size().x;
    let size = Vec2::new(text_w + 20.0, TAB_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if green {
        let pill = rect.shrink2(Vec2::new(2.0, 3.0));
        let fill = if resp.hovered() {
            Color32::from_rgba_unmultiplied(33, 115, 70, 32)
        } else {
            Color32::from_rgba_unmultiplied(33, 115, 70, 18)
        };
        ui.painter().rect_filled(pill, 5.0, fill);
    } else if resp.hovered() && !active {
        ui.painter().rect_filled(
            rect.shrink2(Vec2::new(2.0, 3.0)),
            5.0,
            HOVER_ON_PAGE,
        );
    }
    let col = if green {
        ACCENT
    } else if active {
        if dark { Color32::WHITE } else { Color32::from_rgb(15, 23, 42) }
    } else if dark {
        weak_col
    } else {
        Color32::from_rgb(71, 85, 105)
    };
    let pos = Align2::CENTER_CENTER
        .anchor_size(rect.center(), galley.size())
        .min;
    ui.painter().galley_with_override_text_color(pos, galley, col);
    if active && !green {
        let bar_w = (text_w + 4.0).max(16.0);
        let bar = egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.bottom() - 2.0),
            Vec2::new(bar_w, 2.6),
        );
        ui.painter().rect_filled(bar, 1.5, ACCENT);
    }
    resp
}

/// 分组之间的竖细线（功能区内手动用）
pub fn hairline(ui: &mut egui::Ui) {
    let line_col = {
        let v = ui.visuals();
        if v.dark_mode {
            v.widgets.noninteractive.bg_stroke.color
        } else {
            SEP
        }
    };
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, GROUP_BODY_H), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, line_col);
}

/// 对话框底部的按钮行（右对齐，统一 12px 呼吸留白与细分割线）
pub fn footer(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(10.0);
    let r = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [egui::pos2(r.left(), r.top()), egui::pos2(r.right(), r.top())],
        Stroke::new(1.0, Color32::from_rgb(236, 239, 243)),
    );
    ui.add_space(12.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        add(ui);
    });
}

/// 对话框底部左右双栏按钮行（左侧放轻量辅助入口，右侧放主次操作按钮）
pub fn footer_split(
    ui: &mut egui::Ui,
    left: impl FnOnce(&mut egui::Ui),
    right: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(10.0);
    let r = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [egui::pos2(r.left(), r.top()), egui::pos2(r.right(), r.top())],
        Stroke::new(1.0, Color32::from_rgb(236, 239, 243)),
    );
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        left(ui);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            right(ui);
        });
    });
}

/// 检测当前上下文是否启用了 AccessKit（未启用无障碍树时跳过 `a11y_*` 字符串分配与闭包构造）。
#[inline]
pub fn is_accesskit_active(ctx: &egui::Context) -> bool {
    ctx.accesskit_node_builder(egui::accesskit_root_id(), |_| ()).is_some()
}

/// 同 `a11y_name`，但可访问矩形单独给（拆分按钮的主体不能把箭头区也算进去）
pub fn a11y_name_rect(
    resp: &egui::Response,
    label: &str,
    enabled: bool,
    rect: egui::Rect,
    ctx: &egui::Context,
) {
    if !is_accesskit_active(ctx) {
        return;
    }
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
    if !is_accesskit_active(&resp.ctx) {
        return;
    }
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

/// UI-29.3：空/错误/警告/信息四种状态的颜色，来自 Fluent 的状态色而不是随手取的灰。
pub fn state_color(kind: StateKind) -> Color32 {
    match kind {
        StateKind::Empty => Color32::from_rgb(138, 136, 134),
        StateKind::Error => Color32::from_rgb(196, 43, 28),
        StateKind::Warn => Color32::from_rgb(157, 93, 0),
        StateKind::Info => ACCENT,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StateKind {
    Empty,
    Error,
    Warn,
    Info,
}

/// 空/错误状态的统一版式：图标 + 标题 + 说明，垂直居中。
/// 为什么统一：此前「查找全部 0 处」「加载问题」各写一套 label，字号/颜色/留白都不同。
pub fn state_view(
    ui: &mut egui::Ui,
    kind: StateKind,
    icon: crate::ui_icons::Icon,
    title: &str,
    detail: &str,
) -> egui::Response {
    let color = state_color(kind);
    ui.vertical_centered(|ui| {
        ui.add_space(crate::tokens::space::XL);
        let (rect, resp) = ui.allocate_exact_size(
            Vec2::splat(crate::tokens::icon::XL),
            Sense::hover(),
        );
        crate::ui_icons::draw(ui.painter(), rect, icon, color);
        ui.add_space(crate::tokens::space::L);
        ui.label(
            RichText::new(title)
                .size(crate::tokens::text::TITLE)
                .color(color),
        );
        if !detail.is_empty() {
            ui.add_space(crate::tokens::space::XS);
            ui.add(
                egui::Label::new(
                    RichText::new(detail)
                        .size(crate::tokens::text::SMALL)
                        .color(TEXT_WEAK),
                )
                .wrap(),
            );
        }
        ui.add_space(crate::tokens::space::XL);
        resp
    })
    .inner
}

/// UI-29.3：左对齐的状态条（对话框顶部、面板顶部用）。左边一条 3px 状态色。
pub fn state_banner(
    ui: &mut egui::Ui,
    kind: StateKind,
    title: &str,
    detail: &str,
) -> egui::Response {
    let pal = crate::theme::palette();
    let color = state_color(kind);
    let bg = match kind {
        StateKind::Empty => pal.card_bg,
        StateKind::Error => pal.err_bg,
        StateKind::Warn => pal.warn_bg,
        StateKind::Info => pal.info_bg,
    };
    let resp = egui::Frame::NONE
        .fill(bg)
        .inner_margin(egui::Margin::symmetric(
            crate::tokens::space::L as i8,
            crate::tokens::space::M as i8,
        ))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (bar, _) = ui.allocate_exact_size(
                    Vec2::new(crate::tokens::stroke::HAIR * 3.0, 28.0),
                    Sense::hover(),
                );
                ui.painter().rect_filled(bar, 0.0, color);
                ui.add_space(crate::tokens::space::L);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(title)
                            .size(crate::tokens::text::BODY)
                            .color(color)
                            .strong(),
                    );
                    if !detail.is_empty() {
                        ui.label(
                            RichText::new(detail)
                                .size(crate::tokens::text::SMALL)
                                .color(pal.text_weak),
                        );
                    }
                });
            });
        })
        .response;
    resp
}

/// UI-29.3：加载遮罩。默认保留顶部标题栏（38px）可操作（允许切换标签页、最小化/关闭窗口），
/// 支持真实百分比进度条与“取消”按钮（点击取消或按 Esc 返回 `true`）。
pub fn loading_overlay(ctx: &egui::Context, title: &str, detail: &str, progress: Option<f32>) {
    let _ = loading_overlay_cancellable(ctx, title, detail, progress, false);
}

pub fn loading_overlay_cancellable(
    ctx: &egui::Context,
    title: &str,
    detail: &str,
    progress: Option<f32>,
    cancellable: bool,
) -> bool {
    let pal = crate::theme::palette();
    let screen = ctx.screen_rect();
    let title_h = 38.0_f32;
    let body = egui::Rect::from_min_max(
        egui::pos2(screen.min.x, (screen.min.y + title_h).min(screen.max.y)),
        screen.max,
    );
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("loading_overlay"));
    let p = ctx.layer_painter(layer);
    p.rect_filled(body, 0.0, pal.dialog_scrim);
    let card_h = match (progress.is_some(), cancellable) {
        (true, true) => 166.0,
        (true, false) => 136.0,
        (false, true) => 148.0,
        (false, false) => 116.0,
    };
    let card = egui::Rect::from_center_size(body.center(), Vec2::new(340.0, card_h));
    p.rect_filled(card, crate::tokens::radius::LG, pal.dialog_bg);
    p.rect_stroke(
        card,
        crate::tokens::radius::LG,
        Stroke::new(crate::tokens::stroke::HAIR, pal.card_border),
        egui::StrokeKind::Inside,
    );
    // 旋转弧：8 段，按时间推进透明度
    let t = ctx.input(|i| i.time) as f32;
    let c = egui::pos2(card.center().x, card.min.y + 30.0);
    for i in 0..8 {
        let ang = t * 3.0 + i as f32 * std::f32::consts::TAU / 8.0;
        let a = 0.15 + 0.85 * ((i as f32 / 8.0 + t * 1.5).fract());
        let col = pal.accent.gamma_multiply(a);
        let dir = egui::vec2(ang.cos(), ang.sin());
        p.circle_filled(c + dir * 11.0, 2.0, col);
    }
    p.text(
        egui::pos2(card.center().x, card.min.y + 58.0),
        Align2::CENTER_CENTER,
        title,
        FontId::proportional(crate::tokens::text::TITLE),
        pal.text,
    );
    if !detail.is_empty() {
        p.text(
            egui::pos2(card.center().x, card.min.y + 78.0),
            Align2::CENTER_CENTER,
            detail,
            FontId::proportional(crate::tokens::text::SMALL),
            pal.text_weak,
        );
    }
    if let Some(v) = progress {
        let pct = v.clamp(0.0, 1.0);
        let bar_y = if cancellable { card.max.y - 62.0 } else { card.max.y - 30.0 };
        let bar = egui::Rect::from_min_size(
            egui::pos2(card.min.x + 24.0, bar_y),
            Vec2::new(card.width() - 48.0, 5.0),
        );
        let track_col = if pal.theme == crate::theme::AppTheme::Light {
            Color32::from_rgb(237, 235, 233)
        } else {
            pal.hover
        };
        p.rect_filled(bar, 2.5, track_col);
        let fill = egui::Rect::from_min_size(bar.min, Vec2::new(bar.width() * pct, bar.height()));
        p.rect_filled(fill, 2.5, pal.accent);
        p.text(
            egui::pos2(card.center().x, bar_y + 15.0),
            Align2::CENTER_CENTER,
            format!("{:.0}%", pct * 100.0),
            FontId::proportional(crate::tokens::text::SMALL),
            pal.text_weak,
        );
    }
    let mut cancelled = false;
    // 遮罩仅覆盖内容区（不遮挡 38px 标题栏上的标签页与窗口按钮），卡片内放取消按钮
    let _ = egui::Area::new(egui::Id::new("loading_overlay_block"))
        .order(egui::Order::Foreground)
        .fixed_pos(body.min)
        .show(ctx, |ui| {
            let _ = ui.allocate_exact_size(body.size(), drag_only_sense());
        });
    if cancellable {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancelled = true;
        }
        let btn_rect = egui::Rect::from_center_size(
            egui::pos2(card.center().x, card.max.y - 22.0),
            Vec2::new(96.0, 26.0),
        );
        let _ = egui::Area::new(egui::Id::new("loading_overlay_cancel"))
            .order(egui::Order::Tooltip)
            .fixed_pos(btn_rect.min)
            .show(ctx, |ui| {
                ui.set_min_size(btn_rect.size());
                if secondary_button(ui, "取消 (Esc)").clicked() {
                    cancelled = true;
                }
            });
    }
    ctx.request_repaint();
    cancelled
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
    if !is_accesskit_active(&resp.ctx) {
        return;
    }
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
    let pal = crate::theme::palette();
    let is_hc = pal.theme == crate::theme::AppTheme::HighContrast;
    let fg = if is_hc { Color32::BLACK } else { Color32::WHITE };
    let bg_idle = pal.accent;
    let bg_hov = pal.accent_hover;
    let bg_act = if is_hc {
        Color32::from_rgb(220, 195, 0)
    } else if pal.theme.is_dark() {
        Color32::from_rgb(32, 148, 96)
    } else {
        Color32::from_rgb(20, 77, 46)
    };
    let resp = ui
        .scope(|ui| {
            let w = &mut ui.visuals_mut().widgets;
            w.inactive.weak_bg_fill = bg_idle;
            w.inactive.bg_fill = bg_idle;
            w.inactive.bg_stroke = Stroke::NONE;
            w.hovered.weak_bg_fill = bg_hov;
            w.hovered.bg_fill = bg_hov;
            w.hovered.bg_stroke = Stroke::NONE;
            w.active.weak_bg_fill = bg_act;
            w.active.bg_fill = bg_act;
            w.active.bg_stroke = Stroke::NONE;
            let btn = egui::Button::new(
                RichText::new(label)
                    .font(medium_in(ui.ctx(), crate::tokens::text::BODY))
                    .color(fg),
            )
            .corner_radius(6.0)
            .min_size(Vec2::new(84.0, 32.0));
            ui.add(btn)
        })
        .inner;
    if resp.has_focus() {
        ui.painter().rect_stroke(
            resp.rect.expand(2.0),
            7.5,
            Stroke::new(1.5, pal.accent.gamma_multiply(0.55)),
            egui::StrokeKind::Outside,
        );
    }
    let in_non_modal = CURRENT_DLG_NON_MODAL.with(|c| c.get());
    let enter = !in_non_modal
        && ui.input(|i| i.key_pressed(egui::Key::Enter))
        && ui.ctx().memory(|m| m.focused().is_none())
        && !ui.ctx().memory(|m| m.any_popup_open());
    BtnResp {
        clicked: resp.clicked() || enter,
    }
}

/// 次要按钮（取消/关闭）：与主按钮严格同高同宽（84×32）、6px 圆角、随主题适配底色与文字
pub fn secondary_button(ui: &mut egui::Ui, label: &str) -> Response {
    let pal = crate::theme::palette();
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.weak_bg_fill = pal.card_bg;
        w.inactive.bg_fill = pal.card_bg;
        w.inactive.bg_stroke = Stroke::new(1.0, pal.card_border);
        w.hovered.weak_bg_fill = pal.hover;
        w.hovered.bg_fill = pal.hover;
        w.hovered.bg_stroke = Stroke::new(1.0, pal.accent);
        w.active.weak_bg_fill = pal.pressed;
        w.active.bg_fill = pal.pressed;
        w.active.bg_stroke = Stroke::new(1.0, pal.accent);
        let btn = egui::Button::new(
            RichText::new(label)
                .color(pal.text)
                .size(crate::tokens::text::BODY),
        )
        .corner_radius(6.0)
        .min_size(Vec2::new(84.0, 32.0));
        ui.add(btn)
    })
    .inner
}

/// 商用表单单行输入框：34px 舒适高度、左右 10px 内边距、随主题适配底色/文字/描边
pub fn text_input(ui: &mut egui::Ui, buf: &mut String, hint: &str) -> Response {
    let pal = crate::theme::palette();
    let desired_w = ui.available_width().max(120.0);
    let (outer, _) = ui.allocate_exact_size(Vec2::new(desired_w, 34.0), Sense::hover());
    let bg_slot = ui.painter().add(egui::Shape::Noop);
    let focused_id = ui.memory(|m| m.focused());
    let inner = outer.shrink2(Vec2::new(10.0, 6.0));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let te_resp = child.add(
        egui::TextEdit::singleline(buf)
            .frame(false)
            .desired_width(inner.width())
            .font(FontId::proportional(12.0))
            .text_color(pal.text)
            .hint_text(
                RichText::new(hint)
                    .size(12.0)
                    .color(pal.text_muted),
            ),
    );
    let has_focus = te_resp.has_focus() || focused_id == Some(te_resp.id);
    let hovered = ui.rect_contains_pointer(outer);
    let bg = if has_focus {
        pal.grid_bg
    } else if hovered {
        pal.hover
    } else if pal.theme.is_dark() {
        pal.card_bg
    } else {
        Color32::from_rgb(248, 249, 251)
    };
    let border = if has_focus {
        Stroke::new(1.5, pal.accent)
    } else if hovered {
        Stroke::new(1.0, pal.text_muted)
    } else {
        Stroke::new(1.0, pal.card_border)
    };
    ui.painter().set(
        bg_slot,
        egui::Shape::rect_filled(outer, egui::CornerRadius::same(6), bg),
    );
    let p = ui.painter();
    if has_focus {
        p.rect_stroke(
            outer.expand(1.5),
            7.5,
            Stroke::new(2.5, pal.accent.gamma_multiply(0.22)),
            egui::StrokeKind::Outside,
        );
    }
    p.rect_stroke(outer, 6.0, border, egui::StrokeKind::Inside);
    te_resp
}

/// 紧凑文字按钮（组里放"插入行"这种短命令）
pub fn pill(ui: &mut egui::Ui, label: &str, tip: &str) -> Response {
    let enabled = ui.is_enabled();
    let (dark, sel_bg, hov_bg, txt_col) = {
        let v = ui.visuals();
        (
            v.dark_mode,
            v.selection.bg_fill,
            v.widgets.hovered.weak_bg_fill,
            text_color(v),
        )
    };
    let font = FontId::proportional(crate::tokens::text::BODY);
    let col = if enabled { txt_col } else { DISABLED };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, col);
    let size = Vec2::new(
        galley.size().x + crate::tokens::space::XL,
        crate::tokens::size::BTN_H,
    );
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if resp.is_pointer_button_down_on() {
        if dark { sel_bg } else { PRESSED }
    } else if resp.hovered() {
        if dark { hov_bg } else { HOVER }
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let pos = Align2::CENTER_CENTER
        .anchor_size(rect.center(), galley.size())
        .min;
    ui.painter().galley(pos, galley, col);
    a11y_name(&resp, if tip.is_empty() { label } else { tip }, enabled, None);
    if crate::debug_flags::ui_log_2() {
        eprintln!("[ui]   pill {} w={:.1} x={:.1}", label, rect.width(), rect.min.x);
    }
    if !tip.is_empty() && (resp.hovered() || resp.contains_pointer()) {
        resp.on_hover_text(tip)
    } else {
        resp
    }
}

/// 工作表标签：Excel 365 —— 32 高，活动页白底 + 底部 2px 绿线
pub fn sheet_tab(
    ui: &mut egui::Ui,
    name: &str,
    active: bool,
    color: Option<[u8; 3]>,
) -> Response {
    let (dark, hov_bg, weak_col) = {
        let v = ui.visuals();
        (v.dark_mode, v.widgets.hovered.weak_bg_fill, v.weak_text_color())
    };
    let font = if active && color.is_none() {
        medium_in(ui.ctx(), crate::tokens::text::BODY)
    } else {
        FontId::proportional(crate::tokens::text::BODY)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font, Color32::PLACEHOLDER);
    let size = Vec2::new(
        galley.size().x + crate::tokens::size::SHEET_TAB_PAD,
        crate::tokens::size::SHEET_TAB_H,
    );
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    // UI-24.2：工作表标签在 UIA 里是 Tab 页，名字就是工作表名
    a11y_role(&resp, name, egui::accesskit::Role::Tab, Some(active));
    // 悬浮卡片外壳：标签条是页面灰底，标签画成上下各留 3px 的圆角小卡，
    // 和上面的网格卡片、功能区卡片同一套语言；命中区域仍是整格，不影响点击/拖动。
    let chip = rect.shrink2(Vec2::new(1.0, 3.0));
    let rad = egui::CornerRadius::same(5);
    let tab_rgb = color.map(|c| Color32::from_rgb(c[0], c[1], c[2]));
    let text_pos = Align2::CENTER_CENTER
        .anchor_size(rect.center(), galley.size())
        .min;
    let pal = crate::theme::palette();
    if let Some(fill) = tab_rgb {
        // 有颜色的标签：整块铺色；深色配白字
        ui.painter().rect_filled(chip, rad, fill);
        if active {
            ui.painter().rect_stroke(chip, rad, Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
        }
        let lum = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
        let mut color = if lum > 165.0 {
            Color32::from_rgb(30, 30, 30)
        } else {
            Color32::WHITE
        };
        if !active {
            color = color.gamma_multiply(0.92);
        }
        ui.painter().galley_with_override_text_color(text_pos, galley, color);
        return resp;
    }
    if active {
        // 活动页：底色与上方表格底栏衔接，底部以 2.5px 主题强调条清晰标出当前表
        let chip = Rect::from_min_max(
            egui::pos2(rect.left() + 1.0, rect.top()),
            egui::pos2(rect.right() - 1.0, rect.bottom() - 3.0),
        );
        let active_rad = egui::CornerRadius { nw: 0, ne: 0, sw: 6, se: 6 };
        ui.painter().rect_filled(chip, active_rad, pal.card_bg);
        ui.painter().line_segment(
            [chip.left_top(), chip.left_bottom() - Vec2::new(0.0, 4.0)],
            Stroke::new(1.0, pal.card_border),
        );
        ui.painter().line_segment(
            [chip.right_top(), chip.right_bottom() - Vec2::new(0.0, 4.0)],
            Stroke::new(1.0, pal.card_border),
        );
        let bar = egui::Rect::from_min_size(
            egui::pos2(chip.left() + 8.0, chip.bottom() - 2.5),
            Vec2::new((chip.width() - 16.0).max(4.0), 2.5),
        );
        ui.painter().rect_filled(bar, 1.25, pal.accent);
        ui.painter().galley_with_override_text_color(text_pos, galley, pal.accent);
        return resp;
    } else if resp.hovered() {
        let hover = if dark { hov_bg } else { pal.hover_on_page };
        ui.painter().rect_filled(rect.shrink2(Vec2::new(2.0, 4.0)), 5.0, hover);
    }
    let color = if dark {
        weak_col
    } else {
        pal.text_weak
    };
    ui.painter().galley_with_override_text_color(text_pos, galley, color);
    ui.painter().line_segment(
        [
            egui::pos2(rect.right(), rect.center().y - 6.0),
            egui::pos2(rect.right(), rect.center().y + 6.0),
        ],
        Stroke::new(1.0, pal.card_border),
    );
    resp
}

/// 统一样式的对话框窗口：自绘标题栏（左对齐标题 + 关闭按钮 + 可拖动），
/// 白底、1px 边框、圆角、投影；不再使用 egui 自带标题栏。
/// 本帧是否画过 ui_kit 模态对话框。命令层用它判断「模态对话框是否占着键盘」，
/// 不用逐个枚举 50 个对话框字段——只要对话框还开着，`Dlg::show` 每帧都会打标。
const DIALOG_SHOWN: &str = "ui_kit_dialog_shown";
const DIALOG_BLOCKING: &str = "ui_kit_dialog_blocking";

fn dialog_mark() -> egui::Id {
    egui::Id::new(DIALOG_SHOWN)
}

fn dialog_blocking_mark() -> egui::Id {
    egui::Id::new(DIALOG_BLOCKING)
}

/// 读一次即清：对话框关掉后标记不会残留到下一帧。
pub fn take_dialog_shown(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(dialog_mark()).unwrap_or(false))
}

const FLOATING_RECTS: &str = "ui_kit_floating_rects";

fn floating_rects_mark() -> egui::Id {
    egui::Id::new(FLOATING_RECTS)
}

/// 每帧起始清空一次；本帧任意 `block_background == true` 的 `Dlg::show` 执行后置 `true`，
/// 供后续 `CentralPanel`（`handle_keys` / `draw_grid_pane`）非破坏性读取，避免漏加 `dialog_modal` 布尔位导致穿透。
pub fn reset_dialog_blocking(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        d.remove_temp::<bool>(dialog_blocking_mark());
        d.remove_temp::<Vec<egui::Rect>>(floating_rects_mark());
    });
}

pub fn is_dialog_blocking(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(dialog_blocking_mark()).unwrap_or(false))
}

/// 判断屏幕坐标是否落在任意非模态浮动工具窗口（查找替换、监视窗口、协作面板等）内部，防止点击穿透到表格。
pub fn pointer_over_floating_window(ctx: &egui::Context, pos: egui::Pos2) -> bool {
    ctx.data(|d| {
        d.get_temp::<Vec<egui::Rect>>(floating_rects_mark())
            .is_some_and(|rects| rects.iter().any(|r| r.contains(pos)))
    })
}

pub struct Dlg {
    title: String,
    width: f32,
    height: f32,
    open: Option<*mut bool>,
    resizable: bool,
    anchor: Option<(egui::Align2, [f32; 2])>,
    /// 插入函数向导「拾取」期间要临时放行背景点击，否则模态层会把拖选吃掉
    block_background: bool,
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
        block_background: true,
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
    /// 拾取区域时放行背景点击（对话框仍置顶，只是不再吞掉表格拖选）
    pub fn block_background(mut self, v: bool) -> Self {
        self.block_background = v;
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
        if let Some(p) = self.open {
            if unsafe { !*p } {
                return None;
            }
        }
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
        ctx.data_mut(|d| {
            d.insert_temp(dialog_mark(), true);
            if self.block_background {
                d.insert_temp(dialog_blocking_mark(), true);
            }
        });
        let screen = ctx.screen_rect();
        let mut pos = ctx
            .data(|d| d.get_temp::<egui::Pos2>(id.with("pos")))
            .unwrap_or_else(|| {
                let (a, off) = self
                    .anchor
                    .unwrap_or((egui::Align2::CENTER_CENTER, [0.0, 0.0]));
                let est_h = if self.height > 0.0 {
                    (self.height + 80.0).min(screen.height() - 16.0)
                } else {
                    420.0
                };
                let p = a.align_size_within_rect(
                    egui::vec2(self.width, est_h),
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
        let pal = crate::theme::palette();
        // 模态遮挡：半透明深灰蓝遮罩压暗底层密集网格线，让弹窗干净浮出，并吞掉背景点击
        if self.block_background {
            egui::Area::new(id.with("block"))
                .order(egui::Order::Middle)
                .fixed_pos(screen.min)
                .show(ctx, |ui| {
                    let r = egui::Rect::from_min_size(screen.min, screen.size());
                    ui.painter().rect_filled(r, 0.0, pal.dialog_scrim);
                    // 遮挡层只吞点击，不参与 Tab 焦点（否则 Tab 会卡在它上面，进不到对话框里）
                    let resp = ui.interact(r, id.with("blocki"), drag_only_sense());
                    let _ = resp;
                });
        }
        let area_h = if h > 0.0 {
            h + 80.0
        } else {
            (screen.height() - 24.0).max(420.0)
        };
        let out = egui::Area::new(id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .default_size([w, area_h])
            .show(ctx, |ui| {
                let frame = egui::Frame::window(&ctx.style())
                    .fill(pal.dialog_bg)
                    .stroke(Stroke::new(1.0, pal.card_border))
                    .corner_radius(10.0)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 12],
                        blur: 32,
                        spread: 0,
                        color: Color32::from_black_alpha(34),
                    })
                    .inner_margin(egui::Margin::same(0));
                let fr = frame
                    .show(ui, |ui| {
                        ui.set_width(w);
                        // 标题栏（44px 现代微灰白顶栏 + 13.5px 中黑标题 + 内嵌 28×28 圆角关闭按钮）
                        let (trect, tresp) = ui.allocate_exact_size(
                            egui::vec2(w, 44.0),
                            drag_only_sense(),
                        );
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                trect.min,
                                egui::vec2(w, 44.0),
                            ),
                            egui::CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 },
                            pal.ribbon_bg,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(trect.left() + 1.0, trect.bottom() - 0.5),
                                egui::pos2(trect.right() - 1.0, trect.bottom() - 0.5),
                            ],
                            Stroke::new(1.0 / ctx.pixels_per_point(), pal.card_border),
                        );
                        paint_medium(
                            ui.painter(),
                            egui::pos2(trect.left() + 20.0, trect.center().y),
                            egui::Align2::LEFT_CENTER,
                            &self.title,
                            13.5,
                            pal.text,
                        );
                        // 内嵌圆角关闭按钮（替代老式铺满顶栏右角的红块）
                        let crect = egui::Rect::from_center_size(
                            egui::pos2(trect.right() - 24.0, trect.center().y),
                            egui::vec2(28.0, 28.0),
                        );
                        let cresp = ui.interact(
                            crect,
                            id.with("close"),
                            egui::Sense::click(),
                        );
                        if cresp.hovered() {
                            ui.painter().rect_filled(
                                crect,
                                6.0,
                                pal.hover,
                            );
                        }
                        let cc = if cresp.hovered() {
                            pal.text
                        } else {
                            pal.text_weak
                        };
                        let k = 4.2;
                        let st = Stroke::new(1.25, cc);
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
                        // 内容（20px 左右呼吸边距，26px 舒适控件高度）
                        let body = egui::Frame::none().inner_margin(egui::Margin {
                            left: 20,
                            right: 20,
                            top: 16,
                            bottom: 18,
                        });
                        let _non_modal_guard = NonModalDlgGuard(
                            CURRENT_DLG_NON_MODAL.with(|c| c.replace(!self.block_background)),
                        );
                        let inner = body
                            .show(ui, |ui| {
                                ui.set_width(w - 40.0);
                                ui.set_max_width(w - 40.0);
                                ui.spacing_mut().interact_size.y = 26.0;
                                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                                if h > 0.0 {
                                    egui::ScrollArea::vertical()
                                        .max_height(h)
                                        .show(ui, |ui| {
                                            let r = add(ui);
                                            // UI-24.1：Tab 走到滚动区外的控件时把它滚进视口，
                                            // 否则键盘焦点会落在看不见的地方（Excel 也是自动跟随）。
                                            if let Some(fid) = ui.memory(|m| m.focused()) {
                                                if let Some(fr) = ui.ctx().read_response(fid) {
                                                    if !ui.clip_rect().contains_rect(fr.rect) {
                                                        ui.scroll_to_rect(
                                                            fr.rect,
                                                            Some(egui::Align::Center),
                                                        );
                                                    }
                                                }
                                            }
                                            r
                                        })
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
                // UI-24.1：模态对话框把 Tab/Shift+Tab 锁在本层内；
                // 非模态浮动工具窗（查找替换、监视窗口、协作面板等）绝不设 modal_layer，允许自由编辑主表格。
                let layer = ui.layer_id();
                let focus_in_self = ui
                    .memory(|m| m.focused())
                    .and_then(|fid| ui.ctx().read_response(fid))
                    .is_some_and(|r| r.layer_id == layer);
                let esc_close = ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    && (self.block_background || focus_in_self);
                let caller_closed = self.open.is_some_and(|p| unsafe { !*p });
                let closing = closed || caller_closed || esc_close;
                if self.block_background {
                    ui.memory_mut(|m| {
                        if closing {
                            m.set_modal_layer(egui::LayerId::background());
                        } else {
                            m.set_modal_layer(layer);
                        }
                    });
                }
                (fr.inner, esc_close)
            });
        let esc_close = out.inner.1;
        if !self.block_background {
            let rect = out.response.rect;
            ctx.data_mut(|d| {
                let mut rects = d
                    .get_temp::<Vec<egui::Rect>>(floating_rects_mark())
                    .unwrap_or_default();
                rects.push(rect);
                d.insert_temp(floating_rects_mark(), rects);
            });
        }
        let caller_closed = self.open.is_some_and(|p| unsafe { !*p });
        if closed || caller_closed || esc_close {
            if let Some(p) = self.open {
                unsafe { *p = false; }
            }
            // 关闭对话框的那一帧吞掉 Enter/Esc 并临时屏蔽背景网格键盘，防止关闭按键穿透到表格推进选区。
            ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            });
            ctx.data_mut(|d| d.insert_temp(dialog_blocking_mark(), true));
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
        Some(egui::InnerResponse::new(out.inner.0 .0, out.response))
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

/// 带常驻上下微调箭头的现代数字步进框（28px 高、6px 圆角、双折线 V 型箭头）
pub fn spin(ui: &mut egui::Ui, v: &mut f64, min: f64, max: f64, step: f64, width: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::click());
    let v0 = *v;
    let step_w = 20.0;
    let half_h = rect.height() * 0.5;
    let a = egui::Rect::from_min_size(
        egui::pos2(rect.right() - step_w, rect.top() + 1.0),
        Vec2::new(step_w - 1.0, half_h - 1.0),
    );
    let b = egui::Rect::from_min_size(
        egui::pos2(rect.right() - step_w, rect.top() + half_h),
        Vec2::new(step_w - 1.0, half_h - 1.0),
    );
    let ar = ui.interact(a, ui.id().with(("spinup", rect.left() as i32)), Sense::click());
    let br = ui.interact(b, ui.id().with(("spindn", rect.left() as i32)), Sense::click());
    if ar.clicked() {
        *v = (*v + step).min(max);
    }
    if br.clicked() {
        *v = (*v - step).max(min);
    }
    let hovered = ui.rect_contains_pointer(rect);
    let border_col = if hovered {
        Color32::from_rgb(176, 186, 200)
    } else {
        CARD_BORDER
    };
    ui.painter().rect(
        rect,
        6.0,
        Color32::from_rgb(250, 251, 253),
        Stroke::new(1.0, border_col),
        egui::StrokeKind::Inside,
    );
    let txt = if (step.fract()).abs() < 1e-9 {
        format!("{:.0}", *v)
    } else {
        format!("{:.1}", *v)
    };
    ui.painter().text(
        egui::pos2(rect.left() + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        txt,
        FontId::proportional(12.0),
        TEXT,
    );
    ui.painter().line_segment(
        [
            egui::pos2(rect.right() - step_w, rect.top() + 3.0),
            egui::pos2(rect.right() - step_w, rect.bottom() - 3.0),
        ],
        Stroke::new(1.0, Color32::from_rgb(228, 232, 240)),
    );
    for (r, up, hv) in [(a, true, ar.hovered()), (b, false, br.hovered())] {
        if hv {
            ui.painter().rect_filled(r.shrink(1.0), 3.0, HOVER);
        }
        let c = r.center();
        let st = Stroke::new(
            1.3,
            if hv { TEXT } else { Color32::from_rgb(100, 116, 139) },
        );
        if up {
            ui.painter().line_segment([egui::pos2(c.x - 3.2, c.y + 1.4), egui::pos2(c.x, c.y - 1.6)], st);
            ui.painter().line_segment([egui::pos2(c.x, c.y - 1.6), egui::pos2(c.x + 3.2, c.y + 1.4)], st);
        } else {
            ui.painter().line_segment([egui::pos2(c.x - 3.2, c.y - 1.4), egui::pos2(c.x, c.y + 1.6)], st);
            ui.painter().line_segment([egui::pos2(c.x, c.y + 1.6), egui::pos2(c.x + 3.2, c.y - 1.4)], st);
        }
    }
    ui.painter().line_segment(
        [egui::pos2(a.left() + 2.0, a.bottom()), egui::pos2(a.right() - 2.0, a.bottom())],
        Stroke::new(1.0, Color32::from_rgb(232, 236, 242)),
    );
    let _ = resp;
    (*v - v0).abs() > 1e-9
}

/// 对话框标签页（现代 Excel 365 / WPS 扁平下划线胶囊页签）
pub fn dialog_tabs(ui: &mut egui::Ui, sel: &mut u8, tabs: &[&str]) {
    let top = ui.cursor().top();
    let left = ui.cursor().left();
    let right = left + ui.available_width();
    let tab_h = 32.0;
    ui.painter().line_segment(
        [
            egui::pos2(left, top + tab_h - 0.5),
            egui::pos2(right, top + tab_h - 0.5),
        ],
        Stroke::new(1.0, Color32::from_rgb(226, 232, 240)),
    );
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, t) in tabs.iter().enumerate() {
            let on = *sel as usize == i;
            let font = if on {
                medium_in(ui.ctx(), 12.5)
            } else {
                FontId::proportional(12.5)
            };
            let galley = ui.painter().layout_no_wrap(
                (*t).to_owned(),
                font,
                Color32::PLACEHOLDER,
            );
            let w = galley.size().x + 24.0;
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, tab_h), Sense::click());
            let pos = Align2::CENTER_CENTER
                .anchor_size(rect.center(), galley.size())
                .min;
            if on {
                ui.painter().rect_filled(
                    rect.shrink2(Vec2::new(1.0, 3.0)),
                    6.0,
                    Color32::from_rgb(240, 247, 243),
                );
                let bar = egui::Rect::from_min_size(
                    egui::pos2(rect.left() + 8.0, rect.bottom() - 2.5),
                    Vec2::new((rect.width() - 16.0).max(12.0), 2.5),
                );
                ui.painter().rect_filled(bar, 1.25, ACCENT);
                ui.painter().galley_with_override_text_color(pos, galley, ACCENT);
            } else {
                if resp.hovered() {
                    ui.painter().rect_filled(
                        rect.shrink2(Vec2::new(1.0, 3.0)),
                        6.0,
                        HOVER,
                    );
                }
                ui.painter().galley_with_override_text_color(
                    pos,
                    galley,
                    if resp.hovered() { TEXT } else { TEXT_WEAK },
                );
            }
            if resp.clicked() {
                *sel = i as u8;
            }
        }
    });
    ui.add_space(6.0);
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

/// 协同协作者高辨识度配色板（深色饱和度调优，确保白字名牌对比度 > 4.5:1）
pub fn peer_color(idx: u8) -> Color32 {
    match idx % 7 {
        0 => Color32::from_rgb(217, 70, 31),
        1 => Color32::from_rgb(13, 148, 136),
        2 => Color32::from_rgb(124, 58, 237),
        3 => Color32::from_rgb(2, 132, 199),
        4 => Color32::from_rgb(190, 18, 60),
        5 => Color32::from_rgb(22, 163, 74),
        _ => Color32::from_rgb(180, 83, 9),
    }
}

/// 纯矢量绘制清晰小挂锁图标（杜绝内嵌字体子集缺失 U+1F512 导致方框乱码）
pub fn draw_padlock(painter: &egui::Painter, rect: Rect, col: Color32) {
    let s = rect.width().min(rect.height());
    if s < 6.0 {
        return;
    }
    let cx = rect.center().x;
    let top = rect.center().y - s * 0.44;
    let body = Rect::from_center_size(
        egui::pos2(cx, top + s * 0.62),
        Vec2::new(s * 0.72, s * 0.50),
    );
    let sw = (s * 0.12).max(1.15);
    // 锁梁半圆弧 + 双竖腿
    let r_shackle = s * 0.21;
    let shackle_cy = body.top() - s * 0.12;
    painter.line_segment(
        [
            egui::pos2(cx - r_shackle, body.top() + 0.5),
            egui::pos2(cx - r_shackle, shackle_cy),
        ],
        Stroke::new(sw, col),
    );
    painter.line_segment(
        [
            egui::pos2(cx + r_shackle, body.top() + 0.5),
            egui::pos2(cx + r_shackle, shackle_cy),
        ],
        Stroke::new(sw, col),
    );
    let segs = 8;
    for k in 0..segs {
        let a0 = std::f32::consts::PI * (1.0 + k as f32 / segs as f32);
        let a1 = std::f32::consts::PI * (1.0 + (k + 1) as f32 / segs as f32);
        painter.line_segment(
            [
                egui::pos2(cx + r_shackle * a0.cos(), shackle_cy + r_shackle * a0.sin()),
                egui::pos2(cx + r_shackle * a1.cos(), shackle_cy + r_shackle * a1.sin()),
            ],
            Stroke::new(sw, col),
        );
    }
    // 锁体圆角实心块（或描边）
    painter.rect(
        body,
        (s * 0.12).max(1.5),
        col.gamma_multiply(0.22),
        Stroke::new(sw, col),
        egui::StrokeKind::Inside,
    );
    painter.circle_filled(body.center(), (s * 0.08).max(0.9), col);
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
