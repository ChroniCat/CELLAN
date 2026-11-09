//! 真正的 Excel 数据透视表部件（可刷新、可拖字段）。
//!
//! 需要三个部分：
//! 1. `xl/pivotCache/pivotCacheDefinitionN.xml` —— 数据源与字段缓存
//! 2. `xl/pivotTables/pivotTableN.xml` —— 版面、行/列/值字段
//! 3. 关系与内容类型：workbook 的 `<pivotCaches>`、两处 rels、`[Content_Types]`
//!
//! 结果区仍然由我们写一份「值」，这样即使 Excel 不刷新也能看到数字；
//! `refreshOnLoad="1"` 让 Excel 打开时自己重算。

use crate::pivot::{Agg, PivotResult, PivotSpec};
use anyhow::{Context, Result, ensure};
use quick_xml::{Reader, Writer, XmlVersion, events::{BytesStart, Event}};
use std::{collections::HashSet, io::Read, path::Path};

const MAIN_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PACKAGE_REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

fn col_name(mut c: u32) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (c % 26) as u8) as char);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn agg_label(a: Agg) -> &'static str {
    match a {
        Agg::Sum => "求和项",
        Agg::Count => "计数项",
        Agg::Avg => "平均值项",
        Agg::Max => "最大值项",
        Agg::Min => "最小值项",
    }
}

/// 在已保存的文件上追加透视表部件；返回 cacheId
pub fn attach(
    path: &Path,
    sheet_name: &str,
    spec: &PivotSpec,
    res: &PivotResult,
    out_rect: (u32, u32, u32, u32),
    value_label: &str,
) -> Result<u32> {
    attach_impl(path, sheet_name, spec, res, out_rect, value_label, "sheetcore")
}

/// Mark attachments owned by one live workbook session so a later save can
/// rebuild its pending pivots without consuming edits made during a save.
pub fn attach_for_session(
    path: &Path, sheet_name: &str, spec: &PivotSpec, res: &PivotResult,
    out_rect: (u32, u32, u32, u32), value_label: &str, session: &str,
) -> Result<u32> {
    attach_impl(path, sheet_name, spec, res, out_rect, value_label, &format!("sheetcore:{session}"))
}

fn attach_impl(
    path: &Path, sheet_name: &str, spec: &PivotSpec, res: &PivotResult,
    out_rect: (u32, u32, u32, u32), value_label: &str, refreshed_by: &str,
) -> Result<u32> {
    let valid_rect = |(r1, c1, r2, c2)| r1 <= r2 && c1 <= c2 && r2 < 1_048_576 && c2 < 16_384;
    ensure!(valid_rect(spec.source), "透视表源区域无效");
    ensure!(valid_rect(out_rect), "透视表输出区域无效");
    let valid_field = |col| col >= spec.source.1 && col <= spec.source.3;
    ensure!(valid_field(spec.row_field) && valid_field(spec.value_field)
        && spec.col_field.is_none_or(valid_field), "透视表字段超出源区域");
    // Resolve and validate every existing part before touching the ZIP. IDs,
    // part suffixes and relationship IDs have separate collision domains.
    let plan = AttachmentPlan::read(path, sheet_name)?;
    let cache_id = plan.cache_id;
    let table_idx = plan.table_idx;
    let src = format!(
        "{}{}:{}{}",
        col_name(spec.source.1),
        spec.source.0 + 1,
        col_name(spec.source.3),
        spec.source.2 + 1
    );
    let n_fields = (spec.source.3 - spec.source.1 + 1) as usize;
    let headers: Vec<String> = res.headers.clone();

    // ---------- 字段缓存 ----------
    let mut caches = String::new();
    for i in 0..n_fields {
        let name = headers.get(i).cloned().unwrap_or_else(|| format!("字段{i}"));
        if i as u32 == spec.value_field - spec.source.1 {
            // 数值字段要带 min/max，Excel 用来定刻度
            let (min, max) = numeric_range(res, i);
            caches.push_str(&format!(
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems containsSemiMixedTypes=\"0\" containsString=\"0\" containsNumber=\"1\"{}{}/></cacheField>",
                esc(&name),
                min.map(|v| format!(" minValue=\"{v}\"")).unwrap_or_default(),
                max.map(|v| format!(" maxValue=\"{v}\"")).unwrap_or_default()
            ));
        } else {
            caches.push_str(&format!(
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems containsSemiMixedTypes=\"0\" containsString=\"1\" containsBlank=\"1\"/></cacheField>",
                esc(&name)
            ));
        }
    }
    let def = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<pivotCacheDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
r:id=\"rId1\" refreshOnLoad=\"1\" refreshedBy=\"{}\" recordCount=\"{}\">\
<cacheSource type=\"worksheet\"><worksheetSource ref=\"{src}\" sheet=\"{}\"/></cacheSource>\
<cacheFields count=\"{n_fields}\">{caches}</cacheFields>\
</pivotCacheDefinition>",
        esc(refreshed_by), res.rows.len(),
        esc(sheet_name)
    );

    // ---------- 透视表定义 ----------
    let row_i = (spec.row_field - spec.source.1) as usize;
    let val_i = (spec.value_field - spec.source.1) as usize;
    let mut pfields = String::new();
    for i in 0..n_fields {
        if i == row_i {
            pfields.push_str("<pivotField axis=\"axisRow\" showAll=\"0\" outline=\"1\"/>");
        } else if Some(i as u32) == spec.col_field.map(|c| c - spec.source.1) {
            pfields.push_str("<pivotField axis=\"axisCol\" showAll=\"0\" outline=\"1\"/>");
        } else if i == val_i {
            pfields.push_str("<pivotField dataField=\"1\" showAll=\"0\"/>");
        } else {
            pfields.push_str("<pivotField showAll=\"0\"/>");
        }
    }
    let r2 = out_rect.2;
    let c2 = out_rect.3;
    let loc = format!(
        "{}{}:{}{}",
        col_name(out_rect.1),
        out_rect.0 + 1,
        col_name(c2),
        r2 + 1
    );
    let mut extra = String::new();
    if let Some(cf) = spec.col_field {
        extra.push_str(&format!(
            "<colFields count=\"1\"><field x=\"{}\"/></colFields><colItems count=\"1\"><i/></colItems>",
            cf - spec.source.1
        ));
    } else {
        extra.push_str("<colItems count=\"1\"><i/></colItems>");
    }
    // rowItems：每组一行，最后一行是总计（t="grand"）
    let mut items = String::new();
    for i in 0..res.rows.len() {
        if i + 1 == res.rows.len() {
            items.push_str("<i t=\"grand\"><x/></i>");
        } else {
            items.push_str(&format!("<i><x v=\"{i}\"/></i>"));
        }
    }
    let def_table = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<pivotTableDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
name=\"{}\" cacheId=\"{cache_id}\" dataCaption=\"值\" \
updatedVersion=\"6\" minRefreshableVersion=\"3\" createdVersion=\"6\" \
applyWidthHeightFormats=\"1\" useAutoFormatting=\"1\" itemPrintTitles=\"1\" \
indent=\"0\" outline=\"1\" outlineData=\"1\" multipleFieldFilters=\"0\">\
<location ref=\"{loc}\" firstHeaderRow=\"1\" firstDataRow=\"1\" firstDataCol=\"1\"/>\
<pivotFields count=\"{n_fields}\">{pfields}</pivotFields>\
<rowFields count=\"1\"><field x=\"{row_i}\"/></rowFields>\
<rowItems count=\"{}\">{items}</rowItems>\
{extra}\
<dataFields count=\"1\"><dataField name=\"{}:{}\" fld=\"{val_i}\" baseField=\"0\" baseItem=\"0\"/></dataFields>\
<pivotTableStyleInfo name=\"PivotStyleLight16\" showRowHeaders=\"1\" showColHeaders=\"1\" showRowStripes=\"0\" showColStripes=\"0\" showLastColumn=\"1\"/>\
</pivotTableDefinition>",
        esc(&plan.table_name), res.rows.len(),
        agg_label(spec.agg),
        esc(value_label)
    );

    // ---------- 写 zip ----------
    let cache_part = format!("xl/pivotCache/pivotCacheDefinition{cache_id}.xml");
    let table_part = format!("xl/pivotTables/pivotTable{table_idx}.xml");
    let cache_rels = format!("xl/pivotCache/_rels/pivotCacheDefinition{cache_id}.xml.rels");
    let cache_conn = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheRecords\" Target=\"pivotCacheRecords{cache_id}.xml\"/>\
</Relationships>"
    );
    let records = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<pivotCacheRecords xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" count=\"0\"/>"
    );
    let records_part = format!("xl/pivotCache/pivotCacheRecords{cache_id}.xml");

    // A PivotTable is an implicit worksheet relationship, not a worksheet XML
    // child. Its own relationship to the cache is also required by OPC.
    let table_conn = format!(
        "<Relationships xmlns=\"{PACKAGE_REL_NS}\"><Relationship Id=\"rId1\" Type=\"{REL_NS}/pivotCacheDefinition\" Target=\"../pivotCache/pivotCacheDefinition{cache_id}.xml\"/></Relationships>"
    );
    let workbook = add_cache_registration(&plan.workbook, cache_id, &plan.cache_rid)?;
    let workbook_rels = add_relationship(&plan.workbook_rels, &plan.cache_rid,
        "pivotCacheDefinition", &format!("pivotCache/pivotCacheDefinition{cache_id}.xml"))?;
    let sheet_rels = add_relationship(&plan.sheet_rels, &plan.table_rid,
        "pivotTable", &relative_target(&plan.sheet_part, &table_part))?;
    let mut types = plan.types.clone();
    for (part, kind) in [(&cache_part, "pivotCacheDefinition"), (&records_part, "pivotCacheRecords"), (&table_part, "pivotTable")] {
        let mut tag = BytesStart::new("Override");
        tag.push_attribute(("xmlns", "http://schemas.openxmlformats.org/package/2006/content-types"));
        tag.push_attribute(("PartName", format!("/{part}").as_str()));
        tag.push_attribute(("ContentType", format!("application/vnd.openxmlformats-officedocument.spreadsheetml.{kind}+xml").as_str()));
        types = append_element(&types, "Types", &[tag])?;
    }
    crate::patch::append_entries(path, &[
        (cache_part, def.into_bytes()), (records_part, records.into_bytes()),
        (cache_rels, cache_conn.into_bytes()), (table_part.clone(), def_table.into_bytes()),
        (rels_path(&table_part), table_conn.into_bytes()),
        ("xl/workbook.xml".into(), workbook), ("xl/_rels/workbook.xml.rels".into(), workbook_rels),
        (rels_path(&plan.sheet_part), sheet_rels), ("[Content_Types].xml".into(), types),
    ])?;
    Ok(cache_id)
}

fn numeric_range(res: &PivotResult, col: usize) -> (Option<f64>, Option<f64>) {
    let mut min: Option<f64> = None;
    let mut max: Option<f64> = None;
    for r in res.rows.iter().take(res.rows.len().saturating_sub(1)) {
        if let Some(crate::model::Value::Number(v)) = r.get(col).cloned() {
            min = Some(min.map_or(v, |m: f64| m.min(v)));
            max = Some(max.map_or(v, |m: f64| m.max(v)));
        }
    }
    (min, max)
}

fn attr(tag: &BytesStart<'_>, key: &str) -> Result<Option<String>> {
    for attribute in tag.attributes() {
        let attribute = attribute?;
        if attribute.key.as_ref() == key {
            return Ok(Some(attribute.normalized_value(XmlVersion::Implicit1_0)?.into_owned()));
        }
    }
    Ok(None)
}

/// Parse before writing, including closing tags and attributes. Truncated XML
/// must not turn an attachment into a successful but unregistered ZIP entry.
fn tags(xml: &[u8]) -> Result<Vec<(usize, BytesStart<'static>)>> {
    let mut reader = Reader::from_reader(xml);
    let mut result = Vec::new();
    let mut depth = 0usize;
    let mut roots = 0;
    loop {
        let event = reader.read_event()?;
        match event {
            Event::Start(e) => {
                if depth == 0 { roots += 1; }
                for a in e.attributes() { a?.normalized_value(XmlVersion::Implicit1_0)?; }
                result.push((depth, e.into_owned()));
                depth += 1;
            }
            Event::Empty(e) => {
                if depth == 0 { roots += 1; }
                for a in e.attributes() { a?.normalized_value(XmlVersion::Implicit1_0)?; }
                result.push((depth, e.into_owned()));
            }
            Event::End(_) => {
                ensure!(depth > 0, "XML 结束标签没有开始标签");
                depth -= 1;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(roots == 1 && depth == 0, "XML 根元素缺失或不完整");
    Ok(result)
}

fn root_is(xml: &[u8], root: &str) -> Result<()> {
    let elements = tags(xml)?;
    ensure!(elements[0].1.local_name().as_ref() == root, "XML 根元素应为 {root}");
    Ok(())
}

fn append_element(xml: &[u8], parent: &str, children: &[BytesStart<'_>]) -> Result<Vec<u8>> {
    append_at(xml, parent, 0, children)
}

fn append_at(xml: &[u8], parent: &str, parent_depth: usize, children: &[BytesStart<'_>]) -> Result<Vec<u8>> {
    let mut reader = Reader::from_reader(xml);
    let mut writer = Writer::new(Vec::new());
    let mut depth = 0;
    let mut inserted = false;
    loop {
        let event = reader.read_event()?;
        match &event {
            Event::Start(_) => { depth += 1; writer.write_event(event)?; }
            Event::Empty(e) if depth == parent_depth && e.local_name().as_ref() == parent => {
                ensure!(!inserted, "重复的 {parent} 登记");
                writer.write_event(Event::Start(e.clone()))?;
                for child in children { writer.write_event(Event::Empty(child.clone()))?; }
                writer.write_event(Event::End(e.to_end()))?;
                inserted = true;
            }
            Event::End(e) => {
                depth -= 1;
                if depth == parent_depth && e.local_name().as_ref() == parent {
                    ensure!(!inserted, "重复的 {parent} 登记");
                    for child in children { writer.write_event(Event::Empty(child.clone()))?; }
                    inserted = true;
                }
                writer.write_event(event)?;
            }
            Event::Eof => break,
            _ => writer.write_event(event)?,
        }
    }
    ensure!(inserted, "缺少 {parent} 登记");
    Ok(writer.into_inner())
}

fn add_relationship(xml: &[u8], id: &str, kind: &str, target: &str) -> Result<Vec<u8>> {
    let mut rel = BytesStart::new("Relationship");
    rel.push_attribute(("xmlns", PACKAGE_REL_NS));
    rel.push_attribute(("Id", id));
    rel.push_attribute(("Type", format!("{REL_NS}/{kind}").as_str()));
    rel.push_attribute(("Target", target));
    append_element(xml, "Relationships", &[rel])
}

fn add_cache_registration(xml: &[u8], id: u32, rid: &str) -> Result<Vec<u8>> {
    let mut cache = BytesStart::new("pivotCache");
    cache.push_attribute(("xmlns", MAIN_NS));
    cache.push_attribute(("xmlns:r", REL_NS));
    cache.push_attribute(("cacheId", id.to_string().as_str()));
    cache.push_attribute(("r:id", rid));
    if tags(xml)?.iter().any(|(depth, e)| *depth == 1 && e.local_name().as_ref() == "pivotCaches") {
        return append_at(xml, "pivotCaches", 1, &[cache]);
    }
    let mut container = BytesStart::new("pivotCaches");
    container.push_attribute(("xmlns", MAIN_NS));
    let mut reader = Reader::from_reader(xml);
    let mut writer = Writer::new(Vec::new());
    let mut depth = 0;
    let mut inserted = false;
    loop {
        let event = reader.read_event()?;
        // CT_Workbook places pivotCaches after calcPr/oleSize/customWorkbookViews
        // and before these following elements; extLst must remain last.
        let before = match &event {
            Event::Start(e) | Event::Empty(e) if depth == 1 => matches!(e.local_name().as_ref(),
                "smartTagPr" | "smartTagTypes" | "webPublishing" | "fileRecoveryPr" | "webPublishObjects" | "extLst"),
            Event::End(_) if depth == 1 => true,
            _ => false,
        };
        if before && !inserted {
            writer.write_event(Event::Start(container.clone()))?;
            writer.write_event(Event::Empty(cache.clone()))?;
            writer.write_event(Event::End(container.to_end()))?;
            inserted = true;
        }
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            Event::Eof => break,
            _ => {}
        }
        writer.write_event(event)?;
    }
    ensure!(inserted, "缺少工作簿根元素");
    Ok(writer.into_inner())
}

fn rels_path(part: &str) -> String {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") }
}

fn resolve_target(owner: &str, target: &str) -> Result<String> {
    let mut pieces: Vec<_> = if target.starts_with('/') { Vec::new() }
        else { owner.rsplit_once('/').map(|(dir, _)| dir.split('/').collect()).unwrap_or_default() };
    for piece in target.split('/') {
        match piece {
            "" | "." => {}
            ".." => { ensure!(pieces.pop().is_some(), "关系目标超出包根目录"); }
            _ => pieces.push(piece),
        }
    }
    ensure!(!pieces.is_empty(), "关系目标为空");
    Ok(pieces.join("/"))
}

fn relative_target(owner: &str, target: &str) -> String {
    let from: Vec<_> = owner.rsplit_once('/').map(|(dir, _)| dir.split('/').collect()).unwrap_or_default();
    let to: Vec<_> = target.split('/').collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut pieces = vec![".."; from.len() - common];
    pieces.extend(&to[common..]);
    pieces.join("/")
}

fn available_id(mut free: impl FnMut(u32) -> bool) -> Result<u32> {
    let mut id = 1u32;
    loop {
        if free(id) { return Ok(id); }
        id = id.checked_add(1).context("没有可用的透视表编号")?;
    }
}

fn relationship_ids(xml: &[u8]) -> Result<HashSet<String>> {
    root_is(xml, "Relationships")?;
    let mut ids = HashSet::new();
    for (_, e) in tags(xml)? {
        if e.local_name().as_ref() == "Relationship" {
            let id = attr(&e, "Id")?.context("关系缺少 Id")?;
            ensure!(ids.insert(id), "重复的关系 Id");
        }
    }
    Ok(ids)
}

struct AttachmentPlan {
    cache_id: u32,
    table_idx: u32,
    table_name: String,
    cache_rid: String,
    table_rid: String,
    sheet_part: String,
    workbook: Vec<u8>,
    workbook_rels: Vec<u8>,
    sheet_rels: Vec<u8>,
    types: Vec<u8>,
}

impl AttachmentPlan {
    fn read(path: &Path, sheet_name: &str) -> Result<Self> {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let parts: HashSet<String> = (0..zip.len()).map(|i| zip.by_index_raw(i).map(|e| e.name().to_owned())).collect::<std::result::Result<_, _>>()?;
        let mut read = |part: &str| -> Result<Vec<u8>> {
            let mut bytes = Vec::new();
            zip.by_name(part).with_context(|| format!("缺少部件 {part}"))?.read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        let workbook = read("xl/workbook.xml")?;
        let workbook_rels = read("xl/_rels/workbook.xml.rels")?;
        let types = read("[Content_Types].xml")?;
        root_is(&workbook, "workbook")?;
        root_is(&types, "Types")?;
        let workbook_ids = relationship_ids(&workbook_rels)?;
        let sheet_tag = tags(&workbook)?.into_iter().find(|(_, e)| {
            e.local_name().as_ref() == "sheet" && attr(e, "name").ok().flatten().as_deref() == Some(sheet_name)
        }).with_context(|| format!("找不到工作表 {sheet_name}"))?.1;
        let sheet_rid = sheet_tag.attributes().find_map(|a| {
            let a = a.ok()?;
            a.key.as_ref().ends_with(":id").then(|| a.normalized_value(XmlVersion::Implicit1_0).ok().map(|s| s.into_owned())).flatten()
        }).context("工作表缺少关系 Id")?;
        let sheet_rel = tags(&workbook_rels)?.into_iter().find(|(_, e)| attr(e, "Id").ok().flatten().as_ref() == Some(&sheet_rid))
            .context("找不到工作表关系")?.1;
        ensure!(attr(&sheet_rel, "Type")?.is_some_and(|t| t.ends_with("/worksheet"))
            && attr(&sheet_rel, "TargetMode")?.as_deref() != Some("External"), "工作表关系无效");
        let sheet_part = resolve_target("xl/workbook.xml", &attr(&sheet_rel, "Target")?.context("工作表关系缺少目标")?)?;
        root_is(&read(&sheet_part)?, "worksheet")?;
        let sheet_rels_part = rels_path(&sheet_part);
        let sheet_rels = if parts.contains(&sheet_rels_part) { read(&sheet_rels_part)? }
            else { format!("<Relationships xmlns=\"{PACKAGE_REL_NS}\"/>").into_bytes() };
        let sheet_ids = relationship_ids(&sheet_rels)?;
        let mut reserved = parts.clone();
        let mut table_parts = HashSet::new();
        for (_, e) in tags(&types)? {
            if e.local_name().as_ref() != "Override" { continue; }
            if let Some(part) = attr(&e, "PartName")? {
                let part = resolve_target("", &part)?;
                reserved.insert(part.clone());
                if attr(&e, "ContentType")?.is_some_and(|t| t.ends_with(".pivotTable+xml")) { table_parts.insert(part); }
            }
        }
        // Reserve even dangling relationship targets: allocating that path
        // would silently retarget an existing relationship to the new pivot.
        for part in &parts {
            if !part.ends_with(".rels") { continue; }
            let owner = if part == "_rels/.rels" { String::new() }
                else if let Some((dir, file)) = part.rsplit_once("/_rels/") { format!("{dir}/{}", file.trim_end_matches(".rels")) }
                else if let Some(file) = part.strip_prefix("_rels/") { file.trim_end_matches(".rels").to_owned() }
                else { continue };
            for (_, e) in tags(&read(part)?)? {
                if e.local_name().as_ref() != "Relationship" || attr(&e, "TargetMode")?.as_deref() == Some("External") { continue; }
                if let Some(target) = attr(&e, "Target")? { reserved.insert(resolve_target(&owner, &target)?); }
            }
        }
        let mut cache_ids = HashSet::new();
        for (_, e) in tags(&workbook)? {
            if e.local_name().as_ref() == "pivotCache" {
                cache_ids.insert(attr(&e, "cacheId")?.context("透视缓存缺少编号")?.parse::<u32>()?);
            }
        }
        let mut table_names = HashSet::new();
        for part in &parts {
            if !table_parts.contains(part) && !(part.starts_with("xl/pivotTables/") && part.ends_with(".xml")) { continue; }
            for (_, e) in tags(&read(part)?)? {
                if e.local_name().as_ref() != "pivotTableDefinition" { continue; }
                if let Some(id) = attr(&e, "cacheId")? { cache_ids.insert(id.parse::<u32>()?); }
                if let Some(name) = attr(&e, "name")? { table_names.insert(name.to_lowercase()); }
            }
        }
        let cache_id = available_id(|id| !cache_ids.contains(&id) && [
            format!("xl/pivotCache/pivotCacheDefinition{id}.xml"),
            format!("xl/pivotCache/pivotCacheRecords{id}.xml"),
            format!("xl/pivotCache/_rels/pivotCacheDefinition{id}.xml.rels"),
            format!("xl/pivotCache/_rels/pivotCacheRecords{id}.xml.rels"),
        ].iter().all(|part| !reserved.contains(part)))?;
        let table_idx = available_id(|id| !reserved.contains(&format!("xl/pivotTables/pivotTable{id}.xml"))
            && !reserved.contains(&format!("xl/pivotTables/_rels/pivotTable{id}.xml.rels"))
            && !table_names.contains(&format!("数据透视表{id}")))?;
        let cache_rid = format!("rIdPivotCache{}", available_id(|id| !workbook_ids.contains(&format!("rIdPivotCache{id}")))?);
        let table_rid = format!("rIdPivotTable{}", available_id(|id| !sheet_ids.contains(&format!("rIdPivotTable{id}")))?);
        Ok(Self { cache_id, table_idx, table_name: format!("数据透视表{table_idx}"), cache_rid,
            table_rid, sheet_part, workbook, workbook_rels, sheet_rels, types })
    }
}
