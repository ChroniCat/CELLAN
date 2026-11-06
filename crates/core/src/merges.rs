//! 合并单元格读取：直接扫 xl/worksheets/*.xml 里的 <mergeCells>。
//! calamine 不暴露合并区域，所以这里单独解析一次（只读 workbook.xml 与 rels 定位表名）。

use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// 返回 表名 → [(r1, c1, r2, c2)]
pub fn load_merges(path: &Path) -> Result<HashMap<String, Vec<(u32, u32, u32, u32)>>> {
    let file = std::fs::File::open(path).with_context(|| format!("打开 {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("不是有效的 xlsx（zip 打不开）")?;

    // 1) workbook.xml：表名与 r:id 的顺序
    let mut order: Vec<(String, String)> = Vec::new(); // (name, r:id)
    if let Ok(mut f) = zip.by_name("xl/workbook.xml") {
        let mut xml = String::new();
        let _ = f.read_to_string(&mut xml);
        let mut r = Reader::from_str(&xml);
        loop {
            match r.read_event() {
                Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("sheet") => {
                    let mut name = String::new();
                    let mut rid = String::new();
                    for a in e.attributes().flatten() {
                        let key = a.key.as_ref();
                        let val = a.value.to_string();
                        if key.ends_with("name") {
                            name = val;
                        } else if key == "r:id" || key.ends_with(":id") {
                            rid = val;
                        }
                    }
                    if !name.is_empty() {
                        order.push((name, rid));
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
        }
    }

    // 2) rels：r:id → 目标文件
    let mut targets: HashMap<String, String> = HashMap::new();
    if let Ok(mut f) = zip.by_name("xl/_rels/workbook.xml.rels") {
        let mut xml = String::new();
        let _ = f.read_to_string(&mut xml);
        let mut r = Reader::from_str(&xml);
        loop {
            match r.read_event() {
                Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("Relationship") => {
                    let mut id = String::new();
                    let mut target = String::new();
                    for a in e.attributes().flatten() {
                        let key = a.key.as_ref();
                        let val = a.value.to_string();
                        if key == "Id" {
                            id = val;
                        } else if key == "Target" {
                            target = val;
                        }
                    }
                    if !id.is_empty() && !target.is_empty() {
                        targets.insert(id, target);
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
        }
    }

    let mut out: HashMap<String, Vec<(u32, u32, u32, u32)>> = HashMap::new();
    for (idx, (name, rid)) in order.iter().enumerate() {
        let target = targets
            .get(rid)
            .cloned()
            .unwrap_or_else(|| format!("worksheets/sheet{}.xml", idx + 1));
        let entry = if target.starts_with('/') {
            target.trim_start_matches('/').to_string()
        } else {
            format!("xl/{target}")
        };
        let Ok(mut f) = zip.by_name(&entry) else { continue };
        let mut xml = String::new();
        let _ = f.read_to_string(&mut xml);
        let merges = parse_merges(&xml);
        if !merges.is_empty() {
            out.insert(name.clone(), merges);
        }
    }
    Ok(out)
}

fn parse_merges(xml: &str) -> Vec<(u32, u32, u32, u32)> {
    let mut out = Vec::new();
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("mergeCell") => {
                for a in e.attributes().flatten() {
                    if a.key.as_ref().ends_with("ref") {
                        let v = a.value.to_string();
                        if let Some(rect) = parse_range(&v) {
                            out.push(rect);
                        }
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

/// "A1:B2" → (r1, c1, r2, c2)，0-based
fn parse_range(s: &str) -> Option<(u32, u32, u32, u32)> {
    let (a, b) = s.split_once(':')?;
    let (r1, c1) = parse_cell(a)?;
    let (r2, c2) = parse_cell(b)?;
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let cleaned: String = s.trim().chars().filter(|c| *c != '$').collect();
    let s = cleaned.as_str();
    let mut col = 0u32;
    let mut row = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() {
            col = col * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
        } else if ch.is_ascii_digit() {
            row.push(ch);
        } else {
            return None;
        }
    }
    if col == 0 || row.is_empty() {
        return None;
    }
    let r: u32 = row.parse().ok()?;
    Some((r.saturating_sub(1), col - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_merge_refs() {
        assert_eq!(parse_range("A1:B2"), Some((0, 0, 1, 1)));
        assert_eq!(parse_range("C3:C10"), Some((2, 2, 9, 2)));
        assert_eq!(parse_range("$AA$1:$AB$5"), Some((0, 26, 4, 27)));
        assert_eq!(parse_range("bad"), None);
    }

    #[test]
    fn parses_merge_xml() {
        let xml = r#"<worksheet><mergeCells count="2"><mergeCell ref="A1:B2"/><mergeCell ref="D5:F9"/></mergeCells></worksheet>"#;
        let m = parse_merges(xml);
        assert_eq!(m, vec![(0, 0, 1, 1), (4, 3, 8, 5)]);
    }
}
