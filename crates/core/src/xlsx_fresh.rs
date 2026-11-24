//! Stream ordinary large worksheets into the sparse sheet parts of a newly
//! generated package. umya and the existing patches still own its metadata.
use crate::model::{Cell, CellRef, Sheet, Value, Workbook};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub(super) fn supported(sheet: &Sheet) -> bool {
    // A saved dynamic result needs its cell metadata and persisted spill caches
    // even when the workbook has only one formula.
    if (sheet.len() < 16_384 && sheet.spill_shapes.is_empty())
        || sheet.charts.iter().any(|c| !c.part.is_empty())
        || sheet.pictures.iter().any(|p| p.data.is_none())
    {
        return false;
    }
    sheet.rich.iter().all(|(&(r, c), runs)| {
        runs.is_empty()
            || sheet
                .get(r, c)
                .is_some_and(|cell| super::delta::supports_runs(&cell, runs))
    }) && sheet.cells().iter_pages().all(|(pi, page)| {
        let base = pi * crate::store::PAGE_ROWS;
        page.cols.iter_entries().all(|(col, slab)| {
            if col >= crate::sync::MAX_COLS {
                return false;
            }
            match slab.as_ref() {
                crate::store::ColSlab::Numbers(ns) => {
                    base + (ns.values.len().saturating_sub(1) as u32) < crate::sync::MAX_ROWS
                        && ns.values.iter().all(|n| n.is_finite())
                }
                crate::store::ColSlab::Values(vs) => vs.iter().enumerate().all(|(r, v)| {
                    if v.is_empty() {
                        return true;
                    }
                    let row = base + r as u32;
                    row < crate::sync::MAX_ROWS
                        && match v {
                            Value::Number(n) => n.is_finite(),
                            Value::Text(s) => super::delta::safe_xml_text(s),
                            Value::Error(crate::formula::ErrKind::Spill) => {
                                sheet.spill_shapes.contains_key(&CellRef::new(row, col))
                            }
                            _ => true,
                        }
                }),
                crate::store::ColSlab::Mixed(cs) => cs.iter().enumerate().all(|(r, cell)| {
                    if cell.is_blank() {
                        return true;
                    }
                    let row = base + r as u32;
                    row < crate::sync::MAX_ROWS
                        && cell
                            .formula
                            .as_ref()
                            .is_none_or(|f| super::delta::safe_xml_text(f))
                        && match &cell.value {
                            Value::Number(n) => n.is_finite(),
                            Value::Text(s) => super::delta::safe_xml_text(s),
                            Value::Error(crate::formula::ErrKind::Spill) => {
                                sheet.spill_shapes.contains_key(&CellRef::new(row, col))
                            }
                            _ => true,
                        }
                }),
            }
        })
    })
}

const SST_PART: &str = "xl/sharedStrings.xml";
const STYLES_PART: &str = "xl/styles.xml";
const SST_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings";
const SST_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml";

pub(super) struct FreshResult {
    pub(super) styles: Option<super::delta::SavedStyles>,
    pub(super) trusted_parts: HashMap<String, (u32, u64)>,
}

/// Standard zlib GF(2) CRC-32 combination (`crc32(A || B)` from `crc32(A)`, `crc32(B)`, `B.len()`).
pub(super) fn crc32_combine(mut crc1: u32, crc2: u32, mut len2: u64) -> u32 {
    if len2 == 0 {
        return crc1;
    }
    #[inline]
    fn gf2_matrix_times(mat: &[u32; 32], mut vec: u32) -> u32 {
        let mut sum = 0u32;
        let mut i = 0usize;
        while vec != 0 {
            if (vec & 1) != 0 {
                sum ^= mat[i];
            }
            vec >>= 1;
            i += 1;
        }
        sum
    }
    #[inline]
    fn gf2_matrix_square(square: &mut [u32; 32], mat: &[u32; 32]) {
        for n in 0..32 {
            square[n] = gf2_matrix_times(mat, mat[n]);
        }
    }
    let mut odd = [0u32; 32];
    let mut even = [0u32; 32];
    odd[0] = 0xedb8_8320;
    let mut row = 1u32;
    for n in 1..32 {
        odd[n] = row;
        row <<= 1;
    }
    gf2_matrix_square(&mut even, &odd);
    gf2_matrix_square(&mut odd, &even);
    loop {
        gf2_matrix_square(&mut even, &odd);
        if (len2 & 1) != 0 {
            crc1 = gf2_matrix_times(&even, crc1);
        }
        len2 >>= 1;
        if len2 == 0 {
            break;
        }
        gf2_matrix_square(&mut odd, &even);
        if (len2 & 1) != 0 {
            crc1 = gf2_matrix_times(&odd, crc1);
        }
        len2 >>= 1;
        if len2 == 0 {
            break;
        }
    }
    crc1 ^ crc2
}

/// Compress a raw RFC 1951 deflate chunk (no zlib header) and compute its CRC-32.
/// Non-final chunks end with `FlushCompress::Sync` (byte-aligned); the last chunk ends with `FlushCompress::Finish`.
pub(super) fn deflate_raw_chunk(data: &[u8], level: u32, is_last: bool) -> Result<(Vec<u8>, u32)> {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    let mut comp = flate2::Compress::new(flate2::Compression::new(level), false);
    let mut out = Vec::with_capacity((data.len() / 4).max(1024));
    let mut consumed = 0usize;
    while consumed < data.len() {
        if out.len() == out.capacity() {
            out.reserve((data.len() - consumed).clamp(16 * 1024, 512 * 1024));
        }
        let before_in = comp.total_in();
        let status = comp.compress_vec(&data[consumed..], &mut out, flate2::FlushCompress::None)?;
        consumed += (comp.total_in() - before_in) as usize;
        if status == flate2::Status::BufError && out.len() == out.capacity() {
            out.reserve(64 * 1024);
        }
    }
    let flush = if is_last {
        flate2::FlushCompress::Finish
    } else {
        flate2::FlushCompress::Sync
    };
    loop {
        if out.capacity() - out.len() < 16 * 1024 {
            out.reserve(64 * 1024);
        }
        let before_out = out.len();
        let status = comp.compress_vec(&[], &mut out, flush)?;
        if is_last {
            if status == flate2::Status::StreamEnd {
                break;
            }
        } else if status == flate2::Status::Ok && out.len() < out.capacity() {
            break;
        }
        if status == flate2::Status::BufError && out.len() == before_out {
            out.reserve(64 * 1024);
        }
    }
    Ok((out, crc.sum()))
}

/// Build an in-memory standard single-entry ZIP wrapping pre-compressed RFC 1951 deflate chunks
/// so `ZipWriter::raw_copy_file` can splice them without temp files or recompression.
pub(super) fn build_single_entry_zip(
    name: &str,
    crc32: u32,
    uncompressed_size: u64,
    compressed_chunks: &[Vec<u8>],
) -> Result<Vec<u8>> {
    let comp_total: u64 = compressed_chunks.iter().map(|c| c.len() as u64).sum();
    let uncomp_u32 = u32::try_from(uncompressed_size).context("ZIP 条目未压缩大小超过 4GB")?;
    let comp_u32 = u32::try_from(comp_total).context("ZIP 条目压缩大小超过 4GB")?;
    let name_bytes = name.as_bytes();
    let name_len = u16::try_from(name_bytes.len()).context("ZIP 条目名过长")?;
    let flags: u16 = if name.is_ascii() { 0 } else { 1 << 11 };
    let cd_offset = u32::try_from(30 + name_bytes.len() as u64 + comp_total)
        .context("ZIP 中央目录偏移超过 4GB")?;
    let cd_size = (46 + name_bytes.len()) as u32;

    let mut buf = Vec::with_capacity(98 + name_bytes.len() * 2 + comp_u32 as usize);
    buf.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&flags.to_le_bytes());
    buf.extend_from_slice(&8u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0x0021u16.to_le_bytes());
    buf.extend_from_slice(&crc32.to_le_bytes());
    buf.extend_from_slice(&comp_u32.to_le_bytes());
    buf.extend_from_slice(&uncomp_u32.to_le_bytes());
    buf.extend_from_slice(&name_len.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(name_bytes);
    for c in compressed_chunks {
        buf.extend_from_slice(c);
    }
    buf.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&flags.to_le_bytes());
    buf.extend_from_slice(&8u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0x0021u16.to_le_bytes());
    buf.extend_from_slice(&crc32.to_le_bytes());
    buf.extend_from_slice(&comp_u32.to_le_bytes());
    buf.extend_from_slice(&uncomp_u32.to_le_bytes());
    buf.extend_from_slice(&name_len.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(name_bytes);
    buf.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&cd_size.to_le_bytes());
    buf.extend_from_slice(&cd_offset.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    Ok(buf)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum XmlMode {
    Text,
    TagStart,
    OpenTagName,
    InOpenTag,
    CloseTagName,
    AttrQuote(u8),
    SlashClose,
    Bang,
    CommentDash1,
    Comment,
    CommentEnd1,
    CommentEnd2,
    CdataPrefix(u8),
    Cdata,
    CdataEnd1,
    CdataEnd2,
    Pi,
    PiEnd,
}

/// Lightweight streaming XML well-formedness & UTF-8 checker used during write-out so
/// post-save verification can trust the freshly written `(crc32, uncompressed_size)` without
/// single-core re-decompression and XML re-parsing of 100+ MB worksheets.
pub(super) struct XmlCheckState {
    mode: XmlMode,
    fragment: bool,
    depth: i32,
    min_depth: i32,
    roots: u32,
    closed: bool,
    root_local: [u8; 32],
    root_local_len: usize,
    capturing_root: bool,
    started: bool,
    utf8_tail: [u8; 4],
    utf8_tail_len: usize,
    failed: bool,
}

impl XmlCheckState {
    pub(super) fn new_document() -> Self {
        Self {
            mode: XmlMode::Text,
            fragment: false,
            depth: 0,
            min_depth: 0,
            roots: 0,
            closed: false,
            root_local: [0; 32],
            root_local_len: 0,
            capturing_root: false,
            started: false,
            utf8_tail: [0; 4],
            utf8_tail_len: 0,
            failed: false,
        }
    }

    pub(super) fn new_fragment() -> Self {
        let mut s = Self::new_document();
        s.fragment = true;
        s.started = true;
        s
    }

    fn feed_utf8(&mut self, mut bytes: &[u8]) {
        if self.failed || bytes.is_empty() {
            return;
        }
        if self.utf8_tail_len > 0 {
            let need = (4 - self.utf8_tail_len).min(bytes.len());
            self.utf8_tail[self.utf8_tail_len..self.utf8_tail_len + need]
                .copy_from_slice(&bytes[..need]);
            let combined_len = self.utf8_tail_len + need;
            match std::str::from_utf8(&self.utf8_tail[..combined_len]) {
                Ok(_) => {
                    self.utf8_tail_len = 0;
                    bytes = &bytes[need..];
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid >= self.utf8_tail_len {
                        let used_from_bytes = valid - self.utf8_tail_len;
                        self.utf8_tail_len = 0;
                        bytes = &bytes[used_from_bytes..];
                    } else if e.error_len().is_some() {
                        self.failed = true;
                        return;
                    } else {
                        self.utf8_tail_len = combined_len;
                        return;
                    }
                }
            }
        }
        match std::str::from_utf8(bytes) {
            Ok(_) => {}
            Err(e) => {
                if e.error_len().is_some() {
                    self.failed = true;
                } else {
                    let rem = &bytes[e.valid_up_to()..];
                    self.utf8_tail[..rem.len()].copy_from_slice(rem);
                    self.utf8_tail_len = rem.len();
                }
            }
        }
    }

    pub(super) fn feed(&mut self, mut bytes: &[u8]) {
        if self.failed || bytes.is_empty() {
            return;
        }
        self.feed_utf8(bytes);
        if self.failed {
            return;
        }
        if !self.started {
            self.started = true;
            if bytes.starts_with(b"\xef\xbb\xbf") {
                bytes = &bytes[3..];
            }
        }
        let mut i = 0usize;
        let len = bytes.len();
        while i < len {
            match self.mode {
                XmlMode::Text => {
                    let next = bytes[i..].iter().position(|&b| b == b'<');
                    let end = next.map_or(len, |p| i + p);
                    if !self.fragment && self.depth == 0 {
                        if bytes[i..end]
                            .iter()
                            .any(|&b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
                        {
                            self.failed = true;
                            return;
                        }
                    }
                    if next.is_none() {
                        break;
                    }
                    i = end + 1;
                    self.mode = XmlMode::TagStart;
                }
                XmlMode::TagStart => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        b'/' => self.mode = XmlMode::CloseTagName,
                        b'?' => self.mode = XmlMode::Pi,
                        b'!' => self.mode = XmlMode::Bang,
                        b'<' | b'>' | b' ' | b'\t' | b'\r' | b'\n' => {
                            self.failed = true;
                            return;
                        }
                        _ => {
                            if !self.fragment && self.closed {
                                self.failed = true;
                                return;
                            }
                            self.capturing_root = !self.fragment && self.depth == 0;
                            if self.capturing_root {
                                self.root_local_len = 0;
                                if b != b':' {
                                    self.root_local[0] = b;
                                    self.root_local_len = 1;
                                }
                            }
                            self.mode = XmlMode::OpenTagName;
                        }
                    }
                }
                XmlMode::OpenTagName => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        b'>' => {
                            if !self.fragment && self.depth == 0 {
                                self.roots += 1;
                            }
                            self.depth += 1;
                            self.capturing_root = false;
                            self.mode = XmlMode::Text;
                        }
                        b'/' => {
                            self.capturing_root = false;
                            self.mode = XmlMode::SlashClose;
                        }
                        b' ' | b'\t' | b'\r' | b'\n' => {
                            self.capturing_root = false;
                            self.mode = XmlMode::InOpenTag;
                        }
                        b'<' | b'"' | b'\'' => {
                            self.failed = true;
                            return;
                        }
                        b':' if self.capturing_root => {
                            self.root_local_len = 0;
                        }
                        _ => {
                            if self.capturing_root && self.root_local_len < self.root_local.len() {
                                self.root_local[self.root_local_len] = b;
                                self.root_local_len += 1;
                            }
                        }
                    }
                }
                XmlMode::InOpenTag => {
                    if let Some(rel) = bytes[i..]
                        .iter()
                        .position(|&b| matches!(b, b'>' | b'/' | b'"' | b'\'' | b'<'))
                    {
                        i += rel;
                        let b = bytes[i];
                        i += 1;
                        match b {
                            b'>' => {
                                if !self.fragment && self.depth == 0 {
                                    self.roots += 1;
                                }
                                self.depth += 1;
                                self.mode = XmlMode::Text;
                            }
                            b'/' => self.mode = XmlMode::SlashClose,
                            b'"' | b'\'' => self.mode = XmlMode::AttrQuote(b),
                            _ => {
                                self.failed = true;
                                return;
                            }
                        }
                    } else {
                        break;
                    }
                }
                XmlMode::AttrQuote(q) => {
                    if let Some(rel) = bytes[i..].iter().position(|&b| b == q || b == b'<') {
                        i += rel;
                        if bytes[i] == b'<' {
                            self.failed = true;
                            return;
                        }
                        i += 1;
                        self.mode = XmlMode::InOpenTag;
                    } else {
                        break;
                    }
                }
                XmlMode::SlashClose => {
                    if bytes[i] != b'>' {
                        self.failed = true;
                        return;
                    }
                    i += 1;
                    if !self.fragment && self.depth == 0 {
                        self.roots += 1;
                        self.closed = true;
                    }
                    self.mode = XmlMode::Text;
                }
                XmlMode::CloseTagName => {
                    if let Some(rel) = bytes[i..].iter().position(|&b| b == b'>' || b == b'<') {
                        i += rel;
                        if bytes[i] == b'<' {
                            self.failed = true;
                            return;
                        }
                        i += 1;
                        self.depth -= 1;
                        if self.depth < self.min_depth {
                            self.min_depth = self.depth;
                        }
                        if !self.fragment {
                            if self.depth < 0 {
                                self.failed = true;
                                return;
                            }
                            if self.depth == 0 {
                                self.closed = true;
                            }
                        }
                        self.mode = XmlMode::Text;
                    } else {
                        break;
                    }
                }
                XmlMode::Bang => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        b'-' => self.mode = XmlMode::CommentDash1,
                        b'[' => self.mode = XmlMode::CdataPrefix(0),
                        _ => {
                            self.failed = true;
                            return;
                        }
                    }
                }
                XmlMode::CommentDash1 => {
                    if bytes[i] != b'-' {
                        self.failed = true;
                        return;
                    }
                    i += 1;
                    self.mode = XmlMode::Comment;
                }
                XmlMode::Comment => {
                    if let Some(rel) = bytes[i..].iter().position(|&b| b == b'-') {
                        i += rel + 1;
                        self.mode = XmlMode::CommentEnd1;
                    } else {
                        break;
                    }
                }
                XmlMode::CommentEnd1 => {
                    let b = bytes[i];
                    i += 1;
                    self.mode = if b == b'-' {
                        XmlMode::CommentEnd2
                    } else {
                        XmlMode::Comment
                    };
                }
                XmlMode::CommentEnd2 => {
                    let b = bytes[i];
                    i += 1;
                    if b == b'>' {
                        self.mode = XmlMode::Text;
                    } else {
                        self.failed = true;
                        return;
                    }
                }
                XmlMode::CdataPrefix(step) => {
                    const CDATA: &[u8; 6] = b"CDATA[";
                    if bytes[i] != CDATA[step as usize] {
                        self.failed = true;
                        return;
                    }
                    i += 1;
                    if step == 5 {
                        if !self.fragment && self.depth == 0 {
                            self.failed = true;
                            return;
                        }
                        self.mode = XmlMode::Cdata;
                    } else {
                        self.mode = XmlMode::CdataPrefix(step + 1);
                    }
                }
                XmlMode::Cdata => {
                    if let Some(rel) = bytes[i..].iter().position(|&b| b == b']') {
                        i += rel + 1;
                        self.mode = XmlMode::CdataEnd1;
                    } else {
                        break;
                    }
                }
                XmlMode::CdataEnd1 => {
                    let b = bytes[i];
                    i += 1;
                    self.mode = if b == b']' {
                        XmlMode::CdataEnd2
                    } else {
                        XmlMode::Cdata
                    };
                }
                XmlMode::CdataEnd2 => {
                    let b = bytes[i];
                    i += 1;
                    self.mode = match b {
                        b'>' => XmlMode::Text,
                        b']' => XmlMode::CdataEnd2,
                        _ => XmlMode::Cdata,
                    };
                }
                XmlMode::Pi => {
                    if let Some(rel) = bytes[i..].iter().position(|&b| b == b'?') {
                        i += rel + 1;
                        self.mode = XmlMode::PiEnd;
                    } else {
                        break;
                    }
                }
                XmlMode::PiEnd => {
                    let b = bytes[i];
                    i += 1;
                    self.mode = match b {
                        b'>' => XmlMode::Text,
                        b'?' => XmlMode::PiEnd,
                        _ => XmlMode::Pi,
                    };
                }
            }
        }
    }

    pub(super) fn finish_document(&self, expected_root: Option<&str>) -> bool {
        !self.failed
            && self.utf8_tail_len == 0
            && self.mode == XmlMode::Text
            && self.depth == 0
            && self.roots == 1
            && self.closed
            && expected_root.is_none_or(|e| &self.root_local[..self.root_local_len] == e.as_bytes())
    }

    pub(super) fn finish_fragment(&self) -> bool {
        !self.failed
            && self.utf8_tail_len == 0
            && self.mode == XmlMode::Text
            && self.depth == 0
            && self.min_depth == 0
    }
}

pub(super) struct VerifiedXmlWriter<W> {
    inner: W,
    crc: flate2::Crc,
    size: u64,
    check: XmlCheckState,
}

impl<W: Write> VerifiedXmlWriter<W> {
    pub(super) fn new(inner: W) -> Self {
        Self {
            inner,
            crc: flate2::Crc::new(),
            size: 0,
            check: XmlCheckState::new_document(),
        }
    }

    pub(super) fn finish(mut self, expected_root: Option<&str>) -> Result<Option<(u32, u64)>> {
        self.inner.flush()?;
        Ok(self
            .check
            .finish_document(expected_root)
            .then_some((self.crc.sum(), self.size)))
    }
}

impl<W: Write> Write for VerifiedXmlWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write_all(buf)?;
        self.crc.update(buf);
        self.size += buf.len() as u64;
        self.check.feed(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Stream a producer into parallel RFC 1951 deflate workers (overlapping production with
/// compression across cores for entries > 2 MB), verify XML well-formedness on the fly,
/// and splice the resulting single-entry ZIP into `out` via `raw_copy_file`.
///
/// Returns:
/// - `Ok(None)` if `producer` returned `Ok(false)` (unsupported stream -> caller falls back).
/// - `Ok(Some(trusted_sig))` if written; `trusted_sig` is `Some((crc32, size))` when XML check passed.
pub(super) fn write_parallel_deflate_entry<W: Write + std::io::Seek>(
    out: &mut ZipWriter<W>,
    part: &str,
    level: u32,
    expected_root: Option<&str>,
    producer: impl FnOnce(&mut dyn Write) -> Result<bool>,
) -> Result<Option<Option<(u32, u64)>>> {
    use std::collections::VecDeque;
    use std::sync::{Condvar, Mutex};

    const CHUNK_BYTES: usize = 2 * 1024 * 1024;
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .saturating_sub(1)
        .clamp(1, 7);

    struct QueueState {
        items: VecDeque<(usize, Vec<u8>, bool)>,
        closed: bool,
        aborted: bool,
    }
    struct Queue {
        state: Mutex<QueueState>,
        not_empty: Condvar,
        not_full: Condvar,
        max_depth: usize,
    }
    impl Queue {
        fn push(&self, seq: usize, data: Vec<u8>, is_last: bool) {
            let mut g = self.state.lock().unwrap();
            while g.items.len() >= self.max_depth && !g.aborted {
                g = self.not_full.wait(g).unwrap();
            }
            if !g.aborted {
                g.items.push_back((seq, data, is_last));
                self.not_empty.notify_one();
            }
        }
        fn pop(&self) -> Option<(usize, Vec<u8>, bool)> {
            let mut g = self.state.lock().unwrap();
            loop {
                if g.aborted {
                    return None;
                }
                if let Some(item) = g.items.pop_front() {
                    self.not_full.notify_one();
                    return Some(item);
                }
                if g.closed {
                    return None;
                }
                g = self.not_empty.wait(g).unwrap();
            }
        }
        fn close(&self) {
            let mut g = self.state.lock().unwrap();
            g.closed = true;
            self.not_empty.notify_all();
        }
        fn abort(&self) {
            let mut g = self.state.lock().unwrap();
            g.aborted = true;
            g.items.clear();
            self.not_empty.notify_all();
            self.not_full.notify_all();
        }
    }

    struct PipeWriter<'a> {
        queue: &'a Queue,
        current: Vec<u8>,
        pending: Option<Vec<u8>>,
        seq: usize,
        dispatched: bool,
        check: XmlCheckState,
    }
    impl PipeWriter<'_> {
        fn dispatch_if_ready(&mut self) {
            if let Some(prev) = self.pending.take() {
                let seq = self.seq;
                self.seq += 1;
                self.dispatched = true;
                self.queue.push(seq, prev, false);
            }
            let finished = std::mem::replace(&mut self.current, Vec::with_capacity(CHUNK_BYTES + 4096));
            self.pending = Some(finished);
        }
    }
    impl Write for PipeWriter<'_> {
        fn write(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
            let total = buf.len();
            self.check.feed(buf);
            while !buf.is_empty() {
                let space = CHUNK_BYTES.saturating_sub(self.current.len());
                if space == 0 {
                    self.dispatch_if_ready();
                    continue;
                }
                let take = buf.len().min(space);
                self.current.extend_from_slice(&buf[..take]);
                buf = &buf[take..];
                if self.current.len() >= CHUNK_BYTES {
                    self.dispatch_if_ready();
                }
            }
            Ok(total)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let queue = Queue {
        state: Mutex::new(QueueState {
            items: VecDeque::new(),
            closed: false,
            aborted: false,
        }),
        not_empty: Condvar::new(),
        not_full: Condvar::new(),
        max_depth: workers * 2,
    };
    let compressed_out: Mutex<Vec<Result<(usize, Vec<u8>, u32, u64)>>> = Mutex::new(Vec::new());

    let mut single_buf: Option<Vec<u8>> = None;
    let mut xml_ok = false;
    let mut produced_ok = false;

    std::thread::scope(|scope| -> Result<()> {
        // Workers are only spawned once input actually exceeds CHUNK_BYTES, keeping small sheets zero-overhead.
        let pw = PipeWriter {
            queue: &queue,
            current: Vec::with_capacity(64 * 1024),
            pending: None,
            seq: 0,
            dispatched: false,
            check: XmlCheckState::new_document(),
        };

        // Spawn workers eagerly only if we want streaming overlap; actually spawning up to `workers`
        // threads inside `scope` takes ~0.15 ms on Windows, OR we can spawn lazily on first dispatch!
        // Let's check: a 2 MB threshold means small sheets NEVER spawn threads!
        struct LazyPool<'s, 'a> {
            scope: &'s std::thread::Scope<'s, 'a>,
            queue: &'a Queue,
            out: &'a Mutex<Vec<Result<(usize, Vec<u8>, u32, u64)>>>,
            workers: usize,
            level: u32,
            spawned: bool,
        }
        impl<'s, 'a> LazyPool<'s, 'a> {
            fn ensure(&mut self) {
                if self.spawned {
                    return;
                }
                self.spawned = true;
                for _ in 0..self.workers {
                    let q = self.queue;
                    let o = self.out;
                    let lvl = self.level;
                    self.scope.spawn(move || {
                        while let Some((seq, data, is_last)) = q.pop() {
                            let raw_len = data.len() as u64;
                            let res = deflate_raw_chunk(&data, lvl, is_last)
                                .map(|(comp, crc)| (seq, comp, crc, raw_len));
                            o.lock().unwrap().push(res);
                        }
                    });
                }
            }
        }

        struct LazyPipeWriter<'s, 'a> {
            inner: PipeWriter<'a>,
            pool: LazyPool<'s, 'a>,
        }
        impl Write for LazyPipeWriter<'_, '_> {
            fn write(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
                let total = buf.len();
                self.inner.check.feed(buf);
                while !buf.is_empty() {
                    let space = CHUNK_BYTES.saturating_sub(self.inner.current.len());
                    if space == 0 {
                        if self.inner.pending.is_some() {
                            self.pool.ensure();
                        }
                        self.inner.dispatch_if_ready();
                        continue;
                    }
                    let take = buf.len().min(space);
                    self.inner.current.extend_from_slice(&buf[..take]);
                    buf = &buf[take..];
                    if self.inner.current.len() >= CHUNK_BYTES {
                        if self.inner.pending.is_some() {
                            self.pool.ensure();
                        }
                        self.inner.dispatch_if_ready();
                    }
                }
                Ok(total)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut lpw = LazyPipeWriter {
            inner: pw,
            pool: LazyPool {
                scope,
                queue: &queue,
                out: &compressed_out,
                workers,
                level,
                spawned: false,
            },
        };
        let res = producer(&mut lpw);
        match res {
            Ok(true) => {
                produced_ok = true;
                xml_ok = lpw.inner.check.finish_document(expected_root);
                if !lpw.inner.dispatched && lpw.inner.pending.is_none() {
                    single_buf = Some(lpw.inner.current);
                    queue.close();
                } else if !lpw.inner.dispatched && lpw.inner.current.is_empty() {
                    single_buf = lpw.inner.pending.take();
                    queue.close();
                } else {
                    lpw.pool.ensure();
                    if let Some(prev) = lpw.inner.pending.take() {
                        let is_last = lpw.inner.current.is_empty();
                        let seq = lpw.inner.seq;
                        lpw.inner.seq += 1;
                        queue.push(seq, prev, is_last);
                    }
                    if !lpw.inner.current.is_empty() {
                        let seq = lpw.inner.seq;
                        queue.push(seq, lpw.inner.current, true);
                    }
                    queue.close();
                }
            }
            Ok(false) => {
                queue.abort();
            }
            Err(e) => {
                queue.abort();
                return Err(e);
            }
        }
        Ok(())
    })?;

    if !produced_ok {
        return Ok(None);
    }

    if let Some(data) = single_buf {
        let mut crc = flate2::Crc::new();
        crc.update(&data);
        let sig = (crc.sum(), data.len() as u64);
        out.start_file(
            part,
            SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(i64::from(level))),
        )?;
        out.write_all(&data)?;
        return Ok(Some(xml_ok.then_some(sig)));
    }

    let mut chunks: Vec<(usize, Vec<u8>, u32, u64)> = Vec::new();
    for item in compressed_out.into_inner().unwrap() {
        chunks.push(item?);
    }
    chunks.sort_unstable_by_key(|(seq, _, _, _)| *seq);
    let mut total_crc = 0u32;
    let mut total_len = 0u64;
    let mut comp_slices = Vec::with_capacity(chunks.len());
    for (_, comp, crc, raw_len) in chunks {
        total_crc = if total_len == 0 {
            crc
        } else {
            crc32_combine(total_crc, crc, raw_len)
        };
        total_len += raw_len;
        comp_slices.push(comp);
    }
    let mini = build_single_entry_zip(part, total_crc, total_len, &comp_slices)?;
    let mut mini_zip = ZipArchive::new(std::io::Cursor::new(mini))?;
    out.raw_copy_file(mini_zip.by_index_raw(0)?)?;
    Ok(Some(xml_ok.then_some((total_crc, total_len))))
}

#[inline]
fn push_u32(out: &mut Vec<u8>, mut v: u32) {
    let mut tmp = [0u8; 10];
    let mut len = 0usize;
    loop {
        tmp[len] = b'0' + (v % 10) as u8;
        len += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in (0..len).rev() {
        out.push(tmp[i]);
    }
}

#[inline]
fn push_u16(out: &mut Vec<u8>, v: u16) {
    push_u32(out, u32::from(v));
}

#[inline]
fn push_i64(out: &mut Vec<u8>, v: i64) {
    let mut mag = if v < 0 {
        out.push(b'-');
        v.unsigned_abs()
    } else {
        v as u64
    };
    let mut tmp = [0u8; 20];
    let mut len = 0usize;
    loop {
        tmp[len] = b'0' + (mag % 10) as u8;
        len += 1;
        mag /= 10;
        if mag == 0 {
            break;
        }
    }
    for i in (0..len).rev() {
        out.push(tmp[i]);
    }
}

#[inline]
fn push_a1(out: &mut Vec<u8>, row: u32, col: u32) {
    let mut c = col + 1;
    let mut col_tmp = [0u8; 4];
    let mut clen = 0usize;
    while c > 0 {
        let rem = ((c - 1) % 26) as u8;
        col_tmp[clen] = b'A' + rem;
        clen += 1;
        c = (c - 1) / 26;
    }
    for i in (0..clen).rev() {
        out.push(col_tmp[i]);
    }
    push_u32(out, row + 1);
}

#[inline]
fn format_a1_buf(row: u32, col: u32, buf: &mut [u8; 16]) -> &str {
    let mut c = col + 1;
    let mut col_tmp = [0u8; 4];
    let mut clen = 0usize;
    while c > 0 {
        let rem = ((c - 1) % 26) as u8;
        col_tmp[clen] = b'A' + rem;
        clen += 1;
        c = (c - 1) / 26;
    }
    let mut pos = 0usize;
    for i in (0..clen).rev() {
        buf[pos] = col_tmp[i];
        pos += 1;
    }
    let mut r = row + 1;
    let mut row_tmp = [0u8; 10];
    let mut rlen = 0usize;
    while r > 0 {
        row_tmp[rlen] = b'0' + (r % 10) as u8;
        rlen += 1;
        r /= 10;
    }
    for i in (0..rlen).rev() {
        buf[pos] = row_tmp[i];
        pos += 1;
    }
    // SAFETY: only ASCII uppercase letters and digits were written.
    unsafe { std::str::from_utf8_unchecked(&buf[..pos]) }
}

fn declare_sst(xml: &mut String, part: &str) -> Result<()> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let (closing, record) = if part == super::dynamic::RELS {
        if root.children().any(|r| r.attribute("Type") == Some(SST_REL)) {
            return Ok(());
        }
        let mut n = 1usize;
        let id = loop {
            let candidate = format!("rId{n}");
            if !root.children().any(|r| r.attribute("Id") == Some(&candidate)) {
                break candidate;
            }
            n += 1;
        };
        (
            "</Relationships>",
            format!("<Relationship Id=\"{id}\" Type=\"{SST_REL}\" Target=\"sharedStrings.xml\"/>"),
        )
    } else {
        if root
            .children()
            .any(|r| r.attribute("PartName") == Some("/xl/sharedStrings.xml"))
        {
            return Ok(());
        }
        (
            "</Types>",
            format!("<Override PartName=\"/xl/sharedStrings.xml\" ContentType=\"{SST_CONTENT_TYPE}\"/>"),
        )
    };
    let index = xml[..root.range().end]
        .rfind(closing)
        .context("共享字符串清单无效")?;
    xml.insert_str(index, &record);
    Ok(())
}

struct FreshSst<'a> {
    indices: HashMap<&'a str, u32>,
    xml: Vec<u8>,
    existed_in_zip: bool,
}

impl<'a> FreshSst<'a> {
    fn build(zip: &mut ZipArchive<File>, ordered_sheets: &[(&str, &'a Sheet)]) -> Result<Option<Self>> {
        let mut counts: HashMap<&'a str, u32> = HashMap::new();
        let mut repeated_in_order: Vec<&'a str> = Vec::new();
        for &(_, sheet) in ordered_sheets {
            let has_spilled = !sheet.spilled.is_empty();
            let has_rich = !sheet.rich.is_empty();
            for (pi, page) in sheet.cells().iter_pages() {
                let base = pi * crate::store::PAGE_ROWS;
                for (col, slab) in page.cols.iter_entries() {
                    match slab.as_ref() {
                        crate::store::ColSlab::Numbers(_) => continue, // O(1) skip: zero strings!
                        crate::store::ColSlab::Values(vs) => {
                            for (r_off, val) in vs.iter().enumerate() {
                                let Value::Text(s) = val else { continue };
                                let row = base + r_off as u32;
                                if (has_spilled && sheet.spilled.contains(&CellRef::new(row, col)))
                                    || (has_rich
                                        && sheet
                                            .rich
                                            .get(&(row, col))
                                            .is_some_and(|runs| !runs.is_empty()))
                                {
                                    continue;
                                }
                                let text: &'a str = s.as_ref();
                                let entry = counts.entry(text).or_insert(0);
                                *entry += 1;
                                if *entry == 2 {
                                    repeated_in_order.push(text);
                                }
                            }
                        }
                        crate::store::ColSlab::Mixed(cs) => {
                            for (r_off, cell) in cs.iter().enumerate() {
                                if cell.formula.is_some() {
                                    continue;
                                }
                                let Value::Text(s) = &cell.value else { continue };
                                let row = base + r_off as u32;
                                if (has_spilled && sheet.spilled.contains(&CellRef::new(row, col)))
                                    || (has_rich
                                        && sheet
                                            .rich
                                            .get(&(row, col))
                                            .is_some_and(|runs| !runs.is_empty()))
                                {
                                    continue;
                                }
                                let text: &'a str = s.as_ref();
                                let entry = counts.entry(text).or_insert(0);
                                *entry += 1;
                                if *entry == 2 {
                                    repeated_in_order.push(text);
                                }
                            }
                        }
                    }
                }
            }
        }
        if repeated_in_order.is_empty() {
            return Ok(None);
        }

        let existed_in_zip = zip.index_for_name(SST_PART).is_some();
        let mut xml = String::new();
        let mut next_idx: u32 = 0;
        if existed_in_zip {
            let existing = super::read_zip_text(zip, SST_PART)?;
            // Single-pass streaming scan of existing sharedStrings.xml (no full DOM).
            let mut reader = quick_xml::Reader::from_str(&existing);
            let mut depth = 0usize;
            let mut root_end = 0usize;
            let mut empty_root = false;
            let mut clean_open = String::new();
            let mut close_pos = None;
            let mut saw_root = false;
            loop {
                let start = reader.buffer_position() as usize;
                let ev = reader.read_event()?;
                let end = reader.buffer_position() as usize;
                match ev {
                    quick_xml::events::Event::Start(ref e) | quick_xml::events::Event::Empty(ref e) => {
                        let empty = matches!(ev, quick_xml::events::Event::Empty(_));
                        if depth == 0 {
                            ensure!(
                                !saw_root && e.local_name().as_ref() == "sst",
                                "共享字符串根节点无效"
                            );
                            saw_root = true;
                            empty_root = empty;
                            root_end = end;
                            let mut tag = quick_xml::events::BytesStart::new(e.name().as_ref().to_owned());
                            for a in e.attributes() {
                                let a = a?;
                                if !matches!(a.key.as_ref(), "count" | "uniqueCount") {
                                    tag.push_attribute(a);
                                }
                            }
                            let mut w = quick_xml::Writer::new(Vec::new());
                            w.write_event(quick_xml::events::Event::Start(tag))?;
                            clean_open = String::from_utf8(w.into_inner())?;
                            if !empty {
                                depth = 1;
                            }
                        } else {
                            if depth == 1 && e.local_name().as_ref() == "si" {
                                next_idx = next_idx.checked_add(1).context("共享字符串索引溢出")?;
                            }
                            if !empty {
                                depth += 1;
                            }
                        }
                    }
                    quick_xml::events::Event::End(_) => {
                        depth = depth.checked_sub(1).context("共享字符串标签不匹配")?;
                        if depth == 0 {
                            close_pos = Some(start);
                        }
                    }
                    quick_xml::events::Event::Eof => break,
                    _ => {}
                }
            }
            ensure!(saw_root && depth == 0, "共享字符串缺少根标签");
            xml.reserve(existing.len() + repeated_in_order.len() * 32);
            xml.push_str(&clean_open);
            if !empty_root {
                let close = close_pos.context("共享字符串缺少闭合标签")?;
                xml.push_str(&existing[root_end..close]);
            }
        } else {
            xml.reserve(160 + repeated_in_order.len() * 32);
            xml.push_str(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
            );
        }

        let mut indices = HashMap::with_capacity(repeated_in_order.len());
        for s in repeated_in_order {
            let idx = next_idx;
            next_idx = next_idx.checked_add(1).context("共享字符串索引溢出")?;
            indices.insert(s, idx);
            let preserve = s.starts_with(char::is_whitespace)
                || s.ends_with(char::is_whitespace)
                || s.bytes().any(|b| matches!(b, b'\t' | b'\r' | b'\n'));
            if preserve {
                xml.push_str("<si><t xml:space=\"preserve\">");
            } else {
                xml.push_str("<si><t>");
            }
            xml.push_str(&quick_xml::escape::escape(s));
            xml.push_str("</t></si>");
        }
        xml.push_str("</sst>");
        Ok(Some(Self {
            indices,
            xml: xml.into_bytes(),
            existed_in_zip,
        }))
    }
}

fn extract_dxfs_slice(xml: &str) -> Option<&str> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut depth = 0usize;
    let mut dxfs_start = None;
    loop {
        let start = reader.buffer_position() as usize;
        match reader.read_event().ok()? {
            quick_xml::events::Event::Start(e) => {
                if depth == 1 && e.local_name().as_ref() == "dxfs" {
                    dxfs_start = Some(start);
                }
                depth += 1;
            }
            quick_xml::events::Event::Empty(e) => {
                if depth == 1 && e.local_name().as_ref() == "dxfs" {
                    let end = reader.buffer_position() as usize;
                    return Some(&xml[start..end]);
                }
            }
            quick_xml::events::Event::End(_) => {
                depth = depth.checked_sub(1)?;
                if depth == 1
                    && let Some(ds) = dxfs_start.take()
                {
                    let end = reader.buffer_position() as usize;
                    return Some(&xml[ds..end]);
                }
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    None
}

fn push_color_tag(
    out: &mut String,
    tag: &str,
    rgb: Option<[u8; 3]>,
    theme: Option<(u8, f32)>,
    indexed: Option<u8>,
) {
    use std::fmt::Write as _;
    if let Some(i) = indexed {
        let _ = write!(out, "<{tag} indexed=\"{i}\"/>");
    } else if let Some((idx, tint)) = theme {
        if tint != 0.0 {
            let _ = write!(out, "<{tag} theme=\"{idx}\" tint=\"{tint}\"/>");
        } else {
            let _ = write!(out, "<{tag} theme=\"{idx}\"/>");
        }
    } else if let Some([r, g, b]) = rgb {
        let _ = write!(out, "<{tag} rgb=\"FF{r:02X}{g:02X}{b:02X}\"/>");
    }
}

struct FreshStyles {
    sid_to_xf: Vec<Option<u16>>,
    xml: Vec<u8>,
    sig: (u32, u64),
    saved: super::delta::SavedStyles,
}

impl FreshStyles {
    fn build(
        zip: &mut ZipArchive<File>,
        wb: &Workbook,
        ordered_sheets: &[(&str, &Sheet)],
    ) -> Result<Self> {
        use std::fmt::Write as _;
        let style_count = wb.styles.len();
        let mut used = vec![false; style_count.max(1)];
        used[0] = true;
        for &(_, sheet) in ordered_sheets {
            if !sheet.styles.is_empty() {
                for (_, &sid) in sheet.styles.iter() {
                    if let Some(slot) = used.get_mut(usize::from(sid)) {
                        *slot = true;
                    }
                }
            }
            for &sid in sheet.row_styles.values() {
                if let Some(slot) = used.get_mut(usize::from(sid)) {
                    *slot = true;
                }
            }
            for &sid in sheet.col_styles.values() {
                if let Some(slot) = used.get_mut(usize::from(sid)) {
                    *slot = true;
                }
            }
        }

        let existing_styles = if zip.index_for_name(STYLES_PART).is_some() {
            super::read_zip_text(zip, STYLES_PART).ok()
        } else {
            None
        };
        let dxfs_xml = existing_styles
            .as_deref()
            .and_then(extract_dxfs_slice)
            .unwrap_or("<dxfs count=\"0\"/>");

        let mut num_fmts: Vec<(u16, String)> = Vec::new();
        let mut num_fmt_map: HashMap<String, u16> = HashMap::new();
        let mut next_num_fmt_id: u16 = 164;

        let default_font = "<font><sz val=\"11\"/><color rgb=\"FF000000\"/><name val=\"Calibri\"/></font>".to_owned();
        let mut fonts: Vec<String> = vec![default_font.clone()];
        let mut font_map: HashMap<String, u16> = HashMap::from([(default_font, 0)]);

        let fill_none = "<fill><patternFill patternType=\"none\"/></fill>".to_owned();
        let fill_gray = "<fill><patternFill patternType=\"gray125\"/></fill>".to_owned();
        let mut fills: Vec<String> = vec![fill_none.clone(), fill_gray.clone()];
        let mut fill_map: HashMap<String, u16> = HashMap::from([(fill_none, 0), (fill_gray, 1)]);

        let border_none = "<border><left/><right/><top/><bottom/><diagonal/></border>".to_owned();
        let mut borders: Vec<String> = vec![border_none.clone()];
        let mut border_map: HashMap<String, u16> = HashMap::from([(border_none, 0)]);

        let active_count = used.iter().filter(|&&u| u).count();
        let mut cell_xfs: Vec<String> = Vec::with_capacity(active_count);
        let mut sid_to_xf: Vec<Option<u16>> = vec![None; style_count.max(1)];
        let mut xf_to_sid: HashMap<u16, u16> = HashMap::with_capacity(active_count);

        let mut scratch = String::with_capacity(256);

        for (sid_idx, &is_used) in used.iter().enumerate() {
            if !is_used {
                continue;
            }
            let sid = u16::try_from(sid_idx).context("样式索引溢出")?;
            let st = wb.styles.get(sid);
            let theme = wb.styles.theme_of(sid);

            // 1. Number format
            let nf_id: u16 = if st.format.is_empty() || st.format == "General" {
                0
            } else if st.format == "@" {
                49
            } else if let Some(&id) = num_fmt_map.get(&st.format) {
                id
            } else {
                let id = next_num_fmt_id;
                next_num_fmt_id = next_num_fmt_id.checked_add(1).context("数字格式数量超限")?;
                num_fmt_map.insert(st.format.clone(), id);
                let escaped = quick_xml::escape::escape(&st.format);
                num_fmts.push((id, format!("<numFmt numFmtId=\"{id}\" formatCode=\"{escaped}\"/>")));
                id
            };

            // 2. Font
            scratch.clear();
            scratch.push_str("<font>");
            if st.bold {
                scratch.push_str("<b/>");
            }
            if st.italic {
                scratch.push_str("<i/>");
            }
            if st.strikethrough {
                scratch.push_str("<strike/>");
            }
            if st.underline {
                if st.double_underline {
                    scratch.push_str("<u val=\"double\"/>");
                } else {
                    scratch.push_str("<u/>");
                }
            }
            let sz = if st.size >= 1.0 && st.size.is_finite() {
                st.size
            } else {
                11.0
            };
            let _ = write!(scratch, "<sz val=\"{sz}\"/>");
            if let Some(t) = theme.font {
                push_color_tag(&mut scratch, "color", None, Some((t.idx, t.tint)), None);
            } else {
                push_color_tag(&mut scratch, "color", Some(st.color), None, None);
            }
            let fname = if st.font.is_empty() {
                "Calibri"
            } else {
                st.font.as_str()
            };
            let escaped_font = quick_xml::escape::escape(fname);
            let _ = write!(scratch, "<name val=\"{escaped_font}\"/></font>");
            let font_id = if let Some(&id) = font_map.get(&scratch) {
                id
            } else {
                let id = u16::try_from(fonts.len()).context("字体数量超限")?;
                ensure!(id < 65_490, "字体数量超限");
                let owned = scratch.clone();
                font_map.insert(owned.clone(), id);
                fonts.push(owned);
                id
            };

            // 3. Fill
            let fill_id = if let Some(p) = wb.styles.pattern_of(sid) {
                if p.kind.is_empty() || p.kind == "none" {
                    0
                } else {
                    scratch.clear();
                    let ek = quick_xml::escape::escape(&p.kind);
                    let has_fg = p.fg.is_some() || p.fg_theme.is_some() || p.fg_indexed.is_some();
                    let has_bg = p.bg.is_some() || p.bg_theme.is_some() || p.bg_indexed.is_some();
                    if has_fg || has_bg {
                        let _ = write!(scratch, "<fill><patternFill patternType=\"{ek}\">");
                        push_color_tag(&mut scratch, "fgColor", p.fg, p.fg_theme, p.fg_indexed);
                        push_color_tag(&mut scratch, "bgColor", p.bg, p.bg_theme, p.bg_indexed);
                        scratch.push_str("</patternFill></fill>");
                    } else {
                        let _ = write!(scratch, "<fill><patternFill patternType=\"{ek}\"/></fill>");
                    }
                    if let Some(&id) = fill_map.get(&scratch) {
                        id
                    } else {
                        let id = u16::try_from(fills.len()).context("填充数量超限")?;
                        ensure!(id < 65_490, "填充数量超限");
                        let owned = scratch.clone();
                        fill_map.insert(owned.clone(), id);
                        fills.push(owned);
                        id
                    }
                }
            } else if let Some(rgb) = st.fill {
                scratch.clear();
                scratch.push_str("<fill><patternFill patternType=\"solid\">");
                if let Some(t) = theme.fill {
                    push_color_tag(&mut scratch, "fgColor", None, Some((t.idx, t.tint)), None);
                } else {
                    push_color_tag(&mut scratch, "fgColor", Some(rgb), None, None);
                }
                scratch.push_str("</patternFill></fill>");
                if let Some(&id) = fill_map.get(&scratch) {
                    id
                } else {
                    let id = u16::try_from(fills.len()).context("填充数量超限")?;
                    ensure!(id < 65_490, "填充数量超限");
                    let owned = scratch.clone();
                    fill_map.insert(owned.clone(), id);
                    fills.push(owned);
                    id
                }
            } else {
                0
            };

            // 4. Border
            let diag = wb.styles.diag_of(sid);
            let border_id = if st.border.iter().all(|b| b.style == 0) && diag.is_none() {
                0
            } else {
                scratch.clear();
                scratch.push_str("<border");
                if let Some(d) = diag {
                    if d.up {
                        scratch.push_str(" diagonalUp=\"1\"");
                    }
                    if d.down {
                        scratch.push_str(" diagonalDown=\"1\"");
                    }
                }
                scratch.push('>');
                for (side_name, b) in [
                    ("left", &st.border[0]),
                    ("right", &st.border[1]),
                    ("top", &st.border[2]),
                    ("bottom", &st.border[3]),
                ] {
                    if b.style == 0 {
                        let _ = write!(scratch, "<{side_name}/>");
                    } else {
                        let sname = crate::style::Border::ooxml_name(b.style);
                        let _ = write!(
                            scratch,
                            "<{side_name} style=\"{sname}\"><color rgb=\"FF{:02X}{:02X}{:02X}\"/></{side_name}>",
                            b.color[0], b.color[1], b.color[2]
                        );
                    }
                }
                if let Some(d) = diag {
                    let dstyle = quick_xml::escape::escape(&d.style);
                    let _ = write!(scratch, "<diagonal style=\"{dstyle}\">");
                    push_color_tag(&mut scratch, "color", Some(d.color), d.color_theme, None);
                    scratch.push_str("</diagonal>");
                } else {
                    scratch.push_str("<diagonal/>");
                }
                scratch.push_str("</border>");
                if let Some(&id) = border_map.get(&scratch) {
                    id
                } else {
                    let id = u16::try_from(borders.len()).context("边框数量超限")?;
                    ensure!(id < 65_490, "边框数量超限");
                    let owned = scratch.clone();
                    border_map.insert(owned.clone(), id);
                    borders.push(owned);
                    id
                }
            };

            // 5. XF
            let has_align = st.halign != 0
                || st.valign != 0
                || st.wrap
                || st.shrink_to_fit
                || st.indent > 0
                || st.vertical_text
                || st.rotation != 0;
            let has_prot = !st.locked || st.hidden;

            scratch.clear();
            let _ = write!(
                scratch,
                "<xf numFmtId=\"{nf_id}\" fontId=\"{font_id}\" fillId=\"{fill_id}\" borderId=\"{border_id}\" xfId=\"0\""
            );
            if nf_id != 0 {
                scratch.push_str(" applyNumberFormat=\"1\"");
            }
            if font_id != 0 {
                scratch.push_str(" applyFont=\"1\"");
            }
            if fill_id != 0 {
                scratch.push_str(" applyFill=\"1\"");
            }
            if border_id != 0 {
                scratch.push_str(" applyBorder=\"1\"");
            }
            if has_align {
                scratch.push_str(" applyAlignment=\"1\"");
            }
            if has_prot {
                scratch.push_str(" applyProtection=\"1\"");
            }
            if wb.styles.quote_of(sid) {
                scratch.push_str(" quotePrefix=\"1\"");
            }
            if !has_align && !has_prot {
                scratch.push_str("/>");
            } else {
                scratch.push('>');
                if has_align {
                    scratch.push_str("<alignment");
                    match st.halign {
                        1 => scratch.push_str(" horizontal=\"left\""),
                        2 => scratch.push_str(" horizontal=\"center\""),
                        3 => scratch.push_str(" horizontal=\"right\""),
                        4 => scratch.push_str(" horizontal=\"centerContinuous\""),
                        5 => scratch.push_str(" horizontal=\"distributed\""),
                        _ => {}
                    }
                    match st.valign {
                        1 => scratch.push_str(" vertical=\"top\""),
                        2 => scratch.push_str(" vertical=\"center\""),
                        3 => scratch.push_str(" vertical=\"bottom\""),
                        _ => {}
                    }
                    if st.wrap {
                        scratch.push_str(" wrapText=\"1\"");
                    }
                    if st.shrink_to_fit {
                        scratch.push_str(" shrinkToFit=\"1\"");
                    }
                    if st.indent > 0 {
                        let _ = write!(scratch, " indent=\"{}\"", st.indent);
                    }
                    if st.vertical_text {
                        scratch.push_str(" textRotation=\"255\"");
                    } else if st.rotation != 0 {
                        let v = if st.rotation >= 0 {
                            st.rotation as u32
                        } else {
                            (90 - st.rotation) as u32
                        };
                        let _ = write!(scratch, " textRotation=\"{v}\"");
                    }
                    scratch.push_str("/>");
                }
                if has_prot {
                    let _ = write!(
                        scratch,
                        "<protection locked=\"{}\" hidden=\"{}\"/>",
                        u8::from(st.locked),
                        u8::from(st.hidden)
                    );
                }
                scratch.push_str("</xf>");
            }

            let xf = u16::try_from(cell_xfs.len()).context("单元格样式数量超限")?;
            ensure!(xf < 65_490, "单元格样式数量超限");
            cell_xfs.push(scratch.clone());
            sid_to_xf[sid_idx] = Some(xf);
            xf_to_sid.insert(xf, sid);
        }

        let est = 512
            + num_fmts.len() * 48
            + fonts.len() * 88
            + fills.len() * 88
            + borders.len() * 96
            + cell_xfs.len() * 96
            + dxfs_xml.len();
        let mut xml = String::with_capacity(est);
        xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">");
        if !num_fmts.is_empty() {
            let _ = write!(xml, "<numFmts count=\"{}\">", num_fmts.len());
            for (_, tag) in &num_fmts {
                xml.push_str(tag);
            }
            xml.push_str("</numFmts>");
        }
        let _ = write!(xml, "<fonts count=\"{}\">", fonts.len());
        for f in &fonts {
            xml.push_str(f);
        }
        xml.push_str("</fonts>");
        let _ = write!(xml, "<fills count=\"{}\">", fills.len());
        for f in &fills {
            xml.push_str(f);
        }
        xml.push_str("</fills>");
        let _ = write!(xml, "<borders count=\"{}\">", borders.len());
        for b in &borders {
            xml.push_str(b);
        }
        xml.push_str("</borders>");
        xml.push_str("<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>");
        let _ = write!(xml, "<cellXfs count=\"{}\">", cell_xfs.len());
        for x in &cell_xfs {
            xml.push_str(x);
        }
        xml.push_str("</cellXfs>");
        xml.push_str("<cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>");
        xml.push_str(dxfs_xml);
        xml.push_str("<tableStyles count=\"0\" defaultTableStyle=\"TableStyleMedium2\" defaultPivotStyle=\"PivotStyleLight16\"/></styleSheet>");

        let bytes = xml.into_bytes();
        let mut crc = flate2::Crc::new();
        crc.update(&bytes);
        let sig = (crc.sum(), bytes.len() as u64);
        let saved = super::delta::SavedStyles::from_direct(sig, &xf_to_sid, &wb.styles);
        Ok(Self {
            sid_to_xf,
            xml: bytes,
            sig,
            saved,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn write_value_buf(
    buf: &mut Vec<u8>,
    sheet: &Sheet,
    arrays: &HashMap<CellRef, (u32, u32, u32, u32)>,
    sst: Option<&HashMap<&str, u32>>,
    key: CellRef,
    cell: crate::store::CellView<'_>,
    xf: Option<u16>,
) -> Result<()> {
    let runs = if sheet.rich.is_empty() {
        None
    } else {
        sheet.rich.get(&(key.row, key.col)).map(Vec::as_slice)
    };
    let has_runs = runs.is_some_and(|r| !r.is_empty());

    // Zero-allocation fast path for ordinary scalar numbers, booleans, blanks, and repeated SST strings.
    if !cell.has_formula()
        && !has_runs
        && (sheet.spill_shapes.is_empty() || !sheet.spill_shapes.contains_key(&key))
    {
        match cell {
            crate::store::CellView::Number(n) if n.is_finite() => {
                let n = if n == 0.0 { 0.0 } else { n };
                buf.extend_from_slice(b"<c r=\"");
                push_a1(buf, key.row, key.col);
                if let Some(xf) = xf {
                    buf.extend_from_slice(b"\" s=\"");
                    push_u16(buf, xf);
                }
                buf.extend_from_slice(b"\"><v>");
                let iv = n as i64;
                if (iv as f64) == n && iv.unsigned_abs() < 10_000_000_000_000_000 {
                    push_i64(buf, iv);
                } else {
                    write!(buf, "{n}")?;
                }
                buf.extend_from_slice(b"</v></c>");
                return Ok(());
            }
            crate::store::CellView::Value(val) | crate::store::CellView::Cell(Cell { value: val, .. }) => {
                match val {
                    Value::Empty => {
                        if let Some(xf) = xf {
                            buf.extend_from_slice(b"<c r=\"");
                            push_a1(buf, key.row, key.col);
                            buf.extend_from_slice(b"\" s=\"");
                            push_u16(buf, xf);
                            buf.extend_from_slice(b"\"/>");
                        }
                        return Ok(());
                    }
                    Value::Number(n) if n.is_finite() => {
                        let n = if *n == 0.0 { 0.0 } else { *n };
                        buf.extend_from_slice(b"<c r=\"");
                        push_a1(buf, key.row, key.col);
                        if let Some(xf) = xf {
                            buf.extend_from_slice(b"\" s=\"");
                            push_u16(buf, xf);
                        }
                        buf.extend_from_slice(b"\"><v>");
                        let iv = n as i64;
                        if (iv as f64) == n && iv.unsigned_abs() < 10_000_000_000_000_000 {
                            push_i64(buf, iv);
                        } else {
                            write!(buf, "{n}")?;
                        }
                        buf.extend_from_slice(b"</v></c>");
                        return Ok(());
                    }
                    Value::Bool(b) => {
                        buf.extend_from_slice(b"<c r=\"");
                        push_a1(buf, key.row, key.col);
                        if let Some(xf) = xf {
                            buf.extend_from_slice(b"\" s=\"");
                            push_u16(buf, xf);
                        }
                        buf.extend_from_slice(if *b {
                            b"\" t=\"b\"><v>1</v></c>"
                        } else {
                            b"\" t=\"b\"><v>0</v></c>"
                        });
                        return Ok(());
                    }
                    Value::Text(s) => {
                        let text: &str = s.as_ref();
                        if let Some(&sst_idx) = sst.and_then(|m| m.get(text)) {
                            buf.extend_from_slice(b"<c r=\"");
                            push_a1(buf, key.row, key.col);
                            if let Some(xf) = xf {
                                buf.extend_from_slice(b"\" s=\"");
                                push_u16(buf, xf);
                            }
                            buf.extend_from_slice(b"\" t=\"s\"><v>");
                            push_u32(buf, sst_idx);
                            buf.extend_from_slice(b"</v></c>");
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let cell_cow = cell.to_cow();
    let cell = cell_cow.as_ref();

    let mut a1_buf = [0u8; 16];
    let reference = format_a1_buf(key.row, key.col, &mut a1_buf);
    let mut start = format!("<c r=\"{reference}\"");
    if let Some(xf) = xf {
        start.push_str(&format!(" s=\"{xf}\""));
    }
    let shape = sheet.spill_shapes.get(&key);
    let rect = if cell.formula.is_none() {
        None // A replaced anchor is now an ordinary literal.
    } else if let Some(&rect) = arrays.get(&key) {
        Some(rect)
    } else if let Some(&(h, w)) = shape {
        start.push_str(" cm=\"1\"");
        Some((
            key.row,
            key.col,
            key.row
                .checked_add(h.saturating_sub(1))
                .context("数组越界")?,
            key.col
                .checked_add(w.saturating_sub(1))
                .context("数组越界")?,
        ))
    } else {
        None
    };
    let definition = if let Some(rect) = rect {
        ensure!(
            (rect.0, rect.1) == (key.row, key.col)
                && rect.2 >= rect.0
                && rect.3 >= rect.1
                && rect.2 < crate::sync::MAX_ROWS
                && rect.3 < crate::sync::MAX_COLS,
            "无效的数组范围"
        );
        let formula = cell.formula.as_deref().context("缺少数组锚点公式")?;
        let formula = crate::add_fn_prefixes(formula.strip_prefix('=').unwrap_or(formula));
        let range = super::dynamic::reference(rect);
        Some(format!(
            "<f t=\"array\" ref=\"{range}\">{}</f>",
            quick_xml::escape::escape(&formula)
        ))
    } else {
        None
    };
    let xml = if cell.value == Value::Error(crate::formula::ErrKind::Spill) {
        format!(
            "{start} t=\"e\">{}<v>#SPILL!</v></c>",
            definition.context("溢出错误缺少锚点")?
        )
    } else {
        super::delta::cell_with_runs(
            start,
            cell,
            definition.as_deref(),
            runs,
        )
        .context("无效的工作表单元格")?
    };
    buf.extend_from_slice(xml.as_bytes());
    Ok(())
}

fn write_styled_blank_buf(buf: &mut Vec<u8>, key: CellRef, xf: u16) {
    buf.extend_from_slice(b"<c r=\"");
    push_a1(buf, key.row, key.col);
    buf.extend_from_slice(b"\" s=\"");
    push_u16(buf, xf);
    buf.extend_from_slice(b"\"/>");
}

fn write_sheet(
    mut out: impl Write,
    shell: &str,
    sheet: &Sheet,
    sst: Option<&HashMap<&str, u32>>,
    direct_xf: Option<&[Option<u16>]>,
) -> Result<()> {
    // Only the sparse, freshly generated template is parsed. Values are merged
    // from the ordered model iterator without materializing a second cell store.
    let doc = roxmltree::Document::parse(shell)?;
    let root = doc.root_element();
    let namespace = root.tag_name().namespace().context("新建工作表缺少命名空间")?;
    let dimension = root.children().find(|n| n.has_tag_name((namespace, "dimension")))
        .and_then(|n| n.attribute_node("ref")).context("新建工作表的 dimension 无效")?;
    let data = root.children().find(|n| n.has_tag_name((namespace, "sheetData")))
        .context("新建工作表缺少 sheetData")?;
    let (mut top, mut left, mut bottom, mut right) = sheet.used_bounds().context("流式工作表缺少单元格")?;
    let arrays: HashMap<_, _> = sheet.arrays.iter().copied().collect();
    ensure!(arrays.len()==sheet.arrays.len(),"重复的数组锚点");

    let mut formats = std::collections::HashMap::new();
    if direct_xf.is_none() {
        formats.insert(0u16, 0u16);
        for stub in data.descendants().filter(|n| n.has_tag_name((namespace, "c"))) {
            let (r,c) = stub.attribute("r").and_then(crate::sparse::parse_ref).context("无效的模板单元格")?;
            top = top.min(r); left = left.min(c); bottom = bottom.max(r); right = right.max(c);
            if sheet.contains(r, c) && let Some(id) = sheet.explicit_style_of(r, c) {
                let xf = stub.attribute("s").map(str::parse::<u16>).transpose()?.unwrap_or(0);
                formats.insert(id, xf);
            }
        }
        // The row/column dimensions in the sparse template contain the actual
        // package XF, which can differ from the model's interned style number.
        for row in data.children().filter(|n| n.has_tag_name((namespace,"row"))) {
            if let (Some(r), Some(raw)) = (row.attribute("r"),row.attribute("s"))
                && let Some(id) = r.parse::<u32>()?.checked_sub(1).and_then(|i| sheet.row_styles.get(&i)) {
                formats.insert(*id,raw.parse()?);
            }
        }
        for col in root.children().filter(|n| n.has_tag_name((namespace,"cols")))
            .flat_map(|cols| cols.children().filter(|n| n.has_tag_name((namespace,"col")))) {
            if let (Some(first),Some(last),Some(raw)) = (col.attribute("min"), col.attribute("max"), col.attribute("style")) {
                let xf = raw.parse()?;
                for c in first.parse::<u32>()?..=last.parse::<u32>()?.min(crate::sync::MAX_COLS) {
                    if let Some(id) = c.checked_sub(1).and_then(|c| sheet.col_styles.get(&c)) { formats.insert(*id,xf); }
                }
            }
        }
    } else if !sheet.styles.is_empty() {
        for (pos, &sid) in sheet.styles.iter() {
            if sid != 0
                || sheet.row_styles.contains_key(&pos.row)
                || sheet.col_styles.contains_key(&pos.col)
            {
                top = top.min(pos.row);
                left = left.min(pos.col);
                bottom = bottom.max(pos.row);
                right = right.max(pos.col);
            }
        }
    }

    let lookup_xf = |id: u16| -> Result<u16> {
        if let Some(table) = direct_xf {
            table
                .get(usize::from(id))
                .copied()
                .flatten()
                .context("直接样式表缺少单元格样式")
        } else {
            formats.get(&id).copied().context("模板缺少单元格样式")
        }
    };

    let xf_for = |key: CellRef, explicit: Option<u16>| -> Result<Option<u16>> {
        let inherited = if sheet.row_styles.is_empty() && sheet.col_styles.is_empty() {
            None
        } else {
            sheet
                .row_styles
                .get(&key.row)
                .or_else(|| sheet.col_styles.get(&key.col))
                .copied()
        };
        let sid = explicit.or(inherited);
        match sid {
            None => Ok(None),
            Some(0) if inherited.is_none() && direct_xf.is_some() => Ok(None),
            Some(id) => Ok(Some(lookup_xf(id)?)),
        }
    };

    let first = crate::model::cell_name(top, left);
    let end = crate::model::cell_name(bottom, right);
    let reference = if first == end { first } else { format!("{first}:{end}") };
    let dim = dimension.range_value();
    let mut header = String::with_capacity(data.range().start + reference.len() + 32);
    header.push_str(&shell[..dim.start]);
    header.push_str(&reference);
    header.push_str(&shell[dim.end..data.range().start]);
    if direct_xf.is_some() && !sheet.col_styles.is_empty() {
        let mut col_updates: BTreeMap<u32, Option<u16>> = BTreeMap::new();
        for (&col, &sid) in sheet.col_styles.iter() {
            col_updates.insert(col, Some(lookup_xf(sid)?));
        }
        header = crate::patch::layout_xml::styled_columns(&header, &col_updates)?;
    }

    const FLUSH_AT: usize = 128 * 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(FLUSH_AT + 16 * 1024);
    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(b"<sheetData>");

    let mut direct_row_styles: Vec<(u32, u16)> = if direct_xf.is_some() && !sheet.row_styles.is_empty() {
        let mut v: Vec<(u32, u16)> = sheet
            .row_styles
            .iter()
            .map(|(&r, &sid)| (r, sid))
            .collect();
        v.sort_unstable_by_key(|&(r, _)| r);
        v
    } else {
        Vec::new()
    };
    let mut row_style_iter = direct_row_styles.drain(..).peekable();
    let mut rows = data.children().filter(|n| n.is_element()).peekable();

    let use_merged_styles = direct_xf.is_some() && !sheet.styles.is_empty();
    let mut merged_iter = if use_merged_styles {
        Some(sheet.iter_with_styles().peekable())
    } else {
        None
    };
    let mut plain_iter = if !use_merged_styles {
        Some(sheet.iter_cells_with_explicit_style().peekable())
    } else {
        None
    };

    let peek_entry = |merged: &mut Option<std::iter::Peekable<_>>,
                      plain: &mut Option<std::iter::Peekable<_>>|
     -> Option<CellRef> {
        if let Some(it) = merged {
            it.peek().map(|(k, _, _)| *k)
        } else if let Some(it) = plain {
            it.peek().map(|(k, _, _)| *k)
        } else {
            None
        }
    };

    loop {
        let model_row = peek_entry(&mut merged_iter, &mut plain_iter).map_or(u32::MAX, |k| k.row);
        let template_row = match rows.peek() {
            Some(n) if n.has_tag_name((namespace, "row")) => n.attribute("r")
                .and_then(|r| r.parse::<u32>().ok()?.checked_sub(1)).context("无效的模板行号")?,
            Some(_) => anyhow::bail!("工作表包含未知的行记录"),
            None => u32::MAX,
        };
        let style_row = row_style_iter.peek().map_or(u32::MAX, |&(r, _)| r);
        let r = model_row.min(template_row).min(style_row);
        if r == u32::MAX {
            break;
        }
        let row = (r == template_row).then(|| rows.next().unwrap());
        let row_xf = if r == style_row {
            let (_, sid) = row_style_iter.next().unwrap();
            Some(lookup_xf(sid)?)
        } else {
            None
        };

        if let Some(n) = row {
            let raw = &shell[n.range()];
            let mut reader = quick_xml::Reader::from_str(raw);
            reader.read_event()?;
            let tag = &raw[..reader.buffer_position() as usize];
            let start = tag.strip_suffix("/>").or_else(|| tag.strip_suffix('>')).context("无效的行标签")?;
            let mut open = String::with_capacity(start.len() + 32);
            if let Some(attr) = n.attribute_node("spans") {
                let span = attr.range();
                let base = n.range().start;
                open.push_str(&start[..span.start - base]);
                open.push_str(&start[span.end - base..]);
            } else {
                open.push_str(start);
            }
            open.push('>');
            if let Some(xf) = row_xf {
                open = crate::patch::layout_xml::styled_row(&open, Some(xf))?;
            }
            buf.extend_from_slice(open.as_bytes());
        } else if let Some(xf) = row_xf {
            buf.extend_from_slice(b"<row r=\"");
            push_u32(&mut buf, r + 1);
            buf.extend_from_slice(b"\" s=\"");
            push_u16(&mut buf, xf);
            buf.extend_from_slice(b"\" customFormat=\"1\">");
        } else {
            buf.extend_from_slice(b"<row r=\"");
            push_u32(&mut buf, r + 1);
            buf.extend_from_slice(b"\">");
        }

        let emit_entry = |buf: &mut Vec<u8>,
                              key: CellRef,
                              cell: Option<crate::store::CellView<'_>>,
                              explicit: Option<u16>,
                              override_xf: Option<u16>|
         -> Result<()> {
            if let Some(cell) = cell {
                let xf = match override_xf {
                    Some(xf) => Some(xf),
                    None => xf_for(key, explicit)?,
                };
                write_value_buf(buf, sheet, &arrays, sst, key, cell, xf)?;
            } else if let Some(id) = explicit {
                if id != 0
                    || sheet.row_styles.contains_key(&key.row)
                    || sheet.col_styles.contains_key(&key.col)
                {
                    let xf = match override_xf {
                        Some(xf) => xf,
                        None => lookup_xf(id)?,
                    };
                    write_styled_blank_buf(buf, key, xf);
                }
            }
            Ok(())
        };

        for stub in row.into_iter().flat_map(|n| n.children().filter(|n| n.is_element())) {
            ensure!(stub.has_tag_name((namespace, "c")), "新建模板包含未知的行内容");
            let (sr, sc) = stub.attribute("r").and_then(crate::sparse::parse_ref)
                .context("无效的模板单元格")?;
            ensure!(sr == r, "模板单元格行号不一致");
            while let Some((key, cell, explicit)) = if let Some(it) = &mut merged_iter {
                it.next_if(|(k, _, _)| k.row == r && k.col < sc)
            } else {
                plain_iter
                    .as_mut()
                    .unwrap()
                    .next_if(|(k, _, _)| k.row == r && k.col < sc)
                    .map(|(k, c, s)| (k, Some(c), s))
            } {
                emit_entry(&mut buf, key, cell, explicit, None)?;
            }
            if let Some((key, cell, explicit)) = if let Some(it) = &mut merged_iter {
                it.next_if(|(k, _, _)| k.row == r && k.col == sc)
            } else {
                plain_iter
                    .as_mut()
                    .unwrap()
                    .next_if(|(k, _, _)| k.row == r && k.col == sc)
                    .map(|(k, c, s)| (k, Some(c), s))
            } {
                let xf = stub.attribute("s").map(str::parse::<u16>).transpose()?;
                emit_entry(&mut buf, key, cell, explicit, xf)?;
            } else if stub.attribute("s").is_none() && sheet.explicit_style_of(sr, sc) == Some(0) {
                write_styled_blank_buf(&mut buf, CellRef::new(sr, sc), 0);
            } else {
                buf.extend_from_slice(shell[stub.range()].as_bytes());
            }
        }
        if let Some(it) = &mut merged_iter {
            while let Some((key, cell, explicit)) = it.next_if(|(k, _, _)| k.row == r) {
                emit_entry(&mut buf, key, cell, explicit, None)?;
            }
        } else if let Some(it) = &mut plain_iter {
            while let Some((key, cell, explicit)) = it.next_if(|(k, _, _)| k.row == r) {
                write_value_buf(&mut buf, sheet, &arrays, sst, key, cell, xf_for(key, explicit)?)?;
            }
        }
        buf.extend_from_slice(b"</row>");
        if buf.len() >= FLUSH_AT {
            out.write_all(&buf)?;
            buf.clear();
        }
    }
    buf.extend_from_slice(b"</sheetData>");
    buf.extend_from_slice(shell[data.range().end..].as_bytes());
    out.write_all(&buf)?;
    out.flush()?;
    Ok(())
}

/// Build a single-entry ZIP in memory for a large plain/uniformly-styled worksheet by
/// parallelizing BOTH XML serialization and raw RFC 1951 deflate across CPU cores.
/// Returns `Ok(None)` if the sheet requires the general template-merge path.
fn write_sheet_parallel_zip(
    part: &str,
    shell: &str,
    sheet: &Sheet,
    sst: Option<&HashMap<&str, u32>>,
    direct_xf: &[Option<u16>],
) -> Result<Option<(Vec<u8>, Option<(u32, u64)>)>> {
    if !sheet.styles.is_empty() || !sheet.row_styles.is_empty() || sheet.len() < 32_768 {
        return Ok(None);
    }
    let doc = roxmltree::Document::parse(shell)?;
    let root = doc.root_element();
    let namespace = root.tag_name().namespace().context("新建工作表缺少命名空间")?;
    let dimension = root
        .children()
        .find(|n| n.has_tag_name((namespace, "dimension")))
        .and_then(|n| n.attribute_node("ref"))
        .context("新建工作表的 dimension 无效")?;
    let data = root
        .children()
        .find(|n| n.has_tag_name((namespace, "sheetData")))
        .context("新建工作表缺少 sheetData")?;
    if data.children().any(|n| n.is_element()) {
        return Ok(None);
    }
    let (top, left, bottom, right) = sheet.used_bounds().context("流式工作表缺少单元格")?;
    let arrays: HashMap<_, _> = sheet.arrays.iter().copied().collect();
    ensure!(arrays.len() == sheet.arrays.len(), "重复的数组锚点");

    let lookup_xf = |id: u16| -> Result<u16> {
        direct_xf
            .get(usize::from(id))
            .copied()
            .flatten()
            .context("直接样式表缺少单元格样式")
    };

    let first = crate::model::cell_name(top, left);
    let end = crate::model::cell_name(bottom, right);
    let reference = if first == end {
        first
    } else {
        format!("{first}:{end}")
    };
    let dim = dimension.range_value();
    let mut header = String::with_capacity(data.range().start + reference.len() + 32);
    header.push_str(&shell[..dim.start]);
    header.push_str(&reference);
    header.push_str(&shell[dim.end..data.range().start]);
    if !sheet.col_styles.is_empty() {
        let mut col_updates: BTreeMap<u32, Option<u16>> = BTreeMap::new();
        for (&col, &sid) in sheet.col_styles.iter() {
            col_updates.insert(col, Some(lookup_xf(sid)?));
        }
        header = crate::patch::layout_xml::styled_columns(&header, &col_updates)?;
    }
    let footer = &shell[data.range().end..];

    let mut shell_check = XmlCheckState::new_document();
    shell_check.feed(header.as_bytes());
    shell_check.feed(b"<sheetData/>");
    shell_check.feed(footer.as_bytes());
    let shell_ok = shell_check.finish_document(Some("worksheet"));

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(2, 8);
    let row_span = bottom.saturating_sub(top).saturating_add(1);
    let raw_per = (row_span + workers as u32 - 1) / workers as u32;
    let band_rows = ((raw_per + 255) / 256).max(1) * 256;
    let mut bands: Vec<(u32, u32)> = Vec::with_capacity(workers);
    let mut cur = top;
    while cur <= bottom {
        let end_r = cur.saturating_add(band_rows - 1).min(bottom);
        bands.push((cur, end_r));
        if end_r == bottom {
            break;
        }
        cur = end_r + 1;
    }
    if bands.len() < 2 {
        return Ok(None);
    }

    let num_bands = bands.len();
    let header_ref = header.as_str();
    let arrays_ref = &arrays;
    let results: std::sync::Mutex<Vec<Result<(usize, Vec<u8>, u32, u64, bool)>>> =
        std::sync::Mutex::new(Vec::with_capacity(num_bands));

    std::thread::scope(|scope| {
        for (idx, &(r1, r2)) in bands.iter().enumerate() {
            let res_ref = &results;
            scope.spawn(move || {
                let one = (|| -> Result<(usize, Vec<u8>, u32, u64, bool)> {
                    let is_first = idx == 0;
                    let is_last = idx + 1 == num_bands;
                    let est_cells = (sheet.len() / num_bands).max(1024);
                    let mut buf = Vec::with_capacity(est_cells * 36 + 4096);
                    if is_first {
                        buf.extend_from_slice(header_ref.as_bytes());
                        buf.extend_from_slice(b"<sheetData>");
                    }
                    let body_start = buf.len();
                    let has_col_styles = !sheet.col_styles.is_empty();
                    let mut open_row: Option<u32> = None;
                    for (key, cell) in sheet.cells().iter_rect_views((r1, 0, r2, u32::MAX)) {
                        if open_row != Some(key.row) {
                            if open_row.is_some() {
                                buf.extend_from_slice(b"</row>");
                            }
                            buf.extend_from_slice(b"<row r=\"");
                            push_u32(&mut buf, key.row + 1);
                            buf.extend_from_slice(b"\">");
                            open_row = Some(key.row);
                        }
                        let xf = if has_col_styles {
                            sheet
                                .col_styles
                                .get(&key.col)
                                .map(|&id| lookup_xf(id))
                                .transpose()?
                        } else {
                            None
                        };
                        write_value_buf(&mut buf, sheet, arrays_ref, sst, key, cell, xf)?;
                    }
                    if open_row.is_some() {
                        buf.extend_from_slice(b"</row>");
                    }
                    let body_end = buf.len();
                    if is_last {
                        buf.extend_from_slice(b"</sheetData>");
                        buf.extend_from_slice(footer.as_bytes());
                    }
                    let mut frag_check = XmlCheckState::new_fragment();
                    frag_check.feed(&buf[body_start..body_end]);
                    let frag_ok = frag_check.finish_fragment();
                    let raw_len = buf.len() as u64;
                    let (comp, crc) = deflate_raw_chunk(&buf, 2, is_last)?;
                    Ok((idx, comp, crc, raw_len, frag_ok))
                })();
                res_ref.lock().unwrap().push(one);
            });
        }
    });

    let mut ordered = Vec::with_capacity(num_bands);
    for item in results.into_inner().unwrap() {
        ordered.push(item?);
    }
    ordered.sort_unstable_by_key(|(idx, _, _, _, _)| *idx);
    let mut total_crc = 0u32;
    let mut total_len = 0u64;
    let mut all_ok = shell_ok;
    let mut comp_slices = Vec::with_capacity(num_bands);
    for (_, comp, crc, raw_len, frag_ok) in ordered {
        all_ok &= frag_ok;
        total_crc = if total_len == 0 {
            crc
        } else {
            crc32_combine(total_crc, crc, raw_len)
        };
        total_len += raw_len;
        comp_slices.push(comp);
    }
    let mini = build_single_entry_zip(part, total_crc, total_len, &comp_slices)?;
    Ok(Some((mini, all_ok.then_some((total_crc, total_len)))))
}

/// Write a brand-new plain (unstyled, feature-free) worksheet directly for `xlsx_delta`.
pub(super) fn write_plain_new_sheet(sheet: &Sheet, out: &mut dyn Write) -> Result<Option<(u32, u64)>> {
    let mut vw = VerifiedXmlWriter::new(out);
    let arrays = HashMap::new();
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    buf.extend_from_slice(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    );
    if let Some((top, left, bottom, right)) = sheet.used_bounds() {
        let first = crate::model::cell_name(top, left);
        let end = crate::model::cell_name(bottom, right);
        buf.extend_from_slice(b"<dimension ref=\"");
        buf.extend_from_slice(first.as_bytes());
        if first != end {
            buf.push(b':');
            buf.extend_from_slice(end.as_bytes());
        }
        buf.extend_from_slice(b"\"/><sheetData>");
        let mut open_row: Option<u32> = None;
        for (key, cell) in sheet.cells().iter_rect_views((top, 0, bottom, u32::MAX)) {
            if open_row != Some(key.row) {
                if open_row.is_some() {
                    buf.extend_from_slice(b"</row>");
                }
                buf.extend_from_slice(b"<row r=\"");
                push_u32(&mut buf, key.row + 1);
                buf.extend_from_slice(b"\">");
                open_row = Some(key.row);
            }
            write_value_buf(&mut buf, sheet, &arrays, None, key, cell, None)?;
            if buf.len() >= 128 * 1024 {
                vw.write_all(&buf)?;
                buf.clear();
            }
        }
        if open_row.is_some() {
            buf.extend_from_slice(b"</row>");
        }
        buf.extend_from_slice(b"</sheetData></worksheet>");
    } else {
        buf.extend_from_slice(b"<sheetData/></worksheet>");
    }
    vw.write_all(&buf)?;
    vw.finish(Some("worksheet"))
}

pub(super) fn has_dynamic(sheet: &Sheet) -> bool {
    let fixed: std::collections::HashSet<_> = sheet.arrays.iter().map(|(k,_)| k).collect();
    sheet.spill_shapes.keys().any(|k| !fixed.contains(k)
        && sheet.get(k.row,k.col).is_some_and(|c| c.formula.is_some()))
}

pub(super) fn write(
    path: &Path,
    wb: &Workbook,
    selected: &[bool],
    direct_styles: bool,
) -> Result<FreshResult> {
    let targets = crate::patch::sheet_targets(path)?;
    let mut ordered_sheets: Vec<(&str, &Sheet)> = Vec::new();
    let mut sheets = HashMap::new();
    for (sheet, &streamed) in wb.sheets.iter().zip(selected) {
        if streamed {
            let part = targets.get(&sheet.name).context("找不到流式工作表部件")?;
            ensure!(
                sheets.insert(part.as_str(), sheet).is_none(),
                "工作表部件重复"
            );
            ordered_sheets.push((part.as_str(), sheet));
        }
    }
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let dynamic = sheets.values().any(|s| has_dynamic(s));
    ensure!(!dynamic || zip.index_for_name(super::dynamic::PART).is_none(),"新建模板的元数据部件已占用");
    let fresh_styles = if direct_styles {
        Some(FreshStyles::build(&mut zip, wb, &ordered_sheets)?)
    } else {
        None
    };
    let direct_xf = fresh_styles.as_ref().map(|s| s.sid_to_xf.as_slice());
    let fresh_sst = FreshSst::build(&mut zip, &ordered_sheets)?;
    let need_sst_decl = fresh_sst.as_ref().is_some_and(|s| !s.existed_in_zip);
    let sst_map = fresh_sst.as_ref().map(|s| &s.indices);

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let mut trusted_parts: HashMap<String, (u32, u64)> = HashMap::new();
    if let Some(st) = &fresh_styles {
        trusted_parts.insert(STYLES_PART.to_owned(), st.sig);
    }
    if let Some(sst) = &fresh_sst {
        let mut crc = flate2::Crc::new();
        crc.update(&sst.xml);
        trusted_parts.insert(SST_PART.to_owned(), (crc.sum(), sst.xml.len() as u64));
    }

    // Pre-compress multiple streamed worksheets across cores into single-entry temp ZIPs,
    // then splice via O(1) `raw_copy_file`.
    let mut parallel_parts: HashMap<String, tempfile::TempPath> = HashMap::new();
    if ordered_sheets.len() >= 2 {
        let mut jobs: Vec<(String, String, &Sheet)> = Vec::with_capacity(ordered_sheets.len());
        for &(part, sheet) in &ordered_sheets {
            let shell = super::read_zip_text(&mut zip, part)?;
            jobs.push((part.to_owned(), shell, sheet));
        }
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .min(8)
            .min(jobs.len());
        let next_job = std::sync::atomic::AtomicUsize::new(0);
        let results: std::sync::Mutex<Vec<Result<(String, tempfile::TempPath, Option<(u32, u64)>)>>> =
            std::sync::Mutex::new(Vec::with_capacity(jobs.len()));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let jobs_ref = &jobs;
                let next_ref = &next_job;
                let res_ref = &results;
                scope.spawn(move || {
                    loop {
                        let idx = next_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((part, shell, sheet)) = jobs_ref.get(idx) else {
                            break;
                        };
                        let one = (|| -> Result<(String, tempfile::TempPath, Option<(u32, u64)>)> {
                            let (f, tp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
                            let mut zw = ZipWriter::new(BufWriter::with_capacity(128 * 1024, f));
                            zw.start_file(
                                part.as_str(),
                                SimpleFileOptions::default()
                                    .compression_method(CompressionMethod::Deflated)
                                    .compression_level(Some(2)),
                            )?;
                            let mut vw = VerifiedXmlWriter::new(&mut zw);
                            write_sheet(&mut vw, shell, sheet, sst_map, direct_xf)?;
                            let sig = vw.finish(Some("worksheet"))?;
                            zw.finish()?.flush()?;
                            Ok((part.clone(), tp, sig))
                        })();
                        res_ref.lock().unwrap().push(one);
                    }
                });
            }
        });
        for item in results.into_inner().unwrap() {
            let (part, tp, sig) = item?;
            if let Some(s) = sig {
                trusted_parts.insert(part.clone(), s);
            }
            parallel_parts.insert(part, tp);
        }
    }

    let (file, temp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
    let mut out = ZipWriter::new(BufWriter::with_capacity(256 * 1024, file));
    let mut wrote_styles = false;
    for i in 0..zip.len() {
        let part = zip.name_for_index(i).context("缺少 ZIP 条目名")?.to_owned();
        if let Some(sheet) = sheets.remove(part.as_str()) {
            if let Some(tp) = parallel_parts.remove(&part) {
                let mut part_zip = ZipArchive::new(File::open(&tp)?)?;
                out.raw_copy_file(part_zip.by_index_raw(0)?)?;
            } else {
                let shell = super::read_zip_text(&mut zip, &part)?;
                if let Some(dxf) = direct_xf
                    && let Some((mini, sig)) =
                        write_sheet_parallel_zip(&part, &shell, sheet, sst_map, dxf)?
                {
                    let mut mini_zip = ZipArchive::new(std::io::Cursor::new(mini))?;
                    out.raw_copy_file(mini_zip.by_index_raw(0)?)?;
                    if let Some(s) = sig {
                        trusted_parts.insert(part, s);
                    }
                } else {
                    out.start_file(
                        &part,
                        SimpleFileOptions::default()
                            .compression_method(CompressionMethod::Deflated)
                            .compression_level(Some(2)),
                    )?;
                    let mut vw = VerifiedXmlWriter::new(&mut out);
                    write_sheet(&mut vw, &shell, sheet, sst_map, direct_xf)?;
                    if let Some(s) = vw.finish(Some("worksheet"))? {
                        trusted_parts.insert(part, s);
                    }
                }
            }
        } else if part == STYLES_PART && fresh_styles.is_some() {
            let st = fresh_styles.as_ref().unwrap();
            out.start_file(
                STYLES_PART,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .compression_level(Some(2)),
            )?;
            out.write_all(&st.xml)?;
            wrote_styles = true;
        } else if part == SST_PART && fresh_sst.is_some() {
            let sst = fresh_sst.as_ref().unwrap();
            out.start_file(
                SST_PART,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .compression_level(Some(2)),
            )?;
            out.write_all(&sst.xml)?;
        } else if (dynamic || need_sst_decl)
            && matches!(part.as_str(), super::dynamic::RELS | super::dynamic::TYPES)
        {
            let mut xml = super::read_zip_text(&mut zip, &part)?;
            if dynamic {
                super::dynamic::declare(&mut xml, &part)?;
            }
            if need_sst_decl {
                declare_sst(&mut xml, &part)?;
            }
            out.start_file(
                part,
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )?;
            out.write_all(xml.as_bytes())?;
        } else {
            out.raw_copy_file(zip.by_index_raw(i)?)?;
        }
    }
    ensure!(sheets.is_empty(), "缺少流式工作表部件");
    if let Some(st) = &fresh_styles {
        ensure!(wrote_styles, "新建模板缺少样式表部件");
        let _ = st;
    }
    if let Some(sst) = &fresh_sst {
        if !sst.existed_in_zip {
            out.start_file(
                SST_PART,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .compression_level(Some(2)),
            )?;
            out.write_all(&sst.xml)?;
        }
    }
    if dynamic {
        out.start_file(super::dynamic::PART,SimpleFileOptions::default().compression_method(CompressionMethod::Deflated))?;
        out.write_all(super::dynamic::METADATA.as_bytes())?;
    }
    out.finish()?.flush()?;
    drop(zip);
    temp.persist(path)?;
    Ok(FreshResult {
        styles: fresh_styles.map(|s| s.saved),
        trusted_parts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    #[test]
    fn streamed_drawings_and_both_array_types_roundtrip_with_their_cached_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drawing-arrays.xlsx");
        let marked = crate::textbox::insert_drawing_tag(
            "<s:worksheet xmlns:s=\"urn:w\"><s:sheetData><s:extLst/></s:sheetData><s:tableParts/></s:worksheet>",
            "rId7",
        ).unwrap();
        let tree = roxmltree::Document::parse(&marked).unwrap();
        let tags: Vec<_> = tree
            .root_element()
            .children()
            .filter(|n| n.is_element())
            .map(|n| n.tag_name().name())
            .collect();
        assert_eq!(tags, ["sheetData", "drawing", "tableParts"]);
        let mut wb = Workbook::new_blank();
        let sheet = &mut wb.sheets[0];
        let number = |n| Cell {
            value: Value::Number(n),
            formula: None,
        };
        let formula = |text: &str| Cell {
            formula: Some(text.into()),
            ..Default::default()
        };
        for r in 0..16_384 {
            sheet.set(r, 0, number(f64::from(r)));
        }
        for r in 0..5 {
            sheet.set(r, 9, number(f64::from(2 * r)));
        }
        for r in 0..5 {
            sheet.set(r, 10, number(f64::from(3 * r)));
        }
        sheet.set(0, 1, formula("=SEQUENCE(2,2)"));
        sheet.set(0, 4, formula("=SEQUENCE(1,2)"));
        sheet.arrays.push((CellRef::new(0, 4), (0, 4, 0, 5)));
        sheet.set(0, 7, formula("=SEQUENCE(2)"));
        sheet.set(1, 7, number(109.0));
        let chart = crate::chart_write::chart_from_selection(
            sheet,
            &sheet.name,
            (0, 9, 4, 10),
            "Sales",
            crate::charts::ChartKind::Column,
        );
        sheet.charts.push(chart);
        let png = std::sync::Arc::new(vec![
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 4, 0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 252, 255, 31,
            0, 3, 3, 2, 0, 238, 86, 219, 145, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
        ]);
        sheet.pictures.push(crate::pic::Picture {
            rect: (1, 3, 9, 5),
            off: [0; 4],
            part: "xl/media/image1.png".into(),
            data: Some(png.clone()),
            ext: "png".into(),
            name: "Logo".into(),
        });
        crate::formula::recalc_all(&mut wb);
        assert!(supported(&wb.sheets[0]));
        let receipt = crate::xlsx::save_atomic_with_receipt(&path, &wb).unwrap();
        receipt.adopt_document(&mut wb, &path, true);
        assert!(!wb.sheets[0].charts[0].part.is_empty());
        let zip =
            |part| String::from_utf8(crate::fixups::read_part_file(&path, part).unwrap()).unwrap();
        let xml = zip("xl/worksheets/sheet1.xml");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let cell = |s| {
            doc.descendants()
                .find(|n| n.has_tag_name("c") && n.attribute("r") == Some(s))
                .unwrap()
        };
        for (r, cm, range) in [
            ("B1", Some("1"), "B1:C2"),
            ("E1", None, "E1:F1"),
            ("H1", Some("1"), "H1"),
        ] {
            assert_eq!(cell(r).attribute("cm"), cm);
            let f = cell(r).children().find(|n| n.has_tag_name("f")).unwrap();
            assert_eq!(f.attribute("t"), Some("array"));
            assert_eq!(f.attribute("ref"), Some(range));
        }
        let meta = zip(super::super::dynamic::PART);
        // Other producers may keep unrelated cell metadata before the dynamic record.
        let meta = meta.replace("<cellMetadata count=\"1\">","<cellMetadata count=\"2\"><bk/>");
        let xml = xml.replace("cm=\"1\"","cm=\"2\"");
        crate::fixups::write_parts(&path,&[(super::super::dynamic::PART.into(),meta.into_bytes()),
            ("xl/worksheets/sheet1.xml".into(),xml.into_bytes())]).unwrap();
        let mut reopened = crate::xlsx::load(&path).unwrap();
        let s = &reopened.sheets[0];
        assert_eq!(s.arrays, [(CellRef::new(0, 4), (0, 4, 0, 5))]);
        assert_eq!(s.spill_shapes[&CellRef::new(0, 1)], (2, 2));
        assert_eq!(s.spill_shapes[&CellRef::new(0, 7)], (0, 0));
        assert!(s.spilled.contains(&CellRef::new(1, 2)));
        assert_eq!(s.value(1, 2), Value::Number(4.0));
        assert_eq!(s.value(0, 5), Value::Number(2.0));
        assert_eq!(s.value(1, 7), Value::Number(109.0));
        assert_eq!(s.charts.len(), 1);
        assert_eq!(s.charts[0].series[0].vals, [3.0, 6.0, 9.0, 12.0]);
        assert_eq!(s.pictures.len(), 1);
        assert_eq!(
            crate::fixups::read_part_file(&path, &s.pictures[0].part).as_ref(),
            Some(&*png)
        );
        reopened.sheets[0].remove(1, 7);
        crate::formula::recalc_all(&mut reopened);
        assert_eq!(reopened.sheets[0].value(1, 7), Value::Number(2.0));
        let receipt = crate::xlsx::save_atomic_with_receipt(&path, &reopened).unwrap();
        assert!(receipt.used_cell_delta);
        let again = crate::xlsx::load(&path).unwrap();
        assert_eq!(again.sheets[0].spill_shapes[&CellRef::new(0, 7)], (2, 1));
        assert_eq!(again.sheets[0].charts.len(), 1);
        // A drawing that contains only a textbox initially has no chart/image relationships.
        let other = dir.path().join("textbox-chart.xlsx");
        let mut secondary = Workbook::new_blank();
        let s = &mut secondary.sheets[0];
        s.set(1, 0, number(1.0));
        s.set(1, 1, number(7.0));
        s.textboxes.push(crate::textbox::TextBox {
            text: "Label".into(),
            ..Default::default()
        });
        s.charts.push(crate::chart_write::chart_from_selection(
            s,
            &s.name,
            (0, 0, 1, 1),
            "Second",
            crate::charts::ChartKind::Line,
        ));
        crate::xlsx::save_atomic(&other, &secondary).unwrap();
        let back = crate::xlsx::load(&other).unwrap();
        assert_eq!(back.sheets[0].textboxes[0].text, "Label");
        assert_eq!(back.sheets[0].charts[0].series[0].vals, [7.0]);
    }

    #[test]
    fn streamed_table_notes_sparkline_and_rich_runs_survive_fresh_and_existing_atomic_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("objects.xlsx");
        let mut wb = Workbook::new_blank();
        let sheet = wb.sheet_mut(0);
        for r in 0..16_384 {
            sheet.set(r,0,Cell { value: Value::Number(f64::from(r)), formula: None });
        }
        sheet.set(0,0,Cell { value: Value::Text("Sales".into()), formula: None });
        let text = Cell { value: Value::Text("part 2".into()), formula: None };
        sheet.set(0,1,text.clone());
        let mut first = crate::richtext::RichRun::plain("part ");
        first.bold = true;
        first.font = Some("A&B \"C\"".into());
        let runs = vec![first,crate::richtext::RichRun::plain("2")];
        sheet.rich.insert((0,1),runs.clone());
        sheet.tables.push(crate::table::Table { name:"SalesTable".into(),display:"SalesTable".into(),
            range:(0,0,9,0),columns:vec!["Sales".into()],column_ids:vec![1],header_row:true, ..Default::default() });
        let note = crate::notes::Note {r:0,c:1,author:"Reviewer".into(),text:"retain comment".into()};
        sheet.notes.insert(CellRef::new(0,1),note.clone());
        let spark = crate::sparkline::Sparkline { r:4,c:2,source_sheet:Some("Sheet1".into()),rect:(1,0,3,0), ..Default::default() };
        sheet.sparklines.push(spark.clone());
        let textbox = crate::textbox::TextBox {text:"Quarterly report".into(), ..Default::default()};
        sheet.textboxes.push(textbox.clone());
        assert!(supported(sheet));
        crate::xlsx::save_atomic(&path,&wb).unwrap();
        let mut saved = crate::xlsx::load(&path).unwrap();
        assert_eq!(saved.sheets[0].rich[&(0,1)],runs);
        assert_eq!(saved.sheets[0].notes[&CellRef::new(0,1)],note);
        assert_eq!(saved.sheets[0].sparklines,[spark.clone()]);
        assert_eq!(saved.sheets[0].textboxes[0].text,textbox.text);
        assert_eq!(saved.sheets[0].tables[0].range,(0,0,9,0));
        // A formatting-only rich edit leaves the cell's searchable text identical.
        // Capture a vendor element in another row and require lexical preservation.
        let part = saved.sheets[0].source_part.clone().unwrap();
        let xml = String::from_utf8(crate::fixups::read_part_file(&path,&part).unwrap()).unwrap();
        let raw = "<?keep row?><c r=\"A16384\" t=\"n\"><v>016383.00</v></c>";
        let tagged = xml.replace("<c r=\"A16384\"><v>16383</v></c>",raw);
        assert_ne!(xml,tagged);
        crate::fixups::write_parts(&path,&[(part.clone(),tagged.into_bytes())]).unwrap();
        saved = crate::xlsx::load(&path).unwrap();
        saved.sheets[0].rich.get_mut(&(0,1)).unwrap()[1].italic = true;
        saved.sheets[0].rich_dirty = true;
        let receipt = crate::xlsx::save_atomic_with_receipt(&path,&saved).unwrap();
        assert!(receipt.used_cell_delta);
        receipt.adopt_document(&mut saved,&path,true);
        let reopened = crate::xlsx::load(&path).unwrap();
        assert!(reopened.sheets[0].rich[&(0,1)][1].italic);
        assert_eq!(reopened.sheets[0].tables[0].columns,["Sales"]);
        assert_eq!(reopened.sheets[0].notes[&CellRef::new(0,1)],note);
        assert_eq!(reopened.sheets[0].sparklines,[spark]);
        assert_eq!(reopened.sheets[0].textboxes[0].text,textbox.text);
        saved.sheets[0].rich.remove(&(0,1));
        saved.sheets[0].rich_dirty = true;
        crate::xlsx::save_atomic(&path,&saved).unwrap();
        let reopened = crate::xlsx::load(&path).unwrap();
        assert!(!reopened.sheets[0].rich.contains_key(&(0,1)));
        assert_eq!(reopened.sheets[0].get(0,1).as_deref(),Some(&text));
        let xml = String::from_utf8(crate::fixups::read_part_file(&path,&part).unwrap()).unwrap();
        assert!(xml.contains(raw));
    }

    #[test]
    fn streamed_dense_styles_inherited_defaults_filter_and_hyperlink_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("styles.xlsx");
        let mut wb = Workbook::new_blank();
        let bold = wb.styles.intern(crate::style::CellStyle { bold: true, ..Default::default() });
        let italic = wb.styles.intern(crate::style::CellStyle { italic: true, ..Default::default() });
        let quoted = wb.styles.intern(crate::style::CellStyle { strikethrough: true, ..Default::default() });
        wb.styles.set_quote(quoted, true);
        let sheet = wb.sheet_mut(0);
        for r in 5..16_389 {
            sheet.set(r, 3, Cell { value: Value::Number(r as f64), formula: None });
            sheet.set_style(r, 3, bold);
        }
        sheet.set_col_style(5, italic);
        sheet.set_row_style(7, italic);
        sheet.set(7, 4, Cell {value: Value::Number(123.), formula: None});
        sheet.set(10, 5, Cell { value: Value::Bool(true), formula: None });
        sheet.set(11, 5, Cell { value: Value::Bool(false), formula: None });
        sheet.set_style(11, 5, 0);
        sheet.set_style(12, 5, 0);
        sheet.set(14, 4, Cell { value: Value::Text("0123".into()), formula: None });
        sheet.set_style(14, 4, quoted);
        sheet.set_style(15, 4, quoted);
        sheet.filter = Some(crate::sort_filter::Filter::new(5, 3, 100, 3));
        sheet.hyperlinks.insert((9,3), crate::hyperlinks::Hyperlink { location: "Sheet1!D6".into(), ..Default::default() });
        assert!(supported(sheet));
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let back = crate::xlsx::load(&path).unwrap();
        let sh = back.sheet(0);
        assert!(back.styles.get(sh.style_of(16_388, 3)).bold);
        assert!(back.styles.get(sh.style_of(10, 5)).italic);
        assert!(back.styles.get(sh.style_of(7, 10)).italic);
        assert!(!back.styles.get(sh.style_of(11, 5)).italic);
        assert!(!back.styles.get(sh.style_of(12, 5)).italic);
        assert_eq!(sh.filter.as_ref().unwrap().range, (5, 3, 100, 3));
        assert_eq!(sh.hyperlinks[&(9,3)].location, "Sheet1!D6");
        let xml = String::from_utf8(crate::fixups::read_part_file(&path, "xl/worksheets/sheet1.xml").unwrap()).unwrap();
        assert!(xml.contains(r#"ref="D6:F16389""#), "incorrect used range");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let xf = |address| doc.descendants().find(|n| n.attribute("r") == Some(address)).unwrap().attribute("s").unwrap();
        assert_eq!(xf("E15"), xf("E16"));
        let row_xf = doc.descendants().find(|n| n.tag_name().name()=="row" && n.attribute("r")==Some("8")).unwrap().attribute("s");
        let col_xf = doc.descendants().find(|n| n.tag_name().name()=="col" && n.attribute("min")==Some("6")).unwrap().attribute("style");
        assert_eq!(Some(xf("E8")),row_xf);
        assert_eq!(Some(xf("F11")),col_xf);
        assert_ne!(xf("F11"),"0");
        assert!(back.styles.quote_of(sh.style_of(15, 4)));
    }

    #[test]
    fn streaming_keeps_sparse_xfs_formatted_blanks_and_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.xlsx");
        let mut wb = Workbook::new_blank();
        let bold = wb.styles.intern(crate::style::CellStyle { bold: true, ..Default::default() });
        let sh = &mut wb.sheets[0];
        for r in 0..16_384 {
            sh.set(r, 1, Cell { value: Value::Number(r as f64), formula: None });
        }
        sh.set(4, 25, Cell { value: Value::Number(26.0), formula: None });
        sh.set_style(4, 1, bold);
        sh.set_style(4, 0, bold);
        sh.set_style(17_000, 7, bold);
        sh.rows.set(4, 48.0);
        sh.cols.set(7, 160.0);
        sh.merges.push((3, 5, 3, 6));
        assert!(supported(sh));
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let back = crate::xlsx::load(&path).unwrap();
        let sh = &back.sheets[0];
        let xml = crate::fixups::read_part_file(&path, "xl/worksheets/sheet1.xml").unwrap();
        let doc = roxmltree::Document::parse(std::str::from_utf8(&xml).unwrap()).unwrap();
        let row = doc.descendants().find(|n| n.has_tag_name("row") && n.attribute("r") == Some("5")).unwrap();
        assert!(row.attribute("spans").is_none());
        assert_eq!(sh.value(4, 25), Value::Number(26.0));
        assert_eq!(sh.value(4, 1), Value::Number(4.0));
        assert_eq!(sh.value(16_383, 1), Value::Number(16_383.0));
        assert!(matches!(sh.value(4, 0), Value::Empty));
        for (r, c) in [(4, 1), (4, 0), (17_000, 7)] {
            assert!(back.styles.get(sh.style_of(r, c)).bold);
        }
        assert_eq!(sh.rows.size(4), 48.0);
        assert!((sh.cols.size(7) - 160.0).abs() < 1.0);
        assert!(sh.merges.contains(&(3, 5, 3, 6)));
    }

    #[test]
    fn streamed_cells_and_dom_sibling_survive_atomic_save_and_followup_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.xlsx");
        let mut wb = Workbook::new_blank();
        for r in 0..16_384 {
            wb.sheets[0].set(
                r,
                2,
                Cell {
                    value: Value::Number(f64::from(r)),
                    formula: None,
                },
            );
        }
        let text = Cell {
            value: Value::Text(" 空格 <&>\t\n".into()),
            formula: None,
        };
        let formula = Cell {
            value: Value::Number(3.0),
            formula: Some("=C4".into()),
        };
        wb.sheets[0].set(1, 3, text.clone());
        wb.sheets[0].set(4, 4, formula.clone());
        for (r, value, expr) in [
            (5, Value::Bool(true), None),
            (6, Value::Text(" cached <&> ".into()), Some("=D2")),
            (7, Value::Error(crate::formula::ErrKind::Div0), Some("=1/0")),
            (8, Value::Empty, Some("=A1")),
        ] {
            wb.sheets[0].set(r, 0, Cell { value, formula: expr.map(Into::into) });
        }
        let mut scenario = crate::scenarios::Scenario::new("Forecast");
        scenario.cells.push(((0, 2), "42".into()));
        wb.sheets[0].scenarios.push(scenario.clone());
        assert!(supported(&wb.sheets[0]));
        let mut other = Sheet::new("Mixed");
        other.set(5, 5, text.clone());
        wb.sheets.push(other);
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let part = &crate::patch::sheet_targets(&path).unwrap()["Sheet1"];
        let xml = String::from_utf8(crate::fixups::read_part_file(&path, part).unwrap()).unwrap();
        assert!(xml.contains("ref=\"A1:E16384\""));
        assert!(xml.contains("t=\"inlineStr\""));
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let children: Vec<_> = doc.root_element().children().filter(|n| n.is_element())
            .map(|n| n.tag_name().name()).collect();
        let position = |name| children.iter().position(|&child| child == name).unwrap();
        assert!(position("sheetData") < position("scenarios"));
        assert!(position("scenarios") < position("pageMargins"));
        let mut back = crate::xlsx::load(&path).unwrap();
        assert_eq!(back.sheets[0].scenarios, [scenario]);
        assert_eq!(back.sheets[0].get(16_383, 2), wb.sheets[0].get(16_383, 2));
        assert_eq!(back.sheets[0].get(1, 3).as_deref(), Some(&text));
        assert_eq!(back.sheets[0].get(4, 4).as_deref(), Some(&formula));
        for r in 5..=8 {
            assert_eq!(back.sheets[0].get(r, 0), wb.sheets[0].get(r, 0));
        }
        assert_eq!(back.sheets[1].get(5, 5).as_deref(), Some(&text));
        back.sheets[0].set(16_383, 2, formula.clone());
        crate::xlsx::save_atomic(&path, &back).unwrap();
        assert_eq!(
            crate::xlsx::load(&path).unwrap().sheets[0].get(16_383, 2).as_deref(),
            Some(&formula)
        );
    }

    #[test]
    fn parallel_multisheet_fresh_save_deduplicates_repeated_strings_into_sst() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multisheet-sst.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "DeptA".into();
        let mut s2 = Sheet::new("DeptB");
        for r in 0..16_384 {
            wb.sheets[0].set(
                r,
                0,
                Cell {
                    value: Value::Text(if r % 2 == 0 { "研发部 ".into() } else { "财务部<&>".into() }),
                    formula: None,
                },
            );
            s2.set(
                r,
                0,
                Cell {
                    value: Value::Text(if r % 3 == 0 { "研发部 ".into() } else { "运营部".into() }),
                    formula: None,
                },
            );
        }
        wb.sheets[0].set(
            0,
            1,
            Cell {
                value: Value::Text("唯一字符串".into()),
                formula: None,
            },
        );
        let mut dom_sibling = Sheet::new("SmallDom");
        dom_sibling.set(
            0,
            0,
            Cell {
                value: Value::Text("DOM专属字符串".into()),
                formula: None,
            },
        );
        wb.sheets.push(s2);
        wb.sheets.push(dom_sibling);
        crate::xlsx::save_atomic(&path, &wb).unwrap();

        let targets = crate::patch::sheet_targets(&path).unwrap();
        let xml_a = String::from_utf8(crate::fixups::read_part_file(&path, &targets["DeptA"]).unwrap()).unwrap();
        assert!(xml_a.contains("t=\"s\""), "repeated strings must use SST");
        assert!(xml_a.contains("t=\"inlineStr\""), "1-off unique string stays inline");

        let reopened = crate::xlsx::load(&path).unwrap();
        assert_eq!(reopened.sheets[0].value(0, 0), Value::Text("研发部 ".into()));
        assert_eq!(reopened.sheets[0].value(1, 0), Value::Text("财务部<&>".into()));
        assert_eq!(reopened.sheets[0].value(0, 1), Value::Text("唯一字符串".into()));
        assert_eq!(reopened.sheets[1].value(1, 0), Value::Text("运营部".into()));
        assert_eq!(reopened.sheets[2].value(0, 0), Value::Text("DOM专属字符串".into()));
    }

    #[test]
    fn parallel_rfc1951_deflate_crc_combine_and_xml_checker() {
        use std::io::Read;
        // 1. Verify crc32_combine across varied chunk sizes
        let payload: Vec<u8> = (0..120_000u32)
            .map(|i| ((i.wrapping_mul(31) ^ (i >> 7)) & 0xFF) as u8)
            .collect();
        let mut full_crc = flate2::Crc::new();
        full_crc.update(&payload);
        let expected_crc = full_crc.sum();

        for split in [0usize, 1, 7, 63, 4096, 65537, payload.len()] {
            let mut c1 = flate2::Crc::new();
            c1.update(&payload[..split]);
            let mut c2 = flate2::Crc::new();
            c2.update(&payload[split..]);
            let combined = crc32_combine(c1.sum(), c2.sum(), (payload.len() - split) as u64);
            assert_eq!(combined, expected_crc, "split={split}");
        }

        // 2. Verify multi-chunk raw RFC 1951 deflate + build_single_entry_zip roundtrip
        let chunks: Vec<&[u8]> = payload.chunks(29_999).collect();
        let mut comp_chunks = Vec::new();
        let mut total_crc = 0u32;
        let mut total_len = 0u64;
        for (i, ch) in chunks.iter().enumerate() {
            let (comp, crc) = deflate_raw_chunk(ch, 2, i + 1 == chunks.len()).unwrap();
            total_crc = if total_len == 0 {
                crc
            } else {
                crc32_combine(total_crc, crc, ch.len() as u64)
            };
            total_len += ch.len() as u64;
            comp_chunks.push(comp);
        }
        assert_eq!(total_crc, expected_crc);
        assert_eq!(total_len, payload.len() as u64);
        let mini = build_single_entry_zip("xl/worksheets/sheet1.xml", total_crc, total_len, &comp_chunks).unwrap();
        let mut za = ZipArchive::new(std::io::Cursor::new(mini)).unwrap();
        let mut out = Vec::new();
        za.by_index(0).unwrap().read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);

        // 3. Verify XmlCheckState accepts valid documents & rejects malformed ones
        let ok_xml = "\u{feff}<?xml version=\"1.0\"?><!-- c --><x:worksheet xmlns:x=\"u\"><x:sheetData><x:row r=\"1\"><x:c r=\"A1\"><x:v>42</x:v></x:c></x:row></x:sheetData></x:worksheet>";
        let mut st = XmlCheckState::new_document();
        for piece in ok_xml.as_bytes().chunks(7) {
            st.feed(piece);
        }
        assert!(st.finish_document(Some("worksheet")));
        assert!(!st.finish_document(Some("workbook")));

        for bad in [
            "<worksheet><row></worksheet>",
            "<worksheet/><extra/>",
            "text<worksheet/>",
            "<worksheet><c r=\"A1></worksheet>",
            "<worksheet><row><!-- unclosed</worksheet>",
        ] {
            let mut st = XmlCheckState::new_document();
            st.feed(bad.as_bytes());
            assert!(!st.finish_document(Some("worksheet")), "must reject: {bad}");
        }
    }
}
