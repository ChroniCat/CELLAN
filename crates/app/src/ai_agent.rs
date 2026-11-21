//! 内置 AI 表格助手与轻量化表格微内核（Micro-Harness）。
//!
//! 预设对接企业内网 OpenAI 兼容推理服务（默认 DeepSeek Local H200），
//! 支持流式 SSE 对话、思考链折叠展示、最多 8 轮自动工具调用与自检修复，
//! 且所有写操作统一接入工作簿撤销栈、保护检查与协同编辑锁。

use crate::ui_icons::{self, Icon};
use crate::{cell_name, parse_a1_ref, range_label, ui_kit, update, SheetApp};
use eframe::egui::{self, Align2, Color32, FontId, Key, Rect, Sense, Stroke, Vec2};
use serde_json::{json, Value as JsonValue};
use sheetcore::formula::{self, FValue};
use sheetcore::model::{Cell, Value};
use sheetcore::refs;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

const DEFAULT_PROVIDER_ID: &str = "deepseek-local";
const DEFAULT_DISPLAY_NAME: &str = "DeepSeek Local H200";
const DEFAULT_BASE_URL: &str = "https://deepseek.hollantex.net/v1";
const BUILTIN_KEY_SENTINEL: &str = "@builtin";
const DEFAULT_MODEL: &str = "deepseek-v4.1-flash";
const MAX_TOOL_ROUNDS: usize = 8;

include!(concat!(env!("OUT_DIR"), "/builtin_ai_key.rs"));

/// 返回编译期由本机未入库 `.cellan-ai-key.local` / `CELLAN_AI_KEY` 注入的内置密钥；
/// 在公开 GitHub 源码构建中为空字符串 `""`（源码中绝不硬编码任何密钥）。
pub fn builtin_api_key() -> &'static str {
    static DECODED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DECODED.get_or_init(|| {
        let masked = std::hint::black_box(EMBEDDED_AI_KEY_MASKED);
        let bytes: Vec<u8> = masked
            .iter()
            .enumerate()
            .map(|(i, &b)| b ^ (0xA7u8.wrapping_add((i as u8).wrapping_mul(0x3D)) ^ 0x5C))
            .collect();
        String::from_utf8(bytes).unwrap_or_default()
    })
}

pub fn has_builtin_key() -> bool {
    !builtin_api_key().is_empty()
}

fn is_default_base_url(url: &str) -> bool {
    url.trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(DEFAULT_BASE_URL)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiConfig {
    pub provider_id: String,
    pub display_name: String,
    pub base_url: String,
    /// 模型访问密钥：公开构建默认为空；本地内网分发版默认填入编译期注入密钥。
    pub api_key: String,
    pub model: String,
    /// 用户是否已确认「表格内容会发送到上面的模型服务」。未确认前不发任何请求。
    pub data_consent: bool,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider_id: DEFAULT_PROVIDER_ID.into(),
            display_name: DEFAULT_DISPLAY_NAME.into(),
            base_url: DEFAULT_BASE_URL.into(),
            api_key: builtin_api_key().into(),
            model: DEFAULT_MODEL.into(),
            data_consent: false,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    Some(update::app_dir_opt()?.join("ai.ini"))
}

impl AiConfig {
    pub fn uses_builtin_key(&self) -> bool {
        has_builtin_key()
            && self.api_key == builtin_api_key()
            && is_default_base_url(&self.base_url)
    }

    pub fn has_api_key(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    pub fn load_from(path: &Path) -> Self {
        let mut cfg = Self::default();
        let Ok(txt) = std::fs::read_to_string(path) else {
            return cfg;
        };
        let mut saw_sentinel = false;
        for line in txt.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "provider_id" if !v.is_empty() => cfg.provider_id = v.to_string(),
                "display_name" if !v.is_empty() => cfg.display_name = v.to_string(),
                "base_url" if !v.is_empty() => cfg.base_url = v.to_string(),
                "api_key" => {
                    if v == BUILTIN_KEY_SENTINEL {
                        saw_sentinel = true;
                        cfg.api_key.clear();
                    } else {
                        saw_sentinel = false;
                        cfg.api_key = v.to_string();
                    }
                }
                "model" if !v.is_empty() => cfg.model = v.to_string(),
                "data_consent" => cfg.data_consent = v == "1",
                _ => {}
            }
        }
        // 内置密钥仅允许发往默认内网地址，绝不因配置修改而发往外部第三方站点
        if saw_sentinel {
            if has_builtin_key() && is_default_base_url(&cfg.base_url) {
                cfg.api_key = builtin_api_key().to_string();
            } else {
                cfg.api_key.clear();
            }
        } else if has_builtin_key()
            && cfg.api_key == builtin_api_key()
            && !is_default_base_url(&cfg.base_url)
        {
            cfg.api_key.clear();
        }
        cfg
    }

    pub fn load() -> Self {
        match config_path() {
            Some(p) => Self::load_from(&p),
            None => Self::default(),
        }
    }

    pub fn save_to(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let persisted_key = if self.uses_builtin_key() {
            BUILTIN_KEY_SENTINEL
        } else {
            self.api_key.trim()
        };
        let body = format!(
            "provider_id={}\ndisplay_name={}\nbase_url={}\napi_key={}\nmodel={}\ndata_consent={}\n",
            self.provider_id.trim(),
            self.display_name.trim(),
            self.base_url.trim(),
            persisted_key,
            self.model.trim(),
            if self.data_consent { "1" } else { "0" },
        );
        let _ = std::fs::write(path, body);
    }

    pub fn save(&self) {
        if let Some(p) = config_path() {
            self.save_to(&p);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiRole {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub struct AiMessage {
    pub role: AiRole,
    pub content: String,
    pub reasoning: String,
    pub reasoning_open: bool,
    pub tool_logs: Vec<String>,
    pub error: Option<String>,
    pub undo_ids: Vec<u64>,
}

#[derive(Clone, Debug)]
struct WireMessage {
    role: String,
    content: String,
}

#[derive(Clone, Debug)]
pub struct ToolCallSpec {
    pub name: String,
    pub arguments: JsonValue,
}

enum StreamMsg {
    ReasoningDelta(String),
    ContentDelta(String),
    RoundDone {
        raw_assistant: String,
        visible_content: String,
        tool_calls: Vec<ToolCallSpec>,
    },
    Failed(String),
}

pub struct AiState {
    pub open: bool,
    pub settings_open: bool,
    pub show_key: bool,
    pub cfg: AiConfig,
    pub draft_cfg: AiConfig,
    pub draft: String,
    pub messages: Vec<AiMessage>,
    pub(crate) wire_history: Vec<WireMessage>,
    pub busy: bool,
    pub round: usize,
    pub status_hint: String,
    rx: Option<mpsc::Receiver<StreamMsg>>,
    cancel: Option<Arc<AtomicBool>>,
    scroll_bottom: bool,
    pub(crate) ui_ctx: Option<egui::Context>,
    pub(crate) bound_doc_uid: Option<u64>,
    pub(crate) ime_composing: bool,
    pub(crate) request_input_focus: bool,
    /// 首次发送前等待用户确认数据外发时，暂存的那条提问
    pub(crate) pending_consent_prompt: Option<String>,
}

impl Default for AiState {
    fn default() -> Self {
        let cfg = AiConfig::load();
        Self {
            open: false,
            settings_open: false,
            show_key: false,
            draft_cfg: cfg.clone(),
            cfg,
            draft: String::new(),
            messages: Vec::new(),
            wire_history: Vec::new(),
            busy: false,
            round: 0,
            status_hint: String::new(),
            rx: None,
            cancel: None,
            scroll_bottom: false,
            ui_ctx: None,
            bound_doc_uid: None,
            ime_composing: false,
            request_input_focus: false,
            pending_consent_prompt: None,
        }
    }
}

// =========================================================================
// 原生流式 HTTP(S) 客户端（Windows 使用 winhttp.dll，免额外 TLS 依赖）
// =========================================================================

struct ParsedUrl {
    https: bool,
    host: String,
    port: u16,
    base_path: String,
}

fn parse_endpoint(base_url: &str) -> Result<ParsedUrl, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let (https, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        return Err("接口地址需以 https:// 或 http:// 开头".into());
    };
    let (host_port, path_part) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    if host_port.is_empty() {
        return Err("接口地址缺少主机名".into());
    }
    let (host, port) = if let Some((h, p)) = host_port.rsplit_once(':') {
        let parsed = p
            .parse::<u16>()
            .map_err(|_| format!("接口端口无效：{p}"))?;
        (h.to_string(), parsed)
    } else {
        (host_port.to_string(), if https { 443 } else { 80 })
    };
    // 明文 http 会把 Bearer 密钥和表格内容裸奔在网络上：只允许本机回环
    if !https && !is_loopback_host(&host) {
        return Err("出于安全考虑，非本机地址必须使用 https://（http:// 仅限 localhost / 127.0.0.1）".into());
    }
    let completions_path = if path_part.ends_with("/chat/completions") {
        path_part.to_string()
    } else if path_part.is_empty() {
        "/v1/chat/completions".to_string()
    } else {
        format!("{path_part}/chat/completions")
    };
    Ok(ParsedUrl {
        https,
        host,
        port,
        base_path: completions_path,
    })
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    h.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn format_http_status_error(status: u32, body: &str) -> String {
    let preview: String = body.trim().chars().take(180).collect();
    match status {
        401 | 403 => format!("接口鉴权失败（HTTP {status}），请检查 API Key 是否有效"),
        404 => format!("接口路径不存在（HTTP 404），请确认 Base URL 与模型 ID"),
        429 => "模型服务当前并发排队较多（HTTP 429），请稍后重试".to_string(),
        502 | 503 | 504 => format!(
            "模型网关返回 HTTP {status}（内网推理节点暂未就绪或正在重启，请确认服务状态）"
        ),
        _ if preview.is_empty() => format!("模型服务返回异常状态码 HTTP {status}"),
        _ => format!("模型服务返回 HTTP {status}：{preview}"),
    }
}

fn send_wake(tx: &mpsc::Sender<StreamMsg>, msg: StreamMsg, wake: Option<&egui::Context>) {
    let _ = tx.send(msg);
    if let Some(ctx) = wake {
        ctx.request_repaint();
    }
}

/// 增量解析 SSE 与 `<think>` / `<tool_call>` 标记，避免半截标签或换行闪烁。
struct StreamAssembler {
    line_buf: Vec<u8>,
    raw_content: String,
    raw_reasoning: String,
    emitted_content_len: usize,
    emitted_reason_len: usize,
    in_think: bool,
    in_tool: bool,
    scan_pos: usize,
    visible_clean: String,
    tool_deltas: BTreeMap<usize, (String, String)>,
    wake_ctx: Option<egui::Context>,
}

impl StreamAssembler {
    fn new(wake_ctx: Option<egui::Context>) -> Self {
        Self {
            line_buf: Vec::new(),
            raw_content: String::new(),
            raw_reasoning: String::new(),
            emitted_content_len: 0,
            emitted_reason_len: 0,
            in_think: false,
            in_tool: false,
            scan_pos: 0,
            visible_clean: String::new(),
            tool_deltas: BTreeMap::new(),
            wake_ctx,
        }
    }

    fn feed_bytes(&mut self, chunk: &[u8], tx: &mpsc::Sender<StreamMsg>) {
        self.line_buf.extend_from_slice(chunk);
        while let Some(pos) = self.line_buf.iter().position(|&b| b == b'\n') {
            let mut raw_line = self.line_buf.drain(..=pos).collect::<Vec<u8>>();
            if raw_line.last() == Some(&b'\n') {
                raw_line.pop();
            }
            if raw_line.last() == Some(&b'\r') {
                raw_line.pop();
            }
            let line = String::from_utf8_lossy(&raw_line);
            self.process_sse_line(line.trim(), tx);
        }
    }

    fn process_sse_line(&mut self, line: &str, tx: &mpsc::Sender<StreamMsg>) {
        if line.is_empty() || !line.starts_with("data:") {
            return;
        }
        let payload = line[5..].trim();
        if payload == "[DONE]" {
            return;
        }
        let Ok(v) = serde_json::from_str::<JsonValue>(payload) else {
            return;
        };
        let Some(choice) = v.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) else {
            return;
        };
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        let Some(delta) = delta else {
            return;
        };
        if let Some(rc) = delta.get("reasoning_content").and_then(|x| x.as_str()) {
            if !rc.is_empty() {
                self.raw_reasoning.push_str(rc);
                send_wake(tx, StreamMsg::ReasoningDelta(rc.to_string()), self.wake_ctx.as_ref());
                self.emitted_reason_len = self.raw_reasoning.len();
            }
        }
        if let Some(c) = delta.get("content").and_then(|x| x.as_str()) {
            if !c.is_empty() {
                self.raw_content.push_str(c);
                self.flush_content_tags(false, tx);
            }
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for tc in tcs {
                let idx = tc.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                let entry = self.tool_deltas.entry(idx).or_default();
                if let Some(f) = tc.get("function") {
                    if let Some(n) = f.get("name").and_then(|x| x.as_str()) {
                        entry.0.push_str(n);
                    }
                    if let Some(a) = f.get("arguments").and_then(|x| x.as_str()) {
                        entry.1.push_str(a);
                    }
                }
            }
        }
    }

    fn flush_content_tags(&mut self, final_flush: bool, tx: &mpsc::Sender<StreamMsg>) {
        let wake = self.wake_ctx.as_ref();
        loop {
            let rest = &self.raw_content[self.scan_pos..];
            if rest.is_empty() {
                break;
            }
            if self.in_think {
                if let Some(end_idx) = rest.find("</think>") {
                    let piece = &rest[..end_idx];
                    if !piece.is_empty() {
                        self.raw_reasoning.push_str(piece);
                        send_wake(tx, StreamMsg::ReasoningDelta(piece.to_string()), wake);
                    }
                    self.scan_pos += end_idx + "</think>".len();
                    self.in_think = false;
                } else {
                    let safe = if final_flush {
                        rest.len()
                    } else {
                        safe_prefix_before_partial_tag(rest, "</think>")
                    };
                    if safe > 0 {
                        let piece = &rest[..safe];
                        self.raw_reasoning.push_str(piece);
                        send_wake(tx, StreamMsg::ReasoningDelta(piece.to_string()), wake);
                        self.scan_pos += safe;
                    }
                    break;
                }
            } else if self.in_tool {
                if let Some(end_idx) = rest.find("</tool_call>") {
                    self.scan_pos += end_idx + "</tool_call>".len();
                    self.in_tool = false;
                } else {
                    if final_flush {
                        self.scan_pos = self.raw_content.len();
                    }
                    break;
                }
            } else {
                let think_pos = rest.find("<think>");
                let tool_pos = rest.find("<tool_call>");
                match (think_pos, tool_pos) {
                    (Some(ti), Some(tl)) if ti <= tl => {
                        let piece = &rest[..ti];
                        push_visible(&mut self.visible_clean, &mut self.emitted_content_len, piece, tx, wake);
                        self.scan_pos += ti + "<think>".len();
                        self.in_think = true;
                    }
                    (_, Some(tl)) => {
                        let piece = &rest[..tl];
                        push_visible(&mut self.visible_clean, &mut self.emitted_content_len, piece, tx, wake);
                        self.scan_pos += tl + "<tool_call>".len();
                        self.in_tool = true;
                    }
                    (Some(ti), None) => {
                        let piece = &rest[..ti];
                        push_visible(&mut self.visible_clean, &mut self.emitted_content_len, piece, tx, wake);
                        self.scan_pos += ti + "<think>".len();
                        self.in_think = true;
                    }
                    (None, None) => {
                        let safe = if final_flush {
                            rest.len()
                        } else {
                            let s1 = safe_prefix_before_partial_tag(rest, "<think>");
                            let s2 = safe_prefix_before_partial_tag(rest, "<tool_call>");
                            s1.min(s2)
                        };
                        if safe > 0 {
                            let piece = &rest[..safe];
                            push_visible(&mut self.visible_clean, &mut self.emitted_content_len, piece, tx, wake);
                            self.scan_pos += safe;
                        }
                        break;
                    }
                }
            }
        }
        let _ = self.emitted_content_len;
        let _ = self.emitted_reason_len;
    }

    fn finish(mut self, tx: &mpsc::Sender<StreamMsg>) {
        if !self.line_buf.is_empty() {
            let line = String::from_utf8_lossy(&self.line_buf).trim().to_string();
            self.line_buf.clear();
            self.process_sse_line(&line, tx);
        }
        self.flush_content_tags(true, tx);
        let mut tool_calls = parse_tool_calls_from_text(&self.raw_content);
        for (_, (name, args_raw)) in self.tool_deltas {
            if !name.is_empty() {
                let args = serde_json::from_str::<JsonValue>(&args_raw).unwrap_or_else(|_| json!({}));
                tool_calls.push(ToolCallSpec { name, arguments: args });
            }
        }
        send_wake(
            tx,
            StreamMsg::RoundDone {
                raw_assistant: self.raw_content,
                visible_content: self.visible_clean.trim().to_string(),
                tool_calls,
            },
            self.wake_ctx.as_ref(),
        );
    }
}

fn push_visible(
    visible_clean: &mut String,
    emitted_len: &mut usize,
    piece: &str,
    tx: &mpsc::Sender<StreamMsg>,
    wake: Option<&egui::Context>,
) {
    if piece.is_empty() {
        return;
    }
    visible_clean.push_str(piece);
    send_wake(tx, StreamMsg::ContentDelta(piece.to_string()), wake);
    *emitted_len = visible_clean.len();
}

fn safe_prefix_before_partial_tag(s: &str, tag: &str) -> usize {
    if let Some(lt) = s.rfind('<') {
        let tail = &s[lt..];
        if tag.starts_with(tail) {
            return lt;
        }
    }
    s.len()
}

pub fn parse_tool_calls_from_text(text: &str) -> Vec<ToolCallSpec> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<tool_call>") {
        let after = &rest[start + "<tool_call>".len()..];
        let Some(end) = after.find("</tool_call>") else {
            break;
        };
        let mut block = after[..end].trim();
        if let Some(stripped) = block.strip_prefix("```json") {
            block = stripped.trim();
        } else if let Some(stripped) = block.strip_prefix("```") {
            block = stripped.trim();
        }
        if let Some(stripped) = block.strip_suffix("```") {
            block = stripped.trim();
        }
        if let Ok(v) = serde_json::from_str::<JsonValue>(block) {
            let name = v
                .get("name")
                .or_else(|| v.get("tool"))
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let arguments = v
                .get("arguments")
                .or_else(|| v.get("args"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !name.is_empty() {
                out.push(ToolCallSpec { name, arguments });
            }
        }
        rest = &after[end + "</tool_call>".len()..];
    }
    out
}

#[cfg(windows)]
mod winhttp_client {
    use super::*;
    use core::ffi::c_void;

    type HINTERNET = *mut c_void;
    const WINHTTP_ACCESS_TYPE_NO_PROXY: u32 = 1;
    const WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
    const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
    const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
    const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;
    const WINHTTP_ADDREQ_FLAG_ADD: u32 = 0x2000_0000;

    #[link(name = "winhttp")]
    unsafe extern "system" {
        fn WinHttpOpen(
            agent: *const u16,
            access_type: u32,
            proxy_name: *const u16,
            proxy_bypass: *const u16,
            flags: u32,
        ) -> HINTERNET;
        fn WinHttpSetTimeouts(
            handle: HINTERNET,
            resolve_timeout: i32,
            connect_timeout: i32,
            send_timeout: i32,
            receive_timeout: i32,
        ) -> i32;
        fn WinHttpConnect(
            session: HINTERNET,
            server_name: *const u16,
            server_port: u16,
            reserved: u32,
        ) -> HINTERNET;
        fn WinHttpOpenRequest(
            connect: HINTERNET,
            verb: *const u16,
            object_name: *const u16,
            version: *const u16,
            referrer: *const u16,
            accept_types: *const *const u16,
            flags: u32,
        ) -> HINTERNET;
        fn WinHttpAddRequestHeaders(
            request: HINTERNET,
            headers: *const u16,
            headers_length: u32,
            modifiers: u32,
        ) -> i32;
        fn WinHttpSendRequest(
            request: HINTERNET,
            headers: *const u16,
            headers_length: u32,
            optional: *const c_void,
            optional_length: u32,
            total_length: u32,
            context: usize,
        ) -> i32;
        fn WinHttpReceiveResponse(request: HINTERNET, reserved: *mut c_void) -> i32;
        fn WinHttpQueryHeaders(
            request: HINTERNET,
            info_level: u32,
            name: *const u16,
            buffer: *mut c_void,
            buffer_length: *mut u32,
            index: *mut u32,
        ) -> i32;
        fn WinHttpReadData(
            request: HINTERNET,
            buffer: *mut c_void,
            number_of_bytes_to_read: u32,
            number_of_bytes_read: *mut u32,
        ) -> i32;
        fn WinHttpCloseHandle(internet: HINTERNET) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLastError() -> u32;
    }

    struct HandleGuard(HINTERNET);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    WinHttpCloseHandle(self.0);
                }
            }
        }
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn format_win32_error(stage: &str, code: u32) -> String {
        let hint = match code {
            12002 => "请求超时",
            12007 => "无法解析服务器域名，请检查内网 DNS",
            12029 => "无法连接到服务器端口",
            12175 => "TLS 安全连接或证书校验失败；若内网使用自签证书，请确认本机已安装企业根证书",
            _ => "网络通信失败",
        };
        format!("{stage}（{hint}，错误码 {code}）")
    }

    pub fn stream_post(
        ep: &ParsedUrl,
        api_key: &str,
        body: &[u8],
        cancel: &AtomicBool,
        tx: &mpsc::Sender<StreamMsg>,
        wake_ctx: Option<egui::Context>,
    ) -> Result<(), String> {
        match stream_post_once(
            ep,
            api_key,
            body,
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            cancel,
            tx,
            wake_ctx.clone(),
        ) {
            Ok(()) => Ok(()),
            Err(first_err) => {
                if cancel.load(Ordering::Relaxed) || first_err.contains("HTTP ") {
                    return Err(first_err);
                }
                stream_post_once(
                    ep,
                    api_key,
                    body,
                    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                    cancel,
                    tx,
                    wake_ctx,
                )
            }
        }
    }

    fn stream_post_once(
        ep: &ParsedUrl,
        api_key: &str,
        body: &[u8],
        access_type: u32,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<StreamMsg>,
        wake_ctx: Option<egui::Context>,
    ) -> Result<(), String> {
        let agent_w = to_wide("CELLAN/1.70");
        let session = unsafe {
            WinHttpOpen(
                agent_w.as_ptr(),
                access_type,
                core::ptr::null(),
                core::ptr::null(),
                0,
            )
        };
        if session.is_null() {
            return Err(format_win32_error("初始化 HTTP 会话失败", unsafe { GetLastError() }));
        }
        let _sg = HandleGuard(session);
        unsafe {
            // 552B 模型深思考与多人排队期间接收首包可能较慢，接收超时放宽至 180 秒
            WinHttpSetTimeouts(session, 10_000, 15_000, 30_000, 180_000);
        }

        let host_w = to_wide(&ep.host);
        let conn = unsafe { WinHttpConnect(session, host_w.as_ptr(), ep.port, 0) };
        if conn.is_null() {
            return Err(format_win32_error("连接模型服务器失败", unsafe { GetLastError() }));
        }
        let _cg = HandleGuard(conn);

        let verb_w = to_wide("POST");
        let path_w = to_wide(&ep.base_path);
        let flags = if ep.https { WINHTTP_FLAG_SECURE } else { 0 };
        let req = unsafe {
            WinHttpOpenRequest(
                conn,
                verb_w.as_ptr(),
                path_w.as_ptr(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                flags,
            )
        };
        if req.is_null() {
            return Err(format_win32_error("构造 HTTP 请求失败", unsafe { GetLastError() }));
        }
        let _rg = HandleGuard(req);

        let mut hdrs = String::from("Content-Type: application/json\r\nAccept: text/event-stream\r\n");
        if !api_key.trim().is_empty() {
            hdrs.push_str(&format!("Authorization: Bearer {}\r\n", api_key.trim()));
        }
        let hdrs_w = to_wide(&hdrs);
        unsafe {
            WinHttpAddRequestHeaders(req, hdrs_w.as_ptr(), u32::MAX, WINHTTP_ADDREQ_FLAG_ADD);
        }

        let ok = unsafe {
            WinHttpSendRequest(
                req,
                core::ptr::null(),
                0,
                body.as_ptr() as *const c_void,
                body.len() as u32,
                body.len() as u32,
                0,
            )
        };
        if ok == 0 {
            return Err(format_win32_error("发送请求失败", unsafe { GetLastError() }));
        }
        if unsafe { WinHttpReceiveResponse(req, core::ptr::null_mut()) } == 0 {
            return Err(format_win32_error("等待响应超时或失败", unsafe { GetLastError() }));
        }

        let mut status: u32 = 0;
        let mut status_len: u32 = core::mem::size_of::<u32>() as u32;
        let mut idx: u32 = 0;
        unsafe {
            WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                core::ptr::null(),
                &mut status as *mut u32 as *mut c_void,
                &mut status_len,
                &mut idx,
            );
        }

        if status < 200 || status >= 300 {
            let mut err_bytes = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                let mut read: u32 = 0;
                let r = unsafe {
                    WinHttpReadData(req, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read)
                };
                if r == 0 || read == 0 || err_bytes.len() >= 1024 {
                    break;
                }
                err_bytes.extend_from_slice(&buf[..read as usize]);
            }
            let text = String::from_utf8_lossy(&err_bytes);
            return Err(format_http_status_error(status, &text));
        }

        let mut asm = StreamAssembler::new(wake_ctx);
        let mut buf = [0u8; 4096];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("已停止生成".into());
            }
            let mut read: u32 = 0;
            let r = unsafe {
                WinHttpReadData(req, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read)
            };
            if r == 0 {
                return Err(format_win32_error("读取流式响应中断", unsafe { GetLastError() }));
            }
            if read == 0 {
                break;
            }
            asm.feed_bytes(&buf[..read as usize], tx);
        }
        asm.finish(tx);
        Ok(())
    }
}

#[cfg(not(windows))]
mod posix_http_client {
    use super::*;
    use std::io::Read;
    use std::net::TcpStream;
    use std::time::Duration;

    pub fn stream_post(
        ep: &ParsedUrl,
        api_key: &str,
        body: &[u8],
        cancel: &AtomicBool,
        tx: &mpsc::Sender<StreamMsg>,
        wake_ctx: Option<egui::Context>,
    ) -> Result<(), String> {
        if ep.https {
            return Err("当前非 Windows 测试构建仅支持明文 HTTP 回环测试".into());
        }
        let mut stream = TcpStream::connect((ep.host.as_str(), ep.port))
            .map_err(|e| format!("连接模型服务失败：{e}"))?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let auth = if api_key.trim().is_empty() {
            String::new()
        } else {
            format!("Authorization: Bearer {}\r\n", api_key.trim())
        };
        let req = format!(
            "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nAccept: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n{}\r\n",
            ep.base_path,
            ep.host,
            ep.port,
            body.len(),
            auth
        );
        stream
            .write_all(req.as_bytes())
            .and_then(|_| stream.write_all(body))
            .map_err(|e| format!("发送请求失败：{e}"))?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| format!("读取响应头失败：{e}"))?;
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        loop {
            let mut hline = String::new();
            if reader.read_line(&mut hline).unwrap_or(0) == 0 || hline == "\r\n" || hline == "\n" {
                break;
            }
        }
        if status < 200 || status >= 300 {
            let mut err = String::new();
            let _ = reader.read_to_string(&mut err);
            return Err(format_http_status_error(status, &err));
        }

        let mut asm = StreamAssembler::new(wake_ctx);
        let mut buf = [0u8; 4096];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("已停止生成".into());
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => asm.feed_bytes(&buf[..n], tx),
                Err(e) => return Err(format!("读取流式响应中断：{e}")),
            }
        }
        asm.finish(tx);
        Ok(())
    }
}

fn spawn_completion_round(
    cfg: AiConfig,
    wire: Vec<WireMessage>,
    cancel: Arc<AtomicBool>,
    wake_ctx: Option<egui::Context>,
) -> mpsc::Receiver<StreamMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let ep = match parse_endpoint(&cfg.base_url) {
            Ok(ep) => ep,
            Err(e) => {
                send_wake(&tx, StreamMsg::Failed(e), wake_ctx.as_ref());
                return;
            }
        };
        let msgs: Vec<JsonValue> = wire
            .iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();
        let req_json = json!({
            "model": cfg.model.trim(),
            "messages": msgs,
            "stream": true,
            "temperature": 0.2
        });
        let body = req_json.to_string();
        #[cfg(windows)]
        let res = winhttp_client::stream_post(
            &ep,
            &cfg.api_key,
            body.as_bytes(),
            &cancel,
            &tx,
            wake_ctx.clone(),
        );
        #[cfg(not(windows))]
        let res = posix_http_client::stream_post(
            &ep,
            &cfg.api_key,
            body.as_bytes(),
            &cancel,
            &tx,
            wake_ctx.clone(),
        );
        if let Err(e) = res {
            send_wake(&tx, StreamMsg::Failed(e), wake_ctx.as_ref());
        }
    });
    rx
}

// =========================================================================
// 表格微内核 Harness：上下文采样与 8 个内置表格工具
// =========================================================================

fn system_prompt() -> &'static str {
    concat!(
        "你是集成在 CELLAN 桌面电子表格中的 AI 表格助手。\n",
        "规则：\n",
        "1. 当需要读取更多单元格、修改表格、写入公式、设置样式、整理数据或新建工作表时，使用工具调用：\n",
        "   每个工具调用单独包裹在 <tool_call>...</tool_call> 中，内部为严格 JSON：\n",
        "   <tool_call>{\"name\":\"工具名\",\"arguments\":{...}}</tool_call>\n",
        "2. 计算类需求优先使用标准 Excel 公式（如 SUMIFS、COUNTIFS、XLOOKUP、IF、AVERAGEIF 等），避免靠心算写死结果。\n",
        "3. 写入公式后若返回了计算结果或错误提示，请核对是否出现 #VALUE! / #NAME? / #REF! / #N/A / #DIV/0! 并主动修复。\n",
        "4. 一旦完成操作，用简明中文向用户总结修改内容与计算结果；不要向用户展示原始 JSON 工具标签。\n",
        "\n可用工具清单：\n",
        "- inspect_sheet: {\"sheet\"?: string, \"range\"?: \"A1:H20\"} 读取指定工作表与区域内容（值、公式、数字格式）。\n",
        "- search_cells: {\"query\": string, \"sheet\"?: string} 搜索包含指定文本或公式的单元格。\n",
        "- eval_preview: {\"formula\": \"=SUM(B2:B10)\"} 不修改表格直接试算公式结果。\n",
        "- set_cells: {\"sheet\"?: string, \"edits\": [{\"cell\": \"C2\", \"value\"?: 123|\"文本\"|true|null, \"formula\"?: \"=A2*B2\", \"format\"?: \"#,##0.00\"}]} 批量写入数值、文本、公式或格式。\n",
        "- fill_formula_range: {\"sheet\"?: string, \"range\": \"E2:E50\", \"top_left_formula\": \"=C2*D2\", \"format\"?: \"#,##0.00\"} 以左上角公式为基准相对填充整片区域。\n",
        "- style_range: {\"sheet\"?: string, \"range\": \"A1:F1\", \"bold\"?: bool, \"italic\"?: bool, \"fill_hex\"?: \"#FFF2CC\", \"font_hex\"?: \"#C00000\", \"halign\"?: \"left\"|\"center\"|\"right\", \"format\"?: string, \"borders\"?: \"all\"|\"outer\"|\"none\"} 设置区域样式。\n",
        "- manage_sheet: {\"op\": \"create\"|\"rename\"|\"switch\", \"name\": string, \"new_name\"?: string} 新建、重命名或切换工作表。\n",
        "- data_operation: {\"op\": \"sort_asc\"|\"sort_desc\"|\"highlight_duplicates\"|\"deduplicate\"|\"merge_same\"|\"clear_contents\", \"sheet\"?: string, \"range\": \"A1:D50\"} 对区域执行排序、高亮重复、去重、合并相同项或清空内容。\n"
    )
}

fn build_workbook_context(app: &SheetApp) -> String {
    let mut out = String::new();
    let (r1, c1, r2, c2) = app.sel_rect();
    let active_sh = app.wb.sheet(app.sheet_idx);
    out.push_str(&format!("当前工作簿：{}\n", app.wb.title()));
    out.push_str("工作表列表：\n");
    for (idx, sh) in app.wb.sheets.iter().enumerate() {
        let bounds = sh
            .used_bounds()
            .map(|(a, b, c, d)| range_label((a, b), (c, d)))
            .unwrap_or_else(|| "空表".into());
        let mark = if idx == app.sheet_idx { "（当前活动）" } else { "" };
        out.push_str(&format!("- {}{}: 已用范围 {}\n", sh.name, mark, bounds));
    }
    out.push_str(&format!(
        "\n当前活动工作表：{}，当前选区：{}\n",
        active_sh.name,
        range_label((r1, c1), (r2, c2))
    ));

    // 选区内容摘要（最多 32 格）
    if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 <= 256 {
        let mut sel_items = Vec::new();
        for rr in r1..=r2 {
            for cc in c1..=c2 {
                if let Some(cell) = active_sh.get(rr, cc) {
                    if !cell.is_blank() {
                        let st = app.wb.styles.get(active_sh.style_of(rr, cc));
                        let disp = sheetcore::style::format_value_in(
                            app.wb.date_system,
                            &cell.value,
                            &st.format,
                        );
                        let desc = match &cell.formula {
                            Some(f) => format!("{}=[={f} -> {disp}]", cell_name(rr, cc)),
                            None => format!("{}={disp}", cell_name(rr, cc)),
                        };
                        sel_items.push(desc);
                        if sel_items.len() >= 32 {
                            break;
                        }
                    }
                }
            }
            if sel_items.len() >= 32 {
                break;
            }
        }
        if !sel_items.is_empty() {
            out.push_str("当前选区内容：");
            out.push_str(&sel_items.join(", "));
            out.push('\n');
        }
    }

    // 活动表表头与前 6 行预览
    if let Some((ur1, uc1, ur2, uc2)) = active_sh.used_bounds() {
        let row_end = ur2.min(ur1 + 5);
        let col_end = uc2.min(uc1 + 11);
        out.push_str(&format!(
            "当前工作表前部样本（{}）：\n",
            range_label((ur1, uc1), (row_end, col_end))
        ));
        for rr in ur1..=row_end {
            let mut cols = Vec::new();
            for cc in uc1..=col_end {
                if let Some(cell) = active_sh.get(rr, cc) {
                    if !cell.is_blank() {
                        let st = app.wb.styles.get(active_sh.style_of(rr, cc));
                        let disp = sheetcore::style::format_value_in(
                            app.wb.date_system,
                            &cell.value,
                            &st.format,
                        );
                        cols.push(match &cell.formula {
                            Some(f) => format!("{}:{} [={f}]", cell_name(rr, cc), disp),
                            None => format!("{}:{}", cell_name(rr, cc), disp),
                        });
                    }
                }
            }
            if !cols.is_empty() {
                out.push_str(&format!("  第 {} 行: {}\n", rr + 1, cols.join(" | ")));
            }
        }
        // 扫描前部公式错误
        let mut errs = Vec::new();
        for (cr, cell) in active_sh.iter() {
            if let Value::Error(e) = cell.value {
                errs.push(format!("{}={}", cell_name(cr.row, cr.col), e.text()));
                if errs.len() >= 12 {
                    break;
                }
            }
        }
        if !errs.is_empty() {
            out.push_str(&format!("当前工作表检测到公式错误：{}\n", errs.join(", ")));
        }
    }
    out
}

fn parse_hex_rgb(s: &str) -> Option<[u8; 3]> {
    let hex = s.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r, g, b])
}

fn json_to_cell_value(v: &JsonValue) -> Value {
    match v {
        JsonValue::Null => Value::Empty,
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        JsonValue::String(s) => Value::parse_input(s),
        other => Value::Text(other.to_string().into()),
    }
}

impl SheetApp {
    fn resolve_sheet_index(&self, name: Option<&str>) -> Option<usize> {
        match name.map(str::trim).filter(|s| !s.is_empty()) {
            None => Some(self.sheet_idx),
            Some(n) => self
                .wb
                .sheets
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(n)),
        }
    }

    pub(crate) fn start_ai_prompt(&mut self, user_text: String) {
        let prompt = user_text.trim().to_string();
        if prompt.is_empty() || self.ai.busy {
            return;
        }
        if !self.finish_pending_edit() {
            self.ai.status_hint = self.status.clone();
            return;
        }
        if !self.ai.cfg.has_api_key() {
            self.ai.status_hint = "请先在「设置」里填写模型服务的 API Key".into();
            self.ai.settings_open = true;
            self.ai.draft_cfg = self.ai.cfg.clone();
            return;
        }
        if !self.ai.cfg.data_consent {
            // 第一次发送前必须由用户确认数据外发；确认后用同一条提问继续
            self.ai.pending_consent_prompt = Some(prompt);
            self.ai.status_hint = "发送前请先确认数据外发说明".into();
            return;
        }
        if self.ai.bound_doc_uid.is_some() && self.ai.bound_doc_uid != Some(self.doc_uid) {
            self.ai.wire_history.clear();
        }
        self.ai.bound_doc_uid = Some(self.doc_uid);
        if self.ai.wire_history.is_empty() {
            self.ai.wire_history.push(WireMessage {
                role: "system".into(),
                content: system_prompt().into(),
            });
        }
        let ctx_snap = build_workbook_context(self);
        self.ai.wire_history.push(WireMessage {
            role: "user".into(),
            content: format!("[当前表格上下文]\n{ctx_snap}\n[用户需求]\n{prompt}"),
        });
        self.ai.messages.push(AiMessage {
            role: AiRole::User,
            content: prompt,
            reasoning: String::new(),
            reasoning_open: false,
            tool_logs: Vec::new(),
            error: None,
            undo_ids: Vec::new(),
        });
        self.ai.messages.push(AiMessage {
            role: AiRole::Assistant,
            content: String::new(),
            reasoning: String::new(),
            reasoning_open: false,
            tool_logs: Vec::new(),
            error: None,
            undo_ids: Vec::new(),
        });
        self.ai.round = 1;
        self.ai.status_hint = format!("{} 正在分析请求…", self.ai.cfg.display_name);
        self.ai.scroll_bottom = true;
        self.launch_ai_round();
    }

    fn launch_ai_round(&mut self) {
        let cancel = Arc::new(AtomicBool::new(false));
        let rx = spawn_completion_round(
            self.ai.cfg.clone(),
            self.ai.wire_history.clone(),
            cancel.clone(),
            self.ai.ui_ctx.clone(),
        );
        self.ai.busy = true;
        self.ai.cancel = Some(cancel);
        self.ai.rx = Some(rx);
    }

    pub(crate) fn stop_ai_generation(&mut self) {
        if let Some(c) = &self.ai.cancel {
            c.store(true, Ordering::Relaxed);
        }
        self.ai.busy = false;
        self.ai.rx = None;
        self.ai.status_hint = "已停止生成".into();
    }

    pub(crate) fn poll_ai(&mut self, ctx: &egui::Context) {
        self.ai.ui_ctx = Some(ctx.clone());
        let mut finished: Option<(String, String, Vec<ToolCallSpec>)> = None;
        let mut failed: Option<String> = None;
        let mut progressed = false;

        if let Some(rx) = &self.ai.rx {
            while let Ok(ev) = rx.try_recv() {
                progressed = true;
                match ev {
                    StreamMsg::ReasoningDelta(d) => {
                        if let Some(last) = self.ai.messages.last_mut() {
                            last.reasoning.push_str(&d);
                        }
                    }
                    StreamMsg::ContentDelta(d) => {
                        if let Some(last) = self.ai.messages.last_mut() {
                            last.content.push_str(&d);
                        }
                    }
                    StreamMsg::RoundDone {
                        raw_assistant,
                        visible_content,
                        tool_calls,
                    } => {
                        finished = Some((raw_assistant, visible_content, tool_calls));
                    }
                    StreamMsg::Failed(err) => {
                        failed = Some(err);
                    }
                }
            }
        }
        if progressed {
            self.ai.scroll_bottom = true;
            ctx.request_repaint();
        }
        if let Some(err) = failed {
            self.ai.busy = false;
            self.ai.rx = None;
            self.ai.cancel = None;
            self.ai.status_hint = err.clone();
            if let Some(last) = self.ai.messages.last_mut() {
                last.error = Some(err);
            }
            ctx.request_repaint();
            return;
        }
        if let Some((raw_assistant, visible_content, tool_calls)) = finished {
            self.ai.rx = None;
            self.ai.cancel = None;
            self.ai.wire_history.push(WireMessage {
                role: "assistant".into(),
                content: raw_assistant,
            });
            if tool_calls.is_empty() || self.ai.round >= MAX_TOOL_ROUNDS {
                self.ai.busy = false;
                if let Some(last) = self.ai.messages.last_mut() {
                    if !visible_content.is_empty() {
                        last.content = visible_content;
                    } else if last.content.trim().is_empty() && !last.tool_logs.is_empty() {
                        last.content = "已完成请求的处理并更新工作表。".into();
                    }
                }
                self.ai.status_hint = "已完成".into();
                ctx.request_repaint();
                return;
            }
            if self.ai.bound_doc_uid != Some(self.doc_uid) {
                self.ai.busy = false;
                let msg = "当前活动工作簿已切换，已中止本轮自动修改以防误改其他表格；切回原标签页后可点「重试」继续。".to_string();
                self.ai.status_hint = msg.clone();
                if let Some(last) = self.ai.messages.last_mut() {
                    last.error = Some(msg);
                }
                ctx.request_repaint();
                return;
            }

            let mut observations = Vec::new();
            for tc in tool_calls {
                let (log_label, result_json) = self.execute_ai_tool(&tc, ctx);
                if let Some(last) = self.ai.messages.last_mut() {
                    last.tool_logs.push(log_label);
                }
                observations.push(json!({
                    "tool": tc.name,
                    "result": result_json
                }));
            }
            self.ai.round += 1;
            self.ai.status_hint = format!("第 {} 轮推理中…", self.ai.round);
            let obs_str = serde_json::to_string_pretty(&observations).unwrap_or_default();
            self.ai.wire_history.push(WireMessage {
                role: "user".into(),
                content: format!("[工具执行返回]\n{obs_str}\n请根据结果继续完成用户需求；若已全部完成，请直接用中文向用户汇总。"),
            });
            self.launch_ai_round();
            ctx.request_repaint();
        }
    }

    fn execute_ai_tool(&mut self, tc: &ToolCallSpec, ctx: &egui::Context) -> (String, JsonValue) {
        let checkpoint = self.undo.checkpoint();
        let (label, res) = match tc.name.as_str() {
            "inspect_sheet" => self.tool_inspect_sheet(&tc.arguments),
            "search_cells" => self.tool_search_cells(&tc.arguments),
            "eval_preview" => self.tool_eval_preview(&tc.arguments),
            "set_cells" => self.tool_set_cells(&tc.arguments),
            "fill_formula_range" => self.tool_fill_formula_range(&tc.arguments),
            "style_range" => self.tool_style_range(&tc.arguments),
            "manage_sheet" => self.tool_manage_sheet(&tc.arguments),
            "data_operation" => self.tool_data_operation(&tc.arguments, ctx),
            other => (
                format!("未知工具：{other}"),
                json!({"ok": false, "error": format!("不支持的工具名称: {other}")}),
            ),
        };
        if let Some(last) = self.ai.messages.last_mut() {
            if res.get("undo_history_reset").and_then(JsonValue::as_bool) == Some(true) {
                last.undo_ids.clear();
            }
            last.undo_ids.extend(self.undo.ids_since(checkpoint));
        }
        (label, res)
    }

    pub(crate) fn undo_last_ai_change(&mut self) {
        if self.ai.busy || self.ai.bound_doc_uid != Some(self.doc_uid) {
            self.ai.status_hint = "请回到原工作簿并等待 AI 操作完成".into();
            return;
        }
        if !self.finish_pending_edit() {
            self.ai.status_hint = self.status.clone();
            return;
        }
        let Some(last) = self.ai.messages.last() else { return };
        if !self.undo.prepare_group_undo(&last.undo_ids) {
            self.ai.status_hint = "无法单独撤销本轮修改：历史已变更，或后续编辑与其重叠".into();
            return;
        }
        while let Some(id) = self.ai.messages.last().and_then(|m| m.undo_ids.last().copied()) {
            if self.undo.next_undo_id() != Some(id) {
                self.ai.status_hint = "撤销历史已变更".into();
                return;
            }
            let complete = self.apply_undo(false);
            if self.undo.next_undo_id() == Some(id) {
                self.ai.status_hint = self.status.clone();
                return;
            }
            self.ai.messages.last_mut().unwrap().undo_ids.pop();
            if !complete {
                self.ai.status_hint = self.status.clone();
                return;
            }
        }
        self.ai.status_hint = "已撤销本轮记录的表格编辑".into();
    }

    fn tool_inspect_sheet(&self, args: &JsonValue) -> (String, JsonValue) {
        let Some(si) = self.resolve_sheet_index(args.get("sheet").and_then(|v| v.as_str())) else {
            return (
                "读取工作表失败".into(),
                json!({"ok": false, "error": "未找到指定工作表"}),
            );
        };
        let sh = self.wb.sheet(si);
        let bounds = args
            .get("range")
            .and_then(|v| v.as_str())
            .and_then(Self::parse_rect)
            .or_else(|| sh.used_bounds())
            .unwrap_or((0, 0, 9, 7));
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

    fn tool_search_cells(&self, args: &JsonValue) -> (String, JsonValue) {
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

    fn tool_eval_preview(&self, args: &JsonValue) -> (String, JsonValue) {
        let f = args.get("formula").and_then(|v| v.as_str()).unwrap_or("").trim();
        if f.is_empty() {
            return ("公式试算失败".into(), json!({"ok": false, "error": "formula 不能为空"}));
        }
        let si = self
            .resolve_sheet_index(args.get("sheet").and_then(|v| v.as_str()))
            .unwrap_or(self.sheet_idx);
        let fv = formula::eval_formula(&self.wb, si, f);
        (
            format!("试算 {f} = {}", fv.display()),
            json!({"ok": !matches!(fv, FValue::Error(_)), "result": fv.display()}),
        )
    }

    fn tool_set_cells(&mut self, args: &JsonValue) -> (String, JsonValue) {
        let Some(si) = self.resolve_sheet_index(args.get("sheet").and_then(|v| v.as_str())) else {
            return ("写入失败".into(), json!({"ok": false, "error": "未找到指定工作表"}));
        };
        let Some(edits) = args.get("edits").and_then(|v| v.as_array()) else {
            return ("写入失败".into(), json!({"ok": false, "error": "缺少 edits 数组"}));
        };
        if edits.is_empty() || edits.len() > 2000 {
            return (
                "写入失败".into(),
                json!({"ok": false, "error": "单次 edits 数量需在 1..=2000 之间"}),
            );
        }
        struct ParsedEdit {
            r: u32,
            c: u32,
            val: Option<Value>,
            formula: Option<String>,
            format: Option<String>,
        }
        let mut parsed = Vec::with_capacity(edits.len());
        let mut keys = Vec::with_capacity(edits.len());
        for item in edits {
            let Some(addr) = item.get("cell").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some((r, c)) = parse_a1_ref(addr.trim()) else {
                continue;
            };
            let formula = item
                .get("formula")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().strip_prefix('=').unwrap_or(s.trim()).to_string())
                .filter(|s| !s.is_empty());
            let val = item.get("value").map(json_to_cell_value);
            let format = item
                .get("format")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            keys.push((r, c));
            parsed.push(ParsedEdit { r, c, val, formula, format });
        }
        if keys.is_empty() {
            return ("写入失败".into(), json!({"ok": false, "error": "没有有效的单元格坐标"}));
        }
        if !self.write_preflight_on(si, &keys, "AI 写入单元格") {
            return ("写入被保护/锁定拒绝".into(), json!({"ok": false, "error": self.status}));
        }

        let before = sheetcore::undo::capture(self.wb.sheet(si), si, &keys);
        for ed in &parsed {
            if let Some(f) = &ed.formula {
                self.wb.sheet_mut(si).set(
                    ed.r,
                    ed.c,
                    Cell {
                        value: Value::Empty,
                        formula: Some(f.clone().into_boxed_str()),
                    },
                );
            } else if let Some(v) = &ed.val {
                if *v == Value::Empty {
                    let _ = self.wb.sheet_mut(si).remove(ed.r, ed.c);
                } else {
                    self.wb.sheet_mut(si).set(
                        ed.r,
                        ed.c,
                        Cell {
                            value: v.clone(),
                            formula: None,
                        },
                    );
                }
            }
            if let Some(fmt) = &ed.format {
                let sid = self.wb.sheet(si).style_of(ed.r, ed.c);
                let mut st = self.wb.styles.get(sid).clone();
                st.format = fmt.clone();
                let nsid = self.wb.styles.intern(st);
                self.wb.sheet_mut(si).set_style(ed.r, ed.c, nsid);
            }
        }
        let deltas = sheetcore::undo::finish(before, self.wb.sheet(si));
        if !deltas.is_empty() {
            self.record_revision("AI 写入", &deltas);
            self.undo.push(sheetcore::undo::Action {
                label: "AI 写入".into(),
                merges: None,
                deltas,
                focus: (si, keys[0].0, keys[0].1),
                snap_before: None,
                snap_after: None,
                by_site: 0,
                pictures: None,
                shapes: None,
                refresh_sizes: None,
            });
        }
        let touched: Vec<_> = keys.iter().map(|&(r, c)| (si, r, c)).collect();
        self.recalc_and_broadcast(&touched);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);

        let mut samples = Vec::new();
        let mut errors = Vec::new();
        let sh = self.wb.sheet(si);
        for &(r, c) in &keys {
            let v = sh.value(r, c);
            if matches!(v, Value::Error(_)) {
                errors.push(format!("{}={}", cell_name(r, c), v.display()));
            }
            if samples.len() < 12 {
                samples.push(json!({"cell": cell_name(r, c), "value": v.display()}));
            }
        }
        (
            format!("写入 {} 个单元格", parsed.len()),
            json!({
                "ok": true,
                "written": parsed.len(),
                "samples": samples,
                "errors": errors
            }),
        )
    }

    fn tool_fill_formula_range(&mut self, args: &JsonValue) -> (String, JsonValue) {
        let Some(si) = self.resolve_sheet_index(args.get("sheet").and_then(|v| v.as_str())) else {
            return ("填充公式失败".into(), json!({"ok": false, "error": "未找到工作表"}));
        };
        let Some(range_str) = args.get("range").and_then(|v| v.as_str()) else {
            return ("填充公式失败".into(), json!({"ok": false, "error": "缺少 range"}));
        };
        let Some((r1, c1, r2, c2)) = Self::parse_rect(range_str) else {
            return ("填充公式失败".into(), json!({"ok": false, "error": "range 格式无效"}));
        };
        let area = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
        if area == 0 || area > 50_000 {
            return ("填充公式失败".into(), json!({"ok": false, "error": "区域大小超出范围 (1..=50000)"}));
        }
        let raw_f = args
            .get("top_left_formula")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let body = raw_f.strip_prefix('=').unwrap_or(raw_f);
        if body.is_empty() {
            return ("填充公式失败".into(), json!({"ok": false, "error": "缺少 top_left_formula"}));
        }
        let fmt = args.get("format").and_then(|v| v.as_str()).map(str::to_string);
        let keys = sheetcore::undo::rect_keys(r1, c1, r2, c2);
        if !self.write_preflight_on(si, &keys, "AI 填充公式") {
            return ("填充被保护/锁定拒绝".into(), json!({"ok": false, "error": self.status}));
        }

        let sh_name = self.wb.sheet(si).name.clone();
        let before = sheetcore::undo::capture(self.wb.sheet(si), si, &keys);
        for r in r1..=r2 {
            for c in c1..=c2 {
                let dr = r as i64 - r1 as i64;
                let dc = c as i64 - c1 as i64;
                let translated = if dr == 0 && dc == 0 {
                    body.to_string()
                } else {
                    refs::translate(body, dr, dc, &sh_name)
                };
                self.wb.sheet_mut(si).set(
                    r,
                    c,
                    Cell {
                        value: Value::Empty,
                        formula: Some(translated.into_boxed_str()),
                    },
                );
                if let Some(ref fstr) = fmt {
                    let sid = self.wb.sheet(si).style_of(r, c);
                    let mut st = self.wb.styles.get(sid).clone();
                    st.format = fstr.clone();
                    let nsid = self.wb.styles.intern(st);
                    self.wb.sheet_mut(si).set_style(r, c, nsid);
                }
            }
        }
        let deltas = sheetcore::undo::finish(before, self.wb.sheet(si));
        if !deltas.is_empty() {
            self.record_revision("AI 填充公式", &deltas);
            self.undo.push(sheetcore::undo::Action {
                label: "AI 填充公式".into(),
                merges: None,
                deltas,
                focus: (si, r1, c1),
                snap_before: None,
                snap_after: None,
                by_site: 0,
                pictures: None,
                shapes: None,
                refresh_sizes: None,
            });
        }
        let touched: Vec<_> = keys.iter().map(|&(r, c)| (si, r, c)).collect();
        self.recalc_and_broadcast(&touched);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);

        let sh = self.wb.sheet(si);
        let mut samples = Vec::new();
        let mut errors = Vec::new();
        for &(r, c) in &keys {
            let v = sh.value(r, c);
            if matches!(v, Value::Error(_)) {
                errors.push(format!("{}={}", cell_name(r, c), v.display()));
            }
            if samples.len() < 8 {
                samples.push(json!({"cell": cell_name(r, c), "value": v.display()}));
            }
        }
        let span = range_label((r1, c1), (r2, c2));
        (
            format!("填充公式 {sh_name}!{span}"),
            json!({
                "ok": true,
                "range": span,
                "count": keys.len(),
                "samples": samples,
                "errors": errors
            }),
        )
    }

    fn tool_style_range(&mut self, args: &JsonValue) -> (String, JsonValue) {
        let Some(si) = self.resolve_sheet_index(args.get("sheet").and_then(|v| v.as_str())) else {
            return ("设置样式失败".into(), json!({"ok": false, "error": "未找到工作表"}));
        };
        let Some(range_str) = args.get("range").and_then(|v| v.as_str()) else {
            return ("设置样式失败".into(), json!({"ok": false, "error": "缺少 range"}));
        };
        let Some((r1, c1, r2, c2)) = Self::parse_rect(range_str) else {
            return ("设置样式失败".into(), json!({"ok": false, "error": "range 格式无效"}));
        };
        let saved = (self.sheet_idx, self.sel, self.sel_end, self.extra_sel.clone());
        self.sheet_idx = si;
        self.sel = (r1, c1);
        self.sel_end = (r2, c2);
        self.extra_sel.clear();

        let bold = args.get("bold").and_then(|v| v.as_bool());
        let italic = args.get("italic").and_then(|v| v.as_bool());
        let fill_rgb = args
            .get("fill_hex")
            .and_then(|v| v.as_str())
            .and_then(parse_hex_rgb);
        let clear_fill = args.get("fill_hex").is_some_and(|v| v.is_null());
        let font_rgb = args
            .get("font_hex")
            .and_then(|v| v.as_str())
            .and_then(parse_hex_rgb);
        let halign = args.get("halign").and_then(|v| v.as_str()).and_then(|s| match s {
            "left" => Some(1u8),
            "center" => Some(2),
            "right" => Some(3),
            _ => None,
        });
        let format = args.get("format").and_then(|v| v.as_str()).map(str::to_string);
        let borders = args.get("borders").and_then(|v| v.as_str()).map(str::to_string);

        self.apply_style_xy(|st, r, c| {
            if let Some(b) = bold {
                st.bold = b;
            }
            if let Some(it) = italic {
                st.italic = it;
            }
            if clear_fill {
                st.fill = None;
            } else if let Some(rgb) = fill_rgb {
                st.fill = Some(rgb);
            }
            if let Some(rgb) = font_rgb {
                st.color = rgb;
            }
            if let Some(ha) = halign {
                st.halign = ha;
            }
            if let Some(ref fmt) = format {
                st.format = fmt.clone();
            }
            if let Some(ref bmode) = borders {
                let bd = sheetcore::style::Border { style: 1, color: [0, 0, 0] };
                let none = sheetcore::style::Border::default();
                match bmode.as_str() {
                    "all" => st.border = [bd; 4],
                    "none" => st.border = [none; 4],
                    "outer" => {
                        if c == c1 { st.border[0] = bd; }
                        if c == c2 { st.border[1] = bd; }
                        if r == r1 { st.border[2] = bd; }
                        if r == r2 { st.border[3] = bd; }
                    }
                    _ => {}
                }
            }
        });

        self.sheet_idx = saved.0;
        self.sel = saved.1;
        self.sel_end = saved.2;
        self.extra_sel = saved.3;

        let span = range_label((r1, c1), (r2, c2));
        (format!("设置样式 {span}"), json!({"ok": true, "range": span}))
    }

    fn tool_manage_sheet(&mut self, args: &JsonValue) -> (String, JsonValue) {
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
        if name.is_empty() {
            return ("工作表操作失败".into(), json!({"ok": false, "error": "缺少 name"}));
        }
        match op {
            "create" => {
                if !self.can_change_sheets() {
                    return ("工作簿结构受保护".into(), json!({"ok": false, "error": self.status}));
                }
                let index = self.wb.sheets.len();
                if !sheetcore::sheet_ops::insert_sheet(
                    &mut self.wb,
                    index,
                    sheetcore::model::Sheet::new(name),
                ) {
                    return ("新建工作表失败".into(), json!({"ok": false, "error": "名称无效或已存在"}));
                }
                self.switch_sheet_view(index);
                self.finish_sheet_change();
                (format!("新建工作表「{name}」（已重置撤销历史）"), json!({"ok": true, "sheet": name, "undo_history_reset": true}))
            }
            "switch" => {
                if let Some(i) = self.resolve_sheet_index(Some(name)) {
                    self.switch_sheet_view(i);
                    (format!("切换至「{name}」"), json!({"ok": true, "sheet": name}))
                } else {
                    ("切换工作表失败".into(), json!({"ok": false, "error": "未找到工作表"}))
                }
            }
            "rename" => {
                if !self.can_change_sheets() {
                    return ("重命名失败".into(), json!({"ok": false, "error": self.status}));
                }
                let new_name = args.get("new_name").and_then(|v| v.as_str()).unwrap_or("").trim();
                let Some(i) = self.resolve_sheet_index(Some(name)) else {
                    return ("重命名失败".into(), json!({"ok": false, "error": "未找到工作表"}));
                };
                if new_name.is_empty() || !sheetcore::sheet_ops::rename_sheet(&mut self.wb, i, new_name) {
                    return ("重命名失败".into(), json!({"ok": false, "error": "目标名称无效或重名"}));
                }
                self.finish_sheet_change();
                (
                    format!("重命名「{name}」→「{new_name}」（已重置撤销历史）"),
                    json!({"ok": true, "sheet": new_name, "undo_history_reset": true}),
                )
            }
            _ => ("工作表操作失败".into(), json!({"ok": false, "error": "未知 op"})),
        }
    }

    fn tool_data_operation(&mut self, args: &JsonValue, _ctx: &egui::Context) -> (String, JsonValue) {
        let Some(si) = self.resolve_sheet_index(args.get("sheet").and_then(|v| v.as_str())) else {
            return ("数据操作失败".into(), json!({"ok": false, "error": "未找到工作表"}));
        };
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let (r1, c1, mut r2, mut c2) = args
            .get("range")
            .and_then(|v| v.as_str())
            .and_then(Self::parse_rect)
            .unwrap_or_else(|| self.sel_rect());
        if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 && op != "clear_contents" {
            if let Some((_, _, ur2, uc2)) = self.wb.sheet(si).used_bounds() {
                r2 = r2.min(ur2.max(r1));
                c2 = c2.min(uc2.max(c1));
            } else {
                r2 = r1;
                c2 = c1;
            }
        }
        let saved = (self.sheet_idx, self.sel, self.sel_end, self.extra_sel.clone());
        self.sheet_idx = si;
        self.sel = (r1, c1);
        self.sel_end = (r2, c2);
        self.extra_sel.clear();

        let label = match op {
            "sort_asc" => {
                self.do_sort(c1, true);
                "升序排序"
            }
            "sort_desc" => {
                self.do_sort(c1, false);
                "降序排序"
            }
            "highlight_duplicates" => {
                self.highlight_duplicates();
                "高亮重复值"
            }
            "deduplicate" => {
                self.run_dedup();
                "删除重复行"
            }
            "merge_same" => {
                self.merge_same_cells();
                "合并相同单元格"
            }
            "clear_contents" => {
                self.clear_values();
                "清除内容"
            }
            _ => {
                self.sheet_idx = saved.0;
                self.sel = saved.1;
                self.sel_end = saved.2;
                self.extra_sel = saved.3;
                return ("数据操作失败".into(), json!({"ok": false, "error": "不支持的 op"}));
            }
        };

        self.sheet_idx = saved.0;
        self.sel = saved.1;
        self.sel_end = saved.2;
        self.extra_sel = saved.3;

        let span = range_label((r1, c1), (r2, c2));
        (format!("{label} {span}"), json!({"ok": true, "status": self.status}))
    }

    // =====================================================================
    // 右侧 AI 助手停靠栏界面
    // =====================================================================

    pub(crate) fn draw_ai_panel(&mut self, ctx: &egui::Context) {
        self.ai.ui_ctx = Some(ctx.clone());
        let mut close_panel = false;
        let mut send_prompt: Option<String> = None;
        let mut retry_prompt: Option<String> = None;
        let mut undo_requested = false;
        let mut clear_chat = false;
        let mut consent_answer: Option<bool> = None;

        egui::SidePanel::right("wps_ai_panel")
            .resizable(true)
            .show_separator_line(false)
            .default_width(360.0)
            .width_range(300.0..=520.0)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(12, 10)),
            )
            .show(ctx, |ui| {
                let outer = ui.max_rect().expand2(Vec2::new(12.0, 10.0));
                let clicked_in_ai = ctx.input(|i| {
                    i.pointer.primary_pressed()
                        && i.pointer.interact_pos().is_some_and(|p| outer.contains(p))
                });
                if clicked_in_ai {
                    self.filter_popup = None;
                    if !self.finish_pending_edit() {
                        self.ai.status_hint = self.status.clone();
                        return;
                    }
                }
                ui.painter().line_segment(
                    [outer.left_top(), outer.left_bottom()],
                    Stroke::new(1.0_f32, ui_kit::CARD_BORDER),
                );
                // 1. 顶部栏
                ui.horizontal(|ui| {
                    let (ir, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                    ui_icons::draw_excel(ui.painter(), ir, Icon::AiSparkle, ui_kit::BRAND_NAVY, true);
                    ui_kit::paint_medium(
                        ui.painter(),
                        egui::pos2(ui.cursor().left(), ir.center().y),
                        Align2::LEFT_CENTER,
                        "AI 助手",
                        14.0,
                        ui_kit::TEXT,
                    );
                    ui.add_space(56.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_kit::pill(ui, "关闭", "关闭 AI 助手 (Ctrl+Shift+A)").clicked() {
                            close_panel = true;
                        }
                        if ui_kit::pill(ui, "设置", "配置本地模型接口与密钥").clicked() {
                            self.ai.settings_open = !self.ai.settings_open;
                            if self.ai.settings_open {
                                self.ai.draft_cfg = self.ai.cfg.clone();
                                self.ai.show_key = false;
                            }
                        }
                        if !self.ai.messages.is_empty()
                            && !self.ai.busy
                            && ui_kit::pill(ui, "新对话", "清空当前对话并重置上下文").clicked()
                        {
                            clear_chat = true;
                        }
                    });
                });
                ui.separator();

                // 2. 可折叠模型设置区
                if self.ai.settings_open {
                    egui::Frame::NONE
                        .fill(Color32::from_rgb(248, 250, 252))
                        .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.label(egui::RichText::new("模型服务配置").size(12.0).color(ui_kit::TEXT));
                            ui.label(egui::RichText::new("显示名称").size(11.0).color(ui_kit::TEXT_WEAK));
                            ui_kit::text_input(ui, &mut self.ai.draft_cfg.display_name, "DeepSeek Local H200");
                            ui.label(egui::RichText::new("接口地址 (OpenAI 兼容)").size(11.0).color(ui_kit::TEXT_WEAK));
                            ui_kit::text_input(ui, &mut self.ai.draft_cfg.base_url, "https://deepseek.hollantex.net/v1");
                            ui.label(egui::RichText::new("模型 ID").size(11.0).color(ui_kit::TEXT_WEAK));
                            ui_kit::text_input(ui, &mut self.ai.draft_cfg.model, "deepseek-v4.1-flash");
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("API Key").size(11.0).color(ui_kit::TEXT_WEAK));
                                if self.ai.draft_cfg.uses_builtin_key() {
                                    ui.label(
                                        egui::RichText::new("已启用内置密钥")
                                            .size(10.5)
                                            .color(ui_kit::ACCENT),
                                    );
                                    if ui.small_button("更换密钥").clicked() {
                                        self.ai.draft_cfg.api_key.clear();
                                        self.ai.show_key = false;
                                    }
                                } else {
                                    if !self.ai.draft_cfg.has_api_key() {
                                        ui.label(
                                            egui::RichText::new("未填写（向服务管理员申请自己的密钥）")
                                                .size(10.5)
                                                .color(ui_kit::TEXT_WEAK),
                                        );
                                    }
                                    if ui.small_button(if self.ai.show_key { "隐藏" } else { "显示" }).clicked() {
                                        self.ai.show_key = !self.ai.show_key;
                                    }
                                    if has_builtin_key()
                                        && is_default_base_url(&self.ai.draft_cfg.base_url)
                                        && ui.small_button("恢复内置密钥").clicked()
                                    {
                                        self.ai.draft_cfg.api_key = builtin_api_key().into();
                                        self.ai.show_key = false;
                                    }
                                }
                            });
                            if self.ai.draft_cfg.uses_builtin_key() {
                                egui::Frame::NONE
                                    .fill(Color32::from_rgb(241, 245, 249))
                                    .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                                    .corner_radius(4.0)
                                    .inner_margin(egui::Margin::symmetric(8, 5))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(
                                            egui::RichText::new("••••••••••••••••")
                                                .size(11.5)
                                                .color(ui_kit::TEXT_WEAK),
                                        );
                                    });
                            } else if self.ai.show_key {
                                ui_kit::text_input(ui, &mut self.ai.draft_cfg.api_key, "Bearer 密钥");
                            } else {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.ai.draft_cfg.api_key)
                                        .password(true)
                                        .desired_width(ui.available_width()),
                                );
                            }
                            ui.horizontal(|ui| {
                                if ui_kit::primary_button(ui, "保存").clicked() {
                                    let mut next = self.ai.draft_cfg.clone();
                                    if has_builtin_key()
                                        && next.api_key == builtin_api_key()
                                        && !is_default_base_url(&next.base_url)
                                    {
                                        next.api_key.clear();
                                    }
                                    // 外发确认绑定在接口地址上：换了服务地址就要重新确认
                                    next.data_consent = self.ai.cfg.data_consent
                                        && self.ai.cfg.base_url.trim() == next.base_url.trim();
                                    self.ai.cfg = next;
                                    self.ai.cfg.save();
                                    self.ai.settings_open = false;
                                    self.ai.show_key = false;
                                    self.ai.status_hint = "模型配置已保存".into();
                                }
                                if ui_kit::secondary_button(ui, "恢复默认").clicked() {
                                    self.ai.draft_cfg = AiConfig::default();
                                    self.ai.cfg = self.ai.draft_cfg.clone();
                                    self.ai.cfg.save();
                                    self.ai.show_key = false;
                                    self.ai.status_hint = if has_builtin_key() {
                                        "已恢复默认内网 DeepSeek 配置（已启用内置密钥）".into()
                                    } else {
                                        "已恢复默认配置，请填写自己的 API Key".into()
                                    };
                                }
                            });
                        });
                    ui.add_space(6.0);
                }

                // 2b. 首次发送前的数据外发确认
                if self.ai.pending_consent_prompt.is_some() {
                    let host = parse_endpoint(&self.ai.cfg.base_url)
                        .map(|ep| ep.host)
                        .unwrap_or_else(|_| self.ai.cfg.base_url.trim().to_string());
                    egui::Frame::NONE
                        .fill(Color32::from_rgb(255, 251, 235))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(245, 158, 11)))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.label(
                                egui::RichText::new("发送前请确认：表格内容会离开本机")
                                    .size(12.0)
                                    .color(ui_kit::TEXT),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "助手会把以下内容发送到模型服务 {host}：工作簿名、各工作表名与已用区域、\
                                     当前选区的值、活动工作表的样本单元格、公式错误，以及助手按需读取的更多单元格。\
                                     请确认该服务允许处理这些数据。"
                                ))
                                .size(11.0)
                                .color(ui_kit::TEXT_WEAK),
                            );
                            ui.horizontal(|ui| {
                                if ui_kit::primary_button(ui, "同意并发送").clicked() {
                                    consent_answer = Some(true);
                                }
                                if ui_kit::secondary_button(ui, "取消").clicked() {
                                    consent_answer = Some(false);
                                }
                            });
                        });
                    ui.add_space(6.0);
                }

                // 3. 先在 SidePanel 内保留底部输入区，再让消息列表自然占满剩余高度，避免按钮被裁剪
                egui::TopBottomPanel::bottom("wps_ai_composer")
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(
                        egui::Frame::NONE
                            .fill(Color32::WHITE)
                            .inner_margin(egui::Margin {
                                left: 0,
                                right: 0,
                                top: 6,
                                bottom: 2,
                            }),
                    )
                    .show_inside(ui, |ui| {
                        ui.separator();
                        let (r1, c1, r2, c2) = self.sel_rect();
                        let ctx_lbl = format!(
                            "上下文：{}!{}",
                            self.wb.sheet(self.sheet_idx).name,
                            range_label((r1, c1), (r2, c2))
                        );
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(ctx_lbl).size(11.0).color(ui_kit::TEXT_WEAK));
                            if !self.ai.status_hint.is_empty() {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(
                                        egui::RichText::new(&self.ai.status_hint)
                                            .size(11.0)
                                            .color(ui_kit::ACCENT),
                                    );
                                });
                            }
                        });

                        let te_id = egui::Id::new("wps_ai_draft_input");
                        let had_focus = ui.ctx().memory(|m| m.has_focus(te_id));
                        let was_composing = self.ai.ime_composing;
                        let mut ime_event_this_frame = false;
                        let mut raw_enter_pressed = false;
                        ui.input(|i| {
                            for ev in &i.events {
                                if let egui::Event::Ime(ime_ev) = ev {
                                    match ime_ev {
                                        egui::ImeEvent::Preedit(s) => {
                                            if !s.is_empty() {
                                                self.ai.ime_composing = true;
                                                ime_event_this_frame = true;
                                            } else {
                                                if was_composing {
                                                    ime_event_this_frame = true;
                                                }
                                                self.ai.ime_composing = false;
                                            }
                                        }
                                        egui::ImeEvent::Commit(_) => {
                                            ime_event_this_frame = true;
                                            self.ai.ime_composing = false;
                                        }
                                        egui::ImeEvent::Disabled => {
                                            self.ai.ime_composing = false;
                                        }
                                        egui::ImeEvent::Enabled => {}
                                    }
                                }
                            }
                            raw_enter_pressed = i.key_pressed(Key::Enter)
                                && !i.modifiers.shift
                                && !i.modifiers.alt
                                && !i.modifiers.command;
                        });
                        let ime_busy = was_composing || self.ai.ime_composing || ime_event_this_frame;
                        let mut enter_send = false;
                        if had_focus
                            && !self.ai.busy
                            && !ime_busy
                            && raw_enter_pressed
                            && !self.ai.draft.trim().is_empty()
                        {
                            ui.input_mut(|i| {
                                i.consume_key(egui::Modifiers::NONE, Key::Enter);
                            });
                            enter_send = true;
                        }
                        let box_resp = egui::Frame::NONE
                            .fill(Color32::WHITE)
                            .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.ai.draft)
                                        .id(te_id)
                                        .frame(false)
                                        .desired_rows(3)
                                        .desired_width(ui.available_width())
                                        .hint_text("输入需求，例如：按部门汇总金额并把负数标红"),
                                )
                            });
                        let te_resp = box_resp.inner;
                        if self.ai.request_input_focus {
                            te_resp.request_focus();
                            self.ai.request_input_focus = false;
                        } else if ui.input(|i| i.pointer.primary_clicked())
                            && ui.rect_contains_pointer(box_resp.response.rect)
                            && !te_resp.has_focus()
                        {
                            te_resp.request_focus();
                        }
                        if te_resp.has_focus() {
                            ui.painter().rect_stroke(
                                box_resp.response.rect,
                                6.0,
                                Stroke::new(1.5, ui_kit::ACCENT),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if enter_send {
                            let msg = self.ai.draft.trim().to_string();
                            if !msg.is_empty() {
                                self.ai.draft.clear();
                                send_prompt = Some(msg);
                            }
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Enter 发送 · Shift+Enter 换行")
                                    .size(10.5)
                                    .color(ui_kit::TEXT_WEAK),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if self.ai.busy {
                                    if ui_kit::secondary_button(ui, "停止").clicked() {
                                        self.stop_ai_generation();
                                    }
                                } else if ui_kit::primary_button(ui, "发送").clicked() {
                                    let msg = self.ai.draft.trim().to_string();
                                    if !msg.is_empty() {
                                        self.ai.draft.clear();
                                        send_prompt = Some(msg);
                                    }
                                }
                            });
                        });
                    });

                egui::ScrollArea::vertical()
                    .id_salt("wps_ai_msgs")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 10.0;
                        if self.ai.messages.is_empty() {
                            egui::Frame::NONE
                                .fill(Color32::from_rgb(245, 248, 255))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(219, 230, 250)))
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::same(10))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.spacing_mut().item_spacing.y = 6.0;
                                    for sample in [
                                        "总结当前工作表的数据结构与关键指标",
                                        "为当前选区编写公式并自动填充计算",
                                        "按当前表数据新建一张分类汇总表",
                                        "排查并修复当前工作表中的公式错误",
                                    ] {
                                        let (cr, cresp) = ui.allocate_exact_size(
                                            Vec2::new(ui.available_width(), 30.0),
                                            Sense::click(),
                                        );
                                        ui.painter().rect(
                                            cr,
                                            6.0,
                                            Color32::WHITE,
                                            Stroke::new(
                                                1.0,
                                                if cresp.hovered() {
                                                    ui_kit::ACCENT
                                                } else {
                                                    Color32::from_rgb(219, 230, 250)
                                                },
                                            ),
                                            egui::StrokeKind::Inside,
                                        );
                                        let ir = Rect::from_center_size(
                                            egui::pos2(cr.left() + 14.0, cr.center().y),
                                            Vec2::splat(13.0),
                                        );
                                        ui_icons::draw_excel(
                                            ui.painter(),
                                            ir,
                                            Icon::AiSparkle,
                                            ui_kit::BRAND_NAVY,
                                            true,
                                        );
                                        ui.painter().text(
                                            egui::pos2(cr.left() + 26.0, cr.center().y),
                                            Align2::LEFT_CENTER,
                                            sample,
                                            FontId::proportional(12.0),
                                            ui_kit::TEXT,
                                        );
                                        ui_kit::caret_right(
                                            ui,
                                            egui::pos2(cr.right() - 10.0, cr.center().y),
                                            if cresp.hovered() {
                                                ui_kit::ACCENT
                                            } else {
                                                ui_kit::TEXT_WEAK
                                            },
                                        );
                                        if cresp.hovered() {
                                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                        }
                                        if cresp.clicked() {
                                            send_prompt = Some(sample.to_string());
                                        }
                                    }
                                });
                        }

                        for idx in 0..self.ai.messages.len() {
                            let role = self.ai.messages[idx].role;
                            match role {
                                AiRole::User => {
                                    let txt = self.ai.messages[idx].content.clone();
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                                        egui::Frame::NONE
                                            .fill(Color32::from_rgb(232, 245, 237))
                                            .stroke(Stroke::new(1.0, Color32::from_rgb(198, 229, 210)))
                                            .corner_radius(8.0)
                                            .inner_margin(egui::Margin::symmetric(10, 7))
                                            .show(ui, |ui| {
                                                ui.set_max_width((ui.available_width() * 0.85).max(160.0));
                                                ui.add(egui::Label::new(
                                                    egui::RichText::new(txt).size(12.5).color(ui_kit::TEXT),
                                                ).wrap());
                                            });
                                    });
                                }
                                AiRole::Assistant => {
                                    let has_reason = !self.ai.messages[idx].reasoning.is_empty();
                                    let reason_open = self.ai.messages[idx].reasoning_open;
                                    let reason_txt = self.ai.messages[idx].reasoning.clone();
                                    let tool_logs = self.ai.messages[idx].tool_logs.clone();
                                    let content_txt = self.ai.messages[idx].content.clone();
                                    let err_opt = self.ai.messages[idx].error.clone();
                                    let usteps = self.ai.messages[idx].undo_ids.len();
                                    let is_last = idx + 1 == self.ai.messages.len();

                                    egui::Frame::NONE
                                        .fill(Color32::from_rgb(248, 250, 252))
                                        .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                                        .corner_radius(8.0)
                                        .inner_margin(egui::Margin::symmetric(10, 8))
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            if has_reason {
                                                let (hdr, hresp) = ui.allocate_exact_size(
                                                    Vec2::new(ui.available_width(), 20.0),
                                                    Sense::click(),
                                                );
                                                if reason_open {
                                                    ui_kit::caret(ui, egui::pos2(hdr.left() + 6.0, hdr.center().y), ui_kit::TEXT_WEAK);
                                                } else {
                                                    ui_kit::caret_right(ui, egui::pos2(hdr.left() + 6.0, hdr.center().y), ui_kit::TEXT_WEAK);
                                                }
                                                ui.painter().text(
                                                    egui::pos2(hdr.left() + 16.0, hdr.center().y),
                                                    Align2::LEFT_CENTER,
                                                    if reason_open { "收起思考过程" } else { "已深度思考（点击展开）" },
                                                    FontId::proportional(11.0),
                                                    ui_kit::TEXT_WEAK,
                                                );
                                                if hresp.clicked() {
                                                    self.ai.messages[idx].reasoning_open = !reason_open;
                                                }
                                                if reason_open {
                                                    ui.add(egui::Label::new(
                                                        egui::RichText::new(reason_txt)
                                                            .size(11.0)
                                                            .color(ui_kit::TEXT_WEAK),
                                                    ).wrap());
                                                    ui.separator();
                                                }
                                            }
                                            if !tool_logs.is_empty() {
                                                for tl in &tool_logs {
                                                    ui.label(
                                                        egui::RichText::new(format!("[执行] {tl}"))
                                                            .size(11.0)
                                                            .color(ui_kit::ACCENT),
                                                    );
                                                }
                                            }
                                            if !content_txt.is_empty() {
                                                ui.add(egui::Label::new(
                                                    egui::RichText::new(content_txt).size(12.5).color(ui_kit::TEXT),
                                                ).wrap());
                                            } else if is_last && self.ai.busy {
                                                ui.label(
                                                    egui::RichText::new("正在生成答复…")
                                                        .size(12.0)
                                                        .color(ui_kit::TEXT_WEAK),
                                                );
                                            }
                                            if let Some(err) = err_opt {
                                                ui.add(egui::Label::new(
                                                    egui::RichText::new(err)
                                                        .size(11.5)
                                                        .color(Color32::from_rgb(192, 0, 0)),
                                                ).wrap());
                                                if is_last && idx > 0 && !self.ai.busy {
                                                    if ui_kit::pill(ui, "重试", "重新发送刚才的请求").clicked() {
                                                        retry_prompt = Some(self.ai.messages[idx - 1].content.clone());
                                                    }
                                                }
                                            }
                                            if usteps > 0 && !self.ai.busy && is_last {
                                                ui.add_space(4.0);
                                                if ui_kit::pill(ui, "撤销本轮表格编辑", "撤销本轮在历史中记录的编辑，保留后续独立修改").clicked() {
                                                    undo_requested = true;
                                                }
                                            }
                                        });
                                }
                            }
                        }
                        if self.ai.scroll_bottom {
                            ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                            self.ai.scroll_bottom = false;
                        }
                    });
            });

        if close_panel {
            self.ai.open = false;
        }
        if clear_chat {
            self.ai.messages.clear();
            self.ai.wire_history.clear();
            self.ai.status_hint.clear();
            self.ai.pending_consent_prompt = None;
        }
        if undo_requested {
            self.undo_last_ai_change();
        }
        match consent_answer {
            Some(true) => {
                self.ai.cfg.data_consent = true;
                self.ai.draft_cfg.data_consent = true;
                self.ai.cfg.save();
                if let Some(p) = self.ai.pending_consent_prompt.take() {
                    self.start_ai_prompt(p);
                }
            }
            Some(false) => {
                if let Some(p) = self.ai.pending_consent_prompt.take() {
                    // 把没发出去的提问还给输入框，不丢用户打的字
                    if self.ai.draft.trim().is_empty() {
                        self.ai.draft = p;
                    }
                }
                self.ai.status_hint = "已取消，未发送任何内容".into();
            }
            None => {}
        }
        if let Some(p) = retry_prompt {
            // 删掉失败的最后一轮再重发
            if self.ai.messages.len() >= 2 {
                self.ai.messages.truncate(self.ai.messages.len() - 2);
            }
            if let Some(pos) = self
                .ai
                .wire_history
                .iter()
                .rposition(|w| w.role == "user" && !w.content.starts_with("[工具执行返回]"))
            {
                self.ai.wire_history.truncate(pos);
            }
            self.start_ai_prompt(p);
        } else if let Some(p) = send_prompt {
            self.start_ai_prompt(p);
        }
    }
}

#[cfg(test)]
mod grouped_undo_tests {
    use super::*;
    use std::path::PathBuf;

    fn edited_by_ai() -> SheetApp {
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.ai.bound_doc_uid = Some(app.doc_uid);
        app.ai.messages.push(AiMessage {
            role: AiRole::Assistant, content: String::new(), reasoning: String::new(),
            reasoning_open: false, tool_logs: Vec::new(), error: None, undo_ids: Vec::new(),
        });
        let tool = ToolCallSpec { name: "set_cells".into(), arguments: json!({
            "edits": [{"cell":"A1", "value":100}]
        }) };
        assert_eq!(app.execute_ai_tool(&tool, &ctx).1["ok"], true);
        app
    }

    fn manual_edit(app: &mut SheetApp, col: u32) {
        app.sel = (0, col);
        app.sel_end = app.sel;
        app.begin_edit(Some("200".into()));
        assert!(app.commit_edit(None));
    }

    #[test]
    fn ai_undo_preserves_subsequent_manual_edit_and_redo_order() {
        let mut app = edited_by_ai();
        manual_edit(&mut app, 1);
        app.undo_last_ai_change();
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Empty);
        assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(200.0));
        app.do_undo(false);
        assert_eq!(app.wb.sheet(0).value(0, 1), Value::Empty);
        app.do_undo(true);
        app.do_undo(true);
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(100.0));
        assert_eq!(app.wb.sheet(0).value(0, 1), Value::Number(200.0));
    }

    #[test]
    fn ai_undo_refuses_overlapping_manual_edit_and_other_workbook() {
        let mut app = edited_by_ai();
        manual_edit(&mut app, 0);
        let depth = app.undo.depth();
        app.undo_last_ai_change();
        assert_eq!(app.undo.depth(), depth);
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(200.0));
        app.open_in_new_tab(PathBuf::new());
        manual_edit(&mut app, 0);
        app.undo_last_ai_change();
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(200.0));
    }
}
