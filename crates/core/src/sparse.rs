//! 稀疏工作表的旁路读取。
//!
//! calamine 的 `worksheet_range` 会按声明区域分配稠密网格：一旦有人在
//! A1048576 / XFD1048576 这种角落留了格子（很常见：整列刷格式），
//! 声明区域就是 1048576×16384，直接要 512 GiB。
//!
//! 这条通道直接流式扫 sheet XML，只处理真实存在的 `<c>`，内存与格子数成正比。

use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::model::{Cell, Value};

/// 声明区域超过这个格数就走稀疏通道
pub const DENSE_LIMIT: u64 = 2_000_000;

fn col_of(letters: &str) -> Option<u32> {
    let mut n: u32 = 0;
    let mut any = false;
    for ch in letters.chars() {
        let d = ch.to_ascii_uppercase();
        if !d.is_ascii_uppercase() {
            return None;
        }
        n = n * 26 + (d as u32 - 'A' as u32 + 1);
        any = true;
    }
    if any {
        Some(n - 1)
    } else {
        None
    }
}

/// "B12" → (11, 1)
pub fn parse_ref(s: &str) -> Option<(u32, u32)> {
    let letters: String = s.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits: String = s.chars().skip(letters.len()).collect();
    if letters.is_empty() || digits.is_empty() {
        return None;
    }
    let col = col_of(&letters)?;
    let row: u32 = digits.parse().ok()?;
    Some((row.saturating_sub(1), col))
}


fn tag_start(e: &quick_xml::events::BytesStart<'_>) -> String {
    e.name().as_ref().to_string()
}

fn tag_end(e: &quick_xml::events::BytesEnd<'_>) -> String {
    e.name().as_ref().to_string()
}

/// 直接从 zip 里读某个部件的声明区域（只读开头一小段）
pub fn declared_area_of(path: &Path, part: &str) -> u64 {
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return 0;
    };
    let Ok(mut f) = zip.by_name(part) else {
        return 0;
    };
    let mut buf = vec![0u8; 4096];
    use std::io::Read;
    let n = f.read(&mut buf).unwrap_or(0);
    declared_area(&String::from_utf8_lossy(&buf[..n]))
}

/// 声明区域格数（拿不到维度就返回 0）
pub fn declared_area(xml_head: &str) -> u64 {
    let Some(pos) = xml_head.find("<dimension ref=\"") else {
        return 0;
    };
    let rest = &xml_head[pos + 16..];
    let Some(q) = rest.find(char::from(34)) else { return 0 };
    let r = &rest[..q];
    let (a, b) = match r.split_once(':') {
        Some((a, b)) => (a, b),
        None => (r, r),
    };
    let (Some((r1, c1)), Some((r2, c2))) = (parse_ref(a), parse_ref(b)) else {
        return 0;
    };
    (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1)
}

/// 读共享字符串表
pub fn shared_strings(path: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(file) = std::fs::File::open(path) else {
        return out;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return out;
    };
    let Ok(mut f) = zip.by_name("xl/sharedStrings.xml") else {
        return out;
    };
    let mut xml = String::new();
    if f.read_to_string(&mut xml).is_err() {
        return out;
    }
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);
    let mut in_si = false;
    let mut cur = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if tag_start(&e) == "si" {
                    in_si = true;
                    cur.clear();
                }
            }
            Ok(Event::End(e)) => {
                if tag_end(&e) == "si" {
                    out.push(std::mem::take(&mut cur));
                    in_si = false;
                }
            }
            Ok(Event::Text(t)) => {
                if in_si {
                    cur.push_str(t.as_ref());
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

/// 流式读一张表：返回 (行, 列, 单元格)
pub fn read_sheet(path: &Path, part: &str, ss: &[String]) -> Result<HashMap<(u32, u32), Cell>> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut f = zip.by_name(part)?;
    let mut xml = String::new();
    f.read_to_string(&mut xml)?;

    let mut out: HashMap<(u32, u32), Cell> = HashMap::new();
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);

    let mut row: u32 = 0;
    let mut cell_ref: Option<(u32, u32)> = None;
    let mut cell_type = String::new();
    let mut in_v = false;
    let mut in_t = false;
    let mut in_is = false;
    let mut val = String::new();
    let mut formula: Option<String> = None;

    macro_rules! flush {
        () => {
            if let Some((r, c)) = cell_ref.take() {
                let text = std::mem::take(&mut val);
                let value = match cell_type.as_str() {
                    "s" => {
                        let idx: usize = text.trim().parse().unwrap_or(usize::MAX);
                        match ss.get(idx) {
                            Some(s) if !s.is_empty() => Value::Text(s.clone().into()),
                            _ => Value::Empty,
                        }
                    }
                    "b" => Value::Bool(text.trim() == "1" || text.trim().eq_ignore_ascii_case("true")),
                    "str" | "inlineStr" | "e" => {
                        if text.is_empty() {
                            Value::Empty
                        } else {
                            Value::Text(text.clone().into())
                        }
                    }
                    _ => {
                        if text.trim().is_empty() {
                            Value::Empty
                        } else {
                            match text.trim().parse::<f64>() {
                                Ok(n) => Value::Number(n),
                                Err(_) => Value::Text(text.clone().into()),
                            }
                        }
                    }
                };
                let f = formula.take();
                if !matches!(value, Value::Empty) || f.is_some() {
                    out.insert((r, c), Cell { value, formula: f.map(|s| s.into()) });
                }
            } else {
                val.clear();
                formula = None;
            }
        };
    }

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match tag_start(&e).as_str() {
                "row" => {
                    if let Some(a) = e.attributes().flatten().find(|a| a.key.as_ref() == "r") {
                        row = a.value.as_ref()
                            .parse::<u32>()
                            .map(|v| v.saturating_sub(1))
                            .unwrap_or(row);
                    }
                }
                "c" => {
                    cell_type.clear();
                    val.clear();
                    formula = None;
                    let mut r_attr: Option<(u32, u32)> = None;
                    for a in e.attributes().flatten() {
                        match a.key.as_ref() {
                            "r" => {
                                r_attr = parse_ref(a.value.as_ref());
                            }
                            "t" => cell_type = a.value.as_ref().to_string(),
                            _ => {}
                        }
                    }
                    cell_ref = r_attr.or(Some((row, 0)));
                }
                "v" => in_v = true,
                "f" => {
                    in_t = false;
                    // 公式文本在紧随的 Text 事件里
                    formula = Some(String::new());
                }
                "is" => in_is = true,
                "t" if in_is => in_t = true,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                let s = t.as_ref().to_string();
                if in_v {
                    val.push_str(&s);
                } else if in_t {
                    val.push_str(&s);
                } else if formula.is_some() {
                    if let Some(f) = formula.as_mut() {
                        f.push_str(&s);
                    }
                }
            }
            Ok(Event::End(e)) => match tag_end(&e).as_str() {
                "v" => in_v = false,
                "t" => in_t = false,
                "is" => in_is = false,
                "f" => {
                    if let Some(f) = formula.as_mut() {
                        let t = f.trim();
                        if !t.is_empty() {
                            *f = format!("={t}");
                        }
                    }
                }
                "c" => flush!(),
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow::anyhow!("解析 {part} 失败: {e}")),
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cell_refs() {
        assert_eq!(parse_ref("A1"), Some((0, 0)));
        assert_eq!(parse_ref("B12"), Some((11, 1)));
        assert_eq!(parse_ref("XFD1048576"), Some((1048575, 16383)));
        assert_eq!(parse_ref("12"), None);
    }

    #[test]
    fn computes_declared_area() {
        let head = r#"<worksheet><dimension ref="A1:XFD1048576"/><sheetData>"#;
        assert_eq!(declared_area(head), 1048576 * 16384);
        let small = r#"<worksheet><dimension ref="A1:H20001"/>"#;
        assert_eq!(declared_area(small), 20001 * 8);
        assert_eq!(declared_area("<worksheet>"), 0);
    }
}
