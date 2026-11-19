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
    /// 文档标签条左侧的「回开始页」
    Home,
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
    GroupRows,
    UngroupRows,
    ShowDetail,
    HideDetail,
    Validation,
    SplitColumns,
    RemoveDup,
    Consolidate,
    ImportCsv,
    ImportBook,
    WhatIf,
    OutlineLevels,
    GoalSeek,
    Scenario,
    Eye,
    EyeOff,
    PageLayout,
    PageBreak,
    NormalView,
    ShowHeadings,
    FormulaBar,
    TextSpill,
    ViewPane,
    Pin,
    NewView,
    ZoomSelection,
    FormulaView,
    ErrorCheck,
    NameTag,
    WatchPane,
    Spelling,
    CommentPrev,
    CommentNext,
    ShowComment,
    A11y,
    Online,
    AutoShare,
    Help,
    About,
    Orientation,
    PaperSize,
    PrintGrid,
    PrintHeadings,
    PrintArea,
    PivotRefresh,
    PivotOptions,
    ChartList,
    FolderPlus,
    AppDefault,
    AppAdd,
    PdfExport,
    HostTakeover,
    UpdateCheck,
    FormatCells,
    TextVerticalDraw,
    TextBoxDraw,
    CalcSheet,
    SheetRef,
    FilterReapply,
    SortDialog,
    ShowChanges,
    SheetViewSwitch,
    Zoom100,
    ZoomDialog,
    ZoomIn,
    Unfreeze,
    TableToRange,
    SparkEdit,
    ChartEdit,
    RichText,
    Freeze,
    FreezeRow,
    FreezeCol,
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
    /// 新建文件：空白页 + 绿色加号（和「插入」的纯加号分开）
    NewFile,
    /// 宏：代码页 + 绿色播放三角
    Macro,
    /// 运行宏：绿色播放键
    MacroRun,
    /// VBA 编辑器：代码窗口
    Code,
    /// 并排比较：两页 + 对向箭头
    Compare,
    /// 同步滚动：两页 + 上下双箭头
    SyncScroll,
    /// 标识无效数据：红圈 + 感叹号
    InvalidCircle,
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

/// 线宽量化：逻辑宽 * 设备像素比 → 最近的整数设备像素（至少 1）
fn dev_stroke(w_logical: f32, ppp: f32) -> f32 {
    (w_logical * ppp * 0.82).max(1.0).round()
}

/// 线条中心对齐：奇数设备宽 → 半像素中心；偶数设备宽 → 整像素中心
fn snap_center(v: f32, ppp: f32, wdev: f32) -> f32 {
    let off = if (wdev.round() as i32) & 1 == 1 { 0.5 } else { 0.0 };
    ((v * ppp - off).round() + off) / ppp
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
    /// 线宽量化到整数设备像素（1 / 2 / 3 …）：分数像素宽的线在任何 DPI 下都会糊
    fn wdev(&self, w: f32) -> f32 {
        dev_stroke(w * self.s, self.ppp)
    }
    /// 线宽换回逻辑像素
    fn wl(&self, w: f32) -> f32 {
        self.wdev(w) / self.ppp
    }
    /// 线条中心对齐：奇数设备宽落在半像素，偶数落在整像素
    fn snapl(&self, v: f32, wdev: f32) -> f32 {
        snap_center(v, self.ppp, wdev)
    }
    fn xy(&self, x: f32, y: f32) -> Pos2 {
        Pos2::new(
            self.snap(self.r.left() + x * self.s),
            self.snap(self.r.top() + y * self.s),
        )
    }
    /// 线条端点：按线宽奇偶做对齐
    fn xyl(&self, x: f32, y: f32, wdev: f32) -> Pos2 {
        Pos2::new(
            self.snapl(self.r.left() + x * self.s, wdev),
            self.snapl(self.r.top() + y * self.s, wdev),
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
            // Fluent 2 是平涂：不再加顶部高光条。
            // 高光 + 渐变会让 32px 图标在浅色功能区里显脏，也是一眼「旧 Office」的来源。
        }
        self.p.rect_stroke(
            rect,
            cr,
            Stroke::new(self.wl(sw), self.col(stroke)),
            egui::StrokeKind::Inside,
        );
    }

    /// 只描边的圆角矩形（Excel 的图标主体很多是轮廓）
    fn rro(&self, x: f32, y: f32, w: f32, h: f32, rad: f32, stroke: Color32, sw: f32) {
        self.p.rect_stroke(
            self.rect_of(x, y, w, h),
            self.px(rad),
            Stroke::new(self.wl(sw), self.col(stroke)),
            egui::StrokeKind::Inside,
        );
    }

    fn ln(&self, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, c: Color32) {
        let wd = self.wdev(w);
        self.p.line_segment(
            [self.xyl(x1, y1, wd), self.xyl(x2, y2, wd)],
            Stroke::new(wd / self.ppp, self.col(c)),
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
        let wd = self.wdev(w);
        self.p.circle_stroke(
            self.xyl(x, y, wd),
            self.px(rad),
            Stroke::new(wd / self.ppp, self.col(c)),
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
            Stroke::new(self.wl(sw), self.col(stroke)),
        ));
    }

    fn polyline(&self, pts: &[(f32, f32)], w: f32, c: Color32) {
        let wd = self.wdev(w);
        let v: Vec<Pos2> = pts.iter().map(|q| self.xyl(q.0, q.1, wd)).collect();
        self.p.add(egui::Shape::line(v, Stroke::new(wd / self.ppp, self.col(c))));
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

/// 全部图标（由枚举顺序生成；新增图标后同步这里，测试会数个数）
pub const ALL_ICONS: [Icon; 146] = [
    Icon::Open, Icon::Save, Icon::SaveAs, Icon::Print, Icon::Undo, Icon::Redo,
    Icon::Paste, Icon::Cut, Icon::Copy, Icon::Brush, Icon::Fill, Icon::FontColor,
    Icon::AlignL, Icon::AlignC, Icon::AlignR, Icon::Top, Icon::Middle, Icon::Bottom,
    Icon::Wrap, Icon::Merge, Icon::Border, Icon::Currency, Icon::Percent, Icon::Comma,
    Icon::SortAsc, Icon::SortDesc, Icon::Filter, Icon::Find, Icon::Chart, Icon::Image,
    Icon::Table, Icon::Comment, Icon::Pivot, Icon::Link, Icon::Revision, Icon::Zoom,
    Icon::Calc, Icon::Sheet, Icon::Share, Icon::Fx, Icon::Sum, Icon::Grid,
    Icon::Close, Icon::Plus, Icon::Refresh, Icon::DecInc, Icon::DecDec, Icon::FontGrow,
    Icon::FontShrink, Icon::TextDir, Icon::HeaderFooter, Icon::DocLink, Icon::GroupRows, Icon::UngroupRows,
    Icon::ShowDetail, Icon::HideDetail, Icon::Validation, Icon::SplitColumns, Icon::RemoveDup, Icon::Consolidate,
    Icon::ImportCsv, Icon::ImportBook, Icon::WhatIf, Icon::OutlineLevels, Icon::GoalSeek, Icon::Scenario,
    Icon::Eye, Icon::EyeOff, Icon::PageLayout, Icon::PageBreak, Icon::NormalView, Icon::ShowHeadings,
    Icon::FormulaBar, Icon::TextSpill, Icon::ViewPane, Icon::Pin, Icon::NewView, Icon::ZoomSelection,
    Icon::FormulaView, Icon::ErrorCheck, Icon::NameTag, Icon::WatchPane, Icon::Spelling, Icon::CommentPrev,
    Icon::CommentNext, Icon::ShowComment, Icon::Online, Icon::AutoShare, Icon::Help, Icon::About,
    Icon::Orientation, Icon::PaperSize, Icon::PrintGrid, Icon::PrintHeadings, Icon::PrintArea, Icon::PivotRefresh,
    Icon::PivotOptions, Icon::ChartList, Icon::FolderPlus, Icon::AppDefault, Icon::AppAdd, Icon::PdfExport,
    Icon::HostTakeover, Icon::UpdateCheck, Icon::FormatCells, Icon::TextVerticalDraw, Icon::TextBoxDraw, Icon::CalcSheet,
    Icon::SheetRef, Icon::FilterReapply, Icon::SortDialog, Icon::ShowChanges, Icon::SheetViewSwitch, Icon::ZoomDialog,
    Icon::ZoomIn, Icon::Unfreeze, Icon::TableToRange, Icon::SparkEdit, Icon::ChartEdit, Icon::RichText,
    Icon::Freeze, Icon::FreezeRow, Icon::FreezeCol, Icon::Lock, Icon::Cf, Icon::Eraser,
    Icon::ClearFmt, Icon::IndentInc, Icon::IndentDec, Icon::Symbol, Icon::ZhS, Icon::ZhT,
    Icon::TraceIn, Icon::TraceOut, Icon::SparkLine, Icon::SparkColumn, Icon::SparkLoss, Icon::TextBox,
    Icon::NewFile, Icon::Macro, Icon::MacroRun, Icon::Code, Icon::Compare, Icon::SyncScroll,
    Icon::InvalidCircle, Icon::Home,
];

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

/// 单色绘制：忽略图标内置配色，整枚用 `color`。
/// 给标签条/标题栏这类「只用中性灰」的场景，别再用 `enabled=false` 冒充。
pub fn draw_mono(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    paint(p, rect.expand(rect.width() * 0.04), icon, Some(color));
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
            // Excel 365：橙色夹板外框 + 浅黄纸面 + 白色文档；
            // 原来整枚图标是米色（max-min<20），在 1440 的功能区里几乎没有彩色像素。
            pen.rr(5.4, 4.6, 13.2, 16.0, 1.2, Color32::from_rgb(248, 219, 143));
            pen.rro(5.4, 4.6, 13.2, 16.0, 1.2, Color32::from_rgb(222, 108, 0), 1.2);
            pen.rr(7.8, 7.4, 8.4, 10.6, 0.5, WHITE);
            pen.rro(7.8, 7.4, 8.4, 10.6, 0.5, GRAY_D, 0.8);
            pen.ln(9.6, 10.4, 14.2, 10.4, 0.8, GRAY_D);
            pen.ln(9.6, 12.6, 14.2, 12.6, 0.8, GRAY_D);
            pen.rr(9.6, 2.8, 4.8, 3.6, 0.9, Color32::from_rgb(225, 223, 221));
            pen.rro(9.6, 2.8, 4.8, 3.6, 0.9, Color32::from_rgb(96, 94, 92), 0.95);
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
            // 合并后居中：外框 + 中间虚线 + 相向箭头 + 下方居中文字线
            pen.rro(3.4, 3.8, 17.2, 10.2, 0.6, INK, 1.15);
            for y in [5.0_f32, 8.0, 11.0] {
                pen.ln(12.0, y, 12.0, y + 1.4, 0.9, GRAY_D);
            }
            pen.ln(7.0, 8.9, 10.6, 8.9, 1.4, INK);
            pen.arrow((11.6, 8.9), (1.0, 0.0), 2.2, INK);
            pen.ln(17.0, 8.9, 13.4, 8.9, 1.4, INK);
            pen.arrow((12.4, 8.9), (-1.0, 0.0), 2.2, INK);
            pen.bar(8.4, 17.4, 7.2, 1.7, INK);
            pen.bar(10.2, 20.4, 3.6, 1.5, GRAY_D);
        }
        Border => {
            pen.rrs(3.6, 3.6, 16.8, 16.8, 0.8, WHITE, INK, 1.8);
            pen.bar(4.2, 11.3, 15.6, 1.4, Color32::from_rgb(196,196,196));
            pen.bar(11.3, 4.2, 1.4, 15.6, Color32::from_rgb(196,196,196));
        }

        // ---- 数字 ----
        Currency => pen.text("¥", 14.5, 12.0, 12.2, GREEN_D),
        Percent => pen.text("%", 13.5, 12.0, 12.2, GREEN_D),
        Comma => {
            // UI-29.2：以前是 FontId 画的「,」字号 21 网格单位，字形下伸到 y≈26，
            // 16px 档会压到下面的标签上；改成矢量逗号，稳定落在 0..24 内。
            pen.polyline(&[(12.4, 10.6), (11.6, 14.2), (10.6, 18.4)], 2.6, GREEN_D);
            pen.dot(12.4, 10.6, 1.9, GREEN_D);
            pen.dot(10.6, 18.4, 1.05, GREEN_D);
        }
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
            // Excel 365：左边 A 在上、Z 在下，右边一支向下箭头（A→Z）
            pen.text("A", 8.6, 8.0, 6.4, INK);
            pen.text("Z", 8.6, 8.0, 13.8, GRAY_D);
            pen.bar(18.0, 5.4, 1.6, 12.4, GRAY_D);
            pen.arrow((18.8, 20.2), (0.0, 1.0), 3.8, GREEN_D);
        }
        SortDesc => {
            // Z→A：Z 在上、A 在下，箭头朝上
            pen.text("Z", 8.6, 8.0, 6.4, INK);
            pen.text("A", 8.6, 8.0, 13.8, GRAY_D);
            pen.bar(18.0, 6.2, 1.6, 12.4, GRAY_D);
            pen.arrow((18.8, 3.8), (0.0, -1.0), 3.8, GREEN_D);
        }
        Filter => {
            // Excel 365：深灰漏斗线稿，浅灰内面，不加蓝
            pen.poly(
                &[
                    (3.8, 4.6),
                    (20.2, 4.6),
                    (13.8, 12.0),
                    (13.8, 19.4),
                    (10.2, 16.9),
                    (10.2, 12.0),
                ],
                Color32::from_rgb(243, 242, 241),
                INK,
                1.15,
            );
        }
        Find => {
            pen.ring(10.2, 10.2, 5.4, 1.9, GRAY_D);
            pen.ln(14.2, 14.4, 19.8, 19.8, 2.4, GRAY_D);
        }

        // ---- 插入 ----
        Chart => {
            // 扁平柱状图：绿 / 蓝灰 / 灰三根，同一基线，无高光
            pen.bar(4.6, 12.4, 4.0, 7.6, Color32::from_rgb(33, 115, 70));
            pen.bar(10.0, 7.2, 4.0, 12.8, Color32::from_rgb(91, 127, 166));
            pen.bar(15.4, 10.0, 4.0, 10.0, Color32::from_rgb(165, 165, 165));
            pen.ln(3.0, 20.8, 21.0, 20.8, 1.2, GRAY_D);
        }
        Image => {
            // 相片：深灰相框 + 白底 + 浅蓝山 + 金色太阳（降低饱和度）
            pen.rrs(3.0, 5.0, 18.0, 14.0, 1.0, WHITE, INK, 1.2);
            pen.poly(
                &[(3.9, 18.2), (9.6, 11.6), (14.4, 18.2)],
                Color32::from_rgb(197, 217, 241),
                Color32::from_rgb(91, 127, 166),
                0.85,
            );
            pen.poly(
                &[(12.4, 18.2), (16.6, 12.6), (20.2, 18.2)],
                Color32::from_rgb(162, 190, 226),
                Color32::from_rgb(91, 127, 166),
                0.85,
            );
            pen.dot(8.0, 9.0, 1.7, Color32::from_rgb(255, 192, 0));
        }
        Table => {
            // 表格：中性网格 + 浅灰表头条，网格线用 191 灰
            pen.rrs(3.0, 4.4, 18.0, 15.2, 0.8, WHITE, INK, 1.2);
            pen.rr(3.4, 4.8, 17.2, 4.0, 0.0, Color32::from_rgb(217, 217, 217));
            pen.bar(3.4, 9.2, 17.2, 1.0, Color32::from_rgb(191, 191, 191));
            pen.bar(3.4, 14.4, 17.2, 1.0, Color32::from_rgb(191, 191, 191));
            pen.bar(8.9, 9.2, 1.0, 10.2, Color32::from_rgb(191, 191, 191));
            pen.bar(15.0, 9.2, 1.0, 10.2, Color32::from_rgb(191, 191, 191));
        }
        Comment => {
            pen.rrs(4.2, 4.2, 15.6, 10.6, 2.0, Color32::from_rgb(255, 246, 214), Color32::from_rgb(191, 144, 0), 1.0);
            pen.poly(&[(7.4, 14.6), (7.4, 19.0), (11.6, 14.6)], Color32::from_rgb(255, 246, 214), Color32::from_rgb(191, 144, 0), 0.95);
            pen.bar(7.2, 8.2, 9.6, 1.1, Color32::from_rgb(191, 144, 0));
            pen.bar(7.2, 11.0, 7.0, 1.1, Color32::from_rgb(191, 144, 0));
        }
        Pivot => {
            // 透视表：左边一张小网格，右下角一支汇总箭头（绿色强调）
            pen.rrs(3.0, 4.0, 10.6, 9.0, 0.8, Color32::from_rgb(243, 242, 241), INK, 1.1);
            pen.bar(3.4, 7.0, 9.8, 0.9, INK);
            pen.bar(7.2, 4.4, 0.9, 8.2, INK);
            pen.bar(13.4, 18.8, 7.4, 1.6, GRAY_D);
            pen.bar(17.0, 14.6, 1.6, 4.2, GRAY_D);
            pen.arrow((21.2, 19.6), (1.0, 0.0), 3.0, Color32::from_rgb(33, 115, 70));
        }
        Link => {
            pen.ring(8.2, 12.0, 4.0, 1.9, Color32::from_rgb(68, 114, 196));
            pen.ring(15.8, 12.0, 4.0, 1.9, Color32::from_rgb(68, 114, 196));
            pen.bar(11.2, 11.3, 1.6, 1.4, Color32::from_rgb(47, 85, 151));
        }
        Revision => {
            // 修订记录：纸 + 右上角一支笔（表示改动）
            pen.rrs(3.4, 3.2, 13.6, 17.6, 0.6, WHITE, INK, 1.15);
            pen.bar(6.0, 7.4, 8.4, 1.0, GRAY_D);
            pen.bar(6.0, 10.4, 8.4, 1.0, GRAY_D);
            pen.bar(6.0, 13.4, 5.4, 1.0, GRAY_D);
            pen.poly(&[(13.2, 17.6), (17.2, 13.6), (19.6, 16.0), (15.6, 20.0)], Color32::from_rgb(255, 217, 102), INK, 1.0);
            pen.poly(&[(13.2, 17.6), (12.6, 20.6), (15.6, 20.0)], INK, INK, 0.0);
        }
        Zoom => {
            pen.ring(10.2, 10.2, 5.4, 1.9, INK);
            pen.ln(14.2, 14.4, 19.8, 19.8, 2.4, INK);
            pen.bar(7.6, 9.4, 5.2, 1.4, GRAY_D);
            pen.bar(9.5, 7.5, 1.4, 5.2, GRAY_D);
        }
        FreezeRow => {
            // 冻结首行：顶带淡蓝 + 雪花
            pen.rrs(3.0, 3.0, 17.4, 17.4, 0.7, WHITE, INK, 1.2);
            pen.rr(3.4, 3.4, 16.6, 4.6, 0.0, Color32::from_rgb(222, 235, 247));
            pen.bar(3.4, 8.4, 16.6, 1.2, Color32::from_rgb(91, 127, 166));
            // 右下角小雪花：三个方向各一条，冻结的通用符号
            pen.ln(17.6, 15.4, 17.6, 21.0, 0.9, Color32::from_rgb(91, 127, 166));
            pen.ln(15.2, 16.8, 20.0, 19.6, 0.9, Color32::from_rgb(91, 127, 166));
            pen.ln(20.0, 16.8, 15.2, 19.6, 0.9, Color32::from_rgb(91, 127, 166));
        }
        FreezeCol => {
            // 冻结首列：左带淡蓝 + 雪花
            pen.rrs(3.0, 3.0, 17.4, 17.4, 0.7, WHITE, INK, 1.2);
            pen.rr(3.4, 3.4, 4.6, 16.6, 0.0, Color32::from_rgb(222, 235, 247));
            pen.bar(8.4, 3.4, 1.2, 16.6, Color32::from_rgb(91, 127, 166));
            // 右下角小雪花：三个方向各一条，冻结的通用符号
            pen.ln(17.6, 15.4, 17.6, 21.0, 0.9, Color32::from_rgb(91, 127, 166));
            pen.ln(15.2, 16.8, 20.0, 19.6, 0.9, Color32::from_rgb(91, 127, 166));
            pen.ln(20.0, 16.8, 15.2, 19.6, 0.9, Color32::from_rgb(91, 127, 166));
        }
        Freeze => {
            // 冻结窗格：顶带 + 左带 + 雪花
            pen.rrs(3.0, 3.0, 17.4, 17.4, 0.7, WHITE, INK, 1.2);
            pen.rr(3.4, 3.4, 16.6, 4.6, 0.0, Color32::from_rgb(222, 235, 247));
            pen.rr(3.4, 8.4, 4.6, 11.6, 0.0, Color32::from_rgb(222, 235, 247));
            pen.bar(3.4, 8.4, 16.6, 1.2, Color32::from_rgb(91, 127, 166));
            pen.bar(8.4, 3.4, 1.2, 16.6, Color32::from_rgb(91, 127, 166));
            // 右下角小雪花：三个方向各一条，冻结的通用符号
            pen.ln(17.6, 15.4, 17.6, 21.0, 0.9, Color32::from_rgb(91, 127, 166));
            pen.ln(15.2, 16.8, 20.0, 19.6, 0.9, Color32::from_rgb(91, 127, 166));
            pen.ln(20.0, 16.8, 15.2, 19.6, 0.9, Color32::from_rgb(91, 127, 166));
        }
       Lock => {
            pen.polyline(
                &[(7.8, 10.4), (7.8, 7.2), (12.0, 4.4), (16.2, 7.2), (16.2, 10.4)],
                1.9,
                INK,
            );
            pen.rrs(5.8, 10.0, 12.4, 9.6, 1.0, Color32::from_rgb(255, 217, 102), INK, 1.15);
            pen.dot(12.0, 13.6, 1.5, INK);
            pen.bar(11.4, 14.2, 1.2, 2.4, INK);
        }
        Cf => {
            // 条件格式：白底网格 + 蓝/橙/绿三条数据条（语义色保留，压暗一档）
            pen.rrs(3.0, 4.0, 18.0, 16.0, 0.8, WHITE, INK, 1.15);
            pen.bar(4.6, 5.8, 14.8, 2.6, Color32::from_rgb(68, 114, 196));
            pen.bar(4.6, 10.0, 10.4, 2.6, Color32::from_rgb(237, 125, 49));
            pen.bar(4.6, 14.2, 6.2, 2.6, Color32::from_rgb(112, 173, 71));
        }
        IndentInc => {
            for i in 0..4 {
                pen.bar(8.2, 5.6 + i as f32 * 4.0, 12.0, 1.6, if i < 2 { GRAY_D } else { INK });
            }
            pen.bar(3.4, 11.2, 5.4, 1.7, INK);
            pen.arrow((9.4, 12.0), (1.0, 0.0), 3.0, INK);
        }
        IndentDec => {
            for i in 0..4 {
                pen.bar(3.4, 5.6 + i as f32 * 4.0, 12.0, 1.6, if i < 2 { GRAY_D } else { INK });
            }
            pen.bar(15.2, 11.2, 5.4, 1.7, INK);
            pen.arrow((14.6, 12.0), (-1.0, 0.0), 3.0, INK);
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
        Home => {
            // Fluent 2 家：屋顶、两侧墙、地面、门。墙顶接在屋顶斜边上，
            // 不靠「画到差不多」留一道缝；16px 下门是最后消失的一笔。
            pen.polyline(&[(3.4, 10.6), (12.0, 3.6), (20.6, 10.6)], 1.4, INK);
            pen.ln(5.4, 8.8, 5.4, 20.2, 1.4, INK);
            pen.ln(18.6, 8.8, 18.6, 20.2, 1.4, INK);
            pen.ln(5.4, 20.2, 18.6, 20.2, 1.4, INK);
            pen.rro(9.6, 14.6, 4.8, 5.6, 0.7, INK, 1.2);
        }
        Close => {
            pen.ln(6.8, 6.8, 17.2, 17.2, 1.8, INK);
            pen.ln(17.2, 6.8, 6.8, 17.2, 1.8, INK);
        }
        Plus => {
            pen.bar(11.1, 5.2, 1.8, 13.6, INK);
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
        Sum => pen.text("Σ", 15.0, 12.0, 12.6, INK),
        Symbol => pen.text("Ω", 15.0, 12.0, 12.2, BLUE_D),
        RichText => {
            // 富文本：一个 A + 三条不同颜色的短线（选区里混排样式）
            pen.text("A", 12.0, 9.6, 12.4, INK);
            pen.bar(5.0, 16.6, 14.0, 1.8, Color32::from_rgb(68, 114, 196));
            pen.bar(5.0, 19.2, 9.4, 1.8, Color32::from_rgb(237, 125, 49));
        }
        ZhS => {
            // 简转繁：中性「简」+ 绿色向右箭头
            pen.text("简", 8.4, 12.0, 12.4, INK);
            pen.ln(15.4, 12.0, 21.2, 12.0, 1.8, Color32::from_rgb(33, 115, 70));
            pen.poly(&[(21.8, 12.0), (18.2, 9.6), (18.2, 14.4)], Color32::from_rgb(33, 115, 70), Color32::from_rgb(33, 115, 70), 0.0);
        }
        ZhT => {
            // 繁转简：中性「繁」+ 绿色向左箭头
            pen.text("繁", 14.6, 12.0, 12.4, INK);
            pen.ln(8.6, 12.0, 2.8, 12.0, 1.8, Color32::from_rgb(33, 115, 70));
            pen.poly(&[(2.2, 12.0), (5.8, 9.6), (5.8, 14.4)], Color32::from_rgb(33, 115, 70), Color32::from_rgb(33, 115, 70), 0.0);
        }
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
            pen.rrs(13.6, 15.2, 4.2, 3.0, 1.5, Color32::from_rgb(237, 243, 252), BLUE_D, 0.8);
            pen.rrs(17.0, 17.0, 4.2, 3.0, 1.5, Color32::from_rgb(237, 243, 252), BLUE_D, 0.8);
        }
        FolderPlus => {
            // 打开到新标签：文件夹 + 加号
            pen.poly(&[(2.8, 7.4), (9.0, 7.4), (10.6, 9.2), (21.0, 9.2), (21.0, 19.2), (2.8, 19.2)], Color32::from_rgb(255, 217, 102), INK, 1.15);
            pen.bar(10.4, 13.2, 6.0, 1.8, INK);
            pen.bar(12.5, 11.1, 1.8, 6.0, INK);
        }
        AppAdd => {
            // 加入「打开方式」：文件 + 加号
            pen.rrs(3.6, 3.6, 9.6, 12.4, 0.5, WHITE, INK, 1.1);
            pen.bar(5.4, 6.6, 6.0, 1.0, GRAY_D);
            pen.bar(5.4, 9.4, 6.0, 1.0, GRAY_D);
            pen.bar(14.2, 13.6, 7.0, 1.9, Color32::from_rgb(33, 115, 70));
            pen.bar(16.75, 11.05, 1.9, 7.0, Color32::from_rgb(33, 115, 70));
        }
        AppDefault => {
            // 设为默认打开方式：文件 + 齿轮
            pen.rrs(3.0, 3.6, 9.6, 12.4, 0.5, WHITE, INK, 1.1);
            pen.ring(16.6, 16.4, 3.2, 1.4, INK);
            pen.dot(16.6, 16.4, 0.9, INK);
            for k in 0..4 {
                let a = k as f32 * std::f32::consts::FRAC_PI_2;
                let (s, c) = (a.sin(), a.cos());
                pen.ln(16.6 + c * 3.4, 16.4 + s * 3.4, 16.6 + c * 5.0, 16.4 + s * 5.0, 1.5, INK);
            }
        }
        PdfExport => {
            // 导出 PDF：纸 + 向下箭头 + 红色页脚条
            pen.rrs(4.2, 2.8, 13.2, 15.0, 0.5, WHITE, INK, 1.15);
            pen.rr(4.6, 15.4, 12.4, 1.8, 0.0, Color32::from_rgb(192, 0, 0));
            pen.bar(6.4, 6.0, 8.8, 1.0, GRAY_D);
            pen.bar(6.4, 8.8, 8.8, 1.0, GRAY_D);
            pen.bar(6.4, 11.6, 5.6, 1.0, GRAY_D);
            pen.ln(19.0, 8.0, 19.0, 15.6, 1.8, Color32::from_rgb(33, 115, 70));
            pen.arrow((19.0, 17.8), (0.0, 1.0), 2.8, Color32::from_rgb(33, 115, 70));
        }
        HostTakeover => {
            // 接任宿主：显示器 + 向上箭头
            pen.rrs(3.0, 4.4, 13.2, 9.4, 0.6, WHITE, INK, 1.15);
            pen.bar(7.4, 14.2, 4.2, 1.2, INK);
            pen.bar(6.2, 15.6, 6.6, 1.0, INK);
            pen.ln(18.4, 12.0, 18.4, 5.0, 1.7, Color32::from_rgb(33, 115, 70));
            pen.arrow((18.4, 3.2), (0.0, -1.0), 3.0, Color32::from_rgb(33, 115, 70));
        }
        UpdateCheck => {
            // 检查更新：循环箭头 + 星
            pen.polyline(&[(7.0, 15.0), (4.0, 11.0), (9.6, 10.4)], 1.6, Color32::from_rgb(33, 115, 70));
            pen.polyline(&[(17.0, 9.0), (20.0, 13.0), (14.4, 13.6)], 1.6, Color32::from_rgb(33, 115, 70));
            pen.polyline(&[(6.4, 8.4), (9.2, 3.6), (14.8, 3.6), (17.6, 8.4)], 1.5, GRAY_D);
            pen.polyline(&[(6.4, 15.6), (9.2, 20.4), (14.8, 20.4), (17.6, 15.6)], 1.5, GRAY_D);
        }
        FormatCells => {
            // 设置单元格格式：格子 + 小滚筒
            pen.rrs(3.0, 5.0, 9.6, 8.4, 0.4, WHITE, INK, 1.1);
            pen.bar(3.2, 8.4, 9.2, 0.8, GRAY_D);
            pen.rr(12.2, 4.0, 6.4, 3.6, 0.6, Color32::from_rgb(255, 217, 102));
            pen.rro(12.2, 4.0, 6.4, 3.6, 0.6, INK, 1.05);
            pen.ln(15.4, 7.6, 15.4, 11.4, 1.4, INK);
            pen.rr(14.4, 11.4, 2.0, 4.6, 0.8, Color32::from_rgb(255, 217, 102));
            pen.rro(14.4, 11.4, 2.0, 4.6, 0.8, INK, 1.0);
        }
        TextVerticalDraw => {
            // 竖排文字：竖着的 A 和 b
            pen.text("A", 8.6, 12.0, 10.0, INK);
            pen.ln(16.0, 6.0, 16.0, 19.0, 1.6, GRAY_D);
            pen.ln(16.0, 6.0, 19.0, 6.0, 1.6, GRAY_D);
            pen.ln(16.0, 12.4, 19.0, 12.4, 1.6, GRAY_D);
            pen.ln(16.0, 19.0, 19.0, 19.0, 1.6, GRAY_D);
            pen.ln(19.0, 6.0, 19.0, 19.0, 1.6, GRAY_D);
        }
        TextBoxDraw => {
            // 绘制文本框：框 + 铅笔
            pen.rro(2.8, 5.0, 13.6, 10.4, 0.6, INK, 1.15);
            pen.bar(5.0, 8.4, 9.2, 1.0, GRAY_D);
            pen.bar(5.0, 11.2, 6.4, 1.0, GRAY_D);
            pen.poly(&[(15.4, 16.4), (19.6, 12.2), (21.8, 14.4), (17.6, 18.6)], Color32::from_rgb(255, 217, 102), INK, 1.0);
            pen.poly(&[(15.4, 16.4), (14.8, 19.4), (17.6, 18.6)], INK, INK, 0.0);
        }
        CalcSheet => {
            // 计算工作表：格子 + 计算器
            pen.rrs(2.6, 4.2, 9.6, 8.6, 0.4, WHITE, INK, 1.1);
            pen.bar(2.8, 7.4, 9.2, 0.8, GRAY_D);
            pen.bar(7.0, 4.4, 0.8, 8.2, GRAY_D);
            pen.rrs(13.2, 9.6, 8.0, 10.4, 0.8, Color32::from_rgb(243, 242, 241), INK, 1.1);
            pen.rr(14.4, 11.0, 5.6, 2.2, 0.3, Color32::from_rgb(255, 255, 255));
            pen.rro(14.4, 11.0, 5.6, 2.2, 0.3, GRAY_D, 0.8);
            pen.dot(15.4, 16.0, 0.7, INK);
            pen.dot(17.4, 16.0, 0.7, INK);
            pen.dot(19.4, 16.0, 0.7, INK);
        }
        SheetRef => {
            // 从另一张表取数：两张小表 + 箭头
            pen.rro(2.6, 4.0, 7.6, 7.0, 0.4, INK, 1.0);
            pen.rro(9.0, 12.0, 7.6, 7.0, 0.4, INK, 1.0);
            pen.ln(11.6, 9.2, 11.6, 12.6, 1.4, Color32::from_rgb(33, 115, 70));
            pen.arrow((11.6, 14.2), (0.0, 1.0), 2.6, Color32::from_rgb(33, 115, 70));
        }
        FilterReapply => {
            // 重新应用筛选：漏斗 + 循环箭头
            pen.poly(&[(2.6, 4.2), (14.0, 4.2), (9.6, 9.6), (9.6, 15.4), (6.8, 13.4), (6.8, 9.6)], Color32::from_rgb(243, 242, 241), INK, 1.05);
            pen.polyline(&[(15.2, 16.4), (19.4, 14.2), (19.4, 18.2), (15.6, 18.2)], 1.5, Color32::from_rgb(33, 115, 70));
            pen.arrow((17.0, 19.8), (-0.8, -0.6), 2.4, Color32::from_rgb(33, 115, 70));
        }
        SortDialog => {
            // 排序对话框：AZ 两条 + 滑块
            pen.text("A", 5.6, 8.6, 8.4, INK);
            pen.text("Z", 12.4, 8.6, 8.4, INK);
            pen.bar(4.6, 14.6, 12.0, 1.2, GRAY_D);
            pen.bar(4.6, 17.4, 8.0, 1.2, GRAY_D);
            pen.rr(17.2, 12.6, 2.4, 6.4, 0.6, Color32::from_rgb(217, 217, 217));
            pen.rr(17.6, 13.4, 1.6, 2.2, 0.4, INK);
        }
        ShowChanges => {
            // 显示更改：列表 + 眼睛
            pen.rrs(3.0, 3.6, 11.0, 13.6, 0.5, WHITE, INK, 1.1);
            pen.bar(5.0, 6.4, 7.0, 1.0, GRAY_D);
            pen.bar(5.0, 9.2, 7.0, 1.0, GRAY_D);
            pen.bar(5.0, 12.0, 4.6, 1.0, GRAY_D);
            pen.ring(16.6, 16.8, 3.0, 1.3, INK);
            pen.dot(16.6, 16.8, 1.0, INK);
        }
        SheetViewSwitch => {
            // 切换工作表视图：页签 + 眼睛
            pen.rro(2.8, 4.0, 17.2, 11.2, 0.5, INK, 1.1);
            pen.bar(2.8, 15.2, 17.2, 1.4, INK);
            pen.bar(4.6, 17.4, 4.6, 2.0, INK);
            pen.bar(10.4, 17.4, 4.6, 2.0, INK);
            pen.ring(18.6, 9.4, 3.0, 1.3, Color32::from_rgb(33, 115, 70));
            pen.dot(18.6, 9.4, 1.0, Color32::from_rgb(33, 115, 70));
        }
        Zoom100 => {
            // 100%：放大镜 + 里面一个方框（原始尺寸）
            pen.ring(10.4, 10.4, 5.6, 1.8, INK);
            pen.ln(14.6, 14.6, 20.0, 20.0, 2.2, INK);
            pen.rro(7.8, 7.8, 5.2, 5.2, 0.3, GRAY_D, 0.9);
        }
        ZoomDialog => {
            // 缩放对话框：放大镜 + 加减
            pen.ring(10.4, 10.4, 5.2, 1.8, INK);
            pen.ln(14.2, 14.2, 19.4, 19.4, 2.1, INK);
            pen.bar(7.4, 9.9, 4.0, 1.0, GRAY_D);
            pen.bar(8.9, 8.4, 1.0, 1.0, GRAY_D);
            pen.dot(16.4, 6.0, 1.2, Color32::from_rgb(33, 115, 70));
            pen.dot(19.6, 9.2, 1.2, Color32::from_rgb(33, 115, 70));
        }
        ZoomIn => {
            pen.ring(10.4, 10.4, 5.4, 1.8, INK);
            pen.ln(14.4, 14.4, 19.8, 19.8, 2.2, INK);
            pen.bar(7.4, 9.9, 4.4, 1.0, GRAY_D);
            pen.bar(9.1, 8.2, 1.0, 4.4, GRAY_D);
        }
        Unfreeze => {
            // 取消冻结：冻结格 + 斜杠
            pen.rrs(3.0, 3.0, 17.4, 17.4, 0.7, WHITE, GRAY_D, 1.2);
            pen.rr(3.4, 3.4, 16.6, 4.6, 0.0, Color32::from_rgb(237, 237, 237));
            pen.bar(3.4, 8.4, 16.6, 1.1, GRAY_D);
            pen.ln(3.4, 20.2, 20.4, 3.4, 2.0, Color32::from_rgb(192, 0, 0));
        }
        TableToRange => {
            // 转换为区域：网格 + 向外箭头
            pen.rrs(4.0, 6.0, 12.0, 12.0, 0.5, WHITE, INK, 1.15);
            pen.bar(4.2, 10.0, 11.6, 0.9, GRAY_D);
            pen.bar(10.0, 6.2, 0.9, 11.6, GRAY_D);
            pen.arrow((19.4, 2.6), (0.7, -0.7), 3.0, Color32::from_rgb(33, 115, 70));
            pen.arrow((19.4, 21.4), (0.7, 0.7), 3.0, Color32::from_rgb(33, 115, 70));
        }
        SparkEdit => {
            // 编辑迷你图数据：迷你折线 + 铅笔
            pen.polyline(&[(2.8, 12.4), (6.2, 8.4), (9.0, 12.0), (12.2, 6.8)], 1.6, Color32::from_rgb(33, 115, 70));
            pen.ln(2.6, 15.4, 12.6, 15.4, 1.0, GRAY_D);
            pen.poly(&[(14.4, 18.4), (18.6, 14.2), (20.8, 16.4), (16.6, 20.6)], Color32::from_rgb(255, 217, 102), INK, 1.0);
            pen.poly(&[(14.4, 18.4), (13.8, 21.4), (16.6, 20.6)], INK, INK, 0.0);
        }
        ChartEdit => {
            // 编辑图表：小柱图 + 铅笔
            pen.bar(2.8, 12.0, 2.4, 6.0, Color32::from_rgb(33, 115, 70));
            pen.bar(6.0, 8.8, 2.4, 9.2, Color32::from_rgb(91, 127, 166));
            pen.bar(9.2, 10.6, 2.4, 7.4, Color32::from_rgb(165, 165, 165));
            pen.ln(2.6, 18.8, 12.2, 18.8, 1.1, GRAY_D);
            pen.poly(&[(14.6, 18.2), (18.8, 14.0), (21.0, 16.2), (16.8, 20.4)], Color32::from_rgb(255, 217, 102), INK, 1.0);
            pen.poly(&[(14.6, 18.2), (14.0, 21.2), (16.8, 20.4)], INK, INK, 0.0);
        }
        PivotRefresh => {
            // 刷新透视表：小网格 + 循环箭头
            pen.rrs(3.0, 4.4, 11.0, 9.2, 0.6, Color32::from_rgb(243, 242, 241), INK, 1.1);
            pen.bar(3.4, 7.4, 10.2, 0.9, INK);
            pen.bar(7.4, 4.8, 0.9, 8.4, INK);
            pen.polyline(&[(17.0, 15.4), (20.4, 13.2), (20.4, 17.6), (16.6, 17.6)], 1.6, Color32::from_rgb(33, 115, 70));
            pen.arrow((18.6, 19.0), (-0.8, -0.6), 2.6, Color32::from_rgb(33, 115, 70));
        }
        PivotOptions => {
            // 透视表选项：小网格 + 齿轮
            pen.rrs(3.0, 4.4, 10.4, 9.2, 0.6, Color32::from_rgb(243, 242, 241), INK, 1.1);
            pen.bar(3.4, 7.4, 9.6, 0.9, INK);
            pen.bar(7.0, 4.8, 0.9, 8.4, INK);
            pen.ring(17.4, 12.0, 3.4, 1.4, INK);
            pen.dot(17.4, 12.0, 1.0, INK);
            for k in 0..4 {
                let a = k as f32 * std::f32::consts::FRAC_PI_2;
                let (s, c) = (a.sin(), a.cos());
                pen.ln(17.4 + c * 3.6, 12.0 + s * 3.6, 17.4 + c * 5.2, 12.0 + s * 5.2, 1.6, INK);
            }
        }
        ChartList => {
            // 图表管理：一张小柱图 + 右侧列表线
            pen.bar(3.0, 12.4, 2.6, 6.0, Color32::from_rgb(33, 115, 70));
            pen.bar(6.4, 8.8, 2.6, 9.6, Color32::from_rgb(91, 127, 166));
            pen.bar(9.8, 10.8, 2.6, 7.6, Color32::from_rgb(165, 165, 165));
            pen.ln(2.6, 19.2, 12.8, 19.2, 1.2, GRAY_D);
            pen.bar(15.0, 8.6, 6.6, 1.4, INK);
            pen.bar(15.0, 12.4, 6.6, 1.4, GRAY_D);
            pen.bar(15.0, 16.2, 6.6, 1.4, GRAY_D);
        }
        Spelling => {
            pen.text("A", 8.6, 10.4, 12.6, INK);
            pen.polyline(&[(12.2, 17.6), (15.2, 20.6), (21.4, 12.6)], 2.0, Color32::from_rgb(33, 115, 70));
        }
        CommentPrev => {
            pen.rrs(9.0, 6.0, 11.6, 8.6, 0.8, Color32::from_rgb(255, 246, 214), Color32::from_rgb(191, 144, 0), 1.0);
            pen.poly(&[(10.4, 14.4), (10.4, 18.4), (14.2, 14.4)], Color32::from_rgb(255, 246, 214), Color32::from_rgb(191, 144, 0), 0.9);
            pen.bar(12.6, 8.8, 6.2, 1.0, Color32::from_rgb(191, 144, 0));
            pen.bar(12.6, 11.4, 4.6, 1.0, Color32::from_rgb(191, 144, 0));
            pen.poly(&[(7.6, 7.6), (7.6, 14.4), (2.6, 11.0)], INK, INK, 0.0);
        }
        CommentNext => {
            pen.rrs(3.4, 6.0, 11.6, 8.6, 0.8, Color32::from_rgb(255, 246, 214), Color32::from_rgb(191, 144, 0), 1.0);
            pen.poly(&[(11.4, 14.4), (11.4, 18.4), (7.6, 14.4)], Color32::from_rgb(255, 246, 214), Color32::from_rgb(191, 144, 0), 0.9);
            pen.bar(5.2, 8.8, 6.2, 1.0, Color32::from_rgb(191, 144, 0));
            pen.bar(5.2, 11.4, 4.6, 1.0, Color32::from_rgb(191, 144, 0));
            pen.poly(&[(16.4, 7.6), (16.4, 14.4), (21.4, 11.0)], INK, INK, 0.0);
        }
        ShowComment => {
            pen.rrs(3.0, 4.2, 13.2, 9.4, 0.8, Color32::from_rgb(255, 246, 214), Color32::from_rgb(191, 144, 0), 1.0);
            pen.poly(&[(4.4, 13.4), (4.4, 17.4), (8.2, 13.4)], Color32::from_rgb(255, 246, 214), Color32::from_rgb(191, 144, 0), 0.9);
            pen.bar(5.4, 7.0, 8.2, 1.0, Color32::from_rgb(191, 144, 0));
            pen.ring(17.4, 17.0, 3.2, 1.3, INK);
            pen.dot(17.4, 17.0, 1.1, INK);
        }
        A11y => {
            // 辅助功能：人 + 对勾
            pen.ring(9.0, 5.4, 2.2, 1.4, INK);
            pen.polyline(&[(4.6, 18.6), (4.6, 13.4), (9.0, 11.4), (13.4, 13.4), (13.4, 18.6)], 1.5, INK);
            pen.polyline(&[(13.4, 15.4), (15.6, 17.8), (20.8, 11.4)], 1.9, Color32::from_rgb(33, 115, 70));
        }
        Online => {
            // 在线：人 + 信号弧
            pen.ring(8.4, 8.0, 2.4, 1.4, INK);
            pen.polyline(&[(3.8, 19.2), (3.8, 15.0), (8.4, 12.8), (13.0, 15.0), (13.0, 19.2)], 1.5, INK);
            pen.polyline(&[(15.4, 13.6), (17.4, 11.6)], 1.4, Color32::from_rgb(33, 115, 70));
            pen.polyline(&[(15.4, 9.4), (18.6, 6.2)], 1.4, Color32::from_rgb(33, 115, 70));
            pen.dot(16.6, 16.4, 1.0, Color32::from_rgb(33, 115, 70));
        }
        AutoShare => {
            // 自动共享：三点连线 + 循环箭头
            pen.bar(8.0, 10.6, 6.0, 1.6, Color32::from_rgb(68, 114, 196));
            pen.bar(8.0, 13.0, 6.0, 1.6, Color32::from_rgb(68, 114, 196));
            pen.dot(16.4, 6.2, 2.2, Color32::from_rgb(68, 114, 196));
            pen.dot(6.2, 12.0, 2.2, Color32::from_rgb(68, 114, 196));
            pen.dot(16.4, 17.8, 2.2, Color32::from_rgb(68, 114, 196));
            pen.polyline(&[(13.6, 20.6), (10.4, 17.8), (13.6, 15.0)], 1.5, Color32::from_rgb(33, 115, 70));
        }
        Help => {
            pen.ring(12.0, 12.0, 8.4, 1.5, INK);
            pen.text("?", 12.0, 12.4, 11.0, INK);
        }
        About => {
            pen.ring(12.0, 12.0, 8.4, 1.5, INK);
            pen.text("i", 12.0, 12.2, 11.0, INK);
        }
        Orientation => {
            // 纸张方向：竖纸 + 旋转箭头
            pen.rrs(5.0, 3.6, 9.6, 13.6, 0.5, WHITE, INK, 1.15);
            pen.bar(6.6, 7.0, 6.4, 0.9, GRAY_D);
            pen.bar(6.6, 9.8, 6.4, 0.9, GRAY_D);
            pen.polyline(&[(16.0, 15.6), (19.6, 13.4), (21.0, 9.4), (19.2, 5.6)], 1.5, Color32::from_rgb(33, 115, 70));
            pen.arrow((17.4, 4.4), (-0.6, -0.8), 2.8, Color32::from_rgb(33, 115, 70));
        }
        PaperSize => {
            // 纸张大小：纸 + 尺寸标注
            pen.rrs(4.0, 4.6, 12.0, 15.0, 0.5, WHITE, INK, 1.15);
            pen.bar(4.0, 1.8, 12.0, 1.0, GRAY_D);
            pen.bar(4.0, 1.8, 1.0, 2.2, GRAY_D);
            pen.bar(15.0, 1.8, 1.0, 2.2, GRAY_D);
            pen.bar(20.0, 4.6, 1.0, 15.0, GRAY_D);
            pen.bar(18.4, 4.6, 2.2, 1.0, GRAY_D);
            pen.bar(18.4, 18.6, 2.2, 1.0, GRAY_D);
        }
        PrintGrid => {
            // 打印网格线：打印机 + 纸上的网格
            pen.rrs(3.0, 11.0, 15.0, 7.0, 0.8, Color32::from_rgb(237, 237, 237), INK, 1.15);
            pen.rrs(4.6, 5.2, 11.8, 7.4, 0.4, WHITE, INK, 1.0);
            pen.bar(6.2, 8.6, 8.6, 0.8, GRAY_D);
            pen.bar(10.0, 5.4, 0.8, 7.0, GRAY_D);
            pen.bar(6.2, 10.8, 8.6, 0.8, GRAY_D);
        }
        PrintHeadings => {
            // 打印行号列标：打印机 + 纸上的表头带
            pen.rrs(3.0, 11.0, 15.0, 7.0, 0.8, Color32::from_rgb(237, 237, 237), INK, 1.15);
            pen.rrs(4.6, 5.2, 11.8, 7.4, 0.4, WHITE, INK, 1.0);
            pen.rr(4.8, 5.4, 11.4, 1.8, 0.0, Color32::from_rgb(191, 191, 191));
            pen.rr(4.8, 5.4, 1.8, 7.0, 0.0, Color32::from_rgb(191, 191, 191));
            pen.bar(6.8, 8.2, 9.2, 0.8, GRAY_D);
        }
        PrintArea => {
            // 打印区域：纸 + 虚线框
            pen.rrs(4.4, 2.8, 15.0, 18.0, 0.5, WHITE, INK, 1.15);
            for k in 0..4 {
                pen.bar(6.6 + k as f32 * 2.6, 6.4, 1.5, 0.9, Color32::from_rgb(33, 115, 70));
                pen.bar(6.6 + k as f32 * 2.6, 16.6, 1.5, 0.9, Color32::from_rgb(33, 115, 70));
            }
            for k in 0..4 {
                pen.bar(6.6, 8.6 + k as f32 * 2.2, 0.9, 1.4, Color32::from_rgb(33, 115, 70));
                pen.bar(17.6, 8.6 + k as f32 * 2.2, 0.9, 1.4, Color32::from_rgb(33, 115, 70));
            }
        }
        FormulaView => {
            // 显示公式：小网格 + fx
            pen.rrs(2.8, 4.0, 18.4, 14.0, 0.6, WHITE, INK, 1.15);
            pen.bar(3.0, 8.4, 18.0, 0.9, Color32::from_rgb(191, 191, 191));
            pen.bar(9.4, 4.2, 0.9, 13.6, Color32::from_rgb(191, 191, 191));
            // UI-29.2：原来用 8 号字画 "fx"，行高约 25px（32px 档），顶边跑到框外
            // 2.5px；改成矢量笔画，和网格线同粗，稳定留在 0..24。
            let fx = Color32::from_rgb(33, 115, 70);
            pen.polyline(&[(16.2, 17.4), (16.2, 11.9), (17.4, 11.2), (18.2, 11.7)], 1.3, fx);
            pen.ln(14.8, 13.6, 19.4, 13.6, 1.15, fx);
            pen.polyline(&[(20.1, 11.9), (23.4, 17.4)], 1.3, fx);
            pen.polyline(&[(23.4, 11.9), (20.1, 17.4)], 1.3, fx);
        }
        ErrorCheck => {
            // 错误检查：格子右上角红三角 + 一个对勾
            pen.rrs(3.0, 4.0, 12.6, 11.0, 0.5, WHITE, INK, 1.15);
            pen.poly(&[(15.6, 4.0), (15.6, 9.4), (10.4, 4.0)], Color32::from_rgb(192, 0, 0), Color32::from_rgb(192, 0, 0), 0.0);
            pen.polyline(&[(10.6, 17.0), (13.4, 20.2), (20.4, 12.0)], 2.0, Color32::from_rgb(33, 115, 70));
        }
        NameTag => {
            // 名称管理器：吊牌 + 三条线
            pen.poly(&[(3.4, 8.0), (12.0, 4.4), (20.6, 8.0), (20.6, 16.6), (12.0, 20.2), (3.4, 16.6)], WHITE, INK, 1.15);
            pen.dot(7.4, 9.2, 1.2, INK);
            pen.bar(10.4, 8.6, 7.0, 1.0, GRAY_D);
            pen.bar(10.4, 11.4, 7.0, 1.0, GRAY_D);
            pen.bar(10.4, 14.2, 4.6, 1.0, GRAY_D);
        }
        WatchPane => {
            // 监视窗口：小表 + 眼睛
            pen.rrs(2.6, 3.4, 12.6, 10.0, 0.5, WHITE, INK, 1.1);
            pen.bar(2.8, 6.4, 12.2, 0.9, GRAY_D);
            pen.bar(7.6, 3.6, 0.9, 9.6, GRAY_D);
            pen.ring(16.6, 16.2, 3.4, 1.3, INK);
            pen.dot(16.6, 16.2, 1.2, INK);
        }
        Eye => {
            // 眼睛：上下两条弧 + 瞳孔
            pen.polyline(&[(2.6, 12.0), (6.0, 7.6), (12.0, 6.0), (18.0, 7.6), (21.4, 12.0)], 1.4, INK);
            pen.polyline(&[(2.6, 12.0), (6.0, 16.4), (12.0, 18.0), (18.0, 16.4), (21.4, 12.0)], 1.4, INK);
            pen.ring(12.0, 12.0, 3.0, 1.5, INK);
        }
        EyeOff => {
            pen.polyline(&[(2.6, 12.0), (6.0, 7.6), (12.0, 6.0), (18.0, 7.6), (21.4, 12.0)], 1.4, GRAY_D);
            pen.polyline(&[(2.6, 12.0), (6.0, 16.4), (12.0, 18.0), (18.0, 16.4), (21.4, 12.0)], 1.4, GRAY_D);
            pen.ring(12.0, 12.0, 3.0, 1.5, GRAY_D);
            pen.ln(3.4, 20.2, 20.6, 3.8, 1.8, Color32::from_rgb(192, 0, 0));
        }
        PageLayout => {
            // 页面布局：纸上画页边距虚线 + 页眉带
            pen.rrs(4.6, 2.8, 14.8, 18.4, 0.6, WHITE, INK, 1.15);
            pen.bar(4.6, 2.8, 14.8, 2.6, Color32::from_rgb(217, 217, 217));
            for k in 0..4 {
                let y = 8.0 + k as f32 * 3.4;
                pen.ln(7.0, y, 10.0, y, 0.9, GRAY_D);
                pen.ln(13.0, y, 17.0, y, 0.9, GRAY_D);
            }
            pen.ln(6.4, 6.6, 6.4, 19.6, 0.9, GRAY_D);
            pen.ln(17.6, 6.6, 17.6, 19.6, 0.9, GRAY_D);
        }
        PageBreak => {
            // 分页预览：纸 + 一条粗分页虚线
            pen.rrs(4.6, 2.8, 14.8, 18.4, 0.6, WHITE, INK, 1.15);
            for k in 0..5 {
                pen.ln(6.4 + k as f32 * 2.6, 12.0, 7.4 + k as f32 * 2.6, 12.0, 1.4, Color32::from_rgb(33, 115, 70));
            }
            pen.bar(6.4, 8.6, 11.2, 0.9, Color32::from_rgb(191, 191, 191));
            pen.bar(6.4, 15.4, 11.2, 0.9, Color32::from_rgb(191, 191, 191));
        }
        NormalView => {
            // 普通视图：一张小网格
            pen.rrs(3.4, 4.4, 17.2, 15.2, 0.6, WHITE, INK, 1.15);
            pen.bar(3.6, 9.4, 16.8, 0.9, GRAY_D);
            pen.bar(3.6, 14.4, 16.8, 0.9, GRAY_D);
            pen.bar(9.2, 4.6, 0.9, 14.8, GRAY_D);
            pen.bar(15.0, 4.6, 0.9, 14.8, GRAY_D);
            pen.rr(3.6, 4.6, 5.4, 4.6, 0.0, Color32::from_rgb(226, 239, 218));
        }
        ShowHeadings => {
            // 行/列标题：左上角深色条 + 网格
            pen.rrs(3.4, 4.4, 17.2, 15.2, 0.6, WHITE, INK, 1.15);
            pen.rr(3.6, 4.6, 16.8, 3.4, 0.0, Color32::from_rgb(191, 191, 191));
            pen.rr(3.6, 4.6, 3.4, 14.8, 0.0, Color32::from_rgb(191, 191, 191));
            pen.bar(7.4, 8.4, 0.9, 11.0, GRAY_D);
            pen.bar(3.6, 8.4, 16.8, 0.9, GRAY_D);
        }
        FormulaBar => {
            // 编辑栏：一条长条 + 左边 fx
            pen.rrs(2.8, 8.0, 18.4, 8.0, 0.8, WHITE, INK, 1.15);
            pen.text("fx", 5.4, 12.0, 7.4, GRAY_D);
            pen.bar(9.4, 8.2, 0.9, 7.6, Color32::from_rgb(191, 191, 191));
            pen.bar(11.4, 10.6, 8.0, 0.9, GRAY_D);
            pen.bar(11.4, 13.2, 5.6, 0.9, GRAY_D);
        }
        TextSpill => {
            // 文字溢出：左格有字，向右溢出成三条线
            pen.rro(2.8, 8.4, 7.2, 7.2, 0.4, INK, 1.15);
            pen.bar(11.6, 9.6, 9.6, 1.3, GRAY_D);
            pen.bar(11.6, 12.6, 7.4, 1.3, GRAY_D);
            pen.arrow((21.4, 10.2), (1.0, 0.0), 2.6, Color32::from_rgb(33, 115, 70));
        }
        ViewPane => {
            // 侧栏/视图：一个框 + 左侧分栏
            pen.rrs(2.8, 4.4, 18.4, 15.2, 0.8, WHITE, INK, 1.15);
            pen.bar(8.6, 4.6, 0.9, 14.8, INK);
            pen.bar(4.2, 7.2, 3.0, 0.9, GRAY_D);
            pen.bar(4.2, 10.0, 3.0, 0.9, GRAY_D);
            pen.bar(4.2, 12.8, 3.0, 0.9, GRAY_D);
            pen.bar(10.4, 7.2, 8.4, 0.9, Color32::from_rgb(191, 191, 191));
            pen.bar(10.4, 10.0, 8.4, 0.9, Color32::from_rgb(191, 191, 191));
        }
        Pin => {
            // 图钉：钉帽 + 针
            pen.rrs(9.0, 3.0, 6.0, 3.4, 0.6, Color32::from_rgb(197, 217, 241), INK, 1.0);
            pen.poly(&[(7.4, 6.4), (16.6, 6.4), (14.6, 12.4), (9.4, 12.4)], Color32::from_rgb(197, 217, 241), INK, 1.05);
            pen.ln(12.0, 12.4, 12.0, 21.0, 1.6, INK);
        }
        NewView => {
            // 新建视图：两个重叠窗口 + 加号
            pen.rrs(3.0, 4.6, 12.4, 12.4, 0.6, WHITE, GRAY_D, 1.0);
            pen.rrs(8.0, 8.0, 12.4, 12.4, 0.6, WHITE, INK, 1.15);
            pen.bar(13.6, 18.4, 5.6, 1.6, Color32::from_rgb(33, 115, 70));
            pen.bar(15.6, 16.4, 1.6, 5.6, Color32::from_rgb(33, 115, 70));
        }
        ZoomSelection => {
            // 缩放到选区：放大镜里有虚线选框
            pen.ring(10.4, 10.4, 5.6, 1.8, INK);
            pen.ln(14.6, 14.6, 20.0, 20.0, 2.2, INK);
            pen.rro(7.6, 8.0, 5.6, 4.8, 0.3, GRAY_D, 0.9);
        }
        GoalSeek => {
            // 单变量求解：靶心 + 射中的箭头
            pen.ring(11.0, 11.4, 6.4, 1.3, INK);
            pen.ring(11.0, 11.4, 3.2, 1.3, INK);
            pen.dot(11.0, 11.4, 1.2, INK);
            pen.ln(3.0, 3.4, 9.4, 9.2, 1.4, GRAY_D);
            pen.arrow((11.0, 11.4), (0.62, 0.62), 3.2, GRAY_D);
        }
        Scenario => {
            // 方案管理器：表格 + 右上角书签
            pen.rro(3.0, 6.0, 13.0, 12.0, 0.5, INK, 1.15);
            pen.bar(3.2, 9.4, 12.6, 0.9, GRAY_D);
            pen.bar(8.2, 6.2, 0.9, 11.6, GRAY_D);
            pen.poly(&[(17.0, 3.4), (21.4, 3.4), (21.4, 12.2), (19.2, 9.8), (17.0, 12.2)], Color32::from_rgb(255, 217, 102), INK, 1.1);
        }
        GroupRows => {
            // 组合：方括号 + 加号（不带行线，和「显示明细」区分开）
            pen.ln(5.2, 4.2, 5.2, 19.8, 1.8, INK);
            pen.ln(5.2, 4.2, 8.4, 4.2, 1.8, INK);
            pen.ln(5.2, 19.8, 8.4, 19.8, 1.8, INK);
            pen.bar(11.0, 11.1, 8.0, 1.9, Color32::from_rgb(33, 115, 70));
            pen.bar(14.05, 8.05, 1.9, 8.0, Color32::from_rgb(33, 115, 70));
        }
        UngroupRows => {
            pen.ln(5.2, 4.2, 5.2, 19.8, 1.8, INK);
            pen.ln(5.2, 4.2, 8.4, 4.2, 1.8, INK);
            pen.ln(5.2, 19.8, 8.4, 19.8, 1.8, INK);
            pen.bar(11.0, 11.1, 8.0, 1.9, Color32::from_rgb(33, 115, 70));
        }
        ShowDetail => {
            // 展开明细：行 + 向下的三角（展开），比「+」不容易和插入行混淆
            for i in 0..3 {
                pen.bar(10.4, 7.0 + i as f32 * 4.0, 9.8, 1.6, GRAY_D);
            }
            pen.poly(&[(2.4, 9.2), (7.6, 9.2), (5.0, 14.0)], INK, INK, 0.0);
        }
        HideDetail => {
            // 折叠明细：行 + 向右的三角
            for i in 0..3 {
                pen.bar(10.4, 7.0 + i as f32 * 4.0, 9.8, 1.6, GRAY_D);
            }
            pen.poly(&[(3.0, 8.6), (3.0, 15.0), (7.8, 11.8)], INK, INK, 0.0);
        }
        Validation => {
            // 单元格 + 右侧下拉三角 + 右下角对勾
            pen.rrs(3.2, 4.2, 17.6, 10.4, 0.6, WHITE, INK, 1.15);
            pen.bar(15.6, 8.2, 1.0, 6.4, Color32::from_rgb(191, 191, 191));
            pen.poly(
                &[(16.6, 9.6), (19.4, 9.6), (18.0, 11.6)],
                GRAY_D,
                GRAY_D,
                0.0,
            );
            pen.polyline(&[(13.2, 17.0), (15.4, 19.4), (20.4, 14.0)], 2.0, Color32::from_rgb(33, 115, 70));
        }
        SplitColumns => {
            // 分列：两个色块 + 中间一条加粗分界线，16px 下最清楚
            pen.rr(3.0, 4.4, 7.2, 15.2, 0.3, Color32::from_rgb(217, 217, 217));
            pen.rro(3.0, 4.4, 7.2, 15.2, 0.3, INK, 1.15);
            pen.rro(13.8, 4.4, 7.2, 15.2, 0.3, INK, 1.15);
            pen.bar(11.5, 3.4, 1.0, 17.2, INK);
            pen.arrow((12.0, 2.0), (0.0, -1.0), 2.4, INK);
            pen.arrow((12.0, 22.0), (0.0, 1.0), 2.4, INK);
        }
        RemoveDup => {
            // 三行，最后一行划掉（去重）
            for i in 0..3 {
                pen.bar(4.0, 6.0 + i as f32 * 4.6, 15.0, 1.6, if i == 2 { GRAY_D } else { INK });
            }
            pen.ln(3.4, 16.4, 11.4, 9.4, 1.5, Color32::from_rgb(192, 0, 0));
        }
        Consolidate => {
            // 三张来源表 → 合到一张（右）
            for (k, y) in [3.6_f32, 9.4, 15.2].iter().enumerate() {
                let _ = k;
                pen.rro(2.8, *y, 6.4, 5.2, 0.4, GRAY_D, 1.0);
            }
            pen.rro(14.6, 6.4, 7.0, 11.2, 0.5, INK, 1.15);
            pen.bar(16.2, 9.0, 3.8, 0.9, GRAY_D);
            pen.bar(16.2, 12.2, 3.8, 0.9, GRAY_D);
            pen.bar(16.2, 15.4, 2.6, 0.9, GRAY_D);
            pen.ln(9.8, 8.6, 13.4, 11.0, 1.1, INK);
            pen.ln(9.8, 12.0, 13.4, 12.0, 1.1, INK);
            pen.ln(9.8, 15.4, 13.4, 13.0, 1.1, INK);
        }
        ImportCsv => {
            // 文件 + 向下的绿色导入箭头
            pen.rrs(4.0, 3.4, 12.2, 17.2, 0.5, WHITE, INK, 1.15);
            pen.bar(6.2, 7.4, 7.8, 1.1, GRAY_D);
            pen.bar(6.2, 10.2, 7.8, 1.1, GRAY_D);
            pen.bar(6.2, 13.0, 5.4, 1.1, GRAY_D);
            pen.ln(17.6, 9.0, 17.6, 17.4, 2.0, Color32::from_rgb(33, 115, 70));
            pen.arrow((17.6, 19.6), (0.0, 1.0), 3.0, Color32::from_rgb(33, 115, 70));
        }
        ImportBook => {
            // 小网格文件 + 导入箭头
            pen.rrs(3.6, 3.6, 12.6, 14.0, 0.5, WHITE, INK, 1.15);
            pen.bar(3.8, 7.0, 12.2, 0.9, GRAY_D);
            pen.bar(3.8, 10.4, 12.2, 0.9, GRAY_D);
            pen.bar(9.2, 3.8, 0.9, 13.6, GRAY_D);
            pen.ln(18.4, 9.0, 18.4, 17.4, 2.0, Color32::from_rgb(33, 115, 70));
            pen.arrow((18.4, 19.6), (0.0, 1.0), 3.0, Color32::from_rgb(33, 115, 70));
        }
        WhatIf => {
            // 模拟分析：一张小表 + 上下箭头
            pen.rro(3.4, 8.4, 12.4, 9.6, 0.4, INK, 1.1);
            pen.bar(3.6, 11.4, 12.0, 0.9, GRAY_D);
            pen.bar(7.8, 8.6, 0.9, 9.2, GRAY_D);
            pen.ln(18.6, 6.4, 18.6, 16.4, 1.6, GRAY_D);
            pen.arrow((18.6, 4.8), (0.0, -1.0), 2.6, GRAY_D);
            pen.arrow((18.6, 18.0), (0.0, 1.0), 2.6, GRAY_D);
        }
        OutlineLevels => {
            // 分级显示：三行不同缩进 + 左侧级别点
            for i in 0..3 {
                pen.bar(6.0 + i as f32 * 2.4, 6.4 + i as f32 * 4.4, 14.0 - i as f32 * 2.4, 1.5, INK);
                pen.dot(4.2 + i as f32 * 0.0, 7.1 + i as f32 * 4.4, 1.0, GRAY_D);
            }
        }
        TextDir => {
            // 文字方向：A + 90° 折线旋转箭头；和字号增减的「A + 直箭头」分开
            pen.text("A", 8.4, 8.2, 9.6, INK);
            pen.polyline(&[(14.2, 5.8), (19.2, 5.8), (19.2, 17.2)], 1.6, GRAY_D);
            pen.arrow((19.2, 19.4), (0.0, 1.0), 3.4, GREEN_D);
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
        NewFile => {
            // 空白页 + 右下绿色加号：文件级「新建」，不是表格里的「插入行」
            pen.rrs(4.6, 2.8, 12.4, 15.4, 0.8, WHITE, GRAY_D, 1.1);
            pen.poly(
                &[(11.8, 3.0), (16.8, 8.0), (11.8, 8.0)],
                Color32::from_rgb(242, 242, 242),
                GRAY_D,
                0.9,
            );
            pen.bar(7.2, 10.6, 6.2, 1.0, GRAY);
            pen.bar(7.2, 13.4, 4.2, 1.0, GRAY);
            pen.dot(18.2, 18.2, 3.6, GREEN_D);
            pen.bar(17.5, 15.6, 1.4, 5.2, WHITE);
            pen.bar(15.6, 17.5, 5.2, 1.4, WHITE);
        }
        Macro => {
            pen.rrs(4.0, 3.4, 12.2, 15.4, 0.6, WHITE, GRAY_D, 1.1);
            pen.bar(6.4, 6.8, 7.0, 1.0, GRAY);
            pen.bar(6.4, 9.8, 5.0, 1.0, GRAY);
            pen.poly(&[(15.0, 14.4), (15.0, 21.2), (21.2, 17.8)], GREEN_D, GREEN_D, 0.6);
        }
        MacroRun => {
            pen.ring(12.0, 12.0, 8.8, 1.6, GREEN_D);
            pen.poly(&[(9.8, 7.8), (9.8, 16.2), (16.8, 12.0)], GREEN_D, GREEN_D, 0.6);
        }
        Code => {
            pen.rrs(3.0, 4.4, 18.0, 15.2, 0.7, WHITE, GRAY_D, 1.1);
            pen.bar(3.3, 4.5, 17.4, 2.6, Color32::from_rgb(237, 237, 237));
            pen.polyline(&[(9.2, 10.4), (6.8, 12.8), (9.2, 15.2)], 1.5, INK);
            pen.polyline(&[(14.8, 10.4), (17.2, 12.8), (14.8, 15.2)], 1.5, INK);
            pen.ln(13.4, 9.4, 10.8, 16.2, 1.4, GREEN_D);
        }
        Compare => {
            pen.rro(3.0, 4.4, 7.4, 15.2, 0.5, GRAY_D, 1.0);
            pen.rro(13.6, 4.4, 7.4, 15.2, 0.5, GRAY_D, 1.0);
            pen.ln(8.4, 8.6, 11.4, 8.6, 1.4, GREEN_D);
            pen.arrow((12.6, 8.6), (1.0, 0.0), 2.2, GREEN_D);
            pen.ln(15.6, 15.4, 12.6, 15.4, 1.4, GREEN_D);
            pen.arrow((11.4, 15.4), (-1.0, 0.0), 2.2, GREEN_D);
        }
        SyncScroll => {
            pen.rro(3.0, 4.0, 7.2, 16.0, 0.5, GRAY_D, 1.0);
            pen.rro(13.8, 4.0, 7.2, 16.0, 0.5, GRAY_D, 1.0);
            pen.bar(11.5, 7.0, 1.0, 10.0, GREEN_D);
            pen.arrow((12.0, 5.4), (0.0, -1.0), 2.8, GREEN_D);
            pen.arrow((12.0, 18.6), (0.0, 1.0), 2.8, GREEN_D);
        }
        InvalidCircle => {
            pen.rro(4.2, 4.2, 13.0, 13.0, 0.6, GRAY, 1.0);
            pen.ring(12.0, 12.0, 7.6, 1.8, RED);
            pen.bar(11.2, 7.8, 1.6, 6.0, RED);
            pen.dot(12.0, 16.2, 1.1, RED);
        }
    }
}

#[cfg(test)]
mod stroke_tests {
    use super::*;

    /// 任何 DPI 下，线条的设备宽度都必须是整数（否则抗锯齿把线糊成两像素）
    #[test]
    fn stroke_width_is_integral_device_pixels() {
        for ppp in [1.0_f32, 1.25, 1.5, 2.0, 2.5] {
            for w in [0.5_f32, 0.6, 0.85, 1.0, 1.15, 1.5, 1.8, 2.2, 3.0] {
                let d = dev_stroke(w, ppp);
                assert!(d >= 1.0, "w={w} ppp={ppp} → {d}");
                assert!((d - d.round()).abs() < 1e-6, "线宽不是整数设备像素: w={w} ppp={ppp} → {d}");
            }
        }
    }

    /// 对齐后：奇数宽落半像素、偶数宽落整像素
    #[test]
    fn line_centers_land_on_pixel_centers() {
        for ppp in [1.0_f32, 1.25, 1.5, 2.0] {
            for wdev in [1.0_f32, 2.0, 3.0] {
                for v in [10.0_f32, 10.3, 10.5, 10.77, 12.2] {
                    let dev = snap_center(v, ppp, wdev) * ppp;
                    let err = if wdev as i32 & 1 == 1 { (dev - dev.floor() - 0.5).abs() } else { dev - dev.round() };
                    assert!(err < 1e-4, "v={v} ppp={ppp} wdev={wdev} → 设备坐标 {dev} 没落在像素中心");
                }
            }
        }
    }

    /// 32px 图标：100% 下 1.15 单位的线落到 1 设备像素，150% 下落到 2
    #[test]
    fn thirty_two_px_icon_scales_to_integer_device_pixels() {
        let s = 32.0 / 24.0;
        assert_eq!(dev_stroke(1.15 * s, 1.0), 1.0);
        assert_eq!(dev_stroke(1.15 * s, 1.5), 2.0);
        assert_eq!(dev_stroke(1.15 * s, 2.0), 3.0);
    }
}
