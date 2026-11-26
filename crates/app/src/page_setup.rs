use crate::{dlg_section, ui_kit, win_chrome, SheetApp};
use eframe::egui;

impl SheetApp {
    /// UI-26：打开页面设置。先记快照，取消时完整恢复。
    pub(super) fn open_page_setup(&mut self) {
        if self.page_setup_open {
            return;
        }
        let page = self.wb.sheet(self.sheet_idx).page.clone();
        self.page_setup_sheet = self.wb.sheet(self.sheet_idx).identity;
        if std::env::var("SC_PAGE_LOG").is_ok() {
            eprintln!(
                "[page] landscape={} paper={} fit={} w={} h={} scale={} titles={:?} margins={:?}",
                page.landscape, page.paper, page.fit_to_page, page.fit_width, page.fit_height,
                page.scale, page.title_rows, page.margins
            );
        }
        self.page_setup_draft = page;
        self.page_setup_open = true;
        if self.page_setup_printers.is_empty() {
            self.refresh_printers();
        }
        if self.print_range == 2 {
            let total = sheetcore::print::paginate(
                self.wb.sheet(self.sheet_idx),
                &self.wb.sheet(self.sheet_idx).page,
            )
            .pages
            .len()
            .max(1) as u32;
            self.print_to = self.print_to.clamp(self.print_from.max(1), total);
        }
    }

    /// Only an accepted page setup becomes eligible for saving.
    pub(super) fn apply_page_setup_draft(&mut self) {
        let draft = self.page_setup_draft.clone();
        if self.wb.sheet(self.sheet_idx).page == draft {
            return;
        }
        let sh = self.wb.sheet_mut(self.sheet_idx);
        sh.page = draft;
        sh.page_dirty = true;
        self.wb.dirty = true;
    }

    pub(super) fn cancel_page_setup(&mut self) {
        self.page_setup_open = false;
        self.status = "已取消页面设置".into();
    }

    /// 系统打印机列表：使用 Win32 `winspool.drv` 原生接口在后台异步快速枚举，不阻塞界面线程，不拉起 PowerShell。
    pub(super) fn refresh_printers(&mut self) {
        if self.bg_printers_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.bg_printers_rx = Some(rx);
        std::thread::spawn(move || {
            let (list, def) = win_chrome::enumerate_printers_fast();
            let def_opt = (!def.is_empty()).then_some(def);
            let _ = tx.send((list, def_opt));
        });
    }

    /// Preview the draft while the workbook continues saving committed edits.
    pub(super) fn page_setup_dialog(&mut self, ctx: &egui::Context) {
        if !self.page_setup_open {
            return;
        }
        if self.wb.sheet(self.sheet_idx).identity != self.page_setup_sheet {
            self.page_setup_open = false;
            return;
        }
        let sel = self.sel;
        let sel_rect = self.sel_rect();
        let printers = self.page_setup_printers.clone();
        let printer_cur = self.page_setup_printer.clone();
        let total_pages = sheetcore::print::paginate(
            self.wb.sheet(self.sheet_idx),
            &self.page_setup_draft,
        )
        .pages
        .len()
        .max(1) as u32;
        let mut draft = self.page_setup_draft.clone();
        let mut printer = printer_cur;
        let mut copies = self.print_copies;
        let mut range = self.print_range;
        let mut from = self.print_from;
        let mut to = self.print_to;
        let mut open = true;
        let mut ok = false;
        let mut cancel = false;
        let mut do_print = false;
        let mut do_export = false;

        ui_kit::window(ctx, "页面设置", 560.0)
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(560.0).show(ui, |ui| {
                    dlg_section(ui, "纸张与方向");
                    ui.horizontal(|ui| {
                        let paper = draft.paper_label().to_string();
                        egui::ComboBox::from_id_salt("page_paper")
                            .selected_text(paper)
                            .width(196.0)
                            .show_ui(ui, |ui| {
                                for &(code, name) in sheetcore::page::PAPER_SIZES {
                                    ui.selectable_value(&mut draft.paper, code, name);
                                }
                            });
                        ui.selectable_value(&mut draft.landscape, false, "纵向");
                        ui.selectable_value(&mut draft.landscape, true, "横向");
                    });

                    dlg_section(ui, "页边距（毫米）");
                    let mut mm = [
                        draft.margins[0] * 25.4,
                        draft.margins[1] * 25.4,
                        draft.margins[2] * 25.4,
                        draft.margins[3] * 25.4,
                        draft.margins[4] * 25.4,
                        draft.margins[5] * 25.4,
                    ];
                    egui::Grid::new("page_margins").num_columns(2).show(ui, |ui| {
                        ui.label("左");
                        ui.add(egui::Slider::new(&mut mm[0], 0.0..=50.0).suffix(" mm"));
                        ui.end_row();
                        ui.label("右");
                        ui.add(egui::Slider::new(&mut mm[1], 0.0..=50.0).suffix(" mm"));
                        ui.end_row();
                        ui.label("上");
                        ui.add(egui::Slider::new(&mut mm[2], 0.0..=50.0).suffix(" mm"));
                        ui.end_row();
                        ui.label("下");
                        ui.add(egui::Slider::new(&mut mm[3], 0.0..=50.0).suffix(" mm"));
                        ui.end_row();
                        ui.label("页眉");
                        ui.add(egui::Slider::new(&mut mm[4], 0.0..=30.0).suffix(" mm"));
                        ui.end_row();
                        ui.label("页脚");
                        ui.add(egui::Slider::new(&mut mm[5], 0.0..=30.0).suffix(" mm"));
                        ui.end_row();
                    });
                    for i in 0..6 {
                        draft.margins[i] = mm[i] / 25.4;
                    }
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut draft.center_h, "水平居中");
                        ui.checkbox(&mut draft.center_v, "垂直居中");
                        ui.checkbox(&mut draft.gridlines, "打印网格线");
                        ui.checkbox(&mut draft.headings, "打印行号列标");
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut draft.black_and_white, "黑白单色");
                        ui.checkbox(&mut draft.draft, "草稿模式");
                        ui.separator();
                        ui.label("页序：");
                        ui.selectable_value(&mut draft.page_order_over_then_down, false, "先列后行");
                        ui.selectable_value(&mut draft.page_order_over_then_down, true, "先行后列");
                    });
                    ui.horizontal(|ui| {
                        ui.label("批注：");
                        ui.selectable_value(&mut draft.print_comments, 0, "无");
                        ui.selectable_value(&mut draft.print_comments, 1, "末尾汇总");
                        ui.selectable_value(&mut draft.print_comments, 2, "原位角标");
                        ui.separator();
                        ui.label("错误值：");
                        ui.selectable_value(&mut draft.print_errors, 0, "显示");
                        ui.selectable_value(&mut draft.print_errors, 1, "空白");
                        ui.selectable_value(&mut draft.print_errors, 2, "--");
                        ui.selectable_value(&mut draft.print_errors, 3, "#N/A");
                    });

                    dlg_section(ui, "缩放");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut draft.fit_to_page, false, "缩放");
                        ui.add_enabled(
                            !draft.fit_to_page,
                            egui::Slider::new(&mut draft.scale, 10..=400).suffix("%"),
                        );
                        ui.selectable_value(&mut draft.fit_to_page, true, "调整为");
                        ui.add_enabled(
                            draft.fit_to_page,
                            egui::Slider::new(&mut draft.fit_width, 0..=10).text("页宽"),
                        );
                        ui.label("×");
                        ui.add_enabled(
                            draft.fit_to_page,
                            egui::Slider::new(&mut draft.fit_height, 0..=10).text("页高"),
                        );
                        ui.label("（0=自动）");
                    });

                    dlg_section(ui, "打印区域与重复标题");
                    ui.horizontal(|ui| {
                        let area = match draft.print_area {
                            Some((r1, c1, r2, c2)) => format!(
                                "{}:{}",
                                sheetcore::model::cell_name(r1, c1),
                                sheetcore::model::cell_name(r2, c2)
                            ),
                            None => "未设置（打印整个已用区域）".into(),
                        };
                        ui.label(format!("打印区域：{area}"));
                        if ui.button("用当前选区").clicked() {
                            draft.print_area = Some(sel_rect);
                        }
                        if ui.button("清除").clicked() {
                            draft.print_area = None;
                        }
                    });
                    ui.horizontal(|ui| {
                        let rows = draft
                            .title_rows
                            .map(|(a, b)| format!("第 {}–{} 行", a + 1, b + 1))
                            .unwrap_or_else(|| "无".into());
                        ui.label(format!("顶端标题行：{rows}"));
                        if ui.button("用选区行").clicked() {
                            draft.title_rows = Some((sel_rect.0, sel_rect.2));
                        }
                        if ui.button("用当前行").clicked() {
                            draft.title_rows = Some((sel.0, sel.0));
                        }
                        if ui.button("清除").clicked() {
                            draft.title_rows = None;
                        }
                    });
                    ui.horizontal(|ui| {
                        let cols = draft
                            .title_cols
                            .map(|(a, b)| format!("第 {}–{} 列", a + 1, b + 1))
                            .unwrap_or_else(|| "无".into());
                        ui.label(format!("左侧标题列：{cols}"));
                        if ui.button("用选区列").clicked() {
                            draft.title_cols = Some((sel_rect.1, sel_rect.3));
                        }
                        if ui.button("用当前列").clicked() {
                            draft.title_cols = Some((sel.1, sel.1));
                        }
                        if ui.button("清除").clicked() {
                            draft.title_cols = None;
                        }
                    });

                    dlg_section(ui, "页眉和页脚");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.header)
                            .desired_width(f32::INFINITY)
                            .hint_text("页眉，例如 &L左&C中&R右"),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.footer)
                            .desired_width(f32::INFINITY)
                            .hint_text("页脚，例如 第 &P 页，共 &N 页"),
                    );

                    dlg_section(ui, "分页符");
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(sel.0 > 0, egui::Button::new(format!("在第 {} 行前插入水平分页", sel.0 + 1)))
                            .clicked()
                        {
                            draft.row_breaks.push(sel.0);
                            draft.row_breaks.sort_unstable();
                            draft.row_breaks.dedup();
                        }
                        if ui
                            .add_enabled(
                                !draft.row_breaks.is_empty(),
                                egui::Button::new(format!("清除水平分页（{}）", draft.row_breaks.len())),
                            )
                            .clicked()
                        {
                            draft.row_breaks.clear();
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(sel.1 > 0, egui::Button::new(format!("在第 {} 列前插入垂直分页", sel.1 + 1)))
                            .clicked()
                        {
                            draft.col_breaks.push(sel.1);
                            draft.col_breaks.sort_unstable();
                            draft.col_breaks.dedup();
                        }
                        if ui
                            .add_enabled(
                                !draft.col_breaks.is_empty(),
                                egui::Button::new(format!("清除垂直分页（{}）", draft.col_breaks.len())),
                            )
                            .clicked()
                        {
                            draft.col_breaks.clear();
                        }
                    });

                    dlg_section(ui, "打印");
                    ui.horizontal(|ui| {
                        ui.label("打印机");
                        let text = if printer.is_empty() { "系统默认".to_string() } else { printer.clone() };
                        egui::ComboBox::from_id_salt("page_printer")
                            .selected_text(text)
                            .width(300.0)
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(printer.is_empty(), "系统默认").clicked() {
                                    printer.clear();
                                }
                                for p in &printers {
                                    if ui.selectable_label(&printer == p, p).clicked() {
                                        printer = p.clone();
                                    }
                                }
                            });
                        if ui.button("刷新").clicked() {
                            self.page_setup_printers.clear();
                            self.refresh_printers();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("份数");
                        ui.add(egui::Slider::new(&mut copies, 1..=99));
                        ui.label("页码");
                        ui.selectable_value(&mut range, 0, "全部");
                        ui.selectable_value(&mut range, 1, "当前页");
                        ui.selectable_value(&mut range, 2, "自定义");
                        if range == 2 {
                            ui.add(egui::Slider::new(&mut from, 1..=total_pages.max(1)).text("从"));
                            ui.add(egui::Slider::new(&mut to, 1..=total_pages.max(1)).text("到"));
                        }
                    });
                    ui.label(format!("当前分页：共 {total_pages} 页"));
                    ui.horizontal(|ui| {
                        if ui.button("导出 PDF…").clicked() {
                            do_export = true;
                            ok = true;
                        }
                    });
                });
                ui_kit::footer(ui, |ui| {
                    if ui_kit::secondary_button(ui, "取消").clicked() {
                        cancel = true;
                    }
                    if ui_kit::secondary_button(ui, "打印…").clicked() {
                        do_print = true;
                        ok = true;
                    }
                    if ui_kit::primary_button(ui, "确定").clicked() {
                        ok = true;
                    }
                });
            });

        self.page_setup_draft = draft;
        self.page_setup_printer = printer;
        self.print_copies = copies;
        self.print_range = range;
        self.print_from = from;
        self.print_to = to;
        if ok {
            self.apply_page_setup_draft();
            self.page_setup_open = false;
            self.status = "页面设置已应用".into();
            if do_export {
                self.export_pdf_dialog();
            }
            if do_print {
                self.print_to_printer();
            }
        } else if cancel || !open {
            self.cancel_page_setup();
        }
    }

    /// 页眉和页脚
    pub(super) fn header_footer_dialog(&mut self, ctx: &egui::Context) {
        self.draw_modern_header_footer_dialog(ctx);
    }
}
