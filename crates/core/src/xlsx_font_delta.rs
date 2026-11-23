//! Add a font-only variation of an existing cell XF. Existing records and
//! extension markup stay byte-for-byte intact. Raw indices never move.
use crate::style::StyleTable;
use roxmltree::{Document, Node};
use std::{collections::HashMap, ops::Range};

const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

struct Table {
    rows: Vec<Range<usize>>,
    count: Range<usize>,
    end: usize,
    added: Vec<String>,
}

impl Table {
    fn read(root: Node<'_, '_>, xml: &str, name: &str, item: &str) -> Option<Self> {
        let mut nodes = root.children().filter(|n| n.has_tag_name((NS, name)));
        let table = nodes.next()?;
        if nodes.next().is_some() {
            return None;
        }
        let count = table.attribute_node("count")?;
        let mut rows = Vec::new();
        for child in table.children().filter(|n| n.is_element()) {
            if !child.has_tag_name((NS, item)) {
                return None;
            }
            rows.push(child.range());
        }
        if rows.len() != count.value().parse::<usize>().ok()? {
            return None;
        }
        let close = format!("</{name}>");
        let end = table.range().end.checked_sub(close.len())?;
        if !xml.get(end..)?.starts_with(&close) {
            return None;
        }
        Some(Self {
            rows,
            count: count.range_value(),
            end,
            added: Vec::new(),
        })
    }

    fn intern(&mut self, xml: &str, value: String) -> Option<u16> {
        if let Some(i) = self.rows.iter().position(|r| xml[r.clone()] == value) {
            return u16::try_from(i).ok();
        }
        let len = self.rows.len();
        if let Some(i) = self.added.iter().position(|s| s == &value) {
            return u16::try_from(len + i).ok();
        }
        let idx = u16::try_from(len + self.added.len()).ok()?;
        // Leave room for the collection count and avoid Excel's style limit.
        if idx >= 65_490 {
            return None;
        }
        self.added.push(value);
        Some(idx)
    }

    fn changes(self, edits: &mut Vec<(Range<usize>, String)>) {
        if self.added.is_empty() {
            return;
        }
        edits.push((self.count, (self.rows.len() + self.added.len()).to_string()));
        edits.push((self.end..self.end, self.added.concat()));
    }
}

fn apply_edits(original: &str, start: usize, mut edits: Vec<(Range<usize>, String)>) -> String {
    edits.sort_by_key(|(r, _)| r.start);
    let mut result = String::new();
    let mut cursor = 0;
    for (range, text) in edits {
        result.push_str(&original[cursor..range.start - start]);
        result.push_str(&text);
        cursor = range.end - start;
    }
    result.push_str(&original[cursor..]);
    result
}

pub(super) struct FontEdits<'a> {
    xml: &'a str,
    doc: Document<'a>,
    fonts: Table,
    xfs: Table,
    resolved: HashMap<(u16, u16, u16), u16>,
}

impl<'a> FontEdits<'a> {
    pub(super) fn new(xml: &'a str) -> Option<Self> {
        let doc = Document::parse(xml).ok()?;
        let root = doc.root_element();
        // Inserted font property names inherit the default SpreadsheetML namespace.
        if !root.has_tag_name((NS, "styleSheet")) || root.default_namespace() != Some(NS) {
            return None;
        }
        let fonts = Table::read(root, xml, "fonts", "font")?;
        let xfs = Table::read(root, xml, "cellXfs", "xf")?;
        Some(Self {
            xml,
            doc,
            fonts,
            xfs,
            resolved: HashMap::new(),
        })
    }

    fn node(&self, table: &str, index: u16) -> Option<Node<'_, 'a>> {
        self.doc
            .root_element()
            .children()
            .find(|n| n.has_tag_name((NS, table)))?
            .children()
            .filter(|n| n.is_element())
            .nth(usize::from(index))
    }

    pub(super) fn resolve(
        &mut self,
        base: u16,
        old_id: u16,
        id: u16,
        styles: &StyleTable,
    ) -> Option<u16> {
        if let Some(xf) = self.resolved.get(&(base, old_id, id)) {
            return Some(*xf);
        }
        let old = styles.get(old_id);
        let new = styles.get(id);
        let mut expected = old.clone();
        expected.bold = new.bold;
        expected.italic = new.italic;
        expected.size = new.size;
        if expected != *new || old == new {
            return None;
        }
        let xf = self.node("cellXfs", base)?;
        let font_id = xf.attribute("fontId").unwrap_or("0").parse::<u16>().ok()?;
        if !matches!(xf.attribute("applyFont"), Some("1" | "true")) {
            let parent = xf.attribute("xfId").unwrap_or("0").parse().ok()?;
            let inherited = self.node("cellStyleXfs", parent)?;
            if inherited
                .attribute("fontId")
                .unwrap_or("0")
                .parse::<u16>()
                .ok()?
                != font_id
            {
                return None;
            }
        }
        let font = self.node("fonts", font_id)?;
        if font.default_namespace() != Some(NS) {
            return None;
        }
        let font_range = font.range();
        let mut edits = Vec::new();
        let mut prefix = String::new();
        for (name, was, want) in [("b", old.bold, new.bold), ("i", old.italic, new.italic)] {
            let mut props = font.children().filter(|n| n.has_tag_name((NS, name)));
            let prop = props.next();
            let actual = match prop.map(|p| p.attribute("val").unwrap_or("1")) {
                Some("1" | "true") => true,
                None | Some("0" | "false") => false,
                _ => return None,
            };
            if props.next().is_some() || actual != was {
                return None;
            }
            if was == want {
                continue;
            }
            if let Some(prop) = prop {
                if prop.children().any(|n| n.is_element())
                    || prop
                        .attributes()
                        .any(|a| a.namespace().is_some() || a.name() != "val")
                {
                    return None;
                }
                edits.push((
                    prop.range(),
                    if want {
                        format!("<{name}/>")
                    } else {
                        String::new()
                    },
                ));
            } else if want {
                prefix.push_str(&format!("<{name}/>"));
            }
        }
        let mut sizes = font.children().filter(|n| n.has_tag_name((NS, "sz")));
        let size = sizes.next()?.attribute_node("val")?;
        if sizes.next().is_some() || size.value().parse::<f32>().ok()? != old.size {
            return None;
        }
        if old.size != new.size {
            if !new.size.is_finite() || !(1.0..=409.0).contains(&new.size) {
                return None;
            }
            edits.push((size.range_value(), new.size.to_string()));
        }
        if !prefix.is_empty() {
            // Insert after the start tag using the XML parser's first child offset.
            // Reject a self-closing/empty font where that position is ambiguous.
            let pos = font.first_child()?.range().start;
            edits.insert(0, (pos..pos, prefix));
        }
        let raw_font = apply_edits(&self.xml[font_range.clone()], font_range.start, edits);
        let new_font = self.fonts.intern(self.xml, raw_font)?;
        let xf = self.node("cellXfs", base)?;
        let range = xf.range();
        let mut edits = Vec::new();
        let mut attrs = String::new();
        for (name, value) in [
            ("fontId", new_font.to_string()),
            ("applyFont", "1".to_owned()),
        ] {
            if let Some(attr) = xf.attribute_node(name) {
                edits.push((attr.range_value(), value));
            } else {
                attrs.push_str(&format!(" {name}=\"{value}\""));
            }
        }
        if !attrs.is_empty() {
            let name_end = range.start + "<xf".len();
            if !self.xml[range.clone()].starts_with("<xf") {
                return None;
            }
            edits.push((name_end..name_end, attrs));
        }
        let raw_xf = apply_edits(&self.xml[range.clone()], range.start, edits);
        let new_xf = self.xfs.intern(self.xml, raw_xf)?;
        self.resolved.insert((base, old_id, id), new_xf);
        Some(new_xf)
    }

    pub(super) fn finish(self) -> Option<(String, Vec<(u16, u16, u16)>)> {
        if self.resolved.is_empty() {
            return None;
        }
        let mut edits = Vec::new();
        self.fonts.changes(&mut edits);
        self.xfs.changes(&mut edits);
        let ids = self
            .resolved
            .into_iter()
            .map(|((base, _, id), xf)| (base, id, xf))
            .collect();
        Some((apply_edits(self.xml, 0, edits), ids))
    }
}
