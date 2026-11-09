//! Reconcile sheet identities in a temporary base package before serialization.
//! Keeping worksheet part paths stable lets the existing preservation pipeline
//! retain drawings, extensions and relationships through rename/reorder/delete.

use crate::model::Workbook;
use anyhow::{Context, Result, ensure};
use quick_xml::{Reader, Writer, events::{BytesStart, BytesText, Event}};
use std::{collections::{HashMap, HashSet}, io::{Read, Write}, path::{Path, PathBuf}};

pub(super) struct PreparedBase {
    pub path: PathBuf,
    temporary: bool,
}

impl Drop for PreparedBase {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn attr(tag: &BytesStart<'_>, key: &str) -> Option<String> {
    tag.attributes().flatten().find(|a| a.key.as_ref() == key)
        .map(|a| crate::model::xml_unescape(&a.value))
}

fn replace_attr(tag: &BytesStart<'_>, key: &str, value: &str) -> BytesStart<'static> {
    let mut out = BytesStart::new(tag.name().as_ref().to_string());
    for a in tag.attributes().flatten() {
        if a.key.as_ref() != key {
            out.push_attribute(a);
        }
    }
    out.push_attribute((key, value));
    out.into_owned()
}

fn rewrite_qualifiers(text: &str, changes: &[(String, Option<String>)]) -> String {
    let mut result = text.to_string();
    // Invalid sheet names are temporary tokens only; two passes allow swaps
    // without cascading a rename into the next sheet's original qualifier.
    for (i, (old, new)) in changes.iter().enumerate() {
        let temporary = format!(":sheetflow-rename-{i}:");
        result = crate::refs::rewrite_sheet(&result, old, new.as_ref().map(|_| temporary.as_str()));
    }
    for (i, (_, new)) in changes.iter().enumerate() {
        if let Some(name) = new {
            result = crate::refs::rewrite_sheet(&result, &format!(":sheetflow-rename-{i}:"), Some(name));
        }
    }
    result
}

fn rewrite_pivot_source(xml: &[u8], changes: &[(String, Option<String>)]) -> Result<Vec<u8>> {
    let mut reader = Reader::from_reader(xml);
    let mut writer = Writer::new(Vec::new());
    loop {
        let event = reader.read_event()?;
        match &event {
            Event::Empty(e) | Event::Start(e) if e.local_name().as_ref() == "worksheetSource" => {
                let changed = attr(e, "sheet").and_then(|sheet| {
                    changes.iter().find(|(old, _)| old.eq_ignore_ascii_case(&sheet))
                });
                if let Some((_, new)) = changed {
                    let tag = replace_attr(e, "sheet", new.as_deref().unwrap_or("#REF!"));
                    writer.write_event(if matches!(event, Event::Empty(_)) { Event::Empty(tag) } else { Event::Start(tag) })?;
                } else { writer.write_event(event)?; }
            }
            Event::Eof => break,
            _ => writer.write_event(event)?,
        }
    }
    Ok(writer.into_inner())
}

/// Return the original package when the manifest is unchanged; otherwise make a
/// private raw-copy package with only workbook metadata and deleted sheet parts
/// changed. No source file is ever modified here.
pub(super) fn prepare_base(base: &Path, output: &Path, wb: &Workbook) -> Result<PreparedBase> {
    let original = || PreparedBase { path: base.into(), temporary: false };
    if !base.exists() {
        return Ok(original());
    }
    let xml = crate::fixups::read_part_file(base, "xl/workbook.xml")
        .context("读取基准工作簿目录失败")?;
    let mut reader = Reader::from_reader(xml.as_slice());
    let mut old = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Empty(e) | Event::Start(e) if e.local_name().as_ref() == "sheet" => {
                old.push(e.into_owned());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let targets = crate::patch::sheet_targets(base)?;
    let old_names: Vec<String> = old.iter().map(|e| attr(e, "name").unwrap_or_default()).collect();
    let mut used = HashSet::new();
    let mut selected = Vec::new(); // (old index, final index, renamed tag)
    let mut names = HashSet::new();
    for (new_index, sheet) in wb.sheets.iter().enumerate() {
        ensure!(names.insert(sheet.name.to_lowercase()), "重复的工作表名称: {}", sheet.name);
        let source = match &sheet.source_part {
            Some(part) => old_names.iter().position(|name| targets.get(name) == Some(part)),
            // None means a genuinely new sheet, including a replacement that
            // reuses a deleted sheet's name. Rebuild it from the in-memory model
            // on subsequent saves rather than adopting an unrelated old part.
            None => None,
        };
        if let Some(i) = source {
            ensure!(used.insert(i), "工作表身份重复: {}", sheet.name);
            selected.push((i, new_index, replace_attr(&old[i], "name", &sheet.name)));
        }
    }
    let unchanged = selected.len() == old.len()
        && selected.iter().enumerate().all(|(i, (old_i, _, tag))| {
            i == *old_i && attr(tag, "name").as_ref() == Some(&old_names[i])
        });
    if unchanged {
        return Ok(original());
    }

    let changes: Vec<(String, Option<String>)> = old_names.iter().enumerate().filter_map(|(i, old)| {
        let new = selected.iter().find(|(old_i, _, _)| *old_i == i).and_then(|(_, _, tag)| attr(tag, "name"));
        if new.as_ref() == Some(old) { None } else { Some((old.clone(), new)) }
    }).collect();

    let local_indices: HashMap<usize, usize> = selected.iter().map(|(a, b, _)| (*a, *b)).collect();
    let kept_ids: HashSet<String> = selected.iter().filter_map(|(_, _, e)| attr(e, "r:id")).collect();
    let mut removed_parts = HashSet::new();
    for (i, name) in old_names.iter().enumerate() {
        if !used.contains(&i) {
            if let Some(part) = targets.get(name) {
                removed_parts.insert(part.clone());
                if let Some((dir, file)) = part.rsplit_once('/') {
                    removed_parts.insert(format!("{dir}/_rels/{file}.rels"));
                }
            }
        }
    }

    let mut writer = Writer::new(Vec::new());
    let mut reader = Reader::from_reader(xml.as_slice());
    loop {
        let event = reader.read_event()?;
        match event {
            Event::Start(ref e) if e.local_name().as_ref() == "sheets" => {
                writer.write_event(Event::Start(e.clone()))?;
                for (_, _, tag) in &selected {
                    writer.write_event(Event::Empty(tag.clone()))?;
                }
                reader.read_to_end(e.name())?;
                writer.write_event(Event::End(e.to_end()))?;
            }
            Event::Start(ref e) if e.local_name().as_ref() == "definedName" => {
                let tag = if let Some(index) = attr(e, "localSheetId").and_then(|s| s.parse::<usize>().ok()) {
                    if let Some(new_index) = local_indices.get(&index) {
                        replace_attr(e, "localSheetId", &new_index.to_string())
                    } else {
                        reader.read_to_end(e.name())?;
                        continue;
                    }
                } else {
                    e.clone().into_owned()
                };
                let original_text = reader.read_text(e.name())?;
                let text = rewrite_qualifiers(&crate::model::xml_unescape(original_text.as_ref()), &changes);
                writer.write_event(Event::Start(tag))?;
                writer.write_event(Event::Text(BytesText::new(&text)))?;
                writer.write_event(Event::End(e.to_end()))?;
            }
            Event::Empty(ref e) if e.local_name().as_ref() == "workbookView" => {
                let active = attr(e, "activeTab").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                writer.write_event(Event::Empty(replace_attr(e, "activeTab", &local_indices.get(&active).copied().unwrap_or(0).to_string())))?;
            }
            Event::Eof => break,
            _ => writer.write_event(event)?,
        }
    }
    let workbook = writer.into_inner();
    let rels = crate::fixups::read_part_file(base, "xl/_rels/workbook.xml.rels")
        .context("读取基准工作簿关系失败")?;
    let mut writer = Writer::new(Vec::new());
    let mut reader = Reader::from_reader(rels.as_slice());
    loop {
        let event = reader.read_event()?;
        match &event {
            Event::Empty(e) if e.local_name().as_ref() == "Relationship"
                && attr(e, "Type").is_some_and(|t| t.ends_with("/worksheet"))
                && !attr(e, "Id").is_some_and(|id| kept_ids.contains(&id)) => {}
            Event::Eof => break,
            _ => writer.write_event(event)?,
        }
    }
    let relationships = writer.into_inner();

    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = output.parent().unwrap_or_else(|| Path::new(".")).join(format!(
        ".~sheetflow-base-{}-{seq}.xlsx", std::process::id()));
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    let prepared = PreparedBase { path, temporary: true };
    let mut zip = zip::ZipArchive::new(std::fs::File::open(base)?)?;
    let mut out = zip::ZipWriter::new(file);
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        if removed_parts.contains(&name) { continue; }
        let pivot = if name.contains("pivotCacheDefinition") && name.ends_with(".xml") {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            Some(rewrite_pivot_source(&bytes, &changes)?)
        } else { None };
        let replacement = match name.as_str() {
            "xl/workbook.xml" => Some(&workbook),
            "xl/_rels/workbook.xml.rels" => Some(&relationships),
            _ => pivot.as_ref(),
        };
        if let Some(bytes) = replacement {
            out.start_file(name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated))?;
            out.write_all(bytes)?;
        } else {
            out.raw_copy_file(entry)?;
        }
    }
    out.finish()?;
    Ok(prepared)
}
