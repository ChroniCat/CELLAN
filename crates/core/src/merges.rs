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
    /// `<col min=1 max=16384>` 这种整表默认宽度（按列展开会变成上万条，改用默认值）
    pub col_default: Option<f32>,
    /// 行号（0-based）→ 像素高
    pub row_heights: Vec<(u32, f32)>,
    /// 隐藏的列（0-based）
    pub hidden_cols: Vec<u32>,
    /// 手动隐藏的行（0-based）
    pub hidden_rows: Vec<u32>,
    /// 分级显示设置（None 表示文件里没写，用默认值）
    pub outline_pr: Option<(bool, bool, bool)>,
    /// 方案管理器里的方案
    pub scenarios: Vec<crate::scenarios::Scenario>,
    /// 被忽略错误的单元格（0-based）
    pub ignored_errors: Vec<(u32, u32)>,
    /// 分级显示级别（0-based 行列 → 1..=7）
    pub row_levels: Vec<(u32, u8)>,
    pub col_levels: Vec<(u32, u8)>,
    /// 条件格式
    pub cf: Vec<crate::cf::CfBlock>,
    /// 差异格式表
    pub dxfs: Vec<crate::cf::Dxf>,
    /// 数据验证
    pub dv: Vec<crate::dv::DataValidation>,
    /// 冻结窗格（行, 列），None 表示没冻结
    pub freeze: Option<(u32, u32)>,
    /// 自动筛选区域 + 每列允许值
    pub filter: Option<crate::sort_filter::Filter>,
    /// 工作表标签颜色
    pub tab_color: Option<[u8; 3]>,
    /// 工作表保护
    pub protection: Option<crate::protection::SheetProtection>,
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
        lay.scenarios = crate::scenarios::parse(&xml);
        lay.cf = crate::cf::parse_cf_blocks(&xml);
        lay.dxfs = dxfs.clone();
        lay.dv = crate::dv::parse_validations(&xml);
        if !lay.merges.is_empty()
            || lay.col_default.is_some()
            || !lay.col_widths.is_empty()
            || !lay.row_heights.is_empty()
            || lay.outline_pr.is_some()
            || !lay.scenarios.is_empty()
            || !lay.ignored_errors.is_empty()
            || !lay.row_levels.is_empty()
            || !lay.col_levels.is_empty()
            || !lay.hidden_rows.is_empty()
            || !lay.hidden_cols.is_empty()
            || !lay.cf.is_empty()
            || !lay.dv.is_empty()
            || lay.freeze.is_some()
            || lay.filter.is_some()
            || lay.tab_color.is_some()
            || lay.protection.is_some()
        {
            out.insert(name.clone(), lay);
        }
    }
    Ok(out)
}

/// 一次扫描拿到合并区域、列宽、行高、冻结窗格
fn parse_layout(xml: &str) -> SheetLayout {
    let mut out = SheetLayout::default();
    let mut af_range: Option<(u32, u32, u32, u32)> = None;
    let mut cur_col: Option<u32> = None;
    let mut af_criteria: std::collections::HashMap<u32, crate::sort_filter::Criterion> =
        std::collections::HashMap::new();
    let mut af_allows: std::collections::HashMap<u32, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    let mut freeze_y = 0u32;
    let mut freeze_x = 0u32;
    let mut saw_pane = false;
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                let nm = e.name();
                let name = nm.as_ref();
                if name.ends_with("mergeCell") {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("ref") {
                            if let Some(rect) = parse_range(&crate::model::xml_unescape(&a.value.to_string())) {
                                out.merges.push(rect);
                            }
                        }
                    }
                } else if name.ends_with("sheetProtection") {
                    let mut tag = String::from("<sheetProtection");
                    for a in e.attributes().flatten() {
                        tag.push(' ');
                        tag.push_str(a.key.as_ref());
                        tag.push_str("=\"");
                        tag.push_str(&crate::model::xml_unescape(&a.value.to_string()));
                        tag.push('"');
                    }
                    tag.push_str("/>");
                    out.protection = crate::protection::parse_tag(&tag);
                } else if name.ends_with("tabColor") {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("rgb") {
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            let hex = v.trim_start_matches('#');
                            let hex = if hex.len() == 8 { &hex[2..] } else { hex };
                            if hex.len() == 6 {
                                if let (Ok(r), Ok(g), Ok(b)) = (
                                    u8::from_str_radix(&hex[0..2], 16),
                                    u8::from_str_radix(&hex[2..4], 16),
                                    u8::from_str_radix(&hex[4..6], 16),
                                ) {
                                    out.tab_color = Some([r, g, b]);
                                }
                            }
                        }
                    }
                } else if name.ends_with("outlinePr") {
                    let (mut below, mut right, mut symbols) = (true, true, true);
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        let on = v == "1" || v == "true";
                        if k == "summaryBelow" {
                            below = on;
                        } else if k == "summaryRight" {
                            right = on;
                        } else if k == "showOutlineSymbols" {
                            symbols = on;
                        }
                    }
                    out.outline_pr = Some((below, right, symbols));
                } else if name.ends_with("col") {
                    let mut min = 0u32;
                    let mut max = 0u32;
                    let mut width = 0f32;
                    let mut custom = false;
                    let mut hidden = false;
                    for a in e.attributes().flatten() {
                        let key = a.key.as_ref();
                        let val = crate::model::xml_unescape(&a.value.to_string());
                        match key {
                            k if k.ends_with("min") => min = val.parse().unwrap_or(0),
                            k if k.ends_with("max") => max = val.parse().unwrap_or(0),
                            k if k.ends_with("width") => width = val.parse().unwrap_or(0.0),
                            k if k.ends_with("customWidth") => custom = val == "1" || val == "true",
                            "hidden" => hidden = val == "1" || val == "true",
                            "outlineLevel" => {
                                let lv = val.parse::<u8>().unwrap_or(0);
                                if lv > 0 && min >= 1 && max >= min && (max - min) < 1024 {
                                    for c in (min - 1)..max {
                                        out.col_levels.push((c, lv));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if hidden && min >= 1 && max >= min && (max - min) < 1024 {
                        for c in (min - 1)..max {
                            out.hidden_cols.push(c);
                        }
                    }
                    let is_default = (width - 8.43).abs() < 0.05;
                    if min >= 1 && max >= min && width > 0.0 && (custom || !is_default) {
                        let px = crate::layout::col_width_to_px(width);
                        let span = max - min + 1;
                        if span > 1024 {
                            // 整表级别的列宽：当成默认宽度，不能按列展开
                            out.col_default = Some(px);
                        } else if (px - crate::layout::DEFAULT_COL_W).abs() > 0.5 {
                            for c in (min - 1)..max {
                                out.col_widths.push((c, px));
                            }
                        }
                    }
                } else if name == "row" || name.ends_with(":row") {
                    let mut idx = 0u32;
                    let mut ht = 0f32;
                    let mut custom = false;
                    let mut hidden = false;
                    for a in e.attributes().flatten() {
                        let key = a.key.as_ref();
                        let val = crate::model::xml_unescape(&a.value.to_string());
                        match key {
                            "r" => idx = val.parse().unwrap_or(0),
                            "ht" => ht = val.parse().unwrap_or(0.0),
                            "customHeight" => custom = val == "1" || val == "true",
                            "hidden" => hidden = val == "1" || val == "true",
                            "outlineLevel" => {
                                let lv = val.parse::<u8>().unwrap_or(0);
                                if lv > 0 && idx >= 1 {
                                    out.row_levels.push((idx - 1, lv));
                                }
                            }
                            _ => {}
                        }
                    }
                    if hidden && idx >= 1 {
                        out.hidden_rows.push(idx - 1);
                    }
                    if idx >= 1 && ht > 0.0 && custom {
                        let px = crate::layout::pt_to_px(ht);
                        if (px - crate::layout::DEFAULT_ROW_H).abs() > 0.5 {
                            out.row_heights.push((idx - 1, px));
                        }
                    }
                } else if name.ends_with("ignoredError") {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("sqref") {
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            for tok in v.split_whitespace() {
                                if let Some((r1, c1, r2, c2)) = parse_range(tok) {
                                    // 大区域只取左上角，避免整列忽略把内存撑爆
                                    out.ignored_errors.push((r1, c1));
                                    if (r2 - r1) * (c2 - c1) < 4096 {
                                        for r in r1..=r2 {
                                            for c in c1..=c2 {
                                                out.ignored_errors.push((r, c));
                                            }
                                        }
                                    }
                                } else if let Some((r, c)) = parse_cell(tok) {
                                    out.ignored_errors.push((r, c));
                                }
                            }
                        }
                    }
                } else if name.ends_with("autoFilter") {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("ref") {
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            if let Some(rect) = crate::cf::parse_sqref(&v).into_iter().next() {
                                af_range = Some(rect);
                            }
                        }
                    }
                } else if name.ends_with("filterColumn") {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("colId") {
                            cur_col = crate::model::xml_unescape(&a.value.to_string()).parse().ok();
                        }
                    }
                } else if name.ends_with("customFilter") {
                    // <customFilter operator="contains" val="张"/>；Between 在 Excel 里是两条 and="1"
                    if let (Some(col), Some(range)) = (cur_col, af_range) {
                        let mut op_s = String::new();
                        let mut val = String::new();
                        for a in e.attributes().flatten() {
                            let k = a.key.as_ref();
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            if k.ends_with("operator") {
                                op_s = v;
                            } else if k.ends_with("val") {
                                val = v;
                            }
                        }
                        let key = range.1 + col;
                        if let Some(op) = crate::sort_filter::CmpOp::from_xml(&op_s) {
                            match af_criteria.get_mut(&key) {
                                // 第二条 between 的上界
                                Some(c)
                                    if c.op == crate::sort_filter::CmpOp::Between
                                        && c.val2.is_none() =>
                                {
                                    c.val2 = Some(val);
                                }
                                Some(_) => {}
                                None => {
                                    let c = crate::sort_filter::Criterion {
                                        op,
                                        val,
                                        val2: None,
                                    };
                                    af_criteria.insert(key, c);
                                }
                            }
                        }
                    }
                } else if name == "filter" || name.ends_with(":filter") {
                    if let (Some(col), Some(range)) = (cur_col, af_range) {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with("val") {
                                let v = crate::model::xml_unescape(&a.value.to_string());
                                af_allows
                                    .entry(range.1 + col)
                                    .or_default()
                                    .insert(v);
                            }
                        }
                    }
                } else if name.ends_with("pane") {
                    let mut x = 0u32;
                    let mut y = 0u32;
                    let mut state = String::new();
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        match k {
                            "xSplit" => x = v.parse::<f32>().unwrap_or(0.0) as u32,
                            "ySplit" => y = v.parse::<f32>().unwrap_or(0.0) as u32,
                            "state" => state = v,
                            _ => {}
                        }
                    }
                    if state.starts_with("frozen") {
                        saw_pane = true;
                        freeze_x = x;
                        freeze_y = y;
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    if saw_pane && (freeze_x > 0 || freeze_y > 0) {
        out.freeze = Some((freeze_y, freeze_x));
    }
    if let Some(range) = af_range {
        let mut f = crate::sort_filter::Filter::new(range.0, range.1, range.2, range.3);
        f.allows = af_allows;
        f.criteria = af_criteria;
        out.filter = Some(f);
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
    fn parses_tab_color() {
        let xml = r#"<worksheet><sheetPr><tabColor rgb="0070AD47" /><outlinePr/><pageSetUpPr /></sheetPr><cols/><sheetData/></worksheet>"#;
        let lay = parse_layout(xml);
        assert_eq!(lay.tab_color, Some([0x70, 0xAD, 0x47]));
    }

#[test]
    fn parses_cols_and_rows() {
        let xml = r#"<worksheet><cols><col min="1" max="3" width="16.0" customWidth="1"/><col min="5" max="5" width="8.43"/></cols><sheetData><row r="2" ht="30" customHeight="1"><c r="A2" s="1"/></row></sheetData></worksheet>"#;
        let lay = parse_layout(xml);
        assert_eq!(lay.col_widths.len(), 3, "第 1-3 列应为自定义宽度");
        assert_eq!(lay.col_widths[0].0, 0);
        assert!((lay.col_widths[0].1 - 128.0).abs() < 1.0);
        assert_eq!(lay.row_heights, vec![(1, 40.0)]);
        assert!(lay.merges.is_empty());
    }

    #[test]
    fn parses_outline_levels() {
        let xml = r#"<worksheet><sheetData><row r="2" outlineLevel="1"><c r="A2"/></row><row r="3" outlineLevel="2"><c r="A3"/></row></sheetData></worksheet>"#;
        let lay = parse_layout(xml);
        assert_eq!(lay.row_levels, vec![(1, 1), (2, 2)], "行分级应被读出");
    }

    #[test]
    fn parses_merge_xml() {
        let xml = r#"<worksheet><mergeCells count="2"><mergeCell ref="A1:B2"/><mergeCell ref="D5:F9"/></mergeCells></worksheet>"#;
        let m = parse_merges(xml);
        assert_eq!(m, vec![(0, 0, 1, 1), (4, 3, 8, 5)]);
    }
}
