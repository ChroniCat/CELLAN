pub mod formula;
pub mod errors;
pub mod scenarios;
pub mod zhconv;
pub mod proof;
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
pub mod protection;
pub mod cells;
pub mod search;
pub mod crypto;
pub mod hyperlinks;
pub mod sparkline;
pub mod textbox;
pub mod forecast;
pub mod subtotal;
pub mod flashfill;
pub mod consolidate;
pub mod a11y;
pub mod richtext;
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
pub mod xls_style;
pub mod ole_write;
pub mod xls_formula;
pub mod xls_read;
pub mod xls_write;
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

/// 把公式按引号切成 `(是否在引号内, 片段)`。
///
/// Excel 的引号规则：`"` 包住字符串，串内用 `""` 表示一个引号；
/// `'` 包住工作表名/外部工作簿名，串内用 `''` 表示一个引号。
/// 所有「改写公式」的步骤都必须只动引号外的片段——引号里是数据，不是公式。
pub fn split_quoted(text: &str) -> Vec<(bool, &str)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out: Vec<(bool, &str)> = Vec::new();
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    let mut k = 0usize;
    while k < chars.len() {
        let (i, c) = chars[k];
        match quote {
            None if c == '"' || c == '\'' => {
                if i > start {
                    out.push((false, &text[start..i]));
                }
                quote = Some(c);
                start = i;
            }
            Some(q) if c == q => {
                let escaped = chars.get(k + 1).map(|(_, n)| *n == q).unwrap_or(false);
                if escaped {
                    k += 1; // `""` / `''`：成对跳过，仍在串内
                } else {
                    let end = i + c.len_utf8();
                    out.push((true, &text[start..end]));
                    quote = None;
                    start = end;
                }
            }
            _ => {}
        }
        k += 1;
    }
    if start < text.len() {
        out.push((quote.is_some(), &text[start..]));
    }
    out
}

/// 整列引用 `A:C`（可带表名前缀）在 OOXML 里不带行号，
/// 加载时改写成 `$A$1:$C$1048576`，求值时再按实际使用范围裁剪。
fn expand_whole_cols(text: &str) -> String {
    if !text.contains(':') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 24);
    for (in_quote, seg) in split_quoted(text) {
        if in_quote {
            out.push_str(seg);
        } else {
            out.push_str(&expand_whole_cols_seg(seg));
        }
    }
    out
}

/// 只处理一段**引号外**的文本。
fn expand_whole_cols_seg(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 24);
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        {
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

/// 整行引用 `1:3` / `$1:$3` 的匹配。返回 (结束位置, 起始行, 结束行, 是否绝对行)。
fn match_rows(chars: &[char], i: usize) -> Option<(usize, u32, u32, bool, bool)> {
    let mut j = i;
    let a_abs = j < chars.len() && chars[j] == '$';
    if a_abs {
        j += 1;
    }
    let start = j;
    while j < chars.len() && chars[j].is_ascii_digit() && j - start < 7 {
        j += 1;
    }
    if j == start || j >= chars.len() || chars[j] != ':' {
        return None;
    }
    let first: u32 = chars[start..j].iter().collect::<String>().parse().ok()?;
    j += 1;
    let b_abs = j < chars.len() && chars[j] == '$';
    if b_abs {
        j += 1;
    }
    let start2 = j;
    while j < chars.len() && chars[j].is_ascii_digit() && j - start2 < 7 {
        j += 1;
    }
    if j == start2 {
        return None;
    }
    let second: u32 = chars[start2..j].iter().collect::<String>().parse().ok()?;
    // `1:3` 后面不能紧跟字母/数字/`$`/`(`，否则不是完整的整行引用。
    if j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '$' || chars[j] == '(') {
        return None;
    }
    if first == 0 || second == 0 {
        return None;
    }
    Some((j, first, second, a_abs, b_abs))
}

/// 整行引用在 OOXML/公式里是 `1:3`；解析器只认 A1 区域，因此先展开成
/// `A1:XFD3`（或绝对行 `A$1:XFD$3`），求值时再按实际使用范围裁剪。
pub(crate) fn expand_whole_rows(text: &str) -> String {
    if !text.contains(':') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 24);
    for (in_quote, seg) in split_quoted(text) {
        if in_quote {
            out.push_str(seg);
        } else {
            out.push_str(&expand_whole_rows_seg(seg));
        }
    }
    out
}

fn expand_whole_rows_seg(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 24);
    let mut i = 0usize;
    let mut bracket_depth = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '[' {
            bracket_depth += 1;
        } else if c == ']' {
            bracket_depth = bracket_depth.saturating_sub(1);
        }
        let boundary = bracket_depth == 0
            && (i == 0
                || !(chars[i - 1].is_ascii_alphanumeric()
                    || chars[i - 1] == '$'
                    || chars[i - 1] == '.'
                    || chars[i - 1] == '_'));
        if boundary {
            if let Some((end, a, b, a_abs, b_abs)) = match_rows(&chars, i) {
                let da = if a_abs { "$" } else { "" };
                let db = if b_abs { "$" } else { "" };
                out.push_str(&format!("A{da}{a}:XFD{db}{b}"));
                i = end;
                continue;
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
    // 整行引用必须保留原文（1:3），否则加载后复制/插列会把 A1:XFD3 当作
    // 普通全宽区域平移，#REF 或漏掉 A 列。求值/依赖解析时再临时展开。
    let text = expand_whole_cols(text);
    let mut out = String::with_capacity(text.len());
    for (in_quote, seg) in split_quoted(&text) {
        if in_quote {
            out.push_str(seg);
        } else {
            out.push_str(&strip_fn_prefixes(seg));
        }
    }
    out
}

/// 剥掉 `_xlfn.` / `_xlfn._xlws.` / `_xludf.` 这类**函数调用前缀**。
///
/// 不能对整段做 `replace`：`Tax_xlfn.Rate` 是合法名称、`Sales_xlfn.Backup` 是合法表名，
/// 只要前缀前面还粘着别的字符，就不是前缀。判据三条：
/// 1. 必须从标识符起点开始（前一个字符不是字母/数字/`_`/`.`）；
/// 2. 前缀后面必须是合法标识符；
/// 3. 函数前缀（`_xlfn.`/`_xlws.`/`_xludf.`）后面的标识符必须紧跟 `(` ——
///    这才说明它是一个被调用的函数，而不是同名名称。
/// `_xlpm.` 是 LET/LAMBDA 的参数名，它不跟括号，单独处理。
fn strip_fn_prefixes(seg: &str) -> String {
    const FN_PREFIXES: [&str; 3] = ["_xlfn.", "_xlws.", "_xludf."];
    let chars: Vec<char> = seg.chars().collect();
    let mut out = String::with_capacity(seg.len());
    let mut i = 0usize;
    // 结构化引用（`Sales[_xlpm.Amount]`）里的方括号内容是**列名**，不是公式 token
    let mut bracket_depth = 0usize;
    let id_start = |c: char| c.is_ascii_alphabetic() || c == '_' || c > '\u{7f}';
    let id_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c > '\u{7f}';
    while i < chars.len() {
        if chars[i] == '[' {
            bracket_depth += 1;
        } else if chars[i] == ']' {
            bracket_depth = bracket_depth.saturating_sub(1);
        }
        let boundary = bracket_depth == 0
            && (i == 0
                || !(chars[i - 1].is_ascii_alphanumeric()
                    || matches!(chars[i - 1], '_' | '.' | '$')));
        if boundary {
            let rest: String = chars[i..].iter().take(16).collect();
            // LET/LAMBDA 的参数：_xlpm.name → name
            if rest.starts_with("_xlpm.") {
                let mut j = i + "_xlpm.".len();
                let name_start = j;
                while j < chars.len() && id_char(chars[j]) {
                    j += 1;
                }
                if j > name_start && id_start(chars[name_start]) {
                    out.extend(chars[name_start..j].iter());
                    i = j;
                    continue;
                }
            }
            // 函数前缀：可以连续出现（_xlfn._xlws.FILTER）
            let mut j = i;
            let mut matched = false;
            loop {
                let ahead: String = chars[j..].iter().take(8).collect();
                match FN_PREFIXES.iter().find(|p| ahead.starts_with(**p)) {
                    Some(p) => {
                        j += p.len();
                        matched = true;
                    }
                    None => break,
                }
            }
            if matched {
                let name_start = j;
                while j < chars.len() && id_char(chars[j]) {
                    j += 1;
                }
                let mut k = j;
                while k < chars.len() && chars[k].is_whitespace() {
                    k += 1;
                }
                let is_call = k < chars.len() && chars[k] == '(';
                if j > name_start && id_start(chars[name_start]) && is_call {
                    out.extend(chars[name_start..j].iter());
                    i = j;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}
