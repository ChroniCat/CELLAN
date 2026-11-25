//! 保存后的 XML 补丁：把条件格式 / 数据验证这类 umya 写不干净的结构直接补进 xlsx。
//!
//! 做法是重写 zip：读出每个 entry，命中目标文件就替换内容，再整体写回。
//! 这样不会碰 umya 已经写好的其余部分，也不用担心它内部模型覆盖我们。

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;

/// workbookPr/@date1904：true/1 都算（Excel 写 "1"，有些工具写 "true"）
pub fn read_date1904(path: &Path) -> bool {
    workbook_xml(path).is_some_and(|xml| date1904_xml(&xml))
}

fn workbook_xml(path: &Path) -> Option<String> {
    let name = "xl/workbook.xml";
    let bytes = crate::fixups::read_part_file(path, name)?;
    crate::xlsx::patch_xml_text(&bytes, name).ok().map(|xml| xml.into_owned())
}

pub(crate) fn date1904_xml(xml: &str) -> bool {
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.local_name().as_ref() == "workbookPr" => {
                for a in e.attributes().flatten() {
                    if a.key.local_name().as_ref() == "date1904" {
                        let v = a.value.to_ascii_lowercase();
                        return v == "1" || v == "true";
                    }
                }
                return false;
            }
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
    }
}

pub(crate) fn set_date1904(xml: &str, is_1904: bool) -> String {
    if date1904_xml(xml) == is_1904 {
        return xml.to_string();
    }
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return xml.to_string();
    };
    let root = doc.root_element();
    let prefix = root
        .tag_name()
        .namespace()
        .and_then(|uri| root.lookup_prefix(uri))
        .map(|p| format!("{p}:"))
        .unwrap_or_default();
    if let Some(pr) = root
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "workbookPr")
    {
        let mut out = xml.to_string();
        if let Some(attr) = pr.attribute_node("date1904") {
            if is_1904 {
                out.replace_range(attr.range_value(), "1");
            } else {
                out.replace_range(attr.range(), "");
            }
        } else if is_1904 {
            let r = pr.range();
            let raw = &xml[r.clone()];
            if let Some(gt) = raw.find('>') {
                let insert_at = if raw.as_bytes().get(gt.saturating_sub(1)) == Some(&b'/') {
                    r.start + gt - 1
                } else {
                    r.start + gt
                };
                out.insert_str(insert_at, " date1904=\"1\"");
            }
        }
        return out;
    }
    if !is_1904 {
        return xml.to_string();
    }
    let mut out = xml.to_string();
    let insert_at = root
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "fileVersion")
        .map(|n| n.range().end)
        .or_else(|| {
            xml[root.range().start..]
                .find('>')
                .map(|gt| root.range().start + gt + 1)
        })
        .unwrap_or(0);
    out.insert_str(insert_at, &format!("<{prefix}workbookPr date1904=\"1\"/>"));
    out
}

/// 表名 → zip 内的 worksheet 路径
pub fn sheet_targets(path: &Path) -> Result<HashMap<String, String>> {
    let staged_here = stage_active_for(path);
    if staged_here && let Some(cached) = STAGE_TARGETS.with(|cache| cache.borrow().as_ref()
        .filter(|(owner, _)| owner == path).map(|(_, targets)| targets.clone())) {
        return Ok(cached);
    }
    let workbook = "xl/workbook.xml";
    let relations = "xl/_rels/workbook.xml.rels";
    let (wb_xml_opt, rels_xml_opt) = if staged_here {
        let read = |part| crate::fixups::read_part(path, part)
            .map(|bytes| crate::xlsx::patch_xml_text(&bytes,part).map(|xml| xml.into_owned())).transpose();
        (read(workbook)?, read(relations)?)
    } else {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let mut read = |part| zip.index_for_name(part).map(|_| crate::xlsx::read_zip_text(&mut zip,part)).transpose();
        (read(workbook)?, read(relations)?)
    };

    let targets = sheet_targets_xml(wb_xml_opt.as_deref(), rels_xml_opt.as_deref());
    if staged_here && wb_xml_opt.is_some() && rels_xml_opt.is_some() {
        STAGE_TARGETS.with(|cache| *cache.borrow_mut() = Some((path.to_owned(), targets.clone())));
    }
    Ok(targets)
}

pub(crate) fn sheet_targets_xml(wb_xml_opt: Option<&str>, rels_xml_opt: Option<&str>) -> HashMap<String, String> {
    let mut order: Vec<(String, String)> = Vec::new();
    if let Some(xml) = wb_xml_opt {
        let mut r = Reader::from_str(xml);
        loop {
            match r.read_event() {
                Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("sheet") => {
                    let mut name = String::new();
                    let mut rid = String::new();
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        if k.ends_with("name") {
                            name = v;
                        } else if k == "r:id" || k.ends_with(":id") {
                            rid = v;
                        }
                    }
                    if !name.is_empty() {
                        order.push((name, rid));
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
        }
    }
    let mut targets: HashMap<String, String> = HashMap::new();
    if let Some(xml) = rels_xml_opt {
        let mut r = Reader::from_str(xml);
        loop {
            match r.read_event() {
                Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("Relationship") => {
                    let mut id = String::new();
                    let mut t = String::new();
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        if k == "Id" {
                            id = v;
                        } else if k == "Target" {
                            t = v;
                        }
                    }
                    if !id.is_empty() && !t.is_empty() {
                        let p = if t.starts_with('/') { t.trim_start_matches('/').to_string() } else { format!("xl/{t}") };
                        targets.insert(id, p);
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
        }
    }
    let mut out = HashMap::new();
    for (i, (name, rid)) in order.iter().enumerate() {
        let p = targets
            .get(rid)
            .cloned()
            .unwrap_or_else(|| format!("xl/worksheets/sheet{}.xml", i + 1));
        out.insert(name.clone(), p);
    }
    out
}

/// 按谓词筛选后重写 zip：不命中的部件直接 raw_copy（零解压、零克隆）；在暂存模式下直接改内存暂存表
pub fn rewrite_zip_where<P, F>(path: &Path, mut pred: P, mut f: F) -> Result<()>
where
    P: FnMut(&str) -> bool,
    F: FnMut(&str, Vec<u8>) -> Vec<u8>,
{
    rewrite_zip_borrowed(path, &mut pred, |name, original| {
        let updated = f(name, original.to_vec());
        Ok((updated != original).then_some(updated))
    })
}

pub(crate) fn rewrite_zip_borrowed(
    path: &Path,
    mut pred: impl FnMut(&str) -> bool,
    mut update: impl FnMut(&str, &[u8]) -> Result<Option<Vec<u8>>>,
) -> Result<()> {
    if stage_active_for(path) {
        let names = crate::fixups::entries_of(path);
        for name in names {
            if !pred(&name) {
                continue;
            }
            let original = match staged_shared_at(path, &name) {
                Some(Some(body)) => body,
                Some(None) => continue,
                None => {
                    let buf = crate::fixups::read_part_file(path, &name)
                        .with_context(|| format!("无法读取待更新的 ZIP 部件 {name}"))?;
                    std::rc::Rc::new(buf)
                }
            };
            if let Some(updated) = update(&name, &original)? {
                stage_put(&name, updated);
            }
        }
        return Ok(());
    }
    let file = std::fs::File::open(path).with_context(|| format!("打开 {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut any_match = false;
    for i in 0..zip.len() {
        let name = zip.by_index_raw(i)?.name().to_string();
        if pred(&name) {
            any_match = true;
            break;
        }
    }
    if !any_match {
        return Ok(());
    }
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let (out, tmp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
    let mut any_changed = false;
    {
        let mut zw = zip::ZipWriter::new(BufWriter::new(out));
        for i in 0..zip.len() {
            let name = zip.by_index_raw(i)?.name().to_string();
            if !pred(&name) {
                let e = zip.by_index_raw(i)?;
                zw.raw_copy_file(e)?;
                continue;
            }
            let mut buf = Vec::new();
            {
                let mut e = zip.by_index(i)?;
                e.read_to_end(&mut buf)?;
            }
            if let Some(updated) = update(&name, &buf)? {
                any_changed = true;
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(Some(4));
                zw.start_file(name, opts)?;
                zw.write_all(&updated)?;
            } else {
                zw.raw_copy_file(zip.by_index_raw(i)?)?;
            }
        }
        zw.finish()?.flush()?;
    }
    drop(zip);
    if any_changed {
        #[cfg(unix)]
        std::fs::set_permissions(&tmp, std::fs::metadata(path)?.permissions())?;
        tmp.persist(path)?;
    }
    Ok(())
}

// Worksheet patchers read the original entry by reference and allocate only the replacement.
fn rewrite_sheet_xml<T>(
    path: &Path, sheets: &std::collections::HashMap<String, T>,
    transform: impl Fn(&str, &T) -> String,
) -> Result<()> {
    rewrite_zip_borrowed(path, |name| sheets.contains_key(name), |name, buf| {
        let xml = crate::xlsx::patch_xml_text(buf, name)?;
        let updated = transform(&xml, &sheets[name]);
        Ok((updated.as_str() != xml.as_ref()).then(|| updated.into_bytes()))
    })
}

/// Worksheet-level setters only need the metadata surrounding sheetData. Keep
/// the original row bytes out of their temporary strings and equality checks.
fn rewrite_sheet_metadata<T>(
    path: &Path,
    sheets: &std::collections::HashMap<String, T>,
    transform: impl Fn(&str, &T) -> Result<String>,
) -> Result<()> {
    rewrite_zip_borrowed(
        path,
        |name| sheets.contains_key(name),
        |name, bytes| transform_worksheet(name, bytes, true, |xml| transform(xml, &sheets[name])),
    )
}

pub(crate) fn transform_worksheet(
    name: &str,
    bytes: &[u8],
    metadata_only: bool,
    transform: impl FnOnce(&str) -> Result<String>,
) -> Result<Option<Vec<u8>>> {
    fn contents(xml: &str) -> Result<Option<std::ops::Range<usize>>> {
        // Fast O(header + footer) boundary scan for standard unprefixed `<sheetData>...</sheetData>`
        // when the suffix contains no comments or CDATA, avoiding a 200MB full-sheet XML lex pass.
        if xml.len() >= 64 * 1024 {
            let b = xml.as_bytes();
            if let Some(open_pos) = b.windows(10).position(|w| w == b"<sheetData") {
                if !b[..open_pos].windows(4).any(|w| w == b"<!--") {
                    let after_name = open_pos + 10;
                    if let Some(gt_rel) = b[after_name..].iter().position(|&c| c == b'>') {
                        let gt = after_name + gt_rel;
                        if b[gt - 1] == b'/' {
                            return Ok(Some((gt + 1)..(gt + 1)));
                        }
                        if let Some(close_pos) = xml.rfind("</sheetData>") {
                            if close_pos > gt
                                && !b[close_pos + 12..].windows(4).any(|w| w == b"<!--")
                                && !b[close_pos + 12..].windows(10).any(|w| w == b"<sheetData")
                            {
                                return Ok(Some((gt + 1)..close_pos));
                            }
                        }
                    }
                }
            }
        }
        use quick_xml::events::Event;
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut depth = 0usize;
        loop {
            match reader.read_event()? {
                Event::Start(e) if depth == 1 && e.local_name().as_ref() == "sheetData" => {
                    let span = reader.read_to_end(e.name())?;
                    return Ok(Some(span.start as usize..span.end as usize));
                }
                Event::Start(_) => depth += 1,
                Event::End(_) => depth = depth.checked_sub(1).context("invalid worksheet XML")?,
                Event::Eof => return Ok(None),
                _ => {}
            }
        }
    }
    let xml = crate::xlsx::patch_xml_text(bytes, name)?;
    if metadata_only
        && xml.len() >= 64 * 1024
        && let Some(body) = contents(&xml)?
        && !body.is_empty()
    {
        let shell = [&xml[..body.start], &xml[body.end..]].concat();
        let metadata = transform(&shell)?;
        if metadata == shell {
            return Ok(None);
        }
        let slot = contents(&metadata)?.context("metadata patch removed sheetData")?;
        anyhow::ensure!(
            slot.is_empty(),
            "metadata patch generated unexpected worksheet cells"
        );
        let mut result = Vec::with_capacity(metadata.len() + body.len());
        result.extend_from_slice(&metadata.as_bytes()[..slot.start]);
        result.extend_from_slice(&xml.as_bytes()[body]);
        result.extend_from_slice(&metadata.as_bytes()[slot.end..]);
        return Ok(Some(result));
    }
    let updated = transform(&xml)?;
    Ok((updated.as_str() != xml.as_ref()).then(|| updated.into_bytes()))
}

/// 重写 zip：对每个 entry 调用 f，返回新内容
pub fn rewrite_zip<F>(path: &Path, f: F) -> Result<()>
where
    F: FnMut(&str, Vec<u8>) -> Vec<u8>,
{
    rewrite_zip_where(path, |_| true, f)
}

// ---------------------------------------------------------------------------
// 条件格式
// ---------------------------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn dxf_xml(d: &crate::cf::Dxf) -> String {
    let mut out = String::from("<dxf>");
    if d.bold || d.italic || d.color.is_some() || d.font.is_some() {
        out.push_str("<font>");
        if d.bold {
            out.push_str("<b/>");
        }
        if d.italic {
            out.push_str("<i/>");
        }
        if let Some(c) = d.color {
            out.push_str(&format!("<color rgb=\"FF{:02X}{:02X}{:02X}\"/>", c[0], c[1], c[2]));
        }
        if let Some(f) = &d.font {
            out.push_str(&format!("<name val=\"{}\"/>", esc(f)));
        }
        out.push_str("</font>");
    }
    if let Some(f) = d.fill {
        out.push_str(&format!(
            "<fill><patternFill patternType=\"solid\"><bgColor rgb=\"FF{:02X}{:02X}{:02X}\"/></patternFill></fill>",
            f[0], f[1], f[2]
        ));
    }
    if let Some(code) = &d.num_fmt {
        out.push_str(&format!("<numFmt numFmtId=\"0\" formatCode=\"{}\"/>", esc(code)));
    }
    if let Some(b) = &d.border {
        // 四边各自写各自的 style/颜色，不再拿最后一条套满四边
        let mut border = String::from("<border>");
        for (i, tag) in ["left", "right", "top", "bottom"].iter().enumerate() {
            if let Some((style, color)) = &b.sides[i] {
                border.push_str(&format!(
                    "<{tag} style=\"{s}\"><color rgb=\"FF{:02X}{:02X}{:02X}\"/></{tag}>",
                    color[0],
                    color[1],
                    color[2],
                    s = esc(style)
                ));
            }
        }
        border.push_str("</border>");
        out.push_str(&border);
    }
    out.push_str("</dxf>");
    out
}

fn range_sqref(r: &(u32, u32, u32, u32)) -> String {
    format!(
        "{}{}:{}{}",
        crate::model::col_label(r.1),
        r.0 + 1,
        crate::model::col_label(r.3),
        r.2 + 1
    )
}

fn cfvo_xml(c: &crate::cf::Cfvo) -> String {
    use crate::cf::CfvoKind;
    match c.kind {
        CfvoKind::Min => "<cfvo type=\"min\"/>".to_string(),
        CfvoKind::Max => "<cfvo type=\"max\"/>".to_string(),
        CfvoKind::Formula => format!("<cfvo type=\"formula\" val=\"{}\"/>", esc(&c.formula)),
        _ => format!("<cfvo type=\"{}\" val=\"{}\"/>", c.type_name(), crate::model::format_number(c.val)),
    }
}

/// 按 Excel 的 priority 语义写出：priority 原样保留（0 的补号），
/// 同表内按 priority 升序排列，XML 顺序不再是求值依据。
pub fn cf_blocks_xml(blocks: &[crate::cf::CfBlock]) -> String {
    use crate::cf::CfKind;
    let mut out = String::new();
    // 先给所有规则一个最终 priority（未指定的按出现顺序补）
    let mut next = 1u32;
    let mut used: Vec<u32> = blocks
        .iter()
        .flat_map(|b| b.rules.iter().map(|r| r.priority).filter(|p| *p > 0))
        .collect();
    used.sort_unstable();
    let mut assign: Vec<Vec<u32>> = Vec::new();
    for b in blocks {
        let mut v = Vec::new();
        for r in b.rules.iter() {
            if r.priority > 0 {
                v.push(r.priority);
            } else {
                while used.contains(&next) {
                    next += 1;
                }
                used.push(next);
                v.push(next);
            }
        }
        assign.push(v);
    }
    // 块与块内规则都按 priority 升序（Excel 的存储顺序）
    let mut block_order: Vec<usize> = (0..blocks.len()).collect();
    block_order.sort_by_key(|i| assign[*i].iter().copied().min().unwrap_or(u32::MAX));
    for bi in block_order {
        let b = &blocks[bi];
        if b.ranges.is_empty() || b.rules.is_empty() {
            continue;
        }
        let mut rule_order: Vec<usize> = (0..b.rules.len()).collect();
        rule_order.sort_by_key(|i| assign[bi][*i]);
        let sqref: Vec<String> = b.ranges.iter().map(range_sqref).collect();
        out.push_str(&format!("<conditionalFormatting sqref=\"{}\">", sqref.join(" ")));
        for ri in rule_order {
            let rule = &b.rules[ri];
            let prio = assign[bi][ri];
            let stop = if rule.stop { " stopIfTrue=\"1\"" } else { "" };
            match &rule.kind {
                CfKind::CellIs { op, a, b: b2, dxf } => {
                    let opname = match op {
                        crate::cf::CfOp::Less => "lessThan",
                        crate::cf::CfOp::LessEq => "lessThanOrEqual",
                        crate::cf::CfOp::Equal => "equal",
                        crate::cf::CfOp::NotEqual => "notEqual",
                        crate::cf::CfOp::GreaterEq => "greaterThanOrEqual",
                        crate::cf::CfOp::Greater => "greaterThan",
                        crate::cf::CfOp::Between => "between",
                        crate::cf::CfOp::NotBetween => "notBetween",
                    };
                    out.push_str(&format!(
                        "<cfRule type=\"cellIs\" operator=\"{opname}\" dxfId=\"{dxf}\" priority=\"{prio}\"{stop}>"
                    ));
                    out.push_str(&format!("<formula>{}</formula>", esc(a)));
                    if matches!(op, crate::cf::CfOp::Between | crate::cf::CfOp::NotBetween) {
                        out.push_str(&format!("<formula>{}</formula>", esc(b2)));
                    }
                    out.push_str("</cfRule>");
                }
                CfKind::Text { contains, dxf } => {
                    out.push_str(&format!(
                        "<cfRule type=\"containsText\" operator=\"containsText\" text=\"{}\" dxfId=\"{dxf}\" priority=\"{prio}\"{stop}>",
                        esc(contains)
                    ));
                    let anchor = b.ranges.first().map(|r| format!("{}{}", crate::model::col_label(r.1), r.0 + 1)).unwrap_or_else(|| "A1".into());
                    out.push_str(&format!(
                        "<formula>NOT(ISERROR(SEARCH(\"{}\",{})))</formula>",
                        esc(contains),
                        anchor
                    ));
                    out.push_str("</cfRule>");
                }
                CfKind::TextMatch { op, text, dxf } => {
                    let ty = op.ooxml_type();
                    let oper = op.ooxml_operator();
                    let anchor = b.ranges.first().map(|r| format!("{}{}", crate::model::col_label(r.1), r.0 + 1)).unwrap_or_else(|| "A1".into());
                    let fml = match op {
                        crate::cf::TextOp::Contains => format!("NOT(ISERROR(SEARCH(\"{}\",{})))", esc(text), anchor),
                        crate::cf::TextOp::NotContains => format!("ISERROR(SEARCH(\"{}\",{}))", esc(text), anchor),
                        crate::cf::TextOp::BeginsWith => format!("LEFT({},{})= \"{}\"", anchor, text.chars().count(), esc(text)),
                        crate::cf::TextOp::EndsWith => format!("RIGHT({},{})=\"{}\"", anchor, text.chars().count(), esc(text)),
                    };
                    out.push_str(&format!(
                        "<cfRule type=\"{ty}\" operator=\"{oper}\" text=\"{}\" dxfId=\"{dxf}\" priority=\"{prio}\"{stop}><formula>{fml}</formula></cfRule>",
                        esc(text)
                    ));
                }
                CfKind::TimePeriod { period, dxf } => {
                    let tp = period.as_ooxml();
                    let anchor = b.ranges.first().map(|r| format!("{}{}", crate::model::col_label(r.1), r.0 + 1)).unwrap_or_else(|| "A1".into());
                    out.push_str(&format!(
                        "<cfRule type=\"timePeriod\" timePeriod=\"{tp}\" dxfId=\"{dxf}\" priority=\"{prio}\"{stop}><formula>FLOOR({anchor},1)=TODAY()</formula></cfRule>"
                    ));
                }
                CfKind::Blanks { blank, dxf } => {
                    let ty = if *blank { "containsBlanks" } else { "notContainsBlanks" };
                    let anchor = b.ranges.first().map(|r| format!("{}{}", crate::model::col_label(r.1), r.0 + 1)).unwrap_or_else(|| "A1".into());
                    let fml = if *blank {
                        format!("LEN(TRIM({anchor}))=0")
                    } else {
                        format!("LEN(TRIM({anchor}))&gt;0")
                    };
                    out.push_str(&format!(
                        "<cfRule type=\"{ty}\" dxfId=\"{dxf}\" priority=\"{prio}\"{stop}><formula>{fml}</formula></cfRule>"
                    ));
                }
                CfKind::Errors { error, dxf } => {
                    let ty = if *error { "containsErrors" } else { "notContainsErrors" };
                    let anchor = b.ranges.first().map(|r| format!("{}{}", crate::model::col_label(r.1), r.0 + 1)).unwrap_or_else(|| "A1".into());
                    let fml = if *error {
                        format!("ISERROR({anchor})")
                    } else {
                        format!("NOT(ISERROR({anchor}))")
                    };
                    out.push_str(&format!(
                        "<cfRule type=\"{ty}\" dxfId=\"{dxf}\" priority=\"{prio}\"{stop}><formula>{fml}</formula></cfRule>"
                    ));
                }
                CfKind::Dup { unique, dxf } => {
                    let ty = if *unique { "uniqueValues" } else { "duplicateValues" };
                    out.push_str(&format!(
                        "<cfRule type=\"{ty}\" dxfId=\"{dxf}\" priority=\"{prio}\"{stop}/>"
                    ));
                }
                CfKind::Top { top, n, percent, dxf } => {
                    out.push_str(&format!(
                        "<cfRule type=\"top10\" dxfId=\"{dxf}\" priority=\"{prio}\" rank=\"{n}\" bottom=\"{}\" percent=\"{}\"{stop}/>",
                        if *top { 0 } else { 1 },
                        if *percent { 1 } else { 0 }
                    ));
                }
                CfKind::Avg { above, dxf } => {
                    out.push_str(&format!(
                        "<cfRule type=\"aboveAverage\" dxfId=\"{dxf}\" priority=\"{prio}\" aboveAverage=\"{}\"{stop}/>",
                        if *above { 1 } else { 0 }
                    ));
                }
                CfKind::Expr { formula, dxf } => {
                    out.push_str(&format!(
                        "<cfRule type=\"expression\" dxfId=\"{dxf}\" priority=\"{prio}\"{stop}><formula>{}</formula></cfRule>",
                        esc(formula.trim_start_matches('='))
                    ));
                }
                CfKind::Scale { points } => {
                    out.push_str(&format!("<cfRule type=\"colorScale\" priority=\"{prio}\"{stop}><colorScale>"));
                    // schema：先全部 <cfvo>，再全部 <color>，不能交错
                    for (v, _) in points.iter() {
                        out.push_str(&cfvo_xml(v));
                    }
                    for (_, c) in points.iter() {
                        out.push_str(&format!(
                            "<color rgb=\"FF{:02X}{:02X}{:02X}\"/>",
                            c[0], c[1], c[2]
                        ));
                    }
                    out.push_str("</colorScale></cfRule>");
                }
                CfKind::IconSet { set, reverse, thresholds, show_value } => {
                    out.push_str(&format!(
                        "<cfRule type=\"iconSet\" priority=\"{prio}\"{stop}><iconSet iconSet=\"{}\"{} showValue=\"{}\">",
                        esc(set),
                        if *reverse { " reverse=\"1\"" } else { "" },
                        if *show_value { 1 } else { 0 }
                    ));
                    for t in thresholds.iter() {
                        out.push_str(&cfvo_xml(t));
                    }
                    out.push_str("</iconSet></cfRule>");
                }
                CfKind::Bar { color, neg_color: _, min, max, show_value } => {
                    let sv = if *show_value { "" } else { " showValue=\"0\"" };
                    out.push_str(&format!("<cfRule type=\"dataBar\" priority=\"{prio}\"{stop}><dataBar{sv}>"));
                    out.push_str(&cfvo_xml(min));
                    out.push_str(&cfvo_xml(max));
                    out.push_str(&format!("<color rgb=\"FF{:02X}{:02X}{:02X}\"/>", color[0], color[1], color[2]));
                    out.push_str("</dataBar></cfRule>");
                }
            }
        }
        out.push_str("</conditionalFormatting>");
    }
    out
}

/// 去掉一段 XML 里的 <conditionalFormatting> 块
pub fn strip_cf(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<conditionalFormatting") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("</conditionalFormatting>") {
            rest = &rest[start + end + "</conditionalFormatting>".len()..];
        } else if let Some(end) = rest[start..].find("/>") {
            // 自闭合形式
            rest = &rest[start + end + 2..];
        } else {
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// 把条件格式块插到 worksheet 里正确的位置（mergeCells 之后、dataValidations 之前）
pub fn insert_cf(xml: &str, blocks_xml: &str) -> String {
    let xml = strip_cf(xml);
    if blocks_xml.is_empty() {
        return xml;
    }
    const ANCHORS: [&str; 10] = [
        "<dataValidations",
        "<hyperlinks",
        "<printOptions",
        "<pageMargins",
        "<pageSetup",
        "<headerFooter",
        "<drawing",
        "<legacyDrawing",
        "<tableParts",
        "</worksheet>",
    ];
    let mut at = xml.len();
    for a in ANCHORS {
        if let Some(i) = xml.find(a) {
            if i < at {
                at = i;
            }
        }
    }
    let mut out = String::with_capacity(xml.len() + blocks_xml.len());
    out.push_str(&xml[..at]);
    out.push_str(blocks_xml);
    out.push_str(&xml[at..]);
    out
}

/// 用我们的 dxf 全表替换 styles.xml 里的 <dxfs>
pub fn replace_dxfs(xml: &str, dxfs: &[crate::cf::Dxf]) -> String {
    let mut body = String::from("<dxfs count=\"");
    body.push_str(&dxfs.len().to_string());
    body.push_str("\">");
    for d in dxfs {
        body.push_str(&dxf_xml(d));
    }
    body.push_str("</dxfs>");

    // 1) 先摘掉旧段：umya 写出来的 styles.xml 里 dxfs 就排在 <extLst> 后面，
    //    只做「原地替换」会把它留在非法位置（CT_Stylesheet 要求 dxfs 在
    //    tableStyles / extLst 之前），Excel 会直接判整个包损坏。
    let mut rest = xml.to_string();
    if let Some(start) = rest.find("<dxfs") {
        let end = rest[start..]
            .find("</dxfs>")
            .map(|k| start + k + "</dxfs>".len())
            .or_else(|| rest[start..].find("/>").map(|k| start + k + 2));
        if let Some(end) = end {
            rest.replace_range(start..end, "");
        }
    }

    // 2) 按 schema 顺序插回去
    let anchor = rest
        .find("<tableStyles")
        .or_else(|| rest.find("<extLst"))
        .or_else(|| rest.find("</styleSheet>"));
    match anchor {
        Some(k) => {
            let mut out = String::with_capacity(rest.len() + body.len());
            out.push_str(&rest[..k]);
            out.push_str(&body);
            out.push_str(&rest[k..]);
            out
        }
        None => xml.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_sheet_targets_follow_metadata_replacements_deletions_and_new_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sheets.xlsx");
        crate::xlsx::save(&path, &crate::Workbook::new_blank()).unwrap();
        let workbook = "xl/workbook.xml";
        let rels = "xl/_rels/workbook.xml.rels";
        begin_stage_for(&path);
        let original = sheet_targets(&path).unwrap();
        let w = String::from_utf8(crate::fixups::read_part(&path, workbook).unwrap()).unwrap();
        stage_put(workbook, w.replace("Sheet1", "Renamed").into_bytes());
        let renamed = sheet_targets(&path).unwrap();
        assert_eq!(renamed.get("Renamed"), original.get("Sheet1"));
        let r = String::from_utf8(crate::fixups::read_part(&path, rels).unwrap()).unwrap();
        stage_put(rels, r.replace("sheet1.xml", "changed.xml").into_bytes());
        assert!(sheet_targets(&path).unwrap()["Renamed"].ends_with("/changed.xml"));
        stage_del(workbook);
        assert!(sheet_targets(&path).unwrap().is_empty());
        clear_stage();
        assert_eq!(sheet_targets(&path).unwrap(), original);
    }

    #[test]
    fn staged_edits_and_raw_copies_preserve_compressed_bytes_and_last_write() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.zip");
        let output = dir.path().join("output.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated).compression_level(Some(1));
        zip.start_file("source", opts).unwrap();
        zip.write_all(&vec![b'a'; 64 * 1024]).unwrap();
        zip.finish().unwrap();
        std::fs::copy(&source, &output).unwrap();

        begin_stage_for(&output);
        stage_raw_copy("source", "renamed");
        stage_raw_copy("source", "patched");
        append_entry(&output, "patched", b"latest").unwrap();
        stage_raw_copy("source", "removed");
        remove_entry(&output, "removed").unwrap();
        commit_stage_with_raw_copies(&output, &source).unwrap();

        let compressed = |path: &Path, name: &str| {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
            let mut bytes = Vec::new();
            zip.by_index_raw(zip.index_for_name(name).unwrap()).unwrap().read_to_end(&mut bytes).unwrap();
            bytes
        };
        assert_eq!(compressed(&output, "renamed"), compressed(&source, "source"));
        assert_eq!(crate::fixups::read_part_file(&output, "patched").unwrap(), b"latest");
        assert!(crate::fixups::read_part_file(&output, "removed").is_none());
    }

    #[test]
    fn strips_and_reinserts_cf() {
        let xml = r#"<worksheet><sheetData/><mergeCells count="1"><mergeCell ref="A1:B2"/></mergeCells><conditionalFormatting sqref="A1"><cfRule type="cellIs" priority="1"/></conditionalFormatting><pageMargins/></worksheet>"#;
        let out = insert_cf(xml, "<conditionalFormatting sqref=\"C1\"><cfRule type=\"cellIs\" priority=\"1\"/></conditionalFormatting>");
        assert_eq!(out.matches("conditionalFormatting").count(), 2, "应只剩新加的一对标签");
        assert!(out.find("<mergeCells").unwrap() < out.find("<conditionalFormatting").unwrap());
        assert!(out.find("<conditionalFormatting").unwrap() < out.find("<pageMargins").unwrap());
    }

    #[test]
    fn dxf_rendering() {
        let d = crate::cf::Dxf { bold: true, color: Some([156, 0, 6]), fill: Some([255, 199, 206]), ..Default::default() };
        let x = dxf_xml(&d);
        assert!(x.contains("<b/>"));
        assert!(x.contains("FF9C0006"));
        assert!(x.contains("FFFFC7CE"));
    }

    #[test]
    fn replaces_dxfs_section() {
        let xml = r#"<styleSheet><cellStyles/><dxfs count="1"><dxf/></dxfs><tableStyles/></styleSheet>"#;
        let out = replace_dxfs(xml, &[crate::cf::Dxf { fill: Some([1, 2, 3]), ..Default::default() }]);
        assert!(out.contains("count=\"1\""));
        assert!(out.contains("FF010203"));
        assert_eq!(out.matches("<dxfs").count(), 1);
        assert!(out.find("<dxfs").unwrap() < out.find("<tableStyles").unwrap());
    }
}

// ---------------------------------------------------------------------------
// 数据验证
// ---------------------------------------------------------------------------

pub fn dv_xml(list: &[crate::dv::DataValidation]) -> String {
    use crate::dv::DvKind;
    let list: Vec<&crate::dv::DataValidation> = list
        .iter()
        .filter(|d| d.kind != DvKind::Any && !d.ranges.is_empty())
        .collect();
    if list.is_empty() {
        return String::new();
    }
    let mut out = format!("<dataValidations count=\"{}\">", list.len());
    for d in list {
        let sqref: Vec<String> = d.ranges.iter().map(range_sqref).collect();
        out.push_str(&format!(
            "<dataValidation type=\"{}\" allowBlank=\"{}\" sqref=\"{}\"",
            d.kind.to_xml(),
            if d.allow_blank { 1 } else { 0 },
            sqref.join(" ")
        ));
        if d.kind != DvKind::List && d.kind != DvKind::Custom {
            out.push_str(&format!(" operator=\"{}\"", d.op.to_xml()));
        }
        if d.kind == DvKind::List && !d.show_dropdown {
            // Excel 语义：showDropDown=1 表示隐藏箭头
            out.push_str(" showDropDown=\"1\"");
        }
        // 错误样式与提示开关必须显式写：OOXML 里缺省是 0，不写会让 Excel 关掉警告
        out.push_str(&format!(
            " showErrorMessage=\"{}\" showInputMessage=\"{}\"",
            u8::from(d.show_error),
            u8::from(d.show_prompt)
        ));
        if d.error_style != crate::dv::DvStyle::Stop {
            out.push_str(&format!(" errorStyle=\"{}\"", d.error_style.to_xml()));
        }
        if !d.error_title.is_empty() {
            out.push_str(&format!(" errorTitle=\"{}\"", esc(&d.error_title)));
        }
        if !d.error_msg.is_empty() {
            out.push_str(&format!(" error=\"{}\"", esc(&d.error_msg)));
        }
        if !d.prompt_title.is_empty() {
            out.push_str(&format!(" promptTitle=\"{}\"", esc(&d.prompt_title)));
        }
        if !d.prompt_msg.is_empty() {
            out.push_str(&format!(" prompt=\"{}\"", esc(&d.prompt_msg)));
        }
        let f1 = match d.kind {
            DvKind::List => {
                let t = d.f1.trim();
                let is_formula_or_ref = t.starts_with('"')
                    || t.starts_with('=')
                    || t.contains('!')
                    || t.contains(':');
                if !is_formula_or_ref && (t.contains(',') || t.contains('，')) {
                    format!("\"{}\"", t.replace('，', ","))
                } else {
                    d.f1.clone()
                }
            }
            _ => d.f1.clone(),
        };
        // formula1/formula2 必须是子元素，写成属性 Excel 判损坏
        let f2 = if (d.op.needs_pair() || (d.kind == DvKind::Custom && !d.f2.is_empty()))
            && !d.f2.is_empty()
        {
            Some(d.f2.clone())
        } else {
            None
        };
        out.push('>');
        out.push_str(&format!("<formula1>{}</formula1>", esc(&f1)));
        if let Some(f2) = f2 {
            out.push_str(&format!("<formula2>{}</formula2>", esc(&f2)));
        }
        out.push_str("</dataValidation>");
    }
    out.push_str("</dataValidations>");
    out
}

pub fn strip_dv(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<dataValidations") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("</dataValidations>") {
            rest = &rest[start + end + "</dataValidations>".len()..];
        } else if let Some(end) = rest[start..].find("/>") {
            rest = &rest[start + end + 2..];
        } else {
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

pub fn insert_dv(xml: &str, dv: &str) -> String {
    let xml = strip_dv(xml);
    if dv.is_empty() {
        return xml;
    }
    const ANCHORS: [&str; 8] = [
        "<hyperlinks",
        "<printOptions",
        "<pageMargins",
        "<pageSetup",
        "<headerFooter",
        "<drawing",
        "<legacyDrawing",
        "</worksheet>",
    ];
    let mut at = xml.len();
    for a in ANCHORS {
        if let Some(i) = xml.find(a) {
            if i < at {
                at = i;
            }
        }
    }
    let mut out = String::with_capacity(xml.len() + dv.len());
    out.push_str(&xml[..at]);
    out.push_str(dv);
    out.push_str(&xml[at..]);
    out
}

#[cfg(test)]
mod dv_tests {
    use super::*;
    use crate::dv::{DataValidation, DvKind, DvOp};

    #[test]
    fn writes_and_replaces() {
        let dv = DataValidation {
            ranges: vec![(1, 1, 9, 1)],
            kind: DvKind::List,
            f1: "甲,乙".into(),
            ..Default::default()
        };
        let xml = dv_xml(&[dv]);
        assert!(xml.contains("type=\"list\""));
        assert!(xml.contains("<formula1>&quot;甲,乙&quot;</formula1>"), "{xml}");
        let sheet = r#"<worksheet><sheetData/></worksheet>"#;
        let out = insert_dv(sheet, &xml);
        assert!(out.contains("<dataValidations"));
        assert!(out.find("<dataValidations").unwrap() < out.find("</worksheet>").unwrap());

        // 再插一次应替换而不是叠加
        let out2 = insert_dv(&out, &xml);
        assert_eq!(out2.matches("<dataValidations").count(), 1);
    }

    #[test]
    fn whole_with_pair() {
        let dv = DataValidation {
            ranges: vec![(0, 0, 0, 0)],
            kind: DvKind::Whole,
            op: DvOp::Between,
            f1: "1".into(),
            f2: "100".into(),
            ..Default::default()
        };
        let xml = dv_xml(&[dv]);
        assert!(xml.contains("operator=\"between\""));
        assert!(xml.contains("<formula2>100</formula2>"));
    }

    #[test]
    fn skips_any_kind() {
        let dv = DataValidation { ranges: vec![(0, 0, 0, 0)], kind: DvKind::Any, ..Default::default() };
        assert!(dv_xml(&[dv]).is_empty());
    }
}

// ---------------------------------------------------------------------------
// 冻结窗格
// ---------------------------------------------------------------------------

/// 写入/清除 <pane>：没有就建成 <sheetView>…</sheetView>，有就先删掉旧的
pub fn set_freeze_xml(xml: &str, freeze: Option<(u32, u32)>) -> String {
    // 先去掉已有的 pane
    let mut base = String::with_capacity(xml.len());
    {
        let mut rest = xml;
        while let Some(i) = rest.find("<pane") {
            base.push_str(&rest[..i]);
            if let Some(e) = rest[i..].find("/>") {
                rest = &rest[i + e + 2..];
            } else if let Some(e) = rest[i..].find("</pane>") {
                rest = &rest[i + e + 7..];
            } else {
                rest = "";
            }
        }
        base.push_str(rest);
    }
    let Some((rows, cols)) = freeze else { return base };
    if rows == 0 && cols == 0 {
        return base;
    }
    let pane = format!(
        "<pane xSplit=\"{cols}\" ySplit=\"{rows}\" topLeftCell=\"{}{}\" activePane=\"bottomRight\" state=\"frozen\"/>",
        crate::model::col_label(cols),
        rows + 1
    );
    // 找第一个 <sheetView（注意别匹配到 <sheetViews>）
    let mut found = None;
    let mut from = 0;
    while let Some(rel) = base[from..].find("<sheetView") {
        let i = from + rel;
        let after = base[i + "<sheetView".len()..].chars().next();
        if matches!(after, Some(' ') | Some('>') | Some('/')) {
            found = Some(i);
            break;
        }
        from = i + 1;
    }
    let Some(i) = found else {
        // 新工作簿的 sheet XML 没有 <sheetViews>：以前直接放弃，冻结窗格
        // 在新文件上保存后等于没设。这里按 schema 顺序补出来。
        let anchor = base
            .find("<sheetFormatPr")
            .or_else(|| base.find("<cols"))
            .or_else(|| base.find("<sheetData"));
        let Some(at) = anchor else { return base };
        let block = format!(
            "<sheetViews><sheetView workbookViewId=\"0\">{pane}</sheetView></sheetViews>"
        );
        let mut out = String::with_capacity(base.len() + block.len());
        out.push_str(&base[..at]);
        out.push_str(&block);
        out.push_str(&base[at..]);
        return out;
    };
    let Some(gt_rel) = base[i..].find('>') else { return base };
    let gt = i + gt_rel;
    let self_closing = gt > i && base[..gt].ends_with('/');
    if self_closing {
        let mut out = String::with_capacity(base.len() + pane.len() + 20);
        out.push_str(&base[..gt - 1]);
        out.push('>');
        out.push_str(&pane);
        out.push_str("</sheetView>");
        out.push_str(&base[gt + 1..]);
        return out;
    }
    let mut out = String::with_capacity(base.len() + pane.len());
    out.push_str(&base[..gt + 1]);
    out.push_str(&pane);
    out.push_str(&base[gt + 1..]);
    out
}

#[cfg(test)]
mod freeze_tests {
    use super::*;

    #[test]
    fn adds_pane_to_self_closing_view() {
        let xml = r#"<worksheet><sheetViews><sheetView workbookViewId="0"/></sheetViews><sheetData/></worksheet>"#;
        let out = set_freeze_xml(xml, Some((1, 2)));
        assert!(out.contains("<pane xSplit=\"2\" ySplit=\"1\" topLeftCell=\"C2\""));
        assert!(out.contains("</sheetView>"));
        assert!(out.contains("<sheetData/>"));
    }

    #[test]
    fn replaces_existing_and_clears() {
        let xml = r#"<worksheet><sheetViews><sheetView workbookViewId="0"><pane ySplit="1" state="frozen"/></sheetView></sheetViews></worksheet>"#;
        let out = set_freeze_xml(xml, Some((3, 1)));
        assert_eq!(out.matches("<pane").count(), 1, "旧 pane 应被替换");
        assert!(out.contains("ySplit=\"3\""));
        let cleared = set_freeze_xml(&out, None);
        assert!(!cleared.contains("<pane"));
        assert!(cleared.contains("<sheetView"));
    }
}

// ---------------------------------------------------------------------------
// 自动筛选
// ---------------------------------------------------------------------------

pub fn auto_filter_xml(f: &crate::sort_filter::Filter) -> String {
    let mut tmp = Vec::new();
    auto_filter_xml_with_dxfs(f, &mut tmp)
}

fn intern_color_filter_dxf(dxfs: &mut Vec<crate::cf::Dxf>, want: crate::cf::Dxf) -> usize {
    if let Some(pos) = dxfs.iter().position(|d| *d == want) {
        return pos;
    }
    let idx = dxfs.len();
    dxfs.push(want);
    idx
}

pub fn auto_filter_xml_with_dxfs(
    f: &crate::sort_filter::Filter,
    dxfs: &mut Vec<crate::cf::Dxf>,
) -> String {
    let (r1, c1, r2, c2) = f.range;
    let mut out = format!(
        "<autoFilter ref=\"{}{}:{}{}\">",
        crate::model::col_label(c1),
        r1 + 1,
        crate::model::col_label(c2),
        r2 + 1
    );
    let mut cols: Vec<u32> = f.allows.keys().copied().collect();
    for c in f
        .criteria
        .keys()
        .chain(f.date_groups.keys())
        .chain(f.color_filters.keys())
        .chain(f.top_filters.keys())
        .chain(f.dynamic_filters.keys())
    {
        if !cols.contains(c) {
            cols.push(*c);
        }
    }
    cols.sort();
    for col in cols {
        if col < c1 || col > c2 {
            continue;
        }
        let col_id = col - c1;
        if let Some(tf) = f.top_filters.get(&col) {
            out.push_str(&format!(
                "<filterColumn colId=\"{col_id}\"><top10 top=\"{}\" percent=\"{}\" val=\"{}\"/></filterColumn>",
                if tf.top { 1 } else { 0 },
                if tf.percent { 1 } else { 0 },
                tf.count.max(1)
            ));
            continue;
        }
        if let Some(df) = f.dynamic_filters.get(&col) {
            out.push_str(&format!(
                "<filterColumn colId=\"{col_id}\"><dynamicFilter type=\"{}\"/></filterColumn>",
                df.as_ooxml()
            ));
            continue;
        }
        if let Some(cf) = f.color_filters.get(&col) {
            match cf {
                crate::sort_filter::ColorFilter::Fill(None) => {
                    out.push_str(&format!(
                        "<filterColumn colId=\"{col_id}\"><colorFilter cellColor=\"1\"/></filterColumn>"
                    ));
                }
                crate::sort_filter::ColorFilter::Fill(Some(rgb)) => {
                    let id = intern_color_filter_dxf(
                        dxfs,
                        crate::cf::Dxf {
                            fill: Some(*rgb),
                            ..Default::default()
                        },
                    );
                    out.push_str(&format!(
                        "<filterColumn colId=\"{col_id}\"><colorFilter dxfId=\"{id}\" cellColor=\"1\"/></filterColumn>"
                    ));
                }
                crate::sort_filter::ColorFilter::Font(rgb) => {
                    let id = intern_color_filter_dxf(
                        dxfs,
                        crate::cf::Dxf {
                            color: Some(*rgb),
                            ..Default::default()
                        },
                    );
                    out.push_str(&format!(
                        "<filterColumn colId=\"{col_id}\"><colorFilter dxfId=\"{id}\" cellColor=\"0\"/></filterColumn>"
                    ));
                }
            }
            continue;
        }
        if let Some(crit) = f.criteria.get(&col) {
            // 条件筛选：介于写成两条 and="1"；文本筛选使用 OOXML 标准 equal/notEqual + 通配符
            let esc_wild = |s: &str| -> String {
                let mut out = String::with_capacity(s.len());
                for ch in s.chars() {
                    if matches!(ch, '~' | '*' | '?') {
                        out.push('~');
                    }
                    out.push(ch);
                }
                out
            };
            let write_one_custom = |out: &mut String, op: crate::sort_filter::CmpOp, val: &str| {
                match op {
                    crate::sort_filter::CmpOp::Contains => {
                        let pat = format!("*{}*", esc_wild(val));
                        out.push_str(&format!("<customFilter operator=\"equal\" val=\"{}\"/>", esc(&pat)));
                    }
                    crate::sort_filter::CmpOp::NotContains => {
                        let pat = format!("*{}*", esc_wild(val));
                        out.push_str(&format!("<customFilter operator=\"notEqual\" val=\"{}\"/>", esc(&pat)));
                    }
                    crate::sort_filter::CmpOp::Begins => {
                        let pat = format!("{}*", esc_wild(val));
                        out.push_str(&format!("<customFilter operator=\"equal\" val=\"{}\"/>", esc(&pat)));
                    }
                    crate::sort_filter::CmpOp::Ends => {
                        let pat = format!("*{}", esc_wild(val));
                        out.push_str(&format!("<customFilter operator=\"equal\" val=\"{}\"/>", esc(&pat)));
                    }
                    other => out.push_str(&format!(
                        "<customFilter operator=\"{}\" val=\"{}\"/>",
                        other.xml_operator(),
                        esc(val)
                    )),
                }
            };
            let is_and = crit.second.as_ref().map(|(a, _, _)| *a).unwrap_or(true);
            let and_attr = if is_and { " and=\"1\"" } else { "" };
            out.push_str(&format!(
                "<filterColumn colId=\"{col_id}\"><customFilters{and_attr}>"
            ));
            if crit.op == crate::sort_filter::CmpOp::Between {
                let hi = crit.val2.clone().unwrap_or_default();
                out.push_str(&format!(
                    "<customFilter operator=\"greaterThanOrEqual\" val=\"{}\"/>",
                    esc(&crit.val)
                ));
                out.push_str(&format!(
                    "<customFilter operator=\"lessThanOrEqual\" val=\"{}\"/>",
                    esc(&hi)
                ));
            } else {
                write_one_custom(&mut out, crit.op, &crit.val);
                if let Some((_, op2, val2)) = &crit.second {
                    write_one_custom(&mut out, *op2, val2);
                }
            }
            out.push_str("</customFilters></filterColumn>");
            continue;
        }
        let set = f.allows.get(&col);
        let dgs = f.date_groups.get(&col).filter(|v| !v.is_empty());
        if set.is_none() && dgs.is_none() {
            continue;
        }
        out.push_str(&format!("<filterColumn colId=\"{col_id}\"><filters>"));
        if let Some(set) = set {
            let mut vals: Vec<&String> = set.iter().collect();
            vals.sort();
            for v in vals {
                out.push_str(&format!("<filter val=\"{}\"/>", esc(v)));
            }
        }
        if let Some(groups) = dgs {
            let mut sorted = groups.clone();
            sorted.sort();
            sorted.dedup();
            for dg in sorted {
                match (dg.month, dg.day) {
                    (Some(m), Some(d)) => out.push_str(&format!(
                        "<dateGroupItem year=\"{}\" month=\"{}\" day=\"{}\" dateTimeGrouping=\"day\"/>",
                        dg.year, m, d
                    )),
                    (Some(m), None) => out.push_str(&format!(
                        "<dateGroupItem year=\"{}\" month=\"{}\" dateTimeGrouping=\"month\"/>",
                        dg.year, m
                    )),
                    _ => out.push_str(&format!(
                        "<dateGroupItem year=\"{}\" dateTimeGrouping=\"year\"/>",
                        dg.year
                    )),
                }
            }
        }
        out.push_str("</filters></filterColumn>");
    }
    out.push_str("</autoFilter>");
    out
}

pub fn strip_auto_filter(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("<autoFilter") {
        out.push_str(&rest[..i]);
        if let Some(e) = rest[i..].find("</autoFilter>") {
            rest = &rest[i + e + "</autoFilter>".len()..];
        } else if let Some(e) = rest[i..].find("/>") {
            rest = &rest[i + e + 2..];
        } else {
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// autoFilter 在 schema 里紧跟 sheetData（在 mergeCells 之前）
pub fn insert_auto_filter(xml: &str, af: &str) -> String {
    let xml = strip_auto_filter(xml);
    if af.is_empty() {
        return xml;
    }
    let mut at = xml.len();
    for a in [
        "<phoneticPr",
        "<mergeCells",
        "<conditionalFormatting",
        "<dataValidations",
        "<hyperlinks",
        "<pageMargins",
        "</worksheet>",
    ] {
        if let Some(i) = xml.find(a) {
            if i < at {
                at = i;
            }
        }
    }
    let mut out = String::with_capacity(xml.len() + af.len());
    out.push_str(&xml[..at]);
    out.push_str(af);
    out.push_str(&xml[at..]);
    out
}

/// 本应用用来保存“手动隐藏行”来源的隐藏定义名。
/// 第三方 OOXML 只有 row hidden，无法区分手动与筛选；本应用保存的文件
/// 可以恢复手动行，包括同时被筛选遮住的 overlap 行。
pub const MANUAL_HIDDEN_NAME: &str = "_sc_manual_hidden";

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)? as u8;
        let lo = (bytes[i + 1] as char).to_digit(16)? as u8;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

fn coalesce_sorted_u32_intervals(rows: &[u32]) -> Vec<(u32, u32)> {
    let mut sorted: Vec<u32> = rows.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for r in sorted {
        match out.last_mut() {
            Some((_, hi)) if r == hi.saturating_add(1) => *hi = r,
            _ => out.push((r, r)),
        }
    }
    out
}

/// 读本应用写入的手动隐藏行来源；无定义名时返回 None。兼容 `SCMH2:` 区间编码与旧版 `SCMH1:`。
pub fn read_manual_hidden(
    path: &Path,
) -> Option<std::collections::HashMap<String, std::collections::HashSet<u32>>> {
    manual_hidden_from_names(&read_defined_names(path))
}

pub(crate) fn manual_hidden_intervals_from_names(
    names: &[(String, Option<usize>, String)],
) -> Option<std::collections::HashMap<String, Vec<(u32, u32)>>> {
    let (_, _, text) = names
        .iter()
        .find(|(name, _, _)| name == MANUAL_HIDDEN_NAME)?;
    let text = text.trim().trim_start_matches('=').trim();
    let text = text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(text);
    if let Some(hex) = text.strip_prefix("SCMH2:") {
        let bytes = hex_decode(hex)?;
        let map: std::collections::BTreeMap<String, Vec<(u32, u32)>> =
            serde_json::from_slice(&bytes).ok()?;
        return Some(map.into_iter().collect());
    }
    let hex = text.strip_prefix("SCMH1:")?;
    let bytes = hex_decode(hex)?;
    let map: std::collections::BTreeMap<String, Vec<u32>> = serde_json::from_slice(&bytes).ok()?;
    Some(
        map.into_iter()
            .map(|(sheet, rows)| (sheet, coalesce_sorted_u32_intervals(&rows)))
            .collect(),
    )
}

pub(crate) fn manual_hidden_from_names(
    names: &[(String, Option<usize>, String)],
) -> Option<std::collections::HashMap<String, std::collections::HashSet<u32>>> {
    let intervals = manual_hidden_intervals_from_names(names)?;
    Some(
        intervals
            .into_iter()
            .map(|(sheet, ivs)| {
                let total: usize = ivs.iter().map(|&(a, b)| (b.saturating_sub(a) as usize) + 1).sum();
                let mut set = std::collections::HashSet::with_capacity(total);
                for (lo, hi) in ivs {
                    if lo <= hi {
                        set.extend(lo..=hi);
                    }
                }
                (sheet, set)
            })
            .collect(),
    )
}

pub(crate) fn encode_manual_hidden_intervals(
    map: &std::collections::BTreeMap<String, Vec<(u32, u32)>>,
) -> Result<String> {
    if map.is_empty() {
        return Ok(String::new());
    }
    let bytes = serde_json::to_vec(map)?;
    Ok(format!("\"SCMH2:{}\"", hex_encode(&bytes)))
}

/// 写入手动隐藏行来源（闭区间 `SCMH2:` 编码）；空表则删除来源定义名。
pub fn write_manual_hidden_intervals(
    path: &Path,
    map: &std::collections::BTreeMap<String, Vec<(u32, u32)>>,
) -> Result<()> {
    let text = encode_manual_hidden_intervals(map)?;
    write_manual_hidden_encoded(path, &text)
}

/// 写入手动隐藏行来源；空表则删除来源定义名。
/// 自动合并为 `SCMH2:` 闭区间，避免百万连续隐藏行膨胀至十余 MB。
pub fn write_manual_hidden(
    path: &Path,
    map: &std::collections::BTreeMap<String, Vec<u32>>,
) -> Result<()> {
    let iv_map: std::collections::BTreeMap<String, Vec<(u32, u32)>> = map
        .iter()
        .filter(|(_, rows)| !rows.is_empty())
        .map(|(k, rows)| (k.clone(), coalesce_sorted_u32_intervals(rows)))
        .collect();
    write_manual_hidden_intervals(path, &iv_map)
}

fn write_manual_hidden_encoded(path: &Path, text: &str) -> Result<()> {
    rewrite_zip_where(
        path,
        |n| n == "xl/workbook.xml",
        |_, buf| {
            let Ok(xml) = String::from_utf8(buf.clone()) else { return buf };
            if text.is_empty() && !xml.contains(MANUAL_HIDDEN_NAME) {
                return xml.into_bytes();
            }
            let mut out = String::with_capacity(xml.len() + text.len() + 80);
            let mut rest = xml.as_str();
            while let Some(i) = rest.find("<definedName ") {
                out.push_str(&rest[..i]);
                let Some(head_end) = rest[i..].find('>').map(|e| i + e) else {
                    out.push_str(&rest[i..]);
                    return out.into_bytes();
                };
                let Some(tail) = rest[head_end..]
                    .find("</definedName>")
                    .map(|e| head_end + e)
                else {
                    out.push_str(&rest[i..]);
                    return out.into_bytes();
                };
                let end = tail + "</definedName>".len();
                if !rest[i..head_end].contains(&format!("name=\"{MANUAL_HIDDEN_NAME}\"")) {
                    out.push_str(&rest[i..end]);
                }
                rest = &rest[end..];
            }
            out.push_str(rest);
            if !text.is_empty() {
                let def = format!(
                    "<definedName name=\"{MANUAL_HIDDEN_NAME}\" hidden=\"1\">{text}</definedName>"
                );
                if let Some(i) = out.find("</definedNames>") {
                    out.insert_str(i, &def);
                } else if let Some((i, tag)) = ["<definedNames/>", "<definedNames />"]
                    .iter()
                    .find_map(|tag| out.find(tag).map(|i| (i, *tag)))
                {
                    out.replace_range(i..i + tag.len(), &format!("<definedNames>{def}</definedNames>"));
                } else if let Some(i) = out.find("</sheets>") {
                    out.insert_str(i + "</sheets>".len(), &format!("<definedNames>{def}</definedNames>"));
                }
            }
            out.into_bytes()
        },
    )
}

/// 关闭筛选后删除 workbook.xml 中指向该表的 _xlnm._FilterDatabase。
/// 只删指定工作表，保留其他表的筛选名称和所有其他内置名称。
pub fn remove_filter_database(path: &Path, sheet_name: &str) -> Result<()> {
    let quoted = format!("'{}'!", sheet_name.replace('\'', "''"));
    let plain = format!("{}!", sheet_name);
    rewrite_zip_where(
        path,
        |n| n == "xl/workbook.xml",
        |_, buf| {
            let Ok(xml) = String::from_utf8(buf.clone()) else {
                return buf;
            };
            if !xml.contains("_xlnm._FilterDatabase") {
                return xml.into_bytes();
            }
            let mut out = String::with_capacity(xml.len());
        let mut rest = xml.as_str();
        while let Some(i) = rest.find("<definedName ") {
            out.push_str(&rest[..i]);
            let Some(head_end) = rest[i..].find('>').map(|e| i + e) else {
                out.push_str(&rest[i..]);
                return out.into_bytes();
            };
            let Some(tail) = rest[head_end..]
                .find("</definedName>")
                .map(|e| head_end + e)
            else {
                out.push_str(&rest[i..]);
                return out.into_bytes();
            };
            let end = tail + "</definedName>".len();
            let head = &rest[i..head_end];
            let text = &rest[head_end + 1..tail];
            let is_filter = head.contains("name=\"_xlnm._FilterDatabase\"");
            // workbook.xml 里的工作表引用是 XML 转义后的（如 A&amp;B），
            // 不能拿原始 sheet_name 直接 starts_with，否则含 & / < / > 的表名删不掉。
            let decoded = crate::model::xml_unescape(text);
            let is_sheet = decoded.starts_with(&quoted) || decoded.starts_with(&plain);
            if !(is_filter && is_sheet) {
                out.push_str(&rest[i..end]);
            }
            rest = &rest[end..];
        }
        out.push_str(rest);
        out.into_bytes()
    })
}

/// 给 <row r="N"> 加/去 hidden 属性
pub fn set_hidden_rows(xml: &str, hidden: &std::collections::HashSet<u32>) -> String {
    let mut out = String::with_capacity(xml.len() + hidden.len() * 12);
    let mut rest = xml;
    while let Some(i) = rest.find("<row ") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(gt) = tail.find('>') else {
            out.push_str(tail);
            return out;
        };
        let tag = &tail[..gt];
        // 取 r 属性
        let mut row_no = 0u32;
        if let Some(pos) = tag.find(" r=\"") {
            let after = &tag[pos + 4..];
            if let Some(endq) = after.find('"') {
                row_no = after[..endq].parse().unwrap_or(0);
            }
        }
        let want_hidden = row_no > 0 && hidden.contains(&(row_no - 1));
        let mut new_tag = String::from(tag);
        // 先去掉已有的 hidden 属性
        while let Some(hp) = new_tag.find(" hidden=\"") {
            if let Some(eq) = new_tag[hp + 9..].find('"') {
                new_tag.replace_range(hp..hp + 9 + eq + 1, "");
            } else {
                break;
            }
        }
        if want_hidden {
            // 自闭合标签要把属性插在 "/" 前面
            if new_tag.ends_with('/') {
                new_tag.pop();
                new_tag.push_str(" hidden=\"1\"/");
            } else {
                new_tag.push_str(" hidden=\"1\"");
            }
        }
        out.push_str(&new_tag);
        out.push('>');
        rest = &tail[gt + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn writes_auto_filter() {
        let mut f = crate::sort_filter::Filter::new(0, 0, 9, 2);
        let mut allow = HashSet::new();
        allow.insert("研发".to_string());
        f.allows.insert(1, allow);
        let xml = auto_filter_xml(&f);
        assert!(xml.contains("ref=\"A1:C10\""));
        assert!(xml.contains("<filterColumn colId=\"1\">"));
        assert!(xml.contains("<filter val=\"研发\"/>"));
        let sheet = r#"<worksheet><sheetData/><mergeCells count="0"/></worksheet>"#;
        let out = insert_auto_filter(sheet, &xml);
        assert!(out.find("<autoFilter").unwrap() < out.find("<mergeCells").unwrap());
        assert_eq!(insert_auto_filter(&out, &xml).matches("<autoFilter").count(), 1);
    }

    #[test]
    fn toggles_hidden_rows() {
        let xml = r#"<worksheet><sheetData><row r="1" spans="1:3"><c r="A1"/></row><row r="5" hidden="1"><c r="A5"/></row><row r="7"/></sheetData></worksheet>"#;
        let mut hidden = HashSet::new();
        hidden.insert(4u32); // 第 5 行
        hidden.insert(6u32); // 第 7 行
        let out = set_hidden_rows(xml, &hidden);
        assert_eq!(out.matches("hidden=\"1\"").count(), 2, "{out}");
        assert!(out.contains("<row r=\"7\" hidden=\"1\"/>"));
        // 清空后应全部去掉 hidden
        let cleared = set_hidden_rows(&out, &HashSet::new());
        assert_eq!(cleared.matches("hidden=\"1\"").count(), 0, "{cleared}");
        assert!(cleared.contains("<row r=\"1\" spans=\"1:3\">"));
    }
}

// ---------------------------------------------------------------------------
// 页面设置
// ---------------------------------------------------------------------------

fn strip_tag(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = rest.as_bytes().get(i + open.len()).copied();
        if !matches!(after, Some(b' ' | b'>' | b'/' | b'\t' | b'\r' | b'\n')) {
            out.push_str(&rest[..i + open.len()]);
            rest = &rest[i + open.len()..];
            continue;
        }
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(gt) = tail.find('>') else {
            rest = "";
            break;
        };
        if tail[..gt].ends_with('/') {
            rest = &tail[gt + 1..];
        } else if let Some(e) = tail[gt + 1..].find(&close) {
            rest = &tail[gt + 1 + e + close.len()..];
        } else {
            rest = &tail[gt + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// 序列化 `<headerFooter>` 元素（含首页不同 / 奇偶页不同）
fn header_footer_xml(ps: &crate::page::PageSetup) -> String {
    let any = !ps.header.is_empty()
        || !ps.footer.is_empty()
        || (ps.different_first_page && (!ps.first_header.is_empty() || !ps.first_footer.is_empty()))
        || (ps.different_odd_even && (!ps.even_header.is_empty() || !ps.even_footer.is_empty()));
    if !any {
        return String::new();
    }
    let mut out = String::from("<headerFooter");
    if ps.different_first_page {
        out.push_str(" differentFirst=\"1\"");
    }
    if ps.different_odd_even {
        out.push_str(" differentOddEven=\"1\"");
    }
    out.push('>');
    if !ps.header.is_empty() {
        out.push_str(&format!("<oddHeader>{}</oddHeader>", esc(&ps.header)));
    }
    if !ps.footer.is_empty() {
        out.push_str(&format!("<oddFooter>{}</oddFooter>", esc(&ps.footer)));
    }
    if ps.different_odd_even {
        if !ps.even_header.is_empty() {
            out.push_str(&format!("<evenHeader>{}</evenHeader>", esc(&ps.even_header)));
        }
        if !ps.even_footer.is_empty() {
            out.push_str(&format!("<evenFooter>{}</evenFooter>", esc(&ps.even_footer)));
        }
    }
    if ps.different_first_page {
        if !ps.first_header.is_empty() {
            out.push_str(&format!("<firstHeader>{}</firstHeader>", esc(&ps.first_header)));
        }
        if !ps.first_footer.is_empty() {
            out.push_str(&format!("<firstFooter>{}</firstFooter>", esc(&ps.first_footer)));
        }
    }
    out.push_str("</headerFooter>");
    out
}

/// 序列化 `<pageSetup .../>` 完整属性（含分页顺序、黑白/草稿、起始页码、批注与错误打印策略）
fn page_setup_attrs_xml(p: &crate::page::PageSetup) -> String {
    let landscape = if p.landscape { "landscape" } else { "portrait" };
    let fit = if p.fit_to_page {
        format!(
            " fitToWidth=\"{}\" fitToHeight=\"{}\"",
            p.fit_width, p.fit_height
        )
    } else {
        String::new()
    };
    let scale = if p.fit_to_page {
        String::new()
    } else {
        format!(" scale=\"{}\"", p.scale.clamp(10, 400))
    };
    let mut extra = String::new();
    if p.page_order_over_then_down {
        extra.push_str(" pageOrder=\"overThenDown\"");
    }
    if p.black_and_white {
        extra.push_str(" blackAndWhite=\"1\"");
    }
    if p.draft {
        extra.push_str(" draft=\"1\"");
    }
    if let Some(fpn) = p.first_page_number {
        extra.push_str(&format!(
            " useFirstPageNumber=\"1\" firstPageNumber=\"{fpn}\""
        ));
    }
    match p.print_comments {
        1 => extra.push_str(" cellComments=\"atEnd\""),
        2 => extra.push_str(" cellComments=\"asDisplayed\""),
        _ => {}
    }
    match p.print_errors {
        1 => extra.push_str(" errors=\"blank\""),
        2 => extra.push_str(" errors=\"dash\""),
        3 => extra.push_str(" errors=\"NA\""),
        _ => {}
    }
    format!(
        "<pageSetup paperSize=\"{}\" orientation=\"{landscape}\"{scale}{fit}{extra}/>",
        p.paper
    )
}

/// 改工作表里的页面设置：严格按 ISO/IEC 29500 Schema 顺序插在 `<drawing>/<tableParts>/<extLst>/</worksheet>` 之前
pub fn set_page_xml(xml: &str, ps: &crate::page::PageSetup) -> String {
    set_page_setup(xml, ps)
}

/// 更新 workbook.xml 里某张表的 Print_Area / Print_Titles
pub fn set_print_names(xml: &str, sheet_index: u32, sheet_name: &str, ps: &crate::page::PageSetup) -> String {
    let mut x = strip_defined_name(xml, "_xlnm.Print_Area", sheet_index);
    x = strip_defined_name(&x, "_xlnm.Print_Titles", sheet_index);
    let quoted = format!("'{}'", sheet_name.replace('\'', "''"));
    let mut defs = String::new();
    if let Some((r1, c1, r2, c2)) = ps.print_area {
        defs.push_str(&format!(
            "<definedName name=\"_xlnm.Print_Area\" localSheetId=\"{sheet_index}\">{}!${}${}:${}${}</definedName>",
            quoted,
            crate::model::col_label(c1),
            r1 + 1,
            crate::model::col_label(c2),
            r2 + 1
        ));
    }
    let mut titles = Vec::new();
    if let Some((r1, r2)) = ps.title_rows {
        titles.push(format!("{}!${}:${}", quoted, r1 + 1, r2 + 1));
    }
    if let Some((c1, c2)) = ps.title_cols {
        titles.push(format!(
            "{}!${}:${}",
            quoted,
            crate::model::col_label(c1),
            crate::model::col_label(c2)
        ));
    }
    if !titles.is_empty() {
        defs.push_str(&format!(
            "<definedName name=\"_xlnm.Print_Titles\" localSheetId=\"{sheet_index}\">{}</definedName>",
            titles.join(",")
        ));
    }
    if defs.is_empty() {
        return x;
    }
    // 已有 definedNames 就插进去，没有就建一个（位置在 sheet 定义之后、calcPr 之前都合法）
    if let Some(i) = x.find("</definedNames>") {
        let mut out = String::with_capacity(x.len() + defs.len());
        out.push_str(&x[..i]);
        out.push_str(&defs);
        out.push_str(&x[i..]);
        out
    } else if let Some(i) = x.find("</sheets>") {
        let mut out = String::with_capacity(x.len() + defs.len() + 40);
        out.push_str(&x[..i + "</sheets>".len()]);
        out.push_str("<definedNames>");
        out.push_str(&defs);
        out.push_str("</definedNames>");
        out.push_str(&x[i + "</sheets>".len()..]);
        out
    } else {
        x
    }
}

fn strip_defined_name(xml: &str, name: &str, sheet_index: u32) -> String {
    let key = format!("name=\"{name}\"");
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("<definedName") {
        // 别把容器 <definedNames> 当成元素
        let after = rest[i + "<definedName".len()..].chars().next();
        if !matches!(after, Some(' ') | Some('>')) {
            out.push_str(&rest[..i + "<definedName".len()]);
            rest = &rest[i + "<definedName".len()..];
            continue;
        }
        let Some(end) = rest[i..].find("</definedName>") else { break };
        let whole = &rest[i..i + end + "</definedName>".len()];
        let matches = whole.contains(&key)
            && (whole.contains(&format!("localSheetId=\"{sheet_index}\"")) || !whole.contains("localSheetId"));
        out.push_str(&rest[..i]);
        if !matches {
            out.push_str(whole);
        }
        rest = &rest[i + end + "</definedName>".len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod page_tests {
    use super::*;
    use crate::page::PageSetup;

    #[test]
    fn rewrites_sheet_page_elements() {
        let xml = "<worksheet><sheetData/><pageMargins left=\"0.7\"/><pageSetup orientation=\"portrait\" paperSize=\"9\" scale=\"100\"/></worksheet>";
        let mut ps = PageSetup::default();
        ps.landscape = true;
        ps.paper = 8;
        ps.scale = 85;
        ps.gridlines = true;
        ps.header = "&C报表".into();
        let out = set_page_xml(xml, &ps);
        assert_eq!(out.matches("<pageSetup").count(), 1);
        assert_eq!(out.matches("<pageMargins").count(), 1);
        assert!(out.contains("orientation=\"landscape\""));
        assert!(out.contains("paperSize=\"8\""));
        assert!(out.contains("scale=\"85\""));
        assert!(out.contains("gridLines=\"1\""));
        assert!(out.contains("<oddHeader>&amp;C报表</oddHeader>"));
        assert!(out.find("<pageSetup").unwrap() < out.find("</worksheet>").unwrap());
    }

    #[test]
    fn writes_print_area_names() {
        let xml = "<workbook><sheets><sheet name=\"数据\" sheetId=\"1\"/></sheets><definedNames><definedName name=\"_xlnm.Print_Area\" localSheetId=\"0\">old</definedName></definedNames></workbook>";
        let mut ps = PageSetup::default();
        ps.print_area = Some((0, 0, 19, 5));
        ps.title_rows = Some((0, 1));
        let out = set_print_names(xml, 0, "数据", &ps);
        assert_eq!(out.matches("Print_Area").count(), 1);
        assert!(out.contains("'数据'!$A$1:$F$20"));
        assert!(out.contains("Print_Titles"));
        assert!(out.contains("$1:$2"));
    }
}

/// 往 zip 里追加一个条目（同名已存在则替换）

// ---------------------------------------------------------------------------
// 暂存模式：把一批部件改动攒在内存里，最后一次重写打包。
// 100 MB 的包每重写一次要解压+压缩几十万行 XML，八次就是上百秒。
// ---------------------------------------------------------------------------
type StageState = (
    Option<std::path::PathBuf>,
    std::collections::HashMap<String, Option<std::rc::Rc<Vec<u8>>>>,
);

thread_local! {
    static STAGE: std::cell::RefCell<Option<StageState>> =
        std::cell::RefCell::new(None);
    static STAGE_TARGETS: std::cell::RefCell<Option<(std::path::PathBuf, HashMap<String, String>)>> =
        const { std::cell::RefCell::new(None) };
}

/// 开始攒改动（嵌套调用只认最外层）
pub fn begin_stage() {
    STAGE.with(|s| {
        let mut b = s.borrow_mut();
        if b.is_none() {
            *b = Some((None, std::collections::HashMap::new()));
        }
    });
}

/// 针对指定目标包开始攒改动：读取其他包（如基准包）时绝不会误命中暂存表
pub fn begin_stage_for(path: &Path) {
    STAGE.with(|s| {
        let mut b = s.borrow_mut();
        if b.is_none() {
            *b = Some((Some(path.to_path_buf()), std::collections::HashMap::new()));
        }
    });
}

pub fn stage_active() -> bool {
    STAGE.with(|s| s.borrow().is_some())
}

pub fn stage_active_for(path: &Path) -> bool {
    STAGE.with(|s| match s.borrow().as_ref() {
        Some((None, _)) => true,
        Some((Some(p), _)) => p == path,
        None => false,
    })
}

fn stage_put(name: &str, body: Vec<u8>) {
    invalidate_target_cache(name);
    STAGE.with(|s| {
        if let Some((_, m)) = s.borrow_mut().as_mut() {
            m.insert(name.to_string(), Some(std::rc::Rc::new(body)));
        }
    });
}

fn stage_del(name: &str) {
    invalidate_target_cache(name);
    STAGE.with(|s| {
        if let Some((_, m)) = s.borrow_mut().as_mut() {
            m.insert(name.to_string(), None);
        }
    });
}

fn invalidate_target_cache(name: &str) {
    if matches!(name, "xl/workbook.xml" | "xl/_rels/workbook.xml.rels") {
        STAGE_TARGETS.with(|cache| cache.borrow_mut().take());
    }
}

/// 暂存里有没有这个部件（读的时候要先看这里，否则读到旧内容）
pub fn staged(name: &str) -> Option<Option<Vec<u8>>> {
    STAGE.with(|s| s.borrow().as_ref().and_then(|(_, m)| m.get(name).map(|b| b.as_deref().cloned())))
}

pub fn staged_at(path: &Path, name: &str) -> Option<Option<Vec<u8>>> {
    staged_shared_at(path, name).map(|entry| entry.as_deref().cloned())
}

// Release the RefCell borrow before invoking a patch: callbacks may themselves
// consult the package. Cloning the handle keeps the staged XML bytes shared.
fn staged_shared_at(path: &Path, name: &str) -> Option<Option<std::rc::Rc<Vec<u8>>>> {
    STAGE.with(|s| {
        s.borrow().as_ref().and_then(|(p, m)| match p {
            Some(target) if target != path => None,
            _ => m.get(name).cloned(),
        })
    })
}

pub fn staged_names() -> Vec<String> {
    STAGE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|(_, m)| m.iter().filter(|(_, v)| v.is_some()).map(|(k, _)| k.clone()).collect())
            .unwrap_or_default()
    })
}

pub fn staged_names_at(path: &Path) -> Vec<String> {
    STAGE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|(p, m)| match p {
                Some(target) if target != path => None,
                _ => Some(
                    m.iter()
                        .filter(|(_, v)| v.is_some())
                        .map(|(k, _)| k.clone())
                        .collect(),
                ),
            })
            .unwrap_or_default()
    })
}

pub fn staged_deleted_at(path: &Path) -> std::collections::HashSet<String> {
    STAGE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|(p, m)| match p {
                Some(target) if target != path => None,
                _ => Some(
                    m.iter()
                        .filter(|(_, v)| v.is_none())
                        .map(|(k, _)| k.clone())
                        .collect(),
                ),
            })
            .unwrap_or_default()
    })
}

/// 异常中断或错误退出时清空线程局部暂存与原样搬运队列，防止污染同线程后续保存
pub fn clear_stage() {
    STAGE_TARGETS.with(|cache| cache.borrow_mut().take());
    STAGE.with(|s| {
        let _ = s.borrow_mut().take();
    });
    RAW_QUEUE.with(|q| {
        q.borrow_mut().clear();
    });
}

/// 一次性落盘：写入/替换/删除都在这一遍里完成
pub fn commit_stage(path: &Path) -> Result<usize> {
    commit_package(path, None, true)
}

pub(crate) fn commit_stage_with_raw_copies(path: &Path, src: &Path) -> Result<usize> {
    commit_package(path, Some(src), true)
}

/// Combine replacements, removals and baseline compressed entries in one ZIP pass.
/// A replacement staged after a raw copy takes precedence over that copy.
fn commit_package(path: &Path, raw_source: Option<&Path>, take_stage: bool) -> Result<usize> {
    use std::collections::{hash_map::Entry, HashMap};

    enum Part {
        Write(std::rc::Rc<Vec<u8>>),
        Remove,
        Copy(usize),
    }

    let mut pending = HashMap::new();
    if take_stage {
        STAGE_TARGETS.with(|cache| cache.borrow_mut().take());
        if let Some((_, map)) = STAGE.with(|s| s.borrow_mut().take()) {
            pending.extend(map.into_iter().map(|(name, body)|
                (name, body.map_or(Part::Remove, Part::Write))));
        }
    }
    let mut source = None;
    if let Some(src) = raw_source {
        let copies = RAW_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
        if !copies.is_empty() {
            let zip = zip::ZipArchive::new(std::fs::File::open(src)?)?;
            for (from, to) in copies.into_iter().rev() {
                if let Entry::Vacant(dest) = pending.entry(to) {
                    let index = zip.index_for_name(&from)
                        .with_context(|| format!("源包里没有 {from}"))?;
                    dest.insert(Part::Copy(index));
                }
            }
            source = Some(zip);
        }
    }
    let count = pending.len();
    if count == 0 { return Ok(0); }

    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let temp = tempfile::NamedTempFile::new_in(path.parent()
        .filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new(".")))?;
    let (file, temp) = temp.into_parts();
    let mut out = zip::ZipWriter::new(BufWriter::new(file));
    let mut write = |out: &mut zip::ZipWriter<BufWriter<std::fs::File>>, name: String, part: Part| -> Result<()> {
        match part {
            Part::Write(body) => {
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated).compression_level(Some(4));
                out.start_file(name, opts)?;
                out.write_all(&body)?;
            }
            Part::Remove => {}
            Part::Copy(index) => {
                let src = source.as_mut().context("缺少原样搬运的基准包")?;
                out.raw_copy_file_rename(src.by_index_raw(index)?, name)?;
            }
        }
        Ok(())
    };
    for i in 0..zip.len() {
        let name = zip.name_for_index(i).context("缺少 ZIP 条目名")?.to_owned();
        if let Some(part) = pending.remove(&name) {
            write(&mut out, name, part)?;
        } else {
            out.raw_copy_file(zip.by_index_raw(i)?)?;
        }
    }
    for (name, part) in pending {
        write(&mut out, name, part)?;
    }
    out.finish()?.flush()?;
    drop(zip);
    drop(source);
    #[cfg(unix)]
    std::fs::set_permissions(&temp, std::fs::metadata(path)?.permissions())?;
    temp.persist(path)?;
    Ok(count)
}


// ---------------------------------------------------------------------------
// 原样搬运：从另一个包里直接拷压缩后的字节，不解压也不重压。
// 大文件里 215 MB 的工作表 XML 省掉「解压 → 再压缩」能省十几秒。
// ---------------------------------------------------------------------------
thread_local! {
    static RAW_QUEUE: std::cell::RefCell<Vec<(String, String)>> =
        std::cell::RefCell::new(Vec::new());
}

/// 记一条「从 src 的 A 部件搬到 dst 的 B 部件」
pub fn stage_raw_copy(src_name: &str, dst_name: &str) {
    STAGE.with(|s| {
        if let Some((_, m)) = s.borrow_mut().as_mut() {
            m.remove(dst_name);
        }
    });
    RAW_QUEUE.with(|q| q.borrow_mut().push((src_name.to_string(), dst_name.to_string())));
}

pub fn raw_pending() -> bool {
    RAW_QUEUE.with(|q| !q.borrow().is_empty())
}

/// 一次性把排队的部件从 `src` 搬到 `dst`（只重写一次 dst）
pub fn apply_raw_copies(dst: &Path, src: &Path) -> Result<usize> {
    commit_package(dst, Some(src), false)
}

pub fn append_entry(path: &Path, name: &str, body: &[u8]) -> Result<()> {
    if stage_active_for(path) {
        stage_put(name, body.to_vec());
        return Ok(());
    }
    use std::io::Write;
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("addtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(BufWriter::new(out));
        let opts = || {
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(4))
        };
        let mut replaced = false;
        for i in 0..zip.len() {
            let same = zip.by_index_raw(i)?.name().to_string();
            if same == name {
                // 目标条目：用新内容覆盖，其余条目原样搬运（不重新压缩）
                let _ = zip.by_index_raw(i)?;
                zw.start_file(same, opts())?;
                zw.write_all(body)?;
                replaced = true;
                continue;
            }
            let e = zip.by_index_raw(i)?;
            zw.raw_copy_file(e)?;
        }
        if !replaced {
            zw.start_file(name.to_string(), opts())?;
            zw.write_all(body)?;
        }
        zw.finish()?.flush()?;
    }
    drop(zip);
    std::fs::rename(&tmp, path)?;
    Ok(())
}


/// 一次重写搞定「写入/替换多个部件」——逐条 append_entry 会把整个包重写 N 遍
pub fn append_entries(path: &Path, items: &[(String, Vec<u8>)]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    if stage_active_for(path) {
        for (n, b) in items {
            stage_put(n, b.clone());
        }
        return Ok(());
    }
    use std::io::Write;
    if items.is_empty() {
        return Ok(());
    }
    let mut left: std::collections::HashMap<&str, &[u8]> =
        items.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("addtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(BufWriter::new(out));
        let opts = || {
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(4))
        };
        for i in 0..zip.len() {
            let name = zip.by_index_raw(i)?.name().to_string();
            if let Some(body) = left.remove(name.as_str()) {
                let _ = zip.by_index_raw(i)?;
                zw.start_file(name, opts())?;
                zw.write_all(body)?;
            } else {
                let e = zip.by_index_raw(i)?;
                zw.raw_copy_file(e)?;
            }
        }
        for (name, body) in items.iter() {
            if left.contains_key(name.as_str()) {
                zw.start_file(name.clone(), opts())?;
                zw.write_all(body)?;
            }
        }
        zw.finish()?.flush()?;
    }
    drop(zip);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 一次重写删掉多个条目
pub fn remove_entries(path: &Path, names: &[String]) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    if stage_active_for(path) {
        for n in names {
            stage_del(n);
        }
        return Ok(());
    }
    let to_drop: std::collections::HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut any_found = false;
    for i in 0..zip.len() {
        let n = zip.by_index_raw(i)?.name().to_string();
        if to_drop.contains(n.as_str()) {
            any_found = true;
            break;
        }
    }
    if !any_found {
        return Ok(());
    }
    let tmp = path.with_extension("rmtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(BufWriter::new(out));
        for i in 0..zip.len() {
            let n = zip.by_index_raw(i)?.name().to_string();
            if to_drop.contains(n.as_str()) {
                continue;
            }
            let e = zip.by_index_raw(i)?;
            zw.raw_copy_file(e)?;
        }
        zw.finish()?.flush()?;
    }
    drop(zip);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 从 zip 里删掉一个条目
/// 部件解压后的大小（读不到就当 0）
pub fn entry_size(path: &Path, name: &str) -> u64 {
    let Ok(file) = std::fs::File::open(path) else { return 0 };
    let Ok(mut z) = zip::ZipArchive::new(file) else { return 0 };
    z.by_name(name).map(|e| e.size()).unwrap_or(0)
}

/// 包里有没有这个部件
pub fn has_entry(path: &Path, name: &str) -> bool {
    match zip::ZipArchive::new(std::fs::File::open(path).unwrap_or_else(|_| std::fs::File::open(".").unwrap())) {
        Ok(mut z) => z.by_name(name).is_ok(),
        Err(_) => false,
    }
}

pub fn remove_entry(path: &Path, name: &str) -> Result<()> {
    remove_entries(path, &[name.to_string()])
}

/// 读 workbook.xml 里的自定义名称（不含 _xlnm.* 这些内置名）
pub fn read_defined_names(path: &Path) -> Vec<(String, Option<usize>, String)> {
    workbook_xml(path).map(|xml| defined_names_xml(&xml)).unwrap_or_default()
}

pub(crate) fn defined_names_xml(xml: &str) -> Vec<(String, Option<usize>, String)> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<definedName ") {
        let head_end = match rest[i..].find('>') {
            Some(e) => i + e,
            None => break,
        };
        let head = &rest[i..head_end];
        let tail = match rest[head_end..].find("</definedName>") {
            Some(e) => head_end + e,
            None => break,
        };
        let text = crate::model::xml_unescape(&rest[head_end + 1..tail]);
        let name = head
            .split("name=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("")
            .to_string();
        let name = crate::model::xml_unescape(&name);
        let local = head
            .split("localSheetId=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .and_then(|s| s.parse::<usize>().ok());
        if !name.is_empty() && !name.starts_with("_xlnm.") {
            out.push((name, local, text));
        }
        rest = &rest[tail + "</definedName>".len()..];
    }
    out
}

/// 覆盖写全部自定义名称，保留 _xlnm.* 内置打印/筛选名称。
pub fn write_user_names(path: &Path, names: &[(String, Option<usize>, String)]) -> Result<()> {
    let previous = read_defined_names(path);
    rewrite_zip_where(path, |n| n == "xl/workbook.xml", |n, buf| {
        if n != "xl/workbook.xml" {
            return buf;
        }
        let Ok(xml) = String::from_utf8(buf.clone()) else { return buf };
        let mut x = xml;
        for (name, _, _) in &previous {
            x = strip_name_everywhere(&x, name);
        }
        let mut defs = String::new();
        for (name, local, text) in names {
            // OOXML 的 definedName 正文不带前导 '='；带了 Excel 会拒绝打开
            let body = text.trim_start_matches('=');
            match local {
                Some(i) => defs.push_str(&format!(
                    "<definedName name=\"{}\" localSheetId=\"{i}\">{}</definedName>",
                    esc(name),
                    esc(body)
                )),
                None => defs.push_str(&format!(
                    "<definedName name=\"{}\">{}</definedName>",
                    esc(name),
                    esc(body)
                )),
            }
        }
        if defs.is_empty() {
            return x.into_bytes();
        }
        if let Some(i) = x.find("</definedNames>") {
            x.insert_str(i, &defs);
        } else if let Some(i) = x.find("</sheets>") {
            x.insert_str(i + "</sheets>".len(), &format!("<definedNames>{defs}</definedNames>"));
        }
        x.into_bytes()
    })
}

fn strip_name_everywhere(xml: &str, name: &str) -> String {
    let key = format!("name=\"{}\"", esc(name));
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("<definedName ") {
        let head_end = match rest[i..].find('>') {
            Some(e) => i + e,
            None => break,
        };
        let tail = match rest[head_end..].find("</definedName>") {
            Some(e) => head_end + e + "</definedName>".len(),
            None => break,
        };
        if rest[i..head_end].contains(&key) {
            out.push_str(&rest[..i]);
        } else {
            out.push_str(&rest[..tail]);
        }
        rest = &rest[tail..];
    }
    out.push_str(rest);
    out
}

/// 把透视表缓存里的 worksheetSource ref 按结构改动搬一下
pub fn shift_pivot_caches(
    path: &Path,
    shifts: &[(String, u8, u32, u32, bool)],
) -> Result<()> {
    use std::io::Read;
    let mut targets: Vec<String> = Vec::new();
    {
        let zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        for i in 0..zip.len() {
            let n = zip.name_for_index(i).unwrap_or("").to_string();
            if n.starts_with("xl/pivotCache/pivotCacheDefinition") && n.ends_with(".xml") {
                targets.push(n);
            }
        }
    }
    if targets.is_empty() {
        return Ok(());
    }
    let mut bodies: Vec<(String, String)> = Vec::new();
    {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        for t in targets.iter() {
            let mut s = String::new();
            if let Ok(mut f) = zip.by_name(t) {
                let _ = f.read_to_string(&mut s);
            }
            if s.is_empty() {
                continue;
            }
            // <worksheetSource ref="A1:C6" sheet="表格"/>
            let mut xml = s;
            for (sheet, axis, at, n, delete) in shifts {
                if let Some(pos) = xml.find("<worksheetSource") {
                    let end = xml[pos..].find("/>").map(|e| pos + e).unwrap_or(xml.len());
                    let block = xml[pos..end].to_string();
                    let sheet_ok = block
                        .split("sheet=\"")
                        .nth(1)
                        .and_then(|s| s.split('"').next())
                        .map(|s| s.eq_ignore_ascii_case(sheet))
                        .unwrap_or(false);
                    if !sheet_ok {
                        continue;
                    }
                    let Some(rtext) = block.split("ref=\"").nth(1).and_then(|s| s.split('"').next()) else {
                        continue;
                    };
                    if let Some((a, b)) = rtext.split_once(':') {
                        let parse = |s: &str| -> Option<(u32, u32)> {
                            let mut col = 0u32;
                            let mut i = 0usize;
                            let bs = s.as_bytes();
                            while i < bs.len() && bs[i].is_ascii_alphabetic() {
                                col = col * 26 + (bs[i].to_ascii_uppercase() - b'A') as u32 + 1;
                                i += 1;
                            }
                            let row: u32 = s[i..].parse().ok()?;
                            if col == 0 || row == 0 {
                                return None;
                            }
                            Some((row - 1, col - 1))
                        };
                        let (Some((r1, c1)), Some((r2, c2))) = (parse(a), parse(b)) else {
                            continue;
                        };
                        let sh = crate::refs::Shift {
                            axis: if *axis == 0 {
                                crate::refs::Axis::Row
                            } else {
                                crate::refs::Axis::Col
                            },
                            at: *at,
                            n: *n,
                            delete: *delete,
                            target_sheet: sheet.clone(),
                            own_sheet: sheet.clone(),
                            translate: None,
                            row_filter: None,
                            col_filter: None,
                        };
                        let (nr1, nc1, nr2, nc2) = if *axis == 0 {
                            match sh.map_range(r1, r2) {
                                Some((x, y)) => (x, c1, y, c2),
                                None => continue,
                            }
                        } else {
                            match sh.map_range(c1, c2) {
                                Some((x, y)) => (r1, x, r2, y),
                                None => continue,
                            }
                        };
                        let newr = format!(
                            "{}{}:{}{}",
                            crate::model::col_label(nc1),
                            nr1 + 1,
                            crate::model::col_label(nc2),
                            nr2 + 1
                        );
                        let nb = block.replace(&format!("ref=\"{rtext}\""), &format!("ref=\"{newr}\""));
                        xml.replace_range(pos..end, &nb);
                    }
                }
            }
            bodies.push((t.clone(), xml));
        }
    }
    for (name, body) in bodies {
        append_entry(path, &name, body.as_bytes())?;
    }
    Ok(())
}

/// umya 只写它理解的部件：图表、drawing 关系、透视表缓存这些会被整份丢掉，
/// 存回去就变成「drawing 指着一个不存在的 chart」——Excel 打开直接报修复。
/// 这里把基准文件里有、新文件里没有的部件原样搬回来，并把内容类型补齐。
pub fn restore_missing_parts(path: &Path, base: &Path) -> Result<Vec<String>> {
    use std::io::Read;
    if !base.exists() {
        return Ok(Vec::new());
    }
    // umya 自己会重写的部件，缺了也不能从旧文件搬（会覆盖新数据）
    let managed = |n: &str| -> bool {
        n == "[Content_Types].xml"
            || n == "xl/workbook.xml"
            || n == "xl/styles.xml"
            || n == "xl/sharedStrings.xml"
            // 工作表可能放在非标准目录（例如 xl/stress/worksheets/sheet1.xml），
            // 这类部件由 umya 重写，不能从旧文件搬回来，否则会留下重复的一整份
            || n.contains("/worksheets/")
            || n.starts_with("xl/worksheets/")
            || n.starts_with("docProps/")
            || n == "_rels/.rels"
            || n == "xl/_rels/workbook.xml.rels"
    };

    let mut base_entries: Vec<(String, Vec<u8>)> = Vec::new();
    {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(base)?)?;
        for i in 0..zip.len() {
            let mut e = zip.by_index(i)?;
            let n = e.name().to_string();
            if managed(&n) {
                continue;
            }
            let mut buf = Vec::new();
            e.read_to_end(&mut buf)?;
            base_entries.push((n, buf));
        }
    }
    if std::env::var("SC_SAVE_PROF").is_ok() {
        eprintln!("[restore] 基准 {} 个非托管部件", base_entries.len());
    }
    let existing: Vec<String> = {
        let zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        (0..zip.len())
            .map(|i| zip.name_for_index(i).unwrap_or("").to_string())
            .collect()
    };
    // 情况特殊：umya 会写出一个「空的」drawing（只有 wsDr 壳，没有锚点），
    // 图表部件虽然找回了，但没有锚点照样显示不出来——空壳就用基准的覆盖
    {
        use std::io::Read;
        let mut replacements: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, body) in base_entries.iter() {
            if !name.starts_with("xl/drawings/") || name.ends_with(".rels") {
                continue;
            }
            // 注意：windows(n) 必须和字面量长度一致，否则永远不相等
            let base_has_anchor = body.windows(10).any(|w| w == b"CellAnchor");
            if !base_has_anchor {
                continue;
            }
            // 图表挂在 graphicFrame 里：umya 会保留锚点却把 graphicFrame 删掉，
            // 结果就是「锚点在、图没了」——这种情况也要用基准覆盖
            let base_has_frame = body.windows(12).any(|w| w == b"graphicFrame");
            let mut cur = String::new();
            if let Ok(mut zip) = zip::ZipArchive::new(std::fs::File::open(path)?) {
                if let Ok(mut f) = zip.by_name(name) {
                    let _ = f.read_to_string(&mut cur);
                }
            }
            let cur_has_frame = cur.contains("graphicFrame");
            if std::env::var("SC_SAVE_PROF").is_ok() {
                eprintln!(
                    "[restore] {name}: 基准{}字节 现有{}字节 base_frame={base_has_frame} cur_frame={cur_has_frame}",
                    body.len(),
                    cur.len()
                );
            }
            if cur.is_empty() || !cur.contains("CellAnch") || (base_has_frame && !cur_has_frame) {
                replacements.push((name.clone(), body.clone()));
            }
        }
        append_entries(path, &replacements)?;
    }

    let mut restored = Vec::new();
    let mut ct_additions = Vec::new();
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, body) in base_entries {
        if existing.iter().any(|n| n == &name) {
            continue;
        }
        restored.push(name.clone());
        // 记录内容类型（从基准文件的 [Content_Types].xml 里抄）
        if let Some(ct) = content_type_of(base, &name) {
            ct_additions.push((name.clone(), ct));
        }
        pending.push((name, body));
    }
    append_entries(path, &pending)?;
    if !ct_additions.is_empty() {
        rewrite_zip_where(
            path,
            |n| n == "[Content_Types].xml",
            |_, buf| {
                let Ok(xml) = String::from_utf8(buf.clone()) else { return buf };
                let mut x = xml;
                for (part, ct) in ct_additions.iter() {
                    let marker = format!("/{part}");
                    if x.contains(&marker) {
                        continue;
                    }
                    let line = format!("<Override PartName=\"{marker}\" ContentType=\"{ct}\"/>");
                    if let Some(i) = x.rfind("</Types>") {
                        x.insert_str(i, &line);
                    }
                }
                x.into_bytes()
            },
        )?;
    }
    Ok(restored)
}

fn content_type_of(path: &Path, part: &str) -> Option<String> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut xml = String::new();
    zip.by_name("[Content_Types].xml").ok()?.read_to_string(&mut xml).ok()?;
    let marker = format!("PartName=\"/{part}\"");
    let i = xml.find(&marker)?;
    let head_start = xml[..i].rfind('<')?;
    let head_end = xml[i..].find('>')? + i;
    let head = &xml[head_start..head_end];
    head.split("ContentType=\"").nth(1).map(|s| s.split('"').next().unwrap_or("").to_string())
}

#[path = "layout_xml.rs"]
pub(crate) mod layout_xml;

/// 把列宽/隐藏列写回 `<cols>`：保留原有的 col 属性（样式等），只改涉及到的列。
/// 目标列如果落在多列的 `<col min..max>` 里，会拆成左/中/右三段。
pub fn set_cols_xml(xml: &str, wanted: &[(u32, f32, bool)]) -> String {
    layout_xml::apply(xml, &layout_xml::Changes::columns(wanted)).unwrap_or_else(|_| xml.into())
}

pub fn set_row_sizes(xml: &str, wanted: &[(u32, Option<f32>, bool)]) -> String {
    layout_xml::apply(xml, &layout_xml::Changes::rows(wanted)).unwrap_or_else(|_| xml.into())
}

#[cfg(test)]
mod moved_dimensions {
    use super::*;

    #[test]
    fn clearing_dimensions_preserves_column_style_row_data_and_unmodeled_defaults() {
        let xml = r#"<worksheet><cols><col min="1" max="2" width="16" customWidth="1" style="3"/><col min="3" max="16384" width="14"/></cols><sheetData><row r="1" ht="36" customHeight="1" s="3"><c r="A1"><v>42</v></c></row></sheetData></worksheet>"#;
        let mut prior = crate::Sheet::default();
        prior.cols.set_many([(0, 133.0), (1, 133.0)]);
        prior.rows.set(0, 48.0);
        let base = layout_xml::Baseline::capture(&prior);
        let edits = layout_xml::Changes::from_sheet(&crate::Sheet::default(), Some(&base));
        let updated = layout_xml::apply(xml, &edits).unwrap();
        let doc = roxmltree::Document::parse(&updated).unwrap();
        let cols: Vec<_> = doc.descendants().filter(|n| n.has_tag_name("col")).collect();
        assert_eq!(cols[0].attribute("style"), Some("3"));
        assert_eq!(cols[0].attribute("width"), None);
        assert_eq!(cols[1].attribute("width"), Some("14"));
        let row = doc.descendants().find(|n| n.has_tag_name("row")).unwrap();
        assert_eq!((row.attribute("ht"), row.attribute("customHeight")), (None, None));
        assert_eq!(row.attribute("s"), Some("3"));
        assert_eq!(row.descendants().find(|n| n.has_tag_name("v")).unwrap().text(), Some("42"));

        let empty = set_row_sizes("<worksheet><sheetData/><pageMargins/></worksheet>", &[(2, Some(48.0), false)]);
        let doc = roxmltree::Document::parse(&empty).unwrap();
        let row = doc.descendants().find(|n| n.has_tag_name("row")).unwrap();
        assert_eq!(row.parent_element().unwrap().tag_name().name(), "sheetData");
        assert_eq!((row.attribute("r"), row.attribute("ht")), (Some("3"), Some("36.00")));
    }
}

/// 给 <row>/<col> 补 outlineLevel（分级显示）。单遍流式扫描，避免几千行分级时二次方 memmove。
pub fn apply_outline(xml: &str, rows: &[(u32, u8)], cols: &[(u32, u8)]) -> String {
    layout_xml::apply(xml, &layout_xml::Changes::outline(rows, cols)).unwrap_or_else(|_| xml.into())
}

/// 写 <ignoredErrors>：被用户忽略错误的单元格（Excel 的「忽略错误」）。
/// 空列表表示清空。元素必须排在 drawing / tableParts / extLst 之前。
pub fn set_ignored_errors(xml: &str, cells: &[(u32, u32)]) -> String {
    let mut x = String::with_capacity(xml.len() + 64);
    // 先删旧的
    let mut rest = xml;
    while let Some(i) = rest.find("<ignoredErrors") {
        x.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(gt) = tail.find('>') {
            if tail[..gt].ends_with('/') {
                rest = &tail[gt + 1..];
            } else if let Some(end) = tail.find("</ignoredErrors>") {
                rest = &tail[end + "</ignoredErrors>".len()..];
            } else {
                rest = &tail[gt + 1..];
            }
        } else {
            rest = tail;
            break;
        }
    }
    x.push_str(rest);
    if cells.is_empty() {
        return x;
    }
    let mut list = cells.to_vec();
    list.sort();
    list.dedup();
    let sqref = list
        .iter()
        .map(|(r, c)| crate::model::cell_name(*r, *c))
        .collect::<Vec<_>>()
        .join(" ");
    let block = format!(
        "<ignoredErrors><ignoredError sqref=\"{sqref}\" evalError=\"1\" numberStoredAsText=\"1\"/></ignoredErrors>"
    );
    let mut at = x.len();
    for tag in ["<smartTags", "<drawing", "<legacyDrawing", "<tableParts", "<extLst"] {
        if let Some(i) = x.find(tag) {
            if i < at {
                at = i;
            }
        }
    }
    if at == x.len() {
        if let Some(i) = x.rfind("</worksheet>") {
            at = i;
        }
    }
    x.insert_str(at, &block);
    x
}

/// 写 `<sheetPr><outlinePr …/></sheetPr>`：分级显示的明细位置与符号开关。
/// schema 顺序是 tabColor → outlinePr → pageSetUpPr，插错位置 Excel 会判损坏。
pub fn set_outline_pr(xml: &str, below: bool, right: bool, symbols: bool) -> String {
    let tag = format!(
        "<outlinePr summaryBelow=\"{}\" summaryRight=\"{}\" showOutlineSymbols=\"{}\"/>",
        if below { 1 } else { 0 },
        if right { 1 } else { 0 },
        if symbols { 1 } else { 0 }
    );
    let mut x = xml.to_string();
    // 先删旧的 outlinePr
    while let Some(i) = x.find("<outlinePr") {
        let tail = &x[i..];
        if let Some(gt) = tail.find('>') {
            if tail[..gt].ends_with('/') {
                x.replace_range(i..i + gt + 1, "");
            } else if let Some(end) = tail.find("</outlinePr>") {
                x.replace_range(i..i + end + "</outlinePr>".len(), "");
            } else {
                x.replace_range(i..i + gt + 1, "");
            }
        } else {
            break;
        }
    }
    if let Some(sp) = x.find("<sheetPr") {
        let Some(gt) = x[sp..].find('>') else {
            return x;
        };
        let end = sp + gt + 1;
        if x[sp..end].ends_with("/>") {
            // <sheetPr/> 自闭合：展开
            let open = x[sp..end].replace("/>", ">");
            x.replace_range(sp..end, &format!("{open}{tag}</sheetPr>"));
        } else {
            // 放在 pageSetUpPr 之前，没有就放 </sheetPr> 前
            let at = x[end..]
                .find("<pageSetUpPr")
                .map(|i| end + i)
                .or_else(|| x[end..].find("</sheetPr>").map(|i| end + i))
                .unwrap_or(end);
            x.insert_str(at, &tag);
        }
    } else if let Some(i) = x.find("<dimension") {
        // 没有 sheetPr：紧跟在 <worksheet …> 之后建一个
        let at = x[..i].rfind('>').map(|p| p + 1).unwrap_or(i);
        x.insert_str(at, &format!("<sheetPr>{tag}</sheetPr>"));
    }
    x
}

/// 保存收尾：把分级设置写回各表
pub fn apply_outline_pr(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    if !wb.sheets.iter().any(|s| s.outline_pr_dirty) {
        return Ok(());
    }
    let targets = sheet_targets(path)?;
    let mut map: std::collections::HashMap<String, (bool, bool, bool)> =
        std::collections::HashMap::new();
    for s in &wb.sheets {
        if !s.outline_pr_dirty {
            continue;
        }
        if let Some(part) = targets.get(&s.name) {
            map.insert(
                part.clone(),
                (
                    s.outline_summary_below,
                    s.outline_summary_right,
                    s.outline_symbols,
                ),
            );
        }
    }
    if map.is_empty() {
        return Ok(());
    }
    rewrite_sheet_metadata(path, &map, |xml, (b, r, sym)| Ok(set_outline_pr(xml, *b, *r, *sym)))
}

/// 保存收尾：把方案列表写回各表（`<scenarios>`）
pub fn apply_scenarios(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    restore_scenarios(path, wb, false)
}

pub(crate) fn restore_scenarios(path: &std::path::Path, wb: &crate::model::Workbook, rebuilt: bool) -> anyhow::Result<()> {
    let needed = |s: &crate::model::Sheet| s.scenarios_dirty
        || rebuilt && (!s.scenarios.is_empty() || s.scenario_settings.has_source());
    if !wb.sheets.iter().any(needed) {
        return Ok(());
    }
    let targets = sheet_targets(path)?;
    let mut map = std::collections::HashMap::new();
    for s in wb.sheets.iter().filter(|s| needed(s)) {
        let part = targets.get(&s.name).context("找不到方案所属的工作表")?;
        map.insert(part.clone(), s);
    }
    if map.is_empty() {
        return Ok(());
    }
    rewrite_sheet_metadata(path, &map, |xml,sheet|
        crate::scenarios::edit_scenarios(xml, &sheet.scenarios, &sheet.scenario_settings))
}

/// 保存收尾：把忽略错误列表写回各表
pub fn apply_ignored_errors(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    if !wb.sheets.iter().any(|s| s.ignored_errors_dirty) {
        return Ok(());
    }
    let targets = sheet_targets(path)?;
    let mut map: std::collections::HashMap<String, Vec<(u32, u32)>> = std::collections::HashMap::new();
    for s in &wb.sheets {
        if !s.ignored_errors_dirty {
            continue;
        }
        if let Some(part) = targets.get(&s.name) {
            let mut v: Vec<(u32, u32)> = s.ignored_errors.iter().copied().collect();
            v.sort();
            map.insert(part.clone(), v);
        }
    }
    if map.is_empty() {
        return Ok(());
    }
    rewrite_sheet_metadata(path, &map, |xml, cells| Ok(set_ignored_errors(xml, cells)))
}

/// 保存收尾：把列宽/行高/隐藏状态补进 sheet XML
pub fn apply_layout(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    if !wb.sheets.iter().any(|s| s.layout_dirty) { return Ok(()); }
    let targets = sheet_targets(path)?;
    let mut changes = std::collections::HashMap::new();
    for sheet in wb.sheets.iter().filter(|s| s.layout_dirty) {
        let part = targets.get(&sheet.name).with_context(|| format!("缺少工作表 {}", sheet.name))?;
        changes.insert(part.clone(), layout_xml::Changes::from_sheet(sheet, sheet.source_cells.as_ref().map(|s| &s.layout)));
    }
    rewrite_zip_borrowed(path, |name| changes.contains_key(name), |name, buf| {
        layout_xml::apply_bytes(buf, &changes[name]).with_context(|| format!("{name}: 写入工作表布局失败"))
    })
}

/// 写工作表标签颜色；None 表示删除颜色
pub fn set_tab_color(xml: &str, rgb: Option<[u8; 3]>) -> String {
    // 先删掉已有的 tabColor
    let mut x = xml.to_string();
    while let Some(i) = x.find("<tabColor") {
        if let Some(gt) = x[i..].find('>') {
            x.replace_range(i..i + gt + 1, "");
        } else {
            break;
        }
    }
    let Some(c) = rgb else { return x };
    let tag = format!(
        "<tabColor rgb=\"FF{:02X}{:02X}{:02X}\"/>",
        c[0], c[1], c[2]
    );
    if let Some(i) = x.find("<sheetPr") {
        let Some(gt) = x[i..].find('>') else {
            return x;
        };
        let end = i + gt;
        if x[i..end].trim_end().ends_with('/') {
            // <sheetPr .../> → <sheetPr ...><tabColor/></sheetPr>
            let slash = match x[i..end].rfind('/') {
                Some(v) => i + v,
                None => return x,
            };
            x.replace_range(slash..slash + 1, "");
            x.insert_str(slash + 1, &format!("{tag}</sheetPr>"));
        } else {
            x.insert_str(end + 1, &tag);
        }
        x
    } else if let Some(ws) = x.find("<worksheet") {
        // schema 要求 sheetPr 是 worksheet 的第一个子元素
        let Some(gt) = x[ws..].find('>') else {
            return x;
        };
        let at = ws + gt + 1;
        x.insert_str(at, &format!("<sheetPr>{tag}</sheetPr>"));
        x
    } else {
        x
    }
}

/// 保存收尾：只给改过颜色的表写 <sheetPr><tabColor>
pub fn apply_tab_colors(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    if !wb.sheets.iter().any(|s| s.tab_color_dirty) {
        return Ok(());
    }
    let targets = sheet_targets(path)?;
    let mut map: std::collections::HashMap<String, Option<[u8; 3]>> = std::collections::HashMap::new();
    for s in &wb.sheets {
        if s.tab_color_dirty {
            if let Some(part) = targets.get(&s.name) {
                map.insert(part.clone(), s.tab_color);
            }
        }
    }
    if map.is_empty() {
        return Ok(());
    }
    rewrite_sheet_metadata(path, &map, |xml, c| Ok(set_tab_color(xml, *c)))
}

/// 和 rewrite_zip 一样，但可以丢弃指定的部件（删除表格/图片等用）
pub fn rewrite_zip_skip<F>(
    path: &Path,
    skip: &std::collections::HashSet<String>,
    f: F,
) -> Result<()>
where
    F: FnMut(&str, Vec<u8>) -> Vec<u8>,
{
    rewrite_zip_skip_where(path, skip, |_| true, f)
}

pub(crate) fn rewrite_zip_skip_where(
    path: &Path,
    skip: &std::collections::HashSet<String>,
    pred: impl Fn(&str) -> bool,
    mut f: impl FnMut(&str, Vec<u8>) -> Vec<u8>,
) -> Result<()> {
    let file = std::fs::File::open(path).with_context(|| format!("打开 {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("patchtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(BufWriter::new(out));
        for i in 0..zip.len() {
            let name = zip.name_for_index(i).context("缺少 ZIP 条目名")?.to_owned();
            if skip.contains(&name) {
                continue;
            }
            if !pred(&name) {
                zw.raw_copy_file(zip.by_index_raw(i)?)?;
                continue;
            }
            let mut buf = Vec::new();
            zip.by_index(i)?.read_to_end(&mut buf)?;
            let updated = f(&name, buf.clone());
            if updated == buf {
                zw.raw_copy_file(zip.by_index_raw(i)?)?;
                continue;
            }
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(4));
            zw.start_file(name, opts)?;
            zw.write_all(&updated)?;
        }
        zw.finish()?.flush()?;
    }
    drop(zip);
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// 写/删工作表保护标签（schema 位置：紧跟 </sheetData> 之后）
pub fn set_sheet_protection(
    xml: &str,
    prot: Option<&crate::protection::SheetProtection>,
) -> String {
    let mut x = xml.to_string();
    while let Some(i) = x.find("<sheetProtection") {
        if let Some(gt) = x[i..].find('>') {
            x.replace_range(i..i + gt + 1, "");
        } else {
            break;
        }
    }
    let Some(p) = prot else { return x };
    let tag = p.to_xml();
    if let Some(i) = x.find("</sheetData>") {
        let at = i + "</sheetData>".len();
        x.insert_str(at, &tag);
    } else if let Some(i) = x.find("<sheetData/>") {
        let at = i + "<sheetData/>".len();
        x.insert_str(at, &tag);
    }
    x
}

/// 保存收尾：只给改过保护状态的表写 <sheetProtection>
pub fn apply_protection(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    if !wb.sheets.iter().any(|s| s.protect_dirty) {
        return Ok(());
    }
    let targets = sheet_targets(path)?;
    let mut map: std::collections::HashMap<
        String,
        Option<crate::protection::SheetProtection>,
    > = std::collections::HashMap::new();
    for s in &wb.sheets {
        if s.protect_dirty {
            if let Some(part) = targets.get(&s.name) {
                map.insert(part.clone(), s.protection.clone());
            }
        }
    }
    if map.is_empty() {
        return Ok(());
    }
    rewrite_sheet_metadata(path, &map, |xml, p| Ok(set_sheet_protection(xml, p.as_ref())))
}

/// 页面设置写回：严格按 OOXML Schema 顺序写回 printOptions / pageMargins / pageSetup / headerFooter / rowBreaks / colBreaks
pub fn set_page_setup(xml: &str, p: &crate::page::PageSetup) -> String {
    let mut x = xml.to_string();
    for tag in ["printOptions", "pageMargins", "pageSetup", "headerFooter", "rowBreaks", "colBreaks"] {
        x = strip_tag(&x, tag);
    }
    let page_setup_el = page_setup_attrs_xml(p);
    let hf_el = header_footer_xml(p);
    let mut block = format!(
        "<printOptions gridLines=\"{}\" headings=\"{}\" horizontalCentered=\"{}\" verticalCentered=\"{}\"/>\
<pageMargins left=\"{}\" right=\"{}\" top=\"{}\" bottom=\"{}\" header=\"{}\" footer=\"{}\"/>\
{page_setup_el}{hf_el}",
        if p.gridlines { 1 } else { 0 },
        if p.headings { 1 } else { 0 },
        if p.center_h { 1 } else { 0 },
        if p.center_v { 1 } else { 0 },
        p.margins[0], p.margins[1], p.margins[2], p.margins[3], p.margins[4], p.margins[5],
    );
    if !p.row_breaks.is_empty() {
        block.push_str(&format!(
            "<rowBreaks count=\"{}\" manualBreakCount=\"{}\">",
            p.row_breaks.len(),
            p.row_breaks.len()
        ));
        for r in &p.row_breaks {
            block.push_str(&format!("<brk id=\"{r}\" max=\"16383\" man=\"1\"/>"));
        }
        block.push_str("</rowBreaks>");
    }
    if !p.col_breaks.is_empty() {
        block.push_str(&format!(
            "<colBreaks count=\"{}\" manualBreakCount=\"{}\">",
            p.col_breaks.len(),
            p.col_breaks.len()
        ));
        for c in &p.col_breaks {
            block.push_str(&format!("<brk id=\"{c}\" max=\"1048575\" man=\"1\"/>"));
        }
        block.push_str("</colBreaks>");
    }
    // fitToPage 由 sheetPr/pageSetUpPr 控制；不关掉的话 Excel 会忽略 scale
    let fit_flag = if p.fit_to_page { "1" } else { "0" };
    if let Some(sp) = x.find("<sheetPr") {
        let end = x[sp..].find('>').map(|e| sp + e + 1).unwrap_or(sp);
        if x[sp..end].ends_with("/>") {
            let inner = format!("<pageSetUpPr fitToPage=\"{fit_flag}\"/>");
            let open = x[sp..end].replace("/>", ">");
            x.replace_range(sp..end, &format!("{open}{inner}</sheetPr>"));
        } else if let Some(psp) = x[sp..].find("<pageSetUpPr") {
            let at = sp + psp;
            let pe = x[at..].find('>').map(|e| at + e).unwrap_or(at);
            let tag = &x[at..=pe];
            let mut newtag = tag.to_string();
            if tag.contains("fitToPage=") {
                let re_start = tag.find("fitToPage=\"").unwrap();
                let v0 = re_start + "fitToPage=\"".len();
                let v1 = tag[v0..].find('"').map(|q| v0 + q).unwrap_or(v0);
                newtag.replace_range(v0..v1, fit_flag);
            } else {
                newtag = newtag.replacen(
                    "<pageSetUpPr",
                    &format!("<pageSetUpPr fitToPage=\"{fit_flag}\" "),
                    1,
                );
            }
            x.replace_range(at..=pe, &newtag);
        } else {
            fn after_child(xml: &str, start: usize, tag: &str) -> Option<usize> {
                let rel = xml.get(start..)?.find(&format!("<{tag}"))?;
                let at = start + rel;
                let tail = &xml[at..];
                let gt = tail.find('>')?;
                if tail[..gt].ends_with('/') {
                    Some(at + gt + 1)
                } else {
                    tail.find(&format!("</{tag}>"))
                        .map(|e| at + e + format!("</{tag}>").len())
                        .or(Some(at + gt + 1))
                }
            }
            let at = after_child(&x, end, "outlinePr")
                .or_else(|| after_child(&x, end, "tabColor"))
                .unwrap_or(end);
            x.insert_str(at, &format!("<pageSetUpPr fitToPage=\"{fit_flag}\"/>"));
        }
    } else if let Some(ws) = x.find("<worksheet")
        && let Some(gt) = x[ws..].find('>')
    {
        let at = ws + gt + 1;
        x.insert_str(
            at,
            &format!("<sheetPr><pageSetUpPr fitToPage=\"{fit_flag}\"/></sheetPr>"),
        );
    }
    let markers = [
        "<customProperties",
        "<cellWatches",
        "<ignoredErrors",
        "<smartTags",
        "<drawing",
        "<legacyDrawing",
        "<picture",
        "<oleObjects",
        "<controls",
        "<webPublishItems",
        "<tableParts",
        "<extLst",
        "</worksheet>",
    ];
    let at = markers
        .iter()
        .filter_map(|m| x.find(m))
        .min()
        .unwrap_or(x.len());
    x.insert_str(at, &block);
    x
}

/// 保存收尾：只给改过页面设置的表写回
pub fn apply_page_setup(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    if !wb.sheets.iter().any(|s| s.page_dirty) {
        return Ok(());
    }
    let targets = sheet_targets(path)?;
    let mut map: std::collections::HashMap<String, crate::page::PageSetup> =
        std::collections::HashMap::new();
    for s in &wb.sheets {
        if s.page_dirty {
            if let Some(part) = targets.get(&s.name) {
                map.insert(part.clone(), s.page.clone());
            }
        }
    }
    if map.is_empty() {
        return Ok(());
    }
    rewrite_sheet_metadata(path, &map, |xml, p| Ok(set_page_setup(xml, p)))
}

/// 保存收尾：把改过的富文本单元格写成 inlineStr
pub fn apply_rich(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    if !wb.sheets.iter().any(|s| s.rich_dirty && !s.rich.is_empty()) {
        return Ok(());
    }
    let targets = sheet_targets(path)?;
    let mut map: std::collections::HashMap<
        String,
        crate::model::CellMetadata<(u32, u32), Vec<crate::richtext::RichRun>>,
    > = std::collections::HashMap::new();
    for s in &wb.sheets {
        if s.rich_dirty && !s.rich.is_empty() {
            if let Some(part) = targets.get(&s.name) {
                map.insert(part.clone(), s.rich.clone());
            }
        }
    }
    if map.is_empty() {
        return Ok(());
    }
    rewrite_sheet_xml(path, &map, |xml, cells| crate::richtext::patch_sheet_rich(xml, cells))
}

/// 读 workbook.xml 里每张表的可见性：true = hidden
pub fn read_sheet_states(path: &std::path::Path) -> std::collections::HashMap<String, bool> {
    workbook_xml(path).map(|xml| sheet_states_xml(&xml)).unwrap_or_default()
}

pub(crate) fn sheet_states_xml(xml: &str) -> std::collections::HashMap<String, bool> {
    let mut out = std::collections::HashMap::new();
    let mut from = 0usize;
    while let Some(i) = xml[from..].find("<sheet ") {
        let i = from + i;
        let Some(gt) = xml[i..].find('>').map(|e| i + e) else { break };
        let tag = &xml[i..=gt];
        from = gt + 1;
        let attr = |name: &str| -> Option<String> {
            let pat = format!("{name}=\"");
            let p = tag.find(&pat)? + pat.len();
            let rest = &tag[p..];
            let q = rest.find('"')?;
            Some(crate::model::xml_unescape(&rest[..q]))
        };
        if let Some(name) = attr("name") {
            out.insert(name, attr("state").as_deref() == Some("hidden"));
        }
    }
    out
}

/// 保存收尾：把工作表可见性写回 workbook.xml（只处理 dirty 的表）
pub fn apply_sheet_states(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    let want: std::collections::HashMap<String, bool> = wb
        .sheets
        .iter()
        .filter(|s| s.hidden_dirty)
        .map(|s| (s.name.clone(), s.hidden))
        .collect();
    if want.is_empty() {
        return Ok(());
    }
    let part = "xl/workbook.xml";
    let bytes = crate::fixups::read_part(path,part).context("缺少工作簿部件")?;
    let xml = crate::xlsx::patch_xml_text(&bytes, part)?;
    // 至少要留一张可见表
    if !wb.sheets.iter().any(|s| !s.hidden) {
        return Err(anyhow::anyhow!("不能把全部工作表都隐藏"));
    }
    let mut out = String::with_capacity(xml.len() + 32);
    let mut rest = xml.as_ref();
    while let Some(i) = rest.find("<sheet ") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(gt) = tail.find('>') else {
            out.push_str(tail);
            rest = "";
            break;
        };
        let tag = &tail[..=gt];
        let attr = |name: &str| -> Option<String> {
            let pat = format!("{name}=\"");
            let p = tag.find(&pat)? + pat.len();
            let r2 = &tag[p..];
            let q = r2.find('"')?;
            Some(r2[..q].to_string())
        };
        let name = attr("name").map(|n| crate::model::xml_unescape(&n));
        match name.as_deref().and_then(|n| want.get(n).map(|h| (n, *h))) {
            Some((_, hide)) => {
                // 去掉旧的 state 属性再按需补
                let mut clean = String::new();
                let mut seg = tag;
                loop {
                    let Some(si) = seg.find(" state=\"") else {
                        clean.push_str(seg);
                        break;
                    };
                    clean.push_str(&seg[..si]);
                    let after = &seg[si + " state=\"".len()..];
                    let Some(q) = after.find('"') else {
                        clean.push_str(&seg[si..]);
                        break;
                    };
                    seg = &after[q + 1..];
                }
                if hide {
                    if let Some(slash) = clean.rfind("/>") {
                        clean.insert_str(slash, " state=\"hidden\"");
                    } else if let Some(gt2) = clean.rfind('>') {
                        clean.insert_str(gt2, " state=\"hidden\"");
                    }
                }
                out.push_str(&clean);
            }
            None => out.push_str(tag),
        }
        rest = &tail[gt + 1..];
    }
    out.push_str(rest);
    crate::patch::append_entry(path, "xl/workbook.xml", out.as_bytes())?;
    Ok(())
}

/// 读取 workbook.xml 的 calcPr/@calcMode；手动模式返回 true。
pub fn read_calc_manual(path: &Path) -> bool {
    workbook_xml(path).is_some_and(|xml| calc_manual_xml(&xml))
}

pub(crate) fn calc_manual_xml(xml: &str) -> bool {
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.local_name().as_ref() == "calcPr" => {
                for a in e.attributes().flatten() {
                    if a.key.local_name().as_ref() == "calcMode" {
                        return a.value.eq_ignore_ascii_case("manual");
                    }
                }
                return false;
            }
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
    }
}

struct ElementSpan {
    start: usize,
    end: usize,
    qname: String,
    self_closing: bool,
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.')
}

/// 按 local_name 找元素，兼容 `<calcPr>` 与 `<x:calcPr>`；跳过注释/CDATA/PI。
fn find_element(xml: &str, local: &str) -> Option<ElementSpan> {
    let bytes = xml.as_bytes();
    let mut i = 0usize;
    while let Some(rel) = xml[i..].find('<') {
        let s = i + rel;
        let rest = &xml[s..];
        if rest.starts_with("<!--") {
            let e = rest.find("-->")?;
            i = s + e + 3;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let e = rest.find("]]>")?;
            i = s + e + 3;
            continue;
        }
        if rest.starts_with("<?") {
            let e = rest.find("?>")?;
            i = s + e + 2;
            continue;
        }
        if rest.starts_with("<!") {
            let e = rest.find('>')?;
            i = s + e + 1;
            continue;
        }
        let mut j = s + 1;
        if bytes.get(j) == Some(&b'/') {
            i = s + 1;
            continue;
        }
        let name_start = j;
        while j < bytes.len() && is_name_byte(bytes[j]) {
            j += 1;
        }
        if j == name_start {
            i = s + 1;
            continue;
        }
        let qname = &xml[name_start..j];
        let local_name = qname.rsplit(':').next().unwrap_or(qname);
        let Some(rel_end) = xml[s..].find('>') else { return None };
        let end = s + rel_end;
        if local_name == local {
            return Some(ElementSpan {
                start: s,
                end,
                qname: qname.to_string(),
                self_closing: xml[s..=end].trim_end().ends_with("/>"),
            });
        }
        i = end + 1;
    }
    None
}

/// 写 workbook.xml 的 calcPr/@calcMode：手动模式加 calcMode="manual"，
/// 自动模式删除该属性（Excel 默认 auto），保留 calcPr 里其它属性。
/// 前缀感知：x:workbook / x:calcPr 不会被字面量匹配漏掉。
pub fn set_calc_mode(xml: &str, manual: bool) -> String {
    let Some(calc) = find_element(xml, "calcPr") else {
        if !manual {
            return xml.to_string();
        }
        let Some(root) = find_element(xml, "workbook") else {
            return xml.to_string();
        };
        let prefix = root.qname.split_once(':').map(|(p, _)| format!("{p}:")).unwrap_or_default();
        let tag = format!("<{prefix}calcPr calcId=\"191029\" calcMode=\"manual\"/>");
        let close = format!("</{}>", root.qname);
        return match xml.rfind(&close) {
            Some(i) => format!("{}{}{}", &xml[..i], tag, &xml[i..]),
            None => xml.to_string(),
        };
    };
    let raw = &xml[calc.start..calc.end];
    let mut tag = if calc.self_closing {
        raw.trim_end().trim_end_matches('/').trim_end().to_string()
    } else {
        raw.trim_end().trim_end_matches('>').trim_end().to_string()
    };
    for key in ["calcMode", "fullCalcOnLoad"] {
        loop {
            let Some(k) = tag.find(key) else { break };
            let after = &tag[k + key.len()..];
            let Some(eq_rel) = after.find('=') else { break };
            if !after[..eq_rel].trim().is_empty() {
                break;
            }
            let attr_start = tag[..k].rfind(char::is_whitespace).map(|i| i).unwrap_or(k);
            let rest = &tag[k + key.len() + eq_rel + 1..];
            let rest = rest.trim_start();
            let quote = rest.chars().next().unwrap_or('"');
            let Some(qend) = rest[1..].find(quote) else { break };
            let end_attr = tag.len() - rest.len() + 1 + qend + 1;
            tag.replace_range(attr_start..end_attr, "");
        }
    }
    if manual {
        tag.push_str(" calcMode=\"manual\"");
    }
    let mut out = String::with_capacity(xml.len() + 32);
    out.push_str(&xml[..calc.start]);
    out.push_str(&tag);
    out.push_str(if calc.self_closing { "/>" } else { ">" });
    out.push_str(&xml[calc.end + 1..]);
    out
}

#[cfg(test)]
mod calc_mode_tests {
    use super::set_calc_mode;

    #[test]
    fn inserts_manual_calc_pr_when_missing() {
        let xml = "<workbook><sheets/></workbook>";
        let out = set_calc_mode(xml, true);
        assert!(out.contains("<calcPr"));
        assert!(out.contains("calcMode=\"manual\""));
        assert!(out.contains("</workbook>"));
    }

    #[test]
    fn replaces_existing_calc_mode_and_drops_full_calc_on_load() {
        let xml = "<workbook><calcPr calcId=\"123\" calcMode=\"auto\" fullCalcOnLoad=\"1\"/></workbook>";
        let out = set_calc_mode(xml, true);
        assert!(out.contains("calcId=\"123\""));
        assert!(out.contains("calcMode=\"manual\""));
        assert!(!out.contains("fullCalcOnLoad"));
        assert_eq!(out.matches("calcMode=").count(), 1);
    }

    #[test]
    fn auto_removes_manual_mode_but_keeps_other_attributes() {
        let xml = "<workbook><calcPr calcId=\"123\" calcMode=\"manual\"/></workbook>";
        let out = set_calc_mode(xml, false);
        assert!(out.contains("calcId=\"123\""));
        assert!(!out.contains("calcMode"));
    }

    #[test]
    fn prefixed_workbook_and_calc_pr_are_handled() {
        let xml = "<x:workbook xmlns:x=\"urn:test\"><x:sheets/><x:calcPr calcId=\"7\"/></x:workbook>";
        let out = set_calc_mode(xml, true);
        assert!(out.contains("<x:calcPr calcId=\"7\" calcMode=\"manual\"/>"), "{out}");
        assert!(!out.contains("<calcPr"), "{out}");
        let back = set_calc_mode(&out, false);
        assert!(!back.contains("calcMode"), "{back}");
        assert!(back.contains("<x:calcPr calcId=\"7\"/>"), "{back}");
    }

    #[test]
    fn non_self_closing_calc_pr_is_not_double_closed() {
        let xml = "<workbook><calcPr calcId=\"1\"></calcPr></workbook>";
        let out = set_calc_mode(xml, true);
        assert_eq!(out.matches("<calcPr").count(), 1);
        assert!(out.contains("calcMode=\"manual\""));
        assert!(out.contains("></calcPr>"));
        assert!(!out.contains(">>"));
    }

    #[test]
    fn prefixed_workbook_without_calc_pr_gets_prefixed_tag() {
        let xml = "<x:workbook xmlns:x=\"urn:test\"><x:sheets/></x:workbook>";
        let out = set_calc_mode(xml, true);
        assert!(out.contains("<x:calcPr calcId=\"191029\" calcMode=\"manual\"/>"), "{out}");
        assert!(!out.contains("<calcPr"), "{out}");
    }
}

#[test]
fn read_prefixed_calc_pr_from_zip() {
    let path = std::env::temp_dir().join(format!(
        "sc_calc_prefix_{}_{}.xlsx",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let file = std::fs::File::create(&path).unwrap();
    let mut zw = zip::ZipWriter::new(file);
    zw.start_file(
        "xl/workbook.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zw.write_all(
        b"<x:workbook xmlns:x=\"urn:test\"><x:calcPr calcId=\"7\" calcMode=\"manual\"/></x:workbook>",
    )
    .unwrap();
    zw.finish().unwrap();
    assert!(read_calc_manual(&path));
    let _ = std::fs::remove_file(&path);
}


// ---- QA-01 特性矩阵带出来的三个缺陷回归 ----

#[test]
fn dxfs_already_at_tail_is_moved_before_table_styles() {
    // umya 写出来的样子：dxfs 排在 <extLst> 后面
    let xml = concat!(
        "<styleSheet><cellXfs/><dxfs count=\"0\"/><tableStyles count=\"0\"/>",
        "<extLst><ext uri=\"{X}\"/></extLst></styleSheet>"
    );
    let dxf = crate::cf::Dxf { bold: true, ..Default::default() };
    let out = replace_dxfs(xml, &[dxf]);
    assert_eq!(out.matches("<dxfs").count(), 1, "只能有一份 dxfs：{out}");
    let d = out.find("<dxfs").unwrap();
    let ts = out.find("<tableStyles").unwrap();
    let ex = out.find("<extLst").unwrap();
    assert!(d < ts && d < ex, "dxfs 必须在 tableStyles/extLst 之前：{out}");
    assert!(out.contains("count=\"1\""), "{out}");
}

#[test]
fn empty_dxfs_placeholder_is_replaced_not_duplicated() {
    let xml = "<styleSheet><cellXfs/><dxfs count=\"0\"/><tableStyles/></styleSheet>";
    let out = replace_dxfs(xml, &[]);
    assert_eq!(out.matches("<dxfs").count(), 1, "{out}");
    assert!(out.contains("<dxfs count=\"0\"></dxfs>"), "{out}");
}

#[test]
fn freeze_on_sheet_without_sheet_views_creates_them() {
    // 新建工作簿的 sheet XML 没有 <sheetViews>，以前冻结直接丢掉
    let xml = concat!(
        "<worksheet><dimension ref=\"A1\"/><sheetFormatPr defaultRowHeight=\"15\"/>",
        "<cols><col min=\"1\" max=\"1\" width=\"9\"/></cols><sheetData/></worksheet>"
    );
    let out = set_freeze_xml(xml, Some((1, 2)));
    assert!(out.contains("<sheetViews><sheetView workbookViewId=\"0\">"), "{out}");
    assert!(
        out.contains("<pane xSplit=\"2\" ySplit=\"1\" topLeftCell=\"C2\""),
        "{out}"
    );
    // 顺序：sheetViews 在 sheetFormatPr 之前、sheetData 之前
    let v = out.find("<sheetViews").unwrap();
    let f = out.find("<sheetFormatPr").unwrap();
    let d = out.find("<sheetData").unwrap();
    assert!(v < f && v < d, "{out}");
    // 清除时整段仍然合法
    let cleared = set_freeze_xml(&out, None);
    assert!(!cleared.contains("<pane"), "{cleared}");
    assert!(cleared.contains("<sheetViews>"), "{cleared}");
}

/// 读取 workbook.xml 的迭代计算设置：`calcPr/@iterate`、`@maxIterations`、`@maxChange`。
/// 没有 calcPr 或没有 iterate 属性时返回 None（Excel 默认关闭迭代）。
pub fn read_calc_iteration(path: &Path) -> Option<(bool, u32, f64)> {
    calc_iteration_xml(&workbook_xml(path)?)
}

pub(crate) fn calc_iteration_xml(xml: &str) -> Option<(bool, u32, f64)> {
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.local_name().as_ref() == "calcPr" => {
                let mut iterate: Option<bool> = None;
                let mut max_iter = 100u32;
                let mut max_change = 0.001f64;
                for a in e.attributes().flatten() {
                    let v = a.value;
                    match a.key.local_name().as_ref() {
                        "iterate" => iterate = Some(v == "1" || v.eq_ignore_ascii_case("true")),
                        "maxIterations" => {
                            if let Ok(n) = v.parse::<u32>() {
                                max_iter = n.max(1);
                            }
                        }
                        "maxChange" => {
                            if let Ok(n) = v.parse::<f64>() {
                                max_change = n.max(0.0);
                            }
                        }
                        _ => {}
                    }
                }
                return iterate.map(|i| (i, max_iter, max_change));
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

/// 写 calcPr 的迭代计算三属性；`iterate=false` 时删掉这三个属性（Excel 默认关闭）。
/// 前缀感知，保留 calcPr 里其它属性（calcMode/calcId/fullCalcOnLoad 等）。
pub fn set_calc_iteration(xml: &str, iterate: bool, max_iterations: u32, max_change: f64) -> String {
    let Some(calc) = find_element(xml, "calcPr") else {
        if !iterate {
            return xml.to_string();
        }
        let Some(root) = find_element(xml, "workbook") else {
            return xml.to_string();
        };
        let prefix = root.qname.split_once(':').map(|(p, _)| format!("{p}:")).unwrap_or_default();
        let tag = format!(
            "<{prefix}calcPr calcId=\"191029\" iterate=\"1\" maxIterations=\"{}\" maxChange=\"{}\"/>",
            max_iterations.max(1),
            fmt_change(max_change)
        );
        let close = format!("</{}>", root.qname);
        return match xml.rfind(&close) {
            Some(i) => format!("{}{}{}", &xml[..i], tag, &xml[i..]),
            None => xml.to_string(),
        };
    };
    let raw = &xml[calc.start..calc.end];
    let mut tag = if calc.self_closing {
        raw.trim_end().trim_end_matches('/').trim_end().to_string()
    } else {
        raw.trim_end().trim_end_matches('>').trim_end().to_string()
    };
    for key in ["iterate", "maxIterations", "maxChange"] {
        while let Some(k) = tag.find(key) {
            let after = &tag[k + key.len()..];
            let Some(eq_rel) = after.find('=') else { break };
            if !after[..eq_rel].trim().is_empty() {
                break;
            }
            let attr_start = tag[..k].rfind(char::is_whitespace).unwrap_or(k);
            let mut end = k + key.len() + eq_rel + 1;
            let bytes = tag.as_bytes();
            let mut quote = None;
            for (i, b) in bytes.iter().enumerate().skip(end) {
                if *b == b'"' || *b == b'\'' {
                    if quote.is_none() {
                        quote = Some(*b);
                    } else if Some(*b) == quote {
                        end = i + 1;
                        break;
                    }
                }
            }
            tag.replace_range(attr_start..end.min(tag.len()), "");
            if !tag.trim_end().ends_with('/') && tag.trim_end().ends_with('>') {
                tag = tag.trim_end().trim_end_matches('>').trim_end().to_string();
            }
        }
    }
    if iterate {
        tag.push_str(&format!(
            " iterate=\"1\" maxIterations=\"{}\" maxChange=\"{}\"",
            max_iterations.max(1),
            fmt_change(max_change)
        ));
    }
    tag.push_str(if calc.self_closing { "/>" } else { ">" });
    format!("{}{}{}", &xml[..calc.start], tag, &xml[calc.end + 1..])
}

fn fmt_change(v: f64) -> String {
    let s = format!("{v}");
    if s.contains('e') || s.contains('E') { format!("{v:.10}").trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

#[cfg(test)]
mod scmh2_tests {
    use super::*;

    #[test]
    fn scmh2_manual_hidden_intervals_and_scmh1_backward_compat() {
        let mut map = std::collections::BTreeMap::new();
        map.insert("Sheet1".to_string(), vec![(0, 4), (9, 9), (20, 100_000)]);
        let encoded = encode_manual_hidden_intervals(&map).unwrap();
        assert!(encoded.starts_with("\"SCMH2:"));

        let names = vec![(MANUAL_HIDDEN_NAME.to_string(), None, encoded)];
        let ivs = manual_hidden_intervals_from_names(&names).unwrap();
        assert_eq!(ivs["Sheet1"], vec![(0, 4), (9, 9), (20, 100_000)]);

        // Legacy SCMH1 backward compatibility
        let mut old_map = std::collections::BTreeMap::new();
        old_map.insert("Sheet1".to_string(), vec![1u32, 3, 4]);
        let old_hex = hex_encode(&serde_json::to_vec(&old_map).unwrap());
        let old_names = vec![(
            MANUAL_HIDDEN_NAME.to_string(),
            None,
            format!("\"SCMH1:{old_hex}\""),
        )];
        let old_ivs = manual_hidden_intervals_from_names(&old_names).unwrap();
        assert_eq!(old_ivs["Sheet1"], vec![(1, 1), (3, 4)]);
        let legacy_sets = manual_hidden_from_names(&old_names).unwrap();
        assert!(
            legacy_sets["Sheet1"].contains(&1)
                && legacy_sets["Sheet1"].contains(&3)
                && legacy_sets["Sheet1"].contains(&4)
        );
    }
}
