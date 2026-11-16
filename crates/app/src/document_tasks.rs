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
    pub last_edit: Option<Instant>,
    pub saving: Option<PendingSave>,
    pub pending_host: bool,
    pub pending_join: Option<String>,
    /// 后台全量重算的取消标志与进度（QA-07.3）：Esc 置位，状态栏读计数
    pub recalc_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub recalc_done: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    pub recalc_total: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
}

pub(crate) struct PendingSave {
    pub target: PathBuf,
    pub rx: Receiver<Result<SaveReceipt, String>>,
    /// 开始保存的时刻，状态栏据此显示已用时间（QA-07.3 进度反馈）
    pub started: Instant,
}

impl DocumentTasks {
    pub fn start_save(&mut self, wb: &mut Workbook, target: PathBuf) {
        debug_assert!(self.saving.is_none());
        let mut snapshot = wb.clone();
        // Save As must still inherit parts from the file that was opened.
        snapshot.path = wb.path.clone().filter(|p| p.exists());
        let (tx, rx) = mpsc::channel();
        self.saving = Some(PendingSave { target: target.clone(), rx, started: Instant::now() });
        // Subsequent edits set this back to true. Completion never clears it.
        wb.dirty = false;
        std::thread::spawn(move || {
            // The core owns the temporary write, history, and atomic replacement.
            let result = sheetcore::xlsx::save_atomic_with_receipt(&target, &snapshot)
                .map_err(|e| format!("保存失败: {e:#}"));
            let _ = tx.send(result);
        });
    }

    pub fn poll_save(&mut self, wb: &mut Workbook) -> Option<Result<PathBuf, String>> {
        let pending = self.saving.as_ref()?;
        let result = match pending.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("保存线程意外退出，改动尚未保存".into()),
        };
        let pending = self.saving.take().unwrap();
        Some(match result {
            Ok(receipt) => {
                receipt.apply_to(wb);
                wb.path = Some(pending.target.clone());
                Ok(pending.target)
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
