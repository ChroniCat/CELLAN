//! Status bar statistics for the current selection.

use super::{SheetApp, filter_choices};
use eframe::egui;
use sheetcore::Value;

/// 状态栏统计累加器（UI-25）：计数=非空格；数值统计只吃 Number。
#[derive(Default, Clone, Copy)]
pub(super) struct StatsAcc {
    pub(super) count: u64,
    pub(super) nums: u64,
    pub(super) sum: f64,
    min: f64,
    max: f64,
}

impl StatsAcc {
    pub(super) fn add(&mut self, v: Option<f64>) {
        self.count += 1;
        if let Some(v) = v {
            if self.nums == 0 {
                self.min = v;
                self.max = v;
            } else {
                self.min = self.min.min(v);
                self.max = self.max.max(v);
            }
            self.nums += 1;
            self.sum += v;
        }
    }

    pub(super) fn text(&self, mask: u8) -> String {
        fn num(v: f64) -> String {
            let mut s = format!("{v:.4}");
            while s.contains('.') && (s.ends_with('0') || s.ends_with('.')) {
                s.pop();
            }
            if s == "-0" {
                s = "0".into();
            }
            s
        }
        let mut parts: Vec<String> = Vec::new();
        if mask & 1 != 0 && self.nums > 0 {
            parts.push(format!("平均值: {}", num(self.sum / self.nums as f64)));
        }
        if mask & 2 != 0 && self.count > 0 {
            parts.push(format!("计数: {}", self.count));
        }
        if mask & 4 != 0 && self.nums > 0 {
            parts.push(format!("数值计数: {}", self.nums));
        }
        if mask & 8 != 0 && self.nums > 0 {
            parts.push(format!("求和: {}", num(self.sum)));
        }
        if mask & 16 != 0 && self.nums > 0 {
            parts.push(format!("最小值: {}", num(self.min)));
        }
        if mask & 32 != 0 && self.nums > 0 {
            parts.push(format!("最大值: {}", num(self.max)));
        }
        parts.join("    ")
    }
}

/// UI-25：统计要排除的行 —— 手动隐藏、分级/组合隐藏、当前筛选隐藏。
///
/// 口径来自本机 Excel 16.0 (Build 20326) 状态栏 UIA 实测
/// （`implementation-evidence/ui25-hidden-marker.txt`）：
/// - 手动隐藏第 2 行后选 A1:A4（1,2,text,4）：计数 3、求和 5、平均 2.5（第 2 行排除）；
/// - 折叠分级隐藏第 7-8 行后选 A6:A8（7,8）：计数 1、求和 7（同样排除）；
/// - 手动隐藏 B 列后选 A1:B1（1,100）：计数 2、求和 101（隐藏列**仍计入**）。
///
/// 隐藏列因此不参与过滤：Excel 状态栏按行隐藏生效，和 SUBTOTAL 的行语义一致。
pub(super) fn stats_hidden_rows(sheet: &sheetcore::model::Sheet) -> std::collections::HashSet<u32> {
    let mut skip = (*sheet.hidden_rows).clone();
    skip.extend(sheet.hidden_rows_user.iter().copied());
    if skip.is_empty() {
        if let Some(f) = &sheet.filter {
            skip.extend(f.hidden_rows(sheet));
        }
    }
    skip
}

/// Visit each selected, non-empty cell once. Rows from iter_rect are ordered, so
/// filter comparisons and formatted lookup values are reused across each row.
fn visit_stats_values(
    sheet: &sheetcore::model::Sheet,
    rects: &[(u32, u32, u32, u32)],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    mut visit: impl FnMut(Option<f64>),
) {
    let filter_check_needed = sheet.hidden_rows.is_empty()
        && sheet
            .filter
            .as_ref()
            .is_some_and(|f| !f.allows.is_empty() || !f.criteria.is_empty());
    let compiled_filter = filter_check_needed
        .then(|| sheet.filter.as_ref().map(filter_choices::CompiledFilter::new))
        .flatten();
    let row_hidden = |r: u32| -> bool {
        if sheet.rows.size(r) <= 0.0
            || sheet.hidden_rows.contains(&r)
            || sheet.hidden_rows_user.contains(&r)
        {
            return true;
        }
        compiled_filter
            .as_ref()
            .is_some_and(|cf| cf.row_hidden(sheet, r))
    };

    for (i, &rect) in rects.iter().enumerate() {
        let mut previous_row = None;
        let mut hidden = false;
        let mut add = |r, c, cell: &sheetcore::Cell| {
            if previous_row != Some(r) {
                previous_row = Some(r);
                hidden = row_hidden(r);
            }
            if !hidden && !rects[..i].iter().any(|&(r1, c1, r2, c2)| {
                r1 <= r && r <= r2 && c1 <= c && c <= c2
            }) {
                visit(match &cell.value {
                    Value::Number(n) => Some(*n),
                    _ => None,
                });
            }
        };
        let (r1, c1, r2, c2) = rect;
        let area = (u64::from(r2.saturating_sub(r1)) + 1)
            * (u64::from(c2.saturating_sub(c1)) + 1);
        if area <= 4096 {
            for r in r1..=r2 {
                for c in c1..=c2 {
                    if let Some(cell) = sheet.get(r, c).filter(|cell| !cell.is_blank()) {
                        add(r, c, cell.as_ref());
                    }
                }
            }
        } else {
            for (key, cell) in sheet.iter_rect(rect) {
                if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                    return;
                }
                add(key.row, key.col, cell.as_ref());
            }
        }
        if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
            return;
        }
    }
}

#[cfg(test)]
pub(super) fn collect_stats_values(
    sheet: &sheetcore::model::Sheet,
    rects: &[(u32, u32, u32, u32)],
) -> Vec<Option<f64>> {
    let mut vals = Vec::new();
    visit_stats_values(sheet, rects, None, |v| vals.push(v));
    vals
}

/// 多选区统计入口（状态栏用）；隐藏行口径见 [`stats_hidden_rows`]。
pub(super) fn status_stats_rects(
    sheet: &sheetcore::model::Sheet,
    rects: &[(u32, u32, u32, u32)],
    mask: u8,
) -> String {
    let mut acc = StatsAcc::default();
    visit_stats_values(sheet, rects, None, |v| acc.add(v));
    acc.text(mask)
}

/// 状态栏统计文本（C25）：单矩形兼容入口；按位掩码输出 Excel 同名项。
#[cfg(test)]
pub(super) fn status_stats_text(
    sheet: &sheetcore::model::Sheet,
    rect: (u32, u32, u32, u32),
    mask: u8,
) -> String {
    status_stats_rects(sheet, &[rect], mask)
}

#[derive(Clone)]
pub(super) struct StatsKey {
    document: u64,
    sheet: u64,
    cells: sheetcore::store::CellVersion,
    rows: u64,
    visibility: (u64, u64),
    rects: Vec<(u32, u32, u32, u32)>,
    mask: u8,
    filter: Option<sheetcore::sort_filter::Filter>,
}

fn stats_filter<'a>(sheet: &'a sheetcore::Sheet, rects: &[(u32,u32,u32,u32)]) -> Option<&'a sheetcore::sort_filter::Filter> {
    sheet.filter.as_ref().filter(|f| sheet.hidden_rows.is_empty() &&
        (!f.allows.is_empty() || !f.criteria.is_empty()) &&
        rects.iter().any(|&(r1,_,r2,_)| r2 > f.range.0 && r1 <= f.range.2))
}

impl StatsKey {
    fn current(&mut self, doc: u64, live: &sheetcore::Sheet, rects: &[(u32,u32,u32,u32)],
        mask: u8, source: Option<&sheetcore::Sheet>) -> bool {
        let filter = source.and_then(|s| s.filter.as_ref()).or(self.filter.as_ref());
        if self.document != doc || self.sheet != live.identity || self.mask != mask || self.rects != rects
            || filter != stats_filter(live, rects) { return false; }
        let rows = live.rows.revision();
        let visibility = (live.hidden_rows.revision(), live.hidden_rows_user.revision());
        if self.rows != rows || self.visibility != visibility {
            let Some(old) = source else { return false; };
            let overlaps = |lo: u32, hi: u32| rects.iter().any(|&(r1, _, r2, _)| lo <= r2 && hi >= r1);
            if self.rows != rows && live.rows.zero_size_revision() != old.rows.zero_size_revision() {
                if (live.rows.default_size() <= 0.0) != (old.rows.default_size() <= 0.0) {
                    return false;
                }
                for s in live
                    .rows
                    .spans()
                    .iter()
                    .filter(|s| s.size <= 0.0)
                    .chain(old.rows.spans().iter().filter(|s| s.size <= 0.0))
                {
                    if overlaps(s.start, s.end) {
                        for r in s.start..=s.end {
                            if rects.iter().any(|&(r1, _, r2, _)| r1 <= r && r <= r2)
                                && (live.rows.size(r) <= 0.0) != (old.rows.size(r) <= 0.0)
                            {
                                return false;
                            }
                        }
                    }
                }
            }
            for (before, after) in [(&old.hidden_rows, &live.hidden_rows), (&old.hidden_rows_user, &live.hidden_rows_user)] {
                if before.revision() != after.revision()
                    && before
                        .symmetric_difference_intervals(after)
                        .into_iter()
                        .any(|(lo, hi)| overlaps(lo, hi))
                {
                    return false;
                }
            }
        }
        if self.cells != *live.cell_version() {
            let Some(old) = source else { return false; };
            if !live.same_cells_in(old, rects, false) { return false; }
            // A filter column can be outside the selection and still hide one of its rows.
            if let Some(f) = filter {
                let mut columns = Vec::new();
                for &(r1,_,r2,_) in rects {
                    let start = r1.max(f.range.0.saturating_add(1));
                    let end = r2.min(f.range.2);
                    if start <= end {
                        columns.extend(f.allows.keys().chain(f.criteria.keys()).filter(|&&c| f.range.1 <= c && c <= f.range.3)
                            .map(|&c| (start,c,end,c)));
                    }
                }
                if !columns.is_empty() && !live.same_cells_in(old, &columns, false) { return false; }
            }
        }
        self.cells = live.cell_version().clone();
        self.rows = rows;
        self.visibility = visibility;
        true
    }
}

struct StatsWorkItem {
    id: u64,
    snapshot: std::sync::Arc<sheetcore::Sheet>,
    rects: Vec<(u32, u32, u32, u32)>,
    mask: u8,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    wake: egui::Context,
}

struct StatsWorkerQueue {
    latest: Option<StatsWorkItem>,
    shutdown: bool,
}

pub(super) struct StatsWorker {
    queue: std::sync::Arc<(std::sync::Mutex<StatsWorkerQueue>, std::sync::Condvar)>,
    rx: std::sync::mpsc::Receiver<(u64, String)>,
    next_id: u64,
}

impl StatsWorker {
    fn new() -> Self {
        let queue = std::sync::Arc::new((
            std::sync::Mutex::new(StatsWorkerQueue {
                latest: None,
                shutdown: false,
            }),
            std::sync::Condvar::new(),
        ));
        let (tx, rx) = std::sync::mpsc::channel::<(u64, String)>();
        let q = queue.clone();
        std::thread::spawn(move || {
            let (lock, cvar) = &*q;
            loop {
                let item = {
                    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                    while guard.latest.is_none() && !guard.shutdown {
                        guard = cvar.wait(guard).unwrap_or_else(|e| e.into_inner());
                    }
                    if guard.shutdown {
                        break;
                    }
                    guard.latest.take()
                };
                let Some(item) = item else { continue };
                if item.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    continue;
                }
                let mut acc = StatsAcc::default();
                visit_stats_values(&item.snapshot, &item.rects, Some(&item.cancel), |v| {
                    acc.add(v)
                });
                if item.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    continue;
                }
                if tx.send((item.id, acc.text(item.mask))).is_err() {
                    break;
                }
                item.wake.request_repaint();
            }
        });
        Self {
            queue,
            rx,
            next_id: 1,
        }
    }

    fn submit(
        &mut self,
        snapshot: std::sync::Arc<sheetcore::Sheet>,
        rects: Vec<(u32, u32, u32, u32)>,
        mask: u8,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        wake: egui::Context,
    ) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let (lock, cvar) = &*self.queue;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = guard.latest.take() {
            prev.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        guard.latest = Some(StatsWorkItem {
            id,
            snapshot,
            rects,
            mask,
            cancel,
            wake,
        });
        cvar.notify_one();
        id
    }
}

impl Drop for StatsWorker {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.queue;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = guard.latest.take() {
            prev.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        guard.shutdown = true;
        cvar.notify_one();
    }
}

pub(super) struct StatsJob {
    id: u64,
    key: StatsKey,
    pub(super) snapshot: std::sync::Arc<sheetcore::Sheet>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for StatsJob {
    fn drop(&mut self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl SheetApp {
    pub(super) fn selection_stats_label(
        &mut self, ctx: &egui::Context, rects: &[(u32, u32, u32, u32)], area: u64, mask: u8,
    ) -> String {
        use std::sync::atomic::Ordering;
        let sheet = self.wb.sheet(self.sheet_idx);
        let mut completed: Option<(u64, String)> = None;
        if let Some(worker) = self.stats_worker.as_ref() {
            while let Ok(msg) = worker.rx.try_recv() {
                completed = Some(msg);
            }
        }
        if let Some(job) = self.stats_job.as_mut() {
            let current = job.key.current(self.doc_uid, sheet, rects, mask, Some(&job.snapshot));
            if !current {
                job.cancel.store(true, Ordering::Relaxed);
                self.stats_job = None;
            } else if let Some((done_id, result)) = completed.filter(|(id, _)| *id == job.id) {
                let _ = done_id;
                let finished = self.stats_job.take().unwrap();
                if !finished.cancel.load(Ordering::Relaxed) {
                    self.stat_cache = Some((finished.key.clone(), result, Some(finished.snapshot.clone())));
                }
            }
        }
        if mask == 0 || area == 0 { return String::new(); }
        if let Some((key, result, snapshot)) = self.stat_cache.as_mut() {
            if key.current(self.doc_uid, sheet, rects, mask, snapshot.as_deref()) { return result.clone(); }
        }
        if area > 4096 && self.stats_job.is_some() { return "计算中…".into(); }
        let key = StatsKey {
            document: self.doc_uid, sheet: sheet.identity, cells: sheet.cell_version().clone(),
            rows: sheet.rows.revision(), visibility: (sheet.hidden_rows.revision(), sheet.hidden_rows_user.revision()),
            rects: rects.to_vec(), mask, filter: (area <= 4096).then(|| stats_filter(sheet, rects).cloned()).flatten(),
        };
        if area <= 4096 {
            let result = status_stats_rects(sheet, rects, mask);
            self.stat_cache = Some((key, result.clone(), None));
            return result;
        }
        if self.stats_job.is_none() {
            let snapshot = std::sync::Arc::new(sheet.selection_stats_snapshot_with_filter(stats_filter(sheet, rects).is_some()));
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker = self.stats_worker.get_or_insert_with(StatsWorker::new);
            let id = worker.submit(
                snapshot.clone(),
                rects.to_vec(),
                mask,
                cancel.clone(),
                ctx.clone(),
            );
            self.stats_job = Some(StatsJob { id, key, snapshot, cancel });
        }
        "计算中…".into()
    }
}

/// UI-25 状态栏统计菜单：六项开关 + 复制当前结果。
pub(super) fn stats_mask_menu(ui: &mut egui::Ui, mask: &mut u8, stat: &str) {
    for (bit, name) in [
        (1u8, "平均值"),
        (2, "计数"),
        (4, "数值计数"),
        (8, "求和"),
        (16, "最小值"),
        (32, "最大值"),
    ] {
        let mut on = *mask & bit != 0;
        if ui.checkbox(&mut on, name).changed() {
            if on {
                *mask |= bit;
            } else {
                *mask &= !bit;
            }
        }
    }
    ui.separator();
    if ui
        .add_enabled(!stat.is_empty() && stat != "计算中…", egui::Button::new("复制统计结果"))
        .clicked()
    {
        ui.ctx().copy_text(stat.to_string());
        ui.close_menu();
    }
}
