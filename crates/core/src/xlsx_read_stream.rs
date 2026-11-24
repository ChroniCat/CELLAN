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

#[cfg(target_arch = "x86_64")]
#[inline]
fn has_avx2() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static CACHED: AtomicU8 = AtomicU8::new(0);
    match CACHED.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let ok = std::arch::is_x86_feature_detected!("avx2");
            CACHED.store(if ok { 2 } else { 1 }, Ordering::Relaxed);
            ok
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn memchr_avx2(needle: u8, hay: &[u8]) -> Option<usize> {
    use std::arch::x86_64::*;
    let len = hay.len();
    let ptr = hay.as_ptr();
    let vn = _mm256_set1_epi8(needle as i8);
    let mut i = 0usize;
    while i + 32 <= len {
        let chunk = unsafe { _mm256_loadu_si256(ptr.add(i) as *const __m256i) };
        let mask = _mm256_movemask_epi8(_mm256_cmpeq_epi8(chunk, vn)) as u32;
        if mask != 0 {
            return Some(i + mask.trailing_zeros() as usize);
        }
        i += 32;
    }
    while i < len {
        if unsafe { *ptr.add(i) } == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn memchr2_avx2(n1: u8, n2: u8, hay: &[u8]) -> Option<usize> {
    use std::arch::x86_64::*;
    let len = hay.len();
    let ptr = hay.as_ptr();
    let v1 = _mm256_set1_epi8(n1 as i8);
    let v2 = _mm256_set1_epi8(n2 as i8);
    let mut i = 0usize;
    while i + 32 <= len {
        let chunk = unsafe { _mm256_loadu_si256(ptr.add(i) as *const __m256i) };
        let eq = _mm256_or_si256(_mm256_cmpeq_epi8(chunk, v1), _mm256_cmpeq_epi8(chunk, v2));
        let mask = _mm256_movemask_epi8(eq) as u32;
        if mask != 0 {
            return Some(i + mask.trailing_zeros() as usize);
        }
        i += 32;
    }
    while i < len {
        let b = unsafe { *ptr.add(i) };
        if b == n1 || b == n2 {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn memchr3_avx2(n1: u8, n2: u8, n3: u8, hay: &[u8]) -> Option<usize> {
    use std::arch::x86_64::*;
    let len = hay.len();
    let ptr = hay.as_ptr();
    let v1 = _mm256_set1_epi8(n1 as i8);
    let v2 = _mm256_set1_epi8(n2 as i8);
    let v3 = _mm256_set1_epi8(n3 as i8);
    let mut i = 0usize;
    while i + 32 <= len {
        let chunk = unsafe { _mm256_loadu_si256(ptr.add(i) as *const __m256i) };
        let eq12 = _mm256_or_si256(_mm256_cmpeq_epi8(chunk, v1), _mm256_cmpeq_epi8(chunk, v2));
        let eq = _mm256_or_si256(eq12, _mm256_cmpeq_epi8(chunk, v3));
        let mask = _mm256_movemask_epi8(eq) as u32;
        if mask != 0 {
            return Some(i + mask.trailing_zeros() as usize);
        }
        i += 32;
    }
    while i < len {
        let b = unsafe { *ptr.add(i) };
        if b == n1 || b == n2 || b == n3 {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[inline]
pub(crate) fn simd_memchr(needle: u8, hay: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if hay.len() >= 32 && has_avx2() {
            return unsafe { memchr_avx2(needle, hay) };
        }
        use std::arch::x86_64::*;
        let len = hay.len();
        let ptr = hay.as_ptr();
        let mut i = 0usize;
        if len >= 16 {
            let vn = unsafe { _mm_set1_epi8(needle as i8) };
            while i + 16 <= len {
                let chunk = unsafe { _mm_loadu_si128(ptr.add(i) as *const __m128i) };
                let mask = unsafe { _mm_movemask_epi8(_mm_cmpeq_epi8(chunk, vn)) } as u32;
                if mask != 0 {
                    return Some(i + mask.trailing_zeros() as usize);
                }
                i += 16;
            }
        }
        while i < len {
            if unsafe { *ptr.add(i) } == needle {
                return Some(i);
            }
            i += 1;
        }
        None
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        hay.iter().position(|&b| b == needle)
    }
}

#[inline]
pub(crate) fn simd_memchr2(n1: u8, n2: u8, hay: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if hay.len() >= 32 && has_avx2() {
            return unsafe { memchr2_avx2(n1, n2, hay) };
        }
        use std::arch::x86_64::*;
        let len = hay.len();
        let ptr = hay.as_ptr();
        let mut i = 0usize;
        if len >= 16 {
            let v1 = unsafe { _mm_set1_epi8(n1 as i8) };
            let v2 = unsafe { _mm_set1_epi8(n2 as i8) };
            while i + 16 <= len {
                let chunk = unsafe { _mm_loadu_si128(ptr.add(i) as *const __m128i) };
                let eq = unsafe { _mm_or_si128(_mm_cmpeq_epi8(chunk, v1), _mm_cmpeq_epi8(chunk, v2)) };
                let mask = unsafe { _mm_movemask_epi8(eq) } as u32;
                if mask != 0 {
                    return Some(i + mask.trailing_zeros() as usize);
                }
                i += 16;
            }
        }
        while i < len {
            let b = unsafe { *ptr.add(i) };
            if b == n1 || b == n2 {
                return Some(i);
            }
            i += 1;
        }
        None
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        hay.iter().position(|&b| b == n1 || b == n2)
    }
}

#[inline]
pub(crate) fn simd_memchr3(n1: u8, n2: u8, n3: u8, hay: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if hay.len() >= 32 && has_avx2() {
            return unsafe { memchr3_avx2(n1, n2, n3, hay) };
        }
        use std::arch::x86_64::*;
        let len = hay.len();
        let ptr = hay.as_ptr();
        let mut i = 0usize;
        if len >= 16 {
            let v1 = unsafe { _mm_set1_epi8(n1 as i8) };
            let v2 = unsafe { _mm_set1_epi8(n2 as i8) };
            let v3 = unsafe { _mm_set1_epi8(n3 as i8) };
            while i + 16 <= len {
                let chunk = unsafe { _mm_loadu_si128(ptr.add(i) as *const __m128i) };
                let eq12 = unsafe { _mm_or_si128(_mm_cmpeq_epi8(chunk, v1), _mm_cmpeq_epi8(chunk, v2)) };
                let eq = unsafe { _mm_or_si128(eq12, _mm_cmpeq_epi8(chunk, v3)) };
                let mask = unsafe { _mm_movemask_epi8(eq) } as u32;
                if mask != 0 {
                    return Some(i + mask.trailing_zeros() as usize);
                }
                i += 16;
            }
        }
        while i < len {
            let b = unsafe { *ptr.add(i) };
            if b == n1 || b == n2 || b == n3 {
                return Some(i);
            }
            i += 1;
        }
        None
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        hay.iter().position(|&b| b == n1 || b == n2 || b == n3)
    }
}

#[inline]
fn is_xml_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

#[inline]
fn local_tag_name(raw: &[u8]) -> &[u8] {
    match simd_memchr(b':', raw) {
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
            let Some(rel) = simd_memchr(b'>', &rest[2..]) else {
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
                if quote != 0 {
                    let Some(rel) = simd_memchr(quote, &rest[i..]) else {
                        return Ok(None);
                    };
                    i += rel + 1;
                    quote = 0;
                } else {
                    let Some(rel) = simd_memchr3(b'>', b'"', b'\'', &rest[i..]) else {
                        return Ok(None);
                    };
                    i += rel;
                    let b = rest[i];
                    if b == b'>' {
                        let empty = i >= 2 && rest[i - 1] == b'/';
                        return Ok(Some(XmlSlice::Start {
                            end: at + i + 1,
                            name_start: at + 1,
                            name_end,
                            empty,
                        }));
                    }
                    quote = b;
                    i += 1;
                }
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
        let rel = simd_memchr(first, &haystack[pos..])?;
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
        while let Some(rel) = simd_memchr(b'w', &buf[search..]) {
            let w = search + rel;
            if w >= cursor + 4 && w + 1 < buf.len() && &buf[w - 4..w + 2] == b"</row>" {
                if find_subslice(&buf[cursor..w - 4], b"<!").is_none() {
                    return Ok(Some(w + 2));
                }
                break;
            }
            search = w + 1;
        }
    } else if local_target == b"si" {
        let mut search = cursor;
        while let Some(rel) = simd_memchr(b'i', &buf[search..]) {
            let idx = search + rel;
            if idx >= cursor + 3 && idx + 1 < buf.len() && &buf[idx - 3..idx + 2] == b"</si>" {
                if find_subslice(&buf[cursor..idx - 3], b"<!").is_none() {
                    return Ok(Some(idx + 2));
                }
                break;
            }
            search = idx + 1;
        }
    }
    while cursor < buf.len() {
        let Some(rel) = simd_memchr(b'<', &buf[cursor..]) else {
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
                    if quote != 0 {
                        let Some(qrel) = simd_memchr(quote, &rest[i..]) else {
                            break;
                        };
                        i += qrel + 1;
                        quote = 0;
                    } else {
                        let Some(trel) = simd_memchr3(b'>', b'"', b'\'', &rest[i..]) else {
                            break;
                        };
                        i += trel;
                        let b = rest[i];
                        if b == b'>' {
                            closed_tag = Some(at + i + 1);
                            break;
                        }
                        quote = b;
                        i += 1;
                    }
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
        let Some(rel) = simd_memchr(b'<', &buf[cursor..]) else {
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

const SST_BATCH_BYTES: usize = 1024 * 1024;
const SST_PARALLEL_MIN_ITEMS: usize = 2048;

#[inline]
fn empty_shared_arc() -> &'static std::sync::Arc<str> {
    static EMPTY: std::sync::OnceLock<std::sync::Arc<str>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| std::sync::Arc::from(""))
}

/// Fast decode for standard `<si><t>...</t></si>` and `<si><t xml:space="preserve">...</t></si>`
/// items that contain no nested XML tags, entities (`&`), or CR (`\r`).
#[inline]
fn try_fast_si_arc(
    si_bytes: &[u8],
    empty_arc: &std::sync::Arc<str>,
) -> Result<Option<std::sync::Arc<str>>> {
    if si_bytes == b"<si/>" || si_bytes == b"<si></si>" {
        return Ok(Some(std::sync::Arc::clone(empty_arc)));
    }
    let Some(body) = si_bytes
        .strip_prefix(b"<si>")
        .and_then(|b| b.strip_suffix(b"</si>"))
    else {
        return Ok(None);
    };
    let body = trim_ascii_ws(body);
    if body == b"<t/>" || body == b"<t></t>" {
        return Ok(Some(std::sync::Arc::clone(empty_arc)));
    }
    let (inner, preserve) = if let Some(rest) = body.strip_prefix(b"<t>") {
        let Some(inner) = rest.strip_suffix(b"</t>") else {
            return Ok(None);
        };
        (inner, false)
    } else if let Some(rest) = body.strip_prefix(b"<t xml:space=\"preserve\">") {
        let Some(inner) = rest.strip_suffix(b"</t>") else {
            return Ok(None);
        };
        (inner, true)
    } else if let Some(rest) = body.strip_prefix(b"<t xml:space='preserve'>") {
        let Some(inner) = rest.strip_suffix(b"</t>") else {
            return Ok(None);
        };
        (inner, true)
    } else {
        return Ok(None);
    };
    if simd_memchr3(b'<', b'&', b'\r', inner).is_some() {
        return Ok(None);
    }
    let raw = std::str::from_utf8(inner)?;
    let trimmed = if preserve {
        raw
    } else {
        raw.trim_matches([' ', '\t', '\r', '\n'])
    };
    if trimmed.is_empty() {
        return Ok(Some(std::sync::Arc::clone(empty_arc)));
    }
    let decoded = office_text(trimmed);
    Ok(Some(std::sync::Arc::from(decoded.as_ref())))
}

fn decode_si_slice(
    si_bytes: &[u8],
    empty_arc: &std::sync::Arc<str>,
) -> Result<(std::sync::Arc<str>, Vec<crate::richtext::RichRun>)> {
    if let Some(arc) = try_fast_si_arc(si_bytes, empty_arc)? {
        return Ok((arc, Vec::new()));
    }
    let xml = std::str::from_utf8(si_bytes)?;
    let mut one = Reader::from_str(xml);
    let (text, runs) = match one.read_event()? {
        Event::Start(_) => string_item(&mut one)?,
        Event::Empty(_) => Default::default(),
        _ => anyhow::bail!("无效共享字符串条目"),
    };
    let arc = if text.is_empty() {
        std::sync::Arc::clone(empty_arc)
    } else {
        std::sync::Arc::from(text)
    };
    Ok((arc, runs))
}

fn flush_si_batch(
    buf: &[u8],
    si_ranges: &mut Vec<(usize, usize)>,
    table: &mut Strings,
    empty_arc: &std::sync::Arc<str>,
) -> Result<()> {
    if si_ranges.is_empty() {
        return Ok(());
    }
    let count = si_ranges.len();
    let workers = if count >= SST_PARALLEL_MIN_ITEMS {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(8)
    } else {
        1
    };
    if workers <= 1 {
        table.text.reserve(count);
        for &(start, end) in si_ranges.iter() {
            let (arc, runs) = decode_si_slice(&buf[start..end], empty_arc)?;
            if !runs.is_empty() {
                table.rich.insert(table.text.len(), runs);
            }
            table.text.push_arc(arc);
        }
        si_ranges.clear();
        return Ok(());
    }

    let chunk_len = count.div_ceil(workers);
    let base_idx = table.text.len();
    type PartOut = Result<(
        Vec<std::sync::Arc<str>>,
        Vec<(usize, Vec<crate::richtext::RichRun>)>,
    )>;
    let parts: Vec<PartOut> = std::thread::scope(|pool| {
        let mut handles = Vec::with_capacity(workers);
        for (chunk_idx, sub) in si_ranges.chunks(chunk_len).enumerate() {
            let offset = base_idx + chunk_idx * chunk_len;
            handles.push(pool.spawn(move || -> PartOut {
                let mut arcs = Vec::with_capacity(sub.len());
                let mut rich = Vec::new();
                for (local_i, &(start, end)) in sub.iter().enumerate() {
                    let (arc, runs) = decode_si_slice(&buf[start..end], empty_arc)?;
                    if !runs.is_empty() {
                        rich.push((offset + local_i, runs));
                    }
                    arcs.push(arc);
                }
                Ok((arcs, rich))
            }));
        }
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .map_err(|_| anyhow::anyhow!("sharedStrings 并行解码线程异常退出"))?
            })
            .collect()
    });
    si_ranges.clear();
    table.text.reserve(count);
    for part in parts {
        let (arcs, rich) = part?;
        table.text.extend_arcs(arcs);
        for (idx, runs) in rich {
            table.rich.insert(idx, runs);
        }
    }
    Ok(())
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
    pending_err: Option<std::io::Error>,
    scratch: Vec<u8>,
}

impl<R: Read> StreamBuf<R> {
    fn new(input: R) -> Self {
        Self {
            input,
            buf: Vec::with_capacity(READ_BUF_BYTES * 2),
            pos: 0,
            eof: false,
            pending_err: None,
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
        if let Some(err) = self.pending_err.take() {
            return Err(err.into());
        }
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

    /// Prefetch up to `target_rem` unread bytes without invalidating existing offsets unless
    /// `self.pos > 0` has already been flushed; defers any `io::Error` until buffered items
    /// have been consumed.
    fn prefetch_window(&mut self, target_rem: usize) {
        if self.eof || self.pending_err.is_some() {
            return;
        }
        self.compact();
        while self.rem().len() < target_rem {
            match self.input.read(&mut self.scratch) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(n) => {
                    self.buf.extend_from_slice(&self.scratch[..n]);
                }
                Err(e) => {
                    self.pending_err = Some(e);
                    break;
                }
            }
        }
    }

    #[inline]
    fn rem(&self) -> &[u8] {
        &self.buf[self.pos..]
    }
}

pub(super) fn string_table(input: impl Read, table: &mut Strings) -> Result<()> {
    let mut sb = StreamBuf::new(input);
    let (mut opened, mut closed) = (false, false);
    let empty_arc = empty_shared_arc();
    let mut si_ranges: Vec<(usize, usize)> = Vec::new();

    loop {
        if opened && !closed && si_ranges.is_empty() && sb.rem().len() < READ_BUF_BYTES {
            sb.prefetch_window(SST_BATCH_BYTES);
        }
        let rel = loop {
            if let Some(p) = simd_memchr(b'<', sb.rem()) {
                break Some(p);
            }
            let rem_str = std::str::from_utf8(sb.rem())?;
            ensure!(!closed || rem_str.trim().is_empty(), "sharedStrings.xml 根元素外存在内容");
            flush_si_batch(&sb.buf, &mut si_ranges, table, empty_arc)?;
            sb.pos = sb.buf.len();
            if !sb.fill_more()? {
                break None;
            }
            if opened && !closed && sb.rem().len() < READ_BUF_BYTES {
                sb.prefetch_window(SST_BATCH_BYTES);
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
            flush_si_batch(&sb.buf, &mut si_ranges, table, empty_arc)?;
            ensure!(sb.fill_more()?, "sharedStrings.xml 标签未闭合");
        };
        match tok {
            XmlSlice::DeclOrPi { end } | XmlSlice::Comment { end } => {
                let _ = std::str::from_utf8(&sb.buf[sb.pos..end])?;
                sb.pos = end;
            }
            XmlSlice::CData { .. } => anyhow::bail!("无效共享字符串 CDATA"),
            XmlSlice::Start { end, name_start, name_end, empty } => {
                if opened && !closed && local_tag_name(&sb.buf[name_start..name_end]) == b"si" {
                    let rel_end = end - sb.pos;
                    let rel_item_end = if empty {
                        rel_end
                    } else {
                        let mut rel_search = rel_end;
                        loop {
                            if let Some(close_end) =
                                find_matching_close(&sb.buf, sb.pos + rel_search, b"si")?
                            {
                                break close_end - sb.pos;
                            }
                            flush_si_batch(&sb.buf, &mut si_ranges, table, empty_arc)?;
                            rel_search = sb.rem().len().saturating_sub(8).max(rel_end);
                            ensure!(sb.fill_more()?, "sharedStrings.xml 条目未闭合");
                            sb.prefetch_window(SST_BATCH_BYTES);
                        }
                    };
                    let item_end = sb.pos + rel_item_end;
                    si_ranges.push((sb.pos, item_end));
                    sb.pos = item_end;
                    continue;
                }
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
                    flush_si_batch(&sb.buf, &mut si_ranges, table, empty_arc)?;
                    let rel_end = end - sb.pos;
                    let rel_item_end = if empty {
                        rel_end
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
                    let _ = std::str::from_utf8(&sb.buf[sb.pos..item_end])?;
                    sb.pos = item_end;
                }
            }
            XmlSlice::End { end, name_start, name_end } => {
                ensure!(opened && !closed, "sharedStrings.xml 结束标签无效");
                ensure!(local_tag_name(&sb.buf[name_start..name_end]) == b"sst", "sharedStrings.xml 根结束标签不匹配");
                flush_si_batch(&sb.buf, &mut si_ranges, table, empty_arc)?;
                closed = true;
                sb.pos = end;
            }
        }
    }
    flush_si_batch(&sb.buf, &mut si_ranges, table, empty_arc)?;
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
    all_rows_clean: bool,
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
pub(crate) fn fast_parse_f64_bytes(b: &[u8]) -> Option<f64> {
    let b = trim_ascii_ws(b);
    if !b.is_empty() && b.len() <= 24 {
        let (neg, rest) = match b[0] {
            b'-' => (true, &b[1..]),
            b'+' => (false, &b[1..]),
            _ => (false, b),
        };
        if !rest.is_empty() {
            let mut mantissa: u64 = 0;
            let mut sig_digits: u32 = 0;
            let mut scale: usize = 0;
            let mut seen_dot = false;
            let mut any_digit = false;
            let mut fast = true;
            for &ch in rest {
                let d = ch.wrapping_sub(b'0');
                if d <= 9 {
                    any_digit = true;
                    if mantissa == 0 && d == 0 {
                        if seen_dot {
                            scale += 1;
                        }
                    } else {
                        sig_digits += 1;
                        if sig_digits > 15 {
                            fast = false;
                            break;
                        }
                        mantissa = mantissa * 10 + u64::from(d);
                        if seen_dot {
                            scale += 1;
                        }
                    }
                } else if ch == b'.' && !seen_dot {
                    seen_dot = true;
                } else {
                    fast = false;
                    break;
                }
            }
            if fast && any_digit && scale <= 22 {
                static POW10: [f64; 23] = [
                    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11,
                    1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
                ];
                let val = if scale == 0 {
                    mantissa as f64
                } else if mantissa == 0 {
                    0.0
                } else {
                    (mantissa as f64) / POW10[scale]
                };
                return Some(if neg { -val } else { val });
            }
        }
    }
    std::str::from_utf8(b).ok()?.parse::<f64>().ok()
}

#[inline]
fn fast_normalize_formula_arc(raw: &str) -> std::sync::Arc<str> {
    let body = raw.trim_start_matches('=');
    if body.len() < 255 {
        let mut stack = [0u8; 256];
        stack[0] = b'=';
        stack[1..1 + body.len()].copy_from_slice(body.as_bytes());
        let s = unsafe { std::str::from_utf8_unchecked(&stack[..1 + body.len()]) };
        crate::normalize_formula_cow(s).as_ref().into()
    } else {
        let mut with_eq = String::with_capacity(body.len() + 1);
        with_eq.push('=');
        with_eq.push_str(body);
        crate::normalize_formula_cow(&with_eq).as_ref().into()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FastRow {
    Ok,
    FallbackRow,
    FallbackChunk,
}

/// Fast raw-byte decoder for an individual `<row ...>...</row>` (including real Excel attributes
/// like `spans="1:20"` / `x14ac:dyDescent="0.25"` and plain `<f>...</f>` formulas). Returns
/// `FallbackRow` on complex cells (shared/array formulas, entities, inline strings, rich SST)
/// so only that row uses `quick-xml`, or `FallbackChunk` if row numbering is implicit.
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
                ht = fast_parse_f64_bytes(val).map(|v| v as f32).unwrap_or(0.0);
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
            b"collapsed" | b"thickTop" | b"thickBot" | b"ph" => {
                return FastRow::FallbackRow;
            }
            _ => {}
        }
    }

    let Some(r_idx) = row_idx else {
        return FastRow::FallbackChunk;
    };
    if ht > 0.0 && !custom_ht {
        return FastRow::FallbackRow;
    }
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
                    if !matches!(val, b"" | b"n" | b"s" | b"b" | b"str" | b"e") {
                        return FastRow::FallbackRow;
                    }
                    typ = val;
                }
                b"cm" | b"vm" | b"ph" => return FastRow::FallbackRow,
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

        let mut formula: Option<std::sync::Arc<str>> = None;
        if body[p..].starts_with(b"<f/>") {
            p += 4;
            while p < body.len() && is_xml_ws(body[p]) {
                p += 1;
            }
        } else if body[p..].starts_with(b"<f>") {
            p += 3;
            let f_start = p;
            let Some(f_lt) = simd_memchr(b'<', &body[p..]) else {
                return FastRow::FallbackRow;
            };
            let f_end = p + f_lt;
            if !body[f_end..].starts_with(b"</f>") {
                return FastRow::FallbackRow;
            }
            let f_bytes = &body[f_start..f_end];
            if simd_memchr2(b'&', b'\r', f_bytes).is_some() {
                return FastRow::FallbackRow;
            }
            p = f_end + 4;
            if !f_bytes.is_empty() {
                let Ok(f_str) = std::str::from_utf8(f_bytes) else {
                    return FastRow::FallbackRow;
                };
                formula = Some(fast_normalize_formula_arc(f_str));
            }
            while p < body.len() && is_xml_ws(body[p]) {
                p += 1;
            }
        }

        if body[p..].starts_with(b"</c>") {
            p += 4;
            if let Some(formula) = formula {
                cells.push((
                    (cr, cc),
                    crate::model::Cell {
                        value: crate::model::Value::Empty,
                        formula: Some(formula),
                    },
                ));
            }
            continue;
        }

        let v_bytes: &[u8] = if body[p..].starts_with(b"<v/>") {
            p += 4;
            b""
        } else if body[p..].starts_with(b"<v>") {
            p += 3;
            let v_start = p;
            let Some(lt_rel) = simd_memchr(b'<', &body[p..]) else {
                return FastRow::FallbackRow;
            };
            let v_end = p + lt_rel;
            if !body[v_end..].starts_with(b"</v>") {
                return FastRow::FallbackRow;
            }
            let slice = &body[v_start..v_end];
            if simd_memchr2(b'&', b'\r', slice).is_some() {
                return FastRow::FallbackRow;
            }
            p = v_end + 4;
            slice
        } else {
            return FastRow::FallbackRow;
        };

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
            b"str" => {
                let Ok(s) = std::str::from_utf8(v_bytes) else {
                    return FastRow::FallbackRow;
                };
                crate::model::Value::Text(office_text(s).as_ref().into())
            }
            b"e" if !v_bytes.is_empty() => match v_bytes {
                b"#N/A" => crate::model::Value::Error(crate::formula::ErrKind::Na),
                b"#VALUE!" => crate::model::Value::Error(crate::formula::ErrKind::Value),
                b"#REF!" => crate::model::Value::Error(crate::formula::ErrKind::Ref),
                b"#DIV/0!" => crate::model::Value::Error(crate::formula::ErrKind::Div0),
                b"#NUM!" => crate::model::Value::Error(crate::formula::ErrKind::Num),
                b"#NAME?" => crate::model::Value::Error(crate::formula::ErrKind::Name),
                b"#NULL!" => crate::model::Value::Error(crate::formula::ErrKind::Null),
                b"#SPILL!" => crate::model::Value::Error(crate::formula::ErrKind::Spill),
                _ => return FastRow::FallbackRow,
            },
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
        let cell = crate::model::Cell { value, formula };
        if !cell.is_blank() {
            cells.push(((cr, cc), cell));
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

        // Try fast raw-byte row decoding whenever `style_lookup` is active (including real Excel
        // rows with `spans="1:20"`, `x14ac:dyDescent="..."`, or plain `<f>...</f>` formulas).
        // Any individual row containing shared/array formulas, entities, inline strings, rich SST
        // items, or implicit cell coordinates falls back seamlessly to `parse_section`.
        let can_fast = style_lookup.is_some();
        let mut used_fast = false;
        let mut had_fallback_row = false;
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
                // Find end of this `<row ...>` open tag with quote awareness.
                let open_slice = &bytes[pos..];
                let mut ti = 4usize;
                let mut quote = 0u8;
                let mut open_gt_opt = None;
                while ti < open_slice.len() {
                    if quote != 0 {
                        let Some(qrel) = simd_memchr(quote, &open_slice[ti..]) else {
                            break;
                        };
                        ti += qrel + 1;
                        quote = 0;
                    } else {
                        let Some(trel) = simd_memchr3(b'>', b'"', b'\'', &open_slice[ti..]) else {
                            break;
                        };
                        ti += trel;
                        let b = open_slice[ti];
                        if b == b'>' {
                            open_gt_opt = Some(ti);
                            break;
                        }
                        quote = b;
                        ti += 1;
                    }
                }
                let Some(open_gt) = open_gt_opt else {
                    fast_ok = false;
                    break;
                };
                let open_end = pos + open_gt + 1;
                let row_end = if open_gt >= 1 && bytes[pos + open_gt - 1] == b'/' {
                    open_end
                } else {
                    let Ok(Some(e)) = find_matching_close(bytes, open_end, b"row") else {
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
                        had_fallback_row = true;
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
            all_rows_clean: used_fast && !had_fallback_row,
        })
    }

    fn append(self, out: &mut Loaded, formulas: &mut Formulas, shell: &mut String) -> Result<()> {
        out.all_rows_clean &= self.all_rows_clean;
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
            if let Some(p) = simd_memchr(b'<', sb.rem()) {
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

fn collapse_empty_sheet_data(shell: &str) -> Option<String> {
    let trimmed = shell.trim_start_matches('\u{feff}').trim_start();
    let after_decl = if let Some(rest) = trimmed.strip_prefix("<?xml") {
        let end_decl = rest.find("?>")?;
        rest[end_decl + 2..].trim_start()
    } else {
        trimmed
    };
    if !after_decl.starts_with("<worksheet")
        || !after_decl
            .as_bytes()
            .get("<worksheet".len())
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>')
    {
        return None;
    }
    if shell.contains("<sheetData/>") {
        return Some(shell.to_owned());
    }
    let sd_pos = shell.find("<sheetData")?;
    let after_sd = &shell[sd_pos + "<sheetData".len()..];
    let gt_rel = after_sd.find('>')?;
    let tag_attrs = &after_sd[..gt_rel];
    if tag_attrs.trim() == "/" {
        let mut out = String::with_capacity(shell.len());
        out.push_str(&shell[..sd_pos]);
        out.push_str("<sheetData/>");
        out.push_str(&after_sd[gt_rel + 1..]);
        return Some(out);
    }
    if !tag_attrs.trim().is_empty() {
        return None;
    }
    let inner_start = sd_pos + "<sheetData".len() + gt_rel + 1;
    let close_pos = shell.rfind("</sheetData>")?;
    if close_pos < inner_start {
        return None;
    }
    if !shell.as_bytes()[inner_start..close_pos]
        .iter()
        .all(|&b| is_xml_ws(b))
    {
        return None;
    }
    let footer_start = close_pos + "</sheetData>".len();
    let mut out = String::with_capacity(sd_pos + 12 + (shell.len() - footer_start));
    out.push_str(&shell[..sd_pos]);
    out.push_str("<sheetData/>");
    out.push_str(&shell[footer_start..]);
    Some(out)
}

#[cfg(test)]
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
        if out.all_rows_clean && shared.masters.is_empty() && shared.children.is_empty() {
            out.clean_shell = collapse_empty_sheet_data(&shell).map(std::sync::Arc::from);
        }
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

    if out.all_rows_clean && shared.masters.is_empty() && shared.children.is_empty() {
        out.clean_shell = collapse_empty_sheet_data(&shell).map(std::sync::Arc::from);
    }
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

    #[test]
    fn fast_parse_f64_bytes_matches_std_parse_bit_for_bit() {
        let samples = [
            "0",
            "-0",
            "0.0",
            "1",
            "-42",
            "0.1",
            "0.2",
            "0.3",
            "123456.789",
            "-987654321.125",
            "9007199254740991",
            "0.000000125",
            "3.141592653589793",
            "1.7976931348623157e308",
            "5e-324",
            "  25.5  ",
        ];
        for s in samples {
            let want = s.trim().parse::<f64>().unwrap();
            let got = fast_parse_f64_bytes(s.as_bytes()).unwrap();
            assert_eq!(got.to_bits(), want.to_bits(), "sample={s}");
        }
        assert!(fast_parse_f64_bytes(b"").is_none());
        assert!(fast_parse_f64_bytes(b"12a").is_none());
    }

    #[test]
    fn real_excel_dy_descent_cross_sheet_formula_and_parallel_sst_fast_path() {
        // 1. Parallel SST batch (>= 2048 <si> items)
        let mut sst_xml = String::from("<sst count=\"2500\" uniqueCount=\"2500\">");
        for i in 0..2500 {
            if i % 10 == 0 {
                sst_xml.push_str(&format!("<si><t>富文本 &amp; {i}</t></si>"));
            } else {
                sst_xml.push_str(&format!("<si><t>item_{i}</t></si>"));
            }
        }
        sst_xml.push_str("</sst>");
        let mut table = Strings::default();
        string_table(sst_xml.as_bytes(), &mut table).unwrap();
        assert_eq!(table.text.get(0), Some("富文本 & 0"));
        assert_eq!(table.text.get(2499), Some("item_2499"));

        // 2. Real Excel <row spans="..." x14ac:dyDescent="0.25"> + cross-sheet `!`, `&`, `\r\n`
        let ws_xml = concat!(
            "<worksheet xmlns:x14ac=\"http://schemas.microsoft.com/office/spreadsheetml/2009/9/ac\">\r\n",
            "<sheetData>\r\n",
            "<row r=\"1\" spans=\"1:4\" x14ac:dyDescent=\"0.25\">",
            "<c r=\"A1\" t=\"s\"><v>0</v></c>",
            "<c r=\"B1\"><f>Sheet2!A1+1.5</f><v>42.5</v></c>",
            "<c r=\"C1\" t=\"str\"><f>\"A\"&amp;\"B\"</f><v>AB</v></c>",
            "<c r=\"D1\" t=\"e\"><v>#DIV/0!</v></c>",
            "</row>\r\n",
            "</sheetData></worksheet>"
        );
        let mut out = Loaded::empty("Sheet1", "xl/worksheets/sheet1.xml");
        worksheet(ws_xml.as_bytes(), &table, &mut out, 1, 1).unwrap();
        assert_eq!(out.sheet.value(0, 0), crate::Value::Text("富文本 & 0".into()));
        assert_eq!(out.sheet.value(0, 1), crate::Value::Number(42.5));
        assert_eq!(
            out.sheet.get(0, 1).unwrap().formula.as_deref(),
            Some("=Sheet2!A1+1.5")
        );
        assert_eq!(out.sheet.value(0, 2), crate::Value::Text("AB".into()));
        assert_eq!(
            out.sheet.get(0, 2).unwrap().formula.as_deref(),
            Some("=\"A\"&\"B\"")
        );
        assert_eq!(
            out.sheet.value(0, 3),
            crate::Value::Error(crate::formula::ErrKind::Div0)
        );
    }
}
