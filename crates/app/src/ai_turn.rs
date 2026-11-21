use super::*;
use std::sync::mpsc::TryRecvError;

const HISTORY_TURNS: usize = 12;
const HISTORY_BYTES: usize = 48 * 1024;
const OBSERVATION_BYTES: usize = 16 * 1024;
const DISPLAY_TURNS: usize = 50;

pub(super) struct ActiveTurn {
    prompt: String,
    cfg: AiConfig,
    messages: Vec<JsonValue>,
    pending_calls: VecDeque<WireToolCall>,
    calculation: Option<(String, AiToolCalculation)>,
    view: (usize, (u32, u32, u32, u32)),
    view_changed_during_approval: bool,
    // Workbook revision when this request, calculation or approval was started.
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
    pub(crate) fn ai_recalculated(&mut self, previous_gen: u64) {
        if self.ai.bound_doc_uid == Some(self.doc_uid) && (self.ai.rx.is_some() || !self.ai.busy) {
            if let Some(turn) = &mut self.ai.active_round {
                if turn.revision == (previous_gen, self.wb_epoch) {
                    turn.revision.0 = self.doc_gen;
                }
            }
        }
    }

    pub(crate) fn ai_calculation_in_progress(&self) -> bool {
        self.ai.bound_doc_uid == Some(self.doc_uid)
            && self
                .ai
                .active_round
                .as_ref()
                .is_some_and(|t| t.calculation.is_some())
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
        let messages = self.ai_prompt_messages(&prompt);
        self.ai.active_round = Some(ActiveTurn {
            prompt: prompt.clone(),
            cfg: self.ai.cfg.clone(),
            messages,
            pending_calls: VecDeque::new(),
            calculation: None,
            view: (self.sheet_idx, self.sel_rect()),
            view_changed_during_approval: false,
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
        let view = (self.sheet_idx, self.sel_rect());
        let Some(turn) = self.ai.active_round.as_mut() else {
            return;
        };
        let Some(last) = self.ai.messages.last_mut() else {
            return;
        };
        turn.content_start = last.content.len();
        turn.reasoning_start = last.reasoning.len();
        turn.revision = (self.doc_gen, self.wb_epoch);
        turn.view = view;
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
                turn.cfg == self.ai.cfg
                    && turn.revision == (self.doc_gen, self.wb_epoch)
                    && turn.view == (self.sheet_idx, self.sel_rect())
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
        let view = (self.sheet_idx, self.sel_rect());
        if let Some(turn) = self.ai.active_round.as_mut() {
            if let Some((_, calculation)) = &turn.calculation {
                match calculation.rx.try_recv() {
                    Ok((label, result)) => {
                        if turn.revision != (self.doc_gen, self.wb_epoch) || turn.view != view {
                            self.fail_ai_round(
                                "工作簿在计算期间已变化，本轮已终止。".into(),
                                false,
                            );
                            return;
                        }
                        let (id, _) = turn.calculation.take().unwrap();
                        self.record_ai_observation(id, label, result);
                        self.continue_ai_tools(ctx, false);
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.fail_ai_round("公式复核任务意外退出，本轮已终止。".into(), false)
                    }
                    Err(TryRecvError::Empty) => {}
                }
                return;
            }
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
        let view = (self.sheet_idx, self.sel_rect());
        let Some(turn) = self.ai.active_round.as_mut() else {
            return;
        };
        if turn.view_changed_during_approval
            || turn.revision != (self.doc_gen, self.wb_epoch)
            || turn.view != view
        {
            self.fail_ai_round(
                "工作簿或活动选区已变化，本轮工具未执行。请在当前选区重新提问。".into(),
                false,
            );
            return;
        }
        match calls {
            Err(error) => {
                // A malformed batch performs no edits. Tell the model how to repair its next call.
                turn.messages
                    .push(json!({"role":"assistant","content":visible}));
                turn.messages.push(json!({"role":"system","content":format!("上一轮工具调用解析失败，整批未执行。请修正后重新调用。{error}")}));
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
        if self.ai.bound_doc_uid != Some(self.doc_uid)
            || self
                .ai
                .active_round
                .as_ref()
                .is_none_or(|t| t.revision != (self.doc_gen, self.wb_epoch))
        {
            self.fail_ai_round("工作簿在等待确认时已变化，操作已终止。".into(), false);
            return;
        }
        let view = (self.sheet_idx, self.sel_rect());
        if let Some(turn) = &mut self.ai.active_round {
            // The approved target has already been resolved. Later implicit targets still refer
            // to the view the model saw before the user moved the selection.
            turn.view_changed_during_approval = turn.view != view;
        }
        self.continue_ai_tools(ctx, true);
    }

    fn continue_ai_tools(&mut self, ctx: &egui::Context, mut approved: bool) {
        loop {
            let view = (self.sheet_idx, self.sel_rect());
            let Some(turn) = self.ai.active_round.as_mut() else {
                return;
            };
            let Some(mut call) = turn.pending_calls.pop_front() else {
                self.next_ai_round();
                return;
            };
            if !approved && turn.view_changed_during_approval {
                self.fail_ai_round("活动选区在确认期间已变化，后续工具已终止。".into(), false);
                return;
            }
            if !std::mem::take(&mut approved) {
                match self.stage_ai_confirmation(&mut call.spec) {
                    Ok(Some(label)) => {
                        let turn = self.ai.active_round.as_mut().unwrap();
                        turn.revision = (self.doc_gen, self.wb_epoch);
                        turn.view = view;
                        turn.pending_calls.push_front(call);
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
                    self.record_ai_observation(call.id, label, result)
                }
                AiToolOutput::Calculating(calculation) => {
                    let view = (self.sheet_idx, self.sel_rect());
                    let turn = self.ai.active_round.as_mut().unwrap();
                    turn.revision = (self.doc_gen, self.wb_epoch);
                    turn.view = view;
                    turn.calculation = Some((call.id, calculation));
                    self.ai.status_hint = "正在核对表格计算结果…".into();
                    return;
                }
            }
        }
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
            view: (app.sheet_idx, app.sel_rect()),
            view_changed_during_approval: false,
            revision: (app.doc_gen, app.wb_epoch),
            content_start: 0,
            reasoning_start: 0,
        });
        app
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
        let tool = WireToolCall {
            id: "read".into(),
            spec: ToolCallSpec {
                name: "inspect_sheet".into(),
                arguments: json!({}),
            },
        };
        app.finish_ai_round(
            String::new(),
            Some("工具推理标记".into()),
            Ok(vec![tool]),
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
    fn ai_tool_reply_after_selection_or_edit_does_not_execute() {
        for change_selection in [true, false] {
            let ctx = egui::Context::default();
            let mut app = pending_app(&ctx);
            if change_selection {
                app.sel = (5, 5);
                app.sel_end = app.sel;
            } else {
                app.doc_gen += 1;
            }
            let call = WireToolCall {
                id: "read".into(),
                spec: ToolCallSpec {
                    name: "inspect_sheet".into(),
                    arguments: json!({}),
                },
            };
            app.finish_ai_round(String::new(), None, Ok(vec![call]), &ctx);
            assert!(!app.ai.busy);
            assert!(app.ai.messages.last().unwrap().tool_logs.is_empty());
            assert!(
                app.ai
                    .messages
                    .last()
                    .unwrap()
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("变化")
            );
        }
    }

    #[test]
    fn ai_last_round_reports_unexecuted_tools() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        app.ai.round = MAX_TOOL_ROUNDS;
        app.finish_ai_round(
            "还需读取一个单元格".into(),
            None,
            Ok(vec![WireToolCall {
                id: "read".into(),
                spec: ToolCallSpec {
                    name: "inspect_sheet".into(),
                    arguments: json!({}),
                },
            }]),
            &ctx,
        );
        assert!(!app.ai.busy);
        assert!(app.ai.status_hint.contains("上限"));
        assert!(app.ai.messages.last().unwrap().tool_logs.is_empty());
        assert!(app.ai.wire_history.is_empty());
    }

    #[test]
    fn ai_calculation_after_selection_change_drops_snapshot_observation() {
        let ctx = egui::Context::default();
        let mut app = pending_app(&ctx);
        app.wb.calc_pending = true;
        let AiToolOutput::Calculating(worker) =
            app.with_calculated_ai_view(&ctx, |_| ("读取完成".into(), json!({})))
        else {
            panic!("pending workbook did not start a calculation");
        };
        app.ai.active_round.as_mut().unwrap().calculation = Some(("read".into(), worker));
        app.sel = (8, 8);
        app.sel_end = app.sel;
        let start = Instant::now();
        while app.ai.busy && start.elapsed() < Duration::from_secs(5) {
            app.poll_ai(&ctx);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!app.ai.busy);
        assert!(app.ai.messages.last().unwrap().tool_logs.is_empty());
        assert!(app.ai.status_hint.contains("变化"));
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
