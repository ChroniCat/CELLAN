//! 分页列式存储：把单元格按「页」切开，页内按列连续存放。
//!
//! 为什么不是一个大 HashMap：
//! - 每条 HashMap<CellRef, Cell> 要 48 字节槽位 + 控制字节 + 容量余量（实测约 56 字节/格）
//! - 每次分配一块内存，分配器头部又是十几个字节
//!
//! 分页列式后：
//! - 页内一列就是一段连续的 `Vec<Cell>`，SUM/COUNT 这类按列扫描变成顺序遍历
//! - 每页一次分配（每列一个 Vec），分摊到每格只有 40 字节（Cell 本体）
//! - 空单元格不占额外内存：页只按实际用到的行数分配
//!
//! 页高 256 行：太小则页数多、索引开销大；太大则稀疏表浪费。1024 行时
//! 单页 8 列就要 320 KB，改一行会触碰较大范围，256 行是一个折中。

use crate::model::{Cell, CellRef, Value};

pub const PAGE_ROWS: u32 = 256;

/// Explicit styles, including style zero overriding a row or column default.
/// Each column page packs its u16 IDs in row order. The bitmap distinguishes
/// absent entries without reserving a style ID or allocating up to a distant row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellStyles {
    pages: std::sync::Arc<std::collections::HashMap<(u32, u32), std::sync::Arc<StyleColumn>>>,
    len: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StyleColumn {
    rows: [u64; 4],
    ids: Vec<u16>,
}

impl StyleColumn {
    fn position(&self, offset: u32) -> (usize, u64, usize) {
        let word = (offset / 64) as usize;
        let bit = 1u64 << (offset % 64);
        let rank = self.rows[..word].iter().map(|w| w.count_ones()).sum::<u32>()
            + (self.rows[word] & (bit - 1)).count_ones();
        (word, bit, rank as usize)
    }
}

impl CellStyles {
    pub fn len(&self) -> usize { self.len }
    pub fn is_empty(&self) -> bool { self.len == 0 }

    pub fn get(&self, key: &CellRef) -> Option<&u16> {
        let page = self.pages.get(&(key.row / PAGE_ROWS, key.col))?;
        let (word, bit, rank) = page.position(key.row % PAGE_ROWS);
        (page.rows[word] & bit != 0).then(|| &page.ids[rank])
    }

    pub fn insert(&mut self, key: CellRef, id: u16) -> Option<u16> {
        let page = std::sync::Arc::make_mut(&mut self.pages)
            .entry((key.row / PAGE_ROWS, key.col)).or_default();
        let page = std::sync::Arc::make_mut(page);
        let (word, bit, rank) = page.position(key.row % PAGE_ROWS);
        if page.rows[word] & bit != 0 {
            return Some(std::mem::replace(&mut page.ids[rank], id));
        }
        page.rows[word] |= bit;
        page.ids.insert(rank, id);
        self.len += 1;
        None
    }

    pub fn remove(&mut self, key: &CellRef) -> Option<u16> {
        self.get(key)?;
        let page_key = (key.row / PAGE_ROWS, key.col);
        let pages = std::sync::Arc::make_mut(&mut self.pages);
        let page = std::sync::Arc::make_mut(pages.get_mut(&page_key)?);
        let (word, bit, rank) = page.position(key.row % PAGE_ROWS);
        page.rows[word] &= !bit;
        let old = page.ids.remove(rank);
        if page.ids.is_empty() { pages.remove(&page_key); }
        self.len -= 1;
        Some(old)
    }

    pub fn iter(&self) -> impl Iterator<Item = (CellRef, &u16)> {
        self.iter_from(true, 0)
    }

    pub(crate) fn iter_from(&self, rows: bool, at: u32) -> impl Iterator<Item = (CellRef, &u16)> {
        self.pages.iter().filter(move |&(&(p, col), _)| {
            if rows { p >= at / PAGE_ROWS } else { col >= at }
        }).flat_map(move |(&(p, col), page)| {
            let keys = page.rows.iter().enumerate().flat_map(move |(word, &bits)| {
                let mut bits = bits;
                std::iter::from_fn(move || {
                    if bits == 0 { return None; }
                    let row = p * PAGE_ROWS + word as u32 * 64 + bits.trailing_zeros();
                    bits &= bits - 1;
                    Some(CellRef::new(row, col))
                })
            });
            keys.zip(&page.ids).filter(move |(key, _)| !rows || key.row >= at)
        })
    }

    /// Re-key complete style pages when possible. Repack split pages in ascending
    /// row order so a run of equal styles never shifts the packed vector per cell.
    pub(crate) fn remap_axis(&mut self, rows: bool, at: u32, map: impl Fn(u32) -> Option<u32>) {
        if self.pages.keys().all(|&(p, c)| if rows { p < at / PAGE_ROWS } else { c < at }) {
            return;
        }
        let old = std::mem::take(self);
        let mut pages: Vec<_> = old.pages.iter().collect();
        pages.sort_unstable_by_key(|&(&key, _)| key);
        let mut out = CellStyles::default();
        for (&(p, c), page) in pages {
            let base = p * PAGE_ROWS;
            let same_page = if !rows {
                map(c).map(|col| (p, col))
            } else if base + PAGE_ROWS <= at {
                Some((p, c))
            } else {
                map(base).filter(|r| r % PAGE_ROWS == 0 && map(base + PAGE_ROWS - 1) == Some(r + PAGE_ROWS - 1))
                    .map(|r| (r / PAGE_ROWS, c))
            };
            if let Some(key) = same_page {
                std::sync::Arc::make_mut(&mut out.pages).insert(key, page.clone());
                out.len += page.ids.len();
            } else if rows {
                let mut ids = page.ids.iter();
                for (word, &bits) in page.rows.iter().enumerate() {
                    let mut bits = bits;
                    while bits != 0 {
                        let row = base + word as u32 * 64 + bits.trailing_zeros();
                        let id = *ids.next().unwrap();
                        if let Some(target) = map(row) { out.insert(CellRef::new(target, c), id); }
                        bits &= bits - 1;
                    }
                }
            }
        }
        *self = out;
    }

    pub fn keys(&self) -> impl Iterator<Item = CellRef> {
        self.iter().map(|(key, _)| key)
    }

    pub(crate) fn changed_keys(&self, other: &Self) -> impl Iterator<Item = CellRef> {
        self.pages.iter().filter(move |(key, page)| {
            // Uniform table formatting often yields identical pages at the same
            // coordinates after a row shift, even if the page had to be repacked.
            other.pages.get(key).is_none_or(|old| !std::sync::Arc::ptr_eq(old, page) && old != *page)
        }).flat_map(move |(&(p, col), page)| {
            (0..PAGE_ROWS).filter_map(move |row| {
                let key = CellRef::new(p * PAGE_ROWS + row, col);
                let (word, bit, rank) = page.position(row);
                (page.rows[word] & bit != 0 && other.get(&key) != Some(&page.ids[rank])).then_some(key)
            })
        })
    }
}

#[derive(Clone, Debug, Default)]
struct Page {
    /// Unrestricted cell mutation may introduce a qualified reference. Merely
    /// writing a formula's cached result does not invalidate this fast path.
    may_have_qualified_formula: bool,
    may_have_formula: bool,
    /// 本页实际分配的行数（≤ PAGE_ROWS）
    n_rows: u32,
    /// 按列存放：cols[c][r]，r 是页内行偏移。
    /// 用 Arc 做写时复制：工作簿整体 clone（保存快照、后台重算）时只加引用计数，
    /// 真正改到哪一列才复制那一列（列最多 PAGE_ROWS 个 Cell）。
    cols: Vec<std::sync::Arc<Vec<Cell>>>,
}

impl Page {
    fn ensure(&mut self, r_off: u32, c: u32) {
        if self.cols.len() <= c as usize {
            self.cols
                .resize_with(c as usize + 1, || std::sync::Arc::new(Vec::new()));
        }
        // 只把**目标列**补到需要的行数。早先是把页里所有列都补到 n_rows，
        // 稀疏远端行（比如 A1000000）会让每个被碰到的页给全部列各开 256 个槽，
        // 1 万个散点就能吃到 130MB+（QA-07 基线实测 slots=3.4M × 40B）。
        let col = std::sync::Arc::make_mut(&mut self.cols[c as usize]);
        if col.len() <= r_off as usize {
            col.resize_with(r_off as usize + 1, Cell::default);
        }
        if r_off >= self.n_rows {
            self.n_rows = r_off + 1;
        }
    }

    fn get(&self, r_off: u32, c: u32) -> Option<&Cell> {
        let col = self.cols.get(c as usize)?;
        let cell = col.get(r_off as usize)?;
        if cell.is_blank() {
            None
        } else {
            Some(cell)
        }
    }

    fn get_mut(&mut self, r_off: u32, c: u32) -> Option<&mut Cell> {
        let col = std::sync::Arc::make_mut(self.cols.get_mut(c as usize)?);
        let cell = col.get_mut(r_off as usize)?;
        if cell.is_blank() {
            None
        } else {
            Some(cell)
        }
    }
}

/// Snapshots share an identity until a writer forks. The counter alone can
/// coincide on independently edited copies (for example after undo).
#[derive(Clone, Debug, Default)]
pub struct CellVersion {
    lineage: std::sync::Arc<()>,
    revision: u64,
}

impl PartialEq for CellVersion {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision && std::sync::Arc::ptr_eq(&self.lineage, &other.lineage)
    }
}

impl CellVersion {
    fn advance(&mut self) {
        if std::sync::Arc::strong_count(&self.lineage) > 1 {
            self.lineage = std::sync::Arc::new(());
        }
        self.revision = self.revision.wrapping_add(1);
    }
}

#[derive(Clone, Debug, Default)]
pub struct Store {
    pages: std::collections::HashMap<u32, Page>,
    pub(crate) version: CellVersion,
    len: usize,
    // Valid for this snapshot. Removing a boundary cell invalidates it until the next read.
    bounds: std::sync::OnceLock<Option<(u32, u32, u32, u32)>>,
}

impl Store {
    /// Compare only changed column slabs inside a query's rectangles. A COW
    /// fork in a different row of the same slab still leaves this range valid.
    pub(crate) fn same_cells_in(&self, other: &Self, rects: &[(u32,u32,u32,u32)], formulas: bool) -> bool {
        if self.version == other.version { return true; }
        let same_rect = |&(r1, c1, r2, c2): &(u32,u32,u32,u32)| {
            if r1 > r2 || c1 > c2 { return true; }
            let same_page = |&p: &u32| {
                if p < r1 / PAGE_ROWS || p > r2 / PAGE_ROWS { return true; }
                let a = self.pages.get(&p).map(|p| p.cols.as_slice()).unwrap_or_default();
                let b = other.pages.get(&p).map(|p| p.cols.as_slice()).unwrap_or_default();
                let end = (c2 as usize).saturating_add(1).min(a.len().max(b.len()));
                (c1 as usize..end).all(|c| {
                    let (a,b) = (a.get(c), b.get(c));
                    if a.zip(b).is_some_and(|(a,b)| std::sync::Arc::ptr_eq(a,b)) { return true; }
                    let a = a.map(|v| v.as_slice()).unwrap_or_default();
                    let b = b.map(|v| v.as_slice()).unwrap_or_default();
                    let base = p * PAGE_ROWS;
                    let end = (r2-base).saturating_add(1) as usize;
                    (r1.saturating_sub(base) as usize..end.min(a.len().max(b.len()))).all(|i| {
                        let a = a.get(i).filter(|c| !c.is_blank());
                        let b = b.get(i).filter(|c| !c.is_blank());
                        if formulas { a.and_then(|c| c.formula.as_ref()) == b.and_then(|c| c.formula.as_ref()) }
                        else { a.map(|c| &c.value) == b.map(|c| &c.value) }
                    })
                })
            };
            let first = r1 / PAGE_ROWS;
            let last = r2 / PAGE_ROWS;
            if (last - first) as usize <= self.pages.len() {
                return (first..=last).all(|p| same_page(&p));
            }
            self.pages.keys().all(same_page)
                && other.pages.keys().filter(|p| !self.pages.contains_key(p)).all(same_page)
        };
        if rects.is_empty() { same_rect(&(0,0,u32::MAX,u32::MAX)) }
        else { rects.iter().all(same_rect) }
    }

    pub(crate) fn revision(&self) -> u64 { self.version.revision }
    pub fn len(&self) -> usize {
        self.len
    }

    /// 诊断用：返回 (页数, 列基调槽数, 这些槽位里的 Cell 数)。
    /// 稀疏远端行会让每个被碰到的页都按整页行数分配列槽，用这个核对内存来源。
    pub fn stats(&self) -> (usize, usize, usize) {
        let mut slabs = 0usize;
        let mut slots = 0usize;
        for p in self.pages.values() {
            slabs += p.cols.len();
            slots += p.cols.iter().map(|c| c.capacity()).sum::<usize>();
        }
        (self.pages.len(), slabs, slots)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.version.advance();
        self.pages.clear();
        self.len = 0;
        self.bounds.take();
    }

    #[inline]
    fn split(r: u32) -> (u32, u32) {
        (r / PAGE_ROWS, r % PAGE_ROWS)
    }

    pub fn get(&self, r: u32, c: u32) -> Option<&Cell> {
        let (pi, ro) = Self::split(r);
        self.pages.get(&pi)?.get(ro, c)
    }

    pub fn get_mut(&mut self, r: u32, c: u32) -> Option<&mut Cell> {
        let (pi, ro) = Self::split(r);
        let page = self.pages.get_mut(&pi)?;
        page.may_have_qualified_formula = true;
        page.may_have_formula = true;
        let cell = page.get_mut(ro, c)?;
        // A caller holding &mut Cell may turn an occupied boundary cell into a blank.
        Self::invalidate_boundary(&mut self.bounds, r, c);
        self.version.advance();
        Some(cell)
    }

    pub fn contains(&self, r: u32, c: u32) -> bool {
        self.get(r, c).is_some()
    }

    pub(crate) fn set_formula_cache(&mut self, r: u32, c: u32, value: Value) {
        let (pi, ro) = Self::split(r);
        if let Some(cell) = self.pages.get_mut(&pi).and_then(|p| p.get_mut(ro, c)) {
            cell.value = value;
            if cell.is_blank() { Self::invalidate_boundary(&mut self.bounds, r, c); }
            self.version.advance();
        }
    }

    pub(crate) fn foreign_formula_cells(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.pages.iter().filter(|(_, page)| page.may_have_qualified_formula).flat_map(|(&pi, page)| {
            page.cols.iter().enumerate().flat_map(move |(col, cells)| {
                cells.iter().enumerate().filter_map(move |(row, cell)| {
                    cell.formula.as_deref().is_some_and(|f| f.contains('!'))
                        .then_some((CellRef::new(pi * PAGE_ROWS + row as u32, col as u32), cell))
                })
            })
        })
    }

    pub(crate) fn formula_cells(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.pages.iter().filter(|(_, p)| p.may_have_formula).flat_map(|(&pi, p)| {
            p.cols.iter().enumerate().flat_map(move |(c, col)| {
                col.iter().enumerate().filter_map(move |(row, cell)| cell.formula.is_some()
                    .then_some((CellRef::new(pi * PAGE_ROWS + row as u32, c as u32), cell)))
            })
        })
    }

    pub(crate) fn has_formulas(&self) -> bool {
        self.pages.values().any(|page| page.may_have_formula
            && page.cols.iter().any(|col| col.iter().any(|cell| cell.formula.is_some())))
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
        let existed = self.get(r, c).is_some();
        let page = self.pages.entry(pi).or_default();
        page.may_have_qualified_formula |= cell.formula.as_deref().is_some_and(|f| f.contains('!'));
        page.may_have_formula |= cell.formula.is_some();
        page.ensure(ro, c);
        std::sync::Arc::make_mut(&mut page.cols[c as usize])[ro as usize] = cell;
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
        let page = self.pages.get_mut(&pi)?;
        let col = std::sync::Arc::make_mut(page.cols.get_mut(c as usize)?);
        let slot = col.get_mut(ro as usize)?;
        if slot.is_blank() {
            return None;
        }
        let old = std::mem::take(slot);
        self.version.advance();
        self.len -= 1;
        if self.len == 0 {
            self.clear();
        } else {
            Self::invalidate_boundary(&mut self.bounds, r, c);
        }
        Some(old)
    }

    /// 单列范围内的连续访问：跨页拼接，避免逐格哈希查找
    pub fn column_span(&self, r1: u32, r2: u32, c: u32) -> Vec<&Cell> {
        let mut out = Vec::new();
        let mut r = r1;
        while r <= r2 {
            let (pi, ro) = Self::split(r);
            let Some(page) = self.pages.get(&pi) else {
                r = (pi + 1) * PAGE_ROWS;
                continue;
            };
            let Some(col) = page.cols.get(c as usize) else {
                r = (pi + 1) * PAGE_ROWS;
                continue;
            };
            let end = (ro + (r2 - r)).min(page.n_rows.saturating_sub(1));
            for i in ro..=end {
                if let Some(cell) = col.get(i as usize) {
                    out.push(cell);
                }
            }
            r = (pi + 1) * PAGE_ROWS;
        }
        out
    }

    pub(crate) fn cf_pages(&self, (r1, c1, r2, c2): (u32, u32, u32, u32)) -> Vec<crate::cf::PageSlice> {
        let mut result = Vec::new();
        for (&page, data) in &self.pages {
            let base = page * PAGE_ROWS;
            if base > r2 || base + PAGE_ROWS - 1 < r1 { continue; }
            let start = r1.saturating_sub(base) as usize;
            let cols_end = data.cols.len().min(c2 as usize + 1);
            for (offset, cells) in data.cols.get(c1 as usize..cols_end).unwrap_or_default().iter().enumerate() {
                let end = cells.len().min((r2 - base) as usize + 1);
                if start < end {
                    result.push(crate::cf::PageSlice { page, col: c1 + offset as u32, start, end, cells: cells.clone() });
                }
            }
        }
        result.sort_unstable_by_key(|s| (s.page, s.col));
        result
    }

    /// A read-only view of one column's allocated pages, shared until either side writes.
    pub(crate) fn column_pages(&self, col: u32) -> Vec<(u32, std::sync::Arc<Vec<Cell>>)> {
        self.pages.iter().filter_map(|(&page, data)| {
            let cells = data.cols.get(col as usize)?;
            (!cells.is_empty()).then(|| (page * PAGE_ROWS, cells.clone()))
        }).collect()
    }

    /// 按行列升序遍历（页 → 行 → 列）
    pub fn iter(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.iter_rect((0, 0, u32::MAX, u32::MAX))
    }

    pub(crate) fn iter_rect(&self, (r1, c1, r2, c2): (u32, u32, u32, u32)) -> impl Iterator<Item = (CellRef, &Cell)> {
        let mut keys: Vec<_> = self.pages.keys().copied()
            .filter(|&pi| pi >= r1 / PAGE_ROWS && pi <= r2 / PAGE_ROWS).collect();
        keys.sort_unstable();
        keys.into_iter().flat_map(move |pi| {
            let page = self.pages.get(&pi).unwrap();
            let base = pi * PAGE_ROWS;
            let start = r1.saturating_sub(base);
            let end = page.n_rows.min((r2 - base).saturating_add(1));
            let end_col = page.cols.len().min((c2 as usize).saturating_add(1));
            let columns = page.cols.get(c1 as usize..end_col).unwrap_or_default();
            (start..end).flat_map(move |ro| {
                columns.iter().enumerate().filter_map(move |(c, col)| {
                    col.get(ro as usize)
                        .filter(|cell| !cell.is_blank())
                        .map(|cell| (CellRef::new(base + ro, c1 + c as u32), cell))
                })
            })
        })
    }

    pub fn iter_values(&self) -> impl Iterator<Item = &Cell> {
        self.iter().map(|(_, c)| c)
    }

    pub(crate) fn stored_cells(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.pages.iter().flat_map(|(&p, page)| {
            page.cols.iter().enumerate().flat_map(move |(c, col)| {
                col.iter().enumerate().filter_map(move |(r, cell)| {
                    (!cell.is_blank()).then_some((CellRef::new(p * PAGE_ROWS + r as u32, c as u32), cell))
                })
            })
        })
    }

    /// Search sorts its hits afterwards. Visit only allocated slots, in storage
    /// order, so a distant column does not turn sparse pages into dense grids.
    pub(crate) fn stored_cells_in(&self, (r1,c1,r2,c2): (u32,u32,u32,u32)) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.pages.iter().filter(move |&(&p, _)| r1 / PAGE_ROWS <= p && p <= r2 / PAGE_ROWS)
            .flat_map(move |(&p, page)| {
                let base = p * PAGE_ROWS;
                let last = (c2 as usize).saturating_add(1).min(page.cols.len());
                let cols = page.cols.get(c1 as usize..last).unwrap_or_default();
                cols.iter().enumerate().flat_map(move |(c, cells)| {
                    let first = r1.saturating_sub(base) as usize;
                    let last = (r2 - base).saturating_add(1) as usize;
                    cells.get(first..last.min(cells.len())).unwrap_or_default().iter().enumerate()
                        .filter_map(move |(i, cell)| (!cell.is_blank()).then_some((
                            CellRef::new(base + (first + i) as u32, c1 + c as u32), cell)))
                })
            })
    }

    /// Occupied cells in this snapshot that differ from the other snapshot.
    pub(crate) fn changed_keys(&self, other: &Self) -> impl Iterator<Item = CellRef> {
        self.pages.iter().flat_map(move |(&p, page)| {
            page.cols.iter().enumerate().flat_map(move |(col, cells)| {
                let old = other.pages.get(&p).and_then(|page| page.cols.get(col));
                let shared = old.is_some_and(|old| std::sync::Arc::ptr_eq(old, cells));
                (!shared).then_some(cells).into_iter().flat_map(move |cells| {
                    cells.iter().enumerate().filter_map(move |(row, cell)| {
                        (!cell.is_blank() && old.and_then(|c| c.get(row)) != Some(cell))
                            .then_some(CellRef::new(p * PAGE_ROWS + row as u32, col as u32))
                    })
                })
            })
        })
    }

    /// Rebuild the affected page band once. Column edits move whole COW slabs.
    /// Row edits map each old row once per page, then move owned cells directly.
    pub(crate) fn remap_axis(
        &mut self, rows: bool, at: u32, map: impl Fn(u32) -> Option<u32>,
        cleared: &mut std::collections::HashSet<CellRef>,
    ) {
        // Keep one bit per occupied source slot to find holes in the final store.
        // This avoids a temporary coordinate pair or hash-set entry per cell.
        let mut occupied = Vec::<(u32, usize, [u64; 4])>::new();
        let old_pages = std::mem::take(&mut self.pages);
        self.bounds.take();
        self.version.advance();
        for (pi, mut page) in old_pages {
            let base = pi * PAGE_ROWS;
            if (rows && base + PAGE_ROWS <= at) || (!rows && page.cols.len() <= at as usize) {
                self.pages.insert(pi, page);
                continue;
            }
            if !rows {
                let mut columns = Vec::with_capacity(page.cols.len());
                for (c, slab) in std::mem::take(&mut page.cols).into_iter().enumerate() {
                    if c >= at as usize {
                        let mut bits = [0; 4];
                        for (r, cell) in slab.iter().enumerate() {
                            if !cell.is_blank() { bits[r / 64] |= 1 << (r % 64); }
                        }
                        occupied.push((pi, c, bits));
                    }
                    if let Some(c) = map(c as u32) {
                        columns.resize_with(c as usize, Default::default);
                        columns.push(slab);
                    } else {
                        self.len -= slab.iter().filter(|cell| !cell.is_blank()).count();
                    }
                }
                page.cols = columns;
                self.pages.insert(pi, page);
                continue;
            }

            // Each span maps a consecutive source slice into one destination page.
            let mut spans: Vec<(std::ops::Range<usize>, u32, u32)> = Vec::new();
            for r in 0..page.n_rows {
                let Some(target) = map(base + r) else { continue };
                let (dest, offset) = Self::split(target);
                if let Some((source, pid, start)) = spans.last_mut()
                    && *pid == dest && source.end == r as usize && *start + source.len() as u32 == offset {
                    source.end += 1;
                } else {
                    spans.push((r as usize..r as usize + 1, dest, offset));
                }
            }
            for (col, cells) in page.cols.into_iter().enumerate() {
                if cells.is_empty() { continue; }
                let mut bits = [0; 4];
                let begin = at.saturating_sub(base) as usize;
                for (r, cell) in cells.iter().enumerate().skip(begin) {
                    if !cell.is_blank() { bits[r / 64] |= 1 << (r % 64); }
                }
                occupied.push((pi, col, bits));
                if let [(source, dest, 0)] = spans.as_slice()
                    && source.start == 0 && source.end >= cells.len()
                    && map(base + PAGE_ROWS - 1) == Some(*dest * PAGE_ROWS + PAGE_ROWS - 1)
                {
                    // Page-aligned insertion/deletion: transfer this slab's Arc.
                    let p = self.pages.entry(*dest).or_default();
                    p.n_rows = p.n_rows.max(cells.len() as u32);
                    if p.cols.len() <= col { p.cols.resize_with(col + 1, Default::default); }
                    p.cols[col] = cells;
                    p.may_have_formula |= page.may_have_formula;
                    p.may_have_qualified_formula |= page.may_have_qualified_formula;
                    continue;
                }
                let mut cells = std::sync::Arc::unwrap_or_clone(cells);
                for (source, dest, offset) in &spans {
                    let end = source.end.min(cells.len());
                    if source.start >= end { break; }
                    let src = &mut cells[source.start..end];
                    let dest = self.pages.entry(*dest).or_default();
                    dest.may_have_formula |= page.may_have_formula;
                    dest.may_have_qualified_formula |= page.may_have_qualified_formula;
                    dest.ensure(*offset + src.len() as u32 - 1, col as u32);
                    let dst = std::sync::Arc::make_mut(&mut dest.cols[col]);
                    src.swap_with_slice(&mut dst[*offset as usize..*offset as usize + src.len()]);
                }
                // Slots not transferred are precisely the deleted rows.
                self.len -= cells.iter().filter(|cell| !cell.is_blank()).count();
            }
        }
        cleared.retain(|k| (if rows { k.row } else { k.col }) < at || !self.contains(k.row, k.col));
        for (pi, col, words) in occupied {
            let dest = self.pages.get(&pi).and_then(|p| p.cols.get(col));
            for (word, mut bits) in words.into_iter().enumerate() {
                while bits != 0 {
                    let r = word * 64 + bits.trailing_zeros() as usize;
                    if dest.and_then(|cells| cells.get(r)).is_none_or(Cell::is_blank) {
                        cleared.insert(CellRef::new(pi * PAGE_ROWS + r as u32, col as u32));
                    }
                    bits &= bits - 1;
                }
            }
        }
    }

    fn invalidate_boundary(cache: &mut std::sync::OnceLock<Option<(u32, u32, u32, u32)>>, r: u32, c: u32) {
        if cache.get().copied().flatten().is_some_and(|(r1, c1, r2, c2)| {
            r == r1 || r == r2 || c == c1 || c == c2
        }) {
            cache.take();
        }
    }

    pub fn used_bounds(&self) -> Option<(u32, u32, u32, u32)> {
        if self.len == 0 { return None; }
        *self.bounds.get_or_init(|| {
            let mut cells = self.iter().map(|(key, _)| key);
            let first = cells.next()?;
            Some(cells.fold((first.row, first.col, first.row, first.col), |(r1, c1, r2, c2), k| {
                (r1.min(k.row), c1.min(k.col), r2.max(k.row), c2.max(k.col))
            }))
        })
    }

    /// 实际占用的字节数（调试用）
    pub fn allocated_bytes(&self) -> usize {
        let mut n = 0usize;
        for page in self.pages.values() {
            n += std::mem::size_of::<Page>();
            for col in page.cols.iter() {
                n += std::mem::size_of::<Vec<Cell>>() + col.capacity() * std::mem::size_of::<Cell>();
            }
        }
        n
    }

    /// 稀疏保护：某页实际使用的格子占比过低时报告出来（调参用）
    pub fn dense_stats(&self) -> (usize, usize) {
        let mut used = 0usize;
        for (cref, _) in self.iter() {
            let _ = cref;
            used += 1;
        }
        let alloc: usize = self
            .pages
            .values()
            .map(|p| (p.n_rows as usize) * p.cols.len())
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
        Cell { value: Value::Number(v), formula: None }
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
        assert!(std::sync::Arc::ptr_eq(&styles.pages[&(2, 80)], &saved.pages[&(2, 80)]));
    }

    #[test]
    fn sparse_far_rows_do_not_allocate_full_page_slabs() {
        // QA-07 基线发现：往 A1000000 这种远端格子写值，早先每页给所有列各开 256 槽，
        // 1 万个散点 → 340 万个槽（约 137MB）。修好后每个散点只占自己那一列的少量槽。
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
        // 值必须都读得回来，且相邻页边界两侧都对
        assert_eq!(s.get(0, 3).unwrap().value.clone(), Value::Number(0.0));
        assert_eq!(s.get(104 * 9_999, 3).unwrap().value.clone(), Value::Number(9_999.0));
        let n = s.iter().count();
        assert_eq!(n, 10_000);
    }

    #[test]
    fn dense_and_sparse_columns_coexist_in_one_page() {
        // 同一页里 A 列密集、D 列只在远端有值：两列各自长度不同也不能读错
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
        // 工作簿整体 clone（保存快照、后台重算）必须廉价：只加引用计数，
        // 改到哪个列才复制哪个列，而且不能影响原来那份。
        let mut s = Store::default();
        for r in 0..256u32 {
            s.set(r, 0, num(r as f64));
        }
        let clone = s.clone();
        // 同一个列缓冲：指针相同说明没深拷贝
        let a = &s.pages[&0u32].cols[0] as *const std::sync::Arc<Vec<Cell>>;
        let b = &clone.pages[&0u32].cols[0] as *const std::sync::Arc<Vec<Cell>>;
        assert_ne!(a, b, "两个 Store 的 Arc 单元格地址当然不同，这里只验证内容共享");
        assert!(std::sync::Arc::ptr_eq(
            &s.pages[&0u32].cols[0],
            &clone.pages[&0u32].cols[0]
        ));
        // 写 clone 之后：clone 变，原份不变
        let mut clone = clone;
        clone.set(10, 0, num(999.0));
        assert_eq!(clone.get(10, 0).unwrap().value.clone(), Value::Number(999.0));
        assert_eq!(s.get(10, 0).unwrap().value.clone(), Value::Number(10.0));
        assert!(!std::sync::Arc::ptr_eq(
            &s.pages[&0u32].cols[0],
            &clone.pages[&0u32].cols[0]
        ));
    }

    #[test]
    fn occupied_bounds_stay_cached_across_reads_and_independent_snapshots() {
        let mut store = Store::default();
        store.set(300, 3, num(1.0));
        store.set(4, 8, num(2.0));
        store.set(140, 5, num(3.0));
        let saved = store.clone();
        assert_eq!(store.bounds.get(), Some(&Some((4, 3, 300, 8))));
        store.remove(4, 8);
        store.set(350, 1, num(4.0));
        assert!(store.bounds.get().is_none());
        assert_eq!(store.used_bounds(), Some((140, 1, 350, 5)));
        assert_eq!(store.bounds.get(), Some(&store.used_bounds()));
        assert_eq!(saved.used_bounds(), Some((4, 3, 300, 8)));
        *store.get_mut(350, 1).unwrap() = Cell::default();
        assert_eq!(store.used_bounds(), Some((140, 3, 300, 5)));
        store.set(140, 5, Cell::default());
        assert_eq!(store.used_bounds(), Some((300, 3, 300, 3)));
        store.clear();
        assert_eq!(store.used_bounds(), None);
    }

    #[test]
    fn cross_page_access() {
        let mut s = Store::default();
        s.set(0, 0, num(1.0));
        s.set(255, 3, num(2.0));
        s.set(256, 3, num(3.0));
        s.set(1000, 0, num(4.0));
        assert_eq!(s.len(), 4);
        assert_eq!(s.get(0, 0).unwrap().value.clone(), Value::Number(1.0));
        assert_eq!(s.get(255, 3).unwrap().value.clone(), Value::Number(2.0));
        assert_eq!(s.get(256, 3).unwrap().value.clone(), Value::Number(3.0));
        assert_eq!(s.get(1000, 0).unwrap().value.clone(), Value::Number(4.0));
        assert!(s.get(1001, 0).is_none());
    }

    #[test]
    fn set_remove_len() {
        let mut s = Store::default();
        s.set(5, 2, num(7.0));
        assert_eq!(s.len(), 1);
        s.set(5, 2, num(8.0)); // 覆盖不增加计数
        assert_eq!(s.len(), 1);
        assert!(s.remove(5, 2).is_some());
        assert_eq!(s.len(), 0);
        assert!(s.get(5, 2).is_none());
    }

    #[test]
    fn iterates_in_row_col_order() {
        let mut s = Store::default();
        s.set(2, 1, num(1.0));
        s.set(0, 5, num(2.0));
        s.set(300, 0, num(3.0));
        let order: Vec<(u32, u32)> = s.iter().map(|(k, _)| (k.row, k.col)).collect();
        assert_eq!(order, vec![(0, 5), (2, 1), (300, 0)]);
    }

    #[test]
    fn sparse_page_does_not_allocate_full_height() {
        let mut s = Store::default();
        s.set(260, 1, num(1.0)); // 第二页的第一行
        assert_eq!(s.len(), 1);
        assert!(s.allocated_bytes() < 4096, "稀疏页不该按整页分配");
    }
}
