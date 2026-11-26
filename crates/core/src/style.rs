//! 单元格样式：把 xlsx 的样式表压成一个小结构，供渲染与数字格式化使用。
//!
//! 设计取舍：不做完整的 OOXML 样式模型（字体家族、边框、图案填充……），
//! 只保留「看得见且常用」的部分：粗斜体、字号、字色、底色、对齐、自动换行、数字格式。
//! 每格只存一个 u16 索引，相同样式去重后共享一份。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 单条边的线型和 RGB 颜色。线型编号与 OOXML 名称的映射见 BORDER_NAMES。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
pub struct Border {
    pub style: u8,
    pub color: [u8; 3],
}

/// 边框数组的命名下标，避免各处裸写 0/1/2/3 把左右、上下写反。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum BorderSide {
    Left = 0,
    Right = 1,
    Top = 2,
    Bottom = 3,
}

impl BorderSide {
    pub fn idx(self) -> usize {
        self as usize
    }
}

const BORDER_NAMES: [&str; 14] = [
    "none", "thin", "medium", "thick", "double", "dashed", "dotted", "hair",
    "dashDot", "dashDotDot", "mediumDashed", "mediumDashDot", "mediumDashDotDot", "slantDashDot",
];

impl Border {
    pub fn ooxml_name(style: u8) -> &'static str {
        BORDER_NAMES.get(usize::from(style)).copied().unwrap_or("thin")
    }

    pub fn from_ooxml_name(name: &str) -> u8 {
        BORDER_NAMES.iter().position(|&s| s == name).map_or(1, |i| i as u8)
    }

    pub fn is_none(&self) -> bool {
        self.style == 0
    }
    pub fn px(&self) -> f32 {
        match self.style {
            1 => 1.0,
            2 => 1.6,
            3 => 2.4,
            4 => 1.0,
            5 | 6 | 8 | 9 => 1.0,
            7 => 0.65,
            10..=13 => 1.6,
            _ => 0.0,
        }
    }
}

/// 字体下拉的候选：内嵌四款 + 系统四款。
///
/// 系统那四款（宋体/黑体/微软雅黑/等线）没有内嵌字形，绘制时按 `font_class`
/// 映射到内嵌子集，保证「选了就有字形」；写盘时仍写用户选的原名。
pub const FONT_CHOICES: &[&str] = &[
    "",
    "宋体",
    "黑体",
    "微软雅黑",
    "等线",
    "思源黑体",
    "思源宋体",
    "Inter",
    "JetBrains Mono",
];

/// 字体名归一：Excel 里中英文名混用（宋体 / SimSun），统一折成列表里的写法。
///
/// 认不出来的名字原样保留——渲染时按未知字体回退默认，写盘时也不改用户的选择。
pub fn normalize_font(name: &str) -> String {
    let t = name.trim().trim_matches('"').trim();
    match t.to_lowercase().as_str() {
        "" => String::new(),
        "simsun" | "宋体" => "宋体".into(),
        "simhei" | "黑体" => "黑体".into(),
        "microsoft yahei" | "微软雅黑" => "微软雅黑".into(),
        "dengxian" | "等线" => "等线".into(),
        "noto sans sc" | "source han sans sc" | "思源黑体" => "思源黑体".into(),
        "noto serif sc" | "source han serif sc" | "思源宋体" => "思源宋体".into(),
        "inter" => "Inter".into(),
        "jetbrains mono" => "JetBrains Mono".into(),
        _ => t.to_string(),
    }
}

/// 渲染用的字体族归类。`key()` 就是 egui 里注册的 family 名。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontClass {
    /// 默认：跟随界面字体（内嵌 Inter + 思源黑体）
    Default,
    /// 黑体系：思源黑体 / 黑体 / 微软雅黑 / 等线
    Sans,
    /// 宋体系：思源宋体 / 宋体
    Serif,
    /// 英文正文：Inter
    Latin,
    /// 等宽：JetBrains Mono
    Mono,
}

impl FontClass {
    pub fn key(self) -> &'static str {
        match self {
            FontClass::Default => "default",
            FontClass::Sans => "cjk-sans",
            FontClass::Serif => "cjk-serif",
            FontClass::Latin => "latin",
            FontClass::Mono => "mono",
        }
    }
}

/// 名字 → 字体族：四款系统字体映射到最接近的内嵌子集，未知字体回退默认。
pub fn font_class(name: &str) -> FontClass {
    match normalize_font(name).as_str() {
        "" => FontClass::Default,
        "宋体" | "思源宋体" => FontClass::Serif,
        "黑体" | "微软雅黑" | "等线" | "思源黑体" => FontClass::Sans,
        "Inter" => FontClass::Latin,
        "JetBrains Mono" => FontClass::Mono,
        _ => FontClass::Default,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    /// 单下划线（Excel 的 U）
    #[serde(default)]
    pub underline: bool,
    /// 双下划线（underline 打开时生效）
    #[serde(default)]
    pub double_underline: bool,
    /// 竖排文字（OOXML textRotation=255，字符正立纵向排列）
    #[serde(default)]
    pub vertical_text: bool,
    /// 删除线
    #[serde(default)]
    pub strikethrough: bool,
    /// 上下标：0 常规，1 上标 (superscript)，2 下标 (subscript)
    #[serde(default)]
    pub vert_align: u8,
    /// 文字旋转角度（度，逆时针为正：0 / 45 / 90 / -45 / -90）
    #[serde(default)]
    pub rotation: i16,
    /// 字体名（见 `normalize_font`；空串 = 默认字体）
    #[serde(default)]
    pub font: String,
    pub size: f32,
    /// 字体颜色 RGB
    pub color: [u8; 3],
    /// 底色，None 表示无填充
    pub fill: Option<[u8; 3]>,
    /// 0 常规 1 左 2 中 3 右 4 跨列居中 5 分散对齐
    pub halign: u8,
    /// 0 常规 1 上 2 中 3 下
    pub valign: u8,
    pub wrap: bool,
    /// 缩小字体填充（Excel/WPS ShrinkToFit）
    #[serde(default)]
    pub shrink_to_fit: bool,
    /// 缩进级数（Excel 的 IndentLevel，1 级约 3 个字符宽）
    #[serde(default)]
    pub indent: u8,
    /// 数字格式代码，空串表示常规
    pub format: String,
    /// 左、右、上、下
    pub border: [Border; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagonal: Option<crate::style_ext::DiagBorder>,
    /// 单元格保护：Excel 默认锁定
    #[serde(default = "default_locked")]
    pub locked: bool,
    /// 单元格保护：隐藏公式/内容
    #[serde(default)]
    pub hidden: bool,
}

fn default_locked() -> bool {
    true
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            underline: false,
            double_underline: false,
            vertical_text: false,
            strikethrough: false,
            vert_align: 0,
            rotation: 0,
            font: String::new(),
            size: 0.0,
            color: [0, 0, 0],
            fill: None,
            halign: 0,
            valign: 0,
            wrap: false,
            shrink_to_fit: false,
            indent: 0,
            format: String::new(),
            border: [Border::default(); 4],
            diagonal: None,
            locked: true,
            hidden: false,
        }
    }
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
        self.diagonal = None;
        for b in self.border.iter_mut() {
            *b = Border::default();
        }
    }
}

#[derive(Clone, Debug, Default)]
struct StyleTableInner {
    styles: Vec<CellStyle>,
    index: HashMap<u64, u16>,
    /// 与 styles 同长：每个样式号对应的主题色引用（读盘时记，保存时按它写回）
    themes: Vec<crate::style_theme::XfTheme>,
    /// 与 styles 同长：图案填充（未进 CellStyle，保存时按原 patternType 写回）
    patterns: Vec<Option<crate::style_ext::FillPattern>>,
    /// 与 styles 同长：该样式是否带 quotePrefix（未建模属性，保存后单独补）
    quotes: Vec<bool>,
}

/// Copy-on-write style table: cloning a `Workbook` or snapshot only bumps an `Arc`.
#[derive(Clone, Debug, Default)]
pub struct StyleTable(std::sync::Arc<StyleTableInner>);

impl StyleTable {
    pub fn ptr_eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    /// 工作簿里实际出现的字体名（去重前的原始顺序），供 UI 按需加载系统字体。
    pub fn font_names(&self) -> impl Iterator<Item = &str> {
        self.0
            .styles
            .iter()
            .map(|s| s.font.as_str())
            .filter(|f| !f.trim().is_empty())
    }
}

fn hash_of(s: &CellStyle) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.bold.hash(&mut h);
    s.italic.hash(&mut h);
    s.underline.hash(&mut h);
        s.double_underline.hash(&mut h);
        s.vertical_text.hash(&mut h);
    s.strikethrough.hash(&mut h);
    s.vert_align.hash(&mut h);
    s.rotation.hash(&mut h);
    s.font.hash(&mut h);
    s.size.to_bits().hash(&mut h);
    s.color.hash(&mut h);
    s.fill.hash(&mut h);
    s.halign.hash(&mut h);
    s.valign.hash(&mut h);
    s.wrap.hash(&mut h);
    s.shrink_to_fit.hash(&mut h);
    s.indent.hash(&mut h);
    s.format.hash(&mut h);
    s.border.hash(&mut h);
    if let Some(d) = &s.diagonal {
        (d.up, d.down, &d.style, d.color, d.color_theme.map(|(id, tint)| (id, tint.to_bits()))).hash(&mut h);
    }
    s.locked.hash(&mut h);
    s.hidden.hash(&mut h);
    h.finish()
}

impl StyleTable {
    pub fn new() -> Self {
        let mut t = Self::default();
        t.intern(CellStyle::default_dark());
        t
    }

    pub(crate) fn find_style(&self, style: &CellStyle) -> Option<u16> {
        self.0.index.get(&hash_of(style)).copied().filter(|&id| self.get(id) == style)
    }

    pub fn intern(&mut self, s: CellStyle) -> u16 {
        let k = hash_of(&s);
        if let Some(&id) = self.0.index.get(&k) {
            if self.0.styles[id as usize] == s
                && self.0.themes.get(id as usize).copied().unwrap_or_default()
                    == crate::style_theme::XfTheme::default()
                && self.0.patterns.get(id as usize).and_then(|p| p.as_ref()).is_none()
                && !self.0.quotes.get(id as usize).copied().unwrap_or(false)
            {
                return id;
            }
        }
        let inner = std::sync::Arc::make_mut(&mut self.0);
        // 保留 u16::MAX (65535) 作为流式加载器 AtomicU16 未初始化哨兵，避免 id 回绕污染索引
        if inner.styles.len() >= u16::MAX as usize {
            return inner.styles.len().saturating_sub(1).min((u16::MAX - 1) as usize) as u16;
        }
        let id = inner.styles.len() as u16;
        inner.styles.push(s);
        inner.themes.push(crate::style_theme::XfTheme::default());
        inner.patterns.push(None);
        inner.quotes.push(false);
        inner.index.entry(k).or_insert(id);
        id
    }

    /// 完整保留扩展元数据（主题色、图案填充、quotePrefix）的样式驻留。
    pub fn intern_with_ext(
        &mut self,
        s: CellStyle,
        theme: crate::style_theme::XfTheme,
        pattern: Option<crate::style_ext::FillPattern>,
        quote: bool,
    ) -> u16 {
        let is_plain = theme == crate::style_theme::XfTheme::default()
            && pattern.is_none()
            && !quote;
        if is_plain {
            return self.intern(s);
        }
        for (i, st) in self.0.styles.iter().enumerate() {
            if *st == s
                && self.0.themes.get(i).copied().unwrap_or_default() == theme
                && self.0.patterns.get(i).and_then(|p| p.as_ref()) == pattern.as_ref()
                && self.0.quotes.get(i).copied().unwrap_or(false) == quote
            {
                return i as u16;
            }
        }
        let inner = std::sync::Arc::make_mut(&mut self.0);
        if inner.styles.len() >= u16::MAX as usize {
            return inner.styles.len().saturating_sub(1).min((u16::MAX - 1) as usize) as u16;
        }
        let id = inner.styles.len() as u16;
        inner.styles.push(s);
        inner.themes.push(theme);
        inner.patterns.push(pattern);
        inner.quotes.push(quote);
        id
    }

    /// 基于已有样式 ID 派生修改后的新样式：凡未改动的颜色/填充/前导撇号，自动继承原主题色、图案与 quotePrefix。
    pub fn intern_derived(&mut self, base_id: u16, s: CellStyle) -> u16 {
        let base = self.get(base_id).clone();
        if s == base {
            return base_id;
        }
        let mut theme = self.theme_of(base_id);
        if s.color != base.color {
            theme.font = None;
        }
        if s.fill != base.fill {
            theme.fill = None;
        }
        let pattern = if s.fill != base.fill {
            None
        } else {
            self.pattern_of(base_id).cloned()
        };
        let quote = self.quote_of(base_id);
        self.intern_with_ext(s, theme, pattern, quote)
    }

    /// 将另一工作簿的样式 ID（含主题色、图案填充、quotePrefix）迁移驻留到本样式表。
    pub fn copy_from(&mut self, other: &StyleTable, sid: u16) -> u16 {
        if sid == 0 {
            return 0;
        }
        let s = other.get(sid).clone();
        let theme = other.theme_of(sid);
        let pattern = other.pattern_of(sid).cloned();
        let quote = other.quote_of(sid);
        self.intern_with_ext(s, theme, pattern, quote)
    }

    /// 记录样式号的主题色引用（不参与去重键：主题不同的两格颜色已由解析结果区分）
    pub fn set_theme(&mut self, id: u16, t: crate::style_theme::XfTheme) {
        if self.0.themes.get(id as usize).copied() == Some(t) {
            return;
        }
        if let Some(slot) = std::sync::Arc::make_mut(&mut self.0).themes.get_mut(id as usize) {
            *slot = t;
        }
    }

    pub fn theme_of(&self, id: u16) -> crate::style_theme::XfTheme {
        self.0.themes.get(id as usize).copied().unwrap_or_default()
    }

    pub fn set_pattern(&mut self, id: u16, p: crate::style_ext::FillPattern) {
        if let Some(slot) = std::sync::Arc::make_mut(&mut self.0).patterns.get_mut(id as usize) {
            *slot = Some(p);
        }
    }

    pub fn pattern_of(&self, id: u16) -> Option<&crate::style_ext::FillPattern> {
        self.0.patterns.get(id as usize).and_then(|p| p.as_ref())
    }

    pub fn set_diag(&mut self, id: u16, d: crate::style_ext::DiagBorder) {
        let inner = std::sync::Arc::make_mut(&mut self.0);
        if let Some(style) = inner.styles.get_mut(id as usize) {
            let old = hash_of(style);
            if inner.index.get(&old) == Some(&id) { inner.index.remove(&old); }
            style.diagonal = Some(d);
            inner.index.insert(hash_of(style), id);
        }
    }

    /// Intern a style with explicit `quotePrefix` flag without mutating shared unquoted styles.
    pub fn intern_with_quote(&mut self, s: CellStyle, quote: bool) -> u16 {
        if !quote {
            let k = hash_of(&s);
            if let Some(&id) = self.0.index.get(&k) {
                if self.0.styles[id as usize] == s && !self.quote_of(id) {
                    return id;
                }
            }
            for (idx, existing) in self.0.styles.iter().enumerate() {
                let id = idx as u16;
                if *existing == s
                    && !self.quote_of(id)
                    && self.theme_of(id) == crate::style_theme::XfTheme::default()
                    && self.pattern_of(id).is_none()
                {
                    return id;
                }
            }
            let inner = std::sync::Arc::make_mut(&mut self.0);
            if inner.styles.len() >= u16::MAX as usize {
                return inner.styles.len().saturating_sub(1).min((u16::MAX - 1) as usize) as u16;
            }
            let id = inner.styles.len() as u16;
            inner.styles.push(s);
            inner.themes.push(crate::style_theme::XfTheme::default());
            inner.patterns.push(None);
            inner.quotes.push(false);
            inner.index.insert(k, id);
            return id;
        }
        for (idx, existing) in self.0.styles.iter().enumerate() {
            let id = idx as u16;
            if *existing == s
                && self.quote_of(id)
                && self.theme_of(id) == crate::style_theme::XfTheme::default()
                && self.pattern_of(id).is_none()
            {
                return id;
            }
        }
        let inner = std::sync::Arc::make_mut(&mut self.0);
        if inner.styles.len() >= u16::MAX as usize {
            return inner.styles.len().saturating_sub(1).min((u16::MAX - 1) as usize) as u16;
        }
        let id = inner.styles.len() as u16;
        inner.styles.push(s);
        inner.themes.push(crate::style_theme::XfTheme::default());
        inner.patterns.push(None);
        inner.quotes.push(true);
        id
    }

    pub fn set_quote(&mut self, id: u16, q: bool) {
        if self.0.quotes.get(id as usize).copied() == Some(q) {
            return;
        }
        if let Some(slot) = std::sync::Arc::make_mut(&mut self.0).quotes.get_mut(id as usize) {
            *slot = q;
        }
    }

    pub fn quote_of(&self, id: u16) -> bool {
        self.0.quotes.get(id as usize).copied().unwrap_or(false)
    }

    pub fn diag_of(&self, id: u16) -> Option<&crate::style_ext::DiagBorder> {
        self.0.styles.get(id as usize)?.diagonal.as_ref()
    }

    pub fn get(&self, id: u16) -> &CellStyle {
        self.0.styles.get(id as usize).unwrap_or_else(|| &self.0.styles[0])
    }

    pub fn len(&self) -> usize {
        self.0.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.styles.is_empty()
    }
}

/// 把值按 Excel 格式代码显示（支持最多四段：正;负;零;文本）
/// 按指定工作簿的日期体系格式化（1904 的序列跟 1900 差 1462 天）
pub fn format_value_in(
    sys: crate::model::DateSystem,
    v: &crate::model::Value,
    fmt: &str,
) -> String {
    crate::formula::with_date_system(sys, || format_value(v, fmt))
}

/// 按字面格式串格式化（供 `TEXT()` 使用，不把 `mm-dd-yy` 改写成本机区域短日期）。
pub fn format_value_literal_in(
    sys: crate::model::DateSystem,
    v: &crate::model::Value,
    fmt: &str,
) -> String {
    crate::formula::with_date_system(sys, || format_value_core(v, fmt, false))
}

/// Fast stack-allocated ASCII integer formatter into `[u8; 24]`.
#[inline]
pub(crate) fn format_i64_into(v: i64, buf: &mut [u8; 24]) -> &str {
    if v == 0 {
        buf[23] = b'0';
        return "0";
    }
    let neg = v < 0;
    let mut u = v.unsigned_abs();
    let mut pos = 24usize;
    while u > 0 {
        pos -= 1;
        buf[pos] = b'0' + (u % 10) as u8;
        u /= 10;
    }
    if neg {
        pos -= 1;
        buf[pos] = b'-';
    }
    // SAFETY: only ASCII digits and '-' written into `buf[pos..24]`.
    unsafe { std::str::from_utf8_unchecked(&buf[pos..24]) }
}

/// Excel「常规」在列宽放不下时的显示规则（实测 Excel 16.0）：
/// 最多 11 个字符；整数 11 位以内原样、12 位起科学计数；小数按可用宽度
/// 四舍五入并去掉尾零；有效位不足 3 位时改科学计数；科学计数尾零同样去掉。
pub fn general_display(n: f64, cap: usize) -> String {
    use std::fmt::Write;
    if n == 0.0 {
        return "0".into();
    }
    if !n.is_finite() {
        return n.to_string();
    }
    let cap = cap.max(4);
    let neg = n < 0.0;
    let a = n.abs();
    let avail = cap.saturating_sub(usize::from(neg));

    // Fast stack path for exact integers up to 11 digits (covers >80% of IDs/counts/amounts).
    if a.fract() == 0.0 && a < 1e11 {
        let mut ibuf = [0u8; 24];
        let digits = format_i64_into(a as i64, &mut ibuf);
        if digits.len() <= 11.min(avail) {
            if neg {
                let mut out = String::with_capacity(digits.len() + 1);
                out.push('-');
                out.push_str(digits);
                return out;
            }
            return digits.to_owned();
        }
    }

    // 用 15 位有效数字定指数，避免 log10 在 1eN 边界上的浮点误差（栈缓冲零堆分配）
    let mut sci_buf = StackFmt::<48>::new();
    let _ = write!(sci_buf, "{a:.15e}");
    let sci = sci_buf.as_str();
    let (_, exp_s) = sci.split_once('e').unwrap_or((sci, "0"));
    let exp: i32 = exp_s.parse().unwrap_or(0);
    let exp_chars = if exp.abs() >= 100 { 5 } else { 4 }; // E+11 / E+100
    // 列够宽时 Excel 的 11 字符上限只算数字主体，负号另占一格；
    // 窄列（默认 8.43）则连负号一起算进总宽。
    let sci_avail = if neg && cap < 11 { cap - 1 } else { cap };
    let mut out_buf = StackFmt::<64>::new();
    if neg {
        let _ = out_buf.write_str("-");
    }
    let body_start = out_buf.len;

    if exp >= 0 {
        let int_digits = exp as usize + 1;
        if int_digits <= 11.min(avail) {
            if a.fract() == 0.0 {
                if write!(out_buf, "{a:.0}").is_err() {
                    let mut fb = format!("{a:.0}");
                    if neg {
                        fb.insert(0, '-');
                    }
                    return fb;
                }
            } else {
                let decimals = avail.saturating_sub(int_digits + 1);
                if write!(out_buf, "{a:.decimals$}").is_err() {
                    let mut fb = trim_zeros(format!("{a:.decimals$}"));
                    if fb.is_empty() {
                        fb = "0".into();
                    }
                    if neg {
                        fb.insert(0, '-');
                    }
                    return fb;
                }
                out_buf.trim_decimal_zeros_from(body_start);
                if out_buf.len == body_start {
                    let _ = out_buf.write_str("0");
                }
            }
        } else {
            let decimals = sci_avail.saturating_sub(1 + 1 + exp_chars).min(5);
            write_sci_into(&mut out_buf, a, exp, decimals);
        }
    } else {
        let decimals_avail = avail.saturating_sub(2);
        let leading = (-exp - 1) as usize;
        // 固定小数至少能写出 3 位有效数字，否则 Excel 改科学计数
        if leading + 3 <= decimals_avail {
            let decimals = decimals_avail.min(15);
            let _ = write!(out_buf, "{a:.decimals$}");
            out_buf.trim_decimal_zeros_from(body_start);
            if out_buf.len == body_start {
                let _ = out_buf.write_str("0");
            }
        } else {
            let decimals = sci_avail.saturating_sub(1 + 1 + exp_chars).min(5);
            write_sci_into(&mut out_buf, a, exp, decimals);
        }
    }
    out_buf.as_str().to_owned()
}

fn trim_zeros(mut s: String) -> String {
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

#[inline]
fn write_sci_into<const N: usize>(out: &mut StackFmt<N>, a: f64, mut exp: i32, decimals: usize) {
    use std::fmt::Write;
    let mut m_buf = StackFmt::<32>::new();
    let mant = a / 10f64.powi(exp);
    let _ = write!(m_buf, "{mant:.decimals$}");
    m_buf.trim_decimal_zeros_from(0);
    // 9.999…E+10 四舍五入后会变成 10 -> 1E+11，指数要进位
    if m_buf.as_str().starts_with("10") {
        exp += 1;
        m_buf.len = 0;
        let mant2 = a / 10f64.powi(exp);
        let _ = write!(m_buf, "{mant2:.decimals$}");
        m_buf.trim_decimal_zeros_from(0);
    }
    let _ = write!(
        out,
        "{}E{}{:02}",
        m_buf.as_str(),
        if exp < 0 { '-' } else { '+' },
        exp.abs()
    );
}

/// 带列宽上限的通用入口：只有「常规」受 cap 影响，其他格式原样走 format_value。
pub fn format_value_in_fit(
    sys: crate::model::DateSystem,
    v: &crate::model::Value,
    fmt: &str,
    cap: usize,
) -> String {
    crate::formula::with_date_system(sys, || format_value_fit(v, fmt, cap))
}

pub fn format_value_fit(v: &crate::model::Value, fmt: &str, cap: usize) -> String {
    use crate::model::Value;
    if fmt.trim().is_empty() || fmt.eq_ignore_ascii_case("general") {
        return match v {
            Value::Number(n) => general_display(*n, cap),
            other => other.display(),
        };
    }
    format_value(v, fmt)
}

// Rendered values vary by cell. The format sections, signs, date token programs,
// and literal parts of ordinary numeric patterns can be shared by all cells using that format.
#[derive(Clone, Copy, Debug)]
enum CondOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, Debug)]
struct SectionCond {
    op: CondOp,
    val: f64,
}

impl SectionCond {
    fn matches(self, n: f64) -> bool {
        match self.op {
            CondOp::Eq => (n - self.val).abs() <= f64::EPSILON,
            CondOp::Ne => (n - self.val).abs() > f64::EPSILON,
            CondOp::Lt => n < self.val,
            CondOp::Le => n <= self.val,
            CondOp::Gt => n > self.val,
            CondOp::Ge => n >= self.val,
        }
    }
}

struct FormatSection {
    raw: String,
    color: Option<[u8; 3]>,
    cond: Option<SectionCond>,
    has_neg_marker: bool,
    dbnum: Option<crate::dbnum::DbNumKind>,
    kind: SectionKind,
}

enum SectionKind {
    General,
    Date(DateSection),
    DbNum(crate::dbnum::DbNumKind),
    Scientific {
        prefix: String,
        suffix: String,
        int_slots: usize,
        dec_slots: Vec<char>,
        exp_digits: usize,
        plus: bool,
        scale: f64,
    },
    Mask(MaskSection),
    Number(NumberSection),
    Fallback,
}

impl FormatSection {
    fn renders_number(&self) -> bool {
        match &self.kind {
            SectionKind::General
            | SectionKind::Date(_)
            | SectionKind::DbNum(_)
            | SectionKind::Scientific { .. }
            | SectionKind::Mask(_)
            | SectionKind::Number(_) => true,
            SectionKind::Fallback => {
                let cleaned = strip_brackets(self.raw.trim());
                cleaned.contains('/') && cleaned.contains('?')
            }
        }
    }

    fn compile(raw: String) -> Self {
        let color = section_color(&raw);
        let cond = section_cond(&raw);
        let trimmed = raw.trim();
        let dbnum = crate::dbnum::detect_dbnum(trimmed);
        let cleaned_for_neg = strip_brackets(trimmed);
        let has_neg_marker = cleaned_for_neg.contains('-') || cleaned_for_neg.contains('(');
        let kind = if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("general") {
            SectionKind::General
        } else if is_date_format(trimmed) {
            SectionKind::Date(DateSection::compile(trimmed))
        } else if let Some(kind) = dbnum {
            SectionKind::DbNum(kind)
        } else {
            let sec = cleaned_for_neg.trim();
            let pct = count_unquoted(sec, '%') as i32;
            let permille = count_unquoted(sec, '‰') as i32;
            let base_scale = 100f64.powi(pct) * 1000f64.powi(permille);
            if let Some(epos) = find_unquoted_exp(sec) {
                let before = &sec[..epos];
                let after = &sec[epos + 1..];
                if after.starts_with('+') || after.starts_with('-') {
                    let (prefix, int_slots, dec_slots, scale_commas) =
                        if let Some((ns, ne)) = find_numeric_pattern_span(before) {
                            let pat = &before[ns..ne];
                            let pat_trim = pat.trim_end_matches(',');
                            let mut sc = (pat.len() - pat_trim.len()) as i32;
                            let (raw_i, dec_p) = pat_trim.split_once('.').unwrap_or((pat_trim, ""));
                            let int_trim = raw_i.trim_end_matches(',');
                            sc += (raw_i.len() - int_trim.len()) as i32;
                            let islots = int_trim
                                .chars()
                                .filter(|c| matches!(c, '0' | '#' | '?'))
                                .count()
                                .max(1);
                            let dslots: Vec<char> = dec_p
                                .chars()
                                .filter(|c| matches!(c, '0' | '#' | '?'))
                                .collect();
                            (render_literals(&before[..ns]), islots, dslots, sc)
                        } else {
                            (
                                String::new(),
                                1,
                                vec!['0'; decimals_in(before)],
                                0,
                            )
                        };
                    let after_sign = &after[1..];
                    let mut exp_digits = 0usize;
                    let mut exp_end = 1usize;
                    for (rel, ch) in after_sign.char_indices() {
                        if matches!(ch, '0' | '#' | '?') {
                            exp_digits += 1;
                            exp_end = 1 + rel + ch.len_utf8();
                        } else {
                            break;
                        }
                    }
                    let suffix = render_literals(&after[exp_end..]);
                    SectionKind::Scientific {
                        prefix,
                        suffix,
                        int_slots,
                        dec_slots,
                        exp_digits: exp_digits.max(1),
                        plus: after.starts_with('+'),
                        scale: base_scale / 1000f64.powi(scale_commas),
                    }
                } else if sec.contains('/') && sec.contains('?') {
                    SectionKind::Fallback
                } else if let Some(mask) = MaskSection::from_pattern(sec, base_scale) {
                    SectionKind::Mask(mask)
                } else if let Some(num) = NumberSection::from_numeric_pattern(sec) {
                    SectionKind::Number(num)
                } else {
                    SectionKind::Fallback
                }
            } else if sec.contains('/') && sec.contains('?') {
                SectionKind::Fallback
            } else if let Some(mask) = MaskSection::from_pattern(sec, base_scale) {
                SectionKind::Mask(mask)
            } else if let Some(num) = NumberSection::from_numeric_pattern(sec) {
                SectionKind::Number(num)
            } else {
                SectionKind::Fallback
            }
        };
        Self {
            raw,
            color,
            cond,
            has_neg_marker,
            dbnum,
            kind,
        }
    }
}

fn find_numeric_pattern_span(sec: &str) -> Option<(usize, usize)> {
    let mut it = sec.char_indices().peekable();
    let mut start = None;
    while let Some((idx, c)) = it.next() {
        match c {
            '"' => {
                for (_, c2) in it.by_ref() {
                    if c2 == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                let _ = it.next();
            }
            '0' | '#' | '?' => {
                start = Some(idx);
                break;
            }
            _ => {}
        }
    }
    let start = start?;
    let mut end = sec.len();
    for (rel, c) in sec[start..].char_indices() {
        if !matches!(c, '0' | '#' | '?' | ',' | '.') {
            end = start + rel;
            break;
        }
    }
    Some((start, end))
}

fn find_unquoted_exp(sec: &str) -> Option<usize> {
    let mut it = sec.char_indices().peekable();
    while let Some((idx, c)) = it.next() {
        match c {
            '"' => {
                for (_, c2) in it.by_ref() {
                    if c2 == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                let _ = it.next();
            }
            'E' | 'e' => {
                let after = &sec[idx + 1..];
                if after.starts_with('+') || after.starts_with('-') {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn count_unquoted(sec: &str, needle: char) -> usize {
    let mut n = 0usize;
    let mut it = sec.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                for c2 in it.by_ref() {
                    if c2 == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                let _ = it.next();
            }
            x if x == needle => n += 1,
            _ => {}
        }
    }
    n
}

fn section_cond(sec: &str) -> Option<SectionCond> {
    let mut chars = sec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                for c2 in chars.by_ref() {
                    if c2 == '"' {
                        break;
                    }
                }
            }
            '\\' => {
                let _ = chars.next();
            }
            '[' => {
                let mut buf = String::new();
                for c2 in chars.by_ref() {
                    if c2 == ']' {
                        break;
                    }
                    buf.push(c2);
                }
                let t = buf.trim();
                let (op, rest) = if let Some(r) = t.strip_prefix(">=") {
                    (CondOp::Ge, r)
                } else if let Some(r) = t.strip_prefix("<=") {
                    (CondOp::Le, r)
                } else if let Some(r) = t.strip_prefix("<>") {
                    (CondOp::Ne, r)
                } else if let Some(r) = t.strip_prefix('>') {
                    (CondOp::Gt, r)
                } else if let Some(r) = t.strip_prefix('<') {
                    (CondOp::Lt, r)
                } else if let Some(r) = t.strip_prefix('=') {
                    (CondOp::Eq, r)
                } else {
                    continue;
                };
                if let Ok(val) = rest.trim().parse::<f64>()
                    && val.is_finite()
                {
                    return Some(SectionCond { op, val });
                }
            }
            _ => {}
        }
    }
    None
}

fn select_numeric_section(sections: &[FormatSection], n: f64) -> Option<(&FormatSection, f64, bool)> {
    let num_len = sections.len().min(3);
    let num_secs = &sections[..num_len];
    if num_secs.iter().any(|s| s.cond.is_some()) {
        for s in num_secs {
            if s.cond.map_or(true, |c| c.matches(n)) {
                let neg_cond_handles_sign = matches!(
                    s.cond,
                    Some(SectionCond { op: CondOp::Lt | CondOp::Le, val }) if val <= 0.0
                ) && s.has_neg_marker;
                let need_minus = n < 0.0 && s.renders_number() && !neg_cond_handles_sign;
                return Some((s, n.abs(), need_minus));
            }
        }
        return None;
    }
    if n < 0.0 {
        if sections.len() >= 2 {
            Some((&sections[1], -n, false))
        } else {
            Some((&sections[0], -n, sections[0].renders_number()))
        }
    } else if n == 0.0 && sections.len() >= 3 {
        Some((&sections[2], 0.0, false))
    } else {
        Some((&sections[0], n, false))
    }
}

enum MaskTok {
    Digit(char),
    Lit(String),
}

/// 掩码数字格式（如 `000-0000`、`0000 0000`、`000-00-0000`、`(000) 000-0000`）：
/// 无小数点且数字占位符被字面量分隔为多段时，按右对齐逐位填槽，超出部分挂到最左数字槽前。
struct MaskSection {
    tokens: Vec<MaskTok>,
    digit_count: usize,
    has_zero: bool,
    scale: f64,
}

impl MaskSection {
    fn from_pattern(sec: &str, scale: f64) -> Option<Self> {
        let mut tokens: Vec<MaskTok> = Vec::new();
        let mut digit_count = 0usize;
        let mut digit_runs = 0usize;
        let mut in_run = false;
        let mut has_zero = false;
        let mut has_dot = false;
        let mut lit_buf = String::new();
        let flush_lit = |buf: &mut String, toks: &mut Vec<MaskTok>| {
            if !buf.is_empty() {
                toks.push(MaskTok::Lit(std::mem::take(buf)));
            }
        };
        let mut it = sec.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '"' => {
                    in_run = false;
                    for c2 in it.by_ref() {
                        if c2 == '"' {
                            break;
                        }
                        lit_buf.push(c2);
                    }
                }
                '\\' => {
                    in_run = false;
                    if let Some(c2) = it.next() {
                        lit_buf.push(c2);
                    }
                }
                '_' => {
                    in_run = false;
                    let _ = it.next();
                    lit_buf.push(' ');
                }
                '*' => {
                    in_run = false;
                    let _ = it.next();
                }
                '[' => {
                    in_run = false;
                    let mut buf = String::new();
                    for c2 in it.by_ref() {
                        if c2 == ']' {
                            break;
                        }
                        buf.push(c2);
                    }
                    if let Some(rest) = buf.strip_prefix('$') {
                        lit_buf.push_str(rest.split('-').next().unwrap_or(""));
                    }
                }
                '.' => {
                    has_dot = true;
                    break;
                }
                '0' | '#' | '?' => {
                    flush_lit(&mut lit_buf, &mut tokens);
                    if !in_run {
                        digit_runs += 1;
                        in_run = true;
                    }
                    if c == '0' {
                        has_zero = true;
                    }
                    digit_count += 1;
                    tokens.push(MaskTok::Digit(c));
                }
                ',' if in_run => {
                    // 千分位逗号不属于掩码分隔，交由普通 NumberSection 处理
                    return None;
                }
                '@' => {
                    in_run = false;
                }
                _ => {
                    in_run = false;
                    lit_buf.push(c);
                }
            }
        }
        if has_dot || digit_runs < 2 || digit_count == 0 {
            return None;
        }
        flush_lit(&mut lit_buf, &mut tokens);
        Some(Self {
            tokens,
            digit_count,
            has_zero,
            scale,
        })
    }

    fn render(&self, val: f64) -> String {
        let a = (val * self.scale).abs().round();
        let digits: Vec<char> = if a == 0.0 && !self.has_zero {
            Vec::new()
        } else {
            format!("{:.0}", a).chars().collect()
        };
        let n_slots = self.digit_count;
        let overflow = digits.len().saturating_sub(n_slots);
        let pad = n_slots.saturating_sub(digits.len());
        let mut out = String::with_capacity(self.tokens.len() + overflow + 4);
        let mut slot_idx = 0usize;
        for tok in &self.tokens {
            match tok {
                MaskTok::Lit(s) => out.push_str(s),
                MaskTok::Digit(kind) => {
                    if slot_idx == 0 && overflow > 0 {
                        for &ch in &digits[..overflow] {
                            out.push(ch);
                        }
                    }
                    if slot_idx < pad {
                        match kind {
                            '0' => out.push('0'),
                            '?' => out.push(' '),
                            _ => {}
                        }
                    } else {
                        let di = overflow + (slot_idx - pad);
                        if let Some(&ch) = digits.get(di) {
                            out.push(ch);
                        }
                    }
                    slot_idx += 1;
                }
            }
        }
        out
    }
}

struct NumberSection {
    prefix: String,
    suffix: String,
    scale: f64,
    dec: usize,
    dec_slots: Vec<char>,
    min_int: usize,
    q_int: usize,
    thousands: bool,
    zero_spaces: Option<usize>,
}

impl NumberSection {
    fn from_numeric_pattern(sec: &str) -> Option<Self> {
        let (start, end) = find_numeric_pattern_span(sec)?;
        let pattern = &sec[start..end];
        let pattern_trimmed = pattern.trim_end_matches(',');
        let mut scale_commas = (pattern.len() - pattern_trimmed.len()) as i32;
        let (raw_int, dec_part) = match pattern_trimmed.split_once('.') {
            Some((i, f)) => (i, f),
            None => (pattern_trimmed, ""),
        };
        let int_trimmed = raw_int.trim_end_matches(',');
        scale_commas += (raw_int.len() - int_trimmed.len()) as i32;
        let dec_slots: Vec<char> = dec_part
            .chars()
            .filter(|c| matches!(c, '0' | '#' | '?'))
            .collect();
        let dec = dec_slots.len();
        let min_int = int_trimmed.chars().filter(|c| *c == '0').count();
        let q_int = int_trimmed
            .chars()
            .filter(|c| matches!(c, '0' | '?'))
            .count();
        let thousands = int_trimmed.contains(',');
        let zero_spaces = (!pattern_trimmed.contains('0'))
            .then(|| pattern_trimmed.chars().filter(|c| matches!(c, '0' | '#' | '?')).count());
        let percent = count_unquoted(sec, '%') as i32;
        let permille = count_unquoted(sec, '‰') as i32;
        let scale = 100f64.powi(percent) * 1000f64.powi(permille) / 1000f64.powi(scale_commas);
        Some(Self {
            prefix: render_literals(&sec[..start]),
            suffix: render_literals(&sec[end..]),
            scale,
            dec,
            dec_slots,
            min_int,
            q_int,
            thousands,
            zero_spaces,
        })
    }

    fn render(&self, val: f64) -> String {
        use std::fmt::Write;
        let a = (val * self.scale).abs();
        let mut out = String::with_capacity(self.prefix.len() + self.suffix.len() + 24);
        out.push_str(&self.prefix);
        if a == 0.0
            && let Some(spaces) = self.zero_spaces
        {
            for _ in 0..spaces {
                out.push(' ');
            }
            out.push_str(&self.suffix);
            return out;
        }
        let mut stack = StackFmt::<64>::new();
        let owned;
        let raw = if write!(stack, "{:.*}", self.dec, a).is_ok() {
            stack.as_str()
        } else {
            owned = format!("{:.*}", self.dec, a);
            &owned
        };
        let (int_part, frac_part) = match raw.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (raw, None),
        };
        // 整数部分：先补强制 '0'（min_int），再按需补 '?' 前导空格（q_int）
        let zero_pad = self.min_int.saturating_sub(int_part.len());
        let int_with_zeros_len = int_part.len() + zero_pad;
        let space_pad = self.q_int.saturating_sub(int_with_zeros_len);
        for _ in 0..space_pad {
            out.push(' ');
        }
        if zero_pad > 0 {
            let mut padded = String::with_capacity(int_with_zeros_len);
            for _ in 0..zero_pad {
                padded.push('0');
            }
            padded.push_str(int_part);
            if self.thousands {
                push_grouped_ascii(&mut out, &padded);
            } else {
                out.push_str(&padded);
            }
        } else if self.thousands {
            push_grouped_ascii(&mut out, int_part);
        } else {
            out.push_str(int_part);
        }
        // 小数部分：精确按每个槽位类型处理 '0'（保留） / '#'（尾零省略） / '?'（尾零转空格对齐）
        if let Some(f) = frac_part {
            append_formatted_frac(&mut out, f, &self.dec_slots);
        }
        out.push_str(&self.suffix);
        out
    }
}

fn append_formatted_frac(out: &mut String, frac: &str, slots: &[char]) {
    if frac.is_empty() || slots.is_empty() {
        return;
    }
    let bytes = frac.as_bytes();
    let n = bytes.len().min(slots.len());
    // 找到最后一个显著位（非 '0' 或槽位为 '0'）
    let mut last_sig: Option<usize> = None;
    for i in (0..n).rev() {
        if bytes[i] != b'0' || slots[i] == '0' {
            last_sig = Some(i);
            break;
        }
    }
    let mut rendered = String::with_capacity(n);
    let mut any_digit = false;
    let mut any_space = false;
    for i in 0..n {
        if last_sig.is_some_and(|ls| i <= ls) {
            rendered.push(bytes[i] as char);
            any_digit = true;
        } else if slots[i] == '?' {
            rendered.push(' ');
            any_space = true;
        }
    }
    if any_digit {
        out.push('.');
        out.push_str(&rendered);
    } else if any_space {
        // 小数全为 '?' 空格时，小数点也用空格占位以对齐小数点列
        out.push(' ');
        out.push_str(&rendered);
    }
}

pub(crate) struct StackFmt<const N: usize> {
    pub(crate) buf: [u8; N],
    pub(crate) len: usize,
}

impl<const N: usize> StackFmt<N> {
    #[inline]
    pub(crate) fn new() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
        }
    }

    #[inline]
    pub(crate) fn as_str(&self) -> &str {
        // SAFETY: only valid UTF-8 chunks from `Write::write_str` are appended.
        unsafe { std::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }

    #[inline]
    pub(crate) fn trim_decimal_zeros_from(&mut self, start: usize) {
        if self.buf[start..self.len].contains(&b'.') {
            while self.len > start && self.buf[self.len - 1] == b'0' {
                self.len -= 1;
            }
            if self.len > start && self.buf[self.len - 1] == b'.' {
                self.len -= 1;
            }
        }
    }
}

impl<const N: usize> std::fmt::Write for StackFmt<N> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let bytes = s.as_bytes();
        if self.len + bytes.len() > N {
            return Err(std::fmt::Error);
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(())
    }
}

#[inline]
fn push_grouped_ascii(out: &mut String, formatted: &str) {
    let (int_part, frac) = match formatted.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (formatted, None),
    };
    let ib = int_part.as_bytes();
    if ib.len() <= 3 || !ib.iter().all(|b| b.is_ascii_digit()) {
        out.push_str(int_part);
    } else {
        let first = ib.len() % 3;
        let mut pos = if first == 0 { 3 } else { first };
        out.push_str(&int_part[..pos]);
        while pos < ib.len() {
            out.push(',');
            out.push_str(&int_part[pos..pos + 3]);
            pos += 3;
        }
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
}

/// Two-generation compiled format cache (2 × 512 = 1024 entries) plus a 1-slot MRU fast path
/// so repeated calls with the same format avoid hashing and >128 distinct formats never thrash.
#[derive(Default)]
struct FormatCache {
    last_key: String,
    last_val: Option<std::rc::Rc<[FormatSection]>>,
    hot: HashMap<String, std::rc::Rc<[FormatSection]>>,
    cold: HashMap<String, std::rc::Rc<[FormatSection]>>,
}

impl FormatCache {
    const GEN_CAP: usize = 512;

    fn get_or_compile(&mut self, fmt: &str) -> std::rc::Rc<[FormatSection]> {
        if let Some(ref hit) = self.last_val
            && self.last_key == fmt
        {
            return hit.clone();
        }
        if let Some(hit) = self.hot.get(fmt) {
            let hit = hit.clone();
            if fmt.len() <= 4096 {
                self.last_key.clear();
                self.last_key.push_str(fmt);
                self.last_val = Some(hit.clone());
            }
            return hit;
        }
        if let Some(hit) = self.cold.remove(fmt) {
            if self.hot.len() >= Self::GEN_CAP {
                self.cold = std::mem::take(&mut self.hot);
            }
            self.hot.insert(fmt.to_owned(), hit.clone());
            if fmt.len() <= 4096 {
                self.last_key.clear();
                self.last_key.push_str(fmt);
                self.last_val = Some(hit.clone());
            }
            return hit;
        }
        let sections: std::rc::Rc<[_]> = split_sections(fmt)
            .into_iter()
            .map(FormatSection::compile)
            .collect();
        if fmt.len() <= 4096 {
            if self.hot.len() >= Self::GEN_CAP {
                self.cold = std::mem::take(&mut self.hot);
            }
            self.hot.insert(fmt.to_owned(), sections.clone());
            self.last_key.clear();
            self.last_key.push_str(fmt);
            self.last_val = Some(sections.clone());
        }
        sections
    }
}

fn format_sections(fmt: &str) -> std::rc::Rc<[FormatSection]> {
    thread_local! {
        static CACHE: std::cell::RefCell<FormatCache> = Default::default();
    }
    CACHE.with(|cache| cache.borrow_mut().get_or_compile(fmt))
}

pub fn format_value(v: &crate::model::Value, fmt: &str) -> String {
    format_value_core(v, fmt, true)
}

fn format_value_core(v: &crate::model::Value, fmt: &str, localize_short_date: bool) -> String {
    use crate::model::Value;
    if fmt.trim().is_empty() || fmt.eq_ignore_ascii_case("general") {
        return match v {
            Value::Number(n) => general_display(*n, 11),
            other => other.display(),
        };
    }
    // Excel 的内建 14 号格式是「系统区域短日期」，不同机器显示不同；
    // 读盘时它被还原成 mm-dd-yy，这里按系统设置换成本机的写法（和 Excel 一致）。
    let localized = if localize_short_date {
        localize_builtin_date(fmt)
    } else {
        None
    };
    let fmt: &str = localized.as_deref().unwrap_or(fmt);
    let sections = format_sections(fmt);
    match v {
        Value::Number(n) => {
            let Some((sec, val, need_minus)) = select_numeric_section(&sections, *n) else {
                return general_number(*n);
            };
            let mut out = match &sec.kind {
                SectionKind::General => general_number(val),
                SectionKind::Date(date) => {
                    let sys = crate::formula::current_date_system();
                    if !sys.is_1904() && *n < 0.0 {
                        return "###".to_string();
                    }
                    if let Some(s) = date.render(sys, val) {
                        if s == "###" {
                            return s;
                        }
                        match sec.dbnum {
                            Some(kind) => crate::dbnum::map_digits_for_date(&s, kind),
                            None => s,
                        }
                    } else {
                        render_section_non_date(val, &sec.raw, sec.dbnum)
                    }
                }
                SectionKind::DbNum(kind) => crate::dbnum::format_dbnum(val, *kind, &sec.raw)
                    .unwrap_or_else(|| render_section_numeric(val, &sec.raw)),
                SectionKind::Scientific {
                    prefix,
                    suffix,
                    int_slots,
                    dec_slots,
                    exp_digits,
                    plus,
                    scale,
                } => render_scientific_full(
                    val * *scale,
                    prefix,
                    suffix,
                    *int_slots,
                    dec_slots,
                    *exp_digits,
                    *plus,
                ),
                SectionKind::Mask(mask) => mask.render(val),
                SectionKind::Number(pattern) => pattern.render(val),
                SectionKind::Fallback => render_section_numeric(val, &sec.raw),
            };
            // Excel 单段格式对负数：先取绝对值套格式，再在最前面补一个负号
            // （`(#,##0.00)` → `-(1,234.50)`；`[Red]-#,##0.00` → `--987.65`）
            if need_minus {
                if sec.dbnum.is_some() {
                    if !out.starts_with('负') {
                        out.insert(0, '负');
                    }
                } else {
                    out.insert(0, '-');
                }
            }
            out
        }
        Value::Text(t) => {
            // 文本优先用第 4 段；没有第 4 段时，第 1 段里有 @ 也照样套
            if sections.len() >= 4 {
                render_text_section(t, &sections[3].raw)
            } else if sections[0].raw.contains('@') {
                render_text_section(t, &sections[0].raw)
            } else {
                t.to_string()
            }
        }
        _ => v.display(),
    }
}

/// 内建短日期（14 号）在本机的写法；其它格式原样返回 None。
/// Excel 在中文/欧洲区域显示 29/11/2025，在美式区域显示 11-29-25——
/// 我们按系统的 LOCALE_SSHORTDATE 走，打开同一份文件才和 Excel 一致。
fn localize_builtin_date(fmt: &str) -> Option<std::rc::Rc<str>> {
    let t = fmt.trim();
    let is_short_date = t.eq_ignore_ascii_case("mm-dd-yy")
        || t.eq_ignore_ascii_case("m/d/yy")
        || t.eq_ignore_ascii_case("m-d-yy")
        || t.eq_ignore_ascii_case("mm-dd-yyyy");
    if !is_short_date {
        return None;
    }
    thread_local! {
        static SHORT_DATE: std::cell::RefCell<Option<(std::time::Instant, std::rc::Rc<str>)>> =
            const { std::cell::RefCell::new(None) };
    }
    SHORT_DATE.with(|slot| {
        let mut cache = slot.borrow_mut();
        if let Some((checked, pattern)) = &*cache {
            if checked.elapsed() < std::time::Duration::from_secs(1) { return Some(pattern.clone()); }
        }
        let pattern: std::rc::Rc<str> = system_short_date().into();
        *cache = Some((std::time::Instant::now(), pattern.clone()));
        Some(pattern)
    })
}

#[cfg(windows)]
fn system_short_date() -> String {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocaleInfoW(locale: u32, lctype: u32, buf: *mut u16, cch: i32) -> i32;
    }
    const LOCALE_USER_DEFAULT: u32 = 0x0400;
    const LOCALE_SSHORTDATE: u32 = 0x1F;
    let mut buf = [0u16; 80];
    let n = unsafe {
        GetLocaleInfoW(
            LOCALE_USER_DEFAULT,
            LOCALE_SSHORTDATE,
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    if n > 1 {
        let s: std::ffi::OsString = std::ffi::OsString::from_wide(&buf[..(n as usize - 1)]);
        let s = s.to_string_lossy().to_lowercase();
        if !s.trim().is_empty() {
            return s;
        }
    }
    "yyyy-mm-dd".into()
}

#[cfg(not(windows))]
fn system_short_date() -> String {
    "yyyy-mm-dd".into()
}

/// 会计/重复字符格式（含未加引号的 `*<ch>`）拆分：
/// 返回 `(左段文本, 填充字符, 右段文本)`，供 GPU 网格与打印导出做左右贴边与中间平铺填充。
pub fn format_accounting_split(
    sys: crate::model::DateSystem,
    v: &crate::model::Value,
    fmt: &str,
) -> Option<(String, char, String)> {
    use crate::model::Value;
    if !fmt.contains('*') {
        return None;
    }
    let sections = format_sections(fmt);
    const SENTINEL: char = '\u{E000}';
    let find_star = |raw: &str| -> Option<(usize, char, usize)> {
        let mut it = raw.char_indices().peekable();
        while let Some((idx, c)) = it.next() {
            match c {
                '"' => {
                    for (_, c2) in it.by_ref() {
                        if c2 == '"' {
                            break;
                        }
                    }
                }
                '\\' | '_' => {
                    let _ = it.next();
                }
                '[' => {
                    for (_, c2) in it.by_ref() {
                        if c2 == ']' {
                            break;
                        }
                    }
                }
                '*' => {
                    if let Some((_, rep)) = it.next() {
                        let after = idx + 1 + rep.len_utf8();
                        return Some((idx, rep, after));
                    }
                }
                _ => {}
            }
        }
        None
    };

    match v {
        Value::Number(n) => {
            let (sec, val, need_minus) = select_numeric_section(&sections, *n)?;
            let (spos, rep, after) = find_star(&sec.raw)?;
            let mut marked = String::with_capacity(sec.raw.len() + 4);
            marked.push_str(&sec.raw[..spos]);
            marked.push('"');
            marked.push(SENTINEL);
            marked.push('"');
            marked.push_str(&sec.raw[after..]);
            let compiled = FormatSection::compile(marked);
            let mut rendered = match &compiled.kind {
                SectionKind::General => general_number(val),
                SectionKind::Date(date) => date
                    .render(sys, val)
                    .unwrap_or_else(|| render_section_non_date(val, &compiled.raw, compiled.dbnum)),
                SectionKind::DbNum(kind) => crate::dbnum::format_dbnum(val, *kind, &compiled.raw)
                    .unwrap_or_else(|| render_section_numeric(val, &compiled.raw)),
                SectionKind::Scientific {
                    prefix,
                    suffix,
                    int_slots,
                    dec_slots,
                    exp_digits,
                    plus,
                    scale,
                } => render_scientific_full(
                    val * *scale,
                    prefix,
                    suffix,
                    *int_slots,
                    dec_slots,
                    *exp_digits,
                    *plus,
                ),
                SectionKind::Mask(mask) => mask.render(val),
                SectionKind::Number(pattern) => pattern.render(val),
                SectionKind::Fallback => render_section_numeric(val, &compiled.raw),
            };
            if rendered == "###" {
                return None;
            }
            if need_minus {
                if compiled.dbnum.is_some() {
                    if !rendered.starts_with('负') {
                        rendered.insert(0, '负');
                    }
                } else {
                    rendered.insert(0, '-');
                }
            }
            let (left, right) = rendered.split_once(SENTINEL)?;
            Some((left.to_owned(), rep, right.to_owned()))
        }
        Value::Text(t) => {
            let raw = if sections.len() >= 4 {
                &sections[3].raw
            } else if sections[0].raw.contains('@') {
                &sections[0].raw
            } else {
                return None;
            };
            let (spos, rep, after) = find_star(raw)?;
            let mut marked = String::with_capacity(raw.len() + 4);
            marked.push_str(&raw[..spos]);
            marked.push('"');
            marked.push(SENTINEL);
            marked.push('"');
            marked.push_str(&raw[after..]);
            let rendered = render_text_section(t, &marked);
            let (left, right) = rendered.split_once(SENTINEL)?;
            Some((left.to_owned(), rep, right.to_owned()))
        }
        _ => None,
    }
}

/// 格式代码里指定的文字颜色（[Red] / [Color3] 这类）；没有就返回 None
pub fn format_color(v: &crate::model::Value, fmt: &str) -> Option<[u8; 3]> {
    use crate::model::Value;
    if !fmt.contains('[') { return None; }
    let sections = format_sections(fmt);
    match v {
        Value::Number(n) => select_numeric_section(&sections, *n).and_then(|(s, _, _)| s.color),
        Value::Text(_) if sections.len() >= 4 => sections[3].color,
        _ => sections.first().and_then(|s| s.color),
    }
}

fn section_color(sec: &str) -> Option<[u8; 3]> {
    let mut chars = sec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
            }
            continue;
        }
        if c == '\\' {
            let _ = chars.next();
            continue;
        }
        if c == '[' {
            let mut buf = String::new();
            for c2 in chars.by_ref() {
                if c2 == ']' {
                    break;
                }
                buf.push(c2);
            }
            let b = buf.to_lowercase();
            let named = match b.as_str() {
                "black" => Some([0, 0, 0]),
                "white" => Some([255, 255, 255]),
                "red" => Some([255, 0, 0]),
                "green" => Some([0, 128, 0]),
                "blue" => Some([0, 0, 255]),
                "yellow" => Some([255, 255, 0]),
                "magenta" => Some([255, 0, 255]),
                "cyan" => Some([0, 255, 255]),
                _ => None,
            };
            if named.is_some() {
                return named;
            }
            if let Some(rest) = b.strip_prefix("color") {
                if let Ok(n) = rest.trim().parse::<usize>()
                    && (1..=56).contains(&n)
                {
                    return Some(crate::style_theme::DEFAULT_INDEXED_PALETTE[8 + (n - 1)]);
                }
            }
        }
    }
    None
}

/// 按顶层分号切段（忽略引号、反斜杠转义和方括号里的内容）
fn split_sections(fmt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = fmt.chars().peekable();
    let mut in_quote = false;
    let mut in_bracket = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '[' if !in_quote => {
                in_bracket = true;
                cur.push(c);
            }
            ']' if !in_quote => {
                in_bracket = false;
                cur.push(c);
            }
            ';' if !in_quote && !in_bracket => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// 文本段：把 @ 换成文本，其余按字面量处理
fn render_text_section(text: &str, sec: &str) -> String {
    let mut out = String::new();
    let mut chars = sec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '@' => out.push_str(text),
            '"' => {
                for c2 in chars.by_ref() {
                    if c2 == '"' {
                        break;
                    }
                    out.push(c2);
                }
            }
            '\\' => {
                if let Some(c2) = chars.next() {
                    out.push(c2);
                }
            }
            '_' => {
                let _ = chars.next();
                out.push(' ');
            }
            '*' => {
                let _ = chars.next();
            }
            '[' => {
                let mut buf = String::new();
                for c2 in chars.by_ref() {
                    if c2 == ']' {
                        break;
                    }
                    buf.push(c2);
                }
                if let Some(rest) = buf.strip_prefix('$') {
                    out.push_str(rest.split('-').next().unwrap_or(""));
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// 去掉 [Red]/[>=100] 这类元数据；[$€-407] 换成货币符号
fn strip_brackets(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' {
            let mut buf = String::new();
            for c2 in chars.by_ref() {
                if c2 == ']' {
                    break;
                }
                buf.push(c2);
            }
            if let Some(rest) = buf.strip_prefix('$') {
                out.push_str(rest.split('-').next().unwrap_or(""));
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn render_section_non_date(
    val: f64,
    sec: &str,
    dbnum: Option<crate::dbnum::DbNumKind>,
) -> String {
    let raw = sec.trim();
    if let Some(kind) = dbnum
        && let Some(s) = crate::dbnum::format_dbnum(val, kind, raw)
    {
        return s;
    }
    render_section_numeric(val, raw)
}

fn render_section_numeric(val: f64, raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("general") {
        return general_number(val);
    }
    let cleaned = strip_brackets(raw);
    let sec = cleaned.trim();
    // 百分比 / 千分比：值先放大，% 号作为字面量保留在原位
    let mut v = val;
    let pct = sec.matches('%').count() as i32;
    let permille = sec.matches('‰').count() as i32;
    v *= 100f64.powi(pct);
    v *= 1000f64.powi(permille);

    // 科学计数法
    if let Some(epos) = find_unquoted_exp(sec) {
        let before = &sec[..epos];
        let after = &sec[epos + 1..];
        if after.starts_with('+') || after.starts_with('-') {
            let (prefix, int_slots, dec_slots, scale_commas) =
                if let Some((ns, ne)) = find_numeric_pattern_span(before) {
                    let pat = &before[ns..ne];
                    let pat_trim = pat.trim_end_matches(',');
                    let mut sc = (pat.len() - pat_trim.len()) as i32;
                    let (raw_i, dec_p) = pat_trim.split_once('.').unwrap_or((pat_trim, ""));
                    let int_trim = raw_i.trim_end_matches(',');
                    sc += (raw_i.len() - int_trim.len()) as i32;
                    let islots = int_trim
                        .chars()
                        .filter(|c| matches!(c, '0' | '#' | '?'))
                        .count()
                        .max(1);
                    let dslots: Vec<char> = dec_p
                        .chars()
                        .filter(|c| matches!(c, '0' | '#' | '?'))
                        .collect();
                    (render_literals(&before[..ns]), islots, dslots, sc)
                } else {
                    (String::new(), 1, vec!['0'; decimals_in(before)], 0)
                };
            let after_sign = &after[1..];
            let mut exp_digits = 0usize;
            let mut exp_end = 1usize;
            for (rel, ch) in after_sign.char_indices() {
                if matches!(ch, '0' | '#' | '?') {
                    exp_digits += 1;
                    exp_end = 1 + rel + ch.len_utf8();
                } else {
                    break;
                }
            }
            let suffix = render_literals(&after[exp_end..]);
            return render_scientific_full(
                v / 1000f64.powi(scale_commas),
                &prefix,
                &suffix,
                int_slots,
                &dec_slots,
                exp_digits.max(1),
                after.starts_with('+'),
            );
        }
    }

    // 分数
    if sec.contains('/') && sec.contains('?') {
        if let Some(f) = render_fraction(v, sec) {
            return f;
        }
    }

    let base_scale = 100f64.powi(pct) * 1000f64.powi(permille);
    if let Some(mask) = MaskSection::from_pattern(sec, base_scale) {
        mask.render(val)
    } else if let Some(pattern) = NumberSection::from_numeric_pattern(sec) {
        pattern.render(val)
    } else {
        let literal = render_literals(sec);
        if literal.is_empty() { general_number(val) } else { literal }
    }
}

fn general_number(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{}", v);
        s
    }
}

fn render_scientific_full(
    val: f64,
    prefix: &str,
    suffix: &str,
    int_slots: usize,
    dec_slots: &[char],
    exp_digits: usize,
    plus: bool,
) -> String {
    use std::fmt::Write;
    let dec = dec_slots.len();
    let neg = val < 0.0;
    let abs_v = val.abs();
    let engineering = int_slots >= 3;
    let (m, exp) = if abs_v == 0.0 || !abs_v.is_finite() {
        (0.0, 0i32)
    } else if engineering {
        let mut exp = (abs_v.log10().floor() as i32).div_euclid(3) * 3;
        let mut m = abs_v / 10f64.powi(exp);
        if dec <= 15 {
            let factor = 10f64.powi(dec as i32);
            let rounded = (m * factor + 1e-12).round() / factor;
            if rounded >= 1000.0 {
                m = rounded / 1000.0;
                exp += 3;
            } else {
                m = rounded;
            }
        }
        (m, exp)
    } else {
        let mut exp = abs_v.log10().floor() as i32;
        let mut m = abs_v / 10f64.powi(exp);
        if m >= 10.0 {
            m /= 10.0;
            exp += 1;
        }
        if m < 1.0 {
            m *= 10.0;
            exp -= 1;
        }
        if dec <= 15 {
            let factor = 10f64.powi(dec as i32);
            let rounded = (m * factor + 1e-12).round() / factor;
            if rounded >= 10.0 {
                m = rounded / 10.0;
                exp += 1;
            } else {
                m = rounded;
            }
        }
        (m, exp)
    };
    let sign = if exp < 0 {
        "-"
    } else if plus {
        "+"
    } else {
        ""
    };
    let raw = format!("{:.*}", dec, m);
    let (int_p, frac_p) = raw.split_once('.').unwrap_or((&raw, ""));
    let mut out = String::with_capacity(prefix.len() + suffix.len() + raw.len() + exp_digits + 6);
    if neg {
        out.push('-');
    }
    out.push_str(prefix);
    out.push_str(int_p);
    if !frac_p.is_empty() {
        append_formatted_frac(&mut out, frac_p, dec_slots);
    }
    let _ = write!(out, "E{}{:0width$}", sign, exp.abs(), width = exp_digits);
    out.push_str(suffix);
    out
}

fn render_fraction(val: f64, sec: &str) -> Option<String> {
    let slash = sec.find('/')?;
    let left_all = &sec[..slash];
    let right_all = &sec[slash + 1..];

    // 解析分子和整数段：在 slash 左侧定位分子占位符，其左若以空格分隔且存在数字占位符则为带分数
    let mut num_start = left_all.len();
    for (idx, ch) in left_all.char_indices().rev() {
        if matches!(ch, '?' | '#' | '0') {
            num_start = idx;
        } else if num_start < left_all.len() {
            break;
        }
    }
    if num_start == left_all.len() {
        return None;
    }
    let num_pat = &left_all[num_start..];
    let before_num = &left_all[..num_start];
    let (prefix_raw, int_pat, mixed) = if let Some(sp) = before_num.rfind(' ') {
        let candidate = &before_num[..sp];
        let mut int_start = candidate.len();
        for (idx, ch) in candidate.char_indices().rev() {
            if matches!(ch, '?' | '#' | '0' | ',') {
                int_start = idx;
            } else if int_start < candidate.len() {
                break;
            }
        }
        if int_start < candidate.len() {
            (&candidate[..int_start], &candidate[int_start..], true)
        } else {
            (before_num, "", false)
        }
    } else {
        (before_num, "", false)
    };

    // 解析分母段和后续字面量
    let mut den_end = 0usize;
    for (idx, ch) in right_all.char_indices() {
        if ch.is_ascii_digit() || matches!(ch, '?' | '#') {
            den_end = idx + ch.len_utf8();
        } else if den_end > 0 {
            break;
        }
    }
    let den_pat = &right_all[..den_end];
    let suffix_raw = &right_all[den_end..];
    let prefix = render_literals(prefix_raw);
    let suffix = render_literals(suffix_raw);

    let a = val.abs();
    let (mut int_part, rest) = (a.trunc(), a.fract());
    let fixed: Option<u32> = {
        let d: String = den_pat.chars().filter(|c| c.is_ascii_digit()).collect();
        (!d.is_empty()).then(|| d.parse::<u32>().ok()).flatten().filter(|&v| v > 0)
    };
    let (mut num, den) = if let Some(d) = fixed {
        ((rest * d as f64).round() as u32, d)
    } else {
        let n = den_pat
            .chars()
            .filter(|c| matches!(c, '?' | '#' | '0'))
            .count()
            .max(1) as u32;
        let max = 10u32.pow(n.min(4)).saturating_sub(1).max(9);
        let (mut best_n, mut best_d) = (0u32, 1u32);
        let mut best = f64::MAX;
        for d in 1..=max {
            let cand = (rest * d as f64).round();
            let err = (rest - cand / d as f64).abs();
            if err < best {
                best = err;
                best_n = cand as u32;
                best_d = d;
            }
            if err < 1e-12 {
                break;
            }
        }
        (best_n, best_d)
    };

    // 进位修正：0.9999 四舍五入为 d/d 时归入整数部分，避免输出 "0 1/1"
    if num >= den {
        int_part += (num / den) as f64;
        num %= den;
    }

    let num_q = num_pat.chars().filter(|c| *c == '?').count();
    let den_q = den_pat.chars().filter(|c| *c == '?').count();
    let int_has_zero = int_pat.contains('0');

    let mut out = String::new();
    if val < 0.0 {
        out.push('-');
    }
    out.push_str(&prefix);
    if mixed {
        if num == 0 {
            out.push_str(&format!("{}", int_part as i64));
        } else {
            let show_int = int_part != 0.0 || int_has_zero;
            if show_int {
                out.push_str(&format!("{} ", int_part as i64));
            }
            let ns = num.to_string();
            let ds = den.to_string();
            for _ in 0..num_q.saturating_sub(ns.len()) {
                out.push(' ');
            }
            out.push_str(&ns);
            out.push('/');
            out.push_str(&ds);
            for _ in 0..den_q.saturating_sub(ds.len()) {
                out.push(' ');
            }
        }
    } else {
        num += (int_part as u32) * den;
        if num == 0 {
            out.push('0');
        } else {
            let ns = num.to_string();
            let ds = den.to_string();
            for _ in 0..num_q.saturating_sub(ns.len()) {
                out.push(' ');
            }
            out.push_str(&ns);
            out.push('/');
            out.push_str(&ds);
            for _ in 0..den_q.saturating_sub(ds.len()) {
                out.push(' ');
            }
        }
    }
    out.push_str(&suffix);
    Some(out)
}

fn render_literals(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                for c2 in chars.by_ref() {
                    if c2 == '"' {
                        break;
                    }
                    out.push(c2);
                }
            }
            '\\' => {
                if let Some(c2) = chars.next() {
                    out.push(c2);
                }
            }
            '_' => {
                let _ = chars.next();
                out.push(' ');
            }
            '*' => {
                let _ = chars.next();
            }
            '[' => {
                let mut buf = String::new();
                for c2 in chars.by_ref() {
                    if c2 == ']' {
                        break;
                    }
                    buf.push(c2);
                }
                if let Some(rest) = buf.strip_prefix('$') {
                    let sym = rest.split('-').next().unwrap_or("");
                    out.push_str(sym);
                }
            }
            '@' => {}
            _ => out.push(c),
        }
    }
    out
}

fn decimals_in(fmt: &str) -> usize {
    let Some(dot) = fmt.find('.') else { return 0 };
    fmt[dot + 1..]
        .chars()
        .take_while(|c| *c == '0' || *c == '#')
        .count()
}

fn has_elapsed_bracket(lower: &str) -> bool {
    let mut rest = lower;
    while let Some(open) = rest.find('[') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find(']') else { break };
        let inner = &rest[..close];
        if !inner.is_empty()
            && (inner.bytes().all(|b| b == b'h')
                || inner.bytes().all(|b| b == b'm')
                || inner.bytes().all(|b| b == b's'))
        {
            return true;
        }
        rest = &rest[close + 1..];
    }
    false
}

pub fn is_date_format(fmt: &str) -> bool {
    let f = strip_literals(fmt).to_lowercase();
    f.contains('y')
        || f.contains('d')
        || f.contains("h:")
        || f.contains(":s")
        || has_elapsed_bracket(&f)
        || f.contains("am/pm")
        || f.contains("a/p")
        || f.contains("上午/下午")
        || f.contains("aaa")
        || (f.contains('m') && !f.contains('0') && !f.contains('#'))
}

pub fn is_percent_format(fmt: &str) -> bool {
    strip_literals(fmt).contains('%')
}

/// 去掉引号/转义里的字面量，只留下格式令牌
fn strip_literals(fmt: &str) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                for c2 in chars.by_ref() {
                    if c2 == '"' {
                        break;
                    }
                }
            }
            '\\' => {
                let _ = chars.next();
            }
            '[' => {
                let mut buf = String::new();
                for c2 in chars.by_ref() {
                    if c2 == ']' {
                        break;
                    }
                    buf.push(c2);
                }
                if buf.starts_with('h')
                    || buf.starts_with('H')
                    || buf.starts_with('m')
                    || buf.starts_with('M')
                    || buf.starts_with('s')
                    || buf.starts_with('S')
                {
                    out.push('[');
                    out.push_str(&buf);
                    out.push(']');
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
enum DateTok {
    Lit(usize, usize),
    Year4,
    Year2,
    Year1,
    MonthFull,
    MonthShort,
    Month2,
    Month1,
    Min2,
    Min1,
    DayFull,
    DayShort,
    CnDayFull,
    CnDayShort,
    Day2,
    Day1,
    Hour2,
    Hour1,
    Sec2,
    Sec1,
    FracSec(u8),
    AmPm,
    ShortAmPm,
    CnAmPm,
    ElapsedHour(u8),
    ElapsedMin(u8),
    ElapsedSec(u8),
}

struct DateSection {
    lits: String,
    tokens: Vec<DateTok>,
    has_ampm: bool,
    needs_time: bool,
    needs_weekday: bool,
    needs_elapsed: bool,
    frac_digits: u8,
    est_len: usize,
}

#[inline]
fn starts_with_ascii_ci(s: &str, pat: &[u8]) -> bool {
    s.len() >= pat.len() && s.as_bytes()[..pat.len()].eq_ignore_ascii_case(pat)
}

#[inline]
fn push_lit_char(lits: &mut String, tokens: &mut Vec<DateTok>, c: char) {
    if let Some(DateTok::Lit(_, end)) = tokens.last_mut()
        && *end == lits.len()
    {
        lits.push(c);
        *end = lits.len();
        return;
    }
    let start = lits.len();
    lits.push(c);
    tokens.push(DateTok::Lit(start, lits.len()));
}

#[inline]
fn push_lit_str(lits: &mut String, tokens: &mut Vec<DateTok>, s: &str) {
    if s.is_empty() {
        return;
    }
    if let Some(DateTok::Lit(_, end)) = tokens.last_mut()
        && *end == lits.len()
    {
        lits.push_str(s);
        *end = lits.len();
        return;
    }
    let start = lits.len();
    lits.push_str(s);
    tokens.push(DateTok::Lit(start, lits.len()));
}

#[inline]
fn push_u32_decimal(out: &mut String, mut v: u32) {
    if v < 10 {
        out.push((b'0' + v as u8) as char);
        return;
    }
    if v < 100 {
        let buf = [b'0' + (v / 10) as u8, b'0' + (v % 10) as u8];
        // SAFETY: ASCII digits only.
        out.push_str(unsafe { std::str::from_utf8_unchecked(&buf) });
        return;
    }
    let mut buf = [0u8; 10];
    let mut pos = buf.len();
    while v > 0 {
        pos -= 1;
        buf[pos] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    // SAFETY: ASCII digits only.
    out.push_str(unsafe { std::str::from_utf8_unchecked(&buf[pos..]) });
}

#[inline]
fn push_i64_decimal(out: &mut String, v: i64) {
    if v >= 0 && v <= u32::MAX as i64 {
        push_u32_decimal(out, v as u32);
        return;
    }
    if v < 0 {
        out.push('-');
    }
    let mut mag = v.unsigned_abs();
    let mut buf = [0u8; 20];
    let mut pos = buf.len();
    if mag == 0 {
        out.push('0');
        return;
    }
    while mag > 0 {
        pos -= 1;
        buf[pos] = b'0' + (mag % 10) as u8;
        mag /= 10;
    }
    // SAFETY: ASCII digits only.
    out.push_str(unsafe { std::str::from_utf8_unchecked(&buf[pos..]) });
}

#[inline]
fn push_2digits(out: &mut String, v: u32) {
    if v < 100 {
        let buf = [b'0' + (v / 10) as u8, b'0' + (v % 10) as u8];
        // SAFETY: ASCII digits only.
        out.push_str(unsafe { std::str::from_utf8_unchecked(&buf) });
    } else {
        push_u32_decimal(out, v);
    }
}

#[inline]
fn push_2digits_signed(out: &mut String, v: i32) {
    if (0..100).contains(&v) {
        push_2digits(out, v as u32);
    } else {
        use std::fmt::Write;
        let _ = write!(out, "{v:02}");
    }
}

#[inline]
fn push_4digits(out: &mut String, v: i32) {
    if (0..=9999).contains(&v) {
        let n = v as u32;
        let buf = [
            b'0' + (n / 1000) as u8,
            b'0' + ((n / 100) % 10) as u8,
            b'0' + ((n / 10) % 10) as u8,
            b'0' + (n % 10) as u8,
        ];
        // SAFETY: ASCII digits only.
        out.push_str(unsafe { std::str::from_utf8_unchecked(&buf) });
    } else {
        use std::fmt::Write;
        let _ = write!(out, "{v:04}");
    }
}

#[inline]
fn push_frac_digits(out: &mut String, frac: u32, digits: u8) {
    out.push('.');
    match digits {
        1 => out.push((b'0' + (frac % 10) as u8) as char),
        2 => push_2digits(out, frac % 100),
        _ => {
            let n = frac % 1000;
            let buf = [
                b'0' + (n / 100) as u8,
                b'0' + ((n / 10) % 10) as u8,
                b'0' + (n % 10) as u8,
            ];
            // SAFETY: ASCII digits only.
            out.push_str(unsafe { std::str::from_utf8_unchecked(&buf) });
        }
    }
}

impl DateSection {
    fn compile(fmt: &str) -> Self {
        let cn_ampm = fmt.contains("上午/下午");
        let mut lits = String::new();
        let mut tokens = Vec::new();
        let mut has_ampm = false;
        let mut needs_time = false;
        let mut needs_weekday = false;
        let mut needs_elapsed = false;
        let mut frac_digits = 0u8;
        let mut after_hour = false;
        let mut after_sec = false;
        let mut i = 0usize;

        while i < fmt.len() {
            let rem = &fmt[i..];
            let Some(c) = rem.chars().next() else {
                break;
            };
            if c == '"' {
                i += 1;
                let start = i;
                if let Some(rel_end) = fmt[i..].find('"') {
                    push_lit_str(&mut lits, &mut tokens, &fmt[start..i + rel_end]);
                    i += rel_end + 1;
                } else {
                    push_lit_str(&mut lits, &mut tokens, &fmt[start..]);
                    i = fmt.len();
                }
                after_hour = false;
                after_sec = false;
                continue;
            }
            if c == '\\' {
                i += 1;
                if let Some(esc) = fmt[i..].chars().next() {
                    push_lit_char(&mut lits, &mut tokens, esc);
                    i += esc.len_utf8();
                }
                after_hour = false;
                after_sec = false;
                continue;
            }
            if c == '_' {
                push_lit_char(&mut lits, &mut tokens, ' ');
                i += 1;
                if let Some(skip) = fmt[i..].chars().next() {
                    i += skip.len_utf8();
                }
                after_hour = false;
                after_sec = false;
                continue;
            }
            if c == '*' {
                i += 1;
                if let Some(skip) = fmt[i..].chars().next() {
                    i += skip.len_utf8();
                }
                after_sec = false;
                continue;
            }
            if c == '[' {
                i += 1;
                let inner = if let Some(rel_end) = fmt[i..].find(']') {
                    let s = &fmt[i..i + rel_end];
                    i += rel_end + 1;
                    s
                } else {
                    let s = &fmt[i..];
                    i = fmt.len();
                    s
                };
                if !inner.is_empty() && inner.bytes().all(|b| b.eq_ignore_ascii_case(&b'h')) {
                    tokens.push(DateTok::ElapsedHour(inner.len().min(8) as u8));
                    needs_elapsed = true;
                    after_hour = true;
                    after_sec = false;
                } else if !inner.is_empty() && inner.bytes().all(|b| b.eq_ignore_ascii_case(&b'm')) {
                    tokens.push(DateTok::ElapsedMin(inner.len().min(8) as u8));
                    needs_elapsed = true;
                    after_hour = false;
                    after_sec = false;
                } else if !inner.is_empty() && inner.bytes().all(|b| b.eq_ignore_ascii_case(&b's')) {
                    tokens.push(DateTok::ElapsedSec(inner.len().min(8) as u8));
                    needs_elapsed = true;
                    after_hour = false;
                    after_sec = true;
                } else {
                    after_hour = false;
                    after_sec = false;
                }
                continue;
            }
            if after_sec && c == '.' {
                let zeros = rem.as_bytes()[1..]
                    .iter()
                    .take_while(|&&b| b == b'0')
                    .count();
                if zeros > 0 {
                    let d = zeros.min(3) as u8;
                    tokens.push(DateTok::FracSec(d));
                    frac_digits = frac_digits.max(d);
                    needs_time = true;
                    if zeros > 3 {
                        for _ in 0..(zeros - 3) {
                            push_lit_char(&mut lits, &mut tokens, '0');
                        }
                    }
                    i += 1 + zeros;
                    after_hour = false;
                    after_sec = false;
                    continue;
                }
            }

            let (tok, advance): (DateTok, usize) = if starts_with_ascii_ci(rem, b"yyyy") {
                (DateTok::Year4, 4)
            } else if starts_with_ascii_ci(rem, b"yy") {
                (DateTok::Year2, 2)
            } else if starts_with_ascii_ci(rem, b"mmmm") {
                (DateTok::MonthFull, 4)
            } else if starts_with_ascii_ci(rem, b"mmm") {
                (DateTok::MonthShort, 3)
            } else if starts_with_ascii_ci(rem, b"dddd") {
                needs_weekday = true;
                (DateTok::DayFull, 4)
            } else if starts_with_ascii_ci(rem, b"ddd") {
                needs_weekday = true;
                (DateTok::DayShort, 3)
            } else if starts_with_ascii_ci(rem, b"aaaa") {
                needs_weekday = true;
                (DateTok::CnDayFull, 4)
            } else if starts_with_ascii_ci(rem, b"aaa") {
                needs_weekday = true;
                (DateTok::CnDayShort, 3)
            } else if starts_with_ascii_ci(rem, b"dd") {
                (DateTok::Day2, 2)
            } else if starts_with_ascii_ci(rem, b"hh") {
                needs_time = true;
                (DateTok::Hour2, 2)
            } else if starts_with_ascii_ci(rem, b"ss") {
                needs_time = true;
                (DateTok::Sec2, 2)
            } else if starts_with_ascii_ci(rem, b"am/pm") {
                has_ampm = true;
                needs_time = true;
                (DateTok::AmPm, 5)
            } else if starts_with_ascii_ci(rem, b"a/p") {
                has_ampm = true;
                needs_time = true;
                (DateTok::ShortAmPm, 3)
            } else if cn_ampm && rem.starts_with("上午/下午") {
                has_ampm = true;
                needs_time = true;
                (DateTok::CnAmPm, "上午/下午".len())
            } else if starts_with_ascii_ci(rem, b"mm") {
                let next = &rem[2..];
                if after_hour || starts_with_ascii_ci(next, b":s") {
                    needs_time = true;
                    (DateTok::Min2, 2)
                } else {
                    (DateTok::Month2, 2)
                }
            } else if starts_with_ascii_ci(rem, b"m") {
                let next = &rem[1..];
                if after_hour || starts_with_ascii_ci(next, b":s") {
                    needs_time = true;
                    (DateTok::Min1, 1)
                } else {
                    (DateTok::Month1, 1)
                }
            } else if starts_with_ascii_ci(rem, b"y") {
                (DateTok::Year1, 1)
            } else if starts_with_ascii_ci(rem, b"d") {
                (DateTok::Day1, 1)
            } else if starts_with_ascii_ci(rem, b"h") {
                needs_time = true;
                (DateTok::Hour1, 1)
            } else if starts_with_ascii_ci(rem, b"s") {
                needs_time = true;
                (DateTok::Sec1, 1)
            } else {
                push_lit_char(&mut lits, &mut tokens, c);
                i += c.len_utf8();
                if c != ':' {
                    after_hour = false;
                }
                after_sec = false;
                continue;
            };

            i += advance;
            after_hour = matches!(tok, DateTok::Hour2 | DateTok::Hour1);
            after_sec = matches!(tok, DateTok::Sec2 | DateTok::Sec1);
            tokens.push(tok);
        }

        let est_len = tokens
            .iter()
            .map(|t| match *t {
                DateTok::Lit(s, e) => e - s,
                DateTok::Year4 | DateTok::Year1 => 4,
                DateTok::MonthFull | DateTok::DayFull | DateTok::CnDayFull => 9,
                DateTok::CnDayShort | DateTok::CnAmPm => 6,
                DateTok::MonthShort | DateTok::DayShort => 3,
                DateTok::FracSec(d) => 1 + d as usize,
                _ => 2,
            })
            .sum();

        Self {
            lits,
            tokens,
            has_ampm,
            needs_time,
            needs_weekday,
            needs_elapsed,
            frac_digits,
            est_len,
        }
    }

    fn render(&self, sys: crate::model::DateSystem, serial: f64) -> Option<String> {
        const MONTH_NAMES: [&str; 12] = [
            "January", "February", "March", "April", "May", "June", "July", "August",
            "September", "October", "November", "December",
        ];
        const DAY_NAMES: [&str; 7] = [
            "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
        ];
        const CN_DAY_FULL: [&str; 7] = [
            "星期日", "星期一", "星期二", "星期三", "星期四", "星期五", "星期六",
        ];
        const CN_DAY_SHORT: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

        // 1900 日期体系不支持负序列号（Excel 显示 ###）
        if !sys.is_1904() && serial < 0.0 {
            return Some("###".to_string());
        }

        let scale: u64 = match self.frac_digits {
            0 => 1,
            1 => 10,
            2 => 100,
            _ => 1000,
        };
        let (adj_serial, hh, mi, ss, frac_val, elapsed_h, elapsed_m, elapsed_s) =
            if self.needs_elapsed {
                let total_ticks = (serial.abs() * 86400.0 * scale as f64).round() as u64;
                let total_secs = total_ticks / scale;
                let frac = (total_ticks % scale) as u32;
                let h = ((total_secs / 3600) % 24) as u32;
                let m = ((total_secs % 3600) / 60) as u32;
                let s = (total_secs % 60) as u32;
                (
                    serial,
                    h,
                    m,
                    s,
                    frac,
                    (total_secs / 3600) as i64,
                    (total_secs / 60) as i64,
                    total_secs as i64,
                )
            } else if self.needs_time {
                let mut ticks = (serial.fract().abs() * 86400.0 * scale as f64).round() as u64;
                let mut carry = 0.0f64;
                if ticks >= 86400 * scale {
                    ticks -= 86400 * scale;
                    carry = if serial < 0.0 { -1.0 } else { 1.0 };
                }
                let total = (ticks / scale) as u32;
                let frac = (ticks % scale) as u32;
                (
                    serial.trunc() + carry,
                    total / 3600,
                    (total % 3600) / 60,
                    total % 60,
                    frac,
                    0,
                    0,
                    0,
                )
            } else {
                (serial, 0, 0, 0, 0, 0, 0, 0)
            };

        let (y, mo, d) = crate::formula::serial_to_date_in(sys, adj_serial)?;
        // 1900 体系：serial 25569 = 1970-01-01（周四）；1904 体系：serial 0 = 1904-01-01（周五）
        let wd = if self.needs_weekday {
            if sys.is_1904() {
                (((adj_serial.abs().floor() as i64) % 7 + 7 + 5) % 7) as usize
            } else {
                (((adj_serial.abs().floor() as i64 - 25569) % 7 + 7 + 4) % 7) as usize
            }
        } else {
            0
        };

        let mut out = String::with_capacity(self.est_len);
        for &tok in &self.tokens {
            match tok {
                DateTok::Lit(s, e) => out.push_str(&self.lits[s..e]),
                DateTok::Year4 => push_4digits(&mut out, y),
                DateTok::Year2 => push_2digits_signed(&mut out, y % 100),
                DateTok::Year1 => push_i64_decimal(&mut out, y as i64),
                DateTok::MonthFull => out.push_str(MONTH_NAMES[(mo as usize).saturating_sub(1).min(11)]),
                DateTok::MonthShort => out.push_str(&MONTH_NAMES[(mo as usize).saturating_sub(1).min(11)][..3]),
                DateTok::Month2 => push_2digits(&mut out, mo),
                DateTok::Month1 => push_u32_decimal(&mut out, mo),
                DateTok::Min2 => push_2digits(&mut out, mi),
                DateTok::Min1 => push_u32_decimal(&mut out, mi),
                DateTok::DayFull => out.push_str(DAY_NAMES[wd]),
                DateTok::DayShort => out.push_str(&DAY_NAMES[wd][..3]),
                DateTok::CnDayFull => out.push_str(CN_DAY_FULL[wd]),
                DateTok::CnDayShort => out.push_str(CN_DAY_SHORT[wd]),
                DateTok::Day2 => push_2digits(&mut out, d),
                DateTok::Day1 => push_u32_decimal(&mut out, d),
                DateTok::Hour2 | DateTok::Hour1 => {
                    let h = if self.has_ampm {
                        if hh % 12 == 0 { 12 } else { hh % 12 }
                    } else {
                        hh
                    };
                    if matches!(tok, DateTok::Hour2) {
                        push_2digits(&mut out, h);
                    } else {
                        push_u32_decimal(&mut out, h);
                    }
                }
                DateTok::Sec2 => push_2digits(&mut out, ss),
                DateTok::Sec1 => push_u32_decimal(&mut out, ss),
                DateTok::FracSec(digits) => {
                    let v = match self.frac_digits.saturating_sub(digits) {
                        0 => frac_val,
                        1 => frac_val / 10,
                        _ => frac_val / 100,
                    };
                    push_frac_digits(&mut out, v, digits);
                }
                DateTok::AmPm => out.push_str(if hh < 12 { "AM" } else { "PM" }),
                DateTok::ShortAmPm => out.push_str(if hh < 12 { "A" } else { "P" }),
                DateTok::CnAmPm => out.push_str(if hh < 12 { "上午" } else { "下午" }),
                DateTok::ElapsedHour(w) => push_padded_i64(&mut out, elapsed_h, w as usize),
                DateTok::ElapsedMin(w) => push_padded_i64(&mut out, elapsed_m, w as usize),
                DateTok::ElapsedSec(w) => push_padded_i64(&mut out, elapsed_s, w as usize),
            }
        }
        Some(out)
    }
}

#[inline]
fn push_padded_i64(out: &mut String, v: i64, min_width: usize) {
    if min_width <= 1 {
        push_i64_decimal(out, v);
        return;
    }
    let start = out.len();
    push_i64_decimal(out, v);
    let written = out.len() - start;
    if written < min_width {
        let pad = min_width - written;
        let zeros = "00000000";
        out.insert_str(start, &zeros[..pad.min(zeros.len())]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Value;

    #[test]
    fn general_matches_excel_width_measurements() {
        // Excel 16.0 实测：宽列（cap 11）与默认列宽 8.43（cap 8）
        let wide: &[(f64, &str)] = &[
            (1.0, "1"),
            (1234567890.0, "1234567890"),
            (12345678901.0, "12345678901"),
            (123456789012.0, "1.23457E+11"),
            (0.1, "0.1"),
            (1234.5678, "1234.5678"),
            (0.000012345, "0.000012345"),
            (1.23456789012, "1.23456789"),
            (-123456789012.0, "-1.23457E+11"),
            (100000000000.0, "1E+11"),
            (99999999999.0, "99999999999"),
        ];
        for (v, want) in wide {
            assert_eq!(&general_display(*v, 11), want, "wide {v}");
        }
        let narrow: &[(f64, &str)] = &[
            (1234567890.0, "1.23E+09"),
            (12345678901.0, "1.23E+10"),
            (123456789012.0, "1.23E+11"),
            (1234.5678, "1234.568"),
            (0.000012345, "1.23E-05"),
            (1.23456789012, "1.234568"),
            (-123456789012.0, "-1.2E+11"),
            (100000000000.0, "1E+11"),
            (99999999999.0, "1E+11"),
        ];
        for (v, want) in narrow {
            assert_eq!(&general_display(*v, 8), want, "narrow {v}");
        }
    }

    #[test]
    fn general_fit_is_ignored_for_explicit_formats() {
        let n = |v: f64| Value::Number(v);
        assert_eq!(format_value_fit(&n(1234.5), "0.00", 8), "1234.50");
        assert_eq!(format_value_fit(&n(123456789012.0), "", 8), "1.23E+11");
        assert_eq!(format_value_fit(&n(123456789012.0), "General", 11), "1.23457E+11");
    }

    #[test]
    fn four_section_and_accounting_formats() {
        let n = |v: f64| Value::Number(v);
        assert_eq!(
            format_value(&n(-1234.5), "#,##0.00;(#,##0.00);\"-\""),
            "(1,234.50)"
        );
        assert_eq!(format_value(&n(0.0), "#,##0.00;(#,##0.00);\"-\""), "-");
        assert_eq!(
            format_value(
                &Value::Text("abc".into()),
                "0.00;\"负\"0.00;0;\"[\"@\"]\""
            ),
            "[abc]"
        );
        let acc = "_-* #,##0.00_-;-* #,##0.00_-;_-* \"-\"??_-;_-@_-";
        assert_eq!(format_value(&n(1234.5), acc), " 1,234.50 ");
        assert_eq!(format_value(&n(-1234.5), acc), "-1,234.50 ");
        assert_eq!(format_value(&n(0.0), acc).trim(), "-");
    }

    #[test]
    fn scientific_fraction_and_optional_decimals() {
        let n = |v: f64| Value::Number(v);
        assert_eq!(format_value(&n(12345.0), "0.00E+00"), "1.23E+04");
        assert_eq!(format_value(&n(0.000123), "0.0E-00"), "1.2E-04");
        assert_eq!(format_value(&n(2.5), "# ?/?"), "2 1/2");
        assert_eq!(format_value(&n(0.75), "?/?"), "3/4");
        assert_eq!(format_value(&n(1.5), "0.##"), "1.5");
        assert_eq!(format_value(&n(1.0), "0.##"), "1");
    }

    #[test]
    fn text_section_and_format_color() {
        assert_eq!(
            format_value(&Value::Text("ABC".into()), "\"[\"@\"]\""),
            "[ABC]"
        );
        assert_eq!(
            format_color(&Value::Number(-1.0), "#,##0;[Red]-#,##0"),
            Some([255, 0, 0])
        );
        assert_eq!(
            format_color(&Value::Number(1.0), "#,##0;[Red]-#,##0"),
            None
        );
    }

    #[test]
    fn date_time_and_currency_placements() {
        let n = |v: f64| Value::Number(v);
        assert_eq!(format_value(&n(25569.0), "yyyy-mm-dd"), "1970-01-01");
        assert_eq!(format_value(&n(25569.0), "m/d/yyyy"), "1/1/1970");
        assert_eq!(format_value(&n(25569.0), "dddd"), "Thursday");
        assert_eq!(format_value(&n(1.5), "[h]:mm:ss"), "36:00:00");
        assert_eq!(format_value(&n(0.5), "h:mm AM/PM"), "12:00 PM");
        assert_eq!(format_value(&n(0.5), "上午/下午 h:mm"), "下午 12:00");
        assert_eq!(format_value(&n(1234.5), "#,##0.00 元"), "1,234.50 元");
        assert_eq!(format_value(&n(1234.5), "[$€-407]#,##0.00"), "€1,234.50");
    }

    #[test]
    fn compiled_date_tokens_and_large_format_cache() {
        let n = |v: f64| Value::Number(v);
        // 1970-01-01 13:05:09.256 (Thursday)
        let s = 25569.0 + (13.0 * 3600.0 + 5.0 * 60.0 + 9.256) / 86400.0;
        assert_eq!(format_value(&n(s), "yyyy-mm-dd hh:mm:ss"), "1970-01-01 13:05:09");
        assert_eq!(format_value(&n(s), "yy/m/d h:m:s a/p"), "70/1/1 1:5:9 P");
        assert_eq!(format_value(&n(s), "mmmm mmm dddd ddd"), "January Jan Thursday Thu");
        assert_eq!(format_value(&n(s), "yyyy-mm-dd aaaa aaa"), "1970-01-01 星期四 周四");
        assert_eq!(format_value(&n(s), "hh:mm:ss.000"), "13:05:09.256");
        assert_eq!(format_value(&n(s), "hh:mm:ss.00"), "13:05:09.26");
        assert_eq!(format_value(&n(s), "hh:mm:ss.0"), "13:05:09.3");
        assert_eq!(format_value(&n(s), "mm:ss.000"), "05:09.256");
        assert_eq!(format_value(&n(1.5), "[h]:mm"), "36:00");
        assert_eq!(format_value(&n(1.5), "[m]:ss"), "2160:00");
        assert_eq!(format_value(&n(1.5), "[s]"), "129600");
        assert_eq!(
            format_value(&n(25569.0), "yyyy\"年\"mm\"月\"dd\"日\"_0\\!"),
            "1970年01月01日 !"
        );
        // 1904 date system: serial 0 = 1904-01-01 (Friday)
        assert_eq!(
            format_value_in(crate::model::DateSystem::D1904, &n(0.0), "yyyy-mm-dd dddd aaaa"),
            "1904-01-01 Friday 星期五"
        );
        // >128 distinct formats must remain accurate across two-generation cache rotation.
        for i in 0..600usize {
            let fmt = format!("yyyy-mm-dd\" #{i}\"");
            assert_eq!(format_value(&n(25569.0), &fmt), format!("1970-01-01 #{i}"));
        }
    }

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

    #[test]
    fn font_name_normalized_both_ways() {
        assert_eq!(normalize_font("SimSun"), "宋体");
        assert_eq!(normalize_font(" 宋体 "), "宋体");
        assert_eq!(normalize_font("SimHei"), "黑体");
        assert_eq!(normalize_font("Microsoft YaHei"), "微软雅黑");
        assert_eq!(normalize_font("DengXian"), "等线");
        assert_eq!(normalize_font("Noto Sans SC"), "思源黑体");
        assert_eq!(normalize_font("Noto Serif SC"), "思源宋体");
        assert_eq!(normalize_font("inter"), "Inter");
        assert_eq!(normalize_font("JetBrains Mono"), "JetBrains Mono");
        // 认不出来的原样保留，空串保持空
        assert_eq!(normalize_font("Calibri"), "Calibri");
        assert_eq!(normalize_font(""), "");
    }

    #[test]
    fn font_family_mapping() {
        assert_eq!(font_class(""), FontClass::Default);
        assert_eq!(font_class("宋体"), FontClass::Serif);
        assert_eq!(font_class("SimSun"), FontClass::Serif);
        assert_eq!(font_class("黑体"), FontClass::Sans);
        assert_eq!(font_class("微软雅黑"), FontClass::Sans);
        assert_eq!(font_class("等线"), FontClass::Sans);
        assert_eq!(font_class("Inter"), FontClass::Latin);
        assert_eq!(font_class("JetBrains Mono"), FontClass::Mono);
        // 未知字体回退默认
        assert_eq!(font_class("Comic Sans MS"), FontClass::Default);
    }

    #[test]
    fn styles_dedup_separates_fonts() {
        let mut t = StyleTable::new();
        let plain = CellStyle { size: 11.0, ..Default::default() };
        let song = CellStyle { size: 11.0, font: "宋体".into(), ..Default::default() };
        let hei = CellStyle { size: 11.0, font: "黑体".into(), ..Default::default() };
        let a = t.intern(plain.clone());
        assert_ne!(t.intern(song.clone()), a, "换了字体必须是另一个样式");
        assert_ne!(t.intern(hei), a);
        assert_eq!(t.intern(song), t.intern(CellStyle { font: "宋体".into(), size: 11.0, ..Default::default() }));
    }

    #[test]
    fn wave5_stack_number_formatting_matches_excel_general_and_zero_trimming() {
        assert_eq!(general_display(0.0, 11), "0");
        assert_eq!(general_display(-0.0, 11), "0");
        assert_eq!(general_display(99_999_999_999.0, 11), "99999999999");
        assert_eq!(general_display(100_000_000_000.0, 11), "1E+11");
        assert_eq!(general_display(-12345.0, 11), "-12345");
        assert_eq!(general_display(9.9999999999e11, 11), "1E+12");
        assert_eq!(crate::model::format_number(-0.0), "0");
        assert_eq!(crate::model::format_number(123.45000), "123.45");
        assert_eq!(crate::model::format_number(f64::NAN), "#NUM!");
    }
}
