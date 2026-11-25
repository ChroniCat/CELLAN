//! 最近打开的文件 + 启动页数据。
//!
//! 存 `%LOCALAPPDATA%\CELLAN\recent.txt`，一行一个路径（以 `*` 开头表示已固定），新的在上面。
//! 网络盘暂时断开时**不**自动删除条目，保留给用户；支持固定/取消固定与手动移除。

use std::path::{Path, PathBuf};

pub const MAX: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentEntry {
    pub path: PathBuf,
    pub pinned: bool,
}

fn store() -> PathBuf {
    crate::debug_flags::app_dir().join("recent.txt")
}

/// 快速加载最近打开条目（含固定状态；固定项排在最前，不触盘校验避免断网 SMB 阻塞）。
pub fn load_entries() -> Vec<RecentEntry> {
    let Ok(text) = std::fs::read_to_string(store()) else {
        return Vec::new();
    };
    parse_entries(&text)
}

/// 兼容旧调用点：返回路径列表（固定项在前，未固定项按最近顺序在后）。
pub fn load() -> Vec<PathBuf> {
    load_entries().into_iter().map(|e| e.path).collect()
}

fn parse_entries(text: &str) -> Vec<RecentEntry> {
    let mut pinned = Vec::new();
    let mut normal = Vec::new();
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (is_pin, rest) = if let Some(r) = trimmed.strip_prefix('*') {
            (true, r.trim())
        } else {
            (false, trimmed)
        };
        if rest.is_empty() {
            continue;
        }
        let entry = RecentEntry {
            path: PathBuf::from(rest),
            pinned: is_pin,
        };
        if is_pin {
            pinned.push(entry);
        } else {
            normal.push(entry);
        }
    }
    let normal_cap = MAX.saturating_sub(pinned.len());
    normal.truncate(normal_cap);
    pinned.extend(normal);
    pinned
}

/// 不再在启动后台静默删除离线/网盘文件条目，避免临时断网导致历史记录永久丢失。
/// 保留签名兼容现有启动代码；手动清理请调用 `prune_missing_manual`。
pub fn prune_missing_async(
    _wake: Option<eframe::egui::Context>,
) -> Option<std::sync::mpsc::Receiver<Vec<PathBuf>>> {
    None
}

/// 用户主动触发清理本地已不存在的未固定条目（跳过 UNC 网络路径以防断网误删）。
pub fn prune_missing_manual() -> usize {
    let mut list = load_entries();
    let before = list.len();
    list.retain(|e| e.pinned || is_network_path(&e.path) || e.path.is_file());
    let removed = before.saturating_sub(list.len());
    if removed > 0 {
        save_entries(&list);
    }
    removed
}

/// 把一个文件提到最前（打开/保存时调用；若已固定则保持固定并提到固定区最前）。
pub fn touch(path: &Path) {
    let mut list = load_entries();
    let was_pinned = list.iter().any(|e| e.pinned && same(&e.path, path));
    list.retain(|e| !same(&e.path, path));
    if was_pinned {
        list.insert(
            0,
            RecentEntry {
                path: path.to_path_buf(),
                pinned: true,
            },
        );
    } else {
        let first_unpinned = list.iter().position(|e| !e.pinned).unwrap_or(list.len());
        list.insert(
            first_unpinned,
            RecentEntry {
                path: path.to_path_buf(),
                pinned: false,
            },
        );
    }
    trim_preserving_pinned(&mut list);
    save_entries(&list);
}

/// 切换某条目的固定状态，返回切换后的固定状态。
pub fn toggle_pin(path: &Path) -> bool {
    let mut list = load_entries();
    let mut now_pinned = false;
    if let Some(pos) = list.iter().position(|e| same(&e.path, path)) {
        let mut entry = list.remove(pos);
        entry.pinned = !entry.pinned;
        now_pinned = entry.pinned;
        if entry.pinned {
            list.insert(0, entry);
        } else {
            let first_unpinned = list.iter().position(|e| !e.pinned).unwrap_or(list.len());
            list.insert(first_unpinned, entry);
        }
    } else {
        now_pinned = true;
        list.insert(
            0,
            RecentEntry {
                path: path.to_path_buf(),
                pinned: true,
            },
        );
    }
    trim_preserving_pinned(&mut list);
    save_entries(&list);
    now_pinned
}

pub fn is_pinned(path: &Path) -> bool {
    load_entries()
        .iter()
        .any(|e| e.pinned && same(&e.path, path))
}

pub fn forget(path: &Path) {
    let mut list = load_entries();
    let before = list.len();
    list.retain(|e| !same(&e.path, path));
    if list.len() != before {
        save_entries(&list);
    }
}

fn trim_preserving_pinned(list: &mut Vec<RecentEntry>) {
    let pinned_cnt = list.iter().filter(|e| e.pinned).count();
    let cap = MAX.max(pinned_cnt);
    list.truncate(cap);
}

fn is_network_path(p: &Path) -> bool {
    let s = p.to_string_lossy();
    if s.starts_with("\\\\") || s.starts_with("//") {
        return true;
    }
    #[cfg(windows)]
    {
        // 识别 Windows 映射网络驱动器（例如 Z:\report.xlsx），含暂时断线/不可达的持久化映射盘，
        // 避免手动清理最近文件时把网络盘条目误删。
        let bytes = s.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let letter = bytes[0].to_ascii_uppercase() as u16;
            let root_w = [letter, b':' as u16, b'\\' as u16, 0u16];
            let local_w = [letter, b':' as u16, 0u16];
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetDriveTypeW(lp_root_path_name: *const u16) -> u32;
            }
            const DRIVE_REMOTE: u32 = 4;
            let dtype = unsafe { GetDriveTypeW(root_w.as_ptr()) };
            if dtype == DRIVE_REMOTE {
                return true;
            }
            // 映射盘暂时断开时 GetDriveTypeW 可能返回 DRIVE_NO_ROOT_DIR(1)，进一步向 MPR 查询持久映射表。
            #[link(name = "mpr")]
            unsafe extern "system" {
                fn WNetGetConnectionW(
                    lp_local_name: *const u16,
                    lp_remote_name: *mut u16,
                    lpn_length: *mut u32,
                ) -> u32;
            }
            const NO_ERROR: u32 = 0;
            const ERROR_MORE_DATA: u32 = 234;
            const ERROR_CONNECTION_UNAVAIL: u32 = 1201;
            let mut buf = [0u16; 260];
            let mut len = buf.len() as u32;
            let rc = unsafe { WNetGetConnectionW(local_w.as_ptr(), buf.as_mut_ptr(), &mut len) };
            if matches!(rc, NO_ERROR | ERROR_MORE_DATA | ERROR_CONNECTION_UNAVAIL) {
                return true;
            }
        }
    }
    false
}

fn same(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    #[cfg(windows)]
    {
        let na = a.to_string_lossy().replace('/', "\\");
        let nb = b.to_string_lossy().replace('/', "\\");
        if na.eq_ignore_ascii_case(&nb) {
            return true;
        }
        // 文件名不同或任一路径为 UNC 网盘时，绝不触盘 `canonicalize`，避免阻塞界面线程
        if is_network_path(a) || is_network_path(b) {
            return false;
        }
        let fa = a.file_name().map(|s| s.to_string_lossy().to_lowercase());
        let fb = b.file_name().map(|s| s.to_string_lossy().to_lowercase());
        if fa != fb {
            return false;
        }
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn save_entries(list: &[RecentEntry]) {
    let mut body = String::new();
    for e in list {
        if e.pinned {
            body.push('*');
        }
        body.push_str(&e.path.display().to_string());
        body.push('\n');
    }
    let _ = std::fs::write(store(), body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;

    /// QA-03.1：LOCALAPPDATA 由守卫指向本次运行专用目录，测试结束自动恢复原值。
    #[test]
    fn touch_moves_to_front_and_dedupes() {
        let env = TestEnv::new("recent");
        let d = env.path("files");
        std::fs::create_dir_all(&d).unwrap();
        let a = d.join("a.xlsx");
        let b = d.join("b.xlsx");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();

        touch(&a);
        touch(&b);
        touch(&a);
        let list = load();
        assert_eq!(list.len(), 2, "同一文件不该重复出现");
        assert!(list[0].ends_with("a.xlsx"), "最近打开的排最前");
    }

    #[test]
    fn recent_store_lives_under_isolated_profile() {
        let env = TestEnv::new("recent-store");
        let d = env.path("files");
        std::fs::create_dir_all(&d).unwrap();
        let a = d.join("a.xlsx");
        std::fs::write(&a, b"x").unwrap();
        touch(&a);
        let store = env.profile().join("CELLAN").join("recent.txt");
        assert!(
            store.is_file(),
            "recent.txt 必须落在本次运行目录：{}",
            store.display()
        );
        let text = std::fs::read_to_string(&store).unwrap();
        assert!(text.contains("a.xlsx"));
    }

    #[test]
    fn offline_network_entries_survive_and_pin_works() {
        let _env = TestEnv::new("recent-pin-network");
        let net_path = PathBuf::from(r"\\offline-nas\finance\report.xlsx");
        let local_a = PathBuf::from(r"C:\nonexistent\local_a.xlsx");
        touch(&local_a);
        touch(&net_path);
        assert!(prune_missing_async(None).is_none());
        assert_eq!(load().len(), 2);

        assert!(toggle_pin(&local_a));
        assert!(is_pinned(&local_a));
        let entries = load_entries();
        assert!(entries[0].pinned && entries[0].path == local_a);
        assert!(!entries[1].pinned && entries[1].path == net_path);

        // Manual prune keeps pinned & UNC network paths.
        assert_eq!(prune_missing_manual(), 0);
        assert_eq!(load().len(), 2);

        // Manual forget removes explicitly.
        forget(&net_path);
        assert_eq!(load().len(), 1);
    }
}
