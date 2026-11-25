pub mod font_subset;
pub mod formula;
pub mod dbnum;
pub mod mem;
pub mod errors;
pub mod scenarios;
pub mod zhconv;
pub mod proof;
pub mod cf;
pub mod chart_write;
pub mod charts;
pub mod collate;
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
pub mod display;
pub mod image_embed;
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
pub mod pivot_multi;
pub mod pivot_engine;
pub mod pivot_parse;
pub mod refs;
pub mod sheet_ops;
pub mod pivot_parts;
pub mod pivot_parts2;
pub mod pivot_xml;
pub mod qtable;
pub mod model;
pub mod input_parse;
pub mod style;
pub mod style_ext;
pub mod style_theme;
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
pub(crate) fn match_cols_bytes<'a>(text: &'a str, i: usize) -> Option<(usize, &'a str, &'a str)> {
    let bytes = text.as_bytes();
    let mut j = i;
    if bytes.get(j) == Some(&b'$') {
        j += 1;
    }
    let ls = j;
    while j < bytes.len() && bytes[j].is_ascii_alphabetic() && j - ls < 3 {
        j += 1;
    }
    if j == ls || bytes.get(j) != Some(&b':') {
        return None;
    }
    let first = &text[ls..j];
    j += 1;
    if bytes.get(j) == Some(&b'$') {
        j += 1;
    }
    let ls2 = j;
    while j < bytes.len() && bytes[j].is_ascii_alphabetic() && j - ls2 < 3 {
        j += 1;
    }
    if j == ls2 {
        return None;
    }
    let second = &text[ls2..j];
    if let Some(&next) = bytes.get(j)
        && (next.is_ascii_alphanumeric() || matches!(next, b'$' | b'(' | b'!'))
    {
        return None;
    }
    Some((j, first, second))
}

/// Excel 2010 之后新增、在 OOXML 文件里必须写成 `_xlfn.名字` 的函数。
/// 加载时这些前缀被剥掉（见 `normalize_formula`），保存时必须加回去：
/// 不带前缀的 `XLOOKUP(...)` 在 Excel 里打开就是 `#NAME?`。
/// 名单取自微软「_xlfn 前缀」文档列出的函数，按字母序，便于二分查找。
const XLFN_FUNCTIONS: &[&str] = &[
    "ACOT", "ACOTH", "AGGREGATE", "ANCHORARRAY", "ARABIC", "ARRAYTOTEXT", "BASE", "BETA.DIST",
    "BETA.INV", "BINOM.DIST", "BINOM.DIST.RANGE", "BINOM.INV", "BITAND", "BITLSHIFT", "BITOR",
    "BITRSHIFT", "BITXOR", "BYCOL", "BYROW", "CEILING.MATH", "CEILING.PRECISE", "CHISQ.DIST",
    "CHISQ.DIST.RT", "CHISQ.INV", "CHISQ.INV.RT", "CHISQ.TEST", "CHOOSECOLS", "CHOOSEROWS",
    "COMBINA", "CONCAT", "CONFIDENCE.NORM", "CONFIDENCE.T", "COT", "COTH", "COVARIANCE.P",
    "COVARIANCE.S", "CSC", "CSCH", "DAYS", "DECIMAL", "DROP", "ENCODEURL", "ERF.PRECISE",
    "ERFC.PRECISE", "EXPAND", "EXPON.DIST", "F.DIST", "F.DIST.RT", "F.INV", "F.INV.RT", "F.TEST",
    "FIELDVALUE", "FILTER", "FILTERXML", "FLOOR.MATH", "FLOOR.PRECISE", "FORECAST.ETS",
    "FORECAST.ETS.CONFINT", "FORECAST.ETS.SEASONALITY", "FORECAST.ETS.STAT", "FORECAST.LINEAR",
    "FORMULATEXT", "GAMMA", "GAMMA.DIST", "GAMMA.INV", "GAMMALN.PRECISE", "GAUSS", "GROUPBY",
    "HSTACK", "HYPGEOM.DIST", "IFNA", "IFS", "IMAGE", "IMCOSH", "IMCOT", "IMCSC", "IMCSCH",
    "IMSEC", "IMSECH", "IMSINH", "IMTAN", "ISFORMULA", "ISOMITTED", "ISOWEEKNUM", "LAMBDA", "LET",
    "LOGNORM.DIST", "LOGNORM.INV", "MAKEARRAY", "MAP", "MAXIFS", "MINIFS", "MODE.MULT",
    "MODE.SNGL", "MUNIT", "NEGBINOM.DIST", "NETWORKDAYS.INTL", "NORM.DIST", "NORM.INV",
    "NORM.S.DIST", "NORM.S.INV", "NUMBERVALUE", "PDURATION", "PERCENTILE.EXC", "PERCENTILE.INC",
    "PERCENTOF", "PERCENTRANK.EXC", "PERCENTRANK.INC", "PERMUTATIONA", "PHI", "PIVOTBY",
    "POISSON.DIST", "QUARTILE.EXC", "QUARTILE.INC", "QUERYSTRING", "RANDARRAY", "RANK.AVG",
    "RANK.EQ", "REDUCE", "REGEXEXTRACT", "REGEXREPLACE", "REGEXTEST", "RRI", "SCAN", "SEC", "SECH",
    "SEQUENCE", "SHEET", "SHEETS", "SINGLE", "SKEW.P", "SORT", "SORTBY", "STDEV.P", "STDEV.S",
    "STOCKHISTORY", "SWITCH", "T.DIST", "T.DIST.2T", "T.DIST.RT", "T.INV", "T.INV.2T", "T.TEST",
    "TAKE", "TEXTAFTER", "TEXTBEFORE", "TEXTJOIN", "TEXTSPLIT", "TOCOL", "TOROW", "TRIMRANGE",
    "UNICHAR", "UNICODE", "UNIQUE", "VALUETOTEXT", "VAR.P", "VAR.S", "VSTACK", "WEBSERVICE",
    "WEIBULL.DIST", "WORKDAY.INTL", "WRAPCOLS", "WRAPROWS", "XLOOKUP", "XMATCH", "XOR", "Z.TEST",
];

/// 这两个函数在文件里是 `_xlfn._xlws.名字`
const XLWS_FUNCTIONS: &[&str] = &["FILTER", "SORT"];

fn formula_id_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c > '\u{7f}'
}

fn formula_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c > '\u{7f}'
}

/// 引号外、方括号外的字符位置（`[`…`]` 是结构化引用里的列名）
fn formula_code_mask(chars: &[char]) -> Vec<bool> {
    let mut mask = vec![false; chars.len()];
    let mut quote: Option<char> = None;
    let mut bracket = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                if c == q {
                    if chars.get(i + 1) == Some(&q) {
                        i += 2;
                        continue;
                    }
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '[' => bracket += 1,
            None if c == ']' => bracket = bracket.saturating_sub(1),
            None => mask[i] = bracket == 0,
        }
        i += 1;
    }
    mask
}

/// 从 `open`（左括号）开始，按顶层逗号切出各参数的 [起, 止) 位置
fn formula_call_args(chars: &[char], mask: &[bool], open: usize) -> Vec<(usize, usize)> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = open + 1;
    let mut i = open;
    while i < chars.len() {
        if mask[i] {
            match chars[i] {
                '(' | '{' => depth += 1,
                ')' | '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        args.push((start, i));
                        return args;
                    }
                }
                ',' if depth == 1 => {
                    args.push((start, i));
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    args.push((start, chars.len()));
    args
}

/// 保存前把公式写回 OOXML 的函数名形式（`normalize_formula` 剥前缀的逆操作）：
/// 新函数加 `_xlfn.`（FILTER/SORT 是 `_xlfn._xlws.`），LET/LAMBDA 的参数名加 `_xlpm.`。
/// 只改引号与结构化引用方括号之外的标识符；已带前缀的不再重复加。传入的公式不带开头的 `=`。
pub fn add_fn_prefixes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mask = formula_code_mask(&chars);
    let at_boundary = |i: usize| {
        i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || matches!(chars[i - 1], '_' | '.' | '$'))
    };
    // 标识符 [i, j) 及其后（跳过空白）是否紧跟左括号
    let ident_end = |i: usize| {
        let mut j = i;
        while j < chars.len() && mask[j] && formula_id_char(chars[j]) {
            j += 1;
        }
        j
    };
    let next_non_space = |mut k: usize| {
        while k < chars.len() && chars[k].is_whitespace() {
            k += 1;
        }
        k
    };

    // 1. 收集 LET / LAMBDA 的参数名
    let mut params: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if mask[i] && formula_id_start(chars[i]) && at_boundary(i) {
            let j = ident_end(i);
            let name: String = chars[i..j].iter().collect::<String>().to_ascii_uppercase();
            let k = next_non_space(j);
            if k < chars.len() && chars[k] == '(' && (name == "LET" || name == "LAMBDA") {
                let args = formula_call_args(&chars, &mask, k);
                let n = args.len();
                for (idx, (s, e)) in args.iter().enumerate() {
                    let is_param = if name == "LET" { idx % 2 == 0 && idx + 1 < n } else { idx + 1 < n };
                    if !is_param {
                        continue;
                    }
                    let p: String = chars[*s..*e].iter().collect::<String>().trim().to_string();
                    let valid = p.chars().next().is_some_and(formula_id_start)
                        && p.chars().all(formula_id_char)
                        && !p.starts_with("_xlpm.");
                    if valid && !params.iter().any(|x| x.eq_ignore_ascii_case(&p)) {
                        params.push(p);
                    }
                }
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }

    // 2. 重写
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    while i < chars.len() {
        if mask[i] && formula_id_start(chars[i]) && at_boundary(i) {
            let j = ident_end(i);
            let name: String = chars[i..j].iter().collect();
            let upper = name.to_ascii_uppercase();
            let k = next_non_space(j);
            let is_call = k < chars.len() && chars[k] == '(';
            // `Sheet!A1` 的表名、`A:C` 整列引用里的列字母都不是参数名
            let before_sheet_bang = chars.get(j) == Some(&'!');
            let in_col_range = chars.get(j) == Some(&':') || (i > 0 && chars[i - 1] == ':');
            let is_param = !before_sheet_bang
                && !in_col_range
                && params.iter().any(|p| p.eq_ignore_ascii_case(&name));
            if upper.starts_with("_XL") {
                // 已经是文件形式
            } else if is_param {
                out.push_str("_xlpm.");
            } else if is_call && XLWS_FUNCTIONS.contains(&upper.as_str()) {
                out.push_str("_xlfn._xlws.");
            } else if is_call && XLFN_FUNCTIONS.binary_search(&upper.as_str()).is_ok() {
                out.push_str("_xlfn.");
            }
            out.push_str(&name);
            i = j;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 把公式按引号切成 `(是否在引号内, 片段)`。
///
/// Excel 的引号规则：`"` 包住字符串，串内用 `""` 表示一个引号；
/// `'` 包住工作表名/外部工作簿名，串内用 `''` 表示一个引号。
/// 所有「改写公式」的步骤都必须只动引号外的片段——引号里是数据，不是公式。
pub fn split_quoted(text: &str) -> Vec<(bool, &str)> {
    let bytes = text.as_bytes();
    if !bytes.contains(&b'"') && !bytes.contains(&b'\'') {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![(false, text)]
        };
    }
    let mut out: Vec<(bool, &str)> = Vec::new();
    let mut quote: Option<u8> = None;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            None if b == b'"' || b == b'\'' => {
                if i > start {
                    out.push((false, &text[start..i]));
                }
                quote = Some(b);
                start = i;
            }
            Some(q) if b == q => {
                if bytes.get(i + 1) == Some(&q) {
                    i += 2; // `""` / `''`：成对跳过，仍在串内
                    continue;
                }
                let end = i + 1;
                out.push((true, &text[start..end]));
                quote = None;
                start = end;
            }
            _ => {}
        }
        i += 1;
    }
    if start < text.len() {
        out.push((quote.is_some(), &text[start..]));
    }
    out
}

/// 整列引用 `A:C`（可带表名前缀）在 OOXML 里不带行号，
/// 加载时改写成 `$A$1:$C$1048576`，求值时再按实际使用范围裁剪。
pub(crate) fn expand_whole_cols(text: &str) -> String {
    expand_whole_cols_cow(text).into_owned()
}

pub(crate) fn expand_whole_cols_cow(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(':') {
        return std::borrow::Cow::Borrowed(text);
    }
    let segs = split_quoted(text);
    let mut out: Option<String> = None;
    let mut copied = 0usize;
    let mut offset = 0usize;
    for (in_quote, seg) in segs {
        if !in_quote {
            let bytes = seg.as_bytes();
            let mut i = 0usize;
            while i < bytes.len() {
                let b = bytes[i];
                let boundary = i == 0
                    || !(bytes[i - 1].is_ascii_alphanumeric()
                        || matches!(bytes[i - 1], b'$' | b'.' | b'_'));
                if boundary
                    && (b == b'$' || b.is_ascii_alphabetic())
                    && let Some((end, first, second)) = match_cols_bytes(seg, i)
                {
                    let buf = out.get_or_insert_with(|| String::with_capacity(text.len() + 24));
                    buf.push_str(&text[copied..offset + i]);
                    buf.push('$');
                    buf.push_str(first);
                    buf.push_str("$1:$");
                    buf.push_str(second);
                    buf.push_str("$1048576");
                    copied = offset + end;
                    i = end;
                    continue;
                }
                i += 1;
            }
        }
        offset += seg.len();
    }
    match out {
        Some(mut buf) => {
            buf.push_str(&text[copied..]);
            std::borrow::Cow::Owned(buf)
        }
        None => std::borrow::Cow::Borrowed(text),
    }
}

/// 整行引用 `1:3` / `$1:$3` 的匹配。返回 (结束字节下标, 起始行, 结束行, 是否绝对行)。
pub(crate) fn match_rows_bytes(bytes: &[u8], i: usize) -> Option<(usize, u32, u32, bool, bool)> {
    let mut j = i;
    let a_abs = bytes.get(j) == Some(&b'$');
    if a_abs {
        j += 1;
    }
    let start = j;
    while j < bytes.len() && bytes[j].is_ascii_digit() && j - start < 7 {
        j += 1;
    }
    if j == start || bytes.get(j) != Some(&b':') {
        return None;
    }
    let first: u32 = std::str::from_utf8(&bytes[start..j]).ok()?.parse().ok()?;
    j += 1;
    let b_abs = bytes.get(j) == Some(&b'$');
    if b_abs {
        j += 1;
    }
    let start2 = j;
    while j < bytes.len() && bytes[j].is_ascii_digit() && j - start2 < 7 {
        j += 1;
    }
    if j == start2 {
        return None;
    }
    let second: u32 = std::str::from_utf8(&bytes[start2..j]).ok()?.parse().ok()?;
    // `1:3` 后面不能紧跟字母/数字/`$`/`(`/`!`，否则不是完整的整行引用（如数字表名三维区间 1:3!A1）。
    if let Some(&next) = bytes.get(j)
        && (next.is_ascii_alphanumeric() || matches!(next, b'$' | b'(' | b'!'))
    {
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
    expand_whole_rows_cow(text).into_owned()
}

pub(crate) fn expand_whole_rows_cow(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(':') {
        return std::borrow::Cow::Borrowed(text);
    }
    let segs = split_quoted(text);
    let mut out: Option<String> = None;
    let mut copied = 0usize;
    let mut offset = 0usize;
    for (in_quote, seg) in segs {
        if !in_quote {
            let bytes = seg.as_bytes();
            let mut i = 0usize;
            let mut bracket_depth = 0usize;
            while i < bytes.len() {
                let b = bytes[i];
                if b == b'[' {
                    bracket_depth += 1;
                } else if b == b']' {
                    bracket_depth = bracket_depth.saturating_sub(1);
                }
                let boundary = bracket_depth == 0
                    && (i == 0
                        || !(bytes[i - 1].is_ascii_alphanumeric()
                            || matches!(bytes[i - 1], b'$' | b'.' | b'_')));
                if boundary
                    && (b == b'$' || b.is_ascii_digit())
                    && let Some((end, a, b_row, a_abs, b_abs)) = match_rows_bytes(bytes, i)
                {
                    let buf = out.get_or_insert_with(|| String::with_capacity(text.len() + 24));
                    buf.push_str(&text[copied..offset + i]);
                    use std::fmt::Write;
                    let da = if a_abs { "$" } else { "" };
                    let db = if b_abs { "$" } else { "" };
                    let _ = write!(buf, "A{da}{a}:XFD{db}{b_row}");
                    copied = offset + end;
                    i = end;
                    continue;
                }
                i += 1;
            }
        }
        offset += seg.len();
    }
    match out {
        Some(mut buf) => {
            buf.push_str(&text[copied..]);
            std::borrow::Cow::Owned(buf)
        }
        None => std::borrow::Cow::Borrowed(text),
    }
}

pub fn normalize_formula(text: &str) -> String {
    normalize_formula_cow(text).into_owned()
}

pub(crate) fn normalize_formula_cow(text: &str) -> std::borrow::Cow<'_, str> {
    let has_xl = text.contains("_xl");
    let has_colon = text.contains(':');
    if !has_xl && !has_colon {
        return std::borrow::Cow::Borrowed(text);
    }
    // 整行引用必须保留原文（1:3），否则加载后复制/插列会把 A1:XFD3 当作
    // 普通全宽区域平移，#REF 或漏掉 A 列。求值/依赖解析时再临时展开。
    let cols_expanded = expand_whole_cols_cow(text);
    if !has_xl {
        return cols_expanded;
    }
    let src: &str = &cols_expanded;
    let mut out = String::with_capacity(src.len());
    for (in_quote, seg) in split_quoted(src) {
        if in_quote || !seg.contains("_xl") {
            out.push_str(seg);
        } else {
            out.push_str(&strip_fn_prefixes(seg));
        }
    }
    std::borrow::Cow::Owned(out)
}

/// 剥掉 `_xlfn.` / `_xlfn._xlws.` / `_xludf.` 这类**函数调用前缀**。
fn strip_fn_prefixes(seg: &str) -> String {
    const FN_PREFIXES: [&[u8]; 3] = [b"_xlfn.", b"_xlws.", b"_xludf."];
    let bytes = seg.as_bytes();
    let mut out = String::with_capacity(seg.len());
    let mut copied = 0usize;
    let mut i = 0usize;
    let mut bracket_depth = 0usize;
    let id_start = |b: u8| b.is_ascii_alphabetic() || b == b'_' || b >= 0x80;
    let id_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b >= 0x80;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'[' {
            bracket_depth += 1;
        } else if b == b']' {
            bracket_depth = bracket_depth.saturating_sub(1);
        }
        let boundary = bracket_depth == 0
            && (i == 0
                || !(bytes[i - 1].is_ascii_alphanumeric()
                    || matches!(bytes[i - 1], b'_' | b'.' | b'$')
                    || bytes[i - 1] >= 0x80));
        if boundary && b == b'_' {
            if bytes[i..].starts_with(b"_xlpm.") {
                let name_start = i + 6;
                let mut j = name_start;
                while j < bytes.len() && id_byte(bytes[j]) {
                    j += 1;
                }
                if j > name_start && id_start(bytes[name_start]) {
                    out.push_str(&seg[copied..i]);
                    out.push_str(&seg[name_start..j]);
                    copied = j;
                    i = j;
                    continue;
                }
            }
            let mut j = i;
            let mut matched = false;
            while let Some(p) = FN_PREFIXES.iter().find(|p| bytes[j..].starts_with(p)) {
                j += p.len();
                matched = true;
            }
            if matched {
                let name_start = j;
                while j < bytes.len() && id_byte(bytes[j]) {
                    j += 1;
                }
                let mut k = j;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                if j > name_start && id_start(bytes[name_start]) && bytes.get(k) == Some(&b'(') {
                    out.push_str(&seg[copied..i]);
                    out.push_str(&seg[name_start..j]);
                    copied = j;
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&seg[copied..]);
    out
}

#[cfg(test)]
mod xlfn_table_tests {
    #[test]
    fn xlfn_table_is_strictly_sorted() {
        // add_fn_prefixes 对这张表做二分查找：顺序错了会漏加前缀
        for w in super::XLFN_FUNCTIONS.windows(2) {
            assert!(w[0] < w[1], "{} 应排在 {} 之前", w[0], w[1]);
        }
    }
}
