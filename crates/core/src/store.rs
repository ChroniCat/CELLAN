//! 分页列式存储：把单元格按「页」切开，页内按列连续存放。
//!
//! 设计要点：
//! - 页高 `PAGE_ROWS = 256` 行；页目录对常规坐标（`< 8192` 页 = 209 万行）采用定长索引直达 `O(1)`，
//!   辅以 `page_bits: Vec<u64>` 页非空位图，使 `iter` / `iter_rect` 无需拷贝页号做全键排序。
//! - 页内列目录采用紧凑 `PageCols<T>`：常规列宽（`< 256` 列）走定长直达数组 + 内联 `[u64; 4]` 位图，
//!   一旦出现远端列（如 `XFD` 第 16,383 列）自动切到 64 列块位图 + `POPCNT` rank 紧凑索引，单页目录开销 `< 128 B`。
//! - 页内列块采用类型化 `ColSlab`：纯数值页列走 `Numbers(NumberSlab)`（`8 B/格` + 256 位存在位图），
//!   无公式值列走 `Values(Vec<Value>)`（`24 B/格`），含公式页列走 `Mixed(Vec<Cell>)`（`40 B/格`），支持原地自动升级。
//! - `CellStyles` 支持整页同样式 `StyleColumn::Uniform(u16)` 压缩，整列/大区域刷格式共享单个 `Arc`。

use crate::model::{Cell, CellJournal, CellRef, Value};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

pub const PAGE_ROWS: u32 = 256;
const DIRECT_PAGE_LIMIT: u32 = 8192;
const DENSE_COL_LIMIT: u32 = 256;

#[inline]
pub(crate) fn row_range_mask(lo: u32, hi: u32) -> [u64; 4] {
    let mut m = [0u64; 4];
    if lo > hi || hi >= PAGE_ROWS {
        return m;
    }
    let sw = (lo / 64) as usize;
    let ew = (hi / 64) as usize;
    for w in sw..=ew {
        let mut bits = u64::MAX;
        if w == sw {
            bits &= !0u64 << (lo % 64);
        }
        if w == ew {
            let h = hi % 64;
            if h < 63 {
                bits &= (1u64 << (h + 1)) - 1;
            }
        }
        m[w] = bits;
    }
    m
}

#[inline]
fn highest_bit_plus_one(mask: &[u64; 4]) -> usize {
    for w in (0..4).rev() {
        if mask[w] != 0 {
            return w * 64 + (64 - mask[w].leading_zeros() as usize);
        }
    }
    0
}

#[inline]
fn grow_slab_vec<T: Clone>(v: &mut Vec<T>, needed: usize, fill: T) {
    debug_assert!(needed <= PAGE_ROWS as usize);
    if v.len() < needed {
        let cap = v.capacity();
        if cap < needed {
            let target = if cap == 0 {
                needed.max(4).min(PAGE_ROWS as usize)
            } else {
                cap.saturating_mul(2).max(needed).min(PAGE_ROWS as usize)
            };
            v.reserve_exact(target - v.len());
        }
        v.resize(needed, fill);
    }
}

#[inline]
fn count_bits_below(rows: &[u64; 4], offset: u32) -> usize {
    if offset >= PAGE_ROWS {
        return rows.iter().map(|w| w.count_ones() as usize).sum();
    }
    let word = (offset / 64) as usize;
    let bit = 1u64 << (offset % 64);
    let mut sum = 0u32;
    for w in &rows[..word] {
        sum += w.count_ones();
    }
    sum += (rows[word] & (bit - 1)).count_ones();
    sum as usize
}

/// 64-column bitmap-ranked chunk used by `PageCols::Sparse`.
#[derive(Clone, Debug)]
pub(crate) struct ColChunk<T> {
    /// Column word index (`word = col / 64`, covering `[word * 64 .. word * 64 + 64)`).
    pub(crate) word: u16,
    /// Non-zero 64-bit occupancy bitmap within this 64-column chunk.
    pub(crate) mask: u64,
    /// Packed `Arc<T>` slabs in ascending column order (`slabs.len() == mask.count_ones()`).
    pub(crate) slabs: Vec<Arc<T>>,
}

/// Compact per-page column directory shared by `Page` and `StylePage`.
///
/// Canonical invariant:
/// - `Dense` whenever all occupied columns are `< DENSE_COL_LIMIT` (256), including empty.
/// - `Sparse` whenever at least one occupied column is `>= DENSE_COL_LIMIT` (e.g. `XFD` = 16,383).
#[derive(Clone, Debug)]
pub(crate) enum PageCols<T> {
    Dense {
        nonempty: [u64; 4],
        slots: Vec<Option<Arc<T>>>,
    },
    Sparse {
        chunks: Vec<ColChunk<T>>,
    },
}

impl<T> Default for PageCols<T> {
    #[inline]
    fn default() -> Self {
        Self::Dense {
            nonempty: [0; 4],
            slots: Vec::new(),
        }
    }
}

impl<T> PageCols<T> {
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::Dense { nonempty, .. } => *nonempty == [0; 4],
            Self::Sparse { chunks } => chunks.is_empty(),
        }
    }

    #[inline]
    pub(crate) fn col_count(&self) -> usize {
        match self {
            Self::Dense { nonempty, .. } => {
                nonempty.iter().map(|w| w.count_ones() as usize).sum()
            }
            Self::Sparse { chunks } => chunks.iter().map(|ch| ch.slabs.len()).sum(),
        }
    }

    #[inline]
    pub(crate) fn has_col(&self, col: u32) -> bool {
        match self {
            Self::Dense { nonempty, .. } => {
                col < DENSE_COL_LIMIT
                    && (nonempty[(col / 64) as usize] & (1u64 << (col % 64))) != 0
            }
            Self::Sparse { chunks } => {
                let word = (col / 64) as u16;
                chunks
                    .binary_search_by_key(&word, |ch| ch.word)
                    .ok()
                    .is_some_and(|i| (chunks[i].mask & (1u64 << (col % 64))) != 0)
            }
        }
    }

    #[inline]
    pub(crate) fn max_col(&self) -> Option<u32> {
        match self {
            Self::Dense { nonempty, .. } => {
                for w in (0..4).rev() {
                    if nonempty[w] != 0 {
                        let bit = 63 - nonempty[w].leading_zeros();
                        return Some(w as u32 * 64 + bit);
                    }
                }
                None
            }
            Self::Sparse { chunks } => chunks
                .last()
                .map(|ch| ch.word as u32 * 64 + (63 - ch.mask.leading_zeros())),
        }
    }

    #[cold]
    #[inline(never)]
    fn get_sparse(chunks: &[ColChunk<T>], col: u32) -> Option<&Arc<T>> {
        let word = (col / 64) as u16;
        let idx = chunks.binary_search_by_key(&word, |ch| ch.word).ok()?;
        let ch = &chunks[idx];
        let bit = 1u64 << (col % 64);
        if ch.mask & bit == 0 {
            return None;
        }
        let rank = (ch.mask & (bit - 1)).count_ones() as usize;
        Some(&ch.slabs[rank])
    }

    #[cold]
    #[inline(never)]
    fn get_mut_sparse(chunks: &mut [ColChunk<T>], col: u32) -> Option<&mut Arc<T>> {
        let word = (col / 64) as u16;
        let idx = chunks.binary_search_by_key(&word, |ch| ch.word).ok()?;
        let ch = &mut chunks[idx];
        let bit = 1u64 << (col % 64);
        if ch.mask & bit == 0 {
            return None;
        }
        let rank = (ch.mask & (bit - 1)).count_ones() as usize;
        Some(&mut ch.slabs[rank])
    }

    #[inline]
    pub(crate) fn get(&self, col: u32) -> Option<&Arc<T>> {
        match self {
            Self::Dense { slots, .. } => slots.get(col as usize)?.as_ref(),
            Self::Sparse { chunks } => Self::get_sparse(chunks, col),
        }
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, col: u32) -> Option<&mut Arc<T>> {
        match self {
            Self::Dense { slots, .. } => slots.get_mut(col as usize)?.as_mut(),
            Self::Sparse { chunks } => Self::get_mut_sparse(chunks, col),
        }
    }

    #[cold]
    fn upgrade_dense_to_sparse(&mut self) {
        let Self::Dense { nonempty, slots } = self else {
            return;
        };
        let mut chunks = Vec::new();
        let mut owned_slots = std::mem::take(slots);
        for (w, &bits_orig) in nonempty.iter().enumerate() {
            if bits_orig == 0 {
                continue;
            }
            let mut slabs = Vec::with_capacity(bits_orig.count_ones() as usize);
            let mut bits = bits_orig;
            while bits != 0 {
                let tz = bits.trailing_zeros();
                bits &= bits - 1;
                let c = w * 64 + tz as usize;
                if let Some(arc) = owned_slots.get_mut(c).and_then(Option::take) {
                    slabs.push(arc);
                }
            }
            if !slabs.is_empty() {
                chunks.push(ColChunk {
                    word: w as u16,
                    mask: bits_orig,
                    slabs,
                });
            }
        }
        *self = Self::Sparse { chunks };
    }

    fn maybe_downgrade_sparse_to_dense(&mut self) {
        let should_downgrade = match self {
            Self::Sparse { chunks } => chunks.last().is_none_or(|ch| ch.word < 4),
            Self::Dense { .. } => false,
        };
        if !should_downgrade {
            return;
        }
        let Self::Sparse { chunks } = std::mem::take(self) else {
            return;
        };
        if chunks.is_empty() {
            *self = Self::default();
            return;
        }
        let max_c = chunks
            .last()
            .map_or(0, |ch| ch.word as usize * 64 + (63 - ch.mask.leading_zeros()) as usize);
        let mut nonempty = [0u64; 4];
        let mut slots = vec![None; max_c + 1];
        for ch in chunks {
            let w = ch.word as usize;
            nonempty[w] = ch.mask;
            let mut bits = ch.mask;
            for arc in ch.slabs {
                let tz = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                slots[w * 64 + tz] = Some(arc);
            }
        }
        *self = Self::Dense { nonempty, slots };
    }

    #[inline]
    pub(crate) fn set(&mut self, col: u32, val: Arc<T>) -> Option<Arc<T>> {
        if col >= DENSE_COL_LIMIT && matches!(self, Self::Dense { .. }) {
            self.upgrade_dense_to_sparse();
        }
        match self {
            Self::Dense { nonempty, slots } => {
                let idx = col as usize;
                if slots.len() <= idx {
                    slots.resize(idx + 1, None);
                }
                nonempty[(col / 64) as usize] |= 1u64 << (col % 64);
                slots[idx].replace(val)
            }
            Self::Sparse { chunks } => {
                let word = (col / 64) as u16;
                let bit = 1u64 << (col % 64);
                match chunks.binary_search_by_key(&word, |ch| ch.word) {
                    Ok(i) => {
                        let ch = &mut chunks[i];
                        let rank = (ch.mask & (bit - 1)).count_ones() as usize;
                        if ch.mask & bit != 0 {
                            Some(std::mem::replace(&mut ch.slabs[rank], val))
                        } else {
                            ch.mask |= bit;
                            ch.slabs.insert(rank, val);
                            None
                        }
                    }
                    Err(i) => {
                        chunks.insert(
                            i,
                            ColChunk {
                                word,
                                mask: bit,
                                slabs: vec![val],
                            },
                        );
                        None
                    }
                }
            }
        }
    }

    #[inline]
    pub(crate) fn get_or_insert_with(
        &mut self,
        col: u32,
        f: impl FnOnce() -> Arc<T>,
    ) -> &mut Arc<T> {
        if col >= DENSE_COL_LIMIT && matches!(self, Self::Dense { .. }) {
            self.upgrade_dense_to_sparse();
        }
        match self {
            Self::Dense { nonempty, slots } => {
                let idx = col as usize;
                if slots.len() <= idx {
                    slots.resize(idx + 1, None);
                }
                nonempty[(col / 64) as usize] |= 1u64 << (col % 64);
                slots[idx].get_or_insert_with(f)
            }
            Self::Sparse { chunks } => {
                let word = (col / 64) as u16;
                let bit = 1u64 << (col % 64);
                match chunks.binary_search_by_key(&word, |ch| ch.word) {
                    Ok(i) => {
                        let ch = &mut chunks[i];
                        let rank = (ch.mask & (bit - 1)).count_ones() as usize;
                        if ch.mask & bit == 0 {
                            ch.mask |= bit;
                            ch.slabs.insert(rank, f());
                        }
                        &mut ch.slabs[rank]
                    }
                    Err(i) => {
                        chunks.insert(
                            i,
                            ColChunk {
                                word,
                                mask: bit,
                                slabs: vec![f()],
                            },
                        );
                        &mut chunks[i].slabs[0]
                    }
                }
            }
        }
    }

    #[inline]
    pub(crate) fn remove(&mut self, col: u32) -> Option<Arc<T>> {
        match self {
            Self::Dense { nonempty, slots } => {
                let old = slots.get_mut(col as usize)?.take()?;
                nonempty[(col / 64) as usize] &= !(1u64 << (col % 64));
                while slots.last().is_some_and(Option::is_none) {
                    slots.pop();
                }
                Some(old)
            }
            Self::Sparse { chunks } => {
                let word = (col / 64) as u16;
                let idx = chunks.binary_search_by_key(&word, |ch| ch.word).ok()?;
                let bit = 1u64 << (col % 64);
                let ch = &mut chunks[idx];
                if ch.mask & bit == 0 {
                    return None;
                }
                let rank = (ch.mask & (bit - 1)).count_ones() as usize;
                ch.mask &= !bit;
                let old = ch.slabs.remove(rank);
                if ch.mask == 0 {
                    chunks.remove(idx);
                    self.maybe_downgrade_sparse_to_dense();
                }
                Some(old)
            }
        }
    }

    pub(crate) fn retain_mut(&mut self, mut f: impl FnMut(u32, &mut Arc<T>) -> bool) {
        match self {
            Self::Dense { nonempty, slots } => {
                *nonempty = [0; 4];
                let mut last_some = 0usize;
                for (c, slot) in slots.iter_mut().enumerate() {
                    if let Some(arc) = slot {
                        if f(c as u32, arc) {
                            nonempty[c / 64] |= 1u64 << (c % 64);
                            last_some = c + 1;
                        } else {
                            *slot = None;
                        }
                    }
                }
                slots.truncate(last_some);
            }
            Self::Sparse { chunks } => {
                chunks.retain_mut(|ch| {
                    let base = ch.word as u32 * 64;
                    let mut bits = ch.mask;
                    let mut new_mask = 0u64;
                    let mut write = 0usize;
                    let mut read = 0usize;
                    while bits != 0 {
                        let tz = bits.trailing_zeros();
                        bits &= bits - 1;
                        let col = base + tz;
                        let keep = f(col, &mut ch.slabs[read]);
                        if keep {
                            new_mask |= 1u64 << tz;
                            if write != read {
                                ch.slabs.swap(write, read);
                            }
                            write += 1;
                        }
                        read += 1;
                    }
                    ch.slabs.truncate(write);
                    ch.mask = new_mask;
                    new_mask != 0
                });
                self.maybe_downgrade_sparse_to_dense();
            }
        }
    }

    pub(crate) fn iter_nonempty_cols(&self, c1: u32, c2: u32) -> PageColIdxIter<'_, T> {
        if c1 > c2 || self.is_empty() {
            return PageColIdxIter::Empty;
        }
        match self {
            Self::Dense { nonempty, .. } => {
                if c1 >= DENSE_COL_LIMIT {
                    return PageColIdxIter::Empty;
                }
                let hi = c2.min(DENSE_COL_LIMIT - 1);
                let start_w = (c1 / 64) as usize;
                let end_w = (hi / 64) as usize;
                let mut words = *nonempty;
                for w in 0..start_w {
                    words[w] = 0;
                }
                words[start_w] &= !0u64 << (c1 % 64);
                let hb = hi % 64;
                if hb < 63 {
                    words[end_w] &= (1u64 << (hb + 1)) - 1;
                }
                for w in (end_w + 1)..4 {
                    words[w] = 0;
                }
                PageColIdxIter::Dense {
                    words,
                    word: start_w,
                    end_w,
                }
            }
            Self::Sparse { chunks } => {
                let start_w = (c1 / 64).min(u16::MAX as u32) as u16;
                let end_w = (c2 / 64).min(u16::MAX as u32) as u16;
                let pos = chunks.partition_point(|ch| ch.word < start_w);
                PageColIdxIter::Sparse {
                    chunks,
                    idx: pos,
                    start_w,
                    end_w,
                    c1,
                    c2,
                    cur_word: 0,
                    cur_bits: 0,
                }
            }
        }
    }

    pub(crate) fn iter_nonempty_cols_rev(&self, max_c: u32) -> PageColIdxRevIter<'_, T> {
        if self.is_empty() {
            return PageColIdxRevIter::Empty;
        }
        match self {
            Self::Dense { nonempty, .. } => {
                let hi = max_c.min(DENSE_COL_LIMIT - 1);
                let end_w = (hi / 64) as usize;
                let mut words = *nonempty;
                let hb = hi % 64;
                if hb < 63 {
                    words[end_w] &= (1u64 << (hb + 1)) - 1;
                }
                PageColIdxRevIter::Dense {
                    words,
                    word: end_w as isize,
                }
            }
            Self::Sparse { chunks } => {
                let end_w = (max_c / 64).min(u16::MAX as u32) as u16;
                let end_idx = chunks.partition_point(|ch| ch.word <= end_w);
                if end_idx == 0 {
                    return PageColIdxRevIter::Empty;
                }
                PageColIdxRevIter::Sparse {
                    chunks,
                    idx: end_idx as isize - 1,
                    end_w,
                    max_c,
                    cur_word: 0,
                    cur_bits: 0,
                }
            }
        }
    }

    pub(crate) fn iter_entries_in(&self, c1: u32, c2: u32) -> PageColEntriesIter<'_, T> {
        if c1 > c2 || self.is_empty() {
            return PageColEntriesIter::Empty;
        }
        match self {
            Self::Dense { nonempty, slots } => {
                if c1 >= DENSE_COL_LIMIT {
                    return PageColEntriesIter::Empty;
                }
                let hi = c2.min(DENSE_COL_LIMIT - 1);
                let start_w = (c1 / 64) as usize;
                let end_w = (hi / 64) as usize;
                let mut words = *nonempty;
                for w in 0..start_w {
                    words[w] = 0;
                }
                words[start_w] &= !0u64 << (c1 % 64);
                let hb = hi % 64;
                if hb < 63 {
                    words[end_w] &= (1u64 << (hb + 1)) - 1;
                }
                for w in (end_w + 1)..4 {
                    words[w] = 0;
                }
                PageColEntriesIter::Dense {
                    slots,
                    words,
                    word: start_w,
                    end_w,
                }
            }
            Self::Sparse { chunks } => {
                let start_w = (c1 / 64).min(u16::MAX as u32) as u16;
                let end_w = (c2 / 64).min(u16::MAX as u32) as u16;
                let pos = chunks.partition_point(|ch| ch.word < start_w);
                PageColEntriesIter::Sparse {
                    chunks,
                    idx: pos,
                    start_w,
                    end_w,
                    c1,
                    c2,
                    cur_chunk: 0,
                    cur_bits: 0,
                    cur_rank: 0,
                }
            }
        }
    }

    #[inline]
    pub(crate) fn iter_entries(&self) -> PageColEntriesIter<'_, T> {
        self.iter_entries_in(0, u32::MAX)
    }

    pub(crate) fn into_entries(self) -> Vec<(u32, Arc<T>)> {
        match self {
            Self::Dense { nonempty, mut slots } => {
                let mut out = Vec::with_capacity(
                    nonempty.iter().map(|w| w.count_ones() as usize).sum(),
                );
                for (w, mut bits) in nonempty.into_iter().enumerate() {
                    while bits != 0 {
                        let tz = bits.trailing_zeros();
                        bits &= bits - 1;
                        let c = w * 64 + tz as usize;
                        if let Some(arc) = slots.get_mut(c).and_then(Option::take) {
                            out.push((c as u32, arc));
                        }
                    }
                }
                out
            }
            Self::Sparse { chunks } => {
                let total: usize = chunks.iter().map(|c| c.slabs.len()).sum();
                let mut out = Vec::with_capacity(total);
                for ch in chunks {
                    let base = ch.word as u32 * 64;
                    let mut bits = ch.mask;
                    for arc in ch.slabs {
                        let tz = bits.trailing_zeros();
                        bits &= bits - 1;
                        out.push((base + tz, arc));
                    }
                }
                out
            }
        }
    }

    #[inline]
    pub(crate) fn payload_bytes(&self) -> usize {
        match self {
            Self::Dense { slots, .. } => {
                slots.capacity() * std::mem::size_of::<Option<Arc<T>>>()
            }
            Self::Sparse { chunks } => {
                chunks.capacity() * std::mem::size_of::<ColChunk<T>>()
                    + chunks
                        .iter()
                        .map(|ch| ch.slabs.capacity() * std::mem::size_of::<Arc<T>>())
                        .sum::<usize>()
            }
        }
    }

    pub(crate) fn has_any_outside_sorted(&self, sorted_covered: &[u32]) -> bool {
        self.iter_nonempty_cols(0, u32::MAX)
            .any(|c| sorted_covered.binary_search(&c).is_err())
    }

    /// Visit the union of occupied columns in `(a, b)` within `[c1..=c2]` in ascending order,
    /// with zero heap allocation and O(occupied_cols) time even for `XFD` sparse pages.
    pub(crate) fn for_each_union_col_in(
        a: Option<&Self>,
        b: Option<&Self>,
        c1: u32,
        c2: u32,
        mut f: impl FnMut(u32, Option<&Arc<T>>, Option<&Arc<T>>),
    ) {
        if c1 > c2 {
            return;
        }
        let mut ia = a.map(|x| x.iter_entries_in(c1, c2)).into_iter().flatten().peekable();
        let mut ib = b.map(|x| x.iter_entries_in(c1, c2)).into_iter().flatten().peekable();
        loop {
            match (ia.peek().copied(), ib.peek().copied()) {
                (Some((ca, sa)), Some((cb, sb))) => match ca.cmp(&cb) {
                    std::cmp::Ordering::Less => {
                        ia.next();
                        f(ca, Some(sa), None);
                    }
                    std::cmp::Ordering::Greater => {
                        ib.next();
                        f(cb, None, Some(sb));
                    }
                    std::cmp::Ordering::Equal => {
                        ia.next();
                        ib.next();
                        f(ca, Some(sa), Some(sb));
                    }
                },
                (Some((ca, sa)), None) => {
                    ia.next();
                    f(ca, Some(sa), None);
                }
                (None, Some((cb, sb))) => {
                    ib.next();
                    f(cb, None, Some(sb));
                }
                (None, None) => break,
            }
        }
    }

    pub(crate) fn all_union_cols_in(
        a: Option<&Self>,
        b: Option<&Self>,
        c1: u32,
        c2: u32,
        mut f: impl FnMut(u32, Option<&Arc<T>>, Option<&Arc<T>>) -> bool,
    ) -> bool {
        if c1 > c2 {
            return true;
        }
        let mut ia = a.map(|x| x.iter_entries_in(c1, c2)).into_iter().flatten().peekable();
        let mut ib = b.map(|x| x.iter_entries_in(c1, c2)).into_iter().flatten().peekable();
        loop {
            let ok = match (ia.peek().copied(), ib.peek().copied()) {
                (Some((ca, sa)), Some((cb, sb))) => match ca.cmp(&cb) {
                    std::cmp::Ordering::Less => {
                        ia.next();
                        f(ca, Some(sa), None)
                    }
                    std::cmp::Ordering::Greater => {
                        ib.next();
                        f(cb, None, Some(sb))
                    }
                    std::cmp::Ordering::Equal => {
                        ia.next();
                        ib.next();
                        f(ca, Some(sa), Some(sb))
                    }
                },
                (Some((ca, sa)), None) => {
                    ia.next();
                    f(ca, Some(sa), None)
                }
                (None, Some((cb, sb))) => {
                    ib.next();
                    f(cb, None, Some(sb))
                }
                (None, None) => return true,
            };
            if !ok {
                return false;
            }
        }
    }
}

pub(crate) enum PageColIdxIter<'a, T> {
    Empty,
    Dense {
        words: [u64; 4],
        word: usize,
        end_w: usize,
    },
    Sparse {
        chunks: &'a [ColChunk<T>],
        idx: usize,
        start_w: u16,
        end_w: u16,
        c1: u32,
        c2: u32,
        cur_word: u32,
        cur_bits: u64,
    },
}

impl<'a, T> Iterator for PageColIdxIter<'a, T> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        match self {
            Self::Empty => None,
            Self::Dense { words, word, end_w } => {
                while *word <= *end_w {
                    let bits = &mut words[*word];
                    if *bits != 0 {
                        let tz = bits.trailing_zeros();
                        *bits &= *bits - 1;
                        return Some(*word as u32 * 64 + tz);
                    }
                    *word += 1;
                }
                None
            }
            Self::Sparse {
                chunks,
                idx,
                start_w,
                end_w,
                c1,
                c2,
                cur_word,
                cur_bits,
            } => loop {
                if *cur_bits != 0 {
                    let tz = cur_bits.trailing_zeros();
                    *cur_bits &= *cur_bits - 1;
                    return Some(*cur_word * 64 + tz);
                }
                let ch = chunks.get(*idx)?;
                if ch.word > *end_w {
                    return None;
                }
                *idx += 1;
                let mut bits = ch.mask;
                if ch.word == *start_w {
                    bits &= !0u64 << (*c1 % 64);
                }
                if ch.word == *end_w {
                    let hb = *c2 % 64;
                    if hb < 63 {
                        bits &= (1u64 << (hb + 1)) - 1;
                    }
                }
                *cur_word = ch.word as u32;
                *cur_bits = bits;
            },
        }
    }
}

pub(crate) enum PageColIdxRevIter<'a, T> {
    Empty,
    Dense {
        words: [u64; 4],
        word: isize,
    },
    Sparse {
        chunks: &'a [ColChunk<T>],
        idx: isize,
        end_w: u16,
        max_c: u32,
        cur_word: u32,
        cur_bits: u64,
    },
}

impl<'a, T> Iterator for PageColIdxRevIter<'a, T> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        match self {
            Self::Empty => None,
            Self::Dense { words, word } => {
                while *word >= 0 {
                    let bits = &mut words[*word as usize];
                    if *bits != 0 {
                        let bit_idx = 63 - bits.leading_zeros();
                        *bits &= !(1u64 << bit_idx);
                        return Some(*word as u32 * 64 + bit_idx);
                    }
                    *word -= 1;
                }
                None
            }
            Self::Sparse {
                chunks,
                idx,
                end_w,
                max_c,
                cur_word,
                cur_bits,
            } => loop {
                if *cur_bits != 0 {
                    let bit_idx = 63 - cur_bits.leading_zeros();
                    *cur_bits &= !(1u64 << bit_idx);
                    return Some(*cur_word * 64 + bit_idx);
                }
                if *idx < 0 {
                    return None;
                }
                let ch = &chunks[*idx as usize];
                *idx -= 1;
                let mut bits = ch.mask;
                if ch.word == *end_w {
                    let hb = *max_c % 64;
                    if hb < 63 {
                        bits &= (1u64 << (hb + 1)) - 1;
                    }
                }
                *cur_word = ch.word as u32;
                *cur_bits = bits;
            },
        }
    }
}

pub(crate) enum PageColEntriesIter<'a, T> {
    Empty,
    Dense {
        slots: &'a [Option<Arc<T>>],
        words: [u64; 4],
        word: usize,
        end_w: usize,
    },
    Sparse {
        chunks: &'a [ColChunk<T>],
        idx: usize,
        start_w: u16,
        end_w: u16,
        c1: u32,
        c2: u32,
        cur_chunk: usize,
        cur_bits: u64,
        cur_rank: usize,
    },
}

impl<'a, T> Iterator for PageColEntriesIter<'a, T> {
    type Item = (u32, &'a Arc<T>);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty => None,
            Self::Dense {
                slots,
                words,
                word,
                end_w,
            } => {
                while *word <= *end_w {
                    let bits = &mut words[*word];
                    if *bits != 0 {
                        let tz = bits.trailing_zeros();
                        *bits &= *bits - 1;
                        let col = *word as u32 * 64 + tz;
                        if let Some(Some(arc)) = slots.get(col as usize) {
                            return Some((col, arc));
                        }
                    } else {
                        *word += 1;
                    }
                }
                None
            }
            Self::Sparse {
                chunks,
                idx,
                start_w,
                end_w,
                c1,
                c2,
                cur_chunk,
                cur_bits,
                cur_rank,
            } => loop {
                if *cur_bits != 0 {
                    let tz = cur_bits.trailing_zeros();
                    *cur_bits &= *cur_bits - 1;
                    let ch = &chunks[*cur_chunk];
                    let r = *cur_rank;
                    *cur_rank = r + 1;
                    return Some((ch.word as u32 * 64 + tz, &ch.slabs[r]));
                }
                let ch_i = *idx;
                let ch = chunks.get(ch_i)?;
                if ch.word > *end_w {
                    return None;
                }
                *idx = ch_i + 1;
                let mut bits = ch.mask;
                let rank0 = if ch.word == *start_w {
                    let lo_bit = *c1 % 64;
                    let below_mask = (1u64 << lo_bit) - 1;
                    bits &= !below_mask;
                    (ch.mask & below_mask).count_ones() as usize
                } else {
                    0
                };
                if ch.word == *end_w {
                    let hb = *c2 % 64;
                    if hb < 63 {
                        bits &= (1u64 << (hb + 1)) - 1;
                    }
                }
                *cur_chunk = ch_i;
                *cur_bits = bits;
                *cur_rank = rank0;
            },
        }
    }
}

/// Page-level style container mirroring `Page`: compact `PageCols<StyleColumn>`.
#[derive(Clone, Debug, Default)]
pub(crate) struct StylePage {
    pub(crate) cols: PageCols<StyleColumn>,
}

impl PartialEq for StylePage {
    fn eq(&self, other: &Self) -> bool {
        match (&self.cols, &other.cols) {
            (
                PageCols::Dense { nonempty: na, .. },
                PageCols::Dense { nonempty: nb, .. },
            ) => {
                if na != nb {
                    return false;
                }
            }
            (PageCols::Sparse { chunks: ca }, PageCols::Sparse { chunks: cb }) => {
                if ca.len() != cb.len()
                    || ca
                        .iter()
                        .zip(cb.iter())
                        .any(|(x, y)| x.word != y.word || x.mask != y.mask)
                {
                    return false;
                }
            }
            _ => return false, // Canonical invariant: Dense (<256) vs Sparse (>=256) never equal.
        }
        let mut ia = self.cols.iter_entries();
        let mut ib = other.cols.iter_entries();
        loop {
            match (ia.next(), ib.next()) {
                (Some((ca, x)), Some((cb, y))) => {
                    if ca != cb || (!Arc::ptr_eq(x, y) && x != y) {
                        return false;
                    }
                }
                (None, None) => return true,
                _ => return false,
            }
        }
    }
}

impl Eq for StylePage {}

impl StylePage {
    #[inline]
    pub(crate) fn get_col(&self, c: u32) -> Option<&Arc<StyleColumn>> {
        self.cols.get(c)
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.cols.is_empty()
    }

    #[inline]
    fn col_count(&self) -> usize {
        self.cols.col_count()
    }

    #[inline]
    fn max_col(&self) -> Option<u32> {
        self.cols.max_col()
    }

    #[inline]
    fn set_col(&mut self, c: u32, col: Arc<StyleColumn>) -> usize {
        let prev = self.cols.set(c, col);
        prev.as_ref().map_or(0, |s| s.len())
    }

    #[inline]
    fn remove_col(&mut self, c: u32) -> Option<Arc<StyleColumn>> {
        self.cols.remove(c)
    }

    #[inline]
    pub(crate) fn iter_nonempty_cols(&self, c1: u32, c2: u32) -> PageColIdxIter<'_, StyleColumn> {
        self.cols.iter_nonempty_cols(c1, c2)
    }
}

/// Explicit styles, including style zero overriding a row or column default.
/// Mirrors `Store`'s direct-indexed page directory (`< 8192` pages = 2.09M rows) +
/// `page_bits` bitmap for zero-hash O(1) lookups and page-bounded range queries.
/// Each column page either stores a single uniform style ID for all 256 rows
/// or packs its sparse `u16` IDs in row order using a 256-bit occupancy bitmap.
#[derive(Clone, Debug, Default)]
pub struct CellStyles {
    pages: Vec<Option<Arc<StylePage>>>,
    page_bits: Vec<u64>,
    far_pages: Option<Box<BTreeMap<u32, Arc<StylePage>>>>,
    len: usize,
}

impl PartialEq for CellStyles {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len || self.page_bits != other.page_bits {
            return false;
        }
        for (pi, a) in self.iter_direct_pages_in(0, DIRECT_PAGE_LIMIT - 1) {
            let Some(b) = other.page(pi) else {
                return false;
            };
            if !Arc::ptr_eq(a, b) && a != b {
                return false;
            }
        }
        match (&self.far_pages, &other.far_pages) {
            (None, None) => true,
            (Some(a), None) => a.is_empty(),
            (None, Some(b)) => b.is_empty(),
            (Some(a), Some(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(k, va)| {
                        b.get(k)
                            .is_some_and(|vb| Arc::ptr_eq(va, vb) || va == vb)
                    })
            }
        }
    }
}

impl Eq for CellStyles {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StyleColumn {
    /// All 256 rows in this `(page, col)` share the exact same explicit style ID.
    Uniform(u16),
    Sparse {
        rows: [u64; 4],
        ids: Vec<u16>,
    },
}

impl Default for StyleColumn {
    fn default() -> Self {
        Self::Sparse {
            rows: [0; 4],
            ids: Vec::new(),
        }
    }
}

impl StyleColumn {
    #[inline]
    fn sparse_position(rows: &[u64; 4], offset: u32) -> (usize, u64, usize) {
        let word = (offset / 64) as usize;
        let bit = 1u64 << (offset % 64);
        let rank = rows[..word].iter().map(|w| w.count_ones()).sum::<u32>()
            + (rows[word] & (bit - 1)).count_ones();
        (word, bit, rank as usize)
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Uniform(_) => PAGE_ROWS as usize,
            Self::Sparse { ids, .. } => ids.len(),
        }
    }

    #[inline]
    pub(crate) fn is_uniform(&self) -> bool {
        matches!(self, Self::Uniform(_))
    }

    #[inline]
    pub(crate) fn row_mask(&self) -> [u64; 4] {
        match self {
            Self::Uniform(_) => [u64::MAX; 4],
            Self::Sparse { rows, .. } => *rows,
        }
    }

    #[inline]
    pub(crate) fn get(&self, offset: u32) -> Option<&u16> {
        match self {
            Self::Uniform(id) => Some(id),
            Self::Sparse { rows, ids } => {
                let (word, bit, rank) = Self::sparse_position(rows, offset);
                (rows[word] & bit != 0).then(|| &ids[rank])
            }
        }
    }

    pub(crate) fn from_dense(slice: &[Option<u16>]) -> Option<Arc<Self>> {
        debug_assert!(slice.len() <= PAGE_ROWS as usize);
        if slice.len() == PAGE_ROWS as usize
            && let Some(first) = slice[0]
            && slice.iter().all(|&x| x == Some(first))
        {
            return Some(Arc::new(Self::Uniform(first)));
        }
        let mut rows = [0u64; 4];
        let mut ids = Vec::new();
        for (i, &opt) in slice.iter().enumerate() {
            if let Some(id) = opt {
                rows[i / 64] |= 1u64 << (i % 64);
                ids.push(id);
            }
        }
        if ids.is_empty() {
            None
        } else {
            Some(Arc::new(Self::Sparse { rows, ids }))
        }
    }

    fn insert(&mut self, offset: u32, id: u16) -> Option<u16> {
        match self {
            Self::Uniform(cur) => {
                let old = *cur;
                if old == id {
                    return Some(old);
                }
                let mut ids = vec![old; PAGE_ROWS as usize];
                ids[offset as usize] = id;
                *self = Self::Sparse {
                    rows: [u64::MAX; 4],
                    ids,
                };
                Some(old)
            }
            Self::Sparse { rows, ids } => {
                let (word, bit, rank) = Self::sparse_position(rows, offset);
                if rows[word] & bit != 0 {
                    let old = std::mem::replace(&mut ids[rank], id);
                    if *rows == [u64::MAX; 4] && ids.iter().all(|&x| x == id) {
                        *self = Self::Uniform(id);
                    }
                    return Some(old);
                }
                rows[word] |= bit;
                ids.insert(rank, id);
                if *rows == [u64::MAX; 4] && ids.iter().all(|&x| x == id) {
                    *self = Self::Uniform(id);
                }
                None
            }
        }
    }

    /// Insert `id` for all offsets in `lo..=hi` in a single pass. Returns newly added cell count.
    fn insert_range(&mut self, lo: u32, hi: u32, id: u16) -> usize {
        debug_assert!(lo <= hi && hi < PAGE_ROWS);
        if lo == 0 && hi == PAGE_ROWS - 1 {
            let prev = self.len();
            *self = Self::Uniform(id);
            return PAGE_ROWS as usize - prev;
        }
        match self {
            Self::Uniform(cur) => {
                let old = *cur;
                if old == id {
                    return 0;
                }
                let mut ids = vec![old; PAGE_ROWS as usize];
                ids[lo as usize..=hi as usize].fill(id);
                *self = Self::Sparse {
                    rows: [u64::MAX; 4],
                    ids,
                };
                0
            }
            Self::Sparse { rows, ids } => {
                let m = row_range_mask(lo, hi);
                let added: usize = (0..4)
                    .map(|w| (m[w] & !rows[w]).count_ones() as usize)
                    .sum();
                let lo_rank = count_bits_below(rows, lo);
                let after_hi_rank = count_bits_below(rows, hi + 1);
                let span = (hi - lo + 1) as usize;
                if added == 0 {
                    ids[lo_rank..lo_rank + span].fill(id);
                    if *rows == [u64::MAX; 4] && ids.iter().all(|&x| x == id) {
                        *self = Self::Uniform(id);
                    }
                    return 0;
                }
                let new_rows = [
                    rows[0] | m[0],
                    rows[1] | m[1],
                    rows[2] | m[2],
                    rows[3] | m[3],
                ];
                let mut new_ids = Vec::with_capacity(ids.len() + added);
                new_ids.extend_from_slice(&ids[..lo_rank]);
                new_ids.resize(lo_rank + span, id);
                new_ids.extend_from_slice(&ids[after_hi_rank..]);
                if new_rows == [u64::MAX; 4] && new_ids.iter().all(|&x| x == id) {
                    *self = Self::Uniform(id);
                } else {
                    *rows = new_rows;
                    *ids = new_ids;
                }
                added
            }
        }
    }

    fn remove(&mut self, offset: u32) -> (Option<u16>, bool) {
        match self {
            Self::Uniform(cur) => {
                let old = *cur;
                let mut rows = [u64::MAX; 4];
                rows[(offset / 64) as usize] &= !(1u64 << (offset % 64));
                let ids = vec![old; (PAGE_ROWS - 1) as usize];
                *self = Self::Sparse { rows, ids };
                (Some(old), false)
            }
            Self::Sparse { rows, ids } => {
                let (word, bit, rank) = Self::sparse_position(rows, offset);
                if rows[word] & bit == 0 {
                    return (None, ids.is_empty());
                }
                rows[word] &= !bit;
                let old = ids.remove(rank);
                (Some(old), ids.is_empty())
            }
        }
    }

    /// Remove all explicit entries in `lo..=hi`. Returns `(removed_count, is_now_empty, removed_mask)`.
    fn remove_range(&mut self, lo: u32, hi: u32) -> (usize, bool, [u64; 4]) {
        debug_assert!(lo <= hi && hi < PAGE_ROWS);
        let m = row_range_mask(lo, hi);
        if lo == 0 && hi == PAGE_ROWS - 1 {
            let mask = self.row_mask();
            let removed = self.len();
            *self = Self::default();
            return (removed, true, mask);
        }
        match self {
            Self::Uniform(cur) => {
                let old = *cur;
                let kept_rows = [!m[0], !m[1], !m[2], !m[3]];
                let removed = (hi - lo + 1) as usize;
                let kept = PAGE_ROWS as usize - removed;
                *self = Self::Sparse {
                    rows: kept_rows,
                    ids: vec![old; kept],
                };
                (removed, kept == 0, m)
            }
            Self::Sparse { rows, ids } => {
                let removed_mask = [
                    rows[0] & m[0],
                    rows[1] & m[1],
                    rows[2] & m[2],
                    rows[3] & m[3],
                ];
                let removed: usize = removed_mask
                    .iter()
                    .map(|w| w.count_ones() as usize)
                    .sum();
                if removed == 0 {
                    return (0, ids.is_empty(), [0; 4]);
                }
                let lo_rank = count_bits_below(rows, lo);
                let after_hi_rank = count_bits_below(rows, hi + 1);
                ids.drain(lo_rank..after_hi_rank);
                for w in 0..4 {
                    rows[w] &= !m[w];
                }
                (removed, ids.is_empty(), removed_mask)
            }
        }
    }

    /// Bitmask of row offsets within this 256-row slab where `self.get(r) != other.get(r)`.
    fn diff_mask(&self, other: &Self) -> [u64; 4] {
        match (self, other) {
            (Self::Uniform(a), Self::Uniform(b)) => {
                if a == b {
                    [0; 4]
                } else {
                    [u64::MAX; 4]
                }
            }
            (Self::Uniform(u), Self::Sparse { rows, ids })
            | (Self::Sparse { rows, ids }, Self::Uniform(u)) => {
                let mut mask = [!rows[0], !rows[1], !rows[2], !rows[3]];
                let mut idx = 0usize;
                for (w, &bits_orig) in rows.iter().enumerate() {
                    let mut bits = bits_orig;
                    while bits != 0 {
                        let tz = bits.trailing_zeros();
                        bits &= bits - 1;
                        if ids[idx] != *u {
                            mask[w] |= 1u64 << tz;
                        }
                        idx += 1;
                    }
                }
                mask
            }
            (
                Self::Sparse {
                    rows: ra,
                    ids: ia,
                },
                Self::Sparse {
                    rows: rb,
                    ids: ib,
                },
            ) => {
                let mut mask = [
                    ra[0] ^ rb[0],
                    ra[1] ^ rb[1],
                    ra[2] ^ rb[2],
                    ra[3] ^ rb[3],
                ];
                let mut pa = 0usize;
                let mut pb = 0usize;
                for w in 0..4 {
                    let mut union = ra[w] | rb[w];
                    while union != 0 {
                        let tz = union.trailing_zeros();
                        let bit = 1u64 << tz;
                        union &= union - 1;
                        let ha = (ra[w] & bit) != 0;
                        let hb = (rb[w] & bit) != 0;
                        match (ha, hb) {
                            (true, true) => {
                                if ia[pa] != ib[pb] {
                                    mask[w] |= bit;
                                }
                                pa += 1;
                                pb += 1;
                            }
                            (true, false) => pa += 1,
                            (false, true) => pb += 1,
                            (false, false) => {}
                        }
                    }
                }
                mask
            }
        }
    }

    #[inline]
    fn max_offset(&self) -> Option<u32> {
        match self {
            Self::Uniform(_) => Some(PAGE_ROWS - 1),
            Self::Sparse { rows, .. } => {
                for w in (0..4).rev() {
                    if rows[w] != 0 {
                        let bit = 63 - rows[w].leading_zeros();
                        return Some(w as u32 * 64 + bit);
                    }
                }
                None
            }
        }
    }

    fn iter_entries<'a>(&'a self, p: u32, col: u32) -> StyleColumnIter<'a> {
        self.iter_entries_in(p, col, 0, PAGE_ROWS - 1)
    }

    fn iter_entries_in<'a>(&'a self, p: u32, col: u32, lo: u32, hi: u32) -> StyleColumnIter<'a> {
        let base = p * PAGE_ROWS;
        let hi = hi.min(PAGE_ROWS - 1);
        if lo > hi {
            return StyleColumnIter::Sparse {
                base,
                col,
                rows: [0; 4],
                word: 4,
                ids: [].iter(),
            };
        }
        match self {
            Self::Uniform(id) => StyleColumnIter::Uniform {
                base,
                col,
                off: lo,
                end: hi + 1,
                id,
            },
            Self::Sparse { rows, ids } => {
                let m = row_range_mask(lo, hi);
                let masked = [
                    rows[0] & m[0],
                    rows[1] & m[1],
                    rows[2] & m[2],
                    rows[3] & m[3],
                ];
                let lo_rank = count_bits_below(rows, lo);
                StyleColumnIter::Sparse {
                    base,
                    col,
                    rows: masked,
                    word: (lo / 64) as usize,
                    ids: ids[lo_rank..].iter(),
                }
            }
        }
    }
}

enum StyleColumnIter<'a> {
    Uniform {
        base: u32,
        col: u32,
        off: u32,
        end: u32,
        id: &'a u16,
    },
    Sparse {
        base: u32,
        col: u32,
        rows: [u64; 4],
        word: usize,
        ids: std::slice::Iter<'a, u16>,
    },
}

impl<'a> Iterator for StyleColumnIter<'a> {
    type Item = (CellRef, &'a u16);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Uniform {
                base,
                col,
                off,
                end,
                id,
            } => {
                if *off >= *end {
                    return None;
                }
                let r = *base + *off;
                *off += 1;
                Some((CellRef::new(r, *col), *id))
            }
            Self::Sparse {
                base,
                col,
                rows,
                word,
                ids,
            } => {
                while *word < 4 {
                    let bits = &mut rows[*word];
                    if *bits != 0 {
                        let tz = bits.trailing_zeros();
                        *bits &= *bits - 1;
                        let id = ids.next()?;
                        return Some((CellRef::new(*base + *word as u32 * 64 + tz, *col), id));
                    }
                    *word += 1;
                }
                None
            }
        }
    }
}

impl CellStyles {
    #[inline]
    pub(crate) fn page(&self, pi: u32) -> Option<&Arc<StylePage>> {
        if pi < DIRECT_PAGE_LIMIT {
            self.pages.get(pi as usize)?.as_ref()
        } else {
            self.far_pages.as_ref()?.get(&pi)
        }
    }

    #[inline]
    pub(crate) fn page_col(&self, pi: u32, col: u32) -> Option<&Arc<StyleColumn>> {
        self.page(pi)?.get_col(col)
    }

    #[inline]
    fn set_page_bit(&mut self, pi: u32) {
        if pi < DIRECT_PAGE_LIMIT {
            let w = (pi / 64) as usize;
            if self.page_bits.len() <= w {
                self.page_bits.resize(w + 1, 0);
            }
            self.page_bits[w] |= 1u64 << (pi % 64);
        }
    }

    #[inline]
    fn clear_page_bit(&mut self, pi: u32) {
        if pi < DIRECT_PAGE_LIMIT {
            let w = (pi / 64) as usize;
            if let Some(word) = self.page_bits.get_mut(w) {
                *word &= !(1u64 << (pi % 64));
                while self.page_bits.last() == Some(&0) {
                    self.page_bits.pop();
                }
            }
        }
    }

    #[inline]
    fn page_mut_or_insert(&mut self, pi: u32) -> &mut StylePage {
        if pi < DIRECT_PAGE_LIMIT {
            let idx = pi as usize;
            if self.pages.len() <= idx {
                self.pages.resize(idx + 1, None);
            }
            self.set_page_bit(pi);
            let arc = self.pages[idx].get_or_insert_with(|| Arc::new(StylePage::default()));
            Arc::make_mut(arc)
        } else {
            let map = self.far_pages.get_or_insert_with(|| Box::new(BTreeMap::new()));
            let arc = map
                .entry(pi)
                .or_insert_with(|| Arc::new(StylePage::default()));
            Arc::make_mut(arc)
        }
    }

    #[inline]
    fn page_mut_existing(&mut self, pi: u32) -> Option<&mut StylePage> {
        if pi < DIRECT_PAGE_LIMIT {
            let arc = self.pages.get_mut(pi as usize)?.as_mut()?;
            Some(Arc::make_mut(arc))
        } else {
            let arc = self.far_pages.as_mut()?.get_mut(&pi)?;
            Some(Arc::make_mut(arc))
        }
    }

    #[inline]
    fn insert_page_arc(&mut self, pi: u32, page: Arc<StylePage>) {
        if page.is_empty() {
            self.remove_page(pi);
            return;
        }
        if pi < DIRECT_PAGE_LIMIT {
            let idx = pi as usize;
            if self.pages.len() <= idx {
                self.pages.resize(idx + 1, None);
            }
            self.pages[idx] = Some(page);
            self.set_page_bit(pi);
        } else {
            self.far_pages
                .get_or_insert_with(|| Box::new(BTreeMap::new()))
                .insert(pi, page);
        }
    }

    #[inline]
    fn remove_page(&mut self, pi: u32) {
        if pi < DIRECT_PAGE_LIMIT {
            if let Some(slot) = self.pages.get_mut(pi as usize) {
                *slot = None;
            }
            self.clear_page_bit(pi);
        } else if let Some(map) = self.far_pages.as_mut() {
            map.remove(&pi);
            if map.is_empty() {
                self.far_pages = None;
            }
        }
    }

    fn iter_direct_pages_in(
        &self,
        p1: u32,
        p2: u32,
    ) -> impl Iterator<Item = (u32, &Arc<StylePage>)> + '_ {
        let direct_hi = p2.min(DIRECT_PAGE_LIMIT - 1);
        let total_words = self.page_bits.len();
        let start_w = (p1 / 64) as usize;
        let end_w = (direct_hi / 64) as usize;
        let direct_range = if p1 <= direct_hi && start_w < total_words {
            start_w..=end_w.min(total_words - 1)
        } else {
            1..=0
        };
        direct_range.flat_map(move |w| {
            let mut bits = self.page_bits[w];
            if w == start_w {
                bits &= !0u64 << (p1 % 64);
            }
            if w == end_w {
                let hi = direct_hi % 64;
                if hi < 63 {
                    bits &= (1u64 << (hi + 1)) - 1;
                }
            }
            std::iter::from_fn(move || {
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    bits &= bits - 1;
                    let pi = w as u32 * 64 + tz;
                    if let Some(Some(page)) = self.pages.get(pi as usize) {
                        return Some((pi, page));
                    }
                }
                None
            })
        })
    }

    pub(crate) fn iter_pages_in(
        &self,
        p1: u32,
        p2: u32,
    ) -> impl Iterator<Item = (u32, &Arc<StylePage>)> + '_ {
        let direct = self.iter_direct_pages_in(p1, p2);
        let far = self
            .far_pages
            .as_deref()
            .filter(move |_| p1 <= p2 && p2 >= DIRECT_PAGE_LIMIT)
            .into_iter()
            .flat_map(move |map| {
                map.range(p1.max(DIRECT_PAGE_LIMIT)..=p2)
                    .map(|(&pi, page)| (pi, page))
            });
        direct.chain(far)
    }

    pub(crate) fn iter_pages(&self) -> impl Iterator<Item = (u32, &Arc<StylePage>)> + '_ {
        self.iter_pages_in(0, u32::MAX)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn page_count(&self) -> usize {
        self.iter_pages().map(|(_, p)| p.col_count()).sum()
    }

    pub fn uniform_page_count(&self) -> usize {
        self.iter_pages()
            .map(|(_, p)| {
                p.iter_nonempty_cols(0, u32::MAX)
                    .filter(|&c| p.get_col(c).is_some_and(|sc| sc.is_uniform()))
                    .count()
            })
            .sum()
    }

    #[inline]
    pub fn get(&self, key: &CellRef) -> Option<&u16> {
        if self.len == 0 {
            return None;
        }
        let col = self.page_col(key.row / PAGE_ROWS, key.col)?;
        col.get(key.row % PAGE_ROWS)
    }

    pub fn insert(&mut self, key: CellRef, id: u16) -> Option<u16> {
        let pi = key.row / PAGE_ROWS;
        let offset = key.row % PAGE_ROWS;
        if let Some(existing) = self.page_col(pi, key.col) {
            if existing.get(offset) == Some(&id) {
                return Some(id);
            }
        }
        let page = self.page_mut_or_insert(pi);
        let slot = page
            .cols
            .get_or_insert_with(key.col, || Arc::new(StyleColumn::default()));
        let old = Arc::make_mut(slot).insert(offset, id);
        if old.is_none() {
            self.len += 1;
        }
        old
    }

    /// Batch-insert style `id` across `[r1..=r2, c1..=c2]`.
    /// Complete 256-row column pages share a single `Arc<StyleColumn::Uniform(id)>`!
    pub fn insert_rect(&mut self, r1: u32, c1: u32, r2: u32, c2: u32, id: u16) {
        if r1 > r2 || c1 > c2 {
            return;
        }
        let first_p = r1 / PAGE_ROWS;
        let last_p = r2 / PAGE_ROWS;
        let uniform = Arc::new(StyleColumn::Uniform(id));
        for p in first_p..=last_p {
            let base = p * PAGE_ROWS;
            let lo = r1.saturating_sub(base);
            let hi = (r2 - base).min(PAGE_ROWS - 1);
            let page = self.page_mut_or_insert(p);
            let mut delta: isize = 0;
            if lo == 0 && hi == PAGE_ROWS - 1 {
                for col in c1..=c2 {
                    let prev_len = page.set_col(col, uniform.clone());
                    delta += PAGE_ROWS as isize - prev_len as isize;
                }
            } else {
                for col in c1..=c2 {
                    let slot = page
                        .cols
                        .get_or_insert_with(col, || Arc::new(StyleColumn::default()));
                    if let StyleColumn::Uniform(cur) = slot.as_ref()
                        && *cur == id
                    {
                        continue;
                    }
                    let col_mut = Arc::make_mut(slot);
                    let added = col_mut.insert_range(lo, hi, id);
                    delta += added as isize;
                    if let StyleColumn::Uniform(cur) = col_mut
                        && *cur == id
                    {
                        *slot = uniform.clone();
                    }
                }
            }
            self.len = (self.len as isize + delta) as usize;
        }
    }

    pub fn remove(&mut self, key: &CellRef) -> Option<u16> {
        self.get(key)?;
        let pi = key.row / PAGE_ROWS;
        let page = self.page_mut_existing(pi)?;
        let slot = page.cols.get_mut(key.col)?;
        let (old, empty) = Arc::make_mut(slot).remove(key.row % PAGE_ROWS);
        if old.is_some() {
            if empty {
                page.remove_col(key.col);
                if page.is_empty() {
                    self.remove_page(pi);
                }
            }
            self.len -= 1;
        }
        old
    }

    /// Page-level removal of all explicit styles inside `[r1..=r2, c1..=c2]` in
    /// `O(occupied style pages & cols in rect)` time. Returns number of removed cells.
    pub fn remove_rect(&mut self, r1: u32, c1: u32, r2: u32, c2: u32) -> usize {
        self.remove_rect_inner(r1, c1, r2, c2, None)
    }

    /// Same as `remove_rect`, recording every actually removed cell into `journal`
    /// using 256-bit word masks without per-cell decoding.
    pub(crate) fn remove_rect_recording(
        &mut self,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
        journal: &mut CellJournal,
    ) -> usize {
        self.remove_rect_inner(r1, c1, r2, c2, Some(journal))
    }

    fn remove_rect_inner(
        &mut self,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
        mut journal: Option<&mut CellJournal>,
    ) -> usize {
        if r1 > r2 || c1 > c2 || self.len == 0 {
            return 0;
        }
        let first_p = r1 / PAGE_ROWS;
        let last_p = r2 / PAGE_ROWS;
        let hit_pages: Vec<u32> = self
            .iter_pages_in(first_p, last_p)
            .filter(|(_, page)| page.iter_nonempty_cols(c1, c2).next().is_some())
            .map(|(pi, _)| pi)
            .collect();
        if hit_pages.is_empty() {
            return 0;
        }
        let mut total_removed = 0usize;
        for pi in hit_pages {
            let base = pi * PAGE_ROWS;
            let lo = r1.saturating_sub(base);
            let hi = (r2 - base).min(PAGE_ROWS - 1);
            let full_page = lo == 0 && hi == PAGE_ROWS - 1;
            let Some(page) = self.page_mut_existing(pi) else {
                continue;
            };
            let cols: Vec<u32> = page.iter_nonempty_cols(c1, c2).collect();
            for col in cols {
                if full_page {
                    if let Some(old) = page.remove_col(col) {
                        if let Some(j) = journal.as_deref_mut() {
                            j.or_page_mask(pi, col, old.row_mask());
                        }
                        total_removed += old.len();
                    }
                } else if let Some(slot) = page.cols.get_mut(col) {
                    let (removed, empty, mask) = Arc::make_mut(slot).remove_range(lo, hi);
                    if removed > 0 {
                        if let Some(j) = journal.as_deref_mut() {
                            j.or_page_mask(pi, col, mask);
                        }
                        total_removed += removed;
                    }
                    if empty {
                        page.remove_col(col);
                    }
                }
            }
            if page.is_empty() {
                self.remove_page(pi);
            }
        }
        self.len -= total_removed;
        total_removed
    }

    /// Maximum `(row, col)` among all explicit cell styles in `O(style_pages)` time.
    pub fn max_coords(&self) -> Option<(u32, u32)> {
        if self.len == 0 {
            return None;
        }
        let mut max_r = 0u32;
        let mut max_c = 0u32;
        let mut any = false;
        let highest_page = self
            .far_pages
            .as_deref()
            .and_then(|m| m.iter().next_back().map(|(&pi, p)| (pi, p)))
            .or_else(|| {
                for w in (0..self.page_bits.len()).rev() {
                    if self.page_bits[w] != 0 {
                        let bit = 63 - self.page_bits[w].leading_zeros();
                        let pi = w as u32 * 64 + bit;
                        if let Some(Some(p)) = self.pages.get(pi as usize) {
                            return Some((pi, p));
                        }
                    }
                }
                None
            });
        if let Some((pi, page)) = highest_page {
            let base = pi * PAGE_ROWS;
            for c in page.iter_nonempty_cols(0, u32::MAX) {
                if let Some(off) = page.get_col(c).and_then(|sc| sc.max_offset()) {
                    let r = base + off;
                    if !any || r > max_r {
                        max_r = r;
                    }
                    any = true;
                }
            }
        }
        for (_, page) in self.iter_pages() {
            if let Some(c) = page.max_col() {
                if !any || c > max_c {
                    max_c = c;
                }
                any = true;
            }
        }
        any.then_some((max_r, max_c))
    }

    /// Iterate `(CellRef, style_id)` inside `rect = (r1, c1, r2, c2)` scanning ONLY intersecting pages and columns.
    pub fn iter_rect(
        &self,
        (r1, c1, r2, c2): (u32, u32, u32, u32),
    ) -> impl Iterator<Item = (CellRef, u16)> + '_ {
        let valid = r1 <= r2 && c1 <= c2 && self.len > 0;
        let p1 = if valid { r1 / PAGE_ROWS } else { 1 };
        let p2 = if valid { r2 / PAGE_ROWS } else { 0 };
        self.iter_pages_in(p1, p2).flat_map(move |(pi, page)| {
            let base = pi * PAGE_ROWS;
            let lo = r1.saturating_sub(base);
            let hi = (r2 - base).min(PAGE_ROWS - 1);
            page.iter_nonempty_cols(c1, c2).flat_map(move |col| {
                page.get_col(col)
                    .into_iter()
                    .flat_map(move |sc| sc.iter_entries_in(pi, col, lo, hi).map(|(k, &id)| (k, id)))
            })
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = (CellRef, &u16)> {
        self.iter_from(true, 0)
    }

    pub(crate) fn iter_from(&self, rows: bool, at: u32) -> impl Iterator<Item = (CellRef, &u16)> {
        let p1 = if rows { at / PAGE_ROWS } else { 0 };
        let c1 = if rows { 0 } else { at };
        self.iter_pages_in(p1, u32::MAX).flat_map(move |(pi, page)| {
            let base = pi * PAGE_ROWS;
            let lo = if rows { at.saturating_sub(base) } else { 0 };
            page.iter_nonempty_cols(c1, u32::MAX).flat_map(move |col| {
                page.get_col(col)
                    .into_iter()
                    .flat_map(move |sc| sc.iter_entries_in(pi, col, lo, PAGE_ROWS - 1))
            })
        })
    }

    /// Re-key complete style pages when possible. Repack split pages in ascending
    /// row order so a run of equal styles never shifts the packed vector per cell,
    /// and preserve `StyleColumn::Uniform(sid)` pages via contiguous `insert_rect`.
    pub(crate) fn remap_axis(&mut self, rows: bool, at: u32, map: impl Fn(u32) -> Option<u32>) {
        if self.len == 0 {
            return;
        }
        if rows && self.iter_pages_in(at / PAGE_ROWS, u32::MAX).next().is_none() {
            return;
        }
        if !rows
            && self
                .iter_pages()
                .all(|(_, p)| p.iter_nonempty_cols(at, u32::MAX).next().is_none())
        {
            return;
        }
        let old = std::mem::take(self);
        let mut out = CellStyles::default();
        for (p, page_arc) in old.iter_pages() {
            let base = p * PAGE_ROWS;
            if rows && base + PAGE_ROWS <= at {
                let page_len: usize = page_arc
                    .iter_nonempty_cols(0, u32::MAX)
                    .filter_map(|c| page_arc.get_col(c))
                    .map(|sc| sc.len())
                    .sum();
                out.insert_page_arc(p, page_arc.clone());
                out.len += page_len;
                continue;
            }
            if rows
                && base >= at
                && let Some(target0) = map(base)
                && target0 % PAGE_ROWS == 0
                && map(base + PAGE_ROWS - 1) == Some(target0 + PAGE_ROWS - 1)
            {
                let dest = target0 / PAGE_ROWS;
                if out.page(dest).is_none() {
                    let page_len: usize = page_arc
                        .iter_nonempty_cols(0, u32::MAX)
                        .filter_map(|c| page_arc.get_col(c))
                        .map(|sc| sc.len())
                        .sum();
                    out.insert_page_arc(dest, page_arc.clone());
                    out.len += page_len;
                    continue;
                }
            }
            for c in page_arc.iter_nonempty_cols(0, u32::MAX) {
                let Some(sc) = page_arc.get_col(c) else { continue };
                let same_key = if !rows {
                    if c < at {
                        Some((p, c))
                    } else {
                        map(c).map(|col| (p, col))
                    }
                } else if base + PAGE_ROWS <= at {
                    Some((p, c))
                } else {
                    map(base)
                        .filter(|r| {
                            r % PAGE_ROWS == 0
                                && map(base + PAGE_ROWS - 1) == Some(r + PAGE_ROWS - 1)
                        })
                        .map(|r| (r / PAGE_ROWS, c))
                };
                if let Some((dp, dc)) = same_key {
                    let dp_mut = out.page_mut_or_insert(dp);
                    let prev = dp_mut.set_col(dc, sc.clone());
                    out.len = out.len - prev + sc.len();
                } else if rows {
                    if let StyleColumn::Uniform(sid) = sc.as_ref() {
                        let mut run: Option<(u32, u32)> = None;
                        for off in 0..PAGE_ROWS {
                            if let Some(target) = map(base + off) {
                                match &mut run {
                                    Some((_, end)) if target == *end + 1 => *end = target,
                                    Some((start, end)) => {
                                        out.insert_rect(*start, c, *end, c, *sid);
                                        run = Some((target, target));
                                    }
                                    None => run = Some((target, target)),
                                }
                            }
                        }
                        if let Some((start, end)) = run {
                            out.insert_rect(start, c, end, c, *sid);
                        }
                    } else {
                        for (cref, &id) in sc.iter_entries(p, c) {
                            if let Some(target) = map(cref.row) {
                                out.insert(CellRef::new(target, c), id);
                            }
                        }
                    }
                }
            }
        }
        *self = out;
    }

    /// Shift rows `>= at` ONLY for columns `[c1..=c2]` (insert down by `n` or delete `[at..at+n)`),
    /// recording vacated and written style coordinates into `style_edits` via page masks.
    pub(crate) fn shift_col_band_rows(
        &mut self,
        c1: u32,
        c2: u32,
        at: u32,
        n: u32,
        delete: bool,
        style_edits: &mut CellJournal,
    ) {
        if n == 0 || c1 > c2 || self.len == 0 {
            return;
        }
        let first_p = at / PAGE_ROWS;
        let hit_pages: Vec<u32> = self
            .iter_pages_in(first_p, u32::MAX)
            .filter(|(_, p)| p.iter_nonempty_cols(c1, c2).next().is_some())
            .map(|(pi, _)| pi)
            .collect();
        if hit_pages.is_empty() {
            return;
        }
        let mut extracted: Vec<(u32, u32, Arc<StyleColumn>)> = Vec::new();
        for pi in hit_pages {
            let base = pi * PAGE_ROWS;
            let lo = at.saturating_sub(base);
            let Some(page) = self.page_mut_existing(pi) else {
                continue;
            };
            let cols: Vec<u32> = page.iter_nonempty_cols(c1, c2).collect();
            let mut removed_page = 0usize;
            for col in cols {
                if lo == 0 {
                    if let Some(sc) = page.remove_col(col) {
                        removed_page += sc.len();
                        style_edits.or_page_mask(pi, col, sc.row_mask());
                        extracted.push((pi, col, sc));
                    }
                } else if let Some(slot) = page.cols.get_mut(col) {
                    let snapshot = slot.clone();
                    let (removed, empty, removed_mask) =
                        Arc::make_mut(slot).remove_range(lo, PAGE_ROWS - 1);
                    removed_page += removed;
                    if removed_mask != [0; 4] {
                        style_edits.or_page_mask(pi, col, removed_mask);
                    }
                    if empty {
                        page.remove_col(col);
                    }
                    extracted.push((pi, col, snapshot));
                }
            }
            let page_empty = page.is_empty();
            self.len -= removed_page;
            if page_empty {
                self.remove_page(pi);
            }
        }
        let map_row = |r: u32| -> Option<u32> {
            if delete {
                if r < at {
                    Some(r)
                } else if r < at.saturating_add(n) {
                    None
                } else {
                    Some(r - n)
                }
            } else if r >= at {
                r.checked_add(n)
            } else {
                Some(r)
            }
        };
        for (pi, col, sc) in extracted {
            let base = pi * PAGE_ROWS;
            if base >= at
                && let Some(target0) = map_row(base)
                && target0 % PAGE_ROWS == 0
                && map_row(base + PAGE_ROWS - 1) == Some(target0 + PAGE_ROWS - 1)
            {
                let dp = target0 / PAGE_ROWS;
                style_edits.or_page_mask(dp, col, sc.row_mask());
                let prev = self.page_mut_or_insert(dp).set_col(col, sc.clone());
                self.len = self.len - prev + sc.len();
                continue;
            }
            let lo = at.saturating_sub(base);
            if let StyleColumn::Uniform(sid) = sc.as_ref() {
                let mut run: Option<(u32, u32)> = None;
                for off in lo..PAGE_ROWS {
                    if let Some(target) = map_row(base + off) {
                        match &mut run {
                            Some((_, end)) if target == *end + 1 => *end = target,
                            Some((start, end)) => {
                                self.insert_rect(*start, col, *end, col, *sid);
                                style_edits.insert_rect(*start, col, *end, col);
                                run = Some((target, target));
                            }
                            None => run = Some((target, target)),
                        }
                    }
                }
                if let Some((start, end)) = run {
                    self.insert_rect(start, col, end, col, *sid);
                    style_edits.insert_rect(start, col, end, col);
                }
            } else {
                for (cref, &id) in sc.iter_entries_in(pi, col, lo, PAGE_ROWS - 1) {
                    if let Some(target) = map_row(cref.row) {
                        let dst = CellRef::new(target, col);
                        self.insert(dst, id);
                        style_edits.insert(dst);
                    }
                }
            }
        }
    }

    /// Install pre-built column style slabs for `(col, p_lo..=p_hi)` (used by parallel gather sort).
    pub(crate) fn install_col_pages(
        &mut self,
        col: u32,
        p_lo: u32,
        new_slabs: Vec<Option<Arc<StyleColumn>>>,
    ) {
        for (i, slab_opt) in new_slabs.into_iter().enumerate() {
            let pi = p_lo + i as u32;
            match slab_opt {
                Some(slab) => {
                    let new_len = slab.len();
                    let page = self.page_mut_or_insert(pi);
                    let prev_len = page.set_col(col, slab);
                    self.len = self.len - prev_len + new_len;
                }
                None => {
                    let mut removed = 0usize;
                    let mut remove_pg = false;
                    if let Some(page) = self.page_mut_existing(pi) {
                        if let Some(old) = page.remove_col(col) {
                            removed = old.len();
                        }
                        remove_pg = page.is_empty();
                    }
                    self.len -= removed;
                    if remove_pg {
                        self.remove_page(pi);
                    }
                }
            }
        }
    }

    /// Permute coordinates strictly within `[lo..hi)` while leaving all pages outside `[lo..hi)` untouched.
    pub(crate) fn permute_band(&mut self, rows: bool, lo: u32, hi: u32, map: impl Fn(u32) -> u32) {
        if lo >= hi || self.is_empty() {
            return;
        }
        let p_lo = lo / PAGE_ROWS;
        let p_hi = (hi - 1) / PAGE_ROWS;
        if !rows {
            let page_ids: Vec<u32> = self
                .iter_pages()
                .filter(|(_, p)| p.iter_nonempty_cols(lo, hi - 1).next().is_some())
                .map(|(pi, _)| pi)
                .collect();
            for pi in page_ids {
                let Some(page) = self.page_mut_existing(pi) else {
                    continue;
                };
                let cols: Vec<u32> = page.iter_nonempty_cols(lo, hi - 1).collect();
                let mut moved: Vec<(u32, Arc<StyleColumn>)> = Vec::with_capacity(cols.len());
                for c in cols {
                    if let Some(sc) = page.remove_col(c) {
                        moved.push((map(c), sc));
                    }
                }
                for (nc, sc) in moved {
                    page.set_col(nc, sc);
                }
            }
            return;
        }
        let mut staged: Vec<(CellRef, u16)> = Vec::new();
        let hit_pages: Vec<u32> = self.iter_pages_in(p_lo, p_hi).map(|(pi, _)| pi).collect();
        if hit_pages.is_empty() {
            return;
        }
        for pi in hit_pages {
            let base = pi * PAGE_ROWS;
            let r1 = lo.max(base);
            let r2 = (hi - 1).min(base + PAGE_ROWS - 1);
            let r_lo = r1 - base;
            let r_hi = r2 - base;
            let Some(page) = self.page_mut_existing(pi) else {
                continue;
            };
            let cols: Vec<u32> = page.iter_nonempty_cols(0, u32::MAX).collect();
            let mut removed_page = 0usize;
            for c in cols {
                if let Some(sc) = page.get_col(c) {
                    for (cref, &id) in sc.iter_entries_in(pi, c, r_lo, r_hi) {
                        staged.push((CellRef::new(map(cref.row), c), id));
                    }
                }
                if r_lo == 0 && r_hi == PAGE_ROWS - 1 {
                    if let Some(removed) = page.remove_col(c) {
                        removed_page += removed.len();
                    }
                } else if let Some(slot) = page.cols.get_mut(c) {
                    let (removed, empty, _) = Arc::make_mut(slot).remove_range(r_lo, r_hi);
                    removed_page += removed;
                    if empty {
                        page.remove_col(c);
                    }
                }
            }
            let page_empty = page.is_empty();
            self.len -= removed_page;
            if page_empty {
                self.remove_page(pi);
            }
        }
        for (cref, id) in staged {
            self.insert(cref, id);
        }
    }

    pub fn payload_bytes(&self) -> usize {
        let mut seen_pages = std::collections::HashSet::new();
        let mut seen_cols = std::collections::HashSet::new();
        let mut bytes = self.pages.capacity() * std::mem::size_of::<Option<Arc<StylePage>>>()
            + self.page_bits.capacity() * std::mem::size_of::<u64>();
        for (_, page) in self.iter_pages() {
            if seen_pages.insert(Arc::as_ptr(page) as usize) {
                bytes += std::mem::size_of::<StylePage>() + page.cols.payload_bytes();
                for (_, sc) in page.cols.iter_entries() {
                    if seen_cols.insert(Arc::as_ptr(sc) as usize) {
                        bytes += std::mem::size_of::<StyleColumn>();
                        if let StyleColumn::Sparse { ids, .. } = sc.as_ref() {
                            bytes += ids.capacity() * std::mem::size_of::<u16>();
                        }
                    }
                }
            }
        }
        bytes
    }

    /// Retained heap bytes of style pages/columns forked between `self` and `other`
    /// (for accurate CoW undo memory accounting).
    pub(crate) fn forked_payload_bytes(&self, other: &Self) -> usize {
        let mut seen_cols = std::collections::HashSet::new();
        let mut bytes = 0usize;
        let mut account_col = |sc: &Arc<StyleColumn>| {
            if seen_cols.insert(Arc::as_ptr(sc) as usize) {
                bytes += std::mem::size_of::<StyleColumn>();
                if let StyleColumn::Sparse { ids, .. } = sc.as_ref() {
                    bytes += ids.capacity() * std::mem::size_of::<u16>();
                }
            }
        };
        let mut check_pair = |pa: Option<&Arc<StylePage>>, pb: Option<&Arc<StylePage>>| {
            match (pa, pb) {
                (None, None) => return,
                (Some(a), Some(b)) if Arc::ptr_eq(a, b) => return,
                _ => {}
            }
            PageCols::for_each_union_col_in(
                pa.map(|p| &p.cols),
                pb.map(|p| &p.cols),
                0,
                u32::MAX,
                |_col, ca, cb| match (ca, cb) {
                    (Some(a), Some(b)) if Arc::ptr_eq(a, b) => {}
                    (Some(a), Some(b)) => {
                        account_col(a);
                        account_col(b);
                    }
                    (Some(a), None) => account_col(a),
                    (None, Some(b)) => account_col(b),
                    (None, None) => {}
                },
            );
        };
        let max_direct = self.page_bits.len().max(other.page_bits.len());
        for w in 0..max_direct {
            let mut bits = self.page_bits.get(w).copied().unwrap_or(0)
                | other.page_bits.get(w).copied().unwrap_or(0);
            while bits != 0 {
                let tz = bits.trailing_zeros();
                bits &= bits - 1;
                let pi = w as u32 * 64 + tz;
                check_pair(self.page(pi), other.page(pi));
            }
        }
        if self.far_pages.is_some() || other.far_pages.is_some() {
            let mut keys: Vec<u32> = self
                .far_pages
                .iter()
                .flat_map(|m| m.keys().copied())
                .chain(other.far_pages.iter().flat_map(|m| m.keys().copied()))
                .collect();
            keys.sort_unstable();
            keys.dedup();
            for pi in keys {
                check_pair(self.page(pi), other.page(pi));
            }
        }
        bytes
    }

    /// Word-level symmetric diff of explicit styles between `self` and `other`,
    /// writing `[u64; 4]` page masks directly into `out`.
    pub(crate) fn diff_into_journal(&self, other: &Self, out: &mut CellJournal) {
        let mut check_pair = |pi: u32, pa: Option<&Arc<StylePage>>, pb: Option<&Arc<StylePage>>| {
            match (pa, pb) {
                (None, None) => return,
                (Some(a), Some(b)) if Arc::ptr_eq(a, b) => return,
                _ => {}
            }
            PageCols::for_each_union_col_in(
                pa.map(|p| &p.cols),
                pb.map(|p| &p.cols),
                0,
                u32::MAX,
                |col, ca, cb| {
                    let mask = match (ca, cb) {
                        (None, None) => return,
                        (Some(a), Some(b)) if Arc::ptr_eq(a, b) => return,
                        (Some(a), Some(b)) => a.diff_mask(b),
                        (Some(a), None) => a.row_mask(),
                        (None, Some(b)) => b.row_mask(),
                    };
                    if mask != [0; 4] {
                        out.or_page_mask(pi, col, mask);
                    }
                },
            );
        };
        let max_direct = self.page_bits.len().max(other.page_bits.len());
        for w in 0..max_direct {
            let mut bits = self.page_bits.get(w).copied().unwrap_or(0)
                | other.page_bits.get(w).copied().unwrap_or(0);
            while bits != 0 {
                let tz = bits.trailing_zeros();
                bits &= bits - 1;
                let pi = w as u32 * 64 + tz;
                check_pair(pi, self.page(pi), other.page(pi));
            }
        }
        if self.far_pages.is_some() || other.far_pages.is_some() {
            let mut keys: Vec<u32> = self
                .far_pages
                .iter()
                .flat_map(|m| m.keys().copied())
                .chain(other.far_pages.iter().flat_map(|m| m.keys().copied()))
                .collect();
            keys.sort_unstable();
            keys.dedup();
            for pi in keys {
                check_pair(pi, self.page(pi), other.page(pi));
            }
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = CellRef> {
        self.iter().map(|(key, _)| key)
    }

    pub(crate) fn changed_keys<'a>(&'a self, other: &'a Self) -> impl Iterator<Item = CellRef> + 'a {
        self.iter_pages()
            .filter(move |&(pi, page)| other.page(pi).is_none_or(|old| !Arc::ptr_eq(old, page)))
            .flat_map(move |(pi, page)| {
                let old_page = other.page(pi);
                page.iter_nonempty_cols(0, u32::MAX)
                    .filter_map(move |col| {
                        let sc = page.get_col(col)?;
                        let old_sc = old_page.and_then(|op| op.get_col(col));
                        if old_sc.is_some_and(|old| Arc::ptr_eq(old, sc) || old == sc) {
                            return None;
                        }
                        Some((pi, col, sc, old_sc))
                    })
                    .flat_map(move |(pi, col, sc, old_sc)| {
                        sc.iter_entries(pi, col).filter_map(move |(key, &id)| {
                            let old_id = old_sc.and_then(|o| o.get(key.row % PAGE_ROWS).copied());
                            (old_id != Some(id)).then_some(key)
                        })
                    })
            })
    }
}

/// Pure numeric 256-row column slab: 256-bit occupancy mask (32 B) + dense `Vec<f64>` (8 B/row).
///
/// Invariants:
/// - `values.len() <= PAGE_ROWS as usize`
/// - `values.len() == highest_bit_plus_one(&present)`
/// - Any unoccupied slot inside `0..values.len()` holds `0.0`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NumberSlab {
    pub present: [u64; 4],
    pub values: Vec<f64>,
}

impl NumberSlab {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.present == [0; 4]
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[inline]
    pub fn occupied_count(&self) -> usize {
        self.present.iter().map(|w| w.count_ones() as usize).sum()
    }

    #[inline]
    pub fn contains(&self, ro: u32) -> bool {
        ro < PAGE_ROWS && (self.present[(ro / 64) as usize] & (1u64 << (ro % 64))) != 0
    }

    #[inline]
    pub fn get(&self, ro: u32) -> Option<f64> {
        if self.contains(ro) {
            Some(self.values[ro as usize])
        } else {
            None
        }
    }

    #[inline]
    pub fn set(&mut self, ro: u32, val: f64) -> bool {
        debug_assert!(ro < PAGE_ROWS);
        let idx = ro as usize;
        let word = idx / 64;
        let bit = 1u64 << (idx % 64);
        let existed = (self.present[word] & bit) != 0;
        if self.values.len() <= idx {
            grow_slab_vec(&mut self.values, idx + 1, 0.0);
        }
        self.values[idx] = val;
        self.present[word] |= bit;
        existed
    }

    #[inline]
    pub fn remove(&mut self, ro: u32) -> Option<f64> {
        if ro >= PAGE_ROWS {
            return None;
        }
        let idx = ro as usize;
        let word = idx / 64;
        let bit = 1u64 << (idx % 64);
        if self.present[word] & bit == 0 {
            return None;
        }
        let old = self.values[idx];
        self.present[word] &= !bit;
        self.values[idx] = 0.0;
        let new_len = highest_bit_plus_one(&self.present);
        self.values.truncate(new_len);
        Some(old)
    }

    #[inline]
    pub fn first_last_occupied(&self) -> Option<(u32, u32)> {
        let mut first = None;
        for w in 0..4 {
            if self.present[w] != 0 {
                first = Some(w as u32 * 64 + self.present[w].trailing_zeros());
                break;
            }
        }
        let first = first?;
        for w in (0..4).rev() {
            if self.present[w] != 0 {
                return Some((first, w as u32 * 64 + (63 - self.present[w].leading_zeros())));
            }
        }
        Some((first, first))
    }

    #[inline]
    pub fn has_any_in(&self, lo: usize, hi_excl: usize) -> bool {
        let end = hi_excl.min(self.values.len());
        if lo >= end {
            return false;
        }
        let m = row_range_mask(lo as u32, (end - 1) as u32);
        ((self.present[0] & m[0])
            | (self.present[1] & m[1])
            | (self.present[2] & m[2])
            | (self.present[3] & m[3]))
            != 0
    }

    #[inline]
    pub fn next_occupied_from(&self, lo: usize) -> Option<u32> {
        if lo >= self.values.len() {
            return None;
        }
        let start_w = lo / 64;
        for w in start_w..4 {
            let mut bits = self.present[w];
            if w == start_w {
                bits &= !0u64 << (lo % 64);
            }
            if bits != 0 {
                return Some(w as u32 * 64 + bits.trailing_zeros());
            }
        }
        None
    }

    #[inline]
    pub fn prev_occupied_before(&self, hi_excl: usize) -> Option<u32> {
        let end = hi_excl.min(self.values.len());
        if end == 0 {
            return None;
        }
        let hi = end - 1;
        let end_w = hi / 64;
        for w in (0..=end_w).rev() {
            let mut bits = self.present[w];
            if w == end_w {
                let hb = hi % 64;
                if hb < 63 {
                    bits &= (1u64 << (hb + 1)) - 1;
                }
            }
            if bits != 0 {
                return Some(w as u32 * 64 + (63 - bits.leading_zeros()));
            }
        }
        None
    }

    pub fn clear_range(&mut self, lo: usize, hi_incl: usize) -> (usize, [u64; 4]) {
        let end = (hi_incl + 1).min(self.values.len());
        if lo >= end {
            return (0, [0; 4]);
        }
        let m = row_range_mask(lo as u32, (end - 1) as u32);
        let removed_mask = [
            self.present[0] & m[0],
            self.present[1] & m[1],
            self.present[2] & m[2],
            self.present[3] & m[3],
        ];
        let removed = removed_mask
            .iter()
            .map(|w| w.count_ones() as usize)
            .sum::<usize>();
        if removed == 0 {
            return (0, [0; 4]);
        }
        for w in 0..4 {
            self.present[w] &= !m[w];
        }
        self.values[lo..end].fill(0.0);
        let new_len = highest_bit_plus_one(&self.present);
        self.values.truncate(new_len);
        (removed, removed_mask)
    }

    /// True if every slot in `start..start+len` has its presence bit set.
    #[inline]
    pub fn all_present_in(present: &[u64; 4], start: usize, len: usize) -> bool {
        if len == 0 {
            return true;
        }
        let m = row_range_mask(start as u32, (start + len - 1) as u32);
        (present[0] & m[0]) == m[0]
            && (present[1] & m[1]) == m[1]
            && (present[2] & m[2]) == m[2]
            && (present[3] & m[3]) == m[3]
    }

    /// Count of occupied slots in `start..start+len` via 4 hardware `POPCNT` ops.
    #[inline]
    pub fn present_count_in(present: &[u64; 4], start: usize, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        let m = row_range_mask(start as u32, (start + len - 1) as u32);
        ((present[0] & m[0]).count_ones()
            + (present[1] & m[1]).count_ones()
            + (present[2] & m[2]).count_ones()
            + (present[3] & m[3]).count_ones()) as usize
    }
}

/// Typed 256-row column slab:
/// - `Numbers`: pure `f64` column page (8 B/row + 32 B presence bitmap, zero formula overhead).
/// - `Values`: non-formula value column page (24 B/row, preserving `Arc<str>` zero-copy).
/// - `Mixed`: formula-bearing (or `get_mut`-promoted) column page (40 B/row).
#[derive(Clone, Debug, PartialEq)]
pub enum ColSlab {
    Numbers(NumberSlab),
    Values(Vec<Value>),
    Mixed(Vec<Cell>),
}

/// Zero-copy borrowed slice of a column slab across `[start..end)` of a single 256-row page.
#[derive(Clone, Copy, Debug)]
pub enum ColSlice<'a> {
    Numbers {
        present: [u64; 4],
        start: usize,
        values: &'a [f64],
    },
    Values(&'a [Value]),
    Mixed(&'a [Cell]),
}

impl<'a> ColSlice<'a> {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Self::Numbers { values, .. } => values.len(),
            Self::Values(vs) => vs.len(),
            Self::Mixed(cs) => cs.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn numbers_all_present(present: &[u64; 4], start: usize, len: usize) -> bool {
        NumberSlab::all_present_in(present, start, len)
    }

    #[inline]
    pub fn numbers_present_count(present: &[u64; 4], start: usize, len: usize) -> usize {
        NumberSlab::present_count_in(present, start, len)
    }
}

/// Lightweight zero-allocation borrowed view of a single occupied cell in any `ColSlab`.
#[derive(Clone, Copy, Debug)]
pub enum CellView<'a> {
    Number(f64),
    Value(&'a Value),
    Cell(&'a Cell),
}

impl<'a> CellView<'a> {
    #[inline]
    pub fn value(&self) -> Cow<'a, Value> {
        match *self {
            Self::Number(n) => Cow::Owned(Value::Number(n)),
            Self::Value(v) => Cow::Borrowed(v),
            Self::Cell(c) => Cow::Borrowed(&c.value),
        }
    }

    #[inline]
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::Number(n) => Some(n),
            Self::Value(Value::Number(n)) => Some(*n),
            Self::Cell(c) => match c.value {
                Value::Number(n) => Some(n),
                _ => None,
            },
            _ => None,
        }
    }

    #[inline]
    pub fn as_str(&self) -> Option<&'a str> {
        match *self {
            Self::Number(_) => None,
            Self::Value(Value::Text(s)) => Some(s.as_ref()),
            Self::Cell(c) => match &c.value {
                Value::Text(s) => Some(s.as_ref()),
                _ => None,
            },
            _ => None,
        }
    }

    #[inline]
    pub fn formula(&self) -> Option<&'a str> {
        match *self {
            Self::Cell(c) => c.formula.as_deref(),
            _ => None,
        }
    }

    #[inline]
    pub fn formula_arc(&self) -> Option<&'a Arc<str>> {
        match *self {
            Self::Cell(c) => c.formula.as_ref(),
            _ => None,
        }
    }

    #[inline]
    pub fn has_formula(&self) -> bool {
        matches!(self, Self::Cell(c) if c.formula.is_some())
    }

    #[inline]
    pub fn is_blank(&self) -> bool {
        match *self {
            Self::Number(_) => false,
            Self::Value(v) => matches!(v, Value::Empty),
            Self::Cell(c) => c.is_blank(),
        }
    }

    #[inline]
    pub fn to_cell(&self) -> Cell {
        match *self {
            Self::Number(n) => Cell {
                value: Value::Number(n),
                formula: None,
            },
            Self::Value(v) => Cell {
                value: v.clone(),
                formula: None,
            },
            Self::Cell(c) => c.clone(),
        }
    }

    #[inline]
    pub fn to_cow(&self) -> Cow<'a, Cell> {
        match *self {
            Self::Number(n) => Cow::Owned(Cell {
                value: Value::Number(n),
                formula: None,
            }),
            Self::Value(v) => Cow::Owned(Cell {
                value: v.clone(),
                formula: None,
            }),
            Self::Cell(c) => Cow::Borrowed(c),
        }
    }

    #[inline]
    pub fn same_stored_value(&self, other: &CellView<'_>) -> bool {
        match (*self, *other) {
            (CellView::Number(a), CellView::Number(b)) => a.to_bits() == b.to_bits(),
            (CellView::Number(a), CellView::Value(Value::Number(b))) => a.to_bits() == b.to_bits(),
            (CellView::Value(Value::Number(a)), CellView::Number(b)) => a.to_bits() == b.to_bits(),
            (CellView::Number(a), CellView::Cell(c)) => {
                matches!(c.value, Value::Number(b) if a.to_bits() == b.to_bits())
            }
            (CellView::Cell(c), CellView::Number(b)) => {
                matches!(c.value, Value::Number(a) if a.to_bits() == b.to_bits())
            }
            (CellView::Value(a), CellView::Value(b)) => a.same_stored_value(b),
            (CellView::Value(a), CellView::Cell(b)) => a.same_stored_value(&b.value),
            (CellView::Cell(a), CellView::Value(b)) => a.value.same_stored_value(b),
            (CellView::Cell(a), CellView::Cell(b)) => a.value.same_stored_value(&b.value),
            _ => false,
        }
    }

    #[inline]
    pub fn same_cell(&self, other: &CellView<'_>) -> bool {
        self.formula() == other.formula() && self.same_stored_value(other)
    }

    #[inline]
    pub fn same_input(&self, other: &CellView<'_>) -> bool {
        if self.has_formula() || other.has_formula() {
            return self.formula() == other.formula();
        }
        self.same_stored_value(other)
    }

    #[inline]
    pub fn eq_cell(&self, other: &Cell) -> bool {
        match *self {
            Self::Number(n) => other.formula.is_none() && other.value == Value::Number(n),
            Self::Value(v) => other.formula.is_none() && other.value == *v,
            Self::Cell(c) => c == other,
        }
    }

    #[inline]
    pub fn heap_bytes(&self) -> usize {
        match *self {
            Self::Number(_) => 0,
            Self::Value(v) => match v {
                Value::Text(s) => s.len(),
                _ => 0,
            },
            Self::Cell(c) => crate::undo::cell_heap_bytes(c),
        }
    }
}

impl ColSlab {
    #[inline]
    pub fn new_numbers() -> Self {
        Self::Numbers(NumberSlab::default())
    }

    #[inline]
    pub fn new_values() -> Self {
        Self::Values(Vec::new())
    }

    #[inline]
    pub fn new_mixed() -> Self {
        Self::Mixed(Vec::new())
    }

    #[inline]
    pub fn is_numbers(&self) -> bool {
        matches!(self, Self::Numbers(_))
    }

    #[inline]
    pub fn is_values(&self) -> bool {
        matches!(self, Self::Values(_))
    }

    #[inline]
    pub fn is_mixed(&self) -> bool {
        matches!(self, Self::Mixed(_))
    }

    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Self::Numbers(ns) => ns.values.len(),
            Self::Values(vs) => vs.len(),
            Self::Mixed(cs) => cs.len(),
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        match self {
            Self::Numbers(ns) => ns.values.capacity(),
            Self::Values(vs) => vs.capacity(),
            Self::Mixed(cs) => cs.capacity(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Numbers(ns) => ns.is_empty(),
            Self::Values(vs) => vs.is_empty(),
            Self::Mixed(cs) => cs.is_empty(),
        }
    }

    #[inline]
    pub fn payload_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match self {
                Self::Numbers(ns) => ns.values.capacity() * std::mem::size_of::<f64>(),
                Self::Values(vs) => vs.capacity() * std::mem::size_of::<Value>(),
                Self::Mixed(cs) => cs.capacity() * std::mem::size_of::<Cell>(),
            }
    }

    #[inline]
    pub fn contains(&self, ro: u32) -> bool {
        match self {
            Self::Numbers(ns) => ns.contains(ro),
            Self::Values(vs) => vs.get(ro as usize).is_some_and(|v| !v.is_empty()),
            Self::Mixed(cs) => cs.get(ro as usize).is_some_and(|c| !c.is_blank()),
        }
    }

    #[inline]
    pub fn get_view(&self, ro: u32) -> Option<CellView<'_>> {
        match self {
            Self::Numbers(ns) => ns.get(ro).map(CellView::Number),
            Self::Values(vs) => {
                let v = vs.get(ro as usize)?;
                (!v.is_empty()).then_some(CellView::Value(v))
            }
            Self::Mixed(cs) => {
                let c = cs.get(ro as usize)?;
                (!c.is_blank()).then_some(CellView::Cell(c))
            }
        }
    }

    #[inline]
    pub fn get_cow(&self, ro: u32) -> Option<Cow<'_, Cell>> {
        self.get_view(ro).map(|v| v.to_cow())
    }

    #[inline]
    pub fn get_f64(&self, ro: u32) -> Option<f64> {
        match self {
            Self::Numbers(ns) => ns.get(ro),
            Self::Values(vs) => match vs.get(ro as usize)? {
                Value::Number(n) => Some(*n),
                _ => None,
            },
            Self::Mixed(cs) => match &cs.get(ro as usize)?.value {
                Value::Number(n) => Some(*n),
                _ => None,
            },
        }
    }

    #[inline]
    pub fn get_f64_plain(&self, ro: u32) -> Option<f64> {
        match self {
            Self::Numbers(ns) => ns.get(ro),
            Self::Values(vs) => match vs.get(ro as usize)? {
                Value::Number(n) => Some(*n),
                _ => None,
            },
            Self::Mixed(cs) => {
                let c = cs.get(ro as usize)?;
                if c.formula.is_some() {
                    return None;
                }
                match c.value {
                    Value::Number(n) => Some(n),
                    _ => None,
                }
            }
        }
    }

    #[inline]
    pub fn value_ref(&self, ro: u32) -> Option<Cow<'_, Value>> {
        match self {
            Self::Numbers(ns) => ns.get(ro).map(|n| Cow::Owned(Value::Number(n))),
            Self::Values(vs) => {
                let v = vs.get(ro as usize)?;
                (!v.is_empty()).then_some(Cow::Borrowed(v))
            }
            Self::Mixed(cs) => {
                let c = cs.get(ro as usize)?;
                (!c.is_blank()).then_some(Cow::Borrowed(&c.value))
            }
        }
    }

    #[inline]
    pub fn value(&self, ro: u32) -> Value {
        match self {
            Self::Numbers(ns) => ns.get(ro).map_or(Value::Empty, Value::Number),
            Self::Values(vs) => vs.get(ro as usize).cloned().unwrap_or_default(),
            Self::Mixed(cs) => cs
                .get(ro as usize)
                .map(|c| c.value.clone())
                .unwrap_or_default(),
        }
    }

    #[inline]
    pub fn formula_of(&self, ro: u32) -> Option<&str> {
        match self {
            Self::Numbers(_) | Self::Values(_) => None,
            Self::Mixed(cs) => cs.get(ro as usize)?.formula.as_deref(),
        }
    }

    #[inline]
    pub fn formula_arc_of(&self, ro: u32) -> Option<&Arc<str>> {
        match self {
            Self::Numbers(_) | Self::Values(_) => None,
            Self::Mixed(cs) => cs.get(ro as usize)?.formula.as_ref(),
        }
    }

    #[inline]
    pub fn row_mask(&self) -> [u64; 4] {
        match self {
            Self::Numbers(ns) => ns.present,
            Self::Values(vs) => {
                let mut m = [0u64; 4];
                for (r, v) in vs.iter().enumerate() {
                    if !v.is_empty() {
                        m[r / 64] |= 1u64 << (r % 64);
                    }
                }
                m
            }
            Self::Mixed(cs) => {
                let mut m = [0u64; 4];
                for (r, c) in cs.iter().enumerate() {
                    if !c.is_blank() {
                        m[r / 64] |= 1u64 << (r % 64);
                    }
                }
                m
            }
        }
    }

    #[inline]
    pub fn row_mask_from(&self, begin: usize) -> [u64; 4] {
        match self {
            Self::Numbers(ns) => {
                if begin >= PAGE_ROWS as usize {
                    [0; 4]
                } else {
                    let m = row_range_mask(begin as u32, PAGE_ROWS - 1);
                    [
                        ns.present[0] & m[0],
                        ns.present[1] & m[1],
                        ns.present[2] & m[2],
                        ns.present[3] & m[3],
                    ]
                }
            }
            Self::Values(vs) => {
                let mut m = [0u64; 4];
                for (r, v) in vs.iter().enumerate().skip(begin) {
                    if !v.is_empty() {
                        m[r / 64] |= 1u64 << (r % 64);
                    }
                }
                m
            }
            Self::Mixed(cs) => {
                let mut m = [0u64; 4];
                for (r, c) in cs.iter().enumerate().skip(begin) {
                    if !c.is_blank() {
                        m[r / 64] |= 1u64 << (r % 64);
                    }
                }
                m
            }
        }
    }

    #[inline]
    pub fn occupied_count(&self) -> usize {
        match self {
            Self::Numbers(ns) => ns.occupied_count(),
            Self::Values(vs) => vs.iter().filter(|v| !v.is_empty()).count(),
            Self::Mixed(cs) => cs.iter().filter(|c| !c.is_blank()).count(),
        }
    }

    #[inline]
    pub fn first_last_occupied(&self) -> Option<(u32, u32)> {
        match self {
            Self::Numbers(ns) => ns.first_last_occupied(),
            Self::Values(vs) => {
                let first = vs.iter().position(|v| !v.is_empty())?;
                let last = vs.iter().rposition(|v| !v.is_empty()).unwrap();
                Some((first as u32, last as u32))
            }
            Self::Mixed(cs) => {
                let first = cs.iter().position(|c| !c.is_blank())?;
                let last = cs.iter().rposition(|c| !c.is_blank()).unwrap();
                Some((first as u32, last as u32))
            }
        }
    }

    #[inline]
    pub fn has_any_in(&self, lo: usize, hi_excl: usize) -> bool {
        match self {
            Self::Numbers(ns) => ns.has_any_in(lo, hi_excl),
            Self::Values(vs) => {
                let hi = hi_excl.min(vs.len());
                lo < hi && vs[lo..hi].iter().any(|v| !v.is_empty())
            }
            Self::Mixed(cs) => {
                let hi = hi_excl.min(cs.len());
                lo < hi && cs[lo..hi].iter().any(|c| !c.is_blank())
            }
        }
    }

    #[inline]
    pub fn next_occupied_from(&self, lo: usize) -> Option<u32> {
        match self {
            Self::Numbers(ns) => ns.next_occupied_from(lo),
            Self::Values(vs) => vs
                .iter()
                .enumerate()
                .skip(lo)
                .find_map(|(i, v)| (!v.is_empty()).then_some(i as u32)),
            Self::Mixed(cs) => cs
                .iter()
                .enumerate()
                .skip(lo)
                .find_map(|(i, c)| (!c.is_blank()).then_some(i as u32)),
        }
    }

    #[inline]
    pub fn prev_occupied_before(&self, hi_excl: usize) -> Option<u32> {
        match self {
            Self::Numbers(ns) => ns.prev_occupied_before(hi_excl),
            Self::Values(vs) => {
                let hi = hi_excl.min(vs.len());
                for i in (0..hi).rev() {
                    if !vs[i].is_empty() {
                        return Some(i as u32);
                    }
                }
                None
            }
            Self::Mixed(cs) => {
                let hi = hi_excl.min(cs.len());
                for i in (0..hi).rev() {
                    if !cs[i].is_blank() {
                        return Some(i as u32);
                    }
                }
                None
            }
        }
    }

    #[inline]
    pub fn as_slice(&self, start: usize, end: usize) -> ColSlice<'_> {
        let clamped_end = end.min(self.len());
        let clamped_start = start.min(clamped_end);
        match self {
            Self::Numbers(ns) => ColSlice::Numbers {
                present: ns.present,
                start: clamped_start,
                values: &ns.values[clamped_start..clamped_end],
            },
            Self::Values(vs) => ColSlice::Values(&vs[clamped_start..clamped_end]),
            Self::Mixed(cs) => ColSlice::Mixed(&cs[clamped_start..clamped_end]),
        }
    }

    pub fn slice_eq(
        &self,
        a_start: usize,
        a_end: usize,
        other: &ColSlab,
        b_start: usize,
        b_end: usize,
    ) -> bool {
        let len_a = a_end.saturating_sub(a_start);
        let len_b = b_end.saturating_sub(b_start);
        if len_a != len_b {
            return false;
        }
        match (self, other) {
            (Self::Numbers(na), Self::Numbers(nb)) if a_start == b_start => {
                if len_a == 0 {
                    return true;
                }
                let m = row_range_mask(a_start as u32, (a_end - 1) as u32);
                if (na.present[0] & m[0]) != (nb.present[0] & m[0])
                    || (na.present[1] & m[1]) != (nb.present[1] & m[1])
                    || (na.present[2] & m[2]) != (nb.present[2] & m[2])
                    || (na.present[3] & m[3]) != (nb.present[3] & m[3])
                {
                    return false;
                }
                let sa = &na.values[a_start..a_end.min(na.values.len())];
                let sb = &nb.values[b_start..b_end.min(nb.values.len())];
                sa == sb
            }
            (Self::Values(va), Self::Values(vb)) => {
                va.get(a_start..a_end) == vb.get(b_start..b_end)
            }
            (Self::Mixed(ca), Self::Mixed(cb)) => {
                ca.get(a_start..a_end) == cb.get(b_start..b_end)
            }
            _ => {
                for i in 0..len_a {
                    let va = self.get_view((a_start + i) as u32);
                    let vb = other.get_view((b_start + i) as u32);
                    let same = match (va, vb) {
                        (None, None) => true,
                        (Some(x), Some(y)) => x.formula() == y.formula() && x.value() == y.value(),
                        _ => false,
                    };
                    if !same {
                        return false;
                    }
                }
                true
            }
        }
    }

    pub fn promote_to_values(&mut self) -> &mut Vec<Value> {
        match self {
            Self::Numbers(ns) => {
                let len = ns.values.len();
                let mut vals = Vec::with_capacity(ns.values.capacity().min(PAGE_ROWS as usize));
                for r in 0..len {
                    if (ns.present[r / 64] & (1u64 << (r % 64))) != 0 {
                        vals.push(Value::Number(ns.values[r]));
                    } else {
                        vals.push(Value::Empty);
                    }
                }
                *self = Self::Values(vals);
                match self {
                    Self::Values(v) => v,
                    _ => unreachable!(),
                }
            }
            Self::Values(v) => v,
            Self::Mixed(_) => unreachable!("cannot demote Mixed via promote_to_values"),
        }
    }

    pub fn promote_to_mixed(&mut self) -> &mut Vec<Cell> {
        match self {
            Self::Numbers(ns) => {
                let len = ns.values.len();
                let mut cells = Vec::with_capacity(ns.values.capacity().min(PAGE_ROWS as usize));
                for r in 0..len {
                    if (ns.present[r / 64] & (1u64 << (r % 64))) != 0 {
                        cells.push(Cell {
                            value: Value::Number(ns.values[r]),
                            formula: None,
                        });
                    } else {
                        cells.push(Cell::default());
                    }
                }
                *self = Self::Mixed(cells);
            }
            Self::Values(vs) => {
                let owned = std::mem::take(vs);
                let cells: Vec<Cell> = owned
                    .into_iter()
                    .map(|value| Cell {
                        value,
                        formula: None,
                    })
                    .collect();
                *self = Self::Mixed(cells);
            }
            Self::Mixed(_) => {}
        }
        match self {
            Self::Mixed(c) => c,
            _ => unreachable!(),
        }
    }

    /// Write a non-blank `cell` at row offset `ro`, upgrading the slab in place if necessary.
    /// Returns `(existed, old_has_formula)`.
    #[inline]
    pub fn set(&mut self, ro: u32, cell: Cell) -> (bool, bool) {
        debug_assert!(!cell.is_blank() && ro < PAGE_ROWS);
        let idx = ro as usize;
        if cell.formula.is_none() {
            if let Value::Number(n) = cell.value {
                match self {
                    Self::Numbers(ns) => return (ns.set(ro, n), false),
                    Self::Values(vs) => {
                        let existed = vs.get(idx).is_some_and(|v| !v.is_empty());
                        grow_slab_vec(vs, idx + 1, Value::Empty);
                        vs[idx] = Value::Number(n);
                        return (existed, false);
                    }
                    Self::Mixed(cs) => {
                        let (existed, old_f) = cs
                            .get(idx)
                            .filter(|c| !c.is_blank())
                            .map_or((false, false), |c| (true, c.formula.is_some()));
                        grow_slab_vec(cs, idx + 1, Cell::default());
                        cs[idx] = cell;
                        return (existed, old_f);
                    }
                }
            }
            match self {
                Self::Numbers(_) => {
                    let vs = self.promote_to_values();
                    let existed = vs.get(idx).is_some_and(|v| !v.is_empty());
                    grow_slab_vec(vs, idx + 1, Value::Empty);
                    vs[idx] = cell.value;
                    (existed, false)
                }
                Self::Values(vs) => {
                    let existed = vs.get(idx).is_some_and(|v| !v.is_empty());
                    grow_slab_vec(vs, idx + 1, Value::Empty);
                    vs[idx] = cell.value;
                    (existed, false)
                }
                Self::Mixed(cs) => {
                    let (existed, old_f) = cs
                        .get(idx)
                        .filter(|c| !c.is_blank())
                        .map_or((false, false), |c| (true, c.formula.is_some()));
                    grow_slab_vec(cs, idx + 1, Cell::default());
                    cs[idx] = cell;
                    (existed, old_f)
                }
            }
        } else {
            let cs = self.promote_to_mixed();
            let (existed, old_f) = cs
                .get(idx)
                .filter(|c| !c.is_blank())
                .map_or((false, false), |c| (true, c.formula.is_some()));
            grow_slab_vec(cs, idx + 1, Cell::default());
            cs[idx] = cell;
            (existed, old_f)
        }
    }

    #[inline]
    pub fn set_formula_cache(&mut self, ro: u32, value: Value) -> bool {
        let idx = ro as usize;
        match self {
            Self::Numbers(ns) => match value {
                Value::Empty => {
                    ns.remove(ro);
                    true
                }
                Value::Number(n) => {
                    ns.set(ro, n);
                    false
                }
                other => {
                    let vs = self.promote_to_values();
                    grow_slab_vec(vs, idx + 1, Value::Empty);
                    vs[idx] = other;
                    false
                }
            },
            Self::Values(vs) => {
                if let Some(slot) = vs.get_mut(idx) {
                    *slot = value;
                    let blank = slot.is_empty();
                    if blank {
                        while vs.last().is_some_and(Value::is_empty) {
                            vs.pop();
                        }
                    }
                    blank
                } else {
                    false
                }
            }
            Self::Mixed(cs) => {
                if let Some(slot) = cs.get_mut(idx) {
                    slot.value = value;
                    slot.is_blank()
                } else {
                    false
                }
            }
        }
    }

    pub fn remove(&mut self, ro: u32) -> Option<Cell> {
        let idx = ro as usize;
        match self {
            Self::Numbers(ns) => ns.remove(ro).map(|n| Cell {
                value: Value::Number(n),
                formula: None,
            }),
            Self::Values(vs) => {
                let slot = vs.get_mut(idx)?;
                if slot.is_empty() {
                    return None;
                }
                let old = std::mem::take(slot);
                while vs.last().is_some_and(Value::is_empty) {
                    vs.pop();
                }
                Some(Cell {
                    value: old,
                    formula: None,
                })
            }
            Self::Mixed(cs) => {
                let slot = cs.get_mut(idx)?;
                if slot.is_blank() {
                    return None;
                }
                let old = std::mem::take(slot);
                while cs.last().is_some_and(Cell::is_blank) {
                    cs.pop();
                }
                Some(old)
            }
        }
    }

    pub fn clear_range(&mut self, lo: usize, hi_incl: usize) -> (usize, [u64; 4]) {
        match self {
            Self::Numbers(ns) => ns.clear_range(lo, hi_incl),
            Self::Values(vs) => {
                let end = (hi_incl + 1).min(vs.len());
                if lo >= end {
                    return (0, [0; 4]);
                }
                let mut removed = 0usize;
                let mut mask = [0u64; 4];
                for r in lo..end {
                    if !vs[r].is_empty() {
                        vs[r] = Value::Empty;
                        mask[r / 64] |= 1u64 << (r % 64);
                        removed += 1;
                    }
                }
                while vs.last().is_some_and(Value::is_empty) {
                    vs.pop();
                }
                (removed, mask)
            }
            Self::Mixed(cs) => {
                let end = (hi_incl + 1).min(cs.len());
                if lo >= end {
                    return (0, [0; 4]);
                }
                let mut removed = 0usize;
                let mut mask = [0u64; 4];
                for r in lo..end {
                    if !cs[r].is_blank() {
                        cs[r] = Cell::default();
                        mask[r / 64] |= 1u64 << (r % 64);
                        removed += 1;
                    }
                }
                while cs.last().is_some_and(Cell::is_blank) {
                    cs.pop();
                }
                (removed, mask)
            }
        }
    }

    /// Extract rows `lo..len` into a same-typed tail slab, clearing them from `self`.
    pub fn take_tail(&mut self, lo: usize) -> Option<(ColSlab, [u64; 4])> {
        if self.len() <= lo {
            return None;
        }
        match self {
            Self::Numbers(ns) => {
                let m = row_range_mask(lo as u32, (ns.values.len() - 1) as u32);
                let tail_present = [
                    ns.present[0] & m[0],
                    ns.present[1] & m[1],
                    ns.present[2] & m[2],
                    ns.present[3] & m[3],
                ];
                if tail_present == [0; 4] {
                    return None;
                }
                let tail_len = highest_bit_plus_one(&tail_present);
                let mut tail_vals = vec![0.0; tail_len];
                tail_vals[lo..tail_len].copy_from_slice(&ns.values[lo..tail_len]);
                for w in 0..4 {
                    ns.present[w] &= !m[w];
                }
                ns.values[lo..].fill(0.0);
                ns.values.truncate(highest_bit_plus_one(&ns.present));
                Some((
                    Self::Numbers(NumberSlab {
                        present: tail_present,
                        values: tail_vals,
                    }),
                    tail_present,
                ))
            }
            Self::Values(vs) => {
                let mut tail = vec![Value::Empty; vs.len()];
                let mut bits = [0u64; 4];
                let mut any = false;
                for r in lo..vs.len() {
                    if !vs[r].is_empty() {
                        bits[r / 64] |= 1u64 << (r % 64);
                        tail[r] = std::mem::take(&mut vs[r]);
                        any = true;
                    }
                }
                while vs.last().is_some_and(Value::is_empty) {
                    vs.pop();
                }
                while tail.last().is_some_and(Value::is_empty) {
                    tail.pop();
                }
                any.then_some((Self::Values(tail), bits))
            }
            Self::Mixed(cs) => {
                let mut tail = vec![Cell::default(); cs.len()];
                let mut bits = [0u64; 4];
                let mut any = false;
                for r in lo..cs.len() {
                    if !cs[r].is_blank() {
                        bits[r / 64] |= 1u64 << (r % 64);
                        tail[r] = std::mem::take(&mut cs[r]);
                        any = true;
                    }
                }
                while cs.last().is_some_and(Cell::is_blank) {
                    cs.pop();
                }
                while tail.last().is_some_and(Cell::is_blank) {
                    tail.pop();
                }
                any.then_some((Self::Mixed(tail), bits))
            }
        }
    }

    pub fn new_empty_like(&self) -> Self {
        match self {
            Self::Numbers(_) => Self::new_numbers(),
            Self::Values(_) => Self::new_values(),
            Self::Mixed(_) => Self::new_mixed(),
        }
    }

    /// Move entries in `src_start..src_end` out of `self` (leaving them blank in `self`)
    /// into `dst` starting at `dst_off`, preserving compact types whenever possible.
    pub fn move_range_into(
        &mut self,
        src_start: usize,
        src_end: usize,
        dst: &mut ColSlab,
        dst_off: usize,
    ) {
        let end = src_end.min(self.len());
        if src_start >= end {
            return;
        }
        let span = end - src_start;
        match (self, dst) {
            (Self::Numbers(sa), Self::Numbers(db)) => {
                let m = row_range_mask(src_start as u32, (end - 1) as u32);
                let mut has_any = false;
                let mut max_dst = 0usize;
                for r in src_start..end {
                    if (sa.present[r / 64] & (1u64 << (r % 64))) != 0 {
                        has_any = true;
                        max_dst = dst_off + (r - src_start);
                    }
                }
                if !has_any {
                    return;
                }
                if db.values.len() <= max_dst {
                    grow_slab_vec(&mut db.values, max_dst + 1, 0.0);
                }
                for i in 0..span {
                    let sr = src_start + i;
                    let sw = sr / 64;
                    let sb = 1u64 << (sr % 64);
                    if (sa.present[sw] & sb) != 0 {
                        let dr = dst_off + i;
                        db.values[dr] = sa.values[sr];
                        db.present[dr / 64] |= 1u64 << (dr % 64);
                        sa.values[sr] = 0.0;
                    }
                }
                for w in 0..4 {
                    sa.present[w] &= !m[w];
                }
            }
            (Self::Numbers(sa), Self::Values(db)) => {
                let m = row_range_mask(src_start as u32, (end - 1) as u32);
                let mut max_dst = None;
                for r in src_start..end {
                    if (sa.present[r / 64] & (1u64 << (r % 64))) != 0 {
                        max_dst = Some(dst_off + (r - src_start));
                    }
                }
                let Some(max_dst) = max_dst else { return };
                grow_slab_vec(db, max_dst + 1, Value::Empty);
                for i in 0..span {
                    let sr = src_start + i;
                    if (sa.present[sr / 64] & (1u64 << (sr % 64))) != 0 {
                        db[dst_off + i] = Value::Number(sa.values[sr]);
                        sa.values[sr] = 0.0;
                    }
                }
                for w in 0..4 {
                    sa.present[w] &= !m[w];
                }
            }
            (Self::Values(sa), dst_slab @ Self::Numbers(_)) => {
                let db = dst_slab.promote_to_values();
                let mut max_dst = None;
                for r in src_start..end {
                    if !sa[r].is_empty() {
                        max_dst = Some(dst_off + (r - src_start));
                    }
                }
                let Some(max_dst) = max_dst else { return };
                grow_slab_vec(db, max_dst + 1, Value::Empty);
                for i in 0..span {
                    let sr = src_start + i;
                    if !sa[sr].is_empty() {
                        db[dst_off + i] = std::mem::take(&mut sa[sr]);
                    }
                }
            }
            (Self::Values(sa), Self::Values(db)) => {
                let mut max_dst = None;
                for r in src_start..end {
                    if !sa[r].is_empty() {
                        max_dst = Some(dst_off + (r - src_start));
                    }
                }
                let Some(max_dst) = max_dst else { return };
                grow_slab_vec(db, max_dst + 1, Value::Empty);
                for i in 0..span {
                    let sr = src_start + i;
                    if !sa[sr].is_empty() {
                        db[dst_off + i] = std::mem::take(&mut sa[sr]);
                    }
                }
            }
            (src_slab, dst_slab) => {
                let db = dst_slab.promote_to_mixed();
                match src_slab {
                    Self::Numbers(sa) => {
                        let m = row_range_mask(src_start as u32, (end - 1) as u32);
                        let mut max_dst = None;
                        for r in src_start..end {
                            if (sa.present[r / 64] & (1u64 << (r % 64))) != 0 {
                                max_dst = Some(dst_off + (r - src_start));
                            }
                        }
                        let Some(max_dst) = max_dst else { return };
                        grow_slab_vec(db, max_dst + 1, Cell::default());
                        for i in 0..span {
                            let sr = src_start + i;
                            if (sa.present[sr / 64] & (1u64 << (sr % 64))) != 0 {
                                db[dst_off + i] = Cell {
                                    value: Value::Number(sa.values[sr]),
                                    formula: None,
                                };
                                sa.values[sr] = 0.0;
                            }
                        }
                        for w in 0..4 {
                            sa.present[w] &= !m[w];
                        }
                    }
                    Self::Values(sa) => {
                        let mut max_dst = None;
                        for r in src_start..end {
                            if !sa[r].is_empty() {
                                max_dst = Some(dst_off + (r - src_start));
                            }
                        }
                        let Some(max_dst) = max_dst else { return };
                        grow_slab_vec(db, max_dst + 1, Cell::default());
                        for i in 0..span {
                            let sr = src_start + i;
                            if !sa[sr].is_empty() {
                                db[dst_off + i] = Cell {
                                    value: std::mem::take(&mut sa[sr]),
                                    formula: None,
                                };
                            }
                        }
                    }
                    Self::Mixed(sa) => {
                        let mut max_dst = None;
                        for r in src_start..end {
                            if !sa[r].is_blank() {
                                max_dst = Some(dst_off + (r - src_start));
                            }
                        }
                        let Some(max_dst) = max_dst else { return };
                        grow_slab_vec(db, max_dst + 1, Cell::default());
                        for i in 0..span {
                            let sr = src_start + i;
                            if !sa[sr].is_blank() {
                                db[dst_off + i] = std::mem::take(&mut sa[sr]);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Recompute row mask and max row (+1) for this slab; returns `None` if completely empty.
    pub fn scan_mask(&self) -> Option<([u64; 4], u32)> {
        match self {
            Self::Numbers(ns) => {
                if ns.is_empty() {
                    None
                } else {
                    Some((ns.present, highest_bit_plus_one(&ns.present) as u32))
                }
            }
            Self::Values(vs) => {
                let mut m = [0u64; 4];
                let mut max_r = 0u32;
                for (r, v) in vs.iter().enumerate() {
                    if !v.is_empty() {
                        m[r / 64] |= 1u64 << (r % 64);
                        max_r = r as u32 + 1;
                    }
                }
                (max_r > 0).then_some((m, max_r))
            }
            Self::Mixed(cs) => {
                let mut m = [0u64; 4];
                let mut max_r = 0u32;
                for (r, c) in cs.iter().enumerate() {
                    if !c.is_blank() {
                        m[r / 64] |= 1u64 << (r % 64);
                        max_r = r as u32 + 1;
                    }
                }
                (max_r > 0).then_some((m, max_r))
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Page {
    /// Unrestricted cell mutation may introduce a qualified reference. Merely
    /// writing a formula's cached result does not invalidate this fast path.
    pub(crate) may_have_qualified_formula: bool,
    pub(crate) may_have_formula: bool,
    /// 本页已触及的最大行数（≤ PAGE_ROWS）
    pub(crate) n_rows: u32,
    /// 本页非空行的超集位图（256 行 = 4 × u64），供 `iter_rect` 一步跳过空行。
    pub(crate) row_mask: [u64; 4],
    /// 紧凑页列目录（低列宽 `<256` 定长直达，远端列自动切换 64 列块位图 + `POPCNT` rank）。
    pub(crate) cols: PageCols<ColSlab>,
}

impl Page {
    #[inline]
    pub(crate) fn has_col(&self, c: u32) -> bool {
        self.cols.has_col(c)
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.cols.is_empty()
    }

    #[allow(dead_code)]
    #[inline]
    pub(crate) fn get_col(&self, c: u32) -> Option<&Arc<ColSlab>> {
        self.cols.get(c)
    }

    pub(crate) fn rebuild_masks(&mut self) {
        self.row_mask = [0; 4];
        self.n_rows = 0;
        let mut any_mixed = false;
        let row_mask = &mut self.row_mask;
        let n_rows = &mut self.n_rows;
        self.cols.retain_mut(|_c, arc| {
            if let Some((m, max_r)) = arc.scan_mask() {
                for w in 0..4 {
                    row_mask[w] |= m[w];
                }
                *n_rows = (*n_rows).max(max_r);
                if arc.is_mixed() {
                    any_mixed = true;
                }
                true
            } else {
                false
            }
        });
        if !any_mixed {
            self.may_have_formula = false;
            self.may_have_qualified_formula = false;
        }
    }

    #[inline]
    fn set(&mut self, r_off: u32, c: u32, cell: Cell) -> (bool, bool) {
        self.row_mask[(r_off / 64) as usize] |= 1u64 << (r_off % 64);
        if r_off >= self.n_rows {
            self.n_rows = r_off + 1;
        }
        let slot = if cell.formula.is_some() {
            self.cols
                .get_or_insert_with(c, || Arc::new(ColSlab::new_mixed()))
        } else if matches!(cell.value, Value::Number(_)) {
            self.cols
                .get_or_insert_with(c, || Arc::new(ColSlab::new_numbers()))
        } else {
            self.cols
                .get_or_insert_with(c, || Arc::new(ColSlab::new_values()))
        };
        Arc::make_mut(slot).set(r_off, cell)
    }

    #[inline]
    pub(crate) fn get_view(&self, r_off: u32, c: u32) -> Option<CellView<'_>> {
        self.cols.get(c)?.get_view(r_off)
    }

    #[inline]
    pub(crate) fn contains(&self, r_off: u32, c: u32) -> bool {
        self.cols.get(c).is_some_and(|s| s.contains(r_off))
    }

    #[inline]
    fn get_mut_mixed(&mut self, r_off: u32, c: u32) -> Option<&mut Cell> {
        let slot = self.cols.get_mut(c)?;
        if !slot.contains(r_off) {
            return None;
        }
        let vec = Arc::make_mut(slot).promote_to_mixed();
        vec.get_mut(r_off as usize)
    }

    /// Iterate non-empty column indices in `[c1..=c2]` in ascending order via bit-scan.
    #[inline]
    pub(crate) fn iter_nonempty_cols(&self, c1: u32, c2: u32) -> PageColIdxIter<'_, ColSlab> {
        self.cols.iter_nonempty_cols(c1, c2)
    }
}

/// Snapshots share an identity until a writer forks. The counter alone can
/// coincide on independently edited copies (for example after undo).
#[derive(Clone, Debug, Default)]
pub struct CellVersion {
    lineage: Arc<()>,
    revision: u64,
}

impl PartialEq for CellVersion {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision && Arc::ptr_eq(&self.lineage, &other.lineage)
    }
}

impl CellVersion {
    fn advance(&mut self) {
        if Arc::strong_count(&self.lineage) > 1 {
            self.lineage = Arc::new(());
        }
        self.revision = self.revision.wrapping_add(1);
    }
}

pub(crate) struct GatheredSlab {
    pub(crate) cells: Arc<ColSlab>,
    pub(crate) row_mask: [u64; 4],
}

pub(crate) struct GatheredColumn {
    pub(crate) col: u32,
    pub(crate) slabs: Vec<Option<GatheredSlab>>,
    pub(crate) len_delta: isize,
    pub(crate) has_formula: bool,
    pub(crate) has_qual_formula: bool,
    pub(crate) vacated_masks: Vec<(u32, [u64; 4])>,
    pub(crate) occupied_masks: Vec<(u32, [u64; 4])>,
}

#[derive(Clone, Debug)]
pub struct Store {
    pages: Vec<Option<Arc<Page>>>,
    page_bits: Vec<u64>,
    far_pages: Option<Box<BTreeMap<u32, Arc<Page>>>>,
    pub(crate) version: CellVersion,
    len: usize,
    formula_count: OnceLock<usize>,
    // Valid for this snapshot. Removing a boundary cell invalidates it until the next read.
    bounds: OnceLock<Option<(u32, u32, u32, u32)>>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            pages: Vec::new(),
            page_bits: Vec::new(),
            far_pages: None,
            version: CellVersion::default(),
            len: 0,
            formula_count: OnceLock::from(0usize),
            bounds: OnceLock::new(),
        }
    }
}

impl Store {
    #[inline]
    pub(crate) fn page(&self, pi: u32) -> Option<&Arc<Page>> {
        if pi < DIRECT_PAGE_LIMIT {
            self.pages.get(pi as usize)?.as_ref()
        } else {
            self.far_pages.as_ref()?.get(&pi)
        }
    }

    #[inline]
    fn set_page_bit(&mut self, pi: u32) {
        if pi < DIRECT_PAGE_LIMIT {
            let w = (pi / 64) as usize;
            if self.page_bits.len() <= w {
                self.page_bits.resize(w + 1, 0);
            }
            self.page_bits[w] |= 1u64 << (pi % 64);
        }
    }

    #[inline]
    fn clear_page_bit(&mut self, pi: u32) {
        if pi < DIRECT_PAGE_LIMIT {
            let w = (pi / 64) as usize;
            if let Some(word) = self.page_bits.get_mut(w) {
                *word &= !(1u64 << (pi % 64));
                while self.page_bits.last() == Some(&0) {
                    self.page_bits.pop();
                }
            }
        }
    }

    #[inline]
    fn page_mut_or_insert(&mut self, pi: u32) -> &mut Page {
        if pi < DIRECT_PAGE_LIMIT {
            let idx = pi as usize;
            if self.pages.len() <= idx {
                self.pages.resize(idx + 1, None);
            }
            self.set_page_bit(pi);
            let arc = self.pages[idx].get_or_insert_with(|| Arc::new(Page::default()));
            Arc::make_mut(arc)
        } else {
            let map = self.far_pages.get_or_insert_with(|| Box::new(BTreeMap::new()));
            let arc = map.entry(pi).or_insert_with(|| Arc::new(Page::default()));
            Arc::make_mut(arc)
        }
    }

    #[inline]
    fn page_mut_existing(&mut self, pi: u32) -> Option<&mut Page> {
        if pi < DIRECT_PAGE_LIMIT {
            let arc = self.pages.get_mut(pi as usize)?.as_mut()?;
            Some(Arc::make_mut(arc))
        } else {
            let arc = self.far_pages.as_mut()?.get_mut(&pi)?;
            Some(Arc::make_mut(arc))
        }
    }

    #[inline]
    fn insert_page_arc(&mut self, pi: u32, page: Arc<Page>) {
        if page.is_empty() {
            self.remove_page(pi);
            return;
        }
        if pi < DIRECT_PAGE_LIMIT {
            let idx = pi as usize;
            if self.pages.len() <= idx {
                self.pages.resize(idx + 1, None);
            }
            self.pages[idx] = Some(page);
            self.set_page_bit(pi);
        } else {
            self.far_pages
                .get_or_insert_with(|| Box::new(BTreeMap::new()))
                .insert(pi, page);
        }
    }

    #[inline]
    fn remove_page(&mut self, pi: u32) {
        if pi < DIRECT_PAGE_LIMIT {
            if let Some(slot) = self.pages.get_mut(pi as usize) {
                *slot = None;
            }
            self.clear_page_bit(pi);
        } else if let Some(map) = self.far_pages.as_mut() {
            map.remove(&pi);
            if map.is_empty() {
                self.far_pages = None;
            }
        }
    }

    /// Iterate `(pi, &Arc<Page>)` in strictly ascending page order within `[p1..=p2]`
    /// with ZERO heap allocation and ZERO `sort_unstable()`.
    pub(crate) fn iter_pages_in(
        &self,
        p1: u32,
        p2: u32,
    ) -> impl Iterator<Item = (u32, &Arc<Page>)> + '_ {
        let direct_hi = p2.min(DIRECT_PAGE_LIMIT - 1);
        let total_words = self.page_bits.len();
        let start_w = (p1 / 64) as usize;
        let end_w = (direct_hi / 64) as usize;
        let direct_range = if p1 <= direct_hi && start_w < total_words {
            start_w..=end_w.min(total_words - 1)
        } else {
            1..=0
        };
        let direct = direct_range.flat_map(move |w| {
            let mut bits = self.page_bits[w];
            if w == start_w {
                bits &= !0u64 << (p1 % 64);
            }
            if w == end_w {
                let hi = direct_hi % 64;
                if hi < 63 {
                    bits &= (1u64 << (hi + 1)) - 1;
                }
            }
            std::iter::from_fn(move || {
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    bits &= bits - 1;
                    let pi = w as u32 * 64 + tz;
                    if let Some(Some(page)) = self.pages.get(pi as usize) {
                        return Some((pi, page));
                    }
                }
                None
            })
        });
        let far = self
            .far_pages
            .as_deref()
            .filter(move |_| p1 <= p2 && p2 >= DIRECT_PAGE_LIMIT)
            .into_iter()
            .flat_map(move |map| {
                map.range(p1.max(DIRECT_PAGE_LIMIT)..=p2)
                    .map(|(&pi, page)| (pi, page))
            });
        direct.chain(far)
    }

    pub(crate) fn iter_pages(&self) -> impl Iterator<Item = (u32, &Arc<Page>)> + '_ {
        self.iter_pages_in(0, u32::MAX)
    }

    pub fn page_count(&self) -> usize {
        let direct: usize = self.page_bits.iter().map(|w| w.count_ones() as usize).sum();
        direct + self.far_pages.as_ref().map_or(0, |m| m.len())
    }

    pub(crate) fn same_page(&self, other: &Self, pi: u32) -> bool {
        match (self.page(pi), other.page(pi)) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }

    /// Compare only changed column slabs inside a query's rectangles. A COW
    /// fork in a different row of the same slab still leaves this range valid.
    pub(crate) fn same_cells_in(&self, other: &Self, rects: &[(u32, u32, u32, u32)], formulas: bool) -> bool {
        if self.version == other.version {
            return true;
        }
        let same_rect = |&(r1, c1, r2, c2): &(u32, u32, u32, u32)| {
            if r1 > r2 || c1 > c2 {
                return true;
            }
            let same_page = |p: u32| {
                if p < r1 / PAGE_ROWS || p > r2 / PAGE_ROWS {
                    return true;
                }
                let pa = self.page(p);
                let pb = other.page(p);
                if let (Some(xa), Some(xb)) = (pa, pb)
                    && Arc::ptr_eq(xa, xb)
                {
                    return true;
                }
                if pa.is_none() && pb.is_none() {
                    return true;
                }
                let base = p * PAGE_ROWS;
                let row_start = r1.saturating_sub(base) as usize;
                let row_end = (r2 - base).saturating_add(1).min(PAGE_ROWS) as usize;
                PageCols::all_union_cols_in(
                    pa.map(|p| &p.cols),
                    pb.map(|p| &p.cols),
                    c1,
                    c2,
                    |_col, ca, cb| {
                        match (ca, cb) {
                            (None, None) => return true,
                            (Some(x), Some(y)) if Arc::ptr_eq(x, y) => return true,
                            _ => {}
                        }
                        if formulas
                            && ca.is_none_or(|s| !s.is_mixed())
                            && cb.is_none_or(|s| !s.is_mixed())
                        {
                            return true;
                        }
                        let max_len = ca.map_or(0, |s| s.len()).max(cb.map_or(0, |s| s.len()));
                        let end = row_end.min(max_len);
                        if row_start >= end {
                            return true;
                        }
                        if !formulas
                            && let (Some(sa), Some(sb)) = (ca, cb)
                            && let (ColSlab::Numbers(na), ColSlab::Numbers(nb)) =
                                (sa.as_ref(), sb.as_ref())
                        {
                            let m = row_range_mask(row_start as u32, (end - 1) as u32);
                            if (na.present[0] & m[0]) != (nb.present[0] & m[0])
                                || (na.present[1] & m[1]) != (nb.present[1] & m[1])
                                || (na.present[2] & m[2]) != (nb.present[2] & m[2])
                                || (na.present[3] & m[3]) != (nb.present[3] & m[3])
                            {
                                return false;
                            }
                            let end_v = end.min(na.values.len()).min(nb.values.len());
                            for i in row_start..end_v {
                                if (na.present[i / 64] & (1u64 << (i % 64))) != 0
                                    && na.values[i].to_bits() != nb.values[i].to_bits()
                                {
                                    return false;
                                }
                            }
                            return true;
                        }
                        for i in row_start..end {
                            let xa = ca.and_then(|s| s.get_view(i as u32));
                            let xb = cb.and_then(|s| s.get_view(i as u32));
                            if formulas {
                                if xa.and_then(|v| v.formula()) != xb.and_then(|v| v.formula()) {
                                    return false;
                                }
                            } else {
                                match (xa, xb) {
                                    (Some(va), Some(vb)) => {
                                        if !va.same_stored_value(&vb) {
                                            return false;
                                        }
                                    }
                                    (None, None) => {}
                                    _ => return false,
                                }
                            }
                        }
                        true
                    },
                )
            };
            let first = r1 / PAGE_ROWS;
            let last = r2 / PAGE_ROWS;
            let total_pages = self.page_count() + other.page_count();
            if (last - first) as usize <= total_pages {
                return (first..=last).all(same_page);
            }
            self.iter_pages_in(first, last).all(|(p, _)| same_page(p))
                && other
                    .iter_pages_in(first, last)
                    .filter(|&(p, _)| self.page(p).is_none())
                    .all(|(p, _)| same_page(p))
        };
        if rects.is_empty() {
            same_rect(&(0, 0, u32::MAX, u32::MAX))
        } else {
            rects.iter().all(same_rect)
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.version.revision
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// 诊断用：返回 (页数, 实际已分配列块数, 这些槽位里的容量总数)。
    pub fn stats(&self) -> (usize, usize, usize) {
        let mut pages = 0usize;
        let mut slabs = 0usize;
        let mut slots = 0usize;
        for (_, p) in self.iter_pages() {
            pages += 1;
            for (_, col) in p.cols.iter_entries() {
                slabs += 1;
                slots += col.capacity();
            }
        }
        (pages, slabs, slots)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.version.advance();
        self.pages.clear();
        self.page_bits.clear();
        self.far_pages = None;
        self.len = 0;
        self.formula_count = OnceLock::from(0usize);
        self.bounds.take();
    }

    #[inline]
    fn split(r: u32) -> (u32, u32) {
        (r / PAGE_ROWS, r % PAGE_ROWS)
    }

    #[inline]
    pub fn get(&self, r: u32, c: u32) -> Option<Cow<'_, Cell>> {
        let (pi, ro) = Self::split(r);
        self.page(pi)?.get_view(ro, c).map(|v| v.to_cow())
    }

    #[inline]
    pub fn get_view(&self, r: u32, c: u32) -> Option<CellView<'_>> {
        let (pi, ro) = Self::split(r);
        self.page(pi)?.get_view(ro, c)
    }

    #[inline]
    pub fn get_f64(&self, r: u32, c: u32) -> Option<f64> {
        let (pi, ro) = Self::split(r);
        self.page(pi)?.cols.get(c)?.get_f64(ro)
    }

    #[inline]
    pub(crate) fn get_f64_plain(&self, r: u32, c: u32) -> Option<f64> {
        let (pi, ro) = Self::split(r);
        self.page(pi)?.cols.get(c)?.get_f64_plain(ro)
    }

    #[inline]
    pub fn value_ref(&self, r: u32, c: u32) -> Cow<'_, Value> {
        static EMPTY: Value = Value::Empty;
        let (pi, ro) = Self::split(r);
        self.page(pi)
            .and_then(|p| p.cols.get(c))
            .and_then(|s| s.value_ref(ro))
            .unwrap_or(Cow::Borrowed(&EMPTY))
    }

    #[inline]
    pub fn value(&self, r: u32, c: u32) -> Value {
        let (pi, ro) = Self::split(r);
        self.page(pi)
            .and_then(|p| p.cols.get(c))
            .map_or(Value::Empty, |s| s.value(ro))
    }

    #[inline]
    pub fn formula_of(&self, r: u32, c: u32) -> Option<&str> {
        let (pi, ro) = Self::split(r);
        self.page(pi)?.cols.get(c)?.formula_of(ro)
    }

    #[inline]
    pub(crate) fn formula_arc_of(&self, r: u32, c: u32) -> Option<&Arc<str>> {
        let (pi, ro) = Self::split(r);
        self.page(pi)?.cols.get(c)?.formula_arc_of(ro)
    }

    pub fn get_mut(&mut self, r: u32, c: u32) -> Option<&mut Cell> {
        let (pi, ro) = Self::split(r);
        if !self.page(pi).is_some_and(|p| p.contains(ro, c)) {
            return None;
        }
        // A caller holding &mut Cell may mutate formula or turn a boundary cell into blank.
        self.formula_count.take();
        Self::invalidate_boundary(&mut self.bounds, r, c);
        self.version.advance();
        let page = self.page_mut_existing(pi)?;
        page.may_have_qualified_formula = true;
        page.may_have_formula = true;
        page.get_mut_mixed(ro, c)
    }

    #[inline]
    pub fn contains(&self, r: u32, c: u32) -> bool {
        let (pi, ro) = Self::split(r);
        self.page(pi).is_some_and(|p| p.contains(ro, c))
    }

    pub(crate) fn set_formula_cache(&mut self, r: u32, c: u32, value: Value) {
        let (pi, ro) = Self::split(r);
        let page = if (pi as usize) < self.pages.len() {
            let Some(arc) = self.pages[pi as usize].as_mut() else {
                return;
            };
            if Arc::strong_count(arc) > 1 && !arc.contains(ro, c) {
                return;
            }
            Arc::make_mut(arc)
        } else {
            let Some(arc) = self.far_pages.as_mut().and_then(|m| m.get_mut(&pi)) else {
                return;
            };
            if Arc::strong_count(arc) > 1 && !arc.contains(ro, c) {
                return;
            }
            Arc::make_mut(arc)
        };
        let Some(slot) = page.cols.get_mut(c) else {
            return;
        };
        if !slot.contains(ro) {
            return;
        }
        let became_blank = Arc::make_mut(slot).set_formula_cache(ro, value);
        if became_blank {
            if slot.is_empty() {
                page.cols.remove(c);
                if page.is_empty() {
                    self.remove_page(pi);
                }
            }
            Self::invalidate_boundary(&mut self.bounds, r, c);
        }
        self.version.advance();
    }

    pub(crate) fn foreign_formula_cells(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.iter_pages()
            .filter(|(_, page)| page.may_have_qualified_formula)
            .flat_map(|(pi, page)| {
                page.cols.iter_entries().flat_map(move |(col, slab)| {
                    let cells: &[Cell] = match slab.as_ref() {
                        ColSlab::Mixed(cs) => cs.as_slice(),
                        ColSlab::Numbers(_) | ColSlab::Values(_) => &[],
                    };
                    cells.iter().enumerate().filter_map(move |(row, cell)| {
                        cell.formula
                            .as_deref()
                            .is_some_and(|f| f.contains('!'))
                            .then_some((CellRef::new(pi * PAGE_ROWS + row as u32, col), cell))
                    })
                })
            })
    }

    pub(crate) fn formula_cells(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.iter_pages()
            .filter(|(_, p)| p.may_have_formula)
            .flat_map(|(pi, p)| {
                p.cols.iter_entries().flat_map(move |(c, slab)| {
                    let col: &[Cell] = match slab.as_ref() {
                        ColSlab::Mixed(cs) => cs.as_slice(),
                        ColSlab::Numbers(_) | ColSlab::Values(_) => &[],
                    };
                    col.iter().enumerate().filter_map(move |(row, cell)| {
                        cell.formula
                            .is_some()
                            .then_some((CellRef::new(pi * PAGE_ROWS + row as u32, c), cell))
                    })
                })
            })
    }

    pub fn formula_count(&self) -> usize {
        if self.len == 0 {
            return 0;
        }
        *self.formula_count.get_or_init(|| self.formula_cells().count())
    }

    pub(crate) fn has_formulas(&self) -> bool {
        if let Some(&n) = self.formula_count.get() {
            return n > 0;
        }
        self.formula_cells().next().is_some()
    }

    /// 写入单元格；cell 为空白时等价于删除
    #[inline]
    pub fn set(&mut self, r: u32, c: u32, cell: Cell) {
        if cell.is_blank() {
            if self.remove(r, c).is_none() {
                self.version.advance();
            }
            return;
        }
        self.version.advance();
        let (pi, ro) = Self::split(r);
        let new_has_formula = cell.formula.is_some();
        let new_has_qual = new_has_formula
            && cell.formula.as_deref().is_some_and(|f| f.contains('!'));
        let page = self.page_mut_or_insert(pi);
        if new_has_formula {
            page.may_have_formula = true;
            page.may_have_qualified_formula |= new_has_qual;
        }
        let (existed, old_has_formula) = page.set(ro, c, cell);
        if old_has_formula != new_has_formula
            && let Some(fc) = self.formula_count.get_mut()
        {
            if new_has_formula {
                *fc += 1;
            } else {
                *fc = fc.saturating_sub(1);
            }
        }
        if !existed {
            self.len += 1;
            if self.len == 1 {
                self.bounds = Some((r, c, r, c)).into();
            } else if let Some(bounds) = self.bounds.get_mut() {
                *bounds = Some(match *bounds {
                    Some((r1, c1, r2, c2)) => (r1.min(r), c1.min(c), r2.max(r), c2.max(c)),
                    None => (r, c, r, c),
                });
            }
        }
    }

    pub fn remove(&mut self, r: u32, c: u32) -> Option<Cell> {
        let (pi, ro) = Self::split(r);
        if !self.page(pi).is_some_and(|p| p.contains(ro, c)) {
            return None;
        }
        let page = self.page_mut_existing(pi)?;
        let slot_arc = page.cols.get_mut(c)?;
        let slab = Arc::make_mut(slot_arc);
        let old = slab.remove(ro)?;
        if slab.is_empty() {
            page.cols.remove(c);
        }
        if page.is_empty() {
            self.remove_page(pi);
        }
        self.version.advance();
        if old.formula.is_some()
            && let Some(fc) = self.formula_count.get_mut()
        {
            *fc = fc.saturating_sub(1);
        }
        self.len -= 1;
        if self.len == 0 {
            self.clear();
        } else {
            Self::invalidate_boundary(&mut self.bounds, r, c);
        }
        Some(old)
    }

    /// 单列按页零分配类型化切片迭代：按升序页号返回 `(本段起始行号, ColSlice<'_>)`
    pub fn column_typed_slices(
        &self,
        r1: u32,
        r2: u32,
        c: u32,
    ) -> impl Iterator<Item = (u32, ColSlice<'_>)> + '_ {
        let p1 = r1 / PAGE_ROWS;
        let p2 = r2 / PAGE_ROWS;
        self.iter_pages_in(p1, p2).filter_map(move |(pi, page)| {
            if r1 > r2 || !page.has_col(c) {
                return None;
            }
            let slab = page.cols.get(c)?;
            let base = pi * PAGE_ROWS;
            let start = r1.saturating_sub(base) as usize;
            let end = ((r2 - base) as usize + 1).min(slab.len());
            (start < end).then(|| (base + start as u32, slab.as_slice(start, end)))
        })
    }

    /// 单列范围内的连续访问：跨页拼接
    pub fn column_span(&self, r1: u32, r2: u32, c: u32) -> Vec<Cow<'_, Cell>> {
        let mut out = Vec::new();
        for (_, slice) in self.column_typed_slices(r1, r2, c) {
            match slice {
                ColSlice::Numbers { present, start, values } => {
                    for (i, &n) in values.iter().enumerate() {
                        let ro = start + i;
                        if (present[ro / 64] & (1u64 << (ro % 64))) != 0 {
                            out.push(Cow::Owned(Cell {
                                value: Value::Number(n),
                                formula: None,
                            }));
                        } else {
                            out.push(Cow::Owned(Cell::default()));
                        }
                    }
                }
                ColSlice::Values(vs) => {
                    out.extend(vs.iter().map(|v| {
                        Cow::Owned(Cell {
                            value: v.clone(),
                            formula: None,
                        })
                    }));
                }
                ColSlice::Mixed(cs) => {
                    out.extend(cs.iter().map(Cow::Borrowed));
                }
            }
        }
        out
    }

    pub(crate) fn cf_pages(&self, (r1, c1, r2, c2): (u32, u32, u32, u32)) -> Vec<crate::cf::PageSlice> {
        if r1 > r2 || c1 > c2 || self.len == 0 {
            return Vec::new();
        }
        let mut result = Vec::new();
        for (page, data) in self.iter_pages_in(r1 / PAGE_ROWS, r2 / PAGE_ROWS) {
            let base = page * PAGE_ROWS;
            let start = r1.saturating_sub(base) as usize;
            let max_end = (r2 - base) as usize + 1;
            for (col, cells) in data.cols.iter_entries_in(c1, c2) {
                let end = cells.len().min(max_end);
                if start < end {
                    result.push(crate::cf::PageSlice {
                        page,
                        col,
                        start,
                        end,
                        cells: cells.clone(),
                    });
                }
            }
        }
        result
    }

    /// A read-only view of one column's allocated pages, ordered by base row.
    pub(crate) fn column_pages(&self, col: u32) -> Vec<(u32, Arc<ColSlab>)> {
        self.iter_pages()
            .filter_map(|(page, data)| {
                if !data.has_col(col) {
                    return None;
                }
                let cells = data.cols.get(col)?;
                (!cells.is_empty()).then(|| (page * PAGE_ROWS, cells.clone()))
            })
            .collect()
    }

    /// 按行列升序零拷贝遍历（页 → 行 → 列）
    pub fn iter_views(&self) -> impl Iterator<Item = (CellRef, CellView<'_>)> {
        self.iter_rect_views((0, 0, u32::MAX, u32::MAX))
    }

    /// 按行列升序零拷贝遍历矩形区域，零每行 Vec 克隆。
    pub fn iter_rect_views(
        &self,
        (r1, c1, r2, c2): (u32, u32, u32, u32),
    ) -> impl Iterator<Item = (CellRef, CellView<'_>)> {
        let valid = r1 <= r2 && c1 <= c2 && self.len > 0;
        let p1 = if valid { r1 / PAGE_ROWS } else { 1 };
        let p2 = if valid { r2 / PAGE_ROWS } else { 0 };
        self.iter_pages_in(p1, p2).flat_map(move |(pi, page)| {
            let base = pi * PAGE_ROWS;
            let start_ro = r1.saturating_sub(base);
            let end_ro = page.n_rows.min((r2 - base).saturating_add(1));
            let cols: Vec<(u32, &ColSlab)> = if start_ro < end_ro {
                page.cols
                    .iter_entries_in(c1, c2)
                    .map(|(col, arc)| (col, arc.as_ref()))
                    .collect()
            } else {
                Vec::new()
            };
            let mut row_words = if start_ro < end_ro && !cols.is_empty() {
                page.row_mask
            } else {
                [0; 4]
            };
            if start_ro < end_ro && !cols.is_empty() {
                let sw = (start_ro / 64) as usize;
                let ew = ((end_ro - 1) / 64) as usize;
                for w in 0..sw {
                    row_words[w] = 0;
                }
                row_words[sw] &= !0u64 << (start_ro % 64);
                let hi = (end_ro - 1) % 64;
                if hi < 63 {
                    row_words[ew] &= (1u64 << (hi + 1)) - 1;
                }
                for w in (ew + 1)..4 {
                    row_words[w] = 0;
                }
            }
            PageRectViewsIter {
                base,
                cols,
                row_words,
                word: 0,
                cur_ro: 0,
                col_idx: usize::MAX,
            }
        })
    }

    /// 按行列升序遍历（页 → 行 → 列）
    pub fn iter(&self) -> impl Iterator<Item = (CellRef, Cow<'_, Cell>)> {
        self.iter_rect((0, 0, u32::MAX, u32::MAX))
    }

    pub(crate) fn iter_rect(
        &self,
        rect: (u32, u32, u32, u32),
    ) -> impl Iterator<Item = (CellRef, Cow<'_, Cell>)> {
        self.iter_rect_views(rect).map(|(k, v)| (k, v.to_cow()))
    }

    /// Stream occupied cells in `(row, col)` order paired with their explicit style ID,
    /// fetching each `(page, col)` style slab once per page directly from `CellStyles` page directory
    /// with ZERO per-row Vec cloning and ZERO `Arc<str>` cloning.
    pub(crate) fn iter_with_explicit_style<'a>(
        &'a self,
        styles: &'a CellStyles,
    ) -> impl Iterator<Item = (CellRef, CellView<'a>, Option<u16>)> + 'a {
        self.iter_pages().flat_map(move |(pi, page)| {
            let base = pi * PAGE_ROWS;
            let style_page = styles.page(pi);
            let cols: Vec<(u32, &'a ColSlab, Option<&'a StyleColumn>)> = page
                .cols
                .iter_entries()
                .map(|(col, slab)| {
                    let st = style_page.and_then(|sp| sp.get_col(col)).map(Arc::as_ref);
                    (col, slab.as_ref(), st)
                })
                .collect();
            PageWithExplicitStyleIter {
                base,
                cols,
                row_words: page.row_mask,
                word: 0,
                cur_ro: 0,
                col_idx: usize::MAX,
            }
        })
    }

    /// Merge-iterate all coordinates that have EITHER a non-blank cell OR an explicit style,
    /// in strict `(row, col)` ascending order with ZERO per-row Vec cloning.
    pub(crate) fn iter_merged_with_styles<'a>(
        &'a self,
        styles: &'a CellStyles,
    ) -> impl Iterator<Item = (CellRef, Option<CellView<'a>>, Option<u16>)> + 'a {
        let mut all_pages: Vec<u32> = self
            .iter_pages()
            .map(|(p, _)| p)
            .chain(styles.iter_pages().map(|(p, _)| p))
            .collect();
        all_pages.sort_unstable();
        all_pages.dedup();
        all_pages.into_iter().flat_map(move |pi| {
            let base = pi * PAGE_ROWS;
            let page = self.page(pi);
            let style_page = styles.page(pi);
            let mut merged_cols: Vec<u32> = page
                .into_iter()
                .flat_map(|p| p.iter_nonempty_cols(0, u32::MAX))
                .chain(
                    style_page
                        .into_iter()
                        .flat_map(|sp| sp.iter_nonempty_cols(0, u32::MAX)),
                )
                .collect();
            merged_cols.sort_unstable();
            merged_cols.dedup();
            let mut row_words = page.map_or([0; 4], |p| p.row_mask);
            let col_views: Vec<(u32, Option<&'a ColSlab>, Option<&'a StyleColumn>)> = merged_cols
                .into_iter()
                .map(|c| {
                    let cells = page.and_then(|p| p.cols.get(c)).map(Arc::as_ref);
                    let sc = style_page.and_then(|sp| sp.get_col(c)).map(Arc::as_ref);
                    if let Some(sc) = sc {
                        match sc {
                            StyleColumn::Uniform(_) => row_words = [u64::MAX; 4],
                            StyleColumn::Sparse { rows, .. } => {
                                for w in 0..4 {
                                    row_words[w] |= rows[w];
                                }
                            }
                        }
                    }
                    (c, cells, sc)
                })
                .collect();
            PageMergedStylesIter {
                base,
                col_views,
                row_words,
                word: 0,
                cur_ro: 0,
                col_idx: usize::MAX,
            }
        })
    }

    /// Find the next (or previous) occupied column in `row` strictly after/before `from_col`.
    pub fn next_occupied_in_row(&self, row: u32, from_col: u32, forward: bool) -> Option<u32> {
        let (pi, ro) = Self::split(row);
        let page = self.page(pi)?;
        if (page.row_mask[(ro / 64) as usize] & (1u64 << (ro % 64))) == 0 {
            return None;
        }
        if forward {
            let start = from_col.checked_add(1)?;
            for (c, slab) in page.cols.iter_entries_in(start, u32::MAX) {
                if slab.contains(ro) {
                    return Some(c);
                }
            }
            None
        } else {
            let end = from_col.checked_sub(1)?;
            for c in page.cols.iter_nonempty_cols_rev(end) {
                if page.contains(ro, c) {
                    return Some(c);
                }
            }
            None
        }
    }

    /// Find the next (or previous) occupied row in `col` strictly after/before `from_row`.
    pub fn next_occupied_in_col(&self, col: u32, from_row: u32, forward: bool) -> Option<u32> {
        if forward {
            let start = from_row.checked_add(1)?;
            for (pi, page) in self.iter_pages_in(start / PAGE_ROWS, u32::MAX) {
                if !page.has_col(col) {
                    continue;
                }
                let Some(slab) = page.cols.get(col) else {
                    continue;
                };
                let base = pi * PAGE_ROWS;
                let lo = start.saturating_sub(base) as usize;
                if let Some(off) = slab.next_occupied_from(lo) {
                    return Some(base + off);
                }
            }
            None
        } else {
            let end = from_row.checked_sub(1)?;
            let end_p = end / PAGE_ROWS;
            if let Some(far) = self.far_pages.as_deref()
                && end_p >= DIRECT_PAGE_LIMIT
            {
                for (&pi, page) in far.range(DIRECT_PAGE_LIMIT..=end_p).rev() {
                    if !page.has_col(col) {
                        continue;
                    }
                    if let Some(slab) = page.cols.get(col) {
                        let base = pi * PAGE_ROWS;
                        let hi = ((end - base) as usize + 1).min(slab.len());
                        if let Some(off) = slab.prev_occupied_before(hi) {
                            return Some(base + off);
                        }
                    }
                }
            }
            if !self.page_bits.is_empty() {
                let direct_end = end_p.min(DIRECT_PAGE_LIMIT - 1);
                let max_w = (direct_end / 64) as usize;
                for w in (0..=max_w.min(self.page_bits.len() - 1)).rev() {
                    let mut bits = self.page_bits[w];
                    if w == max_w {
                        let hi = direct_end % 64;
                        if hi < 63 {
                            bits &= (1u64 << (hi + 1)) - 1;
                        }
                    }
                    while bits != 0 {
                        let bit_idx = 63 - bits.leading_zeros();
                        bits &= !(1u64 << bit_idx);
                        let pi = w as u32 * 64 + bit_idx;
                        let Some(Some(page)) = self.pages.get(pi as usize) else {
                            continue;
                        };
                        if !page.has_col(col) {
                            continue;
                        }
                        if let Some(slab) = page.cols.get(col) {
                            let base = pi * PAGE_ROWS;
                            let hi = ((end - base) as usize + 1).min(slab.len());
                            if let Some(off) = slab.prev_occupied_before(hi) {
                                return Some(base + off);
                            }
                        }
                    }
                }
            }
            None
        }
    }

    pub fn iter_values(&self) -> impl Iterator<Item = Cow<'_, Cell>> {
        self.iter().map(|(_, c)| c)
    }

    pub(crate) fn stored_cells(&self) -> impl Iterator<Item = (CellRef, CellView<'_>)> {
        self.stored_cells_in((0, 0, u32::MAX, u32::MAX))
    }

    /// Search sorts its hits afterwards. Visit only allocated non-empty column slabs,
    /// so a distant column does not turn sparse pages into dense grids.
    pub(crate) fn stored_cells_in(
        &self,
        (r1, c1, r2, c2): (u32, u32, u32, u32),
    ) -> impl Iterator<Item = (CellRef, CellView<'_>)> {
        let valid = r1 <= r2 && c1 <= c2 && self.len > 0;
        let p1 = if valid { r1 / PAGE_ROWS } else { 1 };
        let p2 = if valid { r2 / PAGE_ROWS } else { 0 };
        self.iter_pages_in(p1, p2).flat_map(move |(p, page)| {
            let base = p * PAGE_ROWS;
            let first = r1.saturating_sub(base) as usize;
            let last = (r2 - base).saturating_add(1) as usize;
            page.cols.iter_entries_in(c1, c2).flat_map(move |(c, slab)| {
                let end = last.min(slab.len());
                let lo = first.min(end);
                (lo..end).filter_map(move |r| {
                    slab.get_view(r as u32)
                        .map(|v| (CellRef::new(base + r as u32, c), v))
                })
            })
        })
    }

    /// Occupied cells in this snapshot that differ from the other snapshot.
    pub(crate) fn changed_keys(&self, other: &Self) -> impl Iterator<Item = CellRef> {
        self.iter_pages()
            .filter(move |&(p, page)| other.page(p).is_none_or(|old| !Arc::ptr_eq(old, page)))
            .flat_map(move |(p, page)| {
                let old_page = other.page(p);
                page.cols.iter_entries().flat_map(move |(col, cells)| {
                    let old = old_page.and_then(|op| op.cols.get(col));
                    let shared = old.is_some_and(|old| Arc::ptr_eq(old, cells));
                    let len = if shared { 0 } else { cells.len() };
                    (0..len).filter_map(move |row| {
                        let cur = cells.get_view(row as u32)?;
                        let differs = old
                            .and_then(|o| o.get_view(row as u32))
                            .is_none_or(|prior| !prior.same_cell(&cur));
                        differs.then_some(CellRef::new(p * PAGE_ROWS + row as u32, col))
                    })
                })
            })
    }

    /// Rebuild only the affected page band once. Pages strictly before `at / PAGE_ROWS`
    /// (for row edits) stay untouched in `O(1)`. Whole-page multiples move `Arc<Page>` directly.
    pub(crate) fn remap_axis(
        &mut self,
        rows: bool,
        at: u32,
        map: impl Fn(u32) -> Option<u32>,
        cleared: &mut CellJournal,
    ) {
        if self.len == 0 {
            return;
        }
        let mut occupied = Vec::<(u32, u32, [u64; 4])>::new();
        self.bounds.take();
        self.formula_count.take();
        self.version.advance();

        let first_pi = if rows { at / PAGE_ROWS } else { 0 };
        let mut source_pages: Vec<(u32, Arc<Page>)> = Vec::new();
        if first_pi < DIRECT_PAGE_LIMIT && !self.page_bits.is_empty() {
            let start_w = (first_pi / 64) as usize;
            for w in start_w..self.page_bits.len() {
                let mut bits = self.page_bits[w];
                if w == start_w {
                    let keep_mask = (1u64 << (first_pi % 64)) - 1;
                    let take_bits = bits & !keep_mask;
                    self.page_bits[w] = bits & keep_mask;
                    bits = take_bits;
                } else {
                    self.page_bits[w] = 0;
                }
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    bits &= bits - 1;
                    let pi = w as u32 * 64 + tz;
                    if let Some(slot) = self.pages.get_mut(pi as usize)
                        && let Some(page) = slot.take()
                    {
                        source_pages.push((pi, page));
                    }
                }
            }
            while self.page_bits.last() == Some(&0) {
                self.page_bits.pop();
            }
        }
        if let Some(mut far) = self.far_pages.take() {
            if first_pi <= DIRECT_PAGE_LIMIT {
                source_pages.extend(*far);
            } else {
                let tail = far.split_off(&first_pi);
                if !far.is_empty() {
                    self.far_pages = Some(far);
                }
                source_pages.extend(tail);
            }
        }
        if source_pages.is_empty() {
            return;
        }

        let mut dirty_pages = Vec::<u32>::new();

        for (pi, page_arc) in source_pages {
            let base = pi * PAGE_ROWS;
            if (rows && base + PAGE_ROWS <= at)
                || (!rows && page_arc.cols.max_col().is_none_or(|m| m < at))
            {
                self.insert_page_arc(pi, page_arc);
                continue;
            }
            if !rows {
                let mut page = Arc::unwrap_or_clone(page_arc);
                let old_entries = std::mem::take(&mut page.cols).into_entries();
                let mut new_cols = PageCols::default();
                for (c, slab) in old_entries {
                    if c >= at {
                        occupied.push((pi, c, slab.row_mask()));
                    }
                    if let Some(nc) = map(c) {
                        new_cols.set(nc, slab);
                    } else {
                        self.len -= slab.occupied_count();
                    }
                }
                page.cols = new_cols;
                if !page.is_empty() {
                    page.rebuild_masks();
                    self.insert_page_arc(pi, Arc::new(page));
                }
                continue;
            }

            // Whole-page fast path when shifting by an exact multiple of PAGE_ROWS:
            // reuse `Arc<Page>` directly without cloning `Page` or touching `Arc<ColSlab>`.
            if base >= at
                && let Some(target0) = map(base)
                && target0 % PAGE_ROWS == 0
                && map(base + PAGE_ROWS - 1) == Some(target0 + PAGE_ROWS - 1)
            {
                let dest = target0 / PAGE_ROWS;
                for (c, slab) in page_arc.cols.iter_entries() {
                    occupied.push((pi, c, slab.row_mask()));
                }
                if self.page(dest).is_none() {
                    self.insert_page_arc(dest, page_arc);
                    continue;
                }
            }

            let page = Arc::unwrap_or_clone(page_arc);
            let mut spans: Vec<(std::ops::Range<usize>, u32, u32)> = Vec::new();
            for r in 0..page.n_rows {
                let Some(target) = map(base + r) else { continue };
                let (dest, offset) = Self::split(target);
                if let Some((source, pid, start)) = spans.last_mut()
                    && *pid == dest
                    && source.end == r as usize
                    && *start + source.len() as u32 == offset
                {
                    source.end += 1;
                } else {
                    spans.push((r as usize..r as usize + 1, dest, offset));
                }
            }
            for &(_, dest, _) in &spans {
                if dirty_pages.last() != Some(&dest) {
                    dirty_pages.push(dest);
                }
            }
            let page_formula = page.may_have_formula;
            let page_qual = page.may_have_qualified_formula;
            let page_row_mask = page.row_mask;
            for (col, slab) in page.cols.into_entries() {
                if slab.is_empty() {
                    continue;
                }
                let begin = at.saturating_sub(base) as usize;
                occupied.push((pi, col, slab.row_mask_from(begin)));
                if let [(source, dest, 0)] = spans.as_slice()
                    && source.start == 0
                    && source.end >= slab.len()
                    && map(base + PAGE_ROWS - 1) == Some(*dest * PAGE_ROWS + PAGE_ROWS - 1)
                {
                    let p = self.page_mut_or_insert(*dest);
                    p.n_rows = p.n_rows.max(slab.len() as u32);
                    for w in 0..4 {
                        p.row_mask[w] |= page_row_mask[w];
                    }
                    p.cols.set(col, slab);
                    p.may_have_formula |= page_formula;
                    p.may_have_qualified_formula |= page_qual;
                    continue;
                }
                let mut src_slab = Arc::unwrap_or_clone(slab);
                for (source, dest, offset) in &spans {
                    let end = source.end.min(src_slab.len());
                    if source.start >= end {
                        break;
                    }
                    let dest_p = self.page_mut_or_insert(*dest);
                    dest_p.may_have_formula |= page_formula;
                    dest_p.may_have_qualified_formula |= page_qual;
                    let slot = dest_p
                        .cols
                        .get_or_insert_with(col, || Arc::new(src_slab.new_empty_like()));
                    src_slab.move_range_into(
                        source.start,
                        end,
                        Arc::make_mut(slot),
                        *offset as usize,
                    );
                }
                self.len -= src_slab.occupied_count();
            }
        }

        dirty_pages.sort_unstable();
        dirty_pages.dedup();
        for pi in dirty_pages {
            if let Some(p) = self.page_mut_existing(pi) {
                p.rebuild_masks();
                if p.is_empty() {
                    self.remove_page(pi);
                }
            }
        }

        cleared.retain(|k| (if rows { k.row } else { k.col }) < at || !self.contains(k.row, k.col));
        for (pi, col, words) in occupied {
            let dest = self.page(pi).and_then(|p| p.cols.get(col));
            for (word, mut bits) in words.into_iter().enumerate() {
                while bits != 0 {
                    let r = word * 64 + bits.trailing_zeros() as usize;
                    if dest.is_none_or(|slab| !slab.contains(r as u32)) {
                        cleared.insert(CellRef::new(pi * PAGE_ROWS + r as u32, col));
                    }
                    bits &= bits - 1;
                }
            }
        }
    }

    /// Permute coordinates strictly within `[lo..hi)` (`map` is a bijection on `[lo..hi)`),
    /// leaving all pages outside `[lo..hi)` completely untouched (`O(band_pages)`).
    pub(crate) fn permute_band(
        &mut self,
        rows: bool,
        lo: u32,
        hi: u32,
        map: impl Fn(u32) -> u32,
        cleared: &mut CellJournal,
    ) {
        if lo >= hi || self.len == 0 {
            return;
        }
        self.bounds.take();
        self.formula_count.take();
        self.version.advance();

        if !rows {
            let page_ids: Vec<u32> = self
                .iter_pages()
                .filter(|(_, p)| p.iter_nonempty_cols(lo, hi - 1).next().is_some())
                .map(|(pi, _)| pi)
                .collect();
            let mut old_occupied: Vec<(u32, u32, [u64; 4])> = Vec::new();
            for pi in page_ids {
                let Some(page) = self.page_mut_existing(pi) else { continue };
                let cols_in_band: Vec<u32> = page.iter_nonempty_cols(lo, hi - 1).collect();
                let mut moved: Vec<(u32, Arc<ColSlab>)> = Vec::with_capacity(cols_in_band.len());
                for c in cols_in_band {
                    if let Some(slab) = page.cols.remove(c) {
                        old_occupied.push((pi, c, slab.row_mask()));
                        moved.push((map(c), slab));
                    }
                }
                for (nc, slab) in moved {
                    page.cols.set(nc, slab);
                }
                page.rebuild_masks();
            }
            cleared.retain(|k| k.col < lo || k.col >= hi || !self.contains(k.row, k.col));
            for (pi, col, words) in old_occupied {
                let dest = self.page(pi).and_then(|p| p.cols.get(col));
                for (w, mut bits) in words.into_iter().enumerate() {
                    while bits != 0 {
                        let r = w * 64 + bits.trailing_zeros() as usize;
                        if dest.is_none_or(|slab| !slab.contains(r as u32)) {
                            cleared.insert(CellRef::new(pi * PAGE_ROWS + r as u32, col));
                        }
                        bits &= bits - 1;
                    }
                }
            }
            return;
        }

        let p_lo = lo / PAGE_ROWS;
        let p_hi = (hi - 1) / PAGE_ROWS;
        let active_pages: Vec<u32> = self.iter_pages_in(p_lo, p_hi).map(|(pi, _)| pi).collect();
        if active_pages.is_empty() {
            return;
        }
        let mut extracted: Vec<(u32, u32, Cell)> = Vec::new();
        let mut old_coords: Vec<CellRef> = Vec::new();
        let mut dirty_pages: Vec<u32> = Vec::new();
        let mut any_formula = false;
        let mut any_qual = false;
        for pi in active_pages {
            let base = pi * PAGE_ROWS;
            let r_start = lo.saturating_sub(base) as usize;
            let r_end = ((hi - 1 - base) as usize + 1).min(PAGE_ROWS as usize);
            let Some(page) = self.page_mut_existing(pi) else { continue };
            any_formula |= page.may_have_formula;
            any_qual |= page.may_have_qualified_formula;
            let cols: Vec<u32> = page.iter_nonempty_cols(0, u32::MAX).collect();
            let mut touched_page = false;
            for c in cols {
                let Some(slot) = page.cols.get_mut(c) else {
                    continue;
                };
                let end = r_end.min(slot.len());
                if r_start >= end || !slot.has_any_in(r_start, end) {
                    continue;
                }
                let slab = Arc::make_mut(slot);
                for r_off in r_start..end {
                    if let Some(cell) = slab.remove(r_off as u32) {
                        let orig_r = base + r_off as u32;
                        old_coords.push(CellRef::new(orig_r, c));
                        extracted.push((map(orig_r), c, cell));
                        touched_page = true;
                    }
                }
            }
            if touched_page {
                dirty_pages.push(pi);
            }
        }
        for (nr, c, cell) in extracted {
            let (dpi, dro) = Self::split(nr);
            let p = self.page_mut_or_insert(dpi);
            p.may_have_formula |= any_formula || cell.formula.is_some();
            p.may_have_qualified_formula |=
                any_qual || cell.formula.as_deref().is_some_and(|f| f.contains('!'));
            p.set(dro, c, cell);
            dirty_pages.push(dpi);
        }
        dirty_pages.sort_unstable();
        dirty_pages.dedup();
        for pi in dirty_pages {
            if let Some(p) = self.page_mut_existing(pi) {
                p.rebuild_masks();
                if p.is_empty() {
                    self.remove_page(pi);
                }
            }
        }
        cleared.retain(|k| k.row < lo || k.row >= hi || !self.contains(k.row, k.col));
        for k in old_coords {
            if !self.contains(k.row, k.col) {
                cleared.insert(k);
            }
        }
    }

    /// Check if `row` has any occupied cell in `[c1..=c2]` using page `row_mask` + `PageCols`.
    #[inline]
    pub(crate) fn row_has_any_in(&self, row: u32, c1: u32, c2: u32) -> bool {
        if c1 > c2 || self.len == 0 {
            return false;
        }
        let (pi, ro) = Self::split(row);
        let Some(page) = self.page(pi) else {
            return false;
        };
        if (page.row_mask[(ro / 64) as usize] & (1u64 << (ro % 64))) == 0 {
            return false;
        }
        page.cols
            .iter_entries_in(c1, c2)
            .any(|(_, slab)| slab.contains(ro))
    }

    /// Check if `col` has any occupied cell in `[r1..=r2]` using `page_bits` + `has_col` bitmaps.
    #[inline]
    pub(crate) fn col_has_any_in(&self, col: u32, r1: u32, r2: u32) -> bool {
        if r1 > r2 || self.len == 0 {
            return false;
        }
        let p1 = r1 / PAGE_ROWS;
        let p2 = r2 / PAGE_ROWS;
        for (pi, page) in self.iter_pages_in(p1, p2) {
            if !page.has_col(col) {
                continue;
            }
            let Some(slab) = page.cols.get(col) else {
                continue;
            };
            let base = pi * PAGE_ROWS;
            let lo = r1.saturating_sub(base) as usize;
            let hi = ((r2 - base) as usize + 1).min(slab.len());
            if slab.has_any_in(lo, hi) {
                return true;
            }
        }
        false
    }

    /// Single-pass symmetric diff between `self` (`current`) and `other` (`target`):
    /// skips identical `Arc<Page>` and `Arc<ColSlab>` slabs in `O(1)`, marks cells vacated
    /// in `other` into `cleared_in_other`, and appends all changed `(sheet_idx, r, c)` in page order.
    pub(crate) fn diff_both_sides(
        &self,
        other: &Self,
        sheet_idx: usize,
        cleared_in_other: &mut CellJournal,
        out: &mut Vec<(usize, u32, u32)>,
    ) {
        let mut check_pair = |pi: u32, pa: Option<&Arc<Page>>, pb: Option<&Arc<Page>>| {
            match (pa, pb) {
                (None, None) => return,
                (Some(a), Some(b)) if Arc::ptr_eq(a, b) => return,
                _ => {}
            }
            let base = pi * PAGE_ROWS;
            PageCols::for_each_union_col_in(
                pa.map(|p| &p.cols),
                pb.map(|p| &p.cols),
                0,
                u32::MAX,
                |col, ca, cb| {
                    if let (Some(xa), Some(xb)) = (ca, cb)
                        && Arc::ptr_eq(xa, xb)
                    {
                        return;
                    }
                    let rows = ca.map_or(0, |s| s.len()).max(cb.map_or(0, |s| s.len()));
                    for r in 0..rows {
                        let a_cell = ca.and_then(|s| s.get_view(r as u32));
                        let b_cell = cb.and_then(|s| s.get_view(r as u32));
                        let differs = match (a_cell, b_cell) {
                            (None, None) => false,
                            (Some(_), None) => {
                                cleared_in_other.insert(CellRef::new(base + r as u32, col));
                                true
                            }
                            (None, Some(_)) => true,
                            (Some(ac), Some(bc)) => !ac.same_cell(&bc),
                        };
                        if differs {
                            out.push((sheet_idx, base + r as u32, col));
                        }
                    }
                },
            );
        };

        let max_direct_words = self.page_bits.len().max(other.page_bits.len());
        for w in 0..max_direct_words {
            let mut bits = self.page_bits.get(w).copied().unwrap_or(0)
                | other.page_bits.get(w).copied().unwrap_or(0);
            while bits != 0 {
                let tz = bits.trailing_zeros();
                bits &= bits - 1;
                let pi = w as u32 * 64 + tz;
                check_pair(pi, self.page(pi), other.page(pi));
            }
        }
        if self.far_pages.is_some() || other.far_pages.is_some() {
            let mut far_keys: Vec<u32> = self
                .far_pages
                .iter()
                .flat_map(|m| m.keys().copied())
                .chain(other.far_pages.iter().flat_map(|m| m.keys().copied()))
                .collect();
            far_keys.sort_unstable();
            far_keys.dedup();
            for pi in far_keys {
                check_pair(pi, self.page(pi), other.page(pi));
            }
        }
    }

    /// Single-pass symmetric input diff between `self` (`before`) and `other` (`after`):
    /// skips identical `Arc<Page>` and `Arc<ColSlab>` slabs in `O(1)`, marks differing
    /// input coordinates into `cell_mask` using 256-bit word masks, and returns
    /// `(forked_slab_bytes, diff_heap_bytes)` for accurate CoW undo memory accounting.
    pub(crate) fn diff_into_journal(
        &self,
        other: &Self,
        cell_mask: &mut CellJournal,
    ) -> (usize, usize) {
        let mut seen_slabs = std::collections::HashSet::new();
        let mut forked_bytes = 0usize;
        let mut diff_heap = 0usize;
        let mut account_slab = |slab: &Arc<ColSlab>| {
            if seen_slabs.insert(Arc::as_ptr(slab) as usize) {
                forked_bytes += slab.payload_bytes();
            }
        };
        let mut check_pair = |pi: u32, pa: Option<&Arc<Page>>, pb: Option<&Arc<Page>>| {
            match (pa, pb) {
                (None, None) => return,
                (Some(a), Some(b)) if Arc::ptr_eq(a, b) => return,
                _ => {}
            }
            PageCols::for_each_union_col_in(
                pa.map(|p| &p.cols),
                pb.map(|p| &p.cols),
                0,
                u32::MAX,
                |col, ca, cb| {
                    if let (Some(xa), Some(xb)) = (ca, cb)
                        && Arc::ptr_eq(xa, xb)
                    {
                        return;
                    }
                    if let Some(xa) = ca {
                        account_slab(xa);
                    }
                    if let Some(xb) = cb {
                        account_slab(xb);
                    }
                    let mut mask = [0u64; 4];
                    match (ca.map(Arc::as_ref), cb.map(Arc::as_ref)) {
                        (None, Some(ColSlab::Numbers(nb))) => {
                            mask = nb.present;
                        }
                        (Some(ColSlab::Numbers(na)), None) => {
                            mask = na.present;
                        }
                        (Some(ColSlab::Numbers(na)), Some(ColSlab::Numbers(nb))) => {
                            let rows = na.values.len().max(nb.values.len());
                            for w in 0..4 {
                                mask[w] = na.present[w] ^ nb.present[w];
                            }
                            for r in 0..rows {
                                let w = r / 64;
                                let bit = 1u64 << (r % 64);
                                if (na.present[w] & nb.present[w] & bit) != 0
                                    && na.values[r].to_bits() != nb.values[r].to_bits()
                                {
                                    mask[w] |= bit;
                                }
                            }
                        }
                        _ => {
                            let rows = ca.map_or(0, |s| s.len()).max(cb.map_or(0, |s| s.len()));
                            for r in 0..rows {
                                let ac = ca.and_then(|s| s.get_view(r as u32));
                                let bc = cb.and_then(|s| s.get_view(r as u32));
                                let differs = match (ac, bc) {
                                    (None, None) => false,
                                    (Some(a), None) => {
                                        diff_heap += a.heap_bytes();
                                        true
                                    }
                                    (None, Some(b)) => {
                                        diff_heap += b.heap_bytes();
                                        true
                                    }
                                    (Some(a), Some(b)) => {
                                        if !a.same_input(&b) {
                                            diff_heap += a.heap_bytes() + b.heap_bytes();
                                            true
                                        } else {
                                            false
                                        }
                                    }
                                };
                                if differs {
                                    mask[r / 64] |= 1u64 << (r % 64);
                                }
                            }
                        }
                    }
                    if mask != [0; 4] {
                        cell_mask.or_page_mask(pi, col, mask);
                    }
                },
            );
        };

        let max_direct_words = self.page_bits.len().max(other.page_bits.len());
        for w in 0..max_direct_words {
            let mut bits = self.page_bits.get(w).copied().unwrap_or(0)
                | other.page_bits.get(w).copied().unwrap_or(0);
            while bits != 0 {
                let tz = bits.trailing_zeros();
                bits &= bits - 1;
                let pi = w as u32 * 64 + tz;
                check_pair(pi, self.page(pi), other.page(pi));
            }
        }
        if self.far_pages.is_some() || other.far_pages.is_some() {
            let mut far_keys: Vec<u32> = self
                .far_pages
                .iter()
                .flat_map(|m| m.keys().copied())
                .chain(other.far_pages.iter().flat_map(|m| m.keys().copied()))
                .collect();
            far_keys.sort_unstable();
            far_keys.dedup();
            for pi in far_keys {
                check_pair(pi, self.page(pi), other.page(pi));
            }
        }
        (forked_bytes, diff_heap)
    }

    /// Cache-friendly column-slab scan of total retained heap bytes across all cell strings/formulas.
    pub(crate) fn total_cell_heap_bytes(&self) -> usize {
        let mut total = 0usize;
        for (_, page) in self.iter_pages() {
            for (_, slab) in page.cols.iter_entries() {
                match slab.as_ref() {
                    ColSlab::Numbers(_) => {} // O(1) skip: pure f64 slabs have 0 string/formula bytes!
                    ColSlab::Values(vs) => {
                        for v in vs {
                            if let Value::Text(t) = v {
                                total += t.len();
                            }
                        }
                    }
                    ColSlab::Mixed(cs) => {
                        for cell in cs {
                            if let Some(f) = cell.formula.as_deref() {
                                total += f.len();
                            }
                            if let Value::Text(t) = &cell.value {
                                total += t.len();
                            }
                        }
                    }
                }
            }
        }
        total
    }

    /// Page-level rectangle clear for `[r1..=r2, c1..=c2]`: drops full column slabs in `O(1)`
    /// and updates `cleared` using 256-bit word masks. Returns count of cleared cells.
    pub(crate) fn clear_rect(
        &mut self,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
        cleared: &mut CellJournal,
    ) -> usize {
        if r1 > r2 || c1 > c2 || self.len == 0 {
            return 0;
        }
        let p1 = r1 / PAGE_ROWS;
        let p2 = r2 / PAGE_ROWS;
        let hit_pages: Vec<u32> = self
            .iter_pages_in(p1, p2)
            .filter(|(_, page)| page.iter_nonempty_cols(c1, c2).next().is_some())
            .map(|(pi, _)| pi)
            .collect();
        if hit_pages.is_empty() {
            return 0;
        }
        let mut removed = 0usize;
        for pi in hit_pages {
            let base = pi * PAGE_ROWS;
            let lo = r1.saturating_sub(base) as usize;
            let hi = (r2 - base).min(PAGE_ROWS - 1) as usize;
            let full_page = lo == 0 && hi == (PAGE_ROWS - 1) as usize;
            let Some(page) = self.page_mut_existing(pi) else {
                continue;
            };
            let cols: Vec<u32> = page.iter_nonempty_cols(c1, c2).collect();
            let mut page_changed = false;
            for col in cols {
                if full_page {
                    if let Some(slab) = page.cols.remove(col) {
                        let mask = slab.row_mask();
                        let count = mask.iter().map(|w| w.count_ones() as usize).sum::<usize>();
                        if count > 0 {
                            removed += count;
                            cleared.or_page_mask(pi, col, mask);
                        }
                        page_changed = true;
                    }
                } else if let Some(slot) = page.cols.get_mut(col) {
                    let end = (hi + 1).min(slot.len());
                    if lo >= end || !slot.has_any_in(lo, end) {
                        continue;
                    }
                    let slab = Arc::make_mut(slot);
                    let (rem, mask) = slab.clear_range(lo, hi);
                    if rem > 0 {
                        removed += rem;
                        cleared.or_page_mask(pi, col, mask);
                    }
                    if slab.is_empty() {
                        page.cols.remove(col);
                    }
                    page_changed = true;
                }
            }
            if page_changed {
                page.rebuild_masks();
                if page.is_empty() {
                    self.remove_page(pi);
                }
            }
        }
        if removed > 0 {
            self.len -= removed;
            self.version.advance();
            self.formula_count.take();
            if self.len == 0 {
                self.clear();
            } else {
                self.bounds.take();
            }
        }
        removed
    }

    /// Shift rows `>= at` ONLY within columns `[c1..=c2]` (insert down by `n` or delete `[at..at+n)`),
    /// reusing whole-slab moves and typed slab range moves without touching other columns.
    pub(crate) fn shift_col_band_rows(
        &mut self,
        c1: u32,
        c2: u32,
        at: u32,
        n: u32,
        delete: bool,
        cleared: &mut CellJournal,
    ) {
        if n == 0 || c1 > c2 || self.len == 0 {
            return;
        }
        let first_pi = at / PAGE_ROWS;
        let hit_pages: Vec<u32> = self
            .iter_pages_in(first_pi, u32::MAX)
            .filter(|(_, p)| p.iter_nonempty_cols(c1, c2).next().is_some())
            .map(|(pi, _)| pi)
            .collect();
        if hit_pages.is_empty() {
            return;
        }

        self.bounds.take();
        self.formula_count.take();
        self.version.advance();

        let map_row = |r: u32| -> Option<u32> {
            if delete {
                if r < at {
                    Some(r)
                } else if r < at.saturating_add(n) {
                    None
                } else {
                    Some(r - n)
                }
            } else if r >= at {
                r.checked_add(n)
            } else {
                Some(r)
            }
        };

        let mut extracted: Vec<(u32, u32, Arc<ColSlab>, bool, bool)> = Vec::new();
        let mut occupied: Vec<(u32, u32, [u64; 4])> = Vec::new();
        let mut dirty_pages: Vec<u32> = Vec::new();

        for pi in hit_pages {
            let base = pi * PAGE_ROWS;
            let lo = at.saturating_sub(base) as usize;
            let Some(page) = self.page_mut_existing(pi) else {
                continue;
            };
            let page_formula = page.may_have_formula;
            let page_qual = page.may_have_qualified_formula;
            let cols: Vec<u32> = page.iter_nonempty_cols(c1, c2).collect();
            let mut touched_page = false;
            for col in cols {
                if lo == 0 {
                    if let Some(slab) = page.cols.remove(col) {
                        occupied.push((pi, col, slab.row_mask()));
                        extracted.push((pi, col, slab, page_formula, page_qual));
                        touched_page = true;
                    }
                } else if let Some(slot) = page.cols.get_mut(col) {
                    if slot.len() <= lo {
                        continue;
                    }
                    let slab = Arc::make_mut(slot);
                    if let Some((tail, bits)) = slab.take_tail(lo) {
                        if slab.is_empty() {
                            page.cols.remove(col);
                        }
                        occupied.push((pi, col, bits));
                        extracted.push((pi, col, Arc::new(tail), page_formula, page_qual));
                        touched_page = true;
                    }
                }
            }
            if touched_page {
                dirty_pages.push(pi);
            }
        }

        for (pi, col, slab, page_formula, page_qual) in extracted {
            let base = pi * PAGE_ROWS;
            // Whole-slab fast path when shifting by exact multiple of PAGE_ROWS:
            if base >= at
                && let Some(target0) = map_row(base)
                && target0 % PAGE_ROWS == 0
                && map_row(base + PAGE_ROWS - 1) == Some(target0 + PAGE_ROWS - 1)
            {
                let dest = target0 / PAGE_ROWS;
                let dest_p = self.page_mut_or_insert(dest);
                dest_p.cols.set(col, slab);
                dest_p.may_have_formula |= page_formula;
                dest_p.may_have_qualified_formula |= page_qual;
                dirty_pages.push(dest);
                continue;
            }
            let lo = at.saturating_sub(base);
            let mut spans: Vec<(std::ops::Range<usize>, u32, u32)> = Vec::new();
            let slab_len = slab.len() as u32;
            for r in lo..slab_len {
                let Some(target) = map_row(base + r) else {
                    continue;
                };
                let (dest, offset) = Self::split(target);
                if let Some((source, pid, start)) = spans.last_mut()
                    && *pid == dest
                    && source.end == r as usize
                    && *start + source.len() as u32 == offset
                {
                    source.end += 1;
                } else {
                    spans.push((r as usize..r as usize + 1, dest, offset));
                }
            }
            let mut src_slab = Arc::unwrap_or_clone(slab);
            for (source, dest, offset) in &spans {
                let end = source.end.min(src_slab.len());
                if source.start >= end {
                    break;
                }
                let dest_p = self.page_mut_or_insert(*dest);
                dest_p.may_have_formula |= page_formula;
                dest_p.may_have_qualified_formula |= page_qual;
                let slot = dest_p
                    .cols
                    .get_or_insert_with(col, || Arc::new(src_slab.new_empty_like()));
                src_slab.move_range_into(
                    source.start,
                    end,
                    Arc::make_mut(slot),
                    *offset as usize,
                );
                dirty_pages.push(*dest);
            }
            self.len -= src_slab.occupied_count();
        }

        dirty_pages.sort_unstable();
        dirty_pages.dedup();
        for pi in dirty_pages {
            if let Some(p) = self.page_mut_existing(pi) {
                p.rebuild_masks();
                if p.is_empty() {
                    self.remove_page(pi);
                }
            }
        }

        cleared.retain(|k| {
            k.col < c1 || k.col > c2 || k.row < at || !self.contains(k.row, k.col)
        });
        for (pi, col, words) in occupied {
            let dest = self.page(pi).and_then(|p| p.cols.get(col));
            let mut cleared_mask = [0u64; 4];
            for (word, mut bits) in words.into_iter().enumerate() {
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    let r = word * 64 + tz as usize;
                    if dest.is_none_or(|slab| !slab.contains(r as u32)) {
                        cleared_mask[word] |= 1u64 << tz;
                    }
                    bits &= bits - 1;
                }
            }
            if cleared_mask != [0; 4] {
                cleared.or_page_mask(pi, col, cleared_mask);
            }
        }
    }

    /// Parallel page-level formula rewrite: visits only `may_have_formula` pages
    /// (and `may_have_qualified_formula` when `foreign_sheet` is `Some`), skips
    /// `ColSlab::Numbers` and `ColSlab::Values` in `O(1)`, clones each modified
    /// `Mixed` column slab at most once, and uses `std::thread::scope` for large sheets.
    pub(crate) fn rewrite_formulas_parallel<F>(
        &mut self,
        foreign_sheet: Option<&str>,
        rewrite_one: F,
    ) -> bool
    where
        F: Fn(&str) -> Option<Arc<str>> + Sync,
    {
        let quoted_foreign = foreign_sheet.map(|name| format!("'{}'", name.replace('\'', "''")));
        let matches_foreign = |formula: &str| -> bool {
            let Some(sheet_name) = foreign_sheet else {
                return true;
            };
            let quoted = quoted_foreign.as_deref().unwrap();
            formula.match_indices('!').any(|(end, _)| {
                [sheet_name.as_bytes(), quoted.as_bytes()]
                    .iter()
                    .any(|name| {
                        end >= name.len()
                            && formula.as_bytes()[end - name.len()..end].eq_ignore_ascii_case(name)
                    })
            })
        };

        let candidate_pages: Vec<(u32, &Arc<Page>)> = self
            .iter_pages()
            .filter(|(_, p)| {
                p.may_have_formula && (foreign_sheet.is_none() || p.may_have_qualified_formula)
            })
            .collect();
        if candidate_pages.is_empty() {
            return false;
        }

        let process_page = |pi: u32, page: &Arc<Page>| -> Option<(u32, Arc<Page>)> {
            let mut updated_cols: Vec<(u32, Arc<ColSlab>)> = Vec::new();
            let mut any_qual = page.may_have_qualified_formula;
            for (col, slab) in page.cols.iter_entries() {
                let ColSlab::Mixed(cells) = slab.as_ref() else {
                    continue;
                };
                let mut cloned: Option<Vec<Cell>> = None;
                for (r_off, cell) in cells.iter().enumerate() {
                    let Some(old) = cell.formula.as_deref() else {
                        continue;
                    };
                    if foreign_sheet.is_some() && (!old.contains('!') || !matches_foreign(old)) {
                        continue;
                    }
                    if let Some(new_f) = rewrite_one(old) {
                        any_qual |= new_f.contains('!');
                        let vec = cloned.get_or_insert_with(|| cells.clone());
                        vec[r_off].formula = Some(new_f);
                    }
                }
                if let Some(vec) = cloned {
                    updated_cols.push((col, Arc::new(ColSlab::Mixed(vec))));
                }
            }
            if updated_cols.is_empty() {
                return None;
            }
            let mut new_page = page.as_ref().clone();
            new_page.may_have_qualified_formula = any_qual;
            for (col, slab) in updated_cols {
                new_page.cols.set(col, slab);
            }
            Some((pi, Arc::new(new_page)))
        };

        let updates: Vec<(u32, Arc<Page>)> = if candidate_pages.len() >= 16 {
            let n_threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(8)
                .min(candidate_pages.len());
            let chunk_size = candidate_pages.len().div_ceil(n_threads);
            let mut all = Vec::new();
            std::thread::scope(|s| {
                let mut handles = Vec::with_capacity(n_threads);
                for chunk in candidate_pages.chunks(chunk_size) {
                    let proc_ref = &process_page;
                    handles.push(s.spawn(move || {
                        let mut local = Vec::new();
                        for &(pi, page) in chunk {
                            if let Some(upd) = proc_ref(pi, page) {
                                local.push(upd);
                            }
                        }
                        local
                    }));
                }
                for h in handles {
                    if let Ok(local) = h.join() {
                        all.extend(local);
                    }
                }
            });
            all
        } else {
            candidate_pages
                .iter()
                .filter_map(|&(pi, page)| process_page(pi, page))
                .collect()
        };

        if updates.is_empty() {
            return false;
        }
        for (pi, page) in updates {
            self.insert_page_arc(pi, page);
        }
        self.version.advance();
        true
    }

    /// Extract flat slab pointers for `col` across pages `p_lo..=p_hi` for O(1) parallel gather.
    pub(crate) fn col_page_slices(&self, col: u32, p_lo: u32, p_hi: u32) -> Vec<Option<&ColSlab>> {
        let count = (p_hi - p_lo + 1) as usize;
        let mut out = Vec::with_capacity(count);
        for pi in p_lo..=p_hi {
            let slab = self.page(pi).and_then(|p| p.cols.get(col)).map(Arc::as_ref);
            out.push(slab);
        }
        out
    }

    /// Install pre-built column slabs across pages `p_lo..=p_hi` produced by parallel gather sort.
    pub(crate) fn install_gathered_columns(
        &mut self,
        p_lo: u32,
        p_hi: u32,
        mut columns: Vec<GatheredColumn>,
        cleared: &mut CellJournal,
    ) {
        if columns.is_empty() || p_lo > p_hi {
            return;
        }
        let n_pages = (p_hi - p_lo + 1) as usize;
        let had_cleared = !cleared.is_empty();
        let mut net_delta: isize = 0;
        let mut covered_cols: Vec<u32> = Vec::with_capacity(columns.len());
        for col_res in &columns {
            net_delta += col_res.len_delta;
            covered_cols.push(col_res.col);
        }
        covered_cols.sort_unstable();
        for col_res in &columns {
            let col = col_res.col;
            for &(pi, mask) in &col_res.vacated_masks {
                cleared.or_page_mask(pi, col, mask);
            }
        }
        if had_cleared {
            for col_res in &columns {
                let col = col_res.col;
                for &(pi, mask) in &col_res.occupied_masks {
                    cleared.clear_page_mask(pi, col, mask);
                }
            }
        }

        if p_hi < DIRECT_PAGE_LIMIT {
            let needed = p_hi as usize + 1;
            if self.pages.len() < needed {
                self.pages.resize(needed, None);
            }
            let needed_w = (p_hi / 64) as usize + 1;
            if self.page_bits.len() < needed_w {
                self.page_bits.resize(needed_w, 0);
            }
        }

        for idx in 0..n_pages {
            let pi = p_lo + idx as u32;
            let mut max_active_col: Option<u32> = None;
            for col_res in &columns {
                if col_res.slabs[idx].is_some() {
                    let c = col_res.col;
                    max_active_col = Some(max_active_col.map_or(c, |m| m.max(c)));
                }
            }

            let has_untouched = self
                .page(pi)
                .is_some_and(|p| p.cols.has_any_outside_sorted(&covered_cols));

            if !has_untouched {
                let Some(last_col) = max_active_col else {
                    if self.page(pi).is_some() {
                        self.remove_page(pi);
                    }
                    continue;
                };
                let mut row_mask = [0u64; 4];
                let mut n_rows = 0u32;
                let mut may_have_formula = false;
                let mut may_have_qualified_formula = false;
                let cols = if last_col < DENSE_COL_LIMIT {
                    let mut slots = vec![None; last_col as usize + 1];
                    let mut nonempty = [0u64; 4];
                    for col_res in &mut columns {
                        if let Some(slab) = col_res.slabs[idx].take() {
                            let c = col_res.col as usize;
                            n_rows = n_rows.max(slab.cells.len() as u32);
                            row_mask[0] |= slab.row_mask[0];
                            row_mask[1] |= slab.row_mask[1];
                            row_mask[2] |= slab.row_mask[2];
                            row_mask[3] |= slab.row_mask[3];
                            nonempty[c / 64] |= 1u64 << (c % 64);
                            may_have_formula |= col_res.has_formula;
                            may_have_qualified_formula |= col_res.has_qual_formula;
                            slots[c] = Some(slab.cells);
                        }
                    }
                    PageCols::Dense { nonempty, slots }
                } else {
                    let mut pc = PageCols::default();
                    for col_res in &mut columns {
                        if let Some(slab) = col_res.slabs[idx].take() {
                            n_rows = n_rows.max(slab.cells.len() as u32);
                            row_mask[0] |= slab.row_mask[0];
                            row_mask[1] |= slab.row_mask[1];
                            row_mask[2] |= slab.row_mask[2];
                            row_mask[3] |= slab.row_mask[3];
                            may_have_formula |= col_res.has_formula;
                            may_have_qualified_formula |= col_res.has_qual_formula;
                            pc.set(col_res.col, slab.cells);
                        }
                    }
                    pc
                };
                self.insert_page_arc(
                    pi,
                    Arc::new(Page {
                        may_have_qualified_formula,
                        may_have_formula,
                        n_rows,
                        row_mask,
                        cols,
                    }),
                );
            } else {
                let page = self.page_mut_or_insert(pi);
                for col_res in &mut columns {
                    let col = col_res.col;
                    match col_res.slabs[idx].take() {
                        Some(slab) => {
                            page.cols.set(col, slab.cells);
                            page.may_have_formula |= col_res.has_formula;
                            page.may_have_qualified_formula |= col_res.has_qual_formula;
                        }
                        None => {
                            page.cols.remove(col);
                        }
                    }
                }
                page.rebuild_masks();
                if page.is_empty() {
                    self.remove_page(pi);
                }
            }
        }

        self.len = (self.len as isize + net_delta).max(0) as usize;
        self.bounds.take();
        self.formula_count.take();
        self.version.advance();
    }

    /// Parallel columnar gather of rows `r1..=r2` across columns `c1..=c2` using `order`
    /// (`dst = r1 + i` receives source row `order[i]`), producing typed `ColSlab::Numbers` /
    /// `ColSlab::Values` / `ColSlab::Mixed` directly and translating formulas in parallel.
    pub(crate) fn gather_rows(
        &mut self,
        source: &Store,
        region: (u32, u32, u32, u32),
        order: &[u32],
        sheet_name: &str,
        cleared: &mut CellJournal,
    ) -> usize {
        let (r1, c1, r2, c2) = region;
        if r1 > r2 || c1 > c2 || order.is_empty() {
            return 0;
        }
        let p_lo = r1 / PAGE_ROWS;
        let p_hi = r2 / PAGE_ROWS;
        let n_pages = (p_hi - p_lo + 1) as usize;
        let cols: Vec<u32> = (c1..=c2)
            .filter(|&col| source.col_has_any_in(col, r1, r2) || self.col_has_any_in(col, r1, r2))
            .collect();
        if cols.is_empty() {
            return 0;
        }

        let track_cleared = !cleared.is_empty();
        let all_src_slabs: Vec<Vec<Option<&ColSlab>>> = cols
            .iter()
            .map(|&col| source.col_page_slices(col, p_lo, p_hi))
            .collect();
        let all_dst_slabs: Vec<Vec<Option<&ColSlab>>> = cols
            .iter()
            .map(|&col| self.col_page_slices(col, p_lo, p_hi))
            .collect();
        let col_has_formula_flags: Vec<bool> = (0..cols.len())
            .map(|ci| {
                all_src_slabs[ci]
                    .iter()
                    .any(|s| s.is_some_and(ColSlab::is_mixed))
                    || all_dst_slabs[ci]
                        .iter()
                        .any(|s| s.is_some_and(ColSlab::is_mixed))
            })
            .collect();
        let col_src_numbers_flags: Vec<bool> = (0..cols.len())
            .map(|ci| {
                !col_has_formula_flags[ci]
                    && all_src_slabs[ci]
                        .iter()
                        .all(|s| s.is_none_or(ColSlab::is_numbers))
            })
            .collect();

        struct BandResult {
            col_idx: usize,
            page_start: usize,
            slabs: Vec<Option<GatheredSlab>>,
            len_delta: isize,
            has_formula: bool,
            has_qual_formula: bool,
            vacated_masks: Vec<(u32, [u64; 4])>,
            occupied_masks: Vec<(u32, [u64; 4])>,
            changed_cells: usize,
        }

        let build_band = |col_idx: usize, page_start: usize, page_end: usize| -> BandResult {
            let src_slabs = &all_src_slabs[col_idx];
            let dst_old_slabs = &all_dst_slabs[col_idx];
            let col_has_formula = col_has_formula_flags[col_idx];
            let src_all_numbers = col_src_numbers_flags[col_idx];
            let mut fill_cache = col_has_formula.then(crate::refs::FillFormulas::default);
            let mut slabs: Vec<Option<GatheredSlab>> = Vec::with_capacity(page_end - page_start);
            let mut vacated_masks = Vec::new();
            let mut occupied_masks = Vec::new();
            let mut len_delta: isize = 0;
            let mut has_formula = false;
            let mut has_qual_formula = false;
            let mut changed_cells = 0usize;

            for page_idx in page_start..page_end {
                let pi = p_lo + page_idx as u32;
                let base = pi * PAGE_ROWS;
                let lo = r1.saturating_sub(base) as usize;
                let hi = (r2 - base).min(PAGE_ROWS - 1) as usize;
                let old_slab = dst_old_slabs[page_idx];
                let old_len = old_slab.map_or(0, ColSlab::len);
                let push_mode = lo == 0 && hi + 1 >= old_len;

                // Fast path 1: pure numeric column -> direct `ColSlab::Numbers` (8 B/cell, zero Cell/Value)
                if src_all_numbers && push_mode {
                    let mut vals = Vec::with_capacity(hi + 1);
                    let mut present = [0u64; 4];
                    let mut vac_mask = [0u64; 4];
                    let old_count = old_slab.map_or(0, ColSlab::occupied_count) as isize;
                    let mut new_count: isize = 0;
                    let old_ns = match old_slab {
                        Some(ColSlab::Numbers(ns)) => Some(ns),
                        _ => None,
                    };
                    let page_order =
                        &order[(base - r1) as usize..(base - r1) as usize + (hi + 1)];
                    for (r_off, &src_r) in page_order.iter().enumerate() {
                        let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                        let so = src_r % PAGE_ROWS;
                        let src_num = match src_slabs[sp] {
                            Some(ColSlab::Numbers(ns)) => ns.get(so),
                            _ => None,
                        };
                        if let Some(n) = src_num {
                            let same = if let Some(ons) = old_ns {
                                ons.get(r_off as u32) == Some(n)
                            } else {
                                old_slab
                                    .and_then(|s| s.get_view(r_off as u32))
                                    .is_some_and(|v| v.as_f64() == Some(n) && !v.has_formula())
                            };
                            if !same {
                                changed_cells += 1;
                            }
                            new_count += 1;
                            present[r_off / 64] |= 1u64 << (r_off % 64);
                            vals.push(n);
                        } else {
                            let had_old = if let Some(ons) = old_ns {
                                ons.contains(r_off as u32)
                            } else {
                                old_slab.is_some_and(|s| s.contains(r_off as u32))
                            };
                            if had_old {
                                changed_cells += 1;
                                vac_mask[r_off / 64] |= 1u64 << (r_off % 64);
                            }
                            vals.push(0.0);
                        }
                    }
                    vals.truncate(highest_bit_plus_one(&present));
                    len_delta += new_count - old_count;
                    if vac_mask != [0; 4] {
                        vacated_masks.push((pi, vac_mask));
                    }
                    if track_cleared && present != [0; 4] {
                        occupied_masks.push((pi, present));
                    }
                    slabs.push((present != [0; 4]).then(|| GatheredSlab {
                        cells: Arc::new(ColSlab::Numbers(NumberSlab {
                            present,
                            values: vals,
                        })),
                        row_mask: present,
                    }));
                    continue;
                }

                // Fast path 2: non-formula value column -> direct `ColSlab::Values` (24 B/cell, zero Cell)
                if !col_has_formula && push_mode {
                    let mut out_vals = Vec::with_capacity(hi + 1);
                    let old_count = old_slab.map_or(0, ColSlab::occupied_count) as isize;
                    let mut new_count: isize = 0;
                    let mut row_mask = [0u64; 4];
                    let mut vac_mask = [0u64; 4];
                    let page_order =
                        &order[(base - r1) as usize..(base - r1) as usize + (hi + 1)];
                    for (r_off, &src_r) in page_order.iter().enumerate() {
                        let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                        let so = src_r % PAGE_ROWS;
                        let src_view = src_slabs[sp].and_then(|s| s.get_view(so));
                        let old_view = old_slab.and_then(|s| s.get_view(r_off as u32));
                        if let Some(sv) = src_view {
                            let val = sv.value().into_owned();
                            let differs = match old_view {
                                Some(ov) => ov.value().as_ref() != &val,
                                None => true,
                            };
                            if differs {
                                changed_cells += 1;
                            }
                            new_count += 1;
                            row_mask[r_off / 64] |= 1u64 << (r_off % 64);
                            out_vals.push(val);
                        } else {
                            if old_view.is_some() {
                                changed_cells += 1;
                                vac_mask[r_off / 64] |= 1u64 << (r_off % 64);
                            }
                            out_vals.push(Value::Empty);
                        }
                    }
                    while out_vals.last().is_some_and(Value::is_empty) {
                        out_vals.pop();
                    }
                    len_delta += new_count - old_count;
                    if vac_mask != [0; 4] {
                        vacated_masks.push((pi, vac_mask));
                    }
                    if track_cleared && row_mask != [0; 4] {
                        occupied_masks.push((pi, row_mask));
                    }
                    slabs.push((!out_vals.is_empty()).then(|| GatheredSlab {
                        cells: Arc::new(ColSlab::Values(out_vals)),
                        row_mask,
                    }));
                    continue;
                }

                // Fast path 3: partial page on non-formula column -> preserve Numbers or Values!
                if !col_has_formula {
                    let old_count = old_slab.map_or(0, ColSlab::occupied_count) as isize;
                    let target_len = old_len.max(hi + 1);
                    let keep_numbers = src_all_numbers
                        && old_slab.is_none_or(ColSlab::is_numbers);
                    if keep_numbers {
                        let mut vals = vec![0.0; target_len];
                        let mut present = [0u64; 4];
                        let mut new_count: isize = 0;
                        if let Some(ColSlab::Numbers(ons)) = old_slab {
                            for r in 0..lo.min(ons.values.len()) {
                                if ons.contains(r as u32) {
                                    vals[r] = ons.values[r];
                                    present[r / 64] |= 1u64 << (r % 64);
                                    new_count += 1;
                                }
                            }
                            for r in (hi + 1)..ons.values.len() {
                                if ons.contains(r as u32) {
                                    vals[r] = ons.values[r];
                                    present[r / 64] |= 1u64 << (r % 64);
                                    new_count += 1;
                                }
                            }
                        }
                        let mut vac_mask = [0u64; 4];
                        let mut occ_mask = [0u64; 4];
                        let page_order = &order[(base + lo as u32 - r1) as usize
                            ..(base + hi as u32 - r1) as usize + 1];
                        for (i, &src_r) in page_order.iter().enumerate() {
                            let r_off = lo + i;
                            let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                            let so = src_r % PAGE_ROWS;
                            let src_num = match src_slabs[sp] {
                                Some(ColSlab::Numbers(ns)) => ns.get(so),
                                _ => None,
                            };
                            let old_num = match old_slab {
                                Some(ColSlab::Numbers(ons)) => ons.get(r_off as u32),
                                _ => None,
                            };
                            if let Some(n) = src_num {
                                if old_num != Some(n) {
                                    changed_cells += 1;
                                }
                                new_count += 1;
                                present[r_off / 64] |= 1u64 << (r_off % 64);
                                if track_cleared {
                                    occ_mask[r_off / 64] |= 1u64 << (r_off % 64);
                                }
                                vals[r_off] = n;
                            } else if old_num.is_some() {
                                changed_cells += 1;
                                vac_mask[r_off / 64] |= 1u64 << (r_off % 64);
                            }
                        }
                        vals.truncate(highest_bit_plus_one(&present));
                        len_delta += new_count - old_count;
                        if vac_mask != [0; 4] {
                            vacated_masks.push((pi, vac_mask));
                        }
                        if track_cleared && occ_mask != [0; 4] {
                            occupied_masks.push((pi, occ_mask));
                        }
                        slabs.push((present != [0; 4]).then(|| GatheredSlab {
                            cells: Arc::new(ColSlab::Numbers(NumberSlab {
                                present,
                                values: vals,
                            })),
                            row_mask: present,
                        }));
                    } else {
                        let mut out_vals = vec![Value::Empty; target_len];
                        let mut new_count: isize = 0;
                        let mut row_mask = [0u64; 4];
                        if let Some(os) = old_slab {
                            for r in 0..lo.min(old_len) {
                                if let Some(v) = os.get_view(r as u32) {
                                    out_vals[r] = v.value().into_owned();
                                    row_mask[r / 64] |= 1u64 << (r % 64);
                                    new_count += 1;
                                }
                            }
                            for r in (hi + 1)..old_len {
                                if let Some(v) = os.get_view(r as u32) {
                                    out_vals[r] = v.value().into_owned();
                                    row_mask[r / 64] |= 1u64 << (r % 64);
                                    new_count += 1;
                                }
                            }
                        }
                        let mut vac_mask = [0u64; 4];
                        let mut occ_mask = [0u64; 4];
                        let page_order = &order[(base + lo as u32 - r1) as usize
                            ..(base + hi as u32 - r1) as usize + 1];
                        for (i, &src_r) in page_order.iter().enumerate() {
                            let r_off = lo + i;
                            let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                            let so = src_r % PAGE_ROWS;
                            let src_view = src_slabs[sp].and_then(|s| s.get_view(so));
                            let old_view = old_slab.and_then(|s| s.get_view(r_off as u32));
                            if let Some(sv) = src_view {
                                let val = sv.value().into_owned();
                                if old_view.is_none_or(|ov| ov.value().as_ref() != &val) {
                                    changed_cells += 1;
                                }
                                new_count += 1;
                                row_mask[r_off / 64] |= 1u64 << (r_off % 64);
                                if track_cleared {
                                    occ_mask[r_off / 64] |= 1u64 << (r_off % 64);
                                }
                                out_vals[r_off] = val;
                            } else if old_view.is_some() {
                                changed_cells += 1;
                                vac_mask[r_off / 64] |= 1u64 << (r_off % 64);
                            }
                        }
                        while out_vals.last().is_some_and(Value::is_empty) {
                            out_vals.pop();
                        }
                        len_delta += new_count - old_count;
                        if vac_mask != [0; 4] {
                            vacated_masks.push((pi, vac_mask));
                        }
                        if track_cleared && occ_mask != [0; 4] {
                            occupied_masks.push((pi, occ_mask));
                        }
                        slabs.push((!out_vals.is_empty()).then(|| GatheredSlab {
                            cells: Arc::new(ColSlab::Values(out_vals)),
                            row_mask,
                        }));
                    }
                    continue;
                }

                // General formula-bearing column path -> `ColSlab::Mixed`
                let old_count = old_slab.map_or(0, ColSlab::occupied_count) as isize;
                let target_len = old_len.max(hi + 1);
                let mut out_vec: Vec<Cell>;
                let mut new_count: isize = 0;
                let mut row_mask = [0u64; 4];
                if push_mode {
                    out_vec = Vec::with_capacity(target_len);
                } else {
                    out_vec = vec![Cell::default(); target_len];
                    if let Some(os) = old_slab {
                        for r in 0..lo.min(old_len) {
                            if let Some(v) = os.get_view(r as u32) {
                                if let Some(f) = v.formula() {
                                    has_formula = true;
                                    if f.contains('!') {
                                        has_qual_formula = true;
                                    }
                                }
                                new_count += 1;
                                row_mask[r / 64] |= 1u64 << (r % 64);
                                out_vec[r] = v.to_cell();
                            }
                        }
                        for r in (hi + 1)..old_len {
                            if let Some(v) = os.get_view(r as u32) {
                                if let Some(f) = v.formula() {
                                    has_formula = true;
                                    if f.contains('!') {
                                        has_qual_formula = true;
                                    }
                                }
                                new_count += 1;
                                row_mask[r / 64] |= 1u64 << (r % 64);
                                out_vec[r] = v.to_cell();
                            }
                        }
                    }
                }

                let mut vac_mask = [0u64; 4];
                let mut occ_mask = [0u64; 4];
                let page_order = &order[(base + lo as u32 - r1) as usize
                    ..(base + hi as u32 - r1) as usize + 1];
                for (i, &src_r) in page_order.iter().enumerate() {
                    let r_off = lo + i;
                    let dst_r = base + r_off as u32;
                    let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                    let so = src_r % PAGE_ROWS;
                    let src_view = src_slabs[sp].and_then(|s| s.get_view(so));
                    let old_view = old_slab.and_then(|s| s.get_view(r_off as u32));
                    if let Some(sv) = src_view {
                        let mut cell = sv.to_cell();
                        if dst_r != src_r
                            && let Some(f) = cell.formula.as_deref()
                        {
                            let dr = i64::from(dst_r) - i64::from(src_r);
                            let nf: Arc<str> = if !f.contains('!') {
                                fill_cache
                                    .as_mut()
                                    .expect("fill_cache initialized when col_has_formula")
                                    .translate(f, dr, 0)
                                    .into()
                            } else {
                                crate::refs::translate(f, dr, 0, sheet_name).into()
                            };
                            cell.formula = Some(nf);
                        }
                        if old_view.is_none_or(|ov| !ov.eq_cell(&cell)) {
                            changed_cells += 1;
                        }
                        if let Some(f) = cell.formula.as_deref() {
                            has_formula = true;
                            if f.contains('!') {
                                has_qual_formula = true;
                            }
                        }
                        new_count += 1;
                        row_mask[r_off / 64] |= 1u64 << (r_off % 64);
                        if track_cleared {
                            occ_mask[r_off / 64] |= 1u64 << (r_off % 64);
                        }
                        if push_mode {
                            out_vec.push(cell);
                        } else {
                            out_vec[r_off] = cell;
                        }
                    } else {
                        if old_view.is_some() {
                            changed_cells += 1;
                            vac_mask[r_off / 64] |= 1u64 << (r_off % 64);
                        }
                        if push_mode {
                            out_vec.push(Cell::default());
                        }
                    }
                }

                while out_vec.last().is_some_and(Cell::is_blank) {
                    out_vec.pop();
                }
                len_delta += new_count - old_count;
                if vac_mask != [0; 4] {
                    vacated_masks.push((pi, vac_mask));
                }
                if track_cleared && occ_mask != [0; 4] {
                    occupied_masks.push((pi, occ_mask));
                }
                slabs.push((!out_vec.is_empty()).then(|| GatheredSlab {
                    cells: Arc::new(ColSlab::Mixed(out_vec)),
                    row_mask,
                }));
            }

            BandResult {
                col_idx,
                page_start,
                slabs,
                len_delta,
                has_formula,
                has_qual_formula,
                vacated_masks,
                occupied_masks,
                changed_cells,
            }
        };

        let area = u64::from(r2 - r1 + 1) * cols.len() as u64;
        let (gathered, total_changed): (Vec<GatheredColumn>, usize) = if area >= 16_384 {
            let avail = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(16);
            let bands = if n_pages >= 64 {
                (n_pages / 64).clamp(1, 8)
            } else {
                1
            };
            let band_pages = n_pages.div_ceil(bands);
            let mut tasks: Vec<(usize, usize, usize)> = Vec::with_capacity(cols.len() * bands);
            for col_idx in 0..cols.len() {
                let mut ps = 0;
                while ps < n_pages {
                    let pe = (ps + band_pages).min(n_pages);
                    tasks.push((col_idx, ps, pe));
                    ps = pe;
                }
            }
            let n_threads = avail.min(tasks.len());
            let next_task = std::sync::atomic::AtomicUsize::new(0);
            let mut per_col: Vec<Vec<BandResult>> = (0..cols.len()).map(|_| Vec::new()).collect();
            let mut sum_changed = 0usize;
            std::thread::scope(|s| {
                let mut handles = Vec::with_capacity(n_threads);
                let tasks_ref = &tasks;
                let next_ref = &next_task;
                let build_ref = &build_band;
                for _ in 0..n_threads {
                    handles.push(s.spawn(move || {
                        let mut local = Vec::new();
                        loop {
                            let t = next_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if t >= tasks_ref.len() {
                                break;
                            }
                            let (ci, ps, pe) = tasks_ref[t];
                            local.push(build_ref(ci, ps, pe));
                        }
                        local
                    }));
                }
                for h in handles {
                    if let Ok(local) = h.join() {
                        for br in local {
                            sum_changed += br.changed_cells;
                            per_col[br.col_idx].push(br);
                        }
                    }
                }
            });
            let mut all_cols = Vec::with_capacity(cols.len());
            for (col_idx, mut bands_res) in per_col.into_iter().enumerate() {
                bands_res.sort_unstable_by_key(|b| b.page_start);
                let mut slabs = Vec::with_capacity(n_pages);
                let mut len_delta = 0isize;
                let mut has_formula = false;
                let mut has_qual_formula = false;
                let mut vacated_masks = Vec::new();
                let mut occupied_masks = Vec::new();
                for br in bands_res {
                    slabs.extend(br.slabs);
                    len_delta += br.len_delta;
                    has_formula |= br.has_formula;
                    has_qual_formula |= br.has_qual_formula;
                    vacated_masks.extend(br.vacated_masks);
                    occupied_masks.extend(br.occupied_masks);
                }
                all_cols.push(GatheredColumn {
                    col: cols[col_idx],
                    slabs,
                    len_delta,
                    has_formula,
                    has_qual_formula,
                    vacated_masks,
                    occupied_masks,
                });
            }
            (all_cols, sum_changed)
        } else {
            let mut all_cols = Vec::with_capacity(cols.len());
            let mut sum_changed = 0usize;
            for col_idx in 0..cols.len() {
                let br = build_band(col_idx, 0, n_pages);
                sum_changed += br.changed_cells;
                all_cols.push(GatheredColumn {
                    col: cols[col_idx],
                    slabs: br.slabs,
                    len_delta: br.len_delta,
                    has_formula: br.has_formula,
                    has_qual_formula: br.has_qual_formula,
                    vacated_masks: br.vacated_masks,
                    occupied_masks: br.occupied_masks,
                });
            }
            (all_cols, sum_changed)
        };

        self.install_gathered_columns(p_lo, p_hi, gathered, cleared);
        total_changed
    }

    pub fn unique_payload_bytes(&self) -> usize {
        let mut seen_pages = std::collections::HashSet::new();
        let mut seen_slabs = std::collections::HashSet::new();
        let mut bytes = self.pages.capacity() * std::mem::size_of::<Option<Arc<Page>>>()
            + self.page_bits.capacity() * std::mem::size_of::<u64>();
        for (_, page) in self.iter_pages() {
            if seen_pages.insert(Arc::as_ptr(page) as usize) {
                bytes += std::mem::size_of::<Page>() + page.cols.payload_bytes();
                for (_, slab) in page.cols.iter_entries() {
                    if seen_slabs.insert(Arc::as_ptr(slab) as usize) {
                        bytes += slab.payload_bytes();
                    }
                }
            }
        }
        bytes
    }

    fn invalidate_boundary(cache: &mut OnceLock<Option<(u32, u32, u32, u32)>>, r: u32, c: u32) {
        if cache
            .get()
            .copied()
            .flatten()
            .is_some_and(|(r1, c1, r2, c2)| r == r1 || r == r2 || c == c1 || c == c2)
        {
            cache.take();
        }
    }

    pub fn used_bounds(&self) -> Option<(u32, u32, u32, u32)> {
        if self.len == 0 {
            return None;
        }
        *self.bounds.get_or_init(|| {
            let mut min_r = u32::MAX;
            let mut min_c = u32::MAX;
            let mut max_r = 0u32;
            let mut max_c = 0u32;
            let mut found = false;
            for (pi, page) in self.iter_pages() {
                let base = pi * PAGE_ROWS;
                for (c, slab) in page.cols.iter_entries() {
                    if let Some((first_i, last_i)) = slab.first_last_occupied() {
                        min_r = min_r.min(base + first_i);
                        max_r = max_r.max(base + last_i);
                        min_c = min_c.min(c);
                        max_c = max_c.max(c);
                        found = true;
                    }
                }
            }
            found.then_some((min_r, min_c, max_r, max_c))
        })
    }

    /// 实际占用的字节数（调试用）
    pub fn allocated_bytes(&self) -> usize {
        let mut n = self.pages.capacity() * std::mem::size_of::<Option<Arc<Page>>>()
            + self.page_bits.capacity() * std::mem::size_of::<u64>();
        for (_, page) in self.iter_pages() {
            n += std::mem::size_of::<Page>() + page.cols.payload_bytes();
            for (_, col) in page.cols.iter_entries() {
                n += col.payload_bytes();
            }
        }
        n
    }

    /// 稀疏保护：某页实际使用的格子占比过低时报告出来（调参用）
    pub fn dense_stats(&self) -> (usize, usize) {
        let used = self.len;
        let alloc: usize = self
            .iter_pages()
            .map(|(_, p)| p.cols.iter_entries().map(|(_, c)| c.len()).sum::<usize>())
            .sum();
        (used, alloc)
    }
}

struct PageRectViewsIter<'a> {
    base: u32,
    cols: Vec<(u32, &'a ColSlab)>,
    row_words: [u64; 4],
    word: usize,
    cur_ro: u32,
    col_idx: usize,
}

impl<'a> Iterator for PageRectViewsIter<'a> {
    type Item = (CellRef, CellView<'a>);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            while self.col_idx < self.cols.len() {
                let (col, slab) = self.cols[self.col_idx];
                self.col_idx += 1;
                if let Some(view) = slab.get_view(self.cur_ro) {
                    return Some((CellRef::new(self.base + self.cur_ro, col), view));
                }
            }
            while self.word < 4 && self.row_words[self.word] == 0 {
                self.word += 1;
            }
            if self.word >= 4 {
                return None;
            }
            let bits = &mut self.row_words[self.word];
            let tz = bits.trailing_zeros();
            *bits &= *bits - 1;
            self.cur_ro = self.word as u32 * 64 + tz;
            self.col_idx = 0;
        }
    }
}

struct PageWithExplicitStyleIter<'a> {
    base: u32,
    cols: Vec<(u32, &'a ColSlab, Option<&'a StyleColumn>)>,
    row_words: [u64; 4],
    word: usize,
    cur_ro: u32,
    col_idx: usize,
}

impl<'a> Iterator for PageWithExplicitStyleIter<'a> {
    type Item = (CellRef, CellView<'a>, Option<u16>);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            while self.col_idx < self.cols.len() {
                let (col, slab, st) = self.cols[self.col_idx];
                self.col_idx += 1;
                if let Some(view) = slab.get_view(self.cur_ro) {
                    let sid = st.and_then(|s| s.get(self.cur_ro).copied());
                    return Some((CellRef::new(self.base + self.cur_ro, col), view, sid));
                }
            }
            while self.word < 4 && self.row_words[self.word] == 0 {
                self.word += 1;
            }
            if self.word >= 4 {
                return None;
            }
            let bits = &mut self.row_words[self.word];
            let tz = bits.trailing_zeros();
            *bits &= *bits - 1;
            self.cur_ro = self.word as u32 * 64 + tz;
            self.col_idx = 0;
        }
    }
}

struct PageMergedStylesIter<'a> {
    base: u32,
    col_views: Vec<(u32, Option<&'a ColSlab>, Option<&'a StyleColumn>)>,
    row_words: [u64; 4],
    word: usize,
    cur_ro: u32,
    col_idx: usize,
}

impl<'a> Iterator for PageMergedStylesIter<'a> {
    type Item = (CellRef, Option<CellView<'a>>, Option<u16>);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            while self.col_idx < self.col_views.len() {
                let (col, slab, sc) = self.col_views[self.col_idx];
                self.col_idx += 1;
                let cell = slab.and_then(|s| s.get_view(self.cur_ro));
                let sid = sc.and_then(|s| s.get(self.cur_ro).copied());
                if cell.is_some() || sid.is_some() {
                    return Some((CellRef::new(self.base + self.cur_ro, col), cell, sid));
                }
            }
            while self.word < 4 && self.row_words[self.word] == 0 {
                self.word += 1;
            }
            if self.word >= 4 {
                return None;
            }
            let bits = &mut self.row_words[self.word];
            let tz = bits.trailing_zeros();
            *bits &= *bits - 1;
            self.cur_ro = self.word as u32 * 64 + tz;
            self.col_idx = 0;
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = Value::Empty;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(v: f64) -> Cell {
        Cell {
            value: Value::Number(v),
            formula: None,
        }
    }

    #[test]
    fn packed_style_pages_keep_explicit_defaults_and_snapshot_isolation() {
        let mut styles = CellStyles::default();
        for row in [513, 255, 63, 64, 0, 128, 127, 192] {
            styles.insert(CellRef::new(row, 80), row as u16);
        }
        let saved = styles.clone();
        let key = CellRef::new(64, 80);
        assert_eq!(styles.remove(&key), Some(64));
        assert_eq!(styles.insert(key, u16::MAX), None);
        assert_eq!(styles.get(&key), Some(&u16::MAX));
        assert_eq!(saved.get(&key), Some(&64));
        assert_eq!(styles.get(&CellRef::new(0, 80)), Some(&0));
        assert_eq!(styles.changed_keys(&saved).collect::<Vec<_>>(), vec![key]);
        assert_eq!(styles.iter().count(), styles.len());
        assert!(Arc::ptr_eq(
            styles.page_col(2, 80).unwrap(),
            saved.page_col(2, 80).unwrap()
        ));
    }

    #[test]
    fn uniform_style_pages_compress_and_share_single_arc() {
        let mut styles = CellStyles::default();
        styles.insert_rect(0, 0, 1023, 3, 7);
        assert_eq!(styles.len(), 1024 * 4);
        assert_eq!(styles.page_count(), 16);
        assert_eq!(styles.uniform_page_count(), 16);
        assert!(Arc::ptr_eq(
            styles.page_col(0, 0).unwrap(),
            styles.page_col(3, 3).unwrap()
        ));
        assert_eq!(styles.get(&CellRef::new(500, 2)), Some(&7));
        assert_eq!(styles.max_coords(), Some((1023, 3)));
        // Mutating a single cell in one page expands only that page, preserving Uniform elsewhere.
        styles.insert(CellRef::new(10, 1), 9);
        assert_eq!(styles.get(&CellRef::new(10, 1)), Some(&9));
        assert_eq!(styles.get(&CellRef::new(11, 1)), Some(&7));
        assert_eq!(styles.uniform_page_count(), 15);
        // Restoring that cell back to 7 re-compacts the page to Uniform(7).
        styles.insert(CellRef::new(10, 1), 7);
        assert_eq!(styles.uniform_page_count(), 16);
    }

    #[test]
    fn distant_column_xfd_allocates_only_occupied_slabs() {
        let mut s = Store::default();
        for p in 0..120u32 {
            s.set(p * PAGE_ROWS, 16_383, num(p as f64));
        }
        let (pages, slabs, slots) = s.stats();
        assert_eq!(pages, 120);
        assert_eq!(slabs, 120, "远端列 XFD 不应给前 16383 个空列分配空 Vec/Arc");
        assert!(slots <= 480, "slots={slots}");
        assert_eq!(s.iter_rect((0, 0, 120 * PAGE_ROWS, 16_383)).count(), 120);
    }

    #[test]
    fn sparse_far_rows_do_not_allocate_full_page_slabs() {
        let mut s = Store::default();
        for i in 0..10_000u32 {
            s.set(i * 104, 3, num(i as f64));
        }
        assert_eq!(s.len(), 10_000);
        let (pages, _slabs, slots) = s.stats();
        assert!(pages > 1000, "页数不真实：{pages}");
        assert!(
            slots < 1_200_000,
            "稀疏写入的槽位数仍然过大：{slots}（页 {pages}）"
        );
        assert_eq!(s.get(0, 3).unwrap().value.clone(), Value::Number(0.0));
        assert_eq!(
            s.get(104 * 9_999, 3).unwrap().value.clone(),
            Value::Number(9_999.0)
        );
        let n = s.iter().count();
        assert_eq!(n, 10_000);
    }

    #[test]
    fn dense_and_sparse_columns_coexist_in_one_page() {
        let mut s = Store::default();
        for r in 0..256u32 {
            s.set(r, 0, num(r as f64));
        }
        s.set(200, 3, num(42.0));
        assert_eq!(s.get(200, 3).unwrap().value.clone(), Value::Number(42.0));
        assert!(s.get(201, 3).is_none());
        assert_eq!(s.get(255, 0).unwrap().value.clone(), Value::Number(255.0));
        assert_eq!(s.iter().count(), 257);
    }

    #[test]
    fn clone_shares_columns_until_written() {
        let mut s = Store::default();
        for r in 0..256u32 {
            s.set(r, 0, num(r as f64));
        }
        let clone = s.clone();
        assert!(Arc::ptr_eq(
            s.page(0).unwrap(),
            clone.page(0).unwrap(),
        ));
        let mut clone = clone;
        clone.set(10, 0, num(999.0));
        assert_eq!(clone.get(10, 0).unwrap().value.clone(), Value::Number(999.0));
        assert_eq!(s.get(10, 0).unwrap().value.clone(), Value::Number(10.0));
        assert!(!Arc::ptr_eq(
            s.page(0).unwrap().cols.get(0).unwrap(),
            clone.page(0).unwrap().cols.get(0).unwrap(),
        ));
    }

    #[test]
    fn occupied_bounds_stay_cached_across_reads_and_independent_snapshots() {
        let mut store = Store::default();
        for r in 0..500u32 {
            store.set(r, (r % 4) + 2, num(r as f64));
        }
        assert_eq!(store.used_bounds(), Some((0, 2, 499, 5)));
        let mut fork = store.clone();
        fork.set(500, 1, num(1.0));
        assert_eq!(store.used_bounds(), Some((0, 2, 499, 5)));
        assert_eq!(fork.used_bounds(), Some((0, 1, 500, 5)));

        // Non-boundary edits keep the cached bounds intact; boundary removal recomputes lazily.
        fork.remove(250, 4);
        assert!(fork.bounds.get().is_some());
        assert_eq!(fork.used_bounds(), Some((0, 1, 500, 5)));

        fork.remove(500, 1);
        assert!(fork.bounds.get().is_none());
        assert_eq!(fork.used_bounds(), Some((0, 2, 499, 5)));

        let mut blank_boundary = fork.clone();
        blank_boundary.set_formula_cache(499, 5, Value::Empty);
        assert_eq!(blank_boundary.used_bounds(), Some((0, 2, 498, 5)));
        assert_eq!(fork.used_bounds(), Some((0, 2, 499, 5)));

        let mut in_place = fork.clone();
        *in_place.get_mut(0, 2).unwrap() = Cell::default();
        assert_eq!(in_place.used_bounds(), Some((1, 2, 499, 5)));
    }

    #[test]
    fn cell_styles_remove_rect_clears_full_partial_and_uniform_pages() {
        let mut styles = CellStyles::default();
        styles.insert_rect(0, 0, 1023, 3, 7);
        styles.insert(CellRef::new(500_000, 100), 12);
        let mut journal = CellJournal::default();

        // Partial slice out of uniform pages splits only the touched range.
        let removed = styles.remove_rect_recording(100, 1, 600, 2, &mut journal);
        assert_eq!(removed, (600 - 100 + 1) * 2);
        assert_eq!(journal.len(), removed);
        assert_eq!(styles.get(&CellRef::new(99, 1)), Some(&7));
        assert_eq!(styles.get(&CellRef::new(100, 1)), None);
        assert_eq!(styles.get(&CellRef::new(600, 2)), None);
        assert_eq!(styles.get(&CellRef::new(601, 2)), Some(&7));
        assert_eq!(styles.get(&CellRef::new(300, 0)), Some(&7));
        assert_eq!(styles.get(&CellRef::new(300, 3)), Some(&7));
        assert!(journal.contains(&CellRef::new(100, 1)));
        assert!(journal.contains(&CellRef::new(600, 2)));
        assert!(!journal.contains(&CellRef::new(99, 1)));
        assert!(!journal.contains(&CellRef::new(300, 0)));
        assert_eq!(styles.iter().count(), styles.len());

        // Full-sheet clear completes instantly without enumerating 1.7e10 empty coords.
        let rest = styles.len();
        let cleared_all = styles.remove_rect(0, 0, 1_048_575, 16_383);
        assert_eq!(cleared_all, rest);
        assert!(styles.is_empty());
        assert_eq!(styles.page_count(), 0);
        assert_eq!(styles.max_coords(), None);
    }

    #[test]
    fn store_shift_col_band_rows_inserts_and_deletes_without_touching_neighbors() {
        let mut store = Store::default();
        for r in 0..600u32 {
            store.set(r, 0, num(r as f64));
            store.set(r, 1, num(10_000.0 + r as f64));
            store.set(r, 2, num(20_000.0 + r as f64));
        }
        let mut cleared = CellJournal::default();
        // Shift col 1 down by 5 starting at row 100 (crosses page boundary 256/512).
        store.shift_col_band_rows(1, 1, 100, 5, false, &mut cleared);
        assert_eq!(store.len(), 1800);
        assert_eq!(store.get(100, 0).unwrap().value, Value::Number(100.0));
        assert_eq!(store.get(100, 2).unwrap().value, Value::Number(20_100.0));
        for r in 100..105 {
            assert!(store.get(r, 1).is_none());
            assert!(cleared.contains(&CellRef::new(r, 1)));
        }
        assert_eq!(store.get(105, 1).unwrap().value, Value::Number(10_100.0));
        assert_eq!(store.get(604, 1).unwrap().value, Value::Number(10_599.0));

        // Delete 5 rows starting at row 100 in col 1 -> restores exact original layout.
        store.shift_col_band_rows(1, 1, 100, 5, true, &mut cleared);
        assert_eq!(store.len(), 1800);
        for r in 0..600u32 {
            assert_eq!(store.get(r, 1).unwrap().value, Value::Number(10_000.0 + r as f64));
        }
        assert!(store.get(600, 1).is_none());

        // Exact PAGE_ROWS=256 shift fast path.
        store.shift_col_band_rows(1, 1, 256, 256, false, &mut cleared);
        assert_eq!(store.len(), 1800);
        assert_eq!(store.get(255, 1).unwrap().value, Value::Number(10_255.0));
        assert!(store.get(256, 1).is_none());
        assert_eq!(store.get(512, 1).unwrap().value, Value::Number(10_256.0));
        assert_eq!(store.get(855, 1).unwrap().value, Value::Number(10_599.0));
    }

    #[test]
    fn wave3_typed_col_slab_rw_and_upgrades() {
        let mut store = Store::default();
        // 1) Pure numeric column stays ColSlab::Numbers (~8.3 B/cell)
        for r in 0..256u32 {
            if r == 7 {
                continue; // leave an empty gap
            }
            let v = if r == 0 { -0.0 } else { r as f64 * 1.5 };
            store.set(r, 0, num(v));
        }
        assert!(matches!(
            store.page(0).unwrap().cols.get(0).unwrap().as_ref(),
            ColSlab::Numbers(_)
        ));
        assert_eq!(store.get_f64(0, 0).map(f64::to_bits), Some((-0.0f64).to_bits()));
        assert_eq!(store.get_f64(7, 0), None);
        assert_eq!(store.get_f64(255, 0), Some(255.0 * 1.5));
        assert_eq!(store.get_f64_plain(255, 0), Some(255.0 * 1.5));
        assert_eq!(store.formula_of(255, 0), None);
        assert_eq!(store.formula_cells().count(), 0);
        let num_bytes = store.unique_payload_bytes();
        assert!(
            num_bytes < 2_800,
            "255-cell Numbers page should be ~2.2 KB, got {num_bytes} B"
        );

        // 2) Pure value (Text/Bool) column uses ColSlab::Values (~24 B/cell)
        let mut val_store = Store::default();
        for r in 0..256u32 {
            val_store.set(r, 0, Cell { value: Value::Bool(r % 2 == 0), formula: None });
        }
        assert!(matches!(
            val_store.page(0).unwrap().cols.get(0).unwrap().as_ref(),
            ColSlab::Values(_)
        ));
        let val_bytes = val_store.unique_payload_bytes();
        assert!(
            val_bytes < 7_200,
            "256-cell Values page should be ~6.4 KB (24 B/cell), got {val_bytes} B"
        );

        // 3) Upgrade Numbers -> Values preserves all existing numbers & gap
        store.set(7, 0, Cell { value: Value::Text("升级".into()), formula: None });
        assert!(matches!(
            store.page(0).unwrap().cols.get(0).unwrap().as_ref(),
            ColSlab::Values(_)
        ));
        assert_eq!(store.get_f64(0, 0).map(f64::to_bits), Some((-0.0f64).to_bits()));
        assert_eq!(store.value_ref(7, 0).as_ref(), &Value::Text("升级".into()));
        assert_eq!(store.get_f64(255, 0), Some(255.0 * 1.5));
        assert_eq!(store.formula_cells().count(), 0);

        // 4) Upgrade Values -> Mixed on formula write
        store.set(10, 0, Cell { value: Value::Number(42.0), formula: Some("=21*2".into()) });
        assert!(matches!(
            store.page(0).unwrap().cols.get(0).unwrap().as_ref(),
            ColSlab::Mixed(_)
        ));
        assert_eq!(store.formula_of(10, 0), Some("=21*2"));
        assert_eq!(store.get_f64(10, 0), Some(42.0));
        assert_eq!(store.get_f64_plain(10, 0), None);
        assert_eq!(store.value_ref(7, 0).as_ref(), &Value::Text("升级".into()));
        assert_eq!(store.formula_cells().count(), 1);

        // 5) Direct upgrade Numbers -> Mixed on formula write
        store.set(0, 1, num(5.0));
        assert!(matches!(
            store.page(0).unwrap().cols.get(1).unwrap().as_ref(),
            ColSlab::Numbers(_)
        ));
        store.set(1, 1, Cell { value: Value::Number(10.0), formula: Some("=B1*2".into()) });
        assert!(matches!(
            store.page(0).unwrap().cols.get(1).unwrap().as_ref(),
            ColSlab::Mixed(_)
        ));
        assert_eq!(store.get_f64_plain(0, 1), Some(5.0));
        assert_eq!(store.formula_of(1, 1), Some("=B1*2"));
        assert_eq!(store.formula_cells().count(), 2);
    }

    #[test]
    fn wave3_cow_snapshot_isolation_across_slabs_and_page_cols() {
        let mut store = Store::default();
        for r in 0..16u32 {
            store.set(r, 0, num(r as f64));
            store.set(r, 1, Cell { value: Value::Text("文本".into()), formula: None });
            store.set(r, 2, Cell { value: Value::Number(r as f64), formula: Some("=A1".into()) });
        }
        assert!(matches!(store.page(0).unwrap().cols, PageCols::Dense { .. }));

        // Fork Dense -> Sparse on clone; original remains Dense
        let mut sparse_fork = store.clone();
        assert!(Arc::ptr_eq(store.page(0).unwrap(), sparse_fork.page(0).unwrap()));
        sparse_fork.set(5, 256, num(256.0));
        sparse_fork.set(5, 16_383, num(16_383.0));
        assert!(matches!(store.page(0).unwrap().cols, PageCols::Dense { .. }));
        assert!(matches!(sparse_fork.page(0).unwrap().cols, PageCols::Sparse { .. }));
        assert_eq!(store.get_f64(5, 16_383), None);
        assert_eq!(sparse_fork.get_f64(5, 16_383), Some(16_383.0));
        assert_eq!(sparse_fork.get_f64(3, 0), Some(3.0));

        // Fork from Sparse, upgrade slabs & remove distant cols -> canonical downgrade back to Dense
        let snap_sparse = sparse_fork.clone();
        sparse_fork.set(0, 0, Cell { value: Value::Text("变文本".into()), formula: None });
        sparse_fork.set(0, 1, Cell { value: Value::Number(1.0), formula: Some("=1".into()) });
        sparse_fork.remove(5, 256);
        sparse_fork.remove(5, 16_383);
        assert!(matches!(sparse_fork.page(0).unwrap().cols, PageCols::Dense { .. }));
        assert!(matches!(snap_sparse.page(0).unwrap().cols, PageCols::Sparse { .. }));
        assert!(matches!(
            snap_sparse.page(0).unwrap().cols.get(0).unwrap().as_ref(),
            ColSlab::Numbers(_)
        ));
        assert!(matches!(
            snap_sparse.page(0).unwrap().cols.get(1).unwrap().as_ref(),
            ColSlab::Values(_)
        ));
        assert_eq!(snap_sparse.get_f64(5, 16_383), Some(16_383.0));
        assert_eq!(snap_sparse.get_f64(0, 0), Some(0.0));
        assert_eq!(sparse_fork.value_ref(0, 0).as_ref(), &Value::Text("变文本".into()));
        assert_eq!(sparse_fork.formula_of(0, 1), Some("=1"));
        assert_eq!(sparse_fork.get(5, 16_383), None);
    }

    #[test]
    fn wave3_xfd_4096_pages_memory_bound_cells_and_styles() {
        let mut store = Store::default();
        let xfd = 16_383u32;
        for r in 0..1_048_576u32 {
            store.set(r, xfd, num(r as f64));
        }
        assert_eq!(store.len(), 1_048_576);
        assert_eq!(store.page_count(), 4096);
        assert_eq!(store.used_bounds(), Some((0, xfd, 1_048_575, xfd)));
        assert_eq!(store.get_f64(0, xfd), Some(0.0));
        assert_eq!(store.get_f64(255, xfd), Some(255.0));
        assert_eq!(store.get_f64(256, xfd), Some(256.0));
        assert_eq!(store.get_f64(524_288, xfd), Some(524_288.0));
        assert_eq!(store.get_f64(1_048_575, xfd), Some(1_048_575.0));
        let payload = store.unique_payload_bytes();
        // 1M f64s = 8.0 MiB + 4096 compact sparse page dirs + slab headers (~1 MiB) << 12 MiB
        // (Previously >520 MiB due to 128 KB/page dense Vec<Option<Arc<_>>> bloat)
        assert!(
            payload < 12 * 1024 * 1024,
            "1M rows in XFD must stay < 12 MiB (no 128KB/page bloat), got {:.2} MiB",
            payload as f64 / 1_048_576.0
        );

        // Also verify CellStyles across 4096 pages at XFD stay compact & accurate
        let mut styles = CellStyles::default();
        for p in 0..4096u32 {
            styles.insert(CellRef::new(p * 256, xfd), ((p % 17) + 1) as u16);
        }
        assert_eq!(styles.len(), 4096);
        assert_eq!(styles.get(&CellRef::new(0, xfd)), Some(&1));
        assert_eq!(styles.get(&CellRef::new(4095 * 256, xfd)), Some(&((4095 % 17 + 1) as u16)));
        assert_eq!(styles.iter_rect((0, xfd, 1_048_575, xfd)).count(), 4096);
    }

    #[test]
    fn wave3_gather_rows_and_shift_col_band_preserve_typed_slabs_and_arc_text() {
        let mut store = Store::default();
        let shared_a: Arc<str> = Arc::from("应收账款-华东");
        let shared_b: Arc<str> = Arc::from("应付账款-华南");
        let n = 600u32;
        for r in 0..n {
            store.set(r, 0, num(r as f64));
            let txt = if r % 2 == 0 {
                Arc::clone(&shared_a)
            } else {
                Arc::clone(&shared_b)
            };
            store.set(r, 1, Cell { value: Value::Text(txt), formula: None });
            store.set(
                r,
                2,
                Cell {
                    value: Value::Number(r as f64 * 2.0),
                    formula: Some(format!("=A{}*2", r + 1).into()),
                },
            );
        }

        // Reverse all 600 rows via gather_rows
        let order: Vec<u32> = (0..n).rev().collect();
        let source = store.clone();
        let mut cleared = CellJournal::default();
        let changed = store.gather_rows(&source, (0, 0, n - 1, 2), &order, "Sheet1", &mut cleared);
        assert!(changed > 0);
        assert_eq!(store.len(), (n * 3) as usize);

        // Verify slab types are preserved directly (Numbers / Values / Mixed)
        for p in 0..3u32 {
            let page = store.page(p).unwrap();
            assert!(matches!(page.cols.get(0).unwrap().as_ref(), ColSlab::Numbers(_)));
            assert!(matches!(page.cols.get(1).unwrap().as_ref(), ColSlab::Values(_)));
            assert!(matches!(page.cols.get(2).unwrap().as_ref(), ColSlab::Mixed(_)));
        }
        // Verify values, translated formulas, and Arc<str> pointer identity
        assert_eq!(store.get_f64(0, 0), Some(599.0));
        assert_eq!(store.get_f64(599, 0), Some(0.0));
        assert_eq!(store.formula_of(0, 2), Some("=A1*2"));
        assert_eq!(store.formula_of(599, 2), Some("=A600*2"));
        match store.value_ref(0, 1).as_ref() {
            Value::Text(t) => assert!(Arc::ptr_eq(t, &shared_b)),
            other => panic!("expected Text, got {other:?}"),
        }
        match store.value_ref(599, 1).as_ref() {
            Value::Text(t) => assert!(Arc::ptr_eq(t, &shared_a)),
            other => panic!("expected Text, got {other:?}"),
        }

        // shift_col_band_rows preserves Numbers & Values + Arc<str> sharing
        store.shift_col_band_rows(0, 1, 100, 13, false, &mut cleared);
        for p in 0..3u32 {
            let page = store.page(p).unwrap();
            assert!(matches!(page.cols.get(0).unwrap().as_ref(), ColSlab::Numbers(_)));
            assert!(matches!(page.cols.get(1).unwrap().as_ref(), ColSlab::Values(_)));
        }
        assert_eq!(store.get_f64(113, 0), Some(499.0));
        match store.value_ref(612, 1).as_ref() {
            Value::Text(t) => assert!(Arc::ptr_eq(t, &shared_a)),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn wave3_arc_text_zero_copy_across_values_upgrade_and_reads() {
        let mut store = Store::default();
        let shared: Arc<str> = Arc::from("零拷贝共享文本");
        store.set(0, 0, Cell { value: Value::Text(Arc::clone(&shared)), formula: None });
        assert!(matches!(
            store.page(0).unwrap().cols.get(0).unwrap().as_ref(),
            ColSlab::Values(_)
        ));

        // value_ref, get, get_view, and Store::clone all preserve Arc::ptr_eq
        match store.value_ref(0, 0).as_ref() {
            Value::Text(t) => assert!(Arc::ptr_eq(t, &shared)),
            other => panic!("expected Text, got {other:?}"),
        }
        match &store.get(0, 0).unwrap().value {
            Value::Text(t) => assert!(Arc::ptr_eq(t, &shared)),
            other => panic!("expected Text, got {other:?}"),
        }
        let cloned = store.clone();
        match cloned.value_ref(0, 0).as_ref() {
            Value::Text(t) => assert!(Arc::ptr_eq(t, &shared)),
            other => panic!("expected Text, got {other:?}"),
        }

        // Upgrading Values -> Mixed preserves exact Arc<str> pointer in row 0
        store.set(1, 0, Cell { value: Value::Number(1.0), formula: Some("=1".into()) });
        assert!(matches!(
            store.page(0).unwrap().cols.get(0).unwrap().as_ref(),
            ColSlab::Mixed(_)
        ));
        match store.value_ref(0, 0).as_ref() {
            Value::Text(t) => assert!(Arc::ptr_eq(t, &shared)),
            other => panic!("expected Text, got {other:?}"),
        }
    }
}
