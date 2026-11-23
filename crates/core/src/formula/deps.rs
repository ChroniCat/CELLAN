//! 依赖图：记录每条公式引用了哪些区域，用于编辑后的增量重算。
//!
//! 关键设计：
//! 1. 单格引用进入按列有序点表 `points_by_col`，二分查找 `O(log N)`，零行桶哈希探测；
//! 2. 多格区域通过 `AreaId` 全局去重（10 万条公式引用同一 `$A$1:$CV$100000` 仅存 1 份列区间）；
//! 3. `FormulaRefs` 内联存储 0~2 个紧凑 `CompactRect`（20B），消除每条普通公式的独立 `Vec` 堆分配；
//! 4. `update_cell` 采用墓碑（Tombstone）惰性失效，单格编辑摊还 `O(1)`；
//! 5. 动态数组活跃溢出区与 `#SPILL!` 阻塞监视矩形直接编入依赖图，局部唤醒锚点，消除整簿全量回退。

use super::Node;
use std::collections::{HashMap, HashSet};

pub type CellKey = (usize, u32, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CompactRect {
    pub sheet: u32,
    pub r1: u32,
    pub c1: u32,
    pub r2: u32,
    pub c2: u32,
}

impl CompactRect {
    #[inline]
    fn from_rect(r: Rect) -> Option<Self> {
        if r.r1 > u32::MAX as i64 || r.c1 > u32::MAX as i64 {
            return None;
        }
        let sheet = u32::try_from(r.sheet).ok()?;
        let r1 = r.r1.max(0) as u32;
        let c1 = r.c1.max(0) as u32;
        let r2 = r.r2.max(0).min(u32::MAX as i64) as u32;
        let c2 = r.c2.max(0).min(u32::MAX as i64) as u32;
        (r1 <= r2 && c1 <= c2).then_some(Self {
            sheet,
            r1,
            c1,
            r2,
            c2,
        })
    }

    #[inline]
    pub fn to_rect(self) -> Rect {
        Rect {
            sheet: self.sheet as usize,
            r1: self.r1 as i64,
            c1: self.c1 as i64,
            r2: self.r2 as i64,
            c2: self.c2 as i64,
        }
    }

    #[inline]
    fn is_point(self) -> bool {
        self.r1 == self.r2 && self.c1 == self.c2
    }
}

/// Inline storage for 0..=2 compact rectangles to avoid 1 heap Vec allocation per formula.
#[derive(Clone, Debug, Default)]
pub enum FormulaRefs {
    #[default]
    Empty,
    One(CompactRect),
    Two([CompactRect; 2]),
    Many(Box<[CompactRect]>),
}

impl FormulaRefs {
    fn from_compact(v: Vec<CompactRect>) -> Self {
        match v.len() {
            0 => Self::Empty,
            1 => Self::One(v[0]),
            2 => Self::Two([v[0], v[1]]),
            _ => Self::Many(v.into_boxed_slice()),
        }
    }

    #[inline]
    pub fn as_slice(&self) -> &[CompactRect] {
        match self {
            Self::Empty => &[],
            Self::One(r) => std::slice::from_ref(r),
            Self::Two(pair) => pair.as_slice(),
            Self::Many(b) => b,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = Rect> + '_ {
        self.as_slice().iter().copied().map(CompactRect::to_rect)
    }
}

// A multi-cell range reference is indexed once per unique AreaId per column, in the smallest
// aligned row bucket containing both endpoints. A point lookup visits at most seven buckets.
const ROW_SHIFTS: [u32; 7] = [5, 10, 15, 20, 25, 30, 32];

#[inline]
fn bucket(row: u32, shift: u32) -> u32 {
    row.checked_shr(shift).unwrap_or(0)
}

#[inline]
fn row_bucket(first: u32, last: u32) -> (u32, u32) {
    let bits = u32::BITS - (first ^ last).leading_zeros();
    let shift = ROW_SHIFTS[ROW_SHIFTS.partition_point(|&s| s < bits)];
    (shift, bucket(first, shift))
}

#[derive(Clone, Copy)]
struct AreaInterval {
    area_id: u32,
    first: u32,
    last: u32,
}

#[derive(Clone, Default)]
struct SharedArea {
    rect: CompactRect,
    subscribers: Vec<u32>,
}

#[derive(Clone, Default)]
struct PointColumn {
    sorted: Vec<(u32, u32)>,
    unsorted: Vec<(u32, u32)>,
}

impl PointColumn {
    #[inline]
    fn push_unsorted(&mut self, row: u32, formula_id: u32) {
        self.unsorted.push((row, formula_id));
        if self.unsorted.len() >= 64 {
            self.seal();
        }
    }

    fn seal(&mut self) {
        if self.unsorted.is_empty() {
            return;
        }
        self.sorted.append(&mut self.unsorted);
        self.sorted.sort_unstable_by_key(|&(row, id)| (row, id));
        self.sorted.dedup();
    }

    fn query_sorted_rows(&self, rows: &[CellKey], mut emit: impl FnMut(u32) -> bool) -> bool {
        if !self.sorted.is_empty() {
            let mut lo = 0usize;
            for &(_, r, _) in rows {
                lo += self.sorted[lo..].partition_point(|&(pr, _)| pr < r);
                let mut k = lo;
                while k < self.sorted.len() && self.sorted[k].0 == r {
                    if !emit(self.sorted[k].1) {
                        return false;
                    }
                    k += 1;
                }
            }
        }
        if !self.unsorted.is_empty() {
            for &(pr, id) in &self.unsorted {
                if rows.binary_search_by_key(&pr, |k| k.1).is_ok() && !emit(id) {
                    return false;
                }
            }
        }
        true
    }
}

#[derive(Default, Clone)]
pub struct Deps {
    /// 公式单元格 → 引用区域。内联 0~2 个紧凑矩形以避免每条公式一次堆分配。
    pub refs: HashMap<CellKey, FormulaRefs>,
    formula_ids: HashMap<CellKey, u32>,
    slots: Vec<Option<CellKey>>,
    tombstones: usize,
    /// 单格引用列索引：(sheet, col) → 按行有序的 (row, formula_id)
    points_by_col: HashMap<(u32, u32), PointColumn>,
    /// 多格区域共享池：相同矩形仅在列桶登记一次
    area_map: HashMap<CompactRect, u32>,
    areas: Vec<SharedArea>,
    /// 拥有至少一个多格区间的 (sheet, col)，让纯点引用链完全跳过 7 次行桶查询
    range_cols: HashSet<(u32, u32)>,
    /// (表, 列, 行桶位数, 桶号) → AreaInterval
    by_col: HashMap<(u32, u32, u32, u32), Vec<AreaInterval>>,
    wide: HashMap<u32, Vec<u32>>,
    /// 动态数组活跃溢出区与 `#SPILL!` 阻塞监视区：anchor -> (watch_formula_id, watch_rect)
    spill_watches: HashMap<CellKey, (u32, CompactRect)>,
    untracked_spill_errors: HashSet<CellKey>,
    has_spill_op_refs: bool,
    volatile: HashSet<CellKey>,
    volatile_refs: HashSet<CellKey>,
    visibility: HashSet<CellKey>,
    runtime_refs: HashSet<CellKey>,
    structural_sensitive: HashSet<CellKey>,
    indirect_definitions: HashSet<CellKey>,
    computed_baseline: bool,
    pub rect_count: usize,
}

impl Deps {
    pub fn empty() -> Self {
        Deps::default()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
            && self.volatile.is_empty()
            && self.runtime_refs.is_empty()
            && self.spill_watches.is_empty()
            && self.untracked_spill_errors.is_empty()
            && self.visibility.is_empty()
    }

    pub fn requires_full_recalc(&self) -> bool {
        !self.runtime_refs.is_empty() || !self.untracked_spill_errors.is_empty()
    }

    #[inline]
    pub(crate) fn tracks_spill(&self, key: CellKey) -> bool {
        self.spill_watches.contains_key(&key)
    }

    #[inline]
    pub(crate) fn has_spill_op_refs(&self) -> bool {
        self.has_spill_op_refs
    }

    /// A value-to-value edit can share the existing immutable graph with a worker.
    pub fn cell_needs_update(&self, wb: &crate::Workbook, key: CellKey) -> bool {
        self.refs.contains_key(&key)
            || self.structural_sensitive.contains(&key)
            || self.spill_watches.contains_key(&key)
            || self.untracked_spill_errors.contains(&key)
            || self.runtime_refs.contains(&key)
            || self.volatile.contains(&key)
            || self.visibility.contains(&key)
            || wb
                .sheets
                .get(key.0)
                .and_then(|s| s.get(key.1, key.2))
                .is_some_and(|c| c.formula.is_some())
    }

    pub(crate) fn dynamic_cells(&self) -> impl Iterator<Item = CellKey> + '_ {
        self.runtime_refs.iter().chain(&self.volatile_refs).copied()
    }

    /// Recalculate visibility-aware formulas that could read the filtered sheet, plus
    /// volatile formulas. Unresolved references require the full calculation path.
    pub fn visibility_seeds(&self, sheet: usize) -> Option<Vec<CellKey>> {
        if self.requires_full_recalc() {
            return None;
        }
        let mut seeds = self.volatile.clone();
        seeds.extend(self.visibility.iter().copied().filter(|key| {
            self.refs
                .get(key)
                .is_some_and(|refs| refs.as_slice().iter().any(|r| r.sheet as usize == sheet))
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

#[inline]
fn is_position_or_layout_func(name: &str) -> bool {
    matches!(
        name,
        "ROW"
            | "COLUMN"
            | "ROWS"
            | "COLUMNS"
            | "AREAS"
            | "ADDRESS"
            | "CELL"
            | "INFO"
            | "OFFSET"
            | "INDIRECT"
            | "FORMULATEXT"
            | "ISFORMULA"
            | "SUBTOTAL"
            | "AGGREGATE"
            | "SHEET"
            | "SHEETS"
    )
}

#[derive(Default)]
struct Dependencies {
    rects: Vec<Rect>,
    volatile: bool,
    volatile_ref: bool,
    visibility: bool,
    runtime: bool,
    spill_op: bool,
    cached_spill_error: bool,
    structural_sensitive: bool,
    indirect_definition: bool,
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
        self.result.indirect_definition |= expanded.is_some();
        self.result.structural_sensitive |= expanded.is_some();
        if super::compiled::with_node(
            expanded.as_deref().unwrap_or(raw),
            self.row,
            self.col,
            |node| self.walk(node, si),
        )
        .is_err()
        {
            self.result.runtime = true;
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
        if self.name_expansions >= 512
            || self.names.len() >= 32
            || self.names.contains(&(si, index))
        {
            self.result.runtime = true;
            return;
        }
        self.result.indirect_definition = true;
        self.name_expansions += 1;
        self.names.push((si, index));
        self.formula(si, &wb.names[index].2);
        self.names.pop();
    }

    fn walk(&mut self, node: &Node, si: usize) {
        let wb = self.wb;
        // Only position/layout/visibility-observing functions or dynamic indirections
        // can change result when all referenced cell/range inputs remain identical.
        self.result.structural_sensitive |= match node {
            Node::Func(name, _) => {
                is_position_or_layout_func(name.as_str()) || !super::functions::listed_builtin(name)
            }
            Node::Ref(r) => r.book.is_some() || r.sheet2.is_some(),
            Node::Range(a, b) => {
                a.book.is_some() || b.book.is_some() || a.sheet2.is_some() || b.sheet2.is_some()
            }
            Node::Implicit(_) | Node::Spill(_) | Node::Name(_) | Node::ScopedName { .. } => true,
            _ => false,
        };
        match node {
            Node::Ref(re) | Node::Spill(re) => {
                if matches!(node, Node::Spill(_)) {
                    self.result.spill_op = true;
                    let idx = sheet_index(wb, re.sheet.as_deref(), si);
                    let anchor = crate::CellRef::new(re.row.max(0) as u32, re.col.max(0) as u32);
                    if re.book.is_some() || re.sheet2.is_some() {
                        self.result.runtime = true;
                    } else if let Some(&(rows, cols)) =
                        wb.sheets.get(idx).and_then(|s| s.spill_shapes.get(&anchor))
                    {
                        if rows > 0 && cols > 0 {
                            let r1 = re.row.max(0);
                            let c1 = re.col.max(0);
                            self.result.rects.push(Rect {
                                sheet: idx,
                                r1,
                                c1,
                                r2: r1 + i64::from(rows) - 1,
                                c2: c1 + i64::from(cols) - 1,
                            });
                            return;
                        }
                        // Blocked `(0, 0)` anchor: fall through to register the anchor `(idx, r, c)`
                        // point dependency; the anchor itself has a `spill_watch` (or `untracked_spill_errors`).
                    } else {
                        self.result.runtime = true;
                    }
                }
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
            Node::Func(name, args)
                if name == "__CALL__"
                    && matches!(args.first(), Some(Node::ScopedName { book: None, .. })) =>
            {
                let caller = std::mem::take(&mut self.bound);
                self.walk(&args[0], si);
                self.bound = caller;
                for arg in &args[1..] {
                    self.walk(arg, si);
                }
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

pub(super) fn parallel_inputs(wb: &crate::Workbook, key: CellKey, src: &str) -> Option<Vec<Rect>> {
    let info = dependencies(wb, key, src);
    (!info.runtime && !info.volatile && !info.volatile_ref && !info.visibility)
        .then_some(info.rects)
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
        let mut jobs: Vec<(CellKey, &str, bool)> = Vec::new();
        for (si, sheet) in wb.sheets.iter().enumerate() {
            for (cell_ref, cell) in sheet.formula_cells() {
                if let Some(f) = cell.formula.as_deref() {
                    let is_spill_err = super::is_cached_spill_error(&cell.value);
                    jobs.push(((si, cell_ref.row, cell_ref.col), f, is_spill_err));
                }
            }
        }
        let mut blocked_spills: Vec<CellKey> = Vec::new();
        let workers = if jobs.len() >= 4_096 {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(jobs.len().div_ceil(512))
                .min(16)
        } else {
            1
        };
        if workers <= 1 {
            for &(key, f, is_spill_err) in &jobs {
                let mut info = dependencies(wb, key, f);
                info.cached_spill_error = is_spill_err;
                if is_spill_err {
                    blocked_spills.push(key);
                }
                deps.insert_building(key, info);
            }
        } else {
            use std::sync::{
                atomic::{AtomicUsize, Ordering},
                mpsc,
            };
            const CHUNK: usize = 256;
            let next = AtomicUsize::new(0);
            let jobs_slice = jobs.as_slice();
            std::thread::scope(|scope| {
                let (tx, rx) = mpsc::sync_channel(workers * 2);
                for _ in 0..workers {
                    let tx = tx.clone();
                    let next = &next;
                    scope.spawn(move || {
                        loop {
                            let start = next.fetch_add(CHUNK, Ordering::Relaxed);
                            if start >= jobs_slice.len() {
                                break;
                            }
                            let end = (start + CHUNK).min(jobs_slice.len());
                            let mut batch = Vec::with_capacity(end - start);
                            for &(key, f, is_spill_err) in &jobs_slice[start..end] {
                                let mut info = dependencies(wb, key, f);
                                info.cached_spill_error = is_spill_err;
                                batch.push((key, info));
                            }
                            tx.send(batch).unwrap();
                        }
                    });
                }
                drop(tx);
                for batch in rx {
                    for (key, info) in batch {
                        if info.cached_spill_error {
                            blocked_spills.push(key);
                        }
                        deps.insert_building(key, info);
                    }
                }
            });
        }
        deps.index_workbook_spills(wb, &blocked_spills);
        deps.seal_points();
        deps
    }

    /// Use only immediately after a completed whole-workbook calculation.
    /// A graph built from a fast-opened OOXML file does not validate its <v> caches.
    pub fn build_after_recalc(wb: &crate::Workbook) -> Self {
        let mut graph = Self::build(wb);
        graph.computed_baseline = !wb.calc_manual && !wb.calc_pending;
        graph
    }

    /// Build a dependency graph and optionally certify an imported/saved workbook's cached
    /// `<v>` values when all formula cells carry cached values and no volatile formulas exist.
    pub fn build_with_cached_baseline(wb: &crate::Workbook, trust_cached_values: bool) -> Self {
        let mut graph = Self::build(wb);
        if trust_cached_values && !wb.calc_manual && !wb.calc_pending && graph.volatile.is_empty() {
            let all_cached = wb.sheets.iter().all(|sh| {
                sh.formula_cells()
                    .all(|(_, cell)| cell.value != crate::Value::Empty)
            });
            graph.computed_baseline = all_cached;
        }
        graph
    }

    pub fn mark_computed_baseline(&mut self, wb: &crate::Workbook) {
        self.computed_baseline = !wb.calc_manual && !wb.calc_pending;
    }

    fn seal_points(&mut self) {
        for col in self.points_by_col.values_mut() {
            col.seal();
        }
    }

    fn alloc_slot(&mut self, key: CellKey) -> u32 {
        let id = u32::try_from(self.slots.len()).expect("more than 2^32 indexed formulas");
        self.slots.push(Some(key));
        id
    }

    fn register_area(&mut self, rect: CompactRect, formula_id: u32) {
        if let Some(&area_id) = self.area_map.get(&rect) {
            self.areas[area_id as usize].subscribers.push(formula_id);
            return;
        }
        let area_id = u32::try_from(self.areas.len()).expect("more than 2^32 shared areas");
        self.areas.push(SharedArea {
            rect,
            subscribers: vec![formula_id],
        });
        self.area_map.insert(rect, area_id);
        if rect.c2 - rect.c1 > 512 {
            self.wide.entry(rect.sheet).or_default().push(area_id);
        } else {
            let (shift, bin) = row_bucket(rect.r1, rect.r2);
            for c in rect.c1..=rect.c2 {
                self.range_cols.insert((rect.sheet, c));
                self.by_col
                    .entry((rect.sheet, c, shift, bin))
                    .or_default()
                    .push(AreaInterval {
                        area_id,
                        first: rect.r1,
                        last: rect.r2,
                    });
            }
        }
    }

    fn insert_spill_watch(&mut self, key: CellKey, rect: CompactRect) {
        if let Some((old_id, _)) = self.spill_watches.remove(&key) {
            if let Some(slot) = self.slots.get_mut(old_id as usize)
                && slot.take().is_some()
            {
                self.tombstones += 1;
            }
        }
        let watch_id = self.alloc_slot(key);
        self.register_area(rect, watch_id);
        self.spill_watches.insert(key, (watch_id, rect));
    }

    fn index_cell_spill(&mut self, wb: &crate::Workbook, key: CellKey, is_spill_err: bool) {
        self.untracked_spill_errors.remove(&key);
        if let Some((old_id, _)) = self.spill_watches.remove(&key) {
            if let Some(slot) = self.slots.get_mut(old_id as usize)
                && slot.take().is_some()
            {
                self.tombstones += 1;
            }
        }
        let anchor = crate::CellRef::new(key.1, key.2);
        if let Some(&(rows, cols)) = wb
            .sheets
            .get(key.0)
            .and_then(|s| s.spill_shapes.get(&anchor))
            && (rows > 1 || cols > 1)
        {
            if let Some(cr) = CompactRect::from_rect(Rect {
                sheet: key.0,
                r1: i64::from(key.1),
                c1: i64::from(key.2),
                r2: (i64::from(key.1) + i64::from(rows) - 1).min(1_048_575),
                c2: (i64::from(key.2) + i64::from(cols) - 1).min(16_383),
            }) {
                self.insert_spill_watch(key, cr);
            }
            return;
        }
        if !is_spill_err {
            return;
        }
        match super::probe_blocked_spill_shape(wb, key) {
            super::SpillProbe::Footprint(rows, cols) => {
                if let Some(cr) = CompactRect::from_rect(Rect {
                    sheet: key.0,
                    r1: i64::from(key.1),
                    c1: i64::from(key.2),
                    r2: (i64::from(key.1) + i64::from(rows) - 1).min(1_048_575),
                    c2: (i64::from(key.2) + i64::from(cols) - 1).min(16_383),
                }) {
                    self.insert_spill_watch(key, cr);
                }
            }
            super::SpillProbe::NotSpillAnchor => {}
            super::SpillProbe::Unresolved => {
                self.untracked_spill_errors.insert(key);
            }
        }
    }

    fn index_workbook_spills(&mut self, wb: &crate::Workbook, blocked_spills: &[CellKey]) {
        for (si, sheet) in wb.sheets.iter().enumerate() {
            for (&anchor, &(rows, cols)) in &sheet.spill_shapes {
                if rows > 1 || cols > 1 {
                    let key = (si, anchor.row, anchor.col);
                    if let Some(cr) = CompactRect::from_rect(Rect {
                        sheet: si,
                        r1: i64::from(anchor.row),
                        c1: i64::from(anchor.col),
                        r2: (i64::from(anchor.row) + i64::from(rows) - 1).min(1_048_575),
                        c2: (i64::from(anchor.col) + i64::from(cols) - 1).min(16_383),
                    }) {
                        self.insert_spill_watch(key, cr);
                    }
                }
            }
        }
        for &key in blocked_spills {
            self.index_cell_spill(wb, key, true);
        }
    }

    fn insert_building(&mut self, key: CellKey, info: Dependencies) {
        self.insert_inner(key, info, true);
    }

    fn insert(&mut self, key: CellKey, info: Dependencies) {
        self.insert_inner(key, info, false);
    }

    fn insert_inner(&mut self, key: CellKey, info: Dependencies, bulk_building: bool) {
        if info.indirect_definition {
            self.indirect_definitions.insert(key);
        }
        if info.structural_sensitive {
            self.structural_sensitive.insert(key);
        }
        if info.visibility {
            self.visibility.insert(key);
        }
        self.has_spill_op_refs |= info.spill_op;
        if info.runtime {
            self.runtime_refs.insert(key);
        }
        if info.volatile_ref {
            self.volatile_refs.insert(key);
        }
        if info.volatile || info.volatile_ref {
            self.volatile.insert(key);
        }
        let compact: Vec<CompactRect> = info
            .rects
            .into_iter()
            .filter_map(CompactRect::from_rect)
            .collect();
        if compact.is_empty() {
            return;
        }
        let id = self.alloc_slot(key);
        self.formula_ids.insert(key, id);
        for &r in &compact {
            if r.is_point() {
                let col_entry = self.points_by_col.entry((r.sheet, r.c1)).or_default();
                if bulk_building {
                    col_entry.unsorted.push((r.r1, id));
                } else {
                    col_entry.push_unsorted(r.r1, id);
                }
            } else {
                self.register_area(r, id);
            }
        }
        self.rect_count += compact.len();
        self.refs.insert(key, FormulaRefs::from_compact(compact));
    }

    fn maybe_compact_tombstones(&mut self) {
        if self.tombstones < 4_096 || self.tombstones * 2 <= self.slots.len() {
            return;
        }
        let slots = &self.slots;
        for col in self.points_by_col.values_mut() {
            col.sorted
                .retain(|&(_, id)| slots.get(id as usize).is_some_and(Option::is_some));
            col.unsorted
                .retain(|&(_, id)| slots.get(id as usize).is_some_and(Option::is_some));
        }
        self.points_by_col
            .retain(|_, col| !col.sorted.is_empty() || !col.unsorted.is_empty());
        for area in &mut self.areas {
            area.subscribers
                .retain(|&id| slots.get(id as usize).is_some_and(Option::is_some));
        }
        self.tombstones = 0;
    }

    pub fn update_cell(&mut self, wb: &crate::Workbook, key: CellKey) {
        if let Some(old_refs) = self.refs.remove(&key) {
            self.rect_count -= old_refs.len();
        }
        if let Some(old_id) = self.formula_ids.remove(&key)
            && let Some(slot) = self.slots.get_mut(old_id as usize)
            && slot.take().is_some()
        {
            self.tombstones += 1;
        }
        self.indirect_definitions.remove(&key);
        self.structural_sensitive.remove(&key);
        self.runtime_refs.remove(&key);
        self.volatile_refs.remove(&key);
        self.visibility.remove(&key);
        self.volatile.remove(&key);
        self.untracked_spill_errors.remove(&key);
        if let Some((old_watch_id, _)) = self.spill_watches.remove(&key)
            && let Some(slot) = self.slots.get_mut(old_watch_id as usize)
            && slot.take().is_some()
        {
            self.tombstones += 1;
        }
        self.maybe_compact_tombstones();

        if let Some(cell) = wb.sheets.get(key.0).and_then(|s| s.get(key.1, key.2))
            && let Some(f) = cell.formula.as_deref()
        {
            let is_spill_err = super::is_cached_spill_error(&cell.value);
            let mut info = dependencies(wb, key, f);
            info.cached_spill_error = is_spill_err;
            self.insert(key, info);
            self.index_cell_spill(wb, key, is_spill_err);
        }
    }

    pub fn supports_structural(&self, wb: &crate::Workbook) -> bool {
        self.computed_baseline
            && !self.requires_full_recalc()
            && self.spill_watches.is_empty()
            && wb.names.is_empty()
            && !wb.calc_iterate
            && wb.sheets.iter().all(|s| {
                s.tables.is_empty()
                    && s.arrays.is_empty()
                    && s.spill_shapes.is_empty()
                    && s.spilled.is_empty()
            })
    }

    pub fn can_reuse_calculated_sheets(&self, wb: &crate::Workbook) -> bool {
        self.computed_baseline && !wb.calc_iterate && !wb.calc_manual && !wb.calc_pending
    }

    fn structural_sheets(&self, edited: usize, count: usize) -> Vec<bool> {
        let mut readers = vec![HashSet::new(); count];
        for (formula, refs) in &self.refs {
            for r in refs.as_slice() {
                let target = r.sheet as usize;
                if formula.0 != target && target < count {
                    readers[target].insert(formula.0);
                }
            }
        }
        let mut pending = vec![edited];
        pending.extend(
            self.indirect_definitions
                .iter()
                .chain(&self.volatile)
                .chain(&self.runtime_refs)
                .chain(&self.untracked_spill_errors)
                .chain(self.spill_watches.keys())
                .map(|k| k.0),
        );
        let mut selected = vec![false; count];
        while let Some(si) = pending.pop() {
            if si < count && !selected[si] {
                selected[si] = true;
                pending.extend(&readers[si]);
            }
        }
        selected
    }

    pub fn recalc_structural_sheets(
        &self,
        wb: &mut crate::Workbook,
        edited: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Option<Self> {
        if !self.can_reuse_calculated_sheets(wb) {
            return None;
        }
        let selected = self.structural_sheets(edited, wb.sheets.len());
        if !super::recalc_selected(wb, Some(&selected), Some(cancel), None) {
            return None;
        }
        let mut next = Self {
            computed_baseline: true,
            ..Default::default()
        };
        let mut blocked_spills = Vec::new();
        let mut keep = |key: CellKey| {
            if selected[key.0] {
                return;
            }
            let rects: Vec<Rect> = self
                .refs
                .get(&key)
                .map(|r| r.iter().collect())
                .unwrap_or_default();
            let info = Dependencies {
                rects,
                structural_sensitive: self.structural_sensitive.contains(&key),
                visibility: self.visibility.contains(&key),
                indirect_definition: self.indirect_definitions.contains(&key),
                ..Default::default()
            };
            next.insert_building(key, info);
        };
        for &key in self.refs.keys() {
            keep(key);
        }
        for &key in &self.structural_sensitive {
            if !self.refs.contains_key(&key) {
                keep(key);
            }
        }
        for (si, sheet) in wb.sheets.iter().enumerate().filter(|(si, _)| selected[*si]) {
            for (k, cell) in sheet.formula_cells() {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return None;
                }
                let key = (si, k.row, k.col);
                let is_spill_err = super::is_cached_spill_error(&cell.value);
                let mut info = dependencies(wb, key, cell.formula.as_deref()?);
                info.cached_spill_error = is_spill_err;
                if is_spill_err {
                    blocked_spills.push(key);
                }
                next.insert_building(key, info);
            }
        }
        next.index_workbook_spills(wb, &blocked_spills);
        next.seal_points();
        Some(next)
    }

    /// Reindex a broad edit on the worker. A cached scalar remains valid when
    /// every point reference still denotes the very same cell and every multi-cell
    /// range lies on an unedited sheet or strictly before `shift.at`.
    pub fn recalc_structural(
        &self,
        wb: &mut crate::Workbook,
        sheet: usize,
        op: &crate::structural::Structural,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Option<Self> {
        if !self.supports_structural(wb) {
            return None;
        }
        let mut next = Self {
            computed_baseline: true,
            ..Default::default()
        };
        let mut seeds = Vec::new();
        let shift = op.reference_shift();
        let relocate = |(si, r, c): CellKey| {
            if si == sheet {
                crate::sync::shift_structural_coord(r, c, op).map(|(r, c)| (si, r, c))
            } else {
                Some((si, r, c))
            }
        };
        for old in self.refs.keys().chain(
            self.structural_sensitive
                .iter()
                .filter(|k| !self.refs.contains_key(k)),
        ) {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            let Some(key) = relocate(*old) else { continue };
            let old_compact = self.refs.get(old).map(FormulaRefs::as_slice).unwrap_or(&[]);
            if !self.structural_sensitive.contains(old)
                && key == *old
                && !old_compact.iter().any(|r| {
                    r.sheet as usize == sheet
                        && (match shift.axis {
                            crate::refs::Axis::Row => r.r2,
                            crate::refs::Axis::Col => r.c2,
                        }) >= shift.at
                })
            {
                next.insert_building(
                    key,
                    Dependencies {
                        rects: old_compact.iter().copied().map(CompactRect::to_rect).collect(),
                        ..Default::default()
                    },
                );
                continue;
            }
            let cell = wb.sheets[key.0].get(key.1, key.2)?;
            let formula = cell.formula.as_deref()?;
            let mut info = dependencies(wb, key, formula);
            info.cached_spill_error = super::is_cached_spill_error(&cell.value);
            let stable = cell.value != crate::Value::Empty
                && !self.structural_sensitive.contains(old)
                && !info.structural_sensitive
                && !info.runtime
                && info.rects.len() == old_compact.len()
                && old_compact.iter().zip(&info.rects).all(|(r, n)| {
                    if r.is_point() {
                        n.r1 == n.r2
                            && n.c1 == n.c2
                            && relocate((r.sheet as usize, r.r1, r.c1)).is_some_and(
                                |(s, row, col)| {
                                    s == n.sheet && i64::from(row) == n.r1 && i64::from(col) == n.c1
                                },
                            )
                    } else {
                        let unchanged_rect = r.to_rect() == *n;
                        let untouched_by_axis = (r.sheet as usize != sheet)
                            || (match shift.axis {
                                crate::refs::Axis::Row => r.r2,
                                crate::refs::Axis::Col => r.c2,
                            }) < shift.at;
                        unchanged_rect && untouched_by_axis
                    }
                });
            if !stable {
                seeds.push(key);
            }
            next.insert_building(key, info);
        }
        next.seal_points();
        if !super::recalc_dirty_cancelable(wb, &next, &seeds, cancel) {
            return None;
        }
        if !next.supports_structural(wb)
            || wb.sheets.iter().any(|sh| {
                sh.formula_cells()
                    .any(|(_, cell)| super::is_cached_spill_error(&cell.value))
            })
        {
            return Some(Self::build_after_recalc(wb));
        }
        Some(next)
    }

    /// Identify old and new formula slots to reindex after an ordinary row/column
    /// edit. Large changes are left to the caller's background rebuild.
    pub fn structural_updates(
        &self,
        wb: &crate::Workbook,
        sheet: usize,
        op: &crate::structural::Structural,
        limit: usize,
    ) -> Option<Vec<CellKey>> {
        if !self.supports_structural(wb) {
            return None;
        }
        let shift = op.reference_shift();
        let mut slots = HashSet::new();
        for key in self.refs.keys().chain(&self.structural_sensitive) {
            let &(si, row, col) = key;
            let moved = if si == sheet {
                crate::sync::shift_structural_coord(row, col, op).map(|(r, c)| (si, r, c))
            } else {
                Some(*key)
            };
            if moved == Some(*key)
                && !self.structural_sensitive.contains(key)
                && !self.refs.get(key).is_some_and(|refs| {
                    refs.as_slice().iter().any(|r| {
                        r.sheet as usize == sheet
                            && (match shift.axis {
                                crate::refs::Axis::Row => r.r2,
                                crate::refs::Axis::Col => r.c2,
                            }) >= shift.at
                    })
                })
            {
                continue;
            }
            slots.insert(*key);
            slots.extend(moved);
            if slots.len() > limit {
                return None;
            }
        }
        Some(slots.into_iter().collect())
    }

    pub fn closure(&self, seeds: &[CellKey]) -> HashSet<CellKey> {
        self.collect(seeds, false, usize::MAX, None, None).unwrap()
    }

    pub(super) fn closure_excluding_watches(
        &self,
        seeds: &[CellKey],
        anchors: &HashSet<CellKey>,
    ) -> HashSet<CellKey> {
        let excluded: HashSet<u32> = anchors
            .iter()
            .filter_map(|k| self.spill_watches.get(k).map(|(wid, _)| *wid))
            .collect();
        let ex_opt = (!excluded.is_empty()).then_some(&excluded);
        self.collect(seeds, false, usize::MAX, None, ex_opt).unwrap()
    }

    pub fn edit_closure(&self, seeds: &[CellKey]) -> HashSet<CellKey> {
        self.collect(seeds, true, usize::MAX, None, None).unwrap()
    }

    pub fn edit_closure_limited(
        &self,
        seeds: &[CellKey],
        limit: usize,
    ) -> Option<HashSet<CellKey>> {
        self.collect(seeds, true, limit, None, None)
    }

    pub(super) fn edit_closure_cancelable(
        &self,
        seeds: &[CellKey],
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Option<HashSet<CellKey>> {
        self.collect(seeds, true, usize::MAX, cancel, None)
    }

    fn collect(
        &self,
        seeds: &[CellKey],
        edit: bool,
        limit: usize,
        cancel: Option<&std::sync::atomic::AtomicBool>,
        skip_first_wave_ids: Option<&HashSet<u32>>,
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
        let mut triggered_areas: HashSet<u32> = HashSet::new();
        let mut next = Vec::new();
        let mut first_wave = true;
        while !frontier.is_empty() {
            if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                return None;
            }
            frontier.sort_unstable_by_key(|&(s, r, c)| (s, c, r));
            frontier.dedup();
            let skip_now = if first_wave { skip_first_wave_ids } else { None };
            let mut add = |id: u32| -> bool {
                if skip_now.is_some_and(|ex| ex.contains(&id)) {
                    return true;
                }
                let Some(&Some(key)) = self.slots.get(id as usize) else {
                    return true;
                };
                if dirty.insert(key) {
                    next.push(key);
                }
                dirty.len() <= limit
            };
            for column in frontier.chunk_by(|a, b| a.0 == b.0 && a.2 == b.2) {
                let (sheet, _, col) = column[0];
                let Ok(sheet_u32) = u32::try_from(sheet) else {
                    continue;
                };

                // 1. O(log N) sorted point lookup on (sheet, col)
                if let Some(points) = self.points_by_col.get(&(sheet_u32, col))
                    && !points.query_sorted_rows(column, &mut add)
                {
                    return None;
                }

                // 2. Deduplicated multi-cell range intervals (skipped completely if column has none)
                if self.range_cols.contains(&(sheet_u32, col)) {
                    for shift in ROW_SHIFTS {
                        for rows in column.chunk_by(|a, b| bucket(a.1, shift) == bucket(b.1, shift))
                        {
                            let bin = bucket(rows[0].1, shift);
                            if let Some(entries) = self.by_col.get(&(sheet_u32, col, shift, bin)) {
                                for entry in entries {
                                    if triggered_areas.contains(&entry.area_id) {
                                        continue;
                                    }
                                    let p = rows.partition_point(|k| k.1 < entry.first);
                                    if p < rows.len() && rows[p].1 <= entry.last {
                                        triggered_areas.insert(entry.area_id);
                                        for &sub in &self.areas[entry.area_id as usize].subscribers
                                        {
                                            if !add(sub) {
                                                return None;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // 3. Wide ranges (> 512 cols)
                if let Some(area_ids) = self.wide.get(&sheet_u32) {
                    for &area_id in area_ids {
                        if triggered_areas.contains(&area_id) {
                            continue;
                        }
                        let r = self.areas[area_id as usize].rect;
                        if col < r.c1 || col > r.c2 {
                            continue;
                        }
                        let p = column.partition_point(|k| k.1 < r.r1);
                        if p < column.len() && column[p].1 <= r.r2 {
                            triggered_areas.insert(area_id);
                            for &sub in &self.areas[area_id as usize].subscribers {
                                if !add(sub) {
                                    return None;
                                }
                            }
                        }
                    }
                }
            }
            frontier.clear();
            std::mem::swap(&mut frontier, &mut next);
            first_wave = false;
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
    use crate::{
        Cell, Sheet, Value, Workbook,
        structural::{Structural, apply_structural_wb},
    };

    #[test]
    fn incremental_axis_edits_match_full_and_reindex_cross_sheet_readers() {
        for op in [
            Structural::InsertRows { at: 3, n: 1 },
            Structural::DeleteRows { at: 3, n: 1 },
            Structural::InsertCols { at: 3, n: 1 },
            Structural::DeleteCols { at: 3, n: 1 },
        ] {
            let mut wb = Workbook::new_blank();
            wb.sheets.push(Sheet::new("Report"));
            wb.sheets[0].set(
                3,
                3,
                Cell {
                    value: Value::Number(7.0),
                    formula: None,
                },
            );
            for (si, r, c, f) in [
                (0, 0, 0, "=2+2"),
                (0, 4, 4, "=ROW()+COLUMN()"),
                (1, 0, 0, "=Sheet1!D4"),
                (1, 0, 1, "=A1+2"),
                (1, 0, 2, "=SUM(Sheet1!A1:E5)"),
                (1, 0, 3, "=SUM(A10:B10)"),
            ] {
                wb.sheets[si].set(
                    r,
                    c,
                    Cell {
                        value: Value::Empty,
                        formula: Some(f.into()),
                    },
                );
            }
            crate::formula::recalc_all(&mut wb);
            assert!(
                Deps::build(&wb).structural_updates(&wb, 0, &op, 50).is_none(),
                "building edges alone cannot certify cached formula values"
            );
            let mut deps = Deps::build_after_recalc(&wb);
            let old = deps.clone();
            let keys = deps.structural_updates(&wb, 0, &op, 50).unwrap();
            assert!(keys.contains(&(1, 0, 0)));
            assert!(!keys.contains(&(1, 0, 3)));
            apply_structural_wb(&mut wb, 0, &op);
            for &k in &keys {
                deps.update_cell(&wb, k);
            }
            let mut background = wb.clone();
            let broad = old
                .recalc_structural(&mut background, 0, &op, &false.into())
                .unwrap();
            let mut full = wb.clone();
            crate::formula::recalc_all(&mut full);
            crate::formula::recalc_dirty(&mut wb, &deps, &keys);
            for (si, sheet) in full.sheets.iter().enumerate() {
                for (pos, cell) in sheet.iter() {
                    assert_eq!(
                        background.sheets[si].value(pos.row, pos.col),
                        cell.value,
                        "background {op:?} at {pos:?}"
                    );
                    assert_eq!(
                        wb.sheets[si].value(pos.row, pos.col),
                        cell.value,
                        "{op:?} at {pos:?}"
                    );
                }
            }
            let rebuilt = Deps::build(&wb);
            for r in 0..7 {
                for c in 0..7 {
                    assert_eq!(
                        deps.edit_closure(&[(0, r, c)]),
                        rebuilt.edit_closure(&[(0, r, c)])
                    );
                    assert_eq!(
                        broad.edit_closure(&[(0, r, c)]),
                        rebuilt.edit_closure(&[(0, r, c)])
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod structural_lambda_tests {
    use super::*;
    use crate::{
        Sheet, Value, Workbook,
        formula::{self, FValue},
        structural::{Structural, apply_structural_wb},
    };

    #[test]
    fn scoped_axis_recalc_recovers_a_callable_from_another_sheets_cached_display_value() {
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Functions"));
        wb.sheets.push(Sheet::new("Transfer"));
        wb.sheets.push(Sheet::new("Summary"));
        for (si, r, c, src) in [
            (1, 0, 0, "=LAMBDA(n,n*3)"),
            (2, 0, 0, "=Functions!A1"),
            (0, 2, 3, "=MAP(A3:B3,Transfer!A1)"),
            (3, 0, 0, "=SUM(Sheet1!D3#)"),
        ] {
            formula::set_formula(wb.sheet_mut(si), r, c, src, FValue::Empty);
        }
        for (col, value) in [(0, 5.), (1, 7.)] {
            wb.sheet_mut(0).set(
                2,
                col,
                crate::Cell {
                    value: Value::Number(value),
                    formula: None,
                },
            );
        }
        formula::recalc_all_force(&mut wb);
        assert_eq!(wb.sheet(3).value(0, 0), Value::Number(36.));
        let graph = Deps::build_after_recalc(&wb);
        let op = Structural::InsertRows { at: 1, n: 1 };
        apply_structural_wb(&mut wb, 0, &op);
        let mut full = wb.clone();
        formula::recalc_all_force(&mut full);
        graph
            .recalc_structural_sheets(&mut wb, 0, &false.into())
            .unwrap();
        assert_eq!(wb.sheet(3).value(0, 0), full.sheet(3).value(0, 0));
        assert_eq!(wb.sheet(0).value(3, 4), Value::Number(21.));
    }

    #[test]
    fn dirty_levels_preserve_display_text_and_resolve_external_callable_chains() {
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Functions"));
        formula::set_formula(wb.sheet_mut(1), 0, 0, "=LAMBDA(n,n*3)", FValue::Empty);
        formula::set_formula(wb.sheet_mut(1), 1, 0, "=A1", FValue::Empty);
        formula::set_formula(wb.sheet_mut(1), 2, 0, "=7", FValue::Empty);
        let mut seeds = Vec::new();
        for r in 0..2050 {
            wb.sheet_mut(0).set(
                r,
                0,
                crate::Cell {
                    value: Value::Number(1.),
                    formula: None,
                },
            );
            let n = r + 1;
            formula::set_formula(
                wb.sheet_mut(0),
                r,
                1,
                &format!("=SUM(MAP(A{n}:A{n},Functions!$A$2))+LEN(Functions!$A$3)"),
                FValue::Empty,
            );
            seeds.push((0, r, 0));
        }
        formula::recalc_all_force(&mut wb);
        // A genuine text cache using the closure display marker cannot be replaced
        // by evaluating its formula (this cell is outside the edit closure).
        wb.sheet_mut(1)
            .set_formula_cache(2, 0, Value::Text("#CALC!".into()));
        let deps = Deps::build(&wb);
        for &(s, r, c) in &seeds {
            wb.sheet_mut(s).set(
                r,
                c,
                crate::Cell {
                    value: Value::Number(5.),
                    formula: None,
                },
            );
        }
        formula::recalc_dirty(&mut wb, &deps, &seeds);
        for r in [0, 130, 2049] {
            assert_eq!(wb.sheet(0).value(r, 1), Value::Number(21.));
        }
        assert_eq!(wb.sheet(1).value(2, 0), Value::Text("#CALC!".into()));

        // Imported display markers may be stale. Cyclic forwarded candidates
        // must terminate and keep their previous scalar cache.
        for (r, f) in [(0, "=A2"), (1, "=A1")] {
            formula::set_formula(wb.sheet_mut(1), r, 0, f, FValue::Text("#CALC!".into()));
        }
        formula::set_formula(wb.sheet_mut(0), 0, 3, "=LEN(Functions!A1)", FValue::Empty);
        let keys = HashSet::from([(0, 0, 3)]);
        let mut calc = formula::Calc::new(&wb);
        calc.scope = Some(&keys);
        assert_eq!(calc.cell_value(0, 0, 3), FValue::Number(6.));
    }
}
