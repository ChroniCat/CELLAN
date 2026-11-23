//! Stream ordinary large worksheets into the empty sheet parts of a newly
//! generated package. umya and the existing patches still own its metadata.
use crate::model::{Sheet, Value, Workbook};
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
        || !sheet.styles.is_empty()
        || !sheet.row_styles.is_empty()
        || !sheet.col_styles.is_empty()
        || !sheet.rows.is_empty()
        || !sheet.cols.is_empty()
        || !sheet.hidden_rows.is_empty()
        || !sheet.hidden_rows_user.is_empty()
        || !sheet.hidden_cols.is_empty()
        || !sheet.row_levels.is_empty()
        || !sheet.col_levels.is_empty()
        || !sheet.arrays.is_empty()
        || !sheet.spill_shapes.is_empty()
        || !sheet.spilled.is_empty()
        || !sheet.tables.is_empty()
        || !sheet.merges.is_empty()
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

fn write_sheet(out: impl Write, shell: String, sheet: &Sheet) -> Result<()> {
    let mut out = BufWriter::new(out);
    let (_, _, bottom, right) = sheet.used_bounds().context("流式工作表缺少单元格")?;
    let end = crate::model::cell_name(bottom, right);
    let dimension = "<dimension ref=\"A1\"/>";
    let (head, after_dimension) = shell
        .split_once(dimension)
        .context("新建工作表的 dimension 无效")?;
    let (middle, tail) = after_dimension
        .split_once("<sheetData/>")
        .or_else(|| after_dimension.split_once("<sheetData></sheetData>"))
        .context("新建工作表的 sheetData 必须为空")?;
    write!(
        out,
        "{head}<dimension ref=\"A1:{end}\"/>{middle}<sheetData>"
    )?;
    let mut row = None;
    for (key, cell) in sheet.iter() {
        if row != Some(key.row) {
            if row.is_some() {
                out.write_all(b"</row>")?;
            }
            write!(out, "<row r=\"{}\">", key.row + 1)?;
            row = Some(key.row);
        }
        let reference = crate::model::cell_name(key.row, key.col);
        let xml = super::delta::model_cell_xml(format!("<c r=\"{reference}\""), cell, None)
            .context("无效的工作表单元格")?;
        out.write_all(xml.as_bytes())?;
    }
    if row.is_some() {
        out.write_all(b"</row>")?;
    }
    write!(out, "</sheetData>{tail}")?;
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
        assert!(supported(&wb.sheets[0]));
        let mut other = Sheet::new("Mixed");
        other.set(5, 5, text.clone());
        wb.sheets.push(other);
        crate::xlsx::save_atomic(&path, &wb).unwrap();
        let part = &crate::patch::sheet_targets(&path).unwrap()["Sheet1"];
        let xml = String::from_utf8(crate::fixups::read_part_file(&path, part).unwrap()).unwrap();
        assert!(xml.contains("ref=\"A1:E16384\""));
        assert!(xml.contains("t=\"inlineStr\""));
        let mut back = crate::xlsx::load(&path).unwrap();
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
