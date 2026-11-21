//! Patch ordinary cell edits against the exact worksheet captured during load
//! or save. Unsupported cell representations use the full serializer.
use crate::model::{Cell, CellRef, Sheet, Value, Workbook};
use crate::store::Store;
use anyhow::{Context, Result};
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::Write,
    path::Path,
    sync::Arc,
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

#[derive(Clone, Debug)]
pub(crate) struct SourceCells {
    store: Arc<Store>,
    signature: (u32, u64),
    strings: Option<(u32, u64)>,
}

fn signature(zip: &mut ZipArchive<File>, part: &str) -> Option<(u32, u64)> {
    let entry = zip.by_index_raw(zip.index_for_name(part)?).ok()?;
    Some((entry.crc32(), entry.size()))
}

impl SourceCells {
    pub(super) fn capture(zip: &mut ZipArchive<File>, part: &str, sheet: &Sheet) -> Option<Self> {
        if !sheet.spilled.is_empty() {
            return None;
        }
        Some(Self {
            store: Arc::new(sheet.cells().clone()),
            signature: signature(zip, part)?,
            strings: signature(zip, "xl/sharedStrings.xml"),
        })
    }
}

fn coordinates(sheet: &Sheet, base: &Store) -> Option<BTreeSet<CellRef>> {
    // Large structural or paste edits use the existing full-sheet writer.
    const LIMIT: usize = 16_384;
    let mut changes = BTreeSet::new();
    for key in sheet
        .cells()
        .changed_keys(base)
        .chain(base.changed_keys(sheet.cells()))
    {
        changes.insert(key);
        if changes.len() > LIMIT || sheet.rich.contains_key(&(key.row, key.col)) {
            return None;
        }
    }
    Some(changes)
}

fn attr<'a>(e: &'a BytesStart<'a>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| a.value.into_owned())
}

fn xml_text(text: &str) -> Option<String> {
    if text.chars().any(|c| c < ' ' && c != '\t' && c != '\n')
        || text.contains("_x")
        || text.contains('\u{fffe}')
        || text.contains('\u{ffff}')
    {
        return None;
    }
    Some(quick_xml::escape::escape(text).into_owned())
}

/// Leave Excel's formula definition intact on cache-only writes, including
/// shared formula anchors and derived cells. Preserve the explicit style ID.
fn replacement(
    key: CellRef,
    start: &BytesStart<'_>,
    body: &str,
    old: Option<&Cell>,
    new: Option<&Cell>,
    has_chain: bool,
) -> Option<String> {
    let mut style = None;
    for a in start.attributes() {
        let a = a.ok()?;
        match a.key.as_ref() {
            "r" | "t" => {}
            "s" => style = Some(a.value.parse::<u32>().ok()?),
            _ => return None,
        }
    }
    let mut xml = Reader::from_str(body);
    let mut definition = "";
    loop {
        let offset = xml.buffer_position() as usize;
        let event = xml.read_event().ok()?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let tag = e.name();
                if !matches!(tag.as_ref(), "f" | "v" | "is") {
                    return None;
                }
                if matches!(event, Event::Start(_)) {
                    xml.read_to_end(tag).ok()?;
                }
                if tag.as_ref() == "f" {
                    if old.and_then(|c| c.formula.as_ref()) != new.and_then(|c| c.formula.as_ref())
                        && e.attributes().next().is_some()
                    {
                        return None;
                    }
                    definition = &body[offset..xml.buffer_position() as usize];
                }
            }
            Event::Text(t) if t.as_ref().trim().is_empty() => {}
            Event::Eof => break,
            _ => return None,
        }
    }
    let reference = crate::model::cell_name(key.row, key.col);
    let mut out = format!("<c r=\"{reference}\"");
    if let Some(style) = style {
        out.push_str(&format!(" s=\"{style}\""));
    }
    let same_formula = old.and_then(|c| c.formula.as_ref()) == new.and_then(|c| c.formula.as_ref());
    if !same_formula && has_chain {
        return None;
    }
    let Some(cell) = new else {
        return Some(if style.is_some() {
            format!("{out}/>")
        } else {
            String::new()
        });
    };
    let (kind, value) = match &cell.value {
        Value::Empty => ("", String::new()),
        Value::Number(n) if n.is_finite() => ("", format!("<v>{n}</v>")),
        Value::Number(_) => return None,
        Value::Bool(b) => ("b", format!("<v>{}</v>", u8::from(*b))),
        Value::Error(crate::formula::ErrKind::Spill) => return None,
        Value::Error(e) => ("e", format!("<v>{}</v>", e.text())),
        Value::Text(s) if cell.formula.is_some() => ("str", format!("<v>{}</v>", xml_text(s)?)),
        Value::Text(s) => (
            "inlineStr",
            format!("<is><t xml:space=\"preserve\">{}</t></is>", xml_text(s)?),
        ),
    };
    if !kind.is_empty() {
        out.push_str(&format!(" t=\"{kind}\""));
    }
    out.push('>');
    if let Some(f) = &cell.formula {
        if same_formula {
            if definition.is_empty() {
                return None;
            }
            out.push_str(definition);
        } else {
            let f = crate::add_fn_prefixes(f.strip_prefix('=').unwrap_or(f));
            out.push_str(&format!("<f>{}</f>", xml_text(&f)?));
        }
    }
    out.push_str(&value);
    out.push_str("</c>");
    Some(out)
}

/// Keep unchanged XML as borrowed slices so even a large edited sheet only
/// needs one decompressed representation in memory.
struct Edit {
    from: usize,
    to: usize,
    text: String,
}

fn new_cell(key: CellRef, sheet: &Sheet, base: &Store, has_chain: bool) -> Option<String> {
    if base.get(key.row, key.col).is_some() {
        return None;
    }
    replacement(
        key,
        &BytesStart::new("c"),
        "",
        None,
        sheet.get(key.row, key.col),
        has_chain,
    )
}

fn insert_missing_rows(
    before: u32,
    sheet: &Sheet,
    base: &Store,
    keys: &mut BTreeSet<CellRef>,
    has_chain: bool,
) -> Option<String> {
    let mut xml = String::new();
    while let Some(&key) = keys.first().filter(|key| key.row < before) {
        xml.push_str(&format!("<row r=\"{}\">", key.row + 1));
        while let Some(cell) = keys.first().copied().filter(|cell| cell.row == key.row) {
            xml.push_str(&new_cell(cell, sheet, base, has_chain)?);
            keys.pop_first();
        }
        xml.push_str("</row>");
    }
    Some(xml)
}

fn expanded_tag(tag: &BytesStart<'_>, omit: &str) -> Option<String> {
    let mut root = BytesStart::new(tag.name().as_ref().to_owned());
    for a in tag.attributes() {
        let a = a.ok()?;
        if a.key.as_ref() != omit {
            root.push_attribute(a);
        }
    }
    let mut w = quick_xml::Writer::new(Vec::new());
    w.write_event(Event::Start(root)).ok()?;
    String::from_utf8(w.into_inner()).ok()
}

fn sheet_edits(
    xml: &str,
    sheet: &Sheet,
    base: &Store,
    mut keys: BTreeSet<CellRef>,
    has_chain: bool,
    changed_sst: &mut bool,
) -> Option<Vec<Edit>> {
    let mut reader = Reader::from_str(xml);
    let mut edits = Vec::new();
    let mut in_data = false;
    let mut row = None;
    let mut last_row = None;
    let additions: Vec<_> = keys
        .iter()
        .filter(|k| base.get(k.row, k.col).is_none())
        .copied()
        .collect();
    if additions
        .iter()
        .any(|k| k.row >= 1_048_576 || k.col >= 16_384)
    {
        return None;
    }
    loop {
        let from = reader.buffer_position() as usize;
        let event = reader.read_event().ok()?;
        let to = reader.buffer_position() as usize;
        match event {
            Event::Empty(ref e) | Event::Start(ref e)
                if e.local_name().as_ref() == "dimension" && !additions.is_empty() =>
            {
                let bounds = crate::arr::parse_a1_range(&attr(&e, "ref")?)?;
                let expanded = additions.iter().fold(bounds, |(r1, c1, r2, c2), k| {
                    (r1.min(k.row), c1.min(k.col), r2.max(k.row), c2.max(k.col))
                });
                if expanded != bounds {
                    let mut text = expanded_tag(e, "ref")?;
                    text.pop();
                    let end = if matches!(event, Event::Empty(_)) {
                        "/>"
                    } else {
                        ">"
                    };
                    text.push_str(&format!(" ref=\"{}\"{end}", crate::arr::a1_range(expanded)));
                    edits.push(Edit { from, to, text });
                }
            }
            Event::Start(e) if e.name().as_ref() == "sheetData" => in_data = true,
            Event::Empty(e) if e.name().as_ref() == "sheetData" => {
                let mut text = expanded_tag(&e, "")?;
                text.push_str(&insert_missing_rows(
                    u32::MAX,
                    sheet,
                    base,
                    &mut keys,
                    has_chain,
                )?);
                text.push_str("</sheetData>");
                edits.push(Edit { from, to, text });
                break;
            }
            Event::End(e) if e.name().as_ref() == "sheetData" => {
                let text = insert_missing_rows(u32::MAX, sheet, base, &mut keys, has_chain)?;
                if !text.is_empty() {
                    edits.push(Edit {
                        from,
                        to: from,
                        text,
                    });
                }
                break;
            }
            Event::Start(ref e) | Event::Empty(ref e) if in_data && e.name().as_ref() == "row" => {
                let r = attr(e, "r")?.parse::<u32>().ok()?.checked_sub(1)?;
                if last_row.is_some_and(|prev| prev >= r) {
                    return None;
                }
                last_row = Some(r);
                let text = insert_missing_rows(r, sheet, base, &mut keys, has_chain)?;
                if !text.is_empty() {
                    edits.push(Edit {
                        from,
                        to: from,
                        text,
                    });
                }
                if keys.first().is_none_or(|key| key.row != r) {
                    if matches!(event, Event::Start(_)) {
                        reader.read_to_end(e.name()).ok()?;
                    }
                    continue;
                }
                if matches!(event, Event::Empty(_)) {
                    let mut text = expanded_tag(e, "spans")?;
                    while let Some(k) = keys.first().copied().filter(|k| k.row == r) {
                        text.push_str(&new_cell(k, sheet, base, has_chain)?);
                        keys.pop_first();
                    }
                    text.push_str("</row>");
                    edits.push(Edit { from, to, text });
                } else {
                    if attr(e, "spans").is_some() {
                        edits.push(Edit {
                            from,
                            to,
                            text: expanded_tag(e, "spans")?,
                        });
                    }
                    row = Some(r);
                }
            }
            Event::End(e) if row.is_some() && e.name().as_ref() == "row" => {
                let mut text = String::new();
                while let Some(k) = keys.first().copied().filter(|k| Some(k.row) == row) {
                    text.push_str(&new_cell(k, sheet, base, has_chain)?);
                    keys.pop_first();
                }
                if !text.is_empty() {
                    edits.push(Edit {
                        from,
                        to: from,
                        text,
                    });
                }
                row = None;
            }
            Event::Start(ref e) | Event::Empty(ref e)
                if row.is_some() && e.name().as_ref() == "c" =>
            {
                let (r, c) = crate::sparse::parse_ref(&attr(e, "r")?)?;
                if Some(r) != row {
                    return None;
                }
                let mut text = String::new();
                let key = CellRef::new(r, c);
                while let Some(k) = keys.first().copied().filter(|k| *k < key) {
                    text.push_str(&new_cell(k, sheet, base, has_chain)?);
                    keys.pop_first();
                }
                if !text.is_empty() {
                    edits.push(Edit {
                        from,
                        to: from,
                        text,
                    });
                }
                let body = if matches!(event, Event::Start(_)) {
                    let range = reader.read_to_end(e.name()).ok()?;
                    &xml[range.start as usize..range.end as usize]
                } else {
                    ""
                };
                if keys.remove(&key) {
                    let text =
                        replacement(key, e, body, base.get(r, c), sheet.get(r, c), has_chain)?;
                    *changed_sst |= attr(e, "t").as_deref() == Some("s");
                    edits.push(Edit {
                        from,
                        to: reader.buffer_position() as usize,
                        text,
                    });
                }
            }
            // Unknown row/sheetData children have their own ordering rules.
            Event::Start(_) | Event::Empty(_) if in_data => return None,
            Event::Eof => break,
            _ => {}
        }
        if keys.is_empty() {
            break;
        }
    }
    keys.is_empty().then_some(edits)
}

fn write_edits(out: &mut ZipWriter<File>, part: &str, xml: &str, edits: &[Edit]) -> Result<()> {
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(1));
    out.start_file(part, opts)?;
    let mut copied = 0;
    for e in edits {
        out.write_all(&xml.as_bytes()[copied..e.from])?;
        out.write_all(e.text.as_bytes())?;
        copied = e.to;
    }
    out.write_all(&xml.as_bytes()[copied..])?;
    Ok(())
}

pub(super) fn write_if_supported(base: &Path, target: &Path, wb: &Workbook) -> Result<bool> {
    let mut zip = ZipArchive::new(File::open(base)?)?;
    let mut sheets = HashMap::new();
    let sst = signature(&mut zip, "xl/sharedStrings.xml");
    for sheet in &wb.sheets {
        if sheet.styles_dirty
            || sheet.row_styles_dirty
            || sheet.col_styles_dirty
            || sheet.merges_dirty
            || sheet.notes_dirty
            || !sheet.style_edits.is_empty()
        {
            return Ok(false);
        }
        if !sheet.cell_data_needs_save() {
            continue;
        }
        let (Some(part), Some(source)) = (&sheet.source_part, &sheet.source_cells) else {
            return Ok(false);
        };
        if !sheet.spilled.is_empty()
            || !sheet.arrays.is_empty()
            || sheet.rich_dirty
            || signature(&mut zip, part) != Some(source.signature)
            || source.strings != sst
        {
            return Ok(false);
        }
        let Some(keys) = coordinates(sheet, &source.store) else {
            return Ok(false);
        };
        if !keys.is_empty() {
            sheets.insert(part.clone(), (sheet, &source.store, keys));
        }
    }
    let tmp = tempfile::NamedTempFile::new_in(
        target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    let (file, tmp) = tmp.into_parts();
    let mut out = ZipWriter::new(file);
    let has_chain = zip.index_for_name("xl/calcChain.xml").is_some();
    let mut changed_sst = false;
    for index in 0..zip.len() {
        let part = zip
            .name_for_index(index)
            .context("缺少 ZIP 条目名")?
            .to_owned();
        if part == "xl/sharedStrings.xml" {
            continue;
        }
        if let Some((sheet, store, keys)) = sheets.remove(&part) {
            let xml = super::read_zip_text(&mut zip, &part)?;
            let Some(edits) = sheet_edits(&xml, sheet, store, keys, has_chain, &mut changed_sst)
            else {
                return Ok(false);
            };
            write_edits(&mut out, &part, &xml, &edits)?;
        } else {
            out.raw_copy_file(zip.by_index_raw(index)?)?;
        }
    }
    if !sheets.is_empty() {
        return Ok(false);
    }
    if let Some(i) = zip.index_for_name("xl/sharedStrings.xml") {
        if changed_sst {
            // Some indexed references became inline strings. Both counters are
            // optional; preserve indices, rich runs and every original <si>.
            let xml = super::read_zip_text(&mut zip, "xl/sharedStrings.xml")?;
            let mut reader = Reader::from_str(&xml);
            let edit = loop {
                let from = reader.buffer_position() as usize;
                match reader.read_event()? {
                    Event::Start(e) if e.local_name().as_ref() == "sst" => {
                        let mut root = BytesStart::new(e.name().as_ref().to_owned());
                        for a in e.attributes() {
                            let a = a?;
                            if !matches!(a.key.as_ref(), "count" | "uniqueCount") {
                                root.push_attribute(a);
                            }
                        }
                        let mut w = quick_xml::Writer::new(Vec::new());
                        w.write_event(Event::Start(root))?;
                        break Edit {
                            from,
                            to: reader.buffer_position() as usize,
                            text: String::from_utf8(w.into_inner())?,
                        };
                    }
                    Event::Eof => return Ok(false),
                    _ => {}
                }
            };
            write_edits(&mut out, "xl/sharedStrings.xml", &xml, &[edit])?;
        } else {
            out.raw_copy_file(zip.by_index_raw(i)?)?;
        }
    }
    out.finish()?;
    drop(zip);
    tmp.persist(target)?;
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::xlsx::{load_diag, read_part, save, save_atomic_with_receipt};

    fn fixture(rows: &str, dimension: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.xlsx");
        save(&path, &Workbook::new_blank()).unwrap();
        let part = crate::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
        let xml = format!(
            "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">{dimension}<sheetData>{rows}</sheetData></worksheet>"
        );
        crate::fixups::write_parts(&path, &[(part.clone(), xml.into_bytes())]).unwrap();
        (dir, path, part)
    }

    fn num(v: f64) -> Cell {
        Cell {
            value: Value::Number(v),
            formula: None,
        }
    }

    #[test]
    fn stream_missing_dimension_and_expand_shared_formulas() {
        let rows = r#"<row r="1"><c r="A1"><f t="shared" si="2" ref="A1:A2">IF(B1&gt;0,$C$1,0)+'A1'!B1+T[B2]</f><v>12</v></c></row><row r="2"><c r="A2"><f t="shared" si="2"/><v>4</v></c></row><row r="1048576"><c r="XFD1048576"><v>42</v></c></row>"#;
        let (_dir, path, _) = fixture(rows, "");
        let (wb, diag) = load_diag(&path).unwrap();
        assert!(diag.is_complete(), "{:?}", diag.problems);
        let sheet = &wb.sheets[0];
        assert_eq!(sheet.len(), 3);
        assert_eq!(sheet.value(1_048_575, 16_383), Value::Number(42.0));
        assert_eq!(
            sheet.get(1, 0).unwrap().formula.as_deref(),
            Some("=IF(B2>0,$C$1,0)+'A1'!B2+T[B2]")
        );
    }

    #[test]
    fn edits_existing_values_and_inserts_missing_rows_without_rebuilding_neighbors() {
        let kept = r#"<c r="D2" s="0" t="n"><v>03.00</v></c>"#;
        let rows = format!(
            r#"<row r="2" spans="1:4"><c r="A2" s="0"><v>1</v></c>{kept}</row><row r="5"/>"#
        );
        let (dir, path, part) = fixture(&rows, r#"<dimension ref="A2:D2"></dimension>"#);
        let (mut wb, diag) = load_diag(&path).unwrap();
        assert!(diag.is_complete());
        let sheet = &mut wb.sheets[0];
        sheet.remove(1, 0);
        sheet.set(0, 1, num(5.0));
        sheet.set(
            1,
            2,
            Cell {
                value: Value::Text(" 甲<&乙 ".into()),
                formula: None,
            },
        );
        sheet.set(4, 0, num(7.0));
        sheet.set(5, 0, num(8.0));
        let copy = dir.path().join("edited.xlsx");
        assert!(write_if_supported(&path, &copy, &wb).unwrap());
        let receipt = save_atomic_with_receipt(&copy, &wb).unwrap();
        receipt.adopt_document(&mut wb, &copy, true);
        let xml = String::from_utf8(read_part(&copy, &part).unwrap()).unwrap();
        assert!(xml.contains(kept) && xml.contains(r#"<c r="A2" s="0"/>"#));
        assert!(xml.contains(r#"ref="A1:D6""#));
        let (loaded, diag) = load_diag(&copy).unwrap();
        assert!(diag.is_complete(), "{:?}", diag.problems);
        for (r, c) in [(1, 0), (0, 1), (1, 2), (1, 3), (4, 0), (5, 0)] {
            assert_eq!(loaded.sheets[0].value(r, c), wb.sheets[0].value(r, c));
        }

        wb.sheets[0].set(0, 1, num(15.0));
        let next = dir.path().join("next.xlsx");
        assert!(write_if_supported(&copy, &next, &wb).unwrap());
        assert!(
            !write_if_supported(&path, &next, &wb).unwrap(),
            "an old snapshot cannot patch a different XML part"
        );
    }

    #[test]
    fn clearing_a_value_preserves_the_explicit_style_during_full_writeback() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("formatted.xlsx");
        let mut wb = Workbook::new_blank();
        let bold = wb.styles.intern(crate::style::CellStyle {
            bold: true,
            ..Default::default()
        });
        wb.sheets[0].set(0, 0, num(10.0));
        wb.sheets[0].set_style(0, 0, bold);
        save(&file, &wb).unwrap();
        let (mut wb, _) = load_diag(&file).unwrap();
        assert!(wb.styles.get(wb.sheets[0].style_of(0, 0)).bold, "baseline style missing");
        wb.sheets[0].remove(0, 0);
        wb.sheets[0].merge_range(2, 1, 2, 2);
        let out = dir.path().join("cleared.xlsx");
        assert!(!write_if_supported(&file, &out, &wb).unwrap());
        save(&out, &wb).unwrap();
        let (loaded, diag) = load_diag(&out).unwrap();
        assert!(diag.is_complete());
        let sheet = &loaded.sheets[0];
        assert_eq!(sheet.value(0, 0), Value::Empty);
        assert!(loaded.styles.get(sheet.style_of(0, 0)).bold, "sheet XML: {}",
            String::from_utf8(read_part(&out, sheet.source_part.as_deref().unwrap()).unwrap()).unwrap());
    }

    #[test]
    fn editing_an_indexed_string_retains_other_indices_and_rich_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("strings.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(
            0,
            0,
            Cell {
                value: Value::Text("first".into()),
                formula: None,
            },
        );
        wb.sheets[0].set(
            0,
            1,
            Cell {
                value: Value::Text("second".into()),
                formula: None,
            },
        );
        save(&path, &wb).unwrap();
        let sst = String::from_utf8(read_part(&path, "xl/sharedStrings.xml").unwrap()).unwrap();
        let rich = "<r><rPr><b/></rPr><t>second</t></r>";
        let xml = sst.replace("<t>second</t>", rich);
        assert!(xml.contains(rich));
        crate::fixups::write_parts(&path, &[("xl/sharedStrings.xml".into(), xml.into_bytes())])
            .unwrap();
        let (mut wb, _) = load_diag(&path).unwrap();
        wb.sheets[0].set(0, 0, num(5.0));
        let out = dir.path().join("indexed.xlsx");
        assert!(write_if_supported(&path, &out, &wb).unwrap());
        let xml = String::from_utf8(read_part(&out, "xl/sharedStrings.xml").unwrap()).unwrap();
        assert!(xml.contains(rich) && !xml.contains("uniqueCount="));
        let (loaded, diag) = load_diag(&out).unwrap();
        assert!(diag.is_complete());
        assert_eq!(loaded.sheets[0].value(0, 1), Value::Text("second".into()));
        assert!(loaded.sheets[0].rich[&(0, 1)][0].bold);
    }

    #[test]
    fn changing_a_shared_formula_master_uses_the_full_serializer() {
        let rows = r#"<row r="1"><c r="A1"><f t="shared" si="0" ref="A1:A2">B1+1</f><v>2</v></c></row><row r="2"><c r="A2"><f t="shared" si="0"/><v>3</v></c></row>"#;
        let (dir, path, part) = fixture(rows, "");
        let (mut wb, diag) = load_diag(&path).unwrap();
        assert!(diag.is_complete());
        wb.sheets[0].get_mut(0, 0).unwrap().value = Value::Number(11.0);
        let target = dir.path().join("cache.xlsx");
        assert!(write_if_supported(&path, &target, &wb).unwrap());
        let xml = String::from_utf8(read_part(&target, &part).unwrap()).unwrap();
        assert!(xml.contains(r#"<f t="shared" si="0" ref="A1:A2">B1+1</f><v>11</v>"#));
        wb.sheets[0].set(0, 0, num(4.0));
        assert!(!write_if_supported(&path, &target, &wb).unwrap());
        save(&target, &wb).unwrap();
        assert_eq!(
            load_diag(&target).unwrap().0.sheets[0].value(0, 0),
            Value::Number(4.0)
        );
    }
}
