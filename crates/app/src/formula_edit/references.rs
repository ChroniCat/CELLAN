//! A1 reference picking, highlighting and absolute/relative editing.

use super::{byte_index_of_char, is_inside_string_literal};
use crate::{debug_flags, parse_a1_ref};
use eframe::egui;
use sheetcore::{cell_name, col_label};
use std::collections::HashMap;

/// 拖动扩区域时的引用文本：跨表带 `表名!`，同表就是 `A1:B2`
pub(crate) fn pick_range_label(sheet: Option<&str>, a: (u32, u32), b: (u32, u32)) -> String {
    // 单格不写 A1:A1（Excel 也只写 A1）
    let core = if a == b {
        cell_name(a.0, a.1)
    } else {
        range_label(a, b)
    };
    match sheet {
        Some(n) => format!("{}!{}", sheet_ref_name(n), core),
        None => core,
    }
}

/// 选区 → 「A1」或「A1:B2」（向导拾取用）
pub(crate) fn wiz_range_label(a: (u32, u32), b: (u32, u32)) -> String {
    let (r1, c1) = (a.0.min(b.0), a.1.min(b.1));
    let (r2, c2) = (a.0.max(b.0), a.1.max(b.1));
    let head = sheetcore::model::cell_name(r1, c1);
    if (r1, c1) == (r2, c2) {
        head
    } else {
        format!("{}:{}", head, sheetcore::model::cell_name(r2, c2))
    }
}

/// UI-11.3：A1 引用判定（列 1-3 个字母、行 1-7 位数字，$ 可选，行号不能是 0）。
fn is_a1_ref(s: &str) -> bool {
    let b: Vec<char> = s.chars().collect();
    if b.is_empty() {
        return false;
    }
    let mut i = 0;
    if b[i] == '$' {
        i += 1;
    }
    let cs = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    let letters = i - cs;
    if !(1..=3).contains(&letters) {
        return false;
    }
    if i < b.len() && b[i] == '$' {
        i += 1;
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    i > ds && i == b.len() && b[ds] != '0'
}

/// 引用或 `A1:B2` 范围，可带 `Sheet!` / `'My Sheet'!` 前缀。
fn looks_like_ref(tok: &str) -> bool {
    let tail = match tok.rfind('!') {
        Some(i) => &tok[i + 1..],
        None => tok,
    };
    let parts: Vec<&str> = tail.split(':').collect();
    !parts.is_empty() && parts.len() <= 2 && parts.iter().all(|p| is_a1_ref(p))
}

/// 从 `pos` 起解析一个 A1 引用（含可选 `$`），返回结束的字符下标。
fn a1_end(chars: &[char], pos: usize) -> Option<usize> {
    let mut i = pos;
    if i < chars.len() && chars[i] == '$' {
        i += 1;
    }
    let cs = i;
    while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
    }
    if !(1..=3).contains(&(i - cs)) {
        return None;
    }
    if i < chars.len() && chars[i] == '$' {
        i += 1;
    }
    let ds = i;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i == ds || chars[ds] == '0' {
        return None;
    }
    Some(i)
}

/// 从 `i` 起解析「[可选表名!]引用[:引用]」，返回结束下标与原文。
fn parse_ref_token(chars: &[char], i: usize) -> Option<(usize, String)> {
    let mut j = i;
    if i < chars.len() && chars[i] == '\'' {
        let mut k = i + 1;
        while k < chars.len() && chars[k] != '\'' {
            k += 1;
        }
        if k >= chars.len() {
            return None;
        }
        j = k + 1;
        if j >= chars.len() || chars[j] != '!' {
            return None;
        }
    } else {
        let mut k = j;
        while k < chars.len()
            && (chars[k].is_alphanumeric() || matches!(chars[k], '_' | '.'))
        {
            k += 1;
        }
        if k > i && k < chars.len() && chars[k] == '!' {
            j = k;
        }
    }
    if j != i {
        if j >= chars.len() || chars[j] != '!' {
            return None;
        }
        j += 1;
    }
    let mut end = a1_end(chars, j)?;
    if end < chars.len() && chars[end] == ':' {
        if let Some(e2) = a1_end(chars, end + 1) {
            end = e2;
        }
    }
    Some((end, chars[i..end].iter().collect()))
}

/// UI-11.3：点选单元格建立引用 —— 光标左侧若是引用/正在输入的引用就整段替换，
/// 否则在光标处插入。返回新缓冲与新光标（字符下标）。Excel 同款行为。
/// 工作表名在引用里的写法：名字含空格、标点、以数字开头等要加单引号（Excel 规则）。
pub(crate) fn sheet_ref_name(name: &str) -> String {
    let plain = !name.is_empty()
        && !name.chars().next().is_some_and(|c| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c > '\u{7f}');
    if plain {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

pub(crate) fn insert_reference(buf: &str, caret: usize, label: &str) -> (String, usize) {
    let chars: Vec<char> = buf.chars().collect();
    let caret = caret.min(chars.len());
    let mut start = caret;
    while start > 0 {
        let ch = chars[start - 1];
        if ch.is_alphanumeric() || matches!(ch, '$' | '_' | '.' | '!' | ':' | '\'') {
            start -= 1;
        } else {
            break;
        }
    }
    let left: String = chars[start..caret].iter().collect();
    let start = if looks_like_ref(&left) { start } else { caret };
    let mut out: String = chars[..start].iter().collect();
    out.push_str(label);
    out.extend(chars[caret..].iter());
    (out, start + label.chars().count())
}

/// UI-11.5：把缓冲里的 [s,e) 换成 text，返回新缓冲与新光标。
pub(crate) fn replace_char_span(buf: &str, span: (usize, usize), text: &str) -> (String, usize) {
    let chars: Vec<char> = buf.chars().collect();
    let s = span.0.min(chars.len());
    let e = span.1.max(s).min(chars.len());
    let mut out: String = chars[..s].iter().collect();
    out.push_str(text);
    out.extend(chars[e..].iter());
    (out, s + text.chars().count())
}

/// UI-11.5：两格之间的区域引用，锚点在前（从 B3 拖回 A1 显示 B3:A1，和 Excel 一致）。
pub(crate) fn range_label(a: (u32, u32), b: (u32, u32)) -> String {
    format!("{}:{}", cell_name(a.0, a.1), cell_name(b.0, b.1))
}

/// UI-11.5：整列引用 `A:A`。
pub(crate) fn whole_col_label(c: u32) -> String {
    let l = col_label(c);
    format!("{l}:{l}")
}

/// UI-11.5：整行引用 `1:1`。
pub(crate) fn whole_row_label(r: u32) -> String {
    format!("{}:{}", r + 1, r + 1)
}

/// 从字节偏移 `pos` 起解析一个 A1 引用（含可选 `$`），返回结束的字节下标。
#[inline]
fn a1_end_bytes(bytes: &[u8], pos: usize) -> Option<usize> {
    let mut i = pos;
    if i < bytes.len() && bytes[i] == b'$' {
        i += 1;
    }
    let cs = i;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if !(1..=3).contains(&(i - cs)) {
        return None;
    }
    if i < bytes.len() && bytes[i] == b'$' {
        i += 1;
    }
    let ds = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == ds || bytes[ds] == b'0' {
        return None;
    }
    Some(i)
}

/// 从字节偏移 `i` 起零分配解析「[可选表名!]引用[:引用]」，返回结束字节下标。
fn parse_ref_token_bytes(buf: &str, i: usize) -> Option<usize> {
    let bytes = buf.as_bytes();
    let mut j = i;
    if i < bytes.len() && bytes[i] == b'\'' {
        let mut k = i + 1;
        while k < bytes.len() && bytes[k] != b'\'' {
            k += 1;
        }
        if k >= bytes.len() {
            return None;
        }
        j = k + 1;
        if j >= bytes.len() || bytes[j] != b'!' {
            return None;
        }
    } else {
        let mut k = j;
        while k < buf.len() {
            let b = bytes[k];
            if b.is_ascii() {
                if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.') {
                    k += 1;
                } else {
                    break;
                }
            } else {
                let ch = buf[k..].chars().next()?;
                if ch.is_alphanumeric() {
                    k += ch.len_utf8();
                } else {
                    break;
                }
            }
        }
        if k > i && k < bytes.len() && bytes[k] == b'!' {
            j = k;
        }
    }
    if j != i {
        if j >= bytes.len() || bytes[j] != b'!' {
            return None;
        }
        j += 1;
    }
    let mut end = a1_end_bytes(bytes, j)?;
    if end < bytes.len() && bytes[end] == b':' {
        if let Some(e2) = a1_end_bytes(bytes, end + 1) {
            end = e2;
        }
    }
    Some(end)
}

#[inline]
fn is_ref_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '$' | '.' | '\'')
}

/// O(L) 字节级引用扫描（跳过 `"..."` 字符串字面量，零 `Vec<char>` 分配，`HashMap<&str, usize>` O(1) 配对）。
pub(crate) fn ref_byte_spans(buf: &str) -> Vec<(usize, usize, usize)> {
    let mut out: Vec<(usize, usize, usize)> = Vec::new();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let bytes = buf.as_bytes();
    let mut i = 0usize;
    let mut prev_ident = false;
    while i < buf.len() {
        if bytes[i] == b'"' {
            // 跳过整个字符串字面量（含 `""` 转义），绝不把 `"B2"` 当作单元格引用着色
            i += 1;
            while i < buf.len() {
                if bytes[i] == b'"' {
                    if i + 1 < buf.len() && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            prev_ident = false;
            continue;
        }
        let ch = buf[i..].chars().next().unwrap();
        if !prev_ident {
            if let Some(end) = parse_ref_token_bytes(buf, i) {
                let right_ok = end == buf.len() || {
                    let rc = buf[end..].chars().next().unwrap();
                    !(rc.is_alphanumeric() || matches!(rc, '_' | '$'))
                };
                if right_ok {
                    let slice = &buf[i..end];
                    let next_idx = seen.len();
                    let ci = *seen.entry(slice).or_insert(next_idx);
                    out.push((i, end, ci));
                    prev_ident = buf[..end]
                        .chars()
                        .next_back()
                        .is_some_and(is_ref_ident_char);
                    i = end;
                    continue;
                }
            }
        }
        prev_ident = is_ref_ident_char(ch);
        i += ch.len_utf8();
    }
    out
}

/// UI-11.3：公式里的引用片段（字符区间 + 调色序号），同一文本共用同色。
pub(crate) fn ref_spans(buf: &str) -> Vec<(usize, usize, usize)> {
    let bspans = ref_byte_spans(buf);
    if bspans.is_empty() {
        return Vec::new();
    }
    if buf.is_ascii() {
        return bspans;
    }
    let mut out = Vec::with_capacity(bspans.len());
    let mut char_idx = 0usize;
    let mut byte_idx = 0usize;
    for (bs, be, ci) in bspans {
        while byte_idx < bs {
            byte_idx += buf[byte_idx..].chars().next().unwrap().len_utf8();
            char_idx += 1;
        }
        let cs = char_idx;
        while byte_idx < be {
            byte_idx += buf[byte_idx..].chars().next().unwrap().len_utf8();
            char_idx += 1;
        }
        out.push((cs, char_idx, ci));
    }
    out
}

/// 引用配色（Excel 习惯的多引用颜色循环）。
pub(crate) fn ref_color(idx: usize) -> egui::Color32 {
    const PALETTE: [egui::Color32; 6] = [
        egui::Color32::from_rgb(31, 111, 208),
        egui::Color32::from_rgb(26, 138, 59),
        egui::Color32::from_rgb(138, 63, 176),
        egui::Color32::from_rgb(161, 92, 0),
        egui::Color32::from_rgb(201, 55, 86),
        egui::Color32::from_rgb(14, 138, 148),
    ];
    PALETTE[idx % PALETTE.len()]
}

/// 解析本表引用 token（支持 `A1`、`$A$1:$C$10`、`Sheet1!B2:D4`、`'工作表 1'!A1`）为 `(r1, c1, r2, c2)`。
pub(crate) fn parse_local_ref_token(tok: &str, cur_sheet_name: &str) -> Option<(u32, u32, u32, u32)> {
    let tail = if let Some(bang) = tok.rfind('!') {
        let raw_sh = &tok[..bang];
        let unquoted = if raw_sh.starts_with('\'') && raw_sh.ends_with('\'') && raw_sh.len() >= 2 {
            std::borrow::Cow::Owned(raw_sh[1..raw_sh.len() - 1].replace("''", "'"))
        } else {
            std::borrow::Cow::Borrowed(raw_sh)
        };
        if !unquoted.eq_ignore_ascii_case(cur_sheet_name) {
            return None;
        }
        &tok[bang + 1..]
    } else {
        tok
    };
    let parse_one = |part: &str| -> Option<(u32, u32)> {
        let clean: String = part.bytes().filter(|&b| b != b'$').map(char::from).collect();
        let (r, c) = parse_a1_ref(&clean)?;
        (r < 1_048_576 && c < 16_384).then_some((r, c))
    };
    if let Some((a, b)) = tail.split_once(':') {
        let (r1, c1) = parse_one(a)?;
        let (r2, c2) = parse_one(b)?;
        Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
    } else {
        let (r, c) = parse_one(tail)?;
        Some((r, c, r, c))
    }
}

/// UI-11.3：编辑框的着色版排版 —— 公式里的引用按出现顺序上色，其余部分正常色（O(L) 字节切片零中间数组分配）。
pub(crate) fn formula_layout_job(
    text: &str,
    font: egui::FontId,
    normal: egui::Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    if !text.starts_with('=') {
        job.append(
            text,
            0.0,
            egui::TextFormat { font_id: font, color: normal, ..Default::default() },
        );
        return job;
    }
    let mut last = 0usize;
    for (bs, be, ci) in ref_byte_spans(text) {
        if bs < last || be > text.len() {
            continue;
        }
        if bs > last {
            job.append(
                &text[last..bs],
                0.0,
                egui::TextFormat { font_id: font.clone(), color: normal, ..Default::default() },
            );
        }
        job.append(
            &text[bs..be],
            0.0,
            egui::TextFormat { font_id: font.clone(), color: ref_color(ci), ..Default::default() },
        );
        last = be;
    }
    if last < text.len() {
        job.append(
            &text[last..],
            0.0,
            egui::TextFormat { font_id: font, color: normal, ..Default::default() },
        );
    }
    if debug_flags::formula_log() {
        let cs: Vec<String> = job
            .sections
            .iter()
            .map(|s| format!("{:?}={:?}", s.byte_range, s.format.color))
            .collect();
        eprintln!("[lay] text={text:?} secs={cs:?}");
    }
    job
}

/// F4 按光标位置循环所在 A1 引用的绝对/相对形式（跳过字符串字面量）：
/// `A1 → $A$1 → A$1 → $A1 → A1`。返回更新后的光标字符下标。
pub(crate) fn cycle_formula_ref_at(buf: &mut String, caret_chars: usize) -> Option<usize> {
    let caret = byte_index_of_char(buf, caret_chars);
    if is_inside_string_literal(buf, caret) {
        return None;
    }
    let bytes = buf.as_bytes();
    let is_ref_b = |b: u8| b.is_ascii_alphanumeric() || b == b'$';
    // 1) 优先定位直接包含或紧邻光标的 A1 token
    let (mut start, mut end) = if caret < bytes.len() && is_ref_b(bytes[caret])
        || (caret > 0 && is_ref_b(bytes[caret - 1]))
    {
        let mut s = caret;
        while s > 0 && is_ref_b(bytes[s - 1]) {
            s -= 1;
        }
        let mut e = caret;
        while e < bytes.len() && is_ref_b(bytes[e]) {
            e += 1;
        }
        (s, e)
    } else {
        // 2) 若光标在运算符/右括号后，向左跳过非引用字符找最近一个不在字符串内的 A1 token
        let mut e = caret;
        while e > 0 && !is_ref_b(bytes[e - 1]) {
            e -= 1;
        }
        if e == 0 || is_inside_string_literal(buf, e) {
            return None;
        }
        let mut s = e;
        while s > 0 && is_ref_b(bytes[s - 1]) {
            s -= 1;
        }
        (s, e)
    };
    if start >= end || is_inside_string_literal(buf, start) {
        return None;
    }
    // 排除函数名/数字，只接受 [A-Za-z$]{1,3}[0-9]+ 且前后不是标识符
    let token = &buf[start..end];
    let tchars: Vec<char> = token.chars().collect();
    let mut tpos = 0usize;
    let mut letters = String::new();
    while tpos < tchars.len() && (tchars[tpos] == '$' || tchars[tpos].is_ascii_alphabetic()) {
        if tchars[tpos] != '$' {
            letters.push(tchars[tpos].to_ascii_uppercase());
        }
        tpos += 1;
    }
    let rest: String = tchars[tpos..].iter().collect();
    if letters.is_empty()
        || letters.len() > 3
        || rest.is_empty()
        || rest.starts_with('0')
        || !rest.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    if start > 0 {
        let prev = buf[..start].chars().next_back().unwrap_or(' ');
        if prev.is_ascii_alphanumeric() || prev == '_' || prev == '.' {
            return None;
        }
    }
    if end < buf.len() {
        let next = buf[end..].chars().next().unwrap_or(' ');
        if next.is_ascii_alphanumeric() || next == '_' || next == '(' {
            return None;
        }
    }
    let _ = (&mut start, &mut end);
    // 解析当前 $ 状态；A1→$A$1→A$1→$A1→A1
    let chars: Vec<char> = token.chars().collect();
    let mut ci = 0;
    let col_abs = chars.get(ci) == Some(&'$');
    if col_abs {
        ci += 1;
    }
    let col_start = ci;
    while ci < chars.len() && chars[ci].is_ascii_alphabetic() {
        ci += 1;
    }
    let col = chars[col_start..ci].iter().collect::<String>();
    let row_abs = chars.get(ci) == Some(&'$');
    if row_abs {
        ci += 1;
    }
    let row = chars[ci..].iter().collect::<String>();
    let next = match (col_abs, row_abs) {
        (false, false) => (true, true),
        (true, true) => (false, true),
        (false, true) => (true, false),
        (true, false) => (false, false),
    };
    let mut out = String::new();
    if next.0 {
        out.push('$');
    }
    out.push_str(&col);
    if next.1 {
        out.push('$');
    }
    out.push_str(&row);
    let prefix_chars = buf[..start].chars().count();
    let new_caret = prefix_chars + out.chars().count();
    buf.replace_range(start..end, &out);
    Some(new_caret)
}

/// F4 循环当前末尾的 A1 引用（兼容旧单参调用）。
#[cfg(test)]
pub(crate) fn cycle_formula_ref(buf: &mut String) -> bool {
    cycle_formula_ref_at(buf, buf.chars().count()).is_some()
}
