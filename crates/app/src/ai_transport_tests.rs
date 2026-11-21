use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(4);
const HEADERS: &[u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n";
const DELTA: &[u8] = b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n";

struct Client {
    listener: TcpListener,
    cancel: Arc<RequestCancel>,
    events: mpsc::Receiver<StreamMsg>,
    result: mpsc::Receiver<Result<(), String>>,
}

fn client() -> Client {
    client_with_unframed_text(false)
}

fn client_with_unframed_text(allow_unframed_text: bool) -> Client {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ep = ParsedUrl {
        https: false,
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        base_path: "/v1/chat/completions".into(),
    };
    let (tx, events) = mpsc::channel();
    let (send_result, result) = mpsc::channel();
    let cancel = Arc::new(RequestCancel::default());
    let token = cancel.clone();
    thread::spawn(move || {
        let _ = send_result.send(stream_post(
            &ep,
            "",
            b"{}",
            &token,
            &tx,
            None,
            allow_unframed_text,
        ));
    });
    Client {
        listener,
        cancel,
        events,
        result,
    }
}

fn accept_request(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let start = Instant::now();
    let (stream, _) = loop {
        match listener.accept() {
            Ok(conn) => break conn,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && start.elapsed() < DEADLINE => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("expected worker POST: {e}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.starts_with("POST /v1/chat/completions "));
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
    }
    let mut body = [0; 2];
    reader.read_exact(&mut body).unwrap();
    assert_eq!(&body, b"{}");
    reader.into_inner()
}

fn chunk(stream: &mut TcpStream, data: &[u8]) {
    write!(stream, "{:x}\r\n", data.len()).unwrap();
    stream.write_all(data).unwrap();
    stream.write_all(b"\r\n").unwrap();
    stream.flush().unwrap();
}

fn assert_next_delta(client: &Client, expected: &str) {
    let event = client
        .events
        .recv_timeout(DEADLINE)
        .expect("delta before next chunk");
    assert!(matches!(event, StreamMsg::ContentDelta(s) if s == expected));
}

#[test]
fn chunked_delta_and_done_are_delivered_while_the_server_is_still_open() {
    let c = client();
    let mut server = accept_request(&c.listener);
    server.write_all(HEADERS).unwrap();
    // Each display event gates the next numbered chunk. The worker cannot
    // collect several deltas into a batch and still satisfy these handshakes.
    let mut complete = String::new();
    for i in 1..=6 {
        let piece = format!("第{i}段");
        let json = serde_json::json!({"choices":[{"delta":{"content":piece}}]});
        chunk(&mut server, format!("data: {json}\n\n").as_bytes());
        assert_next_delta(&c, &piece);
        complete.push_str(&piece);
    }
    chunk(&mut server, b"data: [DO");
    chunk(&mut server, b"NE]\n\n");
    assert!(matches!(
        c.events.recv_timeout(DEADLINE),
        Ok(StreamMsg::RoundDone { visible_content, .. }) if visible_content == complete
    ));
    c.result.recv_timeout(DEADLINE).unwrap().unwrap();
    // The HTTP zero chunk and FIN have deliberately not been sent.
}

#[test]
fn legacy_text_gateway_completes_without_a_blank_line_while_the_socket_stays_open() {
    let c = client_with_unframed_text(true);
    let mut server = accept_request(&c.listener);
    server.write_all(HEADERS).unwrap();
    chunk(&mut server, DELTA);
    assert_next_delta(&c, "hello");
    // No [DONE], no trailing blank line, no zero chunk and no FIN.
    chunk(
        &mut server,
        b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n",
    );
    assert!(matches!(
        c.events.recv_timeout(DEADLINE),
        Ok(StreamMsg::RoundDone { visible_content, .. }) if visible_content == "hello"
    ));
    c.result.recv_timeout(DEADLINE).unwrap().unwrap();
}

#[test]
fn late_error_field_in_the_same_event_wins_over_the_apparent_stop() {
    let c = client();
    let mut server = accept_request(&c.listener);
    server.write_all(HEADERS).unwrap();
    chunk(&mut server, DELTA);
    assert_next_delta(&c, "hello");
    chunk(&mut server, b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"message\":\"late error\"}\n");
    // A TCP or HTTP chunk boundary and a long pause are both valid within an SSE event.
    assert!(matches!(
        c.result
            .recv_timeout(UNFRAMED_TERMINAL_GRACE + Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    chunk(&mut server, b"event: error\n\n");
    assert!(
        c.result
            .recv_timeout(DEADLINE)
            .unwrap()
            .unwrap_err()
            .contains("late error")
    );
    assert!(
        !c.events
            .try_iter()
            .any(|e| matches!(e, StreamMsg::RoundDone { .. }))
    );
}

#[test]
fn attempt_records_the_done_marker_separately_from_the_choice_finish_reason() {
    let stop = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
    for (end, saw_done, reason) in [
        ("data: [DONE]\n\n".to_owned(), true, None),
        (stop.to_owned(), false, Some("stop")),
        (format!("{stop}data: [DONE]\n\n"), true, Some("stop")),
    ] {
        let bytes = [DELTA, end.as_bytes()].concat();
        let mut source = bytes.as_slice();
        let (tx, _) = mpsc::channel();
        let mut trace = Attempt::new();
        receive_body(
            200,
            |buf, _| source.read(buf).map_err(|e| e.to_string()),
            &mut trace,
            &tx,
            None,
            false,
        )
        .unwrap();
        assert_eq!(trace.saw_done, saw_done);
        assert_eq!(trace.finish_reason.as_deref(), reason);
    }
}

#[cfg(windows)]
#[test]
fn winhttp_proxy_replay_requires_a_failure_before_the_post_could_be_sent() {
    use windows_sys::Win32::Networking::WinHttp::*;
    let replay = platform::replayable_send_failure;
    assert!(replay(ERROR_WINHTTP_NAME_NOT_RESOLVED, 0));
    assert!(replay(ERROR_WINHTTP_CANNOT_CONNECT, 0));
    assert!(replay(
        ERROR_WINHTTP_TIMEOUT,
        WINHTTP_CALLBACK_STATUS_CONNECTING_TO_SERVER
    ));
    assert!(!replay(ERROR_WINHTTP_TIMEOUT, 0));

    // On a pooled connection WinHTTP reports SENDING_REQUEST and REQUEST_SENT without
    // CONNECTED_TO_SERVER. An earlier DNS/connect notification must never reopen replay.
    for observed in [
        WINHTTP_CALLBACK_STATUS_CONNECTED_TO_SERVER,
        WINHTTP_CALLBACK_STATUS_SENDING_REQUEST,
        WINHTTP_CALLBACK_STATUS_SENDING_REQUEST | WINHTTP_CALLBACK_STATUS_REQUEST_SENT,
        WINHTTP_CALLBACK_STATUS_RESOLVING_NAME | WINHTTP_CALLBACK_STATUS_SENDING_REQUEST,
    ] {
        for code in [
            ERROR_WINHTTP_TIMEOUT,
            ERROR_WINHTTP_CANNOT_CONNECT,
            ERROR_WINHTTP_NAME_NOT_RESOLVED,
            ERROR_WINHTTP_CONNECTION_ERROR,
        ] {
            assert!(
                !replay(code, observed),
                "observed={observed:#x} error={code}"
            );
        }
    }
}

#[test]
fn truncated_chunked_reply_keeps_the_displayed_delta_and_is_not_replayed() {
    let c = client();
    let mut server = accept_request(&c.listener);
    server.write_all(HEADERS).unwrap();
    chunk(&mut server, DELTA);
    assert_next_delta(&c, "hello");
    server.shutdown(Shutdown::Both).unwrap();
    drop(server);
    assert!(
        c.result
            .recv_timeout(DEADLINE)
            .expect("one POST only")
            .is_err()
    );
    assert!(
        !c.events
            .try_iter()
            .any(|e| matches!(e, StreamMsg::RoundDone { .. }))
    );
    assert_eq!(
        c.listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn stopping_while_waiting_for_headers_or_first_chunk_closes_the_connection() {
    for send_headers in [false, true] {
        let c = client();
        let mut server = accept_request(&c.listener);
        if send_headers {
            server.write_all(HEADERS).unwrap();
            server.flush().unwrap();
        }
        // Give the worker time to enter ReceiveResponse or QueryDataAvailable
        // with the request sent and the corresponding completion still pending.
        thread::sleep(Duration::from_millis(150));
        // No further byte will arrive to wake a blocked network call.
        c.cancel.cancel();
        assert_eq!(
            c.result.recv_timeout(DEADLINE).unwrap().unwrap_err(),
            "已停止生成"
        );
        let mut byte = [0];
        match server.read(&mut byte) {
            Ok(0) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("request socket still open or received extra bytes: {result:?}"),
        }
        assert_eq!(
            c.listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn disconnect_after_post_before_headers_is_not_retried_via_proxy() {
    let c = client();
    let server = accept_request(&c.listener);
    server.shutdown(Shutdown::Both).unwrap();
    drop(server);
    assert!(
        c.result
            .recv_timeout(DEADLINE)
            .expect("a sent POST must not be replayed")
            .is_err()
    );
    assert_eq!(
        c.listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn explicit_tool_capability_errors_name_the_endpoint_requirement() {
    let error = format_http_status_error(
        400,
        r#"{"error":{"message":"This model does not support tools"}}"#,
    );
    assert!(error.contains("支持 tools"));
    let error = format_http_status_error(422, "invalid tools parameter: name is required");
    assert!(error.contains("name is required"));
}
