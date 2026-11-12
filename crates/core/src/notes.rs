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
    let mut kept = String::with_capacity(rels.len());
    let mut removed: Vec<String> = Vec::new();
    let mut rest = rels.as_str();
    while let Some(i) = rest.find("<Relationship") {
        kept.push_str(&rest[..i]);
        let Some(end) = rest[i..].find("/>") else { break };
        let seg = &rest[i..i + end + 2];
        let ty = attr(seg, "Type").unwrap_or_default();
        if ty.contains("/comments") || ty.contains("/vmlDrawing") {
            if let Some(t) = attr(seg, "Target") {
                removed.push(resolve(dir, &t));
            }
        } else {
            kept.push_str(seg);
        }
        rest = &rest[i + end + 2..];
    }
    kept.push_str(rest);
    if removed.is_empty() {
        return Ok(());
    }
    for p in &removed {
        let _ = crate::patch::remove_entry(path, p);
    }
    crate::fixups::write_parts(path, &[(rels_part, kept.into_bytes())])?;
    // 清掉悬空的 Content_Types Override
    let _ = crate::fixups::prune_content_types(path);
    Ok(())
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
