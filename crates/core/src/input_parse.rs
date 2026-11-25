//! 用户单元格输入解析与编辑栏格式化（对齐 Excel / WPS 规则）。
//!
//! 负责：
//! - 文本格式（`@`）列保持原样为文本，避免长账号/身份证号丢失精度；
//! - 前导单引号 `'` 强制按文本存储并记录 `quote_prefix`；
//! - 自动识别日期、时间、日期时间、百分比、千分位数值、货币金额，并给出建议数字格式；
//! - 进入编辑态 / 编辑栏时，将日期、百分比、前导撇号格格式化为人类可读的可编辑文本。

use crate::formula::functions::{date_to_serial_in, serial_to_date_in};
use crate::model::{intern_short_str, with_formatted_number, DateSystem, Value};
use crate::style::{is_date_format, is_percent_format};

/// 单元格手动输入的解析结果。
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedInput {
    pub value: Value,
    /// 若用户输入触发了自动格式推断（日期/时间/百分比/千分位/货币），给出建议格式串。
    /// 调用方通常仅在单元格当前为常规（空或 `General`）时套用。
    pub auto_format: Option<String>,
    /// 用户是否通过前导单引号 `'` 强制文本输入（对应 OOXML `quotePrefix="1"`）。
    pub quote_prefix: bool,
}

impl ParsedInput {
    #[inline]
    fn plain(value: Value) -> Self {
        Self {
            value,
            auto_format: None,
            quote_prefix: false,
        }
    }
}

/// 按单元格当前格式与工作簿日期体系解析用户输入（使用本地当前年份补全月/日简写）。
pub fn parse_cell_input(raw: &str, existing_format: &str, sys: DateSystem) -> ParsedInput {
    parse_cell_input_with_year(raw, existing_format, sys, current_local_year())
}

/// 同 `parse_cell_input`，显式指定月/日简写（如 `11/25`、`11月25日`）所使用的基准年份。
pub fn parse_cell_input_with_year(
    raw: &str,
    existing_format: &str,
    sys: DateSystem,
    default_year: i32,
) -> ParsedInput {
    let is_text_fmt = existing_format.trim() == "@";
    if is_text_fmt {
        if raw.is_empty() {
            return ParsedInput::plain(Value::Empty);
        }
        return ParsedInput::plain(Value::Text(intern_short_str(raw)));
    }

    if let Some(rest) = raw.strip_prefix('\'') {
        return ParsedInput {
            value: Value::Text(intern_short_str(rest)),
            auto_format: None,
            quote_prefix: true,
        };
    }

    let t = raw.trim();
    if t.is_empty() {
        return ParsedInput::plain(Value::Empty);
    }

    // 1) 布尔值
    if t.len() <= 5 {
        if t.eq_ignore_ascii_case("TRUE") {
            return ParsedInput::plain(Value::Bool(true));
        }
        if t.eq_ignore_ascii_case("FALSE") {
            return ParsedInput::plain(Value::Bool(false));
        }
    }

    // 2) 纯数字（排除 inf / nan 等 Rust f64 特殊字面量）
    if let Some(n) = parse_strict_plain_f64(t) {
        if is_percent_format(existing_format) {
            return ParsedInput::plain(Value::Number(n / 100.0));
        }
        return ParsedInput::plain(Value::Number(n));
    }

    // 3) 会计括号负数（如 (1,000)、(1,000.50)、(¥1,000)、($500.25)、¥(1,000)、(15%)）
    if let Some(parsed) = parse_paren_negative(t) {
        return parsed;
    }

    // 4) 百分比（如 15%、-12.5%、1,234.5%）
    if let Some(parsed) = parse_percent(t) {
        return parsed;
    }

    // 5) 货币金额（如 ¥1,000、￥1,234.56、$1,000.50、€50、£99.9、-$500、¥-100）
    if let Some(parsed) = parse_currency(t) {
        return parsed;
    }

    // 6) 千分位数值（如 1,234、-1,234,567.89）
    if let Some(parsed) = parse_grouped_number(t) {
        return parsed;
    }

    // 7) 带分数（如 1 1/2、0 3/4、-2 3/8；遵循 Excel 规则，避免裸 3/4 与 3月4日 冲突）
    if let Some(parsed) = parse_mixed_fraction(t) {
        return parsed;
    }

    // 8) 日期 / 日期时间 / 时间（含 11/25、11月25日、2026-11、9:5、25:00 累计时间）
    if let Some(parsed) = parse_date_or_time(t, sys, default_year) {
        return parsed;
    }

    ParsedInput::plain(Value::Text(intern_short_str(raw)))
}

fn current_local_year() -> i32 {
    // 复用标准时间（本地近似 UTC+8 或系统 UTC 纪元换算）获取当前公历年
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // civil_from_days (Howard Hinnant algorithm)
    let z = secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + if m <= 2 { 1 } else { 0 }) as i32
}

/// 解析会计括号负数：`(1,000.50)`、`(¥500)`、`¥(500)`、`($1,200.00)`、`(15%)`。
fn parse_paren_negative(t: &str) -> Option<ParsedInput> {
    // 支持货币符在括号外：¥(1,000.50) / $(500)
    let mut outer_sym: Option<&'static str> = None;
    let body = if let Some(inner) = t.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        inner.trim()
    } else {
        let mut chars = t.chars();
        let first = chars.next()?;
        let sym = currency_symbol(first)?;
        let rest = chars.as_str().trim();
        let inner = rest.strip_prefix('(')?.strip_suffix(')')?.trim();
        outer_sym = Some(sym);
        inner
    };
    if body.is_empty() || body.starts_with('-') || body.starts_with('+') {
        return None;
    }
    // 括号内百分比：(15%)
    if outer_sym.is_none() {
        if let Some(pct_body) = body.strip_suffix('%') {
            let (val, _, decimals) = parse_decimal_with_optional_commas(pct_body.trim())?;
            let dec = format_decimals_suffix(decimals, 4);
            let fmt = format!("0{dec}%;(0{dec}%)");
            return Some(ParsedInput {
                value: Value::Number(-val / 100.0),
                auto_format: Some(fmt),
                quote_prefix: false,
            });
        }
    }
    // 括号内或括号外带货币符：(¥1,000.50) / ¥(1,000.50)
    let (sym, num_part) = if let Some(s) = outer_sym {
        (Some(s), body)
    } else {
        let mut chars = body.chars();
        if let Some(ch) = chars.next() {
            if let Some(s) = currency_symbol(ch) {
                (Some(s), chars.as_str().trim())
            } else {
                (None, body)
            }
        } else {
            return None;
        }
    };
    if num_part.is_empty() || num_part.starts_with('-') || num_part.starts_with('+') {
        return None;
    }
    let (val, has_comma, decimals) = parse_decimal_with_optional_commas(num_part)?;
    let dec = format_decimals_suffix(decimals, 4);
    let fmt = if let Some(s) = sym {
        format!("{s}#,##0{dec};({s}#,##0{dec})")
    } else if has_comma {
        format!("#,##0{dec};(#,##0{dec})")
    } else {
        format!("0{dec};(0{dec})")
    };
    Some(ParsedInput {
        value: Value::Number(-val),
        auto_format: Some(fmt),
        quote_prefix: false,
    })
}

/// 解析带分数：`1 1/2`、`0 3/4`、`-2 3/8`（Excel 标准输入格式）。
fn parse_mixed_fraction(t: &str) -> Option<ParsedInput> {
    let (neg, rest) = if let Some(r) = t.strip_prefix('-') {
        (true, r.trim_start())
    } else if let Some(r) = t.strip_prefix('+') {
        (false, r.trim_start())
    } else {
        (false, t)
    };
    let (whole_s, frac_s) = rest.split_once(' ')?;
    let frac_s = frac_s.trim();
    if whole_s.is_empty()
        || !whole_s.bytes().all(|b| b.is_ascii_digit())
        || whole_s.len() > 12
    {
        return None;
    }
    let (num_s, den_s) = frac_s.split_once('/')?;
    if num_s.is_empty()
        || den_s.is_empty()
        || num_s.len() > 6
        || den_s.len() > 6
        || !num_s.bytes().all(|b| b.is_ascii_digit())
        || !den_s.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let whole: u64 = whole_s.parse().ok()?;
    let num: u64 = num_s.parse().ok()?;
    let den: u64 = den_s.parse().ok()?;
    if den == 0 {
        return None;
    }
    let abs_val = whole as f64 + (num as f64 / den as f64);
    let val = if neg { -abs_val } else { abs_val };
    if !val.is_finite() {
        return None;
    }
    let fmt = if den >= 100 {
        "# ???/???"
    } else if den >= 10 {
        "# ??/??"
    } else {
        "# ?/?"
    };
    Some(ParsedInput {
        value: Value::Number(val),
        auto_format: Some(fmt.to_string()),
        quote_prefix: false,
    })
}

/// 规范化公式输入：
/// - `=` 开头的公式自动补全缺失的尾部右括号 `)`（跳过双引号字符串字面量）；
/// - `+` / `-` 开头且主体为单元格引用、函数调用或算术表达式（而非纯有符号数字/货币/百分比/千分位/分数）时，
///   按 Excel / Lotus 习惯转为标准 `=` 公式并补全缺失右括号。
pub fn normalize_formula_input(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if t.starts_with('=') {
        return Some(auto_close_parens(t));
    }
    let first = t.as_bytes()[0];
    if first != b'+' && first != b'-' {
        return None;
    }
    // 先排除合法的纯数值/百分比/货币/千分位/带分数输入
    if parse_strict_plain_f64(t).is_some()
        || parse_percent(t).is_some()
        || parse_currency(t).is_some()
        || parse_grouped_number(t).is_some()
        || parse_mixed_fraction(t).is_some()
    {
        return None;
    }
    let body = t[1..].trim_start();
    if body.is_empty() || !looks_like_signed_formula_body(body) {
        return None;
    }
    let formula = if first == b'+' {
        format!("={body}")
    } else {
        format!("=-{body}")
    };
    Some(auto_close_parens(&formula))
}

fn looks_like_signed_formula_body(body: &str) -> bool {
    let bytes = body.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    // 1) 以 `(` 开头，如 `+(A1+B1)` 或 `-(1+2)*3`
    if bytes[0] == b'(' {
        return true;
    }
    // 2) 以函数调用开头，如 `+SUM(A1:A5)` / `-AVERAGE(B1:B3)`
    if bytes[0].is_ascii_alphabetic() || bytes[0] == b'_' {
        let mut i = 0usize;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
        {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'(' {
            return true;
        }
    }
    // 3) 以 A1 引用或 `Sheet!A1` / `'Sheet 1'!A1` 开头
    if starts_with_ref_or_sheet_ref(body) {
        return true;
    }
    // 4) 纯数字算术表达式（含运算符 `+`/`-`/`*`/`/`/`^`，且至少两个操作数，排除日期或电话串以外的普通文字）
    if bytes[0].is_ascii_digit() || bytes[0] == b'.' {
        let mut has_op = false;
        let mut ok = true;
        for &b in bytes {
            match b {
                b'0'..=b'9' | b'.' | b' ' | b'(' | b')' => {}
                b'+' | b'-' | b'*' | b'/' | b'^' => has_op = true,
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        // 避免把 `-2026-11-25` 或 `+86-138-0013` 错判成公式：仅当含 `*`/`/`/`^`/`+` 或仅单一 `-` 且不含 2+ 个连字符时才视作算术式
        let dash_count = bytes.iter().filter(|&&b| b == b'-').count();
        if ok && has_op && (dash_count <= 1 || bytes.iter().any(|&b| matches!(b, b'+' | b'*' | b'/' | b'^'))) {
            return true;
        }
    }
    false
}

fn starts_with_ref_or_sheet_ref(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut pos = 0usize;
    if bytes.first() == Some(&b'\'') {
        let mut k = 1usize;
        while k < bytes.len() {
            if bytes[k] == b'\'' {
                if k + 1 < bytes.len() && bytes[k + 1] == b'\'' {
                    k += 2;
                    continue;
                }
                break;
            }
            k += 1;
        }
        if k + 1 >= bytes.len() || bytes[k + 1] != b'!' {
            return false;
        }
        pos = k + 2;
    } else if let Some(bang) = s.find('!') {
        let prefix = &s[..bang];
        if !prefix.is_empty()
            && prefix
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.'))
        {
            pos = bang + 1;
        }
    }
    let tail = &bytes[pos..];
    let mut i = 0usize;
    if i < tail.len() && tail[i] == b'$' {
        i += 1;
    }
    let cs = i;
    while i < tail.len() && tail[i].is_ascii_alphabetic() {
        i += 1;
    }
    let col_len = i - cs;
    if !(1..=3).contains(&col_len) {
        return false;
    }
    if i < tail.len() && tail[i] == b'$' {
        i += 1;
    }
    let ds = i;
    while i < tail.len() && tail[i].is_ascii_digit() {
        i += 1;
    }
    if i == ds || tail[ds] == b'0' || i - ds > 7 {
        return false;
    }
    // 紧随其后不能是普通字母或下划线（防止 `A1B` 被误判）
    if i < tail.len() && (tail[i].is_ascii_alphanumeric() || tail[i] == b'_') {
        return false;
    }
    true
}

/// 跳过 `"..."` 字符串字面量（含 `""` 转义）计算未闭合左括号数量，并在尾部补齐 `)`。
fn auto_close_parens(formula: &str) -> String {
    let bytes = formula.as_bytes();
    let mut i = 0usize;
    let mut in_str = false;
    let mut open = 0i32;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'(' => open += 1,
            b')' => open -= 1,
            _ => {}
        }
        i += 1;
    }
    if open > 0 && !in_str {
        let mut out = String::with_capacity(formula.len() + open as usize);
        out.push_str(formula);
        for _ in 0..open {
            out.push(')');
        }
        out
    } else {
        formula.to_string()
    }
}

/// 仅当字符串是严格的数字字面量（允许正负号、小数点、科学计数，但拒绝 inf/nan）时解析为有限 f64。
fn parse_strict_plain_f64(t: &str) -> Option<f64> {
    let bytes = t.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut has_digit = false;
    for &b in bytes {
        match b {
            b'0'..=b'9' => has_digit = true,
            b'+' | b'-' | b'.' | b'e' | b'E' => {}
            _ => return None,
        }
    }
    if !has_digit {
        return None;
    }
    let n = t.parse::<f64>().ok()?;
    n.is_finite().then_some(n)
}

/// 校验并解析带合法千分位逗号或普通形式的十进制数，返回 `(数值, 是否有逗号, 小数位数)`。
fn parse_decimal_with_optional_commas(s: &str) -> Option<(f64, bool, usize)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, body) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest.trim_start())
    } else if let Some(rest) = s.strip_prefix('+') {
        (false, rest.trim_start())
    } else {
        (false, s)
    };
    if body.is_empty() {
        return None;
    }
    let (int_part, frac_part) = match body.split_once('.') {
        Some((i, f)) => {
            if f.contains('.') || f.contains(',') || f.is_empty() && i.is_empty() {
                return None;
            }
            if !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (i, Some(f))
        }
        None => (body, None),
    };

    let has_comma = int_part.contains(',');
    let clean_int: String;
    if has_comma {
        let mut groups = int_part.split(',');
        let first = groups.next()?;
        if first.is_empty() || first.len() > 3 || !first.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        clean_int = {
            let mut buf = String::with_capacity(int_part.len());
            buf.push_str(first);
            let mut count = 0usize;
            for g in groups {
                if g.len() != 3 || !g.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                buf.push_str(g);
                count += 1;
            }
            if count == 0 {
                return None;
            }
            buf
        };
    } else {
        if !int_part.is_empty() && !int_part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if int_part.is_empty() && frac_part.is_none() {
            return None;
        }
        clean_int = int_part.to_string();
    }

    let decimals = frac_part.map(|f| f.len()).unwrap_or(0);
    let mut num_str = String::with_capacity(clean_int.len() + decimals + 2);
    if neg {
        num_str.push('-');
    }
    if clean_int.is_empty() {
        num_str.push('0');
    } else {
        num_str.push_str(&clean_int);
    }
    if let Some(f) = frac_part {
        if !f.is_empty() {
            num_str.push('.');
            num_str.push_str(f);
        }
    }
    let val = num_str.parse::<f64>().ok()?;
    val.is_finite().then_some((val, has_comma, decimals))
}

fn format_decimals_suffix(decimals: usize, max_d: usize) -> String {
    let d = decimals.min(max_d);
    if d == 0 {
        String::new()
    } else {
        let mut s = String::with_capacity(d + 1);
        s.push('.');
        for _ in 0..d {
            s.push('0');
        }
        s
    }
}

fn parse_percent(t: &str) -> Option<ParsedInput> {
    let body = t.strip_suffix('%')?.trim();
    if body.is_empty() {
        return None;
    }
    let (val, _, decimals) = parse_decimal_with_optional_commas(body)?;
    let fmt = format!("0{}%", format_decimals_suffix(decimals, 4));
    Some(ParsedInput {
        value: Value::Number(val / 100.0),
        auto_format: Some(fmt),
        quote_prefix: false,
    })
}

fn currency_symbol(ch: char) -> Option<&'static str> {
    match ch {
        '¥' | '￥' => Some("¥"),
        '$' => Some("$"),
        '€' => Some("€"),
        '£' => Some("£"),
        _ => None,
    }
}

fn parse_currency(t: &str) -> Option<ParsedInput> {
    let (leading_neg, rest) = if let Some(r) = t.strip_prefix('-') {
        (true, r.trim_start())
    } else if let Some(r) = t.strip_prefix('+') {
        (false, r.trim_start())
    } else {
        (false, t)
    };
    let mut chars = rest.chars();
    let first = chars.next()?;
    let sym = currency_symbol(first)?;
    let num_part = chars.as_str().trim();
    if num_part.is_empty() {
        return None;
    }
    if leading_neg && (num_part.starts_with('-') || num_part.starts_with('+')) {
        return None;
    }
    let (mut val, _, decimals) = parse_decimal_with_optional_commas(num_part)?;
    if leading_neg {
        val = -val;
    }
    let fmt = format!("{sym}#,##0{}", format_decimals_suffix(decimals, 4));
    Some(ParsedInput {
        value: Value::Number(val),
        auto_format: Some(fmt),
        quote_prefix: false,
    })
}

fn parse_grouped_number(t: &str) -> Option<ParsedInput> {
    if !t.contains(',') {
        return None;
    }
    let (val, has_comma, decimals) = parse_decimal_with_optional_commas(t)?;
    if !has_comma {
        return None;
    }
    let fmt = format!("#,##0{}", format_decimals_suffix(decimals, 4));
    Some(ParsedInput {
        value: Value::Number(val),
        auto_format: Some(fmt),
        quote_prefix: false,
    })
}

fn is_valid_ymd(y: i32, m: u32, d: u32) -> bool {
    if !(1900..=9999).contains(&y) || !(1..=12).contains(&m) || d == 0 {
        return false;
    }
    let max_d = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    d <= max_d
}

/// 解析时间段：`H:M` / `H:MM` / `H:MM:SS` / `H:M:S`。
/// 当 `allow_elapsed = true` 时，允许小时 `>= 24`（如 `25:00`、`100:30:15`），返回 `(时间值, has_sec, is_elapsed)`。
fn parse_time_part(s: &str, allow_elapsed: bool) -> Option<(f64, bool, bool)> {
    let s = s.trim();
    let mut parts = s.split(':');
    let h_str = parts.next()?;
    let m_str = parts.next()?;
    let s_opt = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let max_h_len = if allow_elapsed { 5 } else { 2 };
    if h_str.is_empty()
        || h_str.len() > max_h_len
        || m_str.is_empty()
        || m_str.len() > 2
        || !h_str.bytes().all(|b| b.is_ascii_digit())
        || !m_str.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let h: u32 = h_str.parse().ok()?;
    let m: u32 = m_str.parse().ok()?;
    if (!allow_elapsed && h >= 24) || m >= 60 {
        return None;
    }
    let (sec, has_sec) = match s_opt {
        Some(ss) => {
            if ss.is_empty() || ss.len() > 2 || !ss.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let v: u32 = ss.parse().ok()?;
            if v >= 60 {
                return None;
            }
            (v, true)
        }
        None => (0, false),
    };
    let total_secs = (h as u64) * 3600 + (m as u64) * 60 + (sec as u64);
    let frac = total_secs as f64 / 86_400.0;
    Some((frac, has_sec, h >= 24))
}

fn parse_ymd_sep(s: &str, sep: char) -> Option<(i32, u32, u32)> {
    let mut parts = s.split(sep);
    let ys = parts.next()?;
    let ms = parts.next()?;
    let ds = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if ys.len() != 4
        || ms.is_empty()
        || ms.len() > 2
        || ds.is_empty()
        || ds.len() > 2
        || !ys.bytes().all(|b| b.is_ascii_digit())
        || !ms.bytes().all(|b| b.is_ascii_digit())
        || !ds.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let y: i32 = ys.parse().ok()?;
    let m: u32 = ms.parse().ok()?;
    let d: u32 = ds.parse().ok()?;
    is_valid_ymd(y, m, d).then_some((y, m, d))
}

/// 解析年月：`2026-11` / `2026/11` -> `(2026, 11, 1)`。
fn parse_ym_sep(s: &str, sep: char) -> Option<(i32, u32, u32)> {
    let (ys, ms) = s.split_once(sep)?;
    if ms.contains(sep) {
        return None;
    }
    if ys.len() != 4
        || ms.is_empty()
        || ms.len() > 2
        || !ys.bytes().all(|b| b.is_ascii_digit())
        || !ms.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let y: i32 = ys.parse().ok()?;
    let m: u32 = ms.parse().ok()?;
    is_valid_ymd(y, m, 1).then_some((y, m, 1))
}

/// 解析月日简写：`11/25` / `11-25` -> `(default_year, 11, 25)`。
fn parse_md_sep(s: &str, sep: char, default_year: i32) -> Option<(i32, u32, u32)> {
    let (ms, ds) = s.split_once(sep)?;
    if ds.contains(sep) {
        return None;
    }
    if ms.is_empty()
        || ms.len() > 2
        || ds.is_empty()
        || ds.len() > 2
        || !ms.bytes().all(|b| b.is_ascii_digit())
        || !ds.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let m: u32 = ms.parse().ok()?;
    let d: u32 = ds.parse().ok()?;
    is_valid_ymd(default_year, m, d).then_some((default_year, m, d))
}

fn parse_cn_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let (ys, rest) = s.split_once('年')?;
    let (ms, rest) = rest.split_once('月')?;
    let ds = rest.strip_suffix('日').or_else(|| rest.strip_suffix('号'))?;
    if ys.len() != 4
        || ms.is_empty()
        || ms.len() > 2
        || ds.is_empty()
        || ds.len() > 2
        || !ys.bytes().all(|b| b.is_ascii_digit())
        || !ms.bytes().all(|b| b.is_ascii_digit())
        || !ds.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let y: i32 = ys.parse().ok()?;
    let m: u32 = ms.parse().ok()?;
    let d: u32 = ds.parse().ok()?;
    is_valid_ymd(y, m, d).then_some((y, m, d))
}

/// 解析中文年月：`2026年11月` -> `(2026, 11, 1)`。
fn parse_cn_ym(s: &str) -> Option<(i32, u32, u32)> {
    let (ys, rest) = s.split_once('年')?;
    let ms = rest.strip_suffix('月')?;
    if ys.len() != 4
        || ms.is_empty()
        || ms.len() > 2
        || !ys.bytes().all(|b| b.is_ascii_digit())
        || !ms.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let y: i32 = ys.parse().ok()?;
    let m: u32 = ms.parse().ok()?;
    is_valid_ymd(y, m, 1).then_some((y, m, 1))
}

/// 解析中文月日简写：`11月25日` / `11月25号` -> `(default_year, 11, 25)`。
fn parse_cn_md(s: &str, default_year: i32) -> Option<(i32, u32, u32)> {
    if s.contains('年') {
        return None;
    }
    let (ms, rest) = s.split_once('月')?;
    let ds = rest.strip_suffix('日').or_else(|| rest.strip_suffix('号'))?;
    if ms.is_empty()
        || ms.len() > 2
        || ds.is_empty()
        || ds.len() > 2
        || !ms.bytes().all(|b| b.is_ascii_digit())
        || !ds.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let m: u32 = ms.parse().ok()?;
    let d: u32 = ds.parse().ok()?;
    is_valid_ymd(default_year, m, d).then_some((default_year, m, d))
}

fn parse_date_or_time(t: &str, sys: DateSystem, default_year: i32) -> Option<ParsedInput> {
    // 1) 纯时间 H:M / H:MM / H:MM:SS / 累计时间 25:00 / 100:30:15
    if t.contains(':') && !t.contains('/') && !t.contains('-') && !t.contains('年') && !t.contains('月') && !t.contains(' ') {
        let (frac, has_sec, is_elapsed) = parse_time_part(t, true)?;
        let fmt = match (is_elapsed, has_sec) {
            (true, true) => "[h]:mm:ss",
            (true, false) => "[h]:mm",
            (false, true) => "hh:mm:ss",
            (false, false) => "hh:mm",
        };
        return Some(ParsedInput {
            value: Value::Number(frac),
            auto_format: Some(fmt.to_string()),
            quote_prefix: false,
        });
    }

    // 2) 分离可能的日期 + 时间部分（支持空格或 ISO-8601 'T'）
    let (date_part, time_part) = if let Some((d, tm)) = t.split_once(' ') {
        (d.trim(), Some(tm.trim()))
    } else if let Some((d, tm)) = t.split_once('T') {
        (d.trim(), Some(tm.trim()))
    } else {
        (t, None)
    };

    let (ymd, base_fmt) = if let Some(ymd) = parse_ymd_sep(date_part, '/') {
        (ymd, "yyyy/mm/dd")
    } else if let Some(ymd) = parse_ymd_sep(date_part, '-') {
        (ymd, "yyyy-mm-dd")
    } else if let Some(ymd) = parse_cn_ymd(date_part) {
        (ymd, "yyyy\"年\"m\"月\"d\"日\"")
    } else if let Some(ymd) = parse_ym_sep(date_part, '-') {
        (ymd, "yyyy-mm")
    } else if let Some(ymd) = parse_ym_sep(date_part, '/') {
        (ymd, "yyyy/mm")
    } else if let Some(ymd) = parse_cn_ym(date_part) {
        (ymd, "yyyy\"年\"m\"月\"")
    } else if let Some(ymd) = parse_md_sep(date_part, '/', default_year) {
        (ymd, "m/d")
    } else if let Some(ymd) = parse_md_sep(date_part, '-', default_year) {
        (ymd, "m-d")
    } else if let Some(ymd) = parse_cn_md(date_part, default_year) {
        (ymd, "m\"月\"d\"日\"")
    } else {
        return None;
    };

    let serial = date_to_serial_in(sys, ymd.0, ymd.1, ymd.2)?;
    if let Some(tm) = time_part {
        if tm.is_empty() {
            return Some(ParsedInput {
                value: Value::Number(serial),
                auto_format: Some(base_fmt.to_string()),
                quote_prefix: false,
            });
        }
        let (frac, has_sec, _) = parse_time_part(tm, false)?;
        let fmt = if has_sec {
            format!("{base_fmt} hh:mm:ss")
        } else {
            format!("{base_fmt} hh:mm")
        };
        return Some(ParsedInput {
            value: Value::Number(serial + frac),
            auto_format: Some(fmt),
            quote_prefix: false,
        });
    }

    Some(ParsedInput {
        value: Value::Number(serial),
        auto_format: Some(base_fmt.to_string()),
        quote_prefix: false,
    })
}

/// 将单元格的值按编辑栏 / F2 规则转为可编辑文本：
/// - 带 `quote_prefix` 的文本补回前导 `'`；
/// - 日期/时间格式显示为可读日期时间而非序列号；
/// - 百分比格式显示为 `15%` 形式而非 `0.15`；
/// - 其余数字返回原始数值文本（与 Excel 编辑栏行为一致）。
pub fn format_cell_for_edit(
    sys: DateSystem,
    value: &Value,
    fmt: &str,
    quote_prefix: bool,
) -> String {
    if quote_prefix {
        if let Value::Text(s) = value {
            let mut out = String::with_capacity(s.len() + 1);
            out.push('\'');
            out.push_str(s);
            return out;
        }
    }

    let Value::Number(n) = *value else {
        return value.display();
    };
    if !n.is_finite() {
        return value.display();
    }

    let trimmed = fmt.trim();
    if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("general") {
        if is_date_format(trimmed) && n >= 0.0 {
            if let Some(s) = format_serial_for_edit(sys, n, trimmed) {
                return s;
            }
        } else if is_percent_format(trimmed) {
            let pct = n * 100.0;
            return with_formatted_number(pct, |s| format!("{s}%"));
        }
    }

    value.display()
}

fn format_serial_for_edit(sys: DateSystem, serial: f64, fmt: &str) -> Option<String> {
    let total_secs = (serial * 86_400.0).round() as i64;
    if total_secs < 0 {
        return None;
    }
    let low = fmt.to_ascii_lowercase();
    // 累计时间格式（[h]:mm / [hh]:mm:ss）保留总小时数而非转成 1900-01-01 日期
    if low.contains("[h") {
        let th = total_secs / 3600;
        let m = (total_secs % 3600) / 60;
        let s = total_secs % 60;
        return if s == 0 && !low.contains('s') {
            Some(format!("{th}:{m:02}"))
        } else {
            Some(format!("{th}:{m:02}:{s:02}"))
        };
    }
    let day = total_secs / 86_400;
    let rem = total_secs % 86_400;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;

    if day == 0 {
        return if s == 0 && !low.contains('s') {
            Some(format!("{h:02}:{m:02}"))
        } else {
            Some(format!("{h:02}:{m:02}:{s:02}"))
        };
    }

    let (y, mo, d) = serial_to_date_in(sys, day as f64)?;
    let sep = if fmt.contains('-') && !fmt.contains('/') { '-' } else { '/' };
    if rem == 0 {
        Some(format!("{y:04}{sep}{mo:02}{sep}{d:02}"))
    } else if s == 0 && !low.contains('s') {
        Some(format!("{y:04}{sep}{mo:02}{sep}{d:02} {h:02}:{m:02}"))
    } else {
        Some(format!("{y:04}{sep}{mo:02}{sep}{d:02} {h:02}:{m:02}:{s:02}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dates_percent_grouped_and_currency() {
        let d = parse_cell_input("2026/11/25", "", DateSystem::D1900);
        assert_eq!(d.value, Value::Number(date_to_serial_in(DateSystem::D1900, 2026, 11, 25).unwrap()));
        assert_eq!(d.auto_format.as_deref(), Some("yyyy/mm/dd"));
        assert!(!d.quote_prefix);

        let d2 = parse_cell_input("2026-11-25 14:30", "", DateSystem::D1900);
        let expected_dt = date_to_serial_in(DateSystem::D1900, 2026, 11, 25).unwrap() + (14.5 / 24.0);
        assert!((match d2.value { Value::Number(v) => v, _ => panic!() } - expected_dt).abs() < 1e-9);
        assert_eq!(d2.auto_format.as_deref(), Some("yyyy-mm-dd hh:mm"));

        let cn = parse_cell_input("2026年11月25日", "", DateSystem::D1900);
        assert_eq!(cn.value, d.value);

        let p = parse_cell_input("15%", "", DateSystem::D1900);
        assert_eq!(p.value, Value::Number(0.15));
        assert_eq!(p.auto_format.as_deref(), Some("0%"));

        let g = parse_cell_input("1,234.56", "", DateSystem::D1900);
        assert_eq!(g.value, Value::Number(1234.56));
        assert_eq!(g.auto_format.as_deref(), Some("#,##0.00"));

        let c = parse_cell_input("¥1,000", "", DateSystem::D1900);
        assert_eq!(c.value, Value::Number(1000.0));
        assert_eq!(c.auto_format.as_deref(), Some("¥#,##0"));

        let c2 = parse_cell_input("-$1,250.50", "", DateSystem::D1900);
        assert_eq!(c2.value, Value::Number(-1250.5));
        assert_eq!(c2.auto_format.as_deref(), Some("$#,##0.00"));
    }

    #[test]
    fn preserves_text_column_and_leading_apostrophe() {
        let acct = parse_cell_input("6222021234567890123", "@", DateSystem::D1900);
        assert_eq!(acct.value, Value::Text("6222021234567890123".into()));
        assert!(!acct.quote_prefix);
        assert!(acct.auto_format.is_none());

        let q = parse_cell_input("'00123", "", DateSystem::D1900);
        assert_eq!(q.value, Value::Text("00123".into()));
        assert!(q.quote_prefix);
        assert!(q.auto_format.is_none());
    }

    #[test]
    fn rejects_invalid_grouping_and_non_finite() {
        assert_eq!(parse_cell_input("1,23", "", DateSystem::D1900).value, Value::Text("1,23".into()));
        assert_eq!(parse_cell_input("inf", "", DateSystem::D1900).value, Value::Text("inf".into()));
        assert_eq!(parse_cell_input("NaN", "", DateSystem::D1900).value, Value::Text("NaN".into()));
        assert_eq!(parse_cell_input("2026/02/30", "", DateSystem::D1900).value, Value::Text("2026/02/30".into()));
    }

    #[test]
    fn formats_dates_percent_and_quote_for_edit() {
        let serial = date_to_serial_in(DateSystem::D1900, 2026, 11, 25).unwrap();
        assert_eq!(
            format_cell_for_edit(DateSystem::D1900, &Value::Number(serial), "yyyy/mm/dd", false),
            "2026/11/25"
        );
        assert_eq!(
            format_cell_for_edit(DateSystem::D1900, &Value::Number(serial), "yyyy-mm-dd", false),
            "2026-11-25"
        );
        assert_eq!(
            format_cell_for_edit(DateSystem::D1900, &Value::Number(0.15), "0%", false),
            "15%"
        );
        assert_eq!(
            format_cell_for_edit(DateSystem::D1900, &Value::Text("00123".into()), "", true),
            "'00123"
        );
    }
}
