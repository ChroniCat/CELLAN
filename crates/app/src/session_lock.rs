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
    pub user: String,
    pub machine: String,
    pub pid: u32,
    pub beat: u64,
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

pub fn machine_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

/// 写占用卡。失败不致命（只读目录里就是发现不了，不影响正常编辑）
pub fn write(file: &Path, host: &str, user: &str) -> std::io::Result<()> {
    let p = lock_path(file);
    let info = format!(
        "version=1\nfile={}\nhost={}\nuser={}\nmachine={}\npid={}\nbeat={}\n",
        file.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        host,
        user,
        machine_name(),
        std::process::id(),
        now_secs()
    );
    // 先写临时文件再改名：别人读到的永远是完整内容
    let tmp = p.with_extension("lock.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(info.as_bytes())?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, &p)
}

pub fn remove(file: &Path) {
    let _ = std::fs::remove_file(lock_path(file));
}

pub fn read(file: &Path) -> Option<LockInfo> {
    let text = std::fs::read_to_string(lock_path(file)).ok()?;
    let mut info = LockInfo {
        file: String::new(),
        host: String::new(),
        user: String::new(),
        machine: String::new(),
        pid: 0,
        beat: 0,
    };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "file" => info.file = v.trim().to_string(),
            "host" => info.host = v.trim().to_string(),
            "user" => info.user = v.trim().to_string(),
            "machine" => info.machine = v.trim().to_string(),
            "pid" => info.pid = v.trim().parse().unwrap_or(0),
            "beat" => info.beat = v.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    if info.host.is_empty() {
        return None;
    }
    Some(info)
}

/// 心跳还新鲜吗（对方是不是还活着）
pub fn is_fresh(info: &LockInfo) -> bool {
    let now = now_secs();
    info.beat > 0 && now.saturating_sub(info.beat) <= STALE_SECS
}

/// 这张卡是不是我自己写的（自己不该加入自己）
pub fn is_mine(info: &LockInfo) -> bool {
    info.machine == machine_name() && info.pid == std::process::id()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("sheetflow_lock_test");
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
}
