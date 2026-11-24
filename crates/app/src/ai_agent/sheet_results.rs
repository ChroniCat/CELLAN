use super::{cell_name, formula, json, range_label, FValue, JsonValue, SheetApp, Value};
use eframe::egui;
use sheetcore::Workbook;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

pub(super) enum AiToolOutput {
    Ready(String, JsonValue),
    Calculating(AiToolCalculation),
    Reordering {
        sheet_id: u64,
        op_desc: String,
        range_lbl: String,
        undo_checkpoint: u64,
        before_gen: u64,
    },
}

impl From<(String, JsonValue)> for AiToolOutput {
    fn from((label, result): (String, JsonValue)) -> Self {
        Self::Ready(label, result)
    }
}

type ToolRead = Box<dyn Fn(WorkbookToolView<'_>) -> (String, JsonValue) + Send>;
type SnapshotDone = (
    (String, JsonValue),
    Arc<Workbook>,
    Option<Arc<formula::deps::Deps>>,
    ToolRead,
);

/// A calculated copy kept while manual calculation prevents updating the visible workbook.
/// `replay` lists cells the assistant wrote after the copy was calculated.
pub(super) struct AiCalcBase {
    revision: (u64, u64, u64),
    wb: Arc<Workbook>,
    deps: Option<Arc<formula::deps::Deps>>,
    replay: Vec<(usize, u32, u32)>,
}

impl AiCalcBase {
    #[cfg(test)]
    pub(super) fn has_deps(&self) -> bool {
        self.deps.is_some()
    }
}

fn workbook_supports_incremental_ai_calc(wb: &Workbook) -> bool {
    !wb.calc_iterate
        && !wb.sheets.iter().any(|s| {
            !s.spilled.is_empty() || !s.spill_shapes.is_empty() || !s.arrays.is_empty()
        })
}

pub(super) struct AiToolCalculation {
    kind: CalcKind,
}

enum CalcKind {
    /// Automatic calculation: the app's background job (incremental where possible)
    /// writes its result into the live workbook, then the tool reads that.
    Live(Option<ToolRead>),
    /// Manual calculation: calculate a private copy without touching the visible values.
    Snapshot {
        rx: mpsc::Receiver<SnapshotDone>,
        cancel: Arc<AtomicBool>,
        progress: Arc<(AtomicUsize, AtomicUsize)>,
        revision: (u64, u64, u64),
    },
}

impl Drop for AiToolCalculation {
    fn drop(&mut self) {
        if let CalcKind::Snapshot { cancel, .. } = &self.kind {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub(super) enum CalcPoll {
    Pending(Option<(usize, usize)>),
    Ready(String, JsonValue),
    Failed(String),
}

impl SheetApp {
    pub(super) fn ai_calc_revision(&self) -> (u64, u64, u64) {
        (self.doc_uid, self.doc_gen, self.wb_epoch)
    }

    fn ai_values_calculated(&self) -> bool {
        !self.wb.calc_pending && !self.tasks.recalc_pending()
    }

    pub(super) fn ai_calculated_workbook(&self) -> Option<&Workbook> {
        if self.ai_values_calculated() {
            return Some(&self.wb);
        }
        let base = self.ai.calc_base.as_ref()?;
        (self.wb.calc_manual && base.revision == self.ai_calc_revision() && base.replay.is_empty())
            .then_some(base.wb.as_ref())
    }

    /// Whether cells written after `ai_calc_base` was calculated can be recalculated alone.
    fn ai_incremental_safe(&self) -> bool {
        self.deps_built
            && !self.deps.requires_full_recalc()
            && workbook_supports_incremental_ai_calc(&self.wb)
    }

    pub(super) fn with_calculated_ai_view(
        &mut self,
        ctx: &egui::Context,
        read: ToolRead,
    ) -> AiToolOutput {
        let sheet_idx = self.sheet_idx;
        if let Some(wb) = self.ai_calculated_workbook() {
            return read(WorkbookToolView { wb, sheet_idx }).into();
        }
        if !self.wb.calc_manual {
            // Reuse the regular background recalculation instead of cancelling it and
            // calculating the whole workbook again; its result also clears the pending state.
            if !self.tasks.recalc_pending() {
                self.request_silent_bg_recalc();
            }
            ctx.request_repaint();
            return AiToolOutput::Calculating(AiToolCalculation {
                kind: CalcKind::Live(Some(read)),
            });
        }
        let revision = self.ai_calc_revision();
        let base = self.ai.calc_base.take().filter(|b| b.revision == revision);
        let incremental = base
            .and_then(|base| {
                let live_safe = self.ai_incremental_safe();
                let base_safe = base.deps.is_some() && workbook_supports_incremental_ai_calc(&self.wb);
                if !live_safe && !base_safe {
                    return None;
                }
                let mut copy = Arc::unwrap_or_clone(base.wb);
                copy.styles = self.wb.styles.clone();
                for &(si, r, c) in &base.replay {
                    let live = self.wb.sheet(si);
                    let target = copy.sheet_mut(si);
                    match live.get(r, c) {
                        Some(cell) => target.set(r, c, cell.into_owned()),
                        None => {
                            let _ = target.remove(r, c);
                        }
                    }
                    target.set_style(r, c, live.style_of(r, c));
                }
                let deps_source = if live_safe {
                    (self.deps.clone(), false)
                } else {
                    (base.deps?, true)
                };
                Some((copy, deps_source, base.replay))
            });
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let (worker_cancel, worker_progress) = (cancel.clone(), progress.clone());
        let full = incremental.is_none().then(|| self.wb.clone());
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let calculated = match (incremental, full) {
                (Some((mut copy, (mut deps, needs_seed_sync), seeds)), _) => {
                    if needs_seed_sync {
                        for &key in &seeds {
                            if deps.cell_needs_update(&copy, key) {
                                Arc::make_mut(&mut deps).update_cell(&copy, key);
                            }
                        }
                    }
                    if deps.requires_full_recalc() {
                        let (done, total) = (&worker_progress.0, &worker_progress.1);
                        let ok = formula::recalc_all_force_impl(
                            &mut copy,
                            Some(&worker_cancel),
                            Some((done, total)),
                        );
                        let rebuilt = ok
                            .then(|| {
                                workbook_supports_incremental_ai_calc(&copy)
                                    .then(|| Arc::new(formula::deps::Deps::build(&copy)))
                            })
                            .flatten();
                        ok.then_some((copy, rebuilt))
                    } else {
                        // The copy mirrors manual mode; calculate only the written cells' closure.
                        copy.calc_manual = false;
                        let ok =
                            formula::recalc_dirty_cancelable(&mut copy, &deps, &seeds, &worker_cancel);
                        copy.calc_manual = true;
                        let kept_deps =
                            workbook_supports_incremental_ai_calc(&copy).then_some(deps);
                        ok.then_some((copy, kept_deps))
                    }
                }
                (None, Some(mut copy)) => {
                    let (done, total) = (&worker_progress.0, &worker_progress.1);
                    let ok = formula::recalc_all_force_impl(
                        &mut copy,
                        Some(&worker_cancel),
                        Some((done, total)),
                    );
                    let built_deps = ok
                        .then(|| {
                            workbook_supports_incremental_ai_calc(&copy)
                                .then(|| Arc::new(formula::deps::Deps::build(&copy)))
                        })
                        .flatten();
                    ok.then_some((copy, built_deps))
                }
                (None, None) => None,
            };
            if let Some((copy, deps)) =
                calculated.filter(|_| !worker_cancel.load(Ordering::Relaxed))
            {
                let result = read(WorkbookToolView { wb: &copy, sheet_idx });
                let _ = tx.send((result, Arc::new(copy), deps, read));
            }
            wake.request_repaint();
        });
        AiToolOutput::Calculating(AiToolCalculation {
            kind: CalcKind::Snapshot { rx, cancel, progress, revision },
        })
    }

    /// Cells written by the assistant keep a manual-mode calculated copy reusable.
    pub(super) fn note_ai_written_cells(&mut self, before: (u64, u64, u64), sheet_id: u64, keys: &[(u32, u32)]) {
        let after = self.ai_calc_revision();
        match self.ai.calc_base.as_mut() {
            Some(base) if base.revision == before && self.wb.calc_manual => {
                // The before/after revisions bracket one synchronous write, so indices match the copy.
                if let Some(sheet) = base.wb.sheets.iter().position(|s| s.identity == sheet_id) {
                    base.replay.extend(keys.iter().map(|&(r, c)| (sheet, r, c)));
                } else {
                    self.ai.calc_base = None;
                    return;
                }
                base.revision = after;
            }
            _ => self.ai.calc_base = None,
        }
    }

    pub(super) fn poll_ai_calculation(
        &mut self,
        job: &mut AiToolCalculation,
        ctx: &egui::Context,
    ) -> CalcPoll {
        match &mut job.kind {
            CalcKind::Live(read) => {
                if self.wb.calc_manual {
                    return CalcPoll::Failed("计算模式在核对期间已切换为手动".into());
                }
                if !self.ai_values_calculated() {
                    if !self.tasks.recalc_pending() {
                        self.request_silent_bg_recalc();
                    }
                    // Idempotent with the frame loop; also drives the job in headless tests.
                    self.poll_silent_recalc();
                    self.start_silent_recalc(ctx);
                    if !self.ai_values_calculated() {
                        return CalcPoll::Pending(self.silent_recalc_progress());
                    }
                }
                let Some(read) = read.take() else {
                    return CalcPoll::Failed("公式复核任务已结束".into());
                };
                let (label, result) = read(WorkbookToolView { wb: &self.wb, sheet_idx: self.sheet_idx });
                CalcPoll::Ready(label, result)
            }
            CalcKind::Snapshot { rx, progress, revision, .. } => match rx.try_recv() {
                Ok(((label, result), wb, deps, read)) => {
                    let current = self.ai_calc_revision();
                    if revision.0 != current.0 || revision.2 != current.2 {
                        return CalcPoll::Failed("核对表格期间工作簿已切换或重新载入".into());
                    }
                    if revision.1 != current.1 || !self.wb.calc_manual {
                        // Re-run only the observation against current data, even for write tools.
                        return match self.with_calculated_ai_view(ctx, read) {
                            AiToolOutput::Ready(label, result) => CalcPoll::Ready(label, result),
                            AiToolOutput::Calculating(next) => {
                                *job = next;
                                CalcPoll::Pending(None)
                            }
                            AiToolOutput::Reordering { .. } => {
                                CalcPoll::Failed("核对结果时不能触发新的重排任务".into())
                            }
                        };
                    }
                    self.ai.calc_base = Some(AiCalcBase {
                        revision: *revision,
                        wb,
                        deps,
                        replay: Vec::new(),
                    });
                    CalcPoll::Ready(label, result)
                }
                Err(mpsc::TryRecvError::Empty) => {
                    let total = progress.1.load(Ordering::Relaxed);
                    CalcPoll::Pending((total > 0).then(|| (progress.0.load(Ordering::Relaxed), total)))
                }
                Err(mpsc::TryRecvError::Disconnected) => CalcPoll::Failed("公式复核任务意外退出".into()),
            },
        }
    }
}

pub(super) struct WrittenCells {
    pub sheet_id: u64,
    pub label: String,
    pub metadata: JsonValue,
    pub sample_limit: usize,
    pub keys: Vec<(u32, u32)>,
}

impl WrittenCells {
    pub(super) fn result(&self, wb: &Workbook) -> (String, JsonValue) {
        let Some(sheet) = wb.sheets.iter().find(|s| s.identity == self.sheet_id) else {
            return ("写入结果核对失败".into(), json!({"ok": false, "error": "写入的工作表已被删除"}));
        };
        let mut samples = Vec::new();
        let mut errors = Vec::new();
        let mut error_count = 0;
        for &(r, c) in &self.keys {
            let value = sheet.value(r, c);
            if matches!(value, Value::Error(_)) {
                error_count += 1;
                if errors.len() < 32 {
                    errors.push(format!("{}={}", cell_name(r, c), value.display()));
                }
            }
            if samples.len() < self.sample_limit {
                samples.push(json!({"cell": cell_name(r, c), "value": value.display()}));
            }
        }
        let mut metadata = self.metadata.clone();
        metadata["samples"] = json!(samples);
        metadata["errors"] = json!(errors);
        metadata["error_count"] = json!(error_count);
        (self.label.clone(), metadata)
    }
}

pub(super) struct WorkbookToolView<'a> {
    pub wb: &'a Workbook,
    pub sheet_idx: usize,
}

#[derive(Clone, Debug)]
pub(super) struct PreparedSearchQuery {
    pub q_low: String,
    pub q_ascii: bool,
    pub q_caseless_cjk: bool,
    pub can_match_num: bool,
    pub can_match_true: bool,
    pub can_match_false: bool,
}

struct StackFmt<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackFmt<N> {
    #[inline]
    fn new() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
        }
    }

    #[inline]
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> std::fmt::Write for StackFmt<N> {
    #[inline]
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let bytes = s.as_bytes();
        if self.len + bytes.len() > N {
            return Err(std::fmt::Error);
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(())
    }
}

impl PreparedSearchQuery {
    pub(super) fn new(query: &str) -> Self {
        let q_low = query.to_lowercase();
        let q_ascii = q_low.is_ascii();
        let q_caseless_cjk = !q_low.is_empty()
            && q_low
                .chars()
                .all(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c));
        let can_match_num = q_ascii
            && q_low
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'.' | b'-' | b'+' | b'e'));
        let can_match_true = "true".contains(&q_low);
        let can_match_false = "false".contains(&q_low);
        Self {
            q_low,
            q_ascii,
            q_caseless_cjk,
            can_match_num,
            can_match_true,
            can_match_false,
        }
    }

    #[inline]
    pub(super) fn matches_str(&self, s: &str) -> bool {
        if s.is_ascii() {
            if !self.q_ascii || s.len() < self.q_low.len() {
                return false;
            }
            let needle = self.q_low.as_bytes();
            if needle.is_empty() {
                return true;
            }
            return s
                .as_bytes()
                .windows(needle.len())
                .any(|w| w.eq_ignore_ascii_case(needle));
        }
        if self.q_caseless_cjk {
            return s.contains(&self.q_low);
        }
        s.to_lowercase().contains(&self.q_low)
    }

    /// 栈缓冲区零堆分配格式化数字并与 `sheetcore::model::format_number` 保持字节级一致。
    #[inline]
    fn matches_number(&self, n: f64) -> bool {
        if n.is_nan() || n.is_infinite() {
            return self.matches_str("#NUM!");
        }
        if !self.can_match_num {
            return false;
        }
        if n.fract() == 0.0 && n.abs() < 1e15 {
            let v = n as i64;
            let mut raw = [0u8; 24];
            let mut pos = raw.len();
            let mut u = v.unsigned_abs();
            if u == 0 {
                pos -= 1;
                raw[pos] = b'0';
            } else {
                while u > 0 {
                    pos -= 1;
                    raw[pos] = b'0' + (u % 10) as u8;
                    u /= 10;
                }
                if v < 0 {
                    pos -= 1;
                    raw[pos] = b'-';
                }
            }
            let s = std::str::from_utf8(&raw[pos..]).unwrap_or("");
            return self.matches_str(s);
        }
        use std::fmt::Write as _;
        let mut fmt = StackFmt::<64>::new();
        if write!(&mut fmt, "{:.10}", n).is_ok() {
            let s = fmt.as_str().trim_end_matches('0').trim_end_matches('.');
            self.matches_str(s)
        } else {
            self.matches_str(&sheetcore::model::format_number(n))
        }
    }

    #[inline]
    pub(super) fn matches_cell(&self, cell: &sheetcore::model::Cell) -> bool {
        let val_hit = match &cell.value {
            Value::Empty => false,
            Value::Number(n) => self.matches_number(*n),
            Value::Bool(b) => {
                if *b {
                    self.can_match_true
                } else {
                    self.can_match_false
                }
            }
            Value::Text(s) => self.matches_str(s),
            Value::Error(e) => self.matches_str(e.text()),
        };
        if val_hit {
            return true;
        }
        match cell.formula.as_deref() {
            Some(f) if !f.is_empty() => self.matches_str(f),
            _ => false,
        }
    }
}

impl WorkbookToolView<'_> {
    pub(super) fn resolve_sheet_index(&self, name: Option<&str>) -> Option<usize> {
        match name.map(str::trim).filter(|s| !s.is_empty()) {
            None => Some(self.sheet_idx),
            Some(n) => self
                .wb
                .sheets
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(n)),
        }
    }

    pub(super) fn tool_inspect_sheet(&self, args: &JsonValue) -> (String, JsonValue) {
        let Some(si) = self.resolve_sheet_index(args.get("sheet").and_then(|v| v.as_str())) else {
            return (
                "读取工作表失败".into(),
                json!({"ok": false, "error": "未找到指定工作表"}),
            );
        };
        let sh = self.wb.sheet(si);
        let bounds = match args.get("range") {
            Some(range) => match range.as_str().and_then(super::checked_ai_range) {
                Some(rect) => rect,
                None => return ("读取工作表失败".into(), json!({"ok": false, "error": "range 格式无效"})),
            },
            None => sh.used_bounds().unwrap_or((0, 0, 9, 7)),
        };
        let (r1, c1, r2, c2) = (
            bounds.0,
            bounds.1,
            bounds.2.min(bounds.0 + 39),
            bounds.3.min(bounds.1 + 15),
        );
        let mut cells = Vec::new();
        for rr in r1..=r2 {
            for cc in c1..=c2 {
                if let Some(cell) = sh.get(rr, cc) {
                    if !cell.is_blank() {
                        let st = self.wb.styles.get(sh.style_of(rr, cc));
                        let disp = sheetcore::style::format_value_in(
                            self.wb.date_system,
                            &cell.value,
                            &st.format,
                        );
                        let mut item = json!({
                            "cell": cell_name(rr, cc),
                            "value": disp,
                        });
                        if let Some(f) = &cell.formula {
                            item["formula"] = json!(format!("={f}"));
                        }
                        if !st.format.is_empty() {
                            item["format"] = json!(st.format);
                        }
                        cells.push(item);
                        if cells.len() >= 160 {
                            break;
                        }
                    }
                }
            }
            if cells.len() >= 160 {
                break;
            }
        }
        let default_col_w = sh.cols.default_size().round() as i64;
        let default_row_h = sh.rows.default_size().round() as i64;
        let mut col_widths = serde_json::Map::new();
        for cc in c1..=c2 {
            let w = sh.cols.size(cc);
            if (w - sh.cols.default_size()).abs() >= 0.5 {
                let col_lbl = cell_name(0, cc);
                col_widths.insert(
                    col_lbl.trim_end_matches('1').to_string(),
                    json!(w.round() as i64),
                );
            }
        }
        let mut row_heights = serde_json::Map::new();
        for rr in r1..=r2 {
            let h = sh.rows.size(rr);
            if (h - sh.rows.default_size()).abs() >= 0.5 {
                row_heights.insert((rr + 1).to_string(), json!(h.round() as i64));
            }
        }
        let span = range_label((r1, c1), (r2, c2));
        let mut payload = json!({
            "ok": true,
            "sheet": sh.name,
            "range": span,
            "default_col_width_px": default_col_w,
            "default_row_height_px": default_row_h,
            "cells": cells
        });
        if !col_widths.is_empty() {
            payload["col_widths_px"] = JsonValue::Object(col_widths);
        }
        if !row_heights.is_empty() {
            payload["row_heights_px"] = JsonValue::Object(row_heights);
        }
        (
            format!("查看 {}!{span}（{} 项）", sh.name, cells.len()),
            payload,
        )
    }

    pub(super) fn tool_search_cells(&self, args: &JsonValue) -> (String, JsonValue) {
        const HIT_LIMIT: usize = 30;
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("").trim();
        if query.is_empty() {
            return ("搜索为空".into(), json!({"ok": false, "error": "query 不能为空"}));
        }
        let prepared = PreparedSearchQuery::new(query);
        let sheets: Vec<usize> = match args.get("sheet").and_then(|v| v.as_str()) {
            Some(name) => match self.resolve_sheet_index(Some(name)) {
                Some(i) => vec![i],
                None => return ("搜索失败".into(), json!({"ok": false, "error": "未找到工作表"})),
            },
            None => (0..self.wb.sheets.len()).collect(),
        };
        let mut hits = Vec::new();
        let mut has_more = false;
        'search: for si in sheets {
            let sh = self.wb.sheet(si);
            for (cr, cell) in sheetcore::sync::iter_sheet_rect(sh, (0, 0, u32::MAX, u32::MAX)) {
                if prepared.matches_cell(cell.as_ref()) {
                    if hits.len() == HIT_LIMIT {
                        has_more = true;
                        break 'search;
                    }
                    hits.push(json!({
                        "sheet": sh.name,
                        "cell": cell_name(cr.row, cr.col),
                        "value": cell.value.display(),
                        "formula": cell.formula.as_deref().map(|s| format!("={s}"))
                    }));
                }
            }
        }
        let label = if has_more {
            format!("搜索「{query}」（仅显示前 {HIT_LIMIT} 处）")
        } else {
            format!("搜索「{query}」（命中 {} 处）", hits.len())
        };
        (label, json!({"ok": true, "has_more": has_more, "hits": hits}))
    }

    pub(super) fn tool_eval_preview(&self, args: &JsonValue) -> (String, JsonValue) {
        let f = args.get("formula").and_then(|v| v.as_str()).unwrap_or("").trim();
        if f.is_empty() {
            return ("公式试算失败".into(), json!({"ok": false, "error": "formula 不能为空"}));
        }
        let Some(si) = self.resolve_sheet_index(args.get("sheet").and_then(|v| v.as_str())) else {
            return ("公式试算失败".into(), json!({"ok": false, "error": "未找到工作表"}));
        };
        let fv = formula::eval_formula(self.wb, si, f);
        (
            format!("试算 {f} = {}", fv.display()),
            json!({"ok": !matches!(fv, FValue::Error(_)), "result": fv.display()}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_agent::{build_workbook_context, ToolCallSpec};
    use sheetcore::Cell;
    use std::time::{Duration, Instant};

    fn tool(name: &str, arguments: JsonValue) -> ToolCallSpec {
        ToolCallSpec { name: name.into(), arguments }
    }

    fn calculated(app: &mut SheetApp, ctx: &egui::Context, output: AiToolOutput) -> JsonValue {
        let AiToolOutput::Calculating(mut job) = output else { panic!("pending formula caches require calculation") };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match app.poll_ai_calculation(&mut job, ctx) {
                CalcPoll::Ready(_, result) => return result,
                CalcPoll::Failed(error) => panic!("{error}"),
                CalcPoll::Pending(_) => {
                    assert!(Instant::now() < deadline, "calculation did not finish");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    fn immediate(output: AiToolOutput) -> JsonValue {
        let AiToolOutput::Ready(_, result) = output else { panic!("calculated values must be reused") };
        result
    }

    #[test]
    fn search_marks_additional_hits_only_when_another_match_exists() {
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.add_sheet();
        let entry = Cell { value: Value::Text("Invoice".into()), formula: None };
        for row in 0..30 {
            app.wb.sheet_mut(0).set(row, 0, entry.clone());
        }
        let search = tool("search_cells", json!({"query": "invoice"}));
        let complete = immediate(app.execute_ai_tool(&search, &ctx));
        assert_eq!(complete["has_more"], false);
        assert_eq!(complete["hits"].as_array().unwrap().len(), 30);

        app.wb.sheet_mut(1).set(0, 0, entry);
        let limited = immediate(app.execute_ai_tool(&search, &ctx));
        assert_eq!(limited["hits"], complete["hits"]);
        assert_eq!(limited["has_more"], true);
    }

    #[test]
    fn manual_mode_verifies_writes_and_inspection_against_a_calculated_snapshot() {
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.manual_calc = true;
        app.wb.calc_manual = true;
        let output = app.execute_ai_tool(&tool("set_cells", json!({"edits": [
            {"cell":"A1","value":4}, {"cell":"B1","formula":"=A1*3"}, {"cell":"C1","formula":"=1/0"}
        ]})), &ctx);
        let written = calculated(&mut app, &ctx, output);
        assert_eq!(written["samples"][1]["value"], "12");
        assert_eq!(written["errors"], json!(["C1=#DIV/0!"]));
        assert!(app.wb.calc_manual && app.wb.calc_pending);
        assert_eq!(app.wb.sheet(0).value(0, 1), Value::Empty);
        assert!(build_workbook_context(&app, app.sheet_idx, app.sel_rect()).contains("待重算"));
        // The calculated copy is reused while nothing else changed.
        let output = app.execute_ai_tool(&tool("inspect_sheet", json!({"range":"B1"})), &ctx);
        assert_eq!(immediate(output)["cells"][0]["value"], "12");
        // A later write is replayed onto that copy and only its dependents are recalculated.
        let output = app.execute_ai_tool(&tool("set_cells", json!({"edits": [{"cell":"A1","value":5}]})), &ctx);
        assert_eq!(calculated(&mut app, &ctx, output)["samples"][0]["value"], "5");
        let output = app.execute_ai_tool(&tool("inspect_sheet", json!({"range":"B1"})), &ctx);
        assert_eq!(immediate(output)["cells"][0]["value"], "15");
        assert_eq!(app.wb.sheet(0).value(0, 1), Value::Empty, "manual mode keeps visible values");
        // Any other change makes the copy unusable.
        app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(6.0), formula: None });
        app.doc_gen += 1;
        let output = app.execute_ai_tool(&tool("inspect_sheet", json!({"range":"B1"})), &ctx);
        assert_eq!(calculated(&mut app, &ctx, output)["cells"][0]["value"], "18");
    }

    #[test]
    fn deferred_large_sheet_verifies_a_spill_before_answering() {
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        for row in 0..1300 {
            app.wb.sheet_mut(0).set(row, 4, Cell { value: Value::Number(10.0), formula: None });
        }
        let output = app.execute_ai_tool(&tool("fill_formula_range", json!({
            "range":"A1", "top_left_formula":"=SEQUENCE(2)"
        })), &ctx);
        assert_eq!(calculated(&mut app, &ctx, output)["samples"][0]["value"], "1");
        // The background job wrote its result into the live workbook, so the next read is immediate.
        assert!(!app.wb.calc_pending && !app.tasks.recalc_pending());
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(2.0));
        let output = app.execute_ai_tool(&tool("inspect_sheet", json!({"range":"A2"})), &ctx);
        assert_eq!(immediate(output)["cells"][0]["value"], "2");
    }

    #[test]
    fn clear_create_and_rename_leave_ordered_undo_and_redo_entries() {
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(9.0), formula: None });
        let original_name = app.wb.sheet(0).name.clone();
        let dangling = "='Summary'!A1";
        app.wb.sheet_mut(0).set(1, 0, Cell { value: Value::Empty, formula: Some(dangling.into()) });
        let reference = "=Draft!A1";
        app.wb.sheet_mut(0).set(2, 0, Cell { value: Value::Empty, formula: Some(reference.into()) });
        app.wb.names.push(("before".into(), None, reference.into()));
        app.wb.names.push(("unresolved".into(), None, dangling.into()));
        for call in [
            tool("data_operation", json!({"op":"clear_contents","range":"A1"})),
            tool("manage_sheet", json!({"op":"create","name":"Draft"})),
            tool("manage_sheet", json!({"op":"rename","name":"Draft","new_name":"Summary"})),
        ] {
            let AiToolOutput::Ready(_, result) = app.execute_ai_tool(&call, &ctx) else { panic!() };
            assert_eq!(result["ok"], true);
        }
        assert_eq!(app.undo.depth(), (3, 0));
        assert_eq!(app.wb.names[0].2, dangling);
        let id = app.wb.sheet(1).identity;
        assert!(app.apply_undo(false));
        assert_eq!(app.wb.sheet(1).name, "Draft");
        assert_eq!(app.wb.sheet(0).get(1, 0).unwrap().formula.as_deref(), Some(dangling));
        assert_eq!(app.wb.sheet(0).get(2, 0).unwrap().formula.as_deref(), Some(reference));
        assert_eq!(app.wb.names[0].2, reference);
        assert_eq!(app.wb.names[1].2, dangling);
        assert!(app.apply_undo(false));
        assert_eq!(app.wb.sheets.len(), 1);
        assert_eq!(app.wb.sheet(app.sheet_idx).name, original_name);
        assert!(app.apply_undo(false));
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(9.0));
        for _ in 0..3 { assert!(app.apply_undo(true)); }
        assert_eq!(app.wb.sheet(1).identity, id);
        assert_eq!(app.wb.sheet(1).name, "Summary");
    }

    #[test]
    fn sort_operates_only_within_requested_cells_and_rejects_invalid_ranges() {
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        for (row, n) in [3.0, 1.0, 2.0].into_iter().enumerate() {
            for col in 0..2 {
                app.wb.sheet_mut(0).set(row as u32, col, Cell { value: Value::Number(n), formula: None });
            }
        }
        let AiToolOutput::Ready(_, result) = app.execute_ai_tool(
            &tool("data_operation", json!({"op":"sort_asc", "range":"A1:A2"})), &ctx,
        ) else { panic!() };
        assert_eq!(result["ok"], true);
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(1.0));
        assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(3.0));
        assert_eq!(app.wb.sheet(0).value(2, 0), Value::Number(2.0));
        let AiToolOutput::Ready(_, result) = app.execute_ai_tool(
            &tool("data_operation", json!({"op":"clear_contents","range":"A0"})), &ctx,
        ) else { panic!() };
        assert_eq!(result["ok"], false);
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(1.0));
    }

    #[test]
    fn prepared_search_query_handles_ascii_cjk_numbers_booleans_and_formulas_zero_alloc() {
        let _env = crate::test_env::TestEnv::new("ai_prepared_search");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        let sh = app.wb.sheet_mut(0);
        for row in 0..600u32 {
            sh.set(
                row,
                0,
                Cell {
                    value: Value::Number(f64::from(row) * 1.5),
                    formula: None,
                },
            );
        }
        sh.set(
            12,
            1,
            Cell {
                value: Value::Text("InVoIcE-2026-Alpha".into()),
                formula: None,
            },
        );
        sh.set(
            310,
            2,
            Cell {
                value: Value::Text("华东大区应收账款明细".into()),
                formula: None,
            },
        );
        sh.set(
            520,
            3,
            Cell {
                value: Value::Bool(true),
                formula: Some("SUM(A1:A10)>0".into()),
            },
        );

        let view = WorkbookToolView { wb: &app.wb, sheet_idx: 0 };
        let (_, ascii_res) = view.tool_search_cells(&json!({"query": "invoice-2026"}));
        assert_eq!(ascii_res["hits"].as_array().unwrap().len(), 1);
        assert_eq!(ascii_res["hits"][0]["cell"], "B13");

        let (_, cjk_res) = view.tool_search_cells(&json!({"query": "应收账款"}));
        assert_eq!(cjk_res["hits"].as_array().unwrap().len(), 1);
        assert_eq!(cjk_res["hits"][0]["cell"], "C311");

        let (_, formula_res) = view.tool_search_cells(&json!({"query": "sum(a1:a10)"}));
        assert_eq!(formula_res["hits"].as_array().unwrap().len(), 1);
        assert_eq!(formula_res["hits"][0]["cell"], "D521");

        // Stack-formatted numeric search matches exact display values across ints, negatives, fractions & NaN/Inf.
        let samples = [
            0.0_f64,
            -0.0_f64,
            -42.0,
            123456789012345.0,
            3.1415926535,
            -0.005,
            1.25e16,
            f64::NAN,
            f64::INFINITY,
        ];
        for n in samples {
            let cell = Cell {
                value: Value::Number(n),
                formula: None,
            };
            let displayed = cell.value.display();
            let q = PreparedSearchQuery::new(&displayed);
            assert!(
                q.matches_cell(&cell),
                "stack number search must match exact display string for {n:?} -> {displayed:?}"
            );
            if n.is_finite() {
                let miss = PreparedSearchQuery::new("not-a-number");
                assert!(!miss.matches_cell(&cell));
            }
        }
    }
}
