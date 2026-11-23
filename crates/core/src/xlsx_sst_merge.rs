//! Keep original SST indices stable for every restored worksheet. Match strings
//! emitted by the metadata writer to those indices and append only missing items.
use super::{Window, attr, inherited_attribute, xml_input};
use anyhow::{Context, Result, ensure};
use quick_xml::{
    Writer,
    events::{BytesStart, Event},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{self, Read, Write},
};
use zip::{ZipArchive, write::SimpleFileOptions};

pub(in crate::xlsx) const PART: &str = "xl/sharedStrings.xml";

pub(in crate::xlsx) fn compression() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(1))
}

pub(in crate::xlsx) struct Merge {
    indices: Vec<usize>,
    changed: HashSet<String>,
    append: Vec<bool>,
}

impl Merge {
    pub(in crate::xlsx) fn prepare(
        source: &mut ZipArchive<File>,
        generated: &mut ZipArchive<File>,
        rebuilt: &HashSet<String>,
    ) -> Result<Option<Self>> {
        if source.index_for_name(PART).is_none() {
            return Ok(None);
        }
        let mut merge = Self {
            indices: Vec::new(),
            changed: HashSet::new(),
            append: Vec::new(),
        };
        if rebuilt.is_empty() || generated.index_for_name(PART).is_none() {
            return Ok(Some(merge));
        }
        let mut used = HashSet::new();
        let mut per_sheet = HashMap::new();
        for part in rebuilt {
            let mut references = HashSet::new();
            map_indices(generated.by_name(part)?, &mut io::sink(), part, |id| {
                references.insert(id);
                Ok(id)
            })?;
            used.extend(references.iter().copied());
            per_sheet.insert(part.clone(),references);
        }
        if used.is_empty() {
            return Ok(Some(merge));
        }
        let mut keys = Vec::new();
        visit(generated, |item| {
            keys.push(<[u8; 32]>::from(Sha256::digest(item)));
            Ok(())
        })?;
        let mut matches: HashMap<_, Option<usize>> = keys
            .iter()
            .enumerate()
            .filter(|(i, _)| used.contains(i))
            .map(|(_, &k)| (k, None))
            .collect();
        let mut count = 0;
        visit(source, |item| {
            let hash: [u8; 32] = Sha256::digest(item).into();
            if let Some(id) = matches.get_mut(&hash) {
                id.get_or_insert(count);
            }
            count += 1;
            Ok(())
        })?;
        ensure!(used.iter().all(|&i| i < keys.len()), "共享字符串索引越界");
        for (i, key) in keys.into_iter().enumerate() {
            if !used.contains(&i) {
                merge.indices.push(i);
                merge.append.push(false);
                continue;
            }
            let id = matches.get_mut(&key).context("missing shared string")?;
            merge.append.push(id.is_none());
            merge.indices.push(*id.get_or_insert_with(|| {
                count += 1;
                count - 1
            }));
        }
        merge.changed = per_sheet.into_iter().filter_map(|(part, ids)| {
            ids.into_iter().any(|id| merge.indices[id] != id).then_some(part)
        }).collect();
        Ok(Some(merge))
    }

    pub(in crate::xlsx) fn remaps(&self, part: &str) -> bool {
        self.changed.contains(part)
    }

    pub(in crate::xlsx) fn write(
        &self,
        source: &mut ZipArchive<File>,
        generated: &mut ZipArchive<File>,
        out: &mut impl Write,
    ) -> Result<()> {
        let append = self.append.iter().any(|a| *a);
        xml_input(source.by_name(PART)?,PART, |input| {
            if !append { return super::shared_strings(input,out); }
            let mut add = Some(|out: &mut dyn Write| {
                let mut flags = self.append.iter();
                visit(generated, |item| {
                    if *flags.next().context("生成的共享字符串数量不一致")? { out.write_all(item)?; }
                    Ok(())
                })?;
                ensure!(flags.next().is_none(),"生成的共享字符串数量不一致");
                Ok::<_,anyhow::Error>(())
            });
            let mut reader = Window::start(input,out)?;
            let mut buf = Vec::new();
            let mut depth = 0usize;
            loop {
                buf.clear();
                let event = reader.read_event_into(&mut buf)?;
                let end = reader.buffer_position();
                let raw = reader.get_mut().through(end)?;
                match &event {
                    Event::Start(e) | Event::Empty(e) if depth == 0 => {
                        ensure!(e.local_name().as_ref() == "sst","无效的共享字符串表");
                        let mut tag = BytesStart::new(e.name().as_ref().to_owned());
                        for a in e.attributes() {
                            let a = a?;
                            if !matches!(a.key.as_ref(),"count"|"uniqueCount") { tag.push_attribute(a); }
                        }
                        Writer::new(&mut *out).write_event(Event::Start(tag))?;
                        if matches!(event,Event::Empty(_)) {
                            add.take().unwrap()(out)?;
                            Writer::new(&mut *out).write_event(Event::End(e.to_end()))?;
                        }
                    }
                    _ => {
                        if depth == 1 && (matches!(&event,Event::End(_)) ||
                            matches!(&event,Event::Start(e)|Event::Empty(e) if e.local_name().as_ref() != "si")) {
                            if let Some(mut add) = add.take() { add(out)?; }
                        }
                        out.write_all(raw)?;
                    }
                }
                match event {
                    Event::Start(_) => depth += 1,
                    Event::End(_) => depth = depth.checked_sub(1).context("无效的共享字符串表")?,
                    Event::Eof => { ensure!(depth == 0 && add.is_none(),"共享字符串表未正常结束"); break; }
                    _ => {}
                }
            }
            Ok(true)
        })?.then_some(()).context("共享字符串表无效")
    }

    pub(in crate::xlsx) fn remap_worksheet(
        &self,
        input: impl Read,
        out: &mut impl Write,
        part: &str,
    ) -> Result<()> {
        map_indices(input, out, part, |id| {
            self.indices.get(id).copied().context("共享字符串索引越界")
        })
    }
}

fn map_indices(
    input: impl Read,
    out: &mut impl Write,
    part: &str,
    mut map: impl FnMut(usize) -> Result<usize>,
) -> Result<()> {
    xml_input(input, part, |input| {
        let mut reader = Window::start(input, out)?;
        let mut buf = Vec::new();
        let mut depth = 0usize;
        let mut in_data = false;
        let mut in_cell = false;
        let mut in_value = false;
        loop {
            buf.clear();
            let event = reader.read_event_into(&mut buf)?;
            let end = reader.buffer_position();
            let raw = reader.get_mut().through(end)?;
            match &event {
                Event::Start(e) if depth == 1 => in_data = e.local_name().as_ref() == "sheetData",
                Event::Start(e) if depth == 3 && in_data => {
                    in_cell = e.local_name().as_ref() == "c" && attr(e, "t").as_deref() == Some("s")
                }
                Event::Start(e) if depth == 4 && in_cell => {
                    in_value = e.local_name().as_ref() == "v"
                }
                Event::Text(t) if depth == 5 && in_value => {
                    let id = t.as_ref().trim().parse::<usize>()?;
                    write!(out, "{}", map(id)?)?;
                    continue;
                }
                Event::End(_) if depth == 5 => in_value = false,
                Event::End(_) if depth == 4 => in_cell = false,
                Event::End(_) if depth == 2 => in_data = false,
                _ => {}
            }
            out.write_all(raw)?;
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => depth = depth.checked_sub(1).context("无效的工作表 XML 深度")?,
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(true)
    })?;
    Ok(())
}

/// Present one portable SI at a time. Ancestor declarations and xml:space
/// participate in identity so different rich markup cannot collide on plain text.
fn visit(zip: &mut ZipArchive<File>, mut item: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    xml_input(zip.by_name(PART)?, PART, |input| {
        let mut reader = Window::start(input, &mut io::sink())?;
        let mut buf = Vec::new();
        let mut skip = Vec::new();
        let mut portable = Vec::new();
        let mut inherited = Vec::new();
        let mut depth = 0usize;
        loop {
            buf.clear();
            let event = reader.read_event_into(&mut buf)?;
            let pos = reader.buffer_position();
            reader.get_mut().through(pos)?;
            match &event {
                Event::Start(e) if depth == 0 => {
                    ensure!(e.local_name().as_ref() == "sst", "无效的共享字符串表");
                    for a in e.attributes() {
                        let a = a?;
                        if inherited_attribute(a.key.as_ref()) {
                            inherited.push((a.key.as_ref().to_owned(), a.value.into_owned()));
                        }
                    }
                }
                Event::Start(e) | Event::Empty(e)
                    if depth == 1 && e.local_name().as_ref() == "si" =>
                {
                    let mut tag = e.to_owned();
                    for (k, v) in &inherited {
                        if !e.attributes().flatten().any(|a| a.key.as_ref() == k) {
                            tag.push_attribute((
                                k.as_str(),
                                quick_xml::escape::unescape(v)?.as_ref(),
                            ));
                        }
                    }
                    portable.clear();
                    if matches!(event, Event::Start(_)) {
                        Writer::new(&mut portable).write_event(Event::Start(tag))?;
                        skip.clear();
                        reader.read_to_end_into(e.name(), &mut skip)?;
                        let end = reader.buffer_position();
                        portable.extend_from_slice(reader.get_mut().through(end)?);
                    } else {
                        Writer::new(&mut portable).write_event(Event::Empty(tag))?;
                    }
                    item(&portable)?;
                    continue;
                }
                _ => {}
            }
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => depth = depth.checked_sub(1).context("无效的共享字符串深度")?,
                Event::Eof => break,
                _ => {}
            }
        }
        ensure!(depth == 0, "共享字符串表未正常结束");
        Ok(true)
    })?;
    Ok(())
}
