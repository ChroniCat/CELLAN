//! Stream source XML directly into the private output ZIP. Keep only the bytes
//! of the current row so unedited markup can be copied with its original spelling.
use super::{
    Changes, StyleChanges, attr, expanded_tag, merge_edit, new_cell, row_edits, write_spans,
};
use crate::{CellRef, Sheet, store::Store};
use anyhow::{Context, Result};
use quick_xml::{Reader, events::Event};
use std::{
    collections::BTreeSet,
    io::{self, BufRead, BufReader, Read, Write},
};

#[path = "xlsx_utf16_stream.rs"]
mod utf16;
#[path = "xlsx_sst_spool.rs"]
mod sst;
pub(in crate::xlsx) use sst::SharedStrings as GridStrings;

pub(super) fn xml_input(
    mut input: impl Read,
    part: &str,
    process: impl FnOnce(&mut dyn Read) -> Result<bool>,
) -> Result<bool> {
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

struct Window<R> {
    input: BufReader<R>,
    bytes: Vec<u8>,
    first: usize,
    offset: u64,
}

impl<R: Read> Window<R> {
    fn start(input: R, out: &mut impl Write) -> Result<Reader<Self>> {
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

    fn through(&mut self, end: u64) -> Result<&[u8]> {
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
    shell: &mut Vec<u8>,
    mut grid: Option<&mut dyn Write>,
    strings: &mut GridStrings,
) -> Result<(bool, Option<String>)> {
    let mut reader = Window::start(input, shell)?;
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
                if inside && grid.is_some() && e.local_name().as_ref() == "c" =>
            {
                if e.attributes()
                    .flatten()
                    .any(|a| matches!(a.key.as_ref(), "cm" | "vm"))
                {
                    portable = false;
                } else if attr(e, "t").as_deref() == Some("s") {
                    if matches!(event, Event::Start(_)) {
                        portable &= sst::inline_cell(
                            &mut reader,
                            e,
                            strings,
                            grid.as_deref_mut().unwrap(),
                        )?;
                        continue;
                    } else {
                        portable = false;
                    }
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

fn missing_rows(
    out: &mut impl Write,
    before: u32,
    keys: &mut BTreeSet<CellRef>,
    sheet: &Sheet,
    base: &Store,
    styles: &StyleChanges,
) -> Result<bool> {
    while let Some(&key) = keys.first().filter(|k| k.row < before) {
        write!(out, "<row r=\"{}\">", key.row + 1)?;
        while let Some(k) = keys.first().copied().filter(|k| k.row == key.row) {
            let Some(cell) = new_cell(k, sheet, base, styles) else {
                return Ok(false);
            };
            out.write_all(cell.as_bytes())?;
            keys.pop_first();
        }
        out.write_all(b"</row>")?;
    }
    Ok(true)
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

pub(super) fn worksheet(
    input: impl Read,
    out: &mut impl Write,
    sheet: &Sheet,
    base: &Store,
    changes: Changes,
    changed_sst: &mut bool,
) -> Result<bool> {
    let Changes {
        mut keys, styles, ..
    } = changes;
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
    let mut last_row = None;
    let mut depth = 0usize;
    loop {
        buf.clear();
        let event = reader.read_event_into(&mut buf)?;
        let start = matches!(event, Event::Start(_));
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(ref e) | Event::Empty(ref e)
                if depth == 0 && e.name().as_ref() != "worksheet" =>
            {
                return Ok(false);
            }
            Event::Start(ref e) | Event::Empty(ref e)
                if depth == 1 && e.name().as_ref() == "dimension" && additions.is_some() =>
            {
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
                    write!(
                        out,
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
                if depth == 1 && e.name().as_ref() == "sheetData" =>
            {
                if empty {
                    let Some(tag) = expanded_tag(e, "") else {
                        return Ok(false);
                    };
                    out.write_all(tag.as_bytes())?;
                    if !missing_rows(out, u32::MAX, &mut keys, sheet, base, &styles)? {
                        return Ok(false);
                    }
                    out.write_all(b"</sheetData>")?;
                    let end = reader.buffer_position();
                    reader.get_mut().through(end)?;
                    break;
                }
                in_data = true;
            }
            Event::Start(ref e) | Event::Empty(ref e)
                if in_data && depth == 2 && e.name().as_ref() == "row" =>
            {
                let Some(r) = attr(e, "r").and_then(|s| s.parse::<u32>().ok()?.checked_sub(1))
                else {
                    return Ok(false);
                };
                if last_row.is_some_and(|p| p >= r)
                    || !missing_rows(out, r, &mut keys, sheet, base, &styles)?
                {
                    return Ok(false);
                }
                last_row = Some(r);
                if start {
                    skip.clear();
                    reader.read_to_end_into(e.name(), &mut skip)?;
                }
                let end = reader.buffer_position();
                let raw = reader.get_mut().through(end)?;
                if keys.first().is_some_and(|k| k.row == r) {
                    let mut row_keys = BTreeSet::new();
                    while keys.first().is_some_and(|k| k.row == r) {
                        row_keys.insert(keys.pop_first().unwrap());
                    }
                    let xml = std::str::from_utf8(raw)?;
                    let Some(edits) = row_edits(xml, sheet, base, row_keys, &styles, changed_sst)
                    else {
                        return Ok(false);
                    };
                    write_spans(out, xml, &edits)?;
                } else {
                    out.write_all(raw)?;
                }
                if keys.is_empty() && !sheet.merges_dirty {
                    break;
                }
                continue;
            }
            Event::Start(_) | Event::Empty(_) if in_data => return Ok(false),
            Event::End(ref e) if in_data && depth == 2 && e.name().as_ref() == "sheetData" => {
                if !missing_rows(out, u32::MAX, &mut keys, sheet, base, &styles)? {
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
        out.write_all(reader.get_mut().through(end)?)?;
    }
    if sheet.merges_dirty {
        // Merges are ordered among worksheet-level metadata after sheetData.
        // Run the same schema/order checks on that suffix without retaining the grid.
        let prefix = b"<worksheet><sheetData/>";
        let mut suffix = prefix.to_vec();
        reader.into_inner().copy_rest(&mut suffix)?;
        let xml = std::str::from_utf8(&suffix)?;
        let Some(edit) = merge_edit(xml, sheet) else {
            return Ok(false);
        };
        out.write_all(&suffix[prefix.len()..edit.from])?;
        out.write_all(edit.text.as_bytes())?;
        out.write_all(&suffix[edit.to..])?;
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
            styles: Default::default(),
            formula_changed: false,
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
