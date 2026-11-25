//! 条件格式的图标集绘制：箭头、红绿灯、等级、旗帜、符号、四分之一圆、红黑、方块。

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use sheetcore::cf::{IconMark, IconStyle};

fn level_colors(levels: u8) -> Vec<Color32> {
    // 从低到高：红 → 黄 → 绿（与 Excel 图标集 idx=0 最低档、idx=levels-1 最高档一致）
    let red = Color32::from_rgb(255, 0, 0);
    let orange = Color32::from_rgb(255, 102, 0);
    let yellow = Color32::from_rgb(255, 192, 0);
    let light_green = Color32::from_rgb(146, 208, 80);
    let green = Color32::from_rgb(0, 176, 80);
    match levels {
        2 => vec![red, green],
        3 => vec![red, yellow, green],
        4 => vec![red, yellow, light_green, green],
        _ => vec![red, orange, yellow, light_green, green],
    }
}

fn red_to_black_color(idx: u8, levels: u8) -> Color32 {
    let palette = [
        Color32::from_rgb(45, 45, 45),
        Color32::from_rgb(140, 140, 140),
        Color32::from_rgb(240, 150, 150),
        Color32::from_rgb(220, 38, 38),
    ];
    let n = levels.max(1) as usize;
    let pos = if n <= 1 {
        palette.len() - 1
    } else {
        (idx as usize * (palette.len() - 1)) / (n - 1)
    };
    palette[pos.min(palette.len() - 1)]
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
            // 信号格：按档位点亮前 idx+1 根递增竖条，其余灰显
            let bars = mark.levels.clamp(3, 5) as usize;
            let lit = (mark.idx as usize + 1).min(bars);
            let gap = 1.0f32;
            let bw = ((size - gap * (bars as f32 - 1.0)) / bars as f32).max(1.5);
            let active = Color32::from_rgb(68, 114, 196);
            let idle = Color32::from_rgb(205, 210, 218);
            for b in 0..bars {
                let bh = size * (0.28 + 0.72 * (b as f32 + 1.0) / bars as f32);
                let x = box_rect.min.x + b as f32 * (bw + gap);
                let r = Rect::from_min_max(
                    Pos2::new(x, box_rect.max.y - bh),
                    Pos2::new(x + bw, box_rect.max.y),
                );
                painter.rect_filled(r, 0.5, if b < lit { active } else { idle });
            }
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
            let last = mark.levels.saturating_sub(1);
            let (glyph, fg) = if mark.idx == 0 {
                ("×", Color32::WHITE)
            } else if mark.idx >= last {
                ("✓", Color32::WHITE)
            } else {
                ("!", Color32::from_rgb(55, 55, 55))
            };
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::proportional(size * 0.8),
                fg,
            );
        }
        IconStyle::Quarters => {
            // 按小三角形扇面绘制，避免 >180° 凹多边形传入 convex_polygon 破面
            let rad = size / 2.0 - 0.5;
            painter.circle_filled(center, rad, Color32::from_rgb(220, 220, 220));
            let frac = if mark.levels <= 1 {
                1.0
            } else {
                (mark.idx as f32) / (mark.levels as f32 - 1.0)
            };
            if frac > 0.0 {
                let segments = 16usize;
                let sweep = frac * std::f32::consts::TAU;
                let fill = Color32::from_rgb(55, 65, 81);
                for k in 0..segments {
                    let a0 = -std::f32::consts::FRAC_PI_2 + sweep * k as f32 / segments as f32;
                    let a1 = -std::f32::consts::FRAC_PI_2 + sweep * (k + 1) as f32 / segments as f32;
                    let pts = vec![
                        center,
                        Pos2::new(center.x + rad * a0.cos(), center.y + rad * a0.sin()),
                        Pos2::new(center.x + rad * a1.cos(), center.y + rad * a1.sin()),
                    ];
                    painter.add(egui::Shape::convex_polygon(pts, fill, Stroke::NONE));
                }
            }
            painter.circle_stroke(center, rad, Stroke::new(0.8, Color32::from_rgb(90, 95, 105)));
        }
        IconStyle::RedToBlack => {
            painter.circle_filled(
                center,
                size / 2.0 - 0.5,
                red_to_black_color(mark.idx, mark.levels),
            );
        }
        IconStyle::Boxes => {
            painter.rect_filled(box_rect.shrink(1.0), 2.0, color);
        }
    }
}
