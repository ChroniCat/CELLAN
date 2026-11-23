//! 局域网协作传输层：星形拓扑（宿主转发）+ 行分隔 JSON 帧。
//!
//! 所有会话使用房间口令和随机盐建立带认证的加密通道；完成握手后才交换工作簿数据。

use sha2::{Digest, Sha256};
use sheetcore::sync::{Cursor, Msg, Op};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 7878;

/// 实际监听的端口：`SC_NET_PORT` 可覆盖（同机多实例、端口被占时用）
pub fn port() -> u16 {
    std::env::var("SC_NET_PORT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

#[derive(Debug, Clone)]
pub enum NetEvent {
    Connected,
    Welcome {
        peers: Vec<(u32, String, u8)>,
        delta_sync: bool,
        clock: u64,
    },
    PeerJoined(u32, String, u8),
    /// 在线参与者的显示名变了
    PeerRenamed(u32, String),
    PeerLeft(u32),
    Ops(Vec<Op>),
    /// 宿主初始/重连快照接收完毕
    SnapshotDone,
    Cursor(Cursor),
    /// 某格被批准编辑（含自己）
    LockOk { site: u32, name: String, sheet: usize, row: u32, col: u32 },
    /// 结构改动历史（加入时补）
    StructuralHistory(Vec<(usize, sheetcore::structural::Structural)>),
    /// 远端结构性改动（带已认证站点号）
    Structural { site: u32, sheet: usize, op: sheetcore::structural::Structural },
    /// 远端格式变更（`snapshot=true` 表示入房补发的既有样式，不记修订日志）
    Styles {
        site: u32,
        items: Vec<(usize, u32, u32, sheetcore::style::CellStyle)>,
        snapshot: bool,
    },
    /// 远端合并变更
    Merge { site: u32, sheet: usize, rect: (u32, u32, u32, u32), on: bool },
    /// 自己申请编辑被拒绝
    LockDeny { holder: u32, holder_name: String, sheet: usize, row: u32, col: u32 },
    /// 自己发起的写入/格式/合并因他人持有活跃租约被宿主驳回
    WriteRejected {
        holder: u32,
        holder_name: String,
        sheet: usize,
        row: u32,
        col: u32,
        revert_ops: Vec<(u32, u32, Option<Op>)>,
        revert_styles: Vec<(u32, u32, Option<sheetcore::style::CellStyle>)>,
        revert_merge: Option<(u32, u32, u32, u32, bool)>,
    },
    /// 某格释放
    LockRelease { site: u32, sheet: usize, row: u32, col: u32 },
    /// 协作者请求宿主将权威状态写回原文件
    SaveRequested { site: u32, name: String },
    /// 宿主已完成原文件保存，通知在线成员清除脏状态
    Saved { clock: u64 },
    /// 房间内有成员持有经过官方公钥校验的新版本可执行文件
    PeerUpdateOffer {
        peer_site: u32,
        peer_name: String,
        manifest: crate::update::SignedManifest,
    },
    /// 正在从协作成员拉取新版进度
    PeerUpdateProgress { received: u64, total: u64 },
    /// 协作成员传来的新版安装包已完成 SHA-256 与官方签名双重核验
    PeerUpdateReady {
        version: String,
        verified_exe: std::path::PathBuf,
    },
    /// 协作接力更新中断或校验未通过
    PeerUpdateFailed(String),
    Disconnected(String),
    Denied(String),
    Error(String),
}

enum Cmd {
    Broadcast(String),
    Cells(Vec<Op>),
    Style(Msg),
    Deliver(Vec<Sender<String>>, RoomChange),
    SendTo(u32, String),
    Stop,
}

/// Recipients are captured in the same order as the corresponding host state
/// updates. Encoding happens on the dispatcher after releasing the state lock.
enum RoomChange {
    Cells(Vec<Op>),
    Message(Msg),
}

fn queue_room_change(cmd: &Sender<Cmd>, roster: &Mutex<RoomRoster>, except: Option<u32>, change: RoomChange) -> bool {
    let peers = roster.lock().unwrap().other_senders(except);
    peers.is_empty() || cmd.send(Cmd::Deliver(peers, change)).is_ok()
}

fn cell_frames(ops: impl IntoIterator<Item = Op>, to_host: bool, batch: bool, mut send: impl FnMut(String)) {
    use sheetcore::sync::ValueWire;
    let mut pending = Vec::new();
    let mut bytes = 0usize;
    let mut flush = |mut chunk: Vec<Op>| {
        let msg = if chunk.len() == 1 {
            Msg::Op(chunk.pop().unwrap())
        } else if to_host {
            Msg::Ops { ops: chunk }
        } else {
            Msg::SnapshotChunk { ops: chunk, last: false }
        };
        send(serde_json::to_string(&msg).unwrap());
    };
    for op in ops {
        let text_len = match &op.value { ValueWire::Text(s) | ValueWire::Error(s) => s.len(), _ => 0 };
        let size = text_len + op.formula.as_ref().map_or(0, String::len) + 128;
        if !pending.is_empty() && (!batch || pending.len() == sheetcore::sync::LIVE_OP_BATCH || bytes + size > 256 * 1024) {
            flush(std::mem::take(&mut pending));
            bytes = 0;
        }
        bytes += size;
        pending.push(op);
    }
    if !pending.is_empty() { flush(pending); }
}

struct RoomRoster {
    host_site: u32,
    peers: HashMap<u32, Sender<String>>,
    connecting: std::collections::HashSet<u32>,
    names: HashMap<u32, (String, u8)>,
    auth_hashes: HashMap<u32, [u8; 32]>,
}

fn sha256_bytes(input: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(input);
    let digest = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn clean_member_name(raw: &str, site: u32) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(32)
        .collect();
    if cleaned.is_empty() {
        format!("用户#{site}")
    } else {
        cleaned
    }
}

impl RoomRoster {
    fn new(host_site: u32, host_name: String) -> Self {
        let mut names = HashMap::new();
        let canonical = clean_member_name(&host_name, host_site);
        names.insert(host_site, (canonical, (host_site % 7) as u8));
        Self {
            host_site,
            peers: HashMap::new(),
            connecting: std::collections::HashSet::new(),
            names,
            auth_hashes: HashMap::new(),
        }
    }

    fn unique_name_for(&self, site: u32, raw: &str) -> String {
        let base = clean_member_name(raw, site);
        let taken = |cand: &str| {
            self.names
                .iter()
                .any(|(s, (n, _))| *s != site && n == cand)
        };
        if !taken(&base) {
            return base;
        }
        let suffixed = format!("{base}#{site}");
        if !taken(&suffixed) {
            return suffixed;
        }
        let mut idx = 2u32;
        loop {
            let cand = format!("{base}#{site}-{idx}");
            if !taken(&cand) {
                return cand;
            }
            idx = idx.saturating_add(1);
        }
    }

    fn name_of(&self, site: u32) -> String {
        self.names
            .get(&site)
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| format!("用户#{site}"))
    }

    fn admit(
        &mut self,
        site: u32,
        raw_name: &str,
        auth: &str,
    ) -> Result<(String, Vec<(u32, String, u8)>), &'static str> {
        if site == 0
            || site == self.host_site
            || self.peers.contains_key(&site)
            || self.connecting.contains(&site)
        {
            return Err("站点标识冲突，请重新加入协作会话");
        }
        let auth_hash = (!auth.trim().is_empty()).then(|| sha256_bytes(auth.as_bytes()));
        if let Some(expected) = self.auth_hashes.get(&site) {
            if auth_hash != Some(*expected) {
                return Err("历史站点标识已被绑定，不能冒用其他成员身份");
            }
        } else if let Some(h) = auth_hash {
            self.auth_hashes.insert(site, h);
        }
        let known: Vec<(u32, String, u8)> = self
            .names
            .iter()
            .map(|(s, (n, c))| (*s, n.clone(), *c))
            .collect();
        let canonical = self.unique_name_for(site, raw_name);
        let color = (site % 7) as u8;
        self.connecting.insert(site);
        self.names.insert(site, (canonical.clone(), color));
        Ok((canonical, known))
    }

    fn finish_admit(&mut self, site: u32, tx: Sender<String>) -> Vec<Sender<String>> {
        self.connecting.remove(&site);
        self.peers.insert(site, tx);
        self.peers
            .iter()
            .filter(|(s, _)| **s != site)
            .map(|(_, ptx)| ptx.clone())
            .collect()
    }

    fn rename_member(&mut self, site: u32, raw_name: &str) -> String {
        let canonical = self.unique_name_for(site, raw_name);
        let color = (site % 7) as u8;
        self.names.insert(site, (canonical.clone(), color));
        canonical
    }

    fn remove_member(&mut self, site: u32) -> Vec<Sender<String>> {
        self.connecting.remove(&site);
        if site != self.host_site {
            self.peers.remove(&site);
            self.names.remove(&site);
        }
        self.peers.values().cloned().collect()
    }

    fn other_senders(&self, exclude: Option<u32>) -> Vec<Sender<String>> {
        self.peers
            .iter()
            .filter(|(s, _)| Some(**s) != exclude)
            .map(|(_, ptx)| ptx.clone())
            .collect()
    }

    fn all_senders(&self) -> Vec<Sender<String>> {
        self.peers.values().cloned().collect()
    }

    fn sender_of(&self, site: u32) -> Option<Sender<String>> {
        self.peers.get(&site).cloned()
    }
}

struct ActivePeerDownload {
    peer_site: u32,
    manifest: crate::update::SignedManifest,
    file: std::fs::File,
    part_path: std::path::PathBuf,
    written: u64,
    last_pct: u64,
}

type SharedUpdateSeed =
    Arc<Mutex<Option<(std::path::PathBuf, crate::update::SignedManifest)>>>;

fn resolve_local_seed(
    override_seed: &SharedUpdateSeed,
) -> Option<(std::path::PathBuf, crate::update::SignedManifest)> {
    if let Ok(g) = override_seed.lock() {
        if let Some(pair) = g.clone() {
            return Some(pair);
        }
    }
    crate::update::local_signed_bundle()
}

const UPDATE_SLICE_RAW_BYTES: usize = 192 * 1024;

fn spawn_peer_update_stream(
    my_site: u32,
    to_site: u32,
    req_version: String,
    seed_override: SharedUpdateSeed,
    serving_guard: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    send_frame: impl Fn(String) -> bool + Send + 'static,
) {
    if serving_guard
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    std::thread::spawn(move || {
        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _g = Guard(serving_guard);

        let Some((exe_path, manifest)) = resolve_local_seed(&seed_override) else {
            return;
        };
        if manifest.version != req_version || !crate::update::verify_manifest_sig(&manifest) {
            return;
        }
        let Ok(mut f) = std::fs::File::open(&exe_path) else {
            return;
        };
        let Ok(md) = f.metadata() else {
            return;
        };
        let total = md.len();
        if total == 0 || total > 256 * 1024 * 1024 {
            return;
        }
        let mut buf = vec![0u8; UPDATE_SLICE_RAW_BYTES];
        let mut offset = 0u64;
        while alive.load(Ordering::Acquire) {
            let n = match std::io::Read::read(&mut f, &mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let next_off = offset + n as u64;
            let done = next_off >= total;
            let msg = Msg::UpdateSlice {
                from_site: my_site,
                to_site,
                version: manifest.version.clone(),
                offset,
                total,
                data_b64: sheetcore::wire::b64_encode(&buf[..n]),
                done,
            };
            let Ok(json) = serde_json::to_string(&msg) else {
                break;
            };
            if !send_frame(json) {
                break;
            }
            offset = next_off;
            if done {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
}

fn accept_incoming_update_slice(
    dl_slot: &Arc<Mutex<Option<ActivePeerDownload>>>,
    evt_tx: &EvtTx,
    from_site: u32,
    version: &str,
    offset: u64,
    total: u64,
    data_b64: &str,
    done: bool,
) {
    let Some(chunk) = sheetcore::wire::b64_decode(data_b64) else {
        let mut g = dl_slot.lock().unwrap();
        if let Some(dl) = g.take() {
            let _ = std::fs::remove_file(&dl.part_path);
            let _ = evt_tx.send(NetEvent::PeerUpdateFailed("更新分片解码失败".into()));
        }
        return;
    };

    let finished = {
        let mut g = dl_slot.lock().unwrap();
        let Some(dl) = g.as_mut() else {
            return;
        };
        if dl.peer_site != from_site || dl.manifest.version != version {
            return;
        }
        if offset != dl.written
            || total < dl.manifest.size
            || total > 256 * 1024 * 1024 + 16_384
            || dl.written.saturating_add(chunk.len() as u64) > total
        {
            let bad = g.take().unwrap();
            drop(bad.file);
            let _ = std::fs::remove_file(&bad.part_path);
            let _ = evt_tx.send(NetEvent::PeerUpdateFailed("更新数据偏移或大小异常".into()));
            return;
        }
        if dl.file.write_all(&chunk).is_err() {
            let bad = g.take().unwrap();
            drop(bad.file);
            let _ = std::fs::remove_file(&bad.part_path);
            let _ = evt_tx.send(NetEvent::PeerUpdateFailed("写入本地更新缓存失败".into()));
            return;
        }
        dl.written += chunk.len() as u64;
        let pct = (dl.written.saturating_mul(100)) / total.max(1);
        if done || pct >= dl.last_pct.saturating_add(2) {
            dl.last_pct = pct;
            let _ = evt_tx.send(NetEvent::PeerUpdateProgress {
                received: dl.written,
                total,
            });
        }
        if done {
            if dl.written != total {
                let bad = g.take().unwrap();
                drop(bad.file);
                let _ = std::fs::remove_file(&bad.part_path);
                let _ = evt_tx.send(NetEvent::PeerUpdateFailed("更新包未完整接收".into()));
                return;
            }
            g.take()
        } else {
            None
        }
    };

    if let Some(mut completed) = finished {
        let _ = completed.file.flush();
        drop(completed.file);
        let evt_tx = evt_tx.clone();
        std::thread::spawn(move || {
            match crate::update::verify_and_finalize_staged_exe(
                &completed.part_path,
                &completed.manifest,
            ) {
                Ok(()) => {
                    let verified_path = crate::update::staging_file_path(
                        &completed.manifest.version,
                        "peer.verified.exe",
                    )
                    .unwrap_or_else(|_| completed.part_path.with_extension("verified.exe"));
                    let _ = std::fs::remove_file(&verified_path);
                    match std::fs::rename(&completed.part_path, &verified_path) {
                        Ok(()) => {
                            let _ = evt_tx.send(NetEvent::PeerUpdateReady {
                                version: completed.manifest.version,
                                verified_exe: verified_path,
                            });
                        }
                        Err(e) => {
                            let _ = std::fs::remove_file(&completed.part_path);
                            let _ = evt_tx.send(NetEvent::PeerUpdateFailed(format!(
                                "保存已验证更新包失败：{e}"
                            )));
                        }
                    }
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&completed.part_path);
                    let _ = evt_tx.send(NetEvent::PeerUpdateFailed(e));
                }
            }
        });
    }
}

#[derive(Clone)]
struct EvtTx {
    tx: Sender<NetEvent>,
    wake: Arc<Mutex<Option<eframe::egui::Context>>>,
}

impl EvtTx {
    fn new(tx: Sender<NetEvent>) -> (Self, Arc<Mutex<Option<eframe::egui::Context>>>) {
        let wake = Arc::new(Mutex::new(None));
        (Self { tx, wake: wake.clone() }, wake)
    }

    fn send(&self, ev: NetEvent) -> Result<(), std::sync::mpsc::SendError<NetEvent>> {
        let res = self.tx.send(ev);
        if res.is_ok() {
            if let Ok(guard) = self.wake.lock() {
                if let Some(ctx) = guard.as_ref() {
                    ctx.request_repaint();
                }
            }
        }
        res
    }
}

type SharedStyles = Arc<Mutex<std::collections::BTreeMap<(usize, u32, u32), Arc<sheetcore::style::CellStyle>>>>;

pub struct Net {
    pub site: u32,
    cmd: Sender<Cmd>,
    pub events: Receiver<NetEvent>,
    pub hosting: bool,
    pub port: u16,
    pub addr: String,
    pub session_id: String,
    pub started_ms: u64,
    /// 宿主写进占用卡的自动协作房间密钥（只在宿主侧、且未设房间口令时非空）。
    /// 每次刷新占用卡都要原样写回，否则后来的同事读不到。
    pub lock_secret: String,
    alive: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    wake_ctx: Arc<Mutex<Option<eframe::egui::Context>>>,
    sockets: Arc<Mutex<Vec<Weak<TcpStream>>>>,
    state: Option<Arc<Mutex<sheetcore::sync::CellOps>>>,
    delta_ops: Option<Arc<Mutex<sheetcore::sync::CellOps>>>,
    host_base_hash: Option<Arc<Mutex<String>>>,
    host_roster: Option<Arc<Mutex<RoomRoster>>>,
    room_order: Option<Arc<Mutex<()>>>,
    #[cfg(test)]
    after_snapshot: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    /// 宿主自己也是参与者：本地租约表 + 光标表 + 事件入口
    host_locks: Option<Arc<Mutex<sheetcore::sync::LockTable>>>,
    host_cursors: Option<Arc<Mutex<HashMap<u32, Cursor>>>>,
    host_styles:
        Option<SharedStyles>,
    delta_styles:
        Option<SharedStyles>,
    delta_merges: Option<Arc<Mutex<Vec<(usize, u32, u32, u32, u32, bool)>>>>,
    host_structs: Option<Arc<Mutex<Vec<(usize, sheetcore::structural::Structural)>>>>,
    peer_update_dl: Arc<Mutex<Option<ActivePeerDownload>>>,
    update_seed_override: SharedUpdateSeed,
    evt_tx: EvtTx,
    t0: std::time::Instant,
}

impl Drop for Net {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn track_socket(
    stream: TcpStream, sockets: &Mutex<Vec<Weak<TcpStream>>>, alive: &AtomicBool,
) -> Option<Arc<TcpStream>> {
    let mut active = sockets.lock().unwrap();
    if !alive.load(Ordering::SeqCst) {
        return None;
    }
    active.retain(|s| s.strong_count() > 0);
    let stream = Arc::new(stream);
    active.push(Arc::downgrade(&stream));
    Some(stream)
}

fn write_line(stream: &mut TcpStream, s: &str) -> std::io::Result<()> {
    stream.write_all(s.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// 认证前单行上限：挑战与 Hello 报文都很短，局域网里的陌生连接不该让宿主攒内存
const MAX_PREAUTH_LINE: usize = 64 * 1024;
/// 认证后单行上限（整表快照分片、批量样式等大报文也在其内）
const MAX_LINE: usize = 256 * 1024 * 1024;
/// 宿主同时保持的连接数上限（含未认证的）
const MAX_CONNECTIONS: usize = 64;
/// 建连后多久不完成 Hello 就断开
const PREAUTH_TIMEOUT: Duration = Duration::from_secs(10);

/// 读一行（去掉行尾 `\n` / `\r\n`）。超过 `max` 字节立即报错断开，不无限攒内存；
/// 对端关闭且没有残留数据时返回 `Ok(None)`。
fn read_line_bounded<R: BufRead>(r: &mut R, max: usize) -> std::io::Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let avail = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if avail.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        if let Some(i) = avail.iter().position(|&b| b == b'\n') {
            if buf.len() + i > max {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "报文过长"));
            }
            buf.extend_from_slice(&avail[..i]);
            r.consume(i + 1);
            break;
        }
        let n = avail.len();
        if buf.len() + n > max {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "报文过长"));
        }
        buf.extend_from_slice(avail);
        r.consume(n);
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "报文不是 UTF-8"))
}

/// 连接计数守卫：连接线程结束时自动归还名额
struct ConnSlot(Arc<std::sync::atomic::AtomicUsize>);

impl ConnSlot {
    fn try_take(counter: &Arc<std::sync::atomic::AtomicUsize>) -> Option<Self> {
        let mut cur = counter.load(Ordering::SeqCst);
        loop {
            if cur >= MAX_CONNECTIONS {
                return None;
            }
            match counter.compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return Some(Self(counter.clone())),
                Err(now) => cur = now,
            }
        }
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 结构改动后，宿主的操作日志、样式表、租约表与光标表都要同步平移坐标，
/// 否则后来加入的人或在线持锁者会停留在旧坐标。
fn upsert_lww_op(map: &mut sheetcore::sync::CellOps, op: Arc<Op>) {
    let key = (op.sheet, op.row, op.col);
    match map.get(&key) {
        Some(cur) if (op.ts, op.site) <= (cur.ts, cur.site) => {}
        _ => {
            map.insert(key, op);
        }
    }
}

fn update_host_ops(state: &Mutex<sheetcore::sync::CellOps>, delta: &Mutex<sheetcore::sync::CellOps>, ops: &[Op]) {
    let mut state = state.lock().unwrap();
    let mut delta = delta.lock().unwrap();
    for op in ops {
        let op = Arc::new(op.clone());
        upsert_lww_op(&mut state, Arc::clone(&op));
        upsert_lww_op(&mut delta, op);
    }
}

fn update_host_styles(state: &SharedStyles, delta: &SharedStyles, sheet: usize,
                      items: &[(u32, u32, sheetcore::style::CellStyle)]) {
    let mut state = state.lock().unwrap();
    let mut delta = delta.lock().unwrap();
    for (row, col, style) in items {
        let key = (sheet, *row, *col);
        let style = Arc::new(style.clone());
        state.insert(key, Arc::clone(&style));
        delta.insert(key, style);
    }
}

fn shift_state(
    state: &Arc<Mutex<sheetcore::sync::CellOps>>,
    delta_ops: &Arc<Mutex<sheetcore::sync::CellOps>>,
    base_hash: &Arc<Mutex<String>>,
    styles: &SharedStyles,
    delta_styles: &SharedStyles,
    locks: &Arc<Mutex<sheetcore::sync::LockTable>>,
    cursors: &Arc<Mutex<HashMap<u32, Cursor>>>,
    sheet: usize,
    op: &sheetcore::structural::Structural,
) {
    let map = |r: u32, c: u32| sheetcore::sync::shift_structural_coord(r, c, op);
    base_hash.lock().unwrap().clear();
    sheetcore::sync::shift_cell_ops(&mut state.lock().unwrap(), sheet, op);
    sheetcore::sync::shift_cell_ops(&mut delta_ops.lock().unwrap(), sheet, op);
    for styles in [styles, delta_styles] {
        sheetcore::sync::shift_cell_map(&mut styles.lock().unwrap(), sheet, op, |_, _, _| {});
    }
    locks.lock().unwrap().shift_structural(sheet, op);
    {
        let mut cm = cursors.lock().unwrap();
        for cur in cm.values_mut() {
            if cur.sheet == sheet {
                if let Some((nr, nc)) = map(cur.row, cur.col) {
                    cur.row = nr;
                    cur.col = nc;
                }
            }
        }
    }
}

// Allow for the authentication tag, sequence number and Base64 expansion.
const MAX_PLAIN_LINE: usize = MAX_LINE / 4 * 3 - 64;

/// Each batch uses the original StyleSnapshot message so older clients can apply it.
fn send_snapshot_styles(items: Vec<((usize, u32, u32), Arc<sheetcore::style::CellStyle>)>,
                        tx: &std::sync::mpsc::SyncSender<String>) -> bool {
    style_frames(&items, MAX_PLAIN_LINE, |batch| {
        serde_json::to_string(&Msg::StyleSnapshot {
            items: batch.iter().map(|&((sheet, row, col), ref style)|
                (sheet, row, col, style.as_ref().clone())).collect(),
        })
    }, |line| tx.send(line).is_ok())
}

fn style_frames<T>(items: &[T], max_bytes: usize, encode: impl Fn(&[T]) -> serde_json::Result<String>,
                   mut send: impl FnMut(String) -> bool) -> bool {
    let mut from = 0;
    while from < items.len() {
        let mut count = (items.len() - from).min(256);
        loop {
            let Ok(line) = encode(&items[from..from + count]) else { return false };
            if line.len() <= max_bytes {
                if !send(line) { return false; }
                from += count;
                break;
            }
            if count == 1 { return false; }
            count /= 2;
        }
    }
    true
}

// A live edit and its rollback each use one message whenever the protocol permits
// it. This preserves the lock check and the receiver's single history entry.
fn send_room_message(msg: Msg, limit: usize, mut send: impl FnMut(String) -> bool,
                     on_split: impl FnOnce()) -> bool {
    struct Frame { data: Vec<u8>, limit: usize }
    impl Write for Frame {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit - self.data.len() {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            self.data.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    let count = match &msg {
        Msg::StyleOp { items, .. } => items.len(),
        Msg::WriteDeny { revert_styles, revert_ops, revert_merge, .. }
            if revert_ops.is_empty() && revert_merge.is_none() => revert_styles.len(),
        _ => return serde_json::to_string(&msg).is_ok_and(send),
    };
    if count <= sheetcore::sync::MAX_BATCH_ITEMS {
        let mut frame = Frame { data: Vec::new(), limit };
        if serde_json::to_writer(&mut frame, &msg).is_ok() {
            return send(String::from_utf8(frame.data).expect("JSON is UTF-8"));
        }
    }
    on_split();
    match msg {
        Msg::StyleOp { site, sheet, ts, items } => style_frames(&items, limit, |batch| {
            serde_json::to_string(&Msg::StyleOp { site, sheet, ts, items: batch.to_vec() })
        }, send),
        Msg::WriteDeny { holder, holder_name, sheet, row, col, revert_styles, .. } =>
            style_frames(&revert_styles, limit, |batch| serde_json::to_string(&Msg::WriteDeny {
                holder, holder_name: holder_name.clone(), sheet, row, col,
                revert_ops: Vec::new(), revert_styles: batch.to_vec(), revert_merge: None,
            }), send),
        _ => unreachable!(),
    }
}

fn latest_cell_op(
    state: &Mutex<sheetcore::sync::CellOps>,
    sheet: usize,
    row: u32,
    col: u32,
) -> Option<Op> {
    state.lock().unwrap().get(&(sheet, row, col)).map(|op| op.as_ref().clone())
}

/// 入房证明的定长比较（避免按字节提前退出泄露时序）
fn proof_matches(got: &str, expect: &str) -> bool {
    let (a, b) = (got.as_bytes(), expect.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

impl Net {
    pub fn is_live(&self) -> bool {
        self.hosting || self.connected.load(Ordering::SeqCst)
    }

    pub fn set_wake_ctx(&self, ctx: &eframe::egui::Context) {
        if let Ok(mut guard) = self.wake_ctx.lock() {
            if guard.is_none() {
                *guard = Some(ctx.clone());
            }
        }
    }

    /// 宿主启动时一次性装填初始快照、样式与光标，避免对成千上万格逐条加锁与序列化阻塞界面线程
    pub fn seed_snapshot(
        &self,
        peer: &sheetcore::sync::Peer,
        workbook: &sheetcore::Workbook,
        mut host_cursor: Cursor,
    ) {
        if let Some(st) = &self.state {
            let mut map = st.lock().unwrap();
            map.clone_from(&peer.cells);
        }
        if let Some(d) = &self.delta_ops {
            d.lock().unwrap().clear();
        }
        if let Some(ds) = &self.delta_styles {
            ds.lock().unwrap().clear();
        }
        if let Some(dm) = &self.delta_merges {
            dm.lock().unwrap().clear();
        }
        if let Some(hs) = &self.host_styles {
            let mut map = hs.lock().unwrap();
            let mut palette = HashMap::new();
            for (index, sheet) in workbook.sheets.iter().enumerate() {
                for (pos, &id) in sheet.styles.iter().filter(|(_, id)| **id != 0) {
                    let style = palette.entry(id).or_insert_with(|| Arc::new(workbook.styles.get(id).clone()));
                    map.insert((index, pos.row, pos.col), Arc::clone(style));
                }
            }
        }
        host_cursor.site = self.site;
        host_cursor.color = (self.site % 7) as u8;
        if let Some(roster) = &self.host_roster {
            host_cursor.name = roster.lock().unwrap().name_of(self.site);
        }
        if let Some(hc) = &self.host_cursors {
            hc.lock().unwrap().insert(host_cursor.site, host_cursor);
        }
    }
    pub fn set_base_hash(&self, hash: String) {
        if let Some(bh) = &self.host_base_hash {
            *bh.lock().unwrap() = hash;
        }
    }

    fn host_now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }

    fn expire_host_locks(&self) {
        let Some(locks) = &self.host_locks else { return };
        let dead = locks.lock().unwrap().expire(self.host_now_ms());
        for (sheet, row, col, site) in dead {
            if let Ok(s) = serde_json::to_string(&Msg::LockRelease { site, sheet, row, col }) {
                let _ = self.cmd.send(Cmd::Broadcast(s));
            }
            let _ = self.evt_tx.send(NetEvent::LockRelease { site, sheet, row, col });
        }
    }

    /// 宿主侧直接查询权威租约表，消除 UI 线程与网络线程之间的事件延迟窗口
    pub fn host_lock_conflict_cells(
        &self,
        sheet: usize,
        keys: &[(u32, u32)],
    ) -> Option<(u32, String, u32, u32)> {
        let locks = self.host_locks.as_ref()?;
        self.expire_host_locks();
        let now = self.host_now_ms();
        let lt = locks.lock().unwrap();
        for (r, c) in keys {
            if let Some(h) = lt.holder_other(sheet, *r, *c, self.site, now) {
                return Some((h.site, h.name.clone(), *r, *c));
            }
        }
        None
    }

    pub fn host_lock_conflict_rect(
        &self,
        sheet: usize,
        rect: (u32, u32, u32, u32),
    ) -> Option<(u32, String, u32, u32)> {
        let locks = self.host_locks.as_ref()?;
        self.expire_host_locks();
        let now = self.host_now_ms();
        let lt = locks.lock().unwrap();
        lt.conflicts_rect(sheet, rect.0, rect.1, rect.2, rect.3, self.site, now)
            .map(|((r, c), h)| (h.site, h.name.clone(), r, c))
    }

    pub fn host_lock_conflict_structural(
        &self,
        sheet: usize,
        op: &sheetcore::structural::Structural,
    ) -> Option<(u32, String, u32, u32)> {
        let locks = self.host_locks.as_ref()?;
        self.expire_host_locks();
        let now = self.host_now_ms();
        let lt = locks.lock().unwrap();
        lt.conflicts_structural(sheet, op, self.site, now)
            .map(|((r, c), h)| (h.site, h.name.clone(), r, c))
    }

    /// 测试用：把一条事件塞进本连接的事件队列，模拟网络线程产生的事件（例如重连后的 Welcome）
    #[cfg(test)]
    pub(crate) fn inject_event_for_test(&self, ev: NetEvent) {
        let _ = self.evt_tx.send(ev);
    }

    pub fn broadcast_op(&self, op: Op) {
        self.broadcast_ops(vec![op]);
    }

    pub fn broadcast_ops(&self, mut ops: Vec<Op>) {
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        self.expire_host_locks();
        let locks = self.host_locks.as_ref().map(|table| table.lock().unwrap());
        let now = self.host_now_ms();
        ops.retain_mut(|op| {
            op.site = self.site;
            if let Some(holder) = locks.as_ref().and_then(|lt| lt.holder_other(op.sheet, op.row, op.col, self.site, now)) {
                let prev = self.state.as_ref().and_then(|st| latest_cell_op(st, op.sheet, op.row, op.col));
                let _ = self.evt_tx.send(NetEvent::WriteRejected {
                    holder: holder.site, holder_name: holder.name.clone(),
                    sheet: op.sheet, row: op.row, col: op.col,
                    revert_ops: vec![(op.row, op.col, prev)], revert_styles: Vec::new(), revert_merge: None,
                });
                return false;
            }
            true
        });
        drop(locks);
        if ops.is_empty() { return; }
        if let (Some(state), Some(delta)) = (&self.state, &self.delta_ops) {
            update_host_ops(state, delta, &ops);
        }
        if let Some(roster) = &self.host_roster {
            queue_room_change(&self.cmd, roster, None, RoomChange::Cells(ops));
        } else {
            let _ = self.cmd.send(Cmd::Cells(ops));
        }
    }

    /// 请求编辑某格
    pub fn request_lock(&self, sheet: usize, row: u32, col: u32, name: &str) {
        // 宿主自己也是参与者：走本地租约表，否则「自己锁不住自己」
        if let Some(locks) = &self.host_locks {
            self.expire_host_locks();
            let auth_name = self
                .host_roster
                .as_ref()
                .map(|r| r.lock().unwrap().name_of(self.site))
                .unwrap_or_else(|| clean_member_name(name, self.site));
            let now = self.host_now_ms();
            let (outcome, evicted) = {
                let mut lt = locks.lock().unwrap();
                let outcome = lt.request(sheet, row, col, self.site, &auth_name, now);
                let evicted = match outcome {
                    sheetcore::sync::LockOutcome::Denied { .. } => Vec::new(),
                    _ => lt.release_all_except(self.site, (sheet, row, col)),
                };
                (outcome, evicted)
            };
            for (psh, pr, pc) in evicted {
                if let Ok(s) = serde_json::to_string(&Msg::LockRelease {
                    site: self.site,
                    sheet: psh,
                    row: pr,
                    col: pc,
                }) {
                    let _ = self.cmd.send(Cmd::Broadcast(s));
                }
                let _ = self.evt_tx.send(NetEvent::LockRelease {
                    site: self.site,
                    sheet: psh,
                    row: pr,
                    col: pc,
                });
            }
            match outcome {
                sheetcore::sync::LockOutcome::Denied { holder, name: hn } => {
                    let _ = self.evt_tx.send(NetEvent::LockDeny {
                        holder,
                        holder_name: hn,
                        sheet,
                        row,
                        col,
                    });
                }
                _ => {
                    let msg = Msg::LockOk {
                        site: self.site,
                        name: auth_name.clone(),
                        sheet,
                        row,
                        col,
                    };
                    if let Ok(s) = serde_json::to_string(&msg) {
                        let _ = self.cmd.send(Cmd::Broadcast(s));
                    }
                    let _ = self.evt_tx.send(NetEvent::LockOk {
                        site: self.site,
                        name: auth_name,
                        sheet,
                        row,
                        col,
                    });
                }
            }
            return;
        }
        let msg = Msg::LockReq {
            site: self.site,
            name: name.to_string(),
            sheet,
            row,
            col,
        };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    pub fn release_lock(&self, sheet: usize, row: u32, col: u32) {
        if let Some(locks) = &self.host_locks {
            if !locks.lock().unwrap().release(sheet, row, col, self.site) {
                return;
            }
        }
        let msg = Msg::LockRelease { site: self.site, sheet, row, col };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    /// 客户端请求宿主代为将权威合并状态写入共享原文件
    pub fn request_save(&self) {
        if self.hosting {
            return;
        }
        let msg = Msg::SaveReq { site: self.site };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    /// 宿主保存共享原文件成功后广播通知全员
    pub fn broadcast_saved(&self, clock: u64) {
        if !self.hosting {
            return;
        }
        let msg = Msg::Saved { site: self.site, clock };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    /// 广播格式变更（宿主顺手记进样式表，供后来的人补）
    pub fn broadcast_style(&self, sheet: usize, items: Vec<(u32, u32, sheetcore::style::CellStyle)>) {
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        if items.is_empty() {
            return;
        }
        let keys: Vec<(u32, u32)> = items.iter().map(|(r, c, _)| (*r, *c)).collect();
        if let Some((holder, holder_name, row, col)) = self.host_lock_conflict_cells(sheet, &keys) {
            let revert_styles = if let Some(st) = &self.host_styles {
                let m = st.lock().unwrap();
                keys.iter().map(|(r, c)| (*r, *c, m.get(&(sheet, *r, *c)).map(|s| s.as_ref().clone()))).collect()
            } else {
                Vec::new()
            };
            let _ = self.evt_tx.send(NetEvent::WriteRejected {
                holder,
                holder_name,
                sheet,
                row,
                col,
                revert_ops: Vec::new(),
                revert_styles,
                revert_merge: None,
            });
            return;
        }
        if let (Some(state), Some(delta)) = (&self.host_styles, &self.delta_styles) {
            update_host_styles(state, delta, sheet, &items);
        }
        let msg = Msg::StyleOp { site: self.site, sheet, ts: 0, items };
        if let Some(roster) = &self.host_roster {
            queue_room_change(&self.cmd, roster, None, RoomChange::Message(msg));
        } else {
            let _ = self.cmd.send(Cmd::Style(msg));
        }
    }

    /// 广播合并/取消合并
    pub fn broadcast_merge(&self, sheet: usize, rect: (u32, u32, u32, u32), on: bool) {
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        if let Some((holder, holder_name, row, col)) = self.host_lock_conflict_rect(sheet, rect) {
            let _ = self.evt_tx.send(NetEvent::WriteRejected {
                holder,
                holder_name,
                sheet,
                row,
                col,
                revert_ops: Vec::new(),
                revert_styles: Vec::new(),
                revert_merge: Some((rect.0, rect.1, rect.2, rect.3, !on)),
            });
            return;
        }
        if let Some(dm) = &self.delta_merges {
            dm.lock().unwrap().push((sheet, rect.0, rect.1, rect.2, rect.3, on));
        }
        let msg = Msg::MergeOp {
            site: self.site,
            sheet,
            ts: 0,
            r1: rect.0,
            c1: rect.1,
            r2: rect.2,
            c2: rect.3,
            on,
        };
        if let Some(roster) = &self.host_roster {
            queue_room_change(&self.cmd, roster, None, RoomChange::Message(msg));
        } else if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    /// 广播结构性改动（宿主顺手把本地快照日志、租约与光标也搬家）
    pub fn broadcast_structural(&self, sheet: usize, op: &sheetcore::structural::Structural) -> Result<(), String> {
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        if let Some((_, name, row, col)) = self.host_lock_conflict_structural(sheet, op) {
            return Err(format!("{name} 正在编辑 {}，请待其完成后再{}", sheetcore::model::cell_name(row, col), op.label()));
        }
        if !self.alive.load(Ordering::SeqCst) || !self.is_live() {
            return Err("协作连接已断开，结构修改尚未执行".into());
        }
        let msg = Msg::Structural { site: self.site, sheet, op: op.clone() };
        let queued = if let Some(roster) = &self.host_roster {
            queue_room_change(&self.cmd, roster, None, RoomChange::Message(msg))
        } else {
            serde_json::to_string(&msg).is_ok_and(|s| self.cmd.send(Cmd::Broadcast(s)).is_ok())
        };
        if !queued { return Err("协作发送队列已关闭，结构修改尚未执行".into()); }
        if let (Some(state), Some(delta_ops), Some(bh), Some(st), Some(ds), Some(locks), Some(cursors)) = (
            &self.state,
            &self.delta_ops,
            &self.host_base_hash,
            &self.host_styles,
            &self.delta_styles,
            &self.host_locks,
            &self.host_cursors,
        ) {
            let before = state.lock().unwrap().len();
            shift_state(state, delta_ops, bh, st, ds, locks, cursors, sheet, op);
            if std::env::var("SC_DBG").is_ok() {
                let g = state.lock().unwrap();
                let r0 = g.values().filter(|o| o.row == 0).count();
                let r2 = g.values().filter(|o| o.row == 2).count();
                eprintln!("[dbg] shift_state: {before} 条，row0={r0} row2={r2}");
            }
        }
        if let Some(h) = &self.host_structs {
            h.lock().unwrap().push((sheet, op.clone()));
        }
        Ok(())
    }

    pub fn broadcast_cursor(&self, mut cur: Cursor) {
        cur.site = self.site;
        cur.color = (self.site % 7) as u8;
        if let Some(roster) = &self.host_roster {
            cur.name = roster.lock().unwrap().name_of(self.site);
        }
        if let Some(hc) = &self.host_cursors {
            hc.lock().unwrap().insert(cur.site, cur.clone());
        }
        if let Ok(s) = serde_json::to_string(&Msg::CursorMsg(cur)) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    /// 在线改显示名：客户端发给宿主，宿主做房间内唯一性消歧后转发给其他客户端；
    /// 本机也收一条事件让成员列表立刻更新。协议不改变 site，已有连接不需要重连。
    pub fn rename(&self, name: &str) {
        let canonical = if let Some(roster) = &self.host_roster {
            roster.lock().unwrap().rename_member(self.site, name)
        } else {
            clean_member_name(name, self.site)
        };
        if let Some(locks) = &self.host_locks {
            locks.lock().unwrap().rename_site(self.site, &canonical);
        }
        if let Some(hc) = &self.host_cursors {
            if let Some(cur) = hc.lock().unwrap().get_mut(&self.site) {
                cur.name = canonical.clone();
            }
        }
        let msg = Msg::Rename { site: self.site, name: canonical.clone() };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
        let _ = self.evt_tx.send(NetEvent::PeerRenamed(self.site, canonical));
    }

    /// 向房间成员宣告本机持有经过官方数字签名的版本（若本机为正式签名包）
    pub fn announce_local_signed_update(&self) {
        let Some((_, manifest)) = resolve_local_seed(&self.update_seed_override) else {
            return;
        };
        self.announce_update(&manifest);
    }

    /// 向指定新加入成员单播本机的签名版本宣告（宿主在新成员入房后调用）
    pub fn announce_local_signed_update_to(&self, target_site: u32) {
        let Some((_, manifest)) = resolve_local_seed(&self.update_seed_override) else {
            return;
        };
        let msg = Msg::UpdateAnnounce {
            site: self.site,
            version: manifest.version,
            size: manifest.size,
            sha256: manifest.sha256,
            sig: manifest.sig,
            notes: manifest.notes,
        };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::SendTo(target_site, s));
        }
    }

    pub fn announce_update(&self, manifest: &crate::update::SignedManifest) {
        if !crate::update::verify_manifest_sig(manifest) {
            return;
        }
        let msg = Msg::UpdateAnnounce {
            site: self.site,
            version: manifest.version.clone(),
            size: manifest.size,
            sha256: manifest.sha256.clone(),
            sig: manifest.sig.clone(),
            notes: manifest.notes.clone(),
        };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    /// 向房间中持有新版签名包的成员发起拉取请求（无需对方额外监听新端口，不触发防火墙弹窗）
    pub fn request_peer_update(
        &self,
        peer_site: u32,
        manifest: &crate::update::SignedManifest,
    ) -> Result<(), String> {
        if peer_site == 0 || peer_site == self.site {
            return Err("无效的协作更新节点".into());
        }
        if !crate::update::verify_manifest_sig(manifest) {
            return Err("更新清单签名无效，已拒绝请求".into());
        }
        let part_path = crate::update::staging_file_path(&manifest.version, "peer.part")
            .map_err(|e| format!("无法创建更新缓存目录：{e}"))?;
        let _ = std::fs::remove_file(&part_path);
        let file = std::fs::File::create(&part_path)
            .map_err(|e| format!("无法创建临时更新文件：{e}"))?;
        {
            let mut g = self.peer_update_dl.lock().unwrap();
            if let Some(prev) = g.take() {
                drop(prev.file);
                let _ = std::fs::remove_file(&prev.part_path);
            }
            *g = Some(ActivePeerDownload {
                peer_site,
                manifest: manifest.clone(),
                file,
                part_path,
                written: 0,
                last_pct: 0,
            });
        }
        let req = Msg::UpdatePull {
            from_site: self.site,
            to_site: peer_site,
            version: manifest.version.clone(),
        };
        let s = serde_json::to_string(&req).map_err(|e| format!("序列化更新请求失败：{e}"))?;
        self.cmd
            .send(Cmd::SendTo(peer_site, s))
            .map_err(|_| "协作连接已关闭".to_string())
    }

    #[cfg(test)]
    pub fn set_update_bundle_for_test(
        &self,
        exe_path: std::path::PathBuf,
        manifest: crate::update::SignedManifest,
    ) {
        *self.update_seed_override.lock().unwrap() = Some((exe_path, manifest));
    }

    pub fn shutdown(&self) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        for socket in self.sockets.lock().unwrap().drain(..).filter_map(|s| s.upgrade()) {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        let _ = self.cmd.send(Cmd::Stop);
    }
}

/// 启动宿主：监听指定端口，校验口令，转发所有操作
pub fn host(
    site: u32,
    name: String,
    doc: String,
    port: u16,
    password: String,
) -> std::io::Result<Net> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    host_on(listener, site, name, doc, password)
}

pub fn host_with_session(
    site: u32,
    name: String,
    doc: String,
    port: u16,
    password: String,
    session_id: String,
    started_ms: u64,
) -> std::io::Result<Net> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    host_on_with_session(listener, site, name, doc, password, session_id, started_ms)
}

/// 启动宿主（支持多工作簿/同机多实例端口自动顺延）
pub fn host_auto(
    site: u32,
    name: String,
    doc: String,
    password: String,
) -> std::io::Result<Net> {
    host_auto_with_session(
        site,
        name,
        doc,
        password,
        crate::session_lock::new_session_id(),
        crate::session_lock::now_ms(),
    )
}

pub fn host_auto_with_session(
    site: u32, name: String, doc: String, password: String, session_id: String, started_ms: u64,
) -> std::io::Result<Net> {
    host_auto_initialized(site, name, doc, password, session_id, started_ms, |_| {})
}

pub fn host_auto_initialized(
    site: u32, name: String, doc: String, password: String, session_id: String, started_ms: u64,
    initialize: impl FnOnce(&Net),
) -> std::io::Result<Net> {
    let listener = if let Some(port) = std::env::var("SC_NET_PORT").ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
    {
        TcpListener::bind(("0.0.0.0", port))?
    } else {
        let mut candidate = None;
        let mut last_error = None;
        for port in DEFAULT_PORT..=DEFAULT_PORT.saturating_add(20) {
            match TcpListener::bind(("0.0.0.0", port)) {
                Ok(listener) => { candidate = Some(listener); break; }
                Err(error) => last_error = Some(error),
            }
        }
        match candidate {
            Some(listener) => listener,
            None => TcpListener::bind(("0.0.0.0", 0)).map_err(|e| last_error.unwrap_or(e))?,
        }
    };
    host_initialized(listener, site, name, doc, password, session_id, started_ms, initialize)
}

/// 用调用方已经绑定好的 listener 起宿主。测试用它绑 `0.0.0.0:0`——
/// 既走真实的通配地址，又消灭「先探测端口、放开、再绑」的 TOCTOU 窗口。
pub fn host_on(
    listener: TcpListener,
    site: u32,
    name: String,
    doc: String,
    password: String,
) -> std::io::Result<Net> {
    host_on_with_session(
        listener,
        site,
        name,
        doc,
        password,
        crate::session_lock::new_session_id(),
        crate::session_lock::now_ms(),
    )
}

pub fn host_on_with_session(
    listener: TcpListener,
    site: u32,
    name: String,
    doc: String,
    password: String,
    session_id: String,
    started_ms: u64,
) -> std::io::Result<Net> {
    host_initialized(listener, site, name, doc, password, session_id, started_ms, |_| {})
}

fn host_initialized(
    listener: TcpListener, site: u32, name: String, doc: String, password: String,
    session_id: String, started_ms: u64, initialize: impl FnOnce(&Net),
) -> std::io::Result<Net> {
    if site == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "宿主站点号不能为 0",
        ));
    }
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    // 协议 v2：开房时随机生成房间 salt，口令只在这里拉伸一次（PBKDF2，故意慢）
    let mut room_salt = [0u8; 16];
    getrandom::fill(&mut room_salt)
        .map_err(|e| std::io::Error::other(format!("系统随机数不可用：{e}")))?;
    let room_key = sheetcore::wire::RoomKey::derive(&password, &room_salt);
    let room_salt_b64 = sheetcore::wire::b64_encode(&room_salt);
    drop(password);
    let sockets = Arc::new(Mutex::new(Vec::new()));
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (evt_raw_tx, evt_rx) = channel::<NetEvent>();
    let (evt_tx, wake_ctx) = EvtTx::new(evt_raw_tx);
    let alive = Arc::new(AtomicBool::new(true));
    let connected = Arc::new(AtomicBool::new(true));

    let roster = Arc::new(Mutex::new(RoomRoster::new(site, name)));
    let roster_for_self = roster.clone();
    let room_order = Arc::new(Mutex::new(()));
    #[cfg(test)]
    let after_snapshot: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>> = Default::default();
    let state: Arc<Mutex<sheetcore::sync::CellOps>> = Default::default();
    let delta_ops: Arc<Mutex<sheetcore::sync::CellOps>> = Default::default();
    let host_base_hash: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    // 广播与定向单播分发
    {
        let roster = roster.clone();
        let dispatch_events = evt_tx.clone();
        let alive = alive.clone();
        std::thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                if !alive.load(Ordering::SeqCst) {
                    break;
                }
                match cmd {
                    Cmd::Cells(ops) => {
                        let senders = roster.lock().unwrap().all_senders();
                        if !senders.is_empty() {
                            cell_frames(ops, false, true, |s| {
                                for tx in &senders { let _ = tx.send(s.clone()); }
                            });
                        }
                    }
                    Cmd::Style(_) => unreachable!("client style dispatch is guest-only"),
                    Cmd::Deliver(peers, change) => {
                        let send = |line: String| {
                            for peer in &peers { let _ = peer.send(line.clone()); }
                        };
                        match change {
                            RoomChange::Cells(ops) => cell_frames(ops, false, true, send),
                            RoomChange::Message(msg) => {
                                if !send_room_message(msg, MAX_PLAIN_LINE, |line| { send(line); true }, || {}) {
                                    let _ = dispatch_events.send(NetEvent::Error("协作修改未能完整发送".into()));
                                }
                            }
                        }
                    }
                    Cmd::Broadcast(s) => {
                        let senders = roster.lock().unwrap().all_senders();
                        for tx in senders {
                            let _ = tx.send(s.clone());
                        }
                    }
                    Cmd::SendTo(target_site, s) => {
                        let target = roster.lock().unwrap().sender_of(target_site);
                        if let Some(tx) = target {
                            let _ = tx.send(s);
                        }
                    }
                    Cmd::Stop => break,
                }
            }
        });
    }

    // 编辑租约（宿主仲裁，同一格只批一个）
    // 样式表：新加入的人要靠它补齐格式（Op 里只有值）
    let styles: SharedStyles = Default::default();
    let delta_styles: SharedStyles = Default::default();
    let delta_merges: Arc<Mutex<Vec<(usize, u32, u32, u32, u32, bool)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let structs: Arc<Mutex<Vec<(usize, sheetcore::structural::Structural)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let structs_for_self = structs.clone();
    let locks: Arc<Mutex<sheetcore::sync::LockTable>> = Arc::new(Mutex::new(
        sheetcore::sync::LockTable::new(sheetcore::sync::DEFAULT_LEASE_TTL_MS),
    ));
    let cursors: Arc<Mutex<HashMap<u32, Cursor>>> = Arc::new(Mutex::new(HashMap::new()));
    let locks_for_self = locks.clone();
    let cursors_for_self = cursors.clone();
    let styles_for_self = styles.clone();
    let delta_ops_for_self = delta_ops.clone();
    let host_base_hash_for_self = host_base_hash.clone();
    let delta_styles_for_self = delta_styles.clone();
    let delta_merges_for_self = delta_merges.clone();
    let evt_tx_self = evt_tx.clone();
    let t0 = std::time::Instant::now();
    let now_ms = {
        let t0 = t0;
        move || t0.elapsed().as_millis() as u64
    };

    // 租约巡检：断线/崩溃的人 20 秒后自动放锁
    {
        let locks = locks.clone();
        let roster = roster.clone();
        let alive = alive.clone();
        let evt_tx = evt_tx.clone();
        let now_ms = now_ms;
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            if !alive.load(Ordering::SeqCst) {
                break;
            }
            let dead = locks.lock().unwrap().expire(now_ms());
            if dead.is_empty() {
                continue;
            }
            let senders = roster.lock().unwrap().all_senders();
            for (sheet, row, col, site) in dead {
                let line = serde_json::to_string(&Msg::LockRelease { site, sheet, row, col })
                    .unwrap();
                for ptx in &senders {
                    let _ = ptx.send(line.clone());
                }
                let _ = evt_tx.send(NetEvent::LockRelease { site, sheet, row, col });
            }
        });
    }

    let host_update_dl: Arc<Mutex<Option<ActivePeerDownload>>> = Arc::new(Mutex::new(None));
    let host_update_dl_for_self = host_update_dl.clone();
    let host_seed_override: SharedUpdateSeed = Arc::new(Mutex::new(None));
    let host_seed_override_for_self = host_seed_override.clone();
    let host_serving_update = Arc::new(AtomicBool::new(false));
    let host_site_id = site;

    let (ready, initialized) = channel();
    // accept 循环
    {
        let sockets = sockets.clone();
        let room_order = room_order.clone();
        #[cfg(test)]
        let after_snapshot = after_snapshot.clone();
        let cmd = cmd_tx.clone();
        let roster = roster.clone();
        let state = state.clone();
        let delta_ops_outer = delta_ops.clone();
        let host_base_hash_outer = host_base_hash.clone();
        let evt_tx = evt_tx.clone();
        let doc_name = doc.clone();
        let room_salt_b64 = room_salt_b64.clone();
        let alive = alive.clone();
        let locks_outer = locks.clone();
        let cursors_outer = cursors.clone();
        let styles_outer = styles.clone();
        let delta_styles_outer = delta_styles.clone();
        let delta_merges_outer = delta_merges.clone();
        let structs_outer = structs.clone();
        let update_dl_outer = host_update_dl.clone();
        let seed_override_outer = host_seed_override.clone();
        let serving_update_outer = host_serving_update.clone();
        let now_ms_outer = now_ms;
        std::thread::spawn(move || {
            if initialized.recv().is_err() { return; }
            let active_conns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            while alive.load(Ordering::SeqCst) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(40));
                        continue;
                    }
                    Err(_) => break,
                };
                // 连接数封顶：每条连接两个线程，局域网里的扫描器不能把宿主线程耗尽
                let Some(slot) = ConnSlot::try_take(&active_conns) else {
                    drop(stream);
                    continue;
                };
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let _ = stream.set_nodelay(true);
                let Some(stream) = track_socket(stream, &sockets, &alive) else { break };
                let roster = roster.clone();
                let room_order = room_order.clone();
                #[cfg(test)]
                let after_snapshot = after_snapshot.clone();
                let cmd = cmd.clone();
                let state = state.clone();
                let delta_ops = delta_ops_outer.clone();
                let host_base_hash = host_base_hash_outer.clone();
                let evt_tx = evt_tx.clone();
                let doc_name = doc_name.clone();
                let room_salt_b64 = room_salt_b64.clone();
                let locks = locks_outer.clone();
                let cursors = cursors_outer.clone();
                let styles = styles_outer.clone();
                let delta_styles = delta_styles_outer.clone();
                let delta_merges = delta_merges_outer.clone();
                let structs = structs_outer.clone();
                let update_dl = update_dl_outer.clone();
                let seed_override = seed_override_outer.clone();
                let serving_update = serving_update_outer.clone();
                let alive_conn = alive.clone();
                let now_ms = now_ms_outer;
                std::thread::spawn(move || {
                    let _slot = slot;
                    let mut write_half = match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let _ = write_half.set_write_timeout(Some(Duration::from_secs(10)));
                    // 未认证连接限时：建连后 10 秒内不完成 Hello 就断开
                    let _ = stream.set_read_timeout(Some(PREAUTH_TIMEOUT));
                    let mut salt = [0u8; 16];
                    let Ok(nonce) = getrandom::u64() else { return };
                    if getrandom::fill(&mut salt).is_err() {
                        return;
                    }
                    let challenge = Msg::ChallengeSalt {
                        nonce,
                        salt: sheetcore::wire::b64_encode(&salt),
                        room_salt: room_salt_b64,
                    };
                    if write_line(&mut write_half, &serde_json::to_string(&challenge).unwrap()).is_err() {
                        return;
                    }
                    let mut sess = sheetcore::wire::Session::host_side(&room_key, &salt);
                    let mut sender = sess.fork_sender();
                    let reader = BufReader::new(&*stream);
                    let (tx, rx) = channel::<String>();
                    let (intro, intro_rx) = std::sync::mpsc::sync_channel::<String>(4);
                    let mut intro = Some(intro);
                    std::thread::spawn(move || {
                        // Live messages can queue as soon as the snapshot is captured,
                        // but the writer drains the complete introduction before them.
                        for s in intro_rx.into_iter().chain(rx) {
                            let frame = if let Some(denial) = s.strip_prefix('\u{1}') {
                                denial.to_string()
                            } else {
                                sender.seal(&s)
                            };
                            if write_line(&mut write_half, &frame).is_err() {
                                break;
                            }
                        }
                        // 写不出去（对端卡死或超时）就整条连接关掉：读循环随之结束、成员被移出，
                        // 不能留下「还在收他的编辑、却再也发不出更新」的僵尸成员
                        let _ = write_half.shutdown(std::net::Shutdown::Both);
                    });

                    let expire_and_broadcast = || {
                        let dead = locks.lock().unwrap().expire(now_ms());
                        if dead.is_empty() {
                            return;
                        }
                        let senders = roster.lock().unwrap().all_senders();
                        for (sheet, row, col, lsite) in dead {
                            let line = serde_json::to_string(&Msg::LockRelease {
                                site: lsite,
                                sheet,
                                row,
                                col,
                            })
                            .unwrap();
                            for ptx in &senders {
                                let _ = ptx.send(line.clone());
                            }
                            let _ = evt_tx.send(NetEvent::LockRelease {
                                site: lsite,
                                sheet,
                                row,
                                col,
                            });
                        }
                    };

                    let mut hello_site: Option<u32> = None;
                    let mut reader = reader;
                    loop {
                        let limit = if hello_site.is_none() { MAX_PREAUTH_LINE } else { MAX_LINE };
                        let Ok(Some(line)) = read_line_bounded(&mut reader, limit) else { break };
                        let Some(line) = sess.open(&line) else {
                            if hello_site.is_none() {
                                let deny = serde_json::to_string(&Msg::Denied {
                                    reason: "口令不正确或客户端不支持加密协作".into(),
                                }).unwrap();
                                let _ = tx.send(format!("\u{1}{deny}"));
                            }
                            break;
                        };
                        let Ok(msg) = serde_json::from_str::<Msg>(&line) else { continue };
                        // 坐标、数量、时间戳越界的报文整条丢弃，不进宿主状态、不转发
                        if !msg.is_within_limits() {
                            continue;
                        }
                        // A connection can introduce itself once. No workbook frames precede Hello.
                        if matches!(msg, Msg::Hello { .. }) != hello_site.is_none() {
                            break;
                        }
                        match msg {
                            Msg::Hello { site, name, proof: got, doc: _, auth, base_hash: client_hash } => {
                                let live_tx = tx.clone();
                                let tx = intro.take().expect("only one introduction");
                                if !proof_matches(&got, &room_key.proof(nonce)) {
                                    let _ = tx.send(
                                        serde_json::to_string(&Msg::Denied {
                                            reason: "口令不正确".into(),
                                        })
                                        .unwrap(),
                                    );
                                    break;
                                }
                                let order = room_order.lock().unwrap();
                                let admitted = roster.lock().unwrap().admit(site, &name, &auth);
                                let (canonical_name, known) = match admitted {
                                    Ok(pair) => pair,
                                    Err(reason) => {
                                        let _ = tx.send(
                                            serde_json::to_string(&Msg::Denied {
                                                reason: reason.into(),
                                            })
                                            .unwrap(),
                                        );
                                        break;
                                    }
                                };
                                hello_site = Some(site);
                                // 已认证成员可以长时间不说话（只看不改），取消认证前的读超时
                                let _ = stream.set_read_timeout(None);
                                let color = (site % 7) as u8;
                                let hist = structs.lock().unwrap().clone();
                                let can_delta = !client_hash.is_empty()
                                    && hist.is_empty()
                                    && *host_base_hash.lock().unwrap() == client_hash;
                                let (snap, host_clock) = if can_delta {
                                    let dops: Vec<Arc<Op>> = delta_ops.lock().unwrap().values().cloned().collect();
                                    let clk = dops
                                        .iter()
                                        .map(|o| o.ts)
                                        .max()
                                        .unwrap_or_else(|| {
                                            state.lock().unwrap().values().map(|o| o.ts).max().unwrap_or(1)
                                        })
                                        .max(1);
                                    (dops, clk)
                                } else {
                                    let full: Vec<Arc<Op>> = state.lock().unwrap().values().cloned().collect();
                                    let clk = full.iter().map(|o| o.ts).max().unwrap_or(1).max(1);
                                    (full, clk)
                                };
                                let style_source = if can_delta { &delta_styles } else { &styles };
                                let style_snapshot = style_source.lock().unwrap().iter()
                                    .map(|(&key, style)| (key, Arc::clone(style))).collect();
                                let merges = delta_merges.lock().unwrap().clone();
                                expire_and_broadcast();
                                let active_locks: Vec<(usize, u32, u32, u32, String)> = locks
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .map(|((sh, r, c), h)| (sh, r, c, h.site, h.name.clone()))
                                    .collect();
                                let active_cursors: Vec<Cursor> =
                                    cursors.lock().unwrap().values().cloned().collect();
                                let other_peers = roster.lock().unwrap().finish_admit(site, live_tx);
                                drop(order);
                                #[cfg(test)]
                                {
                                    let hook = after_snapshot.lock().unwrap().take();
                                    if let Some(run) = hook { run(); }
                                }
                                let _ = tx.send(
                                    serde_json::to_string(&Msg::Welcome {
                                        site,
                                        doc: doc_name.clone(),
                                        peers: known,
                                        delta_sync: can_delta,
                                        clock: host_clock,
                                        batch_ops: true,
                                    })
                                    .unwrap(),
                                );
                                if canonical_name != name {
                                    let _ = tx.send(
                                        serde_json::to_string(&Msg::Rename {
                                            site,
                                            name: canonical_name.clone(),
                                        })
                                        .unwrap(),
                                    );
                                }
                                if !can_delta && !hist.is_empty() {
                                    if std::env::var("SC_DBG").is_ok() {
                                        eprintln!("[dbg] 下发结构历史 {} 条", hist.len());
                                    }
                                    let _ = tx.send(
                                        serde_json::to_string(&Msg::StructuralLog { items: hist }).unwrap(),
                                    );
                                }
                                {
                                    // 样式同步：同文件增量握手只发会话期间新增改动，异文件全量握手发完整非默认样式
                                    if !send_snapshot_styles(style_snapshot, &tx) { break; }
                                }
                                {
                                    for (msh, r1, c1, r2, c2, on) in merges {
                                        let _ = tx.send(
                                            serde_json::to_string(&Msg::MergeOp {
                                                site: 0,
                                                sheet: msh,
                                                ts: 0,
                                                r1,
                                                c1,
                                                r2,
                                                c2,
                                                on,
                                            })
                                            .unwrap(),
                                        );
                                    }
                                }
                                let cells = snap.into_iter().map(Arc::unwrap_or_clone);
                                cell_frames(cells, false, true, |line| { let _ = tx.send(line); });
                                let _ = tx.send(serde_json::to_string(&Msg::Snapshot { ops: Vec::new() }).unwrap());
                                {
                                    // 下发当前所有未过期的单元格编辑租约，确保后加入成员立即看到谁锁了哪格并禁用它
                                    for (sheet, row, col, lsite, lname) in active_locks {
                                        let _ = tx.send(
                                            serde_json::to_string(&Msg::LockOk {
                                                site: lsite,
                                                name: lname,
                                                sheet,
                                                row,
                                                col,
                                            })
                                            .unwrap(),
                                        );
                                    }
                                }
                                {
                                    // 下发所有在线成员的实时选区光标，后加入者无需等待对方移动鼠标即可看见同事 ID 标签
                                    for cur in active_cursors {
                                        if cur.site != site {
                                            let _ = tx.send(
                                                serde_json::to_string(&Msg::CursorMsg(cur)).unwrap(),
                                            );
                                        }
                                    }
                                }
                                let _ = evt_tx.send(NetEvent::PeerJoined(
                                    site,
                                    canonical_name.clone(),
                                    color,
                                ));
                                let hello = serde_json::to_string(&Msg::Hello {
                                    site,
                                    name: canonical_name,
                                    doc: doc_name.clone(),
                                    proof: String::new(),
                                    auth: String::new(),
                                    base_hash: String::new(),
                                })
                                .unwrap();
                                for ptx in other_peers {
                                    let _ = ptx.send(hello.clone());
                                }
                            }
                            message @ (Msg::Op(_) | Msg::Ops { .. }) => {
                                let Some(conn_site) = hello_site else { break };
                                let mut ops = match message {
                                    Msg::Op(op) => vec![op],
                                    Msg::Ops { ops } => ops,
                                    _ => unreachable!(),
                                };
                                if ops.iter().any(|op| op.site != conn_site) { continue; }
                                let _order = room_order.lock().unwrap();
                                expire_and_broadcast();
                                let lt = locks.lock().unwrap();
                                let now = now_ms();
                                ops.retain(|op| {
                                    let Some(h) = lt.holder_other(op.sheet, op.row, op.col, conn_site, now) else { return true };
                                    let prev = latest_cell_op(&state, op.sheet, op.row, op.col);
                                    let deny = Msg::WriteDeny {
                                        holder: h.site, holder_name: h.name.clone(),
                                        sheet: op.sheet, row: op.row, col: op.col,
                                        revert_ops: vec![(op.row, op.col, prev)],
                                        revert_styles: Vec::new(), revert_merge: None,
                                    };
                                    let _ = tx.send(serde_json::to_string(&deny).unwrap());
                                    false
                                });
                                drop(lt);
                                if ops.is_empty() { continue; }
                                update_host_ops(&state, &delta_ops, &ops);
                                queue_room_change(&cmd, &roster, Some(conn_site), RoomChange::Cells(ops.clone()));
                                let _ = evt_tx.send(NetEvent::Ops(ops));
                            }
                            Msg::CursorMsg(mut c) => {
                                let Some(conn_site) = hello_site else { break };
                                if c.site != conn_site {
                                    continue;
                                }
                                c.site = conn_site;
                                c.color = (conn_site % 7) as u8;
                                c.name = roster.lock().unwrap().name_of(conn_site);
                                cursors.lock().unwrap().insert(conn_site, c.clone());
                                let line =
                                    serde_json::to_string(&Msg::CursorMsg(c.clone())).unwrap();
                                let others = roster.lock().unwrap().other_senders(Some(conn_site));
                                for ptx in others {
                                    let _ = ptx.send(line.clone());
                                }
                                let _ = evt_tx.send(NetEvent::Cursor(c));
                            }
                            Msg::Rename { site, name } => {
                                let Some(conn_site) = hello_site else { break };
                                if site != conn_site {
                                    continue;
                                }
                                let (canonical, others) = {
                                    let mut r = roster.lock().unwrap();
                                    let canonical = r.rename_member(conn_site, &name);
                                    let others = r.other_senders(Some(conn_site));
                                    (canonical, others)
                                };
                                locks.lock().unwrap().rename_site(conn_site, &canonical);
                                if let Some(cur) = cursors.lock().unwrap().get_mut(&conn_site) {
                                    cur.name = canonical.clone();
                                }
                                let line = serde_json::to_string(&Msg::Rename {
                                    site: conn_site,
                                    name: canonical.clone(),
                                })
                                .unwrap();
                                for ptx in others {
                                    let _ = ptx.send(line.clone());
                                }
                                if canonical != name {
                                    let _ = tx.send(line);
                                }
                                let _ = evt_tx.send(NetEvent::PeerRenamed(conn_site, canonical));
                            }
                            Msg::Structural { site, sheet, op } => {
                                let Some(conn_site) = hello_site else { break };
                                if site != conn_site {
                                    continue;
                                }
                                let _order = room_order.lock().unwrap();
                                expire_and_broadcast();
                                let conflict = locks
                                    .lock()
                                    .unwrap()
                                    .conflicts_structural(sheet, &op, conn_site, now_ms())
                                    .map(|((r, c), h)| (h.site, h.name.clone(), r, c));
                                if let Some((holder, holder_name, row, col)) = conflict {
                                    let deny = serde_json::to_string(&Msg::LockDeny {
                                        site: conn_site,
                                        sheet,
                                        row,
                                        col,
                                        holder,
                                        holder_name,
                                    })
                                    .unwrap();
                                    let _ = tx.send(deny);
                                    continue;
                                }
                                shift_state(&state, &delta_ops, &host_base_hash, &styles, &delta_styles, &locks, &cursors, sheet, &op);
                                structs.lock().unwrap().push((sheet, op.clone()));
                                queue_room_change(&cmd, &roster, Some(conn_site), RoomChange::Message(Msg::Structural {
                                    site: conn_site,
                                    sheet,
                                    op: op.clone(),
                                }));
                                let _ = evt_tx.send(NetEvent::Structural {
                                    site: conn_site,
                                    sheet,
                                    op,
                                });
                            }
                            Msg::StyleOp { site, sheet, ts, items } => {
                                let Some(conn_site) = hello_site else { break };
                                if site != conn_site {
                                    continue;
                                }
                                let _order = room_order.lock().unwrap();
                                expire_and_broadcast();
                                let conflict = {
                                    let now = now_ms();
                                    let lt = locks.lock().unwrap();
                                    items.iter().find_map(|(r, c, _)| {
                                        lt.holder_other(sheet, *r, *c, conn_site, now)
                                            .map(|h| (h.site, h.name.clone(), *r, *c))
                                    })
                                };
                                if let Some((holder, holder_name, row, col)) = conflict {
                                    let revert_styles: Vec<_> = {
                                        let st = styles.lock().unwrap();
                                        items
                                            .iter()
                                            .map(|(r, c, _)| (*r, *c, st.get(&(sheet, *r, *c)).map(|s| s.as_ref().clone())))
                                            .collect()
                                    };
                                    // A conflict can also return large existing styles. The host
                                    // has rejected the entire request before sending any rollback.
                                    let deny = Msg::WriteDeny {
                                        holder, holder_name, sheet, row, col,
                                        revert_ops: Vec::new(), revert_styles, revert_merge: None,
                                    };
                                    if !send_room_message(deny, MAX_PLAIN_LINE, |line| tx.send(line).is_ok(), || {}) { break; }
                                    continue;
                                }
                                update_host_styles(&styles, &delta_styles, sheet, &items);
                                queue_room_change(&cmd, &roster, Some(conn_site), RoomChange::Message(Msg::StyleOp {
                                    site: conn_site,
                                    sheet,
                                    ts,
                                    items: items.clone(),
                                }));
                                let ev: Vec<(usize, u32, u32, sheetcore::style::CellStyle)> =
                                    items.iter().map(|(r, c, s)| (sheet, *r, *c, s.clone())).collect();
                                let _ = evt_tx.send(NetEvent::Styles {
                                    site: conn_site,
                                    items: ev,
                                    snapshot: false,
                                });
                            }
                            Msg::MergeOp { site, sheet, ts, r1, c1, r2, c2, on } => {
                                let Some(conn_site) = hello_site else { break };
                                if site != conn_site {
                                    continue;
                                }
                                let _order = room_order.lock().unwrap();
                                expire_and_broadcast();
                                let conflict = locks
                                    .lock()
                                    .unwrap()
                                    .conflicts_rect(sheet, r1, c1, r2, c2, conn_site, now_ms())
                                    .map(|((r, c), h)| (h.site, h.name.clone(), r, c));
                                if let Some((holder, holder_name, row, col)) = conflict {
                                    let deny = serde_json::to_string(&Msg::WriteDeny {
                                        holder,
                                        holder_name,
                                        sheet,
                                        row,
                                        col,
                                        revert_ops: Vec::new(),
                                        revert_styles: Vec::new(),
                                        revert_merge: Some((r1, c1, r2, c2, !on)),
                                    })
                                    .unwrap();
                                    let _ = tx.send(deny);
                                    continue;
                                }
                                delta_merges.lock().unwrap().push((sheet, r1, c1, r2, c2, on));
                                queue_room_change(&cmd, &roster, Some(conn_site), RoomChange::Message(Msg::MergeOp {
                                    site: conn_site,
                                    sheet,
                                    ts,
                                    r1,
                                    c1,
                                    r2,
                                    c2,
                                    on,
                                }));
                                let _ = evt_tx.send(NetEvent::Merge {
                                    site: conn_site,
                                    sheet,
                                    rect: (r1, c1, r2, c2),
                                    on,
                                });
                            }
                            Msg::LockReq { site, name: _, sheet, row, col } => {
                                let Some(conn_site) = hello_site else { break };
                                if site != conn_site {
                                    continue;
                                }
                                let _order = room_order.lock().unwrap();
                                expire_and_broadcast();
                                let auth_name = roster.lock().unwrap().name_of(conn_site);
                                let (outcome, evicted) = {
                                    let mut lt = locks.lock().unwrap();
                                    let outcome = lt.request(
                                        sheet,
                                        row,
                                        col,
                                        conn_site,
                                        &auth_name,
                                        now_ms(),
                                    );
                                    let evicted = match outcome {
                                        sheetcore::sync::LockOutcome::Denied { .. } => Vec::new(),
                                        _ => lt.release_all_except(conn_site, (sheet, row, col)),
                                    };
                                    (outcome, evicted)
                                };
                                if !evicted.is_empty() {
                                    let all = roster.lock().unwrap().all_senders();
                                    for (psh, pr, pc) in evicted {
                                        let rel = serde_json::to_string(&Msg::LockRelease {
                                            site: conn_site,
                                            sheet: psh,
                                            row: pr,
                                            col: pc,
                                        })
                                        .unwrap();
                                        for ptx in &all {
                                            let _ = ptx.send(rel.clone());
                                        }
                                        let _ = evt_tx.send(NetEvent::LockRelease {
                                            site: conn_site,
                                            sheet: psh,
                                            row: pr,
                                            col: pc,
                                        });
                                    }
                                }
                                match outcome {
                                    sheetcore::sync::LockOutcome::Denied { holder, name: hn } => {
                                        let line = serde_json::to_string(&Msg::LockDeny {
                                            site: conn_site,
                                            sheet,
                                            row,
                                            col,
                                            holder,
                                            holder_name: hn,
                                        })
                                        .unwrap();
                                        let _ = tx.send(line);
                                    }
                                    _ => {
                                        let line = serde_json::to_string(&Msg::LockOk {
                                            site: conn_site,
                                            name: auth_name.clone(),
                                            sheet,
                                            row,
                                            col,
                                        })
                                        .unwrap();
                                        let others =
                                            roster.lock().unwrap().other_senders(Some(conn_site));
                                        for ptx in others {
                                            let _ = ptx.send(line.clone());
                                        }
                                        let _ = tx.send(line);
                                        let _ = evt_tx.send(NetEvent::LockOk {
                                            site: conn_site,
                                            name: auth_name,
                                            sheet,
                                            row,
                                            col,
                                        });
                                    }
                                }
                            }
                            Msg::LockRelease { site, sheet, row, col } => {
                                let Some(conn_site) = hello_site else { break };
                                // 只允许释放自己持有的租约，杜绝跨站点恶意解锁
                                if site != conn_site {
                                    continue;
                                }
                                let ok = locks.lock().unwrap().release(sheet, row, col, conn_site);
                                if ok {
                                    let line = serde_json::to_string(&Msg::LockRelease {
                                        site: conn_site,
                                        sheet,
                                        row,
                                        col,
                                    })
                                    .unwrap();
                                    let others =
                                        roster.lock().unwrap().other_senders(Some(conn_site));
                                    for ptx in others {
                                        let _ = ptx.send(line.clone());
                                    }
                                    let _ = evt_tx.send(NetEvent::LockRelease {
                                        site: conn_site,
                                        sheet,
                                        row,
                                        col,
                                    });
                                }
                            }
                            Msg::SaveReq { site } => {
                                let Some(conn_site) = hello_site else { break };
                                if site != conn_site {
                                    continue;
                                }
                                let who = roster.lock().unwrap().name_of(conn_site);
                                let _ = evt_tx.send(NetEvent::SaveRequested {
                                    site: conn_site,
                                    name: who,
                                });
                            }
                            Msg::UpdateAnnounce {
                                site: ann_site,
                                version,
                                size,
                                sha256,
                                sig,
                                notes,
                            } => {
                                let Some(conn_site) = hello_site else { break };
                                if ann_site != conn_site {
                                    continue;
                                }
                                let manifest = crate::update::SignedManifest {
                                    version: version.clone(),
                                    exe: "CELLAN.exe".into(),
                                    size,
                                    sha256: sha256.clone(),
                                    sig: sig.clone(),
                                    notes: notes.clone(),
                                };
                                // 宿主先做官方公钥验签：伪造签名直接丢弃，既不提示自己也不转发给其他成员
                                if !crate::update::verify_manifest_sig(&manifest) {
                                    continue;
                                }
                                let (who, others) = {
                                    let r = roster.lock().unwrap();
                                    (r.name_of(conn_site), r.other_senders(Some(conn_site)))
                                };
                                let fwd = serde_json::to_string(&Msg::UpdateAnnounce {
                                    site: conn_site,
                                    version,
                                    size,
                                    sha256,
                                    sig,
                                    notes,
                                })
                                .unwrap();
                                for ptx in others {
                                    let _ = ptx.send(fwd.clone());
                                }
                                if crate::update::is_newer(&manifest.version, crate::update::VERSION)
                                {
                                    let _ = evt_tx.send(NetEvent::PeerUpdateOffer {
                                        peer_site: conn_site,
                                        peer_name: who,
                                        manifest,
                                    });
                                }
                            }
                            Msg::UpdatePull {
                                from_site,
                                to_site,
                                version,
                            } => {
                                let Some(conn_site) = hello_site else { break };
                                if from_site != conn_site || to_site == conn_site {
                                    continue;
                                }
                                if to_site == host_site_id {
                                    let peer_tx = tx.clone();
                                    spawn_peer_update_stream(
                                        host_site_id,
                                        conn_site,
                                        version,
                                        seed_override.clone(),
                                        serving_update.clone(),
                                        alive_conn.clone(),
                                        move |json| peer_tx.send(json).is_ok(),
                                    );
                                } else if let Some(target_tx) =
                                    roster.lock().unwrap().sender_of(to_site)
                                {
                                    if let Ok(fwd) = serde_json::to_string(&Msg::UpdatePull {
                                        from_site: conn_site,
                                        to_site,
                                        version,
                                    }) {
                                        let _ = target_tx.send(fwd);
                                    }
                                }
                            }
                            Msg::UpdateSlice {
                                from_site,
                                to_site,
                                version,
                                offset,
                                total,
                                data_b64,
                                done,
                            } => {
                                let Some(conn_site) = hello_site else { break };
                                if from_site != conn_site || to_site == conn_site {
                                    continue;
                                }
                                if to_site == host_site_id {
                                    accept_incoming_update_slice(
                                        &update_dl,
                                        &evt_tx,
                                        conn_site,
                                        &version,
                                        offset,
                                        total,
                                        &data_b64,
                                        done,
                                    );
                                } else if let Some(target_tx) =
                                    roster.lock().unwrap().sender_of(to_site)
                                {
                                    if let Ok(fwd) = serde_json::to_string(&Msg::UpdateSlice {
                                        from_site: conn_site,
                                        to_site,
                                        version,
                                        offset,
                                        total,
                                        data_b64,
                                        done,
                                    }) {
                                        let _ = target_tx.send(fwd);
                                    }
                                }
                            }
                            Msg::Bye { site } => {
                                let Some(conn_site) = hello_site else { break };
                                if site == conn_site {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(site) = hello_site {
                        let remaining = roster.lock().unwrap().remove_member(site);
                        cursors.lock().unwrap().remove(&site);
                        // 人走了，锁必须放掉并通知宿主与其他全部客户端，绝不残留幽灵锁或幽灵光标
                        let mine: Vec<(usize, u32, u32)> = locks
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|(_, h)| h.site == site)
                            .map(|(k, _)| k)
                            .collect();
                        for (sheet, row, col) in mine {
                            locks.lock().unwrap().release(sheet, row, col, site);
                            let line = serde_json::to_string(&Msg::LockRelease {
                                site, sheet, row, col,
                            })
                            .unwrap();
                            for ptx in &remaining {
                                let _ = ptx.send(line.clone());
                            }
                            let _ = evt_tx.send(NetEvent::LockRelease { site, sheet, row, col });
                        }
                        let bye = serde_json::to_string(&Msg::Bye { site }).unwrap();
                        for ptx in &remaining {
                            let _ = ptx.send(bye.clone());
                        }
                        let _ = evt_tx.send(NetEvent::PeerLeft(site));
                    }
                });
            }
        });
    }

    let net = Net {
        site,
        cmd: cmd_tx,
        events: evt_rx,
        hosting: true,
        port,
        addr: format!("0.0.0.0:{port}"),
        session_id,
        started_ms,
        lock_secret: String::new(),
        alive,
        connected,
        wake_ctx,
        sockets,
        state: Some(state),
        delta_ops: Some(delta_ops_for_self),
        host_base_hash: Some(host_base_hash_for_self),
        host_roster: Some(roster_for_self),
        room_order: Some(room_order),
        #[cfg(test)]
        after_snapshot,
        host_locks: Some(locks_for_self),
        host_cursors: Some(cursors_for_self),
        host_styles: Some(styles_for_self),
        delta_styles: Some(delta_styles_for_self),
        delta_merges: Some(delta_merges_for_self),
        host_structs: Some(structs_for_self),
        peer_update_dl: host_update_dl_for_self,
        update_seed_override: host_seed_override_for_self,
        evt_tx: evt_tx_self,
        t0,
    };
    initialize(&net);
    let _ = ready.send(());
    Ok(net)
}

/// 加入宿主：断开后自动重连（指数退避，最长 10 秒）
pub fn join(
    site: u32,
    name: String,
    doc: String,
    addr: &str,
    password: String,
) -> std::io::Result<Net> {
    join_session(site, name, doc, addr, password, false, String::new())
}

pub fn join_with_base(
    site: u32,
    name: String,
    doc: String,
    addr: &str,
    password: String,
    base_hash: String,
) -> std::io::Result<Net> {
    join_session(site, name, doc, addr, password, false, base_hash)
}

/// Discovery cards can outlive their room, and the port may have been reused.
pub fn join_discovered(
    site: u32,
    name: String,
    doc: String,
    addr: &str,
    password: String,
) -> std::io::Result<Net> {
    join_session(site, name, doc, addr, password, true, String::new())
}

pub fn join_discovered_with_base(
    site: u32,
    name: String,
    doc: String,
    addr: &str,
    password: String,
    base_hash: String,
) -> std::io::Result<Net> {
    join_session(site, name, doc, addr, password, true, base_hash)
}

fn parse_join_targets(raw: &str) -> std::io::Result<(String, u16, Vec<String>)> {
    let mut targets = Vec::new();
    let mut first_port = DEFAULT_PORT;
    for part in raw.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let normalized = if let Ok(sa) = part.parse::<std::net::SocketAddr>() {
            if targets.is_empty() {
                first_port = sa.port();
            }
            sa.to_string()
        } else if let Some((host, p)) = part.rsplit_once(':') {
            let host = host.trim();
            let port: u16 = p.trim().parse().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("地址端口格式不对：{part}"),
                )
            })?;
            if host.is_empty() || port == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("地址格式不对：{part}"),
                ));
            }
            if targets.is_empty() {
                first_port = port;
            }
            format!("{host}:{port}")
        } else {
            // 用户只输入了 IP 或机器名而未带端口时，自动补上默认协作端口 7878
            if targets.is_empty() {
                first_port = DEFAULT_PORT;
            }
            format!("{part}:{DEFAULT_PORT}")
        };
        if !targets.contains(&normalized) {
            targets.push(normalized);
        }
    }
    if targets.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "请填写协作地址",
        ));
    }
    let display_addr = targets[0].clone();
    Ok((display_addr, first_port, targets))
}

fn connect_any_target(targets: &[String], timeout: Duration) -> std::io::Result<TcpStream> {
    use std::net::ToSocketAddrs;
    let mut addrs: Vec<std::net::SocketAddr> = Vec::new();
    let mut last_err: Option<std::io::Error> = None;
    for t in targets {
        if let Ok(sa) = t.parse::<std::net::SocketAddr>() {
            if !addrs.contains(&sa) {
                addrs.push(sa);
            }
        } else {
            match t.to_socket_addrs() {
                Ok(iter) => {
                    for sa in iter {
                        if !addrs.contains(&sa) {
                            addrs.push(sa);
                        }
                    }
                }
                Err(e) => last_err = Some(e),
            }
        }
    }
    if addrs.is_empty() {
        return Err(last_err.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "无法解析宿主地址")
        }));
    }
    if addrs.len() == 1 {
        let s = TcpStream::connect_timeout(&addrs[0], timeout)?;
        let _ = s.set_nodelay(true);
        return Ok(s);
    }
    // 多网卡候选地址并行竞速：哪张网卡先连通就立刻用哪张，彻底解决虚拟网卡选错与串行超时卡顿
    let (tx, rx) = channel::<Result<TcpStream, std::io::Error>>();
    let total = addrs.len();
    for sa in addrs {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(TcpStream::connect_timeout(&sa, timeout));
        });
    }
    drop(tx);
    let mut err_out = None;
    for _ in 0..total {
        match rx.recv_timeout(timeout + Duration::from_millis(150)) {
            Ok(Ok(stream)) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Ok(Err(e)) => err_out = Some(e),
            Err(_) => break,
        }
    }
    Err(err_out.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "连接宿主超时")
    }))
}

fn join_session(
    site: u32,
    name: String,
    doc: String,
    addr: &str,
    password: String,
    require_same_document: bool,
    initial_base_hash: String,
) -> std::io::Result<Net> {
    if site == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "客户端站点号不能为 0",
        ));
    }
    // 仅在调用线程做毫秒级纯文本语法归一化；真正 TCP 连接全部放入后台监督线程执行，绝不卡死 UI！
    let (display_addr, first_port, targets) = parse_join_targets(addr)?;
    let sockets = Arc::new(Mutex::new(Vec::new()));

    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (evt_raw_tx, evt_rx) = channel::<NetEvent>();
    let (evt_tx, wake_ctx) = EvtTx::new(evt_raw_tx);
    let evt_tx_clone = evt_tx.clone();
    let alive = Arc::new(AtomicBool::new(true));
    let connected_flag = Arc::new(AtomicBool::new(false));
    let connected_for_net = connected_flag.clone();
    let writer: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
    // 发送端加密会话单独归写线程持有，读线程使用栈上独立解密会话（彻底消灭大快照解密期间读写锁争用！）
    let send_sess: Arc<Mutex<Option<(sheetcore::wire::Session, bool)>>> = Arc::new(Mutex::new(None));
    // 本客户端实例专属重连凭据：断线重连可继续沿用自身 site，其他客户端不能冒领该 site
    let client_auth = crate::session_lock::new_session_id();
    let client_update_dl: Arc<Mutex<Option<ActivePeerDownload>>> = Arc::new(Mutex::new(None));
    let client_update_dl_for_net = client_update_dl.clone();
    let client_seed_override: SharedUpdateSeed = Arc::new(Mutex::new(None));
    let client_seed_override_for_net = client_seed_override.clone();
    let client_serving_update = Arc::new(AtomicBool::new(false));
    let cmd_tx_for_stream = cmd_tx.clone();

    // 写线程
    {
        let writer = writer.clone();
        let alive = alive.clone();
        let dispatch_events = evt_tx.clone();
        let send_sess = send_sess.clone();
        std::thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                if !alive.load(Ordering::SeqCst) {
                    break;
                }
                match cmd {
                    Cmd::Cells(ops) => {
                        let mut session = send_sess.lock().unwrap();
                        if let (Some((se, batch)), Some(w)) = (session.as_mut(), writer.lock().unwrap().as_mut()) {
                            cell_frames(ops, true, *batch, |line| { let _ = write_line(w, &se.seal(&line)); });
                        }
                    }
                    Cmd::Style(msg) => {
                        let mut session = send_sess.lock().unwrap();
                        if let (Some((se, _)), Some(w)) = (session.as_mut(), writer.lock().unwrap().as_mut()) {
                            if !send_room_message(msg, MAX_PLAIN_LINE, |line| write_line(w, &se.seal(&line)).is_ok(), || {
                                let _ = dispatch_events.send(NetEvent::Error("格式修改超过单条消息上限，宿主将分批检查编辑锁并应用".into()));
                            }) {
                                let _ = dispatch_events.send(NetEvent::Error("协作格式未能完整发送".into()));
                            }
                        }
                    }
                    Cmd::Deliver(_, _) => unreachable!("room dispatch is host-only"),
                    Cmd::Broadcast(s) | Cmd::SendTo(_, s) => {
                        let mut session = send_sess.lock().unwrap();
                        if let (Some((se, _)), Some(w)) = (session.as_mut(), writer.lock().unwrap().as_mut()) {
                            let _ = write_line(w, &se.seal(&s));
                        }
                    }
                    Cmd::Stop => break,
                }
            }
        });
    }

    // 监督线程：后台并行连接 → 握手 → 读循环 → 断开 → 退避重连
    {
        let sockets = sockets.clone();
        let targets = targets.clone();
        let name = name.clone();
        let doc = doc.clone();
        let password = password.clone();
        let evt_tx = evt_tx.clone();
        let writer = writer.clone();
        let alive = alive.clone();
        let client_auth = client_auth.clone();
        std::thread::spawn(move || {
            let mut backoff = 1u64;
            let mut ever_welcomed = false;
            // 重连到同一房间（同一房间 salt）时复用已拉伸的房间密钥，不必再跑一遍 PBKDF2
            let mut room_cache: Option<(String, sheetcore::wire::RoomKey)> = None;
            while alive.load(Ordering::SeqCst) {
                let connected = match connect_any_target(&targets, Duration::from_millis(2500)) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = evt_tx.send(NetEvent::Disconnected(format!("连接失败: {e}")));
                        if !alive.load(Ordering::SeqCst) {
                            break;
                        }
                        std::thread::sleep(Duration::from_secs(backoff));
                        backoff = (backoff * 2).min(10);
                        continue;
                    }
                };
                backoff = 1;
                let Some(connected) = track_socket(connected, &sockets, &alive) else { break };
                let mut write_half = match connected.try_clone() {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let _ = write_half.set_write_timeout(Some(Duration::from_secs(10)));
                let reader = BufReader::new(&*connected);
                let mut denied = false;
                let mut got_welcome = false;
                let mut recv_sess: Option<sheetcore::wire::Session> = None;
                let mut pending_sender: Option<sheetcore::wire::Session> = None;

                let mut reader = reader;
                loop {
                    let Ok(Some(line)) = read_line_bounded(&mut reader, MAX_LINE) else { break };
                    let msg = if let Some(se) = recv_sess.as_mut() {
                        match se.open(&line) {
                            Some(plain) => serde_json::from_str::<Msg>(&plain),
                            None => match serde_json::from_str::<Msg>(&line) {
                                Ok(msg @ Msg::Denied { .. }) if !got_welcome => Ok(msg),
                                _ => break,
                            },
                        }
                    } else {
                        match serde_json::from_str::<Msg>(&line) {
                            Ok(msg @ (Msg::ChallengeSalt { .. } | Msg::Challenge { .. } | Msg::Denied { .. })) => Ok(msg),
                            _ => break,
                        }
                    };
                    let msg = match msg {
                        Ok(m) => m,
                        // 入房后认不出的报文（例如更新版本宿主的新消息类型）跳过即可，
                        // 断开只会换来无限重连；握手阶段认不出才断开
                        Err(_) if got_welcome => continue,
                        Err(_) => break,
                    };
                    if !msg.is_within_limits() {
                        continue;
                    }
                    if !got_welcome && !matches!(msg, Msg::ChallengeSalt { .. } | Msg::Challenge { .. }
                        | Msg::Welcome { .. } | Msg::Denied { .. }) {
                        break;
                    }
                    match msg {
                        Msg::ChallengeSalt { nonce, salt, room_salt } if recv_sess.is_none() => {
                            let Some(raw) = sheetcore::wire::b64_decode(&salt).filter(|s| s.len() == 16) else { break };
                            let Some(room_raw) = sheetcore::wire::b64_decode(&room_salt).filter(|s| s.len() == 16) else {
                                let _ = evt_tx.send(NetEvent::Denied(
                                    "对方的 CELLAN 版本过旧，协作协议不兼容；请双方升级到同一版本".into(),
                                ));
                                denied = true;
                                break;
                            };
                            let room_key = match &room_cache {
                                Some((cached_salt, key)) if *cached_salt == room_salt => *key,
                                _ => {
                                    let key = sheetcore::wire::RoomKey::derive(&password, &room_raw);
                                    room_cache = Some((room_salt.clone(), key));
                                    key
                                }
                            };
                            let client_sess = sheetcore::wire::Session::client_side(&room_key, &raw);
                            let mut sender_sess = client_sess.fork_sender();
                            recv_sess = Some(client_sess);
                            let hello = Msg::Hello {
                                site,
                                name: name.clone(),
                                doc: doc.clone(),
                                proof: room_key.proof(nonce),
                                auth: client_auth.clone(),
                                base_hash: if ever_welcomed {
                                    String::new()
                                } else {
                                    initial_base_hash.clone()
                                },
                            };
                            let frame = sender_sess.seal(&serde_json::to_string(&hello).unwrap());
                            if write_line(&mut write_half, &frame).is_err() {
                                break;
                            }
                            pending_sender = Some(sender_sess);
                        }
                        Msg::Challenge { .. } | Msg::ChallengeSalt { .. } => {
                            let _ = evt_tx.send(NetEvent::Denied("宿主未提供兼容的加密握手，请设置房间口令并重新开启共享".into()));
                            denied = true;
                            break;
                        }
                        Msg::Welcome { peers, doc: remote_doc, delta_sync, clock, batch_ops, .. } if !got_welcome => {
                            if require_same_document && remote_doc != doc {
                                let _ = evt_tx.send(NetEvent::Denied("共享地址已属于另一工作簿，请刷新协作会话".into()));
                                denied = true;
                                break;
                            }
                            got_welcome = true;
                            ever_welcomed = true;
                            if let Some(se) = pending_sender.take() {
                                *send_sess.lock().unwrap() = Some((se, batch_ops));
                            }
                            if let Ok(clone) = write_half.try_clone() {
                                *writer.lock().unwrap() = Some(clone);
                            }
                            connected_flag.store(true, Ordering::SeqCst);
                            let _ = evt_tx.send(NetEvent::Connected);
                            let _ = evt_tx.send(NetEvent::Welcome { peers, delta_sync, clock });
                        }
                        Msg::Denied { reason } => {
                            let _ = evt_tx.send(NetEvent::Denied(reason));
                            denied = true;
                            break;
                        }
                        Msg::SnapshotChunk { ops, last } => {
                            let _ = evt_tx.send(NetEvent::Ops(ops));
                            if last {
                                let _ = evt_tx.send(NetEvent::SnapshotDone);
                            }
                        }
                        Msg::Snapshot { ops } => {
                            let _ = evt_tx.send(NetEvent::Ops(ops));
                            let _ = evt_tx.send(NetEvent::SnapshotDone);
                        }
                        Msg::Op(op) => {
                            let _ = evt_tx.send(NetEvent::Ops(vec![op]));
                        }
                        Msg::StructuralLog { items } => {
                            let _ = evt_tx.send(NetEvent::StructuralHistory(items));
                        }
                        Msg::Structural { site, sheet, op } => {
                            let _ = evt_tx.send(NetEvent::Structural { site, sheet, op });
                        }
                        Msg::StyleOp { site, sheet, items, .. } => {
                            let ev: Vec<(usize, u32, u32, sheetcore::style::CellStyle)> =
                                items.iter().map(|(r, c, s)| (sheet, *r, *c, s.clone())).collect();
                            let _ = evt_tx.send(NetEvent::Styles {
                                site,
                                items: ev,
                                snapshot: false,
                            });
                        }
                        Msg::StyleSnapshot { items } => {
                            let _ = evt_tx.send(NetEvent::Styles {
                                site: 0,
                                items,
                                snapshot: true,
                            });
                        }
                        Msg::MergeOp { site, sheet, ts: _, r1, c1, r2, c2, on } => {
                            let _ = evt_tx.send(NetEvent::Merge {
                                site,
                                sheet,
                                rect: (r1, c1, r2, c2),
                                on,
                            });
                        }
                        Msg::LockOk { site, name, sheet, row, col } => {
                            let _ = evt_tx.send(NetEvent::LockOk { site, name, sheet, row, col });
                        }
                        Msg::LockDeny { holder, holder_name, sheet, row, col, .. } => {
                            let _ = evt_tx.send(NetEvent::LockDeny {
                                holder,
                                holder_name,
                                sheet,
                                row,
                                col,
                            });
                        }
                        Msg::WriteDeny {
                            holder,
                            holder_name,
                            sheet,
                            row,
                            col,
                            revert_ops,
                            revert_styles,
                            revert_merge,
                        } => {
                            let _ = evt_tx.send(NetEvent::WriteRejected {
                                holder,
                                holder_name,
                                sheet,
                                row,
                                col,
                                revert_ops,
                                revert_styles,
                                revert_merge,
                            });
                        }
                        Msg::LockRelease { site, sheet, row, col } => {
                            let _ = evt_tx.send(NetEvent::LockRelease { site, sheet, row, col });
                        }
                        Msg::Saved { clock, .. } => {
                            let _ = evt_tx.send(NetEvent::Saved { clock });
                        }
                        Msg::CursorMsg(c) => {
                            let _ = evt_tx.send(NetEvent::Cursor(c));
                        }
                        Msg::Hello { site: s, name: n, .. } => {
                            let _ = evt_tx.send(NetEvent::PeerJoined(s, n, (s % 7) as u8));
                        }
                        Msg::Rename { site: s, name: n } => {
                            let _ = evt_tx.send(NetEvent::PeerRenamed(s, n));
                        }
                        Msg::UpdateAnnounce {
                            site: peer_site,
                            version,
                            size,
                            sha256,
                            sig,
                            notes,
                        } => {
                            if peer_site != site {
                                let manifest = crate::update::SignedManifest {
                                    version,
                                    exe: "CELLAN.exe".into(),
                                    size,
                                    sha256,
                                    sig,
                                    notes,
                                };
                                if crate::update::verify_manifest_sig(&manifest)
                                    && crate::update::is_newer(
                                        &manifest.version,
                                        crate::update::VERSION,
                                    )
                                {
                                    let _ = evt_tx.send(NetEvent::PeerUpdateOffer {
                                        peer_site,
                                        peer_name: String::new(),
                                        manifest,
                                    });
                                }
                            }
                        }
                        Msg::UpdatePull {
                            from_site,
                            to_site,
                            version,
                        } => {
                            if to_site == site && from_site != site {
                                let out_tx = cmd_tx_for_stream.clone();
                                spawn_peer_update_stream(
                                    site,
                                    from_site,
                                    version,
                                    client_seed_override.clone(),
                                    client_serving_update.clone(),
                                    alive.clone(),
                                    move |json| out_tx.send(Cmd::SendTo(from_site, json)).is_ok(),
                                );
                            }
                        }
                        Msg::UpdateSlice {
                            from_site,
                            to_site,
                            version,
                            offset,
                            total,
                            data_b64,
                            done,
                        } => {
                            if to_site == site && from_site != site {
                                accept_incoming_update_slice(
                                    &client_update_dl,
                                    &evt_tx,
                                    from_site,
                                    &version,
                                    offset,
                                    total,
                                    &data_b64,
                                    done,
                                );
                            }
                        }
                        Msg::Bye { site: s } => {
                            let _ = evt_tx.send(NetEvent::PeerLeft(s));
                        }
                        _ => {}
                    }
                }
                connected_flag.store(false, Ordering::SeqCst);
                *writer.lock().unwrap() = None;
                *send_sess.lock().unwrap() = None;
                if denied {
                    break;
                }
                let hint = if password.is_empty() || got_welcome {
                    "连接中断".to_string()
                } else {
                    "连接中断（当前协作会话设有口令，请确认口令是否正确）".to_string()
                };
                let _ = evt_tx.send(NetEvent::Disconnected(hint));
                if !alive.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_secs(backoff));
                backoff = (backoff * 2).min(10);
            }
        });
    }

    Ok(Net {
        site,
        cmd: cmd_tx,
        events: evt_rx,
        hosting: false,
        sockets,
        port: first_port,
        addr: display_addr,
        session_id: String::new(),
        started_ms: 0,
        lock_secret: String::new(),
        alive,
        connected: connected_for_net,
        wake_ctx,
        state: None,
        delta_ops: None,
        host_base_hash: None,
        host_roster: None,
        room_order: None,
        #[cfg(test)]
        after_snapshot: Default::default(),
        host_locks: None,
        host_cursors: None,
        host_styles: None,
        delta_styles: None,
        delta_merges: None,
        host_structs: None,
        peer_update_dl: client_update_dl_for_net,
        update_seed_override: client_seed_override_for_net,
        evt_tx: evt_tx_clone,
        t0: std::time::Instant::now(),
    })
}

#[cfg(test)]
pub(crate) use tests::host_for_test;

#[cfg(test)]
mod tests {
    use super::*;
    use sheetcore::sync::ValueWire;

    fn op(row: u32, n: f64, ts: u64, site: u32) -> Op {
        Op { sheet: 0, row, col: 0, value: ValueWire::Number(n), formula: None, ts, site }
    }

    /// 让系统分配一个空闲端口（本机回环），不再写死 17881/7987 这类固定端口：
    /// 端口被占时旧写法会把「起不来」当「跳过」而记成通过。
    /// 起宿主：测试自己绑 `127.0.0.1:0`（只回环，不对外开端口），端口由系统分配，
    /// 再把已绑定的 listener 交给 host_on——既没有「探测端口→放开→再绑」的 TOCTOU，
    /// 也不会在跑测试的机器上暴露一个对外监听。生产 `host()` 仍绑 `0.0.0.0`。
    pub(crate) fn host_for_test(site: u32, name: &str, doc: &str, password: &str) -> (Net, u16) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("绑 127.0.0.1:0");
        let port = listener.local_addr().expect("取本地端口").port();
        let net = super::host_on(listener, site, name.into(), doc.into(), password.into())
            .expect("在已绑定 listener 上起宿主");
        (net, port)
    }

    #[test]
    fn live_style_requests_only_split_when_the_wire_limit_requires_it() {
        let style = sheetcore::style::CellStyle::default();
        let items: Vec<_> = (0..700).map(|row| (row, 1, style.clone())).collect();
        let msg = Msg::StyleOp { site: 3, sheet: 0, ts: 0, items };
        let exact = serde_json::to_string(&msg).unwrap().len();
        for (limit, should_split) in [(exact, false), (exact / 3, true)] {
            let mut split = false;
            let mut rows = Vec::new();
            let mut frames = 0;
            assert!(send_room_message(msg.clone(), limit, |line| {
                assert!(line.len() <= limit);
                let Msg::StyleOp { items, .. } = serde_json::from_str(&line).unwrap() else { panic!() };
                rows.extend(items.into_iter().map(|(row, _, _)| row));
                frames += 1;
                true
            }, || split = true));
            assert_eq!((split, frames > 1), (should_split, should_split));
            assert_eq!(rows, (0..700).collect::<Vec<_>>());
        }
    }

    #[test]
    fn peers_receive_large_live_style_edits_in_both_directions() {
        let _env = crate::test_env::TestEnv::new("live-style-room");
        let (host, port) = host_for_test(1, "Owner", "Doc", "pw");
        let guest = join(2, "Peer".into(), "Doc".into(), &format!("127.0.0.1:{port}"), "pw".into()).unwrap();
        assert!(wait_for(&guest, 5, |event| matches!(event, NetEvent::SnapshotDone)));
        let style = sheetcore::style::CellStyle { italic: true, ..Default::default() };
        for (sender, receiver) in [(&host, &guest), (&guest, &host)] {
            let items = (0..700).map(|row| (row, 2, style.clone())).collect();
            sender.broadcast_style(0, items);
            let mut next = 0;
            let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while next < 700 {
                let ev = receiver.events.recv_timeout(end.saturating_duration_since(std::time::Instant::now())).unwrap();
                if let NetEvent::Styles { site, items, snapshot: false } = ev {
                    assert_eq!(site, sender.site);
                    assert_eq!(items.len(), 700, "a normal edit creates one history entry at the receiver");
                    for (sheet, row, col, cell_style) in items {
                        assert_eq!((sheet, row, col, cell_style.italic), (0, next, 2, true));
                        next += 1;
                    }
                }
            }
        }
    }

    #[test]
    fn rejected_structural_broadcast_preserves_host_coordinates() {
        let _env = crate::test_env::TestEnv::new("structural-broadcast-refused");
        let (host, _) = host_for_test(1, "Owner", "Doc", "");
        let now = host.host_now_ms();
        host.host_locks.as_ref().unwrap().lock().unwrap().request(0, 7, 2, 2, "Peer", now);
        let insert = sheetcore::structural::Structural::InsertRows { at: 4, n: 1 };
        assert!(host.broadcast_structural(0, &insert).unwrap_err().contains("Peer"));
        assert!(host.host_structs.as_ref().unwrap().lock().unwrap().is_empty());
        assert!(host.host_lock_conflict_structural(0, &insert).is_some());
    }

    fn wait_for<F: Fn(&NetEvent) -> bool>(net: &Net, secs: u64, f: F) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if let Ok(ev) = net.events.recv_timeout(Duration::from_millis(200)) {
                if f(&ev) {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn initialization_precedes_accept_and_live_edits_follow_the_captured_snapshot() {
        let _env = crate::test_env::TestEnv::new("join-during-edits");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (starting_tx, starting_rx) = channel();
        let (seed_tx, seed_rx) = channel();
        let host_task = std::thread::spawn(move || {
            host_initialized(listener, 1, "Host".into(), "Doc".into(), "pw".into(),
                "session".into(), 1, |host| {
                    starting_tx.send(()).unwrap();
                    seed_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                    host.broadcast_op(op(0, 11.0, 1, 1));
                }).unwrap()
        });
        let timeout = Duration::from_secs(10);
        starting_rx.recv_timeout(timeout).unwrap();
        // The listener is already bound. A TCP client must receive no greeting
        // until the host's initial workbook has been installed.
        let probe = TcpStream::connect(&addr).unwrap();
        probe.set_read_timeout(Some(Duration::from_millis(125))).unwrap();
        assert!(matches!(probe.peek(&mut [0; 1]).unwrap_err().kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock));
        drop(probe);
        let editor = join(2, "Editor".into(), "Doc".into(), &addr, "pw".into()).unwrap();
        seed_tx.send(()).unwrap();
        let host = host_task.join().unwrap();
        assert!(wait_for(&editor, 5, |e| matches!(e, NetEvent::Ops(ops) if ops.iter().any(|o| o.value == ValueWire::Number(11.0)))));
        let (captured_tx, captured_rx) = channel();
        let (release_tx, release_rx) = channel();
        *host.after_snapshot.lock().unwrap() = Some(Box::new(move || {
            captured_tx.send(()).unwrap();
            release_rx.recv_timeout(timeout).unwrap();
        }));
        let viewer = join(3, "Viewer".into(), "Doc".into(), &addr, "pw".into()).unwrap();
        captured_rx.recv_timeout(timeout).unwrap();
        host.broadcast_structural(0, &sheetcore::structural::Structural::InsertRows { at: 0, n: 1 }).unwrap();
        editor.broadcast_op(op(0, 22.0, 4, 2));
        assert!(wait_for(&host, 5, |e| matches!(e, NetEvent::Ops(ops) if ops.iter().any(|o| o.value == ValueWire::Number(22.0)))));
        host.broadcast_op(op(1, 33.0, 6, 1));
        release_tx.send(()).unwrap();
        let mut sequence = Vec::new();
        let until = std::time::Instant::now() + timeout;
        while !sequence.contains(&33) {
            match viewer.events.recv_timeout(until.saturating_duration_since(std::time::Instant::now())).unwrap() {
                NetEvent::Ops(ops) => {
                    for op in ops {
                        if let ValueWire::Number(n) = op.value { sequence.push(n as u32); }
                    }
                }
                NetEvent::SnapshotDone => sequence.push(1),
                NetEvent::Structural { .. } => sequence.push(2),
                _ => {}
            }
        }
        assert_eq!(sequence, [11, 1, 2, 22, 33]);
    }

    #[test]
    fn joining_receives_every_baseline_style_in_multiple_frames() {
        let _env = crate::test_env::TestEnv::new("join-styled-sheet");
        let (host, port) = host_for_test(1, "Host", "Styles", "style-room");
        let mut wb = sheetcore::Workbook::new_blank();
        let style = sheetcore::style::CellStyle { bold: true, ..Default::default() };
        let id = wb.styles.intern(style.clone());
        for row in 0..2501 { wb.sheet_mut(0).set_style(row, 0, id); }
        host.seed_snapshot(&sheetcore::sync::Peer::new(1), &wb, Cursor {
            sheet: 0, row: 0, col: 0, site: 1, name: "Host".into(), color: 1,
        });
        let client = join(2, "Guest".into(), "Styles".into(), &format!("127.0.0.1:{port}"),
                          "style-room".into()).unwrap();
        let mut rows = std::collections::HashSet::new();
        let mut batches = 0;
        let until = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let ev = client.events.recv_timeout(until.saturating_duration_since(std::time::Instant::now())).unwrap();
            match ev {
                NetEvent::Styles { snapshot: true, items, .. } => {
                    batches += 1;
                    for (sheet, row, col, received) in items {
                        assert_eq!((sheet, col, &received), (0, 0, &style));
                        assert!(row < 2501 && rows.insert(row));
                    }
                }
                NetEvent::SnapshotDone => break,
                _ => {}
            }
        }
        assert!(batches > 1);
        assert_eq!(rows.len(), 2501);
    }

    #[test]
    fn host_and_client_exchange_ops() {
        let (host_net, port) = host_for_test(1, "A", "doc", "test-room");
        host_net.broadcast_op(op(0, 1.0, 1, 1));

        let client = join(
            2,
            "B".into(),
            "doc".into(),
            &format!("127.0.0.1:{port}"),
            "test-room".into(),
        )
        .expect("join");
        assert!(
            wait_for(&client, 3, |e| matches!(e, NetEvent::Ops(v) if v.iter().any(|o| o.value == ValueWire::Number(1.0)))),
            "客户端没收到快照"
        );
        client.broadcast_op(op(1, 42.0, 2, 2));
        assert!(
            wait_for(&host_net, 3, |e| matches!(e, NetEvent::Ops(v) if v.iter().any(|o| o.value == ValueWire::Number(42.0)))),
            "宿主没收到操作"
        );
    }

    #[test]
    fn rename_is_relayed_to_host_and_other_clients() {
        let (host_net, port) = host_for_test(1, "A", "doc", "test-room");
        let addr = format!("127.0.0.1:{port}");
        let c1 = join(2, "B".into(), "doc".into(), &addr, "test-room".into()).expect("join c1");
        assert!(wait_for(&c1, 3, |e| matches!(e, NetEvent::Welcome { .. })));
        let c2 = join(3, "C".into(), "doc".into(), &addr, "test-room".into()).expect("join c2");
        assert!(wait_for(&c2, 3, |e| matches!(e, NetEvent::Welcome { .. })));
        // 等宿主处理完 c2 的 Hello，避免它在收到 Rename 前还没登记 3。
        std::thread::sleep(Duration::from_millis(150));
        c1.rename("B-改名");
        assert!(
            wait_for(&host_net, 3, |e| matches!(
                e,
                NetEvent::PeerRenamed(2, n) if n == "B-改名"
            )),
            "宿主没收到改名"
        );
        assert!(
            wait_for(&c2, 3, |e| matches!(
                e,
                NetEvent::PeerRenamed(2, n) if n == "B-改名"
            )),
            "其他客户端没收到转发"
        );
    }

    #[test]
    fn wrong_password_is_denied() {
        let (_host, port) = host_for_test(1, "A", "doc", "secret");
        let client = join(
            2,
            "B".into(),
            "doc".into(),
            &format!("127.0.0.1:{port}"),
            "wrong".into(),
        )
        .expect("join");
        assert!(
            wait_for(&client, 3, |e| matches!(e, NetEvent::Denied(_))),
            "错误口令应被拒绝"
        );
    }

    #[test]
    fn right_password_connects() {
        let (_host, port) = host_for_test(1, "A", "doc", "secret");
        let client = join(
            2,
            "B".into(),
            "doc".into(),
            &format!("127.0.0.1:{port}"),
            "secret".into(),
        )
        .expect("join");
        assert!(
            wait_for(&client, 3, |e| matches!(e, NetEvent::Connected)),
            "正确口令应连上"
        );
    }

    #[test]
    fn discovery_requires_the_same_workbook_and_zero_password_auto_mode_connects() {
        let (_auto_host, auto_port) = host_for_test(1, "A", "book", "");
        let auto_client = join_discovered(
            2,
            "B".into(),
            "book".into(),
            &format!("127.0.0.1:{auto_port}"),
            "".into(),
        )
        .unwrap();
        assert!(wait_for(&auto_client, 3, |e| matches!(e, NetEvent::Connected)));

        let (_host, port) = host_for_test(1, "A", "current", "");
        let client = join_discovered(
            2,
            "B".into(),
            "previous".into(),
            &format!("127.0.0.1:{port}"),
            "".into(),
        )
        .unwrap();
        assert!(wait_for(&client, 3, |e| matches!(e, NetEvent::Denied(_))));
    }

    #[test]
    fn protecting_a_shared_workbook_closes_authenticated_connections() {
        let _env = crate::test_env::TestEnv::new("protect-open-room");
        let mut app = crate::SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        let (host, port) = host_for_test(1, "A", "book", "test-room");
        let client = join(2, "B".into(), "book".into(), &format!("127.0.0.1:{port}"),
            "test-room".into()).unwrap();
        assert!(wait_for(&client, 3, |e| matches!(e, NetEvent::Connected)));
        app.net = Some(host);
        app.auto_share = true;
        app.set_workbook_open_password("book-password".into());
        assert!(app.net.is_none() && !app.auto_share && app.protected_workbook());
        assert!(wait_for(&client, 3, |e| matches!(e, NetEvent::Disconnected(_))));
    }
}

#[cfg(test)]
mod presence_tests {
    use super::*;
    use super::tests::host_for_test;

    /// 新加入的人必须能在 Welcome 里看到宿主（以前宿主不登记自己，
    /// 刚加入的人只看到自己一个人 = 用户说的「显示不出另一台在线」）。
    #[test]
    fn welcome_lists_the_host_as_a_peer() {
        let (host_net, port) = host_for_test(11, "宿主", "doc", "test-room");
        std::thread::sleep(Duration::from_millis(300));
        let client = join(
            22,
            "同事".into(),
            "doc".into(),
            &format!("127.0.0.1:{port}"),
            "test-room".into(),
        )
        .expect("应能连上宿主");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got: Option<Vec<(u32, String, u8)>> = None;
        while std::time::Instant::now() < deadline {
            match client.events.recv_timeout(Duration::from_millis(200)) {
                Ok(NetEvent::Welcome { peers, .. }) => {
                    got = Some(peers);
                    break;
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        let peers = got.expect("5 秒内应收到 Welcome");
        assert!(
            peers.iter().any(|(s, _, _)| *s == 11),
            "Welcome 里必须包含宿主站点 11，实际 {peers:?}"
        );
        drop(client);
        drop(host_net);
    }

    #[test]
    fn late_joiner_immediately_receives_active_locks_and_cursors() {
        let (host_net, port) = host_for_test(11, "王芳@财务部", "budget.xlsx", "test-room");
        host_net.broadcast_cursor(Cursor {
            site: 11,
            name: "王芳@财务部".into(),
            sheet: 0,
            row: 6,
            col: 3,
            color: 4,
        });
        host_net.request_lock(0, 6, 3, "王芳@财务部");
        std::thread::sleep(Duration::from_millis(120));

        let client = join(
            22,
            "张明@审计组".into(),
            "budget.xlsx".into(),
            &format!("127.0.0.1:{port}"),
            "test-room".into(),
        )
        .expect("后加入成员应能连上宿主");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got_lock = false;
        let mut got_cursor = false;
        while std::time::Instant::now() < deadline && (!got_lock || !got_cursor) {
            if let Ok(ev) = client.events.recv_timeout(Duration::from_millis(200)) {
                match ev {
                    NetEvent::LockOk {
                        site: 11,
                        name,
                        sheet: 0,
                        row: 6,
                        col: 3,
                    } if name == "王芳@财务部" => {
                        got_lock = true;
                    }
                    NetEvent::Cursor(c)
                        if c.site == 11
                            && c.name == "王芳@财务部"
                            && c.sheet == 0
                            && c.row == 6
                            && c.col == 3 =>
                    {
                        got_cursor = true;
                    }
                    _ => {}
                }
            }
        }
        assert!(got_lock, "后加入成员必须立即收到既有单元格锁 LockOk");
        assert!(got_cursor, "后加入成员必须立即收到既有同事光标 Cursor");
        drop(client);
        drop(host_net);
    }
}

#[cfg(test)]
mod integrity_tests {
    use super::*;
    use super::tests::host_for_test;
    use sheetcore::structural::Structural;
    use sheetcore::style::CellStyle;
    use sheetcore::sync::ValueWire;

    fn wait_event<F: Fn(&NetEvent) -> bool>(net: &Net, ms: u64, pred: F) -> Option<NetEvent> {
        let deadline = std::time::Instant::now() + Duration::from_millis(ms);
        while std::time::Instant::now() < deadline {
            if let Ok(ev) = net.events.recv_timeout(Duration::from_millis(50)) {
                if pred(&ev) {
                    return Some(ev);
                }
            }
        }
        None
    }

    /// 打开一条已认证的原始加密客户端连接，用来在协议层直接注入伪造 site/name 的报文。
    fn raw_authenticated_client(
        port: u16,
        site: u32,
        name: &str,
        password: &str,
        auth: &str,
    ) -> (TcpStream, BufReader<TcpStream>, sheetcore::wire::Session) {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect raw client");
        stream
            .set_read_timeout(Some(Duration::from_millis(800)))
            .unwrap();
        let write_half = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let Msg::ChallengeSalt { nonce, salt, room_salt } = serde_json::from_str::<Msg>(line.trim()).unwrap()
        else {
            panic!("expected ChallengeSalt, got {line}");
        };
        let raw_salt = sheetcore::wire::b64_decode(&salt).unwrap();
        let room_raw = sheetcore::wire::b64_decode(&room_salt).unwrap();
        let room = sheetcore::wire::RoomKey::derive(password, &room_raw);
        let mut sess = sheetcore::wire::Session::client_side(&room, &raw_salt);
        let hello = Msg::Hello {
            site,
            name: name.into(),
            doc: "doc".into(),
            proof: room.proof(nonce),
            auth: auth.into(),
            base_hash: String::new(),
        };
        let mut w = write_half;
        let frame = sess.seal(&serde_json::to_string(&hello).unwrap());
        write_line(&mut w, &frame).unwrap();
        (w, reader, sess)
    }

    #[test]
    fn duplicate_or_impersonated_site_is_denied_and_duplicate_names_are_disambiguated() {
        let (host, port) = host_for_test(11, "王芳", "doc", "room-pw");
        let addr = format!("127.0.0.1:{port}");

        // 1. 冒用宿主站点号 11 入房必须被拒绝
        let spoof_host = join(11, "李四".into(), "doc".into(), &addr, "room-pw".into()).unwrap();
        assert!(
            wait_event(&spoof_host, 3000, |e| matches!(e, NetEvent::Denied(_))).is_some(),
            "冒用宿主 site=11 必须被拒绝"
        );

        // 2. 合法成员 site=22 使用与宿主相同的显示名「王芳」入房：允许加入但自动消歧为「王芳#22」
        let c1 = join(22, "王芳".into(), "doc".into(), &addr, "room-pw".into()).unwrap();
        assert!(
            wait_event(&host, 3000, |e| matches!(e, NetEvent::PeerJoined(22, n, _) if n == "王芳#22")).is_some(),
            "同名入房成员必须被宿主消歧为 王芳#22"
        );
        assert!(
            wait_event(&c1, 3000, |e| matches!(e, NetEvent::PeerRenamed(22, n) if n == "王芳#22")).is_some(),
            "同名客户端自身也必须收到消歧后的正式名称"
        );

        // 3. 在线期间第三人冒用已在线的 site=22 入房：必须被拒绝且不得踢掉真 site=22
        let spoof_active = join(22, "赵六".into(), "doc".into(), &addr, "room-pw".into()).unwrap();
        assert!(
            wait_event(&spoof_active, 3000, |e| matches!(e, NetEvent::Denied(_))).is_some(),
            "撞用在线成员 site=22 必须被拒绝"
        );

        // 4. c1 试图通过 Rename 改名为「王芳」冒充宿主：仍会被宿主强制消歧为「王芳#22」
        c1.rename("王芳");
        assert!(
            wait_event(&host, 3000, |e| matches!(e, NetEvent::PeerRenamed(22, n) if n == "王芳#22")).is_some(),
            "Rename 撞名宿主也必须被消歧"
        );

        // 5. c1 退出后，另一个新客户端试图复用历史 site=22（不同 auth 令牌）必须被拒绝
        drop(c1);
        let _ = wait_event(&host, 3000, |e| matches!(e, NetEvent::PeerLeft(22)));
        let reuse_historical =
            join(22, "后来者".into(), "doc".into(), &addr, "room-pw".into()).unwrap();
        assert!(
            wait_event(&reuse_historical, 3000, |e| matches!(e, NetEvent::Denied(_))).is_some(),
            "不同客户端实例复用历史成员 site=22 必须被拒绝"
        );
    }

    #[test]
    fn admitted_member_cannot_spoof_other_site_or_release_others_locks_and_writes_keep_sender_site() {
        let (host, port) = host_for_test(1, "宿主", "doc", "room-pw");
        let addr = format!("127.0.0.1:{port}");

        let victim = join(2, "甲".into(), "doc".into(), &addr, "room-pw".into()).unwrap();
        assert!(wait_event(&victim, 3000, |e| matches!(e, NetEvent::Welcome { .. })).is_some());
        victim.request_lock(0, 3, 2, "甲");
        assert!(
            wait_event(&host, 3000, |e| matches!(e, NetEvent::LockOk { site: 2, row: 3, col: 2, .. })).is_some()
        );

        // 已获准入房成员 site=3 通过原始加密通道发送伪造 site=2 / site=1 的各类控制与写入消息
        let (mut w, mut reader, mut sess) =
            raw_authenticated_client(port, 3, "乙", "room-pw", "raw-auth-3");
        for _ in 0..3 {
            let mut buf = String::new();
            let _ = reader.read_line(&mut buf);
        }
        assert!(wait_event(&host, 3000, |e| matches!(e, NetEvent::PeerJoined(3, _, _))).is_some());

        let forged_frames = [
            Msg::LockRelease { site: 2, sheet: 0, row: 3, col: 2 },
            Msg::Op(Op {
                sheet: 0,
                row: 0,
                col: 0,
                value: ValueWire::Number(999.0),
                formula: None,
                ts: 50,
                site: 1,
            }),
            Msg::Rename { site: 2, name: "被篡改".into() },
            Msg::StyleOp { site: 2, sheet: 0, ts: 1, items: vec![(0, 0, CellStyle::default())] },
            Msg::MergeOp { site: 2, sheet: 0, ts: 1, r1: 0, c1: 0, r2: 1, c2: 1, on: true },
            Msg::Structural { site: 2, sheet: 0, op: Structural::InsertRows { at: 10, n: 1 } },
            Msg::Bye { site: 2 },
        ];
        for msg in forged_frames {
            let sealed = sess.seal(&serde_json::to_string(&msg).unwrap());
            write_line(&mut w, &sealed).unwrap();
        }

        // 再发一条真实署名为 site=3 的合并与结构改动（at=10 不触碰甲在 row=3 的锁），验证署名完整保留为 3
        let valid_merge = Msg::MergeOp {
            site: 3,
            sheet: 0,
            ts: 2,
            r1: 8,
            c1: 0,
            r2: 8,
            c2: 1,
            on: true,
        };
        write_line(&mut w, &sess.seal(&serde_json::to_string(&valid_merge).unwrap())).unwrap();
        let valid_struct = Msg::Structural {
            site: 3,
            sheet: 0,
            op: Structural::InsertRows { at: 10, n: 1 },
        };
        write_line(&mut w, &sess.seal(&serde_json::to_string(&valid_struct).unwrap())).unwrap();

        let mut saw_merge_3 = false;
        let mut saw_struct_3 = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline && (!saw_merge_3 || !saw_struct_3) {
            if let Ok(ev) = host.events.recv_timeout(Duration::from_millis(100)) {
                match ev {
                    NetEvent::LockRelease { site: 2, row: 3, col: 2, .. } => {
                        panic!("伪造的跨站点 LockRelease 不得释放甲持有的锁");
                    }
                    NetEvent::Ops(ops) if ops.iter().any(|o| o.value == ValueWire::Number(999.0)) => {
                        panic!("伪造 site=1 的 Op 不得被宿主接受");
                    }
                    NetEvent::PeerRenamed(2, _) => {
                        panic!("伪造 site=2 的 Rename 不得生效");
                    }
                    NetEvent::Merge { site, rect: (8, 0, 8, 1), on: true, .. } => {
                        assert_eq!(site, 3, "Merge 事件必须保留真实发起者 site=3");
                        saw_merge_3 = true;
                    }
                    NetEvent::Structural { site, op: Structural::InsertRows { at: 10, n: 1 }, .. } => {
                        assert_eq!(site, 3, "Structural 事件必须保留真实发起者 site=3");
                        saw_struct_3 = true;
                    }
                    _ => {}
                }
            }
        }
        assert!(saw_merge_3 && saw_struct_3, "合法署名的 Merge/Structural 应正常送达");
    }

    #[test]
    fn unauthenticated_oversized_line_is_cut_off_without_buffering_it() {
        let (host, port) = host_for_test(1, "宿主", "doc", "room-pw");
        let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut challenge = String::new();
        reader.read_line(&mut challenge).unwrap();
        assert!(challenge.contains("ChallengeSalt"));
        // 不带换行地灌入远超认证前上限的数据：宿主必须断开，而不是一直攒内存等换行
        let chunk = vec![b'A'; 16 * 1024];
        let mut cut_off = false;
        for _ in 0..64 {
            if s.write_all(&chunk).is_err() {
                cut_off = true;
                break;
            }
        }
        if !cut_off {
            // 对端关闭后读到 EOF 或连接被重置；若宿主还在傻等换行，这里会读超时
            let mut rest = Vec::new();
            cut_off = match std::io::Read::read_to_end(&mut reader, &mut rest) {
                Ok(_) => true,
                Err(e) => !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
            };
        }
        assert!(cut_off, "超长的未认证报文必须让宿主断开连接");
        // 宿主本身仍然正常：合法成员照样能进
        let ok = join(2, "甲".into(), "doc".into(), &format!("127.0.0.1:{port}"), "room-pw".into()).unwrap();
        assert!(wait_event(&ok, 5000, |e| matches!(e, NetEvent::Welcome { .. })).is_some());
        drop(host);
    }

    #[test]
    fn authenticated_member_cannot_push_out_of_range_coordinates_or_clock() {
        let (host, port) = host_for_test(1, "宿主", "doc", "room-pw");
        let (mut w, mut reader, mut sess) =
            raw_authenticated_client(port, 3, "乙", "room-pw", "raw-auth-oob");
        for _ in 0..3 {
            let mut buf = String::new();
            let _ = reader.read_line(&mut buf);
        }
        assert!(wait_event(&host, 3000, |e| matches!(e, NetEvent::PeerJoined(3, _, _))).is_some());

        let op = |row: u32, col: u32, ts: u64, v: f64| {
            Msg::Op(Op { sheet: 0, row, col, value: ValueWire::Number(v), formula: None, ts, site: 3 })
        };
        let hostile = [
            op(0, 4_000_000_000, 5, 1.0),
            op(2_000_000, 0, 5, 2.0),
            op(0, 0, u64::MAX, 3.0),
            Msg::Structural { site: 3, sheet: 0, op: Structural::InsertRows { at: 5, n: u32::MAX } },
            Msg::MergeOp { site: 3, sheet: 0, ts: 1, r1: 0, c1: 0, r2: 0, c2: 99_999, on: true },
            Msg::StyleOp { site: 3, sheet: 0, ts: 1, items: vec![(0, 70_000, CellStyle::default())] },
        ];
        for msg in hostile {
            write_line(&mut w, &sess.seal(&serde_json::to_string(&msg).unwrap())).unwrap();
        }
        // 最后一条合法编辑：它能到达，说明前面越界的报文只是被丢弃，连接仍然有效
        write_line(&mut w, &sess.seal(&serde_json::to_string(&op(1, 1, 6, 42.0)).unwrap())).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut saw_valid = false;
        while std::time::Instant::now() < deadline && !saw_valid {
            if let Ok(ev) = host.events.recv_timeout(Duration::from_millis(100)) {
                match ev {
                    NetEvent::Ops(ops) => {
                        for o in &ops {
                            assert!(o.is_valid(), "越界操作不得进入宿主：{o:?}");
                            if o.value == ValueWire::Number(42.0) {
                                saw_valid = true;
                            }
                        }
                    }
                    NetEvent::Structural { .. } => panic!("溢出的结构改动不得被接受"),
                    NetEvent::Merge { .. } => panic!("越界合并不得被接受"),
                    NetEvent::Styles { .. } => panic!("越界样式不得被接受"),
                    _ => {}
                }
            }
        }
        assert!(saw_valid, "越界报文之后的合法编辑应正常送达");
    }

    #[test]
    fn hundred_thousand_numeric_edits_keep_order_across_frame_boundaries() {
        let count = 100_000;
        let ops: Vec<_> = (0..count).map(|row| Op {
            sheet: 0, row, col: 0, value: ValueWire::Number(row as f64),
            formula: None, ts: u64::from(row) + 1, site: 2,
        }).collect();
        let mut next = 0;
        let mut frames = 0;
        cell_frames(ops, true, true, |line| {
            let Msg::Ops { ops } = serde_json::from_str(&line).unwrap() else { panic!("expected batch") };
            assert!(ops.len() <= sheetcore::sync::LIVE_OP_BATCH);
            for op in ops {
                assert_eq!(op.row, next);
                next += 1;
            }
            frames += 1;
        });
        assert_eq!(next, count);
        assert_eq!(frames, (count as usize).div_ceil(sheetcore::sync::LIVE_OP_BATCH));
    }

    #[test]
    fn negotiated_batch_reaches_host_and_peer_in_order_and_preserves_locked_cell() {
        let (host, port) = host_for_test(1, "host", "doc", "room-pw");
        let addr = format!("127.0.0.1:{port}");
        let a = join(2, "writer".into(), "doc".into(), &addr, "room-pw".into()).unwrap();
        let b = join(3, "reader".into(), "doc".into(), &addr, "room-pw".into()).unwrap();
        for peer in [&a, &b] {
            assert!(wait_event(peer, 5000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());
        }
        b.request_lock(0, 4, 0, "reader");
        assert!(wait_event(&host, 5000, |e| matches!(e, NetEvent::LockOk { site: 3, row: 4, .. })).is_some());
        let ops: Vec<_> = (0..10).map(|row| Op {
            sheet: 0, row, col: 0, value: ValueWire::Number(row as f64), formula: None,
            ts: u64::from(row) + 1, site: 2,
        }).collect();
        let mut legacy = Vec::new();
        cell_frames(ops.clone(), true, false, |line| legacy.push(serde_json::from_str::<Msg>(&line).unwrap()));
        assert!(legacy.len() == 10 && legacy.iter().all(|m| matches!(m, Msg::Op(_))));
        a.broadcast_ops(ops);
        for net in [&host, &b] {
            let Some(NetEvent::Ops(ops)) = wait_event(net, 5000, |e| matches!(e, NetEvent::Ops(_))) else { panic!("missing batch"); };
            assert_eq!(ops.iter().map(|op| op.row).collect::<Vec<_>>(), [0,1,2,3,5,6,7,8,9]);
            assert!(ops.iter().all(|op| op.site == 2));
        }
        assert!(wait_event(&a, 5000, |e| matches!(e, NetEvent::WriteRejected { row: 4, holder: 3, .. })).is_some());
    }

    #[test]
    fn host_enforces_edit_leases_against_conflicting_ops_styles_merges_and_structural_edits() {
        let (host, port) = host_for_test(1, "宿主", "doc", "room-pw");
        host.broadcast_op(Op {
            sheet: 0,
            row: 3,
            col: 2,
            value: ValueWire::Number(100.0),
            formula: None,
            ts: 1,
            site: 1,
        });
        let addr = format!("127.0.0.1:{port}");
        let holder = join(2, "持锁人".into(), "doc".into(), &addr, "room-pw".into()).unwrap();
        let intruder = join(3, "闯入者".into(), "doc".into(), &addr, "room-pw".into()).unwrap();
        assert!(wait_event(&holder, 3000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());
        assert!(wait_event(&intruder, 3000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());

        // 持锁人先锁 (0, 1, 1)，再锁 (0, 3, 2)，验证旧锁会自动释放（防单成员批量占锁）
        holder.request_lock(0, 1, 1, "持锁人");
        assert!(wait_event(&host, 3000, |e| matches!(e, NetEvent::LockOk { site: 2, row: 1, col: 1, .. })).is_some());
        holder.request_lock(0, 3, 2, "持锁人");
        assert!(wait_event(&host, 3000, |e| matches!(e, NetEvent::LockRelease { site: 2, row: 1, col: 1, .. })).is_some());
        assert!(wait_event(&host, 3000, |e| matches!(e, NetEvent::LockOk { site: 2, row: 3, col: 2, .. })).is_some());

        // 闯入者绕过前端直接向 (0, 3, 2) 发送 Op、StyleOp、覆盖该格的 MergeOp 及上方插行 Structural
        intruder.broadcast_op(Op {
            sheet: 0,
            row: 3,
            col: 2,
            value: ValueWire::Number(777.0),
            formula: None,
            ts: 99,
            site: 3,
        });
        assert!(
            wait_event(
                &intruder,
                3000,
                |e| matches!(e, NetEvent::WriteRejected { holder: 2, row: 3, col: 2, revert_ops, .. }
                    if revert_ops.first().and_then(|(_, _, o)| o.as_ref()).is_some_and(|o| o.value == ValueWire::Number(100.0)))
            )
            .is_some(),
            "闯入者写他人持锁格必须收到 WriteRejected 及权威旧值 100.0 回滚"
        );

        let mut bold = CellStyle::default();
        bold.bold = true;
        let mut requested: Vec<_> = (0..700).map(|row| (row, 4, bold.clone())).collect();
        requested[300] = (3, 2, bold);
        intruder.broadcast_style(0, requested);
        assert!(
            wait_event(&intruder, 3000, |e| matches!(e, NetEvent::WriteRejected { holder: 2, row: 3, col: 2, .. })).is_some(),
            "闯入者改他人持锁格样式必须被驳回"
        );

        assert!(host.host_styles.as_ref().unwrap().lock().unwrap().is_empty(),
            "the held cell in the middle must reject all 700 styles");

        intruder.broadcast_merge(0, (2, 1, 4, 3), true);
        assert!(
            wait_event(&intruder, 3000, |e| matches!(e, NetEvent::WriteRejected { holder: 2, revert_merge: Some((2, 1, 4, 3, false)), .. })).is_some(),
            "闯入者合并覆盖他人持锁格必须被驳回并附带反向解合指令"
        );

        intruder.broadcast_structural(0, &Structural::InsertRows { at: 1, n: 1 }).unwrap();
        assert!(
            wait_event(&intruder, 3000, |e| matches!(e, NetEvent::LockDeny { holder: 2, row: 3, col: 2, .. })).is_some(),
            "闯入者在持锁格上方插行必须被宿主拒绝"
        );

        // 宿主自身若直接向持锁格广播写入，也同样受权威租约表拦截
        host.broadcast_op(Op {
            sheet: 0,
            row: 3,
            col: 2,
            value: ValueWire::Number(888.0),
            formula: None,
            ts: 100,
            site: 1,
        });
        assert!(
            wait_event(&host, 2000, |e| matches!(e, NetEvent::WriteRejected { holder: 2, row: 3, col: 2, .. })).is_some(),
            "宿主自身也不能越过客户端持有的有效编辑租约"
        );

        // 持锁人正常写入 250.0 并释放锁后，宿主与其他客户端均应收到 250.0
        holder.broadcast_op(Op {
            sheet: 0,
            row: 3,
            col: 2,
            value: ValueWire::Number(250.0),
            formula: None,
            ts: 5,
            site: 2,
        });
        holder.release_lock(0, 3, 2);
        assert!(
            wait_event(&host, 3000, |e| matches!(e, NetEvent::Ops(v) if v.iter().any(|o| o.value == ValueWire::Number(250.0)))).is_some(),
            "持锁人自身写入必须成功生效"
        );
    }

    #[cfg(windows)]
    #[test]
    fn collab_peer_relay_transfers_signed_update_across_multiple_slices_and_blocks_forged_sig() {
        let env = crate::test_env::TestEnv::new("collab-peer-update");
        let official_kp = crate::update::cng::TestKeyPair::generate();
        let forged_kp = crate::update::cng::TestKeyPair::generate();
        env.sc("SC_UPDATE_PUBKEY", &official_kp.pubkey_hex);

        // 构造跨越多个 192KB 分片的测试安装包（~420 KB）验证流式重组与尾部完整性校验
        let raw_exe: Vec<u8> = (0..(420 * 1024)).map(|i| ((i * 31 + 7) & 0xFF) as u8).collect();
        let (manifest, full_exe) = crate::update::make_test_signed_release(
            &official_kp,
            "99.2",
            &raw_exe,
            "协作无感接力更新",
        );
        let seed_exe = env.path("seed-CELLAN.exe");
        std::fs::write(&seed_exe, &full_exe).unwrap();

        let (host, port) = host_for_test(1, "宿主旧版", "budget.xlsx", "pw");
        let client_new = join(
            2,
            "已升级成员".into(),
            "budget.xlsx".into(),
            &format!("127.0.0.1:{port}"),
            "pw".into(),
        )
        .unwrap();
        let client_old = join(
            3,
            "待升级成员".into(),
            "budget.xlsx".into(),
            &format!("127.0.0.1:{port}"),
            "pw".into(),
        )
        .unwrap();

        assert!(wait_event(&client_new, 5000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());
        assert!(wait_event(&client_old, 5000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());

        // 伪造签名的通告必须被宿主静默拦截，不会产生任何 PeerUpdateOffer
        let (evil_manifest, _) =
            crate::update::make_test_signed_release(&forged_kp, "99.9", b"fake", "evil");
        let forged_wire = serde_json::to_string(&Msg::UpdateAnnounce {
            site: 2,
            version: evil_manifest.version,
            size: evil_manifest.size,
            sha256: evil_manifest.sha256,
            sig: evil_manifest.sig,
            notes: evil_manifest.notes,
        })
        .unwrap();
        let _ = client_new.cmd.send(Cmd::Broadcast(forged_wire));

        // 合法签名的新版成员（site=2）宣告更新 → 宿主（site=1）与另一旧版客户端（site=3）均收到通告
        client_new.set_update_bundle_for_test(seed_exe.clone(), manifest.clone());
        client_new.announce_local_signed_update();

        let host_offer = wait_event(&host, 5000, |e| {
            matches!(e, NetEvent::PeerUpdateOffer { peer_site: 2, manifest: m, .. } if m.version == "99.2")
        });
        assert!(host_offer.is_some(), "宿主应收到合法签名的 99.2 更新通告");

        let peer_offer = wait_event(&client_old, 5000, |e| {
            matches!(e, NetEvent::PeerUpdateOffer { peer_site: 2, manifest: m, .. } if m.version == "99.2")
        });
        assert!(peer_offer.is_some(), "同房间的旧版客户端也应经宿主转发收到 99.2 更新通告");

        // 客户端 3 经宿主路由从客户端 2 拉取完整签名包并完成双重校验
        client_old.request_peer_update(2, &manifest).unwrap();
        let ready = wait_event(&client_old, 8000, |e| {
            matches!(e, NetEvent::PeerUpdateReady { .. } | NetEvent::PeerUpdateFailed(_))
        })
        .expect("应在时限内收到更新完成事件");
        match ready {
            NetEvent::PeerUpdateReady {
                version,
                verified_exe,
            } => {
                assert_eq!(version, "99.2");
                assert_eq!(std::fs::read(&verified_exe).unwrap(), full_exe);
                assert!(crate::update::inspect_signed_exe(&verified_exe).is_some());
            }
            other => panic!("预期 PeerUpdateReady，实际收到：{other:?}"),
        }
    }
}
