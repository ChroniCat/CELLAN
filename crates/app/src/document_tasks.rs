//! Background work belongs to one document and moves with its tab.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use sheetcore::{Workbook, xlsx::SaveReceipt};

use super::OpenMsg;

#[derive(Default)]
pub(crate) struct DocumentTasks {
    pub open_rx: Option<Receiver<OpenMsg>>,
    pub busy: bool,
    pub needs_full: bool,
    /// Retain pending edits until a result for this document generation is applied.
    pub dirty_recalc_seeds: std::collections::BTreeSet<sheetcore::formula::deps::CellKey>,
    pub last_edit: Option<Instant>,
    pub saving: Option<PendingSave>,
    pub pending_host: bool,
    pub pending_join: Option<String>,
    /// 后台全量重算的取消标志与进度（QA-07.3）：Esc 置位，状态栏读计数
    pub recalc_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub recalc_done: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    pub recalc_total: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    /// UI-29.3：加载遮罩上显示的文件名与起始时刻（打开完成即清空）
    pub open_label: Option<String>,
    pub open_started: Option<Instant>,
    pub open_path: Option<PathBuf>,
}

pub(crate) struct PendingSave {
    pub target: PathBuf,
    pub rx: Receiver<Result<(SaveReceipt, Option<CalculatedSnapshot>), String>>,
    /// 开始保存的时刻，状态栏据此显示已用时间（QA-07.3 进度反馈）
    pub started: Instant,
}

#[derive(Debug)]
pub(crate) struct CalculatedSnapshot {
    pub revisions: Vec<u64>,
    pub workbook: Box<Workbook>,
}

impl CalculatedSnapshot {
    pub fn apply(mut self, live: &mut Workbook) -> bool {
        let calc = &mut self.workbook;
        if live.date_system != calc.date_system || live.names != calc.names
            || live.calc_manual != calc.calc_manual || live.calc_iterate != calc.calc_iterate
            || live.calc_max_iterations != calc.calc_max_iterations || live.calc_max_change != calc.calc_max_change
            || live.links.len() != calc.links.len()
            || !live.links.iter().zip(&calc.links).all(|(a, b)| {
                std::sync::Arc::ptr_eq(&a.cache.0, &b.cache.0) && a.index == b.index
                    && a.target == b.target && a.abs == b.abs && a.sheets == b.sheets
                    && a.defined_names == b.defined_names
            })
            || live.sheets.len() != self.revisions.len()
            || !live.sheets.iter().zip(&calc.sheets).zip(self.revisions).all(|((sh, result), rev)| {
                sh.identity == result.identity && sh.cell_revision() == rev
                    && sh.name == result.name && sh.tables == result.tables
                    && sh.hidden_rows == result.hidden_rows && sh.hidden_rows_user == result.hidden_rows_user
                    && sh.arrays == result.arrays
            })
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
}

impl DocumentTasks {
    pub fn recalc_pending(&self) -> bool {
        self.needs_full || !self.dirty_recalc_seeds.is_empty()
    }

    pub fn start_save(&mut self, wb: &mut Workbook, target: PathBuf) {
        debug_assert!(self.saving.is_none());
        let mut snapshot = wb.clone();
        // Save As must still inherit parts from the file that was opened.
        snapshot.path = wb.path.clone().filter(|p| p.exists());
        let (tx, rx) = mpsc::channel();
        self.saving = Some(PendingSave { target: target.clone(), rx, started: Instant::now() });
        // Subsequent edits set this back to true. Completion never clears it.
        wb.dirty = false;
        let recalc = self.recalc_pending() && !snapshot.calc_manual;
        std::thread::spawn(move || {
            // Saving or closing can beat the editor's debounce. Finish the
            // captured generation before serializing its formula caches.
            let revisions = recalc.then(|| {
                let revisions = snapshot.sheets.iter().map(|sh| sh.cell_revision()).collect();
                sheetcore::formula::recalc_all(&mut snapshot);
                revisions
            });
            // The core owns the temporary write, history, and atomic replacement.
            let result = sheetcore::xlsx::save_atomic_with_receipt(&target, &snapshot)
                .map(|receipt| (receipt, revisions.map(|revisions| CalculatedSnapshot {
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
        Some(match result {
            Ok((receipt, calculated)) => {
                let applied = !wb.dirty && calculated.is_some_and(|result| result.apply(wb));
                if applied {
                    self.needs_full = false;
                    self.dirty_recalc_seeds.clear();
                }
                receipt.apply_to(wb);
                wb.path = Some(pending.target.clone());
                Ok((pending.target, applied))
            }
            Err(error) => {
                wb.dirty = true;
                Err(error)
            }
        })
    }
}

#[cfg(test)]
#[path = "save_receipt_tests.rs"]
mod tests;
