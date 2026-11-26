use super::*;
use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::mpsc::TryRecvError;

const HISTORY_TURNS: usize = 12;
const HISTORY_BYTES: usize = 48 * 1024;
const OBSERVATION_BYTES: usize = 16 * 1024;
const DISPLAY_TURNS: usize = 50;

type Rect = (u32, u32, u32, u32);

/// The sheet and selection the model was told about. Omitted tool targets resolve here,
/// so moving the selection while the model works does not redirect its edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AiView {
    sheet: u64,
    rect: Rect,
}

struct FootprintSnap {
    sheet: sheetcore::model::Sheet,
    styles: sheetcore::style::StyleTable,
    calculated: bool,
    date_1904: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum FootprintTarget {
    /// 矩形选区（或可精确合并为无空洞矩形的批量写入）
    Rect(Rect),
    /// 同一工作表上离散批量 `set_cells` 的有序去重坐标集合（零误报冲突，共享单份快照）
    Cells(Vec<(u32, u32)>),
}

impl FootprintTarget {
    fn label(&self) -> String {
        match self {
            Self::Rect((r1, c1, r2, c2)) => range_label((*r1, *c1), (*r2, *c2)),
            Self::Cells(coords) => {
                let Some(&(mut r1, mut c1)) = coords.first() else {
                    return String::new();
                };
                let (mut r2, mut c2) = (r1, c1);
                for &(r, c) in coords.iter().skip(1) {
                    r1 = r1.min(r);
                    c1 = c1.min(c);
                    r2 = r2.max(r);
                    c2 = c2.max(c);
                }
                range_label((r1, c1), (r2, c2))
            }
        }
    }
}

/// A confirmation protects the inputs. Once a tool reports values, its calculated
/// values and number formats also belong to the observation.
struct Footprint {
    sheet: u64,
    target: FootprintTarget,
    values: bool,
    hash: Option<u64>,
    snap: std::cell::RefCell<Option<(Option<u64>, std::rc::Rc<FootprintSnap>)>>,
}

use super::sheet_results::SheetBandSearchCache;

enum ReadCache {
    None,
    Search {
        prepared: super::sheet_results::PreparedSearchQuery,
        sheets: Vec<SheetBandSearchCache>,
    },
    Overview {
        wb_path: Option<std::path::PathBuf>,
        date_1904: bool,
        calc_pending: bool,
        default_format: String,
        sheet_revs: Vec<(u64, String, sheetcore::store::CellVersion, u64, u32, u32)>,
        hash: u64,
    },
    EvalPreview {
        sheet_id: u64,
        sheet_ver: sheetcore::store::CellVersion,
        styles_rev: u64,
        date_1904: bool,
        hash: u64,
    },
}

struct ReadObservation {
    call: ToolCallSpec,
    sheet: Option<u64>,
    hash: Option<u64>,
    cache: std::cell::RefCell<ReadCache>,
}

enum PendingCalculation {
    Tool { call: WireToolCall, sheet: Option<u64>, job: AiToolCalculation },
    WriteBatch { calls: Vec<(WireToolCall, Option<u64>)>, job: AiToolCalculation },
    Validate { action: TurnAction, job: AiToolCalculation },
    Reorder {
        call: WireToolCall,
        sheet_id: u64,
        op_desc: String,
        range_lbl: String,
        undo_checkpoint: u64,
        before_gen: u64,
    },
}

enum TurnAction {
    Finish { visible: String, reasoning: Option<String>, calls: Result<Vec<WireToolCall>, String> },
    Confirm,
    Retry,
}

pub(super) struct ActiveTurn {
    prompt: String,
    cfg: AiConfig,
    messages: Vec<JsonValue>,
    pending_calls: VecDeque<WireToolCall>,
    calculation: Option<PendingCalculation>,
    view: AiView,
    footprint: Vec<Footprint>,
    reads: Vec<ReadObservation>,
    awaiting_write_results: bool,
    retry_check: std::cell::Cell<Option<((u64, u64), bool)>>,
    // Workbook revision at which `footprint` was last verified.
    revision: (u64, u64),
    // On retry, replace just the streamed text from the failed completion.
    content_start: usize,
    reasoning_start: usize,
}

impl ActiveTurn {
    #[cfg(test)]
    pub(super) fn has_pending_calculation(&self) -> bool {
        self.calculation.is_some()
    }
}

fn bounded(mut text: String, bytes: usize) -> String {
    if text.len() > bytes {
        let mut end = bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("…（已截短）");
    }
    text
}

fn read_hash(result: &JsonValue) -> u64 {
    let mut hash = DefaultHasher::new();
    if let Some(hits) = result["hits"].as_array() {
        // The sparse store may reorder unchanged search hits when another page is inserted.
        let mut hits: Vec<_> = hits.iter().collect();
        hits.sort_unstable_by_key(|hit| (hit["sheet"].as_str(), hit["cell"].as_str()));
        hits.hash(&mut hash);
        result["has_more"].hash(&mut hash);
    } else {
        result.hash(&mut hash);
    }
    hash.finish()
}

#[inline]
fn hash_single_cell(
    wb: &sheetcore::Workbook,
    sheet: &sheetcore::model::Sheet,
    r: u32,
    c: u32,
    cell: &Cell,
    values: bool,
    calculated: bool,
    needs_calculation: &mut bool,
) -> u64 {
    let mut h = DefaultHasher::new();
    (r, c).hash(&mut h);
    cell.formula.hash(&mut h);
    if values {
        if !calculated && !*needs_calculation {
            *needs_calculation = cell.formula.is_some()
                || sheet.spilled.contains(&sheetcore::model::CellRef::new(r, c));
        }
        wb.styles.get(sheet.style_of(r, c)).format.hash(&mut h);
    }
    if values || cell.formula.is_none() {
        match &cell.value {
            Value::Number(n) => (2u8, n.to_bits()).hash(&mut h),
            Value::Text(t) => (3u8, t).hash(&mut h),
            Value::Bool(b) => (4u8, b).hash(&mut h),
            Value::Error(e) => (5u8, e.text()).hash(&mut h),
            Value::Empty => 6u8.hash(&mut h),
        }
    }
    h.finish()
}

/// Hash stored cells without expanding sparse selections such as entire columns.
fn target_hash(
    wb: &sheetcore::Workbook,
    si: usize,
    target: &FootprintTarget,
    values: bool,
    calculated: bool,
) -> Option<u64> {
    let sheet = wb.sheet(si);
    let mut needs_calculation = false;
    // Calls on the wire use sheet names. An old name must not silently resolve to
    // a different sheet while a confirmation card is open.
    let mut identity = DefaultHasher::new();
    sheet.name.hash(&mut identity);
    if values {
        wb.date_system.is_1904().hash(&mut identity);
    }
    let mut sum = identity.finish();
    match target {
        FootprintTarget::Rect(rect) => {
            let (mut r1, mut c1, mut r2, mut c2) = *rect;
            let mut area = u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1);
            if area > 4096 {
                // Large targets such as whole columns: only the used part can hold content.
                let Some((ur1, uc1, ur2, uc2)) = sheet.used_bounds() else {
                    return Some(sum);
                };
                (r1, c1, r2, c2) = (r1.max(ur1), c1.max(uc1), r2.min(ur2), c2.min(uc2));
                if r1 > r2 || c1 > c2 {
                    return Some(sum);
                }
                area = u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1);
            }
            if area <= sheet.len() as u64 {
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        if let Some(cell) = sheet.get(r, c).filter(|cell| !cell.is_blank()) {
                            sum = sum.wrapping_add(hash_single_cell(
                                wb,
                                sheet,
                                r,
                                c,
                                cell.as_ref(),
                                values,
                                calculated,
                                &mut needs_calculation,
                            ));
                        }
                    }
                }
            } else {
                for (at, cell) in sheetcore::sync::iter_sheet_rect(sheet, (r1, c1, r2, c2)) {
                    sum = sum.wrapping_add(hash_single_cell(
                        wb,
                        sheet,
                        at.row,
                        at.col,
                        cell.as_ref(),
                        values,
                        calculated,
                        &mut needs_calculation,
                    ));
                }
            }
        }
        FootprintTarget::Cells(coords) => {
            for &(r, c) in coords {
                if let Some(cell) = sheet.get(r, c).filter(|cell| !cell.is_blank()) {
                    sum = sum.wrapping_add(hash_single_cell(
                        wb,
                        sheet,
                        r,
                        c,
                        cell.as_ref(),
                        values,
                        calculated,
                        &mut needs_calculation,
                    ));
                }
            }
        }
    }
    (calculated || !needs_calculation).then_some(sum)
}

impl AiState {
    pub(super) fn update_retry_tool_protocol(&mut self, cfg: &AiConfig) {
        if self.busy {
            return;
        }
        if let Some(turn) = &mut self.active_round {
            let mut updated = turn.cfg.clone();
            updated.native_tools = cfg.native_tools;
            updated.allow_unframed_text = cfg.allow_unframed_text;
            if updated == *cfg {
                turn.cfg = updated;
            }
        }
    }

    fn remember(&mut self, prompt: String, answer: String) {
        self.wire_history.push_back((
            bounded(prompt, HISTORY_BYTES / 4),
            bounded(answer, HISTORY_BYTES / 4),
        ));
        let mut bytes = self
            .wire_history
            .iter()
            .map(|(p, a)| p.len() + a.len())
            .sum::<usize>();
        while self.wire_history.len() > HISTORY_TURNS || bytes > HISTORY_BYTES {
            let Some((prompt, answer)) = self.wire_history.pop_front() else {
                break;
            };
            bytes -= prompt.len() + answer.len();
        }
    }
}

impl SheetApp {
    /// Recalculation requested by an AI write completes that write's baseline. Other
    /// recalculations must be compared with the values previously shown to the model.
    pub(crate) fn ai_recalculated(&mut self, previous_gen: u64) {
        if self.ai.bound_doc_uid == Some(self.doc_uid)
            && let Some(turn) = &self.ai.active_round
            && turn.awaiting_write_results
            && turn.revision == (previous_gen, self.wb_epoch)
        {
            self.refresh_ai_baseline();
        }
    }

    fn ai_view_now(&self) -> AiView {
        AiView {
            sheet: self.wb.sheet(self.sheet_idx).identity,
            rect: self.sel_rect(),
        }
    }

    fn ai_sheet_by_identity(&self, identity: u64) -> Option<usize> {
        self.wb.sheets.iter().position(|s| s.identity == identity)
    }

    pub(crate) fn start_ai_prompt(&mut self, user_text: String) {
        let prompt = user_text.trim().to_owned();
        if prompt.is_empty() || self.ai.busy {
            return;
        }
        if !self.finish_pending_edit() {
            self.ai.status_hint = self.status.clone();
            if self.ai.draft.trim().is_empty() {
                self.ai.draft = prompt;
            }
            return;
        }
        if !self.ai.cfg.has_api_key() {
            self.ai.status_hint = "请先在「设置」里填写模型服务的 API Key".into();
            self.ai.settings_open = true;
            self.ai.draft_cfg = self.ai.cfg.clone();
            if self.ai.draft.trim().is_empty() {
                self.ai.draft = prompt;
            }
            return;
        }
        if self.ai.bound_doc_uid != Some(self.doc_uid) {
            self.ai.wire_history.clear();
        }
        self.ai.bound_doc_uid = Some(self.doc_uid);
        self.ai.calc_base = None;
        let messages = self.ai_prompt_messages(&prompt);
        self.ai.active_round = Some(ActiveTurn {
            prompt: prompt.clone(),
            cfg: self.ai.cfg.clone(),
            messages,
            pending_calls: VecDeque::new(),
            calculation: None,
            view: self.ai_view_now(),
            footprint: Vec::new(),
            reads: Vec::new(),
            awaiting_write_results: false,
            retry_check: Default::default(),
            revision: (self.doc_gen, self.wb_epoch),
            content_start: 0,
            reasoning_start: 0,
        });
        let discard = (self.ai.messages.len() + 2).saturating_sub(2 * DISPLAY_TURNS);
        if discard > 0 {
            self.ai.messages.drain(..discard);
            self.ai.card_heights.clear();
        }
        for (role, content) in [(AiRole::User, prompt), (AiRole::Assistant, String::new())] {
            self.ai.messages.push(AiMessage {
                role,
                content,
                reasoning: String::new(),
                reasoning_open: false,
                tool_logs: Vec::new(),
                error: None,
                undo_ids: Vec::new(),
            });
        }
        self.ai.round = 1;
        self.ai.status_hint = format!("{} 正在分析请求…", self.ai.cfg.display_name);
        self.ai.scroll_bottom = true;
        self.launch_ai_round();
    }

    fn ai_prompt_messages(&self, prompt: &str) -> Vec<JsonValue> {
        let (r1, c1, r2, c2) = self.sel_rect();
        let system = format!(
            "{}\n当前活动工作表序号 {}/{}，选区 {}。",
            system_prompt(),
            self.sheet_idx + 1,
            self.wb.sheets.len(),
            range_label((r1, c1), (r2, c2))
        );
        let mut messages = vec![json!({"role":"system","content":system})];
        for (question, answer) in &self.ai.wire_history {
            messages.push(json!({"role":"user","content":question}));
            messages.push(json!({"role":"assistant","content":answer}));
        }
        messages.push(json!({"role":"user","content":prompt}));
        messages
    }

    fn launch_ai_round(&mut self) {
        let revision = (self.doc_gen, self.wb_epoch);
        let Some(turn) = self.ai.active_round.as_mut() else {
            return;
        };
        let Some(last) = self.ai.messages.last_mut() else {
            return;
        };
        turn.content_start = last.content.len();
        turn.reasoning_start = last.reasoning.len();
        turn.revision = revision;
        let cancel = Arc::new(RequestCancel::default());
        let rx = spawn_completion_round(
            turn.cfg.clone(),
            turn.messages.clone(),
            cancel.clone(),
            self.ai.ui_ctx.clone(),
        );
        self.ai.busy = true;
        self.ai.cancel = Some(cancel);
        self.ai.rx = Some(rx);
    }

    pub(super) fn can_retry_ai_round(&self) -> bool {
        !self.ai.busy
            && self.ai.bound_doc_uid == Some(self.doc_uid)
            && self.ai.active_round.as_ref().is_some_and(|turn| {
                if turn.cfg != self.ai.cfg {
                    return false;
                }
                let revision = (self.doc_gen, self.wb_epoch);
                if let Some((checked, valid)) = turn.retry_check.get()
                    && checked == revision
                {
                    return valid;
                }
                // A stale manual calculation is checked in the background when retry is clicked.
                let valid = self.ai_validation_needs_calculation(turn) || self.ai_turn_conflict(turn).is_none();
                turn.retry_check.set(Some((revision, valid)));
                valid
            })
    }

    pub(super) fn can_retry_ai_round_ui(&self) -> bool {
        !self.ai.busy
            && self.ai.bound_doc_uid == Some(self.doc_uid)
            && self.ai.active_round.as_ref().is_some_and(|turn| {
                if turn.cfg != self.ai.cfg || turn.revision.1 != self.wb_epoch {
                    return false;
                }
                if self.ai_sheet_by_identity(turn.view.sheet).is_none() {
                    return false;
                }
                let revision = (self.doc_gen, self.wb_epoch);
                if turn.revision == revision {
                    return true;
                }
                if let Some((checked, valid)) = turn.retry_check.get()
                    && checked == revision
                {
                    return valid;
                }
                let has_heavy_preview = turn.reads.iter().any(|r| r.call.name == "eval_preview")
                    && self.wb.sheets.iter().map(|s| s.len()).sum::<usize>() > 10_000;
                if has_heavy_preview {
                    return true;
                }
                self.can_retry_ai_round()
            })
    }

    pub(super) fn retry_failed_ai_round(&mut self) {
        if !self.can_retry_ai_round() {
            return;
        }
        let ctx = self.ai.ui_ctx.clone().unwrap_or_default();
        self.validate_ai_action(TurnAction::Retry, &ctx);
    }

    fn resume_failed_ai_round(&mut self) {
        let Some(turn) = &self.ai.active_round else {
            return;
        };
        if let Some(last) = self.ai.messages.last_mut() {
            last.error = None;
            last.content.truncate(turn.content_start);
            last.reasoning.truncate(turn.reasoning_start);
        }
        self.ai.status_hint = format!("正在重试第 {} 轮…", self.ai.round);
        self.launch_ai_round();
    }

    pub(crate) fn stop_ai_generation(&mut self) {
        if let Some(c) = self.ai.cancel.take() {
            c.cancel();
        }
        self.ai.busy = false;
        self.ai.rx = None;
        self.ai.active_round = None;
        self.ai.calc_base = None;
        self.ai.confirmation_label = None;
        self.ai.pending_preview = None;
        if let Some(last) = self.ai.messages.last_mut()
            && last.role == AiRole::Assistant
            && last.content.is_empty()
            && last.error.is_none()
        {
            last.content = "已停止生成。".into();
        }
        self.ai.status_hint = "已停止生成".into();
    }

    fn fail_ai_round(&mut self, error: String, retry: bool) {
        self.ai.rx = None;
        self.ai.cancel = None;
        self.ai.busy = false;
        self.ai.confirmation_label = None;
        self.ai.pending_preview = None;
        if !retry {
            self.ai.active_round = None;
            self.ai.calc_base = None;
        }
        self.ai.status_hint = error.clone();
        if let Some(last) = self.ai.messages.last_mut() {
            last.error = Some(error);
        }
    }

    pub(crate) fn poll_ai(&mut self, ctx: &egui::Context) {
        self.ai.ui_ctx = Some(ctx.clone());
        if !self.ai.busy {
            return;
        }
        if self.ai.bound_doc_uid != Some(self.doc_uid) {
            self.stop_ai_generation();
            self.fail_ai_round("活动工作簿已切换，本轮已终止。".into(), false);
            return;
        }
        let calculation = self.ai.active_round.as_mut().and_then(|t| t.calculation.take());
        if let Some(mut calculation) = calculation {
            if let PendingCalculation::Reorder {
                call,
                sheet_id,
                op_desc,
                range_lbl,
                undo_checkpoint,
                before_gen,
            } = calculation
            {
                if self.ai_sheet_by_identity(sheet_id).is_none() {
                    self.fail_ai_round("数据操作期间目标工作表已删除，本轮已终止。".into(), false);
                    return;
                }
                self.poll_reorder();
                if self.tasks.reorder.is_some() {
                    self.ai.status_hint = format!("正在执行{op_desc}…");
                    if let Some(turn) = self.ai.active_round.as_mut() {
                        turn.calculation = Some(PendingCalculation::Reorder {
                            call,
                            sheet_id,
                            op_desc,
                            range_lbl,
                            undo_checkpoint,
                            before_gen,
                        });
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                    return;
                }
                if let Some(last) = self.ai.messages.last_mut() {
                    last.undo_ids.extend(self.undo.ids_since(undo_checkpoint));
                }
                let changed = self.doc_gen != before_gen;
                if changed {
                    self.ai.calc_base = None;
                }
                let noop_ok = !changed
                    && (self.status.starts_with("已按") || self.status == "没有重复行");
                let (label, result) = if changed || noop_ok {
                    (
                        format!("{op_desc} {range_lbl}"),
                        json!({"ok": true, "status": self.status}),
                    )
                } else {
                    (
                        format!("{op_desc} {range_lbl} 未执行"),
                        json!({"ok": false, "status": self.status}),
                    )
                };
                let needs_calc = changed
                    && self.ai_calculated_workbook().is_none()
                    && self.ai.active_round.as_ref().is_some_and(|turn| {
                        turn.reads.iter().any(|r| r.call.name != "inspect_workbook")
                            || turn.footprint.iter().any(|f| f.values)
                    });
                if needs_calc {
                    let (c_label, c_result) = (label.clone(), result.clone());
                    if let AiToolOutput::Calculating(job) = self.with_calculated_ai_view(
                        ctx,
                        Box::new(move |_| (c_label.clone(), c_result.clone())),
                    ) {
                        self.note_ai_footprint(&call.spec, &JsonValue::Null);
                        self.refresh_ai_baseline();
                        if let Some(turn) = self.ai.active_round.as_mut() {
                            turn.awaiting_write_results = true;
                            turn.calculation = Some(PendingCalculation::Tool {
                                call,
                                sheet: Some(sheet_id),
                                job,
                            });
                        }
                        self.ai.status_hint = "正在核对表格计算结果…".into();
                        return;
                    }
                }
                self.record_ai_tool_result(&call, label, result);
                self.continue_ai_tools(ctx, false);
                return;
            }
            let job = match &mut calculation {
                PendingCalculation::Tool { call, sheet, job } => {
                    if self.ai_read_sheet(&call.spec) != *sheet {
                        self.fail_ai_round("核对期间工具的目标工作表已变化，本轮已终止。".into(), false);
                        return;
                    }
                    job
                }
                PendingCalculation::WriteBatch { calls, job } => {
                    if calls.iter().any(|(c, sh)| self.ai_read_sheet(&c.spec) != *sh) {
                        self.fail_ai_round("核对期间工具的目标工作表已变化，本轮已终止。".into(), false);
                        return;
                    }
                    job
                }
                PendingCalculation::Validate { job, .. } => job,
                PendingCalculation::Reorder { .. } => unreachable!(),
            };
            match self.poll_ai_calculation(job, ctx) {
                CalcPoll::Pending(progress) => {
                    self.ai.status_hint = match progress {
                        Some((done, total)) => format!(
                            "正在核对表格计算结果… {}%",
                            done.min(total) * 100 / total.max(1)
                        ),
                        None => "正在核对表格计算结果…".into(),
                    };
                    if let Some(turn) = self.ai.active_round.as_mut() {
                        turn.calculation = Some(calculation);
                    }
                }
                CalcPoll::Failed(error) => self.fail_ai_round(format!("{error}，本轮已终止。"), false),
                CalcPoll::Ready(label, result) => match calculation {
                    PendingCalculation::Validate { action, .. } => self.validate_ai_action(action, ctx),
                    PendingCalculation::Tool { call, .. } => {
                        // poll_ai_calculation only returns observations from a current calculated view.
                        if let Err(conflict) = self.ai_turn_still_valid() {
                            self.fail_ai_round(format!("{conflict}，本轮已终止。"), false);
                        } else {
                            self.record_ai_tool_result(&call, label, result);
                            self.continue_ai_tools(ctx, false);
                        }
                    }
                    PendingCalculation::WriteBatch { calls, .. } => {
                        if let Err(conflict) = self.ai_turn_still_valid() {
                            self.fail_ai_round(format!("{conflict}，本轮已终止。"), false);
                        } else {
                            self.record_ai_write_batch_results(&calls, result);
                            self.continue_ai_tools(ctx, false);
                        }
                    }
                    PendingCalculation::Reorder { .. } => unreachable!(),
                },
            }
            return;
        }
        while let Some(rx) = &self.ai.rx {
            match rx.try_recv() {
                Ok(StreamMsg::ReasoningDelta(text)) => {
                    if let Some(last) = self.ai.messages.last_mut() {
                        last.reasoning.push_str(&text);
                    }
                }
                Ok(StreamMsg::ContentDelta(text)) => {
                    if let Some(last) = self.ai.messages.last_mut() {
                        last.content.push_str(&text);
                    }
                }
                Ok(StreamMsg::RoundDone {
                    visible_content,
                    native_reasoning,
                    tool_calls,
                }) => {
                    self.ai.rx = None;
                    self.ai.cancel = None;
                    self.finish_ai_round(visible_content, native_reasoning, tool_calls, ctx);
                    break; // A follow-up round can already own a new receiver.
                }
                Ok(StreamMsg::Failed(error)) => {
                    self.fail_ai_round(error, true);
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    self.fail_ai_round("模型请求线程意外退出，已保留收到的正文。".into(), true);
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
            self.ai.scroll_bottom = true;
        }
    }

    fn finish_ai_round(
        &mut self,
        visible: String,
        native_reasoning: Option<String>,
        calls: Result<Vec<WireToolCall>, String>,
        ctx: &egui::Context,
    ) {
        self.validate_ai_action(TurnAction::Finish { visible, reasoning: native_reasoning, calls }, ctx);
    }

    fn ai_validation_needs_calculation(&self, turn: &ActiveTurn) -> bool {
        turn.revision.1 == self.wb_epoch
            && turn.revision.0 != self.doc_gen
            && self.ai_calculated_workbook().is_none()
            && (turn.reads.iter().any(|r| r.call.name != "inspect_workbook")
                || turn.footprint.iter().any(|f| f.values
                    && (f.hash.is_none() || self.ai_region_hash(f) != f.hash)))
    }

    fn validate_ai_action(&mut self, action: TurnAction, ctx: &egui::Context) {
        let Some(turn) = self.ai.active_round.as_ref() else {
            return;
        };
        if self.ai_validation_needs_calculation(turn)
            && let AiToolOutput::Calculating(job) = self.with_calculated_ai_view(
                ctx, Box::new(|_| (String::new(), JsonValue::Null)),
            )
        {
            self.ai.active_round.as_mut().unwrap().calculation = Some(PendingCalculation::Validate { action, job });
            self.ai.busy = true;
            self.ai.status_hint = "正在核对已读取的表格数据…".into();
            return;
        }
        if let Err(conflict) = self.ai_turn_still_valid() {
            self.fail_ai_round(format!("{conflict}，本轮已终止，请重新提问。"), false);
            return;
        }
        match action {
            TurnAction::Finish { visible, reasoning, calls } => self.apply_ai_round(visible, reasoning, calls, ctx),
            TurnAction::Confirm => self.continue_ai_tools(ctx, true),
            TurnAction::Retry => self.resume_failed_ai_round(),
        }
    }

    fn apply_ai_round(
        &mut self,
        visible: String,
        native_reasoning: Option<String>,
        calls: Result<Vec<WireToolCall>, String>,
        ctx: &egui::Context,
    ) {
        if matches!(&calls, Ok(calls) if calls.is_empty()) {
            if let Some(turn) = self.ai.active_round.take() {
                if let Some(last) = self.ai.messages.last_mut() {
                    last.content = visible.clone();
                }
                self.ai.remember(turn.prompt, visible);
            }
            self.ai.calc_base = None;
            self.ai.busy = false;
            self.ai.status_hint = "已完成".into();
            return;
        }
        if self.ai.round >= MAX_TOOL_ROUNDS {
            self.fail_ai_round(
                format!("已达到 {MAX_TOOL_ROUNDS} 轮上限，最后一轮的工具调用未执行。"),
                false,
            );
            return;
        }
        let Some(turn) = self.ai.active_round.as_mut() else {
            return;
        };
        match calls {
            Err(error) => {
                // A malformed batch performs no edits. Tell the model how to repair its next call.
                // Some chat templates only accept a system message at the start.
                turn.messages
                    .push(json!({"role":"assistant","content":visible}));
                turn.messages.push(json!({"role":"user","content":format!("[程序提示] 上一轮工具调用解析失败，整批未执行。请修正后重新调用。{error}")}));
                if let Some(last) = self.ai.messages.last_mut() {
                    last.tool_logs.push(error);
                }
                self.next_ai_round();
            }
            Ok(mut calls) => {
                let mut ids: BTreeSet<_> = calls.iter().map(|c| c.id.clone()).collect();
                for (i, call) in calls.iter_mut().enumerate() {
                    if call.id.is_empty() {
                        let mut id = format!("call_{}_{}", self.ai.round, i);
                        while !ids.insert(id.clone()) {
                            id.push('_');
                        }
                        call.id = id;
                    }
                }
                let wire_calls: Vec<_> = calls.iter().map(WireToolCall::as_json).collect();
                let mut message =
                    json!({"role":"assistant","content":visible,"tool_calls":wire_calls});
                if let Some(reasoning) = native_reasoning {
                    message["reasoning_content"] = json!(reasoning);
                }
                turn.messages.push(message);
                turn.pending_calls = calls.into();
                self.continue_ai_tools(ctx, false);
            }
        }
    }

    pub(super) fn answer_ai_confirmation(&mut self, approved: bool, ctx: &egui::Context) {
        if self.ai.confirmation_label.is_none() {
            return;
        }
        self.ai.confirmation_label = None;
        self.ai.pending_preview = None;
        if !approved {
            self.fail_ai_round("已取消待确认的操作，本轮剩余工具未执行。".into(), false);
            return;
        }
        if self.ai.bound_doc_uid != Some(self.doc_uid) || self.ai.active_round.is_none() {
            self.fail_ai_round("工作簿在等待确认时已切换，操作已终止。".into(), false);
            return;
        }
        // The card showed resolved targets; they are part of the footprint.
        self.validate_ai_action(TurnAction::Confirm, ctx);
    }

    fn continue_ai_tools(&mut self, ctx: &egui::Context, mut approved: bool) {
        loop {
            let Some(turn) = self.ai.active_round.as_mut() else {
                return;
            };
            let view = turn.view;
            let Some(mut call) = turn.pending_calls.pop_front() else {
                self.next_ai_round();
                return;
            };
            if !std::mem::take(&mut approved) {
                let staged = self
                    .pin_ai_call(view, &mut call.spec)
                    .and_then(|()| self.stage_ai_confirmation(&mut call.spec));
                match staged {
                    Ok(Some(label)) => {
                        // Guard the cells shown on the card until the user answers.
                        let preview = self.build_ai_pending_preview(&call.spec);
                        self.note_ai_footprint(&call.spec, &JsonValue::Null);
                        self.refresh_ai_baseline();
                        if let Some(turn) = self.ai.active_round.as_mut() {
                            turn.pending_calls.push_front(call);
                        }
                        self.ai.confirmation_label = Some(label);
                        self.ai.pending_preview = preview;
                        self.ai.status_hint = "正在等待你确认表格操作".into();
                        return;
                    }
                    Err(error) => {
                        self.record_ai_observation(
                            call.id,
                            error.clone(),
                            json!({"ok":false,"error":error}),
                        );
                        continue;
                    }
                    Ok(None) => {}
                }
            }
            if matches!(call.spec.name.as_str(), "set_cells" | "fill_formula_range") {
                let first_sheet = self.ai_read_sheet(&call.spec);
                let before_rev = self.ai_calc_revision();
                let (first_written, mut all_touched) =
                    match self.apply_ai_write_tool(&call.spec, true) {
                        Ok(ok) => ok,
                        Err((label, result)) => {
                            self.record_ai_tool_result(&call, label, result);
                            continue;
                        }
                    };
                let mut batch = vec![(call, first_sheet, first_written)];
                loop {
                    let Some(mut next_spec) = self
                        .ai
                        .active_round
                        .as_ref()
                        .and_then(|t| t.pending_calls.front())
                        .filter(|c| {
                            matches!(c.spec.name.as_str(), "set_cells" | "fill_formula_range")
                        })
                        .map(|c| c.spec.clone())
                    else {
                        break;
                    };
                    let can_batch = self
                        .pin_ai_call(view, &mut next_spec)
                        .and_then(|()| self.stage_ai_confirmation(&mut next_spec));
                    if !matches!(can_batch, Ok(None)) {
                        break;
                    }
                    let Ok((written_n, touched_n)) = self.apply_ai_write_tool(&next_spec, true)
                    else {
                        break;
                    };
                    let Some(mut next_call) = self
                        .ai
                        .active_round
                        .as_mut()
                        .and_then(|t| t.pending_calls.pop_front())
                    else {
                        break;
                    };
                    next_call.spec = next_spec;
                    let next_sheet = self.ai_read_sheet(&next_call.spec);
                    all_touched.extend(touched_n);
                    batch.push((next_call, next_sheet, written_n));
                }
                if batch.len() > 1 {
                    let mut seen = HashSet::with_capacity(all_touched.len());
                    all_touched.retain(|k| seen.insert(*k));
                }
                self.fast_recalc_cells(&all_touched);
                {
                    let refs: Vec<(u64, &[(u32, u32)])> = batch
                        .iter()
                        .map(|(_, _, w)| (w.sheet_id, w.keys.as_slice()))
                        .collect();
                    self.note_ai_written_batch(before_rev, &refs);
                }
                if batch.len() == 1 {
                    let (call, sheet, written) = batch.pop().unwrap();
                    match self.with_calculated_ai_view(
                        ctx,
                        Box::new(move |view| written.result(view.wb)),
                    ) {
                        AiToolOutput::Ready(label, result) => {
                            self.record_ai_tool_result(&call, label, result);
                        }
                        AiToolOutput::Calculating(calculation) => {
                            self.note_ai_footprint(&call.spec, &JsonValue::Null);
                            self.refresh_ai_baseline();
                            if let Some(turn) = self.ai.active_round.as_mut() {
                                turn.awaiting_write_results = true;
                                turn.calculation = Some(PendingCalculation::Tool {
                                    call,
                                    job: calculation,
                                    sheet,
                                });
                            }
                            self.ai.status_hint = "正在核对表格计算结果…".into();
                            return;
                        }
                        AiToolOutput::Reordering { .. } => unreachable!(),
                    }
                } else {
                    let mut calls = Vec::with_capacity(batch.len());
                    let mut writtens = Vec::with_capacity(batch.len());
                    for (c, sh, w) in batch {
                        calls.push((c, sh));
                        writtens.push(w);
                    }
                    let writtens = Arc::new(writtens);
                    match self.with_calculated_ai_view(
                        ctx,
                        Box::new(move |view| {
                            let items: Vec<JsonValue> = writtens
                                .iter()
                                .map(|w| {
                                    let (lbl, res) = w.result(view.wb);
                                    json!({"label": lbl, "result": res})
                                })
                                .collect();
                            (String::new(), JsonValue::Array(items))
                        }),
                    ) {
                        AiToolOutput::Ready(_, result) => {
                            self.record_ai_write_batch_results(&calls, result);
                        }
                        AiToolOutput::Calculating(calculation) => {
                            for (c, _) in &calls {
                                self.note_ai_footprint(&c.spec, &JsonValue::Null);
                            }
                            self.refresh_ai_baseline();
                            if let Some(turn) = self.ai.active_round.as_mut() {
                                turn.awaiting_write_results = true;
                                turn.calculation = Some(PendingCalculation::WriteBatch {
                                    calls,
                                    job: calculation,
                                });
                            }
                            self.ai.status_hint = "正在核对表格计算结果…".into();
                            return;
                        }
                        AiToolOutput::Reordering { .. } => unreachable!(),
                    }
                }
                continue;
            }
            let sheet = self.ai_read_sheet(&call.spec);
            match self.execute_ai_tool(&call.spec, ctx) {
                AiToolOutput::Ready(label, result) => {
                    self.record_ai_tool_result(&call, label, result)
                }
                AiToolOutput::Calculating(calculation) => {
                    // Writes have already happened; calculation only reads back their results.
                    // Protect the written inputs while the calculation is running.
                    let wrote_cells = matches!(call.spec.name.as_str(), "set_cells" | "fill_formula_range");
                    if wrote_cells {
                        self.note_ai_footprint(&call.spec, &JsonValue::Null);
                        self.refresh_ai_baseline();
                    }
                    if let Some(turn) = self.ai.active_round.as_mut() {
                        turn.awaiting_write_results = wrote_cells;
                        turn.calculation = Some(PendingCalculation::Tool { call, job: calculation, sheet });
                    }
                    self.ai.status_hint = "正在核对表格计算结果…".into();
                    return;
                }
                AiToolOutput::Reordering {
                    sheet_id,
                    op_desc,
                    range_lbl,
                    undo_checkpoint,
                    before_gen,
                } => {
                    // Guard observed footprint prior to reorder; poll_ai will refresh baseline once poll_reorder finishes.
                    self.note_ai_footprint(&call.spec, &JsonValue::Null);
                    self.refresh_ai_baseline();
                    if let Some(turn) = self.ai.active_round.as_mut() {
                        turn.calculation = Some(PendingCalculation::Reorder {
                            call,
                            sheet_id,
                            op_desc: op_desc.clone(),
                            range_lbl,
                            undo_checkpoint,
                            before_gen,
                        });
                    }
                    self.ai.status_hint = format!("正在执行{op_desc}…");
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                    return;
                }
            }
        }
    }

    /// Fill omitted sheet and range arguments from the view the model was given.
    pub(super) fn pin_ai_call(&self, view: AiView, call: &mut ToolCallSpec) -> Result<(), String> {
        let name = call.name.as_str();
        // Search without a sheet covers the workbook. An explicitly empty name means
        // the active sheet; bind it to the view the model was given.
        let search_active = matches!(name, "search_cells" | "inspect_comments")
            && call.arguments["sheet"].as_str().is_some_and(|s| s.trim().is_empty());
        let takes_sheet = matches!(
            name,
            "inspect_workbook"
                | "inspect_sheet"
                | "eval_preview"
                | "set_cells"
                | "fill_formula_range"
                | "style_range"
                | "data_operation"
                | "manage_comments"
        );
        let Some(args) = call.arguments.as_object_mut().filter(|_| takes_sheet || search_active) else {
            return Ok(());
        };
        let omitted = |value: Option<&JsonValue>| {
            value.is_none_or(|v| v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty()))
        };
        let label = range_label((view.rect.0, view.rect.1), (view.rect.2, view.rect.3));
        if omitted(args.get("sheet")) {
            let index = self
                .ai_sheet_by_identity(view.sheet)
                .ok_or("提问时的活动工作表已不存在")?;
            args.insert("sheet".into(), json!(self.wb.sheet(index).name));
            if name == "inspect_workbook" {
                args.insert("selection".into(), json!(label));
            }
        } else if args.get("sheet").is_some_and(|v| !v.is_string()) {
            return Err("工作表名称无效".into());
        }
        if name == "inspect_workbook" {
            let index = self.resolve_sheet_index(args.get("sheet").and_then(JsonValue::as_str))
                .ok_or("未找到指定工作表")?;
            if args.get("selection").and_then(JsonValue::as_str).and_then(checked_ai_range).is_none() {
                let rect = if self.wb.sheet(index).identity == view.sheet {
                    view.rect
                } else {
                    (0, 0, 0, 0)
                };
                args.insert("selection".into(), json!(range_label((rect.0, rect.1), (rect.2, rect.3))));
            }
        }
        if name == "data_operation" && omitted(args.get("range")) {
            args.insert("range".into(), json!(label));
        }
        Ok(())
    }

    /// Regions a finished (or staged) call depends on.
    fn ai_tool_footprint(&self, call: &ToolCallSpec, result: &JsonValue) -> Vec<(u64, FootprintTarget)> {
        let args = &call.arguments;
        let sheet = |name: &JsonValue| name.as_str().and_then(|n| self.resolve_sheet_index(Some(n)));
        let identity = |index: usize| self.wb.sheet(index).identity;
        let mut regions = Vec::new();
        match call.name.as_str() {
            "inspect_sheet" => {
                let rect = result["range"].as_str().and_then(checked_ai_range);
                if let (Some(si), Some(rect)) = (sheet(&result["sheet"]), rect) {
                    regions.push((identity(si), FootprintTarget::Rect(rect)));
                }
            }
            "set_cells" => {
                if let Some(si) = sheet(&args["sheet"]) {
                    let mut coords: Vec<(u32, u32)> = args["edits"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|edit| edit["cell"].as_str().and_then(checked_address))
                        .collect();
                    if !coords.is_empty() {
                        coords.sort_unstable();
                        coords.dedup();
                        let target = if coords.len() == 1 {
                            let (r, c) = coords[0];
                            FootprintTarget::Rect((r, c, r, c))
                        } else {
                            let (mut r1, mut c1) = coords[0];
                            let (mut r2, mut c2) = (r1, c1);
                            for &(r, c) in coords.iter().skip(1) {
                                r1 = r1.min(r);
                                c1 = c1.min(c);
                                r2 = r2.max(r);
                                c2 = c2.max(c);
                            }
                            let area = u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1);
                            if area == coords.len() as u64 {
                                FootprintTarget::Rect((r1, c1, r2, c2))
                            } else {
                                FootprintTarget::Cells(coords)
                            }
                        };
                        regions.push((identity(si), target));
                    }
                }
            }
            "fill_formula_range" | "data_operation" => {
                let rect = args["range"].as_str().and_then(checked_ai_range);
                if let (Some(si), Some(rect)) = (sheet(&args["sheet"]), rect) {
                    regions.push((identity(si), FootprintTarget::Rect(rect)));
                }
            }
            _ => {}
        }
        regions
    }

    fn note_ai_footprint(&mut self, call: &ToolCallSpec, result: &JsonValue) {
        let regions = self.ai_tool_footprint(call, result);
        let Some(turn) = self.ai.active_round.as_mut() else {
            return;
        };
        let values = !result.is_null();
        let mut known: HashSet<_> = turn
            .footprint
            .iter()
            .map(|f| (f.sheet, f.target.clone(), f.values))
            .collect();
        for (sheet, target) in regions {
            if known.insert((sheet, target.clone(), values)) {
                turn.footprint.push(Footprint {
                    sheet,
                    target,
                    values,
                    hash: None,
                    snap: std::cell::RefCell::new(None),
                });
            }
        }
    }

    fn ai_region_hash(&self, f: &Footprint) -> Option<u64> {
        self.ai_region_hash_with_snaps(f, None)
    }

    fn ai_region_hash_with_snaps(
        &self,
        f: &Footprint,
        shared_snaps: Option<&mut std::collections::HashMap<(u64, bool), std::rc::Rc<FootprintSnap>>>,
    ) -> Option<u64> {
        let calculated = self.ai_calculated_workbook();
        let wb = if f.values { calculated.unwrap_or(&self.wb) } else { &self.wb };
        let index = wb.sheets.iter().position(|s| s.identity == f.sheet)?;
        let sh = wb.sheet(index);
        let calc_flag = calculated.is_some();
        let date_1904 = wb.date_system.is_1904();
        if let Some((cached_hash, ref prior)) = *f.snap.borrow()
            && prior.sheet.name == sh.name
            && prior.calculated == calc_flag
            && (!f.values || prior.date_1904 == date_1904)
            && (!f.values || calc_flag || sh.spilled.is_empty())
        {
            let cells_same = sh.cell_version() == prior.sheet.cell_version()
                || match &f.target {
                    FootprintTarget::Rect(rect) => {
                        sh.same_cells_in(&prior.sheet, &[*rect], true)
                            && sh.same_cells_in(&prior.sheet, &[*rect], false)
                    }
                    FootprintTarget::Cells(_) => false,
                };
            let styles_same = !f.values
                || (sh.display_style_revision() == prior.sheet.display_style_revision()
                    && wb.styles.get(0).format == prior.styles.get(0).format)
                || match &f.target {
                    FootprintTarget::Rect(rect) => {
                        sh.same_display_styles_in(&prior.sheet, &[*rect], &wb.styles, &prior.styles)
                    }
                    FootprintTarget::Cells(_) => false,
                };
            if cells_same && styles_same {
                return cached_hash;
            }
        }
        let computed = target_hash(wb, index, &f.target, f.values, calc_flag);
        let snap = if let Some(snaps) = shared_snaps {
            snaps
                .entry((f.sheet, f.values))
                .or_insert_with(|| {
                    std::rc::Rc::new(FootprintSnap {
                        sheet: sh.search_snapshot(),
                        styles: wb.styles.clone(),
                        calculated: calc_flag,
                        date_1904,
                    })
                })
                .clone()
        } else {
            std::rc::Rc::new(FootprintSnap {
                sheet: sh.search_snapshot(),
                styles: wb.styles.clone(),
                calculated: calc_flag,
                date_1904,
            })
        };
        *f.snap.borrow_mut() = Some((computed, snap));
        computed
    }

    fn ai_read_sheet(&self, call: &ToolCallSpec) -> Option<u64> {
        let name = call.arguments["sheet"].as_str()?;
        self.resolve_sheet_index(Some(name)).map(|index| self.wb.sheet(index).identity)
    }

    fn ai_search_hash_incremental(
        &self,
        read: &ReadObservation,
        wb: &sheetcore::Workbook,
    ) -> Option<u64> {
        const HIT_LIMIT: usize = 30;
        const BAND_ROWS: u32 = sheetcore::store::PAGE_ROWS;
        let query = read
            .call
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            let view = WorkbookToolView { wb, sheet_idx: self.sheet_idx };
            let (_, result) = view.tool_search_cells(&read.call.arguments);
            return Some(read_hash(&result));
        }
        let target_sheets: Vec<usize> =
            match read.call.arguments.get("sheet").and_then(|v| v.as_str()) {
                Some(name) => {
                    match (WorkbookToolView { wb, sheet_idx: self.sheet_idx })
                        .resolve_sheet_index(Some(name))
                    {
                        Some(i) => vec![i],
                        None => {
                            return Some(read_hash(
                                &json!({"ok": false, "error": "未找到工作表"}),
                            ));
                        }
                    }
                }
                None => (0..wb.sheets.len()).collect(),
            };
        let mut cache_ref = read.cache.borrow_mut();
        if !matches!(&*cache_ref, ReadCache::Search { .. }) {
            *cache_ref = ReadCache::Search {
                prepared: super::sheet_results::PreparedSearchQuery::new(query),
                sheets: Vec::new(),
            };
        }
        let ReadCache::Search { prepared, sheets: cached_sheets } = &mut *cache_ref else {
            unreachable!()
        };
        let mut old_sheets = std::mem::take(cached_sheets);
        let mut next_sheets = Vec::with_capacity(target_sheets.len().max(old_sheets.len()));
        let mut hits = Vec::with_capacity(HIT_LIMIT);
        let mut has_more = false;

        'sheets: for si in target_sheets {
            let sh = wb.sheet(si);
            let mut entry = match old_sheets.iter().position(|c| c.identity == sh.identity) {
                Some(pos) => old_sheets.swap_remove(pos),
                None => SheetBandSearchCache {
                    identity: sh.identity,
                    snapshot: sh.search_snapshot(),
                    scanned_bands: 0,
                    bands: std::collections::BTreeMap::new(),
                },
            };
            let sh_max = sh.used_bounds().map(|(_, _, r2, _)| r2 / BAND_ROWS);
            let cached_max = entry.bands.keys().next_back().copied();
            let max_band = match (sh_max, cached_max) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            let Some(last_band) = max_band else {
                entry.snapshot = sh.search_snapshot();
                entry.scanned_bands = u32::MAX;
                entry.bands.clear();
                next_sheets.push(entry);
                continue;
            };
            let same_ver = sh.cell_version() == entry.snapshot.cell_version();
            if same_ver && entry.scanned_bands > last_band {
                for band_vec in entry.bands.values() {
                    for (r, c, val, form) in band_vec {
                        if hits.len() == HIT_LIMIT {
                            has_more = true;
                            next_sheets.push(entry);
                            break 'sheets;
                        }
                        hits.push(json!({
                            "sheet": sh.name,
                            "cell": cell_name(*r, *c),
                            "value": val,
                            "formula": form,
                        }));
                    }
                }
                next_sheets.push(entry);
                continue;
            }
            let mut stopped_early = false;
            let mut b = 0u32;
            while b <= last_band {
                let band_start = b * BAND_ROWS;
                let band_end = band_start.saturating_add(BAND_ROWS - 1);
                let rect = [(band_start, 0, band_end, u32::MAX)];
                let reuse_band = b < entry.scanned_bands
                    && (same_ver
                        || (sh.same_cells_in(&entry.snapshot, &rect, false)
                            && sh.same_cells_in(&entry.snapshot, &rect, true)));
                if !reuse_band {
                    let mut band_hits = Vec::new();
                    for (cr, cell) in
                        sheetcore::sync::iter_sheet_rect(sh, (band_start, 0, band_end, u32::MAX))
                    {
                        if prepared.matches_cell(cell.as_ref()) {
                            band_hits.push((
                                cr.row,
                                cr.col,
                                cell.value.display(),
                                cell.formula.as_deref().map(|s| format!("={s}")),
                            ));
                            if band_hits.len() > HIT_LIMIT {
                                break;
                            }
                        }
                    }
                    if band_hits.is_empty() {
                        entry.bands.remove(&b);
                    } else {
                        entry.bands.insert(b, band_hits);
                    }
                }
                if let Some(band_vec) = entry.bands.get(&b) {
                    for (r, c, val, form) in band_vec {
                        if hits.len() == HIT_LIMIT {
                            has_more = true;
                            stopped_early = true;
                            break;
                        }
                        hits.push(json!({
                            "sheet": sh.name,
                            "cell": cell_name(*r, *c),
                            "value": val,
                            "formula": form,
                        }));
                    }
                }
                b += 1;
                if stopped_early {
                    break;
                }
            }
            if stopped_early {
                if !same_ver {
                    let _ = entry.bands.split_off(&b);
                    entry.scanned_bands = b;
                    entry.snapshot = sh.search_snapshot();
                } else {
                    entry.scanned_bands = entry.scanned_bands.max(b);
                }
                next_sheets.push(entry);
                break 'sheets;
            }
            let _ = entry.bands.split_off(&(last_band.saturating_add(1)));
            entry.scanned_bands = u32::MAX;
            entry.snapshot = sh.search_snapshot();
            next_sheets.push(entry);
        }
        next_sheets.extend(old_sheets);
        *cached_sheets = next_sheets;
        Some(read_hash(&json!({"ok": true, "has_more": has_more, "hits": hits})))
    }

    // Re-run a read against a current calculated view. The complete search result
    // includes absence of matches, and a preview may use dynamic or cross-sheet references.
    fn ai_read_hash_obs(&self, read: &ReadObservation) -> Option<u64> {
        if read.call.name == "inspect_workbook" {
            let index = self.resolve_sheet_index(read.call.arguments["sheet"].as_str())?;
            let rect = read.call.arguments["selection"].as_str().and_then(checked_ai_range)?;
            let calc_pending = self.wb.calc_pending || self.tasks.recalc_pending();
            let date_1904 = self.wb.date_system.is_1904();
            let default_format = self.wb.styles.get(0).format.clone();
            let cur_revs: Vec<_> = self
                .wb
                .sheets
                .iter()
                .map(|s| {
                    (
                        s.identity,
                        s.name.clone(),
                        s.cell_version().clone(),
                        s.display_style_revision(),
                        s.cols.default_size().to_bits(),
                        s.rows.default_size().to_bits(),
                    )
                })
                .collect();
            if let ReadCache::Overview {
                ref wb_path,
                date_1904: prev_1904,
                calc_pending: prev_pending,
                default_format: ref prev_fmt,
                ref sheet_revs,
                hash,
            } = *read.cache.borrow()
                && *wb_path == self.wb.path
                && prev_1904 == date_1904
                && prev_pending == calc_pending
                && *prev_fmt == default_format
                && *sheet_revs == cur_revs
            {
                return Some(hash);
            }
            let overview = build_workbook_context(self, index, rect);
            let h = read_hash(&json!({"ok":true, "overview":overview}));
            *read.cache.borrow_mut() = ReadCache::Overview {
                wb_path: self.wb.path.clone(),
                date_1904,
                calc_pending,
                default_format,
                sheet_revs: cur_revs,
                hash: h,
            };
            return Some(h);
        }
        if read.call.name == "inspect_comments" {
            let (_, result) = self.tool_inspect_comments(&read.call.arguments);
            return Some(read_hash(&result));
        }
        let wb = self.ai_calculated_workbook()?;
        match read.call.name.as_str() {
            "search_cells" => self.ai_search_hash_incremental(read, wb),
            "eval_preview" => {
                let f = read
                    .call
                    .arguments
                    .get("formula")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let view = WorkbookToolView { wb, sheet_idx: self.sheet_idx };
                if let Some(si) = view
                    .resolve_sheet_index(read.call.arguments.get("sheet").and_then(|v| v.as_str()))
                {
                    let sh = wb.sheet(si);
                    let other_tables = wb
                        .sheets
                        .iter()
                        .enumerate()
                        .any(|(j, s)| j != si && !s.tables.is_empty());
                    let fb = f.as_bytes();
                    let icase = |n: &[u8]| {
                        fb.len() >= n.len()
                            && fb.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
                    };
                    let self_contained = !f.is_empty()
                        && wb.names.is_empty()
                        && !f.contains('!')
                        && (!other_tables || !f.contains('['))
                        && !icase(b"indirect")
                        && !icase(b"rand")
                        && !icase(b"today")
                        && !icase(b"now")
                        && !icase(b"cell")
                        && !icase(b"info")
                        && sh.tables.is_empty()
                        && sh.hidden_rows.is_empty()
                        && sh.hidden_rows_user.is_empty();
                    let date_1904 = wb.date_system.is_1904();
                    if self_contained
                        && let ReadCache::EvalPreview {
                            sheet_id,
                            ref sheet_ver,
                            styles_rev,
                            date_1904: prev_1904,
                            hash,
                        } = *read.cache.borrow()
                        && sheet_id == sh.identity
                        && sheet_ver == sh.cell_version()
                        && styles_rev == sh.display_style_revision()
                        && prev_1904 == date_1904
                    {
                        return Some(hash);
                    }
                    let (_, result) = view.tool_eval_preview(&read.call.arguments);
                    let h = read_hash(&result);
                    if self_contained {
                        *read.cache.borrow_mut() = ReadCache::EvalPreview {
                            sheet_id: sh.identity,
                            sheet_ver: sh.cell_version().clone(),
                            styles_rev: sh.display_style_revision(),
                            date_1904,
                            hash: h,
                        };
                    }
                    return Some(h);
                }
                let (_, result) = view.tool_eval_preview(&read.call.arguments);
                Some(read_hash(&result))
            }
            _ => None,
        }
    }

    /// The assistant itself changed the workbook (or it was verified): take a new baseline.
    fn refresh_ai_baseline(&mut self) {
        let Some(turn) = self.ai.active_round.as_ref() else {
            return;
        };
        let revision = (self.doc_gen, self.wb_epoch);
        let updated = turn.revision != revision || turn.awaiting_write_results;
        let mut shared_snaps = std::collections::HashMap::new();
        let hashes: Vec<_> = turn
            .footprint
            .iter()
            .map(|f| {
                if updated || f.hash.is_none() {
                    self.ai_region_hash_with_snaps(f, Some(&mut shared_snaps))
                } else {
                    f.hash
                }
            })
            .collect();
        let reads: Vec<_> = turn.reads.iter().map(|read| {
            if updated || read.hash.is_none() {
                self.ai_read_hash_obs(read)
            } else {
                read.hash
            }
        }).collect();
        if let Some(turn) = self.ai.active_round.as_mut() {
            for (footprint, hash) in turn.footprint.iter_mut().zip(hashes) {
                footprint.hash = hash;
            }
            for (read, hash) in turn.reads.iter_mut().zip(reads) {
                read.hash = hash;
            }
            turn.retry_check.set(None);
            turn.revision = revision;
        }
    }

    /// Why the workbook no longer matches what this turn observed or prepared, if it doesn't.
    fn ai_turn_conflict(&self, turn: &ActiveTurn) -> Option<String> {
        if turn.revision.1 != self.wb_epoch {
            return Some("工作簿在 AI 处理期间已重新载入".into());
        }
        if turn.revision.0 == self.doc_gen {
            return None;
        }
        if self.ai_sheet_by_identity(turn.view.sheet).is_none() {
            return Some("提问时的活动工作表在 AI 处理期间被删除".into());
        }
        for read in &turn.reads {
            if self.ai_read_sheet(&read.call) != read.sheet
                || read.hash.is_none()
                || self.ai_read_hash_obs(read) != read.hash
            {
                let what = match read.call.name.as_str() {
                    "search_cells" => "搜索结果",
                    "inspect_workbook" => "工作簿概览",
                    "inspect_comments" => "批注列表",
                    _ => "公式试算结果",
                };
                return Some(format!("{what}在 AI 处理期间发生了变化或有待重算"));
            }
        }
        let changed = turn
            .footprint
            .iter()
            .find(|f| f.hash.is_none() || self.ai_region_hash(f) != f.hash)?;
        let name = self
            .ai_sheet_by_identity(changed.sheet)
            .map(|i| self.wb.sheet(i).name.clone())
            .unwrap_or_default();
        Some(format!(
            "「{name}」{} 在 AI 处理期间发生了变化或有待重算",
            changed.target.label()
        ))
    }

    fn ai_turn_still_valid(&mut self) -> Result<(), String> {
        if let Some(conflict) = self.ai.active_round.as_ref().and_then(|t| self.ai_turn_conflict(t)) {
            return Err(conflict);
        }
        let revision = (self.doc_gen, self.wb_epoch);
        if let Some(turn) = self.ai.active_round.as_mut() {
            turn.revision = revision;
        }
        Ok(())
    }

    fn record_ai_write_batch_results(
        &mut self,
        calls: &[(WireToolCall, Option<u64>)],
        batch_result: JsonValue,
    ) {
        let items = batch_result.as_array().cloned().unwrap_or_default();
        for ((call, _), item) in calls.iter().zip(&items) {
            self.note_ai_footprint(&call.spec, &item["result"]);
        }
        self.refresh_ai_baseline();
        if let Some(turn) = self.ai.active_round.as_mut() {
            turn.awaiting_write_results = false;
        }
        for ((call, _), item) in calls.iter().zip(items) {
            let label = item["label"].as_str().unwrap_or("写入单元格").to_string();
            let res = item.get("result").cloned().unwrap_or(JsonValue::Null);
            self.record_ai_observation(call.id.clone(), label, res);
        }
    }

    fn record_ai_tool_result(&mut self, call: &WireToolCall, label: String, result: JsonValue) {
        let initial_search_cache = (call.spec.name == "search_cells")
            .then(|| {
                self.ai
                    .pending_search_cache
                    .lock()
                    .ok()
                    .and_then(|mut g| g.take())
            })
            .flatten()
            .map(|(prepared, sheets)| ReadCache::Search { prepared, sheets });
        self.note_ai_footprint(&call.spec, &result);
        if call.spec.name == "manage_sheet"
            && result["ok"] == true
            && call.spec.arguments["op"].as_str() == Some("rename")
            && let Some(new_name) = result["sheet"].as_str()
        {
            let renamed_id = self
                .resolve_sheet_index(Some(new_name))
                .map(|i| self.wb.sheet(i).identity);
            if let Some(turn) = self.ai.active_round.as_mut() {
                for read in &mut turn.reads {
                    if let Some(rid) = renamed_id
                        && read.sheet == Some(rid)
                        && read.call.arguments.get("sheet").is_some()
                    {
                        read.call.arguments["sheet"] = json!(new_name);
                    }
                    *read.cache.borrow_mut() = ReadCache::None;
                    read.hash = None;
                }
            }
        }
        let moved_view = call.spec.name == "manage_sheet"
            && result["ok"] == true
            && matches!(call.spec.arguments["op"].as_str(), Some("create" | "switch"));
        if moved_view {
            let view = self.ai_view_now();
            if let Some(turn) = self.ai.active_round.as_mut() {
                turn.view = view;
            }
        }
        self.refresh_ai_baseline();
        let sheet = self.ai_read_sheet(&call.spec);
        if let Some(turn) = self.ai.active_round.as_mut() {
            turn.awaiting_write_results = false;
            if matches!(
                call.spec.name.as_str(),
                "search_cells" | "eval_preview" | "inspect_workbook" | "inspect_comments"
            ) {
                let hash = Some(read_hash(&result));
                if let Some(read) = turn.reads.iter_mut().find(|r| {
                    r.call.name == call.spec.name && r.call.arguments == call.spec.arguments
                }) {
                    read.hash = hash;
                    read.sheet = sheet;
                    if let Some(cache) = initial_search_cache {
                        *read.cache.borrow_mut() = cache;
                    }
                } else {
                    turn.reads.push(ReadObservation {
                        call: call.spec.clone(),
                        sheet,
                        hash,
                        cache: std::cell::RefCell::new(
                            initial_search_cache.unwrap_or(ReadCache::None),
                        ),
                    });
                }
            }
        }
        self.record_ai_observation(call.id.clone(), label, result);
    }

    fn record_ai_observation(&mut self, id: String, label: String, result: JsonValue) {
        if let Some(last) = self.ai.messages.last_mut() {
            last.tool_logs.push(label);
        }
        if let Some(turn) = self.ai.active_round.as_mut() {
            let mut content = result.to_string();
            if content.len() > OBSERVATION_BYTES {
                content = json!({"truncated":true,"preview":bounded(content, OBSERVATION_BYTES)})
                    .to_string();
            }
            turn.messages
                .push(json!({"role":"tool","tool_call_id":id,"content":content}));
        }
    }

    fn next_ai_round(&mut self) {
        self.ai.round += 1;
        self.ai.status_hint = format!("第 {} / {MAX_TOOL_ROUNDS} 轮推理中…", self.ai.round);
        self.launch_ai_round();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn pending_app(ctx: &egui::Context) -> SheetApp {
        let mut app = SheetApp::new_with_context(ctx, None);
        // Follow-up rounds launched by these tests must stay on this machine.
        app.ai.cfg.base_url = "http://127.0.0.1:9/v1".into();
        app.ai.messages.push(AiMessage {
            role: AiRole::Assistant,
            content: String::new(),
            reasoning: String::new(),
            reasoning_open: false,
            tool_logs: Vec::new(),
            error: None,
            undo_ids: Vec::new(),
        });
        app.ai.bound_doc_uid = Some(app.doc_uid);
        app.ai.busy = true;
        app.ai.round = 1;
        app.ai.active_round = Some(ActiveTurn {
            prompt: "读取选区".into(),
            cfg: app.ai.cfg.clone(),
            messages: Vec::new(),
            pending_calls: VecDeque::new(),
            calculation: None,
            view: app.ai_view_now(),
            footprint: Vec::new(),
            reads: Vec::new(),
            awaiting_write_results: false,
            retry_check: Default::default(),
            revision: (app.doc_gen, app.wb_epoch),
            content_start: 0,
            reasoning_start: 0,
        });
        app
    }

    fn call(id: &str, name: &str, arguments: JsonValue) -> WireToolCall {
        WireToolCall {
            id: id.into(),
            spec: ToolCallSpec {
                name: name.into(),
                arguments,
            },
        }
    }

    fn number(n: f64) -> Cell {
        Cell {
            value: Value::Number(n),
            formula: None,
        }
    }

    fn finish_calculations(app: &mut SheetApp, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.ai.active_round.as_ref().is_some_and(|turn| turn.calculation.is_some()) {
            assert!(Instant::now() < deadline, "{}", app.ai.status_hint);
            app.poll_ai(ctx);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn ai_initial_messages_only_include_trusted_coordinates_until_workbook_tool_is_called() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        app.wb.sheet_mut(0).name = "客户名单样本".into();
        app.wb.sheet_mut(0).set(
            0,
            0,
            Cell {
                value: Value::Text("单元格样本".into()),
                formula: None,
            },
        );
        let messages = app.ai_prompt_messages("分析当前工作簿");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert!(
            messages[0]["content"]
                .as_str()
                .unwrap()
                .contains(&format!("1/{}", app.wb.sheets.len()))
        );
        assert_eq!(
            messages[1],
            json!({"role":"user","content":"分析当前工作簿"})
        );
        let system = messages[0]["content"].as_str().unwrap();
        assert!(!system.contains("客户名单样本") && !system.contains("单元格样本"));
        let overview = ToolCallSpec {
            name: "inspect_workbook".into(),
            arguments: json!({}),
        };
        let AiToolOutput::Ready(_, result) = app.execute_ai_tool(&overview, &ctx) else {
            panic!("overview must be immediate")
        };
        assert!(
            result["overview"]
                .as_str()
                .unwrap()
                .contains("客户名单样本")
        );
    }

    #[test]
    fn ai_tool_round_echoes_server_reasoning_only_in_active_conversation() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        app.wb.calc_pending = true;
        app.finish_ai_round(
            String::new(),
            Some("工具推理标记".into()),
            Ok(vec![call("read", "inspect_sheet", json!({}))]),
            &ctx,
        );
        let turn = app.ai.active_round.as_ref().unwrap();
        let assistant = turn.messages.last().unwrap();
        assert_eq!(assistant["reasoning_content"], "工具推理标记");
        assert_eq!(assistant["tool_calls"][0]["id"], "read");
        app.finish_ai_round("已读取".into(), None, Ok(vec![]), &ctx);
        let next = app.ai_prompt_messages("下一条");
        assert!(next.iter().all(
            |m| m.get("reasoning_content").is_none() && !m.to_string().contains("工具推理标记")
        ));
    }

    #[test]
    fn ai_worker_disconnect_ends_busy_state_and_preserves_partial_reply() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        let (tx, rx) = mpsc::channel();
        app.ai.rx = Some(rx);
        tx.send(StreamMsg::ContentDelta("部分回答".into())).unwrap();
        drop(tx);
        app.poll_ai(&ctx);
        assert!(!app.ai.busy);
        assert_eq!(app.ai.messages.last().unwrap().content, "部分回答");
        assert!(app.can_retry_ai_round());
        assert!(app.ai.wire_history.is_empty());
    }

    #[test]
    fn ai_tools_ignore_selection_and_unrelated_edits_but_not_changes_to_observed_cells() {
        for observed_cell_changed in [false, true] {
            let ctx = egui::Context::default();
            let mut app = pending_app(&ctx);
            app.wb.sheet_mut(0).set(0, 0, number(1.0));
            app.finish_ai_round(
                String::new(),
                None,
                Ok(vec![call("read", "inspect_sheet", json!({"range":"A1"}))]),
                &ctx,
            );
            assert_eq!(app.ai.messages.last().unwrap().tool_logs.len(), 1);
            // While the model works, the user clicks elsewhere and a collaborator edits another cell.
            app.sel = (5, 5);
            app.sel_end = app.sel;
            app.wb.sheet_mut(0).set(9, 9, number(7.0));
            app.doc_gen += 1;
            if observed_cell_changed {
                app.wb.sheet_mut(0).set(0, 0, number(2.0));
                app.doc_gen += 1;
            }
            app.finish_ai_round(
                String::new(),
                None,
                Ok(vec![call("write", "set_cells", json!({"edits":[{"cell":"B1","value":3}]}))]),
                &ctx,
            );
            let last = app.ai.messages.last().unwrap();
            if observed_cell_changed {
                assert!(!app.ai.busy);
                assert_eq!(app.wb.sheet(0).value(0, 1), Value::Empty);
                assert_eq!(last.tool_logs.len(), 1);
                let error = last.error.as_deref().unwrap();
                assert!(error.contains("A1") && error.contains("变化"), "{error}");
            } else {
                assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(3.0));
                assert_eq!(last.tool_logs.len(), 2);
            }
            app.stop_ai_generation();
        }
    }

    #[test]
    fn ai_confirmation_rejects_reuse_of_the_displayed_sheet_name() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        let original = app.wb.sheet(0).name.clone();
        app.add_sheet();
        let other = app.sheet_idx;
        app.wb.sheet_mut(0).set(0, 0, number(10.0));
        app.wb.sheet_mut(other).set(0, 0, number(80.0));
        app.finish_ai_round(String::new(), None, Ok(vec![call("clear", "data_operation",
            json!({"op":"clear_contents", "sheet":original, "range":"A1"}),
        )]), &ctx);
        assert!(app.ai.confirmation_label.is_some());
        assert!(app.set_sheet_name(0, "Archive"));
        assert!(app.set_sheet_name(other, &original));
        app.answer_ai_confirmation(true, &ctx);
        assert!(app.ai.messages.last().unwrap().error.is_some());
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(10.0));
        assert_eq!(app.wb.sheet(other).value(0, 0), Value::Number(80.0));
    }

    #[test]
    fn ai_inspection_rechecks_calculated_and_formatted_results() {
        for change_format in [false, true] {
            let ctx = egui::Context::default();
            let mut app = pending_app(&ctx);
            app.wb.sheet_mut(0).set(0, 0, number(10.0));
            app.wb.sheet_mut(0).set(0, 1, Cell {
                formula: Some("A1*2".into()), value: Value::Number(20.0),
            });
            app.finish_ai_round(String::new(), None, Ok(vec![call("read", "inspect_sheet",
                json!({"range":"B1"}),
            )]), &ctx);
            if change_format {
                let mut style = app.wb.styles.get(0).clone();
                style.format = "0.00%".into();
                let index = app.wb.styles.intern(style);
                app.wb.sheet_mut(0).set_style(0, 1, index);
            } else {
                app.wb.sheet_mut(0).set(0, 0, number(30.0));
                sheetcore::formula::recalc_all(&mut app.wb);
                assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(60.0));
            }
            app.doc_gen += 1;
            app.finish_ai_round(String::new(), None, Ok(vec![call("write", "set_cells",
                json!({"edits":[{"cell":"C1","value":20}]}),
            )]), &ctx);
            assert!(app.ai.status_hint.contains("B1"), "{}", app.ai.status_hint);
            assert_eq!(app.wb.sheet(0).value(0, 2), Value::Empty);
        }
    }

    #[test]
    fn ai_search_detects_new_matches_and_accepts_unrelated_edits() {
        for (initial_hit, matching_edit) in [(false, false), (false, true), (true, true)] {
            let ctx = egui::Context::default();
            let mut app = pending_app(&ctx);
            let invoice = Cell { value: Value::Text("Invoice 17".into()), formula: None };
            if initial_hit {
                app.wb.sheet_mut(0).set(0, 0, invoice.clone());
            }
            app.finish_ai_round(String::new(), None, Ok(vec![call("search", "search_cells",
                json!({"query":"invoice 17"}),
            )]), &ctx);
            app.wb.sheet_mut(0).set(300, 0, if matching_edit { invoice } else { number(3.0) });
            app.doc_gen += 1;
            app.finish_ai_round(String::new(), None, Ok(vec![call("write", "set_cells",
                json!({"edits":[{"cell":"C1","value":1}]}),
            )]), &ctx);
            assert_eq!(app.wb.sheet(0).value(0, 2),
                if matching_edit { Value::Empty } else { Value::Number(1.0) });
            assert_eq!(app.ai.messages.last().unwrap().error.is_some(), matching_edit);
            app.stop_ai_generation();
        }
    }

    #[test]
    fn ai_preview_rechecks_its_result_before_editing() {
        for (column, conflict) in [(0, true), (1, false)] {
            let ctx = egui::Context::default();
            let mut app = pending_app(&ctx);
            app.wb.sheet_mut(0).set(0, 0, number(4.0));
            app.finish_ai_round(String::new(), None, Ok(vec![call("preview", "eval_preview",
                json!({"formula":"=A1*2"}),
            )]), &ctx);
            app.wb.sheet_mut(0).set(0, column, number(10.0));
            app.doc_gen += 1;
            app.finish_ai_round(String::new(), None, Ok(vec![call("write", "set_cells",
                json!({"edits":[{"cell":"C1","value":8}]}),
            )]), &ctx);
            assert_eq!(app.wb.sheet(0).value(0, 2),
                if conflict { Value::Empty } else { Value::Number(8.0) });
            assert_eq!(app.ai.messages.last().unwrap().error.is_some(), conflict);
            app.stop_ai_generation();
        }
    }

    #[test]
    fn ai_revalidates_manual_values_before_finishing_confirming_or_retrying() {
        for action in ["finish", "confirm", "retry"] {
            for related_edit in [false, true] {
                let ctx = egui::Context::default();
                let mut app = pending_app(&ctx);
                app.wb.sheet_mut(0).set(0, 0, number(4.0));
                app.wb.sheet_mut(0).set(0, 1, Cell {
                    formula: Some("A1*2".into()), value: Value::Empty,
                });
                app.wb.sheet_mut(0).set(0, 2, number(1.0));
                app.wb.calc_manual = true;
                app.wb.calc_pending = true;
                app.finish_ai_round(String::new(), None, Ok(vec![
                    call("read", "inspect_sheet", json!({"range":"B1"})),
                    call("search", "search_cells", json!({"query":"8"})),
                ]), &ctx);
                finish_calculations(&mut app, &ctx);
                assert_eq!(app.ai.messages.last().unwrap().tool_logs.len(), 2);
                if action == "confirm" {
                    app.finish_ai_round(String::new(), None, Ok(vec![call("write", "set_cells",
                        json!({"edits":[{"cell":"C1","value":8}]}),
                    )]), &ctx);
                    assert!(app.ai.confirmation_label.is_some());
                } else if action == "retry" {
                    app.fail_ai_round("本地测试中断".into(), true);
                }
                app.wb.sheet_mut(0).set(if related_edit { 0 } else { 5 }, 0, number(20.0));
                app.doc_gen += 1;
                match action {
                    "confirm" => app.answer_ai_confirmation(true, &ctx),
                    "retry" => {
                        assert!(app.can_retry_ai_round());
                        app.retry_failed_ai_round();
                    }
                    _ => app.finish_ai_round("结果为 8".into(), None, Ok(vec![]), &ctx),
                }
                assert!(matches!(app.ai.active_round.as_ref().unwrap().calculation,
                    Some(PendingCalculation::Validate { .. })));
                finish_calculations(&mut app, &ctx);
                let last = app.ai.messages.last().unwrap();
                assert_eq!(last.error.is_some(), related_edit, "{action}: {}", app.ai.status_hint);
                assert_eq!(app.wb.sheet(0).value(0, 2),
                    Value::Number(if action == "confirm" && !related_edit { 8.0 } else { 1.0 }));
                assert_eq!(last.undo_ids.len(), usize::from(action == "confirm" && !related_edit));
                if action == "finish" && !related_edit {
                    assert_eq!(app.ai.status_hint, "已完成");
                }
                app.stop_ai_generation();
            }
        }
    }

    #[test]
    fn ai_scoped_reads_keep_the_sheet_identity_even_when_the_result_is_unchanged() {
        for tool in ["search_cells", "eval_preview"] {
            let ctx = egui::Context::default();
            let mut app = pending_app(&ctx);
            app.add_sheet();
            let target = app.sheet_idx;
            let name = app.wb.sheet(target).name.clone();
            app.add_sheet();
            let replacement = app.sheet_idx;
            let args = json!({"sheet":name, "query":"unmatched invoice", "formula":"=1"});
            app.finish_ai_round(String::new(), None, Ok(vec![call("read", tool, args)]), &ctx);
            assert!(app.set_sheet_name(target, "Archive"));
            assert!(app.set_sheet_name(replacement, &name));
            app.finish_ai_round(String::new(), None, Ok(vec![call("write", "set_cells",
                json!({"sheet":name, "edits":[{"cell":"A1","value":1}]}),
            )]), &ctx);
            assert!(app.ai.messages.last().unwrap().error.is_some());
            assert_eq!(app.wb.sheet(replacement).value(0, 0), Value::Empty);
        }
    }

    #[test]
    fn ai_first_pending_read_rejects_name_reuse_and_accepts_sheet_moves() {
        for (tool, reuse_name) in [("inspect_sheet", true), ("eval_preview", true), ("inspect_sheet", false)] {
            let ctx = egui::Context::default();
            let mut app = pending_app(&ctx);
            app.add_sheet();
            let other = app.sheet_idx;
            let original = app.wb.sheet(0).name.clone();
            app.wb.sheet_mut(0).set(0, 0, number(4.0));
            app.wb.sheet_mut(other).set(0, 0, number(90.0));
            app.wb.calc_manual = true;
            app.wb.calc_pending = true;
            app.finish_ai_round(String::new(), None, Ok(vec![call("read", tool,
                json!({"sheet":original, "range":"A1", "formula":"=A1"}),
            )]), &ctx);
            assert!(app.ai.active_round.as_ref().unwrap().calculation.is_some());
            if reuse_name {
                assert!(app.set_sheet_name(0, "Archive"));
                assert!(app.set_sheet_name(other, &original));
            } else {
                app.move_sheet(0, 1);
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            while app.ai.active_round.as_ref().is_some_and(|t| t.calculation.is_some()) {
                assert!(Instant::now() < deadline);
                app.poll_ai(&ctx);
                std::thread::sleep(Duration::from_millis(2));
            }
            if reuse_name {
                assert!(app.ai.status_hint.contains("工作表已变化"), "{}", app.ai.status_hint);
                assert!(app.ai.messages.last().unwrap().tool_logs.is_empty());
            } else {
                let turn = app.ai.active_round.as_ref().unwrap();
                let result = turn.messages.iter().find(|m| m["role"] == "tool").unwrap();
                let observation: JsonValue = serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
                assert_eq!(observation["cells"][0]["value"], "4");
                app.stop_ai_generation();
            }
        }
    }

    #[test]
    fn ai_overview_keeps_its_view_and_rechecks_other_sheets_used_ranges() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        app.add_sheet();
        let other = app.sheet_idx;
        let observed = app.wb.sheet(0).name.clone();
        app.sheet_idx = 0;
        app.finish_ai_round(String::new(), None, Ok(vec![call("read", "inspect_workbook",
            json!({"sheet":observed}),
        )]), &ctx);
        app.sheet_idx = other;
        app.sel = (40, 50);
        app.sel_end = app.sel;
        app.doc_gen += 1;
        assert!(app.ai_turn_still_valid().is_ok());
        app.wb.sheet_mut(other).set(200, 20, number(4.0));
        app.doc_gen += 1;
        app.finish_ai_round(String::new(), None, Ok(vec![call("write", "set_cells",
            json!({"sheet":observed, "edits":[{"cell":"C1","value":1}]}),
        )]), &ctx);
        assert!(app.ai.status_hint.contains("概览"), "{}", app.ai.status_hint);
        assert_eq!(app.wb.sheet(0).value(0, 2), Value::Empty);
    }

    #[test]
    fn ai_search_rechecks_whether_the_first_thirty_matches_were_exhaustive() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        for row in 0..30 {
            app.wb.sheet_mut(0).set(row, 0, number(17.0));
        }
        app.finish_ai_round(String::new(), None, Ok(vec![call("search", "search_cells",
            json!({"query":"17"}),
        )]), &ctx);
        app.wb.sheet_mut(0).set(30, 0, number(17.0));
        app.doc_gen += 1;
        app.finish_ai_round(String::new(), None, Ok(vec![call("write", "set_cells",
            json!({"edits":[{"cell":"C1","value":30}]}),
        )]), &ctx);
        assert!(app.ai.status_hint.contains("搜索"), "{}", app.ai.status_hint);
        assert_eq!(app.wb.sheet(0).value(0, 2), Value::Empty);
    }

    #[test]
    fn ai_omitted_targets_follow_the_view_given_to_the_model() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        app.sel = (2, 1);
        app.sel_end = (3, 2);
        let view = app.ai_view_now();
        let original = app.wb.sheet(0).name.clone();
        assert!(sheetcore::sheet_ops::insert_sheet(
            &mut app.wb,
            1,
            sheetcore::model::Sheet::new("Other")
        ));
        app.sheet_idx = 1;
        app.sel = (9, 9);
        app.sel_end = (9, 9);

        let mut clear = ToolCallSpec {
            name: "data_operation".into(),
            arguments: json!({"op":"clear_contents"}),
        };
        app.pin_ai_call(view, &mut clear).unwrap();
        assert_eq!(clear.arguments["sheet"], original.as_str());
        let range = clear.arguments["range"].as_str().unwrap();
        assert_eq!(checked_ai_range(range), Some((2, 1, 3, 2)));

        let mut overview = ToolCallSpec {
            name: "inspect_workbook".into(),
            arguments: json!({}),
        };
        app.pin_ai_call(view, &mut overview).unwrap();
        let AiToolOutput::Ready(_, result) = app.execute_ai_tool(&overview, &ctx) else {
            panic!("overview must be immediate")
        };
        let text = result["overview"].as_str().unwrap();
        assert!(text.contains(&format!("当前活动工作表：{original}，当前选区：")), "{text}");

        let mut invalid = ToolCallSpec {
            name: "set_cells".into(),
            arguments: json!({"sheet": 3, "edits": []}),
        };
        assert!(app.pin_ai_call(view, &mut invalid).is_err());
    }

    #[test]
    fn ai_last_round_reports_unexecuted_tools() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        app.ai.round = MAX_TOOL_ROUNDS;
        app.finish_ai_round(
            "还需读取一个单元格".into(),
            None,
            Ok(vec![call("read", "inspect_sheet", json!({}))]),
            &ctx,
        );
        assert!(!app.ai.busy);
        assert!(app.ai.status_hint.contains("上限"));
        assert!(app.ai.messages.last().unwrap().tool_logs.is_empty());
        assert!(app.ai.wire_history.is_empty());
    }

    #[test]
    fn ai_calculation_waits_for_background_recalc_and_rechecks_observed_cells() {
        for observed_cell_changed in [false, true] {
            let ctx = egui::Context::default();
            let mut app = pending_app(&ctx);
            app.wb.sheet_mut(0).set(0, 0, number(1.0));
            app.wb.sheet_mut(0).set(
                0,
                1,
                Cell {
                    value: Value::Empty,
                    formula: Some("A1*2".into()),
                },
            );
            let sheet = app.wb.sheet(0).name.clone();
            let read = call("read", "inspect_sheet", json!({"sheet": sheet, "range": "A1:B1"}));
            // A1 was previously observed. B1's formula has not been read until its
            // pending calculation finishes below.
            app.note_ai_footprint(&read.spec, &json!({"sheet": sheet, "range": "A1"}));
            app.refresh_ai_baseline();
            app.wb.calc_pending = true;
            let AiToolOutput::Calculating(job) = app.execute_ai_tool(&read.spec, &ctx) else {
                panic!("pending values need calculation")
            };
            let sheet = app.ai_read_sheet(&read.spec);
            app.ai.active_round.as_mut().unwrap().calculation = Some(PendingCalculation::Tool { call: read, job, sheet });
            app.sel = (8, 8);
            app.sel_end = app.sel;
            if observed_cell_changed {
                app.wb.sheet_mut(0).set(0, 0, number(5.0));
                app.doc_gen += 1;
            }
            let start = Instant::now();
            while app
                .ai
                .active_round
                .as_ref()
                .is_some_and(|t| t.calculation.is_some())
            {
                assert!(start.elapsed() < Duration::from_secs(10), "{}", app.ai.status_hint);
                app.poll_ai(&ctx);
                std::thread::sleep(Duration::from_millis(5));
            }
            let last = app.ai.messages.last().unwrap();
            if observed_cell_changed {
                assert!(!app.ai.busy);
                assert!(last.tool_logs.is_empty());
                assert!(app.ai.status_hint.contains("变化"), "{}", app.ai.status_hint);
            } else {
                assert_eq!(last.tool_logs.len(), 1);
                // The regular background job calculated the live workbook; nothing is left pending.
                assert!(!app.wb.calc_pending && !app.tasks.recalc_pending());
                assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(2.0));
                let turn = app.ai.active_round.as_ref().unwrap();
                let observation = turn.messages.last().unwrap()["content"].as_str().unwrap();
                assert!(observation.contains(r#""value":"2""#), "{observation}");
            }
            app.stop_ai_generation();
        }
    }

    #[test]
    fn ai_history_retains_bounded_complete_turns() {
        let mut state = AiState::default();
        for i in 0..25 {
            state.remember(format!("q{i}"), format!("a{i}"));
        }
        assert_eq!(state.wire_history.len(), HISTORY_TURNS);
        assert_eq!(state.wire_history.back().unwrap().1, "a24");
        for _ in 0..5 {
            state.remember("问".repeat(HISTORY_BYTES), "答".repeat(HISTORY_BYTES));
        }
        let bytes: usize = state
            .wire_history
            .iter()
            .map(|(p, a)| p.len() + a.len())
            .sum();
        assert!(bytes <= HISTORY_BYTES);
        assert!(state.wire_history.back().unwrap().0.ends_with("已截短）"));
    }

    #[test]
    fn ai_scattered_set_cells_footprint_compacts_without_false_bounding_box_conflict() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        app.finish_ai_round(
            String::new(),
            None,
            Ok(vec![call(
                "w1",
                "set_cells",
                json!({"edits": [
                    {"cell": "A1", "value": 10},
                    {"cell": "E10", "value": 20},
                    {"cell": "C5", "value": 30}
                ]}),
            )]),
            &ctx,
        );
        let turn = app.ai.active_round.as_ref().unwrap();
        assert_eq!(turn.footprint.len(), 1, "scattered set_cells must compact into one Footprint");
        assert!(matches!(turn.footprint[0].target, FootprintTarget::Cells(ref v) if v.len() == 3));

        // Editing an unrelated cell INSIDE the A1:E10 bounding box must NOT trigger conflict.
        app.wb.sheet_mut(0).set(1, 1, number(999.0));
        app.doc_gen += 1;
        assert!(
            app.ai_turn_still_valid().is_ok(),
            "editing unrelated B2 inside A1:E10 bounding box must not trigger conflict"
        );

        // Editing one of the actual set_cells coordinates MUST trigger conflict.
        app.wb.sheet_mut(0).set(4, 2, number(31.0));
        app.doc_gen += 1;
        assert!(
            app.ai_turn_still_valid().is_err(),
            "editing targeted C5 must trigger conflict"
        );
        app.stop_ai_generation();
    }
}

#[cfg(test)]
impl SheetApp {
    pub(crate) fn bench_ai_set_cells_footprint(&mut self, edits: usize) -> std::time::Duration {
        let sheet_name = self.wb.sheet(0).name.clone();
        let edit_vals: Vec<JsonValue> = (0..edits)
            .map(|i| json!({ "cell": format!("A{}", i + 1), "value": i }))
            .collect();
        let call = ToolCallSpec {
            name: "set_cells".into(),
            arguments: json!({ "sheet": sheet_name, "edits": edit_vals }),
        };
        self.ai.active_round = Some(ActiveTurn {
            prompt: "bench".into(),
            cfg: self.ai.cfg.clone(),
            messages: Vec::new(),
            pending_calls: VecDeque::new(),
            calculation: None,
            view: self.ai_view_now(),
            footprint: Vec::new(),
            reads: Vec::new(),
            awaiting_write_results: false,
            retry_check: Default::default(),
            revision: (self.doc_gen, self.wb_epoch),
            content_start: 0,
            reasoning_start: 0,
        });
        let t = std::time::Instant::now();
        self.note_ai_footprint(&call, &json!({ "ok": true }));
        self.refresh_ai_baseline();
        let dt = t.elapsed();
        self.ai.active_round = None;
        dt
    }
}
