//! 功能区回调：状态（getEnabled/getPressed/getLabel/getText/getImage）与命令。
//!
//! 命令体全部落在既有方法上；这里只保留 MS-CUSTOMUI 回调那一层的转发，
//! 不复制业务逻辑。

use eframe::egui;

use super::CtlState;
use crate::{cell_name, col_label, ui_prefs, FormatPainter, SheetApp};

fn fmt_label(code: &str) -> &str {
    match code {
        "" => "常规",
        "#,##0.00" => "数值",
        "¥#,##0.00" => "货币",
        "0.0%" => "百分比",
        "yyyy-mm-dd" => "日期",
        "_-* ¥#,##0.00_-;-* ¥#,##0.00_-;_-* \"-\"??_-;_-@_-" => "会计",
        "_-* #,##0.00_-;-* #,##0.00_-;_-* \"-\"??_-;_-@_-" => "会计（无符号）",
        "0.00E+00" => "科学计数",
        "# ?/?" => "分数",
        "@" => "文本",
        other => other,
    }
}

fn size_text(v: f32) -> String {
    if (v - v.round()).abs() < 0.01 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

impl SheetApp {
    /// 控件状态：等价于 MS-CUSTOMUI 的 get* 回调。
    pub fn ctl_state(&self, id: &str) -> CtlState {
        let mut st = CtlState::default();
        let cur = self.current_style();
        match id {
            // 按钮的可用性与快捷键共用命令表里的同一个谓词
            "Undo" => st.enabled = self.command_enabled("Undo"),
            "Redo" => st.enabled = self.command_enabled("Redo"),
            "FileSave" | "FileSaveAs" | "FileOpen" | "NewTab" | "FilePrintPreview"
            | "PagePrintPreview" | "ViewPrintPreview" => {
                let action = crate::command::action_for_ctl(id);
                st.enabled = self.command_enabled(action);
                if let Some(hint) = self.disabled_hint(action) {
                    let base = crate::command::by_action(action)
                        .map(|c| c.tip())
                        .unwrap_or_default();
                    st.tip = Some(if base.is_empty() {
                        format!("{hint}。")
                    } else {
                        format!("{base}\n{hint}。")
                    });
                }
            }
            "Font" => {
                st.text = Some(if cur.font.is_empty() {
                    "默认".to_string()
                } else {
                    cur.font.clone()
                })
            }
            "FontSize" => st.text = Some(size_text(cur.size)),
            "Bold" => st.checked = cur.bold,
            "Italic" => st.checked = cur.italic,
            "Underline" => st.checked = cur.underline,
            "AlignTop" => st.checked = cur.valign == 1,
            "AlignMiddle" => st.checked = cur.valign == 2,
            "AlignBottom" => st.checked = cur.valign == 3,
            "AlignLeft" => st.checked = cur.halign == 1,
            "AlignCenter" => st.checked = cur.halign == 2,
            "AlignRight" => st.checked = cur.halign == 3,
            "WrapText" => st.checked = cur.wrap,
            "MergeCenter" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                st.checked = self
                    .wb
                    .sheet(self.sheet_idx)
                    .merges
                    .iter()
                    .any(|m| m.0 == r1 && m.1 == c1 && m.2 == r2 && m.3 == c2);
            }
            "FillColor" => {
                let f = cur.fill.unwrap_or([255, 255, 255]);
                st.swatch = Some(egui::Color32::from_rgb(f[0], f[1], f[2]));
            }
            "FontColor" => {
                let c = cur.color;
                st.swatch = Some(egui::Color32::from_rgb(c[0], c[1], c[2]));
            }
            "NumberFormat" => st.text = Some(fmt_label(&cur.format).to_string()),
            "PrintGridlines" => st.checked = self.wb.sheet(self.sheet_idx).page.gridlines,
            "PrintHeadings" => st.checked = self.wb.sheet(self.sheet_idx).page.headings,
            "ShowGridlines" => st.checked = self.show_gridlines,
            "ShowHeadings" => st.checked = self.show_headings,
            "ShowFormulaBar" => st.checked = self.show_formula_bar,
            "TextSpill" => st.checked = self.text_spill,
            "ShowFormulas" => st.checked = self.show_formulas,
            "DataFilter" => {
                st.checked = self.wb.sheet(self.sheet_idx).filter.is_some();
                if st.checked {
                    st.tip = Some("筛选已启用：再点一次取消".into());
                }
            }
            "FreezeTopRow" => st.checked = self.wb.sheet(self.sheet_idx).freeze == Some((1, 0)),
            "FreezeFirstColumn" => {
                st.checked = self.wb.sheet(self.sheet_idx).freeze == Some((0, 1))
            }
            "NormalView" => st.checked = self.view_mode == 0,
            "PageLayoutView" => st.checked = self.view_mode == 1,
            "PageBreakPreview" => st.checked = self.view_mode == 2,
            "TableTotals" => {
                if let Some(t) = self.table_at_sel() {
                    st.checked = t.totals_row;
                }
            }
            "ProtectSheet" => {
                let on = self.wb.sheet(self.sheet_idx).protection.is_some();
                st.label = Some(if on { "撤销保护" } else { "保护工作表" }.into());
            }
            "FileShare" | "ReviewShare" | "JoinAddress" | "RoomPassword" | "JoinRoom"
            | "AutoShare" => {
                st.visible = self.net.is_none();
                if id == "AutoShare" {
                    st.checked = self.auto_share;
                }
            }
            "FileOnlinePeers" | "ReviewOnlinePeers" => {
                st.visible = self.net.is_some();
                st.label = Some(format!("在线 {}", self.peers_list.len()));
            }
            "PromoteHost" => st.visible = self.net.is_some() && self.room_lost,
            "PaperSize" => {
                let page = &self.wb.sheet(self.sheet_idx).page;
                st.label = Some(if page.paper == 9 {
                    "纸张：A4".into()
                } else {
                    "纸张：Letter".into()
                });
            }
            "PageOrientation" => {
                let page = &self.wb.sheet(self.sheet_idx).page;
                st.tip = Some(if page.landscape {
                    "当前横向，点击改纵向".into()
                } else {
                    "当前纵向，点击改横向".into()
                });
            }
            "FitToWidth" => {
                let p = &self.wb.sheet(self.sheet_idx).page;
                st.text = Some(if p.fit_width == 0 {
                    "自动".into()
                } else {
                    format!("{} 页", p.fit_width)
                });
            }
            "FitToHeight" => {
                let p = &self.wb.sheet(self.sheet_idx).page;
                st.text = Some(if p.fit_height == 0 {
                    "自动".into()
                } else {
                    format!("{} 页", p.fit_height)
                });
            }
            "PageScale" => {
                let p = self.wb.sheet(self.sheet_idx).page.clone();
                let pct = if p.fit_to_page {
                    (p.scale_factor(self.wb.sheet(self.sheet_idx)) * 100.0).round() as i32
                } else {
                    p.scale as i32
                };
                st.text = Some(format!("{pct}%"));
                st.enabled = !p.fit_to_page;
            }
            "ZoomLevel" | "ZoomPercent" => {
                st.text = Some(format!("{:.0}%", self.zoom * 100.0))
            }
            "ToggleFileTree" => {
                st.label = Some(if self.tree_open {
                    "隐藏文件列表".into()
                } else {
                    "显示文件列表".into()
                })
            }
            "InsertWorkbookLinks" | "DataWorkbookLinks" => {
                let n = self.wb.links.len();
                if n > 0 {
                    st.label = Some(format!("链接 {n}"));
                }
            }
            "AreaLabel" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                st.text = Some(format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2)));
            }
            "ChartTitle" => {
                if let Some(ch) = self.chart_at_sel() {
                    st.text = Some(ch.title.clone());
                } else {
                    st.visible = false;
                }
            }
            "ChartMeta" => {
                if let Some(ch) = self.chart_at_sel() {
                    st.text = Some(format!("{:?} · {} 个系列", ch.kind, ch.series.len()));
                } else {
                    st.visible = false;
                }
            }
            "SparkTypeLine" => {
                if let Some(sp) = self.spark_at_sel() {
                    st.checked = sp.kind == sheetcore::sparkline::SparkKind::Line;
                }
            }
            "SparkTypeColumn" => {
                if let Some(sp) = self.spark_at_sel() {
                    st.checked = sp.kind == sheetcore::sparkline::SparkKind::Column;
                }
            }
            "SparkTypeWinLoss" => {
                if let Some(sp) = self.spark_at_sel() {
                    st.checked = sp.kind == sheetcore::sparkline::SparkKind::WinLoss;
                }
            }
            "SparkMarkers" => st.checked = self.spark_at_sel().map(|s| s.markers).unwrap_or(false),
            "SparkHigh" => st.checked = self.spark_at_sel().map(|s| s.high).unwrap_or(false),
            "SparkLow" => st.checked = self.spark_at_sel().map(|s| s.low).unwrap_or(false),
            "SparkFirst" => st.checked = self.spark_at_sel().map(|s| s.first).unwrap_or(false),
            "SparkLast" => st.checked = self.spark_at_sel().map(|s| s.last).unwrap_or(false),
            "SparkNegative" => {
                st.checked = self.spark_at_sel().map(|s| s.negative).unwrap_or(false)
            }
            "SparkAxis" => st.checked = self.spark_at_sel().map(|s| s.axis).unwrap_or(false),
            // 没有选中文本框时，编辑/删除不可用（和 Excel 一样）
            "EditTextBox" | "DeleteTextBox" => st.enabled = self.tb_sel.is_some(),
            "InsertSlicer" => st.enabled = self.pivot_last.is_some(),
            "TableName" => {
                if let Some(t) = self.table_at_sel() {
                    st.text = Some(format!("表名：{}", t.display));
                } else {
                    st.visible = false;
                }
            }
            "TableRange" => {
                if let Some(t) = self.table_at_sel() {
                    st.text = Some(format!(
                        "区域 {}:{}",
                        cell_name(t.range.0, t.range.1),
                        cell_name(t.range.2, t.range.3)
                    ));
                } else {
                    st.visible = false;
                }
            }
            _ => {}
        }
        if std::env::var("SC_RIBBON_STATE").map(|v| v == id).unwrap_or(false) {
            eprintln!("[ribbon] 状态 {id} → {st:?}");
        }
        st
    }

    /// 命令分发：XML 的 onAction → 既有实现。
    pub fn ribbon_action(&mut self, action: &str, ctx: &egui::Context) {
        // 登记过的应用级命令（保存/另存为/打开/新建/打印/撤销/重做）只有一份实现，
        // 快捷键与菜单点击都落到这里。
        if self.command_exec(action, ctx) {
            return;
        }
        // 下拉项按前缀带值
        if let Some(code) = action.strip_prefix("fmt:") {
            let code = code.to_string();
            self.apply_style(move |s| s.format = code.clone());
            return;
        }
        if let Some(name) = action.strip_prefix("InsertFn:") {
            let name = name.to_string();
            if self.editing.is_none() {
                self.begin_edit(None);
            }
            if let Some(ed) = self.editing.as_mut() {
                if !ed.buf.starts_with('=') {
                    ed.buf.insert(0, '=');
                }
                ed.buf.push_str(&name);
                ed.buf.push('(');
            }
            return;
        }
        if let Some(v) = action.strip_prefix("zoom:") {
            if let Ok(z) = v.parse::<f32>() {
                self.zoom = z.clamp(0.5, 2.5);
                ctx.set_zoom_factor(self.zoom);
            }
            return;
        }
        if let Some(v) = action.strip_prefix("fitw:") {
            let n: u16 = v.parse().unwrap_or(0);
            self.set_page(move |p| {
                p.fit_width = n;
                p.fit_to_page = p.fit_width > 0 || p.fit_height > 0;
                if !p.fit_to_page {
                    p.scale = 100;
                }
            });
            return;
        }
        if let Some(v) = action.strip_prefix("fith:") {
            let n: u16 = v.parse().unwrap_or(0);
            self.set_page(move |p| {
                p.fit_height = n;
                p.fit_to_page = p.fit_width > 0 || p.fit_height > 0;
                if !p.fit_to_page {
                    p.scale = 100;
                }
            });
            return;
        }
        if let Some(v) = action.strip_prefix("scale:") {
            let n: u16 = v.parse().unwrap_or(100);
            self.set_page(move |p| {
                p.scale = n;
                p.fit_to_page = false;
                p.fit_width = 0;
                p.fit_height = 0;
            });
            return;
        }

        match action {
            // ---------------------------------------------------- 文件
            "FileOpen" => self.open(),
            "FileSave" => self.save(false),
            "FileSaveAs" => self.save(true),
            "PrintPreview" => {
                self.preview_page = 0;
                self.print_preview = true;
            }
            "ExportPdf" => self.export_pdf_dialog(),
            "VersionHistory" => {
                if let Some(p) = self.wb.path.clone() {
                    self.history_view = Some((p.clone(), sheetcore::backup::list(&p)));
                } else {
                    self.status = "先保存文件，才有历史版本".into();
                }
            }
            "Undo" => self.do_undo(false),
            "Redo" => self.do_undo(true),
            "NewTab" => self.open_in_new_tab(std::path::PathBuf::new()),
            "OpenInNewTab" => {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Excel 工作簿", &["xlsx", "xlsm", "csv"])
                    .pick_file()
                {
                    self.open_in_new_tab(p);
                }
            }
            "CloseTab" => {
                let idx = self.active_tab;
                self.close_tab(idx);
            }
            "Share" => {
                if self.net.is_none() {
                    self.start_host();
                } else {
                    self.members_open = true;
                }
            }
            "ShowMembers" => self.members_open = true,
            "PromoteHost" => self.promote_to_host(),
            "JoinRoom" => self.join_shared(),
            "CheckUpdate" => self.check_update(),
            "RegisterAssociation" => self.register_file_association(false),
            "SetDefaultApp" => self.register_file_association(true),

            // ---------------------------------------------------- 开始
            "Paste" => self.clip_paste(false, None),
            "PasteSpecial" => self.paste_special = true,
            "Cut" => self.clip_copy(true, ctx),
            "Copy" => self.clip_copy(false, ctx),
            "FormatPainter" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let mut styles = Vec::new();
                {
                    let sheet = self.wb.sheet(self.sheet_idx);
                    for r in r1..=r2 {
                        let mut row = Vec::new();
                        for c in c1..=c2 {
                            row.push(sheet.style_of(r, c));
                        }
                        styles.push(row);
                    }
                }
                let continuous = self.painter_double;
                self.painter_double = false;
                self.format_painter = Some(FormatPainter {
                    rect: (r1, c1, r2, c2),
                    styles,
                    continuous,
                    wb_epoch: self.wb_epoch,
                    sheet: self.sheet_idx,
                });
                self.status = if continuous {
                    "格式刷（连续）：选择目标区域，Esc 结束".into()
                } else {
                    "格式刷：选择目标区域".into()
                };
            }
            "GrowFont" => {
                let cur = self.current_style();
                let s2 = (cur.size + 1.0).min(72.0);
                self.apply_style(move |s| s.size = s2);
            }
            "ShrinkFont" => {
                let cur = self.current_style();
                let s2 = (cur.size - 1.0).max(6.0);
                self.apply_style(move |s| s.size = s2);
            }
            "Bold" => {
                let v = !self.current_style().bold;
                self.apply_style(move |s| s.bold = v);
            }
            "Italic" => {
                let v = !self.current_style().italic;
                self.apply_style(move |s| s.italic = v);
            }
            "Underline" => {
                let v = !self.current_style().underline;
                self.apply_style(move |s| s.underline = v);
            }
            "BordersAll" => self.apply_style(|s| s.set_all_borders(1, [128, 128, 128])),
            "BorderBottom" => self.apply_style(|s| {
                s.border[2] = sheetcore::style::Border {
                    style: 1,
                    color: [128, 128, 128],
                }
            }),
            "BorderTop" => self.apply_style(|s| {
                s.border[0] = sheetcore::style::Border {
                    style: 1,
                    color: [128, 128, 128],
                }
            }),
            "BorderLeft" => self.apply_style(|s| {
                s.border[3] = sheetcore::style::Border {
                    style: 1,
                    color: [128, 128, 128],
                }
            }),
            "BorderRight" => self.apply_style(|s| {
                s.border[1] = sheetcore::style::Border {
                    style: 1,
                    color: [128, 128, 128],
                }
            }),
            "BorderOuter" => self.border_outer(1),
            "BorderThickOuter" => self.border_outer(2),
            "BorderThickBottom" => self.apply_style(|s| {
                s.border[2] = sheetcore::style::Border {
                    style: 2,
                    color: [128, 128, 128],
                }
            }),
            "BorderDoubleBottom" => self.apply_style(|s| {
                s.border[2] = sheetcore::style::Border {
                    style: 4,
                    color: [128, 128, 128],
                }
            }),
            "BorderInsideH" => {
                let (r1, _c1, r2, _c2) = self.sel_rect();
                self.apply_style_xy(|s, r, _c| {
                    if r > r1 && r <= r2 {
                        s.border[0] = sheetcore::style::Border {
                            style: 1,
                            color: [128, 128, 128],
                        };
                    }
                    if r >= r1 && r < r2 {
                        s.border[2] = sheetcore::style::Border {
                            style: 1,
                            color: [128, 128, 128],
                        };
                    }
                });
            }
            "BorderInsideV" => {
                let (_r1, c1, _r2, c2) = self.sel_rect();
                self.apply_style_xy(|s, _r, c| {
                    if c > c1 && c <= c2 {
                        s.border[3] = sheetcore::style::Border {
                            style: 1,
                            color: [128, 128, 128],
                        };
                    }
                    if c >= c1 && c < c2 {
                        s.border[1] = sheetcore::style::Border {
                            style: 1,
                            color: [128, 128, 128],
                        };
                    }
                });
            }
            "BorderNone" => {
                self.apply_style(|s| s.border = [sheetcore::style::Border::default(); 4])
            }
            "FillColor" => self.color_popup = Some(false),
            "FontColor" => self.color_popup = Some(true),
            "ClearFormat" => self.clear_formats(),
            "AlignTop" => self.apply_style(|s| s.valign = 1),
            "AlignMiddle" => self.apply_style(|s| s.valign = 2),
            "AlignBottom" => self.apply_style(|s| s.valign = 3),
            "AlignLeft" => self.apply_style(|s| s.halign = 1),
            "AlignCenter" => self.apply_style(|s| s.halign = 2),
            "AlignRight" => self.apply_style(|s| s.halign = 3),
            "DecreaseIndent" => self.apply_style(|s| s.indent = s.indent.saturating_sub(1)),
            "IncreaseIndent" => self.apply_style(|s| s.indent = (s.indent + 1).min(15)),
            "WrapText" => {
                let v = !self.current_style().wrap;
                self.apply_style(move |s| s.wrap = v);
            }
            "MergeCenter" => {
                let on = !self.ctl_state("MergeCenter").checked;
                self.toggle_merge(on);
                if on {
                    self.apply_style(|s| s.halign = 2);
                }
            }
            "MergeCells" => self.toggle_merge(true),
            "UnmergeCells" => self.toggle_merge(false),
            "CenterAcross" => self.apply_style(|s| s.halign = 2),
            "NumberFormatApply" => {}
            "CurrencyStyle" => self.apply_style(|s| s.format = "¥#,##0.00".into()),
            "PercentStyle" => self.apply_style(|s| s.format = "0.0%".into()),
            "CommaStyle" => self.apply_style(|s| s.format = "#,##0.00".into()),
            "IncreaseDecimal" => self.decimals(1),
            "DecreaseDecimal" => self.decimals(-1),
            "ConditionalFormatting" => self.cf_mgr = true,
            "FormatAsTable" => self.make_table(),
            "CellStyles" => self.fmt_dlg = true,
            "FormatDialogLauncher" => self.fmt_pending = true,
            "FormatCellsDialog" => self.fmt_dlg = true,
            "InsertRow" => {
                let (r1, _c1, r2, _c2) = self.sel_rect();
                self.structural(sheetcore::structural::Structural::InsertRows {
                    at: r1,
                    n: r2 - r1 + 1,
                });
            }
            "InsertColumn" => {
                let (_r1, c1, _r2, _c2) = self.sel_rect();
                self.structural(sheetcore::structural::Structural::InsertCols { at: c1, n: 1 });
            }
            "InsertCellsDown" => self.shift_cells(true, false),
            "InsertCellsRight" => self.shift_cells(true, true),
            "DeleteRow" => {
                let (r1, _c1, r2, _c2) = self.sel_rect();
                self.structural(sheetcore::structural::Structural::DeleteRows {
                    at: r1,
                    n: r2 - r1 + 1,
                });
            }
            "DeleteColumn" => {
                let (_r1, c1, _r2, _c2) = self.sel_rect();
                self.structural(sheetcore::structural::Structural::DeleteCols { at: c1, n: 1 });
            }
            "DeleteCellsUp" => self.shift_cells(false, false),
            "DeleteCellsLeft" => self.shift_cells(false, true),
            "RowHeight" => {
                self.size_dlg = Some((
                    false,
                    self.wb.sheet(self.sheet_idx).rows.size(self.sel.0),
                ))
            }
            "ColumnWidth" => {
                self.size_dlg = Some((
                    true,
                    self.wb.sheet(self.sheet_idx).cols.size(self.sel.1),
                ))
            }
            "AutoSum" => self.auto_sum(),
            "FillDown" => self.fill_down(),
            "ClearContent" => self.clear_all(),
            "SortFilter" => self.start_filter(),
            "FindReplace" => self.find.open = true,

            // ---------------------------------------------------- 插入
            "Hyperlink" => {
                let (r, c) = self.sel;
                let text = self.wb.sheet(self.sheet_idx).value(r, c).display();
                self.hyper_dlg = Some((String::new(), text));
            }
            "ColumnChart" => self.insert_chart_from_selection(),
            "ChartManager" => {
                self.chart_mgr = true;
                self.reload_charts();
            }
            "InsertPivot" => {
                let (r, c) = self.sel;
                let (r1, c1, r2, c2) =
                    sheetcore::sort_filter::current_region(self.wb.sheet(self.sheet_idx), r, c);
                if c2 <= c1 || r2 <= r1 {
                    self.status = "当前区域太小，选一个带表头的表格".into();
                } else {
                    self.pivot_edit = Some(sheetcore::pivot::PivotSpec {
                        source: (r1, c1, r2, c2),
                        row_field: c1,
                        col_field: None,
                        value_field: c2,
                        agg: sheetcore::pivot::Agg::Sum,
                        group: sheetcore::pivot::GroupBy::None,
                        exclude: Vec::new(),
                    });
                }
            }
            "InsertPicture" => {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("图片", &["png", "jpg", "jpeg", "gif", "bmp"])
                    .pick_file()
                {
                    match (self.wb.path.clone(), std::fs::read(&p)) {
                        (Some(out), Ok(bytes)) => {
                            let ext = p
                                .extension()
                                .map(|e| e.to_string_lossy().to_string())
                                .unwrap_or_else(|| "png".into());
                            let sheet_name = self.wb.sheet(self.sheet_idx).name.clone();
                            let (r, c) = self.sel;
                            match sheetcore::pic_write::insert_picture(
                                &out, &sheet_name, &bytes, &ext, c, r,
                            ) {
                                Ok(_) => {
                                    if let Some(list) =
                                        sheetcore::pic::load(&out).remove(&sheet_name)
                                    {
                                        self.wb.sheet_mut(self.sheet_idx).pictures = list;
                                    }
                                    self.status = "已插入图片".into();
                                }
                                Err(e) => self.status = format!("插入图片失败：{e:#}"),
                            }
                        }
                        (None, _) => self.status = "请先保存文件，再插入图片".into(),
                        (_, Err(e)) => self.status = format!("读图片失败：{e}"),
                    }
                }
            }
            "InsertTable" => self.make_table(),
            "InsertComment" | "NewComment" => {
                let (r, c) = self.sel;
                let cur = self
                    .wb
                    .sheet(self.sheet_idx)
                    .notes
                    .get(&sheetcore::model::CellRef::new(r, c))
                    .map(|n| n.text.clone())
                    .unwrap_or_default();
                self.note_edit = Some((r, c, cur));
            }
            "InsertSymbol" => self.symbol_dlg = true,
            "RefreshPivot" => self.refresh_pivot(),
            "PivotOptions" => {
                if let Some((_, spec, _)) = self.pivot_last.clone() {
                    self.pivot_edit = Some(spec);
                } else {
                    self.status = "先选中数据区域再新建透视表".into();
                }
            }
            "WorkbookLinks" => self.xlink_dlg = true,
            "InsertSlicer" => self.slicer_open(),

            // ---------------------------------------------------- 页面布局
            "PageOrientation" => self.set_page(|p| p.landscape = !p.landscape),
            "PaperSize" => self.set_page(|p| p.paper = if p.paper == 9 { 1 } else { 9 }),
            "PageMargins" => {
                let page = self.wb.sheet(self.sheet_idx).page.clone();
                let narrow = page.margins[0] < 0.5;
                let wide = page.margins[0] > 0.9;
                self.set_page(move |p| {
                    let m = if narrow {
                        [1.0, 1.0, 1.0, 1.0, 0.5, 0.5]
                    } else if wide {
                        [0.7, 0.7, 0.75, 0.75, 0.3, 0.3]
                    } else {
                        [0.25, 0.25, 0.75, 0.75, 0.3, 0.3]
                    };
                    p.margins = m;
                });
            }
            "TogglePrintGridlines" => {
                let v = !self.wb.sheet(self.sheet_idx).page.gridlines;
                self.set_page(move |p| p.gridlines = v);
            }
            "TogglePrintHeadings" => {
                let v = !self.wb.sheet(self.sheet_idx).page.headings;
                self.set_page(move |p| p.headings = v);
            }

            // ---------------------------------------------------- 公式
            "InsertFunction" => {
                self.begin_edit(None);
                if let Some(ed) = self.editing.as_mut() {
                    if !ed.buf.starts_with('=') {
                        ed.buf.insert(0, '=');
                    }
                }
            }
            "TracePrecedents" => self.trace_precedents(),
            "TraceDependents" => self.trace_dependents(),
            "RemoveArrows" => {
                self.trace_arrows.clear();
                self.status = "已移去追踪箭头".into();
            }
            "ShowFormulas" => self.show_formulas = !self.show_formulas,
            "ErrorChecking" => self.open_error_check(),
            "CalculateNow" => self.recalc_all_async(),
            "CalculateSheet" => self.recalc_sheet_async(self.sheet_idx),
            "CalcAuto" => {
                self.manual_calc = false;
                self.wb.calc_manual = false;
                self.wb.calc_pending = false;
                self.wb.calc_pending_sheets.clear();
                self.wb.dirty = true;
                self.recalc_all_async();
                self.status = "计算选项：自动".into();
            }
            "CalcManual" => {
                self.manual_calc = true;
                self.wb.calc_manual = true;
                self.wb.dirty = true;
                self.status = "计算选项：手动（按 F9 或“开始计算”重算）".into();
            }
            "WatchWindow" => self.watch_open = true,
            "DataValidation" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let cur = sheetcore::dv::find(&self.wb.sheet(self.sheet_idx).dv, r1, c1).cloned();
                let mut draft = cur.unwrap_or_default();
                draft.ranges = vec![(r1, c1, r2, c2)];
                self.dv_edit = Some(draft);
            }
            "FilterClear" => {
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.filter = None;
                sh.filter_dirty = true;
                self.wb.dirty = true;
                self.status = "已清除筛选".into();
            }
            "FilterReapply" => {
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.filter_dirty = true;
                self.status = "已重新应用筛选".into();
            }
            "HeaderFooter" => self.hf_dlg = true,
            // ---------------------------------------------------- 文本框
            "InsertTextBox" => {
                // 放在当前选区左上角，默认 200×60，插完直接打开编辑框
                let (r, c) = self.sel;
                let (cols, rows) = self.view_axes();
                let x = cols.pos(c) + 4.0;
                let y = rows.pos(r) + 2.0;
                self.tb_create(x, y, 200.0, 60.0);
                if let Some(i) = self.tb_sel {
                    self.tb_open_edit(i);
                }
            }
            "DrawTextBox" => {
                self.tb_insert = true;
                self.status = "绘制文本框：在表格上拖出一个框（单击则用默认大小）".into();
            }
            "EditTextBox" => match self.tb_sel {
                Some(i) => self.tb_open_edit(i),
                None => self.status = "先选中一个文本框".into(),
            },
            "DeleteTextBox" => match self.tb_sel {
                Some(i) => self.tb_delete(i),
                None => self.status = "先选中一个文本框".into(),
            },
            // ---------------------------------------------------- 迷你图
            "SparkLine" => self.spark_open_create(sheetcore::sparkline::SparkKind::Line),
            "SparkColumn" => self.spark_open_create(sheetcore::sparkline::SparkKind::Column),
            "SparkWinLoss" => self.spark_open_create(sheetcore::sparkline::SparkKind::WinLoss),
            "SparkEdit" => self.spark_open_edit(),
            "SparkClear" => self.spark_delete_in_sel(),
            "SparkTypeLine" => {
                self.spark_apply_sel(|s| s.kind = sheetcore::sparkline::SparkKind::Line)
            }
            "SparkTypeColumn" => {
                self.spark_apply_sel(|s| s.kind = sheetcore::sparkline::SparkKind::Column)
            }
            "SparkTypeWinLoss" => {
                self.spark_apply_sel(|s| s.kind = sheetcore::sparkline::SparkKind::WinLoss)
            }
            "SparkMarkers" => self.spark_apply_sel(|s| s.markers = !s.markers),
            "SparkHigh" => self.spark_apply_sel(|s| s.high = !s.high),
            "SparkLow" => self.spark_apply_sel(|s| s.low = !s.low),
            "SparkFirst" => self.spark_apply_sel(|s| s.first = !s.first),
            "SparkLast" => self.spark_apply_sel(|s| s.last = !s.last),
            "SparkNegative" => self.spark_apply_sel(|s| s.negative = !s.negative),
            "SparkAxis" => self.spark_apply_sel(|s| s.axis = !s.axis),
            "NameManager" => self.name_mgr = true,

            // ---------------------------------------------------- 数据
            "GoalSeek" => {
                let (r, c) = self.sel;
                self.goal_target = cell_name(r, c);
                self.goal_goal = self
                    .cell_num(r, c)
                    .map(|v| format!("{v}"))
                    .unwrap_or_default();
                self.goal_input.clear();
                self.goal_msg.clear();
                self.goal_dlg = true;
            }
            "DataTable" => {
                self.dtable_row.clear();
                self.dtable_col.clear();
                self.dtable_msg.clear();
                self.dtable_dlg = true;
            }
            "ScenarioManager" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                self.scen_range = if r1 == r2 && c1 == c2 {
                    cell_name(r1, c1)
                } else {
                    format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
                };
                self.scen_name.clear();
                self.scen_comment.clear();
                self.scen_result.clear();
                self.scen_msg.clear();
                self.scen_sel = None;
                self.scen_dlg = true;
            }
            "GroupSelection" => self.group_selection(),
            "UngroupSelection" => self.ungroup_selection(),
            "OutlineSettings" => self.outline_set_dlg = true,
            "HideDetail" => self.toggle_detail(true),
            "ShowDetail" => self.toggle_detail(false),
            "ImportCsv" => self.import_csv(),
            "ImportWorkbook" => self.import_workbook(),
            "LinkSheet" => self.link_dlg = Some(String::new()),
            "TextToColumns" => self.open_data_dlg(1),
            "RefreshImport" => self.refresh_import(),
            "SortAscending" => {
                let c = self.sel.1;
                self.do_sort(c, true);
            }
            "SortDescending" => {
                let c = self.sel.1;
                self.do_sort(c, false);
            }
            "Filter" => self.start_filter(),
            "MultiSort" => self.open_data_dlg(0),
            "RemoveDuplicates" => self.run_dedup(),
            // ---------------------------------------------------- 数据分析四件套
            "ForecastSheet" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let range = if r1 == r2 && c1 == c2 {
                    // 单格：取当前区域
                    let (a, b) = (self.sel.0, self.sel.1);
                    let reg = sheetcore::sort_filter::current_region(
                        self.wb.sheet(self.sheet_idx),
                        a,
                        b,
                    );
                    format!("{}:{}", cell_name(reg.0, reg.1), cell_name(reg.2, reg.3))
                } else {
                    format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
                };
                self.fc_dlg = Some(crate::ForecastDlg {
                    range,
                    periods: 6,
                    msg: String::new(),
                });
            }
            "Subtotal" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let range = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
                self.st_dlg = Some(crate::SubtotalDlg {
                    range,
                    key_col: col_label(c1),
                    funcs: sheetcore::subtotal::SubFn::Sum,
                    value_cols: if c2 > c1 {
                        col_label(c1 + 1)
                    } else {
                        String::new()
                    },
                    msg: String::new(),
                });
            }
            "FlashFill" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                self.ff_dlg = Some(crate::FlashDlg {
                    range: format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2)),
                    src_col: col_label(c1),
                    dst_col: col_label(c1 + 1),
                    aux_col: if c2 > c1 + 1 { col_label(c2) } else { String::new() },
                    msg: String::new(),
                });
            }
            "Consolidate" => {
                self.cs_dlg = Some(crate::ConsDlg {
                    funcs: sheetcore::consolidate::ConsFn::Sum,
                    sources: String::new(),
                    row_labels: true,
                    col_labels: true,
                    msg: String::new(),
                });
            }
            "CFManager" => self.cf_mgr = true,

            // ---------------------------------------------------- 审阅
            "Spelling" => self.open_proof(),
            "ConvertTraditional" | "ToGeneric" => {
                self.zh_convert(true, sheetcore::zhconv::Variant::Generic)
            }
            "ToTaiwan" => self.zh_convert(true, sheetcore::zhconv::Variant::Taiwan),
            "ToHongKong" => self.zh_convert(true, sheetcore::zhconv::Variant::HongKong),
            "ZhPreview" => {
                self.zh_preview_variant = sheetcore::zhconv::Variant::Generic;
                self.zh_preview_scan();
                self.zh_preview_dlg = true;
            }
            "ConvertSimplified" => {
                self.zh_convert(false, sheetcore::zhconv::Variant::Generic)
            }
            "RichText" => {
                let (r, c) = self.sel;
                let runs = self
                    .wb
                    .sheet(self.sheet_idx)
                    .rich
                    .get(&(r, c))
                    .cloned()
                    .unwrap_or_else(|| {
                        let t = self.wb.sheet(self.sheet_idx).value(r, c).display();
                        vec![sheetcore::richtext::RichRun::plain(t)]
                    });
                self.rich_dlg = Some(crate::RichDlg {
                    r,
                    c,
                    runs,
                    sel: (0, 0),
                    color: [0, 0, 0],
                    size: 0.0,
                });
            }
            "ProtectSheet" => {
                if self.wb.sheet(self.sheet_idx).protection.is_some() {
                    self.protect_dlg = Some(crate::ProtectDlg {
                        unprotect: true,
                        ..Default::default()
                    });
                } else {
                    self.protect_dlg = Some(crate::ProtectDlg::default());
                }
            }
            "TrackChanges" => self.rev_open = true,
            // 审阅：显示更改（复用修订日志）、批注导航、辅助功能检查器
            "ShowChanges" => {
                self.rev_open = true;
                self.status = "显示更改：列表里点「撤回」可还原".into();
            }
            "PrevComment" => self.note_step(false),
            "NextComment" => self.note_step(true),
            "ShowComment" => self.note_show(),
            "A11yChecker" => {
                self.a11y_scan();
                self.a11y_open = true;
            }
            "AutoShare" => self.auto_share = !self.auto_share,

            // ---------------------------------------------------- 视图
            "ShowGridlines" => self.show_gridlines = !self.show_gridlines,
            "ShowHeadings" => self.show_headings = !self.show_headings,
            "ShowFormulaBar" => self.show_formula_bar = !self.show_formula_bar,
            "TextSpill" => self.text_spill = !self.text_spill,
            "Zoom100" => {
                self.zoom = 1.0;
                ctx.set_zoom_factor(1.0);
            }
            "ToggleFileTree" => self.tree_open = !self.tree_open,
            "ZoomIn" => {
                self.zoom = (self.zoom + 0.1).min(2.5);
                ctx.set_zoom_factor(self.zoom);
            }
            "ZoomOut" => {
                self.zoom = (self.zoom - 0.1).max(0.5);
                ctx.set_zoom_factor(self.zoom);
            }
            "FreezeTopRow" => {
                let on = self.wb.sheet(self.sheet_idx).freeze == Some((1, 0));
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.freeze = if on { None } else { Some((1, 0)) };
                sh.freeze_dirty = true;
                self.wb.dirty = true;
            }
            "FreezeFirstColumn" => {
                let on = self.wb.sheet(self.sheet_idx).freeze == Some((0, 1));
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.freeze = if on { None } else { Some((0, 1)) };
                sh.freeze_dirty = true;
                self.wb.dirty = true;
            }
            "ViewNew" => {
                self.view_new = Some(format!("视图 {}", self.views.len() + 1));
            }
            "ViewKeep" => match self.view_active {
                Some(i) => {
                    let name = self.views[i].name.clone();
                    let v = self.view_capture(&name);
                    self.views[i] = v;
                    ui_prefs::save_views(&self.views_key(), &self.views);
                    self.status = format!("已保留「{name}」的当前状态");
                }
                None => self.status = "先切换到一个视图，再点「保留」".into(),
            },
            "ViewExit" => {
                self.view_active = None;
                self.status = "已退出工作表视图（不改变当前缩放/冻结）".into();
            }
            "ViewSwitch" => self.views_open = true,
            "HideSheet" => self.hide_current_sheet(),
            "UnhideSheet" => self.unhide_open = true,
            "FreezePanes" => {
                // 冻结当前单元格上方的行与左侧的列（和 Excel 的「冻结窗格」一致）
                let (r, c) = self.sel;
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.freeze = if r == 0 && c == 0 { None } else { Some((r, c)) };
                sh.freeze_dirty = true;
                self.wb.dirty = true;
                self.status = if r == 0 && c == 0 {
                    "已取消冻结".into()
                } else {
                    format!("冻结在 {}：上方 {} 行、左侧 {} 列", cell_name(r, c), r, c)
                };
            }
            "Unfreeze" => {
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.freeze = None;
                sh.freeze_dirty = true;
                self.wb.dirty = true;
            }
            "PrintAreaSet" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.page.print_area = Some((r1, c1, r2, c2));
                self.wb.dirty = true;
                self.status = format!(
                    "打印区域设为 {}-{}",
                    cell_name(r1, c1),
                    cell_name(r2, c2)
                );
            }
            "PrintAreaClear" => {
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.page.print_area = None;
                self.wb.dirty = true;
                self.status = "已取消打印区域".into();
            }
            "PageBreakInsert" => {
                let r = self.sel.0;
                let sh = self.wb.sheet_mut(self.sheet_idx);
                if !sh.page.row_breaks.contains(&r) {
                    sh.page.row_breaks.push(r);
                    sh.page.row_breaks.sort_unstable();
                    sh.page.row_breaks.dedup();
                }
                self.wb.dirty = true;
                self.status = format!("已在第 {} 行插入分页符", r + 1);
            }
            "PageBreakRemove" => {
                let r = self.sel.0;
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.page.row_breaks.retain(|x| *x != r);
                self.wb.dirty = true;
                self.status = format!("已删除第 {} 行的分页符", r + 1);
            }
            "ZoomToSelection" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let sh = self.wb.sheet(self.sheet_idx);
                let w: f32 = (c1..=c2).map(|c| sh.cols.size(c)).sum();
                let h: f32 = (r1..=r2).map(|r| sh.rows.size(r)).sum();
                let view = self.last_view;
                if w > 1.0 && h > 1.0 && view.x > 10.0 && view.y > 10.0 {
                    let z = ((view.x - 40.0) / w).min((view.y - 40.0) / h).clamp(0.5, 2.5);
                    self.zoom = z;
                    self.scroll = egui::Vec2::new(c1 as f32 * 8.0, r1 as f32 * 4.0);
                    ctx.set_zoom_factor(z);
                    self.status = format!("缩放到选定区域：{:.0}%", z * 100.0);
                }
            }
            "ZoomDialog" => self.zoom_dlg = Some(self.zoom),
            "NormalView" => {
                self.view_mode = 0;
                self.scroll = egui::Vec2::ZERO;
            }
            "PageLayoutView" => {
                self.view_mode = 1;
                self.scroll = egui::Vec2::ZERO;
            }
            "PageBreakPreview" => {
                self.view_mode = 2;
                self.scroll = egui::Vec2::ZERO;
            }

            // ---------------------------------------------------- 帮助
            "Help" | "About" => self.about_open = true,

            // ---------------------------------------------------- 表设计
            "TablePivot" => {
                if let Some(t) = self.table_at_sel() {
                    let (r1, c1, r2, c2) = t.range;
                    self.pivot_edit = Some(sheetcore::pivot::PivotSpec {
                        source: (r1, c1, r2, c2),
                        row_field: c1,
                        col_field: None,
                        group: sheetcore::pivot::GroupBy::None,
                        exclude: Vec::new(),
                        value_field: c2,
                        agg: sheetcore::pivot::Agg::Sum,
                    });
                }
            }
            "TableSelect" => {
                if let Some(t) = self.table_at_sel() {
                    self.sel = (t.range.0, t.range.1);
                    self.sel_end = (t.range.2, t.range.3);
                    self.status = format!("已选中表格 {}", t.display);
                }
            }
            "TableToRange" => {
                if let Some(t) = self.table_at_sel() {
                    let changed =
                        sheetcore::table::convert_table_to_range(&mut self.wb, &t.name);
                    if changed {
                        self.wb.dirty = true;
                        self.doc_gen = self.doc_gen.wrapping_add(1);
                        self.ribbon_tab = 1;
                        // 公式文本已从结构化引用改成 A1，必须让依赖图与缓存值跟上；
                        // 否则转换后的单元格仍显示旧缓存。
                        self.recalc_all_async();
                        self.status = "已转换为普通区域".into();
                    } else {
                        self.status = "没有可转换的表格".into();
                    }
                }
            }
            "TableFillCalculated" => self.fill_table_calculated_column(),
            "ToggleTotals" => {
                if let Some(t) = self.table_at_sel() {
                    self.toggle_totals_row(&t.name);
                }
            }

            // ---------------------------------------------------- 图表工具
            "ChartEdit" => {
                if let Some(ch) = self.chart_at_sel() {
                    self.chart_edit = Some(ch);
                }
            }

            "Noop" | "PhoneticGuide" | "AddIns" => {}
            "DoubleUnderline" => self.apply_style(|s| s.underline = true),
            "TextVertical" => self.apply_style(|s| s.rotation = 90),
            "TextRotateUp" => self.apply_style(|s| s.rotation = 45),
            "TextRotateDown" => self.apply_style(|s| s.rotation = -45),
            "AccountingStyle" => self.apply_style(|s| {
                s.format = "_-* ¥#,##0.00_-;-* ¥#,##0.00_-;_-* \"-\"??_-;_-@_-".into()
            }),
            "AutoAverage" => self.auto_func("AVERAGE"),
            "AutoCount" => self.auto_func("COUNT"),
            "FillRight" => self.fill_cmd(false),
            "ClearValues" => self.clear_values(),
            "StyleNormal" => self.apply_style_preset("normal"),
            "StyleTitle" => self.apply_style_preset("title"),
            "StyleHeading" => self.apply_style_preset("heading"),
            "StyleGood" => self.apply_style_preset("good"),
            "StyleBad" => self.apply_style_preset("bad"),

            // 其余（字体/字号/下拉里带值的项）已在上面处理
            _ => {
                if std::env::var("SC_RIBBON_LOG").is_ok() {
                    eprintln!("[ribbon] 未实现的命令：{action}");
                }
            }
        }
    }
}
