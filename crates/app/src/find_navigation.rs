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
    document: u64,
    revisions: Vec<(u64, sheetcore::store::CellVersion, u64, u64)>,
    dates: sheetcore::model::DateSystem,
}

impl Query {
    fn same_scope(&self, other: &Self) -> bool {
        self.document == other.document && self.sheet == other.sheet
            && self.revisions.iter().map(|v| v.0).eq(other.revisions.iter().map(|v| v.0))
    }

    fn same_request(&self, other: &Self) -> bool {
        self.same_scope(other) && self.needle == other.needle && self.options == other.options
            && self.order == other.order && self.rects == other.rects && self.dates == other.dates
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
    snapshot: Arc<Workbook>,
    text: String,
    replace_formula_results: bool,
}

impl Navigation {
    fn current(&self, now: &Query, wb: &Workbook) -> bool {
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
            query: Arc::clone(&nav.query),
            text: self.find.replace.clone(),
            replace_formula_results: self.find.replace_formula_results,
        });
        if nav.receiver.is_some() {
            self.status = "正在查找可替换的单元格，可点“取消”…".into();
        } else {
            self.poll_find_scan();
        }
    }

    pub(super) fn poll_find_scan(&mut self) {
        let Some(scan) = &self.find_scan else { return };
        let current = self.navigation_query();
        if !scan.query.same_scope(&current) {
            self.cancel_find_scan();
            self.status = "文档已切换，已取消扫描".into();
            return;
        }
        if !scan.query.same_request(&current) || scan.text != self.find.replace
            || scan.replace_formula_results != self.find.replace_formula_results {
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
        if nav.receiver.is_none() {
            let baseline = self.find_scan.as_ref().unwrap().snapshot.clone();
            let hits = Arc::clone(&nav.hits);
            self.find_scan = None;
            self.apply_replace_hits(&hits, &baseline);
        }
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
        Query {
            needle: search::normalize_needle(&self.find.find, &options),
            order: self.search_order(),
            options,
            sheet,
            rects,
            document: self.doc_uid,
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
            let query = Arc::new(query);
            let snapshot = self.search_workbook(&query);
            let mut nav = Navigation {
                query: query.clone(),
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
        assert_eq!(app.wb.sheets[0].get(0, 0), Some(&text("new")));
        assert_eq!(app.wb.sheets[0].get(1, 0), Some(&text("old updated by collaborator")));
        assert_eq!(app.wb.sheets[0].get(2, 0), Some(&text("old added during scan")));
        assert_eq!(app.wb.sheets[0].get(0, 1), Some(&text("old outside selection")));
        assert_eq!(app.wb.sheets[1].get(0, 0), Some(&text("old on another sheet")));
        assert!(app.status.contains("已更改的 1 处"), "{}", app.status);
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
}
