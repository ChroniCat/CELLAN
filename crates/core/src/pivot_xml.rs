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

    // ---------- 缓存与透视表定义（pivot_parts.rs 负责三份 XML 的一致性）----------
    let (def, records, def_table) = crate::pivot_parts::build(
        path, sheet_name, spec, res, out_rect, value_label, cache_id, &plan.table_name, refreshed_by,
    )?;

    write_parts(path, &plan, cache_id, table_idx, def, records, def_table)
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
/// 把缓存三件套写进包，并注册 workbook/sheet 关系与内容类型。
#[allow(clippy::too_many_arguments)]
fn write_parts(
    path: &Path,
    plan: &AttachmentPlan,
    cache_id: u32,
    table_idx: u32,
    def: String,
    records: String,
    def_table: String,
) -> Result<u32> {
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

/// 多字段透视（PivotSpec2 + PivotGrid）附挂入口。
pub fn attach2_for_session(
    path: &Path,
    sheet_name: &str,
    spec: &crate::pivot_multi::PivotSpec2,
    grid: &crate::pivot_multi::PivotGrid,
    out_rect: (u32, u32, u32, u32),
    session: &str,
) -> Result<u32> {
    let (r1, c1, r2, c2) = spec.source;
    ensure!(r1 <= r2 && c1 <= c2, "透视表源区域无效");
    let (or1, oc1, or2, oc2) = out_rect;
    ensure!(or1 <= or2 && oc1 <= oc2, "透视表输出区域无效");
    let plan = AttachmentPlan::read(path, sheet_name)?;
    let cache_id = plan.cache_id;
    let table_idx = plan.table_idx;
    let (def, records, def_table) = crate::pivot_parts2::build(
        path,
        sheet_name,
        spec,
        grid,
        out_rect,
        cache_id,
        &plan.table_name,
        &format!("sheetcore:{session}"),
    )?;
    write_parts(path, &plan, cache_id, table_idx, def, records, def_table)
}

/// 切换透视表布局（压缩 compact / 大纲 outline），只改 pivotTable*.xml 的四个属性。
///
/// Excel 的「压缩形式」= `compact="1" compactData="1" outline="0" outlineData="0"`；
/// 「大纲形式」= 我们生成时用的默认组合。实测 Excel 对两种组合都能正常打开并读回字段。
pub fn set_pivot_layout(path: &std::path::Path, compact: bool) -> anyhow::Result<usize> {
    use std::io::{Read, Write};
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        let name = e.name().to_string();
        let mut buf = Vec::with_capacity(e.size() as usize);
        e.read_to_end(&mut buf)?;
        entries.push((name, buf));
    }
    drop(zip);
    let mut changed = 0usize;
    for (name, buf) in entries.iter_mut() {
        if !name.starts_with("xl/pivotTables/") || !name.ends_with(".xml") {
            continue;
        }
        let Ok(text) = std::str::from_utf8(buf) else { continue };
        if !text.contains("<pivotTableDefinition") {
            continue;
        }
        let (c, cd, o, od) = if compact {
            ("1", "1", "0", "0")
        } else {
            ("0", "0", "1", "1")
        };
        let mut out = text.to_string();
        for (attr, val) in [
            ("compact", c),
            ("compactData", cd),
            ("outline", o),
            ("outlineData", od),
        ] {
            out = set_attr(&out, attr, val);
        }
        if out != *text {
            *buf = out.into_bytes();
            changed += 1;
        }
    }
    let tmp = path.with_extension("layout.tmp");
    {
        let mut w = zip::ZipWriter::new(std::fs::File::create(&tmp)?);
        for (name, buf) in &entries {
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file(name.clone(), opts)?;
            w.write_all(buf)?;
        }
        w.finish()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(changed)
}

/// 计算字段（Excel：cacheField 带 formula + databaseField="0"）
///
/// 只改缓存定义，缓存记录与数据区不动：Excel 打开后按公式自己算这一列。

/// 解析 `sheet` 上透视表对应的 (缓存定义部件, 透视表部件) 清单。
fn sheet_cache_targets(entries: &[(String, Vec<u8>)], sheet: &str) -> (Vec<String>, Vec<String>) {
    fn text_of<'a>(entries: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a str> {
        entries
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, b)| std::str::from_utf8(b).ok())
    }
    fn attr(tag: &str, key: &str) -> Option<String> {
        let pat = format!("{key}=\"");
        let i = tag.find(&pat)? + pat.len();
        let j = tag[i..].find('"')?;
        Some(tag[i..i + j].to_string())
    }
    /// 把 `../pivotTables/x.xml` 之类的相对 target 归一化成 zip 内的绝对名。
    fn join(base_dir: &str, target: &str) -> String {
        let mut parts: Vec<&str> = if target.starts_with('/') {
            Vec::new()
        } else {
            base_dir.split('/').filter(|p| !p.is_empty()).collect()
        };
        for seg in target.trim_start_matches('/').split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                s => parts.push(s),
            }
        }
        parts.join("/")
    }

    let mut caches = Vec::new();
    let mut tables = Vec::new();
    let Some(wb) = text_of(entries, "xl/workbook.xml") else {
        return (caches, tables);
    };
    let Some(wbrels) = text_of(entries, "xl/_rels/workbook.xml.rels") else {
        return (caches, tables);
    };
    let mut sheet_part = None;
    for tag in wb.split('<').filter(|t| t.starts_with("sheet ")) {
        if attr(tag, "name").as_deref() != Some(sheet) {
            continue;
        }
        let Some(rid) = attr(tag, "r:id") else { continue };
        for rel in wbrels.split('<').filter(|t| t.starts_with("Relationship ")) {
            if attr(rel, "Id").as_deref() != Some(rid.as_str()) {
                continue;
            }
            if let Some(t) = attr(rel, "Target") {
                sheet_part = Some(join("xl", &t));
            }
        }
    }
    let Some(sheet_part) = sheet_part else {
        return (caches, tables);
    };
    // 工作表 rels：找到它挂的 pivotTable 部件
    let (dir, file) = match sheet_part.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => ("xl", sheet_part.as_str()),
    };
    let rels_name = format!("{dir}/_rels/{file}.rels");
    let Some(srels) = text_of(entries, &rels_name) else {
        return (caches, tables);
    };
    let mut table_caches: Vec<String> = Vec::new();
    for rel in srels.split('<').filter(|t| t.starts_with("Relationship ")) {
        let Some(t) = attr(rel, "Target") else { continue };
        let part = join(dir, &t);
        if !part.starts_with("xl/pivotTables/") {
            continue;
        }
        if let Some(xml) = text_of(entries, &part) {
            for tag in xml.split('<').filter(|s| s.starts_with("pivotTableDefinition ")) {
                if let Some(cid) = attr(tag, "cacheId") {
                    table_caches.push(cid);
                }
            }
        }
        tables.push(part);
    }
    // cacheId → 缓存定义部件（workbook.xml 的 pivotCaches + rels）
    for tag in wb.split('<').filter(|t| t.starts_with("pivotCache ")) {
        let Some(cid) = attr(tag, "cacheId") else { continue };
        if !table_caches.iter().any(|c| c == &cid) {
            continue;
        }
        let Some(rid) = attr(tag, "r:id") else { continue };
        for rel in wbrels.split('<').filter(|t| t.starts_with("Relationship ")) {
            if attr(rel, "Id").as_deref() != Some(rid.as_str()) {
                continue;
            }
            if let Some(t) = attr(rel, "Target") {
                caches.push(join("xl", &t));
            }
        }
    }
    (caches, tables)
}

pub fn add_calculated_fields(
    path: &std::path::Path,
    calc: &[(&str, &str)],
) -> anyhow::Result<usize> {
    add_calculated_fields_inner(path, None, calc)
}

/// 只把计算字段写进 `sheet` 上那张透视表所属的缓存（多透视表共存时不再污染别人的缓存）。
pub fn add_calculated_fields_on(
    path: &std::path::Path,
    sheet: &str,
    calc: &[(&str, &str)],
) -> anyhow::Result<usize> {
    add_calculated_fields_inner(path, Some(sheet), calc)
}

fn add_calculated_fields_inner(
    path: &std::path::Path,
    sheet: Option<&str>,
    calc: &[(&str, &str)],
) -> anyhow::Result<usize> {
    use std::io::{Read, Write};
    if calc.is_empty() {
        return Ok(0);
    }
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        let name = e.name().to_string();
        let mut buf = Vec::with_capacity(e.size() as usize);
        e.read_to_end(&mut buf)?;
        entries.push((name, buf));
    }
    drop(zip);
    let targets = sheet.map(|sh| sheet_cache_targets(&entries, sh));
    let mut changed = 0usize;
    for (name, buf) in entries.iter_mut() {
        if !name.contains("pivotCacheDefinition") || !name.ends_with(".xml") {
            continue;
        }
        if let Some((caches, _)) = &targets {
            if !caches.iter().any(|c| c == name) {
                continue;
            }
        }
        let Ok(text) = std::str::from_utf8(buf) else { continue };
        let Some(open) = text.find("<cacheFields count=\"") else { continue };
        let count_start = open + "<cacheFields count=\"".len();
        let Some(count_len) = text[count_start..].find('"') else { continue };
        let old_count: usize = text[count_start..count_start + count_len].parse().unwrap_or(0);
        let Some(close) = text.find("</cacheFields>") else { continue };
        let mut add = String::new();
        for (n, f) in calc {
            add.push_str(&format!(
                "<cacheField name=\"{}\" numFmtId=\"0\" formula=\"{}\" databaseField=\"0\"/>",
                esc(n),
                esc(f)
            ));
        }
        let mut out = String::with_capacity(text.len() + add.len());
        out.push_str(&text[..count_start]);
        out.push_str(&(old_count + calc.len()).to_string());
        out.push_str(&text[count_start + count_len..close]);
        out.push_str(&add);
        out.push_str(&text[close..]);
        *buf = out.into_bytes();
        changed += 1;
    }
    // 透视表定义里的 pivotFields 条数必须与缓存字段数一致，否则 Excel 打不开（报「不能取得 Open 属性」）
    for (name, buf) in entries.iter_mut() {
        if !name.starts_with("xl/pivotTables/") || !name.ends_with(".xml") {
            continue;
        }
        if let Some((_, tables)) = &targets {
            if !tables.iter().any(|t| t == name) {
                continue;
            }
        }
        let Ok(text) = std::str::from_utf8(buf) else { continue };
        let Some(open) = text.find("<pivotFields count=\"") else { continue };
        let cs = open + "<pivotFields count=\"".len();
        let Some(cl) = text[cs..].find('"') else { continue };
        let old: usize = text[cs..cs + cl].parse().unwrap_or(0);
        let Some(close) = text.find("</pivotFields>") else { continue };
        let mut ins = String::new();
        for _ in 0..calc.len() {
            ins.push_str("<pivotField compact=\"0\" outline=\"0\" showAll=\"0\"/>");
        }
        let mut out = String::with_capacity(text.len() + ins.len());
        out.push_str(&text[..cs]);
        out.push_str(&(old + calc.len()).to_string());
        out.push_str(&text[cs + cl..close]);
        out.push_str(&ins);
        out.push_str(&text[close..]);
        *buf = out.into_bytes();
        changed += 1;
    }

    if changed == 0 {
        return Ok(0);
    }
    let tmp = path.with_extension("calc.tmp");
    {
        let mut w = zip::ZipWriter::new(std::fs::File::create(&tmp)?);
        for (name, buf) in &entries {
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file(name.clone(), opts)?;
            w.write_all(buf)?;
        }
        w.finish()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(changed)
}

/// 计算项（Excel：缓存 sharedItems 里加 `<s v="名字" f="1"/>`，再写 `<calculatedItems>`）
///
/// `field` 是轴上的字段名，`item_idx` 是它在 sharedItems 里的新下标，
/// `field_idx` 是这个字段在缓存里的序号（0 起），`pivot_field_idx` 是 pivotTable 里对应 pivotField 的序号。
pub fn add_calculated_item(
    path: &std::path::Path,
    field: &str,
    item: &str,
    formula: &str,
    field_idx: usize,
    item_idx: usize,
    pivot_field_idx: usize,
) -> anyhow::Result<usize> {
    use std::io::{Read, Write};
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        let name = e.name().to_string();
        let mut buf = Vec::with_capacity(e.size() as usize);
        e.read_to_end(&mut buf)?;
        entries.push((name, buf));
    }
    drop(zip);
    let mut changed = 0usize;

    for (name, buf) in entries.iter_mut() {
        if !name.contains("pivotCacheDefinition") || !name.ends_with(".xml") {
            continue;
        }
        let Ok(text) = std::str::from_utf8(buf) else { continue };
        // 注意：文本字段实际是 <cacheField name="X" numFmtId="0">，不能带 ">" 收尾
        let Some(fpos) = text.find(&format!("<cacheField name=\"{}\"", esc(field))) else {
            continue;
        };
        let Some(rel_end) = text[fpos..].find("</cacheField>") else { continue };
        let field_end = fpos + rel_end;
        let Some(rel_si) = text[fpos..field_end].find("<sharedItems") else { continue };
        let si_start = fpos + rel_si;
        // count 加一
        let mut out = String::with_capacity(text.len() + 128);
        out.push_str(&text[..field_end]);
        out.push_str(&format!("<s v=\"{}\" f=\"1\"/>", esc(item)));
        out.push_str(&text[field_end..]);
        let count_pat = "<sharedItems count=\"";
        if let Some(ci) = out[si_start..].find(count_pat) {
            let cs = si_start + ci + count_pat.len();
            if let Some(cl) = out[cs..].find('"') {
                let old: usize = out[cs..cs + cl].parse().unwrap_or(0);
                out.replace_range(cs..cs + cl, &(old + 1).to_string());
            }
        }
        // calculatedItems 必须排在 extLst 之前
        let anchor = out.find("<extLst").unwrap_or_else(|| out.rfind("</pivotCacheDefinition>").unwrap_or(out.len()));
        let block = format!(
            "<calculatedItems count=\"1\"><calculatedItem formula=\"{}\"><pivotArea cacheIndex=\"{}\" outline=\"0\" fieldPosition=\"0\"><references count=\"1\"><reference field=\"{}\" count=\"1\"><x v=\"{}\"/></reference></references></pivotArea></calculatedItem></calculatedItems>",
            esc(formula), field_idx, field_idx, item_idx
        );
        out.insert_str(anchor, &block);
        *buf = out.into_bytes();
        changed += 1;
    }

    for (name, buf) in entries.iter_mut() {
        if !name.starts_with("xl/pivotTables/") || !name.ends_with(".xml") {
            continue;
        }
        let Ok(text) = std::str::from_utf8(buf) else { continue };
        // 找第 pivot_field_idx 个 <pivotField ...>...</pivotField>
        let mut idx = 0usize;
        let mut start = 0usize;
        let mut target: Option<(usize, usize)> = None;
        // 注意：不能用 "<pivotField" 找，否则会先命中 "<pivotFields count=...>"
        while let Some(p) = text[start..].find("<pivotField ") {
            let abs = start + p;
            let Some(rel_close) = text[abs..].find("</pivotField>") else { break };
            let close = abs + rel_close;
            if idx == pivot_field_idx {
                target = Some((abs, close));
                break;
            }
            idx += 1;
            start = close + "</pivotField>".len();
        }
        let Some((ps, pe)) = target else { continue };
        let Some(rel_def) = text[ps..pe].find("<item t=\"default\"/>") else { continue };
        let insert_at = ps + rel_def;
        let mut out = String::with_capacity(text.len() + 32);
        out.push_str(&text[..insert_at]);
        out.push_str(&format!("<item f=\"1\" x=\"{}\"/>", item_idx));
        out.push_str(&text[insert_at..]);
        if let Some(ci) = out[ps..].find("<items count=\"") {
            let cs = ps + ci + "<items count=\"".len();
            if let Some(cl) = out[cs..].find('"') {
                let old: usize = out[cs..cs + cl].parse().unwrap_or(0);
                out.replace_range(cs..cs + cl, &(old + 1).to_string());
            }
        }
        *buf = out.into_bytes();
        changed += 1;
    }

    if changed == 0 {
        return Ok(0);
    }
    let tmp = path.with_extension("item.tmp");
    {
        let mut w = zip::ZipWriter::new(std::fs::File::create(&tmp)?);
        for (name, buf) in &entries {
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file(name.clone(), opts)?;
            w.write_all(buf)?;
        }
        w.finish()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(changed)
}

fn set_attr(xml: &str, name: &str, val: &str) -> String {
    let pat = format!("{name}=\"");
    if let Some(i) = xml.find(&pat) {
        let start = i + pat.len();
        if let Some(len) = xml[start..].find('"') {
            let mut s = String::with_capacity(xml.len());
            s.push_str(&xml[..start]);
            s.push_str(val);
            s.push_str(&xml[start + len..]);
            return s;
        }
    }
    xml.to_string()
}
