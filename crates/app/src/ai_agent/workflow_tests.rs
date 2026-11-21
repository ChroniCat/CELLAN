use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

struct Conversation {
    ctx: egui::Context,
    app: SheetApp,
    listener: TcpListener,
    _env: crate::test_env::TestEnv,
}

impl Conversation {
    fn new() -> Self {
        let env = crate::test_env::TestEnv::new("ai_conversation");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        app.ai.cfg.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        app.ai.cfg.api_key = "local-test".into();
        app.ai.cfg.data_consent = true;
        Self {
            ctx,
            app,
            listener,
            _env: env,
        }
    }

    fn poll_until(&mut self, done: impl Fn(&SheetApp) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(&self.app) {
            self.app.poll_ai(&self.ctx);
            assert!(Instant::now() < deadline, "{}", self.app.ai.status_hint);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn request(&mut self) -> (TcpStream, JsonValue) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
            self.app.poll_ai(&self.ctx);
            assert!(Instant::now() < deadline, "{}", self.app.ai.status_hint);
            std::thread::sleep(Duration::from_millis(5));
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let mut reader = BufReader::new(&stream);
        let mut size = None;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':')
                && key.eq_ignore_ascii_case("content-length")
            {
                size = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let mut body = vec![0; size.unwrap()];
        reader.read_exact(&mut body).unwrap();
        let body = serde_json::from_slice(&body).unwrap();
        (stream, body)
    }
}

fn respond(mut stream: TcpStream, event: JsonValue) {
    let body = format!("data: {event}\n\ndata: [DONE]\n\n");
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}

fn tool_event(name: &str, args: JsonValue) -> JsonValue {
    json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_unit","type":"function",
        "function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
}

#[test]
fn ai_stop_then_send_excludes_abandoned_prompt() {
    let mut chat = Conversation::new();
    chat.app.start_ai_prompt("first abandoned question".into());
    let (_stalled, _) = chat.request();
    chat.app.stop_ai_generation();
    chat.app.start_ai_prompt("second live question".into());
    let (second, body) = chat.request();
    assert!(!body.to_string().contains("first abandoned question"));
    assert!(body.to_string().contains("second live question"));
    respond(
        second,
        json!({"choices":[{"delta":{"content":"第二条已送达"}}]}),
    );
    chat.poll_until(|app| !app.ai.busy);
    assert_eq!(chat.app.ai.messages.last().unwrap().content, "第二条已送达");
}

#[test]
fn ai_retry_retains_tool_results_and_undo_ids() {
    let mut chat = Conversation::new();
    chat.app.start_ai_prompt("填写 A1".into());
    let (first, _) = chat.request();
    respond(
        first,
        tool_event("set_cells", json!({"edits":[{"cell":"A1","value":17}]})),
    );
    let (second, before_retry) = chat.request();
    assert_eq!(chat.app.wb.sheet(0).value(0, 0), Value::Number(17.0));
    let ids = chat.app.ai.messages.last().unwrap().undo_ids.clone();
    assert_eq!(ids.len(), 1);
    respond(second, json!({"error":{"message":"本次推理暂时失败"}}));
    chat.poll_until(|app| !app.ai.busy);
    assert!(chat.app.ai.messages.last().unwrap().error.is_some());
    assert!(chat.app.can_retry_ai_round());
    chat.app.retry_failed_ai_round();
    let (third, after_retry) = chat.request();
    assert_eq!(after_retry["messages"], before_retry["messages"]);
    respond(
        third,
        json!({"choices":[{"delta":{"content":"A1 已填写"}}]}),
    );
    chat.poll_until(|app| !app.ai.busy);
    assert_eq!(chat.app.ai.messages.last().unwrap().undo_ids, ids);
    assert_eq!(chat.app.ai.messages.len(), 2);
    chat.app.undo_last_ai_change();
    assert_eq!(chat.app.wb.sheet(0).value(0, 0), Value::Empty);
}

#[test]
fn ai_workbook_data_is_returned_only_after_a_real_tool_call() {
    let mut chat = Conversation::new();
    chat.app.wb.sheet_mut(0).set(
        0,
        0,
        Cell {
            value: Value::Text("sample-only-from-workbook".into()),
            formula: None,
        },
    );
    chat.app.start_ai_prompt("查看表格".into());
    let (first, opening) = chat.request();
    assert!(!opening.to_string().contains("sample-only-from-workbook"));
    assert!(
        opening["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["tool_calls"].is_null() && m["role"] != "tool")
    );
    respond(first, tool_event("inspect_workbook", json!({})));
    let (second, followup) = chat.request();
    let messages = followup["messages"].as_array().unwrap();
    let observation = messages.last().unwrap();
    assert_eq!(observation["role"], "tool");
    assert_eq!(observation["tool_call_id"], "call_unit");
    assert!(
        observation["content"]
            .as_str()
            .unwrap()
            .contains("sample-only-from-workbook")
    );
    respond(second, json!({"choices":[{"delta":{"content":"已读取"}}]}));
    chat.poll_until(|app| !app.ai.busy);

    chat.app.start_ai_prompt("谢谢".into());
    let (third, next_turn) = chat.request();
    assert!(!next_turn.to_string().contains("sample-only-from-workbook"));
    respond(third, json!({"choices":[{"delta":{"content":"不客气"}}]}));
    chat.poll_until(|app| !app.ai.busy);
}

#[test]
fn ai_clear_waits_for_approval_and_uses_displayed_selection() {
    let mut chat = Conversation::new();
    for col in [0, 1] {
        chat.app.wb.sheet_mut(0).set(
            0,
            col,
            Cell {
                value: Value::Number(8.0),
                formula: None,
            },
        );
    }
    chat.app.start_ai_prompt("清空当前单元格".into());
    let (first, _) = chat.request();
    respond(
        first,
        tool_event("data_operation", json!({"op":"clear_contents"})),
    );
    chat.poll_until(|app| app.ai.confirmation_label.is_some());
    assert!(
        chat.app
            .ai
            .confirmation_label
            .as_deref()
            .unwrap()
            .contains("A1:A1")
    );
    assert_eq!(chat.app.wb.sheet(0).value(0, 0), Value::Number(8.0));
    chat.app.sel = (0, 1);
    chat.app.sel_end = (0, 1);
    chat.app.answer_ai_confirmation(true, &chat.ctx);
    let (second, _) = chat.request();
    respond(second, json!({"choices":[{"delta":{"content":"已清空"}}]}));
    chat.poll_until(|app| !app.ai.busy);
    assert_eq!(chat.app.wb.sheet(0).value(0, 0), Value::Empty);
    assert_eq!(chat.app.wb.sheet(0).value(0, 1), Value::Number(8.0));
}

#[test]
fn ai_rejected_or_stale_approval_preserves_cells() {
    let number = |n| Cell {
        value: Value::Number(n),
        formula: None,
    };
    // (edit made while the card is open, approved, expected A1 afterwards)
    for (edit, approved, expected) in [
        (None, false, Value::Number(8.0)),
        (Some((0, 0, 9.0)), true, Value::Number(9.0)),
        (Some((4, 2, 1.0)), true, Value::Empty),
    ] {
        let mut chat = Conversation::new();
        chat.app.wb.sheet_mut(0).set(0, 0, number(8.0));
        chat.app.start_ai_prompt("清空 A1".into());
        let (first, _) = chat.request();
        respond(
            first,
            tool_event(
                "data_operation",
                json!({"op":"clear_contents","range":"A1"}),
            ),
        );
        chat.poll_until(|app| app.ai.confirmation_label.is_some());
        if let Some((row, col, n)) = edit {
            // A collaborator or the user edits a cell before the card is answered.
            chat.app.wb.sheet_mut(0).set(row, col, number(n));
            chat.app.doc_gen += 1;
        }
        chat.app.answer_ai_confirmation(approved, &chat.ctx);
        if expected == Value::Empty {
            // Only the approved target matters: an unrelated edit does not cancel it.
            let (next, _) = chat.request();
            respond(next, json!({"choices":[{"delta":{"content":"已清空"}}]}));
            chat.poll_until(|app| !app.ai.busy);
            assert_eq!(chat.app.wb.sheet(0).value(4, 2), Value::Number(1.0));
        } else {
            assert!(!chat.app.ai.busy);
        }
        assert_eq!(chat.app.wb.sheet(0).value(0, 0), expected);
        if approved && expected != Value::Empty {
            let error = chat.app.ai.messages.last().unwrap().error.clone().unwrap();
            assert!(error.contains("变化"), "{error}");
        }
    }
}

#[test]
fn ai_reports_invalid_call_to_model_and_refuses_edits_at_round_limit() {
    let mut chat = Conversation::new();
    chat.app.start_ai_prompt("读取 A1".into());
    let (first, _) = chat.request();
    let mut invalid = tool_event("inspect_sheet", json!({"range":"A1"}));
    invalid["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"] = json!("{");
    respond(first, invalid);

    for round in 2..=MAX_TOOL_ROUNDS {
        let (stream, request) = chat.request();
        let messages = request["messages"].as_array().unwrap();
        if round == 2 {
            let repair = messages.last().unwrap();
            assert!(repair["content"].as_str().unwrap().contains("解析失败"));
            // Some chat templates reject a system message anywhere but first.
            assert_eq!(repair["role"], "user");
            assert!(messages.iter().skip(1).all(|m| m["role"] != "system"));
        } else {
            assert_eq!(messages.last().unwrap()["role"], "tool");
        }
        let next = if round == MAX_TOOL_ROUNDS {
            tool_event("set_cells", json!({"edits":[{"cell":"A1","value":8}]}))
        } else {
            tool_event("inspect_sheet", json!({"range":"A1"}))
        };
        respond(stream, next);
    }
    chat.poll_until(|app| !app.ai.busy);
    assert!(
        chat.app
            .ai
            .messages
            .last()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("上限")
    );
    assert_eq!(chat.app.wb.sheet(0).value(0, 0), Value::Empty);
    assert!(!chat.app.can_retry_ai_round());
}

#[test]
fn ai_text_only_gateway_can_resume_an_existing_tool_round() {
    let mut chat = Conversation::new();
    chat.app.start_ai_prompt("写入 A1 后核对".into());
    let (first, _) = chat.request();
    respond(
        first,
        tool_event(
            "set_cells",
            json!({"edits":[{"cell":"A1","value":"text-mode-observation"}]}),
        ),
    );
    let (mut rejected, _) = chat.request();
    let error = r#"{"error":{"message":"model does not support tools"}}"#;
    write!(
        rejected,
        "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{error}",
        error.len()
    )
    .unwrap();
    drop(rejected);
    chat.poll_until(|app| !app.ai.busy);
    let ids = chat.app.ai.messages.last().unwrap().undo_ids.clone();
    assert_eq!(ids.len(), 1);
    assert!(chat.app.ai.status_hint.contains("原生工具调用"));

    let mut cfg = chat.app.ai.cfg.clone();
    cfg.native_tools = false;
    chat.app.ai.update_retry_tool_protocol(&cfg);
    chat.app.ai.cfg = cfg;
    assert!(chat.app.can_retry_ai_round());
    chat.app.retry_failed_ai_round();
    let (third, text_request) = chat.request();
    assert!(text_request.get("tools").is_none());
    let messages = text_request["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .all(|m| m["role"] != "tool" && m.get("tool_calls").is_none())
    );
    assert!(messages.iter().any(|m| {
        m["role"] == "assistant"
            && m["content"]
                .as_str()
                .unwrap()
                .contains("text-mode-observation")
    }));
    assert!(!messages.iter().any(|m| {
        m["role"] != "assistant"
            && m["content"]
                .as_str()
                .unwrap()
                .contains("text-mode-observation")
    }));
    let tag = r#"<tool_call>{"name":"inspect_sheet","arguments":{"range":"A1"}}</tool_call>"#;
    respond(third, json!({"choices":[{"delta":{"content":tag}}]}));
    let (last, followup) = chat.request();
    assert!(followup.get("tools").is_none());
    assert_eq!(
        followup["messages"].as_array().unwrap().last().unwrap()["role"],
        "user"
    );
    respond(last, json!({"choices":[{"delta":{"content":"已核对"}}]}));
    chat.poll_until(|app| !app.ai.busy);
    assert_eq!(chat.app.ai.messages.last().unwrap().undo_ids, ids);
    assert_eq!(chat.app.ai.messages.last().unwrap().content, "已核对");
}
