//! XLSX workers share one decoded string table. Large worksheet entries are
//! inflated once and decoded in batches of complete rows.
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
};

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
        Some(&xml), rels.as_deref().and_then(|b| std::str::from_utf8(b).ok()),
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
        dynamic: if let Some(rels) = rels { super::dynamic::indices(src, rels)? } else { Default::default() },
        names,
        types,
        styles,
    }))
}

#[derive(Default)]
struct Strings {
    dynamic: std::collections::HashSet<u32>,
    text: TextPool,
    rich: HashMap<usize, Vec<RichRun>>,
}

// Values are retrieved by SST index while sheets decode concurrently. Pack short
// strings into slabs to avoid a String allocation per item. Growing the table
// never reallocates/copies all previously decoded UTF-8 text.
#[derive(Default)]
struct TextPool {
    slabs: Vec<String>,
    ends: Vec<(usize,usize)>,
}

impl TextPool {
    fn push(&mut self, text: String) {
        const BYTES: usize = 64 * 1024;
        if self.slabs.last().is_none_or(|s| s.len().saturating_add(text.len()) > BYTES) {
            self.slabs.push(text);
        } else {
            self.slabs.last_mut().unwrap().push_str(&text);
        }
        self.ends.push((self.slabs.len()-1,self.slabs.last().unwrap().len()));
    }

    fn get(&self, id: usize) -> Option<&str> {
        let &(page,end) = self.ends.get(id)?;
        let start = id.checked_sub(1).and_then(|prev| self.ends.get(prev))
            .filter(|(old,_)| *old == page).map_or(0,|(_,end)| *end);
        Some(&self.slabs[page][start..end])
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
    super::delta::stream::xml_input(zip.by_name(part)?,part,|input| stream::string_table(input,&mut table))?;
    Ok(table)
}

pub(super) struct Loaded {
    pub sheet: Sheet,
    pub xfs: Vec<(u32, u32, u16)>,
    pub layout: Option<crate::merges::SheetLayout>,
    pub diag: LoadDiag,
}

#[derive(Default)]
struct Formulas {
    masters: HashMap<u32, ((u32, u32), (u32, u32, u32, u32), String)>,
    children: Vec<((u32, u32), u32)>,
}

fn formula_range(s: &str) -> Result<(u32, u32, u32, u32)> {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let coord = |s: &str| {
        crate::sparse::parse_ref(&s.replace('$', ""))
            .filter(|&(r, c)| r < crate::sync::MAX_ROWS && c < crate::sync::MAX_COLS)
    };
    let (r, c) = coord(a).context("公式区域起点无效")?;
    let (rr, cc) = coord(b).context("公式区域终点无效")?;
    Ok((r.min(rr), c.min(cc), r.max(rr), c.max(cc)))
}

fn cell(
    r: &mut Reader<&[u8]>,
    dst: &mut Loaded,
    table: &Strings,
    formulas: &mut Formulas,
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
                                Value::Text(
                                    table
                                        .text
                                        .get(id)
                                        .context("缺少共享字符串")?
                                        .into(),
                                )
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
                            "" => v
                                .parse()
                                .map(Value::Number)
                                .unwrap_or_else(|_| Value::Text(v.as_ref().into())),
                            "n" => Value::Number(v.parse()?),
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
                                    ensure!(
                                        formulas
                                            .masters
                                            .insert(si, (pos, rect, f.to_string()))
                                            .is_none(),
                                        "共享公式序号重复"
                                    );
                                } else {
                                    formulas.children.push((pos, si));
                                    continue;
                                }
                            }
                            Some("array") => {
                                let range = attr(e, "ref").context("数组公式缺少区域")?;
                                let rect = formula_range(&range)?;
                                let anchor = CellRef::new(pos.0, pos.1);
                                if dynamic {
                                    ensure!((rect.0,rect.1) == pos,"动态数组锚点与范围不一致");
                                    dst.sheet.spill_shapes.insert(anchor,(rect.2-rect.0+1,rect.3-rect.1+1));
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
                            formula = Some(
                                crate::normalize_formula(&format!(
                                    "={}",
                                    f.trim_start_matches('=')
                                ))
                                .into(),
                            );
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

fn parse(xml: &str, table: &Strings, dst: &mut Loaded, typ: calamine::SheetType) -> Result<String> {
    let mut f = Formulas::default();
    let shell = parse_section(xml, table, dst, typ, &mut f, None, 0)?;
    expand_shared(dst, f)?;
    Ok(shell)
}

type RowCells = Vec<((u32, u32), Cell)>;

fn parse_section(xml: &str, table: &Strings, dst: &mut Loaded, typ: calamine::SheetType,
    f: &mut Formulas, mut rows: Option<&mut RowCells>, first_row: u32) -> Result<String> {
    let mut r = Reader::from_str(xml);
    let mut pos = (first_row, 0u32);
    let fragment = rows.is_some();
    let mut in_data = fragment;
    let mut keep_row = false;
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
                if fragment && level == initial_depth { ensure!(tag.as_ref() == "row", "行块含其他元素"); }
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
                    "sheetData" if level == 1 => {
                        ensure!(!saw_data, "重复的 sheetData");
                        saw_data = true;
                        in_data = !empty;
                    }
                    "row" if in_data && level == 2 => {
                        let mut explicit_row = false;
                        keep_row = false;
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
                                "s" | "ht" | "hidden" | "outlineLevel" => keep_row = true,
                                _ => {}
                            }
                        }
                        dst.sheet.source_needs_dom_repair |= !explicit_row;
                        if !keep_row {
                            shell.push_str(&xml[copied..start]);
                            copied = r.buffer_position() as usize;
                        } else if !explicit_row {
                            let end = r.buffer_position() as usize - if empty { 2 } else { 1 };
                            shell.push_str(&xml[copied..end]);
                            use std::fmt::Write;
                            write!(shell, " r=\"{}\"", pos.0 + 1)?;
                            copied = end;
                        }
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
                        if let Some(id) = xf {
                            dst.xfs.push((here.0, here.1, id));
                        }
                        if !empty {
                            let cell = cell(&mut r, dst, table, f, here, &typ, dynamic)?;
                            if !cell.is_blank() {
                                if let Some(cells) = rows.as_deref_mut() { cells.push((here, cell)); }
                                else { dst.sheet.set(here.0, here.1, cell); }
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
                    if !keep_row {
                        shell.push_str(&xml[copied..start]);
                        copied = r.buffer_position() as usize;
                    }
                    pos.0 = pos.0.saturating_add(1);
                    pos.1 = 0;
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
        let ((mr, mc), rect, master) = f.masters.get(&si).context("共享公式缺少主单元格")?;
        ensure!(
            (rect.0..=rect.2).contains(&r) && (rect.1..=rect.3).contains(&c),
            "共享公式子单元格在声明范围外"
        );
        let f = crate::refs::translate(
            master,
            i64::from(r) - i64::from(*mr),
            i64::from(c) - i64::from(*mc),
            &dst.sheet.name,
        );
        let mut cc = dst.sheet.get(r, c).cloned().unwrap_or_default();
        cc.formula =
            Some(crate::normalize_formula(&format!("={}", f.trim_start_matches('='))).into());
        dst.sheet.set(r, c, cc);
    }
    Ok(())
}

impl Loaded {
    fn empty(name: &str, part: &str) -> Self {
        let mut sheet = Sheet::new(name);
        sheet.source_part = Some(part.to_owned());
        sheet.loading = true;
        Self { sheet, xfs: Vec::new(), layout: None, diag: Default::default() }
    }
}

pub(super) fn worksheets(
    path: &Path,
    zip: &mut zip::ZipArchive<File>,
    header: &Header,
    targets: &HashMap<String, String>,
    dxfs: &[crate::cf::Dxf],
    dxf_ext: bool,
) -> Result<Vec<Loaded>> {
    let names = &header.names;
    let mut shared = strings(zip).context("读取 xl/sharedStrings.xml 失败")?;
    shared.dynamic.clone_from(&header.dynamic);
    let signatures: Vec<_> = names
        .iter()
        .map(|n| targets.get(n).and_then(|p| signature(zip, p)))
        .collect();
    let largest = signatures
        .iter()
        .flatten()
        .map(|(_, size)| *size)
        .max()
        .unwrap_or(1)
        .max(1);
    // Limit simultaneous uncompressed worksheet buffers, independently of selected CPU count.
    let cores = std::thread::available_parallelism().map_or(1, usize::from).min(4);
    let workers = cores.min(names.len())
        .min((128_000_000 / largest).max(1) as usize)
        .max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let run = |zip: &mut zip::ZipArchive<File>| -> Result<Vec<(usize, Loaded)>> {
        let mut sheets = Vec::new();
        loop {
            let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(name) = names.get(index) else { break };
            let part = targets.get(name).context("工作表缺少部件路径")?;
            ensure!(
                signature(zip, part) == signatures[index],
                "读取期间工作表 {name} 已被替换，请重新打开"
            );
            let mut loaded = Loaded::empty(name, part);
            let typ = header.types[index];
            let parsed = if typ == calamine::SheetType::WorkSheet
                && signatures[index].is_some_and(|(_, size)| size >= 1024 * 1024) {
                let entry = zip.by_name(part)?;
                super::delta::stream::xml_input(entry, part, |input|
                    stream::worksheet(input, &shared, &mut loaded, cores / workers, 256 * 1024))
            } else {
                xml_part(zip, part).and_then(|xml| parse(&xml, &shared, &mut loaded, typ))
            };
            match parsed {
                Ok(shell) => {
                    for (anchor,(h,w)) in loaded.sheet.spill_shapes.clone() {
                        if loaded.sheet.value(anchor.row,anchor.col) == Value::Error(crate::formula::ErrKind::Spill) {
                            loaded.sheet.spill_shapes.insert(anchor,(0,0));
                        } else {
                            let tails: Vec<_> = loaded.sheet.iter_rect((anchor.row,anchor.col,anchor.row+h-1,anchor.col+w-1))
                                .filter_map(|(k,c)| (k != anchor && c.formula.is_none()).then_some(k)).collect();
                            loaded.sheet.spilled.extend(tails);
                        }
                    }
                    loaded.layout =
                        Some(crate::merges::sheet_layout(&shell, dxfs, dxf_ext));
                    loaded.sheet.page = crate::page::parse_page_setup(&shell);
                    loaded.sheet.textboxes =
                        crate::textbox::load_from_sheet(path, part, &shell);
                    loaded.sheet.hyperlinks =
                        crate::hyperlinks::load_sheet(path, part, &shell).into();
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
            let f = if row == 1 { r#"<f t="shared" si="1" ref="B1:B80">A1+2</f>"# }
                else { r#"<f t="shared" si="1"/>"# };
            xml.push_str(&format!(r#"<row r="{row}" ht="24"><c r="A{row}" s="2"><v>{row}</v></c><c r="B{row}">{f}<v>9</v></c><c t="inlineStr"><is><r><rPr><b/></rPr><t>字</t></r></is></c></row>"#));
        }
        xml.push_str("</sheetData><extLst/></worksheet>");
        let table = Strings::default();
        let typ = calamine::SheetType::WorkSheet;
        let mut serial = Loaded::empty("Data", "sheet");
        let shell = parse(&xml, &table, &mut serial, typ).unwrap();
        let mut parallel = Loaded::empty("Data","sheet");
        let row_shell = stream::worksheet(xml.as_bytes(), &table, &mut parallel, 3, 1100).unwrap();
        assert_eq!(row_shell, shell);
        assert_eq!(parallel.xfs, serial.xfs);
        assert_eq!(parallel.sheet.rich, serial.sheet.rich);
        assert_eq!(parallel.sheet.iter().collect::<Vec<_>>(), serial.sheet.iter().collect::<Vec<_>>());
        assert_eq!(parallel.sheet.get(79, 1).unwrap().formula.as_deref(), Some("=A80+2"));
        let commented = xml.replace("<sheetData>", "<sheetData><!-- <row r='200'> -->");
        let mut comments = Loaded::empty("Data","sheet");
        let kept = stream::worksheet(commented.as_bytes(), &table, &mut comments, 3, 1100).unwrap();
        assert!(kept.contains("<!-- <row r='200'> -->"));
        let broken = xml.replace("ref=\"B1:B80\"", "ref=\"B1:B3\"");
        assert!(stream::worksheet(broken.as_bytes(), &table, &mut Loaded::empty("Data","sheet"), 3, 1100).is_err());
        assert!(parse(&broken, &table, &mut Loaded::empty("Data", "sheet"), typ).is_err());
    }

    #[test]
    fn opened_utf16_sheet_streams_implicit_rows_across_batches_and_resolves_late_masters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rows.xlsx");
        super::super::save_atomic(&path,&Workbook::new_blank()).unwrap();
        let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-16"?><s:worksheet xmlns:s="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><s:sheetData>"#);
        let filler = "文".repeat(1100);
        for row in 1..=550 {
            let f = if row == 550 { r#"<s:f t="shared" si="3" ref="B1:B550">A550+1</s:f>"# }
                else { r#"<s:f t="shared" si="3"/>"# };
            xml.push_str(&format!(r#"<s:row ht="23"><s:c><s:v>{row}</s:v></s:c><s:c>{f}<s:v>1</s:v></s:c><s:c t="inlineStr"><s:is><s:t>{filler}</s:t></s:is></s:c></s:row>"#));
        }
        xml.push_str("</s:sheetData></s:worksheet>");
        let bytes: Vec<_> = [0xfe,0xff].into_iter().chain(xml.encode_utf16().flat_map(u16::to_be_bytes)).collect();
        crate::fixups::write_parts(&path,&[("xl/worksheets/sheet1.xml".into(),bytes)]).unwrap();
        let (wb,diag) = super::super::load_diag(&path).unwrap();
        assert!(!diag.has_fatal(),"{:?}",diag.problems);
        let sheet = wb.sheet(0);
        for row in [0,202,549] {
            assert_eq!(sheet.value(row,0),Value::Number((row+1) as f64));
            assert_eq!(sheet.get(row,1).unwrap().formula.as_deref().unwrap(),format!("=A{}+1",row+1));
            assert_eq!(sheet.value(row,2), Value::Text(filler.as_str().into()));
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
