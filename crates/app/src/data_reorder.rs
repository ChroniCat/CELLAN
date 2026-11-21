//! Plan large sorts and duplicate removal on a read-only snapshot. Recheck the
//! document and the full selection's permissions before applying one undoable edit.

use super::SheetApp;
use sheetcore::{Sheet, data_tools, sort_filter};
use std::sync::mpsc::{self, Receiver, TryRecvError};

type Rect = (u32, u32, u32, u32);

pub(super) enum Operation {
    Rows {
        col: u32,
        asc: bool,
    },
    Columns {
        row: u32,
        asc: bool,
    },
    Multi {
        keys: Vec<data_tools::SortKey>,
        header: bool,
    },
    Dedup {
        cols: Vec<u32>,
        header: bool,
    },
}

impl Operation {
    fn label(&self) -> &'static str {
        match self {
            Self::Rows { .. } => "排序",
            Self::Columns { .. } => "按行排序",
            Self::Multi { .. } => "多级排序",
            Self::Dedup { .. } => "删除重复",
        }
    }

    fn plan(
        &self,
        sheet: &Sheet,
        index: usize,
        rect: Rect,
    ) -> Result<data_tools::RowChanges, String> {
        let (r1, c1, r2, c2) = rect;
        match self {
            Self::Rows { col, asc } => {
                let header = sort_filter::has_header(sheet, r1, c1, r2, c2);
                sort_filter::plan_sort_axis(
                    sheet,
                    index,
                    (r1 + u32::from(header), c1, r2, c2),
                    *col,
                    *asc,
                    false,
                )
            }
            Self::Columns { row, asc } => {
                sort_filter::validate_sort_region(sheet, rect)?;
                let header = sort_filter::has_header_col(sheet, r1, c1, r2, c2);
                sort_filter::plan_sort_axis(
                    sheet,
                    index,
                    (r1, c1 + u32::from(header), r2, c2),
                    *row,
                    *asc,
                    true,
                )
            }
            Self::Multi { keys, header } => {
                sort_filter::validate_sort_region(sheet, rect)?;
                data_tools::plan_sort_multi(sheet, index, rect, keys, *header)
            }
            Self::Dedup { cols, header } => Ok(data_tools::plan_remove_duplicates(
                sheet, index, rect, cols, *header,
            )),
        }
    }

    fn completed(&self, count: usize) -> String {
        match self {
            Self::Rows { col, asc } => format!(
                "已按 {} 列{}排序",
                sheetcore::model::cell_name(0, *col),
                if *asc { "升序" } else { "降序" }
            ),
            Self::Columns { row, .. } => format!("已按第 {} 行排序（重排列）", row + 1),
            Self::Multi { keys, .. } => format!("已按 {} 个键排序（{} 行移动）", keys.len(), count),
            Self::Dedup { .. } if count == 0 => "没有重复行".into(),
            Self::Dedup { .. } => format!("删除了 {count} 行重复"),
        }
    }
}

pub(crate) struct Pending {
    sheet: usize,
    doc_gen: u64,
    doc_uid: u64,
    rect: Rect,
    operation: std::sync::Arc<Operation>,
    rx: Receiver<Result<(Sheet, Sheet, data_tools::RowChanges), String>>,
}

impl SheetApp {
    /// The undo guard checks the cell operation's site even for a format-only edit.
    /// Send every touched cell, including empty clears, and only the formats that changed.
    pub(super) fn broadcast_sheet_deltas(
        &mut self,
        sheet: usize,
        deltas: &[sheetcore::undo::CellDelta],
    ) {
        if self.net.is_none() {
            return;
        }
        for chunk in deltas.chunks(sheetcore::sync::LIVE_OP_BATCH) {
            let keys: Vec<_> = chunk.iter().map(|d| (sheet, d.row, d.col)).collect();
            self.broadcast_cells(&keys);
            let styles: Vec<_> = chunk
                .iter()
                .filter(|d| d.before_style != d.after_style)
                .map(|d| (d.row, d.col, self.wb.styles.get(d.after_style).clone()))
                .collect();
            if let Some(net) = &self.net {
                net.broadcast_style(sheet, styles);
            }
        }
    }

    fn preflight_reorder(&mut self, rect: Rect, operation: &Operation) -> bool {
        let (r1, c1, r2, c2) = rect;
        let label = operation.label();
        let op = matches!(operation, Operation::Rows { .. })
            .then_some(sheetcore::protection::ProtOp::Sort);
        if !self.write_preflight_on_op(self.sheet_idx, &[], label, op) {
            return false;
        }
        if op.is_none()
            && self.wb.sheet(self.sheet_idx).protection.is_some()
            && (r1..=r2).any(|r| (c1..=c2).any(|c| self.cell_locked_on(self.sheet_idx, r, c)))
        {
            self.status = format!("工作表受保护：目标区域含锁定单元格，不能{label}");
            return false;
        }
        let conflict = self
            .net
            .as_ref()
            .and_then(|n| n.host_lock_conflict_rect(self.sheet_idx, rect))
            .map(|(_, name, r, c)| (name, r, c))
            .or_else(|| {
                let site = self.net.as_ref().map_or(0, |n| n.site);
                self.locks
                    .conflicts_rect(self.sheet_idx, r1, c1, r2, c2, site, self.lock_now())
                    .map(|((r, c), h)| (h.name.clone(), r, c))
            });
        if let Some((name, r, c)) = conflict {
            self.status = format!(
                "{name} 正在编辑 {}，不能{label}",
                sheetcore::model::cell_name(r, c)
            );
            return false;
        }
        true
    }

    pub(super) fn run_reorder(&mut self, rect: Rect, operation: Operation) {
        if self.tasks.reorder.is_some() {
            self.status = "正在计算上一项数据操作，请稍候".into();
            return;
        }
        if !self.preflight_reorder(rect, &operation) {
            return;
        }
        let sheet = self.wb.sheet(self.sheet_idx);
        let area = u64::from(rect.2 - rect.0 + 1) * u64::from(rect.3 - rect.1 + 1);
        if area <= 100_000 {
            let changes = operation.plan(sheet, self.sheet_idx, rect);
            self.finish_reorder(&operation, changes, None);
            return;
        }
        let snapshot = sheet.reorder_snapshot();
        let index = self.sheet_idx;
        self.status = format!("正在计算{}…", operation.label());
        let (tx, rx) = mpsc::channel();
        let op = std::sync::Arc::new(operation);
        let worker_op = op.clone();
        std::thread::spawn(move || {
            let result = worker_op.plan(&snapshot, index, rect).map(|changes| {
                let mut prepared = snapshot.clone();
                changes.apply(&mut prepared);
                (snapshot, prepared, changes)
            });
            let _ = tx.send(result);
        });
        self.tasks.reorder = Some(Pending {
            sheet: index,
            doc_gen: self.doc_gen,
            doc_uid: self.doc_uid,
            rect,
            operation: op,
            rx,
        });
    }

    pub(super) fn poll_reorder(&mut self) {
        let Some(pending) = &self.tasks.reorder else {
            return;
        };
        let result = match pending.rx.try_recv() {
            Err(TryRecvError::Empty) => return,
            result => result,
        };
        let pending = self.tasks.reorder.take().unwrap();
        let (source, mut prepared, changes) = match result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                self.status = error;
                return;
            }
            Err(_) => {
                self.status = "数据操作未完成，请重试".into();
                return;
            }
        };
        let current = self.wb.sheet(self.sheet_idx);
        if self.sheet_idx != pending.sheet
            || self.doc_gen != pending.doc_gen
            || self.doc_uid != pending.doc_uid
            || !current.matches_reorder_source(&source)
        {
            self.status = "工作表已变更，请重新执行数据操作".into();
            return;
        }
        if self.preflight_reorder(pending.rect, &pending.operation) {
            self.finish_reorder(&pending.operation, Ok(changes), Some(&mut prepared));
        }
    }

    fn finish_reorder(
        &mut self,
        operation: &Operation,
        changes: Result<data_tools::RowChanges, String>,
        prepared: Option<&mut Sheet>,
    ) {
        let changes = match changes {
            Err(error) => {
                self.status = error;
                return;
            }
            Ok(changes) => changes,
        };
        let message = operation.completed(changes.count);
        if !changes.deltas.is_empty() {
            let sheet = self.wb.sheet_mut(self.sheet_idx);
            if let Some(prepared) = prepared {
                sheet.take_row_changes(prepared);
            } else {
                changes.apply(sheet);
            }
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.recalc_all_and_deps_adaptive();
            self.broadcast_sheet_deltas(self.sheet_idx, &changes.deltas);
            self.record_deltas(operation.label(), changes.deltas);
            self.tasks.last_edit = Some(std::time::Instant::now());
        }
        self.status = message;
    }
}
