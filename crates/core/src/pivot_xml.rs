//! 真正的 Excel 数据透视表部件（可刷新、可拖字段）。
//!
//! 需要三个部分：
//! 1. `xl/pivotCache/pivotCacheDefinitionN.xml` —— 数据源与字段缓存
//! 2. `xl/pivotTables/pivotTableN.xml` —— 版面、行/列/值字段
//! 3. 关系与内容类型：workbook 的 `<pivotCaches>`、两处 rels、`[Content_Types]`
//!
//! 结果区仍然由我们写一份「值」，这样即使 Excel 不刷新也能看到数字；
//! `refreshOnLoad="1"` 让 Excel 打开时自己重算。

use crate::pivot::{Agg, PivotResult, PivotSpec};
use anyhow::Result;
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
        Agg::Sum => "求和项",
        Agg::Count => "计数项",
        Agg::Avg => "平均值项",
        Agg::Max => "最大值项",
        Agg::Min => "最小值项",
    }
}

/// 在已保存的文件上追加透视表部件；返回 cacheId
pub fn attach(
    path: &Path,
    sheet_name: &str,
    spec: &PivotSpec,
    res: &PivotResult,
    out_rect: (u32, u32, u32, u32),
    value_label: &str,
) -> Result<u32> {
    // 现有 cacheId 数一下，避免撞号
    let (cache_id, table_idx) = next_ids(path)?;
    let src = format!(
        "{}{}:{}{}",
        col_name(spec.source.1),
        spec.source.0 + 1,
        col_name(spec.source.3),
        spec.source.2 + 1
    );
    let n_fields = (spec.source.3 - spec.source.1 + 1) as usize;
    let headers: Vec<String> = res.headers.clone();

    // ---------- 字段缓存 ----------
    let mut caches = String::new();
    for i in 0..n_fields {
        let name = headers.get(i).cloned().unwrap_or_else(|| format!("字段{i}"));
        if i as u32 == spec.value_field - spec.source.1 {
            // 数值字段要带 min/max，Excel 用来定刻度
            let (min, max) = numeric_range(res, i);
            caches.push_str(&format!(
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems containsSemiMixedTypes=\"0\" containsString=\"0\" containsNumber=\"1\"{}{}/></cacheField>",
                esc(&name),
                min.map(|v| format!(" minValue=\"{v}\"")).unwrap_or_default(),
                max.map(|v| format!(" maxValue=\"{v}\"")).unwrap_or_default()
            ));
        } else {
            caches.push_str(&format!(
                "<cacheField name=\"{}\" numFmtId=\"0\"><sharedItems containsSemiMixedTypes=\"0\" containsString=\"1\" containsBlank=\"1\"/></cacheField>",
                esc(&name)
            ));
        }
    }
    let def = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<pivotCacheDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
r:id=\"rId1\" refreshOnLoad=\"1\" refreshedBy=\"sheetcore\" recordCount=\"{}\">\
<cacheSource type=\"worksheet\"><worksheetSource ref=\"{src}\" sheet=\"{}\"/></cacheSource>\
<cacheFields count=\"{n_fields}\">{caches}</cacheFields>\
</pivotCacheDefinition>",
        res.rows.len(),
        esc(sheet_name)
    );

    // ---------- 透视表定义 ----------
    let row_i = (spec.row_field - spec.source.1) as usize;
    let val_i = (spec.value_field - spec.source.1) as usize;
    let mut pfields = String::new();
    for i in 0..n_fields {
        if i == row_i {
            pfields.push_str("<pivotField axis=\"axisRow\" showAll=\"0\" outline=\"1\"/>");
        } else if Some(i as u32) == spec.col_field.map(|c| c - spec.source.1) {
            pfields.push_str("<pivotField axis=\"axisCol\" showAll=\"0\" outline=\"1\"/>");
        } else if i == val_i {
            pfields.push_str("<pivotField dataField=\"1\" showAll=\"0\"/>");
        } else {
            pfields.push_str("<pivotField showAll=\"0\"/>");
        }
    }
    let r2 = out_rect.2;
    let c2 = out_rect.3;
    let loc = format!(
        "{}{}:{}{}",
        col_name(out_rect.1),
        out_rect.0 + 1,
        col_name(c2),
        r2 + 1
    );
    let mut extra = String::new();
    if let Some(cf) = spec.col_field {
        extra.push_str(&format!(
            "<colFields count=\"1\"><field x=\"{}\"/></colFields><colItems count=\"1\"><i/></colItems>",
            cf - spec.source.1
        ));
    } else {
        extra.push_str("<colItems count=\"1\"><i/></colItems>");
    }
    // rowItems：每组一行，最后一行是总计（t="grand"）
    let mut items = String::new();
    for i in 0..res.rows.len() {
        if i + 1 == res.rows.len() {
            items.push_str("<i t=\"grand\"><x/></i>");
        } else {
            items.push_str(&format!("<i><x v=\"{i}\"/></i>"));
        }
    }
    let def_table = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<pivotTableDefinition xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
name=\"数据透视表{table_idx}\" cacheId=\"{cache_id}\" dataCaption=\"值\" \
updatedVersion=\"6\" minRefreshableVersion=\"3\" createdVersion=\"6\" \
applyWidthHeightFormats=\"1\" useAutoFormatting=\"1\" itemPrintTitles=\"1\" \
indent=\"0\" outline=\"1\" outlineData=\"1\" multipleFieldFilters=\"0\">\
<location ref=\"{loc}\" firstHeaderRow=\"1\" firstDataRow=\"1\" firstDataCol=\"1\"/>\
<pivotFields count=\"{n_fields}\">{pfields}</pivotFields>\
<rowFields count=\"1\"><field x=\"{row_i}\"/></rowFields>\
<rowItems count=\"{}\">{items}</rowItems>\
{extra}\
<dataFields count=\"1\"><dataField name=\"{}:{}\" fld=\"{val_i}\" baseField=\"0\" baseItem=\"0\"/></dataFields>\
<pivotTableStyleInfo name=\"PivotStyleLight16\" showRowHeaders=\"1\" showColHeaders=\"1\" showRowStripes=\"0\" showColStripes=\"0\" showLastColumn=\"1\"/>\
</pivotTableDefinition>",
        res.rows.len(),
        agg_label(spec.agg),
        esc(value_label)
    );

    // ---------- 写 zip ----------
    let cache_part = format!("xl/pivotCache/pivotCacheDefinition{cache_id}.xml");
    let table_part = format!("xl/pivotTables/pivotTable{table_idx}.xml");
    let cache_rels = format!("xl/pivotCache/_rels/pivotCacheDefinition{cache_id}.xml.rels");
    let cache_conn = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheRecords\" Target=\"pivotCacheRecords{cache_id}.xml\"/>\
</Relationships>"
    );
    let records = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<pivotCacheRecords xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" count=\"0\"/>"
    );
    let records_part = format!("xl/pivotCache/pivotCacheRecords{cache_id}.xml");

    crate::patch::append_entry(path, &cache_part, def.as_bytes())?;
    crate::patch::append_entry(path, &records_part, records.as_bytes())?;
    crate::patch::append_entry(path, &cache_rels, cache_conn.as_bytes())?;
    crate::patch::append_entry(path, &table_part, def_table.as_bytes())?;

    // workbook.xml: <pivotCaches>
    let cache_rid = format!("rIdPivotCache{cache_id}");
    let table_rid = format!("rIdPivotTable{table_idx}");
    crate::patch::rewrite_zip(path, |name, buf| {
        if name == "[Content_Types].xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
                if !x.contains(&cache_part) {
                    x = x.replace(
                        "</Types>",
                        &format!(
                            "<Override PartName=\"/{cache_part}\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml\"/>\
<Override PartName=\"/{records_part}\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheRecords+xml\"/>\
<Override PartName=\"/{table_part}\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml\"/></Types>"
                        ),
                    );
                }
                return x.into_bytes();
            }
        }
        if name == "xl/workbook.xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
                if !x.contains("<pivotCaches>") && !x.contains("<pivotCaches/>") {
                    let entry = format!("<pivotCaches><pivotCache cacheId=\"{cache_id}\" r:id=\"{cache_rid}\"/></pivotCaches>");
                    // schema 顺序：calcPr 之前
                    if let Some(pos) = x.find("<calcPr") {
                        x.insert_str(pos, &entry);
                    } else if let Some(pos) = x.find("</workbook>") {
                        x.insert_str(pos, &entry);
                    }
                }
                return x.into_bytes();
            }
        }
        if name == "xl/_rels/workbook.xml.rels" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                if !xml.contains(&cache_part) {
                    let add = format!(
                        "<Relationship Id=\"{cache_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition\" Target=\"pivotCache/pivotCacheDefinition{cache_id}.xml\"/>"
                    );
                    return xml.replace("</Relationships>", &format!("{add}</Relationships>")).into_bytes();
                }
            }
        }
        buf
    })?;

    // 工作表 rels 指向 pivotTable 部件；原本没有 rels 就新建一个
    let add = format!(
        "<Relationship Id=\"{table_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable\" Target=\"../pivotTables/pivotTable{table_idx}.xml\"/>"
    );
    if let Some(rels) = sheet_rels_path(path, sheet_name) {
        crate::patch::rewrite_zip(path, |name, buf| {
            if name == rels {
                if let Ok(xml) = String::from_utf8(buf.clone()) {
                    if !xml.contains("pivotTable") {
                        return xml.replace("</Relationships>", &format!("{add}</Relationships>")).into_bytes();
                    }
                }
            }
            buf
        })?;
    } else {
        // 没有任何关系：按 schema 造一个
        if let Some((rels_name, sheet_file)) = sheet_rel_names(path, sheet_name) {
            let body = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{add}</Relationships>"
            );
            let _ = sheet_file;
            crate::patch::append_entry(path, &rels_name, body.as_bytes())?;
        }
    }
    Ok(cache_id)
}

fn numeric_range(res: &PivotResult, col: usize) -> (Option<f64>, Option<f64>) {
    let mut min: Option<f64> = None;
    let mut max: Option<f64> = None;
    for r in res.rows.iter().take(res.rows.len().saturating_sub(1)) {
        if let Some(crate::model::Value::Number(v)) = r.get(col).cloned() {
            min = Some(min.map_or(v, |m: f64| m.min(v)));
            max = Some(max.map_or(v, |m: f64| m.max(v)));
        }
    }
    (min, max)
}

/// 下一个可用的 cacheId / 表序号
fn next_ids(path: &Path) -> Result<(u32, u32)> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut cache = 0u32;
    let mut table = 0u32;
    for i in 0..zip.len() {
        let name = zip.by_index(i)?.name().to_string();
        if name.starts_with("xl/pivotCache/pivotCacheDefinition") {
            cache += 1;
        } else if name.starts_with("xl/pivotTables/pivotTable") {
            table += 1;
        }
    }
    let mut buf = String::new();
    if let Ok(mut f) = zip.by_name("xl/workbook.xml") {
        let _ = f.read_to_string(&mut buf);
    }
    // cacheId 必须全局唯一：用最大值 +1
    let mut max_id = cache;
    for seg in buf.split("cacheId=\"").skip(1) {
        if let Some(end) = seg.find('"') {
            if let Ok(v) = seg[..end].parse::<u32>() {
                max_id = max_id.max(v);
            }
        }
    }
    Ok((max_id.max(1), table + 1))
}

fn sheet_rels_path(path: &Path, sheet_name: &str) -> Option<String> {
    let (rels, _) = sheet_rel_names(path, sheet_name)?;
    // 只有真的存在才算
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    zip.by_name(&rels).ok()?;
    Some(rels)
}

/// 工作表 rels 的路径（不管存不存在）
fn sheet_rel_names(path: &Path, sheet_name: &str) -> Option<(String, String)> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut wb = String::new();
    zip.by_name("xl/workbook.xml").ok()?.read_to_string(&mut wb).ok()?;
    let mut rels = String::new();
    zip.by_name("xl/_rels/workbook.xml.rels").ok()?.read_to_string(&mut rels).ok()?;
    let key = format!("name=\"{}\"", esc(sheet_name));
    let pos = wb.find(&key)?;
    let tail = &wb[pos..];
    let rid_pos = tail.find("r:id=\"")? + 6;
    let rid_end = tail[rid_pos..].find('"')? + rid_pos;
    let rid = &tail[rid_pos..rid_end];
    let needle = format!("Id=\"{rid}\"");
    let rpos = rels.find(&needle)?;
    let tpos = rels[rpos..].find("Target=\"")? + rpos + 8;
    let tend = rels[tpos..].find('"')? + tpos;
    let target = &rels[tpos..tend];
    let file_name = target.rsplit('/').next()?.to_string();
    let rels_name = format!("xl/worksheets/_rels/{file_name}.rels");
    Some((rels_name, file_name))
}
