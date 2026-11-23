//! Plan large sorts and duplicate removal on a read-only snapshot. Recheck the
//! document and the full selection's permissions before applying one undoable edit.

use super::SheetApp;
use sheetcore::{Sheet, data_tools, sort_filter};
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[test]
fn dense_numeric_sort_keeps_revision_count_and_replays_through_app() {
    let env = crate::test_env::TestEnv::new("dense_numeric_sort_history");
    let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
    app.wb.path = Some(env.path("numbers.xlsx"));
    let sheet = &mut app.wb.sheets[0];
    for row in 0..1000 {
        for col in 0..101 {
            sheet.set(row, col, sheetcore::Cell {
                value: sheetcore::Value::Number((1000 - row + col) as f64), formula: None,
            });
        }
    }
    app.sort_region((0, 0, 999, 100), 0, true);
    let start = std::time::Instant::now();
    while app.tasks.reorder.is_some() {
        app.poll_reorder();
        assert!(start.elapsed().as_secs() < 30);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let log = app.rev.last().unwrap();
    assert_eq!(log.cells.len(), sheetcore::revision::MAX_CELLS);
    assert!(log.note.contains("101000"));
    assert_eq!(app.wb.sheets[0].value(0, 0), sheetcore::Value::Number(1.0));
    assert!(app.apply_undo(false));
    assert_eq!(app.wb.sheets[0].value(0, 0), sheetcore::Value::Number(1000.0));
    assert!(app.apply_undo(true));
    assert_eq!(app.wb.sheets[0].value(0, 0), sheetcore::Value::Number(1.0));
}

#[test]
fn pending_sort_does_not_cross_room_boundaries() {
    let _env = crate::test_env::TestEnv::new("sort_room_change");
    let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
    let number = |n| sheetcore::Cell { value: sheetcore::Value::Number(n), formula: None };
    app.wb.sheets[0].set(0,0,number(2.));
    app.wb.sheets[0].set(1,0,number(1.));
    for joining in [true,false] {
        app.sort_region((0,0,20_000,5),0,true);
        assert!(app.tasks.reorder.is_some());
        if joining {
            app.net = Some(crate::net::host_for_test(91,"test","sort-room","test").0);
        } else {
            app.leave_session_at(None);
        }
        let start = std::time::Instant::now();
        while app.tasks.reorder.is_some() {
            app.poll_reorder();
            assert!(start.elapsed().as_secs() < 30);
            std::thread::yield_now();
        }
        assert_eq!(app.wb.sheets[0].get(0,0),Some(&number(2.)));
        assert_eq!(app.undo.depth(),(0,0));
    }
}

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
    target: super::sheet_edit_target::SheetEditTarget,
    rect: Rect,
    operation: std::sync::Arc<Operation>,
    rx: Receiver<Result<(Sheet, Sheet, PreparedChanges), String>>,
}

enum PreparedChanges {
    Cells(data_tools::RowChanges),
    Permutation {
        order: data_tools::CompactSort,
        total: usize,
        sample: Vec<sheetcore::undo::CellDelta>,
    },
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
            let result = (|| {
                let mut prepared = snapshot.clone();
                let sort = match worker_op.as_ref() {
                    Operation::Rows { col, asc } => {
                        let (r1,c1,r2,c2) = rect;
                        let header = sort_filter::has_header(&snapshot,r1,c1,r2,c2);
                        Some(((r1 + u32::from(header),c1,r2,c2),*col,*asc,false))
                    }
                    Operation::Columns { row, asc } => {
                        sort_filter::validate_sort_region(&snapshot,rect)?;
                        let (r1,c1,r2,c2) = rect;
                        let header = sort_filter::has_header_col(&snapshot,r1,c1,r2,c2);
                        Some(((r1,c1 + u32::from(header),r2,c2),*row,*asc,true))
                    }
                    _ => None,
                };
                if let Some((data,key,asc,horizontal)) = sort {
                    if let Some(order) = data_tools::CompactSort::plan(&snapshot,index,data,key,asc,horizontal)? {
                        let mut total = 0;
                        let mut sample = Vec::new();
                        for d in order.deltas() {
                            if d.before != d.after { prepared.set(d.row, d.col, d.after.clone()); }
                            if d.before_style != d.after_style { prepared.set_style(d.row, d.col, d.after_style); }
                            if total < sheetcore::revision::MAX_CELLS { sample.push(d); }
                            total += 1;
                        }
                        return Ok((snapshot, prepared, PreparedChanges::Permutation { order, total, sample }));
                    }
                }
                let changes = worker_op.plan(&snapshot, index, rect)?;
                changes.apply(&mut prepared);
                Ok((snapshot, prepared, PreparedChanges::Cells(changes)))
            })();
            let _ = tx.send(result);
        });
        self.tasks.reorder = Some(Pending {
            sheet: index,
            target: super::sheet_edit_target::SheetEditTarget::current(self,self.sel),
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
        if !pending.target.valid(self)
            || !current.matches_reorder_source(&source)
        {
            self.status = "工作表已变更，请重新执行数据操作".into();
            return;
        }
        if self.preflight_reorder(pending.rect, &pending.operation) {
            match changes {
                PreparedChanges::Cells(changes) => {
                    self.finish_reorder(&pending.operation, Ok(changes), Some(&mut prepared));
                }
                PreparedChanges::Permutation { order, total, sample } => {
                    let operation = &pending.operation;
                    self.status = operation.completed(order.moved_lines());
                    if total == 0 { return; }
                    self.wb.sheet_mut(pending.sheet).take_row_changes(&mut prepared);
                    self.wb.dirty = true;
                    self.doc_gen = self.doc_gen.wrapping_add(1);
                    self.recalc_all_and_deps_adaptive();
                    if self.net.is_some() {
                        let mut rows = order.deltas();
                        loop {
                            let chunk: Vec<_> = rows.by_ref().take(sheetcore::sync::LIVE_OP_BATCH).collect();
                            if chunk.is_empty() { break; }
                            self.broadcast_sheet_deltas(pending.sheet, &chunk);
                        }
                    }
                    self.record_revision_sample(operation.label(), &sample, total);
                    self.undo.push_compact_sort(operation.label(), order, (pending.sheet, self.sel.0, self.sel.1));
                    self.tasks.last_edit = Some(std::time::Instant::now());
                }
            }
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
