//! Cancellable transports for the spreadsheet assistant's SSE completion rounds.

use super::{ParsedUrl, StreamAssembler, StreamMsg, format_http_status_error};
use eframe::egui;
#[cfg(any(not(windows), not(test)))]
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BUFFER_BYTES: usize = 8192;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ROUND_DURATION: Duration = Duration::from_secs(600);
// A few gateways leave their text-only final data line without an SSE separator or EOF.
const UNFRAMED_TERMINAL_GRACE: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct RequestCancel {
    stopped: AtomicBool,
    interrupt: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl RequestCancel {
    pub(super) fn cancel(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(interrupt) = self.interrupt.lock().unwrap().take() {
            interrupt();
        }
    }

    fn check(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::Acquire) {
            Err("已停止生成".into())
        } else {
            Ok(())
        }
    }

    fn register(&self, interrupt: impl FnOnce() + Send + 'static) -> Result<(), String> {
        let mut slot = self.interrupt.lock().unwrap();
        self.check()?;
        *slot = Some(Box::new(interrupt));
        Ok(())
    }
}

/// Only timing, byte counts, and protocol/transport status are persisted. Never log
/// request or response text, HTTP headers, API keys, or even the configured endpoint.
struct Attempt {
    started: Instant,
    started_unix_ms: u128,
    sent_ms: Option<u128>,
    headers_ms: Option<u128>,
    first_byte_ms: Option<u128>,
    bytes: usize,
    saw_done: bool,
    finish_reason: Option<&'static str>,
    status: Option<u32>,
    stage: &'static str,
    #[cfg(windows)]
    connect_failed: bool,
}

impl Attempt {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            started_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            sent_ms: None,
            headers_ms: None,
            first_byte_ms: None,
            bytes: 0,
            saw_done: false,
            finish_reason: None,
            status: None,
            stage: "connect",
            #[cfg(windows)]
            connect_failed: false,
        }
    }

    fn elapsed(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    fn record_termination(&mut self, stream: &StreamAssembler) {
        self.saw_done = stream.saw_done();
        self.finish_reason = stream.finish_reason().map(|reason| match reason {
            "stop" => "stop",
            "tool_calls" => "tool_calls",
            "length" => "length",
            _ => "other",
        });
    }

    fn log(&self, route: &str, end: &str) {
        let line = format!(
            "start_unix_ms={} route={route} sent_ms={:?} headers_ms={:?} first_byte_ms={:?} bytes={} done={} finish_reason={} http={:?} end={end} stage={} elapsed_ms={}\n",
            self.started_unix_ms,
            self.sent_ms,
            self.headers_ms,
            self.first_byte_ms,
            self.bytes,
            self.saw_done,
            self.finish_reason.unwrap_or("-"),
            self.status,
            self.stage,
            self.elapsed()
        );
        #[cfg(test)]
        eprint!("{line}");
        #[cfg(not(test))]
        {
            static LOG_LOCK: Mutex<()> = Mutex::new(());
            let Ok(_lock) = LOG_LOCK.lock() else { return };
            let dir = crate::update::app_dir();
            let path = dir.join("ai-stream.log");
            if path.metadata().is_ok_and(|m| m.len() >= 256 * 1024) {
                let _ = std::fs::rename(&path, dir.join("ai-stream.prev.log"));
            }
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = file.write_all(line.as_bytes());
            }
        }
    }
}

fn receive_body(
    status: u32,
    mut read: impl FnMut(&mut [u8], bool) -> Result<usize, String>,
    trace: &mut Attempt,
    tx: &mpsc::Sender<StreamMsg>,
    wake: Option<egui::Context>,
    allow_unframed_text: bool,
) -> Result<(), String> {
    trace.status = Some(status);
    trace.stage = "body";
    let mut buf = [0; BUFFER_BYTES];
    let mut assembler = StreamAssembler::new(wake);
    let mut error_body = Vec::new();
    let success = (200..300).contains(&status);
    if matches!(status, 401 | 403 | 404 | 429 | 502 | 503 | 504) {
        return Err(format_http_status_error(status, ""));
    }
    loop {
        let unframed_terminal = allow_unframed_text && success && assembler.has_unframed_terminal();
        let n = match read(&mut buf, unframed_terminal) {
            Ok(n) => n,
            Err(_) if !success => break,
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        if trace.first_byte_ms.is_none() {
            trace.first_byte_ms = Some(trace.elapsed());
        }
        trace.bytes += n;
        if trace.bytes > MAX_RESPONSE_BYTES {
            return Err("模型响应正文超出安全大小上限".into());
        }
        if success {
            trace.stage = "sse";
            let complete = assembler.feed_bytes(&buf[..n], tx);
            trace.record_termination(&assembler);
            if complete? {
                break;
            }
            trace.stage = "body";
        } else {
            error_body.extend_from_slice(&buf[..n.min(1024 - error_body.len())]);
            if error_body.len() == 1024 {
                break;
            }
        }
    }
    if success {
        trace.stage = "sse";
        let result = assembler.finish(tx);
        trace.record_termination(&assembler);
        result
    } else {
        Err(format_http_status_error(
            status,
            &String::from_utf8_lossy(&error_body),
        ))
    }
}

#[cfg(windows)]
mod route_cache {
    use super::ParsedUrl;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    const MAX_ENTRIES: usize = 64;

    fn cache() -> &'static Mutex<HashMap<String, &'static str>> {
        static CACHE: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(super) fn key_for(ep: &ParsedUrl) -> String {
        format!("{}:{}", ep.host.to_ascii_lowercase(), ep.port)
    }

    pub(super) fn ordered_routes(key: &str) -> [&'static str; 2] {
        let preferred = cache().lock().unwrap().get(key).copied();
        if preferred == Some("auto_proxy") {
            ["auto_proxy", "direct"]
        } else {
            ["direct", "auto_proxy"]
        }
    }

    pub(super) fn remember(key: &str, route: &'static str) {
        let mut map = cache().lock().unwrap();
        if map.len() >= MAX_ENTRIES && !map.contains_key(key) {
            map.clear();
        }
        map.insert(key.to_owned(), route);
    }

    pub(super) fn invalidate(key: &str, failed_route: &'static str) {
        let mut map = cache().lock().unwrap();
        if map.get(key).copied() == Some(failed_route) {
            map.remove(key);
        }
    }
}

pub(super) fn stream_post(
    ep: &ParsedUrl,
    api_key: &str,
    body: &[u8],
    cancel: &RequestCancel,
    tx: &mpsc::Sender<StreamMsg>,
    wake: Option<egui::Context>,
    allow_unframed_text: bool,
) -> Result<(), String> {
    #[cfg(windows)]
    let route_key = route_cache::key_for(ep);
    #[cfg(windows)]
    let routes = route_cache::ordered_routes(&route_key);
    #[cfg(not(windows))]
    let routes = ["direct"];
    for (idx, route) in routes.into_iter().enumerate() {
        cancel.check()?;
        let mut trace = Attempt::new();
        let result = platform::post(
            ep,
            api_key,
            body,
            cancel,
            tx,
            wake.clone(),
            route,
            &mut trace,
            allow_unframed_text,
        );
        cancel.interrupt.lock().unwrap().take();
        let was_cancelled = cancel.check().is_err();
        let end = match (&result, was_cancelled, trace.saw_done, trace.stage) {
            (_, true, _, _) => "cancelled",
            (Ok(()), _, true, _) => "done",
            (Ok(()), _, false, _) => "finish_reason",
            (Err(_), _, _, _) if trace.status.is_some_and(|s| !(200..300).contains(&s)) => {
                "http_error"
            }
            (Err(_), _, _, "sse") => "protocol_error",
            (Err(_), _, _, _) => "transport_error",
        };
        trace.log(route, end);
        cancel.check()?;
        #[cfg(windows)]
        if result.is_ok() || trace.status.is_some() {
            route_cache::remember(&route_key, route);
            return result;
        }
        // WinHttpConnect doesn't actually open a socket until SendRequest. Only failures
        // before a TCP connection existed can be replayed without duplicating a POST.
        #[cfg(windows)]
        if idx == 0 && trace.connect_failed {
            route_cache::invalidate(&route_key, route);
            continue;
        }
        return result;
    }
    unreachable!()
}

#[cfg(windows)]
mod platform {
    use super::*;
    use core::ffi::c_void;
    use std::ptr::{null, null_mut};
    use std::sync::{Arc, atomic::AtomicU32};
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Networking::WinHttp::*;

    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }

    /// 全局复用的 WinHTTP 会话句柄（Microsoft 官方文档明确规定 `WinHttpOpen` 会话句柄线程安全，
    /// 进程级复用可保留底层 HTTP/TLS keep-alive 连接池与代理缓存，避免每轮多步推理重做 TLS 握手）。
    struct SharedSession(*mut c_void);
    unsafe impl Send for SharedSession {}
    unsafe impl Sync for SharedSession {}

    fn shared_session(route: &str) -> Result<*mut c_void, String> {
        static DIRECT: std::sync::OnceLock<SharedSession> = std::sync::OnceLock::new();
        static AUTO_PROXY: std::sync::OnceLock<SharedSession> = std::sync::OnceLock::new();
        let (slot, access) = if route == "direct" {
            (&DIRECT, WINHTTP_ACCESS_TYPE_NO_PROXY)
        } else {
            (&AUTO_PROXY, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY)
        };
        if let Some(existing) = slot.get() {
            return Ok(existing.0);
        }
        let opened = handle(
            unsafe {
                WinHttpOpen(
                    wide("CELLAN").as_ptr(),
                    access,
                    null(),
                    null(),
                    WINHTTP_FLAG_ASYNC,
                )
            },
            "初始化 HTTP 会话失败",
        )?;
        unsafe {
            WinHttpSetTimeouts(opened.0, 10_000, 15_000, 30_000, 180_000);
        }
        let raw = opened.0;
        if slot.set(SharedSession(raw)).is_ok() {
            std::mem::forget(opened);
            Ok(raw)
        } else {
            // 并发初始化时另一线程已写入缓存；本地多余句柄随 `opened` 析构安全关闭。
            Ok(slot.get().unwrap().0)
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn win_error(stage: &str, code: u32) -> String {
        let hint = match code {
            12002 => "请求超时",
            12007 => "无法解析服务器域名，请检查内网 DNS",
            12029 => "无法连接到服务器端口",
            12175 => "TLS 安全连接或证书校验失败，请检查企业根证书配置",
            _ => "网络通信失败",
        };
        format!("{stage}（{hint}，错误码 {code}）")
    }

    fn handle(raw: *mut c_void, stage: &str) -> Result<Handle, String> {
        if raw.is_null() {
            Err(win_error(stage, unsafe { GetLastError() }))
        } else {
            Ok(Handle(raw))
        }
    }

    enum Event {
        Complete { status: u32, size: u32 },
        Error(u32),
        Cancel,
    }

    struct Context {
        tx: mpsc::Sender<Event>,
        body: Vec<u8>,
        buffer: Vec<u8>,
        send_notifications: Arc<AtomicU32>,
    }

    const PRE_CONNECT: u32 =
        WINHTTP_CALLBACK_STATUS_RESOLVING_NAME | WINHTTP_CALLBACK_STATUS_CONNECTING_TO_SERVER;
    const CONNECTED_OR_SENDING: u32 = WINHTTP_CALLBACK_STATUS_CONNECTED_TO_SERVER
        | WINHTTP_CALLBACK_STATUS_SENDING_REQUEST
        | WINHTTP_CALLBACK_STATUS_REQUEST_SENT;

    pub(super) fn replayable_send_failure(code: u32, observed: u32) -> bool {
        // Pooled sockets can begin sending without a CONNECTED_TO_SERVER callback.
        // A generic timeout needs positive evidence that the request was still connecting.
        observed & CONNECTED_OR_SENDING == 0
            && (matches!(
                code,
                ERROR_WINHTTP_NAME_NOT_RESOLVED | ERROR_WINHTTP_CANNOT_CONNECT
            ) || (code == ERROR_WINHTTP_TIMEOUT && observed & PRE_CONNECT != 0))
    }

    unsafe extern "system" fn callback(
        _: *mut c_void,
        user: usize,
        status: u32,
        info: *mut c_void,
        size: u32,
    ) {
        let ctx = user as *mut Context;
        if ctx.is_null() {
            return;
        }
        if status == WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING {
            // WinHTTP guarantees this is the final callback and all I/O is over.
            unsafe {
                drop(Box::from_raw(ctx));
            }
            return;
        }
        let event = match status {
            WINHTTP_CALLBACK_STATUS_REQUEST_ERROR => {
                Event::Error(unsafe { (*info.cast::<WINHTTP_ASYNC_RESULT>()).dwError })
            }
            WINHTTP_CALLBACK_STATUS_DATA_AVAILABLE => Event::Complete {
                status,
                size: unsafe { *info.cast::<u32>() },
            },
            WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE
            | WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE
            | WINHTTP_CALLBACK_STATUS_READ_COMPLETE => Event::Complete { status, size },
            status if status & (PRE_CONNECT | CONNECTED_OR_SENDING) != 0 => {
                unsafe { &(*ctx).send_notifications }.fetch_or(status, Ordering::Release);
                return;
            }
            // Informational notifications (WinHTTP may deliver the whole connect group)
            // complete no API call; forwarding them would fail the pending wait.
            _ => return,
        };
        let _ = unsafe { &(*ctx).tx }.send(event);
    }

    // Only the worker uses the request handle. UI cancellation wakes its wait and
    // drops the guard after the async API call has returned. The callback context
    // owns both buffers until HANDLE_CLOSING, including after a cancelled read.
    pub(super) fn post(
        ep: &ParsedUrl,
        api_key: &str,
        body: &[u8],
        cancel: &RequestCancel,
        tx: &mpsc::Sender<StreamMsg>,
        wake: Option<egui::Context>,
        route: &str,
        trace: &mut Attempt,
        allow_unframed_text: bool,
    ) -> Result<(), String> {
        let session = shared_session(route)?;
        let conn = handle(
            unsafe { WinHttpConnect(session, wide(&ep.host).as_ptr(), ep.port, 0) },
            "连接模型服务器失败",
        )?;
        let req = handle(
            unsafe {
                WinHttpOpenRequest(
                    conn.0,
                    wide("POST").as_ptr(),
                    wide(&ep.base_path).as_ptr(),
                    null(),
                    null(),
                    null(),
                    if ep.https { WINHTTP_FLAG_SECURE } else { 0 },
                )
            },
            "构造 HTTP 请求失败",
        )?;
        let redirect_policy: u32 = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
        if unsafe {
            WinHttpSetOption(
                req.0,
                WINHTTP_OPTION_REDIRECT_POLICY,
                (&raw const redirect_policy).cast(),
                size_of::<u32>() as u32,
            )
        } == 0
        {
            return Err(win_error("设置重定向策略失败", unsafe { GetLastError() }));
        }

        let (events, rx) = mpsc::channel();
        cancel.register({
            let events = events.clone();
            move || {
                let _ = events.send(Event::Cancel);
            }
        })?;
        let send_notifications = Arc::new(AtomicU32::new(0));
        let mut ctx = Box::new(Context {
            tx: events,
            body: body.to_vec(),
            buffer: vec![0; BUFFER_BYTES],
            send_notifications: send_notifications.clone(),
        });
        let body_ptr = ctx.body.as_ptr();
        let read_ptr = ctx.buffer.as_mut_ptr();
        let context = Box::into_raw(ctx) as usize;
        if unsafe {
            WinHttpSetOption(
                req.0,
                WINHTTP_OPTION_CONTEXT_VALUE,
                (&raw const context).cast(),
                size_of::<usize>() as u32,
            )
        } == 0
        {
            let err = win_error("设置请求上下文失败", unsafe { GetLastError() });
            unsafe {
                drop(Box::from_raw(context as *mut Context));
            }
            return Err(err);
        }
        let notifications = PRE_CONNECT
            | CONNECTED_OR_SENDING
            | WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE
            | WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE
            | WINHTTP_CALLBACK_STATUS_DATA_AVAILABLE
            | WINHTTP_CALLBACK_STATUS_READ_COMPLETE
            | WINHTTP_CALLBACK_STATUS_REQUEST_ERROR
            | WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING;
        let prev = unsafe { WinHttpSetStatusCallback(req.0, Some(callback), notifications, 0) };
        if prev.is_some_and(|f| f as usize == usize::MAX) {
            let err = win_error("设置网络回调失败", unsafe { GetLastError() });
            unsafe {
                drop(Box::from_raw(context as *mut Context));
            }
            return Err(err);
        }
        // The final callback now owns the context and both I/O buffers.
        let round_deadline = Instant::now() + MAX_ROUND_DURATION;
        let wait_for = |ok: i32, expected: u32, unframed_terminal: bool| -> Result<u32, u32> {
            if ok == 0 {
                return Err(unsafe { GetLastError() });
            }
            let Some(remaining) = round_deadline.checked_duration_since(Instant::now()) else {
                return Err(ERROR_WINHTTP_TIMEOUT);
            };
            if remaining.is_zero() {
                return Err(ERROR_WINHTTP_TIMEOUT);
            }
            let event = if unframed_terminal {
                // Only used for QueryDataAvailable after a text-only terminal line.
                // On idle, receive_body finishes and drops the outstanding request.
                let wait_dur = remaining.min(UNFRAMED_TERMINAL_GRACE);
                match rx.recv_timeout(wait_dur) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if Instant::now() >= round_deadline {
                            return Err(ERROR_WINHTTP_TIMEOUT);
                        }
                        return Ok(0);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Err(12004),
                }
            } else {
                match rx.recv_timeout(remaining) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => return Err(ERROR_WINHTTP_TIMEOUT),
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Err(12004),
                }
            };
            match event {
                Event::Complete { status, size } if status == expected => Ok(size),
                Event::Error(code) => Err(code),
                Event::Cancel => Err(12017),
                _ => Err(12004),
            }
        };
        let wait = |ok, expected| wait_for(ok, expected, false);
        let mut headers =
            "Content-Type: application/json\r\nAccept: text/event-stream\r\n".to_owned();
        if !api_key.trim().is_empty() {
            headers.push_str(&format!("Authorization: Bearer {}\r\n", api_key.trim()));
        }
        let headers = wide(&headers);
        let len = u32::try_from(body.len()).map_err(|_| "请求正文过大")?;
        cancel.check()?;
        trace.stage = "send";
        trace.sent_ms = Some(trace.elapsed());
        let send = unsafe {
            WinHttpSendRequest(
                req.0,
                headers.as_ptr(),
                u32::MAX,
                body_ptr.cast(),
                len,
                len,
                context,
            )
        };
        if let Err(code) = wait(send, WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE) {
            trace.connect_failed =
                replayable_send_failure(code, send_notifications.load(Ordering::Acquire));
            return Err(win_error("发送请求失败", code));
        }
        cancel.check()?;
        trace.stage = "headers";
        wait(
            unsafe { WinHttpReceiveResponse(req.0, null_mut()) },
            WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE,
        )
        .map_err(|e| win_error("等待响应失败", e))?;
        trace.headers_ms = Some(trace.elapsed());
        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        if unsafe {
            WinHttpQueryHeaders(
                req.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&raw mut status).cast(),
                &mut len,
                null_mut(),
            )
        } == 0
        {
            return Err(win_error("读取 HTTP 状态码失败", unsafe {
                GetLastError()
            }));
        }
        receive_body(
            status,
            |dst, unframed_terminal| {
                cancel.check()?;
                let available = wait_for(
                    unsafe { WinHttpQueryDataAvailable(req.0, null_mut()) },
                    WINHTTP_CALLBACK_STATUS_DATA_AVAILABLE,
                    unframed_terminal,
                )
                .map_err(|e| win_error("等待流式响应中断", e))?;
                if available == 0 {
                    return Ok(0);
                }
                cancel.check()?;
                let want = available.min(dst.len() as u32).min(BUFFER_BYTES as u32);
                let n = wait(
                    unsafe { WinHttpReadData(req.0, read_ptr.cast(), want, null_mut()) },
                    WINHTTP_CALLBACK_STATUS_READ_COMPLETE,
                )
                .map_err(|e| win_error("读取流式响应中断", e))? as usize;
                // READ_COMPLETE publishes the buffer. Only the worker initiates reads or closes the handle.
                dst[..n].copy_from_slice(unsafe { std::slice::from_raw_parts(read_ptr, n) });
                Ok(n)
            },
            trace,
            tx,
            wake,
            allow_unframed_text,
        )
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    use std::io::{self, BufRead, BufReader, Read};
    use std::net::{Shutdown, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;

    struct CancellableRead<'a> {
        socket: &'a TcpStream,
        cancel: &'a RequestCancel,
        timeout: Duration,
    }

    impl Read for CancellableRead<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let started = Instant::now();
            loop {
                if self.cancel.stopped.load(Ordering::Acquire) {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "已停止生成",
                    ));
                }
                match self.socket.read(buf) {
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                        ) && started.elapsed() < self.timeout =>
                    {
                        continue;
                    }
                    result => return result,
                }
            }
        }
    }

    pub(super) fn post(
        ep: &ParsedUrl,
        api_key: &str,
        body: &[u8],
        cancel: &RequestCancel,
        tx: &mpsc::Sender<StreamMsg>,
        wake: Option<egui::Context>,
        _route: &str,
        trace: &mut Attempt,
        allow_unframed_text: bool,
    ) -> Result<(), String> {
        if ep.https {
            return Err("当前非 Windows 测试构建仅支持明文 HTTP 回环测试".into());
        }
        let socket = TcpStream::connect((ep.host.as_str(), ep.port))
            .map(Arc::new)
            .map_err(|e| format!("连接模型服务失败：{e}"))?;
        let mut stream = socket.as_ref();
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|e| e.to_string())?;
        let socket = socket.clone();
        cancel.register(move || {
            let _ = socket.shutdown(Shutdown::Both);
        })?;

        let auth = if api_key.trim().is_empty() {
            String::new()
        } else {
            format!("Authorization: Bearer {}\r\n", api_key.trim())
        };
        let host_hdr = if ep.host.contains(':') && !ep.host.starts_with('[') {
            format!("[{}]:{}", ep.host, ep.port)
        } else {
            format!("{}:{}", ep.host, ep.port)
        };
        let headers = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAccept: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n{}\r\n",
            ep.base_path,
            host_hdr,
            body.len(),
            auth
        );
        trace.stage = "send";
        trace.sent_ms = Some(trace.elapsed());
        stream
            .write_all(headers.as_bytes())
            .and_then(|_| stream.write_all(body))
            .map_err(|e| format!("发送请求失败：{e}"))?;
        trace.stage = "headers";
        let mut reader = BufReader::new(CancellableRead {
            socket: stream,
            cancel,
            timeout: Duration::from_secs(30),
        });
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| format!("读取响应头失败：{e}"))?;
        let status = line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or("无效 HTTP 状态行")?;
        let mut remaining: Option<u64> = None;
        let mut chunked = false;
        loop {
            line.clear();
            if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("HTTP 响应头不完整".into());
            }
            if line.trim_end().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("Transfer-Encoding") {
                    chunked = value.trim().eq_ignore_ascii_case("chunked");
                } else if name.eq_ignore_ascii_case("Content-Length") {
                    remaining = Some(value.trim().parse().map_err(|_| "无效 Content-Length")?);
                }
            }
        }
        trace.headers_ms = Some(trace.elapsed());
        let mut need_delimiter = false;
        if chunked {
            remaining = Some(0)
        }
        receive_body(
            status,
            |buf, unframed_terminal| {
                cancel.check()?;
                reader.get_mut().timeout = if unframed_terminal {
                    UNFRAMED_TERMINAL_GRACE
                } else {
                    Duration::from_secs(30)
                };
                let mut read_body = || -> io::Result<usize> {
                    if chunked && remaining == Some(0) {
                        if need_delimiter {
                            let mut delimiter = [0; 2];
                            reader.read_exact(&mut delimiter)?;
                            if &delimiter != b"\r\n" {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "无效 HTTP 分块结尾",
                                ));
                            }
                        }
                        line.clear();
                        reader.read_line(&mut line)?;
                        let n =
                            u64::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
                                .map_err(|_| {
                                    io::Error::new(io::ErrorKind::InvalidData, "HTTP 分块不完整")
                                })?;
                        remaining = Some(n);
                        need_delimiter = true;
                    }
                    let want = remaining.map_or(buf.len(), |n| n.min(buf.len() as u64) as usize);
                    if want == 0 {
                        return Ok(0);
                    }
                    let n = reader.read(&mut buf[..want])?;
                    if let Some(left) = &mut remaining {
                        if n == 0 {
                            return Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "HTTP 响应在正文传输完成前中断",
                            ));
                        }
                        *left -= n as u64;
                    }
                    Ok(n)
                };
                match read_body() {
                    Err(e)
                        if unframed_terminal
                            && matches!(
                                e.kind(),
                                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                            ) =>
                    {
                        Ok(0)
                    }
                    result => result.map_err(|e| format!("读取流式响应中断：{e}")),
                }
            },
            trace,
            tx,
            wake,
            allow_unframed_text,
        )
    }
}

#[cfg(all(test, windows))]
mod route_cache_tests {
    use super::route_cache;

    #[test]
    fn host_route_cache_prefers_last_success_and_falls_back_on_invalidation() {
        let key = "route-cache-unit.internal:443";
        assert_eq!(route_cache::ordered_routes(key), ["direct", "auto_proxy"]);

        route_cache::remember(key, "auto_proxy");
        assert_eq!(route_cache::ordered_routes(key), ["auto_proxy", "direct"]);

        // Invalidating a non-matching route does not disturb cached preference.
        route_cache::invalidate(key, "direct");
        assert_eq!(route_cache::ordered_routes(key), ["auto_proxy", "direct"]);

        // When cached auto_proxy fails before connect, invalidation restores direct-first order.
        route_cache::invalidate(key, "auto_proxy");
        assert_eq!(route_cache::ordered_routes(key), ["direct", "auto_proxy"]);

        route_cache::remember(key, "direct");
        assert_eq!(route_cache::ordered_routes(key), ["direct", "auto_proxy"]);
    }
}

#[cfg(test)]
#[path = "ai_transport_tests.rs"]
mod tests;
