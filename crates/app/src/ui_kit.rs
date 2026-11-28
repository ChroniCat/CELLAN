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

/// Excel 绿色（单元格选区及主题强调色）。
pub const ACCENT: Color32 = Color32::from_rgb(33, 115, 70);
/// 分区分隔线 / 控件边框
pub const BORDER: Color32 = Color32::from_rgb(208, 208, 208);

/// CELLAN 品牌色板：瑞士极简 2×2 几何 C-网格（深海蓝 C + 翡翠青与叠层薄荷绿协同格，取自官方矢量标志）。
pub const BRAND_NAVY: Color32 = Color32::from_rgb(31, 69, 158);
pub const BRAND_MINT: Color32 = Color32::from_rgb(15, 168, 126);
pub const BRAND_AQUA: Color32 = Color32::from_rgb(69, 214, 172);
pub const BRAND_AQUA_LIGHT: Color32 = Color32::from_rgb(132, 238, 208);

/// 悬浮卡片：纯白四圆角卡片浮于柔和 PAGE_BG 之上，配清晰利落的 1px CARD_BORDER（与 Light 调色板对齐）。
pub const PAGE_BG: Color32 = Color32::from_rgb(243, 245, 248);
pub const CARD_BORDER: Color32 = Color32::from_rgb(218, 223, 231);
/// 页签栏底色（Fluent 架构下直接与 PAGE_BG 融为一体，消除双拼色断层边框）
pub const TABSTRIP_BG: Color32 = Color32::from_rgb(243, 245, 248);
pub const TABSTRIP_LINE: Color32 = Color32::from_rgb(218, 223, 231);
/// 灰底上的悬停色：与页面底色拉开明确明度差
pub const HOVER_ON_PAGE: Color32 = Color32::from_rgb(231, 236, 243);
/// 卡片圆角：功能区卡片 8px
pub const CARD_R: u8 = 8;

/// 纯矢量绘制 CELLAN 品牌标志（240×240 归一化坐标系，与 `assets/logo.svg` 严格同源）。
/// 结构：瑞士极简 2×2 几何 C-网格——左侧深海蓝建筑感 C，右上翡翠青单元格，
/// 右下薄荷青单元格内置叠层高光块，象征单元格级实时无锁协同。
pub fn draw_brand_logo(painter: &egui::Painter, rect: Rect) {
    let side = rect.width().min(rect.height());
    if side < 6.0 {
        return;
    }
    let sq = Rect::from_center_size(rect.center(), Vec2::splat(side));
    let tx = |x: f32| sq.left() + x / 240.0 * side;
    let ty = |y: f32| sq.top() + y / 240.0 * side;
    let ts = |v: f32| v / 240.0 * side;

    let dark = painter.ctx().style().visuals.dark_mode;
    let navy = if dark {
        Color32::from_rgb(78, 128, 244)
    } else {
        BRAND_NAVY
    };
    let jade = BRAND_MINT;
    let aqua = BRAND_AQUA;
    let aqua_hi = BRAND_AQUA_LIGHT;

    // 按四角独立半径绘制凸圆角矩形 [nw, ne, se, sw]
    let fill_rrect4 = |p: &egui::Painter, x0: f32, y0: f32, w: f32, h: f32, rad: [f32; 4], col: Color32| {
        let max_r = (w * 0.49).min(h * 0.49);
        let [nw, ne, se, sw] = rad.map(|r| r.clamp(0.0, max_r));
        const SEG: usize = 6;
        let mut pts = Vec::with_capacity(4 * (SEG + 1));
        let push_corner = |pts: &mut Vec<egui::Pos2>, cx: f32, cy: f32, r: f32, a0: f32, corner: egui::Pos2| {
            if r <= 0.05 {
                pts.push(corner);
            } else {
                for k in 0..=SEG {
                    let a = a0 + std::f32::consts::FRAC_PI_2 * (k as f32 / SEG as f32);
                    pts.push(egui::pos2(cx + r * a.cos(), cy + r * a.sin()));
                }
            }
        };
        push_corner(&mut pts, x0 + w - ne, y0 + ne, ne, -std::f32::consts::FRAC_PI_2, egui::pos2(x0 + w, y0));
        push_corner(&mut pts, x0 + w - se, y0 + h - se, se, 0.0, egui::pos2(x0 + w, y0 + h));
        push_corner(&mut pts, x0 + sw, y0 + h - sw, sw, std::f32::consts::FRAC_PI_2, egui::pos2(x0, y0 + h));
        push_corner(&mut pts, x0 + nw, y0 + nw, nw, std::f32::consts::PI, egui::pos2(x0, y0));
        p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
    };

    // 绘制 C 形内腔凹圆角倒角（三角扇填充 + 弧边微抗锯齿）
    let fill_inner_fillet = |p: &egui::Painter, corner: egui::Pos2, center: egui::Pos2, r: f32, a0: f32, a1: f32, col: Color32| {
        if r < 0.4 {
            return;
        }
        const SEG: usize = 8;
        let mut mesh = egui::epaint::Mesh::default();
        mesh.colored_vertex(corner, col);
        let mut arc = Vec::with_capacity(SEG + 1);
        for k in 0..=SEG {
            let a = a0 + (a1 - a0) * (k as f32 / SEG as f32);
            let pt = egui::pos2(center.x + r * a.cos(), center.y + r * a.sin());
            mesh.colored_vertex(pt, col);
            arc.push(pt);
        }
        for k in 0..SEG as u32 {
            mesh.add_triangle(0, 1 + k, 2 + k);
        }
        p.add(egui::Shape::mesh(mesh));
        if side >= 14.0 {
            // 沿弧线内缩 0.35px 补一条微细抗锯齿边，消除三角扇斜边锯齿
            let r_aa = (r + 0.35).max(0.5);
            let aa_pts: Vec<egui::Pos2> = (0..=SEG)
                .map(|k| {
                    let a = a0 + (a1 - a0) * (k as f32 / SEG as f32);
                    egui::pos2(center.x + r_aa * a.cos(), center.y + r_aa * a.sin())
                })
                .collect();
            p.add(egui::Shape::line(aa_pts, Stroke::new(0.75, col)));
        }
    };

    // 1. 左侧深海蓝建筑感 C（0..111 × 0..240，壁厚 38，外圆角 22，内倒角 14，右口平齐）
    let r_out = ts(22.0);
    let r_in = ts(14.0);
    let r_cell = ts(20.0);
    let r_hi = ts(16.0);

    fill_rrect4(painter, tx(0.0), ty(0.0), ts(38.0), ts(240.0), [r_out, 0.0, 0.0, r_out], navy);
    fill_rrect4(painter, tx(22.0), ty(0.0), ts(89.0), ts(38.0), [0.0, 0.0, 0.0, 0.0], navy);
    fill_rrect4(painter, tx(22.0), ty(202.0), ts(89.0), ts(38.0), [0.0, 0.0, 0.0, 0.0], navy);
    fill_inner_fillet(
        painter,
        egui::pos2(tx(37.5), ty(37.5)),
        egui::pos2(tx(52.0), ty(52.0)),
        r_in,
        -std::f32::consts::FRAC_PI_2,
        -std::f32::consts::PI,
        navy,
    );
    fill_inner_fillet(
        painter,
        egui::pos2(tx(37.5), ty(202.5)),
        egui::pos2(tx(52.0), ty(188.0)),
        r_in,
        std::f32::consts::PI,
        std::f32::consts::FRAC_PI_2,
        navy,
    );

    // 2. 右上翡翠青单元格（129..240 × 0..111）
    fill_rrect4(painter, tx(129.0), ty(0.0), ts(111.0), ts(111.0), [0.0, r_out, r_cell, r_cell], jade);

    // 3. 右下薄荷青协同格 + 内置叠层同步高光块（129..240 × 129..240）
    fill_rrect4(painter, tx(129.0), ty(129.0), ts(111.0), ts(111.0), [r_cell, r_cell, r_out, 0.0], aqua);
    fill_rrect4(painter, tx(129.0), ty(129.0), ts(79.0), ts(79.0), [r_cell, 0.0, r_hi, 0.0], aqua_hi);
}

/// Chrome 风格标签页轮廓：顶部圆角 + 底部左右外撇凹圆角平滑过渡到底栏，避免“上圆下尖”。
#[allow(dead_code)]
pub fn paint_chrome_tab(
    painter: &egui::Painter,
    rect: Rect,
    top_r: f32,
    flare_r: f32,
    fill: Color32,
    contour: Option<Stroke>,
) {
    paint_chrome_tab_lifted(painter, rect, top_r, flare_r, 0.0, fill, contour);
}

/// Chrome 风格标签页轮廓（支持拖拽悬浮态 `lift_t: 0.0..=1.0`）：
/// - `lift_t == 0`：底部左右外撇凹圆角平滑汇入下方功能区；
/// - `lift_t > 0`：底部凹角柔和收回为圆角浮动卡片，叠柔和弥散投影与细腻边线，放下时再自然融回功能区。
pub fn paint_chrome_tab_lifted(
    painter: &egui::Painter,
    rect: Rect,
    top_r: f32,
    flare_r: f32,
    lift_t: f32,
    fill: Color32,
    contour: Option<Stroke>,
) {
    let lift_t = lift_t.clamp(0.0, 1.0);
    let top_r = top_r.clamp(0.0, (rect.width() * 0.45).min(rect.height() * 0.48));
    // 拖拽提起前半段先平滑收回外撇凹角，后半段柔和过渡出底部圆角
    let flare_t = (1.0 - lift_t * 2.0).clamp(0.0, 1.0);
    let round_t = ((lift_t - 0.35) / 0.65).clamp(0.0, 1.0);
    let r = (flare_r * flare_t).clamp(0.0, (rect.height() - top_r).max(0.0));
    let top_u8 = top_r.round().clamp(0.0, 255.0) as u8;
    let bot_u8 = (6.5 * round_t).round().clamp(0.0, 255.0) as u8;
    let body_bottom = rect.bottom() + 0.5 * flare_t - 2.0 * round_t;
    let body = Rect::from_min_max(rect.min, egui::pos2(rect.right(), body_bottom.max(rect.top() + 8.0)));
    let corners = egui::CornerRadius { nw: top_u8, ne: top_u8, sw: bot_u8, se: bot_u8 };

    if lift_t > 0.01 {
        let pal = crate::theme::palette();
        let dark = pal.theme.is_dark();
        let shadow_alpha = (if dark { 88.0 } else { 34.0 } * lift_t).round().clamp(0.0, 255.0) as u8;
        if shadow_alpha > 0 {
            let shadow = egui::epaint::Shadow {
                offset: [0, (2.0 * lift_t).round() as i8],
                blur: (10.0 * lift_t).round().max(2.0) as u8,
                spread: 0,
                color: Color32::from_black_alpha(shadow_alpha),
            };
            painter.add(shadow.as_shape(body, corners));
        }
    }

    painter.rect_filled(body, corners, fill);

    const SEG: usize = 10;
    let paint_flare = |corner: egui::Pos2, center: egui::Pos2, a0: f32, a1: f32| {
        if r < 0.5 {
            return;
        }
        let mut mesh = egui::epaint::Mesh::default();
        mesh.colored_vertex(corner, fill);
        for k in 0..=SEG {
            let a = a0 + (a1 - a0) * (k as f32 / SEG as f32);
            mesh.colored_vertex(egui::pos2(center.x + r * a.cos(), center.y + r * a.sin()), fill);
        }
        for k in 0..SEG as u32 {
            mesh.add_triangle(0, 1 + k, 2 + k);
        }
        painter.add(egui::Shape::mesh(mesh));
        let r_aa = r + 0.25;
        let aa_pts: Vec<egui::Pos2> = (0..=SEG)
            .map(|k| {
                let a = a0 + (a1 - a0) * (k as f32 / SEG as f32);
                egui::pos2(center.x + r_aa * a.cos(), center.y + r_aa * a.sin())
            })
            .collect();
        painter.add(egui::Shape::line(aa_pts, Stroke::new(0.75, fill)));
    };

    let center_l = egui::pos2(rect.left() - r, rect.bottom() - r);
    let center_r = egui::pos2(rect.right() + r, rect.bottom() - r);
    paint_flare(
        egui::pos2(rect.left() + 0.25, rect.bottom() + 0.5),
        center_l,
        0.0,
        std::f32::consts::FRAC_PI_2,
    );
    paint_flare(
        egui::pos2(rect.right() - 0.25, rect.bottom() + 0.5),
        center_r,
        std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI,
    );

    if let Some(stroke) = contour {
        if round_t > 0.05 && r < 0.5 {
            painter.rect_stroke(body, corners, stroke, egui::StrokeKind::Inside);
        } else {
            let mut pts = Vec::with_capacity(4 * (SEG + 1));
            if r >= 0.5 {
                for k in 0..=SEG {
                    let a = std::f32::consts::FRAC_PI_2 * (1.0 - k as f32 / SEG as f32);
                    pts.push(egui::pos2(center_l.x + r * a.cos(), center_l.y + r * a.sin()));
                }
            } else {
                pts.push(body.left_bottom());
            }
            if top_r >= 0.5 {
                let tl = egui::pos2(rect.left() + top_r, rect.top() + top_r);
                for k in 0..=SEG {
                    let a = std::f32::consts::PI + std::f32::consts::FRAC_PI_2 * (k as f32 / SEG as f32);
                    pts.push(egui::pos2(tl.x + top_r * a.cos(), tl.y + top_r * a.sin()));
                }
                let tr = egui::pos2(rect.right() - top_r, rect.top() + top_r);
                for k in 0..=SEG {
                    let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::FRAC_PI_2 * (k as f32 / SEG as f32);
                    pts.push(egui::pos2(tr.x + top_r * a.cos(), tr.y + top_r * a.sin()));
                }
            } else {
                pts.push(rect.left_top());
                pts.push(rect.right_top());
            }
            if r >= 0.5 {
                for k in 0..=SEG {
                    let a = std::f32::consts::PI - std::f32::consts::FRAC_PI_2 * (k as f32 / SEG as f32);
                    pts.push(egui::pos2(center_r.x + r * a.cos(), center_r.y + r * a.sin()));
                }
            } else {
                pts.push(body.right_bottom());
            }
            painter.add(egui::Shape::line(pts, stroke));
        }
    } else if lift_t > 0.05 {
        let pal = crate::theme::palette();
        let lift_border = lerp_color(Color32::TRANSPARENT, pal.card_border, lift_t * 0.85);
        if lift_border != Color32::TRANSPARENT {
            painter.rect_stroke(body, corners, Stroke::new(1.0, lift_border), egui::StrokeKind::Inside);
        }
    }
}

/// 把一块已经画满（直角）的区域收成圆角卡片：四角先用页面底色「抠」掉，再描 1px 边。
/// egui 的裁切只有矩形，网格、表头、滚动条又各自直角铺色，只能事后盖住四个角。
#[allow(dead_code)]
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

fn fam_ui_italic() -> FontFamily {
    static F: OnceLock<FontFamily> = OnceLock::new();
    F.get_or_init(|| FontFamily::Name("ui-italic".into())).clone()
}

const MASK_DEFAULT_BOLD: u8 = 1 << 0;
const MASK_UI_BOLD: u8 = 1 << 1;
const MASK_UI_MEDIUM: u8 = 1 << 2;
const MASK_UI_ITALIC: u8 = 1 << 3;

/// 按 `(UI_FONTS_GEN, system_fonts::generation())` 缓存四个常用 UI 字族是否存在，
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
        if fams.contains(&fam_ui_italic()) {
            m |= MASK_UI_ITALIC;
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

/// 500~600 中黑字重（拉丁走 Segoe UI Semibold 600，中文走 500 Medium / 微软雅黑 UI）。
/// 小字号（12~13px）按钮/页签/文件名使用单遍原生渲染，彻底杜绝亚像素双描造成的竖画重影发虚。
pub fn medium_in(ctx: &egui::Context, size: f32) -> FontId {
    if font_family_mask(ctx) & MASK_UI_MEDIUM != 0 {
        FontId::new(size, fam_ui_medium())
    } else {
        FontId::proportional(size)
    }
}

/// 真斜体西文字族（`ui-italic`，由 `system_fonts` 挂载 `segoeuii.ttf` 时生效）；未注册时返回 `None`。
pub fn ui_italic_in(ctx: &egui::Context, size: f32) -> Option<FontId> {
    if font_family_mask(ctx) & MASK_UI_ITALIC != 0 {
        Some(FontId::new(size, fam_ui_italic()))
    } else {
        None
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
pub const TEXT: Color32 = Color32::from_rgb(26, 31, 39);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(92, 100, 112);
pub const HEADER_BG: Color32 = Color32::from_rgb(247, 248, 251);
pub const HEADER_SEL: Color32 = Color32::from_rgb(224, 241, 232);
pub const GRID_LINE: Color32 = Color32::from_rgb(226, 230, 236);
/// 页签行底色
pub const TABBAR_BG: Color32 = Color32::from_rgb(243, 245, 248);
/// 标题栏底色
pub const TITLE_BG: Color32 = Color32::from_rgb(243, 245, 248);
pub const HOVER: Color32 = Color32::from_rgb(238, 242, 247);
pub const PRESSED: Color32 = Color32::from_rgb(222, 228, 237);
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

/// 禁用态前景（文字与图标）：浅色主题与经典禁用灰 `DISABLED`（#A6A6A6）一致，
/// 护眼 / 深色 / 高对比主题沿用当前调色板 `text_muted`。
pub fn disabled_color(pal: crate::theme::ThemePalette) -> Color32 {
    if pal.theme == crate::theme::AppTheme::Light {
        DISABLED
    } else {
        pal.text_muted
    }
}

const HOVER_ANIM_SECS: f32 = 0.11;
const TOGGLE_ANIM_SECS: f32 = 0.14;
const ENTER_ANIM_SECS: f32 = 0.12;

#[inline]
fn motion_duration(base_secs: f32) -> f32 {
    if crate::theme::current_theme() == crate::theme::AppTheme::HighContrast
        || crate::debug_flags::is_set("SC_SHOT")
    {
        0.0
    } else {
        base_secs
    }
}

/// Alpha-safe sRGBA color interpolation for micro-animations.
///
/// - Exact endpoints: `t <= 0.001` returns `a`; `t >= 0.999` returns `b`.
/// - When one endpoint is fully transparent (`alpha == 0`), animates alpha `0..other.a()`
///   while preserving the visible endpoint's RGB (never blending through opaque black).
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    if t <= 0.001 {
        return a;
    }
    if t >= 0.999 {
        return b;
    }
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| -> u8 {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    let [ar, ag, ab, aa] = a.to_srgba_unmultiplied();
    let [br, bg, bb, ba] = b.to_srgba_unmultiplied();
    if aa == 0 {
        Color32::from_rgba_unmultiplied(br, bg, bb, mix(0, ba))
    } else if ba == 0 {
        Color32::from_rgba_unmultiplied(ar, ag, ab, mix(aa, 0))
    } else {
        Color32::from_rgba_unmultiplied(
            mix(ar, br),
            mix(ag, bg),
            mix(ab, bb),
            mix(aa, ba),
        )
    }
}

/// Smooth ~110ms hover ease-out (`0.0..=1.0`). Zero repaints at rest; immediate in HighContrast / `SC_SHOT`.
pub fn anim_hover(ctx: &egui::Context, id: egui::Id, active: bool) -> f32 {
    let dur = motion_duration(HOVER_ANIM_SECS);
    if dur <= 0.0 {
        return if active { 1.0 } else { 0.0 };
    }
    ctx.animate_bool_with_time_and_easing(id, active, dur, egui::emath::easing::cubic_out)
}

/// Smooth ~140ms toggle/active ease-out (`0.0..=1.0`). Zero repaints at rest; immediate in HighContrast / `SC_SHOT`.
pub fn anim_toggle(ctx: &egui::Context, id: egui::Id, on: bool) -> f32 {
    let dur = motion_duration(TOGGLE_ANIM_SECS);
    if dur <= 0.0 {
        return if on { 1.0 } else { 0.0 };
    }
    ctx.animate_bool_with_time_and_easing(id, on, dur, egui::emath::easing::cubic_out)
}

/// Smooth ~115ms drag-lift elevation ease-out (`0.0..=1.0`). Immediate in HighContrast / `SC_SHOT`.
pub fn anim_drag_lift(ctx: &egui::Context, id: egui::Id, lifted: bool) -> f32 {
    let dur = motion_duration(0.115);
    if dur <= 0.0 || ctx.input(|i| i.time <= 0.0) {
        return if lifted { 1.0 } else { 0.0 };
    }
    ctx.animate_bool_with_time_and_easing(id, lifted, dur, egui::emath::easing::cubic_out)
}

/// 纯物理阻尼弹簧单步积分（无全局主题/上下文副作用，供 `anim_spring` 与单元测试共用）。
pub fn anim_spring_step(
    value: &mut f32,
    velocity: &mut f32,
    target: f32,
    dt: f32,
    omega: f32,
    zeta: f32,
) -> bool {
    if !dt.is_finite() || dt <= 0.0 {
        return (*value - target).abs() >= 0.25 || velocity.abs() >= 4.0;
    }
    let step_dt = dt.min(0.05);
    let sub_steps = if step_dt > 0.02 { 4 } else { 2 };
    let sub = step_dt / sub_steps as f32;
    for _ in 0..sub_steps {
        let acc = -omega * omega * (*value - target) - 2.0 * zeta * omega * (*velocity);
        *velocity += acc * sub;
        *value += *velocity * sub;
    }
    if (*value - target).abs() < 0.25 && velocity.abs() < 4.0 {
        *value = target;
        *velocity = 0.0;
        false
    } else {
        true
    }
}

/// 轻量阻尼弹簧单步推进（像素坐标）：用于标签页拖拽让位、松手落位回弹等灵动微动效。
///
/// - 推荐 `omega = 26.0`, `zeta = 0.86`：约 140ms 自然加减速落位，柔和不死板、不晃眼。
/// - 高对比主题、`SC_SHOT` 截图或静态单帧测试（`time <= 0`）直接一步到位；
///   同一时间戳内的多遍重排（`time > 0 && dt == 0`）保持当前位置不误瞬移。
pub fn anim_spring(
    ctx: &egui::Context,
    value: &mut f32,
    velocity: &mut f32,
    target: f32,
    omega: f32,
    zeta: f32,
) -> bool {
    if motion_duration(1.0) <= 0.0 {
        *value = target;
        *velocity = 0.0;
        return false;
    }
    let (now, dt) = ctx.input(|i| (i.time, i.unstable_dt));
    if now <= 0.0 || !dt.is_finite() {
        *value = target;
        *velocity = 0.0;
        return false;
    }
    if dt <= 0.0 {
        return (*value - target).abs() >= 0.25 || velocity.abs() >= 4.0;
    }
    let moving = anim_spring_step(value, velocity, target, dt, omega, zeta);
    if moving {
        ctx.request_repaint();
    }
    moving
}

/// Subtle ~120ms entrance ease-out (`0.0..=1.0`) from the first shown pass.
///
/// Snaps immediately to `1.0` when `dt == 0.0`, in static single-frame test harnesses
/// (`time <= 0.0`), in HighContrast, or during `SC_SHOT` captures so unit tests and
/// screenshots always settle at crisp final opacity and geometry.
pub fn anim_enter(ctx: &egui::Context, id: egui::Id) -> f32 {
    let dur = motion_duration(ENTER_ANIM_SECS);
    if dur <= 0.0 {
        return 1.0;
    }
    let pass = ctx.cumulative_pass_nr();
    let (now, dt) = ctx.input(|i| (i.time, i.unstable_dt));
    let key = id.with("__anim_enter");
    let (start_time, opening) = ctx.data_mut(|d| {
        let prev = d.get_temp::<(u64, f64)>(key);
        let opening = prev.is_none_or(|(last_pass, _)| pass.saturating_sub(last_pass) > 1);
        let start = if opening {
            // On initial boot / single-frame test pass (`pass == 0`), or when `now <= 0.0` / `dt <= 0.0`,
            // settle immediately at 1.0 so static tests and startup frames land crisp.
            if pass == 0 || now <= 0.0 || dt <= 0.0 {
                -10.0
            } else {
                now
            }
        } else {
            prev.unwrap().1
        };
        d.insert_temp(key, (pass, start));
        (start, opening)
    });
    if start_time < 0.0 || (!opening && (now - start_time) <= 0.0) {
        return 1.0;
    }
    let elapsed = (now - start_time).max(0.0) as f32;
    if elapsed >= dur {
        return 1.0;
    }
    let t = (elapsed / dur).clamp(0.0, 1.0);
    let eased = egui::emath::easing::cubic_out(t);
    ctx.request_repaint();
    eased
}

/// Smoothly animated control surface background:
/// disabled → transparent, pointer down → immediate crisp `pressed`,
/// hover → 110ms alpha-safe fade to `hover`, toggled `on` → 140ms fade to `pressed`.
pub fn anim_state_bg(
    ctx: &egui::Context,
    id: egui::Id,
    enabled: bool,
    on: bool,
    hovered: bool,
    down: bool,
    dark: bool,
) -> Color32 {
    let pal = crate::theme::palette();
    let effective = if dark && !pal.theme.is_dark() {
        crate::theme::palette_for(crate::theme::AppTheme::Dark)
    } else {
        pal
    };
    let hover_t = anim_hover(ctx, id.with("__hov"), enabled && hovered);
    let on_t = anim_toggle(ctx, id.with("__on"), enabled && on);
    if !enabled {
        return Color32::TRANSPARENT;
    }
    if down {
        return effective.pressed;
    }
    let hover_col = lerp_color(Color32::TRANSPARENT, effective.hover, hover_t);
    lerp_color(hover_col, effective.pressed, on_t)
}

/// Excel 浅色主题；深色沿用原来的深灰配色（Excel 也有深色模式）。
/// 两套主题都要显式写入：egui 切主题时会用「该主题已保存的样式」，
/// 只改当前主题会让系统深色模式下功能区变成默认灰。
pub fn apply_theme(ctx: &egui::Context) {
    let mut base = (*ctx.style()).clone();
    base.text_styles = [
        (egui::TextStyle::Heading, FontId::proportional(14.0)),
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
    let pal = crate::theme::palette();
    let col = if !enabled {
        disabled_color(pal)
    } else {
        text_color(ui.visuals())
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(12.0), col);
    let w = (galley.size().x + 24.0).max(min_w).max(ui.available_width());
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 27.0), sense);
    let bg = anim_state_bg(
        ui.ctx(),
        resp.id,
        enabled,
        false,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        pal.theme.is_dark(),
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 5.0, bg);
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
        let pal = crate::theme::palette();
        let center = ui.cursor().top() + RIBBON_BODY_H * 0.5;
        ui.painter().vline(
            ui.cursor().left(), (center - 17.0)..=(center + 17.0),
            Stroke::new(crate::tokens::stroke::HAIR, pal.chrome_separator()),
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
            let c = crate::theme::palette().text_weak;
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
    let pal = crate::theme::palette();
    let hov_t = anim_hover(ui.ctx(), resp.id.with("hov"), resp.hovered());
    let bg = if resp.is_pointer_button_down_on() {
        pal.pressed
    } else {
        lerp_color(Color32::TRANSPARENT, pal.hover, hov_t)
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let c = lerp_color(pal.text_weak, pal.accent, hov_t);
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
    let pal = crate::theme::palette();
    let col = if ui.is_enabled() {
        text_color(ui.visuals())
    } else {
        disabled_color(pal)
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
    let pal = crate::theme::palette();
    let txt_col = text_color(ui.visuals());
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let bg = anim_state_bg(
        ui.ctx(),
        resp.id,
        enabled,
        false,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        pal.theme.is_dark(),
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 5.0, bg);
    }
    focus_ring(ui, &resp, rect, 5.0);
    let col = if enabled { txt_col } else { disabled_color(pal) };
    let icon_size = 30.0_f32.min(size.y - 26.0).max(16.0);
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 8.0 + icon_size / 2.0),
        Vec2::splat(icon_size),
    );
    ui_icons::draw_control(ui, icon_rect, icon, col, enabled);
    if let Some(g) = galley {
        let pos = Align2::CENTER_CENTER
            .anchor_size(egui::pos2(rect.center().x, rect.bottom() - 15.0), g.size())
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
    let pal = crate::theme::palette();
    let col = if ui.is_enabled() {
        text_color(ui.visuals())
    } else {
        disabled_color(pal)
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
    let pal = crate::theme::palette();
    let txt_col = text_color(ui.visuals());
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
    let hov_t = anim_hover(
        ui.ctx(),
        resp.id.with("split_hov"),
        enabled && ((resp.hovered() && !over_arrow) || aresp.hovered()),
    );
    let bg = if !enabled {
        Color32::TRANSPARENT
    } else if resp.is_pointer_button_down_on() || aresp.is_pointer_button_down_on() {
        pal.pressed
    } else {
        lerp_color(Color32::TRANSPARENT, pal.hover, hov_t)
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 5.0, bg);
    }
    let col = if enabled { txt_col } else { disabled_color(pal) };
    let icon_size = 28.0_f32.min(size.y - 28.0).max(16.0);
    let icon_rect = Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 8.0 + icon_size / 2.0),
        Vec2::splat(icon_size),
    );
    ui_icons::draw_control(ui, icon_rect, icon, col, enabled);
    if let Some(g) = galley {
        let pos = Align2::CENTER_CENTER
            .anchor_size(egui::pos2(rect.center().x, rect.bottom() - 24.0), g.size())
            .min;
        ui.painter().galley(pos, g, col);
    }
    let sep_t = anim_hover(ui.ctx(), resp.id.with("split_sep"), hovered_any && enabled);
    let sep_col = lerp_color(Color32::TRANSPARENT, pal.card_border, sep_t);
    if sep_col != Color32::TRANSPARENT {
        ui.painter().line_segment(
            [
                egui::pos2(rect.left() + 4.0, rect.bottom() - 15.0),
                egui::pos2(rect.right() - 4.0, rect.bottom() - 15.0),
            ],
            Stroke::new(1.0, sep_col),
        );
    }
    caret(
        ui,
        egui::pos2(rect.center().x, rect.bottom() - 8.5),
        if enabled { pal.text_weak } else { disabled_color(pal) },
    );
    (resp, aresp)
}
pub fn small_on(ui: &mut egui::Ui, icon: Icon, on: bool, tip: &str) -> Response {
    let pal = crate::theme::palette();
    let txt_col = text_color(ui.visuals());
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    let bg = anim_state_bg(
        ui.ctx(),
        resp.id,
        enabled,
        on,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        pal.theme.is_dark(),
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let col = if !enabled { disabled_color(pal) } else { txt_col };
    ui_icons::draw_control(ui, rect.shrink(4.0), icon, col, enabled);
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
            Stroke::new(crate::tokens::stroke::FOCUS, crate::theme::palette().accent),
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
    let bg = anim_state_bg(
        ui.ctx(),
        resp.id,
        enabled,
        false,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        dark,
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 2.0, bg);
    }
    let pal = crate::theme::palette();
    let box_rect = Rect::from_center_size(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        Vec2::splat(13.0),
    );
    if checked && enabled {
        ui.painter().rect_filled(box_rect, 2.0, pal.accent);
        let p = ui.painter();
        let tick_col = if pal.theme == crate::theme::AppTheme::HighContrast {
            Color32::BLACK
        } else {
            Color32::WHITE
        };
        let st = Stroke::new(1.8, tick_col);
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
            pal.card_bg,
            Stroke::new(1.0, if enabled { pal.text_weak } else { pal.card_border }),
            egui::StrokeKind::Inside,
        );
    }
    let fg = if enabled { txt_col } else { disabled_color(pal) };
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
    let pal = crate::theme::palette();
    let col = if ui.is_enabled() {
        text_color(ui.visuals())
    } else {
        disabled_color(pal)
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
    let pal = crate::theme::palette();
    let txt_col = text_color(ui.visuals());
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click());
    let bg = anim_state_bg(
        ui.ctx(),
        resp.id,
        enabled,
        on,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        pal.theme.is_dark(),
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let col = if enabled { txt_col } else { disabled_color(pal) };
    let ir = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 4.0, rect.center().y - 8.0),
        Vec2::splat(16.0),
    );
    ui_icons::draw_control(ui, ir, icon, col, enabled);
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
    let pal = crate::theme::palette();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click());
    let hov_t = anim_hover(ui.ctx(), resp.id.with("tint_hov"), resp.hovered());
    let bg = if resp.is_pointer_button_down_on() {
        pal.pressed
    } else {
        lerp_color(Color32::TRANSPARENT, pal.hover_on_page, hov_t)
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 4.0, bg);
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



/// 控件状态底色：禁用不给底、按下/选中用当前主题 `pressed`、悬停用 `hover`
pub fn state_bg(enabled: bool, on: bool, hovered: bool, down: bool, dark: bool) -> Color32 {
    if !enabled {
        return Color32::TRANSPARENT;
    }
    let pal = crate::theme::palette();
    let effective = if dark && !pal.theme.is_dark() {
        crate::theme::palette_for(crate::theme::AppTheme::Dark)
    } else {
        pal
    };
    if down || on {
        effective.pressed
    } else if hovered {
        effective.hover
    } else {
        Color32::TRANSPARENT
    }
}

/// 拆分按钮右侧的小箭头按钮：悬停底色 + 细折线箭头
pub fn arrow_glyph_button(ui: &mut egui::Ui, size: Vec2, color: Color32) -> Response {
    let pal = crate::theme::palette();
    let dark = ui.visuals().dark_mode || pal.theme.is_dark();
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let bg = anim_state_bg(
        ui.ctx(),
        resp.id,
        enabled,
        false,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        dark,
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect.shrink(1.0), 2.0, bg);
    }
    let c = if enabled { color } else { disabled_color(pal) };
    caret(ui, rect.center(), c);
    resp
}

/// 颜色拆分按钮的主体：点击应用当前色，右侧独立箭头打开调色板。
pub fn color_button(
    ui: &mut egui::Ui,
    icon: Icon,
    color: Color32,
    tip: &str,
) -> Response {
    let pal = crate::theme::palette();
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    let bg = anim_state_bg(
        ui.ctx(),
        resp.id,
        enabled,
        false,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        pal.theme.is_dark(),
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 2.0, bg);
    }
    focus_ring(ui, &resp, rect, 2.0);
    let fg = if enabled { pal.text } else { disabled_color(pal) };
    let center = rect.center() - Vec2::new(0.0, 2.0);
    if icon == Icon::FontColor {
        ui.painter().text(
            center,
            Align2::CENTER_CENTER,
            "A",
            FontId::proportional(15.0),
            fg,
        );
    } else {
        ui_icons::draw_mono(ui.painter(), Rect::from_center_size(center, Vec2::splat(17.0)), icon, fg);
    }
    let bar = Rect::from_min_size(
        egui::pos2(rect.left() + 4.0, rect.bottom() - 6.0),
        Vec2::new(16.0, 4.0),
    );
    let swatch = if enabled { color } else { disabled_color(pal) };
    ui.painter().rect_filled(bar, 1.0, swatch);
    let lum = |c: Color32| egui::Rgba::from(c).intensity() + 0.05;
    let (a, b) = (lum(swatch), lum(pal.ribbon_bg));
    if a.max(b) / a.min(b) < 3.0 {
        ui.painter().rect_stroke(bar, 1.0, Stroke::new(1.0, pal.text_muted), egui::StrokeKind::Inside);
    }
    if !tip.is_empty() && (resp.hovered() || resp.contains_pointer()) {
        resp.on_hover_text(tip)
    } else {
        resp
    }
}

/// 字体类开关（B / I / U）：使用真实粗体字族、真实斜体字族（缺失时矢量斜体 `I`）与精细下划线。
pub fn text_toggle(
    ui: &mut egui::Ui,
    label: &str,
    on: bool,
    tip: &str,
    strong: bool,
    italic: bool,
    underline: bool,
) -> Response {
    let pal = crate::theme::palette();
    let dark = ui.visuals().dark_mode || pal.theme.is_dark();
    let txt_col = text_color(ui.visuals());
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
    let bg = anim_state_bg(
        ui.ctx(),
        resp.id,
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
    let col = if enabled { txt_col } else { disabled_color(pal) };
    let pos = rect.center() - Vec2::new(0.0, 0.5);
    if italic {
        if let Some(italic_font) = ui_italic_in(ui.ctx(), 13.0) {
            let g = ui.painter().layout_no_wrap(label.to_owned(), italic_font, col);
            let gp = Align2::CENTER_CENTER.anchor_size(pos, g.size()).min;
            ui.painter().galley(gp, g, col);
        } else if label == "I" {
            // 未注册 `ui-italic` 时绘制清晰光学倾斜矢量 `I`（顶部衬线 + 14° 倾斜竖干 + 底部衬线）
            let p = ui.painter();
            let c = rect.center();
            let st = Stroke::new(1.35, col);
            p.line_segment([egui::pos2(c.x - 1.2, c.y - 4.8), egui::pos2(c.x + 3.6, c.y - 4.8)], st);
            p.line_segment([egui::pos2(c.x + 1.2, c.y - 4.8), egui::pos2(c.x - 1.2, c.y + 4.8)], st);
            p.line_segment([egui::pos2(c.x - 3.6, c.y + 4.8), egui::pos2(c.x + 1.2, c.y + 4.8)], st);
        } else {
            let g = ui
                .painter()
                .layout_no_wrap(label.to_owned(), FontId::proportional(13.0), col);
            let gp = Align2::CENTER_CENTER.anchor_size(pos, g.size()).min;
            ui.painter().galley(gp, g, col);
        }
    } else {
        let font = if strong {
            ui_bold_in(ui.ctx(), 13.0)
        } else {
            FontId::proportional(13.0)
        };
        let g = ui.painter().layout_no_wrap(label.to_owned(), font, col);
        let gp = Align2::CENTER_CENTER.anchor_size(pos, g.size()).min;
        ui.painter().galley(gp, g, col);
    }
    if underline {
        let uy = pos.y + 6.5;
        ui.painter().line_segment(
            [egui::pos2(pos.x - 4.5, uy), egui::pos2(pos.x + 4.5, uy)],
            Stroke::new(1.25, col),
        );
    }
    if !tip.is_empty() && (resp.hovered() || resp.contains_pointer()) {
        resp.on_hover_text(tip)
    } else {
        resp
    }
}

/// 浮于页面底色（`page_bg` / `title_bg`）之上的当前页签底色和文字色：
/// 采用干净克制的柔和中性底面 `pressed` + 清晰墨色 `text`（无绿色指示条）；高对比使用反白。
pub fn active_tab_colors(pal: crate::theme::ThemePalette) -> (Color32, Color32) {
    if pal.theme == crate::theme::AppTheme::HighContrast {
        (pal.text, pal.page_bg)
    } else {
        (pal.pressed, pal.text)
    }
}

/// 位于弹窗/浮层底面（`dialog_bg`）内部的选中项底色和文字色：
/// 统一采用柔和中性胶囊底 `pressed` + 清晰墨色 `text`；高对比反白。
pub fn active_dialog_tab_colors(pal: crate::theme::ThemePalette) -> (Color32, Color32) {
    if pal.theme == crate::theme::AppTheme::HighContrast {
        (pal.text, pal.page_bg)
    } else {
        (pal.pressed, pal.text)
    }
}

/// 高对比反白页签内的黑色焦点环，在白底上仍然可见。
pub fn tab_focus_ring(ui: &egui::Ui, resp: &Response, chip: Rect, radius: f32, active: bool) {
    let pal = crate::theme::palette();
    if active && pal.theme == crate::theme::AppTheme::HighContrast {
        if resp.has_focus() {
            ui.painter().rect_stroke(chip.shrink(1.0), radius,
                Stroke::new(crate::tokens::stroke::FOCUS, pal.page_bg), egui::StrokeKind::Inside);
        }
    } else {
        focus_ring(ui, resp, chip, radius);
    }
}

/// 按实际字重测量标签，溢出布局与最终绘制共用同一份字形。
pub fn layout_tab(ui: &egui::Ui, name: &str, emphasized: bool) -> (Arc<egui::Galley>, f32) {
    let font = if emphasized {
        medium_in(ui.ctx(), 12.0)
    } else {
        FontId::proportional(12.0)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font, Color32::PLACEHOLDER);
    let width = galley.size().x + 20.0;
    (galley, width)
}

pub fn tab_ex(ui: &mut egui::Ui, layout: (Arc<egui::Galley>, f32), active: bool, green: bool) -> Response {
    let (galley, width) = layout;
    let pal = crate::theme::palette();
    let dark = ui.visuals().dark_mode || pal.theme.is_dark();
    let is_hc = pal.theme == crate::theme::AppTheme::HighContrast;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, TAB_H), Sense::click());
    let pill = rect.shrink2(Vec2::new(2.0, 3.0));
    let (selected_fill, selected_ink) = active_tab_colors(pal);
    let hov_t = anim_hover(ui.ctx(), resp.id.with("tab_hov"), resp.hovered());
    if green {
        let hover_fill = if dark {
            pal.pressed
        } else {
            pal.accent_tint.lerp_to_gamma(pal.accent, 0.14)
        };
        let fill = if resp.is_pointer_button_down_on() {
            pal.pressed
        } else {
            lerp_color(pal.accent_tint, hover_fill, hov_t)
        };
        ui.painter().rect_filled(pill, 5.0, fill);
        if is_hc {
            ui.painter().rect_stroke(
                pill,
                5.0,
                Stroke::new(1.0, pal.accent),
                egui::StrokeKind::Inside,
            );
        }
    } else if active {
        ui.painter().rect_filled(pill, 5.0, selected_fill);
    } else {
        let bg = lerp_color(Color32::TRANSPARENT, pal.hover_on_page, hov_t);
        if bg != Color32::TRANSPARENT {
            ui.painter().rect_filled(pill, 5.0, bg);
        }
    }
    let col = if green {
        pal.accent
    } else if active {
        selected_ink
    } else {
        lerp_color(pal.text_weak, pal.text, hov_t * 0.65)
    };
    let pos = Align2::CENTER_CENTER
        .anchor_size(rect.center(), galley.size())
        .min;
    ui.painter().galley_with_override_text_color(pos, galley, col);
    resp
}

/// 分组之间的竖细线（功能区内手动用）
pub fn hairline(ui: &mut egui::Ui) {
    let line_col = crate::theme::palette().chrome_separator();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, GROUP_BODY_H), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, line_col);
}

/// 对话框底部的按钮行（右对齐，统一 12px 呼吸留白与细分割线）
pub fn footer(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(10.0);
    let r = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [egui::pos2(r.left(), r.top()), egui::pos2(r.right(), r.top())],
        Stroke::new(1.0, crate::theme::palette().card_border),
    );
    ui.add_space(12.0);
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), 32.0),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            add(ui);
        },
    );
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
        Stroke::new(1.0, crate::theme::palette().card_border),
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
    let pal = crate::theme::palette();
    match kind {
        StateKind::Empty => pal.text_weak,
        StateKind::Error => pal.err_fg,
        StateKind::Warn => pal.warn_fg,
        StateKind::Info => pal.accent,
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
                        .color(crate::theme::palette().text_weak),
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
    let enabled = ui.is_enabled();
    let fg = if !enabled {
        pal.text
    } else if pal.theme.is_dark() {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
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
            let btn = if enabled {
                btn
            } else {
                btn.fill(pal.page_bg).stroke(Stroke::new(1.0, pal.card_border))
            };
            ui.add(btn)
        })
        .inner;
    button_focus_ring(ui, &resp);
    let in_non_modal = CURRENT_DLG_NON_MODAL.with(|c| c.get());
    let enter = ui.is_enabled() && !in_non_modal
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
    let resp = ui.scope(|ui| {
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
    .inner;
    button_focus_ring(ui, &resp);
    resp
}

fn button_focus_ring(ui: &egui::Ui, resp: &Response) {
    if resp.has_focus() {
        ui.painter().rect_stroke(
            resp.rect.expand(1.5),
            7.5,
            Stroke::new(crate::tokens::stroke::FOCUS, crate::theme::palette().accent),
            egui::StrokeKind::Outside,
        );
    }
}

/// 单行输入框的留白、点击范围和可访问范围共用同一矩形。
pub fn text_input(ui: &mut egui::Ui, buf: &mut String, hint: &str) -> Response {
    let pal = crate::theme::palette();
    let margin = egui::Margin::symmetric(10, 4);
    let shape = ui.painter().add(egui::Shape::Noop);
    let mut response = ui.add(
        egui::TextEdit::singleline(buf)
            .frame(false)
            .margin(margin)
            .min_size(Vec2::new(0.0, 34.0))
            .desired_width(f32::INFINITY)
            .vertical_align(egui::Align::Center)
            .font(FontId::proportional(T_BODY))
            .text_color(pal.text)
            .hint_text(RichText::new(hint).size(T_BODY).color(pal.text_weak)),
    );
    // egui 0.31 在框体上处理鼠标，但返回的 rect 扣除了 TextEdit 的 margin。
    response.rect += margin;
    let focused = response.has_focus();
    let hov_t = anim_hover(ui.ctx(), response.id.with("ti_hov"), response.hovered() && !focused);
    let border = if focused {
        Stroke::new(crate::tokens::stroke::FOCUS, pal.accent)
    } else {
        Stroke::new(1.0, lerp_color(pal.card_border, pal.text_muted, hov_t))
    };
    ui.painter().set(shape, egui::epaint::RectShape::new(
        response.rect,
        4.0,
        if focused { pal.grid_bg } else { pal.card_bg },
        border,
        egui::StrokeKind::Inside,
    ));
    response
}

/// 紧凑文字按钮（组里放"插入行"这种短命令）
pub fn pill(ui: &mut egui::Ui, label: &str, tip: &str) -> Response {
    let enabled = ui.is_enabled();
    let pal = crate::theme::palette();
    let font = FontId::proportional(crate::tokens::text::BODY);
    // egui 已在禁用的父 Ui 上降低透明度，避免文字二次变淡。
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, pal.text);
    let size = Vec2::new(
        galley.size().x + crate::tokens::space::XL,
        crate::tokens::size::BTN_H,
    );
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let bg = anim_state_bg(
        ui.ctx(),
        resp.id,
        enabled,
        false,
        resp.hovered(),
        resp.is_pointer_button_down_on(),
        pal.theme.is_dark(),
    );
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 3.0, bg);
    }
    let pos = Align2::CENTER_CENTER
        .anchor_size(rect.center(), galley.size())
        .min;
    ui.painter().galley(pos, galley, pal.text);
    focus_ring(ui, &resp, rect, 3.0);
    a11y_name(&resp, label, enabled, None);
    if !tip.is_empty() && tip != label {
        let _ = ui.ctx().accesskit_node_builder(resp.id, |node| node.set_description(tip));
    }
    if crate::debug_flags::ui_log_2() {
        eprintln!("[ui]   pill {} w={:.1} x={:.1}", label, rect.width(), rect.min.x);
    }
    if !tip.is_empty() && (resp.hovered() || resp.contains_pointer()) {
        resp.on_hover_text(tip)
    } else {
        resp
    }
}

/// 侧边栏和浮层使用相同的关闭图标、指针状态和键盘焦点。
pub fn close_icon_button(ui: &mut egui::Ui, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    paint_close_button(ui, rect, resp, tip)
}

fn paint_close_button(ui: &egui::Ui, rect: Rect, resp: Response, tip: &str) -> Response {
    let pal = crate::theme::palette();
    let hov_t = anim_hover(ui.ctx(), resp.id.with("close_hov"), resp.hovered());
    let bg = if resp.is_pointer_button_down_on() {
        pal.pressed
    } else {
        lerp_color(Color32::TRANSPARENT, pal.hover, hov_t)
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 4.0, bg);
    }
    focus_ring(ui, &resp, rect, 4.0);
    let col = if resp.has_focus() {
        pal.text
    } else {
        lerp_color(pal.text_weak, pal.text, hov_t)
    };
    ui_icons::draw_mono(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(20.0)), Icon::Close, col);
    a11y_name(&resp, if tip.is_empty() { "关闭" } else { tip }, resp.enabled(), None);
    if !tip.is_empty() {
        resp.on_hover_text(tip)
    } else {
        resp
    }
}

/// 先排版再分配标签区：用相同的字形计算滚动边界和绘制省略号。
pub fn sheet_tab_label(ui: &egui::Ui, name: &str, active: bool, max_width: f32) -> Arc<egui::Galley> {
    let font = if active {
        medium_in(ui.ctx(), crate::tokens::text::BODY)
    } else {
        FontId::proportional(crate::tokens::text::BODY)
    };
    let mut job = egui::text::LayoutJob::simple_singleline(name.to_owned(), font, Color32::PLACEHOLDER);
    job.wrap = egui::text::TextWrapping::truncate_at_width((max_width - crate::tokens::size::SHEET_TAB_PAD).max(1.0));
    ui.painter().layout_job(job)
}

/// 绘制工作表标签视觉外观（复用于常态标签、平滑让位动画和拖拽悬浮胶囊）。
pub fn paint_sheet_tab_face(
    painter: &egui::Painter,
    rect: Rect,
    galley: Arc<egui::Galley>,
    active: bool,
    grouped: bool,
    hov_t: f32,
    lift_t: f32,
    color: Option<[u8; 3]>,
) {
    let pal = crate::theme::palette();
    let is_hc = pal.theme == crate::theme::AppTheme::HighContrast;
    let lift_t = lift_t.clamp(0.0, 1.0);
    let draw_rect = rect.translate(Vec2::new(0.0, -1.5 * lift_t));
    let chip = draw_rect.shrink2(Vec2::new(1.0, 3.0));
    let (base_fill, base_text) = if active {
        active_tab_colors(pal)
    } else if grouped {
        (pal.accent_tint, pal.accent_hover)
    } else {
        (
            lerp_color(Color32::TRANSPARENT, pal.hover_on_page, hov_t),
            lerp_color(pal.text_weak, pal.text, hov_t * 0.6),
        )
    };
    let (fill, text_color) = if lift_t > 0.01 {
        let lifted_surface = if active || grouped {
            base_fill
        } else {
            pal.card_bg
        };
        (
            lerp_color(base_fill, lifted_surface, lift_t),
            lerp_color(base_text, if active { base_text } else { pal.text }, lift_t * 0.8),
        )
    } else {
        (base_fill, base_text)
    };

    if lift_t > 0.01 {
        let shadow_alpha = (if pal.theme.is_dark() { 92.0 } else { 38.0 } * lift_t)
            .round()
            .clamp(0.0, 255.0) as u8;
        if shadow_alpha > 0 {
            let shadow = egui::epaint::Shadow {
                offset: [0, (2.0 * lift_t).round() as i8],
                blur: (9.0 * lift_t).round().max(2.0) as u8,
                spread: 0,
                color: Color32::from_black_alpha(shadow_alpha),
            };
            painter.add(shadow.as_shape(chip, 5.0));
        }
    }

    if fill != Color32::TRANSPARENT {
        painter.rect_filled(chip, 5.0, fill);
    }
    if lift_t > 0.05 {
        let border_col = lerp_color(pal.card_border, pal.accent, 0.35);
        let stroke_col = lerp_color(Color32::TRANSPARENT, border_col, lift_t * 0.9);
        if stroke_col != Color32::TRANSPARENT {
            painter.rect_stroke(chip, 5.0, Stroke::new(1.0, stroke_col), egui::StrokeKind::Inside);
        }
    }

    let center_y = draw_rect.center().y;
    let color_offset = if let Some([r, g, b]) = color {
        let swatch = Rect::from_center_size(egui::pos2(draw_rect.left() + 10.0, center_y), Vec2::splat(8.0));
        let edge = if active && is_hc { text_color } else { pal.text_weak };
        painter.rect(swatch, 2.0, Color32::from_rgb(r, g, b), Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        5.0
    } else {
        0.0
    };
    let text_pos = Align2::CENTER_CENTER
        .anchor_size(egui::pos2(draw_rect.center().x + color_offset, center_y), galley.size())
        .min;
    painter.galley_with_override_text_color(text_pos, galley, text_color);
}

/// 分配工作表标签交互槽位并支持水平让位偏移动画（`visual_dx`）与拖拽隐身（`ghost_source`）。
pub fn sheet_tab_animated(
    ui: &mut egui::Ui,
    name: &str,
    galley: Arc<egui::Galley>,
    active: bool,
    grouped: bool,
    color: Option<[u8; 3]>,
    visual_dx: f32,
    ghost_source: bool,
    suppress_hover: bool,
) -> Response {
    let size = Vec2::new(galley.size().x + crate::tokens::size::SHEET_TAB_PAD, crate::tokens::size::SHEET_TAB_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    a11y_role(&resp, name, egui::accesskit::Role::Tab, Some(active || grouped));
    let hov_t = anim_hover(
        ui.ctx(),
        resp.id.with("sheet_hov"),
        !suppress_hover && !active && !grouped && resp.hovered(),
    );
    let elided = galley.elided;
    if !ghost_source {
        let shifted = rect.translate(Vec2::new(visual_dx, 0.0));
        paint_sheet_tab_face(ui.painter(), shifted, galley, active, grouped, hov_t, 0.0, color);
        tab_focus_ring(ui, &resp, shifted.shrink2(Vec2::new(1.0, 3.0)), 5.0, active);
    }
    if elided && !suppress_hover {
        resp.on_hover_text(name)
    } else {
        resp
    }
}

#[allow(dead_code)]
pub fn sheet_tab(
    ui: &mut egui::Ui,
    name: &str,
    galley: Arc<egui::Galley>,
    active: bool,
    grouped: bool,
    color: Option<[u8; 3]>,
) -> Response {
    sheet_tab_animated(ui, name, galley, active, grouped, color, 0.0, false, false)
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
        let pass = ctx.cumulative_pass_nr();
        let opening = ctx.data_mut(|d| {
            let last = d.get_temp::<u64>(id.with("shown"));
            d.insert_temp(id.with("shown"), pass);
            last.is_none_or(|last| pass.saturating_sub(last) > 1)
        });
        let now = ctx.input(|i| i.time);
        if opening {
            let before = ctx.memory(|m| m.focused());
            ctx.data_mut(|d| {
                d.insert_temp(id.with("born"), now);
                d.insert_temp(id.with("focus_before"), before);
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
        let measured = ctx.memory(|m| m.area_rect(id.with("area")));
        let mut pos = ctx
            .data(|d| d.get_temp::<egui::Pos2>(id.with("pos")))
            .unwrap_or_else(|| {
                let (a, off) = self
                    .anchor
                    .unwrap_or((egui::Align2::CENTER_CENTER, [0.0, 0.0]));
                let est_h = measured.map_or_else(
                    || if self.height > 0.0 { self.height + 80.0 } else { 420.0 },
                    |r| r.height(),
                ).min(screen.height() - 8.0);
                let p = a.align_size_within_rect(
                    egui::vec2(dialog_width(self.width, screen), est_h),
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
        let born = ctx.data(|d| d.get_temp::<f64>(id.with("born"))).unwrap_or(now);
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new("dlg_born"), born);
        });
        let pal = crate::theme::palette();
        let enter = anim_enter(ctx, id);
        // 模态遮挡：半透明深灰蓝遮罩压暗底层密集网格线，让弹窗干净浮出，并吞掉背景点击
        if self.block_background {
            let scrim = lerp_color(Color32::TRANSPARENT, pal.dialog_scrim, enter);
            egui::Area::new(id.with("block"))
                .order(egui::Order::Middle)
                .fixed_pos(screen.min)
                .show(ctx, |ui| {
                    let r = egui::Rect::from_min_size(screen.min, screen.size());
                    if scrim != Color32::TRANSPARENT {
                        ui.painter().rect_filled(r, 0.0, scrim);
                    }
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
        let slide_y = (1.0 - enter) * 3.0;
        let draw_pos = egui::pos2(
            pos.x,
            (pos.y + slide_y).min((screen.bottom() - 40.0).max(screen.top() + 4.0)),
        );
        let out = egui::Area::new(id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(draw_pos)
            .default_size([w, area_h])
            .show(ctx, |ui| {
                let high_contrast = pal.theme == crate::theme::AppTheme::HighContrast;
                let hairline = 1.0 / ctx.pixels_per_point();
                let radius = 8;
                let frame = egui::Frame::window(&ctx.style())
                    .fill(pal.dialog_bg)
                    .stroke(Stroke::new(if high_contrast { 1.0 } else { hairline }, pal.card_border))
                    .corner_radius(radius)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 6],
                        blur: 20,
                        spread: 0,
                        color: Color32::from_black_alpha(if high_contrast { 0 } else if pal.theme.is_dark() { 76 } else { 32 }),
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
                            trect,
                            egui::CornerRadius { nw: radius - 1, ne: radius - 1, sw: 0, se: 0 },
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
                            14.0,
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
                        let cresp = paint_close_button(ui, crect, cresp, "关闭");
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
                                // 复选框仅 14pt 宽；沿用全局 6pt 圆角会看起来像单选框。
                                let radius = egui::CornerRadius::same(crate::tokens::radius::MD as u8);
                                let widgets = &mut ui.visuals_mut().widgets;
                                widgets.inactive.corner_radius = radius;
                                widgets.hovered.corner_radius = radius;
                                widgets.active.corner_radius = radius;
                                widgets.open.corner_radius = radius;
                                // 标题与外边距留在视口内，未指定高度的对话框也可滚至底部。
                                let top_inset = ui.cursor().top() - trect.top();
                                let limit = (screen.height() - top_inset - 18.0 - 10.0).max(1.0);
                                let body_height = if h > 0.0 { h.min(limit) } else { limit };
                                ui.set_max_height(body_height);
                                egui::ScrollArea::vertical()
                                    .max_height(body_height)
                                    .min_scrolled_height(0.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        let r = add(ui);
                                        if let Some(fr) = ui.memory(|m| m.focused())
                                            .and_then(|fid| ui.ctx().read_response(fid))
                                        {
                                            if fr.gained_focus()
                                                && fr.layer_id == ui.layer_id()
                                                && ui.min_rect().intersects(fr.rect)
                                                && !ui.clip_rect().contains_rect(fr.rect)
                                            {
                                                ui.scroll_to_rect(fr.rect, Some(egui::Align::Center));
                                            }
                                        }
                                        r
                                    })
                                    .inner
                            })
                            .inner;
                        if self.resizable {
                            let bottom = ui.min_rect().bottom();
                            let gr = egui::Rect::from_min_size(
                                egui::pos2(trect.right() - 14.0, bottom - 14.0),
                                egui::vec2(14.0, 14.0),
                            );
                            let g = ui.interact(gr, id.with("grip"), drag_only_sense())
                                .on_hover_cursor(egui::CursorIcon::ResizeNwSe);
                            let gc = pal.text_weak;
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
            let opener = ctx.data_mut(|d| {
                d.remove::<u64>(id.with("shown"));
                d.remove_temp::<Option<egui::Id>>(id.with("focus_before")).flatten()
            });
            let focused = ctx.memory(|m| m.focused());
            let layer = out.response.layer_id;
            let owns_focus = focused.and_then(|f| ctx.read_response(f)).is_some_and(|r| r.layer_id == layer);
            if focused.is_none() || owns_focus || focused == opener {
                // A menu item can disappear as soon as it opens the dialog. Restoring
                // its ID would leave AccessKit pointing outside the current tree.
                let restore = opener.filter(|id| ctx.viewport(|v| {
                    v.this_pass.widgets.get(*id).is_some_and(|w| w.enabled && w.sense.is_focusable() && w.layer_id != layer)
                        && v.this_pass.accesskit_state.as_ref().is_none_or(|s| s.nodes.contains_key(id))
                }));
                if let Some(id) = restore {
                    ctx.memory_mut(|m| m.request_focus(id));
                } else if let Some(id) = focused {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
            }
        }
        ctx.data_mut(|d| {
            // Area 首帧只测量。等真实高度可用再保存初始锚点，并保留后续拖动的位置。
            if measured.is_some() {
                d.insert_temp(id.with("pos"), pos);
            }
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

/// 数字与同排字段对齐，过长的值显示省略号，完整值可悬停查看。
pub fn spin(ui: &mut egui::Ui, v: &mut f64, min: f64, max: f64, step: f64, width: f32) -> bool {
    use egui::accesskit::Action;
    let pal = crate::theme::palette();
    let height = ui.spacing().interact_size.y.max(26.0);
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let step_w = 20.0;
    let arrow_left = rect.right() - step_w;
    let a = Rect::from_min_max(egui::pos2(arrow_left, rect.top()), egui::pos2(rect.right(), rect.center().y));
    let b = Rect::from_min_max(a.left_bottom(), rect.right_bottom());
    let focused = resp.has_focus();
    ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, egui::EventFilter {
        vertical_arrows: true,
        ..Default::default()
    }));
    let mut ticks = ui.input_mut(|i| {
        let mut ticks = i.num_accesskit_action_requests(resp.id, Action::Increment) as i32
            - i.num_accesskit_action_requests(resp.id, Action::Decrement) as i32;
        if focused {
            ticks += i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) as i32
                - i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) as i32;
        }
        ticks
    });
    if resp.clicked_by(egui::PointerButton::Primary) {
        if let Some(pos) = resp.interact_pointer_pos() {
            if pos.x >= arrow_left {
                ticks += if pos.y < rect.center().y { 1 } else { -1 };
            }
        }
    }
    if resp.enabled() && ticks != 0 {
        let value = (*v + ticks as f64 * step).clamp(min, max);
        if value != *v {
            *v = value;
            resp.mark_changed();
        }
    }
    resp.widget_info(|| egui::WidgetInfo::drag_value(resp.enabled(), *v));
    let _ = ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_min_numeric_value(min);
        node.set_max_numeric_value(max);
        node.set_numeric_value_step(step);
        if resp.enabled() && *v < max { node.add_action(Action::Increment); }
        if resp.enabled() && *v > min { node.add_action(Action::Decrement); }
    });
    let spin_hov = anim_hover(ui.ctx(), resp.id.with("spin_hov"), resp.hovered() && !focused);
    let border_col = if focused {
        pal.accent
    } else {
        lerp_color(pal.card_border, pal.text_muted, spin_hov)
    };
    ui.painter().rect(
        rect,
        4.0,
        pal.grid_bg,
        Stroke::new(if focused { crate::tokens::stroke::FOCUS } else { 1.0 }, border_col),
        egui::StrokeKind::Inside,
    );
    let value_rect = Rect::from_min_max(rect.min, egui::pos2(arrow_left, rect.bottom()))
        .shrink2(Vec2::new(4.0, 0.0));
    let txt = egui::emath::format_with_decimals_in_range(*v, 0..=6);
    let mut job = egui::text::LayoutJob::simple_singleline(txt.clone(), FontId::proportional(T_BODY), pal.text);
    job.wrap = egui::text::TextWrapping::truncate_at_width(value_rect.width().max(1.0));
    let galley = ui.painter().layout_job(job);
    let elided = galley.elided;
    let pos = Align2::RIGHT_CENTER.anchor_size(value_rect.right_center(), galley.size()).min;
    ui.painter_at(value_rect).galley(pos, galley, pal.text);
    ui.painter().line_segment(
        [
            egui::pos2(rect.right() - step_w, rect.top() + 3.0),
            egui::pos2(rect.right() - step_w, rect.bottom() - 3.0),
        ],
        Stroke::new(1.0, pal.card_border),
    );
    let pointer = resp.hover_pos();
    for (r, up, enabled) in [(a, true, *v < max), (b, false, *v > min)] {
        let hv = enabled && pointer.is_some_and(|pos| r.contains(pos));
        let step_hov = anim_hover(ui.ctx(), resp.id.with(("spin_step", up)), hv);
        let fill = if hv && resp.is_pointer_button_down_on() {
            pal.pressed
        } else {
            lerp_color(Color32::TRANSPARENT, pal.hover, step_hov)
        };
        if fill != Color32::TRANSPARENT {
            ui.painter().rect_filled(r.shrink(1.0), 3.0, fill);
        }
        let c = r.center();
        let st = Stroke::new(
            1.3,
            if !enabled {
                disabled_color(pal)
            } else {
                lerp_color(pal.text_weak, pal.text, step_hov)
            },
        );
        let dy = if up { 1.5 } else { -1.5 };
        ui.painter().add(egui::Shape::line(vec![
            c + Vec2::new(-3.0, dy), c + Vec2::new(0.0, -dy), c + Vec2::new(3.0, dy),
        ], st));
    }
    ui.painter().line_segment(
        [egui::pos2(a.left() + 2.0, a.bottom()), egui::pos2(a.right() - 2.0, a.bottom())],
        Stroke::new(1.0, pal.card_border),
    );
    let changed = resp.changed();
    if elided {
        resp.on_hover_text(txt);
    }
    changed
}

/// 对话框标签页使用底色和字重标记当前页。
pub fn dialog_tabs(ui: &mut egui::Ui, sel: &mut u8, tabs: &[&str]) {
    if tabs.is_empty() {
        return;
    }
    let pal = crate::theme::palette();
    // 当前页共用一个稳定的 Tab 焦点，方向键换页时焦点仍在页签条内。
    let active_id = ui.next_auto_id().with("active_dialog_tab");
    if ui.memory(|m| m.has_focus(active_id)) {
        ui.memory_mut(|m| m.set_focus_lock_filter(active_id, egui::EventFilter {
            horizontal_arrows: true,
            ..Default::default()
        }));
        let n = tabs.len();
        *sel = ui.input_mut(|i| {
            let idx = usize::from(*sel).min(n - 1);
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                0
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                n - 1
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                (idx + n - 1) % n
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                (idx + 1) % n
            } else {
                idx
            }
        }) as u8;
    }
    let width = ui.available_width();
    let gap = crate::tokens::space::XS;
    // 每格向下取整，预留行末间隙，避免逐控件像素取整将末页签挤入下一行。
    let slot = (width / tabs.len() as f32 - gap).floor();
    let tab_h = 32.0;
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(gap, 0.0);
        for (i, t) in tabs.iter().enumerate() {
            let on = *sel as usize == i;
            let font = if on {
                medium_in(ui.ctx(), T_BODY)
            } else {
                FontId::proportional(T_BODY)
            };
            let galley = ui.painter().layout_no_wrap(
                (*t).to_owned(),
                font,
                Color32::PLACEHOLDER,
            );
            let w = slot.min(92.0).max(galley.size().x + 16.0);
            let (id, rect) = ui.allocate_space(Vec2::new(w, tab_h));
            let resp = if on {
                ui.interact(rect, active_id, Sense::click())
            } else {
                ui.interact(rect, id, Sense::click() - Sense::focusable_noninteractive())
            };
            let pos = Align2::CENTER_CENTER
                .anchor_size(rect.center(), galley.size())
                .min;
            let chip = rect.shrink2(Vec2::new(1.0, 3.0));
            let tab_hov = anim_hover(ui.ctx(), id.with(("dlgtab_hov", i)), !on && resp.hovered());
            if on {
                let (fill, ink) = active_dialog_tab_colors(pal);
                ui.painter().rect_filled(chip, 6.0, fill);
                ui.painter().galley_with_override_text_color(pos, galley, ink);
            } else {
                let bg = lerp_color(Color32::TRANSPARENT, pal.hover, tab_hov);
                if bg != Color32::TRANSPARENT {
                    ui.painter().rect_filled(chip, 6.0, bg);
                }
                ui.painter().galley_with_override_text_color(
                    pos,
                    galley,
                    lerp_color(pal.text_weak, pal.text, tab_hov),
                );
            }
            tab_focus_ring(ui, &resp, rect.shrink2(Vec2::new(1.0, 3.0)), 6.0, on);
            a11y_role(&resp, t, egui::accesskit::Role::Tab, Some(on));
            if resp.clicked() {
                clicked = Some(i as u8);
            }
        }
    });
    if let Some(index) = clicked {
        *sel = index;
        ui.memory_mut(|m| m.request_focus(active_id));
        ui.ctx().request_repaint();
    }
    ui.add_space(6.0);
}

/// 表单的一行：定宽字段名 + 内容
pub fn field(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(64.0, 22.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.label(RichText::new(label).size(12.0).color(crate::theme::palette().text_weak));
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
    fn text_input_padding_accepts_focus_and_typing_at_each_dpi() {
        for ppp in [1.0, 1.5, 2.0] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut buf = String::new();
            let mut rect = Rect::NOTHING;
            for frame in 0..4 {
                // 112pt 的视口扣除面板左右各 8pt，实际供字段使用 96pt。
                let mut input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(112.0, 140.0))),
                    ..Default::default()
                };
                input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(ppp);
                if frame == 1 || frame == 2 {
                    let pos = rect.left_bottom() + Vec2::new(2.0, -2.0);
                    input.events.push(egui::Event::PointerMoved(pos));
                    input.events.push(egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary,
                        pressed: frame == 1, modifiers: egui::Modifiers::NONE,
                    });
                } else if frame == 3 {
                    input.events.push(egui::Event::Text("张".to_owned()));
                }
                let out = ctx.run(input, |ctx| {
                    assert_eq!(ctx.pixels_per_point(), ppp);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response = text_input(ui, &mut buf, "提示");
                        rect = response.rect;
                        assert!((rect.height() - 34.0).abs() < 0.01, "{ppp}: {rect:?}");
                        assert!(rect.width() <= 96.01, "{ppp}: {rect:?}");
                        if frame >= 2 { assert!(response.has_focus()); }
                    });
                });
                if frame == 0 {
                    let tree = out.platform_output.accesskit_update.unwrap();
                    let edit = tree.nodes.iter().map(|(_, n)| n)
                        .find(|n| n.role() == egui::accesskit::Role::TextInput).unwrap();
                    let bounds = edit.bounds().unwrap();
                    assert_eq!([bounds.x0, bounds.y0, bounds.x1, bounds.y1],
                        [rect.left() as f64, rect.top() as f64, rect.right() as f64, rect.bottom() as f64]);
                }
            }
            assert_eq!(buf, "张");
        }
    }

    #[test]
    fn spin_elides_long_values_only_when_they_exceed_the_numeric_area() {
        let ctx = egui::Context::default();
        for (width, expected) in [(44.0, true), (110.0, false)] {
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    spin(ui, &mut 123456.0, 0.0, 1e6, 1.0, width);
                });
            });
            let value = out.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(t) if t.galley.text() == "123456" => Some(&t.galley),
                _ => None,
            }).expect("numeric value must be painted");
            assert_eq!(value.elided, expected, "width {width}");
        }
    }

    #[test]
    fn closing_dialog_keeps_accesskit_focus_on_a_current_widget() {
        let _env = crate::test_env::TestEnv::new("dialog-focus-restoration");
        for focus_elsewhere in [false, true] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut open = true;
            let mut outside = None;
            for closing in [false, true] {
                let frame = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        if !closing {
                            ui.button("选项…").request_focus();
                        } else if focus_elsewhere {
                            let response = ui.push_id("other tool", |ui| ui.button("另一个工具")).inner;
                            response.request_focus();
                            outside = Some(response.id);
                        }
                    });
                    window(ctx, "CELLAN 选项", 360.0).open(&mut open).show(ctx, |ui| {
                        let done = ui.button("完成");
                        if closing {
                            if !focus_elsewhere { done.request_focus(); }
                            open = false;
                        }
                    });
                });
                if closing {
                    assert!(!open);
                    let tree = frame.platform_output.accesskit_update.unwrap();
                    assert!(tree.nodes.iter().any(|(id, _)| *id == tree.focus));
                    assert_eq!(ctx.memory(|m| m.focused()), outside);
                }
            }
        }
    }

    #[test]
    fn dialog_tabs_arrow_keys_switch_pages_and_tab_reaches_the_form() {
        let ctx = egui::Context::default();
        let mut selected = 1;
        let mut field = String::new();
        for (frame, key) in [None, None, Some(egui::Key::ArrowRight), Some(egui::Key::ArrowRight),
            Some(egui::Key::End), Some(egui::Key::Home), Some(egui::Key::Tab)].into_iter().enumerate()
        {
            let mut input = egui::RawInput::default();
            if let Some(key) = key {
                input.events.push(egui::Event::Key {
                    key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                });
            }
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let active_id = ui.next_auto_id().with("active_dialog_tab");
                    dialog_tabs(ui, &mut selected, &["数字", "对齐", "字体"]);
                    let edit = ui.text_edit_singleline(&mut field);
                    if frame == 0 { ui.memory_mut(|m| m.request_focus(active_id)); }
                    if key == Some(egui::Key::Tab) { assert!(edit.has_focus()); }
                });
            });
            assert_eq!(selected, [1, 1, 2, 0, 2, 0, 0][frame], "{key:?}");
        }
    }

    #[test]
    fn format_tabs_fit_one_row_at_dialog_width() {
        let ctx = egui::Context::default();
        for ppp in [1.0, 1.5, 2.0] {
            ctx.set_pixels_per_point(ppp);
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_width(520.0);
                    let strip = ui.scope(|ui| {
                        dialog_tabs(ui, &mut 0, &["数字", "对齐", "字体", "边框", "填充", "保护"]);
                    });
                    assert!(strip.response.rect.height() < 45.0, "{ppp}: {:?}", strip.response.rect);
                });
            });
        }
    }

    #[test]
    fn unsized_dialog_footer_is_reachable_on_a_short_viewport() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(760.0, 420.0));
        let mut initially_clipped = false;
        let mut reached = false;
        for frame in 0..30 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(frame as f64 / 30.0),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                let dlg = window(ctx, "格式测试", 560.0).show(ctx, |ui| {
                    for i in 0..20 {
                        let _ = ui.button(format!("选项 {i}"));
                    }
                    let last = secondary_button(ui, "取消");
                    let visible = ui.clip_rect().intersect(screen).contains_rect(last.rect);
                    if frame == 2 {
                        initially_clipped = !visible;
                        last.scroll_to_me(Some(egui::Align::Max));
                    } else if frame > 2 {
                        reached |= visible;
                    }
                }).unwrap();
                if frame > 1 {
                    assert!(screen.contains_rect(dlg.response.rect), "{:?}", dlg.response.rect);
                }
            });
        }
        assert!(initially_clipped && reached);
    }

    #[test]
    fn short_dialog_footer_stays_next_to_its_content() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1024.0, 768.0));
        for frame in 0..3 {
            let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                let dlg = window(ctx, "确认", 360.0).show(ctx, |ui| {
                    let text = ui.label("保存更改？").rect;
                    footer(ui, |ui| {
                        let cancel = secondary_button(ui, "取消").rect;
                        assert!(cancel.top() - text.bottom() < 48.0);
                    });
                }).unwrap();
                if frame > 0 {
                    assert!(dlg.response.rect.height() < 210.0);
                }
            });
        }
    }

    #[test]
    fn spin_steps_only_the_focused_row_and_respects_its_limit() {
        let ctx = egui::Context::default();
        let mut values = [1.0, 1.0];
        for frame in 0..4 {
            let mut input = egui::RawInput::default();
            if frame >= 2 {
                input.events.push(egui::Event::Key {
                    key: egui::Key::ArrowUp,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    assert!(!spin(ui, &mut values[0], 0.0, 2.0, 1.0, 64.0));
                    let id = ui.next_auto_id();
                    let changed = spin(ui, &mut values[1], 0.0, 2.0, 1.0, 64.0);
                    if frame == 0 {
                        ui.memory_mut(|m| m.request_focus(id));
                    }
                    assert_eq!(changed, frame == 2);
                });
            });
        }
        assert_eq!(values, [1.0, 2.0]);
    }

    #[test]
    fn disabled_has_no_background() {
        assert_eq!(state_bg(false, false, true, true, false), Color32::TRANSPARENT);
        assert_eq!(state_bg(false, true, true, true, true), Color32::TRANSPARENT);
    }

    #[test]
    fn pressed_beats_hover_and_checked_beats_hover() {
        let light = crate::theme::palette_for(crate::theme::AppTheme::Light);
        assert_eq!(state_bg(true, false, true, true, false), light.pressed);
        assert_eq!(state_bg(true, true, true, false, false), light.pressed);
        assert_eq!(state_bg(true, false, true, false, false), light.hover);
        assert_eq!(state_bg(true, false, false, false, false), Color32::TRANSPARENT);
    }

    #[test]
    fn text_toggle_renders_real_bold_and_italic_without_double_draw_ghosting() {
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.horizontal(|ui| {
                    text_toggle(ui, "B", false, "", true, false, false);
                    text_toggle(ui, "I", false, "", false, true, false);
                    text_toggle(ui, "U", false, "", false, false, true);
                });
            });
        });
        let b_shapes = out
            .shapes
            .iter()
            .filter(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == "B"))
            .count();
        assert_eq!(b_shapes, 1, "Bold 'B' must render in a single pass without 0.55px double-draw");
        // Without ui-italic installed, Italic 'I' renders a crisp 3-segment vector italic glyph
        let i_text = out
            .shapes
            .iter()
            .filter(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == "I"))
            .count();
        assert_eq!(i_text, 0, "Without ui-italic font, 'I' should draw vector italic segments");
        let lines = out
            .shapes
            .iter()
            .filter(|s| matches!(&s.shape, egui::Shape::LineSegment { .. }))
            .count();
        assert!(lines >= 4, "Expected 3 italic 'I' vector segments + 1 underline segment, got {lines}");
    }

    #[test]
    fn brand_logo_renders_across_sizes_and_themes() {
        for dark in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let p = ui.painter();
                    for side in [0.0, 4.0, 15.0, 18.0, 20.0, 36.0, 128.0] {
                        let r = Rect::from_min_size(egui::pos2(10.0, 10.0), Vec2::splat(side));
                        draw_brand_logo(p, r);
                    }
                });
            });
            assert!(!out.shapes.is_empty(), "brand logo should emit vector shapes");
        }
    }

    #[test]
    fn lerp_color_preserves_endpoints_and_transparent_rgb() {
        let target = Color32::from_rgb(238, 242, 247);
        let accent = Color32::from_rgb(33, 115, 70);
        // Exact endpoint equality
        assert_eq!(lerp_color(Color32::TRANSPARENT, target, 0.0), Color32::TRANSPARENT);
        assert_eq!(lerp_color(Color32::TRANSPARENT, target, 0.0005), Color32::TRANSPARENT);
        assert_eq!(lerp_color(Color32::TRANSPARENT, target, 1.0), target);
        assert_eq!(lerp_color(Color32::TRANSPARENT, target, 0.9995), target);
        assert_eq!(lerp_color(target, accent, 0.0), target);
        assert_eq!(lerp_color(target, accent, 1.0), accent);

        // Alpha-safe fade from TRANSPARENT keeps target RGB (never blends through black)
        let mid_in = lerp_color(Color32::TRANSPARENT, target, 0.5);
        let [r, g, b, a] = mid_in.to_srgba_unmultiplied();
        assert!((120..=135).contains(&a), "expected ~50% alpha, got {a}");
        assert!(
            r.abs_diff(target.r()) <= 2 && g.abs_diff(target.g()) <= 2 && b.abs_diff(target.b()) <= 2,
            "TRANSPARENT -> target must preserve target RGB: got ({r},{g},{b}) vs {:?}",
            target
        );

        // Alpha-safe fade to TRANSPARENT keeps source RGB
        let mid_out = lerp_color(accent, Color32::TRANSPARENT, 0.5);
        let [r2, g2, b2, a2] = mid_out.to_srgba_unmultiplied();
        assert!((120..=135).contains(&a2), "expected ~50% alpha, got {a2}");
        assert!(
            r2.abs_diff(accent.r()) <= 2 && g2.abs_diff(accent.g()) <= 2 && b2.abs_diff(accent.b()) <= 2,
            "source -> TRANSPARENT must preserve source RGB: got ({r2},{g2},{b2}) vs {:?}",
            accent
        );
    }

    #[test]
    fn anim_hover_and_toggle_reach_exact_endpoints_and_high_contrast_is_instant() {
        let orig = crate::theme::current_theme();
        crate::theme::set_current_theme(crate::theme::AppTheme::Light);

        let ctx = egui::Context::default();
        let hov_id = egui::Id::new("test_anim_hover");
        let tog_id = egui::Id::new("test_anim_toggle");
        let run_step = |t: f64, active: bool| -> (f32, f32) {
            let mut hv = 0.0;
            let mut tg = 0.0;
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(t),
                    ..Default::default()
                },
                |ctx| {
                    hv = anim_hover(ctx, hov_id, active);
                    tg = anim_toggle(ctx, tog_id, active);
                },
            );
            (hv, tg)
        };

        // Initialized at rest -> exact 0.0
        assert_eq!(run_step(1.0, false), (0.0, 0.0));
        // Mid-transition at +50ms -> strictly between 0 and 1
        let (hv_mid, tg_mid) = run_step(1.05, true);
        assert!(hv_mid > 0.0 && hv_mid < 1.0, "hover mid={hv_mid}");
        assert!(tg_mid > 0.0 && tg_mid < 1.0, "toggle mid={tg_mid}");
        // Settled well past 140ms -> exact 1.0
        assert_eq!(run_step(1.30, true), (1.0, 1.0));
        // Fade back out -> settles to exact 0.0
        let (hv_out, tg_out) = run_step(1.35, false);
        assert!(hv_out > 0.0 && hv_out < 1.0, "hover out={hv_out}");
        assert!(tg_out > 0.0 && tg_out < 1.0, "toggle out={tg_out}");
        assert_eq!(run_step(1.60, false), (0.0, 0.0));

        // HighContrast snaps immediately without muddy intermediate alpha
        crate::theme::set_current_theme(crate::theme::AppTheme::HighContrast);
        assert_eq!(run_step(1.61, true), (1.0, 1.0));
        assert_eq!(run_step(1.62, false), (0.0, 0.0));

        crate::theme::set_current_theme(orig);
    }

    #[test]
    fn anim_enter_settles_cleanly_and_snaps_on_static_frames() {
        let orig = crate::theme::current_theme();
        crate::theme::set_current_theme(crate::theme::AppTheme::Light);

        // 1. Static single-frame harness (`RawInput::default()`, pass == 0) snaps immediately to 1.0
        let static_ctx = egui::Context::default();
        let mut static_val = 0.0;
        let _ = static_ctx.run(egui::RawInput::default(), |ctx| {
            static_val = anim_enter(ctx, egui::Id::new("static_enter"));
        });
        assert_eq!(static_val, 1.0, "static single-frame test must land crisp at 1.0");

        // 2. Real advancing time (opened after boot pass 0) eases smoothly 0.0 -> (0..1) -> 1.0 and stops repainting
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput { time: Some(1.0), ..Default::default() }, |_| {});
        let eid = egui::Id::new("live_enter");
        let step = |t: f64| -> (f32, bool) {
            let mut val = 0.0;
            let out = ctx.run(
                egui::RawInput {
                    time: Some(t),
                    ..Default::default()
                },
                |ctx| {
                    val = anim_enter(ctx, eid);
                },
            );
            let repaint = out.viewport_output[&egui::ViewportId::ROOT].repaint_delay.is_zero();
            (val, repaint)
        };
        let (v0, rep0) = step(2.0);
        assert_eq!(v0, 0.0);
        assert!(rep0, "opening frame should request repaint to animate");
        let (v1, rep1) = step(2.04);
        assert!(v1 > 0.0 && v1 < 1.0, "mid entrance should ease smoothly: {v1}");
        assert!(rep1, "mid entrance should continue repainting");
        let (v2, _) = step(2.20);
        assert_eq!(v2, 1.0, "entrance must settle at exact 1.0");
        // Drain egui's single trailing pass from prior request_repaint(), then verify 0 idle repaints.
        let (v3, _) = step(2.25);
        assert_eq!(v3, 1.0);
        let (v4, rep4) = step(2.30);
        assert_eq!(v4, 1.0);
        assert!(!rep4, "settled entrance must not request idle repaints");

        // 3. Frozen clock (`dt == 0.0`) settles cleanly to 1.0
        let fid = egui::Id::new("frozen_enter");
        let _ = ctx.run(egui::RawInput { time: Some(3.0), ..Default::default() }, |ctx| {
            let _ = anim_enter(ctx, fid);
        });
        let mut frozen_val = 0.0;
        let _ = ctx.run(egui::RawInput { time: Some(3.0), ..Default::default() }, |ctx| {
            frozen_val = anim_enter(ctx, fid);
        });
        assert_eq!(frozen_val, 1.0, "zero-dt repeated frame must settle to 1.0");

        // 4. HighContrast always lands immediately at 1.0
        crate::theme::set_current_theme(crate::theme::AppTheme::HighContrast);
        let (vhc, rephc) = step(5.0);
        assert_eq!(vhc, 1.0);
        assert!(!rephc);

        crate::theme::set_current_theme(orig);
    }
}
