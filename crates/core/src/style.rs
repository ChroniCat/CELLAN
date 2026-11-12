//! 单元格样式：把 xlsx 的样式表压成一个小结构，供渲染与数字格式化使用。
//!
//! 设计取舍：不做完整的 OOXML 样式模型（字体家族、边框、图案填充……），
//! 只保留「看得见且常用」的部分：粗斜体、字号、字色、底色、对齐、自动换行、数字格式。
//! 每格只存一个 u16 索引，相同样式去重后共享一份。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 一条边：0 无，1 细，2 中，3 粗，4 双线，5 虚线，6 点线
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    /// 单下划线（Excel 的 U）
    #[serde(default)]
    pub underline: bool,
    /// 删除线
    #[serde(default)]
    pub strikethrough: bool,
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
    /// 0 常规 1 左 2 中 3 右
    pub halign: u8,
    /// 0 常规 1 上 2 中 3 下
    pub valign: u8,
    pub wrap: bool,
    /// 缩进级数（Excel 的 IndentLevel，1 级约 3 个字符宽）
    #[serde(default)]
    pub indent: u8,
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
    s.underline.hash(&mut h);
    s.strikethrough.hash(&mut h);
    s.rotation.hash(&mut h);
    s.font.hash(&mut h);
    s.size.to_bits().hash(&mut h);
    s.color.hash(&mut h);
    s.fill.hash(&mut h);
    s.halign.hash(&mut h);
    s.valign.hash(&mut h);
    s.wrap.hash(&mut h);
    s.indent.hash(&mut h);
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

/// 把值按 Excel 格式代码显示（支持最多四段：正;负;零;文本）
/// 按指定工作簿的日期体系格式化（1904 的序列跟 1900 差 1462 天）
pub fn format_value_in(
    sys: crate::model::DateSystem,
    v: &crate::model::Value,
    fmt: &str,
) -> String {
    crate::formula::with_date_system(sys, || format_value(v, fmt))
}

pub fn format_value(v: &crate::model::Value, fmt: &str) -> String {
    use crate::model::Value;
    if fmt.trim().is_empty() || fmt.eq_ignore_ascii_case("general") {
        return v.display();
    }
    // Excel 的内建 14 号格式是「系统区域短日期」，不同机器显示不同；
    // 读盘时它被还原成 mm-dd-yy，这里按系统设置换成本机的写法（和 Excel 一致）。
    let localized = localize_builtin_date(fmt);
    let fmt: &str = localized.as_deref().unwrap_or(fmt);
    let sections = split_sections(fmt);
    match v {
        Value::Number(n) => {
            let (sec, val, need_minus) = if *n < 0.0 {
                if sections.len() >= 2 {
                    (sections[1].as_str(), -*n, false)
                } else {
                    (sections[0].as_str(), -*n, true)
                }
            } else if *n == 0.0 && sections.len() >= 3 {
                (sections[2].as_str(), 0.0, false)
            } else {
                (sections[0].as_str(), *n, false)
            };
            let mut out = render_section(val, sec);
            if need_minus && !out.trim_start().starts_with('-') {
                out.insert(0, '-');
            }
            out
        }
        Value::Text(t) => {
            // 文本优先用第 4 段；没有第 4 段时，第 1 段里有 @ 也照样套
            if sections.len() >= 4 {
                render_text_section(t, &sections[3])
            } else if sections[0].contains('@') {
                render_text_section(t, &sections[0])
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
fn localize_builtin_date(fmt: &str) -> Option<String> {
    let t = fmt.trim();
    let is_short_date = t.eq_ignore_ascii_case("mm-dd-yy")
        || t.eq_ignore_ascii_case("m/d/yy")
        || t.eq_ignore_ascii_case("m-d-yy")
        || t.eq_ignore_ascii_case("mm-dd-yyyy");
    if !is_short_date {
        return None;
    }
    Some(system_short_date())
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

/// 格式代码里指定的文字颜色（[Red] / [Color3] 这类）；没有就返回 None
pub fn format_color(v: &crate::model::Value, fmt: &str) -> Option<[u8; 3]> {
    use crate::model::Value;
    if fmt.trim().is_empty() {
        return None;
    }
    let sections = split_sections(fmt);
    let sec = match v {
        Value::Number(n) if *n < 0.0 && sections.len() >= 2 => &sections[1],
        Value::Number(n) if *n == 0.0 && sections.len() >= 3 => &sections[2],
        Value::Text(_) if sections.len() >= 4 => &sections[3],
        _ => &sections[0],
    };
    let palette = [
        [0, 0, 0],
        [255, 255, 255],
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [255, 255, 0],
        [255, 0, 255],
        [0, 255, 255],
    ];
    let mut chars = sec.chars().peekable();
    while let Some(c) = chars.next() {
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
                if let Ok(n) = rest.trim().parse::<usize>() {
                    return Some(palette.get(n.saturating_sub(1)).copied().unwrap_or([0, 0, 0]));
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

fn render_section(val: f64, sec: &str) -> String {
    let raw = sec.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("general") {
        return general_number(val);
    }
    // 日期判断要用原始格式（[h] 这类耗时令牌不能被清掉）
    if is_date_format(raw) {
        if let Some(s) = format_date(val, raw) {
            return s;
        }
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
    if let Some(epos) = sec.find(['E', 'e']) {
        let before = &sec[..epos];
        let after = &sec[epos + 1..];
        if after.starts_with('+') || after.starts_with('-') {
            let dec = decimals_in(before);
            let exp_digits = after.chars().filter(|c| *c == '0').count().max(1);
            return render_scientific(v, dec, exp_digits, after.starts_with('+'));
        }
    }

    // 分数
    if sec.contains('/') && sec.contains('?') {
        if let Some(f) = render_fraction(v, sec) {
            return f;
        }
    }

    // 找到数字占位段（0 # ? , .），前后当字面量
    let chars: Vec<char> = sec.chars().collect();
    let is_ph = |c: char| matches!(c, '0' | '#' | '?' | ',' | '.');
    let start = chars.iter().position(|c| matches!(c, '0' | '#' | '?'));
    let Some(start) = start else {
        // 没有数字占位：整段是字面量（会计零值段就是 "-"）
        let lit = render_literals(sec);
        if lit.is_empty() {
            return general_number(val);
        }
        return lit;
    };
    let mut end = start;
    while end < chars.len() && is_ph(chars[end]) {
        end += 1;
    }
    let prefix: String = chars[..start].iter().collect();
    let pattern: String = chars[start..end].iter().collect();
    let suffix: String = chars[end..].iter().collect();
    let body = render_number(v, &pattern);
    format!(
        "{}{}{}",
        render_literals(&prefix),
        body,
        render_literals(&suffix)
    )
}

fn general_number(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{}", v);
        s
    }
}

fn render_number(val: f64, pattern: &str) -> String {
    let plus = val >= 0.0;
    let a = val.abs();
    let dec_part = pattern.split_once('.').map(|(_, f)| f).unwrap_or("");
    let dec = dec_part.chars().filter(|c| matches!(c, '0' | '#')).count();
    let int_part = pattern.split('.').next().unwrap_or("");
    let thousands = int_part.contains(',');
    // 只有 ? / # 的零值段：Excel 用空格占位（会计格式的零就是 "-" 后面跟空格）
    if a == 0.0 && !pattern.contains('0') {
        let n = pattern.chars().filter(|c| matches!(c, '0' | '#' | '?')).count();
        return " ".repeat(n);
    }
    let mut s = if thousands {
        group_thousands(a, dec)
    } else {
        format!("{:.*}", dec, a)
    };
    // 小数位全是 # 时，去掉末尾 0 和小数点
    if !dec_part.is_empty() && dec_part.chars().all(|c| c == '#') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if !plus && !thousands {
        // 单段格式的负号由调用方补，这里只处理数字本体
    }
    s
}

fn render_scientific(val: f64, dec: usize, exp_digits: usize, plus: bool) -> String {
    if val == 0.0 {
        return format!("{:.*}E{}{:0width$}", dec, 0.0, if plus { "+" } else { "-" }, 0, width = exp_digits);
    }
    let mut exp = val.abs().log10().floor() as i32;
    let mut m = val / 10f64.powi(exp);
    if m >= 10.0 {
        m /= 10.0;
        exp += 1;
    }
    if m < 1.0 {
        m *= 10.0;
        exp -= 1;
    }
    let sign = if exp < 0 {
        "-"
    } else if plus {
        "+"
    } else {
        ""
    };
    format!(
        "{:.*}E{}{:0width$}",
        dec,
        m,
        sign,
        exp.abs(),
        width = exp_digits
    )
}

fn render_fraction(val: f64, sec: &str) -> Option<String> {
    let a = val.abs();
    let (int_part, rest) = (a.trunc(), a.fract());
    let den_pat = sec.split('/').nth(1)?;
    let fixed: Option<u32> = den_pat
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok();
    let max = fixed.unwrap_or_else(|| {
        let n = den_pat.chars().filter(|c| *c == '?' || *c == '#').count().max(1) as u32;
        10u32.pow(n.min(4)).saturating_sub(1).max(9)
    });
    let (mut num, mut den) = (0u32, 1u32);
    let mut best = f64::MAX;
    for d in 1..=max {
        let n = (rest * d as f64).round();
        let err = (rest - n / d as f64).abs();
        if err < best {
            best = err;
            num = n as u32;
            den = d;
        }
        if err < 1e-12 {
            break;
        }
    }
    let mixed = sec[..sec.find('/').unwrap_or(0)]
        .chars()
        .any(|c| c == '#' || c == '0' || c == '?')
        && sec.contains(' ');
    let mut out = String::new();
    if mixed {
        out.push_str(&format!("{}", int_part as i64));
        out.push(' ');
    } else {
        num += (int_part as u32) * den;
    }
    if num == 0 {
        out.push('0');
    } else {
        out.push_str(&format!("{num}/{den}"));
    }
    if val < 0.0 {
        out.insert(0, '-');
    }
    Some(out.trim_end().to_string())
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
    let f = strip_literals(fmt).to_lowercase();
    f.contains('y')
        || f.contains('d')
        || f.contains("h:")
        || f.contains("[h]")
        || f.contains("am/pm")
        || f.contains("上午/下午")
        || (f.contains('m') && !f.contains('0') && !f.contains('#'))
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
                if buf.starts_with('h') || buf.starts_with('m') || buf.starts_with('s') {
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

fn format_date(serial: f64, fmt: &str) -> Option<String> {
    format_date_in(crate::formula::current_date_system(), serial, fmt)
}

fn format_date_in(sys: crate::model::DateSystem, serial: f64, fmt: &str) -> Option<String> {
    let (y, mo, d) = crate::formula::serial_to_date_in(sys, serial)?;
    let total = (serial.fract().abs() * 86400.0).round() as u32;
    let (hh, mi, ss) = (total / 3600, (total % 3600) / 60, total % 60);
    let elapsed = serial.abs() * 86400.0;
    let month_names = [
        "January", "February", "March", "April", "May", "June", "July", "August",
        "September", "October", "November", "December",
    ];
    let day_names = [
        "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
    ];
    // 1900 体系：serial 25569 = 1970-01-01（周四）；1904 体系：serial 0 = 1904-01-01（周五）
    let wd = if sys.is_1904() {
        (((serial.abs().floor() as i64) % 7 + 7 + 5) % 7) as usize
    } else {
        (((serial.abs().floor() as i64 - 25569) % 7 + 7 + 4) % 7) as usize
    };
    let lower_fmt = fmt.to_lowercase();
    let cn_ampm = fmt.contains("上午/下午");
    let has_ampm = lower_fmt.contains("am/pm") || lower_fmt.contains("a/p") || cn_ampm;
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    let mut after_hour = false;
    while i < chars.len() {
        let c = chars[i];
        let rem: String = chars[i..].iter().collect();
        let low = rem.to_lowercase();
        if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            after_hour = false;
            continue;
        }
        if c == '\\' {
            if i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
            } else {
                i += 1;
            }
            after_hour = false;
            continue;
        }
        if c == '_' {
            out.push(' ');
            i = (i + 2).min(chars.len());
            after_hour = false;
            continue;
        }
        if c == '*' {
            i = (i + 2).min(chars.len());
            continue;
        }
        if c == '[' {
            let mut buf = String::new();
            i += 1;
            while i < chars.len() && chars[i] != ']' {
                buf.push(chars[i]);
                i += 1;
            }
            i += 1;
            match buf.to_lowercase().as_str() {
                "h" => out.push_str(&format!("{}", (elapsed / 3600.0).floor() as i64)),
                "m" => out.push_str(&format!("{}", (elapsed / 60.0).floor() as i64)),
                "s" => out.push_str(&format!("{}", elapsed.floor() as i64)),
                _ => {}
            }
            after_hour = false;
            continue;
        }
        let (tok, len) = if low.starts_with("yyyy") {
            ("yyyy", 4)
        } else if low.starts_with("yy") {
            ("yy", 2)
        } else if low.starts_with("mmmm") {
            ("mmmm", 4)
        } else if low.starts_with("mmm") {
            ("mmm", 3)
        } else if low.starts_with("dddd") {
            ("dddd", 4)
        } else if low.starts_with("ddd") {
            ("ddd", 3)
        } else if low.starts_with("dd") {
            ("dd", 2)
        } else if low.starts_with("hh") {
            ("hh", 2)
        } else if low.starts_with("ss") {
            ("ss", 2)
        } else if low.starts_with("am/pm") {
            ("am/pm", 5)
        } else if cn_ampm && rem.starts_with("上午/下午") {
            ("cnampm", 5)
        } else if low.starts_with("mm") {
            let next = low.get(2..).unwrap_or("");
            if after_hour || next.starts_with(":s") {
                ("mi2", 2)
            } else {
                ("mo2", 2)
            }
        } else if low.starts_with('m') {
            if after_hour {
                ("mi", 1)
            } else {
                ("mo", 1)
            }
        } else if low.starts_with('y') {
            ("y", 1)
        } else if low.starts_with('d') {
            ("d", 1)
        } else if low.starts_with('h') {
            ("h", 1)
        } else if low.starts_with('s') {
            ("s", 1)
        } else {
            out.push(c);
            i += 1;
            if c != ':' {
                after_hour = false;
            }
            continue;
        };
        i += len;
        match tok {
            "yyyy" => out.push_str(&format!("{y:04}")),
            "yy" => out.push_str(&format!("{:02}", y % 100)),
            "y" => out.push_str(&format!("{y}")),
            "mmmm" => out.push_str(month_names[(mo as usize).saturating_sub(1).min(11)]),
            "mmm" => out.push_str(&month_names[(mo as usize).saturating_sub(1).min(11)][..3]),
            "mo2" => out.push_str(&format!("{mo:02}")),
            "mo" => out.push_str(&format!("{mo}")),
            "mi2" => out.push_str(&format!("{mi:02}")),
            "mi" => out.push_str(&format!("{mi}")),
            "dddd" => out.push_str(day_names[wd]),
            "ddd" => out.push_str(&day_names[wd][..3]),
            "dd" => out.push_str(&format!("{d:02}")),
            "d" => out.push_str(&format!("{d}")),
            "hh" | "h" => {
                let h = if has_ampm {
                    if hh % 12 == 0 {
                        12
                    } else {
                        hh % 12
                    }
                } else {
                    hh
                };
                if tok == "hh" {
                    out.push_str(&format!("{h:02}"));
                } else {
                    out.push_str(&format!("{h}"));
                }
                after_hour = true;
                continue;
            }
            "ss" => out.push_str(&format!("{ss:02}")),
            "s" => out.push_str(&format!("{ss}")),
            "am/pm" => out.push_str(if hh < 12 { "AM" } else { "PM" }),
            "cnampm" => out.push_str(if hh < 12 { "上午" } else { "下午" }),
            _ => {}
        }
        after_hour = false;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Value;

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
}
