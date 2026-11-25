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

struct ChartCanvas<'a> {
    painter: &'a egui::Painter,
    shapes: Vec<egui::Shape>,
}

impl<'a> ChartCanvas<'a> {
    #[inline]
    fn rect_filled(&mut self, rect: Rect, cr: impl Into<egui::CornerRadius>, fill: Color32) {
        self.shapes.push(egui::Shape::rect_filled(rect, cr, fill));
    }

    #[inline]
    fn rect_stroke(
        &mut self,
        rect: Rect,
        cr: impl Into<egui::CornerRadius>,
        stroke: impl Into<Stroke>,
        kind: egui::StrokeKind,
    ) {
        self.shapes
            .push(egui::Shape::rect_stroke(rect, cr, stroke, kind));
    }

    #[inline]
    fn line_segment(&mut self, pts: [Pos2; 2], stroke: impl Into<Stroke>) {
        self.shapes.push(egui::Shape::line_segment(pts, stroke));
    }

    #[inline]
    fn circle_filled(&mut self, center: Pos2, radius: f32, fill: Color32) {
        self.shapes
            .push(egui::Shape::circle_filled(center, radius, fill));
    }

    #[inline]
    fn text(
        &mut self,
        pos: Pos2,
        anchor: Align2,
        text: impl ToString,
        font_id: FontId,
        color: Color32,
    ) {
        let galley = self.painter.layout_no_wrap(text.to_string(), font_id, color);
        let rect = anchor.anchor_size(pos, galley.size());
        self.shapes
            .push(egui::Shape::galley(rect.min, galley, color));
    }

    #[inline]
    fn add(&mut self, shape: egui::Shape) {
        self.shapes.push(shape);
    }
}

/// 构建图表的整套绘制图元（供 `Rules.charts` 按版本与矩形直接缓存重放，避免悬停帧重算 10 万点抽稀与排版）。
pub fn build_chart_shapes(painter: &egui::Painter, rect: Rect, chart: &Chart) -> Vec<egui::Shape> {
    if rect.width() < 40.0 || rect.height() < 30.0 {
        return Vec::new();
    }
    let mut cv = ChartCanvas {
        painter,
        shapes: Vec::with_capacity(32),
    };
    paint_chart_into(&mut cv, rect, chart);
    cv.shapes
}

/// 在 rect 里画一张图（含标题、图例、坐标轴）
pub fn paint_chart(painter: &egui::Painter, rect: Rect, chart: &Chart) {
    let shapes = build_chart_shapes(painter, rect, chart);
    if !shapes.is_empty() {
        painter.extend(shapes);
    }
}

#[inline]
fn series_color(s: Option<&sheetcore::charts::Series>, idx: usize) -> Color32 {
    s.and_then(|x| x.color)
        .map(|[r, g, b]| Color32::from_rgb(r, g, b))
        .unwrap_or(PALETTE[idx % PALETTE.len()])
}

fn paint_chart_into(painter: &mut ChartCanvas<'_>, rect: Rect, chart: &Chart) {
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
    let series_count = chart.series.len().max(1);
    if chart.legend && series_count > 0 {
        let legend_h = 18.0;
        let mut x = inner.min.x + 6.0;
        let y = inner.max.y - 12.0;
        for i in 0..series_count {
            let c = series_color(chart.series.get(i), i);
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(x, y - 3.0), Vec2::new(10.0, 10.0)),
                1.0,
                c,
            );
            let fallback;
            let name: &str = match chart.series.get(i) {
                Some(s) if !s.name.is_empty() => s.name.as_str(),
                _ => {
                    fallback = format!("系列{}", i + 1);
                    &fallback
                }
            };
            painter.text(
                Pos2::new(x + 14.0, y + 2.0),
                Align2::LEFT_CENTER,
                name,
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
        ChartKind::Scatter => paint_scatter(painter, plot, chart),
        ChartKind::Radar => paint_radar(painter, plot, chart),
        _ => paint_bars(painter, plot, chart),
    }
}

fn axis_horizontal(painter: &mut ChartCanvas<'_>, plot: Rect, labels: &[String], max: f64, min: f64) {
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
    // 横轴数值刻度
    let steps = 4;
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let x = plot.min.x + (plot.width() as f64 * t) as f32;
        let v = min + (max - min) * t;
        painter.line_segment(
            [Pos2::new(x, plot.max.y), Pos2::new(x, plot.max.y + 3.0)],
            Stroke::new(1.0, line),
        );
        painter.text(
            Pos2::new(x, plot.max.y + 4.0),
            Align2::CENTER_TOP,
            format_num(v),
            FontId::proportional(9.0),
            text,
        );
    }
    // 纵轴分类标签
    if !labels.is_empty() {
        let n = labels.len();
        let step = (plot.height() / n as f32).max(1.0);
        for (i, l) in labels.iter().enumerate() {
            if n > 12 && i % (n / 12).max(1) != 0 {
                continue;
            }
            let y = plot.min.y + step * (i as f32 + 0.5);
            painter.text(
                Pos2::new(plot.min.x - 4.0, y),
                Align2::RIGHT_CENTER,
                l,
                FontId::proportional(9.0),
                text,
            );
        }
    }
}

fn axis(painter: &mut ChartCanvas<'_>, plot: Rect, labels: &[String], max: f64, min: f64) {
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

fn bounds_slice(series: &[sheetcore::charts::Series]) -> (f64, f64) {
    let mut max = f64::NEG_INFINITY;
    let mut min = f64::INFINITY;
    for s in series {
        for v in &s.vals {
            if v.is_finite() {
                max = max.max(*v);
                min = min.min(*v);
            }
        }
    }
    if !max.is_finite() {
        return (0.0, 1.0);
    }
    let lo = min.min(0.0);
    let hi = max.max(lo + 1.0);
    (lo, hi)
}

/// 像素桶包络降采样（first / min / max / last）：当数据点数超过绘图区像素密度时，
/// 将十万/百万点序列在 O(N) 内压缩为 <= 4*buckets 个包络特征点，视觉零失真且避免十万图元三角化。
pub(crate) fn decimate_line_points(
    vals: &[f64],
    total_n: usize,
    lo: f64,
    hi: f64,
    plot: Rect,
) -> Vec<Pos2> {
    let len = vals.len();
    if len == 0 {
        return Vec::new();
    }
    let denom = total_n.max(len).max(1) as f32;
    let scale_x = total_n.max(1) as f32 / len as f32;
    let span = (hi - lo).max(1e-12);
    let x_of = |i: usize| -> f32 {
        (plot.min.x + plot.width() * (i as f32 + 0.5) / denom * scale_x)
            .clamp(plot.min.x, plot.max.x)
    };
    let y_of = |v: f64| -> f32 {
        let t = ((v - lo) / span).clamp(0.0, 1.0) as f32;
        plot.max.y - plot.height() * t
    };
    let max_buckets = (plot.width().ceil() as usize).clamp(64, 2048);
    if len <= max_buckets * 2 {
        let mut pts = Vec::with_capacity(len);
        for (i, &v) in vals.iter().enumerate() {
            if v.is_finite() {
                pts.push(Pos2::new(x_of(i), y_of(v)));
            }
        }
        return pts;
    }
    let mut pts = Vec::with_capacity(max_buckets * 4);
    for b in 0..max_buckets {
        let start = b * len / max_buckets;
        let end = ((b + 1) * len / max_buckets).min(len);
        if start >= end {
            continue;
        }
        let mut first_idx = None;
        let mut last_idx = start;
        let mut min_idx = start;
        let mut max_idx = start;
        let mut min_v = f64::INFINITY;
        let mut max_v = f64::NEG_INFINITY;
        for i in start..end {
            let v = vals[i];
            if !v.is_finite() {
                continue;
            }
            if first_idx.is_none() {
                first_idx = Some(i);
            }
            last_idx = i;
            if v < min_v {
                min_v = v;
                min_idx = i;
            }
            if v > max_v {
                max_v = v;
                max_idx = i;
            }
        }
        let Some(f_idx) = first_idx else { continue };
        pts.push(Pos2::new(x_of(f_idx), y_of(vals[f_idx])));
        let (a, z) = if min_idx <= max_idx {
            (min_idx, max_idx)
        } else {
            (max_idx, min_idx)
        };
        if a != f_idx && a != last_idx {
            pts.push(Pos2::new(x_of(a), y_of(vals[a])));
        }
        if z != a && z != f_idx && z != last_idx {
            pts.push(Pos2::new(x_of(z), y_of(vals[z])));
        }
        if last_idx != f_idx {
            pts.push(Pos2::new(x_of(last_idx), y_of(vals[last_idx])));
        }
    }
    pts
}

/// 组合图：前 N-1 个系列柱状（主轴），最后一个系列折线（次轴）
fn paint_combo(painter: &mut ChartCanvas<'_>, plot: Rect, chart: &Chart) {
    if chart.series.len() < 2 {
        paint_bars_slice(painter, plot, &chart.series, chart.kind, chart.show_labels);
        return;
    }
    let split = chart.series.len() - 1;
    paint_bars_slice(
        painter,
        plot,
        &chart.series[..split],
        ChartKind::Column,
        chart.show_labels,
    );

    let line = &chart.series[split];
    let max = line
        .vals
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0f64, f64::max)
        .max(1e-9);
    let n = line.vals.len().max(1);
    let color = series_color(Some(line), split);
    let pts = decimate_line_points(&line.vals, n, 0.0, max, plot);
    for w in pts.windows(2) {
        painter.line_segment([w[0], w[1]], Stroke::new(2.0, color));
    }
    if plot.width() / (n as f32) >= 3.5 {
        for p in &pts {
            painter.circle_filled(*p, 2.5, color);
        }
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

fn paint_bars(painter: &mut ChartCanvas<'_>, plot: Rect, chart: &Chart) {
    paint_bars_slice(painter, plot, &chart.series, chart.kind, chart.show_labels);
}

fn bounds_stacked(series: &[sheetcore::charts::Series]) -> (f64, f64) {
    let n = series.iter().map(|s| s.vals.len()).max().unwrap_or(0);
    if n == 0 {
        return (0.0, 1.0);
    }
    let mut lo = 0.0f64;
    let mut hi = 0.0f64;
    for i in 0..n {
        let mut pos = 0.0f64;
        let mut neg = 0.0f64;
        for s in series {
            if let Some(&v) = s.vals.get(i) && v.is_finite() {
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
    if (hi - lo).abs() < 1e-12 {
        (lo, lo + 1.0)
    } else {
        (lo, hi)
    }
}

fn paint_bars_slice(
    painter: &mut ChartCanvas<'_>,
    plot: Rect,
    series: &[sheetcore::charts::Series],
    kind: ChartKind,
    show_labels: bool,
) {
    let stacked = kind == ChartKind::BarStacked;
    let horizontal = matches!(kind, ChartKind::Bar | ChartKind::BarStacked);
    let (lo, hi) = if stacked {
        bounds_stacked(series)
    } else {
        bounds_slice(series)
    };
    let span = (hi - lo).max(1e-12);
    let cats: &[String] = series.first().map(|s| s.cats.as_slice()).unwrap_or(&[]);
    if horizontal {
        axis_horizontal(painter, plot, cats, hi, lo);
    } else {
        axis(painter, plot, cats, hi, lo);
    }
    let zero_t = ((0.0 - lo) / span).clamp(0.0, 1.0) as f32;
    let zero_x = plot.min.x + plot.width() * zero_t;
    let zero_y = plot.max.y - plot.height() * zero_t;
    if lo < 0.0 && hi > 0.0 {
        let zstroke = Stroke::new(1.0, Color32::from_rgb(130, 130, 130));
        if horizontal {
            painter.line_segment([Pos2::new(zero_x, plot.min.y), Pos2::new(zero_x, plot.max.y)], zstroke);
        } else {
            painter.line_segment([Pos2::new(plot.min.x, zero_y), Pos2::new(plot.max.x, zero_y)], zstroke);
        }
    }

    let n = cats
        .len()
        .max(series.iter().map(|s| s.vals.len()).max().unwrap_or(0))
        .max(1);
    let groups = series.len().max(1);
    let axis_px = if horizontal { plot.height() } else { plot.width() };
    let max_buckets = (axis_px.ceil() as usize).clamp(64, 2048);

    if n > max_buckets * 2 {
        let mut mesh = egui::Mesh::default();
        for (si, s) in series.iter().enumerate() {
            let color = series_color(Some(s), si);
            let len = s.vals.len();
            if len == 0 {
                continue;
            }
            for b in 0..max_buckets {
                let start = b * len / max_buckets;
                let end = ((b + 1) * len / max_buckets).min(len);
                if start >= end {
                    continue;
                }
                let mut peak = 0.0f64;
                let mut found = false;
                for &v in &s.vals[start..end] {
                    if v.is_finite() && (!found || v.abs() > peak.abs()) {
                        peak = v;
                        found = true;
                    }
                }
                if !found {
                    continue;
                }
                let t = ((peak - lo) / span).clamp(0.0, 1.0) as f32;
                if horizontal {
                    let slot = plot.height() / max_buckets as f32;
                    let h = (slot / groups as f32).max(1.0);
                    let y = plot.min.y + slot * b as f32 + h * si as f32;
                    let vx = plot.min.x + plot.width() * t;
                    let x0 = zero_x.min(vx);
                    let w = (vx - zero_x).abs().max(1.0);
                    mesh.add_colored_rect(
                        Rect::from_min_size(Pos2::new(x0, y), Vec2::new(w, h)),
                        color,
                    );
                } else {
                    let slot = plot.width() / max_buckets as f32;
                    let bar_w = (slot / groups as f32).max(1.0);
                    let x = plot.min.x + slot * b as f32 + bar_w * si as f32;
                    let vy = plot.max.y - plot.height() * t;
                    let y0 = zero_y.min(vy);
                    let h = (vy - zero_y).abs().max(1.0);
                    mesh.add_colored_rect(
                        Rect::from_min_size(Pos2::new(x, y0), Vec2::new(bar_w, h)),
                        color,
                    );
                }
            }
        }
        if !mesh.is_empty() {
            painter.add(egui::Shape::mesh(mesh));
        }
        return;
    }

    let mut mesh = egui::Mesh::default();
    if stacked {
        let mut pos_acc = vec![0.0f64; n];
        let mut neg_acc = vec![0.0f64; n];
        let row_slot = plot.height() / n as f32;
        let bar_h = (row_slot * 0.72).max(1.5);
        for (si, s) in series.iter().enumerate() {
            let color = series_color(Some(s), si);
            for (i, &v) in s.vals.iter().enumerate() {
                if !v.is_finite() || i >= n {
                    continue;
                }
                let (v0, v1) = if v >= 0.0 {
                    let start = pos_acc[i];
                    pos_acc[i] += v;
                    (start, pos_acc[i])
                } else {
                    let start = neg_acc[i];
                    neg_acc[i] += v;
                    (neg_acc[i], start)
                };
                let t0 = ((v0 - lo) / span).clamp(0.0, 1.0) as f32;
                let t1 = ((v1 - lo) / span).clamp(0.0, 1.0) as f32;
                let x0 = plot.min.x + plot.width() * t0;
                let x1 = plot.min.x + plot.width() * t1;
                let y = plot.min.y + row_slot * i as f32 + (row_slot - bar_h) * 0.5;
                mesh.add_colored_rect(
                    Rect::from_min_size(Pos2::new(x0, y), Vec2::new((x1 - x0).max(1.0), bar_h)),
                    color,
                );
            }
        }
        if !mesh.is_empty() {
            painter.add(egui::Shape::mesh(mesh));
        }
        return;
    }

    let slot = plot.width() / n as f32;
    let bar_w = (slot * 0.8 / groups as f32).max(1.0);
    let row_slot = plot.height() / n as f32;
    let hbar_h = (row_slot * 0.8 / groups as f32).max(1.0);
    for (si, s) in series.iter().enumerate() {
        let color = series_color(Some(s), si);
        for (i, &v) in s.vals.iter().enumerate() {
            if !v.is_finite() {
                continue;
            }
            let t = ((v - lo) / span).clamp(0.0, 1.0) as f32;
            if horizontal {
                let y = plot.min.y + row_slot * i as f32 + row_slot * 0.1 + hbar_h * si as f32;
                let vx = plot.min.x + plot.width() * t;
                let x0 = zero_x.min(vx);
                let w = (vx - zero_x).abs().max(1.0);
                mesh.add_colored_rect(
                    Rect::from_min_size(Pos2::new(x0, y), Vec2::new(w, hbar_h)),
                    color,
                );
                if show_labels && hbar_h > 10.0 {
                    let (lx, align) = if v >= 0.0 {
                        (x0 + w + 2.0, Align2::LEFT_CENTER)
                    } else {
                        (x0 - 2.0, Align2::RIGHT_CENTER)
                    };
                    painter.text(
                        Pos2::new(lx, y + hbar_h * 0.5),
                        align,
                        format_num(v),
                        FontId::proportional(9.0),
                        Color32::from_rgb(60, 60, 60),
                    );
                }
            } else {
                let x = plot.min.x + slot * i as f32 + slot * 0.1 + bar_w * si as f32;
                let vy = plot.max.y - plot.height() * t;
                let y0 = zero_y.min(vy);
                let h = (vy - zero_y).abs().max(1.0);
                mesh.add_colored_rect(
                    Rect::from_min_size(Pos2::new(x, y0), Vec2::new(bar_w, h)),
                    color,
                );
                if show_labels && bar_w > 14.0 {
                    let (ly, align) = if v >= 0.0 {
                        (y0 - 2.0, Align2::CENTER_BOTTOM)
                    } else {
                        (y0 + h + 2.0, Align2::CENTER_TOP)
                    };
                    painter.text(
                        Pos2::new(x + bar_w / 2.0, ly),
                        align,
                        format_num(v),
                        FontId::proportional(9.0),
                        Color32::from_rgb(60, 60, 60),
                    );
                }
            }
        }
    }
    if !mesh.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
}

fn paint_scatter(painter: &mut ChartCanvas<'_>, plot: Rect, chart: &Chart) {
    let mut xmin = f64::INFINITY;
    let mut xmax = f64::NEG_INFINITY;
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    for s in &chart.series {
        let has_x = s.xv.len() == s.vals.len() && !s.xv.is_empty();
        for (i, &y) in s.vals.iter().enumerate() {
            if !y.is_finite() {
                continue;
            }
            let x = if has_x { s.xv[i] } else { (i + 1) as f64 };
            if !x.is_finite() {
                continue;
            }
            xmin = xmin.min(x);
            xmax = xmax.max(x);
            ymin = ymin.min(y);
            ymax = ymax.max(y);
        }
    }
    if !xmin.is_finite() || !ymin.is_finite() {
        return;
    }
    let xlo = xmin.min(0.0);
    let xhi = if (xmax - xlo).abs() < 1e-12 { xlo + 1.0 } else { xmax };
    let ylo = ymin.min(0.0);
    let yhi = if (ymax - ylo).abs() < 1e-12 { ylo + 1.0 } else { ymax };
    let xlabels: Vec<String> = (0..=4)
        .map(|i| format_num(xlo + (xhi - xlo) * (i as f64 / 4.0)))
        .collect();
    axis(painter, plot, &xlabels, yhi, ylo);
    let xspan = (xhi - xlo).max(1e-12);
    let yspan = (yhi - ylo).max(1e-12);
    for (si, s) in chart.series.iter().enumerate() {
        let color = series_color(Some(s), si);
        let has_x = s.xv.len() == s.vals.len() && !s.xv.is_empty();
        for (i, &y) in s.vals.iter().enumerate() {
            if !y.is_finite() {
                continue;
            }
            let x = if has_x { s.xv[i] } else { (i + 1) as f64 };
            if !x.is_finite() {
                continue;
            }
            let px = plot.min.x + plot.width() * (((x - xlo) / xspan).clamp(0.0, 1.0) as f32);
            let py = plot.max.y - plot.height() * (((y - ylo) / yspan).clamp(0.0, 1.0) as f32);
            painter.circle_filled(Pos2::new(px, py), 3.0, color);
        }
    }
}

fn paint_radar(painter: &mut ChartCanvas<'_>, plot: Rect, chart: &Chart) {
    let n = chart
        .series
        .iter()
        .map(|s| s.vals.len().max(s.cats.len()))
        .max()
        .unwrap_or(0);
    if n < 3 {
        return;
    }
    let max_v = chart
        .series
        .iter()
        .flat_map(|s| s.vals.iter().copied())
        .filter(|v| v.is_finite())
        .fold(0.0f64, f64::max)
        .max(1e-9);
    let center = plot.center();
    let radius = plot.width().min(plot.height()) * 0.38;
    let web = Stroke::new(0.8, Color32::from_rgb(205, 205, 205));
    // 同心网格与射线
    for ring in 1..=4 {
        let r = radius * ring as f32 / 4.0;
        let mut pts = Vec::with_capacity(n + 1);
        for k in 0..=n {
            let a = -std::f32::consts::FRAC_PI_2
                + std::f32::consts::TAU * (k % n) as f32 / n as f32;
            pts.push(Pos2::new(center.x + r * a.cos(), center.y + r * a.sin()));
        }
        painter.add(egui::Shape::line(pts, web));
    }
    let cats: &[String] = chart.series.first().map(|s| s.cats.as_slice()).unwrap_or(&[]);
    for k in 0..n {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * k as f32 / n as f32;
        let tip = Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin());
        painter.line_segment([center, tip], web);
        if let Some(lbl) = cats.get(k) && !lbl.is_empty() {
            let lp = Pos2::new(
                center.x + (radius + 10.0) * a.cos(),
                center.y + (radius + 10.0) * a.sin(),
            );
            painter.text(
                lp,
                Align2::CENTER_CENTER,
                lbl,
                FontId::proportional(9.0),
                Color32::from_rgb(90, 90, 90),
            );
        }
    }
    for (si, s) in chart.series.iter().enumerate() {
        let color = series_color(Some(s), si);
        let mut pts = Vec::with_capacity(n);
        for k in 0..n {
            let v = s.vals.get(k).copied().unwrap_or(0.0);
            let frac = if v.is_finite() {
                (v / max_v).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * k as f32 / n as f32;
            pts.push(Pos2::new(
                center.x + radius * frac * a.cos(),
                center.y + radius * frac * a.sin(),
            ));
        }
        if chart.radar_style == "filled" && pts.len() >= 3 {
            let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 80);
            let mut mesh = egui::Mesh::default();
            let uv = egui::epaint::WHITE_UV;
            mesh.vertices.push(egui::epaint::Vertex { pos: center, uv, color: fill });
            for &p in &pts {
                mesh.vertices.push(egui::epaint::Vertex { pos: p, uv, color: fill });
            }
            for k in 0..pts.len() {
                let a = (k + 1) as u32;
                let b = ((k + 1) % pts.len() + 1) as u32;
                mesh.indices.extend_from_slice(&[0, a, b]);
            }
            painter.add(egui::Shape::mesh(mesh));
        }
        if pts.len() >= 2 {
            let mut closed = pts.clone();
            closed.push(pts[0]);
            painter.add(egui::Shape::line(closed, Stroke::new(1.8, color)));
        }
        if chart.radar_style != "filled" {
            for &p in &pts {
                painter.circle_filled(p, 2.5, color);
            }
        }
    }
}

fn paint_line(painter: &mut ChartCanvas<'_>, plot: Rect, chart: &Chart) {
    let (lo, hi) = bounds_slice(&chart.series);
    let cats: &[String] = chart
        .series
        .first()
        .map(|s| s.cats.as_slice())
        .unwrap_or(&[]);
    axis(painter, plot, cats, hi, lo);
    let n = cats.len().max(1);
    for (si, s) in chart.series.iter().enumerate() {
        let color = series_color(Some(s), si);
        let pts = decimate_line_points(&s.vals, n, lo, hi, plot);
        let draw_dots = plot.width() / (s.vals.len().max(1) as f32) >= 3.5;
        let draw_labels = chart.show_labels && s.vals.len() <= 12;
        if !draw_dots && !draw_labels {
            if pts.len() >= 2 {
                painter.add(egui::Shape::line(pts, Stroke::new(1.8, color)));
            }
            continue;
        }
        if pts.len() >= 2 {
            painter.add(egui::Shape::line(pts.clone(), Stroke::new(1.8, color)));
        }
        if draw_dots {
            for p in &pts {
                painter.circle_filled(*p, 2.2, color);
            }
        }
        if draw_labels {
            for (p, &v) in pts.iter().zip(s.vals.iter()) {
                painter.text(
                    Pos2::new(p.x, p.y - 4.0),
                    Align2::CENTER_BOTTOM,
                    format_num(v),
                    FontId::proportional(9.0),
                    Color32::from_rgb(60, 60, 60),
                );
            }
        }
    }
}

fn paint_area(painter: &mut ChartCanvas<'_>, plot: Rect, chart: &Chart) {
    let (lo, hi) = bounds_slice(&chart.series);
    let cats: &[String] = chart
        .series
        .first()
        .map(|s| s.cats.as_slice())
        .unwrap_or(&[]);
    axis(painter, plot, cats, hi, lo);
    let n = chart.series.first().map(|s| s.vals.len()).unwrap_or(0).max(1);
    let zero_y = plot.max.y - plot.height() * (((0.0 - lo) / (hi - lo).max(1e-12)).clamp(0.0, 1.0) as f32);
    for (si, s) in chart.series.iter().enumerate() {
        let color = series_color(Some(s), si);
        let pts = decimate_line_points(&s.vals, n, lo, hi, plot);
        if pts.len() >= 2 {
            let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 110);
            let mut mesh = egui::Mesh::default();
            for w in pts.windows(2) {
                let base = mesh.vertices.len() as u32;
                let uv = egui::epaint::WHITE_UV;
                mesh.vertices.extend_from_slice(&[
                    egui::epaint::Vertex { pos: w[0], uv, color: fill },
                    egui::epaint::Vertex { pos: w[1], uv, color: fill },
                    egui::epaint::Vertex { pos: Pos2::new(w[1].x, zero_y), uv, color: fill },
                    egui::epaint::Vertex { pos: Pos2::new(w[0].x, zero_y), uv, color: fill },
                ]);
                mesh.indices.extend_from_slice(&[
                    base,
                    base + 1,
                    base + 2,
                    base,
                    base + 2,
                    base + 3,
                ]);
            }
            painter.add(egui::Shape::mesh(mesh));
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], Stroke::new(1.6, color));
            }
        }
    }
}

fn add_annular_slice(
    mesh: &mut egui::Mesh,
    center: Pos2,
    inner_r: f32,
    outer_r: f32,
    start: f32,
    sweep: f32,
    color: Color32,
) {
    let segments = ((sweep / 0.1).ceil() as usize).max(3);
    let uv = egui::epaint::WHITE_UV;
    if inner_r <= 0.0 {
        let c_idx = mesh.vertices.len() as u32;
        mesh.vertices.push(egui::epaint::Vertex { pos: center, uv, color });
        let first = mesh.vertices.len() as u32;
        for k in 0..=segments {
            let a = start + sweep * k as f32 / segments as f32;
            let pos = Pos2::new(center.x + outer_r * a.cos(), center.y + outer_r * a.sin());
            mesh.vertices.push(egui::epaint::Vertex { pos, uv, color });
        }
        for k in 0..segments as u32 {
            mesh.indices.extend_from_slice(&[c_idx, first + k, first + k + 1]);
        }
    } else {
        let base = mesh.vertices.len() as u32;
        for k in 0..=segments {
            let a = start + sweep * k as f32 / segments as f32;
            let (ca, sa) = (a.cos(), a.sin());
            mesh.vertices.push(egui::epaint::Vertex {
                pos: Pos2::new(center.x + outer_r * ca, center.y + outer_r * sa),
                uv,
                color,
            });
            mesh.vertices.push(egui::epaint::Vertex {
                pos: Pos2::new(center.x + inner_r * ca, center.y + inner_r * sa),
                uv,
                color,
            });
        }
        for k in 0..segments as u32 {
            let o0 = base + k * 2;
            let i0 = o0 + 1;
            let o1 = o0 + 2;
            let i1 = o0 + 3;
            mesh.indices.extend_from_slice(&[o0, o1, i1, o0, i1, i0]);
        }
    }
}

fn paint_pie(painter: &mut ChartCanvas<'_>, plot: Rect, chart: &Chart) {
    let vals: &[f64] = chart
        .series
        .first()
        .map(|s| s.vals.as_slice())
        .unwrap_or(&[]);
    let total: f64 = vals.iter().copied().filter(|v| v.is_finite() && *v > 0.0).sum();
    if total <= 0.0 {
        return;
    }
    let radius = plot.width().min(plot.height()) * 0.42;
    let center = plot.center();
    let mut start = -std::f32::consts::FRAC_PI_2;
    let inner = if chart.kind == ChartKind::Doughnut {
        radius * 0.55
    } else {
        0.0
    };
    let mut mesh = egui::Mesh::default();
    let mut dividers: Vec<f32> = Vec::new();
    let mut remainder_sweep = 0.0f32;
    let mut positive_count = 0usize;
    for (i, v) in vals.iter().enumerate() {
        if !v.is_finite() || *v <= 0.0 {
            continue;
        }
        positive_count += 1;
        let sweep = (*v / total) as f32 * std::f32::consts::TAU;
        if sweep < 0.004 && vals.len() > 360 {
            remainder_sweep += sweep;
            continue;
        }
        let color = PALETTE[i % PALETTE.len()];
        add_annular_slice(&mut mesh, center, inner, radius, start, sweep, color);
        dividers.push(start);
        start += sweep;
    }
    if remainder_sweep > 0.001 {
        add_annular_slice(
            &mut mesh,
            center,
            inner,
            radius,
            start,
            remainder_sweep,
            Color32::from_rgb(180, 180, 180),
        );
        dividers.push(start);
    }
    if !mesh.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
    if positive_count > 1 {
        let sep = Stroke::new(1.0, Color32::WHITE);
        for a in dividers {
            let (ca, sa) = (a.cos(), a.sin());
            let p0 = Pos2::new(center.x + inner * ca, center.y + inner * sa);
            let p1 = Pos2::new(center.x + radius * ca, center.y + radius * sa);
            painter.line_segment([p0, p1], sep);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimation_bounds_hundred_thousand_point_series_while_preserving_extrema() {
        let mut vals = vec![10.0; 100_000];
        vals[12_345] = 999.0;
        vals[87_654] = -50.0;
        let plot = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(300.0, 200.0));
        let pts = decimate_line_points(&vals, vals.len(), -50.0, 999.0, plot);
        assert!(pts.len() <= 300 * 4);
        assert!(pts.iter().any(|p| (p.y - plot.min.y).abs() < 0.01));
        assert!(pts.iter().any(|p| (p.y - plot.max.y).abs() < 0.01));
    }
}
