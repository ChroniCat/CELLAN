//! 从共享目录发版：只检查、只提示，不自动安装。
//!
//! 发版目录（比如 `Z:\软件\Sheets`）里放新版 exe 和一份 manifest.txt：
//! ```text
//! version=1.70
//! exe=CELLAN-1.70.exe
//! sha256=<64 位十六进制>
//! ```
//!
//! 为什么不自动安装：manifest 和 exe 放在同一个可写共享目录里，SHA-256 只能发现
//! 「复制了半截」，发现不了「有人连 exe 带 manifest 一起换掉」——任何能写这个目录的人
//! 都能借自动更新在每台电脑上运行任意程序。在发版带上签名校验之前，这里只告诉用户
//! 「有新版本」，安装交给管理员的正式安装包。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 产品正式名称（标题栏、开始页、关于窗口、应用数据目录共用）
pub const APP_NAME: &str = "CELLAN";

/// 产品英文全称（关于窗口、文档与元数据展示）
pub const APP_FULL_NAME: &str = "Concurrent Engine for Local-first, Lock-free Async Networks";

/// 当前版本（全应用唯一版本常量：开始页、关于窗口、检查更新、打包元数据共用）。
pub const VERSION: &str = "1.70";

#[derive(Clone, Debug)]
pub struct Remote {
    pub version: String,
    pub exe: PathBuf,
}

/// 应用级配置目录 `%LOCALAPPDATA%\CELLAN`。
/// 首次升级时若 `%LOCALAPPDATA%\CELLAN` 尚未建立，按先后顺序从旧目录 `LANCell` 或 `SheetsNative`
/// 自动拷贝已有配置与最近文件列表，老用户的显示名与最近记录无缝保留。
pub fn app_dir_in(base: &Path) -> PathBuf {
    let d = base.join(APP_NAME);
    if !d.exists() {
        let _ = std::fs::create_dir_all(&d);
        for legacy_name in ["LANCell", "SheetsNative"] {
            let legacy = base.join(legacy_name);
            if legacy.is_dir() {
                for name in [
                    "ui.ini",
                    "recent.txt",
                    "update.txt",
                    "roots.txt",
                    "fnrecent.txt",
                    "cmdusage.txt",
                    "errorcheck.ini",
                ] {
                    let src = legacy.join(name);
                    let dst = d.join(name);
                    if src.is_file() && !dst.exists() {
                        let _ = std::fs::copy(&src, &dst);
                    }
                }
            }
        }
    } else {
        let _ = std::fs::create_dir_all(&d);
    }
    d
}

pub fn app_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    app_dir_in(Path::new(&base))
}

pub fn app_dir_opt() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA").ok()?;
    Some(app_dir_in(Path::new(&base)))
}

fn settings() -> PathBuf {
    app_dir().join("update.txt")
}

/// 记住发版目录（用户在菜单里选一次）
pub fn set_dir(dir: &Path) -> std::io::Result<()> {
    let mut f = std::fs::File::create(settings())?;
    writeln!(f, "dir={}", dir.display())
}

pub fn dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("SC_UPDATE_DIR") {
        return Some(PathBuf::from(v));
    }
    let text = std::fs::read_to_string(settings()).ok()?;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("dir=") {
            let p = PathBuf::from(v.trim());
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    None
}

fn parse_manifest(text: &str) -> Option<(String, String, String)> {
    let mut version = None;
    let mut exe = None;
    let mut sha = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("version=") {
            version = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("exe=") {
            exe = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("sha256=") {
            sha = Some(v.trim().to_lowercase());
        }
    }
    let (version, exe, sha) = (version?, exe?, sha?);
    // manifest 来自共享盘，不可信：版本号只能是数字和点；exe 只能是发版目录里的一个 .exe 文件名，
    // 不能带路径分隔符、盘符或 `..` 指到目录外面；哈希必须是 64 位十六进制
    let version_ok = !version.is_empty()
        && version.len() <= 32
        && version.chars().all(|c| c.is_ascii_digit() || c == '.');
    let exe_ok = !exe.is_empty()
        && exe.len() <= 128
        && !exe.contains(['/', '\\', ':'])
        && !exe.contains("..")
        && exe.to_ascii_lowercase().ends_with(".exe");
    let sha_ok = sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit());
    (version_ok && exe_ok && sha_ok).then_some((version, exe, sha))
}

/// 版本号比较：按点分段，数值比大小（"1.10" 比 "1.9" 新）
pub fn is_newer(remote: &str, local: &str) -> bool {
    let a: Vec<u64> = remote.split('.').filter_map(|x| x.parse().ok()).collect();
    let b: Vec<u64> = local.split('.').filter_map(|x| x.parse().ok()).collect();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 看一眼共享目录，有比本机新的就返回
pub fn check(dir: &Path) -> std::io::Result<Option<Remote>> {
    let text = std::fs::read_to_string(dir.join("manifest.txt"))?;
    let Some((version, exe, _sha256)) = parse_manifest(&text) else {
        return Err(std::io::Error::other(
            "manifest.txt 格式不对（version 只能是数字与点，exe 只能是发版目录里的 .exe 文件名，sha256 须为 64 位十六进制）",
        ));
    };
    if !is_newer(&version, VERSION) {
        return Ok(None);
    }
    let exe_path = dir.join(exe);
    if !exe_path.is_file() {
        return Err(std::io::Error::other(format!(
            "manifest 指向的 {} 不存在",
            exe_path.display()
        )));
    }
    Ok(Some(Remote { version, exe: exe_path }))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("1.38", "1.37"));
        assert!(is_newer("1.10", "1.9"));
        assert!(!is_newer("1.37", "1.37"));
        assert!(!is_newer("1.2", "1.37"));
    }

    #[test]
    fn manifest_parsing() {
        let sha = "AB".repeat(32);
        let text = format!("version=1.38\nexe=a.exe\nsha256={sha}\n");
        let (v, e, s) = parse_manifest(&text).unwrap();
        assert_eq!((v.as_str(), e.as_str(), s.as_str()), ("1.38", "a.exe", "ab".repeat(32).as_str()));
        assert!(parse_manifest("version=1.0").is_none(), "缺字段应当拒绝");
    }

    #[test]
    fn manifest_from_the_share_cannot_point_outside_the_release_folder() {
        let sha = "0".repeat(64);
        let bad = [
            format!("version=1.99\nexe=..\\..\\Windows\\evil.exe\nsha256={sha}"),
            format!("version=1.99\nexe=C:\\evil.exe\nsha256={sha}"),
            format!("version=1.99\nexe=\\\\host\\share\\evil.exe\nsha256={sha}"),
            format!("version=1.99\nexe=sub/evil.exe\nsha256={sha}"),
            format!("version=1.99\nexe=evil.cmd\nsha256={sha}"),
            format!("version=1.99;calc\nexe=a.exe\nsha256={sha}"),
            "version=1.99\nexe=a.exe\nsha256=短".to_string(),
        ];
        for text in bad {
            assert!(parse_manifest(&text).is_none(), "应拒绝：{text}");
        }
    }

    #[test]
    fn newer_version_is_reported_but_nothing_is_installed() {
        let dir = crate::test_env::scratch("update-notice-only");
        std::fs::write(dir.join("CELLAN-9.0.exe"), b"not really an exe").unwrap();
        std::fs::write(
            dir.join("manifest.txt"),
            format!("version=9.0\nexe=CELLAN-9.0.exe\nsha256={}\n", "0".repeat(64)),
        )
        .unwrap();
        let remote = check(&dir).unwrap().expect("应发现新版本");
        assert_eq!(remote.version, "9.0");
        assert_eq!(remote.exe, dir.join("CELLAN-9.0.exe"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_matches_cargo_package_and_migrates_legacy_profile() {
        assert_eq!(APP_NAME, "CELLAN");
        assert_eq!(
            APP_FULL_NAME,
            "Concurrent Engine for Local-first, Lock-free Async Networks"
        );
        let pkg = env!("CARGO_PKG_VERSION");
        assert!(
            pkg.starts_with(&format!("{VERSION}.")),
            "Cargo.toml version ({pkg}) 必须与 update::VERSION ({VERSION}) 对齐"
        );
        let root = crate::test_env::scratch("cellan-migrate");
        let legacy = root.join("SheetsNative");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("ui.ini"), "user_name=王五\n").unwrap();
        let migrated = app_dir_in(&root);
        assert_eq!(migrated, root.join("CELLAN"));
        assert_eq!(
            std::fs::read_to_string(migrated.join("ui.ini")).unwrap(),
            "user_name=王五\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
