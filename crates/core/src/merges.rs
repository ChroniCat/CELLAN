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
    pub merges: Merges,
    /// 列号（0-based）→ 像素宽
    pub col_widths: Vec<(u32, f32)>,
    /// `<col min=1 max=16384>` 这种整表默认宽度（按列展开会变成上万条，改用默认值）
    pub col_default: Option<f32>,
    /// 行号（0-based）→ 像素高
    pub row_heights: Vec<(u32, f32)>,
    /// `<sheetFormatPr defaultRowHeight="14.25">` → 像素（Excel/WPS 的整表默认行高）
    pub row_default: Option<f32>,
    /// 隐藏的列（0-based）
    pub hidden_cols: Vec<u32>,
    /// 手动隐藏的行（0-based）
    pub hidden_rows: Vec<u32>,
    /// 分级显示设置（None 表示文件里没写，用默认值）
    pub outline_pr: Option<(bool, bool, bool)>,
    /// 方案管理器里的方案
    pub scenarios: Vec<crate::scenarios::Scenario>,
    pub scenario_settings: crate::scenarios::ScenarioSettings,
    /// 被忽略错误的单元格（0-based）
    pub ignored_errors: Vec<(u32, u32)>,
    /// 分级显示级别（0-based 行列 → 1..=7）
    pub row_levels: Vec<(u32, u8)>,
    pub col_levels: Vec<(u32, u8)>,
    /// 整列 / 整行样式索引（0-based 行列 → xl/styles.xml cellXfs 索引）
    pub col_xfs: Vec<(u32, u32)>,
    pub(crate) invalid_column_bounds: bool,
    pub row_xfs: Vec<(u32, u32)>,
    /// 条件格式
    pub cf: Vec<crate::cf::CfBlock>,
    /// 是否有未建模的 x14 扩展条件格式
    pub cf_ext: bool,
    /// 差异格式里是否有未建模的边框方向
    pub dxf_ext: bool,
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
    /// 允许编辑的区域（`<protectedRanges>`）
    pub protected_ranges: Vec<crate::protection::ProtectedRange>,
    /// 每表视图缩放百分比（`<sheetView zoomScale="..."/>`）
    pub zoom_scale: Option<u16>,
    /// 是否显示网格线（`<sheetView showGridLines="0/1"/>`）
    pub show_gridlines: Option<bool>,
    /// 是否显示行列标题（`<sheetView showRowColHeaders="0/1"/>`）
    pub show_headings: Option<bool>,
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
    let mut styles_xml = String::new();
    if let Ok(mut f) = zip.by_name("xl/styles.xml") {
        let _ = f.read_to_string(&mut styles_xml);
        dxfs = crate::cf::parse_dxfs(&styles_xml);
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
        let lay = sheet_layout(&xml, &dxfs, crate::cf::dxfs_unsupported(&styles_xml));
        if !lay.merges.is_empty()
            || lay.col_default.is_some()
            || !lay.col_widths.is_empty()
            || !lay.row_heights.is_empty()
            || lay.row_default.is_some()
            || lay.outline_pr.is_some()
            || lay.scenario_settings.has_source() || !lay.scenarios.is_empty()
            || !lay.ignored_errors.is_empty()
            || !lay.row_levels.is_empty()
            || !lay.col_levels.is_empty()
            || !lay.hidden_rows.is_empty()
            || !lay.hidden_cols.is_empty()
            || !lay.col_xfs.is_empty()
            || !lay.row_xfs.is_empty()
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

pub(crate) fn sheet_layout(xml: &str, dxfs: &[crate::cf::Dxf], dxf_ext: bool) -> SheetLayout {
    sheet_layout_with_theme(xml, dxfs, dxf_ext, &crate::style_theme::ThemeBook::default())
}

pub(crate) fn sheet_layout_with_theme(
    xml: &str,
    dxfs: &[crate::cf::Dxf],
    dxf_ext: bool,
    theme: &crate::style_theme::ThemeBook,
) -> SheetLayout {
    let mut layout = parse_layout_with_dxfs(xml, dxfs);
    (layout.scenario_settings, layout.scenarios) = crate::scenarios::parse_with_settings(xml);
    layout.cf = crate::cf::parse_cf_blocks_with_theme(xml, theme);
    layout.cf_ext = crate::cf::xml_has_cf_ext(xml);
    layout.dxfs = dxfs.to_vec();
    layout.dxf_ext = dxf_ext;
    layout.dv = crate::dv::parse_validations(xml);
    layout
}

/// 一次扫描拿到合并区域、列宽、行高、冻结窗格
#[cfg(test)]
fn parse_layout(xml: &str) -> SheetLayout {
    parse_layout_with_dxfs(xml, &[])
}

fn parse_layout_with_dxfs(xml: &str, dxfs: &[crate::cf::Dxf]) -> SheetLayout {
    let mut out = SheetLayout::default();
    let mut af_range: Option<(u32, u32, u32, u32)> = None;
    let mut cur_col: Option<u32> = None;
    let mut af_criteria: std::collections::HashMap<u32, crate::sort_filter::Criterion> =
        std::collections::HashMap::new();
    let mut af_allows: std::collections::HashMap<u32, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    let mut af_date_groups: std::collections::HashMap<u32, Vec<crate::sort_filter::DateGroupItem>> =
        std::collections::HashMap::new();
    let mut af_color_filters: std::collections::HashMap<u32, crate::sort_filter::ColorFilter> =
        std::collections::HashMap::new();
    let mut af_top_filters: std::collections::HashMap<u32, crate::sort_filter::TopFilter> =
        std::collections::HashMap::new();
    let mut af_dynamic_filters: std::collections::HashMap<u32, crate::sort_filter::DynamicFilter> =
        std::collections::HashMap::new();
    let mut cur_custom_and = false;
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
                } else if name == "protectedRange" || name.ends_with(":protectedRange") {
                    let mut tag = String::from("<protectedRange");
                    for a in e.attributes().flatten() {
                        tag.push(' ');
                        tag.push_str(a.key.as_ref());
                        tag.push_str("=\"");
                        tag.push_str(&crate::model::xml_escape(&crate::model::xml_unescape(&a.value.to_string())));
                        tag.push('"');
                    }
                    tag.push_str("/>");
                    if let Some(pr) = crate::protection::parse_protected_range_tag(&tag) {
                        out.protected_ranges.push(pr);
                    }
                } else if name == "sheetView" || name.ends_with(":sheetView") {
                    if out.zoom_scale.is_none() && out.show_gridlines.is_none() && out.show_headings.is_none() {
                        for a in e.attributes().flatten() {
                            let k = a.key.as_ref();
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            if k == "zoomScale" {
                                if let Ok(z) = v.trim().parse::<u16>() {
                                    out.zoom_scale = Some(z.clamp(10, 400));
                                }
                            } else if k == "showGridLines" {
                                out.show_gridlines = Some(v != "0" && !v.eq_ignore_ascii_case("false"));
                            } else if k == "showRowColHeaders" {
                                out.show_headings = Some(v != "0" && !v.eq_ignore_ascii_case("false"));
                            }
                        }
                    }
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
                    let mut style_xf = 0u32;
                    let mut outline_lv = 0u8;
                    for a in e.attributes().flatten() {
                        let key = a.key.as_ref();
                        let val = crate::model::xml_unescape(&a.value.to_string());
                        match key {
                            k if k.ends_with("min") => min = val.parse().unwrap_or(0),
                            k if k.ends_with("max") => max = val.parse().unwrap_or(0),
                            k if k.ends_with("width") => width = val.parse().unwrap_or(0.0),
                            k if k.ends_with("customWidth") => custom = val == "1" || val == "true",
                            "hidden" => hidden = val == "1" || val == "true",
                            "style" => style_xf = val.parse().unwrap_or(0),
                            "outlineLevel" => outline_lv = val.parse::<u8>().unwrap_or(0),
                            _ => {}
                        }
                    }
                    if min == 0 || max < min || max > crate::sync::MAX_COLS {
                        out.invalid_column_bounds = true;
                        max = max.min(crate::sync::MAX_COLS);
                    }
                    if outline_lv > 0 && min >= 1 && max >= min && (max - min) < 1024 {
                        for c in (min - 1)..max {
                            out.col_levels.push((c, outline_lv));
                        }
                    }
                    if style_xf > 0 && min >= 1 && max >= min {
                        for c in (min - 1)..max.min(16_384) {
                            out.col_xfs.push((c, style_xf));
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
                } else if name == "sheetFormatPr" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("defaultRowHeight") {
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            if let Ok(pt) = v.parse::<f32>() {
                                if pt > 0.0 {
                                    out.row_default = Some(crate::layout::pt_to_px(pt));
                                }
                            }
                        }
                    }
                } else if name == "row" || name.ends_with(":row") {
                    let mut idx = 0u32;
                    let mut ht = 0f32;
                    let mut custom = false;
                    let mut hidden = false;
                    let mut row_s = 0u32;
                    let mut row_lv = 0u8;
                    for a in e.attributes().flatten() {
                        let key = a.key.as_ref();
                        let val = crate::model::xml_unescape(&a.value.to_string());
                        match key {
                            "r" => idx = val.parse().unwrap_or(0),
                            "ht" => ht = val.parse().unwrap_or(0.0),
                            "customHeight" => custom = val == "1" || val == "true",
                            "hidden" => hidden = val == "1" || val == "true",
                            "s" => row_s = val.parse().unwrap_or(0),
                            "outlineLevel" => row_lv = val.parse::<u8>().unwrap_or(0),
                            _ => {}
                        }
                    }
                    if row_lv > 0 && idx >= 1 {
                        out.row_levels.push((idx - 1, row_lv));
                    }
                    if row_s > 0 && idx >= 1 {
                        out.row_xfs.push((idx - 1, row_s));
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
                                    let area = (r2 as u64 - r1 as u64 + 1)
                                        .saturating_mul(c2 as u64 - c1 as u64 + 1);
                                    if area <= 4096 {
                                        for r in r1..=r2 {
                                            for c in c1..=c2 {
                                                if (r, c) != (r1, c1) {
                                                    out.ignored_errors.push((r, c));
                                                }
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
                    cur_custom_and = false;
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("colId") {
                            cur_col = crate::model::xml_unescape(&a.value.to_string()).parse().ok();
                        }
                    }
                } else if name.ends_with("customFilters") {
                    cur_custom_and = false;
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("and") {
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            cur_custom_and = v == "1" || v.eq_ignore_ascii_case("true");
                        }
                    }
                } else if name.ends_with("top10") {
                    if let (Some(col), Some(range)) = (cur_col, af_range) {
                        let mut top = true;
                        let mut percent = false;
                        let mut count = 10u32;
                        for a in e.attributes().flatten() {
                            let k = a.key.as_ref();
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            if k.ends_with("top") {
                                top = v != "0" && !v.eq_ignore_ascii_case("false");
                            } else if k.ends_with("percent") {
                                percent = v == "1" || v.eq_ignore_ascii_case("true");
                            } else if k.ends_with("val") {
                                count = v.parse::<f64>().map(|x| x.round().max(1.0) as u32).unwrap_or(10);
                            }
                        }
                        if let Some(key) = range.1.checked_add(col).filter(|&k| k < 16_384) {
                            af_top_filters.insert(key, crate::sort_filter::TopFilter { top, count, percent });
                        }
                    }
                } else if name.ends_with("dynamicFilter") {
                    if let (Some(col), Some(range)) = (cur_col, af_range) {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with("type") {
                                let v = crate::model::xml_unescape(&a.value.to_string());
                                if let Some(df) = crate::sort_filter::DynamicFilter::from_ooxml(&v) {
                                    if let Some(key) = range.1.checked_add(col).filter(|&k| k < 16_384) {
                                        af_dynamic_filters.insert(key, df);
                                    }
                                }
                            }
                        }
                    }
                } else if name.ends_with("customFilter") {
                    // OOXML 标准：文本筛选使用 equal/notEqual + *通配符；Between 为 Ge + Le 两条 and="1"
                    if let (Some(col), Some(range)) = (cur_col, af_range)
                        && let Some(key) = range.1.checked_add(col).filter(|&k| k < 16_384)
                    {
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
                        let op_key = if op_s.is_empty() { "equal" } else { op_s.as_str() };
                        if let Some(raw_op) = crate::sort_filter::CmpOp::from_xml(op_key) {
                            let unesc_wild = |s: &str| -> String {
                                let mut out = String::with_capacity(s.len());
                                let mut chars = s.chars();
                                while let Some(ch) = chars.next() {
                                    if ch == '~' {
                                        if let Some(next) = chars.next() {
                                            out.push(next);
                                        }
                                    } else {
                                        out.push(ch);
                                    }
                                }
                                out
                            };
                            let trailing_unescaped_star = |s: &str| -> bool {
                                if !s.ends_with('*') {
                                    return false;
                                }
                                let tilde_run = s[..s.len() - 1]
                                    .chars()
                                    .rev()
                                    .take_while(|&c| c == '~')
                                    .count();
                                tilde_run % 2 == 0
                            };
                            let (op, val) = match raw_op {
                                crate::sort_filter::CmpOp::Eq => {
                                    let lead = val.starts_with('*');
                                    let trail = trailing_unescaped_star(&val);
                                    if lead && trail && val.len() >= 2 {
                                        (
                                            crate::sort_filter::CmpOp::Contains,
                                            unesc_wild(&val[1..val.len() - 1]),
                                        )
                                    } else if trail && val.len() >= 1 {
                                        (
                                            crate::sort_filter::CmpOp::Begins,
                                            unesc_wild(&val[..val.len() - 1]),
                                        )
                                    } else if lead && val.len() >= 1 {
                                        (
                                            crate::sort_filter::CmpOp::Ends,
                                            unesc_wild(&val[1..]),
                                        )
                                    } else {
                                        (raw_op, val)
                                    }
                                }
                                crate::sort_filter::CmpOp::Ne => {
                                    let lead = val.starts_with('*');
                                    let trail = trailing_unescaped_star(&val);
                                    if lead && trail && val.len() >= 2 {
                                        (
                                            crate::sort_filter::CmpOp::NotContains,
                                            unesc_wild(&val[1..val.len() - 1]),
                                        )
                                    } else {
                                        (raw_op, val)
                                    }
                                }
                                _ => (raw_op, val),
                            };
                            match af_criteria.get_mut(&key) {
                                // 第二条 between 的上界（标准 OOXML: greaterThanOrEqual + lessThanOrEqual）
                                Some(c)
                                    if c.op == crate::sort_filter::CmpOp::Ge
                                        && op == crate::sort_filter::CmpOp::Le
                                        && c.val2.is_none()
                                        && cur_custom_and =>
                                {
                                    c.op = crate::sort_filter::CmpOp::Between;
                                    c.val2 = Some(val);
                                }
                                Some(c)
                                    if c.op == crate::sort_filter::CmpOp::Le
                                        && op == crate::sort_filter::CmpOp::Ge
                                        && c.val2.is_none()
                                        && cur_custom_and =>
                                {
                                    let hi = std::mem::replace(&mut c.val, val);
                                    c.op = crate::sort_filter::CmpOp::Between;
                                    c.val2 = Some(hi);
                                }
                                Some(c)
                                    if c.op == crate::sort_filter::CmpOp::Between
                                        && c.val2.is_none() =>
                                {
                                    c.val2 = Some(val);
                                }
                                Some(c) if c.second.is_none() => {
                                    c.second = Some((cur_custom_and, op, val));
                                }
                                Some(_) => {}
                                None => {
                                    let c = crate::sort_filter::Criterion {
                                        op,
                                        val,
                                        val2: None,
                                        second: None,
                                    };
                                    af_criteria.insert(key, c);
                                }
                            }
                        }
                    }
                } else if name == "filter" || name.ends_with(":filter") {
                    if let (Some(col), Some(range)) = (cur_col, af_range)
                        && let Some(key) = range.1.checked_add(col).filter(|&k| k < 16_384)
                    {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with("val") {
                                let v = crate::model::xml_unescape(&a.value.to_string());
                                af_allows
                                    .entry(key)
                                    .or_default()
                                    .insert(v);
                            }
                        }
                    }
                } else if name.ends_with("dateGroupItem") {
                    if let (Some(col), Some(range)) = (cur_col, af_range)
                        && let Some(key) = range.1.checked_add(col).filter(|&k| k < 16_384)
                    {
                        let mut year: Option<i32> = None;
                        let mut month: Option<u32> = None;
                        let mut day: Option<u32> = None;
                        let mut grouping = String::new();
                        for a in e.attributes().flatten() {
                            let k = a.key.as_ref();
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            if k.ends_with("year") {
                                year = v.parse().ok();
                            } else if k.ends_with("month") {
                                month = v.parse().ok();
                            } else if k.ends_with("day") {
                                day = v.parse().ok();
                            } else if k.ends_with("dateTimeGrouping") {
                                grouping = v;
                            }
                        }
                        if let Some(y) = year {
                            let item = match grouping.as_str() {
                                "year" => crate::sort_filter::DateGroupItem { year: y, month: None, day: None },
                                "month" => crate::sort_filter::DateGroupItem { year: y, month, day: None },
                                _ => crate::sort_filter::DateGroupItem { year: y, month, day },
                            };
                            af_date_groups.entry(key).or_default().push(item);
                        }
                    }
                } else if name.ends_with("colorFilter") {
                    if let (Some(col), Some(range)) = (cur_col, af_range)
                        && let Some(key) = range.1.checked_add(col).filter(|&k| k < 16_384)
                    {
                        let mut dxf_id: Option<usize> = None;
                        let mut cell_color = true;
                        for a in e.attributes().flatten() {
                            let k = a.key.as_ref();
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            if k.ends_with("dxfId") {
                                dxf_id = v.parse().ok();
                            } else if k.ends_with("cellColor") {
                                cell_color = v != "0" && !v.eq_ignore_ascii_case("false");
                            }
                        }
                        if cell_color {
                            match dxf_id.and_then(|id| dxfs.get(id)) {
                                Some(dxf) if dxf.fill.is_some() => {
                                    af_color_filters.insert(key, crate::sort_filter::ColorFilter::Fill(dxf.fill));
                                }
                                None => {
                                    af_color_filters.insert(key, crate::sort_filter::ColorFilter::Fill(None));
                                }
                                _ => {}
                            }
                        } else if let Some(rgb) = dxf_id.and_then(|id| dxfs.get(id)).and_then(|d| d.color) {
                            af_color_filters.insert(key, crate::sort_filter::ColorFilter::Font(rgb));
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
        f.allows = af_allows.into();
        f.criteria = af_criteria;
        f.date_groups = af_date_groups;
        f.color_filters = af_color_filters;
        f.top_filters = af_top_filters;
        f.dynamic_filters = af_dynamic_filters;
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
pub(crate) fn parse_range(s: &str) -> Option<(u32, u32, u32, u32)> {
    let (a, b) = s.split_once(':')?;
    let (r1, c1) = parse_cell(a)?;
    let (r2, c2) = parse_cell(b)?;
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

pub(crate) fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let cleaned: String = s.trim().chars().filter(|c| *c != '$').collect();
    let s = cleaned.as_str();
    let mut col = 0u32;
    let mut row = 0u32;
    let mut letter_count = 0usize;
    let mut seen_digit = false;
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() && !seen_digit {
            letter_count += 1;
            if letter_count > 3 {
                return None;
            }
            col = col.checked_mul(26)?.checked_add(ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1)?;
        } else if ch.is_ascii_digit() {
            seen_digit = true;
            row = row.checked_mul(10)?.checked_add(ch as u32 - '0' as u32)?;
            if row > 1_048_576 {
                return None;
            }
        } else {
            return None;
        }
    }
    if col == 0 || col > 16_384 || row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
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
        assert!((lay.col_widths[0].1 - 133.0).abs() < 1.0); // 16 字符 × 8 + 5
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

    #[test]
    fn page_bucket_and_tall_preserve_overlapping_index_precedence() {
        // Overlapping imported merges: earlier index must win regardless of tall vs bucketed.
        let tall_first = vec![(0, 0, 100_000, 0), (10, 0, 12, 2), (2_000_000, 3, 2_000_002, 5)];
        let m1 = Merges::from(tall_first.clone());
        let mut idx1 = MergeIndex::default();
        idx1.refresh(&tall_first);
        let mut idx1_ref = MergeIndex::default();
        idx1_ref.refresh_merges(&m1);
        for (r, c, want) in [
            (10, 0, Some((0, 0, 100_000, 0))),
            (10, 1, Some((10, 0, 12, 2))),
            (50_000, 0, Some((0, 0, 100_000, 0))),
            (2_000_001, 4, Some((2_000_000, 3, 2_000_002, 5))),
            (2_000_001, 0, None),
        ] {
            assert_eq!(m1.at(r, c), want, "m1 at ({r},{c})");
            assert_eq!(idx1.at(r, c), want, "idx1 at ({r},{c})");
            assert_eq!(idx1_ref.at(r, c), want, "idx1_ref at ({r},{c})");
        }

        let short_first = vec![(10, 0, 12, 2), (0, 0, 100_000, 0)];
        let m2 = Merges::from(short_first.clone());
        let mut idx2 = MergeIndex::default();
        idx2.refresh(&short_first);
        assert_eq!(m2.at(10, 0), Some((10, 0, 12, 2)));
        assert_eq!(idx2.at(10, 0), Some((10, 0, 12, 2)));
        assert_eq!(m2.at(13, 0), Some((0, 0, 100_000, 0)));
        assert_eq!(idx2.at(13, 0), Some((0, 0, 100_000, 0)));
    }

    #[test]
    fn batch_merge_ranges_matches_sequential_semantics_including_transitive_overlaps() {
        let mut expected: Vec<MergeRect> = Vec::new();
        let mut naive_merge = |batch: &[MergeRect]| {
            for &(r1, c1, r2, c2) in batch {
                let (r1, r2) = (r1.min(r2), r1.max(r2));
                let (c1, c2) = (c1.min(c2), c1.max(c2));
                if r1 == r2 && c1 == c2 {
                    continue;
                }
                expected.retain(|&(a, b, cc, d)| a > r2 || cc < r1 || b > c2 || d < c1);
                expected.push((r1, c1, r2, c2));
            }
            expected.sort_unstable();
        };

        let base: Vec<MergeRect> = vec![
            (10, 0, 11, 1),     // killed by incoming X (which is later killed by Y)
            (0, 10, 50_000, 10), // tall existing merge
            (500, 0, 502, 2),
            (1000, 1, 1001, 3),
        ];
        let incoming: Vec<MergeRect> = vec![
            (0, 0, 1, 1),        // A: killed by B
            (1, 1, 2, 2),        // B: kills A, killed by C
            (2, 2, 3, 3),        // C: kills B, survives
            (11, 1, 12, 2),      // X: kills existing (10,0,11,1), killed by Y
            (12, 2, 13, 3),      // Y: kills X, survives
            (25_000, 9, 25_001, 11), // kills tall existing merge
            (500, 0, 502, 2),    // exact duplicate
            (500, 0, 502, 2),    // exact duplicate again
            (7, 7, 7, 7),        // single cell ignored
            (800, 5, 799, 4),    // reversed coords normalized
            (0, 20, 60_000, 20), // tall incoming
            (100, 20, 101, 21),  // kills tall incoming
        ];

        naive_merge(&base);
        naive_merge(&incoming);

        let mut actual = Merges::new();
        assert!(actual.merge_ranges(base));
        assert!(actual.merge_ranges(incoming));
        assert_eq!(actual.as_ref(), expected.as_slice());

        // Verify spatial queries match brute force on all corners and interiors.
        for &(r1, c1, r2, c2) in &expected {
            assert_eq!(actual.at(r1, c1), Some((r1, c1, r2, c2)));
            assert_eq!(actual.at(r2, c2), Some((r1, c1, r2, c2)));
        }
        assert_eq!(actual.at(0, 0), None);
        assert_eq!(actual.at(10, 0), None);
        assert_eq!(actual.at(0, 10), None);
        assert_eq!(actual.at(0, 20), None);
    }
}


pub type MergeRect = (u32, u32, u32, u32);

fn next_merge_revision() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

const MERGE_PAGE_SHIFT: u32 = 8; // 256 rows per page
const TALL_PAGE_LIMIT: u32 = 16; // spans > 16 pages go into `tall`
const MAX_DENSE_PAGES: usize = 4096; // covers rows 0..1_048_576 in O(1)

#[derive(Clone, Debug, Default)]
struct PageBucket {
    entries: Vec<(usize, MergeRect)>,
    ends: Vec<u32>,
}

impl PageBucket {
    fn finalize(&mut self) {
        if self.entries.len() > 1
            && !self
                .entries
                .windows(2)
                .all(|w| (w[0].1.0, w[0].0) <= (w[1].1.0, w[1].0))
        {
            self.entries.sort_unstable_by_key(|&(i, rect)| (rect.0, i));
        }
        self.ends.clear();
        self.ends.reserve(self.entries.len());
        let mut bottom = 0u32;
        for &(_, rect) in &self.entries {
            bottom = bottom.max(rect.2);
            self.ends.push(bottom);
        }
    }
}

#[derive(Clone, Debug, Default)]
struct MergesMeta {
    max_row: Option<u32>,
    max_col: Option<u32>,
    dense_buckets: Vec<PageBucket>,
    overflow_buckets: HashMap<u32, PageBucket>,
    tall: Vec<(usize, MergeRect)>,
}

impl MergesMeta {
    fn build(rects: &[MergeRect]) -> Self {
        if rects.is_empty() {
            return Self::default();
        }
        let mut max_r = 0u32;
        let mut max_c = 0u32;
        let mut max_dense_page: Option<usize> = None;
        for &(r1, c1, r2, c2) in rects {
            max_r = max_r.max(r2);
            max_c = max_c.max(c2);
            if r1 <= r2 && c1 <= c2 {
                let p1 = r1 >> MERGE_PAGE_SHIFT;
                let p2 = r2 >> MERGE_PAGE_SHIFT;
                if p2 - p1 < TALL_PAGE_LIMIT && (p2 as usize) < MAX_DENSE_PAGES {
                    let p2_u = p2 as usize;
                    max_dense_page = Some(max_dense_page.map_or(p2_u, |cur| cur.max(p2_u)));
                }
            }
        }

        let mut dense_buckets = match max_dense_page {
            Some(mp) => vec![PageBucket::default(); mp + 1],
            None => Vec::new(),
        };
        let mut overflow_buckets: HashMap<u32, PageBucket> = HashMap::new();
        let mut tall: Vec<(usize, MergeRect)> = Vec::new();

        for (idx, &rect) in rects.iter().enumerate() {
            let (r1, c1, r2, c2) = rect;
            if r1 > r2 || c1 > c2 {
                continue;
            }
            let p1 = r1 >> MERGE_PAGE_SHIFT;
            let p2 = r2 >> MERGE_PAGE_SHIFT;
            if p2 - p1 >= TALL_PAGE_LIMIT {
                tall.push((idx, rect));
            } else {
                for p in p1..=p2 {
                    if let Some(bucket) = dense_buckets.get_mut(p as usize) {
                        bucket.entries.push((idx, rect));
                    } else {
                        overflow_buckets.entry(p).or_default().entries.push((idx, rect));
                    }
                }
            }
        }

        for bucket in &mut dense_buckets {
            if !bucket.entries.is_empty() {
                bucket.finalize();
            }
        }
        for bucket in overflow_buckets.values_mut() {
            bucket.finalize();
        }

        Self {
            max_row: Some(max_r),
            max_col: Some(max_c),
            dense_buckets,
            overflow_buckets,
            tall,
        }
    }

    #[inline]
    fn at(&self, r: u32, c: u32) -> Option<MergeRect> {
        if r > self.max_row? || c > self.max_col? {
            return None;
        }
        let mut best: Option<(usize, MergeRect)> = None;
        // `tall` is ordered by ascending original index, so the first match is minimal in `tall`.
        for &(idx, rect) in &self.tall {
            if rect.0 <= r && r <= rect.2 && rect.1 <= c && c <= rect.3 {
                best = Some((idx, rect));
                break;
            }
        }
        let page = r >> MERGE_PAGE_SHIFT;
        let bucket = if let Some(b) = self.dense_buckets.get(page as usize) {
            (!b.entries.is_empty()).then_some(b)
        } else {
            self.overflow_buckets.get(&page)
        };
        if let Some(bucket) = bucket {
            let entries = &bucket.entries;
            let slice = if entries.len() <= 8 {
                entries.as_slice()
            } else {
                let start = bucket.ends.partition_point(|&end| end < r);
                let end = entries.partition_point(|&(_, rect)| rect.0 <= r);
                if start < end {
                    &entries[start..end]
                } else {
                    &[]
                }
            };
            for &(idx, rect) in slice {
                if let Some((best_idx, _)) = best
                    && idx >= best_idx
                {
                    continue;
                }
                if rect.0 <= r && r <= rect.2 && rect.1 <= c && c <= rect.3 {
                    best = Some((idx, rect));
                }
            }
        }
        best.map(|(_, rect)| rect)
    }
}

#[inline]
fn rects_overlap(a: MergeRect, b: MergeRect) -> bool {
    a.0 <= b.2 && a.2 >= b.0 && a.1 <= b.3 && a.3 >= b.1
}

#[inline]
fn rect_contains(outer: MergeRect, inner: MergeRect) -> bool {
    outer.0 <= inner.0 && outer.1 <= inner.1 && outer.2 >= inner.2 && outer.3 >= inner.3
}

/// Temporary fine-grained page-bucket conflict index for O(K + M) batch `merge_ranges`.
#[derive(Default)]
struct ConflictIndex {
    buckets: HashMap<u32, Vec<MergeRect>>,
    tall: Vec<MergeRect>,
}

impl ConflictIndex {
    const SHIFT: u32 = 4; // 16 rows per bucket
    const TALL_SPAN: u32 = 16; // > 256 rows goes to `tall`

    fn insert(&mut self, rect: MergeRect) {
        let p1 = rect.0 >> Self::SHIFT;
        let p2 = rect.2 >> Self::SHIFT;
        if p2 - p1 >= Self::TALL_SPAN {
            self.tall.push(rect);
        } else {
            for p in p1..=p2 {
                self.buckets.entry(p).or_default().push(rect);
            }
        }
    }

    fn overlaps_status(&self, rect: MergeRect) -> (bool, bool) {
        for &o in &self.tall {
            if rects_overlap(o, rect) {
                return (true, rect_contains(o, rect));
            }
        }
        let p1 = rect.0 >> Self::SHIFT;
        let p2 = rect.2 >> Self::SHIFT;
        if p2 - p1 < Self::TALL_SPAN {
            for p in p1..=p2 {
                if let Some(list) = self.buckets.get(&p) {
                    for &o in list {
                        if rects_overlap(o, rect) {
                            return (true, rect_contains(o, rect));
                        }
                    }
                }
            }
        } else {
            for (&p, list) in &self.buckets {
                if p < p1 || p > p2 {
                    continue;
                }
                for &o in list {
                    if rects_overlap(o, rect) {
                        return (true, rect_contains(o, rect));
                    }
                }
            }
        }
        (false, false)
    }

    #[inline]
    fn overlaps_any(&self, rect: MergeRect) -> bool {
        self.overlaps_status(rect).0
    }
}

pub(crate) fn groups_overlap(
    existing: impl Iterator<Item = MergeRect>, mut incoming: impl Iterator<Item = MergeRect>,
) -> bool {
    let mut existing = existing.peekable();
    if existing.peek().is_none() { return false; }
    let mut index = ConflictIndex::default();
    for rect in existing { index.insert(rect); }
    incoming.any(|rect| index.overlaps_any(rect))
}

#[derive(Debug)]
struct MergesInner {
    rects: Vec<MergeRect>,
    revision: u64,
    meta: std::sync::OnceLock<MergesMeta>,
}

impl Clone for MergesInner {
    fn clone(&self) -> Self {
        Self {
            rects: self.rects.clone(),
            revision: next_merge_revision(),
            meta: std::sync::OnceLock::new(),
        }
    }
}

/// Copy-on-write merge list with monotonic `revision()`, cached `max_row()` / `max_col()`,
/// and built-in 256-row page-bucketed spatial lookup (`at(r, c)`).
#[derive(Clone, Debug)]
pub struct Merges(std::sync::Arc<MergesInner>);

impl Default for Merges {
    fn default() -> Self {
        static EMPTY: std::sync::OnceLock<std::sync::Arc<MergesInner>> = std::sync::OnceLock::new();
        Self(
            EMPTY
                .get_or_init(|| {
                    std::sync::Arc::new(MergesInner {
                        rects: Vec::new(),
                        revision: 0,
                        meta: std::sync::OnceLock::from(MergesMeta::default()),
                    })
                })
                .clone(),
        )
    }
}

impl From<Vec<MergeRect>> for Merges {
    fn from(rects: Vec<MergeRect>) -> Self {
        if rects.is_empty() {
            return Self::default();
        }
        Self(std::sync::Arc::new(MergesInner {
            rects,
            revision: next_merge_revision(),
            meta: std::sync::OnceLock::new(),
        }))
    }
}

impl FromIterator<MergeRect> for Merges {
    fn from_iter<T: IntoIterator<Item = MergeRect>>(iter: T) -> Self {
        let rects: Vec<MergeRect> = iter.into_iter().collect();
        Self::from(rects)
    }
}

impl Merges {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn revision(&self) -> u64 {
        self.0.revision
    }

    #[inline]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    #[inline]
    fn meta(&self) -> &MergesMeta {
        self.0.meta.get_or_init(|| MergesMeta::build(&self.0.rects))
    }

    #[inline]
    pub fn max_row(&self) -> Option<u32> {
        if self.0.rects.is_empty() {
            return None;
        }
        self.meta().max_row
    }

    #[inline]
    pub fn max_col(&self) -> Option<u32> {
        if self.0.rects.is_empty() {
            return None;
        }
        self.meta().max_col
    }

    #[inline]
    pub fn at(&self, r: u32, c: u32) -> Option<MergeRect> {
        if self.0.rects.is_empty() {
            return None;
        }
        self.meta().at(r, c)
    }

    pub fn merge_range(&mut self, r1: u32, c1: u32, r2: u32, c2: u32) -> bool {
        let (r1, r2) = (r1.min(r2), r1.max(r2));
        let (c1, c2) = (c1.min(c2), c1.max(c2));
        if r1 == r2 && c1 == c2 {
            return false;
        }
        let rects: &mut Vec<MergeRect> = &mut *self;
        rects.retain(|&(a, b, cc, d)| a > r2 || cc < r1 || b > c2 || d < c1);
        rects.push((r1, c1, r2, c2));
        rects.sort_unstable();
        true
    }

    pub fn merge_ranges(&mut self, incoming: impl IntoIterator<Item = MergeRect>) -> bool {
        let normalized: Vec<MergeRect> = incoming
            .into_iter()
            .filter_map(|(r1, c1, r2, c2)| {
                let (r1, r2) = (r1.min(r2), r1.max(r2));
                let (c1, c2) = (c1.min(c2), c1.max(c2));
                (r1 != r2 || c1 != c2).then_some((r1, c1, r2, c2))
            })
            .collect();
        if normalized.is_empty() {
            return false;
        }
        let rects: &mut Vec<MergeRect> = &mut *self;
        if normalized.len() <= 4 && rects.len() <= 32 {
            if !rects.is_empty() {
                rects.retain(|&existing| normalized.iter().all(|&inc| !rects_overlap(existing, inc)));
            }
            for rect in normalized {
                rects.retain(|&existing| !rects_overlap(existing, rect));
                rects.push(rect);
            }
            rects.sort_unstable();
            return true;
        }

        // Process incoming in reverse: an incoming rect survives iff no later incoming rect
        // overlaps it, and every incoming rect (even one killed by a later rect) kills any
        // earlier incoming or existing rect it overlaps.
        let mut seen = ConflictIndex::default();
        let mut kept_incoming = Vec::with_capacity(normalized.len());
        for rect in normalized.into_iter().rev() {
            let (overlaps, covered) = seen.overlaps_status(rect);
            if !overlaps {
                kept_incoming.push(rect);
            }
            if !covered {
                seen.insert(rect);
            }
        }
        if !rects.is_empty() {
            rects.retain(|&existing| !seen.overlaps_any(existing));
        }
        rects.extend(kept_incoming);
        rects.sort_unstable();
        true
    }

    pub fn unmerge_range(&mut self, r1: u32, c1: u32, r2: u32, c2: u32) -> bool {
        if self.0.rects.is_empty() {
            return false;
        }
        let (r1, r2) = (r1.min(r2), r1.max(r2));
        let (c1, c2) = (c1.min(c2), c1.max(c2));
        if !self
            .0
            .rects
            .iter()
            .any(|&(a, b, cc, d)| a <= r2 && cc >= r1 && b <= c2 && d >= c1)
        {
            return false;
        }
        let before = self.0.rects.len();
        let rects: &mut Vec<MergeRect> = &mut *self;
        rects.retain(|&(a, b, cc, d)| a > r2 || cc < r1 || b > c2 || d < c1);
        rects.len() != before
    }
}

impl std::ops::Deref for Merges {
    type Target = Vec<MergeRect>;
    fn deref(&self) -> &Self::Target {
        &self.0.rects
    }
}

impl std::ops::DerefMut for Merges {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let inner = std::sync::Arc::make_mut(&mut self.0);
        inner.revision = next_merge_revision();
        inner.meta.take();
        &mut inner.rects
    }
}

impl AsRef<[MergeRect]> for Merges {
    fn as_ref(&self) -> &[MergeRect] {
        &self.0.rects
    }
}

impl PartialEq for Merges {
    fn eq(&self, other: &Self) -> bool {
        self.0.revision == other.0.revision || self.0.rects == other.0.rects
    }
}

impl Eq for Merges {}

impl PartialEq<Vec<MergeRect>> for Merges {
    fn eq(&self, other: &Vec<MergeRect>) -> bool {
        self.0.rects.as_slice() == other.as_slice()
    }
}

impl PartialEq<[MergeRect]> for Merges {
    fn eq(&self, other: &[MergeRect]) -> bool {
        self.0.rects.as_slice() == other
    }
}

impl PartialEq<&[MergeRect]> for Merges {
    fn eq(&self, other: &&[MergeRect]) -> bool {
        self.0.rects.as_slice() == *other
    }
}

impl<const N: usize> PartialEq<[MergeRect; N]> for Merges {
    fn eq(&self, other: &[MergeRect; N]) -> bool {
        self.0.rects.as_slice() == other.as_slice()
    }
}

impl IntoIterator for Merges {
    type Item = MergeRect;
    type IntoIter = std::vec::IntoIter<MergeRect>;
    fn into_iter(self) -> Self::IntoIter {
        std::sync::Arc::unwrap_or_clone(self.0).rects.into_iter()
    }
}

impl<'a> IntoIterator for &'a Merges {
    type Item = &'a MergeRect;
    type IntoIter = std::slice::Iter<'a, MergeRect>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.rects.iter()
    }
}

impl<'a> IntoIterator for &'a mut Merges {
    type Item = &'a mut MergeRect;
    type IntoIter = std::slice::IterMut<'a, MergeRect>;
    fn into_iter(self) -> Self::IntoIter {
        (&mut **self).iter_mut()
    }
}

/// 256-row page-bucketed spatial index with a separate `tall` list for >16-page merges.
/// Retains source indices so overlapping imported merges keep their original precedence.
#[derive(Default, Clone, Debug)]
pub struct MergeIndex {
    revision: Option<u64>,
    shared: Option<std::sync::Arc<MergesInner>>,
    source: Vec<MergeRect>,
    meta: MergesMeta,
}

impl MergeIndex {
    /// O(1) revision-checked refresh against `Sheet::merges`, sharing the CoW `MergesMeta` index.
    pub fn refresh_merges(&mut self, merges: &Merges) {
        let rev = merges.revision();
        if self.revision == Some(rev) {
            return;
        }
        self.shared = Some(merges.0.clone());
        self.revision = Some(rev);
        self.source.clear();
    }

    pub fn refresh(&mut self, merges: &[MergeRect]) {
        if self.revision.is_none() && self.shared.is_none() && self.source == merges {
            return;
        }
        self.shared = None;
        self.revision = None;
        self.source.clear();
        self.source.extend_from_slice(merges);
        self.meta = MergesMeta::build(&self.source);
    }

    #[inline]
    fn active_meta(&self) -> &MergesMeta {
        if let Some(ref inner) = self.shared {
            inner.meta.get_or_init(|| MergesMeta::build(&inner.rects))
        } else {
            &self.meta
        }
    }

    #[inline]
    pub fn max_row(&self) -> Option<u32> {
        self.active_meta().max_row
    }

    #[inline]
    pub fn max_col(&self) -> Option<u32> {
        self.active_meta().max_col
    }

    #[inline]
    pub fn at(&self, r: u32, c: u32) -> Option<MergeRect> {
        self.active_meta().at(r, c)
    }
}
