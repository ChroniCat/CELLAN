//! Keep original SST indices stable for every restored worksheet. Match strings
//! emitted by the metadata writer to those indices and append only missing items.
use super::{SHEET_NS, STRICT_NS, Window, attr, inherited_attribute, xml_input};
use anyhow::{Context, Result, ensure};
use quick_xml::{
    NsReader, Writer,
    events::{BytesStart, Event},
    name::ResolveResult,
};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{self, Read, Write},
};
use zip::{ZipArchive, write::SimpleFileOptions};

pub(in crate::xlsx) const PART: &str = "xl/sharedStrings.xml";

#[derive(PartialEq, Eq, Hash)]
enum ItemKey {
    Plain { preserve: bool, text: Box<str> },
    Raw(Box<[u8]>),
}

/// umya adds an empty phoneticPr to every SI, even to plain strings without
/// phonetic runs. Only canonicalize this exact shape; preserve rich runs, actual
/// pronunciation data, foreign elements and unknown attributes by raw identity.
fn item_key(item: &[u8]) -> Result<ItemKey> {
    const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
    let xml = std::str::from_utf8(item)?;
    let raw = || ItemKey::Raw(item.to_vec().into_boxed_slice());
    if xml.contains("<r>") || xml.contains("<rPh") {
        return Ok(raw());
    }
    let plain = (|| -> Option<(String, bool)> {
        let mut reader = NsReader::from_str(xml);
        let mut si_ns = String::new();
        let mut si_space: Option<String> = None;
        let mut t_space: Option<String> = None;
        let mut saw_t = false;
        let mut saw_phonetic = false;
        let mut in_t = false;
        let mut in_phonetic = false;
        let mut text = String::new();
        let mut depth = 0usize;

        let check_plain_attrs = |reader: &NsReader<&[u8]>,
                                 e: &BytesStart<'_>|
         -> Option<Option<String>> {
            let mut space = None;
            for a in e.attributes() {
                let a = a.ok()?;
                let k = a.key.as_ref();
                if k == "xmlns" || k.starts_with("xmlns:") {
                    continue;
                }
                let (ns, local) = reader.resolver().resolve(a.key, false);
                let is_xml = matches!(ns, ResolveResult::Bound(n) if n.0 == XML_NS);
                if !is_xml || local.as_ref() != "space" {
                    return None;
                }
                let val = quick_xml::escape::unescape(&a.value).ok()?;
                if !matches!(val.as_ref(), "default" | "preserve") {
                    return None;
                }
                space = Some(val.into_owned());
            }
            Some(space)
        };

        loop {
            let (ns, ev) = reader.read_resolved_event().ok()?;
            let bound = match ns {
                ResolveResult::Bound(n) => Some(n.0),
                ResolveResult::Unbound => None,
                ResolveResult::Unknown(_) => return None,
            };
            match ev {
                Event::Start(ref e) | Event::Empty(ref e) => {
                    let empty = matches!(ev, Event::Empty(_));
                    let local = e.local_name();
                    if depth == 0 {
                        let ns_str = bound?;
                        if !matches!(ns_str, SHEET_NS | STRICT_NS) || local.as_ref() != "si" {
                            return None;
                        }
                        si_ns = ns_str.to_owned();
                        si_space = check_plain_attrs(&reader, e)?;
                        if empty {
                            return None; // no <t> child
                        }
                        depth = 1;
                    } else if depth == 1 {
                        if bound != Some(si_ns.as_str()) {
                            return None;
                        }
                        if !saw_t {
                            if local.as_ref() != "t" {
                                return None;
                            }
                            t_space = check_plain_attrs(&reader, e)?;
                            saw_t = true;
                            if !empty {
                                in_t = true;
                                depth = 2;
                            }
                        } else if !saw_phonetic {
                            if local.as_ref() != "phoneticPr" {
                                return None;
                            }
                            let mut attr_count = 0usize;
                            let mut font_id_ok = false;
                            for a in e.attributes() {
                                let a = a.ok()?;
                                let k = a.key.as_ref();
                                if k == "xmlns" || k.starts_with("xmlns:") {
                                    continue;
                                }
                                attr_count += 1;
                                if k == "fontId"
                                    && quick_xml::escape::unescape(&a.value).ok().as_deref() == Some("1")
                                {
                                    font_id_ok = true;
                                }
                            }
                            if attr_count != 1 || !font_id_ok {
                                return None;
                            }
                            saw_phonetic = true;
                            if !empty {
                                in_phonetic = true;
                                depth = 2;
                            }
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                Event::Text(t) => {
                    if in_t {
                        text.push_str(&t.xml10_content());
                    } else if in_phonetic || (depth == 1 && !t.as_ref().trim().is_empty()) {
                        return None;
                    }
                }
                Event::GeneralRef(g) => {
                    if in_t {
                        if let Ok(Some(ch)) = g.resolve_char_ref() {
                            text.push(ch);
                        } else {
                            let s = crate::model::resolve_entity(g.as_ref().as_ref());
                            if s.is_empty() {
                                return None;
                            }
                            text.push_str(&s);
                        }
                    } else {
                        return None;
                    }
                }
                Event::CData(_) | Event::Comment(_) | Event::PI(_) => {
                    if depth >= 1 {
                        return None;
                    }
                }
                Event::End(_) => {
                    depth = depth.checked_sub(1)?;
                    if depth == 1 {
                        in_t = false;
                        in_phonetic = false;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        if depth != 0 || !saw_t {
            return None;
        }
        let preserve = t_space.as_deref().or(si_space.as_deref()) == Some("preserve")
            && (text.starts_with([' ', '\r', '\n', '\t']) || text.ends_with([' ', '\r', '\n', '\t']));
        Some((text, preserve))
    })();
    if let Some((text, preserve)) = plain {
        Ok(ItemKey::Plain {
            preserve,
            text: text.into_boxed_str(),
        })
    } else {
        Ok(raw())
    }
}

fn write_item(item: &[u8], target: &str, out: &mut dyn Write) -> Result<()> {
    // Metadata-writer items come from its Transitional package. Namespace
    // bindings, including any local redeclaration, must match a Strict SST.
    if target == SHEET_NS {
        out.write_all(item)?;
        return Ok(());
    }
    ensure!(target == STRICT_NS, "不支持的共享字符串命名空间");
    let xml = std::str::from_utf8(item)?;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut last = 0;
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event()?;
        if matches!(event, Event::Eof) {
            break;
        }
        if let Event::Start(tag) | Event::Empty(tag) = &event {
            let mut new = BytesStart::new(tag.name().as_ref().to_owned());
            let mut replaced = false;
            for a in tag.attributes() {
                let a = a?;
                let k = a.key.as_ref();
                if (k == "xmlns" || k.starts_with("xmlns:"))
                    && quick_xml::escape::unescape(&a.value)? == SHEET_NS
                {
                    new.push_attribute((k, STRICT_NS));
                    replaced = true;
                } else {
                    new.push_attribute(a);
                }
            }
            if replaced {
                out.write_all(&item[last..start])?;
                Writer::new(&mut *out).write_event(if matches!(event, Event::Empty(_)) {
                    Event::Empty(new)
                } else {
                    Event::Start(new)
                })?;
                last = reader.buffer_position() as usize;
            }
        }
    }
    out.write_all(&item[last..])?;
    Ok(())
}

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
            keys.push(if used.contains(&keys.len()) { Some(item_key(item)?) } else { None });
            Ok(())
        })?;
        let mut matches: HashMap<&ItemKey, Option<usize>> = keys
            .iter()
            .flatten()
            .map(|k| (k, None))
            .collect();
        let mut count = 0;
        let mut unmatched = matches.len();
        visit(source, |item| {
            if unmatched != 0 {
                let key = item_key(item)?;
                if let Some(id) = matches.get_mut(&key) && id.is_none() {
                    *id = Some(count);
                    unmatched -= 1;
                }
            }
            count += 1;
            Ok(())
        })?;
        ensure!(used.iter().all(|&i| i < keys.len()), "共享字符串索引越界");
        let mut resolved_ids: HashMap<ItemKey, Option<usize>> = matches
            .into_iter()
            .map(|(k, v)| {
                let cloned = match k {
                    ItemKey::Plain { preserve, text } => ItemKey::Plain {
                        preserve: *preserve,
                        text: text.clone(),
                    },
                    ItemKey::Raw(b) => ItemKey::Raw(b.clone()),
                };
                (cloned, v)
            })
            .collect();
        for (i, key) in keys.into_iter().enumerate() {
            let Some(key) = key else {
                merge.indices.push(i);
                merge.append.push(false);
                continue;
            };
            let id = resolved_ids.get_mut(&key).context("missing shared string")?;
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
            let mut add = Some(|out: &mut dyn Write, target: &str| {
                let mut flags = self.append.iter();
                visit(generated, |item| {
                    if *flags.next().context("生成的共享字符串数量不一致")? { write_item(item,target,out)?; }
                    Ok(())
                })?;
                ensure!(flags.next().is_none(),"生成的共享字符串数量不一致");
                Ok::<_,anyhow::Error>(())
            });
            let mut reader = Window::start(input,out)?;
            let mut buf = Vec::new();
            let mut depth = 0usize;
            let mut target = String::new();
            loop {
                buf.clear();
                let event = reader.read_event_into(&mut buf)?;
                let end = reader.buffer_position();
                let raw = reader.get_mut().through(end)?;
                match &event {
                    Event::Start(e) | Event::Empty(e) if depth == 0 => {
                        ensure!(e.local_name().as_ref() == "sst","无效的共享字符串表");
                        let name = e.name();
                        let binding = name.as_ref().split_once(':').map_or_else(||"xmlns".to_owned(),|(prefix,_)|format!("xmlns:{prefix}"));
                        target = quick_xml::escape::unescape(&attr(e,&binding).context("共享字符串表没有命名空间")?)?.into_owned();
                        let mut tag = BytesStart::new(e.name().as_ref().to_owned());
                        for a in e.attributes() {
                            let a = a?;
                            if !matches!(a.key.as_ref(),"count"|"uniqueCount") { tag.push_attribute(a); }
                        }
                        Writer::new(&mut *out).write_event(Event::Start(tag))?;
                        if matches!(event,Event::Empty(_)) {
                            add.take().unwrap()(out,&target)?;
                            Writer::new(&mut *out).write_event(Event::End(e.to_end()))?;
                        }
                    }
                    _ => {
                        if depth == 1 && (matches!(&event,Event::End(_)) ||
                            matches!(&event,Event::Start(e)|Event::Empty(e) if e.local_name().as_ref() != "si")) {
                            if let Some(mut add) = add.take() { add(out,&target)?; }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, Sheet, Value, Workbook};

    #[test]
    fn package_reuses_plain_indices_and_appends_in_the_source_namespace() {
        for ns in [SHEET_NS, STRICT_NS] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("source.xlsx");
            let rebuilt = dir.path().join("rebuilt.xlsx");
            let text = |s: &str| Cell {
                value: Value::Text(s.into()),
                formula: None,
            };
            let mut model = Workbook::new_blank();
            model.sheets.push(Sheet::new("Edited"));
            model.sheets[0].set(0, 0, text("prefix"));
            model.sheets[1].set(0, 0, text("same & text"));
            model.sheets[1].set(1, 0, text("new"));
            crate::xlsx::save_atomic(&rebuilt, &model).unwrap();
            std::fs::copy(&rebuilt, &source).unwrap();
            let targets = crate::patch::sheet_targets(&source).unwrap();
            let rich = "<x:si><x:r><x:rPr><x:b/></x:rPr><x:t>same &amp; text</x:t></x:r></x:si>";
            let phonetic = "<x:si><x:t>same &amp; text</x:t><x:rPh sb='0' eb='1'><x:t>S</x:t></x:rPh><x:phoneticPr fontId='1'/></x:si>";
            let plain = "<x:si><x:t xml:space='preserve'>same &#38; text</x:t></x:si>";
            let sst = format!("<x:sst xmlns:x='{ns}'>{rich}{phonetic}{plain}</x:sst>");
            let keeper = format!(
                "<x:worksheet xmlns:x='{ns}'><x:sheetData><x:row r='1'><x:c r='A1' t='s'><x:v>0</x:v></x:c></x:row></x:sheetData></x:worksheet>"
            );
            crate::fixups::write_parts(
                &source,
                &[
                    (PART.into(), sst.into_bytes()),
                    (targets["Sheet1"].clone(), keeper.clone().into_bytes()),
                ],
            )
            .unwrap();
            // The normal metadata writer restores this worksheet before restore_grids.
            crate::fixups::write_parts(
                &rebuilt,
                &[(targets["Sheet1"].clone(), keeper.into_bytes())],
            )
            .unwrap();
            crate::xlsx::restore_grids(
                &rebuilt,
                &source,
                &["Sheet1".to_owned()],
                false,
                Default::default(),
            )
            .unwrap();
            let bytes = crate::fixups::read_part(&rebuilt, PART).unwrap();
            let xml = std::str::from_utf8(&bytes).unwrap();
            assert!(xml.contains(rich) && xml.contains(phonetic) && xml.contains(plain));
            let doc = roxmltree::Document::parse(xml).unwrap();
            let entries: Vec<_> = doc
                .root_element()
                .children()
                .filter(|n| n.is_element())
                .collect();
            assert_eq!(entries.len(), 4);
            assert!(
                entries[3]
                    .descendants()
                    .filter(|n| n.is_element())
                    .all(|n| n.tag_name().namespace() == Some(ns))
            );
            assert_eq!(
                entries[3].first_element_child().unwrap().text(),
                Some("new")
            );
            let indices = |part: &str| {
                let bytes = crate::fixups::read_part(&rebuilt, part).unwrap();
                let doc = roxmltree::Document::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
                doc.descendants()
                    .filter(|n| n.tag_name().name() == "v")
                    .map(|n| n.text().unwrap().parse::<usize>().unwrap())
                    .collect::<Vec<_>>()
            };
            assert_eq!(indices(&targets["Sheet1"]), [0]);
            assert_eq!(indices(&targets["Edited"]), [2, 3]);
        }
    }
}
