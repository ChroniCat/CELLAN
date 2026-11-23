//! Append verified variations of an existing cell XF. Existing records and
//! extension markup stay byte-for-byte intact. Raw indices never move.
use crate::style::{CellStyle, StyleTable};
use roxmltree::{Document, Node};
use std::{collections::HashMap, ops::Range};

const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

struct Table {
    rows: Vec<Range<usize>>,
    count: Range<usize>,
    end: usize,
    close_empty: Option<String>,
    added: Vec<String>,
}

impl Table {
    fn read(root: Node<'_, '_>, xml: &str, name: &str, item: &str) -> Option<Self> {
        let mut nodes = root.children().filter(|n| n.has_tag_name((NS, name)));
        let table = nodes.next()?;
        if nodes.next().is_some() || table.default_namespace() != Some(NS) {
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
        let raw = &xml[table.range()];
        let (end, close_empty) = if raw.ends_with(&close) {
            (table.range().end - close.len(), None)
        } else if raw.ends_with("/>") && raw.starts_with(&format!("<{name}")) && rows.is_empty() {
            (table.range().end - 2, Some(close))
        } else {
            return None;
        };
        Some(Self {
            rows,
            count: count.range_value(),
            end,
            close_empty,
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
        let rows = self.added.concat();
        if let Some(close) = self.close_empty {
            edits.push((self.end..self.end + 2, format!(">{rows}{close}")));
        } else {
            edits.push((self.end..self.end, rows));
        }
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

fn argb([r, g, b]: [u8; 3]) -> String {
    format!("FF{r:02X}{g:02X}{b:02X}")
}

fn rgb(node: Node<'_, '_>) -> Option<[u8; 3]> {
    if node.children().any(|n| n.is_element()) || node.attributes().len() != 1 {
        return None;
    }
    let s = node.attribute("rgb")?;
    let s = if s.len() == 8 && s.get(..2)?.eq_ignore_ascii_case("ff") {
        &s[2..]
    } else {
        s
    };
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&s[..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..], 16).ok()?,
    ])
}

// A modeled RGB fill cannot describe theme colors, gradients, patterns or extensions.
// Only replace a plain foreground fill whose raw attributes agree with the model.
pub(super) fn plain_fill_pattern(styles: &StyleTable, id: u16) -> bool {
    styles.pattern_of(id).is_none_or(|p| {
        p.kind == "solid"
            && p.fg.is_some()
            && p.fg == styles.get(id).fill
            && p.fg_theme.is_none()
            && p.fg_indexed.is_none()
            && p.bg.is_none()
            && p.bg_theme.is_none()
            && matches!(p.bg_indexed, None | Some(64))
    })
}

fn solid_fill(fill: Node<'_, '_>, old: Option<[u8; 3]>, new: Option<[u8; 3]>) -> Option<String> {
    if fill.attributes().len() != 0 {
        return None;
    }
    let mut children = fill.children().filter(Node::is_element);
    let pattern = children.next()?;
    if children.next().is_some()
        || !pattern.has_tag_name((NS, "patternFill"))
        || pattern
            .attributes()
            .any(|a| a.namespace().is_some() || a.name() != "patternType")
    {
        return None;
    }
    let mut fg = None;
    let mut bg = false;
    for child in pattern.children().filter(Node::is_element) {
        if child.has_tag_name((NS, "fgColor")) && fg.is_none() {
            fg = Some(rgb(child)?);
        } else if child.has_tag_name((NS, "bgColor"))
            && !bg
            && child.attributes().len() == 1
            && child.attribute("indexed") == Some("64")
            && !child.children().any(|n| n.is_element())
        {
            bg = true;
        } else {
            return None;
        }
    }
    match pattern.attribute("patternType").unwrap_or("none") {
        "none" if fg.is_none() && old.is_none() => {}
        "solid" if fg.is_some() && fg == old => {}
        _ => return None,
    }
    Some(match new {
        None => "<fill><patternFill patternType=\"none\"/></fill>".into(),
        Some(color) => format!(
            "<fill><patternFill patternType=\"solid\"><fgColor rgb=\"{}\"/><bgColor indexed=\"64\"/></patternFill></fill>",
            argb(color)
        ),
    })
}

struct NumberFormats {
    table: Option<Table>,
    before_fonts: usize,
    codes: HashMap<u16, String>,
    next: Option<u16>,
    added: Vec<String>,
}

impl NumberFormats {
    fn read(root: Node<'_, '_>, xml: &str) -> Option<Self> {
        let fonts = root.children().find(|n| n.has_tag_name((NS, "fonts")))?;
        let mut codes = HashMap::new();
        let table = if let Some(records) = root.children().find(|n| n.has_tag_name((NS, "numFmts")))
        {
            let table = Table::read(root, xml, "numFmts", "numFmt")?;
            for row in records.children().filter(Node::is_element) {
                if row.children().any(|n| n.is_element())
                    || row.attributes().any(|a| {
                        a.namespace().is_some() || !matches!(a.name(), "numFmtId" | "formatCode")
                    })
                {
                    return None;
                }
                let id = row.attribute("numFmtId")?.parse().ok()?;
                if codes
                    .insert(id, row.attribute("formatCode")?.to_owned())
                    .is_some()
                {
                    return None;
                }
            }
            Some(table)
        } else {
            None
        };
        // Reserve even IDs referenced by other style families (e.g. differential
        // styles); do not give previously unrecognized references a new meaning.
        let mut highest = 163u16;
        for id in root.descendants().filter_map(|n| n.attribute("numFmtId")) {
            highest = highest.max(id.parse().ok()?);
        }
        Some(Self {
            table,
            before_fonts: fonts.range().start,
            codes,
            next: highest.checked_add(1),
            added: Vec::new(),
        })
    }

    fn resolve(&mut self, old_id: u16, old: &str, new: &str) -> Option<u16> {
        // Use only locale-independent built-ins. Other codes can be recorded
        // verbatim in a custom format, preserving the user's exact display code.
        const BUILTINS: &[u16] = &[0, 1, 2, 3, 4, 9, 10, 11, 12, 13, 49];
        let canonical_old = if old.eq_ignore_ascii_case("General") {
            ""
        } else {
            old
        };
        let code = self.codes.get(&old_id).map(String::as_str).or_else(|| {
            BUILTINS
                .contains(&old_id)
                .then(|| crate::xls_style::builtin_format(old_id))
                .flatten()
        })?;
        if code != canonical_old {
            return None;
        }
        let canonical = if new.eq_ignore_ascii_case("General") {
            ""
        } else {
            new
        };
        if let Some(&id) = BUILTINS.iter().find(|&&id| {
            !self.codes.contains_key(&id) && crate::xls_style::builtin_format(id) == Some(canonical)
        }) {
            return Some(id);
        }
        if let Some((&id, _)) = self
            .codes
            .iter()
            .filter(|(id, c)| **id >= 164 && c.as_str() == canonical)
            .min_by_key(|(id, _)| *id)
        {
            return Some(id);
        }
        if canonical.encode_utf16().count() > 255
            || canonical.chars().any(char::is_control)
            || !super::safe_xml_text(canonical)
        {
            return None;
        }
        let id = self.next?;
        self.next = id.checked_add(1);
        self.codes.insert(id, canonical.into());
        let escaped = quick_xml::escape::escape(canonical);
        self.added.push(format!(
            "<numFmt numFmtId=\"{id}\" formatCode=\"{escaped}\"/>"
        ));
        Some(id)
    }

    fn changes(self, edits: &mut Vec<(Range<usize>, String)>) {
        if self.added.is_empty() {
            return;
        }
        if let Some(mut table) = self.table {
            table.added = self.added;
            table.changes(edits);
        } else {
            edits.push((
                self.before_fonts..self.before_fonts,
                format!(
                    "<numFmts count=\"{}\">{}</numFmts>",
                    self.added.len(),
                    self.added.concat()
                ),
            ));
        }
    }
}

pub(super) struct Styles<'a> {
    xml: &'a str,
    doc: Document<'a>,
    fonts: Table,
    fills: Option<Table>,
    formats: Option<NumberFormats>,
    xfs: Table,
    resolved: HashMap<(u16, u16, u16), u16>,
}

impl<'a> Styles<'a> {
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
            fills: None,
            formats: None,
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
        expected.strikethrough = new.strikethrough;
        expected.color = new.color;
        expected.fill = new.fill;
        expected.format.clone_from(&new.format);
        if expected != *new || old == new {
            return None;
        }
        let mut fields = Vec::new();
        if old.bold != new.bold
            || old.italic != new.italic
            || old.size != new.size
            || old.color != new.color
            || old.strikethrough != new.strikethrough
        {
            if old.color != new.color && styles.theme_of(old_id).font.is_some() {
                return None;
            }
            let original = self.component(base, "fontId", "applyFont")?;
            fields.push(("fontId", self.font(original, old, new)?));
            fields.push(("applyFont", 1));
        }
        if old.fill != new.fill {
            if styles.theme_of(old_id).fill.is_some()
                || !plain_fill_pattern(styles, old_id)
                || !plain_fill_pattern(styles, id)
            {
                return None;
            }
            let original = self.component(base, "fillId", "applyFill")?;
            let record = self.node("fills", original)?;
            let fill = solid_fill(record, old.fill, new.fill)?;
            if self.fills.is_none() {
                self.fills = Some(Table::read(
                    self.doc.root_element(),
                    self.xml,
                    "fills",
                    "fill",
                )?);
            }
            fields.push(("fillId", self.fills.as_mut()?.intern(self.xml, fill)?));
            fields.push(("applyFill", 1));
        }
        if old.format != new.format {
            let original = self.component(base, "numFmtId", "applyNumberFormat")?;
            if self.formats.is_none() {
                self.formats = Some(NumberFormats::read(self.doc.root_element(), self.xml)?);
            }
            fields.push((
                "numFmtId",
                self.formats
                    .as_mut()?
                    .resolve(original, &old.format, &new.format)?,
            ));
            fields.push(("applyNumberFormat", 1));
        }
        let new_xf = self.with_fields(base, fields)?;
        self.resolved.insert((base, old_id, id), new_xf);
        Some(new_xf)
    }

    fn component(&self, base: u16, id: &str, apply: &str) -> Option<u16> {
        let xf = self.node("cellXfs", base)?;
        let local = xf.attribute(id).unwrap_or("0").parse().ok()?;
        if !matches!(xf.attribute(apply), Some("1" | "true")) {
            if !matches!(xf.attribute(apply), None | Some("0" | "false")) {
                return None;
            }
            let parent = xf.attribute("xfId").unwrap_or("0").parse().ok()?;
            // Fresh CELLAN files may omit the entire optional cellStyleXfs table.
            // Only the default component of the implicit Normal style is unambiguous.
            let inherited = match self.node("cellStyleXfs", parent) {
                Some(row) => row.attribute(id).unwrap_or("0").parse::<u16>().ok()?,
                None if parent == 0
                    && !self
                        .doc
                        .root_element()
                        .children()
                        .any(|n| n.has_tag_name((NS, "cellStyleXfs"))) =>
                {
                    0
                }
                None => return None,
            };
            if inherited != local {
                return None;
            }
        }
        Some(local)
    }

    fn font(&mut self, font_id: u16, old: &CellStyle, new: &CellStyle) -> Option<u16> {
        let font = self.node("fonts", font_id)?;
        if font.default_namespace() != Some(NS) {
            return None;
        }
        let font_range = font.range();
        let mut edits = Vec::new();
        let mut prefix = String::new();
        for (name, was, want) in [
            ("b", old.bold, new.bold),
            ("i", old.italic, new.italic),
            ("strike", old.strikethrough, new.strikethrough),
        ] {
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
        if old.color != new.color {
            let mut colors = font.children().filter(|n| n.has_tag_name((NS, "color")));
            if let Some(color) = colors.next() {
                if colors.next().is_some() || rgb(color)? != old.color {
                    return None;
                }
                edits.push((color.attribute_node("rgb")?.range_value(), argb(new.color)));
            } else if old.color == [0; 3] {
                prefix.push_str(&format!("<color rgb=\"{}\"/>", argb(new.color)));
            } else {
                return None;
            }
        }
        if !prefix.is_empty() {
            // Insert after the start tag using the XML parser's first child offset.
            // Reject a self-closing/empty font where that position is ambiguous.
            let pos = font.first_child()?.range().start;
            edits.insert(0, (pos..pos, prefix));
        }
        let raw_font = apply_edits(&self.xml[font_range.clone()], font_range.start, edits);
        self.fonts.intern(self.xml, raw_font)
    }

    fn with_fields(&mut self, base: u16, fields: Vec<(&str, u16)>) -> Option<u16> {
        let xf = self.node("cellXfs", base)?;
        let range = xf.range();
        let mut edits = Vec::new();
        let mut attrs = String::new();
        for (name, value) in fields {
            if let Some(attr) = xf.attribute_node(name) {
                edits.push((attr.range_value(), value.to_string()));
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
        self.xfs.intern(self.xml, raw_xf)
    }

    pub(super) fn finish(self) -> Option<(String, Vec<(u16, u16, u16)>)> {
        if self.resolved.is_empty() {
            return None;
        }
        let mut edits = Vec::new();
        self.fonts.changes(&mut edits);
        if let Some(fills) = self.fills {
            fills.changes(&mut edits);
        }
        if let Some(formats) = self.formats {
            formats.changes(&mut edits);
        }
        self.xfs.changes(&mut edits);
        let ids = self
            .resolved
            .into_iter()
            .map(|((base, _, id), xf)| (base, id, xf))
            .collect();
        Some((apply_edits(self.xml, 0, edits), ids))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_colors_and_unsupported_fill_or_format_components_fall_back() {
        let base = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:e="urn:vendor"><fonts count="1"><font><sz val="11"/><color theme="1"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><cellStyleXfs count="1"><xf fontId="0" fillId="0" numFmtId="0"/></cellStyleXfs><cellXfs count="1"><xf fontId="0" fillId="0" numFmtId="0" xfId="0"/></cellXfs></styleSheet>"#;
        let mut styles = StyleTable::new();
        let fill = styles.intern(CellStyle {
            fill: Some([1, 2, 3]),
            ..CellStyle::default_dark()
        });
        let font = styles.intern(CellStyle {
            color: [1, 2, 3],
            ..CellStyle::default_dark()
        });
        let number = styles.intern(CellStyle {
            format: "0.00%".into(),
            ..CellStyle::default_dark()
        });
        for (raw, target) in [
            (base.to_owned(), font),
            (base.replace("<patternFill patternType=\"none\"/>", "<gradientFill/>"), fill),
            (base.replace("<patternFill patternType=\"none\"/>", "<patternFill patternType=\"solid\"><fgColor theme=\"4\"/></patternFill>"), fill),
            (base.replace("<patternFill patternType=\"none\"/>", "<patternFill patternType=\"none\" e:custom=\"1\"/>"), fill),
            (base.replace("<xf fontId=\"0\" fillId=\"0\" numFmtId=\"0\"/>", "<xf fontId=\"0\" fillId=\"5\" numFmtId=\"0\"/>"), fill),
            (base.replace("<xf fontId=\"0\" fillId=\"0\" numFmtId=\"0\"/>", "<xf fontId=\"0\" fillId=\"0\" numFmtId=\"9\"/>"), number),
            (base.replace("<fonts", "<numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\"0_x0022_\" e:custom=\"1\"/></numFmts><fonts"), number),
        ] {
            assert!(Styles::new(&raw).unwrap().resolve(0,0,target,&styles).is_none(), "{raw}");
        }
    }
}
