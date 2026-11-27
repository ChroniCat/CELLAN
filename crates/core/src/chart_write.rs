//! 图表 OOXML 读写与增量写回：支持 22 种图型、趋势线、误差线、双轴、CELLAN `<c:extLst>` 无损扩展。
//!
//! - 严格遵循 OOXML Schema 子元素顺序（`CT_BarSer` / `CT_LineSer` / `CT_ScatterSer` / `CT_BubbleSer` / `CT_ValAx`）。
//! - `Waterfall` / `Funnel` / `Treemap` / `Histogram` / `BoxWhisker` 同时写出兼容回退图与 `<c:extLst>` 完整元数据，
//!   确保 Microsoft Excel 打开不报损坏且 CELLAN 重开 100% 无损还原。

use crate::charts::{
    AxisConfig, Chart, ChartKind, ErrorBarDir, ErrorBarKind, LineDash, MarkerStyle, RefLineAxis,
    RefLineMode, Series, TrendlineKind,
};
use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;

fn any_labels(ch: &Chart) -> bool {
    let o = &ch.label_opts;
    ch.show_labels
        || o.show_cat
        || o.show_ser
        || o.show_pct
        || o.show_legend_key
        || o.leader_lines
        || !o.position.is_empty()
        || !o.num_fmt.is_empty()
}

fn dlbls_xml(ch: &Chart) -> String {
    let o = &ch.label_opts;
    let b = |v: bool| if v { "1" } else { "0" };
    let mut s = String::from("<c:dLbls>");
    if !o.num_fmt.is_empty() {
        s.push_str(&format!(
            "<c:numFmt formatCode=\"{}\" sourceLinked=\"0\"/>",
            esc(&o.num_fmt)
        ));
    }
    // 严格按 OOXML Schema / C19.1 顺序输出：showVal -> showCatName -> showSerName -> showPercent -> showLegendKey -> showLeaderLines -> dLblPos
    s.push_str(&format!("<c:showVal val=\"{}\"/>", b(ch.show_labels)));
    s.push_str(&format!("<c:showCatName val=\"{}\"/>", b(o.show_cat)));
    s.push_str(&format!("<c:showSerName val=\"{}\"/>", b(o.show_ser)));
    s.push_str(&format!("<c:showPercent val=\"{}\"/>", b(o.show_pct)));
    s.push_str(&format!(
        "<c:showLegendKey val=\"{}\"/>",
        b(o.show_legend_key)
    ));
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
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn hex_rgb(c: [u8; 3]) -> String {
    format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// 选区坐标 → 图表（首行表头、首列分类；保持既有行为契约）
pub fn chart_from_selection(
    sheet: &crate::model::Sheet,
    sheet_name: &str,
    rect: (u32, u32, u32, u32),
    title: &str,
    kind: ChartKind,
) -> Chart {
    let (r1, c1, mut r2, mut c2) = rect;
    if (r2.saturating_sub(r1) as u64 + 1) * (c2.saturating_sub(c1) as u64 + 1) > 100_000 {
        if let Some((_, _, ur2, uc2)) = sheet.used_bounds() {
            r2 = r2.min(ur2.max(r1));
            c2 = c2.min(uc2.max(c1));
        } else {
            r2 = r1;
            c2 = c1;
        }
    }
    c2 = c2.min(c1.saturating_add(255));
    r2 = r2.min(r1.saturating_add(50_000));
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
            if let Some(v) = cell.as_deref() {
                cats.push(v.value.display());
            }
            let y = sheet.get(r, c).and_then(|v| num(&v.value));
            if kind == ChartKind::Scatter {
                let x = cell.as_deref().and_then(|v| num(&v.value));
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
        series.push(Series {
            name,
            cats,
            vals,
            name_ref: format!("'{}'!${}${}", sheet_name, range_col, r1 + 1),
            cat_ref: format!(
                "'{}'!${}${}:${}${}",
                sheet_name,
                cat_col,
                r1 + 2,
                cat_col,
                r2 + 1
            ),
            val_ref: format!(
                "'{}'!${}${}:${}${}",
                sheet_name,
                range_col,
                r1 + 2,
                range_col,
                r2 + 1
            ),
            xv,
            ..Series::default()
        });
    }
    let src_ref = format!(
        "'{}'!${}${}:${}${}",
        sheet_name,
        crate::model::col_label(c1),
        r1 + 1,
        crate::model::col_label(c2),
        r2 + 1
    );
    Chart {
        kind,
        title: title.to_string(),
        series,
        legend: true,
        anchor: (r1, c2 + 2, r1 + 15, c2 + 9),
        legend_pos: "r".into(),
        source_range: src_ref,
        ..Chart::default()
    }
}

fn trendlines_xml(s: &Series) -> String {
    let mut out = String::new();
    for tl in &s.trendlines {
        out.push_str("<c:trendline>");
        if !tl.name.is_empty() {
            out.push_str(&format!("<c:name>{}</c:name>", esc(&tl.name)));
        }
        if tl.color.is_some() || tl.dash != LineDash::Solid {
            out.push_str("<c:spPr><a:ln w=\"19050\">");
            if let Some(rgb) = tl.color {
                out.push_str(&format!(
                    "<a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill>",
                    hex_rgb(rgb)
                ));
            }
            if tl.dash != LineDash::Solid {
                out.push_str(&format!("<a:prstDash val=\"{}\"/>", tl.dash.code()));
            }
            out.push_str("</a:ln></c:spPr>");
        }
        match tl.kind {
            TrendlineKind::Linear => {
                out.push_str("<c:trendlineType val=\"linear\"/>");
            }
            TrendlineKind::Polynomial(ord) => {
                out.push_str(&format!(
                    "<c:trendlineType val=\"poly\"/><c:order val=\"{}\"/>",
                    ord.clamp(2, 4)
                ));
            }
            TrendlineKind::Exponential => {
                out.push_str("<c:trendlineType val=\"exp\"/>");
            }
            TrendlineKind::Logarithmic => {
                out.push_str("<c:trendlineType val=\"log\"/>");
            }
            TrendlineKind::Power => {
                out.push_str("<c:trendlineType val=\"power\"/>");
            }
            TrendlineKind::MovingAverage(p) => {
                out.push_str(&format!(
                    "<c:trendlineType val=\"movingAvg\"/><c:period val=\"{}\"/>",
                    p.clamp(2, 30)
                ));
            }
        }
        if tl.forward > 0.0 {
            out.push_str(&format!("<c:forward val=\"{}\"/>", tl.forward));
        }
        if tl.backward > 0.0 {
            out.push_str(&format!("<c:backward val=\"{}\"/>", tl.backward));
        }
        out.push_str(&format!(
            "<c:dispRSqr val=\"{}\"/><c:dispEq val=\"{}\"/>",
            if tl.show_r2 { "1" } else { "0" },
            if tl.show_equation { "1" } else { "0" }
        ));
        out.push_str("</c:trendline>");
    }
    out
}

fn error_bars_xml(s: &Series) -> String {
    let Some(eb) = &s.error_bars else {
        return String::new();
    };
    let dir = match eb.dir {
        ErrorBarDir::Both => "both",
        ErrorBarDir::Plus => "plus",
        ErrorBarDir::Minus => "minus",
    };
    let (vtype, val_attr) = match eb.kind {
        ErrorBarKind::Fixed(v) => ("fixedVal", Some(v)),
        ErrorBarKind::Percent(v) => ("percentage", Some(v)),
        ErrorBarKind::StdDev(v) => ("stdDev", Some(v)),
        ErrorBarKind::StdErr => ("stdErr", None),
    };
    let mut out = format!(
        "<c:errBars><c:errDir val=\"y\"/><c:errBarType val=\"{dir}\"/><c:errValType val=\"{vtype}\"/>"
    );
    if let Some(v) = val_attr {
        out.push_str(&format!("<c:val val=\"{v}\"/>"));
    }
    out.push_str("</c:errBars>");
    out
}

fn series_sp_pr(color: &str, is_line: bool, s: &Series) -> String {
    if is_line {
        let w_emu = if s.line_width > 0.0 {
            (s.line_width as f64 * 12700.0).round() as u32
        } else {
            28575
        };
        let dash = if s.dash != LineDash::Solid {
            format!("<a:prstDash val=\"{}\"/>", s.dash.code())
        } else {
            String::new()
        };
        format!(
            "<c:spPr><a:ln w=\"{w_emu}\"><a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill>{dash}</a:ln></c:spPr>"
        )
    } else {
        format!("<c:spPr><a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill></c:spPr>")
    }
}

fn series_body(ch: &Chart) -> String {
    series_body_at(ch, 0)
}

fn series_body_at(ch: &Chart, offset: usize) -> String {
    let theme = crate::chart_engine::theme_spec(ch.style.theme);
    let mut out = String::new();
    let is_line = matches!(ch.kind, ChartKind::Line | ChartKind::LineStacked | ChartKind::Radar);
    let is_pie = matches!(ch.kind, ChartKind::Pie | ChartKind::Doughnut);
    for (i0, s) in ch.series.iter().enumerate() {
        let i = i0 + offset;
        let color = match s.color {
            Some(c) => hex_rgb(c),
            None => hex_rgb(theme.palette[i % theme.palette.len()]),
        };
        out.push_str(&format!("<c:ser><c:idx val=\"{i}\"/><c:order val=\"{i}\"/>"));
        if !s.name_ref.is_empty() {
            out.push_str(&format!(
                "<c:tx><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>{}</c:v></c:pt></c:strCache></c:strRef></c:tx>",
                esc(&s.name_ref),
                esc(&s.name)
            ));
        } else if !s.name.is_empty() {
            out.push_str(&format!("<c:tx><c:v>{}</c:v></c:tx>", esc(&s.name)));
        }
        if !is_pie {
            out.push_str(&series_sp_pr(&color, is_line, s));
        }
        if is_pie && ch.style.pie_explosion > 0 {
            out.push_str(&format!("<c:explosion val=\"{}\"/>", ch.style.pie_explosion));
        }
        if is_line {
            let sym = if s.marker == MarkerStyle::Auto {
                "none"
            } else {
                s.marker.ooxml_val()
            };
            out.push_str(&format!("<c:marker><c:symbol val=\"{sym}\"/></c:marker>"));
        }
        if any_labels(ch) {
            out.push_str(&dlbls_xml(ch));
        }
        if !is_pie {
            out.push_str(&trendlines_xml(s));
            out.push_str(&error_bars_xml(s));
        }
        if !s.cat_ref.is_empty() || !s.cats.is_empty() {
            out.push_str("<c:cat>");
            if !s.cat_ref.is_empty() {
                out.push_str(&format!(
                    "<c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"{}\"/>",
                    esc(&s.cat_ref),
                    s.cats.len()
                ));
                for (k, c) in s.cats.iter().enumerate() {
                    out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{}</c:v></c:pt>", esc(c)));
                }
                out.push_str("</c:strCache></c:strRef>");
            } else {
                out.push_str(&format!("<c:strLit><c:ptCount val=\"{}\"/>", s.cats.len()));
                for (k, c) in s.cats.iter().enumerate() {
                    out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{}</c:v></c:pt>", esc(c)));
                }
                out.push_str("</c:strLit>");
            }
            out.push_str("</c:cat>");
        }
        out.push_str("<c:val>");
        if !s.val_ref.is_empty() {
            out.push_str(&format!(
                "<c:numRef><c:f>{}</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"{}\"/>",
                esc(&s.val_ref),
                s.vals.len()
            ));
            for (k, &v) in s.vals.iter().enumerate() {
                if v.is_finite() {
                    out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{v}</c:v></c:pt>"));
                }
            }
            out.push_str("</c:numCache></c:numRef>");
        } else {
            out.push_str(&format!(
                "<c:numLit><c:formatCode>General</c:formatCode><c:ptCount val=\"{}\"/>",
                s.vals.len()
            ));
            for (k, &v) in s.vals.iter().enumerate() {
                if v.is_finite() {
                    out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{v}</c:v></c:pt>"));
                }
            }
            out.push_str("</c:numLit>");
        }
        out.push_str("</c:val>");
        if is_line && s.smooth {
            out.push_str("<c:smooth val=\"1\"/>");
        }
        out.push_str("</c:ser>");
    }
    out
}

fn scatter_or_bubble_body(ch: &Chart, is_bubble: bool) -> String {
    let theme = crate::chart_engine::theme_spec(ch.style.theme);
    let mut out = String::new();
    for (i, s) in ch.series.iter().enumerate() {
        let color = match s.color {
            Some(c) => hex_rgb(c),
            None => hex_rgb(theme.palette[i % theme.palette.len()]),
        };
        out.push_str(&format!("<c:ser><c:idx val=\"{i}\"/><c:order val=\"{i}\"/>"));
        if !s.name_ref.is_empty() {
            out.push_str(&format!(
                "<c:tx><c:strRef><c:f>{}</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>{}</c:v></c:pt></c:strCache></c:strRef></c:tx>",
                esc(&s.name_ref),
                esc(&s.name)
            ));
        } else if !s.name.is_empty() {
            out.push_str(&format!("<c:tx><c:v>{}</c:v></c:tx>", esc(&s.name)));
        }
        if is_bubble {
            out.push_str(&series_sp_pr(&color, false, s));
        } else {
            out.push_str(&series_sp_pr(&color, true, s));
            let sym = if s.marker == MarkerStyle::Auto {
                "circle"
            } else {
                s.marker.ooxml_val()
            };
            out.push_str(&format!("<c:marker><c:symbol val=\"{sym}\"/></c:marker>"));
        }
        if any_labels(ch) {
            out.push_str(&dlbls_xml(ch));
        }
        out.push_str(&trendlines_xml(s));
        out.push_str(&error_bars_xml(s));
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
        out.push_str(&format!(
            "</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"{}\"/>",
            xs.len()
        ));
        for (k, &x) in xs.iter().enumerate() {
            if x.is_finite() {
                out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{x}</c:v></c:pt>"));
            }
        }
        out.push_str("</c:numCache></c:numRef></c:xVal>");
        out.push_str(&format!(
            "<c:yVal><c:numRef><c:f>{}</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"{}\"/>",
            esc(&s.val_ref),
            s.vals.len()
        ));
        for (k, &v) in s.vals.iter().enumerate() {
            if v.is_finite() {
                out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{v}</c:v></c:pt>"));
            }
        }
        out.push_str("</c:numCache></c:numRef></c:yVal>");
        if is_bubble {
            let sz: Vec<f64> = if !s.sizes.is_empty() {
                s.sizes.clone()
            } else {
                vec![10.0; s.vals.len()]
            };
            out.push_str("<c:bubbleSize>");
            if !s.size_ref.is_empty() {
                out.push_str(&format!(
                    "<c:numRef><c:f>{}</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val=\"{}\"/>",
                    esc(&s.size_ref),
                    sz.len()
                ));
                for (k, &v) in sz.iter().enumerate() {
                    if v.is_finite() {
                        out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{v}</c:v></c:pt>"));
                    }
                }
                out.push_str("</c:numCache></c:numRef>");
            } else {
                out.push_str(&format!(
                    "<c:numLit><c:formatCode>General</c:formatCode><c:ptCount val=\"{}\"/>",
                    sz.len()
                ));
                for (k, &v) in sz.iter().enumerate() {
                    if v.is_finite() {
                        out.push_str(&format!("<c:pt idx=\"{k}\"><c:v>{v}</c:v></c:pt>"));
                    }
                }
                out.push_str("</c:numLit>");
            }
            out.push_str("</c:bubbleSize>");
        } else if s.smooth {
            out.push_str("<c:smooth val=\"1\"/>");
        }
        out.push_str("</c:ser>");
    }
    out
}

fn axis_title_xml(t: &str) -> String {
    if t.is_empty() {
        String::new()
    } else {
        format!(
            "<c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>{}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val=\"0\"/></c:title>",
            esc(t)
        )
    }
}

fn scaling_xml(ax: &AxisConfig) -> String {
    let orient = if ax.reverse { "maxMin" } else { "minMax" };
    let mut s = format!("<c:scaling><c:orientation val=\"{orient}\"/>");
    if let Some(b) = ax.log_base.filter(|b| *b > 1.0) {
        s.push_str(&format!("<c:logBase val=\"{b}\"/>"));
    }
    // OOXML Schema 顺序：logBase -> orientation -> max -> min（两个独立子元素！）
    if let Some(mx) = ax.max.filter(|v| v.is_finite()) {
        s.push_str(&format!("<c:max val=\"{mx}\"/>"));
    }
    if let Some(mn) = ax.min.filter(|v| v.is_finite()) {
        s.push_str(&format!("<c:min val=\"{mn}\"/>"));
    }
    s.push_str("</c:scaling>");
    s
}

fn cat_ax_xml(ax_id: usize, cross_ax: usize, pos: &str, ax: &AxisConfig, hidden: bool) -> String {
    let del = if hidden || ax.hidden { "1" } else { "0" };
    let mut s = format!(
        "<c:catAx><c:axId val=\"{ax_id}\"/>{}<c:delete val=\"{del}\"/><c:axPos val=\"{pos}\"/>",
        scaling_xml(ax)
    );
    if ax.show_major_grid {
        s.push_str("<c:majorGridlines/>");
    }
    if ax.show_minor_grid {
        s.push_str("<c:minorGridlines/>");
    }
    s.push_str(&axis_title_xml(&ax.title));
    if !ax.num_fmt.is_empty() {
        s.push_str(&format!(
            "<c:numFmt formatCode=\"{}\" sourceLinked=\"0\"/>",
            esc(&ax.num_fmt)
        ));
    }
    s.push_str(&format!("<c:crossAx val=\"{cross_ax}\"/></c:catAx>"));
    s
}

fn val_ax_xml(
    ax_id: usize,
    cross_ax: usize,
    pos: &str,
    ax: &AxisConfig,
    crosses_max: bool,
) -> String {
    let del = if ax.hidden { "1" } else { "0" };
    let mut s = format!(
        "<c:valAx><c:axId val=\"{ax_id}\"/>{}<c:delete val=\"{del}\"/><c:axPos val=\"{pos}\"/>",
        scaling_xml(ax)
    );
    if ax.show_major_grid {
        s.push_str("<c:majorGridlines/>");
    }
    if ax.show_minor_grid {
        s.push_str("<c:minorGridlines/>");
    }
    s.push_str(&axis_title_xml(&ax.title));
    if !ax.num_fmt.is_empty() {
        s.push_str(&format!(
            "<c:numFmt formatCode=\"{}\" sourceLinked=\"0\"/>",
            esc(&ax.num_fmt)
        ));
    }
    s.push_str(&format!("<c:crossAx val=\"{cross_ax}\"/>"));
    if crosses_max {
        s.push_str("<c:crosses val=\"max\"/>");
    }
    if let Some(u) = ax.major_unit.filter(|u| u.is_finite() && *u > 0.0) {
        s.push_str(&format!("<c:majorUnit val=\"{u}\"/>"));
    }
    s.push_str("</c:valAx>");
    s
}

fn cellan_ext_xml(ch: &Chart) -> String {
    let needs_ext = matches!(
        ch.kind,
        ChartKind::Waterfall
            | ChartKind::Funnel
            | ChartKind::Treemap
            | ChartKind::Histogram
            | ChartKind::BoxWhisker
    ) || ch.style.theme != crate::charts::ChartThemeId::CellanModern
        || !ch.subtitle.is_empty()
        || !ch.source_range.is_empty()
        || !ch.ref_lines.is_empty()
        || ch.analytics.highlight_extrema
        || ch.analytics.show_growth_badge
        || ch.analytics.show_summary_bar
        || ch.analytics.show_pareto_line
        || ch.analytics.hist_bins.is_some()
        || !ch.analytics.waterfall_totals.is_empty()
        || ch.analytics.show_center_kpi
        || !ch.analytics.center_kpi_title.is_empty()
        || !ch.style.rounded_bars
        || !ch.style.show_card_shadow
        || ch.style.transparent_bg
        || ch.style.switch_row_col
        || ch.series.iter().any(|s| s.stepped || s.hidden);
    if !needs_ext {
        return String::new();
    }
    let b = |v: bool| if v { "1" } else { "0" };
    let mut meta = format!(
        "<cellan:meta kind=\"{}\" theme=\"{}\" roundedBars=\"{}\" shadow=\"{}\" transparentBg=\"{}\" switchRowCol=\"{}\" extrema=\"{}\" growthBadge=\"{}\" summaryBar=\"{}\" pareto=\"{}\" centerKpi=\"{}\"",
        ch.kind.ext_code(),
        ch.style.theme.code(),
        b(ch.style.rounded_bars),
        b(ch.style.show_card_shadow),
        b(ch.style.transparent_bg),
        b(ch.style.switch_row_col),
        b(ch.analytics.highlight_extrema),
        b(ch.analytics.show_growth_badge),
        b(ch.analytics.show_summary_bar),
        b(ch.analytics.show_pareto_line),
        b(ch.analytics.show_center_kpi),
    );
    if !ch.subtitle.is_empty() {
        meta.push_str(&format!(" subtitle=\"{}\"", esc(&ch.subtitle)));
    }
    if !ch.source_range.is_empty() {
        meta.push_str(&format!(" sourceRange=\"{}\"", esc(&ch.source_range)));
    }
    if let Some(bins) = ch.analytics.hist_bins {
        meta.push_str(&format!(" histBins=\"{bins}\""));
    }
    if !ch.analytics.center_kpi_title.is_empty() {
        meta.push_str(&format!(
            " centerKpiTitle=\"{}\"",
            esc(&ch.analytics.center_kpi_title)
        ));
    }
    if !ch.analytics.waterfall_totals.is_empty() {
        let joined: Vec<String> = ch
            .analytics
            .waterfall_totals
            .iter()
            .map(|i| i.to_string())
            .collect();
        meta.push_str(&format!(" wfTotals=\"{}\"", joined.join(",")));
    }
    if ch.series.iter().any(|s| s.stepped) {
        let mask: Vec<&str> = ch.series.iter().map(|s| b(s.stepped)).collect();
        meta.push_str(&format!(" steppedMask=\"{}\"", mask.join(",")));
    }
    if ch.series.iter().any(|s| s.hidden) {
        let mask: Vec<&str> = ch.series.iter().map(|s| b(s.hidden)).collect();
        meta.push_str(&format!(" hiddenMask=\"{}\"", mask.join(",")));
    }
    meta.push('>');
    for rl in &ch.ref_lines {
        let ax = match rl.axis {
            RefLineAxis::YPrimary => "y1",
            RefLineAxis::YSecondary => "y2",
            RefLineAxis::X => "x",
        };
        let (mode, v1, v2) = match rl.mode {
            RefLineMode::Fixed(v) => ("fixed", Some(v), None),
            RefLineMode::Mean => ("mean", None, None),
            RefLineMode::Median => ("median", None, None),
            RefLineMode::Min => ("min", None, None),
            RefLineMode::Max => ("max", None, None),
            RefLineMode::Percentile(p) => ("pct", Some(p), None),
            RefLineMode::Band { low, high } => ("band", Some(low), Some(high)),
            RefLineMode::StdDevBand => ("stdBand", None, None),
        };
        meta.push_str(&format!(
            "<cellan:refLine label=\"{}\" axis=\"{ax}\" mode=\"{mode}\" color=\"{}\" dash=\"{}\"",
            esc(&rl.label),
            hex_rgb(rl.color),
            rl.dash.code()
        ));
        if let Some(a) = v1 {
            meta.push_str(&format!(" v1=\"{a}\""));
        }
        if let Some(c) = v2 {
            meta.push_str(&format!(" v2=\"{c}\""));
        }
        meta.push_str("/>");
    }
    meta.push_str("</cellan:meta>");
    format!(
        "<c:extLst><c:ext uri=\"{{8B62D2D1-4C5E-4A91-9A63-CELLANCHART1}}\" xmlns:cellan=\"urn:cellan:chart:v1\">{meta}</c:ext></c:extLst>"
    )
}

/// 测试用：直接拿到整段 chart XML
pub fn chart_xml_for_test(ch: &Chart) -> String {
    chart_xml(ch)
}

fn chart_xml(ch: &Chart) -> String {
    use ChartKind::*;
    let x_ax = ch.effective_x_axis();
    let y_ax = ch.effective_y_axis();
    let y2_ax = &ch.y2_axis;
    let gap = ch.style.gap_width.clamp(0, 500);

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

    let is_multi_series_combo = matches!(ch.kind, Combo)
        || ch
            .series
            .iter()
            .any(|s| s.kind.is_some_and(|k| k != ch.kind && !matches!(k, Combo)) || s.secondary);

    if is_multi_series_combo
        && !matches!(
            ch.kind,
            Pie | Doughnut | Radar | Waterfall | Funnel | Treemap | Histogram | BoxWhisker | Unknown
        )
    {
        let mut groups: Vec<(ChartKind, bool, Vec<usize>)> = Vec::new();
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
        for (gk, sub, idxs) in &groups {
            let mut tmp = ch.clone();
            tmp.kind = *gk;
            tmp.series = idxs.iter().filter_map(|i| ch.series.get(*i).cloned()).collect();
            let first = *idxs.first().unwrap_or(&0);
            let (a1, a2) = if *sub { (3usize, 4usize) } else { (1usize, 2usize) };
            axis_of.push((a1, a2));
            match gk {
                Column | ColumnStacked | ColumnStacked100 | Bar | BarStacked | BarStacked100 => {
                    let dir = if gk.is_horizontal() { "bar" } else { "col" };
                    let grp = if gk.is_percent_stacked() {
                        "percentStacked"
                    } else if gk.is_stacked() {
                        "stacked"
                    } else {
                        "clustered"
                    };
                    out.push_str(&format!(
                        "<c:barChart><c:barDir val=\"{dir}\"/><c:grouping val=\"{grp}\"/><c:varyColors val=\"0\"/>"
                    ));
                    out.push_str(&series_body_at(&tmp, first));
                    out.push_str(&format!(
                        "<c:gapWidth val=\"{gap}\"/><c:axId val=\"{a1}\"/><c:axId val=\"{a2}\"/></c:barChart>"
                    ));
                }
                Area | AreaStacked | AreaStacked100 => {
                    let grp = if gk.is_percent_stacked() {
                        "percentStacked"
                    } else if gk.is_stacked() {
                        "stacked"
                    } else {
                        "standard"
                    };
                    out.push_str(&format!(
                        "<c:areaChart><c:grouping val=\"{grp}\"/><c:varyColors val=\"0\"/>"
                    ));
                    out.push_str(&series_body_at(&tmp, first));
                    out.push_str(&format!(
                        "<c:axId val=\"{a1}\"/><c:axId val=\"{a2}\"/></c:areaChart>"
                    ));
                }
                Scatter => {
                    out.push_str(
                        "<c:scatterChart><c:scatterStyle val=\"lineMarker\"/><c:varyColors val=\"0\"/>",
                    );
                    out.push_str(&scatter_or_bubble_body(&tmp, false));
                    out.push_str(&format!(
                        "<c:axId val=\"{a1}\"/><c:axId val=\"{a2}\"/></c:scatterChart>"
                    ));
                }
                _ => {
                    let grp = if matches!(gk, LineStacked) {
                        "stacked"
                    } else {
                        "standard"
                    };
                    out.push_str(&format!(
                        "<c:lineChart><c:grouping val=\"{grp}\"/><c:varyColors val=\"0\"/>"
                    ));
                    out.push_str(&series_body_at(&tmp, first));
                    out.push_str(&format!(
                        "<c:marker val=\"0\"/><c:axId val=\"{a1}\"/><c:axId val=\"{a2}\"/></c:lineChart>"
                    ));
                }
            }
        }
        let (p1, p2) = axis_of.first().copied().unwrap_or((1, 2));
        out.push_str(&cat_ax_xml(p1, p2, "b", &x_ax, false));
        out.push_str(&val_ax_xml(p2, p1, "l", &y_ax, false));
        if has_sub {
            out.push_str(&val_ax_xml(4, 3, "r", y2_ax, true));
            out.push_str(&cat_ax_xml(3, 4, "b", &AxisConfig::default(), true));
        }
    } else {
        match ch.kind {
            Column | ColumnStacked | ColumnStacked100 | Bar | BarStacked | BarStacked100
            | Waterfall | Funnel | Treemap | Histogram | BoxWhisker => {
                let dir = if ch.kind.is_horizontal() { "bar" } else { "col" };
                let grp = if ch.kind.is_percent_stacked() {
                    "percentStacked"
                } else if ch.kind.is_stacked() {
                    "stacked"
                } else {
                    "clustered"
                };
                out.push_str(&format!(
                    "<c:barChart><c:barDir val=\"{dir}\"/><c:grouping val=\"{grp}\"/><c:varyColors val=\"0\"/>"
                ));
                out.push_str(&series_body(ch));
                out.push_str(&format!(
                    "<c:gapWidth val=\"{gap}\"/><c:axId val=\"1\"/><c:axId val=\"2\"/></c:barChart>"
                ));
                out.push_str(&cat_ax_xml(1, 2, "b", &x_ax, false));
                out.push_str(&val_ax_xml(2, 1, "l", &y_ax, false));
            }
            Line | LineStacked | Area | AreaStacked | AreaStacked100 => {
                let is_line = matches!(ch.kind, Line | LineStacked);
                let tag = if is_line { "lineChart" } else { "areaChart" };
                let grp = if ch.kind.is_percent_stacked() {
                    "percentStacked"
                } else if ch.kind.is_stacked() {
                    "stacked"
                } else {
                    "standard"
                };
                out.push_str(&format!(
                    "<c:{tag}><c:grouping val=\"{grp}\"/><c:varyColors val=\"0\"/>"
                ));
                out.push_str(&series_body(ch));
                if is_line {
                    out.push_str("<c:marker val=\"1\"/>");
                }
                out.push_str(&format!(
                    "<c:axId val=\"1\"/><c:axId val=\"2\"/></c:{tag}>"
                ));
                out.push_str(&cat_ax_xml(1, 2, "b", &x_ax, false));
                out.push_str(&val_ax_xml(2, 1, "l", &y_ax, false));
            }
            Pie | Doughnut => {
                let tag = if matches!(ch.kind, Pie) {
                    "pieChart"
                } else {
                    "doughnutChart"
                };
                out.push_str(&format!("<c:{tag}><c:varyColors val=\"1\"/>"));
                let one = Chart {
                    series: ch.series.first().cloned().into_iter().collect(),
                    ..ch.clone()
                };
                out.push_str(&series_body(&one));
                out.push_str(&format!(
                    "<c:firstSliceAng val=\"{}\"/>",
                    ch.style.first_slice_ang % 360
                ));
                if matches!(ch.kind, Doughnut) {
                    out.push_str(&format!(
                        "<c:holeSize val=\"{}\"/>",
                        ch.style.hole_size.clamp(10, 90)
                    ));
                }
                out.push_str(&format!("</c:{tag}>"));
            }
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
                out.push_str(&cat_ax_xml(1, 2, "b", &x_ax, false));
                out.push_str(&val_ax_xml(2, 1, "l", &y_ax, false));
            }
            Scatter => {
                out.push_str(
                    "<c:scatterChart><c:scatterStyle val=\"lineMarker\"/><c:varyColors val=\"0\"/>",
                );
                out.push_str(&scatter_or_bubble_body(ch, false));
                out.push_str("<c:axId val=\"1\"/><c:axId val=\"2\"/></c:scatterChart>");
                out.push_str(&val_ax_xml(1, 2, "b", &x_ax, false));
                out.push_str(&val_ax_xml(2, 1, "l", &y_ax, false));
            }
            Bubble => {
                out.push_str("<c:bubbleChart><c:varyColors val=\"0\"/>");
                out.push_str(&scatter_or_bubble_body(ch, true));
                out.push_str("<c:bubbleScale val=\"100\"/><c:showNegBubbles val=\"0\"/><c:axId val=\"1\"/><c:axId val=\"2\"/></c:bubbleChart>");
                out.push_str(&val_ax_xml(1, 2, "b", &x_ax, false));
                out.push_str(&val_ax_xml(2, 1, "l", &y_ax, false));
            }
            Combo | Unknown => {}
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
    out.push_str(&format!(
        "<c:plotVisOnly val=\"{}\"/><c:dispBlanksAs val=\"{}\"/>",
        if ch.style.plot_vis_only { "1" } else { "0" },
        ch.style.blank_policy.ooxml_val()
    ));
    out.push_str("</c:chart>");
    out.push_str(&cellan_ext_xml(ch));
    out.push_str("</c:chartSpace>");
    out
}

/// 一个图表的 twoCellAnchor 片段（精确保存起点与右下角跨度，不再写死 5400000×2900000）
fn anchor_xml(ch: &Chart, rid: &str, docpr: u32) -> String {
    let (r1, c1, r2, c2) = ch.anchor;
    let r2 = r2.max(r1 + 2);
    let c2 = c2.max(c1 + 3);
    format!(
        "<xdr:twoCellAnchor editAs=\"oneCell\">\
         <xdr:from><xdr:col>{c1}</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{r1}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>\
         <xdr:to><xdr:col>{c2}</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{r2}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>\
         <xdr:graphicFrame macro=\"\"><xdr:nvGraphicFramePr><xdr:cNvPr id=\"{docpr}\" name=\"图表 {docpr}\"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr>\
         <xdr:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></xdr:xfrm>\
         <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\">\
         <c:chart xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"{rid}\"/></a:graphicData></a:graphic></xdr:graphicFrame>\
         <xdr:clientData/></xdr:twoCellAnchor>"
    )
}

fn drawing_xml(ch: &Chart, rid: &str) -> String {
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

pub(crate) fn rewrite_with_additions_pub<F>(
    path: &Path,
    additions: Vec<(String, Vec<u8>)>,
    targets: &[&str],
    mut edit: F,
) -> Result<()>
where
    F: FnMut(&str, Vec<u8>) -> Vec<u8>,
{
    rewrite_with_additions_checked(path, additions, targets, |name, bytes| Ok(edit(name, bytes)))
}

pub(crate) fn rewrite_with_additions_checked(
    path: &Path,
    additions: Vec<(String, Vec<u8>)>,
    targets: &[&str],
    mut edit: impl FnMut(&str, Vec<u8>) -> Result<Vec<u8>>,
) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let mut pending: Vec<_> = additions
        .into_iter()
        .filter(|(name, _)| zip.index_for_name(name).is_none())
        .collect();
    for &name in targets {
        let Some(index) = zip.index_for_name(name) else {
            continue;
        };
        let mut original = Vec::new();
        zip.by_index(index)?.read_to_end(&mut original)?;
        let updated = edit(name, original.clone())?;
        if updated != original {
            pending.push((name.to_owned(), updated));
        }
    }
    drop(zip);
    crate::patch::append_entries(path, &pending)
}

/// 把图表写进文件（新增 chart/drawing 部件，并挂到工作表上）
pub fn insert_chart(path: &Path, sheet_name: &str, ch: &Chart) -> Result<()> {
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

    let existing_drawing = {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let mut rels = String::new();
        if let Ok(mut f) = zip.by_name(&sheet_rels_part) {
            f.read_to_string(&mut rels)?;
        }
        if rels.is_empty() {
            None
        } else {
            let doc = roxmltree::Document::parse(&rels)?;
            let targets: std::collections::HashMap<_, _> = doc
                .root_element()
                .children()
                .filter(|n| {
                    n.attribute("Type")
                        .is_some_and(|t| t.ends_with("/drawing"))
                        && n.attribute("TargetMode") != Some("External")
                })
                .filter_map(|n| Some((n.attribute("Id")?, n.attribute("Target")?)))
                .collect();
            if targets.is_empty() {
                None
            } else {
                let mut reader = quick_xml::NsReader::from_reader(std::io::BufReader::new(
                    zip.by_name(&sheet_path)?,
                ));
                let mut buf = Vec::new();
                let mut skip = Vec::new();
                let mut root = false;
                loop {
                    buf.clear();
                    match reader.read_event_into(&mut buf)? {
                        quick_xml::events::Event::Start(e)
                            if !root && e.local_name().as_ref() == "worksheet" =>
                        {
                            let (ns, _) = reader.resolver().resolve_element(e.name());
                            anyhow::ensure!(
                                !matches!(ns, quick_xml::name::ResolveResult::Bound(n) if n.0 == "http://purl.oclc.org/ooxml/spreadsheetml/main"),
                                "暂不支持改写 Strict 工作表中的绘图部件"
                            );
                            root = true;
                        }
                        quick_xml::events::Event::Empty(e)
                        | quick_xml::events::Event::Start(e)
                            if root && e.local_name().as_ref() == "drawing" =>
                        {
                            let id = e
                                .attributes()
                                .flatten()
                                .find(|a| a.key.local_name().as_ref() == "id")
                                .map(|a| a.value.into_owned())
                                .ok_or_else(|| anyhow::anyhow!("绘图缺少关系标识"))?;
                            let target = targets
                                .get(id.as_str())
                                .ok_or_else(|| anyhow::anyhow!("找不到工作表绘图关系"))?;
                            break Some(crate::fixups::resolve_target(&sheet_rels_part, target));
                        }
                        quick_xml::events::Event::Start(e) if root => {
                            skip.clear();
                            reader.read_to_end_into(e.name(), &mut skip)?;
                        }
                        quick_xml::events::Event::Eof => break None,
                        _ => {}
                    }
                }
            }
        }
    };

    if let Some(dpart) = existing_drawing {
        let mut dxml = String::new();
        let mut drels = String::new();
        let drels_part;
        let has_rels;
        {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
            zip.by_name(&dpart)?.read_to_string(&mut dxml)?;
            drels_part = match dpart.rsplit_once('/') {
                Some((d, b)) => format!("{d}/_rels/{b}.rels"),
                None => String::new(),
            };
            if let Ok(mut f) = zip.by_name(&drels_part) {
                f.read_to_string(&mut drels)?;
                has_rels = true;
            } else {
                drels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".into();
                has_rels = false;
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
            .split("cNvPr id=\"")
            .skip(1)
            .filter_map(|c| c.split('"').next().and_then(|n| n.parse::<u32>().ok()))
            .max()
            .unwrap_or(1)
            + 1;
        let anchor = anchor_xml(ch, &rid, docpr);
        let new_drawing = if dxml.contains("</xdr:wsDr>") {
            dxml.replace("</xdr:wsDr>", &format!("{anchor}</xdr:wsDr>"))
        } else if dxml.contains("</wsDr>") {
            let unprefixed = anchor.replace("xdr:", "");
            dxml.replace("</wsDr>", &format!("{unprefixed}</wsDr>"))
        } else {
            dxml.clone()
        };
        if new_drawing == dxml {
            anyhow::bail!("drawing 结构不认识，无法追加图表锚点");
        }
        let new_rels = drels.replace(
            "</Relationships>",
            &format!("<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart\" Target=\"../charts/chart{chart_no}.xml\"/></Relationships>"),
        );
        anyhow::ensure!(new_rels != drels, "无法向绘图关系清单追加图表");
        let chart_name = format!("chart{chart_no}.xml");
        let mut additions = vec![(
            format!("xl/charts/chart{chart_no}.xml"),
            chart_xml(ch).into_bytes(),
        )];
        let dpart_c = dpart.clone();
        let drels_c = drels_part.clone();
        let new_drawing_c = new_drawing.into_bytes();
        let new_rels_c = new_rels.into_bytes();
        if !has_rels {
            additions.push((drels_part.clone(), new_rels_c.clone()));
        }
        rewrite_with_additions_pub(
            path,
            additions,
            &[&dpart, &drels_part, "[Content_Types].xml"],
            move |name, buf| {
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
            },
        )?;
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
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId{next_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"../drawings/drawing{drawing_no}.xml\"/></Relationships>"
        );
        additions.push((sheet_rels_part.clone(), body.into_bytes()));
    }

    let sheet_path2 = sheet_path.clone();
    let sheet_rels2 = sheet_rels_part.clone();
    let drawing_name = format!("drawing{drawing_no}.xml");
    let chart_name = format!("chart{chart_no}.xml");
    rewrite_with_additions_checked(
        path,
        additions,
        &[&sheet_path, &sheet_rels_part, "[Content_Types].xml"],
        move |name, buf| {
            if name == sheet_rels2 {
                let xml = crate::xlsx::patch_xml_text(&buf, name)?;
                return Ok(crate::textbox::add_drawing_relationship(
                    &xml,
                    &format!("rId{next_rid}"),
                    &format!("../drawings/{drawing_name}"),
                )?
                .into_bytes());
            }
            if name == sheet_path2 {
                let xml = crate::xlsx::patch_xml_text(&buf, name)?;
                return Ok(
                    crate::textbox::insert_drawing_tag(&xml, &format!("rId{next_rid}"))?
                        .into_bytes(),
                );
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
                    return Ok(x.into_bytes());
                }
                return Ok(buf);
            }
            Ok(buf)
        },
    )?;
    Ok(())
}

/// 按系列引用重新读取缓存值（支持纵向列与横向行、气泡 size_ref、散点 X/Y 成对校验）。
pub fn refresh_series_from_refs(
    wb: &crate::model::Workbook,
    sheet_name: &str,
    ch: &mut Chart,
) -> Result<()> {
    let find_sheet = |name: Option<&str>| -> Result<&crate::model::Sheet> {
        let n = name.unwrap_or(sheet_name);
        wb.sheets
            .iter()
            .find(|s| s.name == n)
            .ok_or_else(|| anyhow::anyhow!("找不到工作表 {n}"))
    };
    let num = |v: &crate::model::Value| -> Option<f64> {
        match v {
            crate::model::Value::Number(n) => Some(*n),
            other => other.display().trim().parse::<f64>().ok(),
        }
    };
    let expand_cells = |rect: (u32, u32, u32, u32)| -> Vec<(u32, u32)> {
        let mut pts = Vec::new();
        for r in rect.0..=rect.2 {
            for c in rect.1..=rect.3 {
                pts.push((r, c));
            }
        }
        pts
    };
    for (i, se) in ch.series.iter_mut().enumerate() {
        let (sn, vr) = crate::sparkline::parse_ref(&se.val_ref)
            .ok_or_else(|| anyhow::anyhow!("系列{} 数值引用无效：{}", i + 1, se.val_ref))?;
        let sheet = find_sheet(sn.as_deref())?;
        let val_cells = expand_cells(vr);
        let cat_cells = if se.cat_ref.trim().is_empty() {
            None
        } else {
            let (cn, cr) = crate::sparkline::parse_ref(&se.cat_ref)
                .ok_or_else(|| anyhow::anyhow!("系列{} 分类引用无效：{}", i + 1, se.cat_ref))?;
            if cn.as_deref().map(|n| n != sheet.name).unwrap_or(false) {
                anyhow::bail!(
                    "系列{} 的分类与数值不在同一张表（{} vs {}）",
                    i + 1,
                    cn.unwrap_or_default(),
                    sheet.name
                );
            }
            Some(expand_cells(cr))
        };
        let size_cells = if se.size_ref.trim().is_empty() {
            None
        } else {
            let (_, sr) = crate::sparkline::parse_ref(&se.size_ref)
                .ok_or_else(|| anyhow::anyhow!("系列{} 气泡大小引用无效：{}", i + 1, se.size_ref))?;
            Some(expand_cells(sr))
        };
        let skind = se.kind.unwrap_or(ch.kind);
        let mut vals = Vec::new();
        let mut cats = Vec::new();
        let mut xv = Vec::new();
        let mut sizes = Vec::new();
        for (k, &(r, c)) in val_cells.iter().enumerate() {
            let y = sheet.get(r, c).and_then(|cell| num(&cell.value));
            let xcell = cat_cells
                .as_ref()
                .and_then(|list| list.get(k).or_else(|| list.last()))
                .and_then(|&(rr, cc)| sheet.get(rr, cc));
            if let Some(cell) = xcell.as_deref() {
                cats.push(cell.value.display());
            }
            if matches!(skind, ChartKind::Scatter | ChartKind::Bubble) {
                let x = xcell.as_deref().and_then(|cell| num(&cell.value));
                if let (Some(x), Some(y)) = (x, y) {
                    xv.push(x);
                    vals.push(y);
                    if skind == ChartKind::Bubble {
                        let sz = size_cells
                            .as_ref()
                            .and_then(|list| list.get(k))
                            .and_then(|&(sr, sc)| sheet.get(sr, sc))
                            .and_then(|cell| num(&cell.value))
                            .unwrap_or(10.0);
                        sizes.push(sz);
                    }
                }
            } else if let Some(y) = y {
                vals.push(y);
            }
        }
        if vals.is_empty() {
            anyhow::bail!("系列{} 的数值区域取不到任何数字：{}", i + 1, se.val_ref);
        }
        se.cats = cats;
        se.vals = vals;
        if matches!(skind, ChartKind::Scatter | ChartKind::Bubble) {
            se.xv = xv;
        }
        if skind == ChartKind::Bubble {
            se.sizes = sizes;
        }
    }
    Ok(())
}

/// Update exact formula nodes once per batch, leaving cached values, labels and markup alone.
pub fn replace_refs(path: &Path, chart_part: &str, pairs: &[(String, String)]) -> Result<()> {
    if pairs.is_empty() { return Ok(()); }
    patch_series_refs(path, chart_part, |_, _, old| {
        pairs.iter().find(|(from, _)| {
            if from == old { return true; }
            // Some spreadsheet writers omit the optional quotes around a sheet name.
            let Some((quoted, cell)) = from.strip_prefix('\'').and_then(|f| f.rsplit_once("'!")) else {
                return false;
            };
            old.rsplit_once('!').is_some_and(|(sheet, address)| {
                cell == address && quoted.replace("''", "'") == sheet
            })
        }).map(|(_, to)| to.as_str())
    })
}

/// Use the current field of each series: a sequence of edits and undos can have
/// the same old reference in two different roles with different final values.
pub(crate) fn write_series_refs(path: &Path, chart: &Chart) -> Result<()> {
    patch_series_refs(path, &chart.part, |index, field, _| {
        let series = chart.series.get(index)?;
        let value = match field {
            "tx" => &series.name_ref,
            "cat" | "xVal" | "strDim" => &series.cat_ref,
            "val" | "yVal" | "numDim" => &series.val_ref,
            "bubbleSize" => &series.size_ref,
            _ => return None,
        };
        (!value.is_empty()).then_some(value.as_str())
    })
}

fn patch_series_refs<'a>(path: &Path, part: &str,
    target: impl Fn(usize, &str, &str) -> Option<&'a str>,
) -> Result<()> {
    let mut xml = String::new();
    zip::ZipArchive::new(std::fs::File::open(path)?)?.by_name(part)?.read_to_string(&mut xml)?;
    if let Some(updated) = series_refs_xml(&xml, target)? {
        crate::patch::append_entry(path, part, updated.as_bytes())?;
    }
    Ok(())
}

fn series_refs_xml<'a>(xml: &str, target: impl Fn(usize, &str, &str) -> Option<&'a str>)
    -> Result<Option<String>> {
    let doc = roxmltree::Document::parse(xml)?;
    let ns = doc.root_element().tag_name().namespace();
    let tag = |n: roxmltree::Node, name: &str| {
        n.is_element() && n.tag_name().name() == name && n.tag_name().namespace() == ns
    };
    let mut edits = Vec::new();
    let mut patch = |index, field: &str, node: roxmltree::Node| -> Result<()> {
        let text: String = node.children().filter(|c| c.is_text()).filter_map(|c| c.text()).collect();
        let old = text.trim();
        if let Some(new) = target(index, field, old).filter(|new| *new != old) {
            // roxmltree coalesces adjacent text and CDATA. Use XML token byte spans
            // so character entities, split CDATA and intervening comments stay valid.
            let range = node.range();
            let mut reader = quick_xml::Reader::from_str(&xml[range.clone()]);
            let mut replacement = Some(esc(new));
            loop {
                let start = range.start + reader.buffer_position() as usize;
                match reader.read_event()? {
                    quick_xml::events::Event::Text(_) | quick_xml::events::Event::CData(_)
                    | quick_xml::events::Event::GeneralRef(_) => {
                        let end = range.start + reader.buffer_position() as usize;
                        edits.push((start..end, replacement.take().unwrap_or_default()));
                    }
                    quick_xml::events::Event::Eof => break,
                    _ => {}
                }
            }
        }
        Ok(())
    };
    // Classic chart series occur in plot order, including combo charts.
    let series = doc.descendants().filter(|n| tag(*n, "ser")
        && n.parent_element().is_some_and(|p| p.tag_name().namespace() == ns
            && p.tag_name().name().ends_with("Chart") && p.ancestors().any(|a| tag(a, "plotArea"))));
    for (i, series) in series.enumerate() {
        for field in series.children().filter(|n| n.is_element() && n.tag_name().namespace() == ns) {
            let label = field.tag_name().name();
            if !matches!(label, "tx" | "cat" | "xVal" | "val" | "yVal" | "bubbleSize") { continue; }
            if let Some(f) = field.children()
                .filter(|n| n.is_element() && n.tag_name().namespace() == ns
                    && matches!(n.tag_name().name(), "strRef" | "numRef" | "multiLvlStrRef"))
                .flat_map(|n| n.children())
                .find(|n| tag(*n, "f")) {
                patch(i, label, f)?;
            }
        }
    }
    // The chartEx model stores the first public formula of each data dimension.
    for label in ["strDim", "numDim"] {
        let f = doc.descendants().filter(|n| tag(*n, label))
            .filter(|n| n.parent_element().is_some_and(|p| tag(p, "data")))
            .flat_map(|n| n.children()).find(|n| tag(*n, "f")
                && n.text().is_some_and(|t| !t.trim().starts_with("_xlchart.")));
        if let Some(f) = f { patch(0, label, f)?; }
    }
    if edits.is_empty() { return Ok(None); }
    edits.sort_unstable_by_key(|(r, _)| r.start);
    let mut out = String::with_capacity(xml.len());
    let mut pos = 0;
    for (range, replacement) in edits {
        out.push_str(&xml[pos..range.start]);
        out.push_str(&replacement);
        pos = range.end;
    }
    out.push_str(&xml[pos..]);
    Ok(Some(out))
}

/// 换掉已有图表的内容，保留锚点与部件路径
pub fn replace_chart(path: &Path, chart_part: &str, ch: &Chart) -> Result<()> {
    if matches!(ch.kind, ChartKind::Unknown) || ch.is_chartex {
        anyhow::bail!("该图表类型不受支持，只能保留原图");
    }
    let body = chart_xml(ch);
    crate::patch::append_entry(path, chart_part, body.as_bytes())?;
    Ok(())
}

/// 删除一个图表：摘掉 drawing 里的锚点、drawing rels 的条目、chart 部件本身及 Content_Types
pub fn delete_chart(path: &Path, drawing_part: &str, chart_part: &str) -> Result<()> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut drawing = String::new();
    {
        let mut f = zip.by_name(drawing_part).context("drawing 部件不存在")?;
        f.read_to_string(&mut drawing)?;
    }
    let rels_part = match drawing_part.rsplit_once('/') {
        Some((d, b)) => format!("{d}/_rels/{b}.rels"),
        None => return Err(anyhow::anyhow!("drawing 路径异常")),
    };
    let mut rels = String::new();
    if let Ok(mut f) = zip.by_name(&rels_part) {
        let _ = f.read_to_string(&mut rels);
    }
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
    let pre = if drawing.contains("xmlns:xdr=") {
        "xdr:"
    } else {
        ""
    };
    let mut out = String::new();
    let mut i = 0usize;
    let bytes = drawing.as_str();
    let anchor_opens = [format!("<{pre}oneCellAnchor"), format!("<{pre}twoCellAnchor")];
    while i < bytes.len() {
        let next = anchor_opens
            .iter()
            .filter_map(|tag| bytes[i..].find(tag).map(|p| (i + p, tag.as_str())))
            .min_by_key(|(p, _)| *p);
        let Some((start, open_tag)) = next else {
            out.push_str(&bytes[i..]);
            break;
        };
        out.push_str(&bytes[i..start]);
        let kind = if open_tag.ends_with("oneCellAnchor") {
            "oneCellAnchor"
        } else {
            "twoCellAnchor"
        };
        let close = format!("</{pre}{kind}>");
        let end = bytes[start..]
            .find(&close)
            .map(|x| start + x + close.len())
            .unwrap_or(bytes.len());
        let block = &bytes[start..end];
        let keep = rid.is_empty() || !block.contains(&format!("r:id=\"{rid}\""));
        if keep {
            out.push_str(block);
        }
        i = end;
    }

    crate::patch::append_entry(path, drawing_part, out.as_bytes())?;
    if !rid.is_empty() {
        let mut new_rels = rels.clone();
        let mut k = 0usize;
        while let Some(p) = new_rels[k..].find("<Relationship ") {
            let start = k + p;
            let end = new_rels[start..]
                .find("/>")
                .map(|x| start + x + 2)
                .unwrap_or(new_rels.len());
            let block = &new_rels[start..end];
            if block.contains(&format!("Id=\"{rid}\"")) {
                new_rels.replace_range(start..end, "");
                break;
            }
            k = end;
        }
        crate::patch::append_entry(path, &rels_part, new_rels.as_bytes())?;
    }
    crate::patch::remove_entry(path, chart_part)?;
    let part_name = format!("/{chart_part}");
    crate::patch::rewrite_zip_where(path, |part| part == "[Content_Types].xml", |name, buf| {
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

/// 把 drawing 里第 ordinal 个图表锚点改成单元格锚点（twoCellAnchor，单元格偏移 0）
pub fn rewrite_anchor_xml(xml: &str, ordinal: usize, anchor: (u32, u32, u32, u32)) -> String {
    let (r1, c1, r2, c2) = anchor;
    let (r2, c2) = (r2.max(r1 + 1), c2.max(c1 + 1));
    let pre = if xml.contains("xmlns:xdr=") {
        "xdr:"
    } else {
        ""
    };
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
    let Some((cells, rid)) = crate::charts::parse_drawing_anchors(old).into_iter().next() else {
        return xml.to_string();
    };
    if cells == anchor {
        return xml.to_string();
    }
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
    if edited != xml {
        crate::patch::append_entry(path, drawing_part, edited.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod anchor_tests {
    use super::*;

    #[test]
    fn moving_the_second_native_chart_preserves_its_relationship() {
        let sheet = crate::model::Sheet::new("Data");
        let chart = chart_from_selection(&sheet, "Data", (0, 0, 2, 1), "Sales", ChartKind::Column);
        let first = anchor_xml(&chart, "rId1", 2);
        let second = anchor_xml(&chart, "rId7", 3);
        let xml = format!("<xdr:wsDr xmlns:xdr=\"urn:drawing\">{first}{second}</xdr:wsDr>");
        let moved = (2, 1, 12, 8);
        let after = rewrite_anchor_xml(&xml, 1, moved);
        assert!(after.contains(&first));
        let anchors = crate::charts::parse_drawing_anchors(&after);
        assert_eq!(anchors[1], (moved, "rId7".to_owned()));
    }

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

#[cfg(test)]
mod series_reference_tests {
    use super::*;

    #[test]
    fn writeback_patches_each_series_field_once_and_preserves_other_xml() {
        let xml = r#"<c:chartSpace xmlns:c="urn:chart" xmlns:e="urn:extension"><c:chart>
<c:title>Data!A1</c:title><c:plotArea><c:lineChart>
<c:ser><c:tx><c:strRef><c:f>Data!A10</c:f></c:strRef></c:tx>
<c:val><c:numRef><c:f>Data!A1</c:f><c:numCache>Data!A1</c:numCache></c:numRef></c:val>
<c:extLst><e:f>Data!A1</e:f></c:extLst></c:ser>
<c:ser><c:val><c:numRef><c:f>Data!A2</c:f></c:numRef></c:val></c:ser>
<c:ser><c:val><c:numRef><c:f><![CDATA[Da]]>ta!A1<!--keep--></c:f></c:numRef></c:val></c:ser>
</c:lineChart></c:plotArea></c:chart></c:chartSpace>"#;
        let series = ["Data!A2", "Data!A3", "'R&D'!B1"];
        let out = series_refs_xml(xml, |index, field, _| (field == "val").then_some(series[index]))
            .unwrap().unwrap();
        let expected = xml.replace("<c:f>Data!A2</c:f>", "<c:f>Data!A3</c:f>")
            .replace("<c:f>Data!A1</c:f>", "<c:f>Data!A2</c:f>")
            .replace("<![CDATA[Da]]>ta!A1<!--keep-->", "'R&amp;D'!B1<!--keep-->");
        assert_eq!(out, expected);
        assert!(roxmltree::Document::parse(&out).is_ok());
    }

    #[test]
    fn chartex_only_changes_the_dimension_formulas_exposed_by_the_model() {
        let xml = r#"<cx:chartSpace xmlns:cx="urn:cx"><cx:chartData><cx:data>
<cx:strDim><cx:f>_xlchart.v1.private</cx:f></cx:strDim>
<cx:strDim><cx:f dir="col">S!A1</cx:f></cx:strDim>
<cx:strDim><cx:f>S!A10</cx:f></cx:strDim>
<cx:numDim><cx:f>S!B1</cx:f></cx:numDim>
</cx:data></cx:chartData></cx:chartSpace>"#;
        let out = series_refs_xml(xml, |_, field, _| match field {
            "strDim" => Some("S!C1"), "numDim" => Some("S!D1"), _ => None
        }).unwrap().unwrap();
        assert_eq!(out, xml.replace(">S!A1<", ">S!C1<").replace(">S!B1<", ">S!D1<"));
    }
}
