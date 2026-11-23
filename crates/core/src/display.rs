//! 单元格显示模型：屏幕、打印预览、PDF 共用同一份「这一格最终长什么样」。
//!
//! 只负责内容与属性（文本、颜色、字号、对齐、换行、合并范围、条件格式叠加），
//! 具体画到哪由调用方决定；这样预览和 PDF 不会各算一套。

use crate::cf::Overlay;
use crate::model::{DateSystem, Sheet, Value, Workbook};
use crate::style::{CellStyle, StyleTable};

/// 一格的最终显示属性（合并区只用左上角锚点格的视图）
#[derive(Clone, Debug, PartialEq)]
pub struct CellView {
    pub text: String,
    pub color: [u8; 3],
    pub bold: bool,
    pub italic: bool,
    pub size: f32,
    /// 0 左 1 中 2 右（General 已按值类型解析）
    pub halign: u8,
    /// 0 上 1 中 2 下
    pub valign: u8,
    pub wrap: bool,
    /// 逆时针角度（0 / 45 / 90 / -45 / -90）
    pub rotation: i16,
    pub indent: u8,
    /// 合并范围（含锚点自身）；None = 单格
    pub merge: Option<(u32, u32, u32, u32)>,
    pub fill: Option<[u8; 3]>,
    /// 数据条：比例 0..1 与颜色
    pub bar: Option<(f32, [u8; 3])>,
}

impl CellView {
    /// 文本是否按数字/日期右对齐（General）
    pub fn general_right(&self) -> bool {
        self.halign == 2
    }
}

/// 合并区锚点：返回 (r1,c1,r2,c2)；不在合并区返回 None（直接复用 `Sheet::merges` 内置惰性区间索引）
#[inline]
pub fn merge_span(sheet: &Sheet, r: u32, c: u32) -> Option<(u32, u32, u32, u32)> {
    sheet.merge_at(r, c)
}

/// 该格是否是合并区里被盖住的那部分（打印时跳过，只画锚点）
pub fn merge_covered(sheet: &Sheet, r: u32, c: u32) -> bool {
    matches!(merge_span(sheet, r, c), Some((r1, c1, _, _)) if (r1, c1) != (r, c))
}

/// General 对齐按值类型：文本左、数字/日期右、布尔/错误居中（与屏幕 `cell_text_align` 一致）
pub fn general_halign(v: &Value) -> u8 {
    match v {
        Value::Number(_) => 2,
        Value::Bool(_) => 1,
        Value::Error(_) => 1,
        _ => 0,
    }
}

/// 解析一格的显示属性。`overlay` 是调用方算好的条件格式结果。
pub fn view_at(
    wb: &Workbook,
    sheet: &Sheet,
    r: u32,
    c: u32,
    overlay: Option<&Overlay>,
) -> Option<CellView> {
    view_at_value(wb, sheet, r, c, overlay, None)
}

/// 同上，但允许调用方提供「已算好的值」（公式格：缓存的显示值与公式结果不同时以传入为准）
pub fn view_at_value(
    wb: &Workbook,
    sheet: &Sheet,
    r: u32,
    c: u32,
    overlay: Option<&Overlay>,
    value_override: Option<&Value>,
) -> Option<CellView> {
    view_at_value_fit(wb, sheet, r, c, overlay, value_override, None)
}

/// 打印路径用：General 显示按单元格可用字符数（合并宽度）决定，
/// 否则默认 8.43 列宽的长整数会在 PDF 里显示完整数字、屏幕上却是科学计数。
pub fn view_at_value_fit(
    wb: &Workbook,
    sheet: &Sheet,
    r: u32,
    c: u32,
    overlay: Option<&Overlay>,
    value_override: Option<&Value>,
    cap: Option<usize>,
) -> Option<CellView> {
    view_with_merge(wb, sheet, r, c, overlay, value_override, cap, merge_span(sheet, r, c))
}

pub(crate) fn view_with_merge(
    wb: &Workbook,
    sheet: &Sheet,
    r: u32,
    c: u32,
    overlay: Option<&Overlay>,
    value_override: Option<&Value>,
    cap: Option<usize>,
    merge: Option<(u32, u32, u32, u32)>,
) -> Option<CellView> {
    let cell = sheet.get(r, c);
    let st: &CellStyle = wb.styles.get(sheet.style_of(r, c));
    let has_style = st.fill.is_some()
        || st.border.iter().any(|b| b.style != 0)
        || st.diagonal.is_some()
        || !st.format.is_empty()
        || st.bold
        || st.italic;
    if cell.is_none() && !has_style && overlay.is_none() {
        return None;
    }
    let value = value_override
        .cloned()
        .or_else(|| cell.map(|x| x.value.clone()))
        .unwrap_or(Value::Empty);
    let text = if matches!(value, Value::Empty) {
        String::new()
    } else {
        match cap {
            Some(c) => crate::style::format_value_in_fit(wb.date_system, &value, &st.format, c),
            None => format_value(wb.date_system, &value, st),
        }
    };
    // 富文本用纯文本兜底：逐段字体是屏幕层的富文本路径，打印暂不拆段
    let text = match sheet.rich.get(&(r, c)) {
        Some(runs) if !runs.is_empty() => runs.iter().map(|x| x.text.as_str()).collect::<String>(),
        _ => text,
    };
    let halign = match st.halign {
        1 => 0,
        2 | 4 | 5 => 1,
        3 => 2,
        _ => general_halign(&value),
    };
    let valign = match st.valign {
        1 => 0,
        2 => 1,
        3 => 2,
        _ => 2, // Excel 默认靠下
    };
    let mut color = crate::style::format_color(&value, &st.format).unwrap_or(st.color);
    let mut bold = st.bold;
    let mut italic = st.italic;
    let mut fill = st.fill;
    let mut bar = None;
    if let Some(ov) = overlay {
        if let Some(c2) = ov.color {
            color = c2;
        }
        bold |= ov.bold;
        if ov.fill.is_some() {
            fill = ov.fill;
        }
        bar = ov.bar;
    }
    Some(CellView {
        text,
        color,
        bold,
        italic,
        size: if st.size > 0.0 { st.size } else { 11.0 },
        halign,
        valign,
        wrap: st.wrap,
        rotation: st.rotation,
        indent: st.indent,
        merge,
        fill,
        bar,
    })
}

fn format_value(sys: DateSystem, v: &Value, st: &CellStyle) -> String {
    if st.format.trim().is_empty() || st.format.eq_ignore_ascii_case("general") {
        v.display()
    } else {
        crate::style::format_value_in(sys, v, &st.format)
    }
}

/// 按格宽折行（按字符估算宽度：CJK 约 1 个字号，ASCII 约 0.55）。
/// 返回至少一行；`\n` 强制换行。
pub fn wrap_lines(text: &str, width_pt: f32, size: f32) -> Vec<String> {
    wrap_lines_with(text, width_pt, size, None)
}

/// Excel 的折行模型。
///
/// 实测（`work/probe/wrapmeas.*`，6 组列宽/字号/缩进组合 + Excel 导出 PDF 反解）：
/// 可容纳字数 = floor(列宽 × 0.88 ÷ (0.975 × 字号))，缩进按 3 个 `0` 的宽度扣一次。
/// 列宽是 Excel `Columns.Width` 的 pt 值，和我们 `layout` 里的 px×0.75 一致。
pub fn wrap_lines_excel(
    text: &str,
    width_pt: f32,
    size: f32,
    metric: Option<&(dyn Fn(char, f32) -> f32 + Send + Sync)>,
) -> Vec<String> {
    wrap_scaled(text, width_pt * 0.88, size, metric, 0.975)
}

/// 同上，但可传入真字体度量（导出 PDF 时用；预览/屏幕传 None 走估算）
pub fn wrap_lines_with(
    text: &str,
    width_pt: f32,
    size: f32,
    metric: Option<&(dyn Fn(char, f32) -> f32 + Send + Sync)>,
) -> Vec<String> {
    wrap_scaled(text, width_pt, size, metric, 1.0)
}

fn wrap_scaled(
    text: &str,
    width_pt: f32,
    size: f32,
    metric: Option<&(dyn Fn(char, f32) -> f32 + Send + Sync)>,
    adv_scale: f32,
) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        let mut w = 0.0f32;
        for ch in para.chars() {
            let cw = match metric {
                Some(f) => f(ch, size) * adv_scale,
                None => char_width(ch, size) * adv_scale,
            };
            if w + cw > width_pt && !line.is_empty() {
                out.push(std::mem::take(&mut line));
                w = 0.0;
            }
            line.push(ch);
            w += cw;
        }
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// 屏幕/预览/PDF 共用的行内文本宽度估算
pub fn text_width(s: &str, size: f32) -> f32 {
    s.chars().map(|c| char_width(c, size)).sum()
}

/// Excel AutoFit 的列宽（字符单位），按本机 Excel 16.0 Build 20326 实测反解。
///
/// 目标字体是中文版 Excel 的 Normal 样式 **等线 11**（不是 Calibri）：
/// ASCII 每个字符记 1、CJK 记 2，固定内边距 0.88；超过 9 个字符后每字符
/// 再加 0.125 的像素量化。例：`1`→1.88、`1234.5678`→9.88、
/// `12345678901`→12.13、`1.23457E+14`→12.13。
/// （若 Normal 被改成 Calibri，Excel 会给 1.54/11.69；这不是本项目默认环境。）
/// 存盘时 `width` 就是这个值，不能拿 Carlito 的 `text_width` 再除 7。
pub fn excel_autofit_chars(s: &str) -> f32 {
    let mut ascii = 0.0f32;
    let mut cjk = 0usize;
    for c in s.chars() {
        if (c as u32) >= 0x2e80 {
            cjk += 1;
        } else {
            ascii += 1.0;
        }
    }
    let units = ascii + cjk as f32 * 2.0;
    let base = units + 0.88 + 0.125 * (units - 9.0).max(0.0);
    // 纯 CJK 串的像素量化和 ASCII 不同（中/中文/中文字 实测 2.75/4.63/6.5）。
    if ascii == 0.0 && cjk > 0 {
        let k = cjk as f32;
        let w = if cjk <= 4 {
            1.875 * k + 0.875
        } else {
            // 5 个以上汉字：Excel 按 2 字符/个继续加宽（实测 10.38/12.38）
            8.375 + 2.0 * (k - 4.0)
        };
        w.max(1.88)
    } else {
        base
    }
}

/// 带样式的 AutoFit：Excel 的缩进按 3 个字符/级计入，粗体再补 0.37
/// （等线 11 实测：`1`=1.88、粗体=2.25、缩进 1=4.88、缩进 2=7.88）。
/// 这是 AutoFit 的列宽，不是画布上的文字 advance；不要拿它去乘字号。
pub fn excel_autofit_chars_styled(s: &str, bold: bool, indent: u8) -> f32 {
    excel_autofit_chars(s)
        + if bold { 0.37 } else { 0.0 }
        + indent as f32 * 3.0
}

fn autofit_units(s: &str) -> f32 {
    let mut ascii = 0.0f32;
    let mut cjk = 0usize;
    for c in s.chars() {
        if (c as u32) >= 0x2e80 {
            cjk += 1;
        } else {
            ascii += 1.0;
        }
    }
    ascii + cjk as f32 * 2.0
}

fn is_song_font(font: &str) -> bool {
    let f = font.to_ascii_lowercase();
    font.contains("宋体") || f.contains("simsun")
}

/// 等线的粗体增量。Excel 对等线粗体做 0.125 字符的像素量化，短串/汉字在 11 号
/// 常常为 0；用 `ELINE_STYLE_DELTAS` 在 1/10/12 个 units 三个实测点之间插值。
/// 缩进仍按 xlsx 文件语义计 **3 字符/级**（COM 直接设 IndentLevel 的 1.5 不适用于打开文件）。
fn eline_style_delta(units: f32, size: f32, bold: bool, indent: u8) -> f32 {
    let u = units.clamp(1.0, 12.0);
    let value = |entry: &(f32, [f32; 3], [f32; 3], [f32; 3]), kind: usize| {
        let vals: [f32; 3] = match kind {
            0 => entry.1,
            1 => entry.2,
            _ => entry.3,
        };
        interpolate_autofit_curve(
            &[(1.0, vals[0]), (10.0, vals[1]), (12.0, vals[2])],
            u,
        )
    };
    let at_size = |sz: f32, kind: usize| -> f32 {
        let first = &ELINE_STYLE_DELTAS[0];
        if sz <= first.0 {
            return value(first, kind);
        }
        let last = &ELINE_STYLE_DELTAS[ELINE_STYLE_DELTAS.len() - 1];
        if sz >= last.0 {
            return value(last, kind);
        }
        for pair in ELINE_STYLE_DELTAS.windows(2) {
            let lo = &pair[0];
            let hi = &pair[1];
            if sz <= hi.0 {
                let t = (sz - lo.0) / (hi.0 - lo.0);
                return value(lo, kind) + (value(hi, kind) - value(lo, kind)) * t;
            }
        }
        0.0
    };
    let mut delta = if bold { at_size(size, 0) } else { 0.0 };
    if indent > 0 {
        let d1 = at_size(size, 1);
        let d2 = at_size(size, 2);
        delta += if indent == 1 {
            d1
        } else {
            d1 + (indent as f32 - 1.0) * (d2 - d1)
        };
    }
    delta
}

fn eline_styled_at_size(s: &str, bold: bool, indent: u8, size: f32) -> f32 {
    let base = if (size - 11.0).abs() < 0.01 {
        excel_autofit_chars(s)
    } else {
        autofit_size_width(s, size).unwrap_or_else(|| excel_autofit_chars(s))
    };
    base + eline_style_delta(autofit_units(s), size, bold, indent)
}

/// 带字体名的 11 号 AutoFit：等线走实测缩进/粗体增量，宋体保留旧口径。
pub fn excel_autofit_chars_styled_font(
    s: &str,
    bold: bool,
    indent: u8,
    font: &str,
) -> f32 {
    if is_song_font(font) {
        return excel_autofit_chars_styled(s, bold, indent);
    }
    eline_styled_at_size(s, bold, indent, 11.0)
}

/// 带字体名的非 11 号 AutoFit：等线用实测字号曲线 + 样式增量；宋体走旧缩放。
pub fn excel_autofit_chars_at_size_font(
    s: &str,
    bold: bool,
    indent: u8,
    size: f32,
    font: &str,
) -> f32 {
    if is_song_font(font) {
        return excel_autofit_chars_at_size(s, bold, indent, size);
    }
    eline_styled_at_size(s, bold, indent, size)
}

/// 带字体名与换行的 AutoFit：按行取最大。
pub fn excel_autofit_chars_wrapped_at_size_font(
    s: &str,
    bold: bool,
    indent: u8,
    wrap: bool,
    size: f32,
    font: &str,
) -> f32 {
    if !wrap {
        return excel_autofit_chars_at_size_font(s, bold, indent, size, font);
    }
    s.split('\n')
        .map(|line| excel_autofit_chars_at_size_font(line, bold, indent, size, font))
        .fold(0.0f32, f32::max)
}

/// Excel AutoFit 的等线字号曲线：`(字号, ASCII 字符宽度[1..12], 纯 CJK 宽度[1..5])`。
/// ASCII 必须用 **文本单元格** 实测（数值 111111111111 被 Excel 显示成科学计数法，
/// AutoFit 会给出更窄的列）；数据源 `work/probe/excel_text_ascii_matrix.out`。
/// CJK 数据源 `work/probe/excel_autofit_size_matrix3.out`；11 号仍走精确模型。
const AUTOFIT_SIZE_CURVES: &[(f32, &[(f32, f32)], &[(f32, f32)])] = &[
    (8.0, &[(1.0,1.63),(2.0,2.38),(3.0,3.13),(4.0,3.88),(5.0,4.63),(6.0,5.38),(7.0,6.13),(8.0,6.88),(9.0,7.63),(10.0,8.38),(11.0,9.13),(12.0,9.88)], &[(1.0,2.25),(2.0,3.63),(3.0,5.00),(4.0,6.38),(5.0,7.75)]),
    (9.0, &[(1.0,1.63),(2.0,2.38),(3.0,3.13),(4.0,3.88),(5.0,4.63),(6.0,5.38),(7.0,6.13),(8.0,6.88),(9.0,7.63),(10.0,8.38),(11.0,9.13),(12.0,9.88)], &[(1.0,2.38),(2.0,3.88),(3.0,5.38),(4.0,6.88),(5.0,8.38)]),
    (10.0, &[(1.0,1.75),(2.0,2.63),(3.0,3.50),(4.0,4.38),(5.0,5.25),(6.0,6.13),(7.0,7.00),(8.0,7.88),(9.0,8.75),(10.0,9.63),(11.0,10.63),(12.0,11.63)], &[(1.0,2.50),(2.0,4.13),(3.0,5.75),(4.0,7.38),(5.0,9.00)]),
    (11.0, &[(1.0,1.88),(2.0,2.88),(3.0,3.88),(4.0,4.88),(5.0,5.88),(6.0,6.88),(7.0,7.88),(8.0,8.88),(9.0,9.88),(10.0,11.00),(11.0,12.13),(12.0,13.25)], &[(1.0,2.75),(2.0,4.63),(3.0,6.50),(4.0,8.38),(5.0,10.38)]),
    (12.0, &[(1.0,1.88),(2.0,2.88),(3.0,3.88),(4.0,4.88),(5.0,5.88),(6.0,6.88),(7.0,7.88),(8.0,8.88),(9.0,9.88),(10.0,11.00),(11.0,12.13),(12.0,13.25)], &[(1.0,2.88),(2.0,4.88),(3.0,6.88),(4.0,8.88),(5.0,11.00)]),
    (13.0, &[(1.0,2.25),(2.0,3.38),(3.0,4.50),(4.0,5.63),(5.0,6.75),(6.0,7.88),(7.0,9.00),(8.0,10.13),(9.0,11.38),(10.0,12.63),(11.0,13.88),(12.0,15.13)], &[(1.0,3.25),(2.0,5.38),(3.0,7.50),(4.0,9.63),(5.0,12.00)]),
    (14.0, &[(1.0,2.38),(2.0,3.63),(3.0,4.88),(4.0,6.13),(5.0,7.38),(6.0,8.63),(7.0,9.88),(8.0,11.25),(9.0,12.63),(10.0,14.00),(11.0,15.50),(12.0,16.88)], &[(1.0,3.50),(2.0,5.88),(3.0,8.25),(4.0,10.75),(5.0,13.38)]),
    (15.0, &[(1.0,2.50),(2.0,3.88),(3.0,5.25),(4.0,6.63),(5.0,8.00),(6.0,9.38),(7.0,10.88),(8.0,12.38),(9.0,13.88),(10.0,15.50),(11.0,17.00),(12.0,18.50)], &[(1.0,3.63),(2.0,6.13),(3.0,8.63),(4.0,11.25),(5.0,14.00)]),
    (16.0, &[(1.0,2.50),(2.0,3.88),(3.0,5.25),(4.0,6.63),(5.0,8.00),(6.0,9.38),(7.0,10.88),(8.0,12.38),(9.0,13.88),(10.0,15.50),(11.0,17.00),(12.0,18.50)], &[(1.0,3.75),(2.0,6.38),(3.0,9.00),(4.0,11.88),(5.0,14.75)]),
    (17.0, &[(1.0,2.63),(2.0,4.13),(3.0,5.63),(4.0,7.13),(5.0,8.63),(6.0,10.13),(7.0,11.88),(8.0,13.50),(9.0,15.13),(10.0,16.88),(11.0,18.50),(12.0,20.13)], &[(1.0,4.00),(2.0,6.88),(3.0,9.75),(4.0,13.00),(5.0,16.13)]),
    (18.0, &[(1.0,3.00),(2.0,4.63),(3.0,6.25),(4.0,7.88),(5.0,9.50),(6.0,11.25),(7.0,13.00),(8.0,14.88),(9.0,16.63),(10.0,18.50),(11.0,20.25),(12.0,22.13)], &[(1.0,4.38),(2.0,7.38),(3.0,10.38),(4.0,13.75),(5.0,17.13)]),
    (19.0, &[(1.0,3.00),(2.0,4.63),(3.0,6.25),(4.0,7.88),(5.0,9.50),(6.0,11.25),(7.0,13.00),(8.0,14.88),(9.0,16.63),(10.0,18.50),(11.0,20.25),(12.0,22.13)], &[(1.0,4.50),(2.0,7.63),(3.0,10.88),(4.0,14.25),(5.0,17.75)]),
    (20.0, &[(1.0,3.13),(2.0,4.88),(3.0,6.63),(4.0,8.38),(5.0,10.13),(6.0,12.13),(7.0,14.00),(8.0,16.00),(9.0,17.88),(10.0,19.88),(11.0,21.75),(12.0,23.75)], &[(1.0,4.75),(2.0,8.13),(3.0,11.63),(4.0,15.38),(5.0,19.13)]),
    (21.0, &[(1.0,3.25),(2.0,5.13),(3.0,7.00),(4.0,8.88),(5.0,10.88),(6.0,12.88),(7.0,15.00),(8.0,17.13),(9.0,19.13),(10.0,21.25),(11.0,23.38),(12.0,25.38)], &[(1.0,4.88),(2.0,8.38),(3.0,12.13),(4.0,16.00),(5.0,19.88)]),
    (22.0, &[(1.0,3.25),(2.0,5.13),(3.0,7.00),(4.0,8.88),(5.0,10.88),(6.0,12.88),(7.0,15.00),(8.0,17.13),(9.0,19.13),(10.0,21.25),(11.0,23.38),(12.0,25.38)], &[(1.0,5.00),(2.0,8.63),(3.0,12.50),(4.0,16.50),(5.0,20.50)]),
    (23.0, &[(1.0,3.38),(2.0,5.38),(3.0,7.38),(4.0,9.38),(5.0,11.50),(6.0,13.75),(7.0,16.00),(8.0,18.25),(9.0,20.38),(10.0,22.63),(11.0,24.88),(12.0,27.13)], &[(1.0,5.25),(2.0,9.13),(3.0,13.38),(4.0,17.63),(5.0,21.88)]),
    (24.0, &[(1.0,3.75),(2.0,5.88),(3.0,8.00),(4.0,10.13),(5.0,12.50),(6.0,14.88),(7.0,17.25),(8.0,19.50),(9.0,21.88),(10.0,24.25),(11.0,26.63),(12.0,29.00)], &[(1.0,5.63),(2.0,9.63),(3.0,14.00),(4.0,18.50),(5.0,22.88)]),
];

/// 等线的粗体/缩进增量（xlsx 文件语义）：按 `units`（ASCII=1、CJK=2）在 1/10/12
/// 三个实测点插值；文件里的 `indent=N` 在 AutoFit 是 3 字符/级（COM 直接设
/// `IndentLevel=N` 的 1.5 不适用于打开的文件）。数据源
/// `work/probe/excel_style_file_matrix.out`。
const ELINE_STYLE_DELTAS: &[(f32, [f32; 3], [f32; 3], [f32; 3])] = &[
    (8.0, [0.000,0.000,0.000], [3.000,3.130,3.370], [6.000,6.500,6.750]),
    (9.0, [0.120,0.000,1.750], [3.000,3.250,3.370], [6.000,6.500,6.750]),
    (10.0, [0.000,0.000,0.000], [3.000,3.250,3.250], [6.000,6.630,6.620]),
    (11.0, [0.000,0.000,0.000], [3.000,3.250,3.380], [6.000,6.620,6.630]),
    (12.0, [0.370,0.250,1.880], [3.000,3.380,3.380], [6.000,6.750,6.630]),
    (13.0, [0.000,0.000,0.000], [3.000,3.250,3.370], [6.000,6.630,6.750]),
    (14.0, [0.120,0.000,1.620], [3.000,3.370,3.250], [6.000,6.620,6.620]),
    (15.0, [0.000,0.000,0.000], [3.000,3.380,3.380], [6.000,6.750,6.630]),
    (16.0, [0.130,0.000,1.630], [3.000,3.380,3.380], [6.000,6.630,6.630]),
    (17.0, [0.370,0.250,2.000], [3.000,3.370,3.370], [6.000,6.620,6.750]),
    (18.0, [0.000,0.000,0.000], [3.000,3.250,3.250], [6.000,6.620,6.620]),
    (19.0, [0.130,0.000,1.620], [3.000,3.380,3.250], [6.000,6.630,6.620]),
    (20.0, [0.120,0.000,1.630], [3.000,3.370,3.380], [6.000,6.750,6.630]),
    (21.0, [0.130,0.000,1.750], [3.000,3.250,3.370], [6.000,6.620,6.620]),
    (22.0, [0.130,0.000,1.750], [3.000,3.380,3.370], [6.000,6.750,6.620]),
    (23.0, [0.370,0.250,1.870], [3.000,3.370,3.250], [6.000,6.750,6.620]),
    (24.0, [0.130,0.000,1.630], [3.000,3.370,3.250], [6.000,6.620,6.630]),
];

fn interpolate_autofit_curve(points: &[(f32, f32)], x: f32) -> f32 {
    if points.len() == 1 {
        return points[0].1;
    }
    if x <= points[0].0 {
        return points[0].1;
    }
    for pair in points.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        if x <= x1 {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    let (x0, y0) = points[points.len() - 2];
    let (x1, y1) = points[points.len() - 1];
    y0 + (y1 - y0) * (x - x0) / (x1 - x0)
}

fn autofit_size_width(s: &str, size: f32) -> Option<f32> {
    let mut ascii = 0.0f32;
    let mut cjk = 0usize;
    for c in s.chars() {
        if (c as u32) >= 0x2e80 {
            cjk += 1;
        } else {
            ascii += 1.0;
        }
    }
    let units = ascii + cjk as f32 * 2.0;
    let curve_width = |curve: &(f32, &[(f32, f32)], &[(f32, f32)])| {
        if ascii == 0.0 && cjk > 0 {
            interpolate_autofit_curve(curve.2, cjk as f32)
        } else {
            interpolate_autofit_curve(curve.1, units)
        }
    };
    if size <= AUTOFIT_SIZE_CURVES[0].0 {
        return Some(curve_width(&AUTOFIT_SIZE_CURVES[0]));
    }
    let last = AUTOFIT_SIZE_CURVES.len() - 1;
    if size >= AUTOFIT_SIZE_CURVES[last].0 {
        return Some(curve_width(&AUTOFIT_SIZE_CURVES[last]));
    }
    for pair in AUTOFIT_SIZE_CURVES.windows(2) {
        let lo = &pair[0];
        let hi = &pair[1];
        if size <= hi.0 {
            let t = (size - lo.0) / (hi.0 - lo.0);
            let w0 = curve_width(lo);
            let w1 = curve_width(hi);
            return Some(w0 + (w1 - w0) * t);
        }
    }
    None
}

/// 非 11 号字号的 AutoFit：按 Excel 实测字号曲线取宽度。
/// 11 号保留原精确模型；8–24 的整数磅位逐档实测，非整数磅位才在相邻档之间线性插值。
/// 字体加粗/缩进的偏移按字号比例缩放（11 号仍走原 +0.37/+3.0）。
pub fn excel_autofit_chars_at_size(s: &str, bold: bool, indent: u8, size: f32) -> f32 {
    if (size - 11.0).abs() < 0.01 {
        return excel_autofit_chars_styled(s, bold, indent);
    }
    if let Some(w) = autofit_size_width(s, size) {
        let scale = size / 11.0;
        return w + if bold { 0.37 * scale } else { 0.0 } + indent as f32 * 3.0 * scale;
    }
    excel_autofit_chars_styled(s, bold, indent)
}

/// 自动换行格的 AutoFit：Excel 取**最长一行**，不是整段所有字符相加。
/// `1\n22` 实测列宽 2.88（不是 4.75）；`中文\n中文文字` 取 4 个汉字 8.38。
pub fn excel_autofit_chars_wrapped(
    s: &str,
    bold: bool,
    indent: u8,
    wrap: bool,
) -> f32 {
    if !wrap {
        return excel_autofit_chars_styled(s, bold, indent);
    }
    s.split('\n')
        .map(|line| excel_autofit_chars_styled(line, bold, indent))
        .fold(0.0f32, f32::max)
}

/// 带字号与换行的 AutoFit：按行取最大，且字号不是 11 时使用实测曲线。
pub fn excel_autofit_chars_wrapped_at_size(
    s: &str,
    bold: bool,
    indent: u8,
    wrap: bool,
    size: f32,
) -> f32 {
    if !wrap {
        return excel_autofit_chars_at_size(s, bold, indent, size);
    }
    s.split('\n')
        .map(|line| excel_autofit_chars_at_size(line, bold, indent, size))
        .fold(0.0f32, f32::max)
}

/// 单个字符的排版宽度。
///
/// 拉丁/数字优先用内嵌 Carlito（与 Calibri 度量兼容）的真实 advance，
/// 这样自动换行的断行点、右对齐位置才和 Excel 一致；CJK 仍按 1em（Excel 用等线/宋体也是 1em），
/// Carlito 里没有的字符退回 0.55×字号估算。
pub fn char_width(c: char, size: f32) -> f32 {
    static FONT: std::sync::OnceLock<Option<crate::pdf::Font>> = std::sync::OnceLock::new();
    if (c as u32) < 0x2e80 {
        let f = FONT.get_or_init(|| {
            crate::pdf::Font::parse(
                include_bytes!("../../../assets/fonts/subset/Carlito-Regular.subset.ttf").to_vec(),
            )
            .ok()
        });
        if let Some(font) = f {
            if let Some(em) = font.advance(c) {
                return em * size;
            }
        }
    }
    if (c as u32) > 0x2e80 { size } else { size * 0.55 }
}

/// 样式表里取样式（给需要直接读样式的调用方）
pub fn style_of<'a>(styles: &'a StyleTable, sheet: &Sheet, r: u32, c: u32) -> &'a CellStyle {
    styles.get(sheet.style_of(r, c))
}

/// AutoFit 列宽的 O(1) 严格字符数上界（仅凭最大 UTF-8 字节长度与样式参数估算，零字符串分配与遍历）。
/// 满足对任意长度 `<= max_utf8_bytes` 的字符串 `s`，返回值恒 `>= excel_autofit_chars_wrapped_at_size_font`。
#[inline]
pub fn autofit_chars_upper_bound(max_utf8_bytes: usize, bold: bool, indent: u8, size: f32) -> f32 {
    let u = max_utf8_bytes as f32;
    let sz = if size > 0.0 { size } else { 11.0 };
    let scale = (sz / 11.0).max(1.0);
    let base = if sz <= 12.01 {
        2.2 + 1.15 * u
    } else {
        3.8 + 2.45 * u
    };
    let bold_ub = if bold {
        (0.38 * scale).max(2.05 + 0.88 * (u - 12.0).max(0.0))
    } else {
        0.0
    };
    let indent_ub = indent as f32
        * (3.05 * scale).max(3.55 + 0.15 * (u - 10.0).max(0.0));
    base + bold_ub + indent_ub
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autofit_upper_bound_never_underestimates_any_font_or_size() {
        let samples = [
            "1",
            "123456789012345",
            "中",
            "中文文字测试超长标题行",
            "Order-2026-华东大区汇总\n第二行更长的备注说明ABC12345",
        ];
        for s in samples {
            for size in [8.0, 9.0, 11.0, 12.0, 14.0, 18.0, 24.0, 36.0] {
                for bold in [false, true] {
                    for indent in [0u8, 1, 2, 4] {
                        let ub = autofit_chars_upper_bound(s.len(), bold, indent, size);
                        for font in ["等线", "宋体", "Arial"] {
                            for wrap in [false, true] {
                                let exact = excel_autofit_chars_wrapped_at_size_font(
                                    s, bold, indent, wrap, size, font,
                                );
                                assert!(
                                    ub + 1e-4 >= exact,
                                    "upper bound {ub} < exact {exact} for {s:?} sz={size} bold={bold} indent={indent} font={font}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn merge_span_uses_indexed_lookup_past_threshold() {
        let mut sh = Sheet::new("M");
        for i in 0..32u32 {
            sh.merges.push((i * 3, 1, i * 3 + 1, 4));
        }
        assert_eq!(merge_span(&sh, 31, 2), Some((30, 1, 31, 4)));
        assert_eq!(merge_span(&sh, 32, 2), None);
        assert!(merge_covered(&sh, 31, 2));
        assert!(!merge_covered(&sh, 30, 1));
    }
}
