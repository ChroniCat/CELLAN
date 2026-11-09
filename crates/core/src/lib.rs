pub mod formula;
pub mod cf;
pub mod chart_write;
pub mod charts;
pub mod cfb;
pub mod vba_src;
pub mod arr;
pub mod backup;
pub mod pic;
pub mod pic_write;
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
pub mod sheet_ops;
pub mod pivot_xml;
pub mod qtable;
pub mod model;
pub mod style;
pub mod sort_filter;
pub mod store;
pub mod structural;
pub mod undo;
pub mod vba;
pub mod clipboard_xmlss;
pub mod revision;
pub mod xlinks;
pub mod xls_layout;
pub mod wire;
pub mod sync;
pub mod xlsx;

pub use model::*;

/// OOXML 把新函数写成 _xlfn.XLOOKUP、_xlfn._xlws.FILTER，LAMBDA 参数写成 _xlpm.x。
/// 我们的函数表里没有这些前缀，统一剥掉。

/// 整列/整行引用在 OOXML 里写作 `Sheet!A:C`，解析器要求带行号。
/// 加载时直接改写成 `Sheet!$A$1:$C$1048576`，省得词法再分一套规则。
fn match_cols(chars: &[char], i: usize) -> Option<(usize, String, String)> {
    let mut j = i;
    if j < chars.len() && chars[j] == '$' {
        j += 1;
    }
    let ls = j;
    while j < chars.len() && chars[j].is_ascii_alphabetic() && j - ls < 3 {
        j += 1;
    }
    if j == ls || j >= chars.len() || chars[j] != ':' {
        return None;
    }
    let first: String = chars[ls..j].iter().collect();
    j += 1;
    if j < chars.len() && chars[j] == '$' {
        j += 1;
    }
    let ls2 = j;
    while j < chars.len() && chars[j].is_ascii_alphabetic() && j - ls2 < 3 {
        j += 1;
    }
    if j == ls2 {
        return None;
    }
    let second: String = chars[ls2..j].iter().collect();
    if j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '$' || chars[j] == '(') {
        return None;
    }
    Some((j, first, second))
}

/// 整列引用 `A:C`（可带表名前缀）在 OOXML 里不带行号，
/// 加载时改写成 `$A$1:$C$1048576`，求值时再按实际使用范围裁剪。
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
        if !in_str {
            let boundary = i == 0
                || !(chars[i - 1].is_ascii_alphanumeric()
                    || chars[i - 1] == '$'
                    || chars[i - 1] == '.'
                    || chars[i - 1] == '_');
            if boundary {
                if let Some((end, a, b)) = match_cols(&chars, i) {
                    out.push_str(&format!("${a}$1:${b}$1048576"));
                    i = end;
                    continue;
                }
            }
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
