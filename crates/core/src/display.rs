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

/// 合并区锚点：返回 (r1,c1,r2,c2)；不在合并区返回 None
pub fn merge_span(sheet: &Sheet, r: u32, c: u32) -> Option<(u32, u32, u32, u32)> {
    sheet
        .merges
        .iter()
        .find(|(r1, c1, r2, c2)| r >= *r1 && r <= *r2 && c >= *c1 && c <= *c2)
        .copied()
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
    let cell = sheet.get(r, c);
    let st: &CellStyle = wb.styles.get(sheet.style_of(r, c));
    let has_style = st.fill.is_some()
        || st.border.iter().any(|b| b.style != 0)
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
        format_value(wb.date_system, &value, st)
    };
    // 富文本用纯文本兜底：逐段字体是屏幕层的富文本路径，打印暂不拆段
    let text = match sheet.rich.get(&(r, c)) {
        Some(runs) if !runs.is_empty() => runs.iter().map(|x| x.text.as_str()).collect::<String>(),
        _ => text,
    };
    let halign = match st.halign {
        1 => 0,
        2 => 1,
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
        merge: merge_span(sheet, r, c),
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
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        let mut w = 0.0f32;
        for ch in para.chars() {
            let cw = if (ch as u32) > 0x2e80 { size } else { size * 0.55 };
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
    s.chars()
        .map(|c| if (c as u32) > 0x2e80 { size } else { size * 0.55 })
        .sum()
}

/// 样式表里取样式（给需要直接读样式的调用方）
pub fn style_of<'a>(styles: &'a StyleTable, sheet: &Sheet, r: u32, c: u32) -> &'a CellStyle {
    styles.get(sheet.style_of(r, c))
}
