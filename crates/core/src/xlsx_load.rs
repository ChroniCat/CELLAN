//! XLSX workers share one decoded string table and lock-free style lookup.
//! Large worksheet entries stream via byte-sliced row chunks into persistent workers.
#[path = "xlsx_read_stream.rs"]
mod stream;
use super::LoadDiag;
use crate::model::{Cell, CellRef, Sheet, Value};
use crate::richtext::RichRun;
use anyhow::{Context, Result, ensure};
use calamine::Reader as _;
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs::File,
    io::{Cursor, Write},
    path::Path,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicU16, Ordering},
    },
};

/// Cross-layer-1 bridge: `Value::Text(Arc::clone(s))` makes every `t="s"` SST cell O(1) zero-heap-allocation.
#[inline]
fn shared_text_value(s: &Arc<str>) -> Value {
    Value::Text(Arc::clone(s))
}

/// Fast-path parser for `<= 15`-digit ASCII integers (exact in IEEE-754 `f64`),
/// falling back to `core::num::dec2flt` (`str::parse::<f64>()`) for decimals/exponents.
#[inline]
pub(super) fn fast_parse_f64(s: &str) -> Result<f64, std::num::ParseFloatError> {
    let bytes = s.as_bytes();
    if !bytes.is_empty() && bytes.len() <= 16 {
        let (neg, digits) = match bytes[0] {
            b'-' => (true, &bytes[1..]),
            b'+' => (false, &bytes[1..]),
            _ => (false, bytes),
        };
        if !digits.is_empty() && digits.len() <= 15 {
            let mut acc = 0u64;
            let mut all_digits = true;
            for &b in digits {
                let d = b.wrapping_sub(b'0');
                if d <= 9 {
                    acc = acc * 10 + u64::from(d);
                } else {
                    all_digits = false;
                    break;
                }
            }
            if all_digits {
                return Ok(if neg { -(acc as f64) } else { acc as f64 });
            }
        }
    }
    s.parse::<f64>()
}

fn signature(zip: &mut zip::ZipArchive<File>, part: &str) -> Option<(u32, u64)> {
    let e = zip.by_index_raw(zip.index_for_name(part)?).ok()?;
    Some((e.crc32(), e.size()))
}

/// The central directory includes each part name, size, CRC and offset. Comparing
/// it before and after the load covers relationship files read by side workers too.
pub(super) fn package_stamp(path: &Path) -> Result<[u8; 32]> {
    let zip = zip::ZipArchive::new(File::open(path)?)?;
    let offset = zip.central_directory_start();
    hash_directory(&mut zip.into_inner(), offset)
}

pub(super) fn open_stamped(path: &Path) -> Result<(zip::ZipArchive<File>, [u8; 32])> {
    use std::io::{Seek, SeekFrom};
    let file = File::open(path)?;
    let mut cursor = file.try_clone()?;
    let zip = zip::ZipArchive::new(file)?;
    // Duplicated file handles share the seek position. Restore it before handing
    // the archive to the normal readers, and retain the same physical file.
    let position = cursor.stream_position()?;
    let stamp = hash_directory(&mut cursor, zip.central_directory_start())?;
    cursor.seek(SeekFrom::Start(position))?;
    Ok((zip, stamp))
}

fn hash_directory(file: &mut File, offset: u64) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset))?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 16384];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().into())
}

pub(super) struct Header {
    dynamic: std::collections::HashSet<u32>,
    pub names: Vec<String>,
    types: Vec<calamine::SheetType>,
    pub styles: Vec<calamine::Style>,
}

#[derive(Default)]
pub(super) struct WorkbookMetadata {
    pub xml: String,
    pub targets: HashMap<String, String>,
    pub header: Option<Header>,
}

pub(super) fn workbook_metadata(src: &mut zip::ZipArchive<File>) -> Result<WorkbookMetadata> {
    use std::io::Read;
    let read = |zip: &mut zip::ZipArchive<File>, name: &str| -> Result<Option<Vec<u8>>> {
        let Some(index) = zip.index_for_name(name) else { return Ok(None) };
        let mut bytes = Vec::new();
        zip.by_index(index)?.read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    };
    let Some(bytes) = read(src, "xl/workbook.xml")? else {
        return Ok(WorkbookMetadata::default());
    };
    let xml = String::from_utf8(bytes).unwrap_or_default();
    let rels = read(src, "xl/_rels/workbook.xml.rels")?;
    let targets = crate::patch::sheet_targets_xml(
        Some(&xml),
        rels.as_deref().and_then(|b| std::str::from_utf8(b).ok()),
    );
    let header = header(src, &targets, &xml, rels.as_deref())?;
    Ok(WorkbookMetadata { xml, targets, header })
}

/// Calamine's style converter also reads the entire SST when opening a book.
/// Give it just the original workbook metadata and an empty worksheet. The real
/// sheets and the SST are consumed exclusively by the shared loader below.
pub(super) fn header(
    src: &mut zip::ZipArchive<File>,
    targets: &HashMap<String, String>,
    workbook: &str,
    rels: Option<&[u8]>,
) -> Result<Option<Header>> {
    if targets.is_empty() {
        return Ok(None);
    }
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    out.start_file("xl/workbook.xml", opts)?;
    out.write_all(workbook.as_bytes())?;
    if let Some(xml) = rels {
        out.start_file("xl/_rels/workbook.xml.rels", opts)?;
        out.write_all(xml)?;
    }
    for key in ["_rels/.rels", "xl/styles.xml"] {
        if src.index_for_name(key).is_some() {
            let mut entry = src.by_name(key)?;
            out.start_file(key, opts)?;
            std::io::copy(&mut entry, &mut out)?;
        }
    }
    let Some(part) = targets.values().find(|p| src.index_for_name(p).is_some()) else {
        return Ok(None);
    };
    out.start_file(part.as_str(), opts)?;
    out.write_all(b"<worksheet><sheetData/></worksheet>")?;
    let mut book = calamine::Xlsx::new(out.finish()?)?;
    let names = book.sheet_names();
    let types = book.sheets_metadata().iter().map(|s| s.typ).collect();
    if names.iter().any(|n| !targets.contains_key(n)) {
        return Ok(None);
    }
    let name = names
        .iter()
        .find(|n| targets.get(*n) == Some(part))
        .context("工作表关系无效")?;
    let styles = book.worksheet_cells_reader(name)?.styles().to_vec();
    Ok(Some(Header {
        dynamic: if let Some(rels) = rels {
            super::dynamic::indices(src, rels)?
        } else {
            Default::default()
        },
        names,
        types,
        styles,
    }))
}

/// Concurrent lock-free XF -> StyleTable ID resolver.
/// Only interns XFs actually referenced by worksheet cells/rows/cols, preserving
/// exact `StyleTable` and `StyleMap` uniqueness semantics while eliminating `Loaded::xfs`.
pub(super) struct StyleLookup<'a> {
    cache: Vec<AtomicU16>,
    inner: Mutex<(crate::style::StyleTable, HashMap<u16, u16>)>,
    ooxml_styles: &'a [calamine::Style],
    theme_book: &'a crate::style_theme::ThemeBook,
    ext_book: &'a crate::style_ext::StyleExt,
}

impl<'a> StyleLookup<'a> {
    pub(super) fn new(
        styles: crate::style::StyleTable,
        ids: HashMap<u16, u16>,
        ooxml_styles: &'a [calamine::Style],
        theme_book: &'a crate::style_theme::ThemeBook,
        ext_book: &'a crate::style_ext::StyleExt,
    ) -> Self {
        let cap = ooxml_styles
            .len()
            .max(theme_book.xfs.len())
            .max(ext_book.xf_border.len())
            .saturating_add(16)
            .max(64);
        let cache: Vec<AtomicU16> = (0..cap).map(|_| AtomicU16::new(u16::MAX)).collect();
        for (&xf, &id) in &ids {
            if let Some(slot) = cache.get(xf as usize) {
                slot.store(id, Ordering::Relaxed);
            }
        }
        Self {
            cache,
            inner: Mutex::new((styles, ids)),
            ooxml_styles,
            theme_book,
            ext_book,
        }
    }

    #[inline]
    pub(super) fn resolve(&self, xf: u16) -> u16 {
        if let Some(slot) = self.cache.get(xf as usize) {
            let id = slot.load(Ordering::Acquire);
            if id != u16::MAX {
                return id;
            }
        }
        self.resolve_slow(xf)
    }

    #[cold]
    fn resolve_slow(&self, xf: u16) -> u16 {
        let mut guard = self.inner.lock().unwrap();
        let (styles, ids) = &mut *guard;
        let id = *ids.entry(xf).or_insert_with(|| {
            let mut cs = self
                .ooxml_styles
                .get(xf as usize)
                .map(super::convert_style)
                .unwrap_or_else(|| crate::style::CellStyle {
                    size: 11.0,
                    color: [0, 0, 0],
                    ..Default::default()
                });
            let theme_xf = self.theme_book.xfs.get(xf as usize);
            let resolved = self.theme_book.resolved.get(xf as usize);
            crate::style_theme::apply_resolved(
                &mut cs,
                theme_xf,
                resolved,
                &self.theme_book.palette,
            );
            cs.diagonal = self
                .ext_book
                .xf_border
                .get(xf as usize)
                .and_then(|id| self.ext_book.diags.get(*id as usize))
                .cloned()
                .flatten();
            let id = styles.intern(cs);
            if let Some(txf) = theme_xf {
                styles.set_theme(id, *txf);
            }
            crate::style_ext::attach(styles, self.ext_book, id, xf);
            id
        });
        if let Some(slot) = self.cache.get(xf as usize) {
            slot.store(id, Ordering::Release);
        }
        id
    }

    pub(super) fn into_inner(self) -> (crate::style::StyleTable, HashMap<u16, u16>) {
        self.inner.into_inner().unwrap()
    }
}

#[derive(Default)]
struct Strings {
    dynamic: std::collections::HashSet<u32>,
    text: TextPool,
    rich: HashMap<usize, Vec<RichRun>>,
}

// Values are retrieved by SST index while sheets decode concurrently. Pack short
// strings into slabs to avoid a String allocation per item, and cache shared `Arc<str>`
// handles lazily on demand for referenced SST slots.
#[derive(Default)]
struct TextPool {
    slabs: Vec<String>,
    ends: Vec<(usize, usize)>,
    shared: OnceLock<Vec<OnceLock<Arc<str>>>>,
}

impl TextPool {
    fn len(&self) -> usize {
        self.ends.len()
    }

    fn push(&mut self, text: String) {
        const BYTES: usize = 64 * 1024;
        if self
            .slabs
            .last()
            .is_none_or(|s| s.len().saturating_add(text.len()) > BYTES)
        {
            self.slabs.push(text);
        } else {
            self.slabs.last_mut().unwrap().push_str(&text);
        }
        self.ends
            .push((self.slabs.len() - 1, self.slabs.last().unwrap().len()));
    }

    fn push_str(&mut self, text: &str) {
        const BYTES: usize = 64 * 1024;
        if self
            .slabs
            .last()
            .is_none_or(|s| s.len().saturating_add(text.len()) > BYTES)
        {
            let mut slab = String::with_capacity(text.len().max(BYTES));
            slab.push_str(text);
            self.slabs.push(slab);
        } else {
            self.slabs.last_mut().unwrap().push_str(text);
        }
        self.ends
            .push((self.slabs.len() - 1, self.slabs.last().unwrap().len()));
    }

    fn prepare_shared(&mut self) {
        let slots = (0..self.ends.len()).map(|_| OnceLock::new()).collect();
        let _ = self.shared.set(slots);
    }

    fn get(&self, id: usize) -> Option<&str> {
        let &(page, end) = self.ends.get(id)?;
        let start = id
            .checked_sub(1)
            .and_then(|prev| self.ends.get(prev))
            .filter(|(old, _)| *old == page)
            .map_or(0, |(_, end)| *end);
        Some(&self.slabs[page][start..end])
    }

    fn get_arc(&self, id: usize) -> Option<&Arc<str>> {
        let raw = self.get(id)?;
        let slots = self.shared.get_or_init(|| {
            (0..self.ends.len()).map(|_| OnceLock::new()).collect()
        });
        let slot = slots.get(id)?;
        Some(slot.get_or_init(|| Arc::from(raw)))
    }
}

fn attr<'a>(e: &'a BytesStart<'a>, name: &str) -> Option<Cow<'a, str>> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .map(|a| a.value)
}

// ST_Xstring uses UTF-16 code-unit escapes; _x005F_x0041_ denotes the
// literal string "_x0041_". Decode once, including adjacent surrogate pairs.
fn office_text(s: &str) -> Cow<'_, str> {
    if !s.contains("_x") {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let unit = |s: &str| {
        s.as_bytes()
            .get(..7)
            .filter(|b| b.starts_with(b"_x") && b[6] == b'_')
            .and_then(|b| std::str::from_utf8(&b[2..6]).ok())
            .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|s| u16::from_str_radix(s, 16).ok())
    };
    while let Some(pos) = rest.find("_x") {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        if let Some(u) = unit(rest) {
            rest = &rest[7..];
            let mut value = u32::from(u);
            if (0xd800..=0xdbff).contains(&u) {
                if let Some(low @ 0xdc00..=0xdfff) = unit(rest) {
                    value = 0x10000 + (value - 0xd800) * 1024 + u32::from(low) - 0xdc00;
                    rest = &rest[7..];
                }
            }
            out.push(char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER));
        } else {
            out.push('_');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn content<'a>(r: &mut Reader<&'a [u8]>) -> Result<Cow<'a, str>> {
    let mut text = Cow::Borrowed("");
    loop {
        let next = match r.read_event()? {
            Event::Text(t) if !t.as_ref().contains('\r') => t.into_inner(),
            Event::CData(t) if !AsRef::<str>::as_ref(&t).contains('\r') => t.into_inner(),
            Event::Text(t) => Cow::Owned(t.xml10_content().into_owned()),
            Event::CData(t) => Cow::Owned(t.xml10_content().into_owned()),
            Event::GeneralRef(e) => {
                let s = crate::model::resolve_entity(e.as_ref().as_ref());
                ensure!(!s.is_empty(), "无效 XML 实体");
                Cow::Owned(s)
            }
            Event::End(_) => return Ok(text),
            Event::Comment(_) => continue,
            _ => anyhow::bail!("元素文本未正常结束"),
        };
        if text.is_empty() {
            text = next;
        } else {
            text.to_mut().push_str(&next);
        }
    }
}

fn string_item(r: &mut Reader<&[u8]>) -> Result<(String, Vec<RichRun>)> {
    let mut text = String::new();
    let mut runs = Vec::new();
    let mut run: Option<RichRun> = None;
    let mut in_props = false;
    loop {
        let event = r.read_event()?;
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let tag = e.local_name();
                let empty = matches!(event, Event::Empty(_));
                match tag.as_ref() {
                    "r" if !empty => run = Some(RichRun::plain("")),
                    "rPr" if !empty => in_props = true,
                    "rPh" if !empty => {
                        r.read_to_end(e.name())?;
                    }
                    "t" if !empty => {
                        let value = content(r)?;
                        let preserve = attr(e, "xml:space").as_deref() == Some("preserve");
                        let value = office_text(if preserve {
                            &value
                        } else {
                            value.trim_matches([' ', '\t', '\r', '\n'])
                        });
                        text.push_str(&value);
                        if let Some(run) = &mut run {
                            run.text.push_str(&value);
                        }
                    }
                    prop if in_props => {
                        if let Some(run) = &mut run {
                            let val = attr(e, "val");
                            let on = !matches!(val.as_deref(), Some("0" | "false"));
                            match prop {
                                "b" => run.bold = on,
                                "i" => run.italic = on,
                                "rFont" => run.font = val.map(|v| crate::model::xml_unescape(&v)),
                                "sz" => run.size = val.and_then(|v| v.parse().ok()),
                                "color" => {
                                    run.color = attr(e, "rgb").and_then(|v| {
                                        let rgb = v.get(v.len().checked_sub(6)?..)?;
                                        let v = u32::from_str_radix(rgb, 16).ok()?;
                                        Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
                                    })
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "si" | "is" => {
                    if runs.iter().all(RichRun::is_plain) {
                        runs.clear();
                    }
                    return Ok((text, runs));
                }
                "r" => {
                    if let Some(run) = run.take() {
                        runs.push(run);
                    }
                }
                "rPr" => in_props = false,
                _ => {}
            },
            Event::Eof => anyhow::bail!("字符串元素未正常结束"),
            _ => {}
        }
    }
}

pub(super) fn xml_part(zip: &mut zip::ZipArchive<File>, part: &str) -> Result<String> {
    use std::io::Read;
    let mut entry = zip
        .by_name(part)
        .with_context(|| format!("缺少部件 {part}"))?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    decode_xml_part(bytes, part)
}

pub(super) fn utf16_encoding(bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(b"<\0") {
        Some(encoding_rs::UTF_16LE)
    } else if bytes.starts_with(&[0xfe, 0xff]) || bytes.starts_with(b"\0<") {
        Some(encoding_rs::UTF_16BE)
    } else {
        None
    }
}

pub(super) fn decode_xml_part(bytes: Vec<u8>, part: &str) -> Result<String> {
    if let Some(encoding) = utf16_encoding(&bytes) {
        let (s, _, invalid) = encoding.decode(&bytes);
        ensure!(!invalid, "{part}: UTF-16 编码错误");
        let mut xml = s.into_owned();
        // Saving a modified part emits UTF-8 bytes. Its declaration must describe
        // those bytes even when the original XML was UTF-16LE/BE.
        if xml.starts_with("<?xml") {
            let mut reader = Reader::from_str(&xml);
            if let Event::Decl(decl) = reader.read_event()? {
                let version = decl.version()?;
                let standalone = decl.standalone().transpose()?;
                let decl = quick_xml::events::BytesDecl::new(
                    &version,
                    Some("UTF-8"),
                    standalone.as_deref(),
                );
                let mut writer = quick_xml::Writer::new(Vec::new());
                writer.write_event(Event::Decl(decl))?;
                let end = reader.buffer_position() as usize;
                xml.replace_range(..end, std::str::from_utf8(&writer.into_inner())?);
            }
        }
        return Ok(xml);
    }
    Ok(String::from_utf8(bytes).with_context(|| format!("{part}: 工作表 XML 编码无效"))?)
}

fn strings(zip: &mut zip::ZipArchive<File>) -> Result<Strings> {
    let mut table = Strings::default();
    if zip.index_for_name("xl/sharedStrings.xml").is_none() {
        return Ok(table);
    }
    let part = "xl/sharedStrings.xml";
    super::delta::stream::xml_input(zip.by_name(part)?, part, |input| {
        stream::string_table(input, &mut table)
    })?;
    table.text.prepare_shared();
    Ok(table)
}

#[derive(Default)]
pub(super) struct RowLayout {
    pub row_heights: Vec<(u32, f32)>,
    pub hidden_rows: Vec<u32>,
    pub row_levels: Vec<(u32, u8)>,
    pub row_xfs: Vec<(u32, u32)>,
}

impl RowLayout {
    pub(super) fn extend(&mut self, other: Self) {
        self.row_heights.extend(other.row_heights);
        self.hidden_rows.extend(other.hidden_rows);
        self.row_levels.extend(other.row_levels);
        self.row_xfs.extend(other.row_xfs);
    }

    fn apply_to(&mut self, layout: &mut crate::merges::SheetLayout) {
        layout.row_heights.append(&mut self.row_heights);
        layout.hidden_rows.append(&mut self.hidden_rows);
        layout.row_levels.append(&mut self.row_levels);
        layout.row_xfs.append(&mut self.row_xfs);
    }
}

pub(super) struct Loaded {
    pub sheet: Sheet,
    pub xfs: Vec<(u32, u32, u16)>,
    pub row_layout: RowLayout,
    pub layout: Option<crate::merges::SheetLayout>,
    pub diag: LoadDiag,
}

type MasterSpec = ((u32, u32), (u32, u32, u32, u32), String);

#[derive(Default)]
pub(super) struct SharedMasters {
    map: HashMap<u32, MasterSpec>,
}

#[derive(Default)]
struct Formulas {
    masters: HashMap<u32, MasterSpec>,
    children: Vec<((u32, u32), u32)>,
}

fn formula_range(s: &str) -> Result<(u32, u32, u32, u32)> {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let coord = |s: &str| {
        if s.contains('$') {
            crate::sparse::parse_ref(&s.replace('$', ""))
        } else {
            crate::sparse::parse_ref(s)
        }
        .filter(|&(r, c)| r < crate::sync::MAX_ROWS && c < crate::sync::MAX_COLS)
    };
    let (r, c) = coord(a).context("公式区域起点无效")?;
    let (rr, cc) = coord(b).context("公式区域终点无效")?;
    Ok((r.min(rr), c.min(cc), r.max(rr), c.max(cc)))
}

#[inline]
fn translate_shared_child(
    master_entry: &MasterSpec,
    pos: (u32, u32),
    sheet_name: &str,
) -> Result<Arc<str>> {
    let ((mr, mc), rect, master) = master_entry;
    ensure!(
        (rect.0..=rect.2).contains(&pos.0) && (rect.1..=rect.3).contains(&pos.1),
        "共享公式子单元格在声明范围外"
    );
    let translated = crate::refs::translate(
        master,
        i64::from(pos.0) - i64::from(*mr),
        i64::from(pos.1) - i64::from(*mc),
        sheet_name,
    );
    let body = translated.trim_start_matches('=');
    let mut with_eq = String::with_capacity(body.len() + 1);
    with_eq.push('=');
    with_eq.push_str(body);
    Ok(crate::normalize_formula(&with_eq).into())
}

fn cell(
    r: &mut Reader<&[u8]>,
    dst: &mut Loaded,
    table: &Strings,
    formulas: &mut Formulas,
    shared_masters: Option<&RwLock<SharedMasters>>,
    pos: (u32, u32),
    typ: &str,
    dynamic: bool,
) -> Result<Cell> {
    let mut value = Value::Empty;
    let mut formula = None;
    loop {
        let ev = r.read_event()?;
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref() {
                    "v" => {
                        let v = if empty {
                            Cow::Borrowed("")
                        } else {
                            content(r)?
                        };
                        value = match typ {
                            "s" if !v.is_empty() => {
                                let id: usize = v.trim().parse().context("无效共享字符串序号")?;
                                if let Some(runs) = table.rich.get(&id) {
                                    dst.sheet.rich.insert(pos, runs.clone());
                                }
                                let arc = table.text.get_arc(id).context("缺少共享字符串")?;
                                shared_text_value(arc)
                            }
                            "inlineStr" | "is" => continue,
                            "str" => Value::Text(office_text(&v).as_ref().into()),
                            "d" => Value::Text(v.as_ref().into()),
                            _ if v.is_empty() => Value::Empty,
                            "b" => match v.trim() {
                                "" => Value::Empty,
                                v if v == "1" || v.eq_ignore_ascii_case("true") => {
                                    Value::Bool(true)
                                }
                                v if v == "0" || v.eq_ignore_ascii_case("false") => {
                                    Value::Bool(false)
                                }
                                _ => anyhow::bail!("无效布尔值 {v}"),
                            },
                            "e" => {
                                if v == "#SPILL!" {
                                    Value::Error(crate::formula::ErrKind::Spill)
                                } else {
                                    super::data_to_value(&calamine::Data::Error(
                                        v.parse().map_err(|_| anyhow::anyhow!("无效错误值 {v}"))?,
                                    ))
                                }
                            }
                            "" => fast_parse_f64(&v)
                                .map(Value::Number)
                                .unwrap_or_else(|_| Value::Text(v.as_ref().into())),
                            "n" => Value::Number(fast_parse_f64(&v)?),
                            _ => anyhow::bail!("无法识别的单元格类型 {typ}"),
                        };
                    }
                    "is" => {
                        let (text, runs) = if empty {
                            Default::default()
                        } else {
                            string_item(r)?
                        };
                        value = Value::Text(text.into());
                        if !runs.is_empty() {
                            dst.sheet.rich.insert(pos, runs);
                        }
                    }
                    "f" => {
                        let f = if empty {
                            Cow::Borrowed("")
                        } else {
                            content(r)?
                        };
                        match attr(e, "t").as_deref() {
                            Some("shared") => {
                                let si = attr(e, "si").context("共享公式没有 si")?.parse()?;
                                if let Some(range) = attr(e, "ref") {
                                    let rect = formula_range(&range)?;
                                    ensure!(
                                        !f.is_empty()
                                            && (rect.0..=rect.2).contains(&pos.0)
                                            && (rect.1..=rect.3).contains(&pos.1),
                                        "共享公式主单元格无效"
                                    );
                                    let spec = (pos, rect, f.to_string());
                                    if let Some(global) = shared_masters {
                                        global.write().unwrap().map.entry(si).or_insert_with(|| spec.clone());
                                    }
                                    ensure!(
                                        formulas.masters.insert(si, spec).is_none(),
                                        "共享公式序号重复"
                                    );
                                } else {
                                    if let Some(master_entry) = formulas.masters.get(&si) {
                                        formula = Some(translate_shared_child(
                                            master_entry,
                                            pos,
                                            &dst.sheet.name,
                                        )?);
                                        continue;
                                    }
                                    if let Some(global) = shared_masters {
                                        if let Some(master_entry) = global.read().unwrap().map.get(&si) {
                                            formula = Some(translate_shared_child(
                                                master_entry,
                                                pos,
                                                &dst.sheet.name,
                                            )?);
                                            continue;
                                        }
                                    }
                                    formulas.children.push((pos, si));
                                    continue;
                                }
                            }
                            Some("array") => {
                                let range = attr(e, "ref").context("数组公式缺少区域")?;
                                let rect = formula_range(&range)?;
                                let anchor = CellRef::new(pos.0, pos.1);
                                if dynamic {
                                    ensure!((rect.0, rect.1) == pos, "动态数组锚点与范围不一致");
                                    dst.sheet
                                        .spill_shapes
                                        .insert(anchor, (rect.2 - rect.0 + 1, rect.3 - rect.1 + 1));
                                } else {
                                    dst.sheet.arrays.push((anchor, rect));
                                }
                            }
                            Some("dataTable") => {
                                if !dst.diag.problems.iter().any(|p| p.what == "模拟运算表公式")
                                {
                                    dst.diag.push(
                                        Some(&dst.sheet.name),
                                        dst.sheet.source_part.as_deref().unwrap_or("xl/worksheets"),
                                        "模拟运算表公式",
                                        format!(
                                            "{} 的模拟运算表参数无法完整重建，保留缓存值",
                                            crate::model::cell_name(pos.0, pos.1)
                                        ),
                                        true,
                                    );
                                }
                            }
                            Some("normal") | None => {}
                            Some(typ) => anyhow::bail!("未知公式类型 {typ}"),
                        }
                        if !f.is_empty() {
                            let body = f.trim_start_matches('=');
                            let mut with_eq = String::with_capacity(body.len() + 1);
                            with_eq.push('=');
                            with_eq.push_str(body);
                            formula = Some(crate::normalize_formula(&with_eq).into());
                        }
                    }
                    _ if !empty => {
                        r.read_to_end(e.name())?;
                    }
                    _ => {}
                }
            }
            Event::End(e) if e.local_name().as_ref() == "c" => break,
            Event::Eof => anyhow::bail!("单元格未正常结束"),
            _ => {}
        }
    }
    Ok(Cell { value, formula })
}

fn parse(
    xml: &str,
    table: &Strings,
    style_lookup: Option<&StyleLookup<'_>>,
    dst: &mut Loaded,
    typ: calamine::SheetType,
) -> Result<String> {
    let mut f = Formulas::default();
    let shell = parse_section(xml, table, style_lookup, &[], None, dst, typ, &mut f, None, 0)?;
    expand_shared(dst, f)?;
    Ok(shell)
}

type RowCells = Vec<((u32, u32), Cell)>;
type RowStyles = Vec<(CellRef, u16)>;

#[allow(clippy::too_many_arguments)]
fn parse_section(
    xml: &str,
    table: &Strings,
    style_lookup: Option<&StyleLookup<'_>>,
    col_has_xf_hint: &[bool],
    shared_masters: Option<&RwLock<SharedMasters>>,
    dst: &mut Loaded,
    typ: calamine::SheetType,
    f: &mut Formulas,
    mut batch_out: Option<(&mut RowCells, &mut RowStyles)>,
    first_row: u32,
) -> Result<String> {
    let mut r = Reader::from_str(xml);
    let mut pos = (first_row, 0u32);
    let fragment = batch_out.is_some();
    let mut in_data = fragment;
    let mut local_col_has_xf: Vec<bool> = Vec::new();
    let mut row_has_xf = false;
    let expected_root = match typ {
        calamine::SheetType::WorkSheet => "worksheet",
        calamine::SheetType::ChartSheet => "chartsheet",
        calamine::SheetType::DialogSheet => "dialogsheet",
        calamine::SheetType::MacroSheet => "macrosheet",
        calamine::SheetType::Vba => "module",
    };
    let initial_depth: usize = if fragment { 2 } else { 0 };
    let mut depth = initial_depth;
    let mut saw_data = fragment;
    let mut closed = false;
    let mut shell = String::new();
    let mut copied = 0;
    loop {
        let start = r.buffer_position() as usize;
        let event = r.read_event()?;
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(event, Event::Empty(_));
                let tag = e.local_name();
                let level = depth;
                if fragment && level == initial_depth {
                    ensure!(tag.as_ref() == "row", "行块含其他元素");
                }
                if level == 0 {
                    ensure!(
                        !closed && tag.as_ref() == expected_root,
                        "部件根元素应为 {expected_root}"
                    );
                    closed = empty;
                }
                if !empty {
                    depth += 1;
                }
                match tag.as_ref() {
                    "col" if !in_data => {
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
                            if local_col_has_xf.len() < end {
                                local_col_has_xf.resize(end, false);
                            }
                            for c in (min as usize - 1)..end {
                                local_col_has_xf[c] = true;
                            }
                        }
                    }
                    "sheetData" if level == 1 => {
                        ensure!(!saw_data, "重复的 sheetData");
                        saw_data = true;
                        in_data = !empty;
                    }
                    "row" if in_data && level == 2 => {
                        let mut explicit_row = false;
                        let mut ht = 0f32;
                        let mut custom_ht = false;
                        let mut hidden = false;
                        let mut row_s = 0u32;
                        let mut row_lv = 0u8;
                        for a in e.attributes() {
                            let a = a?;
                            match a.key.as_ref() {
                                "r" => {
                                    pos.0 = a
                                        .value
                                        .parse::<u32>()?
                                        .checked_sub(1)
                                        .context("行号为零")?;
                                    explicit_row = true;
                                }
                                "ht" => ht = a.value.parse().unwrap_or(0.0),
                                "customHeight" => {
                                    custom_ht = a.value == "1" || a.value == "true";
                                }
                                "hidden" => {
                                    hidden = a.value == "1" || a.value == "true";
                                }
                                "s" => row_s = a.value.parse().unwrap_or(0),
                                "outlineLevel" => row_lv = a.value.parse::<u8>().unwrap_or(0),
                                _ => {}
                            }
                        }
                        row_has_xf = row_s > 0;
                        if row_lv > 0 {
                            dst.row_layout.row_levels.push((pos.0, row_lv));
                        }
                        if row_s > 0 {
                            dst.row_layout.row_xfs.push((pos.0, row_s));
                        }
                        if hidden {
                            dst.row_layout.hidden_rows.push(pos.0);
                        }
                        if ht > 0.0 && custom_ht {
                            let px = crate::layout::pt_to_px(ht);
                            if (px - crate::layout::DEFAULT_ROW_H).abs() > 0.5 {
                                dst.row_layout.row_heights.push((pos.0, px));
                            }
                        }
                        dst.sheet.source_needs_dom_repair |= !explicit_row;
                        // Extract row metadata in-flight and omit `<row>` tags from `shell`
                        // so downstream layout/CF/DV/page scanners never re-parse millions of rows.
                        shell.push_str(&xml[copied..start]);
                        copied = r.buffer_position() as usize;
                        pos.1 = 0;
                        if empty {
                            pos.0 = pos.0.saturating_add(1);
                        }
                    }
                    "c" if in_data && level == 3 => {
                        let mut typ = Cow::Borrowed("");
                        let mut xf = None;
                        let mut dynamic = false;
                        let mut here = pos;
                        let mut explicit_cell = false;
                        for a in e.attributes() {
                            let a = a?;
                            match a.key.as_ref() {
                                "r" => {
                                    explicit_cell = true;
                                    here = crate::sparse::parse_ref(&a.value)
                                        .context("无效单元格坐标")?
                                }
                                "s" => xf = Some(a.value.parse::<u16>()?),
                                "cm" => dynamic = table.dynamic.contains(&a.value.parse::<u32>()?),
                                "t" => typ = a.value,
                                _ => {}
                            }
                        }
                        dst.sheet.source_needs_dom_repair |= !explicit_cell;
                        ensure!(
                            here.0 < crate::sync::MAX_ROWS && here.1 < crate::sync::MAX_COLS,
                            "单元格坐标越界"
                        );
                        if let Some(raw_xf) = xf {
                            if let Some(lookup) = style_lookup {
                                let id = lookup.resolve(raw_xf);
                                let col_styled = col_has_xf_hint
                                    .get(here.1 as usize)
                                    .copied()
                                    .unwrap_or(false)
                                    || local_col_has_xf
                                        .get(here.1 as usize)
                                        .copied()
                                        .unwrap_or(false);
                                if raw_xf != 0 || id != 0 || row_has_xf || col_styled {
                                    let key = CellRef::new(here.0, here.1);
                                    if let Some((_, ref mut styles)) = batch_out {
                                        styles.push((key, id));
                                    } else {
                                        dst.sheet.styles.insert(key, id);
                                    }
                                }
                            } else {
                                dst.xfs.push((here.0, here.1, raw_xf));
                            }
                        }
                        if !empty {
                            let cell = cell(
                                &mut r,
                                dst,
                                table,
                                f,
                                shared_masters,
                                here,
                                &typ,
                                dynamic,
                            )?;
                            if !cell.is_blank() {
                                if let Some((ref mut cells, _)) = batch_out {
                                    cells.push((here, cell));
                                } else {
                                    dst.sheet.set(here.0, here.1, cell);
                                }
                            }
                            depth -= 1;
                        }
                        pos.1 = here.1 + 1;
                        shell.push_str(&xml[copied..start]);
                        copied = r.buffer_position() as usize;
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let tag = e.local_name();
                depth = depth.checked_sub(1).context("工作表 XML 标签不匹配")?;
                if depth == 0 {
                    closed = true;
                }
                if depth == 1 && tag.as_ref() == "sheetData" {
                    in_data = false;
                }
                if depth == 2 && in_data && tag.as_ref() == "row" {
                    shell.push_str(&xml[copied..start]);
                    copied = r.buffer_position() as usize;
                    pos.0 = pos.0.saturating_add(1);
                    pos.1 = 0;
                    row_has_xf = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!((fragment || closed) && depth == initial_depth, "工作表 XML 未正常结束");
    ensure!(
        saw_data || typ != calamine::SheetType::WorkSheet,
        "工作表缺少 sheetData"
    );
    shell.push_str(&xml[copied..]);
    Ok(shell)
}

fn expand_shared(dst: &mut Loaded, f: Formulas) -> Result<()> {
    for ((r, c), si) in f.children {
        let master_entry = f.masters.get(&si).context("共享公式缺少主单元格")?;
        let formula = translate_shared_child(master_entry, (r, c), &dst.sheet.name)?;
        if let Some(existing) = dst.sheet.get_mut(r, c) {
            existing.formula = Some(formula);
        } else {
            dst.sheet.set(
                r,
                c,
                Cell {
                    value: Value::Empty,
                    formula: Some(formula),
                },
            );
        }
    }
    Ok(())
}

impl Loaded {
    fn empty(name: &str, part: &str) -> Self {
        let mut sheet = Sheet::new(name);
        sheet.source_part = Some(part.to_owned());
        sheet.loading = true;
        Self {
            sheet,
            xfs: Vec::new(),
            row_layout: RowLayout::default(),
            layout: None,
            diag: Default::default(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn worksheets(
    path: &Path,
    zip: &mut zip::ZipArchive<File>,
    header: &Header,
    targets: &HashMap<String, String>,
    style_lookup: &StyleLookup<'_>,
    dxfs: &[crate::cf::Dxf],
    dxf_ext: bool,
    has_drawing: bool,
) -> Result<Vec<Loaded>> {
    let names = &header.names;
    let mut shared = strings(zip).context("读取 xl/sharedStrings.xml 失败")?;
    shared.dynamic.clone_from(&header.dynamic);
    let signatures: Vec<_> = names
        .iter()
        .map(|n| targets.get(n).and_then(|p| signature(zip, p)))
        .collect();
    // Streaming worksheets (>= 1 MiB) use bounded row chunks rather than whole-sheet buffers,
    // so only buffered sheets (< 1 MiB) count toward the uncompressed memory cap.
    let largest_buffered = signatures
        .iter()
        .enumerate()
        .filter_map(|(idx, sig)| {
            let (_, size) = (*sig)?;
            let streamed = header.types.get(idx) == Some(&calamine::SheetType::WorkSheet)
                && size >= 1024 * 1024;
            (!streamed).then_some(size)
        })
        .max()
        .unwrap_or(1)
        .max(1);
    let cores = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8);
    let workers = cores
        .min(names.len())
        .min((128_000_000 / largest_buffered).max(1) as usize)
        .max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let run = |zip: &mut zip::ZipArchive<File>| -> Result<Vec<(usize, Loaded)>> {
        let mut sheets = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(name) = names.get(index) else { break };
            let part = targets.get(name).context("工作表缺少部件路径")?;
            ensure!(
                signature(zip, part) == signatures[index],
                "读取期间工作表 {name} 已被替换，请重新打开"
            );
            let mut loaded = Loaded::empty(name, part);
            let typ = header.types[index];
            let sheet_size = signatures[index].map_or(0, |(_, sz)| sz);
            let parsed = if typ == calamine::SheetType::WorkSheet && sheet_size >= 1024 * 1024 {
                let entry = zip.by_name(part)?;
                let mut stream_workers = (cores / workers).max(1);
                if stream_workers == 1 && cores >= 2 && sheet_size >= 4 * 1024 * 1024 {
                    stream_workers = 2;
                }
                super::delta::stream::xml_input(entry, part, |input| {
                    stream::worksheet_with_styles(
                        input,
                        &shared,
                        Some(style_lookup),
                        &mut loaded,
                        stream_workers,
                        256 * 1024,
                    )
                })
            } else {
                xml_part(zip, part)
                    .and_then(|xml| parse(&xml, &shared, Some(style_lookup), &mut loaded, typ))
            };
            match parsed {
                Ok(shell) => {
                    for (anchor, (h, w)) in loaded.sheet.spill_shapes.clone() {
                        if loaded.sheet.value(anchor.row, anchor.col)
                            == Value::Error(crate::formula::ErrKind::Spill)
                        {
                            loaded.sheet.spill_shapes.insert(anchor, (0, 0));
                        } else {
                            let tails: Vec<_> = loaded
                                .sheet
                                .iter_rect((
                                    anchor.row,
                                    anchor.col,
                                    anchor.row + h - 1,
                                    anchor.col + w - 1,
                                ))
                                .filter_map(|(k, c)| {
                                    (k != anchor && c.formula.is_none()).then_some(k)
                                })
                                .collect();
                            loaded.sheet.spilled.extend(tails);
                        }
                    }
                    let mut layout = crate::merges::sheet_layout(&shell, dxfs, dxf_ext);
                    loaded.row_layout.apply_to(&mut layout);
                    loaded.layout = Some(layout);
                    loaded.sheet.page = crate::page::parse_page_setup(&shell);
                    if has_drawing && shell.contains("drawing") {
                        loaded.sheet.textboxes =
                            crate::textbox::load_from_sheet(path, part, &shell);
                    }
                    if shell.contains("hyperlink") {
                        loaded.sheet.hyperlinks =
                            crate::hyperlinks::load_sheet(path, part, &shell).into();
                    }
                    if shell.contains("sparkline") {
                        loaded.sheet.sparklines = crate::sparkline::parse_sheet(&shell);
                        loaded.sheet.sparklines.sort_by_key(|s| (s.c, s.r));
                    }
                }
                Err(e) => loaded.diag.push(
                    Some(name),
                    part,
                    "稀疏单元格数据、公式和样式",
                    format!("{e:#}"),
                    true,
                ),
            }
            sheets.push((index, loaded));
        }
        Ok(sheets)
    };
    std::thread::scope(|scope| {
        let mut jobs = Vec::new();
        for _ in 1..workers {
            jobs.push(scope.spawn(|| run(&mut zip::ZipArchive::new(File::open(path)?)?)));
        }
        let mut sheets = run(zip)?;
        for job in jobs {
            sheets.extend(
                job.join()
                    .map_err(|_| anyhow::anyhow!("读取工作表线程异常退出"))??,
            );
        }
        sheets.sort_unstable_by_key(|(i, _)| *i);
        Ok(sheets.into_iter().map(|(_, s)| s).collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DateSystem, Workbook};

    #[test]
    fn parallel_row_chunks_match_serial_layout_shared_formulas_rich_text_and_caches() {
        let mut xml = String::from("<worksheet><sheetPr/><sheetData>");
        for row in 1..=80 {
            let f = if row == 1 {
                r#"<f t="shared" si="1" ref="B1:B80">A1+2</f>"#
            } else {
                r#"<f t="shared" si="1"/>"#
            };
            xml.push_str(&format!(
                r#"<row r="{row}" ht="24" customHeight="1"><c r="A{row}" s="2"><v>{row}</v></c><c r="B{row}">{f}<v>9</v></c><c t="inlineStr"><is><r><rPr><b/></rPr><t>字</t></r></is></c></row>"#
            ));
        }
        xml.push_str("</sheetData><extLst/></worksheet>");
        let table = Strings::default();
        let typ = calamine::SheetType::WorkSheet;
        let mut serial = Loaded::empty("Data", "sheet");
        let shell = parse(&xml, &table, None, &mut serial, typ).unwrap();
        let mut parallel = Loaded::empty("Data", "sheet");
        let row_shell = stream::worksheet(xml.as_bytes(), &table, &mut parallel, 3, 1100).unwrap();
        assert_eq!(row_shell, shell);
        assert_eq!(parallel.xfs, serial.xfs);
        assert_eq!(parallel.row_layout.row_heights, serial.row_layout.row_heights);
        assert_eq!(parallel.row_layout.row_heights.len(), 80);
        assert_eq!(parallel.sheet.rich, serial.sheet.rich);
        assert_eq!(
            parallel.sheet.iter().collect::<Vec<_>>(),
            serial.sheet.iter().collect::<Vec<_>>()
        );
        assert_eq!(parallel.sheet.get(79, 1).unwrap().formula.as_deref(), Some("=A80+2"));
        let commented = xml.replace("<sheetData>", "<sheetData><!-- <row r='200'> -->");
        let mut comments = Loaded::empty("Data", "sheet");
        let kept = stream::worksheet(commented.as_bytes(), &table, &mut comments, 3, 1100).unwrap();
        assert!(kept.contains("<!-- <row r='200'> -->"));
        let broken = xml.replace("ref=\"B1:B80\"", "ref=\"B1:B3\"");
        assert!(stream::worksheet(broken.as_bytes(), &table, &mut Loaded::empty("Data", "sheet"), 3, 1100).is_err());
        assert!(parse(&broken, &table, None, &mut Loaded::empty("Data", "sheet"), typ).is_err());
    }

    #[test]
    fn opened_utf16_sheet_streams_implicit_rows_across_batches_and_resolves_late_masters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rows.xlsx");
        super::super::save_atomic(&path, &Workbook::new_blank()).unwrap();
        let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-16"?><s:worksheet xmlns:s="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><s:sheetData>"#);
        let filler = "文".repeat(1100);
        for row in 1..=550 {
            let f = if row == 550 {
                r#"<s:f t="shared" si="3" ref="B1:B550">A550+1</s:f>"#
            } else {
                r#"<s:f t="shared" si="3"/>"#
            };
            xml.push_str(&format!(
                r#"<s:row ht="23"><s:c><s:v>{row}</s:v></s:c><s:c>{f}<s:v>1</s:v></s:c><s:c t="inlineStr"><s:is><s:t>{filler}</s:t></s:is></s:c></s:row>"#
            ));
        }
        xml.push_str("</s:sheetData></s:worksheet>");
        let bytes: Vec<_> = [0xfe, 0xff]
            .into_iter()
            .chain(xml.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        crate::fixups::write_parts(&path, &[("xl/worksheets/sheet1.xml".into(), bytes)]).unwrap();
        let (wb, diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(), "{:?}", diag.problems);
        let sheet = wb.sheet(0);
        for row in [0, 202, 549] {
            assert_eq!(sheet.value(row, 0), Value::Number((row + 1) as f64));
            assert_eq!(
                sheet.get(row, 1).unwrap().formula.as_deref().unwrap(),
                format!("=A{}+1", row + 1)
            );
            assert_eq!(sheet.value(row, 2), Value::Text(filler.as_str().into()));
        }
    }

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("months.xlsx");
        let mut wb = Workbook::new_blank();
        wb.date_system = DateSystem::D1904;
        wb.sheets.clear();
        let mut date = wb.styles.get(0).clone();
        date.format = "yyyy-mm-dd".into();
        let id = wb.styles.intern(date);
        for i in 1..=12 {
            let mut sh = Sheet::new(format!("Month {i:02}"));
            sh.set(
                0,
                1,
                Cell {
                    value: Value::Text("Amount".into()),
                    formula: None,
                },
            );
            sh.set(
                1,
                0,
                Cell {
                    value: Value::Number(1.),
                    formula: None,
                },
            );
            sh.set_style(1, 0, id);
            sh.set(
                1,
                1,
                Cell {
                    value: Value::Number(i as f64),
                    formula: None,
                },
            );
            wb.sheets.push(sh);
        }
        super::super::save(&path, &wb).unwrap();
        (dir, path)
    }

    #[test]
    fn indexed_strings_runs_forward_shared_formulas_implicit_cells_and_utf16() {
        let (_dir, path) = fixture();
        let parts = crate::patch::sheet_targets(&path).unwrap();
        let sst = r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si/><si><r><rPr><b/></rPr><t>同文</t></r><rPh><t>phonetic</t></rPh></si><si><t>同文</t></si><si><r><rPr><i/></rPr><t>同文</t></r></si></sst>"#;
        let sheet = r#"<x:worksheet xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><x:sheetData><x:row r="4"><x:c t="s"><x:v>1</x:v></x:c><x:c t="s"><x:v>2</x:v></x:c><x:c t="s"><x:v>3</x:v></x:c><x:c t="inlineStr"><x:is><x:r><x:rPr><x:b/></x:rPr><x:t xml:space="preserve"> A&amp;B&#x20;_xD83D__xDE00_ </x:t></x:r></x:is></x:c><x:c t="d"><x:v>2026-06-01</x:v></x:c></x:row><x:row/><x:row hidden="1"><x:c/><x:c><x:f t="shared" si="0"/><x:v>12</x:v></x:c></x:row><x:row><x:c r="B7"><x:f t="shared" si="0" ref="B6:B7">A7+$D$2</x:f><x:v>13</x:v></x:c><x:c t="e"><x:v>#REF!</x:v></x:c><x:c t="b"><x:v> True </x:v></x:c><x:c><x:f t="array" ref="E7:E8">A7*2</x:f><x:v>4</x:v></x:c></x:row></x:sheetData></x:worksheet>"#;
        let utf16: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(sheet.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        crate::fixups::write_parts(
            &path,
            &[
                ("xl/sharedStrings.xml".into(), sst.as_bytes().to_vec()),
                (parts["Month 10"].clone(), utf16),
            ],
        )
        .unwrap();
        let (wb, diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(), "{:?}", diag.problems);
        let sh = &wb.sheets[9];
        assert_eq!(wb.date_system, DateSystem::D1904);
        assert_eq!(sh.value(3, 0), Value::Text("同文".into()));
        assert!(sh.rich[&(3, 0)][0].bold);
        assert!(!sh.rich.contains_key(&(3, 1)));
        assert!(sh.rich[&(3, 2)][0].italic);
        assert_eq!(sh.rich[&(3, 3)][0].text, " A&B 😀 ");
        assert_eq!(sh.value(3, 4), Value::Text("2026-06-01".into()));
        assert_eq!(sh.get(5, 1).unwrap().formula.as_deref(), Some("=A6+$D$2"));
        assert_eq!(sh.value(5, 1), Value::Number(12.));
        assert_eq!(sh.value(6, 2), Value::Error(crate::formula::ErrKind::Ref));
        assert_eq!(sh.value(6, 3), Value::Bool(true));
        assert!(sh.arrays.contains(&(CellRef::new(6, 4), (6, 4, 7, 4))));
        assert!(sh.hidden_rows_user.contains(&5));
    }

    #[test]
    fn rejects_mismatched_parts_and_guards_unsupported_what_if_metadata() {
        let (_dir, path) = fixture();
        let targets = crate::patch::sheet_targets(&path).unwrap();
        let cases = [
            ("Month 01", "<other/>"),
            ("Month 02", "<worksheet/>"),
            (
                "Month 03",
                "<worksheet><sheetData/></worksheet><worksheet><sheetData/></worksheet>",
            ),
            (
                "Month 04",
                r#"<worksheet><sheetData><row><c><f t="dataTable" ref="A1:B2" r1="B3"/><v>22</v></c><c t="str"><v/></c><c t="str"><v></v></c></row></sheetData></worksheet>"#,
            ),
            (
                "Month 05",
                r#"<worksheet><sheetData><row><c><f t="shared" si="1" ref="A1:A1">A2</f></c><c><f t="shared" si="1"/></c></row></sheetData></worksheet>"#,
            ),
        ];
        let parts: Vec<_> = cases
            .iter()
            .map(|(name, xml)| (targets[*name].clone(), xml.as_bytes().to_vec()))
            .collect();
        crate::fixups::write_parts(&path, &parts).unwrap();
        let (wb, diag) = super::super::load_diag(&path).unwrap();
        for (name, _) in cases {
            assert!(
                diag.problems
                    .iter()
                    .any(|p| p.fatal && p.sheet.as_deref() == Some(name)),
                "{name} {:?}",
                diag.problems
            );
        }
        let sh = &wb.sheets[3];
        assert_eq!(sh.value(0, 0), Value::Number(22.));
        assert_eq!(sh.value(0, 1), Value::Text("".into()));
        assert_eq!(sh.value(0, 2), Value::Text("".into()));
        assert!(wb.load_incomplete);
    }

    #[test]
    fn editing_utf16_sheet_and_shared_strings_transcodes_both_declarations_in_place() {
        let (_dir, path) = fixture();
        let target = crate::patch::sheet_targets(&path).unwrap()["Month 10"].clone();
        let sst = r#"<?xml version="1.0" encoding="UTF-16" standalone="no"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="1"><si><t>原值</t></si></sst>"#;
        let sheet = r#"<?xml version="1.0" encoding="UTF-16LE" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>0</v></c></row></sheetData></worksheet>"#;
        let source_le: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(sheet.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let shared_be: Vec<u8> = [0xfe, 0xff]
            .into_iter()
            .chain(sst.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        crate::fixups::write_parts(
            &path,
            &[
                (target.clone(), source_le),
                ("xl/sharedStrings.xml".into(), shared_be),
            ],
        )
        .unwrap();
        let (mut book, diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(), "{:?}", diag.problems);
        assert_eq!(book.sheets[9].value(0, 1), Value::Text("原值".into()));
        book.sheets[9].set(
            0,
            0,
            Cell {
                value: Value::Text("改值 & 中".into()),
                formula: None,
            },
        );
        book.dirty = true;
        super::super::save(&path, &book).unwrap();
        for (part, standalone) in [(target.as_str(), "yes"), ("xl/sharedStrings.xml", "no")] {
            let raw = crate::fixups::read_part_file(&path, part).unwrap();
            let xml = String::from_utf8(raw).unwrap();
            let decl = xml.split_once("?>").unwrap().0;
            assert!(decl.contains(r#"encoding="UTF-8""#), "{part} {decl}");
            assert!(
                decl.contains(&format!(r#"standalone="{standalone}""#)),
                "{part} {decl}"
            );
        }
        let (again, diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(), "{:?}", diag.problems);
        assert_eq!(again.sheets[9].value(0, 0), Value::Text("改值 & 中".into()));
        assert_eq!(again.sheets[9].value(0, 1), Value::Text("原值".into()));
    }

    #[test]
    fn twelfth_sheet_chart_uses_its_own_values_and_workbook_calendar_without_cached_series() {
        let (_dir, path) = fixture();
        let wb = super::super::load(&path).unwrap();
        let sh = &wb.sheets[11];
        let mut chart = crate::chart_write::chart_from_selection(
            sh,
            &sh.name,
            (0, 0, 1, 1),
            "December",
            crate::charts::ChartKind::Column,
        );
        chart.series[0].cats.clear();
        chart.series[0].vals.clear();
        crate::chart_write::insert_chart(&path, &sh.name, &chart).unwrap();
        let (wb, diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(), "{:?}", diag.problems);
        assert!(wb.sheets[..11].iter().all(|s| s.charts.is_empty()));
        let series = &wb.sheets[11].charts[0].series[0];
        assert_eq!(series.vals, vec![12.]);
        assert_eq!(series.cats, vec!["1904-01-02"]);
    }
}
