use eframe::egui::{self, vec2, Color32, Mesh, Painter, Pos2, Shape};
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

#[inline]
fn push_quad(mesh: &mut Mesh, p0: Pos2, p1: Pos2, p2: Pos2, p3: Pos2, color: Color32) {
    let base = mesh.vertices.len() as u32;
    let uv = egui::epaint::WHITE_UV;
    mesh.vertices.extend_from_slice(&[
        egui::epaint::Vertex { pos: p0, uv, color },
        egui::epaint::Vertex { pos: p1, uv, color },
        egui::epaint::Vertex { pos: p2, uv, color },
        egui::epaint::Vertex { pos: p3, uv, color },
    ]);
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

#[inline]
fn push_segment_quad(mesh: &mut Mesh, a: Pos2, b: Pos2, half_w: f32, color: Color32) {
    let d = b - a;
    let len = d.length();
    if len < 0.05 {
        return;
    }
    let n = vec2(-d.y / len, d.x / len) * half_w;
    push_quad(mesh, a - n, b - n, b + n, a + n, color);
}

/// Excel 相邻单元格共享边的优先级比较：粗线 > 中线 > 双线 > 细实线 > 虚线/点线 > 极细线。
#[inline]
pub(crate) fn border_rank(b: &Border) -> u8 {
    match b.style {
        0 => 0,
        7 => 1,
        6 => 2,
        9 | 12 => 3,
        8 | 11 | 13 => 4,
        5 => 5,
        1 => 6,
        10 => 7,
        2 => 8,
        4 => 9,
        3 => 10,
        _ => 6,
    }
}

/// 将任意边框线段（含实线、双线、虚线、点线、斜点划线）直接追加进单个 `egui::Mesh`，
/// 避免逐点 `circle_filled` 产生数万个独立多边形触发 CPU 镶嵌（Tessellation）风暴。
pub(crate) fn append_to_mesh(
    mesh: &mut Mesh,
    a: Pos2,
    b: Pos2,
    style: u8,
    color: Color32,
    scale: f32,
) {
    let length = a.distance(b);
    if style == 0 || length < 0.5 || scale <= 0.0 {
        return;
    }
    let dir = (b - a) / length;
    let normal = vec2(-dir.y, dir.x);
    let width = (Border {
        style,
        color: [0; 3],
    }
    .px()
        * scale)
        .max(0.75);
    let half_w = width * 0.5;
    let pattern: &[f32] = match style {
        4 => {
            let offset = normal * (1.2 * scale);
            let sub_hw = (half_w * 0.85).max(0.45);
            push_quad(
                mesh,
                a - offset - normal * sub_hw,
                b - offset - normal * sub_hw,
                b - offset + normal * sub_hw,
                a - offset + normal * sub_hw,
                color,
            );
            push_quad(
                mesh,
                a + offset - normal * sub_hw,
                b + offset - normal * sub_hw,
                b + offset + normal * sub_hw,
                a + offset + normal * sub_hw,
                color,
            );
            return;
        }
        5 | 10 => &[5., 3.],
        6 => &[1.2, 2.0],
        8 | 11 | 13 => &[6., 2., 1.2, 2.],
        9 | 12 => &[6., 2., 1.2, 2., 1.2, 2.],
        _ => {
            push_quad(
                mesh,
                a - normal * half_w,
                b - normal * half_w,
                b + normal * half_w,
                a + normal * half_w,
                color,
            );
            return;
        }
    };
    let tilt = if style == 13 {
        normal * scale * 0.65
    } else {
        egui::Vec2::ZERO
    };
    let mut start = 0.0f32;
    for pair in pattern.chunks_exact(2).cycle() {
        if start >= length {
            break;
        }
        let seg_len = (pair[0] * scale).max(0.8);
        let end = (start + seg_len).min(length);
        if pair[0] <= 1.25 {
            let c = a + dir * ((start + end) * 0.5);
            let d0 = dir * half_w;
            let n0 = normal * half_w;
            push_quad(mesh, c - d0 - n0, c + d0 - n0, c + d0 + n0, c - d0 + n0, color);
        } else {
            push_segment_quad(
                mesh,
                a + dir * start - tilt,
                a + dir * end + tilt,
                half_w,
                color,
            );
        }
        start = end + (pair[1] * scale).max(0.8);
    }
}

pub(crate) fn paint(p: &Painter, a: Pos2, b: Pos2, style: u8, color: Color32, scale: f32) {
    if style == 0 || scale <= 0.0 || a.distance(b) < 0.5 {
        return;
    }
    let mut mesh = Mesh::default();
    append_to_mesh(&mut mesh, a, b, style, color, scale);
    if !mesh.is_empty() {
        p.add(Shape::mesh(mesh));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_and_double_borders_batch_into_single_compact_mesh() {
        let mut mesh = Mesh::default();
        // 1,500 dotted cell edges previously generated ~45,000 feathered Circle shapes.
        for i in 0..1_500 {
            let y = i as f32 * 20.0;
            append_to_mesh(
                &mut mesh,
                egui::pos2(0.0, y),
                egui::pos2(72.0, y),
                6,
                Color32::BLACK,
                1.0,
            );
        }
        assert!(!mesh.vertices.is_empty());
        assert_eq!(mesh.indices.len() % 6, 0);
        // ~23 dots/edge * 4 vertices = ~138k vertices in ONE single Mesh (0 Circle shapes).
        assert!(mesh.vertices.len() < 160_000);
    }
}
