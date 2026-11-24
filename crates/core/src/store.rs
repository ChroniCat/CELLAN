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
    pub(crate) version: CellVersion,
    len: usize,
}

impl PartialEq for CellStyles {
    fn eq(&self, other: &Self) -> bool {
        if self.version == other.version {
            return true;
        }
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
        self.version.advance();
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
        self.version.advance();
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
            self.version.advance();
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
        if total_removed > 0 {
            self.len -= total_removed;
            self.version.advance();
        }
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

    /// Re-key complete style pages when possible, leaving untouched prefix pages (`< at / PAGE_ROWS`)
    /// in place in `O(1)` and repacking non-aligned pages in parallel without per-cell hash/page lookups.
    pub(crate) fn remap_axis(&mut self, rows: bool, at: u32, map: impl Fn(u32) -> Option<u32>) {
        if self.len == 0 {
            return;
        }
        let first_pi = if rows { at / PAGE_ROWS } else { 0 };
        if rows && self.iter_pages_in(first_pi, u32::MAX).next().is_none() {
            return;
        }
        if !rows
            && self
                .iter_pages()
                .all(|(_, p)| p.iter_nonempty_cols(at, u32::MAX).next().is_none())
        {
            return;
        }
        self.version.advance();

        if !rows {
            let page_ids: Vec<u32> = self
                .iter_pages()
                .filter(|(_, p)| p.iter_nonempty_cols(at, u32::MAX).next().is_some())
                .map(|(pi, _)| pi)
                .collect();
            for pi in page_ids {
                let Some(page) = self.page_mut_existing(pi) else {
                    continue;
                };
                let old_entries = std::mem::take(&mut page.cols).into_entries();
                let mut new_cols = PageCols::default();
                let mut old_cnt = 0usize;
                let mut new_cnt = 0usize;
                for (c, sc) in old_entries {
                    old_cnt += sc.len();
                    if c < at {
                        new_cnt += sc.len();
                        new_cols.set(c, sc);
                    } else if let Some(nc) = map(c) {
                        new_cnt += sc.len();
                        new_cols.set(nc, sc);
                    }
                }
                page.cols = new_cols;
                let empty = page.is_empty();
                self.len = self.len - old_cnt + new_cnt;
                if empty {
                    self.remove_page(pi);
                }
            }
            return;
        }

        // Extract only pages `>= first_pi`, leaving all prefix pages untouched in O(1)!
        let mut source_pages: Vec<(u32, Arc<StylePage>)> = Vec::new();
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

        let mut split_sources: Vec<(u32, Arc<StylePage>, Vec<(std::ops::Range<u32>, u32, u32)>)> =
            Vec::new();
        for (pi, page_arc) in source_pages {
            let base = pi * PAGE_ROWS;
            let page_len: usize = page_arc
                .iter_nonempty_cols(0, u32::MAX)
                .filter_map(|c| page_arc.get_col(c))
                .map(|sc| sc.len())
                .sum();
            if base + PAGE_ROWS <= at {
                self.insert_page_arc(pi, page_arc);
                continue;
            }
            self.len -= page_len;
            if base >= at
                && let Some(target0) = map(base)
                && target0 % PAGE_ROWS == 0
                && map(base + PAGE_ROWS - 1) == Some(target0 + PAGE_ROWS - 1)
            {
                let dest = target0 / PAGE_ROWS;
                if self.page(dest).is_none() {
                    self.insert_page_arc(dest, page_arc);
                    self.len += page_len;
                    continue;
                }
            }
            let mut spans: Vec<(std::ops::Range<u32>, u32, u32)> = Vec::new();
            for r in 0..PAGE_ROWS {
                let Some(target) = map(base + r) else { continue };
                let (dest, offset) = (target / PAGE_ROWS, target % PAGE_ROWS);
                if let Some((source, pid, start)) = spans.last_mut()
                    && *pid == dest
                    && source.end == r
                    && *start + (source.end - source.start) == offset
                {
                    source.end += 1;
                } else {
                    spans.push((r..r + 1, dest, offset));
                }
            }
            if !spans.is_empty() {
                split_sources.push((pi, page_arc, spans));
            }
        }
        if split_sources.is_empty() {
            return;
        }

        // Group `(src_idx, src_range, dst_offset)` by `dest_pi` so every destination page is built independently.
        let mut dest_map: BTreeMap<u32, Vec<(usize, std::ops::Range<u32>, u32)>> = BTreeMap::new();
        for (src_idx, (_, _, spans)) in split_sources.iter().enumerate() {
            for (src_r, dest_pi, dst_off) in spans {
                dest_map
                    .entry(*dest_pi)
                    .or_default()
                    .push((src_idx, src_r.clone(), *dst_off));
            }
        }
        // Pull any pre-existing page at a destination index into `split_sources` as an identity source
        // so parallel workers build complete pages without merging afterwards.
        let dest_keys: Vec<u32> = dest_map.keys().copied().collect();
        for dpi in dest_keys {
            if let Some(existing) = self.page(dpi).cloned() {
                let existing_len: usize = existing
                    .iter_nonempty_cols(0, u32::MAX)
                    .filter_map(|c| existing.get_col(c))
                    .map(|sc| sc.len())
                    .sum();
                self.remove_page(dpi);
                self.len -= existing_len;
                let src_idx = split_sources.len();
                split_sources.push((dpi, existing, Vec::new()));
                dest_map
                    .get_mut(&dpi)
                    .unwrap()
                    .insert(0, (src_idx, 0..PAGE_ROWS, 0));
            }
        }
        let dest_plan: Vec<(u32, Vec<(usize, std::ops::Range<u32>, u32)>)> =
            dest_map.into_iter().collect();

        let build_dest = |dpi: u32, parts: &[(usize, std::ops::Range<u32>, u32)]| -> Option<(u32, Arc<StylePage>, usize)> {
            let mut cols_set: Vec<u32> = Vec::new();
            for &(src_idx, _, _) in parts {
                for c in split_sources[src_idx].1.iter_nonempty_cols(0, u32::MAX) {
                    cols_set.push(c);
                }
            }
            cols_set.sort_unstable();
            cols_set.dedup();
            if cols_set.is_empty() {
                return None;
            }
            let mut page_cols = PageCols::default();
            let mut page_total = 0usize;
            for col in cols_set {
                // Fast Uniform preservation if a single full-page source or matching Uniform covers all 256 rows
                if parts.len() == 1
                    && parts[0].1 == (0..PAGE_ROWS)
                    && parts[0].2 == 0
                    && let Some(sc) = split_sources[parts[0].0].1.get_col(col)
                {
                    page_total += sc.len();
                    page_cols.set(col, sc.clone());
                    continue;
                }
                let mut col_sc = StyleColumn::default();
                for &(src_idx, ref src_range, dst_off) in parts {
                    let Some(sc) = split_sources[src_idx].1.get_col(col) else {
                        continue;
                    };
                    let len = src_range.end - src_range.start;
                    match sc.as_ref() {
                        StyleColumn::Uniform(sid) => {
                            col_sc.insert_range(dst_off, dst_off + len - 1, *sid);
                        }
                        StyleColumn::Sparse { rows, ids } => {
                            let m = row_range_mask(src_range.start, src_range.end - 1);
                            if (rows[0] & m[0]) == 0
                                && (rows[1] & m[1]) == 0
                                && (rows[2] & m[2]) == 0
                                && (rows[3] & m[3]) == 0
                            {
                                continue;
                            }
                            let mut rank = count_bits_below(rows, src_range.start);
                            for w in (src_range.start as usize / 64)..=((src_range.end - 1) as usize / 64) {
                                let mut bits = rows[w] & m[w];
                                while bits != 0 {
                                    let tz = bits.trailing_zeros();
                                    bits &= bits - 1;
                                    let src_ro = w as u32 * 64 + tz;
                                    let target_ro = dst_off + (src_ro - src_range.start);
                                    col_sc.insert(target_ro, ids[rank]);
                                    rank += 1;
                                }
                            }
                        }
                    }
                }
                let clen = col_sc.len();
                if clen > 0 {
                    page_total += clen;
                    page_cols.set(col, Arc::new(col_sc));
                }
            }
            (page_total > 0).then(|| (dpi, Arc::new(StylePage { cols: page_cols }), page_total))
        };

        let built: Vec<(u32, Arc<StylePage>, usize)> = if dest_plan.len() >= 16 {
            let n_threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(8)
                .min(dest_plan.len());
            let chunk_size = dest_plan.len().div_ceil(n_threads);
            let mut out = Vec::with_capacity(dest_plan.len());
            std::thread::scope(|s| {
                let mut handles = Vec::with_capacity(n_threads);
                for chunk in dest_plan.chunks(chunk_size) {
                    let b_ref = &build_dest;
                    handles.push(s.spawn(move || {
                        let mut local = Vec::with_capacity(chunk.len());
                        for (dpi, parts) in chunk {
                            if let Some(res) = b_ref(*dpi, parts) {
                                local.push(res);
                            }
                        }
                        local
                    }));
                }
                for h in handles {
                    if let Ok(local) = h.join() {
                        out.extend(local);
                    }
                }
            });
            out
        } else {
            dest_plan
                .iter()
                .filter_map(|(dpi, parts)| build_dest(*dpi, parts))
                .collect()
        };

        for (dpi, page_arc, cnt) in built {
            self.insert_page_arc(dpi, page_arc);
            self.len += cnt;
        }
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
        self.version.advance();
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
        if !new_slabs.is_empty() {
            self.version.advance();
        }
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
        self.version.advance();
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

    /// Word-level intersection of `journal` (`CellJournal`) with non-empty cells in `self` (`CellStyles`),
    /// writing matching `[u64; 4]` page masks directly into `out`.
    pub(crate) fn intersect_journal_into(&self, journal: &CellJournal, out: &mut CellJournal) {
        if self.len == 0 || journal.is_empty() {
            return;
        }
        for (&(pi, col), &j_mask) in journal.page_masks() {
            if let Some(sc) = self.page_col(pi, col) {
                let s_mask = sc.row_mask();
                let both = [
                    j_mask[0] & s_mask[0],
                    j_mask[1] & s_mask[1],
                    j_mask[2] & s_mask[2],
                    j_mask[3] & s_mask[3],
                ];
                if both != [0; 4] {
                    out.or_page_mask(pi, col, both);
                }
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

const SPARSE_SLAB_CAP: usize = 32;
const DICT_TEXT_MAX_UNIQUE: usize = 128;

#[inline]
fn rank_in_rows(present: &[u64; 4], w: usize, bit: u64) -> usize {
    let prefix: u32 = present[..w].iter().map(|&word| word.count_ones()).sum();
    (prefix + (present[w] & (bit - 1)).count_ones()) as usize
}

#[inline]
fn next_bit_from(present: &[u64; 4], lo: usize) -> Option<u32> {
    if lo >= PAGE_ROWS as usize {
        return None;
    }
    let start_w = lo / 64;
    let first_bits = present[start_w] & (!0u64 << (lo % 64));
    if first_bits != 0 {
        return Some((start_w * 64 + first_bits.trailing_zeros() as usize) as u32);
    }
    for w in (start_w + 1)..4 {
        if present[w] != 0 {
            return Some((w * 64 + present[w].trailing_zeros() as usize) as u32);
        }
    }
    None
}

#[inline]
fn prev_bit_before(present: &[u64; 4], hi_excl: usize) -> Option<u32> {
    let hi = hi_excl.min(PAGE_ROWS as usize);
    if hi == 0 {
        return None;
    }
    let last_idx = hi - 1;
    let end_w = last_idx / 64;
    let rem = last_idx % 64;
    let top_mask = if rem == 63 { !0u64 } else { (1u64 << (rem + 1)) - 1 };
    let end_bits = present[end_w] & top_mask;
    if end_bits != 0 {
        return Some((end_w * 64 + (63 - end_bits.leading_zeros() as usize)) as u32);
    }
    for w in (0..end_w).rev() {
        if present[w] != 0 {
            return Some((w * 64 + (63 - present[w].leading_zeros() as usize)) as u32);
        }
    }
    None
}

#[inline]
fn first_last_bits(present: &[u64; 4]) -> Option<(u32, u32)> {
    let first = next_bit_from(present, 0)?;
    let last = prev_bit_before(present, PAGE_ROWS as usize).unwrap();
    Some((first, last))
}

#[inline]
fn has_bits_in(present: &[u64; 4], lo: usize, hi_excl: usize) -> bool {
    let hi = hi_excl.min(PAGE_ROWS as usize);
    if lo >= hi {
        return false;
    }
    let m = row_range_mask(lo as u32, (hi - 1) as u32);
    (present[0] & m[0]) != 0
        || (present[1] & m[1]) != 0
        || (present[2] & m[2]) != 0
        || (present[3] & m[3]) != 0
}

#[inline]
fn mask_from_offset(present: &[u64; 4], begin: usize) -> [u64; 4] {
    if begin >= PAGE_ROWS as usize {
        return [0; 4];
    }
    let m = row_range_mask(begin as u32, PAGE_ROWS - 1);
    [
        present[0] & m[0],
        present[1] & m[1],
        present[2] & m[2],
        present[3] & m[3],
    ]
}

#[inline]
fn dict_lookup_or_insert(dict: &mut Vec<Value>, text: &Arc<str>) -> Option<u8> {
    // Fast 1-slot MRU check on the most recently inserted dictionary entry.
    if dict.len() > 1
        && let Value::Text(last) = &dict[dict.len() - 1]
        && Arc::ptr_eq(last, text)
    {
        return Some((dict.len() - 1) as u8);
    }
    // Pointer equality pass first (nanoseconds for interned/SST strings).
    for (idx, item) in dict.iter().enumerate().skip(1) {
        if let Value::Text(existing) = item
            && Arc::ptr_eq(existing, text)
        {
            return Some(idx as u8);
        }
    }
    // Content equality fallback.
    for (idx, item) in dict.iter().enumerate().skip(1) {
        if let Value::Text(existing) = item
            && existing.as_ref() == text.as_ref()
        {
            return Some(idx as u8);
        }
    }
    if dict.len() > DICT_TEXT_MAX_UNIQUE {
        return None;
    }
    let code = dict.len() as u8;
    dict.push(Value::Text(Arc::clone(text)));
    Some(code)
}

#[derive(Clone, Debug)]
pub(crate) enum ValueSlabRepr {
    /// Packed non-empty `Value`s (`<= 32` items), indexed in `O(1)` via `count_bits_below(&present, ro)`.
    Sparse(Vec<Value>),
    /// Page-level 1-byte dictionary text slab (`dict[0] = Value::Empty`, `dict[1..=K]` = distinct `Value::Text`).
    Dict {
        codes: Vec<u8>,
        dict: Vec<Value>,
    },
    /// General dense `Vec<Value>` (`len == highest_bit_plus_one(&present)`).
    Dense(Vec<Value>),
}

/// Non-formula column page slab with `[u64; 4]` occupancy bitmap, sparse packing for low-density
/// pages (`<= 32` cells), and 1-byte dictionary compression (`DictText`) for categorical text columns.
/// Implements `Deref<Target = [Value]>` so external slice consumers (`formula`, `xlsx`) work transparently.
#[derive(Debug)]
pub struct ValueSlab {
    pub(crate) present: [u64; 4],
    len: u16,
    pub(crate) repr: ValueSlabRepr,
    dense_cache: OnceLock<Box<[Value]>>,
}

impl Default for ValueSlab {
    #[inline]
    fn default() -> Self {
        Self {
            present: [0; 4],
            len: 0,
            repr: ValueSlabRepr::Sparse(Vec::new()),
            dense_cache: OnceLock::new(),
        }
    }
}

impl Clone for ValueSlab {
    fn clone(&self) -> Self {
        Self {
            present: self.present,
            len: self.len,
            repr: self.repr.clone(),
            dense_cache: OnceLock::new(),
        }
    }
}

impl PartialEq for ValueSlab {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len || self.present != other.present {
            return false;
        }
        for w in 0..4 {
            let mut bits = self.present[w];
            while bits != 0 {
                let tz = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let ro = w * 64 + tz;
                if self.get_val(ro) != other.get_val(ro) {
                    return false;
                }
            }
        }
        true
    }
}

impl From<Vec<Value>> for ValueSlab {
    #[inline]
    fn from(vals: Vec<Value>) -> Self {
        Self::from_vec(vals)
    }
}

impl FromIterator<Value> for ValueSlab {
    #[inline]
    fn from_iter<T: IntoIterator<Item = Value>>(iter: T) -> Self {
        Self::from_vec(iter.into_iter().collect())
    }
}

impl std::ops::Deref for ValueSlab {
    type Target = [Value];

    #[inline]
    fn deref(&self) -> &[Value] {
        match &self.repr {
            ValueSlabRepr::Dense(v) => v.as_slice(),
            _ => self.dense_cache.get_or_init(|| self.materialize_boxed()),
        }
    }
}

impl<'a> IntoIterator for &'a ValueSlab {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        let slice: &'a [Value] = self;
        slice.iter()
    }
}

impl ValueSlab {
    pub fn from_vec(mut vals: Vec<Value>) -> Self {
        while vals.last().is_some_and(Value::is_empty) {
            vals.pop();
        }
        if vals.is_empty() {
            return Self::default();
        }
        let len = vals.len();
        let mut present = [0u64; 4];
        let mut occupied = 0usize;
        let mut all_text = true;
        for (r, v) in vals.iter().enumerate() {
            if !v.is_empty() {
                present[r / 64] |= 1u64 << (r % 64);
                occupied += 1;
                if !matches!(v, Value::Text(_)) {
                    all_text = false;
                }
            }
        }
        if occupied == 0 {
            return Self::default();
        }
        if occupied <= SPARSE_SLAB_CAP {
            let mut items = Vec::with_capacity(occupied);
            for v in vals {
                if !v.is_empty() {
                    items.push(v);
                }
            }
            return Self {
                present,
                len: len as u16,
                repr: ValueSlabRepr::Sparse(items),
                dense_cache: OnceLock::new(),
            };
        }
        if all_text {
            let mut dict = Vec::with_capacity(16);
            dict.push(Value::Empty);
            let mut codes = Vec::with_capacity(len);
            let mut ok = true;
            for v in &vals {
                match v {
                    Value::Empty => codes.push(0),
                    Value::Text(s) => {
                        if let Some(code) = dict_lookup_or_insert(&mut dict, s) {
                            codes.push(code);
                        } else {
                            ok = false;
                            break;
                        }
                    }
                    _ => unreachable!(),
                }
            }
            if ok {
                dict.shrink_to_fit();
                return Self {
                    present,
                    len: len as u16,
                    repr: ValueSlabRepr::Dict { codes, dict },
                    dense_cache: OnceLock::new(),
                };
            }
        }
        Self {
            present,
            len: len as u16,
            repr: ValueSlabRepr::Dense(vals),
            dense_cache: OnceLock::new(),
        }
    }

    fn materialize_boxed(&self) -> Box<[Value]> {
        let len = self.len as usize;
        if len == 0 {
            return Box::new([]);
        }
        match &self.repr {
            ValueSlabRepr::Dense(v) => v.clone().into_boxed_slice(),
            ValueSlabRepr::Dict { codes, dict } => codes[..len]
                .iter()
                .map(|&c| dict[c as usize].clone())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            ValueSlabRepr::Sparse(items) => {
                let mut out = vec![Value::Empty; len];
                let mut rank = 0usize;
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        out[w * 64 + tz] = items[rank].clone();
                        rank += 1;
                    }
                }
                out.into_boxed_slice()
            }
        }
    }

    pub fn into_vec(self) -> Vec<Value> {
        let len = self.len as usize;
        if len == 0 {
            return Vec::new();
        }
        match self.repr {
            ValueSlabRepr::Dense(mut v) => {
                v.truncate(len);
                v
            }
            ValueSlabRepr::Dict { codes, dict } => codes[..len]
                .iter()
                .map(|&c| dict[c as usize].clone())
                .collect(),
            ValueSlabRepr::Sparse(items) => {
                let mut out = vec![Value::Empty; len];
                let mut iter = items.into_iter();
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        if let Some(v) = iter.next() {
                            out[w * 64 + tz] = v;
                        }
                    }
                }
                out
            }
        }
    }

    #[inline]
    pub fn as_slice(&self) -> &[Value] {
        self
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.present == [0; 4]
    }

    #[inline]
    pub fn first_last_occupied(&self) -> Option<(u32, u32)> {
        first_last_bits(&self.present)
    }

    #[inline]
    pub fn has_any_in(&self, lo: usize, hi_excl: usize) -> bool {
        has_bits_in(&self.present, lo, hi_excl)
    }

    #[inline]
    pub fn next_occupied_from(&self, lo: usize) -> Option<u32> {
        next_bit_from(&self.present, lo)
    }

    #[inline]
    pub fn prev_occupied_before(&self, hi_excl: usize) -> Option<u32> {
        prev_bit_before(&self.present, hi_excl)
    }

    #[inline]
    pub fn row_mask_from(&self, begin: usize) -> [u64; 4] {
        mask_from_offset(&self.present, begin)
    }

    #[inline]
    pub fn is_dict_text(&self) -> bool {
        matches!(self.repr, ValueSlabRepr::Dict { .. })
    }

    #[inline]
    pub fn is_sparse(&self) -> bool {
        matches!(self.repr, ValueSlabRepr::Sparse(_))
    }

    /// Borrow `(codes, dict)` when this slab is stored in 1-byte `DictText` form.
    /// `dict[0]` is `Value::Empty`, and `dict[1..]` are distinct `Value::Text` values.
    #[inline]
    pub fn dict_text(&self) -> Option<(&[u8], &[Value])> {
        match &self.repr {
            ValueSlabRepr::Dict { codes, dict } => Some((&codes[..self.len as usize], dict.as_slice())),
            _ => None,
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        match &self.repr {
            ValueSlabRepr::Sparse(v) => v.capacity(),
            ValueSlabRepr::Dict { codes, .. } => codes.capacity(),
            ValueSlabRepr::Dense(v) => v.capacity(),
        }
    }

    #[inline]
    pub fn heap_payload_bytes(&self) -> usize {
        let base = match &self.repr {
            ValueSlabRepr::Sparse(v) => v.capacity() * std::mem::size_of::<Value>(),
            ValueSlabRepr::Dict { codes, dict } => {
                codes.capacity() * std::mem::size_of::<u8>()
                    + dict.capacity() * std::mem::size_of::<Value>()
            }
            ValueSlabRepr::Dense(v) => v.capacity() * std::mem::size_of::<Value>(),
        };
        let cache = self
            .dense_cache
            .get()
            .map_or(0, |b| b.len() * std::mem::size_of::<Value>());
        base + cache
    }

    #[inline]
    pub fn occupied_count(&self) -> usize {
        (self.present[0].count_ones()
            + self.present[1].count_ones()
            + self.present[2].count_ones()
            + self.present[3].count_ones()) as usize
    }

    #[inline]
    pub fn contains(&self, ro: u32) -> bool {
        ro < PAGE_ROWS && (self.present[(ro / 64) as usize] & (1u64 << (ro % 64))) != 0
    }

    /// Fast non-materializing lookup of an occupied cell at row offset `ro`.
    #[inline]
    pub fn get_val(&self, ro: usize) -> Option<&Value> {
        if ro >= self.len as usize {
            return None;
        }
        let w = ro / 64;
        let bit = 1u64 << (ro % 64);
        if (self.present[w] & bit) == 0 {
            return None;
        }
        match &self.repr {
            ValueSlabRepr::Sparse(items) => {
                let rank = rank_in_rows(&self.present, w, bit);
                items.get(rank)
            }
            ValueSlabRepr::Dict { codes, dict } => {
                let code = *codes.get(ro)?;
                dict.get(code as usize)
            }
            ValueSlabRepr::Dense(vals) => vals.get(ro),
        }
    }

    /// Visit every occupied `(ro, &Value)` in ascending row order without materializing `dense_cache`.
    #[inline]
    pub fn for_each_occupied(&self, mut f: impl FnMut(usize, &Value)) {
        match &self.repr {
            ValueSlabRepr::Sparse(items) => {
                let mut rank = 0usize;
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        f(w * 64 + tz, &items[rank]);
                        rank += 1;
                    }
                }
            }
            ValueSlabRepr::Dict { codes, dict } => {
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        let ro = w * 64 + tz;
                        f(ro, &dict[codes[ro] as usize]);
                    }
                }
            }
            ValueSlabRepr::Dense(vals) => {
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        let ro = w * 64 + tz;
                        f(ro, &vals[ro]);
                    }
                }
            }
        }
    }

    pub fn all_numbers(&self) -> bool {
        if self.is_empty() {
            return false;
        }
        match &self.repr {
            ValueSlabRepr::Dict { .. } => false,
            ValueSlabRepr::Sparse(items) => items.iter().all(|v| matches!(v, Value::Number(_))),
            ValueSlabRepr::Dense(vals) => {
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        if !matches!(vals[w * 64 + tz], Value::Number(_)) {
                            return false;
                        }
                    }
                }
                true
            }
        }
    }

    fn to_number_slab(&self) -> NumberSlab {
        let len = self.len as usize;
        let mut values = vec![0.0; len];
        self.for_each_occupied(|ro, v| {
            if let Value::Number(n) = *v {
                values[ro] = n;
            }
        });
        NumberSlab {
            present: self.present,
            values,
        }
    }

    fn expand_to_dense(&mut self) -> &mut Vec<Value> {
        if !matches!(self.repr, ValueSlabRepr::Dense(_)) {
            let len = self.len as usize;
            let mut out = Vec::with_capacity(len.max(64).min(PAGE_ROWS as usize));
            out.resize(len, Value::Empty);
            match &mut self.repr {
                ValueSlabRepr::Sparse(items) => {
                    let mut iter = std::mem::take(items).into_iter();
                    for w in 0..4 {
                        let mut bits = self.present[w];
                        while bits != 0 {
                            let tz = bits.trailing_zeros() as usize;
                            bits &= bits - 1;
                            if let Some(v) = iter.next() {
                                out[w * 64 + tz] = v;
                            }
                        }
                    }
                }
                ValueSlabRepr::Dict { codes, dict } => {
                    for (ro, &c) in codes[..len.min(codes.len())].iter().enumerate() {
                        if c != 0 {
                            out[ro] = dict[c as usize].clone();
                        }
                    }
                }
                ValueSlabRepr::Dense(_) => unreachable!(),
            }
            self.repr = ValueSlabRepr::Dense(out);
        }
        match &mut self.repr {
            ValueSlabRepr::Dense(v) => v,
            _ => unreachable!(),
        }
    }

    fn upgrade_sparse_after_insert(&mut self) {
        let ValueSlabRepr::Sparse(items) = &mut self.repr else {
            return;
        };
        if items.len() <= SPARSE_SLAB_CAP {
            return;
        }
        let len = self.len as usize;
        let all_text = items.iter().all(|v| matches!(v, Value::Text(_)));
        if all_text {
            let mut dict = Vec::with_capacity(16);
            dict.push(Value::Empty);
            let mut codes = vec![0u8; len];
            let mut ok = true;
            let mut rank = 0usize;
            'outer: for w in 0..4 {
                let mut bits = self.present[w];
                while bits != 0 {
                    let tz = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    let ro = w * 64 + tz;
                    if let Value::Text(s) = &items[rank] {
                        if let Some(code) = dict_lookup_or_insert(&mut dict, s) {
                            codes[ro] = code;
                        } else {
                            ok = false;
                            break 'outer;
                        }
                    }
                    rank += 1;
                }
            }
            if ok {
                codes.reserve((PAGE_ROWS as usize).saturating_sub(codes.len()));
                self.repr = ValueSlabRepr::Dict { codes, dict };
                return;
            }
        }
        self.expand_to_dense();
    }

    pub fn set(&mut self, ro: u32, value: Value) -> bool {
        debug_assert!(!value.is_empty() && ro < PAGE_ROWS);
        self.dense_cache.take();
        let idx = ro as usize;
        let w = idx / 64;
        let bit = 1u64 << (idx % 64);
        let existed = (self.present[w] & bit) != 0;
        let rank_before = rank_in_rows(&self.present, w, bit);
        self.present[w] |= bit;
        if (idx + 1) as u16 > self.len {
            self.len = (idx + 1) as u16;
        }
        match &mut self.repr {
            ValueSlabRepr::Sparse(items) => {
                if existed {
                    items[rank_before] = value;
                } else {
                    items.insert(rank_before, value);
                    if items.len() > SPARSE_SLAB_CAP {
                        self.upgrade_sparse_after_insert();
                    }
                }
            }
            ValueSlabRepr::Dict { codes, dict } => {
                if let Value::Text(ref s) = value
                    && let Some(code) = dict_lookup_or_insert(dict, s)
                {
                    if codes.len() < self.len as usize {
                        codes.resize(self.len as usize, 0);
                    }
                    codes[idx] = code;
                } else {
                    let target_len = self.len as usize;
                    let dense = self.expand_to_dense();
                    grow_slab_vec(dense, target_len, Value::Empty);
                    dense[idx] = value;
                }
            }
            ValueSlabRepr::Dense(vals) => {
                grow_slab_vec(vals, self.len as usize, Value::Empty);
                vals[idx] = value;
            }
        }
        existed
    }

    pub fn remove(&mut self, ro: u32) -> Option<Value> {
        let idx = ro as usize;
        if idx >= self.len as usize {
            return None;
        }
        let w = idx / 64;
        let bit = 1u64 << (idx % 64);
        if (self.present[w] & bit) == 0 {
            return None;
        }
        self.dense_cache.take();
        let rank = rank_in_rows(&self.present, w, bit);
        self.present[w] &= !bit;
        self.len = highest_bit_plus_one(&self.present) as u16;
        let old = match &mut self.repr {
            ValueSlabRepr::Sparse(items) => items.remove(rank),
            ValueSlabRepr::Dict { codes, dict } => {
                let code = codes[idx] as usize;
                codes[idx] = 0;
                codes.truncate(self.len as usize);
                dict[code].clone()
            }
            ValueSlabRepr::Dense(vals) => {
                let old = std::mem::take(&mut vals[idx]);
                vals.truncate(self.len as usize);
                old
            }
        };
        if self.present == [0; 4] {
            self.repr = ValueSlabRepr::Sparse(Vec::new());
        } else {
            self.maybe_compact_sparse();
        }
        Some(old)
    }

    fn maybe_compact_sparse(&mut self) {
        if matches!(self.repr, ValueSlabRepr::Sparse(_)) {
            return;
        }
        let occ = self.occupied_count();
        if occ <= SPARSE_SLAB_CAP / 2 {
            let mut items = Vec::with_capacity(occ);
            self.for_each_occupied(|_, v| items.push(v.clone()));
            self.repr = ValueSlabRepr::Sparse(items);
        }
    }

    pub fn clear_range(&mut self, lo: usize, hi_incl: usize) -> (usize, [u64; 4]) {
        let end = (hi_incl + 1).min(self.len as usize);
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
        let removed = (removed_mask[0].count_ones()
            + removed_mask[1].count_ones()
            + removed_mask[2].count_ones()
            + removed_mask[3].count_ones()) as usize;
        if removed == 0 {
            return (0, [0; 4]);
        }
        self.dense_cache.take();
        match &mut self.repr {
            ValueSlabRepr::Sparse(items) => {
                let lo_rank = count_bits_below(&self.present, lo as u32);
                items.drain(lo_rank..lo_rank + removed);
            }
            ValueSlabRepr::Dict { codes, .. } => {
                codes[lo..end].fill(0);
            }
            ValueSlabRepr::Dense(vals) => {
                for v in &mut vals[lo..end] {
                    *v = Value::Empty;
                }
            }
        }
        for w in 0..4 {
            self.present[w] &= !m[w];
        }
        self.len = highest_bit_plus_one(&self.present) as u16;
        match &mut self.repr {
            ValueSlabRepr::Sparse(_) => {}
            ValueSlabRepr::Dict { codes, .. } => codes.truncate(self.len as usize),
            ValueSlabRepr::Dense(vals) => vals.truncate(self.len as usize),
        }
        if self.present == [0; 4] {
            self.repr = ValueSlabRepr::Sparse(Vec::new());
        } else {
            self.maybe_compact_sparse();
        }
        (removed, removed_mask)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum MixedSlabRepr {
    Sparse(Vec<Cell>),
    Dense(Vec<Cell>),
}

/// Formula-capable 256-row column slab with `[u64; 4]` occupancy + formula bitmaps,
/// sparse packing (`<= 32` cells), and `Deref<Target = [Cell]>` compatibility.
#[derive(Debug)]
pub struct MixedSlab {
    pub(crate) present: [u64; 4],
    pub(crate) formula_mask: [u64; 4],
    len: u16,
    dirty_mut: bool,
    pub(crate) repr: MixedSlabRepr,
    dense_cache: OnceLock<Box<[Cell]>>,
}

impl Default for MixedSlab {
    #[inline]
    fn default() -> Self {
        Self {
            present: [0; 4],
            formula_mask: [0; 4],
            len: 0,
            dirty_mut: false,
            repr: MixedSlabRepr::Sparse(Vec::new()),
            dense_cache: OnceLock::new(),
        }
    }
}

impl Clone for MixedSlab {
    fn clone(&self) -> Self {
        Self {
            present: self.present,
            formula_mask: self.formula_mask,
            len: self.len,
            dirty_mut: self.dirty_mut,
            repr: self.repr.clone(),
            dense_cache: OnceLock::new(),
        }
    }
}

impl PartialEq for MixedSlab {
    fn eq(&self, other: &Self) -> bool {
        let pm_a = self.row_mask();
        let pm_b = other.row_mask();
        if pm_a != pm_b {
            return false;
        }
        if !self.dirty_mut && !other.dirty_mut && self.formula_mask != other.formula_mask {
            return false;
        }
        for w in 0..4 {
            let mut bits = pm_a[w];
            while bits != 0 {
                let tz = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let ro = w * 64 + tz;
                if self.get_cell(ro) != other.get_cell(ro) {
                    return false;
                }
            }
        }
        true
    }
}

impl From<Vec<Cell>> for MixedSlab {
    #[inline]
    fn from(cells: Vec<Cell>) -> Self {
        Self::from_vec(cells)
    }
}

impl FromIterator<Cell> for MixedSlab {
    #[inline]
    fn from_iter<T: IntoIterator<Item = Cell>>(iter: T) -> Self {
        Self::from_vec(iter.into_iter().collect())
    }
}

impl std::ops::Deref for MixedSlab {
    type Target = [Cell];

    #[inline]
    fn deref(&self) -> &[Cell] {
        match &self.repr {
            MixedSlabRepr::Dense(v) => v.as_slice(),
            MixedSlabRepr::Sparse(_) => self.dense_cache.get_or_init(|| self.materialize_boxed()),
        }
    }
}

impl<'a> IntoIterator for &'a MixedSlab {
    type Item = &'a Cell;
    type IntoIter = std::slice::Iter<'a, Cell>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        let slice: &'a [Cell] = self;
        slice.iter()
    }
}

impl MixedSlab {
    #[inline]
    pub fn as_slice(&self) -> &[Cell] {
        self
    }

    pub fn from_vec(mut cells: Vec<Cell>) -> Self {
        while cells.last().is_some_and(Cell::is_blank) {
            cells.pop();
        }
        if cells.is_empty() {
            return Self::default();
        }
        let len = cells.len();
        let mut present = [0u64; 4];
        let mut formula_mask = [0u64; 4];
        let mut occupied = 0usize;
        for (r, c) in cells.iter().enumerate() {
            if !c.is_blank() {
                present[r / 64] |= 1u64 << (r % 64);
                occupied += 1;
                if c.formula.is_some() {
                    formula_mask[r / 64] |= 1u64 << (r % 64);
                }
            }
        }
        if occupied == 0 {
            return Self::default();
        }
        if occupied <= SPARSE_SLAB_CAP {
            let mut items = Vec::with_capacity(occupied);
            for c in cells {
                if !c.is_blank() {
                    items.push(c);
                }
            }
            return Self {
                present,
                formula_mask,
                len: len as u16,
                dirty_mut: false,
                repr: MixedSlabRepr::Sparse(items),
                dense_cache: OnceLock::new(),
            };
        }
        Self {
            present,
            formula_mask,
            len: len as u16,
            dirty_mut: false,
            repr: MixedSlabRepr::Dense(cells),
            dense_cache: OnceLock::new(),
        }
    }

    fn materialize_boxed(&self) -> Box<[Cell]> {
        let len = self.len();
        if len == 0 {
            return Box::new([]);
        }
        match &self.repr {
            MixedSlabRepr::Dense(v) => v.clone().into_boxed_slice(),
            MixedSlabRepr::Sparse(items) => {
                let mut out = vec![Cell::default(); len];
                let mut rank = 0usize;
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        out[w * 64 + tz] = items[rank].clone();
                        rank += 1;
                    }
                }
                out.into_boxed_slice()
            }
        }
    }

    pub fn into_vec(self) -> Vec<Cell> {
        match self.repr {
            MixedSlabRepr::Dense(mut v) => {
                while v.last().is_some_and(Cell::is_blank) {
                    v.pop();
                }
                v
            }
            MixedSlabRepr::Sparse(items) => {
                let len = self.len as usize;
                if len == 0 {
                    return Vec::new();
                }
                let mut out = vec![Cell::default(); len];
                let mut iter = items.into_iter();
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        if let Some(c) = iter.next() {
                            out[w * 64 + tz] = c;
                        }
                    }
                }
                out
            }
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        if self.dirty_mut
            && let MixedSlabRepr::Dense(cells) = &self.repr
        {
            return cells
                .iter()
                .rposition(|c| !c.is_blank())
                .map_or(0, |idx| idx + 1);
        }
        self.len as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        if self.dirty_mut
            && let MixedSlabRepr::Dense(cells) = &self.repr
        {
            return cells.iter().all(Cell::is_blank);
        }
        self.present == [0; 4]
    }

    #[inline]
    pub fn has_formulas(&self) -> bool {
        if self.dirty_mut
            && let MixedSlabRepr::Dense(cells) = &self.repr
        {
            return cells.iter().any(|c| c.formula.is_some());
        }
        self.formula_mask != [0; 4]
    }

    #[inline]
    pub fn row_mask(&self) -> [u64; 4] {
        if self.dirty_mut
            && let MixedSlabRepr::Dense(cells) = &self.repr
        {
            let mut m = [0u64; 4];
            for (r, c) in cells.iter().enumerate() {
                if !c.is_blank() {
                    m[r / 64] |= 1u64 << (r % 64);
                }
            }
            return m;
        }
        self.present
    }

    #[inline]
    pub fn row_mask_from(&self, begin: usize) -> [u64; 4] {
        let pm = self.row_mask();
        mask_from_offset(&pm, begin)
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        match &self.repr {
            MixedSlabRepr::Sparse(v) => v.capacity(),
            MixedSlabRepr::Dense(v) => v.capacity(),
        }
    }

    #[inline]
    pub fn heap_payload_bytes(&self) -> usize {
        let base = match &self.repr {
            MixedSlabRepr::Sparse(v) => v.capacity() * std::mem::size_of::<Cell>(),
            MixedSlabRepr::Dense(v) => v.capacity() * std::mem::size_of::<Cell>(),
        };
        let cache = self
            .dense_cache
            .get()
            .map_or(0, |b| b.len() * std::mem::size_of::<Cell>());
        base + cache
    }

    #[inline]
    pub fn occupied_count(&self) -> usize {
        let pm = self.row_mask();
        (pm[0].count_ones() + pm[1].count_ones() + pm[2].count_ones() + pm[3].count_ones()) as usize
    }

    #[inline]
    pub fn contains(&self, ro: u32) -> bool {
        if self.dirty_mut
            && let MixedSlabRepr::Dense(cells) = &self.repr
        {
            return cells.get(ro as usize).is_some_and(|c| !c.is_blank());
        }
        ro < PAGE_ROWS && (self.present[(ro / 64) as usize] & (1u64 << (ro % 64))) != 0
    }

    #[inline]
    pub fn first_last_occupied(&self) -> Option<(u32, u32)> {
        let pm = self.row_mask();
        first_last_bits(&pm)
    }

    #[inline]
    pub fn has_any_in(&self, lo: usize, hi_excl: usize) -> bool {
        let pm = self.row_mask();
        has_bits_in(&pm, lo, hi_excl)
    }

    #[inline]
    pub fn next_occupied_from(&self, lo: usize) -> Option<u32> {
        let pm = self.row_mask();
        next_bit_from(&pm, lo)
    }

    #[inline]
    pub fn prev_occupied_before(&self, hi_excl: usize) -> Option<u32> {
        let pm = self.row_mask();
        prev_bit_before(&pm, hi_excl)
    }

    #[inline]
    pub fn get_cell(&self, ro: usize) -> Option<&Cell> {
        if self.dirty_mut
            && let MixedSlabRepr::Dense(cells) = &self.repr
        {
            return cells.get(ro).filter(|c| !c.is_blank());
        }
        if ro >= self.len as usize {
            return None;
        }
        let w = ro / 64;
        let bit = 1u64 << (ro % 64);
        if (self.present[w] & bit) == 0 {
            return None;
        }
        match &self.repr {
            MixedSlabRepr::Sparse(items) => {
                let rank = rank_in_rows(&self.present, w, bit);
                items.get(rank)
            }
            MixedSlabRepr::Dense(cells) => cells.get(ro),
        }
    }

    #[inline]
    pub fn for_each_occupied(&self, mut f: impl FnMut(usize, &Cell)) {
        if self.dirty_mut
            && let MixedSlabRepr::Dense(cells) = &self.repr
        {
            for (ro, c) in cells.iter().enumerate() {
                if !c.is_blank() {
                    f(ro, c);
                }
            }
            return;
        }
        match &self.repr {
            MixedSlabRepr::Sparse(items) => {
                let mut rank = 0usize;
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        f(w * 64 + tz, &items[rank]);
                        rank += 1;
                    }
                }
            }
            MixedSlabRepr::Dense(cells) => {
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        let ro = w * 64 + tz;
                        f(ro, &cells[ro]);
                    }
                }
            }
        }
    }

    fn expand_to_dense(&mut self) -> &mut Vec<Cell> {
        if !matches!(self.repr, MixedSlabRepr::Dense(_)) {
            let len = self.len as usize;
            let mut out = Vec::with_capacity(len.max(64).min(PAGE_ROWS as usize));
            out.resize(len, Cell::default());
            if let MixedSlabRepr::Sparse(items) = &mut self.repr {
                let mut iter = std::mem::take(items).into_iter();
                for w in 0..4 {
                    let mut bits = self.present[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        if let Some(c) = iter.next() {
                            out[w * 64 + tz] = c;
                        }
                    }
                }
            }
            self.repr = MixedSlabRepr::Dense(out);
        }
        match &mut self.repr {
            MixedSlabRepr::Dense(v) => v,
            _ => unreachable!(),
        }
    }

    fn maybe_compact_sparse(&mut self) {
        if self.dirty_mut || matches!(self.repr, MixedSlabRepr::Sparse(_)) {
            return;
        }
        let occ = self.occupied_count();
        if occ <= SPARSE_SLAB_CAP / 2 {
            let mut items = Vec::with_capacity(occ);
            self.for_each_occupied(|_, c| items.push(c.clone()));
            self.repr = MixedSlabRepr::Sparse(items);
        }
    }

    /// Recompute bitmaps after external `&mut Cell` mutation via legacy `get_mut`.
    pub(crate) fn refresh_masks_from_dense(&mut self) {
        self.dense_cache.take();
        self.dirty_mut = false;
        let MixedSlabRepr::Dense(cells) = &mut self.repr else {
            return;
        };
        while cells.last().is_some_and(Cell::is_blank) {
            cells.pop();
        }
        self.len = cells.len() as u16;
        self.present = [0; 4];
        self.formula_mask = [0; 4];
        for (r, c) in cells.iter().enumerate() {
            if !c.is_blank() {
                self.present[r / 64] |= 1u64 << (r % 64);
                if c.formula.is_some() {
                    self.formula_mask[r / 64] |= 1u64 << (r % 64);
                }
            }
        }
    }

    pub(crate) fn get_mut_cell_legacy(&mut self, ro: u32) -> Option<&mut Cell> {
        if self.dirty_mut {
            self.refresh_masks_from_dense();
        }
        self.dense_cache.take();
        self.dirty_mut = true;
        let idx = ro as usize;
        let vec = self.expand_to_dense();
        vec.get_mut(idx)
    }

    pub fn set(&mut self, ro: u32, cell: Cell) -> (bool, bool) {
        debug_assert!(!cell.is_blank() && ro < PAGE_ROWS);
        if self.dirty_mut {
            self.refresh_masks_from_dense();
        }
        self.dense_cache.take();
        let idx = ro as usize;
        let w = idx / 64;
        let bit = 1u64 << (idx % 64);
        let existed = (self.present[w] & bit) != 0;
        let old_f = (self.formula_mask[w] & bit) != 0;
        let rank_before = rank_in_rows(&self.present, w, bit);
        self.present[w] |= bit;
        if cell.formula.is_some() {
            self.formula_mask[w] |= bit;
        } else {
            self.formula_mask[w] &= !bit;
        }
        if (idx + 1) as u16 > self.len {
            self.len = (idx + 1) as u16;
        }
        match &mut self.repr {
            MixedSlabRepr::Sparse(items) => {
                if existed {
                    items[rank_before] = cell;
                } else {
                    items.insert(rank_before, cell);
                    if items.len() > SPARSE_SLAB_CAP {
                        self.expand_to_dense();
                    }
                }
            }
            MixedSlabRepr::Dense(cells) => {
                grow_slab_vec(cells, self.len as usize, Cell::default());
                cells[idx] = cell;
            }
        }
        (existed, old_f)
    }

    pub fn set_formula_in_place(&mut self, ro: u32, formula: Arc<str>) -> bool {
        if self.dirty_mut {
            self.refresh_masks_from_dense();
        }
        let idx = ro as usize;
        if idx >= self.len as usize {
            return false;
        }
        let w = idx / 64;
        let bit = 1u64 << (idx % 64);
        if (self.present[w] & bit) == 0 {
            return false;
        }
        self.dense_cache.take();
        let had_formula = (self.formula_mask[w] & bit) != 0;
        self.formula_mask[w] |= bit;
        match &mut self.repr {
            MixedSlabRepr::Sparse(items) => {
                let rank = rank_in_rows(&self.present, w, bit);
                items[rank].formula = Some(formula);
            }
            MixedSlabRepr::Dense(cells) => {
                cells[idx].formula = Some(formula);
            }
        }
        !had_formula
    }

    pub fn set_cached_value(&mut self, ro: u32, value: Value) -> bool {
        if self.dirty_mut {
            self.refresh_masks_from_dense();
        }
        let idx = ro as usize;
        if idx >= self.len as usize {
            return false;
        }
        let w = idx / 64;
        let bit = 1u64 << (idx % 64);
        if (self.present[w] & bit) == 0 {
            return false;
        }
        self.dense_cache.take();
        match &mut self.repr {
            MixedSlabRepr::Sparse(items) => {
                let rank = rank_in_rows(&self.present, w, bit);
                items[rank].value = value;
                if items[rank].is_blank() {
                    items.remove(rank);
                    self.present[w] &= !bit;
                    self.formula_mask[w] &= !bit;
                    self.len = highest_bit_plus_one(&self.present) as u16;
                    return true;
                }
                false
            }
            MixedSlabRepr::Dense(cells) => {
                cells[idx].value = value;
                if cells[idx].is_blank() {
                    self.present[w] &= !bit;
                    self.formula_mask[w] &= !bit;
                    self.len = highest_bit_plus_one(&self.present) as u16;
                    cells.truncate(self.len as usize);
                    self.maybe_compact_sparse();
                    return true;
                }
                false
            }
        }
    }

    pub fn remove(&mut self, ro: u32) -> Option<Cell> {
        if self.dirty_mut {
            self.refresh_masks_from_dense();
        }
        let idx = ro as usize;
        if idx >= self.len as usize {
            return None;
        }
        let w = idx / 64;
        let bit = 1u64 << (idx % 64);
        if (self.present[w] & bit) == 0 {
            return None;
        }
        self.dense_cache.take();
        let rank = rank_in_rows(&self.present, w, bit);
        self.present[w] &= !bit;
        self.formula_mask[w] &= !bit;
        self.len = highest_bit_plus_one(&self.present) as u16;
        let old = match &mut self.repr {
            MixedSlabRepr::Sparse(items) => items.remove(rank),
            MixedSlabRepr::Dense(cells) => {
                let old = std::mem::take(&mut cells[idx]);
                cells.truncate(self.len as usize);
                old
            }
        };
        if self.present == [0; 4] {
            self.repr = MixedSlabRepr::Sparse(Vec::new());
        } else {
            self.maybe_compact_sparse();
        }
        Some(old)
    }

    pub fn clear_range(&mut self, lo: usize, hi_incl: usize) -> (usize, [u64; 4]) {
        if self.dirty_mut {
            self.refresh_masks_from_dense();
        }
        let end = (hi_incl + 1).min(self.len as usize);
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
        let removed = (removed_mask[0].count_ones()
            + removed_mask[1].count_ones()
            + removed_mask[2].count_ones()
            + removed_mask[3].count_ones()) as usize;
        if removed == 0 {
            return (0, [0; 4]);
        }
        self.dense_cache.take();
        match &mut self.repr {
            MixedSlabRepr::Sparse(items) => {
                let lo_rank = count_bits_below(&self.present, lo as u32);
                items.drain(lo_rank..lo_rank + removed);
            }
            MixedSlabRepr::Dense(cells) => {
                for c in &mut cells[lo..end] {
                    *c = Cell::default();
                }
            }
        }
        for w in 0..4 {
            self.present[w] &= !m[w];
            self.formula_mask[w] &= !m[w];
        }
        self.len = highest_bit_plus_one(&self.present) as u16;
        if let MixedSlabRepr::Dense(cells) = &mut self.repr {
            cells.truncate(self.len as usize);
        }
        if self.present == [0; 4] {
            self.repr = MixedSlabRepr::Sparse(Vec::new());
        } else {
            self.maybe_compact_sparse();
        }
        (removed, removed_mask)
    }
}

/// Typed 256-row column slab:
/// - `Numbers`: pure `f64` column page (8 B/row + 32 B presence bitmap, zero formula overhead).
/// - `Values`: non-formula value column page (`Sparse` / 1-byte `DictText` / `Dense`, `[u64; 4]` bitmap).
/// - `Mixed`: formula-bearing column page (`Sparse` / `Dense`, `[u64; 4]` occupancy + formula bitmaps).
#[derive(Clone, Debug, PartialEq)]
pub enum ColSlab {
    Numbers(NumberSlab),
    Values(ValueSlab),
    Mixed(MixedSlab),
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
        Self::Values(ValueSlab::default())
    }

    #[inline]
    pub fn new_mixed() -> Self {
        Self::Mixed(MixedSlab::default())
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
                Self::Values(vs) => vs.heap_payload_bytes(),
                Self::Mixed(cs) => cs.heap_payload_bytes(),
            }
    }

    #[inline]
    pub fn contains(&self, ro: u32) -> bool {
        match self {
            Self::Numbers(ns) => ns.contains(ro),
            Self::Values(vs) => vs.contains(ro),
            Self::Mixed(cs) => cs.contains(ro),
        }
    }

    #[inline]
    pub fn get_view(&self, ro: u32) -> Option<CellView<'_>> {
        match self {
            Self::Numbers(ns) => ns.get(ro).map(CellView::Number),
            Self::Values(vs) => vs.get_val(ro as usize).map(CellView::Value),
            Self::Mixed(cs) => cs.get_cell(ro as usize).map(CellView::Cell),
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
            Self::Values(vs) => match vs.get_val(ro as usize)? {
                Value::Number(n) => Some(*n),
                _ => None,
            },
            Self::Mixed(cs) => match &cs.get_cell(ro as usize)?.value {
                Value::Number(n) => Some(*n),
                _ => None,
            },
        }
    }

    #[inline]
    pub fn get_f64_plain(&self, ro: u32) -> Option<f64> {
        match self {
            Self::Numbers(ns) => ns.get(ro),
            Self::Values(vs) => match vs.get_val(ro as usize)? {
                Value::Number(n) => Some(*n),
                _ => None,
            },
            Self::Mixed(cs) => {
                let c = cs.get_cell(ro as usize)?;
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
            Self::Values(vs) => vs.get_val(ro as usize).map(Cow::Borrowed),
            Self::Mixed(cs) => cs.get_cell(ro as usize).map(|c| Cow::Borrowed(&c.value)),
        }
    }

    #[inline]
    pub fn value(&self, ro: u32) -> Value {
        match self {
            Self::Numbers(ns) => ns.get(ro).map_or(Value::Empty, Value::Number),
            Self::Values(vs) => vs.get_val(ro as usize).cloned().unwrap_or_default(),
            Self::Mixed(cs) => cs
                .get_cell(ro as usize)
                .map(|c| c.value.clone())
                .unwrap_or_default(),
        }
    }

    #[inline]
    pub fn formula_of(&self, ro: u32) -> Option<&str> {
        match self {
            Self::Numbers(_) | Self::Values(_) => None,
            Self::Mixed(cs) => cs.get_cell(ro as usize)?.formula.as_deref(),
        }
    }

    #[inline]
    pub fn formula_arc_of(&self, ro: u32) -> Option<&Arc<str>> {
        match self {
            Self::Numbers(_) | Self::Values(_) => None,
            Self::Mixed(cs) => cs.get_cell(ro as usize)?.formula.as_ref(),
        }
    }

    #[inline]
    pub fn row_mask(&self) -> [u64; 4] {
        match self {
            Self::Numbers(ns) => ns.present,
            Self::Values(vs) => vs.present,
            Self::Mixed(cs) => cs.row_mask(),
        }
    }

    #[inline]
    pub fn row_mask_from(&self, begin: usize) -> [u64; 4] {
        match self {
            Self::Numbers(ns) => mask_from_offset(&ns.present, begin),
            Self::Values(vs) => vs.row_mask_from(begin),
            Self::Mixed(cs) => cs.row_mask_from(begin),
        }
    }

    #[inline]
    pub fn occupied_count(&self) -> usize {
        match self {
            Self::Numbers(ns) => ns.occupied_count(),
            Self::Values(vs) => vs.occupied_count(),
            Self::Mixed(cs) => cs.occupied_count(),
        }
    }

    #[inline]
    pub fn first_last_occupied(&self) -> Option<(u32, u32)> {
        match self {
            Self::Numbers(ns) => ns.first_last_occupied(),
            Self::Values(vs) => vs.first_last_occupied(),
            Self::Mixed(cs) => cs.first_last_occupied(),
        }
    }

    #[inline]
    pub fn has_any_in(&self, lo: usize, hi_excl: usize) -> bool {
        match self {
            Self::Numbers(ns) => ns.has_any_in(lo, hi_excl),
            Self::Values(vs) => vs.has_any_in(lo, hi_excl),
            Self::Mixed(cs) => cs.has_any_in(lo, hi_excl),
        }
    }

    #[inline]
    pub fn next_occupied_from(&self, lo: usize) -> Option<u32> {
        match self {
            Self::Numbers(ns) => ns.next_occupied_from(lo),
            Self::Values(vs) => vs.next_occupied_from(lo),
            Self::Mixed(cs) => cs.next_occupied_from(lo),
        }
    }

    #[inline]
    pub fn prev_occupied_before(&self, hi_excl: usize) -> Option<u32> {
        match self {
            Self::Numbers(ns) => ns.prev_occupied_before(hi_excl),
            Self::Values(vs) => vs.prev_occupied_before(hi_excl),
            Self::Mixed(cs) => cs.prev_occupied_before(hi_excl),
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
        if len_a == 0 {
            return true;
        }
        if a_start == b_start {
            let m = row_range_mask(a_start as u32, (a_end - 1) as u32);
            let ma = self.row_mask();
            let mb = other.row_mask();
            if (ma[0] & m[0]) != (mb[0] & m[0])
                || (ma[1] & m[1]) != (mb[1] & m[1])
                || (ma[2] & m[2]) != (mb[2] & m[2])
                || (ma[3] & m[3]) != (mb[3] & m[3])
            {
                return false;
            }
            if let (Self::Numbers(na), Self::Numbers(nb)) = (self, other) {
                let sa = &na.values[a_start..a_end.min(na.values.len())];
                let sb = &nb.values[b_start..b_end.min(nb.values.len())];
                return sa == sb;
            }
        }
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

    pub fn promote_to_values(&mut self) -> &mut ValueSlab {
        match self {
            Self::Numbers(ns) => {
                let len = ns.values.len();
                let occ = ns.occupied_count();
                let vs = if occ <= SPARSE_SLAB_CAP {
                    let mut items = Vec::with_capacity(occ);
                    for w in 0..4 {
                        let mut bits = ns.present[w];
                        while bits != 0 {
                            let tz = bits.trailing_zeros() as usize;
                            bits &= bits - 1;
                            items.push(Value::Number(ns.values[w * 64 + tz]));
                        }
                    }
                    ValueSlab {
                        present: ns.present,
                        len: len as u16,
                        repr: ValueSlabRepr::Sparse(items),
                        dense_cache: OnceLock::new(),
                    }
                } else {
                    let mut vals = Vec::with_capacity(len);
                    for r in 0..len {
                        if (ns.present[r / 64] & (1u64 << (r % 64))) != 0 {
                            vals.push(Value::Number(ns.values[r]));
                        } else {
                            vals.push(Value::Empty);
                        }
                    }
                    ValueSlab {
                        present: ns.present,
                        len: len as u16,
                        repr: ValueSlabRepr::Dense(vals),
                        dense_cache: OnceLock::new(),
                    }
                };
                *self = Self::Values(vs);
                match self {
                    Self::Values(v) => v,
                    _ => unreachable!(),
                }
            }
            Self::Values(v) => v,
            Self::Mixed(_) => unreachable!("cannot demote Mixed via promote_to_values"),
        }
    }

    pub fn promote_to_mixed(&mut self) -> &mut MixedSlab {
        match self {
            Self::Numbers(ns) => {
                let len = ns.values.len();
                let occ = ns.occupied_count();
                let ms = if occ <= SPARSE_SLAB_CAP {
                    let mut items = Vec::with_capacity(occ);
                    for w in 0..4 {
                        let mut bits = ns.present[w];
                        while bits != 0 {
                            let tz = bits.trailing_zeros() as usize;
                            bits &= bits - 1;
                            items.push(Cell {
                                value: Value::Number(ns.values[w * 64 + tz]),
                                formula: None,
                            });
                        }
                    }
                    MixedSlab {
                        present: ns.present,
                        formula_mask: [0; 4],
                        len: len as u16,
                        dirty_mut: false,
                        repr: MixedSlabRepr::Sparse(items),
                        dense_cache: OnceLock::new(),
                    }
                } else {
                    let mut cells = Vec::with_capacity(len);
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
                    MixedSlab {
                        present: ns.present,
                        formula_mask: [0; 4],
                        len: len as u16,
                        dirty_mut: false,
                        repr: MixedSlabRepr::Dense(cells),
                        dense_cache: OnceLock::new(),
                    }
                };
                *self = Self::Mixed(ms);
            }
            Self::Values(vs) => {
                let owned = std::mem::take(vs);
                let present = owned.present;
                let len = owned.len;
                let occ = owned.occupied_count();
                let ms = if occ <= SPARSE_SLAB_CAP {
                    let mut items = Vec::with_capacity(occ);
                    owned.for_each_occupied(|_, v| {
                        items.push(Cell {
                            value: v.clone(),
                            formula: None,
                        });
                    });
                    MixedSlab {
                        present,
                        formula_mask: [0; 4],
                        len,
                        dirty_mut: false,
                        repr: MixedSlabRepr::Sparse(items),
                        dense_cache: OnceLock::new(),
                    }
                } else {
                    let cells: Vec<Cell> = owned
                        .into_vec()
                        .into_iter()
                        .map(|value| Cell {
                            value,
                            formula: None,
                        })
                        .collect();
                    MixedSlab {
                        present,
                        formula_mask: [0; 4],
                        len,
                        dirty_mut: false,
                        repr: MixedSlabRepr::Dense(cells),
                        dense_cache: OnceLock::new(),
                    }
                };
                *self = Self::Mixed(ms);
            }
            Self::Mixed(_) => {}
        }
        match self {
            Self::Mixed(c) => c,
            _ => unreachable!(),
        }
    }

    /// Automatically demote `Mixed -> Numbers / Values` when all formulas are cleared,
    /// and `Values -> Numbers` when all remaining occupied cells are plain numbers.
    pub fn try_demote(&mut self) {
        match self {
            Self::Numbers(_) => {}
            Self::Values(vs) => {
                if vs.all_numbers() {
                    *self = Self::Numbers(vs.to_number_slab());
                }
            }
            Self::Mixed(cs) => {
                if cs.is_empty() || cs.has_formulas() {
                    return;
                }
                if cs.dirty_mut {
                    cs.refresh_masks_from_dense();
                }
                let present = cs.present;
                let len = cs.len as usize;
                let mut all_num = true;
                cs.for_each_occupied(|_, c| {
                    if !matches!(c.value, Value::Number(_)) {
                        all_num = false;
                    }
                });
                if all_num {
                    let mut values = vec![0.0; len];
                    cs.for_each_occupied(|ro, c| {
                        if let Value::Number(n) = c.value {
                            values[ro] = n;
                        }
                    });
                    *self = Self::Numbers(NumberSlab { present, values });
                } else {
                    let owned = std::mem::take(cs);
                    let vals: Vec<Value> = owned.into_vec().into_iter().map(|c| c.value).collect();
                    *self = Self::Values(ValueSlab::from_vec(vals));
                }
            }
        }
    }

    /// Write a non-blank `cell` at row offset `ro`, upgrading/demoting the slab in place if necessary.
    /// Returns `(existed, old_has_formula)`.
    #[inline]
    pub fn set(&mut self, ro: u32, cell: Cell) -> (bool, bool) {
        debug_assert!(!cell.is_blank() && ro < PAGE_ROWS);
        if cell.formula.is_none() {
            if let Value::Number(n) = cell.value {
                return match self {
                    Self::Numbers(ns) => (ns.set(ro, n), false),
                    Self::Values(vs) => {
                        let existed = vs.set(ro, Value::Number(n));
                        if existed && vs.all_numbers() {
                            *self = Self::Numbers(vs.to_number_slab());
                        }
                        (existed, false)
                    }
                    Self::Mixed(cs) => {
                        let (existed, old_f) = cs.set(ro, cell);
                        if old_f && !cs.has_formulas() {
                            self.try_demote();
                        }
                        (existed, old_f)
                    }
                };
            }
            match self {
                Self::Numbers(_) => {
                    let vs = self.promote_to_values();
                    (vs.set(ro, cell.value), false)
                }
                Self::Values(vs) => (vs.set(ro, cell.value), false),
                Self::Mixed(cs) => {
                    let (existed, old_f) = cs.set(ro, cell);
                    if old_f && !cs.has_formulas() {
                        self.try_demote();
                    }
                    (existed, old_f)
                }
            }
        } else {
            let cs = self.promote_to_mixed();
            cs.set(ro, cell)
        }
    }

    #[inline]
    pub fn set_formula_cache(&mut self, ro: u32, value: Value) -> bool {
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
                    vs.set(ro, other);
                    false
                }
            },
            Self::Values(vs) => {
                if !vs.contains(ro) {
                    return false;
                }
                if value.is_empty() {
                    vs.remove(ro);
                    if vs.all_numbers() {
                        *self = Self::Numbers(vs.to_number_slab());
                    }
                    true
                } else {
                    vs.set(ro, value);
                    false
                }
            }
            Self::Mixed(cs) => cs.set_cached_value(ro, value),
        }
    }

    pub fn remove(&mut self, ro: u32) -> Option<Cell> {
        match self {
            Self::Numbers(ns) => ns.remove(ro).map(|n| Cell {
                value: Value::Number(n),
                formula: None,
            }),
            Self::Values(vs) => {
                let old = vs.remove(ro)?;
                if !matches!(old, Value::Number(_)) && vs.all_numbers() {
                    *self = Self::Numbers(vs.to_number_slab());
                }
                Some(Cell {
                    value: old,
                    formula: None,
                })
            }
            Self::Mixed(cs) => {
                let old = cs.remove(ro)?;
                if old.formula.is_some() && !cs.has_formulas() {
                    self.try_demote();
                }
                Some(old)
            }
        }
    }

    pub fn clear_range(&mut self, lo: usize, hi_incl: usize) -> (usize, [u64; 4]) {
        match self {
            Self::Numbers(ns) => ns.clear_range(lo, hi_incl),
            Self::Values(vs) => {
                let res = vs.clear_range(lo, hi_incl);
                if res.0 > 0 && vs.all_numbers() {
                    *self = Self::Numbers(vs.to_number_slab());
                }
                res
            }
            Self::Mixed(cs) => {
                let res = cs.clear_range(lo, hi_incl);
                if res.0 > 0 && !cs.has_formulas() {
                    self.try_demote();
                }
                res
            }
        }
    }

    /// Extract rows `lo..len` into a compact tail slab, clearing them from `self`.
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
                let bits = vs.row_mask_from(lo);
                if bits == [0; 4] {
                    return None;
                }
                let mut tail = vec![Value::Empty; vs.len()];
                for w in (lo / 64)..4 {
                    let mut b = bits[w];
                    while b != 0 {
                        let tz = b.trailing_zeros() as usize;
                        b &= b - 1;
                        let r = w * 64 + tz;
                        if let Some(v) = vs.get_val(r) {
                            tail[r] = v.clone();
                        }
                    }
                }
                vs.clear_range(lo, PAGE_ROWS as usize - 1);
                if vs.all_numbers() {
                    *self = Self::Numbers(vs.to_number_slab());
                }
                let mut tail_slab = Self::Values(ValueSlab::from_vec(tail));
                tail_slab.try_demote();
                Some((tail_slab, bits))
            }
            Self::Mixed(cs) => {
                let bits = cs.row_mask_from(lo);
                if bits == [0; 4] {
                    return None;
                }
                let mut tail = vec![Cell::default(); cs.len()];
                for w in (lo / 64)..4 {
                    let mut b = bits[w];
                    while b != 0 {
                        let tz = b.trailing_zeros() as usize;
                        b &= b - 1;
                        let r = w * 64 + tz;
                        if let Some(c) = cs.get_cell(r) {
                            tail[r] = c.clone();
                        }
                    }
                }
                cs.clear_range(lo, PAGE_ROWS as usize - 1);
                if !cs.has_formulas() {
                    self.try_demote();
                }
                let mut tail_slab = Self::Mixed(MixedSlab::from_vec(tail));
                tail_slab.try_demote();
                Some((tail_slab, bits))
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
        let m = row_range_mask(src_start as u32, (end - 1) as u32);
        let sm = self.row_mask();
        let active = [
            sm[0] & m[0],
            sm[1] & m[1],
            sm[2] & m[2],
            sm[3] & m[3],
        ];
        if active == [0; 4] {
            return;
        }
        if let (Self::Numbers(sa), Self::Numbers(db)) = (&mut *self, &mut *dst) {
            let last_src = prev_bit_before(&active, end).unwrap() as usize;
            let max_dst = dst_off + (last_src - src_start);
            if db.values.len() <= max_dst {
                grow_slab_vec(&mut db.values, max_dst + 1, 0.0);
            }
            for w in (src_start / 64)..=((end - 1) / 64) {
                let mut bits = active[w];
                while bits != 0 {
                    let tz = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    let sr = w * 64 + tz;
                    let dr = dst_off + (sr - src_start);
                    db.values[dr] = sa.values[sr];
                    db.present[dr / 64] |= 1u64 << (dr % 64);
                    sa.values[sr] = 0.0;
                }
            }
            for w in 0..4 {
                sa.present[w] &= !m[w];
            }
            sa.values.truncate(highest_bit_plus_one(&sa.present));
            return;
        }
        for w in (src_start / 64)..=((end - 1) / 64) {
            let mut bits = active[w];
            while bits != 0 {
                let tz = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let sr = w * 64 + tz;
                let dr = (dst_off + (sr - src_start)) as u32;
                if let Some(view) = self.get_view(sr as u32) {
                    dst.set(dr, view.to_cell());
                }
            }
        }
        self.clear_range(src_start, end - 1);
    }

    /// Recompute row mask and max row (+1) for this slab; returns `None` if completely empty.
    pub fn scan_mask(&self) -> Option<([u64; 4], u32)> {
        let pm = self.row_mask();
        if pm == [0; 4] {
            None
        } else {
            Some((pm, highest_bit_plus_one(&pm) as u32))
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
            if let Some(slab) = Arc::get_mut(arc) {
                if let ColSlab::Mixed(cs) = slab
                    && cs.dirty_mut
                {
                    cs.refresh_masks_from_dense();
                }
                slab.try_demote();
            }
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
        Arc::make_mut(slot)
            .promote_to_mixed()
            .get_mut_cell_legacy(r_off)
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
    #[allow(dead_code)]
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

    pub(crate) fn set_formula_in_place(&mut self, r: u32, c: u32, formula: Arc<str>) -> bool {
        let (pi, ro) = Self::split(r);
        if !self.page(pi).is_some_and(|p| p.contains(ro, c)) {
            return false;
        }
        let has_qual = formula.contains('!');
        let page = self.page_mut_existing(pi).unwrap();
        page.may_have_formula = true;
        page.may_have_qualified_formula |= has_qual;
        let slot = page.cols.get_mut(c).unwrap();
        let added = Arc::make_mut(slot)
            .promote_to_mixed()
            .set_formula_in_place(ro, formula);
        if added && let Some(fc) = self.formula_count.get_mut() {
            *fc += 1;
        }
        self.version.advance();
        true
    }

    pub(crate) fn strip_formulas_in_rect(
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
            .filter(|(_, p)| p.may_have_formula && p.iter_nonempty_cols(c1, c2).next().is_some())
            .map(|(pi, _)| pi)
            .collect();
        if hit_pages.is_empty() {
            return 0;
        }
        let mut stripped = 0usize;
        let mut removed_cells = 0usize;
        for pi in hit_pages {
            let base = pi * PAGE_ROWS;
            let lo = r1.saturating_sub(base);
            let hi = (r2 - base).min(PAGE_ROWS - 1);
            let rm = row_range_mask(lo, hi);
            let Some(page) = self.page_mut_existing(pi) else {
                continue;
            };
            let cols: Vec<u32> = page.iter_nonempty_cols(c1, c2).collect();
            let mut page_changed = false;
            for col in cols {
                let Some(slot) = page.cols.get_mut(col) else {
                    continue;
                };
                if !slot.is_mixed() {
                    continue;
                }
                let ColSlab::Mixed(cs_ro) = slot.as_ref() else {
                    continue;
                };
                if !cs_ro.dirty_mut
                    && (cs_ro.formula_mask[0] & rm[0]) == 0
                    && (cs_ro.formula_mask[1] & rm[1]) == 0
                    && (cs_ro.formula_mask[2] & rm[2]) == 0
                    && (cs_ro.formula_mask[3] & rm[3]) == 0
                {
                    continue;
                }
                let slab = Arc::make_mut(slot);
                let ColSlab::Mixed(cs) = slab else {
                    unreachable!()
                };
                if cs.dirty_mut {
                    cs.refresh_masks_from_dense();
                }
                let target_f = [
                    cs.formula_mask[0] & rm[0],
                    cs.formula_mask[1] & rm[1],
                    cs.formula_mask[2] & rm[2],
                    cs.formula_mask[3] & rm[3],
                ];
                let cnt = (target_f[0].count_ones()
                    + target_f[1].count_ones()
                    + target_f[2].count_ones()
                    + target_f[3].count_ones()) as usize;
                if cnt == 0 {
                    continue;
                }
                stripped += cnt;
                let mut blank_mask = [0u64; 4];
                match &mut cs.repr {
                    MixedSlabRepr::Sparse(items) => {
                        let mut new_items = Vec::with_capacity(items.len());
                        let mut rank = 0usize;
                        for w in 0..4 {
                            let mut bits = cs.present[w];
                            while bits != 0 {
                                let tz = bits.trailing_zeros();
                                let bit = 1u64 << tz;
                                bits &= bits - 1;
                                let mut c = items[rank].clone();
                                rank += 1;
                                if (target_f[w] & bit) != 0 {
                                    c.formula = None;
                                }
                                if c.is_blank() {
                                    cs.present[w] &= !bit;
                                    blank_mask[w] |= bit;
                                    removed_cells += 1;
                                } else {
                                    new_items.push(c);
                                }
                            }
                            cs.formula_mask[w] &= !target_f[w];
                        }
                        cs.len = highest_bit_plus_one(&cs.present) as u16;
                        *items = new_items;
                    }
                    MixedSlabRepr::Dense(cells) => {
                        for w in 0..4 {
                            let mut bits = target_f[w];
                            while bits != 0 {
                                let tz = bits.trailing_zeros() as usize;
                                let bit = 1u64 << tz;
                                bits &= bits - 1;
                                let ro = w * 64 + tz;
                                cells[ro].formula = None;
                                if cells[ro].is_blank() {
                                    cs.present[w] &= !bit;
                                    blank_mask[w] |= bit;
                                    removed_cells += 1;
                                }
                            }
                            cs.formula_mask[w] &= !target_f[w];
                        }
                        cs.len = highest_bit_plus_one(&cs.present) as u16;
                        cells.truncate(cs.len as usize);
                        cs.maybe_compact_sparse();
                    }
                }
                cs.dense_cache.take();
                if blank_mask != [0; 4] {
                    cleared.or_page_mask(pi, col, blank_mask);
                }
                if !cs.has_formulas() {
                    slab.try_demote();
                }
                if slab.is_empty() {
                    page.cols.remove(col);
                }
                page_changed = true;
            }
            if page_changed {
                page.rebuild_masks();
                if page.is_empty() {
                    self.remove_page(pi);
                }
            }
        }
        if stripped > 0 {
            if let Some(fc) = self.formula_count.get_mut() {
                *fc = fc.saturating_sub(stripped);
            }
            if removed_cells > 0 {
                self.len = self.len.saturating_sub(removed_cells);
                if self.len == 0 {
                    self.clear();
                    return stripped;
                }
                self.bounds.take();
            }
            self.version.advance();
        }
        stripped
    }

    pub(crate) fn foreign_formula_cells(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.iter_pages()
            .filter(|(_, page)| page.may_have_qualified_formula)
            .flat_map(|(pi, page)| {
                page.cols.iter_entries().flat_map(move |(col, slab)| {
                    MixedFormulaIter::new(pi * PAGE_ROWS, col, slab.as_ref()).filter(|(_, cell)| {
                        cell.formula.as_deref().is_some_and(|f| f.contains('!'))
                    })
                })
            })
    }

    pub(crate) fn formula_cells(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.iter_pages()
            .filter(|(_, p)| p.may_have_formula)
            .flat_map(|(pi, p)| {
                p.cols
                    .iter_entries()
                    .flat_map(move |(c, slab)| MixedFormulaIter::new(pi * PAGE_ROWS, c, slab.as_ref()))
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
    /// (for row edits) stay untouched in `O(1)`. Whole-page multiples move `Arc<Page>` directly;
    /// non-256-aligned page splits build destination pages in parallel across threads.
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

        if !rows {
            for (pi, page_arc) in source_pages {
                if page_arc.cols.max_col().is_none_or(|m| m < at) {
                    self.insert_page_arc(pi, page_arc);
                    continue;
                }
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
            }
            cleared.retain_page_masks(|pi, col, bits| {
                if col >= at
                    && let Some(dest) = self.page(pi).and_then(|p| p.cols.get(col))
                {
                    let dm = dest.row_mask();
                    for w in 0..4 {
                        bits[w] &= !dm[w];
                    }
                }
            });
            for (pi, col, words) in occupied {
                let dm = self
                    .page(pi)
                    .and_then(|p| p.cols.get(col))
                    .map_or([0; 4], |s| s.row_mask());
                let vac = [
                    words[0] & !dm[0],
                    words[1] & !dm[1],
                    words[2] & !dm[2],
                    words[3] & !dm[3],
                ];
                if vac != [0; 4] {
                    cleared.or_page_mask(pi, col, vac);
                }
            }
            return;
        }

        let mut split_sources: Vec<(u32, Arc<Page>, Vec<(std::ops::Range<usize>, u32, usize)>)> =
            Vec::new();
        for (pi, page_arc) in source_pages {
            let base = pi * PAGE_ROWS;
            if base + PAGE_ROWS <= at {
                self.insert_page_arc(pi, page_arc);
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
                if self.page(dest).is_none() {
                    for (c, slab) in page_arc.cols.iter_entries() {
                        occupied.push((pi, c, slab.row_mask()));
                    }
                    self.insert_page_arc(dest, page_arc);
                    continue;
                }
            }

            let begin = at.saturating_sub(base) as usize;
            let mut page_cells = 0usize;
            for (col, slab) in page_arc.cols.iter_entries() {
                if !slab.is_empty() {
                    occupied.push((pi, col, slab.row_mask_from(begin)));
                    page_cells += slab.occupied_count();
                }
            }
            self.len -= page_cells;

            let mut spans: Vec<(std::ops::Range<usize>, u32, usize)> = Vec::new();
            for r in 0..page_arc.n_rows {
                let Some(target) = map(base + r) else { continue };
                let (dest, offset) = Self::split(target);
                let offset = offset as usize;
                if let Some((source, pid, start)) = spans.last_mut()
                    && *pid == dest
                    && source.end == r as usize
                    && *start + source.len() == offset
                {
                    source.end += 1;
                } else {
                    spans.push((r as usize..r as usize + 1, dest, offset));
                }
            }
            if !spans.is_empty() {
                split_sources.push((pi, page_arc, spans));
            }
        }

        if !split_sources.is_empty() {
            let mut dest_map: BTreeMap<u32, Vec<(usize, std::ops::Range<usize>, usize)>> =
                BTreeMap::new();
            for (src_idx, (_, _, spans)) in split_sources.iter().enumerate() {
                for (src_r, dest_pi, dst_off) in spans {
                    dest_map
                        .entry(*dest_pi)
                        .or_default()
                        .push((src_idx, src_r.clone(), *dst_off));
                }
            }
            let dest_keys: Vec<u32> = dest_map.keys().copied().collect();
            for dpi in dest_keys {
                if let Some(existing) = self.page(dpi).cloned() {
                    let existing_cells: usize = existing
                        .cols
                        .iter_entries()
                        .map(|(_, s)| s.occupied_count())
                        .sum();
                    self.remove_page(dpi);
                    self.len -= existing_cells;
                    let src_idx = split_sources.len();
                    let n_r = existing.n_rows as usize;
                    split_sources.push((dpi, existing, Vec::new()));
                    if n_r > 0 {
                        dest_map
                            .get_mut(&dpi)
                            .unwrap()
                            .insert(0, (src_idx, 0..n_r, 0));
                    }
                }
            }
            let dest_plan: Vec<(u32, Vec<(usize, std::ops::Range<usize>, usize)>)> =
                dest_map.into_iter().collect();

            let build_dest = |dpi: u32,
                              parts: &[(usize, std::ops::Range<usize>, usize)]|
             -> Option<(u32, Arc<Page>, usize)> {
                let mut cols_set: Vec<u32> = Vec::new();
                let mut may_have_formula = false;
                let mut may_have_qualified_formula = false;
                for &(src_idx, _, _) in parts {
                    let sp = &split_sources[src_idx].1;
                    may_have_formula |= sp.may_have_formula;
                    may_have_qualified_formula |= sp.may_have_qualified_formula;
                    for c in sp.iter_nonempty_cols(0, u32::MAX) {
                        cols_set.push(c);
                    }
                }
                cols_set.sort_unstable();
                cols_set.dedup();
                if cols_set.is_empty() {
                    return None;
                }
                let mut page_cols = PageCols::default();
                let mut page_row_mask = [0u64; 4];
                let mut page_n_rows = 0u32;
                let mut page_cells = 0usize;
                let mut actual_mixed = false;

                for col in cols_set {
                    // Single full-slab identity copy fast path
                    if parts.len() == 1
                        && parts[0].1.start == 0
                        && parts[0].2 == 0
                        && let Some(slab) = split_sources[parts[0].0].1.cols.get(col)
                        && parts[0].1.end >= slab.len()
                    {
                        let rm = slab.row_mask();
                        let occ = slab.occupied_count();
                        if occ > 0 {
                            for w in 0..4 {
                                page_row_mask[w] |= rm[w];
                            }
                            page_n_rows = page_n_rows.max(highest_bit_plus_one(&rm) as u32);
                            page_cells += occ;
                            actual_mixed |= slab.is_mixed();
                            page_cols.set(col, slab.clone());
                        }
                        continue;
                    }

                    let mut all_numbers = true;
                    let mut any_mixed_src = false;
                    let mut max_dst_end = 0usize;
                    let mut any_hit = false;
                    for &(src_idx, ref sr, dst_off) in parts {
                        if let Some(slab) = split_sources[src_idx].1.cols.get(col) {
                            let end = sr.end.min(slab.len());
                            if sr.start < end && slab.has_any_in(sr.start, end) {
                                any_hit = true;
                                max_dst_end = max_dst_end.max(dst_off + (end - sr.start));
                                match slab.as_ref() {
                                    ColSlab::Numbers(_) => {}
                                    ColSlab::Values(_) => all_numbers = false,
                                    ColSlab::Mixed(_) => {
                                        all_numbers = false;
                                        any_mixed_src = true;
                                    }
                                }
                            }
                        }
                    }
                    if !any_hit {
                        continue;
                    }

                    let built_slab = if all_numbers {
                        let mut values = vec![0.0; max_dst_end];
                        let mut present = [0u64; 4];
                        for &(src_idx, ref sr, dst_off) in parts {
                            let Some(slab) = split_sources[src_idx].1.cols.get(col) else {
                                continue;
                            };
                            let ColSlab::Numbers(ns) = slab.as_ref() else {
                                unreachable!()
                            };
                            let end = sr.end.min(ns.values.len());
                            if sr.start >= end {
                                continue;
                            }
                            let span = end - sr.start;
                            values[dst_off..dst_off + span]
                                .copy_from_slice(&ns.values[sr.start..end]);
                            let m = row_range_mask(sr.start as u32, (end - 1) as u32);
                            for w in (sr.start / 64)..=((end - 1) / 64) {
                                let mut bits = ns.present[w] & m[w];
                                while bits != 0 {
                                    let tz = bits.trailing_zeros() as usize;
                                    bits &= bits - 1;
                                    let dr = dst_off + (w * 64 + tz - sr.start);
                                    present[dr / 64] |= 1u64 << (dr % 64);
                                }
                            }
                        }
                        values.truncate(highest_bit_plus_one(&present));
                        ColSlab::Numbers(NumberSlab { present, values })
                    } else if !any_mixed_src {
                        let mut out_vals = vec![Value::Empty; max_dst_end];
                        for &(src_idx, ref sr, dst_off) in parts {
                            let Some(slab) = split_sources[src_idx].1.cols.get(col) else {
                                continue;
                            };
                            let end = sr.end.min(slab.len());
                            if sr.start >= end {
                                continue;
                            }
                            let sm = slab.row_mask();
                            let m = row_range_mask(sr.start as u32, (end - 1) as u32);
                            for w in (sr.start / 64)..=((end - 1) / 64) {
                                let mut bits = sm[w] & m[w];
                                while bits != 0 {
                                    let tz = bits.trailing_zeros() as usize;
                                    bits &= bits - 1;
                                    let r = w * 64 + tz;
                                    let dr = dst_off + (r - sr.start);
                                    out_vals[dr] = slab.value(r as u32);
                                }
                            }
                        }
                        let mut s = ColSlab::Values(ValueSlab::from_vec(out_vals));
                        s.try_demote();
                        s
                    } else {
                        let mut out_cells = vec![Cell::default(); max_dst_end];
                        for &(src_idx, ref sr, dst_off) in parts {
                            let Some(slab) = split_sources[src_idx].1.cols.get(col) else {
                                continue;
                            };
                            let end = sr.end.min(slab.len());
                            if sr.start >= end {
                                continue;
                            }
                            let sm = slab.row_mask();
                            let m = row_range_mask(sr.start as u32, (end - 1) as u32);
                            for w in (sr.start / 64)..=((end - 1) / 64) {
                                let mut bits = sm[w] & m[w];
                                while bits != 0 {
                                    let tz = bits.trailing_zeros() as usize;
                                    bits &= bits - 1;
                                    let r = w * 64 + tz;
                                    let dr = dst_off + (r - sr.start);
                                    if let Some(view) = slab.get_view(r as u32) {
                                        out_cells[dr] = view.to_cell();
                                    }
                                }
                            }
                        }
                        let mut s = ColSlab::Mixed(MixedSlab::from_vec(out_cells));
                        s.try_demote();
                        s
                    };

                    let rm = built_slab.row_mask();
                    let occ = built_slab.occupied_count();
                    if occ > 0 {
                        for w in 0..4 {
                            page_row_mask[w] |= rm[w];
                        }
                        page_n_rows = page_n_rows.max(highest_bit_plus_one(&rm) as u32);
                        page_cells += occ;
                        actual_mixed |= built_slab.is_mixed();
                        page_cols.set(col, Arc::new(built_slab));
                    }
                }

                (page_cells > 0).then(|| {
                    (
                        dpi,
                        Arc::new(Page {
                            may_have_qualified_formula: actual_mixed && may_have_qualified_formula,
                            may_have_formula: actual_mixed && may_have_formula,
                            n_rows: page_n_rows,
                            row_mask: page_row_mask,
                            cols: page_cols,
                        }),
                        page_cells,
                    )
                })
            };

            let built: Vec<(u32, Arc<Page>, usize)> = if dest_plan.len() >= 16 {
                let n_threads = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4)
                    .min(8)
                    .min(dest_plan.len());
                let chunk_size = dest_plan.len().div_ceil(n_threads);
                let mut out = Vec::with_capacity(dest_plan.len());
                std::thread::scope(|s| {
                    let mut handles = Vec::with_capacity(n_threads);
                    for chunk in dest_plan.chunks(chunk_size) {
                        let b_ref = &build_dest;
                        handles.push(s.spawn(move || {
                            let mut local = Vec::with_capacity(chunk.len());
                            for (dpi, parts) in chunk {
                                if let Some(res) = b_ref(*dpi, parts) {
                                    local.push(res);
                                }
                            }
                            local
                        }));
                    }
                    for h in handles {
                        if let Ok(local) = h.join() {
                            out.extend(local);
                        }
                    }
                });
                out
            } else {
                dest_plan
                    .iter()
                    .filter_map(|(dpi, parts)| build_dest(*dpi, parts))
                    .collect()
            };

            for (dpi, page_arc, cnt) in built {
                self.insert_page_arc(dpi, page_arc);
                self.len += cnt;
            }
        }

        cleared.retain_page_masks(|pi, col, bits| {
            let base = pi * PAGE_ROWS;
            if base + PAGE_ROWS <= at {
                return;
            }
            if let Some(dest) = self.page(pi).and_then(|p| p.cols.get(col)) {
                let begin = at.saturating_sub(base) as usize;
                let dm = dest.row_mask_from(begin);
                for w in 0..4 {
                    bits[w] &= !dm[w];
                }
            }
        });
        for (pi, col, words) in occupied {
            let dm = self
                .page(pi)
                .and_then(|p| p.cols.get(col))
                .map_or([0; 4], |s| s.row_mask());
            let vac = [
                words[0] & !dm[0],
                words[1] & !dm[1],
                words[2] & !dm[2],
                words[3] & !dm[3],
            ];
            if vac != [0; 4] {
                cleared.or_page_mask(pi, col, vac);
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
            cleared.retain_page_masks(|pi, col, bits| {
                if col >= lo
                    && col < hi
                    && let Some(dest) = self.page(pi).and_then(|p| p.cols.get(col))
                {
                    let dm = dest.row_mask();
                    for w in 0..4 {
                        bits[w] &= !dm[w];
                    }
                }
            });
            for (pi, col, words) in old_occupied {
                let dm = self
                    .page(pi)
                    .and_then(|p| p.cols.get(col))
                    .map_or([0; 4], |s| s.row_mask());
                let vac = [
                    words[0] & !dm[0],
                    words[1] & !dm[1],
                    words[2] & !dm[2],
                    words[3] & !dm[3],
                ];
                if vac != [0; 4] {
                    cleared.or_page_mask(pi, col, vac);
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
        cleared.retain_page_masks(|pi, col, bits| {
            let base = pi * PAGE_ROWS;
            if base + PAGE_ROWS <= lo || base >= hi {
                return;
            }
            if let Some(dest) = self.page(pi).and_then(|p| p.cols.get(col)) {
                let r_lo = lo.saturating_sub(base);
                let r_hi = (hi - 1 - base).min(PAGE_ROWS - 1);
                let bm = row_range_mask(r_lo, r_hi);
                let dm = dest.row_mask();
                for w in 0..4 {
                    bits[w] &= !(dm[w] & bm[w]);
                }
            }
        });
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
                        if let Some((_, dict)) = vs.dict_text() {
                            for v in &dict[1..] {
                                if let Value::Text(t) = v {
                                    total += t.len();
                                }
                            }
                        } else {
                            vs.for_each_occupied(|_, v| {
                                if let Value::Text(t) = v {
                                    total += t.len();
                                }
                            });
                        }
                    }
                    ColSlab::Mixed(cs) => {
                        cs.for_each_occupied(|_, cell| {
                            if let Some(f) = cell.formula.as_deref() {
                                total += f.len();
                            }
                            if let Value::Text(t) = &cell.value {
                                total += t.len();
                            }
                        });
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

        cleared.retain_page_masks(|pi, col, bits| {
            if col < c1 || col > c2 {
                return;
            }
            let base = pi * PAGE_ROWS;
            if base + PAGE_ROWS <= at {
                return;
            }
            if let Some(dest) = self.page(pi).and_then(|p| p.cols.get(col)) {
                let begin = at.saturating_sub(base) as usize;
                let dm = dest.row_mask_from(begin);
                for w in 0..4 {
                    bits[w] &= !dm[w];
                }
            }
        });
        for (pi, col, words) in occupied {
            let dm = self
                .page(pi)
                .and_then(|p| p.cols.get(col))
                .map_or([0; 4], |s| s.row_mask());
            let cleared_mask = [
                words[0] & !dm[0],
                words[1] & !dm[1],
                words[2] & !dm[2],
                words[3] & !dm[3],
            ];
            if cleared_mask != [0; 4] {
                cleared.or_page_mask(pi, col, cleared_mask);
            }
        }
    }

    /// Parallel page-level formula rewrite: visits only `may_have_formula` pages
    /// (and `may_have_qualified_formula` when `foreign_sheet` is `Some`), skips
    /// `ColSlab::Numbers` and `ColSlab::Values` in `O(1)`, clones each modified
    /// `Mixed` column slab at most once (preserving `Sparse` packing), and uses
    /// `std::thread::scope` for large sheets.
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
            let base = pi * PAGE_ROWS;
            for (col, slab) in page.cols.iter_entries() {
                let ColSlab::Mixed(cs) = slab.as_ref() else {
                    continue;
                };
                if !cs.dirty_mut && cs.formula_mask == [0; 4] {
                    continue;
                }
                let mut cloned: Option<MixedSlab> = None;
                for (cell_ref, cell) in MixedFormulaIter::new(base, col, slab.as_ref()) {
                    let Some(old) = cell.formula.as_deref() else {
                        continue;
                    };
                    if foreign_sheet.is_some() && (!old.contains('!') || !matches_foreign(old)) {
                        continue;
                    }
                    if let Some(new_f) = rewrite_one(old) {
                        any_qual |= new_f.contains('!');
                        let ms = cloned.get_or_insert_with(|| cs.clone());
                        ms.set_formula_in_place(cell_ref.row - base, new_f);
                    }
                }
                if let Some(ms) = cloned {
                    updated_cols.push((col, Arc::new(ColSlab::Mixed(ms))));
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
                    let ord_base = (base - r1) as usize;
                    for r_off in 0..=hi {
                        let src_num = order
                            .get(ord_base + r_off)
                            .copied()
                            .filter(|&sr| sr != u32::MAX)
                            .and_then(|src_r| {
                                let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                                let so = src_r % PAGE_ROWS;
                                match src_slabs[sp] {
                                    Some(ColSlab::Numbers(ns)) => ns.get(so),
                                    _ => None,
                                }
                            });
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

                // Fast path 2: non-formula value column -> direct `ColSlab::Values` (Sparse / 1B DictText / Dense)
                if !col_has_formula && push_mode {
                    let mut out_vals = Vec::with_capacity(hi + 1);
                    let old_count = old_slab.map_or(0, ColSlab::occupied_count) as isize;
                    let mut new_count: isize = 0;
                    let mut row_mask = [0u64; 4];
                    let mut vac_mask = [0u64; 4];
                    let ord_base = (base - r1) as usize;
                    for r_off in 0..=hi {
                        let src_view = order
                            .get(ord_base + r_off)
                            .copied()
                            .filter(|&sr| sr != u32::MAX)
                            .and_then(|src_r| {
                                let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                                let so = src_r % PAGE_ROWS;
                                src_slabs[sp].and_then(|s| s.get_view(so))
                            });
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
                    slabs.push((!out_vals.is_empty()).then(|| {
                        let mut s = ColSlab::Values(ValueSlab::from_vec(out_vals));
                        s.try_demote();
                        GatheredSlab {
                            cells: Arc::new(s),
                            row_mask,
                        }
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
                        let ord_base = (base + lo as u32 - r1) as usize;
                        for i in 0..=(hi - lo) {
                            let r_off = lo + i;
                            let src_num = order
                                .get(ord_base + i)
                                .copied()
                                .filter(|&sr| sr != u32::MAX)
                                .and_then(|src_r| {
                                    let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                                    let so = src_r % PAGE_ROWS;
                                    match src_slabs[sp] {
                                        Some(ColSlab::Numbers(ns)) => ns.get(so),
                                        _ => None,
                                    }
                                });
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
                        let ord_base = (base + lo as u32 - r1) as usize;
                        for i in 0..=(hi - lo) {
                            let r_off = lo + i;
                            let src_view = order
                                .get(ord_base + i)
                                .copied()
                                .filter(|&sr| sr != u32::MAX)
                                .and_then(|src_r| {
                                    let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                                    let so = src_r % PAGE_ROWS;
                                    src_slabs[sp].and_then(|s| s.get_view(so))
                                });
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
                        slabs.push((!out_vals.is_empty()).then(|| {
                            let mut s = ColSlab::Values(ValueSlab::from_vec(out_vals));
                            s.try_demote();
                            GatheredSlab {
                                cells: Arc::new(s),
                                row_mask,
                            }
                        }));
                    }
                    continue;
                }

                // General formula-bearing column path -> `ColSlab::Mixed` (auto-demoting if formula-free)
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
                let ord_base = (base + lo as u32 - r1) as usize;
                for i in 0..=(hi - lo) {
                    let r_off = lo + i;
                    let dst_r = base + r_off as u32;
                    let src_pair = order
                        .get(ord_base + i)
                        .copied()
                        .filter(|&sr| sr != u32::MAX)
                        .and_then(|src_r| {
                            let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                            let so = src_r % PAGE_ROWS;
                            src_slabs[sp].and_then(|s| s.get_view(so)).map(|v| (src_r, v))
                        });
                    let old_view = old_slab.and_then(|s| s.get_view(r_off as u32));
                    if let Some((src_r, sv)) = src_pair {
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
                slabs.push((!out_vec.is_empty()).then(|| {
                    let mut s = ColSlab::Mixed(MixedSlab::from_vec(out_vec));
                    s.try_demote();
                    GatheredSlab {
                        cells: Arc::new(s),
                        row_mask,
                    }
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

enum MixedFormulaMode<'a> {
    Empty,
    Sparse {
        items: &'a [Cell],
        present: [u64; 4],
        formula_mask: [u64; 4],
        word: usize,
        rank: usize,
    },
    Dense {
        cells: &'a [Cell],
        formula_mask: [u64; 4],
        word: usize,
    },
    DirtyDense {
        cells: &'a [Cell],
        idx: usize,
    },
}

struct MixedFormulaIter<'a> {
    base: u32,
    col: u32,
    mode: MixedFormulaMode<'a>,
}

impl<'a> MixedFormulaIter<'a> {
    #[inline]
    fn new(base: u32, col: u32, slab: &'a ColSlab) -> Self {
        let mode = match slab {
            ColSlab::Numbers(_) | ColSlab::Values(_) => MixedFormulaMode::Empty,
            ColSlab::Mixed(cs) => {
                if cs.dirty_mut {
                    match &cs.repr {
                        MixedSlabRepr::Dense(cells) => MixedFormulaMode::DirtyDense {
                            cells: cells.as_slice(),
                            idx: 0,
                        },
                        MixedSlabRepr::Sparse(_) => MixedFormulaMode::Empty,
                    }
                } else if cs.formula_mask == [0; 4] {
                    MixedFormulaMode::Empty
                } else {
                    match &cs.repr {
                        MixedSlabRepr::Sparse(items) => MixedFormulaMode::Sparse {
                            items: items.as_slice(),
                            present: cs.present,
                            formula_mask: cs.formula_mask,
                            word: 0,
                            rank: 0,
                        },
                        MixedSlabRepr::Dense(cells) => MixedFormulaMode::Dense {
                            cells: cells.as_slice(),
                            formula_mask: cs.formula_mask,
                            word: 0,
                        },
                    }
                }
            }
        };
        Self { base, col, mode }
    }
}

impl<'a> Iterator for MixedFormulaIter<'a> {
    type Item = (CellRef, &'a Cell);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.mode {
            MixedFormulaMode::Empty => None,
            MixedFormulaMode::Dense {
                cells,
                formula_mask,
                word,
            } => {
                while *word < 4 && formula_mask[*word] == 0 {
                    *word += 1;
                }
                if *word >= 4 {
                    return None;
                }
                let tz = formula_mask[*word].trailing_zeros() as usize;
                formula_mask[*word] &= formula_mask[*word] - 1;
                let ro = *word * 64 + tz;
                let cell = cells.get(ro)?;
                Some((CellRef::new(self.base + ro as u32, self.col), cell))
            }
            MixedFormulaMode::Sparse {
                items,
                present,
                formula_mask,
                word,
                rank,
            } => loop {
                while *word < 4 && present[*word] == 0 {
                    *word += 1;
                }
                if *word >= 4 {
                    return None;
                }
                let tz = present[*word].trailing_zeros();
                let bit = 1u64 << tz;
                present[*word] &= present[*word] - 1;
                let idx = *rank;
                *rank += 1;
                if (formula_mask[*word] & bit) != 0 {
                    let ro = *word as u32 * 64 + tz;
                    return Some((CellRef::new(self.base + ro, self.col), &items[idx]));
                }
            },
            MixedFormulaMode::DirtyDense { cells, idx } => {
                while *idx < cells.len() {
                    let ro = *idx;
                    *idx += 1;
                    if cells[ro].formula.is_some() {
                        return Some((CellRef::new(self.base + ro as u32, self.col), &cells[ro]));
                    }
                }
                None
            }
        }
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


#[cfg(test)]
mod wave5_storage_tests {
    use super::*;

    #[test]
    fn sparse_and_dict_text_slabs_bound_memory_and_demote_mixed_after_formula_removal() {
        let mut store = Store::default();
        // 1) Low-density page (5 text cells across 256 rows) stays Sparse without 256-slot Vec
        for r in [0u32, 50, 100, 180, 255] {
            store.set(
                r,
                0,
                Cell {
                    value: Value::Text(format!("sparse-{r}").into()),
                    formula: None,
                },
            );
        }
        match store.page(0).unwrap().cols.get(0).unwrap().as_ref() {
            ColSlab::Values(vs) => {
                assert!(vs.is_sparse());
                assert!(vs.heap_payload_bytes() <= 8 * std::mem::size_of::<Value>());
            }
            other => panic!("expected Values slab, got {other:?}"),
        }

        // 2) Categorical text column (256 rows, 4 categories) compresses to 1-byte DictText
        let cats: [Arc<str>; 4] = ["华东".into(), "华南".into(), "华北".into(), "西南".into()];
        for r in 0..256u32 {
            store.set(
                r,
                1,
                Cell {
                    value: Value::Text(cats[(r as usize) % 4].clone()),
                    formula: None,
                },
            );
        }
        match store.page(0).unwrap().cols.get(1).unwrap().as_ref() {
            ColSlab::Values(vs) => {
                assert!(vs.is_dict_text());
                assert!(
                    vs.heap_payload_bytes() < 900,
                    "DictText should use < 900 B vs 4,096 B Dense Vec<Value>, got {}",
                    vs.heap_payload_bytes()
                );
            }
            other => panic!("expected Values slab, got {other:?}"),
        }

        // 3) MixedSlab auto-demotes to Numbers when all formulas are stripped or overwritten
        for r in 0..64u32 {
            store.set(
                r,
                2,
                Cell {
                    value: Value::Number(r as f64),
                    formula: (r < 2).then(|| format!("={r}").into()),
                },
            );
        }
        assert!(store.page(0).unwrap().cols.get(2).unwrap().is_mixed());
        let mut cleared = CellJournal::default();
        let stripped = store.strip_formulas_in_rect(0, 2, 10, 2, &mut cleared);
        assert_eq!(stripped, 2);
        assert!(
            matches!(
                store.page(0).unwrap().cols.get(2).unwrap().as_ref(),
                ColSlab::Numbers(_)
            ),
            "stripping all formulas in numeric MixedSlab must auto-demote to ColSlab::Numbers"
        );
    }

    #[test]
    fn parallel_non_aligned_remap_axis_preserves_prefix_cow_and_shifts_cells_and_styles() {
        let mut store = Store::default();
        let mut styles = CellStyles::default();
        let cats: [Arc<str>; 3] = ["甲".into(), "乙".into(), "丙".into()];
        // 6,000 rows (~24 pages >= 16 parallel threshold)
        for r in 0..6_000u32 {
            store.set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
            store.set(
                r,
                1,
                Cell {
                    value: Value::Text(cats[(r as usize) % 3].clone()),
                    formula: None,
                },
            );
            if r % 17 == 0 {
                styles.insert(CellRef::new(r, 0), 7);
            }
        }
        styles.insert_rect(512, 1, 1023, 1, 9);
        let prefix_cell_page = store.page(0).unwrap().clone();
        let prefix_style_page = styles.page(0).unwrap().clone();

        // Insert 5 rows at non-page-aligned row 260
        let mut cleared = CellJournal::default();
        store.remap_axis(
            true,
            260,
            |r| if r >= 260 { Some(r + 5) } else { Some(r) },
            &mut cleared,
        );
        styles.remap_axis(true, 260, |r| if r >= 260 { Some(r + 5) } else { Some(r) });

        // Prefix page 0 (< 256) must remain untouched Arc pointer!
        assert!(Arc::ptr_eq(store.page(0).unwrap(), &prefix_cell_page));
        assert!(Arc::ptr_eq(styles.page(0).unwrap(), &prefix_style_page));

        assert_eq!(store.get_f64(259, 0), Some(259.0));
        assert_eq!(store.get_f64(260, 0), None);
        assert_eq!(store.get_f64(264, 0), None);
        assert_eq!(store.get_f64(265, 0), Some(260.0));
        assert_eq!(store.get_f64(6_004, 0), Some(5_999.0));
        assert_eq!(
            store.value_ref(6_004, 1).as_ref(),
            &Value::Text(cats[5_999 % 3].clone())
        );
        assert_eq!(styles.get(&CellRef::new(255, 0)), Some(&7)); // 255 = 15 * 17
        assert_eq!(styles.get(&CellRef::new(272 + 5, 0)), Some(&7)); // 272 = 16 * 17
        assert_eq!(styles.get(&CellRef::new(517, 1)), Some(&9));
        assert_eq!(styles.get(&CellRef::new(1028, 1)), Some(&9));
    }
}
