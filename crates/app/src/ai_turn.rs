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

/// Cells this turn read, wrote or is waiting to change, with a fingerprint of their
/// constants and formula text. Recalculated results are deliberately not part of it.
struct Footprint {
    sheet: u64,
    rect: Rect,
    hash: Option<u64>,
}

pub(super) struct ActiveTurn {
    prompt: String,
    cfg: AiConfig,
    messages: Vec<JsonValue>,
    pending_calls: VecDeque<WireToolCall>,
    calculation: Option<(WireToolCall, AiToolCalculation)>,
    view: AiView,
    footprint: Vec<Footprint>,
    // Workbook revision at which `footprint` was last verified.
    revision: (u64, u64),
    // On retry, replace just the streamed text from the failed completion.
    content_start: usize,
    reasoning_start: usize,
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

/// Order-independent fingerprint of the constants and formula text inside `rect`.
fn region_hash(sheet: &sheetcore::Sheet, rect: Rect) -> u64 {
    let cell_hash = |r: u32, c: u32, cell: &Cell| {
        let mut h = DefaultHasher::new();
        (r, c).hash(&mut h);
        match (&cell.formula, &cell.value) {
            (Some(f), _) => (1u8, f).hash(&mut h),
            (None, Value::Number(n)) => (2u8, n.to_bits()).hash(&mut h),
            (None, Value::Text(t)) => (3u8, t).hash(&mut h),
            (None, Value::Bool(b)) => (4u8, b).hash(&mut h),
            (None, Value::Error(e)) => (5u8, e.text()).hash(&mut h),
            (None, Value::Empty) => 6u8.hash(&mut h),
        }
        h.finish()
    };
    let (mut r1, mut c1, mut r2, mut c2) = rect;
    let mut area = u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1);
    if area > 4096 {
        // Large targets such as whole columns: only the used part can hold content.
        let Some((ur1, uc1, ur2, uc2)) = sheet.used_bounds() else { return 0 };
        (r1, c1, r2, c2) = (r1.max(ur1), c1.max(uc1), r2.min(ur2), c2.min(uc2));
        if r1 > r2 || c1 > c2 {
            return 0;
        }
        area = u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1);
    }
    let mut sum = 0u64;
    if area <= sheet.len() as u64 {
        for r in r1..=r2 {
            for c in c1..=c2 {
                if let Some(cell) = sheet.get(r, c).filter(|cell| !cell.is_blank()) {
                    sum = sum.wrapping_add(cell_hash(r, c, cell));
                }
            }
        }
    } else {
        for (at, cell) in sheet.iter() {
            if (r1..=r2).contains(&at.row) && (c1..=c2).contains(&at.col) && !cell.is_blank() {
                sum = sum.wrapping_add(cell_hash(at.row, at.col, cell));
            }
        }
    }
    sum
}

impl AiState {
    pub(super) fn update_retry_tool_protocol(&mut self, cfg: &AiConfig) {
        if self.busy {
            return;
        }
        if let Some(turn) = &mut self.active_round {
            let mut updated = turn.cfg.clone();
            updated.native_tools = cfg.native_tools;
            if updated == *cfg {
                turn.cfg.native_tools = cfg.native_tools;
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
    /// A background recalculation only changes formula results, which the footprint ignores.
    pub(crate) fn ai_recalculated(&mut self, previous_gen: u64) {
        if self.ai.bound_doc_uid == Some(self.doc_uid)
            && let Some(turn) = &mut self.ai.active_round
            && turn.revision == (previous_gen, self.wb_epoch)
        {
            turn.revision.0 = self.doc_gen;
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
        if !self.ai.cfg.data_consent {
            self.ai.pending_consent_prompt = Some(prompt);
            self.ai.status_hint = "发送前请先确认数据外发说明".into();
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
            revision: (self.doc_gen, self.wb_epoch),
            content_start: 0,
            reasoning_start: 0,
        });
        let discard = (self.ai.messages.len() + 2).saturating_sub(2 * DISPLAY_TURNS);
        self.ai.messages.drain(..discard);
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
                turn.cfg == self.ai.cfg && self.ai_turn_conflict(turn).is_none()
            })
    }

    pub(super) fn retry_failed_ai_round(&mut self) {
        if !self.can_retry_ai_round() {
            return;
        }
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
        if let Some((call, mut job)) = calculation {
            match self.poll_ai_calculation(&mut job, ctx) {
                CalcPoll::Pending(progress) => {
                    self.ai.status_hint = match progress {
                        Some((done, total)) => format!(
                            "正在核对表格计算结果… {}%",
                            done.min(total) * 100 / total.max(1)
                        ),
                        None => "正在核对表格计算结果…".into(),
                    };
                    if let Some(turn) = self.ai.active_round.as_mut() {
                        turn.calculation = Some((call, job));
                    }
                }
                CalcPoll::Failed(error) => self.fail_ai_round(format!("{error}，本轮已终止。"), false),
                CalcPoll::Ready(label, result) => match self.ai_turn_still_valid() {
                    Err(conflict) => self.fail_ai_round(format!("{conflict}，本轮已终止。"), false),
                    Ok(()) => {
                        self.record_ai_tool_result(&call, label, result);
                        self.continue_ai_tools(ctx, false);
                    }
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
        if let Err(conflict) = self.ai_turn_still_valid() {
            self.fail_ai_round(format!("{conflict}，本轮工具未执行，请重新提问。"), false);
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
        if !approved {
            self.fail_ai_round("已取消待确认的操作，本轮剩余工具未执行。".into(), false);
            return;
        }
        if self.ai.bound_doc_uid != Some(self.doc_uid) || self.ai.active_round.is_none() {
            self.fail_ai_round("工作簿在等待确认时已切换，操作已终止。".into(), false);
            return;
        }
        // The card showed resolved targets; they are part of the footprint.
        if let Err(conflict) = self.ai_turn_still_valid() {
            self.fail_ai_round(format!("{conflict}，待确认的操作已终止。"), false);
            return;
        }
        self.continue_ai_tools(ctx, true);
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
                        self.note_ai_footprint(&call.spec, &JsonValue::Null);
                        self.refresh_ai_baseline();
                        if let Some(turn) = self.ai.active_round.as_mut() {
                            turn.pending_calls.push_front(call);
                        }
                        self.ai.confirmation_label = Some(label);
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
            match self.execute_ai_tool(&call.spec, ctx) {
                AiToolOutput::Ready(label, result) => {
                    self.record_ai_tool_result(&call, label, result)
                }
                AiToolOutput::Calculating(calculation) => {
                    self.refresh_ai_baseline();
                    if let Some(turn) = self.ai.active_round.as_mut() {
                        turn.calculation = Some((call, calculation));
                    }
                    self.ai.status_hint = "正在核对表格计算结果…".into();
                    return;
                }
            }
        }
    }

    /// Fill omitted sheet and range arguments from the view the model was given.
    pub(super) fn pin_ai_call(&self, view: AiView, call: &mut ToolCallSpec) -> Result<(), String> {
        let name = call.name.as_str();
        let takes_sheet = matches!(
            name,
            "inspect_workbook"
                | "inspect_sheet"
                | "eval_preview"
                | "set_cells"
                | "fill_formula_range"
                | "style_range"
                | "data_operation"
        );
        let Some(args) = call.arguments.as_object_mut().filter(|_| takes_sheet) else {
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
        if name == "data_operation" && omitted(args.get("range")) {
            args.insert("range".into(), json!(label));
        }
        Ok(())
    }

    /// Regions a finished (or staged) call depends on.
    fn ai_tool_footprint(&self, call: &ToolCallSpec, result: &JsonValue) -> Vec<(u64, Rect)> {
        let args = &call.arguments;
        let sheet = |name: &JsonValue| name.as_str().and_then(|n| self.resolve_sheet_index(Some(n)));
        let identity = |index: usize| self.wb.sheet(index).identity;
        let mut regions = Vec::new();
        match call.name.as_str() {
            "inspect_sheet" => {
                let rect = result["range"].as_str().and_then(checked_ai_range);
                if let (Some(si), Some(rect)) = (sheet(&result["sheet"]), rect) {
                    regions.push((identity(si), rect));
                }
            }
            "search_cells" => {
                for hit in result["hits"].as_array().into_iter().flatten() {
                    let at = hit["cell"].as_str().and_then(checked_address);
                    if let (Some(si), Some((r, c))) = (sheet(&hit["sheet"]), at) {
                        regions.push((identity(si), (r, c, r, c)));
                    }
                }
            }
            "inspect_workbook" => {
                if let Some(si) = sheet(&args["sheet"]) {
                    let small = |&(r1, c1, r2, c2): &Rect| u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1) <= 256;
                    if let Some(rect) = args["selection"].as_str().and_then(checked_ai_range).filter(small) {
                        regions.push((identity(si), rect));
                    }
                    if let Some((r1, c1, r2, c2)) = self.wb.sheet(si).used_bounds() {
                        regions.push((identity(si), (r1, c1, r2.min(r1 + 5), c2.min(c1 + 11))));
                    }
                }
            }
            "set_cells" => {
                if let Some(si) = sheet(&args["sheet"]) {
                    for edit in args["edits"].as_array().into_iter().flatten() {
                        if let Some((r, c)) = edit["cell"].as_str().and_then(checked_address) {
                            regions.push((identity(si), (r, c, r, c)));
                        }
                    }
                }
            }
            "fill_formula_range" | "data_operation" => {
                let rect = args["range"].as_str().and_then(checked_ai_range);
                if let (Some(si), Some(rect)) = (sheet(&args["sheet"]), rect) {
                    regions.push((identity(si), rect));
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
        let mut known: HashSet<_> = turn.footprint.iter().map(|f| (f.sheet, f.rect)).collect();
        for (sheet, rect) in regions {
            if known.insert((sheet, rect)) {
                turn.footprint.push(Footprint { sheet, rect, hash: None });
            }
        }
    }

    fn ai_region_hash(&self, sheet: u64, rect: Rect) -> Option<u64> {
        let index = self.ai_sheet_by_identity(sheet)?;
        Some(region_hash(self.wb.sheet(index), rect))
    }

    /// The assistant itself changed the workbook (or it was verified): take a new baseline.
    fn refresh_ai_baseline(&mut self) {
        let Some(turn) = self.ai.active_round.as_ref() else {
            return;
        };
        let hashes: Vec<_> = turn
            .footprint
            .iter()
            .map(|f| self.ai_region_hash(f.sheet, f.rect))
            .collect();
        let revision = (self.doc_gen, self.wb_epoch);
        if let Some(turn) = self.ai.active_round.as_mut() {
            for (footprint, hash) in turn.footprint.iter_mut().zip(hashes) {
                footprint.hash = hash;
            }
            turn.revision = revision;
        }
    }

    /// Why the workbook no longer matches what this turn observed or prepared, if it doesn't.
    /// Edits elsewhere, collaborators' changes to other cells and recalculation are fine.
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
        let changed = turn
            .footprint
            .iter()
            .find(|f| self.ai_region_hash(f.sheet, f.rect) != f.hash)?;
        let name = self
            .ai_sheet_by_identity(changed.sheet)
            .map(|i| self.wb.sheet(i).name.clone())
            .unwrap_or_default();
        let (r1, c1, r2, c2) = changed.rect;
        Some(format!(
            "「{name}」{} 在 AI 处理期间发生了变化",
            range_label((r1, c1), (r2, c2))
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

    fn record_ai_tool_result(&mut self, call: &WireToolCall, label: String, result: JsonValue) {
        self.note_ai_footprint(&call.spec, &result);
        let moved_view = call.spec.name == "manage_sheet"
            && result["ok"] == true
            && matches!(call.spec.arguments["op"].as_str(), Some("create" | "switch"));
        if moved_view {
            let view = self.ai_view_now();
            if let Some(turn) = self.ai.active_round.as_mut() {
                turn.view = view;
            }
        }
        self.record_ai_observation(call.id.clone(), label, result);
        self.refresh_ai_baseline();
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
        self.ai.status_hint = format!("第 {} 轮推理中…", self.ai.round);
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
            app.note_ai_footprint(&read.spec, &json!({"sheet": sheet, "range": "A1:B1"}));
            app.refresh_ai_baseline();
            app.wb.calc_pending = true;
            let AiToolOutput::Calculating(job) = app.execute_ai_tool(&read.spec, &ctx) else {
                panic!("pending values need calculation")
            };
            app.ai.active_round.as_mut().unwrap().calculation = Some((read, job));
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
}
