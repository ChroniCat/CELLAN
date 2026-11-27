//! 行列尺寸与隐藏区间索引：
//! - `HiddenIndices` 采用有序合并闭区间 `Vec<(u32, u32)>`（连续隐藏 95 万行仅占 1 条 8 字节区间），
//!   支持 `O(log K)` 包含/区间相交查询、`O(K)` 区间对称差，并保持 `.iter().copied()` 兼容。
//! - `Axis` 采用段合并 `Span` 表（相邻同尺寸/隐藏项合并为单段 `Span`），支持单次二分 `index_at`、
//!   `next_visible` / `prev_visible`、`zero_size_revision()` 独立版本号与 `O(spans)` 快速缩放。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

fn layout_revision() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

const STATIC_INDEX_CAP: usize = 1_048_577;

/// Lazily initialized read-only index table so legacy `.iter().copied()` / `.entries()`
/// callers can receive `&'a u32` without storing per-row nodes in `HiddenIndices` or `Axis`.
#[inline]
fn static_index_ref(i: u32) -> Option<&'static u32> {
    if (i as usize) >= STATIC_INDEX_CAP {
        return None;
    }
    static TABLE: OnceLock<Box<[u32]>> = OnceLock::new();
    let slice = TABLE.get_or_init(|| (0..STATIC_INDEX_CAP as u32).collect::<Vec<u32>>().into_boxed_slice());
    Some(&slice[i as usize])
}

#[inline]
fn resolve_index_ref(i: u32, overflow: &[u32]) -> &u32 {
    if let Some(r) = static_index_ref(i) {
        r
    } else {
        let pos = overflow.binary_search(&i).expect("overflow index registered");
        &overflow[pos]
    }
}

fn build_overflow_from_intervals(intervals: &[(u32, u32)]) -> Vec<u32> {
    let mut out = Vec::new();
    for &(lo, hi) in intervals {
        if (hi as usize) >= STATIC_INDEX_CAP {
            let start = lo.max(STATIC_INDEX_CAP as u32);
            out.extend(start..=hi);
        }
    }
    out
}

fn coalesce_sorted_intervals(mut raw: Vec<(u32, u32)>) -> (Vec<(u32, u32)>, usize) {
    raw.retain(|&(lo, hi)| lo <= hi);
    if raw.is_empty() {
        return (Vec::new(), 0);
    }
    raw.sort_unstable_by_key(|&(lo, _)| lo);
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(raw.len());
    for (lo, hi) in raw {
        if let Some(last) = merged.last_mut()
            && lo <= last.1.saturating_add(1)
        {
            last.1 = last.1.max(hi);
        } else {
            merged.push((lo, hi));
        }
    }
    let len = merged
        .iter()
        .map(|&(lo, hi)| (hi - lo) as usize + 1)
        .sum();
    (merged, len)
}

fn indices_to_intervals(mut vals: Vec<u32>) -> (Vec<(u32, u32)>, usize) {
    if vals.is_empty() {
        return (Vec::new(), 0);
    }
    vals.sort_unstable();
    vals.dedup();
    let len = vals.len();
    let mut intervals: Vec<(u32, u32)> = Vec::new();
    for v in vals {
        if let Some(last) = intervals.last_mut()
            && v == last.1 + 1
        {
            last.1 = v;
        } else {
            intervals.push((v, v));
        }
    }
    (intervals, len)
}

/// A snapshot shares sorted closed intervals `[start, end]` of hidden row/column indices.
#[derive(Clone, Debug)]
pub struct HiddenIndices {
    intervals: Arc<Vec<(u32, u32)>>,
    overflow: Arc<Vec<u32>>,
    /// Lazily materialized only if legacy code explicitly dereferences `*hidden_rows` as `HashSet<u32>`.
    /// All inherent interval methods leave this uninitialized (`0` bytes).
    lazy_set: Arc<OnceLock<HashSet<u32>>>,
    len: usize,
    revision: u64,
}

impl Default for HiddenIndices {
    fn default() -> Self {
        static EMPTY_INTERVALS: OnceLock<Arc<Vec<(u32, u32)>>> = OnceLock::new();
        static EMPTY_OVERFLOW: OnceLock<Arc<Vec<u32>>> = OnceLock::new();
        static EMPTY_SET: OnceLock<Arc<OnceLock<HashSet<u32>>>> = OnceLock::new();
        Self {
            intervals: EMPTY_INTERVALS.get_or_init(|| Arc::new(Vec::new())).clone(),
            overflow: EMPTY_OVERFLOW.get_or_init(|| Arc::new(Vec::new())).clone(),
            lazy_set: EMPTY_SET
                .get_or_init(|| Arc::new(OnceLock::from(HashSet::new())))
                .clone(),
            len: 0,
            revision: layout_revision(),
        }
    }
}

impl From<HashSet<u32>> for HiddenIndices {
    fn from(indices: HashSet<u32>) -> Self {
        if indices.is_empty() {
            return Self::default();
        }
        let (intervals, len) = indices_to_intervals(indices.into_iter().collect());
        let overflow = build_overflow_from_intervals(&intervals);
        Self {
            intervals: Arc::new(intervals),
            overflow: Arc::new(overflow),
            lazy_set: Arc::new(OnceLock::new()),
            len,
            revision: layout_revision(),
        }
    }
}

impl From<Vec<(u32, u32)>> for HiddenIndices {
    fn from(raw: Vec<(u32, u32)>) -> Self {
        Self::from_intervals(raw)
    }
}

impl FromIterator<u32> for HiddenIndices {
    fn from_iter<T: IntoIterator<Item = u32>>(iter: T) -> Self {
        let vals: Vec<u32> = iter.into_iter().collect();
        if vals.is_empty() {
            return Self::default();
        }
        let (intervals, len) = indices_to_intervals(vals);
        let overflow = build_overflow_from_intervals(&intervals);
        Self {
            intervals: Arc::new(intervals),
            overflow: Arc::new(overflow),
            lazy_set: Arc::new(OnceLock::new()),
            len,
            revision: layout_revision(),
        }
    }
}

impl std::ops::Deref for HiddenIndices {
    type Target = HashSet<u32>;
    fn deref(&self) -> &Self::Target {
        self.lazy_set.get_or_init(|| {
            let mut set = HashSet::with_capacity(self.len);
            for &(lo, hi) in self.intervals.iter() {
                set.extend(lo..=hi);
            }
            set
        })
    }
}

impl PartialEq for HiddenIndices {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
            || (self.len == other.len && self.intervals == other.intervals)
    }
}

impl Eq for HiddenIndices {}

impl PartialEq<HashSet<u32>> for HiddenIndices {
    fn eq(&self, other: &HashSet<u32>) -> bool {
        self.len == other.len() && self.intervals.iter().all(|&(lo, hi)| (lo..=hi).all(|v| other.contains(&v)))
    }
}

impl HiddenIndices {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_intervals(raw: impl IntoIterator<Item = (u32, u32)>) -> Self {
        let (intervals, len) = coalesce_sorted_intervals(raw.into_iter().collect());
        if intervals.is_empty() {
            return Self::default();
        }
        let overflow = build_overflow_from_intervals(&intervals);
        Self {
            intervals: Arc::new(intervals),
            overflow: Arc::new(overflow),
            lazy_set: Arc::new(OnceLock::new()),
            len,
            revision: layout_revision(),
        }
    }

    #[inline]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn intervals(&self) -> &[(u32, u32)] {
        &self.intervals
    }

    #[inline]
    pub fn contains(&self, value: &u32) -> bool {
        let pos = self.intervals.partition_point(|&(lo, _)| lo <= *value);
        pos > 0 && *value <= self.intervals[pos - 1].1
    }

    /// O(log intervals) range overlap test for `[start, end]`.
    #[inline]
    pub fn intersects_range(&self, start: u32, end: u32) -> bool {
        if start > end || self.intervals.is_empty() {
            return false;
        }
        let pos = self.intervals.partition_point(|&(lo, _)| lo <= end);
        pos > 0 && self.intervals[pos - 1].1 >= start
    }

    pub fn clear(&mut self) {
        if self.len == 0 {
            return;
        }
        *self = Self::default();
    }

    fn refresh_meta(&mut self) {
        self.len = self
            .intervals
            .iter()
            .map(|&(lo, hi)| (hi - lo) as usize + 1)
            .sum();
        let has_overflow = self.intervals.last().is_some_and(|&(_, hi)| (hi as usize) >= STATIC_INDEX_CAP);
        if has_overflow {
            self.overflow = Arc::new(build_overflow_from_intervals(&self.intervals));
        } else if !self.overflow.is_empty() {
            self.overflow = Arc::new(Vec::new());
        }
        if self.lazy_set.get().is_some() {
            self.lazy_set = Arc::new(OnceLock::new());
        }
        self.revision = layout_revision();
    }

    pub fn insert(&mut self, value: u32) -> bool {
        if self.contains(&value) {
            return false;
        }
        self.insert_range(value, value);
        true
    }

    pub fn insert_range(&mut self, start: u32, end: u32) {
        if start > end {
            return;
        }
        let pos = self.intervals.partition_point(|&(lo, _)| lo <= end);
        if pos > 0 {
            let (lo, hi) = self.intervals[pos - 1];
            if lo <= start && end <= hi {
                return;
            }
        }
        let mut raw = Vec::with_capacity(self.intervals.len() + 1);
        raw.extend_from_slice(&self.intervals);
        raw.push((start, end));
        let (merged, _) = coalesce_sorted_intervals(raw);
        self.intervals = Arc::new(merged);
        self.refresh_meta();
    }

    pub fn remove(&mut self, value: &u32) -> bool {
        if !self.contains(value) {
            return false;
        }
        self.remove_range(*value, *value);
        true
    }

    pub fn remove_range(&mut self, start: u32, end: u32) {
        if start > end || !self.intersects_range(start, end) {
            return;
        }
        let mut next = Vec::with_capacity(self.intervals.len() + 1);
        for &(lo, hi) in self.intervals.iter() {
            if hi < start || lo > end {
                next.push((lo, hi));
            } else {
                if lo < start {
                    next.push((lo, start - 1));
                }
                if hi > end {
                    next.push((end + 1, hi));
                }
            }
        }
        self.intervals = Arc::new(next);
        self.refresh_meta();
    }

    /// Return sorted disjoint closed intervals in `[start, end]` that are NOT in `self` (`O(intervals)`).
    pub fn complement_in(&self, start: u32, end: u32) -> Vec<(u32, u32)> {
        if start > end {
            return Vec::new();
        }
        if self.intervals.is_empty() {
            return vec![(start, end)];
        }
        let mut out = Vec::new();
        let mut cur = start;
        let idx = self.intervals.partition_point(|&(_, hi)| hi < start);
        for &(lo, hi) in &self.intervals[idx..] {
            if lo > end {
                break;
            }
            if cur < lo {
                out.push((cur, lo - 1));
            }
            let Some(next) = hi.checked_add(1) else {
                return out;
            };
            cur = cur.max(next);
            if cur > end {
                return out;
            }
        }
        if cur <= end {
            out.push((cur, end));
        }
        out
    }

    /// Delete all indices inside sorted disjoint closed `del_intervals` and shift surviving
    /// intervals down in a single `O(self.intervals + del_intervals)` two-pointer pass.
    pub fn shift_delete_intervals(&mut self, del_intervals: &[(u32, u32)]) -> bool {
        if del_intervals.is_empty() || self.intervals.is_empty() {
            return false;
        }
        let first_del = del_intervals[0].0;
        if self.intervals.last().is_some_and(|&(_, hi)| hi < first_del) {
            return false;
        }
        let mut raw: Vec<(u32, u32)> = Vec::with_capacity(self.intervals.len());
        let mut di = 0usize;
        let mut deleted_before = 0u32;

        for &(mut lo, hi) in self.intervals.iter() {
            while di < del_intervals.len() && del_intervals[di].1 < lo {
                let (d_lo, d_hi) = del_intervals[di];
                deleted_before = deleted_before.saturating_add(d_hi - d_lo + 1);
                di += 1;
            }
            let mut cur_del_idx = di;
            let mut cur_shift = deleted_before;
            while cur_del_idx < del_intervals.len() {
                let (d_lo, d_hi) = del_intervals[cur_del_idx];
                if d_lo > hi {
                    break;
                }
                if lo < d_lo {
                    raw.push((lo - cur_shift, (d_lo - 1) - cur_shift));
                }
                cur_shift = cur_shift.saturating_add(d_hi - d_lo + 1);
                cur_del_idx += 1;
                let Some(next_lo) = d_hi.checked_add(1) else {
                    lo = u32::MAX;
                    break;
                };
                lo = next_lo;
                if lo > hi {
                    break;
                }
            }
            if lo <= hi {
                raw.push((lo - cur_shift, hi - cur_shift));
            }
            while di < del_intervals.len() && del_intervals[di].1 <= hi {
                let (d_lo, d_hi) = del_intervals[di];
                deleted_before = deleted_before.saturating_add(d_hi - d_lo + 1);
                di += 1;
            }
        }

        let (merged, _) = coalesce_sorted_intervals(raw);
        if merged == *self.intervals {
            return false;
        }
        self.intervals = Arc::new(merged);
        self.refresh_meta();
        true
    }

    /// Shift all hidden intervals in `O(intervals)` time for a structural insert or delete.
    pub fn shift_axis(&mut self, at: u32, n: u32, delete: bool) -> bool {
        self.shift_axis_bounded(at, n, delete, u32::MAX)
    }

    /// Shift all hidden intervals in `O(intervals)` time, dropping any shifted index `>= limit`.
    pub fn shift_axis_bounded(&mut self, at: u32, n: u32, delete: bool, limit: u32) -> bool {
        if n == 0 || self.intervals.is_empty() {
            return false;
        }
        if self.intervals.last().is_some_and(|&(_, hi)| hi < at) {
            return false;
        }
        let mut raw = Vec::with_capacity(self.intervals.len() + 1);
        if delete {
            let end_del = at.saturating_add(n);
            for &(lo, hi) in self.intervals.iter() {
                if hi < at {
                    if lo < limit {
                        raw.push((lo, hi.min(limit.saturating_sub(1))));
                    }
                } else if lo >= end_del {
                    let nlo = lo - n;
                    let nhi = hi - n;
                    if nlo < limit {
                        raw.push((nlo, nhi.min(limit.saturating_sub(1))));
                    }
                } else {
                    if lo < at && lo < limit {
                        raw.push((lo, (at - 1).min(limit.saturating_sub(1))));
                    }
                    if hi >= end_del && at < limit {
                        raw.push((at, (hi - n).min(limit.saturating_sub(1))));
                    }
                }
            }
        } else {
            for &(lo, hi) in self.intervals.iter() {
                if hi < at {
                    if lo < limit {
                        raw.push((lo, hi.min(limit.saturating_sub(1))));
                    }
                } else if lo >= at {
                    if let Some(nlo) = lo.checked_add(n)
                        && nlo < limit
                    {
                        let nhi = hi.saturating_add(n).min(limit.saturating_sub(1));
                        raw.push((nlo, nhi));
                    }
                } else {
                    if lo < limit {
                        raw.push((lo, (at - 1).min(limit.saturating_sub(1))));
                    }
                    if let Some(nlo) = at.checked_add(n)
                        && nlo < limit
                    {
                        let nhi = hi.saturating_add(n).min(limit.saturating_sub(1));
                        raw.push((nlo, nhi));
                    }
                }
            }
        }
        let (merged, _) = coalesce_sorted_intervals(raw);
        if merged == *self.intervals {
            return false;
        }
        self.intervals = Arc::new(merged);
        self.refresh_meta();
        true
    }

    /// Remap hidden intervals for multi-point non-deleting insertions (e.g. `MappedShift` subtotals)
    /// in `O(intervals + insertions * log(span))` without expanding Million-row intervals.
    pub fn remap_insertions(&mut self, map: impl Fn(u32) -> Option<u32>) -> bool {
        if self.intervals.is_empty() {
            return false;
        }
        fn split_interval(lo: u32, hi: u32, map: &impl Fn(u32) -> Option<u32>, out: &mut Vec<(u32, u32)>) {
            match (map(lo), map(hi)) {
                (Some(a), Some(b)) if b >= a && b - a == hi - lo => out.push((a, b)),
                _ if lo == hi => {
                    if let Some(a) = map(lo) {
                        out.push((a, a));
                    }
                }
                _ => {
                    let mid = lo + (hi - lo) / 2;
                    split_interval(lo, mid, map, out);
                    split_interval(mid + 1, hi, map, out);
                }
            }
        }
        let mut raw = Vec::with_capacity(self.intervals.len());
        for &(lo, hi) in self.intervals.iter() {
            split_interval(lo, hi, &map, &mut raw);
        }
        let (merged, _) = coalesce_sorted_intervals(raw);
        if merged == *self.intervals {
            return false;
        }
        self.intervals = Arc::new(merged);
        self.refresh_meta();
        true
    }

    pub fn retain(&mut self, mut f: impl FnMut(&u32) -> bool) {
        if self.is_empty() {
            return;
        }
        let mut changed = false;
        let mut next: Vec<(u32, u32)> = Vec::new();
        for &(lo, hi) in self.intervals.iter() {
            for v in lo..=hi {
                if f(&v) {
                    if let Some(last) = next.last_mut()
                        && v == last.1 + 1
                    {
                        last.1 = v;
                    } else {
                        next.push((v, v));
                    }
                } else {
                    changed = true;
                }
            }
        }
        if changed {
            self.intervals = Arc::new(next);
            self.refresh_meta();
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &u32> + '_ {
        let overflow = self.overflow.as_slice();
        self.intervals
            .iter()
            .flat_map(|&(lo, hi)| lo..=hi)
            .map(move |v| resolve_index_ref(v, overflow))
    }

    /// Compute the symmetric difference as merged intervals in `O(intervals_a + intervals_b)` time!
    pub fn symmetric_difference_intervals(&self, other: &Self) -> Vec<(u32, u32)> {
        if self.revision == other.revision {
            return Vec::new();
        }
        let a = self.intervals.as_slice();
        let b = other.intervals.as_slice();
        let mut i = 0usize;
        let mut j = 0usize;
        let mut cur_a: Option<(u32, u32)> = a.first().copied();
        let mut cur_b: Option<(u32, u32)> = b.first().copied();
        let mut out: Vec<(u32, u32)> = Vec::new();
        let mut push_interval = |lo: u32, hi: u32| {
            if lo > hi {
                return;
            }
            if let Some(last) = out.last_mut()
                && lo == last.1.saturating_add(1)
            {
                last.1 = hi;
            } else {
                out.push((lo, hi));
            }
        };

        while cur_a.is_some() || cur_b.is_some() {
            match (cur_a, cur_b) {
                (Some((a1, a2)), None) => {
                    push_interval(a1, a2);
                    i += 1;
                    cur_a = a.get(i).copied();
                }
                (None, Some((b1, b2))) => {
                    push_interval(b1, b2);
                    j += 1;
                    cur_b = b.get(j).copied();
                }
                (Some((a1, a2)), Some((b1, b2))) => {
                    if a2 < b1 {
                        push_interval(a1, a2);
                        i += 1;
                        cur_a = a.get(i).copied();
                    } else if b2 < a1 {
                        push_interval(b1, b2);
                        j += 1;
                        cur_b = b.get(j).copied();
                    } else {
                        if a1 < b1 {
                            push_interval(a1, b1 - 1);
                        } else if b1 < a1 {
                            push_interval(b1, a1 - 1);
                        }
                        let overlap_end = a2.min(b2);
                        if a2 > overlap_end {
                            cur_a = Some((overlap_end + 1, a2));
                        } else {
                            i += 1;
                            cur_a = a.get(i).copied();
                        }
                        if b2 > overlap_end {
                            cur_b = Some((overlap_end + 1, b2));
                        } else {
                            j += 1;
                            cur_b = b.get(j).copied();
                        }
                    }
                }
                (None, None) => break,
            }
        }
        out
    }

    /// Two-pointer interval symmetric difference yielding `&'a u32` (skips identical 1M-row intervals in O(1)!).
    pub fn symmetric_difference<'a>(&'a self, other: &'a Self) -> impl Iterator<Item = &'a u32> + 'a {
        let diff = self.symmetric_difference_intervals(other);
        diff.into_iter().flat_map(|(lo, hi)| lo..=hi).map(move |v| {
            if let Some(r) = static_index_ref(v) {
                r
            } else if let Ok(pos) = self.overflow.binary_search(&v) {
                &self.overflow[pos]
            } else {
                let pos = other.overflow.binary_search(&v).expect("overflow in symmetric_difference");
                &other.overflow[pos]
            }
        })
    }
}

impl Extend<u32> for HiddenIndices {
    fn extend<T: IntoIterator<Item = u32>>(&mut self, iter: T) {
        let vals: Vec<u32> = iter.into_iter().collect();
        if vals.is_empty() {
            return;
        }
        let (mut new_intervals, _) = indices_to_intervals(vals);
        if self.intervals.is_empty() {
            let overflow = build_overflow_from_intervals(&new_intervals);
            let len = new_intervals.iter().map(|&(lo, hi)| (hi - lo) as usize + 1).sum();
            self.intervals = Arc::new(new_intervals);
            self.overflow = Arc::new(overflow);
            if self.lazy_set.get().is_some() {
                self.lazy_set = Arc::new(OnceLock::new());
            }
            self.len = len;
            self.revision = layout_revision();
            return;
        }
        new_intervals.extend_from_slice(&self.intervals);
        let (merged, _) = coalesce_sorted_intervals(new_intervals);
        if merged.as_slice() != self.intervals.as_slice() {
            self.intervals = Arc::new(merged);
            self.refresh_meta();
        }
    }
}

impl<'a> Extend<&'a u32> for HiddenIndices {
    fn extend<T: IntoIterator<Item = &'a u32>>(&mut self, iter: T) {
        self.extend(iter.into_iter().copied());
    }
}

impl IntoIterator for HiddenIndices {
    type Item = u32;
    type IntoIter = Box<dyn Iterator<Item = u32>>;
    fn into_iter(self) -> Self::IntoIter {
        let intervals = Arc::unwrap_or_clone(self.intervals);
        Box::new(intervals.into_iter().flat_map(|(lo, hi)| lo..=hi))
    }
}

impl<'a> IntoIterator for &'a HiddenIndices {
    type Item = &'a u32;
    type IntoIter = Box<dyn Iterator<Item = &'a u32> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

/// Excel 96 DPI 的默认列宽（54 pt = 72 px）
pub const DEFAULT_COL_W: f32 = 72.0;
/// Excel 96 DPI 的默认行高（14.25 pt = 19 px）
pub const DEFAULT_ROW_H: f32 = 18.0;

/// Run-length merged custom axis interval `[start, end]` sharing an identical `size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub size: f32,
    pub start_px: f64,
    pub end_px: f64,
}

#[derive(Clone, Debug)]
pub struct Axis {
    revision: u64,
    zero_size_revision: u64,
    default: f32,
    spans: Arc<Vec<Span>>,
    overflow_ids: Arc<Vec<u32>>,
    custom_len: usize,
}

impl PartialEq for Axis {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
            || (self.default == other.default
                && self.custom_len == other.custom_len
                && self.spans.len() == other.spans.len()
                && self
                    .spans
                    .iter()
                    .zip(other.spans.iter())
                    .all(|(a, b)| a.start == b.start && a.end == b.end && a.size == b.size))
    }
}

impl Default for Axis {
    fn default() -> Self {
        Self::new(DEFAULT_COL_W)
    }
}

impl Axis {
    pub fn new(default: f32) -> Self {
        let rev = layout_revision();
        Self {
            revision: rev,
            zero_size_revision: rev,
            default,
            spans: Default::default(),
            overflow_ids: Default::default(),
            custom_len: 0,
        }
    }

    pub(crate) fn from_default_and_spans(default: f32, mut raw: Vec<(u32, u32, f32)>) -> Self {
        let mut ax = Self::new(default);
        raw.retain(|&(lo, hi, sz)| lo <= hi && sz.is_finite() && (sz - default).abs() >= 0.01);
        if !raw.is_empty() {
            raw.sort_unstable_by_key(|&(lo, _, _)| lo);
            let prev_def_zero = ax.default <= 0.0;
            ax.install_raw_spans(raw, prev_def_zero);
        }
        ax
    }

    #[inline]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Revision tracking ONLY hidden/zero-size visibility changes (unaffected by dragging a non-zero row height!).
    #[inline]
    pub fn zero_size_revision(&self) -> u64 {
        self.zero_size_revision
    }

    #[inline]
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    fn zero_intervals(spans: &[Span]) -> impl Iterator<Item = (u32, u32)> + '_ {
        spans
            .iter()
            .filter(|s| s.size <= 0.0)
            .map(|s| (s.start, s.end))
    }

    /// Recompute prefix `start_px` / `end_px`, coalesce adjacent identical-size spans,
    /// and update `revision` / `zero_size_revision`.
    fn install_raw_spans(&mut self, raw: Vec<(u32, u32, f32)>, prev_default_zero: bool) {
        let mut merged: Vec<Span> = Vec::with_capacity(raw.len());
        let mut custom_len = 0usize;
        let mut next_idx = 0u64;
        let mut cur_px = 0.0f64;

        for (lo, hi, size) in raw {
            if lo > hi {
                continue;
            }
            if let Some(last) = merged.last_mut()
                && lo == last.end.saturating_add(1)
                && last.size.to_bits() == size.to_bits()
            {
                let added = (hi - lo) as usize + 1;
                custom_len += added;
                last.end = hi;
                last.end_px += added as f64 * f64::from(size);
                cur_px = last.end_px;
                next_idx = u64::from(hi) + 1;
            } else {
                let gap = u64::from(lo) - next_idx;
                let start_px = cur_px + gap as f64 * f64::from(self.default);
                let count = (hi - lo) as usize + 1;
                custom_len += count;
                let end_px = start_px + count as f64 * f64::from(size);
                merged.push(Span {
                    start: lo,
                    end: hi,
                    size,
                    start_px,
                    end_px,
                });
                cur_px = end_px;
                next_idx = u64::from(hi) + 1;
            }
        }

        let zero_changed = (self.default <= 0.0) != prev_default_zero
            || !Self::zero_intervals(&self.spans).eq(Self::zero_intervals(&merged));

        let has_overflow = merged.last().is_some_and(|s| (s.end as usize) >= STATIC_INDEX_CAP);
        if has_overflow {
            let mut ov = Vec::new();
            for s in &merged {
                if (s.end as usize) >= STATIC_INDEX_CAP {
                    ov.extend(s.start.max(STATIC_INDEX_CAP as u32)..=s.end);
                }
            }
            self.overflow_ids = Arc::new(ov);
        } else if !self.overflow_ids.is_empty() {
            self.overflow_ids = Default::default();
        }

        let rev = layout_revision();
        self.revision = rev;
        if zero_changed {
            self.zero_size_revision = rev;
        }
        self.custom_len = custom_len;
        self.spans = Arc::new(merged);
    }

    pub fn set(&mut self, i: u32, size: f32) {
        self.set_many([(i, size)]);
    }

    /// Apply absolute sizes in input order, then rebuild the merged span index once.
    pub fn set_many(&mut self, sizes: impl IntoIterator<Item = (u32, f32)>) {
        // Coalesce input edits keeping the last write per index, then merge with existing spans!
        let mut edits: Vec<(u32, usize, f32)> = sizes
            .into_iter()
            .enumerate()
            .map(|(seq, (i, sz))| (i, seq, sz))
            .collect();
        if edits.is_empty() {
            return;
        }
        edits.sort_unstable_by_key(|&(i, seq, _)| (i, seq));
        // Deduplicate keeping the LAST write for each index `i` (preserving input-order semantics!).
        let mut deduped: Vec<(u32, f32)> = Vec::with_capacity(edits.len());
        for (i, _, sz) in edits {
            if let Some(last) = deduped.last_mut()
                && last.0 == i
            {
                last.1 = sz;
            } else {
                deduped.push((i, sz));
            }
        }
        // Check if any edit actually changes the effective stored state.
        let any_change = deduped.iter().any(|&(i, sz)| {
            let is_def = (sz - self.default).abs() < 0.01;
            let cur = self.custom_size_at(i);
            if is_def {
                cur.is_some()
            } else {
                cur != Some(sz)
            }
        });
        if !any_change {
            return;
        }

        // Group `deduped` into contiguous runs `(start, end, Option<f32>)` where `None` means reset to default.
        let mut patch_runs: Vec<(u32, u32, Option<f32>)> = Vec::new();
        for (i, sz) in deduped {
            let val = ((sz - self.default).abs() >= 0.01).then_some(sz);
            if let Some(last) = patch_runs.last_mut()
                && i == last.1 + 1
                && match (last.2, val) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a.to_bits() == b.to_bits(),
                    _ => false,
                }
            {
                last.1 = i;
            } else {
                patch_runs.push((i, i, val));
            }
        }
        self.apply_patch_runs(&patch_runs);
    }

    fn apply_patch_runs(&mut self, patches: &[(u32, u32, Option<f32>)]) {
        if patches.is_empty() {
            return;
        }
        let old = self.spans.as_slice();
        let mut raw: Vec<(u32, u32, f32)> = Vec::with_capacity(old.len() + patches.len() * 2);
        let mut si = 0usize;
        let mut cur_span: Option<(u32, u32, f32)> = old.first().map(|s| (s.start, s.end, s.size));

        for &(p_lo, p_hi, p_val) in patches {
            while let Some((s_lo, s_hi, s_sz)) = cur_span {
                if s_hi < p_lo {
                    raw.push((s_lo, s_hi, s_sz));
                    si += 1;
                    cur_span = old.get(si).map(|s| (s.start, s.end, s.size));
                } else {
                    if s_lo < p_lo {
                        raw.push((s_lo, p_lo - 1, s_sz));
                    }
                    if s_hi <= p_hi {
                        si += 1;
                        cur_span = old.get(si).map(|s| (s.start, s.end, s.size));
                    } else {
                        cur_span = Some((p_hi + 1, s_hi, s_sz));
                        break;
                    }
                }
            }
            if let Some(sz) = p_val {
                raw.push((p_lo, p_hi, sz));
            }
        }
        while let Some((s_lo, s_hi, s_sz)) = cur_span {
            raw.push((s_lo, s_hi, s_sz));
            si += 1;
            cur_span = old.get(si).map(|s| (s.start, s.end, s.size));
        }

        let prev_def_zero = self.default <= 0.0;
        self.install_raw_spans(raw, prev_def_zero);
    }

    /// 批量将闭区间列表 `[start, end]` 尺寸设为 0（百万隐藏行仅需 O(spans + intervals) 区间归并！）
    pub fn hide_ranges(&mut self, intervals: &[(u32, u32)]) {
        if intervals.is_empty() {
            return;
        }
        let (merged, _) = coalesce_sorted_intervals(intervals.to_vec());
        if merged.is_empty() {
            return;
        }
        let already_hidden = merged.iter().all(|&(lo, hi)| {
            let k = self.spans.partition_point(|s| s.start <= lo);
            k > 0 && hi <= self.spans[k - 1].end && self.spans[k - 1].size == 0.0
        });
        if already_hidden {
            return;
        }
        let is_def_zero = (0.0 - self.default).abs() < 0.01;
        let patches: Vec<(u32, u32, Option<f32>)> = merged
            .into_iter()
            .map(|(lo, hi)| (lo, hi, (!is_def_zero).then_some(0.0)))
            .collect();
        self.apply_patch_runs(&patches);
    }

    pub fn hide_hidden(&mut self, hidden: &HiddenIndices) {
        self.hide_ranges(hidden.intervals());
    }

    /// 批量将指定行/列尺寸设为 0（自动合并连续行号为闭区间段）
    pub fn hide_many(&mut self, indices: impl IntoIterator<Item = u32>) {
        let mut iter = indices.into_iter();
        let Some(first) = iter.next() else { return };
        let mut runs: Vec<(u32, u32)> = vec![(first, first)];
        for idx in iter {
            let last = runs.last_mut().unwrap();
            if idx == last.1 + 1 {
                last.1 = idx;
            } else if idx >= last.0 && idx <= last.1 {
                // duplicate in current run
            } else {
                runs.push((idx, idx));
            }
        }
        self.hide_ranges(&runs);
    }

    /// Shift custom spans in `O(spans)` time for a structural insert or delete,
    /// without expanding million-row hidden/uniform spans into per-row loops.
    pub fn shift_axis(&mut self, at: u32, n: u32, delete: bool) -> bool {
        self.shift_axis_bounded(at, n, delete, u32::MAX)
    }

    /// Shift custom spans in `O(spans)` time, dropping any shifted index `>= limit`.
    pub fn shift_axis_bounded(&mut self, at: u32, n: u32, delete: bool, limit: u32) -> bool {
        if n == 0 || self.spans.is_empty() {
            return false;
        }
        if self.spans.last().is_some_and(|s| s.end < at) {
            return false;
        }
        let mut raw: Vec<(u32, u32, f32)> = Vec::with_capacity(self.spans.len() + 1);
        if delete {
            let end_del = at.saturating_add(n);
            for s in self.spans.iter() {
                if s.end < at {
                    if s.start < limit {
                        raw.push((s.start, s.end.min(limit.saturating_sub(1)), s.size));
                    }
                } else if s.start >= end_del {
                    let nlo = s.start - n;
                    let nhi = s.end - n;
                    if nlo < limit {
                        raw.push((nlo, nhi.min(limit.saturating_sub(1)), s.size));
                    }
                } else {
                    if s.start < at && s.start < limit {
                        raw.push((s.start, (at - 1).min(limit.saturating_sub(1)), s.size));
                    }
                    if s.end >= end_del && at < limit {
                        raw.push((at, (s.end - n).min(limit.saturating_sub(1)), s.size));
                    }
                }
            }
        } else {
            for s in self.spans.iter() {
                if s.end < at {
                    if s.start < limit {
                        raw.push((s.start, s.end.min(limit.saturating_sub(1)), s.size));
                    }
                } else if s.start >= at {
                    if let Some(nlo) = s.start.checked_add(n)
                        && nlo < limit
                    {
                        let nhi = s.end.saturating_add(n).min(limit.saturating_sub(1));
                        raw.push((nlo, nhi, s.size));
                    }
                } else {
                    if s.start < limit {
                        raw.push((s.start, (at - 1).min(limit.saturating_sub(1)), s.size));
                    }
                    if let Some(nlo) = at.checked_add(n)
                        && nlo < limit
                    {
                        let nhi = s.end.saturating_add(n).min(limit.saturating_sub(1));
                        raw.push((nlo, nhi, s.size));
                    }
                }
            }
        }
        let prev_def_zero = self.default <= 0.0;
        self.install_raw_spans(raw, prev_def_zero);
        true
    }

    /// Delete sorted disjoint closed intervals from custom spans in a single `O(spans + intervals)` pass.
    pub fn shift_delete_intervals(&mut self, del_intervals: &[(u32, u32)]) -> bool {
        if del_intervals.is_empty() || self.spans.is_empty() {
            return false;
        }
        let first_del = del_intervals[0].0;
        if self.spans.last().is_some_and(|s| s.end < first_del) {
            return false;
        }
        let mut raw: Vec<(u32, u32, f32)> = Vec::with_capacity(self.spans.len());
        let mut di = 0usize;
        let mut deleted_before = 0u32;

        for s in self.spans.iter() {
            let mut lo = s.start;
            let hi = s.end;
            while di < del_intervals.len() && del_intervals[di].1 < lo {
                let (d_lo, d_hi) = del_intervals[di];
                deleted_before = deleted_before.saturating_add(d_hi - d_lo + 1);
                di += 1;
            }
            let mut cur_di = di;
            let mut cur_shift = deleted_before;
            while cur_di < del_intervals.len() {
                let (d_lo, d_hi) = del_intervals[cur_di];
                if d_lo > hi {
                    break;
                }
                if lo < d_lo {
                    raw.push((lo - cur_shift, (d_lo - 1) - cur_shift, s.size));
                }
                cur_shift = cur_shift.saturating_add(d_hi - d_lo + 1);
                cur_di += 1;
                let Some(next_lo) = d_hi.checked_add(1) else {
                    lo = u32::MAX;
                    break;
                };
                lo = next_lo;
                if lo > hi {
                    break;
                }
            }
            if lo <= hi {
                raw.push((lo - cur_shift, hi - cur_shift, s.size));
            }
            while di < del_intervals.len() && del_intervals[di].1 <= hi {
                let (d_lo, d_hi) = del_intervals[di];
                deleted_before = deleted_before.saturating_add(d_hi - d_lo + 1);
                di += 1;
            }
        }

        let prev_def_zero = self.default <= 0.0;
        self.install_raw_spans(raw, prev_def_zero);
        true
    }

    pub(crate) fn remap_indices(&mut self, map: impl Fn(u32) -> Option<u32>) -> bool {
        if self.spans.is_empty() {
            return false;
        }
        let any_changed = self.spans.iter().any(|s| {
            map(s.start) != Some(s.start)
                || map(s.end) != Some(s.end)
                || map(s.end) != map(s.start).map(|lo| lo + (s.end - s.start))
        });
        if !any_changed {
            return false;
        }
        fn split_span(
            lo: u32,
            hi: u32,
            size: f32,
            map: &impl Fn(u32) -> Option<u32>,
            out: &mut Vec<(u32, u32, f32)>,
        ) {
            match (map(lo), map(hi)) {
                (Some(a), Some(b)) if b >= a && b - a == hi - lo => out.push((a, b, size)),
                _ if lo == hi => {
                    if let Some(a) = map(lo) {
                        out.push((a, a, size));
                    }
                }
                _ => {
                    let mid = lo + (hi - lo) / 2;
                    split_span(lo, mid, size, map, out);
                    split_span(mid + 1, hi, size, map, out);
                }
            }
        }
        let mut raw: Vec<(u32, u32, f32)> = Vec::with_capacity(self.spans.len());
        for s in self.spans.iter() {
            split_span(s.start, s.end, s.size, &map, &mut raw);
        }
        raw.sort_unstable_by_key(|&(lo, _, _)| lo);
        let prev_def_zero = self.default <= 0.0;
        self.install_raw_spans(raw, prev_def_zero);
        true
    }

    #[inline]
    fn custom_size_at(&self, i: u32) -> Option<f32> {
        let k = self.spans.partition_point(|s| s.start <= i);
        (k > 0 && i <= self.spans[k - 1].end).then(|| self.spans[k - 1].size)
    }

    /// 第 i 项的起点（相对内容原点）
    #[inline]
    pub fn pos(&self, i: u32) -> f32 {
        self.start_position(i) as f32
    }

    /// 按缩放因子直接计算起点，零克隆 `Axis`！
    #[inline]
    pub fn pos_zoomed(&self, i: u32, z: f32) -> f32 {
        (self.start_position(i) * f64::from(z)) as f32
    }

    #[inline]
    pub fn start_position(&self, i: u32) -> f64 {
        let k = self.spans.partition_point(|s| s.start <= i);
        if k == 0 {
            return f64::from(i) * f64::from(self.default);
        }
        let s = &self.spans[k - 1];
        if i <= s.end {
            s.start_px + f64::from(i - s.start) * f64::from(s.size)
        } else {
            s.end_px + f64::from(i - s.end - 1) * f64::from(self.default)
        }
    }

    /// 从 i（含）直达第一个有尺寸的行/列。跨越百万隐藏行只需 O(log spans) 二分。
    pub fn next_visible(&self, i: u32) -> Option<u32> {
        if self.default > 0.0 {
            let mut k = self.spans.partition_point(|s| s.start <= i);
            if k == 0 || i > self.spans[k - 1].end {
                return Some(i);
            }
            if self.spans[k - 1].size > 0.0 {
                return Some(i);
            }
            let mut cand = self.spans[k - 1].end.checked_add(1)?;
            while let Some(s) = self.spans.get(k) {
                if s.start == cand && s.size <= 0.0 {
                    cand = s.end.checked_add(1)?;
                    k += 1;
                } else {
                    break;
                }
            }
            Some(cand)
        } else {
            let k = self.spans.partition_point(|s| s.end < i);
            for s in &self.spans[k..] {
                if s.size > 0.0 {
                    return Some(i.max(s.start));
                }
            }
            None
        }
    }

    /// 从 i（含）向前直达最近一个有尺寸的行/列。跨越百万隐藏行只需 O(log spans) 二分。
    pub fn prev_visible(&self, i: u32) -> Option<u32> {
        if self.default > 0.0 {
            let mut k = self.spans.partition_point(|s| s.start <= i);
            if k == 0 || i > self.spans[k - 1].end {
                return Some(i);
            }
            if self.spans[k - 1].size > 0.0 {
                return Some(i);
            }
            let mut cand = self.spans[k - 1].start.checked_sub(1)?;
            k -= 1;
            while k > 0 {
                let s = &self.spans[k - 1];
                if s.end == cand && s.size <= 0.0 {
                    cand = s.start.checked_sub(1)?;
                    k -= 1;
                } else {
                    break;
                }
            }
            Some(cand)
        } else {
            let k = self.spans.partition_point(|s| s.start <= i);
            for s in self.spans[..k].iter().rev() {
                if s.size > 0.0 {
                    return Some(i.min(s.end));
                }
            }
            None
        }
    }

    pub fn visible_indices(&self, range: std::ops::Range<u32>) -> impl Iterator<Item = u32> + '_ {
        let mut next = range.start;
        std::iter::from_fn(move || {
            if next >= range.end {
                return None;
            }
            let i = self.next_visible(next)?;
            if i >= range.end {
                return None;
            }
            next = i + 1;
            Some(i)
        })
    }

    #[inline]
    pub fn size(&self, i: u32) -> f32 {
        self.custom_size_at(i).unwrap_or(self.default)
    }

    /// 像素位置落在哪一项（单次 `O(log spans)` 二分 + 区间内算术除法，消除双重嵌套二分！）
    pub fn index_at(&self, px: f32) -> u32 {
        if px <= 0.0 {
            return self.next_visible(0).unwrap_or(0);
        }
        let target = f64::from(px);
        // Find the first span whose `end_px as f32 > px`. All earlier spans (including
        // contiguous hidden 0.0 spans sitting right at `px`) satisfy `pos(end + 1) <= px`.
        let k = self.spans.partition_point(|s| (s.end_px as f32) <= px);
        if let Some(s) = self.spans.get(k)
            && (s.start_px as f32) <= px
        {
            // Inside span `s` (`s.size > 0.0` is guaranteed because `end_px as f32 > px >= start_px as f32`).
            let max_off = s.end - s.start;
            let mut off = if s.size > 0.0 {
                ((target - s.start_px) / f64::from(s.size)).floor().max(0.0) as u32
            } else {
                max_off
            }
            .min(max_off);
            while off < max_off
                && ((s.start_px + f64::from(off + 1) * f64::from(s.size)) as f32) <= px
            {
                off += 1;
            }
            while off > 0 && ((s.start_px + f64::from(off) * f64::from(s.size)) as f32) > px {
                off -= 1;
            }
            return s.start + off;
        }

        // Inside the default-sized gap after `spans[k - 1]` (and before `spans[k]` if any).
        let (base_idx, base_px) = if k > 0 {
            let prev = &self.spans[k - 1];
            (prev.end.saturating_add(1), prev.end_px)
        } else {
            (0u32, 0.0f64)
        };
        if self.default <= 0.0 {
            return if let Some(s) = self.spans.get(k) {
                s.start.saturating_sub(1)
            } else {
                self.spans.last().map_or(0, |s| s.end)
            };
        }
        let max_off = self
            .spans
            .get(k)
            .map_or(u32::MAX - base_idx, |s| s.start.saturating_sub(base_idx + 1));
        let mut off = ((target - base_px) / f64::from(self.default))
            .floor()
            .max(0.0) as u32;
        off = off.min(max_off);
        while off < max_off
            && ((base_px + f64::from(off + 1) * f64::from(self.default)) as f32) <= px
        {
            off += 1;
        }
        while off > 0 && ((base_px + f64::from(off) * f64::from(self.default)) as f32) > px {
            off -= 1;
        }
        base_idx + off
    }

    #[inline]
    pub fn index_at_zoomed(&self, px: f32, z: f32) -> u32 {
        if z <= 0.0 {
            return 0;
        }
        self.index_at(px / z)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.custom_len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.custom_len == 0
    }

    /// 所有自定义尺寸（(索引, 大小)）
    pub fn custom_pairs(&self) -> Vec<(u32, f32)> {
        let mut out = Vec::with_capacity(self.custom_len);
        for s in self.spans.iter() {
            for i in s.start..=s.end {
                out.push((i, s.size));
            }
        }
        out
    }

    /// 清掉所有自定义尺寸
    pub fn clear_custom(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let had_zero = self.spans.iter().any(|s| s.size <= 0.0);
        let rev = layout_revision();
        self.revision = rev;
        if had_zero {
            self.zero_size_revision = rev;
        }
        self.spans = Default::default();
        self.overflow_ids = Default::default();
        self.custom_len = 0;
    }

    /// 按缩放倍数复制一份轴：`O(spans)` 直接缩放段表，百万隐藏行仅复制 1 条 `Span`！
    pub fn scaled(&self, z: f32) -> Self {
        let z64 = f64::from(z);
        let spans: Vec<Span> = self
            .spans
            .iter()
            .map(|s| Span {
                start: s.start,
                end: s.end,
                size: s.size * z,
                start_px: s.start_px * z64,
                end_px: s.end_px * z64,
            })
            .collect();
        let rev = layout_revision();
        Self {
            revision: rev,
            zero_size_revision: rev,
            default: self.default * z,
            spans: Arc::new(spans),
            overflow_ids: self.overflow_ids.clone(),
            custom_len: self.custom_len,
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = (&u32, &f32)> + '_ {
        let overflow = self.overflow_ids.as_slice();
        self.spans.iter().flat_map(move |s| {
            let sz_ref = &s.size;
            (s.start..=s.end).map(move |i| (resolve_index_ref(i, overflow), sz_ref))
        })
    }

    pub fn default_size(&self) -> f32 {
        self.default
    }

    /// 改默认尺寸（Excel 的 `<col min="1" max="16384">` 就是改整表默认列宽）。
    /// 已记下的绝对尺寸不受影响。
    pub fn set_default(&mut self, size: f32) {
        if (size - self.default).abs() > 0.01 {
            let prev_def_zero = self.default <= 0.0;
            self.default = size;
            let raw: Vec<(u32, u32, f32)> = self
                .spans
                .iter()
                .map(|s| (s.start, s.end, s.size))
                .collect();
            self.install_raw_spans(raw, prev_def_zero);
        }
    }
}

/// Excel 列宽单位（字符数）→ 像素。
pub fn excel_width_to_px(w: f32) -> f32 {
    (w * 8.0 + 5.0).round().max(8.0)
}

/// `<col width="21.25">` → 175px（21.25×8+5）。默认宽度 8.38 走同一公式。
pub fn col_width_to_px(w: f32) -> f32 {
    if w <= 0.0 {
        return 0.0;
    }
    (w * 8.0 + 5.0).round()
}

/// 行高单位是磅（pt）→ 像素（96 DPI）
pub fn pt_to_px(pt: f32) -> f32 {
    (pt * 96.0 / 72.0).round().max(4.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_size_edits_leave_shared_axis_snapshot_unchanged() {
        let mut axis = Axis::new(18.0);
        axis.set_many([(2, 24.0), (4, 0.0)]);
        let snapshot = axis.clone();
        axis.set_many([(2, 24.0), (3, 18.0)]);
        assert_eq!(snapshot.revision(), axis.revision());
        assert!(Arc::ptr_eq(&snapshot.spans, &axis.spans));
        let zrev_before = axis.zero_size_revision();
        axis.set_many([(2, 18.0), (3, 12.0), (3, 15.0)]);
        assert_eq!(
            axis.zero_size_revision(),
            zrev_before,
            "仅修改非零行高不应改变 zero_size_revision"
        );
        assert_eq!(axis.pos(5), 69.0);
        assert_eq!(snapshot.pos(5), 78.0);
        assert_eq!(snapshot.size(2), 24.0);
    }

    #[test]
    fn million_hidden_rows_collapse_to_single_span_and_prev_next_jump() {
        let mut hidden = HiddenIndices::default();
        hidden.insert_range(10, 950_009);
        assert_eq!(hidden.intervals(), &[(10, 950_009)]);
        assert_eq!(hidden.len(), 950_000);
        assert!(hidden.contains(&500_000));
        assert!(!hidden.contains(&9));
        assert!(hidden.intersects_range(950_009, 950_100));
        assert!(!hidden.intersects_range(0, 9));

        let mut other = hidden.clone();
        other.insert(950_010);
        assert_eq!(
            other.symmetric_difference(&hidden).copied().collect::<Vec<_>>(),
            vec![950_010]
        );

        let mut axis = Axis::new(18.0);
        axis.hide_hidden(&hidden);
        assert_eq!(axis.spans().len(), 1, "95 万连续隐藏行必须合并为 1 条 Span");
        assert_eq!(axis.next_visible(10), Some(950_010));
        assert_eq!(axis.prev_visible(950_009), Some(9));
        assert_eq!(axis.prev_visible(500_000), Some(9));
        let scaled = axis.scaled(1.25);
        assert_eq!(scaled.spans().len(), 1);
    }

    #[test]
    fn uniform_when_empty() {
        let a = Axis::new(100.0);
        assert_eq!(a.pos(0), 0.0);
        assert_eq!(a.pos(3), 300.0);
        assert_eq!(a.size(5), 100.0);
        assert_eq!(a.index_at(250.0), 2);
        assert_eq!(a.index_at(300.0), 3);
    }

    #[test]
    fn shifted_by_custom_width() {
        let mut a = Axis::new(100.0);
        a.set(0, 200.0);
        assert_eq!(a.pos(0), 0.0);
        assert_eq!(a.pos(1), 200.0);
        assert_eq!(a.pos(2), 300.0);
        assert_eq!(a.size(0), 200.0);
        assert_eq!(a.index_at(150.0), 0);
        assert_eq!(a.index_at(250.0), 1);
        assert_eq!(a.index_at(350.0), 2);
    }

    #[test]
    fn many_custom_widths() {
        let mut a = Axis::new(64.0);
        for i in 0..50 {
            a.set(i, 64.0 + (i as f32) * 3.0);
        }
        let expect: f32 = (0..50).map(|i| 64.0 + i as f32 * 3.0).sum();
        assert!((a.pos(50) - expect).abs() < 0.5, "{} vs {}", a.pos(50), expect);
        assert_eq!(a.index_at(expect + 5.0), 50);
        for i in 0..50 {
            assert_eq!(a.index_at(a.pos(i) + 1.0), i);
        }
    }

    #[test]
    fn reset_to_default_removes_entry() {
        let mut a = Axis::new(100.0);
        a.set(2, 300.0);
        assert_eq!(a.len(), 1);
        a.set(2, 100.0);
        assert_eq!(a.len(), 0);
        assert_eq!(a.pos(3), 300.0);
    }

    #[test]
    fn viewport_skips_twenty_thousand_hidden_rows_at_fractional_zoom() {
        let mut axis = Axis::new(18.0);
        axis.hide_many(2..20_002);
        axis.set(20_003, 30.0);
        let axis = axis.scaled(0.73);
        let edge = axis.pos(2);
        assert_eq!(axis.pos(20_002), edge);
        assert_eq!(axis.index_at(edge), 20_002);
        assert_eq!(axis.index_at(edge - 0.05), 1);
        assert_eq!(axis.next_visible(2), Some(20_002));
        assert_eq!(axis.next_visible(19_999), Some(20_002));
        assert_eq!(axis.prev_visible(19_999), Some(1));
        assert_eq!(
            axis.visible_indices(1..20_005).collect::<Vec<_>>(),
            [1, 20_002, 20_003, 20_004]
        );
        assert_eq!(axis.visible_indices(2..19_999).next(), None);
    }

    #[test]
    fn visible_indices_on_hidden_default_axis_find_only_sized_entries() {
        let mut axis = Axis::new(0.0);
        axis.set(20_000, 18.0);
        axis.set(20_004, 18.0);
        assert_eq!(axis.index_at(0.0), 20_000);
        assert_eq!(axis.prev_visible(25_000), Some(20_004));
        assert_eq!(axis.prev_visible(20_002), Some(20_000));
        assert_eq!(axis.prev_visible(19_999), None);
        assert_eq!(
            axis.visible_indices(0..30_000).collect::<Vec<_>>(),
            [20_000, 20_004]
        );
    }

    #[test]
    fn excel_units() {
        assert_eq!(excel_width_to_px(8.43), 72.0);
        assert_eq!(col_width_to_px(21.25), 175.0);
        assert_eq!(col_width_to_px(9.0), 77.0);
        assert_eq!(pt_to_px(15.0), 20.0);
        assert_eq!(pt_to_px(30.0), 40.0);
    }

    #[test]
    fn scaled_axis_scales_default_and_custom_but_not_hidden() {
        let mut a = Axis::new(100.0);
        a.set(2, 150.0);
        a.set(3, 0.0);
        let b = a.scaled(2.0);
        assert_eq!(b.size(0), 200.0);
        assert_eq!(b.size(2), 300.0);
        assert_eq!(b.size(3), 0.0, "隐藏行/列缩放后仍是 0");
        assert_eq!(b.pos(2), 400.0);
        assert_eq!(a.size(0), 100.0, "原轴不能被改");
    }
}
