use super::SheetApp;
use eframe::egui;
use sheetcore::{dv, formula, Value, Workbook};
use std::collections::BTreeSet;
use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc};

#[derive(Default)]
struct Circles {
    cells: BTreeSet<(u32, u32)>,
    config_error: Option<String>,
}

pub(super) struct InvalidJob {
    view: (u64, u64, u64),
    generation: u64,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<Circles>,
}

impl Drop for InvalidJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheetcore::{Cell, Sheet};

    fn text(value: &str) -> Cell {
        Cell { value: Value::Text(value.into()), formula: None }
    }

    fn poll_until_checked(app: &mut SheetApp, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            app.refresh_invalid(ctx);
            if app.invalid_job.is_none() && app.invalid_gen == app.validation_gen { break; }
            assert!(std::time::Instant::now() < deadline, "validation worker did not complete");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn circles_discard_stale_sources_and_follow_sheet_switches() {
        let _env = crate::test_env::TestEnv::new("circles-revision");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        let mut list = Sheet::new("List");
        for row in 0..4_000 { list.set(row, 1, text("unrelated")); }
        list.set(0, 0, text("old"));
        app.wb.sheets.push(list);
        app.wb.sheets[0].dv.push(dv::DataValidation {
            ranges: vec![(0, 0, 1_048_575, 0)],
            kind: dv::DvKind::List, f1: "List!$A:$A".into(), ..Default::default()
        });
        app.wb.sheets[0].set(900_000, 0, text("old"));
        app.show_invalid(&ctx);
        assert!(app.invalid_job.is_some());
        // Cached formula results can change with no user edit generation.
        app.wb.sheets[1].get_mut(0, 0).unwrap().value = Value::Text("new".into());
        poll_until_checked(&mut app, &ctx);
        assert!(app.invalid_cells.contains(&(900_000, 0)));
        app.sheet_idx = 1;
        poll_until_checked(&mut app, &ctx);
        assert!(app.invalid_cells.is_empty());
        app.sheet_idx = 0;
        app.refresh_invalid(&ctx);
        app.circle_invalid = false;
        app.refresh_invalid(&ctx);
        assert!(app.invalid_job.as_ref().is_none_or(|job| job.cancel.load(Ordering::Relaxed)));
        app.wb.sheets[0].set(900_000, 0, text("new"));
        app.show_invalid(&ctx);
        poll_until_checked(&mut app, &ctx);
        assert!(app.invalid_cells.is_empty());
    }

    #[test]
    fn duplicate_circles_include_rows_beyond_fifty_thousand() {
        let mut wb = Workbook::new_blank();
        let sh = &mut wb.sheets[0];
        for row in 0..50_002 {
            sh.set(row, 0, Cell { value: Value::Number(row as f64), formula: None });
        }
        sh.set(50_001, 0, text("0001"));
        sh.dv.push(dv::DataValidation {
            ranges: vec![(0, 0, 50_001, 0)], kind: dv::DvKind::Custom,
            f1: "=COUNTIF($A$1:$A$50002,A1)<=1".into(), ..Default::default()
        });
        let start = std::time::Instant::now();
        let circles = find_invalid(&wb, 0, &AtomicBool::new(false)).unwrap();
        assert_eq!(circles.cells, [(1, 0), (50_001, 0)].into_iter().collect());
        eprintln!("50,002 COUNTIF validation cells: {:?}", start.elapsed());
    }

    #[test]
    fn named_relative_list_membership_uses_each_targets_actual_source() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, text("first"));
        wb.sheets[0].set(1, 0, text("second"));
        wb.sheets[0].set(2, 0, text("third"));
        wb.sheets[0].set(0, 2, text("first"));
        wb.sheets[0].set(1, 2, text("first"));
        wb.sheets[0].set(2, 2, text("third"));
        wb.names.push(("Choices".into(), None, "=$A1:$A1".into()));
        wb.sheets[0].dv.push(dv::DataValidation {
            ranges: vec![(0, 2, 2, 2)], kind: dv::DvKind::List, f1: "=Choices".into(),
            ..Default::default()
        });
        let result = find_invalid(&wb, 0, &AtomicBool::new(false)).unwrap();
        assert_eq!(result.cells, [(1, 2)].into_iter().collect());
    }

    #[test]
    fn editing_and_clearing_rules_refresh_circles_dropdown_and_in_flight_save() {
        let env = crate::test_env::TestEnv::new("validation-rule-save");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        let rect = (0, 0, 0, 0);
        let rule = |f: &str| dv::DataValidation {
            kind: dv::DvKind::List, f1: f.into(), ..Default::default()
        };
        app.sel = (0, 0);
        app.sel_end = app.sel;
        app.wb.sheets[0].set(0, 0, text("old"));
        app.replace_validation_rule(rect, Some(rule("\"old\"")));
        app.show_invalid(&ctx);
        app.dv_open_for_selection();
        assert_eq!(app.dv_cache, ["old"]);
        let path = env.root().join("rules.xlsx");
        app.tasks.start_save(&mut app.wb, path.clone());
        app.replace_validation_rule(rect, Some(rule("\"new\"")));
        app.refresh_dv_source(0, 0);
        app.refresh_invalid(&ctx);
        assert_eq!(app.dv_cache, ["new"]);
        assert!(app.invalid_cells.contains(&(0, 0)));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(done) = app.tasks.poll_save(&mut app.wb) {
                assert!(done.is_ok());
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(app.wb.dirty && app.wb.sheet(0).dv_dirty);
        assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheet(0).dv[0].f1, "\"old\"");
        app.clear_dv_in_sel();
        app.refresh_dv_source(0, 0);
        app.refresh_invalid(&ctx);
        assert!(app.invalid_cells.is_empty());
        assert!(app.dv_cache.is_empty());
        sheetcore::xlsx::save_atomic(&path, &app.wb).unwrap();
        assert!(sheetcore::xlsx::load(&path).unwrap().sheet(0).dv.is_empty());
    }
}

fn find_invalid(wb: &Workbook, si: usize, cancel: &AtomicBool) -> Option<Circles> {
    let sheet = wb.sheet(si);
    let mut result = Circles::default();
    let mut calc = formula::Calc::new(wb);
    let mut formulas = formula::RuleFormulas::default();
    for rule in &sheet.dv {
        let mut membership = (rule.kind == dv::DvKind::List)
            .then(|| dv::ListMembership::new(wb, si, rule));
        if let Some(list) = &mut membership {
            let (r, c) = rule.ranges.first().map_or((0, 0), |r| (r.0, r.1));
            if list.contains(&Value::Empty, r, c).is_none() {
                result.config_error = Some(format!("列表来源无法解析：{}", rule.f1));
            }
        }
        for &rect in &rule.ranges {
            for (key, cell) in sheet.iter_rect(rect) {
                if cancel.load(Ordering::Relaxed) { return None; }
                if matches!(cell.value, Value::Empty) { continue; }
                let (r, c) = (key.row, key.col);
                let valid = if let Some(list) = &mut membership {
                    list.contains(&cell.value, r, c).unwrap_or_else(|| {
                        result.config_error.get_or_insert_with(|| format!("列表来源无法解析：{}", rule.f1));
                        true // Report an invalid rule separately from invalid data.
                    })
                } else {
                    dv::validate_wb(wb, si, rule, &cell.value, r, c, |src, rr, cc| {
                        let (ar, ac) = dv::range_anchor(rule, rr, cc);
                        match formulas.shifted(src, rr - ar, cc - ac) {
                            Ok(node) => calc.eval_node(&node, si),
                            Err(e) => {
                                result.config_error = Some(format!("公式阈值无法解析：{src}（{e}）"));
                                formula::FValue::Error(formula::ErrKind::Value)
                            }
                        }
                    })
                };
                if !valid { result.cells.insert((r, c)); }
            }
        }
    }
    Some(result)
}

impl SheetApp {
    pub(super) fn replace_validation_rule(&mut self, rect: (u32, u32, u32, u32),
                                          rule: Option<dv::DataValidation>) -> bool {
        let (r1, c1, r2, c2) = rect;
        let sheet = self.wb.sheet_mut(self.sheet_idx);
        let before = sheet.dv.len();
        sheet.dv.retain(|d| !d.ranges.iter().any(|&(a, b, x, y)| a <= r2 && x >= r1 && b <= c2 && y >= c1));
        if let Some(mut rule) = rule {
            rule.ranges = vec![rect];
            sheet.dv.push(rule);
        } else if sheet.dv.len() == before {
            return false;
        }
        sheet.dv_dirty = true;
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.tasks.last_edit = Some(std::time::Instant::now());
        true
    }

    pub(super) fn validation_generation(&mut self) -> u64 {
        // List sources and custom rules may read any sheet, including formula
        // results installed by a worker without a user edit to the target sheet.
        let revisions = || self.wb.sheets.iter().map(|s| (s.identity, s.cell_revision()));
        if self.validation_doc_gen != self.doc_gen || self.validation_cells.iter().copied().ne(revisions()) {
            self.validation_doc_gen = self.doc_gen;
            self.validation_cells.clear();
            self.validation_cells.extend(revisions());
            self.validation_gen = self.validation_gen.wrapping_add(1);
        }
        self.validation_gen
    }

    fn install_circles(&mut self, result: Circles) {
        self.invalid_gen = self.validation_gen;
        self.invalid_cells = result.cells;
        if let Some(error) = result.config_error {
            self.status = format!("数据验证配置无效：{error}");
        } else if self.status == "正在检查无效数据…" {
            self.status = format!("已圈出 {} 个不合规单元格", self.invalid_cells.len());
        }
    }

    /// Used by deterministic probes and tests that read the result immediately.
    pub(super) fn compute_invalid(&mut self) {
        self.invalid_job = None;
        let result = find_invalid(&self.wb, self.sheet_idx, &AtomicBool::new(false)).unwrap();
        self.install_circles(result);
    }

    pub(super) fn show_invalid(&mut self, ctx: &egui::Context) {
        self.circle_invalid = true;
        self.invalid_gen = u64::MAX;
        self.status = "正在检查无效数据…".into();
        self.refresh_invalid(ctx);
    }

    pub(super) fn refresh_invalid(&mut self, ctx: &egui::Context) {
        if !self.circle_invalid {
            if let Some(job) = &self.invalid_job {
                job.cancel.store(true, Ordering::Relaxed);
                // Keep the worker slot until a long formula or sort has returned,
                // even if the user switches circles off and back on immediately.
                if !matches!(job.rx.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    self.invalid_job = None;
                }
            }
            return;
        }
        let view = (self.doc_uid, self.wb_epoch, self.wb.sheet(self.sheet_idx).identity);
        let generation = self.validation_generation();
        if self.invalid_view != Some(view) {
            self.invalid_view = Some(view);
            self.invalid_gen = u64::MAX;
            self.invalid_cells.clear();
        }
        if let Some(job) = &self.invalid_job {
            let current = job.view == view && job.generation == generation && !job.cancel.load(Ordering::Relaxed);
            if !current { job.cancel.store(true, Ordering::Relaxed); }
            match job.rx.try_recv() {
                Ok(result) if current => self.install_circles(result),
                Err(mpsc::TryRecvError::Empty) => return,
                _ => {}
            }
            self.invalid_job = None;
        }
        if self.invalid_gen == generation { return; }
        if self.wb.sheet(self.sheet_idx).dv.is_empty() {
            self.install_circles(Circles::default());
            return;
        }
        if self.wb.sheets.iter().map(|sh| sh.len()).sum::<usize>() <= 2_000 {
            self.compute_invalid();
            return;
        }
        let wb = self.wb.clone();
        let si = self.sheet_idx;
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        self.invalid_job = Some(InvalidJob { view, generation, cancel: cancel.clone(), rx });
        let wake = ctx.clone();
        std::thread::spawn(move || {
            if let Some(result) = find_invalid(&wb, si, &cancel) { let _ = tx.send(result); }
            drop(tx);
            wake.request_repaint();
        });
    }
}
