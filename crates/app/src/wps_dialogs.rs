//! WPS 风格增强弹窗与浮动交互件：
//! 1. 右键搭配的 246×60 Mini 格式工具栏（含字体/字号/颜色/对齐/合并/千分位/百分比）
//! 2. 填充后浮动「自动填充选项 ▾」与粘贴后浮动「粘贴选项 ▾」即时切换器
//! 3. WPS 升级版 560px `Ctrl+1` 设置单元格格式对话框（两栏数字格式、DBNum1/2/RMB 大写、跨列/分散对齐、缩小字体填充、线型边框预览）
//! 4. WPS 升级版 ~920×660 左右实时分栏 `Ctrl+P` 打印预览与设置面板

use eframe::egui::{self, Align2, Color32, FontId, Key, Rect, Sense, Stroke, Vec2};

use crate::fill_seq::FillMode;
use crate::ui_icons::Icon;
use crate::{ui_kit, wps_chrome, SheetApp};

const MINI_SIZES: [f32; 15] = [8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 28.0, 36.0, 48.0, 72.0];

/// 混合选区首次点击统一勾选；没有交互时，保留活动格草稿和各单元格原值。
fn format_checkbox(ui: &mut egui::Ui, label: &str, value: &mut bool, mixed: bool, touched: &mut bool) {
    let indeterminate = mixed && !*touched;
    let mut displayed = *value && !indeterminate;
    if ui.add(egui::Checkbox::new(&mut displayed, label).indeterminate(indeterminate)).changed() {
        *value = displayed;
        *touched = true;
    }
}

#[cfg(test)]
mod checkbox_tests {
    use super::*;

    #[test]
    fn mixed_checkbox_preserves_the_draft_until_the_first_click_selects_all() {
        for original in [false, true] {
            let ctx = egui::Context::default();
            let mut value = original;
            let mut touched = false;
            for frame in 0..3 {
                let mut input = egui::RawInput::default();
                if frame == 2 {
                    input.events.push(egui::Event::Key { key: Key::Space, physical_key: None,
                        pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
                }
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let id = ui.next_auto_id();
                        format_checkbox(ui, "锁定", &mut value, true, &mut touched);
                        if frame == 0 { ui.memory_mut(|m| m.request_focus(id)); }
                    });
                });
                assert_eq!(touched, frame == 2);
                assert_eq!(value, original || frame == 2);
            }
        }
    }
}

fn category_code_sample(cat: u8, dec: u8, sep: bool, neg: u8, sym: u8, cur_fmt: &str) -> String {
    let zeros = if dec == 0 {
        String::new()
    } else {
        format!(".{}", "0".repeat(dec as usize))
    };
    let int_part = if sep { "#,##0" } else { "0" };
    let base = format!("{int_part}{zeros}");
    let cur_sym = match sym {
        1 => "$",
        2 => "€",
        _ => "¥",
    };
    match cat {
        0 => String::new(),
        1 => match neg {
            1 => format!("{base};[Red]-{base}"),
            2 => format!("{base};({base})"),
            _ => base,
        },
        2 => match neg {
            1 => format!("{cur_sym}{base};[Red]-{cur_sym}{base}"),
            2 => format!("{cur_sym}{base};({cur_sym}{base})"),
            _ => format!("{cur_sym}{base}"),
        },
        3 => format!("_-* {cur_sym}{base}_-;-* {cur_sym}{base}_-;_-* \"-\"??_-;_-@_-"),
        4 => {
            if matches!(cur_fmt, "yyyy-mm-dd" | "yyyy/m/d" | "yyyy\"年\"m\"月\"d\"日\"" | "mm-dd" | "yyyy-mm") {
                cur_fmt.to_string()
            } else {
                "yyyy-mm-dd".to_string()
            }
        }
        5 => {
            if matches!(cur_fmt, "hh:mm" | "hh:mm:ss" | "h\"时\"mm\"分\"") {
                cur_fmt.to_string()
            } else {
                "hh:mm:ss".to_string()
            }
        }
        6 => format!("0{zeros}%"),
        7 => {
            if cur_fmt == "# ??/??" { "# ??/??".into() } else { "# ?/?".into() }
        }
        8 => format!("0{zeros}E+00"),
        9 => "@".to_string(),
        10 => {
            let l = cur_fmt.to_ascii_lowercase();
            if l.contains("[dbnum1]") {
                "[DBNum1]G/通用格式".to_string()
            } else if l.contains("[dbnum2]") && !(cur_fmt.contains('¥') || cur_fmt.contains('￥')) {
                "[DBNum2]G/通用格式".to_string()
            } else {
                "[DBNum2][$¥-804]#,##0.00".to_string()
            }
        }
        _ => cur_fmt.to_string(),
    }
}

impl SheetApp {
    /// WPS 268×64 浮动 Mini 格式工具栏（搭配单元格右键菜单同屏出现）
    pub(crate) fn draw_mini_toolbar(&mut self, ctx: &egui::Context, menu_rect: Rect) {
        let screen = ctx.screen_rect();
        let w = 268.0f32;
        let h = 64.0f32;
        let size = Vec2::new(w, h);
        let bounds = screen.shrink(6.0);
        if bounds.width() < w || bounds.height() < h {
            return;
        }
        let x = menu_rect.left().clamp(bounds.left(), bounds.right() - w);
        let y = menu_rect.top().clamp(bounds.top(), bounds.bottom() - h);
        let right = menu_rect.right() + 8.0;
        let left = menu_rect.left() - w - 8.0;
        let positions = [
            egui::pos2(x, menu_rect.top() - h - 8.0),
            egui::pos2(x, menu_rect.bottom() + 8.0),
            egui::pos2(right, y),
            egui::pos2(left, y),
            egui::pos2(right, bounds.bottom() - h),
            egui::pos2(left, bounds.bottom() - h),
        ];
        let Some(mini_rect) = positions.into_iter().map(|p| Rect::from_min_size(p, size)).find(|r| {
            bounds.contains_rect(*r) && !r.intersects(menu_rect)
                && !self.ctx_rects.iter().any(|visible| r.intersects(*visible))
        }) else {
            return;
        };
        self.ctx_rects.push(mini_rect);

        let cur = self.current_style();
        let session = self.ctx_menu.as_ref().map(|m| m.frame);
        let font_pop_id = egui::Id::new("wps_mini_font_pop");
        let size_pop_id = font_pop_id.with("choices");
        let mut pop_mode = ctx.data_mut(|d| d.get_temp::<(Option<u64>, u8)>(font_pop_id))
            .filter(|(opened, _)| *opened == session).map_or(0, |(_, mode)| mode);

        let mut act: Option<String> = None;
        let mut set_font: Option<String> = None;
        let mut set_size: Option<f32> = None;
        let mut open_color: Option<(bool, egui::Pos2)> = None;
        let mut apply_fill_direct = false;
        let mut apply_font_direct = false;

        let pal = crate::theme::palette();
        egui::Area::new(egui::Id::new("wps_mini_toolbar"))
            .order(egui::Order::Foreground)
            .fixed_pos(mini_rect.min)
            .show(ctx, |ui| {
                ui_kit::popup_frame(ui.style())
                    .inner_margin(egui::Margin::symmetric(6, 5))
                    .show(ui, |ui| {
                        ui.set_min_size(Vec2::new(w - 14.0, h - 12.0));
                        ui.spacing_mut().item_spacing = Vec2::new(2.0, 4.0);

                        // Row 1: 字体(74) + 字号(40) + A+(21) + A-(21) + B(22) + I(22) + U(22) = 222 + 12 = 234
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let flabel = if cur.font.is_empty() { "默认" } else { cur.font.as_str() };
                            let (fr, fresp) = ui.allocate_exact_size(Vec2::new(74.0, 22.0), Sense::click());
                            ui.painter().rect(
                                fr,
                                4.0,
                                if fresp.hovered() || pop_mode == 1 { pal.hover } else { pal.grid_bg },
                                Stroke::new(1.0, pal.card_border),
                                egui::StrokeKind::Inside,
                            );
                            let font_label = Rect::from_min_max(
                                fr.left_top() + egui::vec2(5.0, 0.0),
                                fr.right_bottom() - egui::vec2(15.0, 0.0),
                            );
                            ui.painter_at(font_label).text(
                                egui::pos2(fr.left() + 5.0, fr.center().y),
                                Align2::LEFT_CENTER,
                                flabel,
                                FontId::proportional(11.0),
                                pal.text,
                            );
                            ui_kit::caret(ui, egui::pos2(fr.right() - 7.0, fr.center().y), pal.text_muted);
                            if fresp.on_hover_text("字体").clicked() {
                                pop_mode = if pop_mode == 1 { 0 } else { 1 };
                            }

                            let (sr, sresp) = ui.allocate_exact_size(Vec2::new(40.0, 22.0), Sense::click());
                            ui.painter().rect(
                                sr,
                                4.0,
                                if sresp.hovered() || pop_mode == 2 { pal.hover } else { pal.grid_bg },
                                Stroke::new(1.0, pal.card_border),
                                egui::StrokeKind::Inside,
                            );
                            ui.painter().text(
                                egui::pos2(sr.left() + 5.0, sr.center().y),
                                Align2::LEFT_CENTER,
                                format!("{}", cur.size as i32),
                                FontId::proportional(11.0),
                                pal.text,
                            );
                            ui_kit::caret(ui, egui::pos2(sr.right() - 6.0, sr.center().y), pal.text_muted);
                            if sresp.on_hover_text("字号").clicked() {
                                pop_mode = if pop_mode == 2 { 0 } else { 2 };
                            }

                            if ui_kit::small_on(ui, Icon::FontGrow, false, "增大字号").clicked() {
                                act = Some("GrowFont".into());
                            }
                            if ui_kit::small_on(ui, Icon::FontShrink, false, "减小字号").clicked() {
                                act = Some("ShrinkFont".into());
                            }
                            if ui_kit::text_toggle(ui, "B", cur.bold, "加粗", true, false, false).clicked() {
                                act = Some("Bold".into());
                            }
                            if ui_kit::text_toggle(ui, "I", cur.italic, "倾斜", false, true, false).clicked() {
                                act = Some("Italic".into());
                            }
                            if ui_kit::text_toggle(ui, "U", cur.underline, "下划线", false, false, true).clicked() {
                                act = Some("Underline".into());
                            }
                        });

                        // Row 2: Fill split(33) + FontColor split(33) + Border(22) + Align L/C/R(66) + Merge(22) + %(22) + ,(22) = 220 + gaps
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 1.5;
                            // Fill color split
                            let fill_c = Color32::from_rgb(self.last_fill_color[0], self.last_fill_color[1], self.last_fill_color[2]);
                            let fc_r = ui_kit::color_button(ui, Icon::Fill, fill_c, "填充颜色");
                            if fc_r.clicked() {
                                apply_fill_direct = true;
                            }
                            let fa_r = ui_kit::arrow_glyph_button(ui, Vec2::new(11.0, 22.0), pal.text_weak);
                            if fa_r.clicked() {
                                open_color = Some((false, fa_r.rect.left_bottom()));
                            }

                            // Font color split
                            let font_c = Color32::from_rgb(self.last_font_color[0], self.last_font_color[1], self.last_font_color[2]);
                            let fcr = ui_kit::color_button(ui, Icon::FontColor, font_c, "字体颜色");
                            if fcr.clicked() {
                                apply_font_direct = true;
                            }
                            let far = ui_kit::arrow_glyph_button(ui, Vec2::new(11.0, 22.0), pal.text_weak);
                            if far.clicked() {
                                open_color = Some((true, far.rect.left_bottom()));
                            }

                            if ui_kit::small_on(ui, Icon::Border, false, "应用边框").clicked() {
                                act = Some("BordersDefault".into());
                            }
                            if ui_kit::small_on(ui, Icon::AlignL, cur.halign == 1, "左对齐").clicked() {
                                act = Some("AlignLeft".into());
                            }
                            if ui_kit::small_on(ui, Icon::AlignC, cur.halign == 2, "居中").clicked() {
                                act = Some("AlignCenter".into());
                            }
                            if ui_kit::small_on(ui, Icon::AlignR, cur.halign == 3, "右对齐").clicked() {
                                act = Some("AlignRight".into());
                            }
                            let merge_checked = self.ctl_state("MergeCenter").checked;
                            if ui_kit::small_on(ui, Icon::Merge, merge_checked, "合并居中").clicked() {
                                act = Some("MergeCenter".into());
                            }
                            if ui_kit::small_on(ui, Icon::Percent, cur.format.contains('%'), "百分比").clicked() {
                                act = Some("PercentStyle".into());
                            }
                            if ui_kit::small_on(ui, Icon::Comma, cur.format.contains("#,##0"), "千分位").clicked() {
                                act = Some("CommaStyle".into());
                            }
                        });
                    });
            });

        // 内联字体/字号下拉列表（同屏保护：框体注册进 self.ctx_rects，防止点选被当成外部关闭）
        if pop_mode == 1 || pop_mode == 2 {
            let pop_w = if pop_mode == 1 { 132.0 } else { 68.0 };
            let pop_pos = egui::pos2(
                if pop_mode == 1 { mini_rect.left() + 6.0 } else { mini_rect.left() + 82.0 },
                (mini_rect.top() + 30.0).min((screen.bottom() - 200.0).max(screen.top() + 4.0)),
            );
            let pr = egui::Area::new(size_pop_id)
                .order(egui::Order::Foreground)
                .fixed_pos(pop_pos)
                .show(ctx, |ui| {
                    ui_kit::popup_frame(ui.style())
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            ui.set_width(pop_w);
                            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                                if pop_mode == 1 {
                                    for name in sheetcore::style::FONT_CHOICES {
                                        let lbl = if name.is_empty() { "默认" } else { *name };
                                        if ui_kit::popup_item(ui, lbl, true, pop_w - 8.0).clicked() {
                                            set_font = Some((*name).to_string());
                                            pop_mode = 0;
                                        }
                                    }
                                } else {
                                    for sz in MINI_SIZES {
                                        let lbl = format!("{}", sz as i32);
                                        if ui_kit::popup_item(ui, &lbl, true, pop_w - 8.0).clicked() {
                                            set_size = Some(sz);
                                            pop_mode = 0;
                                        }
                                    }
                                }
                            });
                        });
                });
            self.ctx_rects.push(pr.response.rect);
        }

        ctx.data_mut(|d| d.insert_temp(font_pop_id, (session, pop_mode)));

        if let Some(a) = act {
            self.ribbon_action(&a, ctx);
        }
        if let Some(f) = set_font {
            self.apply_style(move |s| s.font = f.clone());
        }
        if let Some(sz) = set_size {
            self.apply_style(move |s| s.size = sz);
        }
        if apply_fill_direct {
            self.apply_fill_color(Some(self.last_fill_color));
        }
        if apply_font_direct {
            self.apply_font_color(self.last_font_color);
        }
        if let Some((for_font, p)) = open_color {
            self.color_popup_pos = Some(p);
            self.color_popup = Some(for_font);
        }
    }

    /// 浮动「自动填充选项 ▾」切换器
    pub(crate) fn draw_fill_options_badge(&mut self, ctx: &egui::Context, anchor: egui::Pos2) {
        let Some((cur_mode, is_open)) = self
            .last_fill
            .as_ref()
            .filter(|lf| lf.sheet_idx == self.sheet_idx)
            .map(|lf| (lf.mode, lf.popup_open))
        else {
            return;
        };
        let screen = ctx.screen_rect();
        let btn_pos = egui::pos2(
            (anchor.x + 4.0).clamp(screen.left() + 6.0, (screen.right() - 82.0).max(6.0)),
            (anchor.y + 4.0).clamp(screen.top() + 40.0, (screen.bottom() - 62.0).max(40.0)),
        );
        let mut toggle_pop = false;
        let mut pick_mode: Option<FillMode> = None;
        let pal = crate::theme::palette();

        egui::Area::new(egui::Id::new("wps_fill_badge"))
            .order(egui::Order::Foreground)
            .fixed_pos(btn_pos)
            .show(ctx, |ui| {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(72.0, 22.0), Sense::click());
                ui.painter().rect(
                    r,
                    5.0,
                    if resp.hovered() || is_open { pal.card_bg } else { pal.page_bg },
                    Stroke::new(1.0, if is_open { pal.accent } else { pal.card_border }),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    egui::pos2(r.left() + 8.0, r.center().y),
                    Align2::LEFT_CENTER,
                    "填充选项",
                    FontId::proportional(11.0),
                    pal.text,
                );
                ui_kit::caret(ui, egui::pos2(r.right() - 9.0, r.center().y), pal.text_weak);
                if resp.on_hover_text("切换填充方式（复制 / 填充序列 / 仅格式 / 不带格式）").clicked() {
                    toggle_pop = true;
                }
            });

        if is_open {
            let pop_pos = egui::pos2(
                btn_pos.x.min((screen.right() - 156.0).max(6.0)),
                (btn_pos.y + 24.0).min((screen.bottom() - 150.0).max(40.0)),
            );
            egui::Area::new(egui::Id::new("wps_fill_badge_pop"))
                .order(egui::Order::Foreground)
                .fixed_pos(pop_pos)
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(pal.card_bg)
                        .stroke(Stroke::new(1.0, pal.card_border))
                        .corner_radius(6.0)
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 4],
                            blur: 12,
                            spread: 0,
                            color: Color32::from_black_alpha(26),
                        })
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| {
                            ui.set_width(148.0);
                            for (lbl, mode) in [
                                ("智能填充（默认）", FillMode::Smart),
                                ("复制单元格", FillMode::CopyCells),
                                ("填充序列", FillMode::FillSeries),
                                ("仅填充格式", FillMode::FormatsOnly),
                                ("不带格式填充", FillMode::ValuesOnly),
                                ("按工作日填充", FillMode::Weekdays),
                                ("按月填充", FillMode::Months),
                                ("按年填充", FillMode::Years),
                                ("线性趋势", FillMode::LinearTrend),
                                ("增长趋势", FillMode::GrowthTrend),
                            ] {
                                let mark = if cur_mode == mode { format!("√ {lbl}") } else { format!("   {lbl}") };
                                if ui_kit::popup_item(ui, &mark, true, 146.0).clicked() {
                                    pick_mode = Some(mode);
                                }
                            }
                        });
                });
        }

        if let Some(m) = pick_mode {
            self.reapply_last_fill(m);
        } else if toggle_pop {
            if let Some(lf) = self.last_fill.as_mut() {
                lf.popup_open = !lf.popup_open;
            }
        }
    }

    /// 浮动「粘贴选项 ▾」即时切换器
    pub(crate) fn draw_paste_options_badge(&mut self, ctx: &egui::Context, anchor: egui::Pos2) {
        let Some((cur_mode, is_open)) = self
            .last_paste
            .as_ref()
            .filter(|lp| lp.sheet_idx == self.sheet_idx)
            .map(|lp| (lp.mode, lp.popup_open))
        else {
            return;
        };
        let screen = ctx.screen_rect();
        let btn_pos = egui::pos2(
            (anchor.x + 4.0).clamp(screen.left() + 6.0, (screen.right() - 82.0).max(6.0)),
            (anchor.y + 4.0).clamp(screen.top() + 40.0, (screen.bottom() - 62.0).max(40.0)),
        );
        let mut toggle_pop = false;
        let mut pick_mode: Option<u8> = None;
        let pal = crate::theme::palette();

        egui::Area::new(egui::Id::new("wps_paste_badge"))
            .order(egui::Order::Foreground)
            .fixed_pos(btn_pos)
            .show(ctx, |ui| {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(72.0, 22.0), Sense::click());
                ui.painter().rect(
                    r,
                    5.0,
                    if resp.hovered() || is_open { pal.card_bg } else { pal.page_bg },
                    Stroke::new(1.0, if is_open { pal.accent } else { pal.card_border }),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    egui::pos2(r.left() + 8.0, r.center().y),
                    Align2::LEFT_CENTER,
                    "粘贴选项",
                    FontId::proportional(11.0),
                    pal.text,
                );
                ui_kit::caret(ui, egui::pos2(r.right() - 9.0, r.center().y), pal.text_weak);
                if resp.on_hover_text("切换粘贴方式（保留格式 / 值 / 数字格式 / 公式 / 格式 / 无边框 / 转置）").clicked() {
                    toggle_pop = true;
                }
            });

        if is_open {
            let pop_pos = egui::pos2(
                btn_pos.x.min((screen.right() - 170.0).max(6.0)),
                (btn_pos.y + 24.0).min((screen.bottom() - 250.0).max(40.0)),
            );
            egui::Area::new(egui::Id::new("wps_paste_badge_pop"))
                .order(egui::Order::Foreground)
                .fixed_pos(pop_pos)
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(pal.card_bg)
                        .stroke(Stroke::new(1.0, pal.card_border))
                        .corner_radius(6.0)
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 4],
                            blur: 12,
                            spread: 0,
                            color: Color32::from_black_alpha(26),
                        })
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| {
                            ui.set_width(152.0);
                            for (lbl, mode) in [
                                ("保留源格式", 0u8),
                                ("仅粘贴数值 (123)", 1),
                                ("值和数字格式", 5),
                                ("值和源格式", 6),
                                ("仅粘贴公式 (fx)", 2),
                                ("仅粘贴格式", 3),
                                ("无边框粘贴", 7),
                                ("转置 (行列互换)", 4),
                            ] {
                                let mark = if cur_mode == mode { format!("√ {lbl}") } else { format!("   {lbl}") };
                                if ui_kit::popup_item(ui, &mark, true, 148.0).clicked() {
                                    pick_mode = Some(mode);
                                }
                            }
                        });
                });
        }

        if let Some(m) = pick_mode {
            self.reapply_last_paste(m);
        } else if toggle_pop {
            if let Some(lp) = self.last_paste.as_mut() {
                lp.popup_open = !lp.popup_open;
            }
        }
    }

    /// WPS 升级版 560px「设置单元格格式」(`Ctrl+1`)
    pub(crate) fn draw_format_dialog_wps(&mut self, ctx: &egui::Context) {
        if !self.fmt_dlg {
            return;
        }
        let mut open = self.fmt_dlg;
        let mut apply = false;
        let mut cancel = false;
        let pal = crate::theme::palette();
        let mixed = self.fmt_mixed.clone();
        let orig = self.fmt_orig.clone();
        let font_mixed = mixed.font && !self.fmt_touched.font && self.fmt_state.font == orig.font;
        let size_mixed = mixed.size && !self.fmt_touched.size && (self.fmt_state.size - orig.size).abs() <= f32::EPSILON;
        let color_mixed = mixed.color && !self.fmt_touched.color && self.fmt_state.color == orig.color;
        let fill_mixed = mixed.fill && !self.fmt_touched.fill && self.fmt_state.fill == orig.fill;
        let halign_mixed = mixed.halign && !self.fmt_touched.halign && self.fmt_state.halign == orig.halign;
        let valign_mixed = mixed.valign && !self.fmt_touched.valign && self.fmt_state.valign == orig.valign;
        let wrap_mixed = mixed.wrap && !self.fmt_touched.wrap && self.fmt_state.wrap == orig.wrap;
        let shrink_mixed = mixed.shrink_to_fit && !self.fmt_touched.shrink_to_fit && self.fmt_state.shrink_to_fit == orig.shrink_to_fit;
        let format_mixed = mixed.format && !self.fmt_touched.format && self.fmt_state.format == orig.format;
        let locked_mixed = mixed.locked && !self.fmt_touched.locked && self.fmt_state.locked == orig.locked;
        let hidden_mixed = mixed.hidden && !self.fmt_touched.hidden && self.fmt_state.hidden == orig.hidden;

        ui_kit::window(ctx, "设置单元格格式", 560.0)
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -16.0])
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                ui_kit::dialog_tabs(ui, &mut self.fmt_tab, &["数字", "对齐", "字体", "边框", "填充", "保护"]);
                ui.add_space(4.0);

                match self.fmt_tab {
                    0 => {
                        // WPS 两栏数字设置
                        ui.horizontal_top(|ui| {
                            // 左栏：12 个分类列表
                            egui::Frame::NONE
                                .fill(pal.page_bg)
                                .stroke(Stroke::new(1.0, pal.card_border))
                                .corner_radius(6.0)
                                .inner_margin(egui::Margin::same(6))
                                .show(ui, |ui| {
                                    let layout = egui::Layout::top_down_justified(egui::Align::Min);
                                    ui.allocate_ui_with_layout(Vec2::new(120.0, 0.0), layout, |ui| {
                                        ui.set_min_height(248.0);
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        ui.visuals_mut().selection.bg_fill = pal.accent_tint;
                                        ui.visuals_mut().selection.stroke.color = pal.text;
                                        let cats = [
                                            "常规", "数值", "货币", "会计专用", "日期", "时间",
                                            "百分比", "分数", "科学记数", "文本", "特殊", "自定义",
                                        ];
                                        for (idx, name) in cats.iter().enumerate() {
                                            let sel = !format_mixed && self.fmt_num_cat == idx as u8;
                                            let r = ui.selectable_label(sel, *name);
                                            if sel {
                                                let edge = r.rect.shrink2(Vec2::new(1.0, 4.0));
                                                ui.painter().line_segment([edge.left_top(), edge.left_bottom()], Stroke::new(2.0, pal.accent));
                                            }
                                            if r.clicked() {
                                                self.fmt_num_cat = idx as u8;
                                                let next_fmt = category_code_sample(
                                                    self.fmt_num_cat,
                                                    self.fmt_num_dec,
                                                    self.fmt_num_sep,
                                                    self.fmt_num_neg,
                                                    self.fmt_num_sym,
                                                    &self.fmt_state.format,
                                                );
                                                self.fmt_state.format = next_fmt;
                                                self.fmt_touched.format = true;
                                            }
                                        }
                                    });
                                });

                            // 右栏：动态选项 + 示例
                            ui.vertical(|ui| {
                                ui.set_width(ui.available_width());
                                let sample_box = |ui: &mut egui::Ui, preview: &str| {
                                    egui::Frame::NONE
                                        .fill(pal.header_bg)
                                        .stroke(Stroke::new(1.0, pal.card_border))
                                        .corner_radius(6.0)
                                        .inner_margin(egui::Margin::symmetric(10, 8))
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.label(egui::RichText::new("示例").size(ui_kit::T_SMALL).color(pal.text_weak));
                                            ui.add(egui::Label::new(egui::RichText::new(preview).size(ui_kit::T_H2).color(pal.text)).truncate())
                                                .on_hover_text(preview);
                                        });
                                };
                                let preview = if format_mixed {
                                    "（选区包含混合数字格式）".to_string()
                                } else {
                                    self.format_dialog_preview()
                                };
                                sample_box(ui, &preview);
                                ui.add_space(4.0);

                                let mut regen = false;
                                match self.fmt_num_cat {
                                    0 => {
                                        ui.label(egui::RichText::new("常规单元格格式不包含任何特定的数字格式。").size(ui_kit::T_BODY).color(pal.text_weak));
                                    }
                                    1 | 2 | 3 | 6 | 8 => {
                                        ui.horizontal(|ui| {
                                            ui.label("小数位数：");
                                            let mut dec = self.fmt_num_dec as f64;
                                            if ui_kit::spin(ui, &mut dec, 0.0, 10.0, 1.0, 62.0) {
                                                self.fmt_num_dec = dec as u8;
                                                regen = true;
                                            }
                                            if self.fmt_num_cat == 1 && ui.checkbox(&mut self.fmt_num_sep, "使用千分位分隔符 (,)").changed() {
                                                regen = true;
                                            }
                                        });
                                        if self.fmt_num_cat == 2 || self.fmt_num_cat == 3 {
                                            ui.horizontal(|ui| {
                                                ui.label("货币符号：");
                                                for (idx, lbl) in [(0u8, "¥ 人民币"), (1, "$ 美元"), (2, "€ 欧元")] {
                                                    if ui.selectable_value(&mut self.fmt_num_sym, idx, lbl).clicked() {
                                                        regen = true;
                                                    }
                                                }
                                            });
                                        }
                                        if self.fmt_num_cat == 1 || self.fmt_num_cat == 2 {
                                            ui.label(egui::RichText::new("负数显示方式：").size(ui_kit::T_BODY).color(pal.text_weak));
                                            for (idx, lbl) in [(0u8, "-1,234.10"), (1, "红色 -1,234.10"), (2, "(1,234.10)")] {
                                                if ui.selectable_value(&mut self.fmt_num_neg, idx, lbl).clicked() {
                                                    regen = true;
                                                }
                                            }
                                        }
                                    }
                                    4 => {
                                        for (lbl, code) in [
                                            ("2026-03-14", "yyyy-mm-dd"),
                                            ("2026/3/14", "yyyy/m/d"),
                                            ("2026年3月14日", "yyyy\"年\"m\"月\"d\"日\""),
                                            ("03-14", "mm-dd"),
                                            ("2026-03", "yyyy-mm"),
                                        ] {
                                            if ui.selectable_label(self.fmt_state.format == code, lbl).clicked() {
                                                self.fmt_state.format = code.to_string();
                                                self.fmt_touched.format = true;
                                            }
                                        }
                                    }
                                    5 => {
                                        for (lbl, code) in [
                                            ("13:30", "hh:mm"),
                                            ("13:30:55", "hh:mm:ss"),
                                            ("13时30分", "h\"时\"mm\"分\""),
                                        ] {
                                            if ui.selectable_label(self.fmt_state.format == code, lbl).clicked() {
                                                self.fmt_state.format = code.to_string();
                                                self.fmt_touched.format = true;
                                            }
                                        }
                                    }
                                    7 => {
                                        for (lbl, code) in [("分母为一位数 (1/4)", "# ?/?"), ("分母为两位数 (21/25)", "# ??/??")] {
                                            if ui.selectable_label(self.fmt_state.format == code, lbl).clicked() {
                                                self.fmt_state.format = code.to_string();
                                                self.fmt_touched.format = true;
                                            }
                                        }
                                    }
                                    9 => {
                                        ui.label(egui::RichText::new("在文本单元格格式中，数字作为文本处理（@）。").size(ui_kit::T_BODY).color(pal.text_weak));
                                    }
                                    10 => {
                                        ui.label(egui::RichText::new("中文特色财务与数字格式：").size(ui_kit::T_BODY).color(pal.text_weak));
                                        for (lbl, code) in [
                                            ("人民币大写（壹万贰仟叁佰元整）", "[DBNum2][$¥-804]#,##0.00"),
                                            ("中文大写数字（壹万贰仟叁佰）", "[DBNum2]G/通用格式"),
                                            ("中文小写数字（一万二千三百）", "[DBNum1]G/通用格式"),
                                        ] {
                                            if ui.selectable_label(self.fmt_state.format == code, lbl).clicked() {
                                                self.fmt_state.format = code.to_string();
                                                self.fmt_touched.format = true;
                                            }
                                        }
                                    }
                                    _ => {
                                        ui.label(egui::RichText::new("自定义格式代码：").size(ui_kit::T_BODY).color(pal.text_weak));
                                        let mut custom = self.fmt_state.format.clone();
                                        if ui.add(egui::TextEdit::singleline(&mut custom).desired_width(ui.available_width()).hint_text("例如 #,##0.00;[Red]-#,##0.00")).changed() {
                                            self.fmt_state.format = custom;
                                            self.fmt_touched.format = true;
                                        }
                                    }
                                }
                                if regen {
                                    self.fmt_state.format = category_code_sample(
                                        self.fmt_num_cat,
                                        self.fmt_num_dec,
                                        self.fmt_num_sep,
                                        self.fmt_num_neg,
                                        self.fmt_num_sym,
                                        &self.fmt_state.format,
                                    );
                                    self.fmt_touched.format = true;
                                }
                            });
                        });
                    }
                    1 => {
                        crate::dlg_section(ui, "文本对齐方式");
                        egui::Grid::new("wps_fmt_align").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                            crate::dlg_cell_label(ui, if halign_mixed { "水平对齐（混合）" } else { "水平对齐" });
                            ui.horizontal_wrapped(|ui| {
                                for (lbl, v) in [("常规", 0u8), ("靠左", 1), ("居中", 2), ("靠右", 3), ("跨列居中", 4), ("分散对齐", 5)] {
                                    if ui.selectable_label(!halign_mixed && self.fmt_state.halign == v, lbl).clicked() {
                                        self.fmt_state.halign = v;
                                        self.fmt_touched.halign = true;
                                    }
                                }
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, if valign_mixed { "垂直对齐（混合）" } else { "垂直对齐" });
                            ui.horizontal(|ui| {
                                for (lbl, v) in [("常规", 0u8), ("顶端", 1), ("居中", 2), ("底端", 3)] {
                                    if ui.selectable_label(!valign_mixed && self.fmt_state.valign == v, lbl).clicked() {
                                        self.fmt_state.valign = v;
                                        self.fmt_touched.valign = true;
                                    }
                                }
                                ui.add_space(12.0);
                                ui.label(egui::RichText::new("缩进").size(ui_kit::T_BODY).color(pal.text_weak));
                                let mut ind = self.fmt_state.indent as f64;
                                if ui_kit::spin(ui, &mut ind, 0.0, 15.0, 1.0, 50.0) {
                                    self.fmt_state.indent = ind as u8;
                                    self.fmt_touched.indent = true;
                                }
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, "文本控制");
                            ui.horizontal_wrapped(|ui| {
                                format_checkbox(ui, "自动换行", &mut self.fmt_state.wrap, wrap_mixed, &mut self.fmt_touched.wrap);
                                format_checkbox(ui, "缩小字体填充", &mut self.fmt_state.shrink_to_fit, shrink_mixed, &mut self.fmt_touched.shrink_to_fit);
                                format_checkbox(ui, "竖排文字", &mut self.fmt_state.vertical_text, mixed.vertical_text, &mut self.fmt_touched.vertical_text);
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, "方向 / 合并");
                            ui.horizontal(|ui| {
                                let rotation_mixed = mixed.rotation && !self.fmt_touched.rotation;
                                let rotations = [("横排（0°）", 0i16), ("斜排（45°）", 45), ("竖排（90°）", 90), ("竖排（-90°）", -90), ("斜排（-45°）", -45)];
                                let angle = self.fmt_state.rotation;
                                let rotation_label = if rotation_mixed {
                                    "混合".to_owned()
                                } else {
                                    rotations.iter().find(|(_, v)| *v == angle)
                                        .map_or_else(|| format!("{angle}°"), |(text, _)| (*text).to_owned())
                                };
                                egui::ComboBox::from_id_salt("wps_fmt_rot")
                                    .selected_text(rotation_label)
                                    .width(120.0)
                                    .show_ui(ui, |ui| {
                                        for (lbl, v) in rotations {
                                            if ui.selectable_label(!rotation_mixed && angle == v, lbl).clicked() {
                                                self.fmt_state.rotation = v;
                                                self.fmt_touched.rotation = true;
                                            }
                                        }
                                    });
                                ui.add_space(10.0);
                                let merge_on = self.fmt_merge_pending == Some(true);
                                let unmerge_on = self.fmt_merge_pending == Some(false);
                                let can_merge = self.can_merge_selection() || unmerge_on;
                                let can_unmerge = self.selection_has_merge() || merge_on;
                                ui.add_enabled_ui(can_merge, |ui| {
                                    if ui_kit::pill(
                                        ui,
                                        if merge_on { "√ 将合并单元格" } else { "合并单元格" },
                                        "点确定后合并所选区域",
                                    )
                                    .clicked()
                                    {
                                        self.fmt_merge_pending = Some(true);
                                    }
                                });
                                ui.add_enabled_ui(can_unmerge, |ui| {
                                    if ui_kit::pill(
                                        ui,
                                        if unmerge_on { "√ 将取消合并" } else { "取消合并" },
                                        "点确定后拆分合并区域",
                                    )
                                    .clicked()
                                    {
                                        self.fmt_merge_pending = Some(false);
                                    }
                                });
                            });
                            ui.end_row();
                        });
                    }
                    2 => {
                        crate::dlg_section(ui, "字体设置");
                        egui::Grid::new("wps_fmt_font").num_columns(2).spacing([12.0, 9.0]).show(ui, |ui| {
                            crate::dlg_cell_label(ui, if font_mixed { "字体（混合）" } else { "字体" });
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("wps_fmt_dlg_font")
                                    .selected_text(if font_mixed {
                                        "混合".to_string()
                                    } else if self.fmt_state.font.is_empty() {
                                        "默认".to_string()
                                    } else {
                                        self.fmt_state.font.clone()
                                    })
                                    .width(160.0)
                                    .show_ui(ui, |ui| {
                                        for name in sheetcore::style::FONT_CHOICES {
                                            let label = if name.is_empty() { "默认" } else { *name };
                                            if ui.selectable_label(!font_mixed && self.fmt_state.font == *name, label).clicked() {
                                                self.fmt_state.font = name.to_string();
                                                self.fmt_touched.font = true;
                                            }
                                        }
                                    });
                                ui.add_space(8.0);
                                ui.label(egui::RichText::new(if size_mixed { "字号（混合）" } else { "字号" }).size(ui_kit::T_BODY).color(pal.text_weak));
                                let mut szv = self.fmt_state.size as f64;
                                if ui_kit::spin(ui, &mut szv, 6.0, 72.0, 1.0, 56.0) {
                                    self.fmt_state.size = szv as f32;
                                    self.fmt_touched.size = true;
                                }
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, "字形与效果");
                            ui.horizontal_wrapped(|ui| {
                                format_checkbox(ui, "加粗", &mut self.fmt_state.bold, mixed.bold, &mut self.fmt_touched.bold);
                                format_checkbox(ui, "倾斜", &mut self.fmt_state.italic, mixed.italic, &mut self.fmt_touched.italic);
                                format_checkbox(ui, "删除线", &mut self.fmt_state.strikethrough, mixed.strikethrough, &mut self.fmt_touched.strikethrough);
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, "下划线");
                            let underline_mixed = (mixed.underline && !self.fmt_touched.underline)
                                || (mixed.double_underline && !self.fmt_touched.double_underline);
                            ui.horizontal(|ui| {
                                for (label, single, double) in [("无", false, false), ("单下划线", true, false), ("双下划线", true, true)] {
                                    let selected = !underline_mixed && self.fmt_state.double_underline == double
                                        && (double || self.fmt_state.underline == single);
                                    if ui.radio(selected, label).clicked() {
                                        self.fmt_state.underline = single;
                                        self.fmt_state.double_underline = double;
                                        self.fmt_touched.underline = true;
                                        self.fmt_touched.double_underline = true;
                                    }
                                }
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, if color_mixed { "颜色（混合）" } else { "文字颜色" });
                            ui.horizontal(|ui| {
                                if ui.color_edit_button_srgb(&mut self.fmt_state.color).changed() {
                                    self.fmt_touched.color = true;
                                }
                                for (label, rgb) in [
                                    ("黑色", [0, 0, 0]), ("橙色", [216, 59, 1]),
                                    ("红色", [192, 0, 0]), ("绿色", [16, 124, 65]),
                                    ("蓝色", [68, 114, 196]),
                                ] {
                                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                                    let selected = !color_mixed && self.fmt_state.color == rgb;
                                    let border = if selected || resp.has_focus() { pal.accent }
                                        else if resp.hovered() { pal.text_weak } else { pal.card_border };
                                    ui.painter().rect_stroke(r, 4.0, Stroke::new(if selected { 2.0 } else { 1.0 }, border), egui::StrokeKind::Inside);
                                    ui.painter().rect_filled(r.shrink(3.0), 2.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                                    ui_kit::a11y_name(&resp, label, ui.is_enabled(), Some(selected));
                                    let resp = resp.on_hover_text(label);
                                    if resp.clicked() {
                                        self.fmt_state.color = rgb;
                                        self.fmt_touched.color = true;
                                    }
                                }
                            });
                            ui.end_row();
                        });
                    }
                    3 => self.format_borders_tab(ui),
                    4 => {
                        crate::dlg_section(ui, if fill_mixed { "单元格背景填充（混合）" } else { "单元格背景填充" });
                        let mut clear = wps_chrome::palette_reset(ui, "无背景色", !fill_mixed && self.fmt_state.fill.is_none()).clicked();
                        ui.add_space(4.0);
                        let mut picked = None;
                        let recent = self.recent_colors.clone();
                        let selected = if fill_mixed { None } else { self.fmt_state.fill };
                        wps_chrome::paint_wps_color_palette(ui, selected, &recent, None, &mut clear, &mut picked);
                        if clear {
                            self.fmt_state.fill = None;
                            self.fmt_touched.fill = true;
                        }
                        if let Some(rgb) = picked {
                            self.fmt_state.fill = Some(rgb);
                            self.fmt_touched.fill = true;
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("自定义颜色：");
                            let mut col = self.fmt_state.fill.unwrap_or([255, 255, 255]);
                            if ui.color_edit_button_srgb(&mut col).changed() {
                                self.fmt_state.fill = Some(col);
                                self.fmt_touched.fill = true;
                            }
                        });
                    }
                    _ => {
                        crate::dlg_section(ui, "工作表保护选项");
                        format_checkbox(ui, "锁定", &mut self.fmt_state.locked, locked_mixed, &mut self.fmt_touched.locked);
                        format_checkbox(ui, "隐藏公式", &mut self.fmt_state.hidden, hidden_mixed, &mut self.fmt_touched.hidden);
                        ui.label(egui::RichText::new("需在「审阅 → 保护工作表」启用后生效。").size(ui_kit::T_BODY).color(pal.text_weak));
                    }
                }

                ui_kit::footer(ui, |ui| {
                    if ui_kit::secondary_button(ui, "取消").clicked() {
                        cancel = true;
                    }
                    if ui_kit::primary_button(ui, "确定").clicked() {
                        apply = true;
                    }
                });
            });

        if apply {
            self.apply_format_dialog();
        } else if cancel || !open {
            self.cancel_format_dialog();
        }
    }

    /// 一体化打印与出版工作室 (`Ctrl+P`)：委托至 `print_studio::draw_print_studio`
    pub(crate) fn draw_print_preview_wps(&mut self, ctx: &egui::Context) {
        self.draw_print_studio(ctx);
    }
}
