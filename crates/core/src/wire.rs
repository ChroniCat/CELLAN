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

/// ChaCha20 的 32 位字
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

/// 生成一个 64 字节的密钥流块
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut st = [0u32; 16];
    st[0] = 0x6170_7865;
    st[1] = 0x3320_646e;
    st[2] = 0x7962_2d32;
    st[3] = 0x6b20_6574;
    for i in 0..8 {
        st[4 + i] = u32::from_le_bytes([key[i * 4], key[i * 4 + 1], key[i * 4 + 2], key[i * 4 + 3]]);
    }
    st[12] = counter;
    for i in 0..3 {
        st[13 + i] = u32::from_le_bytes([
            nonce[i * 4],
            nonce[i * 4 + 1],
            nonce[i * 4 + 2],
            nonce[i * 4 + 3],
        ]);
    }
    let init = st;
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

/// 原地加/解密（同一操作，按 64 字节完整块批量异或以利 SIMD 向量化）
pub fn apply_keystream(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    let mut chunks = data.chunks_exact_mut(64);
    let mut block_idx = 0u32;
    for chunk in &mut chunks {
        let ks = chacha20_block(key, counter.wrapping_add(block_idx), nonce);
        for (dst, src) in chunk.iter_mut().zip(ks.iter()) {
            *dst ^= *src;
        }
        block_idx = block_idx.wrapping_add(1);
    }
    let rem = chunks.into_remainder();
    if !rem.is_empty() {
        let ks = chacha20_block(key, counter.wrapping_add(block_idx), nonce);
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

/// 一个会话的加解密状态
pub struct Session {
    enc: [u8; 32],
    mac: [u8; 32],
    send_ctr: u32,
    recv_ctr: u32,
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
            mac: self.mac,
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
        Self { enc: e, mac: m, send_ctr: 0, recv_ctr: 0, send_dir: 0, recv_dir: 0 }
    }

    fn nonce(dir: u8, ctr: u32) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[0] = dir;
        n[4..8].copy_from_slice(&ctr.to_le_bytes());
        n
    }

    /// 加密一帧（明文 JSON → 可选 deflate 压缩 → 原地加密 + 零拷贝 HMAC → base64）
    pub fn seal(&mut self, plain: &str) -> String {
        self.send_ctr = self.send_ctr.wrapping_add(1);
        let ctr = self.send_ctr;
        let raw_bytes = plain.as_bytes();
        let mut framed = Vec::with_capacity(5 + raw_bytes.len().min(65_536) + 32);
        framed.extend_from_slice(&[0, 0, 0, 0, 0]);
        let wire_dir = if raw_bytes.len() >= COMPRESS_MIN_BYTES {
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
        let tag = hmac_sha256(&self.mac, &framed);
        framed.extend_from_slice(&tag);
        b64_encode(&framed)
    }

    /// 解密并验签；计数器必须比上一帧大
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
        let mac_offset = raw.len() - 32;
        let expect = hmac_sha256(&self.mac, &raw[..mac_offset]);
        // 定长比较，避免时序泄露
        let tag = &raw[mac_offset..];
        let mut diff = 0u8;
        for i in 0..32 {
            diff |= expect[i] ^ tag[i];
        }
        if diff != 0 {
            return None;
        }
        if ctr <= self.recv_ctr {
            return None; // 重放或乱序
        }
        self.recv_ctr = ctr;
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
}
