//! 从共享目录发版：检查更新、暂存、退出时替换并重启。
//!
//! 没有服务器的好处在这里体现：把新版 exe 和一份 manifest.txt 丢进内网共享目录
//! （比如 `Z:\软件\Sheets`），大家启动时比对版本号，比本机新就悄悄下到本地，
//! 关窗时替换。
//!
//! manifest.txt 只有三行：
//! ```text
//! version=1.38
//! exe=SheetsNative-1.38.exe
//! sha256=<64 位十六进制>
//! ```
//!
//! 校验用 SHA-256：共享盘上文件被占、被半截复制的情况很常见，哈希不对就不装。

use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 当前版本。发版时改这里 + 共享目录里的 manifest。
pub const VERSION: &str = "1.37";

#[derive(Clone, Debug)]
pub struct Remote {
    pub version: String,
    pub exe: PathBuf,
    pub sha256: String,
}

fn app_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let d = PathBuf::from(base).join("SheetsNative");
    let _ = std::fs::create_dir_all(&d);
    d
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
    Some((version?, exe?, sha?))
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
    let Some((version, exe, sha256)) = parse_manifest(&text) else {
        return Err(std::io::Error::other("manifest.txt 格式不对"));
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
    Ok(Some(Remote { version, exe: exe_path, sha256 }))
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    use std::io::Read;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// 把新版复制到本地并校验哈希；校验不过就删掉，绝不安装坏文件
pub fn stage(remote: &Remote) -> std::io::Result<PathBuf> {
    let dir = app_dir().join("updates");
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(format!("pending-{}.exe", remote.version));
    std::fs::copy(&remote.exe, &target)?;
    let actual = sha256_of(&target)?;
    if actual != remote.sha256 {
        let _ = std::fs::remove_file(&target);
        return Err(std::io::Error::other(format!(
            "哈希不符（期望 {}，实际 {}），已丢弃",
            &remote.sha256[..8.min(remote.sha256.len())],
            &actual[..8]
        )));
    }
    Ok(target)
}

/// 关窗后替换并重启：写一个批处理等本进程退出，再搬运文件
pub fn apply_on_exit(staged: &Path) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let pid = std::process::id();
    let script = app_dir().join("apply-update.cmd");
    let body = format!(
        "@echo off\r\n\
         :wait\r\n\
         tasklist /FI \"PID eq {pid}\" | find \"{pid}\" >nul\r\n\
         if not errorlevel 1 (\r\n\
           timeout /t 1 /nobreak >nul\r\n\
           goto wait\r\n\
         )\r\n\
         copy /y \"{staged}\" \"{exe}\" >nul\r\n\
         start \"\" \"{exe}\"\r\n\
         del \"%~f0\"\r\n",
        pid = pid,
        staged = staged.display(),
        exe = exe.display()
    );
    let mut f = std::fs::File::create(&script)?;
    f.write_all(body.as_bytes())?;
    Ok(script)
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
        let text = "version=1.38\nexe=a.exe\nsha256=ABC123\n";
        let (v, e, s) = parse_manifest(text).unwrap();
        assert_eq!((v.as_str(), e.as_str(), s.as_str()), ("1.38", "a.exe", "abc123"));
        assert!(parse_manifest("version=1.0").is_none(), "缺字段应当拒绝");
    }
}
