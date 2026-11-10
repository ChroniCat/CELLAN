//! Office 打开密码：ECMA-376 Agile Encryption（AES-256-CBC + SHA-512）。
//!
//! 文件是 CFB 容器，两个流：`EncryptionInfo`（版本头 + Agile XML）和
//! `EncryptedPackage`（8 字节明文长度 + 4096 字节分段的 AES-CBC 密文）。
//! 派生方式与 Excel 365 一致：
//!   H0 = SHA512(salt || UTF16LE(password))
//!   Hi = SHA512(H(i-1) || LE32(i))
//!   key = SHA512(H_final || blockKey)[..keyBytes]
//! 三段 blockKey：验证器输入、验证器哈希、包密钥。

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use sha2::{Digest, Sha512};
use std::io::{Read, Seek, Write};
use std::path::Path;

const BLOCK_VERIFIER_INPUT: [u8; 8] = [0xFE, 0xA7, 0xD2, 0x76, 0x3B, 0x4B, 0x9E, 0x79];
const BLOCK_VERIFIER_HASH: [u8; 8] = [0xD7, 0xAA, 0x0F, 0x6D, 0x30, 0x61, 0x34, 0x4E];
const BLOCK_KEY_VALUE: [u8; 8] = [0x14, 0x6E, 0x0B, 0xE7, 0xAB, 0xAC, 0xD0, 0xD6];
const BLOCK_HMAC_KEY: [u8; 8] = [0x5F, 0xB2, 0xAD, 0x01, 0x0C, 0xB9, 0xE1, 0xF6];
const BLOCK_HMAC_VALUE: [u8; 8] = [0xA0, 0x67, 0x7F, 0x02, 0xB2, 0x2C, 0x84, 0x33];
const SEGMENT: usize = 4096;
const SPIN: u32 = 100_000;

/// Excel 强加密要求 CFB 里有 \x06DataSpaces 结构；这些字节是 Excel 写出的固定模板
const DS_VERSION: &str = "PAAAAE0AaQBjAHIAbwBzAG8AZgB0AC4AQwBvAG4AdABhAGkAbgBlAHIALgBEAGEAdABhAFMAcABhAGMAZQBzAAEAAAABAAAAAQAAAA==";
const DS_MAP: &str = "CAAAAAEAAABoAAAAAQAAAAAAAAAgAAAARQBuAGMAcgB5AHAAdABlAGQAUABhAGMAawBhAGcAZQAyAAAAUwB0AHIAbwBuAGcARQBuAGMAcgB5AHAAdABpAG8AbgBEAGEAdABhAFMAcABhAGMAZQAAAA==";
const DS_STRONG: &str = "CAAAAAEAAAAyAAAAUwB0AHIAbwBuAGcARQBuAGMAcgB5AHAAdABpAG8AbgBUAHIAYQBuAHMAZgBvAHIAbQAAAA==";
const DS_PRIMARY: &str = "WAAAAAEAAABMAAAAewBGAEYAOQBBADMARgAwADMALQA1ADYARQBGAC0ANAA2ADEAMwAtAEIARABEADUALQA1AEEANAAxAEMAMQBEADAANwAyADQANgB9AE4AAABNAGkAYwByAG8AcwBvAGYAdAAuAEMAbwBuAHQAYQBpAG4AZQByAC4ARQBuAGMAcgB5AHAAdABpAG8AbgBUAHIAYQBuAHMAZgBvAHIAbQAAAAEAAAABAAAAAQAAAAAAAAAAAAAAAAAAAAQAAAA=";

/// 打开密码相关设置，跟着工作簿走，另存时继续加密。
#[derive(Clone)]
pub struct EncryptionSettings {
    pub password: String,
    pub spin: u32,
}

impl std::fmt::Debug for EncryptionSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionSettings")
            .field("password", &"<redacted>")
            .field("spin", &self.spin)
            .finish()
    }
}

impl EncryptionSettings {
    pub fn new(password: impl Into<String>) -> Self {
        Self { password: password.into(), spin: SPIN }
    }
}

/// 是否是 CFB 容器里的 Office 加密文件
pub fn needs_password(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    needs_password_bytes(&bytes)
}

pub fn needs_password_bytes(bytes: &[u8]) -> bool {
    if !bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return false;
    }
    // CFB 目录项名是 UTF-16LE
    let needle: Vec<u8> = "EncryptedPackage"
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    find_bytes(bytes, &needle).is_some()
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn b64(v: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(v.trim())
        .with_context(|| "base64 解码失败")
}

fn attr<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("{name}=\"");
    let i = xml.find(&pat)? + pat.len();
    let rest = &xml[i..];
    let q = rest.find('"')?;
    Some(&rest[..q])
}

/// 截取一个自闭合标签块（含属性），找不到返回 None
fn tag_block<'a>(xml: &'a str, start_tag: &str) -> Option<&'a str> {
    let i = xml.find(start_tag)?;
    let rest = &xml[i..];
    let end = rest.find("/>").map(|e| e + 2)?;
    Some(&rest[..end])
}

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

/// 迭代哈希到 H_final（spinCount 次）
fn hash_final(password: &str, salt: &[u8], spin: u32) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(salt);
    h.update(utf16le(password));
    let mut cur: [u8; 64] = h.finalize().into();
    for i in 0..spin {
        let mut d = Sha512::new();
        // 规范：H(pack("<I", i) || H_prev)，序号在前
        d.update(i.to_le_bytes());
        d.update(cur);
        cur = d.finalize().into();
    }
    cur
}

/// key = SHA512(H_final || blockKey) 的前 key_bytes 字节
fn derive_key(h_final: &[u8; 64], block: &[u8; 8], key_bytes: usize) -> Vec<u8> {
    let mut d = Sha512::new();
    d.update(h_final);
    d.update(block);
    d.finalize()[..key_bytes].to_vec()
}

/// AES-256-CBC 解密（不做 PKCS#7：Office 的分段是零填充）
fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{BlockCipherDecrypt, KeyInit};
    if key.len() != 32 || iv.len() != 16 {
        bail!("AES 密钥/IV 长度不对");
    }
    let cipher = aes::Aes256::new_from_slice(key).map_err(|e| anyhow!("AES key: {e}"))?;
    let mut out = Vec::with_capacity(data.len());
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);
    for chunk in data.chunks(16) {
        let mut block = [0u8; 16];
        block[..chunk.len()].copy_from_slice(chunk);
        let mut b = block.into();
        cipher.decrypt_block(&mut b);
        let plain = b;
        for i in 0..16 {
            out.push(plain[i] ^ prev[i]);
        }
        prev.copy_from_slice(&block);
    }
    Ok(out)
}

/// AES-256-CBC 加密（数据需 16 字节对齐）
fn aes_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{BlockCipherEncrypt, KeyInit};
    if key.len() != 32 || iv.len() != 16 {
        bail!("AES 密钥/IV 长度不对");
    }
    if data.len() % 16 != 0 {
        bail!("AES 明文未按 16 字节对齐");
    }
    let cipher = aes::Aes256::new_from_slice(key).map_err(|e| anyhow!("AES key: {e}"))?;
    let mut out = Vec::with_capacity(data.len());
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);
    for chunk in data.chunks(16) {
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = chunk[i] ^ prev[i];
        }
        let mut b = block.into();
        cipher.encrypt_block(&mut b);
        let cipher_block = b;
        out.extend_from_slice(&cipher_block);
        prev.copy_from_slice(&cipher_block);
    }
    Ok(out)
}

/// HMAC-SHA512（RFC 2104），密钥按 128 字节块补齐
fn hmac_sha512(key: &[u8], msg: &[u8]) -> [u8; 64] {
    let mut k = [0u8; 128];
    if key.len() > 128 {
        let mut d = Sha512::new();
        d.update(key);
        let h = d.finalize();
        k[..64].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 128];
    let mut opad = [0x5cu8; 128];
    for i in 0..128 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha512::new();
    inner.update(ipad);
    inner.update(msg);
    let ih = inner.finalize();
    let mut outer = Sha512::new();
    outer.update(opad);
    outer.update(ih);
    outer.finalize().into()
}

fn segment_iv(salt: &[u8], index: u32) -> [u8; 16] {
    let mut d = Sha512::new();
    d.update(salt);
    d.update(index.to_le_bytes());
    let h = d.finalize();
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&h[..16]);
    iv
}

/// 解出明文 xlsx 字节
pub fn decrypt(path: &Path, password: &str) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).with_context(|| format!("读取 {} 失败", path.display()))?;
    decrypt_bytes(&bytes, password)
}

pub fn decrypt_bytes(bytes: &[u8], password: &str) -> Result<Vec<u8>> {
    let mut cf = cfb::CompoundFile::open(std::io::Cursor::new(bytes.to_vec()))
        .map_err(|e| anyhow!("不是有效的 CFB 容器：{e}"))?;
    let mut info = Vec::new();
    cf.open_stream("EncryptionInfo")
        .map_err(|e| anyhow!("缺少 EncryptionInfo：{e}"))?
        .read_to_end(&mut info)?;
    let xml = parse_info(&info)?;
    if let Some(obj) = xml.cipher_algorithm.as_deref() {
        if !obj.eq_ignore_ascii_case("AES") {
            bail!("不支持的加密算法 {obj}");
        }
    }
    if xml.hash_algorithm != "SHA512" {
        bail!("不支持的哈希算法 {}", xml.hash_algorithm);
    }
    if xml.cipher_chaining.as_deref().is_none_or(|c| !c.eq_ignore_ascii_case("ChainingModeCBC")) {
        bail!("只支持 CBC 链接模式");
    }
    let salt_pkg = b64(&xml.salt_data)?;
    let salt = b64(&xml.salt_key)?;
    let key_bytes = (xml.key_bits / 8) as usize;
    let h = hash_final(password, &salt, xml.spin_count.max(1));
    let k_verifier = derive_key(&h, &BLOCK_VERIFIER_INPUT, key_bytes);
    let k_hash = derive_key(&h, &BLOCK_VERIFIER_HASH, key_bytes);
    let k_value = derive_key(&h, &BLOCK_KEY_VALUE, key_bytes);
    let verifier = aes_cbc_decrypt(&k_verifier, &salt, &b64(&xml.enc_verifier_input)?)?;
    let want_hash = aes_cbc_decrypt(&k_hash, &salt, &b64(&xml.enc_verifier_hash)?)?;
    let mut d = Sha512::new();
    d.update(&verifier);
    let got: [u8; 64] = d.finalize().into();
    if got[..] != want_hash[..64] {
        bail!("打开密码不正确");
    }
    let key = aes_cbc_decrypt(&k_value, &salt, &b64(&xml.enc_key_value)?)?;
    if key.len() < key_bytes {
        bail!("包密钥长度不足");
    }

    let mut pkg = Vec::new();
    cf.open_stream("EncryptedPackage")
        .map_err(|e| anyhow!("缺少 EncryptedPackage：{e}"))?
        .read_to_end(&mut pkg)?;
    if pkg.len() < 8 {
        bail!("EncryptedPackage 太短");
    }
    let mut size = [0u8; 8];
    size.copy_from_slice(&pkg[..8]);
    let size = u64::from_le_bytes(size) as usize;
    let mut out = Vec::with_capacity(size);
    for (i, seg) in pkg[8..].chunks(SEGMENT).enumerate() {
        let iv = segment_iv(&salt_pkg, i as u32);
        let plain = aes_cbc_decrypt(&key[..key_bytes], &iv, seg)?;
        out.extend_from_slice(&plain);
    }
    out.truncate(size);
    Ok(out)
}

struct InfoXml {
    /// keyData 的盐：包分段 IV
    salt_data: String,
    /// password keyEncryptor 的盐：口令派生/验证器
    salt_key: String,
    spin_count: u32,
    key_bits: u32,
    hash_algorithm: String,
    cipher_algorithm: Option<String>,
    cipher_chaining: Option<String>,
    enc_verifier_input: String,
    enc_verifier_hash: String,
    enc_key_value: String,
}

/// EncryptionInfo 流：8 字节版本头（4,4,0x40,0）+ XML
fn parse_info(bytes: &[u8]) -> Result<InfoXml> {
    let start = bytes
        .windows(5)
        .position(|w| w == b"<?xml")
        .or_else(|| bytes.windows(12).position(|w| w == b"<encryption "))
        .ok_or_else(|| anyhow!("EncryptionInfo 里找不到 XML"))?;
    let xml = std::str::from_utf8(&bytes[start..]).context("EncryptionInfo 不是 UTF-8")?;
    // keyData 管包加密，p:encryptedKey 管口令校验，两段盐不同
    let key_data = tag_block(xml, "<keyData").unwrap_or(xml);
    let enc_key = tag_block(xml, "<p:encryptedKey")
        .or_else(|| tag_block(xml, "<encryptedKey"))
        .ok_or_else(|| anyhow!("缺少 encryptedKey（口令加密段）"))?;
    let key_bits = attr(key_data, "keyBits").and_then(|v| v.parse().ok()).unwrap_or(256);
    let spin_count = attr(enc_key, "spinCount").and_then(|v| v.parse().ok()).unwrap_or(SPIN);
    let salt_data = attr(key_data, "saltValue")
        .ok_or_else(|| anyhow!("keyData 缺少 saltValue"))?
        .to_string();
    Ok(InfoXml {
        salt_data,
        salt_key: attr(enc_key, "saltValue")
            .ok_or_else(|| anyhow!("encryptedKey 缺少 saltValue"))?
            .into(),
        spin_count,
        key_bits,
        hash_algorithm: attr(enc_key, "hashAlgorithm")
            .unwrap_or("SHA512")
            .trim()
            .to_ascii_uppercase(),
        cipher_algorithm: attr(enc_key, "cipherAlgorithm").map(|s| s.to_string()),
        cipher_chaining: attr(enc_key, "cipherChaining").map(|s| s.to_string()),
        enc_verifier_input: attr(enc_key, "encryptedVerifierHashInput")
            .ok_or_else(|| anyhow!("缺少 encryptedVerifierHashInput"))?
            .into(),
        enc_verifier_hash: attr(enc_key, "encryptedVerifierHashValue")
            .ok_or_else(|| anyhow!("缺少 encryptedVerifierHashValue"))?
            .into(),
        enc_key_value: attr(enc_key, "encryptedKeyValue")
            .ok_or_else(|| anyhow!("缺少 encryptedKeyValue"))?
            .into(),
    })
}

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut v = vec![0u8; n];
    getrandom::fill(&mut v).map_err(|e| anyhow!("系统随机数失败：{e}"))?;
    Ok(v)
}

/// 把明文 xlsx 加密写成 CFB 容器（Agile，AES-256-CBC/SHA-512）
pub fn encrypt(path: &Path, plain: &[u8], settings: &EncryptionSettings) -> Result<()> {
    let salt = random_bytes(16)?;      // 口令派生/验证器
    let salt_pkg = random_bytes(16)?;  // 包分段 IV
    let verifier = random_bytes(16)?;
    let key = random_bytes(32)?;
    let spin = settings.spin.max(1);
    let h = hash_final(&settings.password, &salt, spin);
    let k_verifier = derive_key(&h, &BLOCK_VERIFIER_INPUT, 32);
    let k_hash = derive_key(&h, &BLOCK_VERIFIER_HASH, 32);
    let k_value = derive_key(&h, &BLOCK_KEY_VALUE, 32);
    let enc_verifier = aes_cbc_encrypt(&k_verifier, &salt, &verifier)?;
    let mut d = Sha512::new();
    d.update(&verifier);
    let vhash: [u8; 64] = d.finalize().into();
    let enc_hash = aes_cbc_encrypt(&k_hash, &salt, &vhash)?;
    let enc_key = aes_cbc_encrypt(&k_value, &salt, &key)?;
    // 包：8 字节长度 + 4096 字节分段
    let mut pkg = Vec::with_capacity(plain.len() + 32);
    pkg.extend_from_slice(&(plain.len() as u64).to_le_bytes());
    for (i, seg) in plain.chunks(SEGMENT).enumerate() {
        let mut block = seg.to_vec();
        let pad = (16 - block.len() % 16) % 16;
        block.resize(block.len() + pad, 0);
        let iv = segment_iv(&salt_pkg, i as u32);
        pkg.extend_from_slice(&aes_cbc_encrypt(&key, &iv, &block)?);
    }

    // 数据完整性：对「长度头 + 密文」做 HMAC-SHA512
    let hmac_key = random_bytes(64)?;
    let iv1 = {
        let mut d = Sha512::new();
        d.update(&salt_pkg);
        d.update(BLOCK_HMAC_KEY);
        d.finalize()[..16].to_vec()
    };
    let iv2 = {
        let mut d = Sha512::new();
        d.update(&salt_pkg);
        d.update(BLOCK_HMAC_VALUE);
        d.finalize()[..16].to_vec()
    };
    let enc_hmac_key = aes_cbc_encrypt(&key, &iv1, &hmac_key)?;
    let hmac_value = hmac_sha512(&hmac_key, &pkg);
    let enc_hmac_value = aes_cbc_encrypt(&key, &iv2, &hmac_value)?;
    let b64s = |v: &[u8]| base64::engine::general_purpose::STANDARD.encode(v);
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
<encryption xmlns=\"http://schemas.microsoft.com/office/2006/encryption\" \
xmlns:p=\"http://schemas.microsoft.com/office/2006/keyEncryptor/password\">\
<keyData saltSize=\"16\" blockSize=\"16\" keyBits=\"256\" hashSize=\"64\" \
cipherAlgorithm=\"AES\" cipherChaining=\"ChainingModeCBC\" hashAlgorithm=\"SHA512\" \
saltValue=\"{salt_pkg}\"/>\
<dataIntegrity encryptedHmacKey=\"{hmac_key}\" encryptedHmacValue=\"{hmac_value}\"/>\
<keyEncryptors><keyEncryptor uri=\"http://schemas.microsoft.com/office/2006/keyEncryptor/password\">\
<p:encryptedKey spinCount=\"{spin}\" saltSize=\"16\" blockSize=\"16\" keyBits=\"256\" hashSize=\"64\" \
cipherAlgorithm=\"AES\" cipherChaining=\"ChainingModeCBC\" hashAlgorithm=\"SHA512\" \
saltValue=\"{salt_key}\" encryptedVerifierHashInput=\"{evi}\" \
encryptedVerifierHashValue=\"{evh}\" encryptedKeyValue=\"{ekv}\"/>\
</keyEncryptor></keyEncryptors></encryption>",
        salt_pkg = b64s(&salt_pkg),
        hmac_key = b64s(&enc_hmac_key),
        hmac_value = b64s(&enc_hmac_value),
        salt_key = b64s(&salt),
        evi = b64s(&enc_verifier),
        evh = b64s(&enc_hash),
        ekv = b64s(&enc_key),
    );
    let mut info = vec![0x04u8, 0x00, 0x04, 0x00, 0x40, 0x00, 0x00, 0x00];
    info.extend_from_slice(xml.as_bytes());

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let tmp = path.with_extension(format!("enc-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    {
        let mut cf = cfb::create(&tmp).map_err(|e| anyhow!("创建 CFB 失败：{e}"))?;
        // DataSpaces：Excel 打开强加密文件必需
        let dec = |v: &str| b64(v).unwrap_or_default();
        cf.create_storage_all("\u{6}DataSpaces")?;
        cf.create_storage_all("\u{6}DataSpaces/DataSpaceInfo")?;
        cf.create_storage_all("\u{6}DataSpaces/TransformInfo")?;
        cf.create_storage_all("\u{6}DataSpaces/TransformInfo/StrongEncryptionTransform")?;
        for (name, data) in [
            ("\u{6}DataSpaces/Version", dec(DS_VERSION)),
            ("\u{6}DataSpaces/DataSpaceMap", dec(DS_MAP)),
            (
                "\u{6}DataSpaces/DataSpaceInfo/StrongEncryptionDataSpace",
                dec(DS_STRONG),
            ),
            (
                "\u{6}DataSpaces/TransformInfo/StrongEncryptionTransform/\u{6}Primary",
                dec(DS_PRIMARY),
            ),
        ] {
            let mut st = cf.create_stream(name)?;
            st.set_len(data.len() as u64)?;
            st.seek(std::io::SeekFrom::Start(0))?;
            st.write_all(&data)?;
        }
        {
            let mut s = cf.create_stream("EncryptionInfo")?;
            s.set_len(info.len() as u64)?;
            s.seek(std::io::SeekFrom::Start(0))?;
            s.write_all(&info)?;
        }
        {
            let mut s = cf.create_stream("EncryptedPackage")?;
            s.set_len(pkg.len() as u64)?;
            s.seek(std::io::SeekFrom::Start(0))?;
            s.write_all(&pkg)?;
        }
        cf.flush()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("写入 {} 失败", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_workbook_survives_save_and_reload() {
        let dir = std::env::temp_dir().join("sheets_native_crypto");
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join(format!("enc_plain_{}.xlsx", std::process::id()));
        let enc = dir.join(format!("enc_book_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&plain);
        let _ = std::fs::remove_file(&enc);
        let mut wb = crate::model::Workbook::new_blank();
        wb.sheets[0].set(0, 0, crate::model::Cell { value: crate::model::Value::Text("机密".into()), formula: None });
        crate::xlsx::save(&plain, &wb).unwrap();
        let bytes = std::fs::read(&plain).unwrap();
        encrypt(&enc, &bytes, &EncryptionSettings::new("pw-123")).unwrap();
        let mut back = crate::xlsx::load_with_password(&enc, "pw-123").unwrap();
        assert_eq!(back.sheets[0].value(0, 0).display(), "机密");
        assert!(back.encryption.is_some());
        back.sheets[0].set(1, 0, crate::model::Cell { value: crate::model::Value::Text("改过".into()), formula: None });
        let out = dir.join(format!("enc_out_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&out);
        crate::xlsx::save(&out, &back).unwrap();
        assert!(needs_password(&out));
        let again = crate::xlsx::load_with_password(&out, "pw-123").unwrap();
        assert_eq!(again.sheets[0].value(1, 0).display(), "改过");
    }

    #[test]
    fn agile_round_trip_and_wrong_password() {
        let dir = std::env::temp_dir().join("sheets_native_crypto");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("enc_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let plain = b"PK\x03\x04hello-office-encryption".to_vec();
        encrypt(&p, &plain, &EncryptionSettings::new("p@ss word")).unwrap();
        assert!(needs_password(&p));
        assert_eq!(decrypt(&p, "p@ss word").unwrap(), plain);
        let err = decrypt(&p, "wrong").unwrap_err().to_string();
        assert!(err.contains("密码"), "{err}");
    }
}
