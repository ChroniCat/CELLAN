//! 依赖图：记录每条公式引用了哪些区域，用于编辑后的增量重算。
//!
//! 不把 A1:A20000 展开成 2 万条边（22 万条公式会撑爆内存），只存矩形；
//! 另外按「列号」建一份索引，这样某格变化时只查这一列上的公式，
//! 不必扫全部矩形（那是 12.6 万 × 1.5 万 = 19 亿次比较）。

use super::Node;
use std::collections::{HashMap, HashSet};

pub type CellKey = (usize, u32, u32);

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub sheet: usize,
    pub r1: i64,
    pub c1: i64,
    pub r2: i64,
    pub c2: i64,
}

impl Rect {
    pub fn contains(&self, key: CellKey) -> bool {
        key.0 == self.sheet
            && key.1 as i64 >= self.r1
            && key.1 as i64 <= self.r2
            && key.2 as i64 >= self.c1
            && key.2 as i64 <= self.c2
    }
}

#[derive(Default)]
pub struct Deps {
    /// 公式单元格 → 它引用的区域
    pub refs: HashMap<CellKey, Vec<Rect>>,
    /// (表, 列) → [(公式单元格, 起始行, 结束行)]
    by_col: HashMap<(usize, i64), Vec<(CellKey, i64, i64)>>,
    /// 公式单元格 → 它登记过的列，删除时可以精确定位
    cols_of: HashMap<CellKey, Vec<i64>>,
    pub rect_count: usize,
}

impl Deps {
    pub fn empty() -> Self {
        Deps::default()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    fn index_rect(&mut self, fk: CellKey, rect: Rect) {
        // 按列登记；整行引用（A1:Z1）也就几十列
        let entry = self.cols_of.entry(fk).or_default();
        for c in rect.c1..=rect.c2.min(rect.c1 + 512) {
            if !entry.contains(&c) {
                entry.push(c);
            }
            self.by_col
                .entry((rect.sheet, c))
                .or_default()
                .push((fk, rect.r1, rect.r2));
        }
    }
}

fn sheet_index(wb: &crate::Workbook, name: Option<&str>, cur: usize) -> usize {
    match name {
        Some(n) => wb
            .sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(n))
            .unwrap_or(cur),
        None => cur,
    }
}

fn walk(node: &Node, si: usize, wb: &crate::Workbook, out: &mut Vec<Rect>) {
    match node {
        Node::Ref(re) => {
            let sheet = sheet_index(wb, re.sheet.as_deref(), si);
            let r = re.row.max(0);
            let c = re.col.max(0);
            out.push(Rect { sheet, r1: r, c1: c, r2: r, c2: c });
        }
        Node::Range(a, b) => {
            let sheet = sheet_index(wb, a.sheet.as_deref().or(b.sheet.as_deref()), si);
            out.push(Rect {
                sheet,
                r1: a.row.min(b.row).max(0),
                c1: a.col.min(b.col).max(0),
                r2: a.row.max(b.row).max(0),
                c2: a.col.max(b.col).max(0),
            });
        }
        Node::Func(_, args) => {
            for a in args {
                walk(a, si, wb, out);
            }
        }
        Node::Unary(_, x) | Node::Percent(x) => walk(x, si, wb, out),
        Node::Binary(_, l, r) => {
            walk(l, si, wb, out);
            walk(r, si, wb, out);
        }
        _ => {}
    }
}

pub fn refs_of(wb: &crate::Workbook, si: usize, formula: &str) -> Vec<Rect> {
    let body = formula.strip_prefix('=').unwrap_or(formula);
    let mut out = Vec::new();
    if let Ok(node) = super::parse(body) {
        walk(&node, si, wb, &mut out);
    }
    out
}

impl Deps {
    pub fn build(wb: &crate::Workbook) -> Deps {
        let mut deps = Deps::default();
        for (si, sheet) in wb.sheets.iter().enumerate() {
            for (cref, cell) in sheet.iter() {
                if let Some(f) = cell.formula.as_deref() {
                    let rects = refs_of(wb, si, f);
                    if !rects.is_empty() {
                        let key = (si, cref.row, cref.col);
                        deps.rect_count += rects.len();
                        for r in &rects {
                            deps.index_rect(key, *r);
                        }
                        deps.refs.insert(key, rects);
                    }
                }
            }
        }
        deps
    }

    pub fn update_cell(&mut self, wb: &crate::Workbook, key: CellKey) {
        let f = wb
            .sheets
            .get(key.0)
            .and_then(|s| s.get(key.1, key.2))
            .and_then(|c| c.formula.clone());
        self.refs.remove(&key);
        if let Some(cols) = self.cols_of.remove(&key) {
            let sheet = key.0;
            for c in cols {
                if let Some(list) = self.by_col.get_mut(&(sheet, c)) {
                    list.retain(|(k, _, _)| *k != key);
                    if list.is_empty() {
                        self.by_col.remove(&(sheet, c));
                    }
                }
            }
        }
        if let Some(f) = f {
            let rects = refs_of(wb, key.0, &f);
            if !rects.is_empty() {
                self.rect_count += rects.len();
                for r in &rects {
                    self.index_rect(key, *r);
                }
                self.refs.insert(key, rects);
            }
        }
    }

    /// 从被改动的格子出发，求出所有需要重算的公式单元格
    pub fn closure(&self, seeds: &[CellKey]) -> HashSet<CellKey> {
        let mut dirty: HashSet<CellKey> = HashSet::new();
        // 每轮的「前沿」按列归组，避免逐个格子去扫公式表
        let mut frontier: HashMap<(usize, i64), Vec<u32>> = HashMap::new();
        for (si, r, c) in seeds {
            frontier.entry((*si, *c as i64)).or_default().push(*r);
        }

        let mut rounds = 0;
        while !frontier.is_empty() && rounds < 128 {
            rounds += 1;
            let mut next: HashMap<(usize, i64), Vec<u32>> = HashMap::new();
            for ((sheet, col), mut rows) in frontier {
                rows.sort_unstable();
                rows.dedup();
                let Some(list) = self.by_col.get(&(sheet, col)) else {
                    continue;
                };
                for (fk, r1, r2) in list {
                    if dirty.contains(fk) {
                        continue;
                    }
                    // rows 有序：二分找到 >= r1 的位置，看是否 <= r2
                    let idx = rows.partition_point(|x| (*x as i64) < *r1);
                    if idx < rows.len() && (rows[idx] as i64) <= *r2 {
                        dirty.insert(*fk);
                        next.entry((fk.0, fk.2 as i64)).or_default().push(fk.1);
                    }
                }
            }
            frontier = next;
        }
        dirty
    }
}
