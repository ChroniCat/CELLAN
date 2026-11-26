//! 局域网协作传输层：星形拓扑（宿主转发）+ 行分隔 JSON 帧。
//!
//! 所有会话使用房间口令和随机盐建立带认证的加密通道；完成握手后才交换工作簿数据。

use sha2::{Digest, Sha256};
use crate::net_sort::{AxisStylesMap, Grid, RoomGrids, SharedBaseline, SharedGrid, SheetAxisStyles, SortBatch, shift_grid, upsert};
use sheetcore::sync::{AxisStylePatch, BaselineBook, Cursor, Msg, Op, SortResponse};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

#[derive(Clone, Debug)]
pub(crate) struct SortVote {
    sender: Arc<Mutex<Option<Sender<bool>>>>,
    connection: Weak<()>,
}

impl SortVote {
    pub fn send(&self, value: bool) -> Result<(), ()> {
        let _connection = self.connection.upgrade().ok_or(())?;
        self.sender.lock().unwrap().take().ok_or(())?.send(value).map_err(|_| ())
    }

    pub fn pending(&self) -> bool {
        self.connection.strong_count() != 0 && self.sender.lock().unwrap().is_some()
    }

    /// The UI and deadline compete for a single vote. When it times out, ordinary
    /// cell frames can proceed and a late GUI completion cannot apply the permutation.
    fn wait(rx: Receiver<bool>, sender: Weak<Mutex<Option<Sender<bool>>>>, deadline: Duration) -> bool {
        match rx.recv_timeout(deadline) {
            Ok(value) => value,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(sender) = sender.upgrade()
                    && let Some(tx) = sender.lock().unwrap().take() {
                    let _ = tx.send(false);
                }
                rx.recv().unwrap_or(false)
            }
            Err(_) => false,
        }
    }
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
    Sort(sheetcore::sync::SortPermutation, SortVote),
    SortReady(crate::net_sort::SortTarget, Option<Arc<SortBatch>>, SortVote),
    Sorted(Arc<SortBatch>),
    CommittedSortStyles(Arc<SortBatch>),
    ConfirmedStyles(Vec<(usize,u32,u32,sheetcore::style::CellStyle)>),
    SortRepairDone(u64),
    LocalSortRepair(Arc<SortCorrection>),
    SortRepairs { sheet:usize, first_ts:u64, items:Vec<sheetcore::sync::SortRepair> },
    /// 宿主初始/重连快照接收完毕
    SnapshotDone,
    Cursor(Cursor),
    /// 某格被批准编辑（含自己）
    LockOk { site: u32, name: String, sheet: usize, row: u32, col: u32 },
    /// 结构改动历史（加入时补）
    StructuralHistory(Vec<(usize, sheetcore::structural::Structural)>),
    StructuralRejected(String),
    StructureRevision(usize),
    /// 远端结构性改动（带已认证站点号）
    Structural { site: u32, sheet: usize, op: sheetcore::structural::Structural },
    /// 远端格式变更（`snapshot=true` 表示入房补发的既有样式，不记修订日志）
    Styles {
        site: u32,
        items: Vec<(usize, u32, u32, sheetcore::style::CellStyle)>,
        snapshot: bool,
    },
    /// 远端整行/整列默认样式变更
    AxisStyles {
        site: u32,
        sheet: usize,
        patch: AxisStylePatch,
        snapshot: bool,
    },
    /// 远端合并变更
    Merge { site: u32, sheet: usize, rect: (u32, u32, u32, u32), on: bool },
    /// 远端工作表高频元数据变更（列宽/行高/隐藏/冻结/批注/超链接/筛选/标签结构）
    SheetMeta {
        site: u32,
        sheet: usize,
        kind: sheetcore::sync::SheetMetaKind,
        snapshot: bool,
    },
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
    Saved { clock: u64, watermark_seq: Option<u64> },
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
    BroadcastMsg(Msg),
    GuestStructure(u64,Msg),
    Cells(Vec<Op>),
    SharedCells(Vec<Arc<Op>>),
    Style(Msg),
    Deliver(Vec<LiveSender>, RoomChange),
    Sort(u64,Arc<SortBatch>),
    SendTo(u32, String),
    SendPrepared(u32, Arc<sheetcore::wire::PreparedPayload>),
    PumpUpdate,
    Stop,
}

/// Recipients are captured in the same order as the corresponding host state
/// updates. Encoding happens on the dispatcher after releasing the state lock.
enum RoomChange {
    Sort(Arc<SortBatch>),
    Correction(Arc<SortCorrection>),
    Cells(Vec<Op>),
    SharedCells(Vec<Arc<Op>>),
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
        let size = text_len + op.formula.as_deref().map_or(0, str::len) + 128;
        if !pending.is_empty() && (!batch || pending.len() == sheetcore::sync::LIVE_OP_BATCH || bytes + size > 256 * 1024) {
            flush(std::mem::take(&mut pending));
            bytes = 0;
        }
        bytes += size;
        pending.push(op);
    }
    if !pending.is_empty() { flush(pending); }
}

fn cell_frames_msg(ops: impl IntoIterator<Item = Op>, to_host: bool, batch: bool, mut send: impl FnMut(Msg)) {
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
        send(msg);
    };
    for op in ops {
        let text_len = match &op.value { ValueWire::Text(s) | ValueWire::Error(s) => s.len(), _ => 0 };
        let size = text_len + op.formula.as_deref().map_or(0, str::len) + 96;
        if !pending.is_empty() && (!batch || pending.len() == sheetcore::sync::LIVE_OP_BATCH || bytes + size > 256 * 1024) {
            flush(std::mem::take(&mut pending));
            bytes = 0;
        }
        bytes += size;
        pending.push(op);
    }
    if !pending.is_empty() { flush(pending); }
}

fn shared_cell_frames_to_peers(ops: &[Arc<Op>], peers: &[LiveSender]) {
    if peers.is_empty() || ops.is_empty() {
        return;
    }
    use sheetcore::sync::ValueWire;
    let mut start = 0usize;
    let mut bytes = 0usize;
    let flush_slice = |slice: &[Arc<Op>]| {
        if slice.is_empty() {
            return;
        }
        let any_compact = peers.iter().any(|p| p.is_compact_v3());
        let any_legacy = peers.iter().any(|p| !p.is_compact_v3());
        let compact_prep = any_compact.then(|| {
            // Host -> Guest cell frames use SnapshotChunk { last: false } when len > 1, or Op when len == 1
            let raw = if slice.len() == 1 {
                sheetcore::sync::encode_arc_ops_compact(slice)
            } else {
                let refs: Vec<&Op> = slice.iter().map(Arc::as_ref).collect();
                sheetcore::sync::encode_snapshot_chunk_ref_compact(&refs, false)
            };
            sheetcore::wire::prepare_compact_payload(raw, true)
        });
        let legacy_prep = any_legacy.then(|| {
            let line = if slice.len() == 1 {
                serde_json::to_string(&Msg::Op((*slice[0]).clone())).unwrap()
            } else {
                let refs: Vec<&Op> = slice.iter().map(Arc::as_ref).collect();
                serde_json::to_string(&sheetcore::sync::SnapshotChunkRef {
                    t: "SnapshotChunk",
                    ops: &refs,
                    last: false,
                })
                .unwrap()
            };
            sheetcore::wire::prepare_payload(&line, true)
        });
        for peer in peers {
            if peer.is_compact_v3() {
                if let Some(ref p) = compact_prep {
                    let _ = peer.send_prepared(Arc::clone(p));
                }
            } else if let Some(ref p) = legacy_prep {
                let _ = peer.send_prepared(Arc::clone(p));
            }
        }
    };
    for (idx, op) in ops.iter().enumerate() {
        let text_len = match &op.value { ValueWire::Text(s) | ValueWire::Error(s) => s.len(), _ => 0 };
        let size = text_len + op.formula.as_deref().map_or(0, str::len) + 96;
        if idx > start && (idx - start >= sheetcore::sync::LIVE_OP_BATCH || bytes + size > 256 * 1024) {
            flush_slice(&ops[start..idx]);
            start = idx;
            bytes = 0;
        }
        bytes += size;
    }
    if start < ops.len() {
        flush_slice(&ops[start..]);
    }
}

const LOW_PRIO_UPDATE_CAP: usize = 4;

type LowPrioUpdateQueue = Arc<(
    Mutex<(std::collections::VecDeque<Arc<sheetcore::wire::PreparedPayload>>, bool)>,
    std::sync::Condvar,
)>;

enum LiveFrame {
    Line(String),
    Compact(Vec<u8>),
    Prepared(Arc<sheetcore::wire::PreparedPayload>),
    Sort(Arc<SortBatch>),
    Correction(Arc<SortCorrection>),
    PumpUpdate,
}

#[derive(Clone)]
struct LiveSender {
    tx: Sender<LiveFrame>,
    compact_v3: Arc<AtomicBool>,
    low_prio_update: LowPrioUpdateQueue,
}

impl LiveSender {
    fn new(tx: Sender<LiveFrame>, compact_v3: Arc<AtomicBool>) -> (Self, LowPrioUpdateQueue) {
        let q: LowPrioUpdateQueue = Arc::new((
            Mutex::new((std::collections::VecDeque::with_capacity(LOW_PRIO_UPDATE_CAP), false)),
            std::sync::Condvar::new(),
        ));
        (
            Self {
                tx,
                compact_v3,
                low_prio_update: q.clone(),
            },
            q,
        )
    }

    #[inline]
    fn is_compact_v3(&self) -> bool {
        self.compact_v3.load(Ordering::Acquire)
    }

    fn send(&self, line: String) -> Result<(), ()> {
        self.tx.send(LiveFrame::Line(line)).map_err(|_| ())
    }

    fn send_compact(&self, raw: Vec<u8>) -> Result<(), ()> {
        self.tx.send(LiveFrame::Compact(raw)).map_err(|_| ())
    }

    fn send_prepared(&self, payload: Arc<sheetcore::wire::PreparedPayload>) -> Result<(), ()> {
        self.tx.send(LiveFrame::Prepared(payload)).map_err(|_| ())
    }

    /// 低优先级投递 P2P 自更新分片：队列上限 `LOW_PRIO_UPDATE_CAP` 背压限流，且写线程始终优先排空实时协作帧。
    fn send_update_slice(&self, payload: Arc<sheetcore::wire::PreparedPayload>) -> Result<(), ()> {
        let (lock, cvar) = &*self.low_prio_update;
        let mut guard = lock.lock().unwrap();
        while guard.0.len() >= LOW_PRIO_UPDATE_CAP && !guard.1 {
            guard = cvar.wait(guard).unwrap();
        }
        if guard.1 {
            return Err(());
        }
        guard.0.push_back(payload);
        drop(guard);
        self.tx.send(LiveFrame::PumpUpdate).map_err(|_| ())
    }
}

/// 广播同一明文帧给多个在线 peer 时，先预压缩一次再通过 `Arc<PreparedPayload>` 零拷贝共享给各写线程。
fn broadcast_line_to(peers: &[LiveSender], line: String) {
    match peers.len() {
        0 => {}
        1 => {
            let _ = peers[0].send(line);
        }
        _ => {
            let prepared = sheetcore::wire::prepare_payload(&line, true);
            for peer in peers {
                let _ = peer.send_prepared(Arc::clone(&prepared));
            }
        }
    }
}

/// 广播结构化 `Msg` 给多个在线 peer：`compact_v3` 节点共享单份紧凑二进制预压缩帧，旧版节点按需共享 JSON 帧。
fn broadcast_msg_to(peers: &[LiveSender], msg: &Msg) {
    if peers.is_empty() {
        return;
    }
    if peers.len() == 1 {
        if peers[0].is_compact_v3() {
            let _ = peers[0].send_compact(sheetcore::sync::encode_msg_compact(msg));
        } else if let Ok(line) = serde_json::to_string(msg) {
            let _ = peers[0].send(line);
        }
        return;
    }
    let any_compact = peers.iter().any(|p| p.is_compact_v3());
    let any_legacy = peers.iter().any(|p| !p.is_compact_v3());
    let compact_prep = any_compact.then(|| {
        sheetcore::wire::prepare_compact_payload(sheetcore::sync::encode_msg_compact(msg), true)
    });
    let legacy_prep = if any_legacy {
        serde_json::to_string(msg)
            .ok()
            .map(|line| sheetcore::wire::prepare_payload(&line, true))
    } else {
        None
    };
    for peer in peers {
        if peer.is_compact_v3() {
            if let Some(ref p) = compact_prep {
                let _ = peer.send_prepared(Arc::clone(p));
            }
        } else if let Some(ref p) = legacy_prep {
            let _ = peer.send_prepared(Arc::clone(p));
        }
    }
}

fn ordinary_sort_frames(batch: &SortBatch, to_host: bool, batched: bool, mut send: impl FnMut(String) -> bool) -> bool {
    let mut ok = true;
    cell_frames(batch.ops.iter().map(|o| (**o).clone()), to_host, batched, |line| { if ok { ok = send(line); } });
    for chunk in batch.styles.chunks(sheetcore::sync::LIVE_OP_BATCH) {
        if !ok { break; }
        let msg = Msg::StyleOp { site: batch.plan.site, sheet: batch.plan.sheet, ts: 0, items: chunk.to_vec() };
        ok = send_room_message(msg, MAX_PLAIN_LINE, &mut send, || {});
    }
    ok
}

type RepairKeys = Vec<(u32,u32,Option<sheetcore::style::CellStyle>)>;

fn sort_repair_keys(batch: &SortBatch, mut emit: impl FnMut(u64,RepairKeys) -> bool) -> bool {
    let mut styles = batch.styles.iter().peekable();
    for chunk in batch.ops.chunks(sheetcore::sync::LIVE_OP_BATCH) {
        let keys = chunk.iter().map(|op| {
            let style = styles.next_if(|s| (s.0,s.1)==(op.row,op.col)).map(|s| s.2.clone());
            (op.row,op.col,style)
        }).collect();
        if !emit(chunk[0].ts,keys) { return false; }
    }
    true
}

// Captured at the ordered rejection point. Encoding a large rollback happens
// on the receiver's writer; subsequent room frames remain behind it.
#[derive(Debug)]
pub(crate) struct SortCorrection {
    batch: Arc<SortBatch>,
    baseline: Option<Arc<BaselineBook>>,
    axis_styles: AxisStylesMap,
    cells: Arc<Grid<Op>>,
    styles: Arc<Grid<sheetcore::style::CellStyle>>,
    edits: Vec<(usize,sheetcore::structural::Structural)>,
}

impl SortCorrection {
    fn capture(
        batch: Arc<SortBatch>,
        baseline: &SharedBaseline,
        axis_styles: AxisStylesMap,
        cells: &SharedGrid<Op>,
        styles: &SharedStyles,
        edits: &[(usize, sheetcore::structural::Structural)],
    ) -> Arc<Self> {
        Arc::new(Self {
            batch,
            baseline: baseline.lock().unwrap().clone(),
            axis_styles,
            cells: cells.lock().unwrap().clone(),
            styles: styles.lock().unwrap().clone(),
            edits: edits.to_vec(),
        })
    }

    pub fn chunks(&self, mut send: impl FnMut(usize,u64,Vec<sheetcore::sync::SortRepair>) -> bool) -> bool {
        let sheet = self.batch.plan.sheet;
        sort_repair_keys(&self.batch, |first,keys| {
            send(
                sheet,
                first,
                sort_repairs(
                    self.baseline.as_deref(),
                    &self.axis_styles,
                    &self.cells,
                    &self.styles,
                    sheet,
                    first,
                    keys,
                    &self.edits,
                ),
            )
        })
    }
}

fn sort_repairs(
    baseline: Option<&BaselineBook>,
    axis_styles: &AxisStylesMap,
    cells: &Grid<Op>,
    styles: &Grid<sheetcore::style::CellStyle>,
    sheet: usize,
    first_ts: u64,
    keys: RepairKeys,
    edits: &[(usize, sheetcore::structural::Structural)],
) -> Vec<sheetcore::sync::SortRepair> {
    keys.into_iter().enumerate().filter_map(|(index,(row,col,expected))| {
        // Structural frames preceding this correction have already relocated the
        // optimistic operation on the reader. Deleted slots need no value repair.
        let (row,col) = edits.iter().filter(|(s,_)| *s == sheet).try_fold((row,col),|(r,c),(_,edit)|
            sheetcore::sync::shift_structural_coord(r,c,edit))?;
        let key = (sheet,row,col);
        let cell = cells
            .get(&key)
            .map(|v| (**v).clone())
            .or_else(|| baseline.and_then(|b| b.cell_op(sheet, row, col)));
        let cur_style = styles
            .get(&key)
            .map(|s| (**s).clone())
            .or_else(|| baseline.and_then(|b| b.cell_style(sheet, row, col)))
            .or_else(|| {
                axis_styles
                    .get(&sheet)
                    .and_then(|ax| ax.get(row, col))
                    .map(|s| (**s).clone())
            });
        Some(sheetcore::sync::SortRepair {
            rejected_ts: first_ts + index as u64,
            row,
            col,
            cell,
            style: expected.map(|was| (was, cur_style)),
        })
    }).collect()
}

/// One offer at a time on each direction of a TCP connection. Replies use the
/// independent authenticated write path, so simultaneous opposing offers cannot deadlock.
fn send_sort(batch: &SortBatch, capable: bool, to_host: bool, batched: bool,
             replies: &Receiver<(u64,SortResponse)>, mut send: impl FnMut(String) -> bool) -> bool {
    if capable {
        let offer = Msg::SortPermutation(batch.offer());
        if !send(serde_json::to_string(&offer).unwrap()) { return false; }
        let Some(outcome) = replies.iter().find_map(|(id,v)| (id == batch.plan.first_ts).then_some(v)) else {
            return false; // The read loop closed this connection's acknowledgement channel.
        };
        if outcome == SortResponse::Accepted { return true; }
        if outcome == SortResponse::NeedCells && to_host {
            let first_ts = batch.plan.first_ts;
            let mut fmts = batch.styles.iter().peekable();
            for ops in batch.ops.chunks(sheetcore::sync::LIVE_OP_BATCH) {
                let items = ops.iter().map(|op| {
                    let style = fmts.next_if(|s| (s.0,s.1)==(op.row,op.col)).map(|s| s.2.clone());
                    ((**op).clone(),style)
                }).collect();
                if !send_room_message(Msg::SortData { first_ts,items }, MAX_PLAIN_LINE, &mut send, || {}) { return false; }
            }
            return send(serde_json::to_string(&Msg::SortData { first_ts,items:Vec::new() }).unwrap());
        }
        if let SortResponse::Locked { structural_revision } = outcome && to_host {
            return sort_repair_keys(batch, |first_ts,keys| {
                send_room_message(Msg::SortRepairRequest { sheet: batch.plan.sheet, first_ts, structural_revision, keys }, MAX_PLAIN_LINE, &mut send, || {})
            }) && send(serde_json::to_string(&Msg::SortRepairDone { first_ts: batch.plan.first_ts }).unwrap());
        }
    }
    ordinary_sort_frames(batch, to_host, batched, send)
}

struct RoomRoster {
    host_site: u32,
    peers: HashMap<u32, LiveSender>,
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

    fn finish_admit(&mut self, site: u32, tx: LiveSender) -> Vec<LiveSender> {
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

    fn remove_member(&mut self, site: u32) -> Vec<LiveSender> {
        self.connecting.remove(&site);
        if site != self.host_site {
            self.peers.remove(&site);
            self.names.remove(&site);
        }
        self.peers.values().cloned().collect()
    }

    fn other_senders(&self, exclude: Option<u32>) -> Vec<LiveSender> {
        self.peers
            .iter()
            .filter(|(s, _)| Some(**s) != exclude)
            .map(|(_, ptx)| ptx.clone())
            .collect()
    }

    fn all_senders(&self) -> Vec<LiveSender> {
        self.peers.values().cloned().collect()
    }

    fn sender_of(&self, site: u32) -> Option<LiveSender> {
        self.peers.get(&site).cloned()
    }
}

struct ActivePeerDownload {
    peer_site: u32,
    manifest: crate::update::SignedManifest,
    file: std::fs::File,
    part_path: std::path::PathBuf,
    prefix_hash: crate::update::PrefixSha256,
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
const MAX_PENDING_SEED_JOBS: usize = 16;

struct UpdateSeedJob {
    my_site: u32,
    to_site: u32,
    req_version: String,
    seed_override: SharedUpdateSeed,
    alive: Arc<AtomicBool>,
    use_compact_bin: Arc<AtomicBool>,
    send_slice: Box<dyn Fn(Arc<sheetcore::wire::PreparedPayload>) -> bool + Send + 'static>,
}

#[derive(Clone, Default)]
struct UpdateSeederQueue {
    inner: Arc<(Mutex<UpdateSeederInner>, std::sync::Condvar)>,
}

#[derive(Default)]
struct UpdateSeederInner {
    queue: std::collections::VecDeque<UpdateSeedJob>,
    worker_running: bool,
    stopped: bool,
}

fn make_update_slice_payload(
    from_site: u32,
    to_site: u32,
    version: &str,
    offset: u64,
    total: u64,
    chunk: &[u8],
    done: bool,
    compact_bin: bool,
) -> Arc<sheetcore::wire::PreparedPayload> {
    if compact_bin {
        let raw = sheetcore::sync::encode_update_slice_compact(
            from_site, to_site, version, offset, total, chunk, done,
        );
        sheetcore::wire::prepare_compact_payload(raw, false)
    } else {
        let msg = Msg::UpdateSlice {
            from_site,
            to_site,
            version: version.to_string(),
            offset,
            total,
            data_b64: sheetcore::wire::b64_encode(chunk),
            raw_data: Vec::new(),
            done,
        };
        let json = serde_json::to_string(&msg).unwrap_or_default();
        sheetcore::wire::prepare_payload(&json, false)
    }
}

impl UpdateSeederQueue {
    fn enqueue(&self, job: UpdateSeedJob) {
        let (lock, cvar) = &*self.inner;
        let mut g = lock.lock().unwrap();
        if g.stopped {
            return;
        }
        if g.queue
            .iter()
            .any(|j| j.to_site == job.to_site && j.req_version == job.req_version)
        {
            return;
        }
        if g.queue.len() >= MAX_PENDING_SEED_JOBS {
            let compact = job.use_compact_bin.load(Ordering::Acquire);
            let reject = make_update_slice_payload(
                job.my_site,
                job.to_site,
                &job.req_version,
                0,
                0,
                &[],
                true,
                compact,
            );
            drop(g);
            let _ = (job.send_slice)(reject);
            return;
        }
        g.queue.push_back(job);
        if !g.worker_running {
            g.worker_running = true;
            let runner = self.inner.clone();
            std::thread::spawn(move || {
                let (lock, cvar) = &*runner;
                let mut buf = vec![0u8; UPDATE_SLICE_RAW_BYTES];
                loop {
                    let job = {
                        let mut g = lock.lock().unwrap();
                        if g.stopped {
                            g.queue.clear();
                            g.worker_running = false;
                            return;
                        }
                        match g.queue.pop_front() {
                            Some(j) => j,
                            None => {
                                let (mut g2, _) =
                                    cvar.wait_timeout(g, Duration::from_millis(50)).unwrap();
                                if g2.stopped {
                                    g2.queue.clear();
                                    g2.worker_running = false;
                                    return;
                                }
                                match g2.queue.pop_front() {
                                    Some(j) => j,
                                    None => {
                                        g2.worker_running = false;
                                        return;
                                    }
                                }
                            }
                        }
                    };
                    run_single_seed_job(job, &mut buf);
                }
            });
        } else {
            cvar.notify_one();
        }
    }

    fn shutdown(&self) {
        let (lock, cvar) = &*self.inner;
        let mut g = lock.lock().unwrap();
        g.stopped = true;
        g.queue.clear();
        cvar.notify_all();
    }
}

fn run_single_seed_job(job: UpdateSeedJob, buf: &mut [u8]) {
    if !job.alive.load(Ordering::Acquire) {
        return;
    }
    let Some((exe_path, manifest)) = resolve_local_seed(&job.seed_override) else {
        return;
    };
    if manifest.version != job.req_version || !crate::update::verify_manifest_sig(&manifest) {
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
    let mut offset = 0u64;
    while job.alive.load(Ordering::Acquire) {
        let n = match std::io::Read::read(&mut f, buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let next_off = offset + n as u64;
        let done = next_off >= total;
        let compact = job.use_compact_bin.load(Ordering::Acquire);
        let payload = make_update_slice_payload(
            job.my_site,
            job.to_site,
            &manifest.version,
            offset,
            total,
            &buf[..n],
            done,
            compact,
        );
        if !(job.send_slice)(payload) {
            break;
        }
        offset = next_off;
        if done {
            break;
        }
    }
}

fn accept_incoming_update_slice(
    dl_slot: &Arc<Mutex<Option<ActivePeerDownload>>>,
    evt_tx: &EvtTx,
    from_site: u32,
    version: &str,
    offset: u64,
    total: u64,
    data_b64: &str,
    raw_data: &[u8],
    done: bool,
) {
    if total == 0 && done {
        let mut g = dl_slot.lock().unwrap();
        if g.as_ref()
            .is_some_and(|dl| dl.peer_site == from_site && dl.manifest.version == version)
            && let Some(dl) = g.take()
        {
            drop(dl.file);
            let _ = std::fs::remove_file(&dl.part_path);
            let _ = evt_tx.send(NetEvent::PeerUpdateFailed(
                "供种节点当前繁忙，请稍后重试".into(),
            ));
        }
        return;
    }
    let decoded_b64;
    let chunk: &[u8] = if !raw_data.is_empty() || data_b64.is_empty() {
        raw_data
    } else {
        let Some(bytes) = sheetcore::wire::b64_decode(data_b64) else {
            let mut g = dl_slot.lock().unwrap();
            if let Some(dl) = g.take() {
                let _ = std::fs::remove_file(&dl.part_path);
                let _ = evt_tx.send(NetEvent::PeerUpdateFailed("更新分片解码失败".into()));
            }
            return;
        };
        decoded_b64 = bytes;
        &decoded_b64
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
        dl.prefix_hash.update(&chunk);
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
        let streamed_sha = completed.prefix_hash.finalize_hex();
        let evt_tx = evt_tx.clone();
        std::thread::spawn(move || {
            match crate::update::verify_and_finalize_staged_exe_with_sha(
                &completed.part_path,
                &completed.manifest,
                streamed_sha.as_deref(),
            ) {
                Ok(()) => {
                    let verified_path = completed.part_path.with_extension("verified.exe");
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
    t0: std::time::Instant,
    last_cursor_wake_ms: Arc<AtomicU64>,
    last_snap_wake_ms: Arc<AtomicU64>,
}

impl EvtTx {
    fn new(tx: Sender<NetEvent>) -> (Self, Arc<Mutex<Option<eframe::egui::Context>>>) {
        let wake = Arc::new(Mutex::new(None));
        (
            Self {
                tx,
                wake: wake.clone(),
                t0: std::time::Instant::now(),
                last_cursor_wake_ms: Arc::new(AtomicU64::new(0)),
                last_snap_wake_ms: Arc::new(AtomicU64::new(0)),
            },
            wake,
        )
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

    /// 立即入队光标事件（保证单测零延迟可见），但将 UI 帧重绘唤醒限频在 60Hz（16ms）以内。
    fn send_cursor(&self, ev: NetEvent) -> Result<(), std::sync::mpsc::SendError<NetEvent>> {
        let res = self.tx.send(ev);
        if res.is_ok() {
            let now = self.t0.elapsed().as_millis() as u64 + 1;
            let prev = self.last_cursor_wake_ms.load(Ordering::Relaxed);
            if now.saturating_sub(prev) >= 16 {
                self.last_cursor_wake_ms.store(now, Ordering::Relaxed);
                if let Ok(guard) = self.wake.lock()
                    && let Some(ctx) = guard.as_ref()
                {
                    ctx.request_repaint();
                }
            } else if let Ok(guard) = self.wake.lock()
                && let Some(ctx) = guard.as_ref()
            {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        }
        res
    }

    /// 初始快照分片到达时合并 UI 唤醒（32ms 节流），消除 740 次 `request_repaint()` 风暴。
    fn send_snapshot_batch(&self, ev: NetEvent) -> Result<(), std::sync::mpsc::SendError<NetEvent>> {
        let res = self.tx.send(ev);
        if res.is_ok() {
            let now = self.t0.elapsed().as_millis() as u64 + 1;
            let prev = self.last_snap_wake_ms.load(Ordering::Relaxed);
            if now.saturating_sub(prev) >= 32 {
                self.last_snap_wake_ms.store(now, Ordering::Relaxed);
                if let Ok(guard) = self.wake.lock()
                    && let Some(ctx) = guard.as_ref()
                {
                    ctx.request_repaint();
                }
            }
        }
        res
    }
}

type SharedStyles = SharedGrid<sheetcore::style::CellStyle>;

#[derive(Default)]
struct StructureProgress {
    seen: usize,
    // The host may apply an edit to B before the UI polls an earlier edit to A.
    // Publishing a global revision must wait until all preceding edits were seen.
    applied_out_of_order: std::collections::BTreeSet<usize>,
}

impl StructureProgress {
    fn consume(&mut self, count: usize) {
        self.seen += count;
        while self.applied_out_of_order.remove(&self.seen) { self.seen += 1; }
    }
}

type SeqKey = (usize, u32, u32);
type SeqMap = immutable_chunkmap::map::MapM<SeqKey, u64>;
type AxisSeqTable = HashMap<usize, BTreeMap<u32, (Option<sheetcore::style::CellStyle>, u64)>>;
type MergeWatermarkLog = Vec<(u64, usize, u32, u32, u32, u32, bool)>;

const SEQ_LOG_CHUNK_SIZE: usize = 1024;

/// 按 `seq` 单调追加的分块 CoW 变更日志：
/// - `clone()` 为严格 `O(1)`（仅增加外层 `Arc` 引用计数）；
/// - 写时仅对尾块执行 CoW（<= 1024 项），避免快照存在时复制整条历史；
/// - 增量恢复通过两级 `partition_point` 切片直达 `seq > resume_watermark`，复杂度 `O(log N + Δ)`。
#[derive(Clone, Debug)]
struct SeqLog {
    chunks: Arc<Vec<Arc<Vec<(u64, SeqKey)>>>>,
    len: usize,
}

impl Default for SeqLog {
    fn default() -> Self {
        Self {
            chunks: Arc::new(Vec::new()),
            len: 0,
        }
    }
}

impl SeqLog {
    fn clear(&mut self) {
        self.chunks = Arc::new(Vec::new());
        self.len = 0;
    }

    fn push(&mut self, seq: u64, key: SeqKey) {
        let chunks = Arc::make_mut(&mut self.chunks);
        let need_new = chunks
            .last()
            .is_none_or(|c| c.len() >= SEQ_LOG_CHUNK_SIZE);
        if need_new {
            let mut next = Vec::with_capacity(64);
            next.push((seq, key));
            chunks.push(Arc::new(next));
        } else {
            let tail = Arc::make_mut(chunks.last_mut().unwrap());
            tail.push((seq, key));
        }
        self.len += 1;
    }

    fn since(&self, after_seq: u64) -> impl Iterator<Item = (u64, SeqKey)> + '_ {
        let start_chunk = self
            .chunks
            .partition_point(|ch| ch.last().is_some_and(|(s, _)| *s <= after_seq));
        self.chunks[start_chunk..]
            .iter()
            .enumerate()
            .flat_map(move |(idx, ch)| {
                let start = if idx == 0 {
                    ch.partition_point(|(s, _)| *s <= after_seq)
                } else {
                    0
                };
                ch[start..].iter().copied()
            })
    }

    /// 同一单元格被反复覆盖导致历史冗余过高时压缩重建，限制长会话内存上限。
    fn maybe_compact(&mut self, latest: &SeqMap) {
        if self.len <= latest.len().saturating_mul(4).max(16_384) {
            return;
        }
        let mut entries: Vec<(u64, SeqKey)> =
            latest.range(..).map(|(&k, &seq)| (seq, k)).collect();
        entries.sort_unstable_by_key(|&(seq, _)| seq);
        let total = entries.len();
        let mut chunks = Vec::with_capacity(total.div_ceil(SEQ_LOG_CHUNK_SIZE));
        for part in entries.chunks(SEQ_LOG_CHUNK_SIZE) {
            chunks.push(Arc::new(part.to_vec()));
        }
        self.chunks = Arc::new(chunks);
        self.len = total;
    }
}

fn shift_seq_map(map: &mut SeqMap, sheet: usize, op: &sheetcore::structural::Structural) {
    use sheetcore::structural::Structural::*;
    let source = map.clone();
    let mut moved = Vec::new();
    let mut shift = |first, last| {
        for (&key, &seq) in source.range(first..=last) {
            map.remove_cow(&key);
            if let Some((r, c)) = sheetcore::sync::shift_structural_coord(key.1, key.2, op) {
                moved.push(((sheet, r, c), seq));
            }
        }
    };
    let last = (sheet, u32::MAX, u32::MAX);
    match *op {
        InsertRows { at, .. } | DeleteRows { at, .. } => shift((sheet, at, 0), last),
        InsertCols { at: 0, .. } | DeleteCols { at: 0, .. } => shift((sheet, 0, 0), last),
        InsertCols { at, .. } | DeleteCols { at, .. } => {
            let mut from = 0;
            while let Some((&(_, row, _), _)) = source.range((sheet, from, 0)..=last).next() {
                shift((sheet, row, at), (sheet, row, u32::MAX));
                let Some(next) = row.checked_add(1) else {
                    break;
                };
                from = next;
            }
        }
    }
    for (key, seq) in moved {
        map.insert_cow(key, seq);
    }
}

const MAX_SAVE_CHECKPOINTS: usize = 8;

/// 后台在加入者监督线程中直接组装完成的列式工作簿快照，
/// 供 UI 线程在 `SnapshotDone` 时以 `O(页数)` 原子移交，消除 50 万次 `BTreeMap` 插入与 `CellStyle` 深拷贝。
#[allow(dead_code)]
pub struct PreparedGuestSnapshot {
    pub sheets: Vec<sheetcore::Sheet>,
    pub styles: sheetcore::style::StyleTable,
    pub has_seed_styles: bool,
    pub merge_ops: Vec<(usize, u32, u32, u32, u32, bool)>,
    pub overlay_ops: Vec<Op>,
    pub max_ts: u64,
    pub host_site: u32,
}

#[allow(dead_code)]
impl PreparedGuestSnapshot {
    pub fn install(self, wb: &mut sheetcore::Workbook, peer: Option<&mut sheetcore::sync::Peer>) {
        sheetcore::sync::install_guest_snapshot_into_workbook(
            wb,
            self.sheets,
            self.styles,
            self.has_seed_styles,
            &self.merge_ops,
        );
        if let Some(p) = peer {
            p.clock = p.clock.max(self.max_ts).max(1);
            p.cells.clear();
            let base = BaselineBook::from_workbook(self.host_site.max(1), wb);
            p.cells.set_baseline(base);
            for op in self.overlay_ops {
                p.apply(op);
            }
        }
    }
}

#[derive(Clone, Debug)]
struct HostWatermark {
    seq: u64,
    op_seqs: SeqMap,
    style_seqs: SeqMap,
    log_floor_seq: u64,
    op_log: SeqLog,
    style_log: SeqLog,
    axis_rows: Arc<AxisSeqTable>,
    axis_cols: Arc<AxisSeqTable>,
    cached_axis: Option<AxisStylesMap>,
    merges: Arc<MergeWatermarkLog>,
    /// 带单调水位序号的工作表元数据日志（批注、行高列宽、隐藏、冻结、超链接、筛选、工作表标签），
    /// 供水位增量重连精确补发断线期间的 `SheetMetaOp`。
    sheet_meta: Arc<Vec<(u64, usize, sheetcore::sync::SheetMetaKind)>>,
    meta_floor_seq: u64,
    /// 最近几次保存原文件的 `(file_sha256, watermark_seq, structure_revision)` 检查点环，
    /// 使宿主保存后打开共享文件的同事仍可直接走 `O(Δ)` 水位增量入房而非退化为全量快照。
    checkpoints: Arc<std::collections::VecDeque<(String, u64, usize)>>,
}

impl Default for HostWatermark {
    fn default() -> Self {
        Self {
            seq: 1,
            op_seqs: SeqMap::new(),
            style_seqs: SeqMap::new(),
            log_floor_seq: 0,
            op_log: SeqLog::default(),
            style_log: SeqLog::default(),
            axis_rows: Arc::new(HashMap::new()),
            axis_cols: Arc::new(HashMap::new()),
            cached_axis: None,
            merges: Arc::new(Vec::new()),
            sheet_meta: Arc::new(Vec::new()),
            meta_floor_seq: 0,
            checkpoints: Arc::new(std::collections::VecDeque::new()),
        }
    }
}

impl HostWatermark {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn record_checkpoint(&mut self, hash: String, wm_seq: u64, struct_rev: usize) {
        if hash.is_empty() {
            return;
        }
        let cps = Arc::make_mut(&mut self.checkpoints);
        cps.retain(|(h, _, _)| h != &hash);
        if cps.len() >= MAX_SAVE_CHECKPOINTS {
            cps.pop_front();
        }
        cps.push_back((hash, wm_seq.max(1), struct_rev));
    }

    fn find_checkpoint(&self, hash: &str) -> Option<(u64, usize)> {
        if hash.is_empty() {
            return None;
        }
        self.checkpoints
            .iter()
            .rev()
            .find(|(h, _, _)| h == hash)
            .map(|&(_, wm, srev)| (wm, srev))
    }

    fn axis_styles_snapshot(&mut self) -> AxisStylesMap {
        if let Some(cached) = &self.cached_axis {
            return cached.clone();
        }
        let mut map = HashMap::new();
        let all_sheets: BTreeSet<usize> = self
            .axis_rows
            .keys()
            .chain(self.axis_cols.keys())
            .copied()
            .collect();
        for si in all_sheets {
            let mut sa = SheetAxisStyles::default();
            if let Some(rm) = self.axis_rows.get(&si) {
                for (&r, (st_opt, _)) in rm {
                    if let Some(st) = st_opt {
                        sa.rows.insert(r, Arc::new(st.clone()));
                    }
                }
            }
            if let Some(cm) = self.axis_cols.get(&si) {
                for (&c, (st_opt, _)) in cm {
                    if let Some(st) = st_opt {
                        sa.cols.insert(c, Arc::new(st.clone()));
                    }
                }
            }
            if !sa.rows.is_empty() || !sa.cols.is_empty() {
                map.insert(si, sa);
            }
        }
        let shared = Arc::new(map);
        self.cached_axis = Some(shared.clone());
        shared
    }

    fn bump(&mut self) -> u64 {
        self.seq = self.seq.saturating_add(1);
        self.seq
    }

    fn record_ops<'a>(&mut self, ops: impl IntoIterator<Item = &'a Op>) -> u64 {
        let seq = self.bump();
        for op in ops {
            let key = (op.sheet, op.row, op.col);
            if self.op_seqs.insert_cow(key, seq) != Some(seq) {
                self.op_log.push(seq, key);
            }
        }
        self.op_log.maybe_compact(&self.op_seqs);
        seq
    }

    fn record_styles(
        &mut self,
        sheet: usize,
        items: &[(u32, u32, sheetcore::style::CellStyle)],
    ) -> u64 {
        self.record_style_coords(sheet, items.iter().map(|&(r, c, _)| (r, c)))
    }

    fn record_style_coords(
        &mut self,
        sheet: usize,
        coords: impl IntoIterator<Item = (u32, u32)>,
    ) -> u64 {
        let seq = self.bump();
        for (r, c) in coords {
            let key = (sheet, r, c);
            if self.style_seqs.insert_cow(key, seq) != Some(seq) {
                self.style_log.push(seq, key);
            }
        }
        self.style_log.maybe_compact(&self.style_seqs);
        seq
    }

    fn record_sort(&mut self, batch: &SortBatch) -> u64 {
        let seq = self.bump();
        for op in &batch.ops {
            let key = (op.sheet, op.row, op.col);
            if self.op_seqs.insert_cow(key, seq) != Some(seq) {
                self.op_log.push(seq, key);
            }
        }
        for &(r, c, _) in &batch.styles {
            let key = (batch.plan.sheet, r, c);
            if self.style_seqs.insert_cow(key, seq) != Some(seq) {
                self.style_log.push(seq, key);
            }
        }
        self.op_log.maybe_compact(&self.op_seqs);
        self.style_log.maybe_compact(&self.style_seqs);
        seq
    }

    fn record_axis(&mut self, sheet: usize, patch: &AxisStylePatch) -> u64 {
        let seq = self.bump();
        self.cached_axis = None;
        if !patch.rows.is_empty() {
            let rm = Arc::make_mut(&mut self.axis_rows).entry(sheet).or_default();
            for (r, st) in &patch.rows {
                rm.insert(*r, (st.clone(), seq));
            }
        }
        if !patch.cols.is_empty() {
            let cm = Arc::make_mut(&mut self.axis_cols).entry(sheet).or_default();
            for (c, st) in &patch.cols {
                cm.insert(*c, (st.clone(), seq));
            }
        }
        seq
    }

    fn record_merge(&mut self, sheet: usize, rect: (u32, u32, u32, u32), on: bool) -> u64 {
        let seq = self.bump();
        Arc::make_mut(&mut self.merges)
            .push((seq, sheet, rect.0, rect.1, rect.2, rect.3, on));
        seq
    }

    fn record_sheet_meta(&mut self, sheet: usize, kind: sheetcore::sync::SheetMetaKind) -> u64 {
        let seq = self.bump();
        let log = Arc::make_mut(&mut self.sheet_meta);
        if log.len() >= 10_000 {
            let evicted = log.remove(0);
            self.meta_floor_seq = self.meta_floor_seq.max(evicted.0);
        }
        log.push((seq, sheet, kind));
        seq
    }

    fn shift_structural(&mut self, sheet: usize, op: &sheetcore::structural::Structural) -> u64 {
        self.cached_axis = None;
        shift_seq_map(&mut self.op_seqs, sheet, op);
        shift_seq_map(&mut self.style_seqs, sheet, op);
        use sheetcore::structural::Structural as S;
        match *op {
            S::InsertRows { at, n } => {
                if self.axis_rows.contains_key(&sheet) {
                    if let Some(rm) = Arc::make_mut(&mut self.axis_rows).get_mut(&sheet) {
                        let moved: Vec<_> = rm
                            .extract_if(at.., |_, _| true)
                            .map(|(r, v)| (r.saturating_add(n), v))
                            .collect();
                        rm.extend(moved);
                    }
                }
            }
            S::DeleteRows { at, n } => {
                if self.axis_rows.contains_key(&sheet) {
                    if let Some(rm) = Arc::make_mut(&mut self.axis_rows).get_mut(&sheet) {
                        let moved: Vec<_> = rm
                            .extract_if(at.., |_, _| true)
                            .filter_map(|(r, v)| (r >= at.saturating_add(n)).then_some((r - n, v)))
                            .collect();
                        rm.extend(moved);
                    }
                }
            }
            S::InsertCols { at, n } => {
                if self.axis_cols.contains_key(&sheet) {
                    if let Some(cm) = Arc::make_mut(&mut self.axis_cols).get_mut(&sheet) {
                        let moved: Vec<_> = cm
                            .extract_if(at.., |_, _| true)
                            .map(|(c, v)| (c.saturating_add(n), v))
                            .collect();
                        cm.extend(moved);
                    }
                }
            }
            S::DeleteCols { at, n } => {
                if self.axis_cols.contains_key(&sheet) {
                    if let Some(cm) = Arc::make_mut(&mut self.axis_cols).get_mut(&sheet) {
                        let moved: Vec<_> = cm
                            .extract_if(at.., |_, _| true)
                            .filter_map(|(c, v)| (c >= at.saturating_add(n)).then_some((c - n, v)))
                            .collect();
                        cm.extend(moved);
                    }
                }
            }
        }
        let seq = self.bump();
        self.log_floor_seq = seq;
        self.op_log.clear();
        self.style_log.clear();
        seq
    }
}

pub struct Net {
    pub site: u32,
    structure_seen: Mutex<StructureProgress>,
    sheet_names: Arc<Mutex<Vec<String>>>,
    structural_ack: Arc<AtomicBool>,
    palette_styles_cap: Arc<AtomicBool>,
    connection_generation: Arc<AtomicU64>,
    pending_structure: Arc<AtomicBool>,
    #[cfg(test)]
    unconfirmed_styles: Arc<AtomicU64>,
    last_watermark_seq: Arc<AtomicU64>,
    min_saved_watermark: Arc<AtomicU64>,
    pending_sorts: Arc<Mutex<std::collections::BTreeMap<u64,Arc<SortBatch>>>>,
    local_sort_ranges: Mutex<Vec<(u64,u64)>>,
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
    host_baseline: Option<SharedBaseline>,
    formula_keys: Option<Arc<Mutex<BTreeSet<(usize, u32, u32)>>>>,
    host_watermark: Option<Arc<Mutex<HostWatermark>>>,
    host_max_clock: Option<Arc<AtomicU64>>,
    host_wake_addr: Option<std::net::SocketAddr>,
    state: Option<SharedGrid<Op>>,
    delta_ops: Option<SharedGrid<Op>>,
    host_base_hash: Option<Arc<Mutex<String>>>,
    host_roster: Option<Arc<Mutex<RoomRoster>>>,
    room_order: Option<Arc<Mutex<()>>>,
    #[cfg(test)]
    after_snapshot: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    #[cfg(test)]
    pub(crate) before_sort_reply: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    #[cfg(test)]
    pub(crate) before_sort_commit: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    /// 宿主自己也是参与者：本地租约表 + 光标表 + 事件入口
    host_locks: Option<Arc<Mutex<sheetcore::sync::LockTable>>>,
    host_cursors: Option<Arc<Mutex<HashMap<u32, Cursor>>>>,
    host_styles:
        Option<SharedStyles>,
    delta_styles:
        Option<SharedStyles>,
    delta_merges: Option<Arc<Mutex<Vec<(usize, u32, u32, u32, u32, bool)>>>>,
    host_meta_log: Option<Arc<Mutex<Vec<(usize, sheetcore::sync::SheetMetaKind)>>>>,
    host_structs: Option<Arc<Mutex<Vec<(usize, sheetcore::structural::Structural)>>>>,
    host_file_path: Option<Arc<Mutex<Option<std::path::PathBuf>>>>,
    staged_save_cursor: Mutex<Option<(u64, u64, usize)>>,
    peer_update_dl: Arc<Mutex<Option<ActivePeerDownload>>>,
    update_seed_override: SharedUpdateSeed,
    seeder_queue: UpdateSeederQueue,
    cursor_throttle: CursorSendThrottle,
    fast_snapshot_handoff: Arc<AtomicBool>,
    seed_styles: Arc<Mutex<Option<sheetcore::style::StyleTable>>>,
    ready_snapshot: Arc<Mutex<Option<PreparedGuestSnapshot>>>,
    evt_tx: EvtTx,
    t0: std::time::Instant,
}

const CURSOR_SEND_THROTTLE: Duration = Duration::from_millis(33);

#[derive(Default)]
struct CursorSendState {
    last_sent: Option<std::time::Instant>,
    pending: Option<Cursor>,
    worker_armed: bool,
    stopped: bool,
}

#[derive(Clone, Default)]
struct CursorSendThrottle {
    inner: Arc<(Mutex<CursorSendState>, std::sync::Condvar)>,
}

impl CursorSendThrottle {
    fn send_throttled(&self, cur: Cursor, cmd: &Sender<Cmd>, alive: &Arc<AtomicBool>) {
        let (lock, _) = &*self.inner;
        let mut g = lock.lock().unwrap();
        if g.stopped {
            return;
        }
        let now = std::time::Instant::now();
        if !g.worker_armed
            && g.last_sent
                .is_none_or(|t| now.saturating_duration_since(t) >= CURSOR_SEND_THROTTLE)
        {
            // 前缘 0ms 立即发送：首次或间隔 >= 33ms 直接投递，零额外延迟、零辅助线程
            g.last_sent = Some(now);
            g.pending = None;
            drop(g);
            let _ = cmd.send(Cmd::BroadcastMsg(Msg::CursorMsg(cur)));
            return;
        }
        // 33ms 窗口内高频连发：合并覆盖单槽 `pending`，尾随定时补发确保不丢最终位置
        g.pending = Some(cur);
        if !g.worker_armed {
            g.worker_armed = true;
            let runner = self.inner.clone();
            let cmd = cmd.clone();
            let alive = alive.clone();
            std::thread::spawn(move || {
                let (lock, cvar) = &*runner;
                let mut g = lock.lock().unwrap();
                loop {
                    if g.stopped || !alive.load(Ordering::Acquire) {
                        g.pending = None;
                        g.worker_armed = false;
                        return;
                    }
                    let wait_dur = g
                        .last_sent
                        .map(|t| {
                            (t + CURSOR_SEND_THROTTLE)
                                .saturating_duration_since(std::time::Instant::now())
                        })
                        .unwrap_or(Duration::ZERO);
                    if !wait_dur.is_zero() {
                        let (g2, _) = cvar.wait_timeout(g, wait_dur).unwrap();
                        g = g2;
                        continue;
                    }
                    let Some(latest) = g.pending.take() else {
                        g.worker_armed = false;
                        return;
                    };
                    g.last_sent = Some(std::time::Instant::now());
                    drop(g);
                    if cmd.send(Cmd::BroadcastMsg(Msg::CursorMsg(latest))).is_err() {
                        let mut g_err = lock.lock().unwrap();
                        g_err.pending = None;
                        g_err.worker_armed = false;
                        return;
                    }
                    g = lock.lock().unwrap();
                    if g.pending.is_none() {
                        g.worker_armed = false;
                        return;
                    }
                }
            });
        }
    }

    fn rename_pending(&self, canonical: &str) {
        let (lock, _) = &*self.inner;
        let mut g = lock.lock().unwrap();
        if let Some(cur) = g.pending.as_mut() {
            cur.name = canonical.to_string();
        }
    }

    fn shutdown(&self) {
        let (lock, cvar) = &*self.inner;
        let mut g = lock.lock().unwrap();
        g.stopped = true;
        g.pending = None;
        cvar.notify_all();
    }
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

fn write_line_opt<W: Write>(stream: &mut W, s: &str, flush: bool) -> std::io::Result<()> {
    stream.write_all(s.as_bytes())?;
    stream.write_all(b"\n")?;
    if flush {
        stream.flush()
    } else {
        Ok(())
    }
}

fn write_line<W: Write>(stream: &mut W, s: &str) -> std::io::Result<()> {
    write_line_opt(stream, s, true)
}

fn write_binary_frame_opt<W: Write>(stream: &mut W, frame: &[u8], flush: bool) -> std::io::Result<()> {
    stream.write_all(frame)?;
    if flush {
        stream.flush()
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn write_binary_frame<W: Write>(stream: &mut W, frame: &[u8]) -> std::io::Result<()> {
    write_binary_frame_opt(stream, frame, true)
}

/// 宿主写线程内部控制前缀：明文未加密拒绝报文（认证失败时发给尚未建立会话的对端）
const CTRL_RAW_LINE_PREFIX: char = '\u{1}';
/// 宿主写线程内部控制哨兵：写完 v2 `Welcome` 后原子切换至 v3 二进制帧封装（JSON 内层）
const CTRL_ENABLE_BINARY_V3: &str = "\u{2}";
/// 宿主写线程内部控制哨兵：写完 v2 `Welcome` 后原子切换至 v3 紧凑二进制帧封装（`V3_FLAG_COMPACT_BIN`）
const CTRL_ENABLE_COMPACT_V3: &str = "\u{3}";

enum IntroFrame {
    Line(String),
    Compact(Vec<u8>),
}

enum WireFrame {
    Text(String),
    Binary(Vec<u8>),
}

/// 首字节自识别读取一帧：
/// - 首字节为 `BINARY_V3_MAGIC (0x02)` 且 `allow_binary == true` 时，按 `[0x02][4B LE len][body]` 读入 v3 二进制帧；
/// - 否则按原有 v2 行分隔文本帧读取（`\n` 结束）。
/// 读取端无需依赖切换时序即可同时支持 v2 与 v3，彻底消除协议切换边界竞态。
fn read_wire_frame<R: BufRead>(
    r: &mut R,
    max: usize,
    allow_binary: bool,
) -> std::io::Result<Option<WireFrame>> {
    let first = loop {
        match r.fill_buf() {
            Ok(b) if b.is_empty() => return Ok(None),
            Ok(b) => break b[0],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    };
    if first == sheetcore::wire::BINARY_V3_MAGIC {
        if !allow_binary {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "未完成握手前不允许二进制帧",
            ));
        }
        r.consume(1);
        let mut len_buf = [0u8; 4];
        std::io::Read::read_exact(r, &mut len_buf)?;
        let body_len = u32::from_le_bytes(len_buf) as usize;
        if body_len < sheetcore::wire::BINARY_V3_MIN_BODY_LEN || body_len > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "二进制报文长度越界",
            ));
        }
        let mut body = Vec::with_capacity(body_len.min(256 * 1024));
        let mut limited = std::io::Read::take(&mut *r, body_len as u64);
        let n = std::io::Read::read_to_end(&mut limited, &mut body)?;
        if n != body_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "二进制报文截断",
            ));
        }
        return Ok(Some(WireFrame::Binary(body)));
    }
    read_line_bounded(r, max).map(|opt| opt.map(WireFrame::Text))
}

struct HostOutput {
    writer: BufWriter<TcpStream>,
    sess: sheetcore::wire::Session,
    binary_v3: bool,
    compact_v3: bool,
    scratch: Vec<u8>,
}

impl HostOutput {
    fn new(writer: BufWriter<TcpStream>, sess: sheetcore::wire::Session) -> Self {
        Self {
            writer,
            sess,
            binary_v3: false,
            compact_v3: false,
            scratch: Vec::new(),
        }
    }

    fn write_plain_opt(&mut self, plain: &str, flush: bool) -> std::io::Result<()> {
        if self.binary_v3 {
            self.sess.seal_binary_into(plain, true, &mut self.scratch);
            let res = write_binary_frame_opt(&mut self.writer, &self.scratch, flush);
            if self.scratch.capacity() > 512 * 1024 {
                self.scratch = Vec::new();
            }
            res
        } else {
            let frame = self.sess.seal(plain);
            write_line_opt(&mut self.writer, &frame, flush)
        }
    }

    fn write_plain(&mut self, plain: &str) -> std::io::Result<()> {
        self.write_plain_opt(plain, true)
    }

    fn write_compact_opt(&mut self, raw: &[u8], flush: bool) -> std::io::Result<()> {
        if self.binary_v3 && self.compact_v3 {
            self.sess
                .seal_compact_binary_into(raw, true, &mut self.scratch);
            let res = write_binary_frame_opt(&mut self.writer, &self.scratch, flush);
            if self.scratch.capacity() > 512 * 1024 {
                self.scratch = Vec::new();
            }
            res
        } else if let Some(msg) = sheetcore::sync::decode_msg_compact(raw)
            && let Ok(plain) = serde_json::to_string(&msg)
        {
            self.write_plain_opt(&plain, flush)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compact frame fallback failed",
            ))
        }
    }

    fn write_prepared_opt(
        &mut self,
        prepared: &sheetcore::wire::PreparedPayload,
        flush: bool,
    ) -> std::io::Result<()> {
        if self.binary_v3 && (!prepared.is_compact_bin() || self.compact_v3) {
            self.sess.seal_prepared_binary_into(prepared, &mut self.scratch);
            let res = write_binary_frame_opt(&mut self.writer, &self.scratch, flush);
            if self.scratch.capacity() > 512 * 1024 {
                self.scratch = Vec::new();
            }
            res
        } else if prepared.is_compact_bin() {
            let Some(raw) = prepared.decode_compact_bytes() else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid compact prepared payload",
                ));
            };
            let Some(msg) = sheetcore::sync::decode_msg_compact(&raw) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "compact payload transcoding failed",
                ));
            };
            let plain = serde_json::to_string(&msg)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            self.write_plain_opt(&plain, flush)
        } else {
            let frame = self.sess.seal_prepared(prepared);
            write_line_opt(&mut self.writer, &frame, flush)
        }
    }
}

struct GuestSend {
    sess: sheetcore::wire::Session,
    batch_ops: bool,
    sort_permutation: bool,
    binary_v3: bool,
    compact_v3: bool,
    scratch: Vec<u8>,
    acks: Arc<Mutex<Receiver<(u64, SortResponse)>>>,
}

impl GuestSend {
    fn write_plain_opt<W: Write>(&mut self, w: &mut W, plain: &str, flush: bool) -> std::io::Result<()> {
        if self.binary_v3 {
            self.sess.seal_binary_into(plain, true, &mut self.scratch);
            let res = write_binary_frame_opt(w, &self.scratch, flush);
            if self.scratch.capacity() > 512 * 1024 {
                self.scratch = Vec::new();
            }
            res
        } else {
            let frame = self.sess.seal(plain);
            write_line_opt(w, &frame, flush)
        }
    }

    fn write_plain<W: Write>(&mut self, w: &mut W, plain: &str) -> std::io::Result<()> {
        self.write_plain_opt(w, plain, true)
    }

    fn write_msg_opt<W: Write>(&mut self, w: &mut W, msg: &Msg, flush: bool) -> std::io::Result<()> {
        if self.binary_v3 && self.compact_v3 {
            let raw = sheetcore::sync::encode_msg_compact(msg);
            self.sess
                .seal_compact_binary_into(&raw, true, &mut self.scratch);
            let res = write_binary_frame_opt(w, &self.scratch, flush);
            if self.scratch.capacity() > 512 * 1024 {
                self.scratch = Vec::new();
            }
            res
        } else {
            let plain = serde_json::to_string(msg)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            self.write_plain_opt(w, &plain, flush)
        }
    }

    fn write_prepared_opt<W: Write>(
        &mut self,
        w: &mut W,
        prepared: &sheetcore::wire::PreparedPayload,
        flush: bool,
    ) -> std::io::Result<()> {
        if self.binary_v3 && (!prepared.is_compact_bin() || self.compact_v3) {
            self.sess.seal_prepared_binary_into(prepared, &mut self.scratch);
            let res = write_binary_frame_opt(w, &self.scratch, flush);
            if self.scratch.capacity() > 512 * 1024 {
                self.scratch = Vec::new();
            }
            res
        } else if prepared.is_compact_bin() {
            let Some(raw) = prepared.decode_compact_bytes() else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid compact prepared payload",
                ));
            };
            let Some(msg) = sheetcore::sync::decode_msg_compact(&raw) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "compact payload transcoding failed",
                ));
            };
            let plain = serde_json::to_string(&msg)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            self.write_plain_opt(w, &plain, flush)
        } else {
            let frame = self.sess.seal_prepared(prepared);
            write_line_opt(w, &frame, flush)
        }
    }
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
fn update_host_ops(
    state: &Mutex<Arc<Grid<Op>>>,
    delta: &Mutex<Arc<Grid<Op>>>,
    formula_keys: &Mutex<BTreeSet<(usize, u32, u32)>>,
    max_clock: &AtomicU64,
    ops: &[Op],
) {
    update_host_shared_ops(
        state,
        delta,
        formula_keys,
        max_clock,
        ops.iter().cloned().map(Arc::new),
    );
}

fn update_host_shared_ops(
    state: &Mutex<Arc<Grid<Op>>>,
    delta: &Mutex<Arc<Grid<Op>>>,
    formula_keys: &Mutex<BTreeSet<(usize, u32, u32)>>,
    max_clock: &AtomicU64,
    ops: impl IntoIterator<Item = Arc<Op>>,
) {
    let mut state = state.lock().unwrap();
    let mut delta = delta.lock().unwrap();
    let mut fk = formula_keys.lock().unwrap();
    let state = Arc::make_mut(&mut state);
    let delta = Arc::make_mut(&mut delta);
    let mut batch_max_ts = 0u64;
    for op in ops {
        batch_max_ts = batch_max_ts.max(op.ts);
        let key = (op.sheet, op.row, op.col);
        let prev_winner = state.get(&key);
        let wins = prev_winner.is_none_or(|v| (op.ts, op.site) > (v.ts, v.site));
        upsert(state, &op);
        upsert(delta, &op);
        if wins {
            if op.formula.is_some() {
                fk.insert(key);
            } else {
                fk.remove(&key);
            }
        }
    }
    if batch_max_ts > 0 {
        max_clock.fetch_max(batch_max_ts, Ordering::AcqRel);
    }
}

fn update_formula_keys_for_sort(
    formula_keys: &Mutex<BTreeSet<(usize, u32, u32)>>,
    ops: &[Arc<Op>],
) {
    let mut fk = formula_keys.lock().unwrap();
    for op in ops {
        let key = (op.sheet, op.row, op.col);
        if op.formula.is_some() {
            fk.insert(key);
        } else {
            fk.remove(&key);
        }
    }
}

fn update_host_styles(state: &SharedStyles, delta: &SharedStyles, sheet: usize,
                      items: &[(u32, u32, sheetcore::style::CellStyle)]) {
    let mut state = state.lock().unwrap();
    let mut delta = delta.lock().unwrap();
    let state = Arc::make_mut(&mut state);
    let delta = Arc::make_mut(&mut delta);
    let mut shared_by_hash: HashMap<u64, Vec<Arc<sheetcore::style::CellStyle>>> = HashMap::new();
    for (row, col, style) in items {
        let key = (sheet, *row, *col);
        let h = sheetcore::sync::cell_style_hash(style);
        let bucket = shared_by_hash.entry(h).or_default();
        let shared = if let Some(found) = bucket.iter().find(|s| s.as_ref() == style) {
            Arc::clone(found)
        } else {
            let arc = Arc::new(style.clone());
            bucket.push(Arc::clone(&arc));
            arc
        };
        state.insert_cow(key, Arc::clone(&shared));
        delta.insert_cow(key, shared);
    }
}

/// 按调色板索引直接写入宿主共享样式表（仅分配 `palette.len()` 个 `Arc<CellStyle>`，零逐格哈希与深拷贝）。
fn update_host_palette_styles(
    state: &SharedStyles,
    delta: &SharedStyles,
    sheet: usize,
    palette: &[sheetcore::style::CellStyle],
    items: &[(u32, u32, u16)],
) {
    let shared_palette: Vec<Arc<sheetcore::style::CellStyle>> =
        palette.iter().cloned().map(Arc::new).collect();
    let mut state = state.lock().unwrap();
    let mut delta = delta.lock().unwrap();
    let state = Arc::make_mut(&mut state);
    let delta = Arc::make_mut(&mut delta);
    for &(row, col, idx) in items {
        if let Some(shared) = shared_palette.get(idx as usize) {
            let key = (sheet, row, col);
            state.insert_cow(key, Arc::clone(shared));
            delta.insert_cow(key, Arc::clone(shared));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn shift_state(
    baseline: &SharedBaseline,
    formula_keys: &Mutex<BTreeSet<(usize, u32, u32)>>,
    state: &SharedGrid<Op>,
    delta_ops: &SharedGrid<Op>,
    base_hash: &Arc<Mutex<String>>,
    styles: &SharedStyles,
    delta_styles: &SharedStyles,
    locks: &Arc<Mutex<sheetcore::sync::LockTable>>,
    cursors: &Arc<Mutex<HashMap<u32, Cursor>>>,
    names: &[String],
    sheet: usize,
    op: &sheetcore::structural::Structural,
) {
    let map = |r: u32, c: u32| sheetcore::sync::shift_structural_coord(r, c, op);
    base_hash.lock().unwrap().clear();
    {
        let mut base_guard = baseline.lock().unwrap();
        if let Some(base_arc) = base_guard.as_mut() {
            Arc::make_mut(base_arc).shift_structural(sheet, op);
        }
    }
    let shifts = sheetcore::sync::structural_reference_shifts(names, sheet, op);
    let active_formulas: Vec<(usize, u32, u32)> = {
        let mut fk = formula_keys.lock().unwrap();
        sheetcore::sync::shift_coord_set(&mut fk, sheet, op);
        fk.iter().copied().collect()
    };
    for cells in [state, delta_ops] {
        shift_grid(cells, sheet, op, |value, row, col| {
            let cell = Arc::make_mut(value);
            cell.row = row;
            cell.col = col;
        });
        if !active_formulas.is_empty() {
            let mut grid = cells.lock().unwrap();
            let mut updates = Vec::new();
            for &key in &active_formulas {
                if let Some(cell) = grid.get(&key) {
                    if let Some(formula) = cell.shifted_formula(&shifts) {
                        let mut changed = (**cell).clone();
                        changed.formula = Some(Arc::from(formula));
                        updates.push((key, Arc::new(changed)));
                    }
                }
            }
            if !updates.is_empty() {
                let target = Arc::make_mut(&mut grid);
                for (key, changed) in updates {
                    target.insert_cow(key, changed);
                }
            }
        }
    }
    for styles in [styles, delta_styles] { shift_grid(styles, sheet, op, |_,_,_| {}); }
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
const SNAPSHOT_STYLE_CHUNK: usize = 1_024;
const SNAPSHOT_PALETTE_STYLE_CHUNK: usize = SNAPSHOT_STYLE_CHUNK * 2;
const SNAPSHOT_COMPACT_OP_CHUNK: usize = 8_192;

fn emit_style_snapshot_chunk(
    chunk: &[(usize, u32, u32, sheetcore::style::CellStyle)],
    palette_capable: bool,
    compact_v3: bool,
    tx: &std::sync::mpsc::SyncSender<IntroFrame>,
) -> bool {
    if chunk.is_empty() {
        return true;
    }
    if palette_capable {
        let (palette, items) = sheetcore::sync::encode_style_snapshot_palette(chunk);
        let msg = Msg::StylePaletteSnapshot { palette, items };
        if compact_v3 {
            return tx
                .send(IntroFrame::Compact(sheetcore::sync::encode_msg_compact(&msg)))
                .is_ok();
        }
        if let Ok(line) = serde_json::to_string(&msg) {
            if line.len() <= MAX_PLAIN_LINE {
                return tx.send(IntroFrame::Line(line)).is_ok();
            }
        }
    }
    style_frames(
        chunk,
        MAX_PLAIN_LINE,
        |batch| serde_json::to_string(&Msg::StyleSnapshot { items: batch.to_vec() }),
        |line| tx.send(IntroFrame::Line(line)).is_ok(),
    )
}

fn emit_style_palette_snapshot_chunk(
    palette: &mut Vec<sheetcore::style::CellStyle>,
    items: &mut Vec<(usize, u32, u32, u16)>,
    compact_v3: bool,
    tx: &std::sync::mpsc::SyncSender<IntroFrame>,
) -> bool {
    if items.is_empty() {
        palette.clear();
        return true;
    }
    let pal = std::mem::take(palette);
    let it = std::mem::take(items);
    if compact_v3 {
        let msg = Msg::StylePaletteSnapshot {
            palette: pal,
            items: it,
        };
        return tx
            .send(IntroFrame::Compact(sheetcore::sync::encode_msg_compact(&msg)))
            .is_ok();
    }
    if let Ok(line) = serde_json::to_string(&Msg::StylePaletteSnapshot {
        palette: pal.clone(),
        items: it.clone(),
    }) {
        if line.len() <= MAX_PLAIN_LINE {
            return tx.send(IntroFrame::Line(line)).is_ok();
        }
    }
    // 极端超长样式帧回退至逐条拆帧
    let expanded: Vec<(usize, u32, u32, sheetcore::style::CellStyle)> = it
        .into_iter()
        .filter_map(|(s, r, c, idx)| pal.get(idx as usize).map(|st| (s, r, c, st.clone())))
        .collect();
    style_frames(
        &expanded,
        MAX_PLAIN_LINE,
        |batch| serde_json::to_string(&Msg::StyleSnapshot { items: batch.to_vec() }),
        |line| tx.send(IntroFrame::Line(line)).is_ok(),
    )
}

/// 面向稀疏覆盖样式 `Arc<CellStyle>` 的流式调色板分片发送器：
/// 先按 `Arc` 指针身份 `O(1)` 命中，未命中再按内容哈希去重，避免逐格克隆 `CellStyle` 与重复哈希。
struct OverlayStylePaletteEmitter {
    palette: Vec<sheetcore::style::CellStyle>,
    ptr_to_pal: HashMap<usize, u16>,
    hash_buckets: HashMap<u64, Vec<u16>>,
    items: Vec<(usize, u32, u32, u16)>,
    chunk_limit: usize,
    compact_v3: bool,
}

impl OverlayStylePaletteEmitter {
    fn new(compact_v3: bool) -> Self {
        let chunk_limit = SNAPSHOT_PALETTE_STYLE_CHUNK;
        Self {
            palette: Vec::new(),
            ptr_to_pal: HashMap::new(),
            hash_buckets: HashMap::new(),
            items: Vec::with_capacity(chunk_limit.min(4096)),
            chunk_limit,
            compact_v3,
        }
    }

    fn push(
        &mut self,
        si: usize,
        row: u32,
        col: u32,
        style: &Arc<sheetcore::style::CellStyle>,
        tx: &std::sync::mpsc::SyncSender<IntroFrame>,
    ) -> bool {
        let ptr = Arc::as_ptr(style) as usize;
        let idx = if let Some(&id) = self.ptr_to_pal.get(&ptr) {
            id
        } else {
            let h = sheetcore::sync::cell_style_hash(style);
            let mut found = None;
            if let Some(cands) = self.hash_buckets.get(&h) {
                for &id in cands {
                    if &self.palette[id as usize] == style.as_ref() {
                        found = Some(id);
                        break;
                    }
                }
            }
            let id = match found {
                Some(id) => id,
                None => {
                    let id = u16::try_from(self.palette.len()).unwrap_or(u16::MAX);
                    if (id as usize) == self.palette.len() {
                        self.palette.push(style.as_ref().clone());
                        self.hash_buckets.entry(h).or_default().push(id);
                    }
                    id
                }
            };
            self.ptr_to_pal.insert(ptr, id);
            id
        };
        self.items.push((si, row, col, idx));
        if self.items.len() >= self.chunk_limit {
            return self.flush(tx);
        }
        true
    }

    fn flush(&mut self, tx: &std::sync::mpsc::SyncSender<IntroFrame>) -> bool {
        self.ptr_to_pal.clear();
        self.hash_buckets.clear();
        emit_style_palette_snapshot_chunk(&mut self.palette, &mut self.items, self.compact_v3, tx)
    }
}

/// 流式下发样式快照（合并只读 `BaselineBook` 与稀疏覆盖 `Grid<CellStyle>`，零百万级中间 Vec）：
/// 现代客户端按 `StyleId` / `Arc` 指针直接构造调色板分片，消除逐格克隆 `CellStyle` 与重复哈希。
fn stream_snapshot_styles(
    baseline: Option<&BaselineBook>,
    overlay: &Grid<sheetcore::style::CellStyle>,
    palette_capable: bool,
    compact_v3: bool,
    tx: &std::sync::mpsc::SyncSender<IntroFrame>,
) -> bool {
    let overlay_nonempty = overlay.range(..).next().is_some();
    if palette_capable {
        let chunk_limit = SNAPSHOT_PALETTE_STYLE_CHUNK;
        if let Some(base) = baseline {
            let mut palette: Vec<sheetcore::style::CellStyle> = Vec::new();
            let mut id_to_pal: HashMap<u16, u16> = HashMap::new();
            let mut items: Vec<(usize, u32, u32, u16)> =
                Vec::with_capacity(chunk_limit.min(4096));
            for (si, sh) in base.sheets.iter().enumerate() {
                let sheet_has_overlay = overlay_nonempty
                    && overlay
                        .range((si, 0, 0)..=(si, u32::MAX, u32::MAX))
                        .next()
                        .is_some();
                // Preserve explicit `sid == 0` entries as well: `sh.styles` only stores `0` when
                // masking a row/column default style (e.g. an unbolded cell inside a bold column).
                for (pos, &id) in sh.styles.iter() {
                    if sheet_has_overlay && overlay.get(&(si, pos.row, pos.col)).is_some() {
                        continue;
                    }
                    let pal_idx = *id_to_pal.entry(id).or_insert_with(|| {
                        let idx = palette.len() as u16;
                        palette.push(base.styles.get(id).clone());
                        idx
                    });
                    items.push((si, pos.row, pos.col, pal_idx));
                    if items.len() >= chunk_limit {
                        id_to_pal.clear();
                        if !emit_style_palette_snapshot_chunk(&mut palette, &mut items, compact_v3, tx) {
                            return false;
                        }
                    }
                }
            }
            if !emit_style_palette_snapshot_chunk(&mut palette, &mut items, compact_v3, tx) {
                return false;
            }
        }
        if overlay_nonempty {
            let mut emitter = OverlayStylePaletteEmitter::new(compact_v3);
            for (&(si, r, c), style) in overlay.range(..) {
                if !emitter.push(si, r, c, style, tx) {
                    return false;
                }
            }
            return emitter.flush(tx);
        }
        return true;
    }

    let mut buf: Vec<(usize, u32, u32, sheetcore::style::CellStyle)> =
        Vec::with_capacity(SNAPSHOT_STYLE_CHUNK);
    if let Some(base) = baseline {
        for (si, sh) in base.sheets.iter().enumerate() {
            let sheet_has_overlay = overlay_nonempty
                && overlay
                    .range((si, 0, 0)..=(si, u32::MAX, u32::MAX))
                    .next()
                    .is_some();
            for (pos, &id) in sh.styles.iter() {
                if sheet_has_overlay && overlay.get(&(si, pos.row, pos.col)).is_some() {
                    continue;
                }
                buf.push((si, pos.row, pos.col, base.styles.get(id).clone()));
                if buf.len() >= SNAPSHOT_STYLE_CHUNK {
                    if !emit_style_snapshot_chunk(&buf, false, false, tx) {
                        return false;
                    }
                    buf.clear();
                }
            }
        }
    }
    if overlay_nonempty {
        for (&(si, r, c), style) in overlay.range(..) {
            buf.push((si, r, c, style.as_ref().clone()));
            if buf.len() >= SNAPSHOT_STYLE_CHUNK {
                if !emit_style_snapshot_chunk(&buf, false, false, tx) {
                    return false;
                }
                buf.clear();
            }
        }
    }
    emit_style_snapshot_chunk(&buf, false, false, tx)
}

fn send_snapshot_chunk_refs(
    refs: &[&Op],
    compact_v3: bool,
    tx: &std::sync::mpsc::SyncSender<IntroFrame>,
) -> bool {
    if refs.is_empty() {
        return true;
    }
    if compact_v3 {
        let raw = sheetcore::sync::encode_snapshot_chunk_ref_compact(refs, false);
        tx.send(IntroFrame::Compact(raw)).is_ok()
    } else {
        let frame = sheetcore::sync::SnapshotChunkRef {
            t: "SnapshotChunk",
            ops: refs,
            last: false,
        };
        serde_json::to_string(&frame).is_ok_and(|line| tx.send(IntroFrame::Line(line)).is_ok())
    }
}

/// 流式下发单元格快照（直接借用 `&Op` 序列化，空覆盖表完全跳过 B-Tree 探查，紧凑模式下零 JSON 分配）
fn stream_snapshot_ops(
    baseline: Option<&BaselineBook>,
    overlay: &Grid<Op>,
    compact_v3: bool,
    tx: &std::sync::mpsc::SyncSender<IntroFrame>,
) -> bool {
    let chunk_cap = if compact_v3 {
        SNAPSHOT_COMPACT_OP_CHUNK
    } else {
        sheetcore::sync::SNAPSHOT_CHUNK_OPS
    };
    let byte_cap = if compact_v3 { 1024 * 1024 } else { 512 * 1024 };
    let overlay_nonempty = overlay.range(..).next().is_some();

    let flush_owned = |chunk: &[Op]| -> bool {
        if chunk.is_empty() {
            return true;
        }
        let refs: Vec<&Op> = chunk.iter().collect();
        send_snapshot_chunk_refs(&refs, compact_v3, tx)
    };

    if let Some(base) = baseline {
        let mut chunk: Vec<Op> = Vec::with_capacity(chunk_cap.min(4096));
        let mut bytes = 0usize;
        for (si, sh) in base.sheets.iter().enumerate() {
            let sheet_has_overlay = overlay_nonempty
                && overlay
                    .range((si, 0, 0)..=(si, u32::MAX, u32::MAX))
                    .next()
                    .is_some();
            for (pos, cell) in sh.iter() {
                if sheet_has_overlay && overlay.get(&(si, pos.row, pos.col)).is_some() {
                    continue;
                }
                let op = Op {
                    sheet: si,
                    row: pos.row,
                    col: pos.col,
                    value: (&cell.value).into(),
                    formula: cell.formula.clone(),
                    ts: 1,
                    site: base.site,
                };
                let text_len = match &op.value {
                    sheetcore::sync::ValueWire::Text(s) | sheetcore::sync::ValueWire::Error(s) => {
                        s.len()
                    }
                    _ => 0,
                };
                let size = text_len + op.formula.as_deref().map_or(0, str::len) + 64;
                if !chunk.is_empty() && (chunk.len() >= chunk_cap || bytes + size > byte_cap) {
                    if !flush_owned(&chunk) {
                        return false;
                    }
                    chunk.clear();
                    bytes = 0;
                }
                bytes += size;
                chunk.push(op);
            }
        }
        if !flush_owned(&chunk) {
            return false;
        }
    }

    if overlay_nonempty {
        let mut refs: Vec<&Op> = Vec::with_capacity(chunk_cap.min(4096));
        let mut bytes = 0usize;
        for (_, op_arc) in overlay.range(..) {
            let op = op_arc.as_ref();
            let text_len = match &op.value {
                sheetcore::sync::ValueWire::Text(s) | sheetcore::sync::ValueWire::Error(s) => s.len(),
                _ => 0,
            };
            let size = text_len + op.formula.as_deref().map_or(0, str::len) + 64;
            if !refs.is_empty() && (refs.len() >= chunk_cap || bytes + size > byte_cap) {
                if !send_snapshot_chunk_refs(&refs, compact_v3, tx) {
                    return false;
                }
                refs.clear();
                bytes = 0;
            }
            bytes += size;
            refs.push(op);
        }
        if !refs.is_empty() && !send_snapshot_chunk_refs(&refs, compact_v3, tx) {
            return false;
        }
    }
    true
}

fn style_frames<T>(items: &[T], max_bytes: usize, encode: impl Fn(&[T]) -> serde_json::Result<String>,
                   mut send: impl FnMut(String) -> bool) -> bool {
    indexed_frames(items, max_bytes, |_,part| encode(part), send)
}

fn indexed_frames<T>(items: &[T], max_bytes: usize, encode: impl Fn(usize,&[T]) -> serde_json::Result<String>,
                     mut send: impl FnMut(String) -> bool) -> bool {
    let mut from = 0;
    while from < items.len() {
        let mut count = (items.len() - from).min(256);
        loop {
            let Ok(line) = encode(from,&items[from..from + count]) else { return false };
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
        Msg::StylePaletteOp { items, .. } => items.len(),
        Msg::SortData { items, .. } => items.len(),
        Msg::SortRepairRequest { keys, .. } => keys.len(),
        Msg::SortRepairs { items, .. } => items.len(),
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
        Msg::SortData { first_ts,items } => style_frames(&items,limit,|part|
            serde_json::to_string(&Msg::SortData { first_ts, items: part.to_vec() }),send),
        Msg::SortRepairRequest { sheet, first_ts, structural_revision, keys } => indexed_frames(&keys,limit,|i,keys|
            serde_json::to_string(&Msg::SortRepairRequest { sheet, first_ts: first_ts + i as u64, structural_revision, keys: keys.to_vec() }),send),
        Msg::SortRepairs { sheet, first_ts, items } => indexed_frames(&items,limit,|_,items|
            serde_json::to_string(&Msg::SortRepairs { sheet, first_ts, items: items.to_vec() }),send),
        Msg::StyleOp { site, sheet, ts, items } => style_frames(&items, limit, |batch| {
            serde_json::to_string(&Msg::StyleOp { site, sheet, ts, items: batch.to_vec() })
        }, send),
        Msg::StylePaletteOp { site, sheet, ts, palette, items } => {
            for batch in items.chunks(sheetcore::sync::STYLE_PALETTE_CHUNK_ITEMS) {
                let Ok(line) = serde_json::to_string(&Msg::StylePaletteOp {
                    site,
                    sheet,
                    ts,
                    palette: palette.clone(),
                    items: batch.to_vec(),
                }) else {
                    return false;
                };
                if !send(line) {
                    return false;
                }
            }
            true
        }
        Msg::WriteDeny { holder, holder_name, sheet, row, col, revert_styles, .. } =>
            style_frames(&revert_styles, limit, |batch| serde_json::to_string(&Msg::WriteDeny {
                holder, holder_name: holder_name.clone(), sheet, row, col,
                revert_ops: Vec::new(), revert_styles: batch.to_vec(), revert_merge: None,
            }), send),
        _ => unreachable!(),
    }
}

fn latest_cell_op(
    baseline: Option<&SharedBaseline>,
    state: &Mutex<Arc<Grid<Op>>>,
    sheet: usize,
    row: u32,
    col: u32,
) -> Option<Op> {
    if let Some(op) = state.lock().unwrap().get(&(sheet, row, col)) {
        return Some(op.as_ref().clone());
    }
    baseline
        .and_then(|b| b.lock().unwrap().clone())
        .and_then(|b| b.cell_op(sheet, row, col))
}

fn latest_cell_style(
    baseline: Option<&SharedBaseline>,
    axis_styles: Option<&AxisStylesMap>,
    styles: &SharedStyles,
    sheet: usize,
    row: u32,
    col: u32,
) -> Option<sheetcore::style::CellStyle> {
    if let Some(st) = styles.lock().unwrap().get(&(sheet, row, col)) {
        return Some(st.as_ref().clone());
    }
    if let Some(st) = baseline
        .and_then(|b| b.lock().unwrap().clone())
        .and_then(|b| b.cell_style(sheet, row, col))
    {
        return Some(st);
    }
    axis_styles
        .and_then(|m| m.get(&sheet))
        .and_then(|ax| ax.get(row, col))
        .map(|s| (**s).clone())
}

fn emit_room_watermark(
    cmd: &Sender<Cmd>,
    roster: &Mutex<RoomRoster>,
    seq: u64,
    structure_revision: usize,
) {
    queue_room_change(
        cmd,
        roster,
        None,
        RoomChange::Message(Msg::Watermark {
            seq,
            structure_revision,
        }),
    );
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

    /// 宿主启动时以 O(页数) CoW 快照建立只读基准（零百万格 B-Tree 建树、零样式全表克隆）
    pub fn seed_snapshot(
        &self,
        peer: &sheetcore::sync::Peer,
        workbook: &sheetcore::Workbook,
        mut host_cursor: Cursor,
    ) {
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        *self.sheet_names.lock().unwrap() = workbook.sheets.iter().map(|s| s.name.clone()).collect();
        if let Some(hb) = &self.host_baseline {
            let base = peer
                .cells
                .baseline()
                .cloned()
                .unwrap_or_else(|| BaselineBook::from_workbook(self.site, workbook));
            *hb.lock().unwrap() = Some(Arc::new(base));
        }
        let mut seed_max_ts = 1u64;
        if let Some(st) = &self.state {
            let mut map = st.lock().unwrap();
            *map = Arc::new(
                peer.cells
                    .iter()
                    .map(|(k, v)| {
                        seed_max_ts = seed_max_ts.max(v.ts);
                        (*k, v.clone())
                    })
                    .collect(),
            );
        }
        if let Some(mc) = &self.host_max_clock {
            mc.store(seed_max_ts.max(1), Ordering::Release);
        }
        if let Some(fk) = &self.formula_keys {
            let mut set = fk.lock().unwrap();
            set.clear();
            for (&k, op) in peer.cells.iter() {
                if op.formula.is_some() {
                    set.insert(k);
                }
            }
        }
        if let Some(d) = &self.delta_ops {
            *d.lock().unwrap() = Default::default();
        }
        if let Some(ds) = &self.delta_styles {
            *ds.lock().unwrap() = Default::default();
        }
        if let Some(dm) = &self.delta_merges {
            dm.lock().unwrap().clear();
        }
        if let Some(hs) = &self.host_styles {
            // 基准显式样式保留在只读 CoW `BaselineBook` 中，`host_styles` 仅存储建房后的增量改动
            *hs.lock().unwrap() = Default::default();
        }
        if let Some(hp) = &self.host_file_path {
            *hp.lock().unwrap() = workbook.path.clone();
        }
        *self.staged_save_cursor.lock().unwrap() = None;
        if let Some(wm) = &self.host_watermark {
            let mut w_guard = wm.lock().unwrap();
            let w = &mut *w_guard;
            w.reset();
            if let Some(bh) = &self.host_base_hash {
                let initial_hash = bh.lock().unwrap().clone();
                if !initial_hash.is_empty() {
                    w.record_checkpoint(initial_hash, 1, 0);
                }
            }
            let axis_rows = Arc::make_mut(&mut w.axis_rows);
            let axis_cols = Arc::make_mut(&mut w.axis_cols);
            for (si, sh) in workbook.sheets.iter().enumerate() {
                if !sh.row_styles.is_empty() {
                    let rm = axis_rows.entry(si).or_default();
                    for (&r, &id) in &sh.row_styles {
                        if id != 0 {
                            rm.insert(r, (Some(workbook.styles.get(id).clone()), 1));
                        }
                    }
                }
                if !sh.col_styles.is_empty() {
                    let cm = axis_cols.entry(si).or_default();
                    for (&c, &id) in &sh.col_styles {
                        if id != 0 {
                            cm.insert(c, (Some(workbook.styles.get(id).clone()), 1));
                        }
                    }
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
        if let Some(wm) = &self.host_watermark
            && !hash.is_empty()
        {
            let srev = self
                .host_structs
                .as_ref()
                .map_or(0, |h| h.lock().unwrap().len());
            let mut w = wm.lock().unwrap();
            let seq = w.seq;
            w.record_checkpoint(hash.clone(), seq, srev);
        }
        if let Some(bh) = &self.host_base_hash {
            *bh.lock().unwrap() = hash;
        }
    }

    /// 绑定宿主当前托管的原文件路径，供保存完成后自动异步刷新增量入房检查点。
    #[allow(dead_code)]
    pub fn set_host_file_path(&self, path: Option<std::path::PathBuf>) {
        if let Some(hp) = &self.host_file_path {
            *hp.lock().unwrap() = path;
        }
    }

    /// 显式记录一次宿主原文件保存对应的 `(file_sha256, watermark_seq, structure_revision)` 检查点。
    #[allow(dead_code)]
    pub fn record_save_checkpoint(&self, file_hash: String) {
        if !self.hosting || file_hash.is_empty() {
            return;
        }
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        let srev = self
            .host_structs
            .as_ref()
            .map_or(0, |h| h.lock().unwrap().len());
        if let Some(wm) = &self.host_watermark {
            let mut w = wm.lock().unwrap();
            let seq = w.seq;
            w.record_checkpoint(file_hash, seq, srev);
        }
    }

    /// 启用加入者后台列式工作簿快照直接移交（配合 `take_prepared_snapshot` 使用）。
    #[allow(dead_code)]
    pub fn enable_fast_snapshot_handoff(&self) {
        self.fast_snapshot_handoff.store(true, Ordering::Release);
    }

    /// 以加入者现有工作簿样式表（含主题色 `themes`、图案填充 `patterns`、`quotePrefix`）为种子，
    /// 启用后台列式快照直接移交，使新样式追加在原样式表之后、安装时 `O(1)` 替换且不丢失任何 OOXML 样式扩展。
    #[allow(dead_code)]
    pub fn enable_fast_snapshot_handoff_with_styles(
        &self,
        base_styles: &sheetcore::style::StyleTable,
    ) {
        *self.seed_styles.lock().unwrap() = Some(base_styles.clone());
        self.fast_snapshot_handoff.store(true, Ordering::Release);
    }

    /// 在收到 `NetEvent::SnapshotDone` 时取走后台已组装完毕的列式工作簿快照。
    #[allow(dead_code)]
    pub fn take_prepared_snapshot(&self) -> Option<PreparedGuestSnapshot> {
        self.ready_snapshot.lock().unwrap().take()
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
        if keys.is_empty() {
            return None;
        }
        let locks = self.host_locks.as_ref()?;
        self.expire_host_locks();
        let now = self.host_now_ms();
        let lt = locks.lock().unwrap();
        if lt.is_empty() {
            return None;
        }
        // 大范围选区而活跃锁极少时，先快速确认该表是否存在他人有效租约
        if keys.len() > lt.len().saturating_mul(4)
            && !lt.iter().any(|((sh, _, _), h)| {
                sh == sheet && h.site != self.site && now.saturating_sub(h.at_ms) < lt.ttl_ms
            })
        {
            return None;
        }
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
        if ops.is_empty() {
            return;
        }
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        let now = self.host_now_ms();
        let has_expired = self.host_locks.as_ref().is_some_and(|table| {
            let lt = table.lock().unwrap();
            !lt.is_empty() && lt.iter().any(|(_, h)| now.saturating_sub(h.at_ms) >= lt.ttl_ms)
        });
        if has_expired {
            self.expire_host_locks();
        }
        let locks = self.host_locks.as_ref().map(|table| table.lock().unwrap());
        let has_other_locks = locks.as_ref().is_some_and(|lt| {
            !lt.is_empty()
                && lt
                    .iter()
                    .any(|(_, h)| h.site != self.site && now.saturating_sub(h.at_ms) < lt.ttl_ms)
        });
        if has_other_locks {
            ops.retain_mut(|op| {
                op.site = self.site;
                if let Some(holder) = locks
                    .as_ref()
                    .and_then(|lt| lt.holder_other(op.sheet, op.row, op.col, self.site, now))
                {
                    let prev = self.state.as_ref().and_then(|st| {
                        latest_cell_op(self.host_baseline.as_ref(), st, op.sheet, op.row, op.col)
                    });
                    let _ = self.evt_tx.send(NetEvent::WriteRejected {
                        holder: holder.site,
                        holder_name: holder.name.clone(),
                        sheet: op.sheet,
                        row: op.row,
                        col: op.col,
                        revert_ops: vec![(op.row, op.col, prev)],
                        revert_styles: Vec::new(),
                        revert_merge: None,
                    });
                    return false;
                }
                true
            });
        } else {
            for op in &mut ops {
                op.site = self.site;
            }
        }
        drop(locks);
        if ops.is_empty() { return; }
        let shared: Vec<Arc<Op>> = ops.into_iter().map(Arc::new).collect();
        self.commit_and_dispatch_shared_ops(shared);
    }

    /// 零克隆批量广播已由 `Peer::local_set_shared` 构造的 `Arc<Op>` 切片。
    #[allow(dead_code)]
    pub fn broadcast_shared_ops(&self, mut ops: Vec<Arc<Op>>) {
        if ops.is_empty() {
            return;
        }
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        let now = self.host_now_ms();
        let has_expired = self.host_locks.as_ref().is_some_and(|table| {
            let lt = table.lock().unwrap();
            !lt.is_empty() && lt.iter().any(|(_, h)| now.saturating_sub(h.at_ms) >= lt.ttl_ms)
        });
        if has_expired {
            self.expire_host_locks();
        }
        let locks = self.host_locks.as_ref().map(|table| table.lock().unwrap());
        let has_other_locks = locks.as_ref().is_some_and(|lt| {
            !lt.is_empty()
                && lt
                    .iter()
                    .any(|(_, h)| h.site != self.site && now.saturating_sub(h.at_ms) < lt.ttl_ms)
        });
        if has_other_locks {
            ops.retain_mut(|op_arc| {
                if op_arc.site != self.site {
                    Arc::make_mut(op_arc).site = self.site;
                }
                if let Some(holder) = locks.as_ref().and_then(|lt| {
                    lt.holder_other(op_arc.sheet, op_arc.row, op_arc.col, self.site, now)
                }) {
                    let prev = self.state.as_ref().and_then(|st| {
                        latest_cell_op(
                            self.host_baseline.as_ref(),
                            st,
                            op_arc.sheet,
                            op_arc.row,
                            op_arc.col,
                        )
                    });
                    let _ = self.evt_tx.send(NetEvent::WriteRejected {
                        holder: holder.site,
                        holder_name: holder.name.clone(),
                        sheet: op_arc.sheet,
                        row: op_arc.row,
                        col: op_arc.col,
                        revert_ops: vec![(op_arc.row, op_arc.col, prev)],
                        revert_styles: Vec::new(),
                        revert_merge: None,
                    });
                    return false;
                }
                true
            });
        } else {
            for op_arc in &mut ops {
                if op_arc.site != self.site {
                    Arc::make_mut(op_arc).site = self.site;
                }
            }
        }
        drop(locks);
        if ops.is_empty() {
            return;
        }
        self.commit_and_dispatch_shared_ops(ops);
    }

    fn commit_and_dispatch_shared_ops(&self, ops: Vec<Arc<Op>>) {
        if let (Some(state), Some(delta), Some(fk), Some(mc)) = (
            &self.state,
            &self.delta_ops,
            &self.formula_keys,
            &self.host_max_clock,
        ) {
            update_host_shared_ops(state, delta, fk, mc, ops.iter().cloned());
        }
        if let Some(roster) = &self.host_roster {
            let wm_seq = self
                .host_watermark
                .as_ref()
                .map(|wm| wm.lock().unwrap().record_ops(ops.iter().map(Arc::as_ref)));
            let struct_rev = self
                .host_structs
                .as_ref()
                .map_or(0, |h| h.lock().unwrap().len());
            queue_room_change(&self.cmd, roster, None, RoomChange::SharedCells(ops));
            if let Some(seq) = wm_seq {
                emit_room_watermark(&self.cmd, roster, seq, struct_rev);
            }
        } else {
            let _ = self.cmd.send(Cmd::SharedCells(ops));
        }
    }

    pub fn pending_structure(&self) -> bool { self.pending_structure.load(Ordering::Acquire) }
    pub fn finish_structure(&self) { self.pending_structure.store(false, Ordering::Release); }

    pub fn structure_seen(&self) -> usize { self.structure_seen.lock().unwrap().seen }
    pub fn note_structure(&self, count: usize) { self.structure_seen.lock().unwrap().consume(count); }
    pub fn set_structure_revision(&self, revision: usize) {
        *self.structure_seen.lock().unwrap() = StructureProgress { seen:revision,..Default::default() };
    }
    pub fn reset_structure(&self) { self.set_structure_revision(0); }

    pub fn sort_clock_ranges(&self) -> Vec<(u64,u64)> { self.local_sort_ranges.lock().unwrap().clone() }

    #[cfg(test)]
    pub fn pending_sort_ranges(&self) -> Vec<(u64,u64)> {
        self.pending_sorts.lock().unwrap().iter().map(|(&first,b)| (first,first + b.ops.len() as u64)).collect()
    }

    pub fn finish_sort(&self, first_ts: u64) { self.pending_sorts.lock().unwrap().remove(&first_ts); }

    pub fn discard_pending_sorts(&self) -> Vec<u64> {
        std::mem::take(&mut *self.pending_sorts.lock().unwrap()).into_keys().collect()
    }

    pub(crate) fn check_sort(&self, target: crate::net_sort::SortTarget, plan: sheetcore::sync::SortPermutation, sheet: sheetcore::Sheet,
                              styles: sheetcore::style::StyleTable, reply: SortVote) {
        let events = self.evt_tx.clone();
        std::thread::spawn(move || {
            let batch = crate::net_sort::from_sheet(&sheet,&styles,&plan,|| reply.pending()).map(Arc::new);
            let _ = events.send(NetEvent::SortReady(target,batch,reply));
        });
    }

    pub(crate) fn broadcast_sort(&self, batch: SortBatch) {
        let batch = Arc::new(batch);
        if let (
            Some(order),
            Some(roster),
            Some(baseline),
            Some(fk),
            Some(wm),
            Some(max_clock),
            Some(state),
            Some(delta),
            Some(styles),
            Some(delta_styles),
        ) = (
            &self.room_order,
            &self.host_roster,
            &self.host_baseline,
            &self.formula_keys,
            &self.host_watermark,
            &self.host_max_clock,
            &self.state,
            &self.delta_ops,
            &self.host_styles,
            &self.delta_styles,
        ) {
            let commit_host_sort = |batch: Arc<SortBatch>,
                                    order: &Arc<Mutex<()>>,
                                    roster: &Arc<Mutex<RoomRoster>>,
                                    baseline: &SharedBaseline,
                                    fk: &Arc<Mutex<BTreeSet<(usize, u32, u32)>>>,
                                    wm: &Arc<Mutex<HostWatermark>>,
                                    max_clock: &Arc<AtomicU64>,
                                    state: &SharedGrid<Op>,
                                    delta: &SharedGrid<Op>,
                                    styles: &SharedStyles,
                                    delta_styles: &SharedStyles,
                                    host_locks: &Arc<Mutex<sheetcore::sync::LockTable>>,
                                    host_structs: &Arc<Mutex<Vec<(usize, sheetcore::structural::Structural)>>>,
                                    cmd: &Sender<Cmd>,
                                    evt_tx: &EvtTx,
                                    site: u32,
                                    t0: std::time::Instant| {
                for attempt in 0..3 {
                    let axis_snap = wm.lock().unwrap().axis_styles_snapshot();
                    let current = RoomGrids::capture(baseline, axis_snap, state, delta, styles, delta_styles);
                    let next = current.apply(&batch);
                    let _order = order.lock().unwrap();
                    let leases = host_locks.lock().unwrap();
                    let (r1, c1, r2, c2) = batch.plan.region;
                    let history = host_structs.lock().unwrap();
                    let later = history.get(batch.plan.structural_revision..);
                    let now = t0.elapsed().as_millis() as u64;
                    let allowed = later
                        .is_some_and(|changes| changes.iter().all(|(s, _)| *s != batch.plan.sheet))
                        && leases
                            .conflicts_rect(batch.plan.sheet, r1, c1, r2, c2, site, now)
                            .is_none();
                    if allowed && current.publish(&next, state, delta, styles, delta_styles) {
                        if let Some(m) = batch.ops.iter().map(|o| o.ts).max() {
                            max_clock.fetch_max(m, Ordering::AcqRel);
                        }
                        update_formula_keys_for_sort(fk, &batch.ops);
                        let seq = wm.lock().unwrap().record_sort(&batch);
                        let struct_rev = history.len();
                        if !batch.styles.is_empty() {
                            let _ = evt_tx.send(NetEvent::CommittedSortStyles(batch.clone()));
                        }
                        queue_room_change(cmd, roster, None, RoomChange::Sort(batch));
                        emit_room_watermark(cmd, roster, seq, struct_rev);
                        return;
                    }
                    if !allowed || attempt == 2 {
                        let axis_now = wm.lock().unwrap().axis_styles_snapshot();
                        let correction = SortCorrection::capture(
                            batch,
                            baseline,
                            axis_now,
                            state,
                            styles,
                            later.unwrap_or_default(),
                        );
                        let _ = evt_tx.send(NetEvent::LocalSortRepair(correction));
                        return;
                    }
                }
            };
            let host_locks = self.host_locks.as_ref().expect("host lease table");
            let host_structs = self
                .host_structs
                .as_ref()
                .expect("host structural history");
            if batch.ops.len() + batch.styles.len() > 4_096 {
                let order = order.clone();
                let roster = roster.clone();
                let baseline = baseline.clone();
                let fk = fk.clone();
                let wm = wm.clone();
                let max_clock = max_clock.clone();
                let state = state.clone();
                let delta = delta.clone();
                let styles = styles.clone();
                let delta_styles = delta_styles.clone();
                let host_locks = host_locks.clone();
                let host_structs = host_structs.clone();
                let cmd = self.cmd.clone();
                let evt_tx = self.evt_tx.clone();
                let site = self.site;
                let t0 = self.t0;
                std::thread::spawn(move || {
                    commit_host_sort(
                        batch,
                        &order,
                        &roster,
                        &baseline,
                        &fk,
                        &wm,
                        &max_clock,
                        &state,
                        &delta,
                        &styles,
                        &delta_styles,
                        &host_locks,
                        &host_structs,
                        &cmd,
                        &evt_tx,
                        site,
                        t0,
                    );
                });
            } else {
                commit_host_sort(
                    batch,
                    order,
                    roster,
                    baseline,
                    fk,
                    wm,
                    max_clock,
                    state,
                    delta,
                    styles,
                    delta_styles,
                    host_locks,
                    host_structs,
                    &self.cmd,
                    &self.evt_tx,
                    self.site,
                    self.t0,
                );
            }
        } else {
            let first = batch.plan.first_ts;
            let end = first + batch.ops.len() as u64;
            let mut ranges = self.local_sort_ranges.lock().unwrap();
            if let Some(last) = ranges.last_mut() && last.1 == first { last.1 = end; }
            else { ranges.push((first,end)); }
            drop(ranges);
            self.pending_sorts.lock().unwrap().insert(first,batch.clone());
            let generation = self.connection_generation.load(Ordering::Acquire);
            let _ = self.cmd.send(Cmd::Sort(generation,batch));
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

    /// 获取宿主当前权威房间水位 `(wm_seq, struct_rev)`（无未保存改动时直接回执用）。
    pub fn current_watermark_cursor(&self) -> Option<(u64, usize)> {
        if !self.hosting {
            return None;
        }
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        let wm_seq = self.host_watermark.as_ref()?.lock().unwrap().seq;
        let srev = self
            .host_structs
            .as_ref()
            .map_or(0, |h| h.lock().unwrap().len());
        Some((wm_seq, srev))
    }

    fn note_local_watermark_edit(&self) {
        if self.hosting {
            return;
        }
        let cur_wm = self.last_watermark_seq.load(Ordering::Acquire);
        let _ = self.min_saved_watermark.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |prev| Some(prev.max(cur_wm).saturating_add(1)),
        );
    }

    /// 检查宿主发来的保存回执是否覆盖了本访客自上次保存以来发出的所有非单元格水位变更（列宽/行高/样式/合并等）。
    pub fn save_watermark_satisfied(&self, saved_wm: Option<u64>) -> bool {
        let min_needed = self.min_saved_watermark.load(Ordering::Acquire);
        if min_needed == 0 {
            return true;
        }
        let cur_wm = self.last_watermark_seq.load(Ordering::Acquire);
        saved_wm.is_some_and(|wm| wm >= min_needed && wm >= cur_wm)
    }

    /// 当宿主保存回执确认覆盖当前水位后，清理已落盘的最低水位要求（并发新编辑推进更高水位时自动保留）。
    pub fn clear_saved_watermark(&self, saved_wm: Option<u64>) {
        let Some(wm) = saved_wm else {
            return;
        };
        let _ = self.min_saved_watermark.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |cur| if cur <= wm { Some(0) } else { Some(cur) },
        );
    }

    /// 在发起后台保存（拍摄 `Workbook` 快照）的同一时刻捕获权威房间水位 `(wm_seq, struct_rev)`，
    /// 杜绝写盘期间新编辑推进水位导致检查点超前；既可传给 `broadcast_saved_with_cursor`，也会自动暂存供同 `clock` 的 `broadcast_saved` 消费。
    #[allow(dead_code)]
    pub fn note_save_snapshot(&self, clock: u64) -> Option<(u64, usize)> {
        if !self.hosting {
            return None;
        }
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        let wm_seq = self.host_watermark.as_ref()?.lock().unwrap().seq;
        let srev = self
            .host_structs
            .as_ref()
            .map_or(0, |h| h.lock().unwrap().len());
        *self.staged_save_cursor.lock().unwrap() = Some((clock, wm_seq, srev));
        Some((wm_seq, srev))
    }

    /// 使用拍保存快照时刻捕获的 `(wm_seq, struct_rev)` 异步校验文件 `(len, modified)` 稳定后登记检查点并广播 `Saved`。
    #[allow(dead_code)]
    pub fn broadcast_saved_with_cursor(&self, clock: u64, cursor: Option<(u64, usize)>) {
        self.broadcast_saved_impl(clock, cursor, true);
    }

    fn broadcast_saved_impl(
        &self,
        clock: u64,
        cursor: Option<(u64, usize)>,
        explicit_cursor: bool,
    ) {
        if !self.hosting {
            return;
        }
        {
            let mut staged = self.staged_save_cursor.lock().unwrap();
            if staged.is_some_and(|(c, _, _)| c <= clock) {
                *staged = None;
            }
        }
        if let (Some((saved_seq, saved_srev)), Some(hp), Some(wm)) =
            (cursor, &self.host_file_path, &self.host_watermark)
            && let Some(path) = hp.lock().unwrap().clone()
        {
            let wm_clone = wm.clone();
            let hs_clone = self.host_structs.clone();
            let alive = self.alive.clone();
            std::thread::spawn(move || {
                if !alive.load(Ordering::Acquire) {
                    return;
                }
                let Ok(md_before) = std::fs::metadata(&path) else {
                    return;
                };
                let expected_len = md_before.len();
                let Ok(mtime_before) = md_before.modified() else {
                    return;
                };
                if expected_len == 0 {
                    return;
                }
                let Ok(mut f) = std::fs::File::open(&path) else {
                    return;
                };
                let mut hasher = Sha256::new();
                let mut buf = vec![0u8; 256 * 1024];
                let mut total_read = 0u64;
                loop {
                    match std::io::Read::read(&mut f, &mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            total_read += n as u64;
                            hasher.update(&buf[..n]);
                        }
                        Err(_) => return,
                    }
                }
                drop(f);
                let Ok(md_after) = std::fs::metadata(&path) else {
                    return;
                };
                let Ok(mtime_after) = md_after.modified() else {
                    return;
                };
                // 仅当读盘前后文件大小与修改时间完全一致时才登记检查点，防止并发二次覆盖污染检查点环
                if total_read != expected_len
                    || md_after.len() != expected_len
                    || mtime_after != mtime_before
                    || !alive.load(Ordering::Acquire)
                {
                    return;
                }
                let hash = hex::encode(hasher.finalize());
                let cur_srev = hs_clone.as_ref().map_or(0, |h| h.lock().unwrap().len());
                let mut wm_guard = wm_clone.lock().unwrap();
                if !explicit_cursor && (wm_guard.seq != saved_seq || cur_srev != saved_srev) {
                    return;
                }
                wm_guard.record_checkpoint(hash, saved_seq, saved_srev);
            });
        }
        let watermark_seq = cursor.map(|(seq, _)| seq);
        let msg = Msg::Saved {
            site: self.site,
            clock,
            watermark_seq,
        };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    pub fn broadcast_saved(&self, clock: u64) {
        if !self.hosting {
            return;
        }
        let staged = {
            let mut guard = self.staged_save_cursor.lock().unwrap();
            if guard.is_some_and(|(c, _, _)| c == clock) {
                guard.take().map(|(_, seq, srev)| (seq, srev))
            } else {
                None
            }
        };
        if let Some(cur) = staged {
            self.broadcast_saved_impl(clock, Some(cur), true);
            return;
        }
        let cursor = (|| {
            let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
            let max_clk = self
                .host_max_clock
                .as_ref()
                .map_or(0, |c| c.load(Ordering::Acquire));
            let wm_seq = self.host_watermark.as_ref()?.lock().unwrap().seq;
            let srev = self
                .host_structs
                .as_ref()
                .map_or(0, |h| h.lock().unwrap().len());
            // 未显式调 `note_save_snapshot` 时，仅在写盘期间逻辑时钟与房间水位均未越过快照时刻 `clock` 时才登记检查点
            (max_clk <= clock && wm_seq <= clock.max(1)).then_some((wm_seq, srev))
        })();
        self.broadcast_saved_impl(clock, cursor, false);
    }

    /// 广播格式变更（自动启用调色板字典编码，消除数十万格重复序列化 `CellStyle`）
    pub fn broadcast_style(&self, sheet: usize, items: Vec<(u32, u32, sheetcore::style::CellStyle)>) {
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        if items.is_empty() {
            return;
        }
        let keys: Vec<(u32, u32)> = items.iter().map(|(r, c, _)| (*r, *c)).collect();
        if let Some((holder, holder_name, row, col)) = self.host_lock_conflict_cells(sheet, &keys) {
            let axis_snap = self
                .host_watermark
                .as_ref()
                .map(|wm| wm.lock().unwrap().axis_styles_snapshot());
            let revert_styles = if let Some(st) = &self.host_styles {
                keys.iter()
                    .map(|(r, c)| {
                        (
                            *r,
                            *c,
                            latest_cell_style(
                                self.host_baseline.as_ref(),
                                axis_snap.as_ref(),
                                st,
                                sheet,
                                *r,
                                *c,
                            ),
                        )
                    })
                    .collect()
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
        if self.hosting {
            let own = items.iter().map(|(r,c,s)| (sheet,*r,*c,s.clone())).collect();
            let _ = self.evt_tx.send(NetEvent::ConfirmedStyles(own));
        }
        let use_palette = self.hosting || self.palette_styles_cap.load(Ordering::Acquire);
        if let Some(roster) = &self.host_roster {
            let wm_seq = self
                .host_watermark
                .as_ref()
                .map(|wm| wm.lock().unwrap().record_styles(sheet, &items));
            let struct_rev = self
                .host_structs
                .as_ref()
                .map_or(0, |h| h.lock().unwrap().len());
            let msg = if use_palette && items.len() > 1 {
                let (palette, encoded) = sheetcore::sync::encode_style_palette(&items);
                Msg::StylePaletteOp {
                    site: self.site,
                    sheet,
                    ts: 0,
                    palette,
                    items: encoded,
                }
            } else {
                Msg::StyleOp {
                    site: self.site,
                    sheet,
                    ts: 0,
                    items,
                }
            };
            queue_room_change(&self.cmd, roster, None, RoomChange::Message(msg));
            if let Some(seq) = wm_seq {
                emit_room_watermark(&self.cmd, roster, seq, struct_rev);
            }
        } else {
            let msg = if use_palette && items.len() > 1 {
                let (palette, encoded) = sheetcore::sync::encode_style_palette(&items);
                Msg::StylePaletteOp {
                    site: self.site,
                    sheet,
                    ts: 0,
                    palette,
                    items: encoded,
                }
            } else {
                Msg::StyleOp {
                    site: self.site,
                    sheet,
                    ts: 0,
                    items,
                }
            };
            self.note_local_watermark_edit();
            let _ = self.cmd.send(Cmd::Style(msg));
        }
    }

    /// 广播整行 / 整列默认样式变更
    pub fn broadcast_axis_styles(&self, sheet: usize, patch: AxisStylePatch) {
        if patch.is_empty() {
            return;
        }
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        if let Some(locks) = &self.host_locks {
            self.expire_host_locks();
            let now = self.host_now_ms();
            let lt = locks.lock().unwrap();
            let conflict = lt.iter().find_map(|((sh, r, c), h)| {
                if sh != sheet || h.site == self.site || now.saturating_sub(h.at_ms) >= lt.ttl_ms {
                    return None;
                }
                let hit_row = patch.rows.iter().any(|(pr, _)| *pr == r);
                let hit_col = patch.cols.iter().any(|(pc, _)| *pc == c);
                (hit_row || hit_col).then(|| (h.site, h.name.clone(), r, c))
            });
            if let Some((holder, holder_name, row, col)) = conflict {
                let _ = self.evt_tx.send(NetEvent::WriteRejected {
                    holder,
                    holder_name,
                    sheet,
                    row,
                    col,
                    revert_ops: Vec::new(),
                    revert_styles: Vec::new(),
                    revert_merge: None,
                });
                return;
            }
        }
        let msg = Msg::AxisStyleOp {
            site: self.site,
            sheet,
            ts: 0,
            patch: patch.clone(),
        };
        if let Some(roster) = &self.host_roster {
            let wm_seq = self
                .host_watermark
                .as_ref()
                .map(|wm| wm.lock().unwrap().record_axis(sheet, &patch));
            let struct_rev = self
                .host_structs
                .as_ref()
                .map_or(0, |h| h.lock().unwrap().len());
            queue_room_change(&self.cmd, roster, None, RoomChange::Message(msg));
            if let Some(seq) = wm_seq {
                emit_room_watermark(&self.cmd, roster, seq, struct_rev);
            }
        } else {
            self.note_local_watermark_edit();
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
            let wm_seq = self
                .host_watermark
                .as_ref()
                .map(|wm| wm.lock().unwrap().record_merge(sheet, rect, on));
            let struct_rev = self
                .host_structs
                .as_ref()
                .map_or(0, |h| h.lock().unwrap().len());
            queue_room_change(&self.cmd, roster, None, RoomChange::Message(msg));
            if let Some(seq) = wm_seq {
                emit_room_watermark(&self.cmd, roster, seq, struct_rev);
            }
        } else if let Ok(s) = serde_json::to_string(&msg) {
            self.note_local_watermark_edit();
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    /// 广播工作表高频非单元格元数据变更（列宽/行高/隐藏/冻结/批注/超链接/筛选/工作表标签）。
    /// 返回 `true` 表示载荷合法并已提交发送队列；超限或非法载荷返回 `false`（调用方不得误标为已同步）。
    pub fn broadcast_sheet_meta(&self, sheet: usize, kind: sheetcore::sync::SheetMetaKind) -> bool {
        if !kind.valid(sheet) || !self.alive.load(Ordering::Acquire) {
            return false;
        }
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        if let Some(log) = &self.host_meta_log {
            let mut g = log.lock().unwrap();
            if g.len() >= 10_000 {
                g.remove(0);
            }
            g.push((sheet, kind.clone()));
        }
        let msg = Msg::SheetMetaOp {
            site: self.site,
            sheet,
            ts: 0,
            kind: kind.clone(),
        };
        if let Some(roster) = &self.host_roster {
            let wm_seq = self
                .host_watermark
                .as_ref()
                .map(|wm| wm.lock().unwrap().record_sheet_meta(sheet, kind));
            let struct_rev = self
                .host_structs
                .as_ref()
                .map_or(0, |h| h.lock().unwrap().len());
            queue_room_change(&self.cmd, roster, None, RoomChange::Message(msg));
            if let Some(seq) = wm_seq {
                emit_room_watermark(&self.cmd, roster, seq, struct_rev);
            }
            true
        } else if let Ok(s) = serde_json::to_string(&msg) {
            self.note_local_watermark_edit();
            self.cmd.send(Cmd::Broadcast(s)).is_ok()
        } else {
            false
        }
    }

    /// 广播结构性改动（宿主顺手把本地快照日志、租约与光标也搬家）
    pub fn broadcast_structural(&self, sheet: usize, op: &sheetcore::structural::Structural) -> Result<(), String> {
        let message = Msg::Structural { site: self.site, sheet, op: op.clone() };
        if !message.is_within_limits() { return Err("插删区域超出工作表范围".into()); }
        let generation = self.connection_generation.load(Ordering::Acquire);
        let _order = self.room_order.as_ref().map(|m| m.lock().unwrap());
        if let Some(history) = &self.host_structs {
            let history = history.lock().unwrap();
            let progress = self.structure_seen.lock().unwrap();
            if history.get(progress.seen..).is_none_or(|later| later.iter().enumerate().any(|(i,(s,_))|
                *s == sheet && !progress.applied_out_of_order.contains(&(progress.seen+i)))) {
                return Err("正在同步工作表结构，请稍后重新执行插删操作".into());
            }
        }
        if let Some((_, name, row, col)) = self.host_lock_conflict_structural(sheet, op) {
            return Err(format!("{name} 正在编辑 {}，请待其完成后再{}", sheetcore::model::cell_name(row, col), op.label()));
        }
        if !self.alive.load(Ordering::SeqCst) || !self.is_live() {
            return Err("协作连接已断开，结构修改尚未执行".into());
        }
        if !self.hosting {
            if !self.structural_ack.load(Ordering::Acquire) {
                return Err("当前宿主版本无法确认插删操作，请由宿主执行或升级宿主".into());
            }
            if self.pending_structure.swap(true, Ordering::AcqRel) {
                return Err("正在等待宿主确认上一项插删操作".into());
            }
            let msg = Msg::StructuralRequest { site: self.site, sheet, revision: self.structure_seen(), op: op.clone() };
            if self.cmd.send(Cmd::GuestStructure(generation,msg)).is_ok() {
                return Ok(());
            }
            self.finish_structure();
            return Err("协作连接已断开，结构修改尚未执行".into());
        }
        let queued = queue_room_change(&self.cmd,self.host_roster.as_ref().unwrap(),None,RoomChange::Message(message));
        if !queued { return Err("协作发送队列已关闭，结构修改尚未执行".into()); }
        if let (
            Some(baseline),
            Some(fk),
            Some(state),
            Some(delta_ops),
            Some(bh),
            Some(st),
            Some(ds),
            Some(locks),
            Some(cursors),
        ) = (
            &self.host_baseline,
            &self.formula_keys,
            &self.state,
            &self.delta_ops,
            &self.host_base_hash,
            &self.host_styles,
            &self.delta_styles,
            &self.host_locks,
            &self.host_cursors,
        ) {
            shift_state(
                baseline,
                fk,
                state,
                delta_ops,
                bh,
                st,
                ds,
                locks,
                cursors,
                &self.sheet_names.lock().unwrap(),
                sheet,
                op,
            );
        }
        let mut struct_rev = 0usize;
        if let Some(h) = &self.host_structs {
            let mut history = h.lock().unwrap();
            let index = history.len();
            history.push((sheet, op.clone()));
            struct_rev = history.len();
            let mut progress = self.structure_seen.lock().unwrap();
            progress.applied_out_of_order.insert(index);
            progress.consume(0);
        }
        if let (Some(wm), Some(roster)) = (&self.host_watermark, &self.host_roster) {
            let seq = wm.lock().unwrap().shift_structural(sheet, op);
            emit_room_watermark(&self.cmd, roster, seq, struct_rev);
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
        self.cursor_throttle
            .send_throttled(cur, &self.cmd, &self.alive);
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
        self.cursor_throttle.rename_pending(&canonical);
        let msg = Msg::Rename { site: self.site, name: canonical.clone() };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
        let _ = self.evt_tx.send(NetEvent::PeerRenamed(self.site, canonical));
    }

    /// 向房间成员宣告本机持有经过官方数字签名的版本（若本机为正式签名包；后台异步预热缓存绝不阻塞 UI 主线程）
    pub fn announce_local_signed_update(&self) {
        if let Ok(g) = self.update_seed_override.lock() {
            if let Some((_, manifest)) = g.as_ref() {
                self.announce_update(manifest);
                return;
            }
        }
        let my_site = self.site;
        let cmd = self.cmd.clone();
        let alive = self.alive.clone();
        crate::update::warm_local_signed_bundle_async(move |bundle| {
            if !alive.load(Ordering::Acquire) {
                return;
            }
            let Some((_, manifest)) = bundle else {
                return;
            };
            if !crate::update::verify_manifest_sig(&manifest) {
                return;
            }
            let msg = Msg::UpdateAnnounce {
                site: my_site,
                version: manifest.version,
                size: manifest.size,
                sha256: manifest.sha256,
                sig: manifest.sig,
                notes: manifest.notes,
            };
            if let Ok(s) = serde_json::to_string(&msg) {
                let _ = cmd.send(Cmd::Broadcast(s));
            }
        });
    }

    /// 向指定新加入成员单播本机的签名版本宣告（宿主在新成员入房后调用；后台异步预热缓存绝不阻塞 UI 主线程）
    pub fn announce_local_signed_update_to(&self, target_site: u32) {
        if let Ok(g) = self.update_seed_override.lock() {
            if let Some((_, manifest)) = g.clone() {
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
                return;
            }
        }
        let my_site = self.site;
        let cmd = self.cmd.clone();
        let alive = self.alive.clone();
        crate::update::warm_local_signed_bundle_async(move |bundle| {
            if !alive.load(Ordering::Acquire) {
                return;
            }
            let Some((_, manifest)) = bundle else {
                return;
            };
            let msg = Msg::UpdateAnnounce {
                site: my_site,
                version: manifest.version,
                size: manifest.size,
                sha256: manifest.sha256,
                sig: manifest.sig,
                notes: manifest.notes,
            };
            if let Ok(s) = serde_json::to_string(&msg) {
                let _ = cmd.send(Cmd::SendTo(target_site, s));
            }
        });
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
        let part_name = format!("peer-{}.part", self.site);
        let part_path = crate::update::staging_file_path(&manifest.version, &part_name)
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
            let expected_size = manifest.size;
            *g = Some(ActivePeerDownload {
                peer_site,
                manifest: manifest.clone(),
                file,
                part_path,
                prefix_hash: crate::update::PrefixSha256::new(expected_size),
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

    #[cfg(test)]
    pub(crate) fn disconnect_socket_for_test(&self) {
        for s in self.sockets.lock().unwrap().iter().filter_map(Weak::upgrade) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    pub fn shutdown(&self) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(addr) = self.host_wake_addr {
            // 唤醒阻塞在 accept() 的宿主监听线程，避免 40ms 轮询且确保端口即时释放
            let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(100));
        }
        for socket in self.sockets.lock().unwrap().drain(..).filter_map(|s| s.upgrade()) {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        self.seeder_queue.shutdown();
        self.cursor_throttle.shutdown();
        self.finish_structure();
        self.pending_sorts.lock().unwrap().clear();
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
    let local_addr = listener.local_addr()?;
    let port = local_addr.port();
    let host_wake_addr = match local_addr {
        std::net::SocketAddr::V4(v4) if v4.ip().is_unspecified() => {
            std::net::SocketAddr::from(([127, 0, 0, 1], port))
        }
        std::net::SocketAddr::V6(v6) if v6.ip().is_unspecified() => {
            std::net::SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port))
        }
        other => other,
    };
    listener.set_nonblocking(false)?;
    // 后台非阻塞预热本机签名包缓存，避免成员入房时在 UI 线程同步读自身 60 MB 算 SHA-256
    let _ = crate::update::try_cached_local_signed_bundle();
    // 协议 v2：开房时仅生成 16B 随机 salt（微秒级），20 万轮 PBKDF2 放入后台 accept 线程执行以绝不阻塞 UI 主线程
    let mut room_salt = [0u8; 16];
    getrandom::fill(&mut room_salt)
        .map_err(|e| std::io::Error::other(format!("系统随机数不可用：{e}")))?;
    let room_salt_b64 = sheetcore::wire::b64_encode(&room_salt);
    let sockets = Arc::new(Mutex::new(Vec::new()));
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (evt_raw_tx, evt_rx) = channel::<NetEvent>();
    let (evt_tx, wake_ctx) = EvtTx::new(evt_raw_tx);
    let alive = Arc::new(AtomicBool::new(true));
    let connected = Arc::new(AtomicBool::new(true));

    let roster = Arc::new(Mutex::new(RoomRoster::new(site, name)));
    let roster_for_self = roster.clone();
    let room_order = Arc::new(Mutex::new(()));
    let sheet_names: Arc<Mutex<Vec<String>>> = Default::default();
    #[cfg(test)]
    let after_snapshot: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>> = Default::default();
    #[cfg(test)]
    let before_sort_reply: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>> = Default::default();
    #[cfg(test)]
    let before_sort_commit: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>> = Default::default();
    let host_baseline: SharedBaseline = Default::default();
    let host_baseline_for_self = host_baseline.clone();
    let formula_keys: Arc<Mutex<BTreeSet<(usize, u32, u32)>>> = Default::default();
    let formula_keys_for_self = formula_keys.clone();
    let host_watermark: Arc<Mutex<HostWatermark>> = Default::default();
    let host_watermark_for_self = host_watermark.clone();
    let host_max_clock = Arc::new(AtomicU64::new(1));
    let host_max_clock_for_self = host_max_clock.clone();
    let state: SharedGrid<Op> = Default::default();
    let delta_ops: SharedGrid<Op> = Default::default();
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
                            let shared: Vec<Arc<Op>> = ops.into_iter().map(Arc::new).collect();
                            shared_cell_frames_to_peers(&shared, &senders);
                        }
                    }
                    Cmd::SharedCells(ops) => {
                        let senders = roster.lock().unwrap().all_senders();
                        shared_cell_frames_to_peers(&ops, &senders);
                    }
                    Cmd::BroadcastMsg(msg) => {
                        let senders = roster.lock().unwrap().all_senders();
                        broadcast_msg_to(&senders, &msg);
                    }
                    Cmd::GuestStructure(..) | Cmd::Sort(..) | Cmd::PumpUpdate => {
                        unreachable!("guest command on host")
                    }
                    Cmd::Style(_) => unreachable!("client style dispatch is guest-only"),
                    Cmd::Deliver(peers, change) => {
                        let send = |line: String| {
                            broadcast_line_to(&peers, line);
                        };
                        match change {
                            RoomChange::Sort(batch) => {
                                for peer in &peers {
                                    let _ = peer.tx.send(LiveFrame::Sort(batch.clone()));
                                }
                            }
                            RoomChange::Correction(correction) => {
                                for peer in &peers { let _ = peer.tx.send(LiveFrame::Correction(correction.clone())); }
                            }
                            RoomChange::Cells(ops) => {
                                let shared: Vec<Arc<Op>> = ops.into_iter().map(Arc::new).collect();
                                shared_cell_frames_to_peers(&shared, &peers);
                            }
                            RoomChange::SharedCells(ops) => {
                                shared_cell_frames_to_peers(&ops, &peers);
                            }
                            RoomChange::Message(msg) => match &msg {
                                Msg::Watermark { .. } | Msg::CursorMsg(_) => {
                                    broadcast_msg_to(&peers, &msg);
                                }
                                Msg::StylePaletteOp { items, .. }
                                    if items.len() <= sheetcore::sync::MAX_BATCH_ITEMS =>
                                {
                                    let (compact_peers, legacy_peers): (Vec<_>, Vec<_>) =
                                        peers.iter().cloned().partition(LiveSender::is_compact_v3);
                                    if !compact_peers.is_empty() {
                                        let raw = sheetcore::sync::encode_msg_compact(&msg);
                                        if raw.len() <= MAX_PLAIN_LINE {
                                            let prep =
                                                sheetcore::wire::prepare_compact_payload(raw, true);
                                            for p in &compact_peers {
                                                let _ = p.send_prepared(Arc::clone(&prep));
                                            }
                                        } else {
                                            let send_c = |line: String| {
                                                broadcast_line_to(&compact_peers, line);
                                                true
                                            };
                                            if !send_room_message(msg.clone(), MAX_PLAIN_LINE, send_c, || {}) {
                                                let _ = dispatch_events.send(NetEvent::Error("协作修改未能完整发送".into()));
                                            }
                                        }
                                    }
                                    if !legacy_peers.is_empty() {
                                        let send_l = |line: String| {
                                            broadcast_line_to(&legacy_peers, line);
                                            true
                                        };
                                        if !send_room_message(msg, MAX_PLAIN_LINE, send_l, || {}) {
                                            let _ = dispatch_events.send(NetEvent::Error("协作修改未能完整发送".into()));
                                        }
                                    }
                                }
                                _ => {
                                    if !send_room_message(msg, MAX_PLAIN_LINE, |line| { send(line); true }, || {}) {
                                        let _ = dispatch_events.send(NetEvent::Error("协作修改未能完整发送".into()));
                                    }
                                }
                            },
                        }
                    }
                    Cmd::Broadcast(s) => {
                        let senders = roster.lock().unwrap().all_senders();
                        broadcast_line_to(&senders, s);
                    }
                    Cmd::SendTo(target_site, s) => {
                        let target = roster.lock().unwrap().sender_of(target_site);
                        if let Some(tx) = target {
                            let _ = tx.send(s);
                        }
                    }
                    Cmd::SendPrepared(target_site, prepared) => {
                        let target = roster.lock().unwrap().sender_of(target_site);
                        if let Some(tx) = target {
                            let _ = tx.send_prepared(prepared);
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
    let meta_log: Arc<Mutex<Vec<(usize, sheetcore::sync::SheetMetaKind)>>> =
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
    let meta_log_for_self = meta_log.clone();
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
    let host_seeder_queue = UpdateSeederQueue::default();
    let host_seeder_for_self = host_seeder_queue.clone();
    let host_file_path: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::new(Mutex::new(None));
    let host_site_id = site;

    let (ready, initialized) = channel();
    // accept 循环
    {
        let sockets = sockets.clone();
        let sheet_names = sheet_names.clone();
        let room_order = room_order.clone();
        #[cfg(test)]
        let after_snapshot = after_snapshot.clone();
        #[cfg(test)]
        let before_sort_reply = before_sort_reply.clone();
        #[cfg(test)]
        let before_sort_commit = before_sort_commit.clone();
        let cmd = cmd_tx.clone();
        let roster = roster.clone();
        let host_baseline_outer = host_baseline.clone();
        let formula_keys_outer = formula_keys.clone();
        let host_watermark_outer = host_watermark.clone();
        let host_max_clock_outer = host_max_clock.clone();
        let room_session_id = session_id.clone();
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
        let meta_log_outer = meta_log.clone();
        let structs_outer = structs.clone();
        let update_dl_outer = host_update_dl.clone();
        let seed_override_outer = host_seed_override.clone();
        let seeder_queue_outer = host_seeder_queue.clone();
        let now_ms_outer = now_ms;
        std::thread::spawn(move || {
            let room_key = sheetcore::wire::RoomKey::derive(&password, &room_salt);
            drop(password);
            if initialized.recv().is_err() { return; }
            let active_conns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            while alive.load(Ordering::SeqCst) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                if !alive.load(Ordering::SeqCst) {
                    drop(stream);
                    break;
                }
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
                let sheet_names = sheet_names.clone();
                let room_order = room_order.clone();
                #[cfg(test)]
                let after_snapshot = after_snapshot.clone();
                #[cfg(test)]
                let before_sort_reply = before_sort_reply.clone();
                #[cfg(test)]
                let before_sort_commit = before_sort_commit.clone();
                let cmd = cmd.clone();
                let host_baseline = host_baseline_outer.clone();
                let formula_keys = formula_keys_outer.clone();
                let host_watermark = host_watermark_outer.clone();
                let host_max_clock = host_max_clock_outer.clone();
                let room_session_id = room_session_id.clone();
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
                let meta_log = meta_log_outer.clone();
                let structs = structs_outer.clone();
                let update_dl = update_dl_outer.clone();
                let seed_override = seed_override_outer.clone();
                let seeder_queue = seeder_queue_outer.clone();
                let alive_conn = alive.clone();
                let now_ms = now_ms_outer;
                std::thread::spawn(move || {
                    let _slot = slot;
                    let write_raw = match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let _ = write_raw.set_write_timeout(Some(Duration::from_secs(10)));
                    let mut write_half = BufWriter::with_capacity(128 * 1024, write_raw);
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
                    let output = Arc::new(Mutex::new(HostOutput::new(write_half, sess.fork_sender())));
                    let reader = BufReader::with_capacity(64 * 1024, &*stream);
                    let (live, rx) = channel::<LiveFrame>();
                    let peer_compact_flag = Arc::new(AtomicBool::new(false));
                    let (tx, low_prio_q) = LiveSender::new(live, peer_compact_flag.clone());
                    let (ack_tx, ack_rx) = channel::<(u64,SortResponse)>();
                    let can_sort = Arc::new(AtomicBool::new(false));
                    let write_sort = can_sort.clone();
                    let (intro, intro_rx) = std::sync::mpsc::sync_channel::<IntroFrame>(8);
                    let mut intro = Some(intro);
                    let write_out = output.clone();
                    std::thread::spawn(move || {
                        let send_line_opt = |s: String, flush: bool| -> bool {
                            let mut out = write_out.lock().unwrap();
                            if s == CTRL_ENABLE_BINARY_V3 {
                                out.binary_v3 = true;
                                return !flush || out.writer.flush().is_ok();
                            }
                            if s == CTRL_ENABLE_COMPACT_V3 {
                                out.binary_v3 = true;
                                out.compact_v3 = true;
                                return !flush || out.writer.flush().is_ok();
                            }
                            if let Some(denial) = s.strip_prefix(CTRL_RAW_LINE_PREFIX) {
                                return write_line_opt(&mut out.writer, denial, flush).is_ok();
                            }
                            out.write_plain_opt(&s, flush).is_ok()
                        };
                        let send_line = |s: String| send_line_opt(s, true);

                        let mut intro_ok = true;
                        let mut pending_intro = intro_rx.recv().ok();
                        while let Some(item) = pending_intro.take() {
                            let next = intro_rx.try_recv().ok();
                            let flush_now = next.is_none();
                            let ok = match item {
                                IntroFrame::Line(s) => send_line_opt(s, flush_now),
                                IntroFrame::Compact(raw) => {
                                    let mut out = write_out.lock().unwrap();
                                    out.write_compact_opt(&raw, flush_now).is_ok()
                                }
                            };
                            if !ok {
                                intro_ok = false;
                                break;
                            }
                            pending_intro = next.or_else(|| intro_rx.recv().ok());
                        }

                        if intro_ok {
                            let mut next_frame = rx.recv().ok();
                            while let Some(first) = next_frame.take() {
                                let mut cur = Some(first);
                                let mut batch_ok = true;
                                while let Some(frame) = cur.take() {
                                    let peek = rx.try_recv().ok();
                                    let flush_now = peek.is_none()
                                        && low_prio_q.0.lock().unwrap().0.is_empty();
                                    let ok = match frame {
                                        LiveFrame::Line(line) => send_line_opt(line, flush_now),
                                        LiveFrame::Compact(raw) => {
                                            let mut out = write_out.lock().unwrap();
                                            out.write_compact_opt(&raw, flush_now).is_ok()
                                        }
                                        LiveFrame::Prepared(prepared) => {
                                            let mut out = write_out.lock().unwrap();
                                            out.write_prepared_opt(&prepared, flush_now).is_ok()
                                        }
                                        LiveFrame::Correction(correction) => {
                                            correction.chunks(|sheet,first_ts,items| send_room_message(
                                                Msg::SortRepairs { sheet,first_ts,items },MAX_PLAIN_LINE,send_line,|| {}))
                                                && send_line(serde_json::to_string(&Msg::SortRepairDone { first_ts:correction.batch.plan.first_ts }).unwrap())
                                        }
                                        LiveFrame::Sort(batch) => send_sort(&batch, write_sort.load(Ordering::Acquire), false, true, &ack_rx, send_line),
                                        LiveFrame::PumpUpdate => true,
                                    };
                                    if !ok {
                                        batch_ok = false;
                                        break;
                                    }
                                    cur = peek;
                                }
                                if !batch_ok {
                                    break;
                                }
                                // 高优通道已排空：穿插发送低优 P2P 更新分片，每发 1 片立即检查是否有新实时协作帧到达
                                let mut slice_ok = true;
                                loop {
                                    if let Ok(hp) = rx.try_recv() {
                                        next_frame = Some(hp);
                                        break;
                                    }
                                    let popped = {
                                        let (lock, cvar) = &*low_prio_q;
                                        let mut g = lock.lock().unwrap();
                                        let item = g.0.pop_front();
                                        let empty = g.0.is_empty();
                                        if item.is_some() {
                                            cvar.notify_one();
                                        }
                                        (item, empty)
                                    };
                                    let (Some(slice), queue_empty) = popped else {
                                        let _ = write_out.lock().unwrap().writer.flush();
                                        break;
                                    };
                                    let ok = {
                                        let mut out = write_out.lock().unwrap();
                                        out.write_prepared_opt(&slice, queue_empty).is_ok()
                                    };
                                    if !ok {
                                        slice_ok = false;
                                        break;
                                    }
                                    if queue_empty {
                                        break;
                                    }
                                }
                                if !slice_ok {
                                    break;
                                }
                                if next_frame.is_none() {
                                    next_frame = rx.recv().ok();
                                }
                            }
                        }
                        {
                            let (lock, cvar) = &*low_prio_q;
                            let mut g = lock.lock().unwrap();
                            g.0.clear();
                            g.1 = true;
                            cvar.notify_all();
                        }
                        let _ = write_out.lock().unwrap().writer.get_ref().shutdown(std::net::Shutdown::Both);
                    });

                    let expire_and_broadcast = || {
                        if locks.lock().unwrap().is_empty() {
                            return;
                        }
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

                    let commit_sort = |batch: Arc<SortBatch>, struct_rev: usize| {
                        if let Some(m) = batch.ops.iter().map(|o| o.ts).max() {
                            host_max_clock.fetch_max(m, Ordering::AcqRel);
                        }
                        update_formula_keys_for_sort(&formula_keys, &batch.ops);
                        let seq = host_watermark.lock().unwrap().record_sort(&batch);
                        let site = batch.plan.site;
                        queue_room_change(&cmd,&roster,Some(site),RoomChange::Sort(batch.clone()));
                        let _ = cmd.send(Cmd::Deliver(vec![tx.clone()],RoomChange::Message(
                            Msg::SortCommitted { first_ts: batch.plan.first_ts })));
                        emit_room_watermark(&cmd, &roster, seq, struct_rev);
                        let _ = evt_tx.send(NetEvent::Sorted(batch));
                    };
                    let mut fallback: Option<SortBatch> = None;
                    let mut hello_site: Option<u32> = None;
                    let mut reader = reader;
                    loop {
                        let allow_binary = hello_site.is_some();
                        let limit = if allow_binary { MAX_LINE } else { MAX_PREAUTH_LINE };
                        let Ok(Some(frame)) = read_wire_frame(&mut reader, limit, allow_binary) else { break };
                        let mut raw_json_line: Option<String> = None;
                        let decoded_msg: Option<Msg> = match frame {
                            WireFrame::Text(line) => match sess.open(&line) {
                                Some(plain) => {
                                    let parsed = serde_json::from_str::<Msg>(&plain).ok();
                                    raw_json_line = Some(plain);
                                    parsed
                                }
                                None => {
                                    if hello_site.is_none() {
                                        let deny = serde_json::to_string(&Msg::Denied {
                                            reason: "口令不正确或客户端不支持加密协作".into(),
                                        }).unwrap();
                                        let _ = tx.send(format!("{CTRL_RAW_LINE_PREFIX}{deny}"));
                                    }
                                    break;
                                }
                            },
                            WireFrame::Binary(body) => match sess.open_binary_frame_owned(body) {
                                Some(sheetcore::wire::DecodedWireFrame::Text(plain)) => {
                                    let parsed = serde_json::from_str::<Msg>(&plain).ok();
                                    raw_json_line = Some(plain);
                                    parsed
                                }
                                Some(sheetcore::wire::DecodedWireFrame::Compact(bytes)) => {
                                    // 零 JSON 转发 P2P 二进制自更新分片：宿主仅校验头部站点号并直投目标低优队列
                                    if let Some(conn_site) = hello_site
                                        && let Some((from_s, to_s)) =
                                            sheetcore::sync::peek_compact_update_slice_target(&bytes)
                                        && from_s == conn_site
                                        && to_s != conn_site
                                        && to_s != host_site_id
                                        && bytes.len() <= 390_000
                                    {
                                        if let Some(target_tx) = roster.lock().unwrap().sender_of(to_s)
                                            && target_tx.is_compact_v3()
                                        {
                                            let _ = target_tx.send_update_slice(
                                                sheetcore::wire::prepare_compact_payload(bytes, false),
                                            );
                                            continue;
                                        }
                                    }
                                    sheetcore::sync::decode_msg_compact(&bytes)
                                }
                                None => break,
                            },
                        };
                        let Some(msg) = decoded_msg else { continue };
                        // 坐标、数量、时间戳越界的报文整条丢弃，不进宿主状态、不转发
                        if !msg.is_within_limits() {
                            continue;
                        }
                        // A connection can introduce itself once. No workbook frames precede Hello.
                        if matches!(msg, Msg::Hello { .. }) != hello_site.is_none() {
                            break;
                        }
                        match msg {
                            Msg::Hello {
                                site,
                                name,
                                proof: got,
                                doc: _,
                                auth,
                                base_hash: client_hash,
                                sort_permutation,
                                palette_styles,
                                resume_room,
                                resume_watermark,
                                resume_struct_rev,
                                binary_v3,
                                compact_v3,
                            } => {
                                can_sort.store(sort_permutation, Ordering::Release);
                                let use_compact_v3 = binary_v3 && compact_v3;
                                peer_compact_flag.store(use_compact_v3, Ordering::Release);
                                let live_tx = tx.clone();
                                let tx = intro.take().expect("only one introduction");
                                if !proof_matches(&got, &room_key.proof(nonce)) {
                                    let _ = tx.send(IntroFrame::Line(
                                        serde_json::to_string(&Msg::Denied {
                                            reason: "口令不正确".into(),
                                        })
                                        .unwrap(),
                                    ));
                                    break;
                                }
                                let order = room_order.lock().unwrap();
                                let admitted = roster.lock().unwrap().admit(site, &name, &auth);
                                let (canonical_name, known) = match admitted {
                                    Ok(pair) => pair,
                                    Err(reason) => {
                                        let _ = tx.send(IntroFrame::Line(
                                            serde_json::to_string(&Msg::Denied {
                                                reason: reason.into(),
                                            })
                                            .unwrap(),
                                        ));
                                        break;
                                    }
                                };
                                hello_site = Some(site);
                                // 已认证成员可以长时间不说话（只看不改），取消认证前的读超时
                                let _ = stream.set_read_timeout(None);
                                let color = (site % 7) as u8;
                                let hist = structs.lock().unwrap().clone();
                                let struct_rev = hist.len();
                                let wm_snapshot = host_watermark.lock().unwrap().clone();
                                let current_wm = wm_snapshot.seq;
                                let can_resume = !resume_room.is_empty()
                                    && resume_room == room_session_id
                                    && resume_watermark >= 1
                                    && resume_watermark >= wm_snapshot.meta_floor_seq
                                    && resume_watermark <= current_wm
                                    && resume_struct_rev <= struct_rev
                                    && (resume_struct_rev == struct_rev
                                        || (wm_snapshot
                                            .merges
                                            .partition_point(|(s, ..)| *s <= resume_watermark)
                                            == wm_snapshot.merges.len()
                                            && wm_snapshot
                                                .sheet_meta
                                                .partition_point(|(s, ..)| *s <= resume_watermark)
                                                == wm_snapshot.sheet_meta.len()));
                                let checkpoint_match = (!can_resume && !client_hash.is_empty())
                                    .then(|| wm_snapshot.find_checkpoint(&client_hash))
                                    .flatten();
                                let can_checkpoint_delta = !can_resume
                                    && checkpoint_match.is_some_and(|(cp_wm, cp_srev)| {
                                        cp_wm >= 1
                                            && cp_wm >= wm_snapshot.meta_floor_seq
                                            && cp_wm <= current_wm
                                            && cp_srev <= struct_rev
                                            && (cp_srev == struct_rev
                                                || (wm_snapshot
                                                    .merges
                                                    .partition_point(|(s, ..)| *s <= cp_wm)
                                                    == wm_snapshot.merges.len()
                                                    && wm_snapshot
                                                        .sheet_meta
                                                        .partition_point(|(s, ..)| *s <= cp_wm)
                                                        == wm_snapshot.sheet_meta.len()))
                                    });
                                let can_legacy_hash_delta = !can_resume
                                    && !can_checkpoint_delta
                                    && !client_hash.is_empty()
                                    && hist.is_empty()
                                    && *host_base_hash.lock().unwrap() == client_hash;
                                let is_wm_delta = can_resume || can_checkpoint_delta;
                                let (delta_wm, delta_srev) = if can_resume {
                                    (resume_watermark, resume_struct_rev)
                                } else if let Some(pair) = checkpoint_match {
                                    pair
                                } else {
                                    (0, 0)
                                };
                                let is_delta = is_wm_delta || can_legacy_hash_delta;

                                // 锁内仅做 O(1) CoW 根指针克隆，绝不在 room_order 锁内遍历百万格！
                                let base_arc = if is_delta {
                                    None
                                } else {
                                    host_baseline.lock().unwrap().clone()
                                };
                                let ops_grid = if can_legacy_hash_delta {
                                    delta_ops.lock().unwrap().clone()
                                } else {
                                    state.lock().unwrap().clone()
                                };
                                let styles_grid = if can_legacy_hash_delta {
                                    delta_styles.lock().unwrap().clone()
                                } else {
                                    styles.lock().unwrap().clone()
                                };
                                let host_clock = host_max_clock.load(Ordering::Acquire).max(1);
                                let merges = if is_wm_delta {
                                    Vec::new()
                                } else {
                                    delta_merges.lock().unwrap().clone()
                                };
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
                                let _ = tx.send(IntroFrame::Line(
                                    serde_json::to_string(&Msg::Welcome {
                                        site,
                                        doc: doc_name.clone(),
                                        peers: known,
                                        delta_sync: is_delta,
                                        clock: host_clock,
                                        batch_ops: true,
                                        sort_permutation: true,
                                        structural_ack: true,
                                        palette_styles: true,
                                        room_id: room_session_id.clone(),
                                        watermark: current_wm,
                                        structure_revision: struct_rev,
                                        binary_v3,
                                        compact_v3: use_compact_v3,
                                    })
                                    .unwrap(),
                                ));
                                // Welcome 仍按 v2 文本行发出；紧随 Welcome 之后切换为协商一致的 v3 流帧
                                if use_compact_v3 {
                                    let _ = tx.send(IntroFrame::Line(CTRL_ENABLE_COMPACT_V3.to_string()));
                                } else if binary_v3 {
                                    let _ = tx.send(IntroFrame::Line(CTRL_ENABLE_BINARY_V3.to_string()));
                                }
                                if canonical_name != name {
                                    let _ = tx.send(IntroFrame::Line(
                                        serde_json::to_string(&Msg::Rename {
                                            site,
                                            name: canonical_name.clone(),
                                        })
                                        .unwrap(),
                                    ));
                                }
                                if is_wm_delta {
                                    if delta_srev > 0 {
                                        let _ = tx.send(IntroFrame::Line(
                                            serde_json::to_string(&Msg::StructureRevision {
                                                revision: delta_srev,
                                            })
                                            .unwrap(),
                                        ));
                                    }
                                    for &(sh_i, ref op) in &hist[delta_srev..] {
                                        let _ = tx.send(IntroFrame::Line(
                                            serde_json::to_string(&Msg::Structural {
                                                site: 0,
                                                sheet: sh_i,
                                                op: op.clone(),
                                            })
                                            .unwrap(),
                                        ));
                                    }
                                } else if !is_delta && !hist.is_empty() {
                                    if std::env::var("SC_DBG").is_ok() {
                                        eprintln!("[dbg] 下发结构历史 {} 条", hist.len());
                                    }
                                    let _ = tx.send(IntroFrame::Line(
                                        serde_json::to_string(&Msg::StructuralLog { items: hist }).unwrap(),
                                    ));
                                }
                                // 整行 / 整列默认样式快照（全量握手发全部非零轴样式，水位重连仅发断线期间增量）
                                {
                                    let min_seq = if is_wm_delta {
                                        delta_wm
                                    } else if can_legacy_hash_delta {
                                        1
                                    } else {
                                        0
                                    };
                                    let mut axis_sheets: BTreeSet<usize> = BTreeSet::new();
                                    axis_sheets.extend(wm_snapshot.axis_rows.keys().copied());
                                    axis_sheets.extend(wm_snapshot.axis_cols.keys().copied());
                                    for sh_idx in axis_sheets {
                                        let rows = wm_snapshot
                                            .axis_rows
                                            .get(&sh_idx)
                                            .map(|m| {
                                                m.iter()
                                                    .filter(|(_, (_, seq))| *seq > min_seq)
                                                    .map(|(&r, (st, _))| (r, st.clone()))
                                                    .collect::<Vec<_>>()
                                            })
                                            .unwrap_or_default();
                                        let cols = wm_snapshot
                                            .axis_cols
                                            .get(&sh_idx)
                                            .map(|m| {
                                                m.iter()
                                                    .filter(|(_, (_, seq))| *seq > min_seq)
                                                    .map(|(&c, (st, _))| (c, st.clone()))
                                                    .collect::<Vec<_>>()
                                            })
                                            .unwrap_or_default();
                                        let patch = AxisStylePatch { rows, cols };
                                        if !patch.is_empty() {
                                            let _ = tx.send(IntroFrame::Line(
                                                serde_json::to_string(&Msg::AxisStyleSnapshot {
                                                    sheet: sh_idx,
                                                    patch,
                                                })
                                                .unwrap(),
                                            ));
                                        }
                                    }
                                }
                                if is_wm_delta {
                                    // 水位增量重连/检查点入房：按 partition_point 切片 O(log N + Δ) 发送 seq > delta_wm 的变更样式与单元格
                                    let use_seq_log = delta_wm >= wm_snapshot.log_floor_seq;
                                    let mut style_ok = true;
                                    if palette_styles {
                                        let mut emitter = OverlayStylePaletteEmitter::new(use_compact_v3);
                                        if use_seq_log {
                                            for (seq, key) in wm_snapshot.style_log.since(delta_wm) {
                                                if wm_snapshot.style_seqs.get(&key) != Some(&seq) {
                                                    continue;
                                                }
                                                if let Some(st) = styles_grid.get(&key) {
                                                    if !emitter.push(key.0, key.1, key.2, st, &tx) {
                                                        style_ok = false;
                                                        break;
                                                    }
                                                }
                                            }
                                        } else {
                                            for (&key, &seq) in wm_snapshot.style_seqs.range(..) {
                                                if seq <= delta_wm {
                                                    continue;
                                                }
                                                if let Some(st) = styles_grid.get(&key) {
                                                    if !emitter.push(key.0, key.1, key.2, st, &tx) {
                                                        style_ok = false;
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                        if style_ok && !emitter.flush(&tx) {
                                            style_ok = false;
                                        }
                                    } else {
                                        let mut style_buf = Vec::new();
                                        let mut push_legacy = |key: SeqKey| -> bool {
                                            if let Some(st) = styles_grid.get(&key) {
                                                style_buf.push((key.0, key.1, key.2, st.as_ref().clone()));
                                                if style_buf.len() >= SNAPSHOT_STYLE_CHUNK {
                                                    if !emit_style_snapshot_chunk(&style_buf, false, false, &tx) {
                                                        return false;
                                                    }
                                                    style_buf.clear();
                                                }
                                            }
                                            true
                                        };
                                        if use_seq_log {
                                            for (seq, key) in wm_snapshot.style_log.since(delta_wm) {
                                                if wm_snapshot.style_seqs.get(&key) == Some(&seq)
                                                    && !push_legacy(key)
                                                {
                                                    style_ok = false;
                                                    break;
                                                }
                                            }
                                        } else {
                                            for (&key, &seq) in wm_snapshot.style_seqs.range(..) {
                                                if seq > delta_wm && !push_legacy(key) {
                                                    style_ok = false;
                                                    break;
                                                }
                                            }
                                        }
                                        if style_ok && !emit_style_snapshot_chunk(&style_buf, false, false, &tx) {
                                            style_ok = false;
                                        }
                                    }
                                    if !style_ok {
                                        break;
                                    }
                                    let merge_start = wm_snapshot
                                        .merges
                                        .partition_point(|(seq, ..)| *seq <= delta_wm);
                                    for &(_, msh, r1, c1, r2, c2, on) in
                                        &wm_snapshot.merges[merge_start..]
                                    {
                                        let _ = tx.send(IntroFrame::Line(
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
                                        ));
                                    }
                                    let meta_start = wm_snapshot
                                        .sheet_meta
                                        .partition_point(|(seq, ..)| *seq <= delta_wm);
                                    if meta_start < wm_snapshot.sheet_meta.len() {
                                        let meta_items: Vec<(usize, sheetcore::sync::SheetMetaKind)> =
                                            wm_snapshot.sheet_meta[meta_start..]
                                                .iter()
                                                .map(|(_, msh, kind)| (*msh, kind.clone()))
                                                .collect();
                                        let _ = tx.send(IntroFrame::Line(
                                            serde_json::to_string(&Msg::SheetMetaSnapshot {
                                                items: meta_items,
                                            })
                                            .unwrap(),
                                        ));
                                    }
                                    let op_chunk_cap = if use_compact_v3 {
                                        SNAPSHOT_COMPACT_OP_CHUNK
                                    } else {
                                        sheetcore::sync::LIVE_OP_BATCH
                                    };
                                    let mut op_refs: Vec<&Op> = Vec::new();
                                    let mut push_op = |key: SeqKey| -> bool {
                                        if let Some(op) = ops_grid.get(&key) {
                                            op_refs.push(op.as_ref());
                                            if op_refs.len() >= op_chunk_cap {
                                                if !send_snapshot_chunk_refs(&op_refs, use_compact_v3, &tx) {
                                                    return false;
                                                }
                                                op_refs.clear();
                                            }
                                        }
                                        true
                                    };
                                    let mut ops_ok = true;
                                    if use_seq_log {
                                        for (seq, key) in wm_snapshot.op_log.since(delta_wm) {
                                            if wm_snapshot.op_seqs.get(&key) == Some(&seq)
                                                && !push_op(key)
                                            {
                                                ops_ok = false;
                                                break;
                                            }
                                        }
                                    } else {
                                        for (&key, &seq) in wm_snapshot.op_seqs.range(..) {
                                            if seq > delta_wm && !push_op(key) {
                                                ops_ok = false;
                                                break;
                                            }
                                        }
                                    }
                                    if !ops_ok || !send_snapshot_chunk_refs(&op_refs, use_compact_v3, &tx) {
                                        break;
                                    }
                                    let end_ok = if use_compact_v3 {
                                        tx.send(IntroFrame::Compact(
                                            sheetcore::sync::encode_snapshot_ref_compact(&[]),
                                        ))
                                        .is_ok()
                                    } else {
                                        tx.send(IntroFrame::Line(
                                            serde_json::to_string(&Msg::Snapshot { ops: Vec::new() })
                                                .unwrap(),
                                        ))
                                        .is_ok()
                                    };
                                    if !end_ok {
                                        break;
                                    }
                                } else {
                                    // 样式与单元格流式分片下发（零百万格深拷贝）
                                    if !stream_snapshot_styles(
                                        base_arc.as_deref(),
                                        &styles_grid,
                                        palette_styles,
                                        use_compact_v3,
                                        &tx,
                                    ) {
                                        break;
                                    }
                                    for (msh, r1, c1, r2, c2, on) in merges {
                                        let _ = tx.send(IntroFrame::Line(
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
                                        ));
                                    }
                                    let meta_items = meta_log.lock().unwrap().clone();
                                    if !meta_items.is_empty() {
                                        let _ = tx.send(IntroFrame::Line(
                                            serde_json::to_string(&Msg::SheetMetaSnapshot {
                                                items: meta_items,
                                            })
                                            .unwrap(),
                                        ));
                                    }
                                    if !stream_snapshot_ops(
                                        base_arc.as_deref(),
                                        &ops_grid,
                                        use_compact_v3,
                                        &tx,
                                    ) {
                                        break;
                                    }
                                    let end_ok = if use_compact_v3 {
                                        tx.send(IntroFrame::Compact(
                                            sheetcore::sync::encode_snapshot_ref_compact(&[]),
                                        ))
                                        .is_ok()
                                    } else {
                                        tx.send(IntroFrame::Line(
                                            serde_json::to_string(&Msg::Snapshot { ops: Vec::new() })
                                                .unwrap(),
                                        ))
                                        .is_ok()
                                    };
                                    if !end_ok {
                                        break;
                                    }
                                }
                                {
                                    // 下发当前所有未过期的单元格编辑租约，确保后加入成员立即看到谁锁了哪格并禁用它
                                    for (sheet, row, col, lsite, lname) in active_locks {
                                        let _ = tx.send(IntroFrame::Line(
                                            serde_json::to_string(&Msg::LockOk {
                                                site: lsite,
                                                name: lname,
                                                sheet,
                                                row,
                                                col,
                                            })
                                            .unwrap(),
                                        ));
                                    }
                                }
                                {
                                    // 下发所有在线成员的实时选区光标，后加入者无需等待对方移动鼠标即可看见同事 ID 标签
                                    for cur in active_cursors {
                                        if cur.site != site {
                                            if use_compact_v3 {
                                                let _ = tx.send(IntroFrame::Compact(
                                                    sheetcore::sync::encode_msg_compact(&Msg::CursorMsg(cur)),
                                                ));
                                            } else {
                                                let _ = tx.send(IntroFrame::Line(
                                                    serde_json::to_string(&Msg::CursorMsg(cur)).unwrap(),
                                                ));
                                            }
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
                                    sort_permutation: false,
                                    palette_styles: false,
                                    resume_room: String::new(),
                                    resume_watermark: 0,
                                    resume_struct_rev: 0,
                                    binary_v3: false,
                                    compact_v3: false,
                                })
                                .unwrap();
                                broadcast_line_to(&other_peers, hello);
                            }
                            Msg::SortReply { first_ts, outcome } if can_sort.load(Ordering::Acquire) => {
                                let _ = ack_tx.send((first_ts,outcome));
                            }
                            Msg::SortRepairDone { first_ts } if can_sort.load(Ordering::Acquire) => {
                                let _order = room_order.lock().unwrap();
                                let _ = cmd.send(Cmd::Deliver(vec![tx.clone()],RoomChange::Message(Msg::SortRepairDone { first_ts })));
                            }
                            Msg::SortData { first_ts, items } if can_sort.load(Ordering::Acquire) => {
                                let Some(batch) = fallback.as_mut() else { break };
                                let plan = &batch.plan;
                                let (r1,c1,r2,c2) = plan.region;
                                if first_ts != plan.first_ts { break; }
                                if !items.is_empty() {
                                    let area = u64::from(r2-r1+1) * u64::from(c2-c1+1);
                                    if (batch.ops.len()+items.len()) as u64 > area { break; }
                                    if items.iter().enumerate().any(|(i,(op,_))| op.site!=plan.site || op.sheet!=plan.sheet
                                        || !(r1..=r2).contains(&op.row) || !(c1..=c2).contains(&op.col)
                                        || op.ts != first_ts+(batch.ops.len()+i) as u64) { break; }
                                    for (op,style) in items {
                                        if let Some(style) = style { batch.styles.push((op.row,op.col,style)); }
                                        batch.ops.push(Arc::new(op));
                                    }
                                    continue;
                                }
                                let batch = fallback.take().unwrap();
                                if batch.offer().digest != batch.plan.digest { break; }
                                let batch = Arc::new(batch);
                                for attempt in 0..3 {
                                    let axis_snap = host_watermark.lock().unwrap().axis_styles_snapshot();
                                    let current = RoomGrids::capture(&host_baseline,axis_snap,&state,&delta_ops,&styles,&delta_styles);
                                    let next = current.apply(&batch);
                                    let _order = room_order.lock().unwrap();
                                    let history = structs.lock().unwrap();
                                    let later = history.get(batch.plan.structural_revision..);
                                    let leases = locks.lock().unwrap();
                                    let sheet = batch.plan.sheet;
                                    let allowed = later.is_some_and(|edits| edits.iter().all(|(s,_)| *s!=sheet))
                                        && leases.conflicts_rect(sheet,r1,c1,r2,c2,batch.plan.site,now_ms()).is_none();
                                    if allowed && current.publish(&next,&state,&delta_ops,&styles,&delta_styles) {
                                        commit_sort(batch.clone(), history.len());
                                        break;
                                    }
                                    if !allowed || attempt == 2 {
                                        let axis_now = host_watermark.lock().unwrap().axis_styles_snapshot();
                                        let correction = SortCorrection::capture(batch.clone(),&host_baseline,axis_now,&state,&styles,later.unwrap_or_default());
                                        let _ = cmd.send(Cmd::Deliver(vec![tx.clone()],RoomChange::Correction(correction)));
                                        break;
                                    }
                                }
                            }
                            Msg::SortRepairRequest { sheet, first_ts, structural_revision, keys } if can_sort.load(Ordering::Acquire) => {
                                let _order = room_order.lock().unwrap();
                                let history = structs.lock().unwrap();
                                let edits = history.get(structural_revision..).unwrap_or_default();
                                if structural_revision > history.len() {
                                    let msg = Msg::StructureRevision { revision: history.len() };
                                    let _ = cmd.send(Cmd::Deliver(vec![tx.clone()],RoomChange::Message(msg)));
                                }
                                let base_arc = host_baseline.lock().unwrap().clone();
                                let axis_snap = host_watermark.lock().unwrap().axis_styles_snapshot();
                                let items = sort_repairs(base_arc.as_deref(),&axis_snap,&state.lock().unwrap(),&styles.lock().unwrap(),sheet,first_ts,keys,edits);
                                let _ = cmd.send(Cmd::Deliver(vec![tx.clone()],RoomChange::Message(Msg::SortRepairs {sheet,first_ts,items})));
                            }
                            Msg::SortPermutation(plan) if can_sort.load(Ordering::Acquire) => {
                                let Some(site) = hello_site else { break };
                                if plan.site != site { break; }
                                let (r1,c1,r2,c2) = plan.region;
                                let axis_snap = host_watermark.lock().unwrap().axis_styles_snapshot();
                                let current = RoomGrids::capture(&host_baseline,axis_snap,&state,&delta_ops,&styles,&delta_styles);
                                let same_frame = structs.lock().unwrap().get(plan.structural_revision..)
                                    .is_some_and(|later| later.iter().all(|(s,_)| *s != plan.sheet));
                                let permitted = same_frame && locks.lock().unwrap()
                                    .conflicts_rect(plan.sheet,r1,c1,r2,c2,site,now_ms()).is_none();
                                let candidate = permitted.then(|| current.expand(&plan)).flatten().map(Arc::new);
                                let next = candidate.as_ref().map(|batch| current.apply(batch));
                                #[cfg(test)]
                                if let Some(hook) = before_sort_commit.lock().unwrap().take() { hook(); }
                                let order = room_order.lock().unwrap();
                                let history = structs.lock().unwrap();
                                let leases = locks.lock().unwrap();
                                let allowed = history.get(plan.structural_revision..)
                                    .is_some_and(|later| !later.iter().any(|(s,_)| *s==plan.sheet))
                                    && leases.conflicts_rect(plan.sheet,r1,c1,r2,c2,site,now_ms()).is_none();
                                let outcome = if !allowed {
                                    if plan.structural_revision > history.len() {
                                        let msg = Msg::StructureRevision { revision: history.len() };
                                        let _ = cmd.send(Cmd::Deliver(vec![tx.clone()],RoomChange::Message(msg)));
                                    }
                                    SortResponse::Locked { structural_revision: plan.structural_revision.min(history.len()) }
                                } else if let (Some(batch),Some(next)) = (&candidate,&next)
                                    && current.publish(next,&state,&delta_ops,&styles,&delta_styles) {
                                    commit_sort(batch.clone(), history.len());
                                    SortResponse::Accepted
                                } else {
                                    fallback = Some(SortBatch::new(plan.clone(),Vec::new(),Vec::new()));
                                    SortResponse::NeedCells
                                };
                                drop(leases);
                                drop(history);
                                drop(order);
                                #[cfg(test)]
                                if let Some(hook) = before_sort_reply.lock().unwrap().take() { hook(); }
                                let reply = serde_json::to_string(&Msg::SortReply { first_ts: plan.first_ts, outcome }).unwrap();
                                let mut out = output.lock().unwrap();
                                if out.write_plain(&reply).is_err() { break; }
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
                                if !lt.is_empty() {
                                    let now = now_ms();
                                    ops.retain(|op| {
                                        let Some(h) = lt.holder_other(op.sheet, op.row, op.col, conn_site, now) else { return true };
                                        let prev = latest_cell_op(Some(&host_baseline), &state, op.sheet, op.row, op.col);
                                        let deny = Msg::WriteDeny {
                                            holder: h.site, holder_name: h.name.clone(),
                                            sheet: op.sheet, row: op.row, col: op.col,
                                            revert_ops: vec![(op.row, op.col, prev)],
                                            revert_styles: Vec::new(), revert_merge: None,
                                        };
                                        let _ = tx.send(serde_json::to_string(&deny).unwrap());
                                        false
                                    });
                                }
                                drop(lt);
                                if ops.is_empty() { continue; }
                                let shared: Vec<Arc<Op>> = ops.iter().cloned().map(Arc::new).collect();
                                update_host_shared_ops(&state, &delta_ops, &formula_keys, &host_max_clock, shared.iter().cloned());
                                let seq = host_watermark.lock().unwrap().record_ops(shared.iter().map(Arc::as_ref));
                                let struct_rev = structs.lock().unwrap().len();
                                queue_room_change(&cmd, &roster, Some(conn_site), RoomChange::SharedCells(shared));
                                emit_room_watermark(&cmd, &roster, seq, struct_rev);
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
                                let others = roster.lock().unwrap().other_senders(Some(conn_site));
                                broadcast_msg_to(&others, &Msg::CursorMsg(c.clone()));
                                let _ = evt_tx.send_cursor(NetEvent::Cursor(c));
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
                            request @ (Msg::Structural { .. } | Msg::StructuralRequest { .. }) => {
                                let (site,sheet,op,revision) = match request {
                                    Msg::Structural { site,sheet,op } => (site,sheet,op,None),
                                    Msg::StructuralRequest { site,sheet,op,revision } => (site,sheet,op,Some(revision)),
                                    _ => unreachable!(),
                                };
                                let Some(conn_site) = hello_site else { break };
                                if site != conn_site {
                                    continue;
                                }
                                let _order = room_order.lock().unwrap();
                                if revision.is_some_and(|rev| {
                                    structs.lock().unwrap().get(rev..)
                                        .is_none_or(|later| later.iter().any(|(s,_)| *s==sheet))
                                }) {
                                    let revision = structs.lock().unwrap().len();
                                    let _ = cmd.send(Cmd::Deliver(vec![tx.clone()],RoomChange::Message(Msg::StructureRevision { revision })));
                                    let msg = Msg::StructuralRejected { reason: "工作表结构已变更，请重新执行插删操作".into() };
                                    let _ = cmd.send(Cmd::Deliver(vec![tx.clone()],RoomChange::Message(msg)));
                                    continue;
                                }
                                expire_and_broadcast();
                                let conflict = locks
                                    .lock()
                                    .unwrap()
                                    .conflicts_structural(sheet, &op, conn_site, now_ms())
                                    .map(|((r, c), h)| (h.site, h.name.clone(), r, c));
                                if let Some((holder, holder_name, row, col)) = conflict {
                                    if revision.is_some() {
                                        let reason = format!("{holder_name} 正在编辑 {}，宿主未执行{}",sheetcore::model::cell_name(row,col),op.label());
                                        let _ = cmd.send(Cmd::Deliver(vec![tx.clone()],RoomChange::Message(Msg::StructuralRejected { reason })));
                                        continue;
                                    }
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
                                shift_state(&host_baseline, &formula_keys, &state, &delta_ops, &host_base_hash, &styles, &delta_styles, &locks, &cursors, &sheet_names.lock().unwrap(), sheet, &op);
                                let struct_rev = {
                                    let mut h = structs.lock().unwrap();
                                    h.push((sheet, op.clone()));
                                    h.len()
                                };
                                let seq = host_watermark.lock().unwrap().shift_structural(sheet, &op);
                                queue_room_change(&cmd, &roster, revision.is_none().then_some(conn_site), RoomChange::Message(Msg::Structural {
                                    site: conn_site,
                                    sheet,
                                    op: op.clone(),
                                }));
                                emit_room_watermark(&cmd, &roster, seq, struct_rev);
                                let _ = evt_tx.send(NetEvent::Structural {
                                    site: conn_site,
                                    sheet,
                                    op,
                                });
                            }
                            Msg::StylePaletteOp { site, sheet, ts, palette, items } => {
                                let Some(conn_site) = hello_site else { break };
                                if site != conn_site {
                                    continue;
                                }
                                let _order = room_order.lock().unwrap();
                                expire_and_broadcast();
                                let conflict = {
                                    let now = now_ms();
                                    let lt = locks.lock().unwrap();
                                    if lt.is_empty()
                                        || (items.len() > lt.len().saturating_mul(4)
                                            && !lt.iter().any(|((sh, _, _), h)| {
                                                sh == sheet
                                                    && h.site != conn_site
                                                    && now.saturating_sub(h.at_ms) < lt.ttl_ms
                                            }))
                                    {
                                        None
                                    } else {
                                        items.iter().find_map(|(r, c, _)| {
                                            lt.holder_other(sheet, *r, *c, conn_site, now)
                                                .map(|h| (h.site, h.name.clone(), *r, *c))
                                        })
                                    }
                                };
                                if let Some((holder, holder_name, row, col)) = conflict {
                                    let axis_snap = host_watermark.lock().unwrap().axis_styles_snapshot();
                                    let revert_styles: Vec<_> = items
                                        .iter()
                                        .map(|(r, c, _)| {
                                            (
                                                *r,
                                                *c,
                                                latest_cell_style(
                                                    Some(&host_baseline),
                                                    Some(&axis_snap),
                                                    &styles,
                                                    sheet,
                                                    *r,
                                                    *c,
                                                ),
                                            )
                                        })
                                        .collect();
                                    let deny = Msg::WriteDeny {
                                        holder, holder_name, sheet, row, col,
                                        revert_ops: Vec::new(), revert_styles, revert_merge: None,
                                    };
                                    if !send_room_message(deny, MAX_PLAIN_LINE, |line| tx.send(line).is_ok(), || {}) { break; }
                                    continue;
                                }
                                // 直接按调色板写入宿主共享样式并原样透传 `StylePaletteOp`，彻底消除 decode -> re-encode 与逐格哈希！
                                update_host_palette_styles(&styles, &delta_styles, sheet, &palette, &items);
                                let seq = host_watermark
                                    .lock()
                                    .unwrap()
                                    .record_style_coords(sheet, items.iter().map(|&(r, c, _)| (r, c)));
                                let struct_rev = structs.lock().unwrap().len();
                                let exclude = (!can_sort.load(Ordering::Acquire)).then_some(conn_site);
                                let ev: Vec<(usize, u32, u32, sheetcore::style::CellStyle)> = items
                                    .iter()
                                    .filter_map(|&(r, c, idx)| {
                                        palette
                                            .get(idx as usize)
                                            .map(|st| (sheet, r, c, st.clone()))
                                    })
                                    .collect();
                                let fwd_msg = Msg::StylePaletteOp {
                                    site: conn_site,
                                    sheet,
                                    ts,
                                    palette,
                                    items,
                                };
                                queue_room_change(&cmd, &roster, exclude, RoomChange::Message(fwd_msg));
                                emit_room_watermark(&cmd, &roster, seq, struct_rev);
                                let _ = evt_tx.send(NetEvent::Styles {
                                    site: conn_site,
                                    items: ev,
                                    snapshot: false,
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
                                    if lt.is_empty()
                                        || (items.len() > lt.len().saturating_mul(4)
                                            && !lt.iter().any(|((sh, _, _), h)| {
                                                sh == sheet
                                                    && h.site != conn_site
                                                    && now.saturating_sub(h.at_ms) < lt.ttl_ms
                                            }))
                                    {
                                        None
                                    } else {
                                        items.iter().find_map(|(r, c, _)| {
                                            lt.holder_other(sheet, *r, *c, conn_site, now)
                                                .map(|h| (h.site, h.name.clone(), *r, *c))
                                        })
                                    }
                                };
                                if let Some((holder, holder_name, row, col)) = conflict {
                                    let axis_snap = host_watermark.lock().unwrap().axis_styles_snapshot();
                                    let revert_styles: Vec<_> = items
                                        .iter()
                                        .map(|(r, c, _)| {
                                            (
                                                *r,
                                                *c,
                                                latest_cell_style(
                                                    Some(&host_baseline),
                                                    Some(&axis_snap),
                                                    &styles,
                                                    sheet,
                                                    *r,
                                                    *c,
                                                ),
                                            )
                                        })
                                        .collect();
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
                                let seq = host_watermark.lock().unwrap().record_styles(sheet, &items);
                                let struct_rev = structs.lock().unwrap().len();
                                let exclude = (!can_sort.load(Ordering::Acquire)).then_some(conn_site);
                                let fwd_msg = if items.len() > 1 {
                                    let (palette, encoded) = sheetcore::sync::encode_style_palette(&items);
                                    Msg::StylePaletteOp {
                                        site: conn_site,
                                        sheet,
                                        ts,
                                        palette,
                                        items: encoded,
                                    }
                                } else {
                                    Msg::StyleOp {
                                        site: conn_site,
                                        sheet,
                                        ts,
                                        items: items.clone(),
                                    }
                                };
                                queue_room_change(&cmd, &roster, exclude, RoomChange::Message(fwd_msg));
                                emit_room_watermark(&cmd, &roster, seq, struct_rev);
                                let ev: Vec<(usize, u32, u32, sheetcore::style::CellStyle)> =
                                    items.into_iter().map(|(r, c, s)| (sheet, r, c, s)).collect();
                                let _ = evt_tx.send(NetEvent::Styles {
                                    site: conn_site,
                                    items: ev,
                                    snapshot: false,
                                });
                            }
                            Msg::AxisStyleOp { site, sheet, ts, patch } => {
                                let Some(conn_site) = hello_site else { break };
                                if site != conn_site || patch.is_empty() {
                                    continue;
                                }
                                let _order = room_order.lock().unwrap();
                                expire_and_broadcast();
                                let conflict = {
                                    let now = now_ms();
                                    let lt = locks.lock().unwrap();
                                    lt.iter().find_map(|((sh, r, c), h)| {
                                        if sh != sheet
                                            || h.site == conn_site
                                            || now.saturating_sub(h.at_ms) >= lt.ttl_ms
                                        {
                                            return None;
                                        }
                                        let hit_row = patch.rows.iter().any(|(pr, _)| *pr == r);
                                        let hit_col = patch.cols.iter().any(|(pc, _)| *pc == c);
                                        (hit_row || hit_col).then(|| (h.site, h.name.clone(), r, c))
                                    })
                                };
                                if let Some((holder, holder_name, row, col)) = conflict {
                                    let deny = serde_json::to_string(&Msg::WriteDeny {
                                        holder,
                                        holder_name,
                                        sheet,
                                        row,
                                        col,
                                        revert_ops: Vec::new(),
                                        revert_styles: Vec::new(),
                                        revert_merge: None,
                                    })
                                    .unwrap();
                                    let _ = tx.send(deny);
                                    continue;
                                }
                                let seq = host_watermark.lock().unwrap().record_axis(sheet, &patch);
                                let struct_rev = structs.lock().unwrap().len();
                                let exclude = (!can_sort.load(Ordering::Acquire)).then_some(conn_site);
                                queue_room_change(
                                    &cmd,
                                    &roster,
                                    exclude,
                                    RoomChange::Message(Msg::AxisStyleOp {
                                        site: conn_site,
                                        sheet,
                                        ts,
                                        patch: patch.clone(),
                                    }),
                                );
                                emit_room_watermark(&cmd, &roster, seq, struct_rev);
                                let _ = evt_tx.send(NetEvent::AxisStyles {
                                    site: conn_site,
                                    sheet,
                                    patch,
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
                                let seq = host_watermark
                                    .lock()
                                    .unwrap()
                                    .record_merge(sheet, (r1, c1, r2, c2), on);
                                let struct_rev = structs.lock().unwrap().len();
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
                                emit_room_watermark(&cmd, &roster, seq, struct_rev);
                                let _ = evt_tx.send(NetEvent::Merge {
                                    site: conn_site,
                                    sheet,
                                    rect: (r1, c1, r2, c2),
                                    on,
                                });
                            }
                            Msg::SheetMetaOp { site, sheet, ts, kind } => {
                                let Some(conn_site) = hello_site else { break };
                                if site != conn_site || !kind.valid(sheet) {
                                    continue;
                                }
                                let _order = room_order.lock().unwrap();
                                {
                                    let mut g = meta_log.lock().unwrap();
                                    if g.len() >= 10_000 {
                                        g.remove(0);
                                    }
                                    g.push((sheet, kind.clone()));
                                }
                                let seq = host_watermark
                                    .lock()
                                    .unwrap()
                                    .record_sheet_meta(sheet, kind.clone());
                                let struct_rev = structs.lock().unwrap().len();
                                queue_room_change(
                                    &cmd,
                                    &roster,
                                    Some(conn_site),
                                    RoomChange::Message(Msg::SheetMetaOp {
                                        site: conn_site,
                                        sheet,
                                        ts,
                                        kind: kind.clone(),
                                    }),
                                );
                                emit_room_watermark(&cmd, &roster, seq, struct_rev);
                                let _ = evt_tx.send(NetEvent::SheetMeta {
                                    site: conn_site,
                                    sheet,
                                    kind,
                                    snapshot: false,
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
                                    let use_compact_bin = peer_tx.compact_v3.clone();
                                    seeder_queue.enqueue(UpdateSeedJob {
                                        my_site: host_site_id,
                                        to_site: conn_site,
                                        req_version: version,
                                        seed_override: seed_override.clone(),
                                        alive: alive_conn.clone(),
                                        use_compact_bin,
                                        send_slice: Box::new(move |payload| {
                                            peer_tx.send_update_slice(payload).is_ok()
                                        }),
                                    });
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
                                raw_data,
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
                                        &raw_data,
                                        done,
                                    );
                                } else if let Some(target_tx) =
                                    roster.lock().unwrap().sender_of(to_site)
                                {
                                    let target_compact = target_tx.is_compact_v3();
                                    if !target_compact
                                        && raw_data.is_empty()
                                        && let Some(ref raw_line) = raw_json_line
                                    {
                                        // 旧版 JSON 客户端到旧版 JSON 客户端：原样透传免重编码
                                        let _ = target_tx.send_update_slice(
                                            sheetcore::wire::prepare_payload(raw_line, false),
                                        );
                                    } else {
                                        let decoded_tmp;
                                        let slice_bytes: &[u8] = if !raw_data.is_empty()
                                            || data_b64.is_empty()
                                        {
                                            &raw_data
                                        } else if let Some(b) = sheetcore::wire::b64_decode(&data_b64) {
                                            decoded_tmp = b;
                                            &decoded_tmp
                                        } else {
                                            continue;
                                        };
                                        let payload = make_update_slice_payload(
                                            conn_site,
                                            to_site,
                                            &version,
                                            offset,
                                            total,
                                            slice_bytes,
                                            done,
                                            target_compact,
                                        );
                                        let _ = target_tx.send_update_slice(payload);
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
                    drop(ack_tx); // Wake this socket's writer even if its sort never got a reply.
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
        host_baseline: Some(host_baseline_for_self),
        formula_keys: Some(formula_keys_for_self),
        host_watermark: Some(host_watermark_for_self),
        host_max_clock: Some(host_max_clock_for_self),
        host_wake_addr: Some(host_wake_addr),
        state: Some(state),
        delta_ops: Some(delta_ops_for_self),
        host_base_hash: Some(host_base_hash_for_self),
        host_roster: Some(roster_for_self),
        room_order: Some(room_order),
        #[cfg(test)]
        after_snapshot,

        #[cfg(test)]
        before_sort_reply,
        #[cfg(test)]
        before_sort_commit,
        host_locks: Some(locks_for_self),
        host_cursors: Some(cursors_for_self),
        host_styles: Some(styles_for_self),
        delta_styles: Some(delta_styles_for_self),
        delta_merges: Some(delta_merges_for_self),
        host_meta_log: Some(meta_log_for_self),
        pending_structure: Default::default(),
        #[cfg(test)]
        unconfirmed_styles: Default::default(),
        last_watermark_seq: Default::default(),
        min_saved_watermark: Default::default(),
        structural_ack: Default::default(),
        palette_styles_cap: Arc::new(AtomicBool::new(true)),
        connection_generation: Default::default(),
        sheet_names,
        structure_seen: Default::default(),
        pending_sorts: Default::default(),
        local_sort_ranges: Default::default(),
        host_structs: Some(structs_for_self),
        host_file_path: Some(host_file_path),
        staged_save_cursor: Mutex::new(None),
        peer_update_dl: host_update_dl_for_self,
        update_seed_override: host_seed_override_for_self,
        seeder_queue: host_seeder_for_self,
        cursor_throttle: CursorSendThrottle::default(),
        fast_snapshot_handoff: Arc::new(AtomicBool::new(false)),
        seed_styles: Arc::new(Mutex::new(None)),
        ready_snapshot: Arc::new(Mutex::new(None)),
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

const SNAPSHOT_EVENT_COALESCE: usize = 32_768;

struct PreparedGuestSnapshotBuilder {
    sheets: Vec<sheetcore::Sheet>,
    styles: sheetcore::style::StyleTable,
    has_seed_styles: bool,
    merge_ops: Vec<(usize, u32, u32, u32, u32, bool)>,
    overlay_ops: Vec<Op>,
    pal_ids: Vec<u16>,
    max_ts: u64,
    host_site: u32,
}

impl PreparedGuestSnapshotBuilder {
    fn new(seed_styles: Option<sheetcore::style::StyleTable>) -> Self {
        let has_seed_styles = seed_styles.is_some();
        Self {
            sheets: Vec::new(),
            styles: seed_styles.unwrap_or_else(sheetcore::style::StyleTable::new),
            has_seed_styles,
            merge_ops: Vec::new(),
            overlay_ops: Vec::new(),
            pal_ids: Vec::new(),
            max_ts: 1,
            host_site: 1,
        }
    }

    fn ensure_sheet(&mut self, si: usize) -> &mut sheetcore::Sheet {
        while self.sheets.len() <= si {
            let idx = self.sheets.len() + 1;
            let mut sh = sheetcore::Sheet::new(format!("Sheet{idx}"));
            sh.loading = true;
            self.sheets.push(sh);
        }
        &mut self.sheets[si]
    }

    fn apply_axis_snapshot(&mut self, sheet: usize, patch: AxisStylePatch) {
        let mut row_ids = Vec::with_capacity(patch.rows.len());
        for (r, st_opt) in patch.rows {
            let id = st_opt.map_or(0, |st| self.styles.intern(st));
            row_ids.push((r, id));
        }
        let mut col_ids = Vec::with_capacity(patch.cols.len());
        for (c, st_opt) in patch.cols {
            let id = st_opt.map_or(0, |st| self.styles.intern(st));
            col_ids.push((c, id));
        }
        let sh = self.ensure_sheet(sheet);
        for (r, id) in row_ids {
            sh.set_row_style_loaded(r, id);
        }
        for (c, id) in col_ids {
            sh.set_col_style_loaded(c, id);
        }
    }

    fn apply_style_palette_snapshot(
        &mut self,
        palette: &[sheetcore::style::CellStyle],
        items: &[(usize, u32, u32, u16)],
    ) {
        self.pal_ids.clear();
        self.pal_ids.reserve(palette.len());
        for st in palette {
            self.pal_ids.push(self.styles.intern(st.clone()));
        }
        if let Some(&max_si) = items.iter().map(|(si, ..)| si).max() {
            let _ = self.ensure_sheet(max_si);
        }
        for &(si, r, c, idx) in items {
            if let Some(&sid) = self.pal_ids.get(idx as usize) {
                self.sheets[si].set_style_loaded(r, c, sid);
            }
        }
    }

    fn apply_style_snapshot(
        &mut self,
        items: Vec<(usize, u32, u32, sheetcore::style::CellStyle)>,
    ) {
        if let Some(&max_si) = items.iter().map(|(si, ..)| si).max() {
            let _ = self.ensure_sheet(max_si);
        }
        for (si, r, c, st) in items {
            let sid = self.styles.intern(st);
            self.sheets[si].set_style_loaded(r, c, sid);
        }
    }

    fn apply_merge(&mut self, sheet: usize, r1: u32, c1: u32, r2: u32, c2: u32, on: bool) {
        self.merge_ops.push((sheet, r1, c1, r2, c2, on));
    }

    fn apply_ops(&mut self, ops: Vec<Op>) {
        if let Some(&max_si) = ops.iter().map(|o| &o.sheet).max() {
            let _ = self.ensure_sheet(max_si);
        }
        for op in ops {
            self.max_ts = self.max_ts.max(op.ts);
            if op.site != 0 {
                self.host_site = op.site;
            }
            let cell = sheetcore::model::Cell {
                value: sheetcore::model::Value::from(&op.value),
                formula: op.formula.clone(),
            };
            self.sheets[op.sheet].set(op.row, op.col, cell);
            if op.ts > 1 {
                self.overlay_ops.push(op);
            }
        }
    }

    fn finish(mut self) -> PreparedGuestSnapshot {
        for sh in &mut self.sheets {
            sh.loading = false;
        }
        PreparedGuestSnapshot {
            sheets: self.sheets,
            styles: self.styles,
            has_seed_styles: self.has_seed_styles,
            merge_ops: self.merge_ops,
            overlay_ops: self.overlay_ops,
            max_ts: self.max_ts,
            host_site: self.host_site,
        }
    }
}

fn send_guest_shared_ops<W: Write>(gs: &mut GuestSend, w: &mut W, ops: &[Arc<Op>]) {
    if ops.is_empty() {
        return;
    }
    if gs.binary_v3 && gs.compact_v3 && gs.batch_ops {
        use sheetcore::sync::ValueWire;
        let mut start = 0usize;
        let mut bytes = 0usize;
        for (idx, op) in ops.iter().enumerate() {
            let text_len = match &op.value {
                ValueWire::Text(s) | ValueWire::Error(s) => s.len(),
                _ => 0,
            };
            let size = text_len + op.formula.as_deref().map_or(0, str::len) + 96;
            if idx > start
                && (idx - start >= sheetcore::sync::LIVE_OP_BATCH || bytes + size > 256 * 1024)
            {
                let raw = sheetcore::sync::encode_arc_ops_compact(&ops[start..idx]);
                gs.sess.seal_compact_binary_into(&raw, true, &mut gs.scratch);
                let _ = write_binary_frame_opt(w, &gs.scratch, false);
                start = idx;
                bytes = 0;
            }
            bytes += size;
        }
        if start < ops.len() {
            let raw = sheetcore::sync::encode_arc_ops_compact(&ops[start..]);
            gs.sess.seal_compact_binary_into(&raw, true, &mut gs.scratch);
            let _ = write_binary_frame_opt(w, &gs.scratch, true);
            if gs.scratch.capacity() > 512 * 1024 {
                gs.scratch = Vec::new();
            }
        } else {
            let _ = w.flush();
        }
    } else {
        let batch = gs.batch_ops;
        cell_frames_msg(ops.iter().map(|o| (**o).clone()), true, batch, |msg| {
            let _ = gs.write_msg_opt(w, &msg, false);
        });
        let _ = w.flush();
    }
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
    let _ = crate::update::try_cached_local_signed_bundle();
    let sockets = Arc::new(Mutex::new(Vec::new()));

    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (evt_raw_tx, evt_rx) = channel::<NetEvent>();
    let (evt_tx, wake_ctx) = EvtTx::new(evt_raw_tx);
    let evt_tx_clone = evt_tx.clone();
    let alive = Arc::new(AtomicBool::new(true));
    let connected_flag = Arc::new(AtomicBool::new(false));
    let connected_for_net = connected_flag.clone();
    let structural_ack = Arc::new(AtomicBool::new(false));
    let ack_for_reader = structural_ack.clone();
    let palette_styles_cap = Arc::new(AtomicBool::new(false));
    let palette_for_reader = palette_styles_cap.clone();
    let connection_generation = Arc::new(AtomicU64::new(0));
    let writer_generation = connection_generation.clone();
    let reader_generation = connection_generation.clone();
    let writer: Arc<Mutex<Option<BufWriter<TcpStream>>>> = Arc::new(Mutex::new(None));
    // 发送端加密会话单独归写线程持有，读线程使用栈上独立解密会话（彻底消灭大快照解密期间读写锁争用！）
    let send_sess: Arc<Mutex<Option<GuestSend>>> = Arc::new(Mutex::new(None));
    // 本客户端实例专属重连凭据：断线重连可继续沿用自身 site，其他客户端不能冒领该 site
    let client_auth = crate::session_lock::new_session_id();
    let client_update_dl: Arc<Mutex<Option<ActivePeerDownload>>> = Arc::new(Mutex::new(None));
    let client_update_dl_for_net = client_update_dl.clone();
    let client_seed_override: SharedUpdateSeed = Arc::new(Mutex::new(None));
    let client_seed_override_for_net = client_seed_override.clone();
    let client_seeder_queue = UpdateSeederQueue::default();
    let client_seeder_for_net = client_seeder_queue.clone();
    let host_compact_flag = Arc::new(AtomicBool::new(false));
    let guest_low_prio_update: LowPrioUpdateQueue = Arc::new((
        Mutex::new((
            std::collections::VecDeque::with_capacity(LOW_PRIO_UPDATE_CAP),
            false,
        )),
        std::sync::Condvar::new(),
    ));
    let guest_low_prio_for_writer = guest_low_prio_update.clone();
    let guest_low_prio_for_reader = guest_low_prio_update.clone();
    let fast_snapshot_handoff = Arc::new(AtomicBool::new(false));
    let fast_handoff_for_reader = fast_snapshot_handoff.clone();
    let seed_styles: Arc<Mutex<Option<sheetcore::style::StyleTable>>> = Arc::new(Mutex::new(None));
    let seed_styles_for_reader = seed_styles.clone();
    let ready_snapshot: Arc<Mutex<Option<PreparedGuestSnapshot>>> = Arc::new(Mutex::new(None));
    let ready_snapshot_for_reader = ready_snapshot.clone();
    let cmd_tx_for_stream = cmd_tx.clone();

    let pending_sorts: Arc<Mutex<std::collections::BTreeMap<u64,Arc<SortBatch>>>> = Default::default();
    let pending_for_net = pending_sorts.clone();
    let pending_for_writer = pending_sorts.clone();
    let pending_structure = Arc::new(AtomicBool::new(false));
    let pending_structure_for_reader = pending_structure.clone();
    let unconfirmed_styles = Arc::new(AtomicU64::new(0));
    let unconfirmed_styles_for_writer = unconfirmed_styles.clone();
    let unconfirmed_styles_for_reader = unconfirmed_styles.clone();
    let last_watermark_seq = Arc::new(AtomicU64::new(0));
    let last_wm_for_reader = last_watermark_seq.clone();
    let min_saved_watermark = Arc::new(AtomicU64::new(0));
    let min_saved_wm_for_reader = min_saved_watermark.clone();
    // 写线程
    {
        let writer = writer.clone();
        let alive = alive.clone();
        let dispatch_events = evt_tx.clone();
        let send_sess = send_sess.clone();
        std::thread::spawn(move || {
            let mut next_cmd = cmd_rx.recv().ok();
            while let Some(first) = next_cmd.take() {
                if !alive.load(Ordering::SeqCst) {
                    break;
                }
                let mut cur = Some(first);
                let mut stop_now = false;
                while let Some(cmd) = cur.take() {
                    let peek = cmd_rx.try_recv().ok();
                    let flush_now = peek.is_none()
                        && guest_low_prio_for_writer.0.lock().unwrap().0.is_empty();
                    match cmd {
                        Cmd::Cells(ops) => {
                            let mut session = send_sess.lock().unwrap();
                            if let (Some(gs), Some(w)) = (session.as_mut(), writer.lock().unwrap().as_mut()) {
                                let batch = gs.batch_ops;
                                cell_frames_msg(ops, true, batch, |msg| {
                                    let _ = gs.write_msg_opt(w, &msg, false);
                                });
                                if flush_now {
                                    let _ = w.flush();
                                }
                            }
                        }
                        Cmd::SharedCells(ops) => {
                            let mut session = send_sess.lock().unwrap();
                            if let (Some(gs), Some(w)) = (session.as_mut(), writer.lock().unwrap().as_mut()) {
                                send_guest_shared_ops(gs, w, &ops);
                            }
                        }
                        Cmd::BroadcastMsg(msg) => {
                            let mut session = send_sess.lock().unwrap();
                            if let (Some(gs), Some(w)) = (session.as_mut(), writer.lock().unwrap().as_mut()) {
                                let _ = gs.write_msg_opt(w, &msg, flush_now);
                            }
                        }
                        Cmd::Style(msg) => {
                            let mut session = send_sess.lock().unwrap();
                            if let (Some(gs), Some(w)) = (session.as_mut(), writer.lock().unwrap().as_mut()) {
                                let mut handled_compact = false;
                                if gs.binary_v3
                                    && gs.compact_v3
                                    && matches!(
                                        &msg,
                                        Msg::StylePaletteOp { items, .. }
                                            if items.len() <= sheetcore::sync::MAX_BATCH_ITEMS
                                    )
                                {
                                    let raw = sheetcore::sync::encode_msg_compact(&msg);
                                    if raw.len() <= MAX_PLAIN_LINE {
                                        unconfirmed_styles_for_writer.fetch_add(1, Ordering::AcqRel);
                                        gs.sess.seal_compact_binary_into(&raw, true, &mut gs.scratch);
                                        if write_binary_frame_opt(w, &gs.scratch, flush_now).is_err() {
                                            let _ = unconfirmed_styles_for_writer.fetch_update(
                                                Ordering::AcqRel,
                                                Ordering::Acquire,
                                                |v| Some(v.saturating_sub(1)),
                                            );
                                            let _ = dispatch_events.send(NetEvent::Error("协作格式未能完整发送".into()));
                                        }
                                        if gs.scratch.capacity() > 512 * 1024 {
                                            gs.scratch = Vec::new();
                                        }
                                        handled_compact = true;
                                    }
                                }
                                if !handled_compact {
                                    if !send_room_message(msg, MAX_PLAIN_LINE, |line| {
                                        unconfirmed_styles_for_writer.fetch_add(1, Ordering::AcqRel);
                                        if gs.write_plain_opt(w, &line, flush_now).is_ok() {
                                            true
                                        } else {
                                            let _ = unconfirmed_styles_for_writer.fetch_update(
                                                Ordering::AcqRel,
                                                Ordering::Acquire,
                                                |v| Some(v.saturating_sub(1)),
                                            );
                                            false
                                        }
                                    }, || {
                                        let _ = dispatch_events.send(NetEvent::Error("格式修改超过单条消息上限，宿主将分批检查编辑锁并应用".into()));
                                    }) {
                                        let _ = dispatch_events.send(NetEvent::Error("协作格式未能完整发送".into()));
                                    }
                                }
                            } else {
                                // Disconnected while sending optimistic styles -> require full snapshot on reconnect
                                unconfirmed_styles_for_writer.fetch_add(1, Ordering::AcqRel);
                            }
                        }
                        Cmd::Sort(generation,batch) => {
                            let current = send_sess.lock().unwrap().as_ref()
                                .filter(|_| writer_generation.load(Ordering::Acquire) == generation)
                                .map(|gs| (gs.batch_ops, gs.sort_permutation, gs.acks.clone()));
                            if let Some((batched, capable, acks)) = current {
                                let ok = send_sort(&batch, capable, true, batched, &acks.lock().unwrap(), |line| {
                                    let mut session = send_sess.lock().unwrap();
                                    let mut writer = writer.lock().unwrap();
                                    let (Some(gs), Some(w)) = (session.as_mut(), writer.as_mut()) else { return false };
                                    Arc::ptr_eq(&gs.acks, &acks) && gs.write_plain(w, &line).is_ok()
                                });
                                if ok && !capable { pending_for_writer.lock().unwrap().remove(&batch.plan.first_ts); }
                                if !ok && send_sess.lock().unwrap().as_ref().is_some_and(|gs| Arc::ptr_eq(&gs.acks, &acks))
                                    && let Some(w) = writer.lock().unwrap().as_ref() { let _ = w.get_ref().shutdown(std::net::Shutdown::Both); }
                            }
                        }
                        Cmd::GuestStructure(generation,msg) => {
                            let mut session = send_sess.lock().unwrap();
                            let mut writer = writer.lock().unwrap();
                            if writer_generation.load(Ordering::Acquire) == generation
                                && let (Some(gs), Some(w)) = (session.as_mut(), writer.as_mut()) {
                                let result = gs.write_plain(w, &serde_json::to_string(&msg).unwrap());
                                if result.is_err() { let _ = w.get_ref().shutdown(std::net::Shutdown::Both); }
                            }
                        }
                        Cmd::Deliver(_, _) => unreachable!("room dispatch is host-only"),
                        Cmd::Broadcast(s) | Cmd::SendTo(_, s) => {
                            let mut session = send_sess.lock().unwrap();
                            if let (Some(gs), Some(w)) = (session.as_mut(), writer.lock().unwrap().as_mut()) {
                                let _ = gs.write_plain_opt(w, &s, flush_now);
                            }
                        }
                        Cmd::SendPrepared(_, prepared) => {
                            let mut session = send_sess.lock().unwrap();
                            if let (Some(gs), Some(w)) = (session.as_mut(), writer.lock().unwrap().as_mut()) {
                                let _ = gs.write_prepared_opt(w, &prepared, flush_now);
                            }
                        }
                        Cmd::PumpUpdate => {}
                        Cmd::Stop => {
                            stop_now = true;
                            break;
                        }
                    }
                    cur = peek;
                }
                if stop_now {
                    break;
                }
                // 排空高优命令后穿插发送低优 P2P 更新分片，每发 1 片立即检查是否有新实时协作命令到达
                loop {
                    if let Ok(hp) = cmd_rx.try_recv() {
                        next_cmd = Some(hp);
                        break;
                    }
                    let popped = {
                        let (lock, cvar) = &*guest_low_prio_for_writer;
                        let mut g = lock.lock().unwrap();
                        let item = g.0.pop_front();
                        let empty = g.0.is_empty();
                        if item.is_some() {
                            cvar.notify_one();
                        }
                        (item, empty)
                    };
                    let (Some(slice), queue_empty) = popped else {
                        if let Some(w) = writer.lock().unwrap().as_mut() {
                            let _ = w.flush();
                        }
                        break;
                    };
                    {
                        let mut session = send_sess.lock().unwrap();
                        if let (Some(gs), Some(w)) = (session.as_mut(), writer.lock().unwrap().as_mut()) {
                            let _ = gs.write_prepared_opt(w, &slice, queue_empty);
                        } else {
                            let (lock, cvar) = &*guest_low_prio_for_writer;
                            let mut g = lock.lock().unwrap();
                            g.0.clear();
                            cvar.notify_all();
                            break;
                        }
                    }
                    if queue_empty {
                        break;
                    }
                }
                if next_cmd.is_none() {
                    next_cmd = cmd_rx.recv().ok();
                }
            }
            let (lock, cvar) = &*guest_low_prio_for_writer;
            let mut g = lock.lock().unwrap();
            g.0.clear();
            g.1 = true;
            cvar.notify_all();
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
            let mut synced_once = false;
            // 单调房间水位线（初始快照完成后生效）：闪断重连时仅拉取断线窗口增量而非百万格全量快照
            let mut resume_state: Option<(String, u64, usize)> = None;
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
                let write_raw = match connected.try_clone() {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let _ = write_raw.set_write_timeout(Some(Duration::from_secs(10)));
                let mut write_half = BufWriter::with_capacity(64 * 1024, write_raw);
                let reader = BufReader::with_capacity(128 * 1024, &*connected);
                let mut denied = false;
                let mut got_welcome = false;
                let mut in_initial_snapshot = false;
                let mut pending_snap_ops: Vec<Op> = Vec::new();
                let mut snap_builder: Option<PreparedGuestSnapshotBuilder> = None;
                let mut conn_room_id = String::new();
                let mut conn_welcome_wm = 0u64;
                let mut conn_welcome_srev = 0usize;
                let mut recv_sess: Option<sheetcore::wire::Session> = None;
                let mut pending_sender: Option<sheetcore::wire::Session> = None;

                let (sort_ack, sort_replies) = channel();
                let sort_replies = Arc::new(Mutex::new(sort_replies));
                let connection = Arc::new(());
                let mut active_vote: Weak<Mutex<Option<Sender<bool>>>> = Weak::new();
                let mut reader = reader;
                loop {
                    let allow_binary = got_welcome && recv_sess.is_some();
                    let Ok(Some(frame)) = read_wire_frame(&mut reader, MAX_LINE, allow_binary) else { break };
                    let msg: Option<Msg> = match frame {
                        WireFrame::Binary(body) => {
                            let Some(se) = recv_sess.as_mut() else { break };
                            match se.open_binary_frame_owned(body) {
                                Some(sheetcore::wire::DecodedWireFrame::Text(plain)) => {
                                    serde_json::from_str::<Msg>(&plain).ok()
                                }
                                Some(sheetcore::wire::DecodedWireFrame::Compact(bytes)) => {
                                    sheetcore::sync::decode_msg_compact(&bytes)
                                }
                                None => break,
                            }
                        }
                        WireFrame::Text(line) => {
                            if let Some(se) = recv_sess.as_mut() {
                                match se.open(&line) {
                                    Some(plain) => serde_json::from_str::<Msg>(&plain).ok(),
                                    None => match serde_json::from_str::<Msg>(&line) {
                                        Ok(msg @ Msg::Denied { .. }) if !got_welcome => Some(msg),
                                        _ => break,
                                    },
                                }
                            } else {
                                match serde_json::from_str::<Msg>(&line) {
                                    Ok(msg @ (Msg::ChallengeSalt { .. } | Msg::Challenge { .. } | Msg::Denied { .. })) => Some(msg),
                                    _ => break,
                                }
                            }
                        }
                    };
                    let msg = match msg {
                        Some(m) => m,
                        // 入房后认不出的报文（例如更新版本宿主的新消息类型）跳过即可，
                        // 断开只会换来无限重连；握手阶段认不出才断开
                        None if got_welcome => continue,
                        None => break,
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
                            if !pending_sorts.lock().unwrap().is_empty()
                                || pending_structure_for_reader.load(Ordering::Acquire)
                                || unconfirmed_styles_for_reader.swap(0, Ordering::AcqRel) > 0
                            {
                                resume_state = None;
                            }
                            let (resume_room, resume_watermark, resume_struct_rev) =
                                resume_state.clone().unwrap_or_default();
                            let hello = Msg::Hello {
                                site,
                                name: name.clone(),
                                doc: doc.clone(),
                                proof: room_key.proof(nonce),
                                auth: client_auth.clone(),
                                sort_permutation: true,
                                palette_styles: true,
                                resume_room,
                                resume_watermark,
                                resume_struct_rev,
                                binary_v3: true,
                                compact_v3: true,
                                base_hash: if synced_once {
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
                        Msg::Welcome {
                            peers,
                            doc: remote_doc,
                            delta_sync,
                            clock,
                            batch_ops,
                            sort_permutation,
                            structural_ack,
                            palette_styles,
                            room_id,
                            watermark,
                            structure_revision,
                            binary_v3,
                            compact_v3,
                            ..
                        } if !got_welcome => {
                            if require_same_document && remote_doc != doc {
                                let _ = evt_tx.send(NetEvent::Denied("共享地址已属于另一工作簿，请刷新协作会话".into()));
                                denied = true;
                                break;
                            }
                            let use_compact = binary_v3 && compact_v3;
                            host_compact_flag.store(use_compact, Ordering::Release);
                            reader_generation.fetch_add(1,Ordering::AcqRel);
                            ack_for_reader.store(structural_ack,Ordering::Release);
                            palette_for_reader.store(palette_styles, Ordering::Release);
                            got_welcome = true;
                            in_initial_snapshot = true;
                            pending_snap_ops.clear();
                            *ready_snapshot_for_reader.lock().unwrap() = None;
                            snap_builder = (!delta_sync
                                && fast_handoff_for_reader.load(Ordering::Acquire))
                            .then(|| {
                                PreparedGuestSnapshotBuilder::new(
                                    seed_styles_for_reader.lock().unwrap().clone(),
                                )
                            });
                            conn_room_id = room_id;
                            conn_welcome_wm = watermark;
                            conn_welcome_srev = structure_revision;
                            last_wm_for_reader.fetch_max(watermark, Ordering::AcqRel);
                            if !delta_sync {
                                resume_state = None;
                            }
                            if let Some(se) = pending_sender.take() {
                                *send_sess.lock().unwrap() = Some(GuestSend {
                                    sess: se,
                                    batch_ops,
                                    sort_permutation,
                                    binary_v3,
                                    compact_v3: use_compact,
                                    scratch: Vec::new(),
                                    acks: sort_replies.clone(),
                                });
                            }
                            if let Ok(clone) = connected.try_clone() {
                                let _ = clone.set_write_timeout(Some(Duration::from_secs(10)));
                                *writer.lock().unwrap() = Some(BufWriter::with_capacity(128 * 1024, clone));
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
                        Msg::SortReply { first_ts, outcome } => { let _ = sort_ack.send((first_ts,outcome)); }
                        Msg::SortRepairDone { first_ts } => { let _ = evt_tx.send(NetEvent::SortRepairDone(first_ts)); }
                        Msg::SortCommitted { first_ts } => {
                            if let Some(batch) = pending_sorts.lock().unwrap().remove(&first_ts) {
                                if !batch.styles.is_empty() { let _ = evt_tx.send(NetEvent::CommittedSortStyles(batch)); }
                            }
                        }
                        Msg::SortRepairs { sheet, first_ts, items } => {
                            let _ = evt_tx.send(NetEvent::SortRepairs { sheet,first_ts,items });
                        }
                        Msg::SortPermutation(plan) => {
                            if active_vote.upgrade().is_some_and(|v| v.lock().unwrap().is_some()) { break; }
                            let first_ts = plan.first_ts;
                            let (tx,rx) = channel();
                            let vote = SortVote { sender: Arc::new(Mutex::new(Some(tx))), connection: Arc::downgrade(&connection) };
                            active_vote = Arc::downgrade(&vote.sender);
                            let deadline_vote = active_vote.clone();
                            let connected_to_offer = vote.connection.clone();
                            if evt_tx.send(NetEvent::Sort(plan, vote)).is_err() { break; }
                            let current_replies = sort_replies.clone();
                            let session = send_sess.clone();
                            let Ok(response_raw) = connected.try_clone() else { break };
                            let _ = response_raw.set_write_timeout(Some(Duration::from_secs(10)));
                            let mut response = BufWriter::new(response_raw);
                            std::thread::spawn(move || {
                                // A busy UI may take longer than a previous-version peer's
                                // deadline. Request its ordinary cells before that deadline.
                                // The one-shot vote prevents late work from applying twice.
                                let accepted = SortVote::wait(rx,deadline_vote,Duration::from_secs(15));
                                if connected_to_offer.upgrade().is_none() { return; }
                                let mut session = session.lock().unwrap();
                                let Some(gs) = session.as_mut() else { return };
                                if !gs.sort_permutation || !Arc::ptr_eq(&gs.acks, &current_replies) { return; }
                                let outcome = if accepted { SortResponse::Accepted } else { SortResponse::NeedCells };
                                let line = serde_json::to_string(&Msg::SortReply { first_ts, outcome }).unwrap();
                                if gs.write_plain(&mut response, &line).is_err() {
                                    let _ = response.get_ref().shutdown(std::net::Shutdown::Both);
                                }
                            });
                        }
                        Msg::SnapshotChunk { ops, last } => {
                            if in_initial_snapshot {
                                if let Some(b) = snap_builder.as_mut() {
                                    if !ops.is_empty() {
                                        b.apply_ops(ops);
                                    }
                                } else if !ops.is_empty() {
                                    pending_snap_ops.extend(ops);
                                    if pending_snap_ops.len() >= SNAPSHOT_EVENT_COALESCE {
                                        let _ = evt_tx.send_snapshot_batch(NetEvent::Ops(
                                            std::mem::take(&mut pending_snap_ops),
                                        ));
                                    }
                                }
                                if last {
                                    in_initial_snapshot = false;
                                    if let Some(b) = snap_builder.take() {
                                        *ready_snapshot_for_reader.lock().unwrap() =
                                            Some(b.finish());
                                    } else if !pending_snap_ops.is_empty() {
                                        let _ = evt_tx.send(NetEvent::Ops(std::mem::take(
                                            &mut pending_snap_ops,
                                        )));
                                    }
                                    synced_once = true;
                                    if !conn_room_id.is_empty() && conn_welcome_wm >= 1 {
                                        resume_state = Some((
                                            conn_room_id.clone(),
                                            conn_welcome_wm,
                                            conn_welcome_srev,
                                        ));
                                    }
                                    let _ = evt_tx.send(NetEvent::SnapshotDone);
                                }
                            } else {
                                if !ops.is_empty() {
                                    let _ = evt_tx.send(NetEvent::Ops(ops));
                                }
                                if last {
                                    synced_once = true;
                                    if !conn_room_id.is_empty() && conn_welcome_wm >= 1 {
                                        resume_state = Some((
                                            conn_room_id.clone(),
                                            conn_welcome_wm,
                                            conn_welcome_srev,
                                        ));
                                    }
                                    let _ = evt_tx.send(NetEvent::SnapshotDone);
                                }
                            }
                        }
                        Msg::Snapshot { ops } => {
                            in_initial_snapshot = false;
                            if let Some(mut b) = snap_builder.take() {
                                if !ops.is_empty() {
                                    b.apply_ops(ops);
                                }
                                *ready_snapshot_for_reader.lock().unwrap() = Some(b.finish());
                            } else {
                                if !ops.is_empty() {
                                    pending_snap_ops.extend(ops);
                                }
                                if !pending_snap_ops.is_empty() {
                                    let _ = evt_tx.send(NetEvent::Ops(std::mem::take(
                                        &mut pending_snap_ops,
                                    )));
                                }
                            }
                            synced_once = true;
                            if !conn_room_id.is_empty() && conn_welcome_wm >= 1 {
                                resume_state = Some((
                                    conn_room_id.clone(),
                                    conn_welcome_wm,
                                    conn_welcome_srev,
                                ));
                            }
                            let _ = evt_tx.send(NetEvent::SnapshotDone);
                        }
                        Msg::Watermark { seq, structure_revision } => {
                            last_wm_for_reader.fetch_max(seq, Ordering::AcqRel);
                            if synced_once && !conn_room_id.is_empty() {
                                resume_state =
                                    Some((conn_room_id.clone(), seq, structure_revision));
                            } else {
                                conn_welcome_wm = conn_welcome_wm.max(seq);
                                conn_welcome_srev = structure_revision;
                            }
                        }
                        Msg::Op(op) => {
                            if !in_initial_snapshot
                                && op.site != site
                                && min_saved_wm_for_reader.load(Ordering::Acquire)
                                    > last_wm_for_reader.load(Ordering::Acquire)
                            {
                                min_saved_wm_for_reader.fetch_add(1, Ordering::AcqRel);
                            }
                            let _ = evt_tx.send(NetEvent::Ops(vec![op]));
                        }
                        Msg::Ops { ops } => {
                            if !in_initial_snapshot
                                && ops.iter().any(|o| o.site != site)
                                && min_saved_wm_for_reader.load(Ordering::Acquire)
                                    > last_wm_for_reader.load(Ordering::Acquire)
                            {
                                min_saved_wm_for_reader.fetch_add(1, Ordering::AcqRel);
                            }
                            let _ = evt_tx.send(NetEvent::Ops(ops));
                        }
                        Msg::StructureRevision { revision } => {
                            let _ = evt_tx.send(NetEvent::StructureRevision(revision));
                        }
                        Msg::StructuralRejected { reason } => {
                            let _ = evt_tx.send(NetEvent::StructuralRejected(reason));
                        }
                        Msg::StructuralLog { items } => {
                            let _ = evt_tx.send(NetEvent::StructuralHistory(items));
                        }
                        Msg::Structural { site, sheet, op } => {
                            if let Some((_, _, srev)) = resume_state.as_mut() {
                                *srev = srev.saturating_add(1);
                            }
                            let _ = evt_tx.send(NetEvent::Structural { site, sheet, op });
                        }
                        Msg::StyleOp { site: author, sheet, items, .. } => {
                            if author == site {
                                let _ = unconfirmed_styles_for_reader.fetch_update(
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                    |v| Some(v.saturating_sub(1)),
                                );
                            }
                            let items = items.into_iter().map(|(r,c,s)| (sheet,r,c,s)).collect();
                            let event = if author == site { NetEvent::ConfirmedStyles(items) } else {
                                NetEvent::Styles { site: author, items, snapshot: false }
                            };
                            let _ = evt_tx.send(event);
                        }
                        Msg::StylePaletteOp { site: author, sheet, palette, items, .. } => {
                            if author == site {
                                let _ = unconfirmed_styles_for_reader.fetch_update(
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                    |v| Some(v.saturating_sub(1)),
                                );
                            }
                            if let Some(decoded) = sheetcore::sync::decode_style_palette(&palette, &items) {
                                let items = decoded.into_iter().map(|(r, c, s)| (sheet, r, c, s)).collect();
                                let event = if author == site {
                                    NetEvent::ConfirmedStyles(items)
                                } else {
                                    NetEvent::Styles { site: author, items, snapshot: false }
                                };
                                let _ = evt_tx.send(event);
                            }
                        }
                        Msg::StyleSnapshot { items } => {
                            if let Some(b) = snap_builder.as_mut() {
                                b.apply_style_snapshot(items);
                            } else {
                                let _ = evt_tx.send_snapshot_batch(NetEvent::Styles {
                                    site: 0,
                                    items,
                                    snapshot: true,
                                });
                            }
                        }
                        Msg::StylePaletteSnapshot { palette, items } => {
                            if let Some(b) = snap_builder.as_mut() {
                                b.apply_style_palette_snapshot(&palette, &items);
                            } else if let Some(decoded) =
                                sheetcore::sync::decode_style_snapshot_palette(&palette, &items)
                            {
                                let _ = evt_tx.send_snapshot_batch(NetEvent::Styles {
                                    site: 0,
                                    items: decoded,
                                    snapshot: true,
                                });
                            }
                        }
                        Msg::AxisStyleOp { site: author, sheet, patch, .. } => {
                            if author == site {
                                let _ = unconfirmed_styles_for_reader.fetch_update(
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                    |v| Some(v.saturating_sub(1)),
                                );
                            } else {
                                let _ = evt_tx.send(NetEvent::AxisStyles {
                                    site: author,
                                    sheet,
                                    patch,
                                    snapshot: false,
                                });
                            }
                        }
                        Msg::AxisStyleSnapshot { sheet, patch } => {
                            if let Some(b) = snap_builder.as_mut() {
                                b.apply_axis_snapshot(sheet, patch);
                            } else {
                                let _ = evt_tx.send_snapshot_batch(NetEvent::AxisStyles {
                                    site: 0,
                                    sheet,
                                    patch,
                                    snapshot: true,
                                });
                            }
                        }
                        Msg::MergeOp { site: msite, sheet, ts: _, r1, c1, r2, c2, on } => {
                            if in_initial_snapshot
                                && let Some(b) = snap_builder.as_mut()
                            {
                                b.apply_merge(sheet, r1, c1, r2, c2, on);
                            } else {
                                let _ = evt_tx.send(NetEvent::Merge {
                                    site: msite,
                                    sheet,
                                    rect: (r1, c1, r2, c2),
                                    on,
                                });
                            }
                        }
                        Msg::SheetMetaOp { site: msite, sheet, ts: _, kind } => {
                            if !in_initial_snapshot
                                && msite != site
                                && min_saved_wm_for_reader.load(Ordering::Acquire)
                                    > last_wm_for_reader.load(Ordering::Acquire)
                            {
                                min_saved_wm_for_reader.fetch_add(1, Ordering::AcqRel);
                            }
                            let _ = evt_tx.send(NetEvent::SheetMeta {
                                site: msite,
                                sheet,
                                kind,
                                snapshot: false,
                            });
                        }
                        Msg::SheetMetaSnapshot { items } => {
                            for (sheet, kind) in items {
                                let _ = evt_tx.send(NetEvent::SheetMeta {
                                    site: 0,
                                    sheet,
                                    kind,
                                    snapshot: true,
                                });
                            }
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
                        Msg::Saved { clock, watermark_seq, .. } => {
                            let _ = evt_tx.send(NetEvent::Saved { clock, watermark_seq });
                        }
                        Msg::CursorMsg(c) => {
                            let _ = evt_tx.send_cursor(NetEvent::Cursor(c));
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
                                let low_q = guest_low_prio_for_reader.clone();
                                let use_compact_bin = host_compact_flag.clone();
                                client_seeder_queue.enqueue(UpdateSeedJob {
                                    my_site: site,
                                    to_site: from_site,
                                    req_version: version,
                                    seed_override: client_seed_override.clone(),
                                    alive: alive.clone(),
                                    use_compact_bin,
                                    send_slice: Box::new(move |payload| {
                                        let (lock, cvar) = &*low_q;
                                        let mut g = lock.lock().unwrap();
                                        while g.0.len() >= LOW_PRIO_UPDATE_CAP && !g.1 {
                                            g = cvar.wait(g).unwrap();
                                        }
                                        if g.1 {
                                            return false;
                                        }
                                        g.0.push_back(payload);
                                        drop(g);
                                        out_tx.send(Cmd::PumpUpdate).is_ok()
                                    }),
                                });
                            }
                        }
                        Msg::UpdateSlice {
                            from_site,
                            to_site,
                            version,
                            offset,
                            total,
                            data_b64,
                            raw_data,
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
                                    &raw_data,
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
                drop(connection);
                if let Some(vote) = active_vote.upgrade() {
                    vote.lock().unwrap().take();
                }
                drop(sort_ack);
                if !pending_sorts.lock().unwrap().is_empty()
                    || pending_structure_for_reader.load(Ordering::Acquire)
                    || unconfirmed_styles_for_reader.swap(0, Ordering::AcqRel) > 0
                {
                    resume_state = None;
                }
                connected_flag.store(false, Ordering::SeqCst);
                *writer.lock().unwrap() = None;
                *send_sess.lock().unwrap() = None;
                {
                    let (lock, cvar) = &*guest_low_prio_for_reader;
                    let mut g = lock.lock().unwrap();
                    g.0.clear();
                    cvar.notify_all();
                }
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
        host_baseline: None,
        formula_keys: None,
        host_watermark: None,
        host_max_clock: None,
        host_wake_addr: None,
        state: None,
        delta_ops: None,
        host_base_hash: None,
        host_roster: None,
        room_order: None,
        #[cfg(test)]
        after_snapshot: Default::default(),
        #[cfg(test)]
        before_sort_reply: Default::default(),
        #[cfg(test)]
        before_sort_commit: Default::default(),
        host_locks: None,
        host_cursors: None,
        host_styles: None,
        delta_styles: None,
        delta_merges: None,
        host_meta_log: None,
        sheet_names: Default::default(),
        structure_seen: Default::default(),
        pending_structure,
        #[cfg(test)]
        unconfirmed_styles,
        last_watermark_seq,
        min_saved_watermark,
        structural_ack,
        palette_styles_cap,
        connection_generation,
        pending_sorts: pending_for_net,
        local_sort_ranges: Default::default(),
        host_structs: None,
        host_file_path: None,
        staged_save_cursor: Mutex::new(None),
        peer_update_dl: client_update_dl_for_net,
        update_seed_override: client_seed_override_for_net,
        seeder_queue: client_seeder_for_net,
        cursor_throttle: CursorSendThrottle::default(),
        fast_snapshot_handoff,
        seed_styles,
        ready_snapshot,
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

    pub(super) fn op(row: u32, n: f64, ts: u64, site: u32) -> Op {
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

    pub(super) fn wait_for<F: Fn(&NetEvent) -> bool>(net: &Net, secs: u64, f: F) -> bool {
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
            sheet: 0, row: 0, col: 0, site: 1, name: "Host".into(), color: 1, end_row: None, end_col: None,
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
            end_row: None,
            end_col: None,
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
            sort_permutation: false,
            palette_styles: false,
            resume_room: String::new(),
            resume_watermark: 0,
            resume_struct_rev: 0,
            binary_v3: false,
            compact_v3: false,
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

    use super::tests::{op,wait_for};

    #[test]
    fn guest_insert_seeds_late_join_with_local_and_cross_sheet_formula_references() {
        let _env = crate::test_env::TestEnv::new("structural_formula_snapshot");
        let (host,port) = host_for_test(1,"H","doc","pw");
        let mut book = sheetcore::Workbook::new_blank();
        book.sheets[0].name = "Sales".into();
        book.sheets.push(sheetcore::Sheet::new("Report"));
        let mut peer = sheetcore::sync::Peer::new(1);
        for (s,r,c,formula,value) in [(0,0,0,None,3.),(0,0,1,Some("=A1*2"),6.),
                                       (1,0,0,Some("=Sales!B1+A2"),6.)] {
            let cell = sheetcore::Cell { value:sheetcore::Value::Number(value),formula:formula.map(Into::into) };
            peer.seed_cell(s,r,c,&cell);
            book.sheets[s].set(r,c,cell);
        }
        host.seed_snapshot(&peer,&book,Cursor { site:1,name:"H".into(),color:1,sheet:0,row:0,col:0,end_row:None,end_col:None });
        let addr = format!("127.0.0.1:{port}");
        let guest = join(2,"G".into(),"doc".into(),&addr,"pw".into()).unwrap();
        assert!(wait_event(&guest,3000,|e| matches!(e,NetEvent::SnapshotDone)).is_some());
        guest.broadcast_structural(0,&Structural::InsertRows {at:0,n:1}).unwrap();
        assert!(wait_event(&guest,3000,|e| matches!(e,NetEvent::Structural {site:2,..})).is_some());
        let late = join(3,"L".into(),"doc".into(),&addr,"pw".into()).unwrap();
        let mut cells = HashMap::new();
        loop {
            match late.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                NetEvent::Ops(ops) => for op in ops { cells.insert((op.sheet,op.row,op.col),op); },
                NetEvent::SnapshotDone => break,
                _ => {}
            }
        }
        assert_eq!(cells[&(0,1,1)].formula.as_deref(),Some("=A2*2"));
        assert_eq!(cells[&(1,0,0)].formula.as_deref(),Some("=Sales!B2+A2"));
        assert_eq!(cells[&(0,1,0)].value,ValueWire::Number(3.));
    }

    #[test]
    fn structural_broadcast_rejects_invalid_coordinates_before_setting_pending() {
        let _env = crate::test_env::TestEnv::new("bounded_structure_request");
        let (host,port) = host_for_test(1,"H","doc","pw");
        let guest = join(2,"G".into(),"doc".into(),&format!("127.0.0.1:{port}"),"pw".into()).unwrap();
        assert!(wait_event(&guest,3000,|e| matches!(e,NetEvent::SnapshotDone)).is_some());
        for peer in [&host,&guest] {
            for (sheet,op) in [(0, Structural::InsertCols { at:0,n:0 }),
                (0,Structural::DeleteRows { at:sheetcore::sync::MAX_ROWS,n:1 }),
                (sheetcore::sync::MAX_SHEET_INDEX,Structural::InsertRows { at:0,n:1 })] {
                assert!(peer.broadcast_structural(sheet,&op).is_err());
                assert!(!peer.pending_structure());
                assert_eq!(peer.structure_seen(),0);
            }
        }
        guest.broadcast_structural(0,&Structural::InsertRows { at:5,n:1 }).unwrap();
        assert!(wait_event(&host,3000,|e| matches!(e,NetEvent::Structural { .. })).is_some());
        // Network state is committed, but the host UI still has the previous coordinates.
        assert!(host.broadcast_structural(0,&Structural::DeleteRows { at:5,n:1 }).is_err());
        assert!(host.broadcast_structural(1,&Structural::InsertCols { at:0,n:1 }).is_ok());
        assert_eq!(host.structure_seen(),0);
        assert!(host.broadcast_structural(0,&Structural::DeleteRows { at:5,n:1 }).is_err());
        host.note_structure(1);
        assert_eq!(host.structure_seen(),2);
        assert!(host.broadcast_structural(0,&Structural::DeleteRows { at:5,n:1 }).is_ok());
    }

    #[test]
    fn a_cell_edit_can_commit_while_a_sort_is_being_prepared() {
        let _env = crate::test_env::TestEnv::new("sort_prepare_without_room_lock");
        let (host, port) = host_for_test(1,"H","doc","sort");
        let addr = format!("127.0.0.1:{port}");
        let writer = join(2,"W".into(),"doc".into(),&addr,"sort".into()).unwrap();
        let editor = join(3,"E".into(),"doc".into(),&addr,"sort".into()).unwrap();
        for peer in [&writer,&editor] {
            assert!(wait_event(peer,5000,|e| matches!(e,NetEvent::SnapshotDone)).is_some());
        }
        let make = |row,n,ts,site| Op { sheet:0,row,col:0,value:ValueWire::Number(n),formula:None,ts,site };
        host.broadcast_ops(vec![make(0,2.,1,1),make(1,1.,1,1)]);
        let (parked,entered) = channel();
        let (release,go) = channel();
        *host.before_sort_commit.lock().unwrap() = Some(Box::new(move || {
            parked.send(()).unwrap();
            go.recv_timeout(Duration::from_secs(6)).unwrap();
        }));
        let sort = sheetcore::sync::SortPermutation { structural_revision:0,sheet:0,name:"Sheet1".into(),
            region:(0,0,1,0),horizontal:false,order:vec![1,0],first_ts:2,site:2,digest:[0;32] };
        writer.broadcast_sort(SortBatch::new(sort,vec![Arc::new(make(0,1.,2,2)),Arc::new(make(1,2.,3,2))],vec![]));
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        editor.broadcast_op(make(0,88.,10,3));
        assert!(wait_event(&host,3000,|e| matches!(e,NetEvent::Ops(ops) if ops.iter().any(|o| o.ts==10))).is_some());
        release.send(()).unwrap();
        assert!(wait_event(&host,5000,|e| matches!(e,NetEvent::Sorted(_))).is_some());
        let state = host.state.as_ref().unwrap().lock().unwrap();
        assert_eq!(state.get(&(0,0,0)).unwrap().value,ValueWire::Number(88.));
        assert_eq!(state.get(&(0,1,0)).unwrap().value,ValueWire::Number(2.));
    }

    #[test]
    fn a_modal_ui_missing_its_vote_deadline_can_never_apply_the_fallback_twice() {
        let connection = Arc::new(());
        for answer_before_deadline in [true,false] {
            let (tx,rx) = channel();
            let vote = SortVote { sender:Arc::new(Mutex::new(Some(tx))),connection:Arc::downgrade(&connection) };
            if answer_before_deadline { vote.send(true).unwrap(); }
            assert_eq!(SortVote::wait(rx,Arc::downgrade(&vote.sender),Duration::ZERO),answer_before_deadline);
            assert!(vote.send(true).is_err());
        }
    }

    #[test]
    fn waiting_for_guest_ui_does_not_block_replies_to_its_own_sort() {
        let _env = crate::test_env::TestEnv::new("sort_bidirectional_progress");
        let (host,port) = host_for_test(1,"H","doc","sort");
        let guest = join(2,"G".into(),"doc".into(),&format!("127.0.0.1:{port}"),"sort".into()).unwrap();
        assert!(wait_event(&guest,5000,|e| matches!(e,NetEvent::SnapshotDone)).is_some());
        let make = |r,n,t,site| Op { sheet:0,row:r,col:0,value:ValueWire::Number(n),formula:None,ts:t,site };
        let batch = |row,ts,site| SortBatch::new(sheetcore::sync::SortPermutation {
            sheet:0,structural_revision:0,name:"Sheet1".into(),region:(row,0,row+1,0),
            horizontal:false,order:vec![row+1,row],first_ts:ts,site,digest:[0;32],
        },vec![Arc::new(make(row,1.,ts,site)),Arc::new(make(row+1,2.,ts+1,site))],vec![]);
        host.broadcast_ops(vec![make(0,2.,1,1),make(1,1.,1,1),make(4,2.,1,1),make(5,1.,1,1)]);
        host.broadcast_sort(batch(0,2,1));
        let Some(NetEvent::Sort(_,vote)) = wait_event(&guest,5000,|e| matches!(e,NetEvent::Sort(..))) else {
            panic!("missing host sort");
        };
        // Keep the GUI's vote pending while the guest negotiates the opposite direction.
        guest.broadcast_sort(batch(4,4,2));
        guest.broadcast_op(make(8,9.,9,2));
        assert!(wait_event(&host,3000,|e| matches!(e,NetEvent::Ops(v) if v.iter().any(|o| o.row==8))).is_some());
        vote.send(false).unwrap();
        assert!(wait_event(&guest,3000,|e| matches!(e,NetEvent::Ops(v) if v.iter().any(|o| o.ts==2))).is_some());
        assert!(guest.is_live());
        host.broadcast_sort(batch(0,12,1));
        let Some(NetEvent::Sort(_,late_vote)) = wait_event(&guest,3000,|e| matches!(e,NetEvent::Sort(..))) else {
            panic!("missing second offer");
        };
        guest.shutdown();
        assert!(wait_event(&host,3000,|e| matches!(e,NetEvent::PeerLeft(2))).is_some());
        assert!(wait_event(&guest,3000,|e| matches!(e,NetEvent::Disconnected(_))).is_some());
        assert!(late_vote.send(true).is_err());
    }

    #[test]
    fn negotiated_sort_preserves_order_recovers_a_divergent_replica_and_seeds_late_join() {
        let _env = crate::test_env::TestEnv::new("sort_wire_order");
        let (host, port) = host_for_test(1,"host","doc","sort");
        let addr = format!("127.0.0.1:{port}");
        for (r,n) in [(0,2.),(1,1.)] { host.broadcast_op(op(r,n,1,1)); }
        let writer = join(2,"writer".into(),"doc".into(),&addr,"sort".into()).unwrap();
        let reader = join(3,"reader".into(),"doc".into(),&addr,"sort".into()).unwrap();
        for p in [&writer,&reader] { assert!(wait_for(p,5,|e| matches!(e,NetEvent::SnapshotDone))); }
        let ops = vec![Arc::new(op(0,1.,2,2)), Arc::new(op(1,2.,3,2))];
        let plan = sheetcore::sync::SortPermutation { structural_revision: 0, sheet:0, name:"Sheet1".into(), region:(0,0,1,0),
            horizontal:false, order:vec![1,0], first_ts:2, site:2, digest:[0;32] };
        writer.broadcast_sort(SortBatch::new(plan,ops.clone(),vec![]));
        writer.broadcast_op(op(1,9.,4,2));
        assert!(wait_for(&host,5,|e| matches!(e,NetEvent::Sorted(batch) if batch.ops == ops)));
        let Some(NetEvent::Sort(offered,reply)) = wait_event(&reader,5000,|e| matches!(e,NetEvent::Sort(..))) else { panic!("expected negotiated permutation"); };
        assert_eq!(offered.order,[1,0]);
        // This recipient's source differs. Its exact fallback must precede the later edit.
        reply.send(false).unwrap();
        let Some(NetEvent::Ops(initial)) = wait_event(&reader,5000,|e| matches!(e,NetEvent::Ops(_))) else { panic!("no recovery"); };
        assert_eq!(initial,[(*ops[0]).clone(),(*ops[1]).clone()]);
        assert!(wait_for(&reader,5,|e| matches!(e,NetEvent::Ops(batch) if batch.as_slice()==[op(1,9.,4,2)])));
        let late = join(4,"late".into(),"doc".into(),&addr,"sort".into()).unwrap();
        let Some(NetEvent::Ops(snapshot)) = wait_event(&late,5000,|e| matches!(e,NetEvent::Ops(v) if !v.is_empty())) else { panic!("missing snapshot"); };
        assert_eq!(snapshot,[op(0,1.,2,2),op(1,9.,4,2)]);
    }

    #[test]
    fn successful_sort_sends_only_permutation_and_legacy_gets_exact_cells_and_formats() {
        let mut fmt = sheetcore::style::CellStyle::default();
        fmt.bold = true;
        let ops = vec![Arc::new(op(0,1.,2,2)), Arc::new(op(1,2.,3,2))];
        let plan = sheetcore::sync::SortPermutation { structural_revision: 0, sheet:0, name:"Sheet1".into(), region:(0,0,1,0),
            horizontal:false, order:vec![1,0], first_ts:2, site:2, digest:[0;32] };
        let batch = SortBatch::new(plan,ops,vec![(0,0,fmt.clone()),(1,0,Default::default())]);
        for supported in [false,true] {
            let (ack,rx) = channel();
            ack.send((1,SortResponse::NeedCells)).unwrap(); // already-finished offer
            ack.send((2,SortResponse::Accepted)).unwrap();
            let mut frames = Vec::new();
            assert!(send_sort(&batch,supported,false,true,&rx,|line| {
                frames.push(serde_json::from_str::<Msg>(&line).unwrap()); true
            }));
            if supported {
                assert!(matches!(frames.as_slice(),[Msg::SortPermutation(_)]));
                let replay = SortBatch::expand(&batch.offer(),|r,_| {
                    (ValueWire::Number(if r==0 {2.} else {1.}),None,Arc::new(if r==1 {fmt.clone()} else {Default::default()}))
                }).unwrap();
                assert_eq!(replay.ops,batch.ops);
                assert_eq!(replay.styles,batch.styles);
                assert!(SortBatch::expand(&batch.offer(),|_,_| (ValueWire::Empty,None,Arc::default())).is_none());
            } else {
                assert!(matches!(frames.as_slice(),[Msg::SnapshotChunk { ops,.. }, Msg::StyleOp { items,.. }] if ops.len()==2 && items.len()==2));
            }
        }
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

        assert!(host.host_styles.as_ref().unwrap().lock().unwrap().len()==0,
            "the held cell in the middle must reject all 700 styles");

        intruder.broadcast_merge(0, (2, 1, 4, 3), true);
        assert!(
            wait_event(&intruder, 3000, |e| matches!(e, NetEvent::WriteRejected { holder: 2, revert_merge: Some((2, 1, 4, 3, false)), .. })).is_some(),
            "闯入者合并覆盖他人持锁格必须被驳回并附带反向解合指令"
        );

        intruder.broadcast_structural(0, &Structural::InsertRows { at: 1, n: 1 }).unwrap();
        assert!(
            wait_event(&intruder, 3000, |e| matches!(e, NetEvent::StructuralRejected(reason) if reason.contains("持锁人"))).is_some(),
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

    #[test]
    fn poll_net_coalesces_multi_chunk_ops_and_syncs_axis_and_palette_styles() {
        use std::time::Instant;
        let env = crate::test_env::TestEnv::new("net_coalesce_and_axis_styles");
        let ctx = eframe::egui::Context::default();
        let mut host = crate::SheetApp::new_with_context(&ctx, None);
        let mut guest = crate::SheetApp::new_with_context(&ctx, None);
        host.wb.path = Some(env.path("host_perf.xlsx"));
        guest.wb.path = Some(env.path("guest_perf.xlsx"));

        let (net_host, port) = host_for_test(1, "Host", "perf-doc", "pw");
        let mut peer_host = sheetcore::sync::Peer::new(1);
        peer_host.seed_workbook(&host.wb);
        net_host.seed_snapshot(
            &peer_host,
            &host.wb,
            Cursor {
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
        host.peer = Some(peer_host);
        host.net = Some(net_host);
        host.undo.set_site(1);

        guest.net = Some(
            join(
                2,
                "Guest".into(),
                "perf-doc".into(),
                &format!("127.0.0.1:{port}"),
                "pw".into(),
            )
            .unwrap(),
        );
        guest.peer = Some(sheetcore::sync::Peer::new(2));
        guest.undo.set_site(2);

        let deadline = Instant::now() + Duration::from_secs(8);
        while guest.in_snapshot || !guest.net.as_ref().unwrap().connected.load(Ordering::Relaxed) {
            host.poll_net();
            guest.poll_net();
            assert!(Instant::now() < deadline, "guest snapshot sync timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        guest.rev.clear();

        let mut coords = Vec::with_capacity(2_500);
        for row in 0..2_500u32 {
            host.wb.sheet_mut(0).set(
                row,
                0,
                sheetcore::Cell {
                    value: sheetcore::Value::Number(f64::from(row + 1)),
                    formula: None,
                },
            );
            coords.push((0usize, row, 0u32));
        }
        host.broadcast_cells(&coords);

        // Give the 5 compressed Ops frames time to land in guest's channel, then drain with poll_net().
        std::thread::sleep(Duration::from_millis(120));
        guest.rev.clear();
        let drain_deadline = Instant::now() + Duration::from_secs(8);
        while guest.wb.sheet(0).value(2_499, 0) != sheetcore::Value::Number(2_500.0) {
            guest.poll_net();
            assert!(Instant::now() < drain_deadline, "2500-cell burst did not converge");
            std::thread::sleep(Duration::from_millis(5));
        }
        let total_rev_cells: usize = guest.rev.iter().map(|e| e.cells.len()).sum();
        assert!(
            guest.rev.len() <= 3 && total_rev_cells <= 600,
            "coalesced poll_net must cap remote revision entries: entries={}, cells={total_rev_cells}",
            guest.rev.len()
        );

        // Whole-column style via apply_style_xy syncs via O(1) AxisStyleOp instead of 1M per-cell styles.
        host.sheet_idx = 0;
        host.sel = (0, 1);
        host.sel_end = (1_048_575, 1);
        host.extra_sel.clear();
        host.apply_style_xy(|st, _, _| {
            st.bold = true;
            st.fill = Some([0xE2, 0xF0, 0xD9]);
        });
        let axis_deadline = Instant::now() + Duration::from_secs(8);
        while !guest.wb.styles.get(guest.wb.sheet(0).style_of(500_000, 1)).bold {
            host.poll_net();
            guest.poll_net();
            assert!(Instant::now() < axis_deadline, "whole-column AxisStyleOp did not converge");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            guest.wb.styles.get(guest.wb.sheet(0).style_of(500_000, 1)).fill,
            Some([0xE2, 0xF0, 0xD9])
        );
    }

    #[test]
    fn transient_disconnect_resumes_via_watermark_without_resending_baseline() {
        let _env = crate::test_env::TestEnv::new("net_watermark_resume");
        let (host, port) = host_for_test(1, "Host", "resume-doc", "pw");
        let mut wb = sheetcore::Workbook::new_blank();
        let mut peer = sheetcore::sync::Peer::new(1);
        for row in 0..800u32 {
            let cell = sheetcore::Cell {
                value: sheetcore::Value::Number(f64::from(row + 10)),
                formula: None,
            };
            wb.sheet_mut(0).set(row, 0, cell);
        }
        peer.seed_workbook(&wb);
        host.seed_snapshot(
            &peer,
            &wb,
            Cursor {
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

        // Guest joins WITHOUT base_hash: initial join pulls full snapshot (delta_sync == false).
        let guest = join(
            2,
            "Guest".into(),
            "resume-doc".into(),
            &format!("127.0.0.1:{port}"),
            "pw".into(),
        )
        .unwrap();
        let first_welcome = wait_event(&guest, 5000, |e| matches!(e, NetEvent::Welcome { .. }));
        assert!(
            matches!(first_welcome, Some(NetEvent::Welcome { delta_sync: false, .. })),
            "initial join without base_hash must use full snapshot: {first_welcome:?}"
        );
        assert!(wait_event(&guest, 5000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());

        // Guest sends an AxisStyleOp before transient disconnect; Host echoes it back so
        // `unconfirmed_styles` returns to 0 and watermark incremental resume stays enabled.
        let mut bold_col = sheetcore::style::CellStyle::default();
        bold_col.bold = true;
        guest.broadcast_axis_styles(
            0,
            AxisStylePatch {
                rows: Vec::new(),
                cols: vec![(2, Some(bold_col))],
            },
        );
        assert!(
            wait_event(&host, 5000, |e| matches!(e, NetEvent::AxisStyles { sheet: 0, .. }))
                .is_some(),
            "host must receive guest AxisStyleOp"
        );
        let echo_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while guest.unconfirmed_styles.load(Ordering::Acquire) != 0 {
            assert!(
                std::time::Instant::now() < echo_deadline,
                "guest unconfirmed_styles did not drain after Host AxisStyleOp echo"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        // Drop Guest's TCP socket transiently while Host writes a new cell.
        guest.disconnect_socket_for_test();
        assert!(wait_event(&guest, 5000, |e| matches!(e, NetEvent::Disconnected(_))).is_some());
        host.broadcast_op(sheetcore::sync::Op {
            sheet: 0,
            row: 5,
            col: 3,
            value: sheetcore::sync::ValueWire::Number(9876.0),
            formula: None,
            ts: 5000,
            site: 1,
        });

        // Guest reconnects automatically using its recorded watermark -> delta_sync == true!
        let second_welcome = wait_event(&guest, 8000, |e| matches!(e, NetEvent::Welcome { .. }));
        assert!(
            matches!(second_welcome, Some(NetEvent::Welcome { delta_sync: true, .. })),
            "transient reconnect must resume incrementally via watermark: {second_welcome:?}"
        );
        let delta_op = wait_event(&guest, 5000, |e| {
            matches!(e, NetEvent::Ops(ops) if ops.iter().any(|o| o.row == 5 && o.col == 3 && o.value == sheetcore::sync::ValueWire::Number(9876.0)))
        });
        assert!(delta_op.is_some(), "missed incremental op during transient disconnect");
    }

    #[test]
    fn baseline_explicit_zero_mask_and_session_axis_styles_converge_across_join_and_sort() {
        use sheetcore::{Cell, Value};
        use std::time::Instant;

        let env = crate::test_env::TestEnv::new("net_axis_zero_mask_sort");
        let ctx = eframe::egui::Context::default();
        let mut host = crate::SheetApp::new_with_context(&ctx, None);
        let mut guest = crate::SheetApp::new_with_context(&ctx, None);
        host.wb.path = Some(env.path("host_axis_sort.xlsx"));
        guest.wb.path = Some(env.path("guest_axis_sort.xlsx"));

        // Baseline setup on Host:
        // - Column 0 has whole-column bold style (`bold_id != 0`).
        // - Cell (0, 0) = 20.0 has an explicit `sid == 0` mask (unbolded override inside bold col).
        // - Cells (1, 0) = 10.0 and (2, 0) = 30.0 inherit bold from column 0.
        // - Column 1 has values [200.0, 100.0, 300.0] with default style initially.
        let mut bold_fmt = host.wb.styles.get(0).clone();
        bold_fmt.bold = true;
        let bold_id = host.wb.styles.intern(bold_fmt);
        host.wb.sheet_mut(0).set_col_style(0, bold_id);

        for (r, v0, v1) in [(0u32, 20.0, 200.0), (1, 10.0, 100.0), (2, 30.0, 300.0)] {
            host.wb.sheet_mut(0).set(
                r,
                0,
                Cell {
                    value: Value::Number(v0),
                    formula: None,
                },
            );
            host.wb.sheet_mut(0).set(
                r,
                1,
                Cell {
                    value: Value::Number(v1),
                    formula: None,
                },
            );
        }
        host.wb.sheet_mut(0).set_style(0, 0, 0);
        assert_eq!(host.wb.sheet(0).explicit_style_of(0, 0), Some(0));
        assert!(!host.wb.styles.get(host.wb.sheet(0).style_of(0, 0)).bold);
        assert!(host.wb.styles.get(host.wb.sheet(0).style_of(1, 0)).bold);

        let (net_host, port) = host_for_test(1, "Host", "axis-sort-doc", "pw");
        let mut peer_host = sheetcore::sync::Peer::new(1);
        peer_host.seed_workbook(&host.wb);
        net_host.seed_snapshot(
            &peer_host,
            &host.wb,
            Cursor {
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
        host.peer = Some(peer_host);
        host.net = Some(net_host);
        host.undo.set_site(1);

        guest.net = Some(
            join(
                2,
                "Guest".into(),
                "axis-sort-doc".into(),
                &format!("127.0.0.1:{port}"),
                "pw".into(),
            )
            .unwrap(),
        );
        guest.peer = Some(sheetcore::sync::Peer::new(2));
        guest.undo.set_site(2);

        fn poll_until(
            h: &mut crate::SheetApp,
            g: &mut crate::SheetApp,
            label: &str,
            mut pred: impl FnMut(&crate::SheetApp, &crate::SheetApp) -> bool,
        ) {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !pred(h, g) {
                h.poll_net();
                g.poll_net();
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {label}: H={} G={}",
                    h.status,
                    g.status
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        poll_until(&mut host, &mut guest, "initial snapshot", |_, g| {
            !g.in_snapshot
                && g.net.as_ref().unwrap().connected.load(Ordering::Relaxed)
                && g.wb.sheet(0).value(2, 0) == Value::Number(30.0)
        });

        // 1. Full snapshot must preserve BOTH col 0 bold AND cell (0, 0)'s explicit `sid == 0` mask.
        assert!(guest.wb.styles.get(guest.wb.sheet(0).style_of(1, 0)).bold);
        assert!(guest.wb.styles.get(guest.wb.sheet(0).style_of(500_000, 0)).bold);
        assert!(
            !guest.wb.styles.get(guest.wb.sheet(0).style_of(0, 0)).bold,
            "explicit sid==0 mask at (0,0) must survive full snapshot over bold col 0"
        );

        // 2. Mid-session: Guest sets whole-column italic on column 1 via AxisStyleOp.
        guest.sheet_idx = 0;
        guest.sel = (0, 1);
        guest.sel_end = (1_048_575, 1);
        guest.extra_sel.clear();
        assert!(guest.apply_style_xy(|st, _, _| st.italic = true));
        poll_until(&mut host, &mut guest, "live AxisStyleOp on col 1", |h, g| {
            h.wb.styles.get(h.wb.sheet(0).style_of(500_000, 1)).italic
                && g.net.as_ref().unwrap().unconfirmed_styles.load(Ordering::Acquire) == 0
        });

        // 3. Guest sorts (0, 0, 2, 1) ascending by col 0: [10.0, 20.0, 30.0].
        //    RoomGrids::expand on Host must match Guest's digest (preserving both the moved `sid == 0`
        //    mask on 20.0 -> row 1 and live `axis_cols[1]` italic style).
        guest.sort_region((0, 0, 2, 1), 0, true);
        poll_until(&mut host, &mut guest, "ascending sort convergence", |h, g| {
            h.wb.sheet(0).value(0, 0) == Value::Number(10.0)
                && g.net.as_ref().unwrap().pending_sort_ranges().is_empty()
        });
        for app in [&host, &guest] {
            assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(10.0));
            assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(20.0));
            assert_eq!(app.wb.sheet(0).value(2, 0), Value::Number(30.0));
            assert!(app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).bold);
            assert!(
                !app.wb.styles.get(app.wb.sheet(0).style_of(1, 0)).bold,
                "unbolded sid==0 mask must move with 20.0 to (1, 0)"
            );
            assert!(app.wb.styles.get(app.wb.sheet(0).style_of(2, 0)).bold);
            for r in 0..=2 {
                assert!(
                    app.wb.styles.get(app.wb.sheet(0).style_of(r, 1)).italic,
                    "live col 1 italic style must remain intact at ({r}, 1)"
                );
            }
        }

        // 4. Add an explicit underline+italic style on (0, 1), then trigger a rejected sort on Guest
        //    while Host locks (0, 0): SortCorrection/sort_repairs must restore (2, 1) to col 1's live
        //    italic style rather than clobbering it with default un-italicized style.
        let mut ul_italic = guest.wb.styles.get(guest.wb.sheet(0).style_of(0, 1)).clone();
        ul_italic.underline = true;
        let ul_id = guest.wb.styles.intern(ul_italic);
        guest.wb.sheet_mut(0).set_style(0, 1, ul_id);
        guest.broadcast_cell_styles(&[(0, 0, 1)]);
        poll_until(&mut host, &mut guest, "explicit underline on (0, 1)", |h, g| {
            h.wb.styles.get(h.wb.sheet(0).style_of(0, 1)).underline
                && g.net.as_ref().unwrap().unconfirmed_styles.load(Ordering::Acquire) == 0
        });

        host.editing = Some(crate::EditState::new("10".into()));
        host.edit_sheet = Some(0);
        host.edit_sel = Some((0, 0));
        host.net.as_ref().unwrap().request_lock(0, 0, 0, "Host");
        // Guest has not polled Host's lock yet -> attempts descending sort optimistically.
        guest.sort_region((0, 0, 2, 1), 0, false);
        poll_until(&mut host, &mut guest, "rejected sort repair", |_, g| {
            g.wb.sheet(0).value(0, 0) == Value::Number(10.0)
                && g.net.as_ref().unwrap().pending_sort_ranges().is_empty()
        });
        assert!(guest.wb.styles.get(guest.wb.sheet(0).style_of(0, 1)).underline);
        assert!(guest.wb.styles.get(guest.wb.sheet(0).style_of(0, 1)).italic);
        assert!(!guest.wb.styles.get(guest.wb.sheet(0).style_of(2, 1)).underline);
        assert!(
            guest.wb.styles.get(guest.wb.sheet(0).style_of(2, 1)).italic,
            "sort_repairs must preserve live axis italic on (2, 1)"
        );
    }

    #[test]
    fn watermark_cow_seq_log_deduplicates_and_survives_structural_shift() {
        let mut wm = HostWatermark::default();
        let op1 = Op {
            sheet: 0,
            row: 2,
            col: 1,
            value: ValueWire::Number(1.0),
            formula: None,
            ts: 2,
            site: 1,
        };
        let s1 = wm.record_ops([&op1]);
        let op2 = Op {
            sheet: 0,
            row: 5,
            col: 0,
            value: ValueWire::Number(2.0),
            formula: None,
            ts: 3,
            site: 1,
        };
        let s2 = wm.record_ops([&op2, &op1]);
        let snap = wm.clone();
        // Post-clone mutation does not affect snapshot
        let op3 = Op {
            sheet: 0,
            row: 8,
            col: 0,
            value: ValueWire::Number(3.0),
            formula: None,
            ts: 4,
            site: 1,
        };
        let _s3 = wm.record_ops([&op3]);
        let since_s1: Vec<_> = snap
            .op_log
            .since(s1)
            .filter(|(seq, k)| snap.op_seqs.get(k) == Some(seq))
            .map(|(_, k)| k)
            .collect();
        assert_eq!(since_s1, vec![(0, 5, 0), (0, 2, 1)]);
        assert_eq!(snap.seq, s2);
        // Structural insert shifts op_seqs and advances log_floor_seq
        let s_struct = wm.shift_structural(0, &Structural::InsertRows { at: 3, n: 2 });
        assert_eq!(wm.log_floor_seq, s_struct);
        assert!(wm.op_seqs.get(&(0, 2, 1)).is_some());
        assert!(wm.op_seqs.get(&(0, 7, 0)).is_some());
        assert!(wm.op_seqs.get(&(0, 10, 0)).is_some());
    }

    fn raw_client_handshake(
        port: u16,
        site: u32,
        name: &str,
        password: &str,
        auth: &str,
        binary_v3: bool,
    ) -> (TcpStream, BufReader<TcpStream>, sheetcore::wire::Session, Msg) {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect raw client");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut w = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let challenge_line = read_line_bounded(&mut reader, MAX_PREAUTH_LINE)
            .unwrap()
            .expect("challenge line");
        let Msg::ChallengeSalt { nonce, salt, room_salt } =
            serde_json::from_str::<Msg>(&challenge_line).expect("parse ChallengeSalt")
        else {
            panic!("expected ChallengeSalt, got {challenge_line}");
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
            sort_permutation: false,
            palette_styles: true,
            resume_room: String::new(),
            resume_watermark: 0,
            resume_struct_rev: 0,
            binary_v3,
            compact_v3: false,
        };
        let frame = sess.seal(&serde_json::to_string(&hello).unwrap());
        write_line(&mut w, &frame).unwrap();

        // Welcome 握手帧必须始终保持 v2 文本行，确保与旧版客户端/宿主 100% 互通
        let welcome_frame = read_wire_frame(&mut reader, MAX_LINE, false)
            .unwrap()
            .expect("welcome frame");
        let WireFrame::Text(welcome_line) = welcome_frame else {
            panic!("Welcome 必须走 v2 文本帧");
        };
        let welcome_plain = sess.open(&welcome_line).expect("open Welcome");
        let welcome_msg: Msg = serde_json::from_str(&welcome_plain).expect("parse Welcome");
        (w, reader, sess, welcome_msg)
    }

    fn read_decoded_frame(
        reader: &mut BufReader<TcpStream>,
        sess: &mut sheetcore::wire::Session,
        allow_binary: bool,
    ) -> (bool, usize, Msg) {
        let frame = read_wire_frame(reader, MAX_LINE, allow_binary)
            .unwrap()
            .expect("expected frame");
        match frame {
            WireFrame::Text(line) => {
                let wire_bytes = line.len() + 1; // 含 '\n'
                let plain = sess.open(&line).expect("open v2 text frame");
                let msg = serde_json::from_str::<Msg>(&plain).expect("parse v2 json");
                (false, wire_bytes, msg)
            }
            WireFrame::Binary(body) => {
                let wire_bytes = 5 + body.len(); // 含 [0x02][4B LE len]
                let plain = sess.open_binary(&body).expect("open v3 binary frame");
                let msg = serde_json::from_str::<Msg>(&plain).expect("parse v3 json");
                (true, wire_bytes, msg)
            }
        }
    }

    #[test]
    fn binary_v3_host_and_client_bidirectional_live_sync_and_wire_probe() {
        let (host, port) = host_for_test(1, "宿主", "doc", "pw-v3-sync");
        let addr = format!("127.0.0.1:{port}");

        // 1. 真实 v3 客户端接入
        let guest = join(2, "新客户端".into(), "doc".into(), &addr, "pw-v3-sync".into()).unwrap();
        assert!(wait_event(&guest, 4000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());
        assert!(wait_event(&host, 4000, |e| matches!(e, NetEvent::PeerJoined(2, _, _))).is_some());

        // 2. 原始 v3 协议探针验证握手与切帧边界
        let (mut probe_w, mut probe_r, mut probe_sess, welcome) =
            raw_client_handshake(port, 3, "探针V3", "pw-v3-sync", "auth-probe-3", true);
        assert!(
            matches!(welcome, Msg::Welcome { binary_v3: true, .. }),
            "双方支持时 Welcome 必须回传 binary_v3=true，实际：{welcome:?}"
        );
        // Welcome 之后的 Snapshot 帧必须自动切换为 v3 二进制帧（首字节 0x02）
        let mut saw_snap = false;
        for _ in 0..8 {
            let (is_bin, _, msg) = read_decoded_frame(&mut probe_r, &mut probe_sess, true);
            assert!(is_bin, "Welcome 后的后续帧应使用 v3 二进制帧，实际收到 v2：{msg:?}");
            if matches!(msg, Msg::Snapshot { .. } | Msg::SnapshotChunk { last: true, .. }) {
                saw_snap = true;
                break;
            }
        }
        assert!(saw_snap, "探针应通过 v3 二进制帧收到初始快照");
        assert!(wait_event(&host, 4000, |e| matches!(e, NetEvent::PeerJoined(3, _, _))).is_some());

        // 3. 探针发 v3 二进制 Op → 宿主与真实 v3 客户端均收到
        let op_from_probe = Op {
            sheet: 0,
            row: 4,
            col: 2,
            value: ValueWire::Number(321.5),
            formula: None,
            ts: 10,
            site: 3,
        };
        let bin_frame = probe_sess.seal_binary(&serde_json::to_string(&Msg::Op(op_from_probe)).unwrap());
        assert_eq!(bin_frame[0], sheetcore::wire::BINARY_V3_MAGIC);
        write_binary_frame(&mut probe_w, &bin_frame).unwrap();

        assert!(
            wait_event(&host, 4000, |e| matches!(e, NetEvent::Ops(ops) if ops.iter().any(|o| o.row == 4 && o.col == 2 && o.value == ValueWire::Number(321.5)))).is_some(),
            "宿主应正确解开 v3 二进制 Op"
        );
        assert!(
            wait_event(&guest, 4000, |e| matches!(e, NetEvent::Ops(ops) if ops.iter().any(|o| o.row == 4 && o.col == 2 && o.value == ValueWire::Number(321.5)))).is_some(),
            "真实 v3 客户端应收到转发的 Op"
        );

        // 4. 真实 v3 客户端发 Op → 宿主与探针均收到（探针收到的是 v3 二进制帧）
        let op_from_guest = Op {
            sheet: 0,
            row: 7,
            col: 1,
            value: ValueWire::Text("双向V3同步".into()),
            formula: None,
            ts: 11,
            site: 2,
        };
        guest.broadcast_ops(vec![op_from_guest]);
        assert!(
            wait_event(&host, 4000, |e| matches!(e, NetEvent::Ops(ops) if ops.iter().any(|o| o.row == 7 && o.col == 1))).is_some(),
            "宿主应收到真实 v3 客户端发出的 Op"
        );
        let mut probe_got_guest_op = false;
        for _ in 0..8 {
            let (is_bin, _, msg) = read_decoded_frame(&mut probe_r, &mut probe_sess, true);
            assert!(is_bin, "v3 广播帧应为二进制格式");
            match msg {
                Msg::Op(o) if o.row == 7 && o.col == 1 => {
                    assert_eq!(o.value, ValueWire::Text("双向V3同步".into()));
                    probe_got_guest_op = true;
                    break;
                }
                Msg::SnapshotChunk { ops, .. } | Msg::Ops { ops }
                    if ops.iter().any(|o| o.row == 7 && o.col == 1) =>
                {
                    probe_got_guest_op = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(probe_got_guest_op, "v3 探针应通过二进制帧收到 guest 的编辑");
    }

    #[test]
    fn mixed_room_v2_legacy_and_v3_clients_interop_with_wire_reduction() {
        let (host, port) = host_for_test(1, "宿主", "doc", "pw-mix");
        let addr = format!("127.0.0.1:{port}");

        // 1. 模拟 1.74/1.75 旧版 v2 客户端（binary_v3 = false）
        let (mut v2_w, mut v2_r, mut v2_sess, v2_welcome) =
            raw_client_handshake(port, 10, "旧客户端V2", "pw-mix", "auth-v2-10", false);
        assert!(
            matches!(v2_welcome, Msg::Welcome { binary_v3: false, .. }),
            "旧客户端握手后 Welcome 必须回传 binary_v3=false，实际：{v2_welcome:?}"
        );
        // 旧客户端后续快照帧必须全部是 v2 文本行（allow_binary=false 严格校验不出现 0x02）
        loop {
            let (is_bin, _, msg) = read_decoded_frame(&mut v2_r, &mut v2_sess, false);
            assert!(!is_bin);
            if matches!(msg, Msg::Snapshot { .. }) {
                break;
            }
        }
        assert!(wait_event(&host, 4000, |e| matches!(e, NetEvent::PeerJoined(10, _, _))).is_some());

        // 2. 新版 v3 观测客户端（binary_v3 = true）+ 真实 join() v3 客户端
        let (mut v3_w, mut v3_r, mut v3_sess, v3_welcome) =
            raw_client_handshake(port, 11, "新观测V3", "pw-mix", "auth-v3-11", true);
        assert!(matches!(v3_welcome, Msg::Welcome { binary_v3: true, .. }));
        loop {
            let (is_bin, _, msg) = read_decoded_frame(&mut v3_r, &mut v3_sess, true);
            assert!(is_bin, "v3 观测端在 Welcome 后应只收二进制帧");
            if matches!(msg, Msg::Snapshot { .. }) {
                break;
            }
        }
        assert!(wait_event(&host, 4000, |e| matches!(e, NetEvent::PeerJoined(11, _, _))).is_some());

        let v3_real = join(12, "真实V3".into(), "doc".into(), &addr, "pw-mix".into()).unwrap();
        assert!(wait_event(&v3_real, 4000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());
        assert!(wait_event(&host, 4000, |e| matches!(e, NetEvent::PeerJoined(12, _, _))).is_some());

        // 3. 旧 v2 客户端发编辑 → 宿主、真实 v3 客户端、v3 观测端均能收到
        let op_from_v2 = Op {
            sheet: 0,
            row: 2,
            col: 3,
            value: ValueWire::Text("来自V2旧客户端".into()),
            formula: None,
            ts: 20,
            site: 10,
        };
        write_line(&mut v2_w, &v2_sess.seal(&serde_json::to_string(&Msg::Op(op_from_v2)).unwrap()))
            .unwrap();
        assert!(
            wait_event(&v3_real, 4000, |e| matches!(e, NetEvent::Ops(ops) if ops.iter().any(|o| o.row == 2 && o.col == 3))).is_some(),
            "真实 v3 客户端必须收到旧 v2 客户端的编辑"
        );

        // 4. 新 v3 客户端发编辑 → 宿主与旧 v2 客户端（严格 v2 文本帧）均能收到
        let op_from_v3 = Op {
            sheet: 0,
            row: 3,
            col: 4,
            value: ValueWire::Number(888.0),
            formula: None,
            ts: 21,
            site: 11,
        };
        write_binary_frame(
            &mut v3_w,
            &v3_sess.seal_binary(&serde_json::to_string(&Msg::Op(op_from_v3)).unwrap()),
        )
        .unwrap();
        let mut v2_got_v3_op = false;
        for _ in 0..12 {
            let (is_bin, _, msg) = read_decoded_frame(&mut v2_r, &mut v2_sess, false);
            assert!(!is_bin, "旧 v2 客户端只能收到 v2 文本行");
            if matches!(msg, Msg::Op(ref o) if o.row == 3 && o.col == 4 && o.value == ValueWire::Number(888.0))
            {
                v2_got_v3_op = true;
                break;
            }
        }
        assert!(v2_got_v3_op, "旧 v2 客户端必须收到来自 v3 客户端的编辑");

        // 5. 宿主广播大批量操作（同一份 Arc<PreparedPayload> 同时发给 v2 和 v3 混合房间），比对真实线缆字节缩减
        let batch: Vec<Op> = (0..200u32)
            .map(|r| Op {
                sheet: 0,
                row: 100 + r,
                col: 1,
                value: ValueWire::Number(1000.0 + r as f64 * 0.5),
                formula: None,
                ts: 100 + u64::from(r),
                site: 1,
            })
            .collect();
        host.broadcast_ops(batch);

        let mut v2_batch_bytes = 0usize;
        for _ in 0..16 {
            let (is_bin, bytes, msg) = read_decoded_frame(&mut v2_r, &mut v2_sess, false);
            assert!(!is_bin);
            if let Msg::SnapshotChunk { ops, .. } | Msg::Ops { ops } = msg {
                if ops.len() == 200 && ops[0].row == 100 {
                    v2_batch_bytes = bytes;
                    break;
                }
            }
        }
        let mut v3_batch_bytes = 0usize;
        for _ in 0..16 {
            let (is_bin, bytes, msg) = read_decoded_frame(&mut v3_r, &mut v3_sess, true);
            assert!(is_bin);
            if let Msg::SnapshotChunk { ops, .. } | Msg::Ops { ops } = msg {
                if ops.len() == 200 && ops[0].row == 100 {
                    v3_batch_bytes = bytes;
                    break;
                }
            }
        }
        assert!(v2_batch_bytes > 0 && v3_batch_bytes > 0, "v2/v3 双方均应收到完整 200 格广播批次");
        assert!(
            v3_batch_bytes * 100 <= v2_batch_bytes * 78,
            "混合房间同一 PreparedPayload 广播时 v3 线缆字节应比 v2 减少 ≥22%：v3={v3_batch_bytes}B vs v2={v2_batch_bytes}B"
        );
    }

    #[test]
    fn v3_client_falls_back_to_v2_against_legacy_v2_only_host() {
        // 模拟仅支持 v2 文本行的 1.74/1.75 旧版宿主（不识别 binary_v3，Welcome 中不含 binary_v3）
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock legacy host");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept client");
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut w = BufWriter::new(stream.try_clone().unwrap());
            let mut r = BufReader::new(stream);

            let salt = [7u8; 16];
            let room_salt = [9u8; 16];
            let nonce = 424242u64;
            let room_key = sheetcore::wire::RoomKey::derive("legacy-pw", &room_salt);
            let challenge = Msg::ChallengeSalt {
                nonce,
                salt: sheetcore::wire::b64_encode(&salt),
                room_salt: sheetcore::wire::b64_encode(&room_salt),
            };
            write_line(&mut w, &serde_json::to_string(&challenge).unwrap()).unwrap();

            let mut host_sess = sheetcore::wire::Session::host_side(&room_key, &salt);
            // 旧宿主仅支持 read_line_bounded（v2 文本行）
            let hello_line = read_line_bounded(&mut r, MAX_PREAUTH_LINE)
                .unwrap()
                .expect("hello line");
            let hello_plain = host_sess.open(&hello_line).expect("open v2 Hello");
            let hello: Msg = serde_json::from_str(&hello_plain).expect("parse Hello");
            assert!(
                matches!(hello, Msg::Hello { site: 5, binary_v3: true, .. }),
                "新客户端应在 Hello 中宣告 binary_v3=true"
            );

            // 旧宿主回传不含 binary_v3 字段的传统 Welcome + 空 Snapshot（v2 文本行）
            let legacy_welcome = r#"{"t":"Welcome","site":5,"doc":"doc","peers":[[1,"旧宿主",1]],"delta_sync":false,"batch_ops":true,"clock":1}"#;
            write_line(&mut w, &host_sess.seal(legacy_welcome)).unwrap();
            let snap = serde_json::to_string(&Msg::Snapshot { ops: Vec::new() }).unwrap();
            write_line(&mut w, &host_sess.seal(&snap)).unwrap();

            // 客户端收到旧 Welcome 后必须回退为 v2 文本行发送编辑，旧宿主用 read_line_bounded 应能无误解开
            let op_line = read_line_bounded(&mut r, MAX_LINE)
                .unwrap()
                .expect("client v2 op line");
            assert_ne!(
                op_line.as_bytes().first().copied(),
                Some(sheetcore::wire::BINARY_V3_MAGIC),
                "对旧宿主不得发送 v3 二进制帧"
            );
            let op_plain = host_sess.open(&op_line).expect("open client v2 op");
            let msg: Msg = serde_json::from_str(&op_plain).expect("parse client op");
            assert!(matches!(msg, Msg::Op(ref o) if o.row == 5 && o.col == 6));

            // 旧宿主发回一条 v2 Op，新客户端应能正常接收
            let reply_op = Msg::Op(Op {
                sheet: 0,
                row: 9,
                col: 1,
                value: ValueWire::Number(77.0),
                formula: None,
                ts: 30,
                site: 1,
            });
            write_line(&mut w, &host_sess.seal(&serde_json::to_string(&reply_op).unwrap())).unwrap();
            // 稍等确保客户端读完再结束连接
            std::thread::sleep(Duration::from_millis(200));
        });

        let addr = format!("127.0.0.1:{port}");
        let client = join(5, "新客户端".into(), "doc".into(), &addr, "legacy-pw".into()).unwrap();
        assert!(wait_event(&client, 5000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());
        client.broadcast_ops(vec![Op {
            sheet: 0,
            row: 5,
            col: 6,
            value: ValueWire::Number(12.5),
            formula: None,
            ts: 2,
            site: 5,
        }]);
        assert!(
            wait_event(&client, 5000, |e| matches!(e, NetEvent::Ops(ops) if ops.iter().any(|o| o.row == 9 && o.col == 1 && o.value == ValueWire::Number(77.0)))).is_some(),
            "新客户端对旧宿主应能正常收发 v2 文本帧"
        );
        drop(client);
        server.join().expect("mock legacy host thread should succeed");
    }

    #[cfg(windows)]
    #[test]
    fn concurrent_peer_update_pulls_both_complete_via_compact_slices_without_starving_ops() {
        let env = crate::test_env::TestEnv::new("concurrent-peer-update-queue");
        let official_kp = crate::update::cng::TestKeyPair::generate();
        env.sc("SC_UPDATE_PUBKEY", &official_kp.pubkey_hex);

        // 450 KB 安装包跨越 3 个 192KB 分片，验证双节点并发拉取排队不静默丢包且不阻塞实时 Op
        let raw_exe: Vec<u8> = (0..(450 * 1024)).map(|i| ((i * 17 + 13) & 0xFF) as u8).collect();
        let (manifest, full_exe) = crate::update::make_test_signed_release(
            &official_kp,
            "99.5",
            &raw_exe,
            "并发接力更新测试",
        );
        let seed_exe = env.path("seed-995-CELLAN.exe");
        std::fs::write(&seed_exe, &full_exe).unwrap();

        let (host, port) = host_for_test(1, "宿主", "concurrent-upd.xlsx", "pw");
        let addr = format!("127.0.0.1:{port}");
        let seeder = join(2, "供种成员".into(), "concurrent-upd.xlsx".into(), &addr, "pw".into()).unwrap();
        let peer_b = join(3, "成员B".into(), "concurrent-upd.xlsx".into(), &addr, "pw".into()).unwrap();
        let peer_c = join(4, "成员C".into(), "concurrent-upd.xlsx".into(), &addr, "pw".into()).unwrap();

        for p in [&seeder, &peer_b, &peer_c] {
            assert!(wait_event(p, 5000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());
        }
        seeder.set_update_bundle_for_test(seed_exe, manifest.clone());

        // 两个旧版成员几乎同时向同一供种成员发起拉取（旧实现第二个请求会被 AtomicBool 静默丢弃卡在 0%）
        peer_b.request_peer_update(2, &manifest).unwrap();
        peer_c.request_peer_update(2, &manifest).unwrap();

        // 供种期间宿主并发广播实时单元格操作，验证低优更新队列不饿死高优实时帧
        host.broadcast_op(Op {
            sheet: 0,
            row: 12,
            col: 3,
            value: ValueWire::Number(2026.0),
            formula: None,
            ts: 50,
            site: 1,
        });

        for (idx, receiver) in [(3, &peer_b), (4, &peer_c)] {
            let mut got_live_op = false;
            let mut got_update_ready = false;
            let deadline = std::time::Instant::now() + Duration::from_secs(12);
            while !(got_live_op && got_update_ready) {
                let rem = deadline.saturating_duration_since(std::time::Instant::now());
                let ev = receiver
                    .events
                    .recv_timeout(rem)
                    .unwrap_or_else(|_| panic!("peer {idx} timed out waiting for op+update"));
                match ev {
                    NetEvent::Ops(ops) if ops.iter().any(|o| o.row == 12 && o.col == 3) => {
                        got_live_op = true;
                    }
                    NetEvent::PeerUpdateReady { version, verified_exe } => {
                        assert_eq!(version, "99.5");
                        assert_eq!(std::fs::read(&verified_exe).unwrap(), full_exe);
                        got_update_ready = true;
                    }
                    NetEvent::PeerUpdateFailed(err) => {
                        panic!("peer {idx} update unexpectedly failed: {err}");
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn save_checkpoints_and_post_structural_delta_reconnect_avoid_full_snapshot() {
        let _env = crate::test_env::TestEnv::new("checkpoint-and-structural-delta");
        let (host, port) = host_for_test(1, "Host", "checkpoint-doc", "pw");
        let mut wb = sheetcore::Workbook::new_blank();
        for r in 0..1_200u32 {
            wb.sheet_mut(0).set(
                r,
                0,
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::Number(f64::from(r + 1)),
                    formula: None,
                },
            );
        }
        let mut host_peer = sheetcore::sync::Peer::new(1);
        host_peer.seed_workbook(&wb);
        host.set_base_hash("hash-v0".into());
        host.seed_snapshot(
            &host_peer,
            &wb,
            Cursor {
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

        // 宿主编辑 1 格并保存 v1 检查点，再插行（struct_rev = 1）并再编辑 1 格后保存 v2 检查点
        host.broadcast_op(Op {
            sheet: 0,
            row: 0,
            col: 1,
            value: ValueWire::Number(111.0),
            formula: None,
            ts: 2,
            site: 1,
        });
        host.record_save_checkpoint("hash-v1".into());
        let _ = host.broadcast_structural(0, &sheetcore::structural::Structural::InsertRows { at: 0, n: 2 });
        host.broadcast_op(Op {
            sheet: 0,
            row: 0,
            col: 1,
            value: ValueWire::Number(222.0),
            formula: None,
            ts: 3,
            site: 1,
        });
        host.record_save_checkpoint("hash-v2".into());
        host.broadcast_op(Op {
            sheet: 0,
            row: 1,
            col: 1,
            value: ValueWire::Number(333.0),
            formula: None,
            ts: 4,
            site: 1,
        });

        // 持旧保存版本 hash-v1 的同事入房：即使宿主已插行且又保存了 hash-v2，仍走 delta_sync=true 增量同步！
        let addr = format!("127.0.0.1:{port}");
        let late_v1 = join_with_base(
            2,
            "ColleagueV1".into(),
            "checkpoint-doc".into(),
            &addr,
            "pw".into(),
            "hash-v1".into(),
        )
        .unwrap();

        let mut saw_delta_welcome = false;
        let mut saw_struct_insert = false;
        let mut received_ops = 0usize;
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        loop {
            let ev = late_v1
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            match ev {
                NetEvent::Welcome { delta_sync, .. } => saw_delta_welcome = delta_sync,
                NetEvent::Structural {
                    sheet: 0,
                    op: sheetcore::structural::Structural::InsertRows { at: 0, n: 2 },
                    ..
                } => saw_struct_insert = true,
                NetEvent::Ops(ops) => received_ops += ops.len(),
                NetEvent::SnapshotDone => break,
                _ => {}
            }
        }
        assert!(saw_delta_welcome, "历史保存检查点 hash-v1 应命中增量入房");
        assert!(saw_struct_insert, "增量入房应自动补发检查点之后的插行结构变更");
        assert!(
            received_ops <= 5,
            "增量入房仅应收到少量变更格而非 1200 格全量基准，实际收到 {received_ops} 格"
        );

        // 验证在线断线重连期间发生结构插行（resume_struct_rev < struct_rev）仍走增量重连而非回退全量快照
        late_v1.disconnect_socket_for_test();
        assert!(wait_event(&late_v1, 4000, |e| matches!(e, NetEvent::Disconnected(_))).is_some());
        let _ = host.broadcast_structural(0, &sheetcore::structural::Structural::InsertCols { at: 0, n: 1 });
        host.broadcast_op(Op {
            sheet: 0,
            row: 5,
            col: 2,
            value: ValueWire::Number(999.0),
            formula: None,
            ts: 10,
            site: 1,
        });

        let mut reconnected_delta = false;
        let mut reconnected_insert_col = false;
        let mut reconnected_ops = 0usize;
        let reconn_deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let ev = late_v1
                .events
                .recv_timeout(reconn_deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            match ev {
                NetEvent::Welcome { delta_sync, .. } => reconnected_delta = delta_sync,
                NetEvent::Structural {
                    sheet: 0,
                    op: sheetcore::structural::Structural::InsertCols { at: 0, n: 1 },
                    ..
                } => reconnected_insert_col = true,
                NetEvent::Ops(ops) => reconnected_ops += ops.len(),
                NetEvent::SnapshotDone => break,
                _ => {}
            }
        }
        assert!(reconnected_delta, "断线期间发生插列后重连应继续走增量水位重连");
        assert!(reconnected_insert_col, "重连应补发断线窗口内的 InsertCols");
        assert!(reconnected_ops <= 4, "重连不应重发 1200 格基准，实际收到 {reconnected_ops} 格");
    }

    #[test]
    fn fast_snapshot_handoff_builds_columnar_workbook_off_ui_thread() {
        let _env = crate::test_env::TestEnv::new("fast-snapshot-handoff");
        let (host, port) = host_for_test(1, "Host", "handoff-doc", "pw");
        let mut wb = sheetcore::Workbook::new_blank();
        let mut bold = sheetcore::style::CellStyle::default();
        bold.bold = true;
        let bold_id = wb.styles.intern(bold);
        wb.sheet_mut(0).set_col_style(0, bold_id);
        wb.sheet_mut(0).set(
            0,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Number(50.0),
                formula: None,
            },
        );
        wb.sheet_mut(0).set(
            1,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Number(100.0),
                formula: Some("=A1*2".into()),
            },
        );
        let mut host_peer = sheetcore::sync::Peer::new(1);
        host_peer.seed_workbook(&wb);
        host.seed_snapshot(
            &host_peer,
            &wb,
            Cursor {
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
        host.broadcast_merge(0, (2, 0, 3, 1), true);

        let guest_net = join(
            2,
            "FastGuest".into(),
            "handoff-doc".into(),
            &format!("127.0.0.1:{port}"),
            "pw".into(),
        )
        .unwrap();
        guest_net.enable_fast_snapshot_handoff();

        let mut ui_ops_events = 0usize;
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        loop {
            let ev = guest_net
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            match ev {
                NetEvent::Ops(_) => ui_ops_events += 1,
                NetEvent::SnapshotDone => break,
                _ => {}
            }
        }
        assert_eq!(ui_ops_events, 0, "后台直接组装列式快照时不应向 UI 通道倾倒逐块 Ops");
        let prepared = guest_net
            .take_prepared_snapshot()
            .expect("SnapshotDone 时应可取走已组装完成的列式工作簿快照");
        let mut guest_wb = sheetcore::Workbook::new_blank();
        let mut guest_peer = sheetcore::sync::Peer::new(2);
        prepared.install(&mut guest_wb, Some(&mut guest_peer));

        assert_eq!(guest_wb.sheet(0).value(0, 0), sheetcore::model::Value::Number(50.0));
        assert_eq!(
            guest_wb.sheet(0).get(1, 0).unwrap().formula.as_deref(),
            Some("=A1*2")
        );
        assert!(guest_wb.styles.get(guest_wb.sheet(0).style_of(10, 0)).bold);
        assert_eq!(guest_wb.sheet(0).merge_at(2, 0), Some((2, 0, 3, 1)));
    }

    #[test]
    fn fast_snapshot_handoff_preserves_layout_freeze_themes_trailing_sheets_and_baseline_merges() {
        let _env = crate::test_env::TestEnv::new("fast-snapshot-metadata-preservation");
        let (host, port) = host_for_test(1, "Host", "preserve-doc", "pw");

        let mut host_wb = sheetcore::Workbook::new_blank();
        let mut italic = sheetcore::style::CellStyle::default();
        italic.italic = true;
        let italic_id = host_wb.styles.intern(italic);
        host_wb.sheet_mut(0).set_style(0, 0, italic_id);
        host_wb.sheet_mut(0).set(
            0,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Number(42.0),
                formula: None,
            },
        );
        let mut host_peer = sheetcore::sync::Peer::new(1);
        host_peer.seed_workbook(&host_wb);
        host.seed_snapshot(
            &host_peer,
            &host_wb,
            Cursor {
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
        // 宿主开房后编辑一格 (ts=5) + 拆并原文件合并格 (8,0,9,1) + 新增房间合并格 (2,0,3,1)
        host.broadcast_op(Op {
            sheet: 0,
            row: 0,
            col: 1,
            value: ValueWire::Number(84.0),
            formula: None,
            ts: 5,
            site: 1,
        });
        host.broadcast_merge(0, (8, 0, 9, 1), false);
        host.broadcast_merge(0, (2, 0, 3, 1), true);

        for (idx, seed_mode) in [true, false].into_iter().enumerate() {
            // 模拟加入者本地已打开共享盘原文件：含列宽、行高、冻结、CF、DV、表格、图表、原文件合并格、样式主题/quotePrefix 及尾表
            let mut guest_wb = sheetcore::Workbook::new_blank();
            let mut quoted_style = sheetcore::style::CellStyle::default();
            quoted_style.underline = true;
            let q_id = guest_wb.styles.intern(quoted_style);
            let q_theme = sheetcore::style_theme::XfTheme {
                font: Some(sheetcore::style_theme::ThemeRef { idx: 5, tint: -0.25 }),
                fill: None,
            };
            guest_wb.styles.set_theme(q_id, q_theme);
            guest_wb.styles.set_quote(q_id, true);
            let orig_id0 = guest_wb.sheets[0].identity;
            guest_wb.sheets[0].source_part = Some("xl/worksheets/sheet1.xml".into());
            guest_wb.sheets[0].cols.set(0, 180.0);
            guest_wb.sheets[0].rows.set(0, 32.0);
            guest_wb.sheets[0].freeze = Some((1, 2));
            guest_wb.sheets[0].cf.blocks.push(sheetcore::cf::CfBlock {
                ranges: vec![(0, 0, 20, 0)],
                rules: Vec::new(),
            });
            guest_wb.sheets[0].dv.push(sheetcore::dv::DataValidation {
                ranges: vec![(0, 0, 10, 0)],
                kind: sheetcore::dv::DvKind::List,
                op: sheetcore::dv::DvOp::Between,
                f1: "甲,乙,丙".into(),
                f2: String::new(),
                allow_blank: true,
                show_dropdown: true,
                error_style: sheetcore::dv::DvStyle::Stop,
                show_error: true,
                show_prompt: false,
                error_title: String::new(),
                error_msg: String::new(),
                prompt_title: String::new(),
                prompt_msg: String::new(),
            });
            guest_wb.sheets[0].tables.push(sheetcore::table::Table {
                name: "TblSales".into(),
                display: "TblSales".into(),
                range: (0, 0, 4, 2),
                columns: vec!["地区".into(), "销量".into(), "单价".into()],
                column_ids: vec![1, 2, 3],
                header_row: true,
                totals_row: false,
                ..Default::default()
            });
            guest_wb.sheets[0].charts.push(sheetcore::charts::Chart {
                kind: sheetcore::charts::ChartKind::Line,
                title: "趋势图".into(),
                legend: true,
                anchor: (0, 4, 12, 10),
                legend_pos: "r".into(),
                part: "xl/charts/chart1.xml".into(),
                drawing: "xl/drawings/drawing1.xml".into(),
                ..Default::default()
            });
            guest_wb.sheets[0].merge_range(5, 0, 6, 1); // 原文件基础合并格（宿主未动，必须保留）
            guest_wb.sheets[0].merge_range(8, 0, 9, 1); // 原文件基础合并格（宿主已拆并，必须移除）

            let mut tail = sheetcore::model::Sheet::new("Sheet2");
            tail.freeze = Some((2, 0));
            tail.merge_range(1, 1, 2, 2);
            tail.set(
                0,
                0,
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::Number(999.0),
                    formula: None,
                },
            );
            tail.set_style(0, 0, q_id);
            guest_wb.sheets.push(tail);

            let guest_net = join(
                10 + idx as u32,
                format!("Guest{idx}"),
                "preserve-doc".into(),
                &format!("127.0.0.1:{port}"),
                "pw".into(),
            )
            .unwrap();
            if seed_mode {
                guest_net.enable_fast_snapshot_handoff_with_styles(&guest_wb.styles);
            } else {
                guest_net.enable_fast_snapshot_handoff();
            }

            assert!(wait_event(&guest_net, 6000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());
            let prepared = guest_net.take_prepared_snapshot().expect("prepared snapshot");
            let mut guest_peer = sheetcore::sync::Peer::new(10 + idx as u32);
            prepared.install(&mut guest_wb, Some(&mut guest_peer));

            // 1. 宿主单元格、样式与 ts>1 LWW 水位准确写入
            assert_eq!(guest_wb.sheet(0).value(0, 0), sheetcore::model::Value::Number(42.0));
            assert_eq!(guest_wb.sheet(0).value(0, 1), sheetcore::model::Value::Number(84.0));
            assert!(guest_wb.styles.get(guest_wb.sheet(0).style_of(0, 0)).italic);
            assert!(
                !guest_peer.apply(Op {
                    sheet: 0,
                    row: 0,
                    col: 1,
                    value: ValueWire::Number(1.0),
                    formula: None,
                    ts: 3,
                    site: 99,
                }),
                "ts=3 过期乱序编辑必须被 ts=5 的快照增量格拒绝"
            );
            // 2. 非单元格布局、冻结、CF、DV、表格、图表、source_part、identity 与原样式表主题/quotePrefix 100% 保留
            assert_eq!(guest_wb.sheet(0).identity, orig_id0);
            assert_eq!(guest_wb.sheet(0).source_part.as_deref(), Some("xl/worksheets/sheet1.xml"));
            assert!((guest_wb.sheet(0).cols.pos(1) - 180.0).abs() < 0.05);
            assert!((guest_wb.sheet(0).rows.pos(1) - 32.0).abs() < 0.05);
            assert_eq!(guest_wb.sheet(0).freeze, Some((1, 2)));
            assert_eq!(guest_wb.sheet(0).cf.blocks.len(), 1);
            assert_eq!(guest_wb.sheet(0).dv.len(), 1);
            assert_eq!(guest_wb.sheet(0).tables.len(), 1);
            assert_eq!(guest_wb.sheet(0).charts.len(), 1);
            assert_eq!(guest_wb.styles.theme_of(q_id), q_theme, "StyleTable themes must survive");
            assert!(guest_wb.styles.quote_of(q_id), "StyleTable quotePrefix must survive");
            // 3. 原文件未动合并格保留、宿主拆并的原合并格移除、新增合并格生效
            assert_eq!(guest_wb.sheet(0).merge_at(5, 0), Some((5, 0, 6, 1)));
            assert_eq!(guest_wb.sheet(0).merge_at(8, 0), None);
            assert_eq!(guest_wb.sheet(0).merge_at(2, 0), Some((2, 0, 3, 1)));
            // 4. 快照未触及的尾表旧单元格与旧显式样式被清空，但其自身冻结窗格与基础合并格保留
            assert_eq!(guest_wb.sheet(1).value(0, 0), sheetcore::model::Value::Empty);
            assert_eq!(guest_wb.sheet(1).explicit_style_of(0, 0), None);
            assert_eq!(guest_wb.sheet(1).freeze, Some((2, 0)));
            assert_eq!(guest_wb.sheet(1).merge_at(1, 1), Some((1, 1, 2, 2)));
        }
    }

    #[test]
    fn broadcast_saved_guards_against_inflight_edits_and_supports_snapshot_cursor() {
        let env = crate::test_env::TestEnv::new("save-checkpoint-race-guard");
        let save_path = env.path("shared_budget.xlsx");
        let v1_bytes = b"xlsx-snapshot-v1-bytes";
        std::fs::write(&save_path, v1_bytes).unwrap();
        let v1_hash = hex::encode(Sha256::digest(v1_bytes));

        let (host, port) = host_for_test(1, "Host", "race-doc", "pw");
        let mut wb = sheetcore::Workbook::new_blank();
        wb.path = Some(save_path.clone());
        wb.sheet_mut(0).set(
            0,
            0,
            sheetcore::model::Cell {
                value: sheetcore::model::Value::Number(10.0),
                formula: None,
            },
        );
        let mut host_peer = sheetcore::sync::Peer::new(1);
        host_peer.seed_workbook(&wb);
        host.seed_snapshot(
            &host_peer,
            &wb,
            Cursor {
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

        // 1. 模拟未调 note_save_snapshot 时，后台写盘期间（clock=1）发生了并发新编辑（ts=2）：
        //    默认 broadcast_saved(1) 必须拒绝把已推进到 ts=2 的水位错绑到旧 v1 文件哈希上！
        host.broadcast_op(Op {
            sheet: 0,
            row: 4,
            col: 0,
            value: ValueWire::Number(555.0),
            formula: None,
            ts: 2,
            site: 1,
        });
        host.broadcast_saved(1);
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            host.host_watermark
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .find_checkpoint(&v1_hash)
                .is_none(),
            "写盘期间已发生新编辑时，未捕获快照游标的默认 broadcast_saved 不得错绑超前水位"
        );

        // 2. 模拟在拍保存快照时刻（clock=2）先调 note_save_snapshot(2) 锁存精确水位，
        //    随后写盘期间并发发生插行 + 新编辑（ts=3），再完成写盘并调 broadcast_saved_with_cursor：
        //    持 v2 文件入房的同事应命中 delta_sync=true 且完整收到写盘期间的插行与 ts=3 编辑！
        let snap_cursor = host.note_save_snapshot(2);
        assert!(snap_cursor.is_some());
        let v2_bytes = b"xlsx-snapshot-v2-at-clock-2";
        std::fs::write(&save_path, v2_bytes).unwrap();
        let v2_hash = hex::encode(Sha256::digest(v2_bytes));

        let _ = host.broadcast_structural(0, &sheetcore::structural::Structural::InsertRows { at: 0, n: 1 });
        host.broadcast_op(Op {
            sheet: 0,
            row: 9,
            col: 0,
            value: ValueWire::Number(777.0),
            formula: None,
            ts: 3,
            site: 1,
        });
        host.broadcast_saved_with_cursor(2, snap_cursor);

        let wait_cp = std::time::Instant::now() + Duration::from_secs(3);
        while host
            .host_watermark
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .find_checkpoint(&v2_hash)
            .is_none()
        {
            assert!(std::time::Instant::now() < wait_cp, "v2 checkpoint was not recorded");
            std::thread::sleep(Duration::from_millis(5));
        }

        let joiner = join_with_base(
            2,
            "ColleagueV2".into(),
            "race-doc".into(),
            &format!("127.0.0.1:{port}"),
            "pw".into(),
            v2_hash,
        )
        .unwrap();
        let mut saw_delta = false;
        let mut saw_inflight_struct = false;
        let mut saw_inflight_777 = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        loop {
            let ev = joiner
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            match ev {
                NetEvent::Welcome { delta_sync, .. } => saw_delta = delta_sync,
                NetEvent::Structural {
                    sheet: 0,
                    op: sheetcore::structural::Structural::InsertRows { at: 0, n: 1 },
                    ..
                } => saw_inflight_struct = true,
                NetEvent::Ops(ops) if ops.iter().any(|o| o.row == 9 && o.value == ValueWire::Number(777.0)) => {
                    saw_inflight_777 = true;
                }
                NetEvent::SnapshotDone => break,
                _ => {}
            }
        }
        assert!(saw_delta && saw_inflight_struct && saw_inflight_777);
    }

    #[test]
    fn cursor_sender_throttles_burst_to_30hz_preserving_edges_and_compact_prepared_fallback_works() {
        let _env = crate::test_env::TestEnv::new("cursor-30hz-throttle-and-fallback");
        let (host, port) = host_for_test(1, "Host", "cursor-doc", "pw");
        let guest = join(
            2,
            "Guest".into(),
            "cursor-doc".into(),
            &format!("127.0.0.1:{port}"),
            "pw".into(),
        )
        .unwrap();
        assert!(wait_event(&guest, 5000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());

        // 1. 模拟 120Hz 高频拖选连发 40 次光标：前缘第 1 帧立即发出，中间帧合并，尾随末帧 row=40 必达！
        for r in 1..=40u32 {
            guest.broadcast_cursor(Cursor {
                site: 2,
                name: "Guest".into(),
                color: 2,
                sheet: 0,
                row: r,
                col: 1,
                end_row: None,
                end_col: None,
            });
        }
        let mut seen_rows = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while seen_rows.last().copied() != Some(40) {
            let ev = host
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("trailing cursor row=40 should arrive");
            if let NetEvent::Cursor(c) = ev
                && c.site == 2
            {
                seen_rows.push(c.row);
            }
        }
        assert_eq!(seen_rows.first().copied(), Some(1), "leading-edge cursor must arrive immediately");
        assert_eq!(seen_rows.last().copied(), Some(40), "trailing-edge final cursor must never be dropped");
        assert!(
            seen_rows.len() <= 5,
            "40 rapid cursor updates should coalesce to <= 5 wire frames, got {seen_rows:?}"
        );

        // 2. 验证 GuestSend / HostOutput 的 write_prepared_opt 对未压缩与预压缩 compact_bin 负载在非 compact 通道（v2 文本与 v3 JSON）下的安全转码防御
        let room = sheetcore::wire::RoomKey::derive_with_iters("fb-pw", b"fb-salt", 4);
        let big_ops: Vec<Op> = (0..64u32)
            .map(|r| Op {
                sheet: 0,
                row: r,
                col: 1,
                value: ValueWire::Text("重复文本用于触发预压缩路径".into()),
                formula: None,
                ts: 10 + r as u64,
                site: 1,
            })
            .collect();
        let ops_msg = Msg::Ops { ops: big_ops };
        let compact_prep_compressed = sheetcore::wire::prepare_compact_payload(
            sheetcore::sync::encode_msg_compact(&ops_msg),
            true,
        );
        assert!(compact_prep_compressed.is_compact_bin());

        let wm_msg = Msg::Watermark { seq: 77, structure_revision: 3 };
        let compact_prep = sheetcore::wire::prepare_compact_payload(
            sheetcore::sync::encode_msg_compact(&wm_msg),
            false,
        );
        assert!(compact_prep.is_compact_bin());

        for binary_v3 in [false, true] {
            let (_ack_tx, ack_rx) = channel();
            let mut gs = GuestSend {
                sess: sheetcore::wire::Session::client_side(&room, b"fb-conn"),
                batch_ops: true,
                sort_permutation: true,
                binary_v3,
                compact_v3: false,
                scratch: Vec::new(),
                acks: Arc::new(Mutex::new(ack_rx)),
            };
            let mut host_recv = sheetcore::wire::Session::host_side(&room, b"fb-conn");
            let mut wire_buf = Vec::new();
            gs.write_prepared_opt(&mut wire_buf, &compact_prep, true)
                .expect("GuestSend compact prepared fallback must not panic or fail");
            let mut cursor = std::io::Cursor::new(wire_buf);
            let frame = read_wire_frame(&mut cursor, MAX_LINE, binary_v3)
                .unwrap()
                .unwrap();
            let plain = match frame {
                WireFrame::Text(line) => host_recv.open(&line).unwrap(),
                WireFrame::Binary(body) => host_recv.open_binary(&body).unwrap(),
            };
            let decoded: Msg = serde_json::from_str(&plain).unwrap();
            assert!(matches!(
                decoded,
                Msg::Watermark { seq: 77, structure_revision: 3 }
            ));

            // 同步验证 HostOutput::write_prepared_opt 对预压缩 compact_bin 的降级转码
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let laddr = listener.local_addr().unwrap();
            let cli_sock = TcpStream::connect(laddr).unwrap();
            let (srv_sock, _) = listener.accept().unwrap();
            let mut ho = HostOutput::new(
                BufWriter::new(srv_sock),
                sheetcore::wire::Session::host_side(&room, b"fb-host"),
            );
            ho.binary_v3 = binary_v3;
            ho.compact_v3 = false;
            ho.write_prepared_opt(&compact_prep_compressed, true)
                .expect("HostOutput compressed compact fallback must not panic or fail");
            drop(ho);
            let mut cli_reader = BufReader::new(cli_sock);
            let mut cli_recv = sheetcore::wire::Session::client_side(&room, b"fb-host");
            let hframe = read_wire_frame(&mut cli_reader, MAX_LINE, binary_v3)
                .unwrap()
                .unwrap();
            let hplain = match hframe {
                WireFrame::Text(line) => cli_recv.open(&line).unwrap(),
                WireFrame::Binary(body) => cli_recv.open_binary(&body).unwrap(),
            };
            let hdecoded: Msg = serde_json::from_str(&hplain).unwrap();
            assert!(matches!(hdecoded, Msg::Ops { ref ops } if ops.len() == 64));
        }
    }

    /// D3 (P1): 水位增量重连（delta_sync == true）必须补发断线期间的批注和行高/列宽等 SheetMetaOp。
    #[test]
    fn v180_d3_watermark_delta_reconnect_replays_offline_sheet_meta() {
        let _env = crate::test_env::TestEnv::new("v180_d3_wm_sheet_meta");
        let (host, port) = host_for_test(1, "Host", "meta-resume-doc", "pw");
        let mut wb = sheetcore::Workbook::new_blank();
        let mut peer = sheetcore::sync::Peer::new(1);
        wb.sheet_mut(0).set(
            0,
            0,
            sheetcore::Cell {
                value: sheetcore::Value::Number(1.0),
                formula: None,
            },
        );
        peer.seed_workbook(&wb);
        host.seed_snapshot(
            &peer,
            &wb,
            Cursor {
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

        let guest = join(
            2,
            "Guest".into(),
            "meta-resume-doc".into(),
            &format!("127.0.0.1:{port}"),
            "pw".into(),
        )
        .unwrap();
        let first_welcome = wait_event(&guest, 5000, |e| matches!(e, NetEvent::Welcome { .. }));
        assert!(matches!(first_welcome, Some(NetEvent::Welcome { delta_sync: false, .. })));
        assert!(wait_event(&guest, 5000, |e| matches!(e, NetEvent::SnapshotDone)).is_some());

        // 断线前先同步一条单元格更新，确认水位已推进
        host.broadcast_op(sheetcore::sync::Op {
            sheet: 0,
            row: 0,
            col: 1,
            value: sheetcore::sync::ValueWire::Number(42.0),
            formula: None,
            ts: 100,
            site: 1,
        });
        assert!(
            wait_event(&guest, 5000, |e| {
                matches!(e, NetEvent::Ops(ops) if ops.iter().any(|o| o.row == 0 && o.col == 1))
            })
            .is_some()
        );

        // 模拟访客短暂断网
        guest.disconnect_socket_for_test();
        assert!(wait_event(&guest, 5000, |e| matches!(e, NetEvent::Disconnected(_))).is_some());

        // 断网期间宿主新增批注并修改列宽、行高
        host.broadcast_sheet_meta(
            0,
            sheetcore::sync::SheetMetaKind::Note {
                row: 2,
                col: 3,
                author: "Host".into(),
                text: Some("断线期间批注".into()),
            },
        );
        host.broadcast_sheet_meta(
            0,
            sheetcore::sync::SheetMetaKind::ColWidths(vec![(3, 165.0)]),
        );
        host.broadcast_sheet_meta(
            0,
            sheetcore::sync::SheetMetaKind::RowHeights(vec![(2, 44.0)]),
        );

        // 访客自动水位增量重连（delta_sync == true）
        let second_welcome = wait_event(&guest, 8000, |e| matches!(e, NetEvent::Welcome { .. }));
        assert!(
            matches!(second_welcome, Some(NetEvent::Welcome { delta_sync: true, .. })),
            "短暂断线重连必须走水位增量同步: {second_welcome:?}"
        );

        let mut got_note = false;
        let mut got_col = false;
        let mut got_row = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        while !(got_note && got_col && got_row) && std::time::Instant::now() < deadline {
            if let Some(ev) = wait_event(&guest, 300, |_| true) {
                if let NetEvent::SheetMeta { sheet: 0, kind, .. } = ev {
                    match kind {
                        sheetcore::sync::SheetMetaKind::Note {
                            row: 2,
                            col: 3,
                            text: Some(t),
                            ..
                        } if t == "断线期间批注" => got_note = true,
                        sheetcore::sync::SheetMetaKind::ColWidths(items)
                            if items.iter().any(|&(c, w)| c == 3 && (w - 165.0).abs() < 0.5) =>
                        {
                            got_col = true;
                        }
                        sheetcore::sync::SheetMetaKind::RowHeights(items)
                            if items.iter().any(|&(r, h)| r == 2 && (h - 44.0).abs() < 0.5) =>
                        {
                            got_row = true;
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(got_note, "水位增量重连后必须收到断线期间的批注");
        assert!(got_col, "水位增量重连后必须收到断线期间的列宽修改");
        assert!(got_row, "水位增量重连后必须收到断线期间的行高修改");
    }
}

#[cfg(test)]
pub(crate) fn bench_collab_hotpaths(wm_entries: u32, peers: usize) -> (f64, f64, usize) {
    let mut wm = HostWatermark::default();
    for i in 0..wm_entries {
        wm.seq += 1;
        let key = (0, i, 0);
        wm.op_seqs.insert_cow(key, wm.seq);
        wm.op_log.push(wm.seq, key);
        wm.style_seqs.insert_cow(key, wm.seq);
        wm.style_log.push(wm.seq, key);
    }
    let t_wm = std::time::Instant::now();
    let cloned = wm.clone();
    let wm_clone_ms = t_wm.elapsed().as_secs_f64() * 1000.0;
    std::hint::black_box(cloned.seq);

    let key = sheetcore::wire::RoomKey::derive_with_iters("bench", b"salt", 1);
    let mut sessions: Vec<_> = (0..peers)
        .map(|i| sheetcore::wire::Session::host_side(&key, &i.to_le_bytes()))
        .collect();
    let mut payload = String::with_capacity(65_536);
    payload.push_str(r#"{"Op":{"ops":["#);
    for i in 0..800 {
        if i > 0 {
            payload.push(',');
        }
        payload.push_str(&format!(
            r#"{{"s":0,"r":{i},"c":1,"v":{{"N":{i}.5}},"ts":{i},"site":1}}"#
        ));
    }
    payload.push_str("]}}");

    let t_seal = std::time::Instant::now();
    let mut total_bytes = 0usize;
    let prepared = sheetcore::wire::prepare_payload(&payload, true);
    let mut scratch = Vec::with_capacity(prepared.payload_len() + 64);
    for s in &mut sessions {
        s.seal_prepared_binary_into(&prepared, &mut scratch);
        std::hint::black_box(scratch.as_slice());
        total_bytes += scratch.len();
    }
    let seal_ms = t_seal.elapsed().as_secs_f64() * 1000.0;
    (wm_clone_ms, seal_ms, total_bytes)
}
