//! 出版级高清矢量 SVG 图表导出引擎（纯 Rust 零外部依赖）。
//!
//! 共享 `chart_engine` 的主题配色、Nice Ticks、回归趋势线、误差线、参考线/目标带、
//! 瀑布/漏斗/树图/直方图/箱线图与牵引线防撞排版算法，直接生成可直接嵌入 PPT / Word / 研报的高清 `.svg`。

use crate::chart_engine::{
    compute_axis_scale, compute_box_whisker, compute_error_bar_deltas, compute_funnel,
    compute_histogram, compute_insight_summary, compute_treemap, compute_trendline,
    compute_waterfall, format_chart_number, format_percent, layout_pie_callouts,
    resolve_reference_line, series_rgb, theme_spec, ResolvedRefLine, WaterfallBarKind,
};
use crate::charts::{Chart, ChartKind, LineDash, RefLineAxis, Series};
use crate::model::DateSystem;
use std::fmt::Write as _;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn dash_attr(d: LineDash) -> &'static str {
    match d {
        LineDash::Solid => "",
        LineDash::Dash => " stroke-dasharray=\"6,4\"",
        LineDash::Dot => " stroke-dasharray=\"2,3\"",
        LineDash::DashDot => " stroke-dasharray=\"6,3,2,3\"",
    }
}

/// 将任意图表导出为独立高清矢量 SVG 字符串
pub fn render_chart_svg(chart: &Chart, width: f32, height: f32, sys: DateSystem) -> String {
    let w = width.max(240.0);
    let h = height.max(180.0);
    let th = theme_spec(chart.style.theme);
    let mut out = String::with_capacity(8192);
    let _ = writeln!(
        out,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.0}\" height=\"{h:.0}\" viewBox=\"0 0 {w:.0} {h:.0}\" \
         style=\"font-family: 'Microsoft YaHei UI', 'PingFang SC', 'Segoe UI', sans-serif;\">"
    );
    if !chart.style.transparent_bg {
        let _ = writeln!(
            out,
            "<rect x=\"0.5\" y=\"0.5\" width=\"{:.1}\" height=\"{:.1}\" rx=\"8\" fill=\"{}\" stroke=\"{}\" stroke-width=\"1\"/>",
            w - 1.0,
            h - 1.0,
            hex(th.bg),
            hex(th.border)
        );
    }

    let mut top = 16.0f32;
    if !chart.title.is_empty() {
        let _ = writeln!(
            out,
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"15\" font-weight=\"700\" fill=\"{}\">{}</text>",
            w * 0.5,
            top + 14.0,
            hex(th.title_fg),
            esc(&chart.title)
        );
        top += 24.0;
    }
    if !chart.subtitle.is_empty() {
        let _ = writeln!(
            out,
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"11\" fill=\"{}\">{}</text>",
            w * 0.5,
            top + 10.0,
            hex(th.subtitle_fg),
            esc(&chart.subtitle)
        );
        top += 18.0;
    }

    let mut bottom = h - 14.0;
    if chart.analytics.show_summary_bar
        && let Some(sum) = compute_insight_summary(chart, sys)
    {
        let _ = writeln!(
            out,
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"10\" fill=\"{}\">{}</text>",
            w * 0.5,
            bottom,
            hex(th.muted_fg),
            esc(&sum.text)
        );
        bottom -= 18.0;
    }

    let vis_series: Vec<(usize, &Series)> = chart
        .series
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.hidden)
        .collect();

    // 图例（底部居中）
    let mut right_pad = 22.0f32;
    if chart.legend && !vis_series.is_empty() && !matches!(chart.kind, ChartKind::Waterfall | ChartKind::Funnel | ChartKind::Treemap) {
        if chart.legend_pos == "r" {
            right_pad = 104.0;
            let mut ly = top + 16.0;
            for &(si, s) in vis_series.iter().take(10) {
                let c = hex(series_rgb(chart, si));
                let _ = writeln!(
                    out,
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"10\" height=\"10\" rx=\"2\" fill=\"{c}\"/>",
                    w - right_pad + 10.0,
                    ly - 8.0
                );
                let _ = writeln!(
                    out,
                    "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" fill=\"{}\">{}</text>",
                    w - right_pad + 24.0,
                    ly + 1.0,
                    hex(th.text_fg),
                    esc(if s.name.is_empty() { "系列" } else { &s.name })
                );
                ly += 18.0;
            }
        } else {
            let mut lx = 24.0f32;
            let ly = bottom - 4.0;
            for &(si, s) in vis_series.iter().take(8) {
                let c = hex(series_rgb(chart, si));
                let name = if s.name.is_empty() { "系列" } else { s.name.as_str() };
                let _ = writeln!(
                    out,
                    "<rect x=\"{lx:.1}\" y=\"{:.1}\" width=\"10\" height=\"10\" rx=\"2\" fill=\"{c}\"/>",
                    ly - 9.0
                );
                let _ = writeln!(
                    out,
                    "<text x=\"{:.1}\" y=\"{ly:.1}\" font-size=\"11\" fill=\"{}\">{}</text>",
                    lx + 14.0,
                    hex(th.text_fg),
                    esc(name)
                );
                lx += 26.0 + (name.chars().count() as f32 * 9.0).min(110.0);
                if lx > w - 60.0 {
                    break;
                }
            }
            bottom -= 22.0;
        }
    }

    let has_y2 = chart.has_secondary_axis()
        && matches!(chart.kind, ChartKind::Combo | ChartKind::Column | ChartKind::Line);
    let left_pad = if chart.kind.is_horizontal() { 82.0 } else { 58.0 };
    if has_y2 {
        right_pad = right_pad.max(56.0);
    }
    let px0 = left_pad;
    let py0 = top + 6.0;
    let pw = (w - left_pad - right_pad).max(60.0);
    let ph = (bottom - py0 - 28.0).max(50.0);

    match chart.kind {
        ChartKind::Pie | ChartKind::Doughnut => {
            render_pie_svg(&mut out, chart, px0, py0, pw, ph, sys);
        }
        ChartKind::Treemap => {
            if let Some((_, s)) = vis_series.first() {
                let tiles = compute_treemap(s, px0, py0, pw, ph);
                for (i, t) in tiles.iter().enumerate() {
                    let c = hex(th.palette[i % th.palette.len()]);
                    let _ = writeln!(
                        out,
                        "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"3\" fill=\"{c}\" stroke=\"{}\" stroke-width=\"1.5\"/>",
                        t.x,
                        t.y,
                        t.w.max(1.0),
                        t.h.max(1.0),
                        hex(th.bg)
                    );
                    if t.w > 40.0 && t.h > 22.0 {
                        let _ = writeln!(
                            out,
                            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"11\" font-weight=\"600\" fill=\"#FFFFFF\">{}</text>",
                            t.x + t.w * 0.5,
                            t.y + t.h * 0.5 - 2.0,
                            esc(&t.label)
                        );
                        if t.h > 36.0 {
                            let _ = writeln!(
                                out,
                                "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"10\" fill=\"#FFFFFF\" fill-opacity=\"0.88\">{}</text>",
                                t.x + t.w * 0.5,
                                t.y + t.h * 0.5 + 12.0,
                                format_percent(t.share)
                            );
                        }
                    }
                }
            }
        }
        ChartKind::Funnel => {
            if let Some((_, s)) = vis_series.first() {
                let stages = compute_funnel(s);
                let n = stages.len().max(1);
                let row_h = (ph / n as f32).min(48.0);
                let y_off = py0 + (ph - row_h * n as f32) * 0.5;
                let cx = px0 + pw * 0.5;
                for (i, st) in stages.iter().enumerate() {
                    let w_top = pw * 0.72 * st.width_frac as f32;
                    let w_bot = stages
                        .get(i + 1)
                        .map(|nx| pw * 0.72 * nx.width_frac as f32)
                        .unwrap_or(w_top * 0.82);
                    let y1 = y_off + i as f32 * row_h + 2.0;
                    let y2 = y1 + row_h - 4.0;
                    let c = hex(th.palette[i % th.palette.len()]);
                    let _ = writeln!(
                        out,
                        "<polygon points=\"{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" fill=\"{c}\"/>",
                        cx - w_top * 0.5,
                        y1,
                        cx + w_top * 0.5,
                        y1,
                        cx + w_bot * 0.5,
                        y2,
                        cx - w_bot * 0.5,
                        y2
                    );
                    let val_s = format_chart_number(st.value, &chart.effective_y_axis().num_fmt, sys);
                    let conv = st
                        .step_conv
                        .map(|c| format!(" ({})", format_percent(c)))
                        .unwrap_or_default();
                    let _ = writeln!(
                        out,
                        "<text x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"11\" font-weight=\"600\" fill=\"#FFFFFF\">{} · {}{}</text>",
                        (y1 + y2) * 0.5 + 4.0,
                        esc(&st.label),
                        esc(&val_s),
                        esc(&conv)
                    );
                }
            }
        }
        ChartKind::Radar => {
            render_radar_svg(&mut out, chart, &vis_series, px0, py0, pw, ph);
        }
        ChartKind::Waterfall => {
            if let Some((_, s)) = vis_series.first() {
                let bars = compute_waterfall(s, &chart.analytics.waterfall_totals);
                let mut dmin = 0.0f64;
                let mut dmax = 0.0f64;
                for b in &bars {
                    dmin = dmin.min(b.y_start).min(b.y_end);
                    dmax = dmax.max(b.y_start).max(b.y_end);
                }
                let y_ax = chart.effective_y_axis();
                let sc = compute_axis_scale(dmin, dmax, &y_ax, true, 6);
                draw_cartesian_grid_svg(&mut out, chart, &sc, px0, py0, pw, ph, sys);
                let n = bars.len().max(1);
                let slot = pw / n as f32;
                let bw = (slot * 0.62).max(6.0);
                for (i, b) in bars.iter().enumerate() {
                    let bx = px0 + i as f32 * slot + (slot - bw) * 0.5;
                    let ya = py0 + ph * (1.0 - sc.norm(b.y_start));
                    let yb = py0 + ph * (1.0 - sc.norm(b.y_end));
                    let top_y = ya.min(yb);
                    let bh = (ya - yb).abs().max(2.0);
                    let col = match b.kind {
                        WaterfallBarKind::Increase => th.pos_color,
                        WaterfallBarKind::Decrease => th.neg_color,
                        WaterfallBarKind::Total => th.total_color,
                    };
                    let rx = if chart.style.rounded_bars { 3.0 } else { 0.0 };
                    let _ = writeln!(
                        out,
                        "<rect x=\"{bx:.1}\" y=\"{top_y:.1}\" width=\"{bw:.1}\" height=\"{bh:.1}\" rx=\"{rx}\" fill=\"{}\"/>",
                        hex(col)
                    );
                    if i + 1 < bars.len() {
                        let nx = px0 + (i + 1) as f32 * slot + (slot - bw) * 0.5;
                        let _ = writeln!(
                            out,
                            "<line x1=\"{:.1}\" y1=\"{yb:.1}\" x2=\"{nx:.1}\" y2=\"{yb:.1}\" stroke=\"{}\" stroke-width=\"1\" stroke-dasharray=\"3,3\"/>",
                            bx + bw,
                            hex(th.axis_line)
                        );
                    }
                    let _ = writeln!(
                        out,
                        "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"10\" fill=\"{}\">{}</text>",
                        bx + bw * 0.5,
                        py0 + ph + 15.0,
                        hex(th.text_fg),
                        esc(&b.label)
                    );
                    if chart.show_labels {
                        let lbl = format_chart_number(b.delta, &y_ax.num_fmt, sys);
                        let _ = writeln!(
                            out,
                            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"10\" font-weight=\"600\" fill=\"{}\">{}</text>",
                            bx + bw * 0.5,
                            top_y - 4.0,
                            hex(th.title_fg),
                            esc(&lbl)
                        );
                    }
                }
            }
        }
        ChartKind::Histogram => {
            if let Some((_, s)) = vis_series.first() {
                let bins = compute_histogram(s, chart.analytics.hist_bins, chart.analytics.show_pareto_line);
                let max_c = bins.iter().map(|b| b.count).max().unwrap_or(1) as f64;
                let y_ax = chart.effective_y_axis();
                let sc = compute_axis_scale(0.0, max_c, &y_ax, true, 6);
                draw_cartesian_grid_svg(&mut out, chart, &sc, px0, py0, pw, ph, sys);
                let n = bins.len().max(1);
                let slot = pw / n as f32;
                let col = hex(series_rgb(chart, 0));
                let mut pareto_pts = String::new();
                for (i, b) in bins.iter().enumerate() {
                    let bx = px0 + i as f32 * slot + 1.5;
                    let bw = (slot - 3.0).max(2.0);
                    let vy = py0 + ph * (1.0 - sc.norm(b.count as f64));
                    let _ = writeln!(
                        out,
                        "<rect x=\"{bx:.1}\" y=\"{vy:.1}\" width=\"{bw:.1}\" height=\"{:.1}\" rx=\"2\" fill=\"{col}\"/>",
                        (py0 + ph - vy).max(1.0)
                    );
                    let cx = bx + bw * 0.5;
                    if chart.analytics.show_pareto_line {
                        let py = py0 + ph * (1.0 - b.cum_pct as f32);
                        let _ = write!(pareto_pts, "{cx:.1},{py:.1} ");
                    }
                    if n <= 12 || i % (n / 10).max(1) == 0 {
                        let _ = writeln!(
                            out,
                            "<text x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"9\" fill=\"{}\">{}</text>",
                            py0 + ph + 14.0,
                            hex(th.text_fg),
                            esc(&b.label)
                        );
                    }
                }
                if chart.analytics.show_pareto_line && !pareto_pts.is_empty() {
                    let pcol = hex(th.palette[1]);
                    let _ = writeln!(
                        out,
                        "<polyline fill=\"none\" stroke=\"{pcol}\" stroke-width=\"2\" points=\"{pareto_pts}\"/>"
                    );
                }
            }
        }
        ChartKind::BoxWhisker => {
            let boxes = compute_box_whisker(chart);
            let mut dmin = f64::INFINITY;
            let mut dmax = f64::NEG_INFINITY;
            for b in &boxes {
                dmin = dmin.min(b.whisker_lo);
                dmax = dmax.max(b.whisker_hi);
                for &o in &b.outliers {
                    dmin = dmin.min(o);
                    dmax = dmax.max(o);
                }
            }
            let y_ax = chart.effective_y_axis();
            let sc = compute_axis_scale(dmin, dmax, &y_ax, false, 6);
            draw_cartesian_grid_svg(&mut out, chart, &sc, px0, py0, pw, ph, sys);
            let n = boxes.len().max(1);
            let slot = pw / n as f32;
            let bw = (slot * 0.44).clamp(12.0, 56.0);
            for (i, b) in boxes.iter().enumerate() {
                let cx = px0 + (i as f32 + 0.5) * slot;
                let y_of = |v: f64| py0 + ph * (1.0 - sc.norm(v));
                let col = hex(th.palette[i % th.palette.len()]);
                let y_lo = y_of(b.whisker_lo);
                let y_hi = y_of(b.whisker_hi);
                let y_q1 = y_of(b.q1);
                let y_q3 = y_of(b.q3);
                let y_med = y_of(b.median);
                let y_mean = y_of(b.mean);
                let _ = writeln!(
                    out,
                    "<line x1=\"{cx:.1}\" y1=\"{y_lo:.1}\" x2=\"{cx:.1}\" y2=\"{y_hi:.1}\" stroke=\"{col}\" stroke-width=\"1.5\"/>"
                );
                let _ = writeln!(
                    out,
                    "<line x1=\"{:.1}\" y1=\"{y_lo:.1}\" x2=\"{:.1}\" y2=\"{y_lo:.1}\" stroke=\"{col}\" stroke-width=\"1.5\"/>",
                    cx - bw * 0.3,
                    cx + bw * 0.3
                );
                let _ = writeln!(
                    out,
                    "<line x1=\"{:.1}\" y1=\"{y_hi:.1}\" x2=\"{:.1}\" y2=\"{y_hi:.1}\" stroke=\"{col}\" stroke-width=\"1.5\"/>",
                    cx - bw * 0.3,
                    cx + bw * 0.3
                );
                let _ = writeln!(
                    out,
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{bw:.1}\" height=\"{:.1}\" rx=\"2\" fill=\"{col}\" fill-opacity=\"0.25\" stroke=\"{col}\" stroke-width=\"1.6\"/>",
                    cx - bw * 0.5,
                    y_q3.min(y_q1),
                    (y_q1 - y_q3).abs().max(2.0)
                );
                let _ = writeln!(
                    out,
                    "<line x1=\"{:.1}\" y1=\"{y_med:.1}\" x2=\"{:.1}\" y2=\"{y_med:.1}\" stroke=\"{col}\" stroke-width=\"2.4\"/>",
                    cx - bw * 0.5,
                    cx + bw * 0.5
                );
                let _ = writeln!(
                    out,
                    "<circle cx=\"{cx:.1}\" cy=\"{y_mean:.1}\" r=\"3\" fill=\"#FFFFFF\" stroke=\"{col}\" stroke-width=\"1.5\"/>"
                );
                for &ov in &b.outliers {
                    let yo = y_of(ov);
                    let _ = writeln!(
                        out,
                        "<circle cx=\"{cx:.1}\" cy=\"{yo:.1}\" r=\"3\" fill=\"{col}\"/>"
                    );
                }
                let _ = writeln!(
                    out,
                    "<text x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"10\" fill=\"{}\">{}</text>",
                    py0 + ph + 15.0,
                    hex(th.text_fg),
                    esc(&b.label)
                );
            }
        }
        ChartKind::Scatter | ChartKind::Bubble => {
            render_scatter_bubble_svg(&mut out, chart, &vis_series, px0, py0, pw, ph, sys);
        }
        _ => {
            render_cartesian_svg(&mut out, chart, &vis_series, px0, py0, pw, ph, sys);
        }
    }

    let _ = writeln!(out, "</svg>");
    out
}

fn draw_cartesian_grid_svg(
    out: &mut String,
    chart: &Chart,
    sc: &crate::chart_engine::AxisScale,
    px0: f32,
    py0: f32,
    pw: f32,
    ph: f32,
    sys: DateSystem,
) {
    let th = theme_spec(chart.style.theme);
    let y_ax = chart.effective_y_axis();
    let _ = writeln!(
        out,
        "<rect x=\"{px0:.1}\" y=\"{py0:.1}\" width=\"{pw:.1}\" height=\"{ph:.1}\" fill=\"{}\"/>",
        hex(th.plot_bg)
    );
    for &t in &sc.ticks {
        let y = py0 + ph * (1.0 - sc.norm(t));
        if y_ax.show_major_grid {
            let _ = writeln!(
                out,
                "<line x1=\"{px0:.1}\" y1=\"{y:.1}\" x2=\"{:.1}\" y2=\"{y:.1}\" stroke=\"{}\" stroke-width=\"1\"/>",
                px0 + pw,
                hex(th.grid)
            );
        }
        let lbl = format_chart_number(t, &y_ax.num_fmt, sys);
        let _ = writeln!(
            out,
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\" font-size=\"10\" fill=\"{}\">{}</text>",
            px0 - 6.0,
            y + 3.5,
            hex(th.muted_fg),
            esc(&lbl)
        );
    }
    let _ = writeln!(
        out,
        "<line x1=\"{px0:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{}\" stroke-width=\"1.2\"/>",
        py0 + ph,
        px0 + pw,
        py0 + ph,
        hex(th.axis_line)
    );
    if !y_ax.title.is_empty() {
        let _ = writeln!(
            out,
            "<text x=\"14\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"11\" fill=\"{}\" transform=\"rotate(-90 14 {:.1})\">{}</text>",
            py0 + ph * 0.5,
            hex(th.text_fg),
            py0 + ph * 0.5,
            esc(&y_ax.title)
        );
    }
    let x_ax = chart.effective_x_axis();
    if !x_ax.title.is_empty() {
        let _ = writeln!(
            out,
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"11\" fill=\"{}\">{}</text>",
            px0 + pw * 0.5,
            py0 + ph + 26.0,
            hex(th.text_fg),
            esc(&x_ax.title)
        );
    }
}

fn render_cartesian_svg(
    out: &mut String,
    chart: &Chart,
    vis: &[(usize, &Series)],
    px0: f32,
    py0: f32,
    pw: f32,
    ph: f32,
    sys: DateSystem,
) {
    if vis.is_empty() {
        return;
    }
    let th = theme_spec(chart.style.theme);
    let y_ax = chart.effective_y_axis();
    let cats: &[String] = vis.first().map(|(_, s)| s.cats.as_slice()).unwrap_or(&[]);
    let n = cats
        .len()
        .max(vis.iter().map(|(_, s)| s.vals.len()).max().unwrap_or(0))
        .max(1);

    let (dmin, dmax) = if chart.kind.is_percent_stacked() {
        (0.0, 1.0)
    } else if chart.kind.is_stacked() {
        let mut lo = 0.0f64;
        let mut hi = 0.0f64;
        for i in 0..n {
            let mut pos = 0.0;
            let mut neg = 0.0;
            for (_, s) in vis.iter().filter(|(_, s)| !s.secondary) {
                if let Some(&v) = s.vals.get(i)
                    && v.is_finite()
                {
                    if v >= 0.0 {
                        pos += v;
                    } else {
                        neg += v;
                    }
                }
            }
            hi = hi.max(pos);
            lo = lo.min(neg);
        }
        (lo, hi)
    } else {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for (_, s) in vis.iter().filter(|(_, s)| !s.secondary) {
            for &v in &s.vals {
                if v.is_finite() {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
        }
        (lo, hi)
    };
    let mut y_ax_eff = y_ax.clone();
    if chart.kind.is_percent_stacked() && y_ax_eff.num_fmt.is_empty() {
        y_ax_eff.num_fmt = "0%".into();
    }
    let sc1 = compute_axis_scale(dmin, dmax, &y_ax_eff, !matches!(chart.kind, ChartKind::Line), 6);
    let sc2 = {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for (_, s) in vis.iter().filter(|(_, s)| s.secondary) {
            for &v in &s.vals {
                if v.is_finite() {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
        }
        compute_axis_scale(lo, hi, &chart.y2_axis, true, 6)
    };

    if chart.kind.is_horizontal() {
        let _ = writeln!(
            out,
            "<rect x=\"{px0:.1}\" y=\"{py0:.1}\" width=\"{pw:.1}\" height=\"{ph:.1}\" fill=\"{}\"/>",
            hex(th.plot_bg)
        );
        for &t in &sc1.ticks {
            let x = px0 + pw * sc1.norm(t);
            let _ = writeln!(
                out,
                "<line x1=\"{x:.1}\" y1=\"{py0:.1}\" x2=\"{x:.1}\" y2=\"{:.1}\" stroke=\"{}\" stroke-width=\"1\"/>",
                py0 + ph,
                hex(th.grid)
            );
            let _ = writeln!(
                out,
                "<text x=\"{x:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"10\" fill=\"{}\">{}</text>",
                py0 + ph + 14.0,
                hex(th.muted_fg),
                esc(&format_chart_number(t, &y_ax_eff.num_fmt, sys))
            );
        }
        let row_slot = ph / n as f32;
        let zero_x = px0 + pw * sc1.zero_norm();
        let mut pos_acc = vec![0.0f64; n];
        let mut neg_acc = vec![0.0f64; n];
        let totals: Vec<f64> = (0..n)
            .map(|i| {
                vis.iter()
                    .filter_map(|(_, s)| s.vals.get(i).copied().filter(|v| v.is_finite()))
                    .map(f64::abs)
                    .sum::<f64>()
                    .max(1e-12)
            })
            .collect();
        for (pos_i, &(si, s)) in vis.iter().enumerate() {
            let col = hex(series_rgb(chart, si));
            for (i, &raw) in s.vals.iter().enumerate().take(n) {
                if !raw.is_finite() {
                    continue;
                }
                let v = if chart.kind.is_percent_stacked() {
                    raw / totals[i]
                } else {
                    raw
                };
                if chart.kind.is_stacked() {
                    let (v0, v1) = if v >= 0.0 {
                        let a = pos_acc[i];
                        pos_acc[i] += v;
                        (a, pos_acc[i])
                    } else {
                        let a = neg_acc[i];
                        neg_acc[i] += v;
                        (neg_acc[i], a)
                    };
                    let x0 = px0 + pw * sc1.norm(v0);
                    let x1 = px0 + pw * sc1.norm(v1);
                    let bh = (row_slot * 0.68).max(2.0);
                    let y = py0 + i as f32 * row_slot + (row_slot - bh) * 0.5;
                    let _ = writeln!(
                        out,
                        "<rect x=\"{:.1}\" y=\"{y:.1}\" width=\"{:.1}\" height=\"{bh:.1}\" fill=\"{col}\"/>",
                        x0.min(x1),
                        (x1 - x0).abs().max(1.0)
                    );
                } else {
                    let bh = (row_slot * 0.76 / vis.len().max(1) as f32).max(2.0);
                    let y = py0 + i as f32 * row_slot + row_slot * 0.12 + bh * pos_i as f32;
                    let vx = px0 + pw * sc1.norm(v);
                    let _ = writeln!(
                        out,
                        "<rect x=\"{:.1}\" y=\"{y:.1}\" width=\"{:.1}\" height=\"{bh:.1}\" rx=\"2\" fill=\"{col}\"/>",
                        zero_x.min(vx),
                        (vx - zero_x).abs().max(1.0)
                    );
                }
            }
        }
        for (i, lbl) in cats.iter().enumerate().take(n) {
            let y = py0 + (i as f32 + 0.5) * row_slot + 3.5;
            let _ = writeln!(
                out,
                "<text x=\"{:.1}\" y=\"{y:.1}\" text-anchor=\"end\" font-size=\"10\" fill=\"{}\">{}</text>",
                px0 - 6.0,
                hex(th.text_fg),
                esc(lbl)
            );
        }
        return;
    }

    draw_cartesian_grid_svg(out, chart, &sc1, px0, py0, pw, ph, sys);
    if chart.has_secondary_axis() {
        for &t in &sc2.ticks {
            let y = py0 + ph * (1.0 - sc2.norm(t));
            let _ = writeln!(
                out,
                "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"start\" font-size=\"10\" fill=\"{}\">{}</text>",
                px0 + pw + 6.0,
                y + 3.5,
                hex(th.muted_fg),
                esc(&format_chart_number(t, &chart.y2_axis.num_fmt, sys))
            );
        }
    }

    // 参考线与目标警示带
    let all_primary_vals: Vec<f64> = vis
        .iter()
        .filter(|(_, s)| !s.secondary)
        .flat_map(|(_, s)| s.vals.iter().copied())
        .collect();
    for rl in &chart.ref_lines {
        let sc = if rl.axis == RefLineAxis::YSecondary {
            &sc2
        } else {
            &sc1
        };
        if let Some(res) = resolve_reference_line(rl, &all_primary_vals) {
            let col = hex(rl.color);
            match res {
                ResolvedRefLine::Line { value, label } => {
                    let y = py0 + ph * (1.0 - sc.norm(value));
                    let _ = writeln!(
                        out,
                        "<line x1=\"{px0:.1}\" y1=\"{y:.1}\" x2=\"{:.1}\" y2=\"{y:.1}\" stroke=\"{col}\" stroke-width=\"1.4\"{}/>",
                        px0 + pw,
                        dash_attr(rl.dash)
                    );
                    let _ = writeln!(
                        out,
                        "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\" font-size=\"9.5\" font-weight=\"600\" fill=\"{col}\">{}</text>",
                        px0 + pw - 4.0,
                        y - 3.0,
                        esc(&label)
                    );
                }
                ResolvedRefLine::Band { low, high, label } => {
                    let y1 = py0 + ph * (1.0 - sc.norm(high));
                    let y2 = py0 + ph * (1.0 - sc.norm(low));
                    let _ = writeln!(
                        out,
                        "<rect x=\"{px0:.1}\" y=\"{:.1}\" width=\"{pw:.1}\" height=\"{:.1}\" fill=\"{col}\" fill-opacity=\"0.12\"/>",
                        y1.min(y2),
                        (y2 - y1).abs().max(1.0)
                    );
                    let _ = writeln!(
                        out,
                        "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\" font-size=\"9\" fill=\"{col}\">{}</text>",
                        px0 + pw - 4.0,
                        y1.min(y2) + 11.0,
                        esc(&label)
                    );
                }
            }
        }
    }

    let slot = pw / n as f32;
    let bar_series: Vec<usize> = vis
        .iter()
        .enumerate()
        .filter(|(_, (_, s))| {
            let k = s.kind.unwrap_or(chart.kind);
            matches!(
                k,
                ChartKind::Column | ChartKind::ColumnStacked | ChartKind::ColumnStacked100
            )
        })
        .map(|(idx, _)| idx)
        .collect();
    let totals: Vec<f64> = (0..n)
        .map(|i| {
            vis.iter()
                .filter(|(_, s)| !s.secondary)
                .filter_map(|(_, s)| s.vals.get(i).copied().filter(|v| v.is_finite()))
                .map(f64::abs)
                .sum::<f64>()
                .max(1e-12)
        })
        .collect();
    let mut pos_acc = vec![0.0f64; n];
    let mut neg_acc = vec![0.0f64; n];
    let zero_y = py0 + ph * (1.0 - sc1.zero_norm());

    for (vis_idx, &(si, s)) in vis.iter().enumerate() {
        let skind = s.kind.unwrap_or(chart.kind);
        let sc = if s.secondary { &sc2 } else { &sc1 };
        let col = hex(series_rgb(chart, si));
        let y_of = |v: f64| py0 + ph * (1.0 - sc.norm(v));
        if matches!(skind, ChartKind::Line | ChartKind::LineStacked | ChartKind::Area | ChartKind::AreaStacked | ChartKind::AreaStacked100) {
            let mut poly = String::new();
            let mut pts = Vec::new();
            for (i, &raw) in s.vals.iter().enumerate().take(n) {
                if !raw.is_finite() {
                    continue;
                }
                let v = if skind.is_percent_stacked() {
                    raw / totals[i]
                } else if skind.is_stacked() {
                    pos_acc[i] += raw;
                    pos_acc[i]
                } else {
                    raw
                };
                let x = px0 + (i as f32 + 0.5) * slot;
                let y = y_of(v);
                pts.push((x, y, raw));
                let _ = write!(poly, "{x:.1},{y:.1} ");
            }
            if matches!(skind, ChartKind::Area | ChartKind::AreaStacked | ChartKind::AreaStacked100)
                && pts.len() >= 2
            {
                let first_x = pts[0].0;
                let last_x = pts[pts.len() - 1].0;
                let _ = writeln!(
                    out,
                    "<polygon points=\"{first_x:.1},{zero_y:.1} {poly}{last_x:.1},{zero_y:.1}\" fill=\"{col}\" fill-opacity=\"0.22\"/>"
                );
            }
            if pts.len() >= 2 {
                let lw = if s.line_width > 0.0 { s.line_width } else { 2.2 };
                let _ = writeln!(
                    out,
                    "<polyline fill=\"none\" stroke=\"{col}\" stroke-width=\"{lw:.1}\"{} points=\"{poly}\"/>",
                    dash_attr(s.dash)
                );
            }
            if pts.len() <= 40 {
                for &(x, y, _) in &pts {
                    let _ = writeln!(
                        out,
                        "<circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"3\" fill=\"#FFFFFF\" stroke=\"{col}\" stroke-width=\"1.8\"/>"
                    );
                }
            }
        } else {
            let b_pos = bar_series.iter().position(|&k| k == vis_idx).unwrap_or(0);
            let b_cnt = bar_series.len().max(1);
            let rx = if chart.style.rounded_bars { 2.5 } else { 0.0 };
            for (i, &raw) in s.vals.iter().enumerate().take(n) {
                if !raw.is_finite() {
                    continue;
                }
                let v = if skind.is_percent_stacked() {
                    raw / totals[i]
                } else {
                    raw
                };
                if skind.is_stacked() {
                    let (v0, v1) = if v >= 0.0 {
                        let a = pos_acc[i];
                        pos_acc[i] += v;
                        (a, pos_acc[i])
                    } else {
                        let a = neg_acc[i];
                        neg_acc[i] += v;
                        (neg_acc[i], a)
                    };
                    let bw = (slot * 0.62).max(3.0);
                    let bx = px0 + i as f32 * slot + (slot - bw) * 0.5;
                    let y0 = y_of(v0);
                    let y1 = y_of(v1);
                    let _ = writeln!(
                        out,
                        "<rect x=\"{bx:.1}\" y=\"{:.1}\" width=\"{bw:.1}\" height=\"{:.1}\" fill=\"{col}\"/>",
                        y0.min(y1),
                        (y1 - y0).abs().max(1.0)
                    );
                } else {
                    let bw = (slot * 0.72 / b_cnt as f32).max(2.0);
                    let bx = px0 + i as f32 * slot + slot * 0.14 + bw * b_pos as f32;
                    let vy = y_of(v);
                    let _ = writeln!(
                        out,
                        "<rect x=\"{bx:.1}\" y=\"{:.1}\" width=\"{bw:.1}\" height=\"{:.1}\" rx=\"{rx}\" fill=\"{col}\"/>",
                        zero_y.min(vy),
                        (vy - zero_y).abs().max(1.0)
                    );
                }
            }
        }

        // 误差线
        if let Some(eb) = &s.error_bars {
            let deltas = compute_error_bar_deltas(&s.vals, eb);
            let ecol = hex(eb.color.unwrap_or(th.axis_line));
            for (i, (&v, &d)) in s.vals.iter().zip(deltas.iter()).enumerate().take(n) {
                if !v.is_finite() || d <= 0.0 {
                    continue;
                }
                let cx = px0 + (i as f32 + 0.5) * slot;
                let y_hi = y_of(v + d);
                let y_lo = y_of(v - d);
                let _ = writeln!(
                    out,
                    "<line x1=\"{cx:.1}\" y1=\"{y_lo:.1}\" x2=\"{cx:.1}\" y2=\"{y_hi:.1}\" stroke=\"{ecol}\" stroke-width=\"1.2\"/>"
                );
            }
        }

        // 趋势线 + 方程 & R²
        for tl in &s.trendlines {
            let raw: Vec<(f64, f64)> = s
                .vals
                .iter()
                .enumerate()
                .map(|(i, &v)| (i as f64, v))
                .collect();
            if let Some(comp) = compute_trendline(&raw, tl) {
                let tcol = hex(tl.color.unwrap_or(series_rgb(chart, si)));
                let mut tpts = String::new();
                for &(tx, ty) in &comp.points {
                    let sx = px0 + ((tx + 0.5) as f32 / n as f32) * pw;
                    let sy = y_of(ty);
                    let _ = write!(tpts, "{sx:.1},{sy:.1} ");
                }
                let _ = writeln!(
                    out,
                    "<polyline fill=\"none\" stroke=\"{tcol}\" stroke-width=\"1.6\"{} points=\"{tpts}\"/>",
                    dash_attr(tl.dash)
                );
                if (tl.show_equation || tl.show_r2) && !comp.points.is_empty() {
                    let mut eq = String::new();
                    if tl.show_equation {
                        eq.push_str(&comp.equation);
                    }
                    if tl.show_r2
                        && let Some(r2) = comp.r_squared
                    {
                        if !eq.is_empty() {
                            eq.push_str("  ");
                        }
                        let _ = write!(eq, "R² = {r2:.4}");
                    }
                    let last = comp.points[comp.points.len() - 1];
                    let lx = (px0 + ((last.0 + 0.5) as f32 / n as f32) * pw).clamp(px0 + 40.0, px0 + pw - 6.0);
                    let ly = (y_of(last.1) - 6.0).clamp(py0 + 12.0, py0 + ph - 6.0);
                    let _ = writeln!(
                        out,
                        "<text x=\"{lx:.1}\" y=\"{ly:.1}\" text-anchor=\"end\" font-size=\"9.5\" font-weight=\"600\" fill=\"{tcol}\">{}</text>",
                        esc(&eq)
                    );
                }
            }
        }
    }

    // X 轴分类标签
    let step_lbl = (n / 12).max(1);
    for (i, c) in cats.iter().enumerate().take(n) {
        if i % step_lbl != 0 {
            continue;
        }
        let x = px0 + (i as f32 + 0.5) * slot;
        let _ = writeln!(
            out,
            "<text x=\"{x:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"10\" fill=\"{}\">{}</text>",
            py0 + ph + 15.0,
            hex(th.text_fg),
            esc(c)
        );
    }
}

fn render_scatter_bubble_svg(
    out: &mut String,
    chart: &Chart,
    vis: &[(usize, &Series)],
    px0: f32,
    py0: f32,
    pw: f32,
    ph: f32,
    sys: DateSystem,
) {
    let mut xmin = f64::INFINITY;
    let mut xmax = f64::NEG_INFINITY;
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    let mut smax = 1.0f64;
    for (_, s) in vis {
        let has_x = s.xv.len() == s.vals.len() && !s.xv.is_empty();
        for (i, &y) in s.vals.iter().enumerate() {
            let x = if has_x { s.xv[i] } else { (i + 1) as f64 };
            if x.is_finite() && y.is_finite() {
                xmin = xmin.min(x);
                xmax = xmax.max(x);
                ymin = ymin.min(y);
                ymax = ymax.max(y);
            }
        }
        for &sz in &s.sizes {
            if sz.is_finite() && sz > 0.0 {
                smax = smax.max(sz);
            }
        }
    }
    if !xmin.is_finite() || !ymin.is_finite() {
        return;
    }
    let th = theme_spec(chart.style.theme);
    let xsc = compute_axis_scale(xmin, xmax, &chart.effective_x_axis(), false, 6);
    let ysc = compute_axis_scale(ymin, ymax, &chart.effective_y_axis(), false, 6);
    draw_cartesian_grid_svg(out, chart, &ysc, px0, py0, pw, ph, sys);
    for &tx in &xsc.ticks {
        let x = px0 + pw * xsc.norm(tx);
        let _ = writeln!(
            out,
            "<text x=\"{x:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"10\" fill=\"{}\">{}</text>",
            py0 + ph + 15.0,
            hex(th.muted_fg),
            esc(&format_chart_number(tx, &chart.effective_x_axis().num_fmt, sys))
        );
    }
    let is_bubble = chart.kind == ChartKind::Bubble;
    for &(si, s) in vis {
        let col = hex(series_rgb(chart, si));
        let has_x = s.xv.len() == s.vals.len() && !s.xv.is_empty();
        for (i, &y) in s.vals.iter().enumerate() {
            let x = if has_x { s.xv[i] } else { (i + 1) as f64 };
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            let cx = px0 + pw * xsc.norm(x);
            let cy = py0 + ph * (1.0 - ysc.norm(y));
            let r = if is_bubble {
                let sz = s.sizes.get(i).copied().unwrap_or(10.0).max(0.1);
                (5.0 + 22.0 * (sz / smax).sqrt() as f32).clamp(4.0, 28.0)
            } else {
                4.0
            };
            let op = if is_bubble { "0.55" } else { "0.92" };
            let _ = writeln!(
                out,
                "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"{r:.1}\" fill=\"{col}\" fill-opacity=\"{op}\" stroke=\"{col}\" stroke-width=\"1.2\"/>"
            );
        }
    }
}

fn render_pie_svg(
    out: &mut String,
    chart: &Chart,
    px0: f32,
    py0: f32,
    pw: f32,
    ph: f32,
    sys: DateSystem,
) {
    let Some(s) = chart.series.first() else {
        return;
    };
    let th = theme_spec(chart.style.theme);
    let total: f64 = s
        .vals
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0)
        .sum();
    if total <= 0.0 {
        return;
    }
    let cx = px0 + pw * 0.5;
    let cy = py0 + ph * 0.5;
    let radius = pw.min(ph) * 0.36;
    let inner = if chart.kind == ChartKind::Doughnut {
        radius * (chart.style.hole_size.clamp(10, 88) as f32 / 100.0)
    } else {
        0.0
    };
    let mut start = -std::f32::consts::FRAC_PI_2
        + (chart.style.first_slice_ang as f32).to_radians();
    let mut callout_inputs = Vec::new();
    for (i, &v) in s.vals.iter().enumerate() {
        if !v.is_finite() || v <= 0.0 {
            continue;
        }
        let sweep = (v / total) as f32 * std::f32::consts::TAU;
        let end = start + sweep.min(std::f32::consts::TAU - 0.001);
        let mid = start + sweep * 0.5;
        let exp_off = radius * (chart.style.pie_explosion as f32 / 100.0) * 0.35;
        let ox = cx + exp_off * mid.cos();
        let oy = cy + exp_off * mid.sin();
        let large = if sweep > std::f32::consts::PI { 1 } else { 0 };
        let col = hex(th.palette[i % th.palette.len()]);
        if inner <= 0.5 {
            let x1 = ox + radius * start.cos();
            let y1 = oy + radius * start.sin();
            let x2 = ox + radius * end.cos();
            let y2 = oy + radius * end.sin();
            let _ = writeln!(
                out,
                "<path d=\"M {ox:.1} {oy:.1} L {x1:.1} {y1:.1} A {radius:.1} {radius:.1} 0 {large} 1 {x2:.1} {y2:.1} Z\" fill=\"{col}\" stroke=\"{}\" stroke-width=\"1.2\"/>",
                hex(th.bg)
            );
        } else {
            let x1 = ox + radius * start.cos();
            let y1 = oy + radius * start.sin();
            let x2 = ox + radius * end.cos();
            let y2 = oy + radius * end.sin();
            let ix1 = ox + inner * end.cos();
            let iy1 = oy + inner * end.sin();
            let ix2 = ox + inner * start.cos();
            let iy2 = oy + inner * start.sin();
            let _ = writeln!(
                out,
                "<path d=\"M {x1:.1} {y1:.1} A {radius:.1} {radius:.1} 0 {large} 1 {x2:.1} {y2:.1} L {ix1:.1} {iy1:.1} A {inner:.1} {inner:.1} 0 {large} 0 {ix2:.1} {iy2:.1} Z\" fill=\"{col}\" stroke=\"{}\" stroke-width=\"1.2\"/>",
                hex(th.bg)
            );
        }
        if chart.show_labels || chart.label_opts.show_pct || chart.label_opts.show_cat {
            let cat = s.cats.get(i).map(String::as_str).unwrap_or("");
            let pct = format_percent(v / total);
            let lbl = if !cat.is_empty() {
                format!("{cat} {pct}")
            } else {
                pct
            };
            callout_inputs.push((i, mid, lbl));
        }
        start += sweep;
    }
    if !callout_inputs.is_empty() {
        let callouts = layout_pie_callouts(
            &callout_inputs,
            cx,
            cy,
            radius,
            py0 + 10.0,
            py0 + ph - 10.0,
            14.0,
        );
        for c in callouts {
            let _ = writeln!(
                out,
                "<polyline fill=\"none\" stroke=\"{}\" stroke-width=\"0.9\" points=\"{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\"/>",
                hex(th.axis_line),
                c.anchor_x,
                c.anchor_y,
                c.elbow_x,
                c.elbow_y,
                c.label_x,
                c.label_y
            );
            let align = if c.on_right { "start" } else { "end" };
            let tx = if c.on_right { c.label_x + 3.0 } else { c.label_x - 3.0 };
            let _ = writeln!(
                out,
                "<text x=\"{tx:.1}\" y=\"{:.1}\" text-anchor=\"{align}\" font-size=\"10\" fill=\"{}\">{}</text>",
                c.label_y + 3.5,
                hex(th.text_fg),
                esc(&c.text)
            );
        }
    }
    if chart.kind == ChartKind::Doughnut && chart.analytics.show_center_kpi {
        let kpi_val = format_chart_number(total, &chart.effective_y_axis().num_fmt, sys);
        let kpi_lbl = if chart.analytics.center_kpi_title.is_empty() {
            "总计"
        } else {
            chart.analytics.center_kpi_title.as_str()
        };
        let _ = writeln!(
            out,
            "<text x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"15\" font-weight=\"700\" fill=\"{}\">{}</text>",
            cy + 2.0,
            hex(th.title_fg),
            esc(&kpi_val)
        );
        let _ = writeln!(
            out,
            "<text x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"10\" fill=\"{}\">{}</text>",
            cy + 16.0,
            hex(th.muted_fg),
            esc(kpi_lbl)
        );
    }
}

fn render_radar_svg(
    out: &mut String,
    chart: &Chart,
    vis: &[(usize, &Series)],
    px0: f32,
    py0: f32,
    pw: f32,
    ph: f32,
) {
    let n = vis
        .iter()
        .map(|(_, s)| s.vals.len().max(s.cats.len()))
        .max()
        .unwrap_or(0);
    if n < 3 {
        return;
    }
    let th = theme_spec(chart.style.theme);
    let max_v = vis
        .iter()
        .flat_map(|(_, s)| s.vals.iter().copied())
        .filter(|v| v.is_finite())
        .fold(0.0f64, f64::max)
        .max(1e-9);
    let cx = px0 + pw * 0.5;
    let cy = py0 + ph * 0.5;
    let rad = pw.min(ph) * 0.36;
    for ring in 1..=4 {
        let r = rad * ring as f32 / 4.0;
        let mut pts = String::new();
        for k in 0..n {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * k as f32 / n as f32;
            let _ = write!(pts, "{:.1},{:.1} ", cx + r * a.cos(), cy + r * a.sin());
        }
        let _ = writeln!(
            out,
            "<polygon points=\"{pts}\" fill=\"none\" stroke=\"{}\" stroke-width=\"0.8\"/>",
            hex(th.grid)
        );
    }
    let cats: &[String] = vis.first().map(|(_, s)| s.cats.as_slice()).unwrap_or(&[]);
    for k in 0..n {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * k as f32 / n as f32;
        let _ = writeln!(
            out,
            "<line x1=\"{cx:.1}\" y1=\"{cy:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{}\" stroke-width=\"0.8\"/>",
            cx + rad * a.cos(),
            cy + rad * a.sin(),
            hex(th.grid)
        );
        if let Some(c) = cats.get(k) {
            let _ = writeln!(
                out,
                "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"10\" fill=\"{}\">{}</text>",
                cx + (rad + 14.0) * a.cos(),
                cy + (rad + 14.0) * a.sin() + 3.5,
                hex(th.text_fg),
                esc(c)
            );
        }
    }
    for &(si, s) in vis {
        let col = hex(series_rgb(chart, si));
        let mut pts = String::new();
        for k in 0..n {
            let v = s.vals.get(k).copied().unwrap_or(0.0);
            let frac = if v.is_finite() {
                (v / max_v).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * k as f32 / n as f32;
            let _ = write!(
                pts,
                "{:.1},{:.1} ",
                cx + rad * frac * a.cos(),
                cy + rad * frac * a.sin()
            );
        }
        let fill_op = if chart.radar_style == "filled" { "0.35" } else { "0.10" };
        let _ = writeln!(
            out,
            "<polygon points=\"{pts}\" fill=\"{col}\" fill-opacity=\"{fill_op}\" stroke=\"{col}\" stroke-width=\"2\"/>"
        );
    }
}
