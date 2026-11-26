//! 函数库。所有函数经由 `call()` 分发，新增函数只需在 match 里加一行。

use super::func_id::FuncId;
use super::fxhash::FxHashMap;
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
        out.extend_from_slice(a.slice());
    }
    out
}

// ---------------------------------------------------------------- 日期系统

/// 1970-01-01 起的天数 → 公历年月日（Howard Hinnant 算法）
pub(crate) fn civil_from_days(z: i64) -> (i32, u32, u32) {
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


#[derive(Clone, Copy, Debug)]
pub struct EvalCtx {
    pub date_system: DateSystem,
    pub now_snapshot: Option<f64>,
}

impl EvalCtx {
    #[inline]
    pub fn from_ambient() -> Self {
        Self {
            date_system: current_date_system(),
            now_snapshot: now_snapshot(),
        }
    }

    #[inline]
    pub fn for_workbook(sys: DateSystem) -> Self {
        Self {
            date_system: sys,
            now_snapshot: Some(now_snapshot().unwrap_or_else(|| now_serial_uncached_in(sys))),
        }
    }

    #[inline]
    pub fn now_serial(self) -> f64 {
        self.now_snapshot.unwrap_or_else(|| now_serial_uncached_in(self.date_system))
    }
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

pub(super) fn max_date_serial(sys: DateSystem) -> i64 {
    if sys.is_1904() { 2_957_003 } else { 2_958_465 }
}

fn checked_date_serial(sys: DateSystem, v: f64) -> Result<i64, ErrKind> {
    if !v.is_finite() {
        return Err(ErrKind::Num);
    }
    let f = v.floor();
    let max = max_date_serial(sys);
    if f < 0.0 || f > max as f64 {
        return Err(ErrKind::Num);
    }
    Ok(f as i64)
}

/// O(1 + H log H) 计算 `[start, end]` 闭区间内工作日天数（支持 1900/1904 与自定义周末掩码 `m[0..7]` = Mon..Sun）。
pub(super) fn networkdays_core(
    sys: DateSystem,
    start_f: f64,
    end_f: f64,
    mask: &[bool; 7],
    holidays: &[i64],
) -> Result<f64, ErrKind> {
    if mask.iter().all(|&w| w) {
        return Err(ErrKind::Value);
    }
    let a = checked_date_serial(sys, start_f)?;
    let b = checked_date_serial(sys, end_f)?;
    let date_off = if sys.is_1904() { 1462 } else { 0 };
    let (lo, hi, sign) = if a <= b { (a, b, 1.0) } else { (b, a, -1.0) };
    let total_days = hi - lo + 1;
    let wpw = mask.iter().filter(|&&w| !w).count() as i64;
    let full_weeks = total_days / 7;
    let rem = (total_days % 7) as usize;
    let mut count = full_weeks * wpw;
    let start_mon = ((lo + date_off + 5).rem_euclid(7)) as usize;
    for i in 0..rem {
        if !mask[(start_mon + i) % 7] {
            count += 1;
        }
    }
    if !holidays.is_empty() {
        let mut uniq: Vec<i64> = holidays
            .iter()
            .copied()
            .filter(|&h| h >= lo && h <= hi)
            .collect();
        uniq.sort_unstable();
        uniq.dedup();
        for h in uniq {
            let mon = ((h + date_off + 5).rem_euclid(7)) as usize;
            if !mask[mon] {
                count -= 1;
            }
        }
    }
    Ok(sign * count as f64)
}

/// O(1 + H log H) 计算从 `start` 增减 `days` 个工作日后的日期序列号。
pub(super) fn workday_core(
    sys: DateSystem,
    start_f: f64,
    days_f: f64,
    mask: &[bool; 7],
    holidays: &[i64],
) -> Result<f64, ErrKind> {
    if mask.iter().all(|&w| w) {
        return Err(ErrKind::Value);
    }
    let start = checked_date_serial(sys, start_f)?;
    if !days_f.is_finite() {
        return Err(ErrKind::Num);
    }
    let max_serial = max_date_serial(sys);
    let days_trunc = days_f.trunc();
    if days_trunc == 0.0 {
        return Ok(start as f64);
    }
    if days_trunc.abs() > (max_serial as f64 + 1.0) {
        return Err(ErrKind::Num);
    }
    let days = days_trunc as i64;
    let step: i64 = if days > 0 { 1 } else { -1 };
    let date_off = if sys.is_1904() { 1462 } else { 0 };
    let wpw = mask.iter().filter(|&&w| !w).count() as u64;
    let is_work_dow = |d: i64| !mask[((d + date_off + 5).rem_euclid(7)) as usize];
    let advance = |mut d: i64, k: u64| -> Result<i64, ErrKind> {
        if k == 0 {
            return Ok(d);
        }
        let full_weeks = (k - 1) / wpw;
        let rem = k - full_weeks * wpw;
        let week_delta = (full_weeks as i64)
            .checked_mul(7)
            .and_then(|v| v.checked_mul(step))
            .ok_or(ErrKind::Num)?;
        d = d.checked_add(week_delta).ok_or(ErrKind::Num)?;
        let mut left = rem;
        while left > 0 {
            d = d.checked_add(step).ok_or(ErrKind::Num)?;
            if is_work_dow(d) {
                left -= 1;
            }
        }
        if d < 0 || d > max_serial {
            return Err(ErrKind::Num);
        }
        Ok(d)
    };

    let mut hols: Vec<i64> = holidays
        .iter()
        .copied()
        .filter(|&h| {
            let in_dir = if step > 0 { h > start && h <= max_serial } else { h < start && h >= 0 };
            in_dir && is_work_dow(h)
        })
        .collect();
    if step > 0 {
        hols.sort_unstable();
    } else {
        hols.sort_unstable_by(|a, b| b.cmp(a));
    }
    hols.dedup();

    let mut cur = advance(start, days.unsigned_abs())?;
    let mut idx = 0usize;
    loop {
        let mut hit = 0u64;
        while idx < hols.len() {
            let h = hols[idx];
            if (step > 0 && h <= cur) || (step < 0 && h >= cur) {
                hit += 1;
                idx += 1;
            } else {
                break;
            }
        }
        if hit == 0 {
            break;
        }
        cur = advance(cur, hit)?;
    }
    Ok(cur as f64)
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

pub(crate) fn now_serial_uncached_in(sys: DateSystem) -> f64 {
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
    pub(super) op: &'static str,
    pub(super) num: Option<f64>,
    pub(super) text: String,
    pub(super) wild: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) enum GroupValue {
    Number(u64),
    Text(std::sync::Arc<str>),
}

impl GroupValue {
    #[inline]
    fn canonical_bits(n: f64) -> u64 {
        if n == 0.0 { 0 } else { n.to_bits() }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GroupStats {
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub count_num: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct BuiltGroups {
    pub distinct_nums: Vec<Option<Vec<f64>>>,
    pub map: FxHashMap<Vec<GroupValue>, GroupStats>,
}

impl BuiltGroups {
    /// Zero-`Vec<FValue>` group-by builder directly from persistent `SharedCondColumn`s!
    pub(crate) fn build_from_cond_cols(
        cond_cols: &[std::sync::Arc<super::lookup_cache::SharedCondColumn>],
        val_col: Option<&super::lookup_cache::SharedCondColumn>,
        numeric: &[bool],
        limit: usize,
        cancel: Option<&std::sync::atomic::AtomicBool>,
        deadline: Option<std::time::Instant>,
    ) -> Option<BuiltGroups> {
        let rows = cond_cols
            .iter()
            .map(|c| c.len)
            .min()?
            .min(val_col.map_or(usize::MAX, |v| v.len));
        if rows == 0 || cond_cols.len() != numeric.len() {
            return None;
        }
        if let Some(vc) = val_col
            && (vc.has_non_finite || vc.has_error)
        {
            return None;
        }
        let mut distinct_nums: Vec<Option<Vec<f64>>> = Vec::with_capacity(cond_cols.len());
        for (col, &is_num) in cond_cols.iter().zip(numeric) {
            if !is_num {
                distinct_nums.push(None);
                continue;
            }
            if col.has_non_finite {
                return None;
            }
            let all_int = col.len == rows
                && !col.has_coerced_non_strict
                && col.pages.iter().all(|p| p.all_exact_int);
            if all_int {
                distinct_nums.push(None);
                continue;
            }
            let mut any_fract = false;
            for r in 0..rows {
                if let Some(n) = col.coerced_number_at(r)
                    && n.fract() != 0.0
                {
                    any_fract = true;
                    break;
                }
            }
            if !any_fract {
                distinct_nums.push(None);
            } else {
                let mut vals = if col.len == rows
                    && let Some(sorted) = col.sorted_nums()
                {
                    let mut v: Vec<f64> = sorted
                        .iter()
                        .map(|&(n, _)| if n == 0.0 { 0.0 } else { n })
                        .collect();
                    v.dedup_by(|a, b| a.to_bits() == b.to_bits());
                    v
                } else {
                    let mut v = Vec::new();
                    for r in 0..rows {
                        if let Some(n) = col.coerced_number_at(r) {
                            v.push(if n == 0.0 { 0.0 } else { n });
                        }
                    }
                    v.sort_unstable_by(f64::total_cmp);
                    v.dedup_by(|a, b| a.to_bits() == b.to_bits());
                    v
                };
                if vals.windows(2).any(|w| w[1] - w[0] <= 2.0 * f64::EPSILON) {
                    return None;
                }
                vals.shrink_to_fit();
                distinct_nums.push(Some(vals));
            }
        }

        let empty_arc: std::sync::Arc<str> = "".into();
        let mut map: FxHashMap<Vec<GroupValue>, GroupStats> = FxHashMap::default();
        let mut key = Vec::with_capacity(cond_cols.len());
        'row: for row in 0..rows {
            if (row & 8191) == 0
                && (cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                    || deadline.is_some_and(|d| std::time::Instant::now() >= d))
            {
                return None;
            }
            key.clear();
            for (j, (col, &is_num)) in cond_cols.iter().zip(numeric).enumerate() {
                key.push(if is_num {
                    let Some(n) = col.coerced_number_at(row) else {
                        continue 'row;
                    };
                    let canonical = if let Some(ref vals) = distinct_nums[j] {
                        let c = if n == 0.0 { 0.0 } else { n };
                        let idx = vals.binary_search_by(|x| x.total_cmp(&c)).ok()?;
                        vals[idx]
                    } else {
                        n
                    };
                    GroupValue::Number(GroupValue::canonical_bits(canonical))
                } else {
                    let Some(t) = col.lower_text_opt_at(row, &empty_arc) else {
                        continue 'row;
                    };
                    GroupValue::Text(t)
                });
            }
            let (add_sum, add_min, add_max, add_cnt) = match val_col {
                None => (1.0, 1.0, 1.0, 1u32),
                Some(vc) => match vc.strict_number_opt_at(row) {
                    Some(n) => (n, n, n, 1u32),
                    None => (0.0, f64::INFINITY, f64::NEG_INFINITY, 0u32),
                },
            };
            if let Some(stats) = map.get_mut(&key) {
                stats.sum += add_sum;
                if add_cnt > 0 {
                    if stats.count_num == 0 {
                        stats.min = add_min;
                        stats.max = add_max;
                    } else {
                        stats.min = stats.min.min(add_min);
                        stats.max = stats.max.max(add_max);
                    }
                    stats.count_num += add_cnt;
                }
            } else {
                if map.len() >= limit {
                    return None;
                }
                map.insert(
                    std::mem::take(&mut key),
                    GroupStats {
                        sum: add_sum,
                        min: if add_cnt > 0 { add_min } else { f64::INFINITY },
                        max: if add_cnt > 0 { add_max } else { f64::NEG_INFINITY },
                        count_num: add_cnt,
                    },
                );
            }
        }
        Some(BuiltGroups { distinct_nums, map })
    }

    pub(crate) fn answer(&self, function: u8, raw_criteria: &[Crit]) -> Option<FValue> {
        let mut query = Vec::with_capacity(raw_criteria.len());
        for (crit, distinct) in raw_criteria.iter().zip(&self.distinct_nums) {
            if let Some(n) = crit.num {
                if !n.is_finite() {
                    return Some(Self::empty_answer(function));
                }
                let bits = match distinct {
                    None => {
                        if n.fract() != 0.0 {
                            return Some(Self::empty_answer(function));
                        }
                        GroupValue::canonical_bits(n)
                    }
                    Some(vals) => {
                        let pos = vals.partition_point(|v| *v - n < -f64::EPSILON);
                        match vals.get(pos) {
                            Some(&d) if (d - n).abs() <= f64::EPSILON => GroupValue::canonical_bits(d),
                            _ => return Some(Self::empty_answer(function)),
                        }
                    }
                };
                query.push(GroupValue::Number(bits));
            } else {
                query.push(GroupValue::Text(crit.text.as_str().into()));
            }
        }
        let Some(stats) = self.map.get(&query) else {
            return Some(Self::empty_answer(function));
        };
        Some(match function {
            1 => FValue::number(stats.count_num as f64),
            2 | 3 => {
                if stats.count_num == 0 {
                    FValue::Error(ErrKind::Div0)
                } else {
                    FValue::number(stats.sum / stats.count_num as f64)
                }
            }
            4 => FValue::number(if stats.count_num == 0 { 0.0 } else { stats.min }),
            5 => FValue::number(if stats.count_num == 0 { 0.0 } else { stats.max }),
            _ => FValue::number(stats.sum),
        })
    }

    #[inline]
    fn empty_answer(function: u8) -> FValue {
        match function {
            2 | 3 => FValue::Error(ErrKind::Div0),
            _ => FValue::number(0.0),
        }
    }
}

/// Group a repeated set of literal SUMIFS/COUNTIFS/AVERAGEIFS/MINIFS/MAXIFS ranges by equality criteria.
/// Each function slot shares its group budget across recently used range sets.
#[derive(Default)]
pub(super) struct GroupedCriteria {
    entries: std::collections::VecDeque<GroupedRange>,
}

struct GroupedRange {
    val_range: Option<super::RangeKey>,
    cond_ranges: Vec<super::RangeKey>,
    numeric: Vec<bool>,
    attempted: bool,
    groups: Option<std::sync::Arc<BuiltGroups>>,
}

#[inline]
pub(crate) fn range_cell_len(r: super::RangeKey) -> usize {
    if r.3 < r.1 || r.4 < r.2 {
        return 0;
    }
    ((r.3 - r.1 + 1) as usize).saturating_mul((r.4 - r.2 + 1) as usize)
}

impl GroupedCriteria {
    #[cfg(test)]
    pub(super) fn built_ranges(&self) -> usize {
        self.entries.iter().filter(|e| e.groups.is_some()).count()
    }
    #[cfg(test)]
    pub(super) fn entries_len(&self) -> usize { self.entries.len() }

    pub(super) fn lookup_lazy(
        &mut self,
        key: &super::CriteriaKey,
        shared_get: &mut dyn FnMut(Option<super::RangeKey>, &[super::RangeKey], &[bool]) -> Option<std::sync::Arc<BuiltGroups>>,
        shared_put: &mut dyn FnMut(Option<super::RangeKey>, Vec<super::RangeKey>, Vec<bool>, std::sync::Arc<BuiltGroups>),
        shared_build_from_cond: &mut dyn FnMut(
            Option<super::RangeKey>,
            &[super::RangeKey],
            &[bool],
            usize,
        ) -> Option<std::sync::Arc<BuiltGroups>>,
        load_col: &mut dyn FnMut(super::RangeKey) -> Option<std::sync::Arc<Vec<FValue>>>,
    ) -> Option<FValue> {
        let (val_range, cond_ranges) = key.split_ranges()?;
        if cond_ranges.len() != key.criteria.len() {
            return None;
        }
        let min_len = cond_ranges
            .iter()
            .copied()
            .chain(val_range)
            .map(range_cell_len)
            .min()?;
        if min_len < 256 {
            return None;
        }
        let mut parsed = Vec::with_capacity(key.criteria.len());
        let mut numeric = Vec::with_capacity(key.criteria.len());
        for text in &key.criteria {
            let crit = Crit::parse(text);
            if !crit.indexable() {
                return None;
            }
            if let Some(n) = crit.num && !n.is_finite() {
                return None;
            }
            numeric.push(crit.num.is_some());
            parsed.push(crit);
        }
        if let Some(shared) = shared_get(val_range, cond_ranges, &numeric) {
            if !self.entries.iter().any(|e| e.val_range == val_range && e.cond_ranges == cond_ranges && e.numeric == numeric) {
                if self.entries.len() == 4 {
                    self.entries.pop_front();
                }
                self.entries.push_back(GroupedRange {
                    val_range,
                    cond_ranges: cond_ranges.to_vec(),
                    numeric,
                    attempted: true,
                    groups: Some(shared.clone()),
                });
            }
            return shared.answer(key.function, &parsed);
        }
        let Some(slot) = self.entries.iter().position(|e| {
            e.val_range == val_range && e.cond_ranges == cond_ranges && e.numeric == numeric
        }) else {
            if self.entries.len() == 4 {
                self.entries.pop_front();
            }
            self.entries.push_back(GroupedRange {
                val_range,
                cond_ranges: cond_ranges.to_vec(),
                numeric,
                groups: None,
                attempted: false,
            });
            return None;
        };
        let mut entry = self.entries.remove(slot)?;
        if !entry.attempted {
            let used: usize = self
                .entries
                .iter()
                .filter_map(|e| e.groups.as_ref())
                .map(|g| g.map.len())
                .sum();
            let limit = 65_536usize.saturating_sub(used);
            let built = if let Some(b) =
                shared_build_from_cond(val_range, cond_ranges, &entry.numeric, limit)
            {
                entry.attempted = true;
                Some(b)
            } else {
                let mut cols_owned = Vec::with_capacity(cond_ranges.len());
                for &rk in cond_ranges {
                    let Some(col) = load_col(rk) else {
                        self.entries.push_back(entry);
                        return None;
                    };
                    cols_owned.push(col);
                }
                let amounts_owned = match val_range {
                    Some(vr) => {
                        let Some(col) = load_col(vr) else {
                            self.entries.push_back(entry);
                            return None;
                        };
                        Some(col)
                    }
                    None => None,
                };
                entry.attempted = true;
                let col_slices: Vec<&[FValue]> = cols_owned.iter().map(|c| c.as_slice()).collect();
                entry
                    .build_groups(&col_slices, amounts_owned.as_deref().map(Vec::as_slice), limit)
                    .map(std::sync::Arc::new)
            };
            if let Some(ref g) = built {
                shared_put(entry.val_range, entry.cond_ranges.clone(), entry.numeric.clone(), g.clone());
            }
            entry.groups = built;
        }
        let result = entry.groups.as_ref().and_then(|g| g.answer(key.function, &parsed));
        self.entries.push_back(entry);
        result
    }
}

impl GroupedRange {
    fn build_groups(
        &self,
        columns: &[&[FValue]],
        amounts: Option<&[FValue]>,
        limit: usize,
    ) -> Option<BuiltGroups> {
        let rows = columns
            .iter()
            .map(|col| col.len())
            .min()?
            .min(amounts.map_or(usize::MAX, |v| v.len()));
        let eq_num = |v: &FValue| -> Option<f64> {
            match v {
                FValue::Number(n) => Some(*n),
                FValue::Text(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            }
        };
        if let Some(vals) = amounts
            && vals[..rows].iter().any(FValue::is_error)
        {
            return None;
        }
        let mut distinct_nums: Vec<Option<Vec<f64>>> = Vec::with_capacity(columns.len());
        for (col, &is_num) in columns.iter().zip(&self.numeric) {
            if !is_num {
                distinct_nums.push(None);
                continue;
            }
            let mut any_fract = false;
            for v in &col[..rows] {
                if let Some(n) = eq_num(v) {
                    if !n.is_finite() {
                        return None;
                    }
                    if n.fract() != 0.0 {
                        any_fract = true;
                    }
                }
            }
            if !any_fract {
                distinct_nums.push(None);
            } else {
                let mut vals = Vec::new();
                for v in &col[..rows] {
                    if let Some(n) = eq_num(v) {
                        vals.push(if n == 0.0 { 0.0 } else { n });
                    }
                }
                vals.sort_unstable_by(f64::total_cmp);
                vals.dedup_by(|a, b| a.to_bits() == b.to_bits());
                if vals.windows(2).any(|w| w[1] - w[0] <= 2.0 * f64::EPSILON) {
                    return None;
                }
                distinct_nums.push(Some(vals));
            }
        }

        let mut map: FxHashMap<Vec<GroupValue>, GroupStats> = FxHashMap::default();
        let mut key = Vec::with_capacity(columns.len());
        'row: for row in 0..rows {
            key.clear();
            for (j, (col, &numeric)) in columns.iter().zip(&self.numeric).enumerate() {
                let value = &col[row];
                key.push(if numeric {
                    let Some(n) = eq_num(value) else { continue 'row };
                    let canonical = if let Some(ref vals) = distinct_nums[j] {
                        let c = if n == 0.0 { 0.0 } else { n };
                        let idx = vals.binary_search_by(|x| x.total_cmp(&c)).ok()?;
                        vals[idx]
                    } else {
                        n
                    };
                    GroupValue::Number(GroupValue::canonical_bits(canonical))
                } else {
                    let text: std::sync::Arc<str> = match value {
                        FValue::Text(s) => super::lookup_cache::lowercase_arc(s),
                        FValue::Empty => "".into(),
                        FValue::Bool(true) => "true".into(),
                        FValue::Bool(false) => "false".into(),
                        FValue::Error(e) => e.text().to_ascii_lowercase().into(),
                        FValue::Number(_) => continue 'row,
                        other => other.display().to_lowercase().into(),
                    };
                    GroupValue::Text(text)
                });
            }
            let (add_sum, add_min, add_max, add_cnt) = match amounts {
                None => (1.0, 1.0, 1.0, 1u32),
                Some(values) => match values[row] {
                    FValue::Number(n) => (n, n, n, 1u32),
                    _ => (0.0, f64::INFINITY, f64::NEG_INFINITY, 0u32),
                },
            };
            if let Some(stats) = map.get_mut(&key) {
                stats.sum += add_sum;
                if add_cnt > 0 {
                    if stats.count_num == 0 {
                        stats.min = add_min;
                        stats.max = add_max;
                    } else {
                        stats.min = stats.min.min(add_min);
                        stats.max = stats.max.max(add_max);
                    }
                    stats.count_num += add_cnt;
                }
            } else {
                if map.len() >= limit {
                    return None;
                }
                map.insert(
                    std::mem::take(&mut key),
                    GroupStats {
                        sum: add_sum,
                        min: if add_cnt > 0 { add_min } else { f64::INFINITY },
                        max: if add_cnt > 0 { add_max } else { f64::NEG_INFINITY },
                        count_num: add_cnt,
                    },
                );
            }
        }
        Some(BuiltGroups { distinct_nums, map })
    }
}

/// Matching row offsets for repeated COUNTIF and SUMIF equality queries.
#[derive(Default)]
pub(super) struct CountIndex {
    numbers: Vec<(f64, usize)>,
    numbers_ready: bool,
    text: FxHashMap<String, Vec<usize>>,
    empty_only_count: usize,
    empty_rows_indexed: bool,
    text_ready: bool,
}

#[test]
fn count_index_agrees_with_single_criterion_scan() {
    let data = vec![
        FValue::Empty, FValue::Text("".into()), FValue::Number(0.0), FValue::Number(-0.0),
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
    #[allow(dead_code)]
    pub(super) fn new(data: &[FValue]) -> Self {
        let mut index = Self::default();
        index.ensure_numbers(data);
        index.ensure_text(data);
        index
    }

    fn ensure_numbers(&mut self, data: &[FValue]) {
        if self.numbers_ready {
            return;
        }
        for (row, value) in data.iter().enumerate() {
            match value {
                FValue::Number(n) if n.is_finite() => self.numbers.push((*n, row)),
                FValue::Text(s) => {
                    if let Ok(n) = s.trim().parse::<f64>() {
                        if n.is_finite() {
                            self.numbers.push((n, row));
                        }
                    }
                }
                _ => {}
            }
        }
        self.numbers
            .sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        self.numbers_ready = true;
    }

    fn ensure_text(&mut self, data: &[FValue]) {
        if self.text_ready && self.empty_rows_indexed {
            return;
        }
        self.text.clear();
        self.empty_only_count = 0;
        for (row, value) in data.iter().enumerate() {
            let text = match value {
                FValue::Number(_) => continue,
                FValue::Text(s) => {
                    if s.is_ascii() {
                        s.to_ascii_lowercase()
                    } else {
                        s.to_lowercase()
                    }
                }
                FValue::Bool(b) => b.to_string(),
                FValue::Error(e) => e.text().to_ascii_lowercase(),
                FValue::Empty => String::new(),
                _ => continue,
            };
            self.text.entry(text).or_default().push(row);
        }
        self.empty_rows_indexed = true;
        self.text_ready = true;
    }

    pub(super) fn from_streamed_text(
        text: FxHashMap<String, Vec<usize>>,
        empty_only_count: usize,
    ) -> Self {
        Self {
            numbers: Vec::new(),
            numbers_ready: false,
            text,
            empty_only_count,
            empty_rows_indexed: empty_only_count == 0,
            text_ready: true,
        }
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
        let extra_empty = if crit.num.is_none() && crit.text.is_empty() {
            self.empty_only_count
        } else {
            0
        };
        nums.len() + text.len() + extra_empty
    }

    pub(super) fn rows(&self, crit: &Crit) -> Option<impl Iterator<Item = usize> + '_> {
        if crit.num.is_none() && crit.text.is_empty() && !self.empty_rows_indexed {
            return None;
        }
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
    ranges: FxHashMap<super::RangeKey, Option<CountIndex>>,
    indexed_values: usize,
}

impl CountIndexes {
    #[inline]
    pub(super) fn lookup_ready(&self, range: super::RangeKey, crit: &Crit) -> Option<&CountIndex> {
        if !crit.indexable() {
            return None;
        }
        let ix = self.ranges.get(&range)?.as_ref()?;
        if crit.num.is_some() {
            ix.numbers_ready.then_some(ix)
        } else {
            ix.text_ready.then_some(ix)
        }
    }

    pub(super) fn insert_streamed_text(
        &mut self,
        range: super::RangeKey,
        indexed_entries: usize,
        text: FxHashMap<String, Vec<usize>>,
        empty_only_count: usize,
    ) {
        const LIMIT: usize = 2_097_152;
        if indexed_entries > LIMIT {
            return;
        }
        if self.ranges.len() >= 4096 || self.indexed_values + indexed_entries > LIMIT {
            self.ranges.clear();
            self.indexed_values = 0;
        }
        self.ranges
            .insert(range, Some(CountIndex::from_streamed_text(text, empty_only_count)));
        self.indexed_values += indexed_entries;
    }

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
            self.ranges.insert(range, Some(CountIndex::default()));
            self.indexed_values += data.len();
        }
        let index = self.ranges.get_mut(&range)?.as_mut()?;
        if crit.num.is_some() {
            index.ensure_numbers(data);
        } else {
            index.ensure_text(data);
        }
        Some(index)
    }
}

impl Crit {
    pub(super) fn numeric_query(&self) -> Option<(&'static str,f64)> {
        self.num.map(|n| (self.op,n))
    }

    pub(super) fn numeric_bounds(&self) -> Option<(&'static str, f64)> {
        let n = self.num?;
        (n.is_finite() && matches!(self.op, ">" | "<" | ">=" | "<=" | "<>")).then_some((self.op, n))
    }

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
            wild: is_wildcard(rest),
        }
    }

    #[inline]
    pub(crate) fn matches(&self, v: &FValue) -> bool {
        if let Some(t) = self.num {
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
        match v {
            FValue::Text(s) => {
                if self.wild && matches!(self.op, "=" | "<>") {
                    let m = wildcard_match_ci(&self.text, s);
                    return if self.op == "<>" { !m } else { m };
                }
                let ord = super::value::compare_text(s, &self.text);
                match self.op {
                    "<>" => !ord.is_eq(),
                    ">" => ord.is_gt(),
                    "<" => ord.is_lt(),
                    ">=" => !ord.is_lt(),
                    "<=" => !ord.is_gt(),
                    _ => ord.is_eq(),
                }
            }
            FValue::Empty => match self.op {
                "=" => self.text.is_empty(),
                "<>" => !self.text.is_empty(),
                _ => false,
            },
            FValue::Bool(b) => {
                let eq = (if *b { "true" } else { "false" }) == self.text;
                match self.op {
                    "=" => eq,
                    "<>" => !eq,
                    _ => false,
                }
            }
            FValue::Error(e) => {
                let eq = e.text().eq_ignore_ascii_case(&self.text);
                match self.op {
                    "=" => eq,
                    "<>" => !eq,
                    _ => false,
                }
            }
            // When `self.num.is_none()`, the criterion text failed `parse::<f64>()`,
            // so no numeric cell can equal `self.text`, match wildcards, or satisfy text `<`/`>`.
            FValue::Number(_) => self.op == "<>",
            _ => self.op == "<>",
        }
    }

    /// Zero-conversion criterion match directly on a raw `f64` from `ColSlab::Numbers`.
    #[inline]
    pub(crate) fn matches_number(&self, n: f64) -> bool {
        if let Some(t) = self.num {
            return match self.op {
                ">=" => n >= t,
                "<=" => n <= t,
                ">" => n > t,
                "<" => n < t,
                "<>" => (n - t).abs() > f64::EPSILON,
                _ => (n - t).abs() <= f64::EPSILON,
            };
        }
        self.op == "<>"
    }

    /// Zero-conversion criterion match directly on a stored `crate::Value` in a Sheet page slab.
    #[inline]
    pub(crate) fn matches_value(&self, v: &crate::Value) -> bool {
        if let Some(t) = self.num {
            let n = match v {
                crate::Value::Number(n) => *n,
                crate::Value::Text(s) if self.op == "=" || self.op == "<>" => {
                    match s.trim().parse::<f64>() {
                        Ok(n) => n,
                        Err(_) => return self.op == "<>",
                    }
                }
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
        match v {
            crate::Value::Text(s) => {
                if self.wild && matches!(self.op, "=" | "<>") {
                    let m = wildcard_match_ci(&self.text, s);
                    return if self.op == "<>" { !m } else { m };
                }
                let ord = super::value::compare_text(s, &self.text);
                match self.op {
                    "<>" => !ord.is_eq(),
                    ">" => ord.is_gt(),
                    "<" => ord.is_lt(),
                    ">=" => !ord.is_lt(),
                    "<=" => !ord.is_gt(),
                    _ => ord.is_eq(),
                }
            }
            crate::Value::Empty => match self.op {
                "=" => self.text.is_empty(),
                "<>" => !self.text.is_empty(),
                _ => false,
            },
            crate::Value::Bool(b) => {
                let eq = (if *b { "true" } else { "false" }) == self.text;
                match self.op {
                    "=" => eq,
                    "<>" => !eq,
                    _ => false,
                }
            }
            crate::Value::Error(e) => {
                let eq = e.text().eq_ignore_ascii_case(&self.text);
                match self.op {
                    "=" => eq,
                    "<>" => !eq,
                    _ => false,
                }
            }
            crate::Value::Number(_) => self.op == "<>",
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
        if let FValue::Number(n) = v
            && n.is_finite()
            && !self.wild
            && matches!(self.op, "=" | "<>")
        {
            return self.op == "<>";
        }
        let display;
        let text = match v {
            FValue::Text(s) => s.as_ref(), FValue::Empty => "",
            FValue::Bool(true) => "true", FValue::Bool(false) => "false",
            _ => { display = v.display(); &display }
        };
        self.matches_coerced_text(text)
    }

    pub(super) fn matches_coerced_text(&self, text: &str) -> bool {
        if self.wild && matches!(self.op, "=" | "<>") {
            let matched = wildcard_match_ci(&self.text, text);
            return matched == (self.op == "=");
        }
        let order = super::value::compare_text(text, &self.text);
        match self.op {
            "<>" => !order.is_eq(), ">" => order.is_gt(), "<" => order.is_lt(),
            ">=" => !order.is_lt(), "<=" => !order.is_gt(), _ => order.is_eq(),
        }
    }
}

/// 统一零分配、线性双指针大小写不敏感通配符匹配：支持 `*`、`?` 与 `~` 转义，
/// 无 `Vec<char>` 堆分配，无多重 `*` 指数级回溯，且保持希腊词尾 `Σ -> ς` 上下文语义。
pub(crate) fn wildcard_match_ci(pat: &str, s: &str) -> bool {
    if s.is_ascii() && pat.is_ascii() {
        return wildcard_ascii_ci(s.as_bytes(), pat.as_bytes());
    }
    if s.contains('Σ') || pat.contains('Σ') {
        let s_low = s.to_lowercase();
        let p_low = pat.to_lowercase();
        return wildcard_unicode_ci(&s_low, &p_low);
    }
    wildcard_unicode_ci(s, pat)
}

fn wildcard_ascii_ci(text: &[u8], pat: &[u8]) -> bool {
    let (mut ti, mut pi) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_ti: usize = 0;
    while ti < text.len() {
        if pi < pat.len() {
            match pat[pi] {
                b'*' => {
                    while pi < pat.len() && pat[pi] == b'*' {
                        pi += 1;
                    }
                    if pi == pat.len() {
                        return true;
                    }
                    star_pi = Some(pi);
                    star_ti = ti;
                    continue;
                }
                b'?' => {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                b'~' => {
                    let lit = if pi + 1 < pat.len() {
                        pi += 2;
                        pat[pi - 1]
                    } else {
                        pi += 1;
                        b'~'
                    };
                    if text[ti].eq_ignore_ascii_case(&lit) {
                        ti += 1;
                        continue;
                    }
                }
                c if text[ti].eq_ignore_ascii_case(&c) => {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(sp) = star_pi {
            star_ti += 1;
            ti = star_ti;
            pi = sp;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

fn wildcard_unicode_ci(text: &str, pat: &str) -> bool {
    let lower_fn: fn(char) -> std::char::ToLowercase = char::to_lowercase;
    let mut t_it = text.chars().flat_map(lower_fn);
    let mut p_it = pat.chars().flat_map(lower_fn);
    let mut star: Option<(
        std::iter::FlatMap<std::str::Chars<'_>, std::char::ToLowercase, fn(char) -> std::char::ToLowercase>,
        std::iter::FlatMap<std::str::Chars<'_>, std::char::ToLowercase, fn(char) -> std::char::ToLowercase>,
    )> = None;
    loop {
        let mut p_peek = p_it.clone();
        match p_peek.next() {
            Some('*') => {
                p_it = p_peek;
                loop {
                    let mut next_p = p_it.clone();
                    if next_p.next() == Some('*') {
                        p_it = next_p;
                    } else {
                        break;
                    }
                }
                if p_it.clone().next().is_none() {
                    return true;
                }
                star = Some((t_it.clone(), p_it.clone()));
                continue;
            }
            Some('?') => {
                if t_it.next().is_some() {
                    p_it = p_peek;
                    continue;
                }
            }
            Some('~') => {
                let lit = p_peek.next().unwrap_or('~');
                let mut t_peek = t_it.clone();
                if t_peek.next() == Some(lit) {
                    t_it = t_peek;
                    p_it = p_peek;
                    continue;
                }
            }
            Some(pc) => {
                let mut t_peek = t_it.clone();
                if t_peek.next() == Some(pc) {
                    t_it = t_peek;
                    p_it = p_peek;
                    continue;
                }
            }
            None => {
                if t_it.clone().next().is_none() {
                    return true;
                }
            }
        }
        let Some((retry_t, retry_p)) = star.as_mut() else {
            return false;
        };
        if retry_t.next().is_none() {
            return false;
        }
        t_it = retry_t.clone();
        p_it = retry_p.clone();
    }
}

pub(crate) fn match_criteria(v: &FValue, crit: &str) -> bool {
    Crit::parse(crit).matches(v)
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

pub(super) fn xlookup_search(
    data: &[FValue],
    len: usize,
    stride: usize,
    positions: Option<&[usize]>,
    needle: &FValue,
    match_mode: i64,
    search_mode: i64,
) -> Result<Option<usize>, ErrKind> {
    use std::cmp::Ordering;
    if !matches!(match_mode, -1..=2) || !matches!(search_mode, -2 | -1 | 1 | 2) {
        return Err(ErrKind::Value);
    }
    if match_mode == 2 && search_mode.abs() == 2 {
        return Err(ErrKind::Value);
    }
    if search_mode.abs() == 2 {
        let ascending = search_mode == 2;
        let Some(kind) = LookupKind::of(needle) else {
            return Ok(None);
        };
        let owned_pos;
        let pos = match positions {
            Some(p) => Some(p),
            None => {
                owned_pos = kind.positions(data, len, stride);
                owned_pos.as_deref()
            }
        };
        let needle_cmp = if matches!(needle, FValue::Empty) {
            &FValue::Number(0.0)
        } else {
            needle
        };
        let index = |i: usize| pos.map_or(i, |p| p[i]);
        let count = pos.map_or(len, |p| p.len());
        if count == 0 {
            return Ok(None);
        }
        let (mut lo, mut hi) = (0usize, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let Ok(ord) = data[index(mid) * stride].compare(needle_cmp) else {
                return Ok(None);
            };
            let go_right = if ascending { ord.is_lt() } else { ord.is_gt() };
            if go_right {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < count && matches!(data[index(lo) * stride].compare(needle_cmp), Ok(Ordering::Equal)) {
            return Ok(Some(index(lo)));
        }
        return Ok(match (match_mode, ascending) {
            (0, _) => None,
            (-1, true) => lo.checked_sub(1).map(index),
            (1, true) => (lo < count).then(|| index(lo)),
            (-1, false) => (lo < count).then(|| index(lo)),
            (1, false) => lo.checked_sub(1).map(index),
            _ => None,
        });
    }

    let forward = search_mode >= 0;
    let needle_kind = LookupKind::of(needle);
    let mut best: Option<(usize, &FValue)> = None;
    for step in 0..len {
        let i = if forward { step } else { len - 1 - step };
        let Some(cell) = data.get(i * stride) else { continue };
        if match_mode == 2 {
            let hit = match (cell, needle) {
                (FValue::Text(a), FValue::Text(b)) => wildcard_match_ci(b, a),
                (FValue::Empty, FValue::Empty) => true,
                (FValue::Empty, _) | (_, FValue::Empty) => false,
                _ => matches!(cell.compare(needle), Ok(Ordering::Equal)),
            };
            if hit {
                return Ok(Some(i));
            }
            continue;
        }
        if matches!(cell, FValue::Empty) != matches!(needle, FValue::Empty) {
            continue;
        }
        if match_mode == 0 {
            if matches!(cell.compare(needle), Ok(Ordering::Equal)) {
                return Ok(Some(i));
            }
            continue;
        }
        if !matches!(needle, FValue::Empty) && LookupKind::of(cell) != needle_kind {
            continue;
        }
        let Ok(ord) = cell.compare(needle) else { continue };
        match ord {
            Ordering::Equal => return Ok(Some(i)),
            Ordering::Less if match_mode == -1 => {
                let replace = match best {
                    None => true,
                    Some((_, prev)) => matches!(cell.compare(prev), Ok(Ordering::Greater)),
                };
                if replace {
                    best = Some((i, cell));
                }
            }
            Ordering::Greater if match_mode == 1 => {
                let replace = match best {
                    None => true,
                    Some((_, prev)) => matches!(cell.compare(prev), Ok(Ordering::Less)),
                };
                if replace {
                    best = Some((i, cell));
                }
            }
            _ => {}
        }
    }
    Ok(best.map(|(i, _)| i))
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
pub(super) fn round_sig15(x: f64) -> f64 {
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
    call_in(EvalCtx::from_ambient(), name, args)
}

pub(crate) fn call_in(ctx: EvalCtx, name: &str, args: &[Arg]) -> FValue {
    call_or_else_in(ctx, name, args, &mut || FValue::Error(ErrKind::Name))
}

#[allow(dead_code)]
pub(super) fn call_or_else(name: &str, args: &[Arg], unknown: &mut dyn FnMut() -> FValue) -> FValue {
    call_or_else_in(EvalCtx::from_ambient(), name, args, unknown)
}

pub(crate) fn call_id_in(ctx: EvalCtx, id: FuncId, name: &str, args: &[Arg]) -> FValue {
    call_by_id_in(ctx, id, name, args, &mut || FValue::Error(ErrKind::Name))
}

pub(super) fn call_or_else_in(
    ctx: EvalCtx,
    name: &str,
    args: &[Arg],
    unknown: &mut dyn FnMut() -> FValue,
) -> FValue {
    call_by_id_in(ctx, FuncId::from_upper(name), name, args, unknown)
}

pub(super) fn call_by_id_in(
    ctx: EvalCtx,
    id: FuncId,
    name: &str,
    args: &[Arg],
    unknown: &mut dyn FnMut() -> FValue,
) -> FValue {
    match id {
        // ---------------- 数学 / 三角
        FuncId::Sum => r(|| {
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
        FuncId::Product => r(|| {
            let v = numbers(args, 0)?;
            if v.is_empty() {
                Ok(FValue::number(0.0))
            } else {
                Ok(FValue::number(v.iter().product()))
            }
        }),
        FuncId::Abs => r(|| Ok(FValue::number(num(args, 0)?.abs()))),
        FuncId::Sign => r(|| Ok(FValue::number(num(args, 0)?.signum()))),
        FuncId::Int => r(|| Ok(FValue::number(num(args, 0)?.floor()))),
        FuncId::Trunc => r(|| {
            let n = num(args, 0)?;
            let d = opt_num(args, 1, 0.0)?;
            let f = 10f64.powi(d as i32);
            Ok(FValue::number((n * f).trunc() / f))
        }),
        FuncId::Round => r(|| {
            let n = num(args, 0)?;
            let d = int_arg(args, 1)?;
            let f = 10f64.powi(d as i32);
            Ok(FValue::number(round_sig15(n * f).round() / f))
        }),
        FuncId::RoundUp => r(|| {
            let n = num(args, 0)?;
            let d = int_arg(args, 1)?;
            let f = 10f64.powi(d as i32);
            let x = round_sig15(n * f);
            Ok(FValue::number(if x >= 0.0 { x.ceil() } else { x.floor() } / f))
        }),
        FuncId::RoundDown => r(|| {
            let n = num(args, 0)?;
            let d = int_arg(args, 1)?;
            let f = 10f64.powi(d as i32);
            Ok(FValue::number((n * f).trunc() / f))
        }),
        FuncId::Mod => r(|| {
            let a = num(args, 0)?;
            let b = num(args, 1)?;
            if b == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number(a - b * (a / b).floor()))
        }),
        FuncId::Quotient => r(|| {
            let a = num(args, 0)?;
            let b = num(args, 1)?;
            if b == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number((a / b).trunc()))
        }),
        FuncId::Power => r(|| {
            let a = num(args, 0)?;
            let b = num(args, 1)?;
            if a == 0.0 {
                if b == 0.0 {
                    return Err(ErrKind::Num);
                }
                if b < 0.0 {
                    return Err(ErrKind::Div0);
                }
            }
            Ok(FValue::number(a.powf(b)))
        }),
        FuncId::Sqrt => r(|| {
            let n = num(args, 0)?;
            if n < 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.sqrt()))
        }),
        FuncId::SqrtPi => r(|| {
            let n = num(args, 0)?;
            if n < 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((n * std::f64::consts::PI).sqrt()))
        }),
        FuncId::Exp => r(|| Ok(FValue::number(num(args, 0)?.exp()))),
        FuncId::Ln => r(|| {
            let n = num(args, 0)?;
            if n <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.ln()))
        }),
        FuncId::Log10 => r(|| {
            let n = num(args, 0)?;
            if n <= 0.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.log10()))
        }),
        FuncId::Log => r(|| {
            let n = num(args, 0)?;
            let b = opt_num(args, 1, 10.0)?;
            if n <= 0.0 || b <= 0.0 || b == 1.0 {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(n.log(b)))
        }),
        FuncId::Pi => FValue::number(std::f64::consts::PI),
        FuncId::Sin => r(|| Ok(FValue::number(num(args, 0)?.sin()))),
        FuncId::Cos => r(|| Ok(FValue::number(num(args, 0)?.cos()))),
        FuncId::Tan => r(|| Ok(FValue::number(num(args, 0)?.tan()))),
        FuncId::Asin => r(|| Ok(FValue::number(num(args, 0)?.asin()))),
        FuncId::Acos => r(|| Ok(FValue::number(num(args, 0)?.acos()))),
        FuncId::Atan => r(|| Ok(FValue::number(num(args, 0)?.atan()))),
        FuncId::Atan2 => r(|| Ok(FValue::number(num(args, 0)?.atan2(num(args, 1)?)))),
        FuncId::Sinh => r(|| Ok(FValue::number(num(args, 0)?.sinh()))),
        FuncId::Cosh => r(|| Ok(FValue::number(num(args, 0)?.cosh()))),
        FuncId::Tanh => r(|| Ok(FValue::number(num(args, 0)?.tanh()))),
        FuncId::Degrees => r(|| Ok(FValue::number(num(args, 0)?.to_degrees()))),
        FuncId::Radians => r(|| Ok(FValue::number(num(args, 0)?.to_radians()))),
        FuncId::Fact => r(|| {
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
        FuncId::Gcd => r(|| {
            let v = numbers(args, 0)?;
            let mut g = 0i64;
            for x in v {
                g = gcd(g, x.abs() as i64);
            }
            Ok(FValue::number(g as f64))
        }),
        FuncId::Lcm => r(|| {
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
        FuncId::Combin => r(|| {
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
        FuncId::Ceiling | FuncId::CeilingMath => r(|| {
            let n = num(args, 0)?;
            let s = opt_num(args, 1, 1.0)?;
            if s == 0.0 {
                return Ok(FValue::number(0.0));
            }
            Ok(FValue::number((n / s).ceil() * s))
        }),
        FuncId::Floor | FuncId::FloorMath => r(|| {
            let n = num(args, 0)?;
            let s = opt_num(args, 1, 1.0)?;
            if s == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number((n / s).floor() * s))
        }),
        FuncId::MRound => r(|| {
            let n = num(args, 0)?;
            let s = num(args, 1)?;
            if s == 0.0 {
                return Ok(FValue::number(0.0));
            }
            Ok(FValue::number((n / s).round() * s))
        }),
        FuncId::Even => r(|| {
            let n = num(args, 0)?;
            let v = (n.abs() / 2.0).ceil() * 2.0;
            Ok(FValue::number(if n < 0.0 { -v } else { v }))
        }),
        FuncId::Odd => r(|| {
            let n = num(args, 0)?;
            let mut v = (n.abs() / 2.0).ceil() * 2.0 - 1.0;
            if v == 0.0 {
                v = 1.0;
            }
            Ok(FValue::number(if n < 0.0 { -v } else { v }))
        }),
        FuncId::Rand => FValue::number(rand_f64()),
        FuncId::RandBetween => r(|| {
            let a = int_arg(args, 0)?;
            let b = int_arg(args, 1)?;
            if a > b {
                return Err(ErrKind::Num);
            }
            let span = (b - a + 1) as f64;
            Ok(FValue::number((a as f64 + (rand_f64() * span).floor()).min(b as f64)))
        }),
        FuncId::SumSq => r(|| Ok(FValue::number(numbers(args, 0)?.iter().map(|x| x * x).sum()))),
        FuncId::SumProduct => r(|| {
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
            let total = rows * cols;
            let cell_num = |v: &FValue| -> Result<f64, ErrKind> {
                match v {
                    FValue::Error(e) => Err(*e),
                    FValue::Number(n) => Ok(*n),
                    _ => Ok(0.0),
                }
            };
            if grids.len() == 2 && grids[0].0 == rows && grids[0].1 == cols && grids[1].0 == rows && grids[1].1 == cols {
                let a = &grids[0].2[..total];
                let b = &grids[1].2[..total];
                let mut sum = 0.0;
                for i in 0..total {
                    sum += cell_num(&a[i])? * cell_num(&b[i])?;
                }
                return Ok(FValue::number(sum));
            }
            if grids.len() == 3 && grids.iter().all(|(r, c, _)| *r == rows && *c == cols) {
                let a = &grids[0].2[..total];
                let b = &grids[1].2[..total];
                let c = &grids[2].2[..total];
                let mut sum = 0.0;
                for i in 0..total {
                    sum += cell_num(&a[i])? * cell_num(&b[i])? * cell_num(&c[i])?;
                }
                return Ok(FValue::number(sum));
            }
            let mut sum = 0.0;
            for i in 0..total {
                let mut p = 1.0;
                for (r, c, data) in &grids {
                    let idx = if *r == 1 && *c == 1 { 0 } else { i };
                    p *= cell_num(&data[idx])?;
                }
                sum += p;
            }
            Ok(FValue::number(sum))
        }),
        FuncId::SumIf => r(|| {
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
        FuncId::SumIfs => r(|| {
            let sum_range = args.first().map(|a| a.slice()).unwrap_or(&[]);
            let mut s = 0.0;
            let pairs = criteria_pairs(args.get(1..).unwrap_or_default());
            for (i, v) in sum_range.iter().enumerate() {
                if pairs.iter().all(|(rg, c)| rg.get(i).is_some_and(|x| c.matches(x))) {
                    match v {
                        FValue::Number(n) => s += n,
                        FValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            Ok(FValue::number(s))
        }),
        FuncId::Subtotal => r(|| {
            let f = int_arg(args, 0)?;
            let base = f % 100;
            let sub = &args[1.min(args.len())..];
            Ok(match base {
                1 => call_in(ctx, "AVERAGE", sub),
                2 => call_in(ctx, "COUNT", sub),
                3 => call_in(ctx, "COUNTA", sub),
                4 => call_in(ctx, "MAX", sub),
                5 => call_in(ctx, "MIN", sub),
                6 => call_in(ctx, "PRODUCT", sub),
                9 => call_in(ctx, "SUM", sub),
                7 => call_in(ctx, "STDEV", sub),
                8 => call_in(ctx, "STDEVP", sub),
                10 => call_in(ctx, "VAR", sub),
                11 => call_in(ctx, "VARP", sub),
                _ => FValue::Error(ErrKind::Value),
            })
        }),

        // ---------------- 统计
        FuncId::Average => r(|| {
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
        FuncId::AverageA => r(|| {
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
        FuncId::Count => r(|| {
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
        FuncId::CountA => r(|| {
            let n = args
                .iter()
                .flat_map(|a| a.slice())
                .filter(|v| !matches!(v, FValue::Empty))
                .count();
            Ok(FValue::number(n as f64))
        }),
        FuncId::CountBlank => r(|| {
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
        FuncId::CountIf => r(|| {
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
        FuncId::CountIfs => r(|| {
            let mut count = 0usize;
            let pairs = criteria_pairs(args);
            let len = pairs.iter().map(|(r, _)| r.len()).min().unwrap_or(0);
            for i in 0..len {
                if pairs.iter().all(|(rg, c)| rg.get(i).is_some_and(|x| c.matches(x))) {
                    count += 1;
                }
            }
            Ok(FValue::number(count as f64))
        }),
        FuncId::AverageIf => r(|| {
            let crit = Crit::parse(&text(args, 1)?);
            let target = args.first().map(|a| a.slice()).unwrap_or(&[]);
            let vals = args.get(2).map(|a| a.slice()).unwrap_or(target);
            let mut s = 0.0;
            let mut n = 0.0;
            for (i, v) in target.iter().enumerate() {
                if crit.matches(v) {
                    if let Some(sv) = vals.get(i) {
                        match sv {
                            FValue::Number(x) => {
                                s += x;
                                n += 1.0;
                            }
                            FValue::Error(e) => return Err(*e),
                            _ => {}
                        }
                    }
                }
            }
            if n == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number(s / n))
        }),
        FuncId::Max => r(|| {
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
        FuncId::Min => r(|| {
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
        FuncId::MaxA | FuncId::MinA => r(|| {
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
        FuncId::Median => r(|| {
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
        FuncId::Mode => r(|| {
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
        FuncId::Large | FuncId::Small => r(|| {
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
        FuncId::Rank | FuncId::RankEq => r(|| {
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
        FuncId::Stdev | FuncId::StdevS | FuncId::StdevP | FuncId::StdevDotP => r(|| {
            let v = numbers(args, 0)?;
            let sample = name != "STDEVP" && name != "STDEV.P";
            Ok(FValue::number(std_dev(&v, sample)?))
        }),
        FuncId::Var | FuncId::VarS | FuncId::VarP | FuncId::VarDotP => r(|| {
            let v = numbers(args, 0)?;
            let sample = name != "VARP" && name != "VAR.P";
            let sd = std_dev(&v, sample)?;
            Ok(FValue::number(sd * sd))
        }),
        FuncId::AveDev => r(|| {
            let v = numbers(args, 0)?;
            if v.is_empty() {
                return Err(ErrKind::Num);
            }
            let m = v.iter().sum::<f64>() / v.len() as f64;
            Ok(FValue::number(v.iter().map(|x| (x - m).abs()).sum::<f64>() / v.len() as f64))
        }),
        FuncId::DevSq => r(|| {
            let v = numbers(args, 0)?;
            let m = v.iter().sum::<f64>() / v.len() as f64;
            Ok(FValue::number(v.iter().map(|x| (x - m).powi(2)).sum::<f64>()))
        }),
        FuncId::GeoMean => r(|| {
            let v = numbers(args, 0)?;
            if v.is_empty() || v.iter().any(|x| *x <= 0.0) {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number((v.iter().map(|x| x.ln()).sum::<f64>() / v.len() as f64).exp()))
        }),
        FuncId::HarMean => r(|| {
            let v = numbers(args, 0)?;
            if v.is_empty() || v.iter().any(|x| *x == 0.0) {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(v.len() as f64 / v.iter().map(|x| 1.0 / x).sum::<f64>()))
        }),
        FuncId::Percentile | FuncId::PercentileInc => r(|| {
            let mut v = first_numbers(args)?;
            let p = num(args, 1)?;
            if v.is_empty() || !(0.0..=1.0).contains(&p) {
                return Err(ErrKind::Num);
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            Ok(FValue::number(percentile(&v, p)))
        }),
        FuncId::Quartile | FuncId::QuartileInc => r(|| {
            let mut v = first_numbers(args)?;
            let q = int_arg(args, 1)?;
            if !(0..=4).contains(&q) {
                return Err(ErrKind::Num);
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            Ok(FValue::number(percentile(&v, q as f64 / 4.0)))
        }),
        FuncId::Correl => r(|| {
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
        FuncId::Slope => r(|| {
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
        FuncId::Intercept => r(|| {
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
        FuncId::Len => r(|| Ok(FValue::number(text(args, 0)?.chars().count() as f64))),
        FuncId::Left => r(|| {
            let s = text(args, 0)?;
            let n = opt_num(args, 1, 1.0)?.max(0.0) as usize;
            Ok(FValue::Text(s.chars().take(n).collect::<String>().into()))
        }),
        FuncId::Right => r(|| {
            let s = text(args, 0)?;
            let n = opt_num(args, 1, 1.0)?.max(0.0) as usize;
            let len = s.chars().count();
            let skip = len.saturating_sub(n);
            Ok(FValue::Text(s.chars().skip(skip).collect::<String>().into()))
        }),
        FuncId::Mid => r(|| {
            let s = text(args, 0)?;
            let start = num(args, 1)?.max(1.0) as usize;
            let n = num(args, 2)?.max(0.0) as usize;
            Ok(FValue::Text(s.chars().skip(start - 1).take(n).collect::<String>().into()))
        }),
        FuncId::Lower => r(|| Ok(FValue::Text(text(args, 0)?.to_lowercase().into()))),
        FuncId::Upper => r(|| Ok(FValue::Text(text(args, 0)?.to_uppercase().into()))),
        FuncId::Proper => r(|| {
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
            Ok(FValue::Text(out.into()))
        }),
        FuncId::Trim => r(|| {
            let s = text(args, 0)?;
            let parts: Vec<&str> = s.split_whitespace().collect();
            Ok(FValue::Text(parts.join(" ").into()))
        }),
        FuncId::Concat | FuncId::Concatenate => r(|| {
            let mut out = String::new();
            for a in args {
                for v in a.slice() {
                    out.push_str(&v.display());
                }
            }
            Ok(FValue::Text(out.into()))
        }),
        FuncId::TextJoin => r(|| {
            let sep = text(args, 0)?;
            let ignore_empty = arg(args, 1).to_bool()?;
            let mut parts = Vec::new();
            for a in args.iter().skip(2) {
                for v in a.slice() {
                    let s = v.display();
                    if ignore_empty && s.is_empty() {
                        continue;
                    }
                    parts.push(s);
                }
            }
            Ok(FValue::Text(parts.join(&sep).into()))
        }),
        FuncId::Rept => r(|| {
            let s = text(args, 0)?;
            let n_f = num(args, 1)?;
            if !n_f.is_finite() || n_f < 0.0 {
                return Err(ErrKind::Value);
            }
            let n_trunc = n_f.trunc();
            if s.is_empty() || n_trunc == 0.0 {
                return Ok(FValue::Text("".into()));
            }
            if n_trunc > 32_767.0 {
                return Err(ErrKind::Value);
            }
            let n = n_trunc as usize;
            let char_len = s.chars().count();
            if char_len.checked_mul(n).is_none_or(|t| t > 32_767)
                || s.len().checked_mul(n).is_none_or(|t| t > 32_767 * 4)
            {
                return Err(ErrKind::Value);
            }
            Ok(FValue::Text(s.repeat(n).into()))
        }),
        FuncId::Substitute => r(|| {
            let s = text(args, 0)?;
            let old = text(args, 1)?;
            let new = text(args, 2)?;
            if old.is_empty() {
                return Ok(FValue::Text(s.into()));
            }
            match args.get(3) {
                None => Ok(FValue::Text(s.replace(&old, &new).into())),
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
                            return Ok(FValue::Text(out.into()));
                        }
                        out.push_str(&old);
                        rest = &rest[pos + old.len()..];
                    }
                    out.push_str(rest);
                    Ok(FValue::Text(out.into()))
                }
            }
        }),
        FuncId::Replace => r(|| {
            let s = text(args, 0)?;
            let start = num(args, 1)?.max(1.0) as usize;
            let count = num(args, 2)?.max(0.0) as usize;
            let new = text(args, 3)?;
            let chars: Vec<char> = s.chars().collect();
            let mut out: String = chars.iter().take(start - 1).collect();
            out.push_str(&new);
            out.extend(chars.iter().skip((start - 1 + count).min(chars.len())));
            Ok(FValue::Text(out.into()))
        }),
        FuncId::Find | FuncId::Search => r(|| {
            let needle = text(args, 0)?;
            let hay = text(args, 1)?;
            let start_f = opt_num(args, 2, 1.0)?;
            if start_f < 1.0 {
                return Err(ErrKind::Value);
            }
            let start = start_f.trunc() as usize;
            let hay_len = hay.chars().count();
            if start > hay_len + 1 {
                return Err(ErrKind::Value);
            }
            let byte_off = hay
                .char_indices()
                .nth(start - 1)
                .map_or(hay.len(), |(idx, _)| idx);
            let hay_part = &hay[byte_off..];
            let found_chars = if name == "FIND" {
                hay_part
                    .find(&needle)
                    .map(|byte_pos| hay_part[..byte_pos].chars().count())
            } else {
                search_wildcard_char_pos(hay_part, &needle)
            };
            match found_chars {
                Some(prefix_chars) => Ok(FValue::number((start + prefix_chars) as f64)),
                None => Err(ErrKind::Value),
            }
        }),
        FuncId::Exact => r(|| Ok(FValue::Bool(text(args, 0)? == text(args, 1)?))),
        FuncId::Char => r(|| {
            let n = int_arg(args, 0)?;
            if !(1..=255).contains(&n) {
                return Err(ErrKind::Value);
            }
            Ok(FValue::Text(((n as u8) as char).to_string().into()))
        }),
        FuncId::Code => r(|| {
            let s = text(args, 0)?;
            let c = s.chars().next().ok_or(ErrKind::Value)?;
            Ok(FValue::number(c as u32 as f64))
        }),
        FuncId::Value => r(|| {
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
        FuncId::Text => r(|| {
            let v = arg(args, 0);
            if let FValue::Error(e) = v {
                return Err(e);
            }
            let fmt = text(args, 1)?;
            Ok(FValue::Text(format_value_in(ctx.date_system, &v, &fmt).into()))
        }),
        FuncId::T => r(|| match arg(args, 0) {
            FValue::Text(s) => Ok(FValue::Text(s)),
            _ => Ok(FValue::Text("".into())),
        }),

        // ---------------- 逻辑
        FuncId::True => FValue::Bool(true),
        FuncId::False => FValue::Bool(false),
        FuncId::Not => r(|| Ok(FValue::Bool(!arg(args, 0).to_bool()?))),
        FuncId::And => r(|| {
            let mut any = false;
            for a in args {
                for v in a.slice() {
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
        FuncId::Or => r(|| {
            let mut any = false;
            for a in args {
                for v in a.slice() {
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
        FuncId::Xor => r(|| {
            let mut t = false;
            for a in args {
                for v in a.slice() {
                    if matches!(v, FValue::Empty) {
                        continue;
                    }
                    t ^= v.to_bool()?;
                }
            }
            Ok(FValue::Bool(t))
        }),
        FuncId::If => {
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
        FuncId::IfError => {
            let v = arg(args, 0);
            if v.is_error() {
                arg(args, 1)
            } else {
                v
            }
        }
        FuncId::IfNa => {
            let v = arg(args, 0);
            if matches!(v, FValue::Error(ErrKind::Na)) {
                arg(args, 1)
            } else {
                v
            }
        }
        FuncId::Ifs => {
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
        FuncId::Switch => {
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
        FuncId::Row => r(|| {
            if args.is_empty() {
                return Err(ErrKind::Value);
            }
            Ok(FValue::number(1.0))
        }),
        FuncId::Column => r(|| {
            if args.is_empty() {
                return Err(ErrKind::Value);
            }
            Ok(FValue::number(1.0))
        }),
        FuncId::Rows => {
            let (rows, _) = range_dims(args.first().unwrap_or(&Arg::Val(FValue::Empty)));
            FValue::number(rows as f64)
        }
        FuncId::Columns => {
            let (_, cols) = range_dims(args.first().unwrap_or(&Arg::Val(FValue::Empty)));
            FValue::number(cols as f64)
        }
        FuncId::Choose => r(|| {
            let i = index_arg(args, 0)?;
            Ok(arg(args, i + 1))
        }),
        FuncId::VLookup | FuncId::HLookup => r(|| {
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
        FuncId::Match | FuncId::XMatch => r(|| {
            let needle = arg(args, 0);
            if let FValue::Error(e) = needle {
                return Err(e);
            }
            let lookup = args.get(1).ok_or(ErrKind::Value)?;
            let match_type = opt_num(args, 2, if name == "MATCH" { 1.0 } else { 0.0 })?;
            let vals = lookup.slice();
            if name == "MATCH" {
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
                return approximate_match(vals, vals.len(), 1, &needle, match_type > 0.0)
                    .map(|i| FValue::number((i + 1) as f64))
                    .ok_or(ErrKind::Na);
            }
            let search_mode = opt_num(args, 3, 1.0)?.trunc() as i64;
            let hit = xlookup_search(vals, vals.len(), 1, None, &needle, match_type.trunc() as i64, search_mode)?;
            hit.map(|i| FValue::number((i + 1) as f64)).ok_or(ErrKind::Na)
        }),
        FuncId::XLookup => r(|| {
            let needle = arg(args, 0);
            if let FValue::Error(e) = needle {
                return Err(e);
            }
            let lookup = args.get(1).ok_or(ErrKind::Value)?;
            let ret = args.get(2).ok_or(ErrKind::Value)?;
            let vals = lookup.slice();
            let (l_rows, l_cols) = range_dims(lookup);
            let (rows, cols) = range_dims(ret);
            let vertical = !(l_rows == 1 && l_cols > 1);
            let match_mode = opt_num(args, 4, 0.0)?.trunc() as i64;
            let search_mode = opt_num(args, 5, 1.0)?.trunc() as i64;
            if let Some(i) = xlookup_search(vals, vals.len(), 1, None, &needle, match_mode, search_mode)? {
                return Ok(if vertical {
                    if cols <= 1 {
                        range_get(ret, i, 0)
                    } else {
                        let row_data = (0..cols).map(|c| range_get(ret, i, c)).collect();
                        FValue::Array(std::sync::Arc::new(super::value::Array::new(1, cols, row_data)))
                    }
                } else if rows <= 1 {
                    range_get(ret, 0, i)
                } else {
                    let col_data = (0..rows).map(|r| range_get(ret, r, i)).collect();
                    FValue::Array(std::sync::Arc::new(super::value::Array::new(rows, 1, col_data)))
                });
            }
            if args.len() > 3 {
                let fallback = arg(args, 3);
                if !matches!(fallback, FValue::Empty) {
                    return Ok(fallback);
                }
            }
            Err(ErrKind::Na)
        }),
        FuncId::Index => r(|| {
            let area = args.first().ok_or(ErrKind::Value)?;
            let row_raw = num(args, 1)?;
            if row_raw < 0.0 {
                return Err(ErrKind::Value);
            }
            let row = row_raw.trunc() as usize;
            let (rows, cols) = range_dims(area);
            let has_col = args.len() > 2;
            let col = if has_col {
                let col_raw = num(args, 2)?;
                if col_raw < 0.0 {
                    return Err(ErrKind::Value);
                }
                col_raw.trunc() as usize
            } else {
                0
            };
            if !has_col && rows == 1 && cols > 1 {
                if row == 0 {
                    let data = (0..cols).map(|c| range_get(area, 0, c)).collect();
                    return Ok(FValue::Array(std::sync::Arc::new(super::value::Array::new(1, cols, data))));
                }
                return Ok(range_get(area, 0, row - 1));
            }
            if row == 0 {
                let target_c = if has_col {
                    if col == 0 {
                        let data = area.slice().to_vec();
                        return Ok(FValue::Array(std::sync::Arc::new(super::value::Array::new(rows, cols, data))));
                    }
                    if col > cols {
                        return Err(ErrKind::Ref);
                    }
                    col - 1
                } else {
                    0
                };
                if rows <= 1 {
                    return Ok(range_get(area, 0, target_c));
                }
                let col_data = (0..rows).map(|r| range_get(area, r, target_c)).collect();
                return Ok(FValue::Array(std::sync::Arc::new(super::value::Array::new(rows, 1, col_data))));
            }
            if has_col && col == 0 {
                if row > rows {
                    return Err(ErrKind::Ref);
                }
                if cols <= 1 {
                    return Ok(range_get(area, row - 1, 0));
                }
                let row_data = (0..cols).map(|c| range_get(area, row - 1, c)).collect();
                return Ok(FValue::Array(std::sync::Arc::new(super::value::Array::new(1, cols, row_data))));
            }
            let target_c = if has_col { col - 1 } else { 0 };
            Ok(range_get(area, row - 1, target_c))
        }),
        FuncId::Lookup => r(|| {
            let needle = arg(args, 0);
            let lookup_vals = args.get(1).ok_or(ErrKind::Value)?.slice();
            let result_vals = args
                .get(2)
                .map(|a| a.slice())
                .unwrap_or(lookup_vals);
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
        FuncId::Today => FValue::number(ctx.now_serial().floor()),
        FuncId::Now => FValue::number(ctx.now_serial()),
        FuncId::Date => r(|| {
            let y_raw = int_arg(args, 0)?;
            let m = int_arg(args, 1)?;
            let d = int_arg(args, 2)?;
            if !(0..10_000).contains(&y_raw) {
                return Err(ErrKind::Num);
            }
            let y = if y_raw < 1900 { y_raw + 1900 } else { y_raw };
            // 允许月份、日期溢出（Excel 行为）
            let y2 = y.checked_add((m - 1).div_euclid(12)).ok_or(ErrKind::Num)?;
            let m2 = ((m - 1).rem_euclid(12) + 1) as u32;
            let y2_i32 = i32::try_from(y2).map_err(|_| ErrKind::Num)?;
            let base = date_to_serial_in(ctx.date_system, y2_i32, m2, 1).ok_or(ErrKind::Num)?;
            let serial = base + d as f64 - 1.0;
            let max_serial = max_date_serial(ctx.date_system) as f64;
            if !serial.is_finite() || serial < 0.0 || serial > max_serial {
                return Err(ErrKind::Num);
            }
            Ok(FValue::number(serial))
        }),
        FuncId::Time => r(|| {
            let h = num(args, 0)?;
            let m = num(args, 1)?;
            let s = num(args, 2)?;
            Ok(FValue::number(
                (h * 3600.0 + m * 60.0 + s) / 86_400.0,
            ))
        }),
        FuncId::Year => r(|| {
            let (y, _, _) = serial_to_date_in(ctx.date_system, num(args, 0)?).ok_or(ErrKind::Num)?;
            Ok(FValue::number(y as f64))
        }),
        FuncId::Month => r(|| {
            let (_, m, _) = serial_to_date_in(ctx.date_system, num(args, 0)?).ok_or(ErrKind::Num)?;
            Ok(FValue::number(m as f64))
        }),
        FuncId::Day => r(|| {
            let (_, _, d) = serial_to_date_in(ctx.date_system, num(args, 0)?).ok_or(ErrKind::Num)?;
            Ok(FValue::number(d as f64))
        }),
        FuncId::Hour => r(|| {
            let s = num(args, 0)?.rem_euclid(1.0);
            Ok(FValue::number((s * 24.0).floor()))
        }),
        FuncId::Minute => r(|| {
            let s = num(args, 0)?.rem_euclid(1.0);
            Ok(FValue::number(((s * 24.0 * 60.0) % 60.0).floor()))
        }),
        FuncId::Second => r(|| {
            let s = num(args, 0)?.rem_euclid(1.0);
            Ok(FValue::number(((s * 86_400.0) % 60.0).floor()))
        }),
        FuncId::Weekday => r(|| {
            let serial = checked_date_serial(ctx.date_system, num(args, 0)?)?;
            let t = opt_num(args, 1, 1.0)?.trunc() as i64;
            let adj = serial + if ctx.date_system.is_1904() { 1462 } else { 0 };
            let sun0 = (adj + 6).rem_euclid(7); // 0=Sunday .. 6=Saturday
            let v = match t {
                1 | 17 => sun0 + 1,
                2 | 11 => (sun0 + 6) % 7 + 1,
                3 => (sun0 + 6) % 7,
                12 => (sun0 + 5) % 7 + 1,
                13 => (sun0 + 4) % 7 + 1,
                14 => (sun0 + 3) % 7 + 1,
                15 => (sun0 + 2) % 7 + 1,
                16 => (sun0 + 1) % 7 + 1,
                _ => return Err(ErrKind::Num),
            };
            Ok(FValue::number(v as f64))
        }),
        FuncId::EDate => r(|| {
            let serial = num(args, 0)?;
            let months = int_arg(args, 1)?;
            let (y, m, d) = serial_to_date_in(ctx.date_system, serial).ok_or(ErrKind::Num)?;
            let total = y as i64 * 12 + (m as i64 - 1) + months;
            let (y2, m2) = (total.div_euclid(12) as i32, (total.rem_euclid(12) + 1) as u32);
            let d2 = d.min(days_in_month(y2, m2));
            Ok(FValue::number(date_to_serial_in(ctx.date_system, y2, m2, d2).ok_or(ErrKind::Num)?))
        }),
        FuncId::EOMonth => r(|| {
            let serial = num(args, 0)?;
            let months = int_arg(args, 1)?;
            let (y, m, _) = serial_to_date_in(ctx.date_system, serial).ok_or(ErrKind::Num)?;
            let total = y as i64 * 12 + (m as i64 - 1) + months;
            let (y2, m2) = (total.div_euclid(12) as i32, (total.rem_euclid(12) + 1) as u32);
            Ok(FValue::number(
                date_to_serial_in(ctx.date_system, y2, m2, days_in_month(y2, m2)).ok_or(ErrKind::Num)?,
            ))
        }),
        FuncId::Days => r(|| Ok(FValue::number(num(args, 0)?.floor() - num(args, 1)?.floor()))),
        FuncId::DateDif => r(|| {
            let a = num(args, 0)?.floor();
            let b = num(args, 1)?.floor();
            let unit = text(args, 2)?.to_ascii_uppercase();
            let (y1, m1, d1) = serial_to_date_in(ctx.date_system, a).ok_or(ErrKind::Num)?;
            let (y2, m2, d2) = serial_to_date_in(ctx.date_system, b).ok_or(ErrKind::Num)?;
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
        FuncId::WeekNum => r(|| {
            let serial = num(args, 0)?.floor();
            let rt = opt_num(args, 1, 1.0)?.trunc() as i64;
            if rt == 21 {
                // ISO 8601 周数（周一为周首，含当年首个周四的周为第 1 周）
                let adj = serial as i64 + if ctx.date_system.is_1904() { 1462 } else { 0 };
                let mon0 = (adj + 5).rem_euclid(7); // 0=Mon..6=Sun
                let thu_serial = serial + (3 - mon0) as f64;
                let (iso_y, _, _) = serial_to_date_in(ctx.date_system, thu_serial).ok_or(ErrKind::Num)?;
                let jan4 = date_to_serial_in(ctx.date_system, iso_y, 1, 4).ok_or(ErrKind::Num)?;
                let jan4_adj = jan4 as i64 + if ctx.date_system.is_1904() { 1462 } else { 0 };
                let jan4_mon0 = (jan4_adj + 5).rem_euclid(7);
                let week1_mon = jan4 - jan4_mon0 as f64;
                let wn = ((serial - week1_mon) / 7.0).floor() + 1.0;
                return Ok(FValue::number(wn));
            }
            let start_dow: i64 = match rt {
                1 | 17 => 0, // Sun
                2 | 11 => 1, // Mon
                12 => 2,     // Tue
                13 => 3,     // Wed
                14 => 4,     // Thu
                15 => 5,     // Fri
                16 => 6,     // Sat
                _ => return Err(ErrKind::Num),
            };
            let (y, _, _) = serial_to_date_in(ctx.date_system, serial).ok_or(ErrKind::Num)?;
            let jan1 = date_to_serial_in(ctx.date_system, y, 1, 1).ok_or(ErrKind::Num)?;
            let jan1_adj = jan1 as i64 + if ctx.date_system.is_1904() { 1462 } else { 0 };
            let jan1_sun0 = (jan1_adj + 6).rem_euclid(7);
            let offset = (jan1_sun0 - start_dow).rem_euclid(7) as f64;
            Ok(FValue::number(((serial - jan1 + offset) / 7.0).floor() + 1.0))
        }),
        FuncId::NetworkDays => r(|| {
            let a = num(args, 0)?;
            let b = num(args, 1)?;
            let mut holidays = Vec::new();
            if let Some(h_arg) = args.get(2) {
                for v in h_arg.slice() {
                    match v {
                        FValue::Empty => {}
                        FValue::Error(e) => return Err(*e),
                        _ => {
                            let hf = v.to_number()?;
                            if !hf.is_finite() {
                                return Err(ErrKind::Num);
                            }
                            holidays.push(hf.floor() as i64);
                        }
                    }
                }
            }
            let mask = [false, false, false, false, false, true, true];
            networkdays_core(ctx.date_system, a, b, &mask, &holidays).map(FValue::number)
        }),
        FuncId::Workday => r(|| {
            let start = num(args, 0)?;
            let days = num(args, 1)?;
            let mut holidays = Vec::new();
            if let Some(h_arg) = args.get(2) {
                for v in h_arg.slice() {
                    match v {
                        FValue::Empty => {}
                        FValue::Error(e) => return Err(*e),
                        _ => {
                            let hf = v.to_number()?;
                            if !hf.is_finite() {
                                return Err(ErrKind::Num);
                            }
                            holidays.push(hf.floor() as i64);
                        }
                    }
                }
            }
            let mask = [false, false, false, false, false, true, true];
            workday_core(ctx.date_system, start, days, &mask, &holidays).map(FValue::number)
        }),
        FuncId::YearFrac => r(|| {
            let start = num(args, 0)?.floor();
            let end = num(args, 1)?.floor();
            let basis = opt_num(args, 2, 0.0)?.trunc() as i32;
            year_fraction_in(ctx.date_system, start.min(end), start.max(end), basis).map(FValue::number)
        }),

        // ---------------- 财务
        FuncId::Pmt => r(|| {
            let rate = num(args, 0)?;
            let nper = num(args, 1)?;
            let pv = num(args, 2)?;
            let fv = opt_num(args, 3, 0.0)?;
            let t = opt_num(args, 4, 0.0)?;
            Ok(FValue::number(pmt(rate, nper, pv, fv, t)))
        }),
        FuncId::Fv => r(|| {
            let rate = num(args, 0)?;
            let nper = num(args, 1)?;
            let pmt_v = num(args, 2)?;
            let pv = opt_num(args, 3, 0.0)?;
            let t = opt_num(args, 4, 0.0)?;
            Ok(FValue::number(fv(rate, nper, pmt_v, pv, t)))
        }),
        FuncId::Pv => r(|| {
            let rate = num(args, 0)?;
            let nper = num(args, 1)?;
            let pmt_v = num(args, 2)?;
            let fv_ = opt_num(args, 3, 0.0)?;
            let t = opt_num(args, 4, 0.0)?;
            Ok(FValue::number(pv(rate, nper, pmt_v, fv_, t)))
        }),
        FuncId::Nper => r(|| {
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
        FuncId::Ipmt => r(|| {
            let rate = num(args, 0)?;
            let per = num(args, 1)?;
            let nper = num(args, 2)?;
            let pv_ = num(args, 3)?;
            let fv_ = opt_num(args, 4, 0.0)?;
            let t = if opt_num(args, 5, 0.0)? != 0.0 { 1.0 } else { 0.0 };
            if per < 1.0 || per > nper {
                return Err(ErrKind::Num);
            }
            let p = pmt(rate, nper, pv_, fv_, t);
            let interest = ipmt_calc(rate, per, p, pv_, t);
            Ok(FValue::number(interest))
        }),
        FuncId::Ppmt => r(|| {
            let rate = num(args, 0)?;
            let per = num(args, 1)?;
            let nper = num(args, 2)?;
            let pv_ = num(args, 3)?;
            let fv_ = opt_num(args, 4, 0.0)?;
            let t = if opt_num(args, 5, 0.0)? != 0.0 { 1.0 } else { 0.0 };
            if per < 1.0 || per > nper {
                return Err(ErrKind::Num);
            }
            let p = pmt(rate, nper, pv_, fv_, t);
            let interest = ipmt_calc(rate, per, p, pv_, t);
            Ok(FValue::number(p - interest))
        }),
        FuncId::Npv => r(|| {
            let rate = num(args, 0)?;
            let flows = numbers(args, 1)?;
            let mut v = 0.0;
            for (i, f) in flows.iter().enumerate() {
                v += f / (1.0 + rate).powi(i as i32 + 1);
            }
            Ok(FValue::number(v))
        }),
        FuncId::Sln => r(|| {
            let cost = num(args, 0)?;
            let salvage = num(args, 1)?;
            let life = num(args, 2)?;
            if life == 0.0 {
                return Err(ErrKind::Div0);
            }
            Ok(FValue::number((cost - salvage) / life))
        }),
        FuncId::Syd => r(|| {
            let cost = num(args, 0)?;
            let salvage = num(args, 1)?;
            let life = num(args, 2)?;
            let per = num(args, 3)?;
            Ok(FValue::number((cost - salvage) * (life - per + 1.0) * 2.0 / (life * (life + 1.0))))
        }),

        // ---------------- 信息
        FuncId::IsBlank => FValue::Bool(matches!(arg(args, 0), FValue::Empty)),
        FuncId::IsNumber => FValue::Bool(matches!(arg(args, 0), FValue::Number(_))),
        FuncId::IsText => FValue::Bool(matches!(arg(args, 0), FValue::Text(_))),
        FuncId::IsNonText => FValue::Bool(!matches!(arg(args, 0), FValue::Text(_))),
        FuncId::IsLogical => FValue::Bool(matches!(arg(args, 0), FValue::Bool(_))),
        FuncId::IsEven => FValue::Bool(matches!(arg(args, 0).to_number(), Ok(n) if (n.trunc() as i64) % 2 == 0)),
        FuncId::IsOdd => FValue::Bool(matches!(arg(args, 0).to_number(), Ok(n) if (n.trunc() as i64) % 2 != 0)),
        FuncId::IsError => FValue::Bool(arg(args, 0).is_error()),
        FuncId::IsErr => FValue::Bool(matches!(arg(args, 0), FValue::Error(e) if e != ErrKind::Na)),
        FuncId::IsNa => FValue::Bool(matches!(arg(args, 0), FValue::Error(ErrKind::Na))),
        FuncId::IsRef => FValue::Bool(false),
        FuncId::Na => FValue::Error(ErrKind::Na),
        FuncId::N => r(|| match arg(args, 0) {
            FValue::Number(n) => Ok(FValue::number(n)),
            FValue::Bool(b) => Ok(FValue::number(if b { 1.0 } else { 0.0 })),
            FValue::Error(e) => Err(e),
            _ => Ok(FValue::number(0.0)),
        }),
        FuncId::Type => FValue::number(match arg(args, 0) {
            FValue::Array(_) => 64.0,
            FValue::Number(_) => 1.0,
            FValue::Text(_) => 2.0,
            FValue::Bool(_) => 4.0,
            FValue::Lambda(_) => 128.0,
            FValue::Error(_) => 16.0,
            FValue::Empty => 1.0,
        }),
        FuncId::ErrorType => r(|| match arg(args, 0) {
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
                ErrKind::Field => 11.0,
                ErrKind::Calc => 14.0,
            })),
            _ => Err(ErrKind::Na),
        }),

        _ => crate::formula::array::call_array(name, args)
            .or_else(|| crate::formula::extra::call_extra_in(ctx.date_system, name, args))
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

pub(super) fn year_fraction_in(sys: DateSystem, start: f64, end: f64, basis: i32) -> Result<f64, ErrKind> {
    if start < 0.0 || end > date_to_serial_in(sys, 9999, 12, 31).ok_or(ErrKind::Num)? {
        return Err(ErrKind::Num);
    }
    let calendar = |serial| {
        if serial == 0.0 && !sys.is_1904() {
            Ok((1900, 1, 0))
        } else {
            serial_to_date_in(sys, serial).ok_or(ErrKind::Num)
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

fn ipmt_calc(rate: f64, per: f64, p: f64, pv_: f64, t: f64) -> f64 {
    if rate == 0.0 {
        return 0.0;
    }
    if t != 0.0 {
        if per <= 1.0 {
            0.0
        } else {
            fv(rate, per - 1.0, p, pv_, 1.0) * rate / (1.0 + rate)
        }
    } else {
        fv(rate, per - 1.0, p, pv_, 0.0) * rate
    }
}

fn search_wildcard_char_pos(hay: &str, pat: &str) -> Option<usize> {
    if pat.is_empty() {
        return Some(0);
    }
    #[derive(Clone, Copy)]
    enum Tok {
        Star,
        Any,
        Lit(char),
    }
    let mut toks: Vec<Tok> = Vec::new();
    let mut chars = pat.chars();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if !matches!(toks.last(), Some(Tok::Star)) {
                    toks.push(Tok::Star);
                }
            }
            '?' => toks.push(Tok::Any),
            '~' => {
                let lit = chars.next().unwrap_or('~');
                for lc in lit.to_lowercase() {
                    toks.push(Tok::Lit(lc));
                }
            }
            other => {
                for lc in other.to_lowercase() {
                    toks.push(Tok::Lit(lc));
                }
            }
        }
    }
    if toks.is_empty() || matches!(toks.as_slice(), [Tok::Star]) {
        return Some(0);
    }
    // Map each lowercased char in `hay` back to its 0-based original char index in `hay`.
    let mut hay_lc: Vec<char> = Vec::with_capacity(hay.len());
    let mut orig_start: Vec<usize> = Vec::with_capacity(hay.len());
    for (ci, ch) in hay.chars().enumerate() {
        for lc in ch.to_lowercase() {
            hay_lc.push(lc);
            orig_start.push(ci);
        }
    }
    let match_prefix = |slice: &[char]| -> bool {
        let (mut ti, mut pi) = (0usize, 0usize);
        let mut star_pi: Option<usize> = None;
        let mut star_ti: usize = 0;
        loop {
            if pi == toks.len() {
                return true;
            }
            match toks[pi] {
                Tok::Star => {
                    pi += 1;
                    if pi == toks.len() {
                        return true;
                    }
                    star_pi = Some(pi);
                    star_ti = ti;
                    continue;
                }
                Tok::Any if ti < slice.len() => {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                Tok::Lit(pc) if ti < slice.len() && slice[ti] == pc => {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                _ => {}
            }
            if let Some(sp) = star_pi
                && star_ti < slice.len()
            {
                star_ti += 1;
                ti = star_ti;
                pi = sp;
            } else {
                return false;
            }
        }
    };
    let mut ci_idx = 0usize;
    while ci_idx <= hay_lc.len() {
        // Only start matching at original character boundaries
        let at_boundary = ci_idx == 0
            || ci_idx == hay_lc.len()
            || orig_start[ci_idx] != orig_start[ci_idx - 1];
        if at_boundary && match_prefix(&hay_lc[ci_idx..]) {
            return Some(if ci_idx < orig_start.len() {
                orig_start[ci_idx]
            } else {
                hay.chars().count()
            });
        }
        ci_idx += 1;
    }
    None
}

fn format_value_in(sys: DateSystem, v: &FValue, fmt: &str) -> String {
    let mv = match v {
        FValue::Number(n) => crate::model::Value::Number(*n),
        FValue::Empty => crate::model::Value::Number(0.0),
        FValue::Bool(b) => crate::model::Value::Bool(*b),
        FValue::Error(e) => crate::model::Value::Error(*e),
        FValue::Text(t) => {
            if !fmt.contains('@')
                && let Ok(n) = t.trim().parse::<f64>()
                && n.is_finite()
            {
                crate::model::Value::Number(n)
            } else {
                crate::model::Value::Text(t.clone())
            }
        }
        other => crate::model::Value::Text(other.display().into()),
    };
    crate::style::format_value_literal_in(sys, &mv, fmt)
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
    "ACOTH","GAMMA","GAUSS","PHI","PERMUTATIONA","BINOM.DIST.RANGE","COVAR","PERCENTOF","PERCENTRANK.EXC","LOGEST","MDETERM","MINVERSE","CLEAN","ASC","DOLLAR","FIXED","NUMBERVALUE","DAYS360","DATEVALUE","TIMEVALUE","ISOWEEKNUM","CELL","AREAS","INFO","COUPPCD","COUPNCD","COUPNUM","COUPDAYS","COUPDAYBS","COUPDAYSNC","PRICE","YIELD","DISC","INTRATE","RECEIVED","PRICEDISC","YIELDDISC","PRICEMAT","YIELDMAT","REGEXTEST","REGEXEXTRACT","REGEXREPLACE","NETWORKDAYS.INTL","WORKDAY.INTL","ODDFPRICE","ODDFYIELD","ODDLPRICE","ODDLYIELD","BESSELI","BESSELJ","BESSELK","BESSELY","FORECAST.ETS","FORECAST.ETS.CONFINT","FORECAST.ETS.SEASONALITY","FORECAST.ETS.STAT","LENB","LEFTB","RIGHTB","MIDB","FINDB","SEARCHB","NUMBERSTRING","ACCRINT","ACCRINTM",
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
            crate::formula::FValue::Text(s) => s.to_string(),
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

    #[test]
    fn text_criteria_on_numbers_short_circuits_accurately() {
        use super::{Crit, FValue};
        let n = FValue::Number(42.0);
        for (crit, want) in [
            ("apple", false),
            ("=apple", false),
            ("<>apple", true),
            (">apple", false),
            ("<apple", false),
            (">=apple", false),
            ("<=apple", false),
            ("", false),
            ("<>", true),
            (">", false),
            ("4*", false),
            ("<>4*", true),
            ("*", false),
            ("?", false),
            ("42", true),
            ("=42", true),
            ("<>42", false),
            (">41", true),
            ("<=42", true),
        ] {
            let c = Crit::parse(crit);
            assert_eq!(c.matches(&n), want, "Crit::matches({crit:?}) on 42.0");
        }
    }
}
