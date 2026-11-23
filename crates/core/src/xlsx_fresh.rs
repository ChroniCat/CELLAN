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
    // A saved dynamic result needs its cell metadata and persisted spill caches
    // even when the workbook has only one formula.
    if (sheet.len() < 16_384 && sheet.spill_shapes.is_empty())
        || sheet.charts.iter().any(|c| !c.part.is_empty())
        || sheet.pictures.iter().any(|p| p.data.is_none())
    {
        return false;
    }
    sheet.rich.iter().all(|(&(r, c), runs)| {
        runs.is_empty()
            || sheet
                .get(r, c)
                .is_some_and(|cell| super::delta::supports_runs(cell, runs))
    }) && sheet.iter().all(|(key, cell)| {
        key.row < crate::sync::MAX_ROWS
            && key.col < crate::sync::MAX_COLS
            && cell
                .formula
                .as_ref()
                .is_none_or(|f| super::delta::safe_xml_text(f))
            && match &cell.value {
                Value::Number(n) => n.is_finite(),
                Value::Text(s) => super::delta::safe_xml_text(s),
                Value::Error(crate::formula::ErrKind::Spill) => {
                    sheet.spill_shapes.contains_key(&key)
                }
                _ => true,
            }
    })
}

fn write_value(
    out: &mut impl Write,
    sheet: &Sheet,
    arrays: &HashMap<CellRef, (u32, u32, u32, u32)>,
    key: CellRef,
    cell: &Cell,
    xf: Option<u16>,
) -> Result<()> {
    let reference = crate::model::cell_name(key.row, key.col);
    let mut start = format!("<c r=\"{reference}\"");
    if let Some(xf) = xf {
        start.push_str(&format!(" s=\"{xf}\""));
    }
    let shape = sheet.spill_shapes.get(&key);
    let rect = if let Some(&rect) = arrays.get(&key) {
        Some(rect)
    } else if let Some(&(h, w)) = shape {
        start.push_str(" cm=\"1\"");
        Some((
            key.row,
            key.col,
            key.row
                .checked_add(h.saturating_sub(1))
                .context("数组越界")?,
            key.col
                .checked_add(w.saturating_sub(1))
                .context("数组越界")?,
        ))
    } else {
        None
    };
    let definition = if let Some(rect) = rect {
        ensure!(
            (rect.0, rect.1) == (key.row, key.col)
                && rect.2 >= rect.0
                && rect.3 >= rect.1
                && rect.2 < crate::sync::MAX_ROWS
                && rect.3 < crate::sync::MAX_COLS,
            "无效的数组范围"
        );
        let formula = cell.formula.as_deref().context("缺少数组锚点公式")?;
        let formula = crate::add_fn_prefixes(formula.strip_prefix('=').unwrap_or(formula));
        let range = super::dynamic::reference(rect);
        Some(format!(
            "<f t=\"array\" ref=\"{range}\">{}</f>",
            quick_xml::escape::escape(&formula)
        ))
    } else {
        None
    };
    let xml = if cell.value == Value::Error(crate::formula::ErrKind::Spill) {
        format!(
            "{start} t=\"e\">{}<v>#SPILL!</v></c>",
            definition.context("溢出错误缺少锚点")?
        )
    } else {
        super::delta::cell_with_runs(
            start,
            cell,
            definition.as_deref(),
            sheet.rich.get(&(key.row, key.col)).map(Vec::as_slice),
        )
        .context("无效的工作表单元格")?
    };
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
    let (mut top, mut left, mut bottom, mut right) = sheet.used_bounds().context("流式工作表缺少单元格")?;
    let mut formats = std::collections::HashMap::from([(0u16, 0u16)]);
    let arrays: HashMap<_, _> = sheet.arrays.iter().copied().collect();
    ensure!(arrays.len()==sheet.arrays.len(),"重复的数组锚点");
    for stub in data.descendants().filter(|n| n.has_tag_name((namespace, "c"))) {
        let (r,c) = stub.attribute("r").and_then(crate::sparse::parse_ref).context("无效的模板单元格")?;
        top = top.min(r); left = left.min(c); bottom = bottom.max(r); right = right.max(c);
        if sheet.contains(r, c) && let Some(id) = sheet.explicit_style_of(r, c) {
            let xf = stub.attribute("s").map(str::parse::<u16>).transpose()?.unwrap_or(0);
            formats.insert(id, xf);
        }
    }
    let xf_for = |key: CellRef| -> Result<Option<u16>> {
        sheet.explicit_style_of(key.row, key.col).map(|id| formats.get(&id).copied()
            .context("模板缺少单元格样式")).transpose()
    };
    let first = crate::model::cell_name(top, left);
    let end = crate::model::cell_name(bottom, right);
    let reference = if first == end { first } else { format!("{first}:{end}") };
    let mut out = BufWriter::new(out);
    let dim = dimension.range_value();
    write!(out, "{}{reference}{}<sheetData>", &shell[..dim.start], &shell[dim.end..data.range().start])?;

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
                write_value(&mut out, sheet, &arrays, key, cell, xf_for(key)?)?;
            }
            if let Some((key, cell)) = cells.next_if(|(k, _)| k.row == r && k.col == sc) {
                let xf = stub.attribute("s").map(str::parse::<u16>).transpose()?;
                write_value(&mut out, sheet, &arrays, key, cell, xf.or(xf_for(key)?))?;
            } else if stub.attribute("s").is_none() && sheet.explicit_style_of(sr, sc) == Some(0) {
                write_value(&mut out, sheet, &arrays, CellRef::new(sr, sc), &Cell::default(), Some(0))?;
            } else {
                out.write_all(shell[stub.range()].as_bytes())?;
            }
        }
        while let Some((key, cell)) = cells.next_if(|(k, _)| k.row == r) {
            write_value(&mut out, sheet, &arrays, key, cell, xf_for(key)?)?;
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
    let dynamic = sheets.values().any(|s| {
        let fixed: std::collections::HashSet<_> = s.arrays.iter().map(|(k,_)| k).collect();
        s.spill_shapes.keys().any(|k| !fixed.contains(k))
    });
    ensure!(!dynamic || zip.index_for_name(super::dynamic::PART).is_none(),"新建模板的元数据部件已占用");
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
        } else if dynamic && matches!(part.as_str(), super::dynamic::RELS | super::dynamic::TYPES) {
            let mut xml = super::read_zip_text(&mut zip,&part)?;
            super::dynamic::declare(&mut xml,&part)?;
            out.start_file(part,SimpleFileOptions::default().compression_method(CompressionMethod::Deflated))?;
            out.write_all(xml.as_bytes())?;
        } else {
            out.raw_copy_file(zip.by_index_raw(i)?)?;
        }
    }
    ensure!(sheets.is_empty(), "缺少流式工作表部件");
    if dynamic {
        out.start_file(super::dynamic::PART,SimpleFileOptions::default().compression_method(CompressionMethod::Deflated))?;
        out.write_all(super::dynamic::METADATA.as_bytes())?;
    }
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
    fn streamed_drawings_and_both_array_types_roundtrip_with_their_cached_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drawing-arrays.xlsx");
        let marked = crate::textbox::insert_drawing_tag(
            "<s:worksheet xmlns:s=\"urn:w\"><s:sheetData><s:extLst/></s:sheetData><s:tableParts/></s:worksheet>",
            "rId7",
        );
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
        assert!(zip(super::super::dynamic::PART).contains("XLDAPR"));
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
        assert_eq!(reopened.sheets[0].get(0,1),Some(&text));
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
