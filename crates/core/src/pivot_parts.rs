//! 生成 Excel 能直接当「可编辑数据透视表」打开的三份 XML：
//! `pivotCacheDefinition`、`pivotCacheRecords`、`pivotTableDefinition`。
//!
//! 关键点（旧实现就栽在这三处，Excel 打开会修复或直接崩）：
//! 1. `cacheField@name` 必须是**源表表头**，不是透视输出的列标题；
//! 2. `pivotCacheRecords` 必须有真实记录，条数与 `recordCount` 一致；
//! 3. `rowItems/colItems` 里的 `<x v="n"/>` 必须指向该字段 `sharedItems` 中真实存在的下标。
//!
//! 结果区仍由调用方写一份值，`refreshOnLoad="1"` 让 Excel 打开时自己重算。

use crate::model::{col_label, Value};
use crate::pivot::{Agg, PivotResult, PivotSpec};
use anyhow::{Context, Result};
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
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn agg_label(a: Agg) -> &'static str {
    match a {
        Agg::Sum => "求和项:",
        Agg::Count => "计数项:",
        Agg::Avg => "平均值项:",
        Agg::Max => "最大值项:",
        Agg::Min => "最小值项:",
    }
}

/// 返回 (pivotCacheDefinition, pivotCacheRecords, pivotTableDefinition)
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
    let (r1, c1, r2, c2) = spec.source;
    let wb = crate::xlsx::load(path).context("透视表：读取源工作簿失败")?;
    let sheet = wb
        .sheets
        .iter()
        .find(|s| s.name == sheet_name)
        .with_context(|| format!("透视表：源工作表 {sheet_name} 不存在"))?;

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

    // 2) 每个字段的去重取值（值字段走数值，不用 sharedItems）
    let mut shared: Vec<Vec<String>> = vec![Vec::new(); n_fields];
    let mut numeric = vec![false; n_fields];
    let mut bounds: Vec<(Option<f64>, Option<f64>)> = vec![(None, None); n_fields];
    for i in 0..n_fields {
        let col = c1 + i as u32;
        let mut saw_num = false;
        for r in (r1 + 1)..=r2 {
            match sheet.get(r, col).map(|c| c.value.clone()) {
                None | Some(Value::Empty) => {}
                Some(Value::Number(v)) => {
                    saw_num = true;
                    let (lo, hi) = bounds[i];
                    bounds[i] = (
                        Some(lo.map_or(v, |x: f64| x.min(v))),
                        Some(hi.map_or(v, |x: f64| x.max(v))),
                    );
                    if i != val_i {
                        let s = Value::Number(v).display();
                        if !shared[i].iter().any(|x| *x == s) {
                            shared[i].push(s);
                        }
                    }
                }
                Some(other) => {
                    let s = other.display();
                    if !s.is_empty() && !shared[i].iter().any(|x| *x == s) {
                        shared[i].push(s);
                    }
                }
            }
        }
        numeric[i] = i == val_i && saw_num;
        if numeric[i] {
            shared[i].clear();
        }
    }

    // 3) 结果里的行/列标签必须在 sharedItems 中有下标
    let mut row_items = String::new();
    for (i, line) in res.rows.iter().enumerate() {
        let label = line.first().map(|v| v.display()).unwrap_or_default();
        let is_grand = i + 1 == res.rows.len()
            && res.rows.len() > 1
            && (label == "总计" || label == "Grand Total");
        if is_grand {
            row_items.push_str("<i t=\"grand\"><x/></i>");
        } else {
            let idx = match shared[row_i].iter().position(|s| *s == label) {
                Some(pos) => pos,
                None => {
                    shared[row_i].push(label.clone());
                    shared[row_i].len() - 1
                }
            };
            row_items.push_str(&format!("<i><x v=\"{idx}\"/></i>"));
        }
    }
    let mut col_items = String::new();
    let mut col_count = 1usize;
    match col_i {
        Some(ci) => {
            if res.headers.len() > 1 {
                col_count = res.headers.len() - 1;
                for h in res.headers.iter().skip(1) {
                    let idx = match shared[ci].iter().position(|s| s == h) {
                        Some(pos) => pos,
                        None => {
                            shared[ci].push(h.clone());
                            shared[ci].len() - 1
                        }
                    };
                    col_items.push_str(&format!("<i><x v=\"{idx}\"/></i>"));
                }
            } else {
                col_count = 1;
                col_items.push_str("<i t=\"grand\"><x/></i>");
            }
        }
        None => col_items.push_str("<i/>"),
    }

    // 4) 缓存字段
    let mut caches = String::new();
    for i in 0..n_fields {
        if numeric[i] {
            let (lo, hi) = bounds[i];
            caches.push_str(&format!(
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems containsSemiMixedTypes=\"0\" containsString=\"0\" containsNumber=\"1\"{}{}/></cacheField>",
                esc(&names[i]),
                lo.map(|v| format!(" minValue=\"{v}\"")).unwrap_or_default(),
                hi.map(|v| format!(" maxValue=\"{v}\"")).unwrap_or_default()
            ));
        } else {
            let items: String = shared[i]
                .iter()
                .map(|s| format!("<s v=\"{}\"/>", esc(s)))
                .collect();
            caches.push_str(&format!(
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems count=\"{}\">{items}</sharedItems></cacheField>",
                esc(&names[i]),
                shared[i].len()
            ));
        }
    }

    // 5) 缓存记录
    let mut recs = String::new();
    let mut n_records = 0usize;
    for r in (r1 + 1)..=r2 {
        n_records += 1;
        recs.push_str("<r>");
        for i in 0..n_fields {
            let col = c1 + i as u32;
            let v = sheet
                .get(r, col)
                .map(|c| c.value.clone())
                .unwrap_or(Value::Empty);
            if numeric[i] {
                match v {
                    Value::Number(n) => recs.push_str(&format!("<n v=\"{n}\"/>")),
                    Value::Empty => recs.push_str("<m/>"),
                    other => recs.push_str(&format!("<s v=\"{}\"/>", esc(&other.display()))),
                }
            } else {
                let key = v.display();
                match shared[i].iter().position(|s| *s == key) {
                    Some(idx) if !key.is_empty() => recs.push_str(&format!("<x v=\"{idx}\"/>")),
                    _ => recs.push_str("<m/>"),
                }
            }
        }
        recs.push_str("</r>");
    }

    let src = format!(
        "{}{}:{}{}",
        col_name(c1),
        r1 + 1,
        col_name(c2),
        r2 + 1
    );
    let cache = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotCacheDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"rId1\" refreshOnLoad=\"1\" refreshedBy=\"{}\" recordCount=\"{n_records}\"><cacheSource type=\"worksheet\"><worksheetSource ref=\"{src}\" sheet=\"{}\"/></cacheSource><cacheFields count=\"{n_fields}\">{caches}</cacheFields></pivotCacheDefinition>",
        esc(refreshed_by),
        esc(sheet_name)
    );
    let records = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotCacheRecords xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" count=\"{n_records}\">{recs}</pivotCacheRecords>"
    );

    // 6) 透视表定义
    let items_of = |field: usize| -> String {
        let mut out = String::new();
        for k in 0..shared[field].len() {
            out.push_str(&format!("<item x=\"{k}\"/>"));
        }
        out.push_str("<item t=\"default\"/>");
        out
    };
    let mut pfields = String::new();
    for i in 0..n_fields {
        if i == row_i {
            pfields.push_str(&format!(
                "<pivotField axis=\"axisRow\" compact=\"0\" outline=\"0\" showAll=\"0\"><items count=\"{}\">{}</items></pivotField>",
                shared[i].len() + 1,
                items_of(i)
            ));
        } else if Some(i) == col_i {
            pfields.push_str(&format!(
                "<pivotField axis=\"axisCol\" compact=\"0\" outline=\"0\" showAll=\"0\"><items count=\"{}\">{}</items></pivotField>",
                shared[i].len() + 1,
                items_of(i)
            ));
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
        Some(ci) => extra.push_str(&format!(
            "<colFields count=\"1\"><field x=\"{ci}\"/></colFields><colItems count=\"{col_count}\">{col_items}</colItems>"
        )),
        None => extra.push_str(&format!("<colItems count=\"1\">{col_items}</colItems>")),
    }
    // 值字段标签：调用方可能已经带了「求和项:」，去掉再拼，避免「求和项:求和项:金额」
    let label = value_label.trim();
    let short = label
        .rsplit_once(':')
        .map(|(_, tail)| tail)
        .unwrap_or(label);
    let table = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotTableDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" name=\"{}\" cacheId=\"{cache_id}\" dataCaption=\"值\" updatedVersion=\"6\" minRefreshableVersion=\"3\" createdVersion=\"6\" applyWidthHeightFormats=\"1\" useAutoFormatting=\"1\" itemPrintTitles=\"1\" indent=\"0\" outline=\"1\" outlineData=\"1\" compact=\"0\" compactData=\"0\" multipleFieldFilters=\"0\"><location ref=\"{loc}\" firstHeaderRow=\"1\" firstDataRow=\"2\" firstDataCol=\"1\"/><pivotFields count=\"{n_fields}\">{pfields}</pivotFields><rowFields count=\"1\"><field x=\"{row_i}\"/></rowFields><rowItems count=\"{}\">{row_items}</rowItems>{extra}<dataFields count=\"1\"><dataField name=\"{}{}\" fld=\"{val_i}\" baseField=\"0\" baseItem=\"0\"/></dataFields><pivotTableStyleInfo name=\"PivotStyleLight16\" showRowHeaders=\"1\" showColHeaders=\"1\" showRowStripes=\"0\" showColStripes=\"0\" showLastColumn=\"1\"/></pivotTableDefinition>",
        esc(table_name),
        res.rows.len(),
        agg_label(spec.agg),
        esc(short)
    );

    // 自检：这些不一致会让 Excel 默默丢掉整张透视表（或直接弹修复）
    let rec_count = records.matches("<r>").count();
    if rec_count != n_records || !records.contains(&format!("count=\"{n_records}\"")) {
        anyhow::bail!("透视缓存记录数不一致：<r>={rec_count} recordCount={n_records}");
    }
    // 行/列字段的条目在下标补齐时已保证存在于 sharedItems 中；
    // 源区读空时（如工作表被挪到非标准路径）宁可生成空透视表，也不要整份附挂失败。
    if table.contains("求和项:求和项:") {
        anyhow::bail!("值字段标签重复加了聚合前缀");
    }

    Ok((cache, records, table))
}
