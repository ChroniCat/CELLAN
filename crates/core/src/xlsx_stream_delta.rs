//! Stream source XML directly into the private output ZIP. Keep only the bytes
//! of the current row so unedited markup can be copied with its original spelling.
use super::{
    Changes, SharedFormulaState, StyleChanges, attr, expanded_tag, merge_edit, new_cell, row_edits,
    write_spans,
};
use crate::{CellRef, Sheet, store::Store};
use anyhow::{Context, Result};
use quick_xml::{Reader, events::Event};
use std::{
    collections::{BTreeSet,BTreeMap},
    io::{self, BufRead, BufReader, Read, Write},
};

#[path = "xlsx_utf16_stream.rs"]
mod utf16;
#[path = "xlsx_sst_merge.rs"]
pub(in crate::xlsx) mod sst;

pub(in crate::xlsx) fn xml_input<T>(
    mut input: impl Read,
    part: &str,
    process: impl FnOnce(&mut dyn Read) -> Result<T>,
) -> Result<T> {
    let mut prefix = Vec::with_capacity(2);
    input.by_ref().take(2).read_to_end(&mut prefix)?;
    let encoding = super::super::sheets::utf16_encoding(&prefix);
    let mut stream = io::Cursor::new(prefix).chain(input);
    if let Some(encoding) = encoding {
        utf16::process(BufReader::new(stream), encoding, part, process)
    } else {
        process(&mut stream)
    }
}

pub(in crate::xlsx) struct Window<R> {
    input: BufReader<R>,
    bytes: Vec<u8>,
    first: usize,
    offset: u64,
}

impl<R: Read> Window<R> {
    pub(in crate::xlsx) fn start(input: R, out: &mut impl Write) -> Result<Reader<Self>> {
        let mut input = BufReader::new(input);
        if input.fill_buf()?.starts_with(b"\xef\xbb\xbf") {
            out.write_all(b"\xef\xbb\xbf")?;
            input.consume(3);
        }
        Ok(Reader::from_reader(Self {
            input,
            bytes: Vec::new(),
            first: 0,
            offset: 0,
        }))
    }

    pub(in crate::xlsx) fn through(&mut self, end: u64) -> Result<&[u8]> {
        let len = usize::try_from(end.checked_sub(self.offset).context("XML 读取位置倒退")?)?;
        let start = self.first;
        self.first = start.checked_add(len).context("XML 位置溢出")?;
        self.offset = end;
        self.bytes
            .get(start..self.first)
            .context("XML 字节范围无效")
    }

    fn copy_rest(mut self, out: &mut impl Write) -> Result<()> {
        out.write_all(&self.bytes[self.first..])?;
        io::copy(&mut self.input, out)?;
        Ok(())
    }
}

impl<R: Read> BufRead for Window<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.input.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        if self.first == self.bytes.len() {
            self.bytes.clear();
            self.first = 0;
        } else if self.first >= 8192 {
            self.bytes.drain(..self.first);
            self.first = 0;
        }
        self.bytes.extend_from_slice(&self.input.buffer()[..amount]);
        self.input.consume(amount);
    }
}

impl<R: Read> Read for Window<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.input.fill_buf()?.read(out)?;
        self.consume(n);
        Ok(n)
    }
}

/// Split a source worksheet into its metadata shell and optional portable
/// sheetData. Retain the namespace bindings inherited from its original root.
fn inherited_attribute(key: &str) -> bool {
    key == "xmlns" || key.starts_with("xmlns:") || key.starts_with("xml:")
}

pub(in crate::xlsx) fn split_grid(
    input: impl Read,
    shell: &mut dyn Write,
    mut grid: Option<&mut dyn Write>,
) -> Result<(bool, Option<String>)> {
    let mut reader = Window::start(input, &mut &mut *shell)?;
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut inside = false;
    let mut found = false;
    let mut portable = true;
    let mut dimension = None;
    let mut inherited = Vec::new();
    loop {
        buf.clear();
        let event = reader.read_event_into(&mut buf)?;
        let end = reader.buffer_position();
        let raw = reader.get_mut().through(end)?;
        let mut copy = true;
        match &event {
            Event::Start(e) if depth == 0 => {
                for a in e.attributes() {
                    let a = a?;
                    if inherited_attribute(a.key.as_ref()) {
                        inherited.push((a.key.as_ref().to_owned(), a.value.into_owned()));
                    }
                }
            }
            Event::Start(e) | Event::Empty(e)
                if depth == 1 && e.local_name().as_ref() == "dimension" =>
            {
                dimension = attr(e, "ref").filter(|s| crate::arr::parse_a1_range(s).is_some());
            }
            Event::Start(e) | Event::Empty(e)
                if depth == 1 && e.local_name().as_ref() == "sheetData" =>
            {
                anyhow::ensure!(!found, "重复的 sheetData");
                found = true;
                inside = matches!(event, Event::Start(_));
                quick_xml::Writer::new(&mut *shell).write_event(Event::Empty(e.clone()))?;
                if let Some(out) = grid.as_deref_mut() {
                    let mut tag = e.to_owned();
                    for (k, v) in &inherited {
                        if !e.attributes().flatten().any(|a| a.key.as_ref() == k) {
                            tag.push_attribute((
                                k.as_str(),
                                quick_xml::escape::unescape(v)?.as_ref(),
                            ));
                        }
                    }
                    quick_xml::Writer::new(out).write_event(if inside {
                        Event::Start(tag)
                    } else {
                        Event::Empty(tag)
                    })?;
                }
                copy = false;
            }
            Event::Start(e) | Event::Empty(e)
                if inside && e.local_name().as_ref() == "c" => {
                if e.attributes().flatten().any(|a| matches!(a.key.as_ref(), "cm" | "vm")) {
                    portable = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        if copy {
            if inside {
                if let Some(out) = grid.as_deref_mut() {
                    out.write_all(raw)?;
                }
            } else {
                shell.write_all(raw)?;
            }
        }
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth = depth.checked_sub(1).context("工作表 XML 深度无效")?;
                if depth == 1 {
                    inside = false;
                }
            }
            _ => {}
        }
    }
    anyhow::ensure!(found && depth == 0, "工作表缺少完整的 sheetData");
    Ok((portable, dimension))
}

const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const STRICT_NS: &str = "http://purl.oclc.org/ooxml/spreadsheetml/main";

/// Translate element names while retaining values, attribute spelling and
/// comments. Mixed namespaces in a changed row require the general writer.
fn rebind(xml: &str, from: &str, to: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut output = String::with_capacity(xml.len());
    let mut copied = 0;
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event().ok()?;
        let (offset, name) = match &event {
            Event::Start(e) | Event::Empty(e) => {
                for attribute in e.attributes() {
                    let attribute = attribute.ok()?;
                    if attribute.key.as_ref() == "xmlns" || attribute.key.as_ref().starts_with("xmlns:") {
                        return None;
                    }
                }
                (1,e.name())
            }
            Event::End(e) => (2,e.name()),
            Event::Eof => break,
            _ => continue,
        };
        let local = name.as_ref().strip_prefix(from)?;
        if local.contains(':') { return None; }
        let at = start + offset;
        output.push_str(xml.get(copied..at)?);
        output.push_str(to);
        copied = at + from.len();
    }
    output.push_str(&xml[copied..]);
    Some(output)
}

#[allow(clippy::too_many_arguments)]
fn missing_rows(
    out: &mut impl Write,
    prefix: &str,
    before: u32,
    keys: &mut BTreeSet<CellRef>,
    rows: &mut BTreeMap<u32, Option<u16>>,
    layout_rows: &mut BTreeMap<u32, crate::patch::layout_xml::Change>,
    sheet: &Sheet,
    base: &Store,
    styles: &StyleChanges,
    shared_state: &SharedFormulaState,
) -> Result<bool> {
    loop {
        let row = keys
            .first()
            .map_or(u32::MAX, |k| k.row)
            .min(rows.first_key_value().map_or(u32::MAX, |(&r, _)| r))
            .min(layout_rows.first_key_value().map_or(u32::MAX, |(&r, _)| r));
        if row >= before {
            break;
        }
        let style = rows.remove(&row);
        let mut lchg = layout_rows.remove(&row);
        if let Some(s) = style {
            lchg.get_or_insert_with(Default::default).style = Some(s);
        }
        let has_cells = keys.first().is_some_and(|k| k.row == row);
        if !has_cells {
            if let Some(ch) = lchg {
                if ch.needs_record() {
                    let row_name = format!("{prefix}row");
                    let rec = crate::patch::layout_xml::new_row_record(&row_name, row, &ch)?;
                    out.write_all(rec.as_bytes())?;
                }
            }
            continue;
        }
        let mut tag = format!("<{prefix}row r=\"{}\"/>", row + 1);
        if let Some(ch) = lchg {
            let Ok(updated) = crate::patch::layout_xml::update_row_tag(&tag, &ch) else {
                return Ok(false);
            };
            tag = updated;
        }
        let Some(open) = tag.strip_suffix("/>") else {
            return Ok(false);
        };
        out.write_all(open.as_bytes())?;
        out.write_all(b">")?;
        while let Some(k) = keys.first().copied().filter(|k| k.row == row) {
            if shared_state.promote_master.contains_key(&k) || shared_state.unshare.contains(&k) {
                return Ok(false);
            }
            let Some(cell) = new_cell(k, sheet, base, styles) else {
                return Ok(false);
            };
            if prefix.is_empty() {
                out.write_all(cell.as_bytes())?;
            } else if let Some(tagged) = rebind(&cell, "", prefix) {
                out.write_all(tagged.as_bytes())?;
            } else {
                return Ok(false);
            }
            keys.pop_first();
        }
        write!(out, "</{prefix}row>")?;
    }
    Ok(true)
}

/// Fast-forward raw worksheet bytes from inside `<sheetData>` up through `</sheetData>`,
/// writing everything through `</sheetData>` to `out` and appending the remaining suffix
/// bytes after `</sheetData>` into `suffix_out`. Declines (`Ok(false)`) if comments/CDATA
/// or non-canonical closing tags appear inside `<sheetData>`.
fn fast_forward_sheet_data_end<R: Read>(
    mut win: Window<R>,
    out: &mut impl Write,
    suffix_out: &mut Vec<u8>,
) -> Result<bool> {
    const NEEDLE: &[u8] = b"</sheetData>";
    let mut carry = Vec::with_capacity(64);
    carry.extend_from_slice(&win.bytes[win.first..]);
    win.bytes.clear();
    win.first = 0;

    loop {
        let chunk = win.input.fill_buf()?;
        let eof = chunk.is_empty();
        carry.extend_from_slice(chunk);
        let consumed = chunk.len();
        win.input.consume(consumed);

        if carry.windows(4).any(|w| w == b"<!--" || w == b"<![C") {
            return Ok(false);
        }
        if let Some(pos) = carry.windows(NEEDLE.len()).position(|w| w == NEEDLE) {
            let end_sd = pos + NEEDLE.len();
            out.write_all(&carry[..end_sd])?;
            suffix_out.extend_from_slice(&carry[end_sd..]);
            io::copy(&mut win.input, suffix_out)?;
            return Ok(true);
        }
        if carry.windows(11).any(|w| w == b"</sheetData") && eof {
            return Ok(false);
        }
        if eof {
            return Ok(false);
        }
        let keep = carry.len().min(NEEDLE.len() + 4);
        let flush_len = carry.len() - keep;
        if flush_len > 0 {
            out.write_all(&carry[..flush_len])?;
            carry.drain(..flush_len);
        }
    }
}

/// Removing indexed references changes only two optional root attributes. All
/// <si> records (including rich/phonetic runs) retain their original bytes.
pub(super) fn shared_strings(input: impl Read, out: &mut impl Write) -> Result<bool> {
    let mut reader = Window::start(input, out)?;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let event = reader.read_event_into(&mut buf)?;
        let end = reader.buffer_position();
        match event {
            Event::Start(ref e) | Event::Empty(ref e) if e.local_name().as_ref() == "sst" => {
                let mut tag = quick_xml::events::BytesStart::new(e.name().as_ref().to_owned());
                for attr in e.attributes() {
                    let attr = attr?;
                    if !matches!(attr.key.as_ref(), "count" | "uniqueCount") {
                        tag.push_attribute(attr);
                    }
                }
                let root = if matches!(event, Event::Empty(_)) {
                    Event::Empty(tag)
                } else {
                    Event::Start(tag)
                };
                quick_xml::Writer::new(&mut *out).write_event(root)?;
                reader.get_mut().through(end)?;
                reader.into_inner().copy_rest(out)?;
                return Ok(true);
            }
            Event::Start(_) | Event::Empty(_) | Event::Eof => return Ok(false),
            _ => out.write_all(reader.get_mut().through(end)?)?,
        }
    }
}

/// umya expects explicit cell/row coordinates and bounded column endpoints.
/// Prepare only sheets identified during load. Keep at most one cell and the
/// small worksheet header in memory, and leave other XML byte-for-byte intact.
pub(in crate::xlsx) fn repair_dom_input(input: impl Read, out: &mut impl Write) -> Result<()> {
    use quick_xml::{NsReader, name::ResolveResult};
    let mut reader = NsReader::from_reader(Window::start(input, out)?.into_inner());
    let mut root = None;
    let mut buf = Vec::new();
    let mut skip = Vec::new();
    let (mut data, mut row_open) = (false, false);
    let (mut row, mut next_row, mut next_col) = (0u32, 0u32, 0u32);
    loop {
        buf.clear();
        let start = reader.buffer_position();
        let (ns, event) = reader.read_resolved_event_into(&mut buf)?;
        let ns = match ns {
            ResolveResult::Bound(n) => n.0,
            ResolveResult::Unbound => "",
            ResolveResult::Unknown(_) => anyhow::bail!("工作表使用未声明的 XML 命名空间"),
        };
        let sheet_ns = root.as_deref() == Some(ns);
        let empty = matches!(event, Event::Empty(_));
        let mut finished = false;
        match event {
            Event::Start(tag) if root.is_none() && tag.local_name().as_ref() == "worksheet" => {
                root = Some(ns.to_owned());
            }
            Event::Empty(tag) | Event::Start(tag) => {
                let mut coordinate = None;
                let mut skip_body = true;
                match (data, row_open, tag.local_name().as_ref()) {
                    (false, false, "sheetData") if sheet_ns => {
                        let header = std::str::from_utf8(reader.get_mut().through(start)?)?;
                        out.write_all(
                            crate::patch::layout_xml::repair_column_bounds(header)?
                                .as_bytes(),
                        )?;
                        data = true;
                        finished = empty;
                        skip_body = false;
                    }
                    (true, false, "row") if sheet_ns => {
                        row = match attr(&tag, "r") {
                            Some(value) => {
                                value.parse::<u32>()?.checked_sub(1).context("行号为零")?
                            }
                            None => {
                                coordinate = Some((next_row + 1).to_string());
                                next_row
                            }
                        };
                        anyhow::ensure!(row < crate::sync::MAX_ROWS, "行号超出工作表");
                        next_row = row + 1;
                        next_col = 0;
                        row_open = !empty;
                        skip_body = false;
                    }
                    (true, true, "c") if sheet_ns => {
                        let col = match attr(&tag, "r") {
                            Some(value) => {
                                crate::sparse::parse_ref(&value)
                                    .context("单元格坐标无效")?
                                    .1
                            }
                            None => {
                                coordinate = Some(crate::model::cell_name(row, next_col));
                                next_col
                            }
                        };
                        anyhow::ensure!(col < crate::sync::MAX_COLS, "列号超出工作表");
                        next_col = col + 1;
                    }
                    _ => {}
                }
                if let Some(r) = coordinate {
                    let end = reader.buffer_position();
                    let raw = reader.get_mut().through(end)?;
                    let pos = raw.len() - if empty { 2 } else { 1 };
                    out.write_all(&raw[..pos])?;
                    write!(out, " r=\"{r}\"")?;
                    out.write_all(&raw[pos..])?;
                }
                if !empty && skip_body {
                    skip.clear();
                    reader.read_to_end_into(tag.name(), &mut skip)?;
                }
            }
            Event::End(_) if row_open => row_open = false,
            Event::End(_) if data => finished = true,
            Event::Eof => anyhow::bail!("工作表缺少 sheetData"),
            _ => {}
        }
        if data {
            let end = reader.buffer_position();
            out.write_all(reader.get_mut().through(end)?)?;
        }
        if finished {
            reader.into_inner().copy_rest(out)?;
            return Ok(());
        }
    }
}

pub(super) fn worksheet(
    input: impl Read,
    out: &mut impl Write,
    sheet: &Sheet,
    base: &Store,
    changes: Changes,
    changed_sst: &mut bool,
) -> Result<bool> {
    let Changes {
        mut keys,
        mut styles,
        layout,
        freeze_edit,
        filter_xml,
        page_edit,
        ..
    } = changes;
    let mut shared_state = SharedFormulaState::default();
    let mut axis_rows = std::mem::take(&mut styles.rows);
    let (mut layout_rows, layout_cols) = match layout {
        Some(l) => (l.rows, l.cols),
        None => (BTreeMap::new(), BTreeMap::new()),
    };
    let need_header_patch =
        !styles.cols.is_empty() || !layout_cols.is_empty() || freeze_edit || page_edit;
    let need_suffix_patch = sheet.merges_dirty || filter_xml.is_some() || page_edit;
    let mut header = need_header_patch.then(Vec::new);
    let mut additions: Option<(u32, u32, u32, u32)> = None;
    let merge_corners = sheet
        .merges
        .iter()
        .filter(|_| sheet.merges_dirty)
        .flat_map(|&(r, c, z, d)| [CellRef::new(r, c), CellRef::new(z, d)]);
    for k in keys
        .iter()
        .filter(|k| base.get(k.row, k.col).is_none())
        .copied()
        .chain(merge_corners)
    {
        if k.row >= crate::sync::MAX_ROWS || k.col >= crate::sync::MAX_COLS {
            return Ok(false);
        }
        let (top, left, bottom, right) = additions.get_or_insert((k.row, k.col, k.row, k.col));
        *top = (*top).min(k.row);
        *left = (*left).min(k.col);
        *bottom = (*bottom).max(k.row);
        *right = (*right).max(k.col);
    }
    let mut reader = Window::start(input, out)?;
    let mut buf = Vec::new();
    let mut skip = Vec::new();
    let mut in_data = false;
    let mut fast_forward_suffix = false;
    let mut prefix = String::new();
    let mut binding = String::from("xmlns");
    let mut root_uri = String::new();
    let mut last_row = None;
    let mut depth = 0usize;
    loop {
        buf.clear();
        let event_start = reader.buffer_position();
        let event = reader.read_event_into(&mut buf)?;
        let start = matches!(event, Event::Start(_));
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(ref e) | Event::Empty(ref e)
                if depth == 0 =>
            {
                if e.local_name().as_ref() != "worksheet" { return Ok(false); }
                if let Some((p,_)) = e.name().as_ref().split_once(':') {
                    prefix = format!("{p}:");
                    binding = format!("xmlns:{p}");
                    if !styles.cols.is_empty()
                        || !axis_rows.is_empty()
                        || !layout_rows.is_empty()
                        || !layout_cols.is_empty()
                        || sheet.merges_dirty
                        || freeze_edit
                        || filter_xml.is_some()
                        || page_edit
                    {
                        return Ok(false);
                    }
                }
                match attr(e,&binding) {
                    Some(uri) if uri == SHEET_NS || uri == STRICT_NS => root_uri = uri,
                    None if prefix.is_empty() => {}
                    _ => return Ok(false),
                }
            }
            Event::Start(ref e) | Event::Empty(ref e)
                if depth == 1 && e.name().as_ref().strip_prefix(prefix.as_str()) == Some("dimension") && additions.is_some() =>
            {
                if attr(e,&binding).is_some_and(|uri| uri != root_uri) { return Ok(false); }
                let Some(bounds) = attr(e, "ref").and_then(|s| crate::arr::parse_a1_range(&s))
                else {
                    return Ok(false);
                };
                let (r, c, z, d) = additions.unwrap();
                let expanded = (
                    r.min(bounds.0),
                    c.min(bounds.1),
                    z.max(bounds.2),
                    d.max(bounds.3),
                );
                if expanded != bounds {
                    let Some(mut tag) = expanded_tag(e, "ref") else {
                        return Ok(false);
                    };
                    tag.pop();
                    let target: &mut dyn Write = match &mut header { Some(bytes) => bytes, None => out };
                    write!(
                        target,
                        "{tag} ref=\"{}\"{}",
                        crate::arr::a1_range(expanded),
                        if empty { "/>" } else { ">" }
                    )?;
                    let end = reader.buffer_position();
                    reader.get_mut().through(end)?;
                    depth += usize::from(start);
                    continue;
                }
            }
            Event::Start(ref e) | Event::Empty(ref e)
                if depth == 1 && e.name().as_ref().strip_prefix(prefix.as_str()) == Some("sheetData") =>
            {
                if attr(e,&binding).is_some_and(|uri| uri != root_uri) { return Ok(false); }
                if let Some(header_bytes) = header.take() {
                    let mut hdr = std::str::from_utf8(&header_bytes)?.to_owned();
                    if !styles.cols.is_empty() {
                        let Ok(styled) = crate::patch::layout_xml::styled_columns(&hdr, &styles.cols) else {
                            return Ok(false);
                        };
                        hdr = styled;
                    }
                    if !layout_cols.is_empty() {
                        let Ok(updated) = crate::patch::layout_xml::apply_header(&hdr, &layout_cols) else {
                            return Ok(false);
                        };
                        hdr = updated;
                    }
                    if freeze_edit {
                        let sentinel = "<sheetData/></worksheet>";
                        let wrapped = format!("{hdr}{sentinel}");
                        let updated = crate::patch::set_freeze_xml(&wrapped, sheet.freeze);
                        let Some(stripped) = updated.strip_suffix(sentinel) else {
                            return Ok(false);
                        };
                        hdr = stripped.to_owned();
                    }
                    if page_edit {
                        let wrapped = format!("{hdr}<sheetData/></worksheet>");
                        let updated = crate::patch::set_page_setup(&wrapped, &sheet.page);
                        let Some(pos) = updated.find("<sheetData/>") else {
                            return Ok(false);
                        };
                        hdr = updated[..pos].to_owned();
                    }
                    out.write_all(hdr.as_bytes())?;
                }
                if empty {
                    let Some(tag) = expanded_tag(e, "") else {
                        return Ok(false);
                    };
                    out.write_all(tag.as_bytes())?;
                    if !missing_rows(
                        out,
                        &prefix,
                        u32::MAX,
                        &mut keys,
                        &mut axis_rows,
                        &mut layout_rows,
                        sheet,
                        base,
                        &styles,
                        &shared_state,
                    )? {
                        return Ok(false);
                    }
                    write!(out, "</{prefix}sheetData>")?;
                    let end = reader.buffer_position();
                    reader.get_mut().through(end)?;
                    break;
                }
                in_data = true;
                if keys.is_empty()
                    && axis_rows.is_empty()
                    && layout_rows.is_empty()
                    && shared_state.promote_master.is_empty()
                    && shared_state.unshare.is_empty()
                {
                    let end = reader.buffer_position();
                    out.write_all(reader.get_mut().through(end)?)?;
                    if !need_suffix_patch {
                        break;
                    } else if prefix.is_empty() {
                        fast_forward_suffix = true;
                        break;
                    }
                }
            }
            Event::Start(ref e) | Event::Empty(ref e)
                if in_data && depth == 2 && e.name().as_ref().strip_prefix(prefix.as_str()) == Some("row") =>
            {
                let Some(r) = attr(e, "r").and_then(|s| s.parse::<u32>().ok()?.checked_sub(1))
                else {
                    return Ok(false);
                };
                if last_row.is_some_and(|p| p >= r)
                    || !missing_rows(
                        out,
                        &prefix,
                        r,
                        &mut keys,
                        &mut axis_rows,
                        &mut layout_rows,
                        sheet,
                        base,
                        &styles,
                        &shared_state,
                    )?
                {
                    return Ok(false);
                }
                last_row = Some(r);
                let tag_len = usize::try_from(reader.buffer_position() - event_start)?;
                if start {
                    skip.clear();
                    reader.read_to_end_into(e.name(), &mut skip)?;
                }
                let end = reader.buffer_position();
                let raw = reader.get_mut().through(end)?;
                let axis_xf = axis_rows.remove(&r);
                let mut lchg = layout_rows.remove(&r);
                if let Some(xf) = axis_xf {
                    lchg.get_or_insert_with(Default::default).style = Some(xf);
                }
                let styled;
                let raw = if let Some(ch) = lchg {
                    let (tag, body) = raw.split_at(tag_len);
                    let Ok(styled_tag) =
                        crate::patch::layout_xml::update_row_tag(std::str::from_utf8(tag)?, &ch)
                    else {
                        return Ok(false);
                    };
                    let mut bytes = styled_tag.into_bytes();
                    bytes.extend_from_slice(body);
                    styled = bytes;
                    styled.as_slice()
                } else {
                    raw
                };
                if keys.first().is_some_and(|k| k.row == r) {
                    let mut row_keys = BTreeSet::new();
                    while keys.first().is_some_and(|k| k.row == r) {
                        row_keys.insert(keys.pop_first().unwrap());
                    }
                    let raw_xml = std::str::from_utf8(raw)?;
                    let normalized;
                    let xml = if prefix.is_empty() && !raw_xml.contains("xmlns") {
                        raw_xml
                    } else {
                        let Some(adapted) = rebind(raw_xml,&prefix,"") else { return Ok(false); };
                        normalized = adapted;
                        &normalized
                    };
                    let Some(edits) = row_edits(
                        xml,
                        sheet,
                        base,
                        row_keys,
                        &styles,
                        changed_sst,
                        &mut shared_state,
                        &mut keys,
                    )
                    else {
                        return Ok(false);
                    };
                    if prefix.is_empty() {
                        write_spans(out, xml, &edits)?;
                    } else {
                        let mut row = Vec::new();
                        write_spans(&mut row,xml,&edits)?;
                        let Some(tagged) = rebind(std::str::from_utf8(&row)?,"",&prefix) else { return Ok(false); };
                        out.write_all(tagged.as_bytes())?;
                    }
                } else {
                    out.write_all(raw)?;
                }
                if keys.is_empty()
                    && axis_rows.is_empty()
                    && layout_rows.is_empty()
                    && shared_state.promote_master.is_empty()
                    && shared_state.unshare.is_empty()
                {
                    if !need_suffix_patch {
                        break;
                    } else if prefix.is_empty() {
                        fast_forward_suffix = true;
                        break;
                    }
                }
                continue;
            }
            Event::Start(_) | Event::Empty(_) if in_data => return Ok(false),
            Event::End(ref e) if in_data && depth == 2 && e.name().as_ref().strip_prefix(prefix.as_str()) == Some("sheetData") => {
                if !missing_rows(
                    out,
                    &prefix,
                    u32::MAX,
                    &mut keys,
                    &mut axis_rows,
                    &mut layout_rows,
                    sheet,
                    base,
                    &styles,
                    &shared_state,
                )? {
                    return Ok(false);
                }
                let end = reader.buffer_position();
                out.write_all(reader.get_mut().through(end)?)?;
                break;
            }
            Event::End(_) => depth = depth.checked_sub(1).context("无效的工作表 XML 层次")?,
            Event::Eof => return Ok(false),
            _ => {}
        }
        depth += usize::from(start);
        let end = reader.buffer_position();
        let raw = reader.get_mut().through(end)?;
        if let Some(bytes) = &mut header { bytes.extend_from_slice(raw); }
        else { out.write_all(raw)?; }
    }
    if !shared_state.promote_master.is_empty() || !shared_state.unshare.is_empty() {
        return Ok(false);
    }
    if need_suffix_patch {
        // Metadata elements after sheetData are ordered by schema; run the same
        // schema/order checks on that small suffix without retaining the grid.
        let wrapper_prefix = b"<worksheet><sheetPr/><sheetData/>";
        let mut suffix = wrapper_prefix.to_vec();
        if fast_forward_suffix {
            if !fast_forward_sheet_data_end(reader.into_inner(), out, &mut suffix)? {
                return Ok(false);
            }
        } else {
            reader.into_inner().copy_rest(&mut suffix)?;
        }
        let mut xml = std::str::from_utf8(&suffix)?.to_owned();
        if let Some(ref af) = filter_xml {
            xml = crate::patch::insert_auto_filter(&xml, af);
        }
        if sheet.merges_dirty {
            let Some(edit) = merge_edit(&xml, sheet) else {
                return Ok(false);
            };
            let mut next = String::with_capacity(xml.len() + edit.text.len());
            next.push_str(&xml[..edit.from]);
            next.push_str(&edit.text);
            next.push_str(&xml[edit.to..]);
            xml = next;
        }
        if page_edit {
            xml = crate::patch::set_page_xml(&xml, &sheet.page);
            xml = crate::patch::set_page_setup(&xml, &sheet.page);
        }
        let Some(sd_pos) = xml.find("<sheetData/>") else {
            return Ok(false);
        };
        out.write_all(&xml.as_bytes()[sd_pos + "<sheetData/>".len()..])?;
    } else {
        reader.into_inner().copy_rest(out)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, Value};
    use std::cell::Cell as Counter;

    struct Source<'a> {
        rest: &'a [u8],
        read: &'a Counter<usize>,
    }
    impl Read for Source<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = out.len().min(512);
            let n = self.rest.read(&mut out[..n])?;
            self.read.set(self.read.get() + n);
            Ok(n)
        }
    }
    struct Sink<'a> {
        bytes: Vec<u8>,
        read: &'a Counter<usize>,
        tail: usize,
        wrote_cell: bool,
    }
    impl Write for Sink<'_> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if data.windows(10).any(|w| w == b"<v>123</v>") {
                assert!(
                    self.read.get() < self.tail,
                    "modified row waited for the opaque suffix"
                );
                self.wrote_cell = true;
            }
            self.bytes.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn edited_rows_with_locally_rebound_names_decline_the_prefix_adapter() {
        let row = r#"<x:row r="1"><x:c r="A1"><x:v xmlns:x="urn:other">2</x:v></x:c></x:row>"#;
        assert!(rebind(row,"x:","").is_none());
        let row = r#"<row r="1"><c r="A1"><v xmlns="urn:other">2</v></c></row>"#;
        assert!(rebind(row,"","").is_none());
        let row = r#"<x:row r='1'><!-- <x:c/> --><x:c r='A1'><x:f>"x:c"</x:f><x:v>1</x:v></x:c></x:row>"#;
        let local = rebind(row,"x:","").unwrap();
        assert_eq!(rebind(&local,"","x:").as_deref(),Some(row));
    }

    #[test]
    fn writes_the_modified_row_before_reading_the_trailing_cells_and_copies_original_bytes() {
        let mut sheet = Sheet::new("Sheet1");
        let number = |v| Cell {
            value: Value::Number(v),
            formula: None,
        };
        sheet.set(0, 0, number(1.0));
        let base = sheet.cells().clone();
        sheet.set(0, 0, number(123.0));
        let raw = format!(
            "\u{feff}<?xml version=\"1.0\"?><worksheet><!-- header --><sheetData><row r=\"1\"><c r=\"A1\"><v>1</v></c></row>{}</sheetData><extLst><!-- keep --></extLst></worksheet>",
            (2..2000)
                .map(|r| format!("<row r='{r}'><c r='A{r}' t='n'><v>02.0</v></c></row>"))
                .collect::<String>()
        );
        let changes = Changes {
            keys: BTreeSet::from([CellRef::new(0, 0)]),
            ..Default::default()
        };
        let read = Counter::new(0);
        let mut sink = Sink {
            bytes: Vec::new(),
            read: &read,
            tail: raw.len() / 2,
            wrote_cell: false,
        };
        assert!(
            worksheet(
                Source {
                    rest: raw.as_bytes(),
                    read: &read
                },
                &mut sink,
                &sheet,
                &base,
                changes,
                &mut false
            )
            .unwrap()
        );
        assert!(sink.wrote_cell);
        assert_eq!(
            String::from_utf8(sink.bytes).unwrap(),
            raw.replacen("<v>1</v>", "<v>123</v>", 1)
        );
    }
}
