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

/// 选主文件：谁先创建成功谁当宿主，避免两个人同时打开时开出两个房间
pub fn elect_path(file: &Path) -> PathBuf {
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workbook".into());
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(".~sheetflow-{name}.elect"))
}

/// 尝试当选宿主；已有新鲜的选主文件就返回 false
pub fn claim_election(file: &Path) -> bool {
    let p = elect_path(file);
    let create = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
            .map(|mut f| {
                let _ = f.write_all(
                    format!("machine={}\npid={}\nbeat={}\n", machine_name(), std::process::id(), now_secs())
                        .as_bytes(),
                );
            })
    };
    if create().is_ok() {
        return true;
    }
    // 上一次异常退出留下的选主文件：超过 15 秒就清掉重试
    if let Ok(md) = std::fs::metadata(&p) {
        let age = md.modified().ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(0);
        if age > 15 {
            let _ = std::fs::remove_file(&p);
            return create().is_ok();
        }
    }
    false
}

pub fn release_election(file: &Path) {
    let _ = std::fs::remove_file(elect_path(file));
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
    if enabled {
        let mut f = std::fs::File::create(&marker)?;
        let _ = f.write_all(b"cellan-lan-share=1\n");
        Ok(())
    } else {
        match std::fs::remove_file(&marker) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// 路径是不是网盘/共享目录：支持 UNC、SMB 映射网络盘、主流企业云盘/同步盘（OneDrive / 群晖 / 坚果云 / WPS云盘 / 微盘 / 飞书 / 百度企业网盘等）及显式标记的协同目录。
pub fn is_network_path(file: &Path) -> bool {
    let text = file.to_string_lossy();
    if text.starts_with("\\\\") || text.starts_with("//") {
        return true;
    }
    if is_dir_marked_shared(file) {
        return true;
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
    }
    // 排除系统临时目录（单测/UI 回归沙箱在 %TEMP% 下运行，避免误触发静默建房）
    let lower = text.to_lowercase();
    if lower.contains("\\appdata\\local\\temp\\")
        || lower.contains("/appdata/local/temp/")
        || lower.contains("sheets-native-")
    {
        return false;
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
}
