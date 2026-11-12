//! 跨工作簿引用：读/写 `xl/externalLinks`，刷新缓存，公式里 `[N]` 与文件名的互转。
//!
//! 文件格式（Excel 实测）：
//!
//! ```text
//! sheet1.xml      <c r="A1"><f>[1]预算!A1</f><v>10</v></c>       ← 方括号里是链接序号
//! workbook.xml    <externalReferences><externalReference r:id="rId2"/></externalReferences>
//! wb rels         rId2 → externalLinks/externalLink1.xml
//! externalLink1   <sheetNames><sheetName val="预算"/></sheetNames>
//!                 <sheetDataSet><sheetData sheetId="0"><row r="1">…</row></sheetData></sheetDataSet>
//! extLink rels    rId1 → ext_plan_src.xlsx（相对）  rId2 → file:///…（绝对）
//! ```
//!
//! 我们内部一律保留 `[N]` 原样，只有给人看的时候才换成文件名；写回绝不改序号。

use crate::model::{Value, Workbook};
use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// 一个外部工作簿（对应 `xl/externalLinks/externalLinkN.xml`）
#[derive(Clone, Debug, Default)]
pub struct ExternalBook {
    /// 1 起，与公式里的 `[N]` 对应
    pub index: usize,
    /// 包内部件名，例如 `xl/externalLinks/externalLink1.xml`
    pub part: String,
    /// 相对路径（externalLink rels 里 rId1 的 Target）
    pub target: String,
    /// 绝对路径（xxl21:absoluteUrl 指向的那个 file:/// URL）
    pub abs: Option<String>,
    /// 被引用表的名字，按 sheetId 顺序
    pub sheets: Vec<String>,
    /// (sheetId, row, col) → 缓存值
    pub cache: BTreeMap<(usize, u32, u32), Value>,
    /// 本轮是否从磁盘重读过（决定存盘要不要重写 sheetDataSet）
    pub refreshed: bool,
}

impl ExternalBook {
    pub fn file_name(&self) -> String {
        let t = self.target.replace('\\', "/");
        t.rsplit('/').next().unwrap_or(&t).to_string()
    }
}

/// 链接的可读名字：优先文件名，其次绝对路径，最后原样
pub fn label(link: &ExternalBook) -> String {
    let f = link.file_name();
    if f.is_empty() {
        link.abs.clone().unwrap_or_else(|| link.target.clone())
    } else {
        f
    }
}

// ---------------------------------------------------------------- 读

pub fn read(path: &Path) -> Vec<ExternalBook> {
    let Some(bytes) = crate::fixups::read_part(path, "xl/workbook.xml") else {
        return Vec::new();
    };
    let xml = String::from_utf8_lossy(&bytes).to_string();
    let ids = parse_external_ids(&xml);
    if ids.is_empty() {
        return Vec::new();
    }
    let rels = crate::fixups::read_part(path, "xl/_rels/workbook.xml.rels")
        .map(|b| parse_rels(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();

    let mut out = Vec::new();
    for (i, rid) in ids.iter().enumerate() {
        let Some((target, _)) = rels.get(rid) else { continue };
        let part = join_part("xl", target);
        let Some(pb) = crate::fixups::read_part(path, &part) else { continue };
        let pxml = String::from_utf8_lossy(&pb).to_string();
        let parsed = parse_link_part(&pxml);
        let prels = crate::fixups::read_part(path, &part_rels(&part))
            .map(|b| parse_rels(&String::from_utf8_lossy(&b)))
            .unwrap_or_default();
        let target = parsed
            .book_rid
            .as_ref()
            .and_then(|r| prels.get(r))
            .map(|(t, _)| t.clone())
            .unwrap_or_default();
        let abs = parsed
            .abs_rid
            .as_ref()
            .and_then(|r| prels.get(r))
            .map(|(t, _)| t.clone());
        out.push(ExternalBook {
            index: i + 1,
            part,
            target,
            abs,
            sheets: parsed.sheets,
            cache: parsed.cache,
            refreshed: false,
        });
    }
    out
}

struct ParsedPart {
    sheets: Vec<String>,
    book_rid: Option<String>,
    abs_rid: Option<String>,
    cache: BTreeMap<(usize, u32, u32), Value>,
}

fn parse_link_part(xml: &str) -> ParsedPart {
    let mut out = ParsedPart {
        sheets: Vec::new(),
        book_rid: None,
        abs_rid: None,
        cache: BTreeMap::new(),
    };
    let mut r = Reader::from_str(xml);
    let mut sheet_id: usize = 0;
    let mut cur: Option<(u32, u32)> = None;
    let mut cur_t = String::from("n");
    let mut text = String::new();
    let mut in_v = false;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let n = e.name().as_ref().to_string();
                match n.as_str() {
                    "externalBook" => {
                        if let Some(v) = attr(&e, "id") {
                            out.book_rid = Some(v);
                        }
                    }
                    "absoluteUrl" => {
                        if let Some(v) = attr(&e, "id") {
                            out.abs_rid = Some(v);
                        }
                    }
                    "sheetName" => {
                        if let Some(v) = attr(&e, "val") {
                            out.sheets.push(v);
                        }
                    }
                    "sheetData" => {
                        sheet_id = attr(&e, "sheetId")
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(0);
                    }
                    "cell" => {
                        cur = attr(&e, "r").and_then(|s| crate::table::parse_a1(&format!("{s}:{s}")).map(|a| (a.0, a.1)));
                        cur_t = attr(&e, "t").unwrap_or_else(|| "n".into());
                    }
                    "v" => {
                        in_v = true;
                        text.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let n = e.name().as_ref().to_string();
                if n == "sheetName" {
                    if let Some(v) = attr(&e, "val") {
                        out.sheets.push(v);
                    }
                } else if n == "externalBook" {
                    if let Some(v) = attr(&e, "id") {
                        out.book_rid = Some(v);
                    }
                } else if n == "absoluteUrl" {
                    if let Some(v) = attr(&e, "id") {
                        out.abs_rid = Some(v);
                    }
                } else if n == "sheetData" {
                    sheet_id = attr(&e, "sheetId")
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(0);
                }
            }
            Ok(Event::Text(t)) => {
                if in_v {
                    let raw = t.as_ref().to_string();
                    text.push_str(&crate::model::xml_unescape(&raw));
                }
            }
            Ok(Event::End(e)) => {
                let n = e.name().as_ref().to_string();
                if n == "v" {
                    if let Some((row, col)) = cur {
                        let v = match cur_t.as_str() {
                            "str" => Value::Text(text.clone().into_boxed_str()),
                            "b" => Value::Bool(text.trim() == "1"),
                            "e" => match crate::formula::ErrKind::from_text(&text) {
                                Some(k) => Value::Error(k),
                                None => Value::Text(text.clone().into_boxed_str()),
                            },
                            _ => text
                                .trim()
                                .parse::<f64>()
                                .map(Value::Number)
                                .unwrap_or(Value::Empty),
                        };
                        out.cache.insert((sheet_id, row, col), v);
                    }
                    in_v = false;
                } else if n == "cell" {
                    cur = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

/// workbook.xml 里 `<externalReferences>` 的 r:id 列表（按出现顺序）
fn parse_external_ids(xml: &str) -> Vec<String> {
    let mut r = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut inside = false;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let n = e.name().as_ref().to_string();
                if n == "externalReferences" {
                    inside = true;
                } else if inside && n == "externalReference" {
                    if let Some(v) = attr(&e, "id") {
                        out.push(v);
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let n = e.name().as_ref().to_string();
                if inside && n == "externalReference" {
                    if let Some(v) = attr(&e, "id") {
                        out.push(v);
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == "externalReferences" {
                    inside = false;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// rels 的 id → (Target, Type)
fn parse_rels(xml: &str) -> HashMap<String, (String, String)> {
    let mut r = Reader::from_str(xml);
    let mut out = HashMap::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.name().as_ref() == "Relationship" {
                    let id = attr(&e, "Id").unwrap_or_default();
                    let target = attr(&e, "Target").unwrap_or_default();
                    let ty = attr(&e, "Type").unwrap_or_default();
                    if !id.is_empty() {
                        out.insert(id, (target, ty));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref().ends_with(key))
        .map(|a| crate::model::xml_unescape(&a.value.to_string()))
}

fn join_part(base: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_string();
    }
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn part_rels(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/_rels/{name}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

// ---------------------------------------------------------------- 写缓存

/// 把刷新过的链接缓存写回 externalLink 部件（没刷新的一个字节都不动）
pub fn write_cache(path: &Path, links: &[ExternalBook]) -> Result<usize> {
    let mut n = 0;
    for l in links.iter() {
        if !l.refreshed || l.part.is_empty() {
            continue;
        }
        let Some(bytes) = crate::fixups::read_part(path, &l.part) else {
            continue;
        };
        let xml = String::from_utf8_lossy(&bytes).to_string();
        let new = replace_sheet_data(&xml, l);
        if new != xml {
            crate::patch::append_entry(path, &l.part, new.as_bytes())
                .with_context(|| format!("写回 {} 失败", l.part))?;
            n += 1;
        }
    }
    Ok(n)
}

pub fn replace_sheet_data(xml: &str, link: &ExternalBook) -> String {
    let body = sheet_data_xml(link);
    // 整段替换；自闭合 `<sheetDataSet/>` 也按段处理
    if let Some(start) = xml.find("<sheetDataSet") {
        let after = &xml[start..];
        let end = if let Some(i) = after.find("</sheetDataSet>") {
            Some(start + i + "</sheetDataSet>".len())
        } else {
            after.find("/>").map(|i| start + i + 2)
        };
        if let Some(end) = end {
            let mut out = String::with_capacity(xml.len() + body.len());
            out.push_str(&xml[..start]);
            out.push_str(&body);
            out.push_str(&xml[end..]);
            return out;
        }
    }
    if let Some(i) = xml.find("</externalBook>") {
        let mut out = String::with_capacity(xml.len() + body.len());
        out.push_str(&xml[..i]);
        out.push_str(&body);
        out.push_str(&xml[i..]);
        return out;
    }
    xml.to_string()
}

fn sheet_data_xml(link: &ExternalBook) -> String {
    // sheetId → row → cells
    let mut by_sheet: BTreeMap<usize, BTreeMap<u32, Vec<(u32, &Value)>>> = BTreeMap::new();
    for ((sid, r, c), v) in link.cache.iter() {
        by_sheet.entry(*sid).or_default().entry(*r).or_default().push((*c, v));
    }
    let mut out = String::from("<sheetDataSet>");
    for (sid, rows) in by_sheet {
        out.push_str(&format!("<sheetData sheetId=\"{sid}\">"));
        for (r, mut cells) in rows {
            cells.sort_by_key(|(c, _)| *c);
            out.push_str(&format!("<row r=\"{}\">", r + 1));
            for (c, v) in cells {
                out.push_str(&cell_xml(r, c, v));
            }
            out.push_str("</row>");
        }
        out.push_str("</sheetData>");
    }
    out.push_str("</sheetDataSet>");
    out
}

fn cell_xml(r: u32, c: u32, v: &Value) -> String {
    let name = format!("{}{}", crate::model::col_label(c), r + 1);
    match v {
        Value::Number(n) => format!("<cell r=\"{name}\"><v>{}</v></cell>", crate::model::format_number(*n)),
        Value::Text(t) => format!(
            "<cell r=\"{name}\" t=\"str\"><v>{}</v></cell>",
            xml_escape(t)
        ),
        Value::Bool(b) => format!("<cell r=\"{name}\" t=\"b\"><v>{}</v></cell>", if *b { 1 } else { 0 }),
        Value::Error(e) => format!("<cell r=\"{name}\" t=\"e\"><v>{}</v></cell>", e.text()),
        Value::Empty => String::new(),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------- 补部件

/// 让包里的 externalLink 部件与 `wb.links` 对齐（用户手输新工作簿、或界面上加了链接）。
///
/// 只补缺的：已有部件、已有关系、已有 Override 都不动；`<externalReferences>`
/// 只在条目数/顺序对不上时才整段重写。
pub fn ensure_parts(path: &Path, links: &[ExternalBook]) -> Result<usize> {
    if links.is_empty() {
        return Ok(0);
    }
    let Some(wb_bytes) = crate::fixups::read_part(path, "xl/workbook.xml") else {
        return Ok(0);
    };
    let wbxml = String::from_utf8_lossy(&wb_bytes).to_string();
    let mut ids = parse_external_ids(&wbxml);
    let rels_bytes = crate::fixups::read_part(path, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let mut rels = String::from_utf8_lossy(&rels_bytes).to_string();
    let mut rel_map = parse_rels(&rels);
    let ct_bytes = crate::fixups::read_part(path, "[Content_Types].xml").unwrap_or_default();
    let mut ct = String::from_utf8_lossy(&ct_bytes).to_string();
    let mut changed_parts = 0usize;

    for (i, link) in links.iter().enumerate() {
        let part = if link.part.is_empty() {
            format!("xl/externalLinks/externalLink{}.xml", i + 1)
        } else {
            link.part.clone()
        };
        // 部件不存在就写一份（含当前缓存）
        if crate::fixups::read_part(path, &part).is_none() {
            crate::patch::append_entry(path, &part, link_xml(link).as_bytes())
                .with_context(|| format!("写 {part} 失败"))?;
            let rels_name = part_rels(&part);
            crate::patch::append_entry(path, &rels_name, link_rels_xml(link).as_bytes())
                .with_context(|| format!("写 {rels_name} 失败"))?;
            changed_parts += 1;
        }
        // workbook 级关系
        let rel_target = part.strip_prefix("xl/").unwrap_or(&part).to_string();
        let existing = rel_map
            .iter()
            .find(|(_, (t, ty))| t.eq_ignore_ascii_case(&rel_target) && ty.ends_with("externalLink"))
            .map(|(id, _)| id.clone());
        let had_existing = existing.is_some();
        let rid = match existing {
            Some(id) => id,
            None => {
                let id = format!("rId{}", next_rid(&rels));
                let add = format!(
                    "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink\" Target=\"{rel_target}\"/>"
                );
                insert_before_close(&mut rels, "</Relationships>", &add);
                rel_map.insert(id.clone(), (rel_target.clone(), "externalLink".into()));
                id
            }
        };
        if i < ids.len() {
            if ids[i] != rid && had_existing {
                ids[i] = rid;
            }
        } else {
            ids.push(rid);
        }
        // Content_Types
        let pn = format!("/{part}");
        if !ct.contains(&format!("PartName=\"{pn}\"")) {
            let add = format!(
                "<Override PartName=\"{pn}\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml\"/>"
            );
            insert_before_close(&mut ct, "</Types>", &add);
        }
    }

    // workbook.xml 的 <externalReferences>：数量或顺序不对才重写
    let want: Vec<String> = ids.iter().take(links.len()).cloned().collect();
    let have = parse_external_ids(&wbxml);
    if have != want {
        let block = format!(
            "<externalReferences>{}</externalReferences>",
            want.iter()
                .map(|id| format!("<externalReference r:id=\"{id}\"/>"))
                .collect::<String>()
        );
        let new_wb = replace_or_insert_external_refs(&wbxml, &block);
        crate::patch::append_entry(path, "xl/workbook.xml", new_wb.as_bytes())?;
    }
    if !rels.is_empty() {
        let orig = String::from_utf8_lossy(&rels_bytes).to_string();
        if rels != orig {
            crate::patch::append_entry(path, "xl/_rels/workbook.xml.rels", rels.as_bytes())?;
        }
    }
    if !ct.is_empty() {
        let orig = String::from_utf8_lossy(&ct_bytes).to_string();
        if ct != orig {
            crate::patch::append_entry(path, "[Content_Types].xml", ct.as_bytes())?;
        }
    }
    Ok(changed_parts)
}

fn link_xml(link: &ExternalBook) -> String {
    let names: String = link
        .sheets
        .iter()
        .map(|s| format!("<sheetName val=\"{}\"/>", xml_escape(s)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>         <externalLink xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"          xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">         <externalBook r:id=\"rId1\"><sheetNames>{names}</sheetNames>{}
</externalBook></externalLink>",
        sheet_data_xml(link)
    )
}

fn link_rels_xml(link: &ExternalBook) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">         <Relationship Id=\"rId1\"          Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath\"          Target=\"{}\" TargetMode=\"External\"/></Relationships>",
        xml_escape(&link.target)
    )
}

fn insert_before_close(xml: &mut String, close: &str, add: &str) {
    if let Some(i) = xml.find(close) {
        xml.insert_str(i, add);
    }
}

fn replace_or_insert_external_refs(xml: &str, block: &str) -> String {
    if let Some(start) = xml.find("<externalReferences") {
        let after = &xml[start..];
        if let Some(i) = after.find("</externalReferences>") {
            let end = start + i + "</externalReferences>".len();
            return format!("{}{}{}", &xml[..start], block, &xml[end..]);
        }
        if let Some(i) = after.find("/>") {
            let end = start + i + 2;
            return format!("{}{}{}", &xml[..start], block, &xml[end..]);
        }
    }
    if let Some(i) = xml.find("<calcPr") {
        return format!("{}{}{}", &xml[..i], block, &xml[i..]);
    }
    if let Some(i) = xml.find("</workbook>") {
        return format!("{}{}{}", &xml[..i], block, &xml[i..]);
    }
    format!("{xml}{block}")
}

fn next_rid(rels: &str) -> u32 {
    let mut max = 0u32;
    let mut rest = rels;
    while let Some(i) = rest.find("Id=\"rId") {
        let after = &rest[i + 7..];
        let end = after.find('"').unwrap_or(0);
        if let Ok(n) = after[..end].parse::<u32>() {
            max = max.max(n);
        }
        rest = &after[end..];
    }
    max + 1
}

// ---------------------------------------------------------------- 刷新

pub struct RefreshReport {
    pub ok: usize,
    pub failed: Vec<(String, String)>,
}

/// 按链接里的 target 去磁盘上找文件，重读被引用表的已用区域填进缓存
pub fn refresh(wb: &mut Workbook) -> RefreshReport {
    let base = wb
        .path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let mut rep = RefreshReport { ok: 0, failed: Vec::new() };
    for link in wb.links.iter_mut() {
        let Some(path) = resolve_path(link, &base) else {
            rep.failed.push((label(link), "找不到文件".into()));
            continue;
        };
        match load_sheet_values(&path, &mut link.sheets) {
            Ok(vals) => {
                link.cache = vals;
                link.refreshed = true;
                rep.ok += 1;
            }
            Err(e) => rep.failed.push((label(link), format!("{e:#}"))),
        }
    }
    rep
}

fn resolve_path(link: &ExternalBook, base: &Path) -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if !link.target.is_empty() {
        cands.push(path_of(&link.target, base));
    }
    if let Some(abs) = &link.abs {
        cands.push(path_of(abs, base));
    }
    let fname = link.file_name();
    if !fname.is_empty() {
        cands.push(base.join(&fname));
    }
    cands.into_iter().find(|p| p.exists())
}

fn path_of(target: &str, base: &Path) -> PathBuf {
    let t = target.trim();
    // file:///F:\dir\a.xlsx 或 file:///F:/dir/a.xlsx
    let stripped = t
        .strip_prefix("file:///")
        .or_else(|| t.strip_prefix("file://"))
        .unwrap_or(t);
    let decoded = percent_decode(stripped);
    let p = PathBuf::from(decoded.replace('/', "\\"));
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn load_sheet_values(
    path: &Path,
    sheets: &mut Vec<String>,
) -> Result<BTreeMap<(usize, u32, u32), Value>> {
    use calamine::{open_workbook_auto, Reader};
    let mut book = open_workbook_auto(path)?;
    if sheets.is_empty() {
        *sheets = book.sheet_names().to_owned();
    }
    let names = book.sheet_names().to_owned();
    let mut out = BTreeMap::new();
    for (sid, want) in sheets.iter().enumerate() {
        let Some(name) = names.iter().find(|n| n.eq_ignore_ascii_case(want)) else {
            continue;
        };
        let range = match book.worksheet_range(name) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let (r0, c0) = range.start().unwrap_or((0, 0));
        for (row, col, data) in range.cells() {
            let v = data_to_value(data);
            if !v.is_empty() {
                out.insert((sid, r0 + row as u32, c0 + col as u32), v);
            }
        }
    }
    Ok(out)
}

fn data_to_value(d: &calamine::Data) -> Value {
    use calamine::Data;
    match d {
        Data::Int(i) => Value::Number(*i as f64),
        Data::Float(f) => Value::Number(*f),
        Data::String(s) => Value::Text(s.clone().into_boxed_str()),
        Data::Bool(b) => Value::Bool(*b),
        Data::DateTime(x) => Value::Number(x.as_f64()),
        Data::DateTimeIso(s) | Data::DurationIso(s) => Value::Text(s.clone().into_boxed_str()),
        Data::Error(e) => match e {
            calamine::CellErrorType::Div0 => Value::Error(crate::formula::ErrKind::Div0),
            calamine::CellErrorType::Name => Value::Error(crate::formula::ErrKind::Name),
            calamine::CellErrorType::NA => Value::Error(crate::formula::ErrKind::Na),
            calamine::CellErrorType::Num => Value::Error(crate::formula::ErrKind::Num),
            calamine::CellErrorType::Value => Value::Error(crate::formula::ErrKind::Value),
            calamine::CellErrorType::Ref => Value::Error(crate::formula::ErrKind::Ref),
            calamine::CellErrorType::Null => Value::Error(crate::formula::ErrKind::Null),
            _ => Value::Error(crate::formula::ErrKind::Value),
        },
        Data::Empty => Value::Empty,
    }
}

// ---------------------------------------------------------------- 公式里的书名

/// `[N]` 或 `[文件名]` → links 下标（0 基）
pub fn resolve_book(wb: &Workbook, name: &str) -> Option<usize> {
    let n = name.trim();
    if let Ok(i) = n.parse::<usize>() {
        if i >= 1 && i <= wb.links.len() {
            return Some(i - 1);
        }
    }
    let want = file_part(n);
    wb.links
        .iter()
        .position(|l| {
            l.target.eq_ignore_ascii_case(n)
                || l.file_name().eq_ignore_ascii_case(&want)
                || l.file_name().eq_ignore_ascii_case(n)
                || l.abs.as_deref().map(|a| a.eq_ignore_ascii_case(n)).unwrap_or(false)
        })
        .or_else(|| {
            // 带路径的写法：只看最后一段
            wb.links.iter().position(|l| l.file_name().eq_ignore_ascii_case(&want))
        })
}

fn file_part(s: &str) -> String {
    let t = s.replace('\\', "/");
    t.rsplit('/').next().unwrap_or(&t).to_string()
}

/// 表名 → sheetId（0 基）。没给表名时用第 0 张
pub fn sheet_slot(link: &ExternalBook, name: Option<&str>) -> Option<usize> {
    match name {
        None => Some(0),
        Some(n) => link
            .sheets
            .iter()
            .position(|s| s.eq_ignore_ascii_case(n.trim().trim_matches('\'')))
            .or(Some(0)),
    }
}

/// 给人看的公式：`[1]` → `[预算.xlsx]`
pub fn display_formula(wb: &Workbook, f: &str) -> String {
    map_books(f, |tok| {
        if let Ok(i) = tok.parse::<usize>() {
            if let Some(l) = wb.links.get(i.saturating_sub(1)) {
                return label(l);
            }
        }
        tok.to_string()
    })
}

/// 存盘/求值用的公式：`[预算.xlsx]` → `[1]`
pub fn raw_formula(wb: &Workbook, f: &str) -> String {
    map_books(f, |tok| match resolve_book(wb, tok) {
        Some(i) => (i + 1).to_string(),
        None => tok.to_string(),
    })
}

fn map_books<F: Fn(&str) -> String>(f: &str, map: F) -> String {
    let b: Vec<char> = f.chars().collect();
    let mut out = String::with_capacity(f.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == '[' {
            if let Some(close) = b[i + 1..].iter().position(|c| *c == ']') {
                let inner: String = b[i + 1..i + 1 + close].iter().collect();
                // 只换「书名」形态：数字序号、文件名、带路径的文件名；
                // 结构化引用（表[列]）里的列名不匹配任何链接，map 会原样返回
                out.push('[');
                out.push_str(&map(&inner));
                out.push(']');
                i += close + 2;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// 公式里出现没登记的工作簿时自动建链接，并尝试从磁盘读到表名和缓存。
///
/// 返回新增的链接数。Excel 里手输 `=[预算.xlsx]Sheet1!A1` 就是这个语义。
pub fn absorb_formula(wb: &mut Workbook, formula: &str) -> usize {
    let mut added = 0;
    for name in books_in_formula(formula) {
        if name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if resolve_book(wb, &name).is_some() {
            continue;
        }
        let fname = file_part(&name);
        if fname.is_empty() || fname.eq_ignore_ascii_case("") {
            continue;
        }
        wb.links.push(ExternalBook {
            index: wb.links.len() + 1,
            target: fname,
            ..Default::default()
        });
        added += 1;
    }
    if added > 0 {
        refresh(wb);
    }
    added
}

/// 这条公式是否引用了外部工作簿
pub fn is_external_formula(f: &str) -> bool {
    !books_in_formula(f).is_empty()
}

/// 公式里引用了哪些外部工作簿（用于"用户手输公式后自动建链接"）
pub fn books_in_formula(f: &str) -> Vec<String> {
    let b: Vec<char> = f.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '[' {
            if let Some(close) = b[i + 1..].iter().position(|c| *c == ']') {
                let inner: String = b[i + 1..i + 1 + close].iter().collect();
                let after = b.get(i + 1 + close + 1).copied();
                // 书名的特征是 `]` 后面还有表名和 `!`（`[1]预算!A1`），
                // 或者是纯数字序号；`表[列]` 后面不会有 `!`
                if !inner.is_empty()
                    && (inner.chars().all(|c| c.is_ascii_digit()) || has_bang_after(&b, i + close + 2))
                    && after.is_some()
                {
                    if !out.iter().any(|x| x.eq_ignore_ascii_case(&inner)) {
                        out.push(inner);
                    }
                }
                i += close + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn has_bang_after(b: &[char], from: usize) -> bool {
    let mut i = from;
    while i < b.len() && i < from + 80 {
        match b[i] {
            '!' => return true,
            '[' | ']' | ',' | '(' | ')' | '+' | '-' | '*' | '/' | '&' | '=' => return false,
            c if c.is_whitespace() => return false,
            _ => i += 1,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_ids_in_order() {
        let xml = r#"<workbook><sheets/><externalReferences><externalReference r:id="rId2"/><externalReference r:id="rId5"/></externalReferences></workbook>"#;
        assert_eq!(parse_external_ids(xml), vec!["rId2", "rId5"]);
    }

    #[test]
    fn parses_sheet_names_and_cache() {
        let xml = r#"<externalLink><externalBook r:id="rId1"><sheetNames><sheetName val="预算"/><sheetName val="明细"/></sheetNames><sheetDataSet><sheetData sheetId="0"><row r="1"><cell r="A1"><v>10</v></cell><cell r="B1" t="str"><v>预算</v></cell></row><row r="2"><cell r="A2"><v>20.5</v></cell></row></sheetData><sheetData sheetId="1"><row r="3"><cell r="C3" t="b"><v>1</v></cell></row></sheetData></sheetDataSet></externalBook></externalLink>"#;
        let p = parse_link_part(xml);
        assert_eq!(p.sheets, vec!["预算", "明细"]);
        assert!(matches!(p.cache.get(&(0, 0, 0)), Some(Value::Number(n)) if *n == 10.0));
        assert!(matches!(p.cache.get(&(0, 0, 1)), Some(Value::Text(t)) if &**t == "预算"));
        assert!(matches!(p.cache.get(&(1, 2, 2)), Some(Value::Bool(true))));
    }

    #[test]
    fn rewrites_sheet_data_only() {
        let xml = r#"<externalLink><externalBook r:id="rId1"><sheetNames><sheetName val="预算"/></sheetNames><sheetDataSet><sheetData sheetId="0" refreshError="1"/></sheetDataSet></externalBook></externalLink>"#;
        let mut link = ExternalBook { sheets: vec!["预算".into()], ..Default::default() };
        link.cache.insert((0, 0, 0), Value::Number(42.0));
        link.cache.insert((0, 0, 1), Value::Text("好".into()));
        let out = replace_sheet_data(xml, &link);
        assert!(out.contains("<cell r=\"A1\"><v>42</v></cell>"));
        assert!(out.contains("<cell r=\"B1\" t=\"str\"><v>好</v></cell>"));
        assert!(!out.contains("refreshError"), "旧缓存应被整段替换：{out}");
        assert_eq!(out.matches("<sheetDataSet").count(), 1);
        assert!(out.contains("<sheetName val=\"预算\"/>"), "表名要留着");
    }

    #[test]
    fn book_name_mapping() {
        let mut wb = Workbook::new_blank();
        wb.links.push(ExternalBook {
            index: 1,
            target: "预算.xlsx".into(),
            ..Default::default()
        });
        assert_eq!(display_formula(&wb, "=[1]Sheet1!A1"), "=[预算.xlsx]Sheet1!A1");
        assert_eq!(raw_formula(&wb, "=[预算.xlsx]Sheet1!A1"), "=[1]Sheet1!A1");
        // 结构化引用不动
        assert_eq!(display_formula(&wb, "=SUM(表[列])"), "=SUM(表[列])");
        assert_eq!(resolve_book(&wb, "预算.xlsx"), Some(0));
        assert_eq!(resolve_book(&wb, "1"), Some(0));
        assert_eq!(resolve_book(&wb, "别的.xlsx"), None);
    }

    #[test]
    fn finds_books_in_typed_formula() {
        assert_eq!(
            books_in_formula("=SUM([预算.xlsx]Sheet1!A1:A3)+[2]X!B2"),
            vec!["预算.xlsx", "2"]
        );
        assert!(books_in_formula("=SUM(表[列])").is_empty());
        assert_eq!(books_in_formula("=[预算.xlsx]明细!A1"), vec!["预算.xlsx"]);
    }

    #[test]
    fn joins_package_paths() {
        assert_eq!(join_part("xl", "externalLinks/externalLink1.xml"), "xl/externalLinks/externalLink1.xml");
        assert_eq!(join_part("xl", "../xl/externalLinks/e.xml"), "xl/externalLinks/e.xml");
        assert_eq!(part_rels("xl/externalLinks/externalLink1.xml"), "xl/externalLinks/_rels/externalLink1.xml.rels");
    }
}
