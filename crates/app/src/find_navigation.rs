use super::*;
use sheetcore::search::{self, Hit, SearchOpts, SearchOrder};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

type Rect = (u32, u32, u32, u32);

#[derive(Clone, PartialEq)]
struct Query {
    needle: String,
    options: SearchOpts,
    order: SearchOrder,
    sheet: Option<usize>,
    rects: Vec<Rect>,
    gate: crate::bg_gate::CoordGate,
    revisions: Vec<(u64, sheetcore::store::CellVersion, u64, u64)>,
    dates: sheetcore::model::DateSystem,
}

impl Query {
    fn same_scope(&self, other: &Self) -> bool {
        self.gate.doc_uid == other.gate.doc_uid
            && self.sheet == other.sheet
            && self.revisions.iter().map(|v| v.0).eq(other.revisions.iter().map(|v| v.0))
    }

    fn same_request(&self, other: &Self) -> bool {
        self.same_scope(other)
            && self.gate == other.gate
            && self.needle == other.needle
            && self.options == other.options
            && self.order == other.order
            && self.rects == other.rects
            && self.dates == other.dates
            && self.revisions.iter().map(|v| v.3).eq(other.revisions.iter().map(|v| v.3))
    }


    fn all(&self, wb: &Workbook, cancel: &AtomicBool) -> Vec<Hit> {
        search::scoped_hits(
            wb,
            self.sheet,
            &self.rects,
            &self.needle,
            &self.options,
            self.order,
            cancel,
        )
    }

    /// Test at most one short, contiguous stretch in search order. If it has
    /// no result, the sparse index handles distant cells, other sheets and wrapping.
    fn nearby(&self, wb: &Workbook, start: Hit, backwards: bool) -> Option<Hit> {
        let bounds = wb.sheet(start.sheet).used_bounds()?;
        let column_order = self.order == SearchOrder::ColumnMajor;
        let (low, high, first, last, mut major, mut minor) = if column_order {
            (bounds.1, bounds.3, bounds.0, bounds.2, start.col, start.row)
        } else {
            (bounds.0, bounds.2, bounds.1, bounds.3, start.row, start.col)
        };
        if !(low..=high).contains(&major) || !(first..=last).contains(&minor) {
            return None;
        }
        for _ in 0..512 {
            if backwards {
                if minor > first {
                    minor -= 1;
                } else if major > low {
                    major -= 1;
                    minor = last;
                } else {
                    return None;
                }
            } else if minor < last {
                minor += 1;
            } else if major < high {
                major += 1;
                minor = first;
            } else {
                return None;
            }
            let (row, col) = if column_order {
                (minor, major)
            } else {
                (major, minor)
            };
            if !self.rects.is_empty()
                && !self
                    .rects
                    .iter()
                    .any(|&(r1, c1, r2, c2)| r1 <= row && row <= r2 && c1 <= col && col <= c2)
            {
                continue;
            }
            if search::cell_matches(wb, start.sheet, row, col, &self.needle, &self.options) {
                return Some(Hit {
                    sheet: start.sheet,
                    row,
                    col,
                });
            }
        }
        None
    }
}

struct Pending {
    cursor: Hit,
    selection_end: (u32, u32),
    extra_selection: Vec<Rect>,
    status: String,
    /// Directions pressed before a distant first hit was known.
    steps: Vec<bool>,
}

pub(super) struct Navigation {
    query: Arc<Query>,
    gate: crate::bg_gate::CoordGate,
    hits: Arc<Vec<Hit>>,
    snapshot: Arc<Workbook>,
    verified: std::cell::RefCell<Option<Query>>,
    summary: Option<bool>,
    receiver: Option<mpsc::Receiver<Vec<Hit>>>,
    cancel: Arc<AtomicBool>,
    pending: Option<Pending>,
}

pub(super) struct Replacement {
    query: Arc<Query>,
    gate: crate::bg_gate::CoordGate,
    snapshot: Arc<Workbook>,
    text: String,
    replace_formula_results: bool,
    worker: Option<mpsc::Receiver<ReplacementPlan>>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for Replacement {
    fn drop(&mut self) { self.cancelled.store(true, Ordering::Relaxed); }
}

pub(super) fn replacement(wb: &Workbook, si: usize, r: u32, c: u32, cell: &Cell, needle: &str,
    opts: &SearchOpts, replace: &str, replace_results: bool) -> Option<Cell> {
    if !opts.in_formula && cell.formula.is_some() && !replace_results { return None; }
    let text = search::cell_text_at(wb, si, r, c, cell, opts.in_formula)?;
    let next = if opts.whole_cell {
        let equal = if opts.match_case { text == needle } else { text.to_lowercase() == needle.to_lowercase() };
        if !equal { return None; }
        replace.to_owned()
    } else { search::replace_in_text(&text, needle, replace, opts.match_case) };
    if text == next { return None; }
    if opts.in_formula {
        Some(Cell { formula: Some(next.into()), value: cell.value.clone() })
    } else {
        Some(Cell { value: Value::parse_input(&next), formula: None })
    }
}

struct ReplacementPlan {
    before: Arc<Workbook>,
    sheets: std::collections::BTreeMap<usize, (sheetcore::Sheet, Vec<(u32,u32)>)>,
    history: Option<sheetcore::undo::PreparedAction>,
    sample: Vec<sheetcore::undo::CellDelta>,
    hits: usize,
    changes: usize,
    conflicts: usize,
    formulas: usize,
    formula_edit: bool,
}

impl ReplacementPlan {
    fn build(baseline: &Workbook, before: Arc<Workbook>, hits: &[Hit], needle: &str,
        opts: &SearchOpts, replace: &str, replace_results: bool, cancelled: &AtomicBool) -> Option<Self> {
        let mut result = Self { before, sheets: Default::default(), history: None, sample: Vec::new(),
            hits: hits.len(), changes: 0, conflicts: 0, formulas: 0, formula_edit: false };
        let mut deltas = Vec::new();
        for (n, &Hit { sheet: si, row, col }) in hits.iter().enumerate() {
            if n % 128 == 0 && cancelled.load(Ordering::Relaxed) { return None; }
            let live = result.before.sheet(si);
            let prior = baseline.sheet(si);
            let Some(cell) = live.get(row, col) else { result.conflicts += 1; continue };
            let old = prior.get(row,col);
            let conflict = if opts.in_formula { old.as_deref().and_then(|c| c.formula.as_ref()) != cell.formula.as_ref() }
                else { old.as_deref() != Some(cell.as_ref()) || baseline.styles.get(prior.style_of(row,col)).format
                    != result.before.styles.get(live.style_of(row,col)).format };
            if conflict { result.conflicts += 1; continue; }
            if !opts.in_formula && !replace_results && cell.formula.is_some() { result.formulas += 1; continue; }
            let Some(after) = replacement(&result.before,si,row,col,cell.as_ref(),needle,opts,replace,replace_results) else { continue };
            result.formula_edit |= cell.formula != after.formula;
            let (prepared, keys) = result.sheets.entry(si).or_insert_with(||
                (if hits.len() > 1024 { live.search_snapshot() } else { sheetcore::Sheet::default() }, Vec::new()));
            prepared.set(row,col,after.clone());
            keys.push((row,col));
            let style = live.style_of(row,col);
            deltas.push(sheetcore::undo::CellDelta { sheet: si, row, col, before: cell.into_owned(), after,
                before_style: style, after_style: style });
        }
        for (_, keys) in result.sheets.values_mut() {
            if !keys.is_sorted() { keys.sort_unstable(); }
        }
        if !deltas.is_sorted_by_key(|d| (d.sheet,d.row,d.col)) { deltas.sort_unstable_by_key(|d| (d.sheet,d.row,d.col)); }
        if let Some(first) = deltas.first() {
            result.changes = deltas.len();
            result.sample = deltas.iter().take(sheetcore::revision::MAX_CELLS).cloned().collect();
            result.history = Some(sheetcore::undo::Action { label: "全部替换".into(), focus: (first.sheet,first.row,first.col),
                deltas, merges: None, pictures: None, shapes: None, snap_before: None, snap_after: None,
                refresh_sizes: None, by_site: 0 }.prepare());
        }
        Some(result)
    }

    fn status(&self) -> String {
        if self.hits == 0 { return "没找到可替换的内容".into(); }
        let mut status = format!("找到 {} 处，替换 {} 处", self.hits, self.changes);
        if self.formulas != 0 { status.push_str(&format!("，跳过 {} 处公式结果（可勾选“替换公式结果”）", self.formulas)); }
        if self.conflicts != 0 { status.push_str(&format!("；跳过扫描后已更改的 {} 处", self.conflicts)); }
        if self.formulas == 0 && self.conflicts == 0 {
            if self.changes == self.hits { status.push_str("（全部成功）"); }
            else { status.push_str("（其余替换后没有变化）"); }
        }
        status
    }
}

impl Navigation {
    fn current(&self, now: &Query, wb: &Workbook) -> bool {
        if self.gate.is_stale(now.gate.doc_uid, wb) { return false; }
        if *self.query == *now || self.verified.borrow().as_ref() == Some(now) { return true; }
        if !self.query.same_request(now) { return false; }
        let matches = self.query.revisions.iter().zip(&now.revisions).enumerate().all(|(n, (a,b))| {
            let i = self.query.sheet.unwrap_or(n);
            let live = &wb.sheets[i];
            let prior = &self.snapshot.sheets[i];
            (a.2 == b.2 || live.same_display_styles_in(prior, &now.rects, &wb.styles, &self.snapshot.styles))
                && (a.1 == b.1 || live.same_cells_in(prior, &now.rects, now.options.in_formula))
        });
        if matches { *self.verified.borrow_mut() = Some(now.clone()); }
        matches
    }
}

impl Drop for Navigation {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl SheetApp {
    pub(super) fn start_replace_scan(&mut self) {
        self.find_scan = None;
        if self.find.find.is_empty() { return; }
        self.ensure_navigation(true);
        let Some(nav) = self.find_navigation.as_ref() else { return };
        // A reused index can have identical displayed hits after pre-command
        // edits (e.g. a formula replaced by the same literal value). Guard the
        // transaction with the actual input when the user requested Replace All.
        let snapshot = if nav.query.revisions == self.navigation_query().revisions {
            nav.snapshot.clone()
        } else {
            self.search_workbook(&nav.query)
        };
        let nav = self.find_navigation.as_mut().unwrap();
        nav.pending = None;
        nav.summary = None;
        self.find_scan = Some(Replacement {
            snapshot,
            gate: nav.gate,
            query: Arc::clone(&nav.query),
            text: self.find.replace.clone(),
            replace_formula_results: self.find.replace_formula_results,
            worker: None, cancelled: Arc::new(AtomicBool::new(false)),
        });
        if nav.receiver.is_some() {
            self.status = "正在查找可替换的单元格，可点“取消”…".into();
        } else {
            self.poll_find_scan();
        }
    }

    pub(super) fn enqueue_or_send_replace_broadcast(
        &mut self,
        sheets: &std::collections::BTreeMap<usize, (sheetcore::Sheet, Vec<(u32, u32)>)>,
    ) {
        while self.drain_replace_broadcast() {}
        if self.peer.is_none() || self.net.is_none() {
            return;
        }
        for (&si, (_, keys)) in sheets {
            if !keys.is_empty() {
                self.normal_page_breaks
                    .replace_broadcast
                    .push_back((si, keys.clone(), 0));
            }
        }
        let _ = self.drain_replace_broadcast();
    }

    /// 按每帧 8,192 格预算流式生成并发送超大批量「全部替换」的协同 CRDT Op；仍有剩余分片时返回 `true`。
    pub(super) fn drain_replace_broadcast(&mut self) -> bool {
        if self.normal_page_breaks.replace_broadcast.is_empty() {
            return false;
        }
        let (Some(peer), Some(net)) = (self.peer.as_mut(), self.net.as_ref()) else {
            self.normal_page_breaks.replace_broadcast.clear();
            return false;
        };
        const FRAME_BUDGET: usize = 16 * sheetcore::sync::LIVE_OP_BATCH; // 8,192 格/帧（< 1.5ms）
        let blank = sheetcore::Cell::default();
        let mut sent = 0usize;
        while sent < FRAME_BUDGET {
            let Some((si, keys, cursor)) = self.normal_page_breaks.replace_broadcast.front_mut() else {
                break;
            };
            let si = *si;
            let Some(sh) = self.wb.sheets.get(si) else {
                self.normal_page_breaks.replace_broadcast.pop_front();
                continue;
            };
            let end = (*cursor + sheetcore::sync::LIVE_OP_BATCH).min(keys.len());
            if *cursor < end {
                let ops: Vec<_> = keys[*cursor..end]
                    .iter()
                    .map(|&(r, c)| {
                        let cell = sh.get(r, c);
                        peer.local_set(si, r, c, cell.as_deref().unwrap_or(&blank))
                    })
                    .collect();
                sent += ops.len();
                *cursor = end;
                net.broadcast_ops(ops);
            }
            if *cursor >= keys.len() {
                self.normal_page_breaks.replace_broadcast.pop_front();
            }
        }
        !self.normal_page_breaks.replace_broadcast.is_empty()
    }

    pub(super) fn poll_find_scan(&mut self) {
        let _ = self.drain_replace_broadcast();
        let Some(scan) = &self.find_scan else { return };
        let current = self.navigation_query();
        if !scan.query.same_scope(&current) {
            self.cancel_find_scan();
            self.status = "文档已切换，已取消扫描".into();
            return;
        }
        if scan.gate.is_stale(self.doc_uid, &self.wb)
            || !scan.query.same_request(&current)
            || scan.text != self.find.replace
            || scan.replace_formula_results != self.find.replace_formula_results
        {
            self.cancel_find_scan();
            self.status = "工作簿或替换条件已更改，已取消替换".into();
            return;
        }
        self.finish_navigation_index();
        let Some(nav) = &self.find_navigation else { self.cancel_find_scan(); return };
        if !Arc::ptr_eq(&nav.query, &self.find_scan.as_ref().unwrap().query) {
            self.cancel_find_scan();
            return;
        }
        if nav.receiver.is_some() { return; }
        let hits = nav.hits.clone();
        let scan = self.find_scan.as_ref().unwrap();
        let large = hits.len() > 1024;
        let plan = if let Some(rx) = &scan.worker {
            match rx.try_recv() {
                Ok(plan) => plan,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.cancel_find_scan(); self.status = "替换准备未完成".into(); return;
                }
            }
        } else {
            let baseline = scan.snapshot.clone();
            let live = if nav.query.revisions == current.revisions { nav.snapshot.clone() }
                else { self.search_workbook(&scan.query) };
            let needle = self.find.find.clone();
            let opts = scan.query.options.clone();
            let text = scan.text.clone();
            let replace_results = scan.replace_formula_results;
            let cancelled = scan.cancelled.clone();
            let prepare = move || ReplacementPlan::build(&baseline, live, &hits, &needle, &opts,
                &text, replace_results, &cancelled);
            if large {
                // A cancelled result is freed by the sender even if it is already ready.
                let (tx, rx) = mpsc::sync_channel(0);
                self.find_scan.as_mut().unwrap().worker = Some(rx);
                self.status = "正在准备全部替换，可点“取消”…".into();
                std::thread::spawn(move || { if let Some(plan) = prepare() { let _ = tx.send(plan); } });
                return;
            }
            prepare().unwrap()
        };
        let mut plan = plan;
        // Validate all sheets before publishing any. Preserve edits to other cells,
        // including those in the same COW slab, and honour late protection/leases.
        let mut conflicts = Vec::new();
        for (&si, (prepared, keys)) in &mut plan.sheets {
            if large {
                let live = self.wb.sheet(si);
                let Some(skip) = live.rebase_replacement(plan.before.sheet(si), prepared, keys,
                    !self.find.in_formula, &self.wb.styles, &plan.before.styles) else {
                    self.find_scan.as_mut().unwrap().worker = None;
                    std::thread::spawn(move || drop(plan));
                    return;
                };
                if !skip.is_empty() {
                    let remove: Vec<_> = skip.iter().map(|k| keys.binary_search(k).unwrap()).collect();
                    for i in remove.into_iter().rev() { keys.swap_remove(i); }
                    conflicts.extend(skip.into_iter().map(|(r,c)| (si,r,c)));
                }
            }
            if !self.write_preflight_on(si, keys, "全部替换") {
                self.find_scan = None;
                std::thread::spawn(move || drop(plan));
                return;
            }
        }
        if !conflicts.is_empty() {
            let deltas = plan.history.as_mut().unwrap().skip_sorted_cells(&conflicts);
            plan.changes = deltas.len();
            plan.sample = deltas.iter().take(sheetcore::revision::MAX_CELLS).cloned().collect();
            plan.conflicts += conflicts.len();
            if plan.changes == 0 { plan.history = None; }
            plan.sheets.retain(|_,(_,keys)| !keys.is_empty());
        }
        self.find_scan = None;
        for (&si, (prepared, keys)) in &mut plan.sheets {
            if large { self.wb.sheet_mut(si).take_row_changes(prepared); }
            else {
                let sh = self.wb.sheet_mut(si);
                for &(r,c) in keys.iter() { sh.set(r, c, prepared.get(r,c).map(|c| c.into_owned()).unwrap_or_default()); }
            }
        }
        let status = plan.status();
        if let Some(history) = plan.history.take() {
            self.record_revision_sample("全部替换", &plan.sample, plan.changes);
            self.undo.push_prepared(history);
            if large {
                if self.net.is_some() {
                    self.enqueue_or_send_replace_broadcast(&plan.sheets);
                }
                if plan.formula_edit || !self.deps_built || !self.deps.is_empty()
                    || self.wb.sheets.iter().any(|s| s.has_formulas() || !s.arrays.is_empty() || !s.spilled.is_empty()) {
                    self.wb.calc_manual |= self.manual_calc;
                    self.recalc_all_and_deps_adaptive();
                }
            } else {
                let keys = plan.sheets.iter().flat_map(|(&si, (_, keys))| keys.iter().map(move |&(r,c)| (si,r,c))).collect::<Vec<_>>();
                self.recalc_and_broadcast(&keys);
            }
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.tasks.last_edit = Some(std::time::Instant::now());
        }
        self.status = status;
        if large { std::thread::spawn(move || drop(plan)); }
    }

    fn find_cursor(&self) -> Hit {
        Hit {
            sheet: self.sheet_idx,
            row: self.sel.0,
            col: self.sel.1,
        }
    }

    fn navigation_query(&self) -> Query {
        let options = self.search_opts();
        let sheet = (self.find.scope != 1).then_some(self.sheet_idx);
        let rects = self
            .find_rects()
            .into_iter()
            .map(|(r1, c1, r2, c2)| (r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
            .collect();
        let gate = match sheet {
            Some(si) => crate::bg_gate::CoordGate::sheet(self.doc_uid, &self.wb, si)
                .unwrap_or_else(|| crate::bg_gate::CoordGate::workbook(self.doc_uid, &self.wb)),
            None => crate::bg_gate::CoordGate::workbook(self.doc_uid, &self.wb),
        };
        Query {
            needle: search::normalize_needle(&self.find.find, &options),
            order: self.search_order(),
            options,
            sheet,
            rects,
            gate,
            revisions: self
                .wb
                .sheets
                .iter()
                .enumerate()
                .filter(|(i, _)| sheet.is_none_or(|si| si == *i))
                .map(|(_, sh)| (sh.identity, sh.cell_version().clone(), if self.find.in_formula { 0 } else { sh.display_style_revision() }, sh.coordinate_revision()))
                .collect(),
            dates: if self.find.in_formula { Default::default() } else { self.wb.date_system },
        }
    }

    fn navigate_to_hit(
        &mut self,
        hit: Hit,
        rank: Option<(usize, usize)>,
        backwards: bool,
        wrapped: bool,
    ) {
        if hit.sheet != self.sheet_idx && !self.switch_sheet_view(hit.sheet) {
            return;
        }
        self.sel = (hit.row, hit.col);
        self.sel_end = self.sel;
        self.extra_sel.clear();
        self.nav_rect = None;
        self.ensure_visible();
        self.find_location_status(hit, rank, backwards, wrapped);
    }

    fn find_location_status(
        &mut self,
        hit: Hit,
        rank: Option<(usize, usize)>,
        backwards: bool,
        wrapped: bool,
    ) {
        let where_ = &self.wb.sheet(hit.sheet).name;
        let name = cell_name(hit.row, hit.col);
        let edge = if !wrapped {
            ""
        } else if backwards {
            "，已到最前，从末尾继续"
        } else {
            "，已回到开头"
        };
        self.status = match rank {
            Some((i, total)) => format!("找到 {where_}!{name}（第 {}/{total} 处{edge}）", i + 1),
            None => format!("找到 {where_}!{name}（正在统计总数）"),
        };
    }

    fn search_workbook(&self, query: &Query) -> Arc<Workbook> {
        let mut snapshot = Workbook::new_blank();
        snapshot.date_system = self.wb.date_system;
        if !query.options.in_formula {
            snapshot.styles = self.wb.styles.clone();
        }
        snapshot.sheets = self.wb.sheets.iter().enumerate().map(|(i, sh)| {
            if query.sheet.is_none_or(|si| si == i) { sh.search_snapshot() }
            else { sheetcore::Sheet::default() }
        }).collect();
        Arc::new(snapshot)
    }

    fn ensure_navigation(&mut self, lock_rects: bool) {
        if lock_rects && self.find.scope == 2 && self.find.session_rect.is_none() {
            self.lock_find_rects();
        }
        let query = self.navigation_query();
        if self
            .find_navigation
            .as_ref()
            .is_none_or(|n| !n.current(&query, &self.wb))
        {
            self.find_navigation = None;
            self.find.hits = 0;
            let count: usize = self
                .wb
                .sheets
                .iter()
                .enumerate()
                .filter(|(i, _)| query.sheet.is_none_or(|si| si == *i))
                .map(|(_, sh)| sh.len())
                .sum();
            let gate = query.gate;
            let query = Arc::new(query);
            let snapshot = self.search_workbook(&query);
            let mut nav = Navigation {
                query: query.clone(),
                gate,
                hits: Arc::default(),
                snapshot: snapshot.clone(),
                verified: Default::default(),
                summary: None,
                receiver: None,
                cancel: Arc::new(AtomicBool::new(false)),
                pending: None,
            };
            if count <= 4096 {
                nav.hits = Arc::new(query.all(&self.wb, &nav.cancel));
            } else {
                let (tx, rx) = mpsc::channel();
                let cancel = nav.cancel.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(query.all(&snapshot, &cancel));
                });
                nav.receiver = Some(rx);
            }
            self.find_navigation = Some(nav);
        }
        self.finish_navigation_index();
    }

    pub(super) fn find_summary(&mut self, list: bool) {
        if list {
            self.find_scan = None;
        }
        if self.find.find.is_empty() {
            if !list {
                self.find.hits = 0;
                self.status = "命中 0 处".into();
            }
            return;
        }
        if let Some(nav) = self.find_navigation.as_mut() {
            nav.pending = None;
        }
        self.ensure_navigation(list);
        let Some(nav) = self.find_navigation.as_mut() else {
            return;
        };
        nav.pending = None;
        if nav.receiver.is_some() {
            nav.summary = Some(list);
            self.status = "正在统计查找结果…".into();
        } else {
            self.show_find_summary(list);
        }
    }

    fn show_find_summary(&mut self, list: bool) {
        let Some(nav) = self.find_navigation.as_ref() else {
            return;
        };
        self.find.hits = nav.hits.len();
        if list {
            self.find.find_all = nav.hits.clone();
            self.find.find_all_open = true;
            self.status = format!("查找全部：{} 处", self.find.hits);
        } else {
            let scope = match self.find.scope {
                1 => "整个工作簿",
                2 => "当前选区",
                _ => "当前工作表",
            };
            let mode = if self.find.in_formula {
                "公式"
            } else {
                "值"
            };
            let order = if self.find.order_column {
                "按列"
            } else {
                "按行"
            };
            self.status = format!("{scope}（{mode}/{order}）命中 {} 处", self.find.hits);
        }
    }

    pub(super) fn indexing_find_summary(&self) -> bool {
        self.find_navigation
            .as_ref()
            .is_some_and(|nav| nav.summary.is_some())
    }

    pub(super) fn find_next(&mut self, backwards: bool) {
        if self.find.find.is_empty() {
            return;
        }
        self.ensure_navigation(true);
        let cur = self.find_cursor();
        let Some(nav) = self.find_navigation.as_mut() else {
            return;
        };
        nav.summary = None;
        if nav.receiver.is_none() {
            self.find.hits = nav.hits.len();
            if let Some((i, wrap)) = search::next_index(&nav.hits, cur, !backwards, nav.query.order)
            {
                let (hit, total) = (nav.hits[i], nav.hits.len());
                self.navigate_to_hit(hit, Some((i, total)), backwards, wrap);
            } else {
                self.status = "没找到".into();
            }
        } else {
            if let Some(p) = nav.pending.as_mut() {
                if !p.steps.is_empty()
                    && p.cursor == cur
                    && p.selection_end == self.sel_end
                    && p.extra_selection == self.extra_sel
                {
                    p.steps.push(backwards);
                    return;
                }
            }
            let nearby = nav.query.nearby(&self.wb, cur, backwards);
            let mut steps = Vec::new();
            if let Some(hit) = nearby {
                self.navigate_to_hit(hit, None, backwards, false);
            } else {
                self.status = "正在查找…".into();
                steps.push(backwards);
            }
            let pending = Pending {
                cursor: self.find_cursor(),
                selection_end: self.sel_end,
                extra_selection: self.extra_sel.clone(),
                status: self.status.clone(),
                steps,
            };
            self.find_navigation.as_mut().unwrap().pending = Some(pending);
        }
    }

    fn accepts_pending_index(&self, nav: &Navigation) -> bool {
        if nav.gate.is_stale(self.doc_uid, &self.wb) {
            return false;
        }
        let current = self.navigation_query();
        if self.find_scan.as_ref().is_some_and(|r| Arc::ptr_eq(&r.query, &nav.query)) {
            return nav.query.same_request(&current);
        }
        nav.current(&current, &self.wb)
    }

    pub(super) fn poll_navigation(&mut self, ctx: &egui::Context) {
        if self
            .find_navigation
            .as_ref()
            .is_some_and(|nav| nav.receiver.is_some() && !self.accepts_pending_index(nav))
        {
            self.find_navigation = None;
            return;
        }
        self.finish_navigation_index();
        if self.find_scan.is_some() { ctx.request_repaint_after(std::time::Duration::from_millis(16)); }
        if self
            .find_navigation
            .as_ref()
            .is_some_and(|n| n.receiver.is_some())
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn finish_navigation_index(&mut self) {
        let Some(rx) = self
            .find_navigation
            .as_ref()
            .and_then(|nav| nav.receiver.as_ref())
        else {
            return;
        };
        let hits = match rx.try_recv() {
            Ok(hits) => hits,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.find_navigation = None;
                return;
            }
        };
        if !self.accepts_pending_index(self.find_navigation.as_ref().unwrap()) {
            self.find_navigation = None;
            return;
        }
        let cur = self.find_cursor();
        let nav = self.find_navigation.as_mut().unwrap();
        nav.receiver = None;
        nav.hits = Arc::new(hits);
        self.find.hits = nav.hits.len();
        if let Some(list) = nav.summary.take() {
            self.show_find_summary(list);
            return;
        }
        let Some(p) = nav.pending.take() else { return };
        if p.cursor != cur || p.selection_end != self.sel_end || p.extra_selection != self.extra_sel
        {
            return;
        }
        if nav.hits.is_empty() {
            if p.status == self.status {
                self.status = "没找到".into();
            }
            return;
        }
        let order = nav.query.order;
        let mut hit = cur;
        let mut rank = search::position(&nav.hits, hit, order);
        if p.steps.is_empty() {
            if let Some(i) = rank.filter(|_| p.status == self.status) {
                let total = nav.hits.len();
                self.find_location_status(hit, Some((i, total)), false, false);
            }
            return;
        }
        let mut wrap = false;
        let mut backwards = false;
        for direction in p.steps {
            let (i, wrapped) = search::next_index(&nav.hits, hit, !direction, order).unwrap();
            hit = nav.hits[i];
            rank = Some(i);
            wrap = wrapped;
            backwards = direction;
        }
        if let Some(i) = rank {
            let total = nav.hits.len();
            let status = (self.status != p.status).then(|| self.status.clone());
            self.navigate_to_hit(hit, Some((i, total)), backwards, wrap);
            if let Some(status) = status {
                self.status = status;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_search_reuses_unaffected_hits_and_uses_command_time_replacement_baseline() {
        let _env = crate::test_env::TestEnv::new("selection-find-inputs");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.wb.sheets.push(sheetcore::Sheet::new("Other"));
        let value = |f| Cell { value: Value::Number(0.5), formula: f };
        app.wb.sheets[0].set(5,0,value(Some("=1/2".into())));
        app.sel = (1,0);
        app.sel_end = (7,0);
        app.find.scope = 2;
        app.find.find = "0.5".into();
        app.ensure_navigation(true);
        let original = app.find_navigation.as_ref().unwrap().query.clone();
        let mut percentage = app.wb.styles.get(0).clone();
        percentage.format = "0%".into();
        let percent = app.wb.styles.intern(percentage);
        app.wb.sheets[0].set_style(20,0,percent);
        app.wb.sheets[0].set(8,0,text("0.5"));
        app.wb.sheets[1].set(5,0,text("0.5"));
        app.doc_gen += 1;
        app.wb_epoch += 1;
        app.wb.sheets[0].set(5,0,value(None)); // same displayed value before Replace All
        app.ensure_navigation(true);
        assert!(Arc::ptr_eq(&original, &app.find_navigation.as_ref().unwrap().query));
        app.find.replace = "7".into();
        app.start_replace_scan();
        assert_eq!(app.wb.sheets[0].value(5,0), Value::Number(7.0));
        app.find.find = "700%".into();
        app.ensure_navigation(true);
        assert!(app.find_navigation.as_ref().unwrap().hits.is_empty());
        app.wb.sheets[0].set_style(5,0,percent);
        app.ensure_navigation(true);
        assert_eq!(app.find_navigation.as_ref().unwrap().hits.len(), 1);
    }

    #[test]
    fn replacement_during_collaboration_preserves_changed_hits_and_commits_survivors() {
        let _env = crate::test_env::TestEnv::new("replace-concurrent-room");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.wb.sheets.push(sheetcore::Sheet::new("Other"));
        for r in 0..5_000 { app.wb.sheets[0].set(r, 0, text(if r < 2 { "old" } else { "x" })); }
        app.find.find = "old".into();
        app.find.replace = "new".into();
        app.find.scope = 2;
        app.sel_end = (4_999, 0);
        app.ensure_navigation(true);
        // Stage worker completion so a real inbound Ops batch deterministically
        // lands after the search snapshot and before the replacement transaction.
        let nav = app.find_navigation.as_mut().unwrap();
        let (tx, rx) = mpsc::channel();
        nav.receiver = Some(rx);
        let hits = nav.query.all(&nav.snapshot, &AtomicBool::new(false));
        app.start_replace_scan();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        app.net = Some(crate::net::host_on(listener, 1, "host".into(), "test".into(), "pw".into()).unwrap());
        app.peer = Some(sheetcore::sync::Peer::new(1));
        let op = |sheet, row, col, val: &str| sheetcore::sync::Op {
            sheet, row, col, value: sheetcore::sync::ValueWire::Text(val.into()),
            formula: None, ts: 3, site: 2,
        };
        app.net.as_ref().unwrap().inject_event_for_test(crate::net::NetEvent::Ops(vec![
            op(1, 0, 0, "old on another sheet"), op(0, 0, 1, "old outside selection"),
            op(0, 1, 0, "old updated by collaborator"), op(0, 2, 0, "old added during scan"),
        ]));
        app.poll_net();
        app.wb_epoch += 1;
        app.poll_find_scan();
        assert!(app.find_scan.is_some());
        tx.send(hits).unwrap();
        app.poll_find_scan();
        assert!(app.find_scan.is_none());
        assert_eq!(app.wb.sheets[0].get(0, 0).as_deref(), Some(&text("new")));
        assert_eq!(app.wb.sheets[0].get(1, 0).as_deref(), Some(&text("old updated by collaborator")));
        assert_eq!(app.wb.sheets[0].get(2, 0).as_deref(), Some(&text("old added during scan")));
        assert_eq!(app.wb.sheets[0].get(0, 1).as_deref(), Some(&text("old outside selection")));
        assert_eq!(app.wb.sheets[1].get(0, 0).as_deref(), Some(&text("old on another sheet")));
        assert!(app.status.contains("已更改的 1 处"), "{}", app.status);
    }

    #[test]
    fn prepared_replace_rechecks_late_locks_and_conflicts_and_cancels_atomically() {
        let _env = crate::test_env::TestEnv::new("staged-replace");
        let mut app = SheetApp::new_with_context(&egui::Context::default(),None);
        app.wb.sheets.push(sheetcore::Sheet::new("Other"));
        for r in 0..2500 { app.wb.sheets[0].set(r,0,text("old")); }
        app.wb.sheets[1].set(1,2,text("old"));
        app.find.find = "old".into();
        app.find.replace = "new".into();
        app.find.scope = 1;
        let stage = |app: &mut SheetApp| {
            app.replace_all();
            wait_for_index(app);
            app.poll_find_scan();
            assert!(app.find_scan.as_ref().unwrap().worker.is_some());
        };
        let finish = |app: &mut SheetApp| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while app.find_scan.is_some() { app.poll_find_scan(); assert!(std::time::Instant::now()<until); std::thread::yield_now(); }
        };
        stage(&mut app);
        app.cancel_find_scan();
        assert_eq!(app.undo.depth(),(0,0));
        assert_eq!(app.wb.sheets[0].get(0,0).as_deref(),Some(&text("old")));
        stage(&mut app);
        app.locks.request(1,1,2,66,"late holder",app.lock_now());
        finish(&mut app);
        assert!(app.status.contains("late holder"));
        assert_eq!(app.undo.depth(),(0,0));
        app.locks.release(1,1,2,66);
        stage(&mut app);
        app.wb.sheets[0].set(5,0,text("old edited by another user"));
        app.wb.sheets[0].set(4,1,text("unrelated"));
        finish(&mut app);
        assert_eq!(app.wb.sheets[0].value(2499,0),Value::Text("new".into()));
        assert_eq!(app.wb.sheets[1].value(1,2),Value::Text("new".into()));
        assert!(app.status.contains("已更改的 1 处"));
        assert_eq!(app.undo.depth(),(1,0));
        app.do_undo(false);
        assert_eq!(app.wb.sheets[1].value(1,2),Value::Text("old".into()));
        assert_eq!(app.wb.sheets[0].value(2499,0),Value::Text("old".into()));
        assert_eq!(app.wb.sheets[0].value(5,0),Value::Text("old edited by another user".into()));
        assert_eq!(app.wb.sheets[0].value(4,1),Value::Text("unrelated".into()));
    }

    #[test]
    #[ignore = "explicit large-sheet latency measurement"]
    fn benchmark_large_replace_ui_commit() {
        let _env = crate::test_env::TestEnv::new("benchmark-large-replace");
        let mut app = SheetApp::new_with_context(&egui::Context::default(),None);
        let rows = std::env::var("CELLAN_REPLACE_PROBE_ROWS").ok().and_then(|s| s.parse().ok()).unwrap_or(100_000u32);
        for r in 0..rows { app.wb.sheets[0].set(r,0,text("old")); }
        app.find.find = "old".into();
        app.find.replace = "new".into();
        let start = std::time::Instant::now();
        app.replace_all();
        let mut worst = start.elapsed();
        let mut commit = Default::default();
        while app.find_scan.is_some() {
            let tick = std::time::Instant::now();
            app.poll_find_scan();
            let elapsed = tick.elapsed();
            worst = worst.max(elapsed);
            if app.find_scan.is_none() { commit = elapsed; }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(app.wb.sheets[0].get(rows-1,0).as_deref(),Some(&text("new")));
        assert_eq!(app.undo.depth(),(1,0));
        eprintln!("replace {rows} matches total={:?} max_ui_poll={worst:?} commit={commit:?}",start.elapsed());
    }

    fn wait_for_index(app: &mut SheetApp) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app
            .find_navigation
            .as_ref()
            .is_some_and(|nav| nav.receiver.is_some())
        {
            app.finish_navigation_index();
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    fn text(value: &str) -> Cell {
        Cell {
            value: Value::Text(value.into()),
            formula: None,
        }
    }

    #[test]
    fn large_search_navigates_immediately_then_reuses_ordered_hits_and_invalidates_on_edit() {
        let _env = crate::test_env::TestEnv::new("find-navigation");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        for r in 0..12_000 {
            app.wb.sheet_mut(0).set(
                r,
                0,
                text(if [1, 2, 11_000].contains(&r) {
                    "Target"
                } else {
                    "other"
                }),
            );
        }
        app.find.find = "target".into();
        app.find_next(false);
        assert_eq!(app.sel, (1, 0));
        app.find_next(false);
        assert_eq!(app.sel, (2, 0));
        wait_for_index(&mut app);
        assert_eq!(app.find.hits, 3);
        assert!(app.status.contains("第 2/3"));
        let query = app.find_navigation.as_ref().unwrap().query.clone();
        app.find_next(false);
        assert_eq!(app.sel, (11_000, 0));
        app.find_next(false);
        assert_eq!(app.sel, (1, 0));
        assert!(app.status.contains("回到开头"));
        assert!(Arc::ptr_eq(
            &query,
            &app.find_navigation.as_ref().unwrap().query
        ));

        app.wb.sheet_mut(0).set(11_000, 0, text("gone"));
        app.find_next(true);
        wait_for_index(&mut app);
        assert_eq!(app.sel, (2, 0));
        assert_eq!(app.find.hits, 2);
        assert!(app.status.contains("从末尾继续"));
        app.find.match_case = true;
        app.find_next(false);
        wait_for_index(&mut app);
        assert_eq!(app.find.hits, 0);
        assert_eq!(app.status, "没找到");
    }

    #[test]
    fn delayed_navigation_keeps_unrelated_status_and_shares_counts_with_workbook_results() {
        let _env = crate::test_env::TestEnv::new("pending-find-status");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.wb.sheets.push(sheetcore::Sheet::new("Other"));
        app.wb.sheets[0].set(0, 0, text("start"));
        app.wb.sheets[0].set(8_000, 0, text("target"));
        app.wb.sheets[1].set(0, 0, text("target"));
        app.find.scope = 1;
        app.find.find = "target".into();
        app.ensure_navigation(true);
        // Hold the result until an unrelated save has written the status bar.
        let nav = app.find_navigation.as_mut().unwrap();
        let (tx, rx) = mpsc::channel();
        nav.receiver = Some(rx);
        let expected_hits = (*nav.hits).clone();
        app.find_next(false);
        assert_eq!(app.sel, (0, 0));
        app.status = "保存完成".into();
        tx.send(expected_hits).unwrap();
        app.finish_navigation_index();
        assert_eq!(app.sel, (8_000, 0));
        assert_eq!(app.status, "保存完成");
        app.find_summary(false);
        assert!(app.status.contains("整个工作簿") && app.status.contains("2 处"));
        app.start_scan(0);
        assert!(app.find.find_all_open && app.find.find_all[1].sheet == 1);
        assert!(Arc::ptr_eq(
            &app.find.find_all,
            &app.find_navigation.as_ref().unwrap().hits
        ));
    }

    #[test]
    fn search_cache_follows_number_format_and_discards_a_superseded_query() {
        let _env = crate::test_env::TestEnv::new("find-style-revision");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.wb.sheet_mut(0).set(
            1,
            0,
            Cell {
                value: Value::Number(0.5),
                formula: None,
            },
        );
        app.find.find = "50.00%".into();
        app.find_next(false);
        assert_eq!(app.find.hits, 0);
        let mut style = app.wb.styles.get(0).clone();
        style.format = "0.00%".into();
        let id = app.wb.styles.intern(style);
        app.wb.sheet_mut(0).set_style(1, 0, id);
        app.find_next(false);
        assert_eq!(app.find.hits, 1);
        assert_eq!(app.sel, (1, 0));
        for r in 2..12_000 {
            app.wb.sheet_mut(0).set(r, 0, text("other"));
        }
        app.wb.sheet_mut(0).set(11_999, 0, text("far"));
        app.find.find = "far".into();
        app.find_next(false);
        app.find.find = "replacement query".into();
        app.status = "other action".into();
        let cursor = app.sel;
        wait_for_index(&mut app);
        assert_eq!(app.sel, cursor);
        assert_eq!(app.status, "other action");
    }

    #[test]
    fn phase5_bg_gate_find_navigation_rejects_stale_index_and_replace_on_coord_epoch() {
        use sheetcore::structural::{Structural, apply_structural_wb};

        let _env = crate::test_env::TestEnv::new("phase5-bg-gate-find-nav");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.wb.sheets.push(sheetcore::Sheet::new("Other"));
        for r in 0..1_500u32 {
            app.wb.sheets[0].set(r, 0, text("old"));
        }
        app.find.scope = 0; // current sheet
        app.find.find = "old".into();
        app.find.replace = "new".into();

        // 1. Ordinary edit on sheet 1 must NOT invalidate sheet-0 find navigation cache.
        app.ensure_navigation(true);
        let original = app.find_navigation.as_ref().unwrap().query.clone();
        app.wb.sheets[1].set(0, 0, text("unrelated"));
        app.doc_gen += 1;
        app.ensure_navigation(true);
        assert!(Arc::ptr_eq(&original, &app.find_navigation.as_ref().unwrap().query));

        // 2. Pending background find index is discarded when wb.coord_epoch() advances.
        let (tx, rx) = mpsc::channel();
        let stale_hits = (*app.find_navigation.as_ref().unwrap().hits).clone();
        app.find_navigation.as_mut().unwrap().receiver = Some(rx);
        app.sel = (0, 0);
        app.find_next(false);
        let cursor = app.sel;
        apply_structural_wb(&mut app.wb, 1, &Structural::InsertRows { at: 0, n: 1 });
        tx.send(stale_hits).unwrap();
        app.poll_navigation(&ctx);
        assert!(app.find_navigation.is_none());
        assert_eq!(app.sel, cursor);

        // 3. Background Replace All worker is cancelled cleanly when wb.coord_epoch() advances.
        app.start_replace_scan();
        assert!(app.find_scan.as_ref().unwrap().worker.is_some());
        apply_structural_wb(&mut app.wb, 1, &Structural::InsertCols { at: 0, n: 1 });
        app.poll_find_scan();
        assert!(app.find_scan.is_none());
        assert!(app.status.contains("已取消替换"), "{}", app.status);
        assert_eq!(app.wb.sheets[0].value(0, 0), Value::Text("old".into()));
        assert_eq!(app.undo.depth(), (0, 0));
    }
}
