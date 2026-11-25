//! 生成 Excel 能直接当「可编辑数据透视表」打开的三份 XML：
//! `pivotCacheDefinition`、`pivotCacheRecords`、`pivotTableDefinition`。
//!
//! 关键优化：
//! 1. 提供 `build_wb` / `build_sheet` 直接读取内存 `&Workbook` / `&Sheet`，彻底消除保存阶段二次 `xlsx::load(path)` 读盘；
//! 2. `sharedItems` 去重与缓存记录 `<x v="idx"/>` 反查全部采用 `HashMap<String, usize>` `O(1)` 索引；
//! 3. 直接流式写入单块预分配 `String` 缓冲区，消除百万次 `Value::clone` 与 `format!` 临时堆分配。

use crate::model::{col_label, Sheet, Value, Workbook};
use crate::pivot::{Agg, PivotResult, PivotSpec};
use anyhow::{Context, Result};
use std::collections::HashMap;
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

fn agg_label(a: Agg) -> String {
    format!("{}:", crate::pivot::agg_prefix(a))
}

#[inline]
fn intern_shared(shared: &mut Vec<String>, map: &mut HashMap<String, usize>, s: String) -> usize {
    if let Some(&idx) = map.get(&s) {
        return idx;
    }
    let idx = shared.len();
    map.insert(s.clone(), idx);
    shared.push(s);
    idx
}

#[inline]
fn intern_shared_ref(shared: &mut Vec<String>, map: &mut HashMap<String, usize>, s: &str) -> usize {
    if let Some(&idx) = map.get(s) {
        return idx;
    }
    let idx = shared.len();
    let owned = s.to_string();
    map.insert(owned.clone(), idx);
    shared.push(owned);
    idx
}

/// 返回 (pivotCacheDefinition, pivotCacheRecords, pivotTableDefinition)（兼容路径入口）
pub fn build(
    path: &Path,
    sheet_name: &str,
    spec: &PivotSpec,
    res: &PivotResult,
    out_rect: (u32, u32, u32, u32),
    value_label: &str,
    cache_id: u32,
    table_name: &str,
    refreshed_by: &str,
) -> Result<(String, String, String)> {
    let wb = crate::xlsx::load(path).context("透视表：读取源工作簿失败")?;
    build_wb(
        &wb,
        sheet_name,
        spec,
        res,
        out_rect,
        value_label,
        cache_id,
        table_name,
        refreshed_by,
    )
}

/// 直接从内存 `&Workbook` 生成三份透视表 XML（零磁盘二次加载！）
pub fn build_wb(
    wb: &Workbook,
    sheet_name: &str,
    spec: &PivotSpec,
    res: &PivotResult,
    out_rect: (u32, u32, u32, u32),
    value_label: &str,
    cache_id: u32,
    table_name: &str,
    refreshed_by: &str,
) -> Result<(String, String, String)> {
    let sheet = wb
        .sheets
        .iter()
        .find(|s| s.name == sheet_name)
        .with_context(|| format!("透视表：源工作表 {sheet_name} 不存在"))?;
    build_sheet(
        sheet,
        sheet_name,
        spec,
        res,
        out_rect,
        value_label,
        cache_id,
        table_name,
        refreshed_by,
    )
}

/// 直接从源工作表 `&Sheet` 生成三份透视表 XML。
pub fn build_sheet(
    sheet: &Sheet,
    sheet_name: &str,
    spec: &PivotSpec,
    res: &PivotResult,
    out_rect: (u32, u32, u32, u32),
    value_label: &str,
    cache_id: u32,
    table_name: &str,
    refreshed_by: &str,
) -> Result<(String, String, String)> {
    let (r1, c1, r2, c2) = spec.source;
    let n_fields = (c2 - c1 + 1) as usize;
    let row_i = (spec.row_field - c1) as usize;
    let val_i = (spec.value_field - c1) as usize;
    let col_i = spec.col_field.map(|c| (c - c1) as usize);

    // 1) 字段名取源表表头
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

    // 2) 每个字段的 O(1) 去重取值（值字段走数值，不用 sharedItems）
    let mut shared: Vec<Vec<String>> = vec![Vec::new(); n_fields];
    let mut shared_map: Vec<HashMap<String, usize>> = vec![HashMap::new(); n_fields];
    let mut numeric = vec![false; n_fields];
    let mut bounds: Vec<(Option<f64>, Option<f64>)> = vec![(None, None); n_fields];
    for i in 0..n_fields {
        let col = c1 + i as u32;
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
                    if i != val_i {
                        let s = crate::model::format_number(v);
                        intern_shared(&mut shared[i], &mut shared_map[i], s);
                    }
                }
                Value::Text(t) => {
                    if !t.is_empty() {
                        intern_shared_ref(&mut shared[i], &mut shared_map[i], t.as_ref());
                    }
                }
                other => {
                    let s = other.display();
                    if !s.is_empty() {
                        intern_shared(&mut shared[i], &mut shared_map[i], s);
                    }
                }
            }
        }
        numeric[i] = i == val_i && saw_num;
        if numeric[i] {
            shared[i].clear();
            shared_map[i].clear();
        }
    }

    // 3) 结果里的行/列标签必须在 sharedItems 中有下标
    let mut row_items = String::with_capacity(res.rows.len() * 18);
    for (i, line) in res.rows.iter().enumerate() {
        let label = line.first().map(|v| v.display()).unwrap_or_default();
        let is_grand = i + 1 == res.rows.len()
            && res.rows.len() > 1
            && (label == "总计" || label == "Grand Total");
        if is_grand {
            row_items.push_str("<i t=\"grand\"><x/></i>");
        } else {
            let idx = intern_shared(&mut shared[row_i], &mut shared_map[row_i], label);
            let _ = write!(row_items, "<i><x v=\"{idx}\"/></i>");
        }
    }
    let mut col_items = String::new();
    let col_count;
    match col_i {
        Some(ci) => {
            if res.headers.len() > 1 {
                let data_cols = res.headers.len() - 1 - usize::from(res.col_name.is_some());
                col_count = data_cols + 1;
                for h in res.headers.iter().skip(1).take(data_cols) {
                    let idx = intern_shared_ref(&mut shared[ci], &mut shared_map[ci], h);
                    let _ = write!(col_items, "<i><x v=\"{idx}\"/></i>");
                }
                col_items.push_str("<i t=\"grand\"><x/></i>");
            } else {
                col_count = 1;
                col_items.push_str("<i t=\"grand\"><x/></i>");
            }
        }
        None => {
            col_count = 1;
            col_items.push_str("<i/>");
        }
    }

    // 4) 缓存字段
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

    // 5) 缓存记录（O(1) HashMap 查找 + 直接流式写 String）
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

    // 6) 透视表定义
    let items_of = |field: usize| -> String {
        let mut out = String::with_capacity(shared[field].len() * 14 + 20);
        for k in 0..shared[field].len() {
            let _ = write!(out, "<item x=\"{k}\"/>");
        }
        out.push_str("<item t=\"default\"/>");
        out
    };
    let mut pfields = String::new();
    for i in 0..n_fields {
        if i == row_i {
            let _ = write!(
                pfields,
                "<pivotField axis=\"axisRow\" compact=\"0\" outline=\"0\" showAll=\"0\"><items count=\"{}\">{}</items></pivotField>",
                shared[i].len() + 1,
                items_of(i)
            );
        } else if Some(i) == col_i {
            let _ = write!(
                pfields,
                "<pivotField axis=\"axisCol\" compact=\"0\" outline=\"0\" showAll=\"0\"><items count=\"{}\">{}</items></pivotField>",
                shared[i].len() + 1,
                items_of(i)
            );
        } else if i == val_i {
            pfields.push_str("<pivotField dataField=\"1\" compact=\"0\" outline=\"0\" showAll=\"0\"/>");
        } else {
            pfields.push_str("<pivotField compact=\"0\" outline=\"0\" showAll=\"0\"/>");
        }
    }
    let loc = format!(
        "{}{}:{}{}",
        col_name(out_rect.1),
        out_rect.0 + 1,
        col_name(out_rect.3),
        out_rect.2 + 1
    );
    let mut extra = String::new();
    match col_i {
        Some(ci) => {
            let _ = write!(
                extra,
                "<colFields count=\"1\"><field x=\"{ci}\"/></colFields><colItems count=\"{col_count}\">{col_items}</colItems>"
            );
        }
        None => {
            let _ = write!(extra, "<colItems count=\"1\">{col_items}</colItems>");
        }
    }
    let label = value_label.trim();
    let short = label.rsplit_once(':').map(|(_, tail)| tail).unwrap_or(label);
    let (first_header_row, first_data_row) = if col_i.is_some() { ("2", "3") } else { ("1", "2") };
    let table = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotTableDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" name=\"{}\" cacheId=\"{cache_id}\" dataCaption=\"值\" updatedVersion=\"6\" minRefreshableVersion=\"3\" createdVersion=\"6\" applyWidthHeightFormats=\"1\" useAutoFormatting=\"1\" itemPrintTitles=\"1\" indent=\"0\" outline=\"1\" outlineData=\"1\" compact=\"0\" compactData=\"0\" multipleFieldFilters=\"0\"><location ref=\"{loc}\" firstHeaderRow=\"{first_header_row}\" firstDataRow=\"{first_data_row}\" firstDataCol=\"1\"/><pivotFields count=\"{n_fields}\">{pfields}</pivotFields><rowFields count=\"1\"><field x=\"{row_i}\"/></rowFields><rowItems count=\"{}\">{row_items}</rowItems>{extra}<dataFields count=\"1\"><dataField name=\"{}{}\" fld=\"{val_i}\" baseField=\"0\" baseItem=\"0\"/></dataFields><pivotTableStyleInfo name=\"PivotStyleLight16\" showRowHeaders=\"1\" showColHeaders=\"1\" showRowStripes=\"0\" showColStripes=\"0\" showLastColumn=\"1\"/></pivotTableDefinition>",
        esc(table_name),
        res.rows.len(),
        agg_label(spec.agg),
        esc(short)
    );

    if table.contains("求和项:求和项:") {
        anyhow::bail!("值字段标签重复加了聚合前缀");
    }

    Ok((cache, records, table))
}
