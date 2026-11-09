//! 最近打开的文件 + 启动页数据。
//!
//! 存 `%LOCALAPPDATA%\SheetsNative\recent.txt`，一行一个路径，新的在上面。
//! 只留能打开得到的：文件被删或网盘断开时自动剔除，免得启动页一堆死链。

use std::path::{Path, PathBuf};

pub const MAX: usize = 12;

fn store() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let d = PathBuf::from(base).join("SheetsNative");
    let _ = std::fs::create_dir_all(&d);
    d.join("recent.txt")
}

pub fn load() -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(store()) else {
        return Vec::new();
    };
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .take(MAX)
        .collect()
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

    #[test]
    fn touch_moves_to_front_and_dedupes() {
        let d = std::env::temp_dir().join(format!("sheetflow_recent_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let a = d.join("a.xlsx");
        let b = d.join("b.xlsx");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();

        // SAFETY: 测试内单线程设置，仅用于把存储目录指到临时目录
        unsafe { std::env::set_var("LOCALAPPDATA", &d) };
        touch(&a);
        touch(&b);
        touch(&a);
        let list = load();
        assert_eq!(list.len(), 2, "同一文件不该重复出现");
        assert!(list[0].ends_with("a.xlsx"), "最近打开的排最前");
        std::fs::remove_dir_all(&d).ok();
    }
}
