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

const SST_PART: &str = "xl/sharedStrings.xml";
const SST_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings";
const SST_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml";

#[inline]
fn format_a1_buf(row: u32, col: u32, buf: &mut [u8; 16]) -> &str {
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

struct FreshSst<'a> {
    indices: HashMap<&'a str, u32>,
    xml: Vec<u8>,
    existed_in_zip: bool,
}

impl<'a> FreshSst<'a> {
    fn build(zip: &mut ZipArchive<File>, ordered_sheets: &[(&str, &'a Sheet)]) -> Result<Option<Self>> {
        let mut counts: HashMap<&'a str, u32> = HashMap::new();
        let mut repeated_in_order: Vec<&'a str> = Vec::new();
        for &(_, sheet) in ordered_sheets {
            for (key, cell) in sheet.iter() {
                if sheet.spilled.contains(&key) || cell.formula.is_some() {
                    continue;
                }
                if sheet
                    .rich
                    .get(&(key.row, key.col))
                    .is_some_and(|runs| !runs.is_empty())
                {
                    continue;
                }
                if let Value::Text(s) = &cell.value {
                    let text: &str = s.as_ref();
                    let entry = counts.entry(text).or_insert(0);
                    *entry += 1;
                    if *entry == 2 {
                        repeated_in_order.push(text);
                    }
                }
            }
        }
        if repeated_in_order.is_empty() {
            return Ok(None);
        }

        let existed_in_zip = zip.index_for_name(SST_PART).is_some();
        let mut xml = String::new();
        let mut next_idx: u32 = 0;
        if existed_in_zip {
            let existing = super::read_zip_text(zip, SST_PART)?;
            let doc = roxmltree::Document::parse(&existing)?;
            let root = doc.root_element();
            ensure!(root.tag_name().name() == "sst", "共享字符串根节点无效");
            next_idx = u32::try_from(
                root.children()
                    .filter(|n| n.is_element() && n.tag_name().name() == "si")
                    .count(),
            )?;
            let mut reader = quick_xml::Reader::from_str(&existing);
            let mut buf = Vec::new();
            let (root_end, empty_root, clean_open) = loop {
                buf.clear();
                let ev = reader.read_event_into(&mut buf)?;
                match ev {
                    quick_xml::events::Event::Start(ref e) | quick_xml::events::Event::Empty(ref e)
                        if e.local_name().as_ref() == "sst" =>
                    {
                        let empty_root = matches!(ev, quick_xml::events::Event::Empty(_));
                        let root_end = reader.buffer_position() as usize;
                        let mut tag = quick_xml::events::BytesStart::new(e.name().as_ref().to_owned());
                        for a in e.attributes() {
                            let a = a?;
                            if !matches!(a.key.as_ref(), "count" | "uniqueCount") {
                                tag.push_attribute(a);
                            }
                        }
                        let mut w = quick_xml::Writer::new(Vec::new());
                        w.write_event(quick_xml::events::Event::Start(tag))?;
                        break (root_end, empty_root, String::from_utf8(w.into_inner())?);
                    }
                    quick_xml::events::Event::Eof => anyhow::bail!("共享字符串缺少根标签"),
                    _ => {}
                }
            };
            xml.reserve(existing.len() + repeated_in_order.len() * 32);
            xml.push_str(&clean_open);
            if !empty_root {
                let close_pos = existing[..root.range().end]
                    .rfind("</")
                    .context("共享字符串缺少闭合标签")?;
                xml.push_str(&existing[root_end..close_pos]);
            }
        } else {
            xml.reserve(160 + repeated_in_order.len() * 32);
            xml.push_str(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
            );
        }

        let mut indices = HashMap::with_capacity(repeated_in_order.len());
        for s in repeated_in_order {
            let idx = next_idx;
            next_idx = next_idx.checked_add(1).context("共享字符串索引溢出")?;
            indices.insert(s, idx);
            let preserve = s.starts_with(char::is_whitespace)
                || s.ends_with(char::is_whitespace)
                || s.bytes().any(|b| matches!(b, b'\t' | b'\r' | b'\n'));
            if preserve {
                xml.push_str("<si><t xml:space=\"preserve\">");
            } else {
                xml.push_str("<si><t>");
            }
            xml.push_str(&quick_xml::escape::escape(s));
            xml.push_str("</t></si>");
        }
        xml.push_str("</sst>");
        Ok(Some(Self {
            indices,
            xml: xml.into_bytes(),
            existed_in_zip,
        }))
    }
}

#[allow(clippy::too_many_arguments)]
fn write_value(
    out: &mut impl Write,
    sheet: &Sheet,
    arrays: &HashMap<CellRef, (u32, u32, u32, u32)>,
    sst: Option<&HashMap<&str, u32>>,
    key: CellRef,
    cell: &Cell,
    xf: Option<u16>,
) -> Result<()> {
    let mut a1_buf = [0u8; 16];
    let reference = format_a1_buf(key.row, key.col, &mut a1_buf);
    let runs = sheet.rich.get(&(key.row, key.col)).map(Vec::as_slice);
    let has_runs = runs.is_some_and(|r| !r.is_empty());

    // Zero-allocation fast path for ordinary scalar numbers, booleans, blanks, and repeated SST strings.
    if cell.formula.is_none() && !has_runs && !sheet.spill_shapes.contains_key(&key) {
        match &cell.value {
            Value::Empty => {
                if let Some(xf) = xf {
                    write!(out, "<c r=\"{reference}\" s=\"{xf}\"/>")?;
                }
                return Ok(());
            }
            Value::Number(n) if n.is_finite() => {
                let n = if *n == 0.0 { 0.0 } else { *n };
                if let Some(xf) = xf {
                    write!(out, "<c r=\"{reference}\" s=\"{xf}\"><v>{n}</v></c>")?;
                } else {
                    write!(out, "<c r=\"{reference}\"><v>{n}</v></c>")?;
                }
                return Ok(());
            }
            Value::Bool(b) => {
                let v = u8::from(*b);
                if let Some(xf) = xf {
                    write!(out, "<c r=\"{reference}\" s=\"{xf}\" t=\"b\"><v>{v}</v></c>")?;
                } else {
                    write!(out, "<c r=\"{reference}\" t=\"b\"><v>{v}</v></c>")?;
                }
                return Ok(());
            }
            Value::Text(s) => {
                let text: &str = s.as_ref();
                if let Some(&sst_idx) = sst.and_then(|m| m.get(text)) {
                    if let Some(xf) = xf {
                        write!(out, "<c r=\"{reference}\" s=\"{xf}\" t=\"s\"><v>{sst_idx}</v></c>")?;
                    } else {
                        write!(out, "<c r=\"{reference}\" t=\"s\"><v>{sst_idx}</v></c>")?;
                    }
                    return Ok(());
                }
            }
            _ => {}
        }
    }

    let mut start = format!("<c r=\"{reference}\"");
    if let Some(xf) = xf {
        start.push_str(&format!(" s=\"{xf}\""));
    }
    let shape = sheet.spill_shapes.get(&key);
    let rect = if cell.formula.is_none() {
        None // A replaced anchor is now an ordinary literal.
    } else if let Some(&rect) = arrays.get(&key) {
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
            runs,
        )
        .context("无效的工作表单元格")?
    };
    out.write_all(xml.as_bytes())?;
    Ok(())
}

fn write_sheet(
    out: impl Write,
    shell: &str,
    sheet: &Sheet,
    sst: Option<&HashMap<&str, u32>>,
) -> Result<()> {
    // Only the sparse, freshly generated template is parsed. Values are merged
    // from the ordered model iterator without materializing a second cell store.
    let doc = roxmltree::Document::parse(shell)?;
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
    // The row/column dimensions in the sparse template contain the actual
    // package XF, which can differ from the model's interned style number.
    for row in data.children().filter(|n| n.has_tag_name((namespace,"row"))) {
        if let (Some(r), Some(raw)) = (row.attribute("r"),row.attribute("s"))
            && let Some(id) = r.parse::<u32>()?.checked_sub(1).and_then(|i| sheet.row_styles.get(&i)) {
            formats.insert(*id,raw.parse()?);
        }
    }
    for col in root.children().filter(|n| n.has_tag_name((namespace,"cols")))
        .flat_map(|cols| cols.children().filter(|n| n.has_tag_name((namespace,"col")))) {
        if let (Some(first),Some(last),Some(raw)) = (col.attribute("min"), col.attribute("max"), col.attribute("style")) {
            let xf = raw.parse()?;
            for c in first.parse::<u32>()?..=last.parse::<u32>()?.min(crate::sync::MAX_COLS) {
                if let Some(id) = c.checked_sub(1).and_then(|c| sheet.col_styles.get(&c)) { formats.insert(*id,xf); }
            }
        }
    }
    let xf_for = |key: CellRef| -> Result<Option<u16>> {
        sheet.explicit_style_of(key.row, key.col)
            .or_else(|| sheet.row_styles.get(&key.row).or_else(||sheet.col_styles.get(&key.col)).copied())
            .map(|id| formats.get(&id).copied().context("模板缺少单元格样式")).transpose()
    };
    let first = crate::model::cell_name(top, left);
    let end = crate::model::cell_name(bottom, right);
    let reference = if first == end { first } else { format!("{first}:{end}") };
    let mut out = BufWriter::with_capacity(256 * 1024, out);
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
                write_value(&mut out, sheet, &arrays, sst, key, cell, xf_for(key)?)?;
            }
            if let Some((key, cell)) = cells.next_if(|(k, _)| k.row == r && k.col == sc) {
                let xf = stub.attribute("s").map(str::parse::<u16>).transpose()?;
                write_value(&mut out, sheet, &arrays, sst, key, cell, xf.or(xf_for(key)?))?;
            } else if stub.attribute("s").is_none() && sheet.explicit_style_of(sr, sc) == Some(0) {
                write_value(&mut out, sheet, &arrays, sst, CellRef::new(sr, sc), &Cell::default(), Some(0))?;
            } else {
                out.write_all(shell[stub.range()].as_bytes())?;
            }
        }
        while let Some((key, cell)) = cells.next_if(|(k, _)| k.row == r) {
            write_value(&mut out, sheet, &arrays, sst, key, cell, xf_for(key)?)?;
        }
        out.write_all(b"</row>")?;
    }
    write!(out, "</sheetData>{}", &shell[data.range().end..])?;
    out.flush()?;
    Ok(())
}

pub(super) fn has_dynamic(sheet: &Sheet) -> bool {
    let fixed: std::collections::HashSet<_> = sheet.arrays.iter().map(|(k,_)| k).collect();
    sheet.spill_shapes.keys().any(|k| !fixed.contains(k)
        && sheet.get(k.row,k.col).is_some_and(|c| c.formula.is_some()))
}

pub(super) fn write(path: &Path, wb: &Workbook, selected: &[bool]) -> Result<()> {
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
    ensure!(!dynamic || zip.index_for_name(super::dynamic::PART).is_none(),"新建模板的元数据部件已占用");
    let fresh_sst = FreshSst::build(&mut zip, &ordered_sheets)?;
    let need_sst_decl = fresh_sst.as_ref().is_some_and(|s| !s.existed_in_zip);
    let sst_map = fresh_sst.as_ref().map(|s| &s.indices);

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    // Pre-compress multiple streamed worksheets across cores into single-entry temp ZIPs,
    // then splice via O(1) `raw_copy_file`.
    let mut parallel_parts: HashMap<String, tempfile::TempPath> = HashMap::new();
    if ordered_sheets.len() >= 2 {
        let mut jobs: Vec<(String, String, &Sheet)> = Vec::with_capacity(ordered_sheets.len());
        for &(part, sheet) in &ordered_sheets {
            let shell = super::read_zip_text(&mut zip, part)?;
            jobs.push((part.to_owned(), shell, sheet));
        }
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .min(8)
            .min(jobs.len());
        let next_job = std::sync::atomic::AtomicUsize::new(0);
        let results: std::sync::Mutex<Vec<Result<(String, tempfile::TempPath)>>> =
            std::sync::Mutex::new(Vec::with_capacity(jobs.len()));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let jobs_ref = &jobs;
                let next_ref = &next_job;
                let res_ref = &results;
                scope.spawn(move || {
                    loop {
                        let idx = next_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((part, shell, sheet)) = jobs_ref.get(idx) else {
                            break;
                        };
                        let one = (|| -> Result<(String, tempfile::TempPath)> {
                            let (f, tp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
                            let mut zw = ZipWriter::new(BufWriter::with_capacity(128 * 1024, f));
                            zw.start_file(
                                part.as_str(),
                                SimpleFileOptions::default()
                                    .compression_method(CompressionMethod::Deflated)
                                    .compression_level(Some(2)),
                            )?;
                            write_sheet(&mut zw, shell, sheet, sst_map)?;
                            zw.finish()?.flush()?;
                            Ok((part.clone(), tp))
                        })();
                        res_ref.lock().unwrap().push(one);
                    }
                });
            }
        });
        for item in results.into_inner().unwrap() {
            let (part, tp) = item?;
            parallel_parts.insert(part, tp);
        }
    }

    let (file, temp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
    let mut out = ZipWriter::new(BufWriter::with_capacity(256 * 1024, file));
    for i in 0..zip.len() {
        let part = zip.name_for_index(i).context("缺少 ZIP 条目名")?.to_owned();
        if let Some(sheet) = sheets.remove(part.as_str()) {
            if let Some(tp) = parallel_parts.remove(&part) {
                let mut part_zip = ZipArchive::new(File::open(&tp)?)?;
                out.raw_copy_file(part_zip.by_index_raw(0)?)?;
            } else {
                let shell = super::read_zip_text(&mut zip, &part)?;
                out.start_file(
                    part,
                    SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Deflated)
                        .compression_level(Some(2)),
                )?;
                write_sheet(&mut out, &shell, sheet, sst_map)?;
            }
        } else if part == SST_PART && fresh_sst.is_some() {
            let sst = fresh_sst.as_ref().unwrap();
            out.start_file(
                SST_PART,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .compression_level(Some(2)),
            )?;
            out.write_all(&sst.xml)?;
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
            out.raw_copy_file(zip.by_index_raw(i)?)?;
        }
    }
    ensure!(sheets.is_empty(), "缺少流式工作表部件");
    if let Some(sst) = &fresh_sst {
        if !sst.existed_in_zip {
            out.start_file(
                SST_PART,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .compression_level(Some(2)),
            )?;
            out.write_all(&sst.xml)?;
        }
    }
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
}
