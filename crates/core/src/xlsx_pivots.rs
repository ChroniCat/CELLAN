//! Remove only this session's generated pivot attachments from a private save
//! baseline. Imported pivots and parts shared with retained relationships stay
//! intact; the current pending model is attached again after serialization.

use anyhow::{Context, Result};
use quick_xml::{Reader, Writer, events::{BytesStart, Event}};
use std::{collections::{HashMap, HashSet}, io::Read, path::Path};

#[derive(Default)]
pub(super) struct StripPlan {
    pub removed: HashSet<String>,
    pub replacements: HashMap<String, Vec<u8>>,
}

#[derive(Clone)]
struct Relationship {
    id: String,
    kind: String,
    target: Option<String>,
}

fn attr(tag: &BytesStart<'_>, key: &str) -> Option<String> {
    tag.attributes().flatten().find(|a| a.key.as_ref() == key)
        .map(|a| crate::model::xml_unescape(&a.value))
}

fn relationship_id(tag: &BytesStart<'_>) -> Option<String> {
    tag.attributes().flatten().find(|a| a.key.as_ref().ends_with(":id"))
        .map(|a| crate::model::xml_unescape(&a.value))
}

fn owner(rels: &str) -> Option<String> {
    if rels == "_rels/.rels" { return Some(String::new()); }
    let (dir, file) = rels.rsplit_once("/_rels/")?;
    Some(format!("{dir}/{}", file.strip_suffix(".rels")?))
}

fn rels_of(part: &str) -> String {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") }
}

fn resolve(part: &str, target: &str) -> String {
    let path = if target.starts_with('/') { target.to_owned() } else {
        let dir = part.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
        format!("{dir}/{target}")
    };
    let mut components = Vec::new();
    for component in path.split('/') {
        match component { "" | "." => {}, ".." => { components.pop(); }, _ => components.push(component) }
    }
    components.join("/")
}

fn relationships(xml: &[u8], part: &str) -> Result<Vec<Relationship>> {
    let mut reader = Reader::from_reader(xml);
    let mut found = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" => {
                found.push(Relationship {
                    id: attr(&e, "Id").context("关系缺少 Id")?,
                    kind: attr(&e, "Type").unwrap_or_default(),
                    target: if attr(&e, "TargetMode").as_deref() == Some("External") { None }
                        else { attr(&e, "Target").map(|target| resolve(part, &target)) },
                });
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(found)
}

fn tag_attr(xml: &[u8], tag: &str, attribute: &str) -> Result<Option<String>> {
    let mut reader = Reader::from_reader(xml);
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == tag => return Ok(attr(&e, attribute)),
            Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

fn filter_xml(xml: &[u8], remove: impl Fn(&BytesStart<'_>) -> bool) -> Result<Vec<u8>> {
    let mut reader = Reader::from_reader(xml);
    let mut writer = Writer::new(Vec::new());
    loop {
        let event = reader.read_event()?;
        match &event {
            Event::Start(e) if remove(e) => { reader.read_to_end(e.name())?; }
            Event::Empty(e) if remove(e) => {}
            Event::Eof => break,
            _ => writer.write_event(event)?,
        }
    }
    Ok(writer.into_inner())
}

/// Drop emptied optional pivot containers (and fix the legacy table-part count).
fn clean_containers(xml: &[u8]) -> Result<Vec<u8>> {
    let mut reader = Reader::from_reader(xml);
    let mut writer = Writer::new(Vec::new());
    loop {
        let event = reader.read_event()?;
        match &event {
            Event::Empty(e) if matches!(e.local_name().as_ref(), "pivotCaches" | "pivotTableParts") => {}
            Event::Start(e) if matches!(e.local_name().as_ref(), "pivotCaches" | "pivotTableParts") => {
                let child = if e.local_name().as_ref() == "pivotCaches" { "pivotCache" } else { "pivotTablePart" };
                let tag = e.clone().into_owned();
                let mut inner = Writer::new(Vec::new());
                let mut depth = 1;
                let mut count = 0;
                while depth > 0 {
                    let event = reader.read_event()?;
                    match &event {
                        Event::Start(e) => { depth += 1; if e.local_name().as_ref() == child { count += 1; } }
                        Event::Empty(e) if e.local_name().as_ref() == child => count += 1,
                        Event::End(_) => { depth -= 1; if depth == 0 { break; } }
                        Event::Eof => anyhow::bail!("透视表容器 XML 未结束"),
                        _ => {}
                    }
                    inner.write_event(event)?;
                }
                if count > 0 {
                    let mut updated = BytesStart::new(tag.name().as_ref().to_string());
                    for a in tag.attributes().flatten() {
                        if a.key.as_ref() != "count" { updated.push_attribute(a); }
                    }
                    let count = count.to_string();
                    if child == "pivotTablePart" { updated.push_attribute(("count", count.as_str())); }
                    writer.write_event(Event::Start(updated))?;
                    writer.get_mut().extend_from_slice(&inner.into_inner());
                    writer.write_event(Event::End(tag.to_end()))?;
                }
            }
            Event::Eof => break,
            _ => writer.write_event(event)?,
        }
    }
    Ok(writer.into_inner())
}

pub(super) fn strip_plan(path: &Path, session: &str, extra_removed_tables: &[String]) -> Result<StripPlan> {
    let mut plan = StripPlan::default();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let mut parts = HashMap::new();
    // Read small metadata parts only. Worksheets are opened below only if a
    // removed relationship requires editing a legacy pivotTableParts element.
    for name in &names {
        if name.ends_with(".rels") || name.contains("pivotCacheDefinition") || name.starts_with("xl/pivotTables/")
            || name == "xl/workbook.xml" || name == "[Content_Types].xml" {
            let mut entry = zip.by_name(name)?;
            let body = crate::fixups::read_zip_entry_limited(&mut entry, crate::fixups::MAX_META_PART_BYTES)?;
            parts.insert(name.clone(), body);
        }
    }
    let marker = format!("sheetcore:{session}");
    for (part, body) in &parts {
        if part.contains("pivotCacheDefinition") && part.ends_with(".xml")
            && tag_attr(body, "pivotCacheDefinition", "refreshedBy")?.as_ref() == Some(&marker) {
            plan.removed.insert(part.clone());
        }
    }
    // 用户在当前会话修改或删除的导入透视表：一并剥离旧 pivotTable 部件（及不再被引用的缓存定义）
    let mut rels = HashMap::new();
    for (name, bytes) in &parts {
        if let Some(part) = owner(name) { rels.insert(part.clone(), relationships(bytes, &part)?); }
    }
    let extra_set: HashSet<&str> = extra_removed_tables
        .iter()
        .map(|s| s.trim_start_matches('/'))
        .collect();
    for part in parts.keys() {
        if extra_set.contains(part.as_str()) {
            plan.removed.insert(part.clone());
        }
    }
    // 若某个缓存定义只被已移除的 pivotTable 指向，则连同该缓存定义一并移除
    if !extra_set.is_empty() {
        let candidate_caches: HashSet<String> = rels
            .iter()
            .filter(|(part, _)| plan.removed.contains(*part))
            .flat_map(|(_, list)| {
                list.iter()
                    .filter(|r| r.kind.ends_with("/pivotCacheDefinition"))
                    .filter_map(|r| r.target.clone())
            })
            .collect();
        for cache in candidate_caches {
            let still_used_by_table = rels.iter().any(|(part, list)| {
                part.starts_with("xl/pivotTables/")
                    && !plan.removed.contains(part)
                    && list.iter().any(|r| r.target.as_ref() == Some(&cache))
            });
            if !still_used_by_table {
                plan.removed.insert(cache);
            }
        }
    }
    if plan.removed.is_empty() { return Ok(plan); }

    let wb_rels = rels.get("xl/workbook.xml").context("缺少透视表工作簿关系")?;
    let owned_rids: HashSet<String> = wb_rels.iter().filter(|r| r.target.as_ref().is_some_and(|t| plan.removed.contains(t))).map(|r| r.id.clone()).collect();
    let mut cache_ids = HashSet::new();
    let mut reader = Reader::from_reader(parts.get("xl/workbook.xml").context("缺少工作簿 XML")?.as_slice());
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "pivotCache"
                && relationship_id(&e).is_some_and(|rid| owned_rids.contains(&rid)) => {
                if let Some(id) = attr(&e, "cacheId") { cache_ids.insert(id); }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    for (part, body) in &parts {
        if part.starts_with("xl/pivotTables/") && part.ends_with(".xml") {
            let points_to_owned = rels.get(part).is_some_and(|relations| relations.iter()
                .any(|r| r.kind.ends_with("/pivotCacheDefinition") && r.target.as_ref().is_some_and(|t| plan.removed.contains(t))));
            if points_to_owned || tag_attr(body, "pivotTableDefinition", "cacheId")?.is_some_and(|id| cache_ids.contains(&id)) {
                plan.removed.insert(part.clone());
            }
        }
    }
    // Records can be shared with imported caches; remove them only if every
    // incoming internal relationship belongs to a removed part.
    let record_candidates: HashSet<String> = rels.iter().filter(|(part, _)| plan.removed.contains(*part))
        .flat_map(|(_, list)| list.iter().filter(|r| r.kind.ends_with("/pivotCacheRecords")).filter_map(|r| r.target.clone())).collect();
    for record in record_candidates {
        let retained = rels.iter().any(|(part, list)| !plan.removed.contains(part) && list.iter().any(|r| r.target.as_ref() == Some(&record)));
        if !retained { plan.removed.insert(record); }
    }
    for part in plan.removed.clone() { plan.removed.insert(rels_of(&part)); }

    for (part, list) in &rels {
        if plan.removed.contains(part) { continue; }
        let removed_ids: HashSet<_> = list.iter().filter(|r| r.target.as_ref().is_some_and(|target| plan.removed.contains(target)))
            .map(|r| r.id.clone()).collect();
        if removed_ids.is_empty() { continue; }
        let rels_part = rels_of(part);
        let body = parts.get(&rels_part).context("缺少关系 XML")?;
        plan.replacements.insert(rels_part, filter_xml(body, |tag| {
            tag.local_name().as_ref() == "Relationship" && attr(tag, "Id").is_some_and(|id| removed_ids.contains(&id))
        })?);
        if part == "xl/workbook.xml" || list.iter().any(|r| r.kind.ends_with("/pivotTable") && removed_ids.contains(&r.id)) {
            let mut entry = zip.by_name(part)?;
            let body = crate::fixups::read_zip_entry_limited(&mut entry, crate::fixups::MAX_SHEET_PART_BYTES)?;
            let body = filter_xml(&body, |tag| matches!(tag.local_name().as_ref(), "pivotCache" | "pivotTablePart")
                && relationship_id(tag).is_some_and(|id| removed_ids.contains(&id)))?;
            plan.replacements.insert(part.clone(), clean_containers(&body)?);
        }
    }
    let types = parts.get("[Content_Types].xml").context("缺少内容类型 XML")?;
    plan.replacements.insert("[Content_Types].xml".into(), filter_xml(types, |tag| {
        tag.local_name().as_ref() == "Override" && attr(tag, "PartName").is_some_and(|part| plan.removed.contains(part.trim_start_matches('/')))
    })?);
    Ok(plan)
}
