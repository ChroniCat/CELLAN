//! 分页列式存储：把单元格按「页」切开，页内按列连续存放。
//!
//! 设计要点：
//! - 页高 `PAGE_ROWS = 256` 行；页目录对常规坐标（`< 8192` 页 = 209 万行）采用定长索引直达 `O(1)`，
//!   辅以 `page_bits: Vec<u64>` 页非空位图，使 `iter` / `iter_rect` 无需拷贝页号做全键排序。
//! - 页内列容器为 `Vec<Option<Arc<Vec<Cell>>>>` + `col_nonempty: Vec<u64>` + `row_mask: [u64; 4]`：
//!   远端空列（如 `XFD`）仅占 8 字节 `None` 槽位、零堆分配，且位图扫描可直接跳过空行/空列。
//! - `CellStyles` 支持整页同样式 `StyleColumn::Uniform(u16)` 压缩，整列/大区域刷格式共享单个 `Arc`。

use crate::model::{Cell, CellJournal, CellRef, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

pub const PAGE_ROWS: u32 = 256;
const DIRECT_PAGE_LIMIT: u32 = 8192;

/// Explicit styles, including style zero overriding a row or column default.
/// Each column page either stores a single uniform style ID for all 256 rows
/// or packs its sparse `u16` IDs in row order using a 256-bit occupancy bitmap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellStyles {
    pages: Arc<HashMap<(u32, u32), Arc<StyleColumn>>>,
    len: usize,
}

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
    pub(crate) fn get(&self, offset: u32) -> Option<&u16> {
        match self {
            Self::Uniform(id) => Some(id),
            Self::Sparse { rows, ids } => {
                let (word, bit, rank) = Self::sparse_position(rows, offset);
                (rows[word] & bit != 0).then(|| &ids[rank])
            }
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
        let base = p * PAGE_ROWS;
        match self {
            Self::Uniform(id) => StyleColumnIter::Uniform {
                base,
                col,
                off: 0,
                id,
            },
            Self::Sparse { rows, ids } => StyleColumnIter::Sparse {
                base,
                col,
                rows: *rows,
                word: 0,
                ids: ids.iter(),
            },
        }
    }
}

enum StyleColumnIter<'a> {
    Uniform {
        base: u32,
        col: u32,
        off: u32,
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
            Self::Uniform { base, col, off, id } => {
                if *off >= PAGE_ROWS {
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
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn uniform_page_count(&self) -> usize {
        self.pages.values().filter(|p| p.is_uniform()).count()
    }

    #[inline]
    pub fn get(&self, key: &CellRef) -> Option<&u16> {
        let page = self.pages.get(&(key.row / PAGE_ROWS, key.col))?;
        page.get(key.row % PAGE_ROWS)
    }

    pub fn insert(&mut self, key: CellRef, id: u16) -> Option<u16> {
        let page_key = (key.row / PAGE_ROWS, key.col);
        let offset = key.row % PAGE_ROWS;
        if let Some(existing) = self.pages.get(&page_key) {
            if existing.get(offset) == Some(&id) {
                return Some(id);
            }
        }
        let page = Arc::make_mut(&mut self.pages).entry(page_key).or_default();
        let old = Arc::make_mut(page).insert(offset, id);
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
        let pages = Arc::make_mut(&mut self.pages);
        for p in first_p..=last_p {
            let base = p * PAGE_ROWS;
            let lo = r1.saturating_sub(base);
            let hi = (r2 - base).min(PAGE_ROWS - 1);
            if lo == 0 && hi == PAGE_ROWS - 1 {
                for col in c1..=c2 {
                    let prev_len = pages.get(&(p, col)).map_or(0, |pg| pg.len());
                    pages.insert((p, col), uniform.clone());
                    self.len = self.len - prev_len + PAGE_ROWS as usize;
                }
            } else {
                for col in c1..=c2 {
                    let entry = pages.entry((p, col)).or_default();
                    if let StyleColumn::Uniform(cur) = entry.as_ref()
                        && *cur == id
                    {
                        continue;
                    }
                    let col_mut = Arc::make_mut(entry);
                    for off in lo..=hi {
                        if col_mut.insert(off, id).is_none() {
                            self.len += 1;
                        }
                    }
                    if let StyleColumn::Uniform(cur) = col_mut
                        && *cur == id
                    {
                        *entry = uniform.clone();
                    }
                }
            }
        }
    }

    pub fn remove(&mut self, key: &CellRef) -> Option<u16> {
        self.get(key)?;
        let page_key = (key.row / PAGE_ROWS, key.col);
        let pages = Arc::make_mut(&mut self.pages);
        let page = Arc::make_mut(pages.get_mut(&page_key)?);
        let (old, empty) = page.remove(key.row % PAGE_ROWS);
        if old.is_some() {
            if empty {
                pages.remove(&page_key);
            }
            self.len -= 1;
        }
        old
    }

    /// Maximum `(row, col)` among all explicit cell styles in `O(style_pages)` time.
    pub fn max_coords(&self) -> Option<(u32, u32)> {
        if self.len == 0 {
            return None;
        }
        let mut max_r = 0u32;
        let mut max_c = 0u32;
        let mut any = false;
        for (&(p, col), page) in self.pages.iter() {
            if let Some(off) = page.max_offset() {
                let r = p * PAGE_ROWS + off;
                if !any {
                    max_r = r;
                    max_c = col;
                    any = true;
                } else {
                    max_r = max_r.max(r);
                    max_c = max_c.max(col);
                }
            }
        }
        any.then_some((max_r, max_c))
    }

    /// Iterate `(row, col, style_id)` inside `rect = (r1, c1, r2, c2)` scanning only intersecting pages.
    pub fn iter_rect(&self, (r1, c1, r2, c2): (u32, u32, u32, u32)) -> impl Iterator<Item = (CellRef, u16)> + '_ {
        let p1 = r1 / PAGE_ROWS;
        let p2 = r2 / PAGE_ROWS;
        self.pages
            .iter()
            .filter(move |&(&(p, col), _)| r1 <= r2 && c1 <= c2 && p >= p1 && p <= p2 && col >= c1 && col <= c2)
            .flat_map(move |(&(p, col), page)| {
                page.iter_entries(p, col)
                    .filter(move |(k, _)| k.row >= r1 && k.row <= r2)
                    .map(|(k, &id)| (k, id))
            })
    }

    pub fn iter(&self) -> impl Iterator<Item = (CellRef, &u16)> {
        self.iter_from(true, 0)
    }

    pub(crate) fn iter_from(&self, rows: bool, at: u32) -> impl Iterator<Item = (CellRef, &u16)> {
        self.pages
            .iter()
            .filter(move |&(&(p, col), _)| {
                if rows {
                    p >= at / PAGE_ROWS
                } else {
                    col >= at
                }
            })
            .flat_map(move |(&(p, col), page)| {
                page.iter_entries(p, col)
                    .filter(move |(key, _)| !rows || key.row >= at)
            })
    }

    /// Re-key complete style pages when possible. Repack split pages in ascending
    /// row order so a run of equal styles never shifts the packed vector per cell,
    /// and preserve `StyleColumn::Uniform(sid)` pages via contiguous `insert_rect`.
    pub(crate) fn remap_axis(&mut self, rows: bool, at: u32, map: impl Fn(u32) -> Option<u32>) {
        if self
            .pages
            .keys()
            .all(|&(p, c)| if rows { p < at / PAGE_ROWS } else { c < at })
        {
            return;
        }
        let old = std::mem::take(self);
        let mut pages: Vec<_> = old.pages.iter().collect();
        pages.sort_unstable_by_key(|&(&key, _)| key);
        let mut out = CellStyles::default();
        for (&(p, c), page) in pages {
            let base = p * PAGE_ROWS;
            let same_page = if !rows {
                if c < at {
                    Some((p, c))
                } else {
                    map(c).map(|col| (p, col))
                }
            } else if base + PAGE_ROWS <= at {
                Some((p, c))
            } else {
                map(base)
                    .filter(|r| r % PAGE_ROWS == 0 && map(base + PAGE_ROWS - 1) == Some(r + PAGE_ROWS - 1))
                    .map(|r| (r / PAGE_ROWS, c))
            };
            if let Some(key) = same_page {
                Arc::make_mut(&mut out.pages).insert(key, page.clone());
                out.len += page.len();
            } else if rows {
                if let StyleColumn::Uniform(sid) = page.as_ref() {
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
                    for (cref, &id) in page.iter_entries(p, c) {
                        if let Some(target) = map(cref.row) {
                            out.insert(CellRef::new(target, c), id);
                        }
                    }
                }
            }
        }
        *self = out;
    }

    /// Permute coordinates strictly within `[lo..hi)` while leaving all pages outside `[lo..hi)` untouched.
    pub(crate) fn permute_band(&mut self, rows: bool, lo: u32, hi: u32, map: impl Fn(u32) -> u32) {
        if lo >= hi || self.is_empty() {
            return;
        }
        let p_lo = lo / PAGE_ROWS;
        let p_hi = (hi - 1) / PAGE_ROWS;
        let affected: Vec<((u32, u32), Arc<StyleColumn>)> = self
            .pages
            .iter()
            .filter(|&(&(p, c), _)| {
                if rows {
                    p >= p_lo && p <= p_hi
                } else {
                    c >= lo && c < hi
                }
            })
            .map(|(&k, v)| (k, v.clone()))
            .collect();
        if affected.is_empty() {
            return;
        }
        if !rows {
            let pages = Arc::make_mut(&mut self.pages);
            for ((p, c), _) in &affected {
                if let Some(removed) = pages.remove(&(*p, *c)) {
                    self.len -= removed.len();
                }
            }
            for ((p, c), sc) in affected {
                let nc = map(c);
                self.len += sc.len();
                pages.insert((p, nc), sc);
            }
            return;
        }
        let mut staged: Vec<(CellRef, u16)> = Vec::new();
        for ((p, c), sc) in affected {
            let base = p * PAGE_ROWS;
            for (cref, &id) in sc.iter_entries(p, c) {
                if cref.row >= lo && cref.row < hi {
                    staged.push((CellRef::new(map(cref.row), c), id));
                }
            }
            // Clear only `[lo..hi)` inside `(p, c)`.
            let r1 = lo.max(base);
            let r2 = (hi - 1).min(base + PAGE_ROWS - 1);
            if r1 == base && r2 == base + PAGE_ROWS - 1 {
                if let Some(removed) = Arc::make_mut(&mut self.pages).remove(&(p, c)) {
                    self.len -= removed.len();
                }
            } else {
                for r in r1..=r2 {
                    self.remove(&CellRef::new(r, c));
                }
            }
        }
        for (cref, id) in staged {
            self.insert(cref, id);
        }
    }

    pub fn payload_bytes(&self) -> usize {
        let mut seen = std::collections::HashSet::with_capacity(self.pages.len());
        let mut bytes = self.pages.capacity()
            * (std::mem::size_of::<(u32, u32)>() + std::mem::size_of::<Arc<StyleColumn>>() + 1);
        for sc in self.pages.values() {
            let ptr = Arc::as_ptr(sc) as usize;
            if seen.insert(ptr) {
                bytes += std::mem::size_of::<StyleColumn>();
                if let StyleColumn::Sparse { ids, .. } = sc.as_ref() {
                    bytes += ids.capacity() * std::mem::size_of::<u16>();
                }
            }
        }
        bytes
    }

    pub fn keys(&self) -> impl Iterator<Item = CellRef> {
        self.iter().map(|(key, _)| key)
    }

    pub(crate) fn changed_keys(&self, other: &Self) -> impl Iterator<Item = CellRef> {
        self.pages
            .iter()
            .filter(move |(key, page)| {
                other
                    .pages
                    .get(key)
                    .is_none_or(|old| !Arc::ptr_eq(old, page) && old != *page)
            })
            .flat_map(move |(&(p, col), page)| {
                page.iter_entries(p, col)
                    .filter_map(move |(key, id)| (other.get(&key) != Some(id)).then_some(key))
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
    /// fetching each `(page, col)` style slab once per page instead of hashing per cell.
    pub(crate) fn iter_with_explicit_style<'a>(
        &'a self,
        styles: &'a CellStyles,
    ) -> impl Iterator<Item = (CellRef, &'a Cell, Option<u16>)> + 'a {
        self.iter_pages().flat_map(move |(pi, page)| {
            let base = pi * PAGE_ROWS;
            let cols: Vec<(u32, &'a [Cell], Option<&'a StyleColumn>)> = page
                .iter_nonempty_cols(0, u32::MAX)
                .filter_map(|col| {
                    let slab = page.cols.get(col as usize)?.as_ref()?.as_slice();
                    let st = styles.pages.get(&(pi, col)).map(Arc::as_ref);
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
        let mut style_by_page: BTreeMap<u32, Vec<(u32, &'a StyleColumn)>> = BTreeMap::new();
        for (&(p, col), sc) in styles.pages.iter() {
            style_by_page.entry(p).or_default().push((col, sc.as_ref()));
        }
        for list in style_by_page.values_mut() {
            list.sort_unstable_by_key(|&(c, _)| c);
        }
        let mut all_pages: Vec<u32> = self
            .iter_pages()
            .map(|(p, _)| p)
            .chain(style_by_page.keys().copied())
            .collect();
        all_pages.sort_unstable();
        all_pages.dedup();
        all_pages.into_iter().flat_map(move |pi| {
            let base = pi * PAGE_ROWS;
            let page = self.page(pi);
            let st_cols = style_by_page.get(&pi).cloned().unwrap_or_default();
            let mut merged_cols: Vec<u32> = page
                .into_iter()
                .flat_map(|p| p.iter_nonempty_cols(0, u32::MAX))
                .chain(st_cols.iter().map(|&(c, _)| c))
                .collect();
            merged_cols.sort_unstable();
            merged_cols.dedup();
            let col_views: Vec<(u32, Option<&'a [Cell]>, Option<&'a StyleColumn>)> = merged_cols
                .into_iter()
                .map(|c| {
                    let cells = page
                        .and_then(|p| p.cols.get(c as usize))
                        .and_then(Option::as_ref)
                        .map(|v| v.as_slice());
                    let sc = st_cols
                        .binary_search_by_key(&c, |&(col, _)| col)
                        .ok()
                        .map(|idx| st_cols[idx].1);
                    (c, cells, sc)
                })
                .collect();
            let mut row_words = page.map_or([0; 4], |p| p.row_mask);
            for &(_, sc) in &st_cols {
                match sc {
                    StyleColumn::Uniform(_) => row_words = [u64::MAX; 4],
                    StyleColumn::Sparse { rows, .. } => {
                        for w in 0..4 {
                            row_words[w] |= rows[w];
                        }
                    }
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
        assert!(Arc::ptr_eq(&styles.pages[&(2, 80)], &saved.pages[&(2, 80)]));
    }

    #[test]
    fn uniform_style_pages_compress_and_share_single_arc() {
        let mut styles = CellStyles::default();
        styles.insert_rect(0, 0, 1023, 3, 7);
        assert_eq!(styles.len(), 1024 * 4);
        assert_eq!(styles.page_count(), 16);
        assert_eq!(styles.uniform_page_count(), 16);
        assert!(Arc::ptr_eq(&styles.pages[&(0, 0)], &styles.pages[&(3, 3)]));
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
}
