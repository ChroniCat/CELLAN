//! 分页列式存储：把单元格按「页」切开，页内按列连续存放。
//!
//! 设计要点：
//! - 页高 `PAGE_ROWS = 256` 行；页目录对常规坐标（`< 8192` 页 = 209 万行）采用定长索引直达 `O(1)`，
//!   辅以 `page_bits: Vec<u64>` 页非空位图，使 `iter` / `iter_rect` 无需拷贝页号做全键排序。
//! - 页内列容器为 `Vec<Option<Arc<Vec<Cell>>>>` + `col_nonempty: Vec<u64>` + `row_mask: [u64; 4]`：
//!   远端空列（如 `XFD`）仅占 8 字节 `None` 槽位、零堆分配，且位图扫描可直接跳过空行/空列。
//! - `CellStyles` 支持整页同样式 `StyleColumn::Uniform(u16)` 压缩，整列/大区域刷格式共享单个 `Arc`。

use crate::model::{Cell, CellJournal, CellRef, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

pub const PAGE_ROWS: u32 = 256;
const DIRECT_PAGE_LIMIT: u32 = 8192;

#[inline]
fn row_range_mask(lo: u32, hi: u32) -> [u64; 4] {
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

/// Page-level style container mirroring `Page`: direct column array + 64-col occupancy bitmap.
#[derive(Clone, Debug, Default)]
pub(crate) struct StylePage {
    pub(crate) col_nonempty: Vec<u64>,
    pub(crate) cols: Vec<Option<Arc<StyleColumn>>>,
}

impl PartialEq for StylePage {
    fn eq(&self, other: &Self) -> bool {
        if self.col_nonempty != other.col_nonempty {
            return false;
        }
        for c in self.iter_nonempty_cols(0, u32::MAX) {
            let a = self.cols.get(c as usize).and_then(Option::as_ref);
            let b = other.cols.get(c as usize).and_then(Option::as_ref);
            match (a, b) {
                (Some(x), Some(y)) => {
                    if !Arc::ptr_eq(x, y) && x != y {
                        return false;
                    }
                }
                (None, None) => {}
                _ => return false,
            }
        }
        true
    }
}

impl Eq for StylePage {}

impl StylePage {
    #[inline]
    fn set_col_bit(col_nonempty: &mut Vec<u64>, c: u32) {
        let w = (c / 64) as usize;
        if col_nonempty.len() <= w {
            col_nonempty.resize(w + 1, 0);
        }
        col_nonempty[w] |= 1u64 << (c % 64);
    }

    #[inline]
    fn clear_col_bit(&mut self, c: u32) {
        let w = (c / 64) as usize;
        if let Some(word) = self.col_nonempty.get_mut(w) {
            *word &= !(1u64 << (c % 64));
            while self.col_nonempty.last() == Some(&0) {
                self.col_nonempty.pop();
            }
        }
    }

    #[inline]
    pub(crate) fn get_col(&self, c: u32) -> Option<&Arc<StyleColumn>> {
        self.cols.get(c as usize)?.as_ref()
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.col_nonempty.is_empty()
    }

    #[inline]
    fn col_count(&self) -> usize {
        self.col_nonempty.iter().map(|w| w.count_ones() as usize).sum()
    }

    #[inline]
    fn max_col(&self) -> Option<u32> {
        for w in (0..self.col_nonempty.len()).rev() {
            if self.col_nonempty[w] != 0 {
                let bit = 63 - self.col_nonempty[w].leading_zeros();
                return Some(w as u32 * 64 + bit);
            }
        }
        None
    }

    fn set_col(&mut self, c: u32, col: Arc<StyleColumn>) -> usize {
        let idx = c as usize;
        if self.cols.len() <= idx {
            self.cols.resize(idx + 1, None);
        }
        let prev_len = self.cols[idx].as_ref().map_or(0, |s| s.len());
        self.cols[idx] = Some(col);
        Self::set_col_bit(&mut self.col_nonempty, c);
        prev_len
    }

    fn remove_col(&mut self, c: u32) -> Option<Arc<StyleColumn>> {
        let idx = c as usize;
        let old = self.cols.get_mut(idx)?.take()?;
        self.clear_col_bit(c);
        while self.cols.last().is_some_and(Option::is_none) {
            self.cols.pop();
        }
        Some(old)
    }

    pub(crate) fn iter_nonempty_cols(&self, c1: u32, c2: u32) -> impl Iterator<Item = u32> + '_ {
        let total_words = self.col_nonempty.len();
        let start_w = (c1 / 64) as usize;
        let end_w = (c2 / 64) as usize;
        let active = if c1 <= c2 && start_w < total_words {
            start_w..=end_w.min(total_words - 1)
        } else {
            1..=0
        };
        active.flat_map(move |w| {
            let mut bits = self.col_nonempty[w];
            if w == start_w {
                bits &= !0u64 << (c1 % 64);
            }
            if w == end_w {
                let hi = c2 % 64;
                if hi < 63 {
                    bits &= (1u64 << (hi + 1)) - 1;
                }
            }
            std::iter::from_fn(move || {
                if bits == 0 {
                    return None;
                }
                let tz = bits.trailing_zeros();
                bits &= bits - 1;
                Some(w as u32 * 64 + tz)
            })
        })
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
        let c_idx = key.col as usize;
        if page.cols.len() <= c_idx {
            page.cols.resize(c_idx + 1, None);
        }
        StylePage::set_col_bit(&mut page.col_nonempty, key.col);
        let slot = page.cols[c_idx].get_or_insert_with(|| Arc::new(StyleColumn::default()));
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
            if page.cols.len() <= c2 as usize {
                page.cols.resize(c2 as usize + 1, None);
            }
            let mut delta: isize = 0;
            if lo == 0 && hi == PAGE_ROWS - 1 {
                for col in c1..=c2 {
                    let prev_len = page.cols[col as usize].as_ref().map_or(0, |pg| pg.len());
                    page.cols[col as usize] = Some(uniform.clone());
                    StylePage::set_col_bit(&mut page.col_nonempty, col);
                    delta += PAGE_ROWS as isize - prev_len as isize;
                }
            } else {
                for col in c1..=c2 {
                    let slot = page.cols[col as usize]
                        .get_or_insert_with(|| Arc::new(StyleColumn::default()));
                    StylePage::set_col_bit(&mut page.col_nonempty, col);
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
        let slot = page.cols.get_mut(key.col as usize)?.as_mut()?;
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
                } else if let Some(slot) = page.cols.get_mut(col as usize).and_then(Option::as_mut) {
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
                } else if let Some(slot) = page.cols.get_mut(col as usize).and_then(Option::as_mut) {
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
                } else if let Some(slot) = page.cols.get_mut(c as usize).and_then(Option::as_mut) {
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
                bytes += std::mem::size_of::<StylePage>()
                    + page.cols.capacity() * std::mem::size_of::<Option<Arc<StyleColumn>>>()
                    + page.col_nonempty.capacity() * std::mem::size_of::<u64>();
                for sc in page.cols.iter().flatten() {
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
            let max_w = pa
                .map_or(0, |p| p.col_nonempty.len())
                .max(pb.map_or(0, |p| p.col_nonempty.len()));
            for w in 0..max_w {
                let mut bits = pa.and_then(|p| p.col_nonempty.get(w)).copied().unwrap_or(0)
                    | pb.and_then(|p| p.col_nonempty.get(w)).copied().unwrap_or(0);
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    bits &= bits - 1;
                    let col = w as u32 * 64 + tz;
                    let ca = pa.and_then(|p| p.get_col(col));
                    let cb = pb.and_then(|p| p.get_col(col));
                    match (ca, cb) {
                        (Some(a), Some(b)) if Arc::ptr_eq(a, b) => {}
                        (Some(a), Some(b)) => {
                            account_col(a);
                            account_col(b);
                        }
                        (Some(a), None) => account_col(a),
                        (None, Some(b)) => account_col(b),
                        (None, None) => {}
                    }
                }
            }
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
            let max_w = pa
                .map_or(0, |p| p.col_nonempty.len())
                .max(pb.map_or(0, |p| p.col_nonempty.len()));
            for w in 0..max_w {
                let mut bits = pa.and_then(|p| p.col_nonempty.get(w)).copied().unwrap_or(0)
                    | pb.and_then(|p| p.col_nonempty.get(w)).copied().unwrap_or(0);
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    bits &= bits - 1;
                    let col = w as u32 * 64 + tz;
                    let ca = pa.and_then(|p| p.get_col(col));
                    let cb = pb.and_then(|p| p.get_col(col));
                    let mask = match (ca, cb) {
                        (None, None) => continue,
                        (Some(a), Some(b)) if Arc::ptr_eq(a, b) => continue,
                        (Some(a), Some(b)) => a.diff_mask(b),
                        (Some(a), None) => a.row_mask(),
                        (None, Some(b)) => b.row_mask(),
                    };
                    if mask != [0; 4] {
                        out.or_page_mask(pi, col, mask);
                    }
                }
            }
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
    /// 本页非空列的位图（每 64 列 1 个 u64），跳过远端空列无需遍历 `cols` 槽位。
    pub(crate) col_nonempty: Vec<u64>,
    /// 按列存放：`cols[c]` 为 `None` 表示该列在本页全空（零堆分配）。
    pub(crate) cols: Vec<Option<Arc<Vec<Cell>>>>,
}

impl Page {
    #[inline]
    fn set_col_bit(col_nonempty: &mut Vec<u64>, c: u32) {
        let w = (c / 64) as usize;
        if col_nonempty.len() <= w {
            col_nonempty.resize(w + 1, 0);
        }
        col_nonempty[w] |= 1u64 << (c % 64);
    }

    #[inline]
    fn clear_col_bit(&mut self, c: u32) {
        let w = (c / 64) as usize;
        if let Some(word) = self.col_nonempty.get_mut(w) {
            *word &= !(1u64 << (c % 64));
            while self.col_nonempty.last() == Some(&0) {
                self.col_nonempty.pop();
            }
        }
    }

    #[inline]
    pub(crate) fn has_col(&self, c: u32) -> bool {
        let w = (c / 64) as usize;
        self.col_nonempty
            .get(w)
            .is_some_and(|&bits| (bits & (1u64 << (c % 64))) != 0)
    }

    pub(crate) fn rebuild_masks(&mut self) {
        self.row_mask = [0; 4];
        self.col_nonempty.clear();
        self.n_rows = 0;
        let mut last_some = 0usize;
        for (c, slot) in self.cols.iter_mut().enumerate() {
            let Some(arc) = slot else { continue };
            let mut has_any = false;
            let mut col_max_r = 0u32;
            for (r, cell) in arc.iter().enumerate() {
                if !cell.is_blank() {
                    has_any = true;
                    col_max_r = r as u32 + 1;
                    self.row_mask[r / 64] |= 1u64 << (r % 64);
                }
            }
            if has_any {
                Self::set_col_bit(&mut self.col_nonempty, c as u32);
                self.n_rows = self.n_rows.max(col_max_r);
                last_some = c + 1;
            } else {
                *slot = None;
            }
        }
        self.cols.truncate(last_some);
    }

    fn ensure(&mut self, r_off: u32, c: u32) -> &mut Vec<Cell> {
        if self.cols.len() <= c as usize {
            self.cols.resize(c as usize + 1, None);
        }
        self.row_mask[(r_off / 64) as usize] |= 1u64 << (r_off % 64);
        Self::set_col_bit(&mut self.col_nonempty, c);
        if r_off >= self.n_rows {
            self.n_rows = r_off + 1;
        }
        let slot = self.cols[c as usize].get_or_insert_with(|| Arc::new(Vec::new()));
        let col = Arc::make_mut(slot);
        if col.len() <= r_off as usize {
            col.resize_with(r_off as usize + 1, Cell::default);
        }
        col
    }

    #[inline]
    fn get(&self, r_off: u32, c: u32) -> Option<&Cell> {
        let col = self.cols.get(c as usize)?.as_ref()?;
        let cell = col.get(r_off as usize)?;
        if cell.is_blank() {
            None
        } else {
            Some(cell)
        }
    }

    #[inline]
    fn get_mut(&mut self, r_off: u32, c: u32) -> Option<&mut Cell> {
        let slot = self.cols.get_mut(c as usize)?.as_mut()?;
        if slot.get(r_off as usize).is_none_or(Cell::is_blank) {
            return None;
        }
        let col = Arc::make_mut(slot);
        col.get_mut(r_off as usize)
    }

    /// Iterate non-empty column indices in `[c1..=c2]` in ascending order via bit-scan.
    pub(crate) fn iter_nonempty_cols(&self, c1: u32, c2: u32) -> impl Iterator<Item = u32> + '_ {
        let total_words = self.col_nonempty.len();
        let start_w = (c1 / 64) as usize;
        let end_w = (c2 / 64) as usize;
        let active = if c1 <= c2 && start_w < total_words {
            start_w..=end_w.min(total_words - 1)
        } else {
            1..=0
        };
        active.flat_map(move |w| {
            let mut bits = self.col_nonempty[w];
            if w == start_w {
                bits &= !0u64 << (c1 % 64);
            }
            if w == end_w {
                let hi = c2 % 64;
                if hi < 63 {
                    bits &= (1u64 << (hi + 1)) - 1;
                }
            }
            std::iter::from_fn(move || {
                if bits == 0 {
                    return None;
                }
                let tz = bits.trailing_zeros();
                bits &= bits - 1;
                Some(w as u32 * 64 + tz)
            })
        })
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
    pub(crate) cells: Arc<Vec<Cell>>,
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
        if page.col_nonempty.is_empty() {
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
                let a = pa.map(|p| p.cols.as_slice()).unwrap_or_default();
                let b = pb.map(|p| p.cols.as_slice()).unwrap_or_default();
                let end = (c2 as usize).saturating_add(1).min(a.len().max(b.len()));
                (c1 as usize..end).all(|c| {
                    let ca = a.get(c).and_then(Option::as_ref);
                    let cb = b.get(c).and_then(Option::as_ref);
                    if match (ca, cb) {
                        (None, None) => true,
                        (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                        _ => false,
                    } {
                        return true;
                    }
                    let sa = ca.map(|v| v.as_slice()).unwrap_or_default();
                    let sb = cb.map(|v| v.as_slice()).unwrap_or_default();
                    let base = p * PAGE_ROWS;
                    let row_end = (r2 - base).saturating_add(1) as usize;
                    (r1.saturating_sub(base) as usize..row_end.min(sa.len().max(sb.len()))).all(|i| {
                        let xa = sa.get(i).filter(|c| !c.is_blank());
                        let xb = sb.get(i).filter(|c| !c.is_blank());
                        if formulas {
                            xa.and_then(|c| c.formula.as_ref()) == xb.and_then(|c| c.formula.as_ref())
                        } else {
                            match (xa, xb) {
                                (Some(va), Some(vb)) => va.value.same_stored_value(&vb.value),
                                (None, None) => true,
                                _ => false,
                            }
                        }
                    })
                })
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

    /// 诊断用：返回 (页数, 实际已分配列块数, 这些槽位里的 Cell 容量)。
    pub fn stats(&self) -> (usize, usize, usize) {
        let mut pages = 0usize;
        let mut slabs = 0usize;
        let mut slots = 0usize;
        for (_, p) in self.iter_pages() {
            pages += 1;
            for col in p.cols.iter().flatten() {
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
    pub fn get(&self, r: u32, c: u32) -> Option<&Cell> {
        let (pi, ro) = Self::split(r);
        self.page(pi)?.get(ro, c)
    }

    pub fn get_mut(&mut self, r: u32, c: u32) -> Option<&mut Cell> {
        let (pi, ro) = Self::split(r);
        if self.page(pi).and_then(|p| p.get(ro, c)).is_none() {
            return None;
        }
        // A caller holding &mut Cell may mutate formula or turn a boundary cell into blank.
        self.formula_count.take();
        Self::invalidate_boundary(&mut self.bounds, r, c);
        self.version.advance();
        let page = self.page_mut_existing(pi)?;
        page.may_have_qualified_formula = true;
        page.may_have_formula = true;
        page.get_mut(ro, c)
    }

    #[inline]
    pub fn contains(&self, r: u32, c: u32) -> bool {
        self.get(r, c).is_some()
    }

    pub(crate) fn set_formula_cache(&mut self, r: u32, c: u32, value: Value) {
        let (pi, ro) = Self::split(r);
        if self.page(pi).and_then(|p| p.get(ro, c)).is_none() {
            return;
        }
        if let Some(cell) = self.page_mut_existing(pi).and_then(|p| p.get_mut(ro, c)) {
            cell.value = value;
            if cell.is_blank() {
                Self::invalidate_boundary(&mut self.bounds, r, c);
            }
            self.version.advance();
        }
    }

    pub(crate) fn foreign_formula_cells(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.iter_pages()
            .filter(|(_, page)| page.may_have_qualified_formula)
            .flat_map(|(pi, page)| {
                page.iter_nonempty_cols(0, u32::MAX).flat_map(move |col| {
                    let cells = page.cols[col as usize].as_ref().map(|v| v.as_slice()).unwrap_or_default();
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
                p.iter_nonempty_cols(0, u32::MAX).flat_map(move |c| {
                    let col = p.cols[c as usize].as_ref().map(|v| v.as_slice()).unwrap_or_default();
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
    pub fn set(&mut self, r: u32, c: u32, cell: Cell) {
        if cell.is_blank() {
            if self.remove(r, c).is_none() {
                self.version.advance();
            }
            return;
        }
        self.version.advance();
        let (pi, ro) = Self::split(r);
        let (existed, old_has_formula) = match self.page(pi).and_then(|p| p.get(ro, c)) {
            Some(old) => (true, old.formula.is_some()),
            None => (false, false),
        };
        let new_has_formula = cell.formula.is_some();
        let new_has_qual = cell.formula.as_deref().is_some_and(|f| f.contains('!'));
        let page = self.page_mut_or_insert(pi);
        page.may_have_qualified_formula |= new_has_qual;
        page.may_have_formula |= new_has_formula;
        let col = page.ensure(ro, c);
        col[ro as usize] = cell;
        if let Some(fc) = self.formula_count.get_mut() {
            match (old_has_formula, new_has_formula) {
                (false, true) => *fc += 1,
                (true, false) => *fc = fc.saturating_sub(1),
                _ => {}
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
        if self.page(pi).and_then(|p| p.get(ro, c)).is_none() {
            return None;
        }
        let page = self.page_mut_existing(pi)?;
        let slot_arc = page.cols.get_mut(c as usize)?.as_mut()?;
        let col = Arc::make_mut(slot_arc);
        let slot = col.get_mut(ro as usize)?;
        if slot.is_blank() {
            return None;
        }
        let old = std::mem::take(slot);
        while col.last().is_some_and(Cell::is_blank) {
            col.pop();
        }
        if col.is_empty() {
            page.cols[c as usize] = None;
            page.clear_col_bit(c);
            while page.cols.last().is_some_and(Option::is_none) {
                page.cols.pop();
            }
        }
        let page_empty = page.col_nonempty.is_empty();
        if page_empty {
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

    /// 单列按页零分配切片迭代：按升序页号返回 `(本段起始行号, &[Cell])`
    pub fn column_slices(&self, r1: u32, r2: u32, c: u32) -> impl Iterator<Item = (u32, &[Cell])> + '_ {
        let p1 = r1 / PAGE_ROWS;
        let p2 = r2 / PAGE_ROWS;
        self.iter_pages_in(p1, p2).filter_map(move |(pi, page)| {
            if r1 > r2 || !page.has_col(c) {
                return None;
            }
            let col = page.cols.get(c as usize)?.as_deref()?;
            let base = pi * PAGE_ROWS;
            let start = r1.saturating_sub(base) as usize;
            let end = ((r2 - base) as usize + 1).min(col.len());
            (start < end).then(|| (base + start as u32, &col[start..end]))
        })
    }

    /// 单列范围内的连续访问：跨页拼接，避免逐格哈希查找
    pub fn column_span(&self, r1: u32, r2: u32, c: u32) -> Vec<&Cell> {
        let mut out = Vec::new();
        for (_, slice) in self.column_slices(r1, r2, c) {
            out.extend(slice.iter());
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
            for col in data.iter_nonempty_cols(c1, c2) {
                if let Some(cells) = data.cols.get(col as usize).and_then(Option::as_ref) {
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
        }
        result
    }

    /// A read-only view of one column's allocated pages, ordered by base row.
    pub(crate) fn column_pages(&self, col: u32) -> Vec<(u32, Arc<Vec<Cell>>)> {
        self.iter_pages()
            .filter_map(|(page, data)| {
                if !data.has_col(col) {
                    return None;
                }
                let cells = data.cols.get(col as usize)?.as_ref()?;
                (!cells.is_empty()).then(|| (page * PAGE_ROWS, cells.clone()))
            })
            .collect()
    }

    /// 按行列升序遍历（页 → 行 → 列）
    pub fn iter(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.iter_rect((0, 0, u32::MAX, u32::MAX))
    }

    pub(crate) fn iter_rect(&self, (r1, c1, r2, c2): (u32, u32, u32, u32)) -> impl Iterator<Item = (CellRef, &Cell)> {
        let valid = r1 <= r2 && c1 <= c2 && self.len > 0;
        let p1 = if valid { r1 / PAGE_ROWS } else { 1 };
        let p2 = if valid { r2 / PAGE_ROWS } else { 0 };
        self.iter_pages_in(p1, p2).flat_map(move |(pi, page)| {
            let base = pi * PAGE_ROWS;
            let start_ro = r1.saturating_sub(base);
            let end_ro = page.n_rows.min((r2 - base).saturating_add(1));
            let has_cols = page.iter_nonempty_cols(c1, c2).next().is_some();
            let mut row_words = if start_ro < end_ro && has_cols {
                page.row_mask
            } else {
                [0; 4]
            };
            if start_ro < end_ro && has_cols {
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
            (0..4usize)
                .flat_map(move |w| {
                    let mut bits = row_words[w];
                    std::iter::from_fn(move || {
                        if bits == 0 {
                            return None;
                        }
                        let tz = bits.trailing_zeros();
                        bits &= bits - 1;
                        Some(w as u32 * 64 + tz)
                    })
                })
                .flat_map(move |ro| {
                    let r_idx = ro as usize;
                    page.iter_nonempty_cols(c1, c2).filter_map(move |col| {
                        let cell = page.cols.get(col as usize)?.as_ref()?.get(r_idx)?;
                        (!cell.is_blank()).then_some((CellRef::new(base + ro, col), cell))
                    })
                })
        })
    }

    /// Stream occupied cells in `(row, col)` order paired with their explicit style ID,
    /// fetching each `(page, col)` style slab once per page directly from `CellStyles` page directory.
    pub(crate) fn iter_with_explicit_style<'a>(
        &'a self,
        styles: &'a CellStyles,
    ) -> impl Iterator<Item = (CellRef, &'a Cell, Option<u16>)> + 'a {
        self.iter_pages().flat_map(move |(pi, page)| {
            let base = pi * PAGE_ROWS;
            let style_page = styles.page(pi);
            let cols: Vec<(u32, &'a [Cell], Option<&'a StyleColumn>)> = page
                .iter_nonempty_cols(0, u32::MAX)
                .filter_map(|col| {
                    let slab = page.cols.get(col as usize)?.as_ref()?.as_slice();
                    let st = style_page.and_then(|sp| sp.get_col(col)).map(Arc::as_ref);
                    Some((col, slab, st))
                })
                .collect();
            let row_words = page.row_mask;
            (0..4usize)
                .flat_map(move |w| {
                    let mut bits = row_words[w];
                    std::iter::from_fn(move || {
                        if bits == 0 {
                            return None;
                        }
                        let tz = bits.trailing_zeros();
                        bits &= bits - 1;
                        Some(w as u32 * 64 + tz)
                    })
                })
                .flat_map(move |ro| {
                    let r_idx = ro as usize;
                    let cols = cols.clone();
                    cols.into_iter().filter_map(move |(col, slab, st)| {
                        let cell = slab.get(r_idx).filter(|c| !c.is_blank())?;
                        let sid = st.and_then(|s| s.get(ro).copied());
                        Some((CellRef::new(base + ro, col), cell, sid))
                    })
                })
        })
    }

    /// Merge-iterate all coordinates that have EITHER a non-blank cell OR an explicit style,
    /// in strict `(row, col)` ascending order.
    pub(crate) fn iter_merged_with_styles<'a>(
        &'a self,
        styles: &'a CellStyles,
    ) -> impl Iterator<Item = (CellRef, Option<&'a Cell>, Option<u16>)> + 'a {
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
            let col_views: Vec<(u32, Option<&'a [Cell]>, Option<&'a StyleColumn>)> = merged_cols
                .into_iter()
                .map(|c| {
                    let cells = page
                        .and_then(|p| p.cols.get(c as usize))
                        .and_then(Option::as_ref)
                        .map(|v| v.as_slice());
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
            (0..4usize)
                .flat_map(move |w| {
                    let mut bits = row_words[w];
                    std::iter::from_fn(move || {
                        if bits == 0 {
                            return None;
                        }
                        let tz = bits.trailing_zeros();
                        bits &= bits - 1;
                        Some(w as u32 * 64 + tz)
                    })
                })
                .flat_map(move |ro| {
                    let r_idx = ro as usize;
                    let col_views = col_views.clone();
                    col_views.into_iter().filter_map(move |(col, slab, sc)| {
                        let cell = slab.and_then(|s| s.get(r_idx)).filter(|c| !c.is_blank());
                        let sid = sc.and_then(|s| s.get(ro).copied());
                        (cell.is_some() || sid.is_some())
                            .then(|| (CellRef::new(base + ro, col), cell, sid))
                    })
                })
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
            for c in page.iter_nonempty_cols(start, u32::MAX) {
                if page.get(ro, c).is_some() {
                    return Some(c);
                }
            }
            None
        } else {
            let end = from_col.checked_sub(1)?;
            let max_w = (end / 64) as usize;
            if page.col_nonempty.is_empty() {
                return None;
            }
            for w in (0..=max_w.min(page.col_nonempty.len() - 1)).rev() {
                let mut bits = page.col_nonempty[w];
                if w == max_w {
                    let hi = end % 64;
                    if hi < 63 {
                        bits &= (1u64 << (hi + 1)) - 1;
                    }
                }
                while bits != 0 {
                    let bit_idx = 63 - bits.leading_zeros();
                    bits &= !(1u64 << bit_idx);
                    let c = w as u32 * 64 + bit_idx;
                    if page.get(ro, c).is_some() {
                        return Some(c);
                    }
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
                let Some(slab) = page.cols.get(col as usize).and_then(Option::as_deref) else {
                    continue;
                };
                let base = pi * PAGE_ROWS;
                let lo = start.saturating_sub(base) as usize;
                for (i, cell) in slab.iter().enumerate().skip(lo) {
                    if !cell.is_blank() {
                        return Some(base + i as u32);
                    }
                }
            }
            None
        } else {
            let end = from_row.checked_sub(1)?;
            let end_p = end / PAGE_ROWS;
            // Scan far_pages in reverse if applicable, then page_bits in reverse!
            if let Some(far) = self.far_pages.as_deref()
                && end_p >= DIRECT_PAGE_LIMIT
            {
                for (&pi, page) in far.range(DIRECT_PAGE_LIMIT..=end_p).rev() {
                    if !page.has_col(col) {
                        continue;
                    }
                    if let Some(slab) = page.cols.get(col as usize).and_then(Option::as_deref) {
                        let base = pi * PAGE_ROWS;
                        let hi = ((end - base) as usize + 1).min(slab.len());
                        for i in (0..hi).rev() {
                            if !slab[i].is_blank() {
                                return Some(base + i as u32);
                            }
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
                        if let Some(slab) = page.cols.get(col as usize).and_then(Option::as_deref) {
                            let base = pi * PAGE_ROWS;
                            let hi = ((end - base) as usize + 1).min(slab.len());
                            for i in (0..hi).rev() {
                                if !slab[i].is_blank() {
                                    return Some(base + i as u32);
                                }
                            }
                        }
                    }
                }
            }
            None
        }
    }

    pub fn iter_values(&self) -> impl Iterator<Item = &Cell> {
        self.iter().map(|(_, c)| c)
    }

    pub(crate) fn stored_cells(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.iter_pages().flat_map(|(p, page)| {
            page.iter_nonempty_cols(0, u32::MAX).flat_map(move |c| {
                let col = page.cols[c as usize].as_ref().map(|v| v.as_slice()).unwrap_or_default();
                col.iter().enumerate().filter_map(move |(r, cell)| {
                    (!cell.is_blank()).then_some((CellRef::new(p * PAGE_ROWS + r as u32, c), cell))
                })
            })
        })
    }

    /// Search sorts its hits afterwards. Visit only allocated non-empty column slabs,
    /// so a distant column does not turn sparse pages into dense grids.
    pub(crate) fn stored_cells_in(&self, (r1, c1, r2, c2): (u32, u32, u32, u32)) -> impl Iterator<Item = (CellRef, &Cell)> {
        let valid = r1 <= r2 && c1 <= c2 && self.len > 0;
        let p1 = if valid { r1 / PAGE_ROWS } else { 1 };
        let p2 = if valid { r2 / PAGE_ROWS } else { 0 };
        self.iter_pages_in(p1, p2).flat_map(move |(p, page)| {
            let base = p * PAGE_ROWS;
            let first = r1.saturating_sub(base) as usize;
            let last = (r2 - base).saturating_add(1) as usize;
            page.iter_nonempty_cols(c1, c2).flat_map(move |c| {
                let cells = page.cols[c as usize].as_ref().map(|v| v.as_slice()).unwrap_or_default();
                cells
                    .get(first..last.min(cells.len()))
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                    .filter_map(move |(i, cell)| {
                        (!cell.is_blank())
                            .then_some((CellRef::new(base + (first + i) as u32, c), cell))
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
                page.iter_nonempty_cols(0, u32::MAX).flat_map(move |col| {
                    let cells = page.cols[col as usize].as_ref().unwrap();
                    let old = old_page
                        .and_then(|op| op.cols.get(col as usize))
                        .and_then(Option::as_ref);
                    let shared = old.is_some_and(|old| Arc::ptr_eq(old, cells));
                    (!shared).then_some(cells).into_iter().flat_map(move |cells| {
                        cells.iter().enumerate().filter_map(move |(row, cell)| {
                            (!cell.is_blank()
                                && old.and_then(|c| c.get(row)).is_none_or(|prior| {
                                    prior.formula != cell.formula
                                        || !prior.value.same_stored_value(&cell.value)
                                }))
                            .then_some(CellRef::new(p * PAGE_ROWS + row as u32, col))
                        })
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
            if (rows && base + PAGE_ROWS <= at) || (!rows && page_arc.cols.len() <= at as usize) {
                self.insert_page_arc(pi, page_arc);
                continue;
            }
            if !rows {
                let mut page = Arc::unwrap_or_clone(page_arc);
                let mut columns: Vec<Option<Arc<Vec<Cell>>>> = Vec::with_capacity(page.cols.len());
                let mut col_nonempty = Vec::new();
                for (c, slab_opt) in std::mem::take(&mut page.cols).into_iter().enumerate() {
                    let Some(slab) = slab_opt else { continue };
                    if c >= at as usize {
                        let mut bits = [0u64; 4];
                        for (r, cell) in slab.iter().enumerate() {
                            if !cell.is_blank() {
                                bits[r / 64] |= 1u64 << (r % 64);
                            }
                        }
                        occupied.push((pi, c as u32, bits));
                    }
                    if let Some(nc) = map(c as u32) {
                        if columns.len() <= nc as usize {
                            columns.resize(nc as usize + 1, None);
                        }
                        columns[nc as usize] = Some(slab);
                        Page::set_col_bit(&mut col_nonempty, nc);
                    } else {
                        self.len -= slab.iter().filter(|cell| !cell.is_blank()).count();
                    }
                }
                page.cols = columns;
                page.col_nonempty = col_nonempty;
                if !page.col_nonempty.is_empty() {
                    page.rebuild_masks();
                    self.insert_page_arc(pi, Arc::new(page));
                }
                continue;
            }

            // Whole-page fast path when shifting by an exact multiple of PAGE_ROWS:
            // reuse `Arc<Page>` directly without cloning `Page` or touching `Arc<Vec<Cell>>`.
            if base >= at
                && let Some(target0) = map(base)
                && target0 % PAGE_ROWS == 0
                && map(base + PAGE_ROWS - 1) == Some(target0 + PAGE_ROWS - 1)
            {
                let dest = target0 / PAGE_ROWS;
                for c in page_arc.iter_nonempty_cols(0, u32::MAX) {
                    if let Some(cells) = page_arc.cols.get(c as usize).and_then(Option::as_deref) {
                        let mut bits = [0u64; 4];
                        for (r, cell) in cells.iter().enumerate() {
                            if !cell.is_blank() {
                                bits[r / 64] |= 1u64 << (r % 64);
                            }
                        }
                        occupied.push((pi, c, bits));
                    }
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
            for (col, slab_opt) in page.cols.into_iter().enumerate() {
                let Some(cells) = slab_opt else { continue };
                if cells.is_empty() {
                    continue;
                }
                let mut bits = [0u64; 4];
                let begin = at.saturating_sub(base) as usize;
                for (r, cell) in cells.iter().enumerate().skip(begin) {
                    if !cell.is_blank() {
                        bits[r / 64] |= 1u64 << (r % 64);
                    }
                }
                occupied.push((pi, col as u32, bits));
                if let [(source, dest, 0)] = spans.as_slice()
                    && source.start == 0
                    && source.end >= cells.len()
                    && map(base + PAGE_ROWS - 1) == Some(*dest * PAGE_ROWS + PAGE_ROWS - 1)
                {
                    let p = self.page_mut_or_insert(*dest);
                    p.n_rows = p.n_rows.max(cells.len() as u32);
                    for w in 0..4 {
                        p.row_mask[w] |= page_row_mask[w];
                    }
                    Page::set_col_bit(&mut p.col_nonempty, col as u32);
                    if p.cols.len() <= col {
                        p.cols.resize(col + 1, None);
                    }
                    p.cols[col] = Some(cells);
                    p.may_have_formula |= page_formula;
                    p.may_have_qualified_formula |= page_qual;
                    continue;
                }
                let mut cells = Arc::unwrap_or_clone(cells);
                for (source, dest, offset) in &spans {
                    let end = source.end.min(cells.len());
                    if source.start >= end {
                        break;
                    }
                    let src = &mut cells[source.start..end];
                    let dest_p = self.page_mut_or_insert(*dest);
                    dest_p.may_have_formula |= page_formula;
                    dest_p.may_have_qualified_formula |= page_qual;
                    let dst = dest_p.ensure(*offset + src.len() as u32 - 1, col as u32);
                    src.swap_with_slice(&mut dst[*offset as usize..*offset as usize + src.len()]);
                }
                self.len -= cells.iter().filter(|cell| !cell.is_blank()).count();
            }
        }

        dirty_pages.sort_unstable();
        dirty_pages.dedup();
        for pi in dirty_pages {
            if let Some(p) = self.page_mut_existing(pi) {
                p.rebuild_masks();
                if p.col_nonempty.is_empty() {
                    self.remove_page(pi);
                }
            }
        }

        cleared.retain(|k| (if rows { k.row } else { k.col }) < at || !self.contains(k.row, k.col));
        for (pi, col, words) in occupied {
            let dest = self
                .page(pi)
                .and_then(|p| p.cols.get(col as usize))
                .and_then(Option::as_deref);
            for (word, mut bits) in words.into_iter().enumerate() {
                while bits != 0 {
                    let r = word * 64 + bits.trailing_zeros() as usize;
                    if dest.and_then(|cells| cells.get(r)).is_none_or(Cell::is_blank) {
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
                let mut moved: Vec<(u32, Arc<Vec<Cell>>)> = Vec::with_capacity(cols_in_band.len());
                for c in cols_in_band {
                    if let Some(slab) = page.cols.get_mut(c as usize).and_then(Option::take) {
                        page.clear_col_bit(c);
                        let mut bits = [0u64; 4];
                        for (r, cell) in slab.iter().enumerate() {
                            if !cell.is_blank() {
                                bits[r / 64] |= 1u64 << (r % 64);
                            }
                        }
                        old_occupied.push((pi, c, bits));
                        moved.push((map(c), slab));
                    }
                }
                for (nc, slab) in moved {
                    if page.cols.len() <= nc as usize {
                        page.cols.resize(nc as usize + 1, None);
                    }
                    page.cols[nc as usize] = Some(slab);
                    Page::set_col_bit(&mut page.col_nonempty, nc);
                }
                page.rebuild_masks();
            }
            cleared.retain(|k| k.col < lo || k.col >= hi || !self.contains(k.row, k.col));
            for (pi, col, words) in old_occupied {
                let dest = self
                    .page(pi)
                    .and_then(|p| p.cols.get(col as usize))
                    .and_then(Option::as_deref);
                for (w, mut bits) in words.into_iter().enumerate() {
                    while bits != 0 {
                        let r = w * 64 + bits.trailing_zeros() as usize;
                        if dest.and_then(|cells| cells.get(r)).is_none_or(Cell::is_blank) {
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
                let Some(slot) = page.cols.get_mut(c as usize).and_then(Option::as_mut) else {
                    continue;
                };
                let end = r_end.min(slot.len());
                if r_start >= end {
                    continue;
                }
                if !slot[r_start..end].iter().any(|cell| !cell.is_blank()) {
                    continue;
                }
                let vec = Arc::make_mut(slot);
                for r_off in r_start..end {
                    if !vec[r_off].is_blank() {
                        let cell = std::mem::take(&mut vec[r_off]);
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
            let col = p.ensure(dro, c);
            col[dro as usize] = cell;
            dirty_pages.push(dpi);
        }
        dirty_pages.sort_unstable();
        dirty_pages.dedup();
        for pi in dirty_pages {
            if let Some(p) = self.page_mut_existing(pi) {
                p.rebuild_masks();
                if p.col_nonempty.is_empty() {
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

    /// Check if `row` has any occupied cell in `[c1..=c2]` using page `row_mask` + `col_nonempty`.
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
        page.iter_nonempty_cols(c1, c2).any(|c| page.get(ro, c).is_some())
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
            let Some(slab) = page.cols.get(col as usize).and_then(Option::as_deref) else {
                continue;
            };
            let base = pi * PAGE_ROWS;
            let lo = r1.saturating_sub(base) as usize;
            let hi = ((r2 - base) as usize + 1).min(slab.len());
            if lo < hi && slab[lo..hi].iter().any(|cell| !cell.is_blank()) {
                return true;
            }
        }
        false
    }

    /// Single-pass symmetric diff between `self` (`current`) and `other` (`target`):
    /// skips identical `Arc<Page>` and `Arc<Vec<Cell>>` slabs in `O(1)`, marks cells vacated
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
            let max_words = pa
                .map_or(0, |p| p.col_nonempty.len())
                .max(pb.map_or(0, |p| p.col_nonempty.len()));
            for w in 0..max_words {
                let mut bits = pa.and_then(|p| p.col_nonempty.get(w)).copied().unwrap_or(0)
                    | pb.and_then(|p| p.col_nonempty.get(w)).copied().unwrap_or(0);
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    bits &= bits - 1;
                    let col = w as u32 * 64 + tz;
                    let ca = pa.and_then(|p| p.cols.get(col as usize)).and_then(Option::as_ref);
                    let cb = pb.and_then(|p| p.cols.get(col as usize)).and_then(Option::as_ref);
                    if let (Some(xa), Some(xb)) = (ca, cb)
                        && Arc::ptr_eq(xa, xb)
                    {
                        continue;
                    }
                    let sa = ca.map(|v| v.as_slice()).unwrap_or_default();
                    let sb = cb.map(|v| v.as_slice()).unwrap_or_default();
                    let rows = sa.len().max(sb.len());
                    for r in 0..rows {
                        let a_cell = sa.get(r).filter(|c| !c.is_blank());
                        let b_cell = sb.get(r).filter(|c| !c.is_blank());
                        let differs = match (a_cell, b_cell) {
                            (None, None) => false,
                            (Some(_), None) => {
                                cleared_in_other.insert(CellRef::new(base + r as u32, col));
                                true
                            }
                            (None, Some(_)) => true,
                            (Some(ac), Some(bc)) => {
                                ac.formula != bc.formula || !ac.value.same_stored_value(&bc.value)
                            }
                        };
                        if differs {
                            out.push((sheet_idx, base + r as u32, col));
                        }
                    }
                }
            }
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
    /// skips identical `Arc<Page>` and `Arc<Vec<Cell>>` slabs in `O(1)`, marks differing
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
        let blank = Cell::default();
        let mut account_slab = |slab: &Arc<Vec<Cell>>| {
            if seen_slabs.insert(Arc::as_ptr(slab) as usize) {
                forked_bytes += std::mem::size_of::<Vec<Cell>>()
                    + slab.capacity() * std::mem::size_of::<Cell>();
            }
        };
        let mut check_pair = |pi: u32, pa: Option<&Arc<Page>>, pb: Option<&Arc<Page>>| {
            match (pa, pb) {
                (None, None) => return,
                (Some(a), Some(b)) if Arc::ptr_eq(a, b) => return,
                _ => {}
            }
            let max_words = pa
                .map_or(0, |p| p.col_nonempty.len())
                .max(pb.map_or(0, |p| p.col_nonempty.len()));
            for w in 0..max_words {
                let mut bits = pa.and_then(|p| p.col_nonempty.get(w)).copied().unwrap_or(0)
                    | pb.and_then(|p| p.col_nonempty.get(w)).copied().unwrap_or(0);
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    bits &= bits - 1;
                    let col = w as u32 * 64 + tz;
                    let ca = pa.and_then(|p| p.cols.get(col as usize)).and_then(Option::as_ref);
                    let cb = pb.and_then(|p| p.cols.get(col as usize)).and_then(Option::as_ref);
                    if let (Some(xa), Some(xb)) = (ca, cb)
                        && Arc::ptr_eq(xa, xb)
                    {
                        continue;
                    }
                    if let Some(xa) = ca {
                        account_slab(xa);
                    }
                    if let Some(xb) = cb {
                        account_slab(xb);
                    }
                    let sa = ca.map(|v| v.as_slice()).unwrap_or_default();
                    let sb = cb.map(|v| v.as_slice()).unwrap_or_default();
                    let rows = sa.len().max(sb.len());
                    let mut mask = [0u64; 4];
                    for r in 0..rows {
                        let ac = sa.get(r).filter(|c| !c.is_blank()).unwrap_or(&blank);
                        let bc = sb.get(r).filter(|c| !c.is_blank()).unwrap_or(&blank);
                        if !ac.same_input(bc) {
                            mask[r / 64] |= 1u64 << (r % 64);
                            diff_heap += crate::undo::cell_heap_bytes(ac)
                                + crate::undo::cell_heap_bytes(bc);
                        }
                    }
                    if mask != [0; 4] {
                        cell_mask.or_page_mask(pi, col, mask);
                    }
                }
            }
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
            for c in page.iter_nonempty_cols(0, u32::MAX) {
                if let Some(slab) = page.cols.get(c as usize).and_then(Option::as_deref) {
                    for cell in slab {
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
                    if let Some(slab) = page.cols.get_mut(col as usize).and_then(Option::take) {
                        let mut mask = [0u64; 4];
                        for (r, cell) in slab.iter().enumerate() {
                            if !cell.is_blank() {
                                mask[r / 64] |= 1u64 << (r % 64);
                                removed += 1;
                            }
                        }
                        if mask != [0; 4] {
                            cleared.or_page_mask(pi, col, mask);
                        }
                        page.clear_col_bit(col);
                        page_changed = true;
                    }
                } else if let Some(slot) = page.cols.get_mut(col as usize).and_then(Option::as_mut) {
                    let end = (hi + 1).min(slot.len());
                    if lo >= end {
                        continue;
                    }
                    if !slot[lo..end].iter().any(|c| !c.is_blank()) {
                        continue;
                    }
                    let vec = Arc::make_mut(slot);
                    let mut mask = [0u64; 4];
                    for r in lo..end {
                        if !vec[r].is_blank() {
                            vec[r] = Cell::default();
                            mask[r / 64] |= 1u64 << (r % 64);
                            removed += 1;
                        }
                    }
                    if mask != [0; 4] {
                        cleared.or_page_mask(pi, col, mask);
                    }
                    while vec.last().is_some_and(Cell::is_blank) {
                        vec.pop();
                    }
                    if vec.is_empty() {
                        page.cols[col as usize] = None;
                        page.clear_col_bit(col);
                    }
                    page_changed = true;
                }
            }
            if page_changed {
                page.rebuild_masks();
                if page.col_nonempty.is_empty() {
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
    /// reusing slab `swap_with_slice` and whole-slab moves without touching other columns.
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

        // Extract only column slabs in `[c1..=c2]` for rows `>= at`.
        let mut extracted: Vec<(u32, u32, Arc<Vec<Cell>>, bool, bool)> = Vec::new();
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
                    if let Some(slab) = page.cols.get_mut(col as usize).and_then(Option::take) {
                        page.clear_col_bit(col);
                        let mut bits = [0u64; 4];
                        for (r, cell) in slab.iter().enumerate() {
                            if !cell.is_blank() {
                                bits[r / 64] |= 1u64 << (r % 64);
                            }
                        }
                        occupied.push((pi, col, bits));
                        extracted.push((pi, col, slab, page_formula, page_qual));
                        touched_page = true;
                    }
                } else if let Some(slot) = page.cols.get_mut(col as usize).and_then(Option::as_mut) {
                    if slot.len() <= lo {
                        continue;
                    }
                    let vec = Arc::make_mut(slot);
                    let mut tail = vec![Cell::default(); vec.len()];
                    let mut bits = [0u64; 4];
                    let mut any = false;
                    for r in lo..vec.len() {
                        if !vec[r].is_blank() {
                            bits[r / 64] |= 1u64 << (r % 64);
                            tail[r] = std::mem::take(&mut vec[r]);
                            any = true;
                        }
                    }
                    while vec.last().is_some_and(Cell::is_blank) {
                        vec.pop();
                    }
                    if vec.is_empty() {
                        page.cols[col as usize] = None;
                        page.clear_col_bit(col);
                    }
                    if any {
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
                if dest_p.cols.len() <= col as usize {
                    dest_p.cols.resize(col as usize + 1, None);
                }
                dest_p.cols[col as usize] = Some(slab);
                Page::set_col_bit(&mut dest_p.col_nonempty, col);
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
            let mut cells = Arc::unwrap_or_clone(slab);
            for (source, dest, offset) in &spans {
                let end = source.end.min(cells.len());
                if source.start >= end {
                    break;
                }
                let src = &mut cells[source.start..end];
                let dest_p = self.page_mut_or_insert(*dest);
                dest_p.may_have_formula |= page_formula;
                dest_p.may_have_qualified_formula |= page_qual;
                let dst = dest_p.ensure(*offset + src.len() as u32 - 1, col);
                src.swap_with_slice(&mut dst[*offset as usize..*offset as usize + src.len()]);
                dirty_pages.push(*dest);
            }
            self.len -= cells.iter().filter(|cell| !cell.is_blank()).count();
        }

        dirty_pages.sort_unstable();
        dirty_pages.dedup();
        for pi in dirty_pages {
            if let Some(p) = self.page_mut_existing(pi) {
                p.rebuild_masks();
                if p.col_nonempty.is_empty() {
                    self.remove_page(pi);
                }
            }
        }

        cleared.retain(|k| {
            k.col < c1 || k.col > c2 || k.row < at || !self.contains(k.row, k.col)
        });
        for (pi, col, words) in occupied {
            let dest = self
                .page(pi)
                .and_then(|p| p.cols.get(col as usize))
                .and_then(Option::as_deref);
            let mut cleared_mask = [0u64; 4];
            for (word, mut bits) in words.into_iter().enumerate() {
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    let r = word * 64 + tz as usize;
                    if dest.and_then(|cells| cells.get(r)).is_none_or(Cell::is_blank) {
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
    /// (and `may_have_qualified_formula` when `foreign_sheet` is `Some`), clones each
    /// modified column slab at most once, and uses `std::thread::scope` for large sheets.
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
            let mut updated_cols: Vec<(u32, Arc<Vec<Cell>>)> = Vec::new();
            let mut any_qual = page.may_have_qualified_formula;
            for col in page.iter_nonempty_cols(0, u32::MAX) {
                let Some(slab) = page.cols.get(col as usize).and_then(Option::as_ref) else {
                    continue;
                };
                let mut cloned: Option<Vec<Cell>> = None;
                for (r_off, cell) in slab.iter().enumerate() {
                    let Some(old) = cell.formula.as_deref() else {
                        continue;
                    };
                    if foreign_sheet.is_some() && (!old.contains('!') || !matches_foreign(old)) {
                        continue;
                    }
                    if let Some(new_f) = rewrite_one(old) {
                        any_qual |= new_f.contains('!');
                        let vec = cloned.get_or_insert_with(|| slab.as_ref().clone());
                        vec[r_off].formula = Some(new_f);
                    }
                }
                if let Some(vec) = cloned {
                    updated_cols.push((col, Arc::new(vec)));
                }
            }
            if updated_cols.is_empty() {
                return None;
            }
            let mut new_page = page.as_ref().clone();
            new_page.may_have_qualified_formula = any_qual;
            for (col, slab) in updated_cols {
                new_page.cols[col as usize] = Some(slab);
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

    /// Extract flat slice pointers for `col` across pages `p_lo..=p_hi` for O(1) parallel gather.
    pub(crate) fn col_page_slices(&self, col: u32, p_lo: u32, p_hi: u32) -> Vec<Option<&[Cell]>> {
        let count = (p_hi - p_lo + 1) as usize;
        let mut out = Vec::with_capacity(count);
        for pi in p_lo..=p_hi {
            let slice = self
                .page(pi)
                .and_then(|p| p.cols.get(col as usize))
                .and_then(Option::as_deref)
                .map(|v| v.as_slice());
            out.push(slice);
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
        let mut installed_col_bits: Vec<u64> = Vec::new();
        for col_res in &columns {
            net_delta += col_res.len_delta;
            Page::set_col_bit(&mut installed_col_bits, col_res.col);
        }
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
            let mut max_active_col: Option<usize> = None;
            for col_res in &columns {
                if col_res.slabs[idx].is_some() {
                    let c = col_res.col as usize;
                    max_active_col = Some(max_active_col.map_or(c, |m| m.max(c)));
                }
            }

            let has_untouched = self.page(pi).is_some_and(|p| {
                p.col_nonempty.iter().enumerate().any(|(w, &bits)| {
                    let covered = installed_col_bits.get(w).copied().unwrap_or(0);
                    (bits & !covered) != 0
                })
            });

            if !has_untouched {
                let Some(last_col) = max_active_col else {
                    if self.page(pi).is_some() {
                        self.remove_page(pi);
                    }
                    continue;
                };
                let mut cols_vec = vec![None; last_col + 1];
                let mut col_nonempty = vec![0u64; last_col / 64 + 1];
                let mut row_mask = [0u64; 4];
                let mut n_rows = 0u32;
                let mut may_have_formula = false;
                let mut may_have_qualified_formula = false;
                for col_res in &mut columns {
                    if let Some(slab) = col_res.slabs[idx].take() {
                        let c = col_res.col as usize;
                        n_rows = n_rows.max(slab.cells.len() as u32);
                        row_mask[0] |= slab.row_mask[0];
                        row_mask[1] |= slab.row_mask[1];
                        row_mask[2] |= slab.row_mask[2];
                        row_mask[3] |= slab.row_mask[3];
                        col_nonempty[c / 64] |= 1u64 << (c % 64);
                        may_have_formula |= col_res.has_formula;
                        may_have_qualified_formula |= col_res.has_qual_formula;
                        cols_vec[c] = Some(slab.cells);
                    }
                }
                while col_nonempty.last() == Some(&0) {
                    col_nonempty.pop();
                }
                self.insert_page_arc(
                    pi,
                    Arc::new(Page {
                        may_have_qualified_formula,
                        may_have_formula,
                        n_rows,
                        row_mask,
                        col_nonempty,
                        cols: cols_vec,
                    }),
                );
            } else {
                let page = self.page_mut_or_insert(pi);
                if let Some(last_col) = max_active_col
                    && page.cols.len() <= last_col
                {
                    page.cols.resize(last_col + 1, None);
                }
                for col_res in &mut columns {
                    let col = col_res.col;
                    match col_res.slabs[idx].take() {
                        Some(slab) => {
                            page.cols[col as usize] = Some(slab.cells);
                            Page::set_col_bit(&mut page.col_nonempty, col);
                            page.may_have_formula |= col_res.has_formula;
                            page.may_have_qualified_formula |= col_res.has_qual_formula;
                        }
                        None => {
                            if page
                                .cols
                                .get_mut(col as usize)
                                .and_then(Option::take)
                                .is_some()
                            {
                                page.clear_col_bit(col);
                            }
                        }
                    }
                }
                page.rebuild_masks();
                if page.col_nonempty.is_empty() {
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
    /// (`dst = r1 + i` receives source row `order[i]`), translating formulas in parallel.
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
        let any_formula = (p_lo..=p_hi).any(|pi| {
            source.page(pi).is_some_and(|p| p.may_have_formula)
                || self.page(pi).is_some_and(|p| p.may_have_formula)
        });

        let all_src_slabs: Vec<Vec<Option<&[Cell]>>> = cols
            .iter()
            .map(|&col| source.col_page_slices(col, p_lo, p_hi))
            .collect();
        let all_dst_slabs: Vec<Vec<Option<&[Cell]>>> = cols
            .iter()
            .map(|&col| self.col_page_slices(col, p_lo, p_hi))
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
            let mut fill_cache = any_formula.then(crate::refs::FillFormulas::default);
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
                let old_slab = dst_old_slabs[page_idx].unwrap_or(&[]);
                let push_mode = lo == 0 && hi + 1 >= old_slab.len();

                if !any_formula && push_mode {
                    let mut out_vec = Vec::with_capacity(hi + 1);
                    let mut old_count: isize = 0;
                    let mut new_count: isize = 0;
                    let mut row_mask = [0u64; 4];
                    let mut vac_mask = [0u64; 4];
                    let page_order = &order[(base - r1) as usize..(base - r1) as usize + (hi + 1)];
                    for (r_off, &src_r) in page_order.iter().enumerate() {
                        let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                        let so = (src_r % PAGE_ROWS) as usize;
                        let src_cell = src_slabs[sp]
                            .and_then(|s| s.get(so))
                            .filter(|c| !c.is_blank());
                        let old_cell = old_slab.get(r_off).filter(|c| !c.is_blank());
                        if old_cell.is_some() {
                            old_count += 1;
                        }
                        if let Some(sc) = src_cell {
                            if old_cell.is_none_or(|o| o.value != sc.value) {
                                changed_cells += 1;
                            }
                            new_count += 1;
                            row_mask[r_off / 64] |= 1u64 << (r_off % 64);
                            out_vec.push(sc.clone());
                        } else {
                            if old_cell.is_some() {
                                changed_cells += 1;
                                vac_mask[r_off / 64] |= 1u64 << (r_off % 64);
                            }
                            out_vec.push(Cell::default());
                        }
                    }
                    while out_vec.last().is_some_and(Cell::is_blank) {
                        out_vec.pop();
                    }
                    len_delta += new_count - old_count;
                    if vac_mask != [0; 4] {
                        vacated_masks.push((pi, vac_mask));
                    }
                    if track_cleared && row_mask != [0; 4] {
                        occupied_masks.push((pi, row_mask));
                    }
                    slabs.push((!out_vec.is_empty()).then(|| GatheredSlab {
                        cells: Arc::new(out_vec),
                        row_mask,
                    }));
                    continue;
                }

                let mut old_count: isize = 0;
                for cell in old_slab {
                    if !cell.is_blank() {
                        old_count += 1;
                    }
                }
                let target_len = old_slab.len().max(hi + 1);
                let mut out_vec: Vec<Cell>;
                let mut new_count: isize = 0;
                let mut row_mask = [0u64; 4];
                if push_mode {
                    out_vec = Vec::with_capacity(target_len);
                } else {
                    out_vec = vec![Cell::default(); target_len];
                    for r in 0..lo.min(old_slab.len()) {
                        if !old_slab[r].is_blank() {
                            if let Some(f) = old_slab[r].formula.as_deref() {
                                has_formula = true;
                                if f.contains('!') {
                                    has_qual_formula = true;
                                }
                            }
                            new_count += 1;
                            row_mask[r / 64] |= 1u64 << (r % 64);
                            out_vec[r] = old_slab[r].clone();
                        }
                    }
                    for r in (hi + 1)..old_slab.len() {
                        if !old_slab[r].is_blank() {
                            if let Some(f) = old_slab[r].formula.as_deref() {
                                has_formula = true;
                                if f.contains('!') {
                                    has_qual_formula = true;
                                }
                            }
                            new_count += 1;
                            row_mask[r / 64] |= 1u64 << (r % 64);
                            out_vec[r] = old_slab[r].clone();
                        }
                    }
                }

                let mut vac_mask = [0u64; 4];
                let mut occ_mask = [0u64; 4];
                let page_order =
                    &order[(base + lo as u32 - r1) as usize..(base + hi as u32 - r1) as usize + 1];
                for (i, &src_r) in page_order.iter().enumerate() {
                    let r_off = lo + i;
                    let dst_r = base + r_off as u32;
                    let sp = ((src_r / PAGE_ROWS) - p_lo) as usize;
                    let so = (src_r % PAGE_ROWS) as usize;
                    let src_cell = src_slabs[sp]
                        .and_then(|s| s.get(so))
                        .filter(|c| !c.is_blank());
                    let old_cell = old_slab.get(r_off).filter(|c| !c.is_blank());
                    if let Some(sc) = src_cell {
                        let mut cell = sc.clone();
                        if dst_r != src_r
                            && let Some(f) = cell.formula.as_deref()
                        {
                            let dr = i64::from(dst_r) - i64::from(src_r);
                            let nf: Arc<str> = if !f.contains('!') {
                                fill_cache
                                    .as_mut()
                                    .expect("fill_cache initialized when any_formula")
                                    .translate(f, dr, 0)
                                    .into()
                            } else {
                                crate::refs::translate(f, dr, 0, sheet_name).into()
                            };
                            cell.formula = Some(nf);
                        }
                        if old_cell != Some(&cell) {
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
                        if old_cell.is_some() {
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
                    cells: Arc::new(out_vec),
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
                bytes += std::mem::size_of::<Page>()
                    + page.cols.capacity() * std::mem::size_of::<Option<Arc<Vec<Cell>>>>()
                    + page.col_nonempty.capacity() * std::mem::size_of::<u64>();
                for slab in page.cols.iter().flatten() {
                    if seen_slabs.insert(Arc::as_ptr(slab) as usize) {
                        bytes += std::mem::size_of::<Vec<Cell>>()
                            + slab.capacity() * std::mem::size_of::<Cell>();
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
                for c in page.iter_nonempty_cols(0, u32::MAX) {
                    let Some(slab) = page.cols.get(c as usize).and_then(Option::as_deref) else {
                        continue;
                    };
                    if let Some(first_i) = slab.iter().position(|cell| !cell.is_blank()) {
                        let last_i = slab.iter().rposition(|cell| !cell.is_blank()).unwrap();
                        min_r = min_r.min(base + first_i as u32);
                        max_r = max_r.max(base + last_i as u32);
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
            n += std::mem::size_of::<Page>()
                + page.cols.capacity() * std::mem::size_of::<Option<Arc<Vec<Cell>>>>()
                + page.col_nonempty.capacity() * std::mem::size_of::<u64>();
            for col in page.cols.iter().flatten() {
                n += std::mem::size_of::<Vec<Cell>>() + col.capacity() * std::mem::size_of::<Cell>();
            }
        }
        n
    }

    /// 稀疏保护：某页实际使用的格子占比过低时报告出来（调参用）
    pub fn dense_stats(&self) -> (usize, usize) {
        let used = self.len;
        let alloc: usize = self
            .iter_pages()
            .map(|(_, p)| p.cols.iter().flatten().map(|c| c.len()).sum::<usize>())
            .sum();
        (used, alloc)
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
            s.page(0).unwrap().cols[0].as_ref().unwrap(),
            clone.page(0).unwrap().cols[0].as_ref().unwrap(),
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
}
