//! Cancellable transports for the spreadsheet assistant's SSE completion rounds.

use super::{ParsedUrl, StreamAssembler, StreamMsg, format_http_status_error};
use eframe::egui;
#[cfg(any(not(windows), not(test)))]
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const BUFFER_BYTES: usize = 8192;

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
    done: bool,
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
            done: false,
            status: None,
            stage: "connect",
            #[cfg(windows)]
            connect_failed: false,
        }
    }

    fn elapsed(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    fn log(&self, route: &str, end: &str) {
        let line = format!(
            "start_unix_ms={} route={route} sent_ms={:?} headers_ms={:?} first_byte_ms={:?} bytes={} done={} http={:?} end={end} stage={} elapsed_ms={}\n",
            self.started_unix_ms,
            self.sent_ms,
            self.headers_ms,
            self.first_byte_ms,
            self.bytes,
            self.done,
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
    mut read: impl FnMut(&mut [u8]) -> Result<usize, String>,
    trace: &mut Attempt,
    tx: &mpsc::Sender<StreamMsg>,
    wake: Option<egui::Context>,
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
        let n = match read(&mut buf) {
            Ok(n) => n,
            Err(_) if !success => break,
            Err(e) => return Err(e),
        };
        if n == 0 {
            if success {
                // A clean EOF also terminates an unterminated last "data:" line.
                trace.stage = "sse";
                trace.done = assembler.feed_bytes(b"\n", tx)?;
            }
            break;
        }
        if trace.first_byte_ms.is_none() {
            trace.first_byte_ms = Some(trace.elapsed());
        }
        trace.bytes += n;
        if success {
            trace.stage = "sse";
            if assembler.feed_bytes(&buf[..n], tx)? {
                trace.done = true;
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
        assembler.finish(tx)
    } else {
        Err(format_http_status_error(
            status,
            &String::from_utf8_lossy(&error_body),
        ))
    }
}

pub(super) fn stream_post(
    ep: &ParsedUrl,
    api_key: &str,
    body: &[u8],
    cancel: &RequestCancel,
    tx: &mpsc::Sender<StreamMsg>,
    wake: Option<egui::Context>,
) -> Result<(), String> {
    #[cfg(windows)]
    let routes = ["direct", "auto_proxy"];
    #[cfg(not(windows))]
    let routes = ["direct"];
    for route in routes {
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
        );
        cancel.interrupt.lock().unwrap().take();
        let was_cancelled = cancel.check().is_err();
        let end = match (&result, was_cancelled, trace.done, trace.stage) {
            (_, true, _, _) => "cancelled",
            (Ok(()), _, true, _) => "done",
            (Ok(()), _, false, _) => "eof_after_finish_reason",
            (Err(_), _, _, _) if trace.status.is_some_and(|s| !(200..300).contains(&s)) => "http_error",
            (Err(_), _, _, "sse") => "protocol_error",
            (Err(_), _, _, _) => "transport_error",
        };
        trace.log(route, end);
        cancel.check()?;
        // WinHttpConnect doesn't actually open a socket until SendRequest. Only
        // DNS/connect failures there can be replayed without duplicating a POST.
        #[cfg(windows)]
        if route == "direct" && trace.connect_failed {
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
            _ => Event::Complete { status, size },
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
    ) -> Result<(), String> {
        let access = if route == "direct" {
            WINHTTP_ACCESS_TYPE_NO_PROXY
        } else {
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
        };
        let session = handle(
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
            WinHttpSetTimeouts(session.0, 10_000, 15_000, 30_000, 180_000);
        }
        let conn = handle(
            unsafe { WinHttpConnect(session.0, wide(&ep.host).as_ptr(), ep.port, 0) },
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

        let (events, rx) = mpsc::channel();
        cancel.register({
            let events = events.clone();
            move || {
                let _ = events.send(Event::Cancel);
            }
        })?;
        let mut ctx = Box::new(Context {
            tx: events,
            body: body.to_vec(),
            buffer: vec![0; BUFFER_BYTES],
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
        let notifications = WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE
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
        let wait = |ok: i32, expected: u32| -> Result<u32, u32> {
            if ok == 0 {
                return Err(unsafe { GetLastError() });
            }
            match rx.recv() {
                Ok(Event::Complete { status, size }) if status == expected => Ok(size),
                Ok(Event::Error(code)) => Err(code),
                Ok(Event::Cancel) => Err(12017),
                _ => Err(12004),
            }
        };
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
            trace.connect_failed = matches!(
                code,
                ERROR_WINHTTP_NAME_NOT_RESOLVED | ERROR_WINHTTP_CANNOT_CONNECT
            );
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
            |dst| {
                cancel.check()?;
                let available = wait(
                    unsafe { WinHttpQueryDataAvailable(req.0, null_mut()) },
                    WINHTTP_CALLBACK_STATUS_DATA_AVAILABLE,
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
        )
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::net::{Shutdown, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;

    pub(super) fn post(
        ep: &ParsedUrl,
        api_key: &str,
        body: &[u8],
        cancel: &RequestCancel,
        tx: &mpsc::Sender<StreamMsg>,
        wake: Option<egui::Context>,
        _route: &str,
        trace: &mut Attempt,
    ) -> Result<(), String> {
        if ep.https {
            return Err("当前非 Windows 测试构建仅支持明文 HTTP 回环测试".into());
        }
        let socket = TcpStream::connect((ep.host.as_str(), ep.port))
            .map(Arc::new)
            .map_err(|e| format!("连接模型服务失败：{e}"))?;
        let mut stream = socket.as_ref();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
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
        let headers = format!(
            "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nAccept: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n{}\r\n",
            ep.base_path,
            ep.host,
            ep.port,
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
        let mut reader = BufReader::new(stream);
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
            |buf| {
                cancel.check()?;
                if chunked && remaining == Some(0) {
                    if need_delimiter {
                        let mut delimiter = [0; 2];
                        reader
                            .read_exact(&mut delimiter)
                            .map_err(|e| format!("HTTP 分块中断：{e}"))?;
                        if &delimiter != b"\r\n" {
                            return Err("无效 HTTP 分块结尾".into());
                        }
                    }
                    line.clear();
                    reader
                        .read_line(&mut line)
                        .map_err(|e| format!("读取 HTTP 分块失败：{e}"))?;
                    let n = u64::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
                        .map_err(|_| "HTTP 分块不完整")?;
                    remaining = Some(n);
                    need_delimiter = true;
                }
                let want = remaining.map_or(buf.len(), |n| n.min(buf.len() as u64) as usize);
                if want == 0 {
                    return Ok(0);
                }
                let n = reader
                    .read(&mut buf[..want])
                    .map_err(|e| format!("读取流式响应中断：{e}"))?;
                if let Some(left) = &mut remaining {
                    if n == 0 {
                        return Err("HTTP 响应在正文传输完成前中断".into());
                    }
                    *left -= n as u64;
                }
                Ok(n)
            },
            trace,
            tx,
            wake,
        )
    }
}

#[cfg(test)]
#[path = "ai_transport_tests.rs"]
mod tests;
