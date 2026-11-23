//! 函数库。所有函数经由 `call()` 分发，新增函数只需在 match 里加一行。

use super::{Arg, ErrKind, FValue};

// ---------------------------------------------------------------- 小工具

pub(crate) fn r(f: impl FnOnce() -> Result<FValue, ErrKind>) -> FValue {
    match f() {
        Ok(v) => v,
        Err(e) => FValue::Error(e),
    }
}

pub(crate) fn arg(args: &[Arg], i: usize) -> FValue {
    args.get(i).map(|a| a.first()).unwrap_or(FValue::Empty)
}

pub(crate) fn num(args: &[Arg], i: usize) -> Result<f64, ErrKind> {
    arg(args, i).to_number()
}

pub(crate) fn opt_num(args: &[Arg], i: usize, d: f64) -> Result<f64, ErrKind> {
    match args.get(i) {
        None => Ok(d),
        Some(a) => match a.first() {
            FValue::Empty => Ok(d),
            v => v.to_number(),
        },
    }
}

pub(crate) fn text(args: &[Arg], i: usize) -> Result<String, ErrKind> {
    arg(args, i).to_text()
}

pub(crate) fn int_arg(args: &[Arg], i: usize) -> Result<i64, ErrKind> {
    Ok(num(args, i)?.trunc() as i64)
}

/// 只取第一个参数的数字序列。带 k / p 这类标量参数的函数必须用它，
/// 否则 numbers(args, 0) 会把后面的标量也当成数据（曾导致 TRIMMEAN/LARGE 结果错）。
pub(crate) fn first_numbers(args: &[Arg]) -> Result<Vec<f64>, ErrKind> {
    match args.first() {
        Some(a) => a.numbers(),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn numbers(args: &[Arg], from: usize) -> Result<Vec<f64>, ErrKind> {
    let mut out = Vec::new();
    for a in args.iter().skip(from) {
        out.extend(a.numbers()?);
    }
    Ok(out)
}

pub(crate) fn flatten_values(args: &[Arg], from: usize) -> Vec<FValue> {
    let mut out = Vec::new();
    for a in args.iter().skip(from) {
        out.extend(a.values());
    }
    out
}

// ---------------------------------------------------------------- 日期系统

/// 1970-01-01 起的天数 → 公历年月日（Howard Hinnant 算法）
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y } as i32, m, d)
}

pub(crate) fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = y as i64 - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

// ---------------------------------------------------------------- 日期体系
//
// 函数库拿不到 Workbook，所以当前日期体系放在线程本地：`Calc` / `eval_formula`
// 在求值前用 `with_date_system` 设好，退出时恢复。默认 1900。

use crate::model::DateSystem;
use std::cell::Cell;

thread_local! {
    static DATE_SYSTEM: Cell<DateSystem> = const { Cell::new(DateSystem::D1900) };
    /// 验收注入：UTC Unix 秒 + 本地偏移秒；None 时读系统时钟/系统时区。
    static CLOCK_OVERRIDE: Cell<Option<(i64, i32)>> = const { Cell::new(None) };
    /// 一次 Calc 求值内固定的本地序列号，保证 NOW/TODAY 同轮取同一个时刻。
    static NOW_SNAPSHOT: Cell<Option<f64>> = const { Cell::new(None) };
}

/// 在这段求值里使用指定的 UTC 时间和本地 UTC 偏移（秒）。
/// 测试用它模拟 Asia/Shanghai 的 00:30、时区变化和午夜，不等待真实时间。
pub fn with_clock_at<T>(unix_secs: i64, offset_seconds: i32, f: impl FnOnce() -> T) -> T {
    let prev = CLOCK_OVERRIDE.with(|c| c.replace(Some((unix_secs, offset_seconds))));
    let out = f();
    CLOCK_OVERRIDE.with(|c| c.set(prev));
    out
}

pub fn now_snapshot() -> Option<f64> {
    NOW_SNAPSHOT.with(|c| c.get())
}

pub fn set_now_snapshot(v: Option<f64>) {
    NOW_SNAPSHOT.with(|c| c.set(v));
}

/// 顶层重算开始前固定一次 NOW；返回进入前的值，结束时用 set_now_snapshot 还原。
/// 必须显式传入目标工作簿的日期体系：`now_serial_uncached` 默认读线程本地的
/// D1900，若在 `Calc::new` 设置 D1904 之前先快照，会把 D1900 序列号泄漏给 1904 工作簿。
pub(crate) fn ensure_now_snapshot_in(sys: DateSystem) -> Option<f64> {
    if let Some(v) = now_snapshot() {
        return Some(v);
    }
    set_now_snapshot(Some(now_serial_uncached_in(sys)));
    None
}

fn clock_now() -> (i64, i32) {
    if let Some(v) = CLOCK_OVERRIDE.with(|c| c.get()) {
        return v;
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (secs, local_offset_seconds() as i32)
}


/// 由 `Calc::new` 设置、`Calc::drop` 恢复
pub fn set_current_date_system(sys: DateSystem) {
    DATE_SYSTEM.with(|c| c.set(sys));
}

pub fn current_date_system() -> DateSystem {
    DATE_SYSTEM.with(|c| c.get())
}

/// 在这段求值里用指定日期体系；嵌套调用会正确恢复外层
pub fn with_date_system<T>(sys: DateSystem, f: impl FnOnce() -> T) -> T {
    let prev = DATE_SYSTEM.with(|c| c.replace(sys));
    let out = f();
    DATE_SYSTEM.with(|c| c.set(prev));
    out
}

pub fn date_to_serial_in(sys: DateSystem, y: i32, m: u32, d: u32) -> Option<f64> {
    if !(1..=12).contains(&m) || d == 0 || d > 31 {
        return None;
    }
    // Excel 的 1900 体系有虚构的 1900-02-29（序列 60），公历校验会拒绝它。
    if !sys.is_1904() && y == 1900 && m == 2 && d == 29 {
        return Some(60.0);
    }
    if d > days_in_month(y, m) {
        return None;
    }
    let days = days_from_civil(y, m, d);
    if sys.is_1904() {
        // 1904 体系：1904-01-01 = 0，没有 1900 闰年 bug
        return Some((days - days_from_civil(1904, 1, 1)) as f64);
    }
    // Excel 的 1900 闰年 bug：1900-03-01 之前的序列号要 +1
    let pivot = days_from_civil(1900, 3, 1);
    Some((days + if days >= pivot { 25_569 } else { 25_568 }) as f64)
}

pub fn serial_to_date_in(sys: DateSystem, serial: f64) -> Option<(i32, u32, u32)> {
    if !serial.is_finite() || serial < 0.0 {
        return None;
    }
    let d = serial.floor();
    if sys.is_1904() {
        return Some(civil_from_days(days_from_civil(1904, 1, 1) + d as i64));
    }
    // 1900 体系：序列 60 是 Excel 虚构的 1900-02-29；不能落到公历 03-01。
    if d == 60.0 {
        return Some((1900, 2, 29));
    }
    let days = if d < 60.0 { d - 25_568.0 } else { d - 25_569.0 };
    Some(civil_from_days(days as i64))
}

pub(crate) fn date_to_serial(y: i32, m: u32, d: u32) -> Option<f64> {
    date_to_serial_in(current_date_system(), y, m, d)
}

pub(crate) fn serial_to_date(serial: f64) -> Option<(i32, u32, u32)> {
    serial_to_date_in(current_date_system(), serial)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// 本机 UTC 偏移（秒）。Excel 的 NOW/TODAY 用本地时间，不用 UTC。
/// 用 GetLocalTime 直接拿本地钟面（夏令时由系统处理），只声明一个 SYSTEMTIME，
/// 不碰 TIME_ZONE_INFORMATION 那种容易写错布局的大结构。
#[cfg(windows)]
fn local_offset_seconds() -> i64 {
    #[repr(C)]
    struct SystemTime {
        year: u16, month: u16, dow: u16, day: u16,
        hour: u16, minute: u16, second: u16, ms: u16,
    }
    unsafe extern "system" {
        fn GetLocalTime(st: *mut SystemTime);
    }
    let mut st = SystemTime { year: 0, month: 0, dow: 0, day: 0, hour: 0, minute: 0, second: 0, ms: 0 };
    unsafe { GetLocalTime(&mut st) };
    if st.year < 1970 {
        return 0;
    }
    let utc = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let local = days_from_civil(st.year as i32, st.month as u32, st.day as u32) * 86_400
        + st.hour as i64 * 3600 + st.minute as i64 * 60 + st.second as i64;
    // 取整到 15 分钟，去掉两次读钟之间的秒级抖动
    ((local - utc) as f64 / 900.0).round() as i64 * 900
}

#[cfg(not(windows))]
fn local_offset_seconds() -> i64 {
    0
}

fn now_serial_uncached() -> f64 {
    now_serial_uncached_in(current_date_system())
}

fn now_serial_uncached_in(sys: DateSystem) -> f64 {
    let (secs, offset) = clock_now();
    let local_secs = secs as f64 + offset as f64;
    let serial_1900 = local_secs / 86_400.0 + 25_569.0;
    if sys.is_1904() {
        serial_1900 - DateSystem::OFFSET
    } else {
        serial_1900
    }
}

pub(crate) fn now_serial() -> f64 {
    if let Some(v) = now_snapshot() {
        return v;
    }
    now_serial_uncached()
}

// ---------------------------------------------------------------- 条件匹配

/// Excel 的通配符与比较条件（"">10"、"<>x"、"a*" 等）

/// 预编译的筛选条件：解析一次，逐格比较不再分配
pub(crate) struct Crit {
    op: &'static str,
    num: Option<f64>,
    text: String,
    wild: bool,
}

#[derive(Eq, PartialEq, Hash)]
enum GroupValue {
    Number(u64),
    Text(String),
}

impl GroupValue {
    // Equality scans use an epsilon. Hash integral keys only when *all* numeric
    // data in that column are integral, so nearby fractions cannot be lost.
    fn integer(n: f64) -> Option<Self> {
        (n.fract() == 0.0).then(|| Self::Number(if n == 0.0 { 0 } else { n.to_bits() }))
    }
}

/// Group a repeated set of literal SUMIFS/COUNTIFS ranges by equality criteria.
/// Each function shares its group budget across a few recently used range sets.
/// Inequalities, wildcards and fractional numeric keys keep the scan semantics.
#[derive(Default)]
pub(super) struct GroupedCriteria {
    entries: std::collections::VecDeque<GroupedRange>,
}

struct GroupedRange {
    ranges: Vec<super::RangeKey>,
    numeric: Vec<bool>,
    attempted: bool,
    groups: Option<std::collections::HashMap<Vec<GroupValue>, f64>>,
}

impl GroupedCriteria {
    #[cfg(test)]
    pub(super) fn built_ranges(&self) -> usize {
        self.entries.iter().filter(|e| e.groups.is_some()).count()
    }
    #[cfg(test)]
    pub(super) fn entries_len(&self) -> usize { self.entries.len() }

    pub(super) fn lookup(&mut self, key: &super::CriteriaKey, args: &[Arg]) -> Option<FValue> {
        let sum = key.function == 0;
        let criteria = args.get(usize::from(sum)..)?;
        if criteria.is_empty() || criteria.len() % 2 != 0 || criteria[0].slice().len() < 256 {
            return None;
        }
        let mut query = Vec::with_capacity(key.criteria.len());
        let mut numeric = Vec::with_capacity(key.criteria.len());
        for text in &key.criteria {
            let crit = Crit::parse(text);
            if !crit.indexable() { return None; }
            numeric.push(crit.num.is_some());
            query.push(match crit.num {
                Some(n) => GroupValue::integer(n)?,
                None => GroupValue::Text(crit.text),
            });
        }
        let Some(slot) = self.entries.iter().position(|e| e.ranges == key.ranges && e.numeric == numeric) else {
            if self.entries.len() == 4 { self.entries.pop_front(); }
            self.entries.push_back(GroupedRange {
                ranges: key.ranges.clone(), numeric, groups: None, attempted: false,
            });
            return None;
        };
        let mut entry = self.entries.remove(slot)?;
        if !entry.attempted {
            entry.attempted = true;
            let columns: Vec<_> = criteria.chunks_exact(2).map(|pair| pair[0].slice()).collect();
            let amounts = sum.then(|| args[0].slice());
            let used: usize = self.entries.iter().filter_map(|e| e.groups.as_ref()).map(|g| g.len()).sum();
            entry.groups = entry.build_groups(&columns, amounts, 65_536 - used);
        }
        let result = entry.groups.as_ref().map(|g| FValue::number(g.get(&query).copied().unwrap_or(0.0)));
        self.entries.push_back(entry);
        result
    }
}

impl GroupedRange {
    fn build_groups(
        &self, columns: &[&[FValue]], amounts: Option<&[FValue]>, limit: usize,
    ) -> Option<std::collections::HashMap<Vec<GroupValue>, f64>> {
        let rows = columns.iter().map(|col| col.len()).min()?.min(amounts.map_or(usize::MAX, |v| v.len()));
        let mut groups = std::collections::HashMap::new();
        let mut key = Vec::with_capacity(columns.len());
        'row: for row in 0..rows {
            key.clear();
            for (col, &numeric) in columns.iter().zip(&self.numeric) {
                let value = &col[row];
                key.push(if numeric {
                    let Ok(n) = value.to_number() else { continue 'row };
                    GroupValue::integer(n)?
                } else {
                    let text = match value {
                        FValue::Text(s) => s.to_lowercase(),
                        FValue::Empty => String::new(),
                        FValue::Bool(b) => b.to_string(),
                        other => other.display().to_lowercase(),
                    };
                    GroupValue::Text(text)
                });
            }
            let add = amounts.map_or(1.0, |values| values[row].to_number().unwrap_or(0.0));
            if let Some(total) = groups.get_mut(&key) {
                *total += add;
            } else {
                if groups.len() >= limit { return None; }
                groups.insert(std::mem::take(&mut key), add);
            }
        }
        Some(groups)
    }
}

/// Matching row offsets for repeated COUNTIF and SUMIF equality queries.
#[derive(Default)]
pub(super) struct CountIndex {
    numbers: Vec<(f64, usize)>,
    text: std::collections::HashMap<String, Vec<usize>>,
}

#[test]
fn count_index_agrees_with_single_criterion_scan() {
    let data = vec![
        FValue::Empty, FValue::Text(String::new()), FValue::Number(0.0), FValue::Number(-0.0),
        FValue::Number(f64::EPSILON), FValue::Number(-f64::EPSILON), FValue::Number(1.5),
        FValue::Text(" 1.50 ".into()), FValue::Text("HELLO".into()), FValue::Text("Ä".into()),
        FValue::Text("ä".into()), FValue::Bool(true), FValue::Text("true".into()), FValue::Error(ErrKind::Value),
    ];
    let index = CountIndex::new(&data);
    for query in ["0","1.50","HELLO","ä","Ä","true","","NaN","inf","0.00000000000000022"] {
        let crit = Crit::parse(query);
        assert_eq!(index.count(&crit), data.iter().filter(|v| crit.matches(v)).count(), "{query}");
    }
    for query in ["a*", "a?", ">0", "<>x"] { assert!(!Crit::parse(query).indexable()); }
}

impl CountIndex {
    pub(super) fn new(data: &[FValue]) -> Self {
        let mut index = Self::default();
        for (row, value) in data.iter().enumerate() {
            let text = match value {
                FValue::Number(n) => {
                    if n.is_finite() { index.numbers.push((*n, row)); }
                    continue;
                }
                FValue::Text(s) => {
                    if let Ok(n) = s.trim().parse::<f64>() {
                        if n.is_finite() { index.numbers.push((n, row)); }
                    }
                    if s.is_ascii() { s.to_ascii_lowercase() } else { s.clone() }
                }
                FValue::Bool(b) => b.to_string(),
                FValue::Empty => String::new(),
                _ => continue,
            };
            index.text.entry(text).or_default().push(row);
        }
        index.numbers.sort_unstable_by(|a,b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        index
    }

    fn matching_offsets(&self, crit: &Crit) -> (&[(f64, usize)], &[usize]) {
        let Some(n) = crit.num else {
            return (&[], self.text.get(&crit.text).map_or(&[], Vec::as_slice));
        };
        if !n.is_finite() { return (&[], &[]); }
        let start = self.numbers.partition_point(|(v, _)| v - n < -f64::EPSILON);
        let end = self.numbers.partition_point(|(v, _)| v - n <= f64::EPSILON);
        (&self.numbers[start..end], &[])
    }

    pub(super) fn count(&self, crit: &Crit) -> usize {
        let (nums, text) = self.matching_offsets(crit);
        nums.len() + text.len()
    }

    pub(super) fn rows(&self, crit: &Crit) -> Option<impl Iterator<Item = usize> + '_> {
        let (nums, text) = self.matching_offsets(crit);
        // Values within an epsilon (or signed zeros) can span several sorted keys.
        // Preserve source order for error propagation and floating-point addition.
        if nums.first().map(|v| v.0.to_bits()) != nums.last().map(|v| v.0.to_bits()) {
            return None;
        }
        Some(nums.iter().map(|v| v.1).chain(text.iter().copied()))
    }
}

#[derive(Default)]
pub(super) struct CountIndexes {
    ranges: std::collections::HashMap<super::RangeKey, Option<CountIndex>>,
    indexed_values: usize,
}

impl CountIndexes {
    pub(super) fn get(&mut self, range: super::RangeKey, data: &[FValue], crit: &Crit) -> Option<&CountIndex> {
        const LIMIT: usize = 2_097_152;
        if !crit.indexable() || !(256..=LIMIT).contains(&data.len()) { return None; }
        if !self.ranges.contains_key(&range) {
            if self.ranges.len() >= 4096 {
                self.ranges.clear();
                self.indexed_values = 0;
            }
            self.ranges.insert(range, None);
            return None;
        }
        if self.ranges[&range].is_none() {
            if self.indexed_values + data.len() > LIMIT {
                self.ranges.clear();
                self.indexed_values = 0;
            }
            self.ranges.insert(range, Some(CountIndex::new(data)));
            self.indexed_values += data.len();
        }
        self.ranges.get(&range)?.as_ref()
    }
}

impl Crit {
    pub(super) fn indexable(&self) -> bool {
        self.op == "=" && !self.wild
    }

    pub(crate) fn parse(crit: &str) -> Crit {
        let c = crit.trim();
        let (op, rest) = if let Some(x) = c.strip_prefix(">=") {
            (">=", x)
        } else if let Some(x) = c.strip_prefix("<=") {
            ("<=", x)
        } else if let Some(x) = c.strip_prefix("<>") {
            ("<>", x)
        } else if let Some(x) = c.strip_prefix('>') {
            (">", x)
        } else if let Some(x) = c.strip_prefix('<') {
            ("<", x)
        } else if let Some(x) = c.strip_prefix('=') {
            ("=", x)
        } else {
            ("=", c)
        };
        let rest = rest.trim();
        let num = rest.parse::<f64>().ok();
        Crit {
            op,
            num,
            text: rest.to_lowercase(),
            wild: rest.contains('*') || rest.contains('?'),
        }
    }

    pub(crate) fn matches(&self, v: &FValue) -> bool {
        // 条件匹配里的空白有自己的规则（Excel 16.0 实测）：
        //   COUNTIF(空单元格,0)=0、(">0")=0、(">=0")=0 —— 数字条件默认跳过空白；
        //   COUNTIF(空单元格,"<>0")=1 —— 只有「不等于某值」把空白算进来；
        //   COUNTIF(空单元格,"")=1、("<>")=0。
        // 这跟比较运算 =A1=0 为 TRUE 是两套上下文，不能共用一套强制转换。
        if matches!(v, FValue::Empty) && self.num.is_some() {
            return self.op == "<>";
        }
        if let Some(t) = self.num {
            // Excel 16.0 实测：COUNTIF(range,">0") 只数真正的数字（文本 "5"、TRUE 都不算）；
            // COUNTIF(range,5) 同时数数字 5 和文本 "5"；"<>x" 把非数字、布尔、错误都算进来。
            let n = match v {
                FValue::Number(n) => *n,
                FValue::Text(s) if self.op == "=" || self.op == "<>" => match s.trim().parse::<f64>() {
                    Ok(n) => n,
                    Err(_) => return self.op == "<>",
                },
                _ => return self.op == "<>",
            };
            return match self.op {
                ">=" => n >= t,
                "<=" => n <= t,
                ">" => n > t,
                "<" => n < t,
                "<>" => (n - t).abs() > f64::EPSILON,
                _ => (n - t).abs() <= f64::EPSILON,
            };
        }
        let eq = match v {
            FValue::Text(s) => {
                if self.wild {
                    wildcard_ci(s, &self.text)
                } else if self.text.is_ascii() {
                    s.eq_ignore_ascii_case(&self.text)
                } else {
                    s == &self.text
                }
            }
            FValue::Empty => self.text.is_empty(),
            FValue::Bool(b) => (if *b { "true" } else { "false" }) == self.text,
            FValue::Number(n) => {
                let t = if n.fract() == 0.0 && n.abs() < 1e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                };
                t == self.text
            }
            _ => false,
        };
        match self.op {
            "<>" => !eq,
            ">" => !eq && !self.text.is_empty(),
            "<" => false,
            _ => eq,
        }
    }
    /// Legacy multi-condition functions coerce numeric text, blanks and booleans through
    /// to_number. Retain those rules when sharing the compiled criterion.
    pub(super) fn matches_coerced(&self, v: &FValue) -> bool {
        if let Some(target) = self.num {
            let Ok(n) = v.to_number() else { return self.op == "<>" };
            return match self.op {
                ">=" => n >= target, "<=" => n <= target, ">" => n > target, "<" => n < target,
                "<>" => (n - target).abs() > f64::EPSILON,
                _ => (n - target).abs() <= f64::EPSILON,
            };
        }
        let display;
        let text = match v {
            FValue::Text(s) => s.as_str(), FValue::Empty => "",
            FValue::Bool(true) => "true", FValue::Bool(false) => "false",
            _ => { display = v.display(); &display }
        };
        if self.wild && matches!(self.op, "=" | "<>") {
            let matched = if text.contains('Σ') {
                wildcard_ci(&text.to_lowercase(), &self.text)
            } else {
                wildcard_ci(text, &self.text)
            };
            return matched == (self.op == "=");
        }
        let order = super::value::compare_text(text, &self.text);
        match self.op {
            "<>" => !order.is_eq(), ">" => order.is_gt(), "<" => order.is_lt(),
            ">=" => !order.is_lt(), "<=" => !order.is_gt(), _ => order.is_eq(),
        }
    }

}

/// 大小写不敏感的通配符匹配，不分配字符串
fn wildcard_ci(s: &str, pat: &str) -> bool {
    let mut text = s.chars().flat_map(char::to_lowercase);
    let mut pattern = pat.chars();
    let mut star = None;
    loop {
        let next = text.clone().next();
        match pattern.next() {
            Some('*') => { star = Some((text.clone(), pattern.clone())); }
            Some(c) if next.is_some_and(|ch| c == '?' || ch == c) => { text.next(); }
            None if next.is_none() => return true,
            _ => {
                let Some((retry_text, retry_pattern)) = star.as_mut() else { return false };
                if retry_text.next().is_none() { return false; }
                text = retry_text.clone();
                pattern = retry_pattern.clone();
            }
        }
    }
}

pub(crate) fn match_criteria(v: &FValue, crit: &str) -> bool {
    Crit::parse(crit).matches_coerced(v)
}

pub(super) fn criteria_pairs(args: &[Arg]) -> Vec<(&[FValue], Crit)> {
    args.chunks_exact(2).filter_map(|pair| {
        let text = pair[1].first().to_text().ok()?;
        Some((pair[0].slice(), Crit::parse(&text)))
    }).collect()
}

// ---------------------------------------------------------------- 1-based 行列参数

pub(crate) fn index_arg(args: &[Arg], i: usize) -> Result<usize, ErrKind> {
    let n = num(args, i)?;
    if n < 1.0 {
        return Err(ErrKind::Value);
    }
    Ok(n as usize - 1)
}

pub(crate) fn range_dims(a: &Arg) -> (usize, usize) {
    match a {
        Arg::Range { rows, cols, .. } => (*rows, *cols),
        Arg::Val(FValue::Array(arr)) => (arr.rows, arr.cols),
        Arg::Val(_) => (1, 1),
    }
}

#[derive(Clone, Copy, Hash, Eq, PartialEq)]
pub(super) enum LookupKind {
    Number,
    Text,
    Bool,
}

impl LookupKind {
    pub(super) fn of(key: &FValue) -> Option<Self> {
        match key {
            FValue::Empty | FValue::Number(_) => Some(Self::Number),
            FValue::Text(_) => Some(Self::Text),
            FValue::Bool(_) => Some(Self::Bool),
            _ => None,
        }
    }

    /// Empty table cells, errors and other types have no position in this
    /// ordered sequence. A blank lookup key itself is numeric zero.
    pub(super) fn positions(self, data: &[FValue], len: usize, stride: usize) -> Option<Vec<usize>> {
        let mut positions = None;
        for i in 0..len {
            let eligible = data.get(i * stride)
                .is_some_and(|v| !matches!(v, FValue::Empty) && Self::of(v) == Some(self));
            match &mut positions {
                Some(indices) if eligible => Vec::push(indices, i),
                None if !eligible => positions = Some((0..i).collect()),
                _ => {}
            }
        }
        positions
    }
}

pub(super) fn search_sorted(
    data: &[FValue], len: usize, stride: usize, positions: Option<&[usize]>, needle: &FValue, ascending: bool,
) -> Option<usize> {
    let needle = if matches!(needle, FValue::Empty) { &FValue::Number(0.0) } else { needle };
    let index = |i| positions.map_or(i, |p| p[i]);
    let count = positions.map_or(len, |p| p.len());
    let (mut lo, mut hi) = (0, count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let order = data[index(mid) * stride].compare(needle).ok()?;
        if (ascending && !order.is_gt()) || (!ascending && order.is_gt()) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    // MATCH(...,-1) returns the first exact duplicate. For a missing key it
    // returns the last entry at the next greater value.
    if !ascending && lo < count && data[index(lo) * stride].compare(needle).ok()?.is_eq() {
        return Some(index(lo));
    }
    lo.checked_sub(1).map(index)
}

fn approximate_match(data: &[FValue], len: usize, stride: usize, needle: &FValue, ascending: bool) -> Option<usize> {
    let positions = LookupKind::of(needle)?.positions(data, len, stride);
    search_sorted(data, len, stride, positions.as_deref(), needle, ascending)
}

pub(crate) fn range_get(a: &Arg, r: usize, c: usize) -> FValue {
    match a {
        Arg::Range { values, rows, cols } => {
            if r >= *rows || c >= *cols {
                FValue::Error(ErrKind::Ref)
            } else {
                values.get(r * cols + c).cloned().unwrap_or(FValue::Empty)
            }
        }
        Arg::Val(FValue::Array(arr)) => {
            if r >= arr.rows || c >= arr.cols {
                FValue::Error(ErrKind::Ref)
            } else {
                arr.get(r, c)
            }
        }
        Arg::Val(v) => {
            if r == 0 && c == 0 {
                v.clone()
            } else {
                FValue::Error(ErrKind::Ref)
            }
        }
    }
}

// ---------------------------------------------------------------- 分发

/// Excel 的算术只保留 15 位有效数字：57*275.75*0.62 在二进制里是
/// 9745.004999999998，Excel 看到的是 9745.005，于是 ROUND 进位到 9745.01。
/// 先把中间结果收到 15 位有效数字，再按"四舍五入远离零"取整，才能对上。
fn round_sig15(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let e = x.abs().log10().floor() as i32;
    let p = 14 - e;
    if p.abs() > 300 {
        return x;
    }
    let m = 10f64.powi(p);
    let y = x * m;
    if !y.is_finite() {
        return x;
    }
    y.round() / m
}

pub fn call(name: &str, args: &[Arg]) -> FValue {
    call_or_else(name, args, &mut || FValue::Error(ErrKind::Name))
}

pub(super) fn call_or_else(name: &str, args: &[Arg], unknown: &mut dyn FnMut() -> FValue) -> FValue {
    match name {
        // ---------------- 数学 / 三角
        "SUM" => r(|| {
            let mut s = 0.0;
            for a in args {
                for v in a.slice() {
                    match v {
                        FValue::Number(n) => s += n,
                        FValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            Ok(FValue::number(s))
        }),
        "PRODUCT" => r(|| {
            let v = numbers(args, 0)?;
            if v.is_empty() {
                Ok(FValue::number(0.0))
            } else {
                Ok(FValue::number(v.iter().product()))
            }
        }),
        "ABS" => r(|| Ok(FValue::number(num(args, 0)?.abs()))),
        "SIGN" => r(|| Ok(FValue::number(num(args, 0)?.signum()))),
        "INT" => r(|| Ok(FValue::number(num(args, 0)?.floor()))),
        "TRUNC" => r(|| {
            let n = num(args, 0)?;
            let d = opt_num(args, 1, 0.0)?;
            let f = 10f64.powi(d as i32);
            Ok(FValue::number((n * f).trunc() / f))
        }),
        "ROUND" => r(|| {
            let n = num(args, 0)?;
            let d = int_arg(args, 1)?;
            let f = 10f64.powi(d as i32);
            Ok(FValue::number(round_sig15(n * f).round() / f))
        }),
        "ROUNDUP" => r(|| {
            let n = num(args, 0)?;
            let d = int_arg(args, 1)?;
            let f = 10f64.powi(d as i32);
            let x = round_sig15(n * f);
            Ok(FValue::number(if x >= 0.0 { x.ceil() } else { x.floor() } / f))
        }),
        "ROUNDDOWN" => r(|| {
            let n = num(args, 0)?;
            let d = int_arg(args, 1)?;
            let f = 10f64.powi(d as i32);
            Ok(FValue::number((n * f).trunc() / f))
        }),
        "MOD" => r(|| {
            let a = num(args, 0)?;
            let b = num(args, 1)?;
            if b == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number(a - b * (a / b).floor()))
        }),
        "QUOTIENT" => r(|| {
            let a = num(args, 0)?;
            let b = num(args, 1)?;
            if b == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number((a / b).trunc()))
        }),
        "POWER" => r(|| Ok(FValue::number(num(args, 0)?.powf(num(args, 1)?)))),
        "SQRT" => r(|| {
            let n = num(args, 0)?;
            if n < 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.sqrt()))
        }),
        "SQRTPI" => r(|| {
            let n = num(args, 0)?;
            if n < 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((n * std::f64::consts::PI).sqrt()))
        }),
        "EXP" => r(|| Ok(FValue::number(num(args, 0)?.exp()))),
        "LN" => r(|| {
            let n = num(args, 0)?;
            if n <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.ln()))
        }),
        "LOG10" => r(|| {
            let n = num(args, 0)?;
            if n <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.log10()))
        }),
        "LOG" => r(|| {
            let n = num(args, 0)?;
            let b = opt_num(args, 1, 10.0)?;
            if n <= 0.0 || b <= 0.0 || b == 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.log(b)))
        }),
        "PI" => FValue::number(std::f64::consts::PI),
        "SIN" => r(|| Ok(FValue::number(num(args, 0)?.sin()))),
        "COS" => r(|| Ok(FValue::number(num(args, 0)?.cos()))),
        "TAN" => r(|| Ok(FValue::number(num(args, 0)?.tan()))),
        "ASIN" => r(|| Ok(FValue::number(num(args, 0)?.asin()))),
        "ACOS" => r(|| Ok(FValue::number(num(args, 0)?.acos()))),
        "ATAN" => r(|| Ok(FValue::number(num(args, 0)?.atan()))),
        "ATAN2" => r(|| Ok(FValue::number(num(args, 0)?.atan2(num(args, 1)?)))),
        "SINH" => r(|| Ok(FValue::number(num(args, 0)?.sinh()))),
        "COSH" => r(|| Ok(FValue::number(num(args, 0)?.cosh()))),
        "TANH" => r(|| Ok(FValue::number(num(args, 0)?.tanh()))),
        "DEGREES" => r(|| Ok(FValue::number(num(args, 0)?.to_degrees()))),
        "RADIANS" => r(|| Ok(FValue::number(num(args, 0)?.to_radians()))),
        "FACT" => r(|| {
            let n = int_arg(args, 0)?;
            if n < 0 {
                return Err(ErrKind::Num);
            }
            let mut v = 1.0;
            for i in 2..=n {
                v *= i as f64;
            }
            Ok(FValue::number(v))
        }),
        "GCD" => r(|| {
            let v = numbers(args, 0)?;
            let mut g = 0i64;
            for x in v {
                g = gcd(g, x.abs() as i64);
            }
            Ok(FValue::number(g as f64))
        }),
        "LCM" => r(|| {
            let v = numbers(args, 0)?;
            let mut l = 1i64;
            for x in v {
                let x = x.abs() as i64;
                if x == 0 {
                    return Ok(FValue::number(0.0));
                }
                l = l / gcd(l, x) * x;
            }
            Ok(FValue::number(l as f64))
        }),
        "COMBIN" => r(|| {
            let n = int_arg(args, 0)?;
            let k = int_arg(args, 1)?;
            if n < 0 || k < 0 || k > n {
                return Err(ErrKind::Num);
            }
            let mut v = 1.0f64;
            for i in 0..k {
                v = v * (n - i) as f64 / (i + 1) as f64;
            }
            Ok(FValue::number(v))
        }),
        "CEILING" | "CEILING.MATH" => r(|| {
            let n = num(args, 0)?;
            let s = opt_num(args, 1, 1.0)?;
            if s == 0.0 {
                return Ok(FValue::number(0.0));
            }
            Ok(FValue::number((n / s).ceil() * s))
        }),
        "FLOOR" | "FLOOR.MATH" => r(|| {
            let n = num(args, 0)?;
            let s = opt_num(args, 1, 1.0)?;
            if s == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number((n / s).floor() * s))
        }),
        "MROUND" => r(|| {
            let n = num(args, 0)?;
            let s = num(args, 1)?;
            if s == 0.0 {
                return Ok(FValue::number(0.0));
            }
            Ok(FValue::number((n / s).round() * s))
        }),
        "EVEN" => r(|| {
            let n = num(args, 0)?;
            let v = (n / 2.0).ceil().abs() * 2.0;
            Ok(FValue::number(if n < 0.0 { -v } else { v }))
        }),
        "ODD" => r(|| {
            let n = num(args, 0)?;
            let mut v = (n.abs() / 2.0).ceil() * 2.0 - 1.0;
            if v == 0.0 {
                v = 1.0;
            }
            Ok(FValue::number(if n < 0.0 { -v } else { v }))
        }),
        "RAND" => FValue::number(rand_f64()),
        "RANDBETWEEN" => r(|| {
            let a = int_arg(args, 0)?;
            let b = int_arg(args, 1)?;
            if a > b {
                return Err(ErrKind::Num);
            }
            let span = (b - a + 1) as f64;
            Ok(FValue::number((a as f64 + (rand_f64() * span).floor()).min(b as f64)))
        }),
        "SUMSQ" => r(|| Ok(FValue::number(numbers(args, 0)?.iter().map(|x| x * x).sum()))),
        "SUMPRODUCT" => r(|| {
            // 各参数必须同形（标量 1×1 可参与），只比元素个数会把 2×2 与 1×4 当成一样
            let grids: Vec<_> = args
                .iter()
                .map(|a| {
                    let (rows, cols) = range_dims(a);
                    (rows, cols, a.slice())
                })
                .collect();
            let shape = grids
                .iter()
                .map(|(r, c, _)| (*r, *c))
                .find(|(r, c)| *r != 1 || *c != 1);
            let (rows, cols) = shape.unwrap_or((1, 1));
            for (r, c, _) in &grids {
                let ok = (*r == 1 && *c == 1) || (*r == rows && *c == cols);
                if !ok {
                    return Err(ErrKind::Value);
                }
            }
            let mut sum = 0.0;
            for i in 0..rows * cols {
                let mut p = 1.0;
                for (r, c, data) in &grids {
                    let idx = if *r == 1 && *c == 1 { 0 } else { i };
                    p *= data[idx].to_number().unwrap_or(0.0);
                }
                sum += p;
            }
            Ok(FValue::number(sum))
        }),
        "SUMIF" => r(|| {
            let crit = Crit::parse(&text(args, 1)?);
            for reference in [args.first(), args.get(2)].into_iter().flatten() {
                if let Arg::Val(FValue::Error(error)) = reference {
                    return Err(*error);
                }
            }
            let target = args.first().map(|a| a.slice()).unwrap_or(&[]);
            let sum_range = args.get(2).map(|a| a.slice()).unwrap_or(target);
            let mut s = 0.0;
            for (i, v) in target.iter().enumerate() {
                if crit.matches(v) {
                    if let Some(sv) = sum_range.get(i) {
                        match sv {
                            FValue::Number(n) => s += n,
                            FValue::Error(e) => return Err(*e),
                            _ => {}
                        }
                    }
                }
            }
            Ok(FValue::number(s))
        }),
        "SUMIFS" => r(|| {
            let sum_range = args.first().map(|a| a.slice()).unwrap_or(&[]);
            let mut s = 0.0;
            let pairs = criteria_pairs(args.get(1..).unwrap_or_default());
            for (i, v) in sum_range.iter().enumerate() {
                if pairs.iter().all(|(rg, c)| rg.get(i).is_some_and(|x| c.matches_coerced(x))) {
                    if let Ok(n) = v.to_number() {
                        s += n;
                    }
                }
            }
            Ok(FValue::number(s))
        }),
        "SUBTOTAL" => r(|| {
            let f = int_arg(args, 0)?;
            let base = f % 100;
            let sub = &args[1.min(args.len())..];
            Ok(match base {
                1 => call("AVERAGE", sub),
                2 => call("COUNT", sub),
                3 => call("COUNTA", sub),
                4 => call("MAX", sub),
                5 => call("MIN", sub),
                6 => call("PRODUCT", sub),
                9 => call("SUM", sub),
                7 => call("STDEV", sub),
                8 => call("STDEVP", sub),
                10 => call("VAR", sub),
                11 => call("VARP", sub),
                _ => FValue::Error(ErrKind::Value),
            })
        }),

        // ---------------- 统计
        "AVERAGE" => r(|| {
            let mut s = 0.0;
            let mut n = 0usize;
            for a in args {
                for v in a.slice() {
                    match v {
                        FValue::Number(x) => {
                            s += x;
                            n += 1;
                        }
                        FValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            if n == 0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number(s / n as f64))
        }),
        "AVERAGEA" => r(|| {
            let vals = flatten_values(args, 0);
            if vals.is_empty() {
                return Err(ErrKind::Div0);
            }
            let mut s = 0.0;
            for v in &vals {
                s += match v {
                    FValue::Text(_) => 0.0,
                    FValue::Bool(b) => if *b { 1.0 } else { 0.0 },
                    _ => v.to_number().unwrap_or(0.0),
                };
            }
            Ok(FValue::number(s / vals.len() as f64))
        }),
        "COUNT" => r(|| {
            let mut n = 0usize;
            for a in args {
                // 直接写在参数里的 TRUE/FALSE 要计数；引用区域里的逻辑值不计数
                let scalar_bool = matches!(a, Arg::Val(FValue::Bool(_)));
                if scalar_bool {
                    n += 1;
                    continue;
                }
                for v in a.slice() {
                    if matches!(v, FValue::Number(_)) {
                        n += 1;
                    }
                }
            }
            Ok(FValue::number(n as f64))
        }),
        "COUNTA" => r(|| {
            let n = args
                .iter()
                .flat_map(|a| a.slice())
                .filter(|v| !matches!(v, FValue::Empty))
                .count();
            Ok(FValue::number(n as f64))
        }),
        "COUNTBLANK" => r(|| {
            let n = args
                .iter()
                .flat_map(|a| a.slice())
                .filter(|v| match v {
                    FValue::Empty => true,
                    FValue::Text(s) => s.is_empty(),
                    _ => false,
                })
                .count();
            Ok(FValue::number(n as f64))
        }),
        "COUNTIF" => r(|| {
            let crit = Crit::parse(&text(args, 1)?);
            let mut n = 0usize;
            if let Some(a) = args.first() {
                for v in a.slice() {
                    if crit.matches(v) {
                        n += 1;
                    }
                }
            }
            Ok(FValue::number(n as f64))
        }),
        "COUNTIFS" => r(|| {
            let mut count = 0usize;
            let pairs = criteria_pairs(args);
            let len = pairs.iter().map(|(r, _)| r.len()).min().unwrap_or(0);
            for i in 0..len {
                if pairs.iter().all(|(rg, c)| rg.get(i).is_some_and(|x| c.matches_coerced(x))) {
                    count += 1;
                }
            }
            Ok(FValue::number(count as f64))
        }),
        "AVERAGEIF" => r(|| {
            let crit = Crit::parse(&text(args, 1)?);
            let target = args.first().map(|a| a.slice()).unwrap_or(&[]);
            let vals = args.get(2).map(|a| a.slice()).unwrap_or(target);
            let mut s = 0.0;
            let mut n = 0.0;
            for (i, v) in target.iter().enumerate() {
                if crit.matches_coerced(v) {
                    if let Some(Ok(x)) = vals.get(i).map(|x| x.to_number()) {
                        s += x;
                        n += 1.0;
                    }
                }
            }
            if n == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number(s / n))
        }),
        "MAX" => r(|| {
            let mut m = f64::NEG_INFINITY;
            let mut any = false;
            for a in args {
                for v in a.slice() {
                    match v {
                        FValue::Number(n) => {
                            any = true;
                            if *n > m {
                                m = *n;
                            }
                        }
                        FValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            Ok(FValue::number(if any { m } else { 0.0 }))
        }),
        "MIN" => r(|| {
            let mut m = f64::INFINITY;
            let mut any = false;
            for a in args {
                for v in a.slice() {
                    match v {
                        FValue::Number(n) => {
                            any = true;
                            if *n < m {
                                m = *n;
                            }
                        }
                        FValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            Ok(FValue::number(if any { m } else { 0.0 }))
        }),
        "MAXA" | "MINA" => r(|| {
            let vals = flatten_values(args, 0);
            let mut best = if name == "MAXA" { f64::NEG_INFINITY } else { f64::INFINITY };
            for v in vals {
                let x = match &v {
                    FValue::Bool(b) => if *b { 1.0 } else { 0.0 },
                    FValue::Text(_) => 0.0,
                    other => other.to_number().unwrap_or(0.0),
                };
                best = if name == "MAXA" { best.max(x) } else { best.min(x) };
            }
            Ok(FValue::number(if best.is_infinite() { 0.0 } else { best }))
        }),
        "MEDIAN" => r(|| {
            let mut v = numbers(args, 0)?;
            if v.is_empty() {
                return Err(ErrKind::Num);
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let n = v.len();
            Ok(FValue::number(if n % 2 == 1 {
                v[n / 2]
            } else {
                (v[n / 2 - 1] + v[n / 2]) / 2.0
            }))
        }),
        "MODE" => r(|| {
            let v = numbers(args, 0)?;
            let mut best = (f64::NAN, 0usize);
            for &x in &v {
                let c = v.iter().filter(|y| (**y - x).abs() < 1e-12).count();
                if c > best.1 {
                    best = (x, c);
                }
            }
            if best.1 < 2 {
                return Err(ErrKind::Na);
            }
            Ok(FValue::number(best.0))
        }),
        "LARGE" | "SMALL" => r(|| {
            let mut v = first_numbers(args)?;
            let k = int_arg(args, 1)?;
            if k < 1 || k as usize > v.len() {
                return Err(ErrKind::Num);
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let n = v.len();
            let idx = if name == "LARGE" { n - k as usize } else { k as usize - 1 };
            Ok(FValue::number(v[idx]))
        }),
        "RANK" | "RANK.EQ" => r(|| {
            let x = num(args, 0)?;
            let v = args.get(1).map(|a| a.numbers()).transpose()?.unwrap_or_default();
            let asc = opt_num(args, 2, 0.0)? != 0.0;
            let mut sorted = v.clone();
            sorted.sort_by(|a, b| if asc { a.partial_cmp(b).unwrap() } else { b.partial_cmp(a).unwrap() });
            match sorted.iter().position(|y| (y - x).abs() < 1e-12) {
                Some(i) => Ok(FValue::number((i + 1) as f64)),
                None => Err(ErrKind::Na),
            }
        }),
        "STDEV" | "STDEV.S" | "STDEVP" | "STDEV.P" => r(|| {
            let v = numbers(args, 0)?;
            let sample = name != "STDEVP" && name != "STDEV.P";
            Ok(FValue::number(std_dev(&v, sample)?))
        }),
        "VAR" | "VAR.S" | "VARP" | "VAR.P" => r(|| {
            let v = numbers(args, 0)?;
            let sample = name != "VARP" && name != "VAR.P";
            let sd = std_dev(&v, sample)?;
            Ok(FValue::number(sd * sd))
        }),
        "AVEDEV" => r(|| {
            let v = numbers(args, 0)?;
            if v.is_empty() {
                return Err(ErrKind::Num);
            }
            let m = v.iter().sum::<f64>() / v.len() as f64;
            Ok(FValue::number(v.iter().map(|x| (x - m).abs()).sum::<f64>() / v.len() as f64))
        }),
        "DEVSQ" => r(|| {
            let v = numbers(args, 0)?;
            let m = v.iter().sum::<f64>() / v.len() as f64;
            Ok(FValue::number(v.iter().map(|x| (x - m).powi(2)).sum::<f64>()))
        }),
        "GEOMEAN" => r(|| {
            let v = numbers(args, 0)?;
            if v.is_empty() || v.iter().any(|x| *x <= 0.0) {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((v.iter().map(|x| x.ln()).sum::<f64>() / v.len() as f64).exp()))
        }),
        "HARMEAN" => r(|| {
            let v = numbers(args, 0)?;
            if v.is_empty() || v.iter().any(|x| *x == 0.0) {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(v.len() as f64 / v.iter().map(|x| 1.0 / x).sum::<f64>()))
        }),
        "PERCENTILE" | "PERCENTILE.INC" => r(|| {
            let mut v = first_numbers(args)?;
            let p = num(args, 1)?;
            if v.is_empty() || !(0.0..=1.0).contains(&p) {
                return Err(ErrKind::Num);
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            Ok(FValue::number(percentile(&v, p)))
        }),
        "QUARTILE" | "QUARTILE.INC" => r(|| {
            let mut v = first_numbers(args)?;
            let q = int_arg(args, 1)?;
            if !(0..=4).contains(&q) {
                return Err(ErrKind::Num);
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            Ok(FValue::number(percentile(&v, q as f64 / 4.0)))
        }),
        "CORREL" => r(|| {
            let a = args.first().map(|x| x.numbers()).transpose()?.unwrap_or_default();
            let b = args.get(1).map(|x| x.numbers()).transpose()?.unwrap_or_default();
            let n = a.len().min(b.len());
            if n < 2 {
                return Err(ErrKind::Div0);
            }
            let ma = a[..n].iter().sum::<f64>() / n as f64;
            let mb = b[..n].iter().sum::<f64>() / n as f64;
            let mut num = 0.0;
            let mut da = 0.0;
            let mut db = 0.0;
            for i in 0..n {
                let x = a[i] - ma;
                let y = b[i] - mb;
                num += x * y;
                da += x * x;
                db += y * y;
            }
            if da == 0.0 || db == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number(num / (da.sqrt() * db.sqrt())))
        }),
        "SLOPE" => r(|| {
            let y = args.first().map(|x| x.numbers()).transpose()?.unwrap_or_default();
            let x = args.get(1).map(|x| x.numbers()).transpose()?.unwrap_or_default();
            let n = x.len().min(y.len());
            if n < 2 {
                return Err(ErrKind::Div0);
            }
            let mx = x[..n].iter().sum::<f64>() / n as f64;
            let my = y[..n].iter().sum::<f64>() / n as f64;
            let mut a = 0.0;
            let mut b = 0.0;
            for i in 0..n {
                a += (x[i] - mx) * (y[i] - my);
                b += (x[i] - mx).powi(2);
            }
            if b == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number(a / b))
        }),
        "INTERCEPT" => r(|| {
            let y = args.first().map(|x| x.numbers()).transpose()?.unwrap_or_default();
            let x = args.get(1).map(|x| x.numbers()).transpose()?.unwrap_or_default();
            let n = x.len().min(y.len());
            if n < 2 {
                return Err(ErrKind::Div0);
            }
            let mx = x[..n].iter().sum::<f64>() / n as f64;
            let my = y[..n].iter().sum::<f64>() / n as f64;
            let mut a = 0.0;
            let mut b = 0.0;
            for i in 0..n {
                a += (x[i] - mx) * (y[i] - my);
                b += (x[i] - mx).powi(2);
            }
            if b == 0.0 {
                return Err(ErrKind::Div0);
            }
            let slope = a / b;
            Ok(FValue::number(my - slope * mx))
        }),

        // ---------------- 文本
        "LEN" => r(|| Ok(FValue::number(text(args, 0)?.chars().count() as f64))),
        "LEFT" => r(|| {
            let s = text(args, 0)?;
            let n = opt_num(args, 1, 1.0)?.max(0.0) as usize;
            Ok(FValue::Text(s.chars().take(n).collect()))
        }),
        "RIGHT" => r(|| {
            let s = text(args, 0)?;
            let n = opt_num(args, 1, 1.0)?.max(0.0) as usize;
            let len = s.chars().count();
            let skip = len.saturating_sub(n);
            Ok(FValue::Text(s.chars().skip(skip).collect()))
        }),
        "MID" => r(|| {
            let s = text(args, 0)?;
            let start = num(args, 1)?.max(1.0) as usize;
            let n = num(args, 2)?.max(0.0) as usize;
            Ok(FValue::Text(s.chars().skip(start - 1).take(n).collect()))
        }),
        "LOWER" => r(|| Ok(FValue::Text(text(args, 0)?.to_lowercase()))),
        "UPPER" => r(|| Ok(FValue::Text(text(args, 0)?.to_uppercase()))),
        "PROPER" => r(|| {
            let s = text(args, 0)?;
            let mut out = String::new();
            let mut cap = true;
            for ch in s.chars() {
                if ch.is_alphabetic() {
                    if cap {
                        out.extend(ch.to_uppercase());
                    } else {
                        out.extend(ch.to_lowercase());
                    }
                    cap = false;
                } else {
                    out.push(ch);
                    cap = true;
                }
            }
            Ok(FValue::Text(out.as_str().into()))
        }),
        "TRIM" => r(|| {
            let s = text(args, 0)?;
            let parts: Vec<&str> = s.split_whitespace().collect();
            Ok(FValue::Text(parts.join(" ")))
        }),
        "CONCAT" | "CONCATENATE" => r(|| {
            let mut out = String::new();
            for a in args {
                for v in a.values() {
                    out.push_str(&v.display());
                }
            }
            Ok(FValue::Text(out.as_str().into()))
        }),
        "TEXTJOIN" => r(|| {
            let sep = text(args, 0)?;
            let ignore_empty = arg(args, 1).to_bool()?;
            let mut parts = Vec::new();
            for a in args.iter().skip(2) {
                for v in a.values() {
                    let s = v.display();
                    if ignore_empty && s.is_empty() {
                        continue;
                    }
                    parts.push(s);
                }
            }
            Ok(FValue::Text(parts.join(&sep)))
        }),
        "REPT" => r(|| {
            let s = text(args, 0)?;
            let n = num(args, 1)?.max(0.0) as usize;
            if s.len() * n > 32_767 {
                return Err(ErrKind::Value);
            }
            Ok(FValue::Text(s.repeat(n)))
        }),
        "SUBSTITUTE" => r(|| {
            let s = text(args, 0)?;
            let old = text(args, 1)?;
            let new = text(args, 2)?;
            if old.is_empty() {
                return Ok(FValue::Text(s));
            }
            match args.get(3) {
                None => Ok(FValue::Text(s.replace(&old, &new))),
                Some(_) => {
                    let nth = num(args, 3)?.max(1.0) as usize;
                    let mut count = 0;
                    let mut out = String::new();
                    let mut rest = s.as_str();
                    while let Some(pos) = rest.find(&old) {
                        count += 1;
                        out.push_str(&rest[..pos]);
                        if count == nth {
                            out.push_str(&new);
                            rest = &rest[pos + old.len()..];
                            out.push_str(rest);
                            return Ok(FValue::Text(out.as_str().into()));
                        }
                        out.push_str(&old);
                        rest = &rest[pos + old.len()..];
                    }
                    out.push_str(rest);
                    Ok(FValue::Text(out.as_str().into()))
                }
            }
        }),
        "REPLACE" => r(|| {
            let s = text(args, 0)?;
            let start = num(args, 1)?.max(1.0) as usize;
            let count = num(args, 2)?.max(0.0) as usize;
            let new = text(args, 3)?;
            let chars: Vec<char> = s.chars().collect();
            let mut out: String = chars.iter().take(start - 1).collect();
            out.push_str(&new);
            out.extend(chars.iter().skip((start - 1 + count).min(chars.len())));
            Ok(FValue::Text(out.as_str().into()))
        }),
        "FIND" | "SEARCH" => r(|| {
            let needle = text(args, 0)?;
            let hay = text(args, 1)?;
            let start = opt_num(args, 2, 1.0)?.max(1.0) as usize;
            let hay_part: String = hay.chars().skip(start - 1).collect();
            let found = if name == "FIND" {
                hay_part.find(&needle).map(|p| (p, needle.len()))
            } else {
                hay_part
                    .to_lowercase()
                    .find(&needle.to_lowercase())
                    .map(|p| (p, needle.len()))
            };
            match found {
                Some((p, _)) => {
                    let prefix_chars = hay_part.chars().take(p).count();
                    Ok(FValue::number((start + prefix_chars) as f64))
                }
                None => Err(ErrKind::Value),
            }
        }),
        "EXACT" => r(|| Ok(FValue::Bool(text(args, 0)? == text(args, 1)?))),
        "CHAR" => r(|| {
            let n = int_arg(args, 0)?;
            if !(1..=255).contains(&n) {
                return Err(ErrKind::Value);
            }
            Ok(FValue::Text(((n as u8) as char).to_string()))
        }),
        "CODE" => r(|| {
            let s = text(args, 0)?;
            let c = s.chars().next().ok_or(ErrKind::Value)?;
            Ok(FValue::number(c as u32 as f64))
        }),
        "VALUE" => r(|| {
            let s = text(args, 0)?;
            let t = s.replace(',', "").replace('¥', "").replace('$', "");
            let t = t.trim();
            if let Ok(n) = t.parse::<f64>() {
                return Ok(FValue::number(n));
            }
            if let Some(p) = t.strip_suffix('%') {
                if let Ok(n) = p.trim().parse::<f64>() {
                    return Ok(FValue::number(n / 100.0));
                }
            }
            Err(ErrKind::Value)
        }),
        "TEXT" => r(|| {
            let v = arg(args, 0);
            let fmt = text(args, 1)?;
            Ok(FValue::Text(format_value(&v, &fmt)))
        }),
        "T" => r(|| match arg(args, 0) {
            FValue::Text(s) => Ok(FValue::Text(s)),
            _ => Ok(FValue::Text(String::new())),
        }),

        // ---------------- 逻辑
        "TRUE" => FValue::Bool(true),
        "FALSE" => FValue::Bool(false),
        "NOT" => r(|| Ok(FValue::Bool(!arg(args, 0).to_bool()?))),
        "AND" => r(|| {
            let mut any = false;
            for a in args {
                for v in a.values() {
                    if matches!(v, FValue::Empty) {
                        continue;
                    }
                    any = true;
                    if !v.to_bool()? {
                        return Ok(FValue::Bool(false));
                    }
                }
            }
            if !any {
                return Err(ErrKind::Value);
            }
            Ok(FValue::Bool(true))
        }),
        "OR" => r(|| {
            let mut any = false;
            for a in args {
                for v in a.values() {
                    if matches!(v, FValue::Empty) {
                        continue;
                    }
                    any = true;
                    if v.to_bool()? {
                        return Ok(FValue::Bool(true));
                    }
                }
            }
            if !any {
                return Err(ErrKind::Value);
            }
            Ok(FValue::Bool(false))
        }),
        "XOR" => r(|| {
            let mut t = false;
            for a in args {
                for v in a.values() {
                    if matches!(v, FValue::Empty) {
                        continue;
                    }
                    t ^= v.to_bool()?;
                }
            }
            Ok(FValue::Bool(t))
        }),
        "IF" => {
            let cond = arg(args, 0);
            // IF 需要短路：只求对应分支的值
            match cond.to_bool() {
                Ok(true) => arg(args, 1),
                Ok(false) => {
                    if args.len() >= 3 {
                        arg(args, 2)
                    } else {
                        FValue::Bool(false)
                    }
                }
                Err(e) => FValue::Error(e),
            }
        }
        "IFERROR" => {
            let v = arg(args, 0);
            if v.is_error() {
                arg(args, 1)
            } else {
                v
            }
        }
        "IFNA" => {
            let v = arg(args, 0);
            if matches!(v, FValue::Error(ErrKind::Na)) {
                arg(args, 1)
            } else {
                v
            }
        }
        "IFS" => {
            for pair in args.chunks(2) {
                if pair.len() < 2 {
                    break;
                }
                match pair[0].first().to_bool() {
                    Ok(true) => return pair[1].first(),
                    Ok(false) => continue,
                    Err(e) => return FValue::Error(e),
                }
            }
            FValue::Error(ErrKind::Na)
        }
        "SWITCH" => {
            let target = arg(args, 0);
            let mut i = 1;
            while i + 1 < args.len() {
                if let Ok(std::cmp::Ordering::Equal) = target.compare(&arg(args, i)) {
                    return arg(args, i + 1);
                }
                i += 2;
            }
            if i < args.len() {
                arg(args, i)
            } else {
                FValue::Error(ErrKind::Na)
            }
        }

        // ---------------- 查找
        "ROW" => r(|| {
            if args.is_empty() {
                return Err(ErrKind::Value);
            }
            Ok(FValue::number(1.0))
        }),
        "COLUMN" => r(|| {
            if args.is_empty() {
                return Err(ErrKind::Value);
            }
            Ok(FValue::number(1.0))
        }),
        "ROWS" => {
            let (rows, _) = range_dims(args.first().unwrap_or(&Arg::Val(FValue::Empty)));
            FValue::number(rows as f64)
        }
        "COLUMNS" => {
            let (_, cols) = range_dims(args.first().unwrap_or(&Arg::Val(FValue::Empty)));
            FValue::number(cols as f64)
        }
        "CHOOSE" => r(|| {
            let i = index_arg(args, 0)?;
            Ok(arg(args, i + 1))
        }),
        "VLOOKUP" | "HLOOKUP" => r(|| {
            let needle = arg(args, 0);
            if let FValue::Error(e) = needle {
                return Err(e);
            }
            let table = args.get(1).ok_or(ErrKind::Value)?;
            let idx = index_arg(args, 2)?;
            let approx = opt_bool(args, 3, true);
            let (rows, cols) = range_dims(table);
            let (n, limit) = if name == "VLOOKUP" { (rows, cols) } else { (cols, rows) };
            if idx >= limit {
                return Err(ErrKind::Ref);
            }
            let stride = if name == "VLOOKUP" { cols } else { 1 };
            let data = table.slice();
            let best = if approx {
                approximate_match(data, n, stride, &needle, true)
            } else {
                (0..n).find(|&i| match (data.get(i * stride).unwrap_or(&FValue::Empty), &needle) {
                    (FValue::Text(a), FValue::Text(b)) if is_wildcard(b) => wildcard_match_ci(b, a),
                    (cell, key) => matches!(cell.compare(key), Ok(std::cmp::Ordering::Equal)),
                })
            };
            match best {
                Some(i) => Ok(if name == "VLOOKUP" {
                    range_get(table, i, idx)
                } else {
                    range_get(table, idx, i)
                }),
                None => Err(ErrKind::Na),
            }
        }),
        "MATCH" | "XMATCH" => r(|| {
            let needle = arg(args, 0);
            if let FValue::Error(e) = needle {
                return Err(e);
            }
            let lookup = args.get(1).ok_or(ErrKind::Value)?;
            let match_type = opt_num(args, 2, if name == "MATCH" { 1.0 } else { 0.0 })?;
            let vals = lookup.slice();
            if match_type == 0.0 {
                for (i, v) in vals.iter().enumerate() {
                    let hit = match (v, &needle) {
                        (FValue::Text(a), FValue::Text(b)) if is_wildcard(b) => {
                            wildcard_match_ci(b, a)
                        }
                        _ => matches!(v.compare(&needle), Ok(std::cmp::Ordering::Equal)),
                    };
                    if hit {
                        return Ok(FValue::number((i + 1) as f64));
                    }
                }
                return Err(ErrKind::Na);
            }
            if name == "MATCH" {
                return approximate_match(vals, vals.len(), 1, &needle, match_type > 0.0)
                    .map(|i| FValue::number((i + 1) as f64)).ok_or(ErrKind::Na);
            }
            let mut last: Option<usize> = None;
            for (i, v) in vals.iter().enumerate() {
                if let Ok(o) = v.compare(&needle) {
                    if match_type > 0.0 && o != std::cmp::Ordering::Greater {
                        last = Some(i);
                    } else if match_type < 0.0 && o != std::cmp::Ordering::Less {
                        last = Some(i);
                    }
                }
            }
            match last {
                Some(i) => Ok(FValue::number((i + 1) as f64)),
                None => Err(ErrKind::Na),
            }
        }),
        "XLOOKUP" => r(|| {
            let needle = arg(args, 0);
            if let FValue::Error(e) = needle {
                return Err(e);
            }
            let lookup = args.get(1).ok_or(ErrKind::Value)?;
            let ret = args.get(2).ok_or(ErrKind::Value)?;
            let vals = lookup.values();
            let (rows, cols) = range_dims(ret);
            let vertical = rows >= cols;
            // 第 5 参数 match_mode：0 精确、2 通配符、-1/1 近似（近似未实现，按精确处理）
            let match_mode = if args.len() > 4 { opt_num(args, 4, 0.0)? } else { 0.0 };
            for (i, v) in vals.iter().enumerate() {
                let hit = match (v, &needle) {
                    (FValue::Text(a), FValue::Text(b))
                        if match_mode == 2.0 && is_wildcard(b) =>
                    {
                        wildcard_match_ci(b, a)
                    }
                    _ => matches!(v.compare(&needle), Ok(std::cmp::Ordering::Equal)),
                };
                if hit {
                    return Ok(if vertical { range_get(ret, i, 0) } else { range_get(ret, 0, i) });
                }
            }
            if let Some(4) = Some(args.len()).filter(|_| args.len() > 3) {
                let _ = 4;
                let fallback = arg(args, 3);
                if !matches!(fallback, FValue::Empty) {
                    return Ok(fallback);
                }
            }
            Err(ErrKind::Na)
        }),
        "INDEX" => r(|| {
            let area = args.first().ok_or(ErrKind::Value)?;
            let row = num(args, 1)?.max(0.0) as usize;
            let col = if args.len() > 2 { num(args, 2)?.max(0.0) as usize } else { 0 };
            let (rows, cols) = range_dims(area);
            if row == 0 && col > 0 && col <= cols {
                return Ok(if rows == 1 { range_get(area, 0, col - 1) } else { range_get(area, 0, col - 1) });
            }
            if col == 0 {
                return Ok(range_get(area, row.saturating_sub(1), 0));
            }
            Ok(range_get(area, row.saturating_sub(1), col.saturating_sub(1)))
        }),
        "LOOKUP" => r(|| {
            let needle = arg(args, 0);
            let lookup_vals = args.get(1).ok_or(ErrKind::Value)?.values();
            let result_vals = args
                .get(2)
                .map(|a| a.values())
                .unwrap_or_else(|| lookup_vals.clone());
            let mut last: Option<usize> = None;
            for (i, v) in lookup_vals.iter().enumerate() {
                if let Ok(o) = v.compare(&needle) {
                    if o != std::cmp::Ordering::Greater {
                        last = Some(i);
                    }
                }
            }
            match last {
                Some(i) => Ok(result_vals.get(i).cloned().unwrap_or(FValue::Error(ErrKind::Na))),
                None => Err(ErrKind::Na),
            }
        }),

        // ---------------- 日期时间
        "TODAY" => FValue::number(now_serial().floor()),
        "NOW" => FValue::number(now_serial()),
        "DATE" => r(|| {
            let y = int_arg(args, 0)? as i32;
            let m = int_arg(args, 1)? as i32;
            let d = int_arg(args, 2)?;
            // 允许月份、日期溢出（Excel 行为）
            let (y2, m2) = (y + (m - 1).div_euclid(12), (m - 1).rem_euclid(12) + 1);
            let m2 = m2 as u32;
            let base = date_to_serial(y2, m2, 1).ok_or(ErrKind::Num)?;
            Ok(FValue::number(base + d as f64 - 1.0))
        }),
        "TIME" => r(|| {
            let h = num(args, 0)?;
            let m = num(args, 1)?;
            let s = num(args, 2)?;
            Ok(FValue::number(
                (h * 3600.0 + m * 60.0 + s) / 86_400.0,
            ))
        }),
        "YEAR" => r(|| {
            let (y, _, _) = serial_to_date(num(args, 0)?).ok_or(ErrKind::Num)?;
            Ok(FValue::number(y as f64))
        }),
        "MONTH" => r(|| {
            let (_, m, _) = serial_to_date(num(args, 0)?).ok_or(ErrKind::Num)?;
            Ok(FValue::number(m as f64))
        }),
        "DAY" => r(|| {
            let (_, _, d) = serial_to_date(num(args, 0)?).ok_or(ErrKind::Num)?;
            Ok(FValue::number(d as f64))
        }),
        "HOUR" => r(|| {
            let s = num(args, 0)?.rem_euclid(1.0);
            Ok(FValue::number((s * 24.0).floor()))
        }),
        "MINUTE" => r(|| {
            let s = num(args, 0)?.rem_euclid(1.0);
            Ok(FValue::number(((s * 24.0 * 60.0) % 60.0).floor()))
        }),
        "SECOND" => r(|| {
            let s = num(args, 0)?.rem_euclid(1.0);
            Ok(FValue::number(((s * 86_400.0) % 60.0).floor()))
        }),
        "WEEKDAY" => r(|| {
            let serial = num(args, 0)?;
            let t = opt_num(args, 1, 1.0)? as i64;
            let days = (serial.floor() as i64 + 6) % 7; // 0=Sunday
            let v = match t {
                1 => days + 1,
                2 => (days + 6) % 7 + 1,
                3 => (days + 6) % 7,
                _ => days + 1,
            };
            Ok(FValue::number(v as f64))
        }),
        "EDATE" => r(|| {
            let serial = num(args, 0)?;
            let months = int_arg(args, 1)?;
            let (y, m, d) = serial_to_date(serial).ok_or(ErrKind::Num)?;
            let total = y as i64 * 12 + (m as i64 - 1) + months;
            let (y2, m2) = (total.div_euclid(12) as i32, (total.rem_euclid(12) + 1) as u32);
            let d2 = d.min(days_in_month(y2, m2));
            Ok(FValue::number(date_to_serial(y2, m2, d2).ok_or(ErrKind::Num)?))
        }),
        "EOMONTH" => r(|| {
            let serial = num(args, 0)?;
            let months = int_arg(args, 1)?;
            let (y, m, _) = serial_to_date(serial).ok_or(ErrKind::Num)?;
            let total = y as i64 * 12 + (m as i64 - 1) + months;
            let (y2, m2) = (total.div_euclid(12) as i32, (total.rem_euclid(12) + 1) as u32);
            Ok(FValue::number(
                date_to_serial(y2, m2, days_in_month(y2, m2)).ok_or(ErrKind::Num)?,
            ))
        }),
        "DAYS" => r(|| Ok(FValue::number(num(args, 0)?.floor() - num(args, 1)?.floor()))),
        "DATEDIF" => r(|| {
            let a = num(args, 0)?.floor();
            let b = num(args, 1)?.floor();
            let unit = text(args, 2)?.to_ascii_uppercase();
            let (y1, m1, d1) = serial_to_date(a).ok_or(ErrKind::Num)?;
            let (y2, m2, d2) = serial_to_date(b).ok_or(ErrKind::Num)?;
            let v = match unit.as_str() {
                "D" => b - a,
                "M" => ((y2 - y1) * 12 + m2 as i32 - m1 as i32 - if d2 < d1 { 1 } else { 0 }) as f64,
                "Y" => (y2 - y1 - if (m2, d2) < (m1, d1) { 1 } else { 0 }) as f64,
                "YM" => (((m2 as i32 - m1 as i32) - if d2 < d1 { 1 } else { 0 }).rem_euclid(12)) as f64,
                "MD" => {
                    let mut md = d2 as i32 - d1 as i32;
                    if md < 0 {
                        let pm = if m2 == 1 { 12 } else { m2 - 1 };
                        let py = if m2 == 1 { y2 - 1 } else { y2 };
                        md += days_in_month(py, pm) as i32;
                    }
                    md as f64
                }
                "YD" => (b - a) % 365.25,
                _ => return Err(ErrKind::Num),
            };
            Ok(FValue::number(v))
        }),
        "WEEKNUM" => r(|| {
            let serial = num(args, 0)?.floor();
            let (y, _, _) = serial_to_date(serial).ok_or(ErrKind::Num)?;
            let jan1 = date_to_serial(y, 1, 1).ok_or(ErrKind::Num)?;
            let offset = ((jan1 as i64 + 6) % 7) as f64; // 周日为一周开始
            Ok(FValue::number(((serial - jan1 + offset) / 7.0).floor() + 1.0))
        }),
        "NETWORKDAYS" => r(|| {
            let a = num(args, 0)?.floor() as i64;
            let b = num(args, 1)?.floor() as i64;
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            let mut n = 0;
            for d in lo..=hi {
                // (d+6)%7：0=周日 1=周一 … 6=周六
                let wd = (d + 6) % 7;
                if wd != 0 && wd != 6 {
                    n += 1;
                }
            }
            Ok(FValue::number(n as f64))
        }),
        "WORKDAY" => r(|| {
            let start = num(args, 0)?.floor() as i64;
            let days = int_arg(args, 1)?;
            let mut d = start;
            let mut left = days.abs();
            let step = if days >= 0 { 1 } else { -1 };
            while left > 0 {
                d += step;
                let wd = (d + 6) % 7;
                if wd != 0 && wd != 6 {
                    left -= 1;
                }
            }
            Ok(FValue::number(d as f64))
        }),
        "YEARFRAC" => r(|| {
            let start = num(args, 0)?.floor();
            let end = num(args, 1)?.floor();
            let basis = opt_num(args, 2, 0.0)?.trunc() as i32;
            year_fraction(start.min(end), start.max(end), basis).map(FValue::number)
        }),

        // ---------------- 财务
        "PMT" => r(|| {
            let rate = num(args, 0)?;
            let nper = num(args, 1)?;
            let pv = num(args, 2)?;
            let fv = opt_num(args, 3, 0.0)?;
            let t = opt_num(args, 4, 0.0)?;
            Ok(FValue::number(pmt(rate, nper, pv, fv, t)))
        }),
        "FV" => r(|| {
            let rate = num(args, 0)?;
            let nper = num(args, 1)?;
            let pmt_v = num(args, 2)?;
            let pv = opt_num(args, 3, 0.0)?;
            let t = opt_num(args, 4, 0.0)?;
            Ok(FValue::number(fv(rate, nper, pmt_v, pv, t)))
        }),
        "PV" => r(|| {
            let rate = num(args, 0)?;
            let nper = num(args, 1)?;
            let pmt_v = num(args, 2)?;
            let fv_ = opt_num(args, 3, 0.0)?;
            let t = opt_num(args, 4, 0.0)?;
            Ok(FValue::number(pv(rate, nper, pmt_v, fv_, t)))
        }),
        "NPER" => r(|| {
            let rate = num(args, 0)?;
            let pmt_v = num(args, 1)?;
            let pv_ = num(args, 2)?;
            let fv_ = opt_num(args, 3, 0.0)?;
            let t = opt_num(args, 4, 0.0)?;
            if rate == 0.0 {
                if pmt_v == 0.0 {
                    return Err(ErrKind::Div0);
                }
                return Ok(FValue::number(-(pv_ + fv_) / pmt_v));
            }
            let z = pmt_v * (1.0 + rate * t) / rate;
            let num_ = z - fv_;
            let den = pv_ + z;
            if num_ / den <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((num_ / den).ln() / (1.0 + rate).ln()))
        }),
        "IPMT" => r(|| {
            let rate = num(args, 0)?;
            let per = num(args, 1)?;
            let nper = num(args, 2)?;
            let pv_ = num(args, 3)?;
            let fv_ = opt_num(args, 4, 0.0)?;
            let t = opt_num(args, 5, 0.0)?;
            let p = pmt(rate, nper, pv_, fv_, t);
            // fv() 是 FV 口径（余额与 pv 反号），这里要的是与 pv 同号的贷款余额。
            // Excel：pv=-300000、rate=5%/12 时 IPMT 第 1 期 = +1250，PPMT = +360.46。
            let balance = -fv_after(rate, per - 1.0, p, pv_, t);
            Ok(FValue::number(-(balance * rate)))
        }),
        "PPMT" => r(|| {
            let rate = num(args, 0)?;
            let per = num(args, 1)?;
            let nper = num(args, 2)?;
            let pv_ = num(args, 3)?;
            let fv_ = opt_num(args, 4, 0.0)?;
            let t = opt_num(args, 5, 0.0)?;
            let p = pmt(rate, nper, pv_, fv_, t);
            let balance = -fv_after(rate, per - 1.0, p, pv_, t);
            let interest = -(balance * rate);
            Ok(FValue::number(p - interest))
        }),
        "NPV" => r(|| {
            let rate = num(args, 0)?;
            let flows = numbers(args, 1)?;
            let mut v = 0.0;
            for (i, f) in flows.iter().enumerate() {
                v += f / (1.0 + rate).powi(i as i32 + 1);
            }
            Ok(FValue::number(v))
        }),
        "SLN" => r(|| {
            let cost = num(args, 0)?;
            let salvage = num(args, 1)?;
            let life = num(args, 2)?;
            if life == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number((cost - salvage) / life))
        }),
        "SYD" => r(|| {
            let cost = num(args, 0)?;
            let salvage = num(args, 1)?;
            let life = num(args, 2)?;
            let per = num(args, 3)?;
            Ok(FValue::number((cost - salvage) * (life - per + 1.0) * 2.0 / (life * (life + 1.0))))
        }),

        // ---------------- 信息
        "ISBLANK" => FValue::Bool(matches!(arg(args, 0), FValue::Empty)),
        "ISNUMBER" => FValue::Bool(matches!(arg(args, 0), FValue::Number(_))),
        "ISTEXT" => FValue::Bool(matches!(arg(args, 0), FValue::Text(_))),
        "ISNONTEXT" => FValue::Bool(!matches!(arg(args, 0), FValue::Text(_))),
        "ISLOGICAL" => FValue::Bool(matches!(arg(args, 0), FValue::Bool(_))),
        "ISEVEN" => FValue::Bool(matches!(arg(args, 0).to_number(), Ok(n) if (n.trunc() as i64) % 2 == 0)),
        "ISODD" => FValue::Bool(matches!(arg(args, 0).to_number(), Ok(n) if (n.trunc() as i64) % 2 != 0)),
        "ISERROR" => FValue::Bool(arg(args, 0).is_error()),
        "ISERR" => FValue::Bool(matches!(arg(args, 0), FValue::Error(e) if e != ErrKind::Na)),
        "ISNA" => FValue::Bool(matches!(arg(args, 0), FValue::Error(ErrKind::Na))),
        "ISREF" => FValue::Bool(false),
        "NA" => FValue::Error(ErrKind::Na),
        "N" => r(|| match arg(args, 0) {
            FValue::Number(n) => Ok(FValue::number(n)),
            FValue::Bool(b) => Ok(FValue::number(if b { 1.0 } else { 0.0 })),
            FValue::Error(e) => Err(e),
            _ => Ok(FValue::number(0.0)),
        }),
        "TYPE" => FValue::number(match arg(args, 0) {
            FValue::Array(_) => 64.0,
            FValue::Number(_) => 1.0,
            FValue::Text(_) => 2.0,
            FValue::Bool(_) => 4.0,
            FValue::Lambda(_) => 128.0,
            FValue::Error(_) => 16.0,
            FValue::Empty => 1.0,
        }),
        "ERROR.TYPE" => r(|| match arg(args, 0) {
            FValue::Error(e) => Ok(FValue::number(match e {
                ErrKind::Null => 1.0,
                ErrKind::Div0 => 2.0,
                ErrKind::Value => 3.0,
                ErrKind::Ref => 4.0,
                ErrKind::Name => 5.0,
                ErrKind::Num => 6.0,
                ErrKind::Na => 7.0,
                ErrKind::Circular => 4.0,
                ErrKind::Spill => 9.0,
            })),
            _ => Err(ErrKind::Na),
        }),

        _ => crate::formula::array::call_array(name, args)
            .or_else(|| crate::formula::extra::call_extra(name, args))
            .unwrap_or_else(unknown),
    }
}

// ---------------------------------------------------------------- 辅助实现

pub(crate) fn opt_bool(args: &[Arg], i: usize, d: bool) -> bool {
    args.get(i).and_then(|a| a.first().to_bool().ok()).unwrap_or(d)
}

pub(crate) fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

pub(crate) fn std_dev(v: &[f64], sample: bool) -> Result<f64, ErrKind> {
    let n = v.len();
    if n == 0 || (sample && n < 2) {
        return Err(ErrKind::Div0);
    }
    let m = v.iter().sum::<f64>() / n as f64;
    let ss = v.iter().map(|x| (x - m).powi(2)).sum::<f64>();
    let denom = if sample { n - 1 } else { n } as f64;
    Ok((ss / denom).sqrt())
}

pub(crate) fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = p * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
    }
}

fn pmt(rate: f64, nper: f64, pv: f64, fv: f64, t: f64) -> f64 {
    if rate == 0.0 {
        return -(pv + fv) / nper;
    }
    let p = (1.0 + rate).powf(nper);
    -(rate * (pv * p + fv)) / ((1.0 + rate * t) * (p - 1.0))
}

fn year_fraction(start: f64, end: f64, basis: i32) -> Result<f64, ErrKind> {
    if start < 0.0 || end > date_to_serial(9999, 12, 31).ok_or(ErrKind::Num)? {
        return Err(ErrKind::Num);
    }
    let calendar = |serial| {
        if serial == 0.0 && !current_date_system().is_1904() {
            Ok((1900, 1, 0))
        } else {
            serial_to_date(serial).ok_or(ErrKind::Num)
        }
    };
    let (y1, m1, d1) = calendar(start)?;
    let (y2, m2, d2) = calendar(end)?;
    let actual = end - start;
    match basis {
        0 | 4 => {
            let last_feb = |year, month, day| month == 2 && days_in_month(year, 2) == day;
            let (from, to) = if basis == 4 {
                (d1.min(30), d2.min(30))
            } else if last_feb(y1, m1, d1) {
                (30, if last_feb(y2, m2, d2) { 30 } else { d2 })
            } else {
                (d1.min(30), if d1 >= 30 { d2.min(30) } else { d2 })
            };
            let days = 360 * (y2 - y1) + 30 * (m2 as i32 - m1 as i32) + to as i32 - from as i32;
            Ok(days as f64 / 360.0)
        }
        1 => {
            // For periods longer than a year, Excel averages the lengths of all
            // calendar years touched, including the first and last.
            let longer = y2 > y1 + 1 || (y2 == y1 + 1 && (m2, d2) > (m1, d1));
            let denominator = if longer {
                let days = days_from_civil(y2 + 1, 1, 1) - days_from_civil(y1, 1, 1);
                days as f64 / f64::from(y2 - y1 + 1)
            } else {
                let leap = |year| days_in_month(year, 2) == 29;
                let includes_leap = (leap(y1) && (y1 == y2 || m1 <= 2))
                    || (leap(y2) && (m2, d2) >= (2, 29));
                if includes_leap { 366.0 } else { 365.0 }
            };
            Ok(actual / denominator)
        }
        2 => Ok(actual / 360.0),
        3 => Ok(actual / 365.0),
        _ => Err(ErrKind::Num),
    }
}

fn fv(rate: f64, nper: f64, pmt_v: f64, pv: f64, t: f64) -> f64 {
    if rate == 0.0 {
        return -(pv + pmt_v * nper);
    }
    let p = (1.0 + rate).powf(nper);
    -(pv * p + pmt_v * (1.0 + rate * t) * (p - 1.0) / rate)
}

fn pv(rate: f64, nper: f64, pmt_v: f64, fv: f64, t: f64) -> f64 {
    if rate == 0.0 {
        return -(fv + pmt_v * nper);
    }
    let p = (1.0 + rate).powf(nper);
    -(fv + pmt_v * (1.0 + rate * t) * (p - 1.0) / rate) / p
}

fn fv_after(rate: f64, nper: f64, pmt_v: f64, pv: f64, t: f64) -> f64 {
    fv(rate, nper, pmt_v, pv, t)
}

fn rand_f64() -> f64 {
    use std::cell::Cell;
    use std::time::{SystemTime, UNIX_EPOCH};
    thread_local! {
        static SEED: Cell<u64> = Cell::new(
            SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(12345)
        );
    }
    SEED.with(|s| {
        let mut x = s.get();
        // xorshift64*
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        s.set(x);
        ((x.wrapping_mul(0x2545F4914F6CDD1D)) >> 11) as f64 / (1u64 << 53) as f64
    })
}

/// 按 y/m/d 记号逐个展开日期格式：`yyyy`/`yy`/`y`、`mm`/`m`、`dd`/`d`。
/// 和 Excel 一样，格式里夹着的中文（如 `yyyy年m月d日`）原样保留；
/// 双引号包裹的段落与反斜杠转义都按字面输出。
/// 三字母以上的 m/d（`mmm` 月份名、`ddd` 星期名）依赖区域语言，这里按数值展开。
/// 由年月日算星期（0=周日），与 Excel 的 WEEKDAY(...,1) 一致。
fn weekday_from_ymd(y: i32, m: u32, d: u32) -> usize {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let yy = if m < 3 { y - 1 } else { y };
    let w = (yy + yy / 4 - yy / 100 + yy / 400 + T[(m as usize - 1).min(11)] + d as i32) % 7;
    w.rem_euclid(7) as usize
}

fn format_date_tokens(fmt: &str, y: i32, m: u32, d: u32) -> Option<String> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == '\\' && i + 1 < chars.len() {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        let lower = c.to_ascii_lowercase();
        if !matches!(lower, 'y' | 'm' | 'd') {
            out.push(c);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && chars[j].to_ascii_lowercase() == lower {
            j += 1;
        }
        let run = j - i;
        match lower {
            'y' => {
                if run <= 2 {
                    out.push_str(&format!("{:02}", y.rem_euclid(100)));
                } else {
                    out.push_str(&format!("{y:04}"));
                }
            }
            'm' => {
                const MON: [&str; 12] = [
                    "January", "February", "March", "April", "May", "June", "July", "August",
                    "September", "October", "November", "December",
                ];
                match run {
                    1 => out.push_str(&m.to_string()),
                    2 => out.push_str(&format!("{m:02}")),
                    3 => out.push_str(&MON[(m as usize - 1).min(11)][..3]),
                    _ => out.push_str(MON[(m as usize - 1).min(11)]),
                }
            }
            'd' => {
                const DAY: [&str; 7] = [
                    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
                ];
                let wd = weekday_from_ymd(y, m, d);
                match run {
                    1 => out.push_str(&d.to_string()),
                    2 => out.push_str(&format!("{d:02}")),
                    3 => out.push_str(&DAY[wd][..3]),
                    _ => out.push_str(DAY[wd]),
                }
            }
            _ => out.push(c),
        }
        i = j;
    }
    Some(out)
}

/// Excel 查找通配符：`*` 任意串、`?` 单字符、`~` 转义下一个字符；大小写不敏感。
fn wildcard_match_ci(pat: &str, s: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        match p[0] {
            '*' => (0..=t.len()).any(|k| go(&p[1..], &t[k..])),
            '?' => !t.is_empty() && go(&p[1..], &t[1..]),
            '~' if p.len() > 1 => {
                !t.is_empty() && t[0].eq_ignore_ascii_case(&p[1]) && go(&p[2..], &t[1..])
            }
            c => !t.is_empty() && t[0].eq_ignore_ascii_case(&c) && go(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    go(&p, &t)
}

pub(crate) fn is_wildcard(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('~')
}

/// 按顶层 `;` 切分 TEXT 格式（引号内的分号不算分隔符）。Excel 最多 4 段：
/// 正数;负数;零;文本。
fn split_format_sections(fmt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in fmt.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            ';' if !in_quote => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// 去掉 `[Red]`/`[$-409]` 这类颜色与区域标记；`[h]`/`[m]`/`[s]` 是经过时长，本引擎不支持，一并去掉。
fn strip_format_brackets(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn format_value(v: &FValue, fmt: &str) -> String {
    // TEXT 的分段语义：正数;负数;零;文本。文本值走第 4 段（@ 占位），
    // 没有第 4 段就原样返回；`[Red]`/`[$-409]` 这类标记先剥掉。
    let sections = split_format_sections(fmt);
    if let FValue::Text(t) = v {
        let sec = if sections.len() >= 4 {
            strip_format_brackets(&sections[3])
        } else {
            strip_format_brackets(&sections[0])
        };
        let sec = sec.replace('"', "");
        return if sec.contains('@') { sec.replace('@', t) } else { t.clone() };
    }
    if !matches!(v, FValue::Number(_) | FValue::Empty) {
        return v.display();
    }
    let raw = v.to_number().unwrap_or(0.0);
    let (sec_raw, is_neg) = if raw < 0.0 && sections.len() >= 2 {
        (sections[1].clone(), true)
    } else if raw == 0.0 && sections.len() >= 3 {
        (sections[2].clone(), false)
    } else {
        (sections[0].clone(), raw < 0.0)
    };
    let sec = strip_format_brackets(&sec_raw);
    if sec.trim().eq_ignore_ascii_case("general") {
        let d = v.display();
        return if is_neg { format!("-{}", d.trim_start_matches('-')) } else { d };
    }
    let fmt: &str = sec.as_str();
    // 负数段只对绝对值排版，负号由该段里的字面 '-' 提供（Excel 同一口径）
    let sign = if is_neg && fmt.contains('-') { "-" } else { "" };
    let n = if is_neg { raw.abs() } else { raw };
    // 整段没有数字占位符（如零段写成 "零"）：按字面输出
    let literal_only = !sec
        .chars()
        .any(|c| matches!(c, '0' | '#' | '?' | '.' | ',' | '%' | 'y' | 'Y' | 'm' | 'M' | 'd' | 'D'));
    if literal_only {
        return format!("{sign}{}", sec.replace('"', ""));
    }
    // 常见占位符：0、0.00、#,##0、0%、0.00%、yyyy-mm-dd
    if fmt.chars().any(|c| matches!(c, 'y' | 'Y' | 'm' | 'M' | 'd' | 'D')) {
        if let Some((y, m, d)) = serial_to_date(n) {
            if let Some(out) = format_date_tokens(fmt, y, m, d) {
                return format!("{sign}{out}");
            }
        }
    }
    let percent = fmt.contains('%');
    let mut val = if percent { n * 100.0 } else { n };
    let decimals = fmt
        .split('.')
        .nth(1)
        .map(|s| s.chars().filter(|c| *c == '0' || *c == '#').count())
        .unwrap_or(0);
    let thousand = fmt.contains('#') && fmt.contains(',');
    let mut s = format!("{val:.decimals$}");
    // 整数格式里的 0 是"至少几位"：TEXT(12.3,"000.00") = "012.30"
    let min_int = fmt
        .split('.')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| *c == '0')
        .count();
    if min_int > 0 {
        let (ip, rest) = match s.find('.') {
            Some(i) => (s[..i].to_string(), s[i..].to_string()),
            None => (s.clone(), String::new()),
        };
        let neg = ip.starts_with('-');
        let digits = ip.trim_start_matches('-');
        if digits.len() < min_int {
            let padded = format!("{}{}", "0".repeat(min_int - digits.len()), digits);
            s = format!("{}{}{}", if neg { "-" } else { "" }, padded, rest);
        }
    }
    if thousand {
        let int_part = s.split('.').next().unwrap_or("").to_string();
        let neg = int_part.starts_with('-');
        let digits = int_part.trim_start_matches('-').to_string();
        let mut grouped = String::new();
        for (i, ch) in digits.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(ch);
        }
        let g: String = grouped.chars().rev().collect();
        let mut rest = String::new();
        if let Some(dot) = s.find('.') {
            rest = s[dot..].to_string();
        }
        s = format!("{}{}{}", if neg { "-" } else { "" }, g, rest);
    }
    if percent {
        s.push('%');
    }
    val = 0.0;
    let _ = val;
    format!("{sign}{s}")
}

/// 已实现的函数名列表（用于覆盖率统计与文档）
pub fn implemented() -> Vec<&'static str> {
    IMPLEMENTED.to_vec()
}

pub(super) fn listed_builtin(name: &str) -> bool {
    static NAMES: std::sync::LazyLock<std::collections::HashSet<&'static str>> =
        std::sync::LazyLock::new(|| IMPLEMENTED.iter().copied().collect());
    NAMES.contains(name)
}

const IMPLEMENTED: &[&str] = &[
    "ABS","ACOS","ACOSH","ACOT","AND","ARABIC","ARRAYTOTEXT","ASIN","ASINH","ATAN","ATAN2","ATANH","AVEDEV","AVERAGE","AVERAGEA","AVERAGEIF","BASE","BETA.DIST","BETA.INV","BETADIST","BETAINV","BIN2DEC","BIN2HEX","BIN2OCT","BINOM.DIST","BINOM.INV","BINOMDIST","BITAND","BITLSHIFT","BITOR","BITRSHIFT","BITXOR","CEILING","CEILING.MATH","CEILING.PRECISE","CHAR","CHIDIST","CHIINV","CHISQ.DIST","CHISQ.DIST.RT","CHISQ.INV","CHISQ.INV.RT","CHOOSE","CHOOSECOLS","CHOOSEROWS","CODE","COLUMN","COLUMNS","COMBIN","COMBINA","COMPLEX","CONCAT","CONCATENATE","CONFIDENCE","CONFIDENCE.NORM","CONFIDENCE.T","CORREL","COS","COSH","COT","COTH","COUNT","COUNTA","COUNTBLANK","COUNTIF","COUNTIFS","CRITBINOM","CSC","CSCH","CUMIPMT","CUMPRINC","DATE","DATEDIF","DAVERAGE","DAY","DAYS","DB","DCOUNT","DCOUNTA","DDB","DEC2BIN","DEC2HEX","DEC2OCT","DECIMAL","DEGREES","DELTA","DEVSQ","DGET","DMAX","DMIN","DOLLARDE","DOLLARFR","DPRODUCT","DROP","DSTDEV","DSTDEVP","DSUM","DVAR","DVARP","EDATE","EFFECT","EOMONTH","ERF","ERF.PRECISE","ERFC","ERFC.PRECISE","ERROR.TYPE","EVEN","EXACT","EXP","EXPAND","EXPON.DIST","EXPONDIST","F.DIST","F.DIST.RT","F.INV","F.INV.RT","FACT","FACTDOUBLE","FALSE","FDIST","FILTER","FIND","FINV","FISHER","FISHERINV","FLOOR","FLOOR.MATH","FLOOR.PRECISE","FV","FVSCHEDULE","GAMMA.DIST","GAMMA.INV","GAMMADIST","GAMMAINV","GAMMALN","GAMMALN.PRECISE","GCD","GEOMEAN","GESTEP","HARMEAN","HEX2BIN","HEX2DEC","HEX2OCT","HLOOKUP","HOUR","HSTACK","HYPGEOM.DIST","HYPGEOMDIST","IF","IFERROR","IFNA","IFS","IMABS","IMADD","IMAGINARY","IMARGUMENT","IMCONJUGATE","IMCOS","IMDIV","IMEXP","IMLN","IMLOG10","IMLOG2","IMMUL","IMPOWER","IMPRODUCT","IMREAL","IMSIN","IMSQRT","IMSUB","IMSUM","IMTAN","INDEX","INT","INTERCEPT","IPMT","IRR","ISBLANK","ISERR","ISERROR","ISEVEN","ISLOGICAL","ISNA","ISNONTEXT","ISNUMBER","ISO.CEILING","ISODD","ISREF","ISTEXT","KURT","LARGE","LCM","LEFT","LEN","LET","LN","LOG","LOG10","LOGINV","LOGNORM.DIST","LOGNORM.INV","LOGNORMDIST","LOOKUP","LOWER","MATCH","MAX","MAXA","MAXIFS","MEDIAN","MID","MIN","MINA","MINIFS","MINUTE","MIRR","MMULT","MOD","MODE","MONTH","MROUND","MULTINOMIAL","N","NA","NEGBINOM.DIST","NEGBINOMDIST","NETWORKDAYS","NOMINAL","NORM.DIST","NORM.INV","NORM.S.DIST","NORM.S.INV","NORMDIST","NORMINV","NORMSDIST","NORMSINV","NOT","NOW","NPER","NPV","OCT2BIN","OCT2DEC","OCT2HEX","ODD","OR","PDURATION","PERCENTILE","PERCENTILE.INC","PERMUT","PI","PMT","POISSON","POISSON.DIST","POWER","PPMT","PRODUCT","PROPER","PV","QUARTILE","QUARTILE.INC","QUOTIENT","RADIANS","RAND","RANDARRAY","RANDBETWEEN","RANK","RANK.EQ","RATE","REPLACE","REPT","RIGHT","ROMAN","ROUND","ROUNDDOWN","ROUNDUP","ROW","ROWS","RRI","SEARCH","SEC","SECH","SECOND","SEQUENCE","SERIESSUM","SIGN","SIN","SINH","SKEW","SKEW.P","SLN","SLOPE","SMALL","SORT","SORTBY","SQRT","SQRTPI","STANDARDIZE","STDEV","STDEV.P","STDEV.S","STDEVP","SUBSTITUTE","SUBTOTAL","SUM","SUMIF","SUMIFS","SUMPRODUCT","SUMSQ","SUMX2MY2","SUMX2PY2","SUMXMY2","SWITCH","SYD","T","T.DIST","T.DIST.2T","T.DIST.RT","T.INV","T.INV.2T","TAKE","TAN","TANH","TEXT","TEXTAFTER","TEXTBEFORE","TEXTJOIN","TIME","TINV","TOCOL","TODAY","TOROW","TRANSPOSE","TRIM","TRIMMEAN","TRUE","TRUNC","TYPE","UNICHAR","UNICODE","UNIQUE","UPPER","VALUE","VALUETOTEXT","VAR","VAR.P","VAR.S","VARP","VLOOKUP","VSTACK","WEEKDAY","WEEKNUM","WEIBULL","WEIBULL.DIST","WORKDAY","XIRR","XLOOKUP","XMATCH","XNPV","XOR","YEAR","YEARFRAC","Z.TEST","ZTEST","T.TEST","F.TEST","CHISQ.TEST","RSQ","FORECAST","FORECAST.LINEAR","STEYX","TREND","GROWTH","LINEST","PERCENTRANK","PERCENTRANK.INC","PERCENTILE.EXC","QUARTILE.EXC","RANK.AVG","CONVERT","VDB","DURATION","MDURATION","TBILLPRICE","TBILLYIELD","TBILLEQ","AMORLINC","AMORDEGRC","AVERAGEIFS","MODE.SNGL","MODE.MULT","AGGREGATE","FREQUENCY","PROB","STDEVA","VARA","COVARIANCE.P","COVARIANCE.S","COVARIANCE","PEARSON","HYPERLINK","ENCODEURL","TEXTSPLIT","WRAPROWS","WRAPCOLS","ISFORMULA","FORMULATEXT","SHEET","SHEETS","ADDRESS","INDIRECT","OFFSET","LAMBDA","MAP","REDUCE","SCAN","BYROW","BYCOL","MAKEARRAY","ISOMITTED",
    "ACOTH","GAMMA","GAUSS","PHI","PERMUTATIONA","BINOM.DIST.RANGE","COVAR","PERCENTOF","PERCENTRANK.EXC","LOGEST","MDETERM","MINVERSE","CLEAN","ASC","DOLLAR","FIXED","NUMBERVALUE","DAYS360","DATEVALUE","TIMEVALUE","ISOWEEKNUM","CELL","AREAS","INFO","COUPPCD","COUPNCD","COUPNUM","COUPDAYS","COUPDAYBS","COUPDAYSNC","PRICE","YIELD","DISC","INTRATE","RECEIVED","PRICEDISC","YIELDDISC","PRICEMAT","YIELDMAT","REGEXTEST","REGEXEXTRACT","REGEXREPLACE","NETWORKDAYS.INTL","WORKDAY.INTL","ODDFPRICE","ODDFYIELD","ODDLPRICE","ODDLYIELD","BESSELI","BESSELJ","BESSELK","BESSELY","FORECAST.ETS","FORECAST.ETS.CONFINT","FORECAST.ETS.SEASONALITY","FORECAST.ETS.STAT",
];

#[cfg(test)]
mod clock_and_serial60_tests {
    use super::*;
    use crate::formula::eval_formula;
    use crate::model::{DateSystem, Workbook};

    fn number(v: crate::formula::FValue) -> f64 {
        match v {
            crate::formula::FValue::Number(n) => n,
            other => panic!("expected number, got {other:?}"),
        }
    }

    fn text(v: crate::formula::FValue) -> String {
        match v {
            crate::formula::FValue::Text(s) => s,
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn serial_59_60_61_and_fictional_feb29_are_distinct() {
        assert_eq!(serial_to_date_in(DateSystem::D1900, 59.0), Some((1900, 2, 28)));
        assert_eq!(serial_to_date_in(DateSystem::D1900, 60.0), Some((1900, 2, 29)));
        assert_eq!(serial_to_date_in(DateSystem::D1900, 61.0), Some((1900, 3, 1)));
        assert_eq!(date_to_serial_in(DateSystem::D1900, 1900, 2, 29), Some(60.0));
        assert_eq!(date_to_serial_in(DateSystem::D1900, 1900, 3, 1), Some(61.0));
        // 1904 体系没有这个虚构日，不能凭空接受 1900-02-29。
        assert_eq!(date_to_serial_in(DateSystem::D1904, 1900, 2, 29), None);
    }

    #[test]
    fn day_month_year_and_text_agree_on_serial_60() {
        let wb = Workbook::new_blank();
        assert_eq!(number(eval_formula(&wb, 0, "=DAY(60)")), 29.0);
        assert_eq!(number(eval_formula(&wb, 0, "=MONTH(60)")), 2.0);
        assert_eq!(number(eval_formula(&wb, 0, "=YEAR(60)")), 1900.0);
        assert_eq!(text(eval_formula(&wb, 0, "=TEXT(60,\"yyyy-mm-dd\")")), "1900-02-29");
        assert_eq!(text(eval_formula(&wb, 0, "=TEXT(59,\"yyyy-mm-dd\")")), "1900-02-28");
        assert_eq!(text(eval_formula(&wb, 0, "=TEXT(61,\"yyyy-mm-dd\")")), "1900-03-01");
        assert_eq!(number(eval_formula(&wb, 0, "=DATE(1900,2,29)")), 60.0);
        assert_eq!(
            crate::style::format_value_in(
                DateSystem::D1900,
                &crate::model::Value::Number(60.0),
                "yyyy-mm-dd"
            ),
            "1900-02-29"
        );
    }

    #[test]
    fn injected_clock_uses_local_midnight_not_utc_and_is_stable() {
        let wb = Workbook::new_blank();
        // 2024-03-01 00:30 Asia/Shanghai == 2024-02-29 16:30 UTC
        let utc = days_from_civil(2024, 2, 29) * 86_400 + 16 * 3600 + 30 * 60;
        let mar1 = date_to_serial_in(DateSystem::D1900, 2024, 3, 1).unwrap();
        with_clock_at(utc, 8 * 3600, || {
            assert_eq!(number(eval_formula(&wb, 0, "=TODAY()")), mar1);
            let now = number(eval_formula(&wb, 0, "=NOW()"));
            assert_eq!(now.floor(), mar1);
            assert!((now - mar1 - (30.0 / (24.0 * 60.0))).abs() < 1e-9);
            // 同一次求值里的两个 NOW 必须来自同一快照。
            assert_eq!(number(eval_formula(&wb, 0, "=NOW()-NOW()")), 0.0);
        });
    }

    fn recalc_now_book(sys: DateSystem, one_sheet: bool) -> Workbook {
        // 2024-03-01 00:30 Asia/Shanghai == 2024-02-29 16:30 UTC
        let utc = days_from_civil(2024, 2, 29) * 86_400 + 16 * 3600 + 30 * 60;
        let mut wb = Workbook::new_blank();
        wb.date_system = sys;
        wb.sheets[0].set(0, 0, crate::model::Cell { value: crate::model::Value::Empty, formula: Some("=TODAY()".into()) });
        wb.sheets[0].set(0, 1, crate::model::Cell { value: crate::model::Value::Empty, formula: Some("=NOW()".into()) });
        wb.sheets[0].set(0, 2, crate::model::Cell { value: crate::model::Value::Empty, formula: Some("=NOW()-NOW()".into()) });
        with_clock_at(utc, 8 * 3600, || {
            if one_sheet {
                crate::formula::recalc_sheet(&mut wb, 0);
            } else {
                crate::formula::recalc_all_force(&mut wb);
            }
        });
        wb
    }

    #[test]
    fn recalc_binds_now_snapshot_to_workbook_date_system() {
        let mar1_1900 = date_to_serial_in(DateSystem::D1900, 2024, 3, 1).unwrap();
        assert_eq!(mar1_1900, 45_352.0);
        assert_eq!(mar1_1900 - DateSystem::OFFSET, 43_890.0);
        for (sys, want_day) in [
            (DateSystem::D1900, 45_352.0),
            (DateSystem::D1904, 43_890.0),
        ] {
            for one_sheet in [false, true] {
                let wb = recalc_now_book(sys, one_sheet);
                assert_eq!(
                    wb.sheets[0].value(0, 0),
                    crate::model::Value::Number(want_day),
                    "sys={sys:?} one_sheet={one_sheet}"
                );
                let want_now = want_day + 30.0 / (24.0 * 60.0);
                let now = match wb.sheets[0].value(0, 1) {
                    crate::model::Value::Number(n) => n,
                    other => panic!("expected NOW number, got {other:?}"),
                };
                assert!((now - want_now).abs() < 1e-9, "sys={sys:?} NOW={now}");
                assert_eq!(
                    wb.sheets[0].value(0, 2),
                    crate::model::Value::Number(0.0),
                    "sys={sys:?} 同轮 NOW 快照不一致"
                );
            }
        }
    }

    #[test]
    fn timezone_and_midnight_changes_do_not_leak_previous_snapshot() {
        let wb = Workbook::new_blank();
        let feb29 = date_to_serial_in(DateSystem::D1900, 2024, 2, 29).unwrap();
        let mar1 = date_to_serial_in(DateSystem::D1900, 2024, 3, 1).unwrap();
        // 本地 2024-02-29 23:59（UTC 15:59 +08:00）
        let utc_before = days_from_civil(2024, 2, 29) * 86_400 + 15 * 3600 + 59 * 60;
        with_clock_at(utc_before, 8 * 3600, || {
            assert_eq!(number(eval_formula(&wb, 0, "=TODAY()")), feb29);
        });
        // 本地 2024-03-01 00:00（UTC 16:00 +08:00）
        let utc_after = days_from_civil(2024, 2, 29) * 86_400 + 16 * 3600;
        with_clock_at(utc_after, 8 * 3600, || {
            assert_eq!(number(eval_formula(&wb, 0, "=TODAY()")), mar1);
        });
        // UTC 仍为 02-29 16:00 时，改注入 -5 小时时区应回到 02-29 11:00。
        with_clock_at(utc_after, -5 * 3600, || {
            assert_eq!(number(eval_formula(&wb, 0, "=TODAY()")), feb29);
        });
    }
}

#[cfg(test)]
mod round_precision {
    use super::round_sig15;

    /// 57*275.75*0.62 在二进制里是 9745.004999999998，
    /// Excel 按 15 位有效数字看到 9745.005，ROUND(...,2) 得 9745.01。
    #[test]
    fn matches_excel_fifteen_digit_arithmetic() {
        let v = 57.0 * 275.75 * 0.62;
        // 乘积正好是 9745.005，但乘 100 之后二进制落在 974500.4999999999
        assert!(v * 100.0 < 974500.5);
        assert_eq!(round_sig15(v * 100.0).round() / 100.0, 9745.01);
    }

    #[test]
    fn plain_rounding_is_unchanged() {
        assert_eq!(round_sig15(2.5).round(), 3.0);
        assert_eq!(round_sig15(-2.5).round(), -3.0);
        assert_eq!(round_sig15(0.0), 0.0);
        assert_eq!(round_sig15(1234.5678 * 100.0).round() / 100.0, 1234.57);
    }
}
