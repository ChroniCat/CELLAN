use super::SheetApp;
use eframe::egui::{self, Color32, Rect, Sense, Stroke, Vec2};
use sheetcore::style::Border;

const NAMES: [&str; 8] = [
    "左框线",
    "右框线",
    "上框线",
    "下框线",
    "内部横线",
    "内部竖线",
    "上斜线",
    "下斜线",
];
fn endpoints(r: Rect, i: usize) -> [egui::Pos2; 2] {
    let p = r.center();
    match i {
        0 => [r.left_top(), r.left_bottom()],
        1 => [r.right_top(), r.right_bottom()],
        2 => [r.left_top(), r.right_top()],
        3 => [r.left_bottom(), r.right_bottom()],
        4 => [egui::pos2(r.left(), p.y), egui::pos2(r.right(), p.y)],
        5 => [egui::pos2(p.x, r.top()), egui::pos2(p.x, r.bottom())],
        6 => [r.left_bottom(), r.right_top()],
        _ => [r.left_top(), r.right_bottom()],
    }
}

fn icon(
    ui: &mut egui::Ui,
    edges: &[usize],
    size: f32,
    name: &str,
    selected: Option<bool>,
) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let pal = crate::theme::palette();
    let visuals = ui.style().interact_selectable(&resp, selected.unwrap_or(false));
    ui.painter().rect(
        r,
        3.,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let box_r = r.shrink(size * 0.22);
    for i in 0..6 {
        let [a, b] = endpoints(box_r, i);
        crate::border_lines::paint(ui.painter(), a, b, 6, pal.text_muted, 1.);
    }
    for &i in edges {
        let [a, b] = endpoints(box_r, i);
        crate::border_lines::paint(ui.painter(), a, b, 1, pal.text, 1.);
    }
    crate::ui_kit::focus_ring(ui, &resp, r, 3.0);
    crate::ui_kit::a11y_name(&resp, name, ui.is_enabled(), selected);
    resp.on_hover_text(name)
}

impl SheetApp {
    pub(crate) fn read_border_draft(&mut self) {
        let (t, l, b, r) = self.sel_rect();
        let sh = self.wb.sheet(self.sheet_idx);
        let style = |row, col| self.wb.styles.get(sh.style_of(row, col));
        let tl = style(t, l);
        let mut lines = [Border::default(); 8];
        lines[..4].copy_from_slice(&[
            tl.border[0],
            style(t, r).border[1],
            tl.border[2],
            style(b, l).border[3],
        ]);
        if t < b {
            lines[4] = if tl.border[3].style != 0 {
                tl.border[3]
            } else {
                style(t + 1, l).border[2]
            };
        }
        if l < r {
            lines[5] = if tl.border[1].style != 0 {
                tl.border[1]
            } else {
                style(t, l + 1).border[0]
            };
        }
        if let Some(d) = &tl.diagonal {
            let line = Border {
                style: Border::from_ooxml_name(&d.style),
                color: d.color,
            };
            if d.up {
                lines[6] = line;
            }
            if d.down {
                lines[7] = line;
            }
        }
        self.fmt_lines = lines;
        self.fmt_line_edits = [false; 8];
        self.fmt_state.border = std::array::from_fn(|i| lines[i].style != 0);
        self.fmt_state.border_inside_h = lines[4].style != 0;
        self.fmt_state.border_inside_v = lines[5].style != 0;
    }
    fn set_draft_line(&mut self, i: usize, line: Border) {
        self.fmt_lines[i] = line;
        if i >= 6 && line.style != 0 && self.fmt_lines[13 - i].style != 0 {
            self.fmt_lines[13 - i] = line;
        }
        self.fmt_line_edits[i] = true;
        let on = line.style != 0;
        match i {
            0..=3 => {
                self.fmt_state.border[i] = on;
                self.fmt_touched.border[i] = true;
            }
            4 => {
                self.fmt_state.border_inside_h = on;
                self.fmt_touched.border_inside_h = true;
            }
            5 => {
                self.fmt_state.border_inside_v = on;
                self.fmt_touched.border_inside_v = true;
            }
            _ => {}
        }
    }
    fn pen(&self) -> Border {
        Border {
            style: self.fmt_border_style,
            color: self.fmt_border_color,
        }
    }
    fn toggle_line(&mut self, i: usize) {
        let pen = self.pen();
        self.set_draft_line(
            i,
            if self.fmt_lines[i] == pen {
                Border::default()
            } else {
                pen
            },
        );
    }
    fn edge_button(&mut self, ui: &mut egui::Ui, i: usize, enabled: bool) {
        ui.add_enabled_ui(enabled, |ui| {
            if icon(ui, &[i], 22., NAMES[i], Some(self.fmt_lines[i].style != 0)).clicked() {
                self.toggle_line(i);
            }
        });
    }
    pub(crate) fn format_borders_tab(&mut self, ui: &mut egui::Ui) {
        let pal = crate::theme::palette();
        let [red, green, blue] = self.fmt_border_color;
        let pen = Color32::from_rgb(red, green, blue);
        let rects = self.sel_rects();
        let h = rects.iter().any(|(t, _, b, _)| t != b);
        let v = rects.iter().any(|(_, l, _, r)| l != r);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(132.);
                crate::dlg_section(ui, "线条");
                ui.label("样式");
                egui::Grid::new("fmt_line_styles")
                    .spacing([2., 1.])
                    .show(ui, |ui| {
                        for row in 0..7 {
                            for col in 0..2 {
                                let index = row + col * 7;
                                let (style, name) = if index == 0 {
                                    (0, "无")
                                } else {
                                    crate::border_lines::STYLES[index - 1]
                                };
                                let (r, response) =
                                    ui.allocate_exact_size(Vec2::new(61., 19.), Sense::click());
                                let selected = self.fmt_border_style == style;
                                let surface = if selected {
                                    ui.painter().rect(r, 3.0, pal.accent_tint, Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
                                    pal.accent_tint
                                } else if response.hovered() {
                                    ui.painter().rect_filled(r, 3.0, pal.hover);
                                    pal.hover
                                } else { pal.dialog_bg };
                                if style == 0 {
                                    ui.painter().text(
                                        r.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "无",
                                        egui::FontId::proportional(12.),
                                        pal.text,
                                    );
                                } else {
                                    // 线型目录展示可分辨的线条，旁边的单元格预览始终使用原色。
                                    let line_lum = egui::Rgba::from(pen).intensity() + 0.05;
                                    let surface_lum = egui::Rgba::from(surface).intensity() + 0.05;
                                    let contrast = line_lum.max(surface_lum) / line_lum.min(surface_lum);
                                    crate::border_lines::paint(
                                        ui.painter(),
                                        r.left_center() + egui::vec2(4., 0.),
                                        r.right_center() - egui::vec2(4., 0.),
                                        style,
                                        if contrast >= 3.0 { pen } else { pal.text },
                                        1.,
                                    );
                                }
                                crate::ui_kit::focus_ring(ui, &response, r, 3.0);
                                crate::ui_kit::a11y_name(&response, name, ui.is_enabled(), Some(selected));
                                if response.on_hover_text(name).clicked() {
                                    self.fmt_border_style = style;
                                }
                            }
                            ui.end_row();
                        }
                    });
                ui.add_space(5.);
                ui.label("颜色");
                let rgb = self.fmt_border_color;
                ui.menu_button(
                    if rgb == [0, 0, 0] {
                        "自动 ▼".to_owned()
                    } else {
                        format!("#{:02X}{:02X}{:02X} ▼", rgb[0], rgb[1], rgb[2])
                    },
                    |ui| {
                        let mut automatic = false;
                        let mut picked = None;
                        crate::wps_chrome::paint_wps_color_palette(
                            ui,
                            Some(rgb),
                            &self.recent_colors,
                            Some("自动（黑色）"),
                            &mut automatic,
                            &mut picked,
                        );
                        if automatic {
                            self.fmt_border_color = [0, 0, 0];
                            ui.close_menu();
                        }
                        if let Some(color) = picked {
                            self.fmt_border_color = color;
                            ui.close_menu();
                        }
                        ui.horizontal(|ui| {
                            ui.label("自定义");
                            ui.color_edit_button_srgb(&mut self.fmt_border_color);
                        });
                    },
                );
            });
            ui.add_space(8.);
            ui.vertical(|ui| {
                crate::dlg_section(ui, "预置");
                ui.horizontal(|ui| {
                    for (name, edges) in [
                        ("无", &[][..]),
                        ("外边框", &[0, 1, 2, 3][..]),
                        ("内部", &[4, 5][..]),
                    ] {
                        ui.add_enabled_ui(!matches!(name, "内部") || h || v, |ui| {
                            ui.vertical(|ui| {
                                ui.set_width(55.);
                                let clicked = icon(ui, edges, 32., name, None).clicked();
                                if ui
                                    .add(egui::Label::new(name).sense(Sense::click()))
                                    .clicked()
                                    || clicked
                                {
                                    let pen = self.pen();
                                    if edges.is_empty() {
                                        for i in 0..8 {
                                            self.set_draft_line(i, Border::default());
                                        }
                                    } else {
                                        for &i in edges {
                                            if (i != 4 || h) && (i != 5 || v) {
                                                self.set_draft_line(i, pen);
                                            }
                                        }
                                    }
                                }
                            });
                        });
                    }
                });
                crate::dlg_section(ui, "边框");
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        self.edge_button(ui, 2, true);
                        ui.add_space(20.);
                        self.edge_button(ui, 4, h);
                        ui.add_space(20.);
                        self.edge_button(ui, 3, true);
                    });
                    let (outer, response) =
                        ui.allocate_exact_size(Vec2::new(190., 126.), Sense::click());
                    ui.painter().rect(
                        outer,
                        1.,
                        Color32::WHITE,
                        Stroke::new(1., Color32::LIGHT_GRAY),
                        egui::StrokeKind::Inside,
                    );
                    let cell = outer.shrink(7.);
                    let nx = if v { 2 } else { 1 };
                    let ny = if h { 2 } else { 1 };
                    for y in 0..ny {
                        for x in 0..nx {
                            let sz =
                                egui::vec2(cell.width() / nx as f32, cell.height() / ny as f32);
                            let tile = Rect::from_min_size(
                                cell.min + egui::vec2(x as f32 * sz.x, y as f32 * sz.y),
                                sz,
                            );
                            ui.painter().text(
                                tile.center(),
                                egui::Align2::CENTER_CENTER,
                                "文本",
                                egui::FontId::proportional(13.),
                                Color32::BLACK,
                            );
                            for i in [6, 7] {
                                let line = self.fmt_lines[i];
                                if line.style != 0 {
                                    let [a, b] = endpoints(tile, i);
                                    crate::border_lines::paint(
                                        ui.painter(),
                                        a,
                                        b,
                                        line.style,
                                        Color32::from_rgb(
                                            line.color[0],
                                            line.color[1],
                                            line.color[2],
                                        ),
                                        1.,
                                    );
                                }
                            }
                        }
                    }
                    for i in 0..6 {
                        if (i == 4 && !h) || (i == 5 && !v) {
                            continue;
                        }
                        let [a, b] = endpoints(cell, i);
                        let line = self.fmt_lines[i];
                        if line.style == 0 {
                            crate::border_lines::paint(
                                ui.painter(),
                                a,
                                b,
                                6,
                                Color32::LIGHT_GRAY,
                                1.,
                            );
                        } else {
                            crate::border_lines::paint(
                                ui.painter(),
                                a,
                                b,
                                line.style,
                                Color32::from_rgb(line.color[0], line.color[1], line.color[2]),
                                1.,
                            );
                        }
                    }
                    if response.clicked() {
                        if let Some(p) = response.interact_pointer_pos() {
                            let d = [
                                (p.x - cell.left()).abs(),
                                (p.x - cell.right()).abs(),
                                (p.y - cell.top()).abs(),
                                (p.y - cell.bottom()).abs(),
                                if h {
                                    (p.y - cell.center().y).abs()
                                } else {
                                    f32::INFINITY
                                },
                                if v {
                                    (p.x - cell.center().x).abs()
                                } else {
                                    f32::INFINITY
                                },
                            ];
                            if let Some((i, _)) = d
                                .iter()
                                .enumerate()
                                .filter(|(_, v)| **v <= 6.)
                                .min_by(|(_, a), (_, b)| a.total_cmp(b))
                            {
                                self.toggle_line(i);
                            } else if cell.contains(p) {
                                let w = cell.width() / nx as f32;
                                let ht = cell.height() / ny as f32;
                                let x = (p.x - cell.left()) % w;
                                let y = (p.y - cell.top()) % ht;
                                let down = (x * ht - y * w).abs() / w.hypot(ht);
                                let up = (x * ht - (ht - y) * w).abs() / w.hypot(ht);
                                if up.min(down) <= 6. {
                                    self.toggle_line(if up < down { 6 } else { 7 });
                                }
                            }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    for i in [6, 0, 5, 1, 7] {
                        if i == 5 || i == 1 {
                            ui.add_space(52.);
                        }
                        self.edge_button(ui, i, i != 5 || v);
                    }
                });
            });
        });
        ui.add_space(8.);
        ui.label("选择线型和颜色，然后单击预览或边框按钮。");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_dialog_keeps_draft_and_feedback_when_edit_is_blocked() {
        let _env = crate::test_env::TestEnv::new("border_dialog_blocked");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.open_format_dialog();
        app.toggle_line(0);
        app.deps_pending = true;
        app.apply_format_dialog();
        assert!(app.fmt_dlg);
        assert!(app.status.contains("后台构建依赖图"));
        assert!(app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).border[0].is_none());
        app.deps_pending = false;
        app.apply_format_dialog();
        assert!(!app.fmt_dlg);
        assert_eq!(
            app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).border[0].style,
            1
        );
    }

    #[test]
    fn border_dialog_keeps_per_edge_pens_and_inside_is_independent_of_outline() {
        let _env = crate::test_env::TestEnv::new("border_dialog_pens");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.sel = (0, 0);
        app.sel_end = (1, 1);
        app.open_format_dialog();
        let red = Border {
            style: 3,
            color: [200, 0, 0],
        };
        let blue = Border {
            style: 4,
            color: [0, 0, 200],
        };
        app.set_draft_line(0, red);
        app.set_draft_line(4, blue);
        app.set_draft_line(6, red);
        app.apply_format_dialog();
        let sh = app.wb.sheet(0);
        let tl = app.wb.styles.get(sh.style_of(0, 0));
        let br = app.wb.styles.get(sh.style_of(1, 1));
        assert_eq!(tl.border, [red, Border::default(), Border::default(), blue]);
        assert_eq!(
            br.border,
            [
                Border::default(),
                Border::default(),
                blue,
                Border::default()
            ]
        );
        assert!(
            tl.diagonal
                .as_ref()
                .is_some_and(|d| d.up && !d.down && d.color == red.color)
        );
        app.open_format_dialog();
        app.set_draft_line(0, Border::default());
        app.cancel_format_dialog();
        assert_eq!(
            app.wb.styles.get(app.wb.sheet(0).style_of(0, 0)).border[0],
            red
        );
    }
}
