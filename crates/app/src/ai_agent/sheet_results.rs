use super::{cell_name, formula, json, range_label, FValue, JsonValue, SheetApp, Value};
use eframe::egui;
use sheetcore::Workbook;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

pub(super) enum AiToolOutput {
    Ready(String, JsonValue),
    Calculating(AiToolCalculation),
}

impl From<(String, JsonValue)> for AiToolOutput {
    fn from((label, result): (String, JsonValue)) -> Self {
        Self::Ready(label, result)
    }
}

pub(super) struct AiToolCalculation {
    pub rx: mpsc::Receiver<(String, JsonValue)>,
    cancel: Arc<AtomicBool>,
}

impl Drop for AiToolCalculation {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl SheetApp {
    pub(super) fn with_calculated_ai_view(
        &mut self,
        ctx: &egui::Context,
        read: impl FnOnce(WorkbookToolView<'_>) -> (String, JsonValue) + Send + 'static,
    ) -> AiToolOutput {
        let sheet_idx = self.sheet_idx;
        if !self.wb.calc_pending && !self.tasks.recalc_pending() {
            return read(WorkbookToolView { wb: &self.wb, sheet_idx }).into();
        }
        // Verify the revision observed by this tool. The live workbook keeps its calculation
        // mode, pending jobs and any edits made while the snapshot is calculating.
        self.cancel_silent_recalc();
        let mut snapshot = self.wb.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            if formula::recalc_all_force_impl(&mut snapshot, Some(&worker_cancel), None)
                && !worker_cancel.load(Ordering::Relaxed)
            {
                let result = read(WorkbookToolView { wb: &snapshot, sheet_idx });
                let _ = tx.send(result);
            }
            wake.request_repaint();
        });
        AiToolOutput::Calculating(AiToolCalculation { rx, cancel })
    }
}

pub(super) struct WrittenCells {
    pub sheet: usize,
    pub label: String,
    pub metadata: JsonValue,
    pub sample_limit: usize,
    pub keys: Vec<(u32, u32)>,
}

impl WrittenCells {
    pub(super) fn result(mut self, wb: &Workbook) -> (String, JsonValue) {
        let mut samples = Vec::new();
        let mut errors = Vec::new();
        let mut error_count = 0;
        for (r, c) in self.keys {
            let value = wb.sheet(self.sheet).value(r, c);
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
        self.metadata["samples"] = json!(samples);
        self.metadata["errors"] = json!(errors);
        self.metadata["error_count"] = json!(error_count);
        (self.label, self.metadata)
    }
}

pub(super) struct WorkbookToolView<'a> {
    pub wb: &'a Workbook,
    pub sheet_idx: usize,
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
        let span = range_label((r1, c1), (r2, c2));
        (
            format!("查看 {}!{span}（{} 项）", sh.name, cells.len()),
            json!({
                "ok": true,
                "sheet": sh.name,
                "range": span,
                "cells": cells
            }),
        )
    }

    pub(super) fn tool_search_cells(&self, args: &JsonValue) -> (String, JsonValue) {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("").trim();
        if query.is_empty() {
            return ("搜索为空".into(), json!({"ok": false, "error": "query 不能为空"}));
        }
        let q_low = query.to_lowercase();
        let sheets: Vec<usize> = match args.get("sheet").and_then(|v| v.as_str()) {
            Some(name) => match self.resolve_sheet_index(Some(name)) {
                Some(i) => vec![i],
                None => return ("搜索失败".into(), json!({"ok": false, "error": "未找到工作表"})),
            },
            None => (0..self.wb.sheets.len()).collect(),
        };
        let mut hits = Vec::new();
        for si in sheets {
            let sh = self.wb.sheet(si);
            for (cr, cell) in sh.iter() {
                let val = cell.value.display();
                let f = cell.formula.as_deref().unwrap_or("");
                if val.to_lowercase().contains(&q_low) || f.to_lowercase().contains(&q_low) {
                    hits.push(json!({
                        "sheet": sh.name,
                        "cell": cell_name(cr.row, cr.col),
                        "value": val,
                        "formula": cell.formula.as_deref().map(|s| format!("={s}"))
                    }));
                    if hits.len() >= 30 {
                        break;
                    }
                }
            }
            if hits.len() >= 30 {
                break;
            }
        }
        (
            format!("搜索「{query}」（命中 {} 处）", hits.len()),
            json!({"ok": true, "hits": hits}),
        )
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
    use std::time::Duration;

    fn tool(name: &str, arguments: JsonValue) -> ToolCallSpec {
        ToolCallSpec { name: name.into(), arguments }
    }

    fn calculated(output: AiToolOutput) -> JsonValue {
        let AiToolOutput::Calculating(job) = output else { panic!("pending formula caches require calculation") };
        job.rx.recv_timeout(Duration::from_secs(10)).expect("calculation finished").1
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
        let written = calculated(output);
        assert_eq!(written["samples"][1]["value"], "12");
        assert_eq!(written["errors"], json!(["C1=#DIV/0!"]));
        assert!(app.wb.calc_manual && app.wb.calc_pending);
        assert_eq!(app.wb.sheet(0).value(0, 1), Value::Empty);
        assert!(build_workbook_context(&app).contains("待重算"));
        let output = app.execute_ai_tool(&tool("inspect_sheet", json!({"range":"B1"})), &ctx);
        assert_eq!(calculated(output)["cells"][0]["value"], "12");
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
        assert_eq!(calculated(output)["samples"][0]["value"], "1");
        let output = app.execute_ai_tool(&tool("inspect_sheet", json!({"range":"A2"})), &ctx);
        assert_eq!(calculated(output)["cells"][0]["value"], "2");
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
}
