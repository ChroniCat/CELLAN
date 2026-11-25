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
fn ai_first_manual_inspection_recalculates_if_precedent_changes() {
    let mut chat = Conversation::new();
    chat.app.manual_calc = true;
    chat.app.wb.calc_manual = true;
    let input = |n| Cell { value: Value::Number(n), formula: None };
    chat.app.wb.sheet_mut(0).set(0, 0, input(1.0));
    chat.app.wb.sheet_mut(0).set(0, 1, Cell {
        value: Value::Empty,
        formula: Some("A1*2".into()),
    });
    chat.app.fast_recalc_cells(&[(0, 0, 0), (0, 0, 1)]);
    chat.app.start_ai_prompt("读取 B1".into());
    let (first, _) = chat.request();
    respond(first, tool_event("inspect_sheet", json!({"range":"B1"})));
    chat.poll_until(|app| app.ai.status_hint.starts_with("正在核对表格计算结果"));

    chat.app.wb.sheet_mut(0).set(0, 0, input(3.0));
    chat.app.fast_recalc_cells(&[(0, 0, 0)]);
    chat.app.doc_gen += 1;
    let (second, request) = chat.request();
    let observation = request["messages"].as_array().unwrap().last().unwrap();
    let result: JsonValue = serde_json::from_str(observation["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["cells"][0]["value"], "6");
    assert_eq!(chat.app.wb.sheet(0).value(0, 1), Value::Empty);
    respond(second, json!({"choices":[{"delta":{"content":"B1 是 6"}}]}));
    chat.poll_until(|app| !app.ai.busy);
}

#[test]
fn ai_write_samples_follow_the_written_sheet_after_reordering() {
    let mut chat = Conversation::new();
    let target = chat.app.wb.sheet(0).name.clone();
    chat.app.add_sheet();
    let number = |n| Cell { value: Value::Number(n), formula: None };
    chat.app.wb.sheet_mut(1).set(0, 0, number(80.0));
    for row in 0..1300 {
        chat.app.wb.sheet_mut(0).set(row, 4, number(10.0));
    }
    chat.app.start_ai_prompt("填入序列并核对".into());
    let (first, _) = chat.request();
    respond(first, tool_event("fill_formula_range", json!({
        "sheet": target, "range":"A1", "top_left_formula":"=SEQUENCE(2)"
    })));
    chat.poll_until(|app| app.ai.status_hint.starts_with("正在核对表格计算结果"));

    chat.app.move_sheet(0, 1);
    assert_eq!(chat.app.wb.sheet(1).name, target);
    let (second, request) = chat.request();
    let observation = request["messages"].as_array().unwrap().last().unwrap();
    let result: JsonValue = serde_json::from_str(observation["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["samples"][0]["value"], "1");
    assert_eq!(chat.app.wb.sheet(0).value(0, 0), Value::Number(80.0));
    respond(second, json!({"choices":[{"delta":{"content":"已核对"}}]}));
    chat.poll_until(|app| !app.ai.busy);
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

#[test]
fn ai_executes_multi_step_workflow_beyond_eight_rounds() {
    assert_eq!(MAX_TOOL_ROUNDS, 64);
    let mut chat = Conversation::new();
    chat.app.start_ai_prompt("分 12 步依次写入单元格".into());
    for step in 0..12u32 {
        let (stream, _) = chat.request();
        let addr = cell_name(step, 0);
        respond(
            stream,
            tool_event("set_cells", json!({"edits":[{"cell":addr,"value":step + 1}]})),
        );
    }
    let (final_stream, _) = chat.request();
    respond(
        final_stream,
        json!({"choices":[{"delta":{"content":"12 步全部完成"}}]}),
    );
    chat.poll_until(|app| !app.ai.busy);
    let last = chat.app.ai.messages.last().unwrap();
    assert!(last.error.is_none(), "error: {:?}", last.error);
    assert_eq!(last.content, "12 步全部完成");
    assert_eq!(last.tool_logs.len(), 12);
    for step in 0..12u32 {
        assert_eq!(
            chat.app.wb.sheet(0).value(step, 0),
            Value::Number(f64::from(step + 1))
        );
    }
    chat.app.undo_last_ai_change();
    for step in 0..12u32 {
        assert_eq!(chat.app.wb.sheet(0).value(step, 0), Value::Empty);
    }
}

#[test]
fn ai_large_data_operation_completes_asynchronously_without_false_conflict_and_supports_undo() {
    let mut chat = Conversation::new();
    // 6,000 × 4 = 24,000 cells (> ASYNC_REORDER_CELLS = 20,000), triggering background run_reorder!
    let sh = chat.app.wb.sheet_mut(0);
    for row in 0..6_000u32 {
        let v = f64::from(6_000 - row);
        for col in 0..4u32 {
            sh.set(
                row,
                col,
                Cell {
                    value: Value::Number(v + f64::from(col)),
                    formula: None,
                },
            );
        }
    }

    chat.app.start_ai_prompt("先查看顶部数据再对 A1:D6000 升序排序".into());
    let (first, _) = chat.request();
    respond(first, tool_event("inspect_sheet", json!({"range": "A1:D5"})));

    let (second, _) = chat.request();
    respond(
        second,
        tool_event(
            "data_operation",
            json!({"op": "sort_asc", "range": "A1:D6000"}),
        ),
    );
    // Large sort on populated cells stages a confirmation card; approve it, then await async reorder!
    chat.poll_until(|app| app.ai.confirmation_label.is_some());
    chat.app.answer_ai_confirmation(true, &chat.ctx);

    let (third, followup) = chat.request();
    let tool_msg = followup["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(tool_msg["role"], "tool");
    let parsed: JsonValue = serde_json::from_str(tool_msg["content"].as_str().unwrap()).unwrap();
    assert_eq!(parsed["ok"], true, "async reorder must report ok=true: {parsed}");
    assert_eq!(chat.app.wb.sheet(0).value(0, 0), Value::Number(1.0));
    assert_eq!(chat.app.wb.sheet(0).value(5_999, 0), Value::Number(6_000.0));

    respond(
        third,
        json!({"choices":[{"delta":{"content":"已完成 6000 行升序排序"}}]}),
    );
    chat.poll_until(|app| !app.ai.busy);
    let last = chat.app.ai.messages.last().unwrap();
    assert!(last.error.is_none(), "unexpected conflict error: {:?}", last.error);
    assert_eq!(last.undo_ids.len(), 1);

    chat.app.undo_last_ai_change();
    assert_eq!(chat.app.wb.sheet(0).value(0, 0), Value::Number(6_000.0));
}

#[test]
fn ai_manual_large_workbook_reuses_calc_base_deps_and_broadcasts_fill_formula_styles() {
    let mut chat = Conversation::new();
    chat.app.manual_calc = true;
    chat.app.wb.calc_manual = true;
    chat.app.wb.calc_pending = true;
    chat.app.deps_built = false;

    // Pair with a LAN Guest so we also verify tool_fill_formula_range broadcasts number formats!
    let (net_host, port) = crate::net::host_for_test(1, "Host", "ai-fill-doc", "pw");
    let mut peer_host = sheetcore::sync::Peer::new(1);
    for row in 0..3_000u32 {
        chat.app.wb.sheet_mut(0).set(
            row,
            0,
            Cell {
                value: Value::Number(f64::from(row + 1)),
                formula: None,
            },
        );
    }
    peer_host.seed_workbook(&chat.app.wb);
    net_host.seed_snapshot(
        &peer_host,
        &chat.app.wb,
        sheetcore::sync::Cursor {
            site: 1,
            name: "Host".into(),
            color: 1,
            sheet: 0,
            row: 0,
            col: 0,
            end_row: None,
            end_col: None,
        },
    );
    chat.app.peer = Some(peer_host);
    chat.app.net = Some(net_host);
    chat.app.undo.set_site(1);

    let mut guest = SheetApp::new_with_context(&chat.ctx, None);
    guest.net = Some(
        crate::net::join(
            2,
            "Guest".into(),
            "ai-fill-doc".into(),
            &format!("127.0.0.1:{port}"),
            "pw".into(),
        )
        .unwrap(),
    );
    guest.peer = Some(sheetcore::sync::Peer::new(2));
    let sync_deadline = Instant::now() + Duration::from_secs(8);
    while guest.in_snapshot || guest.wb.sheet(0).value(2_999, 0) != Value::Number(3_000.0) {
        chat.app.poll_net();
        guest.poll_net();
        assert!(Instant::now() < sync_deadline, "guest sync timed out");
        std::thread::sleep(Duration::from_millis(2));
    }

    chat.app.start_ai_prompt("在手动重算大表上填充公式并设置格式".into());
    let (first, _) = chat.request();
    respond(
        first,
        tool_event(
            "fill_formula_range",
            json!({
                "range": "B1:B50",
                "top_left_formula": "=A1*2",
                "format": "0.00"
            }),
        ),
    );
    let (second, req2) = chat.request();
    assert!(!chat.app.deps_built, "manual mode must not force live deps_built");
    assert!(
        chat.app
            .ai
            .calc_base
            .as_ref()
            .is_some_and(|b| b.has_deps()),
        "AiCalcBase must retain built Deps after initial manual snapshot calc"
    );
    let obs1: JsonValue = serde_json::from_str(
        req2["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(obs1["ok"], true);
    assert_eq!(obs1["samples"][0]["value"], "2");

    // Round 2 write overwrites A1 (approving confirmation) and reuses AiCalcBase.deps incrementally!
    respond(
        second,
        tool_event("set_cells", json!({"edits": [{"cell": "A1", "value": 10}]})),
    );
    chat.poll_until(|app| app.ai.confirmation_label.is_some());
    chat.app.answer_ai_confirmation(true, &chat.ctx);
    let (third, _) = chat.request();
    respond(third, tool_event("inspect_sheet", json!({"range": "B1:B2"})));
    let (fourth, req4) = chat.request();
    let obs3: JsonValue = serde_json::from_str(
        req4["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(obs3["cells"][0]["value"], "20.00");
    assert_eq!(obs3["cells"][1]["value"], "4.00");

    respond(fourth, json!({"choices":[{"delta":{"content":"已验证"}}]}));
    chat.poll_until(|app| !app.ai.busy);

    // Verify LAN Guest received both translated formulas AND the "0.00" format broadcast!
    let style_deadline = Instant::now() + Duration::from_secs(8);
    while guest.wb.styles.get(guest.wb.sheet(0).style_of(49, 1)).format != "0.00" {
        chat.app.poll_net();
        guest.poll_net();
        assert!(Instant::now() < style_deadline, "guest did not receive fill_formula_range format");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        guest.wb.sheet(0).get(49, 1).unwrap().formula.as_deref(),
        Some("A50*2")
    );
}

#[test]
fn ai_consecutive_write_tools_coalesce_recalc_and_streaming_blocks_stay_stable() {
    // 1. 验证长流式回复增量分块：闭合段落前缀在后续 SSE token 追加时保持 100% 字节稳定（命中 egui Galley 缓存）
    let long_para_a = "第一段财务分析结论：".repeat(50); // > 768 bytes
    let long_para_b = "第二段现金流预测明细：".repeat(50); // > 768 bytes
    let s1 = format!("{long_para_a}\n\n{long_para_b}\n\n正在生成的尾部第1字");
    let s2 = format!("{long_para_a}\n\n{long_para_b}\n\n正在生成的尾部第1字、第2字、第3字");
    let b1 = crate::ai_agent::split_streaming_text_blocks(&s1);
    let b2 = crate::ai_agent::split_streaming_text_blocks(&s2);
    assert!(b1.len() >= 3 && b2.len() == b1.len());
    assert_eq!(
        b1[..b1.len() - 1],
        b2[..b2.len() - 1],
        "已封口的历史段落块在追加新 SSE token 时必须完全不变，仅最后一个活跃尾块重排"
    );

    // 2. 验证单轮连续多个无确认写工具调用（set_cells + fill_formula_range）合并重算且保留逐条撤销
    let mut chat = Conversation::new();
    chat.app.start_ai_prompt("录入三行基础数据并填充汇总公式".into());
    let (first, _) = chat.request();
    let args_w1 = json!({
        "edits": [
            {"cell": "A1", "value": 10},
            {"cell": "A2", "value": 20},
            {"cell": "A3", "value": 30}
        ]
    })
    .to_string();
    let args_w2 = json!({
        "range": "B1:B3",
        "top_left_formula": "=A1*10"
    })
    .to_string();
    respond(
        first,
        json!({
            "choices": [{
                "delta": {
                    "tool_calls": [
                        {"index": 0, "id": "call_w1", "function": {"name": "set_cells", "arguments": args_w1}},
                        {"index": 1, "id": "call_w2", "function": {"name": "fill_formula_range", "arguments": args_w2}}
                    ]
                }
            }]
        }),
    );

    let (second, req2) = chat.request();
    let msgs = req2["messages"].as_array().unwrap();
    let n = msgs.len();
    let obs_w1: JsonValue = serde_json::from_str(msgs[n - 2]["content"].as_str().unwrap()).unwrap();
    let obs_w2: JsonValue = serde_json::from_str(msgs[n - 1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(obs_w1["ok"], true);
    assert_eq!(obs_w2["ok"], true);
    assert_eq!(obs_w2["samples"][0]["value"], "100");
    assert_eq!(chat.app.wb.sheet(0).value(2, 1), Value::Number(300.0));

    respond(second, json!({"choices": [{"delta": {"content": "已完成批量写入"}}]}));
    chat.poll_until(|app| !app.ai.busy);

    // 两个写工具各自的 undo_id 均被完整保留在当前轮消息中，一键撤销同时回退两条写入
    assert_eq!(chat.app.ai.messages.last().unwrap().undo_ids.len(), 2);
    chat.app.undo_last_ai_change();
    assert_eq!(chat.app.wb.sheet(0).value(2, 1), Value::Empty);
    assert_eq!(chat.app.wb.sheet(0).value(2, 0), Value::Empty);
}
