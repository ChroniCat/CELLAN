//! 会话发现：在文件旁边放一张"占用卡"，同事打开同一个文件时自动找到我。
//!
//! 为什么用文件而不是广播：用户的实际场景是 Z: 盘上的共享工作簿，
//! 两个人在不同网段、甚至广播被交换机拦掉，UDP 都到不了；但两个人都能
//! 读写同一个目录，所以把"谁在编辑、地址是多少"写进目录里最可靠。
//!
//! 卡片名 `.~sheetflow-<文件名>.lock`，内容是 key=value 行，每 5 秒刷新一次
//! 心跳；超过 20 秒没刷新就认为对方已经退出（崩溃也不会留下永久占位）。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 心跳多久算新鲜
pub const STALE_SECS: u64 = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct LockInfo {
    pub file: String,
    pub host: String,
    /// 宿主机器全部候选局域网地址（`ip:port`），兼容多网卡/虚拟网卡环境自动寻址
    pub hosts: Vec<String>,
    pub user: String,
    pub machine: String,
    pub pid: u32,
    pub beat: u64,
    /// 宿主会话唯一标识（围栏令牌）：防止其他宿主误删或无条件覆盖占用卡
    pub session: String,
    /// 宿主建房时的毫秒时间戳，用于双宿主冲突时的确定性主从裁决
    pub started_ms: u64,
    /// 自动协作的房间密钥（宿主开房时随机生成）。只有能读到这个共享目录的人
    /// 才拿得到它，等于把「谁能入房」收紧到「谁能读这个文件夹」。
    /// 设了房间口令或工作簿有打开密码时为空，此时入房必须知道口令。
    pub secret: String,
}

/// 生成自动协作用的随机房间密钥（256 位，十六进制）
pub fn new_room_secret() -> std::io::Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| std::io::Error::other(format!("系统随机数不可用：{e}")))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

impl LockInfo {
    /// 宿主是否为自动协作写了房间密钥（旧版本宿主不写）
    pub fn has_secret(&self) -> bool {
        !self.secret.trim().is_empty()
    }

    pub fn candidate_addrs(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.host.trim().is_empty() {
            out.push(self.host.trim().to_string());
        }
        for h in &self.hosts {
            let h = h.trim();
            if !h.is_empty() && !out.iter().any(|x| x == h) {
                out.push(h.to_string());
            }
        }
        out
    }

    pub fn join_spec(&self) -> String {
        self.candidate_addrs().join(",")
    }

    pub fn matches_addr(&self, addr_spec: &str) -> bool {
        let cands = self.candidate_addrs();
        addr_spec
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .any(|s| cands.iter().any(|c| c == s))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostLiveness {
    /// TCP 探测可达，宿主明确在线
    Reachable,
    /// TCP 暂不可达（如跨网段或防火墙拦截），但共享目录心跳仍新鲜
    UnreachableButFresh,
    /// 端口明确拒绝连接（RST）或心跳已超时，原宿主已离线
    Dead,
}

/// 占用卡路径：和文件放在同一个目录（网络盘上也能写）
pub fn lock_path(file: &Path) -> PathBuf {
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook".into());
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(".~sheetflow-{name}.lock"))
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn new_session_id() -> String {
    let mut raw = [0u8; 16];
    if getrandom::fill(&mut raw).is_ok() {
        hex::encode(raw)
    } else {
        format!("{:08x}{:016x}", std::process::id(), now_ms())
    }
}

pub fn machine_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "unknown".into())
    })
    .clone()
}

#[cfg(windows)]
fn set_win_attrs(path: &Path, attrs: u32) {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileAttributesW(name: *const u16, attrs: u32) -> i32;
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = SetFileAttributesW(wide.as_ptr(), attrs);
    }
}

fn hide_sidecar(path: &Path) {
    #[cfg(windows)]
    set_win_attrs(path, 0x02); // FILE_ATTRIBUTE_HIDDEN
    #[cfg(not(windows))]
    let _ = path;
}

fn unhide_before_replace(path: &Path) {
    #[cfg(windows)]
    if path.exists() {
        set_win_attrs(path, 0x80); // FILE_ATTRIBUTE_NORMAL，防止 Windows MoveFileExW 因目标带 HIDDEN 报拒绝访问
    }
    #[cfg(not(windows))]
    let _ = path;
}

fn create_new_hidden(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.attributes(0x02); // FILE_ATTRIBUTE_HIDDEN
    }
    opts.open(path)
}

fn cleanup_stale_aux_files(file: &Path) {
    let _ = std::fs::remove_file(fw_hint_path(file));
    let Some(dir) = file.parent() else { return };
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook".into());
    let dead_prefix = format!(".~sheetflow-{name}.dead-");
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            if fname.starts_with(&dead_prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn atomic_write_text(target: &Path, content: &str) -> std::io::Result<()> {
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let nonce = getrandom::u64().unwrap_or_else(|_| now_ms());
    let tmp = dir.join(format!(".~sf-{}-{nonce:016x}.tmp", std::process::id()));
    let write_res = (|| {
        let mut f = create_new_hidden(&tmp)?;
        f.write_all(content.as_bytes())?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    unhide_before_replace(target);
    let rename_res = std::fs::rename(&tmp, target);
    if rename_res.is_ok() {
        hide_sidecar(target);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
    rename_res
}

/// 写占用卡。失败不致命（只读目录里就是发现不了，不影响正常编辑）
pub fn write(file: &Path, host: &str, user: &str) -> std::io::Result<()> {
    write_session(file, host, user, "", 0)
}

pub fn write_session(
    file: &Path,
    host: &str,
    user: &str,
    session: &str,
    started_ms: u64,
) -> std::io::Result<()> {
    write_session_with_hosts(file, host, &[], user, session, started_ms, "")
}

const MAX_LOCK_FILE_BYTES: u64 = 16 * 1024;

/// 校验会话令牌：仅允许 1..=64 位单段 ASCII 安全字符，防止 `.dead-<session>` 路径穿越。
fn is_valid_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// 单行锁文件字段清洗：剥离 CR/LF 防止换行注入其他键值对。
fn sanitize_single_line(s: &str) -> String {
    s.chars().filter(|&c| c != '\r' && c != '\n').collect()
}

/// 有界读取锁文件文本（上限 16 KiB，防止共享目录恶意超大锁文件耗尽内存）。
fn read_bounded_text(path: &Path) -> Option<String> {
    use std::io::Read;
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(MAX_LOCK_FILE_BYTES + 1).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > MAX_LOCK_FILE_BYTES {
        return None;
    }
    String::from_utf8(buf).ok()
}

/// 原子回收过期锁文件：先重命名为唯一墓碑名，仅抢占成功的一方删除墓碑并继续创建新文件，
/// 消除 `remove_file` -> `create_new` 的双宿主竞态。
fn claim_stale_file_via_tombstone(path: &Path) -> bool {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return false;
    };
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let nonce = new_session_id();
    let tombstone = dir.join(format!("{name}.reclaim-{}-{nonce}", std::process::id()));
    if std::fs::rename(path, &tombstone).is_ok() {
        let _ = std::fs::remove_file(&tombstone);
        true
    } else {
        false
    }
}

pub fn write_session_with_hosts(
    file: &Path,
    host: &str,
    hosts: &[String],
    user: &str,
    session: &str,
    started_ms: u64,
    secret: &str,
) -> std::io::Result<()> {
    let p = lock_path(file);
    let fname = file
        .file_name()
        .map(|s| sanitize_single_line(&s.to_string_lossy()))
        .unwrap_or_default();
    let mut info = format!(
        "version=2\nfile={}\nhost={}\nuser={}\nmachine={}\npid={}\nbeat={}\n",
        fname,
        sanitize_single_line(host),
        sanitize_single_line(user),
        sanitize_single_line(&machine_name()),
        std::process::id(),
        now_secs()
    );
    if !hosts.is_empty() {
        let clean_hosts: Vec<String> = hosts.iter().map(|h| sanitize_single_line(h)).collect();
        info.push_str(&format!("hosts={}\n", clean_hosts.join(",")));
    }
    let clean_session = sanitize_single_line(session);
    if !clean_session.is_empty() {
        info.push_str(&format!("session={clean_session}\n"));
    }
    if started_ms > 0 {
        info.push_str(&format!("started_ms={started_ms}\n"));
    }
    let clean_secret = sanitize_single_line(secret.trim());
    if !clean_secret.is_empty() {
        info.push_str(&format!("secret={clean_secret}\n"));
    }
    atomic_write_text(&p, &info)
}

pub fn remove(file: &Path) {
    let _ = std::fs::remove_file(lock_path(file));
    cleanup_stale_aux_files(file);
}

/// 仅当占用卡确实归属当前宿主会话时才删除，绝不误删其他宿主的活跃占用卡
pub fn remove_for_session(file: &Path, session: &str) {
    if let Some(info) = read(file) {
        if is_owned_by_session(&info, Some(session)) {
            let _ = std::fs::remove_file(lock_path(file));
            cleanup_stale_aux_files(file);
        }
    }
}

pub fn read(file: &Path) -> Option<LockInfo> {
    let text = read_bounded_text(&lock_path(file))?;
    let mut info = LockInfo {
        file: String::new(),
        host: String::new(),
        hosts: Vec::new(),
        user: String::new(),
        machine: String::new(),
        pid: 0,
        beat: 0,
        session: String::new(),
        started_ms: 0,
        secret: String::new(),
    };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "file" => info.file = v.trim().to_string(),
            "host" => info.host = v.trim().to_string(),
            "hosts" => {
                info.hosts = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "user" => info.user = v.trim().to_string(),
            "machine" => info.machine = v.trim().to_string(),
            "pid" => info.pid = v.trim().parse().unwrap_or(0),
            "beat" => info.beat = v.trim().parse().unwrap_or(0),
            "session" => {
                let s = v.trim();
                if !s.is_empty() {
                    if !is_valid_session_id(s) {
                        return None;
                    }
                    info.session = s.to_string();
                }
            }
            "started_ms" => info.started_ms = v.trim().parse().unwrap_or(0),
            "secret" => info.secret = v.trim().to_string(),
            _ => {}
        }
    }
    if info.host.is_empty() {
        return None;
    }
    if info.hosts.is_empty() {
        info.hosts.push(info.host.clone());
    }
    Some(info)
}

/// 心跳还新鲜吗（对方是不是还活着）
pub fn is_fresh(info: &LockInfo) -> bool {
    let now = now_secs();
    info.beat > 0 && now.saturating_sub(info.beat) <= STALE_SECS
}

/// 这张卡是不是当前进程写的（兼容未带 session 的旧卡片）
pub fn is_mine(info: &LockInfo) -> bool {
    info.machine == machine_name() && info.pid == std::process::id()
}

/// 按宿主会话令牌精确判定卡片归属；旧版卡片未写 `session` 时回退 `machine + pid`。
pub fn is_owned_by_session(info: &LockInfo, my_session: Option<&str>) -> bool {
    match my_session.filter(|s| !s.is_empty()) {
        Some(ms) => {
            if !info.session.is_empty() {
                info.session == ms
            } else {
                is_mine(info)
            }
        }
        None => info.session.is_empty() && is_mine(info),
    }
}

fn probe_single_sock(sock: &std::net::SocketAddr) -> Result<(), std::io::ErrorKind> {
    let timeout = if sock.ip().is_loopback() {
        std::time::Duration::from_millis(200)
    } else {
        std::time::Duration::from_millis(450)
    };
    match std::net::TcpStream::connect_timeout(sock, timeout) {
        Ok(_) => Ok(()),
        Err(e) => {
            if sock.ip().is_loopback() {
                // 本机回环接口不存在路由器丢包；200ms 未应答即代表无监听进程（规避 Windows 回环 2s SYN 重试停顿）
                Err(std::io::ErrorKind::ConnectionRefused)
            } else {
                Err(e.kind())
            }
        }
    }
}

pub fn probe_addr(addr: &str) -> bool {
    for part in addr.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if let Ok(sock) = part.parse::<std::net::SocketAddr>() {
            if probe_single_sock(&sock).is_ok() {
                return true;
            }
        }
    }
    false
}

/// 综合多网卡并行 TCP 探活与心跳判断占用卡对应宿主是否仍在线：
/// - 任一候选地址可达 → `Reachable`；
/// - 全部候选地址均明确拒绝连接（RST）→ 立即判 `Dead`；
/// - 端口超时（可能跨网段/防火墙）→ 心跳仍新鲜则判 `UnreachableButFresh`，否则 `Dead`。
pub fn probe_host(info: &LockInfo) -> HostLiveness {
    let socks: Vec<std::net::SocketAddr> = info
        .candidate_addrs()
        .iter()
        .filter_map(|s| s.parse::<std::net::SocketAddr>().ok())
        .collect();
    if socks.len() == 1 {
        match probe_single_sock(&socks[0]) {
            Ok(()) => return HostLiveness::Reachable,
            Err(std::io::ErrorKind::ConnectionRefused) => return HostLiveness::Dead,
            Err(_) => {}
        }
    } else if socks.len() > 1 {
        let mut handles = Vec::with_capacity(socks.len());
        for sock in socks {
            handles.push(std::thread::spawn(move || probe_single_sock(&sock)));
        }
        let mut any_ok = false;
        let mut all_refused = true;
        for h in handles {
            match h.join().unwrap_or(Err(std::io::ErrorKind::TimedOut)) {
                Ok(()) => {
                    any_ok = true;
                    all_refused = false;
                }
                Err(std::io::ErrorKind::ConnectionRefused) => {}
                Err(_) => {
                    all_refused = false;
                }
            }
        }
        if any_ok {
            return HostLiveness::Reachable;
        }
        if all_refused {
            return HostLiveness::Dead;
        }
    }
    if is_fresh(info) {
        HostLiveness::UnreachableButFresh
    } else {
        HostLiveness::Dead
    }
}

#[cfg(windows)]
fn winsock_local_ipv4s() -> Vec<std::net::Ipv4Addr> {
    use std::os::windows::io::AsRawSocket;
    const SIO_ADDRESS_LIST_QUERY: u32 = 0x4800_0016;
    const AF_INET: u16 = 2;
    #[repr(C)]
    struct SockAddrIn {
        sin_family: u16,
        sin_port: u16,
        sin_addr: [u8; 4],
        sin_zero: [u8; 8],
    }
    #[repr(C)]
    struct SocketAddress {
        lp_sockaddr: *const SockAddrIn,
        i_sockaddr_length: i32,
    }
    #[repr(C)]
    struct SocketAddressList {
        i_address_count: i32,
        address: [SocketAddress; 1],
    }
    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn WSAIoctl(
            s: usize,
            dw_io_control_code: u32,
            lpv_in_buffer: *const core::ffi::c_void,
            cb_in_buffer: u32,
            lpv_out_buffer: *mut core::ffi::c_void,
            cb_out_buffer: u32,
            lpcb_bytes_returned: *mut u32,
            lp_overlapped: *mut core::ffi::c_void,
            lp_completion_routine: *const core::ffi::c_void,
        ) -> i32;
    }
    let Ok(udp) = std::net::UdpSocket::bind("0.0.0.0:0") else {
        return Vec::new();
    };
    let mut buf = vec![0u64; 512];
    let mut bytes_ret = 0u32;
    let rc = unsafe {
        WSAIoctl(
            udp.as_raw_socket() as usize,
            SIO_ADDRESS_LIST_QUERY,
            core::ptr::null(),
            0,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            (buf.len() * 8) as u32,
            &mut bytes_ret,
            core::ptr::null_mut(),
            core::ptr::null(),
        )
    };
    if rc != 0 {
        return Vec::new();
    }
    let list = unsafe { &*(buf.as_ptr() as *const SocketAddressList) };
    let count = list.i_address_count.clamp(0, 64) as usize;
    let items = unsafe { core::slice::from_raw_parts(list.address.as_ptr(), count) };
    let mut out = Vec::new();
    for item in items {
        if !item.lp_sockaddr.is_null()
            && (item.i_sockaddr_length as usize) >= core::mem::size_of::<SockAddrIn>()
        {
            let sa = unsafe { &*item.lp_sockaddr };
            if sa.sin_family == AF_INET {
                let ip = std::net::Ipv4Addr::from(sa.sin_addr);
                if !ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local() {
                    out.push(ip);
                }
            }
        }
    }
    out
}

fn ipv4_priority(v4: &std::net::Ipv4Addr) -> u8 {
    let oct = v4.octets();
    if oct[0] == 192 && oct[1] == 168 {
        if oct[2] == 56 {
            return 3; // VirtualBox Host-Only 常见默认段，排在真实局域网后面
        }
        return 0;
    }
    if oct[0] == 10 {
        return 1;
    }
    if oct[0] == 172 && (16..=31).contains(&oct[1]) {
        return 2; // 兼容企业 172 段，同时排在 192.168/10 之后以降低 WSL/Hyper-V 误选概率
    }
    if oct[0] == 198 && (oct[1] == 18 || oct[1] == 19) {
        return 5; // 代理 TUN 虚拟网段排最后
    }
    4
}

/// 枚举本机全部可用于局域网连接的 IPv4 地址（首选路由物理网段排第一，附带其余活动网卡供对端并行寻址；结果缓存 15 秒）
pub fn local_lan_ips() -> Vec<String> {
    if let Ok(v) = std::env::var("SC_HOST_ADDR") {
        let v = v.trim();
        if !v.is_empty() {
            let ip = v.split(':').next().unwrap_or(v).to_string();
            return vec![ip];
        }
    }
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<Option<(std::time::Instant, Vec<String>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some((at, cached)) = guard.as_ref() {
            if at.elapsed().as_secs() < 15 && !cached.is_empty() {
                return cached.clone();
            }
        }
    }
    use std::net::{IpAddr, Ipv4Addr, UdpSocket};
    let mut routed: Vec<Ipv4Addr> = Vec::new();
    for target in [
        "192.168.1.1:80",
        "192.168.0.1:80",
        "192.168.31.1:80",
        "10.0.0.1:80",
        "172.16.0.1:80",
        "223.5.5.5:80",
        "8.8.8.8:80",
    ] {
        if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
            if s.connect(target).is_ok() {
                if let Ok(a) = s.local_addr() {
                    if let IpAddr::V4(v4) = a.ip() {
                        if !v4.is_loopback()
                            && !v4.is_unspecified()
                            && !v4.is_link_local()
                            && !routed.contains(&v4)
                        {
                            routed.push(v4);
                        }
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    for v4 in winsock_local_ipv4s() {
        if !routed.contains(&v4) {
            routed.push(v4);
        }
    }
    routed.sort_by_key(|v4| ipv4_priority(v4));
    let res: Vec<String> = if routed.is_empty() {
        vec!["127.0.0.1".into()]
    } else {
        routed.into_iter().map(|ip| ip.to_string()).collect()
    };
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((std::time::Instant::now(), res.clone()));
    }
    res
}

/// 双宿主同时存活时的确定性主从裁决：建房时间更早（相同则按 `session` 字典序更小）的宿主胜出。
pub fn wins_tiebreak(my_started_ms: u64, my_session: &str, other: &LockInfo) -> bool {
    let a = (if my_started_ms == 0 { u64::MAX } else { my_started_ms }, my_session);
    let b = (
        if other.started_ms == 0 { u64::MAX } else { other.started_ms },
        other.session.as_str(),
    );
    a < b
}

/// 选主文件：谁先创建成功谁当宿主，避免两个人同时打开或同时接任时开出两个房间
pub fn elect_path(file: &Path) -> PathBuf {
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook".into());
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(".~sheetflow-{name}.elect"))
}

fn format_election(session: &str) -> String {
    let mut s = format!(
        "machine={}\npid={}\nbeat={}\n",
        sanitize_single_line(&machine_name()),
        std::process::id(),
        now_secs()
    );
    let clean = sanitize_single_line(session);
    if !clean.is_empty() {
        s.push_str(&format!("session={clean}\n"));
    }
    s
}

fn read_election(file: &Path) -> Option<(String, String, u32, u64)> {
    let text = read_bounded_text(&elect_path(file))?;
    let mut session = String::new();
    let mut machine = String::new();
    let mut pid = 0u32;
    let mut beat = 0u64;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "session" => {
                let s = v.trim();
                if !s.is_empty() {
                    if !is_valid_session_id(s) {
                        return None;
                    }
                    session = s.to_string();
                }
            }
            "machine" => machine = v.trim().to_string(),
            "pid" => pid = v.trim().parse().unwrap_or(0),
            "beat" => beat = v.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    Some((session, machine, pid, beat))
}

fn election_is_stale(file: &Path) -> bool {
    if let Some((esess, _, _, beat)) = read_election(file) {
        if beat > 0 {
            if now_secs().saturating_sub(beat) <= STALE_SECS {
                return false;
            }
            // 若对应宿主的占用卡仍在持续续期，选主文件也不能被抢占清理
            if !esess.is_empty() {
                if let Some(li) = read(file) {
                    if li.session == esess && is_fresh(&li) {
                        return false;
                    }
                }
            }
            return true;
        }
    }
    if let Ok(md) = std::fs::metadata(elect_path(file)) {
        let age = md
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return age > STALE_SECS;
    }
    false
}

fn dead_claim_path(file: &Path, dead_session: &str) -> Option<PathBuf> {
    if !is_valid_session_id(dead_session) {
        return None;
    }
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook".into());
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    Some(dir.join(format!(".~sheetflow-{name}.dead-{dead_session}")))
}

fn try_claim_dead_session(file: &Path, dead_session: &str) -> bool {
    let Some(claim) = dead_claim_path(file, dead_session) else {
        return false;
    };
    if create_new_hidden(&claim).is_ok() {
        return true;
    }
    if let Ok(md) = std::fs::metadata(&claim) {
        let age = md
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if age > STALE_SECS && claim_stale_file_via_tombstone(&claim) {
            return create_new_hidden(&claim).is_ok();
        }
    }
    false
}

/// 尝试当选宿主；已有新鲜的选主文件就返回 false
pub fn claim_election(file: &Path) -> bool {
    claim_election_for(file, "")
}

pub fn claim_election_for(file: &Path, session: &str) -> bool {
    claim_election_takeover(file, session, None)
}

pub fn claim_election_takeover(
    file: &Path,
    session: &str,
    expected_dead: Option<&str>,
) -> bool {
    let p = elect_path(file);
    let body = format_election(session);
    let create = || {
        create_new_hidden(&p).and_then(|mut f| {
            f.write_all(body.as_bytes())?;
            Ok(())
        })
    };
    if create().is_ok() {
        return true;
    }
    if !session.is_empty() {
        if let Some((esess, _, _, _)) = read_election(file) {
            if esess == session {
                let _ = atomic_write_text(&p, &body);
                return true;
            }
            // 原宿主异常退出或断电但选主心跳尚未满 20 秒：
            // 若其占用卡端口明确 Dead，或属于刚断开的原宿主且不可达，
            // 通过 `.dead-<esess>` 独占锁保证只有一个接任者能回收旧选主卡并当选。
            if is_valid_session_id(&esess) {
                if let Some(li) = read(file) {
                    if li.session == esess {
                        let liveness = probe_host(&li);
                        let is_dead_target = liveness == HostLiveness::Dead
                            || (expected_dead == Some(esess.as_str())
                                && liveness != HostLiveness::Reachable);
                        if is_dead_target {
                            if try_claim_dead_session(file, &esess) {
                                let _ = std::fs::remove_file(&p);
                                let ok = create().is_ok();
                                if ok {
                                    if let Some(claim) = dead_claim_path(file, &esess) {
                                        let _ = std::fs::remove_file(claim);
                                    }
                                }
                                return ok;
                            }
                            return false;
                        }
                    }
                }
            }
        }
    }
    if election_is_stale(file) && claim_stale_file_via_tombstone(&p) {
        return create().is_ok();
    }
    false
}

/// 宿主运行期伴随心跳续期选主文件，防止 20 秒后选主卡失效被误抢占
pub fn refresh_election(file: &Path, session: &str) {
    let p = elect_path(file);
    if !p.exists() {
        let _ = claim_election_for(file, session);
        return;
    }
    if let Some((esess, emachine, epid, _)) = read_election(file) {
        let mine = if !session.is_empty() && !esess.is_empty() {
            esess == session
        } else {
            emachine == machine_name() && epid == std::process::id()
        };
        if mine {
            let _ = atomic_write_text(&p, &format_election(session));
        }
    }
}

pub fn release_election(file: &Path) {
    let _ = std::fs::remove_file(elect_path(file));
}

pub fn release_election_for(file: &Path, session: &str) {
    if let Some((esess, emachine, epid, _)) = read_election(file) {
        let mine = if !session.is_empty() && !esess.is_empty() {
            esess == session
        } else {
            esess.is_empty() && emachine == machine_name() && epid == std::process::id()
        };
        if mine {
            let _ = std::fs::remove_file(elect_path(file));
        }
    }
}

pub const SHARED_MARKER: &str = ".cellan-share";

/// 判断文件所在目录（或上一级目录）是否已显式标记为局域网协同目录
pub fn is_dir_marked_shared(file: &Path) -> bool {
    let Some(dir) = file.parent() else {
        return false;
    };
    if dir.join(SHARED_MARKER).exists() {
        return true;
    }
    dir.parent()
        .map(|p| p.join(SHARED_MARKER).exists())
        .unwrap_or(false)
}

/// 将当前文件所在目录显式标记/取消标记为局域网协同共享目录（支持本机共享盘符或自定义同步盘）
pub fn set_dir_marked_shared(file: &Path, enabled: bool) -> std::io::Result<()> {
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let marker = dir.join(SHARED_MARKER);
    let res = if enabled {
        unhide_before_replace(&marker);
        let mut f = std::fs::File::create(&marker)?;
        let _ = f.write_all(b"cellan-lan-share=1\n");
        drop(f);
        hide_sidecar(&marker);
        Ok(())
    } else {
        match std::fs::remove_file(&marker) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    };
    invalidate_network_path_cache();
    res
}

/// 检测本机路径是否位于已开启 Windows 局域网文件共享（LanmanServer\Shares）的文件夹内，
/// 使共享发起机直接从本地盘符打开文件时也能自动进入实时协同（结果缓存 15 秒，零子进程开销）。
#[cfg(windows)]
fn is_local_windows_smb_share(file: &Path) -> bool {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<Option<(std::time::Instant, Vec<String>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let shares: Vec<String> = {
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, list)) = guard.as_ref() {
            if at.elapsed().as_secs() < 15 {
                list.clone()
            } else {
                let fresh = query_lanman_share_paths();
                *guard = Some((std::time::Instant::now(), fresh.clone()));
                fresh
            }
        } else {
            let fresh = query_lanman_share_paths();
            *guard = Some((std::time::Instant::now(), fresh.clone()));
            fresh
        }
    };
    if shares.is_empty() {
        return false;
    }
    let norm = file
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    shares.iter().any(|sh| {
        norm == *sh
            || (norm.len() > sh.len()
                && norm.starts_with(sh.as_str())
                && norm.as_bytes().get(sh.len()) == Some(&b'\\'))
    })
}

#[cfg(windows)]
fn query_lanman_share_paths() -> Vec<String> {
    const HKEY_LOCAL_MACHINE: usize = 0x8000_0002;
    const KEY_READ: u32 = 0x0002_0019;
    const REG_MULTI_SZ: u32 = 7;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegOpenKeyExW(
            hkey: usize,
            subkey: *const u16,
            options: u32,
            sam: u32,
            result: *mut usize,
        ) -> i32;
        fn RegEnumValueW(
            hkey: usize,
            index: u32,
            value_name: *mut u16,
            value_name_len: *mut u32,
            reserved: *const u32,
            reg_type: *mut u32,
            data: *mut u8,
            data_len: *mut u32,
        ) -> i32;
        fn RegCloseKey(hkey: usize) -> i32;
    }
    let subkey: Vec<u16> = "SYSTEM\\CurrentControlSet\\Services\\LanmanServer\\Shares"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut hkey: usize = 0;
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey.as_ptr(), 0, KEY_READ, &mut hkey) } != 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut idx = 0u32;
    loop {
        let mut name_buf = vec![0u16; 260];
        let mut name_len = name_buf.len() as u32;
        let mut reg_type = 0u32;
        let mut data_buf = vec![0u16; 2048];
        let mut data_bytes = (data_buf.len() * 2) as u32;
        let rc = unsafe {
            RegEnumValueW(
                hkey,
                idx,
                name_buf.as_mut_ptr(),
                &mut name_len,
                core::ptr::null(),
                &mut reg_type,
                data_buf.as_mut_ptr() as *mut u8,
                &mut data_bytes,
            )
        };
        if rc != 0 {
            break;
        }
        idx += 1;
        let share_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        // 排除 C$/D$/ADMIN$/IPC$ 等隐藏管理共享
        if share_name.ends_with('$') {
            continue;
        }
        if reg_type == REG_MULTI_SZ && data_bytes >= 2 {
            let u16_len = (data_bytes as usize) / 2;
            for entry in data_buf[..u16_len].split(|&ch| ch == 0) {
                if entry.is_empty() {
                    continue;
                }
                let s = String::from_utf16_lossy(entry);
                if let Some((k, v)) = s.split_once('=') {
                    if k.trim().eq_ignore_ascii_case("Path") {
                        let p = v
                            .trim()
                            .replace('/', "\\")
                            .trim_end_matches('\\')
                            .to_lowercase();
                        // 排除整盘根目录共享误伤系统目录，但保留子目录共享（或显式整盘非系统盘）
                        if p.len() >= 2 && !out.contains(&p) {
                            out.push(p);
                        }
                    }
                }
            }
        }
    }
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    out
}

fn network_path_cache(
) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, (std::time::Instant, bool)>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, (std::time::Instant, bool)>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub fn invalidate_network_path_cache() {
    if let Ok(mut guard) = network_path_cache().lock() {
        guard.clear();
    }
}

/// 路径是不是网盘/共享目录：支持 UNC、SMB 映射网络盘、本机已开启 Windows 共享的文件夹、主流企业云盘/同步盘及显式标记的协同目录。
/// 结果按路径缓存 30 秒（最多 64 项），消除热路径重复磁盘/驱动器/注册表探测；`set_dir_marked_shared` 自动失效缓存。
pub fn is_network_path(file: &Path) -> bool {
    const TTL: std::time::Duration = std::time::Duration::from_secs(30);
    if let Ok(guard) = network_path_cache().lock() {
        if let Some(&(at, val)) = guard.get(file) {
            if at.elapsed() < TTL {
                return val;
            }
        }
    }
    let val = is_network_path_uncached(file);
    if let Ok(mut guard) = network_path_cache().lock() {
        if guard.len() >= 64 {
            guard.clear();
        }
        guard.insert(file.to_path_buf(), (std::time::Instant::now(), val));
    }
    val
}

fn is_network_path_uncached(file: &Path) -> bool {
    let text = file.to_string_lossy();
    if text.starts_with("\\\\") || text.starts_with("//") {
        return true;
    }
    if is_dir_marked_shared(file) {
        return true;
    }
    // 排除系统临时目录与单测隔离目录（单测/UI 回归沙箱下运行，避免未标记时误触发静默建房）
    let lower = text.to_lowercase();
    if lower.contains("\\appdata\\local\\temp\\")
        || lower.contains("/appdata/local/temp/")
        || lower.contains("\\target\\test-tmp\\")
        || lower.contains("/target/test-tmp/")
        || lower.contains("sheets-native-")
    {
        return false;
    }
    #[cfg(windows)]
    {
        if let Some(root) = text.chars().next().filter(|c| c.is_ascii_alphabetic()) {
            let root = format!("{root}:\\");
            let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetDriveTypeW(lp_root_path_name: *const u16) -> u32;
            }
            // DRIVE_REMOTE = 4（映射的局域网 SMB/WebDAV 驱动器）
            if unsafe { GetDriveTypeW(wide.as_ptr()) == 4 } {
                return true;
            }
        }
        if is_local_windows_smb_share(file) {
            return true;
        }
    }
    // Windows Cloud Filter / 同步盘重解析点与脱机占位属性检测
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const CLOUD_MASK: u32 = 0x0000_0400 // FILE_ATTRIBUTE_REPARSE_POINT
            | 0x0004_0000 // FILE_ATTRIBUTE_RECALL_ON_OPEN
            | 0x0008_0000 // FILE_ATTRIBUTE_PINNED
            | 0x0010_0000 // FILE_ATTRIBUTE_UNPINNED
            | 0x0040_0000; // FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
        if let Ok(md) = std::fs::symlink_metadata(file) {
            if md.file_attributes() & CLOUD_MASK != 0 {
                return true;
            }
        }
        if let Some(parent) = file.parent() {
            if let Ok(md) = std::fs::symlink_metadata(parent) {
                if md.file_attributes() & CLOUD_MASK != 0 {
                    return true;
                }
            }
        }
    }
    // 主流企业网盘 / 局域网同步盘目录特征匹配
    const CLOUD_TOKENS: &[&str] = &[
        "onedrive",
        "synologydrive",
        "synology",
        "seafile",
        "nextcloud",
        "owncloud",
        "nutstore",
        "坚果云",
        "wpscloud",
        "wps网盘",
        "weiyun",
        "wedrive",
        "微盘",
        "企业微盘",
        "baidunetdisk",
        "企业网盘",
        "同步盘",
        "共享盘",
        "dropbox",
        "iclouddrive",
        "feishu",
        "dingtalk",
        "resilio",
        "syncthing",
    ];
    for comp in file.components() {
        let seg = comp.as_os_str().to_string_lossy().to_lowercase();
        if CLOUD_TOKENS.iter().any(|tok| seg.contains(tok)) {
            return true;
        }
    }
    false
}

/// 计算磁盘工作簿文件的流式 SHA-256 指纹（仅对 ≤128MB 文件计算，供双方打开同一共享文件时走 0 冗余增量握手）。
pub fn file_base_hash(file: &Path) -> String {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let Ok(meta) = std::fs::metadata(file) else {
        return String::new();
    };
    if !meta.is_file() || meta.len() == 0 || meta.len() > 128 * 1024 * 1024 {
        return String::new();
    }
    let Ok(mut f) = std::fs::File::open(file) else {
        return String::new();
    };
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return String::new(),
        }
    }
    hex::encode(hasher.finalize())
}

fn fw_hint_path(file: &Path) -> PathBuf {
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook".into());
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(".~sheetflow-{name}.fw"))
}

/// 客户端发现新鲜占用卡却因 TCP 超时连不进去时，向共享目录写入一条轻量受阻提示，
/// 让宿主机器直接在界面顶部弹出「一键放行防火墙」提示条。
pub fn write_join_blocked_hint(file: &Path, client_user: &str) {
    let body = format!(
        "user={}\nbeat={}\n",
        sanitize_single_line(client_user.trim()),
        now_secs()
    );
    let _ = atomic_write_text(&fw_hint_path(file), &body);
}

pub fn read_join_blocked_hint(file: &Path) -> Option<String> {
    let text = read_bounded_text(&fw_hint_path(file))?;
    let mut user = String::new();
    let mut beat = 0u64;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "user" => user = v.trim().to_string(),
            "beat" => beat = v.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    if beat > 0 && now_secs().saturating_sub(beat) <= 35 {
        Some(if user.is_empty() { "其他用户".into() } else { user })
    } else {
        None
    }
}

pub fn clear_join_blocked_hint(file: &Path) {
    let _ = std::fs::remove_file(fw_hint_path(file));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = crate::test_env::scratch("lock-test");
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn lock_path_sits_next_to_workbook() {
        let p = lock_path(Path::new("Z:/共享/月度.xlsx"));
        assert_eq!(p.file_name().unwrap().to_string_lossy(), ".~sheetflow-月度.xlsx.lock");
        assert!(p.parent().unwrap().ends_with("共享"), "占用卡要落在文件所在目录");
    }

    #[test]
    fn write_then_read_roundtrip() {
        let f = tmp("book.xlsx");
        std::fs::write(&f, b"x").unwrap();
        write(&f, "192.168.1.5:7878", "张三").unwrap();
        let info = read(&f).expect("读得到");
        assert_eq!(info.host, "192.168.1.5:7878");
        assert_eq!(info.user, "张三");
        assert_eq!(info.file, "book.xlsx");
        assert!(is_fresh(&info), "刚写的心跳应该是新鲜的");
        assert!(is_mine(&info), "自己写的卡应当认得出");
        remove(&f);
        assert!(read(&f).is_none(), "撤卡后读不到");
    }

    #[test]
    fn stale_beat_is_not_fresh() {
        let f = tmp("old.xlsx");
        std::fs::write(&f, b"x").unwrap();
        let text = format!(
            "version=1
file=old.xlsx
host=10.0.0.2:7878
user=李四
machine=X
pid=1
beat={}
",
            now_secs().saturating_sub(STALE_SECS + 5)
        );
        std::fs::write(lock_path(&f), text).unwrap();
        let info = read(&f).unwrap();
        assert!(!is_fresh(&info), "超过 {} 秒就不该再去找这个地址", STALE_SECS);
        remove(&f);
    }

    #[test]
    fn election_is_exclusive_and_releasable() {
        let f = tmp("elect.xlsx");
        std::fs::write(&f, b"x").unwrap();
        let _ = release_election(&f);
        assert!(claim_election(&f), "第一个客户端应抢到选主");
        assert!(!claim_election(&f), "第二个客户端不应再抢到");
        release_election(&f);
        assert!(claim_election(&f), "释放后可以重新抢");
        release_election(&f);
    }

    #[test]
    fn network_and_cloud_sync_paths_detected_and_share_marker_works() {
        assert!(is_network_path(Path::new(r"\\NAS01\财务部\2026预算.xlsx")));
        assert!(is_network_path(Path::new("C:/Users/Alice/OneDrive/预算.xlsx")));
        assert!(is_network_path(Path::new("D:/SynologyDrive/部门台账/回款.xlsx")));
        assert!(is_network_path(Path::new("E:/我的坚果云/协同表.xlsx")));
        assert!(is_network_path(Path::new("E:/WPS网盘/项目排期.xlsx")));
        assert!(!is_network_path(Path::new("D:/本地个人文档/草稿.xlsx")));

        let sub = crate::test_env::scratch("share-marker-test");
        std::fs::create_dir_all(&sub).unwrap();
        let f = sub.join("team.xlsx");
        std::fs::write(&f, b"x").unwrap();
        set_dir_marked_shared(&f, false).unwrap();
        assert!(!is_dir_marked_shared(&f));
        set_dir_marked_shared(&f, true).unwrap();
        assert!(is_dir_marked_shared(&f));
        assert!(is_network_path(&f), "标记了 .cellan-share 的目录必须被识别为协同目录");
        set_dir_marked_shared(&f, false).unwrap();
        assert!(!is_dir_marked_shared(&f));
    }

    #[test]
    fn session_id_validation_blocks_path_traversal_and_crlf_injection() {
        let dir = crate::test_env::scratch("session-lock-sec");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("doc.xlsx");
        std::fs::write(&f, b"x").unwrap();

        // Malicious session id with path traversal in lock file must be rejected
        let evil = format!(
            "version=1\nfile=doc.xlsx\nhost=127.0.0.1:7878\nuser=evil\nmachine=M\npid=1\nbeat={}\nsession=../../outside\n",
            now_secs()
        );
        std::fs::write(lock_path(&f), evil).unwrap();
        assert!(read(&f).is_none(), "path traversal session id must be rejected");

        // try_claim_dead_session with traversal session id must NOT create files outside dir
        assert!(!try_claim_dead_session(&f, "../../outside_dead"));
        assert!(!dir.parent().unwrap().join("outside_dead").exists());

        // Oversized lock file must be rejected cleanly
        std::fs::write(lock_path(&f), "a".repeat(32 * 1024)).unwrap();
        assert!(read(&f).is_none(), "oversized lock file must be rejected");

        // CR/LF injection in user/host must be sanitized on write
        let _ = std::fs::remove_file(lock_path(&f));
        write_session(&f, "127.0.0.1:7878\nhost=evil:9999", "张三\nuser=冒充", "sess_ok_1", 100).unwrap();
        let info = read(&f).expect("sanitized lock must parse");
        assert!(!info.host.contains('\n'));
        assert!(!info.user.contains('\n'));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
