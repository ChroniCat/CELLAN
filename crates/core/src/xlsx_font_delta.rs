//! Append verified variations of an existing cell XF. Existing records and
//! extension markup stay byte-for-byte intact. Raw indices never move.
use crate::style::{CellStyle, StyleTable};
use quick_xml::{Reader, events::Event};
use roxmltree::{Document, Node};
use std::{borrow::Cow, collections::HashMap, ops::Range};

const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

struct Table<'a> {
    rows: Vec<Range<usize>>,
    count: Range<usize>,
    end: usize,
    close_empty: Option<String>,
    added: Vec<String>,
    lookup: Option<HashMap<Cow<'a, str>, u16>>,
}

impl<'a> Table<'a> {
    fn intern(&mut self, xml: &'a str, value: String) -> Option<u16> {
        let lookup = self.lookup.get_or_insert_with(|| {
            let mut map = HashMap::with_capacity(self.rows.len() + 8);
            for (i, r) in self.rows.iter().enumerate() {
                if let Ok(idx) = u16::try_from(i) {
                    map.entry(Cow::Borrowed(&xml[r.clone()])).or_insert(idx);
                }
            }
            map
        });
        if let Some(&idx) = lookup.get(value.as_str()) {
            return Some(idx);
        }
        let idx = u16::try_from(self.rows.len() + self.added.len()).ok()?;
        // Leave room for the collection count and avoid Excel's style limit.
        if idx >= 65_490 {
            return None;
        }
        lookup.insert(Cow::Owned(value.clone()), idx);
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

/// 在 `<tag ...>` 原始字节串内定位某个属性的值范围（不含引号），避免整表 DOM 解析。
fn find_attr_value_range(xml: &str, tag_start: usize, tag_end: usize, target: &str) -> Option<Range<usize>> {
    let b = xml.as_bytes();
    let mut i = tag_start;
    if i >= tag_end || b[i] != b'<' {
        return None;
    }
    i += 1;
    while i < tag_end && !matches!(b[i], b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>') {
        i += 1;
    }
    while i < tag_end {
        while i < tag_end && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if i >= tag_end || b[i] == b'>' || b[i] == b'/' {
            break;
        }
        let kstart = i;
        while i < tag_end && !matches!(b[i], b' ' | b'\t' | b'\r' | b'\n' | b'=' | b'/' | b'>') {
            i += 1;
        }
        let key = &xml[kstart..i];
        while i < tag_end && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if i >= tag_end || b[i] != b'=' {
            return None;
        }
        i += 1;
        while i < tag_end && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if i >= tag_end || (b[i] != b'"' && b[i] != b'\'') {
            return None;
        }
        let q = b[i];
        i += 1;
        let vstart = i;
        while i < tag_end && b[i] != q {
            i += 1;
        }
        if i >= tag_end {
            return None;
        }
        let vend = i;
        i += 1;
        if key == target {
            return Some(vstart..vend);
        }
    }
    None
}

fn parse_fragment(root_open: &str, raw: &str) -> Option<(String, usize)> {
    let mut wrapped = String::with_capacity(root_open.len() + raw.len() + "</styleSheet>".len());
    wrapped.push_str(root_open);
    wrapped.push_str(raw);
    wrapped.push_str("</styleSheet>");
    Some((wrapped, root_open.len()))
}

fn shift_range(r: Range<usize>, wrap_offset: usize, xml_start: usize) -> Range<usize> {
    (r.start - wrap_offset + xml_start)..(r.end - wrap_offset + xml_start)
}

fn effective_font_name(name: &str) -> &str {
    if name.is_empty() { "Calibri" } else { name }
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
        let bg = self
            .bg
            .or_else(|| (self.kind == "solid").then_some(Paint::Indexed(64)));
        if self.fg.is_none() && bg.is_none() {
            return Some(format!("<fill><patternFill patternType=\"{}\"/></fill>", self.kind));
        }
        let mut out = format!("<fill><patternFill patternType=\"{}\">", self.kind);
        if let Some(c) = self.fg {
            out.push_str(&c.xml("fgColor")?);
        }
        if let Some(c) = bg {
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

struct NumberFormats<'a> {
    table: Option<Table<'a>>,
    before_fonts: usize,
    codes: HashMap<u16, String>,
    next: Option<u16>,
    added: Vec<String>,
}

impl<'a> NumberFormats<'a> {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    NumFmts,
    Fonts,
    Fills,
    Borders,
    CellStyleXfs,
    CellXfs,
}

impl Section {
    fn from_tag(name: &str) -> Option<Self> {
        match name {
            "numFmts" => Some(Self::NumFmts),
            "fonts" => Some(Self::Fonts),
            "fills" => Some(Self::Fills),
            "borders" => Some(Self::Borders),
            "cellStyleXfs" => Some(Self::CellStyleXfs),
            "cellXfs" => Some(Self::CellXfs),
            _ => None,
        }
    }
    fn tag_name(self) -> &'static str {
        match self {
            Self::NumFmts => "numFmts",
            Self::Fonts => "fonts",
            Self::Fills => "fills",
            Self::Borders => "borders",
            Self::CellStyleXfs => "cellStyleXfs",
            Self::CellXfs => "cellXfs",
        }
    }
    fn item_name(self) -> &'static str {
        match self {
            Self::NumFmts => "numFmt",
            Self::Fonts => "font",
            Self::Fills => "fill",
            Self::Borders => "border",
            Self::CellStyleXfs => "xf",
            Self::CellXfs => "xf",
        }
    }
}

struct SectionSlot<'a> {
    seen: usize,
    valid: bool,
    table: Option<Table<'a>>,
}

impl<'a> Default for SectionSlot<'a> {
    fn default() -> Self {
        Self {
            seen: 0,
            valid: true,
            table: None,
        }
    }
}

pub(super) struct Styles<'a> {
    xml: &'a str,
    root_open: String,
    fonts: Table<'a>,
    fills: Option<Table<'a>>,
    borders: Option<Table<'a>>,
    cell_style_xfs: Option<Vec<Range<usize>>>,
    has_cell_style_xfs: bool,
    formats: Option<NumberFormats<'a>>,
    xfs: Table<'a>,
    resolved: HashMap<(u16, u16, u16), u16>,
}

impl<'a> Styles<'a> {
    /// 单遍流式索引 `xl/styles.xml`：不构造全量 `roxmltree` DOM，
    /// 仅记录各表子项字节切片范围，后续按需对单个修改记录做微片段解析。
    pub(super) fn new(xml: &'a str) -> Option<Self> {
        let mut reader = Reader::from_str(xml);
        let mut depth = 0usize;
        let mut root_open = String::new();
        let mut saw_root = false;

        let mut fonts_slot = SectionSlot::default();
        let mut fills_slot = SectionSlot::default();
        let mut borders_slot = SectionSlot::default();
        let mut xfs_slot = SectionSlot::default();
        let mut num_fmts_slot = SectionSlot::default();
        let mut cell_style_xfs_rows: Vec<Range<usize>> = Vec::new();
        let mut has_cell_style_xfs = false;
        let mut cell_style_xfs_count_seen = 0usize;

        let mut before_fonts: Option<usize> = None;
        let mut highest_num_fmt_id: u16 = 163;
        let mut num_fmt_id_overflow = false;
        let mut num_fmt_codes: HashMap<u16, String> = HashMap::new();
        let mut num_fmts_attrs_ok = true;

        let mut active: Option<(Section, usize, Option<Range<usize>>, Option<usize>, Vec<Range<usize>>, bool)> = None;

        loop {
            let start = reader.buffer_position() as usize;
            let ev = reader.read_event().ok()?;
            let end = reader.buffer_position() as usize;
            match ev {
                Event::Start(ref e) | Event::Empty(ref e) => {
                    let empty = matches!(ev, Event::Empty(_));
                    let qname = e.name();
                    let name = qname.as_ref();

                    // 收集任意深度上的 numFmtId（与原先 root.descendants() 保持一致）
                    for a in e.attributes() {
                        let a = a.ok()?;
                        if a.key.as_ref() == "numFmtId" {
                            match a.value.parse::<u16>() {
                                Ok(id) => highest_num_fmt_id = highest_num_fmt_id.max(id),
                                Err(_) => num_fmt_id_overflow = true,
                            }
                        }
                    }

                    if depth == 0 {
                        if saw_root || name != "styleSheet" {
                            return None;
                        }
                        let mut default_ns_ok = false;
                        for a in e.attributes() {
                            let a = a.ok()?;
                            if a.key.as_ref() == "xmlns"
                                && quick_xml::escape::unescape(&a.value).ok().as_deref() == Some(NS)
                            {
                                default_ns_ok = true;
                            }
                        }
                        if !default_ns_ok || empty {
                            return None;
                        }
                        root_open = xml[start..end].to_owned();
                        saw_root = true;
                        depth = 1;
                    } else if depth == 1 {
                        if let Some(sec) = Section::from_tag(name) {
                            let mut bad_ns = false;
                            let mut count_val: Option<usize> = None;
                            for a in e.attributes() {
                                let a = a.ok()?;
                                if a.key.as_ref() == "xmlns"
                                    && quick_xml::escape::unescape(&a.value).ok().as_deref() != Some(NS)
                                {
                                    bad_ns = true;
                                } else if a.key.as_ref() == "count" {
                                    count_val = a.value.parse::<usize>().ok();
                                }
                            }
                            if sec == Section::Fonts && before_fonts.is_none() {
                                before_fonts = Some(start);
                            }
                            if sec == Section::CellStyleXfs {
                                has_cell_style_xfs = true;
                                cell_style_xfs_count_seen += 1;
                                if empty {
                                    // empty <cellStyleXfs/>
                                } else {
                                    active = Some((sec, start, None, None, Vec::new(), !bad_ns));
                                    depth = 2;
                                }
                                continue;
                            }
                            let count_range = find_attr_value_range(xml, start, end, "count");
                            if empty {
                                let valid = !bad_ns
                                    && count_val == Some(0)
                                    && count_range.is_some()
                                    && xml[start..end].ends_with("/>")
                                    && xml[start..end].starts_with(&format!("<{}", sec.tag_name()));
                                let slot = match sec {
                                    Section::NumFmts => &mut num_fmts_slot,
                                    Section::Fonts => &mut fonts_slot,
                                    Section::Fills => &mut fills_slot,
                                    Section::Borders => &mut borders_slot,
                                    Section::CellXfs => &mut xfs_slot,
                                    Section::CellStyleXfs => unreachable!(),
                                };
                                slot.seen += 1;
                                if valid && slot.seen == 1 {
                                    slot.table = Some(Table {
                                        rows: Vec::new(),
                                        count: count_range.unwrap(),
                                        end: end - 2,
                                        close_empty: Some(format!("</{}>", sec.tag_name())),
                                        added: Vec::new(),
                                        lookup: None,
                                    });
                                } else {
                                    slot.valid = false;
                                    slot.table = None;
                                }
                            } else {
                                let valid = !bad_ns && count_val.is_some() && count_range.is_some();
                                let cap = count_val.unwrap_or(0).min(65_536);
                                active = Some((sec, start, count_range, count_val, Vec::with_capacity(cap), valid));
                                depth = 2;
                            }
                        } else if !empty {
                            depth = 2;
                        }
                    } else if depth == 2 {
                        if let Some((sec, _, _, _, ref mut rows, ref mut valid)) = active {
                            let item_name = sec.item_name();
                            if name != item_name {
                                *valid = false;
                            }
                            let mut bad_ns = false;
                            if sec == Section::NumFmts {
                                let mut id_opt = None;
                                let mut fmt_opt = None;
                                for a in e.attributes() {
                                    let a = a.ok()?;
                                    let k = a.key.as_ref();
                                    if k == "xmlns" {
                                        if quick_xml::escape::unescape(&a.value).ok().as_deref() != Some(NS) {
                                            bad_ns = true;
                                        }
                                    } else if k == "numFmtId" {
                                        id_opt = a.value.parse::<u16>().ok();
                                    } else if k == "formatCode" {
                                        fmt_opt = quick_xml::escape::unescape(&a.value).ok().map(Cow::into_owned);
                                    } else {
                                        num_fmts_attrs_ok = false;
                                    }
                                }
                                if !empty {
                                    // 检查是否有子元素
                                    let mut sub_depth = 1usize;
                                    while sub_depth > 0 {
                                        match reader.read_event().ok()? {
                                            Event::Start(_) => {
                                                num_fmts_attrs_ok = false;
                                                sub_depth += 1;
                                            }
                                            Event::Empty(_) => {
                                                num_fmts_attrs_ok = false;
                                            }
                                            Event::End(_) => sub_depth -= 1,
                                            Event::Eof => return None,
                                            _ => {}
                                        }
                                    }
                                }
                                let item_end = reader.buffer_position() as usize;
                                if let (Some(id), Some(fc)) = (id_opt, fmt_opt) {
                                    if num_fmt_codes.insert(id, fc).is_some() {
                                        num_fmts_attrs_ok = false;
                                    }
                                } else {
                                    num_fmts_attrs_ok = false;
                                }
                                if bad_ns {
                                    *valid = false;
                                }
                                rows.push(start..item_end);
                                continue;
                            } else {
                                for a in e.attributes() {
                                    let a = a.ok()?;
                                    if a.key.as_ref() == "xmlns"
                                        && quick_xml::escape::unescape(&a.value).ok().as_deref() != Some(NS)
                                    {
                                        bad_ns = true;
                                    }
                                }
                                if bad_ns {
                                    *valid = false;
                                }
                                if !empty {
                                    reader.read_to_end(e.name()).ok()?;
                                }
                                let item_end = reader.buffer_position() as usize;
                                rows.push(start..item_end);
                                continue;
                            }
                        } else if !empty {
                            depth += 1;
                        }
                    } else if !empty {
                        depth += 1;
                    }
                }
                Event::End(_) => {
                    if depth == 2 && active.is_some() {
                        let (sec, _sec_start, count_range, count_val, rows, valid) = active.take().unwrap();
                        let close = format!("</{}>", sec.tag_name());
                        if sec == Section::CellStyleXfs {
                            if cell_style_xfs_count_seen == 1 && valid {
                                cell_style_xfs_rows = rows;
                            }
                        } else {
                            let ok = valid
                                && count_val == Some(rows.len())
                                && xml[start..end] == close;
                            let slot = match sec {
                                Section::NumFmts => &mut num_fmts_slot,
                                Section::Fonts => &mut fonts_slot,
                                Section::Fills => &mut fills_slot,
                                Section::Borders => &mut borders_slot,
                                Section::CellXfs => &mut xfs_slot,
                                Section::CellStyleXfs => unreachable!(),
                            };
                            slot.seen += 1;
                            if ok && slot.seen == 1 {
                                slot.table = Some(Table {
                                    rows,
                                    count: count_range.unwrap(),
                                    end: start,
                                    close_empty: None,
                                    added: Vec::new(),
                                    lookup: None,
                                });
                            } else {
                                slot.valid = false;
                                slot.table = None;
                            }
                        }
                    }
                    depth = depth.checked_sub(1)?;
                }
                Event::Eof => break,
                _ => {}
            }
        }
        if !saw_root || depth != 0 {
            return None;
        }
        let fonts = (fonts_slot.valid && fonts_slot.seen == 1).then_some(fonts_slot.table)??;
        let xfs = (xfs_slot.valid && xfs_slot.seen == 1).then_some(xfs_slot.table)??;
        let fills = (fills_slot.valid && fills_slot.seen == 1)
            .then_some(fills_slot.table)
            .flatten();
        let borders = (borders_slot.valid && borders_slot.seen == 1)
            .then_some(borders_slot.table)
            .flatten();
        let formats = if !num_fmt_id_overflow
            && num_fmts_attrs_ok
            && (num_fmts_slot.seen == 0 || (num_fmts_slot.seen == 1 && num_fmts_slot.valid))
        {
            Some(NumberFormats {
                table: num_fmts_slot.table,
                before_fonts: before_fonts?,
                codes: num_fmt_codes,
                next: highest_num_fmt_id.checked_add(1),
                added: Vec::new(),
            })
        } else {
            None
        };
        let cell_style_xfs = (has_cell_style_xfs && cell_style_xfs_count_seen == 1)
            .then_some(cell_style_xfs_rows);
        Some(Self {
            xml,
            root_open,
            fonts,
            fills,
            borders,
            cell_style_xfs,
            has_cell_style_xfs,
            formats,
            xfs,
            resolved: HashMap::new(),
        })
    }

    fn row_range(&self, table: &str, index: u16) -> Option<Range<usize>> {
        let i = usize::from(index);
        match table {
            "fonts" => self.fonts.rows.get(i).cloned(),
            "fills" => self.fills.as_ref()?.rows.get(i).cloned(),
            "borders" => self.borders.as_ref()?.rows.get(i).cloned(),
            "cellXfs" => self.xfs.rows.get(i).cloned(),
            "cellStyleXfs" => self.cell_style_xfs.as_ref()?.get(i).cloned(),
            _ => None,
        }
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
        if effective_font_name(&old.font) != effective_font_name(&new.font)
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
            let fill_range = self.row_range("fills", original)?;
            let (wrapped, _) = parse_fragment(&self.root_open, &self.xml[fill_range])?;
            let doc = Document::parse(&wrapped).ok()?;
            let fill_node = doc.root_element().first_element_child()?;
            let mut actual = Pattern::raw(fill_node)?;
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
            fields.push(("fillId", self.fills.as_mut()?.intern(self.xml, fill)?));
            fields.push(("applyFill", 1));
        }
        if old.border != new.border {
            let original = self.component(base, "borderId", "applyBorder")?;
            let border_range = self.row_range("borders", original)?;
            let (wrapped, wrap_off) = parse_fragment(&self.root_open, &self.xml[border_range])?;
            let doc = Document::parse(&wrapped).ok()?;
            let border_node = doc.root_element().first_element_child()?;
            let _ = wrap_off;
            let value = border_variant(&wrapped, border_node, old, new)?;
            fields.push(("borderId", self.borders.as_mut()?.intern(self.xml, value)?));
            fields.push(("applyBorder", 1));
        }
        if old.format != new.format {
            let original = self.component(base, "numFmtId", "applyNumberFormat")?;
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
        let xf_range = self.row_range("cellXfs", base)?;
        let (wrapped, _) = parse_fragment(&self.root_open, &self.xml[xf_range])?;
        let doc = Document::parse(&wrapped).ok()?;
        let xf = doc.root_element().first_element_child()?;
        let local = xf.attribute(id).unwrap_or("0").parse().ok()?;
        if !matches!(xf.attribute(apply), Some("1" | "true")) {
            let apply_attr = xf.attribute(apply);
            if !matches!(apply_attr, None | Some("0" | "false")) {
                return None;
            }
            let parent = xf.attribute("xfId").unwrap_or("0").parse().ok()?;
            // Fresh CELLAN files may omit the entire optional cellStyleXfs table.
            // Only the default component of the implicit Normal style is unambiguous.
            let inherited = match self.row_range("cellStyleXfs", parent) {
                Some(prange) => {
                    let (pw, _) = parse_fragment(&self.root_open, &self.xml[prange])?;
                    let pdoc = Document::parse(&pw).ok()?;
                    let prow = pdoc.root_element().first_element_child()?;
                    prow.attribute(id).unwrap_or("0").parse::<u16>().ok()?
                }
                None if parent == 0 && !self.has_cell_style_xfs => 0,
                None => return None,
            };
            // When apply* is omitted on a cellXf and cellStyleXfs uses default 0,
            // Excel/umya apply the non-zero local component directly.
            if inherited != local && !(apply_attr.is_none() && inherited == 0) {
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
        let font_range = self.row_range("fonts", font_id)?;
        let (wrapped, wrap_off) = parse_fragment(&self.root_open, &self.xml[font_range.clone()])?;
        let doc = Document::parse(&wrapped).ok()?;
        let font = doc.root_element().first_element_child()?;
        if font.default_namespace() != Some(NS) {
            return None;
        }
        let map_r = |r: Range<usize>| shift_range(r, wrap_off, font_range.start);
        let mut edits = Vec::new();
        let mut prefix = String::new();
        let old_fname = effective_font_name(&old.font);
        let new_fname = effective_font_name(&new.font);
        if old_fname != new_fname {
            if new_fname.encode_utf16().count() > 31 || !super::safe_xml_text(new_fname) {
                return None;
            }
            let mut names = font.children().filter(|n| n.has_tag_name((NS, "name")));
            let escaped = quick_xml::escape::escape(new_fname);
            if let Some(name) = names.next() {
                let attr = name.attribute_node("val")?;
                let normalized = crate::style::normalize_font(attr.value());
                if names.next().is_some()
                    || !(normalized == old.font || old.font.is_empty() && normalized == "Calibri")
                {
                    return None;
                }
                edits.push((map_r(attr.range_value()), escaped.to_string()));
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
                edits.push((map_r(scheme.range()), String::new()));
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
                    map_r(prop.range()),
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
                edits.push((map_r(prop.range()), replacement.to_owned()));
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
            edits.push((map_r(size.range_value()), new.size.to_string()));
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
                edits.push((map_r(color.range()), new_color));
            } else if was == Paint::Rgb([0; 3]) {
                prefix.push_str(&new_color);
            } else {
                return None;
            }
        }
        if !prefix.is_empty() {
            // Insert after the start tag using the XML parser's first child offset.
            // Reject a self-closing/empty font where that position is ambiguous.
            let pos = map_r(font.first_child()?.range()).start;
            edits.insert(0, (pos..pos, prefix));
        }
        let raw_font = apply_edits(&self.xml[font_range.clone()], font_range.start, edits);
        self.fonts.intern(self.xml, raw_font)
    }

    fn alignment_edits(
        &self,
        xf: Node<'_, '_>,
        xf_range: Range<usize>,
        wrap_off: usize,
        old: &CellStyle,
        new: &CellStyle,
    ) -> Option<Vec<(Range<usize>, String)>> {
        let xf_start = xf_range.start;
        let map_r = |r: Range<usize>| shift_range(r, wrap_off, xf_start);
        let range = xf_range;
        let mut edits = Vec::new();
        // Enabling an XF's local alignment must not change inherited
        // alignment attributes that the model cannot represent.
        match xf.attribute("applyAlignment") {
            Some("1" | "true") => {}
            None | Some("0" | "false") => {
                let parent = xf.attribute("xfId").unwrap_or("0").parse().ok()?;
                let prange = self.row_range("cellStyleXfs", parent);
                if prange.is_none() && (parent != 0 || self.has_cell_style_xfs) {
                    return None;
                }
                if let Some(pr) = prange {
                    let (pw, _) = parse_fragment(&self.root_open, &self.xml[pr])?;
                    let pdoc = Document::parse(&pw).ok()?;
                    let prow = pdoc.root_element().first_element_child()?;
                    if prow.children().any(|n| n.has_tag_name((NS, "alignment"))) {
                        return None;
                    }
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
                    edits.push((map_r(attr.range()), String::new()));
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
                edits.push((map_r(existing.range_value()), value));
            } else {
                insert.push_str(&format!(" {name}=\"{value}\""));
            }
        }
        if !insert.is_empty() {
            if let Some(alignment) = alignment {
                let ar = map_r(alignment.range());
                let raw = &self.xml[ar.clone()];
                if !raw.starts_with("<alignment") {
                    return None;
                }
                let at = ar.start + "<alignment".len();
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
                let prange = self.row_range("cellStyleXfs", parent);
                if prange.is_none() && (parent != 0 || self.has_cell_style_xfs) {
                    return None;
                }
                if let Some(pr) = prange {
                    let (pw, _) = parse_fragment(&self.root_open, &self.xml[pr])?;
                    let pdoc = Document::parse(&pw).ok()?;
                    let prow = pdoc.root_element().first_element_child()?;
                    if prow.children().any(|n| n.has_tag_name((NS, "protection"))) {
                        return None;
                    }
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
        let range = self.row_range("cellXfs", base)?;
        let (wrapped, wrap_off) = parse_fragment(&self.root_open, &self.xml[range.clone()])?;
        let doc = Document::parse(&wrapped).ok()?;
        let xf = doc.root_element().first_element_child()?;
        let map_r = |r: Range<usize>| shift_range(r, wrap_off, range.start);
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
            edits.extend(self.alignment_edits(xf, range.clone(), wrap_off, old, new)?);
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
                edits.push((map_r(attr.range_value()), value.to_string()));
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
        if let Some(borders) = self.borders {
            borders.changes(&mut edits);
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
