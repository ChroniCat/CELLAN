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
    let locks: Arc<Mutex<sheetcore::sync::LockTable>> =
        Arc::new(Mutex::new(sheetcore::sync::LockTable::new(20_000)));
    let locks_for_self = locks.clone();
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
                let now_ms = now_ms_outer;
                let my_site = host_site;
                std::thread::spawn(move || {
                    let mut write_half = match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let reader = BufReader::new(stream);
                    let (tx, rx) = channel::<String>();
                    std::thread::spawn(move || {
                        while let Ok(s) = rx.recv() {
                            if write_line(&mut write_half, &s).is_err() {
                                break;
                            }
                        }
                    });

                    // 1) 挑战
                    let nonce = random_nonce();
                    let _ =
                        tx.send(serde_json::to_string(&Msg::Challenge { nonce }).unwrap());

                    let mut hello_site: Option<u32> = None;
                    for line in reader.lines() {
                        let Ok(line) = line else { break };
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
                                let snap = state.lock().unwrap().clone();
                                let _ = tx.send(
                                    serde_json::to_string(&Msg::Snapshot { ops: snap }).unwrap(),
                                );
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
    // 先同步试连一次，连不上立刻报错（避免用户以为成功了）
    let first = TcpStream::connect(addr)?;
    drop(first);

    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (evt_tx, evt_rx) = channel::<NetEvent>();
    let evt_tx_clone = evt_tx.clone();
    let alive = Arc::new(AtomicBool::new(true));
    let writer: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));

    // 写线程
    {
        let writer = writer.clone();
        let alive = alive.clone();
        std::thread::spawn(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                if !alive.load(Ordering::SeqCst) {
                    break;
                }
                match cmd {
                    Cmd::Broadcast(s) => {
                        if let Some(w) = writer.lock().unwrap().as_mut() {
                            let _ = write_line(w, &s);
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

                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    let Ok(msg) = serde_json::from_str::<Msg>(&line) else { continue };
                    match msg {
                        Msg::Challenge { nonce } => {
                            let hello = Msg::Hello {
                                site,
                                name: name.clone(),
                                doc: doc.clone(),
                                proof: proof(nonce, &password),
                            };
                            let _ =
                                write_line(&mut write_half, &serde_json::to_string(&hello).unwrap());
                        }
                        Msg::Welcome { peers, .. } => {
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
                        Msg::Snapshot { ops } => {
                            let _ = evt_tx.send(NetEvent::Ops(ops));
                        }
                        Msg::Op(op) => {
                            let _ = evt_tx.send(NetEvent::Ops(vec![op]));
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
                if denied {
                    break;
                }
                let _ = evt_tx.send(NetEvent::Disconnected("连接中断".into()));
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
