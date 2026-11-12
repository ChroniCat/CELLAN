//! 局域网协作传输层：星形拓扑（宿主转发）+ 行分隔 JSON 帧。
//!
//! 安全模型：局域网内的「房间口令」。宿主连接后先发 challenge，客户端回
//! sha256(nonce || 口令)，口令本身不上线。这是防误连/随口令的门槛，不是加密通道；
//! 真要防窃听需要 TLS，等有明确需求再做。

use sha2::{Digest, Sha256};
use sheetcore::sync::{Cursor, Msg, Op};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
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
    Welcome { peers: Vec<(u32, String, u8)> },
    PeerJoined(u32, String, u8),
    PeerLeft(u32),
    Ops(Vec<Op>),
    Cursor(Cursor),
    /// 某格被批准编辑（含自己）
    LockOk { site: u32, name: String, sheet: usize, row: u32, col: u32 },
    /// 结构改动历史（加入时补）
    StructuralHistory(Vec<(usize, sheetcore::structural::Structural)>),
    /// 远端结构性改动
    Structural { sheet: usize, op: sheetcore::structural::Structural },
    /// 远端格式变更
    Styles(Vec<(usize, u32, u32, sheetcore::style::CellStyle)>),
    /// 远端合并变更
    Merge { site: u32, sheet: usize, rect: (u32, u32, u32, u32), on: bool },
    /// 自己被拒绝
    LockDeny { holder: u32, holder_name: String, sheet: usize, row: u32, col: u32 },
    /// 某格释放
    LockRelease { site: u32, sheet: usize, row: u32, col: u32 },
    Disconnected(String),
    Denied(String),
    Error(String),
}

enum Cmd {
    Broadcast(String),
    Stop,
}

pub struct Net {
    pub site: u32,
    cmd: Sender<Cmd>,
    pub events: Receiver<NetEvent>,
    pub hosting: bool,
    pub addr: String,
    alive: Arc<AtomicBool>,
    state: Option<Arc<Mutex<Vec<Op>>>>,
    /// 宿主自己也是参与者：本地租约表 + 事件入口
    host_locks: Option<Arc<Mutex<sheetcore::sync::LockTable>>>,
    host_styles:
        Option<Arc<Mutex<std::collections::HashMap<(usize, u32, u32), sheetcore::style::CellStyle>>>>,
    host_structs: Option<Arc<Mutex<Vec<(usize, sheetcore::structural::Structural)>>>>,
    evt_tx: Sender<NetEvent>,
    t0: std::time::Instant,
}

impl Drop for Net {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        let _ = self.cmd.send(Cmd::Stop);
    }
}

fn write_line(stream: &mut TcpStream, s: &str) -> std::io::Result<()> {
    stream.write_all(s.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}



/// 结构改动后，宿主的操作日志与样式表也要跟着搬家，
/// 否则后来加入的人会收到「旧坐标」的快照——插行后表头又跑回第 1 行。
fn shift_state(
    state: &Arc<Mutex<Vec<Op>>>,
    styles: &Arc<Mutex<std::collections::HashMap<(usize, u32, u32), sheetcore::style::CellStyle>>>,
    sheet: usize,
    op: &sheetcore::structural::Structural,
) {
    let map = |r: u32, c: u32| -> Option<(u32, u32)> {
        use sheetcore::structural::Structural as S;
        let axis = |v: u32, at: u32, n: u32, del: bool| -> Option<u32> {
            if del {
                if v < at {
                    Some(v)
                } else if v < at + n {
                    None
                } else {
                    Some(v - n)
                }
            } else if v >= at {
                Some(v + n)
            } else {
                Some(v)
            }
        };
        match op {
            S::InsertRows { at, n } => axis(r, *at, *n, false).map(|nr| (nr, c)),
            S::DeleteRows { at, n } => axis(r, *at, *n, true).map(|nr| (nr, c)),
            S::InsertCols { at, n } => axis(c, *at, *n, false).map(|nc| (r, nc)),
            S::DeleteCols { at, n } => axis(c, *at, *n, true).map(|nc| (r, nc)),
        }
    };
    {
        let mut st = state.lock().unwrap();
        let mut out = Vec::with_capacity(st.len());
        for o in st.iter() {
            if o.sheet == sheet {
                if let Some((nr, nc)) = map(o.row, o.col) {
                    let mut o2 = o.clone();
                    o2.row = nr;
                    o2.col = nc;
                    out.push(o2);
                }
            } else {
                out.push(o.clone());
            }
        }
        *st = out;
    }
    {
        let mut sm = styles.lock().unwrap();
        let old = std::mem::take(&mut *sm);
        for ((sh, r, c), v) in old {
            if sh == sheet {
                if let Some((nr, nc)) = map(r, c) {
                    sm.insert((sh, nr, nc), v);
                }
            } else {
                sm.insert((sh, r, c), v);
            }
        }
    }
}

/// 加密通道的写：有会话就封包
fn write_frame(stream: &mut TcpStream, sess: &mut Option<sheetcore::wire::Session>, s: &str) -> std::io::Result<()> {
    let frame = match sess {
        Some(se) => se.seal(s),
        None => s.to_string(),
    };
    write_line(stream, &frame)
}

/// 加密通道的读：有会话就解包，解不开直接返回错误（视为断线/被篡改）
fn open_frame(sess: &mut Option<sheetcore::wire::Session>, line: &str) -> Option<String> {
    match sess {
        Some(se) => se.open(line),
        None => Some(line.to_string()),
    }
}

pub fn proof(nonce: u64, password: &str) -> String {
    let mut h = Sha256::new();
    h.update(nonce.to_le_bytes());
    h.update(password.as_bytes());
    hex::encode(h.finalize())
}

fn random_nonce() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(1) as u64;
    n.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

impl Net {
    pub fn broadcast_op(&self, op: Op) {
        if let Some(st) = &self.state {
            st.lock().unwrap().push(op.clone());
        }
        if let Ok(s) = serde_json::to_string(&Msg::Op(op)) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    /// 请求编辑某格
    pub fn request_lock(&self, sheet: usize, row: u32, col: u32, name: &str) {
        // 宿主自己也是参与者：走本地租约表，否则「自己锁不住自己」
        if let Some(locks) = &self.host_locks {
            let now = self.t0.elapsed().as_millis() as u64;
            let outcome = locks.lock().unwrap().request(sheet, row, col, self.site, name, now);
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
                        name: name.to_string(),
                        sheet,
                        row,
                        col,
                    };
                    if let Ok(s) = serde_json::to_string(&msg) {
                        let _ = self.cmd.send(Cmd::Broadcast(format!("{s}
")));
                    }
                    let _ = self.evt_tx.send(NetEvent::LockOk {
                        site: self.site,
                        name: name.to_string(),
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
            let _ = self.cmd.send(Cmd::Broadcast(format!("{s}
")));
        }
    }

    pub fn release_lock(&self, sheet: usize, row: u32, col: u32) {
        if let Some(locks) = &self.host_locks {
            locks.lock().unwrap().release(sheet, row, col, self.site);
        }
        let msg = Msg::LockRelease { site: self.site, sheet, row, col };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(format!("{s}
")));
        }
    }

    /// 广播格式变更（宿主顺手记进样式表，供后来的人补）
    pub fn broadcast_style(&self, sheet: usize, items: Vec<(u32, u32, sheetcore::style::CellStyle)>) {
        if items.is_empty() {
            return;
        }
        if let Some(st) = &self.host_styles {
            let mut m = st.lock().unwrap();
            for (r, c, s) in items.iter() {
                m.insert((sheet, *r, *c), s.clone());
            }
        }
        let msg = Msg::StyleOp { site: self.site, sheet, ts: 0, items };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(format!("{s}
")));
        }
    }

    /// 广播合并/取消合并
    pub fn broadcast_merge(&self, sheet: usize, rect: (u32, u32, u32, u32), on: bool) {
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
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(format!("{s}
")));
        }
    }

    /// 广播结构性改动（宿主顺手把本地快照日志也搬家）
    pub fn broadcast_structural(&self, sheet: usize, op: &sheetcore::structural::Structural) {
        if let (Some(state), Some(st)) = (&self.state, &self.host_styles) {
            let before = state.lock().unwrap().len();
            shift_state(state, st, sheet, op);
            if std::env::var("SC_DBG").is_ok() {
                let g = state.lock().unwrap();
                let r0 = g.iter().filter(|o| o.row == 0).count();
                let r2 = g.iter().filter(|o| o.row == 2).count();
                eprintln!("[dbg] shift_state: {before} 条，row0={r0} row2={r2}");
            }
        }
        if let Some(h) = &self.host_structs {
            h.lock().unwrap().push((sheet, op.clone()));
        }
        let msg = Msg::Structural { site: self.site, sheet, op: op.clone() };
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Broadcast(format!("{s}
")));
        }
    }

    pub fn broadcast_cursor(&self, cur: Cursor) {
        if let Ok(s) = serde_json::to_string(&Msg::CursorMsg(cur)) {
            let _ = self.cmd.send(Cmd::Broadcast(s));
        }
    }

    pub fn shutdown(&self) {
        self.alive.store(false, Ordering::SeqCst);
        let _ = self.cmd.send(Cmd::Stop);
    }
}

/// 启动宿主：监听端口，校验口令，转发所有操作
pub fn host(
    site: u32,
    name: String,
    doc: String,
    port: u16,
    password: String,
) -> std::io::Result<Net> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (evt_tx, evt_rx) = channel::<NetEvent>();
    let alive = Arc::new(AtomicBool::new(true));

    let peers: Arc<Mutex<HashMap<u32, Sender<String>>>> = Arc::new(Mutex::new(HashMap::new()));
    let peer_names: Arc<Mutex<HashMap<u32, (String, u8)>>> = Arc::new(Mutex::new(HashMap::new()));
    // 宿主自己也是参与者：名单里没有它，新加入的同事就只看到自己一个人（"显示不出另一台在线"）
    peer_names
        .lock()
        .unwrap()
        .insert(site, (name.clone(), (site % 7) as u8));
    let state: Arc<Mutex<Vec<Op>>> = Arc::new(Mutex::new(Vec::new()));

    // 广播分发
    {
        let peers = peers.clone();
        let alive = alive.clone();
        std::thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                if !alive.load(Ordering::SeqCst) {
                    break;
                }
                match cmd {
                    Cmd::Broadcast(s) => {
                        let map = peers.lock().unwrap();
                        for tx in map.values() {
                            let _ = tx.send(s.clone());
                        }
                    }
                    Cmd::Stop => break,
                }
            }
        });
    }

    // 编辑租约（宿主仲裁，同一格只批一个）
    // 样式表：新加入的人要靠它补齐格式（Op 里只有值）
    let styles: Arc<Mutex<std::collections::HashMap<(usize, u32, u32), sheetcore::style::CellStyle>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let structs: Arc<Mutex<Vec<(usize, sheetcore::structural::Structural)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let structs_for_self = structs.clone();
    let locks: Arc<Mutex<sheetcore::sync::LockTable>> =
        Arc::new(Mutex::new(sheetcore::sync::LockTable::new(20_000)));
    let locks_for_self = locks.clone();
    let styles_for_self = styles.clone();
    let evt_tx_self = evt_tx.clone();
    let t0 = std::time::Instant::now();
    let now_ms = {
        let t0 = t0;
        move || t0.elapsed().as_millis() as u64
    };

    // 租约巡检：断线/崩溃的人 20 秒后自动放锁
    {
        let locks = locks.clone();
        let peers = peers.clone();
        let alive = alive.clone();
        let now_ms = now_ms;
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            if !alive.load(Ordering::SeqCst) {
                break;
            }
            let dead = locks.lock().unwrap().expire(now_ms());
            for (sheet, row, col, site) in dead {
                let line = serde_json::to_string(&Msg::LockRelease { site, sheet, row, col })
                    .unwrap();
                for ptx in peers.lock().unwrap().values() {
                    let _ = ptx.send(line.clone());
                }
            }
        });
    }

    let host_site = site;
    // accept 循环
    {
        let peers = peers.clone();
        let peer_names = peer_names.clone();
        let state = state.clone();
        let evt_tx = evt_tx.clone();
        let doc_name = doc.clone();
        let password = password.clone();
        let alive = alive.clone();
        let locks_outer = locks.clone();
        let styles_outer = styles.clone();
        let structs_outer = structs.clone();
        let now_ms_outer = now_ms;
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                if !alive.load(Ordering::SeqCst) {
                    break;
                }
                let peers = peers.clone();
                let peer_names = peer_names.clone();
                let state = state.clone();
                let evt_tx = evt_tx.clone();
                let doc_name = doc_name.clone();
                let password = password.clone();
                let locks = locks_outer.clone();
                let styles = styles_outer.clone();
                let structs = structs_outer.clone();
                let now_ms = now_ms_outer;
                let my_site = host_site;
                std::thread::spawn(move || {
                    let mut write_half = match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let reader = BufReader::new(stream);
                    let (tx, rx) = channel::<String>();
                    let (tx_key, rx_key) = channel::<Option<sheetcore::wire::Session>>();
                    let mut write_sess: Option<sheetcore::wire::Session> = None;
                    std::thread::spawn(move || {
                        // 第一帧恒为明文（Challenge）——密钥可能比它先到队列里，
                        // 不能靠「有没有会话」来判断该不该加密
                        let mut sent_first = false;
                        while let Ok(s) = rx.recv() {
                            while let Ok(k) = rx_key.try_recv() {
                                write_sess = k;
                            }
                            // 前缀 \x01 = 这一帧强制明文（挑战、错误口令提示）
                            let (plain, body) = match s.strip_prefix('\u{1}') {
                                Some(rest) => (true, rest.to_string()),
                                None => (false, s),
                            };
                            if plain || !sent_first {
                                sent_first = true;
                                if write_line(&mut write_half, &body).is_err() {
                                    break;
                                }
                                continue;
                            }
                            if write_frame(&mut write_half, &mut write_sess, &body).is_err() {
                                break;
                            }
                        }
                    });

                    // 1) 挑战（口令非空时带 salt，双方据此派生密钥）
                    let nonce = random_nonce();
                    let salt: Vec<u8> = (0..16).map(|i| (nonce.rotate_left(i as u32) & 0xff) as u8).collect();
                    let mut sess: Option<sheetcore::wire::Session> = None;
                    let challenge = if password.is_empty() {
                        Msg::Challenge { nonce }
                    } else {
                        sess = Some(sheetcore::wire::Session::host_side(&password, &salt));
                        Msg::ChallengeSalt { nonce, salt: sheetcore::wire::b64_encode(&salt) }
                    };
                    let _ = tx.send(serde_json::to_string(&challenge).unwrap());
                    let _ = tx_key.send(sess.as_ref().map(|x| x.fork_sender()));

                    let mut hello_site: Option<u32> = None;
                    for line in reader.lines() {
                        let Ok(line) = line else { break };
                        let Some(line) = open_frame(&mut sess, &line) else {
                            // 解不开 = 口令不对或帧被改过。明文回一句再断，
                            // 让对方看到「口令不正确」而不是只有「连接中断」
                            if sess.is_some() {
                                let deny = serde_json::to_string(&Msg::Denied {
                                    reason: "口令不正确".into(),
                                })
                                .unwrap();
                                let _ = tx.send(format!("\u{1}{deny}"));
                                std::thread::sleep(Duration::from_millis(150));
                            }
                            break;
                        };
                        let Ok(msg) = serde_json::from_str::<Msg>(&line) else { continue };
                        match msg {
                            Msg::Hello { site, name, proof: got, doc: _ } => {
                                if !password.is_empty() && got != proof(nonce, &password) {
                                    let _ = tx.send(
                                        serde_json::to_string(&Msg::Denied {
                                            reason: "口令不正确".into(),
                                        })
                                        .unwrap(),
                                    );
                                    break;
                                }
                                hello_site = Some(site);
                                let color = (site % 7) as u8;
                                let known: Vec<(u32, String, u8)> = peer_names
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .map(|(s, (n, c))| (*s, n.clone(), *c))
                                    .collect();
                                let _ = tx.send(
                                    serde_json::to_string(&Msg::Welcome {
                                        site,
                                        doc: doc_name.clone(),
                                        peers: known,
                                    })
                                    .unwrap(),
                                );
                                {
                                    let hist = structs.lock().unwrap().clone();
                                    if std::env::var("SC_DBG").is_ok() {
                                        eprintln!("[dbg] 下发结构历史 {} 条", hist.len());
                                    }
                                    if !hist.is_empty() {
                                        let _ = tx.send(
                                            serde_json::to_string(&Msg::StructuralLog {
                                                items: hist,
                                            })
                                            .unwrap(),
                                        );
                                    }
                                }
                                {
                                    // 样式先补（新人有格式，后面再收值）
                                    let items: Vec<(usize, u32, u32, sheetcore::style::CellStyle)> =
                                        styles
                                            .lock()
                                            .unwrap()
                                            .iter()
                                            .map(|((sh, r, c), st)| (*sh, *r, *c, st.clone()))
                                            .collect();
                                    if !items.is_empty() {
                                        let _ = tx.send(
                                            serde_json::to_string(&Msg::StyleSnapshot { items })
                                                .unwrap(),
                                        );
                                    }
                                }
                                {
                                    // 快照分片：15 万条操作一次发，单帧几 MB，
                                    // 客户端要一次性反序列化；分 4000 条一片更稳
                                    let snap = state.lock().unwrap().clone();
                                    let chunk = 4000;
                                    if snap.is_empty() {
                                        let _ = tx.send(
                                            serde_json::to_string(&Msg::Snapshot { ops: snap })
                                                .unwrap(),
                                        );
                                    } else {
                                        let n = snap.len();
                                        let mut i = 0;
                                        while i < n {
                                            let end = (i + chunk).min(n);
                                            let part = snap[i..end].to_vec();
                                            let msg = if end == n {
                                                Msg::Snapshot { ops: part }
                                            } else {
                                                Msg::SnapshotChunk { ops: part, last: false }
                                            };
                                            let _ = tx.send(serde_json::to_string(&msg).unwrap());
                                            i = end;
                                        }
                                    }
                                }
                                peers.lock().unwrap().insert(site, tx.clone());
                                peer_names.lock().unwrap().insert(site, (name.clone(), color));
                                let _ = evt_tx.send(NetEvent::PeerJoined(site, name.clone(), color));
                                let hello = serde_json::to_string(&Msg::Hello {
                                    site,
                                    name,
                                    doc: doc_name.clone(),
                                    proof: String::new(),
                                })
                                .unwrap();
                                for (s, ptx) in peers.lock().unwrap().iter() {
                                    if *s != site {
                                        let _ = ptx.send(hello.clone());
                                    }
                                }
                            }
                            Msg::Op(op) => {
                                state.lock().unwrap().push(op.clone());
                                let line = serde_json::to_string(&Msg::Op(op.clone())).unwrap();
                                for (s, ptx) in peers.lock().unwrap().iter() {
                                    if Some(*s) != hello_site {
                                        let _ = ptx.send(line.clone());
                                    }
                                }
                                let _ = evt_tx.send(NetEvent::Ops(vec![op]));
                            }
                            Msg::CursorMsg(c) => {
                                let line =
                                    serde_json::to_string(&Msg::CursorMsg(c.clone())).unwrap();
                                for (s, ptx) in peers.lock().unwrap().iter() {
                                    if Some(*s) != hello_site {
                                        let _ = ptx.send(line.clone());
                                    }
                                }
                                let _ = evt_tx.send(NetEvent::Cursor(c));
                            }
                            Msg::Structural { site: _site, sheet, op } => {
                                shift_state(&state, &styles, sheet, &op);
                                structs.lock().unwrap().push((sheet, op.clone()));
                                let line = serde_json::to_string(&Msg::Structural {
                                    site: 0,
                                    sheet,
                                    op: op.clone(),
                                })
                                .unwrap();
                                for (s, ptx) in peers.lock().unwrap().iter() {
                                    if Some(*s) != hello_site {
                                        let _ = ptx.send(line.clone());
                                    }
                                }
                                let _ = evt_tx.send(NetEvent::Structural { sheet, op });
                            }
                            Msg::StyleOp { site: _site, sheet, ts: _ts, items } => {
                                {
                                    let mut st = styles.lock().unwrap();
                                    for (r, c, s) in items.iter() {
                                        st.insert((sheet, *r, *c), s.clone());
                                    }
                                }
                                let line = serde_json::to_string(&Msg::StyleOp {
                                    site: 0,
                                    sheet,
                                    ts: 0,
                                    items: items.clone(),
                                })
                                .unwrap();
                                for (s, ptx) in peers.lock().unwrap().iter() {
                                    if Some(*s) != hello_site {
                                        let _ = ptx.send(line.clone());
                                    }
                                }
                                let ev: Vec<(usize, u32, u32, sheetcore::style::CellStyle)> =
                                    items.iter().map(|(r, c, s)| (sheet, *r, *c, s.clone())).collect();
                                let _ = evt_tx.send(NetEvent::Styles(ev));
                            }
                            Msg::MergeOp { site: _site, sheet, ts: _ts, r1, c1, r2, c2, on } => {
                                let line = serde_json::to_string(&Msg::MergeOp {
                                    site: 0,
                                    sheet,
                                    ts: 0,
                                    r1,
                                    c1,
                                    r2,
                                    c2,
                                    on,
                                })
                                .unwrap();
                                for (s, ptx) in peers.lock().unwrap().iter() {
                                    if Some(*s) != hello_site {
                                        let _ = ptx.send(line.clone());
                                    }
                                }
                                let _ = evt_tx.send(NetEvent::Merge {
                                    site: 0,
                                    sheet,
                                    rect: (r1, c1, r2, c2),
                                    on,
                                });
                            }
                            Msg::LockReq { site, name, sheet, row, col } => {
                                let outcome = locks.lock().unwrap().request(
                                    sheet, row, col, site, &name, now_ms(),
                                );
                                match outcome {
                                    sheetcore::sync::LockOutcome::Denied { holder, name: hn } => {
                                        let line = serde_json::to_string(&Msg::LockDeny {
                                            site,
                                            sheet,
                                            row,
                                            col,
                                            holder,
                                            holder_name: hn,
                                        })
                                        .unwrap();
                                        let _ = tx.send(line);
                                        if site == my_site {
                                            let _ = evt_tx.send(NetEvent::LockDeny {
                                                holder,
                                                holder_name: name,
                                                sheet,
                                                row,
                                                col,
                                            });
                                        }
                                    }
                                    _ => {
                                        let line = serde_json::to_string(&Msg::LockOk {
                                            site,
                                            name,
                                            sheet,
                                            row,
                                            col,
                                        })
                                        .unwrap();
                                        for (s, ptx) in peers.lock().unwrap().iter() {
                                            if Some(*s) != hello_site {
                                                let _ = ptx.send(line.clone());
                                            }
                                        }
                                        let _ = tx.send(line);
                                        let _ = evt_tx.send(NetEvent::LockOk {
                                            site,
                                            name: locks
                                                .lock()
                                                .unwrap()
                                                .holder(sheet, row, col)
                                                .map(|h| h.name.clone())
                                                .unwrap_or_default(),
                                            sheet,
                                            row,
                                            col,
                                        });
                                    }
                                }
                            }
                            Msg::LockRelease { site, sheet, row, col } => {
                                let ok = locks.lock().unwrap().release(sheet, row, col, site);
                                if ok {
                                    let line = serde_json::to_string(&Msg::LockRelease {
                                        site, sheet, row, col,
                                    })
                                    .unwrap();
                                    for (s, ptx) in peers.lock().unwrap().iter() {
                                        if Some(*s) != hello_site {
                                            let _ = ptx.send(line.clone());
                                        }
                                    }
                                    let _ = evt_tx.send(NetEvent::LockRelease {
                                        site, sheet, row, col,
                                    });
                                }
                            }
                            Msg::Bye { site } => {
                                peers.lock().unwrap().remove(&site);
                                peer_names.lock().unwrap().remove(&site);
                                let _ = evt_tx.send(NetEvent::PeerLeft(site));
                                break;
                            }
                            _ => {}
                        }
                    }
                    if let Some(site) = hello_site {
                        peers.lock().unwrap().remove(&site);
                        peer_names.lock().unwrap().remove(&site);
                        // 人走了，锁必须放掉，否则格子会一直灰着
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
                            for ptx in peers.lock().unwrap().values() {
                                let _ = ptx.send(line.clone());
                            }
                        }
                        let _ = evt_tx.send(NetEvent::PeerLeft(site));
                    }
                });
            }
        });
    }

    Ok(Net {
        site,
        cmd: cmd_tx,
        events: evt_rx,
        hosting: true,
        addr: format!("0.0.0.0:{port}"),
        alive,
        state: Some(state),
        host_locks: Some(locks_for_self),
        host_styles: Some(styles_for_self),
        host_structs: Some(structs_for_self),
        evt_tx: evt_tx_self,
        t0,
    })
}

/// 加入宿主：断开后自动重连（指数退避，最长 10 秒）
pub fn join(
    site: u32,
    name: String,
    doc: String,
    addr: &str,
    password: String,
) -> std::io::Result<Net> {
    // 先同步试连一次，连不上立刻报错（避免用户以为成功了）。
    // 必须带超时：对端防火墙丢包时默认 connect 会卡 20 秒，界面像死了。
    let sock: std::net::SocketAddr = addr.parse().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("地址格式不对：{addr}"))
    })?;
    let first = TcpStream::connect_timeout(&sock, Duration::from_secs(3))?;
    drop(first);

    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (evt_tx, evt_rx) = channel::<NetEvent>();
    let evt_tx_clone = evt_tx.clone();
    let alive = Arc::new(AtomicBool::new(true));
    let writer: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
    // 加密会话（客户端侧）：读写共用一份，方向位/计数器已区分
    let sess_shared: Arc<Mutex<Option<sheetcore::wire::Session>>> = Arc::new(Mutex::new(None));

    // 写线程
    {
        let writer = writer.clone();
        let alive = alive.clone();
        let sess_shared = sess_shared.clone();
        std::thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                if !alive.load(Ordering::SeqCst) {
                    break;
                }
                match cmd {
                    Cmd::Broadcast(s) => {
                        let frame = {
                            let mut g = sess_shared.lock().unwrap();
                            match g.as_mut() {
                                Some(se) => se.seal(&s),
                                None => s.clone(),
                            }
                        };
                        if let Some(w) = writer.lock().unwrap().as_mut() {
                            let _ = write_line(w, &frame);
                        }
                    }
                    Cmd::Stop => break,
                }
            }
        });
    }

    // 监督线程：连接 → 握手 → 读循环 → 断开 → 退避重连
    {
        let addr = addr.to_string();
        let name = name.clone();
        let doc = doc.clone();
        let password = password.clone();
        let evt_tx = evt_tx.clone();
        let writer = writer.clone();
        let alive = alive.clone();
        std::thread::spawn(move || {
            let mut backoff = 1u64;
            while alive.load(Ordering::SeqCst) {
                let connected = match TcpStream::connect(&addr) {
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
                let mut write_half = match connected.try_clone() {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let reader = BufReader::new(connected);
                let mut denied = false;
                let mut got_welcome = false;

                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    let opened = {
                        let mut g = sess_shared.lock().unwrap();
                        match g.as_mut() {
                            Some(se) => se.open(&line),
                            None => Some(line.clone()),
                        }
                    };
                    let line = match opened {
                        Some(l) => l,
                        None => {
                            // 明文帧（例如「口令不正确」的提示）解不开，直接当 JSON 认
                            if serde_json::from_str::<Msg>(&line).is_ok() {
                                line
                            } else {
                                break;
                            }
                        }
                    };
                    let Ok(msg) = serde_json::from_str::<Msg>(&line) else { continue };
                    match msg {
                        Msg::ChallengeSalt { nonce, salt } => {
                            let raw = sheetcore::wire::b64_decode(&salt).unwrap_or_default();
                            *sess_shared.lock().unwrap() =
                                Some(sheetcore::wire::Session::client_side(&password, &raw));
                            let hello = Msg::Hello {
                                site,
                                name: name.clone(),
                                doc: doc.clone(),
                                proof: proof(nonce, &password),
                            };
                            {
                                let mut g = sess_shared.lock().unwrap();
                                let _ = write_frame(
                                    &mut write_half,
                                    &mut g,
                                    &serde_json::to_string(&hello).unwrap(),
                                );
                            }
                        }
                        Msg::Challenge { nonce } => {
                            let hello = Msg::Hello {
                                site,
                                name: name.clone(),
                                doc: doc.clone(),
                                proof: proof(nonce, &password),
                            };
                            let mut g = sess_shared.lock().unwrap();
                            let _ = write_frame(
                                &mut write_half,
                                &mut g,
                                &serde_json::to_string(&hello).unwrap(),
                            );
                            drop(g);
                        }
                        Msg::Welcome { peers, .. } => {
                            got_welcome = true;
                            if let Ok(clone) = write_half.try_clone() {
                                *writer.lock().unwrap() = Some(clone);
                            }
                            let _ = evt_tx.send(NetEvent::Connected);
                            let _ = evt_tx.send(NetEvent::Welcome { peers });
                        }
                        Msg::Denied { reason } => {
                            let _ = evt_tx.send(NetEvent::Denied(reason));
                            denied = true;
                            break;
                        }
                        Msg::SnapshotChunk { ops, last } => {
                            let _ = evt_tx.send(NetEvent::Ops(ops));
                            if last {
                                // 收尾片其实用 Snapshot 发，这里兜底
                            }
                        }
                        Msg::Snapshot { ops } => {
                            let _ = evt_tx.send(NetEvent::Ops(ops));
                        }
                        Msg::Op(op) => {
                            let _ = evt_tx.send(NetEvent::Ops(vec![op]));
                        }
                        Msg::StructuralLog { items } => {
                            let _ = evt_tx.send(NetEvent::StructuralHistory(items));
                        }
                        Msg::Structural { sheet, op, .. } => {
                            let _ = evt_tx.send(NetEvent::Structural { sheet, op });
                        }
                        Msg::StyleOp { sheet, items, .. } => {
                            let ev: Vec<(usize, u32, u32, sheetcore::style::CellStyle)> =
                                items.iter().map(|(r, c, s)| (sheet, *r, *c, s.clone())).collect();
                            let _ = evt_tx.send(NetEvent::Styles(ev));
                        }
                        Msg::StyleSnapshot { items } => {
                            let _ = evt_tx.send(NetEvent::Styles(items));
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
                        Msg::LockRelease { site, sheet, row, col } => {
                            let _ = evt_tx.send(NetEvent::LockRelease { site, sheet, row, col });
                        }
                        Msg::CursorMsg(c) => {
                            let _ = evt_tx.send(NetEvent::Cursor(c));
                        }
                        Msg::Hello { site: s, name: n, .. } => {
                            let _ = evt_tx.send(NetEvent::PeerJoined(s, n, (s % 7) as u8));
                        }
                        Msg::Bye { site: s } => {
                            let _ = evt_tx.send(NetEvent::PeerLeft(s));
                        }
                        _ => {}
                    }
                }
                *writer.lock().unwrap() = None;
                *sess_shared.lock().unwrap() = None;
                if denied {
                    break;
                }
                let hint = if password.is_empty() || got_welcome {
                    "连接中断".to_string()
                } else {
                    "连接中断（房间有口令，先确认口令是否正确）".to_string()
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
        addr: addr.to_string(),
        alive,
        state: None,
        host_locks: None,
        host_styles: None,
        host_structs: None,
        evt_tx: evt_tx_clone,
        t0: std::time::Instant::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheetcore::sync::ValueWire;

    fn op(row: u32, n: f64, ts: u64, site: u32) -> Op {
        Op { sheet: 0, row, col: 0, value: ValueWire::Number(n), formula: None, ts, site }
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
    fn host_and_client_exchange_ops() {
        let port = 17881;
        let host_net = host(1, "A".into(), "doc".into(), port, String::new()).expect("host");
        host_net.broadcast_op(op(0, 1.0, 1, 1));

        let client = join(
            2,
            "B".into(),
            "doc".into(),
            &format!("127.0.0.1:{port}"),
            String::new(),
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
    fn wrong_password_is_denied() {
        let port = 17882;
        let _host = host(1, "A".into(), "doc".into(), port, "secret".into()).expect("host");
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
        let port = 17883;
        let _host = host(1, "A".into(), "doc".into(), port, "secret".into()).expect("host");
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
}

#[cfg(test)]
mod presence_tests {
    use super::*;

    /// 新加入的人必须能在 Welcome 里看到宿主（以前宿主不登记自己，
    /// 刚加入的人只看到自己一个人 = 用户说的「显示不出另一台在线」）。
    #[test]
    fn welcome_lists_the_host_as_a_peer() {
        let port: u16 = 7987;
        let host_net = match host(11, "宿主".into(), "doc".into(), port, String::new()) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("跳过：端口 {port} 起不来（{e}）");
                return;
            }
        };
        std::thread::sleep(Duration::from_millis(300));
        let client = join(
            22,
            "同事".into(),
            "doc".into(),
            &format!("127.0.0.1:{port}"),
            String::new(),
        )
        .expect("应能连上宿主");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got: Option<Vec<(u32, String, u8)>> = None;
        while std::time::Instant::now() < deadline {
            match client.events.recv_timeout(Duration::from_millis(200)) {
                Ok(NetEvent::Welcome { peers }) => {
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
}
