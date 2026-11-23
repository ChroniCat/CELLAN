//! Large structural edits reserve their stack position before their immutable snapshots are
//! compressed. A single worker and one queued job bound transient retention across workbooks.
use super::{StructureSnapshot, cell_heap_bytes, structural_cells::CellHistory};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc};

const ASYNC_CELLS: usize = 16_384;

pub(super) struct Snapshots {
    before: StructureSnapshot,
    after: StructureSnapshot,
}

impl Snapshots {
    fn compact(&mut self) -> usize {
        let (before, after) = (&mut self.before, &mut self.after);
        let (undo, redo) =
            CellHistory::pair(&before.target, &after.target, &before.shift, &after.shift);
        let bytes = undo.bytes()
            + redo.bytes()
            + before.reference_uses.bytes()
            + after.reference_uses.bytes()
            + before
                .foreign
                .iter()
                .chain(&after.foreign)
                .map(|f| {
                    std::mem::size_of_val(f.cells.as_slice())
                        + f.cells
                            .iter()
                            .map(|(_, c)| cell_heap_bytes(c))
                            .sum::<usize>()
                })
                .sum::<usize>();
        before.compact = Some(undo);
        after.compact = Some(redo);
        for side in [before, after] {
            side.target.replace_cell_store(Default::default());
            side.target.source_cells = None;
            side.target.saved_cells = None;
            side.target.shared_lookup = Default::default();
        }
        bytes
    }
}

pub(super) struct Job {
    snapshots: Mutex<Option<Box<Snapshots>>>,
    bytes: OnceLock<usize>,
    finished: Condvar,
}

impl Job {
    fn run(&self) {
        let mut slot = self.snapshots.lock().unwrap();
        let snapshots = slot.as_mut().expect("new compression job");
        // Failure to encode a legacy snapshot must never strand a waiting undo. Until both
        // histories exist the original snapshots are intact and support the same replay guard.
        let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| snapshots.compact()))
            .unwrap_or(usize::MAX);
        self.bytes.set(bytes).expect("one worker per job");
        self.finished.notify_all();
    }

    fn result(&self, wait: bool) -> Option<(Box<Snapshots>, usize)> {
        if !wait && self.bytes.get().is_none() {
            return None;
        }
        let slot = self.snapshots.lock().unwrap();
        let mut slot = self
            .finished
            .wait_while(slot, |_| self.bytes.get().is_none())
            .unwrap();
        Some((slot.take()?, *self.bytes.get()?))
    }
}

pub(super) enum History {
    Ready(Box<Snapshots>),
    Pending(Arc<Job>),
}

impl History {
    pub fn record(before: StructureSnapshot, after: StructureSnapshot) -> (Self, usize) {
        let cells = before.target.len().max(after.target.len());
        let mut snapshots = Box::new(Snapshots { before, after });
        if cells < ASYNC_CELLS {
            let bytes = snapshots.compact();
            return (Self::Ready(snapshots), bytes);
        }
        type Queue = mpsc::SyncSender<Weak<Job>>;
        static WORKER: OnceLock<Option<Queue>> = OnceLock::new();
        let queue = WORKER.get_or_init(|| {
            let (send, recv) = mpsc::sync_channel::<Weak<Job>>(1);
            std::thread::Builder::new()
                .name("structural-undo".into())
                .spawn(move || {
                    for ticket in recv {
                        if let Some(job) = ticket.upgrade() {
                            job.run();
                        }
                    }
                })
                .ok()
                .map(|_| send)
        });
        let job = Arc::new(Job {
            snapshots: Mutex::new(Some(snapshots)),
            bytes: OnceLock::new(),
            finished: Condvar::new(),
        });
        // A burst beyond the bounded queue applies backpressure instead of retaining
        // indefinitely many large pre/post-edit grids. Evicted jobs have only weak tickets.
        if !queue
            .as_ref()
            .is_some_and(|q| q.send(Arc::downgrade(&job)).is_ok())
        {
            job.run(); // Also preserve undo if the OS cannot start a worker.
        }
        (Self::Pending(job), 0)
    }

    pub fn pending(&self) -> bool {
        matches!(self, Self::Pending(_))
    }

    pub fn settle(&mut self, wait: bool) -> Option<usize> {
        let Self::Pending(job) = self else {
            return None;
        };
        let (snapshots, bytes) = job.result(wait)?;
        *self = Self::Ready(snapshots);
        Some(bytes)
    }

    pub fn snapshots(&self) -> Option<(&StructureSnapshot, &StructureSnapshot)> {
        match self {
            Self::Ready(s) => Some((&s.before, &s.after)),
            Self::Pending(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Cell, Value, Workbook,
        structural::{Structural, apply_structural_wb},
        undo::{UndoStack, snapshot_structure},
    };

    fn record(stack: &mut UndoStack, wb: &mut Workbook, op: Structural) {
        let before = snapshot_structure(wb, 0, &op);
        apply_structural_wb(wb, 0, &op);
        let after = before.capture_after(wb);
        stack.push_structure(op.label(), before, after, (0, 0, 0));
    }

    #[test]
    fn large_consecutive_edits_replay_in_stack_order_and_evicted_jobs_stay_evicted() {
        let mut wb = Workbook::new_blank();
        for row in 0..=ASYNC_CELLS as u32 {
            wb.sheets[0].set(
                row,
                0,
                Cell {
                    value: Value::Number(f64::from(row)),
                    formula: None,
                },
            );
        }
        let mut stack = UndoStack::new();
        stack.limit = 2;
        record(&mut stack, &mut wb, Structural::InsertRows { at: 1, n: 1 });
        record(&mut stack, &mut wb, Structural::DeleteRows { at: 3, n: 1 });
        record(&mut stack, &mut wb, Structural::InsertCols { at: 0, n: 1 });
        let allow = |_: usize, _: u32, _: u32, _: &Cell| true;
        assert_eq!(stack.depth(), (2, 0));
        stack.undo_workbook(&mut wb, allow).unwrap();
        assert_eq!(wb.sheets[0].value(3, 0), Value::Number(3.));
        stack.undo_workbook(&mut wb, allow).unwrap();
        assert_eq!(wb.sheets[0].value(3, 0), Value::Number(2.));
        assert!(wb.sheets[0].get(1, 0).is_none());
        assert!(!stack.can_undo());
        stack.redo_workbook(&mut wb, allow).unwrap();
        stack.redo_workbook(&mut wb, allow).unwrap();
        assert_eq!(wb.sheets[0].value(3, 1), Value::Number(3.));
        record(&mut stack, &mut wb, Structural::DeleteRows { at: 0, n: 2 });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while stack.poll_compaction() {
            assert!(
                std::time::Instant::now() < deadline,
                "background compression did not complete"
            );
            std::thread::yield_now();
        }
        let retained = stack.done.last().unwrap().stamp().payload_bytes;
        assert!(retained >= std::mem::size_of::<(crate::CellRef, Cell)>());
        stack.trim_history(retained - 1);
        assert_eq!(stack.depth(), (1, 0)); // Keep the newest action, now using its actual retained payload.
        record(&mut stack, &mut wb, Structural::InsertRows { at: 1, n: 1 });
        stack.set_site(42);
        assert!(!stack.poll_compaction());
        assert_eq!(stack.depth(), (0, 0));
    }
}
