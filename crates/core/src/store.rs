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

#[derive(Clone, Debug, Default)]
struct Page {
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

#[derive(Clone, Debug, Default)]
pub struct Store {
    pages: std::collections::HashMap<u32, Page>,
    len: usize,
    max_row: u32,
    max_col: u32,
}

impl Store {
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
        self.pages.clear();
        self.len = 0;
        self.max_row = 0;
        self.max_col = 0;
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
        self.pages.get_mut(&pi)?.get_mut(ro, c)
    }

    pub fn contains(&self, r: u32, c: u32) -> bool {
        self.get(r, c).is_some()
    }

    /// 写入单元格；cell 为空白时等价于删除
    pub fn set(&mut self, r: u32, c: u32, cell: Cell) {
        let (pi, ro) = Self::split(r);
        let blank = cell.is_blank();
        if blank {
            if let Some(page) = self.pages.get_mut(&pi) {
                if let Some(col) = page.cols.get_mut(c as usize).map(std::sync::Arc::make_mut) {
                    if let Some(slot) = col.get_mut(ro as usize) {
                        if !slot.is_blank() {
                            *slot = Cell::default();
                            self.len -= 1;
                        }
                    }
                }
            }
            return;
        }
        let existed = self.get(r, c).is_some();
        let page = self.pages.entry(pi).or_default();
        page.ensure(ro, c);
        std::sync::Arc::make_mut(&mut page.cols[c as usize])[ro as usize] = cell;
        if !existed {
            self.len += 1;
        }
        if r > self.max_row {
            self.max_row = r;
        }
        if c > self.max_col {
            self.max_col = c;
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
        self.len -= 1;
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

    /// 按行列升序遍历（页 → 行 → 列）
    pub fn iter(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        let mut keys: Vec<u32> = self.pages.keys().copied().collect();
        keys.sort_unstable();
        keys.into_iter().flat_map(move |pi| {
            let page = self.pages.get(&pi).unwrap();
            let n_rows = page.n_rows;
            (0..n_rows).flat_map(move |ro| {
                let base = pi * PAGE_ROWS + ro;
                page.cols.iter().enumerate().filter_map(move |(c, col)| {
                    col.get(ro as usize)
                        .filter(|cell| !cell.is_blank())
                        .map(|cell| (CellRef::new(base, c as u32), cell))
                })
            })
        })
    }

    pub fn iter_values(&self) -> impl Iterator<Item = &Cell> {
        self.iter().map(|(_, c)| c)
    }

    pub fn used_bounds(&self) -> Option<(u32, u32, u32, u32)> {
        if self.len == 0 {
            return None;
        }
        // 左上角要真的找一次（右下角有 max_row/max_col 缓存）
        let mut r1 = u32::MAX;
        let mut c1 = u32::MAX;
        for (cref, _) in self.iter() {
            r1 = r1.min(cref.row);
            c1 = c1.min(cref.col);
            if r1 == 0 && c1 == 0 {
                break;
            }
        }
        Some((r1, c1, self.max_row, self.max_col))
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
