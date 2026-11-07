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
                CfKind::IconSet { set, reverse, thresholds, show_value } => {
                    out.push_str(&format!(
                        "<cfRule type=\"iconSet\" priority=\"{prio}\"{stop}><iconSet iconSet=\"{}\"{} showValue=\"{}\">",
                        esc(set),
                        if *reverse { " reverse=\"1\"" } else { "" },
                        if *show_value { 1 } else { 0 }
                    ));
                    for (i, t) in thresholds.iter().enumerate() {
                        let ty = if i == 0 {
                            "min"
                        } else if t.is_none() {
                            "max"
                        } else {
                            "percent"
                        };
                        match (ty, t) {
                            ("percent", Some(v)) => out.push_str(&format!(
                                "<cfvo type=\"percent\" val=\"{v}\"/>"
                            )),
                            _ => out.push_str(&format!("<cfvo type=\"{ty}\"/>")),
                        }
                    }
                    out.push_str("</iconSet></cfRule>");
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
    let mut cols: Vec<&u32> = f.allows.keys().collect();
    cols.sort();
    for col in cols {
        if *col < c1 || *col > c2 {
            continue;
        }
        let col_id = col - c1;
        out.push_str(&format!("<filterColumn colId=\"{col_id}\"><filters>"));
        let mut vals: Vec<&String> = f.allows[col].iter().collect();
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
pub fn append_entry(path: &Path, name: &str, body: &[u8]) -> Result<()> {
    use std::io::{Read, Write};
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("addtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(out);
        let opts = || {
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
        };
        let mut replaced = false;
        for i in 0..zip.len() {
            let (n, buf) = {
                let mut e = zip.by_index(i)?;
                let n = e.name().to_string();
                let mut b = Vec::new();
                e.read_to_end(&mut b)?;
                (n, b)
            };
            zw.start_file(n.clone(), opts())?;
            if n == name {
                zw.write_all(body)?;
                replaced = true;
            } else {
                zw.write_all(&buf)?;
            }
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
    use std::io::{Read, Write};
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
        };
        for i in 0..zip.len() {
            let (n, buf) = {
                let mut e = zip.by_index(i)?;
                let n = e.name().to_string();
                let mut b = Vec::new();
                e.read_to_end(&mut b)?;
                (n, b)
            };
            zw.start_file(n.clone(), opts())?;
            match left.remove(n.as_str()) {
                Some(body) => zw.write_all(body)?,
                None => zw.write_all(&buf)?,
            }
        }
        for (n, body) in items.iter() {
            if left.contains_key(n.as_str()) {
                zw.start_file(n.clone(), opts())?;
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
pub fn remove_entry(path: &Path, name: &str) -> Result<()> {
    use std::io::{Read, Write};
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("rmtmp");
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(out);
        let opts = || {
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
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
        let text = rest[head_end + 1..tail].to_string();
        let name = head
            .split("name=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("")
            .to_string();
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

/// 覆盖写自定义名称：先按名字删掉旧的，再整体插回
pub fn write_user_names(path: &Path, names: &[(String, Option<usize>, String)]) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    rewrite_zip(path, |n, buf| {
        if n != "xl/workbook.xml" {
            return buf;
        }
        let Ok(xml) = String::from_utf8(buf.clone()) else { return buf };
        let mut x = xml;
        for (name, _, _) in names {
            x = strip_name_everywhere(&x, name);
        }
        let mut defs = String::new();
        for (name, local, text) in names {
            match local {
                Some(i) => defs.push_str(&format!(
                    "<definedName name=\"{}\" localSheetId=\"{i}\">{}</definedName>",
                    esc(name),
                    esc(text)
                )),
                None => defs.push_str(&format!(
                    "<definedName name=\"{}\">{}</definedName>",
                    esc(name),
                    esc(text)
                )),
            }
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
