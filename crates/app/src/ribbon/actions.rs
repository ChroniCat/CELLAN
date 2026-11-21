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
        "0%" => "百分比",
        "0.0%" => "百分比 (0.0%)",
        "[DBNum2][$¥-804]#,##0.00" => "人民币大写",
        "[DBNum2]G/通用格式" => "中文大写数字",
        "[DBNum1]G/通用格式" => "中文小写数字",
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
            // 剪贴板空着时「粘贴」系列灰掉（Excel 行为）；剪切/复制对空单元格也有效，保持可用
            "Paste" | "PasteMenu" | "PasteSpecial" | "PasteSplit" | "PasteDirectItem"
            | "PasteValuesItem" | "PasteFormulasItem" | "PasteTransposeItem"
            | "PasteFormatsItem" | "PasteNoBordersItem" | "PasteColWidthsItem"
            | "PasteLinkItem" | "PasteAsPicItem" => {
                st.enabled = self.clip.is_some()
                    || self.clip_pic.is_some()
                    || crate::office_clipboard::has_pasteable();
                if !st.enabled {
                    st.tip = Some("剪贴板里没有可粘贴的内容".into());
                }
            }
            "FmtLockCells" => st.checked = cur.locked,
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
            "StrikeThrough" => st.checked = cur.strikethrough,
            "AlignTop" => st.checked = cur.valign == 1,
            "AlignMiddle" => st.checked = cur.valign == 2,
            "AlignBottom" => st.checked = cur.valign == 3,
            "AlignLeft" => st.checked = cur.halign == 1,
            "AlignCenter" => st.checked = cur.halign == 2,
            "AlignRight" => st.checked = cur.halign == 3,
            "WrapText" => st.checked = cur.wrap,
            "MergeCenter" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let sh = self.wb.sheet(self.sheet_idx);
                st.checked = sh
                    .merges
                    .iter()
                    .any(|m| m.0 == r1 && m.1 == c1 && m.2 == r2 && m.3 == c2)
                    || (r1 == r2 && c1 == c2 && sh.merge_at(r1, c1).is_some());
                st.enabled = !self.is_edit_blocked();
            }
            "MergeCenterItem" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let sh = self.wb.sheet(self.sheet_idx);
                st.checked = sh
                    .merges
                    .iter()
                    .any(|m| m.0 == r1 && m.1 == c1 && m.2 == r2 && m.3 == c2)
                    || (r1 == r2 && c1 == c2 && sh.merge_at(r1, c1).is_some());
                st.enabled = (st.checked || !(r1 == r2 && c1 == c2)) && !self.is_edit_blocked();
                if !st.enabled && r1 == r2 && c1 == c2 {
                    st.tip = Some("请选择两个以上单元格再合并".into());
                }
            }
            "MergeCellsItem" | "MergeCells" => {
                st.enabled = self.can_merge_selection() && !self.is_edit_blocked();
                if !st.enabled {
                    st.tip = Some("请选择两个以上未整块合并的单元格".into());
                }
            }
            "UnmergeCellsItem" | "UnmergeCells" => {
                st.enabled = self.selection_has_merge() && !self.is_edit_blocked();
                if !st.enabled {
                    st.tip = Some("所选区域没有合并单元格".into());
                }
            }
            "InsertComment" | "NewComment" => {
                st.enabled = !self.is_edit_blocked();
            }
            "ClearCommentsItem" | "ClearComments" => {
                st.enabled = self.selection_has_comment() && !self.is_edit_blocked();
                if !st.enabled {
                    st.tip = Some("所选区域没有批注".into());
                }
            }
            "PrevComment" | "NextComment" | "ShowShowCommentBtn" | "ShowComment"
            | "SelectComments" => {
                st.enabled = !self.wb.sheet(self.sheet_idx).notes.is_empty();
                if !st.enabled {
                    st.tip = Some("当前工作表没有批注".into());
                }
            }
            "FillColor" => {
                let f = cur.fill.unwrap_or(self.last_fill_color);
                st.swatch = Some(egui::Color32::from_rgb(f[0], f[1], f[2]));
            }
            "FontColor" => {
                let c = self.last_font_color;
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
            "CompareSync" => st.checked = self.compare_sync,
            "NormalView" => st.checked = self.view_mode == 0,
            "PageLayoutView" => st.checked = self.view_mode == 1,
            "PageBreakPreview" => st.checked = self.view_mode == 2,
            "ToggleSpotlight" => st.checked = self.prefs.spotlight,
            "SplitPanes" | "ViewSplitPanes" => st.checked = self.split_panes.is_some(),
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
        // UI-22.1：功能区点按与命令搜索共用同一份使用频率
        crate::cmdusage::touch(action);
        self.unhandled_action = None;
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
                self.set_zoom(z);
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
            "Undo" => self.do_undo(false),
            "Redo" => self.do_undo(true),
            "NewTab" => self.open_in_new_tab(std::path::PathBuf::new()),
            "OpenInNewTab" => {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Excel 工作簿", &["xlsx", "xlsm", "csv"])
                    .pick_file()
                {
                    self.open_any(p);
                }
            }
            "CloseTab" => {
                let idx = self.active_tab;
                self.request_close_tab(idx);
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
            "PasteValues" => self.paste_mode_exec(crate::wps_menus::PasteMode::Values, ctx),
            "PasteValuesNumFmt" => self.paste_mode_exec(crate::wps_menus::PasteMode::ValuesNumFmt, ctx),
            "PasteValuesSourceFmt" => self.paste_mode_exec(crate::wps_menus::PasteMode::ValuesSourceFmt, ctx),
            "PasteFormulas" => self.paste_mode_exec(crate::wps_menus::PasteMode::Formulas, ctx),
            "PasteFormulasNumFmt" => self.paste_mode_exec(crate::wps_menus::PasteMode::FormulasNumFmt, ctx),
            "PasteFormats" => self.paste_mode_exec(crate::wps_menus::PasteMode::Formats, ctx),
            "PasteNoBorders" => self.paste_mode_exec(crate::wps_menus::PasteMode::NoBorders, ctx),
            "PasteColWidths" => self.paste_mode_exec(crate::wps_menus::PasteMode::ColWidths, ctx),
            "PasteTranspose" => self.paste_mode_exec(crate::wps_menus::PasteMode::Transpose, ctx),
            "PasteLink" => self.paste_mode_exec(crate::wps_menus::PasteMode::Link, ctx),
            "PasteAsPicture" => self.paste_mode_exec(crate::wps_menus::PasteMode::AsPicture, ctx),
            "PasteSpecial" => self.paste_special = true,
            "Cut" => self.clip_copy(true, ctx),
            "Copy" => self.clip_copy(false, ctx),
            "CopyValuesText" => self.copy_values_text_only(ctx),
            "CopyAsPicture" => self.copy_selection_as_picture(ctx),
            "FormatPainter" => {
                let (r1, c1, mut r2, mut c2) = self.sel_rect();
                if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
                    if let Some((_, _, ur2, uc2)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                        r2 = r2.min(ur2.max(r1));
                        c2 = c2.min(uc2.max(c1));
                    } else {
                        r2 = r1;
                        c2 = c1;
                    }
                }
                if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
                    self.status = "源区域太大（超过 20 万格），请缩小选区后再用格式刷".into();
                    return;
                }
                let mut styles = Vec::with_capacity((r2 - r1 + 1) as usize);
                {
                    let sheet = self.wb.sheet(self.sheet_idx);
                    for r in r1..=r2 {
                        let mut row = Vec::with_capacity((c2 - c1 + 1) as usize);
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
            "StrikeThrough" => {
                let v = !self.current_style().strikethrough;
                self.apply_style(move |s| s.strikethrough = v);
            }
            "BordersDefault" => {
                let act = self.last_border_action.clone();
                self.ribbon_action(&act, ctx);
            }
            "BordersAll" => {
                self.last_border_action = "BordersAll".into();
                let bc = self.last_border_color;
                self.apply_style(move |s| s.set_all_borders(1, bc));
            }
            "BorderBottom" => {
                self.last_border_action = "BorderBottom".into();
                let bc = self.last_border_color;
                self.apply_style(move |s| {
                    s.border[sheetcore::style::BorderSide::Bottom.idx()] = sheetcore::style::Border {
                        style: 1,
                        color: bc,
                    }
                });
            }
            "BorderTop" => {
                self.last_border_action = "BorderTop".into();
                let bc = self.last_border_color;
                self.apply_style(move |s| {
                    s.border[sheetcore::style::BorderSide::Top.idx()] = sheetcore::style::Border {
                        style: 1,
                        color: bc,
                    }
                });
            }
            "BorderLeft" => {
                self.last_border_action = "BorderLeft".into();
                let bc = self.last_border_color;
                self.apply_style(move |s| {
                    s.border[sheetcore::style::BorderSide::Left.idx()] = sheetcore::style::Border {
                        style: 1,
                        color: bc,
                    }
                });
            }
            "BorderRight" => {
                self.last_border_action = "BorderRight".into();
                let bc = self.last_border_color;
                self.apply_style(move |s| {
                    s.border[sheetcore::style::BorderSide::Right.idx()] = sheetcore::style::Border {
                        style: 1,
                        color: bc,
                    }
                });
            }
            "BorderOuter" => {
                self.last_border_action = "BorderOuter".into();
                self.border_outer(1);
            }
            "BorderThickOuter" => {
                self.last_border_action = "BorderThickOuter".into();
                self.border_outer(2);
            }
            "BorderThickBottom" => {
                self.last_border_action = "BorderThickBottom".into();
                let bc = self.last_border_color;
                self.apply_style(move |s| {
                    s.border[sheetcore::style::BorderSide::Bottom.idx()] = sheetcore::style::Border {
                        style: 2,
                        color: bc,
                    }
                });
            }
            "BorderDoubleBottom" => {
                self.last_border_action = "BorderDoubleBottom".into();
                let bc = self.last_border_color;
                self.apply_style(move |s| {
                    s.border[sheetcore::style::BorderSide::Bottom.idx()] = sheetcore::style::Border {
                        style: 4,
                        color: bc,
                    }
                });
            }
            "BorderInsideH" => {
                self.last_border_action = "BorderInsideH".into();
                let (r1, _c1, r2, _c2) = self.sel_rect();
                let bc = self.last_border_color;
                self.apply_style_xy(move |s, r, _c| {
                    if r > r1 && r <= r2 {
                        s.border[sheetcore::style::BorderSide::Top.idx()] = sheetcore::style::Border {
                            style: 1,
                            color: bc,
                        };
                    }
                    if r >= r1 && r < r2 {
                        s.border[sheetcore::style::BorderSide::Bottom.idx()] = sheetcore::style::Border {
                            style: 1,
                            color: bc,
                        };
                    }
                });
            }
            "BorderInsideV" => {
                self.last_border_action = "BorderInsideV".into();
                let (_r1, c1, _r2, c2) = self.sel_rect();
                let bc = self.last_border_color;
                self.apply_style_xy(move |s, _r, c| {
                    if c > c1 && c <= c2 {
                        s.border[sheetcore::style::BorderSide::Left.idx()] = sheetcore::style::Border {
                            style: 1,
                            color: bc,
                        };
                    }
                    if c >= c1 && c < c2 {
                        s.border[sheetcore::style::BorderSide::Right.idx()] = sheetcore::style::Border {
                            style: 1,
                            color: bc,
                        };
                    }
                });
            }
            "BorderNone" => {
                self.apply_style(|s| s.border = [sheetcore::style::Border::default(); 4])
            }
            "FillColor" => self.apply_fill_color(Some(self.last_fill_color)),
            "FontColor" => self.apply_font_color(self.last_font_color),
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
                let was_merged = self.ctl_state("MergeCenter").checked;
                let on = !was_merged;
                self.toggle_merge(on);
                if on && self.merge_pending.is_none() && self.ctl_state("MergeCenter").checked {
                    self.apply_style(|s| s.halign = 2);
                }
            }
            "MergeCells" => self.toggle_merge(true),
            "UnmergeCells" => self.toggle_merge(false),
            "CenterAcross" => self.apply_style(|s| s.halign = 4),
            "NumberFormatApply" => {}
            "CurrencyStyle" => self.apply_style(|s| s.format = "¥#,##0.00".into()),
            "PercentStyle" => self.apply_style(|s| s.format = "0%".into()),
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
                let (_r1, c1, _r2, c2) = self.sel_rect();
                self.structural(sheetcore::structural::Structural::InsertCols { at: c1, n: c2 - c1 + 1 });
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
                let (_r1, c1, _r2, c2) = self.sel_rect();
                self.structural(sheetcore::structural::Structural::DeleteCols { at: c1, n: c2 - c1 + 1 });
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
            "AutofitRowHeight" => self.autofit_sel_rows(ctx),
            "AutofitColWidth" => self.autofit_sel_cols(ctx),
            "FillDown" => self.fill_cmd(true),
            "FillUp" => self.fill_up(),
            "FillLeft" => self.fill_left(),
            "FillSeriesDialog" => self.open_fill_series_dlg(),
            "FillSeqLinear" => self.fill_quick_series(crate::wps_menus::SeriesPreset::Linear),
            "FillSeqWeekday" => self.fill_quick_series(crate::wps_menus::SeriesPreset::Weekday),
            "FillSeqMonth" => self.fill_quick_series(crate::wps_menus::SeriesPreset::Month),
            "FillSeqYear" => self.fill_quick_series(crate::wps_menus::SeriesPreset::Year),
            "InsertToday" => self.insert_current_datetime(false),
            "InsertNow" => self.insert_current_datetime(true),
            "ClearContent" => self.clear_all(),
            "ClearComments" => self.clear_comments_in_sel(),
            "ClearHyperlinks" => self.clear_hyperlinks_in_sel(),
            "ClearCfAndDv" => { self.clear_cf(); self.clear_dv_in_sel(); }
            "SortFilter" => self.start_filter(),
            "FilterByCell" => self.filter_by_cell_value(),
            "FindReplace" => self.find_open_new_session(),
            "FindOpen" => { self.find.tab = 0; self.find_open_new_session(); }
            "ReplaceOpen" => { self.find.tab = 1; self.find_open_new_session(); }
            "GotoOpen" => self.open_goto_dlg(0),
            "GotoSpecialOpen" => self.open_goto_dlg(1),
            "SelectFormulas" => self.select_special(crate::wps_menus::SpecialTarget::Formulas),
            "SelectConstants" => self.select_special(crate::wps_menus::SpecialTarget::Constants),
            "SelectBlanks" => self.select_special(crate::wps_menus::SpecialTarget::Blanks),
            "SelectErrors" => self.select_special(crate::wps_menus::SpecialTarget::Errors),
            "SelectComments" => self.select_special(crate::wps_menus::SpecialTarget::Comments),
            "SelectCondFormats" => self.select_special(crate::wps_menus::SpecialTarget::CondFormats),
            "SelectDataValidation" => self.select_special(crate::wps_menus::SpecialTarget::DataValidation),

            // ---------------------------------------------------- 插入
            "Hyperlink" => {
                let (r, c) = self.sel;
                let text = self.wb.sheet(self.sheet_idx).value(r, c).display();
                self.hyper_dlg = Some((String::new(), text));
            }
            "ColumnChart" => self.insert_chart_from_selection(),
            "LineChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Line),
            "PieChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Pie),
            "BarChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Bar),
            "BarStackedChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::BarStacked),
            "AreaChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Area),
            "DoughnutChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Doughnut),
            "ScatterChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Scatter),
            "RadarChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Radar),
            "ComboChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Combo),
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
                    self.insert_picture_file(&p);
                }
            }
            "InsertTable" => self.make_table(),
            "InsertComment" | "NewComment" => {
                self.open_note_editor(Some(ctx));
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
            // UI-26：页面设置入口（功能区/命令搜索/探针共用）
            "PageSetup" => self.open_page_setup(),
            // UI-26：方向/纸型/边距不再循环切换，统一进页面设置做明确选择。
            "PageOrientation" | "PaperSize" | "PageMargins" => self.open_page_setup(),
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
                // UI-11.4：打开函数向导（搜索 + 参数填空），确定后插到光标处
                self.open_fn_wizard();
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
                self.cancel_silent_recalc();
                if self.tasks.recalc_pending() {
                    self.wb.calc_pending = true;
                    self.wb.calc_pending_sheets.extend(0..self.wb.sheets.len());
                }
                self.manual_calc = true;
                self.wb.calc_manual = true;
                self.wb.dirty = true;
                self.status = "计算选项：手动（按 F9 或“开始计算”重算）".into();
            }
            "WatchWindow" => self.watch_open = true,
            // UI-30：旧「数据验证」菜单里的圈释/清除等，从功能区打开同一套窗口
            "DvQuick" => self.dv_quick = true,
            "CfQuick" => self.cf_quick = true,
            "DataValidation" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let cur = sheetcore::dv::find(&self.wb.sheet(self.sheet_idx).dv, r1, c1).cloned();
                let mut draft = cur.unwrap_or_default();
                draft.ranges = vec![(r1, c1, r2, c2)];
                self.dv_edit = Some(draft);
            }
            "FilterClear" => {
                self.clear_filter_criteria();
            }
            "FilterReapply" => {
                self.reapply_filter();
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
                let (r1, c1, mut r2, mut c2) = self.sel_rect();
                if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 512 {
                    if let Some((_, _, ur2, uc2)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                        r2 = r2.min(ur2.max(r1)).min(r1 + 31);
                        c2 = c2.min(uc2.max(c1)).min(c1 + 15);
                    } else {
                        r2 = r1;
                        c2 = c1;
                    }
                }
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
            "RemoveDuplicates" => self.open_dedup_dialog(),
            // ---------------------------------------------------- 数据分析四件套
            "ForecastSheet" => {
                let (r1, c1, mut r2, mut c2) = self.sel_rect();
                if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
                    if let Some((_, _, ur2, uc2)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                        r2 = r2.min(ur2.max(r1));
                        c2 = c2.min(uc2.max(c1));
                    }
                }
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
                let (r1, c1, mut r2, mut c2) = self.sel_rect();
                if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
                    if let Some((_, _, ur2, uc2)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                        r2 = r2.min(ur2.max(r1));
                        c2 = c2.min(uc2.max(c1));
                    }
                }
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
                let (r1, c1, mut r2, mut c2) = self.sel_rect();
                if (r2 - r1 + 1) as u64 > 200_000 {
                    if let Some((_, _, ur2, uc2)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                        r2 = r2.min(ur2.max(r1));
                        c2 = c2.min(uc2.max(c1));
                    }
                }
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
            // UI-30：宏的三个入口从旧「审阅」菜单搬进 Ribbon（查看只读、运行走临时副本）
            "MacroView" => {
                if self.wb.has_vba {
                    self.open_macro_view();
                } else {
                    self.status = "当前工作簿没有 VBA 宏，先打开一个 .xlsm 文件".into();
                }
            }
            "MacroRun" => {
                if self.wb.has_vba {
                    self.macro_run_dlg = Some(String::new());
                } else {
                    self.status = "当前工作簿没有 VBA 宏，没有可运行的宏".into();
                }
            }
            "MacroExport" => {
                if self.wb.has_vba {
                    self.export_macro_part();
                } else {
                    self.status = "当前工作簿没有 VBA 宏部件可导出".into();
                }
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
            "Zoom100" => self.set_zoom(1.0),
            "ToggleFileTree" => self.tree_open = !self.tree_open,
            "ZoomIn" => self.set_zoom(self.zoom + 0.1),
            "ZoomOut" => self.set_zoom(self.zoom - 0.1),
            "FreezeTopRow" => {
                self.split_panes = None;
                self.split_scroll = egui::Vec2::ZERO;
                let on = self.wb.sheet(self.sheet_idx).freeze == Some((1, 0));
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.freeze = if on { None } else { Some((1, 0)) };
                sh.freeze_dirty = true;
                self.wb.dirty = true;
            }
            "FreezeFirstColumn" => {
                self.split_panes = None;
                self.split_scroll = egui::Vec2::ZERO;
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
                // 冻结当前单元格上方的行与左侧的列（和 Excel 的「冻结窗格」一致，与拆分互斥）
                self.split_panes = None;
                self.split_scroll = egui::Vec2::ZERO;
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
            // UI-30：旧「页面布局」菜单里的并排比较，搬到视图页签的窗口组
            "CompareBooks" => self.open_compare(),
            "CompareSync" => {
                self.compare_sync = !self.compare_sync;
                self.status = if self.compare_sync {
                    "并排比较：同步滚动已打开".into()
                } else {
                    "并排比较：同步滚动已关闭".into()
                };
            }
            "Unfreeze" => {
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.freeze = None;
                sh.freeze_dirty = true;
                self.wb.dirty = true;
            }
            "PrintAreaSet" => {
                let (r1, c1, mut r2, mut c2) = self.sel_rect();
                if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 500_000 {
                    if let Some((_, _, ur2, uc2)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                        r2 = r2.min(ur2.max(r1));
                        c2 = c2.min(uc2.max(c1));
                    } else {
                        r2 = r1;
                        c2 = c1;
                    }
                }
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
                // Excel：在活动格左上方同时插入水平+垂直分页符
                let (r, c) = self.sel;
                let sh = self.wb.sheet_mut(self.sheet_idx);
                if r > 0 && !sh.page.row_breaks.contains(&r) {
                    sh.page.row_breaks.push(r);
                    sh.page.row_breaks.sort_unstable();
                    sh.page.row_breaks.dedup();
                }
                if c > 0 && !sh.page.col_breaks.contains(&c) {
                    sh.page.col_breaks.push(c);
                    sh.page.col_breaks.sort_unstable();
                    sh.page.col_breaks.dedup();
                }
                self.wb.dirty = true;
                self.status = format!("已在第 {} 行、第 {} 列前插入分页符", r + 1, c + 1);
            }
            "PageBreakRemove" => {
                let (r, c) = self.sel;
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.page.row_breaks.retain(|x| *x != r);
                sh.page.col_breaks.retain(|x| *x != c);
                self.wb.dirty = true;
                self.status = format!("已删除第 {} 行、第 {} 列的分页符", r + 1, c + 1);
            }
            "ZoomToSelection" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let sh = self.wb.sheet(self.sheet_idx);
                let w: f32 = sh.cols.pos(c2.saturating_add(1)) - sh.cols.pos(c1);
                let h: f32 = sh.rows.pos(r2.saturating_add(1)) - sh.rows.pos(r1);
                let view = self.last_view;
                if w > 1.0 && h > 1.0 && view.x > 10.0 && view.y > 10.0 {
                    let z = ((view.x - 40.0) / w).min((view.y - 40.0) / h).clamp(0.5, 2.5);
                    self.set_zoom(z);
                    self.scroll = egui::Vec2::new(c1 as f32 * 8.0, r1 as f32 * 4.0);
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

            "Noop" => {}
            "DoubleUnderline" => {
                let on = !self.current_style().double_underline;
                self.apply_style(move |s| {
                    s.double_underline = on;
                    s.underline = on;
                });
            }
            "TextVertical" => {
                let on = !self.current_style().vertical_text;
                self.apply_style(move |s| s.vertical_text = on);
            }
            "HighlightDuplicates" => self.highlight_duplicates(),
            "RejectDuplicates" => self.reject_duplicates(),
            "MergeSameCells" => self.merge_same_cells(),
            "ToggleSpotlight" => self.toggle_spotlight(),
            "SplitPanes" => self.toggle_split_panes(),
            "TextRotateUp" => self.apply_style(|s| s.rotation = 45),
            "TextRotateDown" => self.apply_style(|s| s.rotation = -45),
            "AccountingStyle" => self.apply_style(|s| {
                s.format = "_-* ¥#,##0.00_-;-* ¥#,##0.00_-;_-* \"-\"??_-;_-@_-".into()
            }),
            "AutoAverage" => self.auto_func("AVERAGE"),
            "AutoCount" => self.auto_func("COUNT"),
            "AutoMax" => self.auto_func("MAX"),
            "AutoMin" => self.auto_func("MIN"),
            "FillRight" => self.fill_cmd(false),
            "ClearValues" => self.clear_values(),
            "StyleNormal" => self.apply_wps_cell_style("StyleNormal"),
            "StyleTitle" => self.apply_wps_cell_style("StyleTitle"),
            "StyleHeading" => self.apply_wps_cell_style("StyleHeading"),
            "StyleGood" => self.apply_wps_cell_style("StyleGood"),
            "StyleBad" => self.apply_wps_cell_style("StyleBad"),
            "StyleNeutral" => self.apply_wps_cell_style("StyleNeutral"),
            "StyleInput" => self.apply_wps_cell_style("StyleInput"),
            "StyleOutput" => self.apply_wps_cell_style("StyleOutput"),
            "StyleCalc" => self.apply_wps_cell_style("StyleCalc"),
            "StyleWarning" => self.apply_wps_cell_style("StyleWarning"),
            "StyleNote" => self.apply_wps_cell_style("StyleNote"),
            "StyleH1" => self.apply_wps_cell_style("StyleH1"),
            "StyleH2" => self.apply_wps_cell_style("StyleH2"),
            "StyleH3" => self.apply_wps_cell_style("StyleH3"),
            "StyleTotal" => self.apply_wps_cell_style("StyleTotal"),
            "StyleAccentBlue20" => self.apply_wps_cell_style("StyleAccentBlue20"),
            "StyleAccentGreen20" => self.apply_wps_cell_style("StyleAccentGreen20"),
            "StyleAccentOrange20" => self.apply_wps_cell_style("StyleAccentOrange20"),
            "StyleComma0" => self.apply_wps_cell_style("StyleComma0"),
            "StyleCurrency0" => self.apply_wps_cell_style("StyleCurrency0"),
            "TableThemeBlue" => self.apply_table_theme("TableThemeBlue"),
            "TableThemeGreen" => self.apply_table_theme("TableThemeGreen"),
            "TableThemeForest" => self.apply_table_theme("TableThemeForest"),
            "TableThemeOrange" => self.apply_table_theme("TableThemeOrange"),
            "TableThemeNavy" => self.apply_table_theme("TableThemeNavy"),
            "TableThemeGray" => self.apply_table_theme("TableThemeGray"),
            "CfGreater" => self.open_cf_rule_dlg(crate::wps_menus::CfRuleOp::Greater),
            "CfLess" => self.open_cf_rule_dlg(crate::wps_menus::CfRuleOp::Less),
            "CfBetween" => self.open_cf_rule_dlg(crate::wps_menus::CfRuleOp::Between),
            "CfEqual" => self.open_cf_rule_dlg(crate::wps_menus::CfRuleOp::Equal),
            "CfTextContains" => self.open_cf_rule_dlg(crate::wps_menus::CfRuleOp::TextContains),
            "CfDuplicate" | "CfUnique" | "CfTop10" | "CfTop10Pct" | "CfBottom10" | "CfBottom10Pct" | "CfAboveAvg" | "CfBelowAvg" | "CfBarBlue" | "CfBarGreen" | "CfBarRed" | "CfBarOrange" | "CfBarPurple" | "CfBarTeal" | "CfScaleGYR" | "CfScaleRYG" | "CfScaleBWR" | "CfScaleRWB" | "CfScaleGW" | "CfScaleWG" | "CfIconTraffic" | "CfIcon3Arrows" | "CfIcon5Arrows" | "CfIcon3Flags" | "CfIcon3Symbols" | "CfIcon4Rating" | "CfIcon5Quarters" | "CfClearSel" | "CfClearSheet" => self.apply_cf_preset(action),
            "InsertSheet" => self.add_sheet(),
            "DeleteCurrentSheet" => { let idx = self.sheet_idx; self.delete_sheet(idx); }
            "HideRows" => self.ctx_menu_run(crate::CtxAct::HideRows, 0, ctx),
            "UnhideRows" => self.ctx_menu_run(crate::CtxAct::UnhideRows, 0, ctx),
            "HideCols" => self.ctx_menu_run(crate::CtxAct::HideCols, 0, ctx),
            "UnhideCols" => self.ctx_menu_run(crate::CtxAct::UnhideCols, 0, ctx),
            "RenameCurrentSheet" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::RenameSheet, idx, ctx); }
            "MoveCurrentSheet" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::MoveSheet, idx, ctx); }
            "DuplicateCurrentSheet" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::CopySheet, idx, ctx); }
            "TabColorNone" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::TabColor(None), idx, ctx); }
            "TabColorRed" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::TabColor(Some([192, 0, 0])), idx, ctx); }
            "TabColorOrange" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::TabColor(Some([237, 125, 49])), idx, ctx); }
            "TabColorYellow" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::TabColor(Some([255, 192, 0])), idx, ctx); }
            "TabColorGreen" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::TabColor(Some([112, 173, 71])), idx, ctx); }
            "TabColorTeal" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::TabColor(Some([0, 176, 176])), idx, ctx); }
            "TabColorBlue" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::TabColor(Some([68, 114, 196])), idx, ctx); }
            "TabColorPurple" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::TabColor(Some([112, 48, 160])), idx, ctx); }
            "TabColorGray" => { let idx = self.sheet_idx; self.ctx_menu_run(crate::CtxAct::TabColor(Some([128, 128, 128])), idx, ctx); }
            "ToggleLockCells" => { let v = !self.current_style().locked; self.apply_style(move |s| s.locked = v); }
            "SymRmb" => self.insert_quick_symbol('¥'),
            "SymUsd" => self.insert_quick_symbol('$'),
            "SymEur" => self.insert_quick_symbol('€'),
            "SymCheck" => self.insert_quick_symbol('√'),
            "SymCross" => self.insert_quick_symbol('×'),
            "SymPlusMinus" => self.insert_quick_symbol('±'),
            "SymPermille" => self.insert_quick_symbol('‰'),
            "SymDegreeC" => self.insert_quick_symbol('℃'),
            "SymStar" => self.insert_quick_symbol('★'),
            "SymCircle1" => self.insert_quick_symbol('①'),
            "DvCircleInvalid" => { self.circle_invalid = true; self.invalid_gen = u64::MAX; self.compute_invalid(); self.status = format!("已圈出 {} 个不合规单元格", self.invalid_cells.len()); }
            "DvClearCircles" => { self.circle_invalid = false; self.invalid_cells.clear(); self.status = "已清除无效数据圈释".into(); }
            "DvClearValidation" => self.clear_dv_in_sel(),

            // UI-11.4：函数库「最近使用的函数」菜单项（InsertFn:NAME）
            _ if action.starts_with("InsertFn:") => {
                let name = action["InsertFn:".len()..].to_string();
                self.insert_formula_at_caret(&format!("={name}()"));
                crate::fnrecent::record(&name);
                self.status = format!("已插入函数 {name}，接着点选参数区域");
            }
            // 其余（字体/字号/下拉里带值的项）已在上面处理
            _ => {
                // UI-30：未命中的命令必须留痕并让用户看见，不能只写环境变量日志。
                self.unhandled_action = Some(action.to_string());
                self.status = format!("这个命令还没有接入：{action}");
                if std::env::var("SC_RIBBON_LOG").is_ok() {
                    eprintln!("[ribbon] 未实现的命令：{action}");
                }
            }
        }
    }
}
