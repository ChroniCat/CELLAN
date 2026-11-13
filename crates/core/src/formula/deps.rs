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
    cols_of: HashMap<CellKey, Vec<(usize, i64)>>,
    /// 宽区域保留完整矩形，避免每条整行引用展开成上万列。
    wide: HashMap<CellKey, Vec<Rect>>,
    /// 名称和动态引用不能仅靠静态 AST 确定依赖，编辑时保守全量重算。
    dynamic: HashSet<CellKey>,
    /// 真正无法静态确定的依赖；易变的 RAND/NOW 本身不属于此集合。
    runtime_refs: HashSet<CellKey>,
    pub rect_count: usize,
}

impl Deps {
    pub fn empty() -> Self {
        Deps::default()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty() && self.dynamic.is_empty()
    }

    pub fn requires_full_recalc(&self) -> bool {
        !self.dynamic.is_empty()
    }

    pub(crate) fn dynamic_cells(&self) -> impl Iterator<Item = CellKey> + '_ {
        self.runtime_refs.iter().copied()
    }

    fn index_rect(&mut self, fk: CellKey, rect: Rect) {
        if rect.c2 - rect.c1 > 512 {
            self.wide.entry(fk).or_default().push(rect);
            return;
        }
        let entry = self.cols_of.entry(fk).or_default();
        for c in rect.c1..=rect.c2 {
            let column = (rect.sheet, c);
            if !entry.contains(&column) {
                entry.push(column);
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

/// 三维引用 `Sheet1:Sheet3` 的 sheet 下标；端点缺失时返回空，让依赖图
/// 保守地不把错误引用挂到别的表上。
fn sheet_span(wb: &crate::Workbook, a: &str, b: &str) -> Vec<usize> {
    let Some(ia) = wb.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(a)) else { return vec![] };
    let Some(ib) = wb.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(b)) else { return vec![] };
    let (lo, hi) = if ia <= ib { (ia, ib) } else { (ib, ia) };
    (lo..=hi).collect()
}

fn walk(node: &Node, si: usize, wb: &crate::Workbook, out: &mut Vec<Rect>) {
    match node {
        Node::Ref(re) | Node::Spill(re) => {
            // 外部工作簿的引用不参与本簿依赖图
            if re.book.is_some() {
                return;
            }
            let sheets = if let (Some(a), Some(b)) = (re.sheet.as_deref(), re.sheet2.as_deref()) {
                sheet_span(wb, a, b)
            } else {
                vec![sheet_index(wb, re.sheet.as_deref(), si)]
            };
            let r = re.row.max(0);
            let c = re.col.max(0);
            for sheet in sheets {
                out.push(Rect { sheet, r1: r, c1: c, r2: r, c2: c });
            }
        }
        Node::Range(a, b) => {
            if a.book.is_some() || b.book.is_some() {
                return;
            }
            let sheets = if let (Some(x), Some(y)) = (a.sheet.as_deref(), a.sheet2.as_deref()) {
                sheet_span(wb, x, y)
            } else if let (Some(x), Some(y)) = (b.sheet.as_deref(), b.sheet2.as_deref()) {
                sheet_span(wb, x, y)
            } else {
                vec![sheet_index(wb, a.sheet.as_deref().or(b.sheet.as_deref()), si)]
            };
            for sheet in sheets {
                out.push(Rect {
                    sheet,
                    r1: a.row.min(b.row).max(0),
                    c1: a.col.min(b.col).max(0),
                    r2: a.row.max(b.row).max(0),
                    c2: a.col.max(b.col).max(0),
                });
            }
        }
        Node::Func(_, args) => {
            for a in args {
                walk(a, si, wb, out);
            }
        }
        Node::Implicit(x) | Node::Unary(_, x) | Node::Percent(x) => walk(x, si, wb, out),
        Node::Binary(_, l, r) => {
            walk(l, si, wb, out);
            walk(r, si, wb, out);
        }
        _ => {}
    }
}

pub fn refs_of(wb: &crate::Workbook, si: usize, formula: &str) -> Vec<Rect> {
    dependencies(wb, si, 0, 0, formula).0
}

fn dynamic(node: &Node, include_volatile: bool) -> bool {
    match node {
        Node::Name(_) => true,
        Node::Spill(_) => true,
        Node::Func(name, args) => matches!(name.as_str(), "INDIRECT" | "OFFSET")
            || (include_volatile && matches!(name.as_str(), "NOW" | "TODAY" | "RAND" | "RANDBETWEEN" | "RANDARRAY"))
            || args.iter().any(|n| dynamic(n, include_volatile)),
        Node::Implicit(n) | Node::Unary(_, n) | Node::Percent(n) => dynamic(n, include_volatile),
        Node::Binary(_, a, b) => dynamic(a, include_volatile) || dynamic(b, include_volatile),
        _ => false,
    }
}

fn dependencies(wb: &crate::Workbook, si: usize, row: u32, col: u32, formula: &str) -> (Vec<Rect>, bool, bool) {
    let raw = formula.strip_prefix('=').unwrap_or(formula);
    // 与求值入口保持相同的结构化引用展开和 OOXML/整列规范化。
    let expanded = crate::table::expand_structured_at(wb, si, Some(row), Some(col), raw).unwrap_or_else(|| raw.to_owned());
    let body = crate::normalize_formula(&expanded);
    let mut out = Vec::new();
    if let Ok(node) = super::parse(&body) {
        walk(&node, si, wb, &mut out);
        (out, dynamic(&node, true), dynamic(&node, false))
    } else {
        (out, true, true)
    }
}

impl Deps {
    pub fn build(wb: &crate::Workbook) -> Deps {
        let mut deps = Deps::default();
        for (si, sheet) in wb.sheets.iter().enumerate() {
            for (cref, cell) in sheet.iter() {
                if let Some(f) = cell.formula.as_deref() {
                    let key = (si, cref.row, cref.col);
                    let (rects, dynamic, runtime) = dependencies(wb, si, key.1, key.2, f);
                    if dynamic {
                        deps.dynamic.insert(key);
                    }
                    if runtime {
                        deps.runtime_refs.insert(key);
                    }
                    if !rects.is_empty() {
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
        if let Some(old) = self.refs.remove(&key) {
            self.rect_count -= old.len();
        }
        self.wide.remove(&key);
        self.dynamic.remove(&key);
        self.runtime_refs.remove(&key);
        if let Some(cols) = self.cols_of.remove(&key) {
            for column in cols {
                if let Some(list) = self.by_col.get_mut(&column) {
                    list.retain(|(k, _, _)| *k != key);
                    if list.is_empty() {
                        self.by_col.remove(&column);
                    }
                }
            }
        }
        if let Some(f) = f {
            let (rects, dynamic, runtime) = dependencies(wb, key.0, key.1, key.2, &f);
            if dynamic {
                self.dynamic.insert(key);
            }
            if runtime {
                self.runtime_refs.insert(key);
            }
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

        // dirty 保证每个公式最多入队一次，循环依赖也会自然终止。
        while !frontier.is_empty() {
            let mut next: HashMap<(usize, i64), Vec<u32>> = HashMap::new();
            for ((sheet, col), mut rows) in frontier {
                rows.sort_unstable();
                rows.dedup();
                let narrow = self.by_col.get(&(sheet, col)).into_iter().flatten();
                let wide = self.wide.iter().flat_map(|(fk, rects)| {
                    rects.iter().filter(move |r| r.sheet == sheet && col >= r.c1 && col <= r.c2)
                        .map(move |r| (*fk, r.r1, r.r2))
                });
                for (fk, r1, r2) in narrow.copied().chain(wide) {
                    if dirty.contains(&fk) {
                        continue;
                    }
                    // rows 有序：二分找到 >= r1 的位置，看是否 <= r2
                    let idx = rows.partition_point(|x| (*x as i64) < r1);
                    if idx < rows.len() && (rows[idx] as i64) <= r2 {
                        dirty.insert(fk);
                        next.entry((fk.0, fk.2 as i64)).or_default().push(fk.1);
                    }
                }
            }
            frontier = next;
        }
        dirty
    }
}
