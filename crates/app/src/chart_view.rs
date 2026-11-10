//! 图表绘制：把 Chart 画到给定的矩形里（柱状/条形/折线/面积/饼图/散点）。

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Stroke, Vec2};
use sheetcore::charts::{Chart, ChartKind};

pub const PALETTE: [Color32; 8] = [
    Color32::from_rgb(68, 114, 196),
    Color32::from_rgb(237, 125, 49),
    Color32::from_rgb(165, 165, 165),
    Color32::from_rgb(255, 192, 0),
    Color32::from_rgb(91, 155, 213),
    Color32::from_rgb(112, 173, 71),
    Color32::from_rgb(38, 68, 120),
    Color32::from_rgb(158, 72, 14),
];

/// 在 rect 里画一张图（含标题、图例、坐标轴）
pub fn paint_chart(painter: &egui::Painter, rect: Rect, chart: &Chart) {
    if rect.width() < 40.0 || rect.height() < 30.0 {
        return;
    }
    painter.rect_filled(rect, 2.0, Color32::WHITE);
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, Color32::from_rgb(200, 200, 200)),
        egui::StrokeKind::Inside,
    );

    let mut inner = rect.shrink(6.0);
    // 标题
    if !chart.title.is_empty() {
        painter.text(
            Pos2::new(inner.center().x, inner.min.y + 2.0),
            Align2::CENTER_TOP,
            &chart.title,
            FontId::proportional(13.0),
            Color32::from_rgb(40, 40, 40),
        );
        inner.min.y += 20.0;
    }
    // 图例
    let mut legend_h = 0.0;
    let series_count = chart.series.len().max(1);
    if chart.legend && series_count > 0 {
        legend_h = 18.0;
        let mut x = inner.min.x + 6.0;
        let y = inner.max.y - 12.0;
        for i in 0..series_count {
            let c = PALETTE[i % PALETTE.len()];
            painter.rect_filled(Rect::from_min_size(Pos2::new(x, y - 3.0), Vec2::new(10.0, 10.0)), 1.0, c);
            let name = chart
                .series
                .get(i)
                .map(|s| if s.name.is_empty() { format!("系列{}", i + 1) } else { s.name.clone() })
                .unwrap_or_default();
            painter.text(
                Pos2::new(x + 14.0, y + 2.0),
                Align2::LEFT_CENTER,
                &name,
                FontId::proportional(11.0),
                Color32::from_rgb(70, 70, 70),
            );
            x += 14.0 + 8.0 * name.chars().count() as f32 + 14.0;
        }
        inner.max.y -= legend_h;
    }

    let plot = Rect::from_min_max(
        Pos2::new(inner.min.x + 34.0, inner.min.y + 6.0),
        Pos2::new(inner.max.x - 6.0, inner.max.y - 18.0),
    );
    if plot.width() < 20.0 || plot.height() < 20.0 {
        return;
    }

    match chart.kind {
        ChartKind::Pie | ChartKind::Doughnut => paint_pie(painter, plot, chart),
        ChartKind::Combo => paint_combo(painter, plot, chart),
        ChartKind::Line => paint_line(painter, plot, chart),
        ChartKind::Area => paint_area(painter, plot, chart),
        _ => paint_bars(painter, plot, chart),
    }
}

fn axis(painter: &egui::Painter, plot: Rect, labels: &[String], max: f64, min: f64) {
    let line = Color32::from_rgb(180, 180, 180);
    let text = Color32::from_rgb(110, 110, 110);
    painter.line_segment(
        [Pos2::new(plot.min.x, plot.max.y), Pos2::new(plot.max.x, plot.max.y)],
        Stroke::new(1.0, line),
    );
    painter.line_segment(
        [Pos2::new(plot.min.x, plot.min.y), Pos2::new(plot.min.x, plot.max.y)],
        Stroke::new(1.0, line),
    );
    // 纵轴刻度
    let steps = 4;
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let y = plot.max.y - (plot.height() as f64 * t) as f32;
        let v = min + (max - min) * t;
        painter.line_segment(
            [Pos2::new(plot.min.x - 3.0, y), Pos2::new(plot.min.x, y)],
            Stroke::new(1.0, line),
        );
        painter.text(
            Pos2::new(plot.min.x - 5.0, y),
            Align2::RIGHT_CENTER,
            format_num(v),
            FontId::proportional(9.0),
            text,
        );
    }
    // 横轴标签
    if !labels.is_empty() {
        let n = labels.len();
        let step = (plot.width() / n as f32).max(1.0);
        for (i, l) in labels.iter().enumerate() {
            if n > 12 && i % (n / 12).max(1) != 0 {
                continue;
            }
            let x = plot.min.x + step * (i as f32 + 0.5);
            painter.text(
                Pos2::new(x, plot.max.y + 2.0),
                Align2::CENTER_TOP,
                l,
                FontId::proportional(9.0),
                text,
            );
        }
    }
}

fn format_num(v: f64) -> String {
    if v.abs() >= 10_000.0 {
        format!("{:.0}k", v / 1000.0)
    } else if v.fract().abs() < 0.01 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

fn bounds(chart: &Chart) -> (f64, f64) {
    let mut max = f64::NEG_INFINITY;
    let mut min = f64::INFINITY;
    for s in &chart.series {
        for v in &s.vals {
            max = max.max(*v);
            min = min.min(*v);
        }
    }
    if !max.is_finite() {
        return (0.0, 1.0);
    }
    let lo = min.min(0.0);
    let hi = max.max(lo + 1.0);
    (lo, hi)
}

/// 组合图：前 N-1 个系列柱状（主轴），最后一个系列折线（次轴）
fn paint_combo(painter: &egui::Painter, plot: Rect, chart: &Chart) {
    if chart.series.len() < 2 {
        paint_bars(painter, plot, chart);
        return;
    }
    let split = chart.series.len() - 1;
    let mut bars = chart.clone();
    bars.kind = ChartKind::Column;
    bars.series = chart.series[..split].to_vec();
    paint_bars(painter, plot, &bars);

    let line = &chart.series[split];
    let max = line
        .vals
        .iter()
        .cloned()
        .fold(0.0f64, f64::max)
        .max(1e-9);
    let n = line.vals.len().max(1);
    let slot = plot.width() / n as f32;
    let color = PALETTE[split % PALETTE.len()];
    let mut pts = Vec::new();
    for (i, v) in line.vals.iter().enumerate() {
        let t = (v / max).clamp(0.0, 1.0) as f32;
        pts.push(Pos2::new(
            plot.min.x + slot * (i as f32 + 0.5),
            plot.max.y - plot.height() * t,
        ));
    }
    for w in pts.windows(2) {
        painter.line_segment([w[0], w[1]], Stroke::new(2.0, color));
    }
    for p in pts.iter() {
        painter.circle_filled(*p, 2.5, color);
    }
    // 次轴（右侧）
    let line_col = Color32::from_rgb(180, 180, 180);
    let text = Color32::from_rgb(110, 110, 110);
    painter.line_segment(
        [
            Pos2::new(plot.max.x, plot.min.y),
            Pos2::new(plot.max.x, plot.max.y),
        ],
        Stroke::new(1.0, line_col),
    );
    for i in 0..=4 {
        let t = i as f64 / 4.0;
        let y = plot.max.y - (plot.height() as f64 * t) as f32;
        painter.text(
            Pos2::new(plot.max.x + 5.0, y),
            Align2::LEFT_CENTER,
            format_num(max * t),
            FontId::proportional(9.0),
            text,
        );
    }
}

fn paint_bars(painter: &egui::Painter, plot: Rect, chart: &Chart) {
    let (lo, hi) = bounds(chart);
    let cats: Vec<String> = chart
        .series
        .first()
        .map(|s| s.cats.clone())
        .unwrap_or_default();
    axis(painter, plot, &cats, hi, lo);

    let n = cats.len().max(chart.series.iter().map(|s| s.vals.len()).max().unwrap_or(0)).max(1);
    let groups = chart.series.len().max(1);
    let slot = plot.width() / n as f32;
    let bar_w = (slot * 0.8 / groups as f32).max(1.0);
    let horizontal = matches!(chart.kind, ChartKind::Bar | ChartKind::BarStacked);

    for (si, s) in chart.series.iter().enumerate() {
        let color = PALETTE[si % PALETTE.len()];
        for (i, v) in s.vals.iter().enumerate() {
            let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
            if horizontal {
                let h = plot.height() / n as f32 * 0.8 / groups as f32;
                let y = plot.min.y + plot.height() / n as f32 * i as f32 + (plot.height() / n as f32 - h) / 2.0;
                let w = plot.width() * t;
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(plot.min.x, y + h * si as f32), Vec2::new(w.max(1.0), h.max(1.0))),
                    0.0,
                    color,
                );
            } else {
                let x = plot.min.x + slot * i as f32 + slot * 0.1 + bar_w * si as f32;
                let h = plot.height() * t;
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(x, plot.max.y - h), Vec2::new(bar_w, h.max(1.0))),
                    0.0,
                    color,
                );
                if chart.show_labels && bar_w > 14.0 {
                    painter.text(
                        Pos2::new(x + bar_w / 2.0, plot.max.y - h - 2.0),
                        Align2::CENTER_BOTTOM,
                        format_num(*v),
                        FontId::proportional(9.0),
                        Color32::from_rgb(60, 60, 60),
                    );
                }
            }
        }
    }
}

fn paint_line(painter: &egui::Painter, plot: Rect, chart: &Chart) {
    let (lo, hi) = bounds(chart);
    let cats: Vec<String> = chart
        .series
        .first()
        .map(|s| s.cats.clone())
        .unwrap_or_default();
    axis(painter, plot, &cats, hi, lo);
    let n = cats.len().max(1);
    for (si, s) in chart.series.iter().enumerate() {
        let color = PALETTE[si % PALETTE.len()];
        let mut prev: Option<Pos2> = None;
        for (i, v) in s.vals.iter().enumerate() {
            let x = plot.min.x + plot.width() * (i as f32 + 0.5) / n.max(s.vals.len()) as f32 * (n as f32 / s.vals.len().max(1) as f32);
            let x = x.clamp(plot.min.x, plot.max.x);
            let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
            let y = plot.max.y - plot.height() * t;
            if let Some(p) = prev {
                painter.line_segment([p, Pos2::new(x, y)], Stroke::new(1.8, color));
            }
            painter.circle_filled(Pos2::new(x, y), 2.2, color);
            if chart.show_labels && s.vals.len() <= 12 {
                painter.text(
                    Pos2::new(x, y - 4.0),
                    Align2::CENTER_BOTTOM,
                    format_num(*v),
                    FontId::proportional(9.0),
                    Color32::from_rgb(60, 60, 60),
                );
            }
            prev = Some(Pos2::new(x, y));
        }
    }
}

fn paint_area(painter: &egui::Painter, plot: Rect, chart: &Chart) {
    let (lo, hi) = bounds(chart);
    let cats: Vec<String> = chart
        .series
        .first()
        .map(|s| s.cats.clone())
        .unwrap_or_default();
    axis(painter, plot, &cats, hi, lo);
    let n = chart.series.first().map(|s| s.vals.len()).unwrap_or(0).max(1);
    for (si, s) in chart.series.iter().enumerate() {
        let color = PALETTE[si % PALETTE.len()];
        let mut pts: Vec<Pos2> = Vec::new();
        for (i, v) in s.vals.iter().enumerate() {
            let x = plot.min.x + plot.width() * (i as f32 + 0.5) / n as f32;
            let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
            pts.push(Pos2::new(x, plot.max.y - plot.height() * t));
        }
        if pts.len() >= 2 {
            let mut poly = pts.clone();
            poly.push(Pos2::new(pts.last().unwrap().x, plot.max.y));
            poly.push(Pos2::new(pts[0].x, plot.max.y));
            painter.add(egui::Shape::convex_polygon(
                poly,
                Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 110),
                Stroke::NONE,
            ));
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], Stroke::new(1.6, color));
            }
        }
    }
}

fn paint_pie(painter: &egui::Painter, plot: Rect, chart: &Chart) {
    let vals: Vec<f64> = chart
        .series
        .first()
        .map(|s| s.vals.clone())
        .unwrap_or_default();
    let total: f64 = vals.iter().filter(|v| **v > 0.0).sum();
    if total <= 0.0 {
        return;
    }
    let radius = plot.width().min(plot.height()) * 0.42;
    let center = plot.center();
    let mut start = -std::f32::consts::FRAC_PI_2;
    let doughnut = chart.kind == ChartKind::Doughnut;
    for (i, v) in vals.iter().enumerate() {
        if *v <= 0.0 {
            continue;
        }
        let sweep = (*v / total) as f32 * std::f32::consts::TAU;
        let color = PALETTE[i % PALETTE.len()];
        let segments = ((sweep / 0.12).ceil() as usize).max(3);
        let mut pts = Vec::new();
        let inner = if doughnut { radius * 0.55 } else { 0.0 };
        for k in 0..=segments {
            let a = start + sweep * k as f32 / segments as f32;
            pts.push(Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin()));
        }
        for k in (0..=segments).rev() {
            let a = start + sweep * k as f32 / segments as f32;
            let r = if inner > 0.0 { inner } else { 0.0 };
            pts.push(Pos2::new(center.x + r * a.cos(), center.y + r * a.sin()));
        }
        painter.add(egui::Shape::convex_polygon(pts, color, Stroke::new(1.0, Color32::WHITE)));
        start += sweep;
    }
}
