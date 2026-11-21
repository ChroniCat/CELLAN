//! WPS 风格顶栏（38px 文档标签整合标题栏）、轻量「文件」下拉菜单、
//! 锚定调色盘、以及底栏（工作表标签 + 可拖动分割条 + 横向滚动条 32px 单行合一）。

use eframe::egui::{self, Align2, Color32, FontId, Key, Rect, Sense, Stroke, Vec2};
use std::path::PathBuf;

use crate::ui_icons::{self, Icon};
use crate::{tokens, ui_kit, CtxKind, CtxMenuState, SheetApp, TabSlot, SB};

/// WPS 标准 10 个主题基色（浅→深自动插值生成 6×10 矩阵）
const WPS_THEME_BASES: [[u8; 3]; 10] = [
    [255, 255, 255], // 白
    [0, 0, 0],       // 黑
    [231, 230, 230], // 浅灰
    [68, 84, 106],   // 深蓝灰
    [68, 114, 196],  // 主题蓝
    [237, 125, 49],  // 橙
    [165, 165, 165], // 灰
    [255, 192, 0],   // 金黄
    [91, 155, 213],  // 浅蓝
    [112, 173, 71],  // 绿
];

/// WPS 10 个标准色（底排固定）
const WPS_STANDARD_COLORS: [[u8; 3]; 10] = [
    [192, 0, 0],     // 深红
    [255, 0, 0],     // 红
    [255, 192, 0],   // 橙黄
    [255, 255, 0],   // 黄
    [146, 208, 80],  // 浅绿
    [0, 176, 80],    // 绿
    [0, 176, 240],   // 浅蓝
    [0, 112, 192],   // 蓝
    [0, 32, 96],     // 深蓝
    [112, 48, 160],  // 紫
];

fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t).round() as u8,
    ]
}

/// 生成 WPS 6×10 主题色调色矩阵
pub(crate) fn wps_theme_matrix() -> [[[u8; 3]; 10]; 6] {
    let mut out = [[[0u8; 3]; 10]; 6];
    // 第 0 行：基色；1..=3：与白混色变浅；4..=5：与黑混色变深
    // 第 0 列（白）和第 1 列（黑）用经典灰度梯度
    let white_col = [[255, 255, 255], [242, 242, 242], [217, 217, 217], [191, 191, 191], [166, 166, 166], [128, 128, 128]];
    let black_col = [[0, 0, 0], [128, 128, 128], [89, 89, 89], [64, 64, 64], [38, 38, 38], [13, 13, 13]];
    for col in 0..10 {
        for row in 0..6 {
            out[row][col] = if col == 0 {
                white_col[row]
            } else if col == 1 {
                black_col[row]
            } else {
                let base = WPS_THEME_BASES[col];
                match row {
                    0 => base,
                    1 => lerp_rgb(base, [255, 255, 255], 0.80),
                    2 => lerp_rgb(base, [255, 255, 255], 0.60),
                    3 => lerp_rgb(base, [255, 255, 255], 0.40),
                    4 => lerp_rgb(base, [0, 0, 0], 0.25),
                    _ => lerp_rgb(base, [0, 0, 0], 0.50),
                }
            };
        }
    }
    out
}

/// 绘制可复用的 WPS 主题色 + 标准色网格（供浮动调色盘与 Ctrl+1 格式对话框共用）
pub(crate) fn paint_wps_color_palette(
    ui: &mut egui::Ui,
    selected: Option<[u8; 3]>,
    recent: &[[u8; 3]],
    none_label: Option<&str>,
    clear_clicked: &mut bool,
    picked: &mut Option<[u8; 3]>,
) {
    if let Some(lbl) = none_label {
        let is_none = selected.is_none();
        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width().min(216.0), 24.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, ui_kit::HOVER);
        }
        let sw = Rect::from_min_size(egui::pos2(r.left() + 6.0, r.center().y - 7.0), Vec2::splat(14.0));
        ui.painter().rect(
            sw,
            2.0,
            Color32::WHITE,
            Stroke::new(1.0, Color32::from_rgb(180, 184, 192)),
            egui::StrokeKind::Inside,
        );
        if lbl.contains("无") {
            ui.painter().line_segment(
                [sw.left_bottom(), sw.right_top()],
                Stroke::new(1.2, Color32::from_rgb(200, 50, 50)),
            );
        } else {
            ui.painter().rect_filled(sw.shrink(2.0), 1.0, Color32::BLACK);
        }
        ui.painter().text(
            egui::pos2(sw.right() + 8.0, r.center().y),
            Align2::LEFT_CENTER,
            lbl,
            FontId::proportional(12.0),
            ui_kit::TEXT,
        );
        if is_none && lbl.contains("无") {
            ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, ui_kit::ACCENT), egui::StrokeKind::Inside);
        }
        if resp.clicked() {
            *clear_clicked = true;
        }
        ui.add_space(4.0);
    }

    ui.label(egui::RichText::new("主题颜色").size(11.0).color(ui_kit::TEXT_WEAK));
    let matrix = wps_theme_matrix();
    let cell_sz = Vec2::new(17.0, 15.0);
    let gap = 4.0;
    for (ri, row) in matrix.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for rgb in row {
                let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                let (rect, resp) = ui.allocate_exact_size(cell_sz, Sense::click());
                ui.painter().rect_filled(rect, 2.0, c);
                let is_cur = selected == Some(*rgb);
                let stroke = if is_cur {
                    Stroke::new(2.0, ui_kit::BRAND_NAVY)
                } else if resp.hovered() {
                    Stroke::new(1.5, ui_kit::ACCENT)
                } else {
                    Stroke::new(0.8, Color32::from_rgb(212, 216, 224))
                };
                ui.painter().rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
                if resp.clicked() {
                    *picked = Some(*rgb);
                }
            }
        });
        ui.add_space(if ri == 0 { 4.0 } else { 1.5 });
    }

    ui.add_space(4.0);
    ui.label(egui::RichText::new("标准色").size(11.0).color(ui_kit::TEXT_WEAK));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for rgb in WPS_STANDARD_COLORS {
            let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            let (rect, resp) = ui.allocate_exact_size(cell_sz, Sense::click());
            ui.painter().rect_filled(rect, 2.0, c);
            let is_cur = selected == Some(rgb);
            let stroke = if is_cur {
                Stroke::new(2.0, ui_kit::BRAND_NAVY)
            } else if resp.hovered() {
                Stroke::new(1.5, ui_kit::ACCENT)
            } else {
                Stroke::new(0.8, Color32::from_rgb(212, 216, 224))
            };
            ui.painter().rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
            if resp.clicked() {
                *picked = Some(rgb);
            }
        }
    });

    if !recent.is_empty() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new("最近使用").size(11.0).color(ui_kit::TEXT_WEAK));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for rgb in recent.iter().take(10) {
                let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                let (rect, resp) = ui.allocate_exact_size(cell_sz, Sense::click());
                ui.painter().rect_filled(rect, 2.0, c);
                let stroke = if selected == Some(*rgb) {
                    Stroke::new(2.0, ui_kit::BRAND_NAVY)
                } else if resp.hovered() {
                    Stroke::new(1.5, ui_kit::ACCENT)
                } else {
                    Stroke::new(0.8, Color32::from_rgb(212, 216, 224))
                };
                ui.painter().rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
                if resp.clicked() {
                    *picked = Some(*rgb);
                }
            }
        });
    }
}

fn check_outside_dismiss(ctx: &egui::Context, pass_id: egui::Id, rects: &[Rect]) -> bool {
    let cur_pass = ctx.cumulative_pass_nr();
    let opened_pass = ctx.data_mut(|d| {
        let v = d.get_temp::<u64>(pass_id);
        if v.is_none() {
            d.insert_temp::<u64>(pass_id, cur_pass);
        }
        v
    });
    if opened_pass.is_none() || opened_pass == Some(cur_pass) {
        return false;
    }
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        return true;
    }
    if ctx.input(|i| i.pointer.primary_pressed()) {
        if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
            if !rects.iter().any(|r| r.contains(p)) {
                return true;
            }
        }
    }
    false
}

fn clear_popup_pass(ctx: &egui::Context, pass_id: egui::Id) {
    ctx.data_mut(|d| {
        d.remove_temp::<u64>(pass_id);
    });
}

enum FileMenuAction {
    NewBlank,
    Open,
    OpenPath(PathBuf),
    Save,
    SaveAs,
    PrintPreview,
    ExportPdf,
    Encrypt,
    Protect,
    Share,
    BackstageInfo,
    GoHome,
    CloseDoc,
    CheckUpdate,
    DefaultApp,
}

fn menu_sep_line(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(16.0), 7.0), Sense::hover());
    ui.painter().line_segment(
        [egui::pos2(r.left() + 6.0, r.center().y), egui::pos2(r.right() - 6.0, r.center().y)],
        Stroke::new(1.0, ui_kit::SEP),
    );
}

impl SheetApp {
    /// WPS 轻量级「文件」下拉菜单（点「文件」弹出，不遮挡整窗；含文档信息入口进 Backstage）
    pub(crate) fn draw_file_menu(&mut self, ctx: &egui::Context) {
        let pass_id = egui::Id::new("wps_file_menu_pass");
        let sub_id = egui::Id::new("wps_file_menu_sub");
        if !self.file_menu_open {
            clear_popup_pass(ctx, pass_id);
            ctx.data_mut(|d| d.remove_temp::<u8>(sub_id));
            return;
        }

        let screen = ctx.screen_rect();
        let anchor = self
            .file_menu_anchor
            .unwrap_or_else(|| egui::pos2(screen.left() + 12.0, ui_kit::TITLE_H + ui_kit::TAB_H));
        let menu_w = 248.0;
        let menu_h_est = 462.0;
        let pos = egui::pos2(
            anchor.x.clamp(screen.left() + 4.0, (screen.right() - menu_w - 8.0).max(4.0)),
            anchor.y.clamp(screen.top() + 4.0, (screen.bottom() - menu_h_est - 8.0).max(4.0)),
        );

        let mut act: Option<FileMenuAction> = None;
        let mut active_sub = ctx.data_mut(|d| d.get_temp::<u8>(sub_id).unwrap_or(0));
        let mut rects = Vec::new();
        let mut sub_anchor_y = pos.y + 42.0;

        let main_resp = egui::Area::new(egui::Id::new("wps_file_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::WHITE)
                    .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                    .corner_radius(8.0)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 6],
                        blur: 18,
                        spread: 0,
                        color: Color32::from_black_alpha(32),
                    })
                    .inner_margin(egui::Margin::symmetric(6, 6))
                    .show(ui, |ui| {
                        ui.set_width(menu_w - 12.0);
                        ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);

                        let row = |ui: &mut egui::Ui,
                                   icon: Icon,
                                   label: &str,
                                   shortcut: &str,
                                   has_sub: bool,
                                   sub_on: bool|
                         -> egui::Response {
                            let (r, resp) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 28.0),
                                Sense::click(),
                            );
                            if resp.hovered() || sub_on {
                                ui.painter().rect_filled(r, 5.0, ui_kit::HOVER);
                            }
                            ui_icons::draw_excel(
                                ui.painter(),
                                Rect::from_center_size(egui::pos2(r.left() + 14.0, r.center().y), Vec2::splat(15.0)),
                                icon,
                                ui_kit::TEXT,
                                true,
                            );
                            ui.painter().text(
                                egui::pos2(r.left() + 30.0, r.center().y),
                                Align2::LEFT_CENTER,
                                label,
                                FontId::proportional(12.5),
                                ui_kit::TEXT,
                            );
                            if !shortcut.is_empty() {
                                ui.painter().text(
                                    egui::pos2(r.right() - 10.0, r.center().y),
                                    Align2::RIGHT_CENTER,
                                    shortcut,
                                    FontId::proportional(11.0),
                                    ui_kit::TEXT_WEAK,
                                );
                            }
                            if has_sub {
                                ui_kit::caret_right(
                                    ui,
                                    egui::pos2(r.right() - 10.0, r.center().y),
                                    ui_kit::TEXT_WEAK,
                                );
                            }
                            resp
                        };

                        if row(ui, Icon::NewFile, "新建空白工作簿", "Ctrl+N", false, false).clicked() {
                            act = Some(FileMenuAction::NewBlank);
                        }
                        if row(ui, Icon::Open, "打开…", "Ctrl+O", false, false).clicked() {
                            act = Some(FileMenuAction::Open);
                        }
                        let rec_r = row(ui, Icon::FolderPlus, "最近打开", "", true, active_sub == 2);
                        if rec_r.hovered() || rec_r.clicked() {
                            active_sub = 2;
                            sub_anchor_y = rec_r.rect.top();
                        }

                        menu_sep_line(ui);

                        if row(ui, Icon::Save, "保存", "Ctrl+S", false, false).clicked() {
                            act = Some(FileMenuAction::Save);
                        }
                        if row(ui, Icon::SaveAs, "另存为…", "F12", false, false).clicked() {
                            act = Some(FileMenuAction::SaveAs);
                        }

                        menu_sep_line(ui);

                        if row(ui, Icon::Print, "打印 / 打印预览", "Ctrl+P", false, false).clicked() {
                            act = Some(FileMenuAction::PrintPreview);
                        }
                        if row(ui, Icon::PdfExport, "导出为 PDF…", "", false, false).clicked() {
                            act = Some(FileMenuAction::ExportPdf);
                        }

                        menu_sep_line(ui);

                        if row(ui, Icon::Lock, "加密文档", "", false, false).clicked() {
                            act = Some(FileMenuAction::Encrypt);
                        }
                        if row(ui, Icon::Pin, "保护工作表", "", false, false).clicked() {
                            act = Some(FileMenuAction::Protect);
                        }
                        if row(ui, Icon::Share, "共享 / 局域网协作", "", false, false).clicked() {
                            act = Some(FileMenuAction::Share);
                        }

                        menu_sep_line(ui);

                        if row(ui, Icon::About, "文档信息与属性…", "", false, false).clicked() {
                            act = Some(FileMenuAction::BackstageInfo);
                        }
                        if row(ui, Icon::Home, "返回开始主页", "", false, false).clicked() {
                            act = Some(FileMenuAction::GoHome);
                        }
                        if row(ui, Icon::Close, "关闭当前文档", "Ctrl+W", false, false).clicked() {
                            act = Some(FileMenuAction::CloseDoc);
                        }

                        menu_sep_line(ui);

                        if row(ui, Icon::UpdateCheck, "检查更新", "", false, false).clicked() {
                            act = Some(FileMenuAction::CheckUpdate);
                        }
                        if row(ui, Icon::AppDefault, "设为默认电子表格应用", "", false, false).clicked() {
                            act = Some(FileMenuAction::DefaultApp);
                        }
                    });
            });
        rects.push(main_resp.response.rect);

        // 级联子菜单（最近打开）
        if active_sub == 2 {
            let sub_w = 300.0;
            let sub_x = if pos.x + menu_w + sub_w + 8.0 <= screen.right() {
                pos.x + menu_w - 2.0
            } else {
                (pos.x - sub_w + 2.0).max(4.0)
            };
            let sub_y = sub_anchor_y.clamp(screen.top() + 4.0, (screen.bottom() - 260.0).max(4.0));
            let rec_entries = self.ensure_recent_cache().to_vec();
            let sub_resp = egui::Area::new(egui::Id::new("wps_file_menu_cascade"))
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(sub_x, sub_y))
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(Color32::WHITE)
                        .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                        .corner_radius(8.0)
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 6],
                            blur: 16,
                            spread: 0,
                            color: Color32::from_black_alpha(28),
                        })
                        .inner_margin(egui::Margin::symmetric(6, 6))
                        .show(ui, |ui| {
                            ui.set_width(sub_w - 12.0);
                            if rec_entries.is_empty() {
                                ui.label(egui::RichText::new("暂无最近打开的文件").size(12.0).color(ui_kit::TEXT_WEAK));
                            } else {
                                for ent in rec_entries.into_iter().take(10) {
                                    let label = format!("{}  ({})", ent.name, ent.short_dir);
                                    if ui_kit::popup_item(ui, &label, true, ui.available_width()).clicked() {
                                        act = Some(FileMenuAction::OpenPath(ent.path));
                                    }
                                }
                            }
                        });
                });
            rects.push(sub_resp.response.rect);
        }

        ctx.data_mut(|d| d.insert_temp::<u8>(sub_id, active_sub));

        if let Some(a) = act {
            self.file_menu_open = false;
            match a {
                FileMenuAction::NewBlank => self.new_document(),
                FileMenuAction::Open => self.open_dialog(),
                FileMenuAction::OpenPath(p) => {
                    self.show_start = false;
                    self.open_any(p);
                }
                FileMenuAction::Save => self.save(false),
                FileMenuAction::SaveAs => self.save(true),
                FileMenuAction::PrintPreview => {
                    self.preview_page = 0;
                    self.print_preview = true;
                }
                FileMenuAction::ExportPdf => self.export_pdf_dialog(),
                FileMenuAction::Encrypt => {
                    self.enc_dlg = Some((String::new(), String::new(), String::new()));
                }
                FileMenuAction::Protect => {
                    self.protect_dlg = Some(crate::ProtectDlg::default());
                }
                FileMenuAction::Share => {
                    if self.net.is_none() {
                        self.start_host();
                    }
                    self.members_open = true;
                }
                FileMenuAction::BackstageInfo => {
                    self.backstage = true;
                    self.backstage_page = 0;
                }
                FileMenuAction::GoHome => {
                    if self.finish_pending_edit() {
                        self.show_start = true;
                    }
                }
                FileMenuAction::CloseDoc => {
                    let idx = self.active_tab;
                    self.request_close_tab(idx);
                }
                FileMenuAction::CheckUpdate => self.check_update(),
                FileMenuAction::DefaultApp => self.register_file_association(true),
            }
            return;
        }

        if check_outside_dismiss(ctx, pass_id, &rects) {
            self.file_menu_open = false;
        }
    }

    /// WPS 锚定调色盘：10×6 主题色 + 10 标准色 + 最近使用 + 无填充/自动黑 + 自定义 RGB
    pub(crate) fn draw_wps_color_popup(&mut self, ctx: &egui::Context) {
        let pass_id = egui::Id::new("wps_color_popup_pass");
        let custom_id = egui::Id::new("wps_color_custom_rgb");
        let Some(for_font) = self.color_popup else {
            clear_popup_pass(ctx, pass_id);
            ctx.data_mut(|d| d.remove_temp::<bool>(custom_id));
            return;
        };

        let screen = ctx.screen_rect();
        let w = 228.0;
        let anchor = self
            .color_popup_pos
            .unwrap_or_else(|| egui::pos2(screen.center().x - w * 0.5, ui_kit::TITLE_H + ui_kit::RIBBON_H));
        let pos = egui::pos2(
            anchor.x.clamp(screen.left() + 6.0, (screen.right() - w - 8.0).max(6.0)),
            anchor.y.clamp(screen.top() + 6.0, (screen.bottom() - 320.0).max(6.0)),
        );

        let cur_rgb = if for_font {
            Some(self.current_style().color)
        } else {
            self.current_style().fill
        };
        let mut clear_clicked = false;
        let mut picked: Option<[u8; 3]> = None;
        let mut show_custom = ctx.data_mut(|d| d.get_temp::<bool>(custom_id).unwrap_or(false));
        let recent = self.recent_colors.clone();

        let area_resp = egui::Area::new(egui::Id::new("wps_color_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::WHITE)
                    .stroke(Stroke::new(1.0, ui_kit::CARD_BORDER))
                    .corner_radius(8.0)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 6],
                        blur: 16,
                        spread: 0,
                        color: Color32::from_black_alpha(30),
                    })
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(w - 20.0);
                        paint_wps_color_palette(
                            ui,
                            cur_rgb,
                            &recent,
                            Some(if for_font { "自动颜色（黑色）" } else { "无填充颜色" }),
                            &mut clear_clicked,
                            &mut picked,
                        );
                        ui.add_space(6.0);
                        menu_sep_line(ui);
                        if ui_kit::popup_item(
                            ui,
                            if show_custom { "- 收起自定义颜色" } else { "+ 更多自定义颜色…" },
                            true,
                            ui.available_width(),
                        )
                        .clicked()
                        {
                            show_custom = !show_custom;
                            ctx.data_mut(|d| d.insert_temp::<bool>(custom_id, show_custom));
                        }
                        if show_custom {
                            let rgb_state_id = egui::Id::new("wps_color_custom_val");
                            let mut cur = ctx.data_mut(|d| {
                                d.get_temp::<[u8; 3]>(rgb_state_id)
                                    .unwrap_or(cur_rgb.unwrap_or([255, 242, 0]))
                            });
                            let mut c32 = Color32::from_rgb(cur[0], cur[1], cur[2]);
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("自选：").size(12.0));
                                if ui.color_edit_button_srgba(&mut c32).changed() {
                                    cur = [c32.r(), c32.g(), c32.b()];
                                    ctx.data_mut(|d| d.insert_temp::<[u8; 3]>(rgb_state_id, cur));
                                }
                                if ui_kit::secondary_button(ui, "应用").clicked() {
                                    picked = Some(cur);
                                }
                            });
                        }
                    });
            });

        if clear_clicked {
            if for_font {
                self.apply_font_color([0, 0, 0]);
            } else {
                self.apply_fill_color(None);
            }
            self.color_popup = None;
            return;
        }
        if let Some(rgb) = picked {
            if for_font {
                self.apply_font_color(rgb);
            } else {
                self.apply_fill_color(Some(rgb));
            }
            self.color_popup = None;
            return;
        }
        if check_outside_dismiss(ctx, pass_id, &[area_resp.response.rect]) {
            self.color_popup = None;
        }
    }

    /// WPS 38px 第 1 排顶栏：Logo + 单一「⌂ 主页」胶囊 + 文档标签条（+ 新建）+ 命令搜索 + 账户 + 窗口按钮
    pub(crate) fn draw_wps_title_bar(&mut self, ctx: &egui::Context) {
        let dark = ctx.style().visuals.dark_mode;
        let bg = if dark { Color32::from_rgb(38, 42, 49) } else { ui_kit::PAGE_BG };
        let fg = if dark { Color32::from_rgb(226, 229, 234) } else { Color32::from_rgb(29, 29, 29) };
        let hover = if dark { Color32::from_rgb(55, 62, 72) } else { ui_kit::HOVER_ON_PAGE };

        let mut tab_metas: Vec<(String, bool, bool)> = Vec::new();
        if self.has_open_workbook {
            for i in 0..self.tabs.len() {
                let title = self.tab_title(i);
                let dirty = match &self.tabs[i] {
                    TabSlot::Active => self.wb.dirty || (i == self.active_tab && self.editing.is_some()),
                    TabSlot::Parked(d) => d.wb.dirty,
                };
                tab_metas.push((title, dirty, !self.show_start && i == self.active_tab));
            }
        }
        let mut do_switch: Option<usize> = None;
        let mut do_return_tab: Option<usize> = None;
        let mut do_close: Option<usize> = None;
        let mut new_tab = false;

        egui::TopBottomPanel::top("titlebar")
            .show_separator_line(false)
            .exact_height(ui_kit::TITLE_H)
            .frame(egui::Frame::NONE.fill(bg))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter().clone();
                let right_bg = if self.show_start && !self.has_open_workbook && !dark {
                    let rail_w = 224.0;
                    let canvas_bg = Color32::from_rgb(248, 249, 251);
                    painter.rect_filled(
                        Rect::from_min_max(egui::pos2(rect.left() + rail_w, rect.top()), rect.right_bottom()),
                        0.0,
                        canvas_bg,
                    );
                    painter.rect_filled(
                        Rect::from_min_max(egui::pos2(rect.left() + rail_w - 1.0, rect.top()), egui::pos2(rect.left() + rail_w, rect.bottom())),
                        0.0,
                        ui_kit::CARD_BORDER,
                    );
                    canvas_bg
                } else {
                    bg
                };

                // 1. Logo + 「⌂ 主页」页签（只要有已打开工作簿，即使正在浏览主页也始终保留主页与全部文档标签！）
                let logo_cx = if self.show_start && !self.has_open_workbook {
                    rect.left() + 31.0
                } else {
                    rect.left() + 20.0
                };
                let ir = Rect::from_center_size(egui::pos2(logo_cx, rect.center().y), Vec2::splat(20.0));
                ui_kit::draw_brand_logo(&painter, ir);

                let mut left_end = rect.left() + 46.0;
                if self.has_open_workbook {
                    let home_active = self.show_start;
                    let hb = if home_active {
                        Rect::from_min_size(egui::pos2(rect.left() + 38.0, rect.top() + 6.0), Vec2::new(62.0, rect.height() - 6.0))
                    } else {
                        Rect::from_min_size(egui::pos2(rect.left() + 38.0, rect.center().y - 12.0), Vec2::new(58.0, 24.0))
                    };
                    let hresp = ui.interact(hb, egui::Id::new("title_home_btn"), Sense::click());
                    ui_kit::a11y_name(&hresp, "主页", true, None);
                    if home_active {
                        let rad = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
                        painter.rect(
                            hb,
                            rad,
                            Color32::WHITE,
                            Stroke::new(1.0, ui_kit::CARD_BORDER),
                            egui::StrokeKind::Inside,
                        );
                        painter.rect_filled(
                            Rect::from_min_max(egui::pos2(hb.left() + 8.0, hb.top()), egui::pos2(hb.right() - 8.0, hb.top() + 2.5)),
                            1.5,
                            ui_kit::ACCENT,
                        );
                    } else if !dark {
                        painter.rect(
                            hb,
                            6.0,
                            if hresp.hovered() { Color32::WHITE } else { Color32::from_rgba_unmultiplied(255, 255, 255, 190) },
                            Stroke::new(1.0, if hresp.hovered() { ui_kit::BRAND_NAVY } else { ui_kit::CARD_BORDER }),
                            egui::StrokeKind::Inside,
                        );
                    } else if hresp.hovered() {
                        painter.rect_filled(hb, 6.0, hover);
                    }
                    let hic = if home_active {
                        ui_kit::TEXT
                    } else if !dark && hresp.hovered() {
                        ui_kit::BRAND_NAVY
                    } else if !dark {
                        Color32::from_rgb(51, 65, 85)
                    } else {
                        fg
                    };
                    ui_kit::draw_clean_icon(
                        &painter,
                        Rect::from_min_size(egui::pos2(hb.left() + 8.0, hb.center().y - 7.0), Vec2::splat(14.0)),
                        0,
                        hic,
                    );
                    ui_kit::paint_medium(&painter, egui::pos2(hb.left() + 25.0, hb.center().y), Align2::LEFT_CENTER, "主页", 11.5, hic);
                    let tip = if home_active {
                        "返回当前已打开的工作簿"
                    } else {
                        "查看开始主页（当前文档保持打开，可随时点右侧文档标签返回）"
                    };
                    if hresp.on_hover_text(tip).clicked() {
                        if home_active {
                            self.show_start = false;
                        } else if self.finish_pending_edit() {
                            self.show_start = true;
                        }
                    }
                    left_end = hb.right() + 6.0;
                }

                // 2. 右侧控件尺寸预计算：窗口三键 + 账户 + 搜索框
                let user = self.my_name.clone();
                let max_u_chars = if rect.width() >= 1200.0 { 16 } else { 10 };
                let short_user = Self::elide(&user, max_u_chars);
                let u_font = FontId::proportional(11.5);
                let u_w = painter.layout_no_wrap(short_user.clone(), u_font.clone(), Color32::PLACEHOLDER).size().x;
                let acc_w = (32.0 + u_w + 12.0).clamp(84.0, 184.0);
                let acc = Rect::from_min_size(
                    egui::pos2(rect.right() - 46.0 * 3.0 - acc_w - 12.0, rect.center().y - 12.0),
                    Vec2::new(acc_w, 24.0),
                );
                let show_search = rect.width() >= 960.0 && !self.show_start;
                let search_w = (rect.width() * 0.19).clamp(188.0, 260.0);
                let sx = acc.left() - search_w - 10.0;
                let right_limit = if show_search { sx - 10.0 } else { acc.left() - 10.0 };

                // 3. 文档标签直接融入第 1 排（WPS 风格：即使切换到主页也始终保留全部已打开文档标签 + `+`）
                let mut cur_x = left_end;
                if self.has_open_workbook {
                    let tab_space = (right_limit - cur_x - 34.0).max(80.0);
                    let per_max = if tab_metas.len() <= 2 {
                        210.0
                    } else {
                        ((tab_space / tab_metas.len() as f32) - 4.0).clamp(84.0, 180.0)
                    };
                    for (i, (title, dirty, active)) in tab_metas.iter().enumerate() {
                        let natural_w = painter
                            .layout_no_wrap(title.clone(), FontId::proportional(12.0), fg)
                            .size()
                            .x
                            + 44.0;
                        let tw = natural_w.clamp(84.0, per_max);
                        if cur_x + tw + 30.0 > right_limit {
                            break;
                        }
                        let tab = Rect::from_min_size(egui::pos2(cur_x, rect.top() + 6.0), Vec2::new(tw, rect.height() - 6.0));
                        let close = Rect::from_center_size(egui::pos2(tab.right() - 12.0, tab.center().y), Vec2::splat(16.0));
                        let tab_hit = Rect::from_min_max(tab.min, egui::pos2(close.left() - 2.0, tab.bottom()));
                        let tr = ui.interact(tab_hit, egui::Id::new(("wps_doctab", i)), Sense::click());
                        let cr = ui.interact(close, egui::Id::new(("wps_doctabx", i)), Sense::click());
                        let rad = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
                        if *active {
                            painter.rect(
                                tab,
                                rad,
                                Color32::WHITE,
                                Stroke::new(1.0, ui_kit::CARD_BORDER),
                                egui::StrokeKind::Inside,
                            );
                            painter.rect_filled(
                                Rect::from_min_max(egui::pos2(tab.left() + 8.0, tab.top()), egui::pos2(tab.right() - 8.0, tab.top() + 2.5)),
                                1.5,
                                ui_kit::ACCENT,
                            );
                        } else if tr.hovered() || cr.hovered() {
                            painter.rect_filled(tab, rad, Color32::from_rgb(233, 237, 244));
                        } else if self.show_start {
                            painter.rect_filled(tab, rad, Color32::from_rgba_unmultiplied(255, 255, 255, 150));
                        }
                        let text_rect = Rect::from_min_max(egui::pos2(tab.left() + 10.0, tab.top()), egui::pos2(tab.right() - 28.0, tab.bottom()));
                        let tc = if *active { ui_kit::TEXT } else { Color32::from_rgb(71, 85, 105) };
                        painter.with_clip_rect(text_rect).text(
                            egui::pos2(text_rect.left(), tab.center().y),
                            Align2::LEFT_CENTER,
                            title,
                            if *active { ui_kit::medium_in(ctx, 12.0) } else { FontId::proportional(12.0) },
                            tc,
                        );
                        if *dirty {
                            painter.circle_filled(egui::pos2(tab.right() - 24.0, tab.center().y), 3.0, Color32::from_rgb(217, 119, 6));
                        }
                        if cr.hovered() {
                            painter.circle_filled(close.center(), 7.5, Color32::from_rgb(225, 228, 234));
                        }
                        let cc = if cr.hovered() { ui_kit::TEXT } else { Color32::from_rgb(120, 124, 132) };
                        let cc0 = close.center();
                        painter.line_segment([egui::pos2(cc0.x - 3.2, cc0.y - 3.2), egui::pos2(cc0.x + 3.2, cc0.y + 3.2)], Stroke::new(1.1, cc));
                        painter.line_segment([egui::pos2(cc0.x + 3.2, cc0.y - 3.2), egui::pos2(cc0.x - 3.2, cc0.y + 3.2)], Stroke::new(1.1, cc));
                        let tab_tip = if *dirty {
                            format!("{title}（有未保存的修改）")
                        } else {
                            title.clone()
                        };
                        if cr.on_hover_text("关闭此文档 (Ctrl+W)").clicked() {
                            do_close = Some(i);
                        } else if tr.on_hover_text(tab_tip).clicked() {
                            if self.show_start {
                                do_return_tab = Some(i);
                            } else if !*active {
                                do_switch = Some(i);
                            }
                        }
                        cur_x += tw + 3.0;
                    }
                    let plus = Rect::from_min_size(egui::pos2(cur_x + 2.0, rect.center().y - 11.0), Vec2::new(24.0, 22.0));
                    if plus.right() <= right_limit {
                        let pr = ui.interact(plus, egui::Id::new("wps_tab_plus"), Sense::click());
                        if pr.hovered() {
                            painter.rect_filled(plus, 5.0, hover);
                        }
                        let pc = plus.center();
                        painter.line_segment([egui::pos2(pc.x - 4.5, pc.y), egui::pos2(pc.x + 4.5, pc.y)], Stroke::new(1.3, ui_kit::TEXT_WEAK));
                        painter.line_segment([egui::pos2(pc.x, pc.y - 4.5), egui::pos2(pc.x, pc.y + 4.5)], Stroke::new(1.3, ui_kit::TEXT_WEAK));
                        if pr.on_hover_text("新建空白标签页 (Ctrl+N)").clicked() {
                            new_tab = true;
                        }
                        cur_x = plus.right() + 6.0;
                    }
                } else {
                    painter.text(
                        egui::pos2(rect.left() + 50.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        &self.last_title,
                        ui_kit::ui_bold_in(ctx, 14.0),
                        if dark { fg } else { Color32::from_rgb(15, 23, 42) },
                    );
                    cur_x = rect.left() + 216.0;
                }

                // 4. 搜索框
                if show_search {
                    let sr = Rect::from_min_size(egui::pos2(sx, rect.center().y - 13.0), Vec2::new(search_w, 26.0));
                    painter.rect(sr, 13.0, Color32::WHITE, Stroke::new(1.0, ui_kit::CARD_BORDER), egui::StrokeKind::Inside);
                    ui_icons::draw_excel(
                        &painter,
                        Rect::from_center_size(egui::pos2(sr.left() + 15.0, sr.center().y), Vec2::splat(13.0)),
                        Icon::Find,
                        Color32::from_rgb(112, 110, 108),
                        true,
                    );
                    let te_rect = Rect::from_min_max(egui::pos2(sr.left() + 28.0, sr.top() + 3.0), egui::pos2(sr.right() - 10.0, sr.bottom() - 3.0));
                    let te = ui.put(te_rect, egui::TextEdit::singleline(&mut self.search).frame(false).hint_text("搜索命令"));
                    let _ = ui.ctx().accesskit_node_builder(te.id, |node| node.set_label("搜索命令".to_string()));
                    if te.gained_focus() || te.changed() {
                        self.palette_q = self.search.clone();
                        self.palette_open = true;
                    }
                }

                // 5. 账户胶囊
                let aresp = ui.interact(acc, egui::Id::new("account"), Sense::click());
                if !dark {
                    painter.rect(
                        acc,
                        12.0,
                        if aresp.hovered() {
                            Color32::WHITE
                        } else {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 195)
                        },
                        Stroke::new(
                            1.0,
                            if aresp.hovered() {
                                ui_kit::BRAND_NAVY
                            } else {
                                ui_kit::CARD_BORDER
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                } else if aresp.hovered() {
                    painter.rect_filled(acc, 12.0, hover);
                }
                let ac = egui::pos2(acc.left() + 13.0, acc.center().y);
                painter.circle_filled(ac, 9.5, ui_kit::BRAND_NAVY);
                painter.text(
                    ac,
                    Align2::CENTER_CENTER,
                    user.chars().next().unwrap_or('U').to_uppercase().to_string(),
                    ui_kit::ui_bold_in(ctx, 10.5),
                    Color32::WHITE,
                );
                painter.text(
                    egui::pos2(ac.x + 14.0, acc.center().y),
                    Align2::LEFT_CENTER,
                    &short_user,
                    u_font,
                    if dark { fg } else { Color32::from_rgb(51, 65, 85) },
                );
                let _ = ui.ctx().accesskit_node_builder(aresp.id, |node| node.set_label(format!("账户：{user}")));
                if aresp.on_hover_text(format!("账户：{user}")).clicked() {
                    self.profile_draft = self.my_name.clone();
                    self.profile_msg.clear();
                    self.profile_open = true;
                }

                // 6. Windows 11 控制三键
                let bw = 46.0;
                let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                for (name, x) in [("close", rect.right() - bw), ("max", rect.right() - bw * 2.0), ("min", rect.right() - bw * 3.0)] {
                    let br = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(bw, ui_kit::TITLE_H));
                    let resp = ui.interact(br, egui::Id::new(("winbtn", name)), Sense::click());
                    if resp.hovered() {
                        painter.rect_filled(br, 0.0, if name == "close" { Color32::from_rgb(196, 43, 28) } else { hover });
                    }
                    let col = if name == "close" && resp.hovered() { Color32::WHITE } else { fg };
                    let c = br.center();
                    match name {
                        "close" => {
                            let d = 4.8;
                            let st = Stroke::new(1.15, col);
                            painter.line_segment([egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)], st);
                            painter.line_segment([egui::pos2(c.x + d, c.y - d), egui::pos2(c.x - d, c.y + d)], st);
                        }
                        "min" => {
                            painter.line_segment([egui::pos2(c.x - 5.0, c.y), egui::pos2(c.x + 5.0, c.y)], Stroke::new(1.15, col));
                        }
                        _ => {
                            let st = Stroke::new(1.15, col);
                            if maximized {
                                let a = Rect::from_min_size(egui::pos2(c.x - 3.5, c.y - 5.0), Vec2::new(8.0, 8.0));
                                let b2 = Rect::from_min_size(egui::pos2(c.x - 5.5, c.y - 3.0), Vec2::new(8.0, 8.0));
                                painter.rect_stroke(a, 1.0, st, egui::StrokeKind::Inside);
                                painter.rect_filled(b2, 1.0, if resp.hovered() { hover } else { right_bg });
                                painter.rect_stroke(b2, 1.0, st, egui::StrokeKind::Inside);
                            } else {
                                painter.rect_stroke(Rect::from_center_size(c, Vec2::new(9.6, 9.6)), 1.0, st, egui::StrokeKind::Inside);
                            }
                        }
                    }
                    if resp.clicked() {
                        match name {
                            "close" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                            "max" => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                            _ => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                        }
                    }
                }

                // 7. 空白拖动条
                if cur_x < right_limit - 8.0 {
                    let drag = Rect::from_min_max(egui::pos2(cur_x, rect.top() + 5.0), egui::pos2(right_limit, rect.bottom() - 2.0));
                    let dr = ui.interact(drag, egui::Id::new("title_drag"), Sense::click_and_drag());
                    if dr.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if dr.double_clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }
                }
            });

        if let Some(i) = do_close {
            self.request_close_tab(i);
        }
        if let Some(i) = do_return_tab {
            self.show_start = false;
            if i != self.active_tab {
                self.switch_tab(i);
            }
        }
        if let Some(i) = do_switch {
            self.switch_tab(i);
        }
        if new_tab {
            self.command_exec("NewTab", ctx);
        }
    }

    /// WPS 32px 单行合体底栏：左侧 `< >` + 工作表标签 + `+` | 中间可拖分割条 | 右侧表格横向滚动条
    pub(crate) fn draw_wps_bottom_bar(&mut self, ctx: &egui::Context) {
        let (cols, _) = self.view_axes();
        let view_w = (self.last_view.x - crate::HEAD_W).max(1.0);
        let hard_max_x = (cols.pos(16_384) - view_w).max(0.0);
        let mut sx = self.scroll.x;
        let calc_total_w = |app: &SheetApp, cur_sx: f32| -> f32 {
            let sh = app.wb.sheet(app.sheet_idx);
            let used_c2 = sh.used_bounds().map(|(_, _, _, c2)| c2).unwrap_or(0);
            let merge_c2 = sh.merges.iter().map(|m| m.3).max().unwrap_or(0);
            let sel_c2 = app.sel.1.max(app.sel_end.1);
            let view_c2 = cols.index_at((cur_sx + view_w).max(0.0));
            let eff_cols = (used_c2.max(merge_c2).max(sel_c2).max(view_c2) + 16).clamp(32, 16_384);
            cols.pos(eff_cols).max(view_w + 1.0)
        };
        let ratio = self.sheet_tab_split_ratio.clamp(0.2, 0.8);

        egui::TopBottomPanel::bottom("tabs")
            .show_separator_line(false)
            .exact_height(if self.show_start { 0.0 } else { tokens::size::SHEET_TAB_H })
            .frame(egui::Frame::NONE.fill(ui_kit::PAGE_BG))
            .show(ctx, |ui| {
                if self.show_start {
                    return;
                }
                let rect = ui.max_rect();
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                ui.painter().line_segment(
                    [rect.left_top(), rect.right_top()],
                    Stroke::new(1.0, ui_kit::CARD_BORDER),
                );
                let mut goto: Option<usize> = None;
                let mut ren: Option<(usize, String)> = None;

                let split_x = (rect.left() + rect.width() * ratio)
                    .clamp(rect.left() + 180.0, (rect.right() - 160.0).max(rect.left() + 180.0));
                let left_rect = Rect::from_min_max(rect.min, egui::pos2(split_x - 4.0, rect.bottom()));
                let split_rect = Rect::from_min_max(egui::pos2(split_x - 4.0, rect.top()), egui::pos2(split_x + 4.0, rect.bottom()));
                let hsb_right = (rect.right() - SB).max(split_x + 24.0);
                let hsb_rect = Rect::from_min_max(egui::pos2(split_x + 4.0, rect.top()), egui::pos2(hsb_right, rect.bottom()));

                // 左侧 `< >` 按钮（28×31 紧凑排布）
                for (i, dir) in ["left", "right"].iter().enumerate() {
                    let b = Rect::from_min_size(egui::pos2(left_rect.left() + 10.0 + i as f32 * 28.0, left_rect.top() + 1.0), Vec2::new(28.0, 31.0));
                    let can = !(*dir == "left" && self.tab_scroll <= 0.0);
                    let resp = ui.interact(b, egui::Id::new(("tabscroll", i)), Sense::click());
                    ui_kit::a11y_name(&resp, if *dir == "left" { "工作表标签左移" } else { "工作表标签右移" }, can, None);
                    if resp.hovered() && can {
                        ui.painter().rect_filled(b.shrink(4.0), 3.0, ui_kit::HOVER_ON_PAGE);
                    }
                    if resp.clicked() && can {
                        let delta = if *dir == "left" { -160.0 } else { 160.0 };
                        self.tab_scroll = (self.tab_scroll + delta).max(0.0);
                    }
                    let c = b.center();
                    let st = Stroke::new(1.1, if can { Color32::from_rgb(96, 94, 92) } else { Color32::from_rgb(200, 198, 196) });
                    let d = if *dir == "left" { -1.0 } else { 1.0 };
                    ui.painter().line_segment([egui::pos2(c.x - d * 2.5, c.y - 4.5), egui::pos2(c.x + d * 2.5, c.y)], st);
                    ui.painter().line_segment([egui::pos2(c.x + d * 2.5, c.y), egui::pos2(c.x - d * 2.5, c.y + 4.5)], st);
                }

                // 左侧工作表标签滚动区（右边留 34px 给常驻 `+`）
                let scroll_rect = Rect::from_min_max(
                    egui::pos2(left_rect.left() + 70.0, left_rect.top()),
                    egui::pos2((left_rect.right() - 34.0).max(left_rect.left() + 90.0), left_rect.bottom()),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
                    let area = egui::ScrollArea::horizontal()
                        .id_salt("sheet_tabs")
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, false])
                        .horizontal_scroll_offset(self.tab_scroll);
                    let out = area.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for i in 0..self.wb.sheets.len() {
                                if self.wb.sheets[i].hidden {
                                    continue;
                                }
                                let name = self.wb.sheets[i].name.clone();
                                let tab_color = self.wb.sheets[i].tab_color;
                                let resp = ui_kit::sheet_tab(ui, &name, self.sheet_idx == i, tab_color);
                                if resp.drag_started() {
                                    self.tab_drag = Some(i);
                                }
                                if let Some(from) = self.tab_drag {
                                    if from != i && resp.hovered() && ui.input(|ip| ip.pointer.any_down()) && self.swap_sheet_positions(from, i) {
                                        self.tab_drag = Some(i);
                                    }
                                }
                                if resp.drag_stopped() {
                                    self.tab_drag = None;
                                }
                                if resp.clicked() {
                                    if self.editing.as_ref().is_some_and(|ed| ed.buf.starts_with('=')) {
                                        self.switch_sheet_for_ref(i);
                                    } else {
                                        self.commit_edit(None);
                                        goto = Some(i);
                                    }
                                }
                                if resp.double_clicked() {
                                    ren = Some((i, name.clone()));
                                }
                                if resp.secondary_clicked() {
                                    if self.editing.is_some() && !self.finish_pending_edit() {
                                        return;
                                    }
                                    if let Some(p) = resp.interact_pointer_pos() {
                                        self.ctx_menu = Some(CtxMenuState {
                                            pos: p,
                                            frame: ui.ctx().cumulative_pass_nr(),
                                            sub: None,
                                            kind: CtxKind::Tab,
                                            idx: i,
                                            key: 0,
                                            sub_key: 0,
                                        });
                                        ui.ctx().request_repaint();
                                    }
                                }
                            }
                        });
                    });
                    self.tab_scroll = out.state.offset.x;
                });

                // 常驻「+ 新建工作表」按钮（紧邻分割条左侧）
                let nb = Rect::from_min_size(egui::pos2(left_rect.right() - 32.0, left_rect.top() + 1.0), Vec2::new(30.0, 31.0));
                let nresp = ui.interact(nb, egui::Id::new("newsheet"), Sense::click());
                if nresp.hovered() {
                    ui.painter().rect_filled(nb.shrink2(Vec2::new(2.0, 4.0)), 5.0, ui_kit::HOVER_ON_PAGE);
                }
                let c = nb.center();
                let st = Stroke::new(1.2, Color32::from_rgb(68, 68, 68));
                ui.painter().line_segment([egui::pos2(c.x - 4.5, c.y), egui::pos2(c.x + 4.5, c.y)], st);
                ui.painter().line_segment([egui::pos2(c.x, c.y - 4.5), egui::pos2(c.x, c.y + 4.5)], st);
                if nresp.on_hover_text("新建工作表").clicked() {
                    self.add_sheet();
                }

                // 中央可拖动分割条
                let sresp = ui.interact(split_rect, egui::Id::new("sheet_tab_splitter"), Sense::drag());
                if sresp.hovered() || sresp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                let sc = split_rect.center();
                let dot_col = if sresp.hovered() || sresp.dragged() {
                    ui_kit::ACCENT
                } else {
                    Color32::from_rgb(176, 182, 192)
                };
                for dy in [-4.0f32, 0.0, 4.0] {
                    ui.painter().circle_filled(egui::pos2(sc.x, sc.y + dy), 1.3, dot_col);
                }
                if sresp.dragged() {
                    if let Some(p) = sresp.interact_pointer_pos() {
                        self.sheet_tab_split_ratio = ((p.x - rect.left()) / rect.width().max(1.0)).clamp(0.2, 0.8);
                    }
                }

                // 右侧表格横向滚动条（居中 14px 纤细导轨；光标悬停在下方横向滚动条槽区时滚动滚轮同步推动滚动条与网格，对齐 WPS 行为）
                let hover_hsb = !self.dialog_modal
                    && self.ctx_menu.is_none()
                    && ui
                        .ctx()
                        .input(|ip| ip.pointer.hover_pos().is_some_and(|p| hsb_rect.contains(p)));
                if hover_hsb {
                    let (ctrl, d) = ui.input(|ip| (ip.modifiers.command, ip.raw_scroll_delta));
                    if !ctrl && (d.x.abs() > 0.1 || d.y.abs() > 0.1) {
                        let delta = if d.x.abs() > d.y.abs() {
                            -d.x
                        } else {
                            -d.y * 1.8
                        };
                        sx = (sx + delta).clamp(0.0, hard_max_x);
                        ctx.request_repaint();
                    }
                }
                let total_w = calc_total_w(self, sx);
                let hsb_bar = Rect::from_center_size(
                    hsb_rect.center(),
                    Vec2::new((hsb_rect.width() - 10.0).max(16.0), 14.0),
                );
                crate::scrollbar(ui, "hsb", hsb_bar, true, &mut sx, total_w, view_w);
                self.scroll.x = sx.clamp(0.0, hard_max_x);

                if let Some(i) = goto {
                    if i != self.sheet_idx {
                        self.switch_sheet_view(i);
                    }
                }
                if let Some((i, n)) = ren {
                    self.rename_sheet = Some((i, n));
                }
            });
    }
}
