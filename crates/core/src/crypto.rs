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

fn verify_and_extract_key(info: &[u8], password: &str) -> Result<(Vec<u8>, Vec<u8>, usize)> {
    let xml = parse_info(info)?;
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
    Ok((salt_pkg, key, key_bytes))
}

/// 轻量口令校验：仅解析 `EncryptionInfo` 并校验 `EncryptedVerifierHash`，不读取或解密正文。
pub fn verify_password(path: &Path, password: &str) -> Result<()> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("读取 {} 失败", path.display()))?;
    let mut cf = cfb::CompoundFile::open(file)
        .map_err(|e| anyhow!("不是有效的 CFB 容器：{e}"))?;
    let mut info = Vec::new();
    cf.open_stream("EncryptionInfo")
        .map_err(|e| anyhow!("缺少 EncryptionInfo：{e}"))?
        .read_to_end(&mut info)?;
    let _ = verify_and_extract_key(&info, password)?;
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
    // Overlap reading EncryptedPackage sectors from CFB with the 100k-spin SHA-512 KDF.
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
    let (salt_pkg, key, key_bytes) = key_res?;
    if key_bytes != 32 {
        bail!("AES 密钥/IV 长度不对");
    }
    let cipher = aes::Aes256::new_from_slice(&key[..32]).map_err(|e| anyhow!("AES key: {e}"))?;
    let pkg = pkg_res?;
    if pkg.len() < 8 {
        bail!("EncryptedPackage 太短");
    }
    let mut size_bytes = [0u8; 8];
    size_bytes.copy_from_slice(&pkg[..8]);
    let size = u64::from_le_bytes(size_bytes) as usize;
    let payload = &pkg[8..];
    if payload.is_empty() || size == 0 {
        return Ok(Vec::new());
    }
    let n_segs = payload.len().div_ceil(SEGMENT);
    let last_len = payload.len() - (n_segs - 1) * SEGMENT;
    let total_out = (n_segs - 1) * SEGMENT + last_len.div_ceil(16) * 16;
    let mut out = vec![0u8; total_out];
    let iv_gen = SegmentIvGen::new(&salt_pkg);
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
