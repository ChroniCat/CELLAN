//! WPS 智能填充序列识别（星期、月份、季度、带编号文本如 `第1组`/`A-001`、数值等差、日期）。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillMode {
    /// 智能默认：日期/内置周期/带编号文本递推，单个普通数字或文本复制
    Smart,
    /// 复制单元格（值和格式都复制，文本带数字也不递增）
    CopyCells,
    /// 填充序列（单个数字也 +1）
    FillSeries,
    /// 仅填充格式
    FormatsOnly,
    /// 填充不带格式（只写值/公式，保留目标原样式）
    ValuesOnly,
}

const WEEKDAYS_CN_FULL: [&str; 7] = ["星期一", "星期二", "星期三", "星期四", "星期五", "星期六", "星期日"];
const WEEKDAYS_CN_SHORT: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
const WEEKDAYS_EN_FULL: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const WEEKDAYS_EN_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS_CN: [&str; 12] = ["一月", "二月", "三月", "四月", "五月", "六月", "七月", "八月", "九月", "十月", "十一月", "十二月"];
const MONTHS_EN_FULL: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const MONTHS_EN_SHORT: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const QUARTERS_CN: [&str; 4] = ["第一季度", "第二季度", "第三季度", "第四季度"];
const QUARTERS_SHORT: [&str; 4] = ["Q1", "Q2", "Q3", "Q4"];
const STEMS_CN: [&str; 10] = ["甲", "乙", "丙", "丁", "戊", "己", "庚", "辛", "壬", "癸"];

fn cyclic_match(samples: &[&str], list: &[&str], dir_sign: i64, step_index: i64) -> Option<String> {
    if samples.is_empty() {
        return None;
    }
    let find_idx = |s: &str| -> Option<usize> {
        let t = s.trim();
        if list.len() == 7 && list[6] == "星期日" && t == "星期天" {
            return Some(6);
        }
        list.iter().position(|x| x.eq_ignore_ascii_case(t))
    };
    let idxs: Vec<usize> = samples.iter().map(|s| find_idx(s)).collect::<Option<_>>()?;
    let n = list.len() as i64;
    let step = if idxs.len() >= 2 {
        let d = idxs[1] as i64 - idxs[0] as i64;
        if d == 0 { dir_sign } else { d }
    } else {
        dir_sign
    };
    let last = *idxs.last().unwrap() as i64;
    let target = (last + step * step_index).rem_euclid(n) as usize;
    Some(list[target].to_string())
}

/// 解析 `<prefix><digits><suffix>`（取最后一段连续数字，适配 `A-001`、`第1组`、`1月`、`2026Q1` 等）
fn split_prefix_num_suffix(s: &str) -> Option<(&str, i64, usize, &str)> {
    let bytes = s.as_bytes();
    let mut end = None;
    for i in (0..bytes.len()).rev() {
        if bytes[i].is_ascii_digit() {
            end = Some(i + 1);
            break;
        }
    }
    let end = end?;
    let mut start = end;
    while start > 0 && bytes[start - 1].is_ascii_digit() {
        start -= 1;
    }
    let digits = &s[start..end];
    let num: i64 = digits.parse().ok()?;
    let pad = if digits.len() > 1 && digits.starts_with('0') {
        digits.len()
    } else {
        0
    };
    Some((&s[..start], num, pad, &s[end..]))
}

fn format_with_pad(prefix: &str, val: i64, pad: usize, suffix: &str) -> String {
    let body = if pad > 0 && val >= 0 {
        format!("{val:0pad$}")
    } else {
        val.to_string()
    };
    format!("{prefix}{body}{suffix}")
}

/// 对日期字符串（`YYYY-MM-DD` 或 `YYYY/M/D`）按天数递增
fn try_date_str_step(samples: &[&str], dir_sign: i64, step_index: i64) -> Option<String> {
    let parse_ymd = |s: &str| -> Option<(i32, u32, u32, char)> {
        let sep = if s.contains('-') {
            '-'
        } else if s.contains('/') {
            '/'
        } else {
            return None;
        };
        let parts: Vec<&str> = s.trim().split(sep).collect();
        if parts.len() != 3 {
            return None;
        }
        let y: i32 = parts[0].parse().ok()?;
        let m: u32 = parts[1].parse().ok()?;
        let d: u32 = parts[2].parse().ok()?;
        if !(1900..=2999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        Some((y, m, d, sep))
    };

    let parsed: Vec<_> = samples.iter().map(|s| parse_ymd(s)).collect::<Option<_>>()?;
    let to_days = |y: i32, m: u32, d: u32| -> i64 {
        let y = y as i64 - if m <= 2 { 1 } else { 0 };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let mp = m as i64 + if m > 2 { -3 } else { 9 };
        let doy = (153 * mp + 2) / 5 + d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe
    };
    let from_days = |z: i64| -> (i32, u32, u32) {
        let era = z.div_euclid(146097);
        let doe = z.rem_euclid(146097);
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = mp + if mp < 10 { 3 } else { -9 };
        let y = y + if m <= 2 { 1 } else { 0 };
        (y as i32, m as u32, d as u32)
    };

    let d_vals: Vec<i64> = parsed.iter().map(|&(y, m, d, _)| to_days(y, m, d)).collect();
    let sep = parsed.last().unwrap().3;
    let last_day = *d_vals.last().unwrap();
    let step_signed = if d_vals.len() >= 2 { d_vals[1] - d_vals[0] } else { dir_sign };
    let (ry, rm, rd) = from_days(last_day + step_signed * step_index);
    Some(format!("{ry:04}{sep}{rm:02}{sep}{rd:02}"))
}

/// 根据源样本计算向填充方向推进第 `step_index >= 1` 格的文本；
/// 调用方传入已按拖拽方向排列的样本（向上/向左拖拽时已反转为由末到首，`dir_sign = -1`）。
pub fn next_text_sequence(samples: &[&str], dir_sign: i64, step_index: i64) -> Option<String> {
    if samples.is_empty() || step_index == 0 {
        return None;
    }

    for list in [
        &WEEKDAYS_CN_FULL[..],
        &WEEKDAYS_CN_SHORT[..],
        &WEEKDAYS_EN_FULL[..],
        &WEEKDAYS_EN_SHORT[..],
        &MONTHS_CN[..],
        &MONTHS_EN_FULL[..],
        &MONTHS_EN_SHORT[..],
        &QUARTERS_CN[..],
        &QUARTERS_SHORT[..],
        &STEMS_CN[..],
    ] {
        if let Some(hit) = cyclic_match(samples, list, dir_sign, step_index) {
            return Some(hit);
        }
    }

    if let Some(d) = try_date_str_step(samples, dir_sign, step_index) {
        return Some(d);
    }

    // `1月`..`12月`
    let month_nums: Option<Vec<i64>> = samples
        .iter()
        .map(|s| s.trim().strip_suffix('月').and_then(|n| n.parse::<i64>().ok()))
        .collect();
    if let Some(ms) = month_nums {
        if ms.iter().all(|&m| (1..=12).contains(&m)) {
            let step = if ms.len() >= 2 { ms[1] - ms[0] } else { dir_sign };
            let anchor = *ms.last().unwrap();
            let v = (anchor - 1 + step * step_index).rem_euclid(12) + 1;
            return Some(format!("{v}月"));
        }
    }

    // 带数字后缀文本：`第1组`、`A-001` 等
    let parsed: Vec<_> = samples.iter().map(|s| split_prefix_num_suffix(s)).collect::<Option<_>>()?;
    let (p0, _, pad0, s0) = parsed[0];
    if parsed.iter().all(|(p, _, _, s)| *p == p0 && *s == s0) {
        let step = if parsed.len() >= 2 { parsed[1].1 - parsed[0].1 } else { dir_sign };
        let anchor = parsed.last().unwrap().1;
        let max_pad = parsed.iter().map(|x| x.2).max().unwrap_or(pad0);
        return Some(format_with_pad(p0, anchor + step * step_index, max_pad, s0));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_chinese_sequences_and_codes() {
        assert_eq!(next_text_sequence(&["星期一"], 1, 2).as_deref(), Some("星期三"));
        assert_eq!(next_text_sequence(&["周一"], -1, 1).as_deref(), Some("周日"));
        assert_eq!(next_text_sequence(&["1月", "2月"], 1, 11).as_deref(), Some("1月"));
        assert_eq!(next_text_sequence(&["第一季度"], 1, 3).as_deref(), Some("第四季度"));
        assert_eq!(next_text_sequence(&["第1组"], 1, 2).as_deref(), Some("第3组"));
        assert_eq!(next_text_sequence(&["A-001"], 1, 4).as_deref(), Some("A-005"));
        assert_eq!(next_text_sequence(&["2026-01-30"], 1, 3).as_deref(), Some("2026-02-02"));
        // 多样本反向拖拽（传入顺序已被调用方反转为递减）不能双重取反
        assert_eq!(next_text_sequence(&["星期二", "星期一"], -1, 1).as_deref(), Some("星期日"));
        assert_eq!(next_text_sequence(&["A-002", "A-001"], -1, 1).as_deref(), Some("A-000"));
        assert_eq!(next_text_sequence(&["2026-02-02", "2026-02-01"], -1, 2).as_deref(), Some("2026-01-30"));
    }
}
