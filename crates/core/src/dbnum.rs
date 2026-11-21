//! 中文数字大写 / 小写与人民币大写格式化（`[DBNum1]` / `[DBNum2]` / `[DBNum3]`）。

const DIGITS_LOWER: [&str; 10] = ["〇", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
const DIGITS_UPPER: [&str; 10] = ["零", "壹", "贰", "叁", "肆", "伍", "陆", "柒", "捌", "玖"];
const UNITS_LOWER: [&str; 4] = ["", "十", "百", "千"];
const UNITS_UPPER: [&str; 4] = ["", "拾", "佰", "仟"];
const BIG_UNITS: [&str; 5] = ["", "万", "亿", "兆", "京"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbNumKind {
    /// `[DBNum1]` 中文小写数字
    Lower,
    /// `[DBNum2]` 中文大写数字（含 `￥`/`¥`/`元` 时为人民币大写）
    Upper { rmb: bool },
    /// `[DBNum3]` 全角数字
    FullWidth,
}

/// 从 Excel/WPS 数字格式字符串片段中识别 `[DBNumN]` 标记。
pub fn detect_dbnum(raw: &str) -> Option<DbNumKind> {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("[dbnum2]") {
        let rmb = raw.contains('￥')
            || raw.contains('¥')
            || raw.contains('元')
            || lower.contains("rmb");
        Some(DbNumKind::Upper { rmb })
    } else if lower.contains("[dbnum1]") {
        Some(DbNumKind::Lower)
    } else if lower.contains("[dbnum3]") {
        Some(DbNumKind::FullWidth)
    } else {
        None
    }
}

/// 用 `[DBNum1]` / `[DBNum2]` / `[DBNum3]` 规则格式化数值。
///
/// 对于包含 `#`/`0` 小数位定义的格式，非 RMB 模式下会按小数位数四舍五入后输出；
/// RMB 模式下统一精确到分（两位小数）并按财务规范输出 `元/角/分/整`。
pub fn format_dbnum(val: f64, kind: DbNumKind, raw: &str) -> Option<String> {
    if !val.is_finite() {
        return None;
    }
    match kind {
        DbNumKind::Lower => Some(format_chinese_number(val, false, infer_decimals(raw))),
        DbNumKind::Upper { rmb: false } => {
            Some(format_chinese_number(val, true, infer_decimals(raw)))
        }
        DbNumKind::Upper { rmb: true } => Some(format_rmb_upper(val)),
        DbNumKind::FullWidth => Some(to_fullwidth(&format_plain_fallback(val))),
    }
}

/// 将阿拉伯数字字符串中的数字转换为中文小写/大写或全角（用于日期等文本后处理）。
pub fn map_digits_for_date(text: &str, kind: DbNumKind) -> String {
    match kind {
        DbNumKind::FullWidth => to_fullwidth(text),
        DbNumKind::Lower => replace_date_numbers(text, false),
        DbNumKind::Upper { .. } => replace_date_numbers(text, true),
    }
}

fn format_plain_fallback(val: f64) -> String {
    if val == 0.0 {
        return "0".to_string();
    }
    let s = format!("{val:.6}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

fn to_fullwidth(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '0'..='9' => char::from_u32(0xFF10 + (c as u32 - '0' as u32)).unwrap_or(c),
            _ => c,
        })
        .collect()
}

fn infer_decimals(raw: &str) -> Option<usize> {
    // 忽略方括号和引号内部，统计小数点后的 `0` / `#` 位数
    let mut in_bracket = false;
    let mut in_quote = false;
    let mut after_dot = false;
    let mut count = 0usize;
    let mut saw_dot = false;
    for ch in raw.chars() {
        if in_quote {
            if ch == '"' {
                in_quote = false;
            }
            continue;
        }
        if in_bracket {
            if ch == ']' {
                in_bracket = false;
            }
            continue;
        }
        match ch {
            '"' => in_quote = true,
            '[' => in_bracket = true,
            '.' => {
                saw_dot = true;
                after_dot = true;
            }
            '0' | '#' if after_dot => count += 1,
            _ if after_dot => {}
            _ => {}
        }
    }
    if saw_dot {
        Some(count.min(6))
    } else {
        None
    }
}

/// 将非负整数转换为中文数字（`0`..=`9_999_999_999_999_999`）。
fn uint_to_chinese(mut n: u64, upper: bool) -> String {
    let digits = if upper { &DIGITS_UPPER } else { &DIGITS_LOWER };
    let units = if upper { &UNITS_UPPER } else { &UNITS_LOWER };
    let zero_word = digits[0];

    if n == 0 {
        return zero_word.to_string();
    }

    let mut groups = Vec::new();
    while n > 0 {
        groups.push((n % 10_000) as u32);
        n /= 10_000;
    }

    let mut out = String::new();
    let mut pending_zero = false;
    for g_idx in (0..groups.len()).rev() {
        let g = groups[g_idx];
        if g == 0 {
            if !out.is_empty() {
                pending_zero = true;
            }
            continue;
        }
        // 高位组末尾为空（如 10_0500 的万组末尾没零、千位为 0）或整组跨零
        if pending_zero || (!out.is_empty() && g < 1000) {
            out.push_str(zero_word);
        }
        pending_zero = false;
        let g_str = group4_to_chinese(g, digits, units, zero_word, out.is_empty());
        out.push_str(&g_str);
        out.push_str(BIG_UNITS.get(g_idx).copied().unwrap_or(""));
    }

    out
}

fn group4_to_chinese(
    g: u32,
    digits: &[&str; 10],
    units: &[&str; 4],
    zero_word: &str,
    _is_highest_group: bool,
) -> String {
    let mut out = String::new();
    let mut need_zero = false;
    let d = [(g / 1000) % 10, (g / 100) % 10, (g / 10) % 10, g % 10];
    for (idx, &val) in d.iter().enumerate() {
        let pos = 3 - idx;
        if val == 0 {
            if !out.is_empty() {
                need_zero = true;
            }
        } else {
            if need_zero {
                out.push_str(zero_word);
                need_zero = false;
            }
            // Excel/WPS 对十位为 1 的规范（大写 壹拾、小写 一十 / 十；此处统一保留数字，兼容财务/Excel习惯）
            out.push_str(digits[val as usize]);
            out.push_str(units[pos]);
        }
    }
    out
}

/// 格式化中文数字（支持正负号、整数、小数）。
pub fn format_chinese_number(val: f64, upper: bool, fixed_decimals: Option<usize>) -> String {
    let digits = if upper { &DIGITS_UPPER } else { &DIGITS_LOWER };
    let negative = val < -1e-12;
    let abs = val.abs();

    let (int_part, frac_digits) = match fixed_decimals {
        Some(prec) => {
            let factor = 10f64.powi(prec as i32);
            let rounded = (abs * factor).round();
            if prec == 0 {
                (rounded as u64, String::new())
            } else {
                let s = format!("{:.*}", prec, rounded / factor);
                let mut parts = s.split('.');
                let ip = parts.next().and_then(|p| p.parse::<u64>().ok()).unwrap_or(0);
                let fp = parts.next().unwrap_or("").trim_end_matches('0').to_string();
                (ip, fp)
            }
        }
        None => {
            // 常规模式下避免浮点尾数噪音（如 12.340000000000001）
            let s = format!("{abs:.6}");
            let trimmed = s.trim_end_matches('0').trim_end_matches('.');
            let mut parts = trimmed.split('.');
            let ip = parts.next().and_then(|p| p.parse::<u64>().ok()).unwrap_or(0);
            let fp = parts.next().unwrap_or("").to_string();
            (ip, fp)
        }
    };

    let mut out = String::new();
    if negative && (int_part > 0 || !frac_digits.is_empty()) {
        out.push('负');
    }
    out.push_str(&uint_to_chinese(int_part, upper));
    if !frac_digits.is_empty() {
        out.push('点');
        for b in frac_digits.bytes() {
            let idx = (b - b'0') as usize;
            out.push_str(digits[idx]);
        }
    }
    out
}

/// 按财务人民币大写规则格式化金额：`12345.67` -> `壹万贰仟叁佰肆拾伍元陆角柒分`。
pub fn format_rmb_upper(val: f64) -> String {
    let total_fen = (val.abs() * 100.0).round() as u64;
    if total_fen == 0 {
        return "零元整".to_string();
    }

    let negative = val < 0.0;
    let yuan = total_fen / 100;
    let jiao = ((total_fen / 10) % 10) as usize;
    let fen = (total_fen % 10) as usize;

    let mut out = String::new();
    if negative {
        out.push('负');
    }

    if yuan > 0 {
        out.push_str(&uint_to_chinese(yuan, true));
        out.push('元');
        if jiao == 0 && fen == 0 {
            out.push('整');
            return out;
        }
        if jiao == 0 {
            // 如 1005.05 -> 壹仟零伍元零伍分
            out.push_str(DIGITS_UPPER[0]);
            out.push_str(DIGITS_UPPER[fen]);
            out.push('分');
        } else {
            out.push_str(DIGITS_UPPER[jiao]);
            out.push('角');
            if fen == 0 {
                out.push('整');
            } else {
                out.push_str(DIGITS_UPPER[fen]);
                out.push('分');
            }
        }
    } else {
        // 仅角/分
        if jiao > 0 {
            out.push_str(DIGITS_UPPER[jiao]);
            out.push('角');
            if fen == 0 {
                out.push('整');
            } else {
                out.push_str(DIGITS_UPPER[fen]);
                out.push('分');
            }
        } else {
            out.push_str(DIGITS_UPPER[fen]);
            out.push('分');
        }
    }

    out
}

fn replace_date_numbers(text: &str, upper: bool) -> String {
    // 年份逐位替换（如 2026 -> 二〇二六），月/日（<=2位）按中文数字（11 -> 十一）
    let digits = if upper { &DIGITS_UPPER } else { &DIGITS_LOWER };
    let mut out = String::with_capacity(text.len() * 2);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let token = &text[start..i];
            if token.len() == 4 {
                for b in token.bytes() {
                    out.push_str(digits[(b - b'0') as usize]);
                }
            } else if let Ok(n) = token.parse::<u64>() {
                if !upper && (10..=19).contains(&n) {
                    out.push('十');
                    if n > 10 {
                        out.push_str(digits[(n - 10) as usize]);
                    }
                } else if upper && (10..=19).contains(&n) {
                    out.push_str("壹拾");
                    if n > 10 {
                        out.push_str(digits[(n - 10) as usize]);
                    }
                } else {
                    out.push_str(&uint_to_chinese(n, upper));
                }
            } else {
                out.push_str(token);
            }
        } else {
            let ch = text[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rmb_upper_standard_cases() {
        assert_eq!(format_rmb_upper(0.0), "零元整");
        assert_eq!(format_rmb_upper(12345.67), "壹万贰仟叁佰肆拾伍元陆角柒分");
        assert_eq!(format_rmb_upper(1005.05), "壹仟零伍元零伍分");
        assert_eq!(format_rmb_upper(100.0), "壹佰元整");
        assert_eq!(format_rmb_upper(12.5), "壹拾贰元伍角整");
        assert_eq!(format_rmb_upper(100_500.0), "壹拾万零伍佰元整");
        assert_eq!(format_rmb_upper(100_000_000.01), "壹亿元零壹分");
        assert_eq!(format_rmb_upper(-88.08), "负捌拾捌元零捌分");
        assert_eq!(format_rmb_upper(0.5), "伍角整");
        assert_eq!(format_rmb_upper(0.05), "伍分");
    }

    #[test]
    fn chinese_lower_and_upper_numbers() {
        assert_eq!(format_chinese_number(0.0, false, None), "〇");
        assert_eq!(format_chinese_number(12345.0, false, None), "一万二千三百四十五");
        assert_eq!(format_chinese_number(1005.25, false, None), "一千〇五点二五");
        assert_eq!(format_chinese_number(12345.0, true, None), "壹万贰仟叁佰肆拾伍");
        assert_eq!(format_chinese_number(-1005.0, true, None), "负壹仟零伍");
    }

    #[test]
    fn detect_dbnum_variants() {
        assert_eq!(detect_dbnum("[DBNum1]G/通用格式"), Some(DbNumKind::Lower));
        assert_eq!(
            detect_dbnum("[DBNum2][$-804]G/通用格式"),
            Some(DbNumKind::Upper { rmb: false })
        );
        assert_eq!(
            detect_dbnum("[DBNum2][$￥-804]#,##0.00"),
            Some(DbNumKind::Upper { rmb: true })
        );
    }
}
