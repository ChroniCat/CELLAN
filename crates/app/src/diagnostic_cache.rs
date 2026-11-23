use super::SheetApp;
use eframe::egui;
use sheetcore::{Sheet, Workbook, store::CellVersion, table::Table};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

#[derive(Clone, PartialEq)]
struct SheetCells {
    document: u64,
    sheet: u64,
    cells: CellVersion,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheetcore::{Cell, Value};

    fn settle(app: &mut SheetApp, ctx: &egui::Context) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.refresh_diagnostics(ctx);
            if app
                .diagnostics
                .worker
                .as_ref()
                .is_none_or(|w| w.active.is_none())
                && app.diagnostics.pending.is_none()
            {
                break;
            }
            assert!(
                std::time::Instant::now() < until,
                "diagnostics did not finish"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn delayed_marks_and_cycles_cannot_replace_new_inputs_or_a_new_sheet() {
        let _env = crate::test_env::TestEnv::new("delayed-diagnostic-results");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.wb.sheets.push(Sheet::new("Small"));
        let number = || Cell {
            value: Value::Number(8.0),
            formula: None,
        };
        for r in 0..2_000 {
            app.wb.sheets[0].set(r, 0, number());
        }
        app.wb.sheets[0].set(
            0,
            0,
            Cell {
                value: Value::Number(0.0),
                formula: Some("=Target".into()),
            },
        );
        app.wb.names.push(("Target".into(), None, "A1".into()));
        let mut style = app.wb.styles.get(0).clone();
        style.format = "@".into();
        let id = app.wb.styles.intern(style);
        app.wb.sheets[0].set_style(2, 0, id);
        app.ec.background = true;
        app.ec.check_text = true;
        for switch_sheet in [false, true] {
            app.refresh_diagnostics(&ctx);
            let worker = app.diagnostics.worker.as_mut().unwrap();
            let (deliver, temporary) = mpsc::channel();
            let original = std::mem::replace(&mut worker.outputs, temporary);
            let old = original
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(old.cycles, [(0, 0)]);
            assert!(old.marks.contains(&(2, 0)));
            app.wb.sheets[1].set(3, 0, number());
            app.doc_gen += 1;
            app.refresh_diagnostics(&ctx);
            assert!(
                !app.diagnostics
                    .worker
                    .as_ref()
                    .unwrap()
                    .active
                    .as_ref()
                    .unwrap()
                    .1
                    .load(Ordering::Relaxed)
            );
            if switch_sheet {
                app.sheet_idx = 1;
                app.wb.sheets[1].set_style(3, 0, id);
                app.refresh_diagnostics(&ctx); // Small-sheet diagnostics are immediate.
                assert!(app.error_marks.contains(&(3, 0)));
            } else {
                app.wb.names[0].2 = "A2".into();
                app.wb.sheets[0].ignored_errors.insert((2, 0));
                app.doc_gen += 1;
            }
            deliver.send(old).unwrap();
            app.refresh_diagnostics(&ctx);
            assert!(!app.error_marks.contains(&(2, 0)));
            assert!(app.cycles.is_empty());
            if switch_sheet {
                assert!(app.error_marks.contains(&(3, 0)));
            }
            app.diagnostics.worker.as_mut().unwrap().outputs = original;
            settle(&mut app, &ctx);
            if !switch_sheet {
                app.wb.names[0].2 = "A1".into();
                app.wb.sheets[0].ignored_errors.clear();
                app.doc_gen += 1;
            }
        }
    }

    #[test]
    fn diagnostics_ignore_other_sheet_edits_and_follow_name_and_mark_inputs() {
        let _env = crate::test_env::TestEnv::new("scoped-diagnostics");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.wb.sheets.push(Sheet::new("Other"));
        for i in 0..1_600 {
            app.wb.sheets[0].set(
                i,
                0,
                Cell {
                    value: Value::Number(8.0),
                    formula: None,
                },
            );
        }
        app.wb.sheets[0].set(
            0,
            0,
            Cell {
                value: Value::Number(1.0),
                formula: Some("=Target".into()),
            },
        );
        app.wb.names.push(("Target".into(), None, "A1".into()));
        app.ec.background = true;
        app.ec.check_text = true;
        settle(&mut app, &ctx);
        assert_eq!(app.cycles, [(0, 0)]);
        app.wb.sheets[1].set(
            0,
            0,
            Cell {
                value: Value::Number(9.0),
                formula: None,
            },
        );
        app.tasks.last_edit = Some(std::time::Instant::now());
        app.doc_gen += 1;
        settle(&mut app, &ctx);
        assert!(app.diagnostics.pending.is_none());
        assert_eq!(
            app.diagnostics.seen.as_ref().unwrap().document_generation.0,
            app.doc_gen
        );
        app.wb.names[0].2 = "A2".into();
        app.doc_gen += 1;
        app.refresh_diagnostics(&ctx);
        assert!(app.diagnostics.pending.is_some());
        app.tasks.last_edit = None;
        settle(&mut app, &ctx);
        assert!(app.cycles.is_empty());
        let mut style = app.wb.styles.get(0).clone();
        style.format = "@".into();
        let style = app.wb.styles.intern(style);
        app.wb.sheets[0].set_style(2, 0, style);
        settle(&mut app, &ctx);
        assert!(app.error_marks.contains(&(2, 0)));
        app.wb.sheets[0].ignored_errors.insert((2, 0));
        settle(&mut app, &ctx);
        assert!(!app.error_marks.contains(&(2, 0)));
    }
}

#[derive(PartialEq)]
struct Observed {
    source: SheetCells,
    document_generation: (u64, u64),
    styles: u64,
    mark_options: (bool, bool, bool),
    ignored_count: usize,
}

struct Marks {
    source: SheetCells,
    styles: u64,
    options: (bool, bool),
    ignored: sheetcore::model::CellSet<(u32, u32)>,
}

struct Cycles {
    source: SheetCells,
    names: Vec<(String, Option<usize>, String)>,
    sheets: Vec<(String, Vec<Table>)>,
}

impl Cycles {
    fn matches(&self, source: &SheetCells, wb: &Workbook) -> bool {
        self.source == *source
            && self.names == wb.names
            && self.sheets.len() == wb.sheets.len()
            && self
                .sheets
                .iter()
                .zip(&wb.sheets)
                .all(|((name, tables), s)| *name == s.name && *tables == s.tables)
    }
}

impl Marks {
    fn matches(&self, observed: &Observed, sheet: &Sheet) -> bool {
        self.source == observed.source
            && self.styles == observed.styles
            && observed.mark_options == (true, self.options.0, self.options.1)
            && self.ignored == sheet.ignored_errors
    }
}

const SYNC_LIMIT: usize = 1_500;

#[derive(Default)]
struct Results {
    marks: HashSet<(u32, u32)>,
    cycles: Vec<(u32, u32)>,
    mark_key: Option<Marks>,
    cycle_key: Option<Cycles>,
}

struct Scan {
    book: Workbook,
    sheet: usize,
    source: SheetCells,
    styles: u64,
    marks: Option<(bool, bool)>,
    cycles: bool,
    stop: Arc<AtomicBool>,
    wake: egui::Context,
}

impl Scan {
    fn run(&mut self) -> Results {
        let mut result = Results::default();
        if let Some(options) = self.marks {
            result.marks = sheetcore::errors::mark_positions(
                &self.book, self.sheet, options.0, options.1, &self.stop,
            );
            result.mark_key = Some(Marks {
                source: self.source.clone(),
                styles: self.styles,
                options,
                ignored: std::mem::take(&mut self.book.sheets[self.sheet].ignored_errors),
            });
        }
        if self.cycles && !self.stop.load(Ordering::Relaxed) {
            result.cycles =
                sheetcore::errors::find_cycles_cancellable(&self.book, self.sheet, &self.stop);
            result.cycle_key = Some(Cycles {
                source: self.source.clone(),
                names: std::mem::take(&mut self.book.names),
                sheets: self
                    .book
                    .sheets
                    .iter_mut()
                    .map(|s| (std::mem::take(&mut s.name), std::mem::take(&mut s.tables)))
                    .collect(),
            });
        }
        result
    }
}

enum Task {
    Scan(Box<Scan>),
    Dispose(Results),
}

struct Worker {
    input: mpsc::Sender<Task>,
    outputs: mpsc::Receiver<Results>,
    active: Option<(SheetCells, Arc<AtomicBool>)>,
}

impl Worker {
    fn new() -> Self {
        let (input, jobs) = mpsc::channel();
        let (send, outputs) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(job) = jobs.recv() {
                match job {
                    Task::Dispose(old) => drop(old),
                    Task::Scan(mut job) => {
                        let mut out = job.run();
                        if job.stop.load(Ordering::Relaxed) {
                            out = Results::default();
                        }
                        let wake = job.wake.clone();
                        drop(job);
                        if send.send(out).is_err() {
                            break;
                        }
                        wake.request_repaint();
                    }
                }
            }
        });
        Self {
            input,
            outputs,
            active: None,
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some((_, stop)) = &self.active {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Default)]
pub(super) struct Diagnostics {
    seen: Option<Observed>,
    marks: Option<Marks>,
    cycles: Option<Cycles>,
    pending: Option<(SheetCells, std::time::Instant)>,
    worker: Option<Worker>,
}

impl Diagnostics {
    pub(super) fn reset_marks(&mut self) {
        self.seen = None;
    }
}

impl SheetApp {
    pub(super) fn refresh_diagnostics(&mut self, ctx: &egui::Context) {
        let sheet: &Sheet = self.wb.sheet(self.sheet_idx);
        let source = SheetCells {
            document: self.doc_uid,
            sheet: sheet.identity,
            cells: sheet.cell_version().clone(),
        };
        let styles = sheet.display_style_revision();
        let options = (self.ec.check_text, self.ec.check_errors);
        let observed = Observed {
            source: source.clone(),
            document_generation: (self.doc_gen, self.wb_epoch),
            styles,
            mark_options: (self.ec.background, options.0, options.1),
            ignored_count: sheet.ignored_errors.len(),
        };
        self.poll_diagnostic_worker(&observed);
        if self.diagnostics.seen.as_ref() == Some(&observed) {
            return;
        }
        if self
            .diagnostics
            .marks
            .as_ref()
            .is_some_and(|m| m.source.document != source.document || m.source.sheet != source.sheet)
            || self.diagnostics.cycles.as_ref().is_some_and(|c| {
                c.source.document != source.document || c.source.sheet != source.sheet
            })
        {
            self.retire_diagnostics();
        }
        let sheet = self.wb.sheet(self.sheet_idx);
        // Completion wakes the UI. Its result is checked against live metadata,
        // so idle frames need no repeated name/format comparisons while it runs.
        if sheet.len() > SYNC_LIMIT && self.diagnostics.worker.as_ref().is_some_and(|w| w.active.is_some()) {
            return;
        }
        let need_marks = self.ec.background
            && self
                .diagnostics
                .marks
                .as_ref()
                .is_none_or(|m| !m.matches(&observed, sheet));
        let need_cycles = self
            .diagnostics
            .cycles
            .as_ref()
            .is_none_or(|c| !c.matches(&source, &self.wb));
        let recent_edit = self
            .tasks
            .last_edit
            .is_some_and(|t| t.elapsed().as_millis() < 1_200);
        if (need_marks || need_cycles)
            && self
                .diagnostics
                .pending
                .as_ref()
                .is_none_or(|(p, _)| *p != source)
        {
            self.diagnostics.pending = Some((source.clone(), std::time::Instant::now()));
        }
        let pending_ready = self
            .diagnostics
            .pending
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed().as_millis() >= 1_200);
        let idle = sheet.len() <= SYNC_LIMIT
            || (self.editing.is_none() && (!recent_edit || pending_ready));
        if (need_marks || need_cycles) && !idle {
            ctx.request_repaint_after(std::time::Duration::from_millis(650));
            return;
        }
        if (need_marks || need_cycles) && sheet.len() > SYNC_LIMIT {
            let mut book = Workbook::new_blank();
            let si = if need_cycles { self.sheet_idx } else { 0 };
            if need_cycles {
                book.names = self.wb.names.clone();
                book.sheets = self
                    .wb
                    .sheets
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let mut view = if i == si {
                            s.search_snapshot()
                        } else {
                            Sheet::new(&s.name)
                        };
                        view.tables = s.tables.clone();
                        view
                    })
                    .collect();
            } else {
                book.sheets = vec![sheet.search_snapshot()];
            }
            if need_marks {
                book.styles = self.wb.styles.clone();
                book.sheets[si].ignored_errors = sheet.ignored_errors.clone();
            }
            let stop = Arc::new(AtomicBool::new(false));
            let scan = Scan {
                book,
                sheet: si,
                source: source.clone(),
                styles,
                marks: need_marks.then_some(options),
                cycles: need_cycles,
                stop: stop.clone(),
                wake: ctx.clone(),
            };
            let worker = self.diagnostics.worker.get_or_insert_with(Worker::new);
            if worker.input.send(Task::Scan(Box::new(scan))).is_ok() {
                worker.active = Some((source, stop));
            } else {
                self.diagnostics.worker = None;
                ctx.request_repaint();
            }
            return;
        }
        let mut old = Results::default();
        if need_marks {
            let positions = sheetcore::errors::mark_positions(
                &self.wb,
                self.sheet_idx,
                options.0,
                options.1,
                &AtomicBool::new(false),
            );
            old.marks = std::mem::replace(&mut self.error_marks, positions);
            old.mark_key = self.diagnostics.marks.replace(Marks {
                source: source.clone(),
                styles,
                options,
                ignored: sheet.ignored_errors.clone(),
            });
        }
        if need_cycles {
            old.cycles = std::mem::replace(
                &mut self.cycles,
                sheetcore::errors::find_cycles(&self.wb, self.sheet_idx),
            );
            old.cycle_key = self.diagnostics.cycles.replace(Cycles {
                source,
                names: self.wb.names.clone(),
                sheets: self
                    .wb
                    .sheets
                    .iter()
                    .map(|s| (s.name.clone(), s.tables.clone()))
                    .collect(),
            });
        }
        self.dispose_large_diagnostics(old);
        self.diagnostics.seen = Some(observed);
        self.diagnostics.pending = None;
    }

    fn poll_diagnostic_worker(&mut self, observed: &Observed) {
        let Some(worker) = self.diagnostics.worker.as_mut() else {
            return;
        };
        let Some((source, stop)) = &worker.active else {
            return;
        };
        if *source != observed.source {
            stop.store(true, Ordering::Relaxed);
        }
        let mut out = match worker.outputs.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.diagnostics.worker = None;
                return;
            }
        };
        let cancelled = stop.load(Ordering::Relaxed);
        worker.active = None;
        if !cancelled {
            if out
                .mark_key
                .as_ref()
                .is_some_and(|m| m.matches(observed, self.wb.sheet(self.sheet_idx)))
            {
                std::mem::swap(&mut out.marks, &mut self.error_marks);
                std::mem::swap(&mut out.mark_key, &mut self.diagnostics.marks);
            }
            if out
                .cycle_key
                .as_ref()
                .is_some_and(|c| c.matches(&observed.source, &self.wb))
            {
                std::mem::swap(&mut out.cycles, &mut self.cycles);
                std::mem::swap(&mut out.cycle_key, &mut self.diagnostics.cycles);
            }
        }
        // The same worker drops rejected and replaced large results before it
        // accepts the next scan. At most one scan is outstanding per app.
        let _ = worker.input.send(Task::Dispose(out));
    }

    fn retire_diagnostics(&mut self) {
        let old = Results {
            marks: std::mem::take(&mut self.error_marks),
            cycles: std::mem::take(&mut self.cycles),
            mark_key: self.diagnostics.marks.take(),
            cycle_key: self.diagnostics.cycles.take(),
        };
        self.dispose_large_diagnostics(old);
        self.diagnostics.seen = None;
    }

    fn dispose_large_diagnostics(&self, old: Results) {
        if old.marks.capacity() > SYNC_LIMIT
            || old.cycles.capacity() > SYNC_LIMIT
            || old
                .mark_key
                .as_ref()
                .is_some_and(|m| m.ignored.len() > SYNC_LIMIT)
            || old
                .cycle_key
                .as_ref()
                .is_some_and(|c| c.names.len() > SYNC_LIMIT)
        {
            if let Some(w) = &self.diagnostics.worker {
                let _ = w.input.send(Task::Dispose(old));
            }
        }
    }
}
