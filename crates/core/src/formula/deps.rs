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

// A reference is indexed once per column, in the smallest aligned row bucket
// containing both endpoints. A point lookup visits at most seven buckets.
const ROW_SHIFTS: [u32; 7] = [5, 10, 15, 20, 25, 30, 32];

fn bucket(row: u32, shift: u32) -> u32 {
    row.checked_shr(shift).unwrap_or(0)
}

fn row_bucket(first: u32, last: u32) -> (u32, u32) {
    let bits = u32::BITS - (first ^ last).leading_zeros();
    let shift = ROW_SHIFTS[ROW_SHIFTS.partition_point(|&s| s < bits)];
    (shift, bucket(first, shift))
}

#[derive(Clone, Copy)]
struct Interval {
    formula: u32,
    first: u32,
    last: u32,
}

#[derive(Default, Clone)]
pub struct Deps {
    /// 公式单元格 → 引用区域。也用于从反向索引移除旧公式。
    pub refs: HashMap<CellKey, Vec<Rect>>,
    /// (表, 列, 行桶位数, 桶号) → 区间。每个普通单格引用只占一个区间。
    by_col: HashMap<(usize, u32, u32, u32), Vec<Interval>>,
    wide: HashMap<usize, Vec<(u32, Rect)>>,
    keys: Vec<CellKey>,
    free_ids: Vec<u32>,
    volatile: HashSet<CellKey>,
    volatile_refs: HashSet<CellKey>,
    visibility: HashSet<CellKey>,
    runtime_refs: HashSet<CellKey>,
    cached_spill_errors: HashSet<CellKey>,
    structural_sensitive: HashSet<CellKey>,
    computed_baseline: bool,
    pub rect_count: usize,
}

impl Deps {
    pub fn empty() -> Self {
        Deps::default()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty() && self.volatile.is_empty() && self.runtime_refs.is_empty()
            && self.cached_spill_errors.is_empty() && self.visibility.is_empty()
    }

    pub fn requires_full_recalc(&self) -> bool {
        !self.runtime_refs.is_empty() || !self.cached_spill_errors.is_empty()
    }

    /// A value-to-value edit can share the existing immutable graph with a worker.
    pub fn cell_needs_update(&self, wb: &crate::Workbook, key: CellKey) -> bool {
        self.refs.contains_key(&key)
            || self.structural_sensitive.contains(&key)
            || self.cached_spill_errors.contains(&key)
            || self.runtime_refs.contains(&key)
            || self.volatile.contains(&key)
            || self.visibility.contains(&key)
            || wb.sheets.get(key.0).and_then(|s| s.get(key.1, key.2))
                .is_some_and(|c| c.formula.is_some())
    }

    pub(crate) fn dynamic_cells(&self) -> impl Iterator<Item = CellKey> + '_ {
        self.runtime_refs.iter().chain(&self.volatile_refs).copied()
    }

    /// Recalculate visibility-aware formulas that could read the filtered sheet, plus
    /// volatile formulas. Unresolved references require the full calculation path.
    pub fn visibility_seeds(&self, sheet: usize) -> Option<Vec<CellKey>> {
        if self.requires_full_recalc() { return None; }
        let mut seeds = self.volatile.clone();
        seeds.extend(self.visibility.iter().copied().filter(|key| {
            self.refs.get(key).is_some_and(|refs| refs.iter().any(|r| r.sheet == sheet))
        }));
        Some(seeds.into_iter().collect())
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
    let Some(ia) = wb
        .sheets
        .iter()
        .position(|s| s.name.eq_ignore_ascii_case(a))
    else {
        return vec![];
    };
    let Some(ib) = wb
        .sheets
        .iter()
        .position(|s| s.name.eq_ignore_ascii_case(b))
    else {
        return vec![];
    };
    let (lo, hi) = if ia <= ib { (ia, ib) } else { (ib, ia) };
    (lo..=hi).collect()
}

#[derive(Default)]
struct Dependencies {
    rects: Vec<Rect>,
    volatile: bool,
    volatile_ref: bool,
    visibility: bool,
    runtime: bool,
    cached_spill_error: bool,
    structural_sensitive: bool,
}

struct Walker<'a> {
    wb: &'a crate::Workbook,
    row: u32,
    col: u32,
    bound: Vec<String>,
    names: Vec<(usize, usize)>,
    name_expansions: usize,
    result: Dependencies,
}

impl Walker<'_> {
    fn formula(&mut self, si: usize, formula: &str) {
        let raw = formula.strip_prefix('=').unwrap_or(formula);
        let expanded =
            crate::table::expand_structured_at(self.wb, si, Some(self.row), Some(self.col), raw);
        match super::compile_at(expanded.as_deref().unwrap_or(raw), self.row, self.col) {
            Ok(node) => self.walk(&node, si),
            Err(_) => self.result.runtime = true,
        }
    }

    fn name(&mut self, si: usize, name: &str, scoped: bool) {
        let Some(index) = super::named_definition(self.wb, name, si, scoped) else {
            self.result.runtime = true;
            return;
        };
        self.definition(si, index);
    }

    fn definition(&mut self, si: usize, index: usize) {
        let wb = self.wb;
        // Cyclic/name-expanding definitions cannot supply a finite static index.
        if self.name_expansions >= 512 || self.names.len() >= 32 || self.names.contains(&(si, index)) {
            self.result.runtime = true;
            return;
        }
        self.name_expansions += 1;
        self.names.push((si, index));
        self.formula(si, &wb.names[index].2);
        self.names.pop();
    }

    fn walk(&mut self, node: &Node, si: usize) {
        let wb = self.wb;
        // These expressions can observe positions, layouts or values beyond their
        // static rectangles. Re-evaluate them conservatively after an axis edit.
        self.result.structural_sensitive |= match node {
            Node::Func(name, _) => !matches!(name.as_str(), "SUM" | "COUNT" | "COUNTA" | "AVERAGE" | "MIN" | "MAX" | "ABS" | "ROUND"),
            Node::Ref(r) => r.book.is_some() || r.sheet2.is_some(),
            Node::Range(a, b) => a.book.is_some() || b.book.is_some() || a.sheet2.is_some() || b.sheet2.is_some(),
            Node::Implicit(_) | Node::Spill(_) | Node::Name(_) | Node::ScopedName { .. } => true,
            _ => false,
        };
        match node {
            Node::Ref(re) | Node::Spill(re) => {
                if matches!(node, Node::Spill(_)) {
                    let idx = sheet_index(wb, re.sheet.as_deref(), si);
                    let anchor = crate::CellRef::new(re.row.max(0) as u32, re.col.max(0) as u32);
                    self.result.runtime |= re.book.is_some() || re.sheet2.is_some()
                        || !wb.sheets.get(idx).is_some_and(|sheet| sheet.spill_shapes.contains_key(&anchor));
                }
                // 外部工作簿的引用不参与本簿依赖图
                if re.book.is_some() {
                    return;
                }
                let sheets = if let (Some(a), Some(b)) = (re.sheet.as_deref(), re.sheet2.as_deref())
                {
                    sheet_span(wb, a, b)
                } else {
                    vec![sheet_index(wb, re.sheet.as_deref(), si)]
                };
                let r = re.row.max(0);
                let c = re.col.max(0);
                for sheet in sheets {
                    self.result.rects.push(Rect {
                        sheet,
                        r1: r,
                        c1: c,
                        r2: r,
                        c2: c,
                    });
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
                    vec![sheet_index(
                        wb,
                        a.sheet.as_deref().or(b.sheet.as_deref()),
                        si,
                    )]
                };
                for sheet in sheets {
                    self.result.rects.push(Rect {
                        sheet,
                        r1: a.row.min(b.row).max(0),
                        c1: a.col.min(b.col).max(0),
                        r2: a.row.max(b.row).max(0),
                        c2: a.col.max(b.col).max(0),
                    });
                }
            }
            Node::Name(n) => {
                if !self.bound.iter().rev().any(|v| v == n) {
                    self.name(si, n, false);
                }
            }
            Node::ScopedName {
                book: None,
                sheet: Some(sheet),
                name,
            } => {
                if let Some(target) = wb
                    .sheets
                    .iter()
                    .position(|s| s.name.eq_ignore_ascii_case(sheet))
                {
                    self.name(target, name, true);
                }
            }
            Node::Func(name, args) if name == "LET" && args.len() >= 3 => {
                let base = self.bound.len();
                for pair in args[..args.len() - 1].chunks_exact(2) {
                    if let Node::Name(n) = &pair[0] {
                        self.walk(&pair[1], si);
                        self.bound.push(n.clone());
                    }
                }
                self.walk(args.last().unwrap(), si);
                self.bound.truncate(base);
            }
            Node::Func(name, args) if name == "LAMBDA" && !args.is_empty() => {
                let base = self.bound.len();
                for p in &args[..args.len() - 1] {
                    if let Node::Name(n) = p {
                        self.bound.push(n.clone());
                    }
                }
                self.walk(args.last().unwrap(), si);
                self.bound.truncate(base);
            }
            Node::Func(name, args) if name == "__CALL__" && matches!(args.first(), Some(Node::ScopedName { book: None, .. })) => {
                let caller = std::mem::take(&mut self.bound);
                self.walk(&args[0], si);
                self.bound = caller;
                for arg in &args[1..] { self.walk(arg, si); }
            }
            Node::Func(name, args) => {
                if !wb.names.is_empty()
                    && !super::functions::listed_builtin(name)
                    && !self.bound.iter().any(|local| local == name)
                    && let Some(index) = super::named_definition(wb, name, si, false)
                {
                    let caller = std::mem::take(&mut self.bound);
                    self.definition(si, index);
                    self.bound = caller;
                }
                // The target may change on any edit. Re-evaluate this formula and all its
                // statically indexed dependents, pulling changed precedents via Calc's scope.
                self.result.volatile_ref |= matches!(name.as_str(), "INDIRECT" | "OFFSET");
                self.result.visibility |= matches!(name.as_str(), "SUBTOTAL" | "AGGREGATE");
                self.result.volatile |= matches!(
                    name.as_str(),
                    "NOW" | "TODAY" | "RAND" | "RANDBETWEEN" | "RANDARRAY"
                );
                let amounts = if name == "SUMIF" {
                    super::sumif_amount_range(args)
                } else {
                    None
                };
                for (i, a) in args.iter().enumerate() {
                    self.walk(
                        if i == 2 {
                            amounts.as_ref().unwrap_or(a)
                        } else {
                            a
                        },
                        si,
                    );
                }
            }
            Node::Implicit(x) | Node::Unary(_, x) | Node::Percent(x) => self.walk(x, si),
            Node::Binary(_, l, r) => {
                self.walk(l, si);
                self.walk(r, si);
            }
            _ => {}
        }
    }
}

pub fn refs_of(wb: &crate::Workbook, si: usize, formula: &str) -> Vec<Rect> {
    dependencies(wb, (si, 0, 0), formula).rects
}

fn dependencies(wb: &crate::Workbook, key: CellKey, formula: &str) -> Dependencies {
    let mut walker = Walker {
        wb,
        row: key.1,
        col: key.2,
        bound: Vec::new(),
        names: Vec::new(),
        name_expansions: 0,
        result: Dependencies::default(),
    };
    walker.formula(key.0, formula);
    walker.result
}

impl Deps {
    pub fn build(wb: &crate::Workbook) -> Deps {
        let mut deps = Deps::default();
        let visit = |si: usize, emit: &mut dyn FnMut(CellKey, Dependencies)| {
            for (cell_ref, cell) in wb.sheets[si].formula_cells() {
                if let Some(f) = cell.formula.as_deref() {
                    let key = (si, cell_ref.row, cell_ref.col);
                    let mut info = dependencies(wb, key, f);
                    info.cached_spill_error = super::is_cached_spill_error(&cell.value);
                    emit(key, info);
                }
            }
        };
        let total: usize = wb.sheets.iter().map(|s| s.len()).sum();
        let workers = if wb.sheets.len() > 1 && total >= 16_384 {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(wb.sheets.len())
                .min(8)
        } else {
            1
        };
        if workers == 1 {
            for si in 0..wb.sheets.len() {
                visit(si, &mut |key, refs| deps.insert(key, refs));
            }
        } else {
            use std::sync::{
                atomic::{AtomicUsize, Ordering},
                mpsc,
            };
            let next = AtomicUsize::new(0);
            std::thread::scope(|scope| {
                // Bound the number of parsed batches waiting for the index builder.
                let (tx, rx) = mpsc::sync_channel(workers);
                for _ in 0..workers {
                    let (tx, next, visit) = (tx.clone(), &next, &visit);
                    scope.spawn(move || {
                        let mut batch = Vec::with_capacity(128);
                        loop {
                            let si = next.fetch_add(1, Ordering::Relaxed);
                            if si >= wb.sheets.len() {
                                break;
                            }
                            visit(si, &mut |k, d| {
                                batch.push((k, d));
                                if batch.len() == 128 {
                                    tx.send(std::mem::replace(&mut batch, Vec::with_capacity(128)))
                                        .unwrap();
                                }
                            });
                        }
                        if !batch.is_empty() {
                            tx.send(batch).unwrap();
                        }
                    });
                }
                drop(tx);
                for batch in rx {
                    for (key, refs) in batch {
                        deps.insert(key, refs);
                    }
                }
            });
        }
        deps
    }

    /// Use only immediately after a completed whole-workbook calculation.
    /// A graph built from a fast-opened OOXML file does not validate its <v> caches.
    pub fn build_after_recalc(wb: &crate::Workbook) -> Self {
        let mut graph = Self::build(wb);
        graph.computed_baseline = !wb.calc_manual && !wb.calc_pending;
        graph
    }

    fn insert(&mut self, key: CellKey, info: Dependencies) {
        if info.structural_sensitive { self.structural_sensitive.insert(key); }
        if info.visibility { self.visibility.insert(key); }
        if info.cached_spill_error { self.cached_spill_errors.insert(key); }
        if info.runtime {
            self.runtime_refs.insert(key);
        }
        if info.volatile_ref {
            self.volatile_refs.insert(key);
        }
        if info.volatile || info.volatile_ref {
            self.volatile.insert(key);
        }
        let mut rects = info.rects;
        // The model addresses rows and columns with u32.
        rects.retain(|r| r.r1 <= u32::MAX as i64 && r.c1 <= u32::MAX as i64);
        if rects.is_empty() {
            return;
        }
        let id = if let Some(id) = self.free_ids.pop() {
            self.keys[id as usize] = key;
            id
        } else {
            let id = u32::try_from(self.keys.len()).expect("more than 2^32 indexed formulas");
            self.keys.push(key);
            id
        };
        for r in &rects {
            if r.c2 - r.c1 > 512 {
                self.wide.entry(r.sheet).or_default().push((id, *r));
            } else {
                let first = r.r1 as u32;
                let last = r.r2.min(u32::MAX as i64) as u32;
                let (shift, bin) = row_bucket(first, last);
                for c in r.c1..=r.c2.min(u32::MAX as i64) {
                    self.by_col
                        .entry((r.sheet, c as u32, shift, bin))
                        .or_default()
                        .push(Interval {
                            formula: id,
                            first,
                            last,
                        });
                }
            }
        }
        self.rect_count += rects.len();
        rects.shrink_to_fit();
        self.refs.insert(key, rects);
    }

    pub fn update_cell(&mut self, wb: &crate::Workbook, key: CellKey) {
        if let Some(rects) = self.refs.remove(&key) {
            self.rect_count -= rects.len();
            let mut freed = None;
            for r in rects {
                if r.c2 - r.c1 > 512 {
                    if let Some(list) = self.wide.get_mut(&r.sheet) {
                        list.retain(|&(id, _)| {
                            if self.keys[id as usize] == key {
                                freed = Some(id);
                                false
                            } else {
                                true
                            }
                        });
                        if list.is_empty() {
                            self.wide.remove(&r.sheet);
                        }
                    }
                } else {
                    let (shift, bin) = row_bucket(r.r1 as u32, r.r2.min(u32::MAX as i64) as u32);
                    for c in r.c1..=r.c2.min(u32::MAX as i64) {
                        let index = (r.sheet, c as u32, shift, bin);
                        if let Some(list) = self.by_col.get_mut(&index) {
                            list.retain(|i| {
                                if self.keys[i.formula as usize] == key {
                                    freed = Some(i.formula);
                                    false
                                } else {
                                    true
                                }
                            });
                            if list.is_empty() {
                                self.by_col.remove(&index);
                            }
                        }
                    }
                }
            }
            if let Some(id) = freed {
                self.free_ids.push(id);
            }
        }
        self.structural_sensitive.remove(&key);
        self.runtime_refs.remove(&key);
        self.volatile_refs.remove(&key);
        self.visibility.remove(&key);
        self.volatile.remove(&key);
        self.cached_spill_errors.remove(&key);
        if let Some(cell) = wb.sheets.get(key.0).and_then(|s| s.get(key.1, key.2))
            && let Some(f) = cell.formula.as_deref()
        {
            let mut info = dependencies(wb, key, f);
            info.cached_spill_error = super::is_cached_spill_error(&cell.value);
            self.insert(key, info);
        }
    }

    pub fn supports_structural(&self, wb: &crate::Workbook) -> bool {
        self.computed_baseline && !self.requires_full_recalc() && wb.names.is_empty() && !wb.calc_iterate &&
            wb.sheets.iter().all(|s| s.tables.is_empty() && s.arrays.is_empty() && s.spill_shapes.is_empty() && s.spilled.is_empty())
    }

    /// Reindex a broad edit on the worker. A cached scalar remains valid when
    /// every point reference in the rewritten formula still denotes the very same
    /// input, in operand order. Ranges and layout-sensitive functions are dirty.
    pub fn recalc_structural(&self, wb: &mut crate::Workbook, sheet: usize, op: &crate::structural::Structural,
                             cancel: &std::sync::atomic::AtomicBool) -> Option<Self> {
        if !self.supports_structural(wb) { return None; }
        let mut next = Self { computed_baseline: true, ..Default::default() };
        let mut seeds = Vec::new();
        let shift = op.reference_shift();
        let relocate = |(si,r,c): CellKey| {
            if si == sheet { crate::sync::shift_structural_coord(r,c,op).map(|(r,c)| (si,r,c)) }
            else { Some((si,r,c)) }
        };
        for old in self.refs.keys().chain(self.structural_sensitive.iter().filter(|k| !self.refs.contains_key(k))) {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) { return None; }
            let Some(key) = relocate(*old) else { continue };
            let rects = self.refs.get(old).map(Vec::as_slice).unwrap_or_default();
            if !self.structural_sensitive.contains(old) && key == *old && !rects.iter().any(|r|
                r.sheet == sheet && (match shift.axis { crate::refs::Axis::Row => r.r2, crate::refs::Axis::Col => r.c2 }) >= i64::from(shift.at)) {
                next.insert(key, Dependencies { rects: rects.to_owned(), ..Default::default() });
                continue;
            }
            let cell = wb.sheets[key.0].get(key.1,key.2)?;
            let formula = cell.formula.as_deref()?;
            let mut info = dependencies(wb,key,formula);
            info.cached_spill_error = super::is_cached_spill_error(&cell.value);
            let stable = cell.value != crate::Value::Empty && !self.structural_sensitive.contains(old) && !info.structural_sensitive && !info.runtime
                && info.rects.len() == rects.len() && rects.iter().zip(&info.rects).all(|(r,n)| {
                    r.r1 == r.r2 && r.c1 == r.c2 && n.r1 == n.r2 && n.c1 == n.c2
                        && r.r1 <= i64::from(u32::MAX) && r.c1 <= i64::from(u32::MAX)
                        && relocate((r.sheet,r.r1 as u32,r.c1 as u32))
                            .is_some_and(|(s,row,col)| s == n.sheet && i64::from(row) == n.r1 && i64::from(col) == n.c1)
                });
            if !stable { seeds.push(key); }
            next.insert(key,info);
        }
        if !super::recalc_dirty_cancelable(wb,&next,&seeds,cancel) { return None; }
        // Array-producing and formerly blocked formulas can change index metadata
        // even if no array existed before the operation.
        if !next.supports_structural(wb) || wb.sheets.iter().any(|sh|
            sh.formula_cells().any(|(_,cell)| super::is_cached_spill_error(&cell.value))) {
            return Some(Self::build_after_recalc(wb));
        }
        Some(next)
    }

    /// Identify old and new formula slots to reindex after an ordinary row/column
    /// edit. Large changes are left to the caller's background rebuild.
    pub fn structural_updates(&self, wb: &crate::Workbook, sheet: usize, op: &crate::structural::Structural, limit: usize) -> Option<Vec<CellKey>> {
        if !self.supports_structural(wb) { return None; }
        let shift = op.reference_shift();
        let mut slots = HashSet::new();
        for key in self.refs.keys().chain(&self.structural_sensitive) {
            let &(si, row, col) = key;
            let moved = if si == sheet {
                crate::sync::shift_structural_coord(row, col, op).map(|(r,c)| (si,r,c))
            } else { Some(*key) };
            if moved == Some(*key) && !self.structural_sensitive.contains(key)
                && !self.refs.get(key).is_some_and(|refs| refs.iter().any(|r| {
                    r.sheet == sheet && (match shift.axis {
                        crate::refs::Axis::Row => r.r2,
                        crate::refs::Axis::Col => r.c2,
                    }) >= i64::from(shift.at)
                })) { continue; }
            slots.insert(*key);
            slots.extend(moved);
            if slots.len() > limit { return None; }
        }
        Some(slots.into_iter().collect())
    }

    pub fn closure(&self, seeds: &[CellKey]) -> HashSet<CellKey> {
        self.collect(seeds, false, usize::MAX, None).unwrap()
    }

    pub fn edit_closure(&self, seeds: &[CellKey]) -> HashSet<CellKey> {
        self.collect(seeds, true, usize::MAX, None).unwrap()
    }

    pub fn edit_closure_limited(
        &self,
        seeds: &[CellKey],
        limit: usize,
    ) -> Option<HashSet<CellKey>> {
        self.collect(seeds, true, limit, None)
    }

    pub(super) fn edit_closure_cancelable(
        &self,
        seeds: &[CellKey],
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Option<HashSet<CellKey>> {
        self.collect(seeds, true, usize::MAX, cancel)
    }

    fn collect(
        &self,
        seeds: &[CellKey],
        edit: bool,
        limit: usize,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Option<HashSet<CellKey>> {
        let mut dirty = HashSet::new();
        let mut frontier: Vec<CellKey> = seeds.to_vec();
        if edit && !seeds.is_empty() {
            if self.volatile.len() > limit {
                return None;
            }
            dirty.extend(self.volatile.iter().copied());
            frontier.extend(self.volatile.iter().copied());
        }
        let mut next = Vec::new();
        while !frontier.is_empty() {
            if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                return None;
            }
            // Group frontiers so that thousands of pasted rows visit each bucket only once.
            frontier.sort_unstable_by_key(|&(s, r, c)| (s, c, r));
            frontier.dedup();
            let mut add = |id: u32| {
                let key = self.keys[id as usize];
                if dirty.insert(key) {
                    next.push(key);
                }
                dirty.len() <= limit
            };
            for column in frontier.chunk_by(|a, b| a.0 == b.0 && a.2 == b.2) {
                let (sheet, _, col) = column[0];
                for shift in ROW_SHIFTS {
                    for rows in column.chunk_by(|a, b| bucket(a.1, shift) == bucket(b.1, shift)) {
                        let bin = bucket(rows[0].1, shift);
                        if let Some(entries) = self.by_col.get(&(sheet, col, shift, bin)) {
                            for entry in entries {
                                let p = rows.partition_point(|k| k.1 < entry.first);
                                if p < rows.len() && rows[p].1 <= entry.last && !add(entry.formula)
                                {
                                    return None;
                                }
                            }
                        }
                    }
                }
                if let Some(entries) = self.wide.get(&sheet) {
                    for &(id, r) in entries {
                        if (col as i64) < r.c1 || (col as i64) > r.c2 {
                            continue;
                        }
                        let p = column.partition_point(|k| (k.1 as i64) < r.r1);
                        if p < column.len() && (column[p].1 as i64) <= r.r2 && !add(id) {
                            return None;
                        }
                    }
                }
            }
            frontier.clear();
            std::mem::swap(&mut frontier, &mut next);
        }
        Some(dirty)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, Sheet, Value, Workbook, formula};

    fn formula(wb: &mut Workbook, si: usize, row: u32, col: u32, text: &str) {
        wb.sheets[si].set(
            row,
            col,
            Cell {
                value: Value::Number(0.0),
                formula: Some(text.into()),
            },
        );
    }

    #[test]
    fn edit_seeds_include_volatile_cells_and_their_dependents() {
        let mut wb = Workbook::new_blank();
        formula(&mut wb, 0, 0, 0, "=TODAY()");
        formula(&mut wb, 0, 0, 1, "=A1+1");
        formula(&mut wb, 0, 0, 2, "=7");
        let d = Deps::build(&wb);
        assert!(!d.requires_full_recalc());
        let seed = [(0, 5, 0)];
        assert!(d.closure(&seed).is_empty());
        assert_eq!(d.edit_closure(&seed), HashSet::from([(0, 0, 0), (0, 0, 1)]));
        assert_eq!(formula::recalc_dirty(&mut wb, &d, &seed), 2);
        let Value::Number(today) = wb.sheets[0].value(0, 0) else {
            panic!("expected Excel date")
        };
        assert!(today > 1.0);
        assert_eq!(wb.sheets[0].value(0, 1), Value::Number(today + 1.0));
    }

    #[test]
    fn names_follow_sheet_scope_and_let_sequential_binding() {
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Report"));
        wb.names.extend([
            ("Rate".to_owned(), None, "Sheet1!A1".to_owned()),
            ("Rate".to_owned(), Some(1), "Report!B1".to_owned()),
            ("Alias".to_owned(), None, "Rate".to_owned()),
        ]);
        formula(&mut wb, 1, 0, 2, "=LET(rate,Alias,f,LAMBDA(x,x+rate),f(3))");
        formula(&mut wb, 0, 0, 2, "=Alias");
        formula(&mut wb, 0, 0, 3, "=Report!Rate");
        let d = Deps::build(&wb);
        assert!(!d.requires_full_recalc());
        assert_eq!(
            d.closure(&[(1, 0, 1)]),
            HashSet::from([(1, 0, 2), (0, 0, 3)])
        );
        assert_eq!(d.closure(&[(0, 0, 0)]), HashSet::from([(0, 0, 2)]));
        formula(&mut wb, 0, 0, 4, "=LET(place,\"A1\",INDIRECT(place))");
        let d = Deps::build(&wb);
        assert!(!d.requires_full_recalc());
        assert!(d.edit_closure(&[(1, 9, 9)]).contains(&(0, 0, 4)));
        wb.names
            .push(("LeftAlias".to_owned(), None, "RightAlias".to_owned()));
        wb.names
            .push(("RightAlias".to_owned(), None, "LeftAlias".to_owned()));
        let d = dependencies(&wb, (0, 0, 0), "=LeftAlias");
        assert!(d.runtime);
    }

    #[test]
    fn index_updates_shared_buckets_wide_ranges_and_duplicate_refs() {
        let mut wb = Workbook::new_blank();
        formula(&mut wb, 0, 0, 0, "=SUM(B31:B34,B33,B33,AA1:XFD90)");
        formula(&mut wb, 0, 1, 0, "=B33");
        let mut d = Deps::build(&wb);
        assert_eq!(d.closure(&[(0, 32, 1)]).len(), 2);
        assert!(d.closure(&[(0, 33, 1)]).contains(&(0, 0, 0)));
        assert_eq!(d.closure(&[(0, 34, 1)]).len(), 0);
        assert_eq!(d.closure(&[(0, 42, 999)]).len(), 1);
        formula(&mut wb, 0, 0, 0, "=SUM(C1:C2)");
        d.update_cell(&wb, (0, 0, 0));
        assert_eq!(d.closure(&[(0, 32, 1)]), HashSet::from([(0, 1, 0)]));
        assert!(d.closure(&[(0, 42, 999)]).is_empty());
        assert_eq!(d.closure(&[(0, 1, 2)]), HashSet::from([(0, 0, 0)]));
        assert_eq!(d.rect_count, 2);
    }

    #[test]
    fn interactive_defers_large_static_closure_and_worker_honours_cancellation() {
        use formula::InteractiveRecalc::*;
        use std::sync::atomic::AtomicBool;
        let mut wb = Workbook::new_blank();
        for r in 1..1300 {
            formula(&mut wb, 0, r, 0, &format!("=A{}+1", r));
        }
        let d = Deps::build(&wb);
        let seeds = [(0, 0, 0)];
        assert!(d.edit_closure_limited(&seeds, 96).is_none());
        assert_eq!(
            formula::recalc_interactive_plan(&mut wb, &d, &seeds, true),
            BackgroundDirty
        );
        assert_eq!(
            formula::recalc_interactive_plan(&mut wb, &d, &seeds, false),
            BackgroundFull
        );
        assert!(!formula::recalc_dirty_cancelable(
            &mut wb,
            &d,
            &seeds,
            &AtomicBool::new(true)
        ));
        assert!(formula::recalc_dirty_cancelable(
            &mut wb,
            &d,
            &seeds,
            &AtomicBool::new(false)
        ));
        assert_eq!(wb.sheets[0].value(1299, 0), Value::Number(1299.0));
    }

    #[test]
    fn parallel_build_matches_cross_sheet_row_closure() {
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Report"));
        for r in 0..8192 {
            wb.sheets[0].set(
                r,
                0,
                Cell {
                    value: Value::Number(2.0),
                    formula: None,
                },
            );
            formula(&mut wb, 1, r, 0, &format!("=Sheet1!A{}", r + 1));
        }
        let d = Deps::build(&wb);
        assert_eq!(d.rect_count, 8192);
        assert_eq!(
            d.closure(&[(0, 999, 0), (0, 8191, 0)]),
            HashSet::from([(1, 999, 0), (1, 8191, 0)])
        );
    }

    #[test]
    #[ignore = "manual dependency-chain timing, run with --release --ignored --nocapture"]
    fn dependency_chain_timing() {
        for n in [10_000, 20_000] {
            let mut wb = Workbook::new_blank();
            for r in 1..=n {
                formula(&mut wb, 0, r, 0, &format!("=A{}+1", r));
            }
            let at = std::time::Instant::now();
            let d = Deps::build(&wb);
            let build = at.elapsed();
            let at = std::time::Instant::now();
            assert_eq!(d.closure(&[(0, 0, 0)]).len(), n as usize);
            eprintln!("{n} formulas: build {build:?}, closure {:?}", at.elapsed());
        }
    }
}

#[cfg(test)]
mod structural_tests {
    use super::*;
    use crate::{Cell, Sheet, Value, Workbook, structural::{apply_structural_wb, Structural}};

    #[test]
    fn incremental_axis_edits_match_full_and_reindex_cross_sheet_readers() {
        for op in [Structural::InsertRows { at: 3, n: 1 }, Structural::DeleteRows { at: 3, n: 1 },
                   Structural::InsertCols { at: 3, n: 1 }, Structural::DeleteCols { at: 3, n: 1 }] {
            let mut wb = Workbook::new_blank();
            wb.sheets.push(Sheet::new("Report"));
            wb.sheets[0].set(3, 3, Cell { value: Value::Number(7.0), formula: None });
            for (si,r,c,f) in [(0,0,0,"=2+2"), (0,4,4,"=ROW()+COLUMN()"), (1,0,0,"=Sheet1!D4"),
                    (1,0,1,"=A1+2"), (1,0,2,"=SUM(Sheet1!A1:E5)"), (1,0,3,"=SUM(A10:B10)")] {
                wb.sheets[si].set(r,c, Cell { value: Value::Empty, formula: Some(f.into()) });
            }
            crate::formula::recalc_all(&mut wb);
            assert!(Deps::build(&wb).structural_updates(&wb,0,&op,50).is_none(),
                "building edges alone cannot certify cached formula values");
            let mut deps = Deps::build_after_recalc(&wb);
            let old = deps.clone();
            let keys = deps.structural_updates(&wb, 0, &op, 50).unwrap();
            assert!(keys.contains(&(1,0,0)));
            assert!(!keys.contains(&(1,0,3)));
            apply_structural_wb(&mut wb, 0, &op);
            for &k in &keys { deps.update_cell(&wb, k); }
            let mut background = wb.clone();
            let broad = old.recalc_structural(&mut background,0,&op,&false.into()).unwrap();
            let mut full = wb.clone();
            crate::formula::recalc_all(&mut full);
            crate::formula::recalc_dirty(&mut wb, &deps, &keys);
            for (si, sheet) in full.sheets.iter().enumerate() {
                for (pos, cell) in sheet.iter() {
                    assert_eq!(background.sheets[si].value(pos.row,pos.col),cell.value,"background {op:?} at {pos:?}");
                    assert_eq!(wb.sheets[si].value(pos.row,pos.col), cell.value, "{op:?} at {pos:?}");
                }
            }
            let rebuilt = Deps::build(&wb);
            for r in 0..7 {
                for c in 0..7 {
                    assert_eq!(deps.edit_closure(&[(0,r,c)]), rebuilt.edit_closure(&[(0,r,c)]));
                    assert_eq!(broad.edit_closure(&[(0,r,c)]), rebuilt.edit_closure(&[(0,r,c)]));
                }
            }
        }
    }
}
