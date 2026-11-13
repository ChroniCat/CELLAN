//! 中文本地化排序键：交给系统的 zh-CN 排序规则（拼音序），而不是按 Unicode 码位。
//!
//! Excel 在中文 Windows 上默认按拼音排姓名；我们自己按字典序排会和它不一致。
//! 这里用 `LCMapStringEx(LOCALE_NAME / LCMAP_SORTKEY)` 生成可逐字节比较的排序键，
//! 由 Windows 的语言排序表决定顺序。非 Windows 平台退化为码位序。

/// 生成排序键；相同前缀的字符串，键的字节序与系统排序一致
pub fn sort_key(s: &str) -> Vec<u8> {
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
    let wide: Vec<u16> = s.encode_utf16().collect();
    let locale: Vec<u16> = "zh-CN\0".encode_utf16().collect();
    unsafe {
        let need = LCMapStringEx(
            locale.as_ptr(),
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
        let got = LCMapStringEx(
            locale.as_ptr(),
            LCMAP_SORTKEY,
            wide.as_ptr(),
            wide.len() as i32,
            buf.as_mut_ptr() as *mut u16,
            need,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if got <= 0 {
            return None;
        }
        buf.truncate(got as usize);
        Some(buf)
    }
}

/// 中文按拼音序比较
pub fn cmp_pinyin(a: &str, b: &str) -> std::cmp::Ordering {
    sort_key(a).cmp(&sort_key(b))
}

#[cfg(test)]
mod tests {
    use super::*;

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
