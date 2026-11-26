//! Update the contents of existing legacy notes without touching their VML shapes.
use super::{Note, find_comment_targets, parse_a1, parse_comments, resolve_comment_part};
use crate::{CellRef, Sheet};
use anyhow::Result;
use roxmltree::{Document, Node};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use zip::ZipArchive;

fn child<'a, 'input>(node: Node<'a, 'input>, local: &str) -> Option<Node<'a, 'input>> {
    let ns = node.tag_name().namespace()?;
    let mut matches = node.children().filter(|n| n.has_tag_name((ns, local)));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn plain(text: &str) -> Option<std::borrow::Cow<'_, str>> {
    if text
        .as_bytes()
        .windows(7)
        .any(|w| w.starts_with(b"_x") && w[6] == b'_' && w[2..6].iter().all(u8::is_ascii_hexdigit))
        || !text.chars().all(|c| {
            matches!(c, '\t' | '\n' | '\r') || (c >= ' ' && c != '\u{fffe}' && c != '\u{ffff}')
        })
    {
        return None;
    }
    let escaped = quick_xml::escape::escape(text);
    Some(if escaped.contains('\r') {
        std::borrow::Cow::Owned(escaped.replace('\r', "&#13;"))
    } else {
        escaped
    })
}

/// Keep every run's formatting. Replace the changed substring in the first
/// affected run, leaving the unchanged prefix and suffix in their original runs.
fn edit_text(
    text: Node<'_, '_>,
    before: &str,
    after: &str,
    changes: &mut Vec<(Range<usize>, String)>,
) -> Option<()> {
    let ns = text.tag_name().namespace()?;
    let mut runs = Vec::new();
    for node in text.children().filter(Node::is_element) {
        let t = if node.has_tag_name((ns, "r")) {
            child(node, "t")?
        } else if node.has_tag_name((ns, "t")) {
            node
        } else {
            return None;
        };
        if t.attributes().any(|a| {
            a.namespace() != Some("http://www.w3.org/XML/1998/namespace") || a.name() != "space"
        }) {
            return None;
        }
        runs.push((t, t.text().unwrap_or("")));
    }
    if runs.is_empty() || !runs.iter().map(|(_, s)| *s).collect::<String>().eq(before) {
        return None;
    }
    let prefix: usize = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum();
    let suffix: usize = before[prefix..]
        .chars()
        .rev()
        .zip(after[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum();
    let mut insert = Some(&after[prefix..after.len() - suffix]);
    let mut start = 0;
    for (node, original) in runs {
        let end = start + original.len();
        let mut value = original[..prefix.saturating_sub(start).min(original.len())].to_owned();
        if end >= prefix {
            if let Some(middle) = insert.take() {
                value.push_str(middle);
            }
        }
        value.push_str(
            &original[(before.len() - suffix)
                .saturating_sub(start)
                .min(original.len())..],
        );
        if value != original {
            changes.push((
                node.range(),
                format!(
                    "<t xmlns=\"{ns}\" xml:space=\"preserve\">{}</t>",
                    plain(&value)?
                ),
            ));
        }
        start = end;
    }
    Some(())
}

fn edited_comments(xml: &str, live: &HashMap<CellRef, Note>) -> Option<String> {
    let doc = Document::parse(xml).ok()?;
    let root = doc.root_element();
    let ns = root.tag_name().namespace()?;
    if root.tag_name().name() != "comments"
        || !matches!(
            ns,
            "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
                | "http://purl.oclc.org/ooxml/spreadsheetml/main"
        )
    {
        return None;
    }
    let authors = child(root, "authors")?;
    let comments = child(root, "commentList")?;
    let mut author_ids = HashMap::new();
    for (index, n) in authors.children().filter(Node::is_element).enumerate() {
        if !n.has_tag_name((ns, "author")) {
            return None;
        }
        author_ids
            .entry(n.text().unwrap_or("").to_owned())
            .or_insert(index);
    }
    let old_notes: HashMap<_, _> = parse_comments(xml)
        .into_iter()
        .map(|n| (CellRef::new(n.r, n.c), n))
        .collect();
    if old_notes.len() != live.len() {
        return None;
    }
    let mut seen = HashSet::new();
    let mut new_authors = String::new();
    let mut author_count = authors.children().filter(Node::is_element).count();
    let mut changes: Vec<(Range<usize>, String)> = Vec::new();
    for comment in comments.children().filter(Node::is_element) {
        if !comment.has_tag_name((ns, "comment")) {
            return None;
        }
        let (r, c) = parse_a1(comment.attribute("ref")?)?;
        let key = CellRef::new(r, c);
        if !seen.insert(key) {
            return None;
        }
        let before = old_notes.get(&key)?;
        let after = live.get(&key)?;
        if before.author != after.author {
            let author = if after.author.is_empty() {
                "CELLAN"
            } else {
                &after.author
            };
            let id = if let Some(id) = author_ids.get(author) {
                *id
            } else {
                new_authors.push_str(&format!(
                    "<author xmlns=\"{ns}\">{}</author>",
                    plain(author)?
                ));
                let id = author_count;
                author_count += 1;
                author_ids.insert(author.to_owned(), id);
                id
            };
            changes.push((
                comment.attribute_node("authorId")?.range_value(),
                id.to_string(),
            ));
        }
        if before.text != after.text {
            edit_text(
                child(comment, "text")?,
                &before.text,
                &after.text,
                &mut changes,
            )?;
        }
    }
    if seen.len() != live.len() {
        return None;
    }
    if !new_authors.is_empty() {
        let at = authors
            .children()
            .filter(Node::is_element)
            .last()?
            .range()
            .end;
        changes.push((at..at, new_authors));
    }
    changes.sort_unstable_by_key(|(r, _)| r.start);
    let mut out = String::with_capacity(xml.len());
    let mut pos = 0;
    for (range, next) in changes {
        out.push_str(xml.get(pos..range.start)?);
        out.push_str(&next);
        pos = range.end;
    }
    out.push_str(&xml[pos..]);
    Some(out)
}

pub(crate) fn edited_parts(
    zip: &mut ZipArchive<File>,
    part: &str,
    sheet: &Sheet,
) -> Result<Option<HashMap<String, Vec<u8>>>> {
    let Some((dir, file)) = part.rsplit_once('/') else {
        return Ok(None);
    };
    let rels = format!("{dir}/_rels/{file}.rels");
    if zip.index_for_name(&rels).is_none() {
        return Ok(None);
    }
    let mut rel_xml = String::new();
    zip.by_name(&rels)?.read_to_string(&mut rel_xml)?;
    let (legacy, threaded) = find_comment_targets(&rel_xml);
    let [target] = &legacy[..] else {
        return Ok(None);
    };
    if !threaded.is_empty() {
        return Ok(None);
    }
    let path = resolve_comment_part(dir, target);
    if zip.index_for_name(&path).is_none() {
        return Ok(None);
    }
    let mut original = String::new();
    zip.by_name(&path)?.read_to_string(&mut original)?;
    let Some(next) = edited_comments(&original, &sheet.notes) else {
        return Ok(None);
    };
    let mut parts = HashMap::new();
    if next != original {
        parts.insert(path, next.into_bytes());
    }
    Ok(Some(parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_across_rich_text_runs_preserves_the_format_of_the_suffix() {
        let xml = r#"<text xmlns="urn:sheet"><r><rPr><i/></rPr><t>甲ABC</t></r><r><rPr><b/></rPr><t>DEF乙</t></r></text>"#;
        let doc = Document::parse(xml).unwrap();
        let mut edits = Vec::new();
        edit_text(doc.root_element(), "甲ABCDEF乙", "甲A新值F乙", &mut edits).unwrap();
        let mut next = xml.to_owned();
        for (range, value) in edits.into_iter().rev() {
            next.replace_range(range, &value);
        }
        let result = Document::parse(&next).unwrap();
        let runs: Vec<_> = result
            .root_element()
            .children()
            .filter(Node::is_element)
            .collect();
        assert_eq!(child(runs[0], "t").unwrap().text(), Some("甲A新值"));
        assert!(child(child(runs[0], "rPr").unwrap(), "i").is_some());
        assert_eq!(child(runs[1], "t").unwrap().text(), Some("F乙"));
        assert!(child(child(runs[1], "rPr").unwrap(), "b").is_some());
    }

    #[test]
    fn changing_one_note_preserves_other_runs_and_drawing_attributes() {
        let xml = r#"<m:comments xmlns:m="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><m:authors><m:author>A</m:author></m:authors><m:commentList><m:comment ref="A1" authorId="0" shapeId="9"><m:text><m:r><m:rPr><m:i/></m:rPr><m:t>old</m:t></m:r></m:text></m:comment><m:comment ref="C2" authorId="0"><m:text><m:r><m:rPr><m:b/></m:rPr><m:t>keep</m:t></m:r></m:text></m:comment></m:commentList></m:comments>"#;
        let mut notes: HashMap<_, _> = parse_comments(xml)
            .into_iter()
            .map(|n| (CellRef::new(n.r, n.c), n))
            .collect();
        assert_eq!(edited_comments(xml, &notes).as_deref(), Some(xml));
        let note = notes.get_mut(&CellRef::new(0, 0)).unwrap();
        note.author = "陈&李".into();
        note.text = " 新值 <1>\n".into();
        let updated = edited_comments(xml, &notes).unwrap();
        assert!(updated.contains("<m:rPr><m:i/></m:rPr>"));
        assert!(updated.contains("shapeId=\"9\""));
        assert!(updated.contains("<m:r><m:rPr><m:b/></m:rPr><m:t>keep</m:t></m:r>"));
        let read = parse_comments(&updated);
        assert_eq!(read[0], notes[&CellRef::new(0, 0)]);
        notes.remove(&CellRef::new(1, 2));
        assert!(edited_comments(xml, &notes).is_none());
    }
}
