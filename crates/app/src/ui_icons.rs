//! 界面图标：24×24 网格上自己画，按 Excel（Fluent）的色块风格。
//!
//! 为什么自绘：随包可分发的授权要干净（不引图标字体、不抄素材），
//! 而且 egui 默认字体里没有这些符号，用文字硬凑在不同字体下会歪。
//!
//! 两套渲染共用一个画法：
//! - `draw_excel`：彩色模式。图标的每一块都有填充 + 深色描边 + 顶部高光，
//!   和 Excel 自带的彩色图标同一路数。
//! - `draw`：单色模式（状态栏等）。只画描边，颜色统一成调用方给的颜色。

use eframe::egui::{self, Align2, Color32, FontId, Painter, Pos2, Rect, Stroke};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Open,
    Save,
    SaveAs,
    Print,
    Undo,
    Redo,
    Paste,
    Cut,
    Copy,
    Brush,
    Fill,
    FontColor,
    AlignL,
    AlignC,
    AlignR,
    Top,
    Middle,
    Bottom,
    Wrap,
    Merge,
    Border,
    Currency,
    Percent,
    Comma,
    SortAsc,
    SortDesc,
    Filter,
    Find,
    Chart,
    Image,
    Table,
    Comment,
    Pivot,
    Link,
    Revision,
    Zoom,
    Calc,
    Sheet,
    Share,
    Fx,
    Sum,
    Grid,
    Close,
    Plus,
    Refresh,
    DecInc,
    DecDec,
    FontGrow,
    FontShrink,
    TextDir,
    HeaderFooter,
    DocLink,
    Freeze,
    Lock,
    /// 条件格式：三条彩色数据条
    Cf,
    /// 橡皮擦（清除）
    Eraser,
    /// 清除格式：A + 橡皮
    ClearFmt,
    /// 增加缩进
    IndentInc,
    /// 减少缩进
    IndentDec,
    /// 符号（Ω）
    Symbol,
    /// 简体（「简」字图标）
    ZhS,
    /// 繁体（「繁」字图标）
    ZhT,
    /// 追踪引用单元格：方块 + 指向它的箭头
    TraceIn,
    /// 追踪从属单元格：方块 + 离开它的箭头
    TraceOut,
    /// 迷你图-折线
    SparkLine,
    /// 迷你图-柱形
    SparkColumn,
    /// 迷你图-盈亏
    SparkLoss,
    /// 文本框：带文字线的方框 + 角上的小 T
    TextBox,
}

// ---------------------------------------------------------------- 色板
// Office 主题色 + 各自的深色描边。Excel 的图标就是这套配色。
const WHITE: Color32 = Color32::from_rgb(255, 255, 255);
const BLUE: Color32 = Color32::from_rgb(68, 114, 196);
const BLUE_D: Color32 = Color32::from_rgb(47, 85, 151);
const BLUE_M_D: Color32 = Color32::from_rgb(91, 155, 213);
/// 图标线条用的柔蓝（比品牌蓝低饱和）
const BLUE_S: Color32 = Color32::from_rgb(91, 127, 166);
const BLUE_L: Color32 = Color32::from_rgb(222, 235, 247);
const BLUE_XL: Color32 = Color32::from_rgb(189, 215, 238);
const ORANGE: Color32 = Color32::from_rgb(237, 125, 49);
const ORANGE_D: Color32 = Color32::from_rgb(197, 90, 17);
const GOLD: Color32 = Color32::from_rgb(255, 192, 0);
const GOLD_D: Color32 = Color32::from_rgb(191, 144, 0);
const GREEN: Color32 = Color32::from_rgb(112, 173, 71);
const GREEN_D: Color32 = Color32::from_rgb(84, 130, 53);
const GREEN_L: Color32 = Color32::from_rgb(226, 239, 218);
const RED: Color32 = Color32::from_rgb(192, 0, 0);
const BROWN: Color32 = Color32::from_rgb(231, 201, 160);
const BROWN_M: Color32 = Color32::from_rgb(192, 154, 106);
const BROWN_D: Color32 = Color32::from_rgb(166, 112, 58);
const CREAM: Color32 = Color32::from_rgb(255, 242, 204);
const GRAY: Color32 = Color32::from_rgb(165, 165, 165);
const GRAY_D: Color32 = Color32::from_rgb(89, 89, 89);
/// 文字级深灰（对齐类图标的横线）
const INK: Color32 = Color32::from_rgb(58, 58, 58);

fn lighten(c: Color32, f: f32) -> Color32 {
    let l = |v: u8| (v as f32 + (255.0 - v as f32) * f).min(255.0) as u8;
    Color32::from_rgb(l(c.r()), l(c.g()), l(c.b()))
}

// ---------------------------------------------------------------- 画笔
struct Pen<'a> {
    p: &'a Painter,
    r: Rect,
    s: f32,
    /// 标准线宽（像素）
    w: f32,
    /// Some(颜色) = 单色模式：不铺填充，所有线条都用这个颜色
    mono: Option<Color32>,
    /// 设备像素比：图标要按物理像素对齐，否则 1px 线会跨两像素发虚
    ppp: f32,
}

impl Pen<'_> {
    /// 填充块：边界对齐到整像素
    fn snap(&self, v: f32) -> f32 {
        (v * self.ppp).round() / self.ppp
    }
    /// 线条：中心对齐到半像素（1px 线才落在单个像素上）
    fn snapl(&self, v: f32) -> f32 {
        ((v * self.ppp - 0.5).round() + 0.5) / self.ppp
    }
    fn xy(&self, x: f32, y: f32) -> Pos2 {
        Pos2::new(
            self.snap(self.r.left() + x * self.s),
            self.snap(self.r.top() + y * self.s),
        )
    }
    /// 线条端点用半像素对齐
    fn xyl(&self, x: f32, y: f32) -> Pos2 {
        Pos2::new(
            self.snapl(self.r.left() + x * self.s),
            self.snapl(self.r.top() + y * self.s),
        )
    }
    fn px(&self, v: f32) -> f32 {
        v * self.s
    }
    fn col(&self, c: Color32) -> Color32 {
        self.mono.unwrap_or(c)
    }
    fn filled(&self) -> bool {
        self.mono.is_none()
    }
    fn rect_of(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_max(self.xy(x, y), self.xy(x + w, y + h))
    }

    /// 圆角填充（单色模式下跳过）
    fn rr(&self, x: f32, y: f32, w: f32, h: f32, rad: f32, fill: Color32) {
        if !self.filled() {
            return;
        }
        self.p
            .rect_filled(self.rect_of(x, y, w, h), self.px(rad), fill);
    }

    /// 圆角填充 + 描边（单色模式只描边）
    fn rrs(
        &self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        rad: f32,
        fill: Color32,
        stroke: Color32,
        sw: f32,
    ) {
        let rect = self.rect_of(x, y, w, h);
        let cr = self.px(rad);
        if self.filled() {
            self.p.rect_filled(rect, cr, fill);
            // 顶部一道浅色高光：和 Excel 图标一样有光源方向，块面才不呆
            if h * self.s >= 10.0 {
                let hl = Rect::from_min_max(
                    egui::pos2(rect.left() + cr + 0.6, rect.top() + 1.0),
                    egui::pos2(rect.right() - cr - 0.6, rect.top() + 2.0),
                );
                self.p.rect_filled(hl, 0.5, lighten(fill, 0.4));
            }
        }
        self.p.rect_stroke(
            rect,
            cr,
            Stroke::new((sw * self.s * 0.82).max(0.9), self.col(stroke)),
            egui::StrokeKind::Inside,
        );
    }

    /// 只描边的圆角矩形（Excel 的图标主体很多是轮廓）
    fn rro(&self, x: f32, y: f32, w: f32, h: f32, rad: f32, stroke: Color32, sw: f32) {
        self.p.rect_stroke(
            self.rect_of(x, y, w, h),
            self.px(rad),
            Stroke::new((sw * self.s * 0.82).max(0.9), self.col(stroke)),
            egui::StrokeKind::Inside,
        );
    }

    fn ln(&self, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, c: Color32) {
        self.p.line_segment(
            [self.xyl(x1, y1), self.xyl(x2, y2)],
            Stroke::new((w * self.s * 0.82).max(0.9), self.col(c)),
        );
    }

    /// 实心矩形条；单色模式下退化成一条线（否则状态栏图标会整块消失）
    fn bar(&self, x: f32, y: f32, w: f32, h: f32, c: Color32) {
        if !self.filled() {
            if w >= h {
                self.ln(x, y + h / 2.0, x + w, y + h / 2.0, 1.2, c);
            } else {
                self.ln(x + w / 2.0, y, x + w / 2.0, y + h, 1.2, c);
            }
            return;
        }
        self.p.rect_filled(self.rect_of(x, y, w, h), 0.0, c);
    }

    fn dot(&self, x: f32, y: f32, rad: f32, c: Color32) {
        if !self.filled() {
            // 单色模式画成空心圆，轮廓还认得出来
            self.ring(x, y, rad, 1.2, c);
            return;
        }
        self.p.circle_filled(self.xy(x, y), self.px(rad), c);
    }

    fn ring(&self, x: f32, y: f32, rad: f32, w: f32, c: Color32) {
        self.p.circle_stroke(
            self.xyl(x, y),
            self.px(rad),
            Stroke::new((w * self.s * 0.82).max(0.9), self.col(c)),
        );
    }

    fn poly(&self, pts: &[(f32, f32)], fill: Color32, stroke: Color32, sw: f32) {
        let v: Vec<Pos2> = pts.iter().map(|q| self.xy(q.0, q.1)).collect();
        if self.filled() {
            self.p
                .add(egui::Shape::convex_polygon(v.clone(), fill, Stroke::NONE));
        }
        let mut closed = v;
        closed.push(closed[0]);
        self.p.add(egui::Shape::line(
            closed,
            Stroke::new((sw * self.s * 0.82).max(0.9), self.col(stroke)),
        ));
    }

    fn polyline(&self, pts: &[(f32, f32)], w: f32, c: Color32) {
        let v: Vec<Pos2> = pts.iter().map(|q| self.xyl(q.0, q.1)).collect();
        self.p.add(egui::Shape::line(
            v,
            Stroke::new((w * self.s * 0.82).max(0.9), self.col(c)),
        ));
    }

    fn text(&self, s: &str, size: f32, x: f32, y: f32, c: Color32) {
        self.p.text(
            self.xy(x, y),
            Align2::CENTER_CENTER,
            s,
            FontId::proportional(size * self.s),
            self.col(c),
        );
    }

    /// 实心三角箭头：tip 是尖端，dir 指向射方向，size 是长度
    fn arrow(&self, tip: (f32, f32), dir: (f32, f32), size: f32, c: Color32) {
        if !self.filled() {
            return;
        }
        let (dx, dy) = dir;
        let n = (dx * dx + dy * dy).sqrt().max(0.001);
        let (ux, uy) = (dx / n, dy / n);
        let (px, py) = (-uy, ux);
        self.p.add(egui::Shape::convex_polygon(
            vec![
                self.xy(tip.0, tip.1),
                self.xy(
                    tip.0 - ux * size + px * size * 0.62,
                    tip.1 - uy * size + py * size * 0.62,
                ),
                self.xy(
                    tip.0 - ux * size - px * size * 0.62,
                    tip.1 - uy * size - py * size * 0.62,
                ),
            ],
            c,
            Stroke::NONE,
        ));
    }

    /// 一条"文字线"（对齐图标里的横条）
    fn rule(&self, x: f32, y: f32, len: f32, c: Color32) {
        self.bar(x, y, len, 1.7, c);
    }
}

/// 图标里那一格浅色网格（迷你图按钮上都有）
fn grid(pen: &Pen<'_>, x: f32, y: f32, w: f32, h: f32) {
    pen.ln(x + w / 3.0, y, x + w / 3.0, y + h, 0.6, BLUE_XL);
    pen.ln(x + w * 2.0 / 3.0, y, x + w * 2.0 / 3.0, y + h, 0.6, BLUE_XL);
    pen.ln(x, y + h / 2.0, x + w, y + h / 2.0, 0.6, BLUE_XL);
}

// ---------------------------------------------------------------- 入口
pub fn draw(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    paint(p, rect, icon, Some(color));
}

/// 图标总表：给「对照表」和自检用（位图 vs 矢量并排看）
pub const SHEET_ICONS: [Icon; 46] = [
    Icon::AlignC, Icon::AlignL, Icon::AlignR, Icon::Border, Icon::Bottom, Icon::Brush,
    Icon::Cf, Icon::Chart, Icon::ClearFmt, Icon::Close, Icon::Comma, Icon::Comment,
    Icon::Copy, Icon::Currency, Icon::Cut, Icon::Eraser, Icon::Fill, Icon::Filter,
    Icon::Find, Icon::FontColor, Icon::FontGrow, Icon::FontShrink, Icon::Freeze, Icon::Grid,
    Icon::Image, Icon::IndentDec, Icon::IndentInc, Icon::Link, Icon::Lock, Icon::Merge,
    Icon::Middle, Icon::Open, Icon::Paste, Icon::Percent, Icon::Pivot, Icon::Print,
    Icon::Refresh, Icon::SaveAs, Icon::SortAsc, Icon::SortDesc, Icon::Sum, Icon::Symbol,
    Icon::Table, Icon::Top, Icon::Wrap, Icon::Zoom,
];

/// 对照表用：只画矢量（不回落位图）
pub fn draw_vec(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let r = rect.expand(rect.width() * 0.04);
    paint(p, r, icon, Some(color));
}

/// 对照表用：矢量彩色版（和正式界面一致）
pub fn draw_vec_color(p: &Painter, rect: Rect, icon: Icon) {
    let r = rect.expand(rect.width() * 0.04);
    paint(p, r, icon, None);
}

/// 图标来源：默认矢量。imageMso 导出的是 Office 2010 时代的彩色渐变图，
/// 和 Excel 365 的扁平线面风格差得远；SC_ICONS=bitmap 可切回位图做对照。
fn use_bitmap() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("SC_ICONS")
            .map(|v| v.eq_ignore_ascii_case("bitmap"))
            .unwrap_or(false)
    })
}

/// Excel 风格：彩色块面 + 深色描边。关掉的按钮只画灰线，不铺色。
pub fn draw_excel(p: &Painter, rect: Rect, icon: Icon, color: Color32, enabled: bool) {
    if use_bitmap() && crate::ui_png::draw(p, rect, icon, enabled) {
        return;
    }
    // Excel 的图形比 24 网格画得满：外扩 4%，32px 时约多 2.5px
    let r = rect.expand(rect.width() * 0.04);
    paint(p, r, icon, if enabled { None } else { Some(color) });
}

fn paint(p: &Painter, rect: Rect, icon: Icon, mono: Option<Color32>) {
    let s = rect.width() / 24.0;
    let pen = Pen {
        p,
        r: rect,
        s,
        w: (1.6 * s).max(1.0),
        mono,
        ppp: p.pixels_per_point().max(1.0),
    };
    draw_one(&pen, icon);
}

// ---------------------------------------------------------------- 图标本体
fn draw_one(pen: &Pen<'_>, icon: Icon) {
    use Icon::*;
    match icon {
        // ---- 剪贴板 ----
        Paste => {
            // Excel 365：米色小板 + 白纸，线条细、体量克制（不要压过旁边的剪切/复制）
            pen.rr(5.4, 4.6, 13.2, 16.0, 1.2, Color32::from_rgb(250, 244, 233));
            pen.rro(5.4, 4.6, 13.2, 16.0, 1.2, INK, 1.1);
            pen.rr(7.8, 7.4, 8.4, 10.6, 0.5, WHITE);
            pen.rro(7.8, 7.4, 8.4, 10.6, 0.5, GRAY_D, 0.8);
            pen.ln(9.6, 10.4, 14.2, 10.4, 0.8, GRAY_D);
            pen.ln(9.6, 12.6, 14.2, 12.6, 0.8, GRAY_D);
            pen.rr(9.6, 2.8, 4.8, 3.6, 0.9, Color32::from_rgb(250, 244, 233));
            pen.rro(9.6, 2.8, 4.8, 3.6, 0.9, INK, 0.95);
            pen.ln(10.9, 2.8, 10.9, 6.4, 0.75, GRAY_D);
        }
        Cut => {
            // 细刀刃 + 两个空心环，低饱和蓝；开合角度按 Excel 的 40° 左右
            pen.ln(7.6, 4.4, 15.2, 14.8, 1.4, INK);
            pen.ln(16.4, 4.4, 8.8, 14.8, 1.4, INK);
            pen.ring(6.2, 18.0, 2.35, 1.35, INK);
            pen.ring(17.8, 18.0, 2.35, 1.35, INK);
        }
        Copy => {
            // 两张错位纸张：后页浅灰蓝、前页白，前面那页带折角
            pen.rr(8.6, 3.8, 10.6, 12.4, 0.5, Color32::from_rgb(234, 241, 250));
            pen.rro(8.6, 3.8, 10.6, 12.4, 0.5, GRAY_D, 0.95);
            pen.rr(4.0, 7.4, 10.6, 12.4, 0.5, WHITE);
            pen.rro(4.0, 7.4, 10.6, 12.4, 0.5, INK, 1.05);
            pen.poly(
                &[(11.4, 7.4), (14.6, 7.4), (14.6, 10.6)],
                Color32::from_rgb(234, 241, 250),
                INK,
                0.9,
            );
            pen.ln(11.4, 7.4, 14.6, 10.6, 0.8, INK);
        }
        Brush => {
            // Excel 365 的格式刷：斜握，刷头浅色、杆深灰，只用一点暖色区分刷毛
            pen.poly(
                &[(4.6, 5.6), (10.2, 3.4), (12.2, 8.8), (6.6, 11.0)],
                Color32::from_rgb(255, 242, 204),
                INK,
                1.1,
            );
            pen.ln(6.4, 8.4, 8.6, 7.6, 0.8, GRAY_D);
            pen.ln(7.4, 10.6, 9.6, 9.8, 0.8, GRAY_D);
            pen.ln(11.8, 9.4, 15.8, 17.0, 1.9, INK);
            pen.rr(14.2, 16.4, 4.2, 2.0, 0.9, INK);
        }
        Fill => {
            // 正面油漆桶：桶身 + 上沿 + 提手 + 一滴漆（斜放的四边形会被看成菱形）
            pen.rrs(4.6, 9.2, 11.6, 9.6, 1.0, GOLD, GOLD_D, 1.1);
            pen.bar(4.6, 9.2, 11.6, 2.4, GOLD_D);
            pen.polyline(&[(7.6, 9.2), (10.4, 4.6), (13.2, 9.2)], 1.3, GOLD_D);
            pen.poly(
                &[(19.0, 12.6), (21.4, 18.8), (16.6, 18.8)],
                GOLD,
                GOLD_D,
                0.9,
            );
        }
        FontColor => {
            pen.text("A", 13.0, 12.0, 9.6, INK);
            pen.rr(5.0, 18.6, 14.0, 3.0, 1.0, RED);
        }

        // ---- 对齐 ----
        AlignL => {
            let lens = [16.0, 10.0, 14.0, 8.0];
            for (i, l) in lens.iter().enumerate() {
                pen.rule(4.0, 5.2 + i as f32 * 4.4, *l, INK);
            }
        }
        AlignC => {
            let lens = [16.0, 10.0, 14.0, 8.0];
            for (i, l) in lens.iter().enumerate() {
                pen.rule(12.0 - l / 2.0, 5.2 + i as f32 * 4.4, *l, INK);
            }
        }
        AlignR => {
            let lens = [16.0, 10.0, 14.0, 8.0];
            for (i, l) in lens.iter().enumerate() {
                pen.rule(20.0 - l, 5.2 + i as f32 * 4.4, *l, INK);
            }
        }
        Top => {
            for i in 0..4 {
                pen.rule(4.0, 6.6 + i as f32 * 3.5, 8.6, GRAY_D);
            }
            pen.bar(4.0, 3.4, 16.0, 2.0, INK);
            pen.bar(17.2, 7.0, 1.7, 6.4, INK);
            pen.arrow((17.9, 5.4), (0.0, -1.0), 3.6, INK);
        }
        Middle => {
            for i in 0..4 {
                pen.rule(4.0, 6.6 + i as f32 * 3.5, 8.6, GRAY_D);
            }
            pen.bar(4.0, 11.0, 16.0, 2.0, INK);
            pen.arrow((19.6, 9.6), (0.0, -1.0), 3.2, INK);
            pen.arrow((19.6, 14.4), (0.0, 1.0), 3.2, INK);
            pen.bar(18.6, 10.2, 1.6, 3.6, INK);
        }
        Bottom => {
            for i in 0..4 {
                pen.rule(4.0, 6.6 + i as f32 * 3.5, 8.6, GRAY_D);
            }
            pen.bar(4.0, 18.6, 16.0, 2.0, INK);
            pen.bar(17.2, 10.6, 1.7, 6.4, INK);
            pen.arrow((17.9, 18.2), (0.0, 1.0), 3.6, INK);
        }
        Wrap => {
            pen.bar(4.0, 5.0, 16.0, 1.8, INK);
            pen.bar(4.0, 10.4, 9.6, 1.8, INK);
            pen.bar(4.0, 15.8, 16.0, 1.8, INK);
            pen.polyline(&[(13.6, 9.2), (18.0, 12.4), (13.6, 15.6)], 1.8, INK);
        }
        Merge => {
            // 一个大格 + 中间虚线 + 两侧向内箭头 + 底部居中短横（表示「合并后居中」）
            pen.rro(3.4, 4.2, 17.2, 11.6, 0.8, INK, 1.2);
            for (k, y) in [0.0_f32, 2.9, 5.8, 8.7].iter().enumerate() {
                let yy = 4.9 + y;
                if k % 2 == 0 {
                    pen.ln(11.2, yy, 12.8, yy, 0.9, GRAY_D);
                }
            }
            pen.arrow((9.4, 10.0), (-1.0, 0.0), 2.6, INK);
            pen.arrow((14.6, 10.0), (1.0, 0.0), 2.6, INK);
            pen.bar(8.6, 18.2, 6.8, 1.6, INK);
        }
        Border => {
            pen.rrs(3.6, 3.6, 16.8, 16.8, 0.8, WHITE, INK, 1.8);
            pen.bar(4.2, 11.3, 15.6, 1.4, Color32::from_rgb(196,196,196));
            pen.bar(11.3, 4.2, 1.4, 15.6, Color32::from_rgb(196,196,196));
        }

        // ---- 数字 ----
        Currency => pen.text("¥", 14.5, 12.0, 12.2, GREEN_D),
        Percent => pen.text("%", 13.5, 12.0, 12.2, GREEN_D),
        Comma => pen.text(",", 16.4, 12.6, 21.0, GREEN_D),
        DecInc => {
            pen.text("0.0", 8.6, 9.2, 12.6, INK);
            pen.bar(16.4, 6.4, 4.6, 1.3, INK);
            pen.bar(18.05, 4.75, 1.3, 4.6, INK);
        }
        DecDec => {
            pen.text("0.0", 8.6, 9.2, 12.6, INK);
            pen.bar(16.4, 6.4, 4.6, 1.3, INK);
        }

        // ---- 排序 / 筛选 / 查找 ----
        SortAsc => {
            pen.rule(4.0, 6.2, 8.0, GRAY_D);
            pen.rule(4.0, 10.8, 6.0, GRAY_D);
            pen.rule(4.0, 15.4, 4.0, GRAY_D);
            pen.bar(17.0, 6.0, 1.8, 12.0, INK);
            pen.arrow((17.9, 4.2), (0.0, -1.0), 3.6, INK);
        }
        SortDesc => {
            pen.rule(4.0, 6.2, 4.0, GRAY_D);
            pen.rule(4.0, 10.8, 6.0, GRAY_D);
            pen.rule(4.0, 15.4, 8.0, GRAY_D);
            pen.bar(17.0, 5.8, 1.8, 12.0, INK);
            pen.arrow((17.9, 19.6), (0.0, 1.0), 3.6, INK);
        }
        Filter => {
            pen.poly(
                &[
                    (3.8, 4.8),
                    (20.2, 4.8),
                    (13.8, 12.2),
                    (13.8, 19.2),
                    (10.2, 16.8),
                    (10.2, 12.2),
                ],
                BLUE,
                BLUE_D,
                1.0,
            );
        }
        Find => {
            pen.ring(10.2, 10.2, 5.4, 1.9, GRAY_D);
            pen.ln(14.2, 14.4, 19.8, 19.8, 2.4, GRAY_D);
        }

        // ---- 插入 ----
        Chart => {
            pen.bar(4.8, 12.6, 3.8, 7.4, BLUE);
            pen.bar(10.1, 7.4, 3.8, 12.6, ORANGE);
            pen.bar(15.4, 10.2, 3.8, 9.8, GRAY);
            pen.ln(3.0, 20.9, 21.0, 20.9, 1.2, GRAY_D);
        }
        Image => {
            pen.rrs(3.0, 5.0, 18.0, 14.0, 1.2, BLUE_L, BLUE_D, 1.1);
            pen.poly(
                &[(3.8, 18.4), (9.6, 11.4), (14.6, 18.4)],
                GREEN,
                GREEN_D,
                0.9,
            );
            pen.poly(
                &[(12.4, 18.4), (16.6, 12.4), (20.4, 18.4)],
                GREEN_D,
                GREEN_D,
                0.9,
            );
            pen.dot(8.2, 9.2, 1.7, GOLD);
        }
        Table => {
            pen.rrs(3.0, 4.4, 18.0, 15.2, 1.0, WHITE, BLUE_D, 1.1);
            pen.bar(3.4, 4.8, 17.2, 4.0, BLUE);
            pen.bar(3.4, 9.2, 17.2, 1.0, BLUE_XL);
            pen.bar(3.4, 14.4, 17.2, 1.0, BLUE_XL);
            pen.bar(8.9, 9.2, 1.0, 10.2, BLUE_XL);
            pen.bar(15.0, 9.2, 1.0, 10.2, BLUE_XL);
        }
        Comment => {
            pen.rrs(4.0, 4.0, 16.0, 11.0, 2.2, CREAM, GOLD_D, 1.1);
            pen.poly(&[(7.2, 14.6), (7.2, 19.4), (11.8, 14.6)], CREAM, GOLD_D, 1.0);
            pen.bar(7.0, 8.0, 10.0, 1.2, GOLD_D);
            pen.bar(7.0, 11.0, 7.4, 1.2, GOLD_D);
        }
        Pivot => {
            pen.rrs(3.0, 4.0, 11.0, 9.0, 0.8, BLUE_L, BLUE_D, 1.0);
            pen.bar(3.2, 7.2, 10.6, 1.0, BLUE_D);
            pen.bar(7.6, 4.2, 1.0, 8.6, BLUE_D);
            pen.bar(13.6, 18.6, 7.2, 1.6, BLUE_D);
            pen.bar(17.2, 14.4, 1.6, 4.2, BLUE_D);
            pen.arrow((21.4, 19.4), (1.0, 0.0), 3.0, BLUE_D);
        }
        Link => {
            pen.ring(8.0, 12.0, 4.0, 2.0, BLUE);
            pen.ring(16.0, 12.0, 4.0, 2.0, BLUE);
            pen.bar(11.2, 11.2, 1.6, 1.6, BLUE_D);
        }

        // ---- 审阅 / 视图 ----
        Revision => {
            pen.ring(12.0, 12.0, 8.0, 1.8, GRAY_D);
            pen.ln(12.0, 12.0, 12.0, 6.8, 1.8, GRAY_D);
            pen.ln(12.0, 12.0, 15.8, 14.0, 1.8, GRAY_D);
        }
        Zoom => {
            pen.ring(10.2, 10.2, 5.4, 1.9, GRAY_D);
            pen.ln(14.2, 14.4, 19.8, 19.8, 2.4, GRAY_D);
            pen.bar(7.6, 9.4, 5.2, 1.5, BLUE);
            pen.bar(9.45, 7.55, 1.5, 5.2, BLUE);
        }
        Freeze => {
            pen.rrs(3.2, 3.2, 17.6, 17.6, 0.8, WHITE, GRAY_D, 1.3);
            pen.bar(3.6, 3.6, 16.8, 5.0, BLUE_XL);
            pen.bar(3.6, 3.6, 5.0, 16.8, BLUE_XL);
            pen.bar(3.6, 8.4, 16.8, 1.2, BLUE_D);
            pen.bar(8.4, 3.6, 1.2, 16.8, BLUE_D);
        }
        Lock => {
            pen.polyline(
                &[(7.8, 10.4), (7.8, 7.2), (12.0, 4.4), (16.2, 7.2), (16.2, 10.4)],
                1.9,
                GRAY_D,
            );
            pen.rrs(5.8, 10.0, 12.4, 9.6, 1.2, GOLD, GOLD_D, 1.1);
            pen.dot(12.0, 13.6, 1.5, GOLD_D);
            pen.bar(11.4, 14.2, 1.2, 2.4, GOLD_D);
        }
        Cf => {
            pen.rrs(3.0, 4.0, 18.0, 16.0, 1.0, WHITE, GRAY_D, 1.1);
            pen.bar(4.4, 5.6, 15.2, 2.8, BLUE);
            pen.bar(4.4, 9.8, 10.6, 2.8, ORANGE);
            pen.bar(4.4, 14.0, 6.4, 2.8, GREEN);
        }
        IndentInc => {
            for i in 0..4 {
                pen.bar(8.2, 5.6 + i as f32 * 4.0, 12.0, 1.6, if i < 2 { GRAY_D } else { INK });
            }
            pen.bar(3.4, 11.2, 5.4, 1.7, BLUE);
            pen.arrow((9.4, 12.0), (1.0, 0.0), 3.0, BLUE);
        }
        IndentDec => {
            for i in 0..4 {
                pen.bar(3.4, 5.6 + i as f32 * 4.0, 12.0, 1.6, if i < 2 { GRAY_D } else { INK });
            }
            pen.bar(15.2, 11.2, 5.4, 1.7, BLUE);
            pen.arrow((14.6, 12.0), (-1.0, 0.0), 3.0, BLUE);
        }
        ClearFmt => {
            pen.text("A", 11.0, 8.6, 10.4, INK);
            pen.poly(
                &[(13.2, 15.2), (18.2, 10.2), (21.4, 13.4), (16.4, 18.4)],
                Color32::from_rgb(240, 200, 226),
                Color32::from_rgb(164, 104, 146),
                1.05,
            );
            pen.ln(15.6, 17.2, 18.8, 14.0, 0.9, Color32::from_rgb(164, 104, 146));
        }
        Eraser => {
            // 正面橡皮：矩形主体 + 浅色斜切面 + 擦过的痕迹
            pen.rrs(4.2, 9.6, 15.6, 8.4, 1.2, Color32::from_rgb(240, 200, 226), Color32::from_rgb(164, 104, 146), 1.2);
            pen.poly(
                &[(4.2, 9.6), (8.6, 9.6), (4.2, 14.4)],
                Color32::from_rgb(250, 226, 240),
                Color32::from_rgb(164, 104, 146),
                1.0,
            );
            pen.bar(3.0, 19.8, 10.0, 1.6, Color32::from_rgb(190, 190, 190));
        }

        // ---- 文件 / 编辑 ----
        Save => {
            pen.rrs(4.0, 4.0, 16.0, 16.0, 1.0, BLUE_L, BLUE_D, 1.2);
            pen.bar(8.0, 4.4, 8.0, 4.6, BLUE_D);
            pen.rrs(7.0, 12.8, 10.0, 7.2, 0.5, WHITE, BLUE_D, 1.0);
        }
        SaveAs => {
            pen.rrs(3.0, 4.0, 14.4, 16.0, 1.0, BLUE_L, BLUE_D, 1.2);
            pen.bar(6.0, 4.4, 7.0, 4.2, BLUE_D);
            pen.ln(16.4, 20.2, 16.4, 13.0, 1.9, GREEN_D);
            pen.arrow((16.4, 11.6), (0.0, -1.0), 3.4, GREEN_D);
        }
        Open => {
            pen.poly(
                &[
                    (3.0, 19.0),
                    (3.0, 6.0),
                    (10.0, 6.0),
                    (12.2, 8.8),
                    (20.8, 8.8),
                    (20.8, 19.0),
                ],
                GOLD,
                GOLD_D,
                1.1,
            );
            pen.bar(3.4, 9.6, 17.0, 1.0, GOLD_D);
        }
        Print => {
            pen.rrs(6.0, 3.0, 12.0, 6.0, 0.6, WHITE, GRAY_D, 1.0);
            pen.rrs(3.0, 8.8, 18.0, 7.2, 1.0, BLUE_L, BLUE_D, 1.1);
            pen.rrs(6.2, 13.6, 11.6, 7.4, 0.6, WHITE, GRAY_D, 1.0);
            pen.bar(8.0, 16.4, 8.0, 1.0, GRAY);
            pen.bar(8.0, 18.2, 8.0, 1.0, GRAY);
        }
        Undo => {
            pen.polyline(&[(7.6, 8.0), (7.6, 16.6), (16.4, 16.6)], 1.9, BLUE_D);
            pen.bar(7.6, 7.0, 4.6, 1.9, BLUE_D);
            pen.arrow((5.8, 8.0), (-1.0, 0.0), 3.4, BLUE_D);
        }
        Redo => {
            pen.polyline(&[(16.4, 8.0), (16.4, 16.6), (7.6, 16.6)], 1.9, BLUE_D);
            pen.bar(11.8, 7.0, 4.6, 1.9, BLUE_D);
            pen.arrow((18.2, 8.0), (1.0, 0.0), 3.4, BLUE_D);
        }
        Grid => {
            pen.rrs(3.4, 3.4, 17.2, 17.2, 1.0, WHITE, GRAY_D, 1.3);
            pen.bar(4.0, 11.4, 16.0, 1.3, BLUE);
            pen.bar(11.4, 4.0, 1.3, 16.0, BLUE);
            pen.bar(4.0, 4.4, 15.6, 3.4, BLUE_XL);
        }
        Close => {
            pen.ln(6.6, 6.6, 17.4, 17.4, 2.1, RED);
            pen.ln(17.4, 6.6, 6.6, 17.4, 2.1, RED);
        }
        Plus => {
            pen.bar(11.1, 5.2, 1.8, 13.6, GREEN_D);
            pen.bar(5.2, 11.1, 13.6, 1.8, GREEN_D);
        }
        Refresh => {
            pen.ring(12.0, 12.6, 6.6, 1.9, BLUE_D);
            pen.polyline(&[(18.6, 12.0), (18.6, 7.0), (13.6, 7.0)], 1.9, BLUE_D);
            pen.arrow((19.6, 5.6), (1.0, -0.6), 3.2, BLUE_D);
        }
        Share => {
            pen.bar(8.3, 10.9, 6.4, 1.7, BLUE);
            pen.bar(8.3, 13.2, 6.4, 1.7, BLUE);
            pen.dot(17.0, 6.0, 2.5, BLUE);
            pen.dot(6.2, 12.0, 2.5, BLUE);
            pen.dot(17.0, 18.0, 2.5, BLUE);
        }
        Fx => pen.text("fx", 12.0, 12.0, 13.0, GRAY_D),
        Sum => pen.text("Σ", 15.0, 12.0, 12.2, GREEN_D),
        Symbol => pen.text("Ω", 15.0, 12.0, 12.2, BLUE_D),
        ZhS => pen.text("简", 13.0, 12.0, 12.4, RED),
        ZhT => pen.text("繁", 13.0, 12.0, 12.4, BLUE_D),
        TraceIn => {
            // 追踪引用：箭头从外面指向本格
            pen.rro(12.0, 7.0, 9.0, 10.0, 1.0, BLUE_D, 1.4);
            pen.ln(3.0, 12.0, 9.0, 12.0, 1.5, RED);
            pen.poly(&[(11.0, 12.0), (7.5, 9.6), (7.5, 14.4)], RED, RED, 0.0);
        }
        // ---- 文本框 ----
        TextBox => {
            // 一个浅底方框 + 里面几行字线 + 右上角的小 T
            pen.rr(3.4, 6.4, 17.2, 12.6, 0.8, WHITE);
            pen.rro(3.4, 6.4, 17.2, 12.6, 0.8, INK, 1.1);
            pen.rule(6.0, 10.4, 10.0, GRAY_D);
            pen.rule(6.0, 13.2, 12.0, GRAY);
            pen.rule(6.0, 16.0, 8.0, GRAY);
            // 角标 T：深蓝，和 Excel 的文本框按钮一致
            pen.bar(16.4, 2.0, 5.0, 1.6, BLUE_D);
            pen.bar(18.3, 2.0, 1.6, 5.0, BLUE_D);
        }
        // ---- 迷你图 ----
        SparkLine => {
            // 浅色格子 + 蓝折线（和 Excel 的“折线迷你图”按钮同构）
            pen.rr(3.0, 5.0, 18.0, 14.0, 0.8, BLUE_L);
            pen.rro(3.0, 5.0, 18.0, 14.0, 0.8, BLUE_S, 1.0);
            grid(&pen, 3.0, 5.0, 18.0, 14.0);
            let pts = [
                (5.0, 15.0),
                (8.0, 10.5),
                (11.0, 13.0),
                (14.0, 8.0),
                (19.0, 11.5),
            ];
            for w in pts.windows(2) {
                pen.ln(w[0].0, w[0].1, w[1].0, w[1].1, 1.5, BLUE_D);
            }
            pen.bar(18.0, 7.0, 2.0, 2.0, RED);
        }
        SparkColumn => {
            pen.rr(3.0, 5.0, 18.0, 14.0, 0.8, WHITE);
            pen.rro(3.0, 5.0, 18.0, 14.0, 0.8, BLUE_S, 1.0);
            grid(&pen, 3.0, 5.0, 18.0, 14.0);
            // 五根柱，高低错开
            let bars = [(5.0, 6.0), (8.0, 9.5), (11.0, 4.0), (14.0, 8.0), (17.0, 5.0)];
            for (x, h) in bars {
                pen.bar(x, 18.0 - h, 2.2, h, BLUE_M_D);
            }
        }
        SparkLoss => {
            pen.rr(3.0, 5.0, 18.0, 14.0, 0.8, WHITE);
            pen.rro(3.0, 5.0, 18.0, 14.0, 0.8, BLUE_S, 1.0);
            // 中线 + 上下分别的红绿块（Excel 的盈亏图标就是这个意思）
            pen.ln(4.0, 12.0, 20.0, 12.0, 0.9, INK);
            for (i, up) in [true, false, true, false, true].iter().enumerate() {
                let x = 5.0 + i as f32 * 3.0;
                if *up {
                    pen.bar(x, 9.2, 2.2, 2.8, GREEN);
                } else {
                    pen.bar(x, 12.0, 2.2, 2.8, RED);
                }
            }
        }
        TraceOut => {
            // 追踪从属：箭头从本格指向外面
            pen.rro(3.0, 7.0, 9.0, 10.0, 1.0, BLUE_D, 1.4);
            pen.ln(13.0, 12.0, 19.0, 12.0, 1.5, RED);
            pen.poly(&[(21.0, 12.0), (17.5, 9.6), (17.5, 14.4)], RED, RED, 0.0);
        }
        Calc => {
            pen.rrs(
                4.0,
                3.0,
                16.0,
                18.0,
                1.2,
                Color32::from_rgb(237, 237, 237),
                GRAY_D,
                1.1,
            );
            pen.rrs(7.0, 6.0, 10.0, 3.6, 0.6, WHITE, GRAY, 0.9);
            for i in 0..3 {
                pen.dot(8.4 + i as f32 * 3.6, 13.6, 1.1, GRAY_D);
                pen.dot(8.4 + i as f32 * 3.6, 17.4, 1.1, GRAY_D);
            }
        }
        Sheet => {
            pen.rrs(4.0, 3.0, 16.0, 18.0, 1.0, GREEN_L, GREEN_D, 1.1);
            pen.bar(4.4, 8.0, 15.2, 1.0, GREEN_D);
            pen.bar(11.0, 8.0, 1.0, 12.6, GREEN_D);
            pen.bar(4.4, 14.5, 15.2, 1.0, GREEN_D);
        }
        HeaderFooter => {
            // 一页纸：上下各一条深色带（页眉/页脚）+ 中间三条浅线
            pen.rro(4.6, 3.2, 14.8, 17.6, 0.8, INK, 1.2);
            pen.bar(4.6, 3.2, 14.8, 3.0, Color32::from_rgb(33, 115, 70));
            pen.bar(4.6, 17.8, 14.8, 3.0, Color32::from_rgb(33, 115, 70));
            pen.ln(7.0, 8.8, 17.0, 8.8, 0.9, GRAY_D);
            pen.ln(7.0, 11.8, 17.0, 11.8, 0.9, GRAY_D);
            pen.ln(7.0, 14.8, 13.4, 14.8, 0.9, GRAY_D);
        }
        DocLink => {
            // 两份文档 + 右下角一小段链条：跨工作簿引用
            pen.rrs(3.4, 4.2, 10.6, 13.6, 0.6, WHITE, GRAY_D, 1.0);
            pen.rrs(6.4, 6.6, 10.6, 13.6, 0.6, WHITE, INK, 1.1);
            pen.ln(8.4, 10.0, 15.2, 10.0, 0.9, GRAY_D);
            pen.ln(8.4, 12.4, 15.2, 12.4, 0.9, GRAY_D);
            pen.rrs(13.6, 15.2, 4.2, 3.0, 1.5, Color32::from_rgb(222, 235, 247), BLUE_D, 0.9);
            pen.rrs(17.0, 17.0, 4.2, 3.0, 1.5, Color32::from_rgb(222, 235, 247), BLUE_D, 0.9);
        }
        TextDir => {
            // 竖排文字：一个 A + 向下的折线箭头（Excel 的「方向」图标）
            pen.text("A", 8.2, 9.0, 12.0, INK);
            pen.ln(17.6, 4.0, 17.6, 18.0, 1.5, GRAY_D);
            pen.arrow((17.6, 19.6), (0.0, 1.0), 3.2, GRAY_D);
        }
        FontGrow => {
            // Excel：一个 A + 右侧向上的小箭头（不要加号，16px 下三个元素会糊成一团）
            pen.text("A", 9.4, 9.0, 13.0, INK);
            pen.bar(17.2, 7.2, 1.7, 9.4, GREEN_D);
            pen.arrow((18.05, 5.6), (0.0, -1.0), 3.4, GREEN_D);
        }
        FontShrink => {
            pen.text("A", 9.4, 9.0, 13.0, INK);
            pen.bar(17.2, 7.2, 1.7, 9.4, GREEN_D);
            pen.arrow((18.05, 18.4), (0.0, 1.0), 3.4, GREEN_D);
        }
    }
}
