use eframe::egui::{vec2, Color32, Painter, Pos2, Stroke};
use sheetcore::style::Border;

pub(crate) const STYLES: &[(u8, &str)] = &[
    (7, "极细线"),
    (6, "点线"),
    (5, "虚线"),
    (8, "点划线"),
    (9, "双点划线"),
    (1, "细实线"),
    (10, "中虚线"),
    (11, "中点划线"),
    (12, "中双点划线"),
    (13, "斜点划线"),
    (2, "中实线"),
    (3, "粗实线"),
    (4, "双线"),
];

pub(crate) fn paint(p: &Painter, a: Pos2, b: Pos2, style: u8, color: Color32, scale: f32) {
    let length = a.distance(b);
    if style == 0 || length < 0.5 || scale <= 0.0 {
        return;
    }
    let dir = (b - a) / length;
    let normal = vec2(-dir.y, dir.x);
    let stroke = Stroke::new(
        Border {
            style,
            color: [0; 3],
        }
        .px()
            * scale,
        color,
    );
    let pattern: &[f32] = match style {
        4 => {
            let offset = normal * (1.2 * scale);
            p.line_segment([a - offset, b - offset], stroke);
            p.line_segment([a + offset, b + offset], stroke);
            return;
        }
        5 | 10 => &[5., 3.],
        6 => &[1., 2.],
        8 | 11 | 13 => &[6., 2., 1., 2.],
        9 | 12 => &[6., 2., 1., 2., 1., 2.],
        _ => {
            p.line_segment([a, b], stroke);
            return;
        }
    };
    let mut start = 0.0;
    for pair in pattern.chunks_exact(2).cycle() {
        if start >= length {
            break;
        }
        let end = (start + pair[0] * scale).min(length);
        let tilt = if style == 13 {
            normal * scale * 0.65
        } else {
            eframe::egui::Vec2::ZERO
        };
        if pair[0] == 1.0 {
            p.circle_filled(a + dir * ((start + end) * 0.5), stroke.width * 0.5, color);
        } else {
            p.line_segment([a + dir * start - tilt, a + dir * end + tilt], stroke);
        }
        start = end + pair[1] * scale;
    }
}
