//! 功能区回调：状态（getEnabled/getPressed/getLabel/getText/getImage）与命令。
//!
//! 命令体全部落在既有方法上；这里只保留 MS-CUSTOMUI 回调那一层的转发，
//! 不复制业务逻辑。

use eframe::egui;

use super::CtlState;
use crate::{cell_name, col_label, ui_prefs, FormatPainter, SheetApp};

/// Border entries are ordered left, right, top, bottom. None preserves the original edge.
pub(super) fn preset_edges(action: &str, outside: [bool; 4], pen: u8) -> [Option<u8>; 4] {
    let [left, right, top, bottom] = outside;
    let mut edges = [None; 4];
    match action {
        "BorderNone" => return [Some(0); 4],
        "BordersAll" => return [Some(pen); 4],
        "BorderOuter" | "BorderThickOuter" => {
            let style = if action == "BorderThickOuter" { 3 } else { pen };
            return outside.map(|on| on.then_some(style));
        }
        "BorderLeft" => edges[0] = left.then_some(pen),
        "BorderRight" => edges[1] = right.then_some(pen),
        "BorderTop" => edges[2] = top.then_some(pen),
        "BorderBottom" => edges[3] = bottom.then_some(pen),
        "BorderThickBottom" => edges[3] = bottom.then_some(3),
        "BorderDoubleBottom" => edges[3] = bottom.then_some(4),
        "BorderTopBottom" | "BorderTopThickBottom" | "BorderTopDoubleBottom" => {
            edges[2] = top.then_some(pen);
            edges[3] = bottom.then_some(match action {
                "BorderTopThickBottom" => 3,
                "BorderTopDoubleBottom" => 4,
                _ => pen,
            });
        }
        "BorderInsideH" => { edges[2] = (!top).then_some(pen); edges[3] = (!bottom).then_some(pen); }
        "BorderInsideV" => { edges[0] = (!left).then_some(pen); edges[1] = (!right).then_some(pen); }
        _ => {}
    }
    edges
}

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
    /// 控件状态：等价于 MS-CUSTOMUI 的 get* 回调（对外入口，借用当前格样式零克隆）。
    pub fn ctl_state(&self, id: &str) -> CtlState {
        self.ctl_state_with_style(id, self.current_style_ref())
    }

    /// 供功能区每帧复用单次读取的 `CellStyle`，避免单帧克隆 40-50 次样式表项。
    pub(crate) fn ctl_state_with_style(
        &self,
        id: &str,
        cur: &sheetcore::style::CellStyle,
    ) -> CtlState {
        let mut st = CtlState::default();
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
                let m = self.merge_at_fast(r1, c1);
                st.checked = m == Some((r1, c1, r2, c2)) || (r1 == r2 && c1 == c2 && m.is_some());
                st.enabled = !self.is_edit_blocked();
            }
            "MergeCenterItem" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let m = self.merge_at_fast(r1, c1);
                st.checked = m == Some((r1, c1, r2, c2)) || (r1 == r2 && c1 == c2 && m.is_some());
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
            "PrevComment" | "NextComment" | "SelectComments" => {
                let sh = self.wb.sheet(self.sheet_idx);
                st.enabled = !sh.notes.is_empty() || !sh.threads.is_empty();
                if !st.enabled {
                    st.tip = Some("当前工作表没有批注".into());
                }
            }
            "ShowShowCommentBtn" | "ShowComment" => {
                let sh = self.wb.sheet(self.sheet_idx);
                st.enabled = !sh.notes.is_empty() || !sh.threads.is_empty();
                if !st.enabled {
                    st.tip = Some("当前工作表没有批注".into());
                } else {
                    let (ar, ac) = self.note_anchor(self.sel.0, self.sel.1);
                    let key = sheetcore::model::CellRef::new(ar, ac);
                    st.checked = self.show_all_comments
                        || self.pinned_comments.contains(&(sh.identity, key));
                    st.tip = Some(if st.checked {
                        "当前批注已固定显示：再点一次取消固定".into()
                    } else {
                        "固定常显当前单元格批注（若当前格无批注则定位并固定第一条）".into()
                    });
                }
            }
            "FillColor" => {
                let f = self.last_fill_color;
                st.swatch = Some(egui::Color32::from_rgb(f[0], f[1], f[2]));
            }
            "FontColor" => {
                let c = self.last_font_color;
                st.swatch = Some(egui::Color32::from_rgb(c[0], c[1], c[2]));
            }
            "NumberFormat" => st.text = Some(fmt_label(&cur.format).to_string()),
            "PrintGridlines" | "PrintGridlinesToggle" | "PrintPrintGridlines" => {
                st.checked = self.wb.sheet(self.sheet_idx).page.gridlines;
            }
            "PrintHeadings" | "PrintHeadingsToggle" => {
                st.checked = self.wb.sheet(self.sheet_idx).page.headings;
            }
            "ShowGridlines" | "ViewGridlines" => st.checked = self.show_gridlines,
            "ShowHeadings" | "ViewHeadings" => st.checked = self.show_headings,
            "ShowFormulaBar" => st.checked = self.show_formula_bar,
            "TextSpill" => st.checked = self.text_spill,
            "ShowFormulas" => st.checked = self.show_formulas,
            "ShowAllComments" => st.checked = self.show_all_comments,
            "CommentsPanel" => st.checked = self.comments_panel.open,
            "ExportCommentsSheet" => {
                let has_any = self
                    .wb
                    .sheets
                    .iter()
                    .any(|s| !s.notes.is_empty() || !s.threads.is_empty());
                st.enabled = has_any;
                if !has_any {
                    st.tip = Some("当前工作簿没有批注可导出".into());
                }
            }
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
            | "AutoShare" | "AutoAutoShare" => {
                st.visible = self.net.is_none();
                if id == "AutoShare" || id == "AutoAutoShare" {
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
                let sh = self.wb.sheet(self.sheet_idx);
                let p = &sh.page;
                let pct = if p.fit_to_page {
                    (p.scale_factor(sh) * 100.0).round() as i32
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
                    st.text = Some(if ch.title.is_empty() {
                        ch.kind.label().to_string()
                    } else {
                        ch.title.clone()
                    });
                } else {
                    st.visible = false;
                }
            }
            "ChartMeta" => {
                if let Some(ch) = self.chart_at_sel() {
                    st.text = Some(format!(
                        "{} · {} · {} 系列",
                        ch.kind.label(),
                        ch.style.theme.label(),
                        ch.series.len()
                    ));
                } else {
                    st.visible = false;
                }
            }
            "ChartStudioToggle" => {
                st.checked = self.chart_studio.sidebar_open && self.chart_at_sel().is_some();
            }
            "ChartToggleLabels" => {
                st.checked = self.chart_at_sel().is_some_and(|c| c.show_labels);
            }
            "ChartToggleTrendline" => {
                st.checked = self
                    .chart_at_sel()
                    .is_some_and(|c| c.series.iter().any(|s| !s.trendlines.is_empty()));
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
            "InsertSlicer" => st.enabled = self.active_pivot().is_some() || self.pivot_last.is_some(),
            "PivotName" => {
                if let Some(p) = self.active_pivot() {
                    st.text = Some(format!("名称：{}", p.name));
                } else {
                    st.visible = false;
                }
            }
            "PivotSourceRange" => {
                if let Some(p) = self.active_pivot() {
                    st.text = Some(format!(
                        "源 {}!{}:{}",
                        p.source_sheet,
                        cell_name(p.spec.source.0, p.spec.source.1),
                        cell_name(p.spec.source.2, p.spec.source.3)
                    ));
                } else {
                    st.visible = false;
                }
            }
            "PivotFieldList" => st.checked = self.pivot_ui.sidebar_open,
            "PivotToggleAutoRefresh" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.auto_refresh);
            }
            "PivotToggleDataBars" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.show_data_bars);
            }
            "PivotToggleOutliers" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.highlight_outliers);
            }
            "PivotRepeatLabels" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.repeat_labels);
            }
            "PivotBandedRows" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.banded_rows);
            }
            "PivotBandedCols" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.banded_cols);
            }
            "PivotLayoutCompact" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.report_layout == sheetcore::pivot_engine::ReportLayout::Compact);
            }
            "PivotLayoutOutline" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.report_layout == sheetcore::pivot_engine::ReportLayout::Outline);
            }
            "PivotLayoutTabular" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.report_layout == sheetcore::pivot_engine::ReportLayout::Tabular);
            }
            "PivotSubtotalsOff" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.subtotal_pos == sheetcore::pivot_engine::SubtotalPos::Off);
            }
            "PivotSubtotalsTop" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.subtotal_pos == sheetcore::pivot_engine::SubtotalPos::Top);
            }
            "PivotSubtotalsBottom" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.subtotal_pos == sheetcore::pivot_engine::SubtotalPos::Bottom);
            }
            "PivotGrandBoth" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.row_grand_totals && p.options.col_grand_totals);
            }
            "PivotGrandRow" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.row_grand_totals && !p.options.col_grand_totals);
            }
            "PivotGrandCol" => {
                st.checked = self.active_pivot().is_some_and(|p| !p.options.row_grand_totals && p.options.col_grand_totals);
            }
            "PivotGrandOff" => {
                st.checked = self.active_pivot().is_some_and(|p| !p.options.row_grand_totals && !p.options.col_grand_totals);
            }
            "PivotStyleBlue" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.style_theme == sheetcore::pivot_engine::PivotStyleTheme::Blue);
            }
            "PivotStyleGreen" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.style_theme == sheetcore::pivot_engine::PivotStyleTheme::Green);
            }
            "PivotStyleSlate" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.style_theme == sheetcore::pivot_engine::PivotStyleTheme::Slate);
            }
            "PivotStyleAmber" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.style_theme == sheetcore::pivot_engine::PivotStyleTheme::Amber);
            }
            "PivotStyleDark" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.style_theme == sheetcore::pivot_engine::PivotStyleTheme::Dark);
            }
            "PivotStyleMinimal" => {
                st.checked = self.active_pivot().is_some_and(|p| p.options.style_theme == sheetcore::pivot_engine::PivotStyleTheme::Minimal);
            }
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
            "TableStyleChip" => {
                if let Some(t) = self.table_at_sel() {
                    st.text = Some(t.style_name.clone());
                } else {
                    st.visible = false;
                }
            }
            _ => {}
        }
        if crate::debug_flags::ribbon_state_matches(id) {
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
                self.begin_edit(Some(format!("={name}(")));
            } else {
                self.insert_formula_at_caret(&format!("{name}("));
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
            "ExportWorkbookPdf" => self.export_workbook_pdf_dialog(),
            "ExportCsv" => self.csv_export_dlg = Some(crate::CsvExportDlg::default()),
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
                let mut merges = Vec::new();
                let mut cf_blocks = Vec::new();
                let mut cf_dxfs = Vec::new();
                {
                    let sheet = self.wb.sheet(self.sheet_idx);
                    for r in r1..=r2 {
                        let mut row = Vec::with_capacity((c2 - c1 + 1) as usize);
                        for c in c1..=c2 {
                            row.push(sheet.style_of(r, c));
                        }
                        styles.push(row);
                    }
                    for &(mr1, mc1, mr2, mc2) in sheet.merges.iter() {
                        if mr1 >= r1 && mc1 >= c1 && mr2 <= r2 && mc2 <= c2 {
                            merges.push((mr1 - r1, mc1 - c1, mr2 - r1, mc2 - c1));
                        }
                    }
                    for blk in &sheet.cf.blocks {
                        let mut rel_ranges = Vec::new();
                        let mut first_abs: Option<(u32, u32)> = None;
                        for &(cr1, cc1, cr2, cc2) in &blk.ranges {
                            let ir1 = cr1.max(r1);
                            let ic1 = cc1.max(c1);
                            let ir2 = cr2.min(r2);
                            let ic2 = cc2.min(c2);
                            if ir1 <= ir2 && ic1 <= ic2 {
                                if first_abs.is_none() {
                                    first_abs = Some((ir1, ic1));
                                }
                                rel_ranges.push((ir1 - r1, ic1 - c1, ir2 - r1, ic2 - c1));
                            }
                        }
                        if !rel_ranges.is_empty() {
                            let mut rules = blk.rules.clone();
                            // 将规则内的相对引用归一化到裁剪后首个命中格 (first_abs)，
                            // 以便格式刷贴到任意目标位置时按目标锚点精确平移。
                            if let (Some((orig_ar, orig_ac)), Some((cur_ar, cur_ac))) =
                                (blk.ranges.first().map(|&(a, b, _, _)| (a, b)), first_abs)
                            {
                                let dr = i64::from(cur_ar) - i64::from(orig_ar);
                                let dc = i64::from(cur_ac) - i64::from(orig_ac);
                                if dr != 0 || dc != 0 {
                                    SheetApp::translate_cf_rules(&mut rules, dr, dc, &sheet.name);
                                }
                            }
                            cf_blocks.push(sheetcore::cf::CfBlock {
                                ranges: rel_ranges,
                                rules,
                            });
                        }
                    }
                    if !cf_blocks.is_empty() {
                        cf_dxfs = sheet.cf.dxfs.clone();
                    }
                }
                let continuous = self.painter_double;
                self.painter_double = false;
                self.format_painter = Some(FormatPainter {
                    rect: (r1, c1, r2, c2),
                    styles,
                    merges,
                    cf_blocks,
                    cf_dxfs,
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
            "GrowFont" | "FontGrow" => {
                let cur = self.current_style();
                let s2 = (cur.size + 1.0).min(72.0);
                self.apply_style(move |s| s.size = s2);
            }
            "ShrinkFont" | "FontShrink" => {
                let cur = self.current_style();
                let s2 = (cur.size - 1.0).max(6.0);
                self.apply_style(move |s| s.size = s2);
            }
            "Bold" => {
                let v = !self.current_style().bold;
                self.apply_style(move |s| s.bold = v);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("Bold".into()));
            }
            "Italic" => {
                let v = !self.current_style().italic;
                self.apply_style(move |s| s.italic = v);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("Italic".into()));
            }
            "Underline" => {
                let v = !self.current_style().underline;
                self.apply_style(move |s| s.underline = v);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("Underline".into()));
            }
            "StrikeThrough" => {
                let v = !self.current_style().strikethrough;
                self.apply_style(move |s| s.strikethrough = v);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("StrikeThrough".into()));
            }
            "BordersDefault" => {
                let act = self.last_border_action.clone();
                self.ribbon_action(&act, ctx);
            }
            "BordersAll" | "BorderBottom" | "BorderTop" | "BorderLeft" | "BorderRight" | "BorderOuter" | "BorderThickOuter" | "BorderDoubleBottom" | "BorderThickBottom" | "BorderTopBottom" | "BorderTopThickBottom" | "BorderTopDoubleBottom" | "BorderInsideH" | "BorderInsideV" | "BorderNone" => {
                self.last_border_action = action.into();
                self.border_brush = None;
                self.border_brush_start = None;
                let ranges = self.sel_rects();
                let color = self.last_border_color;
                let pen = self.border_brush_style;
                self.apply_style_xy(|s, r, c| {
                    let Some(&(top, left, bottom, right)) = ranges.iter().find(|&&(t, l, b, rr)| {
                        (t..=b).contains(&r) && (l..=rr).contains(&c)
                    }) else { return };
                    let edges = preset_edges(action, [c == left, c == right, r == top, r == bottom], pen);
                    for (edge, style) in s.border.iter_mut().zip(edges) {
                        if let Some(style) = style {
                            *edge = if style == 0 { Default::default() } else { sheetcore::style::Border { style, color } };
                        }
                    }
                    if action == "BorderNone" { s.diagonal = None; }
                });
                self.set_repeat_action(crate::RepeatAction::RibbonCmd(action.into()));
            }
            "BorderDraw" => self.set_border_brush(crate::BorderBrush::Outline),
            "BorderDrawGrid" => self.set_border_brush(crate::BorderBrush::Grid),
            "BorderErase" => self.set_border_brush(crate::BorderBrush::Erase),
            "MoreBorders" => { self.open_format_dialog(); self.fmt_tab = 3; }
            "FillColor" => self.apply_fill_color(Some(self.last_fill_color)),
            "FontColor" => self.apply_font_color(self.last_font_color),
            "ClearFormat" => {
                self.clear_formats();
                self.set_repeat_action(crate::RepeatAction::Clear(crate::ClearPart::Formats));
            }
            "AlignTop" => {
                self.apply_style(|s| s.valign = 1);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("AlignTop".into()));
            }
            "AlignMiddle" => {
                self.apply_style(|s| s.valign = 2);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("AlignMiddle".into()));
            }
            "AlignBottom" => {
                self.apply_style(|s| s.valign = 3);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("AlignBottom".into()));
            }
            "AlignLeft" => {
                self.apply_style(|s| s.halign = 1);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("AlignLeft".into()));
            }
            "AlignCenter" => {
                self.apply_style(|s| s.halign = 2);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("AlignCenter".into()));
            }
            "AlignRight" => {
                self.apply_style(|s| s.halign = 3);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("AlignRight".into()));
            }
            "DecreaseIndent" => {
                self.apply_style(|s| s.indent = s.indent.saturating_sub(1));
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("DecreaseIndent".into()));
            }
            "IncreaseIndent" => {
                self.apply_style(|s| s.indent = (s.indent + 1).min(15));
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("IncreaseIndent".into()));
            }
            "WrapText" => {
                let v = !self.current_style().wrap;
                self.apply_style(move |s| s.wrap = v);
                if v {
                    self.autofit_sel_rows_for_wrap();
                }
                self.set_repeat_action(crate::RepeatAction::ToggleWrap);
            }
            "MergeCenter" => {
                let was_merged = self.ctl_state("MergeCenter").checked;
                let on = !was_merged;
                self.toggle_merge(on);
                if on && self.merge_pending.is_none() && self.ctl_state("MergeCenter").checked {
                    self.apply_style(|s| s.halign = 2);
                }
                self.set_repeat_action(crate::RepeatAction::MergeToggle);
            }
            "MergeAcross" => self.merge_across(),
            "MergeCells" => {
                self.toggle_merge(true);
                self.set_repeat_action(crate::RepeatAction::MergeToggle);
            }
            "MergeContents" => self.merge_contents(),
            "UnmergeCells" => {
                self.toggle_merge(false);
                self.set_repeat_action(crate::RepeatAction::MergeToggle);
            }
            "UnmergeFill" => self.unmerge_and_fill(),
            "CenterAcross" => {
                self.apply_style(|s| s.halign = 4);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("CenterAcross".into()));
            }
            "NumberFormatApply" => {}
            "NumberFormat:General" => {
                self.apply_style(|s| s.format.clear());
                self.set_repeat_action(crate::RepeatAction::NumberFormat(String::new()));
                self.status = "已应用常规数字格式".into();
            }
            "CurrencyStyle" => {
                self.apply_style(|s| s.format = "¥#,##0.00".into());
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("CurrencyStyle".into()));
            }
            "PercentStyle" => {
                self.apply_style(|s| s.format = "0%".into());
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("PercentStyle".into()));
            }
            "CommaStyle" => {
                self.apply_style(|s| s.format = "#,##0.00".into());
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("CommaStyle".into()));
            }
            "IncreaseDecimal" => {
                self.decimals(1);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("IncreaseDecimal".into()));
            }
            "DecreaseDecimal" => {
                self.decimals(-1);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("DecreaseDecimal".into()));
            }
            "ConditionalFormatting" => self.cf_mgr = true,
            "FormatAsTable" => self.make_table(),
            "CellStyles" => self.open_format_dialog(),
            "FormatDialogLauncher" => self.fmt_pending = true,
            "FormatCellsDialog" => self.open_format_dialog(),
            "InsertRow" => {
                let (r1, _c1, r2, _c2) = self.sel_rect();
                self.structural(sheetcore::structural::Structural::InsertRows {
                    at: r1,
                    n: r2 - r1 + 1,
                });
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("InsertRow".into()));
            }
            "InsertColumn" => {
                let (_r1, c1, _r2, c2) = self.sel_rect();
                self.structural(sheetcore::structural::Structural::InsertCols { at: c1, n: c2 - c1 + 1 });
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("InsertColumn".into()));
            }
            "InsertCellsDown" => {
                self.shift_cells(true, false);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("InsertCellsDown".into()));
            }
            "InsertCellsRight" => {
                self.shift_cells(true, true);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("InsertCellsRight".into()));
            }
            "DeleteRow" => {
                let (r1, _c1, r2, _c2) = self.sel_rect();
                self.delete_rows_range(r1, r2);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("DeleteRow".into()));
            }
            "DeleteColumn" => {
                let (_r1, c1, _r2, c2) = self.sel_rect();
                self.structural(sheetcore::structural::Structural::DeleteCols { at: c1, n: c2 - c1 + 1 });
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("DeleteColumn".into()));
            }
            "DeleteCellsUp" => {
                self.shift_cells(false, false);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("DeleteCellsUp".into()));
            }
            "DeleteCellsLeft" => {
                self.shift_cells(false, true);
                self.set_repeat_action(crate::RepeatAction::RibbonCmd("DeleteCellsLeft".into()));
            }
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
            "Hyperlink" => self.open_hyperlink_dialog(),
            "ChartWizard" | "ColumnChart" => self.insert_chart_from_selection(),
            "ColumnClusteredChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Column),
            "ColumnStackedChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::ColumnStacked),
            "ColumnStacked100Chart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::ColumnStacked100),
            "LineChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Line),
            "LineStackedChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::LineStacked),
            "PieChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Pie),
            "BarChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Bar),
            "BarStackedChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::BarStacked),
            "BarStacked100Chart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::BarStacked100),
            "AreaChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Area),
            "AreaStackedChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::AreaStacked),
            "AreaStacked100Chart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::AreaStacked100),
            "DoughnutChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Doughnut),
            "ScatterChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Scatter),
            "BubbleChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Bubble),
            "RadarChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Radar),
            "ComboChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Combo),
            "WaterfallChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Waterfall),
            "FunnelChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Funnel),
            "TreemapChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Treemap),
            "HistogramChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::Histogram),
            "BoxWhiskerChart" => self.insert_chart_of_kind(sheetcore::charts::ChartKind::BoxWhisker),
            "ChartManager" => {
                self.chart_mgr = true;
            }
            "InsertPivot" => self.open_pivot_create_wizard(None),
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
            "RefreshPivot" => self.refresh_pivots_modern(false),
            "PivotOptions" => {
                if self.active_pivot().is_some() {
                    self.run_pivot_command("PivotOptionsDialog");
                } else if let Some((_, spec, _)) = self.pivot_last.clone() {
                    self.pivot_edit = Some(spec);
                } else {
                    self.status = "先选中数据区域再新建透视表".into();
                }
            }
            "WorkbookLinks" => self.xlink_dlg = true,
            "InsertSlicer" => {
                if self.active_pivot().is_some() {
                    self.run_pivot_command("PivotInsertSlicer");
                } else {
                    self.slicer_open();
                }
            }

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
                self.cross_sheet_traces.clear();
                self.cross_trace_popup = None;
                self.status = "已移去追踪箭头".into();
            }
            "ShowFormulas" => self.show_formulas = !self.show_formulas,
            "ErrorChecking" => self.open_error_check(),
            "EvaluateFormula" => self.open_evaluate_formula(),
            "CalculateNow" => self.recalc_all_async(),
            "CalculateSheet" => self.recalc_sheet_async(self.sheet_idx),
            "CalcAuto" => {
                self.manual_calc = false;
                self.wb.calc_manual = false;
                self.wb.calc_pending = false;
                self.wb.calc_pending_sheets.clear();
                self.wb.dirty = true;
                self.doc_gen = self.doc_gen.wrapping_add(1);
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
                self.doc_gen = self.doc_gen.wrapping_add(1);
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
            "FilterAdvanced" => {
                self.open_adv_filter_dialog();
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
            a if a.starts_with("InsertShape:") => {
                let tag = &a["InsertShape:".len()..];
                let kind = match tag {
                    "Rect" => sheetcore::textbox::ShapeKind::Rect,
                    "RoundedRect" => sheetcore::textbox::ShapeKind::RoundedRect,
                    "RightArrow" => sheetcore::textbox::ShapeKind::RightArrow,
                    "Line" => sheetcore::textbox::ShapeKind::Line,
                    "Ellipse" => sheetcore::textbox::ShapeKind::Ellipse,
                    "Diamond" => sheetcore::textbox::ShapeKind::Diamond,
                    "Callout" => sheetcore::textbox::ShapeKind::Callout,
                    _ => sheetcore::textbox::ShapeKind::TextBox,
                };
                let (r, c) = self.sel;
                let (cols, rows) = self.view_axes();
                let x = cols.pos(c) + 4.0;
                let y = rows.pos(r) + 2.0;
                self.tb_create_kind(x, y, 160.0, 64.0, kind);
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
                let sheet = self.wb.sheet(self.sheet_idx);
                self.scen_result = sheet.scenario_settings.sqref.clone().unwrap_or_default();
                self.scen_msg.clear();
                self.scen_sel = sheet.scenario_settings.current.filter(|&i| i < sheet.scenarios.len());
                self.scen_dlg = true;
            }
            "GroupSelection" => self.group_selection(),
            "UngroupSelection" => self.ungroup_selection(),
            "OutlineSettings" => self.outline_set_dlg = true,
            "OutlineAuto" => self.auto_outline(),
            "OutlineClear" => self.clear_outline(),
            "HideDetail" => self.toggle_detail(true),
            "ShowDetail" => self.toggle_detail(false),
            "ImportCsv" => self.import_csv(),
            "ImportWorkbook" => self.import_workbook(),
            "LinkSheet" => {
                if !self.edit_blocked() {
                    self.commit_edit(None);
                    self.link_dlg = Some((crate::sheet_edit_target::SheetEditTarget::current(self, self.sel), String::new()));
                }
            }
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
                    season_len: 0,
                    confidence: 95,
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
                    replace_existing: true,
                    page_break_between_groups: false,
                    msg: String::new(),
                });
            }
            "FlashFill" => {
                self.flash_fill_instant_or_dialog();
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
            "ProtectSheet" => self.open_protect_sheet_dialog(),
            "TrackChanges" => {
                self.open_audit_panel();
            }
            // 审阅：显示更改（统一打开编辑与防篡改记录）、批注导航、辅助功能检查器
            "ShowChanges" => {
                self.open_audit_panel();
                self.status = "编辑与防篡改记录已打开".into();
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
            "ShowAllComments" => {
                self.show_all_comments = !self.show_all_comments;
                self.status = if self.show_all_comments {
                    "已显示当前工作表的全部批注".into()
                } else {
                    "已隐藏全部批注（悬停或选中单元格时自动展开）".into()
                };
            }
            "CommentsPanel" => self.toggle_comments_panel(),
            "ExportCommentsSheet" => {
                self.export_comments_summary_sheet(true);
            }
            "A11yChecker" => {
                self.a11y_scan();
                self.a11y_open = true;
            }
            "AutoShare" => self.auto_share = !self.auto_share,

            // ---------------------------------------------------- 视图
            "ShowGridlines" => {
                self.show_gridlines = !self.show_gridlines;
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.show_gridlines = self.show_gridlines;
                sh.view_dirty = true;
                self.wb.dirty = true;
            }
            "ShowHeadings" => {
                self.show_headings = !self.show_headings;
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.show_headings = self.show_headings;
                sh.view_dirty = true;
                self.wb.dirty = true;
            }
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
                sh.page_dirty = true;
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
                sh.page_dirty = true;
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
                sh.page_dirty = true;
                self.wb.dirty = true;
                self.status = format!("已在第 {} 行、第 {} 列前插入分页符", r + 1, c + 1);
            }
            "PageBreakRemove" => {
                let (r, c) = self.sel;
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.page.row_breaks.retain(|x| *x != r);
                sh.page.col_breaks.retain(|x| *x != c);
                sh.page_dirty = true;
                self.wb.dirty = true;
                self.status = format!("已删除第 {} 行、第 {} 列的分页符", r + 1, c + 1);
            }
            "PageBreakResetAll" => {
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.page.row_breaks.clear();
                sh.page.col_breaks.clear();
                sh.page_dirty = true;
                self.wb.dirty = true;
                self.status = "已重置当前工作表的所有手动分页符".into();
            }
            "ZoomToSelection" => {
                let (r1, c1, r2, c2) = self.sel_rect();
                let (cur_cols, cur_rows) = self.view_axes();
                let cur_z = self.zoom.clamp(0.1, 4.0);
                // 还原为 100% 真实可见尺寸（已自动跳过隐藏行列）
                let w = ((cur_cols.pos(c2.saturating_add(1)) - cur_cols.pos(c1)) / cur_z).max(0.0);
                let h = ((cur_rows.pos(r2.saturating_add(1)) - cur_rows.pos(r1)) / cur_z).max(0.0);
                let x0_unscaled = (cur_cols.pos(c1) / cur_z).max(0.0);
                let y0_unscaled = (cur_rows.pos(r1) / cur_z).max(0.0);
                let view = self.last_view;
                if w > 1.0 && h > 1.0 && view.x > 60.0 && view.y > 40.0 {
                    let z = (((view.x - 52.0) / w).min((view.y - 32.0) / h)).clamp(0.1, 4.0);
                    self.set_zoom(z);
                    let (fy, fx) = self.wb.sheet(self.sheet_idx).freeze.unwrap_or((0, 0));
                    let (new_cols, new_rows) = self.view_axes();
                    let fw = new_cols.pos(fx.min(16_383));
                    let fh = new_rows.pos(fy.min(1_048_575));
                    self.scroll = egui::Vec2::new(
                        (x0_unscaled * z - fw).max(0.0),
                        (y0_unscaled * z - fh).max(0.0),
                    );
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
            "Help" => self.help_open = true,
            "About" => self.about_open = true,

            // ---------------------------------------------------- 表设计
            "TablePivot" => {
                if let Some(t) = self.table_at_sel() {
                    self.open_pivot_create_wizard(Some(t.range));
                }
            }
            // ---------------------------------------------------- 数据透视表分析 & 设计
            "PivotRefresh" | "PivotRefreshAll" | "PivotChangeSource" | "PivotToggleAutoRefresh" | "PivotInsertSlicer" | "PivotDrillDown" | "PivotExpandAll" | "PivotCollapseAll" | "PivotCalcField" | "PivotOptionsDialog" | "PivotDelete" | "PivotFieldList" => { self.run_pivot_command(action); }
            "PivotLayoutCompact" | "PivotLayoutOutline" | "PivotLayoutTabular" | "PivotSubtotalsBottom" | "PivotSubtotalsTop" | "PivotSubtotalsOff" | "PivotGrandBoth" | "PivotGrandRow" | "PivotGrandCol" | "PivotGrandOff" | "PivotRepeatLabels" => { self.run_pivot_command(action); }
            "PivotBandedRows" | "PivotBandedCols" | "PivotToggleDataBars" | "PivotToggleOutliers" | "PivotStyleBlue" | "PivotStyleGreen" | "PivotStyleSlate" | "PivotStyleAmber" | "PivotStyleDark" | "PivotStyleMinimal" => { self.run_pivot_command(action); }
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
            a if a.starts_with("TableTotalsFn:") => {
                if let Some(fn_name) = a.strip_prefix("TableTotalsFn:") {
                    self.set_table_totals_fn(fn_name);
                }
            }

            // ---------------------------------------------------- 图表工具
            "ChartEdit" => self.open_chart_studio_for_selected(),
            "ChartStudioToggle" => {
                if self.chart_studio.sidebar_open {
                    self.chart_studio.sidebar_open = false;
                } else {
                    self.open_chart_studio_for_selected();
                }
            }
            "ChartSwitchRowCol" => self.switch_selected_chart_row_col(),
            "ChartCycleTheme" => self.cycle_selected_chart_theme(),
            "ChartToggleLabels" => self.toggle_selected_chart_labels(),
            "ChartToggleTrendline" => self.toggle_selected_chart_trendline(),
            "ChartExportSvg" => {
                if let Some(ci) = self.chart_sel {
                    self.export_chart_svg_dialog(ci);
                }
            }
            "ChartDuplicate" => {
                if let Some(ci) = self.chart_sel {
                    self.duplicate_chart_at(ci);
                }
            }
            "ChartDelete" => self.delete_selected_chart(),

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
            "DvCircleInvalid" => self.show_invalid(ctx),
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
                if crate::debug_flags::ribbon_log() {
                    eprintln!("[ribbon] 未实现的命令：{action}");
                }
            }
        }
    }
}
