//! 多字段透视（`PivotSpec2`）→ 真 `pivotTable` 部件。
//!
//! 关键优化：
//! 1. 提供 `build_wb` / `build_sheet` 直接接收内存 `&Workbook` / `&Sheet`（零磁盘二次加载）；
//! 2. `sharedItems` 去重、`idx_of` 与缓存记录写入全部改为 `HashMap<String, usize>` `O(1)` 查找；
//! 3. 直接使用 `sheet.value_ref` 与单缓冲流式拼接，消除百万次 `Value::clone` 与 `O(N * U)` 线性扫描。

use crate::model::{col_label, Sheet, Value, Workbook};
use crate::pivot::Agg;
use crate::pivot_multi::{PivotGrid, PivotSpec2};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

fn col_name(mut c: u32) -> String {
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

fn esc(s: &str) -> String {
    if !s.bytes().any(|b| matches!(b, b'&' | b'<' | b'>' | b'"')) {
        return s.to_string();
    }
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn push_esc(out: &mut String, s: &str) {
    if !s.bytes().any(|b| matches!(b, b'&' | b'<' | b'>' | b'"')) {
        out.push_str(s);
        return;
    }
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

#[inline]
fn idx_of(shared: &mut Vec<String>, map: &mut HashMap<String, usize>, v: &str) -> usize {
    if let Some(&i) = map.get(v) {
        return i;
    }
    let idx = shared.len();
    let owned = v.to_string();
    map.insert(owned.clone(), idx);
    shared.push(owned);
    idx
}

#[inline]
fn idx_of_owned(shared: &mut Vec<String>, map: &mut HashMap<String, usize>, v: String) -> usize {
    if let Some(&i) = map.get(&v) {
        return i;
    }
    let idx = shared.len();
    map.insert(v.clone(), idx);
    shared.push(v);
    idx
}

pub fn build(
    path: &Path,
    sheet_name: &str,
    spec: &PivotSpec2,
    grid: &PivotGrid,
    out_rect: (u32, u32, u32, u32),
    cache_id: u32,
    table_name: &str,
    refreshed_by: &str,
) -> Result<(String, String, String)> {
    if spec.rows.is_empty() {
        bail!("多字段透视至少需要一个行字段");
    }
    if spec.values.is_empty() {
        bail!("多字段透视至少需要一个值字段");
    }
    let wb = crate::xlsx::load(path).context("透视表：读取源工作簿失败")?;
    build_wb(
        &wb,
        sheet_name,
        spec,
        grid,
        out_rect,
        cache_id,
        table_name,
        refreshed_by,
    )
}

/// 直接从内存 `&Workbook` 构建多字段透视部件 XML（零磁盘二次加载！）
pub fn build_wb(
    wb: &Workbook,
    sheet_name: &str,
    spec: &PivotSpec2,
    grid: &PivotGrid,
    out_rect: (u32, u32, u32, u32),
    cache_id: u32,
    table_name: &str,
    refreshed_by: &str,
) -> Result<(String, String, String)> {
    if spec.rows.is_empty() {
        bail!("多字段透视至少需要一个行字段");
    }
    if spec.values.is_empty() {
        bail!("多字段透视至少需要一个值字段");
    }
    let sheet = wb
        .sheets
        .iter()
        .find(|s| s.name == sheet_name)
        .with_context(|| format!("透视表：源工作表 {sheet_name} 不存在"))?;
    build_sheet(
        sheet,
        sheet_name,
        spec,
        grid,
        out_rect,
        cache_id,
        table_name,
        refreshed_by,
    )
}

/// 直接从内存源表 `&Sheet` 构建多字段透视部件 XML。
pub fn build_sheet(
    sheet: &Sheet,
    sheet_name: &str,
    spec: &PivotSpec2,
    grid: &PivotGrid,
    out_rect: (u32, u32, u32, u32),
    cache_id: u32,
    table_name: &str,
    refreshed_by: &str,
) -> Result<(String, String, String)> {
    if spec.rows.is_empty() {
        bail!("多字段透视至少需要一个行字段");
    }
    if spec.values.is_empty() {
        bail!("多字段透视至少需要一个值字段");
    }

    let (r1, c1, r2, c2) = spec.source;
    let n_fields = (c2 - c1 + 1) as usize;
    let value_cols: Vec<u32> = spec.values.iter().map(|v| v.col).collect();
    let mut names = Vec::with_capacity(n_fields);
    for i in 0..n_fields {
        let col = c1 + i as u32;
        let name = sheet
            .get(r1, col)
            .map(|c| c.value.display())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| col_label(col));
        names.push(name);
    }

    let mut shared: Vec<Vec<String>> = vec![Vec::new(); n_fields];
    let mut shared_map: Vec<HashMap<String, usize>> = vec![HashMap::new(); n_fields];
    let mut numeric = vec![false; n_fields];
    let mut bounds: Vec<(Option<f64>, Option<f64>)> = vec![(None, None); n_fields];
    for i in 0..n_fields {
        let col = c1 + i as u32;
        let is_value = value_cols.contains(&col);
        let mut saw_num = false;
        for r in (r1 + 1)..=r2 {
            let cell_val = sheet.value_ref(r, col);
            match cell_val.as_ref() {
                Value::Empty => {}
                Value::Number(v) => {
                    let v = *v;
                    saw_num = true;
                    let (lo, hi) = bounds[i];
                    bounds[i] = (
                        Some(lo.map_or(v, |x: f64| x.min(v))),
                        Some(hi.map_or(v, |x: f64| x.max(v))),
                    );
                    if !is_value {
                        let s = crate::model::format_number(v);
                        idx_of_owned(&mut shared[i], &mut shared_map[i], s);
                    }
                }
                Value::Text(t) => {
                    if !t.is_empty() {
                        idx_of(&mut shared[i], &mut shared_map[i], t.as_ref());
                    }
                }
                other => {
                    let s = other.display();
                    if !s.is_empty() {
                        idx_of_owned(&mut shared[i], &mut shared_map[i], s);
                    }
                }
            }
        }
        numeric[i] = is_value && saw_num;
        if numeric[i] {
            shared[i].clear();
            shared_map[i].clear();
        }
    }

    // ---------- 行轴 ----------
    let row_depth = spec.rows.len();
    let row_fields: Vec<usize> = spec.rows.iter().map(|f| (f.col - c1) as usize).collect();
    let mut row_items = String::with_capacity(grid.rows.len() * (row_depth * 12 + 8));
    let mut calc_marks: HashSet<(usize, usize)> = Default::default();
    let base_len: Vec<usize> = shared.iter().map(|v| v.len()).collect();
    let mut prev_labels: Vec<String> = vec![String::new(); row_depth];
    for (ri, line) in grid.rows.iter().enumerate() {
        let labels: Vec<String> = (0..row_depth)
            .map(|d| line.get(d).map(|v| v.display()).unwrap_or_default())
            .collect();
        let is_grand = grid.grand_total_row == Some(ri);
        let is_subtotal = !is_grand
            && row_depth >= 2
            && labels.first().map(|s| s.ends_with("小计")).unwrap_or(false);
        if is_grand {
            row_items.push_str("<i t=\"grand\"><x/></i>");
            prev_labels = vec![String::new(); row_depth];
            continue;
        }
        if is_subtotal {
            row_items.push_str("<i t=\"default\"><x/></i>");
            prev_labels = vec![String::new(); row_depth];
            continue;
        }
        row_items.push_str("<i>");
        for d in 0..row_depth {
            let label = if labels[d].is_empty() {
                prev_labels[d].as_str()
            } else {
                labels[d].as_str()
            };
            let fi = row_fields[d];
            let idx = idx_of(&mut shared[fi], &mut shared_map[fi], label);
            if idx >= base_len[fi] {
                calc_marks.insert((fi, idx));
            }
            let _ = write!(row_items, "<x v=\"{idx}\"/>");
        }
        row_items.push_str("</i>");
        prev_labels = labels;
    }

    // ---------- 列轴 ----------
    let k = spec.values.len();
    let mut extra = String::new();
    let n_cols = spec.cols.len();
    if n_cols == 0 {
        if k > 1 {
            let mut items = String::new();
            for j in 0..k {
                let _ = write!(items, "<i i=\"{j}\"><x v=\"{j}\"/></i>");
            }
            let _ = write!(
                extra,
                "<colFields count=\"1\"><field x=\"-2\"/></colFields><colItems count=\"{k}\">{items}</colItems>"
            );
        } else {
            extra.push_str("<colItems count=\"1\"><i/></colItems>");
        }
    } else {
        let col_field_idx: Vec<usize> = spec.cols.iter().map(|f| (f.col - c1) as usize).collect();
        if grid.col_paths.is_empty() {
            bail!("多字段透视：列字段没有任何叶子标签");
        }
        let paths = &grid.col_paths;
        let mut items: Vec<String> = Vec::new();
        let mut i = 0usize;
        while i < paths.len() {
            let head = paths[i].first().map(String::as_str).unwrap_or("");
            let group_start = i;
            while i < paths.len() && paths[i].first().map(String::as_str).unwrap_or("") == head {
                i += 1;
            }
            let group = &paths[group_start..i];
            for (j, path) in group.iter().enumerate() {
                if j == 0 {
                    let mut xs = String::new();
                    for d in 0..n_cols {
                        let fi = col_field_idx[d];
                        let idx = idx_of(&mut shared[fi], &mut shared_map[fi], &path[d]);
                        let _ = write!(xs, "<x v=\"{idx}\"/>");
                    }
                    if k > 1 {
                        xs.push_str("<x/>");
                    }
                    items.push(format!("<i>{xs}</i>"));
                } else {
                    let fi = col_field_idx[n_cols - 1];
                    let idx = idx_of(&mut shared[fi], &mut shared_map[fi], &path[n_cols - 1]);
                    let tail = if k > 1 { "<x/>" } else { "" };
                    items.push(format!("<i r=\"{}\"><x v=\"{idx}\"/>{}</i>", n_cols - 1, tail));
                }
                if k > 1 {
                    for v in 1..k {
                        items.push(format!("<i r=\"{n_cols}\" i=\"{v}\"><x v=\"{v}\"/></i>"));
                    }
                }
            }
            if spec.layout.subtotals && n_cols >= 2 {
                let mut xs = String::new();
                for d in 0..n_cols - 1 {
                    let fi = col_field_idx[d];
                    let idx = idx_of(&mut shared[fi], &mut shared_map[fi], &group[0][d]);
                    let _ = write!(xs, "<x v=\"{idx}\"/>");
                }
                xs.push_str("<x/>");
                if k > 1 {
                    items.push(format!("<i t=\"default\">{xs}</i>"));
                    for v in 1..k {
                        items.push(format!("<i t=\"default\" i=\"{v}\">{xs}</i>"));
                    }
                } else {
                    items.push(format!("<i t=\"default\">{xs}</i>"));
                }
            }
            i = i.max(group_start + 1);
        }
        if k > 1 {
            items.push("<i t=\"grand\"><x/></i>".to_string());
            for v in 1..k {
                items.push(format!("<i t=\"grand\" i=\"{v}\"><x/></i>"));
            }
        } else {
            items.push("<i t=\"grand\"><x/></i>".to_string());
        }
        let col_count = items.len();
        let col_items = items.join("");
        let mut fields = String::new();
        for f in &col_field_idx {
            let _ = write!(fields, "<field x=\"{f}\"/>");
        }
        if k > 1 {
            fields.push_str("<field x=\"-2\"/>");
        }
        let _ = write!(
            extra,
            "<colFields count=\"{}\">{fields}</colFields><colItems count=\"{col_count}\">{col_items}</colItems>",
            n_cols + usize::from(k > 1)
        );
    }

    // ---------- 缓存字段与记录 ----------
    let mut caches = String::new();
    for i in 0..n_fields {
        if numeric[i] {
            let (lo, hi) = bounds[i];
            let _ = write!(
                caches,
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems containsSemiMixedTypes=\"0\" containsString=\"0\" containsNumber=\"1\"",
                esc(&names[i])
            );
            if let Some(v) = lo {
                let _ = write!(caches, " minValue=\"{v}\"");
            }
            if let Some(v) = hi {
                let _ = write!(caches, " maxValue=\"{v}\"");
            }
            caches.push_str("/></cacheField>");
        } else {
            let _ = write!(
                caches,
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems count=\"{}\">",
                esc(&names[i]),
                shared[i].len()
            );
            for (k_idx, s) in shared[i].iter().enumerate() {
                caches.push_str("<s v=\"");
                push_esc(&mut caches, s);
                if calc_marks.contains(&(i, k_idx)) {
                    caches.push_str("\" f=\"1\"/>");
                } else {
                    caches.push_str("\"/>");
                }
            }
            caches.push_str("</sharedItems></cacheField>");
        }
    }

    let n_records = r2.saturating_sub(r1) as usize;
    let mut recs = String::with_capacity(n_records.saturating_mul(n_fields * 12 + 8));
    for r in (r1 + 1)..=r2 {
        recs.push_str("<r>");
        for i in 0..n_fields {
            let col = c1 + i as u32;
            let v = sheet.value_ref(r, col);
            if numeric[i] {
                match v.as_ref() {
                    Value::Number(n) => {
                        let _ = write!(recs, "<n v=\"{n}\"/>");
                    }
                    Value::Empty => recs.push_str("<m/>"),
                    other => {
                        recs.push_str("<s v=\"");
                        push_esc(&mut recs, &other.display());
                        recs.push_str("\"/>");
                    }
                }
            } else {
                let idx_opt = match v.as_ref() {
                    Value::Empty => None,
                    Value::Text(t) if !t.is_empty() => shared_map[i].get(t.as_ref()).copied(),
                    other => {
                        let key = other.display();
                        (!key.is_empty()).then(|| shared_map[i].get(&key).copied()).flatten()
                    }
                };
                match idx_opt {
                    Some(idx) => {
                        let _ = write!(recs, "<x v=\"{idx}\"/>");
                    }
                    None => recs.push_str("<m/>"),
                }
            }
        }
        recs.push_str("</r>");
    }

    let src = format!("{}{}:{}{}", col_name(c1), r1 + 1, col_name(c2), r2 + 1);
    let cache = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotCacheDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"rId1\" refreshedBy=\"{}\" createdVersion=\"8\" refreshedVersion=\"8\" minRefreshableVersion=\"3\" recordCount=\"{n_records}\"><cacheSource type=\"worksheet\"><worksheetSource ref=\"{src}\" sheet=\"{}\"/></cacheSource><cacheFields count=\"{n_fields}\">{caches}</cacheFields></pivotCacheDefinition>",
        esc(refreshed_by),
        esc(sheet_name)
    );
    let records = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotCacheRecords xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" count=\"{n_records}\">{recs}</pivotCacheRecords>"
    );

    // ---------- pivotFields ----------
    let items_for = |field: usize, sh: &[String]| -> String {
        let mut out = String::with_capacity(sh.len() * 16 + 20);
        for k_idx in 0..sh.len() {
            if calc_marks.contains(&(field, k_idx)) {
                let _ = write!(out, "<item f=\"1\" x=\"{k_idx}\"/>");
            } else {
                let _ = write!(out, "<item x=\"{k_idx}\"/>");
            }
        }
        out.push_str("<item t=\"default\"/>");
        out
    };
    let col_field_set: Vec<usize> = spec.cols.iter().map(|f| (f.col - c1) as usize).collect();
    let mut pfields = String::new();
    for i in 0..n_fields {
        if row_fields.contains(&i) {
            let _ = write!(
                pfields,
                "<pivotField axis=\"axisRow\" compact=\"0\" outline=\"0\" showAll=\"0\"><items count=\"{}\">{}</items></pivotField>",
                shared[i].len() + 1,
                items_for(i, &shared[i])
            );
        } else if col_field_set.contains(&i) {
            let _ = write!(
                pfields,
                "<pivotField axis=\"axisCol\" compact=\"0\" outline=\"0\" showAll=\"0\"><items count=\"{}\">{}</items></pivotField>",
                shared[i].len() + 1,
                items_for(i, &shared[i])
            );
        } else if value_cols.contains(&(c1 + i as u32)) {
            pfields.push_str("<pivotField dataField=\"1\" compact=\"0\" outline=\"0\" showAll=\"0\"/>");
        } else {
            pfields.push_str("<pivotField compact=\"0\" outline=\"0\" showAll=\"0\"/>");
        }
    }

    // ---------- 表头/位置 ----------
    let n_head = if spec.cols.is_empty() { 1u32 } else { spec.cols.len() as u32 + 1 };
    let first_data_row = if spec.cols.is_empty() {
        1
    } else {
        n_head + u32::from(k > 1)
    };
    let loc = format!(
        "{}{}:{}{}",
        col_name(out_rect.1),
        out_rect.0 + 1,
        col_name(out_rect.3),
        out_rect.2 + 1
    );
    let row_fields_xml: String = row_fields
        .iter()
        .map(|f| format!("<field x=\"{f}\"/>"))
        .collect();
    let data_fields: String = spec
        .values
        .iter()
        .map(|v| {
            let label = v
                .label
                .clone()
                .unwrap_or_else(|| format!("{}{}", agg_label(v.agg), names[(v.col - c1) as usize]));
            let short = label.rsplit_once(':').map(|(_, t)| t.to_string()).unwrap_or(label);
            let sub = match v.agg.ooxml_subtotal() {
                Some(s) => format!(" subtotal=\"{s}\""),
                None => String::new(),
            };
            let show = match v.show_as.xml() {
                Some(x) if v.show_as.percent_format() => {
                    format!(" showDataAs=\"{x}\" numFmtId=\"10\"")
                }
                Some(x) => format!(" showDataAs=\"{x}\""),
                None => String::new(),
            };
            let (bf, bi) = match &v.base {
                Some((col, item)) => {
                    let idx = col.saturating_sub(c1) as usize;
                    let pos = shared_map
                        .get(idx)
                        .and_then(|m| m.get(item).copied())
                        .unwrap_or(0);
                    (idx.to_string(), pos.to_string())
                }
                None => ("0".to_string(), "0".to_string()),
            };
            format!(
                "<dataField name=\"{}{}\" fld=\"{}\"{}{} baseField=\"{}\" baseItem=\"{}\"/>",
                agg_label(v.agg),
                esc(&short),
                v.col - c1,
                sub,
                show,
                bf,
                bi
            )
        })
        .collect();

    let mut table = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotTableDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" name=\"{}\" cacheId=\"{cache_id}\" dataCaption=\"值\" updatedVersion=\"6\" minRefreshableVersion=\"3\" createdVersion=\"6\" applyWidthHeightFormats=\"1\" useAutoFormatting=\"1\" itemPrintTitles=\"1\" indent=\"0\" outline=\"1\" outlineData=\"1\" compact=\"0\" compactData=\"0\" multipleFieldFilters=\"0\"><location ref=\"{loc}\" firstHeaderRow=\"1\" firstDataRow=\"{first_data_row}\" firstDataCol=\"{row_depth}\"/><pivotFields count=\"{n_fields}\">{pfields}</pivotFields><rowFields count=\"{row_depth}\">{row_fields_xml}</rowFields><rowItems count=\"{}\">{row_items}</rowItems>{extra}<dataFields count=\"{k}\">{data_fields}</dataFields><pivotTableStyleInfo name=\"PivotStyleLight16\" showRowHeaders=\"1\" showColHeaders=\"1\" showRowStripes=\"0\" showColStripes=\"0\" showLastColumn=\"1\"/></pivotTableDefinition>",
        esc(table_name),
        grid.rows.len()
    );

    if !spec.layout.tabular {
        table = table.replace(
            "outline=\"1\" outlineData=\"1\" compact=\"0\" compactData=\"0\"",
            "outline=\"0\" outlineData=\"0\" compact=\"1\" compactData=\"1\"",
        );
    }

    Ok((cache, records, table))
}

fn agg_label(a: Agg) -> String {
    format!("{}:", crate::pivot::agg_prefix(a))
}

/// 从一等公民 [`crate::pivot_engine::PivotTableModel`] 生成跨表兼容、含页筛选/隐藏项/排序/主题样式的真 OOXML 三件套。
pub fn build_model_wb(
    wb: &Workbook,
    model: &crate::pivot_engine::PivotTableModel,
    cache_id: u32,
    table_name: &str,
    refreshed_by: &str,
) -> Result<(String, String, String)> {
    let source_sheet = wb
        .sheets
        .iter()
        .find(|s| model.source_sheet_id == Some(s.identity) || s.name == model.source_sheet)
        .with_context(|| format!("透视表：源工作表 {} 不存在", model.source_sheet))?;
    let spec = &model.spec;
    let opts = &model.options;
    let computed = crate::pivot_engine::compute_table(source_sheet, model);
    let grid = &computed.grid;

    let (r1, c1, r2, c2) = spec.source;
    let n_fields = (c2.saturating_sub(c1) + 1) as usize;
    let value_cols: Vec<u32> = spec.values.iter().map(|v| v.col).collect();
    let mut names = Vec::with_capacity(n_fields);
    for i in 0..n_fields {
        let col = c1 + i as u32;
        let name = source_sheet
            .get(r1, col)
            .map(|c| c.value.display())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| col_label(col));
        names.push(name);
    }

    let mut shared: Vec<Vec<String>> = vec![Vec::new(); n_fields];
    let mut shared_map: Vec<HashMap<String, usize>> = vec![HashMap::new(); n_fields];
    let mut numeric = vec![false; n_fields];
    let mut bounds: Vec<(Option<f64>, Option<f64>)> = vec![(None, None); n_fields];
    for i in 0..n_fields {
        let col = c1 + i as u32;
        let is_value = value_cols.contains(&col)
            && !spec.rows.iter().any(|f| f.col == col)
            && !spec.cols.iter().any(|f| f.col == col)
            && !spec.filters.iter().any(|f| f.col == col);
        let mut saw_num = false;
        for r in (r1 + 1)..=r2 {
            let cell_val = source_sheet.value_ref(r, col);
            match cell_val.as_ref() {
                Value::Empty => {}
                Value::Number(v) => {
                    let v = *v;
                    saw_num = true;
                    let (lo, hi) = bounds[i];
                    bounds[i] = (
                        Some(lo.map_or(v, |x: f64| x.min(v))),
                        Some(hi.map_or(v, |x: f64| x.max(v))),
                    );
                    if !is_value {
                        let s = crate::model::format_number(v);
                        idx_of_owned(&mut shared[i], &mut shared_map[i], s);
                    }
                }
                Value::Text(t) => {
                    if !t.is_empty() {
                        idx_of(&mut shared[i], &mut shared_map[i], t.as_ref());
                    }
                }
                other => {
                    let s = other.display();
                    if !s.is_empty() {
                        idx_of_owned(&mut shared[i], &mut shared_map[i], s);
                    }
                }
            }
        }
        numeric[i] = is_value && saw_num;
        if numeric[i] {
            shared[i].clear();
            shared_map[i].clear();
        }
    }

    let row_fields: Vec<usize> = spec
        .rows
        .iter()
        .filter_map(|f| f.col.checked_sub(c1).map(|d| d as usize).filter(|&x| x < n_fields))
        .collect();
    let col_fields: Vec<usize> = spec
        .cols
        .iter()
        .filter_map(|f| f.col.checked_sub(c1).map(|d| d as usize).filter(|&x| x < n_fields))
        .collect();
    let page_fields: Vec<(usize, &crate::pivot_multi::FilterField)> = spec
        .filters
        .iter()
        .filter_map(|f| f.col.checked_sub(c1).map(|d| (d as usize, f)).filter(|&(x, _)| x < n_fields))
        .collect();

    // 缓存字段 XML（含计算字段）
    let mut caches = String::new();
    for i in 0..n_fields {
        if numeric[i] {
            let (lo, hi) = bounds[i];
            let _ = write!(
                caches,
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems containsSemiMixedTypes=\"0\" containsString=\"0\" containsNumber=\"1\"",
                esc(&names[i])
            );
            if let Some(v) = lo {
                let _ = write!(caches, " minValue=\"{v}\"");
            }
            if let Some(v) = hi {
                let _ = write!(caches, " maxValue=\"{v}\"");
            }
            caches.push_str("/></cacheField>");
        } else {
            let _ = write!(
                caches,
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems count=\"{}\">",
                esc(&names[i]),
                shared[i].len()
            );
            for s in &shared[i] {
                caches.push_str("<s v=\"");
                push_esc(&mut caches, s);
                caches.push_str("\"/>");
            }
            caches.push_str("</sharedItems></cacheField>");
        }
    }
    let calc_field_base_idx = n_fields;
    for (cname, cformula) in &opts.calc_fields {
        let _ = write!(
            caches,
            "<cacheField name=\"{}\" numFmtId=\"0\" databaseField=\"0\" formula=\"{}\"/>",
            esc(cname),
            esc(cformula)
        );
    }
    let total_cache_fields = n_fields + opts.calc_fields.len();

    let n_records = r2.saturating_sub(r1) as usize;
    let mut recs = String::with_capacity(n_records.saturating_mul(n_fields * 12 + 8));
    for r in (r1 + 1)..=r2 {
        recs.push_str("<r>");
        for i in 0..n_fields {
            let col = c1 + i as u32;
            let v = source_sheet.value_ref(r, col);
            if numeric[i] {
                match v.as_ref() {
                    Value::Number(n) => {
                        let _ = write!(recs, "<n v=\"{n}\"/>");
                    }
                    Value::Empty => recs.push_str("<m/>"),
                    other => {
                        recs.push_str("<s v=\"");
                        push_esc(&mut recs, &other.display());
                        recs.push_str("\"/>");
                    }
                }
            } else {
                let idx_opt = match v.as_ref() {
                    Value::Empty => None,
                    Value::Text(t) if !t.is_empty() => shared_map[i].get(t.as_ref()).copied(),
                    other => {
                        let key = other.display();
                        (!key.is_empty()).then(|| shared_map[i].get(&key).copied()).flatten()
                    }
                };
                match idx_opt {
                    Some(idx) => {
                        let _ = write!(recs, "<x v=\"{idx}\"/>");
                    }
                    None => recs.push_str("<m/>"),
                }
            }
        }
        recs.push_str("</r>");
    }

    let src = format!("{}{}:{}{}", col_name(c1), r1 + 1, col_name(c2), r2 + 1);
    let cache = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotCacheDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"rId1\" refreshedBy=\"{}\" createdVersion=\"8\" refreshedVersion=\"8\" minRefreshableVersion=\"3\" refreshOnLoad=\"1\" recordCount=\"{n_records}\"><cacheSource type=\"worksheet\"><worksheetSource ref=\"{src}\" sheet=\"{}\"/></cacheSource><cacheFields count=\"{total_cache_fields}\">{caches}</cacheFields></pivotCacheDefinition>",
        esc(refreshed_by),
        esc(&source_sheet.name)
    );
    let records = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotCacheRecords xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" count=\"{n_records}\">{recs}</pivotCacheRecords>"
    );

    // pivotFields：写出 axis / hidden items / sortType / defaultSubtotal
    let mut pfields = String::new();
    for i in 0..total_cache_fields {
        if i >= n_fields {
            pfields.push_str("<pivotField dataField=\"1\" compact=\"0\" outline=\"0\" showAll=\"0\"/>");
            continue;
        }
        let col = c1 + i as u32;
        let row_pos = spec.rows.iter().position(|f| f.col == col);
        let col_pos = spec.cols.iter().position(|f| f.col == col);
        let page_pos = spec.filters.iter().position(|f| f.col == col);
        let is_val = value_cols.contains(&col);
        let mut attrs = String::new();
        let mut hidden_set: HashSet<&str> = HashSet::new();
        if let Some(rp) = row_pos {
            attrs.push_str(" axis=\"axisRow\"");
            if let Some(ext) = opts.row_ext.get(rp) {
                for h in &ext.hidden_items {
                    hidden_set.insert(h.as_str());
                }
                if matches!(ext.sort, crate::pivot_engine::FieldSortMode::PinyinDesc) {
                    attrs.push_str(" sortType=\"descending\"");
                }
                if !ext.show_subtotal || opts.subtotal_pos == crate::pivot_engine::SubtotalPos::Off {
                    attrs.push_str(" defaultSubtotal=\"0\"");
                }
            }
        } else if let Some(cp) = col_pos {
            attrs.push_str(" axis=\"axisCol\"");
            if let Some(ext) = opts.col_ext.get(cp) {
                for h in &ext.hidden_items {
                    hidden_set.insert(h.as_str());
                }
                if matches!(ext.sort, crate::pivot_engine::FieldSortMode::PinyinDesc) {
                    attrs.push_str(" sortType=\"descending\"");
                }
                if !ext.show_subtotal || opts.subtotal_pos == crate::pivot_engine::SubtotalPos::Off {
                    attrs.push_str(" defaultSubtotal=\"0\"");
                }
            }
        } else if let Some(pp) = page_pos {
            attrs.push_str(" axis=\"axisPage\"");
            let keep = &spec.filters[pp].keep;
            if !keep.is_empty() {
                let keep_set: HashSet<&str> = keep.iter().map(String::as_str).collect();
                for s in &shared[i] {
                    if !keep_set.contains(s.as_str()) {
                        hidden_set.insert(s.as_str());
                    }
                }
            }
        }
        if is_val {
            attrs.push_str(" dataField=\"1\"");
        }
        if row_pos.is_some() || col_pos.is_some() || page_pos.is_some() {
            let mut items_xml = String::new();
            for (k_idx, s) in shared[i].iter().enumerate() {
                if hidden_set.contains(s.as_str()) {
                    let _ = write!(items_xml, "<item h=\"1\" x=\"{k_idx}\"/>");
                } else {
                    let _ = write!(items_xml, "<item x=\"{k_idx}\"/>");
                }
            }
            items_xml.push_str("<item t=\"default\"/>");
            let _ = write!(
                pfields,
                "<pivotField{attrs} compact=\"0\" outline=\"0\" showAll=\"0\"><items count=\"{}\">{items_xml}</items></pivotField>",
                shared[i].len() + 1
            );
        } else {
            let _ = write!(pfields, "<pivotField{attrs} compact=\"0\" outline=\"0\" showAll=\"0\"/>");
        }
    }

    // rowFields / colFields / pageFields / dataFields
    let row_fields_xml = if row_fields.is_empty() {
        String::new()
    } else {
        let inner: String = row_fields.iter().map(|f| format!("<field x=\"{f}\"/>")).collect();
        format!("<rowFields count=\"{}\">{inner}</rowFields><rowItems count=\"1\"><i t=\"grand\"><x/></i></rowItems>", row_fields.len())
    };
    let k = spec.values.len() + opts.calc_fields.len();
    let col_fields_xml = if col_fields.is_empty() {
        if k > 1 {
            format!("<colFields count=\"1\"><field x=\"-2\"/></colFields><colItems count=\"{k}\">{}</colItems>", (0..k).map(|j| format!("<i i=\"{j}\"><x v=\"{j}\"/></i>")).collect::<String>())
        } else {
            "<colItems count=\"1\"><i/></colItems>".to_string()
        }
    } else {
        let mut inner: String = col_fields.iter().map(|f| format!("<field x=\"{f}\"/>")).collect();
        if k > 1 {
            inner.push_str("<field x=\"-2\"/>");
        }
        format!(
            "<colFields count=\"{}\">{inner}</colFields><colItems count=\"1\"><i t=\"grand\"><x/></i></colItems>",
            col_fields.len() + usize::from(k > 1)
        )
    };
    let page_fields_xml = if page_fields.is_empty() {
        String::new()
    } else {
        let mut inner = String::new();
        for (fi, ff) in &page_fields {
            if ff.keep.len() == 1
                && let Some(&item_idx) = shared_map[*fi].get(&ff.keep[0])
            {
                let _ = write!(inner, "<pageField fld=\"{fi}\" item=\"{item_idx}\" hier=\"-1\"/>");
            } else {
                let _ = write!(inner, "<pageField fld=\"{fi}\" hier=\"-1\"/>");
            }
        }
        format!("<pageFields count=\"{}\">{inner}</pageFields>", page_fields.len())
    };

    let mut data_fields_xml = String::new();
    for v in &spec.values {
        let fi = v.col.saturating_sub(c1) as usize;
        if fi >= n_fields {
            continue;
        }
        let default_name = format!("{}{}", agg_label(v.agg), names[fi]);
        let disp = v.label.as_deref().filter(|s| !s.is_empty()).unwrap_or(&default_name);
        let sub = match v.agg.ooxml_subtotal() {
            Some(s) => format!(" subtotal=\"{s}\""),
            None => String::new(),
        };
        let show = match v.show_as.xml() {
            Some(x) if v.show_as.percent_format() => format!(" showDataAs=\"{x}\" numFmtId=\"10\""),
            Some(x) => format!(" showDataAs=\"{x}\""),
            None => String::new(),
        };
        let (bf, bi) = match &v.base {
            Some((col, item)) => {
                let idx = col.saturating_sub(c1) as usize;
                let pos = shared_map.get(idx).and_then(|m| m.get(item).copied()).unwrap_or(0);
                (idx.to_string(), pos.to_string())
            }
            None => ("0".to_string(), "0".to_string()),
        };
        let _ = write!(
            data_fields_xml,
            "<dataField name=\"{}\" fld=\"{fi}\"{sub}{show} baseField=\"{bf}\" baseItem=\"{bi}\"/>",
            esc(disp)
        );
    }
    for (ci, (cname, _)) in opts.calc_fields.iter().enumerate() {
        let fi = calc_field_base_idx + ci;
        let _ = write!(
            data_fields_xml,
            "<dataField name=\"{}\" fld=\"{fi}\" baseField=\"0\" baseItem=\"0\"/>",
            esc(cname)
        );
    }
    let data_fields_section = if k > 0 {
        format!("<dataFields count=\"{k}\">{data_fields_xml}</dataFields>")
    } else {
        String::new()
    };

    let out_rect = model.rect;
    let loc = format!(
        "{}{}:{}{}",
        col_name(out_rect.1),
        out_rect.0 + 1,
        col_name(out_rect.3),
        out_rect.2 + 1
    );
    let compact_flag = u8::from(opts.report_layout == crate::pivot_engine::ReportLayout::Compact);
    let outline_flag = u8::from(opts.report_layout != crate::pivot_engine::ReportLayout::Compact);
    let rgrand_attr = if opts.row_grand_totals { "" } else { " rowGrandTotals=\"0\"" };
    let cgrand_attr = if opts.col_grand_totals { "" } else { " colGrandTotals=\"0\"" };
    let missing_attr = if opts.empty_cell_text.is_empty() {
        String::new()
    } else {
        format!(" missingCaption=\"{}\"", esc(&opts.empty_cell_text))
    };
    let first_data_row = grid.headers.len().max(1);
    let first_data_col = computed.meta.row_label_cols.max(1);
    let style_name = opts.style_theme.ooxml_name();
    let r_stripes = u8::from(opts.banded_rows);
    let c_stripes = u8::from(opts.banded_cols);

    let table = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotTableDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" name=\"{}\" cacheId=\"{cache_id}\" dataCaption=\"值\" updatedVersion=\"6\" minRefreshableVersion=\"3\" createdVersion=\"6\" applyWidthHeightFormats=\"1\" useAutoFormatting=\"1\" itemPrintTitles=\"1\" indent=\"0\" outline=\"{outline_flag}\" outlineData=\"{outline_flag}\" compact=\"{compact_flag}\" compactData=\"{compact_flag}\" multipleFieldFilters=\"1\"{rgrand_attr}{cgrand_attr}{missing_attr}><location ref=\"{loc}\" firstHeaderRow=\"1\" firstDataRow=\"{first_data_row}\" firstDataCol=\"{first_data_col}\"/><pivotFields count=\"{total_cache_fields}\">{pfields}</pivotFields>{row_fields_xml}{col_fields_xml}{page_fields_xml}{data_fields_section}<pivotTableStyleInfo name=\"{style_name}\" showRowHeaders=\"1\" showColHeaders=\"1\" showRowStripes=\"{r_stripes}\" showColStripes=\"{c_stripes}\" showLastColumn=\"1\"/></pivotTableDefinition>",
        esc(table_name)
    );

    Ok((cache, records, table))
}
