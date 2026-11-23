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
}

impl Scenario {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            comment: String::new(),
            cells: Vec::new(),
            hidden: false,
        }
    }
}

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
pub fn capture(sheet: &Sheet, name: &str, comment: &str, rect: (u32, u32, u32, u32)) -> Scenario {
    let (r1, c1, r2, c2) = rect;
    let mut sc = Scenario::new(name);
    sc.comment = comment.to_string();
    const MAX_SCENARIO_CELLS: usize = 512;
    'outer: for r in r1..=r2 {
        for c in c1..=c2 {
            if sc.cells.len() >= MAX_SCENARIO_CELLS {
                break 'outer;
            }
            let v = sheet.get(r, c).map(|x| x.value.clone()).unwrap_or_default();
            sc.cells.push(((r, c), value_text(&v)));
        }
    }
    sc
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
        col = col * 26 + (ch as u32 - 'A' as u32 + 1);
    }
    if col == 0 {
        return None;
    }
    Some((row, col - 1))
}

fn attr(tag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = tag.find(&pat)? + pat.len();
    let rest = &tag[i..];
    let end = rest.find('"')?;
    Some(crate::model::xml_unescape(&rest[..end]))
}

/// 解析工作表里的 `<scenarios>`
pub fn parse(xml: &str) -> Vec<Scenario> {
    use quick_xml::events::{BytesStart, Event};
    use quick_xml::Reader;

    fn scenario_of(e: &BytesStart) -> Scenario {
        let mut sc = Scenario::new("");
        for a in e.attributes().flatten() {
            let k = a.key.as_ref();
            let v = crate::model::xml_unescape(&a.value.to_string());
            if k == "name" {
                sc.name = v;
            } else if k == "comment" {
                sc.comment = v;
            } else if k == "hidden" {
                sc.hidden = v == "1" || v == "true";
            }
        }
        sc
    }

    fn cell_of(e: &BytesStart) -> Option<((u32, u32), String)> {
        let mut addr = String::new();
        let mut val = String::new();
        for a in e.attributes().flatten() {
            let k = a.key.as_ref();
            let v = crate::model::xml_unescape(&a.value.to_string());
            if k == "r" {
                addr = v;
            } else if k == "val" {
                val = v;
            }
        }
        parse_cell(&addr).map(|p| (p, val))
    }

    let mut out = Vec::new();
    let mut cur: Option<Scenario> = None;
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let n = e.name();
                let n = n.as_ref().to_string();
                if n == "scenario" || n.ends_with(":scenario") {
                    cur = Some(scenario_of(&e));
                } else if n == "inputCells" || n.ends_with(":inputCells") {
                    if let (Some(sc), Some(item)) = (cur.as_mut(), cell_of(&e)) {
                        sc.cells.push(item);
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let n = e.name();
                let n = n.as_ref().to_string();
                if n == "scenario" || n.ends_with(":scenario") {
                    let sc = scenario_of(&e);
                    if !sc.name.is_empty() {
                        out.push(sc);
                    }
                } else if n == "inputCells" || n.ends_with(":inputCells") {
                    if let (Some(sc), Some(item)) = (cur.as_mut(), cell_of(&e)) {
                        sc.cells.push(item);
                    }
                }
            }
            Ok(Event::End(e)) => {
                let n = e.name();
                let n = n.as_ref().to_string();
                if (n == "scenario" || n.ends_with(":scenario")) && cur.is_some() {
                    let sc = cur.take().unwrap();
                    if !sc.name.is_empty() {
                        out.push(sc);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 写出 `<scenarios>`（空列表表示删掉整段）
pub fn set_scenarios(xml: &str, list: &[Scenario]) -> String {
    edit_scenarios(xml, list).unwrap_or_else(|_| xml.to_owned())
}

pub(crate) fn edit_scenarios(xml: &str, list: &[Scenario]) -> anyhow::Result<String> {
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
                let prefix = name.as_ref().strip_suffix("worksheet").context("缺少 worksheet 根元素")?;
                ensure!(tag.local_name().as_ref() == "worksheet", "无效的工作表根元素");
                root = Some((uri.to_owned(), prefix.to_owned()));
            }
            event @ (Event::Start(_) | Event::Empty(_)) if root.is_some() => {
                let empty = matches!(event, Event::Empty(_));
                let (Event::Start(tag) | Event::Empty(tag)) = event else { unreachable!() };
                let worksheet_ns = root.as_ref().unwrap().0 == uri;
                let local = tag.local_name();
                if !empty { reader.read_to_end(tag.name())?; }
                let end = reader.buffer_position() as usize;
                if worksheet_ns {
                    match local.as_ref() {
                        "sheetData" | "sheetCalcPr" | "sheetProtection" | "protectedRanges" => insert = Some(end),
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
    let mut block = if list.is_empty() { String::new() } else { format!("<{prefix}scenarios>") };
    for sc in list {
        block.push_str(&format!(
            "<{prefix}scenario name=\"{}\" locked=\"1\" hidden=\"{}\"",
            esc(&sc.name),
            if sc.hidden { 1 } else { 0 }
        ));
        if !sc.comment.is_empty() {
            block.push_str(&format!(" comment=\"{}\"", esc(&sc.comment)));
        }
        block.push('>');
        for ((r, c), v) in &sc.cells {
            block.push_str(&format!(
                "<{prefix}inputCells r=\"{}\" val=\"{}\"/>",
                crate::model::cell_name(*r, *c),
                esc(v)
            ));
        }
        block.push_str(&format!("</{prefix}scenario>"));
    }
    if !list.is_empty() { block.push_str(&format!("</{prefix}scenarios>")); }
    let mut out = String::with_capacity(xml.len() + block.len());
    out.push_str(xml);
    if let Some(range) = old {
        if range.end <= insert { insert -= range.len(); }
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
        let sc = capture(&s, "方案1", "说明", (0, 0, 1, 0));
        assert_eq!(sc.name, "方案1");
        assert_eq!(sc.cells.len(), 2);
        assert_eq!(sc.cells[0].1, "10");
        assert_eq!(sc.cells[1].1, "abc");
    }

    #[test]
    fn scenarios_follow_protection_and_leave_extension_markup_intact() {
        let xml = r#"<x:worksheet xmlns:x="urn:sheet" xmlns:f="urn:other"><x:sheetData/><x:sheetProtection/><x:protectedRanges/><x:pageMargins/><x:scenarios/><x:extLst><f:scenarios/></x:extLst></x:worksheet>"#;
        let mut scenario = Scenario::new("Base");
        scenario.cells.push(((0, 0), "7".into()));
        let patched = edit_scenarios(xml, &[scenario]).unwrap();
        let doc = roxmltree::Document::parse(&patched).unwrap();
        let names: Vec<_> = doc.root_element().children().filter(|n| n.is_element()).map(|n| n.tag_name().name()).collect();
        assert_eq!(names, ["sheetData", "sheetProtection", "protectedRanges", "scenarios", "pageMargins", "extLst"]);
        assert!(patched.contains("<f:scenarios/>"));
        assert!(doc.descendants().any(|n| n.has_tag_name(("urn:sheet", "inputCells")) && n.attribute("val") == Some("7")));
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
