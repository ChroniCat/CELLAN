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
            buf = f(&name, buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file(name, opts)?;
            zw.write_all(&buf)?;
        }
        zw.finish()?;
    }
    drop(zip);
    std::fs::rename(&tmp, path)?;
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
    if d.bold || d.italic || d.color.is_some() {
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
        out.push_str("</font>");
    }
    if let Some(f) = d.fill {
        out.push_str(&format!(
            "<fill><patternFill patternType=\"solid\"><bgColor rgb=\"FF{:02X}{:02X}{:02X}\"/></patternFill></fill>",
            f[0], f[1], f[2]
        ));
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

pub fn cf_blocks_xml(blocks: &[crate::cf::CfBlock]) -> String {
    use crate::cf::CfKind;
    let mut out = String::new();
    let mut prio = 1;
    for b in blocks {
        if b.ranges.is_empty() || b.rules.is_empty() {
            continue;
        }
        let sqref: Vec<String> = b.ranges.iter().map(range_sqref).collect();
        out.push_str(&format!("<conditionalFormatting sqref=\"{}\">", sqref.join(" ")));
        for rule in b.rules.iter() {
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
                    for (i, (v, c)) in points.iter().enumerate() {
                        let ty = if i == 0 {
                            "min"
                        } else if i == points.len() - 1 {
                            "max"
                        } else {
                            "percentile"
                        };
                        match (ty, v) {
                            ("percentile", Some(p)) => out.push_str(&format!(
                                "<cfvo type=\"percentile\" val=\"{p}\"/>"
                            )),
                            _ => out.push_str(&format!("<cfvo type=\"{ty}\"/>")),
                        }
                        out.push_str(&format!("<color rgb=\"FF{:02X}{:02X}{:02X}\"/>", c[0], c[1], c[2]));
                    }
                    out.push_str("</colorScale></cfRule>");
                }
                CfKind::Bar { color } => {
                    out.push_str(&format!("<cfRule type=\"dataBar\" priority=\"{prio}\"{stop}><dataBar>"));
                    out.push_str("<cfvo type=\"min\"/><cfvo type=\"max\"/>");
                    out.push_str(&format!("<color rgb=\"FF{:02X}{:02X}{:02X}\"/>", color[0], color[1], color[2]));
                    out.push_str("</dataBar></cfRule>");
                }
            }
            prio += 1;
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
        out.push_str(&format!(" formula1=\"{}\"", esc(&f1)));
        if d.op.needs_pair() || (d.kind == DvKind::Custom && !d.f2.is_empty()) {
            if !d.f2.is_empty() {
                out.push_str(&format!(" formula2=\"{}\"", esc(&d.f2)));
            }
        }
        out.push_str("/>");
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
        assert!(xml.contains("formula1=\"&quot;甲,乙&quot;\""), "{xml}");
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
        assert!(xml.contains("formula2=\"100\""));
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
