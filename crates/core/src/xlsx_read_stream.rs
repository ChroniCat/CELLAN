//! Shared strings and worksheet rows are sliced directly on raw UTF-8 bytes
//! without a pre-pass XML lexer, and decoded by a persistent worker pool.
use super::{
    Formulas, Loaded, RowLayout, SharedMasters, Strings, StyleLookup, expand_shared, office_text,
    parse_section, string_item,
};
use anyhow::{Context, Result, ensure};
use quick_xml::{Reader, events::Event};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Mutex, RwLock, mpsc};

const READ_BUF_BYTES: usize = 256 * 1024;

#[inline]
fn is_xml_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

#[inline]
fn local_tag_name(raw: &[u8]) -> &[u8] {
    match raw.iter().position(|&b| b == b':') {
        Some(colon) => &raw[colon + 1..],
        None => raw,
    }
}

#[derive(Clone, Copy, Debug)]
enum XmlSlice {
    Comment { end: usize },
    CData { end: usize },
    DeclOrPi { end: usize },
    Start { end: usize, name_start: usize, name_end: usize, empty: bool },
    End { end: usize, name_start: usize, name_end: usize },
}

/// Inspect the XML construct starting at `buf[at]` (where `buf[at] == b'<'`).
/// Returns `Ok(None)` when more bytes from the underlying `Read` are required.
fn parse_lt_at(buf: &[u8], at: usize) -> Result<Option<XmlSlice>> {
    let rest = &buf[at..];
    if rest.len() < 2 {
        return Ok(None);
    }
    match rest[1] {
        b'!' => {
            if rest.len() < 4 {
                return Ok(None);
            }
            if rest.starts_with(b"<!--") {
                let Some(rel) = find_subslice(&rest[4..], b"-->") else {
                    return Ok(None);
                };
                return Ok(Some(XmlSlice::Comment { end: at + 4 + rel + 3 }));
            }
            if rest.len() < 9 && b"<![CDATA[".starts_with(rest) {
                return Ok(None);
            }
            if rest.starts_with(b"<![CDATA[") {
                let Some(rel) = find_subslice(&rest[9..], b"]]>") else {
                    return Ok(None);
                };
                return Ok(Some(XmlSlice::CData { end: at + 9 + rel + 3 }));
            }
            // DOCTYPE or other declaration
            let Some(rel) = rest[2..].iter().position(|&b| b == b'>') else {
                return Ok(None);
            };
            Ok(Some(XmlSlice::DeclOrPi { end: at + 2 + rel + 1 }))
        }
        b'?' => {
            let Some(rel) = find_subslice(&rest[2..], b"?>") else {
                return Ok(None);
            };
            Ok(Some(XmlSlice::DeclOrPi { end: at + 2 + rel + 2 }))
        }
        b'/' => {
            let mut i = 2;
            while i < rest.len() && !is_xml_ws(rest[i]) && rest[i] != b'>' {
                i += 1;
            }
            if i == rest.len() {
                return Ok(None);
            }
            let name_end = at + i;
            while i < rest.len() && is_xml_ws(rest[i]) {
                i += 1;
            }
            if i >= rest.len() {
                return Ok(None);
            }
            ensure!(rest[i] == b'>', "无效结束标签");
            ensure!(name_end > at + 2, "空结束标签");
            Ok(Some(XmlSlice::End {
                end: at + i + 1,
                name_start: at + 2,
                name_end,
            }))
        }
        _ => {
            let mut i = 1;
            while i < rest.len() && !is_xml_ws(rest[i]) && rest[i] != b'>' && rest[i] != b'/' {
                i += 1;
            }
            if i == rest.len() {
                return Ok(None);
            }
            let name_end = at + i;
            ensure!(name_end > at + 1, "无效起始标签");
            let mut quote: u8 = 0;
            while i < rest.len() {
                let b = rest[i];
                if quote != 0 {
                    if b == quote {
                        quote = 0;
                    }
                } else if b == b'"' || b == b'\'' {
                    quote = b;
                } else if b == b'>' {
                    let empty = i >= 2 && rest[i - 1] == b'/';
                    return Ok(Some(XmlSlice::Start {
                        end: at + i + 1,
                        name_start: at + 1,
                        name_end,
                        empty,
                    }));
                }
                i += 1;
            }
            Ok(None)
        }
    }
}

#[inline]
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let first = needle[0];
    let mut pos = 0;
    while pos + needle.len() <= haystack.len() {
        let rel = haystack[pos..].iter().position(|&b| b == first)?;
        pos += rel;
        if pos + needle.len() > haystack.len() {
            return None;
        }
        if &haystack[pos..pos + needle.len()] == needle {
            return Some(pos);
        }
        pos += 1;
    }
    None
}

/// Fast scan to the end of a non-nested element (`row` or `si`) starting after its opening `<tag...>`.
/// Skips over `<c>`, `<v>`, `<f>`, `<t>`, etc. in a couple of instructions without parsing attributes,
/// while still respecting XML comments and CDATA sections.
fn find_matching_close(buf: &[u8], mut cursor: usize, local_target: &[u8]) -> Result<Option<usize>> {
    if local_target == b"row" {
        let mut search = cursor;
        while let Some(rel) = buf[search..].iter().position(|&b| b == b'w') {
            let w = search + rel;
            if w >= cursor + 4 && w + 1 < buf.len() && &buf[w - 4..w + 2] == b"</row>" {
                if !buf[cursor..w - 4].contains(&b'!') {
                    return Ok(Some(w + 2));
                }
                break;
            }
            search = w + 1;
        }
    }
    while cursor < buf.len() {
        let Some(rel) = buf[cursor..].iter().position(|&b| b == b'<') else {
            return Ok(None);
        };
        let at = cursor + rel;
        let rest = &buf[at..];
        if rest.len() < 2 {
            return Ok(None);
        }
        match rest[1] {
            b'/' => {
                let Some(XmlSlice::End { end, name_start, name_end }) = parse_lt_at(buf, at)? else {
                    return Ok(None);
                };
                if local_tag_name(&buf[name_start..name_end]) == local_target {
                    return Ok(Some(end));
                }
                cursor = end;
            }
            b'!' => {
                let Some(tok) = parse_lt_at(buf, at)? else {
                    return Ok(None);
                };
                cursor = match tok {
                    XmlSlice::Comment { end }
                    | XmlSlice::CData { end }
                    | XmlSlice::DeclOrPi { end } => end,
                    _ => at + 2,
                };
            }
            b'?' => {
                let Some(XmlSlice::DeclOrPi { end }) = parse_lt_at(buf, at)? else {
                    return Ok(None);
                };
                cursor = end;
            }
            _ => {
                // Ordinary inner start tag (<c>, <v>, <f>, <t>, <r>, ...).
                // Ensure we don't skip past an unclosed quote at buffer tail.
                let mut i = 1;
                let mut quote = 0u8;
                let mut closed_tag = None;
                while i < rest.len() {
                    let b = rest[i];
                    if quote != 0 {
                        if b == quote {
                            quote = 0;
                        }
                    } else if b == b'"' || b == b'\'' {
                        quote = b;
                    } else if b == b'>' {
                        closed_tag = Some(at + i + 1);
                        break;
                    }
                    i += 1;
                }
                let Some(end) = closed_tag else {
                    return Ok(None);
                };
                cursor = end;
            }
        }
    }
    Ok(None)
}

/// Scan to the matching close tag of an arbitrary nested element (e.g. `<sheetViews>...</sheetViews>`).
fn find_nested_close(buf: &[u8], mut cursor: usize, raw_name: &[u8]) -> Result<Option<usize>> {
    let mut depth = 1usize;
    while cursor < buf.len() {
        let Some(rel) = buf[cursor..].iter().position(|&b| b == b'<') else {
            return Ok(None);
        };
        let at = cursor + rel;
        let Some(tok) = parse_lt_at(buf, at)? else {
            return Ok(None);
        };
        match tok {
            XmlSlice::Comment { end } | XmlSlice::CData { end } | XmlSlice::DeclOrPi { end } => {
                cursor = end;
            }
            XmlSlice::Start { end, name_start, name_end, empty } => {
                if !empty && &buf[name_start..name_end] == raw_name {
                    depth += 1;
                }
                cursor = end;
            }
            XmlSlice::End { end, name_start, name_end } => {
                if &buf[name_start..name_end] == raw_name {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(Some(end));
                    }
                }
                cursor = end;
            }
        }
    }
    Ok(None)
}

/// Extract `r="123"` from a `<row ...>` start tag slice without allocating.
fn extract_row_attr(tag_bytes: &[u8]) -> Result<Option<u32>> {
    // Skip `<` + tag name
    let mut i = 1;
    while i < tag_bytes.len() && !is_xml_ws(tag_bytes[i]) && tag_bytes[i] != b'>' && tag_bytes[i] != b'/' {
        i += 1;
    }
    while i < tag_bytes.len() {
        while i < tag_bytes.len() && is_xml_ws(tag_bytes[i]) {
            i += 1;
        }
        if i >= tag_bytes.len() || tag_bytes[i] == b'>' || tag_bytes[i] == b'/' {
            break;
        }
        let key_start = i;
        while i < tag_bytes.len() && !is_xml_ws(tag_bytes[i]) && tag_bytes[i] != b'=' && tag_bytes[i] != b'>' {
            i += 1;
        }
        let key = &tag_bytes[key_start..i];
        while i < tag_bytes.len() && is_xml_ws(tag_bytes[i]) {
            i += 1;
        }
        if i >= tag_bytes.len() || tag_bytes[i] != b'=' {
            break;
        }
        i += 1;
        while i < tag_bytes.len() && is_xml_ws(tag_bytes[i]) {
            i += 1;
        }
        if i >= tag_bytes.len() || (tag_bytes[i] != b'"' && tag_bytes[i] != b'\'') {
            break;
        }
        let quote = tag_bytes[i];
        i += 1;
        let val_start = i;
        while i < tag_bytes.len() && tag_bytes[i] != quote {
            i += 1;
        }
        if i >= tag_bytes.len() {
            break;
        }
        let val = &tag_bytes[val_start..i];
        i += 1;
        if key == b"r" {
            let s = std::str::from_utf8(val).context("无效行号编码")?;
            let n = s.parse::<u32>().context("无效行号")?;
            return Ok(Some(n.checked_sub(1).context("行号为零")?));
        }
    }
    Ok(None)
}

/// Fast zero-allocation decode for standard `<si><t>...</t></si>` and `<si><t xml:space="preserve">...</t></si>`
/// items that contain no nested XML tags, entities (`&`), or CR (`\r`).
fn try_fast_si(si_bytes: &[u8], table: &mut Strings) -> Result<bool> {
    if si_bytes == b"<si/>" || si_bytes == b"<si></si>" {
        table.text.push_str("");
        return Ok(true);
    }
    let Some(body) = si_bytes.strip_prefix(b"<si>").and_then(|b| b.strip_suffix(b"</si>")) else {
        return Ok(false);
    };
    let body = trim_ascii_ws(body);
    if body == b"<t/>" || body == b"<t></t>" {
        table.text.push_str("");
        return Ok(true);
    }
    let (inner, preserve) = if let Some(rest) = body.strip_prefix(b"<t>") {
        let Some(inner) = rest.strip_suffix(b"</t>") else { return Ok(false) };
        (inner, false)
    } else if let Some(rest) = body.strip_prefix(b"<t xml:space=\"preserve\">") {
        let Some(inner) = rest.strip_suffix(b"</t>") else { return Ok(false) };
        (inner, true)
    } else if let Some(rest) = body.strip_prefix(b"<t xml:space='preserve'>") {
        let Some(inner) = rest.strip_suffix(b"</t>") else { return Ok(false) };
        (inner, true)
    } else {
        return Ok(false);
    };
    if inner.iter().any(|&b| b == b'<' || b == b'&' || b == b'\r') {
        return Ok(false);
    }
    let raw = std::str::from_utf8(inner)?;
    let trimmed = if preserve {
        raw
    } else {
        raw.trim_matches([' ', '\t', '\r', '\n'])
    };
    let decoded = office_text(trimmed);
    table.text.push_str(&decoded);
    Ok(true)
}

#[inline]
fn trim_ascii_ws(mut s: &[u8]) -> &[u8] {
    while let Some((&first, rest)) = s.split_first() {
        if is_xml_ws(first) { s = rest; } else { break; }
    }
    while let Some((&last, rest)) = s.split_last() {
        if is_xml_ws(last) { s = rest; } else { break; }
    }
    s
}

struct StreamBuf<R> {
    input: R,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    scratch: Vec<u8>,
}

impl<R: Read> StreamBuf<R> {
    fn new(input: R) -> Self {
        Self {
            input,
            buf: Vec::with_capacity(READ_BUF_BYTES * 2),
            pos: 0,
            eof: false,
            scratch: vec![0u8; READ_BUF_BYTES],
        }
    }

    fn compact(&mut self) {
        if self.pos > 0 && (self.pos >= READ_BUF_BYTES || self.pos * 2 >= self.buf.len()) {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }

    fn fill_more(&mut self) -> Result<bool> {
        if self.eof {
            return Ok(false);
        }
        self.compact();
        let n = self.input.read(&mut self.scratch)?;
        if n == 0 {
            self.eof = true;
            return Ok(false);
        }
        self.buf.extend_from_slice(&self.scratch[..n]);
        Ok(true)
    }

    #[inline]
    fn rem(&self) -> &[u8] {
        &self.buf[self.pos..]
    }
}

pub(super) fn string_table(input: impl Read, table: &mut Strings) -> Result<()> {
    let mut sb = StreamBuf::new(input);
    let (mut opened, mut closed) = (false, false);

    loop {
        let rel = loop {
            if let Some(p) = sb.rem().iter().position(|&b| b == b'<') {
                break Some(p);
            }
            let rem_str = std::str::from_utf8(sb.rem())?;
            ensure!(!closed || rem_str.trim().is_empty(), "sharedStrings.xml 根元素外存在内容");
            sb.pos = sb.buf.len();
            if !sb.fill_more()? {
                break None;
            }
        };
        let Some(rel) = rel else { break };
        if rel > 0 {
            let lead = std::str::from_utf8(&sb.rem()[..rel])?;
            ensure!(!closed || lead.trim().is_empty(), "sharedStrings.xml 根元素外存在内容");
            sb.pos += rel;
        }
        let tok = loop {
            if let Some(tok) = parse_lt_at(&sb.buf, sb.pos)? {
                break tok;
            }
            ensure!(sb.fill_more()?, "sharedStrings.xml 标签未闭合");
        };
        match tok {
            XmlSlice::DeclOrPi { end } | XmlSlice::Comment { end } => {
                let _ = std::str::from_utf8(&sb.buf[sb.pos..end])?;
                sb.pos = end;
            }
            XmlSlice::CData { .. } => anyhow::bail!("无效共享字符串 CDATA"),
            XmlSlice::Start { end, name_start, name_end, empty } => {
                let raw_name = sb.buf[name_start..name_end].to_vec();
                let local = local_tag_name(&raw_name);
                if !opened {
                    ensure!(local == b"sst", "无效共享字符串根元素");
                    let head = std::str::from_utf8(&sb.buf[sb.pos..end])?;
                    if let Some(pos) = head.find("uniqueCount=") {
                        let rest = &head[pos + "uniqueCount=".len()..];
                        if let Some(q) = rest.as_bytes().first().copied().filter(|&b| b == b'"' || b == b'\'') {
                            let val = &rest[1..];
                            if let Some(end_q) = val.find(q as char) {
                                if let Ok(n) = val[..end_q].parse::<usize>() {
                                    table.text.reserve(n.min(2_000_000));
                                }
                            }
                        }
                    }
                    opened = true;
                    closed = empty;
                    sb.pos = end;
                } else {
                    ensure!(!closed, "共享字符串根元素重复");
                    let is_si = local == b"si";
                    let rel_end = end - sb.pos;
                    let rel_item_end = if empty {
                        rel_end
                    } else if is_si {
                        let mut rel_search = rel_end;
                        loop {
                            if let Some(close_end) =
                                find_matching_close(&sb.buf, sb.pos + rel_search, b"si")?
                            {
                                break close_end - sb.pos;
                            }
                            rel_search = sb.rem().len().saturating_sub(8).max(rel_end);
                            ensure!(sb.fill_more()?, "sharedStrings.xml 条目未闭合");
                        }
                    } else {
                        loop {
                            if let Some(close_end) =
                                find_nested_close(&sb.buf, sb.pos + rel_end, &raw_name)?
                            {
                                break close_end - sb.pos;
                            }
                            ensure!(sb.fill_more()?, "sharedStrings.xml 子元素未闭合");
                        }
                    };
                    let item_end = sb.pos + rel_item_end;
                    let slice = &sb.buf[sb.pos..item_end];
                    if is_si {
                        if !try_fast_si(slice, table)? {
                            let xml = std::str::from_utf8(slice)?;
                            let mut one = Reader::from_str(xml);
                            let (text, runs) = match one.read_event()? {
                                Event::Start(_) => string_item(&mut one)?,
                                Event::Empty(_) => Default::default(),
                                _ => anyhow::bail!("无效共享字符串条目"),
                            };
                            if !runs.is_empty() {
                                table.rich.insert(table.text.len(), runs);
                            }
                            table.text.push(text);
                        }
                    } else {
                        let _ = std::str::from_utf8(slice)?;
                    }
                    sb.pos = item_end;
                }
            }
            XmlSlice::End { end, name_start, name_end } => {
                ensure!(opened && !closed, "sharedStrings.xml 结束标签无效");
                ensure!(local_tag_name(&sb.buf[name_start..name_end]) == b"sst", "sharedStrings.xml 根结束标签不匹配");
                closed = true;
                sb.pos = end;
            }
        }
    }
    ensure!(opened && closed, "sharedStrings.xml 未正常结束");
    Ok(())
}

#[derive(Default)]
struct Chunk {
    first_row: u32,
    bytes: Vec<u8>,
}

struct Rows {
    local: Loaded,
    cells: Vec<((u32, u32), crate::model::Cell)>,
    styles: Vec<(crate::model::CellRef, u16)>,
    row_layout: RowLayout,
    shared: Formulas,
    shell: String,
}

#[inline]
fn parse_u32_ascii(b: &[u8]) -> Option<u32> {
    if b.is_empty() || b.len() > 10 {
        return None;
    }
    let mut acc = 0u64;
    for &ch in b {
        let d = ch.wrapping_sub(b'0');
        if d > 9 {
            return None;
        }
        acc = acc * 10 + u64::from(d);
    }
    u32::try_from(acc).ok()
}

#[inline]
fn parse_a1_bytes(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 0usize;
    let mut col = 0u32;
    while i < b.len() {
        let ch = b[i];
        if ch.is_ascii_uppercase() {
            col = col.checked_mul(26)?.checked_add(u32::from(ch - b'A' + 1))?;
            i += 1;
        } else {
            break;
        }
    }
    if i == 0 || i > 3 || i >= b.len() {
        return None;
    }
    let row = parse_u32_ascii(&b[i..])?;
    if row == 0 || col == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

#[inline]
fn fast_parse_f64_bytes(b: &[u8]) -> Option<f64> {
    if !b.is_empty() && b.len() <= 16 {
        let (neg, digits) = match b[0] {
            b'-' => (true, &b[1..]),
            b'+' => (false, &b[1..]),
            _ => (false, b),
        };
        if !digits.is_empty() && digits.len() <= 15 {
            let mut acc = 0u64;
            let mut all_digits = true;
            for &ch in digits {
                let d = ch.wrapping_sub(b'0');
                if d <= 9 {
                    acc = acc * 10 + u64::from(d);
                } else {
                    all_digits = false;
                    break;
                }
            }
            if all_digits {
                return Some(if neg { -(acc as f64) } else { acc as f64 });
            }
        }
    }
    std::str::from_utf8(b).ok()?.parse::<f64>().ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FastRow {
    Ok,
    FallbackRow,
    FallbackChunk,
}

/// Fast raw-byte decoder for an individual plain `<row ...>...</row>` containing no entities,
/// comments, namespaces, formulas, or inline strings. Returns `FallbackRow` on complex cells
/// (e.g. formulas) so only that row uses `quick-xml`, or `FallbackChunk` if row numbering is implicit.
fn try_fast_decode_row(
    row: &[u8],
    table: &Strings,
    lookup: &StyleLookup<'_>,
    col_has_xf: &[bool],
    cells: &mut Vec<((u32, u32), crate::model::Cell)>,
    styles: &mut Vec<(crate::model::CellRef, u16)>,
    row_layout: &mut RowLayout,
) -> FastRow {
    if !row.starts_with(b"<row") {
        return FastRow::FallbackChunk;
    }
    let Some(&after_name) = row.get(4) else {
        return FastRow::FallbackChunk;
    };
    if !is_xml_ws(after_name) && after_name != b'>' && after_name != b'/' {
        return FastRow::FallbackChunk;
    }

    // Parse `<row ...>` open tag attributes
    let mut i = 4usize;
    let mut row_idx: Option<u32> = None;
    let mut ht = 0f32;
    let mut custom_ht = false;
    let mut hidden = false;
    let mut row_s = 0u32;
    let mut row_lv = 0u8;
    let open_end;
    let row_empty;
    loop {
        while i < row.len() && is_xml_ws(row[i]) {
            i += 1;
        }
        if i >= row.len() {
            return FastRow::FallbackChunk;
        }
        if row[i] == b'>' {
            open_end = i + 1;
            row_empty = false;
            break;
        }
        if row[i] == b'/' {
            if row.get(i + 1) != Some(&b'>') {
                return FastRow::FallbackChunk;
            }
            open_end = i + 2;
            row_empty = true;
            break;
        }
        let ks = i;
        while i < row.len() && !is_xml_ws(row[i]) && row[i] != b'=' && row[i] != b'>' && row[i] != b'/' {
            i += 1;
        }
        let key = &row[ks..i];
        while i < row.len() && is_xml_ws(row[i]) {
            i += 1;
        }
        if row.get(i) != Some(&b'=') {
            return FastRow::FallbackChunk;
        }
        i += 1;
        while i < row.len() && is_xml_ws(row[i]) {
            i += 1;
        }
        let Some(&q) = row.get(i).filter(|&&b| b == b'"' || b == b'\'') else {
            return FastRow::FallbackChunk;
        };
        i += 1;
        let vs = i;
        while i < row.len() && row[i] != q {
            if row[i] == b'<' {
                return FastRow::FallbackChunk;
            }
            i += 1;
        }
        if i >= row.len() {
            return FastRow::FallbackChunk;
        }
        let val = &row[vs..i];
        i += 1;
        match key {
            b"r" => {
                let Some(n) = parse_u32_ascii(val).and_then(|v| v.checked_sub(1)) else {
                    return FastRow::FallbackChunk;
                };
                if n >= crate::sync::MAX_ROWS {
                    return FastRow::FallbackChunk;
                }
                row_idx = Some(n);
            }
            b"ht" => {
                ht = std::str::from_utf8(val).ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            }
            b"customHeight" => {
                custom_ht = val == b"1" || val == b"true";
            }
            b"hidden" => {
                hidden = val == b"1" || val == b"true";
            }
            b"s" => {
                row_s = parse_u32_ascii(val).unwrap_or(0);
            }
            b"outlineLevel" => {
                row_lv = parse_u32_ascii(val).and_then(|v| u8::try_from(v).ok()).unwrap_or(0);
            }
            _ => {}
        }
    }

    let Some(r_idx) = row_idx else {
        return FastRow::FallbackChunk;
    };
    let row_has_xf = row_s > 0;
    if row_lv > 0 {
        row_layout.row_levels.push((r_idx, row_lv));
    }
    if row_s > 0 {
        row_layout.row_xfs.push((r_idx, row_s));
    }
    if hidden {
        row_layout.hidden_rows.push(r_idx);
    }
    if ht > 0.0 && custom_ht {
        let px = crate::layout::pt_to_px(ht);
        if (px - crate::layout::DEFAULT_ROW_H).abs() > 0.5 {
            row_layout.row_heights.push((r_idx, px));
        }
    }
    if row_empty {
        return if open_end == row.len() {
            FastRow::Ok
        } else {
            FastRow::FallbackChunk
        };
    }
    if row.len() < open_end + 6 || &row[row.len() - 6..] != b"</row>" {
        return FastRow::FallbackChunk;
    }

    let body = &row[open_end..row.len() - 6];
    let mut p = 0usize;
    while p < body.len() {
        while p < body.len() && is_xml_ws(body[p]) {
            p += 1;
        }
        if p >= body.len() {
            break;
        }
        if !body[p..].starts_with(b"<c") {
            return FastRow::FallbackRow;
        }
        let Some(&after_c) = body.get(p + 2) else {
            return FastRow::FallbackRow;
        };
        if !is_xml_ws(after_c) && after_c != b'>' && after_c != b'/' {
            return FastRow::FallbackRow;
        }
        p += 2;
        let mut here: Option<(u32, u32)> = None;
        let mut xf: Option<u16> = None;
        let mut typ: &[u8] = b"";
        let c_empty;
        loop {
            while p < body.len() && is_xml_ws(body[p]) {
                p += 1;
            }
            if p >= body.len() {
                return FastRow::FallbackRow;
            }
            if body[p] == b'>' {
                p += 1;
                c_empty = false;
                break;
            }
            if body[p] == b'/' {
                if body.get(p + 1) != Some(&b'>') {
                    return FastRow::FallbackRow;
                }
                p += 2;
                c_empty = true;
                break;
            }
            let ks = p;
            while p < body.len() && !is_xml_ws(body[p]) && body[p] != b'=' && body[p] != b'>' && body[p] != b'/' {
                p += 1;
            }
            let key = &body[ks..p];
            while p < body.len() && is_xml_ws(body[p]) {
                p += 1;
            }
            if body.get(p) != Some(&b'=') {
                return FastRow::FallbackRow;
            }
            p += 1;
            while p < body.len() && is_xml_ws(body[p]) {
                p += 1;
            }
            let Some(&q) = body.get(p).filter(|&&b| b == b'"' || b == b'\'') else {
                return FastRow::FallbackRow;
            };
            p += 1;
            let vs = p;
            while p < body.len() && body[p] != q {
                if body[p] == b'<' {
                    return FastRow::FallbackRow;
                }
                p += 1;
            }
            if p >= body.len() {
                return FastRow::FallbackRow;
            }
            let val = &body[vs..p];
            p += 1;
            match key {
                b"r" => {
                    let Some((cr, cc)) = parse_a1_bytes(val) else {
                        return FastRow::FallbackRow;
                    };
                    if cr >= crate::sync::MAX_ROWS || cc >= crate::sync::MAX_COLS {
                        return FastRow::FallbackRow;
                    }
                    here = Some((cr, cc));
                }
                b"s" => {
                    let Some(s) = parse_u32_ascii(val).and_then(|v| u16::try_from(v).ok()) else {
                        return FastRow::FallbackRow;
                    };
                    xf = Some(s);
                }
                b"t" => {
                    if !matches!(val, b"" | b"n" | b"s" | b"b") {
                        return FastRow::FallbackRow;
                    }
                    typ = val;
                }
                b"cm" => return FastRow::FallbackRow,
                _ => {}
            }
        }
        let Some((cr, cc)) = here else {
            return FastRow::FallbackRow;
        };
        if let Some(raw_xf) = xf {
            let id = lookup.resolve(raw_xf);
            let col_styled = col_has_xf.get(cc as usize).copied().unwrap_or(false);
            if raw_xf != 0 || id != 0 || row_has_xf || col_styled {
                styles.push((crate::model::CellRef::new(cr, cc), id));
            }
        }
        if c_empty {
            continue;
        }
        while p < body.len() && is_xml_ws(body[p]) {
            p += 1;
        }
        if body[p..].starts_with(b"</c>") {
            p += 4;
            continue;
        }
        if !body[p..].starts_with(b"<v>") {
            return FastRow::FallbackRow;
        }
        p += 3;
        let v_start = p;
        let Some(lt_rel) = body[p..].iter().position(|&b| b == b'<') else {
            return FastRow::FallbackRow;
        };
        let v_end = p + lt_rel;
        if !body[v_end..].starts_with(b"</v>") {
            return FastRow::FallbackRow;
        }
        let v_bytes = &body[v_start..v_end];
        p = v_end + 4;
        while p < body.len() && is_xml_ws(body[p]) {
            p += 1;
        }
        if !body[p..].starts_with(b"</c>") {
            return FastRow::FallbackRow;
        }
        p += 4;

        let value = match typ {
            b"s" if !v_bytes.is_empty() => {
                let trimmed = trim_ascii_ws(v_bytes);
                let Some(id) = parse_u32_ascii(trimmed).map(|v| v as usize) else {
                    return FastRow::FallbackRow;
                };
                if table.rich.contains_key(&id) {
                    return FastRow::FallbackRow;
                }
                let Some(arc) = table.text.get_arc(id) else {
                    return FastRow::FallbackRow;
                };
                crate::model::Value::Text(std::sync::Arc::clone(arc))
            }
            _ if v_bytes.is_empty() => crate::model::Value::Empty,
            b"" => match fast_parse_f64_bytes(v_bytes) {
                Some(n) => crate::model::Value::Number(n),
                None => match std::str::from_utf8(v_bytes) {
                    Ok(s) => crate::model::Value::Text(s.into()),
                    Err(_) => return FastRow::FallbackRow,
                },
            },
            b"n" => match fast_parse_f64_bytes(v_bytes) {
                Some(n) => crate::model::Value::Number(n),
                None => return FastRow::FallbackRow,
            },
            b"b" => {
                let trimmed = trim_ascii_ws(v_bytes);
                if trimmed.is_empty() {
                    crate::model::Value::Empty
                } else if trimmed == b"1" || trimmed.eq_ignore_ascii_case(b"true") {
                    crate::model::Value::Bool(true)
                } else if trimmed == b"0" || trimmed.eq_ignore_ascii_case(b"false") {
                    crate::model::Value::Bool(false)
                } else {
                    return FastRow::FallbackRow;
                }
            }
            _ => return FastRow::FallbackRow,
        };
        if !value.is_empty() {
            cells.push((
                (cr, cc),
                crate::model::Cell {
                    value,
                    formula: None,
                },
            ));
        }
    }
    FastRow::Ok
}

impl Rows {
    fn decode(
        chunk: Chunk,
        table: &Strings,
        style_lookup: Option<&StyleLookup<'_>>,
        col_has_xf: &[bool],
        shared_masters: &RwLock<SharedMasters>,
        name: &str,
        part: &str,
    ) -> Result<Self> {
        let mut local = Loaded::empty(name, part);
        let mut cells = Vec::with_capacity(chunk.bytes.len() / 24);
        let mut styles = Vec::new();
        let mut shared = Formulas::default();
        let mut shell = String::new();

        // Try fast raw-byte row decoding when `style_lookup` is active and the chunk has no
        // comments/PIs/entities/namespace prefixes. Any individual row containing formulas,
        // inline strings, rich SST items, or implicit coordinates falls back seamlessly to
        // `parse_section`.
        let can_fast = style_lookup.is_some()
            && !chunk
                .bytes
                .iter()
                .any(|&b| b == b'!' || b == b'?' || b == b'&' || b == b':' || b == b'\r');
        let mut used_fast = false;
        if can_fast {
            let lookup = style_lookup.unwrap();
            let bytes = chunk.bytes.as_slice();
            let mut pos = 0usize;
            let mut fast_ok = true;
            let mut row_layout = RowLayout::default();
            while pos < bytes.len() {
                let ws_start = pos;
                while pos < bytes.len() && is_xml_ws(bytes[pos]) {
                    pos += 1;
                }
                if pos > ws_start {
                    // Preserve any inter-row whitespace in `shell` just like `parse_section`.
                    let Ok(ws) = std::str::from_utf8(&bytes[ws_start..pos]) else {
                        fast_ok = false;
                        break;
                    };
                    shell.push_str(ws);
                }
                if pos >= bytes.len() {
                    break;
                }
                if !bytes[pos..].starts_with(b"<row") {
                    fast_ok = false;
                    break;
                }
                // Find end of this `<row ...>` element
                let Some(open_gt) = bytes[pos..].iter().position(|&b| b == b'>') else {
                    fast_ok = false;
                    break;
                };
                let open_end = pos + open_gt + 1;
                let row_end = if open_gt >= 1 && bytes[pos + open_gt - 1] == b'/' {
                    open_end
                } else {
                    let mut search = open_end;
                    let mut found = None;
                    while let Some(rel) = bytes[search..].iter().position(|&b| b == b'w') {
                        let w = search + rel;
                        if w >= open_end + 4 && w + 1 < bytes.len() && &bytes[w - 4..w + 2] == b"</row>" {
                            found = Some(w + 2);
                            break;
                        }
                        search = w + 1;
                    }
                    let Some(e) = found else {
                        fast_ok = false;
                        break;
                    };
                    e
                };
                let row_slice = &bytes[pos..row_end];
                let saved_lens = (
                    cells.len(),
                    styles.len(),
                    row_layout.row_heights.len(),
                    row_layout.hidden_rows.len(),
                    row_layout.row_levels.len(),
                    row_layout.row_xfs.len(),
                );
                match try_fast_decode_row(
                    row_slice,
                    table,
                    lookup,
                    col_has_xf,
                    &mut cells,
                    &mut styles,
                    &mut row_layout,
                ) {
                    FastRow::Ok => {}
                    FastRow::FallbackChunk => {
                        fast_ok = false;
                        break;
                    }
                    FastRow::FallbackRow => {
                        cells.truncate(saved_lens.0);
                        styles.truncate(saved_lens.1);
                        row_layout.row_heights.truncate(saved_lens.2);
                        row_layout.hidden_rows.truncate(saved_lens.3);
                        row_layout.row_levels.truncate(saved_lens.4);
                        row_layout.row_xfs.truncate(saved_lens.5);
                        let row_xml = std::str::from_utf8(row_slice)?;
                        let row_shell = parse_section(
                            row_xml,
                            table,
                            style_lookup,
                            col_has_xf,
                            Some(shared_masters),
                            &mut local,
                            calamine::SheetType::WorkSheet,
                            &mut shared,
                            Some((&mut cells, &mut styles)),
                            chunk.first_row,
                        )?;
                        shell.push_str(&row_shell);
                        row_layout.extend(std::mem::take(&mut local.row_layout));
                    }
                }
                pos = row_end;
            }
            if fast_ok {
                local.row_layout = row_layout;
                used_fast = true;
            } else {
                // Full clean rollback to `parse_section` for the whole chunk.
                cells.clear();
                styles.clear();
                shared = Formulas::default();
                shell.clear();
                local = Loaded::empty(name, part);
            }
        }

        if !used_fast {
            let xml = std::str::from_utf8(&chunk.bytes)?;
            shell = parse_section(
                xml,
                table,
                style_lookup,
                col_has_xf,
                Some(shared_masters),
                &mut local,
                calamine::SheetType::WorkSheet,
                &mut shared,
                Some((&mut cells, &mut styles)),
                chunk.first_row,
            )?;
        }

        let row_layout = std::mem::take(&mut local.row_layout);
        // When coordinates in the chunk are strictly unique (normal OOXML), reorder cells by
        // `(page, Reverse(col), Reverse(row))` so the single-threaded merger's `Page::ensure`
        // allocates each `page.cols` and each 256-slot column `Vec<Cell>` ONCE (zero reallocs).
        if cells.len() >= 32 {
            let mut strictly_ordered = true;
            let mut prev: Option<(u32, u32)> = None;
            for &((r, c), _) in &cells {
                if let Some(p) = prev {
                    if (r, c) <= p {
                        strictly_ordered = false;
                        break;
                    }
                }
                prev = Some((r, c));
            }
            if strictly_ordered {
                cells.sort_unstable_by_key(|&((r, c), _)| {
                    (
                        r / crate::store::PAGE_ROWS,
                        std::cmp::Reverse(c),
                        std::cmp::Reverse(r),
                    )
                });
            }
        }
        // Keep styles in ascending `(page, col, row)` order so `StyleColumn::insert` appends in O(1).
        if styles.len() >= 64 {
            styles.sort_unstable_by_key(|&(k, _)| (k.row / crate::store::PAGE_ROWS, k.col, k.row));
        }
        Ok(Self {
            local,
            cells,
            styles,
            row_layout,
            shared,
            shell,
        })
    }

    fn append(self, out: &mut Loaded, formulas: &mut Formulas, shell: &mut String) -> Result<()> {
        out.sheet.source_needs_dom_repair |= self.local.sheet.source_needs_dom_repair;
        for ((r, c), cell) in self.cells {
            out.sheet.set(r, c, cell);
        }
        for (key, id) in self.styles {
            out.sheet.styles.insert(key, id);
        }
        out.xfs.extend(self.local.xfs);
        out.row_layout.extend(self.row_layout);
        out.sheet.rich.extend(self.local.sheet.rich);
        out.sheet.arrays.extend(self.local.sheet.arrays);
        out.sheet.spill_shapes.extend(self.local.sheet.spill_shapes);
        for p in self.local.diag.problems {
            if !out.diag.problems.iter().any(|old| old.what == p.what) {
                out.diag.problems.push(p);
            }
        }
        for (index, master) in self.shared.masters {
            ensure!(
                formulas.masters.insert(index, master).is_none(),
                "共享公式序号重复"
            );
        }
        formulas.children.extend(self.shared.children);
        shell.push_str(&self.shell);
        Ok(())
    }
}

enum NonRowPiece {
    Shell(String),
}

enum StreamItem {
    RowChunk(Chunk),
    Shell(String),
}

fn scan_worksheet_stream<R: Read>(
    input: R,
    chunk_size: usize,
    col_has_xf_out: &RwLock<Vec<bool>>,
    mut emit: impl FnMut(StreamItem) -> Result<()>,
) -> Result<()> {
    let mut sb = StreamBuf::new(input);
    let (mut opened, mut closed, mut in_data, mut saw_data) = (false, false, false, false);
    let mut root_raw: Vec<u8> = Vec::new();
    let mut data_raw: Vec<u8> = Vec::new();
    let mut next_row = 0u32;
    let mut chunk = Chunk::default();

    let flush_chunk = |chunk: &mut Chunk, emit: &mut dyn FnMut(StreamItem) -> Result<()>| -> Result<()> {
        if !chunk.bytes.is_empty() {
            emit(StreamItem::RowChunk(std::mem::take(chunk)))?;
        }
        Ok(())
    };

    loop {
        let rel = loop {
            if let Some(p) = sb.rem().iter().position(|&b| b == b'<') {
                break Some(p);
            }
            if sb.rem().is_empty() {
                if !sb.fill_more()? {
                    break None;
                }
                continue;
            }
            let text = std::str::from_utf8(sb.rem())?;
            if closed {
                ensure!(text.trim().is_empty(), "工作表存在根外内容");
            } else if in_data {
                if !chunk.bytes.is_empty() {
                    chunk.bytes.extend_from_slice(sb.rem());
                } else if !text.trim().is_empty() {
                    anyhow::bail!("sheetData 含非法文本");
                } else {
                    emit(StreamItem::Shell(text.to_owned()))?;
                }
            } else {
                emit(StreamItem::Shell(text.to_owned()))?;
            }
            sb.pos = sb.buf.len();
            if !sb.fill_more()? {
                break None;
            }
        };
        let Some(rel) = rel else { break };
        if rel > 0 {
            let lead_bytes = &sb.rem()[..rel];
            let lead = std::str::from_utf8(lead_bytes)?;
            if closed {
                ensure!(lead.trim().is_empty(), "工作表存在根外内容");
            } else if in_data {
                if !chunk.bytes.is_empty() {
                    chunk.bytes.extend_from_slice(lead_bytes);
                } else {
                    ensure!(lead.trim().is_empty(), "sheetData 含非法文本");
                    emit(StreamItem::Shell(lead.to_owned()))?;
                }
            } else {
                emit(StreamItem::Shell(lead.to_owned()))?;
            }
            sb.pos += rel;
        }

        let tok = loop {
            if let Some(tok) = parse_lt_at(&sb.buf, sb.pos)? {
                break tok;
            }
            ensure!(sb.fill_more()?, "工作表 XML 标签未闭合");
        };

        match tok {
            XmlSlice::DeclOrPi { end } => {
                let s = std::str::from_utf8(&sb.buf[sb.pos..end])?.to_owned();
                flush_chunk(&mut chunk, &mut emit)?;
                emit(StreamItem::Shell(s))?;
                sb.pos = end;
            }
            XmlSlice::Comment { end } => {
                let slice = &sb.buf[sb.pos..end];
                let s = std::str::from_utf8(slice)?;
                if in_data && !chunk.bytes.is_empty() {
                    chunk.bytes.extend_from_slice(slice);
                } else {
                    let s = s.to_owned();
                    flush_chunk(&mut chunk, &mut emit)?;
                    emit(StreamItem::Shell(s))?;
                }
                sb.pos = end;
            }
            XmlSlice::CData { .. } => anyhow::bail!("工作表顶层含非法 CDATA"),
            XmlSlice::Start { end, name_start, name_end, empty } => {
                // Fast zero-allocation branch for `<row>` inside `<sheetData>`.
                if opened && !closed && in_data && local_tag_name(&sb.buf[name_start..name_end]) == b"row" {
                    let rel_end = end - sb.pos;
                    let r_idx = extract_row_attr(&sb.buf[sb.pos..end])?.unwrap_or(next_row);
                    next_row = r_idx.saturating_add(1);
                    let rel_row_end = if empty {
                        rel_end
                    } else {
                        let mut rel_search = rel_end;
                        loop {
                            if let Some(close_end) =
                                find_matching_close(&sb.buf, sb.pos + rel_search, b"row")?
                            {
                                break close_end - sb.pos;
                            }
                            rel_search = sb.rem().len().saturating_sub(12).max(rel_end);
                            ensure!(sb.fill_more()?, "工作表 row 未闭合");
                        }
                    };
                    let row_end = sb.pos + rel_row_end;
                    if chunk.bytes.is_empty() {
                        chunk.first_row = r_idx;
                        chunk.bytes.reserve(chunk_size.saturating_add(65536));
                    }
                    chunk.bytes.extend_from_slice(&sb.buf[sb.pos..row_end]);
                    sb.pos = row_end;
                    if chunk.bytes.len() >= chunk_size
                        && (next_row % crate::store::PAGE_ROWS == 0
                            || chunk.bytes.len() >= chunk_size.saturating_mul(2))
                    {
                        flush_chunk(&mut chunk, &mut emit)?;
                    }
                    continue;
                }
                let raw_name = sb.buf[name_start..name_end].to_vec();
                let local = local_tag_name(&raw_name);
                if !opened {
                    ensure!(local == b"worksheet", "工作表缺少 worksheet 根元素");
                    let s = std::str::from_utf8(&sb.buf[sb.pos..end])?.to_owned();
                    opened = true;
                    closed = empty;
                    root_raw = raw_name;
                    emit(StreamItem::Shell(s))?;
                    sb.pos = end;
                } else {
                    ensure!(!closed, "工作表存在多个根元素");
                    if !in_data && local == b"sheetData" {
                        ensure!(!saw_data, "重复的 sheetData");
                        let s = std::str::from_utf8(&sb.buf[sb.pos..end])?.to_owned();
                        saw_data = true;
                        in_data = !empty;
                        data_raw = raw_name;
                        emit(StreamItem::Shell(s))?;
                        sb.pos = end;
                    } else {
                        let rel_end = end - sb.pos;
                        let rel_elem_end = if empty {
                            rel_end
                        } else {
                            loop {
                                if let Some(close_end) =
                                    find_nested_close(&sb.buf, sb.pos + rel_end, &raw_name)?
                                {
                                    break close_end - sb.pos;
                                }
                                ensure!(sb.fill_more()?, "工作表元素未闭合");
                            }
                        };
                        let elem_end = sb.pos + rel_elem_end;
                        let s = std::str::from_utf8(&sb.buf[sb.pos..elem_end])?.to_owned();
                        if !in_data && local == b"cols" {
                            record_col_xfs(&s, col_has_xf_out);
                        }
                        flush_chunk(&mut chunk, &mut emit)?;
                        emit(StreamItem::Shell(s))?;
                        sb.pos = elem_end;
                    }
                }
            }
            XmlSlice::End { end, name_start, name_end } => {
                ensure!(opened && !closed, "工作表 XML 标签不匹配");
                let raw_name = &sb.buf[name_start..name_end];
                if in_data {
                    ensure!(raw_name == data_raw.as_slice(), "sheetData 结束标签不匹配");
                    in_data = false;
                } else {
                    ensure!(raw_name == root_raw.as_slice(), "worksheet 结束标签不匹配");
                    closed = true;
                }
                let s = std::str::from_utf8(&sb.buf[sb.pos..end])?.to_owned();
                flush_chunk(&mut chunk, &mut emit)?;
                emit(StreamItem::Shell(s))?;
                sb.pos = end;
            }
        }
    }
    flush_chunk(&mut chunk, &mut emit)?;
    ensure!(opened && closed && saw_data && !in_data, "工作表 XML 不完整");
    Ok(())
}

pub(super) fn record_col_xfs(cols_xml: &str, col_has_xf_out: &RwLock<Vec<bool>>) {
    let mut reader = Reader::from_str(cols_xml);
    let mut flags = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().as_ref() == "col" => {
                let mut min = 0u32;
                let mut max = 0u32;
                let mut style_xf = 0u32;
                for a in e.attributes().flatten() {
                    match a.key.as_ref() {
                        k if k.ends_with("min") => min = a.value.parse().unwrap_or(0),
                        k if k.ends_with("max") => max = a.value.parse().unwrap_or(0),
                        "style" => style_xf = a.value.parse().unwrap_or(0),
                        _ => {}
                    }
                }
                if style_xf > 0 && min >= 1 && max >= min {
                    let end = max.min(16_384) as usize;
                    if flags.len() < end {
                        flags.resize(end, false);
                    }
                    for c in (min as usize - 1)..end {
                        flags[c] = true;
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if !flags.is_empty() {
        let mut guard = col_has_xf_out.write().unwrap();
        if guard.len() < flags.len() {
            guard.resize(flags.len(), false);
        }
        for (i, on) in flags.into_iter().enumerate() {
            if on {
                guard[i] = true;
            }
        }
    }
}

pub(super) fn worksheet(
    input: impl Read,
    table: &Strings,
    out: &mut Loaded,
    workers: usize,
    chunk_size: usize,
) -> Result<String> {
    worksheet_with_styles(input, table, None, out, workers, chunk_size)
}

pub(super) fn worksheet_with_styles(
    input: impl Read,
    table: &Strings,
    style_lookup: Option<&StyleLookup<'_>>,
    out: &mut Loaded,
    workers: usize,
    chunk_size: usize,
) -> Result<String> {
    let mut shared = Formulas::default();
    let mut shell = String::new();
    let col_has_xf = RwLock::new(Vec::<bool>::new());
    let shared_masters = RwLock::new(SharedMasters::default());
    let name = out.sheet.name.clone();
    let part = out.sheet.source_part.clone().unwrap_or_default();

    if workers <= 1 {
        scan_worksheet_stream(input, chunk_size.max(1), &col_has_xf, |item| match item {
            StreamItem::RowChunk(chunk) => {
                let cols = col_has_xf.read().unwrap();
                let rows = Rows::decode(
                    chunk,
                    table,
                    style_lookup,
                    &cols,
                    &shared_masters,
                    &name,
                    &part,
                )?;
                drop(cols);
                rows.append(out, &mut shared, &mut shell)
            }
            StreamItem::Shell(s) => {
                shell.push_str(&s);
                Ok(())
            }
        })?;
        expand_shared(out, shared)?;
        return Ok(shell);
    }

    enum OrderedEvent {
        Rows(Result<Rows>),
        Shell(NonRowPiece),
    }

    let worker_count = workers.max(2);
    let (tx_work, rx_work) = mpsc::sync_channel::<(usize, Chunk)>(worker_count * 2);
    let rx_work = Mutex::new(rx_work);
    let (tx_done, rx_done) = mpsc::sync_channel::<(usize, OrderedEvent)>(worker_count * 3);
    let (name_ref, part_ref) = (name.as_str(), part.as_str());

    std::thread::scope(|pool| -> Result<()> {
        for _ in 0..worker_count {
            let rx_work = &rx_work;
            let tx_done = tx_done.clone();
            let col_has_xf = &col_has_xf;
            let shared_masters = &shared_masters;
            pool.spawn(move || {
                loop {
                    let next = rx_work.lock().unwrap().recv();
                    let Ok((seq, chunk)) = next else { break };
                    let cols = col_has_xf.read().unwrap().clone();
                    let res = Rows::decode(
                        chunk,
                        table,
                        style_lookup,
                        &cols,
                        shared_masters,
                        name_ref,
                        part_ref,
                    );
                    if tx_done.send((seq, OrderedEvent::Rows(res))).is_err() {
                        break;
                    }
                }
            });
        }

        let tx_shell = tx_done.clone();
        drop(tx_done);

        // Merger runs concurrently on a dedicated scoped thread while the current thread
        // streams `input: impl Read` (which does not need to be `Send`).
        let merger = pool.spawn(move || -> Result<(Loaded, Formulas, String)> {
            let mut merged = Loaded::empty(name_ref, part_ref);
            let mut formulas = Formulas::default();
            let mut merged_shell = String::new();
            let mut pending: BTreeMap<usize, OrderedEvent> = BTreeMap::new();
            let mut next_seq = 0usize;
            while let Ok((seq, ev)) = rx_done.recv() {
                pending.insert(seq, ev);
                while let Some(ready) = pending.remove(&next_seq) {
                    next_seq += 1;
                    match ready {
                        OrderedEvent::Rows(res) => {
                            res?.append(&mut merged, &mut formulas, &mut merged_shell)?;
                        }
                        OrderedEvent::Shell(NonRowPiece::Shell(s)) => {
                            merged_shell.push_str(&s);
                        }
                    }
                }
            }
            Ok((merged, formulas, merged_shell))
        });

        let mut seq = 0usize;
        let scan_res = scan_worksheet_stream(input, chunk_size.max(1), &col_has_xf, |item| {
            let cur = seq;
            seq += 1;
            match item {
                StreamItem::RowChunk(chunk) => {
                    tx_work.send((cur, chunk)).map_err(|_| anyhow::anyhow!("行块工作线程提前退出"))?;
                }
                StreamItem::Shell(s) => {
                    tx_shell
                        .send((cur, OrderedEvent::Shell(NonRowPiece::Shell(s))))
                        .map_err(|_| anyhow::anyhow!("行块归并线程提前退出"))?;
                }
            }
            Ok(())
        });
        drop(tx_work);
        drop(tx_shell);

        let join_res = merger
            .join()
            .map_err(|_| anyhow::anyhow!("行块归并线程异常退出"))?;
        let (merged, formulas, merged_shell) = join_res?;
        *out = merged;
        shared = formulas;
        shell = merged_shell;
        scan_res
    })?;

    expand_shared(out, shared)?;
    Ok(shell)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    struct CutConnection;
    impl Read for CutConnection {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("connection interrupted"))
        }
    }

    #[test]
    fn shared_text_offsets_stay_valid_across_slabs_and_empty_items() {
        let mut table = Strings::default();
        let labels = [
            "a".repeat(62000),
            String::new(),
            "b".repeat(5000),
            "长".repeat(30000),
            String::new(),
            "尾".to_owned(),
        ];
        for label in &labels {
            table.text.push(label.clone());
        }
        for (i, label) in labels.iter().enumerate() {
            assert_eq!(table.text.get(i), Some(label.as_str()));
        }
        assert!(table.text.get(labels.len()).is_none());
    }

    #[test]
    fn decodes_complete_rows_and_string_items_before_the_end_of_the_entry() {
        let input = "<worksheet><sheetData><row><c><v>41</v></c></row>";
        let mut out = Loaded::empty("one", "sheet");
        let table = Strings::default();
        assert!(
            worksheet(
                input.as_bytes().chain(CutConnection),
                &table,
                &mut out,
                1,
                1
            )
            .is_err()
        );
        assert_eq!(out.sheet.value(0, 0), crate::Value::Number(41.));
        let input = "<sst><si><t>first</t></si><si/><si><t>third</t></si>";
        let mut table = Strings::default();
        assert!(string_table(input.as_bytes().chain(CutConnection), &mut table).is_err());
        assert_eq!(table.text.get(0), Some("first"));
        assert_eq!(table.text.get(1), Some(""));
        assert_eq!(table.text.get(2), Some("third"));
    }
}
