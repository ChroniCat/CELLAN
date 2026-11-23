//! 中文本地化排序键：交给系统的 zh-CN 排序规则（拼音序），而不是按 Unicode 码位。
//!
//! Excel 在中文 Windows 上默认按拼音排姓名；我们自己按字典序排会和它不一致。
//! 这里用 `LCMapStringEx(LOCALE_NAME / LCMAP_SORTKEY)` 生成可逐字节比较的排序键，
//! 由 Windows 的语言排序表决定顺序。非 Windows 平台退化为码位序。

use std::collections::HashMap;

/// Per-pass deduplicating cache for `zh-CN` sort keys so repeated text values in a
/// column invoke `LCMapStringEx` only once per distinct string.
#[derive(Default)]
pub struct SortKeyCache {
    map: HashMap<String, Box<[u8]>>,
}

impl SortKeyCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            map: HashMap::with_capacity(cap),
        }
    }

    #[inline]
    pub fn get_or_compute(&mut self, s: &str) -> &[u8] {
        if !self.map.contains_key(s) {
            let k = sort_key(s).into_boxed_slice();
            self.map.insert(s.to_owned(), k);
        }
        &self.map[s]
    }
}

/// 生成排序键；相同前缀的字符串，键的字节序与系统排序一致
pub fn sort_key(s: &str) -> Vec<u8> {
    if s.is_empty() {
        return Vec::new();
    }
    #[cfg(windows)]
    {
        if let Some(k) = win_sort_key(s) {
            return k;
        }
    }
    s.as_bytes().to_vec()
}

#[cfg(windows)]
fn win_sort_key(s: &str) -> Option<Vec<u8>> {
    use windows_sys::Win32::Globalization::{LCMapStringEx, LCMAP_SORTKEY};
    const ZH_CN_LOCALE: [u16; 6] = [
        b'z' as u16,
        b'h' as u16,
        b'-' as u16,
        b'C' as u16,
        b'N' as u16,
        0,
    ];

    let mut stack_wide = [0u16; 64];
    let heap_wide: Vec<u16>;
    let wide: &[u16] = if s.len() <= 64 {
        let mut len = 0usize;
        for u in s.encode_utf16() {
            stack_wide[len] = u;
            len += 1;
        }
        &stack_wide[..len]
    } else {
        heap_wide = s.encode_utf16().collect();
        &heap_wide
    };
    if wide.is_empty() {
        return Some(Vec::new());
    }

    unsafe {
        // Fast single-syscall path using a 256-byte stack buffer (handles virtually all cell strings).
        let mut stack_out = [0u8; 256];
        let got = LCMapStringEx(
            ZH_CN_LOCALE.as_ptr(),
            LCMAP_SORTKEY,
            wide.as_ptr(),
            wide.len() as i32,
            stack_out.as_mut_ptr() as *mut u16,
            stack_out.len() as i32,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if got > 0 && (got as usize) <= stack_out.len() {
            return Some(stack_out[..got as usize].to_vec());
        }

        // Fallback for very long strings exceeding the 256-byte stack buffer.
        let need = LCMapStringEx(
            ZH_CN_LOCALE.as_ptr(),
            LCMAP_SORTKEY,
            wide.as_ptr(),
            wide.len() as i32,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if need <= 0 {
            return None;
        }
        let mut buf = vec![0u8; need as usize];
        let got2 = LCMapStringEx(
            ZH_CN_LOCALE.as_ptr(),
            LCMAP_SORTKEY,
            wide.as_ptr(),
            wide.len() as i32,
            buf.as_mut_ptr() as *mut u16,
            need,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if got2 <= 0 {
            return None;
        }
        buf.truncate(got2 as usize);
        Some(buf)
    }
}

/// 中文按拼音序比较
pub fn cmp_pinyin(a: &str, b: &str) -> std::cmp::Ordering {
    if a == b {
        return std::cmp::Ordering::Equal;
    }
    sort_key(a).cmp(&sort_key(b))
}

/// Excel「笔画排序」的表：`assets/stroke_order.bin` 是按 Excel 的
/// `SortMethod=xlStroke` 排出来的 27 584 个汉字（BMP，CJK 基本区 + 扩展 A）的顺序，
/// 每字 2 字节小端码位；数组下标即 Excel 笔画序里的次序。
static STROKE_ORDER: &[u8] = include_bytes!("../../../assets/stroke_order.bin");

fn stroke_index() -> &'static Vec<(u16, u16)> {
    static IDX: std::sync::OnceLock<Vec<(u16, u16)>> = std::sync::OnceLock::new();
    IDX.get_or_init(|| {
        let mut v: Vec<(u16, u16)> = STROKE_ORDER
            .chunks_exact(2)
            .enumerate()
            .map(|(i, c)| (u16::from_le_bytes([c[0], c[1]]), i as u16))
            .collect();
        v.sort_unstable();
        v
    })
}

/// 单字在 Excel 笔画序里的名次；表外字符（非 BMP 汉字、假名、字母等）返回 None
pub fn stroke_rank(ch: char) -> Option<u16> {
    let c = ch as u32;
    if c > 0xFFFF {
        return None;
    }
    let key = c as u16;
    stroke_index()
        .binary_search_by_key(&key, |e| e.0)
        .ok()
        .map(|i| stroke_index()[i].1)
}

/// 中文按笔画序比较（与 Excel 的 xlStroke 同表）。
/// 表外字符无法定位名次，整串退回拼音序，保证全序且可复现。
pub fn cmp_stroke(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (x, y) in a.chars().zip(b.chars()) {
        match (stroke_rank(x), stroke_rank(y)) {
            (Some(rx), Some(ry)) => {
                if rx != ry {
                    return rx.cmp(&ry);
                }
            }
            _ => return cmp_pinyin(a, b),
        }
    }
    let la = a.chars().count();
    let lb = b.chars().count();
    if la != lb {
        return la.cmp(&lb);
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_rank_follows_excel_xlstroke_sample() {
        // Excel 对 28 个字做 xlStroke 排序得到的顺序（scripts 里可复现）
        let sample = "一乙二十人三大小山川王天木中水火石目田白耳虫竹自行衣羊米";
        let ranks: Vec<u16> = sample.chars().filter_map(super::stroke_rank).collect();
        assert_eq!(ranks.len(), sample.chars().count(), "样字都应在表内");
        assert!(ranks.windows(2).all(|w| w[0] < w[1]), "名次应严格递增：{ranks:?}");
        assert_eq!(super::stroke_rank('一'), Some(0), "一是 Excel 笔画序第一");
    }

    #[test]
    fn cmp_stroke_matches_excel_ties() {
        use std::cmp::Ordering::Less;
        // 同画数时 Excel 的次序：三(3) 大(3) 小(3) 山(3) 川(3)
        for w in ["三", "大", "小", "山", "川"].windows(2) {
            assert_eq!(super::cmp_stroke(w[0], w[1]), Less, "{} 应排在 {} 前", w[0], w[1]);
        }
        // 四画：王 天 木 中 水 火
        for w in ["王", "天", "木", "中", "水", "火"].windows(2) {
            assert_eq!(super::cmp_stroke(w[0], w[1]), Less, "{} 应排在 {} 前", w[0], w[1]);
        }
        assert_eq!(super::cmp_stroke("张", "张"), std::cmp::Ordering::Equal);
    }

    #[test]
    fn pinyin_order_differs_from_codepoint() {
        // 张(zhang) 应排在 李(li) 之后；码位序里 张 U+5F20 < 李 U+674E
        assert_eq!(cmp_pinyin("李", "张"), std::cmp::Ordering::Less);
        assert_eq!(cmp_pinyin("王", "赵"), std::cmp::Ordering::Less);
    }

    #[test]
    fn ascii_prefix_stays_before_chinese() {
        assert_eq!(cmp_pinyin("A", "张"), std::cmp::Ordering::Less);
    }
}
