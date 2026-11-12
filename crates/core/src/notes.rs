//! 单元格批注（xlsx 的 comments 部件）。
//!
//! 读取要绕一圈：sheet XML 本身没有批注内容，得通过 worksheet 的 rels
//! 找到 `comments*.xml`；作者名在 `<authors>` 里按 authorId 索引。

use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub r: u32,
    pub c: u32,
    pub text: String,
    pub author: String,
}

/// 表名 → 批注列表
pub fn load_notes(path: &Path) -> Result<HashMap<String, Vec<Note>>> {
    let targets = crate::patch::sheet_targets(path)?;
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut out = HashMap::new();
    for (name, sheet_path) in targets.iter() {
        // xl/worksheets/sheet1.xml → xl/worksheets/_rels/sheet1.xml.rels
        let rels = match sheet_path.rsplit_once('/') {
            Some((dir, base)) => format!("{dir}/_rels/{base}.rels"),
            None => continue,
        };
        let mut xml = String::new();
        {
            let Ok(mut f) = zip.by_name(&rels) else { continue };
            let _ = f.read_to_string(&mut xml);
        }
        let Some(target) = find_comments_target(&xml) else { continue };
        let entry = if target.starts_with('/') {
            target.trim_start_matches('/').to_string()
        } else {
            format!("xl/{target}")
        };
        let mut cx = String::new();
        {
            let Ok(mut f) = zip.by_name(&entry) else { continue };
            let _ = f.read_to_string(&mut cx);
        }
        let notes = parse_comments(&cx);
        if !notes.is_empty() {
            out.insert(name.clone(), notes);
        }
    }
    Ok(out)
}

fn find_comments_target(rels_xml: &str) -> Option<String> {
    let mut r = Reader::from_str(rels_xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("Relationship") => {
                let mut ty = String::new();
                let mut target = String::new();
                for a in e.attributes().flatten() {
                    let k = a.key.as_ref();
                    let v = crate::model::xml_unescape(&a.value.to_string());
                    if k == "Type" {
                        ty = v;
                    } else if k == "Target" {
                        target = v;
                    }
                }
                if ty.ends_with("/comments") && !target.is_empty() {
                    // ../comments/comment1.xml → comments/comment1.xml
                    let t = target.trim_start_matches("../").to_string();
                    return Some(t);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    None
}

/// 取本地名（去掉前缀）—— `ends_with("t")` 会把 "comment" 也算成 t
fn local<'a>(name: &'a str) -> &'a str {
    match name.rsplit_once(':') {
        Some((_, l)) => l,
        None => name,
    }
}

pub fn parse_comments(xml: &str) -> Vec<Note> {
    let mut r = Reader::from_str(xml);
    let mut authors: Vec<String> = Vec::new();
    let mut notes: Vec<Note> = Vec::new();
    let mut in_authors = false;
    let mut in_author = false;
    let mut author_buf = String::new();
    let mut cur: Option<Note> = None;
    let mut author_id = 0usize;
    let mut in_text = false;
    let mut in_t = false;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref());
                if name == "authors" {
                    in_authors = true;
                } else if in_authors && name == "author" {
                    in_author = true;
                    author_buf.clear();
                } else if name == "comment" {
                    let mut coord = String::new();
                    let mut aid = 0usize;
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        if k == "ref" {
                            coord = v;
                        } else if k == "authorId" {
                            aid = v.parse().unwrap_or(0);
                        }
                    }
                    if let Some((row, col)) = parse_a1(&coord) {
                        author_id = aid;
                        cur = Some(Note { r: row, c: col, text: String::new(), author: String::new() });
                    }
                } else if cur.is_some() && name == "text" {
                    in_text = true;
                } else if in_text && name == "t" {
                    in_t = true;
                }
            }
            Ok(Event::Text(t)) => {
                let raw: &str = &t;
                let txt = crate::model::xml_unescape(raw);
                if in_author {
                    author_buf.push_str(&txt);
                } else if in_t {
                    if let Some(n) = cur.as_mut() {
                        n.text.push_str(&txt);
                    }
                }
            }
            Ok(Event::End(e)) => {
                let nm = e.name();
                let name = local(nm.as_ref());
                if name == "author" && in_author {
                    authors.push(author_buf.trim().to_string());
                    in_author = false;
                } else if name == "authors" {
                    in_authors = false;
                } else if name == "t" {
                    in_t = false;
                } else if name == "text" {
                    in_text = false;
                } else if name == "comment" {
                    if let Some(mut n) = cur.take() {
                        n.author = authors.get(author_id).cloned().unwrap_or_default();
                        if !n.text.is_empty() {
                            notes.push(n);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    notes
}

fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let mut col = 0u32;
    let mut row = 0u32;
    let mut digits = false;
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() && !digits {
            col = col * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
        } else if ch.is_ascii_digit() {
            digits = true;
            row = row * 10 + (ch as u32 - '0' as u32);
        } else if ch == '$' {
            continue;
        } else {
            return None;
        }
    }
    if col == 0 || row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_xml() {
        let xml = r#"<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
          <authors><author>张三</author><author>李四</author></authors>
          <commentList>
            <comment ref="B2" authorId="0"><text><r><t>这行要复核</t></r></text></comment>
            <comment ref="A3" authorId="1"><text><r><t>第一段</t></r><r><t>第二段</t></r></text></comment>
          </commentList>
        </comments>"#;
        let notes = parse_comments(xml);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].r, 1);
        assert_eq!(notes[0].c, 1);
        assert_eq!(notes[0].author, "张三");
        assert_eq!(notes[0].text, "这行要复核");
        assert_eq!(notes[1].text, "第一段第二段", "多段富文本要拼起来");
        assert_eq!(notes[1].author, "李四");
    }

    #[test]
    fn rela_paths() {
        let rels = r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments/comment1.xml"/></Relationships>"#;
        assert_eq!(find_comments_target(rels), Some("comments/comment1.xml".into()));
    }
}

/// 删掉某张表的批注部件（comments + VML）及其工作表关系。
/// umya 只会清空它模型里的 comments，旧部件仍留在包里，重开就会"复活"。
pub fn clear_sheet_notes(path: &std::path::Path, sheet_name: &str) -> anyhow::Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    let Some(part) = targets.get(sheet_name) else { return Ok(()) };
    let (dir, file) = match part.rsplit_once('/') {
        Some(v) => v,
        None => return Ok(()),
    };
    let rels_part = format!("{dir}/_rels/{file}.rels");
    let Some(rels) = crate::fixups::read_part(path, &rels_part).and_then(|b| String::from_utf8(b).ok())
    else {
        return Ok(());
    };
    // 按 XML 节点删，不做字符串切分：根标签是 <Relationships>，
    // 前缀匹配 <Relationship 会把根开始标签一起吃掉，生成非法 XML。
    let doc = match roxmltree::Document::parse(&rels) {
        Ok(d) => d,
        Err(e) => anyhow::bail!("关系部件 {rels_part} 不是合法 XML：{e}"),
    };
    let mut removed: Vec<String> = Vec::new();
    let mut keep_ids: Vec<String> = Vec::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("Relationship")) {
        let ty = node.attribute("Type").unwrap_or("");
        if ty.ends_with("/comments") || ty.ends_with("/vmlDrawing") {
            // 只有包内关系才解析成路径；External 的 Target 是 URL
            if node.attribute("TargetMode") != Some("External") {
                if let Some(t) = node.attribute("Target") {
                    removed.push(resolve(dir, t));
                }
            }
        } else if let Some(id) = node.attribute("Id") {
            keep_ids.push(id.to_string());
        }
    }
    if removed.is_empty() {
        return Ok(());
    }
    for p in &removed {
        let _ = crate::patch::remove_entry(path, p);
    }
    // 重建关系部件：保留原有根属性（命名空间），只写没删的关系
    let root = doc.root_element();
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>");
    out.push_str("<Relationships");
    // roxmltree 的 attributes() 不含命名空间声明，必须单独补回，
    // 否则重建出来的关系部件没有 xmlns，Excel 视为非法包。
    for ns in root.namespaces() {
        match ns.name() {
            Some(prefix) => out.push_str(&format!(" xmlns:{}=\"{}\"", prefix, xml_escape(ns.uri()))),
            None => out.push_str(&format!(" xmlns=\"{}\"", xml_escape(ns.uri()))),
        }
    }
    for a in root.attributes() {
        out.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
    }
    out.push('>');
    for node in doc.descendants().filter(|n| n.has_tag_name("Relationship")) {
        let Some(id) = node.attribute("Id") else { continue };
        if !keep_ids.iter().any(|k| k == id) {
            continue;
        }
        out.push_str("<Relationship");
        for a in node.attributes() {
            out.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
        }
        out.push_str("/>");
    }
    out.push_str("</Relationships>");
    // 写盘前先严格解析一遍，任何不合法都拒绝写出
    roxmltree::Document::parse(&out).map_err(|e| anyhow::anyhow!("重建的关系 XML 非法：{e}"))?;
    crate::fixups::write_parts(path, &[(rels_part, out.into_bytes())])?;
    let _ = crate::fixups::prune_content_types(path);
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn attr(seg: &str, name: &str) -> Option<String> {
    let key = format!("{name}=");
    let i = seg.find(&key)? + key.len();
    let quote = seg.as_bytes().get(i).copied()? as char;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &seg[i + 1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

fn resolve(base: &str, target: &str) -> String {
    if let Some(t) = target.strip_prefix('/') {
        return t.to_string();
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

/// 保存前后保住"外链关系"的原文：某些引擎会把 Target="https://…" 当包内路径
/// 归一（变成 https:/…）并丢掉 TargetMode。这里按关系 Id 捕获原文，保存结束后
/// 原样恢复。
pub fn capture_external_rels(path: &std::path::Path) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for name in crate::fixups::entries_of(path) {
        if !name.ends_with(".rels") {
            continue;
        }
        let Some(raw) = crate::fixups::read_part_file(path, &name).and_then(|b| String::from_utf8(b).ok())
        else {
            continue;
        };
        let mut kept = Vec::new();
        let mut rest = raw.as_str();
        while let Some(i) = rest.find("<Relationship ") {
            let Some(gt) = rest[i..].find('>').map(|e| i + e + 1) else { break };
            let tag = &rest[i..gt];
            if tag.contains("TargetMode=\"External\"") || tag.contains("TargetMode='External'") {
                kept.push(tag.to_string());
            }
            rest = &rest[gt..];
        }
        if !kept.is_empty() {
            out.push((name, kept));
        }
    }
    out
}

/// 把捕获的外链关系按原文恢复——只恢复"被引擎改写过的"关系：
/// - 目标完全相同：只补回丢失的 TargetMode；
/// - 目标只是被当成包内路径归一（https:// → https:/）：恢复原文；
/// - 目标已经不同（用户在本应用里改了链接）：保留新值，只补 TargetMode。
pub fn restore_external_rels(path: &std::path::Path, captured: &[(String, Vec<String>)]) -> anyhow::Result<()> {
    if captured.is_empty() {
        return Ok(());
    }
    for (rels_part, tags) in captured {
        let Some(raw) = crate::fixups::read_part_file(path, rels_part).and_then(|b| String::from_utf8(b).ok())
        else {
            continue;
        };
        let doc = match roxmltree::Document::parse(&raw) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let root = doc.root_element();
        let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>");
        out.push_str("<Relationships");
        for ns in root.namespaces() {
            match ns.name() {
                Some(p) => out.push_str(&format!(" xmlns:{}=\"{}\"", p, xml_escape(ns.uri()))),
                None => out.push_str(&format!(" xmlns=\"{}\"", xml_escape(ns.uri()))),
            }
        }
        for a in root.attributes() {
            out.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
        }
        out.push('>');
        for node in doc.descendants().filter(|n| n.has_tag_name("Relationship")) {
            let id = node.attribute("Id").unwrap_or("");
            let cur_target = node.attribute("Target").unwrap_or("");
            let cur_external = node.attribute("TargetMode") == Some("External");
            let cap = tags.iter().find(|t| id_of(t) == id);
            let rebuilt = match cap {
                Some(tag) if !cur_external && cur_target.is_empty() => Some(tag.to_string()),
                Some(tag) => {
                    let cap_target = attr_of(tag, "Target").unwrap_or_default();
                    let cap_mode = attr_of(tag, "TargetMode");
                    if cur_target == cap_target {
                        // 只补 TargetMode
                        let mut s = String::from("<Relationship");
                        for a in node.attributes() {
                            s.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
                        }
                        if cap_mode.is_some() && !cur_external {
                            s.push_str(" TargetMode=\"External\"");
                        }
                        s.push_str("/>");
                        Some(s)
                    } else if engine_mangled(&cap_target).trim_end_matches('/')
                        == cur_target.trim_end_matches('/')
                    {
                        // 只是被当成包内路径改写了，恢复原文
                        Some(tag.clone())
                    } else {
                        // 用户改了目标：保留新 URL，TargetMode 按原关系补
                        let mut s = String::from("<Relationship");
                        for a in node.attributes() {
                            s.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
                        }
                        if cap_mode.is_some() && !cur_external {
                            s.push_str(" TargetMode=\"External\"");
                        }
                        s.push_str("/>");
                        Some(s)
                    }
                }
                None => None,
            };
            match rebuilt {
                Some(tag) => out.push_str(&tag),
                None => {
                    out.push_str("<Relationship");
                    for a in node.attributes() {
                        out.push_str(&format!(" {}=\"{}\"", a.name(), xml_escape(a.value())));
                    }
                    out.push_str("/>");
                }
            }
        }
        out.push_str("</Relationships>");
        if roxmltree::Document::parse(&out).is_ok() {
            crate::fixups::write_parts(path, &[(rels_part.clone(), out.into_bytes())])?;
        }
    }
    Ok(())
}

/// 引擎把 `https://host/` 当包内路径拼过后会变成 `https:/host/`。
fn engine_mangled(target: &str) -> String {
    let mut out = String::new();
    let mut prev_slash = false;
    for ch in target.chars() {
        if ch == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        out.push(ch);
    }
    out
}

fn attr_of(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=");
    let i = tag.find(&pat)?;
    let rest = &tag[i + pat.len()..];
    let quote = rest.chars().next()?;
    let body = rest.trim_start_matches(quote);
    Some(body.split(quote).next()?.to_string())
}

fn id_of(tag: &str) -> String {
    let Some(i) = tag.find("Id=") else { return String::new() };
    let rest = &tag[i + 3..];
    let quote = rest.chars().next().unwrap_or('"');
    let body = rest.trim_start_matches(quote);
    body.split(quote).next().unwrap_or("").to_string()
}
