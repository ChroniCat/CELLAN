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
}

/// 工作表 XML 里的 `<hyperlink>` 原始条目
#[derive(Clone, Debug, Default)]
pub struct RawLink {
    pub r: u32,
    pub c: u32,
    pub rid: Option<String>,
    pub location: Option<String>,
    pub tooltip: Option<String>,
}

pub fn parse_sheet(xml: &str) -> Vec<RawLink> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(i) = xml[from..].find("<hyperlink ") {
        let i = from + i;
        let Some(gt) = xml[i..].find('>').map(|e| i + e) else {
            break;
        };
        let tag = &xml[i..=gt];
        let attr = |name: &str| -> Option<String> {
            let pat = format!("{name}=\"");
            let p = tag.find(&pat)? + pat.len();
            let rest = &tag[p..];
            let q = rest.find('"')?;
            Some(rest[..q].to_string())
        };
        if let Some((r, c)) = attr("ref").as_deref().and_then(parse_a1) {
            out.push(RawLink {
                r,
                c,
                rid: attr("r:id"),
                location: attr("location"),
                tooltip: attr("tooltip"),
            });
        }
        from = gt + 1;
    }
    out
}

fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let mut c = 0u32;
    let mut i = 0usize;
    let b = s.as_bytes();
    while i < b.len() && b[i].is_ascii_alphabetic() {
        c = c * 26 + (b[i].to_ascii_uppercase() - b'A' + 1) as u32;
        i += 1;
    }
    if i == 0 || i == b.len() {
        return None;
    }
    let r = s[i..].parse::<u32>().ok()?;
    if r == 0 {
        return None;
    }
    Some((r - 1, c - 1))
}

/// 读工作表 rels：rId → (Target, 是否外部)
pub fn read_rels(path: &Path, sheet_part: &str) -> HashMap<String, (String, bool)> {
    let mut map = HashMap::new();
    let Some(body) = crate::fixups::read_part_file(path, &rels_part(sheet_part)) else {
        return map;
    };
    let Ok(xml) = String::from_utf8(body) else {
        return map;
    };
    let mut from = 0usize;
    while let Some(i) = xml[from..].find("<Relationship ") {
        let i = from + i;
        let Some(gt) = xml[i..].find('>').map(|e| i + e) else {
            break;
        };
        let tag = &xml[i..=gt];
        let attr = |name: &str| -> Option<String> {
            let pat = format!("{name}=\"");
            let p = tag.find(&pat)? + pat.len();
            let rest = &tag[p..];
            let q = rest.find('"')?;
            Some(rest[..q].to_string())
        };
        if let (Some(id), Some(target)) = (attr("Id"), attr("Target")) {
            let external = attr("TargetMode").as_deref() == Some("External");
            map.insert(id, (target, external));
        }
        from = gt + 1;
    }
    map
}

pub fn rels_part(sheet_part: &str) -> String {
    match sheet_part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{sheet_part}.rels"),
    }
}

/// 把模型里的链接写回工作表（只处理 dirty 的表）
pub fn apply(path: &Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    if !wb.sheets.iter().any(|s| s.hyperlinks_dirty) {
        return Ok(());
    }
    let targets = crate::patch::sheet_targets(path)?;
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    for sheet in &wb.sheets {
        if !sheet.hyperlinks_dirty {
            continue;
        }
        let Some(part) = targets.get(&sheet.name) else {
            continue;
        };
        let Some(xml) = crate::fixups::read_part_file(path, part).and_then(|b| String::from_utf8(b).ok())
        else {
            continue;
        };
        let rels_name = rels_part(part);
        let rels_xml = crate::fixups::read_part_file(path, &rels_name)
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_else(|| "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".into());
        // 去掉旧的 hyperlink 关系，保留根元素、图片/图表等
        let mut kept = String::new();
        {
            let mut rest = rels_xml.as_str();
            while let Some(i) = rest.find("<Relationship ") {
                kept.push_str(&rest[..i]);
                let Some(gt) = rest[i..].find('>').map(|e| i + e + 1) else {
                    kept.push_str(&rest[i..]);
                    rest = "";
                    break;
                };
                let tag = &rest[i..gt];
                if !tag.contains("/hyperlink\"") {
                    kept.push_str(tag);
                }
                rest = &rest[gt..];
            }
            kept.push_str(rest);
        }
        if !kept.contains("</Relationships>") {
            kept = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{}</Relationships>",
                kept
            );
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
            let a1 = crate::model::cell_name(r, c);
            if !link.target.is_empty() {
                n += 1;
                let rid = format!("rIdHk{n}");
                links_xml.push_str(&format!(
                    "<hyperlink ref=\"{a1}\" r:id=\"{rid}\"{} />",
                    if link.tooltip.is_empty() {
                        String::new()
                    } else {
                        format!(" tooltip=\"{}\"", esc(&link.tooltip))
                    }
                ));
                rels_add.push_str(&format!(
                    "<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"{}\" TargetMode=\"External\"/>",
                    esc(&link.target)
                ));
            } else if !link.location.is_empty() {
                links_xml.push_str(&format!(
                    "<hyperlink ref=\"{a1}\" location=\"{}\"{} />",
                    esc(&link.location),
                    if link.tooltip.is_empty() {
                        String::new()
                    } else {
                        format!(" tooltip=\"{}\"", esc(&link.tooltip))
                    }
                ));
            }
        }
        let mut new_xml = strip_links(&xml);
        if !links_xml.is_empty() {
            let block = format!("<hyperlinks>{links_xml}</hyperlinks>");
            let at = find_insert(&new_xml);
            new_xml.insert_str(at, &block);
        }
        pending.push((part.clone(), new_xml.into_bytes()));
        if !rels_add.is_empty() || rels_xml != kept {
            let new_rels = kept.replace("</Relationships>", &format!("{rels_add}</Relationships>"));
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
