//! 往已保存的 xlsx 里插入一张图表：生成 chart 部件、drawing 部件、两处 rels、内容类型。
//!
//! 只做柱状图（Excel 最常用），数据来自选区：首行当系列名、首列当分类。

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::Path;

/// 数据标签 XML：按 Excel 的元素顺序写 showVal/showCatName/showSerName/showPercent/
/// showLegendKey/leaderLines/dLblPos，位置串直接用 OOXML 的值（ctr/outEnd/...）。
fn any_labels(ch: &crate::charts::Chart) -> bool {
    let o = &ch.label_opts;
    ch.show_labels
        || o.show_cat
        || o.show_ser
        || o.show_pct
        || o.show_legend_key
        || o.leader_lines
        || !o.position.is_empty()
}

fn dlbls_xml(ch: &crate::charts::Chart) -> String {
    let o = &ch.label_opts;
    let b = |v: bool| if v { "1" } else { "0" };
    let mut s = String::from("<c:dLbls>");
    s.push_str(&format!("<c:showVal val=\"{}\"/>", b(ch.show_labels)));
    s.push_str(&format!("<c:showCatName val=\"{}\"/>", b(o.show_cat)));
    s.push_str(&format!("<c:showSerName val=\"{}\"/>", b(o.show_ser)));
    s.push_str(&format!("<c:showPercent val=\"{}\"/>", b(o.show_pct)));
    s.push_str(&format!("<c:showLegendKey val=\"{}\"/>", b(o.show_legend_key)));
    if o.leader_lines {
        s.push_str("<c:showLeaderLines val=\"1\"/>");
    }
    if !o.position.is_empty() {
        s.push_str(&format!("<c:dLblPos val=\"{}\"/>", esc(&o.position)));
    }
    s.push_str("</c:dLbls>");
    s
}

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
        let mut xv = Vec::new();
        let num = |v: &crate::model::Value| -> Option<f64> {
            match v {
                crate::model::Value::Number(n) => Some(*n),
                other => other.display().trim().parse::<f64>().ok(),
            }
        };
        for r in (r1 + 1)..=r2 {
            let cell = sheet.get(r, c1);
            if let Some(v) = cell {
                cats.push(v.value.display());
            }
            let y = sheet.get(r, c).and_then(|v| num(&v.value));
            // 散点图是 (x,y) 成对数据：X 不是数值的行整行丢弃，保证 xv/vals 一一对应；
            // 其它图型沿用原语义（第一列当分类名，空值跳过）。
            if kind == crate::charts::ChartKind::Scatter {
                let x = cell.and_then(|v| num(&v.value));
                if let (Some(x), Some(y)) = (x, y) {
                    xv.push(x);
                    vals.push(y);
                }
            } else if let Some(y) = y {
                vals.push(y);
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
            xv,
            kind: None,
            secondary: false,
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
        label_opts: Default::default(),
        radar_style: String::new(),
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
        y_fmt: String::new(),
        y_min: None,
        y_max: None,
    }
}

const PALETTE: [&str; 8] = [
    "4472C4", "ED7D31", "A5A5A5", "FFC000", "5B9BD5", "70AD47", "264478", "9E480E",
];

fn series_body(ch: &crate::charts::Chart) -> String {
    series_body_at(ch, 0)
}

fn series_body_at(ch: &crate::charts::Chart, offset: usize) -> String {
    let mut out = String::new();
    for (i0, s) in ch.series.iter().enumerate() {
        let i = i0 + offset;
        let color: String = match s.color {
            Some(c) => format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2]),
            None => PALETTE[i % PALETTE.len()].to_string(),
        };
        // CT_BarSer / CT_LineSer 的子元素顺序：idx, order, tx, spPr, [marker], dLbls, cat, val。
        // dLbls 必须排在 cat/val 之前，否则 Excel 直接判文件损坏（实测打开即 COMException）。
        out.push_str(&format!("<c:ser><c:idx val=\"{i}\"/><c:order val=\"{i}\"/>"));
        out.push_str(&format!(
            "<c:tx><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>{}</c:v></c:pt></c:strCache></c:strRef></c:tx>",
            esc(&s.name_ref),
            esc(&s.name)
        ));
        let line = matches!(ch.kind, crate::charts::ChartKind::Line);
        if line {
            out.push_str(&format!(
                "<c:spPr><a:ln w=\"28575\"><a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill></a:ln></c:spPr>"
            ));
            out.push_str("<c:marker><c:symbol val=\"none\"/></c:marker>");
        } else {
            out.push_str(&format!(
                "<c:spPr><a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill></c:spPr>"
            ));
        }
        if any_labels(ch) {
            out.push_str(&dlbls_xml(ch));
        }
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
        // CT_ScatterSer 顺序：idx, order, tx, spPr, marker, dLbls, xVal, yVal
        out.push_str(&format!("<c:ser><c:idx val=\"{i}\"/><c:order val=\"{i}\"/>"));
        out.push_str(&format!(
            "<c:tx><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>{}</c:v></c:pt></c:strCache></c:strRef></c:tx>",
            esc(&s.name_ref),
            esc(&s.name)
        ));
        out.push_str(&format!(
            "<c:spPr><a:ln w=\"19050\"><a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill></a:ln></c:spPr>"
        ));
        out.push_str("<c:marker><c:symbol val=\"circle\"/></c:marker>");
        if any_labels(ch) {
            out.push_str(&dlbls_xml(ch));
        }
        // 有建模的 X（xv 与 vals 等长）就用它，否则退回分类列解析/序号
        let xs: Vec<f64> = if !s.xv.is_empty() && s.xv.len() == s.vals.len() {
            s.xv.clone()
        } else {
            s.cats
                .iter()
                .enumerate()
                .map(|(k, c)| c.trim().parse::<f64>().unwrap_or(k as f64 + 1.0))
                .collect()
        };
        out.push_str("<c:xVal><c:numRef><c:f>");
        out.push_str(&esc(&s.cat_ref));
        out.push_str("</c:f><c:numCache><c:formatCode>General</c:formatCode>");
        out.push_str(&format!("<c:ptCount val=\"{}\"/>", xs.len()));
        for (k, x) in xs.iter().enumerate() {
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
    if !ch.y_fmt.is_empty() {
        let nf = format!(
            "<c:numFmt formatCode=\"{}\" sourceLinked=\"0\"/>",
            esc(&ch.y_fmt)
        );
        let at = xml
            .find("<c:majorGridlines/>")
            .map(|p| p + "<c:majorGridlines/>".len())
            .or_else(|| xml.find("<c:axPos val=\"l\"/>").map(|p| p + "<c:axPos val=\"l\"/>".len()));
        if let Some(p) = at {
            xml.insert_str(p, &nf);
        }
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
        _ if matches!(ch.kind, Combo)
            || ch
                .series
                .iter()
                .any(|s| s.kind.is_some_and(|k| !matches!(k, Combo)) || s.secondary) =>
        {
            use crate::charts::ChartKind::*;
            // 按（图型, 主轴/次轴）分组，每组一个 chart 容器
            let mut groups: Vec<(crate::charts::ChartKind, bool, Vec<usize>)> = Vec::new();
            for (i, se) in ch.series.iter().enumerate() {
                let gk = match se.kind {
                    Some(k) if !matches!(k, Combo) => k,
                    _ => match ch.kind {
                        Combo | Pie | Doughnut | Scatter => Column,
                        k => k,
                    },
                };
                match groups.iter_mut().find(|g| g.0 == gk && g.1 == se.secondary) {
                    Some(g) => g.2.push(i),
                    None => groups.push((gk, se.secondary, vec![i])),
                }
            }
            let has_sub = groups.iter().any(|g| g.1);
            let mut axis_of: Vec<(usize, usize)> = Vec::new();
            for (gi, (gk, sub, idxs)) in groups.iter().enumerate() {
                let mut tmp = ch.clone();
                tmp.kind = *gk;
                tmp.series = idxs.iter().filter_map(|i| ch.series.get(*i).cloned()).collect();
                let first = *idxs.first().unwrap_or(&0);
                let body = |tmp: &crate::charts::Chart| -> String {
                    if matches!(gk, Scatter) {
                        scatter_body(tmp)
                    } else {
                        series_body_at(tmp, first)
                    }
                };
                let (a1, a2) = if *sub { (3usize, 4usize) } else { (1usize, 2usize) };
                axis_of.push((a1, a2));
                match gk {
                    Bar | BarStacked | Column => {
                        let dir = if matches!(gk, Bar) { "bar" } else { "col" };
                        let grouping = if matches!(gk, BarStacked) { "stacked" } else { "clustered" };
                        out.push_str(&format!("<c:barChart><c:barDir val=\"{dir}\"/><c:grouping val=\"{grouping}\"/><c:varyColors val=\"0\"/>"));
                        out.push_str(&body(&tmp));
                        out.push_str(&format!(
                            "<c:gapWidth val=\"150\"/><c:axId val=\"{a1}\"/><c:axId val=\"{a2}\"/></c:barChart>"
                        ));
                    }
                    Area => {
                        out.push_str("<c:areaChart><c:grouping val=\"standard\"/><c:varyColors val=\"0\"/>");
                        out.push_str(&body(&tmp));
                        out.push_str(&format!(
                            "<c:axId val=\"{a1}\"/><c:axId val=\"{a2}\"/></c:areaChart>"
                        ));
                    }
                    Scatter => {
                        out.push_str("<c:scatterChart><c:scatterStyle val=\"lineMarker\"/><c:varyColors val=\"0\"/>");
                        out.push_str(&body(&tmp));
                        out.push_str(&format!(
                            "<c:axId val=\"{a1}\"/><c:axId val=\"{a2}\"/></c:scatterChart>"
                        ));
                    }
                    _ => {
                        out.push_str("<c:lineChart><c:grouping val=\"standard\"/><c:varyColors val=\"0\"/>");
                        out.push_str(&body(&tmp));
                        out.push_str(&format!(
                            "<c:marker val=\"0\"/><c:axId val=\"{a1}\"/><c:axId val=\"{a2}\"/></c:lineChart>"
                        ));
                    }
                }
                let _ = gi;
            }
            let (p1, p2) = axis_of.first().copied().unwrap_or((1, 2));
            out.push_str(&format!("<c:catAx><c:axId val=\"{p1}\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"b\"/><c:crossAx val=\"{p2}\"/></c:catAx>"));
            out.push_str(&format!("<c:valAx><c:axId val=\"{p2}\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"l\"/><c:majorGridlines/><c:crossAx val=\"{p1}\"/></c:valAx>"));
            if has_sub {
                out.push_str("<c:valAx><c:axId val=\"4\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"r\"/><c:crossAx val=\"3\"/><c:crosses val=\"max\"/></c:valAx>");
                out.push_str("<c:catAx><c:axId val=\"3\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"1\"/><c:axPos val=\"b\"/><c:crossAx val=\"4\"/></c:catAx>");
            }
        }
        Combo => {}
        Radar => {
            let style = match ch.radar_style.as_str() {
                "standard" | "marker" | "filled" => ch.radar_style.as_str(),
                _ => "marker",
            };
            out.push_str(&format!(
                "<c:radarChart><c:radarStyle val=\"{style}\"/><c:varyColors val=\"0\"/>"
            ));
            out.push_str(&series_body(ch));
            out.push_str("<c:axId val=\"1\"/><c:axId val=\"2\"/></c:radarChart>");
            out.push_str("<c:catAx><c:axId val=\"1\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"b\"/><c:crossAx val=\"2\"/></c:catAx>");
            out.push_str("<c:valAx><c:axId val=\"2\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"l\"/><c:majorGridlines/><c:crossAx val=\"1\"/></c:valAx>");
        }
        Unknown => {}
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

/// 一个图表的 oneCellAnchor 片段（追加到已有 drawing 时复用）
fn anchor_xml(ch: &crate::charts::Chart, rid: &str, docpr: u32) -> String {
    let (r1, c1, _, _) = ch.anchor;
    format!(
        "<xdr:oneCellAnchor><xdr:from><xdr:col>{c1}</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{r1}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>\
         <xdr:ext cx=\"5400000\" cy=\"2900000\"/>\
         <xdr:graphicFrame macro=\"\"><xdr:nvGraphicFramePr><xdr:cNvPr id=\"{docpr}\" name=\"图表 {docpr}\"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr>\
         <xdr:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></xdr:xfrm>\
         <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\">\
         <c:chart xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"{rid}\"/></a:graphicData></a:graphic></xdr:graphicFrame>\
         <xdr:clientData/></xdr:oneCellAnchor>"
    )
}

fn drawing_xml(ch: &crate::charts::Chart, rid: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
         xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">{}</xdr:wsDr>",
        anchor_xml(ch, rid, 2)
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
pub(crate) fn rewrite_with_additions_pub<F>(path: &Path, additions: Vec<(String, Vec<u8>)>, mut f: F) -> Result<()>
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

    // 表上已挂着 drawing 时，新图表要作为新锚点追加进同一个 drawing。
    // 另建 drawingN 只会留下没有工作表引用的孤儿部件：Excel 打开看不到图，
    // 我们自己的 load 也读不到（chart 部件没被任何 sheet 引用）。
    let existing_drawing = {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let mut sheet_xml = String::new();
        if let Ok(mut f) = zip.by_name(&sheet_path) {
            let _ = f.read_to_string(&mut sheet_xml);
        }
        let rid = sheet_xml
            .split("<drawing r:id=\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .map(|s| s.to_string());
        rid.and_then(|rid| {
            let mut rels = String::new();
            if let Ok(mut f) = zip.by_name(&sheet_rels_part) {
                let _ = f.read_to_string(&mut rels);
            }
            rels.split("<Relationship ")
                .find(|c| c.contains(&format!("Id=\"{rid}\"")))
                .and_then(|c| c.split("Target=\"").nth(1))
                .and_then(|c| c.split('"').next())
                .map(|t| {
                    if let Some(rest) = t.strip_prefix("../") {
                        format!("xl/{rest}")
                    } else if let Some(rest) = t.strip_prefix('/') {
                        rest.to_string()
                    } else {
                        format!("xl/worksheets/{t}")
                    }
                })
        })
    };
    if let Some(dpart) = existing_drawing {
        let mut dxml = String::new();
        let mut drels = String::new();
        let mut drels_part = String::new();
        {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
            zip.by_name(&dpart)?.read_to_string(&mut dxml)?;
            drels_part = match dpart.rsplit_once('/') {
                Some((d, b)) => format!("{d}/_rels/{b}.rels"),
                None => String::new(),
            };
            if let Ok(mut f) = zip.by_name(&drels_part) {
                let _ = f.read_to_string(&mut drels);
            }
        }
        let max_rel = drels
            .split("Id=\"rId")
            .skip(1)
            .filter_map(|c| c.split('"').next().and_then(|n| n.parse::<u32>().ok()))
            .max()
            .unwrap_or(0);
        let rid = format!("rId{}", max_rel + 1);
        let docpr = dxml
            .split("<xdr:cNvPr id=\"")
            .skip(1)
            .filter_map(|c| c.split('"').next().and_then(|n| n.parse::<u32>().ok()))
            .max()
            .unwrap_or(1)
            + 1;
        let anchor = anchor_xml(ch, &rid, docpr);
        let new_drawing = dxml.replace("</xdr:wsDr>", &format!("{anchor}</xdr:wsDr>"));
        if new_drawing == dxml {
            anyhow::bail!("drawing 结构不认识，无法追加图表锚点");
        }
        let new_rels = drels.replace(
            "</Relationships>",
            &format!("<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart\" Target=\"../charts/chart{chart_no}.xml\"/></Relationships>"),
        );
        let chart_name = format!("chart{chart_no}.xml");
        let additions = vec![(
            format!("xl/charts/chart{chart_no}.xml"),
            chart_xml(ch).into_bytes(),
        )];
        let dpart_c = dpart.clone();
        let drels_c = drels_part.clone();
        let new_drawing_c = new_drawing.into_bytes();
        let new_rels_c = new_rels.into_bytes();
        rewrite_with_additions_pub(path, additions, move |name, buf| {
            // 已有条目不能靠 additions 覆盖（那里只追加新名字），必须在闭包里替换
            if name == dpart_c {
                return new_drawing_c.clone();
            }
            if name == drels_c {
                return new_rels_c.clone();
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
                    return x.into_bytes();
                }
                return buf;
            }
            buf
        })?;
        return Ok(());
    }

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
    rewrite_with_additions_pub(path, additions, move |name, buf| {
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
                    // 工作表根常常没有 xmlns:r（只有 drawing/超链接才用得上）。直接写 r:id
                    // 会变成未绑定前缀，Excel 打开即报「不能取得 Workbooks 的 Open 属性」。
                    // 跳过 <?xml ... ?> 声明：它的 '>' 也在，直接 find('>') 会把属性插进声明里
                    let decl_end = x.find("?>").map(|i| i + 2).unwrap_or(0);
                    if let Some(rel) = x[decl_end..].find('>') {
                        let root_end = decl_end + rel;
                        if !x[..root_end].contains("xmlns:r=") {
                            let head = format!(
                                "{} xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"",
                                &x[..root_end]
                            );
                            x = format!("{head}{}", &x[root_end..]);
                        }
                    }
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
        if xml.contains(old.as_str()) {
            xml = xml.replace(old.as_str(), new.as_str());
            continue;
        }
        // umya 重写图表部件时会去掉工作表名的引号；按去引号形式再试一次，
        // 否则插删行列后的引用改写会静默落空。
        let bare = |s: &str| s.replace('\'', "");
        let o = bare(old);
        if o != *old && xml.contains(&o) {
            xml = xml.replace(&o, new);
        }
    }
    crate::patch::append_entry(path, chart_part, xml.as_bytes())?;
    Ok(())
}

/// 换掉已有图表的内容（改类型/改标题），保留锚点与部件路径
pub fn replace_chart(path: &Path, chart_part: &str, ch: &crate::charts::Chart) -> Result<()> {
    // 不建模的图种只能原样保留：用 2D 结果覆盖会把原图降级
    if matches!(ch.kind, crate::charts::ChartKind::Unknown) {
        anyhow::bail!("该图表类型不受支持，只能保留原图");
    }
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

/// 把 drawing 里第 ordinal 个图表锚点改成单元格锚点（twoCellAnchor，单元格偏移 0）。
/// 图表拖动/缩放后保存时调用；不依赖列宽行高，Excel 自己换算。
pub fn rewrite_anchor_xml(xml: &str, ordinal: usize, anchor: (u32, u32, u32, u32)) -> String {
    let (r1, c1, r2, c2) = anchor;
    let (r2, c2) = (r2.max(r1 + 1), c2.max(c1 + 1));
    // 有些文件用 xdr: 前缀，umya 重写后是默认命名空间（无前缀），两种都要认
    let pre = if xml.contains("xmlns:xdr=") { "xdr:" } else { "" };
    let opens = [format!("<{pre}oneCellAnchor"), format!("<{pre}twoCellAnchor")];
    let closes = [
        format!("</{pre}oneCellAnchor>"),
        format!("</{pre}twoCellAnchor>"),
    ];
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut from = 0usize;
    while from < xml.len() {
        let next = opens
            .iter()
            .filter_map(|t| xml[from..].find(t).map(|p| (from + p, t.clone())))
            .min_by_key(|(p, _)| *p);
        let Some((i, open)) = next else { break };
        let close = if open.ends_with("oneCellAnchor") {
            &closes[0]
        } else {
            &closes[1]
        };
        let Some(rel) = xml[i..].find(close.as_str()) else {
            break;
        };
        let end = i + rel + close.len();
        let span = &xml[i..end];
        if span.contains("chart r:id") || span.contains(":chart") {
            spans.push((i, end));
        }
        from = end;
    }
    let Some(&(s0, s1)) = spans.get(ordinal) else {
        return xml.to_string();
    };
    let old = &xml[s0..s1];
    let rid = old
        .find("chart r:id=\"")
        .and_then(|i| {
            let rest = &old[i + "chart r:id=\"".len()..];
            rest.find('"').map(|e| rest[..e].to_string())
        })
        .unwrap_or_else(|| "rId1".into());
    let name = old
        .find("name=\"")
        .and_then(|i| {
            let rest = &old[i + 6..];
            rest.find('"').map(|e| rest[..e].to_string())
        })
        .unwrap_or_else(|| "图表 1".into());
    let id = old
        .find("cNvPr id=\"")
        .and_then(|i| {
            let rest = &old[i + "cNvPr id=\"".len()..];
            rest.find('"').map(|e| rest[..e].to_string())
        })
        .unwrap_or_else(|| "2".into());
    let p = pre;
    let new = format!(
        "<{p}twoCellAnchor editAs=\"oneCell\"><{p}from><{p}col>{c1}</{p}col><{p}colOff>0</{p}colOff><{p}row>{r1}</{p}row><{p}rowOff>0</{p}rowOff></{p}from><{p}to><{p}col>{c2}</{p}col><{p}colOff>0</{p}colOff><{p}row>{r2}</{p}row><{p}rowOff>0</{p}rowOff></{p}to><{p}graphicFrame macro=\"\"><{p}nvGraphicFramePr><{p}cNvPr id=\"{id}\" name=\"{name}\"/><{p}cNvGraphicFramePr/></{p}nvGraphicFramePr><{p}xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></{p}xfrm><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"{rid}\"/></a:graphicData></a:graphic></{p}graphicFrame><{p}clientData/></{p}twoCellAnchor>"
    );
    let mut out = String::with_capacity(xml.len() + new.len());
    out.push_str(&xml[..s0]);
    out.push_str(&new);
    out.push_str(&xml[s1..]);
    out
}

/// 把某张表第 ordinal 个图表的锚点写回 drawing 部件
fn anchor_cells(xml: &str) -> (Vec<u32>, Vec<u32>) {
    fn nums(xml: &str, tag: &str) -> Vec<u32> {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let mut out = Vec::new();
        let mut from = 0usize;
        while let Some(i) = xml[from..].find(&open) {
            let s = from + i + open.len();
            let Some(j) = xml[s..].find(&close) else { break };
            if let Ok(v) = xml[s..s + j].trim().parse::<u32>() {
                out.push(v);
            }
            from = s + j + close.len();
        }
        out
    }
    (nums(xml, "col"), nums(xml, "row"))
}

pub fn write_anchor(
    path: &Path,
    drawing_part: &str,
    ordinal: usize,
    anchor: (u32, u32, u32, u32),
) -> Result<()> {
    let mut xml = String::new();
    {
        let file = std::fs::File::open(path)?;
        let mut zip = zip::ZipArchive::new(file)?;
        let mut f = zip.by_name(drawing_part)?;
        f.read_to_string(&mut xml)?;
    }
    let edited = rewrite_anchor_xml(&xml, ordinal, anchor);
    // 锚点单元格没变就别重写：重写会把原来的 xdr:ext（精确尺寸）换成按单元格推的尺寸，
    // WPS 侧会读成 191 -> 202 这种高度漂移（chart-ext resave 实测）。
    if !edited.is_empty() && anchor_cells(&edited) == anchor_cells(&xml) {
        return Ok(());
    }
    if edited != xml {
        crate::patch::rewrite_zip(path, |name, buf| {
            if name == drawing_part {
                return edited.clone().into_bytes();
            }
            buf
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod anchor_tests {
    use super::*;

    #[test]
    fn rewrites_chart_anchor_and_keeps_rid() {
        let xml = r#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"><xdr:oneCellAnchor><xdr:from><xdr:col>4</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:ext cx="1" cy="1"/><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id="2" name="图表 1"/></xdr:nvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rId7"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:oneCellAnchor></xdr:wsDr>"#;
        let out = rewrite_anchor_xml(xml, 0, (2, 3, 10, 7));
        assert!(out.contains("<xdr:twoCellAnchor"));
        assert!(out.contains("<xdr:col>3</xdr:col>"), "{out}");
        assert!(out.contains("<xdr:col>7</xdr:col>"));
        assert!(out.contains("<xdr:row>2</xdr:row>"));
        assert!(out.contains("<xdr:row>10</xdr:row>"));
        assert!(out.contains("r:id=\"rId7\""));
        assert!(out.contains("name=\"图表 1\""));
        assert!(!out.contains("<xdr:oneCellAnchor"));
    }

    #[test]
    fn leaves_other_anchors_alone() {
        let one = r#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"><xdr:oneCellAnchor><xdr:graphicFrame><a:graphic><a:graphicData><c:chart r:id="rId1"/></a:graphicData></a:graphic></xdr:graphicFrame></xdr:oneCellAnchor><xdr:oneCellAnchor><xdr:graphicFrame><a:graphic><a:graphicData><c:chart r:id="rId2"/></a:graphicData></a:graphic></xdr:graphicFrame></xdr:oneCellAnchor></xdr:wsDr>"#;
        let out = rewrite_anchor_xml(one, 1, (0, 0, 5, 5));
        assert_eq!(out.matches("<xdr:oneCellAnchor").count(), 1);
        assert_eq!(out.matches("<xdr:twoCellAnchor").count(), 1);
        assert!(out.contains("rId1"));
        assert!(out.contains("rId2"));
    }
}
