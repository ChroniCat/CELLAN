//! 出版级 GPU 交互图表渲染引擎（支持 22 种图表、8 套主题、回归趋势线、参考线、
//! 误差线、极值/增幅徽章、智能防撞标签、零延迟十字准线、富 Tooltip 与图例交互隔离）。

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Stroke, Vec2};
use sheetcore::chart_engine::{
    compute_axis_scale, compute_box_whisker, compute_error_bar_deltas, compute_funnel,
    compute_histogram, compute_insight_summary, compute_treemap, compute_trendline,
    compute_waterfall, format_chart_number, format_percent, layout_pie_callouts,
    resolve_reference_line, series_rgb, theme_spec, AxisScale, ChartThemeSpec, ResolvedRefLine,
    WaterfallBarKind,
};
use sheetcore::charts::{
    AxisConfig, BlankPolicy, Chart, ChartKind, LineDash, MarkerStyle, RefLineAxis, Series,
};
use sheetcore::model::DateSystem;

pub const PALETTE: [Color32; 8] = [
    Color32::from_rgb(37, 99, 235),
    Color32::from_rgb(14, 165, 233),
    Color32::from_rgb(245, 158, 11),
    Color32::from_rgb(16, 185, 129),
    Color32::from_rgb(139, 92, 246),
    Color32::from_rgb(239, 68, 68),
    Color32::from_rgb(236, 72, 153),
    Color32::from_rgb(20, 184, 166),
];

#[inline]
fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

#[inline]
fn rgba(c: [u8; 3], a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], a)
}

#[inline]
fn series_color(chart: &Chart, idx: usize) -> Color32 {
    rgb(series_rgb(chart, idx))
}

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
    fn dashed_segment(&mut self, a: Pos2, b: Pos2, stroke: Stroke, dash: LineDash) {
        match dash {
            LineDash::Solid => self.line_segment([a, b], stroke),
            LineDash::Dash => self.shapes.extend(egui::Shape::dashed_line(&[a, b], stroke, 6.0, 4.0)),
            LineDash::Dot => self.shapes.extend(egui::Shape::dashed_line(&[a, b], stroke, 2.0, 3.0)),
            LineDash::DashDot => {
                self.shapes.extend(egui::Shape::dashed_line(&[a, b], stroke, 7.0, 3.5));
            }
        }
    }

    #[inline]
    fn dashed_polyline(&mut self, pts: &[Pos2], stroke: Stroke, dash: LineDash) {
        if pts.len() < 2 {
            return;
        }
        match dash {
            LineDash::Solid => self.shapes.push(egui::Shape::line(pts.to_vec(), stroke)),
            LineDash::Dash => self.shapes.extend(egui::Shape::dashed_line(pts, stroke, 6.0, 4.0)),
            LineDash::Dot => self.shapes.extend(egui::Shape::dashed_line(pts, stroke, 2.0, 3.0)),
            LineDash::DashDot => {
                self.shapes.extend(egui::Shape::dashed_line(pts, stroke, 7.0, 3.5));
            }
        }
    }

    #[inline]
    fn circle_filled(&mut self, center: Pos2, radius: f32, fill: Color32) {
        self.shapes
            .push(egui::Shape::circle_filled(center, radius, fill));
    }

    #[inline]
    fn circle_stroke(&mut self, center: Pos2, radius: f32, stroke: Stroke) {
        self.shapes
            .push(egui::Shape::circle_stroke(center, radius, stroke));
    }

    #[inline]
    fn marker(&mut self, p: Pos2, radius: f32, style: MarkerStyle, color: Color32, bg: Color32) {
        match style {
            MarkerStyle::None => {}
            MarkerStyle::Auto | MarkerStyle::Circle => {
                self.circle_filled(p, radius, bg);
                self.circle_stroke(p, radius, Stroke::new(1.6, color));
            }
            MarkerStyle::Square => {
                let r = Rect::from_center_size(p, Vec2::splat(radius * 1.9));
                self.rect_filled(r, 1.0, bg);
                self.rect_stroke(r, 1.0, Stroke::new(1.5, color), egui::StrokeKind::Inside);
            }
            MarkerStyle::Diamond => {
                let d = radius * 1.25;
                let pts = vec![
                    Pos2::new(p.x, p.y - d),
                    Pos2::new(p.x + d, p.y),
                    Pos2::new(p.x, p.y + d),
                    Pos2::new(p.x - d, p.y),
                ];
                self.shapes.push(egui::Shape::convex_polygon(pts, color, Stroke::new(1.0, bg)));
            }
            MarkerStyle::Triangle => {
                let d = radius * 1.3;
                let pts = vec![
                    Pos2::new(p.x, p.y - d),
                    Pos2::new(p.x + d * 0.95, p.y + d * 0.8),
                    Pos2::new(p.x - d * 0.95, p.y + d * 0.8),
                ];
                self.shapes.push(egui::Shape::convex_polygon(pts, color, Stroke::new(1.0, bg)));
            }
        }
    }

    #[inline]
    fn text(
        &mut self,
        pos: Pos2,
        anchor: Align2,
        text: impl ToString,
        font_id: FontId,
        color: Color32,
    ) -> Rect {
        let galley = self.painter.layout_no_wrap(text.to_string(), font_id, color);
        let rect = anchor.anchor_size(pos, galley.size());
        self.shapes
            .push(egui::Shape::galley(rect.min, galley, color));
        rect
    }

    #[inline]
    fn add(&mut self, shape: egui::Shape) {
        self.shapes.push(shape);
    }
}

/// 布局几何结果（供静态渲染与实时交互命中测试共享，保证坐标 100% 一致）
#[derive(Clone, Debug)]
pub(crate) struct ChartLayout {
    pub card: Rect,
    pub title_rect: Option<Rect>,
    pub plot: Rect,
    pub legend_items: Vec<(usize, Rect)>,
    pub has_y2: bool,
}

pub(crate) fn compute_chart_layout(rect: Rect, chart: &Chart) -> ChartLayout {
    let mut inner = rect.shrink(10.0);
    let mut title_rect = None;
    if !chart.title.is_empty() {
        let tr = Rect::from_min_max(inner.min, Pos2::new(inner.max.x, inner.min.y + 20.0));
        title_rect = Some(tr);
        inner.min.y += 22.0;
    }
    if !chart.subtitle.is_empty() {
        inner.min.y += 15.0;
    }
    if chart.analytics.show_summary_bar {
        inner.max.y -= 18.0;
    }

    let mut legend_items = Vec::new();
    let show_legend = chart.legend
        && !chart.series.is_empty()
        && !matches!(
            chart.kind,
            ChartKind::Waterfall | ChartKind::Funnel | ChartKind::Treemap | ChartKind::Histogram
        );
    if show_legend {
        let pos = chart.legend_pos.as_str();
        if pos == "r" && inner.width() > 260.0 {
            let lw = 96.0f32;
            let lx = inner.max.x - lw + 6.0;
            let mut ly = inner.min.y + 6.0;
            for (i, s) in chart.series.iter().enumerate().take(12) {
                let item_r = Rect::from_min_size(Pos2::new(lx, ly), Vec2::new(lw - 8.0, 16.0));
                legend_items.push((i, item_r));
                ly += 18.0;
                if ly > inner.max.y - 16.0 {
                    break;
                }
                let _ = s;
            }
            inner.max.x -= lw;
        } else if pos == "l" && inner.width() > 260.0 {
            let lw = 96.0f32;
            let lx = inner.min.x + 2.0;
            let mut ly = inner.min.y + 6.0;
            for (i, _) in chart.series.iter().enumerate().take(12) {
                let item_r = Rect::from_min_size(Pos2::new(lx, ly), Vec2::new(lw - 8.0, 16.0));
                legend_items.push((i, item_r));
                ly += 18.0;
                if ly > inner.max.y - 16.0 {
                    break;
                }
            }
            inner.min.x += lw;
        } else {
            let is_top = pos == "t";
            let ly = if is_top { inner.min.y } else { inner.max.y - 16.0 };
            let mut x = inner.min.x + 6.0;
            for (i, s) in chart.series.iter().enumerate().take(10) {
                let name = if s.name.is_empty() { "系列" } else { s.name.as_str() };
                let est_w = (22.0 + name.chars().count() as f32 * 8.0).clamp(44.0, 120.0);
                if x + est_w > inner.max.x - 4.0 && i > 0 {
                    break;
                }
                legend_items.push((i, Rect::from_min_size(Pos2::new(x, ly), Vec2::new(est_w, 16.0))));
                x += est_w + 8.0;
            }
            if is_top {
                inner.min.y += 20.0;
            } else {
                inner.max.y -= 20.0;
            }
        }
    }

    let has_y2 = chart.has_secondary_axis()
        && matches!(
            chart.kind,
            ChartKind::Combo | ChartKind::Column | ChartKind::Line | ChartKind::Area
        );
    let non_axis = matches!(
        chart.kind,
        ChartKind::Pie | ChartKind::Doughnut | ChartKind::Radar | ChartKind::Funnel | ChartKind::Treemap
    );
    let left_pad = if non_axis {
        6.0
    } else if chart.kind.is_horizontal() {
        68.0
    } else {
        48.0 + if chart.effective_y_axis().title.is_empty() { 0.0 } else { 14.0 }
    };
    let right_pad = if has_y2 {
        48.0
    } else if non_axis {
        6.0
    } else {
        12.0
    };
    let bottom_pad = if non_axis {
        6.0
    } else {
        22.0 + if chart.effective_x_axis().title.is_empty() { 0.0 } else { 14.0 }
    };
    let plot = Rect::from_min_max(
        Pos2::new(inner.min.x + left_pad, inner.min.y + 6.0),
        Pos2::new(inner.max.x - right_pad, inner.max.y - bottom_pad),
    );
    ChartLayout {
        card: rect,
        title_rect,
        plot,
        legend_items,
        has_y2,
    }
}

/// 构建图表的整套绘制图元（供 `grid_render_cache` 局部坐标缓存重放，滚动 100% 命中）
pub fn build_chart_shapes(painter: &egui::Painter, rect: Rect, chart: &Chart) -> Vec<egui::Shape> {
    build_chart_shapes_in(painter, rect, chart, DateSystem::D1900)
}

pub fn build_chart_shapes_in(
    painter: &egui::Painter,
    rect: Rect,
    chart: &Chart,
    sys: DateSystem,
) -> Vec<egui::Shape> {
    if rect.width() < 48.0 || rect.height() < 36.0 {
        return Vec::new();
    }
    let mut cv = ChartCanvas {
        painter,
        shapes: Vec::with_capacity(64),
    };
    paint_chart_into(&mut cv, rect, chart, sys);
    cv.shapes
}

/// 直接在 rect 里画一张图
pub fn paint_chart(painter: &egui::Painter, rect: Rect, chart: &Chart) {
    let shapes = build_chart_shapes(painter, rect, chart);
    if !shapes.is_empty() {
        painter.extend(shapes);
    }
}

fn paint_chart_into(cv: &mut ChartCanvas<'_>, rect: Rect, chart: &Chart, sys: DateSystem) {
    let th = theme_spec(chart.style.theme);
    let cr = 6.0f32;
    if !chart.style.transparent_bg {
        if chart.style.show_card_shadow {
            let shadow = rect.translate(Vec2::new(0.0, 2.0));
            cv.rect_filled(shadow, cr, Color32::from_rgba_unmultiplied(15, 23, 42, 18));
        }
        cv.rect_filled(rect, cr, rgb(th.bg));
        cv.rect_stroke(rect, cr, Stroke::new(1.0, rgb(th.border)), egui::StrokeKind::Inside);
    }

    if matches!(chart.kind, ChartKind::Unknown) {
        let label = if chart.title.is_empty() {
            "高级图表（已完整保留原文件部件）".to_string()
        } else {
            format!("{}（已完整保留原文件部件）", chart.title)
        };
        cv.text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(12.0),
            rgb(th.muted_fg),
        );
        return;
    }

    let layout = compute_chart_layout(rect, chart);

    // 标题 & 副标题 & 右上角首末变化徽章
    if let Some(tr) = layout.title_rect {
        cv.text(
            Pos2::new(tr.center().x, tr.min.y + 1.0),
            Align2::CENTER_TOP,
            &chart.title,
            FontId::proportional(13.5),
            rgb(th.title_fg),
        );
    }
    if !chart.subtitle.is_empty() {
        let sy = layout
            .title_rect
            .map(|r| r.max.y + 1.0)
            .unwrap_or(rect.min.y + 10.0);
        cv.text(
            Pos2::new(rect.center().x, sy),
            Align2::CENTER_TOP,
            &chart.subtitle,
            FontId::proportional(10.5),
            rgb(th.subtitle_fg),
        );
    }
    if chart.analytics.show_growth_badge
        && let Some(sum) = compute_insight_summary(chart, sys)
        && let Some(g) = sum.growth_pct
    {
        let up = g >= 0.0;
        let badge_col = if up { th.pos_color } else { th.neg_color };
        let arrow = if up { "▲" } else { "▼" };
        let txt = format!("{arrow} {:+.1}%", g * 100.0);
        let badge_r = Rect::from_min_size(
            Pos2::new(rect.max.x - 76.0, rect.min.y + 8.0),
            Vec2::new(66.0, 18.0),
        );
        cv.rect_filled(badge_r, 9.0, rgba(badge_col, 34));
        cv.rect_stroke(badge_r, 9.0, Stroke::new(1.0, rgba(badge_col, 130)), egui::StrokeKind::Inside);
        cv.text(
            badge_r.center(),
            Align2::CENTER_CENTER,
            txt,
            FontId::proportional(10.0),
            rgb(badge_col),
        );
    }

    // 底部实时统计洞察条
    if chart.analytics.show_summary_bar
        && let Some(sum) = compute_insight_summary(chart, sys)
    {
        cv.text(
            Pos2::new(rect.center().x, rect.max.y - 6.0),
            Align2::CENTER_BOTTOM,
            sum.text,
            FontId::proportional(9.5),
            rgb(th.muted_fg),
        );
    }

    // 图例（含被隐藏系列的淡化与删除线视觉反馈）
    for &(si, item_r) in &layout.legend_items {
        if let Some(s) = chart.series.get(si) {
            let c = series_color(chart, si);
            let swatch_c = if s.hidden {
                Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 65)
            } else {
                c
            };
            let sw = Rect::from_min_size(
                Pos2::new(item_r.min.x, item_r.center().y - 4.5),
                Vec2::new(9.0, 9.0),
            );
            cv.rect_filled(sw, 2.0, swatch_c);
            let name = if s.name.is_empty() {
                format!("系列{}", si + 1)
            } else {
                s.name.clone()
            };
            let fg = if s.hidden {
                rgb(th.muted_fg)
            } else {
                rgb(th.text_fg)
            };
            let tr = cv.text(
                Pos2::new(sw.max.x + 5.0, item_r.center().y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(10.5),
                fg,
            );
            if s.hidden {
                cv.line_segment(
                    [Pos2::new(tr.min.x, tr.center().y), Pos2::new(tr.max.x, tr.center().y)],
                    Stroke::new(1.0, rgb(th.muted_fg)),
                );
            }
        }
    }

    let plot = layout.plot;
    if plot.width() < 24.0 || plot.height() < 20.0 {
        return;
    }

    match chart.kind {
        ChartKind::Pie | ChartKind::Doughnut => paint_pie(cv, plot, chart, &th, sys),
        ChartKind::Radar => paint_radar(cv, plot, chart, &th),
        ChartKind::Waterfall => paint_waterfall(cv, plot, chart, &th, sys),
        ChartKind::Funnel => paint_funnel(cv, plot, chart, &th, sys),
        ChartKind::Treemap => paint_treemap(cv, plot, chart, &th, sys),
        ChartKind::Histogram => paint_histogram(cv, plot, chart, &th, sys),
        ChartKind::BoxWhisker => paint_box_whisker(cv, plot, chart, &th, sys),
        ChartKind::Scatter | ChartKind::Bubble => paint_scatter_bubble(cv, plot, chart, &th, sys),
        _ => paint_cartesian(cv, plot, chart, &th, sys),
    }
}

fn paint_plot_frame_and_y_grid(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    th: &ChartThemeSpec,
    sc: &AxisScale,
    y_ax: &AxisConfig,
    sys: DateSystem,
) {
    if !chart.style.transparent_bg {
        cv.rect_filled(plot, 2.0, rgb(th.plot_bg));
    }
    let grid_stroke = Stroke::new(1.0, rgb(th.grid));
    let minor_stroke = Stroke::new(0.6, rgb(th.minor_grid));
    let axis_stroke = Stroke::new(1.1, rgb(th.axis_line));
    let txt_c = rgb(th.muted_fg);

    for (idx, &t) in sc.ticks.iter().enumerate() {
        let y = plot.max.y - plot.height() * sc.norm(t);
        if y_ax.show_major_grid {
            cv.dashed_segment(
                Pos2::new(plot.min.x, y),
                Pos2::new(plot.max.x, y),
                grid_stroke,
                LineDash::Dot,
            );
        }
        if y_ax.show_minor_grid && idx + 1 < sc.ticks.len() {
            let mid_v = 0.5 * (t + sc.ticks[idx + 1]);
            let ym = plot.max.y - plot.height() * sc.norm(mid_v);
            cv.line_segment([Pos2::new(plot.min.x, ym), Pos2::new(plot.max.x, ym)], minor_stroke);
        }
        if !y_ax.hidden {
            cv.text(
                Pos2::new(plot.min.x - 6.0, y),
                Align2::RIGHT_CENTER,
                format_chart_number(t, &y_ax.num_fmt, sys),
                FontId::proportional(9.5),
                txt_c,
            );
        }
    }
    cv.line_segment(
        [Pos2::new(plot.min.x, plot.max.y), Pos2::new(plot.max.x, plot.max.y)],
        axis_stroke,
    );
    cv.line_segment(
        [Pos2::new(plot.min.x, plot.min.y), Pos2::new(plot.min.x, plot.max.y)],
        axis_stroke,
    );
    if sc.min < 0.0 && sc.max > 0.0 {
        let zy = plot.max.y - plot.height() * sc.zero_norm();
        cv.line_segment(
            [Pos2::new(plot.min.x, zy), Pos2::new(plot.max.x, zy)],
            Stroke::new(1.1, rgb(th.axis_line)),
        );
    }
    let x_ax = chart.effective_x_axis();
    if !x_ax.title.is_empty() {
        cv.text(
            Pos2::new(plot.center().x, plot.max.y + 18.0),
            Align2::CENTER_TOP,
            &x_ax.title,
            FontId::proportional(10.0),
            rgb(th.text_fg),
        );
    }
    if !y_ax.title.is_empty() {
        cv.text(
            Pos2::new(plot.min.x - 4.0, plot.min.y - 3.0),
            Align2::RIGHT_BOTTOM,
            &y_ax.title,
            FontId::proportional(9.5),
            rgb(th.text_fg),
        );
    }
}

fn paint_category_labels(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    cats: &[String],
    n: usize,
    th: &ChartThemeSpec,
) {
    if cats.is_empty() || n == 0 {
        return;
    }
    let slot = plot.width() / n as f32;
    let max_visible = (plot.width() / 42.0).floor().max(2.0) as usize;
    let step = (n / max_visible).max(1);
    let txt_c = rgb(th.text_fg);
    for (i, c) in cats.iter().enumerate().take(n) {
        if i % step != 0 {
            continue;
        }
        let x = plot.min.x + slot * (i as f32 + 0.5);
        let clipped = if c.chars().count() > 10 && slot < 56.0 {
            let s: String = c.chars().take(8).collect();
            format!("{s}…")
        } else {
            c.clone()
        };
        cv.text(
            Pos2::new(x, plot.max.y + 4.0),
            Align2::CENTER_TOP,
            clipped,
            FontId::proportional(9.5),
            txt_c,
        );
    }
}

fn paint_reference_lines(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    sc1: &AxisScale,
    sc2: &AxisScale,
    primary_vals: &[f64],
    secondary_vals: &[f64],
) {
    for rl in &chart.ref_lines {
        let (sc, vals) = match rl.axis {
            RefLineAxis::YSecondary => (sc2, secondary_vals),
            _ => (sc1, primary_vals),
        };
        let Some(res) = resolve_reference_line(rl, vals) else {
            continue;
        };
        let c = rgb(rl.color);
        match res {
            ResolvedRefLine::Line { value, label } => {
                let y = plot.max.y - plot.height() * sc.norm(value);
                cv.dashed_segment(
                    Pos2::new(plot.min.x, y),
                    Pos2::new(plot.max.x, y),
                    Stroke::new(1.5, c),
                    rl.dash,
                );
                let pill_pos = Pos2::new(plot.max.x - 4.0, (y - 3.0).max(plot.min.y + 10.0));
                cv.text(pill_pos, Align2::RIGHT_BOTTOM, label, FontId::proportional(9.5), c);
            }
            ResolvedRefLine::Band { low, high, label } => {
                let y1 = plot.max.y - plot.height() * sc.norm(high);
                let y2 = plot.max.y - plot.height() * sc.norm(low);
                let br = Rect::from_min_max(
                    Pos2::new(plot.min.x, y1.min(y2)),
                    Pos2::new(plot.max.x, y1.max(y2)),
                );
                cv.rect_filled(br, 0.0, rgba(rl.color, 26));
                cv.dashed_segment(
                    Pos2::new(br.min.x, br.min.y),
                    Pos2::new(br.max.x, br.min.y),
                    Stroke::new(1.0, rgba(rl.color, 130)),
                    LineDash::Dot,
                );
                cv.dashed_segment(
                    Pos2::new(br.min.x, br.max.y),
                    Pos2::new(br.max.x, br.max.y),
                    Stroke::new(1.0, rgba(rl.color, 130)),
                    LineDash::Dot,
                );
                cv.text(
                    Pos2::new(br.max.x - 4.0, br.min.y + 2.0),
                    Align2::RIGHT_TOP,
                    label,
                    FontId::proportional(9.0),
                    c,
                );
            }
        }
    }
}

/// Catmull-Rom 平滑样条插值（将折线控制点平滑为丝滑贝塞尔曲线）
fn smooth_catmull_rom(pts: &[Pos2], plot: Rect) -> Vec<Pos2> {
    if pts.len() < 3 || pts.len() > 256 {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(pts.len() * 6);
    let sub = 6usize;
    for i in 0..(pts.len() - 1) {
        let p0 = if i == 0 { pts[0] } else { pts[i - 1] };
        let p1 = pts[i];
        let p2 = pts[i + 1];
        let p3 = if i + 2 < pts.len() { pts[i + 2] } else { pts[pts.len() - 1] };
        for s in 0..sub {
            let t = s as f32 / sub as f32;
            let t2 = t * t;
            let t3 = t2 * t;
            let x = 0.5
                * ((2.0 * p1.x)
                    + (-p0.x + p2.x) * t
                    + (2.0 * p0.x - 5.0 * p1.x + 4.0 * p2.x - p3.x) * t2
                    + (-p0.x + 3.0 * p1.x - 3.0 * p2.x + p3.x) * t3);
            let y = 0.5
                * ((2.0 * p1.y)
                    + (-p0.y + p2.y) * t
                    + (2.0 * p0.y - 5.0 * p1.y + 4.0 * p2.y - p3.y) * t2
                    + (-p0.y + 3.0 * p1.y - 3.0 * p2.y + p3.y) * t3);
            out.push(Pos2::new(
                x.clamp(plot.min.x, plot.max.x),
                y.clamp(plot.min.y, plot.max.y),
            ));
        }
    }
    out.push(pts[pts.len() - 1]);
    out
}

fn stepped_points(pts: &[Pos2]) -> Vec<Pos2> {
    if pts.len() < 2 {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(pts.len() * 2);
    out.push(pts[0]);
    for w in pts.windows(2) {
        out.push(Pos2::new(w[1].x, w[0].y));
        out.push(w[1]);
    }
    out
}

/// 像素桶包络降采样（first / min / max / last）：十万/百万点 O(N) 视觉零失真压缩
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

fn series_effective_kind(s: &Series, chart_kind: ChartKind) -> ChartKind {
    match s.kind {
        Some(k) if !matches!(k, ChartKind::Combo) => k,
        _ => match chart_kind {
            ChartKind::Combo => {
                if s.secondary {
                    ChartKind::Line
                } else {
                    ChartKind::Column
                }
            }
            k => k,
        },
    }
}

fn compute_cartesian_scales(chart: &Chart, vis: &[(usize, &Series)]) -> (AxisScale, AxisScale, Vec<f64>) {
    let n = vis
        .iter()
        .map(|(_, s)| s.vals.len().max(s.cats.len()))
        .max()
        .unwrap_or(0)
        .max(1);
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
    let (dmin1, dmax1) = if chart.kind.is_percent_stacked() {
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
    let mut y1_cfg = chart.effective_y_axis();
    if chart.kind.is_percent_stacked() && y1_cfg.num_fmt.is_empty() {
        y1_cfg.num_fmt = "0%".into();
    }
    let inc_zero = !matches!(chart.kind, ChartKind::Line);
    let sc1 = compute_axis_scale(dmin1, dmax1, &y1_cfg, inc_zero, 5);

    let (mut dmin2, mut dmax2) = (f64::INFINITY, f64::NEG_INFINITY);
    for (_, s) in vis.iter().filter(|(_, s)| s.secondary) {
        for &v in &s.vals {
            if v.is_finite() {
                dmin2 = dmin2.min(v);
                dmax2 = dmax2.max(v);
            }
        }
    }
    let sc2 = compute_axis_scale(dmin2, dmax2, &chart.y2_axis, true, 5);
    (sc1, sc2, totals)
}

fn paint_cartesian(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    th: &ChartThemeSpec,
    sys: DateSystem,
) {
    let vis: Vec<(usize, &Series)> = chart
        .series
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.hidden)
        .collect();
    if vis.is_empty() {
        return;
    }
    let cats: &[String] = vis.first().map(|(_, s)| s.cats.as_slice()).unwrap_or(&[]);
    let n = cats
        .len()
        .max(vis.iter().map(|(_, s)| s.vals.len()).max().unwrap_or(0))
        .max(1);
    let (sc1, sc2, totals) = compute_cartesian_scales(chart, &vis);
    let mut y1_cfg = chart.effective_y_axis();
    if chart.kind.is_percent_stacked() && y1_cfg.num_fmt.is_empty() {
        y1_cfg.num_fmt = "0%".into();
    }

    if chart.kind.is_horizontal() {
        paint_horizontal_bars(cv, plot, chart, &vis, cats, n, &sc1, &y1_cfg, &totals, th, sys);
        return;
    }

    paint_plot_frame_and_y_grid(cv, plot, chart, th, &sc1, &y1_cfg, sys);
    paint_category_labels(cv, plot, cats, n, th);

    // 次 Y 轴（右侧）
    if chart.has_secondary_axis() {
        cv.line_segment(
            [Pos2::new(plot.max.x, plot.min.y), Pos2::new(plot.max.x, plot.max.y)],
            Stroke::new(1.1, rgb(th.axis_line)),
        );
        for &t in &sc2.ticks {
            let y = plot.max.y - plot.height() * sc2.norm(t);
            cv.text(
                Pos2::new(plot.max.x + 5.0, y),
                Align2::LEFT_CENTER,
                format_chart_number(t, &chart.y2_axis.num_fmt, sys),
                FontId::proportional(9.5),
                rgb(th.muted_fg),
            );
        }
    }

    let primary_vals: Vec<f64> = vis
        .iter()
        .filter(|(_, s)| !s.secondary)
        .flat_map(|(_, s)| s.vals.iter().copied())
        .collect();
    let secondary_vals: Vec<f64> = vis
        .iter()
        .filter(|(_, s)| s.secondary)
        .flat_map(|(_, s)| s.vals.iter().copied())
        .collect();
    paint_reference_lines(cv, plot, chart, &sc1, &sc2, &primary_vals, &secondary_vals);

    let bar_vis_indices: Vec<usize> = vis
        .iter()
        .enumerate()
        .filter(|(_, (_, s))| {
            matches!(
                series_effective_kind(s, chart.kind),
                ChartKind::Column | ChartKind::ColumnStacked | ChartKind::ColumnStacked100
            )
        })
        .map(|(i, _)| i)
        .collect();
    let slot = plot.width() / n as f32;
    let gap_frac = (chart.style.gap_width as f32 / 100.0).clamp(0.2, 3.0);
    let group_fill = (1.0 / (1.0 + gap_frac * 0.35)).clamp(0.45, 0.86);
    let mut pos_acc = vec![0.0f64; n];
    let mut neg_acc = vec![0.0f64; n];
    let mut line_acc = vec![0.0f64; n];
    let zero_y1 = plot.max.y - plot.height() * sc1.zero_norm();
    let max_buckets = (plot.width().ceil() as usize).clamp(64, 2048);

    // 先画柱形与面积，再画折线（避免柱形遮挡折线）
    for pass in 0..2 {
        for (vis_i, &(si, s)) in vis.iter().enumerate() {
            let skind = series_effective_kind(s, chart.kind);
            let is_line_pass = matches!(skind, ChartKind::Line | ChartKind::LineStacked);
            if (pass == 0 && is_line_pass) || (pass == 1 && !is_line_pass) {
                continue;
            }
            let sc = if s.secondary { &sc2 } else { &sc1 };
            let col = series_color(chart, si);
            let y_of = |v: f64| plot.max.y - plot.height() * sc.norm(v);
            let zero_y = plot.max.y - plot.height() * sc.zero_norm();

            if matches!(
                skind,
                ChartKind::Line
                    | ChartKind::LineStacked
                    | ChartKind::Area
                    | ChartKind::AreaStacked
                    | ChartKind::AreaStacked100
            ) {
                let is_area = matches!(
                    skind,
                    ChartKind::Area | ChartKind::AreaStacked | ChartKind::AreaStacked100
                );
                // 百万点走降采样；中小型数据集保留精确 Gap/Zero/Span 分段与平滑/阶梯控制
                if s.vals.len() > max_buckets * 2 && !skind.is_stacked() {
                    let pts = decimate_line_points(&s.vals, n, sc.min, sc.max, plot);
                    if is_area && pts.len() >= 2 {
                        paint_area_mesh(cv, &pts, zero_y, col);
                    }
                    let lw = if s.line_width > 0.0 { s.line_width } else { 2.0 };
                    cv.dashed_polyline(&pts, Stroke::new(lw, col), s.dash);
                } else {
                    let mut segments: Vec<Vec<(usize, Pos2, f64)>> = vec![Vec::new()];
                    for (i, &raw) in s.vals.iter().enumerate().take(n) {
                        let v_opt = if raw.is_finite() {
                            Some(raw)
                        } else if chart.style.blank_policy == BlankPolicy::Zero {
                            Some(0.0)
                        } else {
                            None
                        };
                        match v_opt {
                            Some(raw_v) => {
                                let v = if skind.is_percent_stacked() {
                                    raw_v / totals[i]
                                } else if skind.is_stacked() {
                                    line_acc[i] += raw_v;
                                    line_acc[i]
                                } else {
                                    raw_v
                                };
                                let p = Pos2::new(plot.min.x + (i as f32 + 0.5) * slot, y_of(v));
                                segments.last_mut().unwrap().push((i, p, raw_v));
                            }
                            None => {
                                if chart.style.blank_policy == BlankPolicy::Gap
                                    && !segments.last().unwrap().is_empty()
                                {
                                    segments.push(Vec::new());
                                }
                            }
                        }
                    }
                    let lw = if s.line_width > 0.0 { s.line_width } else { 2.2 };
                    for seg in &segments {
                        if seg.is_empty() {
                            continue;
                        }
                        let raw_pts: Vec<Pos2> = seg.iter().map(|x| x.1).collect();
                        let draw_pts = if s.stepped {
                            stepped_points(&raw_pts)
                        } else if s.smooth {
                            smooth_catmull_rom(&raw_pts, plot)
                        } else {
                            raw_pts.clone()
                        };
                        if is_area && draw_pts.len() >= 2 {
                            paint_area_mesh(cv, &draw_pts, zero_y, col);
                        }
                        cv.dashed_polyline(&draw_pts, Stroke::new(lw, col), s.dash);
                        let draw_markers = s.marker != MarkerStyle::None
                            && (s.marker != MarkerStyle::Auto || (slot >= 12.0 && seg.len() <= 48));
                        if draw_markers {
                            for &(_, p, _) in seg {
                                cv.marker(p, 3.2, s.marker, col, rgb(th.bg));
                            }
                        }
                        if chart.show_labels && seg.len() <= 24 {
                            let fmt = if !chart.label_opts.num_fmt.is_empty() {
                                chart.label_opts.num_fmt.as_str()
                            } else if s.secondary {
                                chart.y2_axis.num_fmt.as_str()
                            } else {
                                y1_cfg.num_fmt.as_str()
                            };
                            for &(_, p, rv) in seg {
                                cv.text(
                                    Pos2::new(p.x, p.y - 6.0),
                                    Align2::CENTER_BOTTOM,
                                    format_chart_number(rv, fmt, sys),
                                    FontId::proportional(9.0),
                                    rgb(th.text_fg),
                                );
                            }
                        }
                    }
                }
            } else {
                // 柱形（簇状 / 堆积 / 百分比堆积）
                if n > max_buckets * 2 && !skind.is_stacked() {
                    let mut mesh = egui::Mesh::default();
                    let len = s.vals.len().max(1);
                    let b_pos = bar_vis_indices.iter().position(|&k| k == vis_i).unwrap_or(0);
                    let b_cnt = bar_vis_indices.len().max(1);
                    let bslot = plot.width() / max_buckets as f32;
                    let bw = (bslot / b_cnt as f32).max(1.0);
                    for b in 0..max_buckets {
                        let start = b * len / max_buckets;
                        let end = ((b + 1) * len / max_buckets).min(len);
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
                        let vy = y_of(peak);
                        let x = plot.min.x + bslot * b as f32 + bw * b_pos as f32;
                        mesh.add_colored_rect(
                            Rect::from_min_size(
                                Pos2::new(x, zero_y.min(vy)),
                                Vec2::new(bw, (vy - zero_y).abs().max(1.0)),
                            ),
                            col,
                        );
                    }
                    if !mesh.is_empty() {
                        cv.add(egui::Shape::mesh(mesh));
                    }
                } else {
                    let b_pos = bar_vis_indices.iter().position(|&k| k == vis_i).unwrap_or(0);
                    let b_cnt = bar_vis_indices.len().max(1);
                    let cr = if chart.style.rounded_bars && !skind.is_stacked() {
                        3.0
                    } else {
                        0.0
                    };
                    for (i, &raw) in s.vals.iter().enumerate().take(n) {
                        if !raw.is_finite() {
                            continue;
                        }
                        let v = if skind.is_percent_stacked() {
                            raw / totals[i]
                        } else {
                            raw
                        };
                        let (bar_r, label_y) = if skind.is_stacked() {
                            let (v0, v1) = if v >= 0.0 {
                                let a = pos_acc[i];
                                pos_acc[i] += v;
                                (a, pos_acc[i])
                            } else {
                                let a = neg_acc[i];
                                neg_acc[i] += v;
                                (neg_acc[i], a)
                            };
                            let bw = (slot * group_fill).max(2.0);
                            let bx = plot.min.x + i as f32 * slot + (slot - bw) * 0.5;
                            let y0 = y_of(v0);
                            let y1 = y_of(v1);
                            let r = Rect::from_min_max(
                                Pos2::new(bx, y0.min(y1)),
                                Pos2::new(bx + bw, y0.max(y1).max(y0.min(y1) + 1.0)),
                            );
                            (r, r.center().y)
                        } else {
                            let bw = (slot * group_fill / b_cnt as f32).max(1.5);
                            let bx = plot.min.x
                                + i as f32 * slot
                                + slot * (1.0 - group_fill) * 0.5
                                + bw * b_pos as f32;
                            let vy = y_of(v);
                            let r = Rect::from_min_max(
                                Pos2::new(bx, zero_y1.min(vy)),
                                Pos2::new(bx + bw, zero_y1.max(vy).max(zero_y1.min(vy) + 1.0)),
                            );
                            (r, if v >= 0.0 { r.min.y - 2.0 } else { r.max.y + 10.0 })
                        };
                        cv.rect_filled(bar_r, cr, col);
                        if chart.show_labels && bar_r.width() >= 14.0 && n <= 36 {
                            let txt = if skind.is_percent_stacked() || chart.label_opts.show_pct {
                                format_percent(raw / totals[i])
                            } else {
                                format_chart_number(raw, &y1_cfg.num_fmt, sys)
                            };
                            let fg = if skind.is_stacked() {
                                Color32::WHITE
                            } else {
                                rgb(th.text_fg)
                            };
                            if !skind.is_stacked() || bar_r.height() >= 13.0 {
                                cv.text(
                                    Pos2::new(bar_r.center().x, label_y),
                                    if skind.is_stacked() {
                                        Align2::CENTER_CENTER
                                    } else {
                                        Align2::CENTER_BOTTOM
                                    },
                                    txt,
                                    FontId::proportional(9.0),
                                    fg,
                                );
                            }
                        }
                    }
                }
            }

            // 误差线
            if let Some(eb) = &s.error_bars {
                let deltas = compute_error_bar_deltas(&s.vals, eb);
                let ecol = eb.color.map(rgb).unwrap_or(rgb(th.axis_line));
                let estroke = Stroke::new(1.2, ecol);
                for (i, (&v, &d)) in s.vals.iter().zip(deltas.iter()).enumerate().take(n) {
                    if !v.is_finite() || d <= 0.0 {
                        continue;
                    }
                    let cx = plot.min.x + (i as f32 + 0.5) * slot;
                    let y_hi = y_of(v + d);
                    let y_lo = y_of(v - d);
                    let y_mid = y_of(v);
                    match eb.dir {
                        sheetcore::charts::ErrorBarDir::Both => {
                            cv.line_segment([Pos2::new(cx, y_hi), Pos2::new(cx, y_lo)], estroke);
                            cv.line_segment([Pos2::new(cx - 3.0, y_hi), Pos2::new(cx + 3.0, y_hi)], estroke);
                            cv.line_segment([Pos2::new(cx - 3.0, y_lo), Pos2::new(cx + 3.0, y_lo)], estroke);
                        }
                        sheetcore::charts::ErrorBarDir::Plus => {
                            cv.line_segment([Pos2::new(cx, y_mid), Pos2::new(cx, y_hi)], estroke);
                            cv.line_segment([Pos2::new(cx - 3.0, y_hi), Pos2::new(cx + 3.0, y_hi)], estroke);
                        }
                        sheetcore::charts::ErrorBarDir::Minus => {
                            cv.line_segment([Pos2::new(cx, y_mid), Pos2::new(cx, y_lo)], estroke);
                            cv.line_segment([Pos2::new(cx - 3.0, y_lo), Pos2::new(cx + 3.0, y_lo)], estroke);
                        }
                    }
                }
            }

            // 趋势线（回归拟合 + 方程 & R² 徽章）
            for tl in &s.trendlines {
                let raw: Vec<(f64, f64)> = s
                    .vals
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| (i as f64, v))
                    .collect();
                if let Some(comp) = compute_trendline(&raw, tl) {
                    let tcol = tl.color.map(rgb).unwrap_or(col);
                    let tpts: Vec<Pos2> = comp
                        .points
                        .iter()
                        .map(|&(tx, ty)| {
                            let sx = (plot.min.x + ((tx + 0.5) as f32 / n as f32) * plot.width())
                                .clamp(plot.min.x, plot.max.x);
                            let sy = y_of(ty).clamp(plot.min.y, plot.max.y);
                            Pos2::new(sx, sy)
                        })
                        .collect();
                    cv.dashed_polyline(&tpts, Stroke::new(1.7, tcol), tl.dash);
                    if (tl.show_equation || tl.show_r2) && !tpts.is_empty() {
                        let mut txt = String::new();
                        if tl.show_equation {
                            txt.push_str(&comp.equation);
                        }
                        if tl.show_r2
                            && let Some(r2) = comp.r_squared
                        {
                            if !txt.is_empty() {
                                txt.push_str("  ");
                            }
                            txt.push_str(&format!("R²={r2:.3}"));
                        }
                        let last = tpts[tpts.len() - 1];
                        let anchor_p = Pos2::new(
                            last.x.clamp(plot.min.x + 80.0, plot.max.x - 4.0),
                            (last.y - 6.0).clamp(plot.min.y + 12.0, plot.max.y - 6.0),
                        );
                        cv.text(anchor_p, Align2::RIGHT_BOTTOM, txt, FontId::proportional(9.5), tcol);
                    }
                }
            }
        }
    }

    // 极值自动高亮（针对首个可见系列标出 Max / Min 气泡）
    if chart.analytics.highlight_extrema
        && let Some(&(si, s)) = vis.first()
    {
        let mut best_max: Option<(usize, f64)> = None;
        let mut best_min: Option<(usize, f64)> = None;
        for (i, &v) in s.vals.iter().enumerate().take(n) {
            if !v.is_finite() {
                continue;
            }
            if best_max.is_none_or(|(_, mv)| v > mv) {
                best_max = Some((i, v));
            }
            if best_min.is_none_or(|(_, mv)| v < mv) {
                best_min = Some((i, v));
            }
        }
        let sc = if s.secondary { &sc2 } else { &sc1 };
        let col = series_color(chart, si);
        for (tag, item, badge_c) in [
            ("高", best_max, th.pos_color),
            ("低", best_min, th.neg_color),
        ] {
            if let Some((idx, v)) = item {
                let p = Pos2::new(
                    plot.min.x + (idx as f32 + 0.5) * slot,
                    plot.max.y - plot.height() * sc.norm(v),
                );
                cv.circle_filled(p, 4.5, rgb(badge_c));
                cv.circle_stroke(p, 4.5, Stroke::new(1.5, rgb(th.bg)));
                let _ = col;
                cv.text(
                    Pos2::new(p.x, (p.y - 7.0).max(plot.min.y + 10.0)),
                    Align2::CENTER_BOTTOM,
                    format!("{tag}: {}", format_chart_number(v, &y1_cfg.num_fmt, sys)),
                    FontId::proportional(9.5),
                    rgb(badge_c),
                );
            }
        }
    }
}

fn paint_area_mesh(cv: &mut ChartCanvas<'_>, pts: &[Pos2], zero_y: f32, col: Color32) {
    if pts.len() < 2 {
        return;
    }
    let top_fill = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 105);
    let bot_fill = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 16);
    let mut mesh = egui::Mesh::default();
    let uv = egui::epaint::WHITE_UV;
    for w in pts.windows(2) {
        let base = mesh.vertices.len() as u32;
        mesh.vertices.extend_from_slice(&[
            egui::epaint::Vertex { pos: w[0], uv, color: top_fill },
            egui::epaint::Vertex { pos: w[1], uv, color: top_fill },
            egui::epaint::Vertex { pos: Pos2::new(w[1].x, zero_y), uv, color: bot_fill },
            egui::epaint::Vertex { pos: Pos2::new(w[0].x, zero_y), uv, color: bot_fill },
        ]);
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    cv.add(egui::Shape::mesh(mesh));
}

#[allow(clippy::too_many_arguments)]
fn paint_horizontal_bars(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    vis: &[(usize, &Series)],
    cats: &[String],
    n: usize,
    sc: &AxisScale,
    y_ax: &AxisConfig,
    totals: &[f64],
    th: &ChartThemeSpec,
    sys: DateSystem,
) {
    if !chart.style.transparent_bg {
        cv.rect_filled(plot, 2.0, rgb(th.plot_bg));
    }
    for &t in &sc.ticks {
        let x = plot.min.x + plot.width() * sc.norm(t);
        cv.dashed_segment(
            Pos2::new(x, plot.min.y),
            Pos2::new(x, plot.max.y),
            Stroke::new(1.0, rgb(th.grid)),
            LineDash::Dot,
        );
        cv.text(
            Pos2::new(x, plot.max.y + 4.0),
            Align2::CENTER_TOP,
            format_chart_number(t, &y_ax.num_fmt, sys),
            FontId::proportional(9.5),
            rgb(th.muted_fg),
        );
    }
    cv.line_segment(
        [Pos2::new(plot.min.x, plot.min.y), Pos2::new(plot.min.x, plot.max.y)],
        Stroke::new(1.1, rgb(th.axis_line)),
    );
    cv.line_segment(
        [Pos2::new(plot.min.x, plot.max.y), Pos2::new(plot.max.x, plot.max.y)],
        Stroke::new(1.1, rgb(th.axis_line)),
    );
    let zero_x = plot.min.x + plot.width() * sc.zero_norm();
    let row_slot = plot.height() / n.max(1) as f32;
    let step_lbl = (n / 14).max(1);
    for (i, c) in cats.iter().enumerate().take(n) {
        if i % step_lbl == 0 {
            let y = plot.min.y + (i as f32 + 0.5) * row_slot;
            cv.text(
                Pos2::new(plot.min.x - 5.0, y),
                Align2::RIGHT_CENTER,
                c,
                FontId::proportional(9.5),
                rgb(th.text_fg),
            );
        }
    }
    let mut pos_acc = vec![0.0f64; n];
    let mut neg_acc = vec![0.0f64; n];
    let stacked = chart.kind.is_stacked();
    let pct = chart.kind.is_percent_stacked();
    let cr = if chart.style.rounded_bars && !stacked { 2.5 } else { 0.0 };
    for (vi, &(si, s)) in vis.iter().enumerate() {
        let col = series_color(chart, si);
        for (i, &raw) in s.vals.iter().enumerate().take(n) {
            if !raw.is_finite() {
                continue;
            }
            let v = if pct { raw / totals[i] } else { raw };
            let r = if stacked {
                let (v0, v1) = if v >= 0.0 {
                    let a = pos_acc[i];
                    pos_acc[i] += v;
                    (a, pos_acc[i])
                } else {
                    let a = neg_acc[i];
                    neg_acc[i] += v;
                    (neg_acc[i], a)
                };
                let bh = (row_slot * 0.68).max(2.0);
                let y = plot.min.y + i as f32 * row_slot + (row_slot - bh) * 0.5;
                let x0 = plot.min.x + plot.width() * sc.norm(v0);
                let x1 = plot.min.x + plot.width() * sc.norm(v1);
                Rect::from_min_max(Pos2::new(x0.min(x1), y), Pos2::new(x0.max(x1).max(x0.min(x1) + 1.0), y + bh))
            } else {
                let bh = (row_slot * 0.74 / vis.len().max(1) as f32).max(2.0);
                let y = plot.min.y + i as f32 * row_slot + row_slot * 0.13 + bh * vi as f32;
                let vx = plot.min.x + plot.width() * sc.norm(v);
                Rect::from_min_max(Pos2::new(zero_x.min(vx), y), Pos2::new(zero_x.max(vx).max(zero_x.min(vx) + 1.0), y + bh))
            };
            cv.rect_filled(r, cr, col);
            if chart.show_labels && r.height() >= 10.0 && n <= 24 {
                let lbl = if pct {
                    format_percent(raw / totals[i])
                } else {
                    format_chart_number(raw, &y_ax.num_fmt, sys)
                };
                if stacked {
                    if r.width() >= 24.0 {
                        cv.text(r.center(), Align2::CENTER_CENTER, lbl, FontId::proportional(9.0), Color32::WHITE);
                    }
                } else {
                    let (lx, al) = if v >= 0.0 {
                        (r.max.x + 3.0, Align2::LEFT_CENTER)
                    } else {
                        (r.min.x - 3.0, Align2::RIGHT_CENTER)
                    };
                    cv.text(Pos2::new(lx, r.center().y), al, lbl, FontId::proportional(9.0), rgb(th.text_fg));
                }
            }
        }
    }
}

fn paint_waterfall(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    th: &ChartThemeSpec,
    sys: DateSystem,
) {
    let Some(s) = chart.series.iter().find(|s| !s.hidden) else {
        return;
    };
    let bars = compute_waterfall(s, &chart.analytics.waterfall_totals);
    if bars.is_empty() {
        return;
    }
    let mut dmin = 0.0f64;
    let mut dmax = 0.0f64;
    for b in &bars {
        dmin = dmin.min(b.y_start).min(b.y_end);
        dmax = dmax.max(b.y_start).max(b.y_end);
    }
    let y_ax = chart.effective_y_axis();
    let sc = compute_axis_scale(dmin, dmax, &y_ax, true, 5);
    paint_plot_frame_and_y_grid(cv, plot, chart, th, &sc, &y_ax, sys);
    let cats: Vec<String> = bars.iter().map(|b| b.label.clone()).collect();
    paint_category_labels(cv, plot, &cats, bars.len(), th);

    let slot = plot.width() / bars.len() as f32;
    let bw = (slot * 0.62).max(4.0);
    let cr = if chart.style.rounded_bars { 3.0 } else { 0.0 };
    for (i, b) in bars.iter().enumerate() {
        let bx = plot.min.x + i as f32 * slot + (slot - bw) * 0.5;
        let ya = plot.max.y - plot.height() * sc.norm(b.y_start);
        let yb = plot.max.y - plot.height() * sc.norm(b.y_end);
        let col = rgb(match b.kind {
            WaterfallBarKind::Increase => th.pos_color,
            WaterfallBarKind::Decrease => th.neg_color,
            WaterfallBarKind::Total => th.total_color,
        });
        let r = Rect::from_min_max(
            Pos2::new(bx, ya.min(yb)),
            Pos2::new(bx + bw, ya.max(yb).max(ya.min(yb) + 2.0)),
        );
        cv.rect_filled(r, cr, col);
        // 虚线桥连接下一柱起点
        if i + 1 < bars.len() {
            let next_bx = plot.min.x + (i + 1) as f32 * slot + (slot - bw) * 0.5;
            cv.dashed_segment(
                Pos2::new(r.max.x, yb),
                Pos2::new(next_bx, yb),
                Stroke::new(1.0, rgb(th.axis_line)),
                LineDash::Dot,
            );
        }
        if chart.show_labels && bw >= 16.0 {
            let prefix = match b.kind {
                WaterfallBarKind::Increase if b.delta > 0.0 => "+",
                _ => "",
            };
            let lbl = format!("{prefix}{}", format_chart_number(b.delta, &y_ax.num_fmt, sys));
            let (ly, al) = if b.kind == WaterfallBarKind::Decrease {
                (r.max.y + 2.0, Align2::CENTER_TOP)
            } else {
                (r.min.y - 2.0, Align2::CENTER_BOTTOM)
            };
            cv.text(Pos2::new(r.center().x, ly), al, lbl, FontId::proportional(9.5), col);
        }
    }
}

fn paint_funnel(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    th: &ChartThemeSpec,
    sys: DateSystem,
) {
    let Some(s) = chart.series.iter().find(|s| !s.hidden) else {
        return;
    };
    let stages = compute_funnel(s);
    if stages.is_empty() {
        return;
    }
    let n = stages.len();
    let row_h = (plot.height() / n as f32).min(46.0);
    let y0 = plot.min.y + (plot.height() - row_h * n as f32) * 0.5;
    let cx = plot.center().x;
    let max_w = plot.width() * 0.74;
    let fmt = chart.effective_y_axis().num_fmt;

    for (i, st) in stages.iter().enumerate() {
        let wt = max_w * st.width_frac as f32;
        let wb = stages
            .get(i + 1)
            .map(|nx| max_w * nx.width_frac as f32)
            .unwrap_or(wt * 0.8);
        let y1 = y0 + i as f32 * row_h + 2.0;
        let y2 = y1 + row_h - 4.0;
        let col = rgb(th.palette[i % th.palette.len()]);
        let pts = vec![
            Pos2::new(cx - wt * 0.5, y1),
            Pos2::new(cx + wt * 0.5, y1),
            Pos2::new(cx + wb * 0.5, y2),
            Pos2::new(cx - wb * 0.5, y2),
        ];
        cv.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
        let val_s = format_chart_number(st.value, &fmt, sys);
        let label = format!("{} · {}", st.label, val_s);
        cv.text(
            Pos2::new(cx, (y1 + y2) * 0.5),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(10.5),
            Color32::WHITE,
        );
        if let Some(conv) = st.step_conv {
            cv.text(
                Pos2::new(cx + wt.max(wb) * 0.5 + 8.0, (y1 + y2) * 0.5),
                Align2::LEFT_CENTER,
                format!("转化 {}", format_percent(conv)),
                FontId::proportional(9.5),
                rgb(th.muted_fg),
            );
        }
    }
}

fn paint_treemap(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    th: &ChartThemeSpec,
    sys: DateSystem,
) {
    let Some(s) = chart.series.iter().find(|s| !s.hidden) else {
        return;
    };
    let tiles = compute_treemap(s, plot.min.x, plot.min.y, plot.width(), plot.height());
    let fmt = chart.effective_y_axis().num_fmt;
    for (i, t) in tiles.iter().enumerate() {
        let r = Rect::from_min_size(Pos2::new(t.x, t.y), Vec2::new(t.w, t.h)).shrink(1.0);
        if r.width() <= 1.0 || r.height() <= 1.0 {
            continue;
        }
        let col = rgb(th.palette[i % th.palette.len()]);
        cv.rect_filled(r, 3.0, col);
        if r.width() >= 38.0 && r.height() >= 20.0 {
            cv.text(
                Pos2::new(r.center().x, r.center().y - if r.height() >= 36.0 { 6.0 } else { 0.0 }),
                Align2::CENTER_CENTER,
                &t.label,
                FontId::proportional(10.5),
                Color32::WHITE,
            );
            if r.height() >= 36.0 {
                let sub = format!(
                    "{} ({})",
                    format_chart_number(t.value, &fmt, sys),
                    format_percent(t.share)
                );
                cv.text(
                    Pos2::new(r.center().x, r.center().y + 8.0),
                    Align2::CENTER_CENTER,
                    sub,
                    FontId::proportional(9.0),
                    Color32::from_rgba_unmultiplied(255, 255, 255, 220),
                );
            }
        }
    }
}

fn paint_histogram(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    th: &ChartThemeSpec,
    sys: DateSystem,
) {
    let Some(s) = chart.series.iter().find(|s| !s.hidden) else {
        return;
    };
    let bins = compute_histogram(s, chart.analytics.hist_bins, chart.analytics.show_pareto_line);
    if bins.is_empty() {
        return;
    }
    let max_c = bins.iter().map(|b| b.count).max().unwrap_or(1) as f64;
    let y_ax = chart.effective_y_axis();
    let sc = compute_axis_scale(0.0, max_c, &y_ax, true, 5);
    paint_plot_frame_and_y_grid(cv, plot, chart, th, &sc, &y_ax, sys);
    let cats: Vec<String> = bins.iter().map(|b| b.label.clone()).collect();
    paint_category_labels(cv, plot, &cats, bins.len(), th);

    let slot = plot.width() / bins.len() as f32;
    let col = series_color(chart, 0);
    let cr = if chart.style.rounded_bars { 2.0 } else { 0.0 };
    let mut pareto_pts = Vec::new();
    for (i, b) in bins.iter().enumerate() {
        let bx = plot.min.x + i as f32 * slot + 1.5;
        let bw = (slot - 3.0).max(2.0);
        let vy = plot.max.y - plot.height() * sc.norm(b.count as f64);
        let r = Rect::from_min_max(Pos2::new(bx, vy), Pos2::new(bx + bw, plot.max.y));
        cv.rect_filled(r, cr, col);
        if chart.show_labels && bw >= 16.0 {
            cv.text(
                Pos2::new(r.center().x, r.min.y - 2.0),
                Align2::CENTER_BOTTOM,
                b.count.to_string(),
                FontId::proportional(9.0),
                rgb(th.text_fg),
            );
        }
        if chart.analytics.show_pareto_line {
            let py = plot.max.y - plot.height() * b.cum_pct as f32;
            pareto_pts.push(Pos2::new(r.center().x, py));
        }
    }
    if chart.analytics.show_pareto_line && pareto_pts.len() >= 2 {
        let pcol = rgb(th.palette[1]);
        cv.dashed_polyline(&pareto_pts, Stroke::new(2.0, pcol), LineDash::Solid);
        for &p in &pareto_pts {
            cv.marker(p, 3.0, MarkerStyle::Circle, pcol, rgb(th.bg));
        }
        // 右侧 0%~100% 帕累托刻度
        for k in 0..=4 {
            let f = k as f32 / 4.0;
            let y = plot.max.y - plot.height() * f;
            cv.text(
                Pos2::new(plot.max.x + 4.0, y),
                Align2::LEFT_CENTER,
                format!("{}%", k * 25),
                FontId::proportional(8.5),
                pcol,
            );
        }
    }
}

fn paint_box_whisker(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    th: &ChartThemeSpec,
    sys: DateSystem,
) {
    let boxes = compute_box_whisker(chart);
    if boxes.is_empty() {
        return;
    }
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for b in &boxes {
        lo = lo.min(b.whisker_lo);
        hi = hi.max(b.whisker_hi);
        for &o in &b.outliers {
            lo = lo.min(o);
            hi = hi.max(o);
        }
    }
    let y_ax = chart.effective_y_axis();
    let sc = compute_axis_scale(lo, hi, &y_ax, false, 5);
    paint_plot_frame_and_y_grid(cv, plot, chart, th, &sc, &y_ax, sys);
    let cats: Vec<String> = boxes.iter().map(|b| b.label.clone()).collect();
    paint_category_labels(cv, plot, &cats, boxes.len(), th);

    let slot = plot.width() / boxes.len() as f32;
    let bw = (slot * 0.44).clamp(12.0, 56.0);
    for (i, b) in boxes.iter().enumerate() {
        let cx = plot.min.x + (i as f32 + 0.5) * slot;
        let y_of = |v: f64| plot.max.y - plot.height() * sc.norm(v);
        let col = rgb(th.palette[i % th.palette.len()]);
        let st = Stroke::new(1.6, col);
        let y_lo = y_of(b.whisker_lo);
        let y_hi = y_of(b.whisker_hi);
        let y_q1 = y_of(b.q1);
        let y_q3 = y_of(b.q3);
        let y_med = y_of(b.median);
        let y_mean = y_of(b.mean);

        cv.line_segment([Pos2::new(cx, y_lo), Pos2::new(cx, y_hi)], st);
        cv.line_segment([Pos2::new(cx - bw * 0.3, y_lo), Pos2::new(cx + bw * 0.3, y_lo)], st);
        cv.line_segment([Pos2::new(cx - bw * 0.3, y_hi), Pos2::new(cx + bw * 0.3, y_hi)], st);
        let box_r = Rect::from_min_max(
            Pos2::new(cx - bw * 0.5, y_q3.min(y_q1)),
            Pos2::new(cx + bw * 0.5, y_q3.max(y_q1).max(y_q3.min(y_q1) + 2.0)),
        );
        cv.rect_filled(box_r, 2.0, rgba(th.palette[i % th.palette.len()], 60));
        cv.rect_stroke(box_r, 2.0, st, egui::StrokeKind::Inside);
        cv.line_segment([Pos2::new(box_r.min.x, y_med), Pos2::new(box_r.max.x, y_med)], Stroke::new(2.4, col));
        // 均值叉号
        cv.line_segment([Pos2::new(cx - 3.5, y_mean - 3.5), Pos2::new(cx + 3.5, y_mean + 3.5)], st);
        cv.line_segment([Pos2::new(cx + 3.5, y_mean - 3.5), Pos2::new(cx - 3.5, y_mean + 3.5)], st);
        for &ov in &b.outliers {
            cv.circle_filled(Pos2::new(cx, y_of(ov)), 3.0, col);
        }
    }
}

fn paint_scatter_bubble(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    th: &ChartThemeSpec,
    sys: DateSystem,
) {
    let vis: Vec<(usize, &Series)> = chart
        .series
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.hidden)
        .collect();
    let mut xmin = f64::INFINITY;
    let mut xmax = f64::NEG_INFINITY;
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    let mut smax = 1.0f64;
    for (_, s) in &vis {
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
    let x_ax = chart.effective_x_axis();
    let y_ax = chart.effective_y_axis();
    let xsc = compute_axis_scale(xmin, xmax, &x_ax, false, 5);
    let ysc = compute_axis_scale(ymin, ymax, &y_ax, false, 5);
    paint_plot_frame_and_y_grid(cv, plot, chart, th, &ysc, &y_ax, sys);
    for &tx in &xsc.ticks {
        let x = plot.min.x + plot.width() * xsc.norm(tx);
        if x_ax.show_major_grid {
            cv.dashed_segment(
                Pos2::new(x, plot.min.y),
                Pos2::new(x, plot.max.y),
                Stroke::new(1.0, rgb(th.grid)),
                LineDash::Dot,
            );
        }
        cv.text(
            Pos2::new(x, plot.max.y + 4.0),
            Align2::CENTER_TOP,
            format_chart_number(tx, &x_ax.num_fmt, sys),
            FontId::proportional(9.5),
            rgb(th.muted_fg),
        );
    }

    let is_bubble = chart.kind == ChartKind::Bubble;
    for &(si, s) in &vis {
        let rgb_c = series_rgb(chart, si);
        let col = rgb(rgb_c);
        let has_x = s.xv.len() == s.vals.len() && !s.xv.is_empty();
        let mut raw_xy = Vec::new();
        for (i, &y) in s.vals.iter().enumerate() {
            let x = if has_x { s.xv[i] } else { (i + 1) as f64 };
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            raw_xy.push((x, y));
            let px = plot.min.x + plot.width() * xsc.norm(x);
            let py = plot.max.y - plot.height() * ysc.norm(y);
            if is_bubble {
                let sz = s.sizes.get(i).copied().unwrap_or(10.0).max(0.1);
                let r = (4.5 + 22.0 * (sz / smax).sqrt() as f32).clamp(4.0, 26.0);
                cv.circle_filled(Pos2::new(px, py), r, rgba(rgb_c, 130));
                cv.circle_stroke(Pos2::new(px, py), r, Stroke::new(1.4, col));
            } else {
                cv.marker(
                    Pos2::new(px, py),
                    3.8,
                    if s.marker == MarkerStyle::Auto {
                        MarkerStyle::Circle
                    } else {
                        s.marker
                    },
                    col,
                    col,
                );
            }
        }
        for tl in &s.trendlines {
            if let Some(comp) = compute_trendline(&raw_xy, tl) {
                let tcol = tl.color.map(rgb).unwrap_or(col);
                let tpts: Vec<Pos2> = comp
                    .points
                    .iter()
                    .map(|&(tx, ty)| {
                        Pos2::new(
                            plot.min.x + plot.width() * xsc.norm(tx),
                            plot.max.y - plot.height() * ysc.norm(ty),
                        )
                    })
                    .collect();
                cv.dashed_polyline(&tpts, Stroke::new(1.7, tcol), tl.dash);
                if (tl.show_equation || tl.show_r2) && !tpts.is_empty() {
                    let mut txt = if tl.show_equation {
                        comp.equation.clone()
                    } else {
                        String::new()
                    };
                    if tl.show_r2
                        && let Some(r2) = comp.r_squared
                    {
                        if !txt.is_empty() {
                            txt.push_str("  ");
                        }
                        txt.push_str(&format!("R²={r2:.3}"));
                    }
                    let last = tpts[tpts.len() - 1];
                    cv.text(
                        Pos2::new(
                            last.x.clamp(plot.min.x + 80.0, plot.max.x - 4.0),
                            (last.y - 6.0).clamp(plot.min.y + 12.0, plot.max.y - 6.0),
                        ),
                        Align2::RIGHT_BOTTOM,
                        txt,
                        FontId::proportional(9.5),
                        tcol,
                    );
                }
            }
        }
    }
}

fn paint_radar(cv: &mut ChartCanvas<'_>, plot: Rect, chart: &Chart, th: &ChartThemeSpec) {
    let vis: Vec<(usize, &Series)> = chart
        .series
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.hidden)
        .collect();
    let n = vis
        .iter()
        .map(|(_, s)| s.vals.len().max(s.cats.len()))
        .max()
        .unwrap_or(0);
    if n < 3 {
        return;
    }
    let max_v = vis
        .iter()
        .flat_map(|(_, s)| s.vals.iter().copied())
        .filter(|v| v.is_finite())
        .fold(0.0f64, f64::max)
        .max(1e-9);
    let center = plot.center();
    let radius = plot.width().min(plot.height()) * 0.37;
    let web = Stroke::new(0.8, rgb(th.grid));
    for ring in 1..=4 {
        let r = radius * ring as f32 / 4.0;
        let mut pts = Vec::with_capacity(n + 1);
        for k in 0..=n {
            let a = -std::f32::consts::FRAC_PI_2
                + std::f32::consts::TAU * (k % n) as f32 / n as f32;
            pts.push(Pos2::new(center.x + r * a.cos(), center.y + r * a.sin()));
        }
        cv.add(egui::Shape::line(pts, web));
    }
    let cats: &[String] = vis.first().map(|(_, s)| s.cats.as_slice()).unwrap_or(&[]);
    for k in 0..n {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * k as f32 / n as f32;
        let tip = Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin());
        cv.line_segment([center, tip], web);
        if let Some(lbl) = cats.get(k)
            && !lbl.is_empty()
        {
            cv.text(
                Pos2::new(center.x + (radius + 12.0) * a.cos(), center.y + (radius + 12.0) * a.sin()),
                Align2::CENTER_CENTER,
                lbl,
                FontId::proportional(9.5),
                rgb(th.text_fg),
            );
        }
    }
    for &(si, s) in &vis {
        let rgb_c = series_rgb(chart, si);
        let col = rgb(rgb_c);
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
        let alpha = if chart.radar_style == "filled" { 90 } else { 28 };
        if pts.len() >= 3 {
            let fill = rgba(rgb_c, alpha);
            let mut mesh = egui::Mesh::default();
            let uv = egui::epaint::WHITE_UV;
            mesh.vertices.push(egui::epaint::Vertex { pos: center, uv, color: fill });
            for &p in &pts {
                mesh.vertices.push(egui::epaint::Vertex { pos: p, uv, color: fill });
            }
            for k in 0..pts.len() {
                mesh.indices.extend_from_slice(&[0, (k + 1) as u32, ((k + 1) % pts.len() + 1) as u32]);
            }
            cv.add(egui::Shape::mesh(mesh));
        }
        if pts.len() >= 2 {
            let mut closed = pts.clone();
            closed.push(pts[0]);
            cv.add(egui::Shape::line(closed, Stroke::new(2.0, col)));
        }
        if chart.radar_style != "filled" {
            for &p in &pts {
                cv.marker(p, 2.8, MarkerStyle::Circle, col, rgb(th.bg));
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
    let segments = ((sweep / 0.08).ceil() as usize).max(4);
    let uv = egui::epaint::WHITE_UV;
    if inner_r <= 0.5 {
        let c_idx = mesh.vertices.len() as u32;
        mesh.vertices.push(egui::epaint::Vertex { pos: center, uv, color });
        let first = mesh.vertices.len() as u32;
        for k in 0..=segments {
            let a = start + sweep * k as f32 / segments as f32;
            mesh.vertices.push(egui::epaint::Vertex {
                pos: Pos2::new(center.x + outer_r * a.cos(), center.y + outer_r * a.sin()),
                uv,
                color,
            });
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
            mesh.indices.extend_from_slice(&[o0, o0 + 2, o0 + 3, o0, o0 + 3, o0 + 1]);
        }
    }
}

fn paint_pie(
    cv: &mut ChartCanvas<'_>,
    plot: Rect,
    chart: &Chart,
    th: &ChartThemeSpec,
    sys: DateSystem,
) {
    let Some(s) = chart.series.first() else {
        return;
    };
    let vals = &s.vals;
    let total: f64 = vals.iter().copied().filter(|v| v.is_finite() && *v > 0.0).sum();
    if total <= 0.0 {
        return;
    }
    let show_callout =
        chart.show_labels || chart.label_opts.show_pct || chart.label_opts.show_cat;
    let radius = plot.width().min(plot.height()) * if show_callout { 0.34 } else { 0.42 };
    let center = plot.center();
    let mut start = -std::f32::consts::FRAC_PI_2
        + (chart.style.first_slice_ang as f32).to_radians();
    let inner = if chart.kind == ChartKind::Doughnut {
        radius * (chart.style.hole_size.clamp(15, 86) as f32 / 100.0)
    } else {
        0.0
    };
    let mut mesh = egui::Mesh::default();
    let mut callout_inputs = Vec::new();
    let exp_px = radius * (chart.style.pie_explosion as f32 / 100.0) * 0.32;
    let fmt = chart.effective_y_axis().num_fmt;
    let mut dividers = Vec::new();
    let mut rem_sweep = 0.0f32;
    for (i, &v) in vals.iter().enumerate() {
        if !v.is_finite() || v <= 0.0 {
            continue;
        }
        let sweep = (v / total) as f32 * std::f32::consts::TAU;
        if sweep < 0.004 && vals.len() > 360 {
            rem_sweep += sweep;
            continue;
        }
        let mid = start + sweep * 0.5;
        let scenter = Pos2::new(center.x + exp_px * mid.cos(), center.y + exp_px * mid.sin());
        let color = rgb(th.palette[i % th.palette.len()]);
        add_annular_slice(&mut mesh, scenter, inner, radius, start, sweep, color);
        dividers.push((scenter, start));
        if show_callout && sweep >= 0.06 {
            let mut parts = Vec::new();
            if chart.label_opts.show_cat || (!chart.show_labels && chart.label_opts.show_pct) {
                if let Some(c) = s.cats.get(i)
                    && !c.is_empty()
                {
                    parts.push(c.clone());
                }
            }
            if chart.show_labels {
                parts.push(format_chart_number(v, &fmt, sys));
            }
            if chart.label_opts.show_pct {
                parts.push(format_percent(v / total));
            }
            if !parts.is_empty() {
                callout_inputs.push((i, mid, parts.join(" ")));
            }
        }
        start += sweep;
    }
    if rem_sweep > 0.001 {
        add_annular_slice(&mut mesh, center, inner, radius, start, rem_sweep, rgb(th.muted_fg));
    }
    if !mesh.is_empty() {
        cv.add(egui::Shape::mesh(mesh));
    }
    if dividers.len() > 1 {
        let sep = Stroke::new(1.2, rgb(th.bg));
        for (scenter, a) in dividers {
            let (ca, sa) = (a.cos(), a.sin());
            cv.line_segment(
                [
                    Pos2::new(scenter.x + inner * ca, scenter.y + inner * sa),
                    Pos2::new(scenter.x + radius * ca, scenter.y + radius * sa),
                ],
                sep,
            );
        }
    }
    if !callout_inputs.is_empty() && plot.width() > 150.0 {
        let callouts = layout_pie_callouts(
            &callout_inputs,
            center.x,
            center.y,
            radius + exp_px,
            plot.min.y + 8.0,
            plot.max.y - 8.0,
            13.0,
        );
        let line_s = Stroke::new(0.9, rgb(th.axis_line));
        for c in callouts {
            if chart.label_opts.leader_lines || show_callout {
                cv.line_segment([Pos2::new(c.anchor_x, c.anchor_y), Pos2::new(c.elbow_x, c.elbow_y)], line_s);
                cv.line_segment([Pos2::new(c.elbow_x, c.elbow_y), Pos2::new(c.label_x, c.label_y)], line_s);
            }
            let (tx, align) = if c.on_right {
                (c.label_x + 3.0, Align2::LEFT_CENTER)
            } else {
                (c.label_x - 3.0, Align2::RIGHT_CENTER)
            };
            cv.text(Pos2::new(tx, c.label_y), align, c.text, FontId::proportional(9.5), rgb(th.text_fg));
        }
    }
    if chart.kind == ChartKind::Doughnut && chart.analytics.show_center_kpi && inner >= 24.0 {
        let val_s = format_chart_number(total, &fmt, sys);
        let sub = if chart.analytics.center_kpi_title.is_empty() {
            "总计"
        } else {
            chart.analytics.center_kpi_title.as_str()
        };
        cv.text(
            Pos2::new(center.x, center.y - 4.0),
            Align2::CENTER_CENTER,
            val_s,
            FontId::proportional((inner * 0.34).clamp(11.0, 18.0)),
            rgb(th.title_fg),
        );
        cv.text(
            Pos2::new(center.x, center.y + 11.0),
            Align2::CENTER_CENTER,
            sub,
            FontId::proportional(9.5),
            rgb(th.muted_fg),
        );
    }
}

// =============================================================================
// 实时交互层：零延迟十字准线、高亮光环、富 Tooltip、可点击图例、悬浮快捷操作条
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartQuickAction {
    CycleTheme,
    SwitchRowCol,
    ToggleLabels,
    ToggleTrendline,
    OpenStudio,
    ExportSvg,
    Duplicate,
    Delete,
}

#[derive(Clone, Debug, Default)]
pub struct ChartInteractionOut {
    pub toggled_legend_series: Option<usize>,
    pub isolated_legend_series: Option<usize>,
    pub quick_action: Option<ChartQuickAction>,
    pub title_double_clicked: bool,
    pub hovered: bool,
}

/// 在已绘制的图表之上叠加实时交互视觉（十字准线、悬停 Tooltip、图例点击与选中态快捷浮条）
pub fn paint_chart_interactive_overlay(
    painter: &egui::Painter,
    rect: Rect,
    chart: &Chart,
    pointer_pos: Option<Pos2>,
    clicked_pos: Option<Pos2>,
    double_clicked_pos: Option<Pos2>,
    selected: bool,
    sys: DateSystem,
) -> ChartInteractionOut {
    let mut out = ChartInteractionOut::default();
    if rect.width() < 60.0 || rect.height() < 45.0 || matches!(chart.kind, ChartKind::Unknown) {
        return out;
    }
    let layout = compute_chart_layout(rect, chart);
    let th = theme_spec(chart.style.theme);

    // 1. 双击标题判定
    if let (Some(dp), Some(tr)) = (double_clicked_pos, layout.title_rect)
        && tr.contains(dp)
    {
        out.title_double_clicked = true;
    }

    // 2. 可点击图例交互（单击切换隐藏/显示，双击单独聚焦）
    for &(si, item_r) in &layout.legend_items {
        if let Some(pp) = pointer_pos
            && item_r.contains(pp)
        {
            painter.rect_filled(item_r.expand(2.0), 3.0, rgba(th.palette[si % th.palette.len()], 24));
            out.hovered = true;
        }
        if let Some(dp) = double_clicked_pos
            && item_r.contains(dp)
        {
            out.isolated_legend_series = Some(si);
        } else if let Some(cp) = clicked_pos
            && item_r.contains(cp)
        {
            out.toggled_legend_series = Some(si);
        }
    }

    // 3. 选中态右上角悬浮快捷操作条（位于图表上方外沿，不挡绘图区）
    if selected {
        let pills: &[(&str, ChartQuickAction)] = &[
            ("主题", ChartQuickAction::CycleTheme),
            ("行列", ChartQuickAction::SwitchRowCol),
            ("标签", ChartQuickAction::ToggleLabels),
            ("趋势", ChartQuickAction::ToggleTrendline),
            ("设计", ChartQuickAction::OpenStudio),
            ("SVG", ChartQuickAction::ExportSvg),
            ("复制", ChartQuickAction::Duplicate),
            ("删", ChartQuickAction::Delete),
        ];
        let pill_h = 20.0f32;
        let total_w: f32 = pills
            .iter()
            .map(|(l, _)| if l.chars().count() <= 1 { 22.0 } else { 34.0 })
            .sum::<f32>()
            + (pills.len() as f32 + 1.0) * 3.0;
        let bar_y = (rect.min.y - pill_h - 4.0).max(rect.min.y + 2.0);
        let bar_r = Rect::from_min_size(
            Pos2::new((rect.max.x - total_w).max(rect.min.x), bar_y),
            Vec2::new(total_w, pill_h + 4.0),
        );
        painter.rect_filled(bar_r, 6.0, Color32::from_rgba_unmultiplied(24, 34, 52, 232));
        painter.rect_stroke(
            bar_r,
            6.0,
            Stroke::new(1.0, Color32::from_rgb(70, 92, 128)),
            egui::StrokeKind::Inside,
        );
        let mut bx = bar_r.min.x + 3.0;
        for &(lbl, act) in pills {
            let bw = if lbl.chars().count() <= 1 { 22.0 } else { 34.0 };
            let r = Rect::from_min_size(Pos2::new(bx, bar_r.min.y + 2.0), Vec2::new(bw, pill_h));
            let hov = pointer_pos.is_some_and(|p| r.contains(p));
            if hov {
                let bg = if act == ChartQuickAction::Delete {
                    Color32::from_rgb(200, 56, 56)
                } else {
                    Color32::from_rgb(56, 110, 210)
                };
                painter.rect_filled(r, 4.0, bg);
                out.hovered = true;
            }
            painter.text(
                r.center(),
                Align2::CENTER_CENTER,
                lbl,
                FontId::proportional(10.0),
                Color32::WHITE,
            );
            if clicked_pos.is_some_and(|p| r.contains(p)) {
                out.quick_action = Some(act);
            }
            bx += bw + 3.0;
        }
    }

    // 4. 实时十字准线 + 富 BI Tooltip
    let Some(pp) = pointer_pos else {
        return out;
    };
    if !layout.plot.contains(pp) || out.quick_action.is_some() {
        return out;
    }
    out.hovered = true;
    paint_hover_tooltip(painter, rect, layout.plot, chart, pp, &th, sys);
    out
}

fn paint_hover_tooltip(
    painter: &egui::Painter,
    card: Rect,
    plot: Rect,
    chart: &Chart,
    pp: Pos2,
    th: &ChartThemeSpec,
    sys: DateSystem,
) {
    let vis: Vec<(usize, &Series)> = chart
        .series
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.hidden)
        .collect();
    if vis.is_empty() {
        return;
    }
    let fmt = chart.effective_y_axis().num_fmt;
    let mut header = String::new();
    let mut rows: Vec<(Option<Color32>, String)> = Vec::new();

    match chart.kind {
        ChartKind::Pie | ChartKind::Doughnut => {
            let s = vis[0].1;
            let total: f64 = s.vals.iter().copied().filter(|v| v.is_finite() && *v > 0.0).sum();
            if total <= 0.0 {
                return;
            }
            let c = plot.center();
            let d = pp - c;
            let dist = d.length();
            let rad = plot.width().min(plot.height()) * 0.42;
            if dist > rad + 8.0 {
                return;
            }
            let mut ang = d.y.atan2(d.x) + std::f32::consts::FRAC_PI_2
                - (chart.style.first_slice_ang as f32).to_radians();
            while ang < 0.0 {
                ang += std::f32::consts::TAU;
            }
            ang %= std::f32::consts::TAU;
            let mut acc = 0.0f32;
            for (i, &v) in s.vals.iter().enumerate() {
                if !v.is_finite() || v <= 0.0 {
                    continue;
                }
                let sw = (v / total) as f32 * std::f32::consts::TAU;
                if ang >= acc && ang <= acc + sw {
                    header = s
                        .cats
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| format!("项 {}", i + 1));
                    rows.push((
                        Some(rgb(th.palette[i % th.palette.len()])),
                        format!(
                            "数值: {}  ({} 占比)",
                            format_chart_number(v, &fmt, sys),
                            format_percent(v / total)
                        ),
                    ));
                    break;
                }
                acc += sw;
            }
        }
        ChartKind::Treemap => {
            let tiles = compute_treemap(vis[0].1, plot.min.x, plot.min.y, plot.width(), plot.height());
            for (i, t) in tiles.iter().enumerate() {
                let r = Rect::from_min_size(Pos2::new(t.x, t.y), Vec2::new(t.w, t.h));
                if r.contains(pp) {
                    painter.rect_stroke(r, 3.0, Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Inside);
                    header = t.label.clone();
                    rows.push((
                        Some(rgb(th.palette[i % th.palette.len()])),
                        format!("{} ({})", format_chart_number(t.value, &fmt, sys), format_percent(t.share)),
                    ));
                    break;
                }
            }
        }
        ChartKind::Funnel => {
            let stages = compute_funnel(vis[0].1);
            if !stages.is_empty() {
                let row_h = (plot.height() / stages.len() as f32).min(46.0);
                let y0 = plot.min.y + (plot.height() - row_h * stages.len() as f32) * 0.5;
                let idx = ((pp.y - y0) / row_h).floor() as isize;
                if idx >= 0 && (idx as usize) < stages.len() {
                    let st = &stages[idx as usize];
                    header = st.label.clone();
                    rows.push((
                        Some(rgb(th.palette[idx as usize % th.palette.len()])),
                        format!("规模: {}", format_chart_number(st.value, &fmt, sys)),
                    ));
                    if let Some(c) = st.step_conv {
                        rows.push((None, format!("较上步转化: {}", format_percent(c))));
                    }
                    if let Some(tc) = st.total_conv {
                        rows.push((None, format!("总体转化率: {}", format_percent(tc))));
                    }
                }
            }
        }
        ChartKind::Waterfall => {
            let bars = compute_waterfall(vis[0].1, &chart.analytics.waterfall_totals);
            if !bars.is_empty() {
                let slot = plot.width() / bars.len() as f32;
                let idx = ((pp.x - plot.min.x) / slot).floor().clamp(0.0, (bars.len() - 1) as f32) as usize;
                let b = &bars[idx];
                let cx = plot.min.x + (idx as f32 + 0.5) * slot;
                painter.extend(egui::Shape::dashed_line(
                    &[Pos2::new(cx, plot.min.y), Pos2::new(cx, plot.max.y)],
                    Stroke::new(1.0, rgb(th.axis_line)),
                    4.0,
                    3.0,
                ));
                header = b.label.clone();
                let col = match b.kind {
                    WaterfallBarKind::Increase => th.pos_color,
                    WaterfallBarKind::Decrease => th.neg_color,
                    WaterfallBarKind::Total => th.total_color,
                };
                rows.push((
                    Some(rgb(col)),
                    format!("变动: {:+}", b.delta),
                ));
                rows.push((
                    None,
                    format!(
                        "桥接: {} → {}",
                        format_chart_number(b.y_start, &fmt, sys),
                        format_chart_number(b.y_end, &fmt, sys)
                    ),
                ));
            }
        }
        ChartKind::Histogram => {
            let bins = compute_histogram(vis[0].1, chart.analytics.hist_bins, chart.analytics.show_pareto_line);
            if !bins.is_empty() {
                let slot = plot.width() / bins.len() as f32;
                let idx = ((pp.x - plot.min.x) / slot).floor().clamp(0.0, (bins.len() - 1) as f32) as usize;
                let b = &bins[idx];
                header = format!("区间 {}", b.label);
                rows.push((Some(series_color(chart, 0)), format!("频数: {}", b.count)));
                rows.push((None, format!("累积占比: {}", format_percent(b.cum_pct))));
            }
        }
        ChartKind::BoxWhisker => {
            let boxes = compute_box_whisker(chart);
            if !boxes.is_empty() {
                let slot = plot.width() / boxes.len() as f32;
                let idx = ((pp.x - plot.min.x) / slot).floor().clamp(0.0, (boxes.len() - 1) as f32) as usize;
                let b = &boxes[idx];
                header = b.label.clone();
                rows.push((None, format!("上须/Q3: {} / {}", format_chart_number(b.whisker_hi, &fmt, sys), format_chart_number(b.q3, &fmt, sys))));
                rows.push((Some(rgb(th.palette[idx % th.palette.len()])), format!("中位/均值: {} / {}", format_chart_number(b.median, &fmt, sys), format_chart_number(b.mean, &fmt, sys))));
                rows.push((None, format!("Q1/下须: {} / {}  (离群 {})", format_chart_number(b.q1, &fmt, sys), format_chart_number(b.whisker_lo, &fmt, sys), b.outliers.len())));
            }
        }
        ChartKind::Scatter | ChartKind::Bubble => {
            // 找最近数据点
            let mut xmin = f64::INFINITY;
            let mut xmax = f64::NEG_INFINITY;
            let mut ymin = f64::INFINITY;
            let mut ymax = f64::NEG_INFINITY;
            for (_, s) in &vis {
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
            }
            if xmin.is_finite() && ymin.is_finite() {
                let xsc = compute_axis_scale(xmin, xmax, &chart.effective_x_axis(), false, 5);
                let ysc = compute_axis_scale(ymin, ymax, &chart.effective_y_axis(), false, 5);
                let mut best: Option<(f32, usize, usize, Pos2, f64, f64, Option<f64>)> = None;
                for &(si, s) in &vis {
                    let has_x = s.xv.len() == s.vals.len() && !s.xv.is_empty();
                    for (i, &y) in s.vals.iter().enumerate() {
                        let x = if has_x { s.xv[i] } else { (i + 1) as f64 };
                        if !x.is_finite() || !y.is_finite() {
                            continue;
                        }
                        let pos = Pos2::new(
                            plot.min.x + plot.width() * xsc.norm(x),
                            plot.max.y - plot.height() * ysc.norm(y),
                        );
                        let dist = pos.distance(pp);
                        if dist <= 26.0 && best.is_none_or(|b| dist < b.0) {
                            best = Some((dist, si, i, pos, x, y, s.sizes.get(i).copied()));
                        }
                    }
                }
                if let Some((_, si, _, pos, x, y, sz)) = best {
                    let c = series_color(chart, si);
                    painter.circle_stroke(pos, 7.0, Stroke::new(2.0, c));
                    header = chart.series[si].name.clone();
                    let mut line = format!(
                        "X={}  Y={}",
                        format_chart_number(x, &chart.effective_x_axis().num_fmt, sys),
                        format_chart_number(y, &fmt, sys)
                    );
                    if chart.kind == ChartKind::Bubble
                        && let Some(sv) = sz
                    {
                        line.push_str(&format!("  体量={}", format_chart_number(sv, "", sys)));
                    }
                    rows.push((Some(c), line));
                }
            }
        }
        ChartKind::Radar => {}
        _ => {
            let n = vis
                .iter()
                .map(|(_, s)| s.vals.len().max(s.cats.len()))
                .max()
                .unwrap_or(0);
            if n == 0 {
                return;
            }
            let idx = if chart.kind.is_horizontal() {
                let step = plot.height() / n as f32;
                let i = ((pp.y - plot.min.y) / step).floor().clamp(0.0, (n - 1) as f32) as usize;
                let cy = plot.min.y + (i as f32 + 0.5) * step;
                painter.extend(egui::Shape::dashed_line(
                    &[Pos2::new(plot.min.x, cy), Pos2::new(plot.max.x, cy)],
                    Stroke::new(1.0, rgb(th.axis_line)),
                    4.0,
                    3.0,
                ));
                i
            } else {
                let step = plot.width() / n as f32;
                let i = ((pp.x - plot.min.x) / step).floor().clamp(0.0, (n - 1) as f32) as usize;
                let cx = plot.min.x + (i as f32 + 0.5) * step;
                painter.extend(egui::Shape::dashed_line(
                    &[Pos2::new(cx, plot.min.y), Pos2::new(cx, plot.max.y)],
                    Stroke::new(1.0, rgb(th.axis_line)),
                    4.0,
                    3.0,
                ));
                let (sc1, sc2, _) = compute_cartesian_scales(chart, &vis);
                for &(si, s) in &vis {
                    if let Some(&v) = s.vals.get(i)
                        && v.is_finite()
                        && !chart.kind.is_stacked()
                    {
                        let sc = if s.secondary { &sc2 } else { &sc1 };
                        let py = plot.max.y - plot.height() * sc.norm(v);
                        let c = series_color(chart, si);
                        painter.circle_filled(Pos2::new(cx, py), 4.5, c);
                        painter.circle_stroke(Pos2::new(cx, py), 5.5, Stroke::new(1.5, rgb(th.bg)));
                    }
                }
                i
            };
            header = vis[0]
                .1
                .cats
                .get(idx)
                .cloned()
                .unwrap_or_else(|| format!("#{}", idx + 1));
            let slice_sum: f64 = vis
                .iter()
                .filter(|(_, s)| !s.secondary)
                .filter_map(|(_, s)| s.vals.get(idx).copied().filter(|v| v.is_finite()))
                .sum();
            for &(si, s) in vis.iter().take(6) {
                let Some(&v) = s.vals.get(idx) else { continue };
                if !v.is_finite() {
                    continue;
                }
                let sfmt = if s.secondary && !chart.y2_axis.num_fmt.is_empty() {
                    chart.y2_axis.num_fmt.as_str()
                } else {
                    fmt.as_str()
                };
                let mut line = format!(
                    "{}: {}",
                    if s.name.is_empty() { "值" } else { s.name.as_str() },
                    format_chart_number(v, sfmt, sys)
                );
                if vis.len() > 1 && !s.secondary && slice_sum.abs() > 1e-12 && v >= 0.0 {
                    line.push_str(&format!(" ({})", format_percent(v / slice_sum)));
                }
                if idx > 0
                    && let Some(&prev) = s.vals.get(idx - 1)
                    && prev.is_finite()
                    && prev.abs() > 1e-12
                {
                    let pct = (v - prev) / prev.abs() * 100.0;
                    let arrow = if pct >= 0.0 { "▲" } else { "▼" };
                    line.push_str(&format!("  {arrow}{:+.1}%", pct));
                }
                rows.push((Some(series_color(chart, si)), line));
            }
        }
    }

    if rows.is_empty() {
        return;
    }
    let bg = if th.is_dark {
        Color32::from_rgba_unmultiplied(15, 23, 42, 240)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 244)
    };
    let border = rgb(th.border);
    let fg = rgb(th.title_fg);
    let font_h = FontId::proportional(10.5);
    let font_r = FontId::proportional(10.0);
    let mut max_w = if header.is_empty() {
        80.0f32
    } else {
        painter.layout_no_wrap(header.clone(), font_h.clone(), fg).size().x + 16.0
    };
    let mut g_rows = Vec::with_capacity(rows.len());
    for (dot, txt) in rows {
        let g = painter.layout_no_wrap(txt, font_r.clone(), rgb(th.text_fg));
        max_w = max_w.max(g.size().x + if dot.is_some() { 26.0 } else { 16.0 });
        g_rows.push((dot, g));
    }
    let tip_h = (if header.is_empty() { 8.0 } else { 24.0 }) + g_rows.len() as f32 * 16.0 + 6.0;
    let tip_w = max_w.clamp(110.0, 280.0);
    let mut tx = pp.x + 14.0;
    if tx + tip_w > card.max.x - 4.0 {
        tx = (pp.x - tip_w - 14.0).max(card.min.x + 4.0);
    }
    let ty = (pp.y - tip_h * 0.5).clamp(card.min.y + 4.0, (card.max.y - tip_h - 4.0).max(card.min.y + 4.0));
    let tip_r = Rect::from_min_size(Pos2::new(tx, ty), Vec2::new(tip_w, tip_h));
    painter.rect_filled(tip_r.translate(Vec2::new(0.0, 2.0)), 6.0, Color32::from_rgba_unmultiplied(0, 0, 0, 28));
    painter.rect_filled(tip_r, 6.0, bg);
    painter.rect_stroke(tip_r, 6.0, Stroke::new(1.0, border), egui::StrokeKind::Inside);

    let mut cy = tip_r.min.y + 6.0;
    if !header.is_empty() {
        painter.text(Pos2::new(tip_r.min.x + 8.0, cy), Align2::LEFT_TOP, header, font_h, fg);
        cy += 17.0;
    }
    for (dot, g) in g_rows {
        let mut x = tip_r.min.x + 8.0;
        if let Some(c) = dot {
            painter.circle_filled(Pos2::new(x + 3.5, cy + 6.5), 3.5, c);
            x += 12.0;
        }
        painter.galley(Pos2::new(x, cy), g, rgb(th.text_fg));
        cy += 16.0;
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
