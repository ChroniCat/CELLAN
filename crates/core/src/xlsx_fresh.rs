//! Pure native streaming OOXML package writer for both brand-new workbooks
//! and existing-package rebuilds (zero external DOM serializer dependency).
use crate::model::{Cell, CellRef, Sheet, Value, Workbook};
use anyhow::{Context, Result, ensure};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{BufWriter, Write},
    ops::Range,
    path::Path,
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

/// Every valid in-memory worksheet is supported by the native streaming writer.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn supported(sheet: &Sheet) -> bool {
    let _ = sheet;
    true
}

/// Encode a Rust string into valid OOXML `ST_Xstring` XML element text:
/// - Literal `_xHHHH_` sequences are escaped as `_x005F_xHHHH_` so Excel/CELLAN
///   do not decode them into character code units.
/// - XML-illegal control characters (`< 0x20` other than `\t`, `\n`), `\r`,
///   `\u{FFFE}`, and `\u{FFFF}` are encoded as `_xHHHH_`.
/// - `&`, `<`, `>` are XML-escaped.
pub(super) fn encode_xstring_xml(text: &str) -> Cow<'_, str> {
    if text.bytes().all(|b| b >= 0x20 && !matches!(b, b'&' | b'<' | b'>' | b'_'))
        && !text.contains('\u{fffe}')
        && !text.contains('\u{ffff}')
    {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len() + 16);
    for (i, ch) in text.char_indices() {
        if ch == '_'
            && i + 7 <= bytes.len()
            && bytes[i + 1] == b'x'
            && bytes[i + 6] == b'_'
            && bytes[i + 2..i + 6].iter().all(u8::is_ascii_hexdigit)
        {
            out.push_str("_x005F_");
            continue;
        }
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' | '\n' => out.push(ch),
            '\r' => out.push_str("_x000D_"),
            c if c < ' ' || c == '\u{fffe}' || c == '\u{ffff}' => {
                use std::fmt::Write as _;
                let _ = write!(out, "_x{:04X}_", c as u32);
            }
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Decode OOXML `ST_Xstring` `_xHHHH_` escapes (matching `xlsx_load::office_text`).
fn decode_xstring(s: &str) -> Cow<'_, str> {
    if !s.contains("_x") {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let unit = |s: &str| {
        s.as_bytes()
            .get(..7)
            .filter(|b| b.starts_with(b"_x") && b[6] == b'_')
            .and_then(|b| std::str::from_utf8(&b[2..6]).ok())
            .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|s| u16::from_str_radix(s, 16).ok())
    };
    while let Some(pos) = rest.find("_x") {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        if let Some(u) = unit(rest) {
            rest = &rest[7..];
            let mut value = u32::from(u);
            if (0xd800..=0xdbff).contains(&u) {
                if let Some(low @ 0xdc00..=0xdfff) = unit(rest) {
                    value = 0x10000 + (value - 0xd800) * 1024 + u32::from(low) - 0xdc00;
                    rest = &rest[7..];
                }
            }
            out.push(char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER));
        } else {
            out.push('_');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn encode_formula_xml(raw: &str) -> String {
    let formula = crate::add_fn_prefixes(raw.strip_prefix('=').unwrap_or(raw));
    let mut out = String::with_capacity(formula.len() + 8);
    for ch in formula.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#13;"),
            '\t' | '\n' => out.push(ch),
            c if c < ' ' || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

fn encode_xml_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\t' | '\n' => out.push(ch),
            c if c < ' ' || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

fn encode_rich_inline(runs: &[crate::richtext::RichRun], prefix: &str) -> String {
    use std::fmt::Write as _;
    let mut out = format!("<{prefix}is>");
    for run in runs {
        let _ = write!(out, "<{prefix}r>");
        let has_pr = run.bold
            || run.italic
            || run.color.is_some()
            || run.size.is_some_and(|v| v.is_finite() && v > 0.0)
            || run.font.as_ref().is_some_and(|f| !f.is_empty());
        if has_pr {
            let _ = write!(out, "<{prefix}rPr>");
            if run.bold {
                let _ = write!(out, "<{prefix}b/>");
            }
            if run.italic {
                let _ = write!(out, "<{prefix}i/>");
            }
            if let Some([r, g, b]) = run.color {
                let _ = write!(out, "<{prefix}color rgb=\"FF{r:02X}{g:02X}{b:02X}\"/>");
            }
            if let Some(sz) = run.size.filter(|v| v.is_finite() && *v > 0.0) {
                let _ = write!(out, "<{prefix}sz val=\"{sz}\"/>");
            }
            if let Some(f) = run.font.as_deref().filter(|f| !f.is_empty()) {
                let _ = write!(out, "<{prefix}rFont val=\"{}\"/>", encode_xml_attr(f));
            }
            let _ = write!(out, "</{prefix}rPr>");
        }
        let _ = write!(
            out,
            "<{prefix}t xml:space=\"preserve\">{}</{prefix}t></{prefix}r>",
            encode_xstring_xml(&run.text)
        );
    }
    let _ = write!(out, "</{prefix}is>");
    out
}

fn valid_arrays_map(sheet: &Sheet) -> HashMap<CellRef, (u32, u32, u32, u32)> {
    let mut map = HashMap::with_capacity(sheet.arrays.len());
    for &(anchor, rect) in &sheet.arrays {
        if (rect.0, rect.1) == (anchor.row, anchor.col)
            && rect.2 >= rect.0
            && rect.3 >= rect.1
            && rect.2 < crate::sync::MAX_ROWS
            && rect.3 < crate::sync::MAX_COLS
            && sheet.get(anchor.row, anchor.col).is_some_and(|c| c.formula.is_some())
        {
            map.entry(anchor).or_insert(rect);
        }
    }
    map
}

fn build_merge_cells_xml(merges: &[(u32, u32, u32, u32)], prefix: &str) -> String {
    use std::fmt::Write as _;
    let mut seen = HashSet::with_capacity(merges.len());
    let mut valid = Vec::with_capacity(merges.len());
    for &(r1, c1, r2, c2) in merges {
        if r1 <= r2
            && c1 <= c2
            && r2 < crate::sync::MAX_ROWS
            && c2 < crate::sync::MAX_COLS
            && seen.insert((r1, c1, r2, c2))
        {
            valid.push((r1, c1, r2, c2));
        }
    }
    if valid.is_empty() {
        return String::new();
    }
    let mut out = format!("<{prefix}mergeCells count=\"{}\">", valid.len());
    for (r1, c1, r2, c2) in valid {
        let _ = write!(
            out,
            "<{prefix}mergeCell ref=\"{}:{}\"/>",
            crate::model::cell_name(r1, c1),
            crate::model::cell_name(r2, c2)
        );
    }
    let _ = write!(out, "</{prefix}mergeCells>");
    out
}

fn patch_merge_cells(
    root_open_tag: &str,
    footer: &str,
    merges: &[(u32, u32, u32, u32)],
    prefix: &str,
) -> String {
    let wrapped = format!("{root_open_tag}{footer}");
    let Ok(doc) = roxmltree::Document::parse(&wrapped) else {
        return footer.to_owned();
    };
    let offset = root_open_tag.len();
    let root = doc.root_element();
    let mut remove_ranges: Vec<Range<usize>> = Vec::new();
    for ch in root.children().filter(|n| n.is_element()) {
        if ch.tag_name().name() == "mergeCells" {
            let r = ch.range();
            remove_ranges.push((r.start - offset)..(r.end - offset));
        }
    }
    let mut out = footer.to_owned();
    for r in remove_ranges.into_iter().rev() {
        out.replace_range(r, "");
    }
    let block = build_merge_cells_xml(merges, prefix);
    if block.is_empty() {
        return out;
    }
    let wrapped2 = format!("{root_open_tag}{out}");
    let Ok(doc2) = roxmltree::Document::parse(&wrapped2) else {
        return out;
    };
    let root2 = doc2.root_element();
    let insert_at = root2
        .children()
        .filter(|n| n.is_element())
        .find(|n| {
            !matches!(
                n.tag_name().name(),
                "sheetCalcPr"
                    | "sheetProtection"
                    | "protectedRanges"
                    | "scenarios"
                    | "autoFilter"
                    | "sortState"
                    | "dataConsolidate"
                    | "customSheetViews"
            )
        })
        .map(|n| n.range().start - offset)
        .or_else(|| out.rfind("</"))
        .unwrap_or(out.len());
    out.insert_str(insert_at, &block);
    out
}

pub(super) fn update_dimension_in_header(
    header: &str,
    root_qname: &str,
    bounds: Option<(u32, u32, u32, u32)>,
) -> String {
    let closed = format!("{header}</{root_qname}>");
    let Ok(doc) = roxmltree::Document::parse(&closed) else {
        return header.to_owned();
    };
    let root = doc.root_element();
    let Some(dim_attr) = root
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "dimension")
        .and_then(|n| n.attribute_node("ref"))
    else {
        return header.to_owned();
    };
    let reference = match bounds {
        Some((top, left, bottom, right)) => {
            let first = crate::model::cell_name(top, left);
            let end = crate::model::cell_name(bottom, right);
            if first == end {
                first
            } else {
                format!("{first}:{end}")
            }
        }
        None => "A1".to_owned(),
    };
    let mut out = header.to_owned();
    out.replace_range(dim_attr.range_value(), &reference);
    out
}

fn minimal_sheet_shell_xml(sheet: &Sheet, tab_selected: bool) -> String {
    let sel = if tab_selected { " tabSelected=\"1\"" } else { "" };
    let merges = build_merge_cells_xml(&sheet.merges, "");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<sheetPr><outlinePr summaryBelow=\"1\" summaryRight=\"1\" showOutlineSymbols=\"1\"/><pageSetUpPr fitToPage=\"0\"/></sheetPr>\
<dimension ref=\"A1\"/>\
<sheetViews><sheetView{sel} workbookViewId=\"0\"/></sheetViews>\
<sheetFormatPr defaultRowHeight=\"15\"/>\
<sheetData/>{merges}\
<printOptions gridLines=\"0\" headings=\"0\" horizontalCentered=\"0\" verticalCentered=\"0\"/>\
<pageMargins left=\"0.7\" right=\"0.7\" top=\"0.75\" bottom=\"0.75\" header=\"0.3\" footer=\"0.3\"/>\
<pageSetup paperSize=\"9\" orientation=\"portrait\" scale=\"100\"/>\
</worksheet>"
    )
}

fn direct_sheet_shell_xml(sheet: &Sheet, tab_selected: bool) -> Result<String> {
    use std::fmt::Write as _;
    let sel = if tab_selected { " tabSelected=\"1\"" } else { "" };
    let merges = build_merge_cells_xml(&sheet.merges, "");
    let mut fmt_pr = String::from("<sheetFormatPr");
    if (sheet.rows.default_size() - crate::layout::DEFAULT_ROW_H).abs() > 0.01 {
        let _ = write!(
            fmt_pr,
            " defaultRowHeight=\"{:.2}\" customHeight=\"1\"",
            sheet.rows.default_size() * 72.0 / 96.0
        );
    } else {
        fmt_pr.push_str(" defaultRowHeight=\"15\"");
    }
    if let Some(max_rlv) = sheet.row_levels.values().copied().max().filter(|&v| v > 0) {
        let _ = write!(fmt_pr, " outlineLevelRow=\"{max_rlv}\"");
    }
    if let Some(max_clv) = sheet.col_levels.values().copied().max().filter(|&v| v > 0) {
        let _ = write!(fmt_pr, " outlineLevelCol=\"{max_clv}\"");
    }
    fmt_pr.push_str("/>");

    let has_col_default = (sheet.cols.default_size() - crate::layout::DEFAULT_COL_W).abs() > 0.01;
    let init_cols = if has_col_default {
        let w = ((sheet.cols.default_size() - 5.0) / 8.0).max(0.0);
        format!("<cols><col min=\"1\" max=\"16384\" width=\"{w:.2}\" customWidth=\"1\"/></cols>")
    } else {
        String::new()
    };

    let mut shell = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<sheetPr><outlinePr summaryBelow=\"1\" summaryRight=\"1\" showOutlineSymbols=\"1\"/><pageSetUpPr fitToPage=\"0\"/></sheetPr>\
<dimension ref=\"A1\"/>\
<sheetViews><sheetView{sel} workbookViewId=\"0\"/></sheetViews>\
{fmt_pr}{init_cols}\
<sheetData/>{merges}\
<printOptions gridLines=\"0\" headings=\"0\" horizontalCentered=\"0\" verticalCentered=\"0\"/>\
<pageMargins left=\"0.7\" right=\"0.7\" top=\"0.75\" bottom=\"0.75\" header=\"0.3\" footer=\"0.3\"/>\
<pageSetup paperSize=\"9\" orientation=\"portrait\" scale=\"100\"/>\
</worksheet>"
    );

    if sheet.tab_color.is_some() {
        shell = crate::patch::set_tab_color(&shell, sheet.tab_color);
    }
    if !sheet.outline_summary_below || !sheet.outline_summary_right || !sheet.outline_symbols {
        shell = crate::patch::set_outline_pr(
            &shell,
            sheet.outline_summary_below,
            sheet.outline_summary_right,
            sheet.outline_symbols,
        );
    }
    if sheet.zoom_scale != 100 || !sheet.show_gridlines || !sheet.show_headings {
        shell = crate::patch::set_sheet_view_xml(
            &shell,
            sheet.zoom_scale,
            sheet.show_gridlines,
            sheet.show_headings,
        );
    }
    if sheet.freeze.is_some() {
        shell = crate::patch::set_freeze_xml(&shell, sheet.freeze);
    }
    if !sheet.cols.spans().is_empty()
        || !sheet.hidden_cols.is_empty()
        || !sheet.col_levels.is_empty()
    {
        let mut col_changes: BTreeMap<u32, crate::patch::layout_xml::Change> = BTreeMap::new();
        for (&c, &w) in sheet.cols.entries() {
            if c < crate::sync::MAX_COLS {
                col_changes.entry(c).or_default().size = Some(Some(w));
            }
        }
        for &c in sheet.hidden_cols.iter() {
            if c < crate::sync::MAX_COLS {
                col_changes.entry(c).or_default().hidden = Some(true);
            }
        }
        for (&c, &lv) in &sheet.col_levels {
            if c < crate::sync::MAX_COLS && lv > 0 {
                col_changes.entry(c).or_default().level = Some(lv);
            }
        }
        if !col_changes.is_empty() {
            if let Some(sd_pos) = shell.find("<sheetData") {
                let updated_head =
                    crate::patch::layout_xml::apply_header(&shell[..sd_pos], &col_changes)?;
                let mut next = String::with_capacity(updated_head.len() + shell.len() - sd_pos);
                next.push_str(&updated_head);
                next.push_str(&shell[sd_pos..]);
                shell = next;
            }
        }
    }
    if sheet.protection.is_some() || !sheet.protected_ranges.is_empty() {
        shell = crate::patch::set_sheet_protection_and_ranges(
            &shell,
            sheet.protection.as_ref(),
            (!sheet.protected_ranges.is_empty()).then_some(sheet.protected_ranges.as_slice()),
        );
    }
    if let Some(f) = &sheet.filter
        && f.color_filters.is_empty()
    {
        shell = crate::patch::insert_auto_filter(&shell, &crate::patch::auto_filter_xml(f));
    }
    if !sheet.cf.blocks.is_empty() && sheet.cf.dxfs.is_empty() {
        shell = crate::patch::insert_cf(&shell, &crate::patch::cf_blocks_xml(&sheet.cf.blocks));
    }
    if !sheet.dv.is_empty() {
        shell = crate::patch::insert_dv(&shell, &crate::patch::dv_xml(&sheet.dv));
    }
    if sheet.page != crate::page::PageSetup::default() {
        shell = crate::patch::set_page_xml(&shell, &sheet.page);
        shell = crate::patch::set_page_setup(&shell, &sheet.page);
    }
    if !sheet.ignored_errors.is_empty() {
        let mut cells: Vec<(u32, u32)> = sheet.ignored_errors.iter().copied().collect();
        cells.sort_unstable();
        shell = crate::patch::set_ignored_errors(&shell, &cells);
    }
    Ok(shell)
}

fn minimal_styles_xml() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
<fonts count=\"1\"><font><sz val=\"11\"/><color rgb=\"FF000000\"/><name val=\"Calibri\"/></font></fonts>\
<fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills>\
<borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>\
<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>\
<cellXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/></cellXfs>\
<cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>\
<dxfs count=\"0\"/>\
<tableStyles count=\"0\" defaultTableStyle=\"TableStyleMedium2\" defaultPivotStyle=\"PivotStyleLight16\"/>\
</styleSheet>"
}

fn build_fresh_package_xmls(
    wb: &Workbook,
    sheet_names: &[&str],
    has_sst: bool,
    has_metadata: bool,
    full_calc_on_load: bool,
    include_direct_workbook_meta: bool,
) -> Result<(String, String, String, String)> {
    use std::fmt::Write as _;
    let mut ct = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>",
    );
    let mut wb_rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    let date_attr = if wb.date_system.is_1904() {
        " date1904=\"1\""
    } else {
        ""
    };
    let wb_prot_xml = if include_direct_workbook_meta {
        wb.protection.as_ref().map(|p| p.to_xml()).unwrap_or_default()
    } else {
        String::new()
    };
    let mut wb_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<workbookPr{date_attr}/>{wb_prot_xml}\
<bookViews><workbookView xWindow=\"0\" yWindow=\"0\" windowWidth=\"24000\" windowHeight=\"12000\"/></bookViews>\
<sheets>"
    );
    let any_visible_sheet = wb.sheets.iter().any(|s| !s.is_hidden());
    for (idx, name) in sheet_names.iter().enumerate() {
        let n = idx + 1;
        let _ = write!(
            ct,
            "<Override PartName=\"/xl/worksheets/sheet{n}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
        );
        let _ = write!(
            wb_rels,
            "<Relationship Id=\"rId{n}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{n}.xml\"/>"
        );
        let state_attr = if include_direct_workbook_meta && any_visible_sheet {
            match wb.sheets.get(idx).map(|s| s.visibility()) {
                Some(crate::model::SheetVisibility::VeryHidden) => " state=\"veryHidden\"",
                Some(crate::model::SheetVisibility::Hidden) => " state=\"hidden\"",
                _ => "",
            }
        } else {
            ""
        };
        let _ = write!(
            wb_xml,
            "<sheet name=\"{}\" sheetId=\"{n}\"{state_attr} r:id=\"rId{n}\"/>",
            encode_xml_attr(name)
        );
    }
    let mut next_rid = sheet_names.len() + 1;
    let _ = write!(
        wb_rels,
        "<Relationship Id=\"rId{next_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>"
    );
    next_rid += 1;
    if has_sst {
        let _ = write!(
            ct,
            "<Override PartName=\"/xl/sharedStrings.xml\" ContentType=\"{SST_CONTENT_TYPE}\"/>"
        );
        let _ = write!(
            wb_rels,
            "<Relationship Id=\"rId{next_rid}\" Type=\"{SST_REL}\" Target=\"sharedStrings.xml\"/>"
        );
        next_rid += 1;
    }
    if has_metadata {
        let _ = write!(
            ct,
            "<Override PartName=\"/xl/metadata.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheetMetadata+xml\"/>"
        );
        let _ = write!(
            wb_rels,
            "<Relationship Id=\"rId{next_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/sheetMetadata\" Target=\"metadata.xml\"/>"
        );
    }
    ct.push_str("</Types>");
    wb_rels.push_str("</Relationships>");
    wb_xml.push_str("</sheets>");

    if include_direct_workbook_meta {
        let mut defs = String::new();
        for (name, local, text) in &wb.names {
            if name == crate::patch::MANUAL_HIDDEN_NAME {
                continue;
            }
            let body = text.trim_start_matches('=');
            match local {
                Some(i) => {
                    let _ = write!(
                        defs,
                        "<definedName name=\"{}\" localSheetId=\"{i}\">{}</definedName>",
                        encode_xml_attr(name),
                        encode_xml_attr(body)
                    );
                }
                None => {
                    let _ = write!(
                        defs,
                        "<definedName name=\"{}\">{}</definedName>",
                        encode_xml_attr(name),
                        encode_xml_attr(body)
                    );
                }
            }
        }
        let manual_hidden: BTreeMap<String, Vec<(u32, u32)>> = wb
            .sheets
            .iter()
            .filter(|s| !s.hidden_rows_user.is_empty())
            .map(|s| (s.name.clone(), s.hidden_rows_user.intervals().to_vec()))
            .collect();
        if !manual_hidden.is_empty() {
            let encoded = crate::patch::encode_manual_hidden_intervals(&manual_hidden)?;
            let _ = write!(
                defs,
                "<definedName name=\"{}\" hidden=\"1\">{encoded}</definedName>",
                crate::patch::MANUAL_HIDDEN_NAME
            );
        }
        if !defs.is_empty() {
            wb_xml.push_str("<definedNames>");
            wb_xml.push_str(&defs);
            wb_xml.push_str("</definedNames>");
        }
    }

    if full_calc_on_load {
        wb_xml.push_str("<calcPr calcId=\"0\" fullCalcOnLoad=\"1\"/>");
    } else {
        wb_xml.push_str("<calcPr calcId=\"191029\"/>");
    }
    wb_xml.push_str("</workbook>");

    if include_direct_workbook_meta {
        for (idx, sheet) in wb.sheets.iter().enumerate() {
            if sheet.page.print_area.is_some()
                || sheet.page.title_rows.is_some()
                || sheet.page.title_cols.is_some()
            {
                wb_xml = crate::patch::set_print_names(
                    &wb_xml,
                    idx as u32,
                    &sheet.name,
                    &sheet.page,
                );
            }
        }
        if wb.calc_manual {
            wb_xml = crate::patch::set_calc_mode(&wb_xml, true);
        }
        if wb.calc_iterate
            || wb.calc_max_iterations != 100
            || (wb.calc_max_change - 0.001).abs() > 1e-12
        {
            wb_xml = crate::patch::set_calc_iteration(
                &wb_xml,
                wb.calc_iterate,
                wb.calc_max_iterations,
                wb.calc_max_change,
            );
        }
    }

    let root_rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
</Relationships>",
    );
    Ok((ct, root_rels, wb_xml, wb_rels))
}

/// Write a tiny (~1.5 KB) valid OOXML skeleton so all post-save metadata patches
/// (CF, DV, layout, notes, tables, drawings, charts, pivots, etc.) run on an empty
/// grid before `fresh::write` streams the cell data + styles + SST in one pass.
pub(super) fn write_fresh_skeleton(path: &Path, wb: &Workbook) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let fallback_sheet = Sheet::new("Sheet1");
    let sheets: Vec<&Sheet> = if wb.sheets.is_empty() {
        vec![&fallback_sheet]
    } else {
        wb.sheets.iter().collect()
    };
    let names: Vec<&str> = sheets.iter().map(|s| s.name.as_str()).collect();
    let (ct, root_rels, wb_xml, wb_rels) =
        build_fresh_package_xmls(wb, &names, false, false, false, false)?;

    let file = File::create(path).with_context(|| format!("创建文件失败 {}", path.display()))?;
    let mut out = ZipWriter::new(BufWriter::with_capacity(64 * 1024, file));
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(1));
    out.start_file("[Content_Types].xml", opts)?;
    out.write_all(ct.as_bytes())?;
    out.start_file("_rels/.rels", opts)?;
    out.write_all(root_rels.as_bytes())?;
    out.start_file("xl/workbook.xml", opts)?;
    out.write_all(wb_xml.as_bytes())?;
    out.start_file("xl/_rels/workbook.xml.rels", opts)?;
    out.write_all(wb_rels.as_bytes())?;
    out.start_file(STYLES_PART, opts)?;
    out.write_all(minimal_styles_xml().as_bytes())?;
    for (idx, sheet) in sheets.iter().enumerate() {
        let part = format!("xl/worksheets/sheet{}.xml", idx + 1);
        out.start_file(part, opts)?;
        out.write_all(minimal_sheet_shell_xml(sheet, idx == 0).as_bytes())?;
    }
    out.finish()?.flush()?;
    Ok(())
}

/// True when a brand-new workbook needs zero multi-part post-save patches (charts,
/// pictures, textboxes, tables, notes, hyperlinks, sparklines, scenarios, DXFs, pivots)
/// and can be streamed directly to `path` in a single parallel ZIP pass!
pub(super) fn can_write_direct(wb: &Workbook, wants_macro: bool) -> bool {
    if wants_macro
        || wb.sheets.is_empty()
        || !wb.sheets.iter().any(|s| !s.hidden)
        || !wb.sources.is_empty()
        || !wb.links.is_empty()
        || !wb.pivots_pending.is_empty()
        || !wb.pivots_pending2.is_empty()
        || !wb.pivots.is_empty()
        || !wb.pivots_removed_parts.is_empty()
        || !wb.pivot_calc_fields.is_empty()
        || !wb.pivot_shifts.is_empty()
        || std::env::var("SC_FAIL_SAVE_STEP").is_ok()
    {
        return false;
    }
    wb.sheets.iter().all(|s| {
        s.cf.dxfs.is_empty()
            && s.filter.as_ref().is_none_or(|f| f.color_filters.is_empty())
            && s.notes.is_empty()
            && !s.scenario_settings.has_source()
            && s.scenarios.is_empty()
            && s.sparklines.is_empty()
            && s.hyperlinks.is_empty()
            && s.textboxes.is_empty()
            && s.pictures.is_empty()
            && s.charts.is_empty()
            && s.tables.is_empty()
    })
}

const SST_PART: &str = "xl/sharedStrings.xml";
const STYLES_PART: &str = "xl/styles.xml";
const SST_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings";
const SST_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml";

pub(super) struct FreshResult {
    pub(super) styles: Option<super::delta::SavedStyles>,
    pub(super) trusted_parts: HashMap<String, (u32, u64)>,
}

/// Standard zlib GF(2) CRC-32 combination (`crc32(A || B)` from `crc32(A)`, `crc32(B)`, `B.len()`).
pub(super) fn crc32_combine(mut crc1: u32, crc2: u32, mut len2: u64) -> u32 {
    if len2 == 0 {
        return crc1;
    }
    #[inline]
    fn gf2_matrix_times(mat: &[u32; 32], mut vec: u32) -> u32 {
        let mut sum = 0u32;
        let mut i = 0usize;
        while vec != 0 {
            if (vec & 1) != 0 {
                sum ^= mat[i];
            }
            vec >>= 1;
            i += 1;
        }
        sum
    }
    #[inline]
    fn gf2_matrix_square(square: &mut [u32; 32], mat: &[u32; 32]) {
        for n in 0..32 {
            square[n] = gf2_matrix_times(mat, mat[n]);
        }
    }
    let mut odd = [0u32; 32];
    let mut even = [0u32; 32];
    odd[0] = 0xedb8_8320;
    let mut row = 1u32;
    for n in 1..32 {
        odd[n] = row;
        row <<= 1;
    }
    gf2_matrix_square(&mut even, &odd);
    gf2_matrix_square(&mut odd, &even);
    loop {
        gf2_matrix_square(&mut even, &odd);
        if (len2 & 1) != 0 {
            crc1 = gf2_matrix_times(&even, crc1);
        }
        len2 >>= 1;
        if len2 == 0 {
            break;
        }
        gf2_matrix_square(&mut odd, &even);
        if (len2 & 1) != 0 {
            crc1 = gf2_matrix_times(&odd, crc1);
        }
        len2 >>= 1;
        if len2 == 0 {
            break;
        }
    }
    crc1 ^ crc2
}

/// Compress a raw RFC 1951 deflate chunk (no zlib header) and compute its CRC-32.
/// Non-final chunks end with `FlushCompress::Sync` (byte-aligned); the last chunk ends with `FlushCompress::Finish`.
pub(super) fn deflate_raw_chunk(data: &[u8], level: u32, is_last: bool) -> Result<(Vec<u8>, u32)> {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    let mut comp = flate2::Compress::new(flate2::Compression::new(level), false);
    let mut out = Vec::with_capacity((data.len() / 4).max(1024));
    let mut consumed = 0usize;
    while consumed < data.len() {
        if out.len() == out.capacity() {
            out.reserve((data.len() - consumed).clamp(16 * 1024, 512 * 1024));
        }
        let before_in = comp.total_in();
        let status = comp.compress_vec(&data[consumed..], &mut out, flate2::FlushCompress::None)?;
        consumed += (comp.total_in() - before_in) as usize;
        if status == flate2::Status::BufError && out.len() == out.capacity() {
            out.reserve(64 * 1024);
        }
    }
    let flush = if is_last {
        flate2::FlushCompress::Finish
    } else {
        flate2::FlushCompress::Sync
    };
    loop {
        if out.capacity() - out.len() < 16 * 1024 {
            out.reserve(64 * 1024);
        }
        let before_out = out.len();
        let status = comp.compress_vec(&[], &mut out, flush)?;
        if is_last {
            if status == flate2::Status::StreamEnd {
                break;
            }
        } else if status == flate2::Status::Ok && out.len() < out.capacity() {
            break;
        }
        if status == flate2::Status::BufError && out.len() == before_out {
            out.reserve(64 * 1024);
        }
    }
    Ok((out, crc.sum()))
}

/// Build an in-memory standard single-entry ZIP wrapping pre-compressed RFC 1951 deflate chunks
/// so `ZipWriter::raw_copy_file` can splice them without temp files or recompression.
pub(super) fn build_single_entry_zip(
    name: &str,
    crc32: u32,
    uncompressed_size: u64,
    compressed_chunks: &[Vec<u8>],
) -> Result<Vec<u8>> {
    let comp_total: u64 = compressed_chunks.iter().map(|c| c.len() as u64).sum();
    let uncomp_u32 = u32::try_from(uncompressed_size).context("ZIP 条目未压缩大小超过 4GB")?;
    let comp_u32 = u32::try_from(comp_total).context("ZIP 条目压缩大小超过 4GB")?;
    let name_bytes = name.as_bytes();
    let name_len = u16::try_from(name_bytes.len()).context("ZIP 条目名过长")?;
    let flags: u16 = if name.is_ascii() { 0 } else { 1 << 11 };
    let cd_offset = u32::try_from(30 + name_bytes.len() as u64 + comp_total)
        .context("ZIP 中央目录偏移超过 4GB")?;
    let cd_size = (46 + name_bytes.len()) as u32;

    let mut buf = Vec::with_capacity(98 + name_bytes.len() * 2 + comp_u32 as usize);
    buf.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&flags.to_le_bytes());
    buf.extend_from_slice(&8u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0x0021u16.to_le_bytes());
    buf.extend_from_slice(&crc32.to_le_bytes());
    buf.extend_from_slice(&comp_u32.to_le_bytes());
    buf.extend_from_slice(&uncomp_u32.to_le_bytes());
    buf.extend_from_slice(&name_len.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(name_bytes);
    for c in compressed_chunks {
        buf.extend_from_slice(c);
    }
    buf.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&flags.to_le_bytes());
    buf.extend_from_slice(&8u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0x0021u16.to_le_bytes());
    buf.extend_from_slice(&crc32.to_le_bytes());
    buf.extend_from_slice(&comp_u32.to_le_bytes());
    buf.extend_from_slice(&uncomp_u32.to_le_bytes());
    buf.extend_from_slice(&name_len.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(name_bytes);
    buf.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&cd_size.to_le_bytes());
    buf.extend_from_slice(&cd_offset.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    Ok(buf)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum XmlMode {
    Text,
    TagStart,
    OpenTagName,
    InOpenTag,
    CloseTagName,
    AttrQuote(u8),
    SlashClose,
    Bang,
    CommentDash1,
    Comment,
    CommentEnd1,
    CommentEnd2,
    CdataPrefix(u8),
    Cdata,
    CdataEnd1,
    CdataEnd2,
    Pi,
    PiEnd,
}

/// Lightweight streaming XML well-formedness & UTF-8 checker used during write-out so
/// post-save verification can trust the freshly written `(crc32, uncompressed_size)` without
/// single-core re-decompression and XML re-parsing of 100+ MB worksheets.
pub(super) struct XmlCheckState {
    mode: XmlMode,
    fragment: bool,
    depth: i32,
    min_depth: i32,
    roots: u32,
    closed: bool,
    root_local: [u8; 32],
    root_local_len: usize,
    capturing_root: bool,
    started: bool,
    utf8_tail: [u8; 4],
    utf8_tail_len: usize,
    failed: bool,
}

impl XmlCheckState {
    pub(super) fn new_document() -> Self {
        Self {
            mode: XmlMode::Text,
            fragment: false,
            depth: 0,
            min_depth: 0,
            roots: 0,
            closed: false,
            root_local: [0; 32],
            root_local_len: 0,
            capturing_root: false,
            started: false,
            utf8_tail: [0; 4],
            utf8_tail_len: 0,
            failed: false,
        }
    }

    pub(super) fn new_fragment() -> Self {
        let mut s = Self::new_document();
        s.fragment = true;
        s.started = true;
        s
    }

    fn feed_utf8(&mut self, mut bytes: &[u8]) {
        if self.failed || bytes.is_empty() {
            return;
        }
        if self.utf8_tail_len > 0 {
            let need = (4 - self.utf8_tail_len).min(bytes.len());
            self.utf8_tail[self.utf8_tail_len..self.utf8_tail_len + need]
                .copy_from_slice(&bytes[..need]);
            let combined_len = self.utf8_tail_len + need;
            match std::str::from_utf8(&self.utf8_tail[..combined_len]) {
                Ok(_) => {
                    self.utf8_tail_len = 0;
                    bytes = &bytes[need..];
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid >= self.utf8_tail_len {
                        let used_from_bytes = valid - self.utf8_tail_len;
                        self.utf8_tail_len = 0;
                        bytes = &bytes[used_from_bytes..];
                    } else if e.error_len().is_some() {
                        self.failed = true;
                        return;
                    } else {
                        self.utf8_tail_len = combined_len;
                        return;
                    }
                }
            }
        }
        match std::str::from_utf8(bytes) {
            Ok(_) => {}
            Err(e) => {
                if e.error_len().is_some() {
                    self.failed = true;
                } else {
                    let rem = &bytes[e.valid_up_to()..];
                    self.utf8_tail[..rem.len()].copy_from_slice(rem);
                    self.utf8_tail_len = rem.len();
                }
            }
        }
    }

    pub(super) fn feed(&mut self, mut bytes: &[u8]) {
        if self.failed || bytes.is_empty() {
            return;
        }
        self.feed_utf8(bytes);
        if self.failed {
            return;
        }
        if !self.started {
            self.started = true;
            if bytes.starts_with(b"\xef\xbb\xbf") {
                bytes = &bytes[3..];
            }
        }
        let mut i = 0usize;
        let len = bytes.len();
        while i < len {
            match self.mode {
                XmlMode::Text => {
                    let next = bytes[i..].iter().position(|&b| b == b'<');
                    let end = next.map_or(len, |p| i + p);
                    if !self.fragment && self.depth == 0 {
                        if bytes[i..end]
                            .iter()
                            .any(|&b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
                        {
                            self.failed = true;
                            return;
                        }
                    }
                    if next.is_none() {
                        break;
                    }
                    i = end + 1;
                    self.mode = XmlMode::TagStart;
                }
                XmlMode::TagStart => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        b'/' => self.mode = XmlMode::CloseTagName,
                        b'?' => self.mode = XmlMode::Pi,
                        b'!' => self.mode = XmlMode::Bang,
                        b'<' | b'>' | b' ' | b'\t' | b'\r' | b'\n' => {
                            self.failed = true;
                            return;
                        }
                        _ => {
                            if !self.fragment && self.closed {
                                self.failed = true;
                                return;
                            }
                            self.capturing_root = !self.fragment && self.depth == 0;
                            if self.capturing_root {
                                self.root_local_len = 0;
                                if b != b':' {
                                    self.root_local[0] = b;
                                    self.root_local_len = 1;
                                }
                            }
                            self.mode = XmlMode::OpenTagName;
                        }
                    }
                }
                XmlMode::OpenTagName => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        b'>' => {
                            if !self.fragment && self.depth == 0 {
                                self.roots += 1;
                            }
                            self.depth += 1;
                            self.capturing_root = false;
                            self.mode = XmlMode::Text;
                        }
                        b'/' => {
                            self.capturing_root = false;
                            self.mode = XmlMode::SlashClose;
                        }
                        b' ' | b'\t' | b'\r' | b'\n' => {
                            self.capturing_root = false;
                            self.mode = XmlMode::InOpenTag;
                        }
                        b'<' | b'"' | b'\'' => {
                            self.failed = true;
                            return;
                        }
                        b':' if self.capturing_root => {
                            self.root_local_len = 0;
                        }
                        _ => {
                            if self.capturing_root && self.root_local_len < self.root_local.len() {
                                self.root_local[self.root_local_len] = b;
                                self.root_local_len += 1;
                            }
                        }
                    }
                }
                XmlMode::InOpenTag => {
                    if let Some(rel) = bytes[i..]
                        .iter()
                        .position(|&b| matches!(b, b'>' | b'/' | b'"' | b'\'' | b'<'))
                    {
                        i += rel;
                        let b = bytes[i];
                        i += 1;
                        match b {
                            b'>' => {
                                if !self.fragment && self.depth == 0 {
                                    self.roots += 1;
                                }
                                self.depth += 1;
                                self.mode = XmlMode::Text;
                            }
                            b'/' => self.mode = XmlMode::SlashClose,
                            b'"' | b'\'' => self.mode = XmlMode::AttrQuote(b),
                            _ => {
                                self.failed = true;
                                return;
                            }
                        }
                    } else {
                        break;
                    }
                }
                XmlMode::AttrQuote(q) => {
                    if let Some(rel) = bytes[i..].iter().position(|&b| b == q || b == b'<') {
                        i += rel;
                        if bytes[i] == b'<' {
                            self.failed = true;
                            return;
                        }
                        i += 1;
                        self.mode = XmlMode::InOpenTag;
                    } else {
                        break;
                    }
                }
                XmlMode::SlashClose => {
                    if bytes[i] != b'>' {
                        self.failed = true;
                        return;
                    }
                    i += 1;
                    if !self.fragment && self.depth == 0 {
                        self.roots += 1;
                        self.closed = true;
                    }
                    self.mode = XmlMode::Text;
                }
                XmlMode::CloseTagName => {
                    if let Some(rel) = bytes[i..].iter().position(|&b| b == b'>' || b == b'<') {
                        i += rel;
                        if bytes[i] == b'<' {
                            self.failed = true;
                            return;
                        }
                        i += 1;
                        self.depth -= 1;
                        if self.depth < self.min_depth {
                            self.min_depth = self.depth;
                        }
                        if !self.fragment {
                            if self.depth < 0 {
                                self.failed = true;
                                return;
                            }
                            if self.depth == 0 {
                                self.closed = true;
                            }
                        }
                        self.mode = XmlMode::Text;
                    } else {
                        break;
                    }
                }
                XmlMode::Bang => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        b'-' => self.mode = XmlMode::CommentDash1,
                        b'[' => self.mode = XmlMode::CdataPrefix(0),
                        _ => {
                            self.failed = true;
                            return;
                        }
                    }
                }
                XmlMode::CommentDash1 => {
                    if bytes[i] != b'-' {
                        self.failed = true;
                        return;
                    }
                    i += 1;
                    self.mode = XmlMode::Comment;
                }
                XmlMode::Comment => {
                    if let Some(rel) = bytes[i..].iter().position(|&b| b == b'-') {
                        i += rel + 1;
                        self.mode = XmlMode::CommentEnd1;
                    } else {
                        break;
                    }
                }
                XmlMode::CommentEnd1 => {
                    let b = bytes[i];
                    i += 1;
                    self.mode = if b == b'-' {
                        XmlMode::CommentEnd2
                    } else {
                        XmlMode::Comment
                    };
                }
                XmlMode::CommentEnd2 => {
                    let b = bytes[i];
                    i += 1;
                    if b == b'>' {
                        self.mode = XmlMode::Text;
                    } else {
                        self.failed = true;
                        return;
                    }
                }
                XmlMode::CdataPrefix(step) => {
                    const CDATA: &[u8; 6] = b"CDATA[";
                    if bytes[i] != CDATA[step as usize] {
                        self.failed = true;
                        return;
                    }
                    i += 1;
                    if step == 5 {
                        if !self.fragment && self.depth == 0 {
                            self.failed = true;
                            return;
                        }
                        self.mode = XmlMode::Cdata;
                    } else {
                        self.mode = XmlMode::CdataPrefix(step + 1);
                    }
                }
                XmlMode::Cdata => {
                    if let Some(rel) = bytes[i..].iter().position(|&b| b == b']') {
                        i += rel + 1;
                        self.mode = XmlMode::CdataEnd1;
                    } else {
                        break;
                    }
                }
                XmlMode::CdataEnd1 => {
                    let b = bytes[i];
                    i += 1;
                    self.mode = if b == b']' {
                        XmlMode::CdataEnd2
                    } else {
                        XmlMode::Cdata
                    };
                }
                XmlMode::CdataEnd2 => {
                    let b = bytes[i];
                    i += 1;
                    self.mode = match b {
                        b'>' => XmlMode::Text,
                        b']' => XmlMode::CdataEnd2,
                        _ => XmlMode::Cdata,
                    };
                }
                XmlMode::Pi => {
                    if let Some(rel) = bytes[i..].iter().position(|&b| b == b'?') {
                        i += rel + 1;
                        self.mode = XmlMode::PiEnd;
                    } else {
                        break;
                    }
                }
                XmlMode::PiEnd => {
                    let b = bytes[i];
                    i += 1;
                    self.mode = match b {
                        b'>' => XmlMode::Text,
                        b'?' => XmlMode::PiEnd,
                        _ => XmlMode::Pi,
                    };
                }
            }
        }
    }

    pub(super) fn finish_document(&self, expected_root: Option<&str>) -> bool {
        !self.failed
            && self.utf8_tail_len == 0
            && self.mode == XmlMode::Text
            && self.depth == 0
            && self.roots == 1
            && self.closed
            && expected_root.is_none_or(|e| &self.root_local[..self.root_local_len] == e.as_bytes())
    }

    pub(super) fn finish_fragment(&self) -> bool {
        !self.failed
            && self.utf8_tail_len == 0
            && self.mode == XmlMode::Text
            && self.depth == 0
            && self.min_depth == 0
    }
}

pub(super) struct VerifiedXmlWriter<W> {
    inner: W,
    crc: flate2::Crc,
    size: u64,
    check: XmlCheckState,
}

impl<W: Write> VerifiedXmlWriter<W> {
    pub(super) fn new(inner: W) -> Self {
        Self {
            inner,
            crc: flate2::Crc::new(),
            size: 0,
            check: XmlCheckState::new_document(),
        }
    }

    pub(super) fn finish(mut self, expected_root: Option<&str>) -> Result<Option<(u32, u64)>> {
        self.inner.flush()?;
        Ok(self
            .check
            .finish_document(expected_root)
            .then_some((self.crc.sum(), self.size)))
    }
}

impl<W: Write> Write for VerifiedXmlWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write_all(buf)?;
        self.crc.update(buf);
        self.size += buf.len() as u64;
        self.check.feed(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Stream a producer into parallel RFC 1951 deflate workers (overlapping production with
/// compression across cores for entries > 2 MB), verify XML well-formedness on the fly,
/// and splice the resulting single-entry ZIP into `out` via `raw_copy_file`.
///
/// Returns:
/// - `Ok(None)` if `producer` returned `Ok(false)` (unsupported stream -> caller falls back).
/// - `Ok(Some(trusted_sig))` if written; `trusted_sig` is `Some((crc32, size))` when XML check passed.
pub(super) fn write_parallel_deflate_entry<W: Write + std::io::Seek>(
    out: &mut ZipWriter<W>,
    part: &str,
    level: u32,
    expected_root: Option<&str>,
    producer: impl FnOnce(&mut dyn Write) -> Result<bool>,
) -> Result<Option<Option<(u32, u64)>>> {
    use std::collections::VecDeque;
    use std::sync::{Condvar, Mutex};

    const CHUNK_BYTES: usize = 2 * 1024 * 1024;
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .saturating_sub(1)
        .clamp(1, 7);

    struct QueueState {
        items: VecDeque<(usize, Vec<u8>, bool)>,
        closed: bool,
        aborted: bool,
    }
    struct Queue {
        state: Mutex<QueueState>,
        not_empty: Condvar,
        not_full: Condvar,
        max_depth: usize,
    }
    impl Queue {
        fn push(&self, seq: usize, data: Vec<u8>, is_last: bool) {
            let mut g = self.state.lock().unwrap();
            while g.items.len() >= self.max_depth && !g.aborted {
                g = self.not_full.wait(g).unwrap();
            }
            if !g.aborted {
                g.items.push_back((seq, data, is_last));
                self.not_empty.notify_one();
            }
        }
        fn pop(&self) -> Option<(usize, Vec<u8>, bool)> {
            let mut g = self.state.lock().unwrap();
            loop {
                if g.aborted {
                    return None;
                }
                if let Some(item) = g.items.pop_front() {
                    self.not_full.notify_one();
                    return Some(item);
                }
                if g.closed {
                    return None;
                }
                g = self.not_empty.wait(g).unwrap();
            }
        }
        fn close(&self) {
            let mut g = self.state.lock().unwrap();
            g.closed = true;
            self.not_empty.notify_all();
        }
        fn abort(&self) {
            let mut g = self.state.lock().unwrap();
            g.aborted = true;
            g.items.clear();
            self.not_empty.notify_all();
            self.not_full.notify_all();
        }
    }

    struct PipeWriter<'a> {
        queue: &'a Queue,
        current: Vec<u8>,
        pending: Option<Vec<u8>>,
        seq: usize,
        dispatched: bool,
        check: XmlCheckState,
    }
    impl PipeWriter<'_> {
        fn dispatch_if_ready(&mut self) {
            if let Some(prev) = self.pending.take() {
                let seq = self.seq;
                self.seq += 1;
                self.dispatched = true;
                self.queue.push(seq, prev, false);
            }
            let finished = std::mem::replace(&mut self.current, Vec::with_capacity(CHUNK_BYTES + 4096));
            self.pending = Some(finished);
        }
    }
    impl Write for PipeWriter<'_> {
        fn write(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
            let total = buf.len();
            self.check.feed(buf);
            while !buf.is_empty() {
                let space = CHUNK_BYTES.saturating_sub(self.current.len());
                if space == 0 {
                    self.dispatch_if_ready();
                    continue;
                }
                let take = buf.len().min(space);
                self.current.extend_from_slice(&buf[..take]);
                buf = &buf[take..];
                if self.current.len() >= CHUNK_BYTES {
                    self.dispatch_if_ready();
                }
            }
            Ok(total)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let queue = Queue {
        state: Mutex::new(QueueState {
            items: VecDeque::new(),
            closed: false,
            aborted: false,
        }),
        not_empty: Condvar::new(),
        not_full: Condvar::new(),
        max_depth: workers * 2,
    };
    let compressed_out: Mutex<Vec<Result<(usize, Vec<u8>, u32, u64)>>> = Mutex::new(Vec::new());

    let mut single_buf: Option<Vec<u8>> = None;
    let mut xml_ok = false;
    let mut produced_ok = false;

    std::thread::scope(|scope| -> Result<()> {
        // Workers are only spawned once input actually exceeds CHUNK_BYTES, keeping small sheets zero-overhead.
        let pw = PipeWriter {
            queue: &queue,
            current: Vec::with_capacity(64 * 1024),
            pending: None,
            seq: 0,
            dispatched: false,
            check: XmlCheckState::new_document(),
        };

        // Spawn workers eagerly only if we want streaming overlap; actually spawning up to `workers`
        // threads inside `scope` takes ~0.15 ms on Windows, OR we can spawn lazily on first dispatch!
        // Let's check: a 2 MB threshold means small sheets NEVER spawn threads!
        struct LazyPool<'s, 'a> {
            scope: &'s std::thread::Scope<'s, 'a>,
            queue: &'a Queue,
            out: &'a Mutex<Vec<Result<(usize, Vec<u8>, u32, u64)>>>,
            workers: usize,
            level: u32,
            spawned: bool,
        }
        impl<'s, 'a> LazyPool<'s, 'a> {
            fn ensure(&mut self) {
                if self.spawned {
                    return;
                }
                self.spawned = true;
                for _ in 0..self.workers {
                    let q = self.queue;
                    let o = self.out;
                    let lvl = self.level;
                    self.scope.spawn(move || {
                        while let Some((seq, data, is_last)) = q.pop() {
                            let raw_len = data.len() as u64;
                            let res = deflate_raw_chunk(&data, lvl, is_last)
                                .map(|(comp, crc)| (seq, comp, crc, raw_len));
                            o.lock().unwrap().push(res);
                        }
                    });
                }
            }
        }

        struct LazyPipeWriter<'s, 'a> {
            inner: PipeWriter<'a>,
            pool: LazyPool<'s, 'a>,
        }
        impl Write for LazyPipeWriter<'_, '_> {
            fn write(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
                let total = buf.len();
                self.inner.check.feed(buf);
                while !buf.is_empty() {
                    let space = CHUNK_BYTES.saturating_sub(self.inner.current.len());
                    if space == 0 {
                        if self.inner.pending.is_some() {
                            self.pool.ensure();
                        }
                        self.inner.dispatch_if_ready();
                        continue;
                    }
                    let take = buf.len().min(space);
                    self.inner.current.extend_from_slice(&buf[..take]);
                    buf = &buf[take..];
                    if self.inner.current.len() >= CHUNK_BYTES {
                        if self.inner.pending.is_some() {
                            self.pool.ensure();
                        }
                        self.inner.dispatch_if_ready();
                    }
                }
                Ok(total)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut lpw = LazyPipeWriter {
            inner: pw,
            pool: LazyPool {
                scope,
                queue: &queue,
                out: &compressed_out,
                workers,
                level,
                spawned: false,
            },
        };
        let res = producer(&mut lpw);
        match res {
            Ok(true) => {
                produced_ok = true;
                xml_ok = lpw.inner.check.finish_document(expected_root);
                if !lpw.inner.dispatched && lpw.inner.pending.is_none() {
                    single_buf = Some(lpw.inner.current);
                    queue.close();
                } else if !lpw.inner.dispatched && lpw.inner.current.is_empty() {
                    single_buf = lpw.inner.pending.take();
                    queue.close();
                } else {
                    lpw.pool.ensure();
                    if let Some(prev) = lpw.inner.pending.take() {
                        let is_last = lpw.inner.current.is_empty();
                        let seq = lpw.inner.seq;
                        lpw.inner.seq += 1;
                        queue.push(seq, prev, is_last);
                    }
                    if !lpw.inner.current.is_empty() {
                        let seq = lpw.inner.seq;
                        queue.push(seq, lpw.inner.current, true);
                    }
                    queue.close();
                }
            }
            Ok(false) => {
                queue.abort();
            }
            Err(e) => {
                queue.abort();
                return Err(e);
            }
        }
        Ok(())
    })?;

    if !produced_ok {
        return Ok(None);
    }

    if let Some(data) = single_buf {
        let mut crc = flate2::Crc::new();
        crc.update(&data);
        let sig = (crc.sum(), data.len() as u64);
        out.start_file(
            part,
            SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(i64::from(level))),
        )?;
        out.write_all(&data)?;
        return Ok(Some(xml_ok.then_some(sig)));
    }

    let mut chunks: Vec<(usize, Vec<u8>, u32, u64)> = Vec::new();
    for item in compressed_out.into_inner().unwrap() {
        chunks.push(item?);
    }
    chunks.sort_unstable_by_key(|(seq, _, _, _)| *seq);
    let mut total_crc = 0u32;
    let mut total_len = 0u64;
    let mut comp_slices = Vec::with_capacity(chunks.len());
    for (_, comp, crc, raw_len) in chunks {
        total_crc = if total_len == 0 {
            crc
        } else {
            crc32_combine(total_crc, crc, raw_len)
        };
        total_len += raw_len;
        comp_slices.push(comp);
    }
    let mini = build_single_entry_zip(part, total_crc, total_len, &comp_slices)?;
    let mut mini_zip = ZipArchive::new(std::io::Cursor::new(mini))?;
    out.raw_copy_file(mini_zip.by_index_raw(0)?)?;
    Ok(Some(xml_ok.then_some((total_crc, total_len))))
}

#[inline]
fn push_u32(out: &mut Vec<u8>, mut v: u32) {
    let mut tmp = [0u8; 10];
    let mut len = 0usize;
    loop {
        tmp[len] = b'0' + (v % 10) as u8;
        len += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in (0..len).rev() {
        out.push(tmp[i]);
    }
}

#[inline]
fn push_u16(out: &mut Vec<u8>, v: u16) {
    push_u32(out, u32::from(v));
}

#[inline]
fn push_i64(out: &mut Vec<u8>, v: i64) {
    let mut mag = if v < 0 {
        out.push(b'-');
        v.unsigned_abs()
    } else {
        v as u64
    };
    let mut tmp = [0u8; 20];
    let mut len = 0usize;
    loop {
        tmp[len] = b'0' + (mag % 10) as u8;
        len += 1;
        mag /= 10;
        if mag == 0 {
            break;
        }
    }
    for i in (0..len).rev() {
        out.push(tmp[i]);
    }
}

#[inline]
pub(super) fn push_f64_xml(out: &mut Vec<u8>, n: f64) {
    let n = if n == 0.0 { 0.0 } else { n };
    let iv = n as i64;
    if (iv as f64) == n && iv.unsigned_abs() < 10_000_000_000_000_000 {
        push_i64(out, iv);
    } else {
        let _ = serde_json::to_writer(out, &n);
    }
}

#[inline]
fn push_pt_2dec(out: &mut Vec<u8>, px: f32) {
    let pt = f64::from(px * 72.0 / 96.0);
    let scaled = pt * 100.0;
    let rounded = scaled.round();
    if pt >= 0.0 && pt < 1_000_000.0 && (scaled - rounded).abs() < 1e-3 {
        let centi = rounded as u64;
        push_i64(out, (centi / 100) as i64);
        out.push(b'.');
        out.push(b'0' + ((centi / 10) % 10) as u8);
        out.push(b'0' + (centi % 10) as u8);
    } else {
        let _ = write!(out, "{:.2}", px * 72.0 / 96.0);
    }
}

#[inline]
fn push_a1(out: &mut Vec<u8>, row: u32, col: u32) {
    let mut c = col + 1;
    let mut col_tmp = [0u8; 4];
    let mut clen = 0usize;
    while c > 0 {
        let rem = ((c - 1) % 26) as u8;
        col_tmp[clen] = b'A' + rem;
        clen += 1;
        c = (c - 1) / 26;
    }
    for i in (0..clen).rev() {
        out.push(col_tmp[i]);
    }
    push_u32(out, row + 1);
}

#[inline]
pub(super) fn format_a1_buf(row: u32, col: u32, buf: &mut [u8; 16]) -> &str {
    let mut c = col + 1;
    let mut col_tmp = [0u8; 4];
    let mut clen = 0usize;
    while c > 0 {
        let rem = ((c - 1) % 26) as u8;
        col_tmp[clen] = b'A' + rem;
        clen += 1;
        c = (c - 1) / 26;
    }
    let mut pos = 0usize;
    for i in (0..clen).rev() {
        buf[pos] = col_tmp[i];
        pos += 1;
    }
    let mut r = row + 1;
    let mut row_tmp = [0u8; 10];
    let mut rlen = 0usize;
    while r > 0 {
        row_tmp[rlen] = b'0' + (r % 10) as u8;
        rlen += 1;
        r /= 10;
    }
    for i in (0..rlen).rev() {
        buf[pos] = row_tmp[i];
        pos += 1;
    }
    // SAFETY: only ASCII uppercase letters and digits were written.
    unsafe { std::str::from_utf8_unchecked(&buf[..pos]) }
}

fn declare_sst(xml: &mut String, part: &str) -> Result<()> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let (closing, record) = if part == super::dynamic::RELS {
        if root.children().any(|r| r.attribute("Type") == Some(SST_REL)) {
            return Ok(());
        }
        let mut n = 1usize;
        let id = loop {
            let candidate = format!("rId{n}");
            if !root.children().any(|r| r.attribute("Id") == Some(&candidate)) {
                break candidate;
            }
            n += 1;
        };
        (
            "</Relationships>",
            format!("<Relationship Id=\"{id}\" Type=\"{SST_REL}\" Target=\"sharedStrings.xml\"/>"),
        )
    } else {
        if root
            .children()
            .any(|r| r.attribute("PartName") == Some("/xl/sharedStrings.xml"))
        {
            return Ok(());
        }
        (
            "</Types>",
            format!("<Override PartName=\"/xl/sharedStrings.xml\" ContentType=\"{SST_CONTENT_TYPE}\"/>"),
        )
    };
    let index = xml[..root.range().end]
        .rfind(closing)
        .context("共享字符串清单无效")?;
    xml.insert_str(index, &record);
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) struct SheetSstLookup<'a> {
    str_map: &'a HashMap<&'a str, u32>,
    ptr_map: &'a HashMap<(usize, usize), u32>,
}

impl<'a> SheetSstLookup<'a> {
    #[inline]
    pub(super) fn from_maps(
        str_map: &'a HashMap<&'a str, u32>,
        ptr_map: &'a HashMap<(usize, usize), u32>,
    ) -> Self {
        Self { str_map, ptr_map }
    }

    #[inline]
    pub(super) fn get_arc(&self, s: &std::sync::Arc<str>) -> Option<u32> {
        let key = (s.as_ptr() as usize, s.len());
        if let Some(&idx) = self.ptr_map.get(&key) {
            return Some(idx);
        }
        self.str_map.get(s.as_ref()).copied()
    }

    #[inline]
    pub(super) fn get_str(&self, s: &str) -> Option<u32> {
        self.str_map.get(s).copied()
    }
}

struct FreshSst<'a> {
    indices: HashMap<&'a str, u32>,
    large_indices: HashMap<&'a str, u32>,
    indices_ptr: HashMap<(usize, usize), u32>,
    large_indices_ptr: HashMap<(usize, usize), u32>,
    xml: Vec<u8>,
    existed_in_zip: bool,
    modified: bool,
}

impl<'a> FreshSst<'a> {
    fn map_for(&self, sheet: &Sheet) -> SheetSstLookup<'_> {
        if sheet.len() < 16_384 {
            SheetSstLookup {
                str_map: &self.indices,
                ptr_map: &self.indices_ptr,
            }
        } else {
            SheetSstLookup {
                str_map: &self.large_indices,
                ptr_map: &self.large_indices_ptr,
            }
        }
    }

    fn build(
        zip: Option<&mut ZipArchive<File>>,
        ordered_sheets: &[(&str, &'a Sheet)],
    ) -> Result<Option<Self>> {
        let mut large_counts: HashMap<&'a str, u32> = HashMap::new();
        let mut large_ptr_counts: HashMap<(usize, usize), (u32, &'a str)> = HashMap::new();
        let mut large_repeated: Vec<&'a str> = Vec::new();
        let mut small_seen: HashSet<&'a str> = HashSet::new();
        let mut small_ptr_seen: HashMap<(usize, usize), &'a str> = HashMap::new();
        let mut small_order: Vec<&'a str> = Vec::new();

        for &(_, sheet) in ordered_sheets {
            let is_small = sheet.len() < 16_384;
            let has_spilled = !sheet.spilled.is_empty();
            let has_rich = !sheet.rich.is_empty();
            let mut record_arc = |s: &'a std::sync::Arc<str>| {
                let text: &'a str = s.as_ref();
                let pkey = (text.as_ptr() as usize, text.len());
                if is_small {
                    if let std::collections::hash_map::Entry::Vacant(v) = small_ptr_seen.entry(pkey)
                    {
                        v.insert(text);
                        if small_seen.insert(text) {
                            small_order.push(text);
                        }
                    }
                } else {
                    let entry = large_ptr_counts.entry(pkey).or_insert((0, text));
                    if entry.0 >= 2 {
                        return;
                    }
                    entry.0 += 1;
                    let sc = large_counts.entry(text).or_insert(0);
                    *sc += 1;
                    if *sc == 2 {
                        large_repeated.push(text);
                    }
                }
            };
            for (pi, page) in sheet.cells().iter_pages() {
                let base = pi * crate::store::PAGE_ROWS;
                for (col, slab) in page.cols.iter_entries() {
                    match slab.as_ref() {
                        crate::store::ColSlab::Numbers(_) => continue, // O(1) skip: zero strings!
                        crate::store::ColSlab::Values(vs) => {
                            for (r_off, val) in vs.iter().enumerate() {
                                let Value::Text(s) = val else { continue };
                                let row = base + r_off as u32;
                                if (has_spilled && sheet.spilled.contains(&CellRef::new(row, col)))
                                    || (has_rich
                                        && sheet
                                            .rich
                                            .get(&(row, col))
                                            .is_some_and(|runs| !runs.is_empty()))
                                {
                                    continue;
                                }
                                record_arc(s);
                            }
                        }
                        crate::store::ColSlab::Mixed(cs) => {
                            for (r_off, cell) in cs.iter().enumerate() {
                                if cell.formula.is_some() {
                                    continue;
                                }
                                let Value::Text(s) = &cell.value else { continue };
                                let row = base + r_off as u32;
                                if (has_spilled && sheet.spilled.contains(&CellRef::new(row, col)))
                                    || (has_rich
                                        && sheet
                                            .rich
                                            .get(&(row, col))
                                            .is_some_and(|runs| !runs.is_empty()))
                                {
                                    continue;
                                }
                                record_arc(s);
                            }
                        }
                    }
                }
            }
        }

        if large_repeated.is_empty() && small_order.is_empty() {
            return Ok(None);
        }

        let mut all_needed: Vec<&'a str> =
            Vec::with_capacity(large_repeated.len() + small_order.len());
        let mut needed_seen: HashSet<&'a str> =
            HashSet::with_capacity(large_repeated.len() + small_order.len());
        for s in &large_repeated {
            if needed_seen.insert(*s) {
                all_needed.push(*s);
            }
        }
        for s in &small_order {
            if needed_seen.insert(*s) {
                all_needed.push(*s);
            }
        }

        let existed_in_zip = zip
            .as_ref()
            .is_some_and(|z| z.index_for_name(SST_PART).is_some());
        let mut xml = String::new();
        let mut next_idx: u32 = 0;
        let mut existing_plain: HashMap<String, u32> = HashMap::new();
        let mut sst_prefix = String::new();
        let mut root_close = String::from("</sst>");

        if existed_in_zip {
            let existing = super::read_zip_text(zip.unwrap(), SST_PART)?;
            // Single-pass streaming scan of existing sharedStrings.xml (no full DOM).
            let mut reader = quick_xml::Reader::from_str(&existing);
            let mut depth = 0usize;
            let mut root_end = 0usize;
            let mut empty_root = false;
            let mut clean_open = String::new();
            let mut close_pos = None;
            let mut saw_root = false;
            let mut cur_si_idx: u32 = 0;
            let mut cur_si_plain = false;
            let mut cur_si_saw_t = false;
            let mut in_plain_t = false;
            let mut cur_t_buf = String::new();
            loop {
                let start = reader.buffer_position() as usize;
                let ev = reader.read_event()?;
                let end = reader.buffer_position() as usize;
                match ev {
                    quick_xml::events::Event::Start(ref e)
                    | quick_xml::events::Event::Empty(ref e) => {
                        let empty = matches!(ev, quick_xml::events::Event::Empty(_));
                        if depth == 0 {
                            ensure!(
                                !saw_root && e.local_name().as_ref() == "sst",
                                "共享字符串根节点无效"
                            );
                            saw_root = true;
                            empty_root = empty;
                            root_end = end;
                            let qname = e.name().as_ref().to_owned();
                            if let Some((pfx, _)) = qname.split_once(':') {
                                sst_prefix = format!("{pfx}:");
                            }
                            root_close = format!("</{qname}>");
                            let mut tag = quick_xml::events::BytesStart::new(qname);
                            for a in e.attributes() {
                                let a = a?;
                                if !matches!(a.key.as_ref(), "count" | "uniqueCount") {
                                    tag.push_attribute(a);
                                }
                            }
                            let mut w = quick_xml::Writer::new(Vec::new());
                            w.write_event(quick_xml::events::Event::Start(tag))?;
                            clean_open = String::from_utf8(w.into_inner())?;
                            if !empty {
                                depth = 1;
                            }
                        } else if depth == 1 {
                            if e.local_name().as_ref() == "si" {
                                cur_si_idx = next_idx;
                                next_idx =
                                    next_idx.checked_add(1).context("共享字符串索引溢出")?;
                                cur_si_plain = !empty;
                                cur_si_saw_t = false;
                                in_plain_t = false;
                                cur_t_buf.clear();
                            }
                            if !empty {
                                depth = 2;
                            }
                        } else if depth == 2 {
                            if cur_si_plain
                                && !cur_si_saw_t
                                && e.local_name().as_ref() == "t"
                            {
                                cur_si_saw_t = true;
                                if !empty {
                                    in_plain_t = true;
                                }
                            } else {
                                cur_si_plain = false;
                                in_plain_t = false;
                            }
                            if !empty {
                                depth = 3;
                            }
                        } else {
                            cur_si_plain = false;
                            in_plain_t = false;
                            if !empty {
                                depth += 1;
                            }
                        }
                    }
                    quick_xml::events::Event::Text(t) => {
                        if in_plain_t {
                            cur_t_buf.push_str(&t.xml10_content());
                        }
                    }
                    quick_xml::events::Event::GeneralRef(g) => {
                        if in_plain_t {
                            if let Ok(Some(ch)) = g.resolve_char_ref() {
                                cur_t_buf.push(ch);
                            } else {
                                let s = crate::model::resolve_entity(g.as_ref().as_ref());
                                if s.is_empty() {
                                    cur_si_plain = false;
                                    in_plain_t = false;
                                } else {
                                    cur_t_buf.push_str(&s);
                                }
                            }
                        }
                    }
                    quick_xml::events::Event::CData(_) => {
                        if depth >= 2 {
                            cur_si_plain = false;
                            in_plain_t = false;
                        }
                    }
                    quick_xml::events::Event::End(_) => {
                        depth = depth.checked_sub(1).context("共享字符串标签不匹配")?;
                        if depth == 2 {
                            in_plain_t = false;
                        } else if depth == 1 {
                            if cur_si_plain && cur_si_saw_t {
                                let decoded = decode_xstring(&cur_t_buf).into_owned();
                                existing_plain.entry(decoded).or_insert(cur_si_idx);
                            }
                            cur_si_plain = false;
                            cur_si_saw_t = false;
                            in_plain_t = false;
                        } else if depth == 0 {
                            close_pos = Some(start);
                        }
                    }
                    quick_xml::events::Event::Eof => break,
                    _ => {}
                }
            }
            ensure!(saw_root && depth == 0, "共享字符串缺少根标签");
            xml.reserve(existing.len() + all_needed.len() * 32);
            xml.push_str(&clean_open);
            if !empty_root {
                let close = close_pos.context("共享字符串缺少闭合标签")?;
                xml.push_str(&existing[root_end..close]);
            }
        } else {
            xml.reserve(160 + all_needed.len() * 32);
            xml.push_str(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
            );
        }

        let mut indices = HashMap::with_capacity(all_needed.len());
        let mut appended_any = false;
        for s in all_needed {
            if let Some(&idx) = existing_plain.get(s) {
                indices.insert(s, idx);
                continue;
            }
            let idx = next_idx;
            next_idx = next_idx.checked_add(1).context("共享字符串索引溢出")?;
            indices.insert(s, idx);
            appended_any = true;
            let preserve = s.starts_with(char::is_whitespace)
                || s.ends_with(char::is_whitespace)
                || s.bytes().any(|b| matches!(b, b'\t' | b'\r' | b'\n'));
            xml.push('<');
            xml.push_str(&sst_prefix);
            xml.push_str("si><");
            xml.push_str(&sst_prefix);
            if preserve {
                xml.push_str("t xml:space=\"preserve\">");
            } else {
                xml.push_str("t>");
            }
            xml.push_str(&encode_xstring_xml(s));
            xml.push_str("</");
            xml.push_str(&sst_prefix);
            xml.push_str("t></");
            xml.push_str(&sst_prefix);
            xml.push_str("si>");
        }

        let mut large_indices = HashMap::with_capacity(large_repeated.len());
        for s in large_repeated {
            if let Some(&idx) = indices.get(s) {
                large_indices.insert(s, idx);
            }
        }

        let mut indices_ptr = HashMap::with_capacity(small_ptr_seen.len() + large_ptr_counts.len());
        let mut large_indices_ptr = HashMap::with_capacity(large_ptr_counts.len());
        for (pkey, text) in small_ptr_seen {
            if let Some(&idx) = indices.get(text) {
                indices_ptr.insert(pkey, idx);
            }
        }
        for (pkey, (_, text)) in large_ptr_counts {
            if let Some(&idx) = indices.get(text) {
                indices_ptr.insert(pkey, idx);
            }
            if let Some(&idx) = large_indices.get(text) {
                large_indices_ptr.insert(pkey, idx);
            }
        }

        let modified = !existed_in_zip || appended_any;
        let final_xml = if modified {
            xml.push_str(&root_close);
            xml.into_bytes()
        } else {
            Vec::new()
        };

        Ok(Some(Self {
            indices,
            large_indices,
            indices_ptr,
            large_indices_ptr,
            xml: final_xml,
            existed_in_zip,
            modified,
        }))
    }
}

fn extract_dxfs_slice(xml: &str) -> Option<&str> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut depth = 0usize;
    let mut dxfs_start = None;
    loop {
        let start = reader.buffer_position() as usize;
        match reader.read_event().ok()? {
            quick_xml::events::Event::Start(e) => {
                if depth == 1 && e.local_name().as_ref() == "dxfs" {
                    dxfs_start = Some(start);
                }
                depth += 1;
            }
            quick_xml::events::Event::Empty(e) => {
                if depth == 1 && e.local_name().as_ref() == "dxfs" {
                    let end = reader.buffer_position() as usize;
                    return Some(&xml[start..end]);
                }
            }
            quick_xml::events::Event::End(_) => {
                depth = depth.checked_sub(1)?;
                if depth == 1
                    && let Some(ds) = dxfs_start.take()
                {
                    let end = reader.buffer_position() as usize;
                    return Some(&xml[ds..end]);
                }
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    None
}

fn push_color_tag(
    out: &mut String,
    prefix: &str,
    tag: &str,
    rgb: Option<[u8; 3]>,
    theme: Option<(u8, f32)>,
    indexed: Option<u8>,
) {
    use std::fmt::Write as _;
    if let Some(i) = indexed {
        let _ = write!(out, "<{prefix}{tag} indexed=\"{i}\"/>");
    } else if let Some((idx, tint)) = theme {
        if tint != 0.0 {
            let _ = write!(out, "<{prefix}{tag} theme=\"{idx}\" tint=\"{tint}\"/>");
        } else {
            let _ = write!(out, "<{prefix}{tag} theme=\"{idx}\"/>");
        }
    } else if let Some([r, g, b]) = rgb {
        let _ = write!(out, "<{prefix}{tag} rgb=\"FF{r:02X}{g:02X}{b:02X}\"/>");
    }
}

fn style_ref<'a>(
    wb: &'a Workbook,
    sid: u16,
    fallback: &'a crate::style::CellStyle,
) -> &'a crate::style::CellStyle {
    if wb.styles.is_empty() {
        fallback
    } else {
        wb.styles.get(sid)
    }
}

fn format_font_xml(
    st: &crate::style::CellStyle,
    theme: &crate::style_theme::XfTheme,
    prefix: &str,
    out: &mut String,
) {
    use std::fmt::Write as _;
    out.clear();
    let _ = write!(out, "<{prefix}font>");
    if st.bold {
        let _ = write!(out, "<{prefix}b/>");
    }
    if st.italic {
        let _ = write!(out, "<{prefix}i/>");
    }
    if st.strikethrough {
        let _ = write!(out, "<{prefix}strike/>");
    }
    if st.underline {
        if st.double_underline {
            let _ = write!(out, "<{prefix}u val=\"double\"/>");
        } else {
            let _ = write!(out, "<{prefix}u/>");
        }
    }
    let sz = if st.size >= 1.0 && st.size.is_finite() {
        st.size
    } else {
        11.0
    };
    let _ = write!(out, "<{prefix}sz val=\"{sz}\"/>");
    if let Some(t) = theme.font {
        push_color_tag(out, prefix, "color", None, Some((t.idx, t.tint)), None);
    } else {
        push_color_tag(out, prefix, "color", Some(st.color), None, None);
    }
    let fname = if st.font.is_empty() {
        "Calibri"
    } else {
        st.font.as_str()
    };
    let escaped_font = encode_xml_attr(fname);
    let _ = write!(out, "<{prefix}name val=\"{escaped_font}\"/></{prefix}font>");
}

fn format_fill_xml(
    st: &crate::style::CellStyle,
    theme: &crate::style_theme::XfTheme,
    pattern: Option<&crate::style_ext::FillPattern>,
    prefix: &str,
    out: &mut String,
) -> bool {
    use std::fmt::Write as _;
    if let Some(p) = pattern {
        if p.kind.is_empty() || p.kind == "none" {
            return false;
        }
        out.clear();
        let ek = encode_xml_attr(&p.kind);
        let has_fg = p.fg.is_some() || p.fg_theme.is_some() || p.fg_indexed.is_some();
        let has_bg = p.bg.is_some() || p.bg_theme.is_some() || p.bg_indexed.is_some();
        if has_fg || has_bg {
            let _ = write!(out, "<{prefix}fill><{prefix}patternFill patternType=\"{ek}\">");
            push_color_tag(out, prefix, "fgColor", p.fg, p.fg_theme, p.fg_indexed);
            push_color_tag(out, prefix, "bgColor", p.bg, p.bg_theme, p.bg_indexed);
            let _ = write!(out, "</{prefix}patternFill></{prefix}fill>");
        } else {
            let _ = write!(out, "<{prefix}fill><{prefix}patternFill patternType=\"{ek}\"/></{prefix}fill>");
        }
        true
    } else if let Some(rgb) = st.fill {
        out.clear();
        let _ = write!(out, "<{prefix}fill><{prefix}patternFill patternType=\"solid\">");
        if let Some(t) = theme.fill {
            push_color_tag(out, prefix, "fgColor", None, Some((t.idx, t.tint)), None);
        } else {
            push_color_tag(out, prefix, "fgColor", Some(rgb), None, None);
        }
        let _ = write!(out, "</{prefix}patternFill></{prefix}fill>");
        true
    } else {
        false
    }
}

fn format_border_xml(
    st: &crate::style::CellStyle,
    diag: Option<&crate::style_ext::DiagBorder>,
    prefix: &str,
    out: &mut String,
) -> bool {
    use std::fmt::Write as _;
    if st.border.iter().all(|b| b.style == 0) && diag.is_none() {
        return false;
    }
    out.clear();
    let _ = write!(out, "<{prefix}border");
    if let Some(d) = diag {
        if d.up {
            out.push_str(" diagonalUp=\"1\"");
        }
        if d.down {
            out.push_str(" diagonalDown=\"1\"");
        }
    }
    out.push('>');
    for (side_name, b) in [
        ("left", &st.border[0]),
        ("right", &st.border[1]),
        ("top", &st.border[2]),
        ("bottom", &st.border[3]),
    ] {
        if b.style == 0 {
            let _ = write!(out, "<{prefix}{side_name}/>");
        } else {
            let sname = crate::style::Border::ooxml_name(b.style);
            let _ = write!(
                out,
                "<{prefix}{side_name} style=\"{sname}\"><{prefix}color rgb=\"FF{:02X}{:02X}{:02X}\"/></{prefix}{side_name}>",
                b.color[0], b.color[1], b.color[2]
            );
        }
    }
    if let Some(d) = diag {
        let dstyle = encode_xml_attr(&d.style);
        let _ = write!(out, "<{prefix}diagonal style=\"{dstyle}\">");
        push_color_tag(out, prefix, "color", Some(d.color), d.color_theme, None);
        let _ = write!(out, "</{prefix}diagonal>");
    } else {
        let _ = write!(out, "<{prefix}diagonal/>");
    }
    let _ = write!(out, "</{prefix}border>");
    true
}

#[allow(clippy::too_many_arguments)]
fn format_xf_xml(
    st: &crate::style::CellStyle,
    quote: bool,
    nf_id: u16,
    font_id: u16,
    fill_id: u16,
    border_id: u16,
    prefix: &str,
    out: &mut String,
) {
    use std::fmt::Write as _;
    let has_align = st.halign != 0
        || st.valign != 0
        || st.wrap
        || st.shrink_to_fit
        || st.indent > 0
        || st.vertical_text
        || st.rotation != 0;
    let has_prot = !st.locked || st.hidden;

    out.clear();
    let _ = write!(
        out,
        "<{prefix}xf numFmtId=\"{nf_id}\" fontId=\"{font_id}\" fillId=\"{fill_id}\" borderId=\"{border_id}\" xfId=\"0\""
    );
    if nf_id != 0 {
        out.push_str(" applyNumberFormat=\"1\"");
    }
    if font_id != 0 {
        out.push_str(" applyFont=\"1\"");
    }
    if fill_id != 0 {
        out.push_str(" applyFill=\"1\"");
    }
    if border_id != 0 {
        out.push_str(" applyBorder=\"1\"");
    }
    if has_align {
        out.push_str(" applyAlignment=\"1\"");
    }
    if has_prot {
        out.push_str(" applyProtection=\"1\"");
    }
    if quote {
        out.push_str(" quotePrefix=\"1\"");
    }
    if !has_align && !has_prot {
        out.push_str("/>");
    } else {
        out.push('>');
        if has_align {
            let _ = write!(out, "<{prefix}alignment");
            match st.halign {
                1 => out.push_str(" horizontal=\"left\""),
                2 => out.push_str(" horizontal=\"center\""),
                3 => out.push_str(" horizontal=\"right\""),
                4 => out.push_str(" horizontal=\"centerContinuous\""),
                5 => out.push_str(" horizontal=\"distributed\""),
                _ => {}
            }
            match st.valign {
                1 => out.push_str(" vertical=\"top\""),
                2 => out.push_str(" vertical=\"center\""),
                3 => out.push_str(" vertical=\"bottom\""),
                _ => {}
            }
            if st.wrap {
                out.push_str(" wrapText=\"1\"");
            }
            if st.shrink_to_fit {
                out.push_str(" shrinkToFit=\"1\"");
            }
            if st.indent > 0 {
                let _ = write!(out, " indent=\"{}\"", st.indent);
            }
            if st.vertical_text {
                out.push_str(" textRotation=\"255\"");
            } else if st.rotation != 0 {
                let v = if st.rotation >= 0 {
                    st.rotation as u32
                } else {
                    (90 - st.rotation) as u32
                };
                let _ = write!(out, " textRotation=\"{v}\"");
            }
            out.push_str("/>");
        }
        if has_prot {
            let _ = write!(
                out,
                "<{prefix}protection locked=\"{}\" hidden=\"{}\"/>",
                u8::from(st.locked),
                u8::from(st.hidden)
            );
        }
        let _ = write!(out, "</{prefix}xf>");
    }
}

struct FreshStyles {
    sid_to_xf: Vec<Option<u16>>,
    xml: Vec<u8>,
    sig: (u32, u64),
    saved: super::delta::SavedStyles,
}

impl FreshStyles {
    fn build(
        zip: Option<&mut ZipArchive<File>>,
        wb: &Workbook,
        ordered_sheets: &[(&str, &Sheet)],
    ) -> Result<Self> {
        use std::fmt::Write as _;
        let default_style = crate::style::CellStyle::default_dark();
        let style_count = wb.styles.len();
        let mut used = vec![false; style_count.max(1)];
        used[0] = true;
        for &(_, sheet) in ordered_sheets {
            if !sheet.styles.is_empty() {
                for (_, &sid) in sheet.styles.iter() {
                    if let Some(slot) = used.get_mut(usize::from(sid)) {
                        *slot = true;
                    }
                }
            }
            for &sid in sheet.row_styles.values() {
                if let Some(slot) = used.get_mut(usize::from(sid)) {
                    *slot = true;
                }
            }
            for &sid in sheet.col_styles.values() {
                if let Some(slot) = used.get_mut(usize::from(sid)) {
                    *slot = true;
                }
            }
        }

        let existing_styles = zip
            .filter(|z| z.index_for_name(STYLES_PART).is_some())
            .and_then(|z| super::read_zip_text(z, STYLES_PART).ok());
        let dxfs_xml = existing_styles
            .as_deref()
            .and_then(extract_dxfs_slice)
            .unwrap_or("<dxfs count=\"0\"/>");

        let mut num_fmts: Vec<(u16, String)> = Vec::new();
        let mut num_fmt_map: HashMap<String, u16> = HashMap::new();
        let mut next_num_fmt_id: u16 = 164;

        let default_font =
            "<font><sz val=\"11\"/><color rgb=\"FF000000\"/><name val=\"Calibri\"/></font>"
                .to_owned();
        let mut fonts: Vec<String> = vec![default_font.clone()];
        let mut font_map: HashMap<String, u16> = HashMap::from([(default_font, 0)]);

        let fill_none = "<fill><patternFill patternType=\"none\"/></fill>".to_owned();
        let fill_gray = "<fill><patternFill patternType=\"gray125\"/></fill>".to_owned();
        let mut fills: Vec<String> = vec![fill_none.clone(), fill_gray.clone()];
        let mut fill_map: HashMap<String, u16> = HashMap::from([(fill_none, 0), (fill_gray, 1)]);

        let border_none = "<border><left/><right/><top/><bottom/><diagonal/></border>".to_owned();
        let mut borders: Vec<String> = vec![border_none.clone()];
        let mut border_map: HashMap<String, u16> = HashMap::from([(border_none, 0)]);

        let active_count = used.iter().filter(|&&u| u).count();
        let mut cell_xfs: Vec<String> = Vec::with_capacity(active_count);
        let mut sid_to_xf: Vec<Option<u16>> = vec![None; style_count.max(1)];
        let mut xf_to_sid: HashMap<u16, u16> = HashMap::with_capacity(active_count);

        let mut scratch = String::with_capacity(256);

        for (sid_idx, &is_used) in used.iter().enumerate() {
            if !is_used {
                continue;
            }
            let sid = u16::try_from(sid_idx).context("样式索引溢出")?;
            let st = style_ref(wb, sid, &default_style);
            let theme = wb.styles.theme_of(sid);

            // 1. Number format
            let nf_id: u16 = if st.format.is_empty() || st.format == "General" {
                0
            } else if st.format == "@" {
                49
            } else if let Some(&id) = num_fmt_map.get(&st.format) {
                id
            } else {
                let id = next_num_fmt_id;
                next_num_fmt_id = next_num_fmt_id.checked_add(1).context("数字格式数量超限")?;
                num_fmt_map.insert(st.format.clone(), id);
                let escaped = encode_xml_attr(&st.format);
                num_fmts.push((
                    id,
                    format!("<numFmt numFmtId=\"{id}\" formatCode=\"{escaped}\"/>"),
                ));
                id
            };

            // 2. Font
            format_font_xml(st, &theme, "", &mut scratch);
            let font_id = if let Some(&id) = font_map.get(&scratch) {
                id
            } else {
                let id = u16::try_from(fonts.len()).context("字体数量超限")?;
                ensure!(id < 65_490, "字体数量超限");
                let owned = scratch.clone();
                font_map.insert(owned.clone(), id);
                fonts.push(owned);
                id
            };

            // 3. Fill
            let fill_id = if format_fill_xml(st, &theme, wb.styles.pattern_of(sid), "", &mut scratch)
            {
                if let Some(&id) = fill_map.get(&scratch) {
                    id
                } else {
                    let id = u16::try_from(fills.len()).context("填充数量超限")?;
                    ensure!(id < 65_490, "填充数量超限");
                    let owned = scratch.clone();
                    fill_map.insert(owned.clone(), id);
                    fills.push(owned);
                    id
                }
            } else {
                0
            };

            // 4. Border
            let border_id = if format_border_xml(st, wb.styles.diag_of(sid), "", &mut scratch) {
                if let Some(&id) = border_map.get(&scratch) {
                    id
                } else {
                    let id = u16::try_from(borders.len()).context("边框数量超限")?;
                    ensure!(id < 65_490, "边框数量超限");
                    let owned = scratch.clone();
                    border_map.insert(owned.clone(), id);
                    borders.push(owned);
                    id
                }
            } else {
                0
            };

            // 5. XF
            format_xf_xml(
                st,
                wb.styles.quote_of(sid),
                nf_id,
                font_id,
                fill_id,
                border_id,
                "",
                &mut scratch,
            );

            let xf = u16::try_from(cell_xfs.len()).context("单元格样式数量超限")?;
            ensure!(xf < 65_490, "单元格样式数量超限");
            cell_xfs.push(scratch.clone());
            sid_to_xf[sid_idx] = Some(xf);
            xf_to_sid.insert(xf, sid);
        }

        let est = 512
            + num_fmts.len() * 48
            + fonts.len() * 88
            + fills.len() * 88
            + borders.len() * 96
            + cell_xfs.len() * 96
            + dxfs_xml.len();
        let mut xml = String::with_capacity(est);
        xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">");
        if !num_fmts.is_empty() {
            let _ = write!(xml, "<numFmts count=\"{}\">", num_fmts.len());
            for (_, tag) in &num_fmts {
                xml.push_str(tag);
            }
            xml.push_str("</numFmts>");
        }
        let _ = write!(xml, "<fonts count=\"{}\">", fonts.len());
        for f in &fonts {
            xml.push_str(f);
        }
        xml.push_str("</fonts>");
        let _ = write!(xml, "<fills count=\"{}\">", fills.len());
        for f in &fills {
            xml.push_str(f);
        }
        xml.push_str("</fills>");
        let _ = write!(xml, "<borders count=\"{}\">", borders.len());
        for b in &borders {
            xml.push_str(b);
        }
        xml.push_str("</borders>");
        xml.push_str("<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>");
        let _ = write!(xml, "<cellXfs count=\"{}\">", cell_xfs.len());
        for x in &cell_xfs {
            xml.push_str(x);
        }
        xml.push_str("</cellXfs>");
        xml.push_str("<cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>");
        xml.push_str(dxfs_xml);
        xml.push_str("<tableStyles count=\"0\" defaultTableStyle=\"TableStyleMedium2\" defaultPivotStyle=\"PivotStyleLight16\"/></styleSheet>");

        let bytes = xml.into_bytes();
        let mut crc = flate2::Crc::new();
        crc.update(&bytes);
        let sig = (crc.sum(), bytes.len() as u64);
        let saved = super::delta::SavedStyles::from_direct(sig, &xf_to_sid, &wb.styles);
        Ok(Self {
            sid_to_xf,
            xml: bytes,
            sig,
            saved,
        })
    }
}

struct StyleSection {
    tag_name: String,
    existing: usize,
    count_range: Option<Range<usize>>,
    open_end_gt: usize,
    close_start: usize,
    is_empty_tag: bool,
    present: bool,
    insert_if_missing: usize,
    added: Vec<String>,
    lookup: HashMap<String, u16>,
}

impl StyleSection {
    fn intern(&mut self, item: String, limit_err: &str) -> Result<u16> {
        if let Some(&id) = self.lookup.get(&item) {
            return Ok(id);
        }
        let id = u16::try_from(self.existing + self.added.len()).context(limit_err.to_owned())?;
        ensure!(id < 65_490, "{limit_err}");
        self.lookup.insert(item.clone(), id);
        self.added.push(item);
        Ok(id)
    }

    fn collect_edits(&self, edits: &mut Vec<(Range<usize>, String)>) {
        if self.added.is_empty() {
            return;
        }
        let total = self.existing + self.added.len();
        let body = self.added.concat();
        if !self.present {
            edits.push((
                self.insert_if_missing..self.insert_if_missing,
                format!(
                    "<{} count=\"{total}\">{body}</{}>",
                    self.tag_name, self.tag_name
                ),
            ));
        } else if self.is_empty_tag {
            if let Some(cr) = &self.count_range {
                edits.push((cr.clone(), total.to_string()));
                edits.push((
                    self.close_start..self.close_start + 2,
                    format!(">{body}</{}>", self.tag_name),
                ));
            } else {
                edits.push((
                    self.close_start..self.close_start + 2,
                    format!(" count=\"{total}\">{body}</{}>", self.tag_name),
                ));
            }
        } else {
            if let Some(cr) = &self.count_range {
                edits.push((cr.clone(), total.to_string()));
            } else {
                edits.push((self.open_end_gt..self.open_end_gt, format!(" count=\"{total}\"")));
            }
            edits.push((self.close_start..self.close_start, body));
        }
    }
}

/// Appends new styles into an existing `xl/styles.xml` while keeping every
/// existing `0..N-1` XF, `<dxfs>`, `<tableStyles>`, `<colors>`, `<extLst>`,
/// and namespace prefix byte-for-byte intact.
struct StyleAppender {
    original: String,
    prefix: String,
    num_fmts: StyleSection,
    num_fmt_map: HashMap<String, u16>,
    next_num_fmt_id: u16,
    fonts: StyleSection,
    fills: StyleSection,
    borders: StyleSection,
    cell_xfs: StyleSection,
    sid_cache: HashMap<u16, u16>,
    scratch: String,
}

impl StyleAppender {
    fn new(original: String) -> Result<Self> {
        let doc = roxmltree::Document::parse(&original).context("解析原样式表失败")?;
        let root = doc.root_element();
        ensure!(root.tag_name().name() == "styleSheet", "样式表根节点无效");
        let root_start = root.range().start;
        let root_open_end = {
            let mut r = quick_xml::Reader::from_str(&original[root_start..]);
            r.read_event()?;
            root_start + r.buffer_position() as usize
        };
        let root_open = &original[root_start..root_open_end];
        let root_qname = root_open
            .strip_prefix('<')
            .unwrap_or("")
            .split(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
            .next()
            .unwrap_or("styleSheet");
        let prefix = root_qname
            .split_once(':')
            .map(|(p, _)| format!("{p}:"))
            .unwrap_or_default();
        let root_close_pos = original
            .rfind(&format!("</{root_qname}>"))
            .unwrap_or(original.len());

        let inspect_section = |local: &str,
                               child_local: &str,
                               insert_pos: usize|
         -> Result<StyleSection> {
            let tag_name = format!("{prefix}{local}");
            let Some(node) = root
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == local)
            else {
                return Ok(StyleSection {
                    tag_name,
                    existing: 0,
                    count_range: None,
                    open_end_gt: insert_pos,
                    close_start: insert_pos,
                    is_empty_tag: false,
                    present: false,
                    insert_if_missing: insert_pos,
                    added: Vec::new(),
                    lookup: HashMap::new(),
                });
            };
            let range = node.range();
            let raw = &original[range.clone()];
            let mut r = quick_xml::Reader::from_str(raw);
            let ev = r.read_event()?;
            let open_len = r.buffer_position() as usize;
            let is_empty_tag = matches!(ev, quick_xml::events::Event::Empty(_));
            let count_range = node.attribute_node("count").map(|a| a.range_value());
            let (open_end_gt, close_start) = if is_empty_tag {
                let slash = range.start + open_len - 2;
                (slash, slash)
            } else {
                let gt = range.start + open_len - 1;
                let close_tag_start = raw
                    .rfind("</")
                    .map(|p| range.start + p)
                    .context("样式容器缺少闭合标签")?;
                (gt, close_tag_start)
            };
            let mut existing = 0usize;
            let mut lookup = HashMap::new();
            for ch in node
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == child_local)
            {
                if let Ok(idx) = u16::try_from(existing) {
                    lookup
                        .entry(original[ch.range()].to_owned())
                        .or_insert(idx);
                }
                existing += 1;
            }
            let actual_tag_name = raw
                .strip_prefix('<')
                .unwrap_or(&tag_name)
                .split(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
                .next()
                .unwrap_or(&tag_name)
                .to_owned();
            Ok(StyleSection {
                tag_name: actual_tag_name,
                existing,
                count_range,
                open_end_gt,
                close_start,
                is_empty_tag,
                present: true,
                insert_if_missing: range.start,
                added: Vec::new(),
                lookup,
            })
        };

        let find_before = |after_names: &[&str]| -> usize {
            root.children()
                .filter(|n| n.is_element())
                .find(|n| after_names.contains(&n.tag_name().name()))
                .map(|n| n.range().start)
                .unwrap_or(root_close_pos)
        };

        let num_fmts_pos = find_before(&[
            "fonts",
            "fills",
            "borders",
            "cellStyleXfs",
            "cellXfs",
            "cellStyles",
            "dxfs",
            "tableStyles",
            "colors",
            "extLst",
        ]);
        let fonts_pos = find_before(&[
            "fills",
            "borders",
            "cellStyleXfs",
            "cellXfs",
            "cellStyles",
            "dxfs",
            "tableStyles",
            "colors",
            "extLst",
        ]);
        let fills_pos = find_before(&[
            "borders",
            "cellStyleXfs",
            "cellXfs",
            "cellStyles",
            "dxfs",
            "tableStyles",
            "colors",
            "extLst",
        ]);
        let borders_pos = find_before(&[
            "cellStyleXfs",
            "cellXfs",
            "cellStyles",
            "dxfs",
            "tableStyles",
            "colors",
            "extLst",
        ]);
        let xfs_pos = find_before(&["cellStyles", "dxfs", "tableStyles", "colors", "extLst"]);

        let num_fmts = inspect_section("numFmts", "numFmt", num_fmts_pos)?;
        let fonts = inspect_section("fonts", "font", fonts_pos)?;
        let fills = inspect_section("fills", "fill", fills_pos)?;
        let borders = inspect_section("borders", "border", borders_pos)?;
        let cell_xfs = inspect_section("cellXfs", "xf", xfs_pos)?;

        let mut num_fmt_map = HashMap::new();
        let mut next_num_fmt_id: u16 = 164;
        if let Some(nf_node) = root
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "numFmts")
        {
            for ch in nf_node
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == "numFmt")
            {
                if let (Some(id_s), Some(code)) =
                    (ch.attribute("numFmtId"), ch.attribute("formatCode"))
                    && let Ok(id) = id_s.parse::<u16>()
                {
                    num_fmt_map.entry(code.to_owned()).or_insert(id);
                    if id >= next_num_fmt_id {
                        next_num_fmt_id = id.saturating_add(1);
                    }
                }
            }
        }

        Ok(Self {
            original,
            prefix,
            num_fmts,
            num_fmt_map,
            next_num_fmt_id,
            fonts,
            fills,
            borders,
            cell_xfs,
            sid_cache: HashMap::new(),
            scratch: String::with_capacity(256),
        })
    }

    fn intern_style(&mut self, wb: &Workbook, sid: u16) -> Result<u16> {
        if let Some(&xf) = self.sid_cache.get(&sid) {
            return Ok(xf);
        }
        let default_style = crate::style::CellStyle::default_dark();
        let st = style_ref(wb, sid, &default_style);
        let theme = wb.styles.theme_of(sid);

        // 1. Number format
        let nf_id: u16 = if st.format.is_empty() || st.format == "General" {
            0
        } else if st.format == "@" {
            49
        } else if let Some(&id) = self.num_fmt_map.get(&st.format) {
            id
        } else {
            let id = self.next_num_fmt_id;
            ensure!(id < u16::MAX, "数字格式数量超限");
            self.next_num_fmt_id = id + 1;
            self.num_fmt_map.insert(st.format.clone(), id);
            let escaped = encode_xml_attr(&st.format);
            self.num_fmts.added.push(format!(
                "<{}numFmt numFmtId=\"{id}\" formatCode=\"{escaped}\"/>",
                self.prefix
            ));
            id
        };

        // 2. Font
        format_font_xml(st, &theme, &self.prefix, &mut self.scratch);
        let font_id = self.fonts.intern(self.scratch.clone(), "字体数量超限")?;

        // 3. Fill
        let fill_id = if format_fill_xml(
            st,
            &theme,
            wb.styles.pattern_of(sid),
            &self.prefix,
            &mut self.scratch,
        ) {
            self.fills.intern(self.scratch.clone(), "填充数量超限")?
        } else {
            0
        };

        // 4. Border
        let border_id = if format_border_xml(
            st,
            wb.styles.diag_of(sid),
            &self.prefix,
            &mut self.scratch,
        ) {
            self.borders.intern(self.scratch.clone(), "边框数量超限")?
        } else {
            0
        };

        // 5. XF
        format_xf_xml(
            st,
            wb.styles.quote_of(sid),
            nf_id,
            font_id,
            fill_id,
            border_id,
            &self.prefix,
            &mut self.scratch,
        );
        let xf = self
            .cell_xfs
            .intern(self.scratch.clone(), "单元格样式数量超限")?;
        self.sid_cache.insert(sid, xf);
        Ok(xf)
    }

    fn finish(self) -> Option<Vec<u8>> {
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        self.num_fmts.collect_edits(&mut edits);
        self.fonts.collect_edits(&mut edits);
        self.fills.collect_edits(&mut edits);
        self.borders.collect_edits(&mut edits);
        self.cell_xfs.collect_edits(&mut edits);
        if edits.is_empty() {
            return None;
        }
        edits.sort_by_key(|(r, _)| (r.start, r.end));
        let extra: usize = edits.iter().map(|(_, s)| s.len()).sum();
        let mut out = String::with_capacity(self.original.len() + extra);
        let mut last = 0usize;
        for (r, s) in edits {
            out.push_str(&self.original[last..r.start]);
            out.push_str(&s);
            last = r.end;
        }
        out.push_str(&self.original[last..]);
        Some(out.into_bytes())
    }
}

#[derive(Clone, Copy, Debug)]
struct SharedRun {
    r_start: u32,
    r_end: u32,
    si: u32,
}

#[derive(Clone, Copy, Debug)]
enum SharedCell {
    Master { r_end: u32, si: u32 },
    Child { si: u32 },
}

/// Compact per-column run plan for OOXML `<f t="shared" ...>` compression on fresh save.
/// Memory usage is O(number of contiguous formula runs), not O(number of cells).
struct SharedFormulaPlan {
    by_col: HashMap<u32, Vec<SharedRun>>,
}

impl SharedFormulaPlan {
    fn build(sheet: &Sheet, arrays: &HashMap<CellRef, (u32, u32, u32, u32)>) -> Option<Self> {
        if !sheet.has_formulas() {
            return None;
        }
        let mut by_col_cells: HashMap<u32, Vec<(u32, std::sync::Arc<str>)>> = HashMap::new();
        for (key, cell) in sheet.formula_cells() {
            if !arrays.is_empty() && arrays.contains_key(&key) {
                continue;
            }
            if !sheet.spill_shapes.is_empty() && sheet.spill_shapes.contains_key(&key) {
                continue;
            }
            if let Some(f) = cell.formula.as_ref() {
                if f.as_bytes().contains(&b'[') {
                    continue;
                }
                by_col_cells
                    .entry(key.col)
                    .or_default()
                    .push((key.row, std::sync::Arc::clone(f)));
            }
        }
        if by_col_cells.is_empty() {
            return None;
        }
        let mut cols: Vec<u32> = by_col_cells.keys().copied().collect();
        cols.sort_unstable();
        let mut by_col: HashMap<u32, Vec<SharedRun>> = HashMap::new();
        let mut next_si = 0u32;
        let mut prev_key = Vec::with_capacity(128);
        let mut cur_key = Vec::with_capacity(128);

        for col in cols {
            let Some(mut cells) = by_col_cells.remove(&col) else {
                continue;
            };
            if cells.len() < 2 {
                continue;
            }
            cells.sort_unstable_by_key(|(r, _)| *r);
            let mut runs = Vec::new();
            let mut run_start = 0u32;
            let mut run_end = 0u32;
            let mut run_active = false;
            prev_key.clear();

            for (row, f) in cells {
                let ok = crate::formula::compiled::encode_relative_formula_key(
                    f.as_ref(),
                    row,
                    col,
                    &mut cur_key,
                )
                .is_some();
                if ok && run_active && row == run_end.saturating_add(1) && cur_key == prev_key {
                    run_end = row;
                } else {
                    if run_active && run_end > run_start {
                        runs.push(SharedRun {
                            r_start: run_start,
                            r_end: run_end,
                            si: next_si,
                        });
                        next_si += 1;
                    }
                    if ok {
                        run_start = row;
                        run_end = row;
                        run_active = true;
                        std::mem::swap(&mut prev_key, &mut cur_key);
                    } else {
                        run_active = false;
                    }
                }
            }
            if run_active && run_end > run_start {
                runs.push(SharedRun {
                    r_start: run_start,
                    r_end: run_end,
                    si: next_si,
                });
                next_si += 1;
            }
            if !runs.is_empty() {
                by_col.insert(col, runs);
            }
        }
        (!by_col.is_empty()).then_some(Self { by_col })
    }

    #[inline]
    fn lookup(&self, key: CellRef) -> Option<SharedCell> {
        let runs = self.by_col.get(&key.col)?;
        let idx = runs.partition_point(|r| r.r_end < key.row);
        let run = runs.get(idx)?;
        if key.row < run.r_start || key.row > run.r_end {
            return None;
        }
        if key.row == run.r_start {
            Some(SharedCell::Master {
                r_end: run.r_end,
                si: run.si,
            })
        } else {
            Some(SharedCell::Child { si: run.si })
        }
    }
}

#[inline]
fn push_scalar_formula_tag(
    buf: &mut Vec<u8>,
    key: CellRef,
    f_raw: &str,
    shared: Option<&SharedFormulaPlan>,
) {
    match shared.and_then(|sp| sp.lookup(key)) {
        Some(SharedCell::Master { r_end, si }) => {
            buf.extend_from_slice(b"<f t=\"shared\" ref=\"");
            push_a1(buf, key.row, key.col);
            buf.push(b':');
            push_a1(buf, r_end, key.col);
            buf.extend_from_slice(b"\" si=\"");
            push_u32(buf, si);
            buf.extend_from_slice(b"\">");
            push_formula_xml_bytes(buf, f_raw);
            buf.extend_from_slice(b"</f>");
        }
        Some(SharedCell::Child { si }) => {
            buf.extend_from_slice(b"<f t=\"shared\" si=\"");
            push_u32(buf, si);
            buf.extend_from_slice(b"\"/>");
        }
        None => {
            buf.extend_from_slice(b"<f>");
            push_formula_xml_bytes(buf, f_raw);
            buf.extend_from_slice(b"</f>");
        }
    }
}

fn push_formula_xml_bytes(buf: &mut Vec<u8>, raw: &str) {
    let formula = crate::add_fn_prefixes(raw.strip_prefix('=').unwrap_or(raw));
    if formula
        .bytes()
        .all(|b| b >= 0x20 && !matches!(b, b'&' | b'<' | b'>'))
        && !formula.contains('\u{fffe}')
        && !formula.contains('\u{ffff}')
    {
        buf.extend_from_slice(formula.as_bytes());
        return;
    }
    for ch in formula.chars() {
        match ch {
            '&' => buf.extend_from_slice(b"&amp;"),
            '<' => buf.extend_from_slice(b"&lt;"),
            '>' => buf.extend_from_slice(b"&gt;"),
            '\r' => buf.extend_from_slice(b"&#13;"),
            '\t' | '\n' => {
                let mut tmp = [0u8; 4];
                buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
            }
            c if c < ' ' || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => {
                let mut tmp = [0u8; 4];
                buf.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_value_buf(
    buf: &mut Vec<u8>,
    sheet: &Sheet,
    arrays: &HashMap<CellRef, (u32, u32, u32, u32)>,
    sst: Option<SheetSstLookup<'_>>,
    key: CellRef,
    cell: crate::store::CellView<'_>,
    xf: Option<u16>,
    prefix: &str,
    cm_index: Option<u32>,
    shared: Option<&SharedFormulaPlan>,
) -> Result<()> {
    let runs = if sheet.rich.is_empty() {
        None
    } else {
        sheet.rich.get(&(key.row, key.col)).map(Vec::as_slice)
    };
    let has_valid_runs = match (runs, &cell) {
        (Some(r), _) if !r.is_empty() && !cell.has_formula() => {
            let text_opt = match &cell {
                crate::store::CellView::Value(Value::Text(s))
                | crate::store::CellView::Cell(Cell {
                    value: Value::Text(s),
                    ..
                }) => Some(s.as_ref()),
                _ => None,
            };
            text_opt.is_some_and(|t| r.iter().flat_map(|run| run.text.bytes()).eq(t.bytes()))
        }
        _ => false,
    };

    // Zero-allocation fast path for ordinary unprefixed scalar cells.
    if prefix.is_empty()
        && !cell.has_formula()
        && !has_valid_runs
        && (sheet.spill_shapes.is_empty() || !sheet.spill_shapes.contains_key(&key))
    {
        match cell {
            crate::store::CellView::Number(n) => {
                buf.extend_from_slice(b"<c r=\"");
                push_a1(buf, key.row, key.col);
                if let Some(xf) = xf {
                    buf.extend_from_slice(b"\" s=\"");
                    push_u16(buf, xf);
                }
                if n.is_finite() {
                    buf.extend_from_slice(b"\"><v>");
                    push_f64_xml(buf, n);
                    buf.extend_from_slice(b"</v></c>");
                } else {
                    buf.extend_from_slice(b"\" t=\"e\"><v>#NUM!</v></c>");
                }
                return Ok(());
            }
            crate::store::CellView::Value(val)
            | crate::store::CellView::Cell(Cell { value: val, .. }) => match val {
                Value::Empty => {
                    if let Some(xf) = xf {
                        buf.extend_from_slice(b"<c r=\"");
                        push_a1(buf, key.row, key.col);
                        buf.extend_from_slice(b"\" s=\"");
                        push_u16(buf, xf);
                        buf.extend_from_slice(b"\"/>");
                    }
                    return Ok(());
                }
                Value::Number(n) => {
                    buf.extend_from_slice(b"<c r=\"");
                    push_a1(buf, key.row, key.col);
                    if let Some(xf) = xf {
                        buf.extend_from_slice(b"\" s=\"");
                        push_u16(buf, xf);
                    }
                    if n.is_finite() {
                        buf.extend_from_slice(b"\"><v>");
                        push_f64_xml(buf, *n);
                        buf.extend_from_slice(b"</v></c>");
                    } else {
                        buf.extend_from_slice(b"\" t=\"e\"><v>#NUM!</v></c>");
                    }
                    return Ok(());
                }
                Value::Bool(b) => {
                    buf.extend_from_slice(b"<c r=\"");
                    push_a1(buf, key.row, key.col);
                    if let Some(xf) = xf {
                        buf.extend_from_slice(b"\" s=\"");
                        push_u16(buf, xf);
                    }
                    buf.extend_from_slice(if *b {
                        b"\" t=\"b\"><v>1</v></c>"
                    } else {
                        b"\" t=\"b\"><v>0</v></c>"
                    });
                    return Ok(());
                }
                Value::Error(e) => {
                    buf.extend_from_slice(b"<c r=\"");
                    push_a1(buf, key.row, key.col);
                    if let Some(xf) = xf {
                        buf.extend_from_slice(b"\" s=\"");
                        push_u16(buf, xf);
                    }
                    buf.extend_from_slice(b"\" t=\"e\"><v>");
                    buf.extend_from_slice(e.text().as_bytes());
                    buf.extend_from_slice(b"</v></c>");
                    return Ok(());
                }
                Value::Text(s) => {
                    if let Some(sst_idx) = sst.and_then(|m| m.get_arc(s)) {
                        buf.extend_from_slice(b"<c r=\"");
                        push_a1(buf, key.row, key.col);
                        if let Some(xf) = xf {
                            buf.extend_from_slice(b"\" s=\"");
                            push_u16(buf, xf);
                        }
                        buf.extend_from_slice(b"\" t=\"s\"><v>");
                        push_u32(buf, sst_idx);
                        buf.extend_from_slice(b"</v></c>");
                        return Ok(());
                    }
                }
            },
        }
    }

    // Fast path for unprefixed ordinary scalar formula cells (no CSE array / spill shape).
    if prefix.is_empty()
        && let crate::store::CellView::Cell(cell_ref) = cell
        && let Some(f_raw) = cell_ref.formula.as_deref()
        && (arrays.is_empty() || !arrays.contains_key(&key))
        && (sheet.spill_shapes.is_empty() || !sheet.spill_shapes.contains_key(&key))
    {
        buf.extend_from_slice(b"<c r=\"");
        push_a1(buf, key.row, key.col);
        if let Some(xf) = xf {
            buf.extend_from_slice(b"\" s=\"");
            push_u16(buf, xf);
        }
        match &cell_ref.value {
            Value::Empty => {
                buf.extend_from_slice(b"\">");
                push_scalar_formula_tag(buf, key, f_raw, shared);
                buf.extend_from_slice(b"</c>");
            }
            Value::Number(n) => {
                if n.is_finite() {
                    buf.extend_from_slice(b"\">");
                    push_scalar_formula_tag(buf, key, f_raw, shared);
                    buf.extend_from_slice(b"<v>");
                    push_f64_xml(buf, *n);
                    buf.extend_from_slice(b"</v></c>");
                } else {
                    buf.extend_from_slice(b"\" t=\"e\">");
                    push_scalar_formula_tag(buf, key, f_raw, shared);
                    buf.extend_from_slice(b"<v>#NUM!</v></c>");
                }
            }
            Value::Bool(b) => {
                buf.extend_from_slice(b"\" t=\"b\">");
                push_scalar_formula_tag(buf, key, f_raw, shared);
                buf.extend_from_slice(if *b {
                    b"<v>1</v></c>"
                } else {
                    b"<v>0</v></c>"
                });
            }
            Value::Error(e) => {
                buf.extend_from_slice(b"\" t=\"e\">");
                push_scalar_formula_tag(buf, key, f_raw, shared);
                buf.extend_from_slice(b"<v>");
                buf.extend_from_slice(e.text().as_bytes());
                buf.extend_from_slice(b"</v></c>");
            }
            Value::Text(s) => {
                buf.extend_from_slice(b"\" t=\"str\">");
                push_scalar_formula_tag(buf, key, f_raw, shared);
                buf.extend_from_slice(b"<v>");
                buf.extend_from_slice(encode_xstring_xml(s.as_ref()).as_bytes());
                buf.extend_from_slice(b"</v></c>");
            }
        }
        return Ok(());
    }

    let cell_cow = cell.to_cow();
    let cell = cell_cow.as_ref();

    let mut a1_buf = [0u8; 16];
    let reference = format_a1_buf(key.row, key.col, &mut a1_buf);
    if cell.formula.is_none() && matches!(cell.value, Value::Empty) {
        if let Some(xf) = xf {
            write_styled_blank_buf(buf, key, xf, prefix);
        }
        return Ok(());
    }

    let mut start = format!("<{prefix}c r=\"{reference}\"");
    if let Some(xf) = xf {
        use std::fmt::Write as _;
        let _ = write!(start, " s=\"{xf}\"");
    }

    if has_valid_runs {
        let runs = runs.unwrap();
        buf.extend_from_slice(start.as_bytes());
        buf.extend_from_slice(b" t=\"inlineStr\">");
        buf.extend_from_slice(encode_rich_inline(runs, prefix).as_bytes());
        buf.extend_from_slice(b"</");
        buf.extend_from_slice(prefix.as_bytes());
        buf.extend_from_slice(b"c>");
        return Ok(());
    }

    if cell.formula.is_none() {
        match &cell.value {
            Value::Empty => unreachable!(),
            Value::Number(n) => {
                buf.extend_from_slice(start.as_bytes());
                if n.is_finite() {
                    buf.extend_from_slice(b"><");
                    buf.extend_from_slice(prefix.as_bytes());
                    buf.extend_from_slice(b"v>");
                    push_f64_xml(buf, *n);
                    buf.extend_from_slice(b"</");
                    buf.extend_from_slice(prefix.as_bytes());
                    buf.extend_from_slice(b"v></");
                    buf.extend_from_slice(prefix.as_bytes());
                    buf.extend_from_slice(b"c>");
                } else {
                    write!(buf, " t=\"e\"><{prefix}v>#NUM!</{prefix}v></{prefix}c>")?;
                }
            }
            Value::Bool(b) => {
                buf.extend_from_slice(start.as_bytes());
                write!(
                    buf,
                    " t=\"b\"><{prefix}v>{}</{prefix}v></{prefix}c>",
                    u8::from(*b)
                )?;
            }
            Value::Error(e) => {
                buf.extend_from_slice(start.as_bytes());
                write!(
                    buf,
                    " t=\"e\"><{prefix}v>{}</{prefix}v></{prefix}c>",
                    e.text()
                )?;
            }
            Value::Text(s) => {
                let text: &str = s.as_ref();
                if let Some(sst_idx) = sst.and_then(|m| m.get_arc(s).or_else(|| m.get_str(text))) {
                    buf.extend_from_slice(start.as_bytes());
                    write!(buf, " t=\"s\"><{prefix}v>{sst_idx}</{prefix}v></{prefix}c>")?;
                } else {
                    buf.extend_from_slice(start.as_bytes());
                    write!(
                        buf,
                        " t=\"inlineStr\"><{prefix}is><{prefix}t xml:space=\"preserve\">{}</{prefix}t></{prefix}is></{prefix}c>",
                        encode_xstring_xml(text)
                    )?;
                }
            }
        }
        return Ok(());
    }

    // Formula cell (scalar, fixed CSE array, or dynamic spill array).
    let f_raw = cell.formula.as_deref().unwrap();
    let f_xml = encode_formula_xml(f_raw);
    let shape = sheet.spill_shapes.get(&key);
    let f_tag = if let Some(&rect) = arrays.get(&key) {
        let range = super::dynamic::reference(rect);
        format!("<{prefix}f t=\"array\" ref=\"{range}\">{f_xml}</{prefix}f>")
    } else if let Some(&(h, w)) = shape {
        if let Some(cm) = cm_index {
            let rect = (
                key.row,
                key.col,
                key.row
                    .checked_add(h.saturating_sub(1))
                    .context("数组越界")?,
                key.col
                    .checked_add(w.saturating_sub(1))
                    .context("数组越界")?,
            );
            if rect.2 < crate::sync::MAX_ROWS && rect.3 < crate::sync::MAX_COLS {
                use std::fmt::Write as _;
                let _ = write!(start, " cm=\"{cm}\"");
                let range = super::dynamic::reference(rect);
                format!("<{prefix}f t=\"array\" ref=\"{range}\">{f_xml}</{prefix}f>")
            } else {
                format!("<{prefix}f>{f_xml}</{prefix}f>")
            }
        } else {
            format!("<{prefix}f>{f_xml}</{prefix}f>")
        }
    } else {
        format!("<{prefix}f>{f_xml}</{prefix}f>")
    };

    buf.extend_from_slice(start.as_bytes());
    match &cell.value {
        Value::Empty => {
            write!(buf, ">{f_tag}</{prefix}c>")?;
        }
        Value::Number(n) => {
            if n.is_finite() {
                buf.push(b'>');
                buf.extend_from_slice(f_tag.as_bytes());
                buf.push(b'<');
                buf.extend_from_slice(prefix.as_bytes());
                buf.extend_from_slice(b"v>");
                push_f64_xml(buf, *n);
                buf.extend_from_slice(b"</");
                buf.extend_from_slice(prefix.as_bytes());
                buf.extend_from_slice(b"v></");
                buf.extend_from_slice(prefix.as_bytes());
                buf.extend_from_slice(b"c>");
            } else {
                write!(
                    buf,
                    " t=\"e\">{f_tag}<{prefix}v>#NUM!</{prefix}v></{prefix}c>"
                )?;
            }
        }
        Value::Bool(b) => {
            write!(
                buf,
                " t=\"b\">{f_tag}<{prefix}v>{}</{prefix}v></{prefix}c>",
                u8::from(*b)
            )?;
        }
        Value::Error(e) => {
            write!(
                buf,
                " t=\"e\">{f_tag}<{prefix}v>{}</{prefix}v></{prefix}c>",
                e.text()
            )?;
        }
        Value::Text(s) => {
            write!(
                buf,
                " t=\"str\">{f_tag}<{prefix}v>{}</{prefix}v></{prefix}c>",
                encode_xstring_xml(s.as_ref())
            )?;
        }
    }
    Ok(())
}

fn write_styled_blank_buf(buf: &mut Vec<u8>, key: CellRef, xf: u16, prefix: &str) {
    buf.push(b'<');
    buf.extend_from_slice(prefix.as_bytes());
    buf.extend_from_slice(b"c r=\"");
    push_a1(buf, key.row, key.col);
    buf.extend_from_slice(b"\" s=\"");
    push_u16(buf, xf);
    buf.extend_from_slice(b"\"/>");
}

pub(super) fn sheet_styled_bounds(sheet: &Sheet) -> Option<(u32, u32, u32, u32)> {
    let mut bounds = sheet.used_bounds();
    if let Some((max_r, max_c)) = sheet.styles.max_coords() {
        if let Some((first_pos, _)) = sheet.styles.iter().next() {
            let min_r = first_pos.row;
            let mut min_c = first_pos.col;
            if let Some((_, l, _, _)) = bounds {
                min_c = min_c.min(l);
            }
            for (_, page) in sheet.styles.iter_pages() {
                if let Some(c) = page.iter_nonempty_cols(0, min_c).next() {
                    min_c = min_c.min(c);
                }
            }
            bounds = Some(match bounds {
                Some((t, l, b, r)) => (t.min(min_r), l.min(min_c), b.max(max_r), r.max(max_c)),
                None => (min_r, min_c, max_r, max_c),
            });
        }
    }
    bounds
}

fn sheet_row_span_bounds(
    sheet: &Sheet,
    styled_bounds: Option<(u32, u32, u32, u32)>,
) -> Option<(u32, u32)> {
    let mut range: Option<(u32, u32)> = styled_bounds.map(|(t, _, b, _)| (t, b));
    let mut merge = |lo: u32, hi: u32| {
        let lo = lo.min(crate::sync::MAX_ROWS - 1);
        let hi = hi.min(crate::sync::MAX_ROWS - 1);
        if lo <= hi {
            range = Some(match range {
                Some((a, b)) => (a.min(lo), b.max(hi)),
                None => (lo, hi),
            });
        }
    };
    if let (Some(f), Some(l)) = (sheet.rows.spans().first(), sheet.rows.spans().last()) {
        if f.start < crate::sync::MAX_ROWS {
            merge(f.start, l.end);
        }
    }
    if let (Some(f), Some(l)) = (
        sheet.hidden_rows.intervals().first(),
        sheet.hidden_rows.intervals().last(),
    ) {
        if f.0 < crate::sync::MAX_ROWS {
            merge(f.0, l.1);
        }
    }
    if let (Some(f), Some(l)) = (
        sheet.hidden_rows_user.intervals().first(),
        sheet.hidden_rows_user.intervals().last(),
    ) {
        if f.0 < crate::sync::MAX_ROWS {
            merge(f.0, l.1);
        }
    }
    for &r in sheet.row_styles.keys() {
        if r < crate::sync::MAX_ROWS {
            merge(r, r);
        }
    }
    for (&r, &lv) in &sheet.row_levels {
        if lv > 0 && r < crate::sync::MAX_ROWS {
            merge(r, r);
        }
    }
    range
}

fn sheet_estimated_work(sheet: &Sheet) -> usize {
    let row_h_work: usize = sheet
        .rows
        .spans()
        .iter()
        .filter(|s| s.start < crate::sync::MAX_ROWS)
        .map(|s| (s.end.min(crate::sync::MAX_ROWS - 1) - s.start) as usize + 1)
        .sum();
    sheet
        .len()
        .saturating_add(sheet.styles.len())
        .saturating_add(row_h_work)
        .saturating_add(sheet.hidden_rows.len())
        .saturating_add(sheet.hidden_rows_user.len())
}

struct RowMetaCursor<'a> {
    ht_spans: &'a [crate::layout::Span],
    ht_idx: usize,
    ht_cur: u32,
    hid1: &'a [(u32, u32)],
    hid1_idx: usize,
    hid1_cur: u32,
    hid2: &'a [(u32, u32)],
    hid2_idx: usize,
    hid2_cur: u32,
    row_xfs: &'a [(u32, u16)],
    xf_idx: usize,
    row_levels: &'a [(u32, u8)],
    lv_idx: usize,
    r2: u32,
}

impl<'a> RowMetaCursor<'a> {
    fn new(
        sheet: &'a Sheet,
        row_xfs: &'a [(u32, u16)],
        row_levels: &'a [(u32, u8)],
        r1: u32,
        r2: u32,
    ) -> Self {
        let ht_spans = sheet.rows.spans();
        let ht_idx = ht_spans.partition_point(|s| s.end < r1);
        let hid1 = sheet.hidden_rows.intervals();
        let hid1_idx = hid1.partition_point(|&(_, hi)| hi < r1);
        let hid2 = sheet.hidden_rows_user.intervals();
        let hid2_idx = hid2.partition_point(|&(_, hi)| hi < r1);
        let xf_idx = row_xfs.partition_point(|&(r, _)| r < r1);
        let lv_idx = row_levels.partition_point(|&(r, _)| r < r1);
        Self {
            ht_spans,
            ht_idx,
            ht_cur: r1,
            hid1,
            hid1_idx,
            hid1_cur: r1,
            hid2,
            hid2_idx,
            hid2_cur: r1,
            row_xfs,
            xf_idx,
            row_levels,
            lv_idx,
            r2,
        }
    }

    #[inline]
    fn peek_row(&self) -> u32 {
        let mut best = u32::MAX;
        if let Some(s) = self.ht_spans.get(self.ht_idx) {
            let cand = s.start.max(self.ht_cur);
            if cand <= s.end && cand <= self.r2 {
                best = best.min(cand);
            }
        }
        if let Some(&(lo, hi)) = self.hid1.get(self.hid1_idx) {
            let cand = lo.max(self.hid1_cur);
            if cand <= hi && cand <= self.r2 {
                best = best.min(cand);
            }
        }
        if let Some(&(lo, hi)) = self.hid2.get(self.hid2_idx) {
            let cand = lo.max(self.hid2_cur);
            if cand <= hi && cand <= self.r2 {
                best = best.min(cand);
            }
        }
        if let Some(&(r, _)) = self.row_xfs.get(self.xf_idx)
            && r <= self.r2
        {
            best = best.min(r);
        }
        if let Some(&(r, _)) = self.row_levels.get(self.lv_idx)
            && r <= self.r2
        {
            best = best.min(r);
        }
        best
    }

    #[inline]
    fn take_at(&mut self, r: u32) -> (Option<f32>, bool, Option<u16>, Option<u8>) {
        let mut ht = None;
        if let Some(s) = self.ht_spans.get(self.ht_idx) {
            let cand = s.start.max(self.ht_cur);
            if cand == r && r <= s.end {
                ht = Some(s.size);
                self.ht_cur = r.saturating_add(1);
                if self.ht_cur > s.end {
                    self.ht_idx += 1;
                }
            }
        }
        let mut hidden = false;
        if let Some(&(lo, hi)) = self.hid1.get(self.hid1_idx) {
            let cand = lo.max(self.hid1_cur);
            if cand == r && r <= hi {
                hidden = true;
                self.hid1_cur = r.saturating_add(1);
                if self.hid1_cur > hi {
                    self.hid1_idx += 1;
                }
            }
        }
        if let Some(&(lo, hi)) = self.hid2.get(self.hid2_idx) {
            let cand = lo.max(self.hid2_cur);
            if cand == r && r <= hi {
                hidden = true;
                self.hid2_cur = r.saturating_add(1);
                if self.hid2_cur > hi {
                    self.hid2_idx += 1;
                }
            }
        }
        let row_xf = if let Some(&(xr, xf)) = self.row_xfs.get(self.xf_idx)
            && xr == r
        {
            self.xf_idx += 1;
            Some(xf)
        } else {
            None
        };
        let row_lv = if let Some(&(lr, lv)) = self.row_levels.get(self.lv_idx)
            && lr == r
        {
            self.lv_idx += 1;
            Some(lv)
        } else {
            None
        };
        (ht, hidden, row_xf, row_lv)
    }
}

#[inline]
fn write_row_open_tag(
    buf: &mut Vec<u8>,
    r: u32,
    ht: Option<f32>,
    hidden: bool,
    row_xf: Option<u16>,
    row_lv: Option<u8>,
    self_closing: bool,
) {
    buf.extend_from_slice(b"<row r=\"");
    push_u32(buf, r + 1);
    buf.push(b'"');
    if let Some(xf) = row_xf {
        buf.extend_from_slice(b" s=\"");
        push_u16(buf, xf);
        buf.extend_from_slice(b"\" customFormat=\"1\"");
    }
    if let Some(px) = ht {
        buf.extend_from_slice(b" ht=\"");
        push_pt_2dec(buf, px);
        buf.extend_from_slice(b"\" customHeight=\"1\"");
    }
    if hidden {
        buf.extend_from_slice(b" hidden=\"1\"");
    }
    if let Some(lv) = row_lv.filter(|&l| l > 0) {
        buf.extend_from_slice(b" outlineLevel=\"");
        push_u32(buf, u32::from(lv));
        buf.push(b'"');
    }
    if self_closing {
        buf.extend_from_slice(b"/>");
    } else {
        buf.push(b'>');
    }
}

#[allow(clippy::too_many_arguments)]
fn write_band_rows_streaming(
    buf: &mut Vec<u8>,
    mut flush_to: Option<&mut dyn Write>,
    sheet: &Sheet,
    arrays: &HashMap<CellRef, (u32, u32, u32, u32)>,
    sst: Option<SheetSstLookup<'_>>,
    direct_xf: &[Option<u16>],
    row_xfs: &[(u32, u16)],
    row_levels: &[(u32, u8)],
    r1: u32,
    r2: u32,
    shared: Option<&SharedFormulaPlan>,
) -> Result<()> {
    let lookup_xf = |id: u16| -> Result<u16> {
        direct_xf
            .get(usize::from(id))
            .copied()
            .flatten()
            .context("直接样式表缺少单元格样式")
    };
    let has_row_or_col_styles = !sheet.row_styles.is_empty() || !sheet.col_styles.is_empty();
    let xf_for = |key: CellRef, explicit: Option<u16>| -> Result<Option<u16>> {
        let inherited = if !has_row_or_col_styles {
            None
        } else {
            sheet
                .row_styles
                .get(&key.row)
                .or_else(|| sheet.col_styles.get(&key.col))
                .copied()
        };
        let sid = explicit.or(inherited);
        match sid {
            None => Ok(None),
            Some(0) if inherited.is_none() => Ok(None),
            Some(id) => Ok(Some(lookup_xf(id)?)),
        }
    };

    let plain_grid = sheet.styles.is_empty()
        && row_xfs.is_empty()
        && row_levels.is_empty()
        && sheet.rows.spans().is_empty()
        && sheet.hidden_rows.is_empty()
        && sheet.hidden_rows_user.is_empty();
    if plain_grid {
        let has_col_styles = !sheet.col_styles.is_empty();
        let mut open_row: Option<u32> = None;
        for (key, cell) in sheet.cells().iter_rect_views((r1, 0, r2, u32::MAX)) {
            if open_row != Some(key.row) {
                if open_row.is_some() {
                    buf.extend_from_slice(b"</row>");
                    if let Some(w) = flush_to.as_deref_mut()
                        && buf.len() >= 128 * 1024
                    {
                        w.write_all(buf)?;
                        buf.clear();
                    }
                }
                buf.extend_from_slice(b"<row r=\"");
                push_u32(buf, key.row + 1);
                buf.extend_from_slice(b"\">");
                open_row = Some(key.row);
            }
            let xf = if has_col_styles {
                sheet
                    .col_styles
                    .get(&key.col)
                    .map(|&id| lookup_xf(id))
                    .transpose()?
            } else {
                None
            };
            write_value_buf(buf, sheet, arrays, sst, key, cell, xf, "", Some(1), shared)?;
        }
        if open_row.is_some() {
            buf.extend_from_slice(b"</row>");
        }
        return Ok(());
    }

    let mut meta = RowMetaCursor::new(sheet, row_xfs, row_levels, r1, r2);
    let has_cell_styles = !sheet.styles.is_empty();
    let p1 = r1 / crate::store::PAGE_ROWS;
    let p2 = r2 / crate::store::PAGE_ROWS;
    let mut page_styles: Vec<(CellRef, u16)> = Vec::new();
    let mut row_entries_buf: Vec<(CellRef, Option<crate::store::CellView<'_>>, Option<u16>)> =
        Vec::new();

    for p in p1..=p2 {
        let ps = (p * crate::store::PAGE_ROWS).max(r1);
        let pe = (p * crate::store::PAGE_ROWS + crate::store::PAGE_ROWS - 1).min(r2);
        if ps > pe {
            continue;
        }
        page_styles.clear();
        if has_cell_styles {
            page_styles.extend(sheet.styles.iter_rect((ps, 0, pe, u32::MAX)));
            page_styles.sort_unstable_by_key(|&(k, _)| (k.row, k.col));
        }
        let mut cell_it = sheet.cells().iter_rect_views((ps, 0, pe, u32::MAX)).peekable();
        let mut style_idx = 0usize;

        loop {
            let next_c = cell_it.peek().map_or(u32::MAX, |(k, _)| k.row);
            let next_s = page_styles.get(style_idx).map_or(u32::MAX, |&(k, _)| k.row);
            let next_m = meta.peek_row();
            let r = next_c.min(next_s).min(next_m);
            if r > pe {
                break;
            }
            let (ht, hidden, row_xf, row_lv) = meta.take_at(r);
            row_entries_buf.clear();
            while cell_it.peek().is_some_and(|(k, _)| k.row == r)
                || page_styles.get(style_idx).is_some_and(|&(k, _)| k.row == r)
            {
                let c_col = cell_it
                    .peek()
                    .filter(|(k, _)| k.row == r)
                    .map_or(u32::MAX, |(k, _)| k.col);
                let s_col = page_styles
                    .get(style_idx)
                    .filter(|&&(k, _)| k.row == r)
                    .map_or(u32::MAX, |&(k, _)| k.col);
                let col = c_col.min(s_col);
                let cell_opt = if c_col == col {
                    Some(cell_it.next().unwrap().1)
                } else {
                    None
                };
                let explicit = if s_col == col {
                    let sid = page_styles[style_idx].1;
                    style_idx += 1;
                    Some(sid)
                } else {
                    None
                };
                if cell_opt.is_some()
                    || explicit.is_some_and(|id| {
                        id != 0
                            || sheet.row_styles.contains_key(&r)
                            || sheet.col_styles.contains_key(&col)
                    })
                {
                    row_entries_buf.push((CellRef::new(r, col), cell_opt, explicit));
                }
            }

            if row_entries_buf.is_empty() {
                if ht.is_some() || hidden || row_xf.is_some() || row_lv.is_some_and(|l| l > 0) {
                    write_row_open_tag(buf, r, ht, hidden, row_xf, row_lv, true);
                }
            } else {
                write_row_open_tag(buf, r, ht, hidden, row_xf, row_lv, false);
                for (key, cell_opt, explicit) in row_entries_buf.drain(..) {
                    if let Some(cell) = cell_opt {
                        let xf = xf_for(key, explicit)?;
                        write_value_buf(buf, sheet, arrays, sst, key, cell, xf, "", Some(1), shared)?;
                    } else if let Some(id) = explicit {
                        let xf = lookup_xf(id)?;
                        write_styled_blank_buf(buf, key, xf, "");
                    }
                }
                buf.extend_from_slice(b"</row>");
            }

            if let Some(w) = flush_to.as_deref_mut()
                && buf.len() >= 128 * 1024
            {
                w.write_all(buf)?;
                buf.clear();
            }
        }
    }
    Ok(())
}

fn prepare_row_meta_vecs(
    sheet: &Sheet,
    direct_xf: &[Option<u16>],
) -> Result<(Vec<(u32, u16)>, Vec<(u32, u8)>)> {
    let mut row_xfs: Vec<(u32, u16)> = Vec::with_capacity(sheet.row_styles.len());
    for (&r, &sid) in &sheet.row_styles {
        if r < crate::sync::MAX_ROWS {
            let xf = direct_xf
                .get(usize::from(sid))
                .copied()
                .flatten()
                .context("直接样式表缺少行样式")?;
            row_xfs.push((r, xf));
        }
    }
    row_xfs.sort_unstable_by_key(|&(r, _)| r);

    let mut row_levels: Vec<(u32, u8)> = sheet
        .row_levels
        .iter()
        .filter_map(|(&r, &lv)| (lv > 0 && r < crate::sync::MAX_ROWS).then_some((r, lv)))
        .collect();
    row_levels.sort_unstable_by_key(|&(r, _)| r);
    Ok((row_xfs, row_levels))
}

fn write_sheet(
    mut out: impl Write,
    shell: &str,
    sheet: &Sheet,
    sst: Option<SheetSstLookup<'_>>,
    direct_xf: Option<&[Option<u16>]>,
) -> Result<()> {
    // Only the sparse, freshly generated template is parsed. Values are merged
    // from the ordered model iterator without materializing a second cell store.
    let doc = roxmltree::Document::parse(shell)?;
    let root = doc.root_element();
    let namespace = root
        .tag_name()
        .namespace()
        .context("新建工作表缺少命名空间")?;
    let dimension = root
        .children()
        .find(|n| n.has_tag_name((namespace, "dimension")))
        .and_then(|n| n.attribute_node("ref"));
    let data = root
        .children()
        .find(|n| n.has_tag_name((namespace, "sheetData")))
        .context("新建工作表缺少 sheetData")?;
    let mut bounds = sheet.used_bounds();
    let arrays = valid_arrays_map(sheet);

    let mut formats = std::collections::HashMap::new();
    if direct_xf.is_none() {
        formats.insert(0u16, 0u16);
        for stub in data.descendants().filter(|n| n.has_tag_name((namespace, "c"))) {
            let (r, c) = stub
                .attribute("r")
                .and_then(crate::sparse::parse_ref)
                .context("无效的模板单元格")?;
            bounds = Some(match bounds {
                Some((t, l, b, rr)) => (t.min(r), l.min(c), b.max(r), rr.max(c)),
                None => (r, c, r, c),
            });
            if sheet.contains(r, c)
                && let Some(id) = sheet.explicit_style_of(r, c)
            {
                let xf = stub
                    .attribute("s")
                    .map(str::parse::<u16>)
                    .transpose()?
                    .unwrap_or(0);
                formats.insert(id, xf);
            }
        }
        for row in data
            .children()
            .filter(|n| n.has_tag_name((namespace, "row")))
        {
            if let (Some(r), Some(raw)) = (row.attribute("r"), row.attribute("s"))
                && let Some(id) = r
                    .parse::<u32>()?
                    .checked_sub(1)
                    .and_then(|i| sheet.row_styles.get(&i))
            {
                formats.insert(*id, raw.parse()?);
            }
        }
        for col in root
            .children()
            .filter(|n| n.has_tag_name((namespace, "cols")))
            .flat_map(|cols| cols.children().filter(|n| n.has_tag_name((namespace, "col"))))
        {
            if let (Some(first), Some(last), Some(raw)) = (
                col.attribute("min"),
                col.attribute("max"),
                col.attribute("style"),
            ) {
                let xf = raw.parse()?;
                for c in first.parse::<u32>()?..=last.parse::<u32>()?.min(crate::sync::MAX_COLS) {
                    if let Some(id) = c.checked_sub(1).and_then(|c| sheet.col_styles.get(&c)) {
                        formats.insert(*id, xf);
                    }
                }
            }
        }
    } else if !sheet.styles.is_empty() {
        bounds = sheet_styled_bounds(sheet);
    }

    let lookup_xf = |id: u16| -> Result<u16> {
        if let Some(table) = direct_xf {
            table
                .get(usize::from(id))
                .copied()
                .flatten()
                .context("直接样式表缺少单元格样式")
        } else {
            formats.get(&id).copied().context("模板缺少单元格样式")
        }
    };

    let mut header = if let Some(dimension) = dimension {
        let reference = match bounds {
            Some((top, left, bottom, right)) => {
                let first = crate::model::cell_name(top, left);
                let end = crate::model::cell_name(bottom, right);
                if first == end {
                    first
                } else {
                    format!("{first}:{end}")
                }
            }
            None => "A1".to_owned(),
        };
        let dim = dimension.range_value();
        let mut h = String::with_capacity(data.range().start + reference.len() + 32);
        h.push_str(&shell[..dim.start]);
        h.push_str(&reference);
        h.push_str(&shell[dim.end..data.range().start]);
        h
    } else {
        shell[..data.range().start].to_owned()
    };

    if direct_xf.is_some() && !sheet.col_styles.is_empty() {
        let mut col_updates: BTreeMap<u32, Option<u16>> = BTreeMap::new();
        for (&col, &sid) in sheet.col_styles.iter() {
            col_updates.insert(col, Some(lookup_xf(sid)?));
        }
        header = crate::patch::layout_xml::styled_columns(&header, &col_updates)?;
    }

    const FLUSH_AT: usize = 128 * 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(FLUSH_AT + 16 * 1024);

    let shared = SharedFormulaPlan::build(sheet, &arrays);

    // Direct empty-<sheetData/> fast path: stream all cells + styles + row metadata directly!
    if let Some(dxf) = direct_xf
        && !data.children().any(|n| n.is_element())
    {
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(b"<sheetData>");
        if let Some((top_r, bot_r)) = sheet_row_span_bounds(sheet, bounds) {
            let (row_xfs, row_levels) = prepare_row_meta_vecs(sheet, dxf)?;
            write_band_rows_streaming(
                &mut buf,
                Some(&mut out),
                sheet,
                &arrays,
                sst,
                dxf,
                &row_xfs,
                &row_levels,
                top_r,
                bot_r,
                shared.as_ref(),
            )?;
        }
        buf.extend_from_slice(b"</sheetData>");
        buf.extend_from_slice(shell[data.range().end..].as_bytes());
        out.write_all(&buf)?;
        out.flush()?;
        return Ok(());
    }

    let xf_for = |key: CellRef, explicit: Option<u16>| -> Result<Option<u16>> {
        let inherited = if sheet.row_styles.is_empty() && sheet.col_styles.is_empty() {
            None
        } else {
            sheet
                .row_styles
                .get(&key.row)
                .or_else(|| sheet.col_styles.get(&key.col))
                .copied()
        };
        let sid = explicit.or(inherited);
        match sid {
            None => Ok(None),
            Some(0) if inherited.is_none() && direct_xf.is_some() => Ok(None),
            Some(id) => Ok(Some(lookup_xf(id)?)),
        }
    };

    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(b"<sheetData>");

    let mut direct_row_styles: Vec<(u32, u16)> =
        if direct_xf.is_some() && !sheet.row_styles.is_empty() {
            let mut v: Vec<(u32, u16)> = sheet
                .row_styles
                .iter()
                .map(|(&r, &sid)| (r, sid))
                .collect();
            v.sort_unstable_by_key(|&(r, _)| r);
            v
        } else {
            Vec::new()
        };
    let mut row_style_iter = direct_row_styles.drain(..).peekable();
    let mut rows = data.children().filter(|n| n.is_element()).peekable();

    let use_merged_styles = direct_xf.is_some() && !sheet.styles.is_empty();
    let mut merged_iter = if use_merged_styles {
        Some(sheet.iter_with_styles().peekable())
    } else {
        None
    };
    let mut plain_iter = if !use_merged_styles {
        Some(sheet.iter_cells_with_explicit_style().peekable())
    } else {
        None
    };

    let peek_entry = |merged: &mut Option<std::iter::Peekable<_>>,
                      plain: &mut Option<std::iter::Peekable<_>>|
     -> Option<CellRef> {
        if let Some(it) = merged {
            it.peek().map(|(k, _, _)| *k)
        } else if let Some(it) = plain {
            it.peek().map(|(k, _, _)| *k)
        } else {
            None
        }
    };

    loop {
        let model_row = peek_entry(&mut merged_iter, &mut plain_iter).map_or(u32::MAX, |k| k.row);
        let template_row = match rows.peek() {
            Some(n) if n.has_tag_name((namespace, "row")) => n
                .attribute("r")
                .and_then(|r| r.parse::<u32>().ok()?.checked_sub(1))
                .context("无效的模板行号")?,
            Some(_) => anyhow::bail!("工作表包含未知的行记录"),
            None => u32::MAX,
        };
        let style_row = row_style_iter.peek().map_or(u32::MAX, |&(r, _)| r);
        let r = model_row.min(template_row).min(style_row);
        if r == u32::MAX {
            break;
        }
        let row = (r == template_row).then(|| rows.next().unwrap());
        let row_xf = if r == style_row {
            let (_, sid) = row_style_iter.next().unwrap();
            Some(lookup_xf(sid)?)
        } else {
            None
        };

        if let Some(n) = row {
            let raw = &shell[n.range()];
            let mut reader = quick_xml::Reader::from_str(raw);
            reader.read_event()?;
            let tag = &raw[..reader.buffer_position() as usize];
            let start = tag
                .strip_suffix("/>")
                .or_else(|| tag.strip_suffix('>'))
                .context("无效的行标签")?;
            let mut open = String::with_capacity(start.len() + 32);
            if let Some(attr) = n.attribute_node("spans") {
                let span = attr.range();
                let base = n.range().start;
                open.push_str(&start[..span.start - base]);
                open.push_str(&start[span.end - base..]);
            } else {
                open.push_str(start);
            }
            open.push('>');
            if let Some(xf) = row_xf {
                open = crate::patch::layout_xml::styled_row(&open, Some(xf))?;
            }
            buf.extend_from_slice(open.as_bytes());
        } else if let Some(xf) = row_xf {
            buf.extend_from_slice(b"<row r=\"");
            push_u32(&mut buf, r + 1);
            buf.extend_from_slice(b"\" s=\"");
            push_u16(&mut buf, xf);
            buf.extend_from_slice(b"\" customFormat=\"1\">");
        } else {
            buf.extend_from_slice(b"<row r=\"");
            push_u32(&mut buf, r + 1);
            buf.extend_from_slice(b"\">");
        }

        let emit_entry = |buf: &mut Vec<u8>,
                          key: CellRef,
                          cell: Option<crate::store::CellView<'_>>,
                          explicit: Option<u16>,
                          override_xf: Option<u16>|
         -> Result<()> {
            if let Some(cell) = cell {
                let xf = match override_xf {
                    Some(xf) => Some(xf),
                    None => xf_for(key, explicit)?,
                };
                write_value_buf(buf, sheet, &arrays, sst, key, cell, xf, "", Some(1), shared.as_ref())?;
            } else if let Some(id) = explicit {
                if id != 0
                    || sheet.row_styles.contains_key(&key.row)
                    || sheet.col_styles.contains_key(&key.col)
                {
                    let xf = match override_xf {
                        Some(xf) => xf,
                        None => lookup_xf(id)?,
                    };
                    write_styled_blank_buf(buf, key, xf, "");
                }
            }
            Ok(())
        };

        for stub in row
            .into_iter()
            .flat_map(|n| n.children().filter(|n| n.is_element()))
        {
            ensure!(
                stub.has_tag_name((namespace, "c")),
                "新建模板包含未知的行内容"
            );
            let (sr, sc) = stub
                .attribute("r")
                .and_then(crate::sparse::parse_ref)
                .context("无效的模板单元格")?;
            ensure!(sr == r, "模板单元格行号不一致");
            while let Some((key, cell, explicit)) = if let Some(it) = &mut merged_iter {
                it.next_if(|(k, _, _)| k.row == r && k.col < sc)
            } else {
                plain_iter
                    .as_mut()
                    .unwrap()
                    .next_if(|(k, _, _)| k.row == r && k.col < sc)
                    .map(|(k, c, s)| (k, Some(c), s))
            } {
                emit_entry(&mut buf, key, cell, explicit, None)?;
            }
            if let Some((key, cell, explicit)) = if let Some(it) = &mut merged_iter {
                it.next_if(|(k, _, _)| k.row == r && k.col == sc)
            } else {
                plain_iter
                    .as_mut()
                    .unwrap()
                    .next_if(|(k, _, _)| k.row == r && k.col == sc)
                    .map(|(k, c, s)| (k, Some(c), s))
            } {
                let xf = stub.attribute("s").map(str::parse::<u16>).transpose()?;
                emit_entry(&mut buf, key, cell, explicit, xf)?;
            } else if stub.attribute("s").is_none() && sheet.explicit_style_of(sr, sc) == Some(0) {
                write_styled_blank_buf(&mut buf, CellRef::new(sr, sc), 0, "");
            } else {
                buf.extend_from_slice(shell[stub.range()].as_bytes());
            }
        }
        if let Some(it) = &mut merged_iter {
            while let Some((key, cell, explicit)) = it.next_if(|(k, _, _)| k.row == r) {
                emit_entry(&mut buf, key, cell, explicit, None)?;
            }
        } else if let Some(it) = &mut plain_iter {
            while let Some((key, cell, explicit)) = it.next_if(|(k, _, _)| k.row == r) {
                write_value_buf(
                    &mut buf,
                    sheet,
                    &arrays,
                    sst,
                    key,
                    cell,
                    xf_for(key, explicit)?,
                    "",
                    Some(1),
                    shared.as_ref(),
                )?;
            }
        }
        buf.extend_from_slice(b"</row>");
        if buf.len() >= FLUSH_AT {
            out.write_all(&buf)?;
            buf.clear();
        }
    }
    buf.extend_from_slice(b"</sheetData>");
    buf.extend_from_slice(shell[data.range().end..].as_bytes());
    out.write_all(&buf)?;
    out.flush()?;
    Ok(())
}

/// Build a single-entry ZIP in memory for a large worksheet (including styled, row-height,
/// hidden-row, and outline-level sheets!) by parallelizing BOTH XML serialization and raw
/// RFC 1951 deflate across CPU cores. Returns `Ok(None)` if the sheet is small or has template rows.
pub(super) fn write_sheet_parallel_zip(
    part: &str,
    shell: &str,
    sheet: &Sheet,
    sst: Option<SheetSstLookup<'_>>,
    direct_xf: &[Option<u16>],
) -> Result<Option<(Vec<u8>, Option<(u32, u64)>)>> {
    let est_work = sheet_estimated_work(sheet);
    if est_work < 32_768 {
        return Ok(None);
    }
    let doc = roxmltree::Document::parse(shell)?;
    let root = doc.root_element();
    let namespace = root
        .tag_name()
        .namespace()
        .context("新建工作表缺少命名空间")?;
    let dimension = root
        .children()
        .find(|n| n.has_tag_name((namespace, "dimension")))
        .and_then(|n| n.attribute_node("ref"));
    let data = root
        .children()
        .find(|n| n.has_tag_name((namespace, "sheetData")))
        .context("新建工作表缺少 sheetData")?;
    if data.children().any(|n| n.is_element()) {
        return Ok(None);
    }
    let styled_bounds = sheet_styled_bounds(sheet);
    let Some((top, bottom)) = sheet_row_span_bounds(sheet, styled_bounds) else {
        return Ok(None);
    };
    let arrays = valid_arrays_map(sheet);

    let lookup_xf = |id: u16| -> Result<u16> {
        direct_xf
            .get(usize::from(id))
            .copied()
            .flatten()
            .context("直接样式表缺少单元格样式")
    };

    let mut header = if let Some(dimension) = dimension {
        let reference = match styled_bounds {
            Some((dt, dl, db, dr)) => {
                let first = crate::model::cell_name(dt, dl);
                let end = crate::model::cell_name(db, dr);
                if first == end {
                    first
                } else {
                    format!("{first}:{end}")
                }
            }
            None => "A1".to_owned(),
        };
        let dim = dimension.range_value();
        let mut h = String::with_capacity(data.range().start + reference.len() + 32);
        h.push_str(&shell[..dim.start]);
        h.push_str(&reference);
        h.push_str(&shell[dim.end..data.range().start]);
        h
    } else {
        shell[..data.range().start].to_owned()
    };
    if !sheet.col_styles.is_empty() {
        let mut col_updates: BTreeMap<u32, Option<u16>> = BTreeMap::new();
        for (&col, &sid) in sheet.col_styles.iter() {
            col_updates.insert(col, Some(lookup_xf(sid)?));
        }
        header = crate::patch::layout_xml::styled_columns(&header, &col_updates)?;
    }
    let footer = &shell[data.range().end..];

    let mut shell_check = XmlCheckState::new_document();
    shell_check.feed(header.as_bytes());
    shell_check.feed(b"<sheetData/>");
    shell_check.feed(footer.as_bytes());
    let shell_ok = shell_check.finish_document(Some("worksheet"));

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(2, 8);
    let first_page_row = (top / crate::store::PAGE_ROWS) * crate::store::PAGE_ROWS;
    let row_span = bottom.saturating_sub(first_page_row).saturating_add(1);
    let raw_per = row_span.div_ceil(workers as u32);
    let band_rows = raw_per.div_ceil(256).max(1) * 256;
    let mut bands: Vec<(u32, u32)> = Vec::with_capacity(workers);
    let mut cur = first_page_row;
    while cur <= bottom {
        let lo = cur.max(top);
        let end_r = cur.saturating_add(band_rows - 1).min(bottom);
        bands.push((lo, end_r));
        if end_r == bottom {
            break;
        }
        cur = end_r + 1;
    }
    if bands.len() < 2 {
        return Ok(None);
    }

    let (row_xfs, row_levels) = prepare_row_meta_vecs(sheet, direct_xf)?;
    let row_xfs_ref = row_xfs.as_slice();
    let row_levels_ref = row_levels.as_slice();
    let num_bands = bands.len();
    let header_ref = header.as_str();
    let arrays_ref = &arrays;
    let shared_plan = SharedFormulaPlan::build(sheet, arrays_ref);
    let shared_ref = shared_plan.as_ref();
    let results: std::sync::Mutex<Vec<Result<(usize, Vec<u8>, u32, u64, bool)>>> =
        std::sync::Mutex::new(Vec::with_capacity(num_bands));

    std::thread::scope(|scope| {
        for (idx, &(r1, r2)) in bands.iter().enumerate() {
            let res_ref = &results;
            scope.spawn(move || {
                let one = (|| -> Result<(usize, Vec<u8>, u32, u64, bool)> {
                    let is_first = idx == 0;
                    let is_last = idx + 1 == num_bands;
                    let est_items = (est_work / num_bands).max(1024);
                    let mut buf = Vec::with_capacity(est_items * 36 + 4096);
                    if is_first {
                        buf.extend_from_slice(header_ref.as_bytes());
                        buf.extend_from_slice(b"<sheetData>");
                    }
                    let body_start = buf.len();
                    write_band_rows_streaming(
                        &mut buf,
                        None,
                        sheet,
                        arrays_ref,
                        sst,
                        direct_xf,
                        row_xfs_ref,
                        row_levels_ref,
                        r1,
                        r2,
                        shared_ref,
                    )?;
                    let body_end = buf.len();
                    if is_last {
                        buf.extend_from_slice(b"</sheetData>");
                        buf.extend_from_slice(footer.as_bytes());
                    }
                    let mut frag_check = XmlCheckState::new_fragment();
                    frag_check.feed(&buf[body_start..body_end]);
                    let frag_ok = frag_check.finish_fragment();
                    let raw_len = buf.len() as u64;
                    let (comp, crc) = deflate_raw_chunk(&buf, 2, is_last)?;
                    Ok((idx, comp, crc, raw_len, frag_ok))
                })();
                res_ref.lock().unwrap().push(one);
            });
        }
    });

    let mut ordered = Vec::with_capacity(num_bands);
    for item in results.into_inner().unwrap() {
        ordered.push(item?);
    }
    ordered.sort_unstable_by_key(|(idx, _, _, _, _)| *idx);
    let mut total_crc = 0u32;
    let mut total_len = 0u64;
    let mut all_ok = shell_ok;
    let mut comp_slices = Vec::with_capacity(num_bands);
    for (_, comp, crc, raw_len, frag_ok) in ordered {
        all_ok &= frag_ok;
        total_crc = if total_len == 0 {
            crc
        } else {
            crc32_combine(total_crc, crc, raw_len)
        };
        total_len += raw_len;
        comp_slices.push(comp);
    }
    let mini = build_single_entry_zip(part, total_crc, total_len, &comp_slices)?;
    Ok(Some((mini, all_ok.then_some((total_crc, total_len)))))
}

/// Write a brand-new plain (unstyled, feature-free) worksheet directly for `xlsx_delta`.
pub(super) fn write_plain_new_sheet(sheet: &Sheet, out: &mut dyn Write) -> Result<Option<(u32, u64)>> {
    let mut vw = VerifiedXmlWriter::new(out);
    let arrays = HashMap::new();
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    buf.extend_from_slice(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    );
    let shared_plan = SharedFormulaPlan::build(sheet, &arrays);
    if let Some((top, left, bottom, right)) = sheet.used_bounds() {
        let first = crate::model::cell_name(top, left);
        let end = crate::model::cell_name(bottom, right);
        buf.extend_from_slice(b"<dimension ref=\"");
        buf.extend_from_slice(first.as_bytes());
        if first != end {
            buf.push(b':');
            buf.extend_from_slice(end.as_bytes());
        }
        buf.extend_from_slice(b"\"/><sheetData>");
        let mut open_row: Option<u32> = None;
        for (key, cell) in sheet.cells().iter_rect_views((top, 0, bottom, u32::MAX)) {
            if open_row != Some(key.row) {
                if open_row.is_some() {
                    buf.extend_from_slice(b"</row>");
                }
                buf.extend_from_slice(b"<row r=\"");
                push_u32(&mut buf, key.row + 1);
                buf.extend_from_slice(b"\">");
                open_row = Some(key.row);
            }
            write_value_buf(
                &mut buf,
                sheet,
                &arrays,
                None,
                key,
                cell,
                None,
                "",
                None,
                shared_plan.as_ref(),
            )?;
            if buf.len() >= 128 * 1024 {
                vw.write_all(&buf)?;
                buf.clear();
            }
        }
        if open_row.is_some() {
            buf.extend_from_slice(b"</row>");
        }
        buf.extend_from_slice(b"</sheetData></worksheet>");
    } else {
        buf.extend_from_slice(b"<sheetData/></worksheet>");
    }
    vw.write_all(&buf)?;
    vw.finish(Some("worksheet"))
}

pub(super) fn has_dynamic(sheet: &Sheet) -> bool {
    let fixed: std::collections::HashSet<_> = sheet.arrays.iter().map(|(k, _)| k).collect();
    sheet.spill_shapes.keys().any(|k| {
        !fixed.contains(k) && sheet.get(k.row, k.col).is_some_and(|c| c.formula.is_some())
    })
}

/// Single-pass direct native streaming writer for fresh workbooks with no multi-part post-save patches.
pub(super) fn write_direct(path: &Path, wb: &Workbook) -> Result<FreshResult> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let sheet_names: Vec<&str> = wb.sheets.iter().map(|s| s.name.as_str()).collect();
    let part_names: Vec<String> = (0..wb.sheets.len())
        .map(|i| format!("xl/worksheets/sheet{}.xml", i + 1))
        .collect();
    let ordered_sheets: Vec<(&str, &Sheet)> = part_names
        .iter()
        .zip(wb.sheets.iter())
        .map(|(p, s)| (p.as_str(), s))
        .collect();

    let dynamic = wb.sheets.iter().any(has_dynamic);
    let full_calc = wb.needs_full_calc_on_load();
    let fresh_styles = FreshStyles::build(None, wb, &ordered_sheets)?;
    let direct_xf = fresh_styles.sid_to_xf.as_slice();
    let fresh_sst = FreshSst::build(None, &ordered_sheets)?;
    let (ct, root_rels, wb_xml, wb_rels) = build_fresh_package_xmls(
        wb,
        &sheet_names,
        fresh_sst.is_some(),
        dynamic,
        full_calc,
        true,
    )?;

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let mut trusted_parts: HashMap<String, (u32, u64)> = HashMap::new();
    trusted_parts.insert(STYLES_PART.to_owned(), fresh_styles.sig);
    if let Some(sst) = &fresh_sst {
        let mut crc = flate2::Crc::new();
        crc.update(&sst.xml);
        trusted_parts.insert(SST_PART.to_owned(), (crc.sum(), sst.xml.len() as u64));
    }

    // Pre-build shells; sheets >= 32,768 use intra-sheet parallel XML+deflate (`write_sheet_parallel_zip`),
    // while multiple smaller sheets run in parallel across sheets!
    let mut shells: Vec<String> = Vec::with_capacity(ordered_sheets.len());
    for (idx, &(_, sheet)) in ordered_sheets.iter().enumerate() {
        shells.push(direct_sheet_shell_xml(sheet, idx == 0)?);
    }

    let mut parallel_minis: HashMap<String, Vec<u8>> = HashMap::new();
    let mut parallel_parts: HashMap<String, tempfile::TempPath> = HashMap::new();
    let mut small_jobs: Vec<(String, &str, &Sheet)> = Vec::new();
    for ((part, sheet), shell) in ordered_sheets.iter().copied().zip(shells.iter()) {
        if sheet_estimated_work(sheet) >= 32_768 {
            let sheet_sst = fresh_sst.as_ref().map(|s| s.map_for(sheet));
            if let Some((mini, sig)) =
                write_sheet_parallel_zip(part, shell, sheet, sheet_sst, direct_xf)?
            {
                if let Some(s) = sig {
                    trusted_parts.insert(part.to_owned(), s);
                }
                parallel_minis.insert(part.to_owned(), mini);
                continue;
            }
        }
        small_jobs.push((part.to_owned(), shell.as_str(), sheet));
    }

    if small_jobs.len() >= 2 {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .min(8)
            .min(small_jobs.len());
        let next_job = std::sync::atomic::AtomicUsize::new(0);
        let sst_ref = fresh_sst.as_ref();
        let results: std::sync::Mutex<
            Vec<Result<(String, tempfile::TempPath, Option<(u32, u64)>)>>,
        > = std::sync::Mutex::new(Vec::with_capacity(small_jobs.len()));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let jobs_ref = &small_jobs;
                let next_ref = &next_job;
                let res_ref = &results;
                scope.spawn(move || loop {
                    let idx = next_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((part, shell, sheet)) = jobs_ref.get(idx) else {
                        break;
                    };
                    let one = (|| -> Result<(String, tempfile::TempPath, Option<(u32, u64)>)> {
                        let (f, tp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
                        let mut zw = ZipWriter::new(BufWriter::with_capacity(128 * 1024, f));
                        zw.start_file(
                            part.as_str(),
                            SimpleFileOptions::default()
                                .compression_method(CompressionMethod::Deflated)
                                .compression_level(Some(2)),
                        )?;
                        let mut vw = VerifiedXmlWriter::new(&mut zw);
                        write_sheet(
                            &mut vw,
                            shell,
                            sheet,
                            sst_ref.map(|s| s.map_for(sheet)),
                            Some(direct_xf),
                        )?;
                        let sig = vw.finish(Some("worksheet"))?;
                        zw.finish()?.flush()?;
                        Ok((part.clone(), tp, sig))
                    })();
                    res_ref.lock().unwrap().push(one);
                });
            }
        });
        for item in results.into_inner().unwrap() {
            let (part, tp, sig) = item?;
            if let Some(s) = sig {
                trusted_parts.insert(part.clone(), s);
            }
            parallel_parts.insert(part, tp);
        }
    }

    let file = File::create(path).with_context(|| format!("创建文件失败 {}", path.display()))?;
    let mut out = ZipWriter::new(BufWriter::with_capacity(256 * 1024, file));
    let meta_opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(2));
    out.start_file("[Content_Types].xml", meta_opts)?;
    out.write_all(ct.as_bytes())?;
    out.start_file("_rels/.rels", meta_opts)?;
    out.write_all(root_rels.as_bytes())?;
    out.start_file("xl/workbook.xml", meta_opts)?;
    out.write_all(wb_xml.as_bytes())?;
    out.start_file("xl/_rels/workbook.xml.rels", meta_opts)?;
    out.write_all(wb_rels.as_bytes())?;
    out.start_file(STYLES_PART, meta_opts)?;
    out.write_all(&fresh_styles.xml)?;
    if let Some(sst) = &fresh_sst {
        if sst.xml.len() >= 2 * 1024 * 1024 {
            let _ = write_parallel_deflate_entry(&mut out, SST_PART, 2, Some("sst"), |w| {
                w.write_all(&sst.xml)?;
                Ok(true)
            })?;
        } else {
            out.start_file(SST_PART, meta_opts)?;
            out.write_all(&sst.xml)?;
        }
    }
    if dynamic {
        out.start_file(super::dynamic::PART, meta_opts)?;
        out.write_all(super::dynamic::METADATA.as_bytes())?;
    }
    for ((part, sheet), shell) in ordered_sheets.iter().copied().zip(shells.iter()) {
        if let Some(mini) = parallel_minis.remove(part) {
            let mut mini_zip = ZipArchive::new(std::io::Cursor::new(mini))?;
            out.raw_copy_file(mini_zip.by_index_raw(0)?)?;
        } else if let Some(tp) = parallel_parts.remove(part) {
            let mut part_zip = ZipArchive::new(File::open(&tp)?)?;
            out.raw_copy_file(part_zip.by_index_raw(0)?)?;
        } else {
            let sheet_sst = fresh_sst.as_ref().map(|s| s.map_for(sheet));
            out.start_file(part, meta_opts)?;
            let mut vw = VerifiedXmlWriter::new(&mut out);
            write_sheet(&mut vw, shell, sheet, sheet_sst, Some(direct_xf))?;
            if let Some(s) = vw.finish(Some("worksheet"))? {
                trusted_parts.insert(part.to_owned(), s);
            }
        }
    }
    out.finish()?.flush()?;
    Ok(FreshResult {
        styles: Some(fresh_styles.saved),
        trusted_parts,
    })
}

pub(super) fn write(
    path: &Path,
    wb: &Workbook,
    selected: &[bool],
    direct_styles: bool,
) -> Result<FreshResult> {
    let targets = crate::patch::sheet_targets(path)?;
    let mut ordered_sheets: Vec<(&str, &Sheet)> = Vec::new();
    let mut sheets = HashMap::new();
    for (sheet, &streamed) in wb.sheets.iter().zip(selected) {
        if streamed {
            let part = targets.get(&sheet.name).context("找不到流式工作表部件")?;
            ensure!(
                sheets.insert(part.as_str(), sheet).is_none(),
                "工作表部件重复"
            );
            ordered_sheets.push((part.as_str(), sheet));
        }
    }
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let dynamic = sheets.values().any(|s| has_dynamic(s));
    ensure!(
        !dynamic || zip.index_for_name(super::dynamic::PART).is_none(),
        "新建模板的元数据部件已占用"
    );
    let fresh_styles = if direct_styles {
        Some(FreshStyles::build(Some(&mut zip), wb, &ordered_sheets)?)
    } else {
        None
    };
    let direct_xf = fresh_styles.as_ref().map(|s| s.sid_to_xf.as_slice());
    let fresh_sst = FreshSst::build(Some(&mut zip), &ordered_sheets)?;
    let need_sst_decl = fresh_sst.as_ref().is_some_and(|s| !s.existed_in_zip);
    let sst_ref = fresh_sst.as_ref();

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let mut trusted_parts: HashMap<String, (u32, u64)> = HashMap::new();
    if let Some(st) = &fresh_styles {
        trusted_parts.insert(STYLES_PART.to_owned(), st.sig);
    }
    if let Some(sst) = &fresh_sst
        && sst.modified
    {
        let mut crc = flate2::Crc::new();
        crc.update(&sst.xml);
        trusted_parts.insert(SST_PART.to_owned(), (crc.sum(), sst.xml.len() as u64));
    }

    // Use intra-sheet parallel XML+deflate for large sheets (>= 32,768), and sheet-level
    // parallelism for remaining sheets!
    let mut parallel_minis: HashMap<String, Vec<u8>> = HashMap::new();
    let mut parallel_parts: HashMap<String, tempfile::TempPath> = HashMap::new();
    let mut small_jobs: Vec<(String, String, &Sheet)> = Vec::new();
    if ordered_sheets.len() >= 2 {
        for &(part, sheet) in &ordered_sheets {
            let shell = super::read_zip_text(&mut zip, part)?;
            if let Some(dxf) = direct_xf
                && sheet_estimated_work(sheet) >= 32_768
                && let Some((mini, sig)) =
                    write_sheet_parallel_zip(part, &shell, sheet, sst_ref.map(|s| s.map_for(sheet)), dxf)?
            {
                if let Some(s) = sig {
                    trusted_parts.insert(part.to_owned(), s);
                }
                parallel_minis.insert(part.to_owned(), mini);
                continue;
            }
            small_jobs.push((part.to_owned(), shell, sheet));
        }
    }
    if small_jobs.len() >= 2 {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .min(8)
            .min(small_jobs.len());
        let next_job = std::sync::atomic::AtomicUsize::new(0);
        let results: std::sync::Mutex<
            Vec<Result<(String, tempfile::TempPath, Option<(u32, u64)>)>>,
        > = std::sync::Mutex::new(Vec::with_capacity(small_jobs.len()));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let jobs_ref = &small_jobs;
                let next_ref = &next_job;
                let res_ref = &results;
                scope.spawn(move || loop {
                    let idx = next_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((part, shell, sheet)) = jobs_ref.get(idx) else {
                        break;
                    };
                    let one = (|| -> Result<(String, tempfile::TempPath, Option<(u32, u64)>)> {
                        let (f, tp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
                        let mut zw = ZipWriter::new(BufWriter::with_capacity(128 * 1024, f));
                        zw.start_file(
                            part.as_str(),
                            SimpleFileOptions::default()
                                .compression_method(CompressionMethod::Deflated)
                                .compression_level(Some(2)),
                        )?;
                        let mut vw = VerifiedXmlWriter::new(&mut zw);
                        write_sheet(
                            &mut vw,
                            shell,
                            sheet,
                            sst_ref.map(|s| s.map_for(sheet)),
                            direct_xf,
                        )?;
                        let sig = vw.finish(Some("worksheet"))?;
                        zw.finish()?.flush()?;
                        Ok((part.clone(), tp, sig))
                    })();
                    res_ref.lock().unwrap().push(one);
                });
            }
        });
        for item in results.into_inner().unwrap() {
            let (part, tp, sig) = item?;
            if let Some(s) = sig {
                trusted_parts.insert(part.clone(), s);
            }
            parallel_parts.insert(part, tp);
        }
    }

    let (file, temp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
    let mut out = ZipWriter::new(BufWriter::with_capacity(256 * 1024, file));
    let mut wrote_styles = false;
    for i in 0..zip.len() {
        let part = zip.name_for_index(i).context("缺少 ZIP 条目名")?.to_owned();
        if let Some(sheet) = sheets.remove(part.as_str()) {
            if let Some(mini) = parallel_minis.remove(&part) {
                let mut mini_zip = ZipArchive::new(std::io::Cursor::new(mini))?;
                out.raw_copy_file(mini_zip.by_index_raw(0)?)?;
            } else if let Some(tp) = parallel_parts.remove(&part) {
                let mut part_zip = ZipArchive::new(File::open(&tp)?)?;
                out.raw_copy_file(part_zip.by_index_raw(0)?)?;
            } else {
                let shell = super::read_zip_text(&mut zip, &part)?;
                let sheet_sst = sst_ref.map(|s| s.map_for(sheet));
                if let Some(dxf) = direct_xf
                    && let Some((mini, sig)) =
                        write_sheet_parallel_zip(&part, &shell, sheet, sheet_sst, dxf)?
                {
                    let mut mini_zip = ZipArchive::new(std::io::Cursor::new(mini))?;
                    out.raw_copy_file(mini_zip.by_index_raw(0)?)?;
                    if let Some(s) = sig {
                        trusted_parts.insert(part, s);
                    }
                } else {
                    out.start_file(
                        &part,
                        SimpleFileOptions::default()
                            .compression_method(CompressionMethod::Deflated)
                            .compression_level(Some(2)),
                    )?;
                    let mut vw = VerifiedXmlWriter::new(&mut out);
                    write_sheet(&mut vw, &shell, sheet, sheet_sst, direct_xf)?;
                    if let Some(s) = vw.finish(Some("worksheet"))? {
                        trusted_parts.insert(part, s);
                    }
                }
            }
        } else if part == STYLES_PART && fresh_styles.is_some() {
            let st = fresh_styles.as_ref().unwrap();
            out.start_file(
                STYLES_PART,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .compression_level(Some(2)),
            )?;
            out.write_all(&st.xml)?;
            wrote_styles = true;
        } else if part == SST_PART && fresh_sst.as_ref().is_some_and(|s| s.modified) {
            let sst = fresh_sst.as_ref().unwrap();
            if sst.xml.len() >= 2 * 1024 * 1024 {
                let _ = write_parallel_deflate_entry(&mut out, SST_PART, 2, Some("sst"), |w| {
                    w.write_all(&sst.xml)?;
                    Ok(true)
                })?;
            } else {
                out.start_file(
                    SST_PART,
                    SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Deflated)
                        .compression_level(Some(2)),
                )?;
                out.write_all(&sst.xml)?;
            }
        } else if (dynamic || need_sst_decl)
            && matches!(part.as_str(), super::dynamic::RELS | super::dynamic::TYPES)
        {
            let mut xml = super::read_zip_text(&mut zip, &part)?;
            if dynamic {
                super::dynamic::declare(&mut xml, &part)?;
            }
            if need_sst_decl {
                declare_sst(&mut xml, &part)?;
            }
            out.start_file(
                part,
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )?;
            out.write_all(xml.as_bytes())?;
        } else {
            let raw = zip.by_index_raw(i)?;
            if !wb.load_incomplete && (raw.size() == 0 || raw.compressed_size() > 0) {
                trusted_parts.insert(part, (raw.crc32(), raw.size()));
            }
            out.raw_copy_file(raw)?;
        }
    }
    ensure!(sheets.is_empty(), "缺少流式工作表部件");
    if let Some(st) = &fresh_styles {
        ensure!(wrote_styles, "新建模板缺少样式表部件");
        let _ = st;
    }
    if let Some(sst) = &fresh_sst
        && !sst.existed_in_zip
    {
        out.start_file(
            SST_PART,
            SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(2)),
        )?;
        out.write_all(&sst.xml)?;
    }
    if dynamic {
        out.start_file(
            super::dynamic::PART,
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )?;
        out.write_all(super::dynamic::METADATA.as_bytes())?;
    }
    out.finish()?.flush()?;
    drop(zip);
    temp.persist(path)?;
    Ok(FreshResult {
        styles: fresh_styles.map(|s| s.saved),
        trusted_parts,
    })
}

struct ScannedBaseSheet {
    root_open_tag: String,
    root_qname: String,
    prefix: String,
    header_end: usize,
    footer_start: usize,
    root_was_empty: bool,
    orig_row_tags: BTreeMap<u32, String>,
    orig_row_xf: HashMap<u32, u16>,
    orig_cell_xf: HashMap<CellRef, u16>,
}

fn scan_base_sheet(xml: &str, need_cell_xf: bool) -> Result<ScannedBaseSheet> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut depth = 0usize;
    let mut root_open_tag = String::new();
    let mut root_qname = String::new();
    let mut prefix = String::new();
    let mut root_was_empty = false;
    let mut data_start: Option<usize> = None;
    let mut data_end: Option<usize> = None;
    let mut in_sheet_data = false;
    let mut cur_row: Option<u32> = None;
    let mut next_row: u32 = 0;
    let mut next_col: u32 = 0;
    let mut orig_row_tags: BTreeMap<u32, String> = BTreeMap::new();
    let mut orig_row_xf: HashMap<u32, u16> = HashMap::new();
    let mut orig_cell_xf: HashMap<CellRef, u16> = HashMap::new();

    loop {
        let start = reader.buffer_position() as usize;
        let ev = reader.read_event()?;
        let end = reader.buffer_position() as usize;
        match ev {
            quick_xml::events::Event::Start(ref e) | quick_xml::events::Event::Empty(ref e) => {
                let empty = matches!(ev, quick_xml::events::Event::Empty(_));
                if depth == 0 {
                    ensure!(
                        e.local_name().as_ref() == "worksheet",
                        "基准工作表根节点无效"
                    );
                    root_qname = e.name().as_ref().to_owned();
                    prefix = root_qname
                        .split_once(':')
                        .map(|(p, _)| format!("{p}:"))
                        .unwrap_or_default();
                    let raw = &xml[start..end];
                    if empty {
                        root_was_empty = true;
                        let stem = raw.strip_suffix("/>").unwrap_or(raw);
                        root_open_tag = format!("{stem}>");
                        data_start = Some(end);
                        data_end = Some(end);
                    } else {
                        root_open_tag = raw.to_owned();
                        depth = 1;
                    }
                } else if depth == 1 {
                    if e.local_name().as_ref() == "sheetData" && data_start.is_none() {
                        data_start = Some(start);
                        if empty {
                            data_end = Some(end);
                        } else {
                            in_sheet_data = true;
                            depth = 2;
                        }
                    } else if !empty {
                        depth = 2;
                    }
                } else if in_sheet_data && depth == 2 && e.local_name().as_ref() == "row" {
                    let mut explicit_r: Option<u32> = None;
                    let mut row_s: Option<u16> = None;
                    let mut has_extra_attr = false;
                    let mut has_spans = false;
                    for a in e.attributes().flatten() {
                        match a.key.as_ref() {
                            "r" => {
                                if let Ok(v) = a.value.parse::<u32>()
                                    && let Some(idx) = v.checked_sub(1)
                                {
                                    explicit_r = Some(idx);
                                }
                            }
                            "spans" => {
                                has_spans = true;
                            }
                            "s" => {
                                has_extra_attr = true;
                                if let Ok(v) = a.value.parse::<u16>() {
                                    row_s = Some(v);
                                }
                            }
                            _ => {
                                has_extra_attr = true;
                            }
                        }
                    }
                    let row_idx = explicit_r.unwrap_or(next_row);
                    next_row = row_idx.saturating_add(1);
                    next_col = 0;
                    if let Some(xf) = row_s {
                        orig_row_xf.insert(row_idx, xf);
                    }
                    if has_extra_attr && explicit_r.is_some() {
                        let raw = &xml[start..end];
                        let clean = if has_spans {
                            let mut tag =
                                quick_xml::events::BytesStart::new(e.name().as_ref().to_owned());
                            for a in e.attributes().flatten() {
                                if a.key.as_ref() != "spans" {
                                    tag.push_attribute(a);
                                }
                            }
                            let mut w = quick_xml::Writer::new(Vec::new());
                            w.write_event(quick_xml::events::Event::Start(tag))?;
                            String::from_utf8(w.into_inner())?
                        } else if let Some(stem) = raw.strip_suffix("/>") {
                            format!("{stem}>")
                        } else {
                            raw.to_owned()
                        };
                        orig_row_tags.insert(row_idx, clean);
                    }
                    if !empty {
                        cur_row = Some(row_idx);
                        depth = 3;
                    }
                } else if in_sheet_data && depth == 3 && e.local_name().as_ref() == "c" {
                    if need_cell_xf {
                        let mut parsed_ref: Option<(u32, u32)> = None;
                        let mut cell_s: Option<u16> = None;
                        for a in e.attributes().flatten() {
                            match a.key.as_ref() {
                                "r" => {
                                    parsed_ref = crate::sparse::parse_ref(&a.value);
                                }
                                "s" => {
                                    if let Ok(v) = a.value.parse::<u16>() {
                                        cell_s = Some(v);
                                    }
                                }
                                _ => {}
                            }
                        }
                        let (r, c) = match parsed_ref {
                            Some((r, c)) => {
                                next_col = c.saturating_add(1);
                                (r, c)
                            }
                            None => {
                                let r = cur_row.unwrap_or(0);
                                let c = next_col;
                                next_col = c.saturating_add(1);
                                (r, c)
                            }
                        };
                        if let Some(xf) = cell_s {
                            orig_cell_xf.insert(CellRef::new(r, c), xf);
                        }
                    }
                    if !empty {
                        depth = 4;
                    }
                } else if !empty {
                    depth += 1;
                }
            }
            quick_xml::events::Event::End(_) => {
                depth = depth.checked_sub(1).context("基准工作表标签不匹配")?;
                if in_sheet_data && depth == 1 {
                    data_end = Some(end);
                    in_sheet_data = false;
                } else if depth == 2 {
                    cur_row = None;
                }
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }

    ensure!(!root_qname.is_empty(), "基准工作表缺少根元素");
    let (header_end, footer_start) = match (data_start, data_end) {
        (Some(s), Some(e)) => (s, e),
        _ => {
            // Malformed worksheet without <sheetData>: insert before </worksheet>.
            let close = xml
                .rfind(&format!("</{root_qname}>"))
                .context("基准工作表缺少闭合标签")?;
            (close, close)
        }
    };

    Ok(ScannedBaseSheet {
        root_open_tag,
        root_qname,
        prefix,
        header_end,
        footer_start,
        root_was_empty,
        orig_row_tags,
        orig_row_xf,
        orig_cell_xf,
    })
}

fn collect_orig_col_xfs(
    header: &str,
    root_qname: &str,
    wanted_cols: &crate::model::CellMetadata<u32, u16>,
) -> HashMap<u32, u16> {
    let mut out = HashMap::new();
    if wanted_cols.is_empty() {
        return out;
    }
    let closed = format!("{header}</{root_qname}>");
    let Ok(doc) = roxmltree::Document::parse(&closed) else {
        return out;
    };
    let root = doc.root_element();
    for cols in root
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "cols")
    {
        for col in cols
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "col")
        {
            let (Some(min_s), Some(max_s), Some(st_s)) = (
                col.attribute("min"),
                col.attribute("max"),
                col.attribute("style"),
            ) else {
                continue;
            };
            let (Ok(min1), Ok(max1), Ok(xf)) = (
                min_s.parse::<u32>(),
                max_s.parse::<u32>(),
                st_s.parse::<u16>(),
            ) else {
                continue;
            };
            if min1 == 0 || min1 > max1 {
                continue;
            }
            let lo = min1 - 1;
            let hi = max1.min(crate::sync::MAX_COLS) - 1;
            for &c in wanted_cols.keys() {
                if c >= lo && c <= hi {
                    out.insert(c, xf);
                }
            }
        }
    }
    out
}

fn row_tag_has_non_r_attrs(open_tag: &str) -> bool {
    let mut r = quick_xml::Reader::from_str(open_tag);
    if let Ok(
        quick_xml::events::Event::Start(e) | quick_xml::events::Event::Empty(e),
    ) = r.read_event()
    {
        return e
            .attributes()
            .flatten()
            .any(|a| !matches!(a.key.as_ref(), "r" | "spans"));
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn write_rebuilt_sheet(
    mut out: impl Write,
    base_xml: &str,
    part: &str,
    wb: &Workbook,
    sheet: &Sheet,
    sst: Option<SheetSstLookup<'_>>,
    cm_index: Option<u32>,
    style_appender: &mut Option<StyleAppender>,
    base_styles_xml: &mut Option<String>,
) -> Result<()> {
    let need_cell_xf = !sheet.styles.is_empty()
        || sheet.styles_dirty
        || !sheet.style_edits.is_empty()
        || sheet
            .source_cells
            .as_ref()
            .is_some_and(|s| !s.base_styles().is_empty());
    let scanned = scan_base_sheet(base_xml, need_cell_xf)?;
    let prefix = scanned.prefix.as_str();
    let src = sheet
        .source_cells
        .as_ref()
        .filter(|_| sheet.source_part.as_deref() == Some(part));
    let is_new_sheet = sheet.source_part.is_none();

    let mut resolve_new_xf = |sid: u16| -> Result<u16> {
        if let Some(xf) = src.and_then(|s| s.existing_xf(sid, &wb.styles)) {
            return Ok(xf);
        }
        if style_appender.is_none() {
            let xml = base_styles_xml
                .take()
                .unwrap_or_else(|| minimal_styles_xml().to_owned());
            *style_appender = Some(StyleAppender::new(xml)?);
        }
        style_appender.as_mut().unwrap().intern_style(wb, sid)
    };

    let mut header = if scanned.root_was_empty {
        scanned.root_open_tag.clone()
    } else {
        base_xml[..scanned.header_end].to_owned()
    };
    let mut footer = if scanned.root_was_empty {
        format!("</{}>", scanned.root_qname)
    } else {
        base_xml[scanned.footer_start..].to_owned()
    };

    if sheet.source_needs_dom_repair || sheet.col_styles_dirty {
        header = crate::patch::layout_xml::repair_column_bounds(&header)?;
        header = crate::patch::layout_xml::strip_col_children(&header)?;
    }

    let orig_col_xf = collect_orig_col_xfs(&header, &scanned.root_qname, &sheet.col_styles);
    let mut col_xf_map: HashMap<u32, u16> = HashMap::with_capacity(sheet.col_styles.len());
    let mut col_updates: BTreeMap<u32, Option<u16>> = BTreeMap::new();
    let update_cols = sheet.col_styles_dirty || (is_new_sheet && !sheet.col_styles.is_empty());
    if update_cols && let Some(s) = src {
        for &c in s.col_styles.keys() {
            if !sheet.col_styles.contains_key(&c) {
                col_updates.insert(c, None);
            }
        }
    }
    for (&c, &sid) in &sheet.col_styles {
        let unchanged = !sheet.col_styles_dirty
            || src.is_some_and(|s| {
                s.col_styles.get(&c) == Some(&sid) && s.style_unchanged(sid, &wb.styles)
            });
        let xf = if unchanged && let Some(&raw) = orig_col_xf.get(&c) {
            raw
        } else {
            let resolved = resolve_new_xf(sid)?;
            if update_cols {
                col_updates.insert(c, Some(resolved));
            }
            resolved
        };
        col_xf_map.insert(c, xf);
    }
    if !col_updates.is_empty() {
        header =
            crate::patch::layout_xml::styled_columns_root(&header, &scanned.root_qname, &col_updates)?;
    }

    let bounds = sheet_styled_bounds(sheet);
    header = update_dimension_in_header(&header, &scanned.root_qname, bounds);

    if sheet.merges_dirty || (is_new_sheet && !sheet.merges.is_empty()) {
        footer = patch_merge_cells(&scanned.root_open_tag, &footer, &sheet.merges, prefix);
    }

    // Resolve row_styles XFs up front.
    let mut row_xf_map: BTreeMap<u32, u16> = BTreeMap::new();
    for (&r, &sid) in &sheet.row_styles {
        let unchanged = !sheet.row_styles_dirty
            || src.is_some_and(|s| {
                s.row_styles.get(&r) == Some(&sid) && s.style_unchanged(sid, &wb.styles)
            });
        let xf = if unchanged && let Some(&raw) = scanned.orig_row_xf.get(&r) {
            raw
        } else {
            resolve_new_xf(sid)?
        };
        row_xf_map.insert(r, xf);
    }

    let arrays = valid_arrays_map(sheet);
    let skip_spilled = cm_index.is_none() && !sheet.spilled.is_empty();

    const FLUSH_AT: usize = 128 * 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(FLUSH_AT + 16 * 1024);
    buf.extend_from_slice(header.as_bytes());
    write!(buf, "<{prefix}sheetData>")?;

    let mut orig_rows = scanned.orig_row_tags.into_iter().peekable();
    let mut style_rows = row_xf_map.iter().peekable();
    let mut cell_iter = sheet.iter_with_styles().peekable();

    loop {
        let cell_r = cell_iter.peek().map_or(u32::MAX, |(k, _, _)| k.row);
        let style_r = style_rows.peek().map_or(u32::MAX, |&(&r, _)| r);
        let orig_r = orig_rows.peek().map_or(u32::MAX, |&(r, _)| r);
        let r = cell_r.min(style_r).min(orig_r);
        if r == u32::MAX {
            break;
        }
        let has_cells = r == cell_r;
        let row_xf = if r == style_r {
            let (_, &xf) = style_rows.next().unwrap();
            Some(xf)
        } else {
            None
        };
        let orig_tag = if r == orig_r {
            Some(orig_rows.next().unwrap().1)
        } else {
            None
        };

        // Determine row opening tag (preserving original ht/hidden/outline/vendor attrs).
        let open_tag = if let Some(tag) = orig_tag {
            if let Some(xf) = row_xf {
                if scanned.orig_row_xf.get(&r) == Some(&xf) && !sheet.row_styles_dirty {
                    Some(tag)
                } else {
                    Some(crate::patch::layout_xml::styled_row(&tag, Some(xf))?)
                }
            } else if scanned.orig_row_xf.contains_key(&r) {
                let stripped = crate::patch::layout_xml::styled_row(&tag, None)?;
                if has_cells || row_tag_has_non_r_attrs(&stripped) {
                    Some(stripped)
                } else {
                    None
                }
            } else {
                Some(tag)
            }
        } else if let Some(xf) = row_xf {
            Some(format!(
                "<{prefix}row r=\"{}\" s=\"{xf}\" customFormat=\"1\">",
                r + 1
            ))
        } else if has_cells {
            Some(format!("<{prefix}row r=\"{}\">", r + 1))
        } else {
            None
        };

        let Some(open_tag) = open_tag else {
            continue;
        };

        if !has_cells {
            if let Some(stem) = open_tag.strip_suffix('>') {
                buf.extend_from_slice(stem.as_bytes());
                buf.extend_from_slice(b"/>");
            } else {
                buf.extend_from_slice(open_tag.as_bytes());
                write!(buf, "</{prefix}row>")?;
            }
            if buf.len() >= FLUSH_AT {
                out.write_all(&buf)?;
                buf.clear();
            }
            continue;
        }

        // Write cells for row `r` into a row buffer so empty rows (e.g. only skipped spills)
        // don't emit unwanted empty `<row>` tags unless row metadata requires them.
        let row_mark = buf.len();
        buf.extend_from_slice(open_tag.as_bytes());
        let body_mark = buf.len();

        while let Some((key, cell, explicit)) = cell_iter.next_if(|(k, _, _)| k.row == r) {
            let row_styled = row_xf_map.contains_key(&r);
            let col_styled = col_xf_map.contains_key(&key.col);
            let resolve_cell_xf = |explicit: Option<u16>,
                                   resolve_new: &mut dyn FnMut(u16) -> Result<u16>|
             -> Result<Option<u16>> {
                if let Some(sid) = explicit {
                    let unchanged = !sheet.style_edits.contains(&key)
                        && src.is_some_and(|s| {
                            s.base_styles().get(&key).copied() == Some(sid)
                                && s.style_unchanged(sid, &wb.styles)
                        });
                    if unchanged {
                        if let Some(&raw) = scanned.orig_cell_xf.get(&key) {
                            return Ok(Some(raw));
                        }
                        if sid == 0 {
                            return Ok((row_styled || col_styled).then_some(0));
                        }
                    }
                    if sid == 0 {
                        return Ok((row_styled || col_styled).then_some(0));
                    }
                    Ok(Some(resolve_new(sid)?))
                } else {
                    Ok(row_xf_map
                        .get(&r)
                        .copied()
                        .or_else(|| col_xf_map.get(&key.col).copied()))
                }
            };

            if let Some(cell) = cell {
                if skip_spilled && sheet.spilled.contains(&key) {
                    continue;
                }
                let xf = resolve_cell_xf(explicit, &mut resolve_new_xf)?;
                write_value_buf(
                    &mut buf, sheet, &arrays, sst, key, cell, xf, prefix, cm_index, None,
                )?;
            } else if let Some(sid) = explicit {
                let keep_raw_blank = !sheet.style_edits.contains(&key)
                    && scanned.orig_cell_xf.get(&key).is_some_and(|&x| x != 0)
                    && src.is_some_and(|s| {
                        s.base_styles().get(&key).copied() == Some(sid)
                            && s.style_unchanged(sid, &wb.styles)
                    });
                if sid != 0 || row_styled || col_styled || keep_raw_blank {
                    if let Some(xf) = resolve_cell_xf(Some(sid), &mut resolve_new_xf)? {
                        write_styled_blank_buf(&mut buf, key, xf, prefix);
                    }
                }
            }
        }

        if buf.len() == body_mark && row_xf.is_none() && !row_tag_has_non_r_attrs(&open_tag) {
            buf.truncate(row_mark);
        } else {
            write!(buf, "</{prefix}row>")?;
        }
        if buf.len() >= FLUSH_AT {
            out.write_all(&buf)?;
            buf.clear();
        }
    }

    write!(buf, "</{prefix}sheetData>")?;
    buf.extend_from_slice(footer.as_bytes());
    out.write_all(&buf)?;
    out.flush()?;
    Ok(())
}

/// Pure native streaming repackager for existing OOXML workbooks when `xlsx_delta`
/// cannot handle the edit in-place (e.g., sheet add/delete/rename/reorder, complex style
/// changes, structural row/col shifts, notes/merges/rich edits, etc.).
///
/// - Bucket 1 (untouched sheets): O(1) `raw_copy_file` directly from `base`.
/// - Bucket 2 (reusable grid with value/cache edits): streaming `stage_grid_edits`.
/// - Bucket 3 (dirty/new sheets): full native streaming rebuild preserving base
///   non-`<sheetData>` header/footer, row attributes, unchanged raw XFs, `StyleAppender`,
///   and existing SST indices `0..N-1`.
pub(super) fn rebuild_from_base(
    base: &Path,
    target: &Path,
    wb: &Workbook,
) -> Result<FreshResult> {
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut zip = ZipArchive::new(File::open(base).with_context(|| {
        format!("打开保存基准失败 {}", base.display())
    })?)?;
    let wb_xml_orig = super::read_zip_text(&mut zip, "xl/workbook.xml")?;
    let wb_rels_orig = super::read_zip_text(&mut zip, "xl/_rels/workbook.xml.rels")?;
    let targets = crate::patch::sheet_targets_xml(Some(&wb_xml_orig), Some(&wb_rels_orig));

    enum SheetBucket {
        RawCopy,
        StagedGrid(tempfile::NamedTempFile),
        Rebuild,
    }

    let mut buckets: HashMap<String, SheetBucket> = HashMap::with_capacity(wb.sheets.len());
    let mut rebuild_ordered: Vec<(String, &Sheet)> = Vec::new();
    let mut any_cell_or_struct_change = wb.names_dirty;

    for sheet in &wb.sheets {
        let part = targets
            .get(&sheet.name)
            .with_context(|| format!("基准包缺少工作表部件: {}", sheet.name))?
            .clone();
        if sheet.source_part.is_none() || sheet.cell_data_needs_save() {
            any_cell_or_struct_change = true;
        }
        let same_part = sheet.source_part.as_deref() == Some(part.as_str());
        let grid_style_meta_clean = !sheet.styles_dirty
            && !sheet.row_styles_dirty
            && !sheet.col_styles_dirty
            && sheet.style_edits.is_empty()
            && !sheet.merges_dirty
            && !sheet.rich_dirty;

        if same_part && grid_style_meta_clean && !sheet.cell_data_needs_save() {
            buckets.insert(part, SheetBucket::RawCopy);
            continue;
        }

        if same_part
            && grid_style_meta_clean
            && !sheet.source_needs_dom_repair
            && let Some(src) = sheet.source_cells.as_ref()
            && src.reusable_grid(&mut zip, &part, sheet)
            && let Some(staged) = src.stage_grid_edits(&mut zip, &part, sheet, &wb.styles)?
        {
            buckets.insert(part, SheetBucket::StagedGrid(staged));
            continue;
        }

        rebuild_ordered.push((part.clone(), sheet));
        buckets.insert(part, SheetBucket::Rebuild);
    }

    // Determine dynamic array metadata handling.
    let has_existing_meta = zip.index_for_name(super::dynamic::PART).is_some();
    let any_rebuilt_dynamic = rebuild_ordered.iter().any(|(_, s)| has_dynamic(s));
    let (cm_index, create_metadata) = if has_existing_meta {
        (super::dynamic::editable(&mut zip), false)
    } else if any_rebuilt_dynamic {
        (Some(1), true)
    } else {
        (None, false)
    };

    // Build incremental SST ONLY for Bucket 3 rebuilt sheets (existing 0..N-1 preserved!).
    let rebuild_refs: Vec<(&str, &Sheet)> = rebuild_ordered
        .iter()
        .map(|(p, s)| (p.as_str(), *s))
        .collect();
    let fresh_sst = FreshSst::build(Some(&mut zip), &rebuild_refs)?;
    let need_sst_decl = fresh_sst.as_ref().is_some_and(|s| !s.existed_in_zip);
    let sst_ref = fresh_sst.as_ref();

    // Prepare optional calcChain invalidation + date1904 update on xl/workbook.xml.
    let mut extra_parts: HashMap<String, Vec<u8>> = HashMap::new();
    let mut deleted_parts: HashSet<String> = HashSet::new();
    if any_cell_or_struct_change
        && let Some((chain_parts, removed)) =
            super::delta::invalidate_chain(&mut zip, wb.calc_manual)?
    {
        extra_parts.extend(chain_parts);
        deleted_parts.extend(removed);
    }

    let current_wb_xml = match extra_parts.get("xl/workbook.xml") {
        Some(b) => String::from_utf8(b.clone())?,
        None => wb_xml_orig,
    };
    let dated_wb_xml = crate::patch::set_date1904(&current_wb_xml, wb.date_system.is_1904());
    if dated_wb_xml != current_wb_xml {
        extra_parts.insert("xl/workbook.xml".to_owned(), dated_wb_xml.into_bytes());
    }

    if create_metadata || need_sst_decl {
        for meta_part in [super::dynamic::RELS, super::dynamic::TYPES] {
            if zip.index_for_name(meta_part).is_some() {
                let mut xml = match extra_parts.remove(meta_part) {
                    Some(b) => String::from_utf8(b)?,
                    None => super::read_zip_text(&mut zip, meta_part)?,
                };
                if create_metadata {
                    super::dynamic::declare(&mut xml, meta_part)?;
                }
                if need_sst_decl {
                    declare_sst(&mut xml, meta_part)?;
                }
                extra_parts.insert(meta_part.to_owned(), xml.into_bytes());
            }
        }
    }

    // Lazily read xl/styles.xml only if a rebuilt sheet needs to intern a new style.
    let styles_existed = zip.index_for_name(STYLES_PART).is_some();
    let mut base_styles_xml = if styles_existed && !rebuild_ordered.is_empty() {
        Some(super::read_zip_text(&mut zip, STYLES_PART)?)
    } else {
        None
    };
    let mut style_appender: Option<StyleAppender> = None;

    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    // Pre-write rebuilt sheets into single-entry temp ZIPs so style_appender finishes
    // BEFORE we emit xl/styles.xml (which can appear before sheets in the base ZIP!).
    let mut rebuilt_zips: HashMap<String, tempfile::TempPath> =
        HashMap::with_capacity(rebuild_ordered.len());
    let mut trusted_parts: HashMap<String, (u32, u64)> = HashMap::new();
    for (part, sheet) in rebuild_ordered {
        let base_xml = super::read_zip_text(&mut zip, &part)?;
        let (f, tp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
        let mut zw = ZipWriter::new(BufWriter::with_capacity(128 * 1024, f));
        if sheet_estimated_work(sheet) >= 32_768 || base_xml.len() >= 2 * 1024 * 1024 {
            if let Some(Some(sig)) =
                write_parallel_deflate_entry(&mut zw, part.as_str(), 2, Some("worksheet"), |w| {
                    write_rebuilt_sheet(
                        w,
                        &base_xml,
                        &part,
                        wb,
                        sheet,
                        sst_ref.map(|s| s.map_for(sheet)),
                        cm_index,
                        &mut style_appender,
                        &mut base_styles_xml,
                    )?;
                    Ok(true)
                })?
            {
                trusted_parts.insert(part.clone(), sig);
            }
        } else {
            zw.start_file(
                part.as_str(),
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .compression_level(Some(2)),
            )?;
            let mut vw = VerifiedXmlWriter::new(&mut zw);
            write_rebuilt_sheet(
                &mut vw,
                &base_xml,
                &part,
                wb,
                sheet,
                sst_ref.map(|s| s.map_for(sheet)),
                cm_index,
                &mut style_appender,
                &mut base_styles_xml,
            )?;
            if let Some(sig) = vw.finish(Some("worksheet"))? {
                trusted_parts.insert(part.clone(), sig);
            }
        }
        zw.finish()?.flush()?;
        rebuilt_zips.insert(part, tp);
    }

    let updated_styles = style_appender.and_then(StyleAppender::finish);
    if let Some(bytes) = &updated_styles {
        let mut crc = flate2::Crc::new();
        crc.update(bytes);
        trusted_parts.insert(STYLES_PART.to_owned(), (crc.sum(), bytes.len() as u64));
    }
    if let Some(sst) = &fresh_sst
        && sst.modified
    {
        let mut crc = flate2::Crc::new();
        crc.update(&sst.xml);
        trusted_parts.insert(SST_PART.to_owned(), (crc.sum(), sst.xml.len() as u64));
    }

    let (file, temp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
    let mut out = ZipWriter::new(BufWriter::with_capacity(256 * 1024, file));
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(2));
    let mut wrote_styles = false;

    for i in 0..zip.len() {
        let part = zip.name_for_index(i).context("缺少 ZIP 条目名")?.to_owned();
        if deleted_parts.contains(&part) {
            continue;
        }
        if let Some(tp) = rebuilt_zips.remove(&part) {
            let mut part_zip = ZipArchive::new(File::open(&tp)?)?;
            out.raw_copy_file(part_zip.by_index_raw(0)?)?;
            continue;
        }
        if let Some(SheetBucket::StagedGrid(staged)) = buckets.remove(&part) {
            out.start_file(&part, opts)?;
            let mut vw = VerifiedXmlWriter::new(&mut out);
            let mut reader = std::io::BufReader::new(staged.reopen()?);
            std::io::copy(&mut reader, &mut vw)?;
            if let Some(sig) = vw.finish(Some("worksheet"))? {
                trusted_parts.insert(part, sig);
            }
            continue;
        }
        if part == STYLES_PART && let Some(bytes) = &updated_styles {
            out.start_file(STYLES_PART, opts)?;
            out.write_all(bytes)?;
            wrote_styles = true;
            continue;
        }
        if part == SST_PART && fresh_sst.as_ref().is_some_and(|s| s.modified) {
            let sst = fresh_sst.as_ref().unwrap();
            if sst.xml.len() >= 2 * 1024 * 1024 {
                let _ = write_parallel_deflate_entry(&mut out, SST_PART, 2, Some("sst"), |w| {
                    w.write_all(&sst.xml)?;
                    Ok(true)
                })?;
            } else {
                out.start_file(SST_PART, opts)?;
                out.write_all(&sst.xml)?;
            }
            continue;
        }
        if let Some(bytes) = extra_parts.remove(&part) {
            out.start_file(&part, opts)?;
            out.write_all(&bytes)?;
            continue;
        }
        let raw = zip.by_index_raw(i)?;
        if !wb.load_incomplete && (raw.size() == 0 || raw.compressed_size() > 0) {
            trusted_parts.insert(part, (raw.crc32(), raw.size()));
        }
        out.raw_copy_file(raw)?;
    }

    ensure!(rebuilt_zips.is_empty(), "重建工作表部件缺失");
    if let Some(bytes) = &updated_styles
        && !wrote_styles
    {
        out.start_file(STYLES_PART, opts)?;
        out.write_all(bytes)?;
    }
    if let Some(sst) = &fresh_sst
        && !sst.existed_in_zip
    {
        out.start_file(SST_PART, opts)?;
        out.write_all(&sst.xml)?;
    }
    if create_metadata {
        out.start_file(super::dynamic::PART, opts)?;
        out.write_all(super::dynamic::METADATA.as_bytes())?;
    }
    out.finish()?.flush()?;
    drop(zip);
    temp.persist(target)?;

    Ok(FreshResult {
        styles: None,
        trusted_parts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    #[test]
    fn streamed_drawings_and_both_array_types_roundtrip_with_their_cached_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drawing-arrays.xlsx");
        let marked = crate::textbox::insert_drawing_tag(
            "<s:worksheet xmlns:s=\"urn:w\"><s:sheetData><s:extLst/></s:sheetData><s:tableParts/></s:worksheet>",
            "rId7",
        ).unwrap();
        let tree = roxmltree::Document::parse(&marked).unwrap();
        let tags: Vec<_> = tree
            .root_element()
            .children()
            .filter(|n| n.is_element())
            .map(|n| n.tag_name().name())
            .collect();
        assert_eq!(tags, ["sheetData", "drawing", "tableParts"]);
        let mut wb = Workbook::new_blank();
        let sheet = &mut wb.sheets[0];
        let number = |n| Cell {
            value: Value::Number(n),
            formula: None,
        };
        let formula = |text: &str| Cell {
            formula: Some(text.into()),
            ..Default::default()
        };
        for r in 0..16_384 {
            sheet.set(r, 0, number(f64::from(r)));
        }
        for r in 0..5 {
            sheet.set(r, 9, number(f64::from(2 * r)));
        }
        for r in 0..5 {
            sheet.set(r, 10, number(f64::from(3 * r)));
        }
        sheet.set(0, 1, formula("=SEQUENCE(2,2)"));
        sheet.set(0, 4, formula("=SEQUENCE(1,2)"));
        sheet.arrays.push((CellRef::new(0, 4), (0, 4, 0, 5)));
        sheet.set(0, 7, formula("=SEQUENCE(2)"));
        sheet.set(1, 7, number(109.0));
        let chart = crate::chart_write::chart_from_selection(
            sheet,
            &sheet.name,
            (0, 9, 4, 10),
            "Sales",
            crate::charts::ChartKind::Column,
        );
        sheet.charts.push(chart);
        let png = std::sync::Arc::new(vec![
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 4, 0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 252, 255, 31,
            0, 3, 3, 2, 0, 238, 86, 219, 145, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
        ]);
        sheet.pictures.push(crate::pic::Picture {
            rect: (1, 3, 9, 5),
            off: [0; 4],
            part: "xl/media/image1.png".into(),
            data: Some(png.clone()),
            ext: "png".into(),
            name: "Logo".into(),
        });
        crate::formula::recalc_all(&mut wb);
        assert!(supported(&wb.sheets[0]));
        let receipt = crate::xlsx::save_atomic_with_receipt(&path, &wb).unwrap();
        receipt.adopt_document(&mut wb, &path, true);
        assert!(!wb.sheets[0].charts[0].part.is_empty());
        let zip =
            |part| String::from_utf8(crate::fixups::read_part_file(&path, part).unwrap()).unwrap();
        let xml = zip("xl/worksheets/sheet1.xml");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let cell = |s| {
            doc.descendants()
                .find(|n| n.has_tag_name("c") && n.attribute("r") == Some(s))
                .unwrap()
        };
        for (r, cm, range) in [
            ("B1", Some("1"), "B1:C2"),
            ("E1", None, "E1:F1"),
            ("H1", Some("1"), "H1"),
        ] {
            assert_eq!(cell(r).attribute("cm"), cm);
            let f = cell(r).children().find(|n| n.has_tag_name("f")).unwrap();
            assert_eq!(f.attribute("t"), Some("array"));
            assert_eq!(f.attribute("ref"), Some(range));
        }
        let meta = zip(super::super::dynamic::PART);
        // Other producers may keep unrelated cell metadata before the dynamic record.
        let meta = meta.replace("<cellMetadata count=\"1\">","<cellMetadata count=\"2\"><bk/>");
        let xml = xml.replace("cm=\"1\"","cm=\"2\"");
        crate::fixups::write_parts(&path,&[(super::super::dynamic::PART.into(),meta.into_bytes()),
            ("xl/worksheets/sheet1.xml".into(),xml.into_bytes())]).unwrap();
        let mut reopened = crate::xlsx::load(&path).unwrap();
        let s = &reopened.sheets[0];
        assert_eq!(s.arrays, [(CellRef::new(0, 4), (0, 4, 0, 5))]);
        assert_eq!(s.spill_shapes[&CellRef::new(0, 1)], (2, 2));
        assert_eq!(s.spill_shapes[&CellRef::new(0, 7)], (0, 0));
        assert!(s.spilled.contains(&CellRef::new(1, 2)));
        assert_eq!(s.value(1, 2), Value::Number(4.0));
        assert_eq!(s.value(0, 5), Value::Number(2.0));
        assert_eq!(s.value(1, 7), Value::Number(109.0));
        assert_eq!(s.charts.len(), 1);
        assert_eq!(s.charts[0].series[0].vals, [3.0, 6.0, 9.0, 12.0]);
        assert_eq!(s.pictures.len(), 1);
        assert_eq!(
            crate::fixups::read_part_file(&path, &s.pictures[0].part).as_ref(),
            Some(&*png)
        );
        reopened.sheets[0].remove(1, 7);
        crate::formula::recalc_all(&mut reopened);
        assert_eq!(reopened.sheets[0].value(1, 7), Value::Number(2.0));
        let receipt = crate::xlsx::save_atomic_with_receipt(&path, &reopened).unwrap();
        assert!(receipt.used_cell_delta);
        let again = crate::xlsx::load(&path).unwrap();
        assert_eq!(again.sheets[0].spill_shapes[&CellRef::new(0, 7)], (2, 1));
        assert_eq!(again.sheets[0].charts.len(), 1);
        // A drawing that contains only a textbox initially has no chart/image relationships.
        let other = dir.path().join("textbox-chart.xlsx");
        let mut secondary = Workbook::new_blank();
        let s = &mut secondary.sheets[0];
        s.set(1, 0, number(1.0));
        s.set(1, 1, number(7.0));
        s.textboxes.push(crate::textbox::TextBox {
            text: "Label".into(),
            ..Default::default()
        });
        s.charts.push(crate::chart_write::chart_from_selection(
            s,
            &s.name,
            (0, 0, 1, 1),
            "Second",
            crate::charts::ChartKind::Line,
        ));
        crate::xlsx::save_atomic(&other, &secondary).unwrap();
        let back = crate::xlsx::load(&other).unwrap();
        assert_eq!(back.sheets[0].textboxes[0].text, "Label");
        assert_eq!(back.sheets[0].charts[0].series[0].vals, [7.0]);
    }

    #[test]
    fn streamed_table_notes_sparkline_and_rich_runs_survive_fresh_and_existing_atomic_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("objects.xlsx");
        let mut wb = Workbook::new_blank();
        let sheet = wb.sheet_mut(0);
        for r in 0..16_384 {
            sheet.set(r,0,Cell { value: Value::Number(f64::from(r)), formula: None });
        }
        sheet.set(0,0,Cell { value: Value::Text("Sales".into()), formula: None });
        let text = Cell { value: Value::Text("part 2".into()), formula: None };
        sheet.set(0,1,text.clone());
        let mut first = crate::richtext::RichRun::plain("part ");
        first.bold = true;
        first.font = Some("A&B \"C\"".into());
        let runs = vec![first,crate::richtext::RichRun::plain("2")];
        sheet.rich.insert((0,1),runs.clone());
        sheet.tables.push(crate::table::Table { name:"SalesTable".into(),display:"SalesTable".into(),
            range:(0,0,9,0),columns:vec!["Sales".into()],column_ids:vec![1],header_row:true, ..Default::default() });
        let note = crate::notes::Note {r:0,c:1,author:"Reviewer".into(),text:"retain comment".into()};
        sheet.notes.insert(CellRef::new(0,1),note.clone());
        let spark = crate::sparkline::Sparkline { r:4,c:2,source_sheet:Some("Sheet1".into()),rect:(1,0,3,0), ..Default::default() };
        sheet.sparklines.push(spark.clone());
        let textbox = crate::textbox::TextBox {text:"Quarterly report".into(), ..Default::default()};
        sheet.textboxes.push(textbox.clone());
        assert!(supported(sheet));
        crate::xlsx::save_atomic(&path,&wb).unwrap();
        let mut saved = crate::xlsx::load(&path).unwrap();
        assert_eq!(saved.sheets[0].rich[&(0,1)],runs);
        assert_eq!(saved.sheets[0].notes[&CellRef::new(0,1)],note);
        assert_eq!(saved.sheets[0].sparklines,[spark.clone()]);
        assert_eq!(saved.sheets[0].textboxes[0].text,textbox.text);
        assert_eq!(saved.sheets[0].tables[0].range,(0,0,9,0));
        // A formatting-only rich edit leaves the cell's searchable text identical.
        // Capture a vendor element in another row and require lexical preservation.
        let part = saved.sheets[0].source_part.clone().unwrap();
        let xml = String::from_utf8(crate::fixups::read_part_file(&path,&part).unwrap()).unwrap();
        let raw = "<?keep row?><c r=\"A16384\" t=\"n\"><v>016383.00</v></c>";
        let tagged = xml.replace("<c r=\"A16384\"><v>16383</v></c>",raw);
        assert_ne!(xml,tagged);
        crate::fixups::write_parts(&path,&[(part.clone(),tagged.into_bytes())]).unwrap();
        saved = crate::xlsx::load(&path).unwrap();
        saved.sheets[0].rich.get_mut(&(0,1)).unwrap()[1].italic = true;
        saved.sheets[0].rich_dirty = true;
        let receipt = crate::xlsx::save_atomic_with_receipt(&path,&saved).unwrap();
        assert!(receipt.used_cell_delta);
        receipt.adopt_document(&mut saved,&path,true);
        let reopened = crate::xlsx::load(&path).unwrap();
        assert!(reopened.sheets[0].rich[&(0,1)][1].italic);
        assert_eq!(reopened.sheets[0].tables[0].columns,["Sales"]);
        assert_eq!(reopened.sheets[0].notes[&CellRef::new(0,1)],note);
        assert_eq!(reopened.sheets[0].sparklines,[spark]);
        assert_eq!(reopened.sheets[0].textboxes[0].text,textbox.text);
        saved.sheets[0].rich.remove(&(0,1));
        saved.sheets[0].rich_dirty = true;
        crate::xlsx::save_atomic(&path,&saved).unwrap();
        let reopened = crate::xlsx::load(&path).unwrap();
        assert!(!reopened.sheets[0].rich.contains_key(&(0,1)));
        assert_eq!(reopened.sheets[0].get(0,1).as_deref(),Some(&text));
        let xml = String::from_utf8(crate::fixups::read_part_file(&path,&part).unwrap()).unwrap();
        assert!(xml.contains(raw));
    }

    #[test]
    fn streamed_dense_styles_inherited_defaults_filter_and_hyperlink_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("styles.xlsx");
        let mut wb = Workbook::new_blank();
        let bold = wb.styles.intern(crate::style::CellStyle { bold: true, ..Default::default() });
        let italic = wb.styles.intern(crate::style::CellStyle { italic: true, ..Default::default() });
        let quoted = wb.styles.intern(crate::style::CellStyle { strikethrough: true, ..Default::default() });
        wb.styles.set_quote(quoted, true);
        let sheet = wb.sheet_mut(0);
        for r in 5..16_389 {
            sheet.set(r, 3, Cell { value: Value::Number(r as f64), formula: None });
            sheet.set_style(r, 3, bold);
        }
        sheet.set_col_style(5, italic);
        sheet.set_row_style(7, italic);
        sheet.set(7, 4, Cell {value: Value::Number(123.), formula: None});
        sheet.set(10, 5, Cell { value: Value::Bool(true), formula: None });
        sheet.set(11, 5, Cell { value: Value::Bool(false), formula: None });
        sheet.set_style(11, 5, 0);
        sheet.set_style(12, 5, 0);
        sheet.set(14, 4, Cell { value: Value::Text("0123".into()), formula: None });
        sheet.set_style(14, 4, quoted);
        sheet.set_style(15, 4, quoted);
        sheet.filter = Some(crate::sort_filter::Filter::new(5, 3, 100, 3));
        sheet.hyperlinks.insert((9,3), crate::hyperlinks::Hyperlink { location: "Sheet1!D6".into(), ..Default::default() });
        assert!(supported(sheet));
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let back = crate::xlsx::load(&path).unwrap();
        let sh = back.sheet(0);
        assert!(back.styles.get(sh.style_of(16_388, 3)).bold);
        assert!(back.styles.get(sh.style_of(10, 5)).italic);
        assert!(back.styles.get(sh.style_of(7, 10)).italic);
        assert!(!back.styles.get(sh.style_of(11, 5)).italic);
        assert!(!back.styles.get(sh.style_of(12, 5)).italic);
        assert_eq!(sh.filter.as_ref().unwrap().range, (5, 3, 100, 3));
        assert_eq!(sh.hyperlinks[&(9,3)].location, "Sheet1!D6");
        let xml = String::from_utf8(crate::fixups::read_part_file(&path, "xl/worksheets/sheet1.xml").unwrap()).unwrap();
        assert!(xml.contains(r#"ref="D6:F16389""#), "incorrect used range");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let xf = |address| doc.descendants().find(|n| n.attribute("r") == Some(address)).unwrap().attribute("s").unwrap();
        assert_eq!(xf("E15"), xf("E16"));
        let row_xf = doc.descendants().find(|n| n.tag_name().name()=="row" && n.attribute("r")==Some("8")).unwrap().attribute("s");
        let col_xf = doc.descendants().find(|n| n.tag_name().name()=="col" && n.attribute("min")==Some("6")).unwrap().attribute("style");
        assert_eq!(Some(xf("E8")),row_xf);
        assert_eq!(Some(xf("F11")),col_xf);
        assert_ne!(xf("F11"),"0");
        assert!(back.styles.quote_of(sh.style_of(15, 4)));
    }

    #[test]
    fn streaming_keeps_sparse_xfs_formatted_blanks_and_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.xlsx");
        let mut wb = Workbook::new_blank();
        let bold = wb.styles.intern(crate::style::CellStyle { bold: true, ..Default::default() });
        let sh = &mut wb.sheets[0];
        for r in 0..16_384 {
            sh.set(r, 1, Cell { value: Value::Number(r as f64), formula: None });
        }
        sh.set(4, 25, Cell { value: Value::Number(26.0), formula: None });
        sh.set_style(4, 1, bold);
        sh.set_style(4, 0, bold);
        sh.set_style(17_000, 7, bold);
        sh.rows.set(4, 48.0);
        sh.cols.set(7, 160.0);
        sh.merges.push((3, 5, 3, 6));
        assert!(supported(sh));
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let back = crate::xlsx::load(&path).unwrap();
        let sh = &back.sheets[0];
        let xml = crate::fixups::read_part_file(&path, "xl/worksheets/sheet1.xml").unwrap();
        let doc = roxmltree::Document::parse(std::str::from_utf8(&xml).unwrap()).unwrap();
        let row = doc.descendants().find(|n| n.has_tag_name("row") && n.attribute("r") == Some("5")).unwrap();
        assert!(row.attribute("spans").is_none());
        assert_eq!(sh.value(4, 25), Value::Number(26.0));
        assert_eq!(sh.value(4, 1), Value::Number(4.0));
        assert_eq!(sh.value(16_383, 1), Value::Number(16_383.0));
        assert!(matches!(sh.value(4, 0), Value::Empty));
        for (r, c) in [(4, 1), (4, 0), (17_000, 7)] {
            assert!(back.styles.get(sh.style_of(r, c)).bold);
        }
        assert_eq!(sh.rows.size(4), 48.0);
        assert!((sh.cols.size(7) - 160.0).abs() < 1.0);
        assert!(sh.merges.contains(&(3, 5, 3, 6)));
    }

    #[test]
    fn streamed_cells_and_dom_sibling_survive_atomic_save_and_followup_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.xlsx");
        let mut wb = Workbook::new_blank();
        for r in 0..16_384 {
            wb.sheets[0].set(
                r,
                2,
                Cell {
                    value: Value::Number(f64::from(r)),
                    formula: None,
                },
            );
        }
        let text = Cell {
            value: Value::Text(" 空格 <&>\t\n".into()),
            formula: None,
        };
        let formula = Cell {
            value: Value::Number(3.0),
            formula: Some("=C4".into()),
        };
        wb.sheets[0].set(1, 3, text.clone());
        wb.sheets[0].set(4, 4, formula.clone());
        for (r, value, expr) in [
            (5, Value::Bool(true), None),
            (6, Value::Text(" cached <&> ".into()), Some("=D2")),
            (7, Value::Error(crate::formula::ErrKind::Div0), Some("=1/0")),
            (8, Value::Empty, Some("=A1")),
        ] {
            wb.sheets[0].set(r, 0, Cell { value, formula: expr.map(Into::into) });
        }
        let mut scenario = crate::scenarios::Scenario::new("Forecast");
        scenario.cells.push(((0, 2), "42".into()));
        wb.sheets[0].scenarios.push(scenario.clone());
        assert!(supported(&wb.sheets[0]));
        let mut other = Sheet::new("Mixed");
        other.set(5, 5, text.clone());
        wb.sheets.push(other);
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let part = &crate::patch::sheet_targets(&path).unwrap()["Sheet1"];
        let xml = String::from_utf8(crate::fixups::read_part_file(&path, part).unwrap()).unwrap();
        assert!(xml.contains("ref=\"A1:E16384\""));
        assert!(xml.contains("t=\"inlineStr\""));
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let children: Vec<_> = doc.root_element().children().filter(|n| n.is_element())
            .map(|n| n.tag_name().name()).collect();
        let position = |name| children.iter().position(|&child| child == name).unwrap();
        assert!(position("sheetData") < position("scenarios"));
        assert!(position("scenarios") < position("pageMargins"));
        let mut back = crate::xlsx::load(&path).unwrap();
        assert_eq!(back.sheets[0].scenarios, [scenario]);
        assert_eq!(back.sheets[0].get(16_383, 2), wb.sheets[0].get(16_383, 2));
        assert_eq!(back.sheets[0].get(1, 3).as_deref(), Some(&text));
        assert_eq!(back.sheets[0].get(4, 4).as_deref(), Some(&formula));
        for r in 5..=8 {
            assert_eq!(back.sheets[0].get(r, 0), wb.sheets[0].get(r, 0));
        }
        assert_eq!(back.sheets[1].get(5, 5).as_deref(), Some(&text));
        back.sheets[0].set(16_383, 2, formula.clone());
        crate::xlsx::save_atomic(&path, &back).unwrap();
        assert_eq!(
            crate::xlsx::load(&path).unwrap().sheets[0].get(16_383, 2).as_deref(),
            Some(&formula)
        );
    }

    #[test]
    fn parallel_multisheet_fresh_save_deduplicates_repeated_strings_into_sst() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multisheet-sst.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "DeptA".into();
        let mut s2 = Sheet::new("DeptB");
        for r in 0..16_384 {
            wb.sheets[0].set(
                r,
                0,
                Cell {
                    value: Value::Text(if r % 2 == 0 { "研发部 ".into() } else { "财务部<&>".into() }),
                    formula: None,
                },
            );
            s2.set(
                r,
                0,
                Cell {
                    value: Value::Text(if r % 3 == 0 { "研发部 ".into() } else { "运营部".into() }),
                    formula: None,
                },
            );
        }
        wb.sheets[0].set(
            0,
            1,
            Cell {
                value: Value::Text("唯一字符串".into()),
                formula: None,
            },
        );
        let mut dom_sibling = Sheet::new("SmallDom");
        dom_sibling.set(
            0,
            0,
            Cell {
                value: Value::Text("DOM专属字符串".into()),
                formula: None,
            },
        );
        wb.sheets.push(s2);
        wb.sheets.push(dom_sibling);
        crate::xlsx::save_atomic(&path, &wb).unwrap();

        let targets = crate::patch::sheet_targets(&path).unwrap();
        let xml_a = String::from_utf8(crate::fixups::read_part_file(&path, &targets["DeptA"]).unwrap()).unwrap();
        assert!(xml_a.contains("t=\"s\""), "repeated strings must use SST");
        assert!(xml_a.contains("t=\"inlineStr\""), "1-off unique string stays inline");

        let reopened = crate::xlsx::load(&path).unwrap();
        assert_eq!(reopened.sheets[0].value(0, 0), Value::Text("研发部 ".into()));
        assert_eq!(reopened.sheets[0].value(1, 0), Value::Text("财务部<&>".into()));
        assert_eq!(reopened.sheets[0].value(0, 1), Value::Text("唯一字符串".into()));
        assert_eq!(reopened.sheets[1].value(1, 0), Value::Text("运营部".into()));
        assert_eq!(reopened.sheets[2].value(0, 0), Value::Text("DOM专属字符串".into()));
    }

    #[test]
    fn parallel_rfc1951_deflate_crc_combine_and_xml_checker() {
        use std::io::Read;
        // 1. Verify crc32_combine across varied chunk sizes
        let payload: Vec<u8> = (0..120_000u32)
            .map(|i| ((i.wrapping_mul(31) ^ (i >> 7)) & 0xFF) as u8)
            .collect();
        let mut full_crc = flate2::Crc::new();
        full_crc.update(&payload);
        let expected_crc = full_crc.sum();

        for split in [0usize, 1, 7, 63, 4096, 65537, payload.len()] {
            let mut c1 = flate2::Crc::new();
            c1.update(&payload[..split]);
            let mut c2 = flate2::Crc::new();
            c2.update(&payload[split..]);
            let combined = crc32_combine(c1.sum(), c2.sum(), (payload.len() - split) as u64);
            assert_eq!(combined, expected_crc, "split={split}");
        }

        // 2. Verify multi-chunk raw RFC 1951 deflate + build_single_entry_zip roundtrip
        let chunks: Vec<&[u8]> = payload.chunks(29_999).collect();
        let mut comp_chunks = Vec::new();
        let mut total_crc = 0u32;
        let mut total_len = 0u64;
        for (i, ch) in chunks.iter().enumerate() {
            let (comp, crc) = deflate_raw_chunk(ch, 2, i + 1 == chunks.len()).unwrap();
            total_crc = if total_len == 0 {
                crc
            } else {
                crc32_combine(total_crc, crc, ch.len() as u64)
            };
            total_len += ch.len() as u64;
            comp_chunks.push(comp);
        }
        assert_eq!(total_crc, expected_crc);
        assert_eq!(total_len, payload.len() as u64);
        let mini = build_single_entry_zip("xl/worksheets/sheet1.xml", total_crc, total_len, &comp_chunks).unwrap();
        let mut za = ZipArchive::new(std::io::Cursor::new(mini)).unwrap();
        let mut out = Vec::new();
        za.by_index(0).unwrap().read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);

        // 3. Verify XmlCheckState accepts valid documents & rejects malformed ones
        let ok_xml = "\u{feff}<?xml version=\"1.0\"?><!-- c --><x:worksheet xmlns:x=\"u\"><x:sheetData><x:row r=\"1\"><x:c r=\"A1\"><x:v>42</x:v></x:c></x:row></x:sheetData></x:worksheet>";
        let mut st = XmlCheckState::new_document();
        for piece in ok_xml.as_bytes().chunks(7) {
            st.feed(piece);
        }
        assert!(st.finish_document(Some("worksheet")));
        assert!(!st.finish_document(Some("workbook")));

        for bad in [
            "<worksheet><row></worksheet>",
            "<worksheet/><extra/>",
            "text<worksheet/>",
            "<worksheet><c r=\"A1></worksheet>",
            "<worksheet><row><!-- unclosed</worksheet>",
        ] {
            let mut st = XmlCheckState::new_document();
            st.feed(bad.as_bytes());
            assert!(!st.finish_document(Some("worksheet")), "must reject: {bad}");
        }
    }

    #[test]
    fn direct_layout_metadata_and_parallel_sheet_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("direct-meta-parallel.xlsx");
        let mut wb = Workbook::new_blank();
        wb.calc_manual = true;
        wb.names.push(("Rate".into(), None, "=0.08".into()));
        let bold_sid = wb.styles.intern(crate::style::CellStyle {
            bold: true,
            fill: Some([230, 240, 255]),
            ..Default::default()
        });
        {
            let sh = &mut wb.sheets[0];
            sh.tab_color = Some([255, 128, 0]);
            sh.freeze = Some((2, 1));
            sh.cols.set(0, 140.0);
            sh.hidden_cols.insert(3);
            sh.rows.set(0, 36.0);
            sh.rows.set(500, 42.0);
            sh.hidden_rows.insert_range(10, 15);
            sh.hidden_rows_user.insert_range(10, 15);
            sh.row_levels.insert(5, 1);
            sh.col_levels.insert(2, 1);
            sh.merges.push((0, 0, 0, 1));
            sh.filter = Some(crate::sort_filter::Filter::new(1, 0, 100, 2));
            sh.page.landscape = true;
            sh.page.print_area = Some((0, 0, 50, 2));
            for r in 0..35_000u32 {
                sh.set(
                    r,
                    0,
                    Cell {
                        value: Value::Number(f64::from(r) * 1.25),
                        formula: None,
                    },
                );
                if r % 500 == 0 {
                    sh.set_style(r, 0, bold_sid);
                }
            }
        }
        let mut hidden_sh = crate::model::Sheet::new("HiddenTab");
        hidden_sh.hidden = true;
        hidden_sh.set(
            0,
            0,
            Cell {
                value: Value::Text("后台数据".into()),
                formula: None,
            },
        );
        wb.sheets.push(hidden_sh);

        assert!(
            can_write_direct(&wb, false),
            "workbook with layout/meta must qualify for write_direct"
        );
        crate::xlsx::save(&path, &wb).unwrap();

        let (back, diag) = crate::xlsx::load_diag(&path).unwrap();
        assert!(diag.is_complete());
        assert!(back.calc_manual);
        assert_eq!(back.names.len(), 1);
        let s0 = &back.sheets[0];
        assert_eq!(s0.tab_color, Some([255, 128, 0]));
        assert_eq!(s0.freeze, Some((2, 1)));
        assert!((s0.rows.size(0) - 36.0).abs() < 0.5);
        assert!((s0.rows.size(500) - 42.0).abs() < 0.5);
        assert!(s0.hidden_cols.contains(&3));
        assert!(s0.hidden_rows.is_empty() && s0.hidden_rows_user.contains(&12));
        assert_eq!(s0.merges, vec![(0, 0, 0, 1)]);
        assert!(s0.page.landscape);
        assert_eq!(s0.page.print_area, Some((0, 0, 50, 2)));
        assert!(back.styles.get(s0.style_of(500, 0)).bold);
        assert_eq!(s0.value(34_999, 0), Value::Number(34_999.0 * 1.25));
        assert!(back.sheets[1].hidden);
        assert_eq!(back.sheets[1].value(0, 0), Value::Text("后台数据".into()));
    }

    #[test]
    fn direct_save_then_delta_edit_and_encrypted_memory_hyperlink_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("direct-to-delta.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].freeze = Some((1, 0));
        wb.sheets[0].rows.set(0, 32.0);
        wb.sheets[0].cols.set(0, 128.0);
        for r in 0..2_000u32 {
            wb.sheets[0].set(
                r,
                0,
                Cell {
                    value: Value::Number(f64::from(r)),
                    formula: None,
                },
            );
        }
        crate::xlsx::save(&path, &wb).unwrap();

        let (mut loaded, _) = crate::xlsx::load_diag(&path).unwrap();
        loaded.sheets[0].set(
            5,
            0,
            Cell {
                value: Value::Number(999.5),
                formula: None,
            },
        );
        let receipt = crate::xlsx::save_atomic_with_receipt(&path, &loaded).unwrap();
        assert!(receipt.used_cell_delta, "1-cell edit after direct save must use delta");

        let (after_delta, diag) = crate::xlsx::load_diag(&path).unwrap();
        assert!(diag.is_complete());
        assert_eq!(after_delta.sheets[0].freeze, Some((1, 0)));
        assert!((after_delta.sheets[0].rows.size(0) - 32.0).abs() < 0.5);
        assert_eq!(after_delta.sheets[0].value(5, 0), Value::Number(999.5));
        assert_eq!(after_delta.sheets[0].value(1_999, 0), Value::Number(1_999.0));

        // Encrypted workbook with internal hyperlink roundtrip via in-memory Cursor loader
        let enc_path = dir.path().join("encrypted-internal-link.xlsx");
        let mut ewb = Workbook::new_blank();
        ewb.encryption = Some(crate::crypto::EncryptionSettings::new("pw123"));
        ewb.sheets[0].set(
            0,
            0,
            Cell {
                value: Value::Text("跳转".into()),
                formula: None,
            },
        );
        ewb.sheets[0].hyperlinks.insert(
            (0, 0),
            crate::hyperlinks::Hyperlink {
                location: "Sheet1!A10".into(),
                tooltip: "去第10行".into(),
                ..Default::default()
            },
        );
        ewb.sheets[0].hyperlinks_dirty = true;
        crate::xlsx::save(&enc_path, &ewb).unwrap();

        let (dec_wb, dec_diag) = crate::xlsx::load_with_password_diag(&enc_path, "pw123").unwrap();
        assert!(dec_diag.is_complete());
        let link = dec_wb.sheets[0].hyperlinks.get(&(0, 0)).expect("internal hyperlink preserved");
        assert_eq!(link.location, "Sheet1!A10");
    }
}
