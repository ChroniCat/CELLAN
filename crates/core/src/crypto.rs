//! Office 打开密码：同时支持 ECMA-376 Standard（Excel 2007，AES-128/192/256-ECB + SHA-1）
//! 与 ECMA-376 Agile（Excel 2010/2013/2016/365，AES-128/192/256-CBC + SHA-1/256/384/512 + HMAC 完整性校验）。
//!
//! 文件是 CFB 容器，两个流：`EncryptionInfo`（版本头 + 二进制 Standard 头或 Agile XML）和
//! `EncryptedPackage`（8 字节明文长度 + 分段密文）。

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use sha2::{Digest, Sha256, Sha384, Sha512};
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

/// 只读取文件头前 `magic.len()` 字节做魔数判断，避免将上百 MB 的工作簿整包读入内存。
pub fn has_magic(path: &Path, magic: &[u8]) -> bool {
    if magic.is_empty() {
        return true;
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut stack = [0u8; 16];
    if magic.len() <= stack.len() {
        let buf = &mut stack[..magic.len()];
        return f.read_exact(buf).is_ok() && buf == magic;
    }
    let mut buf = vec![0u8; magic.len()];
    f.read_exact(&mut buf).is_ok() && buf.as_slice() == magic
}

/// 是否是 CFB 容器里的 Office 加密文件（按需读 CFB 头与目录扇区，不整包扫描）
pub fn needs_password(path: &Path) -> bool {
    crate::cfb::has_encrypted_package_file(path)
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

const SHA512_IV: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];

const PARALLEL_THRESHOLD: usize = 64 * 1024;

/// 迭代哈希到 H_final（spinCount 次）。
///
/// 热循环每轮固定输入 68 字节 `LE32(i) || H_prev(64B)`，恰好装入单个 128 字节 SHA-512 块，
/// 直接调用 `sha2::block_api::compress512` 消除每轮 `Sha512::new()/update()/finalize()` 抽象与拷贝开销。
fn hash_final(password: &str, salt: &[u8], spin: u32) -> [u8; 64] {
    use sha2::block_api::compress512;
    let mut h = Sha512::new();
    h.update(salt);
    h.update(utf16le(password));
    let init: [u8; 64] = h.finalize().into();

    let mut block = [0u8; 128];
    block[4..68].copy_from_slice(&init);
    block[68] = 0x80;
    block[120..128].copy_from_slice(&(68u64 * 8).to_be_bytes());
    for i in 0..spin {
        block[0..4].copy_from_slice(&i.to_le_bytes());
        let mut st = SHA512_IV;
        compress512(&mut st, &[block]);
        for k in 0..8 {
            block[4 + k * 8..12 + k * 8].copy_from_slice(&st[k].to_be_bytes());
        }
    }
    let mut cur = [0u8; 64];
    cur.copy_from_slice(&block[4..68]);
    cur
}

/// key = SHA512(H_final || blockKey) 的前 key_bytes 字节
fn derive_key(h_final: &[u8; 64], block: &[u8; 8], key_bytes: usize) -> Vec<u8> {
    let mut d = Sha512::new();
    d.update(h_final);
    d.update(block);
    d.finalize()[..key_bytes].to_vec()
}

/// AES-256-CBC 解密（小载荷单次调用；不做 PKCS#7，Office 分段为零填充）
fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::KeyInit;
    if key.len() != 32 || iv.len() != 16 {
        bail!("AES 密钥/IV 长度不对");
    }
    let cipher = aes::Aes256::new_from_slice(key).map_err(|e| anyhow!("AES key: {e}"))?;
    let iv_arr: [u8; 16] = iv.try_into().unwrap();
    let padded = data.len().div_ceil(16) * 16;
    let mut out = vec![0u8; padded];
    decrypt_chunk_into(&cipher, &iv_arr, data, &mut out);
    Ok(out)
}

/// 复用已展开轮密钥的 `Aes256` 对一段（最多 4 KB）做批量 CBC 解密（利用 `decrypt_blocks` AES-NI 流水线）。
fn decrypt_chunk_into(cipher: &aes::Aes256, iv: &[u8; 16], src: &[u8], dst: &mut [u8]) {
    use aes::cipher::{Array, BlockCipherDecrypt};
    if src.is_empty() {
        return;
    }
    // 常规 4 KB 分段走栈缓冲零堆分配；若传入超过 4 KB 则按 4 KB 分批
    let mut prev = u128::from_ne_bytes(*iv);
    let mut off = 0usize;
    let mut blocks = [Array::default(); SEGMENT / 16];
    while off < src.len() {
        let take = (src.len() - off).min(SEGMENT);
        let chunk = &src[off..off + take];
        let full = take / 16;
        let rem = take % 16;
        let n_blocks = full + usize::from(rem != 0);
        for i in 0..full {
            blocks[i].as_mut_slice().copy_from_slice(&chunk[i * 16..(i + 1) * 16]);
        }
        if rem != 0 {
            let b = blocks[full].as_mut_slice();
            b[..rem].copy_from_slice(&chunk[full * 16..]);
            b[rem..].fill(0);
        }
        cipher.decrypt_blocks(&mut blocks[..n_blocks]);
        for i in 0..full {
            let ct = u128::from_ne_bytes(chunk[i * 16..(i + 1) * 16].try_into().unwrap());
            let pt = u128::from_ne_bytes(blocks[i].into()) ^ prev;
            dst[off + i * 16..off + (i + 1) * 16].copy_from_slice(&pt.to_ne_bytes());
            prev = ct;
        }
        if rem != 0 {
            let pt = u128::from_ne_bytes(blocks[full].into()) ^ prev;
            dst[off + full * 16..off + (full + 1) * 16].copy_from_slice(&pt.to_ne_bytes());
            let mut last_ct = [0u8; 16];
            last_ct[..rem].copy_from_slice(&chunk[full * 16..]);
            prev = u128::from_ne_bytes(last_ct);
        }
        off += take;
    }
}

/// 复用已展开轮密钥的 `Aes256` 对一段做零分配 CBC 加密（不足 16 字节尾部自动补 0）。
fn encrypt_chunk_into(cipher: &aes::Aes256, iv: &[u8; 16], src: &[u8], dst: &mut [u8]) {
    use aes::cipher::{Array, BlockCipherEncrypt};
    let full = src.len() / 16;
    let rem = src.len() % 16;
    let mut prev = u128::from_ne_bytes(*iv);
    for i in 0..full {
        let pt = u128::from_ne_bytes(src[i * 16..(i + 1) * 16].try_into().unwrap());
        let mut b = Array::from((pt ^ prev).to_ne_bytes());
        cipher.encrypt_block(&mut b);
        let ct: [u8; 16] = b.into();
        dst[i * 16..(i + 1) * 16].copy_from_slice(&ct);
        prev = u128::from_ne_bytes(ct);
    }
    if rem != 0 {
        let mut tail = [0u8; 16];
        tail[..rem].copy_from_slice(&src[full * 16..]);
        let pt = u128::from_ne_bytes(tail);
        let mut b = Array::from((pt ^ prev).to_ne_bytes());
        cipher.encrypt_block(&mut b);
        let ct: [u8; 16] = b.into();
        dst[full * 16..(full + 1) * 16].copy_from_slice(&ct);
    }
}

/// AES-256-CBC 加密（小载荷单次调用，数据需 16 字节对齐）
fn aes_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::KeyInit;
    if key.len() != 32 || iv.len() != 16 {
        bail!("AES 密钥/IV 长度不对");
    }
    if data.len() % 16 != 0 {
        bail!("AES 明文未按 16 字节对齐");
    }
    let cipher = aes::Aes256::new_from_slice(key).map_err(|e| anyhow!("AES key: {e}"))?;
    let iv_arr: [u8; 16] = iv.try_into().unwrap();
    let mut out = vec![0u8; data.len()];
    encrypt_chunk_into(&cipher, &iv_arr, data, &mut out);
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

struct SegmentIvGen<'a> {
    salt: &'a [u8],
    template: Option<[u8; 128]>,
}

impl<'a> SegmentIvGen<'a> {
    fn new(salt: &'a [u8]) -> Self {
        let template = if salt.len() <= 107 {
            let mut block = [0u8; 128];
            block[..salt.len()].copy_from_slice(salt);
            block[salt.len() + 4] = 0x80;
            let bit_len = ((salt.len() + 4) as u64) * 8;
            block[120..128].copy_from_slice(&bit_len.to_be_bytes());
            Some(block)
        } else {
            None
        };
        Self { salt, template }
    }

    #[inline]
    fn iv(&self, index: u32) -> [u8; 16] {
        if let Some(mut block) = self.template {
            let off = self.salt.len();
            block[off..off + 4].copy_from_slice(&index.to_le_bytes());
            let mut st = SHA512_IV;
            sha2::block_api::compress512(&mut st, &[block]);
            let mut iv = [0u8; 16];
            iv[..8].copy_from_slice(&st[0].to_be_bytes());
            iv[8..16].copy_from_slice(&st[1].to_be_bytes());
            iv
        } else {
            segment_iv(self.salt, index)
        }
    }
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

fn segment_workers(payload_len: usize, num_segments: usize) -> usize {
    if payload_len < PARALLEL_THRESHOLD || num_segments < 16 {
        return 1;
    }
    let avail = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    avail.min(8).min(num_segments / 8).max(1)
}

/// 哈希算法枚举（兼容 Agile SHA1/SHA256/SHA384/SHA512 及 Standard SHA1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HashAlg {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl HashAlg {
    fn from_name(name: &str) -> Result<Self> {
        match name.trim().to_ascii_uppercase().as_str() {
            "SHA1" | "SHA-1" => Ok(Self::Sha1),
            "SHA256" | "SHA-256" => Ok(Self::Sha256),
            "SHA384" | "SHA-384" => Ok(Self::Sha384),
            "SHA512" | "SHA-512" => Ok(Self::Sha512),
            other => bail!("不支持的哈希算法 {other}"),
        }
    }

    fn digest_len(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }

    fn block_len(self) -> usize {
        match self {
            Self::Sha1 | Self::Sha256 => 64,
            Self::Sha384 | Self::Sha512 => 128,
        }
    }

    fn hash_slices(self, parts: &[&[u8]]) -> Vec<u8> {
        match self {
            Self::Sha1 => sha1_slices(parts).to_vec(),
            Self::Sha256 => {
                let mut d = Sha256::new();
                for p in parts {
                    d.update(p);
                }
                d.finalize().to_vec()
            }
            Self::Sha384 => {
                let mut d = Sha384::new();
                for p in parts {
                    d.update(p);
                }
                d.finalize().to_vec()
            }
            Self::Sha512 => {
                let mut d = Sha512::new();
                for p in parts {
                    d.update(p);
                }
                d.finalize().to_vec()
            }
        }
    }
}

/// 纯 Rust FIPS 180-4 SHA-1（离线构建零新增依赖，用于 Excel 2007 Standard 与 2010 Agile-SHA1 解密）。
fn sha1_slices(parts: &[&[u8]]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let total_len: u64 = parts.iter().map(|p| p.len() as u64).sum();
    let mut buf = [0u8; 64];
    let mut used = 0usize;
    let compress = |h: &mut [u32; 5], blk: &[u8; 64]| {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                blk[i * 4],
                blk[i * 4 + 1],
                blk[i * 4 + 2],
                blk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    };
    for part in parts {
        let mut off = 0usize;
        while off < part.len() {
            let take = (64 - used).min(part.len() - off);
            buf[used..used + take].copy_from_slice(&part[off..off + take]);
            used += take;
            off += take;
            if used == 64 {
                compress(&mut h, &buf);
                used = 0;
            }
        }
    }
    buf[used] = 0x80;
    used += 1;
    if used > 56 {
        buf[used..].fill(0);
        compress(&mut h, &buf);
        used = 0;
    }
    buf[used..56].fill(0);
    buf[56..64].copy_from_slice(&(total_len * 8).to_be_bytes());
    compress(&mut h, &buf);
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// 通用 RFC 2104 HMAC（支持 SHA1 / SHA256 / SHA384 / SHA512）。
fn hmac_generic(alg: HashAlg, key: &[u8], msg: &[u8]) -> Vec<u8> {
    let bl = alg.block_len();
    let mut k = vec![0u8; bl];
    if key.len() > bl {
        let h = alg.hash_slices(&[key]);
        k[..h.len()].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = vec![0x36u8; bl];
    let mut opad = vec![0x5cu8; bl];
    for i in 0..bl {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let ih = alg.hash_slices(&[&ipad, msg]);
    alg.hash_slices(&[&opad, &ih])
}

/// Agile 通用迭代哈希（SHA-512 走 `compress512` 极速路径，SHA-1/256/384 走通用路径）。
fn hash_final_alg(alg: HashAlg, password: &str, salt: &[u8], spin: u32) -> Vec<u8> {
    if alg == HashAlg::Sha512 {
        return hash_final(password, salt, spin).to_vec();
    }
    let pw = utf16le(password);
    let mut cur = alg.hash_slices(&[salt, &pw]);
    for i in 0..spin {
        let le = i.to_le_bytes();
        cur = alg.hash_slices(&[&le, &cur]);
    }
    cur
}

/// ECMA-376 §2.3.4.11：key = Hash(H_final || blockKey)，不足 key_bytes 补 `0x36`，超出截断。
fn derive_key_alg(alg: HashAlg, h_final: &[u8], block: &[u8], key_bytes: usize) -> Vec<u8> {
    let mut h = alg.hash_slices(&[h_final, block]);
    if h.len() < key_bytes {
        h.resize(key_bytes, 0x36);
    } else {
        h.truncate(key_bytes);
    }
    h
}

/// 通用 Agile 分段 IV：`Hash(salt || extra)`，不足 16 字节补 `0x36`，超出截断到 16 字节。
fn derive_iv_alg(alg: HashAlg, salt: &[u8], extra: &[u8]) -> [u8; 16] {
    let mut h = alg.hash_slices(&[salt, extra]);
    if h.len() < 16 {
        h.resize(16, 0x36);
    }
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&h[..16]);
    iv
}

/// 通用 AES-128/192/256-CBC 解密（Office 分段零填充）。
fn aes_cbc_decrypt_any(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{Array, BlockCipherDecrypt, KeyInit};
    if iv.len() != 16 {
        bail!("AES IV 长度不对");
    }
    if key.len() == 32 {
        return aes_cbc_decrypt(key, iv, data);
    }
    let iv_arr: [u8; 16] = iv.try_into().unwrap();
    let padded = data.len().div_ceil(16) * 16;
    let mut out = vec![0u8; padded];
    let mut prev = u128::from_ne_bytes(iv_arr);
    let mut run = |dec: &dyn Fn(&mut Array<u8, _>), dst: &mut [u8]| {
        let full = data.len() / 16;
        let rem = data.len() % 16;
        for i in 0..full {
            let chunk: [u8; 16] = data[i * 16..(i + 1) * 16].try_into().unwrap();
            let mut b = Array::from(chunk);
            dec(&mut b);
            let pt = u128::from_ne_bytes(b.into()) ^ prev;
            dst[i * 16..(i + 1) * 16].copy_from_slice(&pt.to_ne_bytes());
            prev = u128::from_ne_bytes(chunk);
        }
        if rem != 0 {
            let mut last = [0u8; 16];
            last[..rem].copy_from_slice(&data[full * 16..]);
            let mut b = Array::from(last);
            dec(&mut b);
            let pt = u128::from_ne_bytes(b.into()) ^ prev;
            dst[full * 16..(full + 1) * 16].copy_from_slice(&pt.to_ne_bytes());
        }
    };
    match key.len() {
        16 => {
            let c = aes::Aes128::new_from_slice(key).map_err(|e| anyhow!("AES-128 key: {e}"))?;
            run(&|b| c.decrypt_block(b), &mut out);
        }
        24 => {
            let c = aes::Aes192::new_from_slice(key).map_err(|e| anyhow!("AES-192 key: {e}"))?;
            run(&|b| c.decrypt_block(b), &mut out);
        }
        other => bail!("不支持的 AES 密钥长度 {other}"),
    }
    Ok(out)
}

/// 通用 AES-128/192/256-CBC 加密（需 16 字节对齐，用于测试构造多种 Agile 加密包）。
fn aes_cbc_encrypt_any(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{Array, BlockCipherEncrypt, KeyInit};
    if iv.len() != 16 {
        bail!("AES IV 长度不对");
    }
    if key.len() == 32 && data.len() % 16 == 0 {
        return aes_cbc_encrypt(key, iv, data);
    }
    let padded = data.len().div_ceil(16) * 16;
    let mut out = vec![0u8; padded];
    let mut prev = u128::from_ne_bytes(iv.try_into().unwrap());
    let mut run = |enc: &dyn Fn(&mut Array<u8, _>), dst: &mut [u8]| {
        let full = data.len() / 16;
        let rem = data.len() % 16;
        for i in 0..full {
            let pt = u128::from_ne_bytes(data[i * 16..(i + 1) * 16].try_into().unwrap());
            let mut b = Array::from((pt ^ prev).to_ne_bytes());
            enc(&mut b);
            let ct: [u8; 16] = b.into();
            dst[i * 16..(i + 1) * 16].copy_from_slice(&ct);
            prev = u128::from_ne_bytes(ct);
        }
        if rem != 0 {
            let mut tail = [0u8; 16];
            tail[..rem].copy_from_slice(&data[full * 16..]);
            let pt = u128::from_ne_bytes(tail);
            let mut b = Array::from((pt ^ prev).to_ne_bytes());
            enc(&mut b);
            let ct: [u8; 16] = b.into();
            dst[full * 16..(full + 1) * 16].copy_from_slice(&ct);
        }
    };
    match key.len() {
        16 => {
            let c = aes::Aes128::new_from_slice(key).map_err(|e| anyhow!("AES-128 key: {e}"))?;
            run(&|b| c.encrypt_block(b), &mut out);
        }
        24 => {
            let c = aes::Aes192::new_from_slice(key).map_err(|e| anyhow!("AES-192 key: {e}"))?;
            run(&|b| c.encrypt_block(b), &mut out);
        }
        32 => {
            let c = aes::Aes256::new_from_slice(key).map_err(|e| anyhow!("AES-256 key: {e}"))?;
            run(&|b| c.encrypt_block(b), &mut out);
        }
        other => bail!("不支持的 AES 密钥长度 {other}"),
    }
    Ok(out)
}

/// AES-128/192/256-ECB 解密（用于 Excel 2007 Standard Encryption）。
fn aes_ecb_decrypt_any(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{Array, BlockCipherDecrypt, KeyInit};
    let blocks = data.len() / 16;
    let mut out = vec![0u8; blocks * 16];
    let mut run = |dec: &dyn Fn(&mut Array<u8, _>), dst: &mut [u8]| {
        for i in 0..blocks {
            let mut b = Array::from(<[u8; 16]>::try_from(&data[i * 16..(i + 1) * 16]).unwrap());
            dec(&mut b);
            let pt: [u8; 16] = b.into();
            dst[i * 16..(i + 1) * 16].copy_from_slice(&pt);
        }
    };
    match key.len() {
        16 => {
            let c = aes::Aes128::new_from_slice(key).map_err(|e| anyhow!("AES-128 key: {e}"))?;
            run(&|b| c.decrypt_block(b), &mut out);
        }
        24 => {
            let c = aes::Aes192::new_from_slice(key).map_err(|e| anyhow!("AES-192 key: {e}"))?;
            run(&|b| c.decrypt_block(b), &mut out);
        }
        32 => {
            let c = aes::Aes256::new_from_slice(key).map_err(|e| anyhow!("AES-256 key: {e}"))?;
            run(&|b| c.decrypt_block(b), &mut out);
        }
        other => bail!("不支持的 Standard AES 密钥长度 {other}"),
    }
    Ok(out)
}

fn aes_ecb_encrypt_any(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{Array, BlockCipherEncrypt, KeyInit};
    let padded = data.len().div_ceil(16) * 16;
    let mut out = vec![0u8; padded];
    let mut run = |enc: &dyn Fn(&mut Array<u8, _>), dst: &mut [u8]| {
        let blocks = padded / 16;
        for i in 0..blocks {
            let mut blk = [0u8; 16];
            let s = i * 16;
            let e = (s + 16).min(data.len());
            if e > s {
                blk[..e - s].copy_from_slice(&data[s..e]);
            }
            let mut b = Array::from(blk);
            enc(&mut b);
            let ct: [u8; 16] = b.into();
            dst[s..s + 16].copy_from_slice(&ct);
        }
    };
    match key.len() {
        16 => {
            let c = aes::Aes128::new_from_slice(key).map_err(|e| anyhow!("AES-128 key: {e}"))?;
            run(&|b| c.encrypt_block(b), &mut out);
        }
        24 => {
            let c = aes::Aes192::new_from_slice(key).map_err(|e| anyhow!("AES-192 key: {e}"))?;
            run(&|b| c.encrypt_block(b), &mut out);
        }
        32 => {
            let c = aes::Aes256::new_from_slice(key).map_err(|e| anyhow!("AES-256 key: {e}"))?;
            run(&|b| c.encrypt_block(b), &mut out);
        }
        other => bail!("不支持的 Standard AES 密钥长度 {other}"),
    }
    Ok(out)
}

/// 判断 `EncryptionInfo` 流是否为 ECMA-376 Standard Encryption（Excel 2007，minor == 2）。
fn is_standard_encryption_info(info: &[u8]) -> bool {
    if info.len() < 8 {
        return false;
    }
    let major = u16::from_le_bytes([info[0], info[1]]);
    let minor = u16::from_le_bytes([info[2], info[3]]);
    minor == 2 && matches!(major, 2 | 3 | 4)
}

/// ECMA-376 §2.3.4.7 Standard Encryption 密钥派生与口令校验（AES-128/192/256-ECB + SHA-1 50,000 轮）。
fn verify_and_extract_standard_key(info: &[u8], password: &str) -> Result<Vec<u8>> {
    if info.len() < 48 {
        bail!("Standard EncryptionInfo 过短");
    }
    let rd_u32 = |off: usize| -> Result<u32> {
        let s = info
            .get(off..off + 4)
            .ok_or_else(|| anyhow!("Standard EncryptionInfo 越界"))?;
        Ok(u32::from_le_bytes(s.try_into().unwrap()))
    };
    let header_size = rd_u32(8)? as usize;
    let header_start = 12usize;
    let verifier_start = header_start
        .checked_add(header_size)
        .ok_or_else(|| anyhow!("Standard EncryptionHeader 长度溢出"))?;
    if header_size < 32 || verifier_start + 4 + 16 + 16 + 4 + 32 > info.len() {
        bail!("Standard EncryptionInfo 结构损坏");
    }
    let alg_id = rd_u32(header_start + 8)?;
    let alg_id_hash = rd_u32(header_start + 12)?;
    let key_size_bits = rd_u32(header_start + 16)?;
    let key_bytes = match (alg_id, key_size_bits) {
        (0x0000_660E, _) | (0, 0 | 128) => 16usize,
        (0x0000_660F, _) | (0, 192) => 24usize,
        (0x0000_6610, _) | (0, 256) => 32usize,
        _ => bail!("不支持的 Standard 加密算法 0x{alg_id:08X} ({key_size_bits} bit)"),
    };
    if alg_id_hash != 0 && alg_id_hash != 0x0000_8004 {
        bail!("不支持的 Standard 哈希算法 0x{alg_id_hash:08X}");
    }
    let salt_size = rd_u32(verifier_start)? as usize;
    if salt_size != 16 {
        bail!("Standard saltSize={salt_size} 非标准 16 字节");
    }
    let salt = &info[verifier_start + 4..verifier_start + 20];
    let enc_verifier = &info[verifier_start + 20..verifier_start + 36];
    let hash_size = rd_u32(verifier_start + 36)? as usize;
    let enc_verifier_hash = &info[verifier_start + 40..verifier_start + 72];
    if hash_size != 20 {
        bail!("Standard verifierHashSize={hash_size} 非 SHA-1");
    }

    // Standard KDF: H0 = SHA1(salt || UTF16LE(pw)); 50000 rounds Hi = SHA1(LE32(i) || H_{i-1})
    let pw = utf16le(password);
    let mut h = sha1_slices(&[salt, &pw]);
    for i in 0..50_000u32 {
        let le = i.to_le_bytes();
        h = sha1_slices(&[&le, &h]);
    }
    let zero = 0u32.to_le_bytes();
    let h_final = sha1_slices(&[&h, &zero]);
    let mut b1 = [0x36u8; 64];
    let mut b2 = [0x5Cu8; 64];
    for i in 0..20 {
        b1[i] ^= h_final[i];
        b2[i] ^= h_final[i];
    }
    let x1 = sha1_slices(&[&b1]);
    let x2 = sha1_slices(&[&b2]);
    let mut x = [0u8; 40];
    x[..20].copy_from_slice(&x1);
    x[20..].copy_from_slice(&x2);
    let key = x[..key_bytes].to_vec();

    // 验证口令
    let verifier = aes_ecb_decrypt_any(&key, enc_verifier)?;
    let dec_hash = aes_ecb_decrypt_any(&key, enc_verifier_hash)?;
    let got_hash = sha1_slices(&[&verifier]);
    if got_hash[..] != dec_hash[..20] {
        bail!("打开密码不正确");
    }
    Ok(key)
}

/// 构造 Excel 2007 Standard Encryption（AES-128-ECB + SHA-1）CFB 字节流（用于兼容性测试与互通验证）。
pub fn encrypt_standard_bytes(plain: &[u8], password: &str) -> Result<Vec<u8>> {
    let salt = random_bytes(16)?;
    let verifier = random_bytes(16)?;
    let pw = utf16le(password);
    let mut h = sha1_slices(&[&salt, &pw]);
    for i in 0..50_000u32 {
        let le = i.to_le_bytes();
        h = sha1_slices(&[&le, &h]);
    }
    let h_final = sha1_slices(&[&h, &0u32.to_le_bytes()]);
    let mut b1 = [0x36u8; 64];
    let mut b2 = [0x5Cu8; 64];
    for i in 0..20 {
        b1[i] ^= h_final[i];
        b2[i] ^= h_final[i];
    }
    let x1 = sha1_slices(&[&b1]);
    let x2 = sha1_slices(&[&b2]);
    let mut x = [0u8; 40];
    x[..20].copy_from_slice(&x1);
    x[20..].copy_from_slice(&x2);
    let key = &x[..16];

    let enc_verifier = aes_ecb_encrypt_any(key, &verifier)?;
    let vhash = sha1_slices(&[&verifier]);
    let mut vhash_padded = [0u8; 32];
    vhash_padded[..20].copy_from_slice(&vhash);
    let enc_vhash = aes_ecb_encrypt_any(key, &vhash_padded)?;

    // EncryptionInfo: version(3,2) + flags(0x24) + headerSize + EncryptionHeader + EncryptionVerifier
    let csp = utf16le("Microsoft Enhanced RSA and AES Cryptographic Provider\0");
    let header_size = (32 + csp.len()) as u32;
    let mut info = Vec::new();
    info.extend_from_slice(&3u16.to_le_bytes()); // major
    info.extend_from_slice(&2u16.to_le_bytes()); // minor
    info.extend_from_slice(&0x0000_0024u32.to_le_bytes()); // fCryptoAPI | fAES
    info.extend_from_slice(&header_size.to_le_bytes());
    info.extend_from_slice(&0x0000_0024u32.to_le_bytes()); // flags
    info.extend_from_slice(&0u32.to_le_bytes()); // sizeExtra
    info.extend_from_slice(&0x0000_660Eu32.to_le_bytes()); // CALG_AES_128
    info.extend_from_slice(&0x0000_8004u32.to_le_bytes()); // CALG_SHA1
    info.extend_from_slice(&128u32.to_le_bytes()); // keySize
    info.extend_from_slice(&24u32.to_le_bytes()); // providerType (PROV_RSA_AES)
    info.extend_from_slice(&0u32.to_le_bytes()); // reserved1
    info.extend_from_slice(&0u32.to_le_bytes()); // reserved2
    info.extend_from_slice(&csp);
    info.extend_from_slice(&16u32.to_le_bytes()); // saltSize
    info.extend_from_slice(&salt);
    info.extend_from_slice(&enc_verifier);
    info.extend_from_slice(&20u32.to_le_bytes()); // verifierHashSize
    info.extend_from_slice(&enc_vhash);

    let enc_pkg = aes_ecb_encrypt_any(key, plain)?;
    let mut pkg = Vec::with_capacity(8 + enc_pkg.len());
    pkg.extend_from_slice(&(plain.len() as u64).to_le_bytes());
    pkg.extend_from_slice(&enc_pkg);

    let mut cur = std::io::Cursor::new(Vec::new());
    {
        let mut cf = cfb::CompoundFile::create(&mut cur)?;
        cf.create_stream("EncryptionInfo")?.write_all(&info)?;
        cf.create_stream("EncryptedPackage")?.write_all(&pkg)?;
        cf.flush()?;
    }
    Ok(cur.into_inner())
}

/// 构造自定义算法组合（AES-128/192/256-CBC + SHA1/SHA256/SHA384/SHA512 + dataIntegrity HMAC）的 Agile 加密包。
pub fn encrypt_agile_bytes_with(
    plain: &[u8],
    password: &str,
    key_bits: u32,
    hash_name: &str,
    spin: u32,
) -> Result<Vec<u8>> {
    let alg = HashAlg::from_name(hash_name)?;
    let key_bytes = (key_bits / 8) as usize;
    let hash_bytes = alg.digest_len();
    let salt = random_bytes(16)?;
    let salt_pkg = random_bytes(16)?;
    let verifier = random_bytes(16)?;
    let key = random_bytes(key_bytes)?;
    let hmac_key = random_bytes(hash_bytes)?;
    let spin = spin.max(1);

    let h = hash_final_alg(alg, password, &salt, spin);
    let k_vi = derive_key_alg(alg, &h, &BLOCK_VERIFIER_INPUT, key_bytes);
    let k_vh = derive_key_alg(alg, &h, &BLOCK_VERIFIER_HASH, key_bytes);
    let k_kv = derive_key_alg(alg, &h, &BLOCK_KEY_VALUE, key_bytes);
    let enc_vi = aes_cbc_encrypt_any(&k_vi, &salt, &verifier)?;
    let mut vhash = alg.hash_slices(&[&verifier]);
    let vhash_pad = vhash.len().div_ceil(16) * 16;
    vhash.resize(vhash_pad, 0);
    let enc_vh = aes_cbc_encrypt_any(&k_vh, &salt, &vhash)?;
    let enc_kv = aes_cbc_encrypt_any(&k_kv, &salt, &key)?;

    let mut pkg = Vec::with_capacity(8 + plain.len() + 16);
    pkg.extend_from_slice(&(plain.len() as u64).to_le_bytes());
    for (i, seg) in plain.chunks(SEGMENT).enumerate() {
        let iv = derive_iv_alg(alg, &salt_pkg, &(i as u32).to_le_bytes());
        let ct = aes_cbc_encrypt_any(&key, &iv, seg)?;
        pkg.extend_from_slice(&ct);
    }

    let hmac_val = hmac_generic(alg, &hmac_key, &pkg);
    let mut hk_pad = hmac_key.clone();
    let hk_pad_len = hk_pad.len().div_ceil(16) * 16;
    hk_pad.resize(hk_pad_len, 0);
    let mut hv_pad = hmac_val.clone();
    let hv_pad_len = hv_pad.len().div_ceil(16) * 16;
    hv_pad.resize(hv_pad_len, 0);
    let enc_hk = aes_cbc_encrypt_any(&key, &derive_iv_alg(alg, &salt_pkg, &BLOCK_HMAC_KEY), &hk_pad)?;
    let enc_hv = aes_cbc_encrypt_any(&key, &derive_iv_alg(alg, &salt_pkg, &BLOCK_HMAC_VALUE), &hv_pad)?;

    let b = |v: &[u8]| base64::engine::general_purpose::STANDARD.encode(v);
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<encryption xmlns=\"http://schemas.microsoft.com/office/2006/encryption\" \
xmlns:p=\"http://schemas.microsoft.com/office/2006/keyEncryptor/password\">\
<keyData saltSize=\"16\" blockSize=\"16\" keyBits=\"{key_bits}\" hashSize=\"{hash_bytes}\" \
cipherAlgorithm=\"AES\" cipherChaining=\"ChainingModeCBC\" hashAlgorithm=\"{hash_name}\" \
saltValue=\"{}\"/>\
<dataIntegrity encryptedHmacKey=\"{}\" encryptedHmacValue=\"{}\"/>\
<keyEncryptors><keyEncryptor uri=\"http://schemas.microsoft.com/office/2006/keyEncryptor/password\">\
<p:encryptedKey spinCount=\"{spin}\" saltSize=\"16\" blockSize=\"16\" keyBits=\"{key_bits}\" \
hashSize=\"{hash_bytes}\" cipherAlgorithm=\"AES\" cipherChaining=\"ChainingModeCBC\" \
hashAlgorithm=\"{hash_name}\" saltValue=\"{}\" encryptedVerifierHashInput=\"{}\" \
encryptedVerifierHashValue=\"{}\" encryptedKeyValue=\"{}\"/>\
</keyEncryptor></keyEncryptors></encryption>",
        b(&salt_pkg),
        b(&enc_hk),
        b(&enc_hv),
        b(&salt),
        b(&enc_vi),
        b(&enc_vh),
        b(&enc_kv)
    );
    let mut info = Vec::with_capacity(8 + xml.len());
    info.extend_from_slice(&[4, 0, 4, 0, 0x40, 0, 0, 0]);
    info.extend_from_slice(xml.as_bytes());

    let mut cur = std::io::Cursor::new(Vec::new());
    {
        let mut cf = cfb::CompoundFile::create(&mut cur)?;
        cf.create_stream("EncryptionInfo")?.write_all(&info)?;
        cf.create_stream("EncryptedPackage")?.write_all(&pkg)?;
        cf.flush()?;
    }
    Ok(cur.into_inner())
}

struct AgileKeyMaterial {
    salt_pkg: Vec<u8>,
    key: Vec<u8>,
    key_bytes: usize,
    data_hash_alg: HashAlg,
    enc_hmac_key: Option<Vec<u8>>,
    enc_hmac_value: Option<Vec<u8>>,
}

fn verify_and_extract_key(info: &[u8], password: &str) -> Result<AgileKeyMaterial> {
    let xml = parse_info(info)?;
    if let Some(obj) = xml.cipher_algorithm.as_deref() {
        if !obj.eq_ignore_ascii_case("AES") {
            bail!("不支持的加密算法 {obj}");
        }
    }
    let key_alg = HashAlg::from_name(&xml.hash_algorithm)?;
    let data_alg = HashAlg::from_name(&xml.data_hash_algorithm)?;
    if xml
        .cipher_chaining
        .as_deref()
        .is_none_or(|c| !c.eq_ignore_ascii_case("ChainingModeCBC"))
    {
        bail!("只支持 CBC 链接模式");
    }
    let salt_pkg = b64(&xml.salt_data)?;
    let salt = b64(&xml.salt_key)?;
    let key_bytes = (xml.key_bits / 8) as usize;
    let enc_key_bytes = (xml.enc_key_bits / 8) as usize;
    let h = hash_final_alg(key_alg, password, &salt, xml.spin_count.max(1));
    let k_verifier = derive_key_alg(key_alg, &h, &BLOCK_VERIFIER_INPUT, enc_key_bytes);
    let k_hash = derive_key_alg(key_alg, &h, &BLOCK_VERIFIER_HASH, enc_key_bytes);
    let k_value = derive_key_alg(key_alg, &h, &BLOCK_KEY_VALUE, enc_key_bytes);
    let verifier = aes_cbc_decrypt_any(&k_verifier, &salt, &b64(&xml.enc_verifier_input)?)?;
    let want_hash = aes_cbc_decrypt_any(&k_hash, &salt, &b64(&xml.enc_verifier_hash)?)?;
    let got = key_alg.hash_slices(&[&verifier]);
    let hlen = key_alg.digest_len();
    if want_hash.len() < hlen || got[..hlen] != want_hash[..hlen] {
        bail!("打开密码不正确");
    }
    let mut key = aes_cbc_decrypt_any(&k_value, &salt, &b64(&xml.enc_key_value)?)?;
    if key.len() < key_bytes {
        bail!("包密钥长度不足");
    }
    key.truncate(key_bytes);
    let enc_hmac_key = xml.enc_hmac_key.as_deref().map(b64).transpose()?;
    let enc_hmac_value = xml.enc_hmac_value.as_deref().map(b64).transpose()?;
    Ok(AgileKeyMaterial {
        salt_pkg,
        key,
        key_bytes,
        data_hash_alg: data_alg,
        enc_hmac_key,
        enc_hmac_value,
    })
}

/// 轻量口令校验：同时支持 Excel 2007 Standard 与 2010–365 Agile，不读取或解密正文。
pub fn verify_password(path: &Path, password: &str) -> Result<()> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("读取 {} 失败", path.display()))?;
    let mut cf = cfb::CompoundFile::open(file)
        .map_err(|e| anyhow!("不是有效的 CFB 容器：{e}"))?;
    let mut info = Vec::new();
    cf.open_stream("EncryptionInfo")
        .map_err(|e| anyhow!("缺少 EncryptionInfo：{e}"))?
        .read_to_end(&mut info)?;
    if is_standard_encryption_info(&info) {
        let _ = verify_and_extract_standard_key(&info, password)?;
    } else {
        let _ = verify_and_extract_key(&info, password)?;
    }
    Ok(())
}

/// 解出明文 xlsx 字节
pub fn decrypt(path: &Path, password: &str) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).with_context(|| format!("读取 {} 失败", path.display()))?;
    decrypt_bytes(&bytes, password)
}

/// 解出明文 xlsx 字节并顺手返回 `(file_len, cfb_fingerprint)`，供 `load_with_password_diag` 写入缓存时免去二次打开源文件。
pub(crate) fn decrypt_with_fingerprint(path: &Path, password: &str) -> Result<(Vec<u8>, u64, [u8; 32])> {
    let bytes = std::fs::read(path).with_context(|| format!("读取 {} 失败", path.display()))?;
    let len = bytes.len() as u64;
    let fp = cfb_fingerprint_bytes(&bytes);
    let plain = decrypt_bytes(&bytes, password)?;
    Ok((plain, len, fp))
}

pub fn decrypt_bytes(bytes: &[u8], password: &str) -> Result<Vec<u8>> {
    use aes::cipher::KeyInit;
    let mut cf = cfb::CompoundFile::open(std::io::Cursor::new(bytes))
        .map_err(|e| anyhow!("不是有效的 CFB 容器：{e}"))?;
    let mut info = Vec::new();
    {
        let mut s = cf
            .open_stream("EncryptionInfo")
            .map_err(|e| anyhow!("缺少 EncryptionInfo：{e}"))?;
        info.reserve(s.len() as usize);
        s.read_to_end(&mut info)?;
    }

    // 分支 1：Excel 2007 Standard Encryption（AES-128/192/256-ECB + SHA-1）
    if is_standard_encryption_info(&info) {
        let key = verify_and_extract_standard_key(&info, password)?;
        let mut pkg = Vec::new();
        cf.open_stream("EncryptedPackage")
            .map_err(|e| anyhow!("缺少 EncryptedPackage：{e}"))?
            .read_to_end(&mut pkg)?;
        if pkg.len() < 8 {
            bail!("EncryptedPackage 太短");
        }
        let size = u64::from_le_bytes(pkg[..8].try_into().unwrap()) as usize;
        let payload = &pkg[8..];
        if payload.is_empty() || size == 0 {
            return Ok(Vec::new());
        }
        let mut out = aes_ecb_decrypt_any(&key, payload)?;
        out.truncate(size);
        return Ok(out);
    }

    // 分支 2：Excel 2010/2013/2016/365 Agile Encryption（AES-128/192/256-CBC + SHA1/256/384/512 + HMAC 校验）
    let (key_res, pkg_res) = std::thread::scope(|s| {
        let kdf = s.spawn(|| verify_and_extract_key(&info, password));
        let pkg = (|| -> Result<Vec<u8>> {
            let mut st = cf
                .open_stream("EncryptedPackage")
                .map_err(|e| anyhow!("缺少 EncryptedPackage：{e}"))?;
            let mut buf = Vec::with_capacity(st.len() as usize);
            st.read_to_end(&mut buf)?;
            Ok(buf)
        })();
        (kdf.join().expect("kdf thread panicked"), pkg)
    });
    let mat = key_res?;
    let pkg = pkg_res?;
    if pkg.len() < 8 {
        bail!("EncryptedPackage 太短");
    }

    // HMAC dataIntegrity 完整性校验（若 EncryptionInfo 提供）
    if let (Some(enc_hk), Some(enc_hv)) = (&mat.enc_hmac_key, &mat.enc_hmac_value) {
        let hlen = mat.data_hash_alg.digest_len();
        let iv_hk = derive_iv_alg(mat.data_hash_alg, &mat.salt_pkg, &BLOCK_HMAC_KEY);
        let iv_hv = derive_iv_alg(mat.data_hash_alg, &mat.salt_pkg, &BLOCK_HMAC_VALUE);
        let dec_hk = aes_cbc_decrypt_any(&mat.key, &iv_hk, enc_hk)?;
        let dec_hv = aes_cbc_decrypt_any(&mat.key, &iv_hv, enc_hv)?;
        if dec_hk.len() >= hlen && dec_hv.len() >= hlen {
            let computed = hmac_generic(mat.data_hash_alg, &dec_hk[..hlen], &pkg);
            // 常量时间比对，防止密文被篡改静默解出损坏包
            let mut diff = 0u8;
            for i in 0..hlen {
                diff |= computed[i] ^ dec_hv[i];
            }
            if diff != 0 {
                bail!("加密包 HMAC 完整性校验失败：文件可能已被篡改或损坏");
            }
        }
    }

    let mut size_bytes = [0u8; 8];
    size_bytes.copy_from_slice(&pkg[..8]);
    let size = u64::from_le_bytes(size_bytes) as usize;
    let payload = &pkg[8..];
    if payload.is_empty() || size == 0 {
        return Ok(Vec::new());
    }

    // 非 AES-256 或非 SHA-512 路径（Excel 2010/2013 AES-128 + SHA-1/SHA-256 等）
    if mat.key_bytes != 32 || mat.data_hash_alg != HashAlg::Sha512 {
        let mut out = Vec::with_capacity(size.saturating_add(16));
        for (i, seg) in payload.chunks(SEGMENT).enumerate() {
            let iv = derive_iv_alg(mat.data_hash_alg, &mat.salt_pkg, &(i as u32).to_le_bytes());
            let pt = aes_cbc_decrypt_any(&mat.key, &iv, seg)?;
            out.extend_from_slice(&pt);
        }
        out.truncate(size);
        return Ok(out);
    }

    let cipher = aes::Aes256::new_from_slice(&mat.key[..32]).map_err(|e| anyhow!("AES key: {e}"))?;
    let n_segs = payload.len().div_ceil(SEGMENT);
    let last_len = payload.len() - (n_segs - 1) * SEGMENT;
    let total_out = (n_segs - 1) * SEGMENT + last_len.div_ceil(16) * 16;
    let mut out = vec![0u8; total_out];
    let iv_gen = SegmentIvGen::new(&mat.salt_pkg);
    let workers = segment_workers(payload.len(), n_segs);

    if workers == 1 {
        for (i, seg) in payload.chunks(SEGMENT).enumerate() {
            let dst_off = i * SEGMENT;
            let dst_len = seg.len().div_ceil(16) * 16;
            let iv = iv_gen.iv(i as u32);
            decrypt_chunk_into(&cipher, &iv, seg, &mut out[dst_off..dst_off + dst_len]);
        }
    } else {
        let segs_per_worker = n_segs.div_ceil(workers);
        std::thread::scope(|s| {
            let mut rem_out = out.as_mut_slice();
            let mut seg_start = 0usize;
            while seg_start < n_segs {
                let seg_end = (seg_start + segs_per_worker).min(n_segs);
                let src_start = seg_start * SEGMENT;
                let src_end = (seg_end * SEGMENT).min(payload.len());
                let src_slice = &payload[src_start..src_end];
                let out_len = if seg_end == n_segs {
                    rem_out.len()
                } else {
                    (seg_end - seg_start) * SEGMENT
                };
                let (dst_slice, rest) = rem_out.split_at_mut(out_len);
                rem_out = rest;
                let cipher = &cipher;
                let iv_gen = &iv_gen;
                s.spawn(move || {
                    for (local_i, seg) in src_slice.chunks(SEGMENT).enumerate() {
                        let dst_off = local_i * SEGMENT;
                        let dst_len = seg.len().div_ceil(16) * 16;
                        let iv = iv_gen.iv((seg_start + local_i) as u32);
                        decrypt_chunk_into(cipher, &iv, seg, &mut dst_slice[dst_off..dst_off + dst_len]);
                    }
                });
                seg_start = seg_end;
            }
        });
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
    enc_key_bits: u32,
    hash_algorithm: String,
    data_hash_algorithm: String,
    cipher_algorithm: Option<String>,
    cipher_chaining: Option<String>,
    enc_verifier_input: String,
    enc_verifier_hash: String,
    enc_key_value: String,
    enc_hmac_key: Option<String>,
    enc_hmac_value: Option<String>,
}

/// EncryptionInfo 流：8 字节版本头（4,4,0x40,0）+ XML
fn parse_info(bytes: &[u8]) -> Result<InfoXml> {
    let start = bytes
        .windows(5)
        .position(|w| w == b"<?xml")
        .or_else(|| bytes.windows(12).position(|w| w == b"<encryption "))
        .ok_or_else(|| anyhow!("EncryptionInfo 里找不到 XML"))?;
    let xml = std::str::from_utf8(&bytes[start..]).context("EncryptionInfo 不是 UTF-8")?;
    // keyData 管包加密，p:encryptedKey 管口令校验，dataIntegrity 管 HMAC 校验
    let key_data = tag_block(xml, "<keyData").unwrap_or(xml);
    let data_integrity = tag_block(xml, "<dataIntegrity");
    let enc_key = tag_block(xml, "<p:encryptedKey")
        .or_else(|| tag_block(xml, "<encryptedKey"))
        .ok_or_else(|| anyhow!("缺少 encryptedKey（口令加密段）"))?;
    let key_bits = attr(key_data, "keyBits").and_then(|v| v.parse().ok()).unwrap_or(256);
    let enc_key_bits = attr(enc_key, "keyBits").and_then(|v| v.parse().ok()).unwrap_or(key_bits);
    let spin_count = attr(enc_key, "spinCount").and_then(|v| v.parse().ok()).unwrap_or(SPIN);
    let salt_data = attr(key_data, "saltValue")
        .ok_or_else(|| anyhow!("keyData 缺少 saltValue"))?
        .to_string();
    let key_hash = attr(enc_key, "hashAlgorithm")
        .unwrap_or("SHA512")
        .trim()
        .to_ascii_uppercase();
    let data_hash = attr(key_data, "hashAlgorithm")
        .unwrap_or(&key_hash)
        .trim()
        .to_ascii_uppercase();
    Ok(InfoXml {
        salt_data,
        salt_key: attr(enc_key, "saltValue")
            .ok_or_else(|| anyhow!("encryptedKey 缺少 saltValue"))?
            .into(),
        spin_count,
        key_bits,
        enc_key_bits,
        hash_algorithm: key_hash,
        data_hash_algorithm: data_hash,
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
        enc_hmac_key: data_integrity
            .and_then(|di| attr(di, "encryptedHmacKey"))
            .map(str::to_string),
        enc_hmac_value: data_integrity
            .and_then(|di| attr(di, "encryptedHmacValue"))
            .map(str::to_string),
    })
}

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut v = vec![0u8; n];
    getrandom::fill(&mut v).map_err(|e| anyhow!("系统随机数失败：{e}"))?;
    Ok(v)
}

/// 计算加密 CFB 字节流的首尾指纹（与 `xlsx::encrypted_file_key` 保持字节级一致）：
/// `SHA256(len_le64 || head[..8192] || tail[len-4096..])`。
pub(crate) fn cfb_fingerprint_bytes(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let len = bytes.len() as u64;
    let mut hasher = Sha256::new();
    hasher.update(len.to_le_bytes());
    let hn = bytes.len().min(8192);
    hasher.update(&bytes[..hn]);
    if bytes.len() > 8192 {
        let tail_start = bytes.len().saturating_sub(4096).max(hn);
        hasher.update(&bytes[tail_start..]);
    }
    hasher.finalize().into()
}

/// 把明文 xlsx 加密写成 CFB 容器（Agile，AES-256-CBC/SHA-512）
pub fn encrypt(path: &Path, plain: &[u8], settings: &EncryptionSettings) -> Result<()> {
    encrypt_with_fingerprint(path, plain, settings).map(|_| ())
}

/// 同 `encrypt`，并在内存构建 CFB 时顺手校验必要流并返回 `(file_len, cfb_fingerprint)`，
/// 让原子保存免去立即二次 `File::open` 重读刚写出的 CFB 文件。
pub(crate) fn encrypt_with_fingerprint(
    path: &Path,
    plain: &[u8],
    settings: &EncryptionSettings,
) -> Result<Option<(u64, [u8; 32])>> {
    use aes::cipher::KeyInit;
    let salt = random_bytes(16)?;      // 口令派生/验证器
    let salt_pkg = random_bytes(16)?;  // 包分段 IV
    let verifier = random_bytes(16)?;
    let key = random_bytes(32)?;
    let hmac_key = random_bytes(64)?;
    let spin = settings.spin.max(1);
    let cipher = aes::Aes256::new_from_slice(&key).map_err(|e| anyhow!("AES key: {e}"))?;

    // 包：8 字节长度 + 4096 字节分段；与 10 万轮口令 KDF 并行执行，掩盖正文加密与 HMAC 开销
    let n_segs = plain.len().div_ceil(SEGMENT);
    let enc_payload_len = if n_segs == 0 {
        0
    } else {
        let last_len = plain.len() - (n_segs - 1) * SEGMENT;
        (n_segs - 1) * SEGMENT + last_len.div_ceil(16) * 16
    };
    let mut pkg = vec![0u8; 8 + enc_payload_len];
    pkg[..8].copy_from_slice(&(plain.len() as u64).to_le_bytes());

    let (h, hmac_value) = std::thread::scope(|s| {
        let kdf_handle = s.spawn(|| hash_final(&settings.password, &salt, spin));

        let iv_gen = SegmentIvGen::new(&salt_pkg);
        let workers = segment_workers(plain.len(), n_segs);
        let out_payload = &mut pkg[8..];
        if workers == 1 {
            for (i, seg) in plain.chunks(SEGMENT).enumerate() {
                let dst_off = i * SEGMENT;
                let dst_len = seg.len().div_ceil(16) * 16;
                let iv = iv_gen.iv(i as u32);
                encrypt_chunk_into(&cipher, &iv, seg, &mut out_payload[dst_off..dst_off + dst_len]);
            }
        } else {
            let segs_per_worker = n_segs.div_ceil(workers);
            std::thread::scope(|inner| {
                let mut rem_out = out_payload;
                let mut seg_start = 0usize;
                while seg_start < n_segs {
                    let seg_end = (seg_start + segs_per_worker).min(n_segs);
                    let src_start = seg_start * SEGMENT;
                    let src_end = (seg_end * SEGMENT).min(plain.len());
                    let src_slice = &plain[src_start..src_end];
                    let out_len = if seg_end == n_segs {
                        rem_out.len()
                    } else {
                        (seg_end - seg_start) * SEGMENT
                    };
                    let (dst_slice, rest) = rem_out.split_at_mut(out_len);
                    rem_out = rest;
                    let cipher = &cipher;
                    let iv_gen = &iv_gen;
                    inner.spawn(move || {
                        for (local_i, seg) in src_slice.chunks(SEGMENT).enumerate() {
                            let dst_off = local_i * SEGMENT;
                            let dst_len = seg.len().div_ceil(16) * 16;
                            let iv = iv_gen.iv((seg_start + local_i) as u32);
                            encrypt_chunk_into(
                                cipher,
                                &iv,
                                seg,
                                &mut dst_slice[dst_off..dst_off + dst_len],
                            );
                        }
                    });
                    seg_start = seg_end;
                }
            });
        }
        // 数据完整性：对「长度头 + 密文」做 HMAC-SHA512
        let hmac_val = hmac_sha512(&hmac_key, &pkg);
        let h = kdf_handle.join().expect("kdf thread panicked");
        (h, hmac_val)
    });

    let k_verifier = derive_key(&h, &BLOCK_VERIFIER_INPUT, 32);
    let k_hash = derive_key(&h, &BLOCK_VERIFIER_HASH, 32);
    let k_value = derive_key(&h, &BLOCK_KEY_VALUE, 32);
    let enc_verifier = aes_cbc_encrypt(&k_verifier, &salt, &verifier)?;
    let mut d = Sha512::new();
    d.update(&verifier);
    let vhash: [u8; 64] = d.finalize().into();
    let enc_hash = aes_cbc_encrypt(&k_hash, &salt, &vhash)?;
    let enc_key = aes_cbc_encrypt(&k_value, &salt, &key)?;

    let iv1 = {
        let mut d = Sha512::new();
        d.update(&salt_pkg);
        d.update(BLOCK_HMAC_KEY);
        let h = d.finalize();
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&h[..16]);
        iv
    };
    let iv2 = {
        let mut d = Sha512::new();
        d.update(&salt_pkg);
        d.update(BLOCK_HMAC_VALUE);
        let h = d.finalize();
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&h[..16]);
        iv
    };
    let mut enc_hmac_key = vec![0u8; hmac_key.len()];
    encrypt_chunk_into(&cipher, &iv1, &hmac_key, &mut enc_hmac_key);
    let mut enc_hmac_value = vec![0u8; hmac_value.len()];
    encrypt_chunk_into(&cipher, &iv2, &hmac_value, &mut enc_hmac_value);
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

    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut staged = tempfile::Builder::new().prefix(".~cipher-").tempfile_in(dir)?;
    let write_cfb = |writer: &mut dyn WriteSeek| -> Result<()> {
        let mut cf = cfb::CompoundFile::create(writer)
            .map_err(|e| anyhow!("创建 CFB 失败：{e}"))?;
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
        Ok(())
    };
    let meta_info = if pkg.len() <= 64 * 1024 * 1024 {
        // 内存构建 CFB 再一次性顺序写出，消除 Windows 上数百次 512B/4KB 随机 seek/write 系统调用
        let mut mem = std::io::Cursor::new(Vec::with_capacity(pkg.len() + 32 * 1024));
        write_cfb(&mut mem)?;
        let cfb_bytes = mem.into_inner();
        // O(1) header + size sanity check on the freshly flushed in-memory CFB container
        // (avoids re-parsing the entire FAT sector chain via `CompoundFile::open`).
        if !cfb_bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
            || info.len() < 8
            || pkg.len() < 8
            || cfb_bytes.len() < 512 + pkg.len()
        {
            bail!(
                "生成的加密流长度异常（cfb={}, EncryptionInfo={}, EncryptedPackage={}）",
                cfb_bytes.len(),
                info.len(),
                pkg.len()
            );
        }
        let fp = cfb_fingerprint_bytes(&cfb_bytes);
        let total_len = cfb_bytes.len() as u64;
        staged.as_file_mut().write_all(&cfb_bytes)?;
        staged.as_file_mut().flush()?;
        Some((total_len, fp))
    } else {
        write_cfb(staged.as_file_mut())?;
        staged.as_file_mut().flush()?;
        None
    };
    staged.persist(path).with_context(|| format!("写入 {} 失败", path.display()))?;
    Ok(meta_info)
}

trait WriteSeek: Read + Write + Seek {}
impl<T: Read + Write + Seek> WriteSeek for T {}

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
        verify_password(&p, "p@ss word").unwrap();
        assert!(verify_password(&p, "wrong").is_err());
        assert_eq!(decrypt(&p, "p@ss word").unwrap(), plain);
        let err = decrypt(&p, "wrong").unwrap_err().to_string();
        assert!(err.contains("密码"), "{err}");
    }

    #[test]
    fn compress512_kdf_and_parallel_segments_match_reference() {
        let salt = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00];
        let pw = "Cellan测试Password!2026";
        for spin in [0u32, 1, 17, 257] {
            let fast = hash_final(pw, &salt, spin);
            let mut h = Sha512::new();
            h.update(salt);
            h.update(utf16le(pw));
            let mut slow: [u8; 64] = h.finalize().into();
            for i in 0..spin {
                let mut d = Sha512::new();
                d.update(i.to_le_bytes());
                d.update(slow);
                slow = d.finalize().into();
            }
            assert_eq!(fast, slow, "mismatch at spin={spin}");
        }
        let iv_gen = SegmentIvGen::new(&salt);
        for idx in [0u32, 1, 15, 255, 1024] {
            assert_eq!(iv_gen.iv(idx), segment_iv(&salt, idx));
        }

        // > 64 KB 非 16 字节对齐载荷，触发并行 4 KB 分段加解密
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big_enc.xlsx");
        let plain: Vec<u8> = (0..(150 * 1024 + 13)).map(|i| ((i * 131 + 17) & 0xFF) as u8).collect();
        encrypt(&p, &plain, &EncryptionSettings { password: pw.into(), spin: 32 }).unwrap();
        verify_password(&p, pw).unwrap();
        assert!(verify_password(&p, "bad").is_err());
        assert_eq!(decrypt(&p, pw).unwrap(), plain);
    }
}
