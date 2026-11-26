//! 本机防篡改审计持久化存储：
//! 1. **设备签名身份**：Windows CNG ECDSA P-256 密钥对，私钥经 Windows DPAPI 保护存入 `%LOCALAPPDATA%\CELLAN\audit_identity.dpapi`；
//! 2. **去中心化防回滚水印（P2P Witnessing）**：记录本机见过的每个审计文档 `(doc_id -> max_seq, head_hash)` 与路径映射，
//!    一旦有人用昨天的旧备份覆盖共享盘文件或用外部软件剥离审计部件，任何曾打开过新版的同事再次打开立刻亮红灯告警；
//! 3. **TOFU 同事公钥信任簿**：首次见到某作者自动记住其公钥指纹，若同名或同账号更换了签名密钥立即标黄提醒防冒充。

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use sheetcore::audit::{
    AuditAuthor, AuditIssue, AuditIssueKind, AuditKeyPair, AuditLog, AuditSeverity,
    AuditVerification,
};
use sheetcore::model::Workbook;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const IDENTITY_FILE: &str = "audit_identity.dpapi";
const WATERMARKS_FILE: &str = "audit_watermarks.json";
const TRUST_FILE: &str = "audit_trust.json";
const KEY_ENTROPY: &[u8] = b"CELLAN-AUDIT-DEVICE-KEY-V1";
const MAX_WATERMARK_DOCS: usize = 2000;
const MAX_TRUST_ENTRIES: usize = 1000;

fn audit_path(name: &str) -> PathBuf {
    crate::update::app_dir().join(name)
}

pub fn os_username() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub fn machine_name() -> String {
    crate::session_lock::machine_name()
}

pub fn current_author(display_name: &str, kp: &AuditKeyPair) -> AuditAuthor {
    AuditAuthor {
        display_name: display_name.trim().to_string(),
        os_user: os_username(),
        machine: machine_name(),
        pubkey_hex: kp.pubkey_hex.clone(),
    }
}

/// 加载或首次自动生成本机 ECDSA P-256 签名密钥对（私钥经 DPAPI 加密绑定当前 Windows 用户）
pub fn load_or_create_device_keypair() -> Result<AuditKeyPair> {
    let path = audit_path(IDENTITY_FILE);
    if path.is_file() {
        let raw = std::fs::read(&path)
            .with_context(|| format!("读取本机审计签名密钥失败: {}", path.display()))?;
        if let Ok(plain) = sheetcore::audit::dpapi_unprotect(&raw, KEY_ENTROPY) {
            if plain.len() == 96 {
                let blob: [u8; 96] = plain.try_into().unwrap();
                if let Ok(kp) = AuditKeyPair::from_priv_blob(blob) {
                    return Ok(kp);
                }
            }
        }
        // 若密钥文件损坏或跨 Windows 账户迁移，备份旧文件并生成新密钥对
        let bak = path.with_extension("dpapi.bak");
        let _ = std::fs::rename(&path, &bak);
    }

    let kp = sheetcore::audit::generate_keypair()?;
    let protected = sheetcore::audit::dpapi_protect(&kp.priv_blob, KEY_ENTROPY)?;
    write_atomic(&path, &protected)?;
    Ok(kp)
}

/// 为协作 `Hello` 握手签名身份证明（证明当前连接确实持有 `pubkey_hex` 对应私钥）
pub fn sign_collab_hello(
    kp: &AuditKeyPair,
    nonce: u64,
    site: u32,
    display_name: &str,
) -> Result<String> {
    let digest = collab_hello_digest(nonce, site, &kp.pubkey_hex, display_name);
    kp.sign_digest(&digest)
}

pub fn verify_collab_hello(
    nonce: u64,
    site: u32,
    pubkey_hex: &str,
    display_name: &str,
    sig_hex: &str,
) -> bool {
    if pubkey_hex.len() != 128 || sig_hex.len() != 128 {
        return false;
    }
    let digest = collab_hello_digest(nonce, site, pubkey_hex, display_name);
    sheetcore::audit::verify_digest_hex(pubkey_hex, &digest, sig_hex)
}

fn collab_hello_digest(nonce: u64, site: u32, pubkey_hex: &str, display_name: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"CELLAN-COLLAB-ID-V1\n");
    h.update(nonce.to_le_bytes());
    h.update(site.to_le_bytes());
    h.update(pubkey_hex.to_ascii_lowercase().as_bytes());
    h.update(b"\n");
    h.update(display_name.as_bytes());
    h.finalize().into()
}

// ---------------------------------------------------------------------------
// 去中心化防回滚水印表
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct WatermarkEntry {
    pub doc_id: String,
    pub max_seq: u64,
    pub head_hash: String,
    pub last_seen_ms: u64,
    pub last_author: String,
    pub last_path_hash: String,
}

impl WatermarkEntry {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "doc_id": self.doc_id,
            "max_seq": self.max_seq,
            "head_hash": self.head_hash,
            "last_seen_ms": self.last_seen_ms,
            "last_author": self.last_author,
            "last_path_hash": self.last_path_hash,
        })
    }

    fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            doc_id: v.get("doc_id")?.as_str()?.to_string(),
            max_seq: v.get("max_seq")?.as_u64()?,
            head_hash: v.get("head_hash")?.as_str()?.to_string(),
            last_seen_ms: v.get("last_seen_ms").and_then(|x| x.as_u64()).unwrap_or(0),
            last_author: v
                .get("last_author")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            last_path_hash: v
                .get("last_path_hash")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct PathWatermark {
    pub doc_id: String,
    pub max_seq: u64,
    pub head_hash: String,
    pub last_seen_ms: u64,
}

impl PathWatermark {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "doc_id": self.doc_id,
            "max_seq": self.max_seq,
            "head_hash": self.head_hash,
            "last_seen_ms": self.last_seen_ms,
        })
    }

    fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            doc_id: v.get("doc_id")?.as_str()?.to_string(),
            max_seq: v.get("max_seq")?.as_u64()?,
            head_hash: v.get("head_hash")?.as_str()?.to_string(),
            last_seen_ms: v.get("last_seen_ms").and_then(|x| x.as_u64()).unwrap_or(0),
        })
    }
}

#[derive(Clone, Debug, Default)]
struct WatermarkStore {
    by_doc_id: HashMap<String, WatermarkEntry>,
    by_path_hash: HashMap<String, PathWatermark>,
}

impl WatermarkStore {
    fn load() -> Self {
        let path = audit_path(WATERMARKS_FILE);
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Self::default();
        };
        let mut store = Self::default();
        if let Some(map) = val.get("by_doc_id").and_then(|v| v.as_object()) {
            for (k, v) in map {
                if let Some(entry) = WatermarkEntry::from_json(v) {
                    store.by_doc_id.insert(k.clone(), entry);
                }
            }
        }
        if let Some(map) = val.get("by_path_hash").and_then(|v| v.as_object()) {
            for (k, v) in map {
                if let Some(entry) = PathWatermark::from_json(v) {
                    store.by_path_hash.insert(k.clone(), entry);
                }
            }
        }
        store
    }

    fn save(&mut self) {
        // LRU 裁剪，保证本地配置目录也有严格上界
        if self.by_doc_id.len() > MAX_WATERMARK_DOCS {
            let mut items: Vec<(String, u64)> = self
                .by_doc_id
                .iter()
                .map(|(k, v)| (k.clone(), v.last_seen_ms))
                .collect();
            items.sort_by_key(|(_, ts)| *ts);
            let drop_n = self.by_doc_id.len() - MAX_WATERMARK_DOCS;
            for (k, _) in items.into_iter().take(drop_n) {
                self.by_doc_id.remove(&k);
            }
        }
        if self.by_path_hash.len() > MAX_WATERMARK_DOCS {
            let mut items: Vec<(String, u64)> = self
                .by_path_hash
                .iter()
                .map(|(k, v)| (k.clone(), v.last_seen_ms))
                .collect();
            items.sort_by_key(|(_, ts)| *ts);
            let drop_n = self.by_path_hash.len() - MAX_WATERMARK_DOCS;
            for (k, _) in items.into_iter().take(drop_n) {
                self.by_path_hash.remove(&k);
            }
        }
        let mut by_doc_id = serde_json::Map::new();
        for (k, v) in &self.by_doc_id {
            by_doc_id.insert(k.clone(), v.to_json());
        }
        let mut by_path_hash = serde_json::Map::new();
        for (k, v) in &self.by_path_hash {
            by_path_hash.insert(k.clone(), v.to_json());
        }
        let val = serde_json::json!({
            "by_doc_id": by_doc_id,
            "by_path_hash": by_path_hash,
        });
        if let Ok(bytes) = serde_json::to_vec_pretty(&val) {
            let _ = write_atomic(&audit_path(WATERMARKS_FILE), &bytes);
        }
    }
}

fn normalized_path_hash(path: &Path) -> String {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut s = canon.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        s = s.to_ascii_lowercase();
    }
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    let mut h = Sha256::new();
    h.update(b"cellan-path-wm-v1:");
    h.update(s.as_bytes());
    sheetcore::audit::hex_encode(&h.finalize())
}

/// 推进本机防回滚水印（仅在保存成功或通过验签的正常打开/协作同步时调用）
pub fn record_verified_watermark(path: Option<&Path>, log: &AuditLog) {
    let mut store = WatermarkStore::load();
    let seq = log.head_seq();
    let head_hash = log.head_hash().to_string();
    let now_ms = sheetcore::revision::now_ms();
    let last_author = log
        .head()
        .and_then(|c| log.author_of(c.committer_idx))
        .map(|a| a.short_label())
        .unwrap_or_default();
    let path_hash = path.map(normalized_path_hash).unwrap_or_default();

    let should_update = store
        .by_doc_id
        .get(&log.doc_id)
        .is_none_or(|prev| seq > prev.max_seq || (seq == prev.max_seq && head_hash == prev.head_hash));
    if should_update {
        store.by_doc_id.insert(
            log.doc_id.clone(),
            WatermarkEntry {
                doc_id: log.doc_id.clone(),
                max_seq: seq,
                head_hash: head_hash.clone(),
                last_seen_ms: now_ms,
                last_author,
                last_path_hash: path_hash.clone(),
            },
        );
    }
    if !path_hash.is_empty() {
        let path_ok = store.by_path_hash.get(&path_hash).is_none_or(|prev| {
            prev.doc_id != log.doc_id || seq >= prev.max_seq
        });
        if path_ok {
            store.by_path_hash.insert(
                path_hash,
                PathWatermark {
                    doc_id: log.doc_id.clone(),
                    max_seq: seq,
                    head_hash,
                    last_seen_ms: now_ms,
                },
            );
        }
    }
    store.save();
}

// ---------------------------------------------------------------------------
// TOFU 同事身份公钥信任簿
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorTrustBadge {
    SelfDevice,
    KnownTrusted,
    KeyChanged,
    FirstSeen,
}

#[derive(Clone, Debug)]
struct TrustRecord {
    pubkey_hex: String,
    fingerprint: String,
    first_seen_ms: u64,
    last_seen_ms: u64,
    display_name: String,
    os_user: String,
    machine: String,
}

impl TrustRecord {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "pubkey_hex": self.pubkey_hex,
            "fingerprint": self.fingerprint,
            "first_seen_ms": self.first_seen_ms,
            "last_seen_ms": self.last_seen_ms,
            "display_name": self.display_name,
            "os_user": self.os_user,
            "machine": self.machine,
        })
    }

    fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            pubkey_hex: v.get("pubkey_hex")?.as_str()?.to_string(),
            fingerprint: v.get("fingerprint").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            first_seen_ms: v.get("first_seen_ms").and_then(|x| x.as_u64()).unwrap_or(0),
            last_seen_ms: v.get("last_seen_ms").and_then(|x| x.as_u64()).unwrap_or(0),
            display_name: v
                .get("display_name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            os_user: v.get("os_user").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            machine: v.get("machine").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        })
    }
}

#[derive(Clone, Debug, Default)]
struct TrustStore {
    /// 标准化名字/账号键 -> 已知的公钥列表（首个视为首次信任基准）
    by_identity: HashMap<String, Vec<TrustRecord>>,
}

impl TrustStore {
    fn load() -> Self {
        let path = audit_path(TRUST_FILE);
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Self::default();
        };
        let mut store = Self::default();
        if let Some(map) = val.get("by_identity").and_then(|v| v.as_object()) {
            for (k, arr) in map {
                if let Some(items) = arr.as_array() {
                    let recs: Vec<TrustRecord> =
                        items.iter().filter_map(TrustRecord::from_json).collect();
                    if !recs.is_empty() {
                        store.by_identity.insert(k.clone(), recs);
                    }
                }
            }
        }
        store
    }

    fn save(&mut self) {
        if self.by_identity.len() > MAX_TRUST_ENTRIES {
            let mut items: Vec<(String, u64)> = self
                .by_identity
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().map(|r| r.last_seen_ms).max().unwrap_or(0)))
                .collect();
            items.sort_by_key(|(_, ts)| *ts);
            let drop_n = self.by_identity.len() - MAX_TRUST_ENTRIES;
            for (k, _) in items.into_iter().take(drop_n) {
                self.by_identity.remove(&k);
            }
        }
        let mut by_identity = serde_json::Map::new();
        for (k, recs) in &self.by_identity {
            let arr: Vec<serde_json::Value> = recs.iter().map(TrustRecord::to_json).collect();
            by_identity.insert(k.clone(), serde_json::Value::Array(arr));
        }
        let val = serde_json::json!({
            "by_identity": by_identity,
        });
        if let Ok(bytes) = serde_json::to_vec_pretty(&val) {
            let _ = write_atomic(&audit_path(TRUST_FILE), &bytes);
        }
    }

    fn identity_keys(author: &AuditAuthor) -> Vec<String> {
        let mut keys = Vec::new();
        let dn = author.display_name.trim().to_ascii_lowercase();
        if !dn.is_empty() {
            keys.push(format!("name:{dn}"));
        }
        let u = author.os_user.trim().to_ascii_lowercase();
        let m = author.machine.trim().to_ascii_lowercase();
        if !u.is_empty() && !m.is_empty() {
            keys.push(format!("acct:{u}@{m}"));
        }
        keys
    }
}

pub fn trust_badge_for_author(author: &AuditAuthor, my_pubkey: Option<&str>) -> AuthorTrustBadge {
    if my_pubkey.is_some_and(|pk| !pk.is_empty() && pk.eq_ignore_ascii_case(&author.pubkey_hex)) {
        return AuthorTrustBadge::SelfDevice;
    }
    let store = TrustStore::load();
    let keys = TrustStore::identity_keys(author);
    let mut saw_known = false;
    for k in &keys {
        if let Some(list) = store.by_identity.get(k) {
            if let Some(first) = list.first() {
                if first.pubkey_hex.eq_ignore_ascii_case(&author.pubkey_hex) {
                    saw_known = true;
                } else {
                    return AuthorTrustBadge::KeyChanged;
                }
            }
        }
    }
    if saw_known {
        AuthorTrustBadge::KnownTrusted
    } else {
        AuthorTrustBadge::FirstSeen
    }
}

// ---------------------------------------------------------------------------
// 完整综合核验入口（核心哈希链/签名/状态根 + 本地防回滚水印 + TOFU 身份簿）
// ---------------------------------------------------------------------------

pub fn verify_workbook_full(
    path: Option<&Path>,
    wb: &Workbook,
    my_pubkey: Option<&str>,
) -> Option<AuditVerification> {
    let mut wm_store = WatermarkStore::load();
    let path_hash = path.map(normalized_path_hash);

    let Some(mut verification) = sheetcore::audit::verify_workbook_audit(wb) else {
        // 文件当前没有任何审计日志，但检查本机路径水印：
        // 若此路径曾经是启用防篡改审计的文件，说明被外部工具彻底剥离了或被无审计旧备份覆盖！
        if let Some(ph) = path_hash.as_deref() {
            if let Some(prev) = wm_store.by_path_hash.get(ph) {
                let mut v = AuditVerification {
                    severity: AuditSeverity::Tampered,
                    doc_id: Some(prev.doc_id.clone()),
                    head_seq: 0,
                    head_hash: String::new(),
                    current_state_hash: sheetcore::audit::workbook_state_hash(wb),
                    expected_state_hash: None,
                    embedded_bytes: 0,
                    detailed_commit_count: 0,
                    rolled_up_commit_count: 0,
                    blame_count: 0,
                    issues: Vec::new(),
                };
                v.push_issue(AuditIssue {
                    severity: AuditSeverity::Tampered,
                    kind: AuditIssueKind::RollbackDetected,
                    seq: None,
                    message: format!(
                        "高危告警：此路径文件曾启用防篡改审计（本机记录至第 #{} 版），但当前文件不含任何审计记录！可能被外部软件清除了审计数据，或被启用审计前的旧备份覆盖。",
                        prev.max_seq
                    ),
                });
                return Some(v);
            }
        }
        return None;
    };

    let Some(log) = wb.audit.log.as_ref() else {
        return Some(verification);
    };

    // 1. 去中心化防回滚 / 分叉水印核验
    let seq = log.head_seq();
    let head_hash = log.head_hash();
    if let Some(prev) = wm_store.by_doc_id.get(&log.doc_id) {
        if seq < prev.max_seq {
            verification.push_issue(AuditIssue {
                severity: AuditSeverity::Tampered,
                kind: AuditIssueKind::RollbackDetected,
                seq: Some(seq),
                message: format!(
                    "检测到版本回退！本机曾见证此文档达到第 #{} 版（上次由 {} 提交），当前文件仅为第 #{} 版（疑似被旧备份覆盖）",
                    prev.max_seq, prev.last_author, seq
                ),
            });
        } else if seq == prev.max_seq && head_hash != prev.head_hash {
            verification.push_issue(AuditIssue {
                severity: AuditSeverity::Tampered,
                kind: AuditIssueKind::ForkDetected,
                seq: Some(seq),
                message: format!(
                    "检测到审计链分叉！同为第 #{} 版，当前提交哈希（{}…）与本机此前见证（{}…）不一致",
                    seq,
                    &head_hash[..head_hash.len().min(10)],
                    &prev.head_hash[..prev.head_hash.len().min(10)],
                ),
            });
        }
    }

    // 2. TOFU 作者身份公钥指纹核验
    let mut trust_store = TrustStore::load();
    let now_ms = sheetcore::revision::now_ms();
    let mut warned_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for author in &log.author_dict {
        if author.pubkey_hex.is_empty() {
            continue;
        }
        if my_pubkey.is_some_and(|pk| pk.eq_ignore_ascii_case(&author.pubkey_hex)) {
            continue;
        }
        let fp = author.fingerprint();
        for key in TrustStore::identity_keys(author) {
            if let Some(records) = trust_store.by_identity.get(&key) {
                if let Some(first) = records.first() {
                    if !first.pubkey_hex.eq_ignore_ascii_case(&author.pubkey_hex)
                        && warned_keys.insert(format!("{key}|{}", author.pubkey_hex))
                    {
                        verification.push_issue(AuditIssue {
                            severity: AuditSeverity::Warning,
                            kind: AuditIssueKind::IdentityKeyChanged,
                            seq: None,
                            message: format!(
                                "作者身份指纹变化：{} 本次签名指纹为 {}，与本机首次信任记录（{}）不同（请核实是否更换设备或同名冒充）",
                                author.short_label(),
                                fp,
                                first.fingerprint
                            ),
                        });
                    }
                }
            }
        }
    }

    // 3. 仅在非 Tampered 状态下推进本机水印与 TOFU 信任库（防止恶意文件污染本机信任基准）
    if verification.severity != AuditSeverity::Tampered {
        let last_author = log
            .head()
            .and_then(|c| log.author_of(c.committer_idx))
            .map(|a| a.short_label())
            .unwrap_or_default();
        let ph_str = path_hash.clone().unwrap_or_default();
        wm_store.by_doc_id.insert(
            log.doc_id.clone(),
            WatermarkEntry {
                doc_id: log.doc_id.clone(),
                max_seq: seq,
                head_hash: head_hash.to_string(),
                last_seen_ms: now_ms,
                last_author,
                last_path_hash: ph_str.clone(),
            },
        );
        if let Some(ph) = path_hash {
            wm_store.by_path_hash.insert(
                ph,
                PathWatermark {
                    doc_id: log.doc_id.clone(),
                    max_seq: seq,
                    head_hash: head_hash.to_string(),
                    last_seen_ms: now_ms,
                },
            );
        }
        wm_store.save();

        let mut trust_dirty = false;
        for author in &log.author_dict {
            if author.pubkey_hex.is_empty() {
                continue;
            }
            for key in TrustStore::identity_keys(author) {
                let list = trust_store.by_identity.entry(key).or_default();
                if let Some(existing) = list
                    .iter_mut()
                    .find(|r| r.pubkey_hex.eq_ignore_ascii_case(&author.pubkey_hex))
                {
                    existing.last_seen_ms = now_ms;
                } else {
                    list.push(TrustRecord {
                        pubkey_hex: author.pubkey_hex.clone(),
                        fingerprint: author.fingerprint(),
                        first_seen_ms: now_ms,
                        last_seen_ms: now_ms,
                        display_name: author.display_name.clone(),
                        os_user: author.os_user.clone(),
                        machine: author.machine.clone(),
                    });
                }
                trust_dirty = true;
            }
        }
        if trust_dirty {
            trust_store.save();
        }
    }

    Some(verification)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    if std::fs::rename(&tmp, path).is_err() {
        std::fs::write(path, bytes)?;
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheetcore::audit::{compute_net_diff, AuditLog};
    use sheetcore::model::{Cell, Value, Workbook};
    use std::sync::Arc;

    #[test]
    fn device_identity_dpapi_persists_isolated_and_detects_rollback_and_tofu_change() {
        let env = crate::test_env::TestEnv::new("audit-store");
        let kp1 = load_or_create_device_keypair().expect("首次创建本机密钥");
        let kp2 = load_or_create_device_keypair().expect("再次读取应返回同一把私钥");
        assert_eq!(kp1.pubkey_hex, kp2.pubkey_hex);
        assert!(env.profile().join("CELLAN").join(IDENTITY_FILE).is_file());

        // 构造文档并见证第 2 版
        let file = env.root().join("ledger.xlsx");
        let alice_kp = sheetcore::audit::generate_keypair().unwrap();
        let alice = AuditAuthor {
            display_name: "李会计".into(),
            os_user: "li".into(),
            machine: "FIN-02".into(),
            pubkey_hex: alice_kp.pubkey_hex.clone(),
        };
        let mut wb1 = Workbook::new_blank();
        wb1.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        let mut log1 = AuditLog::enable(&wb1, alice.clone(), &alice_kp, 1_700_000_000_000).unwrap();
        wb1.audit.log = Some(Arc::new(log1.clone()));

        let mut wb2 = wb1.clone();
        wb2.sheets[0].set(0, 0, Cell { value: Value::Number(200.0), formula: None });
        let diff = compute_net_diff(&wb1, &wb2, &alice, &HashMap::new(), &[]);
        log1.append_commit(&wb2, alice.clone(), &alice_kp, "保存修改", &diff, &[], None, 1_700_000_060_000)
            .unwrap();
        wb2.audit.log = Some(Arc::new(log1.clone()));

        // 本机打开第 2 版 -> Verified，写入水印 #2 与 李会计 TOFU 公钥
        let v2 = verify_workbook_full(Some(&file), &wb2, Some(&kp1.pubkey_hex)).unwrap();
        assert_eq!(v2.severity, AuditSeverity::Verified, "{:?}", v2.issues);

        // 1. 回滚测试：有人把昨天的 wb1（第 1 版）拷回来覆盖 -> 必须报 RollbackDetected
        let v_rb = verify_workbook_full(Some(&file), &wb1, Some(&kp1.pubkey_hex)).unwrap();
        assert_eq!(v_rb.severity, AuditSeverity::Tampered);
        assert!(v_rb.issues.iter().any(|i| i.kind == AuditIssueKind::RollbackDetected));

        // 2. 彻底剥离测试：有人用无审计的旧文件覆盖同一路径 -> 必须由路径水印捕获
        let clean_wb = Workbook::new_blank();
        let v_strip = verify_workbook_full(Some(&file), &clean_wb, Some(&kp1.pubkey_hex)).unwrap();
        assert_eq!(v_strip.severity, AuditSeverity::Tampered);
        assert!(v_strip.issues.iter().any(|i| i.kind == AuditIssueKind::RollbackDetected));

        // 3. TOFU 冒充测试：陌生人改名叫“李会计”并用另一把私钥提交第 3 版 -> 必须报 IdentityKeyChanged
        let imposter_kp = sheetcore::audit::generate_keypair().unwrap();
        let imposter = AuditAuthor {
            display_name: "李会计".into(),
            os_user: "hacker".into(),
            machine: "OTHER-PC".into(),
            pubkey_hex: imposter_kp.pubkey_hex.clone(),
        };
        let mut wb3 = wb2.clone();
        wb3.sheets[0].set(0, 0, Cell { value: Value::Number(9999.0), formula: None });
        let diff3 = compute_net_diff(&wb2, &wb3, &imposter, &HashMap::new(), &[]);
        let mut log3 = log1.clone();
        log3.append_commit(&wb3, imposter, &imposter_kp, "保存修改", &diff3, &[], None, 1_700_000_120_000)
            .unwrap();
        wb3.audit.log = Some(Arc::new(log3));
        let v3 = verify_workbook_full(Some(&file), &wb3, Some(&kp1.pubkey_hex)).unwrap();
        assert_eq!(v3.severity, AuditSeverity::Warning);
        assert!(v3.issues.iter().any(|i| i.kind == AuditIssueKind::IdentityKeyChanged));
    }
}
