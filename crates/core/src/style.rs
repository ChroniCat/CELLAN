//! 单元格样式：把 xlsx 的样式表压成一个小结构，供渲染与数字格式化使用。
//!
//! 设计取舍：不做完整的 OOXML 样式模型（字体家族、边框、图案填充……），
//! 只保留「看得见且常用」的部分：粗斜体、字号、字色、底色、对齐、自动换行、数字格式。
//! 每格只存一个 u16 索引，相同样式去重后共享一份。

use std::collections::HashMap;

/// 一条边：0 无，1 细，2 中，3 粗，4 双线，5 虚线，6 点线
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Border {
    pub style: u8,
    pub color: [u8; 3],
}

impl Border {
    pub fn is_none(&self) -> bool {
        self.style == 0
    }
    pub fn px(&self) -> f32 {
        match self.style {
            1 => 1.0,
            2 => 1.6,
            3 => 2.4,
            4 => 1.0,
            5 => 1.0,
            6 => 1.0,
            _ => 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    pub size: f32,
    /// 字体颜色 RGB
    pub color: [u8; 3],
    /// 底色，None 表示无填充
    pub fill: Option<[u8; 3]>,
    /// 0 常规 1 左 2 中 3 右
    pub halign: u8,
    /// 0 常规 1 上 2 中 3 下
    pub valign: u8,
    pub wrap: bool,
    /// 数字格式代码，空串表示常规
    pub format: String,
    /// 左、右、上、下
    pub border: [Border; 4],
}

impl CellStyle {
    pub fn default_dark() -> Self {
        Self { size: 11.0, color: [0, 0, 0], ..Default::default() }
    }
}

impl CellStyle {
    pub fn set_all_borders(&mut self, style: u8, color: [u8; 3]) {
        for b in self.border.iter_mut() {
            *b = Border { style, color };
        }
    }
    pub fn clear_borders(&mut self) {
        for b in self.border.iter_mut() {
            *b = Border::default();
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct StyleTable {
    styles: Vec<CellStyle>,
    index: HashMap<u64, u16>,
}

fn hash_of(s: &CellStyle) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.bold.hash(&mut h);
    s.italic.hash(&mut h);
    s.size.to_bits().hash(&mut h);
    s.color.hash(&mut h);
    s.fill.hash(&mut h);
    s.halign.hash(&mut h);
    s.valign.hash(&mut h);
    s.wrap.hash(&mut h);
    s.format.hash(&mut h);
    s.border.hash(&mut h);
    h.finish()
}

impl StyleTable {
    pub fn new() -> Self {
        let mut t = Self::default();
        t.intern(CellStyle::default_dark());
        t
    }

    pub fn intern(&mut self, s: CellStyle) -> u16 {
        let k = hash_of(&s);
        if let Some(id) = self.index.get(&k) {
            if self.styles[*id as usize] == s {
                return *id;
            }
        }
        let id = self.styles.len() as u16;
        self.styles.push(s);
        self.index.insert(k, id);
        id
    }

    pub fn get(&self, id: u16) -> &CellStyle {
        self.styles.get(id as usize).unwrap_or_else(|| &self.styles[0])
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }
}

/// 把数字按格式代码显示。只覆盖常见格式，其余回退到常规。
pub fn format_value(v: &crate::model::Value, fmt: &str) -> String {
    use crate::model::Value;
    let Value::Number(n) = v else {
        return v.display();
    };
    if fmt.is_empty() {
        return v.display();
    }
    // 百分比
    if fmt.contains('%') {
        let dec = decimals_in(fmt);
        return format!("{:.*}%", dec, n * 100.0);
    }
    // 日期/时间
    if is_date_format(fmt) {
        if let Some(s) = format_date(*n, fmt) {
            return s;
        }
    }
    // 千分位与小数位
    let dec = decimals_in(fmt);
    let thousands = fmt.contains("#,##") || fmt.contains(",0");
    let prefix = currency_prefix(fmt);
    let body = if thousands {
        group_thousands(*n, dec)
    } else {
        format!("{:.*}", dec, n)
    };
    match prefix {
        Some(p) => format!("{p}{body}"),
        None => body,
    }
}

fn decimals_in(fmt: &str) -> usize {
    let Some(dot) = fmt.find('.') else { return 0 };
    fmt[dot + 1..].chars().take_while(|c| *c == '0' || *c == '#').count()
}

fn currency_prefix(fmt: &str) -> Option<char> {
    for c in ['¥', '$', '€', '£'] {
        if fmt.contains(c) {
            return Some(c);
        }
    }
    None
}

fn group_thousands(n: f64, dec: usize) -> String {
    let s = format!("{:.*}", dec, n.abs());
    let (int_part, frac) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s.clone(), None),
    };
    let mut out = String::new();
    let chars: Vec<char> = int_part.chars().collect();
    for (i, ch) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*ch);
    }
    if n < 0.0 {
        out.insert(0, '-');
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(&f);
    }
    out
}

fn is_date_format(fmt: &str) -> bool {
    let lower = fmt.to_lowercase();
    (lower.contains('y') && lower.contains('m'))
        || lower.contains("mm-dd")
        || lower.contains("h:mm")
        || lower.contains("am/pm")
}

fn format_date(serial: f64, fmt: &str) -> Option<String> {
    let (y, m, d) = crate::formula::serial_to_date_pub(serial)?;
    let (hh, mm, ss) = {
        let frac = serial.fract().abs();
        let total = (frac * 86400.0).round() as u32;
        (total / 3600, (total % 3600) / 60, total % 60)
    };
    let lower = fmt.to_lowercase();
    if lower.contains("h:mm") {
        if lower.contains("am/pm") {
            let (h12, ap) = if hh % 12 == 0 { (12, "AM") } else if hh < 12 { (hh, "AM") } else { (hh - 12, "PM") };
            return Some(format!("{h12}:{mm:02} {ap}"));
        }
        return Some(if ss > 0 { format!("{hh:02}:{mm:02}:{ss:02}") } else { format!("{hh}:{mm:02}") });
    }
    if lower.contains("yyyy") && lower.contains("mm") && lower.contains("dd") {
        if lower.contains("yyyy-mm-dd") || lower.contains("yyyy/mm/dd") || lower.contains("yyyy年") {
            let sep = if fmt.contains('/') { '/' } else if fmt.contains('年') { '年' } else { '-' };
            return Some(if sep == '年' {
                format!("{y}年{m}月{d}日")
            } else {
                format!("{y}{sep}{m:02}{sep}{d:02}")
            });
        }
    }
    if lower.contains("yy") && fmt.matches('m').count() >= 2 && fmt.matches('d').count() >= 2 {
        return Some(format!("{y:04}-{m:02}-{d:02}"));
    }
    Some(format!("{y}-{m:02}-{d:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Value;

    #[test]
    fn number_formats() {
        let n = |v: f64| Value::Number(v);
        assert_eq!(format_value(&n(1234.5), "#,##0.00"), "1,234.50");
        assert_eq!(format_value(&n(1234.5), "0.00"), "1234.50");
        assert_eq!(format_value(&n(0.126), "0.0%"), "12.6%");
        assert_eq!(format_value(&n(-9876.5), "#,##0.00"), "-9,876.50");
        assert_eq!(format_value(&n(1234.5), "¥#,##0.00"), "¥1,234.50");
        assert_eq!(format_value(&n(1234.5), ""), "1234.5");
    }

    #[test]
    fn styles_dedup() {
        let mut t = StyleTable::new();
        let a = CellStyle { bold: true, size: 12.0, ..Default::default() };
        let id1 = t.intern(a.clone());
        let id2 = t.intern(a);
        assert_eq!(id1, id2);
        let b = CellStyle { bold: false, size: 12.0, ..Default::default() };
        assert_ne!(t.intern(b), id1);
    }
}
