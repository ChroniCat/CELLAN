pub mod formula;
pub mod cf;
pub mod chart_write;
pub mod charts;
pub mod cfb;
pub mod vba_src;
pub mod arr;
pub mod backup;
pub mod pic;
pub mod sparse;
pub mod table;
pub mod data_tools;
pub mod external;
pub mod csv;
pub mod fixups;
pub mod dv;
pub mod layout;
pub mod merges;
pub mod notes;
pub mod page;
pub mod patch;
pub mod pdf;
pub mod print;
pub mod pivot;
pub mod refs;
pub mod pivot_xml;
pub mod qtable;
pub mod model;
pub mod style;
pub mod sort_filter;
pub mod store;
pub mod structural;
pub mod undo;
pub mod vba;
pub mod wire;
pub mod sync;
pub mod xlsx;

pub use model::*;

/// OOXML 把新函数写成 _xlfn.XLOOKUP、_xlfn._xlws.FILTER，LAMBDA 参数写成 _xlpm.x。
/// 我们的函数表里没有这些前缀，统一剥掉。

/// 整列/整行引用在 OOXML 里写作 `Sheet!A:C`，解析器要求带行号。
/// 加载时直接改写成 `Sheet!$A$1:$C$1048576`，省得词法再分一套规则。
fn expand_whole_cols(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 24);
    let mut i = 0usize;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            in_str = !in_str;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '!' && !in_str {
            out.push('!');
            i += 1;
            let start = i;
            // [$]字母{1,3} : [$]字母{1,3}
            let mut j = i;
            let mut ok = true;
            // 第一段：[$]字母{1,3}
            if j < chars.len() && chars[j] == '$' {
                j += 1;
            }
            let ls = j;
            while j < chars.len() && chars[j].is_ascii_alphabetic() && j - ls < 3 {
                j += 1;
            }
            if j == ls || j >= chars.len() || chars[j] != ':' {
                ok = false;
            } else {
                j += 1;
                // 第二段：[$]字母{1,3}
                if j < chars.len() && chars[j] == '$' {
                    j += 1;
                }
                let ls2 = j;
                while j < chars.len() && chars[j].is_ascii_alphabetic() && j - ls2 < 3 {
                    j += 1;
                }
                if j == ls2 {
                    ok = false;
                }
            }
            let tail_ok = j >= chars.len() || !(chars[j].is_ascii_alphanumeric() || chars[j] == '$' || chars[j] == '(');
            if ok && tail_ok && j > start {
                let raw: String = chars[start..j].iter().collect();
                let raw = raw.trim_end_matches(':');
                if let Some((a, b)) = raw.split_once(':') {
                    let a = a.trim_start_matches('$');
                    let b = b.trim_start_matches('$');
                    if a.chars().all(|x| x.is_ascii_alphabetic())
                        && b.chars().all(|x| x.is_ascii_alphabetic())
                    {
                        out.push_str(&format!("${a}$1:${b}$1048576"));
                        i = j;
                        continue;
                    }
                }
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

pub fn normalize_formula(text: &str) -> String {
    if !text.contains("_xl") && !text.contains('!') && !text.contains(':') {
        return text.to_string();
    }
    let text = expand_whole_cols(text);
    text.replace("_xlfn._xlws.", "")
        .replace("_xlfn.", "")
        .replace("_xlws.", "")
        .replace("_xlpm.", "")
        .replace("_xludf.", "")
}
