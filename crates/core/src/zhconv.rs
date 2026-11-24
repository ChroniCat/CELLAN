//! 中文简繁转换：内置 OpenCC 的字/词表（`assets/zhconv.bin`，由
//! `scripts/gen_zhconv.py` 生成），做最长匹配替换。
//!
//! 性能设计：
//! - `OnceLock` 惰性初始化：未使用简繁转换的会话零堆内存、零启动耗时。
//! - 兼容未压缩 `ZHC1`（零拷贝直接借用 `.rdata` 静态 UTF-8 切片，消除 ~9.4 万次 `String` 堆分配）
//!   与 zlib 压缩 `ZHC2`（`b"ZHC2" + u32 原始长度 + zlib 流`，按需解压一次后静态借用）。
//! - 按转换方向拆分为 5 张独立 `FxHash` 子表，配合首字符长度位掩码索引（`[u16; 4096]`）与
//!   UTF-8 字节偏移零拷贝切片查找，消除逐字符 `Vec<char>` 与内层循环 `String` 堆分配。
//!
//! 词典版权归 OpenCC（Apache-2.0），随包在 THIRD_PARTY_NOTICES.txt 里注明。

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Read;
use std::sync::OnceLock;

static BLOB: &[u8] = include_bytes!("../assets/zhconv.bin");

const FIRST_CHAR_SLOTS: usize = 4096;

#[derive(Default)]
struct FxByteHasher(u64);

impl Hasher for FxByteHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        let mut h = self.0;
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let word = u64::from_le_bytes(chunk.try_into().unwrap());
            h = (h.rotate_left(5) ^ word).wrapping_mul(K);
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            let mut tail = 0u64;
            for (i, &b) in rem.iter().enumerate() {
                tail |= (b as u64) << (i * 8);
            }
            h = (h.rotate_left(5) ^ tail).wrapping_mul(K);
        }
        self.0 = h;
    }
}

type FastStrMap<'a> = HashMap<&'a str, &'a str, BuildHasherDefault<FxByteHasher>>;

#[inline]
fn char_slot(ch: char) -> usize {
    ((ch as u32).wrapping_mul(0x9E37_79B1) >> 20) as usize & (FIRST_CHAR_SLOTS - 1)
}

struct DirTable<'a> {
    map: FastStrMap<'a>,
    /// 槽位内记录以该首字符开头的词条长度位掩码：
    /// bit 0..=14 对应词长 1..=15，bit 15 表示存在词长 >= 16。
    len_mask: Box<[u16; FIRST_CHAR_SLOTS]>,
    max_len: usize,
}

impl<'a> DirTable<'a> {
    fn with_capacity(cap: usize) -> Self {
        Self {
            map: HashMap::with_capacity_and_hasher(cap, BuildHasherDefault::default()),
            len_mask: Box::new([0u16; FIRST_CHAR_SLOTS]),
            max_len: 0,
        }
    }

    #[inline]
    fn insert(&mut self, key: &'a str, val: &'a str) {
        let mut chars = key.chars();
        let Some(first) = chars.next() else {
            return;
        };
        let char_len = 1 + chars.count();
        let slot = char_slot(first);
        if char_len <= 15 {
            self.len_mask[slot] |= 1u16 << (char_len - 1);
        } else {
            self.len_mask[slot] |= 1u16 << 15;
        }
        if char_len > self.max_len {
            self.max_len = char_len;
        }
        self.map.insert(key, val);
    }
}

struct Table<'a> {
    s: DirTable<'a>,
    t: DirTable<'a>,
    w: DirTable<'a>,
    v: DirTable<'a>,
    k: DirTable<'a>,
}

impl<'a> Table<'a> {
    fn parse_zhc1(raw: &'a [u8]) -> Self {
        let mut s = DirTable::with_capacity(43_000);
        let mut t = DirTable::with_capacity(4_500);
        let mut w = DirTable::with_capacity(512);
        let mut v = DirTable::with_capacity(64);
        let mut k = DirTable::with_capacity(64);

        if raw.len() > 8 && &raw[..4] == b"ZHC1" {
            let n = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
            let mut at = 8usize;
            for _ in 0..n {
                if at + 2 > raw.len() {
                    break;
                }
                let kl = u16::from_le_bytes([raw[at], raw[at + 1]]) as usize;
                at += 2;
                if at + kl + 2 > raw.len() {
                    break;
                }
                let kb = &raw[at..at + kl];
                at += kl;
                let vl = u16::from_le_bytes([raw[at], raw[at + 1]]) as usize;
                at += 2;
                if at + vl > raw.len() {
                    break;
                }
                let vb = &raw[at..at + vl];
                at += vl;

                if kl <= 1 {
                    continue;
                }
                let (Ok(full_key), Ok(val)) = (std::str::from_utf8(kb), std::str::from_utf8(vb))
                else {
                    continue;
                };
                let dir_byte = kb[0];
                let sub_key = &full_key[1..];
                match dir_byte {
                    b'S' => s.insert(sub_key, val),
                    b'T' => t.insert(sub_key, val),
                    b'W' => w.insert(sub_key, val),
                    b'V' => v.insert(sub_key, val),
                    b'K' => k.insert(sub_key, val),
                    _ => {}
                }
            }
        }

        Self { s, t, w, v, k }
    }

    #[inline]
    fn dir(&self, dir: u8) -> &DirTable<'a> {
        match dir {
            b'S' => &self.s,
            b'T' => &self.t,
            b'W' => &self.w,
            b'V' => &self.v,
            _ => &self.k,
        }
    }
}

/// 解码 `ZHC1`（未压缩直借静态切片）或 `ZHC2`（`b"ZHC2" + u32 原始字节数 + zlib 流`）。
fn decode_static_blob(blob: &'static [u8]) -> &'static [u8] {
    if blob.len() > 8 && &blob[..4] == b"ZHC2" {
        let expected_len = u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]) as usize;
        let mut out = Vec::with_capacity(expected_len);
        if flate2::read::ZlibDecoder::new(&blob[8..])
            .read_to_end(&mut out)
            .is_ok()
        {
            return Box::leak(out.into_boxed_slice());
        }
        return b"";
    }
    blob
}

fn table() -> &'static Table<'static> {
    static T: OnceLock<Table<'static>> = OnceLock::new();
    T.get_or_init(|| Table::parse_zhc1(decode_static_blob(BLOB)))
}

fn convert_with_dir_table(dt: &DirTable<'_>, text: &str) -> String {
    if text.is_empty() || text.is_ascii() || dt.map.is_empty() {
        return text.to_string();
    }

    // 单字替换表（如 TWVariants 'V' / HKVariants 'K'）：无需构建偏移数组，单趟扫描即可。
    if dt.max_len == 1 {
        let mut out: Option<String> = None;
        for (byte_idx, ch) in text.char_indices() {
            let len_utf8 = ch.len_utf8();
            if (dt.len_mask[char_slot(ch)] & 1) != 0 {
                let slice = &text[byte_idx..byte_idx + len_utf8];
                if let Some(&mapped) = dt.map.get(slice) {
                    let buf = out.get_or_insert_with(|| {
                        let mut s = String::with_capacity(text.len() + 8);
                        s.push_str(&text[..byte_idx]);
                        s
                    });
                    buf.push_str(mapped);
                    continue;
                }
            }
            if let Some(buf) = out.as_mut() {
                buf.push(ch);
            }
        }
        return out.unwrap_or_else(|| text.to_string());
    }

    // 短单元格走栈上偏移缓存（<= 64 字符零堆分配），超长文本才分配 Vec。
    let mut stack_offs = [0usize; 65];
    let mut stack_chars = ['\0'; 64];
    let mut heap_offs: Vec<usize>;
    let mut heap_chars: Vec<char>;

    let (offs, chars): (&[usize], &[char]) = if text.len() <= 64 {
        let mut n = 0usize;
        for (b, ch) in text.char_indices() {
            stack_offs[n] = b;
            stack_chars[n] = ch;
            n += 1;
        }
        stack_offs[n] = text.len();
        (&stack_offs[..=n], &stack_chars[..n])
    } else {
        let cap = text.len() / 3 + 2;
        heap_offs = Vec::with_capacity(cap + 1);
        heap_chars = Vec::with_capacity(cap);
        for (b, ch) in text.char_indices() {
            heap_offs.push(b);
            heap_chars.push(ch);
        }
        heap_offs.push(text.len());
        (&heap_offs, &heap_chars)
    };

    let n_chars = chars.len();
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut i = 0usize;
    while i < n_chars {
        let ch = chars[i];
        let mask = dt.len_mask[char_slot(ch)];
        if mask == 0 {
            out.push(ch);
            i += 1;
            continue;
        }

        let avail = n_chars - i;
        let start_b = offs[i];
        let mut matched = false;

        // 若存在 >= 16 字的超长词条，先从 max_len 向下试探到 16
        if (mask & (1u16 << 15)) != 0 && avail >= 16 {
            let hi = dt.max_len.min(avail);
            for len in (16..=hi).rev() {
                if let Some(&v) = dt.map.get(&text[start_b..offs[i + len]]) {
                    out.push_str(v);
                    i += len;
                    matched = true;
                    break;
                }
            }
        }

        if !matched {
            let mut bits = (mask & 0x7FFF) as u32;
            if avail < 15 {
                bits &= (1u32 << avail) - 1;
            }
            while bits != 0 {
                let len = 32 - bits.leading_zeros() as usize;
                bits &= !(1u32 << (len - 1));
                if let Some(&v) = dt.map.get(&text[start_b..offs[i + len]]) {
                    out.push_str(v);
                    i += len;
                    matched = true;
                    break;
                }
            }
        }

        if !matched {
            out.push(ch);
            i += 1;
        }
    }
    out
}

#[inline]
fn convert(text: &str, dir: u8) -> String {
    if text.is_empty() || text.is_ascii() {
        return text.to_string();
    }
    convert_with_dir_table(table().dir(dir), text)
}

/// 简体 → 繁体
pub fn to_traditional(text: &str) -> String {
    convert(text, b'S')
}

/// 繁体 → 简体
pub fn to_simplified(text: &str) -> String {
    convert(text, b'T')
}

/// 繁体的目标地区
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// 通用繁体
    Generic,
    /// 台湾正体：先通用繁体，再换台湾用词（软件→軟體），最后台湾字形
    Taiwan,
    /// 香港繁体：通用繁体 + 香港字形
    HongKong,
}

/// 简体 → 指定变体的繁体
pub fn to_traditional_with(text: &str, v: Variant) -> String {
    if text.is_empty() || text.is_ascii() {
        return text.to_string();
    }
    let base = to_traditional(text);
    match v {
        Variant::Generic => base,
        Variant::Taiwan => convert(&convert(&base, b'W'), b'V'),
        Variant::HongKong => convert(&base, b'K'),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_words() {
        assert_eq!(to_traditional("汉字"), "漢字");
        assert_eq!(to_simplified("漢字"), "汉字");
        // 词组优先：头发 → 頭髮（不是 頭发）
        assert_eq!(to_traditional("头发"), "頭髮");
        assert_eq!(to_simplified("頭髮"), "头发");
    }

    #[test]
    fn mixed_text_keeps_ascii() {
        assert_eq!(to_traditional("A1: 中国 2024"), "A1: 中國 2024");
        assert_eq!(to_simplified("A1: 中國 2024"), "A1: 中国 2024");
    }

    #[test]
    fn variants_taiwan_and_hongkong() {
        // 台湾用词：U盘 → 隨身碟（词表），字形走 TWVariants
        assert_eq!(
            to_traditional_with("U盘", Variant::Taiwan),
            "隨身碟",
            "台湾正体应换成当地用词"
        );
        assert_eq!(to_traditional_with("U盘", Variant::Generic), "U盤");
        // 香港字形：HKVariants 把「兌」归到「兑」
        assert_eq!(to_traditional_with("兑换", Variant::Generic), "兌換");
        assert_eq!(to_traditional_with("兑换", Variant::HongKong), "兑換");
    }

    #[test]
    fn unchanged_passes_through() {
        assert_eq!(to_traditional("hello world"), "hello world");
        assert_eq!(to_simplified("12345"), "12345");
    }

    #[test]
    fn long_paragraph_and_zhc2_compressed_blob_equivalence() {
        use std::io::Write;
        // 1) 超出 64 字节栈快路径的长文本转换验证
        let long_cn = "企业的软件开发与数据库系统需要高可靠性的内存管理与网络传输协议；".repeat(6);
        let trad_tw = to_traditional_with(&long_cn, Variant::Taiwan);
        assert!(trad_tw.contains("軟體"));
        assert!(trad_tw.contains("記憶體") || trad_tw.contains("內存"));
        let back_cn = to_simplified(&to_traditional(&long_cn));
        assert!(back_cn.contains("企业"));

        // 2) 验证 ZHC2（zlib 压缩词典流）按需解压与 ZHC1 全量词条 100% 等价
        let mut zhc2 = Vec::with_capacity(BLOB.len() / 2);
        zhc2.extend_from_slice(b"ZHC2");
        zhc2.extend_from_slice(&(BLOB.len() as u32).to_le_bytes());
        {
            let mut enc = flate2::write::ZlibEncoder::new(&mut zhc2, flate2::Compression::fast());
            enc.write_all(BLOB).unwrap();
            enc.finish().unwrap();
        }
        let leaked_zhc2: &'static [u8] = Box::leak(zhc2.into_boxed_slice());
        let decoded = decode_static_blob(leaked_zhc2);
        assert_eq!(decoded, BLOB);
        let t2 = Table::parse_zhc1(decoded);
        assert_eq!(t2.s.map.len(), table().s.map.len());
        assert_eq!(t2.t.map.len(), table().t.map.len());
        assert_eq!(t2.w.map.len(), table().w.map.len());
        assert_eq!(t2.v.map.len(), table().v.map.len());
        assert_eq!(t2.k.map.len(), table().k.map.len());
        assert_eq!(
            convert_with_dir_table(&t2.s, "头发和汉字"),
            "頭髮和漢字"
        );
    }
}
