//! Worksheet context menus for cells, headers, and sheet tabs.

use super::{
    ctx_menu_height, ctx_row_focusable, next_ctx_row, paint_fitted_label, theme, ui_icons, ui_kit,
    CtxAct, CtxKind, CtxRow, SheetApp, CTX_ROW_H, CTX_STRIP_H, CTX_W,
};
use eframe::egui::{self, Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, Vec2};

#[derive(Default)]
struct MenuResponse {
    rows: Vec<Rect>,
    pointed: Option<usize>,
    clicked: Option<usize>,
    paste: Option<CtxAct>,
}

fn bounded_panel(anchor: Pos2, size: Vec2, bounds: Rect) -> Rect {
    let size = size.min(bounds.size());
    Rect::from_min_size(anchor.clamp(bounds.min, bounds.max - size), size)
}

fn child_width(rows: &[CtxRow]) -> f32 {
    if rows.iter().any(|row| !row.shortcut.is_empty()) {
        220.0
    } else {
        184.0
    }
}

fn opens_left(parent: Rect, bounds: Rect, width: f32) -> bool {
    let right = bounds.right() - parent.right();
    right < width && parent.left() - bounds.left() > right
}

fn paste_strip(ui: &mut egui::Ui, rect: Rect, enabled: bool) -> Option<CtxAct> {
    use ui_icons::Icon;
    let pal = theme::palette();
    let tools = [
        (CtxAct::Paste, Icon::Paste, "全部粘贴"),
        (
            CtxAct::PasteValues,
            Icon::PasteValues123,
            "仅粘贴数值 (123)",
        ),
        (CtxAct::PasteFormulas, Icon::Fx, "仅粘贴公式"),
        (
            CtxAct::PasteTranspose,
            Icon::Transpose,
            "转置粘贴 (行列互换)",
        ),
        (CtxAct::PasteFormats, Icon::Brush, "仅粘贴格式"),
        (CtxAct::PasteAsPicture, Icon::PastePic, "粘贴为图片"),
    ];
    let strip = rect.shrink2(Vec2::new(6.0, 2.0));
    ui.painter().rect_filled(strip, 4.0, pal.header_bg);
    let slot = strip.width() / tools.len() as f32;
    for (idx, (action, icon, label)) in tools.into_iter().enumerate() {
        let hit = Rect::from_min_size(
            strip.min + Vec2::new(idx as f32 * slot, 0.0),
            Vec2::new(slot, strip.height()),
        );
        let resp = ui.interact(
            hit,
            ui.id().with(("paste", idx)),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if enabled && resp.hovered() {
            ui.painter().rect_filled(hit.shrink(1.0), 3.0, pal.hover);
        }
        let glyph = Rect::from_center_size(hit.center(), Vec2::splat(16.0));
        if enabled {
            ui_icons::draw_excel(ui.painter(), glyph, icon, pal.text, true);
        } else {
            ui_icons::draw_mono(ui.painter(), glyph, icon, pal.text_muted);
        }
        ui_kit::a11y_name(&resp, label, enabled, None);
        ui_kit::focus_ring(ui, &resp, hit.shrink(1.0), 3.0);
        if resp.on_hover_text(label).clicked() && enabled {
            return Some(action);
        }
    }
    None
}

struct MenuPanel<'a> {
    id: egui::Id,
    rect: Rect,
    opened: u64,
    rows: &'a [CtxRow],
    selected: usize,
    reveal: bool,
    child: bool,
    tab_color: Option<[u8; 3]>,
}

impl MenuPanel<'_> {
    fn show(self, ctx: &egui::Context) -> MenuResponse {
        let mut result = MenuResponse::default();
        egui::Area::new(self.id)
            .order(egui::Order::Foreground)
            .movable(false)
            .constrain(false)
            .fixed_pos(self.rect.min)
            .default_size(self.rect.size())
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(self.rect.size(), Sense::hover());
                let frame = ui_kit::popup_frame(ui.style()).inner_margin(0);
                ui.painter()
                    .add(frame.paint(rect.shrink(frame.stroke.width)));
                let inner = rect.shrink2(Vec2::new(1.0, 4.0));
                let mut body = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(inner)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                body.spacing_mut().item_spacing.y = 0.0;
                let mut scroll = egui::ScrollArea::vertical()
                    .max_height(inner.height())
                    .min_scrolled_height(0.0)
                    .auto_shrink([false, true]);
                let session_id = self.id.with("scroll_session");
                let fresh = ctx.data_mut(|data| {
                    let fresh = data.get_temp::<u64>(session_id) != Some(self.opened);
                    if fresh {
                        data.insert_temp(session_id, self.opened);
                    }
                    fresh
                });
                if fresh {
                    scroll = scroll.vertical_scroll_offset(0.0);
                }
                scroll.show(&mut body, |ui| self.paint_rows(ui, &mut result));
            });
        result
    }
    fn paint_rows(&self, ui: &mut egui::Ui, result: &mut MenuResponse) {
        let pal = theme::palette();
        let pointer_moved = ui.input(|i| i.pointer.delta() != Vec2::ZERO);
        for (idx, row) in self.rows.iter().enumerate() {
            let height = if row.is_sep() {
                8.0
            } else if row.strip {
                CTX_STRIP_H
            } else {
                CTX_ROW_H
            };
            let (id, r) = ui.allocate_space(Vec2::new(ui.available_width(), height));
            result.rows.push(r);
            if self.reveal && idx == self.selected {
                ui.scroll_to_rect_animation(r, None, egui::style::ScrollAnimation::none());
            }
            if row.is_sep() {
                let c = r.center().y;
                ui.painter().line_segment(
                    [
                        egui::pos2(r.left() + 10.0, c),
                        egui::pos2(r.right() - 10.0, c),
                    ],
                    Stroke::new(1.0 / ui.ctx().pixels_per_point(), pal.card_border),
                );
                continue;
            }
            if row.strip {
                result.paste = paste_strip(ui, r, row.enabled);
                continue;
            }
            let resp = ui.interact(
                r,
                id,
                if row.enabled {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            );
            if resp.hovered() && pointer_moved {
                result.pointed = Some(idx);
            }
            if row.enabled && resp.clicked() {
                result.clicked = Some(idx);
            }
            let selected = self.selected == idx;
            if selected {
                ui.painter()
                    .rect_filled(r.shrink2(Vec2::new(4.0, 1.0)), 4.0, pal.hover);
            }
            let fg = if row.enabled {
                pal.text
            } else {
                pal.text_muted
            };
            let secondary = if row.enabled {
                pal.text_weak
            } else {
                pal.text_muted
            };
            let glyph =
                Rect::from_center_size(r.left_center() + Vec2::new(20.0, 0.0), Vec2::splat(16.0));
            if let Some(icon) = row.icon {
                if row.enabled {
                    ui_icons::draw_excel(ui.painter(), glyph, icon, fg, true);
                } else {
                    ui_icons::draw_mono(ui.painter(), glyph, icon, fg);
                }
            }
            let color_item = matches!(row.act, Some(CtxAct::TabColor(_)));
            let checked =
                matches!(row.act, Some(CtxAct::TabColor(color)) if self.tab_color == color);
            if color_item {
                let swatch = glyph.shrink(1.0);
                let fill = row
                    .swatch
                    .map_or(pal.dialog_bg, |[r, g, b]| Color32::from_rgb(r, g, b));
                ui.painter().rect(
                    swatch,
                    2.0,
                    fill,
                    Stroke::new(1.0, pal.text_muted),
                    egui::StrokeKind::Inside,
                );
                if row.swatch.is_none() {
                    let mark = swatch.shrink(2.0);
                    ui.painter().line_segment(
                        [mark.left_bottom(), mark.right_top()],
                        Stroke::new(1.3, pal.text_weak),
                    );
                }
            }
            let inset = if self.child && !color_item && row.icon.is_none() {
                12.0
            } else {
                36.0
            };
            let mut label = r.shrink2(Vec2::new(12.0, 0.0));
            label.min.x = r.left() + inset;
            if !row.children.is_empty() {
                let c = r.right_center() - Vec2::new(14.0, 0.0);
                if opens_left(
                    self.rect,
                    ui.ctx().screen_rect().shrink(4.0),
                    child_width(&row.children),
                ) {
                    ui.painter().add(egui::Shape::line(
                        vec![
                            c + Vec2::new(1.5, -2.5),
                            c - Vec2::new(1.5, 0.0),
                            c + Vec2::new(1.5, 2.5),
                        ],
                        Stroke::new(1.0, secondary),
                    ));
                } else {
                    ui_kit::caret_right(ui, c, secondary);
                }
                label.max.x -= 14.0;
            } else if checked {
                let c = r.right_center() - Vec2::new(15.0, 0.0);
                ui.painter().add(egui::Shape::line(
                    vec![
                        c + Vec2::new(-4.0, 0.0),
                        c + Vec2::new(-1.0, 3.0),
                        c + Vec2::new(4.0, -3.0),
                    ],
                    Stroke::new(1.5, pal.accent),
                ));
                label.max.x -= 15.0;
            }
            if !row.shortcut.is_empty() {
                let hint = ui.painter().text(
                    label.right_center(),
                    Align2::RIGHT_CENTER,
                    row.shortcut,
                    FontId::proportional(11.0),
                    secondary,
                );
                label.max.x = hint.left() - 8.0;
            }
            paint_fitted_label(
                ui.painter(),
                label,
                row.label,
                FontId::proportional(12.0),
                fg,
            );
            if ui_kit::is_accesskit_active(ui.ctx()) {
                let _ = ui.ctx().accesskit_node_builder(resp.id, |node| {
                    node.set_role(egui::accesskit::Role::MenuItem);
                    node.set_label(row.label);
                    if !row.shortcut.is_empty() {
                        node.set_keyboard_shortcut(row.shortcut);
                    }
                    if !row.enabled {
                        node.set_disabled();
                    }
                    if !row.children.is_empty() {
                        node.set_has_popup(egui::accesskit::HasPopup::Menu);
                    }
                    if color_item {
                        node.set_selected(checked);
                    }
                });
            }
        }
    }
}

impl SheetApp {
    pub(super) fn ctx_menu_window(&mut self, ctx: &egui::Context) {
        self.ctx_rects.clear();
        let Some(menu) = self.ctx_menu.as_ref() else {
            return;
        };
        let (kind, idx, opened, anchor) = (menu.kind, menu.idx, menu.frame, menu.pos);
        let (mut key, mut child, mut sub_key) = (menu.key, menu.sub, menu.sub_key);
        let rows = match kind {
            CtxKind::Cell => self.cell_ctx_menu_rows(),
            CtxKind::Rows => self.head_ctx_menu_rows(false),
            CtxKind::Cols => self.head_ctx_menu_rows(true),
            CtxKind::Tab => self.tab_ctx_menu_rows(idx),
        };
        child = child.filter(|&i| {
            rows.get(i)
                .is_some_and(|row| row.enabled && !row.children.is_empty())
        });
        if !ctx_row_focusable(&rows, key) {
            key = next_ctx_row(&rows, rows.len().saturating_sub(1), 1).unwrap_or(0);
        }
        let pressed = ctx.input_mut(|i| {
            [
                Key::Escape,
                Key::ArrowLeft,
                Key::ArrowRight,
                Key::ArrowUp,
                Key::ArrowDown,
                Key::Home,
                Key::End,
                Key::Enter,
            ]
            .into_iter()
            .find(|k| i.consume_key(egui::Modifiers::NONE, *k))
        });
        let mut action = None;
        let reveal_child = pressed.is_some();
        let reveal_parent = child.is_none() && pressed.is_some();
        match pressed {
            Some(Key::Escape) if child.is_none() => {
                self.ctx_menu = None;
                ctx.request_repaint();
                return;
            }
            Some(Key::Escape | Key::ArrowLeft) => child = None,
            Some(k @ (Key::ArrowUp | Key::ArrowDown | Key::Home | Key::End)) => {
                let (items, selected) = if let Some(i) = child {
                    (&rows[i].children[..], &mut sub_key)
                } else {
                    (&rows[..], &mut key)
                };
                let (from, dir) = match k {
                    Key::Home => (items.len().saturating_sub(1), 1),
                    Key::End => (0, -1),
                    Key::ArrowUp => (*selected, -1),
                    _ => (*selected, 1),
                };
                if let Some(next) = next_ctx_row(items, from, dir) {
                    *selected = next;
                }
            }
            Some(Key::ArrowRight | Key::Enter) => {
                if let Some(i) = child {
                    if pressed == Some(Key::Enter) {
                        action = rows[i]
                            .children
                            .get(sub_key)
                            .filter(|r| r.enabled)
                            .and_then(|r| r.act);
                    }
                } else if let Some(row) = rows.get(key).filter(|r| r.enabled) {
                    if !row.children.is_empty() {
                        child = Some(key);
                        sub_key = 0;
                    } else if pressed == Some(Key::Enter) {
                        action = row.act;
                    }
                }
            }
            _ => {}
        }
        let bounds = ctx.screen_rect().shrink(4.0);
        let parent = bounded_panel(anchor, Vec2::new(CTX_W, ctx_menu_height(&rows)), bounds);
        let id = egui::Id::new("worksheet_context_menu");
        let tab_color = if kind == CtxKind::Tab {
            self.wb.sheets.get(idx).and_then(|s| s.tab_color)
        } else {
            None
        };
        let root = MenuPanel {
            id,
            rect: parent,
            opened,
            rows: &rows,
            selected: child.unwrap_or(key),
            reveal: reveal_parent,
            child: false,
            tab_color,
        }
        .show(ctx);
        self.ctx_rects.push(parent);
        if let Some(i) = root.clicked.or(root.pointed.filter(|_| pressed.is_none())) {
            key = i;
            let next = (rows[i].enabled && !rows[i].children.is_empty()).then_some(i);
            if next != child {
                child = next;
                sub_key = 0;
            }
            if root.clicked.is_some() {
                action = rows[i].act;
            }
        }
        if root.paste.is_some() {
            action = root.paste;
        }
        if let Some(i) = child {
            let row_rect = root.rows[i];
            if row_rect.intersects(parent.shrink2(Vec2::new(0.0, 4.0))) {
                let kids = &rows[i].children;
                let width = child_width(kids);
                let x = if opens_left(parent, bounds, width) {
                    parent.left() - width
                } else {
                    parent.right()
                };
                let panel = bounded_panel(
                    egui::pos2(x, row_rect.top() - 4.0),
                    Vec2::new(width, ctx_menu_height(kids)),
                    bounds,
                );
                let result = MenuPanel {
                    id: id.with(("children", i)),
                    rect: panel,
                    opened,
                    rows: kids,
                    selected: sub_key,
                    reveal: reveal_child,
                    child: true,
                    tab_color,
                }
                .show(ctx);
                self.ctx_rects.push(panel);
                if let Some(k) = result.pointed.filter(|_| pressed.is_none()) {
                    sub_key = k;
                }
                if let Some(k) = result.clicked {
                    action = kids[k].act;
                }
            } else if !reveal_parent {
                child = None;
            }
        }
        if kind == CtxKind::Cell {
            self.draw_mini_toolbar(ctx, parent);
        }
        let outside = ctx.input(|i| {
            i.pointer.any_click()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !self.ctx_rects.iter().any(|r| r.contains(p)))
        });
        if let Some(a) = action {
            self.ctx_menu = None;
            self.ctx_menu_run(a, idx, ctx);
            ctx.request_repaint();
        } else if outside && ctx.cumulative_pass_nr() != opened {
            self.ctx_menu = None;
            ctx.request_repaint();
        } else if let Some(menu) = &mut self.ctx_menu {
            if (menu.key, menu.sub, menu.sub_key) != (key, child, sub_key) {
                (menu.key, menu.sub, menu.sub_key) = (key, child, sub_key);
                ctx.request_repaint();
            }
        }
    }
}
