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
        let mut x = menu_rect
            .left()
            .clamp(screen.left() + 6.0, (screen.right() - w - 6.0).max(screen.left() + 6.0));
        let y = if menu_rect.top() - h - 8.0 >= screen.top() + 6.0 {
            menu_rect.top() - h - 8.0
        } else if menu_rect.bottom() + h + 8.0 <= screen.bottom() - 6.0 {
            menu_rect.bottom() + 8.0
        } else {
            // 窗口较矮导致上下均放不下时，避让到右键菜单右/左侧，绝不遮盖菜单底部的批注/合并条目
            if menu_rect.right() + w + 8.0 <= screen.right() - 6.0 {
                x = menu_rect.right() + 8.0;
            } else if menu_rect.left() - w - 8.0 >= screen.left() + 6.0 {
                x = menu_rect.left() - w - 8.0;
            }
            menu_rect
                .top()
                .clamp(screen.top() + 6.0, (screen.bottom() - h - 6.0).max(screen.top() + 6.0))
        };
        let mini_rect = Rect::from_min_size(egui::pos2(x, y), Vec2::new(w, h));
        self.ctx_rects.push(mini_rect);

        let cur = self.current_style();
        let font_pop_id = egui::Id::new("wps_mini_font_pop");
        let size_pop_id = egui::Id::new("wps_mini_size_pop");
        let mut pop_mode = ctx.data_mut(|d| d.get_temp::<u8>(font_pop_id).unwrap_or(0));

        let mut act: Option<String> = None;
        let mut set_font: Option<String> = None;
        let mut set_size: Option<f32> = None;
        let mut open_color: Option<(bool, egui::Pos2)> = None;
        let mut apply_fill_direct = false;
        let mut apply_font_direct = false;

        egui::Area::new(egui::Id::new("wps_mini_toolbar"))
            .order(egui::Order::Foreground)
            .fixed_pos(mini_rect.min)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::WHITE)
                    .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                    .corner_radius(8.0)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 14,
                        spread: 0,
                        color: Color32::from_black_alpha(26),
                    })
                    .inner_margin(egui::Margin::symmetric(6, 5))
                    .show(ui, |ui| {
                        ui.set_min_size(Vec2::new(w - 12.0, h - 10.0));
                        ui.spacing_mut().item_spacing = Vec2::new(2.0, 4.0);

                        // Row 1: 字体(74) + 字号(40) + A+(21) + A-(21) + B(22) + I(22) + U(22) = 222 + 12 = 234
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let flabel = if cur.font.is_empty() { "默认" } else { cur.font.as_str() };
                            let (fr, fresp) = ui.allocate_exact_size(Vec2::new(74.0, 22.0), Sense::click());
                            ui.painter().rect(
                                fr,
                                4.0,
                                if fresp.hovered() || pop_mode == 1 { ui_kit::HOVER } else { Color32::from_rgb(248, 250, 252) },
                                Stroke::new(1.0, ui_kit::CARD_BORDER),
                                egui::StrokeKind::Inside,
                            );
                            ui.painter().with_clip_rect(fr.shrink2(Vec2::new(5.0, 2.0))).text(
                                egui::pos2(fr.left() + 5.0, fr.center().y),
                                Align2::LEFT_CENTER,
                                flabel,
                                FontId::proportional(11.0),
                                ui_kit::TEXT,
                            );
                            ui_kit::caret(ui, egui::pos2(fr.right() - 7.0, fr.center().y), ui_kit::TEXT_WEAK);
                            if fresp.on_hover_text("字体").clicked() {
                                pop_mode = if pop_mode == 1 { 0 } else { 1 };
                            }

                            let (sr, sresp) = ui.allocate_exact_size(Vec2::new(40.0, 22.0), Sense::click());
                            ui.painter().rect(
                                sr,
                                4.0,
                                if sresp.hovered() || pop_mode == 2 { ui_kit::HOVER } else { Color32::from_rgb(248, 250, 252) },
                                Stroke::new(1.0, ui_kit::CARD_BORDER),
                                egui::StrokeKind::Inside,
                            );
                            ui.painter().text(
                                egui::pos2(sr.left() + 5.0, sr.center().y),
                                Align2::LEFT_CENTER,
                                format!("{}", cur.size as i32),
                                FontId::proportional(11.0),
                                ui_kit::TEXT,
                            );
                            ui_kit::caret(ui, egui::pos2(sr.right() - 6.0, sr.center().y), ui_kit::TEXT_WEAK);
                            if sresp.on_hover_text("字号").clicked() {
                                pop_mode = if pop_mode == 2 { 0 } else { 2 };
                            }

                            if ui_kit::small_on(ui, Icon::FontGrow, false, "增大字号").clicked() {
                                act = Some("FontGrow".into());
                            }
                            if ui_kit::small_on(ui, Icon::FontShrink, false, "减小字号").clicked() {
                                act = Some("FontShrink".into());
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
                            let fc_r = ui_kit::color_button(ui, None, Some(Icon::Fill), fill_c, "填充颜色");
                            if fc_r.clicked() {
                                apply_fill_direct = true;
                            }
                            let fa_r = ui_kit::arrow_glyph_button(ui, Vec2::new(11.0, 22.0), ui_kit::TEXT_WEAK);
                            if fa_r.clicked() {
                                open_color = Some((false, fa_r.rect.left_bottom()));
                            }

                            // Font color split
                            let font_c = Color32::from_rgb(self.last_font_color[0], self.last_font_color[1], self.last_font_color[2]);
                            let fcr = ui_kit::color_button(ui, Some("A"), Some(Icon::FontColor), font_c, "字体颜色");
                            if fcr.clicked() {
                                apply_font_direct = true;
                            }
                            let far = ui_kit::arrow_glyph_button(ui, Vec2::new(11.0, 22.0), ui_kit::TEXT_WEAK);
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
                    egui::Frame::NONE
                        .fill(Color32::WHITE)
                        .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                        .corner_radius(6.0)
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 4],
                            blur: 12,
                            spread: 0,
                            color: Color32::from_black_alpha(28),
                        })
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

        ctx.data_mut(|d| d.insert_temp::<u8>(font_pop_id, pop_mode));

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

        egui::Area::new(egui::Id::new("wps_fill_badge"))
            .order(egui::Order::Foreground)
            .fixed_pos(btn_pos)
            .show(ctx, |ui| {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(72.0, 22.0), Sense::click());
                ui.painter().rect(
                    r,
                    5.0,
                    if resp.hovered() || is_open { Color32::WHITE } else { Color32::from_rgb(248, 250, 252) },
                    Stroke::new(1.0, if is_open { ui_kit::ACCENT } else { ui_kit::CARD_BORDER }),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    egui::pos2(r.left() + 8.0, r.center().y),
                    Align2::LEFT_CENTER,
                    "填充选项",
                    FontId::proportional(11.0),
                    ui_kit::TEXT,
                );
                ui_kit::caret(ui, egui::pos2(r.right() - 9.0, r.center().y), ui_kit::TEXT_WEAK);
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
                        .fill(Color32::WHITE)
                        .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                        .corner_radius(6.0)
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 4],
                            blur: 12,
                            spread: 0,
                            color: Color32::from_black_alpha(26),
                        })
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| {
                            ui.set_width(136.0);
                            for (lbl, mode) in [
                                ("智能填充（默认）", FillMode::Smart),
                                ("复制单元格", FillMode::CopyCells),
                                ("填充序列", FillMode::FillSeries),
                                ("仅填充格式", FillMode::FormatsOnly),
                                ("不带格式填充", FillMode::ValuesOnly),
                            ] {
                                let mark = if cur_mode == mode { format!("√ {lbl}") } else { format!("   {lbl}") };
                                if ui_kit::popup_item(ui, &mark, true, 134.0).clicked() {
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

        egui::Area::new(egui::Id::new("wps_paste_badge"))
            .order(egui::Order::Foreground)
            .fixed_pos(btn_pos)
            .show(ctx, |ui| {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(72.0, 22.0), Sense::click());
                ui.painter().rect(
                    r,
                    5.0,
                    if resp.hovered() || is_open { Color32::WHITE } else { Color32::from_rgb(248, 250, 252) },
                    Stroke::new(1.0, if is_open { ui_kit::ACCENT } else { ui_kit::CARD_BORDER }),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    egui::pos2(r.left() + 8.0, r.center().y),
                    Align2::LEFT_CENTER,
                    "粘贴选项",
                    FontId::proportional(11.0),
                    ui_kit::TEXT,
                );
                ui_kit::caret(ui, egui::pos2(r.right() - 9.0, r.center().y), ui_kit::TEXT_WEAK);
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
                        .fill(Color32::WHITE)
                        .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
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
                ui.set_width(520.0);
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                ui_kit::dialog_tabs(ui, &mut self.fmt_tab, &["数字", "对齐", "字体", "边框", "填充", "保护"]);
                ui.add_space(4.0);

                match self.fmt_tab {
                    0 => {
                        // WPS 两栏数字设置
                        ui.horizontal(|ui| {
                            // 左栏：12 个分类列表
                            egui::Frame::NONE
                                .fill(Color32::from_rgb(248, 250, 252))
                                .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                                .corner_radius(6.0)
                                .inner_margin(egui::Margin::same(6))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.set_width(120.0);
                                        ui.set_min_height(248.0);
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        let cats = [
                                            "常规", "数值", "货币", "会计专用", "日期", "时间",
                                            "百分比", "分数", "科学记数", "文本", "特殊", "自定义",
                                        ];
                                        for (idx, name) in cats.iter().enumerate() {
                                            let sel = self.fmt_num_cat == idx as u8;
                                            let r = ui.selectable_label(sel, *name);
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
                                ui.set_width(376.0);
                                let sample_box = |ui: &mut egui::Ui, preview: &str| {
                                    egui::Frame::NONE
                                        .fill(Color32::from_rgb(248, 250, 252))
                                        .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                                        .corner_radius(6.0)
                                        .inner_margin(egui::Margin::symmetric(10, 8))
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.label(egui::RichText::new("示例").size(11.0).color(ui_kit::TEXT_WEAK));
                                            ui.label(egui::RichText::new(preview).size(15.0).color(ui_kit::TEXT));
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
                                        ui.label(egui::RichText::new("常规单元格格式不包含任何特定的数字格式。").size(12.0).color(ui_kit::TEXT_WEAK));
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
                                            ui.label(egui::RichText::new("负数显示方式：").size(11.5).color(ui_kit::TEXT_WEAK));
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
                                        ui.label(egui::RichText::new("在文本单元格格式中，数字作为文本处理（@）。").size(12.0).color(ui_kit::TEXT_WEAK));
                                    }
                                    10 => {
                                        ui.label(egui::RichText::new("中文特色财务与数字格式：").size(11.5).color(ui_kit::TEXT_WEAK));
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
                                        ui.label(egui::RichText::new("自定义格式代码：").size(11.5).color(ui_kit::TEXT_WEAK));
                                        let mut custom = self.fmt_state.format.clone();
                                        if ui.add(egui::TextEdit::singleline(&mut custom).desired_width(350.0).hint_text("例如 #,##0.00;[Red]-#,##0.00")).changed() {
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
                                    if ui.selectable_value(&mut self.fmt_state.halign, v, lbl).clicked() {
                                        self.fmt_touched.halign = true;
                                    }
                                }
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, if valign_mixed { "垂直对齐（混合）" } else { "垂直对齐" });
                            ui.horizontal(|ui| {
                                for (lbl, v) in [("常规", 0u8), ("顶端", 1), ("居中", 2), ("底端", 3)] {
                                    if ui.selectable_value(&mut self.fmt_state.valign, v, lbl).clicked() {
                                        self.fmt_touched.valign = true;
                                    }
                                }
                                ui.add_space(12.0);
                                ui.label(egui::RichText::new("缩进").size(11.5).weak());
                                let mut ind = self.fmt_state.indent as f64;
                                if ui_kit::spin(ui, &mut ind, 0.0, 15.0, 1.0, 50.0) {
                                    self.fmt_state.indent = ind as u8;
                                    self.fmt_touched.indent = true;
                                }
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, "文本控制");
                            ui.horizontal_wrapped(|ui| {
                                if ui.checkbox(&mut self.fmt_state.wrap, if wrap_mixed { "自动换行（混合）" } else { "自动换行" }).changed() {
                                    self.fmt_touched.wrap = true;
                                }
                                if ui.checkbox(&mut self.fmt_state.shrink_to_fit, if shrink_mixed { "缩小字体填充（混合）" } else { "缩小字体填充" }).changed() {
                                    self.fmt_touched.shrink_to_fit = true;
                                }
                                if ui.checkbox(&mut self.fmt_state.vertical_text, "竖排文字").changed() {
                                    self.fmt_touched.vertical_text = true;
                                }
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, "方向 / 合并");
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("wps_fmt_rot")
                                    .selected_text(match self.fmt_state.rotation {
                                        90 => "竖排（90°）",
                                        -90 => "竖排（-90°）",
                                        45 => "斜排（45°）",
                                        -45 => "斜排（-45°）",
                                        _ => "横排（0°）",
                                    })
                                    .width(120.0)
                                    .show_ui(ui, |ui| {
                                        for (lbl, v) in [("横排（0°）", 0i16), ("斜排（45°）", 45), ("竖排（90°）", 90), ("竖排（-90°）", -90), ("斜排（-45°）", -45)] {
                                            if ui.selectable_value(&mut self.fmt_state.rotation, v, lbl).clicked() {
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
                                            if ui.selectable_label(self.fmt_state.font == *name, label).clicked() {
                                                self.fmt_state.font = name.to_string();
                                                self.fmt_touched.font = true;
                                            }
                                        }
                                    });
                                ui.add_space(8.0);
                                ui.label(egui::RichText::new(if size_mixed { "字号(混合)" } else { "字号" }).size(11.5).weak());
                                let mut szv = self.fmt_state.size as f64;
                                if ui_kit::spin(ui, &mut szv, 6.0, 72.0, 1.0, 56.0) {
                                    self.fmt_state.size = szv as f32;
                                    self.fmt_touched.size = true;
                                }
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, "字形与效果");
                            ui.horizontal(|ui| {
                                if ui_kit::text_toggle(ui, "B", self.fmt_state.bold, "加粗", true, false, false).clicked() {
                                    self.fmt_state.bold = !self.fmt_state.bold;
                                    self.fmt_touched.bold = true;
                                }
                                if ui_kit::text_toggle(ui, "I", self.fmt_state.italic, "倾斜", false, true, false).clicked() {
                                    self.fmt_state.italic = !self.fmt_state.italic;
                                    self.fmt_touched.italic = true;
                                }
                                if ui_kit::text_toggle(ui, "U", self.fmt_state.underline, "单下划线", false, false, true).clicked() {
                                    self.fmt_state.underline = !self.fmt_state.underline;
                                    self.fmt_touched.underline = true;
                                }
                                if ui_kit::text_toggle(ui, "双", self.fmt_state.double_underline, "双下划线", false, false, true).clicked() {
                                    self.fmt_state.double_underline = !self.fmt_state.double_underline;
                                    self.fmt_touched.double_underline = true;
                                    if self.fmt_state.double_underline {
                                        self.fmt_state.underline = true;
                                        self.fmt_touched.underline = true;
                                    }
                                }
                                if ui_kit::text_toggle(ui, "S", self.fmt_state.strikethrough, "删除线", false, false, false).clicked() {
                                    self.fmt_state.strikethrough = !self.fmt_state.strikethrough;
                                    self.fmt_touched.strikethrough = true;
                                }
                            });
                            ui.end_row();

                            crate::dlg_cell_label(ui, if color_mixed { "颜色（混合）" } else { "文字颜色" });
                            ui.horizontal(|ui| {
                                if ui.color_edit_button_srgb(&mut self.fmt_state.color).changed() {
                                    self.fmt_touched.color = true;
                                }
                                for rgb in [[0u8, 0, 0], [216, 59, 1], [192, 0, 0], [16, 124, 65], [68, 114, 196]] {
                                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                                    ui.painter().rect_filled(r, 3.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                                    if resp.clicked() {
                                        self.fmt_state.color = rgb;
                                        self.fmt_touched.color = true;
                                    }
                                }
                            });
                            ui.end_row();
                        });
                    }
                    3 => {
                        // WPS 边框选项卡：线型 + 黑色默认颜色 + 预设 + 可点击预览盒
                        ui.horizontal(|ui| {
                            // 左侧：线型 + 颜色
                            ui.vertical(|ui| {
                                ui.set_width(210.0);
                                crate::dlg_section(ui, "线条样式");
                                for (lbl, code) in [("细实线", 1u8), ("中实线", 2), ("粗实线", 3), ("虚线", 5), ("双线", 4)] {
                                    let sel = self.fmt_border_style == code;
                                    if ui.selectable_label(sel, lbl).clicked() {
                                        self.fmt_border_style = code;
                                    }
                                }
                                ui.add_space(6.0);
                                crate::dlg_section(ui, "颜色");
                                ui.horizontal(|ui| {
                                    ui.label("边框颜色：");
                                    ui.color_edit_button_srgb(&mut self.fmt_border_color);
                                });
                            });

                            ui.separator();

                            // 右侧：预置 + 交互式 2×2 预览方块
                            ui.vertical(|ui| {
                                crate::dlg_section(ui, "预置与边线点击");
                                ui.horizontal(|ui| {
                                    if ui_kit::pill(ui, "无边框", "清除所有边框").clicked() {
                                        self.fmt_state.border = [false; 4];
                                        self.fmt_touched.border = [true; 4];
                                        self.fmt_state.border_inside_h = false;
                                        self.fmt_state.border_inside_v = false;
                                        self.fmt_touched.border_inside_h = true;
                                        self.fmt_touched.border_inside_v = true;
                                    }
                                    if ui_kit::pill(ui, "外边框", "设置外框线").clicked() {
                                        self.fmt_state.border = [true; 4];
                                        self.fmt_touched.border = [true; 4];
                                    }
                                    if ui_kit::pill(ui, "所有/内部框线", "同时包含内部横竖框线").clicked() {
                                        self.fmt_state.border = [true; 4];
                                        self.fmt_touched.border = [true; 4];
                                        self.fmt_state.border_inside_h = true;
                                        self.fmt_state.border_inside_v = true;
                                        self.fmt_touched.border_inside_h = true;
                                        self.fmt_touched.border_inside_v = true;
                                    }
                                });
                                ui.add_space(6.0);
                                let (box_r, _) = ui.allocate_exact_size(Vec2::new(220.0, 128.0), Sense::hover());
                                ui.painter().rect(
                                    box_r,
                                    6.0,
                                    Color32::from_rgb(249, 250, 252),
                                    Stroke::new(1.0, ui_kit::CARD_BORDER),
                                    egui::StrokeKind::Inside,
                                );
                                let preview = box_r.shrink2(Vec2::new(34.0, 20.0));
                                ui.painter().rect_filled(preview, 0.0, Color32::WHITE);
                                let lw = match self.fmt_border_style {
                                    2 => 2.0,
                                    3 => 3.0,
                                    _ => 1.2,
                                };
                                let bc = Color32::from_rgb(self.fmt_border_color[0], self.fmt_border_color[1], self.fmt_border_color[2]);
                                let sides = [
                                    (0usize, preview.left_top(), preview.left_bottom(), Rect::from_min_max(preview.left_top() - Vec2::new(8.0, 0.0), egui::pos2(preview.left() + 8.0, preview.bottom())), "左边线"),
                                    (1usize, preview.right_top(), preview.right_bottom(), Rect::from_min_max(egui::pos2(preview.right() - 8.0, preview.top()), preview.right_bottom() + Vec2::new(8.0, 0.0)), "右边线"),
                                    (2usize, preview.left_top(), preview.right_top(), Rect::from_min_max(preview.left_top() - Vec2::new(0.0, 8.0), egui::pos2(preview.right(), preview.top() + 8.0)), "上边线"),
                                    (3usize, preview.left_bottom(), preview.right_bottom(), Rect::from_min_max(egui::pos2(preview.left(), preview.bottom() - 8.0), preview.right_bottom() + Vec2::new(0.0, 8.0)), "下边线"),
                                ];
                                for (idx, a, b, hit, tip) in sides {
                                    let resp = ui.interact(hit, egui::Id::new(("wps_bd_hit", idx)), Sense::click());
                                    if self.fmt_state.border[idx] {
                                        ui.painter().line_segment([a, b], Stroke::new(lw, bc));
                                    } else {
                                        ui.painter().line_segment([a, b], Stroke::new(1.0, Color32::from_rgb(224, 228, 236)));
                                    }
                                    if resp.hovered() {
                                        ui.painter().rect_stroke(hit.shrink(3.0), 2.0, Stroke::new(1.0, ui_kit::ACCENT), egui::StrokeKind::Inside);
                                    }
                                    if resp.on_hover_text(tip).clicked() {
                                        self.fmt_state.border[idx] = !self.fmt_state.border[idx];
                                        self.fmt_touched.border[idx] = true;
                                    }
                                }
                                // 内部横/竖可点击十字
                                let mid_h = Rect::from_center_size(preview.center(), Vec2::new(preview.width() - 16.0, 14.0));
                                let rh = ui.interact(mid_h, egui::Id::new("wps_bd_ih"), Sense::click());
                                ui.painter().line_segment(
                                    [egui::pos2(preview.left(), preview.center().y), egui::pos2(preview.right(), preview.center().y)],
                                    Stroke::new(if self.fmt_state.border_inside_h { lw } else { 1.0 }, if self.fmt_state.border_inside_h { bc } else { Color32::from_rgb(230, 234, 240) }),
                                );
                                if rh.on_hover_text("内部横线").clicked() {
                                    self.fmt_state.border_inside_h = !self.fmt_state.border_inside_h;
                                    self.fmt_touched.border_inside_h = true;
                                }
                                let mid_v = Rect::from_center_size(preview.center(), Vec2::new(14.0, preview.height() - 16.0));
                                let rv = ui.interact(mid_v, egui::Id::new("wps_bd_iv"), Sense::click());
                                ui.painter().line_segment(
                                    [egui::pos2(preview.center().x, preview.top()), egui::pos2(preview.center().x, preview.bottom())],
                                    Stroke::new(if self.fmt_state.border_inside_v { lw } else { 1.0 }, if self.fmt_state.border_inside_v { bc } else { Color32::from_rgb(230, 234, 240) }),
                                );
                                if rv.on_hover_text("内部竖线").clicked() {
                                    self.fmt_state.border_inside_v = !self.fmt_state.border_inside_v;
                                    self.fmt_touched.border_inside_v = true;
                                }
                            });
                        });
                    }
                    4 => {
                        crate::dlg_section(ui, if fill_mixed { "单元格背景填充（混合）" } else { "单元格背景填充" });
                        let mut clear = false;
                        let mut picked = None;
                        let recent = self.recent_colors.clone();
                        wps_chrome::paint_wps_color_palette(ui, self.fmt_state.fill, &recent, Some("无背景色"), &mut clear, &mut picked);
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
                        if ui.checkbox(&mut self.fmt_state.locked, if locked_mixed { "锁定（混合）" } else { "锁定" }).changed() {
                            self.fmt_touched.locked = true;
                        }
                        if ui.checkbox(&mut self.fmt_state.hidden, if hidden_mixed { "隐藏公式（混合）" } else { "隐藏公式" }).changed() {
                            self.fmt_touched.hidden = true;
                        }
                        ui.label(egui::RichText::new("需在「审阅 → 保护工作表」启用后生效。").size(11.5).weak());
                    }
                }

                ui.add_space(6.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_kit::secondary_button(ui, "取消").clicked() {
                            cancel = true;
                        }
                        if ui_kit::primary_button(ui, "确定").clicked() {
                            apply = true;
                        }
                    });
                });
            });

        if apply {
            self.apply_format_dialog();
        } else if cancel || !open {
            self.cancel_format_dialog();
        }
    }

    /// WPS 升级版左右实时分栏打印预览 (`Ctrl+P`, ~920×660)
    pub(crate) fn draw_print_preview_wps(&mut self, ctx: &egui::Context) {
        if !self.print_preview {
            return;
        }
        let mut open = true;
        let mut do_print = false;
        let mut do_export = false;
        let mut do_page_setup = false;
        let mut goto_page: Option<usize> = None;

        let (layout, ops, page_no, total, cur_page) = {
            let sheet = self.wb.sheet(self.sheet_idx);
            let layout = sheetcore::print::paginate(sheet, &sheet.page);
            let total = layout.pages.len().max(1);
            let page_no = self.preview_page.min(total - 1);
            let ops = sheetcore::print::page_ops_with(&self.wb, self.sheet_idx, &layout, page_no, self.wb.path.as_deref());
            (layout, ops, page_no, total, sheet.page.clone())
        };

        let (pw, ph) = layout.page_size;
        let margins = cur_page.margins;
        let page_h = 460.0 * self.preview_zoom.clamp(0.5, 2.0);
        let page_w = page_h * pw / ph;

        ui_kit::window(ctx, "打印与预览", 920.0)
            .open(&mut open)
            .default_size([920.0, 600.0])
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    // 左侧 272px 设置列
                    ui.vertical(|ui| {
                        ui.set_width(272.0);
                        ui.spacing_mut().item_spacing = Vec2::new(6.0, 5.0);
                        ui.horizontal(|ui| {
                            if ui_kit::primary_button(ui, "立即打印").clicked() {
                                do_print = true;
                            }
                            if ui_kit::secondary_button(ui, "导出 PDF…").clicked() {
                                do_export = true;
                            }
                        });
                        ui.separator();

                        ui.horizontal(|ui| {
                            ui.label("打印份数：");
                            let mut cp = self.print_copies as f64;
                            if ui_kit::spin(ui, &mut cp, 1.0, 99.0, 1.0, 64.0) {
                                self.print_copies = cp as u32;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("页面范围：");
                            ui.selectable_value(&mut self.print_range, 0, "全部");
                            ui.selectable_value(&mut self.print_range, 1, "当前页");
                        });

                        ui.separator();
                        crate::dlg_section(ui, "页面与纸张（实时生效）");
                        ui.horizontal(|ui| {
                            ui.label("方向：");
                            if ui.selectable_label(!cur_page.landscape, "纵向").clicked() {
                                self.set_page(|p| p.landscape = false);
                            }
                            if ui.selectable_label(cur_page.landscape, "横向").clicked() {
                                self.set_page(|p| p.landscape = true);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("纸张：");
                            for (lbl, id) in [("A4", 9u16), ("A3", 8), ("Letter", 1)] {
                                if ui.selectable_label(cur_page.paper == id, lbl).clicked() {
                                    self.set_page(|p| p.paper = id);
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("页边距：");
                            let is_narrow = (margins[0] - 0.25).abs() < 0.05;
                            let is_wide = (margins[0] - 1.0).abs() < 0.05;
                            if ui.selectable_label(!is_narrow && !is_wide, "常规").clicked() {
                                self.set_page(|p| p.margins = [0.7, 0.7, 0.75, 0.75, 0.3, 0.3]);
                            }
                            if ui.selectable_label(is_narrow, "窄").clicked() {
                                self.set_page(|p| p.margins = [0.25, 0.25, 0.75, 0.75, 0.3, 0.3]);
                            }
                            if ui.selectable_label(is_wide, "宽").clicked() {
                                self.set_page(|p| p.margins = [1.0, 1.0, 1.0, 1.0, 0.5, 0.5]);
                            }
                        });

                        crate::dlg_section(ui, "一键适应页面缩放");
                        let fit_mode = if !cur_page.fit_to_page {
                            0u8
                        } else if cur_page.fit_width == 1 && cur_page.fit_height == 1 {
                            1
                        } else if cur_page.fit_width == 1 {
                            2
                        } else {
                            3
                        };
                        for (m, lbl) in [
                            (0u8, "无缩放（按实际大小 100%）"),
                            (1, "将工作表调整为一页 (1×1)"),
                            (2, "将所有列调整为一页宽"),
                            (3, "将所有行调整为一页高"),
                        ] {
                            if ui.selectable_label(fit_mode == m, lbl).clicked() {
                                self.set_page(|p| match m {
                                    1 => {
                                        p.fit_to_page = true;
                                        p.fit_width = 1;
                                        p.fit_height = 1;
                                    }
                                    2 => {
                                        p.fit_to_page = true;
                                        p.fit_width = 1;
                                        p.fit_height = 0;
                                    }
                                    3 => {
                                        p.fit_to_page = true;
                                        p.fit_width = 0;
                                        p.fit_height = 1;
                                    }
                                    _ => {
                                        p.fit_to_page = false;
                                        p.fit_width = 0;
                                        p.fit_height = 0;
                                    }
                                });
                            }
                        }

                        ui.separator();
                        let mut g = cur_page.gridlines;
                        if ui.checkbox(&mut g, "打印网格线").changed() {
                            self.set_page(|p| p.gridlines = g);
                        }
                        let mut h = cur_page.headings;
                        if ui.checkbox(&mut h, "打印行号与列标").changed() {
                            self.set_page(|p| p.headings = h);
                        }
                        let mut ch = cur_page.center_h;
                        if ui.checkbox(&mut ch, "水平居中打印").changed() {
                            self.set_page(|p| p.center_h = ch);
                        }
                        let mut cv = cur_page.center_v;
                        if ui.checkbox(&mut cv, "垂直居中打印").changed() {
                            self.set_page(|p| p.center_v = cv);
                        }
                        ui.add_space(2.0);
                        if ui_kit::secondary_button(ui, "更多页面设置…").clicked() {
                            do_page_setup = true;
                        }
                    });

                    ui.separator();

                    // 右侧：实时纸张预览区 + 底部翻页缩放条
                    ui.vertical(|ui| {
                        let avail_w = ui.available_width().max(320.0);
                        ui.set_width(avail_w);
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(236, 240, 245))
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_min_height(490.0);
                                egui::ScrollArea::both().max_height(490.0).show(ui, |ui| {
                                    ui.vertical_centered(|ui| {
                                        let (resp, painter) = ui.allocate_painter(Vec2::new(page_w, page_h), Sense::hover());
                                        let page_rect = resp.rect;
                                        painter.rect_filled(page_rect.translate(Vec2::new(3.0, 4.0)), 2.0, Color32::from_black_alpha(24));
                                        painter.rect_filled(page_rect, 2.0, Color32::WHITE);
                                        painter.rect_stroke(page_rect, 2.0, Stroke::new(1.0, Color32::from_gray(165)), egui::StrokeKind::Inside);

                                        let s_pt = page_h / ph;
                                        let inner = Rect::from_min_max(
                                            page_rect.min + Vec2::new(margins[0] * 72.0 * s_pt, margins[2] * 72.0 * s_pt),
                                            page_rect.max - Vec2::new(margins[1] * 72.0 * s_pt, margins[3] * 72.0 * s_pt),
                                        );
                                        painter.rect_stroke(inner, 0.0, Stroke::new(0.8, Color32::from_gray(218)), egui::StrokeKind::Inside);

                                        let to_xy = |x: f32, y: f32| egui::pos2(page_rect.min.x + x * s_pt, page_rect.min.y + (ph - y) * s_pt);
                                        for (x, y, w, h, color) in &ops.rects {
                                            let a = to_xy(*x, *y + *h);
                                            let b = to_xy(*x + *w, *y);
                                            painter.rect_filled(Rect::from_min_max(a, b), 0.0, Color32::from_rgb(color[0], color[1], color[2]));
                                        }
                                        for (x1, y1, x2, y2, lw, color) in &ops.lines {
                                            painter.line_segment(
                                                [to_xy(*x1, *y1), to_xy(*x2, *y2)],
                                                Stroke::new((lw * s_pt).max(0.4), Color32::from_rgb(color[0], color[1], color[2])),
                                            );
                                        }
                                        for t in &ops.texts {
                                            painter.text(
                                                to_xy(t.x, t.baseline),
                                                Align2::LEFT_BOTTOM,
                                                &t.text,
                                                FontId::proportional((t.size * s_pt).max(3.0)),
                                                Color32::from_rgb(t.color[0], t.color[1], t.color[2]),
                                            );
                                        }
                                    });
                                });
                            });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui_kit::secondary_button(ui, "< 上一页").clicked() && page_no > 0 {
                                goto_page = Some(page_no - 1);
                            }
                            ui.label(format!("第 {} / {} 页", page_no + 1, total));
                            if ui_kit::secondary_button(ui, "下一页 >").clicked() && page_no + 1 < total {
                                goto_page = Some(page_no + 1);
                            }
                            ui.separator();
                            ui.label(format!(
                                "{} · 内容缩放 {:.0}%",
                                cur_page.paper_label(),
                                layout.scale * 100.0
                            ));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add(egui::Slider::new(&mut self.preview_zoom, 0.5..=2.0).text("预览缩放"));
                            });
                        });
                    });
                });
            });

        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            open = false;
        }
        if let Some(p) = goto_page {
            self.preview_page = p;
        }
        if do_print {
            self.print_to_printer();
        }
        if do_export {
            self.export_pdf_dialog();
        }
        if do_page_setup {
            self.open_page_setup();
        }
        self.print_preview = open;
    }
}
