//! 多字段透视（`PivotSpec2`）→ 真 `pivotTable` 部件。
//!
//! 结构黄金样本来自 Excel 自己造的三份文件（`scripts/c12_make_pivots_multi.ps1`）：
//! - A：2 行字段 + 1 值，无列字段 —— rowItems 每行一个 `<x>`/层，组尾 `<i t="default"><x/></i>`；
//! - B：1 行 + 1 列 + 2 值 —— `colFields` 末尾加 `<field x="-2"/>`（数据字段伪字段），
//!   每个列路径产出 K 个 `<i>`，第 2 个起带 `i="1"`；
//! - C：2 行 + 1 列 + 1 值 —— 列维度按叶子标签逐个 `<i>`，末尾 `<i t="grand"><x/></i>`。
//!
//! 只做「缓存与定义自洽」不够，Excel 会因为 `rowItems/colItems` 与 `sharedItems` 对不上
//! 而静默丢掉整张透视表，所以这里所有下标都由同一份 `sharedItems` 生成。

use crate::model::{col_label, Value};
use crate::pivot::Agg;
use crate::pivot_multi::{PivotGrid, PivotSpec2};
use anyhow::{bail, Context, Result};
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

fn idx_of(shared: &mut Vec<String>, v: &str) -> usize {
    match shared.iter().position(|s| s == v) {
        Some(i) => i,
        None => {
            shared.push(v.to_string());
            shared.len() - 1
        }
    }
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
    if spec.cols.len() > 1 {
        bail!("多字段透视暂只支持 0 或 1 个列字段");
    }
    if spec.values.is_empty() {
        bail!("多字段透视至少需要一个值字段");
    }
    if spec.cols.is_empty() && spec.values.len() > 1 {
        bail!("多值透视必须带列字段（Excel 用 -2 数据伪字段表达）");
    }

    let (r1, c1, r2, c2) = spec.source;
    let wb = crate::xlsx::load(path).context("透视表：读取源工作簿失败")?;
    let sheet = wb
        .sheets
        .iter()
        .find(|s| s.name == sheet_name)
        .with_context(|| format!("透视表：源工作表 {sheet_name} 不存在"))?;

    // ---------- 缓存：整块源区都要进缓存（Excel 自己也是这样，未用的列也在）----------
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
    let mut numeric = vec![false; n_fields];
    let mut bounds: Vec<(Option<f64>, Option<f64>)> = vec![(None, None); n_fields];
    for i in 0..n_fields {
        let col = c1 + i as u32;
        let is_value = value_cols.contains(&col);
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
                    if !is_value {
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
        numeric[i] = is_value && saw_num;
        if numeric[i] {
            shared[i].clear();
        }
    }

    // ---------- 行轴 ----------
    let row_depth = spec.rows.len();
    let row_fields: Vec<usize> = spec.rows.iter().map(|f| (f.col - c1) as usize).collect();
    let mut row_items = String::new();
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
            // 压缩形式下深层标签可能为空：沿用上一行同层标签，保证下标始终有效
            let label = if labels[d].is_empty() {
                prev_labels[d].clone()
            } else {
                labels[d].clone()
            };
            let fi = row_fields[d];
            let idx = idx_of(&mut shared[fi], &label);
            row_items.push_str(&format!("<x v=\"{idx}\"/>"));
        }
        row_items.push_str("</i>");
        prev_labels = labels;
    }

    // ---------- 列轴 ----------
    let k = spec.values.len();
    let mut col_items = String::new();
    let mut col_count = 1usize;
    let mut extra = String::new();
    match spec.cols.first() {
        Some(cf) => {
            let ci = (cf.col - c1) as usize;
            let labels: Vec<String> = grid.col_labels.clone();
            if labels.is_empty() {
                bail!("多字段透视：列字段没有任何叶子标签");
            }
            for label in &labels {
                let idx = idx_of(&mut shared[ci], label);
                for vi in 0..k {
                    let data_attr = if vi == 0 {
                        String::new()
                    } else {
                        format!(" i=\"{vi}\"")
                    };
                    col_items.push_str(&format!("<i{data_attr}><x v=\"{idx}\"/></i>"));
                }
            }
            for vi in 0..k {
                let data_attr = if vi == 0 {
                    String::new()
                } else {
                    format!(" i=\"{vi}\"")
                };
                col_items.push_str(&format!("<i t=\"grand\"{data_attr}><x/></i>"));
            }
            col_count = labels.len() * k + k;
            let fields = if k > 1 {
                format!("<colFields count=\"2\"><field x=\"{ci}\"/><field x=\"-2\"/></colFields>")
            } else {
                format!("<colFields count=\"1\"><field x=\"{ci}\"/></colFields>")
            };
            extra.push_str(&format!(
                "{fields}<colItems count=\"{col_count}\">{col_items}</colItems>"
            ));
        }
        None => {
            col_items.push_str("<i/>");
            extra.push_str("<colItems count=\"1\"><i/></colItems>");
        }
    }

    // ---------- 缓存字段与记录 ----------
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

    let src = format!("{}{}:{}{}", col_name(c1), r1 + 1, col_name(c2), r2 + 1);
    let cache = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotCacheDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"rId1\" refreshOnLoad=\"1\" refreshedBy=\"{}\" recordCount=\"{n_records}\"><cacheSource type=\"worksheet\"><worksheetSource ref=\"{src}\" sheet=\"{}\"/></cacheSource><cacheFields count=\"{n_fields}\">{caches}</cacheFields></pivotCacheDefinition>",
        esc(refreshed_by),
        esc(sheet_name)
    );
    let records = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotCacheRecords xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" count=\"{n_records}\">{recs}</pivotCacheRecords>"
    );

    // ---------- pivotFields ----------
    let items_for = |shared: &Vec<String>| -> String {
        let mut out = String::new();
        for k in 0..shared.len() {
            out.push_str(&format!("<item x=\"{k}\"/>"));
        }
        out.push_str("<item t=\"default\"/>");
        out
    };
    let col_i = spec.cols.first().map(|f| (f.col - c1) as usize);
    let mut pfields = String::new();
    for i in 0..n_fields {
        if row_fields.contains(&i) {
            pfields.push_str(&format!(
                "<pivotField axis=\"axisRow\" compact=\"0\" outline=\"0\" showAll=\"0\"><items count=\"{}\">{}</items></pivotField>",
                shared[i].len() + 1,
                items_for(&shared[i])
            ));
        } else if Some(i) == col_i {
            pfields.push_str(&format!(
                "<pivotField axis=\"axisCol\" compact=\"0\" outline=\"0\" showAll=\"0\"><items count=\"{}\">{}</items></pivotField>",
                shared[i].len() + 1,
                items_for(&shared[i])
            ));
        } else if value_cols.contains(&(c1 + i as u32)) {
            pfields.push_str("<pivotField dataField=\"1\" compact=\"0\" outline=\"0\" showAll=\"0\"/>");
        } else {
            pfields.push_str("<pivotField compact=\"0\" outline=\"0\" showAll=\"0\"/>");
        }
    }

    // ---------- 表头/位置 ----------
    let n_head = if spec.cols.is_empty() { 1 } else { 2 };
    let first_data_row = if spec.cols.is_empty() {
        1
    } else if k > 1 {
        3
    } else {
        2
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
        .enumerate()
        .map(|(vi, v)| {
            let label = v
                .label
                .clone()
                .unwrap_or_else(|| format!("{}{}", agg_label(v.agg), names[(v.col - c1) as usize].clone()));
            let short = label.rsplit_once(':').map(|(_, t)| t.to_string()).unwrap_or(label);
            format!(
                "<dataField name=\"{}{}\" fld=\"{}\" baseField=\"0\" baseItem=\"0\"/>",
                agg_label(v.agg),
                esc(&short),
                v.col - c1
            )
        })
        .collect();

    let table = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><pivotTableDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" name=\"{}\" cacheId=\"{cache_id}\" dataCaption=\"值\" updatedVersion=\"6\" minRefreshableVersion=\"3\" createdVersion=\"6\" applyWidthHeightFormats=\"1\" useAutoFormatting=\"1\" itemPrintTitles=\"1\" indent=\"0\" outline=\"1\" outlineData=\"1\" compact=\"0\" compactData=\"0\" multipleFieldFilters=\"0\"><location ref=\"{loc}\" firstHeaderRow=\"1\" firstDataRow=\"{first_data_row}\" firstDataCol=\"{row_depth}\"/><pivotFields count=\"{n_fields}\">{pfields}</pivotFields><rowFields count=\"{row_depth}\">{row_fields_xml}</rowFields><rowItems count=\"{}\">{row_items}</rowItems>{extra}<dataFields count=\"{k}\">{data_fields}</dataFields><pivotTableStyleInfo name=\"PivotStyleLight16\" showRowHeaders=\"1\" showColHeaders=\"1\" showRowStripes=\"0\" showColStripes=\"0\" showLastColumn=\"1\"/></pivotTableDefinition>",
        esc(table_name),
        grid.rows.len()
    );

    // 自检：条数与索引自洽
    let rec_count = records.matches("<r>").count();
    if rec_count != n_records {
        bail!("透视缓存记录数不一致：<r>={rec_count} recordCount={n_records}");
    }
    let _ = n_head;
    Ok((cache, records, table))
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
