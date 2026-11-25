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

/// 按单元格当前格式与工作簿日期体系解析用户输入。
pub fn parse_cell_input(raw: &str, existing_format: &str, sys: DateSystem) -> ParsedInput {
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

    // 3) 百分比（如 15%、-12.5%、1,234.5%）
    if let Some(parsed) = parse_percent(t) {
        return parsed;
    }

    // 4) 货币金额（如 ¥1,000、￥1,234.56、$1,000.50、€50、£99.9、-$500、¥-100）
    if let Some(parsed) = parse_currency(t) {
        return parsed;
    }

    // 5) 千分位数值（如 1,234、-1,234,567.89）
    if let Some(parsed) = parse_grouped_number(t) {
        return parsed;
    }

    // 6) 日期 / 日期时间 / 时间
    if let Some(parsed) = parse_date_or_time(t, sys) {
        return parsed;
    }

    ParsedInput::plain(Value::Text(intern_short_str(raw)))
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

fn parse_time_part(s: &str) -> Option<(f64, bool)> {
    let s = s.trim();
    let mut parts = s.split(':');
    let h_str = parts.next()?;
    let m_str = parts.next()?;
    let s_opt = parts.next();
    if parts.next().is_some() {
        return None;
    }
    if h_str.is_empty()
        || h_str.len() > 2
        || m_str.len() != 2
        || !h_str.bytes().all(|b| b.is_ascii_digit())
        || !m_str.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let h: u32 = h_str.parse().ok()?;
    let m: u32 = m_str.parse().ok()?;
    if h >= 24 || m >= 60 {
        return None;
    }
    let (sec, has_sec) = match s_opt {
        Some(ss) => {
            if ss.len() != 2 || !ss.bytes().all(|b| b.is_ascii_digit()) {
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
    let frac = (h * 3600 + m * 60 + sec) as f64 / 86_400.0;
    Some((frac, has_sec))
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

fn parse_date_or_time(t: &str, sys: DateSystem) -> Option<ParsedInput> {
    // 1) 纯时间 H:MM 或 H:MM:SS
    if t.contains(':') && !t.contains('/') && !t.contains('-') && !t.contains('年') && !t.contains(' ') {
        let (frac, has_sec) = parse_time_part(t)?;
        let fmt = if has_sec { "hh:mm:ss" } else { "hh:mm" };
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
        let (frac, has_sec) = parse_time_part(tm)?;
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
    let day = total_secs / 86_400;
    let rem = total_secs % 86_400;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;

    if day == 0 {
        return if s == 0 && !fmt.to_ascii_lowercase().contains('s') {
            Some(format!("{h:02}:{m:02}"))
        } else {
            Some(format!("{h:02}:{m:02}:{s:02}"))
        };
    }

    let (y, mo, d) = serial_to_date_in(sys, day as f64)?;
    let sep = if fmt.contains('-') && !fmt.contains('/') { '-' } else { '/' };
    if rem == 0 {
        Some(format!("{y:04}{sep}{mo:02}{sep}{d:02}"))
    } else if s == 0 && !fmt.to_ascii_lowercase().contains('s') {
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
