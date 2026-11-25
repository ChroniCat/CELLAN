//! 迷你图绘制：把单元格里的一条迷你图画进格子矩形。
//!
//! 版式照 Excel 365 实测：
//! - 四边各留 2px（锚点格 72×19 时绘图区 68×15）
//! - 折线 1px 线宽，5 个点以内也照常连线；空值按 gap / zero / span 处理
//! - 柱形按「含 0 的 min..max」缩放，零线画在 0 的位置（全正是底边）
//! - 盈亏是等高分柱：正值向上、负值向下，从体的中线分开
//! - 高点/低点/首点/末点/标记点画 2px 小方块，Excel 也是方块不是圆点

use eframe::egui::{self, Color32, Rect, Stroke, Vec2};
use sheetcore::sparkline::{EmptyAs, SparkKind, Sparkline};
use sheetcore::Workbook;

/// 迷你图主色（colorSeries）
fn series_color(sp: &Sparkline) -> Color32 {
    Color32::from_rgb(sp.color[0], sp.color[1], sp.color[2])
}

const NEGATIVE: Color32 = Color32::from_rgb(0xD0, 0x00, 0x00);
const HIGH_COL: Color32 = Color32::from_rgb(0x15, 0x80, 0x3D);
const LOW_COL: Color32 = Color32::from_rgb(0xDC, 0x26, 0x26);
const FIRST_COL: Color32 = Color32::from_rgb(0xD9, 0x77, 0x06);
const LAST_COL: Color32 = Color32::from_rgb(0x7C, 0x3A, 0xED);
const AXIS: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
const MARKER: Color32 = Color32::from_rgb(0xD0, 0x00, 0x00);

/// 取迷你图的数据点（行优先；空/非数字按 empty 规则处理）
pub fn values(wb: &Workbook, sheet_idx: usize, sp: &Sparkline) -> Vec<Option<f64>> {
    let sheet = match sp.source_sheet.as_deref() {
        Some(name) => match wb.sheets.iter().find(|s| s.name == name) {
            Some(s) => s,
            None => return Vec::new(),
        },
        None => wb.sheet(sheet_idx),
    };
    const MAX_SPARK_POINTS: usize = 4096;
    let (r1, c1, r2, c2) = sp.rect;
    // 单行 → 从左到右；单列 → 从上到下；多行多列按行优先（Excel 只允许单行/单列，总点数上限统一封顶 4096）
    let rows = r2.saturating_sub(r1) + 1;
    let cols = c2.saturating_sub(c1) + 1;
    let cap = (rows as usize)
        .saturating_mul(cols as usize)
        .min(MAX_SPARK_POINTS);
    let mut out = Vec::with_capacity(cap);
    if rows > 1 && cols > 1 {
        'outer: for r in r1..=r2.min(r1 + 4095) {
            for c in c1..=c2.min(c1 + 255) {
                if out.len() >= MAX_SPARK_POINTS {
                    break 'outer;
                }
                out.push(num_at(sheet, r, c));
            }
        }
        return out;
    }
    if rows == 1 {
        for c in c1..=c2.min(c1 + (MAX_SPARK_POINTS as u32 - 1)) {
            out.push(num_at(sheet, r1, c));
        }
    } else {
        for r in r1..=r2.min(r1 + (MAX_SPARK_POINTS as u32 - 1)) {
            out.push(num_at(sheet, r, c1));
        }
    }
    out
}

#[inline]
fn num_at(sheet: &sheetcore::Sheet, r: u32, c: u32) -> Option<f64> {
    match &sheet.get(r, c)?.value {
        sheetcore::Value::Number(n) => Some(*n),
        sheetcore::Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// 把一条迷你图画进 rect（rect 是单元格矩形）
pub fn draw(painter: &egui::Painter, rect: Rect, sp: &Sparkline, raw: &[Option<f64>]) {
    if rect.width() < 8.0 || rect.height() < 6.0 {
        return;
    }
    let area = rect.shrink2(Vec2::new(2.0, 2.0));
    if area.width() <= 2.0 || area.height() <= 2.0 {
        return;
    }
    // 空值处理
    let vals: Vec<Option<f64>> = raw
        .iter()
        .map(|v| match v {
            Some(x) if x.is_finite() => Some(*x),
            _ => match sp.empty {
                EmptyAs::Zero => Some(0.0),
                _ => None,
            },
        })
        .collect();
    let pts: Vec<(usize, f64)> = vals
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|x| (i, x)))
        .collect();
    if pts.is_empty() {
        return;
    }
    let n = vals.len().max(1);
    let color = series_color(sp);
    // 横向位置：Excel 把 n 个点均分到宽度上，首末点贴边
    let x_at = |i: usize| -> f32 {
        if n <= 1 {
            area.center().x
        } else {
            area.left() + area.width() * (i as f32) / ((n - 1) as f32)
        }
    };

    // 预先计算首点、末点、高点、低点索引（供折线、柱形与盈亏迷你图统一生效）
    let first_idx = pts.first().map(|(i, _)| *i);
    let last_idx = pts.last().map(|(i, _)| *i);
    let high_idx = pts
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| *i);
    let low_idx = pts
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| *i);
    let bar_color_for = |i: usize, v: f64| -> Color32 {
        if sp.high && high_idx == Some(i) {
            HIGH_COL
        } else if sp.low && low_idx == Some(i) {
            LOW_COL
        } else if sp.first && first_idx == Some(i) {
            FIRST_COL
        } else if sp.last && last_idx == Some(i) {
            LAST_COL
        } else if sp.negative && v < 0.0 {
            NEGATIVE
        } else {
            color
        }
    };

    match sp.kind {
        SparkKind::WinLoss => {
            // 每点一根等高分柱：中线以上为正、以下为负（支持高/低/首/末/负点高亮色）
            let mid = area.center().y;
            let half = area.height() / 2.0;
            let slot = area.width() / n as f32;
            let bw = (slot - 1.0).max(1.0);
            for (i, v) in vals.iter().enumerate() {
                let Some(val) = v else { continue };
                let x = area.left() + slot * i as f32 + (slot - bw) / 2.0;
                let bar = if *val >= 0.0 {
                    Rect::from_min_max(egui::pos2(x, mid - half), egui::pos2(x + bw, mid))
                } else {
                    Rect::from_min_max(egui::pos2(x, mid), egui::pos2(x + bw, mid + half))
                };
                painter.rect_filled(bar, 0.0, bar_color_for(i, *val));
            }
            if sp.axis {
                painter.line_segment(
                    [
                        egui::pos2(area.left(), mid),
                        egui::pos2(area.right(), mid),
                    ],
                    Stroke::new(1.0_f32, AXIS),
                );
            }
            return;
        }
        SparkKind::Column => {
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for (_, v) in &pts {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
            // 柱形永远把 0 包进来，零线才有意义
            lo = lo.min(0.0);
            hi = hi.max(0.0);
            let span = (hi - lo).max(f64::MIN_POSITIVE);
            let y_at = |v: f64| -> f32 {
                area.bottom() - (((v - lo) / span) as f32) * area.height()
            };
            let y0 = y_at(0.0);
            let slot = area.width() / n as f32;
            let bw = (slot - 1.0).max(1.0);
            for (i, v) in vals.iter().enumerate() {
                let Some(v) = v else { continue };
                let x = area.left() + slot * i as f32 + (slot - bw) / 2.0;
                let y = y_at(*v);
                let bar = Rect::from_min_max(
                    egui::pos2(x, y.min(y0)),
                    egui::pos2(x + bw, y.max(y0)),
                );
                painter.rect_filled(bar, 0.0, bar_color_for(i, *v));
            }
            if sp.axis {
                painter.line_segment(
                    [
                        egui::pos2(area.left(), y0),
                        egui::pos2(area.right(), y0),
                    ],
                    Stroke::new(1.0_f32, AXIS),
                );
            }
            return;
        }
        SparkKind::Line => {}
    }

    // 折线
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (_, v) in &pts {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    if (hi - lo).abs() < f64::EPSILON {
        // 全等值：画在中间一条水平线（Excel 也是这样）
        lo -= 0.5;
        hi += 0.5;
    }
    let span = (hi - lo).max(f64::MIN_POSITIVE);
    let y_at = |v: f64| -> f32 {
        area.bottom() - (((v - lo) / span) as f32) * area.height()
    };
    // 分段连线：gap 遇到空值就断，span 则跨过去
    let mut prev: Option<egui::Pos2> = None;
    for (i, v) in vals.iter().enumerate() {
        match v {
            Some(v) => {
                let p = egui::pos2(x_at(i), y_at(*v));
                if let Some(q) = prev {
                    let col = if sp.negative && (*v < 0.0) { NEGATIVE } else { color };
                    painter.line_segment([q, p], Stroke::new(1.0_f32, col));
                } else if i > 0 && sp.empty == EmptyAs::Connect {
                    // 上一个可视点（跨过空值连线）
                    if let Some((j, pv)) = pts.iter().rev().find(|(j, _)| *j < i) {
                        painter.line_segment(
                            [egui::pos2(x_at(*j), y_at(*pv)), p],
                            Stroke::new(1.0_f32, color),
                        );
                    }
                }
                prev = Some(p);
            }
            None => {
                if sp.empty != EmptyAs::Connect {
                    prev = None;
                }
            }
        }
    }

    // 标记与特殊点
    let mark = |p: egui::Pos2, c: Color32| {
        let r = Rect::from_center_size(p, Vec2::splat(3.0));
        painter.rect_filled(r, 0.0, c);
    };
    if sp.markers {
        for (i, v) in &pts {
            mark(egui::pos2(x_at(*i), y_at(*v)), MARKER);
        }
    }
    if sp.first {
        if let Some((i, v)) = pts.first() {
            mark(egui::pos2(x_at(*i), y_at(*v)), MARKER);
        }
    }
    if sp.last {
        if let Some((i, v)) = pts.last() {
            mark(egui::pos2(x_at(*i), y_at(*v)), MARKER);
        }
    }
    if sp.high {
        if let Some((i, v)) = pts.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)) {
            mark(egui::pos2(x_at(*i), y_at(*v)), MARKER);
        }
    }
    if sp.low {
        if let Some((i, v)) = pts.iter().min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)) {
            mark(egui::pos2(x_at(*i), y_at(*v)), MARKER);
        }
    }
    if sp.axis {
        let y0 = y_at(0.0).clamp(area.top(), area.bottom());
        painter.line_segment(
            [egui::pos2(area.left(), y0), egui::pos2(area.right(), y0)],
            Stroke::new(1.0_f32, AXIS),
        );
    }
}

/// 迷你图在功能区/对话框里的缩略预览（固定尺寸，用假数据也照常画）
pub fn preview(painter: &egui::Painter, rect: Rect, sp: &Sparkline) {
    let demo: Vec<Option<f64>> = vec![
        Some(3.0),
        Some(-1.0),
        Some(5.0),
        Some(4.0),
        Some(7.0),
        Some(2.0),
    ];
    draw(painter, rect, sp, &demo);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheetcore::sparkline::SparkKind;

    fn wb_with(vals: &[f64]) -> Workbook {
        let mut wb = Workbook::new_blank();
        for (i, v) in vals.iter().enumerate() {
            wb.sheets[0].set(0, i as u32, sheetcore::Cell {
                value: sheetcore::Value::Number(*v),
                formula: None,
            });
        }
        wb
    }

    #[test]
    fn reads_single_row_left_to_right() {
        let wb = wb_with(&[1.0, 2.0, 3.0]);
        let sp = Sparkline {
            rect: (0, 0, 0, 2),
            ..Default::default()
        };
        assert_eq!(values(&wb, 0, &sp), vec![Some(1.0), Some(2.0), Some(3.0)]);
    }

    #[test]
    fn reads_single_col_top_to_bottom() {
        let mut wb = Workbook::new_blank();
        for (i, v) in [4.0, 5.0].iter().enumerate() {
            wb.sheets[0].set(i as u32, 0, sheetcore::Cell {
                value: sheetcore::Value::Number(*v),
                formula: None,
            });
        }
        let sp = Sparkline {
            rect: (0, 0, 1, 0),
            ..Default::default()
        };
        assert_eq!(values(&wb, 0, &sp), vec![Some(4.0), Some(5.0)]);
    }

    #[test]
    fn missing_cells_are_gaps() {
        let mut wb = wb_with(&[1.0, 2.0, 3.0]);
        wb.sheets[0].remove(0, 1);
        let sp = Sparkline {
            rect: (0, 0, 0, 2),
            ..Default::default()
        };
        assert_eq!(values(&wb, 0, &sp), vec![Some(1.0), None, Some(3.0)]);
    }

    #[test]
    fn cross_sheet_source() {
        let mut wb = Workbook::new_blank();
        let mut s2 = sheetcore::Sheet::new("数据");
        s2.set(0, 0, sheetcore::Cell {
            value: sheetcore::Value::Number(9.0),
            formula: None,
        });
        wb.sheets.push(s2);
        let sp = Sparkline {
            source_sheet: Some("数据".into()),
            rect: (0, 0, 0, 0),
            ..Default::default()
        };
        assert_eq!(values(&wb, 0, &sp), vec![Some(9.0)]);
        // 表名不存在：空数据，不炸
        let sp2 = Sparkline {
            source_sheet: Some("没有这张表".into()),
            ..Default::default()
        };
        assert!(values(&wb, 0, &sp2).is_empty());
    }

    #[test]
    fn winloss_and_column_draw_without_panic() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let p = ui.painter().clone();
                let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(68.0, 15.0));
                let vals: Vec<Option<f64>> = vec![Some(1.0), Some(-2.0), None, Some(4.0)];
                for kind in [SparkKind::Line, SparkKind::Column, SparkKind::WinLoss] {
                    let sp = Sparkline {
                        kind,
                        markers: true,
                        high: true,
                        low: true,
                        first: true,
                        last: true,
                        axis: true,
                        ..Default::default()
                    };
                    draw(&p, rect, &sp, &vals);
                }
                // 空数据 + 全等值也不能炸
                draw(&p, rect, &Sparkline::default(), &[]);
                draw(
                    &p,
                    rect,
                    &Sparkline::default(),
                    &[Some(2.0), Some(2.0), Some(2.0)],
                );
            });
        });
    }
}
