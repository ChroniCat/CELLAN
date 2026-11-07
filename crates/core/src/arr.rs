//! 数组公式（CSE）的读写：`<f t="array" ref="A1:A3">`。
//!
//! 动态数组的溢出值按 Excel 的规矩不落盘（只存锚点，打开时重算），
//! 但老式 CSE 数组公式必须把 `t="array"` 和 `ref` 原样写回去，
//! 否则 Excel 会把它当成普通公式，只显示左上角一个值。

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct ArrayFml {
    /// 数组公式覆盖的区域 (r1, c1, r2, c2)
    pub range: (u32, u32, u32, u32),
    /// 锚点（左上角）
    pub anchor: (u32, u32),
}

fn col_num(s: &str) -> Option<u32> {
    let mut n: u32 = 0;
    let mut any = false;
    for ch in s.chars() {
        let d = ch.to_ascii_uppercase();
        if !d.is_ascii_uppercase() {
            break;
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

/// "B2:D5" → (1,1,4,3)
pub fn parse_a1_range(s: &str) -> Option<(u32, u32, u32, u32)> {
    let s = s.trim();
    let (a, b) = match s.split_once(':') {
        Some((a, b)) => (a, b),
        None => (s, s),
    };
    let parse = |t: &str| -> Option<(u32, u32)> {
        let t: String = t.chars().filter(|c| *c != '$').collect();
        let letters: String = t.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        let digits: String = t
            .chars()
            .skip(letters.len())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if digits.is_empty() || letters.is_empty() {
            return None;
        }
        let col = col_num(&letters)?;
        let row: u32 = digits.parse().ok()?;
        Some((row.saturating_sub(1), col))
    };
    let (r1, c1) = parse(a)?;
    let (r2, c2) = parse(b)?;
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

pub fn a1_range(r: (u32, u32, u32, u32)) -> String {
    format!(
        "{}{}:{}{}",
        col_label(r.1),
        r.0 + 1,
        col_label(r.3),
        r.2 + 1
    )
}

fn col_label(mut c: u32) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (c % 26) as u8) as char);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s
}

/// 从 `<c r="B2" ...>` 取坐标
fn cell_ref_of(tag: &str) -> Option<(u32, u32)> {
    let i = tag.find(" r=\"")? + 4;
    let rest = &tag[i..];
    let end = rest.find('"')?;
    parse_a1_range(&rest[..end]).map(|(r1, c1, _, _)| (r1, c1))
}

/// 扫一张 sheet xml，找出所有数组公式
pub fn scan_xml(xml: &str) -> Vec<ArrayFml> {
    let mut out = Vec::new();
    let bytes = xml.as_bytes();
    let mut i = 0usize;
    while let Some(pos) = xml[i..].find("<c ") {
        let start = i + pos;
        let Some(end_rel) = xml[start..].find("</c>").or_else(|| xml[start..].find("/>")) else {
            break;
        };
        let end = start + end_rel + 3;
        let chunk = &xml[start..end.min(bytes.len())];
        if let Some(fpos) = chunk.find("<f") {
            let f = &chunk[fpos..];
            let fend = f.find('>').map(|p| &f[..=p]).unwrap_or(f);
            if fend.contains("t=\"array\"") {
                if let Some(rp) = fend.find("ref=\"") {
                    let rest = &fend[rp + 5..];
                    if let Some(q) = rest.find('"') {
                        if let Some(range) = parse_a1_range(&rest[..q]) {
                            if let Some(anchor) = cell_ref_of(chunk) {
                                out.push(ArrayFml { range, anchor });
                            }
                        }
                    }
                }
            }
        }
        i = end;
    }
    out
}

/// 读整本文件的数组公式：表名 → 数组列表
pub fn scan(path: &Path) -> HashMap<String, Vec<ArrayFml>> {
    let mut out = HashMap::new();
    let Ok(targets) = crate::patch::sheet_targets(path) else {
        return out;
    };
    let Ok(file) = std::fs::File::open(path) else {
        return out;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return out;
    };
    for (name, part) in targets.iter() {
        if let Ok(mut f) = zip.by_name(part) {
            let mut xml = String::new();
            if f.read_to_string(&mut xml).is_ok() {
                let list = scan_xml(&xml);
                if !list.is_empty() {
                    out.insert(name.clone(), list);
                }
            }
        }
    }
    out
}

/// 给锚点单元格的 `<f>` 补上 `t="array" ref="..."`
pub fn apply_xml(xml: &str, arrays: &[ArrayFml]) -> String {
    let mut out = xml.to_string();
    for a in arrays {
        let key = format!(" r=\"{}\"", a1_range((a.anchor.0, a.anchor.1, a.anchor.0, a.anchor.1)).split(':').next().unwrap_or("A1"));
        let Some(cpos) = out.find(&key) else { continue };
        // 往回找到这个单元格的 <c（注意 cpos 落在 " r=" 的空格上，不能用 "<c " 匹配）
        let cstart = out[..cpos].rfind("<c").unwrap_or(cpos);
        let Some(cend_rel) = out[cstart..].find("</c>") else { continue };
        let cend = cstart + cend_rel;
        let chunk = &out[cstart..cend];
        let Some(fpos) = chunk.find("<f") else { continue };
        let f_at = cstart + fpos;
        let Some(fend_rel) = out[f_at..].find('>') else { continue };
        let f_tag = &out[f_at..f_at + fend_rel];
        if f_tag.contains("t=\"array\"") {
            continue;
        }
        let insert = format!(" t=\"array\" ref=\"{}\"", a1_range(a.range));
        out.replace_range(f_at + 2..f_at + 2, &insert);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ranges() {
        assert_eq!(parse_a1_range("B2:D5"), Some((1, 1, 4, 3)));
        assert_eq!(parse_a1_range("A1"), Some((0, 0, 0, 0)));
        assert_eq!(a1_range((1, 1, 4, 3)), "B2:D5");
    }

    #[test]
    fn finds_array_formula_in_xml() {
        let xml = r#"<worksheet><sheetData><row r="2"><c r="B2" s="0"><f t="array" ref="B2:D2">SUM(A1:A3*C1:C3)</f><v>10</v></c><c r="E2"><f>1+1</f></c></row></sheetData></worksheet>"#;
        let list = scan_xml(xml);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].anchor, (1, 1));
        assert_eq!(list[0].range, (1, 1, 1, 3));
    }

    #[test]
    fn adds_attribute_to_anchor() {
        let xml = r#"<row r="2"><c r="B2"><f>SUM(A1:A3)</f></c></row>"#;
        let out = apply_xml(
            xml,
            &[ArrayFml {
                range: (1, 1, 1, 3),
                anchor: (1, 1),
            }],
        );
        assert!(out.contains(r#"<f t="array" ref="B2:D2">"#), "{out}");
    }

    #[test]
    fn keeps_existing_array_flag() {
        let xml = r#"<c r="B2"><f t="array" ref="B2:B2">1</f></c>"#;
        let out = apply_xml(
            xml,
            &[ArrayFml {
                range: (1, 1, 1, 1),
                anchor: (1, 1),
            }],
        );
        assert_eq!(out.matches("t=\"array\"").count(), 1);
    }
}
