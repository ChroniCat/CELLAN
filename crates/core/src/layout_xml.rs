//! Change only modeled dimension attributes. Column metadata is confined to
//! the small worksheet header. Row edits copy the original cell bodies into the output XML.
use crate::{
    Sheet,
    layout::{Axis, HiddenIndices},
};
use anyhow::{Context, Result, ensure};
use quick_xml::{NsReader, events::Event, name::ResolveResult};
use std::{collections::BTreeMap, ops::Range};

#[derive(Clone, Debug)]
pub(crate) struct Baseline {
    rows: Axis,
    cols: Axis,
    hidden: HiddenIndices,
    manual: HiddenIndices,
    hidden_cols: HiddenIndices,
    row_levels: Vec<u32>,
    col_levels: Vec<u32>,
}

impl Baseline {
    pub(crate) fn capture(s: &Sheet) -> Self {
        Self {
            rows: s.rows.clone(),
            cols: s.cols.clone(),
            hidden: s.hidden_rows.clone(),
            manual: s.hidden_rows_user.clone(),
            hidden_cols: s.hidden_cols.clone(),
            row_levels: s.row_levels.keys().copied().collect(),
            col_levels: s.col_levels.keys().copied().collect(),
        }
    }
}

#[derive(Clone, Default, PartialEq)]
struct Change {
    size: Option<Option<f32>>,
    hidden: Option<bool>,
    level: Option<u8>,
    style: Option<Option<u16>>,
}
type Attrs = Vec<(&'static str, Option<String>)>;

impl Change {
    fn attrs(&self, row: bool) -> Attrs {
        let mut out = Vec::new();
        if let Some(size) = self.size {
            let value = size.map(|px| {
                if row {
                    format!("{:.2}", px * 72.0 / 96.0)
                } else {
                    format!("{:.2}", ((px - 5.0) / 8.0).max(0.0))
                }
            });
            out.push((if row { "ht" } else { "width" }, value));
            out.push((
                if row { "customHeight" } else { "customWidth" },
                size.map(|_| "1".into()),
            ));
        }
        if let Some(hidden) = self.hidden {
            out.push(("hidden", hidden.then(|| "1".into())));
        }
        if let Some(lv) = self.level {
            out.push(("outlineLevel", (lv != 0).then(|| lv.to_string())));
        }
        if let Some(style) = self.style {
            out.push((
                if row { "s" } else { "style" },
                style.map(|xf| xf.to_string()),
            ));
            if row {
                out.push(("customFormat", style.map(|_| "1".into())));
            }
        }
        out
    }
    fn needs_record(&self) -> bool {
        self.size.flatten().is_some()
            || self.hidden == Some(true)
            || self.level.is_some_and(|l| l > 0)
            || self.style.flatten().is_some()
    }
}

#[derive(Default)]
pub(super) struct Changes {
    rows: BTreeMap<u32, Change>,
    cols: BTreeMap<u32, Change>,
}

impl Changes {
    pub(super) fn from_sheet(s: &Sheet, base: Option<&Baseline>) -> Self {
        let mut out = Self::default();
        if let Some(old) = base {
            for &r in &old.row_levels {
                out.rows.entry(r).or_default().level = Some(0);
            }
            for &c in &old.col_levels {
                out.cols.entry(c).or_default().level = Some(0);
            }
            for (&r, _) in old.rows.entries() {
                out.rows.entry(r).or_default().size = Some(None);
            }
            for (&c, _) in old.cols.entries() {
                // A wide source span supplies the model's default width without
                // changing sheetFormatPr. Removing a split-off column's width
                // outright would give it Excel's unrelated implicit default.
                let default = s.cols.default_size();
                out.cols.entry(c).or_default().size =
                    Some((default != crate::layout::DEFAULT_COL_W).then_some(default));
            }
            for &r in old.manual.iter().chain(old.hidden.iter()) {
                out.rows.entry(r).or_default().hidden = Some(false);
            }
            for &c in old.hidden_cols.iter() {
                out.cols.entry(c).or_default().hidden = Some(false);
            }
        }
        for (&r, &h) in s.rows.entries() {
            out.rows.entry(r).or_default().size = Some(Some(h));
        }
        for (&c, &w) in s.cols.entries() {
            out.cols.entry(c).or_default().size = Some(Some(w));
        }
        for &r in s.hidden_rows_user.iter().chain(s.hidden_rows.iter()) {
            out.rows.entry(r).or_default().hidden = Some(true);
        }
        for &c in s.hidden_cols.iter() {
            out.cols.entry(c).or_default().hidden = Some(true);
        }
        for (&r, &lv) in &s.row_levels {
            out.rows.entry(r).or_default().level = Some(lv);
        }
        for (&c, &lv) in &s.col_levels {
            out.cols.entry(c).or_default().level = Some(lv);
        }
        out
    }

    pub(super) fn columns(updates: &[(u32, f32, bool)]) -> Self {
        let mut out = Self::default();
        for &(c, w, h) in updates {
            out.cols.insert(
                c,
                Change {
                    size: Some(Some(w)),
                    hidden: Some(h),
                    ..Default::default()
                },
            );
        }
        out
    }
    pub(super) fn rows(updates: &[(u32, Option<f32>, bool)]) -> Self {
        let mut out = Self::default();
        for &(r, h, hidden) in updates {
            out.rows.insert(
                r,
                Change {
                    size: h.map(Some),
                    hidden: Some(hidden),
                    ..Default::default()
                },
            );
        }
        out
    }
    pub(super) fn outline(rows: &[(u32, u8)], cols: &[(u32, u8)]) -> Self {
        let mut out = Self::default();
        for &(r, lv) in rows {
            out.rows.entry(r).or_default().level = Some(lv);
        }
        for &(c, lv) in cols {
            out.cols.entry(c).or_default().level = Some(lv);
        }
        out
    }
}

fn qualified(qname: &str, name: &str) -> String {
    if let Some((prefix, _)) = qname.rsplit_once(':') {
        format!("{prefix}:{name}")
    } else {
        name.into()
    }
}

// Attribute values below are always numeric. Retain the original quoting,
// entities, namespace declarations, whitespace and unknown attributes exactly.
fn update_tag(raw: &str, attrs: &Attrs) -> Result<String> {
    let mut reader = quick_xml::Reader::from_str(raw);
    let (Event::Start(tag) | Event::Empty(tag)) = reader.read_event()? else {
        anyhow::bail!("无效的工作表标记")
    };
    let bytes = raw.as_bytes();
    let mut off = 1 + tag.name().as_ref().len();
    let mut last = 0;
    let mut out = String::new();
    let mut seen = Vec::new();
    for attr in tag.attributes() {
        let attr = attr?;
        let key = attr.key.as_ref();
        let start = off;
        while bytes[off].is_ascii_whitespace() {
            off += 1;
        }
        ensure!(raw[off..].starts_with(key), "列/行属性位置无效");
        off += key.len();
        while bytes[off].is_ascii_whitespace() {
            off += 1;
        }
        ensure!(bytes[off] == b'=', "列/行属性缺少等号");
        off += 1;
        while bytes[off].is_ascii_whitespace() {
            off += 1;
        }
        let quote = bytes[off];
        off += 1;
        let end = off
            + bytes[off..]
                .iter()
                .position(|b| *b == quote)
                .context("列/行属性未闭合")?;
        if let Some((key, value)) = attrs.iter().find(|(k, _)| *k == key) {
            seen.push(*key);
            match value {
                Some(v) if v != &raw[off..end] => {
                    out.push_str(&raw[last..off]);
                    out.push_str(v);
                    last = end;
                }
                None => {
                    out.push_str(&raw[last..start]);
                    last = end + 1;
                }
                _ => {}
            }
        }
        off = end + 1;
    }
    out.push_str(&raw[last..off]);
    for (key, value) in attrs {
        if !seen.contains(key)
            && let Some(v) = value
        {
            out.push_str(&format!(" {key}=\"{v}\""));
        }
    }
    out.push_str(&raw[off..]);
    Ok(out)
}

fn new_record(name: &str, row: bool, first: u32, last: u32, change: &Change) -> Result<String> {
    let id = if row {
        format!("r=\"{}\"", first + 1)
    } else {
        format!("min=\"{}\" max=\"{}\"", first + 1, last + 1)
    };
    update_tag(&format!("<{name} {id}/>"), &change.attrs(row))
}

type Edit = (Range<usize>, String);
fn render(xml: &str, mut edits: Vec<Edit>) -> Result<String> {
    edits.sort_by_key(|(r, _)| (r.start, r.end));
    let mut out = String::with_capacity(xml.len());
    let mut last = 0;
    for (r, s) in edits {
        ensure!(last <= r.start, "工作表补丁位置重叠");
        out.push_str(&xml[last..r.start]);
        out.push_str(&s);
        last = r.end;
    }
    out.push_str(&xml[last..]);
    Ok(out)
}

fn start_end(xml: &str, start: usize) -> Result<usize> {
    let mut reader = quick_xml::Reader::from_str(&xml[start..]);
    reader.read_event()?;
    Ok(start + reader.buffer_position() as usize)
}

pub(crate) fn styled_columns(header: &str, updates: &BTreeMap<u32, Option<u16>>) -> Result<String> {
    let changes = updates
        .iter()
        .map(|(&col, &style)| {
            (
                col,
                Change {
                    style: Some(style),
                    ..Default::default()
                },
            )
        })
        .collect();
    columns(header, "worksheet", &changes)
}

pub(crate) fn styled_row(tag: &str, style: Option<u16>) -> Result<String> {
    update_tag(
        tag,
        &Change {
            style: Some(style),
            ..Default::default()
        }
        .attrs(true),
    )
}

/// umya requires both endpoints and eagerly allocates one dimension per column.
/// Repair only the private DOM input, keeping the valid portion of every range.
pub(crate) fn repair_column_bounds(header: &str) -> Result<String> {
    let mut reader = quick_xml::Reader::from_str(header);
    let root_name = loop {
        match reader.read_event()? {
            Event::Start(tag) => break tag.name().as_ref().to_owned(),
            Event::Eof => anyhow::bail!("缺少 worksheet 根元素"),
            _ => {}
        }
    };
    let closed = format!("{header}</{root_name}>");
    let doc = roxmltree::Document::parse(&closed)?;
    let root = doc.root_element();
    let named = |n: roxmltree::Node, name| {
        n.is_element()
            && n.tag_name().name() == name
            && n.tag_name().namespace() == root.tag_name().namespace()
    };
    let mut edits = Vec::new();
    for group in root.children().filter(|n| named(*n, "cols")) {
        let mut group_edits = Vec::new();
        let mut retained = false;
        for col in group.children().filter(|n| named(*n, "col")) {
            let index = |name| {
                col.attribute(name)
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(0)
            };
            let (first, last) = (index("min"), index("max"));
            if first == 0 || first > last || first > crate::sync::MAX_COLS {
                group_edits.push((col.range(), String::new()));
                continue;
            }
            retained = true;
            if last > crate::sync::MAX_COLS {
                group_edits.push((
                    col.attribute_node("max").unwrap().range_value(),
                    crate::sync::MAX_COLS.to_string(),
                ));
            }
        }
        if !retained
            && !group_edits.is_empty()
            && group.children().all(|n| !n.is_element() || named(n, "col"))
        {
            edits.push((group.range(), String::new()));
        } else {
            edits.extend(group_edits);
        }
    }
    render(header, edits)
}

fn columns(header: &str, root_name: &str, changes: &BTreeMap<u32, Change>) -> Result<String> {
    patch_columns(header, root_name, changes, false)
}

fn patch_columns(
    header: &str,
    root_name: &str,
    changes: &BTreeMap<u32, Change>,
    unordered: bool,
) -> Result<String> {
    if changes.is_empty() {
        return Ok(header.into());
    }
    // Parse just the header, retaining offsets into the original XML. Inherited
    // namespaces of cols and col are resolved exactly as in the complete part.
    let closed = format!("{header}</{root_name}>");
    let doc = roxmltree::Document::parse(&closed)?;
    let root = doc.root_element();
    let name = |node: roxmltree::Node, local: &str| {
        node.is_element()
            && node.tag_name().namespace() == root.tag_name().namespace()
            && node.tag_name().name() == local
    };
    let groups: Vec<_> = root.children().filter(|n| name(*n, "cols")).collect();
    let mut todo = changes.iter().peekable();
    let mut edits = Vec::new();
    let mut previous = None;
    let mut final_group = None;
    let mut covered = std::collections::BTreeSet::new();
    for group in groups {
        let gr = group.range();
        let open_end = start_end(header, gr.start)?;
        let group_tag = &header[gr.start..open_end];
        let group_name = group_tag[1..]
            .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap();
        let col_name = qualified(group_name, "col");
        for col in group.children().filter(|n| name(*n, "col")) {
            let index = |attr: &str| -> Result<u32> {
                col.attribute(attr)
                    .context("列区间缺少编号")?
                    .parse::<u32>()?
                    .checked_sub(1)
                    .context("无效的列区间")
            };
            let first = index("min")?;
            let last = index("max")?;
            ensure!(first <= last && last < 16_384, "重叠或无效的列区间");
            if !unordered && previous.is_some_and(|p| p >= first) {
                return patch_columns(header, root_name, changes, true);
            }
            previous = Some(last);
            let range = col.range();
            let mut before = String::new();
            while let Some((&c, ch)) = todo.next_if(|(c, _)| !unordered && **c < first) {
                if ch.needs_record() {
                    before.push_str(&new_record(&col_name, false, c, c, ch)?);
                }
            }
            if !before.is_empty() {
                edits.push((range.start..range.start, before));
            }
            let mut pieces: Vec<(u32, u32, Option<&Change>)> = Vec::new();
            let mut next = first;
            for (&c, ch) in changes.range(first..=last) {
                if unordered {
                    covered.insert(c);
                } else {
                    todo.next();
                }
                if next < c {
                    pieces.push((next, c - 1, None));
                }
                if let Some((_, end, Some(prior))) = pieces.last_mut()
                    && *end + 1 == c
                    && *prior == ch
                {
                    *end = c;
                } else {
                    pieces.push((c, c, Some(ch)));
                }
                next = c + 1;
            }
            if pieces.is_empty() {
                continue;
            }
            if next <= last {
                pieces.push((next, last, None));
            }
            let open = start_end(header, range.start)?;
            let tag = &header[range.start..open];
            let tail = &header[open..range.end];
            ensure!(
                pieces.len() == 1
                    || !col
                        .children()
                        .any(|child| !child.is_text()
                            || !child.text().unwrap_or("").trim().is_empty()),
                "列定义含扩展子节点，无法安全拆分"
            );
            let mut text = String::new();
            for (a, b, ch) in pieces {
                let mut attrs = ch.map(|v| v.attrs(false)).unwrap_or_default();
                if a != first {
                    attrs.push(("min", Some((a + 1).to_string())));
                }
                if b != last {
                    attrs.push(("max", Some((b + 1).to_string())));
                }
                text.push_str(&update_tag(tag, &attrs)?);
                text.push_str(tail);
            }
            if text != header[range.clone()] {
                edits.push((range, text));
            }
        }
        final_group = Some((gr, open_end, group_name, col_name));
    }
    let mut append = String::new();
    let col_name = final_group
        .as_ref()
        .map(|v| v.3.clone())
        .unwrap_or_else(|| qualified(root_name, "col"));
    for (&c, ch) in todo {
        if !covered.contains(&c) && ch.needs_record() {
            append.push_str(&new_record(&col_name, false, c, c, ch)?);
        }
    }
    if !append.is_empty() {
        if let Some((r, open, gname, _)) = final_group {
            if open == r.end {
                edits.push((open - 2..open, format!(">{append}</{gname}>")));
            } else {
                let close = r.start + header[r.clone()].rfind("</").context("列集合未闭合")?;
                edits.push((close..close, append));
            }
        } else {
            let group = qualified(root_name, "cols");
            let at = header.len();
            edits.push((at..at, format!("<{group}>{append}</{group}>")));
        }
    }
    render(header, edits)
}

pub(super) fn apply(xml: &str, changes: &Changes) -> Result<String> {
    patch_rows(xml, changes, false)
}

fn patch_rows(xml: &str, changes: &Changes, unordered: bool) -> Result<String> {
    let mut covered = std::collections::BTreeSet::new();
    ensure!(
        changes
            .rows
            .last_key_value()
            .is_none_or(|(r, _)| *r < 1_048_576)
            && changes
                .cols
                .last_key_value()
                .is_none_or(|(c, _)| *c < 16_384),
        "工作表尺寸超出范围"
    );
    if changes.rows.is_empty() && changes.cols.is_empty() {
        return Ok(xml.into());
    }
    let mut reader = NsReader::from_str(xml);
    let mut root: Option<(String, String)> = None;
    let mut data = None;
    let mut previous = None;
    let mut rows = changes.rows.iter().peekable();
    let mut pos = 0;
    let mut out = String::new();
    loop {
        let start = reader.buffer_position() as usize;
        let (ns, event) = reader.read_resolved_event()?;
        let uri = match ns {
            ResolveResult::Bound(n) => n.0,
            ResolveResult::Unbound => "",
            ResolveResult::Unknown(_) => anyhow::bail!("工作表使用未声明的命名空间"),
        };
        let root_uri = root.is_none().then(|| uri.to_owned());
        let sheet_ns = root.as_ref().is_some_and(|(wns, _)| uri == wns);
        let end = reader.buffer_position() as usize;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(tag) if root.is_none() => {
                ensure!(
                    tag.local_name().as_ref() == "worksheet",
                    "缺少 worksheet 根元素"
                );
                root = Some((root_uri.unwrap(), tag.name().as_ref().to_owned()));
            }
            Event::Empty(tag) | Event::Start(tag) if root.is_some() => {
                let (_, root_name) = root.as_ref().unwrap();
                if data.is_none() && sheet_ns && tag.local_name().as_ref() == "sheetData" {
                    let row_name = qualified(tag.name().as_ref(), "row");
                    out = columns(&xml[..start], root_name, &changes.cols)?;
                    if empty {
                        let mut body = String::new();
                        for (&r, ch) in rows {
                            if ch.needs_record() {
                                body.push_str(&new_record(&row_name, true, r, r, ch)?);
                            }
                        }
                        if body.is_empty() {
                            out.push_str(&xml[start..]);
                        } else {
                            out.push_str(&xml[start..end - 2]);
                            out.push('>');
                            out.push_str(&body);
                            out.push_str(&format!("</{}>", tag.name().as_ref()));
                            out.push_str(&xml[end..]);
                        }
                        return Ok(out);
                    }
                    if changes.rows.is_empty() {
                        out.push_str(&xml[start..]);
                        return Ok(out);
                    }
                    data = Some(row_name);
                    pos = start;
                    continue;
                }
                if let Some(row_name) = &data
                    && sheet_ns
                    && tag.local_name().as_ref() == "row"
                {
                    let mut number = None;
                    for a in tag.attributes() {
                        let a = a?;
                        if a.key.as_ref() == "r" {
                            number =
                                Some(a.value.parse::<u32>()?.checked_sub(1).context("无效行号")?);
                        }
                    }
                    let r = number.unwrap_or_else(|| previous.map_or(0, |p| p + 1));
                    ensure!(r < 1_048_576, "无效的行顺序");
                    if !unordered && previous.is_some_and(|p| p >= r) {
                        drop(out);
                        return patch_rows(xml, changes, true);
                    }
                    previous = Some(r);
                    out.push_str(&xml[pos..start]);
                    pos = end;
                    while let Some((&i, ch)) = rows.next_if(|(i, _)| !unordered && **i < r) {
                        if ch.needs_record() {
                            out.push_str(&new_record(row_name, true, i, i, ch)?);
                        }
                    }
                    let change = if unordered {
                        let change = changes.rows.get(&r);
                        if change.is_some() {
                            covered.insert(r);
                        }
                        change
                    } else {
                        rows.next_if(|(i, _)| **i == r).map(|(_, v)| v)
                    };
                    if let Some(ch) = change {
                        out.push_str(&update_tag(&xml[start..end], &ch.attrs(true))?);
                    } else {
                        out.push_str(&xml[start..end]);
                    }
                }
                if !empty {
                    reader.read_to_end(tag.name())?;
                }
            }
            Event::End(_) if data.is_some() => {
                out.push_str(&xml[pos..start]);
                let row_name = data.as_ref().unwrap();
                for (&r, ch) in rows {
                    if !covered.contains(&r) && ch.needs_record() {
                        out.push_str(&new_record(row_name, true, r, r, ch)?);
                    }
                }
                out.push_str(&xml[start..]);
                return Ok(out);
            }
            Event::Eof => anyhow::bail!("工作表缺少 sheetData"),
            _ => {}
        }
    }
}

pub(super) fn apply_bytes(raw: &[u8], changes: &Changes) -> Result<Option<Vec<u8>>> {
    let little = raw.starts_with(&[0xff, 0xfe]) || raw.starts_with(b"<\0");
    let big = raw.starts_with(&[0xfe, 0xff]) || raw.starts_with(b"\0<");
    if little || big {
        let encoding = if little {
            encoding_rs::UTF_16LE
        } else {
            encoding_rs::UTF_16BE
        };
        let (original, _, invalid) = encoding.decode(raw);
        ensure!(!invalid, "工作表 UTF-16 数据无效");
        let updated = apply(&original, changes)?;
        if updated == original {
            return Ok(None);
        }
        let mut out = Vec::with_capacity(raw.len());
        if raw.starts_with(&[0xff, 0xfe]) || raw.starts_with(&[0xfe, 0xff]) {
            out.extend_from_slice(&raw[..2]);
        }
        for c in updated.encode_utf16() {
            out.extend_from_slice(&if little {
                c.to_le_bytes()
            } else {
                c.to_be_bytes()
            });
        }
        return Ok(Some(out));
    }
    let (bom, bytes) = if let Some(bytes) = raw.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        (&raw[..3], bytes)
    } else {
        (&raw[..0], raw)
    };
    let xml = std::str::from_utf8(bytes)?;
    let updated = apply(xml, changes)?;
    if updated == xml {
        return Ok(None);
    }
    if bom.is_empty() {
        Ok(Some(updated.into_bytes()))
    } else {
        let mut bytes = Vec::with_capacity(bom.len() + updated.len());
        bytes.extend_from_slice(bom);
        bytes.extend_from_slice(updated.as_bytes());
        Ok(Some(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

    #[test]
    fn removed_outline_levels_are_cleared_even_after_a_structural_move() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grouped.xlsx");
        let mut wb = crate::Workbook::new_blank();
        wb.sheets[0].row_levels.extend([(1, 1), (4, 2)]);
        wb.sheets[0].col_levels.extend([(1, 1), (4, 2)]);
        wb.sheets[0].layout_dirty = true;
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let mut wb = crate::xlsx::load(&path).unwrap();
        wb.sheets[0].row_levels.remove(&1);
        wb.sheets[0].col_levels.remove(&1);
        crate::structural::apply_structural_wb(
            &mut wb,
            0,
            &crate::structural::Structural::DeleteRows { at: 2, n: 1 },
        );
        crate::structural::apply_structural_wb(
            &mut wb,
            0,
            &crate::structural::Structural::DeleteCols { at: 2, n: 1 },
        );
        wb.sheets[0].layout_dirty = true;
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let wb = crate::xlsx::load(&path).unwrap();
        assert_eq!(wb.sheets[0].row_levels, [(3, 2)].into());
        assert_eq!(wb.sheets[0].col_levels, [(3, 2)].into());
    }

    #[test]
    fn irregular_dimension_order_updates_all_overlaps_without_duplicate_rows() {
        let xml = r#"<worksheet xmlns="urn:w"><cols><col min="2" max="3" x="1"/><col min="1" max="3" y="2"/></cols><sheetData><row r="4"><c r="A4"/></row><row r="1"><c r="A1"/></row></sheetData></worksheet>"#;
        let mut changes = Changes::outline(&[(0, 2), (1, 1), (3, 1)], &[(1, 3), (4, 2)]);
        changes.rows.entry(3).or_default().hidden = Some(true);
        let generated = apply(xml, &changes).unwrap();
        let doc = roxmltree::Document::parse(&generated).unwrap();
        let nodes = |tag| {
            doc.descendants()
                .filter(move |n| n.has_tag_name(("urn:w", tag)))
        };
        assert_eq!(nodes("row").count(), 3);
        assert_eq!(
            nodes("row")
                .find(|n| n.attribute("r") == Some("1"))
                .unwrap()
                .attribute("outlineLevel"),
            Some("2")
        );
        assert_eq!(
            nodes("row")
                .find(|n| n.attribute("r") == Some("4"))
                .unwrap()
                .attribute("hidden"),
            Some("1")
        );
        assert_eq!(
            nodes("col")
                .filter(
                    |n| n.attribute("min") == Some("2") && n.attribute("outlineLevel") == Some("3")
                )
                .count(),
            2
        );
        assert!(
            nodes("col").any(
                |n| n.attribute("min") == Some("5") && n.attribute("outlineLevel") == Some("2")
            )
        );
        assert!(generated.contains("<c r=\"A4\"/>") && generated.contains("<c r=\"A1\"/>"));
    }

    #[test]
    fn can_save_and_reopen_dimension_edits_on_out_of_order_third_party_sheets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unordered.xlsx");
        crate::xlsx::save(&path,&crate::Workbook::new_blank()).unwrap();
        let part = crate::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
        let xml = format!(r#"<worksheet xmlns="{NS}"><cols><col min="2" max="3"/><col min="1" max="3"/></cols><sheetData><row r="4"><c r="A4"><v>4</v></c></row><row r="1"><c r="A1"><v>1</v></c></row></sheetData></worksheet>"#);
        crate::fixups::write_parts(&path,&[(part,xml.into_bytes())]).unwrap();
        let mut wb = crate::xlsx::load(&path).unwrap();
        wb.sheets[0].rows.set(0,48.0);
        wb.sheets[0].cols.set(1,101.0);
        wb.sheets[0].layout_dirty=true;
        crate::xlsx::save_atomic(&path,&wb).unwrap();
        let back=crate::xlsx::load(&path).unwrap();
        assert_eq!(back.sheets[0].rows.size(0),48.);
        assert_eq!(back.sheets[0].cols.size(1),101.);
        assert_eq!(back.sheets[0].value(3,0),crate::Value::Number(4.));
        assert_eq!(back.sheets[0].value(0,0),crate::Value::Number(1.));
    }

    #[test]
    fn local_prefixes_empty_groups_and_prefixed_rows_retain_scope() {
        for group in [
            r#"<s:cols xmlns:s="urn:w" xmlns:e="urn:ext" e:keep="1"/>"#,
            r#"<s:cols xmlns:s="urn:w" xmlns:e="urn:ext" e:keep="1"></s:cols>"#,
            "",
        ] {
            let xml = format!(
                r#"<worksheet xmlns="urn:w"><sheetFormatPr defaultRowHeight="12"></sheetFormatPr>{group}<w:sheetData xmlns:w="urn:w"/><extLst><ext><row r="3" ht="777"/></ext></extLst></worksheet>"#
            );
            let mut edits = Changes::columns(&[(1, 100.0, false)]);
            edits.rows.insert(
                2,
                Change {
                    size: Some(Some(48.0)),
                    ..Default::default()
                },
            );
            let updated = apply(&xml, &edits).unwrap();
            let doc = roxmltree::Document::parse(&updated).unwrap();
            let children: Vec<_> = doc
                .root_element()
                .children()
                .filter(|n| n.is_element())
                .collect();
            assert_eq!(
                children
                    .iter()
                    .map(|n| n.tag_name().name())
                    .collect::<Vec<_>>(),
                ["sheetFormatPr", "cols", "sheetData", "extLst"]
            );
            if !group.is_empty() {
                assert_eq!(children[1].attribute(("urn:ext", "keep")), Some("1"));
            }
            let col = children[1].first_element_child().unwrap();
            assert!(col.has_tag_name(("urn:w", "col")));
            assert_eq!(col.attribute("min"), Some("2"));
            let row = children[2].first_element_child().unwrap();
            assert!(row.has_tag_name(("urn:w", "row")));
            assert_eq!(row.attribute("ht"), Some("36.00"));
            assert!(updated.contains(r#"<ext><row r="3" ht="777"/></ext>"#));
        }
    }

    #[test]
    fn utf16_layout_retains_its_declaration_and_byte_order() {
        let xml = r#"<?xml version="1.0" encoding="UTF-16"?><worksheet><sheetData><row r='1'><c r='A1'/></row></sheetData></worksheet>"#;
        let edits = Changes::rows(&[(0, Some(40.0), false)]);
        for little in [true, false] {
            let mut raw = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for c in xml.encode_utf16() {
                raw.extend_from_slice(&if little {
                    c.to_le_bytes()
                } else {
                    c.to_be_bytes()
                });
            }
            let data = apply_bytes(&raw, &edits).unwrap().unwrap();
            assert_eq!(data[..2], raw[..2]);
            let encoding = if little {
                encoding_rs::UTF_16LE
            } else {
                encoding_rs::UTF_16BE
            };
            let (s, _, invalid) = encoding.decode(&data);
            assert!(!invalid);
            assert!(s.contains(r#"encoding="UTF-16""#) && s.contains(r#"ht="30.00""#));
        }
    }

    #[test]
    fn layout_only_saves_preserve_unknown_columns_and_reset_a_split_wide_span() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("layout.xlsx");
        crate::xlsx::save(&file, &crate::Workbook::new_blank()).unwrap();
        let part = crate::patch::sheet_targets(&file).unwrap()["Sheet1"].clone();
        let xml = format!(
            r#"<worksheet xmlns="{NS}"><sheetFormatPr defaultRowHeight="13.5"></sheetFormatPr>
<cols xmlns:e="urn:test"><col min='1' max='1' width='16' customWidth='1' e:keep='a"b'/>
<col
 min="2" max="2" style="0" bestFit="1"/><col min="3" max="16384" width="14" e:wide="yes"/></cols>
<sheetData><row
 r='1' ht='36' customHeight='1' xmlns:q='urn:row' q:keep='yes'><c r='A1'><v>42</v></c></row></sheetData></worksheet>"#
        );
        crate::fixups::write_parts(&file, &[(part.clone(), xml.into_bytes())]).unwrap();
        let (mut wb, diag) = crate::xlsx::load_diag(&file).unwrap();
        assert!(diag.is_complete(), "{:?}", diag.problems);
        let default = wb.sheets[0].cols.default_size();
        for pass in 0..3 {
            let sh = &mut wb.sheets[0];
            match pass {
                0 => {
                    sh.rows.set(0, sh.rows.default_size());
                }
                1 => {
                    sh.cols.set(5, 165.0);
                }
                _ => {
                    sh.cols.set(5, default);
                }
            }
            sh.layout_dirty = true;
            let receipt = crate::xlsx::save_atomic_with_receipt(&file, &wb).unwrap();
            receipt.adopt_document(&mut wb, &file, true);
            let raw = String::from_utf8(crate::xlsx::read_part(&file, &part).unwrap()).unwrap();
            let doc = roxmltree::Document::parse(&raw).unwrap();
            let cols: Vec<_> = doc
                .descendants()
                .filter(|n| n.has_tag_name((NS, "col")))
                .collect();
            assert_eq!(cols[0].attribute(("urn:test", "keep")), Some("a\"b"));
            assert_eq!(cols[1].attribute("bestFit"), Some("1"));
            assert_eq!(cols[1].attribute("style"), Some("0"));
            let row = doc
                .descendants()
                .find(|n| n.has_tag_name((NS, "row")))
                .unwrap();
            assert_eq!(row.attribute(("urn:row", "keep")), Some("yes"));
            assert_eq!(
                (row.attribute("ht"), row.attribute("customHeight")),
                (None, None)
            );
            for col in &cols[2..] {
                assert_eq!(col.attribute(("urn:test", "wide")), Some("yes"));
            }
            let sixth = cols
                .iter()
                .find(|n| {
                    n.attribute("min").unwrap().parse::<u32>().unwrap() <= 6
                        && n.attribute("max").unwrap().parse::<u32>().unwrap() >= 6
                })
                .unwrap();
            assert_eq!(
                sixth.attribute("width").unwrap().parse::<f32>().unwrap(),
                if pass == 1 { 20.0 } else { 14.0 }
            );
            let (back, diag) = crate::xlsx::load_diag(&file).unwrap();
            assert!(diag.is_complete());
            assert_eq!(back.sheets[0].value(0, 0), crate::Value::Number(42.0));
            assert_eq!(
                back.sheets[0].cols.size(5),
                if pass == 1 { 165.0 } else { default }
            );
        }
    }
}
