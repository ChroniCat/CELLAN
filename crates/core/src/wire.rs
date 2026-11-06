//! 局域网传输加密：ChaCha20 流加密 + HMAC-SHA256 认证（encrypt-then-MAC）。
//!
//! 为什么自己写：本机离线，拿不到 rustls / ring。但 ChaCha20 与 HMAC 都有公开的
//! 标准测试向量，实现对不对可以当场证明，不需要「看起来能跑」。
//!
//! 密钥派生：口令 + 宿主随机 salt → `SHA256("sheetcore-v1" || salt || 口令)`，
//! 再各自展开出加密钥与认证钥（不同前缀，互不相关）。
//! 每帧独立计数器作为 nonce，接收端要求严格递增（防重放/重排）。

use sha2::{Digest, Sha256};

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

/// 原地加/解密（同一操作）
pub fn apply_keystream(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    let mut block = chacha20_block(key, counter, nonce);
    for (i, b) in data.iter_mut().enumerate() {
        if i > 0 && i % 64 == 0 {
            block = chacha20_block(key, counter.wrapping_add((i / 64) as u32), nonce);
        }
        *b ^= block[i % 64];
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

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut rev = [255u8; 256];
    for (i, c) in B64.iter().enumerate() {
        rev[*c as usize] = i as u8;
    }
    let bytes: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut acc = 0u32;
    let mut n = 0;
    for b in bytes {
        let v = rev[b as usize];
        if v == 255 {
            return None;
        }
        acc = (acc << 6) | v as u32;
        n += 1;
        if n == 4 {
            out.push((acc >> 16) as u8);
            out.push((acc >> 8) as u8);
            out.push(acc as u8);
            acc = 0;
            n = 0;
        }
    }
    match n {
        2 => out.push((acc >> 4) as u8),
        3 => {
            out.push((acc >> 10) as u8);
            out.push((acc >> 2) as u8);
        }
        _ => {}
    }
    Some(out)
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
    pub fn host_side(password: &str, salt: &[u8]) -> Self {
        let mut s = Self::derive(password, salt);
        s.send_dir = 1;
        s.recv_dir = 2;
        s
    }

    /// 客户端持有：方向相反
    pub fn client_side(password: &str, salt: &[u8]) -> Self {
        let mut s = Self::derive(password, salt);
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

    /// 由口令与 salt 派生
    pub fn derive(password: &str, salt: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(b"sheetcore-wire-v1");
        h.update(salt);
        h.update(password.as_bytes());
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

    /// 加密一帧（明文 JSON → base64(计数器 | 密文 | tag)）
    pub fn seal(&mut self, plain: &str) -> String {
        self.send_ctr = self.send_ctr.wrapping_add(1);
        let ctr = self.send_ctr;
        let dir = self.send_dir;
        let mut buf = plain.as_bytes().to_vec();
        apply_keystream(&self.enc, ctr, &Self::nonce(dir, ctr), &mut buf);
        let mut framed = Vec::with_capacity(5 + buf.len() + 32);
        framed.push(dir);
        framed.extend_from_slice(&ctr.to_le_bytes());
        framed.extend_from_slice(&buf);
        let mut m = Vec::with_capacity(framed.len());
        m.extend_from_slice(&framed);
        let tag = hmac_sha256(&self.mac, &m);
        framed.extend_from_slice(&tag);
        b64_encode(&framed)
    }

    /// 解密并验签；计数器必须比上一帧大
    pub fn open(&mut self, frame: &str) -> Option<String> {
        let raw = b64_decode(frame)?;
        if raw.len() < 5 + 32 {
            return None;
        }
        let dir = raw[0];
        if dir != self.recv_dir {
            return None; // 方向的密钥相同，方向位是防重放/防回环的关键
        }
        let ctr = u32::from_le_bytes([raw[1], raw[2], raw[3], raw[4]]);
        let body_len = raw.len() - 5 - 32;
        let body = raw[5..5 + body_len].to_vec();
        let tag = &raw[5 + body_len..];
        let mut m = Vec::with_capacity(raw.len() - 32);
        m.extend_from_slice(&raw[..5]);
        m.extend_from_slice(&body);
        let expect = hmac_sha256(&self.mac, &m);
        // 定长比较，避免时序泄露
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
        let mut buf = body;
        apply_keystream(&self.enc, ctr, &Self::nonce(dir, ctr), &mut buf);
        String::from_utf8(buf).ok()
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
        let mut a = Session::host_side("口令123", b"salt");
        let mut b = Session::client_side("口令123", b"salt");
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
        let mut c = Session::client_side("错口令", b"salt");
        let f2 = a.seal("{}");
        assert!(c.open(&f2).is_none());
    }

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
}
