//! 中文简繁转换：内置 OpenCC 的字/词表（`assets/zhconv.bin`，由
//! `scripts/gen_zhconv.py` 生成），做最长匹配替换。
//!
//! 词典版权归 OpenCC（Apache-2.0），随包在 THIRD_PARTY_NOTICES.txt 里注明。

use std::collections::HashMap;
use std::sync::OnceLock;

static BLOB: &[u8] = include_bytes!("../assets/zhconv.bin");

struct Table {
    /// 键带方向前缀：'S' + 简体词 / 'T' + 繁体词
    map: HashMap<String, String>,
    max_len: usize,
}

fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| {
        let mut map = HashMap::new();
        let mut max_len = 1;
        if BLOB.len() > 8 && &BLOB[..4] == b"ZHC1" {
            let n = u32::from_le_bytes([BLOB[4], BLOB[5], BLOB[6], BLOB[7]]) as usize;
            let mut at = 8;
            for _ in 0..n {
                if at + 2 > BLOB.len() {
                    break;
                }
                let kl = u16::from_le_bytes([BLOB[at], BLOB[at + 1]]) as usize;
                at += 2;
                let key = String::from_utf8_lossy(&BLOB[at..at + kl]).to_string();
                at += kl;
                if at + 2 > BLOB.len() {
                    break;
                }
                let vl = u16::from_le_bytes([BLOB[at], BLOB[at + 1]]) as usize;
                at += 2;
                let val = String::from_utf8_lossy(&BLOB[at..at + vl]).to_string();
                at += vl;
                let chars = key.chars().count();
                if chars > max_len {
                    max_len = chars;
                }
                map.insert(key, val);
            }
        }
        Table { map, max_len }
    })
}

fn convert(text: &str, dir: char) -> String {
    let t = table();
    if t.map.is_empty() || text.is_empty() {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut i = 0;
    while i < chars.len() {
        let mut hit: Option<(usize, &String)> = None;
        let max = t.max_len.min(chars.len() - i);
        for len in (1..=max).rev() {
            let mut key = String::with_capacity(len + 1);
            key.push(dir);
            key.extend(&chars[i..i + len]);
            if let Some(v) = t.map.get(&key) {
                hit = Some((len, v));
                break;
            }
        }
        match hit {
            Some((len, v)) => {
                out.push_str(v);
                i += len;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// 简体 → 繁体
pub fn to_traditional(text: &str) -> String {
    convert(text, 'S')
}

/// 繁体 → 简体
pub fn to_simplified(text: &str) -> String {
    convert(text, 'T')
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
    let base = to_traditional(text);
    match v {
        Variant::Generic => base,
        Variant::Taiwan => convert(&convert(&base, 'W'), 'V'),
        Variant::HongKong => convert(&base, 'K'),
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
}
