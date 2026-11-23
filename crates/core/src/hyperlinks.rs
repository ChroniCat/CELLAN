//! 单元格超链接：读 `<hyperlinks>` + 工作表 rels，写回时重建这两块。
//!
//! 外部链接用 `r:id` 指到 `_rels/sheetN.xml.rels` 里的 Target；
//! 内部链接（本簿某处）用 `location`。Excel 两种都认。

use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Hyperlink {
    /// 外部地址（http(s)://、mailto:、相对文件路径…）
    pub target: String,
    /// 本簿内位置（#Sheet1!A1）
    pub location: String,
    pub tooltip: String,
    pub external: bool,
    /// Inclusive distance from the anchor to the end of an OOXML ref rectangle.
    pub span: (u32, u32),
    /// Additional OOXML attributes such as display.
    pub attributes: Vec<(String, String)>,
}

/// 工作表 XML 里的 `<hyperlink>` 原始条目
#[derive(Clone, Debug, Default)]
pub struct RawLink {
    pub r: u32,
    pub c: u32,
    pub rid: Option<String>,
    pub location: Option<String>,
    pub tooltip: Option<String>,
    pub span: (u32, u32),
    pub attributes: Vec<(String, String)>,
}

fn namespaces(tag: &quick_xml::events::BytesStart<'_>, out: &mut HashMap<String, String>) {
    for attr in tag.attributes().flatten() {
        if let Some(prefix) = attr.key.as_ref().strip_prefix("xmlns:") {
            out.insert(prefix.to_owned(), crate::model::xml_unescape(&attr.value));
        }
    }
}

fn visit_links(xml: &str, mut visit: impl FnMut(usize, usize, &quick_xml::events::BytesStart<'_>, &HashMap<String, String>)) {
    use quick_xml::events::Event;
    if !xml.contains("hyperlink") { return; }
    let mut bindings = HashMap::new();
    let mut head = quick_xml::Reader::from_str(xml);
    loop {
        match head.read_event() {
            Ok(Event::Start(root)) => { namespaces(&root, &mut bindings); break; }
            Ok(Event::Eof) | Err(_) => return,
            _ => {}
        }
    }
    let offset = xml.find("</sheetData>").map_or(0, |i| i + "</sheetData>".len());
    let mut reader = quick_xml::Reader::from_str(&xml[offset..]);
    loop {
        let start = offset + reader.buffer_position() as usize;
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == "hyperlinks" =>
                namespaces(&e, &mut bindings),
            Ok(Event::Empty(e)) if e.local_name().as_ref() == "hyperlink" =>
                visit(start, offset + reader.buffer_position() as usize, &e, &bindings),
            Ok(Event::Start(e)) if e.local_name().as_ref() == "hyperlink" => {
                if reader.read_to_end(e.name()).is_err() { break; }
                visit(start, offset + reader.buffer_position() as usize, &e, &bindings);
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == "hyperlinks" => break,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

pub fn parse_sheet(xml: &str) -> Vec<RawLink> {
    let mut out = Vec::new();
    visit_links(xml, |_, _, tag, inherited| {
        let mut link = RawLink::default();
        let mut anchor = None;
        let mut bindings = inherited.clone();
        namespaces(tag, &mut bindings);
        for a in tag.attributes().flatten() {
            let name = a.key.as_ref();
            let value = crate::model::xml_unescape(&a.value);
            match name {
                "ref" => {
                    let (first, last) = value.split_once(':').unwrap_or((&value, &value));
                    if let (Some((r, c)), Some((r2, c2))) = (parse_a1(first), parse_a1(last)) {
                        if r2 >= r && c2 >= c {
                            anchor = Some((r, c));
                            link.span = (r2 - r, c2 - c);
                        }
                    }
                }
                "location" => link.location = Some(value),
                "tooltip" => link.tooltip = Some(value),
                n if n == "r:id" || n.strip_suffix(":id").and_then(|p| bindings.get(p))
                    .is_some_and(|ns| ns.ends_with("/relationships")) => link.rid = Some(value),
                n if n.starts_with("xmlns:") => {}
                n => {
                    if let Some((prefix, _)) = n.split_once(':') {
                        if let Some(uri) = bindings.get(prefix) {
                            let declaration = (format!("xmlns:{prefix}"), uri.clone());
                            if !link.attributes.contains(&declaration) { link.attributes.push(declaration); }
                        }
                    }
                    link.attributes.push((n.to_owned(), value));
                }
            }
        }
        if let Some((r, c)) = anchor {
            (link.r, link.c) = (r, c);
            out.push(link);
        }
    });
    out
}

/// umya expects one cell per hyperlink and panics on rectangular references.
pub(crate) fn remove_range_links(xml: &mut String) -> bool {
    let mut ranges = Vec::new();
    visit_links(xml, |start, end, tag, _| {
        if tag.attributes().flatten().any(|a| a.key.as_ref() == "ref" && a.value.contains(':')) {
            ranges.push(start..end);
        }
    });
    let changed = !ranges.is_empty();
    for range in ranges.into_iter().rev() { xml.replace_range(range, ""); }
    changed
}

fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let mut c = 0u32;
    let mut i = 0usize;
    let s = s.strip_prefix('$').unwrap_or(s);
    let b = s.as_bytes();
    while i < b.len() && b[i].is_ascii_alphabetic() {
        c = c.checked_mul(26)?.checked_add((b[i].to_ascii_uppercase() - b'A' + 1) as u32)?;
        i += 1;
    }
    if i == 0 || i == b.len() {
        return None;
    }
    let digits = s[i..].strip_prefix('$').unwrap_or(&s[i..]);
    let r = digits.parse::<u32>().ok()?;
    if r == 0 {
        return None;
    }
    Some((r - 1, c - 1))
}

/// 读工作表 rels：rId → (Target, 是否外部)
pub fn read_rels(path: &Path, sheet_part: &str) -> HashMap<String, (String, bool)> {
    let mut map = HashMap::new();
    let Some(body) = crate::fixups::read_part(path, &rels_part(sheet_part)) else {
        return map;
    };
    let Ok(xml) = crate::xlsx::patch_xml_text(&body,sheet_part) else {
        return map;
    };
    if let Ok(doc) = roxmltree::Document::parse(&xml) {
        for tag in doc.root_element().children().filter(|tag| tag.tag_name().name() == "Relationship") {
            if let (Some(id), Some(target)) = (tag.attribute("Id"), tag.attribute("Target")) {
                map.insert(id.to_owned(), (target.to_owned(), tag.attribute("TargetMode") == Some("External")));
            }
        }
    }
    map
}

pub fn rels_part(sheet_part: &str) -> String {
    match sheet_part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{sheet_part}.rels"),
    }
}

pub(crate) fn load_sheet(path: &Path, part: &str, xml: &str) -> HashMap<(u32, u32), Hyperlink> {
    let raw = parse_sheet(xml);
    let rels = if raw.iter().any(|link| link.rid.is_some()) {
        read_rels(path, part)
    } else { HashMap::new() };
    raw.into_iter().filter_map(|link| {
        let (target, external) = link.rid.as_ref().and_then(|id| rels.get(id))
            .cloned().unwrap_or_default();
        let location = link.location.unwrap_or_default();
        (!target.is_empty() || !location.is_empty()).then(|| ((link.r, link.c), Hyperlink {
            target, location, external, tooltip: link.tooltip.unwrap_or_default(),
            span: link.span, attributes: link.attributes,
        }))
    }).collect()
}

/// 把模型里的链接写回工作表（只处理 dirty 的表）
pub fn apply(path: &Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    apply_after_save(path, wb, false)
}

pub(crate) fn apply_after_save(
    path: &Path, wb: &crate::model::Workbook, rebuilt: bool,
) -> anyhow::Result<()> {
    // A rebuilt sheet may have lost its original relationship IDs. Check only
    // those sheets before deciding whether a repair is actually necessary.
    let needs_check = |s: &crate::model::Sheet| s.hyperlinks_dirty
        || (rebuilt && !s.hyperlinks.is_empty() && !crate::fixups::can_restore_sheet_xml(s));
    if !wb.sheets.iter().any(&needs_check) {
        return Ok(());
    }
    let targets = crate::patch::sheet_targets(path)?;
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    for sheet in &wb.sheets {
        if !needs_check(sheet) {
            continue;
        }
        let Some(part) = targets.get(&sheet.name) else {
            continue;
        };
        let bytes = crate::fixups::read_part(path, part).ok_or_else(|| anyhow::anyhow!("缺少工作表 {part}"))?;
        let xml = crate::xlsx::patch_xml_text(&bytes, part)?;
        if !sheet.hyperlinks_dirty && load_sheet(path, part, &xml) == *sheet.hyperlinks {
            continue;
        }
        let rels_name = rels_part(part);
        let rels_xml = crate::fixups::read_part(path, &rels_name)
            .map(|b| crate::xlsx::patch_xml_text(&b,&rels_name).map(|s| s.into_owned())).transpose()?
            .unwrap_or_else(|| "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".into());
        // 去掉旧的 hyperlink 关系，保留根元素、图片/图表等
        let doc = roxmltree::Document::parse(&rels_xml)?;
        let root = doc.root_element();
        let mut kept = rels_xml.clone();
        let mut taken_ids = std::collections::HashSet::new();
        let mut remove = Vec::new();
        for rel in root.children().filter(|rel| rel.tag_name().name() == "Relationship") {
            if rel.attribute("Type").is_some_and(|ty| ty.ends_with("/hyperlink")) {
                remove.push(rel.range());
            } else if let Some(id) = rel.attribute("Id") {
                taken_ids.insert(id);
            }
        }
        for range in remove.into_iter().rev() { kept.replace_range(range, ""); }
        let root_name = root.tag_name().name();
        let root_qname = root.tag_name().namespace().and_then(|uri| root.lookup_prefix(uri))
            .map_or_else(|| root_name.to_owned(), |prefix| format!("{prefix}:{root_name}"));
        let rel_qname = root_qname.rsplit_once(':')
            .map_or_else(|| "Relationship".to_owned(), |(prefix, _)| format!("{prefix}:Relationship"));
        if !kept.contains(&format!("</{root_qname}>")) {
            let pos = kept.rfind("/>").ok_or_else(|| anyhow::anyhow!("超链接关系缺少根元素"))?;
            kept.replace_range(pos..pos + 2, &format!("></{root_qname}>"));
        }
        let mut links_xml = String::new();
        let mut rels_add = String::new();
        let mut items: Vec<((u32, u32), &Hyperlink)> = sheet
            .hyperlinks
            .iter()
            .map(|(k, v)| (*k, v))
            .collect();
        items.sort_by_key(|(k, _)| *k);
        let mut n = 0usize;
        for ((r, c), link) in items {
            let mut reference = crate::model::cell_name(r, c);
            if link.span != (0, 0) {
                reference.push(':');
                reference.push_str(&crate::model::cell_name(r.saturating_add(link.span.0), c.saturating_add(link.span.1)));
            }
            links_xml.push_str(&format!("<hyperlink ref=\"{reference}\""));
            if !link.target.is_empty() {
                let rid = loop {
                    n += 1;
                    let rid = format!("rIdHk{n}");
                    if !taken_ids.contains(rid.as_str()) { break rid; }
                };
                links_xml.push_str(&format!(" r:id=\"{rid}\""));
                rels_add.push_str(&format!(
                    "<{rel_qname} Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"{}\"{}/>",
                    esc(&link.target), if link.external { " TargetMode=\"External\"" } else { "" }
                ));
            }
            if !link.location.is_empty() {
                links_xml.push_str(&format!(" location=\"{}\"", esc(&link.location)));
            }
            if !link.tooltip.is_empty() {
                links_xml.push_str(&format!(" tooltip=\"{}\"", esc(&link.tooltip)));
            }
            for (name, value) in &link.attributes {
                links_xml.push_str(&format!(" {name}=\"{}\"", esc(value)));
            }
            links_xml.push_str("/>");
        }
        let mut new_xml = strip_links(&xml);
        if !links_xml.is_empty() {
            let ns = if !rels_add.is_empty() {
                " xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\""
            } else { "" };
            let block = format!("<hyperlinks{ns}>{links_xml}</hyperlinks>");
            let at = find_insert(&new_xml);
            new_xml.insert_str(at, &block);
        }
        pending.push((part.clone(), new_xml.into_bytes()));
        if !rels_add.is_empty() || rels_xml != kept {
            let end_tag = format!("</{root_qname}>");
            let new_rels = kept.replace(&end_tag, &format!("{rels_add}{end_tag}"));
            pending.push((rels_name, new_rels.into_bytes()));
        }
    }
    if !pending.is_empty() {
        crate::patch::append_entries(path, &pending)?;
    }
    Ok(())
}

/// 删掉旧的 `<hyperlinks>…</hyperlinks>`
fn strip_links(xml: &str) -> String {
    let mut x = xml.to_string();
    while let Some(i) = x.find("<hyperlinks") {
        let Some(gt) = x[i..].find('>').map(|e| i + e) else {
            break;
        };
        if x[i..=gt].ends_with("/>") {
            x.replace_range(i..=gt, "");
            continue;
        }
        let Some(end) = x[gt..].find("</hyperlinks>").map(|e| gt + e + "</hyperlinks>".len()) else {
            break;
        };
        x.replace_range(i..end, "");
    }
    x
}

/// schema 位置：在 dataValidations 之后，printOptions/pageMargins/pageSetup 之前
fn find_insert(xml: &str) -> usize {
    for tag in [
        "<printOptions",
        "<pageMargins",
        "<pageSetup",
        "<headerFooter",
        "<rowBreaks",
        "<colBreaks",
        "<drawing",
        "<legacyDrawing",
        "<tableParts",
        "</worksheet>",
    ] {
        if let Some(i) = xml.find(tag) {
            return i;
        }
    }
    xml.len()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hyperlink_tags() {
        let xml = r#"<worksheet><hyperlinks><hyperlink ref="A1" r:id="rId1"/><hyperlink ref="B2" location="Sheet1!A1" tooltip="内部"/></hyperlinks></worksheet>"#;
        let list = parse_sheet(xml);
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].r, list[0].c), (0, 0));
        assert_eq!(list[0].rid.as_deref(), Some("rId1"));
        assert_eq!(list[1].location.as_deref(), Some("Sheet1!A1"));
    }

    #[test]
    fn rels_part_path() {
        assert_eq!(rels_part("xl/worksheets/sheet1.xml"), "xl/worksheets/_rels/sheet1.xml.rels");
    }
}
