//! 往已保存的 xlsx 里插入一张图表：生成 chart 部件、drawing 部件、两处 rels、内容类型。
//!
//! 只做柱状图（Excel 最常用），数据来自选区：首行当系列名、首列当分类。

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::Path;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 选区坐标 → 图表（首行表头、首列分类）
pub fn chart_from_selection(
    sheet: &crate::model::Sheet,
    sheet_name: &str,
    rect: (u32, u32, u32, u32),
    title: &str,
) -> crate::charts::Chart {
    let (r1, c1, r2, c2) = rect;
    let mut series = Vec::new();
    for c in (c1 + 1)..=c2 {
        let name = sheet
            .get(r1, c)
            .map(|x| x.value.display())
            .unwrap_or_else(|| format!("系列{}", c - c1));
        let mut cats = Vec::new();
        let mut vals = Vec::new();
        for r in (r1 + 1)..=r2 {
            if let Some(v) = sheet.get(r, c1) {
                cats.push(v.value.display());
            }
            if let Some(v) = sheet.get(r, c) {
                if let crate::model::Value::Number(n) = v.value {
                    vals.push(n);
                } else if let Ok(n) = v.value.display().parse::<f64>() {
                    vals.push(n);
                }
            }
        }
        let range_col = crate::model::col_label(c);
        let cat_col = crate::model::col_label(c1);
        series.push(crate::charts::Series {
            name,
            cats,
            vals,
            name_ref: format!("'{}'!${}${}", sheet_name, range_col, r1 + 1),
            cat_ref: format!("'{}'!${}${}:${}${}", sheet_name, cat_col, r1 + 2, cat_col, r2 + 1),
            val_ref: format!("'{}'!${}${}:${}${}", sheet_name, range_col, r1 + 2, range_col, r2 + 1),
        });
    }
    crate::charts::Chart {
        kind: crate::charts::ChartKind::Column,
        title: title.to_string(),
        series,
        legend: true,
        anchor: (r1, c2 + 2, r1 + 15, c2 + 9),
        legend_pos: "r".into(),
    }
}

fn chart_xml(ch: &crate::charts::Chart) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
         <c:chart>",
    );
    if !ch.title.is_empty() {
        out.push_str(&format!(
            "<c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>{}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val=\"0\"/></c:title>",
            esc(&ch.title)
        ));
    }
    out.push_str("<c:plotArea><c:layout/><c:barChart><c:barDir val=\"col\"/><c:grouping val=\"clustered\"/><c:varyColors val=\"0\"/>");
    for (i, s) in ch.series.iter().enumerate() {
        out.push_str(&format!("<c:ser><c:idx val=\"{i}\"/><c:order val=\"{i}\"/>"));
        out.push_str(&format!(
            "<c:tx><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>{}</c:v></c:pt></c:strCache></c:strRef></c:tx>",
            esc(&s.name_ref),
            esc(&s.name)
        ));
        out.push_str(&format!("<c:cat><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"{}\"/>", esc(&s.cat_ref), s.cats.len()));
        for (k, c) in s.cats.iter().enumerate() {
            out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{}</c:v></c:pt>", esc(c)));
        }
        out.push_str("</c:strCache></c:strRef></c:cat>");
        out.push_str(&format!("<c:val><c:numRef><c:f>{}</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"{}\"/>", esc(&s.val_ref), s.vals.len()));
        for (k, v) in s.vals.iter().enumerate() {
            out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{v}</c:v></c:pt>"));
        }
        out.push_str("</c:numCache></c:numRef></c:val>");
        out.push_str("<c:spPr><a:solidFill><a:srgbClr val=\"4472C4\"/></a:solidFill></c:spPr>");
        out.push_str("</c:ser>");
    }
    out.push_str("<c:gapWidth val=\"150\"/><c:axId val=\"1\"/><c:axId val=\"2\"/></c:barChart>");
    out.push_str("<c:catAx><c:axId val=\"1\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"b\"/><c:crossAx val=\"2\"/></c:catAx>");
    out.push_str("<c:valAx><c:axId val=\"2\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"l\"/><c:majorGridlines/><c:crossAx val=\"1\"/></c:valAx>");
    out.push_str("</c:plotArea>");
    if ch.legend {
        out.push_str("<c:legend><c:legendPos val=\"r\"/><c:overlay val=\"0\"/></c:legend>");
    }
    out.push_str("<c:plotVisOnly val=\"1\"/></c:chart></c:chartSpace>");
    out
}

fn drawing_xml(ch: &crate::charts::Chart, rid: &str) -> String {
    let (r1, c1, _, _) = ch.anchor;
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
         xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
         <xdr:oneCellAnchor><xdr:from><xdr:col>{c1}</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{r1}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>\
         <xdr:ext cx=\"5400000\" cy=\"2900000\"/>\
         <xdr:graphicFrame macro=\"\"><xdr:nvGraphicFramePr><xdr:cNvPr id=\"2\" name=\"图表 1\"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr>\
         <xdr:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></xdr:xfrm>\
         <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\">\
         <c:chart r:id=\"{rid}\"/></a:graphicData></a:graphic></xdr:graphicFrame>\
         <xdr:clientData/></xdr:oneCellAnchor></xdr:wsDr>"
    )
}

fn next_index(zip: &mut zip::ZipArchive<std::fs::File>, prefix: &str) -> u32 {
    let mut max = 0;
    for i in 0..zip.len() {
        if let Ok(f) = zip.by_index(i) {
            let n = f.name().to_string();
            if let Some(rest) = n.strip_prefix(prefix) {
                if let Some(num) = rest.strip_suffix(".xml") {
                    if let Ok(v) = num.parse::<u32>() {
                        max = max.max(v);
                    }
                }
            }
        }
    }
    max + 1
}

/// 重写 zip：既能改已有条目，也能追加新条目
fn rewrite_with_additions<F>(path: &Path, additions: Vec<(String, Vec<u8>)>, mut f: F) -> Result<()>
where
    F: FnMut(&str, Vec<u8>) -> Vec<u8>,
{
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let tmp = path.with_extension("charttmp");
    let existing: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();
    {
        let out = std::fs::File::create(&tmp)?;
        let mut zw = zip::ZipWriter::new(out);
        let opts = || {
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)
        };
        for i in 0..zip.len() {
            let (name, mut buf) = {
                let mut e = zip.by_index(i)?;
                let name = e.name().to_string();
                let mut b = Vec::new();
                e.read_to_end(&mut b)?;
                (name, b)
            };
            buf = f(&name, buf);
            zw.start_file(name, opts())?;
            zw.write_all(&buf)?;
        }
        for (name, body) in additions.into_iter() {
            if existing.contains(&name) {
                continue;
            }
            zw.start_file(name, opts())?;
            zw.write_all(&body)?;
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

/// 把图表写进文件（新增 chart/drawing 部件，并挂到工作表上）
pub fn insert_chart(path: &Path, sheet_name: &str, ch: &crate::charts::Chart) -> Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    let Some(sheet_path) = targets.get(sheet_name).cloned() else {
        return Err(anyhow::anyhow!("找不到工作表 {sheet_name}"));
    };

    let (chart_no, drawing_no, next_rid, has_sheet_rels) = {
        let file = std::fs::File::open(path)?;
        let mut zip = zip::ZipArchive::new(file)?;
        let chart_no = next_index(&mut zip, "xl/charts/chart");
        let drawing_no = next_index(&mut zip, "xl/drawings/drawing");
        let rels_path = match sheet_path.rsplit_once('/') {
            Some((d, b)) => format!("{d}/_rels/{b}.rels"),
            None => String::new(),
        };
        let mut max_rid = 0u32;
        let mut has_rels = false;
        if let Ok(mut f) = zip.by_name(&rels_path) {
            has_rels = true;
            let mut xml = String::new();
            let _ = f.read_to_string(&mut xml);
            for chunk in xml.split("Id=\"rId").skip(1) {
                if let Some(end) = chunk.find('"') {
                    if let Ok(v) = chunk[..end].parse::<u32>() {
                        max_rid = max_rid.max(v);
                    }
                }
            }
        }
        (chart_no, drawing_no, max_rid + 1, has_rels)
    };

    let chart_part = format!("xl/charts/chart{chart_no}.xml");
    let drawing_part = format!("xl/drawings/drawing{drawing_no}.xml");
    let drawing_rels_part = format!("xl/drawings/_rels/drawing{drawing_no}.xml.rels");
    let sheet_rels_part = match sheet_path.rsplit_once('/') {
        Some((d, b)) => format!("{d}/_rels/{b}.rels"),
        None => return Err(anyhow::anyhow!("工作表路径异常")),
    };

    let chart_body = chart_xml(ch);
    let drawing_body = drawing_xml(ch, "rId1");
    let drawing_rels = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart\" Target=\"../charts/chart{chart_no}.xml\"/></Relationships>"
    );

    let mut additions = vec![
        (chart_part.clone(), chart_body.into_bytes()),
        (drawing_part.clone(), drawing_body.into_bytes()),
        (drawing_rels_part, drawing_rels.into_bytes()),
    ];
    if !has_sheet_rels {
        // 这张表本来没有任何关系，得新建 rels 才能挂上 drawing
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId{next_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"../drawings/drawing{drawing_no}.xml\"/></Relationships>"
        );
        additions.push((sheet_rels_part.clone(), body.into_bytes()));
    }

    let sheet_path2 = sheet_path.clone();
    let sheet_rels2 = sheet_rels_part.clone();
    let drawing_name = format!("drawing{drawing_no}.xml");
    let chart_name = format!("chart{chart_no}.xml");
    rewrite_with_additions(path, additions, move |name, buf| {
        if name == sheet_rels2 {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                if !xml.contains(&drawing_name) {
                    let add = format!(
                        "<Relationship Id=\"rId{next_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"../drawings/{drawing_name}\"/>"
                    );
                    return xml.replace("</Relationships>", &format!("{add}</Relationships>")).into_bytes();
                }
            }
            return buf;
        }
        if name == sheet_path2 {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
                if !x.contains("<drawing ") {
                    let tag = format!("<drawing r:id=\"rId{next_rid}\"/>");
                    if let Some(i) = x.find("</worksheet>") {
                        x = format!("{}{}{}", &x[..i], tag, &x[i..]);
                    }
                }
                return x.into_bytes();
            }
            return buf;
        }
        if name == "[Content_Types].xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
                if !x.contains(&chart_name) {
                    x = x.replace(
                        "</Types>",
                        &format!("<Override PartName=\"/xl/charts/{chart_name}\" ContentType=\"application/vnd.openxmlformats-officedocument.drawingml.chart+xml\"/></Types>"),
                    );
                }
                if !x.contains(&drawing_name) {
                    x = x.replace(
                        "</Types>",
                        &format!("<Override PartName=\"/xl/drawings/{drawing_name}\" ContentType=\"application/vnd.openxmlformats-officedocument.drawing+xml\"/></Types>"),
                    );
                }
                return x.into_bytes();
            }
            return buf;
        }
        buf
    })?;
    Ok(())
}
