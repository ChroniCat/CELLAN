//! WPS 高频下拉菜单扩展能力与交互窗口：
//! - 12 种「粘贴 ▾」直达模式（数值 / 值和数字格式 / 值和源格式 / 仅公式 / 公式和数字格式 / 格式 / 无边框 / 列宽 / 转置 / 粘贴链接 / 粘贴为图片）
//! - 「复制 ▾」选区转高清 PNG 位图（复制为图片 / 粘贴为图片 / 仅复制数值）
//! - 升级版「选择性粘贴…」与右键粘贴图标条联动
//! - 向上/向左填充与「填充 ▾」序列生成对话框（等差 / 等比 / 工作日 / 按月 / 按年）
//! - 「查找和选择 ▾」定位 / 定位条件（公式 / 常量 / 空值 / 错误值 / 批注 / 条件格式 / 数据验证）
//! - 「条件格式 ▾」「套用表格格式 ▾」「单元格样式 ▾」「清除 ▾」「格式 ▾」「图表 ▾」「符号 ▾」「数据验证 ▾」级联动作与聚焦对话框

use std::io::Cursor;

use eframe::egui::{self, Color32, RichText};
use sheetcore::cell_name;
use sheetcore::cf::{CfKind, CfOp, Cfvo, CfvoKind};
use sheetcore::charts::ChartKind;
use sheetcore::model::{Cell, CellRef, Value, Workbook};
use sheetcore::style::BorderSide;

use crate::{col_label, ui_kit, Clip, LastPasteState, SheetApp};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PasteMode {
    All,
    Values,
    ValuesNumFmt,
    ValuesSourceFmt,
    Formulas,
    FormulasNumFmt,
    Formats,
    NoBorders,
    ColWidths,
    Transpose,
    Link,
    AsPicture,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SeriesPreset {
    Linear,
    Weekday,
    Month,
    Year,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SpecialTarget {
    Formulas,
    Constants,
    Blanks,
    Errors,
    Comments,
    CondFormats,
    DataValidation,
    VisibleCells,
    CurrentRegion,
    RowDifferences,
    ColDifferences,
    Precedents,
    Dependents,
    LastCell,
    CurrentArray,
    Objects,
}

impl SpecialTarget {
    pub(crate) fn label(self) -> &'static str {
        match self {
            SpecialTarget::Formulas => "公式",
            SpecialTarget::Constants => "常量",
            SpecialTarget::Blanks => "空值",
            SpecialTarget::Errors => "错误值",
            SpecialTarget::Comments => "批注",
            SpecialTarget::CondFormats => "条件格式",
            SpecialTarget::DataValidation => "数据验证",
            SpecialTarget::VisibleCells => "可见单元格",
            SpecialTarget::CurrentRegion => "当前区域",
            SpecialTarget::RowDifferences => "行内容差异单元格",
            SpecialTarget::ColDifferences => "列内容差异单元格",
            SpecialTarget::Precedents => "引用单元格",
            SpecialTarget::Dependents => "从属单元格",
            SpecialTarget::LastCell => "最后一个单元格",
            SpecialTarget::CurrentArray => "当前数组",
            SpecialTarget::Objects => "对象",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GotoDlgState {
    pub tab: u8, // 0 = 定位，1 = 定位条件
    pub target_ref: String,
    pub special: SpecialTarget,
    /// 公式/常量子类型过滤（数字/文本/逻辑值/错误值）
    pub sub_numbers: bool,
    pub sub_text: bool,
    pub sub_bools: bool,
    pub sub_errors: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FillSeriesDlgState {
    pub by_row: bool,
    pub kind: u8,      // 0 = 等差，1 = 等比，2 = 日期，3 = 自动填充
    pub date_unit: u8, // 0 = 日，1 = 工作日，2 = 月，3 = 年
    pub step: String,
    pub stop: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CfRuleOp {
    Greater,
    Less,
    GreaterEq,
    LessEq,
    Between,
    NotBetween,
    Equal,
    NotEqual,
    TextContains,
    TextNotContains,
    TextBeginsWith,
    TextEndsWith,
    TimePeriod,
    TopN,
    BottomN,
    AboveAvg,
    BelowAvg,
    Duplicate,
    Unique,
    Blanks,
    NoBlanks,
    Errors,
    NoErrors,
    Formula,
    DataBar,
    ColorScale,
    IconSet,
}

impl CfRuleOp {
    pub(crate) const ALL: [CfRuleOp; 27] = [
        CfRuleOp::Greater,
        CfRuleOp::Less,
        CfRuleOp::GreaterEq,
        CfRuleOp::LessEq,
        CfRuleOp::Between,
        CfRuleOp::NotBetween,
        CfRuleOp::Equal,
        CfRuleOp::NotEqual,
        CfRuleOp::TextContains,
        CfRuleOp::TextNotContains,
        CfRuleOp::TextBeginsWith,
        CfRuleOp::TextEndsWith,
        CfRuleOp::TimePeriod,
        CfRuleOp::TopN,
        CfRuleOp::BottomN,
        CfRuleOp::AboveAvg,
        CfRuleOp::BelowAvg,
        CfRuleOp::Duplicate,
        CfRuleOp::Unique,
        CfRuleOp::Blanks,
        CfRuleOp::NoBlanks,
        CfRuleOp::Errors,
        CfRuleOp::NoErrors,
        CfRuleOp::Formula,
        CfRuleOp::DataBar,
        CfRuleOp::ColorScale,
        CfRuleOp::IconSet,
    ];
    pub(crate) fn title(self) -> &'static str {
        match self {
            CfRuleOp::Greater => "大于",
            CfRuleOp::Less => "小于",
            CfRuleOp::GreaterEq => "大于等于",
            CfRuleOp::LessEq => "小于等于",
            CfRuleOp::Between => "介于",
            CfRuleOp::NotBetween => "不介于",
            CfRuleOp::Equal => "等于",
            CfRuleOp::NotEqual => "不等于",
            CfRuleOp::TextContains => "文本包含",
            CfRuleOp::TextNotContains => "文本不包含",
            CfRuleOp::TextBeginsWith => "文本开头为",
            CfRuleOp::TextEndsWith => "文本结尾为",
            CfRuleOp::TimePeriod => "发生日期",
            CfRuleOp::TopN => "前 N 项 / 前 N%",
            CfRuleOp::BottomN => "后 N 项 / 后 N%",
            CfRuleOp::AboveAvg => "高于平均值",
            CfRuleOp::BelowAvg => "低于平均值",
            CfRuleOp::Duplicate => "重复值",
            CfRuleOp::Unique => "唯一值",
            CfRuleOp::Blanks => "空值",
            CfRuleOp::NoBlanks => "非空值",
            CfRuleOp::Errors => "错误值",
            CfRuleOp::NoErrors => "无错误",
            CfRuleOp::Formula => "自定义公式",
            CfRuleOp::DataBar => "数据条（支持负值）",
            CfRuleOp::ColorScale => "色阶",
            CfRuleOp::IconSet => "图标集",
        }
    }
    fn uses_dxf(self) -> bool {
        !matches!(self, CfRuleOp::DataBar | CfRuleOp::ColorScale | CfRuleOp::IconSet)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CfRuleDlgState {
    pub edit_target: Option<(usize, usize)>,
    pub sqref: String,
    pub op: CfRuleOp,
    pub v1: String,
    pub v2: String,
    pub top_n: u32,
    pub top_percent: bool,
    pub period: sheetcore::cf::TimePeriod,
    pub style_idx: u8,
    pub bold: bool,
    pub italic: bool,
    pub stop: bool,
    pub bar_color: [u8; 3],
    pub bar_neg_color: [u8; 3],
    pub bar_show_value: bool,
    pub scale_preset: u8,
    pub icon_set: String,
    pub icon_reverse: bool,
    pub icon_show_value: bool,
}

pub(crate) fn style_preset_visuals(id: &str) -> (Color32, Color32, bool) {
    match id {
        "StyleTitle" => (Color32::WHITE, Color32::from_rgb(31, 78, 121), true),
        "StyleHeading" => (
            Color32::from_rgb(217, 226, 243),
            Color32::from_rgb(31, 56, 100),
            true,
        ),
        "StyleGood" => (Color32::from_rgb(198, 239, 206), Color32::from_rgb(0, 97, 0), false),
        "StyleBad" => (Color32::from_rgb(255, 199, 206), Color32::from_rgb(156, 0, 6), false),
        "StyleNeutral" => (Color32::from_rgb(255, 235, 156), Color32::from_rgb(156, 101, 0), false),
        "StyleInput" => (Color32::from_rgb(255, 204, 153), Color32::from_rgb(63, 63, 118), false),
        "StyleOutput" => (Color32::from_rgb(242, 242, 242), Color32::from_rgb(63, 63, 63), true),
        "StyleCalc" => (Color32::from_rgb(242, 242, 242), Color32::from_rgb(250, 125, 0), true),
        "StyleWarning" => (Color32::WHITE, Color32::from_rgb(255, 0, 0), false),
        "StyleNote" => (Color32::WHITE, Color32::from_rgb(127, 127, 127), false),
        "StyleH1" | "StyleH2" | "StyleH3" => (Color32::WHITE, Color32::from_rgb(31, 78, 121), true),
        "StyleTotal" => (Color32::WHITE, Color32::from_rgb(15, 23, 42), true),
        "StyleAccentBlue20" => (Color32::from_rgb(217, 225, 242), Color32::from_rgb(31, 78, 121), false),
        "StyleAccentGreen20" => (Color32::from_rgb(226, 239, 218), Color32::from_rgb(56, 87, 35), false),
        "StyleAccentOrange20" => (Color32::from_rgb(252, 228, 214), Color32::from_rgb(198, 89, 17), false),
        "StyleComma0" | "StyleCurrency0" => (Color32::from_rgb(248, 250, 252), Color32::from_rgb(30, 41, 59), false),
        _ => (Color32::WHITE, ui_kit::TEXT, false),
    }
}

fn quote_sheet_name(name: &str) -> String {
    let simple = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if simple {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

fn shift_formula_coords(
    f: &str,
    coords: &Option<Vec<Vec<(u32, u32)>>>,
    src_sheet_name: &Option<String>,
    i: usize,
    j: usize,
    r: u32,
    c: u32,
    target_sheet_name: &str,
) -> String {
    match (coords, src_sheet_name) {
        (Some(co), Some(_)) => {
            if let Some(&(sr, sc)) = co.get(i).and_then(|row| row.get(j)) {
                sheetcore::refs::translate(
                    f,
                    r as i64 - sr as i64,
                    c as i64 - sc as i64,
                    target_sheet_name,
                )
            } else {
                f.to_string()
            }
        }
        _ => f.to_string(),
    }
}

fn cf_dxf_by_preset(app: &mut SheetApp, style_idx: u8) -> usize {
    cf_dxf_by_preset_ext(app, style_idx, false, false)
}

fn cf_dxf_by_preset_ext(app: &mut SheetApp, style_idx: u8, bold: bool, italic: bool) -> usize {
    match style_idx {
        1 => app.cf_dxf_ext(bold, italic, Some([156, 101, 0]), Some([255, 235, 156])), // 黄填充深黄文本
        2 => app.cf_dxf_ext(bold, italic, Some([0, 97, 0]), Some([198, 239, 206])),   // 绿填充深绿文本
        3 => app.cf_dxf_ext(bold, italic, None, Some([255, 199, 206])),               // 仅浅红填充
        4 => app.cf_dxf_ext(bold, italic, Some([192, 0, 0]), None),                   // 仅红色文本
        5 => app.cf_dxf_ext(bold, italic, Some([31, 78, 121]), Some([217, 225, 242])),// 浅蓝填充深蓝文本
        _ => app.cf_dxf_ext(bold, italic, Some([156, 0, 6]), Some([255, 199, 206])),  // 浅红填充深红文本
    }
}

// ============================================================================
//  TTF 矢量字体栅格化 & 选区离屏绘图 -> RGBA/PNG
// ============================================================================

struct SegCollector {
    segs: Vec<(f32, f32, f32, f32)>,
    start: (f32, f32),
    cur: (f32, f32),
}

impl SegCollector {
    fn new() -> Self {
        Self {
            segs: Vec::new(),
            start: (0.0, 0.0),
            cur: (0.0, 0.0),
        }
    }
}

impl ttf_parser::OutlineBuilder for SegCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.start = (x, y);
        self.cur = (x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        if (x - self.cur.0).abs() > 1e-4 || (y - self.cur.1).abs() > 1e-4 {
            self.segs.push((self.cur.0, self.cur.1, x, y));
        }
        self.cur = (x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x0, y0) = self.cur;
        let steps = 6;
        let mut prev = (x0, y0);
        for k in 1..=steps {
            let t = k as f32 / steps as f32;
            let mt = 1.0 - t;
            let px = mt * mt * x0 + 2.0 * mt * t * x1 + t * t * x;
            let py = mt * mt * y0 + 2.0 * mt * t * y1 + t * t * y;
            self.segs.push((prev.0, prev.1, px, py));
            prev = (px, py);
        }
        self.cur = (x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x0, y0) = self.cur;
        let steps = 8;
        let mut prev = (x0, y0);
        for k in 1..=steps {
            let t = k as f32 / steps as f32;
            let mt = 1.0 - t;
            let px = mt * mt * mt * x0
                + 3.0 * mt * mt * t * x1
                + 3.0 * mt * t * t * x2
                + t * t * t * x;
            let py = mt * mt * mt * y0
                + 3.0 * mt * mt * t * y1
                + 3.0 * mt * t * t * y2
                + t * t * t * y;
            self.segs.push((prev.0, prev.1, px, py));
            prev = (px, py);
        }
        self.cur = (x, y);
    }

    fn close(&mut self) {
        if (self.start.0 - self.cur.0).abs() > 1e-4 || (self.start.1 - self.cur.1).abs() > 1e-4 {
            self.segs.push((self.cur.0, self.cur.1, self.start.0, self.start.1));
        }
        self.cur = self.start;
    }
}

struct Canvas<'a> {
    w: usize,
    h: usize,
    buf: &'a mut [u8],
}

impl<'a> Canvas<'a> {
    fn blend_px(&mut self, x: i32, y: i32, rgb: [u8; 3], cov: f32) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 || cov <= 0.0 {
            return;
        }
        let a = cov.clamp(0.0, 1.0);
        let idx = (y as usize * self.w + x as usize) * 4;
        let inv = 1.0 - a;
        self.buf[idx] = (self.buf[idx] as f32 * inv + rgb[0] as f32 * a).round() as u8;
        self.buf[idx + 1] = (self.buf[idx + 1] as f32 * inv + rgb[1] as f32 * a).round() as u8;
        self.buf[idx + 2] = (self.buf[idx + 2] as f32 * inv + rgb[2] as f32 * a).round() as u8;
        self.buf[idx + 3] = 255;
    }

    fn fill_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, rgb: [u8; 3]) {
        let ix0 = x0.floor().max(0.0) as i32;
        let iy0 = y0.floor().max(0.0) as i32;
        let ix1 = x1.ceil().min(self.w as f32) as i32;
        let iy1 = y1.ceil().min(self.h as f32) as i32;
        for y in iy0..iy1 {
            for x in ix0..ix1 {
                let idx = (y as usize * self.w + x as usize) * 4;
                self.buf[idx] = rgb[0];
                self.buf[idx + 1] = rgb[1];
                self.buf[idx + 2] = rgb[2];
                self.buf[idx + 3] = 255;
            }
        }
    }

    fn fill_rect_clip(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, clip: [f32; 4], rgb: [u8; 3]) {
        self.fill_rect(
            x0.max(clip[0]),
            y0.max(clip[1]),
            x1.min(clip[2]),
            y1.min(clip[3]),
            rgb,
        );
    }

    fn draw_glyph(
        &mut self,
        face: &ttf_parser::Face<'_>,
        gid: ttf_parser::GlyphId,
        pen_x: f32,
        baseline_y: f32,
        px_scale: f32,
        clip: [f32; 4],
        rgb: [u8; 3],
    ) {
        let mut col = SegCollector::new();
        if face.outline_glyph(gid, &mut col).is_none() || col.segs.is_empty() {
            return;
        }
        let mut min_x = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut edges: Vec<(f32, f32, f32, f32, i32)> = Vec::with_capacity(col.segs.len());
        for (ax, ay, bx, by) in col.segs {
            let x0 = pen_x + ax * px_scale;
            let y0 = baseline_y - ay * px_scale;
            let x1 = pen_x + bx * px_scale;
            let y1 = baseline_y - by * px_scale;
            min_x = min_x.min(x0).min(x1);
            max_x = max_x.max(x0).max(x1);
            min_y = min_y.min(y0).min(y1);
            max_y = max_y.max(y0).max(y1);
            if (y1 - y0).abs() > 1e-4 {
                if y0 < y1 {
                    edges.push((x0, y0, x1, y1, 1));
                } else {
                    edges.push((x1, y1, x0, y0, -1));
                }
            }
        }
        if edges.is_empty() {
            return;
        }
        let ix0 = (min_x.floor() as i32).max(clip[0].floor() as i32).max(0);
        let ix1 = (max_x.ceil() as i32).min(clip[2].ceil() as i32).min(self.w as i32);
        let iy0 = (min_y.floor() as i32).max(clip[1].floor() as i32).max(0);
        let iy1 = (max_y.ceil() as i32).min(clip[3].ceil() as i32).min(self.h as i32);
        if ix1 <= ix0 || iy1 <= iy0 {
            return;
        }
        let width_px = (ix1 - ix0) as usize;
        let mut row_cov = vec![0.0f32; width_px];
        let mut hits: Vec<(f32, i32)> = Vec::with_capacity(16);
        const SUB_Y: [f32; 4] = [0.125, 0.375, 0.625, 0.875];
        for py in iy0..iy1 {
            row_cov.fill(0.0);
            for &sy_off in &SUB_Y {
                let sy = py as f32 + sy_off;
                hits.clear();
                for &(x0, y0, x1, y1, dir) in &edges {
                    if sy >= y0 && sy < y1 {
                        let t = (sy - y0) / (y1 - y0);
                        hits.push((x0 + (x1 - x0) * t, dir));
                    }
                }
                if hits.len() < 2 {
                    continue;
                }
                hits.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut winding = 0i32;
                let mut span_start = 0.0f32;
                for &(hx, dir) in &hits {
                    let prev = winding;
                    winding += dir;
                    if prev == 0 && winding != 0 {
                        span_start = hx;
                    } else if prev != 0 && winding == 0 {
                        let sx = span_start.max(ix0 as f32);
                        let ex = hx.min(ix1 as f32);
                        if ex > sx {
                            let c0 = sx.floor() as i32;
                            let c1 = ex.ceil() as i32;
                            for cx in c0..c1 {
                                let cov_x = (ex.min((cx + 1) as f32) - sx.max(cx as f32)).max(0.0);
                                row_cov[(cx - ix0) as usize] += cov_x * 0.25;
                            }
                        }
                    }
                }
            }
            for (k, &cov) in row_cov.iter().enumerate() {
                if cov > 0.01 {
                    self.blend_px(ix0 + k as i32, py, rgb, cov);
                }
            }
        }
    }
}

fn pick_face<'a>(
    ch: char,
    bold: bool,
    sans: &'a ttf_parser::Face<'a>,
    sans_b: &'a ttf_parser::Face<'a>,
    latin: &'a ttf_parser::Face<'a>,
    latin_b: &'a ttf_parser::Face<'a>,
) -> (&'a ttf_parser::Face<'a>, ttf_parser::GlyphId) {
    let is_ascii_like = ch.is_ascii() || matches!(ch, '¥' | '€' | '£' | '±' | '×' | '÷' | '≈' | '≠' | '≤' | '≥' | '‰' | '°' | '℃');
    if is_ascii_like {
        let primary = if bold { latin_b } else { latin };
        if let Some(gid) = primary.glyph_index(ch) {
            return (primary, gid);
        }
    }
    let fallback = if bold { sans_b } else { sans };
    if let Some(gid) = fallback.glyph_index(ch) {
        return (fallback, gid);
    }
    if let Some(gid) = latin.glyph_index(ch) {
        return (latin, gid);
    }
    (sans, sans.glyph_index(ch).unwrap_or(ttf_parser::GlyphId(0)))
}

fn measure_text_px(
    text: &str,
    font_px: f32,
    bold: bool,
    sans: &ttf_parser::Face<'_>,
    sans_b: &ttf_parser::Face<'_>,
    latin: &ttf_parser::Face<'_>,
    latin_b: &ttf_parser::Face<'_>,
) -> f32 {
    let mut w = 0.0f32;
    for ch in text.chars() {
        let (face, gid) = pick_face(ch, bold, sans, sans_b, latin, latin_b);
        let adv = face.glyph_hor_advance(gid).unwrap_or(face.units_per_em() / 2) as f32;
        w += adv * (font_px / face.units_per_em().max(1) as f32);
    }
    w
}

pub(crate) fn encode_rgba_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let enc = image::codecs::png::PngEncoder::new(Cursor::new(&mut out));
    let _ = image::ImageEncoder::write_image(
        enc,
        rgba,
        w,
        h,
        image::ExtendedColorType::Rgba8,
    );
    out
}

fn render_sheet_rect_to_rgba(wb: &Workbook, si: usize, rect: (u32, u32, u32, u32)) -> (u32, u32, Vec<u8>) {
    let sh = wb.sheet(si);
    let (r1, c1, mut r2, mut c2) = (
        rect.0.min(rect.2),
        rect.1.min(rect.3),
        rect.0.max(rect.2),
        rect.1.max(rect.3),
    );
    // 避免超大图导致 OOM：最多截取 60 行 × 30 列
    r2 = r2.min(r1 + 59);
    c2 = c2.min(c1 + 29);

    let cols: Vec<u32> = (c1..=c2)
        .filter(|c| !sh.hidden_cols.contains(c) && sh.cols.size(*c) > 0.5)
        .collect();
    let rows: Vec<u32> = (r1..=r2)
        .filter(|r| !sh.hidden_rows.contains(r) && !sh.hidden_rows_user.contains(r) && sh.rows.size(*r) > 0.5)
        .collect();
    let cols = if cols.is_empty() { vec![c1] } else { cols };
    let rows = if rows.is_empty() { vec![r1] } else { rows };

    let col_ws: Vec<f32> = cols.iter().map(|c| sh.cols.size(*c).max(16.0)).collect();
    let row_hs: Vec<f32> = rows.iter().map(|r| sh.rows.size(*r).max(12.0)).collect();
    let logical_w = col_ws.iter().sum::<f32>().max(24.0);
    let logical_h = row_hs.iter().sum::<f32>().max(18.0);

    let mut scale = 2.0f32;
    if logical_w * scale > 2400.0 {
        scale = (2400.0 / logical_w).clamp(1.0, 2.0);
    }
    if logical_h * scale > 1800.0 {
        scale = scale.min((1800.0 / logical_h).clamp(1.0, 2.0));
    }

    let px_w = ((logical_w * scale).ceil() as u32 + 2).max(8);
    let px_h = ((logical_h * scale).ceil() as u32 + 2).max(8);
    let mut buf = vec![255u8; (px_w as usize) * (px_h as usize) * 4];
    let mut canvas = Canvas { w: px_w as usize, h: px_h as usize, buf: &mut buf };

    let mut xs = Vec::with_capacity(cols.len() + 1);
    let mut cur_x = 1.0f32;
    xs.push(cur_x);
    for w in &col_ws {
        cur_x += w * scale;
        xs.push(cur_x);
    }
    let mut ys = Vec::with_capacity(rows.len() + 1);
    let mut cur_y = 1.0f32;
    ys.push(cur_y);
    for h in &row_hs {
        cur_y += h * scale;
        ys.push(cur_y);
    }

    let col_pos = |c: u32| -> Option<usize> { cols.binary_search(&c).ok() };
    let row_pos = |r: u32| -> Option<usize> { rows.binary_search(&r).ok() };

    // 1. 浅灰网格线（底图）
    let grid_col = [226u8, 232, 240];
    let x_min = xs[0];
    let x_max = *xs.last().unwrap();
    let y_min = ys[0];
    let y_max = *ys.last().unwrap();
    for &x in &xs {
        canvas.fill_rect(x, y_min, x + 1.0, y_max + 1.0, grid_col);
    }
    for &y in &ys {
        canvas.fill_rect(x_min, y, x_max + 1.0, y + 1.0, grid_col);
    }

    // 2. 单元格背景（先普通格，再合并区域覆盖内部网格线）
    for (ri, &r) in rows.iter().enumerate() {
        for (ci, &c) in cols.iter().enumerate() {
            let st = wb.styles.get(sh.style_of(r, c));
            if let Some(fill) = st.fill {
                canvas.fill_rect(xs[ci] + 0.5, ys[ri] + 0.5, xs[ci + 1], ys[ri + 1], fill);
            }
        }
    }
    for &(mr1, mc1, mr2, mc2) in &sh.merges {
        let (Some(ri1), Some(ci1)) = (row_pos(mr1.max(r1)), col_pos(mc1.max(c1))) else { continue };
        let ri2 = rows.iter().rposition(|&r| r <= mr2).unwrap_or(ri1);
        let ci2 = cols.iter().rposition(|&c| c <= mc2).unwrap_or(ci1);
        if ri2 < ri1 || ci2 < ci1 {
            continue;
        }
        let st = wb.styles.get(sh.style_of(mr1, mc1));
        let fill = st.fill.unwrap_or([255, 255, 255]);
        canvas.fill_rect(xs[ci1] + 0.5, ys[ri1] + 0.5, xs[ci2 + 1], ys[ri2 + 1], fill);
    }

    // 3. 文字栅格化
    let sans = ttf_parser::Face::parse(crate::FONT_SANS, 0).expect("FONT_SANS");
    let sans_b = ttf_parser::Face::parse(crate::FONT_SANS_BOLD, 0).expect("FONT_SANS_BOLD");
    let latin = ttf_parser::Face::parse(crate::FONT_LATIN, 0).expect("FONT_LATIN");
    let latin_b = ttf_parser::Face::parse(crate::FONT_LATIN_BOLD, 0).expect("FONT_LATIN_BOLD");

    let merge_of = |r: u32, c: u32| -> Option<(u32, u32, u32, u32)> {
        sh.merges
            .iter()
            .find(|&&(a, b, cc, d)| r >= a && r <= cc && c >= b && c <= d)
            .copied()
    };

    for (ri, &r) in rows.iter().enumerate() {
        for (ci, &c) in cols.iter().enumerate() {
            let (box_x0, box_y0, box_x1, box_y1, src_r, src_c) = if let Some((mr1, mc1, mr2, mc2)) = merge_of(r, c) {
                if r != mr1 || c != mc1 {
                    continue;
                }
                let ri2 = rows.iter().rposition(|&rr| rr <= mr2).unwrap_or(ri);
                let ci2 = cols.iter().rposition(|&cc| cc <= mc2).unwrap_or(ci);
                (xs[ci], ys[ri], xs[ci2 + 1], ys[ri2 + 1], mr1, mc1)
            } else {
                (xs[ci], ys[ri], xs[ci + 1], ys[ri + 1], r, c)
            };
            let Some(cell) = sh.get(src_r, src_c) else { continue };
            if matches!(cell.value, Value::Empty) {
                continue;
            }
            let sid = sh.style_of(src_r, src_c);
            let st = wb.styles.get(sid);
            let text = sheetcore::style::format_value_in(
                wb.date_system,
                &cell.value,
                &st.format,
            );
            if text.is_empty() {
                continue;
            }
            let font_px = (st.size.clamp(7.0, 48.0) * 1.25 * scale).max(9.0);
            let text_w = measure_text_px(&text, font_px, st.bold, &sans, &sans_b, &latin, &latin_b);
            let pad = 4.0 * scale;
            let align = match st.halign {
                1 => 1,
                2 => 2,
                3 => 3,
                _ => {
                    if matches!(cell.value, Value::Number(_)) {
                        3
                    } else if matches!(cell.value, Value::Bool(_) | Value::Error(_)) {
                        2
                    } else {
                        1
                    }
                }
            };
            let mut pen_x = match align {
                2 => (box_x0 + box_x1 - text_w) * 0.5,
                3 => box_x1 - pad - text_w,
                _ => box_x0 + pad,
            };
            if pen_x < box_x0 + 2.0 * scale {
                pen_x = box_x0 + 2.0 * scale;
            }
            let baseline_y = match st.valign {
                1 => box_y0 + 3.0 * scale + font_px * 0.82,
                3 => box_y1 - 3.0 * scale - font_px * 0.16,
                _ => (box_y0 + box_y1) * 0.5 + font_px * 0.34,
            };
            let clip = [
                box_x0 + 1.0,
                box_y0 + 1.0,
                box_x1 - 1.0,
                box_y1 - 1.0,
            ];
            let rgb = st.color;
            for ch in text.chars() {
                let (face, gid) = pick_face(ch, st.bold, &sans, &sans_b, &latin, &latin_b);
                let px_scale = font_px / face.units_per_em().max(1) as f32;
                canvas.draw_glyph(face, gid, pen_x, baseline_y, px_scale, clip, rgb);
                let adv = face.glyph_hor_advance(gid).unwrap_or(face.units_per_em() / 2) as f32;
                pen_x += adv * px_scale;
                if pen_x > box_x1 {
                    break;
                }
            }
        }
    }

    // 4. 边框
    let clip_all = [0.0, 0.0, px_w as f32, px_h as f32];
    for (ri, &r) in rows.iter().enumerate() {
        for (ci, &c) in cols.iter().enumerate() {
            let st = wb.styles.get(sh.style_of(r, c));
            let x0 = xs[ci];
            let x1 = xs[ci + 1];
            let y0 = ys[ri];
            let y1 = ys[ri + 1];
            let b_left = st.border[BorderSide::Left.idx()];
            if !b_left.is_none() {
                let t = (b_left.px() * scale).max(1.0);
                canvas.fill_rect_clip(x0 - t * 0.5, y0, x0 + t * 0.5, y1 + t * 0.5, clip_all, b_left.color);
            }
            let b_right = st.border[BorderSide::Right.idx()];
            if !b_right.is_none() {
                let t = (b_right.px() * scale).max(1.0);
                canvas.fill_rect_clip(x1 - t * 0.5, y0, x1 + t * 0.5, y1 + t * 0.5, clip_all, b_right.color);
            }
            let b_top = st.border[BorderSide::Top.idx()];
            if !b_top.is_none() {
                let t = (b_top.px() * scale).max(1.0);
                canvas.fill_rect_clip(x0, y0 - t * 0.5, x1 + t * 0.5, y0 + t * 0.5, clip_all, b_top.color);
            }
            let b_bot = st.border[BorderSide::Bottom.idx()];
            if !b_bot.is_none() {
                let t = (b_bot.px() * scale).max(1.0);
                canvas.fill_rect_clip(x0, y1 - t * 0.5, x1 + t * 0.5, y1 + t * 0.5, clip_all, b_bot.color);
                if b_bot.style == 4 {
                    canvas.fill_rect_clip(x0, y1 - t * 2.5, x1, y1 - t * 1.5, clip_all, b_bot.color);
                }
            }
        }
    }

    (px_w, px_h, buf)
}

impl SheetApp {
    pub(crate) fn render_range_to_rgba(&self, si: usize, rect: (u32, u32, u32, u32)) -> (u32, u32, Vec<u8>) {
        render_sheet_rect_to_rgba(&self.wb, si, rect)
    }

    fn render_matrix_to_rgba(&self, matrix: &[Vec<(Cell, u16)>]) -> (u32, u32, Vec<u8>) {
        let mut tmp_wb = Workbook::new_blank();
        tmp_wb.styles = self.wb.styles.clone();
        tmp_wb.date_system = self.wb.date_system;
        let h = matrix.len().max(1);
        let w = matrix.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
        for (i, row) in matrix.iter().enumerate() {
            for (j, (cell, sid)) in row.iter().enumerate() {
                tmp_wb.sheets[0].set(i as u32, j as u32, cell.clone());
                tmp_wb.sheets[0].set_style(i as u32, j as u32, *sid);
            }
        }
        render_sheet_rect_to_rgba(&tmp_wb, 0, (0, 0, (h - 1) as u32, (w - 1) as u32))
    }

    fn render_clip_cells_to_rgba(&self, clip: &Clip) -> (u32, u32, Vec<u8>) {
        if let Some(range) = &clip.deferred {
            return render_sheet_rect_to_rgba(range.book(), 0, clip.rect);
        }
        if clip.source_rows.is_none() && clip.sheet < self.wb.sheets.len() && !clip.cells().is_empty() {
            return self.render_range_to_rgba(clip.sheet, clip.rect);
        }
        self.render_matrix_to_rgba(clip.cells())
    }

    pub(crate) fn copy_selection_as_picture(&mut self, _ctx: &egui::Context) {
        let rect = self.sel_rect();
        let (w, h, rgba) = self.render_range_to_rgba(self.sheet_idx, rect);
        let png = encode_rgba_png(w, h, &rgba);
        self.retire_previous_clipboard();
        self.clip_pic = Some((w, h, png));
        self.clip = None;
        self.clip_stamp = None;
        self.clip_seq = None;
        self.clip_seq_pending_sync = false;
        self.clip_copied_at = None;
        #[cfg(not(test))]
        {
            let img = arboard::ImageData {
                width: w as usize,
                height: h as usize,
                bytes: std::borrow::Cow::Borrowed(&rgba),
            };
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_image(img);
            }
        }
        self.status = format!("已将选区复制为图片（{w}×{h} px，可直接粘贴到本表或聊天软件）");
    }

    pub(crate) fn copy_values_text_only(&mut self, ctx: &egui::Context) {
        let (r1, c1, r2, c2) = self.sel_rect();
        if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
            self.status = "选区太大（超过 20 万格），不复制".into();
            return;
        }
        let sh = self.wb.sheet(self.sheet_idx);
        let mut lines = Vec::new();
        let mut cells = Vec::new();
        for r in r1..=r2 {
            if sh.hidden_rows.contains(&r) {
                continue;
            }
            let mut row_txt = Vec::new();
            let mut row_cells = Vec::new();
            for c in c1..=c2 {
                let cell = sh.get(r, c).map(|c| c.into_owned()).unwrap_or_default();
                let sid = sh.style_of(r, c);
                let fmt = &self.wb.styles.get(sid).format;
                let s = sheetcore::style::format_value_in(self.wb.date_system, &cell.value, fmt);
                row_txt.push(s);
                row_cells.push((
                    Cell {
                        value: cell.value,
                        formula: None,
                    },
                    0u16,
                ));
            }
            lines.push(row_txt.join("\t"));
            cells.push(row_cells);
        }
        let tsv = format!("{}\n", lines.join("\n"));
        ctx.copy_text(tsv.clone());
        self.retire_previous_clipboard();
        #[cfg(not(test))]
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(tsv.clone());
        }
        self.clip_pic = None;
        self.clip_stamp = Some(tsv);
        self.clip_seq = crate::current_os_clipboard_seq();
        self.clip_seq_pending_sync = true;
        self.clip_copied_at = Some(std::time::Instant::now());
        self.clip = Some(Clip { deferred: None,
            source_rows: None,
            source_cols: None,
            sheet: self.sheet_idx,
            rect: (r1, c1, r2, c2),
            cells,
            merges: Vec::new(),
            cut: false,
        });
        self.status = "已仅复制数值（纯文本）".into();
    }

    pub(crate) fn paste_as_picture(&mut self, _ctx: &egui::Context) {
        if self.edit_blocked() {
            return;
        }
        let mut decoded: Option<(u32, u32, Vec<u8>)> = None;
        if let Some((w, h, png)) = self.clip_pic.clone() {
            decoded = Some((w, h, png));
        } else if let Some(clip) = self.internal_clip_for(self.system_paste_text().as_deref(), true) {
            if crate::office_clipboard::delayed::is_full_columns(clip.rect) {
                self.status = "请先选中要粘贴为图片的数据区域".into();
                return;
            }
            let (w, h, rgba) = self.render_clip_cells_to_rgba(clip);
            let png = encode_rgba_png(w, h, &rgba);
            decoded = Some((w, h, png));
        }
        #[cfg(not(test))]
        if decoded.is_none() {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                if let Ok(img) = cb.get_image() {
                    let w = img.width as u32;
                    let h = img.height as u32;
                    let png = encode_rgba_png(w, h, img.bytes.as_ref());
                    decoded = Some((w, h, png));
                }
            }
        }
        if decoded.is_none() {
            let ext = crate::read_system_clip().text;
            if !ext.trim().is_empty() {
                let mat = Self::parse_clip_text(&ext);
                if !mat.is_empty() {
                    let (w, h, rgba) = self.render_matrix_to_rgba(&mat);
                    let png = encode_rgba_png(w, h, &rgba);
                    decoded = Some((w, h, png));
                }
            }
        }
        let Some((w, h, png)) = decoded else {
            self.status = "剪贴板里没有可粘贴为图片的内容".into();
            return;
        };
        if png.is_empty() || w == 0 || h == 0 {
            self.status = "图片生成失败".into();
            return;
        }
        // 渲染用 2x 缩放，实际锚点按逻辑像素换算成网格跨越行列
        let logical_w = (w as f32 / 2.0).ceil() as u32;
        let logical_h = (h as f32 / 2.0).ceil() as u32;
        let cols = ((logical_w + 63) / 64).max(2);
        let rows = ((logical_h + 18) / 19).max(2);
        let (r, c) = self.sel;
        let before = self.wb.sheet(self.sheet_idx).pictures.clone();
        let name = format!("粘贴图片 {}", before.len() + 1);
        let part = sheetcore::pic_write::memory_part(&png, "png");
        self.wb.sheet_mut(self.sheet_idx).pictures.push(sheetcore::pic::Picture {
            rect: (r, c, r + rows, c + cols),
            off: [0; 4],
            part,
            data: Some(std::sync::Arc::new(png)),
            ext: "png".into(),
            name,
            ..Default::default()
        });
        self.pic_commit("粘贴为图片", before);
        self.status = format!("已粘贴为图片（{w}×{h} px，可用 Delete 删除或 Ctrl+Z 撤销）");
    }

    // ========================================================================
    //  12 种 WPS 粘贴直达 + 升级版选择性粘贴
    // ========================================================================

    pub(crate) fn paste_mode_exec(&mut self, mode: PasteMode, ctx: &egui::Context) {
        match mode {
            PasteMode::All => self.clip_paste(false, None),
            PasteMode::Values => self.apply_wps_paste_variant(ctx, PasteMode::Values),
            PasteMode::ValuesNumFmt => self.apply_wps_paste_variant(ctx, PasteMode::ValuesNumFmt),
            PasteMode::ValuesSourceFmt => self.apply_wps_paste_variant(ctx, PasteMode::ValuesSourceFmt),
            PasteMode::Formulas => self.apply_wps_paste_variant(ctx, PasteMode::Formulas),
            PasteMode::FormulasNumFmt => self.apply_wps_paste_variant(ctx, PasteMode::FormulasNumFmt),
            PasteMode::Formats => self.apply_wps_paste_variant(ctx, PasteMode::Formats),
            PasteMode::NoBorders => self.apply_wps_paste_variant(ctx, PasteMode::NoBorders),
            PasteMode::ColWidths => self.apply_wps_paste_variant(ctx, PasteMode::ColWidths),
            PasteMode::Transpose => {
                self.ps_kind = 0;
                self.ps_transpose = true;
                self.ps_op = 0;
                let prev_skip = self.ps_skip_blanks;
                self.ps_skip_blanks = false;
                self.paste_special_apply(ctx);
                self.ps_transpose = false;
                self.ps_skip_blanks = prev_skip;
            }
            PasteMode::Link => self.paste_link(),
            PasteMode::AsPicture => self.paste_as_picture(ctx),
        }
    }

    fn apply_wps_paste_variant(&mut self, _ctx: &egui::Context, mode: PasteMode) {
        let ext_text = self.system_paste_text();
        self.apply_wps_paste_variant_impl(mode, ext_text, None, true);
    }

    #[cfg(test)]
    pub(crate) fn apply_wps_paste_variant_rich(
        &mut self,
        mode: PasteMode,
        ext_text: Option<String>,
        ext_rich: Option<crate::office_clipboard::RichClipboard>,
    ) {
        self.apply_wps_paste_variant_impl(mode, ext_text, ext_rich, false);
    }

    fn apply_wps_paste_variant_impl(
        &mut self,
        mode: PasteMode,
        ext_text: Option<String>,
        ext_rich: Option<crate::office_clipboard::RichClipboard>,
        allow_os_rich: bool,
    ) {
        if self.edit_blocked() {
            return;
        }
        let (r0, c0) = self.sel;
        let internal = self.internal_clip_for(ext_text.as_deref(), true).cloned();
        let values_only_rich = matches!(
            mode,
            PasteMode::Values | PasteMode::ValuesNumFmt | PasteMode::ValuesSourceFmt
        );
        let (matrix, coords, src_sheet_idx, src_rect, src_col_widths) = if let Some(clip) = internal {
            if crate::office_clipboard::delayed::is_full_columns(clip.rect) {
                if matches!(mode, PasteMode::Values) {
                    self.clip_paste(true, ext_text);
                } else {
                    self.status = "整列支持普通粘贴和粘贴数值，其他选项请先选中数据区域".into();
                }
                return;
            }
            let (_, sc1, _, sc2) = clip.rect;
            let mut coords = Vec::with_capacity(clip.cells().len());
            for (i, row) in clip.cells().iter().enumerate() {
                coords.push((0..row.len()).map(|j| (clip.source_row(i), sc1 + j as u32)).collect());
            }
            let source = clip.deferred.as_ref().map(|range| range.book().sheet(0))
                .or_else(|| self.wb.sheets.get(clip.sheet));
            let widths: Vec<f32> = if let Some(source) = source {
                (sc1..=sc2).map(|c| source.cols.size(c)).collect()
            } else {
                Vec::new()
            };
            (clip.cells().to_vec(), Some(coords), Some(clip.sheet), Some(clip.rect), widths)
        } else if let Some(rich) = ext_rich.or_else(|| {
            (allow_os_rich && !cfg!(test) && crate::office_clipboard::has_rich_table_format())
                .then(|| crate::office_clipboard::read_rich_table_ext(values_only_rich))
                .flatten()
        }) {
            let Some(grid) = self.materialize_rich_clipboard_matrix(r0, c0, rich) else {
                return;
            };
            (grid, None, None, None, Vec::new())
        } else if let Some(t) = ext_text {
            (Self::parse_clip_text(&t), None, None, None, Vec::new())
        } else {
            self.status = "剪贴板是空的".into();
            return;
        };
        let _ = src_rect;
        let rows = matrix.len() as u32;
        let cols = matrix.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
        if rows == 0 || cols == 0 {
            self.status = "剪贴板是空的".into();
            return;
        }
        if (rows as u64) * (cols as u64) > 200_000 {
            self.status = "粘贴范围过大（单次最多支持 20 万个单元格）".into();
            return;
        }
        let (r0, c0) = self.sel;
        if r0.saturating_add(rows - 1) > 1_048_575 || c0.saturating_add(cols - 1) > 16_383 {
            self.status = "粘贴范围超出工作表行列上限".into();
            return;
        }
        let si = self.sheet_idx;
        let target_sheet_name = self.wb.sheet(si).name.clone();
        let src_sheet_name = src_sheet_idx
            .and_then(|s| self.wb.sheets.get(s))
            .map(|sh| sh.name.clone());
        let keys = sheetcore::undo::rect_keys(r0, c0, r0 + rows - 1, c0 + cols - 1);
        if !self.write_preflight(&keys, "粘贴") {
            return;
        }
        let paste_before: Vec<(u32, u32, Option<Cell>, u16)> = keys
            .iter()
            .map(|&(rr, cc)| {
                let sh = self.wb.sheet(si);
                (rr, cc, sh.get(rr, cc).map(|c| c.into_owned()), sh.style_of(rr, cc))
            })
            .collect();
        let txn = self.txn_begin(&keys);
        let mut wire_styles = Vec::new();
        let mut touched_cells = Vec::new();
        for (i, row) in matrix.iter().enumerate() {
            for (j, (src, sid)) in row.iter().enumerate() {
                let (r, c) = (r0 + i as u32, c0 + j as u32);
                let src_style = self.wb.styles.get(*sid).clone();
                let dst_sid = self.wb.sheet(si).style_of(r, c);
                let dst_style = self.wb.styles.get(dst_sid).clone();
                let mut cell = self.wb.sheet(si).get(r, c).map(|c| c.into_owned()).unwrap_or_default();
                let mut write_cell = false;
                let mut new_style_id: Option<u16> = None;

                match mode {
                    PasteMode::Values => {
                        cell.value = src.value.clone();
                        cell.formula = None;
                        write_cell = true;
                    }
                    PasteMode::ValuesNumFmt => {
                        cell.value = src.value.clone();
                        cell.formula = None;
                        write_cell = true;
                        let mut st = dst_style;
                        st.format = src_style.format;
                        new_style_id = Some(self.wb.styles.intern(st) as u16);
                    }
                    PasteMode::ValuesSourceFmt => {
                        cell.value = src.value.clone();
                        cell.formula = None;
                        write_cell = true;
                        new_style_id = Some(*sid);
                    }
                    PasteMode::Formulas => {
                        if let Some(f) = &src.formula {
                            cell.formula = Some(
                                shift_formula_coords(f, &coords, &src_sheet_name, i, j, r, c, &target_sheet_name)
                                    .into(),
                            );
                        } else {
                            cell.formula = None;
                            cell.value = src.value.clone();
                        }
                        write_cell = true;
                    }
                    PasteMode::FormulasNumFmt => {
                        if let Some(f) = &src.formula {
                            cell.formula = Some(
                                shift_formula_coords(f, &coords, &src_sheet_name, i, j, r, c, &target_sheet_name)
                                    .into(),
                            );
                        } else {
                            cell.formula = None;
                            cell.value = src.value.clone();
                        }
                        write_cell = true;
                        let mut st = dst_style;
                        st.format = src_style.format;
                        new_style_id = Some(self.wb.styles.intern(st) as u16);
                    }
                    PasteMode::Formats => {
                        new_style_id = Some(*sid);
                    }
                    PasteMode::NoBorders => {
                        cell = src.clone();
                        if let Some(f) = &src.formula {
                            cell.formula = Some(
                                shift_formula_coords(f, &coords, &src_sheet_name, i, j, r, c, &target_sheet_name)
                                    .into(),
                            );
                        }
                        write_cell = true;
                        let mut st = src_style;
                        st.border = [sheetcore::style::Border::default(); 4];
                        new_style_id = Some(self.wb.styles.intern(st) as u16);
                    }
                    PasteMode::ColWidths => {
                        cell = src.clone();
                        if let Some(f) = &src.formula {
                            cell.formula = Some(
                                shift_formula_coords(f, &coords, &src_sheet_name, i, j, r, c, &target_sheet_name)
                                    .into(),
                            );
                        }
                        write_cell = true;
                        new_style_id = Some(*sid);
                    }
                    _ => {}
                }

                if write_cell {
                    self.wb.sheet_mut(si).set(r, c, cell);
                    touched_cells.push((si, r, c));
                }
                if let Some(nsid) = new_style_id {
                    self.wb.sheet_mut(si).set_style(r, c, nsid);
                    wire_styles.push((r, c, self.wb.styles.get(nsid).clone()));
                }
            }
        }
        if mode == PasteMode::ColWidths && !src_col_widths.is_empty() {
            let sh = self.wb.sheet_mut(si);
            for (j, w) in src_col_widths.iter().enumerate() {
                sh.cols.set(c0 + j as u32, *w);
            }
            sh.layout_dirty = true;
        }
        let label = match mode {
            PasteMode::Values => "粘贴为数值",
            PasteMode::ValuesNumFmt => "粘贴值和数字格式",
            PasteMode::ValuesSourceFmt => "粘贴值和源格式",
            PasteMode::Formulas => "仅粘贴公式",
            PasteMode::FormulasNumFmt => "粘贴公式和数字格式",
            PasteMode::Formats => "仅粘贴格式",
            PasteMode::NoBorders => "无边框粘贴",
            PasteMode::ColWidths => "保留源列宽粘贴",
            _ => "粘贴",
        };
        self.txn_end(label, txn);
        if !wire_styles.is_empty() {
            if let Some(net) = self.net.as_ref() {
                net.broadcast_style(si, wire_styles);
            }
        }
        if !touched_cells.is_empty() {
            self.recalc_and_broadcast(&touched_cells);
        }
        let badge_mode = match mode {
            PasteMode::Values => 1,
            PasteMode::Formulas => 2,
            PasteMode::Formats => 3,
            PasteMode::ValuesNumFmt => 5,
            PasteMode::ValuesSourceFmt => 6,
            PasteMode::NoBorders => 7,
            PasteMode::ColWidths => 8,
            _ => 0,
        };
        self.last_paste = Some(LastPasteState {
            sheet_idx: si,
            rect: (r0, c0, r0 + rows - 1, c0 + cols - 1),
            cells: matrix,
            mode: badge_mode,
            before: paste_before,
            popup_open: false,
        });
        self.sel_end = (r0 + rows - 1, c0 + cols - 1);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已{label} {rows} 行 × {cols} 列");
    }

    pub(crate) fn paste_link(&mut self) {
        if self.edit_blocked() {
            return;
        }
        let Some(clip) = self.internal_clip_for(self.system_paste_text().as_deref(), true).cloned() else {
            self.status = "请先在工作簿内复制单元格区域，再使用「粘贴链接」".into();
            return;
        };
        let Some(source_sheet) = clip.live_sheet_index(&self.wb) else {
            self.status = "源工作表已不存在".into();
            return;
        };
        let (_, sc1, _, sc2) = clip.rect;
        let rows = if let Some(ref sr) = clip.source_rows {
            sr.len() as u32
        } else if clip.deferred.is_some() {
            clip.rect.2 - clip.rect.0 + 1
        } else {
            clip.cells.len() as u32
        };
        if rows == 0 { return; }
        let cols = sc2 - sc1 + 1;
        if (rows as u64) * (cols as u64) > 200_000 {
            self.status = "粘贴链接范围过大（最多 20 万格）".into();
            return;
        }
        let (r0, c0) = self.sel;
        if r0.saturating_add(rows - 1) > 1_048_575 || c0.saturating_add(cols - 1) > 16_383 {
            self.status = "粘贴链接超出工作表边界".into();
            return;
        }
        let si = self.sheet_idx;
        let src_sheet_name = self.wb.sheet(source_sheet).name.clone();
        // 支持选区平铺粘贴链接（当目标选区尺寸为源尺寸整数倍时自动平铺）
        let (sr1, sc1_dst, sr2, sc2_dst) = self.sel_rect();
        let sel_h = sr2 - sr1 + 1;
        let sel_w = sc2_dst - sc1_dst + 1;
        let (tile_r, tile_c) = if (sel_h > rows || sel_w > cols)
            && sel_h % rows == 0
            && sel_w % cols == 0
            && (sel_h as u64) * (sel_w as u64) <= 200_000
        {
            (sel_h / rows, sel_w / cols)
        } else {
            (1, 1)
        };
        let total_rows = rows * tile_r;
        let total_cols = cols * tile_c;
        let (r0, c0) = if tile_r > 1 || tile_c > 1 { (sr1, sc1_dst) } else { (r0, c0) };
        if r0.saturating_add(total_rows - 1) > 1_048_575 || c0.saturating_add(total_cols - 1) > 16_383 {
            self.status = "粘贴链接超出工作表边界".into();
            return;
        }
        // 筛选态下粘贴链接：自动跳过被筛选隐藏的目标行，防止覆写隐藏行
        let dst_row_list: Vec<u32> = {
            let sh = self.wb.sheet(si);
            let has_hidden = !sh.hidden_rows.is_empty()
                && (r0..r0.saturating_add(total_rows)).any(|rr| sh.hidden_rows.contains(&rr));
            if has_hidden {
                let mut out = Vec::with_capacity(total_rows as usize);
                let mut cur = r0;
                while out.len() < total_rows as usize {
                    if cur > 1_048_575 {
                        self.status = "粘贴链接超出工作表边界".into();
                        return;
                    }
                    if !sh.hidden_rows.contains(&cur) && !sh.hidden_rows_user.contains(&cur) {
                        out.push(cur);
                    }
                    cur = cur.saturating_add(1);
                }
                out
            } else {
                (0..total_rows).map(|i| r0 + i).collect()
            }
        };
        let mut keys = Vec::with_capacity((total_rows as usize) * (total_cols as usize));
        for &dr in &dst_row_list {
            for j in 0..total_cols {
                keys.push((dr, c0 + j));
            }
        }
        if !self.write_preflight(&keys, "粘贴链接") {
            return;
        }
        let txn = self.txn_begin(&keys);
        let mut touched = Vec::with_capacity(keys.len());
        for (ti, &dr) in dst_row_list.iter().enumerate() {
            let i = (ti as u32) % rows;
            for tj in 0..total_cols {
                let j = tj % cols;
                let (sr, sc) = (clip.source_row(i as usize), sc1 + j);
                let dc = c0 + tj;
                let target_ref = if source_sheet == si {
                    format!("={}", cell_name(sr, sc))
                } else {
                    format!("={}!{}", quote_sheet_name(&src_sheet_name), cell_name(sr, sc))
                };
                let src_val = self
                    .wb
                    .sheet(source_sheet)
                    .get(sr, sc)
                    .map(|c| c.value.clone())
                    .unwrap_or(Value::Number(0.0));
                self.wb.sheet_mut(si).set(
                    dr,
                    dc,
                    Cell {
                        value: src_val,
                        formula: Some(target_ref.into()),
                    },
                );
                touched.push((si, dr, dc));
            }
        }
        self.txn_end("粘贴链接", txn);
        self.recalc_and_broadcast(&touched);
        self.sel_end = (r0 + rows - 1, c0 + cols - 1);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已粘贴链接 {rows} 行 × {cols} 列");
    }

    pub(crate) fn draw_paste_special_wps(&mut self, ctx: &egui::Context) {
        if !self.paste_special {
            return;
        }
        let mut open = true;
        let mut apply = false;
        let mut link = false;
        let mut close = false;
        ui_kit::window(ctx, "选择性粘贴", 420.0).open(&mut open).show(ctx, |ui| {
            ui.set_width(390.0);
            ui.label(RichText::new("粘贴").font(ui_kit::bold(ui_kit::T_BODY)));
            ui.add_space(2.0);
            ui.columns(2, |cols| {
                for &(v, lbl) in &[
                    (0u8, "全部"),
                    (1, "数值"),
                    (2, "公式"),
                    (3, "格式"),
                    (8, "批注"),
                ] {
                    cols[0].radio_value(&mut self.ps_kind, v, lbl);
                }
                for &(v, lbl) in &[
                    (9u8, "数据验证"),
                    (4, "除边框外全部"),
                    (5, "列宽"),
                    (6, "值和数字格式"),
                    (7, "公式和数字格式"),
                ] {
                    cols[1].radio_value(&mut self.ps_kind, v, lbl);
                }
            });

            ui.separator();
            ui.label(RichText::new("运算").font(ui_kit::bold(ui_kit::T_BODY)));
            ui.horizontal_wrapped(|ui| {
                for &(v, lbl) in &[(0u8, "无"), (1, "加"), (2, "减"), (3, "乘"), (4, "除")] {
                    ui.radio_value(&mut self.ps_op, v, lbl);
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.ps_skip_blanks, "跳过空单元格");
                ui.add_space(16.0);
                ui.checkbox(&mut self.ps_transpose, "转置（行列互换）");
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui_kit::secondary_button(ui, "粘贴链接").clicked() {
                    link = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui_kit::secondary_button(ui, "取消").clicked() {
                        close = true;
                    }
                    if ui_kit::primary_button(ui, "确定").clicked() {
                        apply = true;
                    }
                });
            });
        });

        if link {
            self.paste_link();
            close = true;
        } else if apply {
            self.paste_special_apply(ctx);
            close = true;
        }
        if close {
            open = false;
        }
        self.paste_special = open;
    }

    // ========================================================================
    //  向上 / 向左填充 & 序列对话框
    // ========================================================================

    pub(crate) fn fill_up(&mut self) {
        if self.edit_blocked() {
            return;
        }
        self.commit_edit(None);
        let (r1, c1, r2, c2) = self.sel_rect();
        if r2 == r1 {
            self.status = "请选中两行以上再向上填充".into();
            return;
        }
        let area = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
        let max_area = if self.net.is_none() {
            sheetcore::undo::MAX_DENSE_RECT_KEYS as u64
        } else {
            200_000
        };
        if area > max_area {
            self.status = format!(
                "填充范围过大（单次最多支持 {} 万个单元格）",
                max_area / 10_000
            );
            return;
        }
        let si = self.sheet_idx;
        let has_hidden = !self.wb.sheet(si).hidden_rows.is_empty()
            && self.wb.sheet(si).hidden_rows.intersects_range(r1, r2);
        let src_row = if has_hidden {
            let sh = self.wb.sheet(si);
            match (r1..=r2).rev().find(|r| !sh.hidden_rows.contains(r)) {
                Some(r) if r > r1 => r,
                _ => {
                    self.status = "选区内没有可填充的可见行（筛选隐藏行已保留）".into();
                    return;
                }
            }
        } else {
            r2
        };
        if self.net.is_none() && area > sheetcore::undo::COW_BATCH_THRESHOLD as u64 {
            if !self.check_sparse_rect_preflight(si, (r1, c1, src_row - 1, c2), &[], "向上填充") {
                return;
            }
            let before_sheet = self.wb.sheet(si).clone();
            let mut formulas = sheetcore::refs::FillFormulas::default();
            let mut changed = 0usize;
            let mut sample = Vec::with_capacity(sheetcore::revision::MAX_CELLS);
            let mut has_formula = false;
            {
                let sh = self.wb.sheet_mut(si);
                let vis_rows = if has_hidden {
                    crate::visible_row_intervals_in_range(&sh.hidden_rows, r1, src_row - 1)
                } else {
                    vec![(r1, src_row - 1)]
                };
                for c in c1..=c2 {
                    let src = sh.get(src_row, c).map(|cell| cell.into_owned());
                    let sid = sh.style_of(src_row, c);
                    if let Some(source) = src {
                        has_formula |= source.formula.is_some();
                        for &(vr1, vr2) in &vis_rows {
                            for r in vr1..=vr2 {
                                let after = Cell {
                                    value: source.value.clone(),
                                    formula: source.formula.as_deref().map(|f| {
                                        formulas.translate(f, r as i64 - src_row as i64, 0).into()
                                    }),
                                };
                                if sample.len() < sheetcore::revision::MAX_CELLS {
                                    let before = sh.get(r, c).map(|x| x.into_owned()).unwrap_or_default();
                                    let before_style = sh.style_of(r, c);
                                    sample.push(sheetcore::undo::CellDelta {
                                        sheet: si,
                                        row: r,
                                        col: c,
                                        before,
                                        after: after.clone(),
                                        before_style,
                                        after_style: sid,
                                    });
                                }
                                sh.set(r, c, after);
                                changed += 1;
                            }
                            sh.set_style_rect(vr1, c, vr2, c, sid);
                        }
                    } else {
                        for &(vr1, vr2) in &vis_rows {
                            for r in vr1..=vr2 {
                                if sh.get(r, c).is_some() {
                                    if sample.len() < sheetcore::revision::MAX_CELLS {
                                        let before = sh.get(r, c).map(|x| x.into_owned()).unwrap_or_default();
                                        let before_style = sh.style_of(r, c);
                                        sample.push(sheetcore::undo::CellDelta {
                                            sheet: si,
                                            row: r,
                                            col: c,
                                            before,
                                            after: Cell::default(),
                                            before_style,
                                            after_style: sid,
                                        });
                                    }
                                    sh.remove(r, c);
                                    changed += 1;
                                }
                            }
                            sh.set_style_rect(vr1, c, vr2, c, sid);
                        }
                    }
                }
            }
            self.record_revision_sample("向上填充", &sample, changed.max(1));
            self.undo.push_cow_batch(
                "向上填充",
                si,
                &before_sheet,
                self.wb.sheet(si),
                (si, self.sel.0, self.sel.1),
            );
            self.wb.calc_manual |= self.manual_calc;
            if has_formula || !self.deps.is_empty() {
                self.recalc_all_and_deps_adaptive();
            }
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.set_repeat_action(crate::RepeatAction::FillUp);
            self.tasks.last_edit = Some(std::time::Instant::now());
            std::thread::spawn(move || drop(before_sheet));
            self.status = format!("已用底行向上填充 {}-{} 行", r1 + 1, r2 + 1);
            return;
        }
        let mut gate = sheetcore::undo::rect_keys(r1, c1, src_row - 1, c2);
        if has_hidden {
            let sh = self.wb.sheet(si);
            gate.retain(|&(r, _)| !sh.hidden_rows.contains(&r));
        }
        if gate.is_empty() {
            self.status = "选区内没有可填充的可见行（筛选隐藏行已保留）".into();
            return;
        }
        if !self.write_preflight(&gate, "向上填充") {
            return;
        }
        let mut formulas = sheetcore::refs::FillFormulas::default();
        let txn = self.txn_begin(&gate);
        let mut touched = Vec::new();
        for c in c1..=c2 {
            let src = self.wb.sheet(self.sheet_idx).get(src_row, c).map(|c| c.into_owned());
            let sid = self.wb.sheet(self.sheet_idx).style_of(src_row, c);
            for r in r1..src_row {
                if has_hidden && self.wb.sheet(self.sheet_idx).hidden_rows.contains(&r) {
                    continue;
                }
                if let Some(ref s) = src {
                    let mut cell = s.clone();
                    if let Some(ref f) = s.formula {
                        cell.formula = Some(formulas.translate(f, r as i64 - src_row as i64, 0).into());
                    }
                    self.wb.sheet_mut(self.sheet_idx).set(r, c, cell);
                } else {
                    self.wb.sheet_mut(self.sheet_idx).remove(r, c);
                }
                self.wb.sheet_mut(self.sheet_idx).set_style(r, c, sid);
                touched.push((self.sheet_idx, r, c));
            }
        }
        self.txn_end("向上填充", txn);
        self.recalc_and_broadcast(&touched);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.set_repeat_action(crate::RepeatAction::FillUp);
        self.status = format!("已用底行向上填充 {}-{} 行", r1 + 1, r2 + 1);
    }

    pub(crate) fn fill_left(&mut self) {
        if self.edit_blocked() {
            return;
        }
        self.commit_edit(None);
        let (r1, c1, r2, c2) = self.sel_rect();
        if c2 == c1 {
            self.status = "请选中两列以上再向左填充".into();
            return;
        }
        let area = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
        let max_area = if self.net.is_none() {
            sheetcore::undo::MAX_DENSE_RECT_KEYS as u64
        } else {
            200_000
        };
        if area > max_area {
            self.status = format!(
                "填充范围过大（单次最多支持 {} 万个单元格）",
                max_area / 10_000
            );
            return;
        }
        let si = self.sheet_idx;
        let has_hidden = !self.wb.sheet(si).hidden_rows.is_empty()
            && self.wb.sheet(si).hidden_rows.intersects_range(r1, r2);
        if self.net.is_none() && area > sheetcore::undo::COW_BATCH_THRESHOLD as u64 {
            if !self.check_sparse_rect_preflight(si, (r1, c1, r2, c2 - 1), &[], "向左填充") {
                return;
            }
            let before_sheet = self.wb.sheet(si).clone();
            let mut formulas = sheetcore::refs::FillFormulas::default();
            let mut changed = 0usize;
            let mut sample = Vec::with_capacity(sheetcore::revision::MAX_CELLS);
            let mut has_formula = false;
            {
                let sh = self.wb.sheet_mut(si);
                let vis_rows = if has_hidden {
                    crate::visible_row_intervals_in_range(&sh.hidden_rows, r1, r2)
                } else {
                    vec![(r1, r2)]
                };
                for &(vr1, vr2) in &vis_rows {
                    for r in vr1..=vr2 {
                        let src = sh.get(r, c2).map(|cell| cell.into_owned());
                        let sid = sh.style_of(r, c2);
                        if let Some(source) = src {
                            has_formula |= source.formula.is_some();
                            for c in c1..c2 {
                                let after = Cell {
                                    value: source.value.clone(),
                                    formula: source.formula.as_deref().map(|f| {
                                        formulas.translate(f, 0, c as i64 - c2 as i64).into()
                                    }),
                                };
                                if sample.len() < sheetcore::revision::MAX_CELLS {
                                    let before = sh.get(r, c).map(|x| x.into_owned()).unwrap_or_default();
                                    let before_style = sh.style_of(r, c);
                                    sample.push(sheetcore::undo::CellDelta {
                                        sheet: si,
                                        row: r,
                                        col: c,
                                        before,
                                        after: after.clone(),
                                        before_style,
                                        after_style: sid,
                                    });
                                }
                                sh.set(r, c, after);
                                changed += 1;
                            }
                        } else {
                            for c in c1..c2 {
                                if sh.get(r, c).is_some() {
                                    if sample.len() < sheetcore::revision::MAX_CELLS {
                                        let before = sh.get(r, c).map(|x| x.into_owned()).unwrap_or_default();
                                        let before_style = sh.style_of(r, c);
                                        sample.push(sheetcore::undo::CellDelta {
                                            sheet: si,
                                            row: r,
                                            col: c,
                                            before,
                                            after: Cell::default(),
                                            before_style,
                                            after_style: sid,
                                        });
                                    }
                                    sh.remove(r, c);
                                    changed += 1;
                                }
                            }
                        }
                        sh.set_style_rect(r, c1, r, c2 - 1, sid);
                    }
                }
            }
            self.record_revision_sample("向左填充", &sample, changed.max(1));
            self.undo.push_cow_batch(
                "向左填充",
                si,
                &before_sheet,
                self.wb.sheet(si),
                (si, self.sel.0, self.sel.1),
            );
            self.wb.calc_manual |= self.manual_calc;
            if has_formula || !self.deps.is_empty() {
                self.recalc_all_and_deps_adaptive();
            }
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.set_repeat_action(crate::RepeatAction::FillLeft);
            self.tasks.last_edit = Some(std::time::Instant::now());
            std::thread::spawn(move || drop(before_sheet));
            self.status = format!("已用最右列向左填充至 {} 列", col_label(c1));
            return;
        }
        let mut gate = sheetcore::undo::rect_keys(r1, c1, r2, c2 - 1);
        if has_hidden {
            let sh = self.wb.sheet(si);
            gate.retain(|&(r, _)| !sh.hidden_rows.contains(&r));
        }
        if gate.is_empty() {
            self.status = "选区内没有可填充的可见行（筛选隐藏行已保留）".into();
            return;
        }
        if !self.write_preflight(&gate, "向左填充") {
            return;
        }
        let mut formulas = sheetcore::refs::FillFormulas::default();
        let txn = self.txn_begin(&gate);
        let mut touched = Vec::new();
        for r in r1..=r2 {
            if has_hidden && self.wb.sheet(self.sheet_idx).hidden_rows.contains(&r) {
                continue;
            }
            let src = self.wb.sheet(self.sheet_idx).get(r, c2).map(|c| c.into_owned());
            let sid = self.wb.sheet(self.sheet_idx).style_of(r, c2);
            for c in c1..c2 {
                if let Some(ref s) = src {
                    let mut cell = s.clone();
                    if let Some(ref f) = s.formula {
                        cell.formula = Some(formulas.translate(f, 0, c as i64 - c2 as i64).into());
                    }
                    self.wb.sheet_mut(self.sheet_idx).set(r, c, cell);
                } else {
                    self.wb.sheet_mut(self.sheet_idx).remove(r, c);
                }
                self.wb.sheet_mut(self.sheet_idx).set_style(r, c, sid);
                touched.push((self.sheet_idx, r, c));
            }
        }
        self.txn_end("向左填充", txn);
        self.recalc_and_broadcast(&touched);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.set_repeat_action(crate::RepeatAction::FillLeft);
        self.status = format!("已用最右列向左填充至 {} 列", col_label(c1));
    }

    pub(crate) fn open_fill_series_dlg(&mut self) {
        let (r1, c1, r2, c2) = self.sel_rect();
        let by_row = c2 - c1 > r2 - r1;
        self.fill_series_dlg = Some(FillSeriesDlgState {
            by_row,
            kind: 0,
            date_unit: 0,
            step: "1".into(),
            stop: String::new(),
        });
    }

    pub(crate) fn fill_quick_series(&mut self, preset: SeriesPreset) {
        let (r1, c1, r2, c2) = self.sel_rect();
        if r1 == r2 && c1 == c2 {
            self.fill_series_dlg = Some(FillSeriesDlgState {
                by_row: false,
                kind: match preset {
                    SeriesPreset::Linear => 0,
                    _ => 2,
                },
                date_unit: match preset {
                    SeriesPreset::Weekday => 1,
                    SeriesPreset::Month => 2,
                    SeriesPreset::Year => 3,
                    _ => 0,
                },
                step: "1".into(),
                stop: String::new(),
            });
            self.status = "当前仅选中一格：请输入终止值或先框选要填充的区域".into();
            return;
        }
        let by_row = c2 - c1 > r2 - r1;
        let cfg = FillSeriesDlgState {
            by_row,
            kind: match preset {
                SeriesPreset::Linear => 0,
                _ => 2,
            },
            date_unit: match preset {
                SeriesPreset::Weekday => 1,
                SeriesPreset::Month => 2,
                SeriesPreset::Year => 3,
                _ => 0,
            },
            step: "1".into(),
            stop: String::new(),
        };
        self.apply_fill_series(&cfg);
    }

    fn apply_fill_series(&mut self, cfg: &FillSeriesDlgState) {
        self.commit_edit(None);
        let step = cfg.step.trim().parse::<f64>().unwrap_or(1.0);
        let is_date = cfg.kind == 2;
        let date_sys = self.wb.date_system;
        let parse_val = |raw: &str| -> Option<f64> {
            let t = raw.trim();
            if t.is_empty() {
                return None;
            }
            if let Ok(n) = t.parse::<f64>() {
                return Some(n);
            }
            if is_date {
                let p = sheetcore::input_parse::parse_cell_input(t, "", date_sys);
                if let Value::Number(serial) = p.value {
                    if p.auto_format.is_some() {
                        return Some(serial);
                    }
                }
            }
            None
        };
        let stop = parse_val(&cfg.stop);
        let (r1, c1, mut r2, mut c2) = self.sel_rect();
        let start_val = self
            .wb
            .sheet(self.sheet_idx)
            .get(r1, c1)
            .and_then(|c| match c.value {
                Value::Number(n) => Some(n),
                Value::Text(ref t) => parse_val(t),
                _ => None,
            })
            .unwrap_or_else(|| if is_date { 45658.0 } else { 1.0 });

        let step_i = step.round() as i64;
        let advance_serial = |idx: u32, base: f64| -> f64 {
            if !is_date {
                match cfg.kind {
                    1 => base * step.powi(idx as i32),
                    _ => base + step * idx as f64,
                }
            } else {
                match cfg.date_unit {
                    1 => {
                        let dir = if step_i >= 0 { 1 } else { -1 };
                        let total = (idx as i64) * step_i.abs();
                        if total == 0 {
                            base
                        } else {
                            crate::fill_seq::next_date_serial(
                                base,
                                crate::fill_seq::FillMode::Weekdays,
                                dir,
                                total,
                                date_sys,
                            )
                        }
                    }
                    2 => {
                        if idx == 0 || step_i == 0 {
                            base
                        } else {
                            let dir = if step_i >= 0 { 1 } else { -1 };
                            crate::fill_seq::next_date_serial(
                                base,
                                crate::fill_seq::FillMode::Months,
                                dir,
                                (idx as i64) * step_i.abs(),
                                date_sys,
                            )
                        }
                    }
                    3 => {
                        if idx == 0 || step_i == 0 {
                            base
                        } else {
                            let dir = if step_i >= 0 { 1 } else { -1 };
                            crate::fill_seq::next_date_serial(
                                base,
                                crate::fill_seq::FillMode::Years,
                                dir,
                                (idx as i64) * step_i.abs(),
                                date_sys,
                            )
                        }
                    }
                    _ => base + step * idx as f64,
                }
            }
        };

        if r1 == r2 && c1 == c2 {
            if let Some(lim) = stop {
                let cap = if self.net.is_none() {
                    if cfg.by_row {
                        16_384 - c1
                    } else {
                        1_048_576 - r1
                    }
                } else {
                    5_000
                };
                let count = if is_date {
                    // 按实际日期单元精确递增，避免月/年/工作日序列用天数近似产生越界或截断
                    let mut k = 1u32;
                    while k < cap && step_i != 0 {
                        let nxt = advance_serial(k, start_val);
                        if (step_i > 0 && nxt > lim + 1e-9) || (step_i < 0 && nxt < lim - 1e-9) {
                            break;
                        }
                        k += 1;
                    }
                    k
                } else if cfg.kind == 1 {
                    let mut cur = start_val;
                    let mut k = 1u32;
                    while k < cap {
                        cur *= step;
                        if step <= 0.0
                            || (step - 1.0).abs() < 1e-9
                            || (step > 1.0 && cur > lim)
                            || (step > 0.0 && step < 1.0 && cur < lim)
                        {
                            break;
                        }
                        k += 1;
                    }
                    k
                } else if step.abs() > 1e-9 {
                    (((lim - start_val) / step).floor().max(0.0) as u32 + 1).min(cap)
                } else {
                    1
                };
                if cfg.by_row {
                    c2 = (c1 + count.saturating_sub(1)).min(16_383);
                } else {
                    r2 = (r1 + count.saturating_sub(1)).min(1_048_575);
                }
            }
        }
        let area = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
        if area > 200_000 {
            self.status = "序列填充区域超过 20 万格，请缩小选区或指定终止值".into();
            return;
        }

        let si = self.sheet_idx;
        let has_hidden = !self.wb.sheet(si).hidden_rows.is_empty()
            && self.wb.sheet(si).hidden_rows.intersects_range(r1, r2);
        let src_sid = self.wb.sheet(si).style_of(r1, c1);
        let target_sid = if is_date && self.wb.styles.get(src_sid).format.is_empty() {
            let mut st = self.wb.styles.get(src_sid).clone();
            st.format = "yyyy-mm-dd".into();
            self.wb.styles.intern(st) as u16
        } else {
            src_sid
        };

        if self.net.is_none() && area > sheetcore::undo::COW_BATCH_THRESHOLD as u64 {
            if !self.check_sparse_rect_preflight(si, (r1, c1, r2, c2), &[], "序列填充") {
                return;
            }
            let before_sheet = self.wb.sheet(si).clone();
            let mut written = 0usize;
            let mut sample = Vec::with_capacity(sheetcore::revision::MAX_CELLS);
            {
                let sh = self.wb.sheet_mut(si);
                let vis_rows = if has_hidden {
                    crate::visible_row_intervals_in_range(&sh.hidden_rows, r1, r2)
                } else {
                    vec![(r1, r2)]
                };
                if cfg.by_row {
                    for &(vr1, vr2) in &vis_rows {
                        for r in vr1..=vr2 {
                            let base = sh
                                .get(r, c1)
                                .and_then(|c| match c.value {
                                    Value::Number(n) => Some(n),
                                    _ => None,
                                })
                                .unwrap_or(start_val);
                            let mut last_c = None;
                            for c in c1..=c2 {
                                let v = advance_serial(c - c1, base);
                                if let Some(lim) = stop {
                                    if (step >= 0.0 && v > lim + 1e-9) || (step < 0.0 && v < lim - 1e-9)
                                    {
                                        break;
                                    }
                                }
                                let after = Cell {
                                    value: Value::Number(v),
                                    formula: None,
                                };
                                if sample.len() < sheetcore::revision::MAX_CELLS {
                                    let before = sh.get(r, c).map(|x| x.into_owned()).unwrap_or_default();
                                    let before_style = sh.style_of(r, c);
                                    sample.push(sheetcore::undo::CellDelta {
                                        sheet: si,
                                        row: r,
                                        col: c,
                                        before,
                                        after: after.clone(),
                                        before_style,
                                        after_style: if target_sid != 0 { target_sid } else { before_style },
                                    });
                                }
                                sh.set(r, c, after);
                                last_c = Some(c);
                                written += 1;
                            }
                            if target_sid != 0
                                && let Some(lc) = last_c
                            {
                                sh.set_style_rect(r, c1, r, lc, target_sid);
                            }
                        }
                    }
                } else {
                    for c in c1..=c2 {
                        let first_r = vis_rows.first().map(|&(a, _)| a).unwrap_or(r1);
                        let base = sh
                            .get(first_r, c)
                            .and_then(|cell| match cell.value {
                                Value::Number(n) => Some(n),
                                _ => None,
                            })
                            .unwrap_or(start_val);
                        let mut step_idx = 0u32;
                        'col_fill: for &(vr1, vr2) in &vis_rows {
                            let mut last_r = None;
                            for r in vr1..=vr2 {
                                let v = advance_serial(step_idx, base);
                                step_idx += 1;
                                if let Some(lim) = stop {
                                    if (step >= 0.0 && v > lim + 1e-9) || (step < 0.0 && v < lim - 1e-9)
                                    {
                                        if target_sid != 0
                                            && let Some(lr) = last_r
                                        {
                                            sh.set_style_rect(vr1, c, lr, c, target_sid);
                                        }
                                        break 'col_fill;
                                    }
                                }
                                let after = Cell {
                                    value: Value::Number(v),
                                    formula: None,
                                };
                                if sample.len() < sheetcore::revision::MAX_CELLS {
                                    let before = sh.get(r, c).map(|x| x.into_owned()).unwrap_or_default();
                                    let before_style = sh.style_of(r, c);
                                    sample.push(sheetcore::undo::CellDelta {
                                        sheet: si,
                                        row: r,
                                        col: c,
                                        before,
                                        after: after.clone(),
                                        before_style,
                                        after_style: if target_sid != 0 { target_sid } else { before_style },
                                    });
                                }
                                sh.set(r, c, after);
                                last_r = Some(r);
                                written += 1;
                            }
                            if target_sid != 0
                                && let Some(lr) = last_r
                            {
                                sh.set_style_rect(vr1, c, lr, c, target_sid);
                            }
                        }
                    }
                }
            }
            if written > 0 {
                self.record_revision_sample("序列填充", &sample, written);
                self.undo.push_cow_batch(
                    "序列填充",
                    si,
                    &before_sheet,
                    self.wb.sheet(si),
                    (si, self.sel.0, self.sel.1),
                );
                self.wb.calc_manual |= self.manual_calc;
                if !self.deps.is_empty() {
                    self.recalc_all_and_deps_adaptive();
                }
                self.tasks.last_edit = Some(std::time::Instant::now());
            }
            std::thread::spawn(move || drop(before_sheet));
            self.sel_end = (r2, c2);
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.status = format!("已完成序列填充（{written} 格）");
            return;
        }

        let mut keys = sheetcore::undo::rect_keys(r1, c1, r2, c2);
        if has_hidden {
            let sh = self.wb.sheet(si);
            keys.retain(|&(r, _)| !sh.hidden_rows.contains(&r));
        }
        if keys.is_empty() {
            self.status = "选区内没有可填充的可见行（筛选隐藏行已保留）".into();
            return;
        }
        if !self.write_preflight(&keys, "序列填充") {
            return;
        }
        let txn = self.txn_begin(&keys);
        let mut touched = Vec::new();

        if cfg.by_row {
            for r in r1..=r2 {
                if has_hidden && self.wb.sheet(self.sheet_idx).hidden_rows.contains(&r) {
                    continue;
                }
                let base = self
                    .wb
                    .sheet(self.sheet_idx)
                    .get(r, c1)
                    .and_then(|c| match c.value {
                        Value::Number(n) => Some(n),
                        _ => None,
                    })
                    .unwrap_or(start_val);
                for c in c1..=c2 {
                    let v = advance_serial(c - c1, base);
                    if let Some(lim) = stop {
                        if (step >= 0.0 && v > lim + 1e-9) || (step < 0.0 && v < lim - 1e-9) {
                            break;
                        }
                    }
                    self.wb.sheet_mut(self.sheet_idx).set(
                        r,
                        c,
                        Cell {
                            value: Value::Number(v),
                            formula: None,
                        },
                    );
                    if target_sid != 0 {
                        self.wb.sheet_mut(self.sheet_idx).set_style(r, c, target_sid);
                    }
                    touched.push((self.sheet_idx, r, c));
                }
            }
        } else {
            for c in c1..=c2 {
                let first_r = (r1..=r2)
                    .find(|r| !has_hidden || !self.wb.sheet(self.sheet_idx).hidden_rows.contains(r))
                    .unwrap_or(r1);
                let base = self
                    .wb
                    .sheet(self.sheet_idx)
                    .get(first_r, c)
                    .and_then(|cell| match cell.value {
                        Value::Number(n) => Some(n),
                        _ => None,
                    })
                    .unwrap_or(start_val);
                let mut step_idx = 0u32;
                for r in r1..=r2 {
                    if has_hidden && self.wb.sheet(self.sheet_idx).hidden_rows.contains(&r) {
                        continue;
                    }
                    let v = advance_serial(step_idx, base);
                    step_idx += 1;
                    if let Some(lim) = stop {
                        if (step >= 0.0 && v > lim + 1e-9) || (step < 0.0 && v < lim - 1e-9) {
                            break;
                        }
                    }
                    self.wb.sheet_mut(self.sheet_idx).set(
                        r,
                        c,
                        Cell {
                            value: Value::Number(v),
                            formula: None,
                        },
                    );
                    if target_sid != 0 {
                        self.wb.sheet_mut(self.sheet_idx).set_style(r, c, target_sid);
                    }
                    touched.push((self.sheet_idx, r, c));
                }
            }
        }

        self.txn_end("序列填充", txn);
        self.recalc_and_broadcast(&touched);
        self.sel_end = (r2, c2);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已完成序列填充（{} 格）", touched.len());
    }

    pub(crate) fn draw_fill_series_dlg(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.fill_series_dlg.take() else { return };
        let mut open = true;
        let mut apply = false;
        let mut close = false;
        ui_kit::window(ctx, "序列", 370.0).open(&mut open).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("序列产生在").font(ui_kit::bold(ui_kit::T_BODY)));
                    ui.radio_value(&mut st.by_row, true, "行 (R)");
                    ui.radio_value(&mut st.by_row, false, "列 (C)");
                });
                ui.add_space(18.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("类型").font(ui_kit::bold(ui_kit::T_BODY)));
                    ui.radio_value(&mut st.kind, 0, "等差序列");
                    ui.radio_value(&mut st.kind, 1, "等比序列");
                    ui.radio_value(&mut st.kind, 2, "日期");
                });
                ui.add_space(18.0);
                ui.add_enabled_ui(st.kind == 2, |ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("日期单位").font(ui_kit::bold(ui_kit::T_BODY)));
                        ui.radio_value(&mut st.date_unit, 0, "日");
                        ui.radio_value(&mut st.date_unit, 1, "工作日");
                        ui.radio_value(&mut st.date_unit, 2, "月");
                        ui.radio_value(&mut st.date_unit, 3, "年");
                    });
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("步长值:");
                ui.add(egui::TextEdit::singleline(&mut st.step).desired_width(80.0));
                ui.add_space(12.0);
                ui.label("终止值:");
                ui.add(egui::TextEdit::singleline(&mut st.stop).desired_width(90.0).hint_text("可选"));
            });
            ui_kit::footer(ui, |ui| {
                if ui_kit::primary_button(ui, "确定").clicked() {
                    apply = true;
                }
                if ui_kit::secondary_button(ui, "取消").clicked() {
                    close = true;
                }
            });
        });
        if apply {
            self.apply_fill_series(&st);
            close = true;
        }
        if open && !close {
            self.fill_series_dlg = Some(st);
        }
    }

    // ========================================================================
    //  定位 / 定位条件
    // ========================================================================

    pub(crate) fn open_goto_dlg(&mut self, tab: u8) {
        self.goto_dlg = Some(GotoDlgState {
            tab,
            target_ref: cell_name(self.sel.0, self.sel.1),
            special: SpecialTarget::Blanks,
            sub_numbers: true,
            sub_text: true,
            sub_bools: true,
            sub_errors: true,
        });
    }

    pub(crate) fn select_special(&mut self, target: SpecialTarget) {
        self.select_special_with_subtypes(target, (true, true, true, true));
    }

    pub(crate) fn select_special_with_subtypes(
        &mut self,
        target: SpecialTarget,
        subs: (bool, bool, bool, bool),
    ) {
        self.commit_edit(None);
        self.nav_rect = None;
        let si = self.sheet_idx;
        // 特殊快捷目标：直接求目标区域/对象，无需逐格扫描
        match target {
            SpecialTarget::CurrentRegion => {
                let (r1, c1, r2, c2) = self.current_region_rect(self.sel.0, self.sel.1);
                self.sel = (r1, c1);
                self.sel_end = (r2, c2);
                self.extra_sel.clear();
                self.ensure_visible();
                self.status = format!("已选中当前区域 {}:{}", cell_name(r1, c1), cell_name(r2, c2));
                return;
            }
            SpecialTarget::LastCell => {
                let (mr, mc) = self.used_end();
                self.sel = (mr, mc);
                self.sel_end = (mr, mc);
                self.extra_sel.clear();
                self.ensure_visible();
                self.status = format!("已定位到最后一个单元格 {}", cell_name(mr, mc));
                return;
            }
            SpecialTarget::CurrentArray => {
                let (ar, ac) = self.sel;
                let sh = self.wb.sheet(si);
                let found = sh
                    .arrays
                    .iter()
                    .find_map(|&(_, rect)| {
                        (ar >= rect.0 && ar <= rect.2 && ac >= rect.1 && ac <= rect.3).then_some(rect)
                    })
                    .or_else(|| {
                        sh.spill_shapes.iter().find_map(|(&anchor, &(h, w))| {
                            let r2 = anchor.row.saturating_add(h.saturating_sub(1));
                            let c2 = anchor.col.saturating_add(w.saturating_sub(1));
                            (ar >= anchor.row && ar <= r2 && ac >= anchor.col && ac <= c2)
                                .then_some((anchor.row, anchor.col, r2, c2))
                        })
                    });
                if let Some((r1, c1, r2, c2)) = found {
                    self.sel = (r1, c1);
                    self.sel_end = (r2, c2);
                    self.extra_sel.clear();
                    self.ensure_visible();
                    self.status = format!("已选中当前数组 {}:{}", cell_name(r1, c1), cell_name(r2, c2));
                } else {
                    self.status = "当前单元格不属于任何数组或溢出区域".into();
                }
                return;
            }
            SpecialTarget::Objects => {
                let sh = self.wb.sheet(si);
                if !sh.pictures.is_empty() {
                    self.pic_sel = Some(0);
                    self.status = format!("已选中图片对象（共 {} 个）", sh.pictures.len());
                } else if !sh.textboxes.is_empty() {
                    self.tb_sel = Some(0);
                    self.status = format!("已选中文本框对象（共 {} 个）", sh.textboxes.len());
                } else if !sh.charts.is_empty() {
                    self.status = format!("本表有 {} 个图表对象", sh.charts.len());
                } else {
                    self.status = "未找到任何对象".into();
                }
                return;
            }
            SpecialTarget::Precedents => {
                self.jump_precedents_or_dependents(true, false);
                return;
            }
            SpecialTarget::Dependents => {
                self.jump_precedents_or_dependents(false, false);
                return;
            }
            _ => {}
        }

        let sh = self.wb.sheet(si);
        let sel_rects = self.sel_rects();
        let multi = sel_rects.len() > 1
            || sel_rects
                .first()
                .is_some_and(|&(r1, c1, r2, c2)| r1 != r2 || c1 != c2);
        let search_rects: Vec<(u32, u32, u32, u32)> = if multi {
            sel_rects
        } else {
            let (mut mr, mut mc) = self.used_end();
            for k in sh.notes.keys() {
                mr = mr.max(k.row);
                mc = mc.max(k.col);
            }
            for blk in &sh.cf.blocks {
                for &(a, b, cc, d) in &blk.ranges {
                    mr = mr.max(cc.min(2000));
                    mc = mc.max(d.min(200));
                    let _ = (a, b);
                }
            }
            for dv in &sh.dv {
                for &(a, b, cc, d) in &dv.ranges {
                    mr = mr.max(cc.min(2000));
                    mc = mc.max(d.min(200));
                    let _ = (a, b);
                }
            }
            vec![(0, 0, mr, mc)]
        };
        let total_area: u64 = search_rects
            .iter()
            .map(|&(r1, c1, r2, c2)| u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1))
            .fold(0u64, u64::saturating_add);
        let supports_sparse = matches!(
            target,
            SpecialTarget::VisibleCells
                | SpecialTarget::Formulas
                | SpecialTarget::Constants
                | SpecialTarget::Errors
                | SpecialTarget::Comments
                | SpecialTarget::CondFormats
                | SpecialTarget::DataValidation
        );
        if !supports_sparse && total_area > 500_000 {
            self.status = "搜索范围超过 50 万格，请先框选需要定位的区域".into();
            return;
        }
        let in_search = |r: u32, c: u32| -> bool {
            search_rects
                .iter()
                .any(|&(r1, c1, r2, c2)| r >= r1 && r <= r2 && c >= c1 && c <= c2)
        };
        let (sub_num, sub_txt, sub_bool, sub_err) = subs;
        let matches_sub = |v: &Value| -> bool {
            match v {
                Value::Number(_) => sub_num,
                Value::Text(_) => sub_txt,
                Value::Bool(_) => sub_bool,
                Value::Error(_) => sub_err,
                Value::Empty => sub_num && sub_txt && sub_bool && sub_err,
            }
        };

        let in_cf = |r: u32, c: u32| -> bool {
            sh.cf.blocks.iter().any(|blk| {
                blk.ranges
                    .iter()
                    .any(|&(a, b, cc, d)| r >= a && r <= cc && c >= b && c <= d)
            })
        };
        let in_dv = |r: u32, c: u32| -> bool {
            sh.dv.iter().any(|dv| {
                dv.ranges
                    .iter()
                    .any(|&(a, b, cc, d)| r >= a && r <= cc && c >= b && c <= d)
            })
        };
        let active_r = self.sel.0;
        let active_c = self.sel.1;

        let mut count = 0usize;
        let mut rects: Vec<(u32, u32, u32, u32)> = Vec::new();
        let pack_points = |pts: &mut Vec<(u32, u32)>, out: &mut Vec<(u32, u32, u32, u32)>| {
            pts.sort_unstable();
            pts.dedup();
            for &(r, c) in pts.iter() {
                if let Some(last) = out.last_mut() {
                    if last.0 == r && last.2 == r && last.3.saturating_add(1) == c {
                        last.3 = c;
                        continue;
                    }
                }
                out.push((r, c, r, c));
            }
        };
        if target == SpecialTarget::VisibleCells {
            for &(r1, c1, r2, c2) in &search_rects {
                // 同时跳过筛选隐藏行、手动隐藏行与隐藏列
                let mut vis_cols: Vec<(u32, u32)> = Vec::new();
                let mut cs: Option<u32> = None;
                for c in c1..=c2 {
                    let hidden_c = sh.hidden_cols.contains(&c);
                    if !hidden_c {
                        if cs.is_none() {
                            cs = Some(c);
                        }
                    } else if let Some(s) = cs.take() {
                        vis_cols.push((s, c - 1));
                    }
                }
                if let Some(s) = cs {
                    vis_cols.push((s, c2));
                }
                let mut rs: Option<u32> = None;
                let mut vis_rows: Vec<(u32, u32)> = Vec::new();
                for r in r1..=r2 {
                    let hidden_r = sh.hidden_rows.contains(&r) || sh.hidden_rows_user.contains(&r);
                    if !hidden_r {
                        if rs.is_none() {
                            rs = Some(r);
                        }
                    } else if let Some(s) = rs.take() {
                        vis_rows.push((s, r - 1));
                    }
                }
                if let Some(s) = rs {
                    vis_rows.push((s, r2));
                }
                for &(vr1, vr2) in &vis_rows {
                    for &(vc1, vc2) in &vis_cols {
                        count = count.saturating_add(
                            (vr2 - vr1 + 1) as usize * (vc2 - vc1 + 1) as usize,
                        );
                        rects.push((vr1, vc1, vr2, vc2));
                    }
                }
            }
        } else if matches!(
            target,
            SpecialTarget::Formulas
                | SpecialTarget::Constants
                | SpecialTarget::Errors
                | SpecialTarget::Comments
        ) {
            let mut pts: Vec<(u32, u32)> = Vec::new();
            match target {
                SpecialTarget::Formulas => {
                    for (cr, cell) in sh.formula_cells() {
                        if in_search(cr.row, cr.col) && matches_sub(&cell.value) {
                            pts.push((cr.row, cr.col));
                        }
                    }
                }
                SpecialTarget::Constants => {
                    for (cr, cell) in sh.iter() {
                        if cell.formula.is_none()
                            && !matches!(cell.value, Value::Empty)
                            && in_search(cr.row, cr.col)
                            && matches_sub(&cell.value)
                        {
                            pts.push((cr.row, cr.col));
                        }
                    }
                }
                SpecialTarget::Errors => {
                    for (cr, cell) in sh.iter() {
                        if matches!(cell.value, Value::Error(_)) && in_search(cr.row, cr.col) {
                            pts.push((cr.row, cr.col));
                        }
                    }
                }
                SpecialTarget::Comments => {
                    for cr in sh.notes.keys() {
                        if in_search(cr.row, cr.col) {
                            pts.push((cr.row, cr.col));
                        }
                    }
                }
                _ => {}
            }
            count = pts.len();
            pack_points(&mut pts, &mut rects);
        } else if matches!(target, SpecialTarget::CondFormats | SpecialTarget::DataValidation) {
            let mut raw_rects: Vec<(u32, u32, u32, u32)> = Vec::new();
            let iter_rects: Vec<(u32, u32, u32, u32)> = if target == SpecialTarget::CondFormats {
                sh.cf
                    .blocks
                    .iter()
                    .flat_map(|b| b.ranges.iter().copied())
                    .collect()
            } else {
                sh.dv
                    .iter()
                    .flat_map(|d| d.ranges.iter().copied())
                    .collect()
            };
            for (a1, b1, a2, b2) in iter_rects {
                for &(sr1, sc1, sr2, sc2) in &search_rects {
                    let ir1 = a1.max(sr1);
                    let ic1 = b1.max(sc1);
                    let ir2 = a2.min(sr2);
                    let ic2 = b2.min(sc2);
                    if ir1 <= ir2 && ic1 <= ic2 {
                        raw_rects.push((ir1, ic1, ir2, ic2));
                        count = count.saturating_add(
                            (ir2 - ir1 + 1) as usize * (ic2 - ic1 + 1) as usize,
                        );
                    }
                }
            }
            rects = raw_rects;
        } else {
            let _ = (&in_cf, &in_dv);
            for &(r1, c1, r2, c2) in &search_rects {
                for r in r1..=r2 {
                    let mut run_start: Option<u32> = None;
                    for c in c1..=c2 {
                        let cell = sh.get(r, c);
                        let hit = match target {
                            SpecialTarget::Blanks => cell.is_none_or(|x| {
                                x.formula.is_none() && matches!(x.value, Value::Empty)
                            }),
                            SpecialTarget::RowDifferences => {
                                let base = sh.value(r, active_c);
                                sh.value(r, c) != base
                            }
                            SpecialTarget::ColDifferences => {
                                let base = sh.value(active_r, c);
                                sh.value(r, c) != base
                            }
                            _ => false,
                        };
                        if hit {
                            count += 1;
                            if run_start.is_none() {
                                run_start = Some(c);
                            }
                        } else if let Some(cs) = run_start.take() {
                            rects.push((r, cs, r, c - 1));
                        }
                    }
                    if let Some(cs) = run_start {
                        rects.push((r, cs, r, c2));
                    }
                }
            }
        }

        if rects.is_empty() {
            self.status = format!("未找到任何「{}」单元格", target.label());
            return;
        }
        // 垂直合并列范围相同的连续行带，显著减少多选矩形块数，并彻底解除旧 129 块硬截断
        let mut coalesced: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(rects.len());
        for rc in rects {
            if let Some(prev) = coalesced.last_mut() {
                if prev.1 == rc.1 && prev.3 == rc.3 && prev.2.saturating_add(1) == rc.0 {
                    prev.2 = rc.2;
                    continue;
                }
            }
            coalesced.push(rc);
        }
        let first = coalesced[0];
        self.sel = (first.0, first.1);
        self.sel_end = (first.2, first.3);
        self.extra_sel = coalesced.into_iter().skip(1).take(65_536).collect();
        self.ensure_visible();
        self.status = format!("已选中 {count} 个「{}」单元格", target.label());
    }

    pub(crate) fn draw_goto_dlg(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.goto_dlg.take() else { return };
        let mut open = true;
        let mut close = false;
        let mut exec_ref = false;
        let mut exec_special: Option<SpecialTarget> = None;
        let title = if st.tab == 1 { "定位条件" } else { "定位" };

        ui_kit::window(ctx, title, 380.0).open(&mut open).show(ctx, |ui| {
            ui.set_width(350.0);
            ui_kit::dialog_tabs(ui, &mut st.tab, &["定位引用", "定位条件"]);
            ui.add_space(6.0);
            if st.tab == 0 {
                ui.horizontal(|ui| {
                    ui.label("引用位置:");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut st.target_ref)
                            .desired_width(240.0)
                            .hint_text("如 B12、A1:D20 或已定义名称"),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        exec_ref = true;
                    }
                });
                if !self.wb.names.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("工作簿名称:").size(11.5).color(ui_kit::TEXT_WEAK));
                    egui::ScrollArea::vertical().max_height(110.0).show(ui, |ui| {
                        for (name, _, refers) in &self.wb.names {
                            let label = format!("{name}  ({refers})");
                            if ui.selectable_label(st.target_ref == *name, label).clicked() {
                                st.target_ref = name.clone();
                            }
                        }
                    });
                }
            } else {
                ui.label(RichText::new("选择符合以下条件的单元格:").size(12.0));
                ui.add_space(4.0);
                ui.columns(2, |cols| {
                    for t in [
                        SpecialTarget::Formulas,
                        SpecialTarget::Constants,
                        SpecialTarget::Blanks,
                        SpecialTarget::Errors,
                        SpecialTarget::VisibleCells,
                        SpecialTarget::CurrentRegion,
                        SpecialTarget::CurrentArray,
                        SpecialTarget::LastCell,
                    ] {
                        cols[0].radio_value(&mut st.special, t, t.label());
                    }
                    for t in [
                        SpecialTarget::RowDifferences,
                        SpecialTarget::ColDifferences,
                        SpecialTarget::Precedents,
                        SpecialTarget::Dependents,
                        SpecialTarget::Comments,
                        SpecialTarget::CondFormats,
                        SpecialTarget::DataValidation,
                        SpecialTarget::Objects,
                    ] {
                        cols[1].radio_value(&mut st.special, t, t.label());
                    }
                });
                if matches!(st.special, SpecialTarget::Formulas | SpecialTarget::Constants) {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("子类型:").size(11.5).color(ui_kit::TEXT_WEAK));
                        ui.checkbox(&mut st.sub_numbers, "数字");
                        ui.checkbox(&mut st.sub_text, "文本");
                        ui.checkbox(&mut st.sub_bools, "逻辑值");
                        ui.checkbox(&mut st.sub_errors, "错误值");
                    });
                }
            }
            ui_kit::footer(ui, |ui| {
                if ui_kit::primary_button(ui, "确定").clicked() {
                    if st.tab == 0 {
                        exec_ref = true;
                    } else {
                        exec_special = Some(st.special);
                    }
                }
                if ui_kit::secondary_button(ui, "取消").clicked() {
                    close = true;
                }
            });
        });

        if exec_ref {
            self.name_buf = st.target_ref.trim().to_string();
            self.goto_name();
            close = true;
        } else if let Some(sp) = exec_special {
            self.select_special_with_subtypes(
                sp,
                (st.sub_numbers, st.sub_text, st.sub_bools, st.sub_errors),
            );
            close = true;
        }
        if open && !close {
            self.goto_dlg = Some(st);
        }
    }

    // ========================================================================
    //  条件格式快捷预设 + 聚焦规则对话框
    // ========================================================================

    fn sel_sqref_string(&self) -> String {
        let (r1, c1, r2, c2) = self.sel_rect();
        format!(
            "{}{}:{}{}",
            sheetcore::model::col_label(c1),
            r1 + 1,
            sheetcore::model::col_label(c2),
            r2 + 1
        )
    }

    pub(crate) fn open_cf_rule_dlg(&mut self, op: CfRuleOp) {
        let (r, c) = self.sel;
        let cur_txt = self.wb.sheet(self.sheet_idx).value(r, c).display();
        let v1 = if cur_txt.is_empty() {
            match op {
                CfRuleOp::TextContains
                | CfRuleOp::TextNotContains
                | CfRuleOp::TextBeginsWith
                | CfRuleOp::TextEndsWith => String::new(),
                CfRuleOp::Formula => format!("={}{}>0", sheetcore::model::col_label(c), r + 1),
                _ => "0".into(),
            }
        } else if op == CfRuleOp::Formula {
            format!("={}{}>0", sheetcore::model::col_label(c), r + 1)
        } else {
            cur_txt
        };
        self.cf_rule_dlg = Some(CfRuleDlgState {
            edit_target: None,
            sqref: self.sel_sqref_string(),
            op,
            v1,
            v2: "100".into(),
            top_n: 10,
            top_percent: false,
            period: sheetcore::cf::TimePeriod::Today,
            style_idx: 0,
            bold: false,
            italic: false,
            stop: false,
            bar_color: [99, 142, 198],
            bar_neg_color: [220, 53, 69],
            bar_show_value: true,
            scale_preset: 0,
            icon_set: "3TrafficLights1".into(),
            icon_reverse: false,
            icon_show_value: true,
        });
    }

    pub(crate) fn open_cf_new_rule_dlg(&mut self) {
        self.open_cf_rule_dlg(CfRuleOp::Greater);
    }

    pub(crate) fn open_cf_edit_rule_dlg(&mut self, bi: usize, ri: usize) {
        let sh = self.wb.sheet(self.sheet_idx);
        let Some(blk) = sh.cf.blocks.get(bi) else { return };
        let Some(rule) = blk.rules.get(ri) else { return };
        let sqref = blk
            .ranges
            .iter()
            .map(|(r1, c1, r2, c2)| {
                format!(
                    "{}{}:{}{}",
                    sheetcore::model::col_label(*c1),
                    r1 + 1,
                    sheetcore::model::col_label(*c2),
                    r2 + 1
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mut st = CfRuleDlgState {
            edit_target: Some((bi, ri)),
            sqref,
            op: CfRuleOp::Greater,
            v1: String::new(),
            v2: String::new(),
            top_n: 10,
            top_percent: false,
            period: sheetcore::cf::TimePeriod::Today,
            style_idx: 0,
            bold: false,
            italic: false,
            stop: rule.stop,
            bar_color: [99, 142, 198],
            bar_neg_color: [220, 53, 69],
            bar_show_value: true,
            scale_preset: 0,
            icon_set: "3TrafficLights1".into(),
            icon_reverse: false,
            icon_show_value: true,
        };
        if let Some(dxf_id) = rule.kind.dxf_id()
            && let Some(dxf) = sh.cf.dxfs.get(dxf_id)
        {
            st.bold = dxf.bold;
            st.italic = dxf.italic;
            st.style_idx = match (dxf.color, dxf.fill) {
                (Some([156, 101, 0]), Some([255, 235, 156])) => 1,
                (Some([0, 97, 0]), Some([198, 239, 206])) => 2,
                (None, Some([255, 199, 206])) => 3,
                (Some([192, 0, 0]), None) => 4,
                (Some([31, 78, 121]), Some([217, 225, 242])) => 5,
                _ => 0,
            };
        }
        match &rule.kind {
            CfKind::CellIs { op, a, b, .. } => {
                st.op = match op {
                    CfOp::Greater => CfRuleOp::Greater,
                    CfOp::Less => CfRuleOp::Less,
                    CfOp::GreaterEq => CfRuleOp::GreaterEq,
                    CfOp::LessEq => CfRuleOp::LessEq,
                    CfOp::Equal => CfRuleOp::Equal,
                    CfOp::NotEqual => CfRuleOp::NotEqual,
                    CfOp::Between => CfRuleOp::Between,
                    CfOp::NotBetween => CfRuleOp::NotBetween,
                };
                st.v1 = a.clone();
                st.v2 = b.clone();
            }
            CfKind::Text { contains, .. } => {
                st.op = CfRuleOp::TextContains;
                st.v1 = contains.clone();
            }
            CfKind::TextMatch { op, text, .. } => {
                st.op = match op {
                    sheetcore::cf::TextOp::Contains => CfRuleOp::TextContains,
                    sheetcore::cf::TextOp::NotContains => CfRuleOp::TextNotContains,
                    sheetcore::cf::TextOp::BeginsWith => CfRuleOp::TextBeginsWith,
                    sheetcore::cf::TextOp::EndsWith => CfRuleOp::TextEndsWith,
                };
                st.v1 = text.clone();
            }
            CfKind::TimePeriod { period, .. } => {
                st.op = CfRuleOp::TimePeriod;
                st.period = *period;
            }
            CfKind::Top { top, n, percent, .. } => {
                st.op = if *top { CfRuleOp::TopN } else { CfRuleOp::BottomN };
                st.top_n = (*n).max(1);
                st.top_percent = *percent;
            }
            CfKind::Avg { above, .. } => {
                st.op = if *above { CfRuleOp::AboveAvg } else { CfRuleOp::BelowAvg };
            }
            CfKind::Dup { unique, .. } => {
                st.op = if *unique { CfRuleOp::Unique } else { CfRuleOp::Duplicate };
            }
            CfKind::Blanks { blank, .. } => {
                st.op = if *blank { CfRuleOp::Blanks } else { CfRuleOp::NoBlanks };
            }
            CfKind::Errors { error, .. } => {
                st.op = if *error { CfRuleOp::Errors } else { CfRuleOp::NoErrors };
            }
            CfKind::Expr { formula, .. } => {
                st.op = CfRuleOp::Formula;
                st.v1 = formula.clone();
            }
            CfKind::Bar { color, neg_color, show_value, .. } => {
                st.op = CfRuleOp::DataBar;
                st.bar_color = *color;
                st.bar_neg_color = neg_color.unwrap_or([220, 53, 69]);
                st.bar_show_value = *show_value;
            }
            CfKind::Scale { .. } => {
                st.op = CfRuleOp::ColorScale;
            }
            CfKind::IconSet { set, reverse, show_value, .. } => {
                st.op = CfRuleOp::IconSet;
                st.icon_set = set.clone();
                st.icon_reverse = *reverse;
                st.icon_show_value = *show_value;
            }
        }
        self.cf_rule_dlg = Some(st);
    }

    pub(crate) fn draw_cf_rule_dlg(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.cf_rule_dlg.take() else { return };
        let mut open = true;
        let mut apply = false;
        let mut close = false;
        let title = if st.edit_target.is_some() {
            format!("编辑条件格式规则：{}", st.op.title())
        } else {
            format!("新建条件格式规则：{}", st.op.title())
        };

        ui_kit::window(ctx, &title, 440.0).open(&mut open).show(ctx, |ui| {
            ui.set_width(410.0);
            ui.horizontal(|ui| {
                ui.label("规则类型:");
                egui::ComboBox::from_id_salt("cf_rule_op_combo")
                    .selected_text(st.op.title())
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for op in CfRuleOp::ALL {
                            ui.selectable_value(&mut st.op, op, op.title());
                        }
                    });
            });
            ui.horizontal(|ui| {
                ui.label("应用区域:");
                ui.add(egui::TextEdit::singleline(&mut st.sqref).desired_width(240.0));
            });
            ui.separator();

            match st.op {
                CfRuleOp::Greater
                | CfRuleOp::Less
                | CfRuleOp::GreaterEq
                | CfRuleOp::LessEq
                | CfRuleOp::Equal
                | CfRuleOp::NotEqual => {
                    ui.horizontal(|ui| {
                        ui.label("条件值/引用:");
                        ui.add(egui::TextEdit::singleline(&mut st.v1).desired_width(180.0));
                    });
                    ui.label(RichText::new("支持数字、带引号文本或单元格引用（如 =$B$1）").weak().small());
                }
                CfRuleOp::Between | CfRuleOp::NotBetween => {
                    ui.horizontal(|ui| {
                        ui.label("下限:");
                        ui.add(egui::TextEdit::singleline(&mut st.v1).desired_width(110.0));
                        ui.label("上限:");
                        ui.add(egui::TextEdit::singleline(&mut st.v2).desired_width(110.0));
                    });
                }
                CfRuleOp::TextContains
                | CfRuleOp::TextNotContains
                | CfRuleOp::TextBeginsWith
                | CfRuleOp::TextEndsWith => {
                    ui.horizontal(|ui| {
                        ui.label("匹配文本:");
                        ui.add(egui::TextEdit::singleline(&mut st.v1).desired_width(220.0));
                    });
                }
                CfRuleOp::TimePeriod => {
                    ui.horizontal(|ui| {
                        ui.label("发生日期:");
                        egui::ComboBox::from_id_salt("cf_time_period_combo")
                            .selected_text(st.period.label())
                            .width(160.0)
                            .show_ui(ui, |ui| {
                                for p in sheetcore::cf::TimePeriod::ALL {
                                    ui.selectable_value(&mut st.period, p, p.label());
                                }
                            });
                    });
                }
                CfRuleOp::TopN | CfRuleOp::BottomN => {
                    ui.horizontal(|ui| {
                        ui.label("排名数量 N:");
                        ui.add(egui::DragValue::new(&mut st.top_n).range(1..=1000));
                        ui.checkbox(&mut st.top_percent, "按百分比 (%)");
                    });
                }
                CfRuleOp::Formula => {
                    ui.horizontal(|ui| {
                        ui.label("公式:");
                        ui.add(egui::TextEdit::singleline(&mut st.v1).desired_width(280.0));
                    });
                    ui.label(
                        RichText::new("相对区域左上角书写，返回 TRUE 时命中（例如 =$B2>1000 或 =MOD(ROW(),2)=0）")
                            .weak()
                            .small(),
                    );
                }
                CfRuleOp::DataBar => {
                    ui.horizontal(|ui| {
                        ui.label("正值色:");
                        for (lbl, rgb) in [
                            ("蓝", [99u8, 142, 198]),
                            ("绿", [99, 190, 123]),
                            ("橙", [255, 166, 76]),
                            ("紫", [147, 112, 219]),
                        ] {
                            if ui.selectable_label(st.bar_color == rgb, lbl).clicked() {
                                st.bar_color = rgb;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("负值色:");
                        for (lbl, rgb) in [
                            ("深红", [220u8, 53, 69]),
                            ("珊瑚红", [248, 105, 107]),
                            ("琥珀", [217, 119, 6]),
                        ] {
                            if ui.selectable_label(st.bar_neg_color == rgb, lbl).clicked() {
                                st.bar_neg_color = rgb;
                            }
                        }
                    });
                    let mut hide_val = !st.bar_show_value;
                    if ui.checkbox(&mut hide_val, "仅显示数据条（隐藏单元格数值）").changed() {
                        st.bar_show_value = !hide_val;
                    }
                }
                CfRuleOp::ColorScale => {
                    for (idx, lbl) in [
                        (0u8, "绿-黄-红（高值绿）"),
                        (1, "红-黄-绿（低值绿）"),
                        (2, "蓝-白-红"),
                        (3, "红-白-蓝"),
                        (4, "绿-白 2 色阶"),
                        (5, "白-绿 2 色阶"),
                    ] {
                        ui.radio_value(&mut st.scale_preset, idx, lbl);
                    }
                }
                CfRuleOp::IconSet => {
                    ui.horizontal(|ui| {
                        ui.label("图标样式:");
                        egui::ComboBox::from_id_salt("cf_icon_set_combo")
                            .selected_text(&st.icon_set)
                            .width(180.0)
                            .show_ui(ui, |ui| {
                                for s in [
                                    "3TrafficLights1",
                                    "3Arrows",
                                    "5Arrows",
                                    "3Flags",
                                    "3Symbols",
                                    "4Rating",
                                    "5Rating",
                                    "5Quarters",
                                ] {
                                    ui.selectable_value(&mut st.icon_set, s.to_string(), s);
                                }
                            });
                    });
                    ui.checkbox(&mut st.icon_reverse, "反转图标次序");
                    let mut hide_val = !st.icon_show_value;
                    if ui.checkbox(&mut hide_val, "仅显示图标（隐藏单元格数值）").changed() {
                        st.icon_show_value = !hide_val;
                    }
                }
                _ => {}
            }

            if st.op.uses_dxf() {
                ui.add_space(6.0);
                ui.label(RichText::new("突出显示格式:").font(ui_kit::bold(ui_kit::T_BODY)));
                for (idx, lbl) in [
                    (0u8, "浅红填充色深红色文本"),
                    (1, "黄填充色深黄色文本"),
                    (2, "绿填充色深绿色文本"),
                    (3, "仅浅红填充"),
                    (4, "仅红色文本"),
                    (5, "浅蓝填充色深蓝色文本"),
                ] {
                    ui.radio_value(&mut st.style_idx, idx, lbl);
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.bold, "加粗");
                    ui.checkbox(&mut st.italic, "斜体");
                });
            }
            ui.checkbox(&mut st.stop, "如果为真则停止 (stopIfTrue)");

            ui_kit::footer(ui, |ui| {
                if ui_kit::primary_button(ui, "确定").clicked() {
                    apply = true;
                }
                if ui_kit::secondary_button(ui, "取消").clicked() {
                    close = true;
                }
            });
        });

        if apply {
            let parsed_ranges = {
                let r = sheetcore::cf::parse_sqref(&st.sqref);
                if r.is_empty() { vec![self.sel_rect()] } else { r }
            };
            let dxf = if st.op.uses_dxf() {
                cf_dxf_by_preset_ext(self, st.style_idx, st.bold, st.italic)
            } else {
                0
            };
            let kind = match st.op {
                CfRuleOp::Greater => CfKind::CellIs { op: CfOp::Greater, a: st.v1.clone(), b: String::new(), dxf },
                CfRuleOp::Less => CfKind::CellIs { op: CfOp::Less, a: st.v1.clone(), b: String::new(), dxf },
                CfRuleOp::GreaterEq => CfKind::CellIs { op: CfOp::GreaterEq, a: st.v1.clone(), b: String::new(), dxf },
                CfRuleOp::LessEq => CfKind::CellIs { op: CfOp::LessEq, a: st.v1.clone(), b: String::new(), dxf },
                CfRuleOp::Between => CfKind::CellIs { op: CfOp::Between, a: st.v1.clone(), b: st.v2.clone(), dxf },
                CfRuleOp::NotBetween => CfKind::CellIs { op: CfOp::NotBetween, a: st.v1.clone(), b: st.v2.clone(), dxf },
                CfRuleOp::Equal => CfKind::CellIs { op: CfOp::Equal, a: st.v1.clone(), b: String::new(), dxf },
                CfRuleOp::NotEqual => CfKind::CellIs { op: CfOp::NotEqual, a: st.v1.clone(), b: String::new(), dxf },
                CfRuleOp::TextContains => CfKind::Text { contains: st.v1.clone(), dxf },
                CfRuleOp::TextNotContains => CfKind::TextMatch { op: sheetcore::cf::TextOp::NotContains, text: st.v1.clone(), dxf },
                CfRuleOp::TextBeginsWith => CfKind::TextMatch { op: sheetcore::cf::TextOp::BeginsWith, text: st.v1.clone(), dxf },
                CfRuleOp::TextEndsWith => CfKind::TextMatch { op: sheetcore::cf::TextOp::EndsWith, text: st.v1.clone(), dxf },
                CfRuleOp::TimePeriod => CfKind::TimePeriod { period: st.period, dxf },
                CfRuleOp::TopN => CfKind::Top { top: true, n: st.top_n.max(1), percent: st.top_percent, dxf },
                CfRuleOp::BottomN => CfKind::Top { top: false, n: st.top_n.max(1), percent: st.top_percent, dxf },
                CfRuleOp::AboveAvg => CfKind::Avg { above: true, dxf },
                CfRuleOp::BelowAvg => CfKind::Avg { above: false, dxf },
                CfRuleOp::Duplicate => CfKind::Dup { unique: false, dxf },
                CfRuleOp::Unique => CfKind::Dup { unique: true, dxf },
                CfRuleOp::Blanks => CfKind::Blanks { blank: true, dxf },
                CfRuleOp::NoBlanks => CfKind::Blanks { blank: false, dxf },
                CfRuleOp::Errors => CfKind::Errors { error: true, dxf },
                CfRuleOp::NoErrors => CfKind::Errors { error: false, dxf },
                CfRuleOp::Formula => CfKind::Expr { formula: st.v1.clone(), dxf },
                CfRuleOp::DataBar => CfKind::Bar {
                    color: st.bar_color,
                    neg_color: Some(st.bar_neg_color),
                    min: Cfvo::min(),
                    max: Cfvo::max(),
                    show_value: st.bar_show_value,
                },
                CfRuleOp::ColorScale => {
                    let points = match st.scale_preset {
                        1 => vec![(Cfvo::min(), [248, 105, 107]), (Cfvo::new(CfvoKind::Percentile, 50.0), [255, 235, 132]), (Cfvo::max(), [99, 190, 123])],
                        2 => vec![(Cfvo::min(), [93, 135, 202]), (Cfvo::new(CfvoKind::Percentile, 50.0), [255, 255, 255]), (Cfvo::max(), [248, 105, 107])],
                        3 => vec![(Cfvo::min(), [248, 105, 107]), (Cfvo::new(CfvoKind::Percentile, 50.0), [255, 255, 255]), (Cfvo::max(), [93, 135, 202])],
                        4 => vec![(Cfvo::min(), [99, 190, 123]), (Cfvo::max(), [255, 255, 255])],
                        5 => vec![(Cfvo::min(), [255, 255, 255]), (Cfvo::max(), [99, 190, 123])],
                        _ => vec![(Cfvo::min(), [99, 190, 123]), (Cfvo::new(CfvoKind::Percentile, 50.0), [255, 235, 132]), (Cfvo::max(), [248, 105, 107])],
                    };
                    CfKind::Scale { points }
                }
                CfRuleOp::IconSet => {
                    let levels = st.icon_set.chars().next().and_then(|c| c.to_digit(10)).unwrap_or(3) as usize;
                    let mut th = vec![Cfvo::min()];
                    for i in 1..levels {
                        th.push(Cfvo::new(CfvoKind::Percent, (i as f64) * 100.0 / levels as f64));
                    }
                    CfKind::IconSet {
                        set: st.icon_set.clone(),
                        reverse: st.icon_reverse,
                        thresholds: th,
                        show_value: st.icon_show_value,
                    }
                }
            };
            if let Some((bi, ri)) = st.edit_target {
                let stop_v = st.stop;
                if self.mutate_cf("编辑条件格式规则", |cf| {
                    if let Some(blk) = cf.blocks.get_mut(bi) {
                        blk.ranges = parsed_ranges;
                        if let Some(rule) = blk.rules.get_mut(ri) {
                            rule.kind = kind;
                            rule.stop = stop_v;
                            return true;
                        }
                    }
                    false
                }) {
                    self.status = "已更新条件格式规则".into();
                }
            } else {
                self.add_cf_with_opts(parsed_ranges, kind, st.stop);
            }
            close = true;
        }
        if open && !close {
            self.cf_rule_dlg = Some(st);
        }
    }

    pub(crate) fn apply_cf_preset(&mut self, action: &str) {
        match action {
            "CfDuplicate" => self.highlight_duplicates(),
            "CfUnique" => {
                let dxf = cf_dxf_by_preset(self, 2);
                self.add_cf(CfKind::Dup { unique: true, dxf });
            }
            "CfTop10" => {
                let dxf = cf_dxf_by_preset(self, 0);
                self.add_cf(CfKind::Top { top: true, n: 10, percent: false, dxf });
            }
            "CfTop10Pct" => {
                let dxf = cf_dxf_by_preset(self, 0);
                self.add_cf(CfKind::Top { top: true, n: 10, percent: true, dxf });
            }
            "CfBottom10" => {
                let dxf = cf_dxf_by_preset(self, 0);
                self.add_cf(CfKind::Top { top: false, n: 10, percent: false, dxf });
            }
            "CfBottom10Pct" => {
                let dxf = cf_dxf_by_preset(self, 0);
                self.add_cf(CfKind::Top { top: false, n: 10, percent: true, dxf });
            }
            "CfAboveAvg" => {
                let dxf = cf_dxf_by_preset(self, 2);
                self.add_cf(CfKind::Avg { above: true, dxf });
            }
            "CfBelowAvg" => {
                let dxf = cf_dxf_by_preset(self, 0);
                self.add_cf(CfKind::Avg { above: false, dxf });
            }
            "CfBarBlue" => self.add_cf_bar([99, 142, 198]),
            "CfBarGreen" => self.add_cf_bar([99, 190, 123]),
            "CfBarRed" => self.add_cf_bar([248, 105, 107]),
            "CfBarOrange" => self.add_cf_bar([255, 166, 76]),
            "CfBarPurple" => self.add_cf_bar([147, 112, 219]),
            "CfBarTeal" => self.add_cf_bar([45, 178, 160]),
            "CfScaleGYR" => self.add_cf_scale3([99, 190, 123], [255, 235, 132], [248, 105, 107]),
            "CfScaleRYG" => self.add_cf_scale3([248, 105, 107], [255, 235, 132], [99, 190, 123]),
            "CfScaleBWR" => self.add_cf_scale3([93, 135, 202], [255, 255, 255], [248, 105, 107]),
            "CfScaleRWB" => self.add_cf_scale3([248, 105, 107], [255, 255, 255], [93, 135, 202]),
            "CfScaleGW" => self.add_cf_scale2([99, 190, 123], [255, 255, 255]),
            "CfScaleWG" => self.add_cf_scale2([255, 255, 255], [99, 190, 123]),
            "CfIconTraffic" => self.add_cf_iconset("3TrafficLights1", 3),
            "CfIcon3Arrows" => self.add_cf_iconset("3Arrows", 3),
            "CfIcon5Arrows" => self.add_cf_iconset("5Arrows", 5),
            "CfIcon3Flags" => self.add_cf_iconset("3Flags", 3),
            "CfIcon3Symbols" => self.add_cf_iconset("3Symbols", 3),
            "CfIcon4Rating" => self.add_cf_iconset("4Rating", 4),
            "CfIcon5Quarters" => self.add_cf_iconset("5Quarters", 5),
            "CfClearSel" => self.clear_cf(),
            "CfClearSheet" => {
                if self.mutate_cf("清除整表条件格式", |cf| {
                    if cf.blocks.is_empty() {
                        false
                    } else {
                        cf.blocks.clear();
                        true
                    }
                }) {
                    self.status = "已清除当前工作表的全部条件格式".into();
                } else if self.wb.sheet(self.sheet_idx).cf.blocks.is_empty() {
                    self.status = "当前工作表没有条件格式".into();
                }
            }
            _ => {}
        }
    }

    fn add_cf_bar(&mut self, color: [u8; 3]) {
        self.add_cf(CfKind::bar_default(color, Cfvo::min(), Cfvo::max()));
    }

    fn add_cf_scale3(&mut self, c_min: [u8; 3], c_mid: [u8; 3], c_max: [u8; 3]) {
        self.add_cf(CfKind::Scale {
            points: vec![
                (Cfvo::min(), c_min),
                (Cfvo::new(CfvoKind::Percentile, 50.0), c_mid),
                (Cfvo::max(), c_max),
            ],
        });
    }

    fn add_cf_scale2(&mut self, c_min: [u8; 3], c_max: [u8; 3]) {
        self.add_cf(CfKind::Scale {
            points: vec![(Cfvo::min(), c_min), (Cfvo::max(), c_max)],
        });
    }

    fn add_cf_iconset(&mut self, set: &str, levels: usize) {
        let mut th = vec![Cfvo::min()];
        for i in 1..levels {
            th.push(Cfvo::new(CfvoKind::Percent, (i as f64) * 100.0 / levels as f64));
        }
        self.add_cf(CfKind::IconSet {
            set: set.into(),
            reverse: false,
            thresholds: th,
            show_value: true,
        });
    }

    // ========================================================================
    //  套用表格格式（多配色主题） + 18 种 WPS 单元格样式
    // ========================================================================

    pub(crate) fn apply_table_theme(&mut self, theme: &str) {
        if self.table_at_sel().is_none() {
            let (r1, c1, r2, c2) = self.sel_rect();
            if r1 == r2 && c1 == c2 {
                let reg = sheetcore::sort_filter::current_region(
                    self.wb.sheet(self.sheet_idx),
                    r1,
                    c1,
                );
                if reg.2 > reg.0 && reg.3 > reg.1 {
                    self.sel = (reg.0, reg.1);
                    self.sel_end = (reg.2, reg.3);
                }
            }
            self.make_table();
        }
        let Some(t) = self.table_at_sel() else {
            return;
        };
        let (r1, c1, r2, c2) = t.range;
        let style_name = match theme {
            "TableThemeGreen" => "TableStyleMedium7",
            "TableThemeOrange" => "TableStyleMedium3",
            "TableThemeGray" => "TableStyleMedium1",
            "TableThemeNavy" => "TableStyleMedium9",
            "TableThemeForest" => "TableStyleDark4",
            _ => "TableStyleMedium2",
        };
        let (hdr_fill, hdr_fg, stripe_even, stripe_odd, border_col) =
            sheetcore::table::table_style_palette(style_name);
        {
            let si = self.sheet_idx;
            let before_tables = self.wb.sheet(si).tables.clone();
            if let Some(tb) = self.wb.sheet_mut(si).tables.iter_mut().find(|x| x.name == t.name) {
                tb.style_name = style_name.to_string();
            }
            self.wb.sheet_mut(si).tables_dirty = true;
            self.wb.sheet_mut(si).bump_display_style_revision();
            let after_tables = self.wb.sheet(si).tables.clone();
            if before_tables != after_tables {
                self.undo.push_meta(
                    "套用表格样式",
                    (si, self.sel.0, self.sel.1),
                    sheetcore::undo::MetaEdit {
                        sheet: si,
                        tables: Some((before_tables, after_tables)),
                        ..Default::default()
                    },
                );
            }
        }
        let prev_sel = (self.sel, self.sel_end);
        self.sel = (r1, c1);
        self.sel_end = (r2, c2);
        self.apply_style_xy(move |s, r, _c| {
            let b = sheetcore::style::Border {
                style: 1,
                color: border_col,
            };
            s.border[BorderSide::Top.idx()] = b;
            s.border[BorderSide::Bottom.idx()] = b;
            if r == r1 {
                s.bold = true;
                s.fill = Some(hdr_fill);
                s.color = hdr_fg;
            } else {
                s.bold = false;
                s.color = [0, 0, 0];
                s.fill = if (r - r1) % 2 == 1 {
                    stripe_odd
                } else {
                    stripe_even
                };
            }
        });
        self.sel = prev_sel.0;
        self.sel_end = prev_sel.1;
        self.status = format!("已套用表格样式（{style_name}）");
    }

    pub(crate) fn apply_wps_cell_style(&mut self, id: &str) {
        match id {
            "StyleNormal" => self.apply_style_preset("normal"),
            "StyleTitle" => self.apply_style_preset("title"),
            "StyleHeading" => self.apply_style_preset("heading"),
            "StyleGood" => self.apply_style_preset("good"),
            "StyleBad" => self.apply_style_preset("bad"),
            "StyleNeutral" => self.apply_style(|s| {
                s.bold = false;
                s.color = [156, 101, 0];
                s.fill = Some([255, 235, 156]);
            }),
            "StyleInput" => self.apply_style(|s| {
                s.bold = false;
                s.color = [63, 63, 118];
                s.fill = Some([255, 204, 153]);
                s.set_all_borders(1, [127, 127, 127]);
            }),
            "StyleOutput" => self.apply_style(|s| {
                s.bold = true;
                s.color = [63, 63, 63];
                s.fill = Some([242, 242, 242]);
                s.set_all_borders(1, [63, 63, 63]);
            }),
            "StyleCalc" => self.apply_style(|s| {
                s.bold = true;
                s.color = [250, 125, 0];
                s.fill = Some([242, 242, 242]);
                s.set_all_borders(1, [127, 127, 127]);
            }),
            "StyleWarning" => self.apply_style(|s| {
                s.color = [255, 0, 0];
            }),
            "StyleNote" => self.apply_style(|s| {
                s.italic = true;
                s.color = [127, 127, 127];
            }),
            "StyleH1" => self.apply_style(|s| {
                s.bold = true;
                s.size = 15.0;
                s.color = [31, 78, 121];
                s.border[BorderSide::Bottom.idx()] = sheetcore::style::Border {
                    style: 2,
                    color: [68, 114, 196],
                };
            }),
            "StyleH2" => self.apply_style(|s| {
                s.bold = true;
                s.size = 13.0;
                s.color = [31, 78, 121];
                s.border[BorderSide::Bottom.idx()] = sheetcore::style::Border {
                    style: 2,
                    color: [142, 169, 219],
                };
            }),
            "StyleH3" => self.apply_style(|s| {
                s.bold = true;
                s.size = 11.0;
                s.color = [31, 78, 121];
                s.border[BorderSide::Bottom.idx()] = sheetcore::style::Border {
                    style: 1,
                    color: [142, 169, 219],
                };
            }),
            "StyleTotal" => self.apply_style(|s| {
                s.bold = true;
                s.color = [0, 0, 0];
                s.border[BorderSide::Top.idx()] = sheetcore::style::Border {
                    style: 1,
                    color: [68, 114, 196],
                };
                s.border[BorderSide::Bottom.idx()] = sheetcore::style::Border {
                    style: 4,
                    color: [68, 114, 196],
                };
            }),
            "StyleAccentBlue20" => self.apply_style(|s| {
                s.fill = Some([217, 225, 242]);
                s.color = [0, 0, 0];
            }),
            "StyleAccentGreen20" => self.apply_style(|s| {
                s.fill = Some([226, 239, 218]);
                s.color = [0, 0, 0];
            }),
            "StyleAccentOrange20" => self.apply_style(|s| {
                s.fill = Some([252, 228, 214]);
                s.color = [0, 0, 0];
            }),
            "StyleComma0" => self.apply_style(|s| s.format = "#,##0".into()),
            "StyleCurrency0" => self.apply_style(|s| s.format = "¥#,##0".into()),
            _ => {}
        }
    }

    // ========================================================================
    //  清除 / 格式 / 图表 / 符号 / 数据验证补充动作
    // ========================================================================

    pub(crate) fn clear_comments_in_sel(&mut self) {
        if self.edit_blocked() {
            return;
        }
        let targets = self.comment_keys_in_sel();
        if targets.is_empty() {
            self.status = "所选区域没有批注".into();
            return;
        }
        let gate: Vec<(u32, u32)> = targets.iter().map(|k| (k.row, k.col)).collect();
        if !self.write_preflight(&gate, "清除批注") {
            return;
        }
        let sh = self.wb.sheet_mut(self.sheet_idx);
        let mut removed = 0usize;
        for k in targets {
            if sh.notes.remove(&k).is_some() {
                removed += 1;
            }
        }
        if removed > 0 {
            sh.notes_dirty = true;
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.tasks.last_edit = Some(std::time::Instant::now());
            self.status = format!("已清除 {removed} 条批注");
        } else {
            self.status = "所选区域没有批注".into();
        }
    }

    pub(crate) fn clear_hyperlinks_in_sel(&mut self) {
        let rects = self.sel_rects();
        let sh = self.wb.sheet_mut(self.sheet_idx);
        let before = sh.hyperlinks.len();
        sh.hyperlinks.retain(|&(r, c), _| {
            !rects
                .iter()
                .any(|&(r1, c1, r2, c2)| r >= r1 && r <= r2 && c >= c1 && c <= c2)
        });
        let removed = before - sh.hyperlinks.len();
        if removed > 0 {
            sh.hyperlinks_dirty = true;
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.status = format!("已清除 {removed} 个超链接");
        } else {
            self.status = "所选区域没有超链接".into();
        }
    }

    pub(crate) fn insert_chart_of_kind(&mut self, kind: ChartKind) {
        self.open_chart_create_wizard(Some(kind));
    }

    pub(crate) fn insert_quick_symbol(&mut self, ch: char) {
        self.insert_symbol(ch);
        self.commit_edit(None);
        self.status = format!("已插入符号 {ch}");
    }

    pub(crate) fn clear_dv_in_sel(&mut self) {
        if self.replace_validation_rule(self.sel_rect(), None) {
            self.status = "已清除所选范围的数据验证".into();
        } else {
            self.status = "所选范围没有数据验证".into();
        }
    }
}

fn advance_months_serial(base_serial: f64, delta_months: i64) -> f64 {
    // Excel 1900 epoch: serial 0 = 1899-12-30 (day_num offset from Unix 1970-01-01 = -25569)
    let serial_i = base_serial.round() as i64;
    let unix_days = serial_i - 25569;
    // Civil date from unix_days (Howard Hinnant algorithm)
    let z = unix_days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    if m <= 2 {
        y += 1;
    }

    let total_m = (y * 12 + (m - 1)) + delta_months;
    let ny = total_m.div_euclid(12);
    let nm = total_m.rem_euclid(12) + 1;
    let is_leap = (ny % 4 == 0 && ny % 100 != 0) || ny % 400 == 0;
    let max_d = match nm {
        2 => if is_leap { 29 } else { 28 },
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    let nd = d.min(max_d);

    let cy = ny - if nm <= 2 { 1 } else { 0 };
    let cera = cy.div_euclid(400);
    let cyoe = cy.rem_euclid(400);
    let cmp = nm + if nm > 2 { -3 } else { 9 };
    let cdoy = (153 * cmp + 2) / 5 + nd - 1;
    let cdoe = cyoe * 365 + cyoe / 4 - cyoe / 100 + cdoy;
    let new_unix_days = cera * 146097 + cdoe - 719468;
    (new_unix_days + 25569) as f64
}
