//! 合并单元格读取：直接扫 xl/worksheets/*.xml 里的 <mergeCells>。
//! calamine 不暴露合并区域，所以这里单独解析一次（只读 workbook.xml 与 rels 定位表名）。

use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// 一张表除单元格之外的版式信息
#[derive(Default, Clone, Debug)]
pub struct SheetLayout {
    pub merges: Vec<(u32, u32, u32, u32)>,
    /// 列号（0-based）→ 像素宽
    pub col_widths: Vec<(u32, f32)>,
    /// 行号（0-based）→ 像素高
    pub row_heights: Vec<(u32, f32)>,
    /// 条件格式
    pub cf: Vec<crate::cf::CfBlock>,
    /// 差异格式表
    pub dxfs: Vec<crate::cf::Dxf>,
    /// 数据验证
    pub dv: Vec<crate::dv::DataValidation>,
}

/// 返回 表名 → 版式
pub fn load_layout(path: &Path) -> Result<HashMap<String, SheetLayout>> {
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
                        let val = crate::model::xml_unescape(&a.value.to_string());
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
                        let val = crate::model::xml_unescape(&a.value.to_string());
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

    // dxf 格式表（styles.xml，全局一份，挂到每张表上）
    let mut dxfs: Vec<crate::cf::Dxf> = Vec::new();
    if let Ok(mut f) = zip.by_name("xl/styles.xml") {
        let mut xml = String::new();
        let _ = f.read_to_string(&mut xml);
        dxfs = crate::cf::parse_dxfs(&xml);
    }

    let mut out: HashMap<String, SheetLayout> = HashMap::new();
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
        let mut lay = parse_layout(&xml);
        lay.cf = crate::cf::parse_cf_blocks(&xml);
        lay.dxfs = dxfs.clone();
        lay.dv = crate::dv::parse_validations(&xml);
        if !lay.merges.is_empty() || !lay.col_widths.is_empty() || !lay.row_heights.is_empty() || !lay.cf.is_empty() || !lay.dv.is_empty() {
            out.insert(name.clone(), lay);
        }
    }
    Ok(out)
}

/// 一次扫描拿到合并区域、列宽、行高
fn parse_layout(xml: &str) -> SheetLayout {
    let mut out = SheetLayout::default();
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                let name = e.name();
                let name = name.as_ref();
                if name.ends_with("mergeCell") {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("ref") {
                            if let Some(rect) = parse_range(&a.value) {
                                out.merges.push(rect);
                            }
                        }
                    }
                } else if name.ends_with("col") {
                    // <col min="1" max="3" width="16.7" customWidth="1"/>
                    let mut min = 0u32;
                    let mut max = 0u32;
                    let mut width = 0f32;
                    let mut custom = false;
                    for a in e.attributes().flatten() {
                        let key = a.key.as_ref();
                        let val = crate::model::xml_unescape(&a.value.to_string());
                        match key {
                            k if k.ends_with("min") => min = val.parse().unwrap_or(0),
                            k if k.ends_with("max") => max = val.parse().unwrap_or(0),
                            k if k.ends_with("width") => width = val.parse().unwrap_or(0.0),
                            k if k.ends_with("customWidth") => custom = val == "1" || val == "true",
                            _ => {}
                        }
                    }
                    // Excel 默认列宽 8.43 字符；没写 customWidth 又是默认宽的，不必记
                    let is_default = (width - 8.43).abs() < 0.05;
                    if min >= 1 && max >= min && width > 0.0 && (custom || !is_default) {
                        let px = crate::layout::excel_width_to_px(width);
                        if (px - crate::layout::DEFAULT_COL_W).abs() > 0.5 {
                            for c in (min - 1)..max {
                                out.col_widths.push((c, px));
                            }
                        }
                    }
                } else if name.ends_with("row") {
                    // <row r="1" ht="30" customHeight="1">
                    let mut idx = 0u32;
                    let mut ht = 0f32;
                    let mut custom = false;
                    for a in e.attributes().flatten() {
                        let key = a.key.as_ref();
                        let val = crate::model::xml_unescape(&a.value.to_string());
                        match key {
                            "r" => idx = val.parse().unwrap_or(0),
                            "ht" => ht = val.parse().unwrap_or(0.0),
                            "customHeight" => custom = val == "1" || val == "true",
                            _ => {}
                        }
                    }
                    if idx >= 1 && ht > 0.0 && custom {
                        let px = crate::layout::pt_to_px(ht);
                        if (px - crate::layout::DEFAULT_ROW_H).abs() > 0.5 {
                            out.row_heights.push((idx - 1, px));
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

fn parse_merges(xml: &str) -> Vec<(u32, u32, u32, u32)> {
    let mut out = Vec::new();
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("mergeCell") => {
                for a in e.attributes().flatten() {
                    if a.key.as_ref().ends_with("ref") {
                        let v = crate::model::xml_unescape(&a.value.to_string());
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
    fn parses_cols_and_rows() {
        let xml = r#"<worksheet><cols><col min="1" max="3" width="16.0" customWidth="1"/><col min="5" max="5" width="8.43"/></cols><sheetData><row r="2" ht="30" customHeight="1"><c r="A2" s="1"/></row></sheetData></worksheet>"#;
        let lay = parse_layout(xml);
        assert_eq!(lay.col_widths.len(), 3, "第 1-3 列应为自定义宽度");
        assert_eq!(lay.col_widths[0].0, 0);
        assert!((lay.col_widths[0].1 - 117.0).abs() < 1.0);
        assert_eq!(lay.row_heights, vec![(1, 40.0)]);
        assert!(lay.merges.is_empty());
    }

    #[test]
    fn parses_merge_xml() {
        let xml = r#"<worksheet><mergeCells count="2"><mergeCell ref="A1:B2"/><mergeCell ref="D5:F9"/></mergeCells></worksheet>"#;
        let m = parse_merges(xml);
        assert_eq!(m, vec![(0, 0, 1, 1), (4, 3, 8, 5)]);
    }
}
