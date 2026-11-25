//! 从 `.xlsx` 包内解析既有的 `xl/pivotTables/pivotTable*.xml` 与 `xl/pivotCache/pivotCacheDefinition*.xml`，
//! 还原为 CELLAN 原生可交互、可编辑、可刷新、可下钻的 [`PivotTableModel`]。
//!
//! 兼容 Excel、WPS 以及 CELLAN 自身保存的数据透视表；解析容错，单个不支持的高级特性不会阻断打开。

use crate::model::Workbook;
use crate::pivot::Agg;
use crate::pivot_engine::{
    compute_table, AxisFieldExt, FieldSortMode, PivotStyleTheme, PivotTableModel, PivotTableOptions,
    ReportLayout, SubtotalPos, ValueFieldExt,
};
use crate::pivot_multi::{AxisField, AxisSort, FilterField, PivotLayout, PivotSpec2, ShowAs, ValueField};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::{Read, Seek};

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .map(|a| crate::model::xml_unescape(&a.value))
}

fn rel_id_attr(e: &BytesStart<'_>) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref().ends_with(":id"))
        .map(|a| crate::model::xml_unescape(&a.value))
}

fn rels_path(part: &str) -> String {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    if dir.is_empty() {
        format!("_rels/{file}.rels")
    } else {
        format!("{dir}/_rels/{file}.rels")
    }
}

fn resolve_target(owner: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        owner
            .rsplit_once('/')
            .map(|(dir, _)| dir.split('/').filter(|s| !s.is_empty()).collect())
            .unwrap_or_default()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn read_zip_part<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<Vec<u8>> {
    let mut entry = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn parse_rels(xml: &[u8], owner: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut reader = Reader::from_reader(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if e.local_name().as_ref() == "Relationship" =>
            {
                if let (Some(id), Some(kind), Some(target)) =
                    (attr(&e, "Id"), attr(&e, "Type"), attr(&e, "Target"))
                {
                    if attr(&e, "TargetMode").as_deref() != Some("External") {
                        out.push((id, kind, resolve_target(owner, &target)));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn parse_cell_a1(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut col: u32 = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        let v = (bytes[i].to_ascii_uppercase() - b'A') as u32 + 1;
        col = col.checked_mul(26)?.checked_add(v)?;
        i += 1;
    }
    if col == 0 || i >= bytes.len() {
        return None;
    }
    let row: u32 = s[i..].parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

pub fn parse_a1_range(raw: &str) -> Option<(u32, u32, u32, u32)> {
    let clean = raw.trim().replace('$', "");
    if let Some((a, b)) = clean.split_once(':') {
        let (r1, c1) = parse_cell_a1(a)?;
        let (r2, c2) = parse_cell_a1(b)?;
        Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
    } else {
        let (r, c) = parse_cell_a1(&clean)?;
        Some((r, c, r, c))
    }
}

#[derive(Clone)]
struct ParsedCache {
    source_sheet: String,
    source_rect: (u32, u32, u32, u32),
    /// 每个缓存字段的 (名称, sharedItems 字符串表, 可选公式)
    fields: Vec<(String, Vec<String>, Option<String>)>,
}

fn parse_cache_def(xml: &[u8]) -> Option<ParsedCache> {
    let mut reader = Reader::from_reader(xml);
    let mut source_sheet = String::new();
    let mut source_rect: Option<(u32, u32, u32, u32)> = None;
    let mut fields: Vec<(String, Vec<String>, Option<String>)> = Vec::new();
    let mut in_shared = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "worksheetSource" => {
                    if let Some(s) = attr(&e, "sheet") {
                        source_sheet = s;
                    }
                    if let Some(rf) = attr(&e, "ref") {
                        source_rect = parse_a1_range(&rf);
                    }
                }
                "cacheField" => {
                    let name = attr(&e, "name").unwrap_or_default();
                    let formula = attr(&e, "formula");
                    fields.push((name, Vec::new(), formula));
                }
                "sharedItems" => {
                    in_shared = true;
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                "worksheetSource" => {
                    if let Some(s) = attr(&e, "sheet") {
                        source_sheet = s;
                    }
                    if let Some(rf) = attr(&e, "ref") {
                        source_rect = parse_a1_range(&rf);
                    }
                }
                "cacheField" => {
                    let name = attr(&e, "name").unwrap_or_default();
                    let formula = attr(&e, "formula");
                    fields.push((name, Vec::new(), formula));
                }
                "s" | "n" | "d" | "b" if in_shared => {
                    if let Some((_, items, _)) = fields.last_mut() {
                        let v = attr(&e, "v").unwrap_or_default();
                        items.push(v);
                    }
                }
                "m" if in_shared => {
                    if let Some((_, items, _)) = fields.last_mut() {
                        items.push("(空白)".into());
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => {
                if e.local_name().as_ref() == "sharedItems" {
                    in_shared = false;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let rect = source_rect?;
    if source_sheet.is_empty() {
        return None;
    }
    Some(ParsedCache {
        source_sheet,
        source_rect: rect,
        fields,
    })
}

/// 在 `xlsx::load` 阶段直接从打开的 ZIP 包还原所有工作表关联的透视表对象。
pub fn load_pivots_from_zip<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    wb: &Workbook,
) -> Vec<PivotTableModel> {
    let mut out = Vec::new();
    let Some(wb_xml) = read_zip_part(zip, "xl/workbook.xml") else {
        return out;
    };
    let Some(wb_rels_xml) = read_zip_part(zip, "xl/_rels/workbook.xml.rels") else {
        return out;
    };
    let wb_rels = parse_rels(&wb_rels_xml, "xl/workbook.xml");
    let rid_to_target: HashMap<String, String> = wb_rels
        .iter()
        .map(|(id, _, target)| (id.clone(), target.clone()))
        .collect();

    // 1. 解析 workbook.xml 的 sheet 顺序 -> worksheet part 路径 & cacheId -> cache part 路径
    let mut sheet_parts: Vec<(String, String)> = Vec::new();
    let mut cache_parts: HashMap<u32, String> = HashMap::new();
    {
        let mut reader = Reader::from_reader(wb_xml.as_slice());
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                    "sheet" => {
                        if let (Some(name), Some(rid)) = (attr(&e, "name"), rel_id_attr(&e))
                            && let Some(target) = rid_to_target.get(&rid)
                        {
                            sheet_parts.push((name, target.clone()));
                        }
                    }
                    "pivotCache" => {
                        if let (Some(cid_s), Some(rid)) = (attr(&e, "cacheId"), rel_id_attr(&e))
                            && let Ok(cid) = cid_s.parse::<u32>()
                            && let Some(target) = rid_to_target.get(&rid)
                        {
                            cache_parts.insert(cid, target.clone());
                        }
                    }
                    _ => {}
                },
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }

    // 2. 遍历每个工作表的 rels，找出挂载的 pivotTable 部件
    let mut parsed_caches: HashMap<String, Option<ParsedCache>> = HashMap::new();
    for (sheet_name, sheet_part) in &sheet_parts {
        let Some(target_sheet_idx) = wb.sheets.iter().position(|s| s.name == *sheet_name) else {
            continue;
        };
        let Some(srels_xml) = read_zip_part(zip, &rels_path(sheet_part)) else {
            continue;
        };
        let srels = parse_rels(&srels_xml, sheet_part);
        for (_, kind, pt_part) in srels {
            if !kind.ends_with("/pivotTable") {
                continue;
            }
            let Some(pt_xml) = read_zip_part(zip, &pt_part) else {
                continue;
            };
            let mut cache_part_opt: Option<String> = None;
            if let Some(pt_rels_xml) = read_zip_part(zip, &rels_path(&pt_part)) {
                for (_, rk, rt) in parse_rels(&pt_rels_xml, &pt_part) {
                    if rk.ends_with("/pivotCacheDefinition") {
                        cache_part_opt = Some(rt);
                        break;
                    }
                }
            }
            if let Some(mut model) = parse_pivot_table_xml(
                &pt_xml,
                &pt_part,
                target_sheet_idx,
                wb,
                |cid| {
                    let cp = cache_part_opt
                        .clone()
                        .or_else(|| cache_parts.get(&cid).cloned())?;
                    if !parsed_caches.contains_key(&cp) {
                        let parsed = read_zip_part(zip, &cp).and_then(|b| parse_cache_def(&b));
                        parsed_caches.insert(cp.clone(), parsed);
                    }
                    parsed_caches.get(&cp).and_then(|o| o.as_ref()).cloned()
                },
            ) {
                if let Some(src_sh) = wb.sheets.iter().find(|s| s.name == model.source_sheet) {
                    model.source_sheet_id = Some(src_sh.identity);
                    model.last_source_rev = src_sh.cell_revision();
                    let computed = compute_table(src_sh, &model);
                    model.last_meta = computed.meta;
                    model.last_meta.rect = model.rect;
                }
                out.push(model);
            }
        }
    }
    out
}

#[derive(Default)]
struct RawPivotField {
    hidden_indices: Vec<usize>,
    sort_desc: bool,
    subtotal_off: bool,
}

fn parse_pivot_table_xml(
    xml: &[u8],
    part_path: &str,
    target_sheet: usize,
    wb: &Workbook,
    mut load_cache: impl FnMut(u32) -> Option<ParsedCache>,
) -> Option<PivotTableModel> {
    let mut reader = Reader::from_reader(xml);
    let mut name = "数据透视表".to_string();
    let mut cache_id: u32 = 0;
    let mut compact = false;
    let mut outline = true;
    let mut row_grand = true;
    let mut col_grand = true;
    let mut empty_caption = String::new();
    let mut loc_rect: Option<(u32, u32, u32, u32)> = None;
    let mut style_theme = PivotStyleTheme::Blue;
    let mut banded_rows = true;
    let mut banded_cols = false;

    let mut pivot_fields: Vec<RawPivotField> = Vec::new();
    let mut row_field_ids: Vec<i32> = Vec::new();
    let mut col_field_ids: Vec<i32> = Vec::new();
    let mut page_fields: Vec<(usize, Option<usize>)> = Vec::new();
    let mut data_fields: Vec<(usize, Agg, Option<String>, ShowAs, Option<(usize, usize)>)> =
        Vec::new();

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Sec {
        None,
        PivotFields,
        RowFields,
        ColFields,
        PageFields,
        DataFields,
    }
    let mut sec = Sec::None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "pivotTableDefinition" => {
                    if let Some(n) = attr(&e, "name") {
                        name = n;
                    }
                    if let Some(c) = attr(&e, "cacheId").and_then(|s| s.parse().ok()) {
                        cache_id = c;
                    }
                    compact = attr(&e, "compact").as_deref() == Some("1")
                        || attr(&e, "compactData").as_deref() == Some("1");
                    outline = attr(&e, "outline").as_deref() != Some("0");
                    row_grand = attr(&e, "rowGrandTotals").as_deref() != Some("0");
                    col_grand = attr(&e, "colGrandTotals").as_deref() != Some("0");
                    if let Some(mc) = attr(&e, "missingCaption") {
                        empty_caption = mc;
                    }
                }
                "location" => {
                    if let Some(rf) = attr(&e, "ref") {
                        loc_rect = parse_a1_range(&rf);
                    }
                }
                "pivotFields" => sec = Sec::PivotFields,
                "rowFields" => sec = Sec::RowFields,
                "colFields" => sec = Sec::ColFields,
                "pageFields" => sec = Sec::PageFields,
                "dataFields" => sec = Sec::DataFields,
                "pivotField" if sec == Sec::PivotFields => {
                    let sort_desc = attr(&e, "sortType").as_deref() == Some("descending");
                    let subtotal_off = attr(&e, "defaultSubtotal").as_deref() == Some("0");
                    pivot_fields.push(RawPivotField {
                        hidden_indices: Vec::new(),
                        sort_desc,
                        subtotal_off,
                    });
                }
                "pivotTableStyleInfo" => {
                    style_theme = PivotStyleTheme::from_ooxml_name(attr(&e, "name").as_deref());
                    banded_rows = attr(&e, "showRowStripes").as_deref() == Some("1");
                    banded_cols = attr(&e, "showColStripes").as_deref() == Some("1");
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                "location" => {
                    if let Some(rf) = attr(&e, "ref") {
                        loc_rect = parse_a1_range(&rf);
                    }
                }
                "pivotField" if sec == Sec::PivotFields => {
                    let sort_desc = attr(&e, "sortType").as_deref() == Some("descending");
                    let subtotal_off = attr(&e, "defaultSubtotal").as_deref() == Some("0");
                    pivot_fields.push(RawPivotField {
                        hidden_indices: Vec::new(),
                        sort_desc,
                        subtotal_off,
                    });
                }
                "item" if sec == Sec::PivotFields => {
                    if attr(&e, "h").as_deref() == Some("1")
                        && let Some(x) = attr(&e, "x").and_then(|s| s.parse::<usize>().ok())
                        && let Some(pf) = pivot_fields.last_mut()
                    {
                        pf.hidden_indices.push(x);
                    }
                }
                "field" if sec == Sec::RowFields => {
                    if let Some(x) = attr(&e, "x").and_then(|s| s.parse::<i32>().ok()) {
                        row_field_ids.push(x);
                    }
                }
                "field" if sec == Sec::ColFields => {
                    if let Some(x) = attr(&e, "x").and_then(|s| s.parse::<i32>().ok()) {
                        col_field_ids.push(x);
                    }
                }
                "pageField" if sec == Sec::PageFields => {
                    if let Some(fld) = attr(&e, "fld").and_then(|s| s.parse::<usize>().ok()) {
                        let item = attr(&e, "item").and_then(|s| s.parse::<usize>().ok());
                        page_fields.push((fld, item));
                    }
                }
                "dataField" if sec == Sec::DataFields => {
                    if let Some(fld) = attr(&e, "fld").and_then(|s| s.parse::<usize>().ok()) {
                        let agg = Agg::from_ooxml_subtotal(attr(&e, "subtotal").as_deref());
                        let label = attr(&e, "name");
                        let show_as = ShowAs::from_ooxml(attr(&e, "showDataAs").as_deref());
                        let base = match (
                            attr(&e, "baseField").and_then(|s| s.parse::<usize>().ok()),
                            attr(&e, "baseItem").and_then(|s| s.parse::<usize>().ok()),
                        ) {
                            (Some(bf), Some(bi))
                                if matches!(
                                    show_as,
                                    ShowAs::DifferencePrev | ShowAs::PercentDifferencePrev
                                ) && bi < 1_000_000 =>
                            {
                                Some((bf, bi))
                            }
                            _ => None,
                        };
                        data_fields.push((fld, agg, label, show_as, base));
                    }
                }
                "pivotTableStyleInfo" => {
                    style_theme = PivotStyleTheme::from_ooxml_name(attr(&e, "name").as_deref());
                    banded_rows = attr(&e, "showRowStripes").as_deref() == Some("1");
                    banded_cols = attr(&e, "showColStripes").as_deref() == Some("1");
                }
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "pivotFields" | "rowFields" | "colFields" | "pageFields" | "dataFields" => {
                    sec = Sec::None;
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    let rect = loc_rect?;
    let cache = load_cache(cache_id)?;
    let (_, c1, _, c2) = cache.source_rect;
    let n_src_cols = (c2.saturating_sub(c1) + 1) as usize;

    let mut rows = Vec::new();
    let mut row_ext = Vec::new();
    for fid in row_field_ids {
        if fid >= 0 && (fid as usize) < n_src_cols {
            let fi = fid as usize;
            rows.push(AxisField::plain(c1 + fi as u32));
            let mut ext = AxisFieldExt::default();
            if let Some(pf) = pivot_fields.get(fi) {
                if pf.sort_desc {
                    ext.sort = FieldSortMode::PinyinDesc;
                }
                ext.show_subtotal = !pf.subtotal_off;
                if let Some((_, shared, _)) = cache.fields.get(fi) {
                    ext.hidden_items = pf
                        .hidden_indices
                        .iter()
                        .filter_map(|&idx| shared.get(idx).cloned())
                        .collect();
                }
            }
            row_ext.push(ext);
        }
    }

    let mut cols = Vec::new();
    let mut col_ext = Vec::new();
    for fid in col_field_ids {
        if fid >= 0 && (fid as usize) < n_src_cols {
            let fi = fid as usize;
            cols.push(AxisField::plain(c1 + fi as u32));
            let mut ext = AxisFieldExt::default();
            if let Some(pf) = pivot_fields.get(fi) {
                if pf.sort_desc {
                    ext.sort = FieldSortMode::PinyinDesc;
                }
                ext.show_subtotal = !pf.subtotal_off;
                if let Some((_, shared, _)) = cache.fields.get(fi) {
                    ext.hidden_items = pf
                        .hidden_indices
                        .iter()
                        .filter_map(|&idx| shared.get(idx).cloned())
                        .collect();
                }
            }
            col_ext.push(ext);
        }
    }

    let mut filters = Vec::new();
    for (fi, item_opt) in page_fields {
        if fi < n_src_cols {
            let mut keep = Vec::new();
            if let Some(item_idx) = item_opt
                && let Some((_, shared, _)) = cache.fields.get(fi)
                && let Some(val) = shared.get(item_idx)
            {
                keep.push(val.clone());
            } else if let Some(pf) = pivot_fields.get(fi)
                && !pf.hidden_indices.is_empty()
                && let Some((_, shared, _)) = cache.fields.get(fi)
            {
                let hid: std::collections::HashSet<usize> =
                    pf.hidden_indices.iter().copied().collect();
                for (idx, val) in shared.iter().enumerate() {
                    if !hid.contains(&idx) {
                        keep.push(val.clone());
                    }
                }
            }
            filters.push(FilterField {
                col: c1 + fi as u32,
                keep,
            });
        }
    }

    let mut values = Vec::new();
    let mut val_ext = Vec::new();
    let mut calc_fields = Vec::new();
    for (fi, agg, label, show_as, base_opt) in data_fields {
        if fi < n_src_cols {
            let base = base_opt.and_then(|(bf, bi)| {
                let item = cache.fields.get(bf)?.1.get(bi)?.clone();
                Some((c1 + bf as u32, item))
            });
            values.push(ValueField {
                col: c1 + fi as u32,
                agg,
                label,
                show_as,
                base,
            });
            val_ext.push(ValueFieldExt::default());
        } else if let Some((cf_name, _, Some(formula))) = cache.fields.get(fi) {
            calc_fields.push((cf_name.clone(), formula.clone()));
        }
    }

    let report_layout = if compact {
        ReportLayout::Compact
    } else if outline {
        ReportLayout::Tabular
    } else {
        ReportLayout::Tabular
    };
    let has_any_subtotal = row_ext.iter().any(|e| e.show_subtotal) || row_ext.is_empty();
    let spec = PivotSpec2 {
        source: cache.source_rect,
        rows,
        cols,
        values,
        filters,
        layout: PivotLayout {
            tabular: report_layout == ReportLayout::Tabular,
            subtotals: has_any_subtotal,
            grand_totals: row_grand || col_grand,
            sort: AxisSort::Pinyin,
        },
    };
    let source_sheet_id = wb
        .sheets
        .iter()
        .find(|s| s.name == cache.source_sheet)
        .map(|s| s.identity);
    let mut model = PivotTableModel::new(
        name,
        cache.source_sheet,
        source_sheet_id,
        target_sheet,
        (rect.0, rect.1),
        spec,
    );
    model.rect = rect;
    model.imported_part = Some(part_path.to_string());
    model.modified = false;
    model.options = PivotTableOptions {
        report_layout,
        row_grand_totals: row_grand,
        col_grand_totals: col_grand,
        subtotal_pos: if has_any_subtotal {
            SubtotalPos::Bottom
        } else {
            SubtotalPos::Off
        },
        empty_cell_text: empty_caption,
        style_theme,
        banded_rows,
        banded_cols,
        row_ext,
        col_ext,
        val_ext,
        calc_fields,
        ..PivotTableOptions::default()
    };
    model.ensure_ext_synced();
    Some(model)
}

#[cfg(test)]
mod tests {
    use crate::model::{Cell, Sheet, Value, Workbook};
    use crate::pivot::Agg;
    use crate::pivot_engine::{
        compute_table, write_table_styled, PivotStyleTheme, PivotTableModel,
    };
    use crate::pivot_multi::{AxisField, FilterField, PivotLayout, PivotSpec2, ShowAs, ValueField};

    #[test]
    fn cross_sheet_pivot_saves_to_ooxml_and_reloads_as_live_model() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "数据源".into();
        for (c, h) in ["区域", "渠道", "品类", "金额", "数量"].iter().enumerate() {
            wb.sheets[0].set(0, c as u32, Cell { value: Value::Text((*h).into()), formula: None });
        }
        let rows = [
            ("华东", "线上", "手机", 100.0, 2.0),
            ("华东", "线下", "电脑", 300.0, 3.0),
            ("华北", "线上", "手机", 500.0, 5.0),
            ("华南", "线上", "平板", 80.0, 1.0),
        ];
        for (i, (reg, ch, cat, amt, qty)) in rows.into_iter().enumerate() {
            let r = i as u32 + 1;
            wb.sheets[0].set(r, 0, Cell { value: Value::Text(reg.into()), formula: None });
            wb.sheets[0].set(r, 1, Cell { value: Value::Text(ch.into()), formula: None });
            wb.sheets[0].set(r, 2, Cell { value: Value::Text(cat.into()), formula: None });
            wb.sheets[0].set(r, 3, Cell { value: Value::Number(amt), formula: None });
            wb.sheets[0].set(r, 4, Cell { value: Value::Number(qty), formula: None });
        }
        wb.sheets.push(Sheet::new("透视报表"));

        let spec = PivotSpec2 {
            source: (0, 0, 4, 4),
            rows: vec![AxisField::plain(0)],
            cols: vec![AxisField::plain(2)],
            values: vec![ValueField {
                col: 3,
                agg: Agg::Sum,
                label: Some("销售额".into()),
                show_as: ShowAs::Normal,
                base: None,
            }],
            filters: vec![FilterField {
                col: 1,
                keep: vec!["线上".into()],
            }],
            layout: PivotLayout::default(),
        };
        let mut model = PivotTableModel::new(
            "销售透视".into(),
            "数据源".into(),
            Some(wb.sheets[0].identity),
            1,
            (1, 0),
            spec,
        );
        model.options.style_theme = PivotStyleTheme::Green;
        model.options.row_ext[0].hidden_items.push("华南".into());
        model.options.calc_fields.push(("单价".into(), "金额/数量".into()));

        let computed = compute_table(&wb.sheets[0], &model);
        let rect = write_table_styled(&mut wb.sheets[1], &mut wb.styles, (1, 0), &computed, &model);
        model.rect = rect;
        model.last_meta = computed.meta;
        wb.pivots.push(model);

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cross_pivot.xlsx");
        crate::xlsx::save_atomic(&path, &wb).unwrap();

        let reloaded = crate::xlsx::load(&path).unwrap();
        assert_eq!(reloaded.pivots.len(), 1, "重开文件应自动还原透视表对象");
        let p = &reloaded.pivots[0];
        assert_eq!(p.name, "销售透视");
        assert_eq!(p.source_sheet, "数据源");
        assert_eq!(p.target_sheet, 1);
        assert_eq!(p.spec.source, (0, 0, 4, 4));
        assert_eq!(p.spec.rows.len(), 1);
        assert_eq!(p.spec.cols.len(), 1);
        assert_eq!(p.spec.filters.len(), 1);
        assert_eq!(p.spec.filters[0].keep, vec!["线上".to_string()]);
        assert!(p.options.row_ext[0].hidden_items.contains(&"华南".to_string()));
        assert_eq!(p.options.style_theme, PivotStyleTheme::Green);
        assert_eq!(p.options.calc_fields.len(), 1);
        assert_eq!(p.options.calc_fields[0].0, "单价");
        assert!(!p.modified, "刚加载未修改的透视表 modified 应为 false");
    }
}
