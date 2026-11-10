//! 工作表保护：SHA-512（Excel 2013+）与旧的 16 位口令哈希。
//!
//! Excel 365 写的是 `<sheetProtection algorithmName="SHA-512" hashValue=".."
//! saltValue=".." spinCount="100000" sheet="1"/>`。哈希算法（实测对齐）：
//!   H0 = SHA512(salt || UTF-16LE(password))
//!   Hi = SHA512(H(i-1) || LE32(i))，i = 0..spinCount-1
//!   hashValue = H(spinCount-1)
//! 旧文件只有 `password="XXXX"`（16 位哈希的十六进制）。

use base64::Engine;
use sha2::{Digest, Sha512};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SheetProtection {
    /// "SHA-512" 或空（旧格式）
    pub algorithm: String,
    /// base64 的 hashValue（SHA-512 模式）
    pub hash: String,
    /// base64 的 saltValue
    pub salt: String,
    pub spin: u32,
    /// 旧格式的 16 位哈希
    pub legacy: Option<u16>,
    pub sheet: bool,
    pub objects: bool,
    pub scenarios: bool,
}

impl SheetProtection {
    /// 用密码生成一份新的保护（SHA-512，随机盐）
    pub fn new_with_password(pw: &str) -> Self {
        let salt = random_salt();
        let spin = 100_000u32;
        let hash = hash_password(pw, &salt, spin);
        Self {
            algorithm: "SHA-512".into(),
            hash: base64::engine::general_purpose::STANDARD.encode(hash),
            salt: base64::engine::general_purpose::STANDARD.encode(salt),
            spin,
            legacy: None,
            sheet: true,
            objects: true,
            scenarios: true,
        }
    }

    pub fn verify(&self, pw: &str) -> bool {
        if self.algorithm.eq_ignore_ascii_case("SHA-512") && !self.salt.is_empty() {
            let Ok(salt) = base64::engine::general_purpose::STANDARD.decode(&self.salt) else {
                return false;
            };
            let Ok(want) = base64::engine::general_purpose::STANDARD.decode(&self.hash) else {
                return false;
            };
            let got = hash_password(pw, &salt, self.spin.max(1));
            return got == want;
        }
        match self.legacy {
            Some(h) => legacy_hash(pw) == h,
            None => true,
        }
    }

    pub fn to_xml(&self) -> String {
        if self.algorithm.eq_ignore_ascii_case("SHA-512") && !self.hash.is_empty() {
            format!(
                "<sheetProtection algorithmName=\"SHA-512\" hashValue=\"{}\" saltValue=\"{}\" spinCount=\"{}\" sheet=\"1\" objects=\"1\" scenarios=\"1\"/>",
                self.hash, self.salt, self.spin.max(1)
            )
        } else if let Some(h) = self.legacy {
            format!(
                "<sheetProtection password=\"{:04X}\" sheet=\"1\" objects=\"1\" scenarios=\"1\"/>",
                h
            )
        } else {
            "<sheetProtection sheet=\"1\" objects=\"1\" scenarios=\"1\"/>".into()
        }
    }
}

pub fn hash_password(pw: &str, salt: &[u8], spin: u32) -> Vec<u8> {
    let mut h = Sha512::new();
    h.update(salt);
    for unit in pw.encode_utf16() {
        h.update(unit.to_le_bytes());
    }
    let mut cur = h.finalize().to_vec();
    for i in 0..spin {
        let mut d = Sha512::new();
        d.update(&cur);
        d.update(i.to_le_bytes());
        cur = d.finalize().to_vec();
    }
    cur
}

/// 旧格式（BIFF8 / 早期 xlsx）的 16 位哈希
pub fn legacy_hash(pw: &str) -> u16 {
    let mut hash: u16 = 0;
    for ch in pw.chars() {
        hash ^= ch as u16;
        hash = ((hash >> 14) & 0x01) | ((hash << 1) & 0x7FFF);
    }
    hash ^= pw.chars().count() as u16;
    hash ^= 0xCE4B;
    hash
}

fn random_salt() -> Vec<u8> {
    // 盐只要求唯一，不要求保密：时间 + 进程 + 地址搅一遍再 SHA-512
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let stack = &nanos as *const _ as usize;
    let mut d = Sha512::new();
    d.update(nanos.to_le_bytes());
    d.update(pid.to_le_bytes());
    d.update(stack.to_le_bytes());
    d.finalize()[..16].to_vec()
}

/// 从 `<sheetProtection .../>` 标签里解析
pub fn parse_tag(tag: &str) -> Option<SheetProtection> {
    let attr = |name: &str| -> Option<String> {
        let pat = format!("{name}=\"");
        let i = tag.find(&pat)? + pat.len();
        let rest = &tag[i..];
        let j = rest.find('"')?;
        Some(rest[..j].to_string())
    };
    let algorithm = attr("algorithmName").unwrap_or_default();
    let hash = attr("hashValue").unwrap_or_default();
    let salt = attr("saltValue").unwrap_or_default();
    let spin = attr("spinCount").and_then(|v| v.parse().ok()).unwrap_or(0);
    let legacy = attr("password").and_then(|v| u16::from_str_radix(v.trim(), 16).ok());
    let sheet = attr("sheet").map(|v| v == "1" || v == "true").unwrap_or(true);
    if !sheet && hash.is_empty() && legacy.is_none() {
        return None;
    }
    Some(SheetProtection {
        algorithm,
        hash,
        salt,
        spin,
        legacy,
        sheet,
        objects: attr("objects").map(|v| v == "1").unwrap_or(true),
        scenarios: attr("scenarios").map(|v| v == "1").unwrap_or(true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha512_matches_excel_reference() {
        // 由 Excel 365 生成：密码 "test"，spinCount 100000
        let salt = base64::engine::general_purpose::STANDARD
            .decode("iSCmYeo9C/KCvdr/wWS9VA==")
            .unwrap();
        let want = base64::engine::general_purpose::STANDARD
            .decode("a8QwQr9Et7Bm+zSa+DePDv+zB8WnNaBU3WjBhu7km7s80y42RP7cR7bWsHWoh+wAz7pfvpzjym3mjpzHOBX6bg==")
            .unwrap();
        assert_eq!(hash_password("test", &salt, 100000), want);
    }

    #[test]
    fn protect_and_verify() {
        let p = SheetProtection::new_with_password("s3cret");
        assert!(p.verify("s3cret"));
        assert!(!p.verify("wrong"));
        let xml = p.to_xml();
        let parsed = parse_tag(&xml).expect("应能解析");
        assert!(parsed.verify("s3cret"));
        assert_eq!(parsed.algorithm, "SHA-512");
    }

    #[test]
    fn legacy_hash_is_stable() {
        // 同一口令两次结果一致，且长度异或参与
        assert_eq!(legacy_hash("abc"), legacy_hash("abc"));
        assert_ne!(legacy_hash("abc"), legacy_hash("abd"));
    }
}
