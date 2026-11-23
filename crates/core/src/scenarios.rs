//! 方案管理器：一张表可以存多套「可变单元格 = 值」的组合（Excel 的方案管理器）。
//!
//! OOXML 位置：`<scenarios>` 必须排在 `<sheetData>` 之后、`<autoFilter>` / `<mergeCells>` 之前，
//! 也就是 sheetProtection / protectedRanges 这一段里。

use crate::model::{Sheet, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Scenario {
    pub name: String,
    pub comment: String,
    /// ((行, 列), 值文本)
    pub cells: Vec<((u32, u32), String)>,
    pub hidden: bool,
    pub locked: bool,
    user: Option<String>,
    explicit_count: bool,
    // Rare per-input XML properties. Ordinary scenarios allocate no entries.
    input_properties: std::collections::HashMap<(u32, u32), Vec<(String, String)>>,
}

impl Scenario {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            comment: String::new(),
            cells: Vec::new(),
            hidden: false,
            locked: true,
            user: None,
            explicit_count: false,
            input_properties: Default::default(),
        }
    }
    pub fn replace_inputs(&mut self, captured: Scenario) {
        self.comment = captured.comment;
        self.cells = captured.cells;
        self.input_properties
            .retain(|key, _| self.cells.iter().any(|(k, _)| k == key));
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScenarioSettings {
    pub current: Option<usize>,
    pub show: Option<usize>,
    pub sqref: Option<String>,
}

impl ScenarioSettings {
    pub fn removed(&mut self, position: usize, remaining: usize) {
        if remaining == 0 {
            *self = Self::default();
            return;
        }
        for index in [&mut self.current, &mut self.show] {
            *index = index.and_then(|i| {
                if i == position {
                    None
                } else {
                    Some(i - usize::from(i > position))
                }
            });
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
            let v = sheet.get(r, c).map_or(&Value::Empty, |cell| &cell.value);
            sc.cells.push(((r, c), value_text(v)));
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
    use quick_xml::{
        NsReader,
        events::{BytesStart, Event},
        name::ResolveResult,
    };
    let mut settings = ScenarioSettings::default();
    let mut scenarios = Vec::new();
    if !xml.contains("scenario") {
        return (settings, scenarios);
    }

    fn attr(e: &BytesStart, name: &str) -> Option<String> {
        e.attributes()
            .flatten()
            .find(|a| a.key.as_ref() == name)
            .and_then(|a| {
                quick_xml::escape::unescape(a.value.as_ref())
                    .ok()
                    .map(|v| v.into_owned())
            })
    }

    let mut reader = NsReader::from_str(xml);
    let mut namespace = None;
    let mut depth = 0;
    let mut in_scenarios = false;
    let mut current = None;
    while let Ok((ns, event)) = reader.read_resolved_event() {
        let (tag, closed) = match event {
            Event::Start(tag) => (tag, false),
            Event::Empty(tag) => (tag, true),
            Event::End(_) => {
                if depth == 2 {
                    in_scenarios = false;
                }
                if depth == 3 {
                    current = None;
                }
                depth -= 1;
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let uri = match ns {
            ResolveResult::Bound(n) => n.0,
            _ => "",
        };
        let local = tag.local_name();
        let child = namespace.as_deref() == Some(uri);
        match (depth, local.as_ref()) {
            (0, "worksheet") => namespace = Some(uri.to_owned()),
            (1, "scenarios") if child => {
                settings.current = attr(&tag, "current").and_then(|x| x.parse().ok());
                settings.show = attr(&tag, "show").and_then(|x| x.parse().ok());
                settings.sqref = attr(&tag, "sqref");
                in_scenarios = !closed;
            }
            (2, "scenario") if child && in_scenarios => {
                if let Some(name) = attr(&tag, "name") {
                    let mut sc = Scenario::new(name);
                    sc.locked =
                        attr(&tag, "locked").is_some_and(|x| matches!(x.as_str(), "1" | "true"));
                    sc.hidden =
                        attr(&tag, "hidden").is_some_and(|x| matches!(x.as_str(), "1" | "true"));
                    sc.comment = attr(&tag, "comment").unwrap_or_default();
                    sc.user = attr(&tag, "user");
                    sc.explicit_count = attr(&tag, "count").is_some();
                    current = (!closed).then_some(scenarios.len());
                    scenarios.push(sc);
                }
            }
            (3, "inputCells") if child && in_scenarios => {
                if let Some(i) = current
                    && let Some(k) = attr(&tag, "r").and_then(|s| parse_cell(&s))
                {
                    let properties: Vec<_> = ["numFmtId", "deleted", "undone"]
                        .into_iter()
                        .filter_map(|name| attr(&tag, name).map(|v| (name.to_owned(), v)))
                        .collect();
                    if !properties.is_empty() {
                        scenarios[i].input_properties.insert(k, properties);
                    }
                    scenarios[i]
                        .cells
                        .push((k, attr(&tag, "val").unwrap_or_default()));
                }
            }
            _ => {
                if !closed && reader.read_to_end(tag.name()).is_err() {
                    break;
                }
                continue;
            }
        }
        if !closed {
            depth += 1;
        }
    }
    (settings, scenarios)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 写出 `<scenarios>`（空列表表示删掉整段）
pub fn set_scenarios(xml: &str, list: &[Scenario]) -> String {
    edit_scenarios(xml, list, &parse_with_settings(xml).0).unwrap_or_else(|_| xml.to_owned())
}

pub(crate) fn edit_scenarios(
    xml: &str,
    list: &[Scenario],
    settings: &ScenarioSettings,
) -> anyhow::Result<String> {
    use anyhow::{Context, ensure};
    use quick_xml::{NsReader, events::Event, name::ResolveResult};

    // Only worksheet children determine the slot. In particular sheetData may
    // be empty, and extensions may contain unrelated elements named scenarios.
    let mut reader = NsReader::from_str(xml);
    let mut root = None;
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
    let mut insert = insert.context("缺少工作表数据")?;
    let (_, prefix) = root.context("缺少工作表根元素")?;
    let mut block = String::new();
    if !list.is_empty() {
        block.push_str(&format!("<{prefix}scenarios"));
        for (key, value) in [("current", settings.current), ("show", settings.show)] {
            if let Some(i) = value {
                ensure!(i < list.len(), "方案 {key} 的索引 {i} 已超出方案列表");
                block.push_str(&format!(" {key}=\"{i}\""));
            }
        }
        if let Some(references) = &settings.sqref {
            block.push_str(&format!(" sqref=\"{}\"", esc(references)));
        }
        block.push('>');
    }
    for sc in list {
        ensure!(
            (1..=MAX_CHANGING_CELLS).contains(&sc.cells.len()),
            "方案「{}」必须有 1–32 个可变单元格（当前 {} 个）",
            sc.name,
            sc.cells.len()
        );
        block.push_str(&format!(
            "<{prefix}scenario name=\"{}\" locked=\"{}\" hidden=\"{}\"",
            esc(&sc.name),
            u8::from(sc.locked),
            u8::from(sc.hidden)
        ));
        if let Some(user) = &sc.user {
            block.push_str(&format!(" user=\"{}\"", esc(user)));
        }
        if sc.explicit_count {
            block.push_str(&format!(" count=\"{}\"", sc.cells.len()));
        }
        if !sc.comment.is_empty() {
            block.push_str(&format!(" comment=\"{}\"", esc(&sc.comment)));
        }
        block.push('>');
        for ((r, c), v) in &sc.cells {
            ensure!(
                *r < crate::sync::MAX_ROWS && *c < crate::sync::MAX_COLS,
                "方案「{}」的可变单元格越界",
                sc.name
            );
            block.push_str(&format!(
                "<{prefix}inputCells r=\"{}\" val=\"{}\"",
                crate::model::cell_name(*r, *c),
                esc(v)
            ));
            if let Some(attributes) = sc.input_properties.get(&(*r, *c)) {
                for (key, value) in attributes {
                    block.push_str(&format!(" {key}=\"{}\"", esc(value)));
                }
            }
            block.push_str("/>");
        }
        block.push_str(&format!("</{prefix}scenario>"));
    }
    if !list.is_empty() {
        block.push_str(&format!("</{prefix}scenarios>"));
    }
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
                sqref: Some("G4 G6:G8".into())
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
