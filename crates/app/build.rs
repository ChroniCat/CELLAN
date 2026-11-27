//! 把 assets/app.ico 编译进 exe（资源管理器/任务栏看到的图标）。
//!
//! 不引入 winres 之类的额外依赖：直接调用 Windows SDK 里的 rc.exe 生成 .res，
//! 再通过 cargo:rustc-link-arg 交给 MSVC 链接器。找不到 rc.exe 时只警告，
//! 不影响编译（图标缺失，但程序照常构建）。

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn verify_release_commit() {
    println!("cargo:rerun-if-env-changed=CELLAN_RELEASE_COMMIT");
    let mut refs = vec!["HEAD".to_string(), "packed-refs".to_string()];
    if let Some(branch) = git(&["symbolic-ref", "--quiet", "HEAD"]) {
        refs.push(branch);
    }
    for reference in refs {
        if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", &reference]) {
            if Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
    let revision = git(&["rev-parse", "--verify", "HEAD"])
        .filter(|rev| rev.len() >= 12 && rev.bytes().all(|ch| ch.is_ascii_hexdigit()));
    if let Ok(expected) = std::env::var("CELLAN_RELEASE_COMMIT") {
        assert_eq!(revision.as_ref(), Some(&expected), "发布提交与实际源码不符");
    }
}

fn main() {
    verify_release_commit();
    println!("cargo:rerun-if-env-changed=CELLAN_DEFAULT_UPDATE_URL");
    println!("cargo:rerun-if-env-changed=CELLAN_UPDATE_PUBKEY");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let ico = manifest.join("../../assets/app.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    if !ico.exists() {
        println!("cargo:warning=缺少 {}，exe 图标不打包", ico.display());
        return;
    }
    if !cfg!(target_os = "windows") {
        return;
    }
    let Some(rc) = find_rc() else {
        println!("cargo:warning=没找到 rc.exe（Windows SDK），exe 图标不打包");
        return;
    };
    let rc_file = out.join("app.rc");
    let res_file = out.join("app.res");
    // rc 的路径里反斜杠要转义，用正斜杠最稳；同时打入 VERSIONINFO，让资源管理器与任务管理器显示 CELLAN 与统一版本号
    let ico_arg = ico.display().to_string().replace('\\', "/");
    let pkg_ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.70.0".into());
    let parts: Vec<u32> = pkg_ver
        .split('.')
        .filter_map(|x| x.parse::<u32>().ok())
        .collect();
    let (maj, min, pat) = (
        parts.first().copied().unwrap_or(1),
        parts.get(1).copied().unwrap_or(70),
        parts.get(2).copied().unwrap_or(0),
    );
    let short_ver = format!("{maj}.{min}");
    let rc_body = format!(
        "1 ICON \"{ico_arg}\"\n\
1 VERSIONINFO\n\
FILEVERSION {maj},{min},{pat},0\n\
PRODUCTVERSION {maj},{min},{pat},0\n\
FILEFLAGSMASK 0x3fL\n\
FILEFLAGS 0x0L\n\
FILEOS 0x40004L\n\
FILETYPE 0x1L\n\
FILESUBTYPE 0x0L\n\
BEGIN\n\
    BLOCK \"StringFileInfo\"\n\
    BEGIN\n\
        BLOCK \"040904b0\"\n\
        BEGIN\n\
            VALUE \"CompanyName\", \"CELLAN\\0\"\n\
            VALUE \"FileDescription\", \"CELLAN - Concurrent Engine for Local-first, Lock-free Async Networks\\0\"\n\
            VALUE \"FileVersion\", \"{maj}.{min}.{pat}.0\\0\"\n\
            VALUE \"InternalName\", \"CELLAN\\0\"\n\
            VALUE \"OriginalFilename\", \"CELLAN.exe\\0\"\n\
            VALUE \"ProductName\", \"CELLAN\\0\"\n\
            VALUE \"ProductVersion\", \"{short_ver}\\0\"\n\
        END\n\
    END\n\
    BLOCK \"VarFileInfo\"\n\
    BEGIN\n\
        VALUE \"Translation\", 0x409, 1200\n\
    END\n\
END\n"
    );
    if std::fs::write(&rc_file, rc_body).is_err() {
        return;
    }
    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_file)
        .arg(&rc_file)
        .status();
    match status {
        Ok(s) if s.success() && res_file.exists() => {
            println!("cargo:rustc-link-arg={}", res_file.display());
            println!("cargo:warning=已把 app.ico 打进 exe（{}）", rc.display());
        }
        Ok(s) => println!("cargo:warning=rc.exe 退出码 {s}，exe 图标不打包"),
        Err(e) => println!("cargo:warning=调用 rc.exe 失败：{e}"),
    }
}

/// 找 rc.exe：PATH 里没有就去 Windows Kits 目录挑最高版本（x64）
fn find_rc() -> Option<PathBuf> {
    if let Ok(out) = Command::new("rc.exe").arg("/?").output() {
        if out.status.success() || !out.stderr.is_empty() {
            return Some(PathBuf::from("rc.exe"));
        }
    }
    let roots = [
        std::env::var("ProgramFiles(x86)").ok(),
        std::env::var("ProgramFiles").ok(),
    ];
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for root in roots.into_iter().flatten() {
        let kits = Path::new(&root).join("Windows Kits").join("10").join("bin");
        let Ok(rd) = std::fs::read_dir(&kits) else { continue };
        for e in rd.flatten() {
            let p = e.path().join("x64").join("rc.exe");
            if !p.exists() {
                continue;
            }
            let ver: Vec<u32> = e
                .file_name()
                .to_string_lossy()
                .split('.')
                .filter_map(|s| s.parse().ok())
                .collect();
            if best.as_ref().map(|(v, _)| ver > *v).unwrap_or(true) {
                best = Some((ver, p));
            }
        }
    }
    best.map(|(_, p)| p)
}
