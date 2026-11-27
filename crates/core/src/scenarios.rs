//! 方案管理器：一张表可以存多套「可变单元格 = 值」的组合（Excel 的方案管理器）。
//!
//! OOXML 位置：`<scenarios>` 必须排在 `<sheetData>` 之后、`<autoFilter>` / `<mergeCells>` 之前，
//! 也就是 sheetProtection / protectedRanges 这一段里。

use crate::model::{Sheet, Value};

use std::{collections::HashMap, ops::Range, sync::Arc};

#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: String,
    pub comment: String,
    /// ((行, 列), 值文本)
    pub cells: Vec<((u32, u32), String)>,
    pub hidden: bool,
    pub locked: bool,
    // Source record and input positions survive coordinate changes and allow opaque XML to round-trip.
    source_record: Option<usize>,
    source_inputs: HashMap<(u32, u32), Vec<usize>>,
    user: Option<String>,
    explicit_count: bool,
    // Rare per-input XML properties. Ordinary scenarios allocate no entries.
    input_properties: std::collections::HashMap<(u32, u32), Vec<(String, String)>>,
}

impl PartialEq for Scenario {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.comment == other.comment
            && self.cells == other.cells
            && self.hidden == other.hidden
            && self.locked == other.locked
            && self.user == other.user
            && self.explicit_count == other.explicit_count
            && self.input_properties == other.input_properties
    }
}

impl Scenario {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            comment: String::new(),
            cells: Vec::new(),
            hidden: false,
            locked: true,
            source_record: None,
            source_inputs: HashMap::new(),
            user: None,
            explicit_count: false,
            input_properties: Default::default(),
        }
    }
    pub fn replace_inputs(&mut self, captured: Scenario) {
        self.source_inputs
            .retain(|k, _| captured.cells.iter().any(|(key, _)| key == k));
        self.comment = captured.comment;
        self.cells = captured.cells;
        self.input_properties
            .retain(|key, _| self.cells.iter().any(|(k, _)| k == key));
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScenarioSettings {
    original: Option<Arc<str>>,
    pub current: Option<usize>,
    pub show: Option<usize>,
    pub sqref: Option<String>,
}

impl PartialEq for ScenarioSettings {
    fn eq(&self, other: &Self) -> bool {
        self.current == other.current && self.show == other.show && self.sqref == other.sqref
    }
}

impl ScenarioSettings {
    pub(crate) fn has_source(&self) -> bool {
        self.original.is_some()
    }

    pub fn removed(&mut self, position: usize, remaining: usize) {
        if remaining == 0 {
            *self = Self::default();
            return;
        }
        for index in [&mut self.current, &mut self.show] {
            *index = index.and_then(|i| {
                if i > remaining {
                    Some(i) // Preserve an unrelated, out-of-range legacy index.
                } else if i == position {
                    None
                } else {
                    Some(i - usize::from(i > position))
                }
            });
        }
    }
}

/// Parse all A1/range rectangles listed in `ScenarioSettings::sqref`.
pub(crate) fn sqref_rects(sqref: &str) -> Vec<(u32, u32, u32, u32)> {
    let mut out = Vec::new();
    for area in sqref.split_whitespace() {
        let (left, right) = area.split_once(':').unwrap_or((area, area));
        if let (Some((r1, c1)), Some((r2, c2))) = (parse_cell(left), parse_cell(right)) {
            out.push((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)));
        }
    }
    out
}

/// Transform all scenario cell coordinates and `scenario_settings.sqref` under `transform`
/// (supporting whole-axis shift, multi-insert, and lossless axis permutation).
pub(crate) fn transform_references(sheet: &mut Sheet, transform: &crate::transform::CoordTransform) {
    if transform.is_noop() || (sheet.scenarios.is_empty() && sheet.scenario_settings.sqref.is_none()) {
        return;
    }
    let rows = transform.axis() == crate::refs::Axis::Row;
    let map = |(r, c)| transform.map_cell(r, c);
    let mut removed = Vec::new();
    for (i, sc) in sheet.scenarios.iter_mut().enumerate() {
        if !sc.cells.iter().any(|(k, _)| map(*k) != Some(*k)) {
            continue;
        }
        sc.cells.retain_mut(|(key, _)| {
            if let Some(k) = map(*key) {
                *key = k;
                true
            } else {
                false
            }
        });
        sc.input_properties = std::mem::take(&mut sc.input_properties)
            .into_iter()
            .filter_map(|(k, v)| Some((map(k)?, v)))
            .collect();
        sc.source_inputs = std::mem::take(&mut sc.source_inputs)
            .into_iter()
            .filter_map(|(k, v)| Some((map(k)?, v)))
            .collect();
        sheet.scenarios_dirty = true;
        if sc.cells.is_empty() {
            removed.push(i);
        }
    }
    for i in removed.into_iter().rev() {
        sheet.scenarios.remove(i);
        sheet.scenario_settings.removed(i, sheet.scenarios.len());
    }
    if let Some(refs) = &sheet.scenario_settings.sqref {
        let mut changed = false;
        let mut refs_out = Vec::new();
        let format_cell = |pattern: &str, (r, c)| {
            format!(
                "{}{}{}{}",
                if pattern.starts_with('$') { "$" } else { "" },
                crate::model::col_label(c),
                if pattern.trim_start_matches('$').contains('$') {
                    "$"
                } else {
                    ""
                },
                r + 1
            )
        };
        for area in refs.split_whitespace() {
            let (left, right) = area.split_once(':').unwrap_or((area, area));
            let (Some((r1, c1)), Some((r2, c2))) = (parse_cell(left), parse_cell(right)) else {
                refs_out.push(area.to_owned());
                continue;
            };
            let (a, b) = if rows { (r1, r2) } else { (c1, c2) };
            let pieces: Vec<(u32, u32)> = match *transform {
                crate::transform::CoordTransform::AxisPermute { from, n, at, .. } => {
                    crate::transform::permute_axis_range_split(a, b, from, n, at)
                }
                _ => transform.map_range(a, b).into_iter().collect(),
            };
            if pieces.len() == 1 && pieces[0] == (a.min(b), a.max(b)) {
                refs_out.push(area.to_owned());
                continue;
            }
            changed = true;
            for (pa, pb) in pieces {
                let (ka, kb) = if rows {
                    ((pa, c1), (pb, c2))
                } else {
                    ((r1, pa), (r2, pb))
                };
                if area.contains(':') || ka != kb {
                    refs_out.push(format!("{}:{}", format_cell(left, ka), format_cell(right, kb)));
                } else {
                    refs_out.push(format_cell(left, ka));
                }
            }
        }
        if changed {
            sheet.scenario_settings.sqref = (!refs_out.is_empty()).then(|| refs_out.join(" "));
            sheet.scenarios_dirty = true;
        }
    }
}

/// Shift the same sheet coordinates as the grid. An input removed by a structural edit cannot
/// subsequently overwrite the cell that has moved into its former address.
pub(crate) fn shift_references(sheet: &mut Sheet, shift: &crate::refs::MappedShift) {
    if sheet.scenarios.is_empty() && sheet.scenario_settings.sqref.is_none() {
        return;
    }
    let rows = shift.axis == crate::refs::Axis::Row;
    let limit = if rows {
        crate::sync::MAX_ROWS
    } else {
        crate::sync::MAX_COLS
    };
    let map = |(r, c)| {
        let value = shift.map(if rows { r } else { c })?;
        (value < limit).then_some(if rows { (value, c) } else { (r, value) })
    };
    let mut removed = Vec::new();
    for (i, sc) in sheet.scenarios.iter_mut().enumerate() {
        if !sc.cells.iter().any(|(k, _)| map(*k) != Some(*k)) {
            continue;
        }
        sc.cells.retain_mut(|(key, _)| {
            if let Some(k) = map(*key) {
                *key = k;
                true
            } else {
                false
            }
        });
        sc.input_properties = std::mem::take(&mut sc.input_properties)
            .into_iter()
            .filter_map(|(k, v)| Some((map(k)?, v)))
            .collect();
        sc.source_inputs = std::mem::take(&mut sc.source_inputs)
            .into_iter()
            .filter_map(|(k, v)| Some((map(k)?, v)))
            .collect();
        sheet.scenarios_dirty = true;
        if sc.cells.is_empty() {
            removed.push(i);
        }
    }
    for i in removed.into_iter().rev() {
        sheet.scenarios.remove(i);
        sheet.scenario_settings.removed(i, sheet.scenarios.len());
    }
    if let Some(refs) = &sheet.scenario_settings.sqref {
        let mut changed = false;
        let mut refs_out = Vec::new();
        for area in refs.split_whitespace() {
            let (left, right) = area.split_once(':').unwrap_or((area, area));
            let (Some((r1, c1)), Some((r2, c2))) = (parse_cell(left), parse_cell(right)) else {
                refs_out.push(area.to_owned());
                continue;
            };
            let (a, b) = if rows { (r1, r2) } else { (c1, c2) };
            let moved = shift
                .map_range(a, b)
                .filter(|(a, _)| *a < limit)
                .map(|(a, b)| (a, b.min(limit - 1)));
            if moved == Some((a, b)) {
                refs_out.push(area.to_owned());
                continue;
            }
            changed = true;
            let Some((a, b)) = moved else {
                continue;
            };
            let (ka, kb) = if rows {
                ((a, c1), (b, c2))
            } else {
                ((r1, a), (r2, b))
            };
            let format = |pattern: &str, (r, c)| {
                format!(
                    "{}{}{}{}",
                    if pattern.starts_with('$') { "$" } else { "" },
                    crate::model::col_label(c),
                    if pattern.trim_start_matches('$').contains('$') {
                        "$"
                    } else {
                        ""
                    },
                    r + 1
                )
            };
            let mut ref_text = format(left, ka);
            if area.contains(':') {
                ref_text.push(':');
                ref_text.push_str(&format(right, kb));
            }
            refs_out.push(ref_text);
        }
        if changed {
            sheet.scenario_settings.sqref = (!refs_out.is_empty()).then(|| refs_out.join(" "));
            sheet.scenarios_dirty = true;
        }
    }
}

const MAX_CHANGING_CELLS: usize = 32;

/// 把单元格当前值抓成方案里存的文本（Excel 的 `val` 属性都是字符串）
pub fn value_text(v: &Value) -> String {
    match v {
        Value::Empty => String::new(),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        Value::Text(t) => t.to_string(),
        Value::Error(e) => e.text().to_string(),
        Value::Bool(b) => {
            if *b {
                "TRUE".into()
            } else {
                "FALSE".into()
            }
        }
    }
}

/// 方案里存的文本 → 单元格值（数字优先，其次 TRUE/FALSE，其余当文本）
pub fn apply_value(text: &str) -> Value {
    let t = text.trim();
    if t.is_empty() {
        return Value::Empty;
    }
    if let Ok(n) = t.parse::<f64>() {
        return Value::Number(n);
    }
    match t.to_uppercase().as_str() {
        "TRUE" => Value::Bool(true),
        "FALSE" => Value::Bool(false),
        _ => Value::Text(text.into()),
    }
}

/// 从选中的格子里抓一套方案（对可变单元格数设安全上限，避免误在全选状态添加方案导致 OOM）
pub fn capture(
    sheet: &Sheet,
    name: &str,
    comment: &str,
    rect: (u32, u32, u32, u32),
) -> anyhow::Result<Scenario> {
    let (r1, c1, r2, c2) = rect;
    anyhow::ensure!(
        r2 < crate::sync::MAX_ROWS && c2 < crate::sync::MAX_COLS && r1 <= r2 && c1 <= c2,
        "可变单元格地址无效"
    );
    let count = u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1);
    anyhow::ensure!(
        count <= MAX_CHANGING_CELLS as u64,
        "每个方案最多可以使用 32 个可变单元格，当前选中了 {count} 个"
    );
    let mut sc = Scenario::new(name);
    sc.comment = comment.to_owned();
    for r in r1..=r2 {
        for c in c1..=c2 {
            let v = sheet.value_ref(r, c);
            sc.cells.push(((r, c), value_text(v.as_ref())));
        }
    }
    Ok(sc)
}

/// "B3" / "$B$3" → (2, 1)
fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let t = s.trim().replace('$', "");
    let letters: String = t.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits: String = t.chars().skip(letters.len()).collect();
    if letters.is_empty() || digits.is_empty() {
        return None;
    }
    let row = digits.parse::<u32>().ok()?.checked_sub(1)?;
    let mut col = 0u32;
    for ch in letters.to_uppercase().chars() {
        col = col
            .checked_mul(26)?
            .checked_add(ch as u32 - 'A' as u32 + 1)?;
    }
    if col == 0 || col > crate::sync::MAX_COLS || row >= crate::sync::MAX_ROWS {
        return None;
    }
    Some((row, col - 1))
}

/// Parse the worksheet child only. Vendor extensions can contain similarly named elements.
pub fn parse(xml: &str) -> Vec<Scenario> {
    parse_with_settings(xml).1
}

pub(crate) fn parse_with_settings(xml: &str) -> (ScenarioSettings, Vec<Scenario>) {
    if !xml.contains("scenario") {
        return Default::default();
    }
    let Ok(slot) = worksheet_slot(xml) else {
        return Default::default();
    };
    let Some(block) = slot.old else {
        return Default::default();
    };
    let wrapper = format!(
        "{}{}</{}worksheet>",
        &xml[slot.root_open], &xml[block], slot.prefix
    );
    let Ok(doc) = roxmltree::Document::parse(&wrapper) else {
        return Default::default();
    };
    let root = doc.root_element().first_element_child().unwrap();
    let mut settings = read_settings(root);
    let scenarios = elements(root, "scenario")
        .enumerate()
        .filter_map(|(i, node)| read_scenario(i, node))
        .collect();
    settings.original = Some(wrapper.into());
    (settings, scenarios)
}

fn elements<'a, 'b>(
    node: roxmltree::Node<'a, 'b>,
    name: &'static str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'b>> {
    node.children().filter(move |n| {
        n.is_element()
            && n.tag_name().namespace() == node.tag_name().namespace()
            && n.tag_name().name() == name
    })
}

fn read_settings(node: roxmltree::Node) -> ScenarioSettings {
    ScenarioSettings {
        current: node.attribute("current").and_then(|x| x.parse().ok()),
        show: node.attribute("show").and_then(|x| x.parse().ok()),
        sqref: node.attribute("sqref").map(str::to_owned),
        original: None,
    }
}

fn read_scenario(index: usize, node: roxmltree::Node) -> Option<Scenario> {
    let mut sc = Scenario::new(node.attribute("name")?);
    sc.source_record = Some(index);
    sc.comment = node.attribute("comment").unwrap_or_default().into();
    sc.user = node.attribute("user").map(str::to_owned);
    sc.explicit_count = node.has_attribute("count");
    sc.locked = matches!(node.attribute("locked"), Some("1" | "true"));
    sc.hidden = matches!(node.attribute("hidden"), Some("1" | "true"));
    for (pos, input) in elements(node, "inputCells").enumerate() {
        let Some(k) = input.attribute("r").and_then(parse_cell) else {
            continue;
        };
        let props: Vec<_> = ["numFmtId", "deleted", "undone"]
            .iter()
            .filter_map(|&name| input.attribute(name).map(|v| (name.into(), v.into())))
            .collect();
        if !props.is_empty() {
            sc.input_properties.insert(k, props);
        }
        sc.source_inputs.entry(k).or_default().push(pos);
        sc.cells
            .push((k, input.attribute("val").unwrap_or_default().into()));
    }
    Some(sc)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 写出 `<scenarios>`（空列表表示删掉整段）
pub fn set_scenarios(xml: &str, list: &[Scenario]) -> String {
    edit_scenarios(xml, list, &parse_with_settings(xml).0).unwrap_or_else(|_| xml.to_owned())
}

struct WorksheetSlot {
    root_open: Range<usize>,
    old: Option<Range<usize>>,
    prefix: String,
    insert: Option<usize>,
}

fn worksheet_slot(xml: &str) -> anyhow::Result<WorksheetSlot> {
    use anyhow::{Context, ensure};
    use quick_xml::{NsReader, events::Event, name::ResolveResult};

    // Only worksheet children determine the slot. In particular sheetData may
    // be empty, and extensions may contain unrelated elements named scenarios.
    let mut reader = NsReader::from_str(xml);
    let mut root = None;
    let mut root_open = 0..0;
    let mut old = None;
    let mut insert = None;
    loop {
        let start = reader.buffer_position() as usize;
        let (ns, event) = reader.read_resolved_event()?;
        let uri = match ns {
            ResolveResult::Bound(ns) => ns.0,
            ResolveResult::Unbound => "",
            ResolveResult::Unknown(_) => anyhow::bail!("工作表 XML 使用未声明的命名空间"),
        };
        match event {
            Event::Start(tag) if root.is_none() => {
                let name = tag.name();
                let prefix = name
                    .as_ref()
                    .strip_suffix("worksheet")
                    .context("缺少 worksheet 根元素")?;
                ensure!(
                    tag.local_name().as_ref() == "worksheet",
                    "无效的工作表根元素"
                );
                root = Some((uri.to_owned(), prefix.to_owned()));
                root_open = start..reader.buffer_position() as usize;
            }
            event @ (Event::Start(_) | Event::Empty(_)) if root.is_some() => {
                let empty = matches!(event, Event::Empty(_));
                let (Event::Start(tag) | Event::Empty(tag)) = event else {
                    unreachable!()
                };
                let worksheet_ns = root.as_ref().unwrap().0 == uri;
                let local = tag.local_name();
                if !empty {
                    reader.read_to_end(tag.name())?;
                }
                let end = reader.buffer_position() as usize;
                if worksheet_ns {
                    match local.as_ref() {
                        "sheetData" | "sheetCalcPr" | "sheetProtection" | "protectedRanges" => {
                            insert = Some(end)
                        }
                        "scenarios" => {
                            ensure!(old.is_none(), "工作表存在重复的 scenarios");
                            old = Some(start..end);
                        }
                        _ => {}
                    }
                }
            }
            Event::End(_) if root.is_some() => break,
            Event::Eof => anyhow::bail!("工作表 XML 不完整"),
            _ => {}
        }
    }
    let (_, prefix) = root.context("缺少工作表根元素")?;
    Ok(WorksheetSlot {
        root_open,
        old,
        prefix,
        insert,
    })
}
pub(crate) fn edit_scenarios(
    xml: &str,
    list: &[Scenario],
    settings: &ScenarioSettings,
) -> anyhow::Result<String> {
    use anyhow::Context;
    let slot = worksheet_slot(xml)?;
    let mut insert = slot.insert.context("缺少工作表数据")?;
    let old = slot.old;
    let block = if let Some(original) = &settings.original {
        let doc = roxmltree::Document::parse(original)?;
        let root = doc
            .root_element()
            .first_element_child()
            .context("缺少来源方案")?;
        let destination = format!("{}</{}worksheet>", &xml[slot.root_open], slot.prefix);
        let dest = roxmltree::Document::parse(&destination)?;
        let block = render_existing(original, root, dest.root_element(), list, settings)?;
        match (
            root.tag_name().namespace(),
            dest.root_element().tag_name().namespace(),
        ) {
            (Some(previous), Some(next)) if previous != next => {
                rebind_namespace(&block, previous, next)?
            }
            _ => block,
        }
    } else {
        render_new(&slot.prefix, list, settings)?
    };
    let mut out = String::with_capacity(xml.len() + block.len());
    out.push_str(xml);
    if let Some(range) = old {
        if range.end <= insert {
            insert -= range.len();
        }
        out.replace_range(range, "");
    }
    out.insert_str(insert, &block);
    Ok(out)
}

// A fresh package can use Transitional worksheet namespaces for a Strict source workbook.
// Change only namespace declarations; cell text or extension values may contain the same URI.
fn rebind_namespace(xml: &str, previous: &str, next: &str) -> anyhow::Result<String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut updated = String::new();
    let mut copied = 0;
    loop {
        match reader.read_event()? {
            Event::Start(tag) | Event::Empty(tag) => {
                for attr in tag.attributes() {
                    let attr = attr?;
                    let key = attr.key.as_ref();
                    if (key == "xmlns" || key.starts_with("xmlns:"))
                        && quick_xml::escape::unescape(&attr.value)? == previous
                    {
                        // The slice reader borrows the unescaped source bytes for this attribute.
                        let raw = attr.value.as_ref();
                        let start = (raw.as_ptr() as usize).checked_sub(xml.as_ptr() as usize);
                        let start = start
                            .filter(|&i| {
                                i >= copied && xml.get(i..i.saturating_add(raw.len())) == Some(raw)
                            })
                            .ok_or_else(|| anyhow::anyhow!("无法定位方案的 XML 命名空间"))?;
                        updated.push_str(&xml[copied..start]);
                        updated.push_str(&esc(next));
                        copied = start + raw.len();
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    updated.push_str(&xml[copied..]);
    Ok(updated)
}

fn validate(sc: &Scenario) -> anyhow::Result<()> {
    anyhow::ensure!(
        (1..=MAX_CHANGING_CELLS).contains(&sc.cells.len()),
        "方案「{}」必须有 1–32 个可变单元格（当前 {} 个）",
        sc.name,
        sc.cells.len()
    );
    anyhow::ensure!(
        sc.cells
            .iter()
            .all(|((r, c), _)| *r < crate::sync::MAX_ROWS && *c < crate::sync::MAX_COLS),
        "方案「{}」的可变单元格越界",
        sc.name
    );
    Ok(())
}

fn new_input(prefix: &str, k: (u32, u32), value: &str, sc: &Scenario) -> String {
    let mut xml = format!(
        "<{prefix}inputCells r=\"{}\" val=\"{}\"",
        crate::model::cell_name(k.0, k.1),
        esc(value)
    );
    for (key, val) in sc.input_properties.get(&k).into_iter().flatten() {
        xml.push_str(&format!(" {key}=\"{}\"", esc(val)));
    }
    xml.push_str("/>");
    xml
}

fn new_record(prefix: &str, sc: &Scenario) -> anyhow::Result<String> {
    validate(sc)?;
    let mut xml = format!(
        "<{prefix}scenario name=\"{}\" locked=\"{}\" hidden=\"{}\"",
        esc(&sc.name),
        u8::from(sc.locked),
        u8::from(sc.hidden)
    );
    if let Some(v) = &sc.user {
        xml.push_str(&format!(" user=\"{}\"", esc(v)));
    }
    if sc.explicit_count {
        xml.push_str(&format!(" count=\"{}\"", sc.cells.len()));
    }
    if !sc.comment.is_empty() {
        xml.push_str(&format!(" comment=\"{}\"", esc(&sc.comment)));
    }
    xml.push('>');
    for (key, val) in &sc.cells {
        xml.push_str(&new_input(prefix, *key, val, sc));
    }
    xml.push_str(&format!("</{prefix}scenario>"));
    Ok(xml)
}

fn render_new(prefix: &str, list: &[Scenario], s: &ScenarioSettings) -> anyhow::Result<String> {
    if list.is_empty() {
        return Ok(String::new());
    }
    let mut xml = format!("<{prefix}scenarios");
    for (name, index) in [("current", s.current), ("show", s.show)] {
        if let Some(i) = index {
            anyhow::ensure!(i < list.len(), "方案 {name} 的索引 {i} 已超出方案列表");
            xml.push_str(&format!(" {name}=\"{i}\""));
        }
    }
    if let Some(refs) = &s.sqref {
        xml.push_str(&format!(" sqref=\"{}\"", esc(refs)));
    }
    xml.push('>');
    for sc in list {
        xml.push_str(&new_record(prefix, sc)?);
    }
    xml.push_str(&format!("</{prefix}scenarios>"));
    Ok(xml)
}

// Patch a single element in its source document. Unchanged attributes, comments, and vendor children
// keep their original spelling. Ranges come from the XML parser, including the ranges of child nodes.
fn patch_element(
    source: &str,
    node: roxmltree::Node,
    attrs: Vec<(String, Option<String>)>,
    mut edits: Vec<(Range<usize>, String)>,
    append: &str,
) -> String {
    let span = node.range();
    let raw = &source[span.clone()];
    let qname = raw[1..]
        .split([' ', '\t', '\r', '\n', '/', '>'])
        .next()
        .unwrap();
    let mut extra = String::new();
    for (name, value) in attrs {
        if let Some(attr) = node
            .attributes()
            .find(|a| a.namespace().is_none() && a.name() == name)
        {
            match value {
                Some(v) if v != attr.value() => edits.push((attr.range_value(), esc(&v))),
                None => edits.push((attr.range(), String::new())),
                _ => {}
            }
        } else if let Some(v) = value {
            extra.push_str(&format!(" {name}=\"{}\"", esc(&v)));
        }
    }
    if !extra.is_empty() {
        let p = span.start + 1 + qname.len();
        edits.push((p..p, extra));
    }
    if !append.is_empty() {
        if raw.ends_with("/>") {
            edits.push((span.end - 2..span.end, format!(">{append}</{qname}>")));
        } else {
            let p = span.start + raw.rfind("</").unwrap();
            edits.push((p..p, append.into()));
        }
    }
    edits.sort_by_key(|(r, _)| (r.start, r.end));
    let mut at = span.start;
    let mut out = String::new();
    for (r, v) in edits {
        out.push_str(&source[at..r.start]);
        out.push_str(&v);
        at = r.end;
    }
    out.push_str(&source[at..span.end]);
    out
}

fn element_prefix<'a>(source: &'a str, node: roxmltree::Node) -> &'a str {
    let name = source[node.range().start + 1..]
        .split([' ', '\t', '\r', '\n', '/', '>'])
        .next()
        .unwrap();
    name.strip_suffix(node.tag_name().name()).unwrap()
}

fn replace_children<'a, 'b>(
    slots: impl Iterator<Item = roxmltree::Node<'a, 'b>>,
    values: Vec<String>,
) -> (Vec<(Range<usize>, String)>, String)
where
    'b: 'a,
{
    let mut iter = values.into_iter();
    let edits = slots
        .map(|node| (node.range(), iter.next().unwrap_or_default()))
        .collect();
    (edits, iter.collect())
}

fn existing_record(
    source: &str,
    node: roxmltree::Node,
    index: usize,
    sc: &Scenario,
) -> anyhow::Result<String> {
    let Some(original) = read_scenario(index, node) else {
        anyhow::bail!("无法读取原方案");
    };
    if sc == &original {
        return Ok(source[node.range()].into());
    }
    let inputs: Vec<_> = elements(node, "inputCells").collect();
    // Excel caps interactive edits at 32 inputs. An axis change can move an imported
    // larger scenario without changing its values. Keep that legacy record usable.
    let moved_legacy = sc.cells.len() > MAX_CHANGING_CELLS
        && original.cells.len() >= sc.cells.len()
        && sc.name == original.name
        && sc.comment == original.comment
        && sc.hidden == original.hidden
        && sc.locked == original.locked
        && sc.cells.iter().all(|(k, v)| {
            sc.source_inputs.get(k).is_some_and(|indices| {
                indices.len() == 1
                    && inputs
                        .get(indices[0])
                        .is_some_and(|input| input.attribute("val").unwrap_or_default() == v)
            })
        })
        && sc
            .cells
            .iter()
            .all(|((r, c), _)| *r < crate::sync::MAX_ROWS && *c < crate::sync::MAX_COLS);
    if !moved_legacy {
        validate(sc)?;
    }
    let mut attrs = Vec::new();
    for (name, after, before) in [
        ("name", &sc.name, &original.name),
        ("comment", &sc.comment, &original.comment),
    ] {
        if after != before {
            attrs.push((name.into(), Some(after.clone())));
        }
    }
    for (name, after, before) in [
        ("hidden", sc.hidden, original.hidden),
        ("locked", sc.locked, original.locked),
    ] {
        if after != before {
            attrs.push((name.into(), Some(u8::from(after).to_string())));
        }
    }
    if sc.explicit_count
        && node
            .attribute("count")
            .and_then(|n| n.parse::<usize>().ok())
            != Some(sc.cells.len())
    {
        attrs.push(("count".into(), Some(sc.cells.len().to_string())));
    }
    let prefix = element_prefix(source, node);
    let mut values = Vec::with_capacity(sc.cells.len());
    let mut occurrences = HashMap::<_, usize>::new();
    for (key, val) in &sc.cells {
        let occurrence = occurrences.entry(key).or_default();
        let source_input = sc
            .source_inputs
            .get(key)
            .and_then(|indices| indices.get(*occurrence))
            .and_then(|&idx| inputs.get(idx));
        *occurrence += 1;
        if let Some(&input) = source_input {
            let mut updates = Vec::new();
            if input.attribute("r").and_then(parse_cell) != Some(*key) {
                updates.push(("r".into(), Some(crate::model::cell_name(key.0, key.1))));
            }
            if input.attribute("val").unwrap_or_default() != val {
                updates.push(("val".into(), Some(val.clone())));
            }
            values.push(patch_element(source, input, updates, Vec::new(), ""));
        } else {
            values.push(new_input(prefix, *key, val, sc));
        }
    }
    let (edits, append) = replace_children(inputs.into_iter(), values);
    Ok(patch_element(source, node, attrs, edits, &append))
}

fn render_existing(
    source: &str,
    node: roxmltree::Node,
    dest: roxmltree::Node,
    list: &[Scenario],
    settings: &ScenarioSettings,
) -> anyhow::Result<String> {
    let records: Vec<_> = elements(node, "scenario").collect();
    if list.is_empty() && records.iter().any(|n| n.has_attribute("name")) {
        return Ok(String::new());
    }
    let mut attrs = Vec::new();
    let mut reader = quick_xml::Reader::from_str(&source[node.range()]);
    let first = reader.read_event()?;
    let (quick_xml::events::Event::Start(tag) | quick_xml::events::Event::Empty(tag)) = first
    else {
        anyhow::bail!("方案 XML 缺少开始元素")
    };
    let local_declarations: std::collections::HashSet<_> = tag
        .attributes()
        .flatten()
        .map(|a| a.key.as_ref().to_owned())
        .collect();
    for ns in node.parent_element().unwrap().namespaces() {
        let prefix = ns.name();
        if node.lookup_namespace_uri(prefix) == Some(ns.uri())
            && dest.lookup_namespace_uri(prefix) != Some(ns.uri())
        {
            let name = prefix.map_or_else(|| "xmlns".into(), |p| format!("xmlns:{p}"));
            // A local declaration already travels with this fragment.
            if !local_declarations.contains(&name) {
                attrs.push((name, Some(ns.uri().into())));
            }
        }
    }
    let before = read_settings(node);
    for (name, new, old) in [
        ("current", settings.current, before.current),
        ("show", settings.show, before.show),
    ] {
        if new != old {
            anyhow::ensure!(
                new.is_none_or(|i| i < list.len()),
                "方案 {name} 的索引已超出方案列表"
            );
            attrs.push((name.into(), new.map(|i| i.to_string())));
        }
    }
    if settings.sqref != before.sqref {
        attrs.push(("sqref".into(), settings.sqref.clone()));
    }
    let prefix = element_prefix(source, node);
    let mut values = Vec::with_capacity(list.len());
    for sc in list {
        let record = if let Some((i, n)) = sc
            .source_record
            .and_then(|i| records.get(i).copied().map(|n| (i, n)))
        {
            existing_record(source, n, i, sc)?
        } else {
            new_record(prefix, sc)?
        };
        values.push(record);
    }
    let (edits, append) = replace_children(
        records.into_iter().filter(|n| n.has_attribute("name")),
        values,
    );
    Ok(patch_element(source, node, attrs, edits, &append))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Sheet};

    #[test]
    fn value_text_formats_like_excel() {
        assert_eq!(value_text(&Value::Number(3.0)), "3");
        assert_eq!(value_text(&Value::Number(2.5)), "2.5");
        assert_eq!(value_text(&Value::Text("x".into())), "x");
        assert_eq!(value_text(&Value::Bool(true)), "TRUE");
        assert_eq!(value_text(&Value::Empty), "");
    }

    #[test]
    fn capture_reads_selection() {
        let mut s = Sheet::new("S");
        s.set(
            0,
            0,
            Cell {
                value: Value::Number(10.0),
                formula: None,
            },
        );
        s.set(
            1,
            0,
            Cell {
                value: Value::Text("abc".into()),
                formula: None,
            },
        );
        let sc = capture(&s, "方案1", "说明", (0, 0, 1, 0)).unwrap();
        assert_eq!(sc.name, "方案1");
        assert_eq!(sc.cells.len(), 2);
        assert_eq!(sc.cells[0].1, "10");
        assert_eq!(sc.cells[1].1, "abc");
    }

    #[test]
    fn scenario_input_limit_is_checked_before_capture_and_atomic_replace() {
        let sheet = Sheet::new("S");
        assert!(capture(&sheet, "ok", "", (0, 0, 31, 0)).is_ok());
        assert!(capture(&sheet, "too many", "", (0, 0, 32, 0)).is_err());
        assert!(capture(&sheet, "empty", "", (3, 0, 2, 0)).is_err());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("scenario.xlsx");
        let mut wb = crate::Workbook::new_blank();
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let baseline = std::fs::read(&path).unwrap();
        for count in [0u32, 33] {
            let mut scenario = Scenario::new("bad");
            scenario.cells = (0..count).map(|r| ((r, 0), "1".into())).collect();
            wb.sheets[0].scenarios = vec![scenario];
            wb.sheets[0].scenarios_dirty = true;
            assert!(crate::xlsx::save_atomic(&path, &wb).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), baseline);
        }
    }

    #[test]
    fn utf16_scenario_edit_preserves_selection_summary_and_protection() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("scenario.xlsx");
        crate::xlsx::save_atomic(&path, &crate::Workbook::new_blank()).unwrap();
        let part = "xl/worksheets/sheet1.xml";
        let sheet = r#"<?xml version="1.0" encoding="UTF-16" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/><scenarios current="1" show="0" sqref="G4 G6:G8"><scenario name="a" locked="0" user="a&amp;b" count="1"><inputCells r="A1" val="1" numFmtId="14" deleted="0" undone="1"/></scenario><scenario name="b" locked="1"><inputCells r="A1" val="2"/></scenario></scenarios></worksheet>"#;
        let bytes: Vec<_> = [0xff, 0xfe]
            .into_iter()
            .chain(sheet.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        crate::fixups::write_parts(&path, &[(part.into(), bytes)]).unwrap();
        let mut wb = crate::xlsx::load(&path).unwrap();
        assert_eq!(wb.sheets[0].scenario_settings.current, Some(1));
        wb.sheets[0].scenarios[0].cells[0].1 = "7".into();
        wb.sheets[0].scenarios_dirty = true;
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let saved = String::from_utf8(crate::fixups::read_part_file(&path, part).unwrap()).unwrap();
        assert!(saved.contains(r#"encoding="UTF-8""#));
        for attribute in [
            r#"user="a&amp;b""#,
            r#"count="1""#,
            r#"numFmtId="14""#,
            r#"deleted="0""#,
            r#"undone="1""#,
        ] {
            assert!(saved.contains(attribute), "{attribute}");
        }
        let s = &crate::xlsx::load(&path).unwrap().sheets[0];
        assert_eq!(
            s.scenario_settings,
            ScenarioSettings {
                current: Some(1),
                show: Some(0),
                sqref: Some("G4 G6:G8".into()),
                ..Default::default()
            }
        );
        assert!(!s.scenarios[0].locked);
        assert!(s.scenarios[1].locked);
        assert_eq!(s.scenarios[0].cells[0].1, "7");
    }

    #[test]
    fn scenarios_follow_protection_and_leave_extension_markup_intact() {
        let xml = r#"<x:worksheet xmlns:x="urn:sheet" xmlns:f="urn:other"><x:sheetData/><x:sheetProtection/><x:protectedRanges/><x:pageMargins/><x:scenarios/><x:extLst><f:scenarios/></x:extLst></x:worksheet>"#;
        let mut scenario = Scenario::new("Base");
        scenario.cells.push(((0, 0), "7".into()));
        let patched = edit_scenarios(xml, &[scenario], &Default::default()).unwrap();
        let doc = roxmltree::Document::parse(&patched).unwrap();
        let names: Vec<_> = doc
            .root_element()
            .children()
            .filter(|n| n.is_element())
            .map(|n| n.tag_name().name())
            .collect();
        assert_eq!(
            names,
            [
                "sheetData",
                "sheetProtection",
                "protectedRanges",
                "scenarios",
                "pageMargins",
                "extLst"
            ]
        );
        assert!(patched.contains("<f:scenarios/>"));
        assert!(doc.descendants().any(
            |n| n.has_tag_name(("urn:sheet", "inputCells")) && n.attribute("val") == Some("7")
        ));
    }

    #[test]
    fn legacy_scenarios_survive_fresh_and_incremental_saves_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("legacy.xlsx");
        let saved = temp.path().join("copy.xlsx");
        crate::xlsx::save_atomic(&path, &crate::Workbook::new_blank()).unwrap();
        let part = "xl/worksheets/sheet1.xml";
        let many = (1..=33)
            .map(|r| format!("<inputCells r='A{r}' val='1'/>"))
            .collect::<String>();
        let block = format!(
            "<scenarios current='99' host:version='1' xmlns:host='urn:vendor'><scenario name='empty' host:type='unknown'/><!--gap--><scenario name='many'>{many}</scenario><scenario name='ordinary' host:type='x'><inputCells r='B3' val='a' host:id='15'/><host:ext/></scenario></scenarios>"
        );
        let worksheet = format!(
            "<worksheet xmlns='http://schemas.openxmlformats.org/spreadsheetml/2006/main'><sheetData/>{block}</worksheet>"
        );
        crate::fixups::write_parts(&path, &[(part.into(), worksheet.into_bytes())]).unwrap();
        let mut wb = crate::xlsx::load(&path).unwrap();
        assert_eq!(wb.sheets[0].scenarios[1].cells.len(), 33);
        // A new package has no donor worksheet XML. Opaque metadata must also survive here.
        wb.path = None;
        crate::xlsx::save_atomic(&saved, &wb).unwrap();
        let xml = String::from_utf8(crate::fixups::read_part_file(&saved, part).unwrap()).unwrap();
        assert!(xml.contains(&block), "{xml}");
        let mut wb = crate::xlsx::load(&saved).unwrap();
        wb.sheets[0].scenarios[2].cells[0].1 = "x'&".into();
        wb.sheets[0].scenarios_dirty = true;
        crate::xlsx::save_atomic(&saved, &wb).unwrap();
        let changed = block.replace("val='a'", "val='x&apos;&amp;'");
        let xml = String::from_utf8(crate::fixups::read_part_file(&saved, part).unwrap()).unwrap();
        assert!(xml.contains(&changed), "{xml}");
        wb.sheets[0].scenarios[1].comment = "genuine edit".into();
        assert!(crate::xlsx::save_atomic(&saved, &wb).is_err());
        assert_eq!(
            crate::xlsx::load(&saved).unwrap().sheets[0].scenarios[2].cells[0].1,
            "x'&"
        );
    }

    #[test]
    fn scenario_references_shift_with_axes_and_keep_input_xml_on_the_moved_cell() {
        let xml = r#"<x:worksheet xmlns:x="urn:s" xmlns:e="urn:extension"><x:sheetData/><x:scenarios current="1" show="0" sqref="$B$2:$B$4 D8 XFD1"><x:scenario name="gone"><x:inputCells r="B2" val="x"/></x:scenario><x:scenario name="follow"><x:inputCells r="B4" val="y" e:id="42"/><x:inputCells r="XFD1" val="z"/></x:scenario></x:scenarios></x:worksheet>"#;
        let mut sheet = Sheet::new("S");
        (sheet.scenario_settings, sheet.scenarios) = parse_with_settings(xml);
        sheet.delete_rows(1, 1);
        sheet.insert_cols(0, 1);
        assert_eq!(sheet.scenario_settings.current, Some(0));
        assert_eq!(sheet.scenario_settings.show, None);
        assert_eq!(
            sheet.scenario_settings.sqref.as_deref(),
            Some("$C$2:$C$3 E7")
        );
        assert_eq!(sheet.scenarios[0].cells, vec![((2, 2), "y".into())]);
        let written = edit_scenarios(
            "<worksheet xmlns='urn:s'><sheetData/></worksheet>",
            &sheet.scenarios,
            &sheet.scenario_settings,
        )
        .unwrap();
        let doc = roxmltree::Document::parse(&written).unwrap();
        let input = doc
            .descendants()
            .find(|n| n.has_tag_name(("urn:s", "inputCells")))
            .unwrap();
        assert_eq!(input.attribute(("urn:extension", "id")), Some("42"));
        assert_eq!(input.attribute("r"), Some("C3"));
        let converted = edit_scenarios(
            "<worksheet xmlns='urn:other-version'><sheetData/></worksheet>",
            &sheet.scenarios,
            &sheet.scenario_settings,
        )
        .unwrap();
        let doc = roxmltree::Document::parse(&converted).unwrap();
        let input = doc
            .descendants()
            .find(|n| n.has_tag_name(("urn:other-version", "inputCells")))
            .unwrap();
        assert_eq!(input.attribute(("urn:extension", "id")), Some("42"));
        sheet.scenarios_dirty = false;
        sheet.insert_rows(40, 1);
        assert!(!sheet.scenarios_dirty);
    }

    #[test]
    fn roundtrip_xml() {
        let mut sc = Scenario::new("好的 & 坏的");
        sc.comment = "备注".into();
        sc.cells.push(((0, 0), "1".into()));
        sc.cells.push(((2, 1), "文本".into()));
        let xml = "<worksheet><sheetData/><mergeCells/></worksheet>";
        let out = set_scenarios(xml, &[sc.clone()]);
        assert!(out.contains("<scenarios>"));
        assert!(out.find("<scenarios>").unwrap() < out.find("<mergeCells").unwrap());
        // 再读回来
        let back = parse(&out);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "好的 & 坏的");
        assert_eq!(back[0].comment, "备注");
        assert_eq!(back[0].cells.len(), 2);
        assert_eq!(back[0].cells[1].1, "文本");
        // 清空
        let empty = set_scenarios(&out, &[]);
        assert!(!empty.contains("<scenarios>"));
    }
}
