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
    kind: crate::charts::ChartKind,
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
            color: None,
        });
    }
    crate::charts::Chart {
        kind,
        title: title.to_string(),
        series,
        legend: true,
        anchor: (r1, c2 + 2, r1 + 15, c2 + 9),
        legend_pos: "r".into(),
        part: String::new(),
        drawing: String::new(),
        show_labels: false,
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
        y_min: None,
        y_max: None,
    }
}

const PALETTE: [&str; 8] = [
    "4472C4", "ED7D31", "A5A5A5", "FFC000", "5B9BD5", "70AD47", "264478", "9E480E",
];

fn series_body(ch: &crate::charts::Chart) -> String {
    let mut out = String::new();
    for (i, s) in ch.series.iter().enumerate() {
        let color: String = match s.color {
            Some(c) => format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2]),
            None => PALETTE[i % PALETTE.len()].to_string(),
        };
        out.push_str(&format!("<c:ser><c:idx val=\"{i}\"/><c:order val=\"{i}\"/>"));
        out.push_str(&format!(
            "<c:tx><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>{}</c:v></c:pt></c:strCache></c:strRef></c:tx>",
            esc(&s.name_ref),
            esc(&s.name)
        ));
        out.push_str(&format!(
            "<c:cat><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"{}\"/>",
            esc(&s.cat_ref),
            s.cats.len()
        ));
        for (k, c) in s.cats.iter().enumerate() {
            out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{}</c:v></c:pt>", esc(c)));
        }
        out.push_str("</c:strCache></c:strRef></c:cat>");
        out.push_str(&format!(
            "<c:val><c:numRef><c:f>{}</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"{}\"/>",
            esc(&s.val_ref),
            s.vals.len()
        ));
        for (k, v) in s.vals.iter().enumerate() {
            out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{v}</c:v></c:pt>"));
        }
        out.push_str("</c:numCache></c:numRef></c:val>");
        if ch.show_labels {
            out.push_str("<c:dLbls><c:showVal val=\"1\"/><c:showSerName val=\"0\"/><c:showCatName val=\"0\"/><c:showLegendKey val=\"0\"/></c:dLbls>");
        }
        if matches!(ch.kind, crate::charts::ChartKind::Line) {
            out.push_str("<c:marker><c:symbol val=\"circle\"/><c:size val=\"5\"/></c:marker>");
            out.push_str(&format!(
                "<c:spPr><a:ln w=\"28575\"><a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill></a:ln></c:spPr>"
            ));
        } else {
            out.push_str(&format!(
                "<c:spPr><a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill></c:spPr>"
            ));
        }
        out.push_str("</c:ser>");
    }
    out
}

fn scatter_body(ch: &crate::charts::Chart) -> String {
    let mut out = String::new();
    for (i, s) in ch.series.iter().enumerate() {
        let color: String = match s.color {
            Some(c) => format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2]),
            None => PALETTE[i % PALETTE.len()].to_string(),
        };
        out.push_str(&format!("<c:ser><c:idx val=\"{i}\"/><c:order val=\"{i}\"/>"));
        out.push_str(&format!(
            "<c:tx><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>{}</c:v></c:pt></c:strCache></c:strRef></c:tx>",
            esc(&s.name_ref),
            esc(&s.name)
        ));
        // X：分类能转数字就用，否则用序号
        out.push_str("<c:xVal><c:numRef><c:f>");
        out.push_str(&esc(&s.cat_ref));
        out.push_str("</c:f><c:numCache><c:formatCode>General</c:formatCode>");
        out.push_str(&format!("<c:ptCount val=\"{}\"/>", s.cats.len()));
        for (k, c) in s.cats.iter().enumerate() {
            let x = c.trim().parse::<f64>().unwrap_or(k as f64 + 1.0);
            out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{x}</c:v></c:pt>"));
        }
        out.push_str("</c:numCache></c:numRef></c:xVal>");
        out.push_str(&format!(
            "<c:yVal><c:numRef><c:f>{}</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"{}\"/>",
            esc(&s.val_ref),
            s.vals.len()
        ));
        for (k, v) in s.vals.iter().enumerate() {
            out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{v}</c:v></c:pt>"));
        }
        out.push_str("</c:numCache></c:numRef></c:yVal>");
        if ch.show_labels {
            out.push_str("<c:dLbls><c:showVal val=\"1\"/></c:dLbls>");
        }
        out.push_str(&format!(
            "<c:spPr><a:ln w=\"19050\"><a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill></a:ln></c:spPr>"
        ));
        out.push_str("</c:ser>");
    }
    out
}


/// 坐标轴标题与 Y 轴范围的补丁：在生成好的 XML 上做替换，避免改动大段字面量
fn axis_fix(mut xml: String, ch: &crate::charts::Chart) -> String {
    let title = |t: &str| -> String {
        if t.is_empty() {
            String::new()
        } else {
            format!(
                "<c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>{}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val=\"0\"/></c:title>",
                esc(t)
            )
        }
    };
    // Y 轴：min/max
    if ch.y_min.is_some() || ch.y_max.is_some() {
        let mut sc = String::from("<c:scaling><c:orientation val=\"minMax\"/>");
        if let Some(v) = ch.y_min {
            sc.push_str(&format!("<c:max val=\"{}\" min=\"{}\"/>", ch.y_max.unwrap_or(0.0), v));
        } else if let Some(v) = ch.y_max {
            sc.push_str(&format!("<c:max val=\"{}\"/>", v));
        }
        sc.push_str("</c:scaling>");
        // 只替换值轴那一段（图表里最后出现的 scaling）
        if let Some(pos) = xml.rfind("<c:scaling><c:orientation val=\"minMax\"/></c:scaling>") {
            xml.replace_range(
                pos..pos + "<c:scaling><c:orientation val=\"minMax\"/></c:scaling>".len(),
                &sc,
            );
        }
    }
    if !ch.axis_x.is_empty() {
        xml = xml.replace(
            "<c:axPos val=\"b\"/>",
            &format!("<c:axPos val=\"b\"/>{}", title(&ch.axis_x)),
        );
    }
    if !ch.axis_y.is_empty() {
        xml = xml.replace(
            "<c:axPos val=\"l\"/>",
            &format!("<c:axPos val=\"l\"/>{}", title(&ch.axis_y)),
        );
    }
    xml
}

/// 测试用：直接拿到整段 chart XML
pub fn chart_xml_for_test(ch: &crate::charts::Chart) -> String {
    chart_xml(ch)
}

fn chart_xml(ch: &crate::charts::Chart) -> String {
    use crate::charts::ChartKind::*;
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
    out.push_str("<c:plotArea><c:layout/>");
    match ch.kind {
        Column | Bar | BarStacked => {
            let dir = if matches!(ch.kind, Bar) { "bar" } else { "col" };
            let grouping = if matches!(ch.kind, BarStacked) { "stacked" } else { "clustered" };
            out.push_str(&format!(
                "<c:barChart><c:barDir val=\"{dir}\"/><c:grouping val=\"{grouping}\"/><c:varyColors val=\"0\"/>"
            ));
            out.push_str(&series_body(ch));
            out.push_str("<c:gapWidth val=\"150\"/><c:axId val=\"1\"/><c:axId val=\"2\"/></c:barChart>");
            out.push_str("<c:catAx><c:axId val=\"1\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"b\"/><c:crossAx val=\"2\"/></c:catAx>");
            out.push_str("<c:valAx><c:axId val=\"2\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"l\"/><c:majorGridlines/><c:crossAx val=\"1\"/></c:valAx>");
        }
        Line | Area => {
            let tag = if matches!(ch.kind, Line) { "lineChart" } else { "areaChart" };
            out.push_str(&format!("<c:{tag}><c:grouping val=\"standard\"/><c:varyColors val=\"0\"/>"));
            out.push_str(&series_body(ch));
            out.push_str(&format!("<c:marker val=\"1\"/><c:axId val=\"1\"/><c:axId val=\"2\"/></c:{tag}>"));
            out.push_str("<c:catAx><c:axId val=\"1\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"b\"/><c:crossAx val=\"2\"/></c:catAx>");
            out.push_str("<c:valAx><c:axId val=\"2\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"l\"/><c:majorGridlines/><c:crossAx val=\"1\"/></c:valAx>");
        }
        Pie | Doughnut => {
            let tag = if matches!(ch.kind, Pie) { "pieChart" } else { "doughnutChart" };
            out.push_str(&format!("<c:{tag}><c:varyColors val=\"1\"/>"));
            // 饼图只画第一个系列
            let one = crate::charts::Chart {
                series: ch.series.first().cloned().into_iter().collect(),
                ..ch.clone()
            };
            out.push_str(&series_body(&one));
            if matches!(ch.kind, Doughnut) {
                out.push_str("<c:firstSliceAng val=\"0\"/><c:holeSize val=\"50\"/>");
            }
            out.push_str(&format!("</c:{tag}>"));
        }
        Scatter => {
            out.push_str("<c:scatterChart><c:scatterStyle val=\"lineMarker\"/><c:varyColors val=\"0\"/>");
            out.push_str(&scatter_body(ch));
            out.push_str("<c:axId val=\"1\"/><c:axId val=\"2\"/></c:scatterChart>");
            out.push_str("<c:valAx><c:axId val=\"1\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"b\"/><c:crossAx val=\"2\"/></c:valAx>");
            out.push_str("<c:valAx><c:axId val=\"2\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"l\"/><c:majorGridlines/><c:crossAx val=\"1\"/></c:valAx>");
        }
    }
    out.push_str("</c:plotArea>");
    if ch.legend && !matches!(ch.kind, Pie) {
        let pos = match ch.legend_pos.as_str() {
            "t" => "t",
            "b" => "b",
            "l" => "l",
            _ => "r",
        };
        out.push_str(&format!(
            "<c:legend><c:legendPos val=\"{pos}\"/><c:overlay val=\"0\"/></c:legend>"
        ));
    }
    out.push_str("<c:plotVisOnly val=\"1\"/></c:chart></c:chartSpace>");
    axis_fix(out, ch)
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

/// 只把 chart 部件里的引用字符串换掉，其它内容原样保留
pub fn replace_refs(path: &Path, chart_part: &str, pairs: &[(String, String)]) -> Result<()> {
    use std::io::Read;
    if pairs.is_empty() {
        return Ok(());
    }
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let mut xml = String::new();
    zip.by_name(chart_part)?.read_to_string(&mut xml)?;
    drop(zip);
    for (old, new) in pairs {
        xml = xml.replace(old.as_str(), new.as_str());
    }
    crate::patch::append_entry(path, chart_part, xml.as_bytes())?;
    Ok(())
}

/// 换掉已有图表的内容（改类型/改标题），保留锚点与部件路径
pub fn replace_chart(path: &Path, chart_part: &str, ch: &crate::charts::Chart) -> Result<()> {
    let body = chart_xml(ch);
    crate::patch::append_entry(path, chart_part, body.as_bytes())?;
    Ok(())
}

/// 删除一个图表：摘掉 drawing 里的锚点、drawing rels 的条目、chart 部件本身
pub fn delete_chart(path: &Path, drawing_part: &str, chart_part: &str) -> Result<()> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut drawing = String::new();
    {
        use std::io::Read;
        let mut f = zip.by_name(drawing_part).context("drawing 部件不存在")?;
        f.read_to_string(&mut drawing)?;
    }
    let rels_part = match drawing_part.rsplit_once('/') {
        Some((d, b)) => format!("{d}/_rels/{b}.rels"),
        None => return Err(anyhow::anyhow!("drawing 路径异常")),
    };
    let mut rels = String::new();
    {
        use std::io::Read;
        if let Ok(mut f) = zip.by_name(&rels_part) {
            let _ = f.read_to_string(&mut rels);
        }
    }
    // 找指向 chart_part 的 rId
    let file_name = chart_part.rsplit('/').next().unwrap_or(chart_part);
    let mut rid = String::new();
    for chunk in rels.split("<Relationship ").skip(1) {
        let head = &chunk[..chunk.find('>').unwrap_or(chunk.len())];
        if head.contains(file_name) && head.contains("/chart") {
            if let Some(p) = head.find("Id=\"") {
                let rest = &head[p + 4..];
                if let Some(q) = rest.find('"') {
                    rid = rest[..q].to_string();
                }
            }
        }
    }
    // 去掉包含该 r:id 的整个锚点
    let mut out = String::new();
    let mut i = 0usize;
    let bytes = drawing.as_str();
    while let Some(p) = bytes[i..].find("<xdr:") {
        let start = i + p;
        out.push_str(&bytes[i..start]);
        // 找锚点结束
        let tag_end = bytes[start..].find('>').map(|x| start + x + 1).unwrap_or(bytes.len());
        let head = &bytes[start..tag_end];
        let is_anchor = head.contains("oneCellAnchor") || head.contains("twoCellAnchor");
        if is_anchor {
            let kind = if head.contains("oneCellAnchor") { "oneCellAnchor" } else { "twoCellAnchor" };
            let close = format!("</xdr:{kind}>");
            let end = bytes[tag_end..]
                .find(&close)
                .map(|x| tag_end + x + close.len())
                .unwrap_or(bytes.len());
            let block = &bytes[start..end];
            let keep = if rid.is_empty() {
                true
            } else {
                !block.contains(&format!("r:id=\"{rid}\""))
            };
            if keep {
                out.push_str(block);
            }
            i = end;
        } else {
            out.push_str(&bytes[start..tag_end]);
            i = tag_end;
        }
    }
    out.push_str(&bytes[i..]);

    crate::patch::append_entry(path, drawing_part, out.as_bytes())?;
    // drawing rels 去掉一条
    if !rid.is_empty() {
        let mut new_rels = rels.clone();
        let mut k = 0usize;
        while let Some(p) = new_rels[k..].find("<Relationship ") {
            let start = k + p;
            let end = new_rels[start..].find("/>").map(|x| start + x + 2).unwrap_or(new_rels.len());
            let block = &new_rels[start..end];
            if block.contains(&format!("Id=\"{rid}\"")) {
                new_rels.replace_range(start..end, "");
                break;
            }
            k = end;
        }
        crate::patch::append_entry(path, &rels_part, new_rels.as_bytes())?;
    }
    // 删部件本身，并去掉内容类型里的 Override（否则 Excel 会认为包坏了）
    crate::patch::remove_entry(path, chart_part)?;
    let part_name = format!("/{chart_part}");
    crate::patch::rewrite_zip(path, |name, buf| {
        if name == "[Content_Types].xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut out = String::new();
                for chunk in xml.split("<Override ").enumerate() {
                    if chunk.0 == 0 {
                        out.push_str(chunk.1);
                        continue;
                    }
                    let end = chunk.1.find("/>").map(|p| p + 2).unwrap_or(chunk.1.len());
                    let block = &chunk.1[..end];
                    if block.contains(&part_name) {
                        // 丢掉这条，但保留后面的内容
                        out.push_str(&chunk.1[end..]);
                    } else {
                        out.push_str("<Override ");
                        out.push_str(chunk.1);
                    }
                }
                return out.into_bytes();
            }
        }
        buf
    })?;
    Ok(())
}
