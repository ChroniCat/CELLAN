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

#[derive(Clone, Copy, PartialEq)]
enum Paint {
    Rgb([u8; 3]),
    Theme(u8, f32),
    Indexed(u8),
}

impl Paint {
    fn read(n: Node<'_, '_>) -> Option<Self> {
        if n.children().any(|n| n.is_element()) {
            return None;
        }
        if let Some(c) = rgb(n) {
            return Some(Self::Rgb(c));
        }
        if let Some(index) = n.attribute("theme") {
            if n.attributes()
                .any(|a| a.namespace().is_some() || !matches!(a.name(), "theme" | "tint"))
            {
                return None;
            }
            let index = index.parse().ok()?;
            let tint: f32 = n.attribute("tint").unwrap_or("0").parse().ok()?;
            return (index < 12 && (-1.0..=1.0).contains(&tint))
                .then_some(Self::Theme(index, tint));
        }
        if n.attributes().len() != 1 {
            return None;
        }
        Some(Self::Indexed(n.attribute("indexed")?.parse().ok()?))
    }
    fn xml(self, tag: &str) -> Option<String> {
        let attrs = match self {
            Self::Rgb(color) => format!("rgb=\"{}\"", argb(color)),
            Self::Indexed(i) if i <= 65 => format!("indexed=\"{i}\""),
            Self::Theme(i, t) if i < 12 && (-1.0..=1.0).contains(&t) => {
                format!("theme=\"{i}\" tint=\"{t}\"")
            }
            _ => return None,
        };
        Some(format!("<{tag} {attrs}/>"))
    }
    fn model(
        color: Option<[u8; 3]>,
        theme: Option<(u8, f32)>,
        indexed: Option<u8>,
    ) -> Option<Option<Self>> {
        match (color, theme, indexed) {
            (None, Some((i, t)), None) => Some(Some(Self::Theme(i, t))),
            (None, None, Some(i)) => Some(Some(Self::Indexed(i))),
            (c, None, None) => Some(c.map(Self::Rgb)),
            _ => None,
        }
    }
}
#[derive(PartialEq)]
struct Pattern<'a> {
    kind: &'a str,
    fg: Option<Paint>,
    bg: Option<Paint>,
}

impl<'a> Pattern<'a> {
    fn model(styles: &'a StyleTable, id: u16) -> Option<Self> {
        let theme = styles.theme_of(id).fill.map(|t| (t.idx, t.tint));
        if let Some(p) = styles.pattern_of(id) {
            if theme.is_some() && p.fg_theme != theme {
                return None;
            }
            if p.kind == "solid" && p.fg.is_some() && p.fg != styles.get(id).fill {
                return None;
            }
            return Some(Self {
                kind: &p.kind,
                fg: Paint::model(p.fg, p.fg_theme, p.fg_indexed)?,
                bg: Paint::model(p.bg, p.bg_theme, p.bg_indexed)?,
            });
        }
        let fg = theme
            .map(|(i, t)| Paint::Theme(i, t))
            .or_else(|| styles.get(id).fill.map(Paint::Rgb));
        Some(Self {
            kind: if fg.is_some() { "solid" } else { "none" },
            fg,
            bg: None,
        })
    }
    fn raw(fill: Node<'a, '_>) -> Option<Self> {
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
        let mut out = Self {
            kind: pattern.attribute("patternType").unwrap_or("none"),
            fg: None,
            bg: None,
        };
        for child in pattern.children().filter(Node::is_element) {
            if child.has_tag_name((NS, "fgColor")) && out.fg.is_none() {
                out.fg = Some(Paint::read(child)?);
            } else if child.has_tag_name((NS, "bgColor")) && out.bg.is_none() {
                out.bg = Some(Paint::read(child)?);
            } else {
                return None;
            }
        }
        Some(out)
    }
    fn xml(&self) -> Option<String> {
        if !matches!(
            self.kind,
            "none"
                | "solid"
                | "mediumGray"
                | "darkGray"
                | "lightGray"
                | "darkHorizontal"
                | "darkVertical"
                | "darkDown"
                | "darkUp"
                | "darkGrid"
                | "darkTrellis"
                | "lightHorizontal"
                | "lightVertical"
                | "lightDown"
                | "lightUp"
                | "lightGrid"
                | "lightTrellis"
                | "gray125"
                | "gray0625"
        ) {
            return None;
        }
        let mut out = format!("<fill><patternFill patternType=\"{}\">", self.kind);
        if let Some(c) = self.fg {
            out.push_str(&c.xml("fgColor")?);
        }
        if let Some(c) = self
            .bg
            .or_else(|| (self.kind == "solid").then_some(Paint::Indexed(64)))
        {
            out.push_str(&c.xml("bgColor")?);
        }
        out.push_str("</patternFill></fill>");
        Some(out)
    }
}

/// Preserve diagonal, interior lines, border flags and extensions on the original
/// record. Only replace a requested side whose old representation is understood.
fn border_variant(
    xml: &str,
    border: Node<'_, '_>,
    old: &CellStyle,
    new: &CellStyle,
) -> Option<String> {
    const NAMES: [&str; 7] = [
        "left",
        "right",
        "top",
        "bottom",
        "diagonal",
        "vertical",
        "horizontal",
    ];
    const STYLES: [&str; 14] = [
        "none", "thin", "medium", "thick", "double", "dashed", "dotted", "hair",
        "dashDot", "dashDotDot", "mediumDashed", "mediumDashDot", "mediumDashDotDot", "slantDashDot",
    ];
    if border.default_namespace() != Some(NS) {
        return None;
    }
    let range = border.range();
    let raw = &xml[range.clone()];
    if !raw.starts_with("<border") {
        return None;
    }
    let mut edits = Vec::new();
    let mut end_insert = String::new();
    for (index, name) in NAMES[..4].iter().enumerate() {
        let was = old.border[index];
        let want = new.border[index];
        if was == want {
            continue;
        }
        let mut sides = border.children().filter(|n| n.has_tag_name((NS, *name)));
        let side = sides.next();
        if sides.next().is_some() {
            return None;
        }
        let raw_style = side.and_then(|n| n.attribute("style")).unwrap_or("none");
        if raw_style != *STYLES.get(usize::from(was.style))? {
            return None;
        }
        let style = STYLES.get(usize::from(want.style))?;
        let mut color = None;
        if let Some(side) = side {
            if side
                .attributes()
                .any(|a| a.namespace().is_some() || a.name() != "style")
                || side
                    .namespaces()
                    .any(|ns| border.lookup_namespace_uri(ns.name()) != Some(ns.uri()))
            {
                return None;
            }
            for c in side.children().filter(Node::is_element) {
                if !c.has_tag_name((NS, "color")) || color.replace(c).is_some() {
                    return None;
                }
            }
        }
        let replacement = if want.style == 0 {
            if let Some(c) = color {
                if !ordinary_border_color(c, was.color) {
                    return None;
                }
            }
            format!("<{name}/>")
        } else {
            let color = if was.color == want.color && color.is_some() {
                xml[color?.range()].to_owned()
            } else {
                if color.is_some_and(|c| !ordinary_border_color(c, was.color)) {
                    return None;
                }
                format!("<color rgb=\"{}\"/>", argb(want.color))
            };
            format!("<{name} style=\"{style}\">{color}</{name}>")
        };
        if let Some(side) = side {
            edits.push((side.range(), replacement));
        } else if let Some(next) = border.children().find(|n| {
            n.is_element()
                && (NAMES[index + 1..]
                    .iter()
                    .any(|name| n.has_tag_name((NS, *name)))
                    || n.tag_name().namespace() != Some(NS))
        }) {
            let pos = next.range().start;
            edits.push((pos..pos, replacement));
        } else {
            end_insert.push_str(&replacement);
        }
    }
    if !end_insert.is_empty() {
        if raw.ends_with("/>") {
            edits.push((range.end - 2..range.end, format!(">{end_insert}</border>")));
        } else {
            let pos = range.start + raw.rfind("</")?;
            edits.push((pos..pos, end_insert));
        }
    }
    Some(apply_edits(raw, range.start, edits))
}

fn ordinary_border_color(c: Node<'_, '_>, expected: [u8; 3]) -> bool {
    rgb(c) == Some(expected)
        || expected == [0; 3]
            && c.attributes().len() == 1
            && matches!(c.attribute("auto"), Some("1" | "true"))
            && !c.children().any(|n| n.is_element())
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
    borders: Option<Table>,
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
            borders: None,
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
        expected.underline = new.underline;
        expected.double_underline = new.double_underline;
        expected.size = new.size;
        expected.strikethrough = new.strikethrough;
        expected.color = new.color;
        expected.fill = new.fill;
        expected.format.clone_from(&new.format);
        expected.wrap = new.wrap;
        expected.shrink_to_fit = new.shrink_to_fit;
        expected.indent = new.indent;
        expected.halign = new.halign;
        expected.valign = new.valign;
        expected.rotation = new.rotation;
        expected.vertical_text = new.vertical_text;
        expected.font.clone_from(&new.font);
        expected.border = new.border;
        expected.locked = new.locked;
        expected.hidden = new.hidden;
        let was_theme = styles.theme_of(old_id);
        let want_theme = styles.theme_of(id);
        let fill_changed = old.fill != new.fill
            || was_theme.fill != want_theme.fill
            || styles.pattern_of(old_id) != styles.pattern_of(id);
        if expected != *new || (old == new && !fill_changed && was_theme == want_theme) {
            return None;
        }
        let mut fields = Vec::new();
        if old.font != new.font
            || old.bold != new.bold
            || was_theme.font != want_theme.font
            || old.italic != new.italic
            || (old.underline, old.double_underline) != (new.underline, new.double_underline)
            || old.size != new.size
            || old.color != new.color
            || old.strikethrough != new.strikethrough
        {
            let original = self.component(base, "fontId", "applyFont")?;
            fields.push((
                "fontId",
                self.font(original, old, new, was_theme.font, want_theme.font)?,
            ));
            fields.push(("applyFont", 1));
        }
        if fill_changed {
            let original = self.component(base, "fillId", "applyFill")?;
            let mut actual = Pattern::raw(self.node("fills", original)?)?;
            let expected = Pattern::model(styles, old_id)?;
            if expected.bg.is_none()
                && actual.bg == Some(Paint::Indexed(64))
                && expected.kind == "solid"
            {
                actual.bg = None;
            }
            if actual != expected {
                return None;
            }
            let fill = Pattern::model(styles, id)?.xml()?;
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
        if old.border != new.border {
            let original = self.component(base, "borderId", "applyBorder")?;
            let value = border_variant(self.xml, self.node("borders", original)?, old, new)?;
            if self.borders.is_none() {
                self.borders = Some(Table::read(
                    self.doc.root_element(),
                    self.xml,
                    "borders",
                    "border",
                )?);
            }
            fields.push(("borderId", self.borders.as_mut()?.intern(self.xml, value)?));
            fields.push(("applyBorder", 1));
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
        let new_xf = self.with_fields(base, fields, old, new)?;
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

    fn font(
        &mut self,
        font_id: u16,
        old: &CellStyle,
        new: &CellStyle,
        was_theme: Option<crate::style_theme::ThemeRef>,
        want_theme: Option<crate::style_theme::ThemeRef>,
    ) -> Option<u16> {
        let font = self.node("fonts", font_id)?;
        if font.default_namespace() != Some(NS) {
            return None;
        }
        let font_range = font.range();
        let mut edits = Vec::new();
        let mut prefix = String::new();
        if old.font != new.font {
            if new.font.is_empty()
                || new.font.encode_utf16().count() > 31
                || !super::safe_xml_text(&new.font)
            {
                return None;
            }
            let mut names = font.children().filter(|n| n.has_tag_name((NS, "name")));
            let escaped = quick_xml::escape::escape(&new.font);
            if let Some(name) = names.next() {
                let attr = name.attribute_node("val")?;
                let normalized = crate::style::normalize_font(attr.value());
                if names.next().is_some()
                    || !(normalized == old.font || old.font.is_empty() && normalized == "Calibri")
                {
                    return None;
                }
                edits.push((attr.range_value(), escaped.to_string()));
            } else if old.font.is_empty() {
                prefix.push_str(&format!("<name val=\"{escaped}\"/>"));
            } else {
                return None;
            }
            // Explicit typeface selection supersedes the major/minor theme font.
            for scheme in font.children().filter(|n| n.has_tag_name((NS, "scheme"))) {
                if scheme.attributes().len() != 1
                    || !matches!(scheme.attribute("val"), Some("none" | "major" | "minor"))
                    || scheme.children().any(|n| n.is_element())
                {
                    return None;
                }
                edits.push((scheme.range(), String::new()));
            }
        }
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
        if (old.underline, old.double_underline) != (new.underline, new.double_underline) {
            let mut props = font.children().filter(|n| n.has_tag_name((NS, "u")));
            let prop = props.next();
            let source = match prop.map(|p| p.attribute("val").unwrap_or("single")) {
                None | Some("none") => (false, false),
                Some("single") => (true, false),
                Some("double") => (true, true),
                _ => return None, // Accounting underlines are not represented by CellStyle.
            };
            if props.next().is_some()
                || source != (old.underline, old.double_underline)
                || (!new.underline && new.double_underline)
            {
                return None;
            }
            let replacement = match (new.underline, new.double_underline) {
                (true, true) => r#"<u val="double"/>"#,
                (true, false) => "<u/>",
                _ => "",
            };
            if let Some(prop) = prop {
                if prop.children().any(|n| n.is_element())
                    || prop
                        .attributes()
                        .any(|a| a.namespace().is_some() || a.name() != "val")
                {
                    return None;
                }
                edits.push((prop.range(), replacement.to_owned()));
            } else {
                prefix.push_str(replacement);
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
        if old.color != new.color || was_theme != want_theme {
            let was = was_theme.map_or(Paint::Rgb(old.color), |t| Paint::Theme(t.idx, t.tint));
            let want = want_theme.map_or(Paint::Rgb(new.color), |t| Paint::Theme(t.idx, t.tint));
            let mut colors = font.children().filter(|n| n.has_tag_name((NS, "color")));
            let new_color = want.xml("color")?;
            if let Some(color) = colors.next() {
                if colors.next().is_some() || Paint::read(color)? != was {
                    return None;
                }
                edits.push((color.range(), new_color));
            } else if was == Paint::Rgb([0; 3]) {
                prefix.push_str(&new_color);
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

    fn alignment_edits(
        &self,
        xf: Node<'_, '_>,
        old: &CellStyle,
        new: &CellStyle,
    ) -> Option<Vec<(Range<usize>, String)>> {
        let range = xf.range();
        let mut edits = Vec::new();
        // Enabling an XF's local alignment must not change inherited
        // alignment attributes that the model cannot represent.
        match xf.attribute("applyAlignment") {
            Some("1" | "true") => {}
            None | Some("0" | "false") => {
                let parent = xf.attribute("xfId").unwrap_or("0").parse().ok()?;
                let record = self.node("cellStyleXfs", parent);
                if record.is_none()
                    && (parent != 0
                        || self
                            .doc
                            .root_element()
                            .children()
                            .any(|n| n.has_tag_name((NS, "cellStyleXfs"))))
                    || record
                        .is_some_and(|p| p.children().any(|n| n.has_tag_name((NS, "alignment"))))
                {
                    return None;
                }
            }
            _ => return None,
        }
        let mut children = xf.children().filter(|n| n.has_tag_name((NS, "alignment")));
        let alignment = children.next();
        if children.next().is_some() {
            return None;
        }
        let attribute = |name| alignment.and_then(|n| n.attribute(name));
        let mut changed = Vec::new();
        for (name, was, want) in [
            ("wrapText", old.wrap, new.wrap),
            ("shrinkToFit", old.shrink_to_fit, new.shrink_to_fit),
        ] {
            if was == want {
                continue;
            }
            let raw = match attribute(name) {
                None | Some("false" | "0") => false,
                Some("true" | "1") => true,
                _ => return None,
            };
            if raw != was {
                return None;
            }
            changed.push((name, u8::from(want).to_string()));
        }
        if old.indent != new.indent {
            if new.indent > 15
                || attribute("indent").unwrap_or("0").parse::<u8>().ok()? != old.indent
            {
                return None;
            }
            changed.push(("indent", new.indent.to_string()));
        }
        if old.halign != new.halign {
            let horizontal = [
                "general",
                "left",
                "center",
                "right",
                "centerContinuous",
                "distributed",
            ];
            if attribute("horizontal").unwrap_or("general")
                != *horizontal.get(usize::from(old.halign))?
            {
                return None;
            }
            changed.push((
                "horizontal",
                horizontal.get(usize::from(new.halign))?.to_string(),
            ));
        }
        if old.valign != new.valign {
            let old_raw = attribute("vertical");
            let modeled = match old_raw {
                Some("top") => 1,
                Some("center") => 2,
                Some("bottom") => 3,
                None => 0,
                _ => return None,
            };
            if old.valign != modeled && !(matches!(old.valign, 0 | 3) && matches!(modeled, 0 | 3)) {
                return None;
            }
            if new.valign == 0 {
                if let Some(attr) = alignment.and_then(|n| n.attribute_node("vertical")) {
                    edits.push((attr.range(), String::new()));
                }
            } else {
                let value = ["", "top", "center", "bottom"].get(new.valign as usize)?;
                changed.push(("vertical", value.to_string()));
            }
        }
        if (old.vertical_text, old.rotation) != (new.vertical_text, new.rotation) {
            let rotation = |st: &CellStyle| match (st.vertical_text, st.rotation) {
                (true, 0) => Some(255i16),
                (false, degrees @ -90..=-1) => Some(90 - degrees),
                (false, degrees @ 0..=90) => Some(degrees),
                _ => None,
            };
            if attribute("textRotation")
                .unwrap_or("0")
                .parse::<i16>()
                .ok()?
                != rotation(old)?
            {
                return None;
            }
            changed.push(("textRotation", rotation(new)?.to_string()));
        }
        let mut insert = String::new();
        for (name, value) in changed {
            if let Some(existing) = alignment.and_then(|n| n.attribute_node(name)) {
                edits.push((existing.range_value(), value));
            } else {
                insert.push_str(&format!(" {name}=\"{value}\""));
            }
        }
        if !insert.is_empty() {
            if let Some(alignment) = alignment {
                let raw = &self.xml[alignment.range()];
                if !raw.starts_with("<alignment") {
                    return None;
                }
                let at = alignment.range().start + "<alignment".len();
                edits.push((at..at, insert));
            } else {
                let tag = format!("<alignment{insert}/>");
                let raw = &self.xml[range.clone()];
                if raw.ends_with("/>") {
                    edits.push((range.end - 2..range.end, format!(">{tag}</xf>")));
                } else {
                    let mut reader = quick_xml::Reader::from_str(raw);
                    reader.read_event().ok()?;
                    let at = range.start + reader.buffer_position() as usize;
                    edits.push((at..at, tag));
                }
            }
        }
        Some(edits)
    }

    fn protection_applied(&self, xf: Node<'_, '_>, old: &CellStyle) -> Option<()> {
        match xf.attribute("applyProtection") {
            Some("1" | "true") => {}
            None | Some("0" | "false") => {
                let parent = xf.attribute("xfId").unwrap_or("0").parse().ok()?;
                let record = self.node("cellStyleXfs", parent);
                if record.is_none()
                    && (parent != 0
                        || self
                            .doc
                            .root_element()
                            .children()
                            .any(|n| n.has_tag_name((NS, "cellStyleXfs"))))
                    || record
                        .is_some_and(|p| p.children().any(|n| n.has_tag_name((NS, "protection"))))
                {
                    return None;
                }
            }
            _ => return None,
        }
        let mut children = xf.children().filter(|n| n.has_tag_name((NS, "protection")));
        let prot = children.next();
        if children.next().is_some() {
            return None;
        }
        if let Some(p) = prot {
            if p.children().any(|n| n.is_element())
                || p.attributes()
                    .any(|a| a.namespace().is_some() || !matches!(a.name(), "locked" | "hidden"))
            {
                return None;
            }
        }
        let bool_attr = |name: &str, default: bool| -> Option<bool> {
            match prot.and_then(|p| p.attribute(name)) {
                None => Some(default),
                Some("1" | "true") => Some(true),
                Some("0" | "false") => Some(false),
                _ => None,
            }
        };
        if bool_attr("locked", true)? != old.locked || bool_attr("hidden", false)? != old.hidden {
            return None;
        }
        Some(())
    }

    fn apply_protection_tag(raw_xf: &str, new: &CellStyle) -> Option<String> {
        let wrapped = format!("<styleSheet xmlns=\"{NS}\">{raw_xf}</styleSheet>");
        let doc = Document::parse(&wrapped).ok()?;
        let xf = doc.root_element().first_element_child()?;
        let base = "<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">".len();
        let tag = format!(
            "<protection locked=\"{}\" hidden=\"{}\"/>",
            u8::from(new.locked),
            u8::from(new.hidden)
        );
        if let Some(existing) = xf.children().find(|n| n.has_tag_name((NS, "protection"))) {
            let r = existing.range();
            let mut out = String::with_capacity(raw_xf.len() + tag.len());
            out.push_str(&raw_xf[..r.start - base]);
            out.push_str(&tag);
            out.push_str(&raw_xf[r.end - base..]);
            return Some(out);
        }
        if raw_xf.ends_with("/>") {
            let mut out = String::with_capacity(raw_xf.len() + tag.len() + 6);
            out.push_str(&raw_xf[..raw_xf.len() - 2]);
            out.push('>');
            out.push_str(&tag);
            out.push_str("</xf>");
            return Some(out);
        }
        let at = if let Some(align) = xf.children().find(|n| n.has_tag_name((NS, "alignment"))) {
            align.range().end - base
        } else if let Some(ext) = xf.children().find(|n| n.has_tag_name((NS, "extLst"))) {
            ext.range().start - base
        } else {
            raw_xf.rfind("</xf>")?
        };
        let mut out = String::with_capacity(raw_xf.len() + tag.len());
        out.push_str(&raw_xf[..at]);
        out.push_str(&tag);
        out.push_str(&raw_xf[at..]);
        Some(out)
    }

    fn with_fields(
        &mut self,
        base: u16,
        mut fields: Vec<(&str, u16)>,
        old: &CellStyle,
        new: &CellStyle,
    ) -> Option<u16> {
        let xf = self.node("cellXfs", base)?;
        let range = xf.range();
        let mut edits = Vec::new();
        if (
            old.wrap,
            old.shrink_to_fit,
            old.indent,
            old.halign,
            old.valign,
            old.rotation,
            old.vertical_text,
        ) != (
            new.wrap,
            new.shrink_to_fit,
            new.indent,
            new.halign,
            new.valign,
            new.rotation,
            new.vertical_text,
        ) {
            edits.extend(self.alignment_edits(xf, old, new)?);
            fields.push(("applyAlignment", 1));
        }
        let protection_changed = (old.locked, old.hidden) != (new.locked, new.hidden);
        if protection_changed {
            self.protection_applied(xf, old)?;
            fields.push(("applyProtection", 1));
        }
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
            // With a bare <xf/>, an added child expands the slash at the same
            // offset. Emit the new attributes before that replacement.
            edits.insert(0, (name_end..name_end, attrs));
        }
        let mut raw_xf = apply_edits(&self.xml[range.clone()], range.start, edits);
        if protection_changed {
            raw_xf = Self::apply_protection_tag(&raw_xf, new)?;
        }
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
        if let Some(borders) = self.borders { borders.changes(&mut edits); }
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
    fn new_alignment_handles_an_empty_xf_and_leaves_inherited_alignment_to_the_fallback() {
        let xml = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font/></fonts><cellXfs count="1"><xf/></cellXfs></styleSheet>"#;
        let mut styles = StyleTable::new();
        let id = styles.intern(CellStyle {wrap:true, ..styles.get(0).clone()});
        let mut delta = Styles::new(xml).unwrap();
        assert_eq!(delta.resolve(0,0,id,&styles),Some(1));
        let (encoded,_) = delta.finish().unwrap();
        let doc = Document::parse(&encoded).unwrap();
        let rows: Vec<_> = doc.descendants().filter(|n| n.has_tag_name((NS,"xf"))).collect();
        assert_eq!(rows[1].attribute("applyAlignment"),Some("1"));
        assert_eq!(rows[1].first_element_child().unwrap().attribute("wrapText"),Some("1"));
        let inherited = xml.replace("<cellXfs",r#"<cellStyleXfs count="1"><xf><alignment readingOrder="2"/></xf></cellStyleXfs><cellXfs"#);
        assert!(Styles::new(&inherited).unwrap().resolve(0,0,id,&styles).is_none());
    }

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
