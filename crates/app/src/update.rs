//! 内网零配置签名自更新引擎（HTTP 固定发版源 + 局域网实时协作无感接力 + 兼容共享目录）。
//!
//! 安全模型：
//! - 发版端持有本机 ECDSA P-256 私钥（`%USERPROFILE%\.cellan-release\signing-key.blob`，不进仓库），
//!   客户端内置对应公钥 [`DEFAULT_PUBKEY_HEX`]。
//! - 每次打包生成 `manifest.txt`（含 `version / exe / size / sha256 / sig / notes`），并将签名清单作为
//!   PE Overlay 尾部标记（`CLNUPD01`）直接附着在 `CELLAN.exe` 末尾。
//! - 无论是从发版机 HTTP 静态源拉取，还是从「局域网实时协作」房间里版本更高的同事客户端接力拉取，
//!   客户端在落盘替换前都必须同时通过 **ECDSA P-256 公钥验签** 与 **全文件 SHA-256 摘要校验**。
//!   未签名的本地开发调试构建不含 `CLNUPD01` 尾部，绝不会向同事广播半成品。

use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 产品正式名称（标题栏、开始页、关于窗口、应用数据目录共用）
pub const APP_NAME: &str = "CELLAN";

/// 产品英文全称（关于窗口、文档与元数据展示）
pub const APP_FULL_NAME: &str = "Concurrent Engine for Local-first, Lock-free Async Networks";

/// 当前版本（全应用唯一版本常量：开始页、关于窗口、检查更新、打包元数据共用）。
pub const VERSION: &str = "1.82";

/// 内网 HTTP 发版服务默认端口（避开常见开发端口与协作端口 7878 / 单实例端口 48765）
pub const DEFAULT_UPDATE_PORT: u16 = 48_768;

/// 官方发版签名公钥（ECDSA P-256 未压缩坐标 `X || Y`，64 字节 = 128 位十六进制）。
/// 首次运行 `scripts/publish.ps1` 时自动生成密钥对并写入此处。
pub const DEFAULT_PUBKEY_HEX: &str = "f77e05d4bf68673e27454d54fbe2c881e0996e640d037c417e9729bf941a51de40359d15e20ca966f23b69c22fa25235baba56ebb3ea748edfc31d51cbcd6054";

/// 编译期注入的默认内网更新地址（由 `scripts/publish.ps1` 自动读取发版机当前内网 IP 注入）。
pub const COMPILED_UPDATE_URL: Option<&str> = option_env!("CELLAN_DEFAULT_UPDATE_URL");

/// 编译期可选覆盖公钥（便于 CI 或自定义打包注入）
pub const COMPILED_PUBKEY_HEX: Option<&str> = option_env!("CELLAN_UPDATE_PUBKEY");

/// PE Overlay 尾部魔数：标记该 `CELLAN.exe` 末尾附带了官方签名清单
pub const TRAILER_MAGIC: &[u8; 8] = b"CLNUPD01";

const MAX_MANIFEST_BYTES: usize = 8 * 1024;
const MAX_EXE_BYTES: u64 = 256 * 1024 * 1024;
const IO_BUF_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub struct Remote {
    pub version: String,
    pub exe: PathBuf,
}

/// 经过格式与签名校验的发版清单
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedManifest {
    pub version: String,
    pub exe: String,
    pub size: u64,
    pub sha256: String,
    pub sig: String,
    pub notes: String,
}

/// 发现的可用更新来源
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateSource {
    /// 内网 HTTP 发版源（如 `http://192.168.x.x:48768`）
    Http { base_url: String },
    /// 局域网实时协作房间内持有官方签名包的成员接力供种
    CollabPeer { peer_site: u32, peer_name: String },
    /// 已配置并带有合法签名的共享盘目录
    SharedDir { dir: PathBuf },
    /// 旧式无签名共享盘（仅提示，拒绝自动执行替换）
    LegacyNoticeOnly { exe_path: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub manifest: SignedManifest,
    pub source: UpdateSource,
}

impl AvailableUpdate {
    pub fn source_label(&self) -> String {
        match &self.source {
            UpdateSource::Http { base_url } => format!("内网发版服务（{base_url}）"),
            UpdateSource::CollabPeer { peer_name, .. } => {
                format!("局域网协作成员「{peer_name}」接力同步")
            }
            UpdateSource::SharedDir { dir } => format!("共享发版目录（{}）", dir.display()),
            UpdateSource::LegacyNoticeOnly { exe_path } => {
                format!("共享目录未签名包（{}）", exe_path.display())
            }
        }
    }

    pub fn can_auto_install(&self) -> bool {
        !matches!(self.source, UpdateSource::LegacyNoticeOnly { .. })
    }
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

#[derive(Default)]
struct SavedSettings {
    dir: Option<PathBuf>,
    url: Option<String>,
}

fn load_saved_settings() -> SavedSettings {
    let mut out = SavedSettings::default();
    let Ok(text) = std::fs::read_to_string(settings()) else {
        return out;
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("dir=") {
            let v = v.trim();
            if !v.is_empty() {
                out.dir = Some(PathBuf::from(v));
            }
        } else if let Some(v) = line.strip_prefix("url=") {
            let v = v.trim();
            if !v.is_empty() {
                out.url = Some(v.to_string());
            }
        }
    }
    out
}

fn write_saved_settings(st: &SavedSettings) -> std::io::Result<()> {
    let p = settings();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut f = std::fs::File::create(p)?;
    if let Some(url) = &st.url {
        writeln!(f, "url={url}")?;
    }
    if let Some(dir) = &st.dir {
        writeln!(f, "dir={}", dir.display())?;
    }
    Ok(())
}

/// 记住发版目录（兼容旧接口）
#[allow(dead_code)]
pub fn set_dir(dir: &Path) -> std::io::Result<()> {
    let mut st = load_saved_settings();
    st.dir = Some(dir.to_path_buf());
    write_saved_settings(&st)
}

/// 记住曾成功验签连通的内网 HTTP 更新地址
pub fn remember_http_url(url: &str) {
    let trimmed = url.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return;
    }
    let mut st = load_saved_settings();
    if st.url.as_deref() == Some(trimmed.as_str()) {
        return;
    }
    st.url = Some(trimmed);
    let _ = write_saved_settings(&st);
}

pub fn dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("SC_UPDATE_DIR") {
        let t = v.trim();
        if !t.is_empty() {
            return Some(PathBuf::from(t));
        }
    }
    // 避免在 UI 主线程同步对可能离线的 SMB 共享路径执行 `is_dir()` 阻塞；
    // 真正的可达性检查推迟到后台线程的 `probe_all_sources()` 中执行。
    load_saved_settings().dir
}

/// 按优先级汇总候选 HTTP 更新地址（去重）：
/// 1. `SC_UPDATE_URL` 环境变量（测试或临时指定；设为空串或 `off` 时禁用所有默认 HTTP 源）
/// 2. 编译期自动打入的发版机地址 `CELLAN_DEFAULT_UPDATE_URL`
/// 3. `%LOCALAPPDATA%\CELLAN\update.txt` 中记录的 `url=`
pub fn candidate_http_urls() -> Vec<String> {
    let mut urls = Vec::new();
    let mut push_unique = |raw: &str| {
        let clean = raw.trim().trim_end_matches('/').to_string();
        if !clean.is_empty()
            && (clean.starts_with("http://") || clean.starts_with("https://"))
            && !urls.contains(&clean)
        {
            urls.push(clean);
        }
    };
    if let Ok(env_url) = std::env::var("SC_UPDATE_URL") {
        let t = env_url.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("off") || t.eq_ignore_ascii_case("none") {
            return Vec::new();
        }
        for part in t.split(',') {
            push_unique(part);
        }
        return urls;
    }
    if let Some(compiled) = COMPILED_UPDATE_URL {
        push_unique(compiled);
    }
    if let Some(saved) = load_saved_settings().url {
        push_unique(&saved);
    }
    urls
}

/// 当前生效的发版公钥（128 位十六进制）
pub fn active_pubkey_hex() -> Option<String> {
    #[cfg(any(test, debug_assertions))]
    if let Ok(v) = std::env::var("SC_UPDATE_PUBKEY") {
        let t = v.trim().to_ascii_lowercase();
        if is_valid_hex(&t, 128) {
            return Some(t);
        }
        if t == "off" || t == "none" {
            return None;
        }
    }
    if let Some(v) = COMPILED_PUBKEY_HEX {
        let t = v.trim().to_ascii_lowercase();
        if is_valid_hex(&t, 128) {
            return Some(t);
        }
    }
    let t = DEFAULT_PUBKEY_HEX.trim().to_ascii_lowercase();
    if is_valid_hex(&t, 128) {
        Some(t)
    } else {
        None
    }
}

fn is_valid_hex(s: &str, expected_len: usize) -> bool {
    s.len() == expected_len && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn sanitize_notes(raw: &str) -> String {
    let joined = raw.replace("\r\n", "\n").replace('\r', "\n");
    let parts: Vec<&str> = joined
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let single = parts.join("；");
    let clipped: String = single
        .chars()
        .filter(|c| !c.is_control())
        .take(500)
        .collect();
    clipped.trim().to_string()
}

pub fn is_valid_version_str(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 32
        && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        && version
            .split('.')
            .all(|seg| !seg.is_empty() && seg.len() <= 9 && seg.parse::<u64>().is_ok())
}

pub fn is_safe_exe_filename(exe: &str) -> bool {
    !exe.is_empty()
        && exe.len() <= 128
        && !exe.contains(['/', '\\', ':'])
        && !exe.contains("..")
        && exe.to_ascii_lowercase().ends_with(".exe")
}

/// 构造待签名的规范字节串（Rust 端与 `scripts/publish.ps1` 严格字节级一致）
pub fn canonical_signing_bytes(version: &str, size: u64, sha256: &str, notes: &str) -> Vec<u8> {
    let clean_notes = sanitize_notes(notes);
    format!(
        "CELLAN-RELEASE-V1\nversion={}\nsize={}\nsha256={}\nnotes={}\n",
        version.trim(),
        size,
        sha256.trim().to_ascii_lowercase(),
        clean_notes
    )
    .into_bytes()
}

impl SignedManifest {
    pub fn to_manifest_text(&self) -> String {
        let clean_notes = sanitize_notes(&self.notes);
        format!(
            "version={}\nexe={}\nsize={}\nsha256={}\nsig={}\nnotes={}\n",
            self.version.trim(),
            self.exe.trim(),
            self.size,
            self.sha256.trim().to_ascii_lowercase(),
            self.sig.trim().to_ascii_lowercase(),
            clean_notes
        )
    }
}

/// 解析新版带签名的 `manifest.txt`
pub fn parse_signed_manifest(text: &str) -> Option<SignedManifest> {
    let text = text.trim_start_matches('\u{feff}');
    let mut version = None;
    let mut exe = None;
    let mut size: Option<u64> = None;
    let mut sha256 = None;
    let mut sig = None;
    let mut notes = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(v) = line.strip_prefix("version=") {
            version = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("exe=") {
            exe = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("size=") {
            size = v.trim().parse::<u64>().ok();
        } else if let Some(v) = line.strip_prefix("sha256=") {
            sha256 = Some(v.trim().to_ascii_lowercase());
        } else if let Some(v) = line.strip_prefix("sig=") {
            sig = Some(v.trim().to_ascii_lowercase());
        } else if let Some(v) = line.strip_prefix("notes=") {
            notes = sanitize_notes(v);
        }
    }
    let version = version?;
    let exe = exe.unwrap_or_else(|| "CELLAN.exe".to_string());
    let size = size?;
    let sha256 = sha256?;
    let sig = sig?;
    if !is_valid_version_str(&version)
        || !is_safe_exe_filename(&exe)
        || size == 0
        || size > MAX_EXE_BYTES
        || !is_valid_hex(&sha256, 64)
        || !is_valid_hex(&sig, 128)
    {
        return None;
    }
    Some(SignedManifest {
        version,
        exe,
        size,
        sha256,
        sig,
        notes,
    })
}

/// 兼容旧版 3 字段 `manifest.txt`（仅用于旧测试和无签名共享盘提示）
fn parse_manifest(text: &str) -> Option<(String, String, String)> {
    let text = text.trim_start_matches('\u{feff}');
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
    let version_ok = is_valid_version_str(&version);
    let exe_ok = is_safe_exe_filename(&exe);
    let sha_ok = is_valid_hex(&sha, 64);
    (version_ok && exe_ok && sha_ok).then_some((version, exe, sha))
}

/// 版本号比较：按点分段，数值比大小（"1.10" 比 "1.9" 新）；任意段解析溢出或非数字时返回 false。
pub fn is_newer(remote: &str, local: &str) -> bool {
    let Ok(a) = remote
        .split('.')
        .map(|x| x.parse::<u64>())
        .collect::<Result<Vec<u64>, _>>()
    else {
        return false;
    };
    let Ok(b) = local
        .split('.')
        .map(|x| x.parse::<u64>())
        .collect::<Result<Vec<u64>, _>>()
    else {
        return false;
    };
    if a.is_empty() || b.is_empty() {
        return false;
    }
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 旧共享目录检查接口（兼容保留）：若无有效签名，仅提示版本号与位置，不自动安装。
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
    Ok(Some(Remote {
        version,
        exe: exe_path,
    }))
}

// ---------------------------------------------------------------------------
// Windows CNG (bcrypt.dll) 原生 ECDSA P-256 验签（零外部依赖，保持 --offline 构建）
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub(crate) mod cng {
    use core::ffi::c_void;

    type NtStatus = i32;
    type AlgHandle = *mut c_void;
    type KeyHandle = *mut c_void;

    const BCRYPT_ECDSA_PUBLIC_P256_MAGIC: u32 = 0x3153_4345; // "ECS1"

    #[repr(C)]
    struct BcryptEccKeyBlob {
        dw_magic: u32,
        cb_key: u32,
    }

    #[link(name = "bcrypt")]
    unsafe extern "system" {
        fn BCryptOpenAlgorithmProvider(
            ph_algorithm: *mut AlgHandle,
            psz_alg_id: *const u16,
            psz_implementation: *const u16,
            dw_flags: u32,
        ) -> NtStatus;
        fn BCryptCloseAlgorithmProvider(h_algorithm: AlgHandle, dw_flags: u32) -> NtStatus;
        fn BCryptImportKeyPair(
            h_algorithm: AlgHandle,
            h_import_key: KeyHandle,
            psz_blob_type: *const u16,
            ph_key: *mut KeyHandle,
            pb_input: *const u8,
            cb_input: u32,
            dw_flags: u32,
        ) -> NtStatus;
        fn BCryptDestroyKey(h_key: KeyHandle) -> NtStatus;
        fn BCryptVerifySignature(
            h_key: KeyHandle,
            p_padding_info: *const c_void,
            pb_hash: *const u8,
            cb_hash: u32,
            pb_signature: *const u8,
            cb_signature: u32,
            dw_flags: u32,
        ) -> NtStatus;

        #[cfg(test)]
        fn BCryptGenerateKeyPair(
            h_algorithm: AlgHandle,
            ph_key: *mut KeyHandle,
            dw_length: u32,
            dw_flags: u32,
        ) -> NtStatus;
        #[cfg(test)]
        fn BCryptFinalizeKeyPair(h_key: KeyHandle, dw_flags: u32) -> NtStatus;
        #[cfg(test)]
        fn BCryptExportKey(
            h_key: KeyHandle,
            h_export_key: KeyHandle,
            psz_blob_type: *const u16,
            pb_output: *mut u8,
            cb_output: u32,
            pcb_result: *mut u32,
            dw_flags: u32,
        ) -> NtStatus;
        #[cfg(test)]
        fn BCryptSignHash(
            h_key: KeyHandle,
            p_padding_info: *const c_void,
            pb_input: *const u8,
            cb_input: u32,
            pb_output: *mut u8,
            cb_output: u32,
            pcb_result: *mut u32,
            dw_flags: u32,
        ) -> NtStatus;
    }

    fn wide_z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(core::iter::once(0)).collect()
    }

    pub fn verify_p256_sha256(pubkey_xy: &[u8; 64], digest: &[u8; 32], sig_rs: &[u8; 64]) -> bool {
        let alg_name = wide_z("ECDSA_P256");
        let blob_type = wide_z("ECCPUBLICBLOB");

        let mut alg: AlgHandle = core::ptr::null_mut();
        let st = unsafe {
            BCryptOpenAlgorithmProvider(&mut alg, alg_name.as_ptr(), core::ptr::null(), 0)
        };
        if st != 0 || alg.is_null() {
            return false;
        }

        let mut blob = Vec::with_capacity(8 + 64);
        let hdr = BcryptEccKeyBlob {
            dw_magic: BCRYPT_ECDSA_PUBLIC_P256_MAGIC,
            cb_key: 32,
        };
        blob.extend_from_slice(&hdr.dw_magic.to_le_bytes());
        blob.extend_from_slice(&hdr.cb_key.to_le_bytes());
        blob.extend_from_slice(pubkey_xy);

        let mut key: KeyHandle = core::ptr::null_mut();
        let import_st = unsafe {
            BCryptImportKeyPair(
                alg,
                core::ptr::null_mut(),
                blob_type.as_ptr(),
                &mut key,
                blob.as_ptr(),
                blob.len() as u32,
                0,
            )
        };
        if import_st != 0 || key.is_null() {
            unsafe {
                let _ = BCryptCloseAlgorithmProvider(alg, 0);
            }
            return false;
        }

        let verify_st = unsafe {
            BCryptVerifySignature(
                key,
                core::ptr::null(),
                digest.as_ptr(),
                digest.len() as u32,
                sig_rs.as_ptr(),
                sig_rs.len() as u32,
                0,
            )
        };

        unsafe {
            let _ = BCryptDestroyKey(key);
            let _ = BCryptCloseAlgorithmProvider(alg, 0);
        }
        verify_st == 0
    }

    #[cfg(test)]
    pub struct TestKeyPair {
        alg: AlgHandle,
        key: KeyHandle,
        pub pubkey_hex: String,
    }

    #[cfg(test)]
    impl TestKeyPair {
        pub fn generate() -> Self {
            let alg_name = wide_z("ECDSA_P256");
            let blob_type = wide_z("ECCPUBLICBLOB");
            let mut alg: AlgHandle = core::ptr::null_mut();
            assert_eq!(
                unsafe {
                    BCryptOpenAlgorithmProvider(&mut alg, alg_name.as_ptr(), core::ptr::null(), 0)
                },
                0
            );
            let mut key: KeyHandle = core::ptr::null_mut();
            assert_eq!(unsafe { BCryptGenerateKeyPair(alg, &mut key, 256, 0) }, 0);
            assert_eq!(unsafe { BCryptFinalizeKeyPair(key, 0) }, 0);
            let mut buf = [0u8; 72];
            let mut got = 0u32;
            assert_eq!(
                unsafe {
                    BCryptExportKey(
                        key,
                        core::ptr::null_mut(),
                        blob_type.as_ptr(),
                        buf.as_mut_ptr(),
                        buf.len() as u32,
                        &mut got,
                        0,
                    )
                },
                0
            );
            assert_eq!(got, 72);
            Self {
                alg,
                key,
                pubkey_hex: hex::encode(&buf[8..72]),
            }
        }

        pub fn sign_digest(&self, digest: &[u8; 32]) -> String {
            let mut sig = [0u8; 64];
            let mut got = 0u32;
            assert_eq!(
                unsafe {
                    BCryptSignHash(
                        self.key,
                        core::ptr::null(),
                        digest.as_ptr(),
                        digest.len() as u32,
                        sig.as_mut_ptr(),
                        sig.len() as u32,
                        &mut got,
                        0,
                    )
                },
                0
            );
            assert_eq!(got, 64);
            hex::encode(sig)
        }
    }

    #[cfg(test)]
    impl Drop for TestKeyPair {
        fn drop(&mut self) {
            unsafe {
                let _ = BCryptDestroyKey(self.key);
                let _ = BCryptCloseAlgorithmProvider(self.alg, 0);
            }
        }
    }
}

/// 校验 `SignedManifest` 的 ECDSA P-256 数字签名（使用指定公钥十六进制串）
pub fn verify_manifest_sig_with_pubkey(manifest: &SignedManifest, pubkey_hex: &str) -> bool {
    let Ok(pub_vec) = hex::decode(pubkey_hex.trim()) else {
        return false;
    };
    let Ok(pubkey_xy): Result<[u8; 64], _> = pub_vec.try_into() else {
        return false;
    };
    let Ok(sig_vec) = hex::decode(manifest.sig.trim()) else {
        return false;
    };
    let Ok(sig_rs): Result<[u8; 64], _> = sig_vec.try_into() else {
        return false;
    };
    let msg = canonical_signing_bytes(
        &manifest.version,
        manifest.size,
        &manifest.sha256,
        &manifest.notes,
    );
    let mut hasher = Sha256::new();
    hasher.update(&msg);
    let digest_ga = hasher.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&digest_ga);

    #[cfg(windows)]
    {
        cng::verify_p256_sha256(&pubkey_xy, &digest, &sig_rs)
    }
    #[cfg(not(windows))]
    {
        let _ = (pubkey_xy, digest, sig_rs);
        false
    }
}

/// 用客户端当前生效的官方公钥校验发版清单签名
pub fn verify_manifest_sig(manifest: &SignedManifest) -> bool {
    let Some(pk) = active_pubkey_hex() else {
        return false;
    };
    verify_manifest_sig_with_pubkey(manifest, &pk)
}

// ---------------------------------------------------------------------------
// PE Overlay 尾部清单读写与流式文件完整性校验
// ---------------------------------------------------------------------------

/// 生成附着在 `CELLAN.exe` 末尾的 `CLNUPD01` 尾部字节块
pub fn build_trailer_bytes(manifest: &SignedManifest) -> Vec<u8> {
    let text = manifest.to_manifest_text();
    let body = text.as_bytes();
    let len = body.len() as u32;
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(body);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(TRAILER_MAGIC);
    out
}

fn sha256_prefix_of_file(path: &Path, byte_count: u64) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("打开更新文件失败：{e}"))?;
    let total_len = f
        .metadata()
        .map_err(|e| format!("读取文件大小失败：{e}"))?
        .len();
    if total_len < byte_count {
        return Err(format!(
            "安装包不完整（预期至少 {} 字节，实际 {} 字节）",
            byte_count, total_len
        ));
    }
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; IO_BUF_BYTES];
    let mut remaining = byte_count;
    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        let n = f
            .read(&mut buf[..to_read])
            .map_err(|e| format!("读取更新文件失败：{e}"))?;
        if n == 0 {
            return Err("读取更新文件时意外遇到文件末尾".into());
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    Ok(hex::encode(hasher.finalize()))
}

/// 边写盘边累加前 `expected_prefix_len` 字节的 SHA-256，消除下载落盘后的整包重读哈希。
pub struct PrefixSha256 {
    hasher: Sha256,
    remaining: u64,
}

impl PrefixSha256 {
    pub fn new(expected_prefix_len: u64) -> Self {
        Self {
            hasher: Sha256::new(),
            remaining: expected_prefix_len,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        if self.remaining == 0 || bytes.is_empty() {
            return;
        }
        let take = (bytes.len() as u64).min(self.remaining) as usize;
        self.hasher.update(&bytes[..take]);
        self.remaining -= take as u64;
    }

    pub fn finalize_hex(self) -> Option<String> {
        (self.remaining == 0).then(|| hex::encode(self.hasher.finalize()))
    }
}

/// 仅校验可执行文件末尾的 `CLNUPD01` 签名清单与文件长度自洽性（只读末尾 <= 8 KB，不做全文件哈希）。
pub fn inspect_signed_exe_trailer(exe_path: &Path) -> Option<SignedManifest> {
    let mut f = std::fs::File::open(exe_path).ok()?;
    let file_len = f.metadata().ok()?.len();
    if file_len < 16 {
        return None;
    }
    f.seek(SeekFrom::End(-12)).ok()?;
    let mut tail = [0u8; 12];
    f.read_exact(&mut tail).ok()?;
    if &tail[4..12] != TRAILER_MAGIC {
        return None;
    }
    let manifest_len = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) as usize;
    if manifest_len == 0 || manifest_len > MAX_MANIFEST_BYTES {
        return None;
    }
    let trailer_total = (manifest_len + 12) as u64;
    if file_len <= trailer_total {
        return None;
    }
    f.seek(SeekFrom::End(-(trailer_total as i64))).ok()?;
    let mut raw = vec![0u8; manifest_len];
    f.read_exact(&mut raw).ok()?;
    let text = std::str::from_utf8(&raw).ok()?;
    let manifest = parse_signed_manifest(text)?;
    if file_len != manifest.size.saturating_add(trailer_total) {
        return None;
    }
    if !verify_manifest_sig(&manifest) {
        return None;
    }
    Some(manifest)
}

/// 检查任意一个 `CELLAN.exe` 是否带有合法且自洽的官方签名尾部（`CLNUPD01`），并校验全文件 SHA-256。
pub fn inspect_signed_exe(exe_path: &Path) -> Option<SignedManifest> {
    let manifest = inspect_signed_exe_trailer(exe_path)?;
    let got_sha = sha256_prefix_of_file(exe_path, manifest.size).ok()?;
    if !got_sha.eq_ignore_ascii_case(&manifest.sha256) {
        return None;
    }
    Some(manifest)
}

static LOCAL_BUNDLE_CACHE: std::sync::OnceLock<Option<(PathBuf, SignedManifest)>> =
    std::sync::OnceLock::new();
static LOCAL_BUNDLE_WARMING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn compute_local_signed_bundle() -> Option<(PathBuf, SignedManifest)> {
    let exe = std::env::current_exe().ok()?;
    let m = inspect_signed_exe(&exe)?;
    (m.version == VERSION).then_some((exe, m))
}

/// 触发后台异步预热 `local_signed_bundle()` 缓存（立即返回，绝不阻塞调用线程）；
/// 若缓存已就绪则直接返回克隆。
pub fn try_cached_local_signed_bundle() -> Option<(PathBuf, SignedManifest)> {
    if cfg!(test) {
        return compute_local_signed_bundle();
    }
    if let Some(cached) = LOCAL_BUNDLE_CACHE.get() {
        return cached.clone();
    }
    if LOCAL_BUNDLE_WARMING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        std::thread::spawn(|| {
            let _ = LOCAL_BUNDLE_CACHE.get_or_init(compute_local_signed_bundle);
        });
    }
    None
}

/// 异步获取本机已签名安装包：若缓存已热则当场回调，否则放入后台线程校验后再回调，避免 UI 主线程同步算 60 MB SHA-256。
pub fn warm_local_signed_bundle_async(
    on_ready: impl FnOnce(Option<(PathBuf, SignedManifest)>) + Send + 'static,
) {
    if cfg!(test) {
        on_ready(compute_local_signed_bundle());
        return;
    }
    if let Some(cached) = LOCAL_BUNDLE_CACHE.get() {
        on_ready(cached.clone());
        return;
    }
    std::thread::spawn(move || {
        let val = LOCAL_BUNDLE_CACHE
            .get_or_init(compute_local_signed_bundle)
            .clone();
        on_ready(val);
    });
}

/// 若当前正在运行的 `CELLAN.exe` 是经过官方签名的正式发行版，返回 `(自身路径, 签名清单)`。
/// 本地 `cargo run` / `cargo test` 未签名开发版返回 `None`，不会对外接力广播。
pub fn local_signed_bundle() -> Option<(PathBuf, SignedManifest)> {
    if cfg!(test) {
        return compute_local_signed_bundle();
    }
    LOCAL_BUNDLE_CACHE
        .get_or_init(compute_local_signed_bundle)
        .clone()
}

/// 对下载落盘的临时文件执行严格的「公钥验签 + SHA-256 全量校验 + 尾部补全」。
/// 若调用方已在流式写盘时计算完前 `expected.size` 字节的 SHA-256，传入 `precomputed_sha256` 可彻底省去落盘后的全文件重读；
/// 若文件已带 `CLNUPD01` 尾部，仅校验尾部清单与元信息，不再重读整包做第二遍 SHA-256。
pub fn verify_and_finalize_staged_exe_with_sha(
    staged_path: &Path,
    expected: &SignedManifest,
    precomputed_sha256: Option<&str>,
) -> Result<(), String> {
    if !is_newer(&expected.version, VERSION) {
        return Err(format!(
            "更新包版本（v{}）不高于当前版本（v{}），已拒绝安装",
            expected.version, VERSION
        ));
    }
    if !verify_manifest_sig(expected) {
        return Err("更新包数字签名校验未通过，已拒绝安装".into());
    }
    let file_len = std::fs::metadata(staged_path)
        .map_err(|e| format!("无法读取已下载安装包信息：{e}"))?
        .len();
    if file_len < expected.size {
        return Err(format!(
            "下载文件不完整（预期 {} 字节，已接收 {} 字节）",
            expected.size, file_len
        ));
    }
    match precomputed_sha256 {
        Some(got_sha) => {
            if !got_sha.eq_ignore_ascii_case(&expected.sha256) {
                return Err("更新包 SHA-256 摘要不匹配（文件损坏或被篡改），已拒绝安装".into());
            }
        }
        None => {
            let got_sha = sha256_prefix_of_file(staged_path, expected.size)?;
            if !got_sha.eq_ignore_ascii_case(&expected.sha256) {
                return Err("更新包 SHA-256 摘要不匹配（文件损坏或被篡改），已拒绝安装".into());
            }
        }
    }

    if file_len == expected.size {
        let trailer = build_trailer_bytes(expected);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(staged_path)
            .map_err(|e| format!("写入签名尾部失败：{e}"))?;
        f.write_all(&trailer)
            .and_then(|_| f.flush())
            .map_err(|e| format!("刷新签名尾部失败：{e}"))?;
        return Ok(());
    }

    // 文件已带尾部：前面已验证主体前 `expected.size` 字节的 SHA-256 与 expected 一致，
    // 这里仅验证末尾 `CLNUPD01` 尾部清单、签名与总长度严格匹配，彻底避免第二遍 60 MB 哈希。
    let Some(embedded) = inspect_signed_exe_trailer(staged_path) else {
        return Err("更新包附加数据异常，已拒绝安装".into());
    };
    if embedded.version != expected.version
        || embedded.size != expected.size
        || !embedded.sha256.eq_ignore_ascii_case(&expected.sha256)
        || !embedded.sig.eq_ignore_ascii_case(&expected.sig)
    {
        return Err("更新包内嵌签名清单与通告不一致，已拒绝安装".into());
    }
    Ok(())
}

/// 对下载落盘的临时文件执行严格的「公钥验签 + SHA-256 全量校验 + 尾部补全」
#[allow(dead_code)]
pub fn verify_and_finalize_staged_exe(
    staged_path: &Path,
    expected: &SignedManifest,
) -> Result<(), String> {
    verify_and_finalize_staged_exe_with_sha(staged_path, expected, None)
}

// ---------------------------------------------------------------------------
// 轻量内网直连 HTTP 客户端（绕过企业系统代理直连内网 IP，防 502 与长阻塞）
// ---------------------------------------------------------------------------

struct ParsedHttpBase {
    host_header: String,
    connect_host: String,
    port: u16,
    prefix: String,
}

fn parse_http_base(raw: &str) -> Result<ParsedHttpBase, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("http://")
        .ok_or_else(|| format!("仅支持内网 http:// 更新源：{trimmed}"))?;
    let (authority, path_part) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err("更新地址缺少主机名或 IP".into());
    }
    let (connect_host, port) = if let Some(after_bracket) = authority.strip_prefix('[') {
        let end = after_bracket
            .find(']')
            .ok_or_else(|| "IPv6 地址格式错误".to_string())?;
        let h = &after_bracket[..end];
        let tail = &after_bracket[end + 1..];
        let p = if let Some(ps) = tail.strip_prefix(':') {
            ps.parse::<u16>().map_err(|_| "端口号无效".to_string())?
        } else {
            80
        };
        (h.to_string(), p)
    } else if let Some((h, ps)) = authority.rsplit_once(':') {
        let p = ps.parse::<u16>().map_err(|_| "端口号无效".to_string())?;
        (h.to_string(), p)
    } else {
        (authority.to_string(), 80)
    };
    if connect_host.is_empty() || port == 0 {
        return Err("更新地址主机或端口无效".into());
    }
    Ok(ParsedHttpBase {
        host_header: authority.to_string(),
        connect_host,
        port,
        prefix: path_part.trim_end_matches('/').to_string(),
    })
}

fn connect_http_direct(base: &ParsedHttpBase, timeout: Duration) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = (base.connect_host.as_str(), base.port)
        .to_socket_addrs()
        .map_err(|e| format!("解析更新源地址失败：{e}"))?
        .collect();
    if addrs.is_empty() {
        return Err("无法解析更新源地址".into());
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut last_err = None;
    for (idx, addr) in addrs.iter().enumerate() {
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            break;
        };
        if remaining.is_zero() {
            break;
        }
        let left = (addrs.len() - idx) as u32;
        let per_attempt = if left <= 1 {
            remaining
        } else {
            (remaining / left)
                .max(Duration::from_millis(500))
                .min(remaining)
        };
        match TcpStream::connect_timeout(addr, per_attempt) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(match last_err {
        Some(e) => format!("连接更新源超时或未在线：{e}"),
        None => "连接更新源超时或未在线".into(),
    })
}

const MAX_HTTP_LINE_BYTES: usize = 4096;

fn read_bounded_http_line(
    reader: &mut BufReader<TcpStream>,
    max_len: usize,
    deadline: std::time::Instant,
) -> Result<Option<String>, String> {
    let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
        return Err("读取 HTTP 响应超时".into());
    };
    if remaining.is_zero() {
        return Err("读取 HTTP 响应超时".into());
    }
    let _ = reader.get_ref().set_read_timeout(Some(remaining));
    let mut buf = Vec::new();
    let n = reader
        .by_ref()
        .take((max_len as u64).saturating_add(1))
        .read_until(b'\n', &mut buf)
        .map_err(|e| format!("读取 HTTP 数据失败：{e}"))?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > max_len {
        return Err("HTTP 行长度超出安全上限".into());
    }
    let line = String::from_utf8(buf).map_err(|_| "HTTP 头部包含非法 UTF-8".to_string())?;
    Ok(Some(line))
}

fn read_http_response_head(
    reader: &mut BufReader<TcpStream>,
    deadline: std::time::Instant,
) -> Result<(u16, Option<u64>), String> {
    let Some(status_line) = read_bounded_http_line(reader, MAX_HTTP_LINE_BYTES, deadline)? else {
        return Err("更新服务提前关闭了连接".into());
    };
    let mut parts = status_line.split_whitespace();
    let _http_ver = parts.next().unwrap_or("");
    let code: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "无效的 HTTP 状态行".to_string())?;
    let mut content_len: Option<u64> = None;
    let mut header_bytes = 0usize;
    loop {
        let Some(line) = read_bounded_http_line(reader, MAX_HTTP_LINE_BYTES, deadline)? else {
            break;
        };
        if line == "\r\n" || line == "\n" {
            break;
        }
        header_bytes += line.len();
        if header_bytes > 16 * 1024 {
            return Err("HTTP 响应头过长".into());
        }
        if let Some((name, val)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_len = val.trim().parse::<u64>().ok();
            }
        }
    }
    Ok((code, content_len))
}

fn check_http_with_pubkey(
    base_url: &str,
    pubkey_hex: Option<&str>,
) -> Result<(Option<AvailableUpdate>, String), String> {
    let parsed = parse_http_base(base_url)?;
    let mut stream = connect_http_direct(&parsed, Duration::from_millis(2_200))?;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_read_timeout(Some(Duration::from_secs(4)));
    let req_path = format!("{}/manifest.txt", parsed.prefix);
    let req = format!(
        "GET {req_path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: CELLAN/{VERSION}\r\nConnection: close\r\n\r\n",
        parsed.host_header
    );
    stream
        .write_all(req.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|e| format!("发送更新检查请求失败：{e}"))?;

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut reader = BufReader::new(stream);
    let (code, content_len) = read_http_response_head(&mut reader, deadline)?;
    if code != 200 {
        return Err(format!("更新源返回 HTTP {code}"));
    }
    if let Some(len) = content_len {
        if len > MAX_MANIFEST_BYTES as u64 {
            return Err("远端 manifest.txt 体积异常".into());
        }
    }
    let mut body = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return Err("读取 manifest.txt 超时".into());
        };
        if remaining.is_zero() {
            return Err("读取 manifest.txt 超时".into());
        }
        let _ = reader.get_ref().set_read_timeout(Some(remaining));
        let n = reader
            .read(&mut chunk)
            .map_err(|e| format!("读取 manifest.txt 失败：{e}"))?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
        if body.len() > MAX_MANIFEST_BYTES {
            return Err("远端 manifest.txt 超出长度上限".into());
        }
    }
    let text = String::from_utf8(body).map_err(|_| "manifest.txt 不是合法 UTF-8".to_string())?;
    let manifest =
        parse_signed_manifest(&text).ok_or_else(|| "远端 manifest.txt 缺少签名或格式无效".to_string())?;
    let sig_ok = pubkey_hex.is_some_and(|pk| verify_manifest_sig_with_pubkey(&manifest, pk));
    if !sig_ok {
        return Err("远端 manifest.txt 数字签名不匹配，已忽略".into());
    }
    let clean_url = base_url.trim().trim_end_matches('/').to_string();
    if !is_newer(&manifest.version, VERSION) {
        return Ok((None, clean_url));
    }
    Ok((
        Some(AvailableUpdate {
            manifest,
            source: UpdateSource::Http {
                base_url: clean_url.clone(),
            },
        }),
        clean_url,
    ))
}

/// 检查单个内网 HTTP 更新源是否有经过合法签名的新版本
pub fn check_http(base_url: &str) -> Result<Option<AvailableUpdate>, String> {
    let pk = active_pubkey_hex();
    let (avail, clean_url) = check_http_with_pubkey(base_url, pk.as_deref())?;
    remember_http_url(&clean_url);
    Ok(avail)
}

fn check_shared_dir_with_pubkey(
    dir: &Path,
    pubkey_hex: Option<&str>,
) -> Result<Option<AvailableUpdate>, String> {
    let text = std::fs::read_to_string(dir.join("manifest.txt"))
        .map_err(|e| format!("读取共享目录 manifest.txt 失败：{e}"))?;
    if let Some(signed) = parse_signed_manifest(&text) {
        let sig_ok = pubkey_hex.is_some_and(|pk| verify_manifest_sig_with_pubkey(&signed, pk));
        if !sig_ok {
            return Err("共享盘更新包签名校验失败，已拒绝".into());
        }
        if !is_newer(&signed.version, VERSION) {
            return Ok(None);
        }
        let exe_path = dir.join(&signed.exe);
        if !exe_path.is_file() {
            return Err(format!("共享目录中缺少 {}", exe_path.display()));
        }
        return Ok(Some(AvailableUpdate {
            manifest: signed,
            source: UpdateSource::SharedDir {
                dir: dir.to_path_buf(),
            },
        }));
    }
    match check(dir) {
        Ok(Some(remote)) => Ok(Some(AvailableUpdate {
            manifest: SignedManifest {
                version: remote.version,
                exe: "CELLAN.exe".into(),
                size: 0,
                sha256: String::new(),
                sig: String::new(),
                notes: String::new(),
            },
            source: UpdateSource::LegacyNoticeOnly {
                exe_path: remote.exe,
            },
        })),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// 检查共享目录（若包含合法签名清单则支持一键自动更新；若为旧式无签名清单则仅提示）
pub fn check_shared_dir(dir: &Path) -> Result<Option<AvailableUpdate>, String> {
    let pk = active_pubkey_hex();
    check_shared_dir_with_pubkey(dir, pk.as_deref())
}

fn is_unc_network_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with(r"\\") || s.starts_with("//")
}

/// 并行探测所有已配置 HTTP 更新源，回退检查共享目录（UNC 共享带 3 秒超时保护，防离线卡死）
pub fn probe_all_sources() -> Result<Option<AvailableUpdate>, String> {
    let urls = candidate_http_urls();
    let raw_shared = dir();
    if urls.is_empty() && raw_shared.is_none() {
        return Err("尚未配置更新源（将在与新版同事协作时自动同步）".into());
    }
    let pk = active_pubkey_hex();
    let mut last_err: Option<String> = None;
    let mut any_reachable = false;

    if urls.len() == 1 {
        match check_http_with_pubkey(&urls[0], pk.as_deref()) {
            Ok((Some(avail), clean_url)) => {
                remember_http_url(&clean_url);
                return Ok(Some(avail));
            }
            Ok((None, clean_url)) => {
                remember_http_url(&clean_url);
                any_reachable = true;
            }
            Err(e) => last_err = Some(e),
        }
    } else if urls.len() > 1 {
        let (tx, rx) = std::sync::mpsc::channel();
        let total = urls.len();
        for (idx, u) in urls.into_iter().enumerate() {
            let tx_i = tx.clone();
            let pk_i = pk.clone();
            std::thread::spawn(move || {
                let res = check_http_with_pubkey(&u, pk_i.as_deref());
                let _ = tx_i.send((idx, res));
            });
        }
        drop(tx);
        for _ in 0..total {
            let Ok((_idx, res)) = rx.recv() else { break };
            match res {
                Ok((Some(avail), clean_url)) => {
                    remember_http_url(&clean_url);
                    return Ok(Some(avail));
                }
                Ok((None, clean_url)) => {
                    remember_http_url(&clean_url);
                    any_reachable = true;
                }
                Err(e) => last_err = Some(e),
            }
        }
    }

    if let Some(d) = raw_shared {
        if is_unc_network_path(&d) {
            let (tx, rx) = std::sync::mpsc::channel();
            let pk_s = pk.clone();
            std::thread::spawn(move || {
                let res = if d.is_dir() {
                    Some(check_shared_dir_with_pubkey(&d, pk_s.as_deref()))
                } else {
                    None
                };
                let _ = tx.send(res);
            });
            match rx.recv_timeout(Duration::from_secs(3)) {
                Ok(Some(Ok(Some(avail)))) => return Ok(Some(avail)),
                Ok(Some(Ok(None))) => any_reachable = true,
                Ok(Some(Err(e))) => last_err = Some(e),
                Ok(None) => {}
                Err(_) => {
                    if last_err.is_none() {
                        last_err = Some("共享盘更新目录响应超时".into());
                    }
                }
            }
        } else if d.is_dir() {
            match check_shared_dir_with_pubkey(&d, pk.as_deref()) {
                Ok(Some(avail)) => return Ok(Some(avail)),
                Ok(None) => any_reachable = true,
                Err(e) => last_err = Some(e),
            }
        }
    }

    if any_reachable {
        Ok(None)
    } else {
        Err(last_err.unwrap_or_else(|| "更新服务暂未在线".into()))
    }
}

/// 准备一个位于 `%LOCALAPPDATA%\CELLAN\updates` 下的临时下载路径
pub fn staging_file_path(version: &str, suffix: &str) -> std::io::Result<PathBuf> {
    let dir = app_dir().join("updates");
    std::fs::create_dir_all(&dir)?;
    let safe_ver: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.')
        .collect();
    Ok(dir.join(format!(
        "CELLAN-{safe_ver}-{}.{suffix}",
        std::process::id()
    )))
}

/// 从内网 HTTP 源或签名共享目录流式拉取新版，完成验签与完整性核验后返回已就绪文件路径
pub fn download_and_verify(
    avail: &AvailableUpdate,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    if !is_newer(&avail.manifest.version, VERSION) {
        return Err(format!(
            "更新包版本（v{}）不高于当前版本（v{}），已拒绝安装",
            avail.manifest.version, VERSION
        ));
    }
    if !verify_manifest_sig(&avail.manifest) {
        return Err("更新清单缺少有效官方签名，无法自动安装".into());
    }
    let part_path =
        staging_file_path(&avail.manifest.version, "part").map_err(|e| format!("创建缓存目录失败：{e}"))?;
    let verified_path = staging_file_path(&avail.manifest.version, "verified.exe")
        .map_err(|e| format!("创建验证文件路径失败：{e}"))?;
    let _ = std::fs::remove_file(&part_path);
    let _ = std::fs::remove_file(&verified_path);

    let res: Result<Option<String>, String> = match &avail.source {
        UpdateSource::Http { base_url } => {
            download_from_http(base_url, &avail.manifest, &part_path, &mut on_progress)
        }
        UpdateSource::SharedDir { dir } => {
            copy_from_shared_dir(&dir.join(&avail.manifest.exe), &avail.manifest, &part_path, &mut on_progress)
        }
        UpdateSource::CollabPeer { .. } => {
            Err("协作节点更新通过实时协作连接直接传输".into())
        }
        UpdateSource::LegacyNoticeOnly { .. } => {
            Err("未签名的共享目录包不支持自动安装".into())
        }
    };
    let streamed_sha = match res {
        Ok(sha) => sha,
        Err(e) => {
            let _ = std::fs::remove_file(&part_path);
            return Err(e);
        }
    };
    if let Err(e) =
        verify_and_finalize_staged_exe_with_sha(&part_path, &avail.manifest, streamed_sha.as_deref())
    {
        let _ = std::fs::remove_file(&part_path);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&part_path, &verified_path) {
        let _ = std::fs::remove_file(&part_path);
        return Err(format!("归档已验证安装包失败：{e}"));
    }
    Ok(verified_path)
}

fn download_from_http(
    base_url: &str,
    manifest: &SignedManifest,
    dest: &Path,
    on_progress: &mut impl FnMut(u64, u64),
) -> Result<Option<String>, String> {
    let parsed = parse_http_base(base_url)?;
    let mut stream = connect_http_direct(&parsed, Duration::from_secs(4))?;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    if !is_safe_exe_filename(&manifest.exe) {
        return Err("非法可执行文件名".into());
    }
    let req_path = format!("{}/{}", parsed.prefix, manifest.exe);
    let req = format!(
        "GET {req_path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: CELLAN/{VERSION}\r\nConnection: close\r\n\r\n",
        parsed.host_header
    );
    stream
        .write_all(req.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|e| format!("请求下载更新包失败：{e}"))?;

    let head_deadline = std::time::Instant::now() + Duration::from_secs(15);
    let total_deadline = std::time::Instant::now() + Duration::from_secs(300);
    let mut reader = BufReader::new(stream);
    let (code, content_len) = read_http_response_head(&mut reader, head_deadline)?;
    if code != 200 {
        return Err(format!("下载更新包失败（HTTP {code}）"));
    }
    let total = content_len.unwrap_or(manifest.size).max(manifest.size);
    if total > MAX_EXE_BYTES + (MAX_MANIFEST_BYTES as u64) + 64 {
        return Err("远端更新包大小超出安全上限".into());
    }
    let mut out = std::fs::File::create(dest).map_err(|e| format!("创建临时安装包失败：{e}"))?;
    let mut buf = vec![0u8; IO_BUF_BYTES];
    let mut prefix_hash = PrefixSha256::new(manifest.size);
    let mut received = 0u64;
    on_progress(0, total);
    loop {
        let Some(remaining) = total_deadline.checked_duration_since(std::time::Instant::now()) else {
            return Err("下载更新包总耗时超时".into());
        };
        if remaining.is_zero() {
            return Err("下载更新包总耗时超时".into());
        }
        let _ = reader
            .get_ref()
            .set_read_timeout(Some(remaining.min(Duration::from_secs(30))));
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("下载中断：{e}"))?;
        if n == 0 {
            break;
        }
        received += n as u64;
        if received > MAX_EXE_BYTES + (MAX_MANIFEST_BYTES as u64) + 64 {
            return Err("下载数据超过大小上限".into());
        }
        prefix_hash.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| format!("写入临时安装包失败：{e}"))?;
        on_progress(received.min(total), total);
    }
    out.flush()
        .map_err(|e| format!("刷新临时安装包失败：{e}"))?;
    let _ = out.sync_all();
    Ok(prefix_hash.finalize_hex())
}

fn copy_from_shared_dir(
    src_exe: &Path,
    manifest: &SignedManifest,
    dest: &Path,
    on_progress: &mut impl FnMut(u64, u64),
) -> Result<Option<String>, String> {
    let mut src = std::fs::File::open(src_exe).map_err(|e| format!("打开共享盘更新包失败：{e}"))?;
    let total = src
        .metadata()
        .map(|m| m.len())
        .unwrap_or(manifest.size)
        .max(manifest.size);
    if total > MAX_EXE_BYTES + (MAX_MANIFEST_BYTES as u64) + 64 {
        return Err("共享盘更新包大小超出安全上限".into());
    }
    let mut out = std::fs::File::create(dest).map_err(|e| format!("创建本地缓存文件失败：{e}"))?;
    let mut buf = vec![0u8; IO_BUF_BYTES];
    let mut prefix_hash = PrefixSha256::new(manifest.size);
    let mut received = 0u64;
    on_progress(0, total);
    loop {
        let n = src
            .read(&mut buf)
            .map_err(|e| format!("从共享盘读取失败：{e}"))?;
        if n == 0 {
            break;
        }
        received += n as u64;
        prefix_hash.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| format!("写入本地缓存失败：{e}"))?;
        on_progress(received.min(total), total);
    }
    out.flush()
        .map_err(|e| format!("刷新本地缓存失败：{e}"))?;
    Ok(prefix_hash.finalize_hex())
}

// ---------------------------------------------------------------------------
// Windows 原地原子替换与无缝重启清理
// ---------------------------------------------------------------------------

/// 将已验签通过的 `verified_exe` 原地原子替换到 `target_exe`：
/// 1. 同卷优先原子 `rename` 到与 `target_exe` 同目录的临时文件（零 60 MB 磁盘拷贝），跨卷或重命名失败时回退 `copy`；
/// 2. 将正在运行的 `target_exe` 重命名为 `.old`（Windows 允许对运行中进程执行同目录 rename）；
/// 3. 将同目录新文件原子重命名为 `target_exe`，失败则立即回滚 `.old`。
pub fn install_to_target(verified_exe: &Path, target_exe: &Path) -> Result<(), String> {
    let parent = target_exe
        .parent()
        .ok_or_else(|| "无法定位当前程序所在目录".to_string())?;
    let file_name = target_exe
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "CELLAN.exe".into());
    let pid = std::process::id();
    let staged_same_vol = parent.join(format!(".{file_name}.{pid}.new"));
    let _ = std::fs::remove_file(&staged_same_vol);

    let moved_in_place = match std::fs::rename(verified_exe, &staged_same_vol) {
        Ok(()) => true,
        Err(_) => {
            std::fs::copy(verified_exe, &staged_same_vol).map_err(|e| {
                format!(
                    "当前程序所在目录不可写（{}），请将 CELLAN.exe 放在桌面或个人目录运行：{e}",
                    parent.display()
                )
            })?;
            false
        }
    };
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&staged_same_vol) {
        let _ = f.sync_all();
    }

    let primary_old = parent.join(format!("{file_name}.old"));
    let backup_old = if !primary_old.exists() || std::fs::remove_file(&primary_old).is_ok() {
        primary_old
    } else {
        parent.join(format!("{file_name}.{pid}.old"))
    };

    if target_exe.exists() {
        if let Err(e) = std::fs::rename(target_exe, &backup_old) {
            if !moved_in_place || std::fs::rename(&staged_same_vol, verified_exe).is_err() {
                let _ = std::fs::remove_file(&staged_same_vol);
            }
            return Err(format!("重命名当前运行程序失败：{e}"));
        }
    }

    if let Err(e) = std::fs::rename(&staged_same_vol, target_exe) {
        let rolled_back = std::fs::rename(&backup_old, target_exe).is_ok();
        if !moved_in_place || std::fs::rename(&staged_same_vol, verified_exe).is_err() {
            let _ = std::fs::remove_file(&staged_same_vol);
        }
        return Err(if rolled_back {
            format!("替换新版可执行文件失败（已回滚原版本）：{e}")
        } else {
            format!(
                "替换新版可执行文件失败且自动回滚失败（原备份位于 {}）：{e}",
                backup_old.display()
            )
        });
    }

    if !moved_in_place {
        let _ = std::fs::remove_file(verified_exe);
    }
    Ok(())
}

/// 将已验签的更新包原地替换当前正在运行的 `CELLAN.exe`
pub fn install_in_place(verified_exe: &Path) -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| format!("无法获取当前程序路径：{e}"))?;
    install_to_target(verified_exe, &current)?;
    Ok(current)
}

/// 拉起替换后的新版 `CELLAN.exe`（传入 `--after-update=<当前PID>` 以便新进程等待旧进程释放单实例端口并清理 `.old`）
pub fn spawn_restarted_process(reopen_file: Option<&Path>) -> Result<(), String> {
    let current = std::env::current_exe().map_err(|e| format!("无法获取程序路径：{e}"))?;
    let mut cmd = std::process::Command::new(&current);
    cmd.arg(format!("--after-update={}", std::process::id()));
    if let Some(f) = reopen_file {
        if f.is_file() {
            cmd.arg(f);
        }
    }
    cmd.spawn()
        .map_err(|e| format!("启动新版本进程失败：{e}"))?;
    Ok(())
}

/// 清理上一次原地自更新遗留的 `CELLAN.exe.old` / `.CELLAN.exe.*.new` 与过期的 `updates/` 临时文件
pub fn cleanup_old_exe() {
    let my_pid = std::process::id();
    if let Ok(current) = std::env::current_exe()
        && let Some(parent) = current.parent()
        && let Some(name) = current.file_name().map(|n| n.to_string_lossy().to_string())
    {
        let old_prefix = format!("{name}.");
        let new_prefix = format!(".{name}.");
        let own_new = format!(".{name}.{my_pid}.new");
        if let Ok(rd) = std::fs::read_dir(parent) {
            for entry in rd.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                let is_old = fname == format!("{name}.old")
                    || (fname.starts_with(&old_prefix) && fname.ends_with(".old"));
                let is_stale_new = fname != own_new
                    && fname.starts_with(&new_prefix)
                    && fname.ends_with(".new");
                if is_old || is_stale_new {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
    cleanup_stale_update_cache(my_pid);
}

fn cleanup_stale_update_cache(my_pid: u32) {
    let dir = app_dir().join("updates");
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let own_tag = format!("-{my_pid}.");
    let cutoff = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(600))
        .unwrap_or(std::time::UNIX_EPOCH);
    for entry in rd.flatten() {
        let fname = entry.file_name().to_string_lossy().to_string();
        if !fname.starts_with("CELLAN-")
            || (!fname.ends_with(".part") && !fname.ends_with(".verified.exe"))
            || fname.contains(&own_tag)
        {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 新进程收到 `--after-update=<old_pid>` 时调用：等待旧进程退出释放单实例端口，随后立刻清理 `.old`
pub fn wait_for_old_process_and_cleanup(old_pid: Option<u32>) {
    #[cfg(windows)]
    if let Some(pid) = old_pid {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(
                dw_desired_access: u32,
                b_inherit_handle: i32,
                dw_process_id: u32,
            ) -> *mut core::ffi::c_void;
            fn WaitForSingleObject(h_handle: *mut core::ffi::c_void, dw_milliseconds: u32) -> u32;
            fn CloseHandle(h_object: *mut core::ffi::c_void) -> i32;
        }
        const SYNCHRONIZE: u32 = 0x0010_0000;
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if !h.is_null() {
                let _ = WaitForSingleObject(h, 5_000);
                let _ = CloseHandle(h);
            }
        }
    }
    #[cfg(not(windows))]
    if old_pid.is_some() {
        std::thread::sleep(Duration::from_millis(250));
    }
    for _ in 0..8 {
        cleanup_old_exe();
        let Ok(current) = std::env::current_exe() else {
            break;
        };
        let old = current
            .parent()
            .and_then(|p| {
                current
                    .file_name()
                    .map(|n| p.join(format!("{}.old", n.to_string_lossy())))
            });
        if old.as_ref().is_none_or(|p| !p.exists()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

// ---------------------------------------------------------------------------
// 内置静默 HTTP 静态发版服务（`CELLAN.exe --serve-updates <dir> [--port=48768]`）
// 只读暴露 `manifest.txt` 与其指向的 `.exe`，零控制台弹窗、每次请求按需读盘无需重启
// ---------------------------------------------------------------------------

const SERVER_MAX_CONNS: usize = 16;
const SERVER_MAX_CONNS_PER_IP: usize = 4;

struct ConnPermit {
    active: Arc<AtomicUsize>,
    per_ip: Arc<std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, usize>>>,
    ip: std::net::IpAddr,
}

impl Drop for ConnPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
        let mut map = self.per_ip.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cnt) = map.get_mut(&self.ip) {
            if *cnt <= 1 {
                map.remove(&self.ip);
            } else {
                *cnt -= 1;
            }
        }
    }
}

fn is_update_server_alive(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(400)) else {
        return false;
    };
    let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    if s.write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut resp = String::new();
    let _ = s.read_to_string(&mut resp);
    resp.contains("CELLAN-UPDATE-OK")
}

pub fn serve_updates_blocking(dir: &Path, port: u16) -> std::io::Result<()> {
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && is_update_server_alive(port) => {
            // 本机后台发版服务已在运行，新发布的 manifest.txt / CELLAN.exe 直接热生效
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    serve_updates_on_listener(listener, dir.to_path_buf(), None)
}

#[derive(Default)]
struct ServerManifestCache {
    raw_bytes: Vec<u8>,
    allowed_exe: Option<String>,
}

pub fn serve_updates_on_listener(
    listener: TcpListener,
    dir: PathBuf,
    stop_flag: Option<Arc< std::sync::atomic::AtomicBool >>,
) -> std::io::Result<()> {
    if stop_flag.is_some() {
        listener.set_nonblocking(true)?;
    }
    let active = Arc::new(AtomicUsize::new(0));
    let per_ip = Arc::new(std::sync::Mutex::new(
        std::collections::HashMap::<std::net::IpAddr, usize>::new(),
    ));
    let manifest_cache = Arc::new(std::sync::Mutex::new(ServerManifestCache::default()));
    loop {
        if stop_flag
            .as_ref()
            .is_some_and(|f| !f.load(Ordering::Acquire))
        {
            break;
        }
        let (stream, peer) = match listener.accept() {
            Ok(pair) => pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        if active.load(Ordering::Relaxed) >= SERVER_MAX_CONNS {
            drop(stream);
            continue;
        }
        let ip = peer.ip();
        let ip_limit = if ip.is_loopback() {
            SERVER_MAX_CONNS
        } else {
            SERVER_MAX_CONNS_PER_IP
        };
        {
            let mut map = per_ip.lock().unwrap_or_else(|e| e.into_inner());
            let cnt = map.entry(ip).or_insert(0);
            if *cnt >= ip_limit {
                drop(stream);
                continue;
            }
            *cnt += 1;
        }
        active.fetch_add(1, Ordering::Relaxed);
        let permit = ConnPermit {
            active: active.clone(),
            per_ip: per_ip.clone(),
            ip,
        };
        let dir_clone = dir.clone();
        let cache_clone = manifest_cache.clone();
        std::thread::spawn(move || {
            let _permit = permit;
            let _ = stream.set_nonblocking(false);
            let _ = handle_update_http_conn(stream, &dir_clone, &cache_clone);
        });
    }
    Ok(())
}

fn resolve_allowed_exe_cached(
    dir: &Path,
    cache: &std::sync::Mutex<ServerManifestCache>,
) -> Option<String> {
    let bytes = std::fs::read(dir.join("manifest.txt")).ok()?;
    if let Ok(guard) = cache.lock()
        && guard.raw_bytes == bytes
    {
        return guard.allowed_exe.clone();
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let allowed = parse_signed_manifest(text)
        .map(|m| m.exe)
        .or_else(|| parse_manifest(text).map(|(_, e, _)| e));
    if let Ok(mut guard) = cache.lock() {
        guard.raw_bytes = bytes;
        guard.allowed_exe = allowed.clone();
    }
    allowed
}

fn handle_update_http_conn(
    mut stream: TcpStream,
    dir: &Path,
    cache: &std::sync::Mutex<ServerManifestCache>,
) -> std::io::Result<()> {
    let head_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let transfer_deadline = std::time::Instant::now() + Duration::from_secs(120);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let mut reader = BufReader::new(stream.try_clone()?);
    let Ok(Some(req_line)) = read_bounded_http_line(&mut reader, MAX_HTTP_LINE_BYTES, head_deadline)
    else {
        return Ok(());
    };
    // 消费剩余请求头（受总字节数、单行上限与总截止时间三重约束）
    let mut hdr_bytes = 0usize;
    loop {
        let Ok(Some(line)) = read_bounded_http_line(&mut reader, MAX_HTTP_LINE_BYTES, head_deadline)
        else {
            break;
        };
        if line == "\r\n" || line == "\n" {
            break;
        }
        hdr_bytes += line.len();
        if hdr_bytes > 16 * 1024 {
            return Ok(());
        }
    }

    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("");
    let path_only = raw_path.split('?').next().unwrap_or("");

    if method != "GET" && method != "HEAD" {
        return write_simple_http(&mut stream, 405, "Method Not Allowed", b"method not allowed");
    }

    if path_only == "/healthz" {
        let body = b"CELLAN-UPDATE-OK\n";
        return write_http_bytes(&mut stream, 200, "OK", "text/plain; charset=utf-8", body, method == "HEAD");
    }

    if path_only == "/manifest.txt" {
        let Ok(bytes) = std::fs::read(dir.join("manifest.txt")) else {
            return write_simple_http(&mut stream, 404, "Not Found", b"manifest not found");
        };
        return write_http_bytes(
            &mut stream,
            200,
            "OK",
            "text/plain; charset=utf-8",
            &bytes,
            method == "HEAD",
        );
    }

    // 仅允许下载当前 manifest.txt 中显式指明的那个 .exe 文件，彻底杜绝任意文件读取
    let requested_name = path_only.trim_start_matches('/');
    if !is_safe_exe_filename(requested_name) {
        return write_simple_http(&mut stream, 404, "Not Found", b"not found");
    }
    let Some(allowed_exe) = resolve_allowed_exe_cached(dir, cache) else {
        return write_simple_http(&mut stream, 404, "Not Found", b"manifest missing");
    };
    if !allowed_exe.eq_ignore_ascii_case(requested_name) {
        return write_simple_http(&mut stream, 404, "Not Found", b"not found");
    }

    let exe_path = dir.join(requested_name);
    let Ok(mut f) = std::fs::File::open(&exe_path) else {
        return write_simple_http(&mut stream, 404, "Not Found", b"exe missing");
    };
    let len = f.metadata()?.len();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {len}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(header.as_bytes())?;
    if method != "HEAD" {
        let mut buf = vec![0u8; IO_BUF_BYTES];
        loop {
            let Some(remaining) = transfer_deadline.checked_duration_since(std::time::Instant::now())
            else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "update response transfer deadline exceeded",
                ));
            };
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "update response transfer deadline exceeded",
                ));
            }
            let _ = stream.set_write_timeout(Some(remaining.min(Duration::from_secs(30))));
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            stream.write_all(&buf[..n])?;
        }
    }
    stream.flush()
}

fn write_http_bytes(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn write_simple_http(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write_http_bytes(stream, code, reason, "text/plain; charset=utf-8", body, false)
}

#[cfg(all(test, windows))]
pub(crate) fn make_test_signed_release(
    kp: &cng::TestKeyPair,
    version: &str,
    raw_exe: &[u8],
    notes: &str,
) -> (SignedManifest, Vec<u8>) {
    let mut h = Sha256::new();
    h.update(raw_exe);
    let sha256 = hex::encode(h.finalize());
    let msg = canonical_signing_bytes(version, raw_exe.len() as u64, &sha256, notes);
    let mut mh = Sha256::new();
    mh.update(&msg);
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&mh.finalize());
    let sig = kp.sign_digest(&digest);
    let manifest = SignedManifest {
        version: version.to_string(),
        exe: "CELLAN.exe".to_string(),
        size: raw_exe.len() as u64,
        sha256,
        sig,
        notes: sanitize_notes(notes),
    };
    let mut full = raw_exe.to_vec();
    full.extend_from_slice(&build_trailer_bytes(&manifest));
    (manifest, full)
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
        assert_eq!(
            (v.as_str(), e.as_str(), s.as_str()),
            ("1.38", "a.exe", "ab".repeat(32).as_str())
        );
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

    #[cfg(windows)]
    #[test]
    fn signed_http_update_downloads_verifies_replaces_and_rejects_tampering() {
        let env = crate::test_env::TestEnv::new("signed-http-update");
        let kp = cng::TestKeyPair::generate();
        let attacker_kp = cng::TestKeyPair::generate();
        env.sc("SC_UPDATE_PUBKEY", &kp.pubkey_hex);

        let raw_payload = b"MZ-fake-cellan-binary-payload-for-update-verification-test";
        let (manifest, full_exe) =
            make_test_signed_release(&kp, "99.1", raw_payload, "新增内网一键无感更新");

        let serve_dir = env.path("publish-server");
        std::fs::create_dir_all(&serve_dir).unwrap();
        std::fs::write(serve_dir.join("manifest.txt"), manifest.to_manifest_text()).unwrap();
        std::fs::write(serve_dir.join("CELLAN.exe"), &full_exe).unwrap();
        std::fs::write(serve_dir.join("secret.txt"), b"should-not-be-served").unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running_bg = running.clone();
        let dir_bg = serve_dir.clone();
        let handle = std::thread::spawn(move || {
            let _ = serve_updates_on_listener(listener, dir_bg, Some(running_bg));
        });

        let url = format!("http://127.0.0.1:{port}");
        env.sc("SC_UPDATE_URL", &url);

        // 1. 探测发现合法新版本
        let avail = probe_all_sources().unwrap().expect("应发现签名新版本");
        assert_eq!(avail.manifest.version, "99.1");
        assert_eq!(avail.manifest.notes, "新增内网一键无感更新");

        // 2. 流式拉取并验签
        let mut saw_progress = false;
        let verified_path = download_and_verify(&avail, |done, total| {
            if done == total && total > 0 {
                saw_progress = true;
            }
        })
        .expect("下载与验签应成功");
        assert!(saw_progress);
        assert!(
            inspect_signed_exe(&verified_path).is_some(),
            "验证后的安装包必须保留合法 CLNUPD01 签名尾部以支持接力供种"
        );

        // 3. 原地原子替换目标 exe，旧版重命名为 .old
        let target_exe = env.path("desktop").join("CELLAN.exe");
        std::fs::create_dir_all(target_exe.parent().unwrap()).unwrap();
        std::fs::write(&target_exe, b"old-binary").unwrap();
        install_to_target(&verified_path, &target_exe).expect("原地替换应成功");
        assert_eq!(std::fs::read(&target_exe).unwrap(), full_exe);
        assert_eq!(
            std::fs::read(env.path("desktop").join("CELLAN.exe.old")).unwrap(),
            b"old-binary"
        );

        // 4. 攻击者用非官方私钥重签替换服务器上的包 → 客户端必须拒绝
        let (evil_manifest, evil_exe) =
            make_test_signed_release(&attacker_kp, "99.9", b"malware", "伪造更新");
        std::fs::write(
            serve_dir.join("manifest.txt"),
            evil_manifest.to_manifest_text(),
        )
        .unwrap();
        std::fs::write(serve_dir.join("CELLAN.exe"), &evil_exe).unwrap();
        assert!(
            check_http(&url).is_err(),
            "非官方私钥签名的 manifest 必须被拒绝"
        );

        // 5. 合法 manifest 但二进制内容被篡改 1 字节 → 下载阶段 SHA-256 校验必须拦截
        std::fs::write(serve_dir.join("manifest.txt"), manifest.to_manifest_text()).unwrap();
        let mut tampered = full_exe.clone();
        tampered[0] ^= 0xFF;
        std::fs::write(serve_dir.join("CELLAN.exe"), &tampered).unwrap();
        let avail2 = check_http(&url).unwrap().unwrap();
        assert!(
            download_and_verify(&avail2, |_, _| {}).is_err(),
            "篡改 1 字节的 exe 必须在落盘前被拒绝"
        );

        running.store(false, Ordering::Release);
        let _ = handle.join();
    }

    #[cfg(windows)]
    #[test]
    fn powershell_ecdsa_signing_interoperates_with_rust_verifier() {
        let env = crate::test_env::TestEnv::new("ps-ecdsa-interop");
        let exe_path = env.path("CELLAN.exe");
        let pub_path = env.path("pub.txt");
        std::fs::write(&exe_path, b"MZ-powershell-signed-binary-test-payload").unwrap();

        let ps_script = format!(
            "$ErrorActionPreference='Stop'; \
             $utf8 = New-Object System.Text.UTF8Encoding($false); \
             $ec = New-Object System.Security.Cryptography.ECDsaCng(256); \
             $ec.HashAlgorithm = [System.Security.Cryptography.CngAlgorithm]::Sha256; \
             $pub = $ec.Key.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob); \
             $pubHex = ([BitConverter]::ToString($pub, 8, 64).Replace('-', '')).ToLowerInvariant(); \
             [System.IO.File]::WriteAllText('{}', $pubHex, $utf8); \
             $raw = [System.IO.File]::ReadAllBytes('{}'); \
             $size = [uint64]$raw.LongLength; \
             $sha = ([BitConverter]::ToString([System.Security.Cryptography.SHA256]::Create().ComputeHash($raw)).Replace('-', '')).ToLowerInvariant(); \
             $canon = \"CELLAN-RELEASE-V1`nversion=99.5`nsize=$size`nsha256=$sha`nnotes=PSInterop`n\"; \
             $sig = ([BitConverter]::ToString($ec.SignData($utf8.GetBytes($canon))).Replace('-', '')).ToLowerInvariant(); \
             $man = \"version=99.5`nexe=CELLAN.exe`nsize=$size`nsha256=$sha`nsig=$sig`nnotes=PSInterop`n\"; \
             $manBytes = $utf8.GetBytes($man); \
             $lenBytes = [BitConverter]::GetBytes([uint32]$manBytes.Length); \
             $magic = [System.Text.Encoding]::ASCII.GetBytes('CLNUPD01'); \
             $fs = [System.IO.File]::Open('{}', [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write); \
             $fs.Write($manBytes, 0, $manBytes.Length); \
             $fs.Write($lenBytes, 0, 4); \
             $fs.Write($magic, 0, 8); \
             $fs.Dispose()",
            pub_path.display().to_string().replace('\'', "''"),
            exe_path.display().to_string().replace('\'', "''"),
            exe_path.display().to_string().replace('\'', "''"),
        );

        let status = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-InputFormat",
                "None",
                "-Command",
                &ps_script,
            ])
            .stdin(std::process::Stdio::null())
            .status()
            .expect("应能调用 powershell.exe");
        assert!(status.success(), "PowerShell 签名脚本应成功退出");

        let pub_hex = std::fs::read_to_string(&pub_path).unwrap();
        env.sc("SC_UPDATE_PUBKEY", pub_hex.trim());
        let verified = inspect_signed_exe(&exe_path)
            .expect("PowerShell ECDsaCng 签出的 CLNUPD01 安装包必须通过 Rust 端验签");
        assert_eq!(verified.version, "99.5");
        assert_eq!(verified.notes, "PSInterop");
    }

    #[cfg(windows)]
    #[test]
    fn streaming_prefix_sha_verifies_signed_and_raw_staged_exe_and_rejects_bad_trailer() {
        let env = crate::test_env::TestEnv::new("update-streaming-sha");
        let kp = cng::TestKeyPair::generate();
        env.sc("SC_UPDATE_PUBKEY", &kp.pubkey_hex);

        let raw = b"MZ-streaming-prefix-sha256-payload-data-0123456789";
        let (manifest, full_exe) = make_test_signed_release(&kp, "99.7", raw, "流式单遍校验");

        // 1. Streaming hash across chunk boundaries matches manifest.sha256
        let mut h = PrefixSha256::new(manifest.size);
        for chunk in full_exe.chunks(11) {
            h.update(chunk);
        }
        let sha = h.finalize_hex().expect("complete prefix");
        assert_eq!(sha, manifest.sha256);

        // 2. Signed exe with CLNUPD01 trailer passes trailer-only check + precomputed sha
        let signed_path = env.path("signed.part");
        std::fs::write(&signed_path, &full_exe).unwrap();
        assert_eq!(inspect_signed_exe_trailer(&signed_path), Some(manifest.clone()));
        verify_and_finalize_staged_exe_with_sha(&signed_path, &manifest, Some(&sha)).unwrap();

        // 3. Raw exe without trailer gets trailer appended cleanly
        let raw_path = env.path("raw.part");
        std::fs::write(&raw_path, raw).unwrap();
        verify_and_finalize_staged_exe_with_sha(&raw_path, &manifest, Some(&sha)).unwrap();
        assert_eq!(std::fs::read(&raw_path).unwrap(), full_exe);

        // 4. Corrupted trailer is rejected even when precomputed prefix sha matches
        let mut bad_trailer = full_exe.clone();
        let last = bad_trailer.len() - 1;
        bad_trailer[last] ^= 0xFF;
        let bad_path = env.path("bad_trailer.part");
        std::fs::write(&bad_path, &bad_trailer).unwrap();
        assert!(
            verify_and_finalize_staged_exe_with_sha(&bad_path, &manifest, Some(&sha)).is_err()
        );
    }

    #[test]
    fn version_overflow_is_rejected_and_downgrade_manifest_blocked() {
        assert!(!is_valid_version_str("99999999999999999999999999.1"));
        assert!(!is_newer("99999999999999999999999999.1", "1.82"));
        assert!(!is_newer("1.0", VERSION));
        assert!(!is_newer(VERSION, VERSION));

        #[cfg(windows)]
        {
            let env = crate::test_env::TestEnv::new("update-downgrade-block");
            let kp = cng::TestKeyPair::generate();
            env.sc("SC_UPDATE_PUBKEY", &kp.pubkey_hex);
            let raw = b"MZ-old-version-payload";
            let (old_manifest, full_exe) = make_test_signed_release(&kp, "1.0", raw, "old");
            let staged = env.path("old.part");
            std::fs::write(&staged, &full_exe).unwrap();
            assert!(
                verify_and_finalize_staged_exe_with_sha(&staged, &old_manifest, None).is_err(),
                "downgrade signed manifest must be rejected by verifier"
            );
        }
    }
}
