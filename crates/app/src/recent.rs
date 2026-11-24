//! 最近打开的文件 + 启动页数据。
//!
//! 存 `%LOCALAPPDATA%\CELLAN\recent.txt`，一行一个路径，新的在上面。
//! 只留能打开得到的：文件被删或网盘断开时自动剔除，免得启动页一堆死链。

use std::path::{Path, PathBuf};

pub const MAX: usize = 12;

fn store() -> PathBuf {
    crate::debug_flags::app_dir().join("recent.txt")
}

/// 快速加载最近打开路径列表：启动热路径不做逐条同步 `p.is_file()`（避免断网 SMB/UNC 盘阻塞冷启动），
/// 失效路径由 `prune_missing_async` 在后台线程异步清理。
pub fn load() -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(store()) else {
        return Vec::new();
    };
    parse_lines(&text)
}

fn parse_lines(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .take(MAX)
        .collect()
}

/// 后台异步检测并剔除 `recent.txt` 中已失效的死链（仅在文件存在且非空时派生单次轻量线程；
/// 写回前比对原文以防与并发 `touch()` / 测试环境清理竞态）。
pub fn prune_missing_async(
    wake: Option<eframe::egui::Context>,
) -> Option<std::sync::mpsc::Receiver<Vec<PathBuf>>> {
    let path = store();
    let Ok(original) = std::fs::read_to_string(&path) else {
        return None;
    };
    let parsed = parse_lines(&original);
    if parsed.is_empty() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let kept: Vec<PathBuf> = parsed.into_iter().filter(|p| p.is_file()).collect();
        let body: String = kept.iter().map(|p| format!("{}\n", p.display())).collect();
        if body != original {
            // 仅当磁盘文件未被期间的 `touch`/`forget` 改动过且父目录仍在时才写回
            if std::fs::read_to_string(&path).ok().as_deref() == Some(original.as_str()) {
                let _ = std::fs::write(&path, body);
            }
            let _ = tx.send(kept);
            if let Some(ctx) = wake {
                ctx.request_repaint();
            }
        }
    });
    Some(rx)
}

/// 把一个文件提到最前（打开/保存时调用）
pub fn touch(path: &Path) {
    let mut list = load();
    list.retain(|p| !same(p, path));
    list.insert(0, path.to_path_buf());
    list.truncate(MAX);
    save(&list);
}

pub fn forget(path: &Path) {
    let mut list = load();
    let before = list.len();
    list.retain(|p| !same(p, path));
    if list.len() != before {
        save(&list);
    }
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
        // 文件名不同时无需触盘 `canonicalize`，避免对断网 SMB 路径阻塞
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

fn save(list: &[PathBuf]) {
    let body: String = list
        .iter()
        .map(|p| format!("{}\n", p.display()))
        .collect();
    let _ = std::fs::write(store(), body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;

    /// QA-03.1：LOCALAPPDATA 由守卫指向本次运行专用目录，测试结束自动恢复原值。
    /// 旧实现直接 `set_var("LOCALAPPDATA")` 且不还原，后续测试会读到别人的临时目录。
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
}
