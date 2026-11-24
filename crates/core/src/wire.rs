//! 局域网传输加密：ChaCha20 流加密 + HMAC-SHA256 认证（encrypt-then-MAC）。
//!
//! 为什么自己写：本机离线，拿不到 rustls / ring。但 ChaCha20 与 HMAC 都有公开的
//! 标准测试向量，实现对不对可以当场证明，不需要「看起来能跑」。
//!
//! 密钥派生分两层（协议 v2）：
//! 1. 房间密钥：`PBKDF2-HMAC-SHA256(口令, 房间 salt, ROOM_KDF_ITERS)`。宿主开房时
//!    随机生成房间 salt 并只算一次；客户端连上时从挑战里拿到同一个 salt 再算一次。
//!    这一步是故意做慢的——旁听者想离线穷举口令，每猜一次都要付出同样的代价。
//! 2. 连接密钥：`SHA256("sheetcore-wire-v2" || 连接 salt || 房间密钥)`，再各自展开出
//!    加密钥与认证钥（不同前缀，互不相关）。每条连接 salt 不同，密钥互不复用。
//! 每帧独立计数器作为 nonce，接收端要求严格递增（防重放/重排）。

use base64::Engine;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

const DIR_COMPRESSED_FLAG: u8 = 0x80;
const COMPRESS_MIN_BYTES: usize = 512;
const MAX_DECOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

/// 协议 v3 二进制帧魔数首字节（非可打印 ASCII，与 v2 JSON/Base64 首字节严格不相交）
pub const BINARY_V3_MAGIC: u8 = 0x02;
/// v3 二进制帧体最小长度：`1B flags + 8B counter + 32B HMAC-SHA256`
pub const BINARY_V3_MIN_BODY_LEN: usize = 1 + 8 + 32;
/// v3 flags 标志位：`bit0 = compressed`，`bits 1..=2 = dir`，`bit3 = compact_bin`，`bit6 = v3 域分离标记`
const V3_FLAG_COMPRESSED: u8 = 0x01;
const V3_FLAG_DIR_MASK: u8 = 0x06;
pub const V3_FLAG_COMPACT_BIN: u8 = 0x08;
const V3_FLAG_MARKER: u8 = 0x40;
const V3_FLAG_ALLOWED_MASK: u8 =
    V3_FLAG_MARKER | V3_FLAG_COMPACT_BIN | V3_FLAG_DIR_MASK | V3_FLAG_COMPRESSED;

#[inline(always)]
fn init_chacha_state(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u32; 16] {
    [
        0x6170_7865,
        0x3320_646e,
        0x7962_2d32,
        0x6b20_6574,
        u32::from_le_bytes([key[0], key[1], key[2], key[3]]),
        u32::from_le_bytes([key[4], key[5], key[6], key[7]]),
        u32::from_le_bytes([key[8], key[9], key[10], key[11]]),
        u32::from_le_bytes([key[12], key[13], key[14], key[15]]),
        u32::from_le_bytes([key[16], key[17], key[18], key[19]]),
        u32::from_le_bytes([key[20], key[21], key[22], key[23]]),
        u32::from_le_bytes([key[24], key[25], key[26], key[27]]),
        u32::from_le_bytes([key[28], key[29], key[30], key[31]]),
        counter,
        u32::from_le_bytes([nonce[0], nonce[1], nonce[2], nonce[3]]),
        u32::from_le_bytes([nonce[4], nonce[5], nonce[6], nonce[7]]),
        u32::from_le_bytes([nonce[8], nonce[9], nonce[10], nonce[11]]),
    ]
}

/// ChaCha20 的 32 位字
#[inline(always)]
fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] ^= s[c];
    s[b] = s[b].rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] ^= s[c];
    s[b] = s[b].rotate_left(7);
}

#[inline(always)]
fn chacha20_block_from_state(mut init: [u32; 16], counter: u32) -> [u8; 64] {
    init[12] = counter;
    let mut st = init;
    for _ in 0..10 {
        quarter(&mut st, 0, 4, 8, 12);
        quarter(&mut st, 1, 5, 9, 13);
        quarter(&mut st, 2, 6, 10, 14);
        quarter(&mut st, 3, 7, 11, 15);
        quarter(&mut st, 0, 5, 10, 15);
        quarter(&mut st, 1, 6, 11, 12);
        quarter(&mut st, 2, 7, 8, 13);
        quarter(&mut st, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        let v = st[i].wrapping_add(init[i]);
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// 生成一个 64 字节的密钥流块
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    chacha20_block_from_state(init_chacha_state(key, counter, nonce), counter)
}

#[inline(always)]
fn add4(a: [u32; 4], b: [u32; 4]) -> [u32; 4] {
    [
        a[0].wrapping_add(b[0]),
        a[1].wrapping_add(b[1]),
        a[2].wrapping_add(b[2]),
        a[3].wrapping_add(b[3]),
    ]
}

#[inline(always)]
fn xor4(a: [u32; 4], b: [u32; 4]) -> [u32; 4] {
    [a[0] ^ b[0], a[1] ^ b[1], a[2] ^ b[2], a[3] ^ b[3]]
}

#[inline(always)]
fn rotl4(a: [u32; 4], n: u32) -> [u32; 4] {
    [
        a[0].rotate_left(n),
        a[1].rotate_left(n),
        a[2].rotate_left(n),
        a[3].rotate_left(n),
    ]
}

#[inline(always)]
fn quarter_x4(
    mut a: [u32; 4],
    mut b: [u32; 4],
    mut c: [u32; 4],
    mut d: [u32; 4],
) -> ([u32; 4], [u32; 4], [u32; 4], [u32; 4]) {
    a = add4(a, b);
    d = rotl4(xor4(d, a), 16);
    c = add4(c, d);
    b = rotl4(xor4(b, c), 12);
    a = add4(a, b);
    d = rotl4(xor4(d, a), 8);
    c = add4(c, d);
    b = rotl4(xor4(b, c), 7);
    (a, b, c, d)
}

#[inline(always)]
fn xor_quad_transposed(
    chunk: &mut [u8; 256],
    quad_offset: usize,
    w0: [u32; 4],
    w1: [u32; 4],
    w2: [u32; 4],
    w3: [u32; 4],
) {
    let rows = [
        [w0[0], w1[0], w2[0], w3[0]],
        [w0[1], w1[1], w2[1], w3[1]],
        [w0[2], w1[2], w2[2], w3[2]],
        [w0[3], w1[3], w2[3], w3[3]],
    ];
    for (lane, row) in rows.iter().enumerate() {
        let base = lane * 64 + quad_offset;
        for (j, &word) in row.iter().enumerate() {
            let off = base + j * 4;
            let cur = u32::from_le_bytes([
                chunk[off],
                chunk[off + 1],
                chunk[off + 2],
                chunk[off + 3],
            ]);
            chunk[off..off + 4].copy_from_slice(&(cur ^ word).to_le_bytes());
        }
    }
}

/// 4-way 并行生成 4 个 64 字节密钥流块（256 字节）并原地异或。
/// 每个状态字为 `[u32; 4]`，在 x86-64-v2（SSE2/SSSE3/SSE4.2）上由 LLVM 自动向量化为 128 位 SIMD 指令。
#[inline(always)]
fn chacha20_xor_4blocks(init: &[u32; 16], counter: u32, chunk: &mut [u8; 256]) {
    let c = [
        counter,
        counter.wrapping_add(1),
        counter.wrapping_add(2),
        counter.wrapping_add(3),
    ];
    let mut s0 = [init[0]; 4];
    let mut s1 = [init[1]; 4];
    let mut s2 = [init[2]; 4];
    let mut s3 = [init[3]; 4];
    let mut s4 = [init[4]; 4];
    let mut s5 = [init[5]; 4];
    let mut s6 = [init[6]; 4];
    let mut s7 = [init[7]; 4];
    let mut s8 = [init[8]; 4];
    let mut s9 = [init[9]; 4];
    let mut s10 = [init[10]; 4];
    let mut s11 = [init[11]; 4];
    let mut s12 = c;
    let mut s13 = [init[13]; 4];
    let mut s14 = [init[14]; 4];
    let mut s15 = [init[15]; 4];

    for _ in 0..10 {
        (s0, s4, s8, s12) = quarter_x4(s0, s4, s8, s12);
        (s1, s5, s9, s13) = quarter_x4(s1, s5, s9, s13);
        (s2, s6, s10, s14) = quarter_x4(s2, s6, s10, s14);
        (s3, s7, s11, s15) = quarter_x4(s3, s7, s11, s15);
        (s0, s5, s10, s15) = quarter_x4(s0, s5, s10, s15);
        (s1, s6, s11, s12) = quarter_x4(s1, s6, s11, s12);
        (s2, s7, s8, s13) = quarter_x4(s2, s7, s8, s13);
        (s3, s4, s9, s14) = quarter_x4(s3, s4, s9, s14);
    }

    s0 = add4(s0, [init[0]; 4]);
    s1 = add4(s1, [init[1]; 4]);
    s2 = add4(s2, [init[2]; 4]);
    s3 = add4(s3, [init[3]; 4]);
    s4 = add4(s4, [init[4]; 4]);
    s5 = add4(s5, [init[5]; 4]);
    s6 = add4(s6, [init[6]; 4]);
    s7 = add4(s7, [init[7]; 4]);
    s8 = add4(s8, [init[8]; 4]);
    s9 = add4(s9, [init[9]; 4]);
    s10 = add4(s10, [init[10]; 4]);
    s11 = add4(s11, [init[11]; 4]);
    s12 = add4(s12, c);
    s13 = add4(s13, [init[13]; 4]);
    s14 = add4(s14, [init[14]; 4]);
    s15 = add4(s15, [init[15]; 4]);

    xor_quad_transposed(chunk, 0, s0, s1, s2, s3);
    xor_quad_transposed(chunk, 16, s4, s5, s6, s7);
    xor_quad_transposed(chunk, 32, s8, s9, s10, s11);
    xor_quad_transposed(chunk, 48, s12, s13, s14, s15);
}

#[inline(always)]
fn quarter_16way(s: &mut [[u32; 16]; 16], a: usize, b: usize, c: usize, d: usize) {
    let mut i = 0;
    while i < 16 {
        s[a][i] = s[a][i].wrapping_add(s[b][i]);
        s[d][i] = (s[d][i] ^ s[a][i]).rotate_left(16);
        s[c][i] = s[c][i].wrapping_add(s[d][i]);
        s[b][i] = (s[b][i] ^ s[c][i]).rotate_left(12);
        s[a][i] = s[a][i].wrapping_add(s[b][i]);
        s[d][i] = (s[d][i] ^ s[a][i]).rotate_left(8);
        s[c][i] = s[c][i].wrapping_add(s[d][i]);
        s[b][i] = (s[b][i] ^ s[c][i]).rotate_left(7);
        i += 1;
    }
}

/// 16 块（4×4-way = 1024 字节）并行纯安全 Rust ChaCha20：
/// 固定 16 路循环触发 LLVM Loop Vectorizer（VF=4）在 x86-64-v2 上生成真实
/// SSE2/SSSE3 `paddd`/`pxor`/`pshufb`/`pslld`/`psrld` 128 位向量指令（~2.8 GB/s）。
#[inline(always)]
fn chacha20_xor_16blocks(init: &[u32; 16], counter: u32, chunk: &mut [u8; 1024]) {
    let mut s = [[0u32; 16]; 16];
    for w in 0..16 {
        s[w] = [init[w]; 16];
    }
    let mut ctrs = [0u32; 16];
    for lane in 0..16 {
        ctrs[lane] = counter.wrapping_add(lane as u32);
    }
    s[12] = ctrs;

    for _ in 0..10 {
        quarter_16way(&mut s, 0, 4, 8, 12);
        quarter_16way(&mut s, 1, 5, 9, 13);
        quarter_16way(&mut s, 2, 6, 10, 14);
        quarter_16way(&mut s, 3, 7, 11, 15);
        quarter_16way(&mut s, 0, 5, 10, 15);
        quarter_16way(&mut s, 1, 6, 11, 12);
        quarter_16way(&mut s, 2, 7, 8, 13);
        quarter_16way(&mut s, 3, 4, 9, 14);
    }

    for w in 0..16 {
        if w == 12 {
            for lane in 0..16 {
                s[12][lane] = s[12][lane].wrapping_add(ctrs[lane]);
            }
        } else {
            let add = init[w];
            for lane in 0..16 {
                s[w][lane] = s[w][lane].wrapping_add(add);
            }
        }
    }

    for lane in 0..16 {
        let block = &mut chunk[lane * 64..(lane + 1) * 64];
        for w in 0..16 {
            let off = w * 4;
            let cur = u32::from_le_bytes([
                block[off],
                block[off + 1],
                block[off + 2],
                block[off + 3],
            ]);
            block[off..off + 4].copy_from_slice(&(cur ^ s[w][lane]).to_le_bytes());
        }
    }
}

/// 原地加/解密（同一操作：4×4-way 1024B SSE2/SSSE3 向量主循环 + 4-way 256B + 单块 64B 尾部回退）
pub fn apply_keystream(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    if data.is_empty() {
        return;
    }
    let init = init_chacha_state(key, counter, nonce);
    let mut block_idx = 0u32;

    let mut chunks1024 = data.chunks_exact_mut(1024);
    for chunk in &mut chunks1024 {
        let block16: &mut [u8; 1024] = chunk.try_into().expect("1024-byte exact chunk");
        chacha20_xor_16blocks(&init, counter.wrapping_add(block_idx), block16);
        block_idx = block_idx.wrapping_add(16);
    }

    let mut chunks256 = chunks1024.into_remainder().chunks_exact_mut(256);
    for chunk in &mut chunks256 {
        let block4: &mut [u8; 256] = chunk.try_into().expect("256-byte exact chunk");
        chacha20_xor_4blocks(&init, counter.wrapping_add(block_idx), block4);
        block_idx = block_idx.wrapping_add(4);
    }

    let mut chunks64 = chunks256.into_remainder().chunks_exact_mut(64);
    for chunk in &mut chunks64 {
        let ks = chacha20_block_from_state(init, counter.wrapping_add(block_idx));
        for (dst, src) in chunk.iter_mut().zip(ks.iter()) {
            *dst ^= *src;
        }
        block_idx = block_idx.wrapping_add(1);
    }

    let rem = chunks64.into_remainder();
    if !rem.is_empty() {
        let ks = chacha20_block_from_state(init, counter.wrapping_add(block_idx));
        for (dst, src) in rem.iter_mut().zip(ks.iter()) {
            *dst ^= *src;
        }
    }
}

/// 房间口令拉伸的迭代次数（PBKDF2-HMAC-SHA256）
pub const ROOM_KDF_ITERS: u32 = 200_000;

/// PBKDF2-HMAC-SHA256，只输出一个 32 字节块（dkLen = 32）。
///
/// 热循环里每轮的消息都是上一轮的 32 字节结果，所以直接对 SHA-256 压缩函数操作：
/// ipad/opad 两个状态各压缩一次后缓存，每轮只做两次压缩，不走通用 Digest 包装
/// （调试构建里通用包装比压缩本身还慢）。第一轮的消息是任意长度的 salt，走 HMAC。
pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iters: u32) -> [u8; 32] {
    use sha2::block_api::compress256;
    const IV: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut k = [0u8; 64];
    if password.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(password));
    } else {
        k[..password.len()].copy_from_slice(password);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut istate = IV;
    compress256(&mut istate, &[ipad]);
    let mut ostate = IV;
    compress256(&mut ostate, &[opad]);

    // 一个已填好 SHA-256 尾部填充的块：前 32 字节放消息，总长 = 64（pad 块）+ 32 字节
    let mut block = [0u8; 64];
    block[32] = 0x80;
    block[56..].copy_from_slice(&((64u64 + 32) * 8).to_be_bytes());
    let state_bytes = |st: &[u32; 8], out: &mut [u8]| {
        for (i, w) in st.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
        }
    };
    let mut first = Vec::with_capacity(salt.len() + 4);
    first.extend_from_slice(salt);
    first.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &first);
    let mut out = u;
    for _ in 1..iters.max(1) {
        block[..32].copy_from_slice(&u);
        let mut st = istate;
        compress256(&mut st, &[block]);
        state_bytes(&st, &mut block[..32]);
        let mut st = ostate;
        compress256(&mut st, &[block]);
        state_bytes(&st, &mut u);
        for i in 0..32 {
            out[i] ^= u[i];
        }
    }
    out
}

/// 长期房间密钥：口令经 PBKDF2 拉伸后的结果。宿主每次开房只算一次。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RoomKey([u8; 32]);

impl std::fmt::Debug for RoomKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomKey(..)")
    }
}

impl RoomKey {
    /// 由口令与房间 salt 派生（慢：ROOM_KDF_ITERS 轮）
    pub fn derive(password: &str, room_salt: &[u8]) -> Self {
        Self::derive_with_iters(password, room_salt, ROOM_KDF_ITERS)
    }

    pub fn derive_with_iters(password: &str, room_salt: &[u8], iters: u32) -> Self {
        let mut salt = Vec::with_capacity(room_salt.len() + 16);
        salt.extend_from_slice(b"cellan-room-v2:");
        salt.extend_from_slice(room_salt);
        Self(pbkdf2_hmac_sha256(password.as_bytes(), &salt, iters))
    }

    /// 入房证明：HMAC(房间密钥, "proof" || nonce)，十六进制
    pub fn proof(&self, nonce: u64) -> String {
        let mut msg = Vec::with_capacity(13);
        msg.extend_from_slice(b"proof");
        msg.extend_from_slice(&nonce.to_le_bytes());
        hmac_sha256(&self.0, &msg).iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let d = Sha256::digest(key);
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    let out = outer.finalize();
    let mut r = [0u8; 32];
    r.copy_from_slice(&out);
    r
}

pub fn b64_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let trimmed = s.trim_ascii();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    if !trimmed.bytes().any(|b| b.is_ascii_whitespace()) {
        if let Ok(out) = base64::engine::general_purpose::STANDARD.decode(trimmed) {
            return Some(out);
        }
        return base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(trimmed.trim_end_matches('='))
            .ok();
    }
    let cleaned: Vec<u8> = trimmed
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(&cleaned)
        .ok()
}

/// v3 二进制帧解密后的明文载荷：兼容 UTF-8 JSON 文本帧与紧凑二进制内层帧。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodedWireFrame {
    Text(String),
    Compact(Vec<u8>),
}

/// 预处理的帧负载：供宿主广播给多个 peer 时仅做一次可选 deflate 压缩，
/// 各 peer 只需按自身会话密钥执行 ChaCha20 加密 + HMAC-SHA256 认证。
#[derive(Clone, Debug)]
pub struct PreparedPayload {
    payload: Vec<u8>,
    compressed: bool,
    compact_bin: bool,
}

impl PreparedPayload {
    /// 该预处理负载是否启用了 deflate 压缩
    pub fn is_compressed(&self) -> bool {
        self.compressed
    }

    /// 该预处理负载是否为 v3 紧凑二进制内层格式
    pub fn is_compact_bin(&self) -> bool {
        self.compact_bin
    }

    /// 预处理后（压缩或原始）负载的字节长度
    pub fn payload_len(&self) -> usize {
        self.payload.len()
    }
}

/// 预处理待发送的紧凑二进制内层帧（仅压缩一次，供多客户端零重复序列化广播）
pub fn prepare_compact_payload(raw_bytes: Vec<u8>, compress: bool) -> std::sync::Arc<PreparedPayload> {
    if compress && raw_bytes.len() >= COMPRESS_MIN_BYTES {
        let mut compressed_buf = Vec::with_capacity(raw_bytes.len().min(65_536));
        let ok = {
            let mut enc = flate2::write::DeflateEncoder::new(
                &mut compressed_buf,
                flate2::Compression::fast(),
            );
            enc.write_all(&raw_bytes).is_ok() && enc.finish().is_ok()
        };
        if ok && compressed_buf.len() < raw_bytes.len() {
            return std::sync::Arc::new(PreparedPayload {
                payload: compressed_buf,
                compressed: true,
                compact_bin: true,
            });
        }
    }
    std::sync::Arc::new(PreparedPayload {
        payload: raw_bytes,
        compressed: false,
        compact_bin: true,
    })
}

/// 预处理待发送的明文 JSON 帧：
/// - `compress == true` 且长度不小于 `COMPRESS_MIN_BYTES` 时仅压缩一次；若压缩后反而变大则回退原始明文。
/// - `compress == false` 时跳过 deflate（适用于 `UpdateSlice` 等高熵 Base64 帧）。
pub fn prepare_payload(plain: &str, compress: bool) -> std::sync::Arc<PreparedPayload> {
    let raw_bytes = plain.as_bytes();
    if compress && raw_bytes.len() >= COMPRESS_MIN_BYTES {
        let mut compressed_buf = Vec::with_capacity(raw_bytes.len().min(65_536));
        let ok = {
            let mut enc = flate2::write::DeflateEncoder::new(
                &mut compressed_buf,
                flate2::Compression::fast(),
            );
            enc.write_all(raw_bytes).is_ok() && enc.finish().is_ok()
        };
        if ok && compressed_buf.len() < raw_bytes.len() {
            return std::sync::Arc::new(PreparedPayload {
                payload: compressed_buf,
                compressed: true,
                compact_bin: false,
            });
        }
    }
    std::sync::Arc::new(PreparedPayload {
        payload: raw_bytes.to_vec(),
        compressed: false,
        compact_bin: false,
    })
}

/// 一个会话的加解密状态
pub struct Session {
    enc: [u8; 32],
    mac_inner: Sha256,
    mac_outer: Sha256,
    send_ctr: u64,
    recv_ctr: u64,
    /// 方向位（1 = 宿主发出，2 = 客户端发出）。两个方向若用同一 nonce，
    /// 密钥流会重复，等于把两段明文异或在一起——必须分开。
    send_dir: u8,
    recv_dir: u8,
}

impl Session {
    /// 宿主持有的会话：发给客户端用方向 1，收客户端用方向 2
    pub fn host_side(room: &RoomKey, salt: &[u8]) -> Self {
        let mut s = Self::derive(room, salt);
        s.send_dir = 1;
        s.recv_dir = 2;
        s
    }

    /// 客户端持有：方向相反
    pub fn client_side(room: &RoomKey, salt: &[u8]) -> Self {
        let mut s = Self::derive(room, salt);
        s.send_dir = 2;
        s.recv_dir = 1;
        s
    }

    /// 拷一份只用于写（独立发送计数器）
    pub fn fork_sender(&self) -> Self {
        Self {
            enc: self.enc,
            mac_inner: self.mac_inner.clone(),
            mac_outer: self.mac_outer.clone(),
            send_ctr: 0,
            recv_ctr: 0,
            send_dir: self.send_dir,
            recv_dir: self.recv_dir,
        }
    }

    /// 由房间密钥与本连接的 salt 派生
    pub fn derive(room: &RoomKey, salt: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(b"sheetcore-wire-v2");
        h.update(salt);
        h.update(room.0);
        let base = h.finalize();
        let mut enc = Sha256::new();
        enc.update(b"enc");
        enc.update(base);
        let mut mac = Sha256::new();
        mac.update(b"mac");
        mac.update(base);
        let mut e = [0u8; 32];
        let mut m = [0u8; 32];
        e.copy_from_slice(&enc.finalize());
        m.copy_from_slice(&mac.finalize());
        let mut ipad = [0x36u8; 64];
        let mut opad = [0x5cu8; 64];
        for i in 0..32 {
            ipad[i] ^= m[i];
            opad[i] ^= m[i];
        }
        let mut mac_inner = Sha256::new();
        mac_inner.update(ipad);
        let mut mac_outer = Sha256::new();
        mac_outer.update(opad);
        Self {
            enc: e,
            mac_inner,
            mac_outer,
            send_ctr: 0,
            recv_ctr: 0,
            send_dir: 0,
            recv_dir: 0,
        }
    }

    #[inline]
    fn mac_tag(&self, msg: &[u8]) -> [u8; 32] {
        let mut inner = self.mac_inner.clone();
        inner.update(msg);
        let inner_digest = inner.finalize();
        let mut outer = self.mac_outer.clone();
        outer.update(inner_digest);
        let out = outer.finalize();
        let mut r = [0u8; 32];
        r.copy_from_slice(&out);
        r
    }

    fn nonce(dir: u8, ctr: u32) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[0] = dir;
        n[4..8].copy_from_slice(&ctr.to_le_bytes());
        n
    }

    fn nonce_v3(flags: u8, ctr: u64) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[0] = flags;
        n[1] = BINARY_V3_MAGIC;
        n[4..12].copy_from_slice(&ctr.to_le_bytes());
        n
    }

    #[inline]
    fn v3_flags(&self, compressed: bool, compact_bin: bool) -> u8 {
        V3_FLAG_MARKER
            | (if compact_bin { V3_FLAG_COMPACT_BIN } else { 0 })
            | ((self.send_dir & 0x03) << 1)
            | u8::from(compressed)
    }

    /// 使用预处理的共享负载加密一帧 v2 文本帧（与 `seal`/`open` 线缆格式 100% 兼容）
    pub fn seal_prepared(&mut self, prepared: &PreparedPayload) -> String {
        debug_assert!(!prepared.compact_bin, "v2 text frame requires UTF-8 JSON payload");
        self.send_ctr = self.send_ctr.wrapping_add(1);
        let ctr = self.send_ctr as u32;
        let wire_dir = if prepared.compressed {
            self.send_dir | DIR_COMPRESSED_FLAG
        } else {
            self.send_dir
        };
        let mut framed = Vec::with_capacity(5 + prepared.payload.len() + 32);
        framed.push(wire_dir);
        framed.extend_from_slice(&ctr.to_le_bytes());
        framed.extend_from_slice(&prepared.payload);
        apply_keystream(&self.enc, ctr, &Self::nonce(wire_dir, ctr), &mut framed[5..]);
        let tag = self.mac_tag(&framed);
        framed.extend_from_slice(&tag);
        b64_encode(&framed)
    }

    /// 使用预处理的共享负载封装一帧 v3 二进制帧并直接写入复用缓冲区 `out`（零二次分配、无 Base64 膨胀）。
    ///
    /// 线缆格式：`[0x02][4B LE body_len][1B flags: bit0=compressed, bit3=compact_bin][8B LE counter][ciphertext][32B HMAC-SHA256]`。
    /// 其中 `HMAC-SHA256` 覆盖 `flags || counter || ciphertext`。
    pub fn seal_prepared_binary_into(&mut self, prepared: &PreparedPayload, out: &mut Vec<u8>) {
        self.send_ctr = self.send_ctr.wrapping_add(1);
        let ctr = self.send_ctr;
        let flags = self.v3_flags(prepared.compressed, prepared.compact_bin);
        let body_len = 1 + 8 + prepared.payload.len() + 32;
        let body_len_u32 =
            u32::try_from(body_len).expect("v3 binary frame body length exceeds u32::MAX");
        out.clear();
        out.reserve(5 + body_len);
        out.push(BINARY_V3_MAGIC);
        out.extend_from_slice(&body_len_u32.to_le_bytes());
        out.push(flags);
        out.extend_from_slice(&ctr.to_le_bytes());
        let ct_start = out.len();
        out.extend_from_slice(&prepared.payload);
        apply_keystream(&self.enc, 1, &Self::nonce_v3(flags, ctr), &mut out[ct_start..]);
        let tag = self.mac_tag(&out[5..]);
        out.extend_from_slice(&tag);
    }

    /// 使用预处理的共享负载封装一帧完整 v3 二进制帧（含 `0x02` 魔数与 4 字节小端长度头）
    pub fn seal_prepared_binary(&mut self, prepared: &PreparedPayload) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + 1 + 8 + prepared.payload.len() + 32);
        self.seal_prepared_binary_into(prepared, &mut out);
        out
    }

    fn seal_bytes_v3_into(
        &mut self,
        raw_bytes: &[u8],
        compress: bool,
        compact_bin: bool,
        out: &mut Vec<u8>,
    ) {
        self.send_ctr = self.send_ctr.wrapping_add(1);
        let ctr = self.send_ctr;
        out.clear();
        out.reserve(5 + 1 + 8 + raw_bytes.len().min(65_536) + 32);
        // 占位 14 字节：[0x02][4B LE body_len][1B flags][8B LE counter]
        out.extend_from_slice(&[BINARY_V3_MAGIC, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&ctr.to_le_bytes());
        let ct_start = 14usize;
        let compressed = if compress && raw_bytes.len() >= COMPRESS_MIN_BYTES {
            let compressed_ok = {
                let mut enc = flate2::write::DeflateEncoder::new(
                    &mut *out,
                    flate2::Compression::fast(),
                );
                enc.write_all(raw_bytes).is_ok() && enc.finish().is_ok()
            };
            if compressed_ok && out.len() - ct_start < raw_bytes.len() {
                true
            } else {
                out.truncate(ct_start);
                out.extend_from_slice(raw_bytes);
                false
            }
        } else {
            out.extend_from_slice(raw_bytes);
            false
        };
        let flags = self.v3_flags(compressed, compact_bin);
        let body_len = (out.len() - 5) + 32;
        let body_len_u32 =
            u32::try_from(body_len).expect("v3 binary frame body length exceeds u32::MAX");
        out[1..5].copy_from_slice(&body_len_u32.to_le_bytes());
        out[5] = flags;
        apply_keystream(&self.enc, 1, &Self::nonce_v3(flags, ctr), &mut out[ct_start..]);
        let tag = self.mac_tag(&out[5..]);
        out.extend_from_slice(&tag);
    }

    /// 加密一帧明文 JSON 为 v3 二进制帧并直接写入复用缓冲区 `out`
    pub fn seal_binary_into(&mut self, plain: &str, compress: bool, out: &mut Vec<u8>) {
        self.seal_bytes_v3_into(plain.as_bytes(), compress, false, out);
    }

    /// 加密一帧紧凑二进制内层载荷为 v3 二进制帧并直接写入复用缓冲区 `out`
    pub fn seal_compact_binary_into(&mut self, raw_bytes: &[u8], compress: bool, out: &mut Vec<u8>) {
        self.seal_bytes_v3_into(raw_bytes, compress, true, out);
    }

    /// 加密一帧为完整 v3 二进制帧（默认启用 deflate 自适应压缩）
    pub fn seal_binary(&mut self, plain: &str) -> Vec<u8> {
        let mut out = Vec::new();
        self.seal_binary_into(plain, true, &mut out);
        out
    }

    /// 加密一帧（明文 JSON → 可选 deflate 压缩 → 原地加密 + 零拷贝 HMAC → base64）
    pub fn seal(&mut self, plain: &str) -> String {
        self.seal_with_compression(plain, true)
    }

    /// 加密一帧，允许调用方显式控制是否尝试 deflate 压缩
    pub fn seal_with_compression(&mut self, plain: &str, compress: bool) -> String {
        self.send_ctr = self.send_ctr.wrapping_add(1);
        let ctr = self.send_ctr as u32;
        let raw_bytes = plain.as_bytes();
        let mut framed = Vec::with_capacity(5 + raw_bytes.len().min(65_536) + 32);
        framed.extend_from_slice(&[0, 0, 0, 0, 0]);
        let wire_dir = if compress && raw_bytes.len() >= COMPRESS_MIN_BYTES {
            let compressed_ok = {
                let mut enc = flate2::write::DeflateEncoder::new(
                    &mut framed,
                    flate2::Compression::fast(),
                );
                enc.write_all(raw_bytes).is_ok() && enc.finish().is_ok()
            };
            if compressed_ok && framed.len() - 5 < raw_bytes.len() {
                self.send_dir | DIR_COMPRESSED_FLAG
            } else {
                framed.truncate(5);
                framed.extend_from_slice(raw_bytes);
                self.send_dir
            }
        } else {
            framed.extend_from_slice(raw_bytes);
            self.send_dir
        };
        framed[0] = wire_dir;
        framed[1..5].copy_from_slice(&ctr.to_le_bytes());
        apply_keystream(&self.enc, ctr, &Self::nonce(wire_dir, ctr), &mut framed[5..]);
        let tag = self.mac_tag(&framed);
        framed.extend_from_slice(&tag);
        b64_encode(&framed)
    }

    /// 解密并验签 v2 文本帧；计数器必须比上一帧大
    pub fn open(&mut self, frame: &str) -> Option<String> {
        let mut raw = b64_decode(frame)?;
        if raw.len() < 5 + 32 {
            return None;
        }
        let wire_dir = raw[0];
        let compressed = (wire_dir & DIR_COMPRESSED_FLAG) != 0;
        let dir = wire_dir & !DIR_COMPRESSED_FLAG;
        if dir != self.recv_dir {
            return None; // 方向的密钥相同，方向位是防重放/防回环的关键
        }
        let ctr = u32::from_le_bytes([raw[1], raw[2], raw[3], raw[4]]);
        let ctr_u64 = u64::from(ctr);
        let mac_offset = raw.len() - 32;
        let expect = self.mac_tag(&raw[..mac_offset]);
        // 定长比较，避免时序泄露
        let tag = &raw[mac_offset..];
        let mut diff = 0u8;
        for i in 0..32 {
            diff |= expect[i] ^ tag[i];
        }
        if diff != 0 {
            return None;
        }
        if ctr_u64 <= self.recv_ctr {
            return None; // 重放或乱序
        }
        self.recv_ctr = ctr_u64;
        apply_keystream(&self.enc, ctr, &Self::nonce(wire_dir, ctr), &mut raw[5..mac_offset]);
        if compressed {
            let mut dec = flate2::read::DeflateDecoder::new(&raw[5..mac_offset])
                .take(MAX_DECOMPRESSED_BYTES + 1);
            let mut out = Vec::with_capacity((mac_offset - 5).saturating_mul(4).min(4 * 1024 * 1024));
            dec.read_to_end(&mut out).ok()?;
            if out.len() as u64 > MAX_DECOMPRESSED_BYTES {
                return None;
            }
            String::from_utf8(out).ok()
        } else {
            raw.truncate(mac_offset);
            raw.drain(..5);
            String::from_utf8(raw).ok()
        }
    }

    fn verify_binary_body(&self, body: &[u8]) -> Option<(u8, u64, bool, usize)> {
        if body.len() < BINARY_V3_MIN_BODY_LEN {
            return None;
        }
        let flags = body[0];
        if (flags & V3_FLAG_MARKER) == 0 || (flags & !V3_FLAG_ALLOWED_MASK) != 0 {
            return None;
        }
        let dir = (flags & V3_FLAG_DIR_MASK) >> 1;
        if dir != self.recv_dir {
            return None;
        }
        let compressed = (flags & V3_FLAG_COMPRESSED) != 0;
        let ctr = u64::from_le_bytes(body[1..9].try_into().ok()?);
        let mac_offset = body.len() - 32;
        let expect = self.mac_tag(&body[..mac_offset]);
        let tag = &body[mac_offset..];
        let mut diff = 0u8;
        for i in 0..32 {
            diff |= expect[i] ^ tag[i];
        }
        if diff != 0 {
            return None;
        }
        if ctr <= self.recv_ctr {
            return None;
        }
        Some((flags, ctr, compressed, mac_offset))
    }

    /// 解密并验签 v3 二进制帧体（不含 5 字节 `[0x02][4B len]` 外层头，即 `[flags][8B ctr][ciphertext][32B HMAC]`）。
    pub fn open_binary(&mut self, body: &[u8]) -> Option<String> {
        let (flags, ctr, compressed, mac_offset) = self.verify_binary_body(body)?;
        if (flags & V3_FLAG_COMPACT_BIN) != 0 {
            return None;
        }
        self.recv_ctr = ctr;
        let mut plain = body[9..mac_offset].to_vec();
        apply_keystream(&self.enc, 1, &Self::nonce_v3(flags, ctr), &mut plain);
        if compressed {
            let mut dec = flate2::read::DeflateDecoder::new(plain.as_slice())
                .take(MAX_DECOMPRESSED_BYTES + 1);
            let mut out = Vec::with_capacity(plain.len().saturating_mul(4).min(4 * 1024 * 1024));
            dec.read_to_end(&mut out).ok()?;
            if out.len() as u64 > MAX_DECOMPRESSED_BYTES {
                return None;
            }
            String::from_utf8(out).ok()
        } else {
            String::from_utf8(plain).ok()
        }
    }

    /// 零拷贝原地解密并验签已拥有缓冲区的 v3 二进制帧体（避免密文二次复制）。
    pub fn open_binary_owned(&mut self, body: Vec<u8>) -> Option<String> {
        match self.open_binary_frame_owned(body)? {
            DecodedWireFrame::Text(s) => Some(s),
            DecodedWireFrame::Compact(_) => None,
        }
    }

    /// 零拷贝原地解密并验签 v3 二进制帧体，自动按 `V3_FLAG_COMPACT_BIN` 区分紧凑二进制内层帧与 UTF-8 JSON 帧。
    pub fn open_binary_frame_owned(&mut self, mut body: Vec<u8>) -> Option<DecodedWireFrame> {
        let (flags, ctr, compressed, mac_offset) = self.verify_binary_body(&body)?;
        let compact_bin = (flags & V3_FLAG_COMPACT_BIN) != 0;
        self.recv_ctr = ctr;
        apply_keystream(&self.enc, 1, &Self::nonce_v3(flags, ctr), &mut body[9..mac_offset]);
        let raw = if compressed {
            let mut dec = flate2::read::DeflateDecoder::new(&body[9..mac_offset])
                .take(MAX_DECOMPRESSED_BYTES + 1);
            let mut out =
                Vec::with_capacity((mac_offset - 9).saturating_mul(4).min(4 * 1024 * 1024));
            dec.read_to_end(&mut out).ok()?;
            if out.len() as u64 > MAX_DECOMPRESSED_BYTES {
                return None;
            }
            out
        } else {
            body.truncate(mac_offset);
            body.drain(..9);
            body
        };
        if compact_bin {
            Some(DecodedWireFrame::Compact(raw))
        } else {
            String::from_utf8(raw).ok().map(DecodedWireFrame::Text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .flat_map(|w| {
                let b = u8::from_str_radix(w, 16).unwrap();
                vec![b]
            })
            .collect()
    }

    #[test]
    fn rfc8439_block_vector() {
        // RFC 8439 §2.3.2
        let key: [u8; 32] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ];
        let nonce = [0u8, 0, 0, 0, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        // RFC 里的 nonce 是 00:00:00:09:00:00:00:4a:00:00:00:00，counter=1
        let nonce = [0u8, 0, 0, 0x09, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let block = chacha20_block(&key, 1, &nonce);
        let expect = hex(
            "10 f1 e7 e4 d1 3b 59 15 50 0f dd 1f a3 20 71 c4
             c7 d1 f4 c7 33 c0 68 03 04 22 aa 9a c3 d4 6c 4e
             d2 82 64 46 07 9f aa 09 14 c2 d7 05 d9 8b 02 a2
             b5 12 9c d1 de 16 4e b9 cb d0 83 e8 a2 50 3c 4e",
        );
        assert_eq!(block.to_vec(), expect, "ChaCha20 块输出与 RFC 不符");
    }

    #[test]
    fn rfc8439_encryption_vector() {
        // RFC 8439 §2.4.2：明文 "Ladies and Gentlemen of the class of '99..."
        let key: [u8; 32] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ];
        let nonce = [0u8, 0, 0, 0, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let mut data = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.".to_vec();
        apply_keystream(&key, 1, &nonce, &mut data);
        let expect = hex(
            "6e 2e 35 9a 25 68 f9 80 41 ba 07 28 dd 0d 69 81
             e9 7e 7a ec 1d 43 60 c2 0a 27 af cc fd 9f ae 0b
             f9 1b 65 c5 52 47 33 ab 8f 59 3d ab cd 62 b3 57
             16 39 d6 24 e6 51 52 ab 8f 53 0c 35 9f 08 61 d8
             07 ca 0d bf 50 0d 6a 61 56 a3 8e 08 8a 22 b6 5e
             52 bc 51 4d 16 cc f8 06 81 8c e9 1a b7 79 37 36
             5a f9 0b bf 74 a3 5b e6 b4 0b 8e ed f2 78 5e 42
             87 4d",
        );
        assert_eq!(data, expect, "ChaCha20 加密与 RFC 不符");
    }

    #[test]
    fn session_round_trip_and_tamper() {
        let room = RoomKey::derive_with_iters("口令123", b"room", 10);
        let mut a = Session::host_side(&room, b"salt");
        let mut b = Session::client_side(&room, b"salt");
        let frame = a.seal("{\"t\":\"Hello\",\"site\":7}");
        let out = b.open(&frame).expect("应能解开");
        assert_eq!(out, "{\"t\":\"Hello\",\"site\":7}");
        // 篡改
        let mut bad = frame.clone().into_bytes();
        bad[10] = if bad[10] == b'A' { b'B' } else { b'A' };
        let bad = String::from_utf8(bad).unwrap();
        assert!(b.open(&bad).is_none(), "被改过的帧必须拒绝");
        // 重放
        assert!(b.open(&frame).is_none(), "重放同一帧必须拒绝");
        // 口令不对
        let wrong = RoomKey::derive_with_iters("错口令", b"room", 10);
        let mut c = Session::client_side(&wrong, b"salt");
        let f2 = a.seal("{}");
        assert!(c.open(&f2).is_none());
        // 同口令、不同房间 salt：密钥不同
        let other_room = RoomKey::derive_with_iters("口令123", b"other", 10);
        let mut d = Session::client_side(&other_room, b"salt");
        assert!(d.open(&a.seal("{}")).is_none());
        assert_ne!(room.proof(42), other_room.proof(42));
        assert_ne!(room.proof(42), room.proof(43));
    }

    /// RFC 7914 §11 / 通用 PBKDF2-HMAC-SHA256 测试向量（已用 Python hashlib 复核）
    #[test]
    fn pbkdf2_hmac_sha256_vectors() {
        let hex = |b: [u8; 32]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(
            hex(pbkdf2_hmac_sha256(b"password", b"salt", 1)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex(pbkdf2_hmac_sha256(b"password", b"salt", 2)),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
        assert_eq!(
            hex(pbkdf2_hmac_sha256(b"password", b"salt", 4096)),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
        assert_eq!(
            hex(pbkdf2_hmac_sha256(
                b"passwordPASSWORDpassword",
                b"saltSALTsaltSALTsaltSALTsaltSALTsalt",
                4096
            )),
            "348c89dbcbd32b2f32d814b8116e84cf2b17347ebc1800181c4e2a1fb8dd53e1"
        );
        // 超过一个块（64 字节）的口令要先哈希成 HMAC 密钥
        let long_pw = [b'k'; 100];
        assert_eq!(
            hex(pbkdf2_hmac_sha256(&long_pw, b"salt", 3)),
            LONG_PW_VECTOR
        );
    }

    const LONG_PW_VECTOR: &str = "219c68bb88f1010f4a763b7c8fd58639c2280baf6820814c6d404891fcd85c3d";

    #[test]
    fn base64_round_trip() {
        for len in 0..40usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 % 251) as u8).collect();
            let enc = b64_encode(&data);
            let dec = b64_decode(&enc).expect("解码失败");
            assert_eq!(dec, data, "长度 {len} 往返不一致");
        }
        assert_eq!(b64_encode(b"Man"), "TWFu");
        assert_eq!(b64_encode(b"Ma"), "TWE=");
        assert_eq!(b64_encode(b"M"), "TQ==");
    }

    #[test]
    fn compressed_large_frames_shrink_round_trip_and_reject_tamper() {
        let room = RoomKey::derive_with_iters("perf-room", b"room-salt", 10);
        let mut host = Session::host_side(&room, b"conn-salt");
        let mut client = Session::client_side(&room, b"conn-salt");

        let mut big = String::from("{\"t\":\"Ops\",\"ops\":[");
        for i in 0..512 {
            if i > 0 {
                big.push(',');
            }
            big.push_str(&format!(
                "{{\"sheet\":0,\"row\":{i},\"col\":2,\"value\":{{\"Number\":12345.67}},\"formula\":null,\"ts\":{},\"site\":1}}",
                100 + i
            ));
        }
        big.push_str("]}");
        assert!(big.len() > 40_000);

        let sealed = host.seal(&big);
        assert!(
            sealed.len() < big.len() / 3,
            "repetitive JSON frame should compress >3x even after base64: {} vs {}",
            sealed.len(),
            big.len()
        );
        let raw = b64_decode(&sealed).expect("valid base64");
        assert_ne!(raw[0] & DIR_COMPRESSED_FLAG, 0, "compressed flag must be set");

        let mut tampered = sealed.clone().into_bytes();
        let mid = tampered.len() / 2;
        tampered[mid] = if tampered[mid] == b'X' { b'Y' } else { b'X' };
        let tampered = String::from_utf8(tampered).unwrap();
        assert!(client.open(&tampered).is_none(), "tampered compressed frame must fail MAC");

        let opened = client.open(&sealed).expect("untampered compressed frame opens");
        assert_eq!(opened, big);
        assert!(client.open(&sealed).is_none(), "replay of compressed frame must fail");
    }

    #[test]
    fn prepared_payload_shares_single_compression_and_respects_compress_flag() {
        let room = RoomKey::derive_with_iters("perf-room", b"room-salt", 10);
        let mut h1 = Session::host_side(&room, b"conn-1");
        let mut c1 = Session::client_side(&room, b"conn-1");
        let mut h2 = Session::host_side(&room, b"conn-2");
        let mut c2 = Session::client_side(&room, b"conn-2");

        let mut big = String::from("{\"t\":\"Ops\",\"ops\":[");
        for i in 0..256 {
            if i > 0 {
                big.push(',');
            }
            big.push_str(&format!(
                "{{\"sheet\":0,\"row\":{i},\"col\":0,\"value\":{{\"Number\":99.5}},\"formula\":null,\"ts\":{},\"site\":1}}",
                10 + i
            ));
        }
        big.push_str("]}");

        let p_comp = prepare_payload(&big, true);
        assert!(p_comp.is_compressed());
        assert!(p_comp.payload_len() < big.len() / 4);

        let f1 = h1.seal_prepared(&p_comp);
        let f2 = h2.seal_prepared(&p_comp);
        assert_ne!(f1, f2, "distinct peer sessions must produce distinct ciphertexts");
        assert_eq!(c1.open(&f1).as_deref(), Some(big.as_str()));
        assert_eq!(c2.open(&f2).as_deref(), Some(big.as_str()));

        // High-entropy / UpdateSlice bypasses deflate when compress = false
        let p_raw = prepare_payload(&big, false);
        assert!(!p_raw.is_compressed());
        assert_eq!(p_raw.payload_len(), big.len());
        let f_raw = h1.seal_prepared(&p_raw);
        let raw_bytes = b64_decode(&f_raw).expect("valid base64");
        assert_eq!(raw_bytes[0] & DIR_COMPRESSED_FLAG, 0, "uncompressed flag must be clear");
        assert_eq!(c1.open(&f_raw).as_deref(), Some(big.as_str()));

        // Byte-identical wire output between seal() and seal_prepared() for same session state
        let mut h_a = Session::host_side(&room, b"same-conn");
        let mut h_b = Session::host_side(&room, b"same-conn");
        assert_eq!(h_a.seal(&big), h_b.seal_prepared(&p_comp));
    }

    /// 标量版 ChaCha20 参考实现，用于与 4-way 向量化 `apply_keystream` 逐字节交叉比对
    fn apply_keystream_scalar_ref(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
        let mut block_idx = 0u32;
        for chunk in data.chunks_mut(64) {
            let ks = chacha20_block(key, counter.wrapping_add(block_idx), nonce);
            for (dst, src) in chunk.iter_mut().zip(ks.iter()) {
                *dst ^= *src;
            }
            block_idx = block_idx.wrapping_add(1);
        }
    }

    #[test]
    fn chacha20_4way_matches_scalar_across_lengths_and_counter_wrap() {
        let key: [u8; 32] = std::array::from_fn(|i| ((i * 19 + 7) & 0xff) as u8);
        let nonce: [u8; 12] = std::array::from_fn(|i| ((i * 31 + 3) & 0xff) as u8);
        let lengths = [
            0usize, 1, 63, 64, 65, 127, 128, 129, 255, 256, 257, 511, 512, 513, 1023, 1024, 1025,
            1280, 2048, 4096,
        ];
        let counters = [0u32, 1, 42, u32::MAX - 17, u32::MAX - 15, u32::MAX - 2, u32::MAX - 1, u32::MAX];

        for &ctr in &counters {
            for &len in &lengths {
                let plain: Vec<u8> = (0..len)
                    .map(|i| ((i.wrapping_mul(131) ^ (ctr as usize)) & 0xff) as u8)
                    .collect();
                let mut fast = plain.clone();
                let mut scalar = plain.clone();
                apply_keystream(&key, ctr, &nonce, &mut fast);
                apply_keystream_scalar_ref(&key, ctr, &nonce, &mut scalar);
                assert_eq!(
                    fast, scalar,
                    "4-way ChaCha20 与标量参考输出不一致：len={len}, counter={ctr:#x}"
                );
                // 自反解密回到原文
                apply_keystream(&key, ctr, &nonce, &mut fast);
                assert_eq!(fast, plain, "4-way ChaCha20 往返解密失败：len={len}, counter={ctr:#x}");
            }
        }
    }

    #[test]
    fn binary_v3_round_trip_compression_tamper_replay_and_v2_continuity() {
        let room = RoomKey::derive_with_iters("v3-room-pw", b"room-salt-v3", 10);
        let mut host = Session::host_side(&room, b"conn-v3");
        let mut client = Session::client_side(&room, b"conn-v3");

        // 1. 握手帧走 v2 文本行，随后无缝切入 v3 二进制帧（共享单调递增 counter）
        let welcome = r#"{"t":"Welcome","site":2,"doc":"d","peers":[],"binary_v3":true}"#;
        let v2_frame = host.seal(welcome);
        assert_eq!(client.open(&v2_frame).as_deref(), Some(welcome));

        // 2. 小帧（非压缩分支）：bit0 == 0
        let small = r#"{"t":"Op","row":1,"col":2,"value":"Empty","ts":2,"site":1}"#;
        let bin_small = host.seal_binary(small);
        assert_eq!(bin_small[0], BINARY_V3_MAGIC);
        let body_len = u32::from_le_bytes(bin_small[1..5].try_into().unwrap()) as usize;
        assert_eq!(body_len, bin_small.len() - 5);
        assert_eq!(bin_small[5] & V3_FLAG_COMPRESSED, 0, "小帧不得置位压缩标志");
        assert_eq!(client.open_binary(&bin_small[5..]).as_deref(), Some(small));
        // 重放必须被拒绝
        assert!(client.open_binary(&bin_small[5..]).is_none(), "v3 重放帧必须拒绝");
        // 旧 v2 握手帧也不能在切入 v3 后重放
        assert!(client.open(&v2_frame).is_none(), "旧 v2 帧重放必须拒绝");

        // 3. 大帧（压缩分支）+ PreparedPayload 同时复用于 v2 和 v3，验证线缆体积缩减 ~25%
        let mut big = String::from("{\"t\":\"Ops\",\"ops\":[");
        for i in 0..400 {
            if i > 0 {
                big.push(',');
            }
            big.push_str(&format!(
                "{{\"sheet\":0,\"row\":{i},\"col\":1,\"value\":{{\"Number\":{i}.25}},\"formula\":null,\"ts\":{},\"site\":1}}",
                100 + i
            ));
        }
        big.push_str("]}");
        let prepared = prepare_payload(&big, true);
        assert!(prepared.is_compressed());

        let mut host_v2 = Session::host_side(&room, b"size-cmp");
        let mut host_v3 = Session::host_side(&room, b"size-cmp");
        let v2_wire_len = host_v2.seal_prepared(&prepared).len() + 1; // + '\n'
        let mut scratch = Vec::new();
        host_v3.seal_prepared_binary_into(&prepared, &mut scratch);
        let v3_wire_len = scratch.len();
        assert!(
            v3_wire_len * 100 <= v2_wire_len * 78,
            "v3 二进制帧应相比 v2 Base64 缩减至少 22% 体积：v3={v3_wire_len} vs v2={v2_wire_len}"
        );

        // 实际用 host/client 会话走 seal_prepared_binary 与 open_binary_owned
        let bin_big = host.seal_prepared_binary(&prepared);
        assert_eq!(bin_big[0], BINARY_V3_MAGIC);
        assert_ne!(bin_big[5] & V3_FLAG_COMPRESSED, 0, "大帧压缩标志必须置位");

        // 4. 各类篡改必须被拒绝（flags / counter / ciphertext / tag / 截断）
        let body = &bin_big[5..];
        for &idx in &[0usize, 1, 8, 9, body.len() / 2, body.len() - 1] {
            let mut bad = body.to_vec();
            bad[idx] ^= 0x01;
            assert!(
                client.open_binary(&bad).is_none(),
                "篡改 body[{idx}] 必须被 HMAC 或标志位校验拒绝"
            );
        }
        assert!(client.open_binary(&body[..BINARY_V3_MIN_BODY_LEN - 1]).is_none());

        // 5. 跨方向反射拒绝（host 自己不得解开自己发出的 v3 帧）
        assert!(host.open_binary(body).is_none(), "同向反射 v3 帧必须拒绝");

        // 6. 跨版本混淆拒绝（把 v3 body 做 Base64 喂给 v2 open，或把 v2 原始帧喂给 open_binary）
        assert!(
            client.open(&b64_encode(body)).is_none(),
            "v3 帧体伪装为 v2 Base64 必须拒绝"
        );
        let v2_raw = b64_decode(&v2_frame).unwrap();
        assert!(
            client.open_binary(&v2_raw).is_none(),
            "v2 帧体伪装为 v3 二进制帧必须拒绝"
        );

        // 7. 未篡改的大压缩帧通过 open_binary_owned 正常解开
        assert_eq!(client.open_binary_owned(body.to_vec()).as_deref(), Some(big.as_str()));

        // 8. 不压缩的大帧（compress=false，如 UpdateSlice）
        let raw_prepared = prepare_payload(&big, false);
        assert!(!raw_prepared.is_compressed());
        let mut reuse_buf = Vec::new();
        host.seal_prepared_binary_into(&raw_prepared, &mut reuse_buf);
        assert_eq!(reuse_buf[5] & V3_FLAG_COMPRESSED, 0);
        assert_eq!(client.open_binary(&reuse_buf[5..]).as_deref(), Some(big.as_str()));

        // 9. 错误口令会话拒绝解开
        let wrong_room = RoomKey::derive_with_iters("wrong-pw", b"room-salt-v3", 10);
        let mut attacker = Session::client_side(&wrong_room, b"conn-v3");
        let f_next = host.seal_binary("{\"t\":\"Bye\",\"site\":1}");
        assert!(attacker.open_binary(&f_next[5..]).is_none());
        assert_eq!(client.open_binary(&f_next[5..]).as_deref(), Some("{\"t\":\"Bye\",\"site\":1}"));

        // 10. seal_binary 与 seal_prepared_binary 对同一会话状态逐字节一致
        let mut h_a = Session::host_side(&room, b"same-v3");
        let mut h_b = Session::host_side(&room, b"same-v3");
        assert_eq!(h_a.seal_binary(&big), h_b.seal_prepared_binary(&prepared));
    }

    #[test]
    fn v3_compact_binary_flag_roundtrip_and_type_separation() {
        let room = RoomKey::derive_with_iters("compact-pw", b"compact-salt", 8);
        let mut host = Session::host_side(&room, b"conn-compact");
        let mut client = Session::client_side(&room, b"conn-compact");

        // 1. 小型紧凑二进制帧（不压缩）
        let raw_small = vec![0x08, 42, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0];
        let mut scratch = Vec::new();
        host.seal_compact_binary_into(&raw_small, true, &mut scratch);
        assert_eq!(scratch[0], BINARY_V3_MAGIC);
        assert_ne!(scratch[5] & V3_FLAG_COMPACT_BIN, 0);
        // JSON 专用解码入口必须拒绝 V3_FLAG_COMPACT_BIN（类型安全隔离）
        let mut probe_json = Session::client_side(&room, b"conn-compact");
        assert!(probe_json.open_binary(&scratch[5..]).is_none());
        match client.open_binary_frame_owned(scratch[5..].to_vec()) {
            Some(DecodedWireFrame::Compact(got)) => assert_eq!(got, raw_small),
            other => panic!("expected Compact frame, got {other:?}"),
        }

        // 2. 大型紧凑二进制帧（自动 deflate 压缩 + prepare_compact_payload 共享广播）
        let mut raw_big = Vec::with_capacity(4096);
        for i in 0..512u32 {
            raw_big.extend_from_slice(&i.to_le_bytes());
            raw_big.extend_from_slice(&0u32.to_le_bytes());
        }
        let prep = prepare_compact_payload(raw_big.clone(), true);
        assert!(prep.is_compact_bin());
        assert!(prep.is_compressed());
        host.seal_prepared_binary_into(&prep, &mut scratch);
        assert_ne!(scratch[5] & (V3_FLAG_COMPACT_BIN | V3_FLAG_COMPRESSED), 0);
        match client.open_binary_frame_owned(scratch[5..].to_vec()) {
            Some(DecodedWireFrame::Compact(got)) => assert_eq!(got, raw_big),
            other => panic!("expected decompressed Compact frame, got {other:?}"),
        }
    }
}
