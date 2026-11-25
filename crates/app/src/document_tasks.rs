//! Background work belongs to one document and moves with its tab.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use sheetcore::{Workbook, xlsx::SaveReceipt};

use super::{egui, OpenMsg};

#[derive(Clone)]
pub(crate) struct StructuralCalculation {
    pub generation: u64,
    pub sheet: usize,
    pub op: sheetcore::structural::Structural,
    pub complex: bool,
    pub graph: std::sync::Arc<sheetcore::formula::deps::Deps>,
}

impl StructuralCalculation {
    pub fn apply(&self, wb: &mut Workbook, cancel: &std::sync::atomic::AtomicBool)
        -> Option<sheetcore::formula::deps::Deps> {
        if self.complex {
            self.graph.recalc_structural_sheets(wb, self.sheet, cancel)
        } else {
            self.graph.recalc_structural(wb, self.sheet, &self.op, cancel)
        }
    }
}

#[derive(Default)]
pub(crate) struct DocumentTasks {
    pub open_rx: Option<Receiver<OpenMsg>>,
    pub reorder: Option<super::data_reorder::Pending>,
    pub busy: bool,
    pub needs_full: bool,
    pub structural_recalc: Option<StructuralCalculation>,
    /// Retain pending edits until a result for this document generation is applied.
    pub dirty_recalc_seeds: std::collections::BTreeSet<sheetcore::formula::deps::CellKey>,
    pub last_edit: Option<Instant>,
    pub autosave_dirty_since: Option<Instant>,
    pub autosave_retry_at: Option<Instant>,
    /// Elapsed wall-clock milliseconds of the most recent completed save (drives adaptive autosave backoff).
    pub last_save_ms: Option<u64>,
    pub save_started_at: Option<Instant>,
    /// Ctrl+S requested a second snapshot while the previous one was being written.
    pub manual_save_queued: bool,
    /// A shared document inherits the host preference until the room switch is changed.
    pub shared_autosave: Option<bool>,
    /// Preserve save authority when a tab leaves its collaboration session.
    /// A former guest must not automatically overwrite a shared workbook.
    pub autosave_after_detach: Option<bool>,
    pub saving: Option<PendingSave>,
    pub pending_host: bool,
    pub pending_join: Option<String>,
    /// 后台全量重算的取消标志与进度（QA-07.3）：Esc 置位，状态栏读计数
    pub recalc_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub recalc_done: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    pub recalc_total: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    /// UI-29.3：加载遮罩上显示的文件名、起始时刻、路径与字节进度/取消控制器（打开完成即清空）
    pub open_label: Option<String>,
    pub open_started: Option<Instant>,
    pub open_path: Option<PathBuf>,
    pub open_ctrl: Option<std::sync::Arc<sheetcore::xlsx::LoadControl>>,
    /// 后台加载/手动重算发起时各表的 cell_revision 快照（纯格式变更不丢弃结果）
    pub open_recalc_revs: Vec<u64>,
    /// 大文件快速解锁首屏后，异步补齐的磁盘 SHA-256 基准指纹
    pub base_hash_rx: Option<Receiver<(PathBuf, String)>>,
    /// 大表后台 PDF 导出通道
    pub pdf_export: Option<Receiver<Result<(PathBuf, usize), String>>>,
}

pub(crate) struct PendingSave {
    pub target: PathBuf,
    pub rx: Receiver<Result<(SaveReceipt, Option<CalculatedSnapshot>), String>>,
    pub quiet: bool,
    pub used_dirty_graph: bool,
    /// Host session and collaboration clock represented by this original-file snapshot.
    pub host: Option<(String, u64)>,
}

#[derive(Debug)]
pub(crate) struct CalculatedSnapshot {
    pub revisions: Vec<u64>,
    pub workbook: Box<Workbook>,
}

#[inline]
fn contains_ascii_icase(haystack: &str, needle_lower: &[u8]) -> bool {
    haystack.len() >= needle_lower.len()
        && haystack
            .as_bytes()
            .windows(needle_lower.len())
            .any(|w| w.eq_ignore_ascii_case(needle_lower))
}

fn sheet_calc_is_self_contained(sheet: &sheetcore::Sheet, other_sheets_have_tables: bool) -> bool {
    if !sheet.has_formulas() {
        return true;
    }
    for (_, cell) in sheet.formula_cells() {
        let Some(f) = cell.formula.as_deref() else { continue };
        if f.contains('!')
            || (other_sheets_have_tables && f.contains('['))
            || contains_ascii_icase(f, b"indirect")
        {
            return false;
        }
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplyOutcome {
    /// All sheets matched snapshot revisions and were fully installed.
    FullySynced,
    /// Concurrent edits occurred, but every untainted formula cell across ALL sheets
    /// (including untouched cross-sheet formulas) was safely adopted; only `concurrent_edits`
    /// and their dependents remain dirty.
    PartialUntaintedAdopted,
    /// At least one sheet could not be safely partially adopted (e.g. spills, iterative calc,
    /// missing/incomplete dependency graph, or workbook-level invariants changed).
    NeedsFullRecalc,
}

impl CalculatedSnapshot {
    fn workbook_invariants_match(&self, live: &Workbook) -> bool {
        let calc = &self.workbook;
        live.date_system == calc.date_system
            && live.names == calc.names
            && live.calc_manual == calc.calc_manual
            && live.calc_iterate == calc.calc_iterate
            && live.calc_max_iterations == calc.calc_max_iterations
            && live.calc_max_change == calc.calc_max_change
            && live.links.len() == calc.links.len()
            && live.links.iter().zip(&calc.links).all(|(a, b)| {
                std::sync::Arc::ptr_eq(&a.cache.0, &b.cache.0)
                    && a.index == b.index
                    && a.target == b.target
                    && a.abs == b.abs
                    && a.sheets == b.sheets
                    && a.defined_names == b.defined_names
            })
            && live.sheets.len() == self.revisions.len()
            && live.sheets.len() == calc.sheets.len()
            && live
                .sheets
                .iter()
                .zip(&calc.sheets)
                .all(|(sh, result)| sh.identity == result.identity && sh.name == result.name)
    }

    #[inline]
    fn sheet_matches(sh: &sheetcore::Sheet, result: &sheetcore::Sheet, rev: u64) -> bool {
        sh.identity == result.identity
            && sh.cell_revision() == rev
            && sh.name == result.name
            && sh.tables == result.tables
            && sh.hidden_rows == result.hidden_rows
            && sh.hidden_rows_user == result.hidden_rows_user
            && sh.arrays == result.arrays
    }

    /// Strict all-or-nothing installation used when adopting a full save receipt.
    pub fn apply_all(mut self, live: &mut Workbook) -> bool {
        if !self.workbook_invariants_match(live) {
            return false;
        }
        let calc = &mut self.workbook;
        if !live
            .sheets
            .iter()
            .zip(&calc.sheets)
            .zip(&self.revisions)
            .all(|((sh, result), &rev)| Self::sheet_matches(sh, result, rev))
        {
            return false;
        }
        for (sheet, result) in live.sheets.iter_mut().zip(&mut calc.sheets) {
            sheet.take_calculation(result);
        }
        live.calc_pending = calc.calc_pending;
        live.calc_pending_sheets = std::mem::take(&mut calc.calc_pending_sheets);
        true
    }

    /// Install calculated results onto `live`. If all sheets remain untouched, installs everything
    /// and returns `true`. If some sheet was edited concurrently, still preserves calculated caches
    /// for untouched self-contained sheets while returning `false` so dirty sheets remain scheduled.
    pub fn apply(self, live: &mut Workbook) -> bool {
        self.apply_with_deps_and_edits(live, None, &[]) == ApplyOutcome::FullySynced
    }

    #[allow(dead_code)]
    pub(crate) fn apply_with_deps(
        self,
        live: &mut Workbook,
        deps: Option<&sheetcore::formula::deps::Deps>,
    ) -> bool {
        self.apply_with_deps_and_edits(live, deps, &[]) == ApplyOutcome::FullySynced
    }

    /// Install calculated results onto `live`.
    /// - If all sheets stayed untouched, installs everything and returns `ApplyOutcome::FullySynced`.
    /// - If concurrent edits occurred in `concurrent_edits` and `deps` is available (without
    ///   requiring full recalc / iterative calc / spills), adopts calculated formula caches for
    ///   EVERY formula cell OUTSIDE `deps.edit_closure(concurrent_edits)` across ALL sheets
    ///   (including untouched sheets with cross-sheet refs or defined names) and returns
    ///   `ApplyOutcome::PartialUntaintedAdopted`.
    /// - Otherwise returns `ApplyOutcome::NeedsFullRecalc` so callers keep `needs_full = true`
    ///   and preserve `fullCalcOnLoad="1"` on save until recalculation truly finishes.
    pub(crate) fn apply_with_deps_and_edits(
        mut self,
        live: &mut Workbook,
        deps: Option<&sheetcore::formula::deps::Deps>,
        concurrent_edits: &[(usize, u32, u32)],
    ) -> ApplyOutcome {
        if !self.workbook_invariants_match(live) {
            return ApplyOutcome::NeedsFullRecalc;
        }
        let calc = &mut self.workbook;
        let clean_flags: Vec<bool> = live
            .sheets
            .iter()
            .zip(&calc.sheets)
            .zip(&self.revisions)
            .map(|((sh, result), &rev)| Self::sheet_matches(sh, result, rev))
            .collect();
        if clean_flags.iter().all(|&ok| ok) {
            for (sheet, result) in live.sheets.iter_mut().zip(&mut calc.sheets) {
                sheet.take_calculation(result);
            }
            live.calc_pending = calc.calc_pending;
            live.calc_pending_sheets = std::mem::take(&mut calc.calc_pending_sheets);
            live.mark_recalc_synced();
            return ApplyOutcome::FullySynced;
        }
        // 1) Adopt completely untouched self-contained sheets
        let mut adopted_full = vec![false; live.sheets.len()];
        if live.names.is_empty() && !live.calc_iterate {
            let table_sheet_count = live.sheets.iter().filter(|s| !s.tables.is_empty()).count();
            for (si, ((sheet, result), &is_clean)) in live
                .sheets
                .iter_mut()
                .zip(&mut calc.sheets)
                .zip(&clean_flags)
                .enumerate()
            {
                if !is_clean {
                    continue;
                }
                let other_sheets_have_tables = if sheet.tables.is_empty() {
                    table_sheet_count > 0
                } else {
                    table_sheet_count > 1
                };
                if sheet_calc_is_self_contained(sheet, other_sheets_have_tables) {
                    sheet.take_calculation(result);
                    live.calc_pending_sheets.remove(&si);
                    adopted_full[si] = true;
                }
            }
        }
        // 2) Partial cell-level adoption across ALL non-fully-adopted sheets (including untouched
        //    sheets with cross-sheet refs or defined names) outside the tainted closure.
        if !concurrent_edits.is_empty()
            && !live.calc_iterate
            && let Some(d) = deps
            && !d.requires_full_recalc()
        {
            let mut tainted = d.edit_closure(concurrent_edits);
            tainted.extend(concurrent_edits.iter().copied());
            let mut all_covered = true;
            for (si, (sheet, result)) in live
                .sheets
                .iter_mut()
                .zip(&calc.sheets)
                .enumerate()
            {
                if adopted_full[si] {
                    continue;
                }
                if !result.spill_shapes.is_empty() || !sheet.spill_shapes.is_empty() {
                    all_covered = false;
                    continue;
                }
                let mut updates: Vec<(u32, u32, sheetcore::Value)> = Vec::new();
                for (cref, calc_cell) in result.formula_cells() {
                    let key = (si, cref.row, cref.col);
                    if tainted.contains(&key) {
                        continue;
                    }
                    if let Some(live_cell) = sheet.get(cref.row, cref.col)
                        && live_cell.formula == calc_cell.formula
                        && live_cell.value != calc_cell.value
                    {
                        updates.push((cref.row, cref.col, calc_cell.value.clone()));
                    }
                }
                for (r, c, val) in updates {
                    sheet.set_formula_cache(r, c, val);
                }
            }
            if all_covered {
                return ApplyOutcome::PartialUntaintedAdopted;
            }
        }
        ApplyOutcome::NeedsFullRecalc
    }
}

impl DocumentTasks {
    pub fn start_pdf_export_pack(
        &mut self,
        wb: Workbook,
        sheet_idx: usize,
        out_path: PathBuf,
        pack: sheetcore::print::PdfFontPack,
        page_indices: Option<Vec<usize>>,
    ) {
        let (tx, rx) = mpsc::channel();
        self.pdf_export = Some(rx);
        std::thread::spawn(move || {
            let wb_path = wb.path.clone();
            let res = sheetcore::print::export_pdf_sheet_with_pack(
                &wb,
                sheet_idx,
                &out_path,
                pack,
                wb_path.as_deref(),
                page_indices.as_deref(),
            )
            .map(|n| (out_path, n))
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(res);
        });
    }

    pub fn start_workbook_pdf_export(
        &mut self,
        wb: Workbook,
        out_path: PathBuf,
        pack: sheetcore::print::PdfFontPack,
    ) {
        let (tx, rx) = mpsc::channel();
        self.pdf_export = Some(rx);
        std::thread::spawn(move || {
            let wb_path = wb.path.clone();
            let res = sheetcore::print::export_workbook_pdf_with_pack(
                &wb,
                &out_path,
                pack,
                wb_path.as_deref(),
            )
            .map(|n| (out_path, n))
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(res);
        });
    }

    pub fn poll_pdf_export(&mut self, status: &mut String, ctx: &egui::Context) {
        let Some(rx) = self.pdf_export.as_ref() else { return };
        match rx.try_recv() {
            Ok(Ok((path, n))) => {
                self.pdf_export = None;
                *status = format!("已导出 {}", path.display());
                eprintln!("[pdf] 已导出 {} 页 {}", n, path.display());
                ctx.request_repaint();
            }
            Ok(Err(e)) => {
                self.pdf_export = None;
                *status = format!("导出失败：{e}");
                eprintln!("[pdf] 导出失败 {e}");
                ctx.request_repaint();
            }
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            Err(TryRecvError::Disconnected) => {
                self.pdf_export = None;
                *status = "导出失败：后台 PDF 导出线程异常中止".into();
                ctx.request_repaint();
            }
        }
    }

    pub fn poll_base_hash(&mut self, wb: &Workbook, wb_base_hash: &mut String) -> Option<String> {
        let rx = self.base_hash_rx.as_ref()?;
        match rx.try_recv() {
            Ok((path, hash)) => {
                self.base_hash_rx = None;
                if !wb.dirty && wb.path.as_deref() == Some(path.as_path()) && !hash.is_empty() {
                    *wb_base_hash = hash.clone();
                    return Some(hash);
                }
                None
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.base_hash_rx = None;
                None
            }
        }
    }

    /// Save a quiet edit promptly, or snapshot an ongoing stream of committed edits.
    pub fn autosave_due(&mut self, wb: &Workbook, ctx: Option<&egui::Context>) -> bool {
        if !wb.dirty || wb.load_incomplete || wb.path.as_deref().is_none_or(|p| !sheetcore::xlsx::can_save_to(p)) {
            self.autosave_dirty_since = None;
            return false;
        }
        let now = Instant::now();
        let first = *self.autosave_dirty_since.get_or_insert(now);
        if self.saving.is_some() || self.open_rx.is_some() || self.busy {
            return false;
        }
        if self.manual_save_queued {
            return true;
        }
        let total_cells: usize = wb.sheets.iter().map(|s| s.len()).sum();
        let last_ms = self.last_save_ms.unwrap_or(0);
        let (quiet_ms, max_ms) = if last_ms > 250 || total_cells > 50_000 {
            let backoff_ms = last_ms.saturating_mul(8).clamp(5_000, 60_000);
            (backoff_ms, backoff_ms)
        } else {
            (400, 2_000)
        };
        let quiet = self.last_edit.unwrap_or(first) + Duration::from_millis(quiet_ms);
        let mut due = quiet.min(first + Duration::from_millis(max_ms));
        if let Some(retry) = self.autosave_retry_at {
            due = due.max(retry);
        }
        if due <= now {
            return true;
        }
        if let Some(ctx) = ctx {
            ctx.request_repaint_after(due.duration_since(now));
        }
        false
    }

    pub fn recalc_pending(&self) -> bool {
        self.needs_full || !self.dirty_recalc_seeds.is_empty()
    }

    pub fn start_save(&mut self, wb: &mut Workbook, target: PathBuf) {
        self.start_save_checked(wb, target, None, None, |_| None);
    }

    pub fn start_save_checked(
        &mut self,
        wb: &mut Workbook,
        target: PathBuf,
        deps: Option<std::sync::Arc<sheetcore::formula::deps::Deps>>,
        generation: Option<u64>,
        check_target: impl FnOnce(&Path) -> Option<String> + Send + 'static,
    ) {
        debug_assert!(self.saving.is_none());
        let mut snapshot = wb.clone();
        let (tx, rx) = mpsc::channel();
        let used_dirty_graph = deps.is_some() && !self.needs_full && !snapshot.calc_iterate;
        self.save_started_at = Some(Instant::now());
        self.saving = Some(PendingSave {
            target: target.clone(),
            rx,
            quiet: false,
            used_dirty_graph,
            host: None,
        });
        // Subsequent edits set this back to true. Completion never clears it.
        wb.dirty = false;
        self.autosave_dirty_since = None;
        let pending_recalc = self.recalc_pending();
        let recalc = pending_recalc && !snapshot.calc_manual;
        if pending_recalc && snapshot.calc_manual {
            // 手动计算且存在未重算改动：写出时明确标记 calc_pending，避免 Excel 盲信旧缓存。
            snapshot.calc_pending = true;
        }
        let structure = self.structural_recalc.as_ref()
            .filter(|s| recalc && self.needs_full && Some(s.generation) == generation).cloned();
        let incremental = deps.filter(|_| recalc && !self.needs_full && !snapshot.calc_iterate)
            .map(|graph| (graph, self.dirty_recalc_seeds.iter().copied().collect::<Vec<_>>()));
        std::thread::spawn(move || {
            // A missing original has no package to inherit on Save As.
            snapshot.path = snapshot.path.take().filter(|p| p.exists());
            // Saving or closing can beat the editor's debounce. Finish the
            // captured generation before serializing its formula caches.
            let revisions = recalc.then(|| {
                let revisions = snapshot.sheets.iter().map(|sh| sh.cell_revision()).collect();
                if let Some((graph, seeds)) = incremental {
                    sheetcore::formula::recalc_dirty(&mut snapshot, &graph, &seeds);
                } else if !structure.is_some_and(|s| s.apply(&mut snapshot,&false.into()).is_some()) {
                    sheetcore::formula::recalc_all(&mut snapshot);
                }
                snapshot.calc_pending = false;
                snapshot.calc_pending_sheets.clear();
                snapshot.mark_recalc_synced();
                revisions
            });
            // The core owns the temporary write, history, and atomic replacement.
            if let Some(error) = check_target(&target) {
                let _ = tx.send(Err(error));
                return;
            }
            let result = sheetcore::xlsx::save_snapshot_with_receipt(&target, snapshot)
                .map(|(receipt, snapshot)| (receipt, revisions.map(|revisions| CalculatedSnapshot {
                    revisions, workbook: Box::new(snapshot),
                })))
                .map_err(|e| format!("保存失败: {e:#}"));
            let _ = tx.send(result);
        });
    }

    pub fn poll_save(&mut self, wb: &mut Workbook) -> Option<Result<(PathBuf, bool), String>> {
        let pending = self.saving.as_ref()?;
        let result = match pending.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("保存线程意外退出，改动尚未保存".into()),
        };
        let pending = self.saving.take().unwrap();
        let started_at = self.save_started_at.take();
        Some(match result {
            Ok((receipt, calculated)) => {
                if let Some(t) = started_at {
                    self.last_save_ms = Some(t.elapsed().as_millis() as u64);
                }
                self.autosave_retry_at = None;
                if wb.path.as_ref() != Some(&pending.target)
                    || (self.autosave_after_detach == Some(true) && !wb.dirty)
                {
                    self.autosave_after_detach = None;
                }
                let calculated_on_save = calculated.is_some();
                let applied = !wb.dirty && calculated.is_some_and(|result| result.apply_all(wb));
                if applied {
                    self.structural_recalc = None;
                    self.needs_full = false;
                    self.dirty_recalc_seeds.clear();
                }
                receipt.adopt_document(wb, &pending.target, !calculated_on_save || applied);
                Ok((pending.target, applied))
            }
            Err(error) => {
                wb.dirty = true;
                self.manual_save_queued = false;
                self.autosave_retry_at = Some(Instant::now() + Duration::from_secs(12));
                Err(error)
            }
        })
    }
}

#[cfg(test)]
#[path = "save_receipt_tests.rs"]
mod tests;

#[cfg(test)]
mod partial_calc_tests {
    use super::*;
    use sheetcore::model::{Cell, Sheet, Value};

    #[test]
    fn partial_calc_preserves_untouched_self_contained_sheets_and_skips_cross_sheet_refs() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "SelfContained".into();
        wb.sheets.push(Sheet::new("EditedSheet"));
        wb.sheets.push(Sheet::new("CrossRefSheet"));

        // Sheet 0: 纯表内自包含公式 `=A1*3`
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
        wb.sheets[0].set(0, 1, Cell { value: Value::Empty, formula: Some("=A1*3".into()) });
        // Sheet 1: 将在后台重算期间被并发编辑
        wb.sheets[1].set(0, 0, Cell { value: Value::Number(5.0), formula: None });
        // Sheet 2: 跨表引用 `=EditedSheet!A1+1`（当 Sheet 1 被并发改动时不得提前采纳旧快照结果）
        wb.sheets[2].set(
            0,
            0,
            Cell {
                value: Value::Empty,
                formula: Some("=EditedSheet!A1+1".into()),
            },
        );
        wb.calc_pending = true;
        wb.calc_pending_sheets.extend([0usize, 1, 2]);

        let revisions: Vec<u64> = wb.sheets.iter().map(|s| s.cell_revision()).collect();
        let mut worker_wb = wb.clone();
        sheetcore::formula::recalc_all(&mut worker_wb);
        let snap = CalculatedSnapshot {
            revisions,
            workbook: Box::new(worker_wb),
        };

        // 模拟后台重算飞行期间用户在 Sheet 1 输入新值 100.0
        wb.sheets[1].set(0, 0, Cell { value: Value::Number(100.0), formula: None });

        // apply 返回 false 以继续排队重算被修改的脏表，但 Sheet 0 的重算结果必须已被就地保留！
        assert!(!snap.apply(&mut wb));
        assert_eq!(wb.sheets[0].value(0, 1), Value::Number(30.0));
        assert!(!wb.calc_pending_sheets.contains(&0));
        assert_eq!(
            wb.sheets[2].value(0, 0),
            Value::Empty,
            "跨表依赖页不得写入基于旧 Sheet1=5 计算出的过期缓存"
        );
        assert!(wb.calc_pending_sheets.contains(&2));
    }
}
