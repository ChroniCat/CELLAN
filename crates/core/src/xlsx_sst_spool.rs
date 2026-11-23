//! Preserve shared strings when a metadata-only DOM may renumber its SST.
//! The worksheet uses inline rich strings; offsets are the only per-string RAM.
use super::{Window, xml_input};
use anyhow::{Context, Result, ensure};
use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub(in crate::xlsx) struct SharedStrings {
    base: PathBuf,
    table: Option<Table>,
}

impl SharedStrings {
    pub(in crate::xlsx) fn new(base: &Path) -> Self {
        Self {
            base: base.to_owned(),
            table: None,
        }
    }

    pub(super) fn copy(&mut self, index: usize, out: &mut dyn Write) -> Result<()> {
        if self.table.is_none() {
            let mut zip = zip::ZipArchive::new(File::open(&self.base)?)?;
            let part = "xl/sharedStrings.xml";
            xml_input(zip.by_name(part)?, part, |input| {
                self.table = Some(Table::build(input)?);
                Ok(true)
            })?;
        }
        let table = self.table.as_mut().context("缺少共享字符串索引")?;
        let &(offset, size) = table.entries.get(index).context("共享字符串索引越界")?;
        table.data.seek(SeekFrom::Start(offset))?;
        ensure!(
            io::copy(&mut (&mut table.data).take(size), out)? == size,
            "共享字符串暂存内容不完整"
        );
        Ok(())
    }
}

struct Table {
    entries: Vec<(u64, u64)>,
    data: BufReader<File>,
}

impl Table {
    fn build(input: impl Read) -> Result<Self> {
        let mut reader = Window::start(input, &mut io::sink())?;
        let mut data = tempfile::tempfile()?;
        let mut out = BufWriter::new(&mut data);
        let mut entries = Vec::new();
        let mut inherited = Vec::new();
        let mut name = String::new();
        let mut start = 0;
        let mut offset = 0;
        let mut depth = 0usize;
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let event = reader.read_event_into(&mut buf)?;
            let end = reader.buffer_position();
            let raw = reader.get_mut().through(end)?;
            match &event {
                Event::Start(e) if depth == 0 => {
                    ensure!(e.local_name().as_ref() == "sst", "共享字符串表根元素无效");
                    for a in e.attributes() {
                        let a = a?;
                        if super::inherited_attribute(a.key.as_ref()) {
                            inherited.push((a.key.as_ref().to_owned(), a.value.into_owned()));
                        }
                    }
                }
                Event::Start(e) | Event::Empty(e)
                    if depth == 1 && e.local_name().as_ref() == "si" =>
                {
                    start = offset;
                    name = e
                        .name()
                        .as_ref()
                        .rsplit_once(':')
                        .map_or_else(|| "is".to_owned(), |(p, _)| format!("{p}:is"));
                    let mut tag = BytesStart::new(name.clone());
                    for a in e.attributes() {
                        tag.push_attribute(a?);
                    }
                    for (k, v) in &inherited {
                        if !e.attributes().flatten().any(|a| a.key.as_ref() == k) {
                            tag.push_attribute((
                                k.as_str(),
                                quick_xml::escape::unescape(v)?.as_ref(),
                            ));
                        }
                    }
                    let mut header = Writer::new(Vec::new());
                    if matches!(event, Event::Empty(_)) {
                        header.write_event(Event::Empty(tag))?;
                    } else {
                        header.write_event(Event::Start(tag))?;
                    }
                    let bytes = header.into_inner();
                    out.write_all(&bytes)?;
                    offset += bytes.len() as u64;
                    if matches!(event, Event::Empty(_)) {
                        entries.push((start, offset - start));
                        name.clear();
                    }
                }
                Event::End(_) if depth == 2 && !name.is_empty() => {
                    write!(out, "</{name}>")?;
                    offset += 3 + name.len() as u64;
                    entries.push((start, offset - start));
                    name.clear();
                }
                Event::Eof => break,
                _ if !name.is_empty() => {
                    out.write_all(raw)?;
                    offset += raw.len() as u64;
                }
                _ => {}
            }
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => depth = depth.checked_sub(1).context("共享字符串 XML 深度无效")?,
                _ => {}
            }
        }
        ensure!(depth == 0, "共享字符串表未正常结束");
        out.flush()?;
        drop(out);
        Ok(Self {
            entries,
            data: BufReader::new(data),
        })
    }
}

// Caller has consumed the source <c t="s">. Only the ordinary <v> index is
// replaced; opaque caches or companion elements go back to the full serializer.
pub(super) fn inline_cell<R: Read>(
    reader: &mut Reader<Window<R>>,
    cell: &BytesStart<'_>,
    strings: &mut SharedStrings,
    out: &mut dyn Write,
) -> Result<bool> {
    let mut tag = BytesStart::new(cell.name().as_ref().to_owned());
    for a in cell.attributes() {
        let a = a?;
        if a.key.as_ref() == "t" {
            tag.push_attribute(("t", "inlineStr"));
        } else {
            tag.push_attribute(a);
        }
    }
    Writer::new(&mut *out).write_event(Event::Start(tag))?;
    let mut depth = 1usize;
    let mut valid = true;
    let mut in_index = false;
    let mut inserted = false;
    let mut index = String::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let event = reader.read_event_into(&mut buf)?;
        let end = reader.buffer_position();
        let raw = reader.get_mut().through(end)?;
        match &event {
            Event::Start(e)
                if depth == 1
                    && !inserted
                    && e.local_name().as_ref() == "v"
                    && e.attributes().next().is_none() =>
            {
                in_index = true
            }
            Event::Text(e) if in_index && depth == 2 => index.push_str(e.as_ref()),
            Event::End(e) if in_index && depth == 2 && e.local_name().as_ref() == "v" => {
                if let Ok(n) = index.trim().parse::<usize>() {
                    strings.copy(n, out)?;
                    inserted = true;
                } else {
                    valid = false;
                }
                in_index = false;
            }
            Event::End(_) if depth == 1 => {
                out.write_all(raw)?;
                return Ok(valid && inserted);
            }
            Event::Text(e) if depth == 1 && e.as_ref().trim().is_empty() => out.write_all(raw)?,
            Event::Comment(_) | Event::PI(_) if depth == 1 => out.write_all(raw)?,
            Event::Eof => anyhow::bail!("单元格未正常结束"),
            _ => valid = false,
        }
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
}
