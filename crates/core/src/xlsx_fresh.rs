//! Stream ordinary large worksheets into the sparse sheet parts of a newly
//! generated package. umya and the existing patches still own its metadata.
use crate::model::{Cell, CellRef, Sheet, Value, Workbook};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub(super) fn supported(sheet: &Sheet) -> bool {
    if sheet.len() < 16_384
        // The template contains only the formatted cells; keep the DOM path for
        // densely formatted sheets where the template would already be large.
        || sheet.styles.len() > sheet.len() / 8
        || !sheet.row_styles.is_empty()
        || !sheet.col_styles.is_empty()
        || !sheet.arrays.is_empty()
        || !sheet.spill_shapes.is_empty()
        || !sheet.spilled.is_empty()
        || !sheet.tables.is_empty()
        || !sheet.notes.is_empty()
        || !sheet.rich.is_empty()
        || !sheet.hyperlinks.is_empty()
        || sheet.filter.is_some()
        || !sheet.sparklines.is_empty()
        || !sheet.charts.is_empty()
        || !sheet.pictures.is_empty()
        || !sheet.textboxes.is_empty()
    {
        return false;
    }
    sheet.iter().all(|(key, cell)| {
        key.row < crate::sync::MAX_ROWS
            && key.col < crate::sync::MAX_COLS
            && cell
                .formula
                .as_ref()
                .is_none_or(|f| super::delta::safe_xml_text(f))
            && match &cell.value {
                Value::Number(n) => n.is_finite(),
                Value::Text(s) => super::delta::safe_xml_text(s),
                Value::Error(
                    crate::formula::ErrKind::Spill | crate::formula::ErrKind::Circular,
                ) => false,
                _ => true,
            }
    })
}

fn write_value(out: &mut impl Write, key: CellRef, cell: &Cell, xf: Option<u16>) -> Result<()> {
    let reference = crate::model::cell_name(key.row, key.col);
    let mut start = format!("<c r=\"{reference}\"");
    if let Some(xf) = xf {
        start.push_str(&format!(" s=\"{xf}\""));
    }
    let xml = super::delta::model_cell_xml(start, cell, None).context("无效的工作表单元格")?;
    out.write_all(xml.as_bytes())?;
    Ok(())
}

fn write_sheet(out: impl Write, shell: String, sheet: &Sheet) -> Result<()> {
    // Only the sparse, freshly generated template is parsed. Values are merged
    // from the ordered model iterator without materializing a second cell store.
    let doc = roxmltree::Document::parse(&shell)?;
    let root = doc.root_element();
    let namespace = root.tag_name().namespace().context("新建工作表缺少命名空间")?;
    let dimension = root.children().find(|n| n.has_tag_name((namespace, "dimension")))
        .and_then(|n| n.attribute_node("ref")).context("新建工作表的 dimension 无效")?;
    let data = root.children().find(|n| n.has_tag_name((namespace, "sheetData")))
        .context("新建工作表缺少 sheetData")?;
    let (_, _, bottom, right) = sheet.used_bounds().context("流式工作表缺少单元格")?;
    let (_, _, shell_bottom, shell_right) = crate::arr::parse_a1_range(dimension.value())
        .context("新建工作表的 dimension 坐标无效")?;
    let end = crate::model::cell_name(bottom.max(shell_bottom), right.max(shell_right));
    let mut out = BufWriter::new(out);
    let dim = dimension.range_value();
    write!(out, "{}A1:{end}{}<sheetData>", &shell[..dim.start], &shell[dim.end..data.range().start])?;

    let mut cells = sheet.iter().peekable();
    let mut rows = data.children().filter(|n| n.is_element()).peekable();
    while cells.peek().is_some() || rows.peek().is_some() {
        let model_row = cells.peek().map_or(u32::MAX, |(key, _)| key.row);
        let template_row = match rows.peek() {
            Some(n) if n.has_tag_name((namespace, "row")) => n.attribute("r")
                .and_then(|r| r.parse::<u32>().ok()?.checked_sub(1)).context("无效的模板行号")?,
            Some(_) => anyhow::bail!("工作表包含未知的行记录"),
            None => u32::MAX,
        };
        let r = model_row.min(template_row);
        let row = (r == template_row).then(|| rows.next().unwrap());
        if let Some(n) = row {
            let raw = &shell[n.range()];
            let mut reader = quick_xml::Reader::from_str(raw);
            reader.read_event()?;
            let tag = &raw[..reader.buffer_position() as usize];
            let start = tag.strip_suffix("/>").or_else(|| tag.strip_suffix('>')).context("无效的行标签")?;
            // The template's spans attribute counts only its formatted cells.
            // It is optional; the full row being streamed can extend past it.
            if let Some(attr) = n.attribute_node("spans") {
                let span = attr.range();
                let base = n.range().start;
                out.write_all(start[..span.start-base].as_bytes())?;
                out.write_all(start[span.end-base..].as_bytes())?;
            } else {
                out.write_all(start.as_bytes())?;
            }
            out.write_all(b">")?;
        } else {
            write!(out, "<row r=\"{}\">", r + 1)?;
        }

        for stub in row.into_iter().flat_map(|n| n.children().filter(|n| n.is_element())) {
            ensure!(stub.has_tag_name((namespace, "c")), "新建模板包含未知的行内容");
            let (sr, sc) = stub.attribute("r").and_then(crate::sparse::parse_ref)
                .context("无效的模板单元格")?;
            ensure!(sr == r, "模板单元格行号不一致");
            while let Some((key, cell)) = cells.next_if(|(k, _)| k.row == r && k.col < sc) {
                write_value(&mut out, key, cell, None)?;
            }
            if let Some((key, cell)) = cells.next_if(|(k, _)| k.row == r && k.col == sc) {
                let xf = stub.attribute("s").map(str::parse::<u16>).transpose()?;
                write_value(&mut out, key, cell, xf)?;
            } else {
                out.write_all(shell[stub.range()].as_bytes())?;
            }
        }
        while let Some((key, cell)) = cells.next_if(|(k, _)| k.row == r) {
            write_value(&mut out, key, cell, None)?;
        }
        out.write_all(b"</row>")?;
    }
    write!(out, "</sheetData>{}", &shell[data.range().end..])?;
    out.flush()?;
    Ok(())
}

pub(super) fn write(path: &Path, wb: &Workbook, selected: &[bool]) -> Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    let mut sheets = HashMap::new();
    for (sheet, &streamed) in wb.sheets.iter().zip(selected) {
        if streamed {
            let part = targets.get(&sheet.name).context("找不到流式工作表部件")?;
            ensure!(
                sheets.insert(part.as_str(), sheet).is_none(),
                "工作表部件重复"
            );
        }
    }
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (file, temp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
    let mut out = ZipWriter::new(BufWriter::new(file));
    for i in 0..zip.len() {
        let part = zip.name_for_index(i).context("缺少 ZIP 条目名")?.to_owned();
        if let Some(sheet) = sheets.remove(part.as_str()) {
            let shell = super::read_zip_text(&mut zip, &part)?;
            out.start_file(
                part,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .compression_level(Some(4)),
            )?;
            write_sheet(&mut out, shell, sheet)?;
        } else {
            out.raw_copy_file(zip.by_index_raw(i)?)?;
        }
    }
    ensure!(sheets.is_empty(), "缺少流式工作表部件");
    out.finish()?.flush()?;
    drop(zip);
    temp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

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
        assert_eq!(back.sheets[0].get(1, 3), Some(&text));
        assert_eq!(back.sheets[0].get(4, 4), Some(&formula));
        for r in 5..=8 {
            assert_eq!(back.sheets[0].get(r, 0), wb.sheets[0].get(r, 0));
        }
        assert_eq!(back.sheets[1].get(5, 5), Some(&text));
        back.sheets[0].set(16_383, 2, formula.clone());
        crate::xlsx::save_atomic(&path, &back).unwrap();
        assert_eq!(
            crate::xlsx::load(&path).unwrap().sheets[0].get(16_383, 2),
            Some(&formula)
        );
    }
}
