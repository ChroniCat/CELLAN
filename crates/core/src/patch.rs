//! 保存后的 XML 补丁：把条件格式 / 数据验证这类 umya 写不干净的结构直接补进 xlsx。
//!
//! 做法是重写 zip：读出每个 entry，命中目标文件就替换内容，再整体写回。
//! 这样不会碰 umya 已经写好的其余部分，也不用担心它内部模型覆盖我们。

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;

/// workbookPr/@date1904：true/1 都算（Excel 写 "1"，有些工具写 "true"）
pub fn read_date1904(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else { return false };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return false };
    let Ok(mut f) = zip.by_name("xl/workbook.xml") else { return false };
    let mut xml = String::new();
    if f.read_to_string(&mut xml).is_err() {
        return false;
    }
    let mut r = Reader::from_str(&xml);
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

/// 表名 → zip 内的 worksheet 路径
pub fn sheet_targets(path: &Path) -> Result<HashMap<String, String>> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;

    let mut order: Vec<(String, String)> = Vec::new();
    if let Ok(mut f) = zip.by_name("xl/workbook.xml") {
        let mut xml = String::new();
        let _ = f.read_to_string(&mut xml);
        let mut r = Reader::from_str(&xml);
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
    if let Ok(mut f) = zip.by_name("xl/_rels/workbook.xml.rels") {
        let mut xml = String::new();
        let _ = f.read_to_string(&mut xml);
        let mut r = Reader::from_str(&xml);
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
    Ok(out)
}

/// 重写 zip：对每个 entry 调用 f，返回新内容
pub fn rewrite_zip<F>(path: &Path, mut f: F) -> Result<()>
where
    F: FnMut(&str, Vec<u8>) -> Vec<u8>,
{
    let file = std::fs::File::open(path).with_context(|| format!("打开 {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("patchtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(out);
        for i in 0..zip.len() {
            let (name, mut buf) = {
                let mut e = zip.by_index(i)?;
                let name = e.name().to_string();
                let mut buf = Vec::new();
                e.read_to_end(&mut buf)?;
                (name, buf)
            };
            let orig = buf.clone();
            buf = f(&name, buf);
            if buf == orig {
                // 没改动的部件原样拷过去：不重新压缩（省掉整包重编码，也保住原压缩率）
                let e = zip.by_index_raw(i)?;
                zw.raw_copy_file(e)?;
            } else {
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(Some(4));
                zw.start_file(name, opts)?;
                zw.write_all(&buf)?;
            }
        }
        zw.finish()?;
    }
    drop(zip);
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
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
                    // Excel 里包含文本还带一个 SEARCH 公式，用于部分阅读器
                    let anchor = b.ranges.first().map(|r| format!("{}{}", crate::model::col_label(r.1), r.0 + 1)).unwrap_or_else(|| "A1".into());
                    out.push_str(&format!(
                        "<formula>NOT(ISERROR(SEARCH(\"{}\",{})))</formula>",
                        esc(contains),
                        anchor
                    ));
                    out.push_str("</cfRule>");
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
                CfKind::Bar { color, min, max } => {
                    out.push_str(&format!("<cfRule type=\"dataBar\" priority=\"{prio}\"{stop}><dataBar>"));
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
    if blocks_xml.is_empty() {
        return xml.to_string();
    }
    let xml = strip_cf(xml);
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

    // 已有 dxfs：整段替换
    if let Some(start) = xml.find("<dxfs") {
        let end = xml[start..]
            .find("</dxfs>")
            .map(|i| start + i + "</dxfs>".len())
            .or_else(|| xml[start..].find("/>").map(|i| start + i + 2));
        if let Some(end) = end {
            let mut out = String::with_capacity(xml.len() + body.len());
            out.push_str(&xml[..start]);
            out.push_str(&body);
            out.push_str(&xml[end..]);
            return out;
        }
    }
    // 没有：插在 </styleSheet> 前
    if let Some(i) = xml.find("</styleSheet>") {
        let mut out = String::with_capacity(xml.len() + body.len());
        out.push_str(&xml[..i]);
        out.push_str(&body);
        out.push_str(&xml[i..]);
        return out;
    }
    xml.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            DvKind::List if !d.f1.trim_start().starts_with('"') && !d.f1.contains('!') && !d.f1.contains(':') => {
                format!("\"{}\"", d.f1)
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
    let Some(i) = found else { return base };
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
    let (r1, c1, r2, c2) = f.range;
    let mut out = format!(
        "<autoFilter ref=\"{}{}:{}{}\">",
        crate::model::col_label(c1),
        r1 + 1,
        crate::model::col_label(c2),
        r2 + 1
    );
    let mut cols: Vec<u32> = f.allows.keys().copied().collect();
    for c in f.criteria.keys() {
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
        if let Some(crit) = f.criteria.get(&col) {
            // 条件筛选：介于写成两条 and="1"
            out.push_str(&format!(
                "<filterColumn colId=\"{col_id}\"><customFilters and=\"1\">"
            ));
            match crit.op {
                crate::sort_filter::CmpOp::Between => {
                    let hi = crit.val2.clone().unwrap_or_default();
                    out.push_str(&format!(
                        "<customFilter operator=\"greaterThanOrEqual\" val=\"{}\"/>",
                        esc(&crit.val)
                    ));
                    out.push_str(&format!(
                        "<customFilter operator=\"lessThanOrEqual\" val=\"{}\"/>",
                        esc(&hi)
                    ));
                }
                op => out.push_str(&format!(
                    "<customFilter operator=\"{}\" val=\"{}\"/>",
                    op.xml_operator(),
                    esc(&crit.val)
                )),
            }
            out.push_str("</customFilters></filterColumn>");
            continue;
        }
        let Some(set) = f.allows.get(&col) else {
            continue;
        };
        out.push_str(&format!("<filterColumn colId=\"{col_id}\"><filters>"));
        let mut vals: Vec<&String> = set.iter().collect();
        vals.sort();
        for v in vals {
            out.push_str(&format!("<filter val=\"{}\"/>", esc(v)));
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

/// 读本应用写入的手动隐藏行来源；无定义名时返回 None。
pub fn read_manual_hidden(
    path: &Path,
) -> Option<std::collections::HashMap<String, std::collections::HashSet<u32>>> {
    let (_, _, text) = read_defined_names(path)
        .into_iter()
        .find(|(name, _, _)| name == MANUAL_HIDDEN_NAME)?;
    let text = text.trim().trim_start_matches('=').trim();
    let text = text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(text);
    let hex = text.strip_prefix("SCMH1:")?;
    let bytes = hex_decode(hex)?;
    let map: std::collections::BTreeMap<String, Vec<u32>> = serde_json::from_slice(&bytes).ok()?;
    Some(
        map.into_iter()
            .map(|(sheet, rows)| (sheet, rows.into_iter().collect()))
            .collect(),
    )
}

/// 写入手动隐藏行来源；空表则删除来源定义名。
/// 使用隐藏的工作簿级 definedName；内容用 hex 包装，避免表名和 JSON 字符串影响 definedName 语法。
pub fn write_manual_hidden(
    path: &Path,
    map: &std::collections::BTreeMap<String, Vec<u32>>,
) -> Result<()> {
    let text = if map.is_empty() {
        String::new()
    } else {
        let bytes = serde_json::to_vec(map)?;
        format!("\"SCMH1:{}\"", hex_encode(&bytes))
    };
    rewrite_zip(path, |n, buf| {
        if n != "xl/workbook.xml" {
            return buf;
        }
        let Ok(xml) = String::from_utf8(buf.clone()) else { return buf };
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
    })
}

/// 关闭筛选后删除 workbook.xml 中指向该表的 _xlnm._FilterDatabase。
/// 只删指定工作表，保留其他表的筛选名称和所有其他内置名称。
pub fn remove_filter_database(path: &Path, sheet_name: &str) -> Result<()> {
    let quoted = format!("'{}'!", sheet_name.replace('\'', "''"));
    let plain = format!("{}!", sheet_name);
    rewrite_zip(path, |n, buf| {
        if n != "xl/workbook.xml" {
            return buf;
        }
        let Ok(xml) = String::from_utf8(buf.clone()) else {
            return buf;
        };
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
        out.push_str(&rest[..i]);
        if let Some(e) = rest[i..].find(&close) {
            rest = &rest[i + e + close.len()..];
        } else if let Some(e) = rest[i..].find("/>") {
            rest = &rest[i + e + 2..];
        } else {
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// 改工作表里的页面设置：四段一起重写，插在 </worksheet> 前（顺序按 schema）
pub fn set_page_xml(xml: &str, ps: &crate::page::PageSetup) -> String {
    let mut x = xml.to_string();
    for t in ["printOptions", "pageMargins", "pageSetup", "headerFooter"] {
        x = strip_tag(&x, t);
    }
    let mut body = String::new();
    if ps.gridlines || ps.headings || ps.center_h || ps.center_v {
        body.push_str(&format!(
            "<printOptions gridLines=\"{}\" headings=\"{}\" horizontalCentered=\"{}\" verticalCentered=\"{}\"/>",
            if ps.gridlines { 1 } else { 0 },
            if ps.headings { 1 } else { 0 },
            if ps.center_h { 1 } else { 0 },
            if ps.center_v { 1 } else { 0 }
        ));
    }
    body.push_str(&format!(
        "<pageMargins left=\"{}\" right=\"{}\" top=\"{}\" bottom=\"{}\" header=\"{}\" footer=\"{}\"/>",
        ps.margins[0], ps.margins[1], ps.margins[2], ps.margins[3], ps.margins[4], ps.margins[5]
    ));
    body.push_str(&format!(
        "<pageSetup paperSize=\"{}\" orientation=\"{}\" {} fitToWidth=\"{}\" fitToHeight=\"{}\" scale=\"{}\"/>",
        ps.paper,
        if ps.landscape { "landscape" } else { "portrait" },
        "",
        if ps.fit_to_page { ps.fit_width } else { 0 },
        if ps.fit_to_page { ps.fit_height } else { 0 },
        if ps.fit_to_page { 100 } else { ps.scale }
    ));
    if !ps.header.is_empty() || !ps.footer.is_empty() {
        body.push_str("<headerFooter>");
        if !ps.header.is_empty() {
            body.push_str(&format!("<oddHeader>{}</oddHeader>", esc(&ps.header)));
        }
        if !ps.footer.is_empty() {
            body.push_str(&format!("<oddFooter>{}</oddFooter>", esc(&ps.footer)));
        }
        body.push_str("</headerFooter>");
    }
    if let Some(i) = x.find("</worksheet>") {
        let mut out = String::with_capacity(x.len() + body.len());
        out.push_str(&x[..i]);
        out.push_str(&body);
        out.push_str(&x[i..]);
        return out;
    }
    x.push_str(&body);
    x
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
thread_local! {
    static STAGE: std::cell::RefCell<Option<std::collections::HashMap<String, Option<Vec<u8>>>>> =
        std::cell::RefCell::new(None);
}

/// 开始攒改动（嵌套调用只认最外层）
pub fn begin_stage() {
    STAGE.with(|s| {
        let mut b = s.borrow_mut();
        if b.is_none() {
            *b = Some(std::collections::HashMap::new());
        }
    });
}

pub fn stage_active() -> bool {
    STAGE.with(|s| s.borrow().is_some())
}

fn stage_put(name: &str, body: Vec<u8>) {
    STAGE.with(|s| {
        if let Some(m) = s.borrow_mut().as_mut() {
            m.insert(name.to_string(), Some(body));
        }
    });
}

fn stage_del(name: &str) {
    STAGE.with(|s| {
        if let Some(m) = s.borrow_mut().as_mut() {
            m.insert(name.to_string(), None);
        }
    });
}

/// 暂存里有没有这个部件（读的时候要先看这里，否则读到旧内容）
pub fn staged(name: &str) -> Option<Option<Vec<u8>>> {
    STAGE.with(|s| s.borrow().as_ref().and_then(|m| m.get(name).cloned()))
}

pub fn staged_names() -> Vec<String> {
    STAGE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|m| m.iter().filter(|(_, v)| v.is_some()).map(|(k, _)| k.clone()).collect())
            .unwrap_or_default()
    })
}

/// 一次性落盘：写入/替换/删除都在这一遍里完成
pub fn commit_stage(path: &Path) -> Result<usize> {
    use std::io::Write;
    let Some(map) = STAGE.with(|s| s.borrow_mut().take()) else {
        return Ok(0);
    };
    if map.is_empty() {
        return Ok(0);
    }
    let mut left = map;
    let n = left.len();
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("stgtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(out);
        let opts = || {
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(4))
        };
        for i in 0..zip.len() {
            let name = zip.by_index_raw(i)?.name().to_string();
            match left.remove(name.as_str()) {
                Some(Some(body)) => {
                    let _ = zip.by_index_raw(i)?;
                    zw.start_file(name, opts())?;
                    zw.write_all(&body)?;
                }
                Some(None) => { /* 删除：跳过不写 */ }
                None => {
                    let e = zip.by_index_raw(i)?;
                    zw.raw_copy_file(e)?;
                }
            }
        }
        for (name, body) in left.into_iter() {
            if let Some(body) = body {
                zw.start_file(name, opts())?;
                zw.write_all(&body)?;
            }
        }
        zw.finish()?;
    }
    drop(zip);
    std::fs::rename(&tmp, path)?;
    Ok(n)
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
    RAW_QUEUE.with(|q| q.borrow_mut().push((src_name.to_string(), dst_name.to_string())));
}

pub fn raw_pending() -> bool {
    RAW_QUEUE.with(|q| !q.borrow().is_empty())
}

/// 一次性把排队的部件从 `src` 搬到 `dst`（只重写一次 dst）
pub fn apply_raw_copies(dst: &Path, src: &Path) -> Result<usize> {
    let items: Vec<(String, String)> = RAW_QUEUE.with(|q| q.borrow_mut().drain(..).collect());
    if items.is_empty() {
        return Ok(0);
    }
    use std::io::Write;
    let map: std::collections::HashMap<String, String> =
        items.iter().map(|(s, d)| (d.clone(), s.clone())).collect();
    let mut src_zip = zip::ZipArchive::new(std::fs::File::open(src)?)?;
    // 先建名字 → 序号索引（zip 只提供按序号的原样读）
    let mut src_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for i in 0..src_zip.len() {
        if let Ok(e) = src_zip.by_index_raw(i) {
            src_index.insert(e.name().to_string(), i);
        }
    }
    let dst_file = std::fs::File::open(dst)?;
    let mut dst_zip = zip::ZipArchive::new(dst_file)?;
    let tmp = dst.with_extension("rawtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(out);
        let opts = || {
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(4))
        };
        for i in 0..dst_zip.len() {
            let name = dst_zip.by_index_raw(i)?.name().to_string();
            if let Some(src_name) = map.get(&name) {
                let idx = *src_index.get(src_name.as_str()).ok_or_else(|| {
                    anyhow::anyhow!("源包里没有 {src_name}")
                })?;
                let e = src_zip.by_index_raw(idx)?;
                zw.raw_copy_file_rename(e, name.clone())?;
            } else {
                let e = dst_zip.by_index_raw(i)?;
                zw.raw_copy_file(e)?;
            }
        }
        // 目标包里没有的，补在末尾
        let existing: std::collections::HashSet<String> =
            (0..dst_zip.len()).filter_map(|i| dst_zip.by_index_raw(i).ok().map(|e| e.name().to_string())).collect();
        for (_, d) in items.iter() {
            if existing.contains(d) {
                continue;
            }
            if let Some(idx) = src_index.get(d.as_str()).copied() {
                if let Ok(e) = src_zip.by_index_raw(idx) {
                    zw.raw_copy_file_rename(e, d.clone())?;
                }
            }
        }
        zw.finish()?;
    }
    drop(dst_zip);
    drop(src_zip);
    std::fs::rename(&tmp, dst)?;
    Ok(items.len())
}

pub fn append_entry(path: &Path, name: &str, body: &[u8]) -> Result<()> {
    if stage_active() {
        stage_put(name, body.to_vec());
        return Ok(());
    }
    use std::io::Write;
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("addtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(out);
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
        zw.finish()?;
    }
    drop(zip);
    std::fs::rename(&tmp, path)?;
    Ok(())
}


/// 一次重写搞定「写入/替换多个部件」——逐条 append_entry 会把整个包重写 N 遍
pub fn append_entries(path: &Path, items: &[(String, Vec<u8>)]) -> Result<()> {
    if stage_active() {
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
        let mut zw = zip::ZipWriter::new(out);
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
        zw.finish()?;
    }
    drop(zip);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 一次重写删掉多个条目
pub fn remove_entries(path: &Path, names: &[String]) -> Result<()> {
    use std::io::{Read, Write};
    if names.is_empty() {
        return Ok(());
    }
    let to_drop: std::collections::HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("rmtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(out);
        let opts = || {
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(4))
        };
        for i in 0..zip.len() {
            let (n, buf) = {
                let mut e = zip.by_index(i)?;
                let n = e.name().to_string();
                let mut b = Vec::new();
                e.read_to_end(&mut b)?;
                (n, b)
            };
            if to_drop.contains(n.as_str()) {
                continue;
            }
            zw.start_file(n.clone(), opts())?;
            zw.write_all(&buf)?;
        }
        zw.finish()?;
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
    if stage_active() {
        stage_del(name);
        return Ok(());
    }
    use std::io::{Read, Write};
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("rmtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(out);
        let opts = || {
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(4))
        };
        for i in 0..zip.len() {
            let (n, buf) = {
                let mut e = zip.by_index(i)?;
                let n = e.name().to_string();
                if n == name {
                    continue;
                }
                let mut b = Vec::new();
                e.read_to_end(&mut b)?;
                (n, b)
            };
            zw.start_file(n, opts())?;
            zw.write_all(&buf)?;
        }
        zw.finish()?;
    }
    drop(zip);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 读 workbook.xml 里的自定义名称（不含 _xlnm.* 这些内置名）
pub fn read_defined_names(path: &Path) -> Vec<(String, Option<usize>, String)> {
    use std::io::Read;
    let mut out = Vec::new();
    let Ok(mut zip) = zip::ZipArchive::new(std::fs::File::open(path).unwrap_or_else(|_| std::fs::File::open("nul").unwrap())) else {
        return out;
    };
    let mut xml = String::new();
    if zip.by_name("xl/workbook.xml").map(|mut f| f.read_to_string(&mut xml)).is_err() {
        return out;
    }
    let mut rest = xml.as_str();
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
    rewrite_zip(path, |n, buf| {
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
        rewrite_zip(path, |n, buf| {
            if n != "[Content_Types].xml" {
                return buf;
            }
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
        })?;
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

/// 解析一个 XML 标签里的属性（值可能有引号、允许 `/>` 结尾）
fn xml_attrs(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = tag.as_bytes();
    // 跳过元素名（`<row r="1"` → 从第一个空格开始）
    let mut i = tag.find(char::is_whitespace).unwrap_or(0);
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        let ks = i;
        while i < bytes.len() && bytes[i] != b'=' && bytes[i] != b'/' && bytes[i] != b'>' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            break;
        }
        let key = tag[ks..i].trim().to_string();
        i += 1;
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let quote = bytes[i];
        if quote == b'"' || quote == b'\'' {
            i += 1;
            let vs = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            out.push((key, tag[vs..i].to_string()));
            i += 1;
        } else {
            break;
        }
    }
    out
}

fn with_attrs(base: &[(String, String)], set: &[(&str, String)]) -> String {
    let mut v: Vec<(String, String)> = base
        .iter()
        .filter(|(k, _)| !set.iter().any(|(n, _)| n == k))
        .cloned()
        .collect();
    for (k, val) in set {
        v.push((k.to_string(), val.clone()));
    }
    v.iter()
        .map(|(k, val)| format!(" {k}=\"{val}\""))
        .collect()
}

/// 把列宽/隐藏列写回 `<cols>`：保留原有的 col 属性（样式等），只改涉及到的列。
/// 目标列如果落在多列的 `<col min..max>` 里，会拆成左/中/右三段。
pub fn set_cols_xml(xml: &str, wanted: &[(u32, f32, bool)]) -> String {
    if wanted.is_empty() {
        return xml.to_string();
    }
    let mut tags: Vec<(u32, u32, Vec<(String, String)>)> = Vec::new();
    let existing = xml.find("<cols").and_then(|s| {
        xml[s..].find("</cols>").map(|e| (s, s + e))
    });
    if let Some((start, end)) = existing {
        let body = &xml[start..end];
        let mut rest = body;
        while let Some(i) = rest.find("<col ") {
            let tail = &rest[i..];
            let Some(gt) = tail.find('>') else { break };
            let tag = &tail[..gt];
            let attrs = xml_attrs(tag);
            let mut min = 0u32;
            let mut max = 0u32;
            for (k, v) in &attrs {
                if k == "min" {
                    min = v.parse().unwrap_or(0);
                }
                if k == "max" {
                    max = v.parse().unwrap_or(0);
                }
            }
            if min >= 1 && max >= min {
                tags.push((min, max, attrs));
            }
            rest = &tail[gt + 1..];
        }
    }
    for &(c, px, hidden) in wanted {
        let idx = c + 1;
        // Excel 的 ColumnWidth 与 Range.Width 差 5px 内边距：写文件用 px/8
        let width = (px / 8.0 * 100.0).round() / 100.0;
        let width = width.max(0.0);
        if let Some(pos) = tags.iter().position(|(a, b, _)| *a <= idx && idx <= *b) {
            let (a, b, attrs) = tags[pos].clone();
            let mut pieces = Vec::new();
            if a < idx {
                pieces.push((a, idx - 1, attrs.clone()));
            }
            let mut set = vec![("width", format!("{width:.2}")), ("customWidth", "1".into())];
            if hidden {
                set.push(("hidden", "1".into()));
            }
            pieces.push((idx, idx, {
                // min/max 由输出阶段统一加；隐藏状态和宽度重新算
                let mut v: Vec<(String, String)> = attrs
                    .iter()
                    .filter(|(k, _)| k != "min" && k != "max" && k != "hidden")
                    .cloned()
                    .collect();
                if !hidden {
                    // 取消隐藏时不能留着原来的 hidden
                }
                let rendered = with_attrs(&v, &set);
                v = xml_attrs(&rendered);
                v
            }));
            if idx < b {
                pieces.push((idx + 1, b, attrs));
            }
            tags.splice(pos..pos + 1, pieces);
        } else {
            let mut set = vec![("width", format!("{width:.2}")), ("customWidth", "1".into())];
            if hidden {
                set.push(("hidden", "1".into()));
            }
            tags.push((idx, idx, xml_attrs(&with_attrs(&[], &set))));
        }
    }
    if tags.is_empty() {
        return xml.to_string();
    }
    tags.sort_by_key(|t| t.0);
    let mut block = String::from("<cols>");
    for (min, max, attrs) in &tags {
        block.push_str(&format!("<col min=\"{min}\" max=\"{max}\""));
        let mut a: Vec<(String, String)> = attrs
            .iter()
            .filter(|(k, _)| k != "min" && k != "max")
            .cloned()
            .collect();
        // 取消隐藏后不能保留旧的 hidden 属性
        if !attrs.iter().any(|(k, v)| k == "hidden" && v == "1") {
            a.retain(|(k, _)| k != "hidden");
        }
        block.push_str(&with_attrs(&a, &[]));
        block.push_str("/>");
    }
    block.push_str("</cols>");
    if let Some((start, end)) = existing {
        let mut out = String::with_capacity(xml.len() + block.len());
        out.push_str(&xml[..start]);
        out.push_str(&block);
        out.push_str(&xml[end + "</cols>".len()..]);
        out
    } else {
        // 没有 <cols>：插在 sheetFormatPr 或 dimension 后面
        let anchor = xml.find("<sheetFormatPr").and_then(|i| {
            xml[i..].find('>').map(|g| i + g + 1)
        });
        let at = anchor.unwrap_or_else(|| xml.find("<sheetData").unwrap_or(xml.len()));
        let mut out = String::with_capacity(xml.len() + block.len());
        out.push_str(&xml[..at]);
        out.push_str(&block);
        out.push_str(&xml[at..]);
        out
    }
}

/// 行高/手动隐藏行写回 `<row>`；数据区里没有的行会补一个空 `<row>` 占位
pub fn set_row_sizes(xml: &str, wanted: &[(u32, Option<f32>, bool)]) -> String {
    if wanted.is_empty() {
        return xml.to_string();
    }
    fn stub(r: u32, ht: Option<f32>, hidden: bool) -> String {
        let mut tag = format!("<row r=\"{}\"", r + 1);
        if let Some(h) = ht {
            tag.push_str(&format!(
                " ht=\"{:.2}\" customHeight=\"1\" x14ac:dyDescent=\"0.15\"",
                h * 72.0 / 96.0
            ));
        }
        if hidden {
            tag.push_str(" hidden=\"1\"");
        }
        tag.push_str("/>");
        tag
    }
    let mut pending: Vec<(u32, Option<f32>, bool)> = wanted.to_vec();
    pending.sort_by_key(|x| x.0);
    let mut out = String::with_capacity(xml.len() + wanted.len() * 48);
    let mut rest = xml;
    while let Some(i) = rest.find("<row ") {
        let tail = &rest[i..];
        let Some(gt) = tail.find('>') else {
            out.push_str(tail);
            break;
        };
        let tag = &tail[..gt];
        let attrs = xml_attrs(tag);
        let mut row_no = 0u32;
        for (k, v) in &attrs {
            if k == "r" {
                row_no = v.parse().unwrap_or(0);
            }
        }
        out.push_str(&rest[..i]);
        // 这一行之前的缺行先补上（按行号顺序）
        while let Some(front) = pending.first().cloned() {
            if front.0 + 1 < row_no {
                out.push_str(&stub(front.0, front.1, front.2));
                pending.remove(0);
            } else {
                break;
            }
        }
        let hit = pending.iter().position(|(r, _, _)| r + 1 == row_no);
        if let Some(p) = hit {
            let (_, ht, hidden) = pending.remove(p);
            let mut set: Vec<(&str, String)> = Vec::new();
            if let Some(h) = ht {
                set.push(("ht", format!("{:.2}", h * 72.0 / 96.0)));
                set.push(("customHeight", "1".into()));
                set.push(("x14ac:dyDescent", "0.15".into()));
            }
            let base: Vec<(String, String)> = attrs
                .iter()
                .filter(|(k, _)| k != "hidden")
                .cloned()
                .collect();
            if hidden {
                set.push(("hidden", "1".to_string()));
            }
            let rendered = with_attrs(&base, &set);
            out.push_str("<row");
            out.push_str(&rendered);
            if tag.trim_end().ends_with('/') {
                out.push_str("/>");
            } else {
                out.push('>');
            }
        } else {
            out.push_str(tag);
            if !tag.trim_end().ends_with('/') {
                out.push('>');
            }
        }
        rest = &tail[gt + 1..];
    }
    // 剩下的缺行插在 </sheetData> 前
    if let Some(i) = rest.find("</sheetData>") {
        out.push_str(&rest[..i]);
        for (r, ht, hidden) in &pending {
            out.push_str(&stub(*r, *ht, *hidden));
        }
        out.push_str(&rest[i..]);
    } else {
        out.push_str(rest);
        for (r, ht, hidden) in &pending {
            out.push_str(&stub(*r, *ht, *hidden));
        }
    }
    out
}

/// 给 <row>/<col> 补 outlineLevel（分级显示）。只加属性，不改已有内容。
pub fn apply_outline(xml: &str, rows: &[(u32, u8)], cols: &[(u32, u8)]) -> String {
    let mut x = xml.to_string();
    for (r, lv) in rows {
        if *lv == 0 {
            continue;
        }
        let needle = format!("<row r=\"{}\"", r + 1);
        if let Some(i) = x.find(&needle) {
            if let Some(gt) = x[i..].find('>') {
                let end = i + gt;
                if !x[i..end].contains("outlineLevel=") {
                    // 自闭合标签要插在 "/>" 之前，否则会写出 `.../> outlineLevel="1">` 这种坏 XML
                    let at = if x[i..end].trim_end().ends_with('/') {
                        end - 1
                    } else {
                        end
                    };
                    x.insert_str(at, &format!(" outlineLevel=\"{lv}\""));
                }
            }
        }
    }
    for (c, lv) in cols {
        if *lv == 0 {
            continue;
        }
        let needle = format!("<col min=\"{}\"", c + 1);
        if let Some(i) = x.find(&needle) {
            if let Some(gt) = x[i..].find('>') {
                let end = i + gt;
                if !x[i..end].contains("outlineLevel=") {
                    // 自闭合标签要插在 "/>" 之前，否则会写出 `.../> outlineLevel="1">` 这种坏 XML
                    let at = if x[i..end].trim_end().ends_with('/') {
                        end - 1
                    } else {
                        end
                    };
                    x.insert_str(at, &format!(" outlineLevel=\"{lv}\""));
                }
            }
        }
    }
    x
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
    rewrite_zip(path, |name, buf| {
        if let Some((b, r, sym)) = map.get(name) {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                return set_outline_pr(&xml, *b, *r, *sym).into_bytes();
            }
        }
        buf
    })
}

/// 保存收尾：把方案列表写回各表（`<scenarios>`）
pub fn apply_scenarios(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    let targets = sheet_targets(path)?;
    let mut map: std::collections::HashMap<String, Vec<crate::scenarios::Scenario>> =
        std::collections::HashMap::new();
    for s in &wb.sheets {
        if !s.scenarios_dirty {
            continue;
        }
        if let Some(part) = targets.get(&s.name) {
            map.insert(part.clone(), s.scenarios.clone());
        }
    }
    if map.is_empty() {
        return Ok(());
    }
    rewrite_zip(path, |name, buf| {
        if let Some(list) = map.get(name) {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                return crate::scenarios::set_scenarios(&xml, list).into_bytes();
            }
        }
        buf
    })
}

/// 保存收尾：把忽略错误列表写回各表
pub fn apply_ignored_errors(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
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
    rewrite_zip(path, |name, buf| {
        if let Some(cells) = map.get(name) {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                return set_ignored_errors(&xml, cells).into_bytes();
            }
        }
        buf
    })
}

/// 保存收尾：把列宽/行高/隐藏状态补进 sheet XML
pub fn apply_layout(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    let targets = sheet_targets(path)?;
    let mut map: std::collections::HashMap<String, (Vec<(u32, f32, bool)>, Vec<(u32, Option<f32>, bool)>)> =
        std::collections::HashMap::new();
    let mut outline: std::collections::HashMap<String, (Vec<(u32, u8)>, Vec<(u32, u8)>)> =
        std::collections::HashMap::new();
    for s in &wb.sheets {
        if !s.layout_dirty {
            continue;
        }
        let Some(part) = targets.get(&s.name) else { continue };
        let mut cols: Vec<(u32, f32, bool)> = s
            .cols
            .entries()
            .map(|(c, w)| (*c, *w, s.hidden_cols.contains(c)))
            .collect();
        // 隐藏但没自定义宽度的列也要写进去
        for c in s.hidden_cols.iter() {
            if !cols.iter().any(|(x, _, _)| x == c) {
                cols.push((*c, s.cols.size(*c), true));
            }
        }
        let mut rows: Vec<(u32, Option<f32>, bool)> = s
            .rows
            .entries()
            .map(|(r, h)| (*r, Some(*h), s.hidden_rows_user.contains(r) || s.hidden_rows.contains(r)))
            .collect();
        for r in s.hidden_rows_user.iter().chain(s.hidden_rows.iter()) {
            if !rows.iter().any(|(x, _, _)| x == r) {
                rows.push((*r, None, true));
            }
        }
        // 只有分级、没有尺寸/隐藏的行也要有 <row> 元素，否则 outlineLevel 无处可挂
        for r in s.row_levels.keys() {
            if !rows.iter().any(|(x, _, _)| x == r) {
                rows.push((*r, None, false));
            }
        }
        let mut rl: Vec<(u32, u8)> = s.row_levels.iter().map(|(r, l)| (*r, *l)).collect();
        rl.sort();
        let mut cl: Vec<(u32, u8)> = s.col_levels.iter().map(|(c, l)| (*c, *l)).collect();
        cl.sort();
        if !rl.is_empty() || !cl.is_empty() {
            outline.insert(part.clone(), (cl, rl));
        }
        map.insert(part.clone(), (cols, rows));
    }
    if map.is_empty() {
        return Ok(());
    }
    rewrite_zip(path, |name, buf| {
        if let Some((cols, rows)) = map.get(name) {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = set_cols_xml(&xml, cols);
                x = set_row_sizes(&x, rows);
                if let Some((cl, rl)) = outline.get(name) {
                    x = apply_outline(&x, rl, cl);
                }
                return x.into_bytes();
            }
        }
        buf
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
    rewrite_zip(path, |name, buf| {
        if let Some(c) = map.get(name) {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                return set_tab_color(&xml, *c).into_bytes();
            }
        }
        buf
    })
}

/// 和 rewrite_zip 一样，但可以丢弃指定的部件（删除表格/图片等用）
pub fn rewrite_zip_skip<F>(
    path: &Path,
    skip: &std::collections::HashSet<String>,
    mut f: F,
) -> Result<()>
where
    F: FnMut(&str, Vec<u8>) -> Vec<u8>,
{
    let file = std::fs::File::open(path).with_context(|| format!("打开 {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("patchtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(out);
        for i in 0..zip.len() {
            let (name, mut buf) = {
                let mut e = zip.by_index(i)?;
                let name = e.name().to_string();
                let mut buf = Vec::new();
                e.read_to_end(&mut buf)?;
                (name, buf)
            };
            if skip.contains(&name) {
                continue;
            }
            buf = f(&name, buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(4));
            zw.start_file(name, opts)?;
            zw.write_all(&buf)?;
        }
        zw.finish()?;
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
    rewrite_zip(path, |name, buf| {
        if let Some(p) = map.get(name) {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                return set_sheet_protection(&xml, p.as_ref()).into_bytes();
            }
        }
        buf
    })
}

/// 页面设置写回：printOptions / pageMargins / pageSetup 三个元素
pub fn set_page_setup(xml: &str, p: &crate::page::PageSetup) -> String {
    let mut x = xml.to_string();
    for tag in ["printOptions", "pageMargins", "pageSetup"] {
        while let Some(i) = x.find(&format!("<{tag}")) {
            if let Some(gt) = x[i..].find('>') {
                x.replace_range(i..i + gt + 1, "");
            } else {
                break;
            }
        }
    }
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
    let block = format!(
        "<printOptions gridLines=\"{}\" headings=\"{}\" horizontalCentered=\"{}\" verticalCentered=\"{}\"/>\
<pageMargins left=\"{}\" right=\"{}\" top=\"{}\" bottom=\"{}\" header=\"{}\" footer=\"{}\"/>\
<pageSetup paperSize=\"{}\" orientation=\"{landscape}\"{scale}{fit}/>",
        if p.gridlines { 1 } else { 0 },
        if p.headings { 1 } else { 0 },
        if p.center_h { 1 } else { 0 },
        if p.center_v { 1 } else { 0 },
        p.margins[0], p.margins[1], p.margins[2], p.margins[3], p.margins[4], p.margins[5],
        p.paper,
    );
    let mut breaks = String::new();
    if !p.row_breaks.is_empty() {
        breaks.push_str(&format!(
            "<rowBreaks count=\"{}\" manualBreakCount=\"{}\">",
            p.row_breaks.len(),
            p.row_breaks.len()
        ));
        for r in &p.row_breaks {
            breaks.push_str(&format!("<brk id=\"{r}\" max=\"16383\" man=\"1\"/>"));
        }
        breaks.push_str("</rowBreaks>");
    }
    if !p.col_breaks.is_empty() {
        breaks.push_str(&format!(
            "<colBreaks count=\"{}\" manualBreakCount=\"{}\">",
            p.col_breaks.len(),
            p.col_breaks.len()
        ));
        for c in &p.col_breaks {
            breaks.push_str(&format!("<brk id=\"{c}\" max=\"1048575\" man=\"1\"/>"));
        }
        breaks.push_str("</colBreaks>");
    }
    // fitToPage 由 sheetPr/pageSetUpPr 控制；不关掉的话 Excel 会忽略 scale
    let fit_flag = if p.fit_to_page { "1" } else { "0" };
    if let Some(sp) = x.find("<sheetPr") {
        let end = x[sp..].find('>').map(|e| sp + e + 1).unwrap_or(sp);
        if x[sp..end].ends_with("/>") {
            // <sheetPr/> 自闭合：展开成带 pageSetUpPr 的形式
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
                newtag = newtag.replacen("<pageSetUpPr", &format!("<pageSetUpPr fitToPage=\"{fit_flag}\" "), 1);
            }
            x.replace_range(at..=pe, &newtag);
        } else {
            // schema 顺序：tabColor → outlinePr → pageSetUpPr。不能紧贴
            // <sheetPr> 开标签插入，否则会跑到已有的 outlinePr 前面。
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
    } else {
        // 没有 sheetPr：作为 worksheet 的第一个子元素插入（不能插到 XML 声明后面）
        if let Some(ws) = x.find("<worksheet") {
            if let Some(gt) = x[ws..].find('>') {
                let at = ws + gt + 1;
                x.insert_str(
                    at,
                    &format!("<sheetPr><pageSetUpPr fitToPage=\"{fit_flag}\"/></sheetPr>"),
                );
            }
        }
    }
    // 旧的分页符先清掉，避免重复
    for tag in ["rowBreaks", "colBreaks"] {
        while let Some(i) = x.find(&format!("<{tag}")) {
            let Some(rel) = x[i..].find(&format!("</{tag}>")) else { break };
            let end = i + rel + format!("</{tag}>").len();
            x.replace_range(i..end, "");
        }
    }
    let markers = [
        "<headerFooter",
        "<rowBreaks",
        "<colBreaks",
        "<customProperties",
        "<drawing",
        "<legacyDrawing",
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
    if !breaks.is_empty() {
        // schema：rowBreaks/colBreaks 在 headerFooter 之后
        let after = x
            .find("</headerFooter>")
            .map(|e| e + "</headerFooter>".len())
            .unwrap_or_else(|| x.find("</worksheet>").unwrap_or(x.len()));
        x.insert_str(after, &breaks);
    }
    x
}

/// 保存收尾：只给改过页面设置的表写回
pub fn apply_page_setup(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
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
    rewrite_zip(path, |name, buf| {
        if let Some(p) = map.get(name) {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                return set_page_setup(&xml, p).into_bytes();
            }
        }
        buf
    })
}

/// 保存收尾：把改过的富文本单元格写成 inlineStr
pub fn apply_rich(path: &std::path::Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    let targets = sheet_targets(path)?;
    let mut map: std::collections::HashMap<
        String,
        std::collections::HashMap<(u32, u32), Vec<crate::richtext::RichRun>>,
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
    rewrite_zip(path, |name, buf| {
        if let Some(cells) = map.get(name) {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                return crate::richtext::patch_sheet_rich(&xml, cells).into_bytes();
            }
        }
        buf
    })
}

/// 读 workbook.xml 里每张表的可见性：true = hidden
pub fn read_sheet_states(path: &std::path::Path) -> std::collections::HashMap<String, bool> {
    let mut out = std::collections::HashMap::new();
    let Some(xml) = crate::fixups::read_part_file(path, "xl/workbook.xml")
        .and_then(|b| String::from_utf8(b).ok())
    else {
        return out;
    };
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
    let Some(xml) = crate::fixups::read_part_file(path, "xl/workbook.xml")
        .and_then(|b| String::from_utf8(b).ok())
    else {
        return Ok(());
    };
    // 至少要留一张可见表
    if !wb.sheets.iter().any(|s| !s.hidden) {
        return Err(anyhow::anyhow!("不能把全部工作表都隐藏"));
    }
    let mut out = String::with_capacity(xml.len() + 32);
    let mut rest = xml.as_str();
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
    let Ok(file) = std::fs::File::open(path) else { return false };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return false };
    let Ok(mut f) = zip.by_name("xl/workbook.xml") else { return false };
    let mut xml = String::new();
    if f.read_to_string(&mut xml).is_err() {
        return false;
    }
    let mut r = Reader::from_str(&xml);
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
