//! 条件格式的图标集绘制：箭头、红绿灯、等级、旗帜、符号、四分之一圆、红黑、方块。

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use sheetcore::cf::{IconMark, IconStyle};

fn level_colors(levels: u8) -> Vec<Color32> {
    // 从低到高：绿 → 黄 → 红（Excel 的 3 档就是这样）
    let green = Color32::from_rgb(0, 176, 80);
    let yellow = Color32::from_rgb(255, 192, 0);
    let red = Color32::from_rgb(255, 0, 0);
    let dark_red = Color32::from_rgb(192, 0, 0);
    match levels {
        2 => vec![red, green],
        3 => vec![green, yellow, red],
        4 => vec![green, Color32::from_rgb(146, 208, 80), yellow, red],
        _ => vec![
            green,
            Color32::from_rgb(146, 208, 80),
            yellow,
            Color32::from_rgb(255, 102, 0),
            red,
        ],
    }
    .into_iter()
    .map(|c| {
        let _ = dark_red;
        c
    })
    .collect()
}

pub fn draw_icon(painter: &egui::Painter, cell: Rect, mark: &IconMark) {
    let size = (cell.height() - 6.0).clamp(8.0, 14.0);
    let center = Pos2::new(cell.min.x + 3.0 + size / 2.0, cell.center().y);
    let box_rect = Rect::from_center_size(center, Vec2::splat(size));
    let colors = level_colors(mark.levels);
    let color = colors
        .get(mark.idx as usize)
        .copied()
        .unwrap_or(Color32::from_rgb(120, 120, 120));

    match mark.style {
        IconStyle::Arrows => {
            // 低档向下、高档向上，中间是横杠
            let t = if mark.levels <= 1 {
                0.5
            } else {
                mark.idx as f32 / (mark.levels - 1) as f32
            };
            if t < 0.34 {
                let pts = vec![
                    Pos2::new(box_rect.min.x, box_rect.min.y),
                    Pos2::new(box_rect.max.x, box_rect.min.y),
                    Pos2::new(box_rect.center().x, box_rect.max.y),
                ];
                painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
            } else if t < 0.67 {
                let h = size * 0.28;
                painter.rect_filled(
                    Rect::from_center_size(box_rect.center(), Vec2::new(size, h)),
                    0.0,
                    color,
                );
            } else {
                let pts = vec![
                    Pos2::new(box_rect.min.x, box_rect.max.y),
                    Pos2::new(box_rect.max.x, box_rect.max.y),
                    Pos2::new(box_rect.center().x, box_rect.min.y),
                ];
                painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
            }
        }
        IconStyle::TrafficLights => {
            painter.circle_filled(center, size / 2.0 - 0.5, color);
        }
        IconStyle::Rating => {
            // 已点亮的圆填色，其余画空心
            let lit = mark.idx + 1;
            let _ = lit;
            painter.circle_filled(center, size / 2.0 - 1.0, color);
            painter.circle_stroke(center, size / 2.0 - 1.0, Stroke::new(1.0, Color32::from_rgb(160, 160, 160)));
        }
        IconStyle::Flags => {
            let pole = Rect::from_min_size(
                Pos2::new(box_rect.min.x, box_rect.min.y),
                Vec2::new(1.5, size),
            );
            painter.rect_filled(pole, 0.0, Color32::from_rgb(90, 90, 90));
            let flag = Rect::from_min_size(
                Pos2::new(box_rect.min.x + 1.0, box_rect.min.y),
                Vec2::new(size - 2.0, size * 0.55),
            );
            painter.rect_filled(flag, 0.0, color);
        }
        IconStyle::Symbols => {
            painter.circle_filled(center, size / 2.0 - 0.5, color);
            let mark_color = if mark.idx == 0 {
                Color32::from_rgb(90, 90, 90)
            } else {
                Color32::WHITE
            };
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                if mark.idx == 0 { "!" } else { "✓" },
                egui::FontId::proportional(size * 0.8),
                mark_color,
            );
        }
        IconStyle::Quarters => {
            // 画一个圆，按档位涂扇形
            painter.circle_filled(center, size / 2.0 - 0.5, Color32::from_rgb(220, 220, 220));
            let frac = (mark.idx as f32 + 1.0) / mark.levels as f32;
            let segments = 12;
            let mut pts = vec![center];
            let sweep = frac * std::f32::consts::TAU;
            for k in 0..=segments {
                let a = -std::f32::consts::FRAC_PI_2 + sweep * k as f32 / segments as f32;
                pts.push(Pos2::new(
                    center.x + (size / 2.0) * a.cos(),
                    center.y + (size / 2.0) * a.sin(),
                ));
            }
            painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        IconStyle::RedToBlack => {
            painter.rect_filled(box_rect.shrink(1.0), 1.0, color);
        }
        IconStyle::Boxes => {
            painter.rect_filled(box_rect.shrink(1.0), 2.0, color);
        }
    }
}
