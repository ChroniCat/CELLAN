//! 不可篡改审计日志核心引擎（Tamper-Evident Audit Log）
//!
//! 设计目标（去中心化、单文件内嵌、有界体积、防抵赖与篡改必露馅）：
//! 1. **内嵌于 `.xlsx` 标准 OOXML Custom XML 部件**：`customXml/cellanAudit.xml`（兼容 Excel/WPS，
//!    即使被 Excel 重命名为 `customXml/itemN.xml` 也能按命名空间 `urn:cellan:audit:v1` 精准识别），
//!    并辅以 `docProps/custom.xml` 中的轻量标记 `_sc_audit_meta` 用于检测外部软件剥离审计部件。
//! 2. **防无限增长（有界体积）**：
//!    - 按「保存（Commit）」合并净变更，抵消会话内中间修改；
//!    - 字典去重作者/工作表名 + 单次大块变更截断摘要（保留全量 `overflow_digest`）+ 紧凑二进制 Delta-Varint
//!      编码单元格溯源表（`BlameMap`）+ DEFLATE 压缩；
//!    - 超出体积上限（默认 512 KB）或条数上限（默认 500 条）时，自动将最老提交折叠为带哈希锚点与
//!      Merkle 根的 `AuditRollup` 归档节点，链条永不断裂，且支持导出/验证 `.cellan-audit` 冷归档包。
//! 3. **密码学防篡改与不可抵赖**：
//!    - 哈希链（`prev_hash` + 单调 `seq`）防止删改历史记录；
//!    - 业务数据规范化状态根（`state_hash`）绑定整本工作簿的值、公式、结构、合并区、名称与保护状态，
//!      一旦绕过 CELLAN 用 Excel/WPS 偷偷改数，重新打开立即告警；
//!    - Windows CNG（`bcrypt.dll`）原生 ECDSA P-256 非对称签名与验签（零外部依赖，支持离线构建），
//!      支持多人协作提交的联署签名（`CoSignerAttestation`）与 Windows DPAPI 私钥保护。

use crate::model::{Cell, Sheet, Value, Workbook};
use crate::store::{CellView, PAGE_ROWS};
use anyhow::{Context, Result};
use base64::Engine as _;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::sync::Arc;

pub const AUDIT_FORMAT_VERSION: u16 = 1;
pub const AUDIT_XML_NS: &str = "urn:cellan:audit:v1";
pub const AUDIT_CUSTOM_XML_PART: &str = "customXml/cellanAudit.xml";
pub const AUDIT_CUSTOM_XML_PROPS_PART: &str = "customXml/cellanAuditProps.xml";
pub const AUDIT_CUSTOM_XML_RELS_PART: &str = "customXml/_rels/cellanAudit.xml.rels";
pub const AUDIT_CUSTOM_PROP_NAME: &str = "_sc_audit_meta";
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// 紧凑二进制载荷魔数
const PAYLOAD_MAGIC: &[u8; 8] = b"CLNAUD01";
/// 默认内嵌压缩后体积硬上限（512 KB）
pub const DEFAULT_MAX_EMBEDDED_BYTES: u32 = 512 * 1024;
/// 默认内嵌保留的详细提交条数上限
pub const DEFAULT_MAX_DETAILED_COMMITS: u32 = 500;
/// 单次提交明细最多精确记录多少个变更单元格（超出部分以摘要 + SHA-256 覆盖）
pub const MAX_COMMIT_CELLS: usize = 120;
/// 审计快照里单个文本值的最大保留字节数
pub const MAX_SNAP_TEXT_LEN: usize = 512;
/// 单元格最后修改溯源表（Blame）条数硬上限
pub const MAX_BLAME_ENTRIES: usize = 100_000;
/// 允许的局域网本机时钟误差（60 秒内不报时钟倒退）
const CLOCK_SKEW_TOLERANCE_MS: u64 = 60_000;

// ---------------------------------------------------------------------------
// 密码学原语：Windows CNG (bcrypt.dll) ECDSA P-256 签名/验签 + DPAPI 保护
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditKeyPair {
    /// 64 字节未压缩公钥 X||Y 的小写十六进制（128 字符）
    pub pubkey_hex: String,
    /// Windows CNG ECCPRIVATEBLOB 载荷：X(32) || Y(32) || d(32)
    pub priv_blob: [u8; 96],
}

impl AuditKeyPair {
    pub fn fingerprint(&self) -> String {
        pubkey_fingerprint(&self.pubkey_hex)
    }

    pub fn sign_digest(&self, digest: &[u8; 32]) -> Result<String> {
        let sig = crypto_cng::sign_p256_sha256(&self.priv_blob, digest)?;
        Ok(hex_encode(&sig))
    }

    pub fn from_priv_blob(priv_blob: [u8; 96]) -> Result<Self> {
        let pubkey_hex = hex_encode(&priv_blob[..64]);
        let kp = Self { pubkey_hex, priv_blob };
        // 自检密钥对可用性
        let probe = [0x42u8; 32];
        let sig = kp.sign_digest(&probe).context("本地审计私钥格式无效")?;
        anyhow::ensure!(
            verify_digest_hex(&kp.pubkey_hex, &probe, &sig),
            "本地审计密钥对自检验签失败"
        );
        Ok(kp)
    }
}

pub fn generate_keypair() -> Result<AuditKeyPair> {
    let priv_blob = crypto_cng::generate_p256_keypair()?;
    let pubkey_hex = hex_encode(&priv_blob[..64]);
    Ok(AuditKeyPair { pubkey_hex, priv_blob })
}

pub fn verify_digest_hex(pubkey_hex: &str, digest: &[u8; 32], sig_hex: &str) -> bool {
    let Some(pub_bytes) = hex_decode_64(pubkey_hex) else {
        return false;
    };
    let Some(sig_bytes) = hex_decode_64(sig_hex) else {
        return false;
    };
    crypto_cng::verify_p256_sha256(&pub_bytes, digest, &sig_bytes)
}

pub fn pubkey_fingerprint(pubkey_hex: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"cellan-key-fp-v1:");
    h.update(pubkey_hex.to_ascii_lowercase().as_bytes());
    let d = h.finalize();
    format!(
        "{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}",
        d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

pub fn dpapi_protect(plaintext: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
    crypto_cng::dpapi_protect(plaintext, entropy)
}

pub fn dpapi_unprotect(ciphertext: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
    crypto_cng::dpapi_unprotect(ciphertext, entropy)
}

#[cfg(windows)]
mod crypto_cng {
    use anyhow::{bail, Result};
    use core::ffi::c_void;

    type NtStatus = i32;
    type AlgHandle = *mut c_void;
    type KeyHandle = *mut c_void;

    const BCRYPT_ECDSA_PUBLIC_P256_MAGIC: u32 = 0x3153_4345; // "ECS1"
    const BCRYPT_ECDSA_PRIVATE_P256_MAGIC: u32 = 0x3253_4345; // "ECS2"

    #[link(name = "bcrypt")]
    unsafe extern "system" {
        fn BCryptOpenAlgorithmProvider(
            ph_algorithm: *mut AlgHandle,
            psz_alg_id: *const u16,
            psz_implementation: *const u16,
            dw_flags: u32,
        ) -> NtStatus;
        fn BCryptCloseAlgorithmProvider(h_algorithm: AlgHandle, dw_flags: u32) -> NtStatus;
        fn BCryptGenerateKeyPair(
            h_algorithm: AlgHandle,
            ph_key: *mut KeyHandle,
            dw_length: u32,
            dw_flags: u32,
        ) -> NtStatus;
        fn BCryptFinalizeKeyPair(h_key: KeyHandle, dw_flags: u32) -> NtStatus;
        fn BCryptExportKey(
            h_key: KeyHandle,
            h_export_key: KeyHandle,
            psz_blob_type: *const u16,
            pb_output: *mut u8,
            cb_output: u32,
            pcb_result: *mut u32,
            dw_flags: u32,
        ) -> NtStatus;
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
        fn BCryptVerifySignature(
            h_key: KeyHandle,
            p_padding_info: *const c_void,
            pb_hash: *const u8,
            cb_hash: u32,
            pb_signature: *const u8,
            cb_signature: u32,
            dw_flags: u32,
        ) -> NtStatus;
    }

    #[repr(C)]
    struct CryptDataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            p_data_in: *const CryptDataBlob,
            sz_data_descr: *const u16,
            p_optional_entropy: *const CryptDataBlob,
            pv_reserved: *mut c_void,
            p_prompt_struct: *const c_void,
            dw_flags: u32,
            p_data_out: *mut CryptDataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            p_data_in: *const CryptDataBlob,
            ppsz_data_descr: *mut *mut u16,
            p_optional_entropy: *const CryptDataBlob,
            pv_reserved: *mut c_void,
            p_prompt_struct: *const c_void,
            dw_flags: u32,
            p_data_out: *mut CryptDataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(h_mem: *mut c_void) -> *mut c_void;
    }

    fn wide_z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(core::iter::once(0)).collect()
    }

    struct AlgGuard(AlgHandle);
    impl Drop for AlgGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = BCryptCloseAlgorithmProvider(self.0, 0);
                }
            }
        }
    }

    struct KeyGuard(KeyHandle);
    impl Drop for KeyGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = BCryptDestroyKey(self.0);
                }
            }
        }
    }

    fn open_p256() -> Result<AlgGuard> {
        let alg_name = wide_z("ECDSA_P256");
        let mut alg: AlgHandle = core::ptr::null_mut();
        let st = unsafe {
            BCryptOpenAlgorithmProvider(&mut alg, alg_name.as_ptr(), core::ptr::null(), 0)
        };
        if st != 0 || alg.is_null() {
            bail!("打开 Windows CNG ECDSA_P256 算法提供器失败 (NTSTATUS=0x{st:08x})");
        }
        Ok(AlgGuard(alg))
    }

    pub fn generate_p256_keypair() -> Result<[u8; 96]> {
        let alg = open_p256()?;
        let mut key: KeyHandle = core::ptr::null_mut();
        let st = unsafe { BCryptGenerateKeyPair(alg.0, &mut key, 256, 0) };
        if st != 0 || key.is_null() {
            bail!("生成 ECDSA P-256 密钥对失败 (NTSTATUS=0x{st:08x})");
        }
        let key = KeyGuard(key);
        let st = unsafe { BCryptFinalizeKeyPair(key.0, 0) };
        if st != 0 {
            bail!("定型 ECDSA P-256 密钥对失败 (NTSTATUS=0x{st:08x})");
        }
        let blob_type = wide_z("ECCPRIVATEBLOB");
        let mut buf = [0u8; 8 + 96];
        let mut got = 0u32;
        let st = unsafe {
            BCryptExportKey(
                key.0,
                core::ptr::null_mut(),
                blob_type.as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut got,
                0,
            )
        };
        if st != 0 || got != 104 {
            bail!("导出 ECDSA P-256 私钥块失败 (NTSTATUS=0x{st:08x}, len={got})");
        }
        let mut out = [0u8; 96];
        out.copy_from_slice(&buf[8..104]);
        Ok(out)
    }

    pub fn sign_p256_sha256(priv_blob: &[u8; 96], digest: &[u8; 32]) -> Result<[u8; 64]> {
        let alg = open_p256()?;
        let blob_type = wide_z("ECCPRIVATEBLOB");
        let mut raw = [0u8; 8 + 96];
        raw[0..4].copy_from_slice(&BCRYPT_ECDSA_PRIVATE_P256_MAGIC.to_le_bytes());
        raw[4..8].copy_from_slice(&32u32.to_le_bytes());
        raw[8..104].copy_from_slice(priv_blob);

        let mut key: KeyHandle = core::ptr::null_mut();
        let st = unsafe {
            BCryptImportKeyPair(
                alg.0,
                core::ptr::null_mut(),
                blob_type.as_ptr(),
                &mut key,
                raw.as_ptr(),
                raw.len() as u32,
                0,
            )
        };
        if st != 0 || key.is_null() {
            bail!("导入 ECDSA P-256 私钥失败 (NTSTATUS=0x{st:08x})");
        }
        let key = KeyGuard(key);
        let mut sig = [0u8; 64];
        let mut got = 0u32;
        let st = unsafe {
            BCryptSignHash(
                key.0,
                core::ptr::null(),
                digest.as_ptr(),
                digest.len() as u32,
                sig.as_mut_ptr(),
                sig.len() as u32,
                &mut got,
                0,
            )
        };
        if st != 0 || got != 64 {
            bail!("ECDSA P-256 签名失败 (NTSTATUS=0x{st:08x}, len={got})");
        }
        Ok(sig)
    }

    pub fn verify_p256_sha256(pubkey_xy: &[u8; 64], digest: &[u8; 32], sig_rs: &[u8; 64]) -> bool {
        let Ok(alg) = open_p256() else {
            return false;
        };
        let blob_type = wide_z("ECCPUBLICBLOB");
        let mut raw = [0u8; 8 + 64];
        raw[0..4].copy_from_slice(&BCRYPT_ECDSA_PUBLIC_P256_MAGIC.to_le_bytes());
        raw[4..8].copy_from_slice(&32u32.to_le_bytes());
        raw[8..72].copy_from_slice(pubkey_xy);

        let mut key: KeyHandle = core::ptr::null_mut();
        let st = unsafe {
            BCryptImportKeyPair(
                alg.0,
                core::ptr::null_mut(),
                blob_type.as_ptr(),
                &mut key,
                raw.as_ptr(),
                raw.len() as u32,
                0,
            )
        };
        if st != 0 || key.is_null() {
            return false;
        }
        let key = KeyGuard(key);
        let verify_st = unsafe {
            BCryptVerifySignature(
                key.0,
                core::ptr::null(),
                digest.as_ptr(),
                digest.len() as u32,
                sig_rs.as_ptr(),
                sig_rs.len() as u32,
                0,
            )
        };
        verify_st == 0
    }

    pub fn dpapi_protect(plaintext: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
        let in_blob = CryptDataBlob {
            cb_data: plaintext.len() as u32,
            pb_data: plaintext.as_ptr() as *mut u8,
        };
        let ent_blob = CryptDataBlob {
            cb_data: entropy.len() as u32,
            pb_data: entropy.as_ptr() as *mut u8,
        };
        let descr = wide_z("CELLAN Audit Signing Key");
        let mut out_blob = CryptDataBlob { cb_data: 0, pb_data: core::ptr::null_mut() };
        let ok = unsafe {
            CryptProtectData(
                &in_blob,
                descr.as_ptr(),
                if entropy.is_empty() { core::ptr::null() } else { &ent_blob },
                core::ptr::null_mut(),
                core::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out_blob,
            )
        };
        if ok == 0 || out_blob.pb_data.is_null() {
            bail!("Windows DPAPI 加密本地签名密钥失败");
        }
        let slice = unsafe { core::slice::from_raw_parts(out_blob.pb_data, out_blob.cb_data as usize) };
        let res = slice.to_vec();
        unsafe {
            let _ = LocalFree(out_blob.pb_data as *mut c_void);
        }
        Ok(res)
    }

    pub fn dpapi_unprotect(ciphertext: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
        let in_blob = CryptDataBlob {
            cb_data: ciphertext.len() as u32,
            pb_data: ciphertext.as_ptr() as *mut u8,
        };
        let ent_blob = CryptDataBlob {
            cb_data: entropy.len() as u32,
            pb_data: entropy.as_ptr() as *mut u8,
        };
        let mut out_blob = CryptDataBlob { cb_data: 0, pb_data: core::ptr::null_mut() };
        let ok = unsafe {
            CryptUnprotectData(
                &in_blob,
                core::ptr::null_mut(),
                if entropy.is_empty() { core::ptr::null() } else { &ent_blob },
                core::ptr::null_mut(),
                core::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out_blob,
            )
        };
        if ok == 0 || out_blob.pb_data.is_null() {
            bail!("Windows DPAPI 解密本地签名密钥失败（可能更换了 Windows 用户账户）");
        }
        let slice = unsafe { core::slice::from_raw_parts(out_blob.pb_data, out_blob.cb_data as usize) };
        let res = slice.to_vec();
        unsafe {
            let _ = LocalFree(out_blob.pb_data as *mut c_void);
        }
        Ok(res)
    }
}

#[cfg(not(windows))]
mod crypto_cng {
    use anyhow::Result;
    use sha2::{Digest, Sha256};

    pub fn generate_p256_keypair() -> Result<[u8; 96]> {
        let mut blob = [0u8; 96];
        getrandom::fill(&mut blob).map_err(|e| anyhow::anyhow!("随机数失败: {e}"))?;
        let d_pub = Sha256::digest(&blob[64..96]);
        let d_pub2 = Sha256::digest(&d_pub);
        blob[0..32].copy_from_slice(&d_pub);
        blob[32..64].copy_from_slice(&d_pub2);
        Ok(blob)
    }

    pub fn sign_p256_sha256(priv_blob: &[u8; 96], digest: &[u8; 32]) -> Result<[u8; 64]> {
        let mut sig = [0u8; 64];
        let mut h1 = Sha256::new();
        h1.update(b"nonwin-fallback-sig-1:");
        h1.update(&priv_blob[0..64]);
        h1.update(digest);
        sig[0..32].copy_from_slice(&h1.finalize());
        let mut h2 = Sha256::new();
        h2.update(b"nonwin-fallback-sig-2:");
        h2.update(&priv_blob[0..64]);
        h2.update(digest);
        sig[32..64].copy_from_slice(&h2.finalize());
        Ok(sig)
    }

    pub fn verify_p256_sha256(pubkey_xy: &[u8; 64], digest: &[u8; 32], sig_rs: &[u8; 64]) -> bool {
        let mut expect = [0u8; 64];
        let mut h1 = Sha256::new();
        h1.update(b"nonwin-fallback-sig-1:");
        h1.update(pubkey_xy);
        h1.update(digest);
        expect[0..32].copy_from_slice(&h1.finalize());
        let mut h2 = Sha256::new();
        h2.update(b"nonwin-fallback-sig-2:");
        h2.update(pubkey_xy);
        h2.update(digest);
        expect[32..64].copy_from_slice(&h2.finalize());
        expect == *sig_rs
    }

    pub fn dpapi_protect(plaintext: &[u8], _entropy: &[u8]) -> Result<Vec<u8>> {
        Ok(plaintext.to_vec())
    }

    pub fn dpapi_unprotect(ciphertext: &[u8], _entropy: &[u8]) -> Result<Vec<u8>> {
        Ok(ciphertext.to_vec())
    }
}

// ---------------------------------------------------------------------------
// 核心数据结构
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct AuditAuthor {
    #[serde(rename = "n")]
    pub display_name: String,
    #[serde(rename = "u", default)]
    pub os_user: String,
    #[serde(rename = "m", default)]
    pub machine: String,
    /// 64 字节 ECDSA P-256 公钥十六进制（128 字符）
    #[serde(rename = "k")]
    pub pubkey_hex: String,
}

impl AuditAuthor {
    pub fn fingerprint(&self) -> String {
        pubkey_fingerprint(&self.pubkey_hex)
    }

    pub fn short_label(&self) -> String {
        let name = if self.display_name.trim().is_empty() {
            if self.os_user.is_empty() { "未知用户" } else { self.os_user.as_str() }
        } else {
            self.display_name.as_str()
        };
        if self.machine.is_empty() {
            name.to_string()
        } else {
            format!("{name} ({})", self.machine)
        }
    }
}

/// 紧凑单元格语义快照（不含样式）
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum AuditSnap {
    #[default]
    #[serde(rename = "e")]
    Empty,
    #[serde(rename = "n")]
    Num(f64),
    #[serde(rename = "s")]
    Text(String),
    #[serde(rename = "b")]
    Bool(bool),
    #[serde(rename = "x")]
    Err(String),
    #[serde(rename = "f")]
    Formula(String),
}

impl AuditSnap {
    pub fn from_cell_view(v: Option<CellView<'_>>) -> Self {
        let Some(v) = v else {
            return Self::Empty;
        };
        if let Some(f) = v.formula() {
            return Self::Formula(normalize_formula_for_audit(f));
        }
        Self::from_value(&v.value())
    }

    pub fn from_cell(c: Option<&Cell>) -> Self {
        let Some(c) = c else {
            return Self::Empty;
        };
        if let Some(f) = c.formula.as_deref() {
            return Self::Formula(normalize_formula_for_audit(f));
        }
        Self::from_value(&c.value)
    }

    pub fn from_value(v: &Value) -> Self {
        match v {
            Value::Empty => Self::Empty,
            Value::Number(n) => {
                let norm = if *n == 0.0 { 0.0 } else { *n };
                Self::Num(norm)
            }
            Value::Bool(b) => Self::Bool(*b),
            Value::Error(e) => Self::Err(e.text().to_string()),
            Value::Text(s) => {
                let norm = normalize_text_for_audit(s);
                if norm.is_empty() {
                    Self::Empty
                } else {
                    Self::Text(truncate_snap_text(&norm))
                }
            }
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Empty => "(空)".into(),
            Self::Num(n) => crate::model::format_number(*n),
            Self::Text(s) => s.clone(),
            Self::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
            Self::Err(e) => e.clone(),
            Self::Formula(f) => f.clone(),
        }
    }

    fn write_canonical(&self, h: &mut Sha256) {
        match self {
            Self::Empty => h.update(b"E;"),
            Self::Num(n) => {
                h.update(b"N:");
                let bits = canonical_f64_bits(*n);
                h.update(bits.to_le_bytes());
                h.update(b";");
            }
            Self::Text(s) => {
                h.update(b"S:");
                h.update((s.len() as u32).to_le_bytes());
                h.update(s.as_bytes());
                h.update(b";");
            }
            Self::Bool(b) => {
                h.update(if *b { b"B:1;" } else { b"B:0;" });
            }
            Self::Err(e) => {
                h.update(b"X:");
                h.update((e.len() as u32).to_le_bytes());
                h.update(e.as_bytes());
                h.update(b";");
            }
            Self::Formula(f) => {
                h.update(b"F:");
                h.update((f.len() as u32).to_le_bytes());
                h.update(f.as_bytes());
                h.update(b";");
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AuditCellChange {
    #[serde(rename = "s")]
    pub sheet_idx: u16,
    #[serde(rename = "r")]
    pub row: u32,
    #[serde(rename = "c")]
    pub col: u32,
    #[serde(rename = "b")]
    pub before: AuditSnap,
    #[serde(rename = "a")]
    pub after: AuditSnap,
    #[serde(rename = "u")]
    pub author_idx: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditBulkOp {
    #[serde(rename = "u")]
    pub author_idx: u16,
    #[serde(rename = "s")]
    pub sheet_idx: u16,
    #[serde(rename = "k")]
    pub kind: String,
    #[serde(rename = "m")]
    pub summary: String,
    #[serde(rename = "d", default, skip_serializing_if = "String::is_empty")]
    pub range_digest: String,
}

/// 局域网协作多作者联署证明
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoSignerAttestation {
    #[serde(rename = "u")]
    pub author_idx: u16,
    #[serde(rename = "n")]
    pub change_count: u32,
    #[serde(rename = "d")]
    pub changes_digest: String,
    #[serde(rename = "g", default, skip_serializing_if = "String::is_empty")]
    pub sig_hex: String,
    /// `true` 表示由该协作者本人私钥签名；`false` 表示仅由保存宿主代为署名（如对方已离线或旧版客户端）
    #[serde(rename = "p", default)]
    pub attested_by_peer: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AuditCommit {
    #[serde(rename = "q")]
    pub seq: u64,
    #[serde(rename = "t")]
    pub ts_ms: u64,
    #[serde(rename = "p")]
    pub prev_hash: String,
    #[serde(rename = "st")]
    pub state_hash: String,
    /// 若打开文件时检测到绕过审计的外部修改（Excel/WPS 等），在后续合规保存时永久记下当时观察到的外部状态哈希
    #[serde(rename = "eg", default, skip_serializing_if = "Option::is_none")]
    pub external_gap_hash: Option<String>,
    #[serde(rename = "u")]
    pub committer_idx: u16,
    #[serde(rename = "ac")]
    pub action: String,
    #[serde(rename = "cs", default, skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<AuditCellChange>,
    #[serde(rename = "tc", default)]
    pub total_cell_changes: u32,
    #[serde(rename = "od", default, skip_serializing_if = "String::is_empty")]
    pub overflow_digest: String,
    #[serde(rename = "bo", default, skip_serializing_if = "Vec::is_empty")]
    pub bulk_ops: Vec<AuditBulkOp>,
    #[serde(rename = "co", default, skip_serializing_if = "Vec::is_empty")]
    pub co_signers: Vec<CoSignerAttestation>,
    #[serde(rename = "h")]
    pub commit_hash: String,
    #[serde(rename = "g")]
    pub sig_hex: String,
}

impl AuditCommit {
    pub fn compute_hash(&self, doc_id: &str) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"CELLAN-AUDIT-COMMIT-V1\n");
        write_str(&mut h, doc_id);
        h.update(self.seq.to_le_bytes());
        h.update(self.ts_ms.to_le_bytes());
        write_str(&mut h, &self.prev_hash);
        write_str(&mut h, &self.state_hash);
        match &self.external_gap_hash {
            Some(eg) => {
                h.update([1u8]);
                write_str(&mut h, eg);
            }
            None => h.update([0u8]),
        }
        h.update(self.committer_idx.to_le_bytes());
        write_str(&mut h, &self.action);
        h.update((self.cells.len() as u32).to_le_bytes());
        for ch in &self.cells {
            h.update(ch.sheet_idx.to_le_bytes());
            h.update(ch.row.to_le_bytes());
            h.update(ch.col.to_le_bytes());
            ch.before.write_canonical(&mut h);
            ch.after.write_canonical(&mut h);
            h.update(ch.author_idx.to_le_bytes());
        }
        h.update(self.total_cell_changes.to_le_bytes());
        write_str(&mut h, &self.overflow_digest);
        h.update((self.bulk_ops.len() as u32).to_le_bytes());
        for op in &self.bulk_ops {
            h.update(op.author_idx.to_le_bytes());
            h.update(op.sheet_idx.to_le_bytes());
            write_str(&mut h, &op.kind);
            write_str(&mut h, &op.summary);
            write_str(&mut h, &op.range_digest);
        }
        h.update((self.co_signers.len() as u32).to_le_bytes());
        for co in &self.co_signers {
            h.update(co.author_idx.to_le_bytes());
            h.update(co.change_count.to_le_bytes());
            write_str(&mut h, &co.changes_digest);
            write_str(&mut h, &co.sig_hex);
            h.update([u8::from(co.attested_by_peer)]);
        }
        h.finalize().into()
    }
}

/// 滚动归档折叠节点（封顶内嵌体积同时维持哈希链连续性）
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRollup {
    #[serde(rename = "fq")]
    pub from_seq: u64,
    #[serde(rename = "tq")]
    pub to_seq: u64,
    #[serde(rename = "ft")]
    pub from_ts_ms: u64,
    #[serde(rename = "tt")]
    pub to_ts_ms: u64,
    #[serde(rename = "pa")]
    pub prev_anchor_hash: String,
    #[serde(rename = "ah")]
    pub anchor_hash: String,
    #[serde(rename = "mr")]
    pub commit_merkle_root: String,
    #[serde(rename = "ac")]
    pub author_counts: Vec<(u16, u32)>,
    #[serde(rename = "tc")]
    pub total_cell_changes: u64,
    #[serde(rename = "cu")]
    pub compacted_by_idx: u16,
    #[serde(rename = "ct")]
    pub compacted_ts_ms: u64,
    #[serde(rename = "h")]
    pub rollup_hash: String,
    #[serde(rename = "g")]
    pub sig_hex: String,
}

impl AuditRollup {
    pub fn compute_hash(&self, doc_id: &str) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"CELLAN-AUDIT-ROLLUP-V1\n");
        write_str(&mut h, doc_id);
        h.update(self.from_seq.to_le_bytes());
        h.update(self.to_seq.to_le_bytes());
        h.update(self.from_ts_ms.to_le_bytes());
        h.update(self.to_ts_ms.to_le_bytes());
        write_str(&mut h, &self.prev_anchor_hash);
        write_str(&mut h, &self.anchor_hash);
        write_str(&mut h, &self.commit_merkle_root);
        h.update((self.author_counts.len() as u32).to_le_bytes());
        for &(au, cnt) in &self.author_counts {
            h.update(au.to_le_bytes());
            h.update(cnt.to_le_bytes());
        }
        h.update(self.total_cell_changes.to_le_bytes());
        h.update(self.compacted_by_idx.to_le_bytes());
        h.update(self.compacted_ts_ms.to_le_bytes());
        h.finalize().into()
    }
}

/// 单元格级最后修改溯源记录（Blame）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlameRecord {
    pub sheet_idx: u16,
    pub row: u32,
    pub col: u32,
    pub author_idx: u16,
    pub seq: u64,
    pub ts_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameView {
    pub author: AuditAuthor,
    pub seq: u64,
    pub ts_ms: u64,
    /// `true` 表示该记录仅来自开启审计时的创世基线（#1），开启审计后尚未发生实际修改
    pub is_baseline: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CellHistoryHit {
    pub seq: u64,
    pub ts_ms: u64,
    pub action: String,
    pub author: AuditAuthor,
    pub before: AuditSnap,
    pub after: AuditSnap,
}

/// 独立可验证的冷归档包（`.cellan-audit`）
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AuditColdArchive {
    pub version: u16,
    pub doc_id: String,
    pub author_dict: Vec<AuditAuthor>,
    pub sheet_dict: Vec<String>,
    pub rollup: AuditRollup,
    pub commits: Vec<AuditCommit>,
}

/// 协作联署待提交输入
#[derive(Clone, Debug)]
pub struct PeerAttestationInput {
    pub author: AuditAuthor,
    pub changes_digest: String,
    pub sig_hex: String,
    pub attested_by_peer: bool,
}

/// 挂在 `Workbook` 上的审计状态（`Arc` 克隆零开销）
#[derive(Clone, Debug, Default)]
pub struct WorkbookAudit {
    pub log: Option<Arc<AuditLog>>,
    /// 若 `docProps/custom.xml` 留有 `_sc_audit_meta` 标记但 `customXml` 审计主体被剥离
    pub stripped_marker: Option<String>,
    /// 若 `customXml` 审计部件存在但解析/解压失败（疑似损坏或篡改）
    pub parse_error: Option<String>,
    /// 保存干净副本时置为 `true`：强制清除输出 `.xlsx` 中的所有审计部件与自定义属性标记
    pub strip_on_save: bool,
}

impl WorkbookAudit {
    pub fn is_enabled(&self) -> bool {
        self.log.is_some()
    }

    pub fn has_anomaly(&self) -> bool {
        self.stripped_marker.is_some() || self.parse_error.is_some()
    }
}

/// 主审计日志对象（内嵌于 `.xlsx` 的 `customXml/cellanAudit.xml`）
#[derive(Clone, Debug, PartialEq)]
pub struct AuditLog {
    pub version: u16,
    pub doc_id: String,
    pub created_ts_ms: u64,
    pub created_by_idx: u16,
    pub max_embedded_bytes: u32,
    pub max_detailed_commits: u32,
    pub author_dict: Vec<AuditAuthor>,
    pub sheet_dict: Vec<String>,
    /// 当前存活工作表名称 -> 持久化审计槽位 `sheet_idx`（跨重命名、新建同名表、保存与重开精确识别同一张表）
    pub live_sheet_slots: Vec<(String, u16)>,
    pub rollups: Vec<AuditRollup>,
    pub commits: Vec<AuditCommit>,
    /// 按 `(sheet_idx, row, col)` 严格升序排列的单元格最后修改溯源表
    pub blame: Vec<BlameRecord>,
    /// 最近一次压缩落盘时刚被折叠掉的详细提交（供 UI 立即导出 `.cellan-audit` 冷归档，不重复写入 `.xlsx`）
    pub last_compacted: Option<AuditColdArchive>,
}

/// 序列化用内部头结构（不含 `blame`，`blame` 走独立 Delta-Varint 二进制段）
#[derive(Serialize, Deserialize)]
struct AuditLogHeader {
    #[serde(rename = "v")]
    version: u16,
    #[serde(rename = "id")]
    doc_id: String,
    #[serde(rename = "ct")]
    created_ts_ms: u64,
    #[serde(rename = "cu")]
    created_by_idx: u16,
    #[serde(rename = "mb")]
    max_embedded_bytes: u32,
    #[serde(rename = "mc")]
    max_detailed_commits: u32,
    #[serde(rename = "ad")]
    author_dict: Vec<AuditAuthor>,
    #[serde(rename = "sd")]
    sheet_dict: Vec<String>,
    #[serde(rename = "ls", default, skip_serializing_if = "Vec::is_empty")]
    live_sheet_slots: Vec<(String, u16)>,
    #[serde(rename = "ro", default, skip_serializing_if = "Vec::is_empty")]
    rollups: Vec<AuditRollup>,
    #[serde(rename = "cm", default, skip_serializing_if = "Vec::is_empty")]
    commits: Vec<AuditCommit>,
}

/// 当前工作簿单张表相对上次已提交基线的持久槽位绑定信息
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetAuditBinding {
    pub identity: u64,
    pub audit_slot_hint: Option<u16>,
    pub old_name: Option<String>,
    pub new_name: String,
}

/// 本次保存相比上次已落盘基线的净变更集
#[derive(Clone, Debug, Default)]
pub struct PendingAuditDelta {
    /// `(sheet_name, row, col, before, after, author)`
    pub cells: Vec<(String, u32, u32, AuditSnap, AuditSnap, AuditAuthor)>,
    /// `(sheet_name, author, kind, summary, range_digest)`
    pub bulk_ops: Vec<(String, AuditAuthor, String, String, String)>,
    /// 当前工作簿所有存活表的槽位继承/新建描述（用于重命名与新建同名表防串号）
    pub sheet_bindings: Vec<SheetAuditBinding>,
    /// 本次保存前最终留存（未被撤销）的插删行列结构操作：`(sheet_name, author, Structural)`
    pub structural_ops: Vec<(String, AuditAuthor, crate::structural::Structural)>,
}

impl PendingAuditDelta {
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.bulk_ops.is_empty() && self.structural_ops.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditSeverity {
    Verified,
    Warning,
    Tampered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditIssueKind {
    BrokenHashChain,
    InvalidCommitSignature,
    InvalidRollupSignature,
    InvalidCoSignerSignature,
    ExternalModification,
    AuditStripped,
    CorruptPayload,
    RollbackDetected,
    ForkDetected,
    ClockRollback,
    IdentityKeyChanged,
    HistoricalExternalGap,
    UnattestedCollabEdits,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditIssue {
    pub severity: AuditSeverity,
    pub kind: AuditIssueKind,
    pub seq: Option<u64>,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct AuditVerification {
    pub severity: AuditSeverity,
    pub doc_id: Option<String>,
    pub head_seq: u64,
    pub head_hash: String,
    pub current_state_hash: String,
    pub expected_state_hash: Option<String>,
    pub embedded_bytes: usize,
    pub detailed_commit_count: usize,
    pub rolled_up_commit_count: u64,
    pub blame_count: usize,
    pub issues: Vec<AuditIssue>,
}

impl AuditVerification {
    pub fn has_external_gap(&self) -> bool {
        self.issues
            .iter()
            .any(|i| i.kind == AuditIssueKind::ExternalModification)
    }

    pub fn push_issue(&mut self, issue: AuditIssue) {
        if issue.severity == AuditSeverity::Tampered {
            self.severity = AuditSeverity::Tampered;
        } else if issue.severity == AuditSeverity::Warning && self.severity == AuditSeverity::Verified {
            self.severity = AuditSeverity::Warning;
        }
        self.issues.push(issue);
    }

    pub fn short_badge(&self) -> &'static str {
        match self.severity {
            AuditSeverity::Verified => "审计完整",
            AuditSeverity::Warning => "审计提醒",
            AuditSeverity::Tampered => "审计异常",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColdArchiveVerifyResult {
    pub valid: bool,
    pub matched_rollup_idx: Option<usize>,
    pub commit_count: usize,
    pub summary: String,
}

// ---------------------------------------------------------------------------
// 规范化业务数据状态哈希（State Root）
// ---------------------------------------------------------------------------

/// 规范化自定义名称公式正文：与 `patch::write_user_names` 对齐，剥离首尾空白及前导 `=`，
/// 避免内存带 `=`（如 `=0.05`）而写入 OOXML 后不带 `=`（`0.05`）导致 CELLAN 保存后重开误报外部修改。
#[inline]
pub fn canonical_defined_name_formula(f: &str) -> &str {
    f.trim().trim_start_matches('=').trim()
}

/// 计算整本工作簿业务数据的规范化 SHA-256 哈希。
///
/// 覆盖：日期体系、工作簿保护、自定义名称（不含内部 `_sc_*`）、工作表顺序/名称/可见性/保护/合并区、
/// 所有非空单元格（含公式文本或值，不含易变的公式缓存计算结果与 UI/样式表内部索引）。
pub fn workbook_state_hash(wb: &Workbook) -> String {
    let mut h = Sha256::new();
    h.update(b"CELLAN-WB-STATE-V1\n");
    h.update(if wb.date_system.is_1904() { b"D1904" } else { b"D1900" });

    // 工作簿保护
    match &wb.protection {
        Some(p) => {
            h.update([1u8, u8::from(p.lock_structure), u8::from(p.lock_windows)]);
            write_str(&mut h, &p.hash);
            h.update(p.legacy.unwrap_or(0).to_le_bytes());
        }
        None => h.update([0u8]),
    }

    // 自定义名称（过滤内部保留前缀 _sc_ 与 _xlnm._FilterDatabase 等易变项，与 OOXML 规范对齐剥离前导 '='）
    let mut names: Vec<(&str, i32, &str)> = wb
        .names
        .iter()
        .filter(|(n, _, _)| !n.starts_with("_sc_") && !n.eq_ignore_ascii_case("_xlnm._FilterDatabase"))
        .map(|(n, scope, f)| {
            (
                n.as_str(),
                scope.map(|i| i as i32).unwrap_or(-1),
                canonical_defined_name_formula(f),
            )
        })
        .collect();
    names.sort_unstable();
    h.update((names.len() as u32).to_le_bytes());
    for (n, scope, f) in names {
        write_str(&mut h, n);
        h.update(scope.to_le_bytes());
        write_str(&mut h, f);
    }

    // 工作表顺序与内容
    h.update((wb.sheets.len() as u32).to_le_bytes());
    for sheet in &wb.sheets {
        write_str(&mut h, &sheet.name);
        h.update([u8::from(sheet.hidden), u8::from(sheet.very_hidden)]);
        match &sheet.protection {
            Some(sp) => {
                h.update([1u8]);
                write_str(&mut h, &sp.hash);
                h.update(sp.legacy.unwrap_or(0).to_le_bytes());
            }
            None => h.update([0u8]),
        }

        // 合并区（排序后哈希）
        let mut merges: Vec<(u32, u32, u32, u32)> = sheet.merges.iter().copied().collect();
        merges.sort_unstable();
        h.update((merges.len() as u32).to_le_bytes());
        for (r1, c1, r2, c2) in merges {
            h.update(r1.to_le_bytes());
            h.update(c1.to_le_bytes());
            h.update(r2.to_le_bytes());
            h.update(c2.to_le_bytes());
        }

        // 单元格（iter_views 严格按 (row, col) 升序）
        let mut cell_count = 0u64;
        for (cref, view) in sheet.iter_views() {
            let snap = AuditSnap::from_cell_view(Some(view));
            if matches!(snap, AuditSnap::Empty) {
                continue;
            }
            cell_count += 1;
            h.update(cref.row.to_le_bytes());
            h.update(cref.col.to_le_bytes());
            snap.write_canonical(&mut h);
        }
        h.update(cell_count.to_le_bytes());
    }

    hex_encode(&h.finalize())
}

// ---------------------------------------------------------------------------
// CoW 增量净差计算（O(变化页数)）
// ---------------------------------------------------------------------------

/// 计算 `baseline` 与 `current` 之间的净业务变更。
///
/// 利用 `Sheet::same_page` 在 O(1) 内跳过所有未修改的 256 行数据页；
/// 对发生变更的格子，自动按 `cell_attributions` 归属到真实修改者（多人协作时精确区分各成员），
/// 会话内先改后还原的格子（`before == after`）自动抵消不入账。
pub fn compute_net_diff(
    baseline: &Workbook,
    current: &Workbook,
    default_author: &AuditAuthor,
    cell_attributions: &HashMap<(u64, u32, u32), AuditAuthor>,
    extra_bulk_ops: &[(String, AuditAuthor, String, String)],
) -> PendingAuditDelta {
    compute_net_diff_full(
        baseline,
        current,
        default_author,
        cell_attributions,
        &HashMap::new(),
        extra_bulk_ops,
        &[],
    )
}

/// 完整版净差计算：额外支持按合并区域精确归属作者（协作访客合并不再误归宿主）以及携带待落盘结构操作。
#[allow(clippy::too_many_arguments)]
pub fn compute_net_diff_full(
    baseline: &Workbook,
    current: &Workbook,
    default_author: &AuditAuthor,
    cell_attributions: &HashMap<(u64, u32, u32), AuditAuthor>,
    merge_attributions: &HashMap<(u64, (u32, u32, u32, u32)), AuditAuthor>,
    extra_bulk_ops: &[(String, AuditAuthor, String, String)],
    structural_ops: &[(String, AuditAuthor, crate::structural::Structural)],
) -> PendingAuditDelta {
    let mut delta = PendingAuditDelta::default();
    for (sheet_name, author, kind, summary) in extra_bulk_ops {
        delta.bulk_ops.push((
            sheet_name.clone(),
            author.clone(),
            kind.clone(),
            summary.clone(),
            String::new(),
        ));
    }
    delta.structural_ops.extend(structural_ops.iter().cloned());

    // 按 identity 匹配基准表
    let base_by_id: HashMap<u64, &Sheet> = baseline.sheets.iter().map(|s| (s.identity, s)).collect();
    let cur_ids: HashSet<u64> = current.sheets.iter().map(|s| s.identity).collect();

    // 记录当前所有存活表的槽位绑定（供 append_commit 精确区分重命名与新建同名表）
    for csh in &current.sheets {
        let base_sh = base_by_id.get(&csh.identity).copied();
        delta.sheet_bindings.push(SheetAuditBinding {
            identity: csh.identity,
            audit_slot_hint: csh.audit_sheet_idx.or_else(|| base_sh.and_then(|b| b.audit_sheet_idx)),
            old_name: base_sh.map(|b| b.name.clone()),
            new_name: csh.name.clone(),
        });
    }

    // 删除的工作表
    for bsh in &baseline.sheets {
        if !cur_ids.contains(&bsh.identity) {
            delta.bulk_ops.push((
                bsh.name.clone(),
                default_author.clone(),
                "工作表变更".into(),
                format!("删除工作表「{}」（原有 {} 个非空格）", bsh.name, bsh.len()),
                String::new(),
            ));
        }
    }

    // 自定义名称变更（与 workbook_state_hash 使用同一套规范化规则）
    let canon_names = |wb: &Workbook| -> BTreeMap<(String, Option<usize>), String> {
        wb.names
            .iter()
            .filter(|(n, _, _)| !n.starts_with("_sc_") && !n.eq_ignore_ascii_case("_xlnm._FilterDatabase"))
            .map(|(n, sc, f)| ((n.clone(), *sc), canonical_defined_name_formula(f).to_string()))
            .collect()
    };
    let b_names = canon_names(baseline);
    let c_names = canon_names(current);
    if b_names != c_names {
        let mut added = 0usize;
        let mut changed = 0usize;
        let mut removed = 0usize;
        let mut sample = Vec::new();
        for (k, v) in &c_names {
            match b_names.get(k) {
                None => {
                    added += 1;
                    if sample.len() < 3 {
                        sample.push(format!("+{}", k.0));
                    }
                }
                Some(old_v) if old_v != v => {
                    changed += 1;
                    if sample.len() < 3 {
                        sample.push(format!("~{}", k.0));
                    }
                }
                _ => {}
            }
        }
        for k in b_names.keys() {
            if !c_names.contains_key(k) {
                removed += 1;
                if sample.len() < 3 {
                    sample.push(format!("-{}", k.0));
                }
            }
        }
        delta.bulk_ops.push((
            "(工作簿)".into(),
            default_author.clone(),
            "名称变更".into(),
            format!(
                "自定义名称变更（新增 {added} / 修改 {changed} / 删除 {removed}：{}）",
                sample.join("、")
            ),
            String::new(),
        ));
    }

    // 工作簿保护/加密变更
    if baseline.protection != current.protection {
        let msg = if current.protection.is_some() {
            "启用/修改工作簿结构保护"
        } else {
            "取消工作簿结构保护"
        };
        delta.bulk_ops.push((
            "(工作簿)".into(),
            default_author.clone(),
            "保护变更".into(),
            msg.into(),
            String::new(),
        ));
    }
    if baseline.encryption.is_some() != current.encryption.is_some() {
        let msg = if current.encryption.is_some() {
            "设置工作簿打开密码"
        } else {
            "取消工作簿打开密码"
        };
        delta.bulk_ops.push((
            "(工作簿)".into(),
            default_author.clone(),
            "保护变更".into(),
            msg.into(),
            String::new(),
        ));
    }

    // 逐表比对
    for csh in &current.sheets {
        let Some(&bsh) = base_by_id.get(&csh.identity) else {
            // 新建/复制的工作表
            delta.bulk_ops.push((
                csh.name.clone(),
                default_author.clone(),
                "工作表变更".into(),
                format!("新增工作表「{}」（{} 个非空格）", csh.name, csh.len()),
                String::new(),
            ));
            for (cref, view) in csh.iter_views() {
                let after = AuditSnap::from_cell_view(Some(view));
                if matches!(after, AuditSnap::Empty) {
                    continue;
                }
                let author = cell_attributions
                    .get(&(csh.identity, cref.row, cref.col))
                    .unwrap_or(default_author)
                    .clone();
                delta.cells.push((
                    csh.name.clone(),
                    cref.row,
                    cref.col,
                    AuditSnap::Empty,
                    after,
                    author,
                ));
            }
            continue;
        };

        if bsh.name != csh.name {
            delta.bulk_ops.push((
                csh.name.clone(),
                default_author.clone(),
                "工作表变更".into(),
                format!("重命名工作表「{}」→「{}」", bsh.name, csh.name),
                String::new(),
            ));
        }
        if bsh.protection != csh.protection {
            delta.bulk_ops.push((
                csh.name.clone(),
                default_author.clone(),
                "保护变更".into(),
                if csh.protection.is_some() {
                    "启用/修改工作表保护".into()
                } else {
                    "取消工作表保护".into()
                },
                String::new(),
            ));
        }

        // 合并区变更：逐块对比并按真实操作者（支持协作访客归属）分组记录
        if bsh.merges.len() != csh.merges.len()
            || bsh.merges.iter().ne(csh.merges.iter())
        {
            let b_set: BTreeSet<(u32, u32, u32, u32)> = bsh.merges.iter().copied().collect();
            let c_set: BTreeSet<(u32, u32, u32, u32)> = csh.merges.iter().copied().collect();
            let mut per_author_merges: Vec<(AuditAuthor, Vec<String>)> = Vec::new();
            let mut push_merge = |author: AuditAuthor, desc: String| {
                if let Some((_, list)) = per_author_merges.iter_mut().find(|(a, _)| *a == author) {
                    list.push(desc);
                } else {
                    per_author_merges.push((author, vec![desc]));
                }
            };
            for &rect in c_set.difference(&b_set) {
                let au = merge_attributions
                    .get(&(csh.identity, rect))
                    .unwrap_or(default_author)
                    .clone();
                let desc = format!(
                    "合并 {}:{}",
                    crate::model::cell_name(rect.0, rect.1),
                    crate::model::cell_name(rect.2, rect.3)
                );
                push_merge(au, desc);
            }
            for &rect in b_set.difference(&c_set) {
                let au = merge_attributions
                    .get(&(csh.identity, rect))
                    .unwrap_or(default_author)
                    .clone();
                let desc = format!(
                    "取消合并 {}:{}",
                    crate::model::cell_name(rect.0, rect.1),
                    crate::model::cell_name(rect.2, rect.3)
                );
                push_merge(au, desc);
            }
            for (au, items) in per_author_merges {
                let preview = if items.len() <= 3 {
                    items.join("、")
                } else {
                    format!("{} 等 {} 处", items[..3].join("、"), items.len())
                };
                delta.bulk_ops.push((
                    csh.name.clone(),
                    au,
                    "结构变更".into(),
                    format!(
                        "合并区域变更（{} → {} 处：{}）",
                        bsh.merges.len(),
                        csh.merges.len(),
                        preview
                    ),
                    String::new(),
                ));
            }
        }

        // 页级跳过（CoW O(1) 比对！）
        let max_row_b = bsh.used_bounds().map(|b| b.2);
        let max_row_c = csh.used_bounds().map(|b| b.2);
        let Some(max_row) = (match (max_row_b, max_row_c) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }) else {
            continue;
        };
        let last_page = max_row / PAGE_ROWS;
        for page_idx in 0..=last_page {
            if bsh.same_page(csh, page_idx) {
                continue;
            }
            let r1 = page_idx * PAGE_ROWS;
            let r2 = r1.saturating_add(PAGE_ROWS - 1);
            let mut it_b = bsh.iter_rect_views((r1, 0, r2, u32::MAX)).peekable();
            let mut it_c = csh.iter_rect_views((r1, 0, r2, u32::MAX)).peekable();
            loop {
                match (it_b.peek(), it_c.peek()) {
                    (None, None) => break,
                    (Some((rb, _)), Some((rc, _))) if rb == rc => {
                        let (cref, vb) = it_b.next().unwrap();
                        let (_, vc) = it_c.next().unwrap();
                        let sb = AuditSnap::from_cell_view(Some(vb));
                        let sc = AuditSnap::from_cell_view(Some(vc));
                        if sb != sc {
                            let author = cell_attributions
                                .get(&(csh.identity, cref.row, cref.col))
                                .unwrap_or(default_author)
                                .clone();
                            delta.cells.push((csh.name.clone(), cref.row, cref.col, sb, sc, author));
                        }
                    }
                    (Some((rb, _)), Some((rc, _))) if rb < rc => {
                        let (cref, vb) = it_b.next().unwrap();
                        let sb = AuditSnap::from_cell_view(Some(vb));
                        if !matches!(sb, AuditSnap::Empty) {
                            let author = cell_attributions
                                .get(&(csh.identity, cref.row, cref.col))
                                .unwrap_or(default_author)
                                .clone();
                            delta.cells.push((
                                csh.name.clone(),
                                cref.row,
                                cref.col,
                                sb,
                                AuditSnap::Empty,
                                author,
                            ));
                        }
                    }
                    (Some(_), Some(_)) => {
                        let (cref, vc) = it_c.next().unwrap();
                        let sc = AuditSnap::from_cell_view(Some(vc));
                        if !matches!(sc, AuditSnap::Empty) {
                            let author = cell_attributions
                                .get(&(csh.identity, cref.row, cref.col))
                                .unwrap_or(default_author)
                                .clone();
                            delta.cells.push((
                                csh.name.clone(),
                                cref.row,
                                cref.col,
                                AuditSnap::Empty,
                                sc,
                                author,
                            ));
                        }
                    }
                    (Some(_), None) => {
                        let (cref, vb) = it_b.next().unwrap();
                        let sb = AuditSnap::from_cell_view(Some(vb));
                        if !matches!(sb, AuditSnap::Empty) {
                            let author = cell_attributions
                                .get(&(csh.identity, cref.row, cref.col))
                                .unwrap_or(default_author)
                                .clone();
                            delta.cells.push((
                                csh.name.clone(),
                                cref.row,
                                cref.col,
                                sb,
                                AuditSnap::Empty,
                                author,
                            ));
                        }
                    }
                    (None, Some(_)) => {
                        let (cref, vc) = it_c.next().unwrap();
                        let sc = AuditSnap::from_cell_view(Some(vc));
                        if !matches!(sc, AuditSnap::Empty) {
                            let author = cell_attributions
                                .get(&(csh.identity, cref.row, cref.col))
                                .unwrap_or(default_author)
                                .clone();
                            delta.cells.push((
                                csh.name.clone(),
                                cref.row,
                                cref.col,
                                AuditSnap::Empty,
                                sc,
                                author,
                            ));
                        }
                    }
                }
            }
        }
    }

    delta
}

/// 为协作联署计算某一位作者在本轮变更集合中的规范化摘要（签名对象）
pub fn author_contribution_digest(
    doc_id: &str,
    base_seq: u64,
    cells: &[(String, u32, u32, AuditSnap, AuditSnap)],
) -> [u8; 32] {
    let mut sorted: Vec<&(String, u32, u32, AuditSnap, AuditSnap)> = cells.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2)));
    let mut h = Sha256::new();
    h.update(b"CELLAN-AUDIT-CONTRIB-V1\n");
    write_str(&mut h, doc_id);
    h.update(base_seq.to_le_bytes());
    h.update((sorted.len() as u32).to_le_bytes());
    for (sheet, r, c, b, a) in sorted {
        write_str(&mut h, sheet);
        h.update(r.to_le_bytes());
        h.update(c.to_le_bytes());
        b.write_canonical(&mut h);
        a.write_canonical(&mut h);
    }
    h.finalize().into()
}

// ---------------------------------------------------------------------------
// AuditLog 核心逻辑：启用、追加提交、滚动折叠、Blame、验签
// ---------------------------------------------------------------------------

impl AuditLog {
    /// 为工作簿启用防篡改审计记录（生成唯一 `doc_id` 与创世提交 `#1`）
    pub fn enable(
        wb: &Workbook,
        author: AuditAuthor,
        keypair: &AuditKeyPair,
        ts_ms: u64,
    ) -> Result<Self> {
        let mut raw_id = [0u8; 16];
        getrandom::fill(&mut raw_id).map_err(|e| anyhow::anyhow!("生成文档审计标识失败: {e}"))?;
        let doc_id = hex_encode(&raw_id);
        let state_hash = workbook_state_hash(wb);

        let mut log = Self {
            version: AUDIT_FORMAT_VERSION,
            doc_id,
            created_ts_ms: ts_ms,
            created_by_idx: 0,
            max_embedded_bytes: DEFAULT_MAX_EMBEDDED_BYTES,
            max_detailed_commits: DEFAULT_MAX_DETAILED_COMMITS,
            author_dict: Vec::new(),
            sheet_dict: Vec::new(),
            live_sheet_slots: Vec::new(),
            rollups: Vec::new(),
            commits: Vec::new(),
            blame: Vec::new(),
            last_compacted: None,
        };
        let committer_idx = log.intern_author(author);
        log.created_by_idx = committer_idx;

        let total_cells: usize = wb.sheets.iter().map(|s| s.len()).sum();
        let sheet_idx = log.intern_sheet("(工作簿)");
        let mut commit = AuditCommit {
            seq: 1,
            ts_ms,
            prev_hash: GENESIS_HASH.to_string(),
            state_hash,
            external_gap_hash: None,
            committer_idx,
            action: "启用防篡改审计".into(),
            cells: Vec::new(),
            total_cell_changes: 0,
            overflow_digest: String::new(),
            bulk_ops: vec![AuditBulkOp {
                author_idx: committer_idx,
                sheet_idx,
                kind: "启用审计".into(),
                summary: format!(
                    "初始化基线：{} 张工作表，共 {} 个非空单元格",
                    wb.sheets.len(),
                    total_cells
                ),
                range_digest: String::new(),
            }],
            co_signers: Vec::new(),
            commit_hash: String::new(),
            sig_hex: String::new(),
        };
        let digest = commit.compute_hash(&log.doc_id);
        commit.commit_hash = hex_encode(&digest);
        commit.sig_hex = keypair.sign_digest(&digest)?;
        log.commits.push(commit);

        // 为现有每张工作表分配独立持久化槽位，再为非空单元格建立初始基线 Blame（若不超过上限）
        let mut blame = Vec::new();
        let mut sheet_slots = Vec::with_capacity(wb.sheets.len());
        for sh in &wb.sheets {
            let si = log.alloc_new_sheet_slot(&sh.name);
            log.live_sheet_slots.push((sh.name.clone(), si));
            sheet_slots.push(si);
        }
        'outer: for (sh, &si) in wb.sheets.iter().zip(&sheet_slots) {
            for (cref, view) in sh.iter_views() {
                if matches!(AuditSnap::from_cell_view(Some(view)), AuditSnap::Empty) {
                    continue;
                }
                blame.push(BlameRecord {
                    sheet_idx: si,
                    row: cref.row,
                    col: cref.col,
                    author_idx: committer_idx,
                    seq: 1,
                    ts_ms,
                });
                if blame.len() >= MAX_BLAME_ENTRIES {
                    break 'outer;
                }
            }
        }
        blame.sort_unstable_by_key(|b| (b.sheet_idx, b.row, b.col));
        log.blame = blame;
        Ok(log)
    }

    pub fn head(&self) -> Option<&AuditCommit> {
        self.commits.last()
    }

    pub fn head_seq(&self) -> u64 {
        self.commits
            .last()
            .map(|c| c.seq)
            .or_else(|| self.rollups.last().map(|r| r.to_seq))
            .unwrap_or(0)
    }

    pub fn head_hash(&self) -> &str {
        self.commits
            .last()
            .map(|c| c.commit_hash.as_str())
            .or_else(|| self.rollups.last().map(|r| r.anchor_hash.as_str()))
            .unwrap_or(GENESIS_HASH)
    }

    pub fn head_state_hash(&self) -> Option<&str> {
        self.commits.last().map(|c| c.state_hash.as_str())
    }

    pub fn rolled_up_count(&self) -> u64 {
        self.rollups
            .iter()
            .map(|r| r.to_seq.saturating_sub(r.from_seq).saturating_add(1))
            .sum()
    }

    pub fn intern_author(&mut self, author: AuditAuthor) -> u16 {
        if let Some(pos) = self.author_dict.iter().position(|a| *a == author) {
            return pos as u16;
        }
        let idx = self.author_dict.len().min(u16::MAX as usize) as u16;
        if self.author_dict.len() < u16::MAX as usize {
            self.author_dict.push(author);
        }
        idx
    }

    pub fn intern_sheet(&mut self, name: &str) -> u16 {
        if let Some((_, si)) = self.live_sheet_slots.iter().find(|(n, _)| n == name) {
            return *si;
        }
        if let Some(pos) = self.sheet_dict.iter().position(|s| s == name) {
            return pos as u16;
        }
        self.alloc_new_sheet_slot(name)
    }

    fn alloc_new_sheet_slot(&mut self, name: &str) -> u16 {
        let idx = self.sheet_dict.len().min(u16::MAX as usize) as u16;
        if self.sheet_dict.len() < u16::MAX as usize {
            self.sheet_dict.push(name.to_string());
        }
        idx
    }

    /// 将当前已提交日志中的存活工作表槽位绑定回 `wb.sheets[*].audit_sheet_idx`，
    /// 使工作表即使在内存中被重命名、重排，也能始终追踪同一逻辑表。
    pub fn bind_workbook_sheets(&self, wb: &mut Workbook) {
        for sh in &mut wb.sheets {
            sh.audit_sheet_idx = self.resolve_sheet_slot_by_name(&sh.name);
        }
    }

    /// 按当前存活工作表名解析持久槽位（严格区分重命名旧表与新建同名表）。
    pub fn resolve_sheet_slot_by_name(&self, name: &str) -> Option<u16> {
        if !self.live_sheet_slots.is_empty() {
            return self
                .live_sheet_slots
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, si)| *si);
        }
        self.sheet_dict
            .iter()
            .position(|s| s == name)
            .map(|i| i as u16)
    }

    /// 在活动工作簿中解析某张 `Sheet` 的持久槽位（支持未保存前刚改名/新建同名表的实时查询）。
    pub fn resolve_sheet_slot_in_wb(&self, wb: &Workbook, sheet: &Sheet) -> Option<u16> {
        if let Some(si) = sheet.audit_sheet_idx {
            return Some(si);
        }
        let cand = self.resolve_sheet_slot_by_name(&sheet.name)?;
        if wb
            .sheets
            .iter()
            .any(|other| other.identity != sheet.identity && other.audit_sheet_idx == Some(cand))
        {
            return None;
        }
        Some(cand)
    }

    pub fn author_of(&self, idx: u16) -> Option<&AuditAuthor> {
        self.author_dict.get(idx as usize)
    }

    pub fn sheet_of(&self, idx: u16) -> &str {
        if let Some((cur_name, _)) = self.live_sheet_slots.iter().find(|(_, s)| *s == idx) {
            return cur_name.as_str();
        }
        self.sheet_dict
            .get(idx as usize)
            .map(|s| s.as_str())
            .unwrap_or("?")
    }

    /// 按持久槽位同步平移 Blame 坐标，确保插删行列后单元格溯源仍对准真实数据
    pub fn apply_structural_shift_slot(&mut self, si: u16, op: &crate::structural::Structural) {
        let mut next = Vec::with_capacity(self.blame.len());
        for mut b in std::mem::take(&mut self.blame) {
            if b.sheet_idx == si {
                if let Some((nr, nc)) = crate::sync::shift_structural_coord(b.row, b.col, op) {
                    b.row = nr;
                    b.col = nc;
                    next.push(b);
                }
            } else {
                next.push(b);
            }
        }
        next.sort_unstable_by_key(|b| (b.sheet_idx, b.row, b.col));
        self.blame = next;
    }

    /// 按表名同步平移 Blame 坐标（兼容旧调用方）
    pub fn apply_structural_shift(&mut self, sheet_name: &str, op: &crate::structural::Structural) {
        let Some(si) = self.resolve_sheet_slot_by_name(sheet_name) else {
            return;
        };
        self.apply_structural_shift_slot(si, op);
    }

    /// 根据本次提交的 `sheet_bindings` 与 `wb.sheets` 刷新 `live_sheet_slots`，
    /// 返回 `(当前表名 -> 持久槽位, 变动前旧表名 -> 持久槽位)`。
    fn reconcile_sheet_slots(
        &mut self,
        wb: &Workbook,
        bindings: &[SheetAuditBinding],
    ) -> (HashMap<String, u16>, HashMap<String, u16>) {
        // 兼容旧版 v1.82 日志（live_sheet_slots 为空时从 sheet_dict 反推）
        let prev_live: HashMap<String, u16> = if !self.live_sheet_slots.is_empty() {
            self.live_sheet_slots.iter().cloned().collect()
        } else {
            self.sheet_dict
                .iter()
                .enumerate()
                .filter(|(_, n)| n.as_str() != "(工作簿)")
                .map(|(i, n)| (n.clone(), i as u16))
                .collect()
        };

        let mut used_slots: HashSet<u16> = HashSet::new();
        let mut new_live: Vec<(String, u16)> = Vec::with_capacity(wb.sheets.len());
        let mut live_by_name: HashMap<String, u16> = HashMap::with_capacity(wb.sheets.len());

        // 第一遍：先保留已绑定旧槽位的存活表（包括重命名后的旧表），确保新表绝不会抢占旧槽位
        let mut resolved: Vec<Option<u16>> = vec![None; wb.sheets.len()];
        for (i, sh) in wb.sheets.iter().enumerate() {
            let bind = bindings.iter().find(|b| b.identity == sh.identity);
            let existing = if let Some(b) = bind {
                if b.old_name.is_some() {
                    b.audit_slot_hint
                        .or_else(|| b.old_name.as_deref().and_then(|o| prev_live.get(o).copied()))
                } else {
                    // 明确为新建/复制表：绝不继承任何旧槽位
                    None
                }
            } else {
                sh.audit_sheet_idx
                    .or_else(|| prev_live.get(&sh.name).copied())
            };
            if let Some(si) = existing {
                if used_slots.insert(si) {
                    resolved[i] = Some(si);
                }
            }
        }

        // 第二遍：为所有新建表分配全新的 sheet_dict 槽位
        for (i, sh) in wb.sheets.iter().enumerate() {
            let si = match resolved[i] {
                Some(si) => si,
                None => self.alloc_new_sheet_slot(&sh.name),
            };
            new_live.push((sh.name.clone(), si));
            live_by_name.insert(sh.name.clone(), si);
        }

        self.live_sheet_slots = new_live;
        (live_by_name, prev_live)
    }

    /// 追加一条经 ECDSA P-256 签名的防篡改提交，并自动维护 Blame 与滚动压缩上限。
    #[allow(clippy::too_many_arguments)]
    pub fn append_commit(
        &mut self,
        wb: &Workbook,
        committer: AuditAuthor,
        keypair: &AuditKeyPair,
        action: &str,
        delta: &PendingAuditDelta,
        co_signers_in: &[PeerAttestationInput],
        external_gap_hash: Option<String>,
        ts_ms: u64,
    ) -> Result<&AuditCommit> {
        let state_hash = workbook_state_hash(wb);
        self.append_commit_with_state_hash(
            wb,
            state_hash,
            committer,
            keypair,
            action,
            delta,
            co_signers_in,
            external_gap_hash,
            ts_ms,
        )
    }

    /// 同 `append_commit`，但复用调用方已计算的 `state_hash`，避免保存路径对大工作簿重复全量哈希扫描。
    #[allow(clippy::too_many_arguments)]
    pub fn append_commit_with_state_hash(
        &mut self,
        wb: &Workbook,
        state_hash: String,
        committer: AuditAuthor,
        keypair: &AuditKeyPair,
        action: &str,
        delta: &PendingAuditDelta,
        co_signers_in: &[PeerAttestationInput],
        external_gap_hash: Option<String>,
        ts_ms: u64,
    ) -> Result<&AuditCommit> {
        let prev_hash = self.head_hash().to_string();
        let seq = self.head_seq().saturating_add(1);
        let committer_idx = self.intern_author(committer.clone());

        // 1. 刷新存活工作表持久槽位映射（重命名继承旧槽位，新建同名表分配全新槽位）
        let (live_by_name, prev_live) = self.reconcile_sheet_slots(wb, &delta.sheet_bindings);
        let resolve_slot = |log: &mut AuditLog, name: &str| -> u16 {
            if let Some(&si) = live_by_name.get(name) {
                return si;
            }
            if let Some(&si) = prev_live.get(name) {
                return si;
            }
            log.intern_sheet(name)
        };

        // 2. 仅在落盘签名时将留存（未被撤销）的插删行列操作应用到 Blame 坐标
        for (sheet_name, _, op) in &delta.structural_ops {
            let si = resolve_slot(self, sheet_name);
            self.apply_structural_shift_slot(si, op);
        }

        let total_cell_changes = delta.cells.len() as u32;
        let mut kept_cells = Vec::with_capacity(delta.cells.len().min(MAX_COMMIT_CELLS));
        let mut overflow_hasher = (delta.cells.len() > MAX_COMMIT_CELLS).then(|| {
            let mut h = Sha256::new();
            h.update(b"CELLAN-AUDIT-OVERFLOW-V1\n");
            h
        });

        // 统计每位非提交者作者的单元格数与批量事件
        let mut per_author_counts: HashMap<u16, u32> = HashMap::new();
        let mut per_author_cells: HashMap<u16, Vec<(String, u32, u32, AuditSnap, AuditSnap)>> = HashMap::new();
        let mut per_author_has_bulk: HashSet<u16> = HashSet::new();

        // 更新 Blame 映射（所有本次真实修改无条件写入，超限时优先淘汰最老基线 #1）
        let mut blame_map: BTreeMap<(u16, u32, u32), (u16, u64, u64)> = self
            .blame
            .iter()
            .map(|b| ((b.sheet_idx, b.row, b.col), (b.author_idx, b.seq, b.ts_ms)))
            .collect();

        for (idx, (sheet_name, row, col, before, after, author)) in delta.cells.iter().enumerate() {
            let si = resolve_slot(self, sheet_name);
            let au = self.intern_author(author.clone());
            if au != committer_idx {
                *per_author_counts.entry(au).or_insert(0) += 1;
                per_author_cells.entry(au).or_default().push((
                    sheet_name.clone(),
                    *row,
                    *col,
                    before.clone(),
                    after.clone(),
                ));
            }
            blame_map.insert((si, *row, *col), (au, seq, ts_ms));
            if idx < MAX_COMMIT_CELLS {
                kept_cells.push(AuditCellChange {
                    sheet_idx: si,
                    row: *row,
                    col: *col,
                    before: before.clone(),
                    after: after.clone(),
                    author_idx: au,
                });
            } else if let Some(h) = overflow_hasher.as_mut() {
                h.update(si.to_le_bytes());
                h.update(row.to_le_bytes());
                h.update(col.to_le_bytes());
                before.write_canonical(h);
                after.write_canonical(h);
                h.update(au.to_le_bytes());
            }
        }

        let overflow_digest = overflow_hasher
            .map(|h| hex_encode(&h.finalize()))
            .unwrap_or_default();

        let mut bulk_ops = Vec::with_capacity(delta.bulk_ops.len() + delta.structural_ops.len() + 1);
        for (sheet_name, author, op) in &delta.structural_ops {
            let si = resolve_slot(self, sheet_name);
            let au = self.intern_author(author.clone());
            if au != committer_idx {
                per_author_counts.entry(au).or_insert(0);
                per_author_has_bulk.insert(au);
            }
            bulk_ops.push(AuditBulkOp {
                author_idx: au,
                sheet_idx: si,
                kind: "结构变更".into(),
                summary: op.label(),
                range_digest: String::new(),
            });
        }
        for (sheet_name, author, kind, summary, range_digest) in &delta.bulk_ops {
            let si = resolve_slot(self, sheet_name);
            let au = self.intern_author(author.clone());
            if au != committer_idx {
                per_author_counts.entry(au).or_insert(0);
                per_author_has_bulk.insert(au);
            }
            bulk_ops.push(AuditBulkOp {
                author_idx: au,
                sheet_idx: si,
                kind: kind.clone(),
                summary: summary.clone(),
                range_digest: range_digest.clone(),
            });
        }
        if delta.cells.len() > MAX_COMMIT_CELLS {
            let si = kept_cells.first().map(|c| c.sheet_idx).unwrap_or(0);
            bulk_ops.push(AuditBulkOp {
                author_idx: committer_idx,
                sheet_idx: si,
                kind: "批量截断摘要".into(),
                summary: format!(
                    "本次共变更 {} 格（已精确记录前 {} 格，其余 {} 格已以 SHA-256 锚定防篡改）",
                    delta.cells.len(),
                    MAX_COMMIT_CELLS,
                    delta.cells.len() - MAX_COMMIT_CELLS
                ),
                range_digest: overflow_digest.clone(),
            });
        }

        // 3. 严格核对并组装协作联署记录：
        // 仅当远端联署摘要与该作者最终落盘的单元格集合完全一致且 ECDSA 验签通过时，才标记 `attested_by_peer = true`；
        // 若期间有他人覆写导致最终集合变化，则如实降级为宿主代签并写入真实集合摘要，避免“已联署”与最终修改集失配。
        let mut attest_by_pubkey: HashMap<String, &PeerAttestationInput> = HashMap::new();
        for inp in co_signers_in {
            if !inp.author.pubkey_hex.is_empty() {
                attest_by_pubkey.insert(inp.author.pubkey_hex.to_ascii_lowercase(), inp);
            }
        }
        let mut co_signers = Vec::new();
        for (au, cnt) in per_author_counts {
            if au == committer_idx {
                continue;
            }
            if cnt == 0 && !per_author_has_bulk.contains(&au) {
                continue;
            }
            let expected_digest_bytes = per_author_cells
                .get(&au)
                .filter(|c| !c.is_empty())
                .map(|c| author_contribution_digest(&self.doc_id, seq.saturating_sub(1), c))
                .unwrap_or([0u8; 32]);
            let expected_digest_hex = hex_encode(&expected_digest_bytes);
            let author_pk = self
                .author_of(au)
                .map(|a| a.pubkey_hex.to_ascii_lowercase())
                .unwrap_or_default();
            let matched_attest = if cnt > 0 && !author_pk.is_empty() {
                attest_by_pubkey.get(&author_pk).copied().filter(|inp| {
                    inp.attested_by_peer
                        && inp.changes_digest.eq_ignore_ascii_case(&expected_digest_hex)
                        && verify_digest_hex(&author_pk, &expected_digest_bytes, &inp.sig_hex)
                })
            } else {
                None
            };
            if let Some(inp) = matched_attest {
                co_signers.push(CoSignerAttestation {
                    author_idx: au,
                    change_count: cnt,
                    changes_digest: expected_digest_hex,
                    sig_hex: inp.sig_hex.clone(),
                    attested_by_peer: true,
                });
            } else {
                co_signers.push(CoSignerAttestation {
                    author_idx: au,
                    change_count: cnt,
                    changes_digest: expected_digest_hex,
                    sig_hex: String::new(),
                    attested_by_peer: false,
                });
            }
        }
        co_signers.sort_by_key(|c| c.author_idx);

        let mut commit = AuditCommit {
            seq,
            ts_ms,
            prev_hash,
            state_hash,
            external_gap_hash,
            committer_idx,
            action: action.to_string(),
            cells: kept_cells,
            total_cell_changes,
            overflow_digest,
            bulk_ops,
            co_signers,
            commit_hash: String::new(),
            sig_hex: String::new(),
        };
        let digest = commit.compute_hash(&self.doc_id);
        commit.commit_hash = hex_encode(&digest);
        commit.sig_hex = keypair.sign_digest(&digest)?;
        self.commits.push(commit);

        // 4. 重建有序 Blame；若超过 MAX_BLAME_ENTRIES，优先淘汰最老的 (seq, ts_ms)（例如 #1 创世基线），确保最新真实修改不丢失
        let mut next_blame: Vec<BlameRecord> = blame_map
            .into_iter()
            .map(|((sheet_idx, row, col), (author_idx, seq, ts_ms))| BlameRecord {
                sheet_idx,
                row,
                col,
                author_idx,
                seq,
                ts_ms,
            })
            .collect();
        if next_blame.len() > MAX_BLAME_ENTRIES {
            let excess = next_blame.len() - MAX_BLAME_ENTRIES;
            let mut by_age: Vec<usize> = (0..next_blame.len()).collect();
            by_age.sort_unstable_by_key(|&i| (next_blame[i].seq, next_blame[i].ts_ms));
            let drop_set: HashSet<usize> = by_age.into_iter().take(excess).collect();
            let mut kept = Vec::with_capacity(MAX_BLAME_ENTRIES);
            for (i, rec) in next_blame.into_iter().enumerate() {
                if !drop_set.contains(&i) {
                    kept.push(rec);
                }
            }
            next_blame = kept;
        }
        self.blame = next_blame;

        // 若超限，自动执行滚动折叠（保留链条连续性，同时产生可导出冷归档包）
        self.compact_if_needed(committer_idx, keypair, ts_ms)?;

        Ok(self.commits.last().expect("commit just pushed"))
    }

    /// 检查提交数与压缩后字节体积是否超过硬上限；超出则将最老的一批提交折叠为带签名与 Merkle 根的 `AuditRollup`
    pub fn compact_if_needed(
        &mut self,
        compacted_by_idx: u16,
        keypair: &AuditKeyPair,
        ts_ms: u64,
    ) -> Result<bool> {
        let mut compacted_any = false;
        let max_bytes = self.max_embedded_bytes.max(1024) as usize;
        let max_commits = self.max_detailed_commits.max(4) as usize;

        loop {
            let over_count = self.commits.len() > max_commits;
            let cur_bytes = self.encode_compact_bytes()?.len();
            let over_bytes = cur_bytes > max_bytes;
            if !over_count && !over_bytes {
                break;
            }
            // 始终至少保留最新的 2 条详细提交（确保最近状态与增量审计可见）
            if self.commits.len() <= 2 {
                // 若 Blame 自身导致超限，按最老 ts_ms 裁剪一部分 Blame
                if over_bytes && self.blame.len() > 256 {
                    let target = self.blame.len() / 2;
                    let mut by_age: Vec<usize> = (0..self.blame.len()).collect();
                    by_age.sort_by_key(|&i| (self.blame[i].seq, self.blame[i].ts_ms));
                    let drop_set: HashSet<usize> = by_age.into_iter().take(self.blame.len() - target).collect();
                    let mut kept = Vec::with_capacity(target);
                    for (i, rec) in std::mem::take(&mut self.blame).into_iter().enumerate() {
                        if !drop_set.contains(&i) {
                            kept.push(rec);
                        }
                    }
                    self.blame = kept;
                    compacted_any = true;
                    continue;
                }
                break;
            }

            // 每次折叠最老的一半提交（或至少超出部分 + 20%）
            let fold_count = if over_count {
                (self.commits.len() - max_commits + (max_commits / 5).max(1))
                    .min(self.commits.len() - 2)
            } else {
                (self.commits.len() / 2).max(1).min(self.commits.len() - 2)
            };

            let folded: Vec<AuditCommit> = self.commits.drain(0..fold_count).collect();
            let first = folded.first().unwrap();
            let last = folded.last().unwrap();

            let mut counts: BTreeMap<u16, u32> = BTreeMap::new();
            let mut total_cells = 0u64;
            let mut commit_hashes: Vec<&str> = Vec::with_capacity(folded.len());
            for c in &folded {
                *counts.entry(c.committer_idx).or_insert(0) += 1;
                for co in &c.co_signers {
                    *counts.entry(co.author_idx).or_insert(0) += 1;
                }
                total_cells = total_cells.saturating_add(c.total_cell_changes as u64);
                commit_hashes.push(&c.commit_hash);
            }
            let merkle = compute_merkle_root(&commit_hashes);
            let mut rollup = AuditRollup {
                from_seq: first.seq,
                to_seq: last.seq,
                from_ts_ms: first.ts_ms,
                to_ts_ms: last.ts_ms,
                prev_anchor_hash: first.prev_hash.clone(),
                anchor_hash: last.commit_hash.clone(),
                commit_merkle_root: merkle,
                author_counts: counts.into_iter().collect(),
                total_cell_changes: total_cells,
                compacted_by_idx,
                compacted_ts_ms: ts_ms,
                rollup_hash: String::new(),
                sig_hex: String::new(),
            };
            let digest = rollup.compute_hash(&self.doc_id);
            rollup.rollup_hash = hex_encode(&digest);
            rollup.sig_hex = keypair.sign_digest(&digest)?;

            self.last_compacted = Some(AuditColdArchive {
                version: AUDIT_FORMAT_VERSION,
                doc_id: self.doc_id.clone(),
                author_dict: self.author_dict.clone(),
                sheet_dict: self.sheet_dict.clone(),
                rollup: rollup.clone(),
                commits: folded,
            });
            self.rollups.push(rollup);
            compacted_any = true;
        }

        Ok(compacted_any)
    }

    /// 按持久槽位 O(log N) 查询某格最后修改人；若超出 Blame 索引上限被裁剪，则自动回退检索近期详细提交窗口
    pub fn lookup_blame_by_slot(&self, si: u16, row: u32, col: u32) -> Option<BlameView> {
        if let Ok(idx) = self
            .blame
            .binary_search_by_key(&(si, row, col), |b| (b.sheet_idx, b.row, b.col))
        {
            let rec = self.blame[idx];
            if let Some(author) = self.author_of(rec.author_idx) {
                return Some(BlameView {
                    author: author.clone(),
                    seq: rec.seq,
                    ts_ms: rec.ts_ms,
                    is_baseline: rec.seq == 1,
                });
            }
        }
        // 回退补查近期详细提交窗口（防止超过 10 万格 Blame 上限时近期修改查不到作者）
        for c in self.commits.iter().rev() {
            for ch in &c.cells {
                if ch.sheet_idx == si && ch.row == row && ch.col == col {
                    if let Some(author) = self.author_of(ch.author_idx) {
                        return Some(BlameView {
                            author: author.clone(),
                            seq: c.seq,
                            ts_ms: c.ts_ms,
                            is_baseline: c.seq == 1,
                        });
                    }
                }
            }
        }
        None
    }

    /// O(log N) 查询某格最后修改人（优先按 `live_sheet_slots` 解析当前工作表槽位，不与已改名/已删除旧表串号）
    pub fn lookup_blame(&self, sheet_name: &str, row: u32, col: u32) -> Option<BlameView> {
        let si = self.resolve_sheet_slot_by_name(sheet_name)?;
        self.lookup_blame_by_slot(si, row, col)
    }

    /// 在活动工作簿上下文中查询某格最后修改人（即使工作表刚改名尚未保存也能精准命中）
    pub fn lookup_blame_for_sheet(
        &self,
        wb: &Workbook,
        sheet: &Sheet,
        row: u32,
        col: u32,
    ) -> Option<BlameView> {
        let si = self.resolve_sheet_slot_in_wb(wb, sheet)?;
        self.lookup_blame_by_slot(si, row, col)
    }

    /// 按持久槽位查询当前内嵌明细窗口中所有涉及 `(si, row, col)` 的修改历史（新在前）
    pub fn cell_history_by_slot(&self, si: u16, row: u32, col: u32) -> Vec<CellHistoryHit> {
        let mut out = Vec::new();
        for c in self.commits.iter().rev() {
            for ch in &c.cells {
                if ch.sheet_idx == si && ch.row == row && ch.col == col {
                    if let Some(author) = self.author_of(ch.author_idx) {
                        out.push(CellHistoryHit {
                            seq: c.seq,
                            ts_ms: c.ts_ms,
                            action: c.action.clone(),
                            author: author.clone(),
                            before: ch.before.clone(),
                            after: ch.after.clone(),
                        });
                    }
                }
            }
        }
        out
    }

    /// 查询当前内嵌明细窗口中所有涉及 `(sheet_name, row, col)` 的修改历史（新在前）
    pub fn cell_history(&self, sheet_name: &str, row: u32, col: u32) -> Vec<CellHistoryHit> {
        let Some(si) = self.resolve_sheet_slot_by_name(sheet_name) else {
            return Vec::new();
        };
        self.cell_history_by_slot(si, row, col)
    }

    /// 在活动工作簿上下文中查询某格修改历史（跨重命名/新建同名表不串号）
    pub fn cell_history_for_sheet(
        &self,
        wb: &Workbook,
        sheet: &Sheet,
        row: u32,
        col: u32,
    ) -> Vec<CellHistoryHit> {
        let Some(si) = self.resolve_sheet_slot_in_wb(wb, sheet) else {
            return Vec::new();
        };
        self.cell_history_by_slot(si, row, col)
    }

    /// 全量密码学校验：哈希链 + ECDSA P-256 签名 + Rollup 锚点 + 协作联署 + 时钟顺序 + 工作簿业务状态根
    pub fn verify_against_workbook(&self, wb: &Workbook) -> AuditVerification {
        let current_state_hash = workbook_state_hash(wb);
        let embedded_bytes = self.encode_compact_bytes().map(|b| b.len()).unwrap_or(0);
        let mut res = AuditVerification {
            severity: AuditSeverity::Verified,
            doc_id: Some(self.doc_id.clone()),
            head_seq: self.head_seq(),
            head_hash: self.head_hash().to_string(),
            current_state_hash: current_state_hash.clone(),
            expected_state_hash: self.head_state_hash().map(|s| s.to_string()),
            embedded_bytes,
            detailed_commit_count: self.commits.len(),
            rolled_up_commit_count: self.rolled_up_count(),
            blame_count: self.blame.len(),
            issues: Vec::new(),
        };

        let mut expected_prev_hash = GENESIS_HASH.to_string();
        let mut expected_next_seq = 1u64;
        let mut prev_ts = 0u64;

        // 1. 校验全部 Rollup 归档锚点
        for r in &self.rollups {
            if r.from_seq != expected_next_seq || r.to_seq < r.from_seq {
                res.push_issue(AuditIssue {
                    severity: AuditSeverity::Tampered,
                    kind: AuditIssueKind::BrokenHashChain,
                    seq: Some(r.from_seq),
                    message: format!(
                        "归档节点序号不连续：期望 #{expected_next_seq}，实际 #{}..#{}",
                        r.from_seq, r.to_seq
                    ),
                });
            }
            if r.prev_anchor_hash != expected_prev_hash {
                res.push_issue(AuditIssue {
                    severity: AuditSeverity::Tampered,
                    kind: AuditIssueKind::BrokenHashChain,
                    seq: Some(r.from_seq),
                    message: format!("归档节点 #{}..#{} 前驱哈希不匹配（疑似被删改）", r.from_seq, r.to_seq),
                });
            }
            let digest = r.compute_hash(&self.doc_id);
            let hex_d = hex_encode(&digest);
            if hex_d != r.rollup_hash {
                res.push_issue(AuditIssue {
                    severity: AuditSeverity::Tampered,
                    kind: AuditIssueKind::BrokenHashChain,
                    seq: Some(r.to_seq),
                    message: format!("归档节点 #{}..#{} 摘要被篡改", r.from_seq, r.to_seq),
                });
            } else {
                let ok = self
                    .author_of(r.compacted_by_idx)
                    .is_some_and(|a| verify_digest_hex(&a.pubkey_hex, &digest, &r.sig_hex));
                if !ok {
                    res.push_issue(AuditIssue {
                        severity: AuditSeverity::Tampered,
                        kind: AuditIssueKind::InvalidRollupSignature,
                        seq: Some(r.to_seq),
                        message: format!("归档节点 #{}..#{} ECDSA 数字签名验证失败", r.from_seq, r.to_seq),
                    });
                }
            }
            expected_prev_hash = r.anchor_hash.clone();
            expected_next_seq = r.to_seq.saturating_add(1);
            prev_ts = r.to_ts_ms;
        }

        // 2. 校验每条详细提交
        if self.commits.is_empty() && self.rollups.is_empty() {
            res.push_issue(AuditIssue {
                severity: AuditSeverity::Tampered,
                kind: AuditIssueKind::BrokenHashChain,
                seq: None,
                message: "审计日志为空，缺少创世提交".into(),
            });
        }
        for c in &self.commits {
            if c.seq != expected_next_seq {
                res.push_issue(AuditIssue {
                    severity: AuditSeverity::Tampered,
                    kind: AuditIssueKind::BrokenHashChain,
                    seq: Some(c.seq),
                    message: format!("提交序号断裂：期望 #{expected_next_seq}，实际 #{}", c.seq),
                });
            }
            if c.prev_hash != expected_prev_hash {
                res.push_issue(AuditIssue {
                    severity: AuditSeverity::Tampered,
                    kind: AuditIssueKind::BrokenHashChain,
                    seq: Some(c.seq),
                    message: format!("提交 #{} 的前驱哈希链断裂（历史记录可能被删除或篡改）", c.seq),
                });
            }
            let digest = c.compute_hash(&self.doc_id);
            let hex_d = hex_encode(&digest);
            if hex_d != c.commit_hash {
                res.push_issue(AuditIssue {
                    severity: AuditSeverity::Tampered,
                    kind: AuditIssueKind::BrokenHashChain,
                    seq: Some(c.seq),
                    message: format!("提交 #{} 内容哈希校验失败（记录字段已被擅改）", c.seq),
                });
            } else {
                let sig_ok = self
                    .author_of(c.committer_idx)
                    .is_some_and(|a| verify_digest_hex(&a.pubkey_hex, &digest, &c.sig_hex));
                if !sig_ok {
                    res.push_issue(AuditIssue {
                        severity: AuditSeverity::Tampered,
                        kind: AuditIssueKind::InvalidCommitSignature,
                        seq: Some(c.seq),
                        message: format!("提交 #{} 的作者 ECDSA 数字签名无效", c.seq),
                    });
                }
            }

            // 校验联署签名
            for co in &c.co_signers {
                if co.attested_by_peer {
                    let Some(digest_32) = hex_decode_32(&co.changes_digest) else {
                        res.push_issue(AuditIssue {
                            severity: AuditSeverity::Tampered,
                            kind: AuditIssueKind::InvalidCoSignerSignature,
                            seq: Some(c.seq),
                            message: format!("提交 #{} 的协作联署摘要格式损坏", c.seq),
                        });
                        continue;
                    };
                    let ok = self
                        .author_of(co.author_idx)
                        .is_some_and(|a| verify_digest_hex(&a.pubkey_hex, &digest_32, &co.sig_hex));
                    if !ok {
                        let name = self
                            .author_of(co.author_idx)
                            .map(|a| a.short_label())
                            .unwrap_or_else(|| "未知成员".into());
                        res.push_issue(AuditIssue {
                            severity: AuditSeverity::Tampered,
                            kind: AuditIssueKind::InvalidCoSignerSignature,
                            seq: Some(c.seq),
                            message: format!("提交 #{} 中协作者「{name}」的联署签名校验失败", c.seq),
                        });
                    }
                } else {
                    let name = self
                        .author_of(co.author_idx)
                        .map(|a| a.short_label())
                        .unwrap_or_else(|| "未知成员".into());
                    res.push_issue(AuditIssue {
                        severity: AuditSeverity::Warning,
                        kind: AuditIssueKind::UnattestedCollabEdits,
                        seq: Some(c.seq),
                        message: format!("提交 #{} 中「{name}」的改动由保存宿主代签（无成员独立联署）", c.seq),
                    });
                }
            }

            // 历史外部修改缺口提醒
            if c.external_gap_hash.is_some() {
                res.push_issue(AuditIssue {
                    severity: AuditSeverity::Warning,
                    kind: AuditIssueKind::HistoricalExternalGap,
                    seq: Some(c.seq),
                    message: format!("提交 #{} 之前曾检测到绕过审计的外部软件修改（已永久留痕）", c.seq),
                });
            }

            // 时钟倒退检测
            if prev_ts > 0 && c.ts_ms.saturating_add(CLOCK_SKEW_TOLERANCE_MS) < prev_ts {
                res.push_issue(AuditIssue {
                    severity: AuditSeverity::Warning,
                    kind: AuditIssueKind::ClockRollback,
                    seq: Some(c.seq),
                    message: format!("提交 #{} 本机时间戳早于前一条记录（本机时钟可能异常或被回调）", c.seq),
                });
            }

            expected_prev_hash = c.commit_hash.clone();
            expected_next_seq = c.seq.saturating_add(1);
            prev_ts = c.ts_ms;
        }

        // 3. 核对当前工作簿正文状态哈希与最新提交是否一致（抓出 Excel/WPS 静默改数！）
        if let Some(expected_st) = self.head_state_hash() {
            if expected_st != current_state_hash {
                let last_who = self
                    .head()
                    .and_then(|c| self.author_of(c.committer_idx))
                    .map(|a| a.short_label())
                    .unwrap_or_else(|| "未知".into());
                res.push_issue(AuditIssue {
                    severity: AuditSeverity::Tampered,
                    kind: AuditIssueKind::ExternalModification,
                    seq: Some(self.head_seq()),
                    message: format!(
                        "检测到绕过审计的外部修改：当前表格数据与最新合规提交 #{}（{last_who}）不一致（可能曾用 Excel/WPS 修改或被直接篡改）",
                        self.head_seq()
                    ),
                });
            }
        }

        res
    }

    /// 验证一份外部导入的 `.cellan-audit` 冷归档包是否与当前文件中的 `AuditRollup` 锚点严丝合缝匹配
    pub fn verify_cold_archive(&self, archive: &AuditColdArchive) -> ColdArchiveVerifyResult {
        if archive.doc_id != self.doc_id {
            return ColdArchiveVerifyResult {
                valid: false,
                matched_rollup_idx: None,
                commit_count: archive.commits.len(),
                summary: format!(
                    "归档包文档标识不匹配（归档={}，当前={}）",
                    &archive.doc_id[..archive.doc_id.len().min(8)],
                    &self.doc_id[..self.doc_id.len().min(8)]
                ),
            };
        }
        let Some(rollup_idx) = self.rollups.iter().position(|r| {
            r.from_seq == archive.rollup.from_seq
                && r.to_seq == archive.rollup.to_seq
                && r.anchor_hash == archive.rollup.anchor_hash
                && r.commit_merkle_root == archive.rollup.commit_merkle_root
        }) else {
            return ColdArchiveVerifyResult {
                valid: false,
                matched_rollup_idx: None,
                commit_count: archive.commits.len(),
                summary: "当前文件中不存在与该归档包匹配的折叠锚点（可能已被伪造或不属于本阶段）".into(),
            };
        };

        let target = &self.rollups[rollup_idx];
        let mut expected_prev = target.prev_anchor_hash.clone();
        let mut expected_seq = target.from_seq;
        let mut hashes: Vec<&str> = Vec::with_capacity(archive.commits.len());

        for c in &archive.commits {
            if c.seq != expected_seq || c.prev_hash != expected_prev {
                return ColdArchiveVerifyResult {
                    valid: false,
                    matched_rollup_idx: Some(rollup_idx),
                    commit_count: archive.commits.len(),
                    summary: format!("归档包内提交 #{} 哈希链断裂", c.seq),
                };
            }
            let digest = c.compute_hash(&archive.doc_id);
            if hex_encode(&digest) != c.commit_hash {
                return ColdArchiveVerifyResult {
                    valid: false,
                    matched_rollup_idx: Some(rollup_idx),
                    commit_count: archive.commits.len(),
                    summary: format!("归档包内提交 #{} 内容哈希被篡改", c.seq),
                };
            }
            let sig_ok = archive
                .author_dict
                .get(c.committer_idx as usize)
                .is_some_and(|a| verify_digest_hex(&a.pubkey_hex, &digest, &c.sig_hex));
            if !sig_ok {
                return ColdArchiveVerifyResult {
                    valid: false,
                    matched_rollup_idx: Some(rollup_idx),
                    commit_count: archive.commits.len(),
                    summary: format!("归档包内提交 #{} ECDSA 签名验证失败", c.seq),
                };
            }
            hashes.push(&c.commit_hash);
            expected_prev = c.commit_hash.clone();
            expected_seq = expected_seq.saturating_add(1);
        }

        if expected_seq != target.to_seq.saturating_add(1)
            || expected_prev != target.anchor_hash
            || compute_merkle_root(&hashes) != target.commit_merkle_root
        {
            return ColdArchiveVerifyResult {
                valid: false,
                matched_rollup_idx: Some(rollup_idx),
                commit_count: archive.commits.len(),
                summary: "归档包 Merkle 根或尾锚点哈希与文件内嵌记录不一致（已篡改）".into(),
            };
        }

        ColdArchiveVerifyResult {
            valid: true,
            matched_rollup_idx: Some(rollup_idx),
            commit_count: archive.commits.len(),
            summary: format!(
                "验真通过：完整还原历史提交 #{}..#{}（共 {} 条，Merkle 根与全部 ECDSA 签名匹配）",
                target.from_seq,
                target.to_seq,
                archive.commits.len()
            ),
        }
    }

    /// 导出可读 CSV 审计报告（含 UTF-8 BOM，Excel 双击打开不乱码）
    pub fn export_csv(&self) -> String {
        let mut out = String::from("\u{FEFF}序号,时间(UTC),提交者,Windows账户,设备,公钥指纹,动作,工作表,位置/摘要,原值,新值,联署状态,提交哈希\r\n");
        let csv_esc = |s: &str| {
            if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        };
        for r in &self.rollups {
            let who = self.author_of(r.compacted_by_idx);
            let line = [
                format!("#{}..#{}", r.from_seq, r.to_seq),
                format_utc_ms(r.to_ts_ms),
                who.map(|a| a.display_name.as_str()).unwrap_or("").to_string(),
                who.map(|a| a.os_user.as_str()).unwrap_or("").to_string(),
                who.map(|a| a.machine.as_str()).unwrap_or("").to_string(),
                who.map(|a| a.fingerprint()).unwrap_or_default(),
                "历史归档折叠".into(),
                "(归档锚点)".into(),
                format!("共折叠 {} 条提交 / {} 格变更", r.to_seq - r.from_seq + 1, r.total_cell_changes),
                String::new(),
                String::new(),
                "锚点已签".into(),
                r.anchor_hash.clone(),
            ];
            out.push_str(&line.iter().map(|s| csv_esc(s)).collect::<Vec<_>>().join(","));
            out.push_str("\r\n");
        }
        for c in &self.commits {
            let committer = self.author_of(c.committer_idx);
            let co_note = if c.co_signers.is_empty() {
                "单人已签".to_string()
            } else {
                let peer_ok = c.co_signers.iter().filter(|x| x.attested_by_peer).count();
                format!("多人协作（{peer_ok}/{} 独立联署）", c.co_signers.len())
            };
            if c.cells.is_empty() && c.bulk_ops.is_empty() {
                let line = [
                    format!("#{}", c.seq),
                    format_utc_ms(c.ts_ms),
                    committer.map(|a| a.display_name.as_str()).unwrap_or("").to_string(),
                    committer.map(|a| a.os_user.as_str()).unwrap_or("").to_string(),
                    committer.map(|a| a.machine.as_str()).unwrap_or("").to_string(),
                    committer.map(|a| a.fingerprint()).unwrap_or_default(),
                    c.action.clone(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    co_note.clone(),
                    c.commit_hash.clone(),
                ];
                out.push_str(&line.iter().map(|s| csv_esc(s)).collect::<Vec<_>>().join(","));
                out.push_str("\r\n");
            }
            for ch in &c.cells {
                let who = self.author_of(ch.author_idx).or(committer);
                let line = [
                    format!("#{}", c.seq),
                    format_utc_ms(c.ts_ms),
                    who.map(|a| a.display_name.as_str()).unwrap_or("").to_string(),
                    who.map(|a| a.os_user.as_str()).unwrap_or("").to_string(),
                    who.map(|a| a.machine.as_str()).unwrap_or("").to_string(),
                    who.map(|a| a.fingerprint()).unwrap_or_default(),
                    c.action.clone(),
                    self.sheet_of(ch.sheet_idx).to_string(),
                    crate::model::cell_name(ch.row, ch.col),
                    ch.before.summary(),
                    ch.after.summary(),
                    co_note.clone(),
                    c.commit_hash.clone(),
                ];
                out.push_str(&line.iter().map(|s| csv_esc(s)).collect::<Vec<_>>().join(","));
                out.push_str("\r\n");
            }
            for bo in &c.bulk_ops {
                let who = self.author_of(bo.author_idx).or(committer);
                let line = [
                    format!("#{}", c.seq),
                    format_utc_ms(c.ts_ms),
                    who.map(|a| a.display_name.as_str()).unwrap_or("").to_string(),
                    who.map(|a| a.os_user.as_str()).unwrap_or("").to_string(),
                    who.map(|a| a.machine.as_str()).unwrap_or("").to_string(),
                    who.map(|a| a.fingerprint()).unwrap_or_default(),
                    bo.kind.clone(),
                    self.sheet_of(bo.sheet_idx).to_string(),
                    bo.summary.clone(),
                    String::new(),
                    String::new(),
                    co_note.clone(),
                    c.commit_hash.clone(),
                ];
                out.push_str(&line.iter().map(|s| csv_esc(s)).collect::<Vec<_>>().join(","));
                out.push_str("\r\n");
            }
        }
        out
    }

    // -----------------------------------------------------------------------
    // 紧凑二进制 + DEFLATE 编解码
    // -----------------------------------------------------------------------

    pub fn encode_compact_bytes(&self) -> Result<Vec<u8>> {
        let header = AuditLogHeader {
            version: self.version,
            doc_id: self.doc_id.clone(),
            created_ts_ms: self.created_ts_ms,
            created_by_idx: self.created_by_idx,
            max_embedded_bytes: self.max_embedded_bytes,
            max_detailed_commits: self.max_detailed_commits,
            author_dict: self.author_dict.clone(),
            sheet_dict: self.sheet_dict.clone(),
            live_sheet_slots: self.live_sheet_slots.clone(),
            rollups: self.rollups.clone(),
            commits: self.commits.clone(),
        };
        let json = serde_json::to_vec(&header).context("序列化审计日志头失败")?;
        let blame_raw = encode_blame_varint(&self.blame);

        let mut plain = Vec::with_capacity(8 + json.len() + blame_raw.len());
        plain.extend_from_slice(&(json.len() as u32).to_le_bytes());
        plain.extend_from_slice(&json);
        plain.extend_from_slice(&(blame_raw.len() as u32).to_le_bytes());
        plain.extend_from_slice(&blame_raw);

        let mut out = Vec::with_capacity(PAYLOAD_MAGIC.len() + plain.len() / 2);
        out.extend_from_slice(PAYLOAD_MAGIC);
        let mut enc = DeflateEncoder::new(out, Compression::default());
        enc.write_all(&plain).context("压缩审计载荷失败")?;
        enc.finish().context("完成审计载荷压缩失败")
    }

    pub fn decode_compact_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < PAYLOAD_MAGIC.len() || &bytes[..PAYLOAD_MAGIC.len()] != PAYLOAD_MAGIC {
            anyhow::bail!("审计载荷魔数不匹配或已损坏");
        }
        let dec = DeflateDecoder::new(&bytes[PAYLOAD_MAGIC.len()..]);
        let mut plain = Vec::new();
        // 解压安全上限（16 MB 防 zip bomb）
        dec.take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut plain)
            .context("解压审计载荷失败")?;
        anyhow::ensure!(plain.len() <= 16 * 1024 * 1024, "审计载荷解压超限");
        anyhow::ensure!(plain.len() >= 8, "审计载荷被截断");

        let json_len = u32::from_le_bytes(plain[0..4].try_into().unwrap()) as usize;
        anyhow::ensure!(plain.len() >= 4 + json_len + 4, "审计头长度越界");
        let header: AuditLogHeader =
            serde_json::from_slice(&plain[4..4 + json_len]).context("解析审计日志头失败")?;
        let blame_off = 4 + json_len;
        let blame_len = u32::from_le_bytes(plain[blame_off..blame_off + 4].try_into().unwrap()) as usize;
        anyhow::ensure!(plain.len() >= blame_off + 4 + blame_len, "审计溯源段长度越界");
        let blame = decode_blame_varint(&plain[blame_off + 4..blame_off + 4 + blame_len])?;

        Ok(Self {
            version: header.version,
            doc_id: header.doc_id,
            created_ts_ms: header.created_ts_ms,
            created_by_idx: header.created_by_idx,
            max_embedded_bytes: header.max_embedded_bytes,
            max_detailed_commits: header.max_detailed_commits,
            author_dict: header.author_dict,
            sheet_dict: header.sheet_dict,
            live_sheet_slots: header.live_sheet_slots,
            rollups: header.rollups,
            commits: header.commits,
            blame,
            last_compacted: None,
        })
    }

    pub fn encode_base64(&self) -> Result<String> {
        let raw = self.encode_compact_bytes()?;
        Ok(base64::engine::general_purpose::STANDARD.encode(raw))
    }

    pub fn decode_base64(b64: &str) -> Result<Self> {
        let cleaned: String = b64.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(cleaned.as_bytes())
            .context("解码审计 Base64 载荷失败")?;
        Self::decode_compact_bytes(&raw)
    }
}

/// 独立校验 `WorkbookAudit`（包括处理审计部件被剥离、损坏的异常状态）
pub fn verify_workbook_audit(wb: &Workbook) -> Option<AuditVerification> {
    if let Some(log) = wb.audit.log.as_ref() {
        return Some(log.verify_against_workbook(wb));
    }
    if let Some(marker) = wb.audit.stripped_marker.as_ref() {
        let mut v = AuditVerification {
            severity: AuditSeverity::Tampered,
            doc_id: marker.split('|').nth(1).map(|s| s.to_string()),
            head_seq: 0,
            head_hash: String::new(),
            current_state_hash: workbook_state_hash(wb),
            expected_state_hash: None,
            embedded_bytes: 0,
            detailed_commit_count: 0,
            rolled_up_commit_count: 0,
            blame_count: 0,
            issues: Vec::new(),
        };
        v.push_issue(AuditIssue {
            severity: AuditSeverity::Tampered,
            kind: AuditIssueKind::AuditStripped,
            seq: None,
            message: format!(
                "审计记录被剥离！文档属性标明此文件曾启用防篡改审计（{marker}），但内嵌审计部件已丢失（可能被外部软件清除或人为删除）"
            ),
        });
        return Some(v);
    }
    if let Some(err) = wb.audit.parse_error.as_ref() {
        let mut v = AuditVerification {
            severity: AuditSeverity::Tampered,
            doc_id: None,
            head_seq: 0,
            head_hash: String::new(),
            current_state_hash: workbook_state_hash(wb),
            expected_state_hash: None,
            embedded_bytes: 0,
            detailed_commit_count: 0,
            rolled_up_commit_count: 0,
            blame_count: 0,
            issues: Vec::new(),
        };
        v.push_issue(AuditIssue {
            severity: AuditSeverity::Tampered,
            kind: AuditIssueKind::CorruptPayload,
            seq: None,
            message: format!("内嵌审计数据损坏或遭非法篡改：{err}"),
        });
        return Some(v);
    }
    None
}

// ---------------------------------------------------------------------------
// OOXML 读写与干净副本剥离（支持 Excel 重命名 customXml/itemN.xml 往返）
// ---------------------------------------------------------------------------

/// 从已打开的 OOXML ZIP 中读取内嵌审计日志及剥离检测标记
pub fn read_from_zip<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> WorkbookAudit {
    let mut out = WorkbookAudit::default();
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();

    // 1. 扫描所有 customXml/*.xml（兼容 Excel 重命名为 customXml/item1.xml 等情况）
    for name in &names {
        let lower = name.to_ascii_lowercase();
        if !lower.starts_with("customxml/")
            || !lower.ends_with(".xml")
            || lower.contains("_rels/")
            || lower.contains("props")
        {
            continue;
        }
        let Ok(mut f) = zip.by_name(name) else {
            continue;
        };
        let mut xml = String::new();
        if f.read_to_string(&mut xml).is_err() {
            continue;
        }
        if !xml.contains(AUDIT_XML_NS) || !xml.contains("cellanAudit") {
            continue;
        }
        match extract_payload_from_audit_xml(&xml).and_then(|b64| AuditLog::decode_base64(&b64)) {
            Ok(log) => {
                out.log = Some(Arc::new(log));
                break;
            }
            Err(e) => {
                out.parse_error = Some(format!("{e:#}"));
                break;
            }
        }
    }

    // 2. 检查 docProps/custom.xml 中的轻量防剥离标记 `_sc_audit_meta`
    if out.log.is_none() && out.parse_error.is_none() {
        if let Ok(mut f) = zip.by_name("docProps/custom.xml") {
            let mut xml = String::new();
            if f.read_to_string(&mut xml).is_ok() {
                if let Some(marker) = extract_custom_prop(&xml, AUDIT_CUSTOM_PROP_NAME) {
                    out.stripped_marker = Some(marker);
                }
            }
        }
    }
    out
}

/// 将 `wb.audit` 同步写入已生成的 OOXML 包，或在 `strip_on_save` 时彻底剥离全部审计部件。
pub fn sync_package_audit(path: &Path, wb: &Workbook) -> Result<()> {
    if wb.audit.log.is_none() && !wb.audit.strip_on_save {
        return Ok(());
    }

    crate::patch::begin_stage_for(path);

    // 1. 清理包里所有旧的 cellanAudit 部件（包括 Excel 改名后的 customXml/itemN.xml）
    let old_parts = find_existing_audit_customxml_parts(path);
    for part in &old_parts {
        let _ = crate::patch::remove_entry(path, part);
        if let Some(rels_part) = customxml_rels_path(part) {
            if let Some(rels_bytes) = crate::fixups::read_part(path, &rels_part) {
                if let Ok(rels_xml) = String::from_utf8(rels_bytes) {
                    for prop_target in extract_rel_targets(&rels_xml) {
                        let resolved = resolve_customxml_target(&prop_target);
                        let _ = crate::patch::remove_entry(path, &resolved);
                        remove_content_type_override(path, &format!("/{resolved}"))?;
                    }
                }
            }
            let _ = crate::patch::remove_entry(path, &rels_part);
        }
        remove_workbook_rel_by_target(path, part)?;
        remove_content_type_override(path, &format!("/{part}"))?;
    }

    if wb.audit.strip_on_save || wb.audit.log.is_none() {
        // 干净副本导出：同时移除 docProps/custom.xml 里的 _sc_audit_meta 标记
        upsert_custom_prop(path, AUDIT_CUSTOM_PROP_NAME, None)?;
        crate::patch::commit_stage(path).context("剥离审计记录失败")?;
        return Ok(());
    }

    let log = wb.audit.log.as_ref().unwrap();
    let payload_b64 = log.encode_base64()?;
    let guid = doc_id_to_guid(&log.doc_id);

    // 2. 写出标准 OOXML Custom XML Part + Props + Rels
    let audit_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
         <cellanAudit xmlns=\"{AUDIT_XML_NS}\" version=\"{}\" docId=\"{}\" headSeq=\"{}\" headHash=\"{}\">\
         <payload>{payload_b64}</payload></cellanAudit>",
        log.version,
        crate::model::xml_escape(&log.doc_id),
        log.head_seq(),
        crate::model::xml_escape(log.head_hash()),
    );
    let props_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
         <ds:datastoreItem ds:itemID=\"{guid}\" xmlns:ds=\"http://schemas.openxmlformats.org/officeDocument/2006/customXml\">\
         <ds:schemaRefs><ds:schemaRef ds:uri=\"{AUDIT_XML_NS}\"/></ds:schemaRefs></ds:datastoreItem>"
    );
    let item_rels_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXmlProps\" Target=\"cellanAuditProps.xml\"/>\
        </Relationships>";

    crate::fixups::write_parts(
        path,
        &[
            (AUDIT_CUSTOM_XML_PART.to_string(), audit_xml.into_bytes()),
            (AUDIT_CUSTOM_XML_PROPS_PART.to_string(), props_xml.into_bytes()),
            (AUDIT_CUSTOM_XML_RELS_PART.to_string(), item_rels_xml.as_bytes().to_vec()),
        ],
    )?;

    // 3. 在 xl/_rels/workbook.xml.rels 注册 customXml 关系
    add_workbook_customxml_rel(path, "../customXml/cellanAudit.xml")?;

    // 4. 在 [Content_Types].xml 注册 Override
    ensure_content_type_override(path, "/customXml/cellanAudit.xml", "application/xml")?;
    ensure_content_type_override(
        path,
        "/customXml/cellanAuditProps.xml",
        "application/vnd.openxmlformats-officedocument.customXmlProperties+xml",
    )?;

    // 5. 在 docProps/custom.xml 写入轻量防剥离标记 `_sc_audit_meta`
    let marker = format!("v1|{}|{}", log.doc_id, log.head_seq());
    upsert_custom_prop(path, AUDIT_CUSTOM_PROP_NAME, Some(&marker))?;

    crate::patch::commit_stage(path).context("写入防篡改审计记录失败")?;
    Ok(())
}

/// 导出不含任何审计日志与标记的「干净副本」`.xlsx`（用于对外发给客户/供应商）
pub fn save_clean_copy(path: &Path, wb: &Workbook) -> Result<()> {
    let mut clean = wb.clone();
    clean.audit.log = None;
    clean.audit.stripped_marker = None;
    clean.audit.parse_error = None;
    clean.audit.strip_on_save = true;
    crate::xlsx::save_atomic(path, &clean)
}

// ---------------------------------------------------------------------------
// Delta-Varint 编解码（BlameMap 极致压缩：每格约 2~4 字节）
// ---------------------------------------------------------------------------

fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7F) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

fn read_varint(input: &[u8], pos: &mut usize) -> Result<u64> {
    let mut out = 0u64;
    let mut shift = 0u32;
    loop {
        anyhow::ensure!(*pos < input.len(), "varint 意外截断");
        let b = input[*pos];
        *pos += 1;
        out |= ((b & 0x7F) as u64) << shift;
        if (b & 0x80) == 0 {
            return Ok(out);
        }
        shift += 7;
        anyhow::ensure!(shift < 64, "varint 溢出");
    }
}

fn encode_blame_varint(records: &[BlameRecord]) -> Vec<u8> {
    let mut out = Vec::with_capacity(records.len() * 6 + 4);
    write_varint(&mut out, records.len() as u64);
    let mut prev_sheet = 0u16;
    let mut prev_row = 0u32;
    let mut prev_col = 0u32;
    for r in records {
        let ds = r.sheet_idx.wrapping_sub(prev_sheet) as u64;
        let dr = if r.sheet_idx == prev_sheet {
            r.row.wrapping_sub(prev_row) as u64
        } else {
            r.row as u64
        };
        let dc = if r.sheet_idx == prev_sheet && r.row == prev_row {
            r.col.wrapping_sub(prev_col) as u64
        } else {
            r.col as u64
        };
        write_varint(&mut out, ds);
        write_varint(&mut out, dr);
        write_varint(&mut out, dc);
        write_varint(&mut out, r.author_idx as u64);
        write_varint(&mut out, r.seq);
        write_varint(&mut out, r.ts_ms);
        prev_sheet = r.sheet_idx;
        prev_row = r.row;
        prev_col = r.col;
    }
    out
}

fn decode_blame_varint(input: &[u8]) -> Result<Vec<BlameRecord>> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let mut pos = 0usize;
    let count = read_varint(input, &mut pos)? as usize;
    anyhow::ensure!(count <= MAX_BLAME_ENTRIES * 2, "Blame 条数超限");
    let mut out = Vec::with_capacity(count);
    let mut prev_sheet = 0u16;
    let mut prev_row = 0u32;
    let mut prev_col = 0u32;
    for _ in 0..count {
        let ds = read_varint(input, &mut pos)? as u16;
        let dr = read_varint(input, &mut pos)? as u32;
        let dc = read_varint(input, &mut pos)? as u32;
        let author_idx = read_varint(input, &mut pos)? as u16;
        let seq = read_varint(input, &mut pos)?;
        let ts_ms = read_varint(input, &mut pos)?;
        let sheet_idx = prev_sheet.wrapping_add(ds);
        let row = if sheet_idx == prev_sheet {
            prev_row.wrapping_add(dr)
        } else {
            dr
        };
        let col = if sheet_idx == prev_sheet && row == prev_row {
            prev_col.wrapping_add(dc)
        } else {
            dc
        };
        out.push(BlameRecord { sheet_idx, row, col, author_idx, seq, ts_ms });
        prev_sheet = sheet_idx;
        prev_row = row;
        prev_col = col;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 辅助函数：XML / Merkle / 编码 / 时间格式化
// ---------------------------------------------------------------------------

fn compute_merkle_root(hashes: &[&str]) -> String {
    if hashes.is_empty() {
        return GENESIS_HASH.to_string();
    }
    let mut level: Vec<[u8; 32]> = hashes
        .iter()
        .map(|h| {
            let mut sh = Sha256::new();
            sh.update(b"leaf:");
            sh.update(h.as_bytes());
            sh.finalize().into()
        })
        .collect();
    while level.len() > 1 {
        let mut next = Vec::with_capacity((level.len() + 1) / 2);
        for chunk in level.chunks(2) {
            let left = chunk[0];
            let right = if chunk.len() == 2 { chunk[1] } else { chunk[0] };
            let mut sh = Sha256::new();
            sh.update(b"node:");
            sh.update(left);
            sh.update(right);
            next.push(sh.finalize().into());
        }
        level = next;
    }
    hex_encode(&level[0])
}

fn write_str(h: &mut Sha256, s: &str) {
    h.update((s.len() as u32).to_le_bytes());
    h.update(s.as_bytes());
}

fn canonical_f64_bits(n: f64) -> u64 {
    if n == 0.0 {
        0.0f64.to_bits()
    } else if n.is_nan() {
        u64::MAX
    } else {
        n.to_bits()
    }
}

fn normalize_formula_for_audit(f: &str) -> String {
    let t = f.trim();
    if t.starts_with('=') {
        t.to_string()
    } else {
        format!("={t}")
    }
}

fn normalize_text_for_audit(s: &str) -> String {
    if s.contains('\r') {
        s.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        s.to_string()
    }
}

fn truncate_snap_text(s: &str) -> String {
    if s.len() <= MAX_SNAP_TEXT_LEN {
        return s.to_string();
    }
    let mut end = MAX_SNAP_TEXT_LEN;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let d = Sha256::digest(s.as_bytes());
    format!("{}…[len={},sha={:02x}{:02x}{:02x}{:02x}]", &s[..end], s.len(), d[0], d[1], d[2], d[3])
}

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

fn hex_decode_n<const N: usize>(hex: &str) -> Option<[u8; N]> {
    let bytes = hex.as_bytes();
    if bytes.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    let nibble = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    for i in 0..N {
        let hi = nibble(bytes[i * 2])?;
        let lo = nibble(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_decode_64(hex: &str) -> Option<[u8; 64]> {
    hex_decode_n::<64>(hex)
}

fn hex_decode_32(hex: &str) -> Option<[u8; 32]> {
    hex_decode_n::<32>(hex)
}

fn doc_id_to_guid(doc_id: &str) -> String {
    let hex: String = doc_id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(32)
        .collect::<String>()
        .to_ascii_uppercase();
    let padded = format!("{hex:0<32}");
    format!(
        "{{{}-{}-{}-{}-{}}}",
        &padded[0..8],
        &padded[8..12],
        &padded[12..16],
        &padded[16..20],
        &padded[20..32]
    )
}

fn extract_payload_from_audit_xml(xml: &str) -> Result<String> {
    let start_tag = "<payload>";
    let end_tag = "</payload>";
    let s = xml
        .find(start_tag)
        .context("审计 XML 缺少 <payload> 节点")?
        + start_tag.len();
    let e = xml[s..]
        .find(end_tag)
        .context("审计 XML 缺少 </payload> 闭合标签")?;
    Ok(xml[s..s + e].to_string())
}

fn find_existing_audit_customxml_parts(path: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for name in crate::fixups::entries_of(path) {
        let lower = name.to_ascii_lowercase();
        if !lower.starts_with("customxml/")
            || !lower.ends_with(".xml")
            || lower.contains("_rels/")
            || lower.contains("props")
        {
            continue;
        }
        if let Some(bytes) = crate::fixups::read_part(path, &name) {
            if let Ok(xml) = String::from_utf8(bytes) {
                if xml.contains(AUDIT_XML_NS) && xml.contains("cellanAudit") {
                    found.push(name);
                }
            }
        }
    }
    found
}

fn customxml_rels_path(part: &str) -> Option<String> {
    let file = part.strip_prefix("customXml/").or_else(|| part.strip_prefix("customxml/"))?;
    Some(format!("customXml/_rels/{file}.rels"))
}

fn resolve_customxml_target(target: &str) -> String {
    let t = target.trim_start_matches('/');
    if t.starts_with("customXml/") || t.starts_with("customxml/") {
        t.to_string()
    } else {
        format!("customXml/{}", t.trim_start_matches("./"))
    }
}

fn extract_rel_targets(rels_xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut reader = quick_xml::Reader::from_str(rels_xml);
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Empty(e)) | Ok(quick_xml::events::Event::Start(e))
                if e.local_name().as_ref() == "Relationship" =>
            {
                for a in e.attributes().flatten() {
                    if a.key.as_ref() == "Target" {
                        out.push(a.value.to_string());
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn remove_workbook_rel_by_target(path: &Path, part_name: &str) -> Result<()> {
    let Some(bytes) = crate::fixups::read_part(path, "xl/_rels/workbook.xml.rels") else {
        return Ok(());
    };
    let Ok(xml) = String::from_utf8(bytes) else {
        return Ok(());
    };
    let want_rel = format!("../{part_name}");
    let want_abs = format!("/{part_name}");
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml.as_str();
    let mut changed = false;
    while let Some(pos) = rest.find("<Relationship") {
        out.push_str(&rest[..pos]);
        let end = rest[pos..]
            .find("/>")
            .map(|e| pos + e + 2)
            .unwrap_or(rest.len());
        let tag = &rest[pos..end];
        if tag.contains(&format!("Target=\"{want_rel}\""))
            || tag.contains(&format!("Target=\"{want_abs}\""))
            || tag.contains(&format!("Target=\"{part_name}\""))
        {
            changed = true;
        } else {
            out.push_str(tag);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    if changed {
        crate::patch::append_entry(path, "xl/_rels/workbook.xml.rels", out.as_bytes())?;
    }
    Ok(())
}

fn add_workbook_customxml_rel(path: &Path, target: &str) -> Result<()> {
    let Some(bytes) = crate::fixups::read_part(path, "xl/_rels/workbook.xml.rels") else {
        return Ok(());
    };
    let Ok(xml) = String::from_utf8(bytes) else {
        return Ok(());
    };
    if xml.contains(&format!("Target=\"{target}\"")) {
        return Ok(());
    }
    let max_id = xml
        .match_indices("Id=\"rId")
        .filter_map(|(i, _)| {
            xml[i + 7..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u32>()
                .ok()
        })
        .max()
        .unwrap_or(0);
    let rel = format!(
        "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml\" Target=\"{target}\"/>",
        max_id + 1
    );
    if let Some(pos) = xml.rfind("</Relationships>") {
        let mut updated = String::with_capacity(xml.len() + rel.len());
        updated.push_str(&xml[..pos]);
        updated.push_str(&rel);
        updated.push_str(&xml[pos..]);
        crate::patch::append_entry(path, "xl/_rels/workbook.xml.rels", updated.as_bytes())?;
    }
    Ok(())
}

fn remove_content_type_override(path: &Path, part_name_with_slash: &str) -> Result<()> {
    let Some(bytes) = crate::fixups::read_part(path, "[Content_Types].xml") else {
        return Ok(());
    };
    let Ok(xml) = String::from_utf8(bytes) else {
        return Ok(());
    };
    let pat = format!("PartName=\"{part_name_with_slash}\"");
    if !xml.contains(&pat) {
        return Ok(());
    }
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml.as_str();
    while let Some(pos) = rest.find("<Override") {
        out.push_str(&rest[..pos]);
        let end = rest[pos..]
            .find("/>")
            .map(|e| pos + e + 2)
            .unwrap_or(rest.len());
        let tag = &rest[pos..end];
        if !tag.contains(&pat) {
            out.push_str(tag);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    crate::patch::append_entry(path, "[Content_Types].xml", out.as_bytes())?;
    Ok(())
}

fn ensure_content_type_override(path: &Path, part_name_with_slash: &str, content_type: &str) -> Result<()> {
    let Some(bytes) = crate::fixups::read_part(path, "[Content_Types].xml") else {
        return Ok(());
    };
    let Ok(xml) = String::from_utf8(bytes) else {
        return Ok(());
    };
    if xml.contains(&format!("PartName=\"{part_name_with_slash}\"")) {
        return Ok(());
    }
    if let Some(pos) = xml.rfind("</Types>") {
        let tag = format!(
            "<Override PartName=\"{part_name_with_slash}\" ContentType=\"{content_type}\"/>"
        );
        let mut updated = String::with_capacity(xml.len() + tag.len());
        updated.push_str(&xml[..pos]);
        updated.push_str(&tag);
        updated.push_str(&xml[pos..]);
        crate::patch::append_entry(path, "[Content_Types].xml", updated.as_bytes())?;
    }
    Ok(())
}

fn extract_custom_prop(xml: &str, want_name: &str) -> Option<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut in_target = false;
    let mut in_val = false;
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) => {
                let local = e.local_name();
                if local.as_ref() == "property" {
                    let mut matched = false;
                    for a in e.attributes().flatten() {
                        if a.key.local_name().as_ref() == "name" && a.value.as_ref() == want_name {
                            matched = true;
                        }
                    }
                    in_target = matched;
                } else if in_target && local.as_ref() == "lpwstr" {
                    in_val = true;
                }
            }
            Ok(quick_xml::events::Event::Text(t)) if in_val => {
                text.push_str(t.as_ref());
            }
            Ok(quick_xml::events::Event::End(e)) => {
                let local = e.local_name();
                if local.as_ref() == "lpwstr" && in_val {
                    return Some(crate::model::xml_unescape(&text));
                }
                if local.as_ref() == "property" && in_target {
                    in_target = false;
                    in_val = false;
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    None
}

/// 在 `docProps/custom.xml` 中写入或删除 `_sc_audit_meta`，原样保留第三方属性及 `_sc_src_*` 数据源属性。
fn upsert_custom_prop(path: &Path, prop_name: &str, value: Option<&str>) -> Result<()> {
    const CP_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties";
    const VT_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes";
    const PART: &str = "docProps/custom.xml";

    let existing = crate::fixups::read_part(path, PART).and_then(|b| String::from_utf8(b).ok());
    let Some(existing_xml) = existing else {
        let Some(val) = value else {
            return Ok(());
        };
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
             <Properties xmlns=\"{CP_NS}\" xmlns:vt=\"{VT_NS}\">\
             <property fmtid=\"{{D5CDD505-2E9C-101B-9397-08002B2CF9AE}}\" pid=\"2\" name=\"{}\">\
             <vt:lpwstr>{}</vt:lpwstr></property></Properties>",
            crate::model::xml_escape(prop_name),
            crate::model::xml_escape(val)
        );
        crate::patch::append_entry(path, PART, xml.as_bytes())?;
        // 补齐 _rels/.rels 与 [Content_Types].xml
        if let Some(rels) = crate::fixups::read_part(path, "_rels/.rels").and_then(|b| String::from_utf8(b).ok()) {
            if !rels.contains("custom-properties") {
                let max_id = rels
                    .match_indices("Id=\"rId")
                    .filter_map(|(i, _)| {
                        rels[i + 7..]
                            .chars()
                            .take_while(|c| c.is_ascii_digit())
                            .collect::<String>()
                            .parse::<u32>()
                            .ok()
                    })
                    .max()
                    .unwrap_or(0);
                let rel = format!(
                    "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties\" Target=\"docProps/custom.xml\"/>",
                    max_id + 1
                );
                if let Some(pos) = rels.rfind("</Relationships>") {
                    let updated = format!("{}{}{}", &rels[..pos], rel, &rels[pos..]);
                    crate::patch::append_entry(path, "_rels/.rels", updated.as_bytes())?;
                }
            }
        }
        ensure_content_type_override(
            path,
            "/docProps/custom.xml",
            "application/vnd.openxmlformats-officedocument.custom-properties+xml",
        )?;
        return Ok(());
    };

    // 剥掉已有的同名属性片段并计算最大 pid
    let mut max_pid = 1u32;
    let mut kept_xml = String::with_capacity(existing_xml.len());
    let mut rest = existing_xml.as_str();
    let mut removed_any = false;
    let mut remaining_props = 0usize;
    while let Some(pos) = rest.find("<property").or_else(|| rest.find("<cp:property")) {
        kept_xml.push_str(&rest[..pos]);
        let is_cp = rest[pos..].starts_with("<cp:property");
        let close_tag = if is_cp { "</cp:property>" } else { "</property>" };
        let end = if let Some(cpos) = rest[pos..].find(close_tag) {
            pos + cpos + close_tag.len()
        } else if let Some(epos) = rest[pos..].find("/>") {
            pos + epos + 2
        } else {
            rest.len()
        };
        let frag = &rest[pos..end];
        if let Some(pidx) = frag.find("pid=\"") {
            if let Ok(p) = frag[pidx + 5..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u32>()
            {
                max_pid = max_pid.max(p);
            }
        }
        if frag.contains(&format!("name=\"{prop_name}\"")) || frag.contains(&format!("name='{prop_name}'")) {
            removed_any = true;
        } else {
            remaining_props += 1;
            kept_xml.push_str(frag);
        }
        rest = &rest[end..];
    }
    kept_xml.push_str(rest);

    match value {
        None => {
            if !removed_any {
                return Ok(());
            }
            if remaining_props == 0 {
                let _ = crate::patch::remove_entry(path, PART);
                remove_content_type_override(path, "/docProps/custom.xml")?;
                if let Some(rels) = crate::fixups::read_part(path, "_rels/.rels").and_then(|b| String::from_utf8(b).ok()) {
                    if rels.contains("docProps/custom.xml") {
                        let mut out = String::with_capacity(rels.len());
                        let mut r = rels.as_str();
                        while let Some(pos) = r.find("<Relationship") {
                            out.push_str(&r[..pos]);
                            let end = r[pos..].find("/>").map(|e| pos + e + 2).unwrap_or(r.len());
                            let tag = &r[pos..end];
                            if !tag.contains("docProps/custom.xml") {
                                out.push_str(tag);
                            }
                            r = &r[end..];
                        }
                        out.push_str(r);
                        crate::patch::append_entry(path, "_rels/.rels", out.as_bytes())?;
                    }
                }
            } else {
                crate::patch::append_entry(path, PART, kept_xml.as_bytes())?;
            }
        }
        Some(val) => {
            let pid = max_pid + 1;
            let prop_prefix = if kept_xml.contains("<cp:Properties") { "cp:" } else { "" };
            let prop_frag = format!(
                "<{prop_prefix}property fmtid=\"{{D5CDD505-2E9C-101B-9397-08002B2CF9AE}}\" pid=\"{pid}\" name=\"{}\"><vt:lpwstr>{}</vt:lpwstr></{prop_prefix}property>",
                crate::model::xml_escape(prop_name),
                crate::model::xml_escape(val)
            );
            let close_pos = kept_xml
                .rfind("</cp:Properties>")
                .or_else(|| kept_xml.rfind("</Properties>"));
            if let Some(pos) = close_pos {
                // 确保根元素声明了 xmlns:vt
                if !kept_xml[..pos].contains("xmlns:vt=") {
                    if let Some(root_end) = kept_xml.find("Properties").and_then(|i| kept_xml[i..].find('>').map(|j| i + j)) {
                        kept_xml.insert_str(root_end, &format!(" xmlns:vt=\"{VT_NS}\""));
                    }
                }
                let pos = kept_xml
                    .rfind("</cp:Properties>")
                    .or_else(|| kept_xml.rfind("</Properties>"))
                    .unwrap_or(kept_xml.len());
                kept_xml.insert_str(pos, &prop_frag);
                crate::patch::append_entry(path, PART, kept_xml.as_bytes())?;
            }
        }
    }
    Ok(())
}

/// 格式化 Unix 毫秒时间为 `YYYY-MM-DD HH:MM:SS`（UTC+8 本地办公友好或 UTC）
pub fn format_utc_ms(ts_ms: u64) -> String {
    format_time_with_offset(ts_ms, 0)
}

pub fn format_local_ms(ts_ms: u64) -> String {
    // Windows 办公环境默认东八区；简洁无外部时区库依赖
    format_time_with_offset(ts_ms, 8 * 3600)
}

fn format_time_with_offset(ts_ms: u64, offset_secs: i64) -> String {
    let secs = (ts_ms / 1000) as i64 + offset_secs;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// ---------------------------------------------------------------------------
// 单元测试（覆盖 CNG 签名、DPAPI、净差抵消、哈希链篡改、Excel 改数、滚动归档、冷归档验真、干净导出）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Value, Workbook};

    fn test_author(name: &str, kp: &AuditKeyPair) -> AuditAuthor {
        AuditAuthor {
            display_name: name.into(),
            os_user: name.into(),
            machine: "TEST-PC".into(),
            pubkey_hex: kp.pubkey_hex.clone(),
        }
    }

    #[test]
    fn cng_keypair_sign_verify_and_dpapi_roundtrip() {
        let kp = generate_keypair().expect("生成 CNG ECDSA P-256 密钥对");
        assert_eq!(kp.pubkey_hex.len(), 128);
        let msg = Sha256::digest(b"hello cellan audit");
        let digest: [u8; 32] = msg.into();
        let sig = kp.sign_digest(&digest).expect("签名");
        assert_eq!(sig.len(), 128);
        assert!(verify_digest_hex(&kp.pubkey_hex, &digest, &sig));

        // 篡改 1 字节摘要必须验签失败
        let mut bad_digest = digest;
        bad_digest[0] ^= 0x01;
        assert!(!verify_digest_hex(&kp.pubkey_hex, &bad_digest, &sig));

        // DPAPI 加密与解密私钥块
        let enc = dpapi_protect(&kp.priv_blob, b"cellan-test-entropy").expect("DPAPI 加密");
        let dec = dpapi_unprotect(&enc, b"cellan-test-entropy").expect("DPAPI 解密");
        assert_eq!(dec.as_slice(), &kp.priv_blob);
        let restored = AuditKeyPair::from_priv_blob(dec.try_into().unwrap()).expect("恢复密钥");
        assert_eq!(restored.pubkey_hex, kp.pubkey_hex);
    }

    #[test]
    fn net_diff_cancels_reverted_edits_and_attributes_multi_authors() {
        let kp_a = generate_keypair().unwrap();
        let kp_b = generate_keypair().unwrap();
        let alice = test_author("张三", &kp_a);
        let bob = test_author("李四", &kp_b);

        let mut base = Workbook::new_blank();
        base.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        base.sheets[0].set(0, 1, Cell { value: Value::Text("初始".into()), formula: None });

        let mut cur = base.clone();
        // A1 改成 200 再改回 100 -> 净差抵消
        cur.sheets[0].set(0, 0, Cell { value: Value::Number(200.0), formula: None });
        cur.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        // B1 由李四修改
        cur.sheets[0].set(0, 1, Cell { value: Value::Text("已审核".into()), formula: None });
        // C1 由张三新增公式
        cur.sheets[0].set(0, 2, Cell { value: Value::Number(200.0), formula: Some("=A1*2".into()) });

        let mut attrs = HashMap::new();
        attrs.insert((cur.sheets[0].identity, 0, 1), bob.clone());
        attrs.insert((cur.sheets[0].identity, 0, 2), alice.clone());

        let diff = compute_net_diff(&base, &cur, &alice, &attrs, &[]);
        assert_eq!(diff.cells.len(), 2, "A1 改回原值必须被净差抵消");
        assert_eq!(diff.cells[0].2, 1);
        assert_eq!(diff.cells[0].5.display_name, "李四");
        assert_eq!(diff.cells[1].2, 2);
        assert_eq!(diff.cells[1].5.display_name, "张三");
        assert_eq!(diff.cells[1].4, AuditSnap::Formula("=A1*2".into()));
    }

    #[test]
    fn hash_chain_tamper_and_external_edit_detection() {
        let kp = generate_keypair().unwrap();
        let alice = test_author("张三", &kp);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        let mut log = AuditLog::enable(&wb, alice.clone(), &kp, 1_700_000_000_000).unwrap();

        let base = wb.clone();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(250.0), formula: None });
        let diff = compute_net_diff(&base, &wb, &alice, &HashMap::new(), &[]);
        log.append_commit(&wb, alice.clone(), &kp, "保存修改", &diff, &[], None, 1_700_000_060_000)
            .unwrap();

        // 正常校验应为 Verified
        let ok = log.verify_against_workbook(&wb);
        assert_eq!(ok.severity, AuditSeverity::Verified, "{:?}", ok.issues);

        // 1. 模拟 Excel/WPS 绕过审计悄悄改数 -> 必须报 ExternalModification
        let mut ext_wb = wb.clone();
        ext_wb.sheets[0].set(0, 0, Cell { value: Value::Number(999.0), formula: None });
        let ext_v = log.verify_against_workbook(&ext_wb);
        assert_eq!(ext_v.severity, AuditSeverity::Tampered);
        assert!(ext_v.issues.iter().any(|i| i.kind == AuditIssueKind::ExternalModification));

        // 2. 模拟人为修改内嵌日志里的金额 -> 必须报 BrokenHashChain
        let mut tampered_log = log.clone();
        tampered_log.commits[1].cells[0].after = AuditSnap::Num(100.0);
        let tam_v = tampered_log.verify_against_workbook(&wb);
        assert_eq!(tam_v.severity, AuditSeverity::Tampered);
        assert!(tam_v.issues.iter().any(|i| i.kind == AuditIssueKind::BrokenHashChain));

        // 3. 模拟修改日志并重算 commit_hash 但伪造签名 -> 必须报 InvalidCommitSignature
        let mut forged = tampered_log.clone();
        let new_h = forged.commits[1].compute_hash(&forged.doc_id);
        forged.commits[1].commit_hash = hex_encode(&new_h);
        let forge_v = forged.verify_against_workbook(&wb);
        assert_eq!(forge_v.severity, AuditSeverity::Tampered);
        assert!(forge_v.issues.iter().any(|i| i.kind == AuditIssueKind::InvalidCommitSignature));
    }

    #[test]
    fn bounded_rollup_compaction_and_cold_archive_verification_and_blame() {
        let kp = generate_keypair().unwrap();
        let alice = test_author("张三", &kp);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
        let mut log = AuditLog::enable(&wb, alice.clone(), &kp, 1_700_000_000_000).unwrap();
        // 调低上限强制触发滚动折叠
        log.max_detailed_commits = 5;
        log.max_embedded_bytes = 2500;

        for step in 1..=12u32 {
            let base = wb.clone();
            wb.sheets[0].set(step, 0, Cell { value: Value::Number(step as f64 * 10.0), formula: None });
            let diff = compute_net_diff(&base, &wb, &alice, &HashMap::new(), &[]);
            log.append_commit(
                &wb,
                alice.clone(),
                &kp,
                "保存修改",
                &diff,
                &[],
                None,
                1_700_000_000_000 + step as u64 * 60_000,
            )
            .unwrap();
        }

        // 压缩后仍在硬上限内、链条完整且产生 Rollup 与可验证冷归档
        assert!(!log.rollups.is_empty(), "应触发滚动折叠");
        assert!(log.commits.len() <= 5, "详细提交条数应受上限约束: {}", log.commits.len());
        let v = log.verify_against_workbook(&wb);
        assert_eq!(v.severity, AuditSeverity::Verified, "{:?}", v.issues);
        assert!(v.embedded_bytes <= 2500, "压缩体积应受硬上限控制: {}", v.embedded_bytes);

        // 冷归档包验真必须通过，篡改 1 格必须失败
        let archive = log.last_compacted.clone().expect("折叠时应产出冷归档包");
        let av = log.verify_cold_archive(&archive);
        assert!(av.valid, "{}", av.summary);

        let mut bad_archive = archive.clone();
        bad_archive.commits[0].action = "伪造动作".into();
        assert!(!log.verify_cold_archive(&bad_archive).valid);

        // 即使第 1 行早已被折叠进 Rollup，单元格 Blame 依然能精准查出修改人与序号！
        let blame = log.lookup_blame("Sheet1", 1, 0).expect("Blame 应保留早期的最后修改人");
        assert_eq!(blame.author.display_name, "张三");
        assert_eq!(blame.seq, 2);
    }

    #[test]
    fn compact_binary_roundtrip_preserves_all_fields_and_blame() {
        let kp = generate_keypair().unwrap();
        let alice = test_author("王会计", &kp);
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(5, 3, Cell { value: Value::Number(888.88), formula: None });
        let mut log = AuditLog::enable(&wb, alice.clone(), &kp, 1_700_000_000_000).unwrap();
        let base = wb.clone();
        wb.sheets[0].set(5, 3, Cell { value: Value::Number(999.99), formula: None });
        let diff = compute_net_diff(&base, &wb, &alice, &HashMap::new(), &[]);
        log.append_commit(&wb, alice, &kp, "保存修改", &diff, &[], None, 1_700_000_120_000)
            .unwrap();

        let bytes = log.encode_compact_bytes().unwrap();
        let decoded = AuditLog::decode_compact_bytes(&bytes).unwrap();
        assert_eq!(decoded.doc_id, log.doc_id);
        assert_eq!(decoded.commits, log.commits);
        assert_eq!(decoded.blame, log.blame);
        assert_eq!(decoded.verify_against_workbook(&wb).severity, AuditSeverity::Verified);
    }

    #[test]
    fn co_signer_peer_attestation_verifies_and_rejects_forgery() {
        let kp_host = generate_keypair().unwrap();
        let kp_guest = generate_keypair().unwrap();
        let host = test_author("宿主张三", &kp_host);
        let guest = test_author("访客李四", &kp_guest);

        let mut wb = Workbook::new_blank();
        let mut log = AuditLog::enable(&wb, host.clone(), &kp_host, 1_700_000_000_000).unwrap();

        let base = wb.clone();
        wb.sheets[0].set(2, 1, Cell { value: Value::Number(520.0), formula: None });
        let mut attrs = HashMap::new();
        attrs.insert((wb.sheets[0].identity, 2, 1), guest.clone());
        let diff = compute_net_diff(&base, &wb, &host, &attrs, &[]);

        let guest_cells = vec![(
            "Sheet1".to_string(),
            2,
            1,
            AuditSnap::Empty,
            AuditSnap::Num(520.0),
        )];
        let g_digest = author_contribution_digest(&log.doc_id, log.head_seq(), &guest_cells);
        let g_sig = kp_guest.sign_digest(&g_digest).unwrap();
        let co = vec![PeerAttestationInput {
            author: guest.clone(),
            changes_digest: hex_encode(&g_digest),
            sig_hex: g_sig,
            attested_by_peer: true,
        }];

        log.append_commit(&wb, host, &kp_host, "协作保存", &diff, &co, None, 1_700_000_060_000)
            .unwrap();
        let v = log.verify_against_workbook(&wb);
        assert_eq!(v.severity, AuditSeverity::Verified, "{:?}", v.issues);

        // 篡改访客联署签名 -> 必须检测失败
        let mut bad = log.clone();
        bad.commits[1].co_signers[0].sig_hex = "00".repeat(64);
        let bad_h = bad.commits[1].compute_hash(&bad.doc_id);
        bad.commits[1].commit_hash = hex_encode(&bad_h);
        bad.commits[1].sig_hex = kp_host.sign_digest(&bad_h).unwrap();
        let bv = bad.verify_against_workbook(&wb);
        assert_eq!(bv.severity, AuditSeverity::Tampered);
        assert!(bv.issues.iter().any(|i| i.kind == AuditIssueKind::InvalidCoSignerSignature));
    }

    #[test]
    fn clock_rollback_triggers_warning() {
        let kp = generate_keypair().unwrap();
        let alice = test_author("张三", &kp);
        let mut wb = Workbook::new_blank();
        let mut log = AuditLog::enable(&wb, alice.clone(), &kp, 1_700_000_500_000).unwrap();
        let base = wb.clone();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
        let diff = compute_net_diff(&base, &wb, &alice, &HashMap::new(), &[]);
        // 故意回调本机时钟 5 分钟
        log.append_commit(&wb, alice, &kp, "保存修改", &diff, &[], None, 1_700_000_200_000)
            .unwrap();
        let v = log.verify_against_workbook(&wb);
        assert_eq!(v.severity, AuditSeverity::Warning);
        assert!(v.issues.iter().any(|i| i.kind == AuditIssueKind::ClockRollback));
    }

    #[test]
    fn ooxml_roundtrip_encrypted_excel_rename_strip_and_clean_export() {
        let dir = tempfile::tempdir().unwrap();
        let plain_path = dir.path().join("audited.xlsx");
        let enc_path = dir.path().join("audited_enc.xlsx");
        let clean_path = dir.path().join("clean_export.xlsx");

        let kp = generate_keypair().unwrap();
        let alice = test_author("财务张三", &kp);

        // 1. 新建启用审计 -> save_atomic -> load -> 100% Verified
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(1234.56), formula: None });
        wb.sheets[0].set(0, 1, Cell { value: Value::Number(2469.12), formula: Some("=A1*2".into()) });
        wb.sheets[0].set(1, 0, Cell { value: Value::Text("合同金额".into()), formula: None });
        let mut log = AuditLog::enable(&wb, alice.clone(), &kp, 1_700_000_000_000).unwrap();
        wb.audit.log = Some(Arc::new(log.clone()));

        crate::xlsx::save_atomic(&plain_path, &wb).expect("保存带审计明文 xlsx");
        let loaded = crate::xlsx::load(&plain_path).expect("重开带审计明文 xlsx");
        let v = verify_workbook_audit(&loaded).expect("应加载到审计日志");
        assert_eq!(v.severity, AuditSeverity::Verified, "{:?}", v.issues);
        assert_eq!(v.head_seq, 1);

        // 2. 增量修改并二次保存 -> 增量 delta 写盘 + 审计链追加 #2
        let base = loaded.clone();
        let mut edited = loaded.clone();
        edited.sheets[0].set(0, 0, Cell { value: Value::Number(2000.0), formula: None });
        let diff = compute_net_diff(&base, &edited, &alice, &HashMap::new(), &[]);
        log = (*edited.audit.log.as_ref().unwrap().as_ref()).clone();
        log.append_commit(&edited, alice.clone(), &kp, "保存修改", &diff, &[], None, 1_700_000_060_000)
            .unwrap();
        edited.audit.log = Some(Arc::new(log.clone()));
        crate::xlsx::save_atomic(&plain_path, &edited).expect("二次保存带审计 xlsx");

        let loaded2 = crate::xlsx::load(&plain_path).expect("二次重开");
        let v2 = verify_workbook_audit(&loaded2).expect("应存在审计日志");
        assert_eq!(v2.severity, AuditSeverity::Verified, "{:?}", v2.issues);
        assert_eq!(v2.head_seq, 2);
        let hist = loaded2.audit.log.as_ref().unwrap().cell_history("Sheet1", 0, 0);
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].before, AuditSnap::Num(1234.56));
        assert_eq!(hist[0].after, AuditSnap::Num(2000.0));

        // 3. 加密工作簿往返：审计部件随整包加密并在解密打开后验证通过
        let mut enc_wb = loaded2.clone();
        enc_wb.encryption = Some(crate::crypto::EncryptionSettings::new("secret-2026"));
        crate::xlsx::save_atomic(&enc_path, &enc_wb).expect("保存加密审计 xlsx");
        let loaded_enc = crate::xlsx::load_with_password(&enc_path, "secret-2026").expect("解密重开");
        let ve = verify_workbook_audit(&loaded_enc).expect("加密文件应保留审计日志");
        assert_eq!(ve.severity, AuditSeverity::Verified, "{:?}", ve.issues);
        assert_eq!(ve.head_seq, 2);

        // 4. 模拟 Excel 把 customXml/cellanAudit.xml 改名为 customXml/item1.xml 并静默修改单元格
        let excel_path = dir.path().join("excel_edited.xlsx");
        std::fs::copy(&plain_path, &excel_path).unwrap();
        {
            let audit_xml = crate::fixups::read_part_file(&excel_path, AUDIT_CUSTOM_XML_PART).unwrap();
            crate::patch::begin_stage_for(&excel_path);
            let _ = crate::patch::remove_entry(&excel_path, AUDIT_CUSTOM_XML_PART);
            crate::patch::append_entry(&excel_path, "customXml/item1.xml", &audit_xml).unwrap();
            crate::patch::commit_stage(&excel_path).unwrap();
        }
        let reloaded_renamed = crate::xlsx::load(&excel_path).expect("Excel 改名后仍应识别");
        assert!(reloaded_renamed.audit.log.is_some(), "按命名空间识别 item1.xml");
        assert_eq!(
            verify_workbook_audit(&reloaded_renamed).unwrap().severity,
            AuditSeverity::Verified
        );

        // 5. 模拟外部工具彻底剥离 customXml 但保留 docProps/custom.xml -> 必须检出 AuditStripped
        let stripped_path = dir.path().join("stripped.xlsx");
        std::fs::copy(&plain_path, &stripped_path).unwrap();
        {
            crate::patch::begin_stage_for(&stripped_path);
            let _ = crate::patch::remove_entry(&stripped_path, AUDIT_CUSTOM_XML_PART);
            crate::patch::commit_stage(&stripped_path).unwrap();
        }
        let loaded_stripped = crate::xlsx::load(&stripped_path).unwrap();
        let vs = verify_workbook_audit(&loaded_stripped).expect("应触发剥离告警");
        assert_eq!(vs.severity, AuditSeverity::Tampered);
        assert!(vs.issues.iter().any(|i| i.kind == AuditIssueKind::AuditStripped));

        // 6. 导出干净副本：零审计自定义部件、零 _sc_audit_meta 标记、打开干净无异常
        save_clean_copy(&clean_path, &loaded2).expect("导出干净副本");
        let loaded_clean = crate::xlsx::load(&clean_path).expect("打开干净副本");
        assert!(loaded_clean.audit.log.is_none());
        assert!(!loaded_clean.audit.has_anomaly());
        assert!(verify_workbook_audit(&loaded_clean).is_none());
        let zip_parts = crate::fixups::parts(&clean_path);
        assert!(
            !zip_parts.iter().any(|p| p.to_ascii_lowercase().starts_with("customxml/")),
            "干净副本不得残留任何 customXml 审计部件: {zip_parts:?}"
        );
    }

    fn make_author(name: &str, kp: &AuditKeyPair) -> AuditAuthor {
        AuditAuthor {
            display_name: name.to_string(),
            os_user: "testuser".into(),
            machine: "TEST-PC".into(),
            pubkey_hex: kp.pubkey_hex.clone(),
        }
    }

    #[test]
    fn review_182_defined_name_leading_equals_roundtrip_has_no_false_external_mod() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("names_audit.xlsx");
        let kp = generate_keypair().unwrap();
        let alice = make_author("李会计", &kp);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        let mut log = AuditLog::enable(&wb, alice.clone(), &kp, 1_700_000_000_000).unwrap();
        log.bind_workbook_sheets(&mut wb);
        wb.audit.log = Some(Arc::new(log.clone()));

        let base = wb.clone();
        // 按内存约定新增带前导 '=' 的名称常量（写入 OOXML 时 patch::write_user_names 会去掉前导 '='）
        wb.names.push(("LedgerRate".into(), None, "=0.05".into()));
        wb.names_dirty = true;
        let delta = compute_net_diff(&base, &wb, &alice, &HashMap::new(), &[]);
        assert!(
            delta.bulk_ops.iter().any(|b| b.2 == "名称变更"),
            "新增名称常量应记录名称变更摘要"
        );
        log.append_commit(&wb, alice, &kp, "保存修改", &delta, &[], None, 1_700_000_060_000)
            .unwrap();
        wb.audit.log = Some(Arc::new(log));

        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let reopened = crate::xlsx::load(&path).unwrap();
        let ver = verify_workbook_audit(&reopened).expect("重开后应含审计日志");
        assert_eq!(
            ver.severity,
            AuditSeverity::Verified,
            "保存带前导 '=' 名称常量后重开绝不能误报外部修改: {:?}",
            ver.issues
        );
    }

    #[test]
    fn review_182_sheet_rename_and_new_sheet_with_old_name_keeps_blame_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rename_audit.xlsx");
        let kp = generate_keypair().unwrap();
        let alice = make_author("张三", &kp);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        let mut log = AuditLog::enable(&wb, alice.clone(), &kp, 1_700_000_000_000).unwrap();
        log.bind_workbook_sheets(&mut wb);
        wb.audit.log = Some(Arc::new(log.clone()));

        // #2 修改 Sheet1!A1 = 250
        let base1 = wb.clone();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(250.0), formula: None });
        let d2 = compute_net_diff(&base1, &wb, &alice, &HashMap::new(), &[]);
        log.append_commit(&wb, alice.clone(), &kp, "保存修改", &d2, &[], None, 1_700_000_060_000)
            .unwrap();
        log.bind_workbook_sheets(&mut wb);
        wb.audit.log = Some(Arc::new(log.clone()));

        // 把 Sheet1 改名为 Revenue，并新建一张名为 Sheet1 的空表
        let base2 = wb.clone();
        wb.sheets[0].name = "Revenue".into();
        wb.sheets.push(Sheet::new("Sheet1"));

        // 保存前实时查询也必须正确隔离
        let pre_rev = log.lookup_blame_for_sheet(&wb, &wb.sheets[0], 0, 0).expect("改名后保存前原表应保留 Blame");
        assert_eq!(pre_rev.seq, 2);
        assert!(
            log.lookup_blame_for_sheet(&wb, &wb.sheets[1], 0, 0).is_none(),
            "新建的同名空表在保存前绝不能串用旧表 Blame"
        );

        let d3 = compute_net_diff(&base2, &wb, &alice, &HashMap::new(), &[]);
        log.append_commit(&wb, alice, &kp, "保存修改", &d3, &[], None, 1_700_000_120_000)
            .unwrap();
        wb.audit.log = Some(Arc::new(log));

        // 写盘并重开后核验
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let reopened = crate::xlsx::load(&path).unwrap();
        let ver = verify_workbook_audit(&reopened).unwrap();
        assert_eq!(ver.severity, AuditSeverity::Verified, "{:?}", ver.issues);
        let rlog = reopened.audit.log.as_ref().unwrap();

        let rev_blame = rlog.lookup_blame("Revenue", 0, 0).expect("重开后 Revenue!A1 必须保留原作者");
        assert_eq!(rev_blame.seq, 2);
        assert!(!rev_blame.is_baseline);
        let rev_hist = rlog.cell_history("Revenue", 0, 0);
        assert_eq!(rev_hist.len(), 1);
        assert_eq!(rev_hist[0].after, AuditSnap::Num(250.0));

        assert!(
            rlog.lookup_blame("Sheet1", 0, 0).is_none(),
            "重开后新建同名空表 Sheet1!A1 不得显示旧表作者"
        );
        assert!(
            rlog.cell_history("Sheet1", 0, 0).is_empty(),
            "重开后新建同名空表 Sheet1!A1 修改历史必须为空"
        );
    }

    #[test]
    fn review_182_cosigner_mismatch_downgrades_and_merge_attributes_to_guest() {
        let host_kp = generate_keypair().unwrap();
        let guest_kp = generate_keypair().unwrap();
        let host = make_author("宿主", &host_kp);
        let guest = make_author("访客", &guest_kp);

        let mut wb = Workbook::new_blank();
        let mut log = AuditLog::enable(&wb, host.clone(), &host_kp, 1_700_000_000_000).unwrap();
        log.bind_workbook_sheets(&mut wb);
        let base = wb.clone();

        // 访客先改了 B1=10 与 B2=20 并对两格联署；随后宿主覆写了 B2=99，访客又合并了 C1:D1
        let stale_guest_cells = vec![
            ("Sheet1".to_string(), 0, 1, AuditSnap::Empty, AuditSnap::Num(10.0)),
            ("Sheet1".to_string(), 1, 1, AuditSnap::Empty, AuditSnap::Num(20.0)),
        ];
        let stale_digest = author_contribution_digest(&log.doc_id, 1, &stale_guest_cells);
        let stale_sig = guest_kp.sign_digest(&stale_digest).unwrap();
        let stale_att = PeerAttestationInput {
            author: guest.clone(),
            changes_digest: hex_encode(&stale_digest),
            sig_hex: stale_sig,
            attested_by_peer: true,
        };

        wb.sheets[0].set(0, 1, Cell { value: Value::Number(10.0), formula: None });
        wb.sheets[0].set(1, 1, Cell { value: Value::Number(99.0), formula: None });
        wb.sheets[0].merge_range(0, 2, 0, 3);
        let sid = wb.sheets[0].identity;

        let mut cell_attr = HashMap::new();
        cell_attr.insert((sid, 0, 1), guest.clone());
        cell_attr.insert((sid, 1, 1), host.clone());
        let mut merge_attr = HashMap::new();
        merge_attr.insert((sid, (0, 2, 0, 3)), guest.clone());

        let delta = compute_net_diff_full(&base, &wb, &host, &cell_attr, &merge_attr, &[], &[]);
        let merge_op = delta
            .bulk_ops
            .iter()
            .find(|b| b.2 == "结构变更" && b.3.contains("合并"))
            .expect("应生成合并结构摘要");
        assert_eq!(merge_op.1.pubkey_hex, guest.pubkey_hex, "访客合并空白区域必须归属访客而非宿主");

        let commit = log
            .append_commit(&wb, host.clone(), &host_kp, "协作保存", &delta, &[stale_att], None, 1_700_000_060_000)
            .unwrap()
            .clone();
        assert_eq!(commit.co_signers.len(), 1);
        assert_eq!(commit.co_signers[0].change_count, 1);
        assert!(
            !commit.co_signers[0].attested_by_peer,
            "联署摘要与最终落盘集合不一致时绝不能继续标为已联署"
        );
        let expected_single = author_contribution_digest(
            &log.doc_id,
            1,
            &[("Sheet1".to_string(), 0, 1, AuditSnap::Empty, AuditSnap::Num(10.0))],
        );
        assert_eq!(commit.co_signers[0].changes_digest, hex_encode(&expected_single));
        let ver = log.verify_against_workbook(&wb);
        assert_eq!(ver.severity, AuditSeverity::Warning);
        assert!(ver.issues.iter().any(|i| i.kind == AuditIssueKind::UnattestedCollabEdits));
    }

    #[test]
    fn review_182_blame_baseline_distinction_and_over_100k_cap_fallback() {
        let kp = generate_keypair().unwrap();
        let alice = make_author("开启人", &kp);
        let bob_kp = generate_keypair().unwrap();
        let bob = make_author("编辑人", &bob_kp);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        let mut log = AuditLog::enable(&wb, alice.clone(), &kp, 1_700_000_000_000).unwrap();
        let base_blame = log.lookup_blame("Sheet1", 0, 0).unwrap();
        assert!(base_blame.is_baseline, "开启审计时已有单元格必须标记为基线登记人而非最后修改人");

        // 人为填满 MAX_BLAME_ENTRIES 条 seq=1 基线记录，模拟 10 万格大表
        let si = log.resolve_sheet_slot_by_name("Sheet1").unwrap();
        log.blame.clear();
        log.blame.reserve(MAX_BLAME_ENTRIES);
        for i in 0..MAX_BLAME_ENTRIES {
            log.blame.push(BlameRecord {
                sheet_idx: si,
                row: (i / 100) as u32,
                col: (i % 100) as u32,
                author_idx: 0,
                seq: 1,
                ts_ms: 1_700_000_000_000,
            });
        }
        log.blame.sort_unstable_by_key(|b| (b.sheet_idx, b.row, b.col));

        // 超过 10 万格后再新填一个单元格 (2000, 5)
        let base = wb.clone();
        wb.sheets[0].set(2000, 5, Cell { value: Value::Number(777.0), formula: None });
        let mut attr = HashMap::new();
        attr.insert((wb.sheets[0].identity, 2000, 5), bob.clone());
        let d2 = compute_net_diff(&base, &wb, &bob, &attr, &[]);
        log.append_commit(&wb, bob.clone(), &bob_kp, "保存修改", &d2, &[], None, 1_700_000_060_000)
            .unwrap();
        assert!(log.blame.len() <= MAX_BLAME_ENTRIES);

        let b_new = log.lookup_blame("Sheet1", 2000, 5).expect("超过 10 万格后新编辑格必须查得到最后修改人");
        assert_eq!(b_new.seq, 2);
        assert!(!b_new.is_baseline);
        assert_eq!(b_new.author.pubkey_hex, bob.pubkey_hex);

        // 即使从 blame 索引中移除，也应通过近期提交明细窗口补查命中
        log.blame.retain(|r| !(r.sheet_idx == si && r.row == 2000 && r.col == 5));
        let b_fallback = log.lookup_blame("Sheet1", 2000, 5).expect("Blame 裁剪后应从近期明细回退命中");
        assert_eq!(b_fallback.seq, 2);
        assert_eq!(b_fallback.author.pubkey_hex, bob.pubkey_hex);
    }
}
