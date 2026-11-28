//! Modern Fluent-2 / Lucide-grade icon system for CELLAN.
//!
//! All 165 icons are pre-rasterized from clean 24×24 smooth-Bezier, round-cap
//! SVGs (Lucide ISC + matching custom spreadsheet SVGs, see
//! `assets/icons/modern/LICENSE-LUCIDE.txt` and `scripts/build_modern_icons.py`)
//! into two supersampled, stroke-tuned L8 grayscale alpha atlases:
//! - 64×64 per cell (`stroke-width="1.70"`) for 20–32px ribbon & dialog icons
//! - 32×32 per cell (`stroke-width="1.95"`) for crisp 12–18px small icons
//! plus compact disjoint semantic duotone/accent overlay atlases tinted
//! dynamically at runtime from [`crate::theme::ThemePalette`] across Light,
//! EyeCare, Dark, and HighContrast themes.

mod atlas_meta;

use std::sync::{Arc, OnceLock};

use eframe::egui::{self, Color32, Painter, Pos2, Rect, TextureHandle, TextureOptions, Vec2};

use crate::theme::{AppTheme, ThemePalette};
use atlas_meta::{
    ACCENT_COLS, ACCENT_ROWS, ColorRole, ColorSpec, MONO_COLS, MONO_ROWS, color_spec, icon_index,
};

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
    /// 粘贴仅数值（123 矢量符号）
    PasteValues123,
    /// 删除线（S + 横线）
    StrikeThrough,
    /// 聚光灯（阅读模式）
    Spotlight,
    /// 拆分窗格
    SplitPanes,
    /// 高亮重复项
    HighlightDup,
    /// 拒绝录入重复项
    RejectDup,
    /// 合并相同单元格
    MergeSame,
    /// AI 助手（双四角星）
    AiSparkle,
    /// 插入单元格/行列
    InsertCell,
    /// 删除单元格/行列
    DeleteCell,
    /// 单元格样式卡片
    CellStyle,
    /// 函数分类手册
    FnBook,
    /// 转置粘贴（行列互换）
    Transpose,
    /// 粘贴链接（剪贴板 + 链接环）
    PasteLink,
    /// 粘贴/复制为图片
    PastePic,
    /// 无边框粘贴
    NoBorder,
    /// 保留源列宽
    ColWidth,
}

/// 全部图标（保持既有 163 项公开清单兼容；图集覆盖全部 165 个枚举变体）
pub const ALL_ICONS: [Icon; 163] = [
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
    Icon::InvalidCircle, Icon::Home, Icon::PasteValues123, Icon::StrikeThrough,
    Icon::Spotlight, Icon::SplitPanes, Icon::HighlightDup, Icon::RejectDup,
    Icon::MergeSame, Icon::AiSparkle, Icon::InsertCell, Icon::DeleteCell,
    Icon::CellStyle, Icon::FnBook, Icon::Transpose, Icon::PasteLink,
    Icon::PastePic, Icon::NoBorder, Icon::ColWidth,
];

/// 图标总表：给「对照表」和自检用
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

pub(crate) const RIBBON_COMMANDS: &str = "cellan_ribbon_commands";

// ---------------------------------------------------------------- Embedded Atlases

const ATLAS_MONO_64_PNG: &[u8] = include_bytes!("../../../../assets/icons/modern/atlas_64.png");
const ATLAS_MONO_32_PNG: &[u8] = include_bytes!("../../../../assets/icons/modern/atlas_32.png");
const ATLAS_ACCENT_64_PNG: &[u8] = include_bytes!("../../../../assets/icons/modern/accent_64.png");
const ATLAS_ACCENT_32_PNG: &[u8] = include_bytes!("../../../../assets/icons/modern/accent_32.png");

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum AtlasSlot {
    Mono64 = 0,
    Mono32 = 1,
    Accent64 = 2,
    Accent32 = 3,
}

impl AtlasSlot {
    fn label(self) -> &'static str {
        match self {
            Self::Mono64 => "cellan_icons_mono_64",
            Self::Mono32 => "cellan_icons_mono_32",
            Self::Accent64 => "cellan_icons_accent_64",
            Self::Accent32 => "cellan_icons_accent_32",
        }
    }

    fn decoded(self) -> &'static Arc<egui::ColorImage> {
        static MONO_64: OnceLock<Arc<egui::ColorImage>> = OnceLock::new();
        static MONO_32: OnceLock<Arc<egui::ColorImage>> = OnceLock::new();
        static ACCENT_64: OnceLock<Arc<egui::ColorImage>> = OnceLock::new();
        static ACCENT_32: OnceLock<Arc<egui::ColorImage>> = OnceLock::new();

        match self {
            Self::Mono64 => MONO_64.get_or_init(|| decode_l8_alpha_atlas(ATLAS_MONO_64_PNG)),
            Self::Mono32 => MONO_32.get_or_init(|| decode_l8_alpha_atlas(ATLAS_MONO_32_PNG)),
            Self::Accent64 => ACCENT_64.get_or_init(|| decode_l8_alpha_atlas(ATLAS_ACCENT_64_PNG)),
            Self::Accent32 => ACCENT_32.get_or_init(|| decode_l8_alpha_atlas(ATLAS_ACCENT_32_PNG)),
        }
    }
}

/// Decode a single-channel L8 grayscale alpha atlas into premultiplied white-alpha pixels `(a, a, a, a)`.
fn decode_l8_alpha_atlas(bytes: &'static [u8]) -> Arc<egui::ColorImage> {
    let luma = image::load_from_memory(bytes)
        .expect("embedded modern icon atlas PNG must decode")
        .to_luma8();
    let size = [luma.width() as usize, luma.height() as usize];
    let pixels = luma
        .as_raw()
        .iter()
        .map(|&a| Color32::from_rgba_premultiplied(a, a, a, a))
        .collect();
    Arc::new(egui::ColorImage { size, pixels })
}

fn atlas_texture_id(ctx: &egui::Context, slot: AtlasSlot) -> egui::TextureId {
    let id = egui::Id::new(("cellan_modern_icon_atlas_v1", slot as u8));
    if let Some(handle) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return handle.id();
    }
    let handle = ctx.load_texture(slot.label(), Arc::clone(slot.decoded()), TextureOptions::LINEAR);
    let tid = handle.id();
    ctx.data_mut(|d| d.insert_temp(id, handle));
    tid
}

/// Choose the 32px stroke-tuned atlas for small physical targets (<=24px) and 64px supersampled atlas above.
#[inline]
fn use_small_atlas(rect: Rect, ppp: f32) -> bool {
    let dev_px = rect.width().min(rect.height()) * ppp.max(1.0);
    dev_px <= 24.0
}

#[inline]
fn cell_uv(index: usize, cols: u32, rows: u32, cell_px: u32) -> Rect {
    let col = (index as u32) % cols;
    let row = (index as u32) / cols;
    let atlas_w = (cols * cell_px) as f32;
    let atlas_h = (rows * cell_px) as f32;
    // Half-texel inset ensures TextureOptions::LINEAR never samples across neighboring cells.
    let x0 = (col * cell_px) as f32 + 0.5;
    let y0 = (row * cell_px) as f32 + 0.5;
    let x1 = ((col + 1) * cell_px) as f32 - 0.5;
    let y1 = ((row + 1) * cell_px) as f32 - 0.5;
    Rect::from_min_max(
        Pos2::new(x0 / atlas_w, y0 / atlas_h),
        Pos2::new(x1 / atlas_w, y1 / atlas_h),
    )
}

#[inline]
fn snapped_square(rect: Rect, ppp: f32) -> Rect {
    let ppp = ppp.max(1.0);
    let raw = rect.width().min(rect.height()).max(1.0);
    let side = (raw * ppp).round().max(1.0) / ppp;
    let min_x = ((rect.center().x - side * 0.5) * ppp).round() / ppp;
    let min_y = ((rect.center().y - side * 0.5) * ppp).round() / ppp;
    Rect::from_min_size(Pos2::new(min_x, min_y), Vec2::splat(side))
}

#[inline]
fn soft_tint(rgb: Color32, alpha: u8) -> Color32 {
    let [r, g, b, _] = rgb.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, alpha)
}

#[inline]
fn resolve_role(role: ColorRole, base_text: Color32, pal: ThemePalette) -> Color32 {
    let dark = pal.theme.is_dark();
    let hc = pal.theme == AppTheme::HighContrast;
    match role {
        ColorRole::Text => base_text,
        ColorRole::TextWeak => pal.text_weak,
        ColorRole::Accent => pal.accent,
        ColorRole::Info => pal.info_fg,
        ColorRole::Warn => pal.warn_fg,
        ColorRole::Err => pal.err_fg,
        ColorRole::AccentSoft => {
            if hc {
                Color32::TRANSPARENT
            } else {
                soft_tint(pal.accent, if dark { 56 } else { 44 })
            }
        }
        ColorRole::InfoSoft => {
            if hc {
                Color32::TRANSPARENT
            } else {
                soft_tint(pal.info_fg, if dark { 52 } else { 40 })
            }
        }
        ColorRole::WarnSoft => {
            if hc {
                Color32::TRANSPARENT
            } else if dark {
                soft_tint(pal.warn_fg, 54)
            } else {
                // Warm amber duotone tint reads cleaner than muddy dark ochre on light surfaces.
                Color32::from_rgba_unmultiplied(245, 176, 36, 52)
            }
        }
        ColorRole::ErrSoft => {
            if hc {
                Color32::TRANSPARENT
            } else {
                soft_tint(pal.err_fg, if dark { 52 } else { 40 })
            }
        }
    }
}

#[inline]
fn paint_quad(p: &Painter, rect: Rect, tid: egui::TextureId, uv: Rect, tint: Color32) {
    p.add(egui::epaint::RectShape::filled(rect, 0.0, tint).with_texture(tid, uv));
}

fn paint_mono(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let ppp = p.pixels_per_point().max(1.0);
    let draw_rect = snapped_square(rect, ppp);
    let small = use_small_atlas(draw_rect, ppp);
    let (slot, cell_px) = if small {
        (AtlasSlot::Mono32, 32)
    } else {
        (AtlasSlot::Mono64, 64)
    };
    let tid = atlas_texture_id(p.ctx(), slot);
    let uv = cell_uv(icon_index(icon), MONO_COLS, MONO_ROWS, cell_px);
    paint_quad(p, draw_rect, tid, uv, color);
}

fn paint_semantic(p: &Painter, rect: Rect, icon: Icon, base_text: Color32, pal: ThemePalette) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let Some(ColorSpec {
        replace_base,
        layers,
    }) = color_spec(icon)
    else {
        paint_mono(p, rect, icon, base_text);
        return;
    };

    let ppp = p.pixels_per_point().max(1.0);
    let draw_rect = snapped_square(rect, ppp);
    let small = use_small_atlas(draw_rect, ppp);
    let (mono_slot, accent_slot, cell_px) = if small {
        (AtlasSlot::Mono32, AtlasSlot::Accent32, 32)
    } else {
        (AtlasSlot::Mono64, AtlasSlot::Accent64, 64)
    };
    let accent_tid = atlas_texture_id(p.ctx(), accent_slot);

    for layer in layers {
        let tint = resolve_role(layer.role, base_text, pal);
        if tint == Color32::TRANSPARENT {
            continue;
        }
        let uv = cell_uv(layer.cell as usize, ACCENT_COLS, ACCENT_ROWS, cell_px);
        paint_quad(p, draw_rect, accent_tid, uv, tint);
    }

    if !replace_base {
        let mono_tid = atlas_texture_id(p.ctx(), mono_slot);
        let uv = cell_uv(icon_index(icon), MONO_COLS, MONO_ROWS, cell_px);
        paint_quad(p, draw_rect, mono_tid, uv, base_text);
    }
}

/// Curated set of ribbon icons whose color carries semantic meaning inside the ribbon.
#[inline]
fn is_ribbon_semantic(icon: Icon) -> bool {
    use Icon::*;
    matches!(
        icon,
        FontColor
            | Fill
            | Chart
            | Image
            | Cf
            | CellStyle
            | InsertCell
            | DeleteCell
            | ErrorCheck
            | InvalidCircle
            | PdfExport
            | AiSparkle
            | A11y
            | Save
            | Paste
            | Open
            | FolderPlus
            | Share
            | Comment
    )
}

#[cfg(feature = "legacy_png_icons")]
fn use_bitmap() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("SC_ICONS")
            .map(|v| v.eq_ignore_ascii_case("bitmap"))
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------- Public API

/// 单色绘制：使用调用方指定颜色，严格落在 `rect` 内（状态栏、内联按钮等）。
pub fn draw(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    paint_mono(p, rect, icon, color);
}

/// 单色绘制：忽略图标内置配色，整枚用 `color`（标签条、标题栏、中性工具图标）。
pub fn draw_mono(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    paint_mono(p, rect, icon, color);
}

/// 对照表用：单色现代图标（不回落 legacy 位图）。
pub fn draw_vec(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    paint_mono(p, rect, icon, color);
}

/// 对照表用：语义彩色版（和正式界面一致）。
pub fn draw_vec_color(p: &Painter, rect: Rect, icon: Icon) {
    let pal = crate::theme::palette();
    paint_semantic(p, rect, icon, pal.text, pal);
}

/// 语义彩色模式：启用态呈现主题适配的 Fluent-2 双色/语义强调，禁用态退化为单色灰。
pub fn draw_excel(p: &Painter, rect: Rect, icon: Icon, color: Color32, enabled: bool) {
    #[cfg(feature = "legacy_png_icons")]
    if use_bitmap() && crate::ui_png::draw(p, rect, icon, enabled) {
        return;
    }
    if !enabled {
        paint_mono(p, rect, icon, color);
        return;
    }
    let pal = crate::theme::palette();
    paint_semantic(p, rect, icon, pal.text, pal);
}

/// Inside the ribbon, reserve color for commands whose color carries semantic meaning.
pub(crate) fn draw_control(ui: &egui::Ui, rect: Rect, icon: Icon, color: Color32, enabled: bool) {
    let pal = crate::theme::palette();
    if !enabled
        || pal.theme == AppTheme::HighContrast
        || !ui.stack().iter().any(|node| node.tags().contains(RIBBON_COMMANDS))
    {
        draw_excel(ui.painter(), rect, icon, color, enabled);
        return;
    }
    #[cfg(feature = "legacy_png_icons")]
    if use_bitmap() && crate::ui_png::draw(ui.painter(), rect, icon, enabled) {
        return;
    }
    if is_ribbon_semantic(icon) {
        paint_semantic(ui.painter(), rect, icon, color, pal);
    } else {
        paint_mono(ui.painter(), rect, icon, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ENUM_ICONS: [Icon; 165] = [
        Icon::Open, Icon::Save, Icon::SaveAs, Icon::Print, Icon::Undo, Icon::Redo,
        Icon::Paste, Icon::Cut, Icon::Copy, Icon::Brush, Icon::Fill, Icon::FontColor,
        Icon::AlignL, Icon::AlignC, Icon::AlignR, Icon::Top, Icon::Middle, Icon::Bottom,
        Icon::Wrap, Icon::Merge, Icon::Border, Icon::Currency, Icon::Percent, Icon::Comma,
        Icon::SortAsc, Icon::SortDesc, Icon::Filter, Icon::Find, Icon::Chart, Icon::Image,
        Icon::Table, Icon::Comment, Icon::Pivot, Icon::Link, Icon::Revision, Icon::Zoom,
        Icon::Calc, Icon::Sheet, Icon::Share, Icon::Fx, Icon::Sum, Icon::Grid,
        Icon::Home, Icon::Close, Icon::Plus, Icon::Refresh, Icon::DecInc, Icon::DecDec,
        Icon::FontGrow, Icon::FontShrink, Icon::TextDir, Icon::HeaderFooter, Icon::DocLink,
        Icon::GroupRows, Icon::UngroupRows, Icon::ShowDetail, Icon::HideDetail, Icon::Validation,
        Icon::SplitColumns, Icon::RemoveDup, Icon::Consolidate, Icon::ImportCsv, Icon::ImportBook,
        Icon::WhatIf, Icon::OutlineLevels, Icon::GoalSeek, Icon::Scenario, Icon::Eye, Icon::EyeOff,
        Icon::PageLayout, Icon::PageBreak, Icon::NormalView, Icon::ShowHeadings, Icon::FormulaBar,
        Icon::TextSpill, Icon::ViewPane, Icon::Pin, Icon::NewView, Icon::ZoomSelection, Icon::FormulaView,
        Icon::ErrorCheck, Icon::NameTag, Icon::WatchPane, Icon::Spelling, Icon::CommentPrev,
        Icon::CommentNext, Icon::ShowComment, Icon::A11y, Icon::Online, Icon::AutoShare, Icon::Help,
        Icon::About, Icon::Orientation, Icon::PaperSize, Icon::PrintGrid, Icon::PrintHeadings,
        Icon::PrintArea, Icon::PivotRefresh, Icon::PivotOptions, Icon::ChartList, Icon::FolderPlus,
        Icon::AppDefault, Icon::AppAdd, Icon::PdfExport, Icon::HostTakeover, Icon::UpdateCheck,
        Icon::FormatCells, Icon::TextVerticalDraw, Icon::TextBoxDraw, Icon::CalcSheet, Icon::SheetRef,
        Icon::FilterReapply, Icon::SortDialog, Icon::ShowChanges, Icon::SheetViewSwitch, Icon::Zoom100,
        Icon::ZoomDialog, Icon::ZoomIn, Icon::Unfreeze, Icon::TableToRange, Icon::SparkEdit,
        Icon::ChartEdit, Icon::RichText, Icon::Freeze, Icon::FreezeRow, Icon::FreezeCol, Icon::Lock,
        Icon::Cf, Icon::Eraser, Icon::ClearFmt, Icon::IndentInc, Icon::IndentDec, Icon::Symbol,
        Icon::ZhS, Icon::ZhT, Icon::TraceIn, Icon::TraceOut, Icon::SparkLine, Icon::SparkColumn,
        Icon::SparkLoss, Icon::TextBox, Icon::NewFile, Icon::Macro, Icon::MacroRun, Icon::Code,
        Icon::Compare, Icon::SyncScroll, Icon::InvalidCircle, Icon::PasteValues123, Icon::StrikeThrough,
        Icon::Spotlight, Icon::SplitPanes, Icon::HighlightDup, Icon::RejectDup, Icon::MergeSame,
        Icon::AiSparkle, Icon::InsertCell, Icon::DeleteCell, Icon::CellStyle, Icon::FnBook,
        Icon::Transpose, Icon::PasteLink, Icon::PastePic, Icon::NoBorder, Icon::ColWidth,
    ];

    #[test]
    fn public_constants_and_indices_stay_compatible() {
        assert_eq!(ALL_ICONS.len(), 163);
        assert_eq!(SHEET_ICONS.len(), 46);
        assert_eq!(RIBBON_COMMANDS, "cellan_ribbon_commands");
        let mut seen = [false; 165];
        for icon in ALL_ENUM_ICONS {
            let idx = icon_index(icon);
            assert!(idx < 165, "{icon:?} index out of range: {idx}");
            assert!(!seen[idx], "duplicate atlas index {idx} for {icon:?}");
            seen[idx] = true;
        }
        assert!(seen.iter().all(|&b| b));
    }

    #[test]
    fn embedded_atlases_decode_cleanly_within_budget_and_have_non_empty_cells() {
        let total_bytes = ATLAS_MONO_64_PNG.len()
            + ATLAS_MONO_32_PNG.len()
            + ATLAS_ACCENT_64_PNG.len()
            + ATLAS_ACCENT_32_PNG.len();
        assert!(
            total_bytes < 350 * 1024,
            "total embedded icon atlases ({total_bytes} bytes) exceeded 350 KB cap"
        );

        let m64 = AtlasSlot::Mono64.decoded();
        let m32 = AtlasSlot::Mono32.decoded();
        let a64 = AtlasSlot::Accent64.decoded();
        let a32 = AtlasSlot::Accent32.decoded();

        assert_eq!(m64.size, [(MONO_COLS * 64) as usize, (MONO_ROWS * 64) as usize]);
        assert_eq!(m32.size, [(MONO_COLS * 32) as usize, (MONO_ROWS * 32) as usize]);
        assert_eq!(a64.size, [(ACCENT_COLS * 64) as usize, (ACCENT_ROWS * 64) as usize]);
        assert_eq!(a32.size, [(ACCENT_COLS * 32) as usize, (ACCENT_ROWS * 32) as usize]);

        let cell_max_alpha = |img: &egui::ColorImage, idx: usize, cols: u32, cell: u32| -> u8 {
            let cx = ((idx as u32) % cols) * cell;
            let cy = ((idx as u32) / cols) * cell;
            let w = img.size[0];
            let mut max_a = 0u8;
            for y in cy..(cy + cell) {
                for x in cx..(cx + cell) {
                    max_a = max_a.max(img.pixels[y as usize * w + x as usize].a());
                }
            }
            max_a
        };

        for icon in ALL_ENUM_ICONS {
            let idx = icon_index(icon);
            assert!(
                cell_max_alpha(m64, idx, MONO_COLS, 64) >= 160,
                "mono 64 cell for {icon:?} is empty or too faint"
            );
            assert!(
                cell_max_alpha(m32, idx, MONO_COLS, 32) >= 160,
                "mono 32 cell for {icon:?} is empty or too faint"
            );
            if let Some(spec) = color_spec(icon) {
                assert!(!spec.layers.is_empty(), "{icon:?} has empty color_spec");
                for layer in spec.layers {
                    assert!(
                        cell_max_alpha(a64, layer.cell as usize, ACCENT_COLS, 64) >= 120,
                        "accent 64 cell {} for {icon:?} is empty",
                        layer.cell
                    );
                    assert!(
                        cell_max_alpha(a32, layer.cell as usize, ACCENT_COLS, 32) >= 120,
                        "accent 32 cell {} for {icon:?} is empty",
                        layer.cell
                    );
                }
            }
        }
    }

    #[test]
    fn uv_rects_have_half_texel_inset_and_stay_in_unit_square() {
        for cell_px in [32u32, 64u32] {
            for icon in ALL_ENUM_ICONS {
                let uv = cell_uv(icon_index(icon), MONO_COLS, MONO_ROWS, cell_px);
                assert!(uv.min.x > 0.0 && uv.min.y > 0.0);
                assert!(uv.max.x < 1.0 && uv.max.y < 1.0);
                assert!(uv.width() > 0.0 && uv.height() > 0.0);
            }
        }
    }

    #[test]
    fn headless_render_all_icons_across_themes_and_sizes() {
        let orig = crate::theme::current_theme();
        let ctx = egui::Context::default();
        for theme in AppTheme::ALL {
            crate::theme::set_current_theme(theme);
            let pal = crate::theme::palette_for(theme);
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 480.0)));
            let out = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let commands = egui::UiStackInfo::default().with_tag(RIBBON_COMMANDS);
                    ui.scope_builder(egui::UiBuilder::new().ui_stack_info(commands), |ui| {
                        for &sz in &[14.0_f32, 16.0, 24.0, 32.0] {
                            let r = Rect::from_min_size(Pos2::new(8.0, 8.0), Vec2::splat(sz));
                            for icon in ALL_ENUM_ICONS {
                                draw_control(ui, r, icon, pal.text, true);
                                draw_control(ui, r, icon, pal.text_muted, false);
                                draw_mono(ui.painter(), r, icon, pal.text_weak);
                                draw(ui.painter(), r, icon, pal.accent);
                                draw_vec(ui.painter(), r, icon, pal.text);
                                draw_vec_color(ui.painter(), r, icon);
                            }
                        }
                    });
                });
            });
            assert!(!out.shapes.is_empty(), "expected icon shapes for theme {theme:?}");
        }
        crate::theme::set_current_theme(orig);
    }
}
