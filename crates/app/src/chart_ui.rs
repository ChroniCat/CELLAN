//! 现代化超越 Excel 的 Chart Studio 交互界面：
//! - 智能新建图表向导（自动框选当前数据区 + 数据画像分析 + Top-N 场景化推荐 + 22 种全图型库 + 实时 GPU 交互预览 + 8 套主题 + 无需先保存文件）
//! - 右侧可停靠非模态「Chart Studio 设计器」（外观/数据/坐标轴/标签/分析五大页签，实时 WYSIWYG 联动画布，全操作一步可撤销/重做）
//! - 选中图表自动在工作表网格上用系列颜色高亮源数据区域（分类区/系列名/数值区/气泡体量区）
//! - 图表右键/悬浮条快捷操作（主题切换/行列互换/数据标签/趋势线/设计器/高清矢量 SVG 导出/克隆/删除）
//! - 图表双击标题快速重命名弹窗与全内存可撤销图表管理器

use crate::{cell_name, chart_view, ui_kit, SheetApp};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use sheetcore::chart_engine::{
    analyze_selection, build_smart_chart, infer_source_rect, series_rgb, switch_row_column,
    theme_spec, ChartRecommendation, SelectionProfile,
};
use sheetcore::charts::{
    AxisConfig, BlankPolicy, Chart, ChartKind, ChartThemeId, ErrorBar, ErrorBarDir, ErrorBarKind,
    LineDash, MarkerStyle, RefLineAxis, RefLineMode, ReferenceLine, Series, Trendline,
    TrendlineKind,
};
use sheetcore::model::SharedList;
use sheetcore::undo::ChartUndoEdit;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChartStudioTab {
    #[default]
    Style,
    Data,
    Axes,
    Labels,
    Analytics,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChartAxisTab {
    #[default]
    YPrimary,
    YSecondary,
    X,
}

#[derive(Clone, Debug)]
pub(crate) struct ChartCreateWizard {
    pub source_sheet_idx: usize,
    pub source_sheet_name: String,
    pub source_rect: (u32, u32, u32, u32),
    pub source_text: String,
    pub profile: SelectionProfile,
    pub recs: Vec<ChartRecommendation>,
    pub selected_kind: ChartKind,
    pub switch_row_col: bool,
    pub theme: ChartThemeId,
    pub title: String,
    pub subtitle: String,
    pub highlight_extrema: bool,
    pub show_growth_badge: bool,
    pub show_summary_bar: bool,
    pub gallery_cat: usize,
    pub preview_chart: Chart,
}

pub(crate) struct ChartStudioState {
    pub wizard: Option<ChartCreateWizard>,
    pub sidebar_open: bool,
    pub active_tab: ChartStudioTab,
    pub axis_tab: ChartAxisTab,
    pub selected_series: usize,
    pub source_edit_buf: String,
    pub source_edit_key: Option<(usize, usize)>,
    pub new_ref_label: String,
    pub new_ref_val: String,
    pub new_ref_low: String,
    pub new_ref_high: String,
    pub new_ref_color: [u8; 3],
    pub new_tl_kind: usize,
    pub new_tl_poly_order: u8,
    pub new_tl_ma_period: u8,
    pub new_tl_forward: String,
    pub new_tl_backward: String,
    pub inline_title_modal: Option<(usize, String, String)>,
    pub last_refresh_sig: (u64, u64, usize),
    /// 连续输入/拖动控件修改前捕获的快照，松手/失焦时合并提交为单条 `ChartUndoEdit`
    pub pending_undo: Option<(usize, String, SharedList<Chart>, Vec<(String, String)>)>,
}

impl Default for ChartStudioState {
    fn default() -> Self {
        Self {
            wizard: None,
            sidebar_open: false,
            active_tab: ChartStudioTab::Style,
            axis_tab: ChartAxisTab::YPrimary,
            selected_series: 0,
            source_edit_buf: String::new(),
            source_edit_key: None,
            new_ref_label: "目标线".into(),
            new_ref_val: "100".into(),
            new_ref_low: "80".into(),
            new_ref_high: "120".into(),
            new_ref_color: [220, 76, 62],
            new_tl_kind: 0,
            new_tl_poly_order: 2,
            new_tl_ma_period: 3,
            new_tl_forward: "0".into(),
            new_tl_backward: "0".into(),
            inline_title_modal: None,
            last_refresh_sig: (u64::MAX, u64::MAX, usize::MAX),
            pending_undo: None,
        }
    }
}

const CHART_CATEGORIES: &[(&str, &[ChartKind])] = &[
    (
        "柱形/条形",
        &[
            ChartKind::Column,
            ChartKind::ColumnStacked,
            ChartKind::ColumnStacked100,
            ChartKind::Bar,
            ChartKind::BarStacked,
            ChartKind::BarStacked100,
        ],
    ),
    (
        "折线/面积",
        &[
            ChartKind::Line,
            ChartKind::LineStacked,
            ChartKind::Area,
            ChartKind::AreaStacked,
            ChartKind::AreaStacked100,
        ],
    ),
    (
        "财务/组合",
        &[ChartKind::Combo, ChartKind::Waterfall, ChartKind::Funnel],
    ),
    (
        "占比/层级",
        &[ChartKind::Pie, ChartKind::Doughnut, ChartKind::Treemap],
    ),
    (
        "统计/相关",
        &[
            ChartKind::Scatter,
            ChartKind::Bubble,
            ChartKind::Histogram,
            ChartKind::BoxWhisker,
            ChartKind::Radar,
        ],
    ),
];

const AXIS_NUM_FMT_PRESETS: &[(&str, &str)] = &[
    ("自动", ""),
    ("整数", "#,##0"),
    ("两位小数", "#,##0.00"),
    ("人民币", "¥#,##0"),
    ("百分比", "0.0%"),
    ("千位", "#,##0,\"千\""),
    ("科学计数", "0.00E+00"),
];

#[inline]
fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn find_category_index(kind: ChartKind) -> usize {
    CHART_CATEGORIES
        .iter()
        .position(|(_, list)| list.contains(&kind))
        .unwrap_or(0)
}

impl SheetApp {
    /// 单元格/公式/筛选变动时，实时刷新当前工作表全部图表的缓存数据（支持跨表与 `plot_vis_only` 筛选隐藏行）。
    pub(crate) fn sync_live_charts(&mut self) {
        let sig = (self.wb_epoch, self.doc_gen, self.sheet_idx);
        if self.chart_studio.last_refresh_sig == sig {
            return;
        }
        self.chart_studio.last_refresh_sig = sig;
        if self.sheet_idx >= self.wb.sheets.len() {
            return;
        }
        if self.wb.sheets[self.sheet_idx].charts.is_empty() {
            return;
        }
        let sys = self.wb.date_system;
        let styles = self.wb.styles.clone();
        // 安全取回本表 charts 列表并基于全工作簿只读刷新（避免借用冲突）
        let mut charts: Vec<Chart> = self.wb.sheets[self.sheet_idx].charts.iter().cloned().collect();
        let before_fp = crate::grid_render_cache::charts_fingerprint(&charts);
        for ch in &mut charts {
            sheetcore::charts::refresh_chart_from_sheets(
                ch,
                &self.wb.sheets,
                self.sheet_idx,
                Some(&styles),
                sys,
            );
        }
        let after_fp = crate::grid_render_cache::charts_fingerprint(&charts);
        if after_fp != before_fp {
            self.wb.sheets[self.sheet_idx].charts = SharedList::from(charts);
        }
    }

    /// 打开「智能新建图表向导」：单格自动扩选当前数据区，支持未保存的空白工作簿！
    pub(crate) fn open_chart_create_wizard(&mut self, preset_kind: Option<ChartKind>) {
        let mut rect = self.sel_rect();
        if rect.0 == rect.2 && rect.1 == rect.3 {
            let expanded = Self::current_region_fast(self.wb.sheet(self.sheet_idx), rect.0, rect.1);
            if expanded.2 > expanded.0 || expanded.3 > expanded.1 {
                rect = expanded;
            }
        }
        if rect.2 <= rect.0 && rect.3 <= rect.1 {
            self.status = "请先选中包含数据的区域（或点击数据表内任意单元格）再插入图表".into();
            return;
        }
        let sh = self.wb.sheet(self.sheet_idx);
        let sheet_name = sh.name.clone();
        let (profile, recs) = analyze_selection(sh, rect, Some(&self.wb.styles), self.wb.date_system);
        let chosen_kind = preset_kind
            .or_else(|| recs.first().map(|r| r.kind))
            .unwrap_or(ChartKind::Column);
        let default_title = recs
            .iter()
            .find(|r| r.kind == chosen_kind)
            .map(|r| r.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| chosen_kind.label().to_string());
        let mut preview = build_smart_chart(
            sh,
            &sheet_name,
            rect,
            &default_title,
            chosen_kind,
            false,
            Some(&self.wb.styles),
            self.wb.date_system,
        );
        preview.style.theme = ChartThemeId::CellanModern;
        let source_text = sheetcore::table::a1(rect);
        let gallery_cat = find_category_index(chosen_kind);
        self.chart_studio.wizard = Some(ChartCreateWizard {
            source_sheet_idx: self.sheet_idx,
            source_sheet_name: sheet_name,
            source_rect: rect,
            source_text,
            profile,
            recs,
            selected_kind: chosen_kind,
            switch_row_col: false,
            theme: ChartThemeId::CellanModern,
            title: default_title,
            subtitle: String::new(),
            highlight_extrema: false,
            show_growth_badge: false,
            show_summary_bar: false,
            gallery_cat,
            preview_chart: preview,
        });
    }

    fn rebuild_wizard_preview(&self, w: &mut ChartCreateWizard) {
        let Some(sh) = self.wb.sheets.get(w.source_sheet_idx) else {
            return;
        };
        let mut ch = build_smart_chart(
            sh,
            &w.source_sheet_name,
            w.source_rect,
            &w.title,
            w.selected_kind,
            w.switch_row_col,
            Some(&self.wb.styles),
            self.wb.date_system,
        );
        ch.subtitle = w.subtitle.clone();
        ch.style.theme = w.theme;
        ch.analytics.highlight_extrema = w.highlight_extrema;
        ch.analytics.show_growth_badge = w.show_growth_badge;
        ch.analytics.show_summary_bar = w.show_summary_bar;
        w.preview_chart = ch;
    }

    /// 打开右侧 Chart Studio 设计器（自动关闭透视表侧边栏避免抢占空间）
    pub(crate) fn open_chart_studio_for_selected(&mut self) {
        if self.chart_at_sel().is_none() {
            if !self.wb.sheet(self.sheet_idx).charts.is_empty() {
                self.chart_sel = Some(0);
            } else {
                self.status = "当前工作表没有可设计的图表".into();
                return;
            }
        }
        self.pivot_ui.sidebar_open = false;
        self.chart_studio.sidebar_open = true;
    }

    /// 立即向当前表插入一张纯内存图表（支持未保存工作簿，进入撤销栈，自动选中并打开设计器）。
    pub(crate) fn insert_chart_in_memory(&mut self, mut ch: Chart) -> usize {
        self.flush_chart_pending_undo();
        let sh_idx = self.sheet_idx;
        let existing_count = self.wb.sheets[sh_idx].charts.len() as u32;
        // 若锚点与现有图表重叠，向右下错开，避免新旧图表完全叠在一起
        let offset_r = (existing_count % 5) * 2;
        let offset_c = (existing_count % 5) * 2;
        if ch.anchor == (0, 0, 0, 0) || ch.anchor.2 <= ch.anchor.0 || ch.anchor.3 <= ch.anchor.1 {
            ch.anchor = (1 + offset_r, 4 + offset_c, 16 + offset_r, 12 + offset_c);
        } else if existing_count > 0 {
            let collides = self.wb.sheets[sh_idx]
                .charts
                .iter()
                .any(|c| c.anchor == ch.anchor);
            if collides {
                ch.anchor = (
                    ch.anchor.0 + 2,
                    ch.anchor.1 + 1,
                    ch.anchor.2 + 2,
                    ch.anchor.3 + 1,
                );
            }
        }
        ch.content_dirty = true;
        ch.sync_axis_compat();

        let charts_before = self.wb.sheets[sh_idx].charts.clone();
        let deleted_before = self.wb.sheets[sh_idx].deleted_charts.clone();
        let kind_label = ch.kind.label();
        let series_cnt = ch.series.len();

        let sh = self.wb.sheet_mut(sh_idx);
        sh.charts.push(ch);
        let new_idx = sh.charts.len() - 1;
        sh.charts_dirty = true;
        let charts_after = sh.charts.clone();
        let deleted_after = sh.deleted_charts.clone();

        self.commit_chart_tx(
            format!("插入{kind_label}"),
            (sh_idx, self.sel.0, self.sel.1),
            ChartUndoEdit {
                sheet: sh_idx,
                charts_before,
                charts_after,
                deleted_before,
                deleted_after,
            },
        );
        self.chart_sel = Some(new_idx);
        self.pic_sel = None;
        self.tb_sel = None;
        self.pivot_ui.sidebar_open = false;
        self.chart_studio.sidebar_open = true;
        self.chart_studio.selected_series = 0;
        self.status = format!("已插入{kind_label}（{series_cnt} 个系列，Ctrl+Z 可撤销）");
        new_idx
    }

    /// 修改指定下标的图表并立即写入单条撤销记录（按钮、复选框、下拉选择等离散动作使用）。
    pub(crate) fn mutate_chart_at(
        &mut self,
        idx: usize,
        label: &str,
        f: impl FnOnce(&mut Chart) -> bool,
    ) -> bool {
        self.flush_chart_pending_undo();
        let sh_idx = self.sheet_idx;
        if idx >= self.wb.sheets[sh_idx].charts.len() {
            return false;
        }
        let charts_before = self.wb.sheets[sh_idx].charts.clone();
        let deleted_before = self.wb.sheets[sh_idx].deleted_charts.clone();
        let changed = {
            let sh = self.wb.sheet_mut(sh_idx);
            let ch = &mut sh.charts[idx];
            if f(ch) {
                ch.content_dirty = true;
                ch.sync_axis_compat();
                sh.charts_dirty = true;
                true
            } else {
                false
            }
        };
        if !changed {
            return false;
        }
        let charts_after = self.wb.sheets[sh_idx].charts.clone();
        let deleted_after = self.wb.sheets[sh_idx].deleted_charts.clone();
        self.commit_chart_tx(
            label,
            (sh_idx, self.sel.0, self.sel.1),
            ChartUndoEdit {
                sheet: sh_idx,
                charts_before,
                charts_after,
                deleted_before,
                deleted_after,
            },
        );
        true
    }

    pub(crate) fn mutate_selected_chart(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut Chart) -> bool,
    ) -> bool {
        let Some(ci) = self.chart_sel else {
            return false;
        };
        self.mutate_chart_at(ci, label, f)
    }

    /// 连续控件（输入框/滑块/取色器）修改前捕获一次基准快照，保证整个输入/拖动过程只生成 1 条撤销记录。
    fn capture_chart_pending_undo(&mut self, label: &str) {
        let sh_idx = self.sheet_idx;
        if self
            .chart_studio
            .pending_undo
            .as_ref()
            .is_none_or(|(s, l, _, _)| *s != sh_idx || l != label)
        {
            self.flush_chart_pending_undo();
            let sh = &self.wb.sheets[sh_idx];
            self.chart_studio.pending_undo = Some((
                sh_idx,
                label.to_string(),
                sh.charts.clone(),
                sh.deleted_charts.clone(),
            ));
        }
    }

    pub(crate) fn flush_chart_pending_undo(&mut self) {
        let Some((sh_idx, label, charts_before, deleted_before)) =
            self.chart_studio.pending_undo.take()
        else {
            return;
        };
        let Some(sh) = self.wb.sheets.get(sh_idx) else {
            return;
        };
        if sh.charts == charts_before && sh.deleted_charts == deleted_before {
            return;
        }
        let charts_after = sh.charts.clone();
        let deleted_after = sh.deleted_charts.clone();
        self.commit_chart_tx(
            label,
            (sh_idx, self.sel.0, self.sel.1),
            ChartUndoEdit {
                sheet: sh_idx,
                charts_before,
                charts_after,
                deleted_before,
                deleted_after,
            },
        );
    }

    /// 删除指定下标的图表（纯内存 + 登记 `deleted_charts` 以便保存时清理 zip 部件 + 完整可撤销）。
    pub(crate) fn delete_chart_at(&mut self, idx: usize) {
        self.flush_chart_pending_undo();
        let sh_idx = self.sheet_idx;
        if idx >= self.wb.sheets[sh_idx].charts.len() {
            return;
        }
        let charts_before = self.wb.sheets[sh_idx].charts.clone();
        let deleted_before = self.wb.sheets[sh_idx].deleted_charts.clone();

        let removed = {
            let sh = self.wb.sheet_mut(sh_idx);
            let ch = sh.charts.remove(idx);
            if !ch.part.is_empty() && !ch.drawing.is_empty() {
                sh.deleted_charts.push((ch.drawing.clone(), ch.part.clone()));
            }
            sh.charts_dirty = true;
            ch
        };
        let charts_after = self.wb.sheets[sh_idx].charts.clone();
        let deleted_after = self.wb.sheets[sh_idx].deleted_charts.clone();
        self.commit_chart_tx(
            "删除图表",
            (sh_idx, self.sel.0, self.sel.1),
            ChartUndoEdit {
                sheet: sh_idx,
                charts_before,
                charts_after,
                deleted_before,
                deleted_after,
            },
        );
        if self.chart_sel == Some(idx) {
            self.chart_sel = None;
            self.chart_studio.sidebar_open = false;
        } else if let Some(sel) = self.chart_sel
            && sel > idx
        {
            self.chart_sel = Some(sel - 1);
        }
        let name = if removed.title.is_empty() {
            removed.kind.label()
        } else {
            removed.title.as_str()
        };
        self.status = format!("已删除图表「{name}」（Ctrl+Z 可恢复）");
    }

    pub(crate) fn delete_selected_chart(&mut self) {
        if let Some(ci) = self.chart_sel {
            self.delete_chart_at(ci);
        }
    }

    /// 克隆指定图表并向右下错开 2 行 1 列（自动清空 `part`/`drawing` 以便作为新部件保存）。
    pub(crate) fn duplicate_chart_at(&mut self, idx: usize) {
        let Some(orig) = self.wb.sheet(self.sheet_idx).charts.get(idx).cloned() else {
            return;
        };
        let mut copy = orig;
        copy.uid = sheetcore::charts::ChartUid::next();
        copy.part.clear();
        copy.drawing.clear();
        copy.is_chartex = false;
        copy.anchor = (
            copy.anchor.0 + 2,
            copy.anchor.1 + 1,
            copy.anchor.2 + 2,
            copy.anchor.3 + 1,
        );
        if !copy.title.is_empty() {
            copy.title = format!("{} (副本)", copy.title);
        }
        self.insert_chart_in_memory(copy);
        self.status = "已复制图表副本".into();
    }

    /// 一键行列互换（Switch Row / Column）
    pub(crate) fn switch_selected_chart_row_col(&mut self) {
        let Some(ci) = self.chart_sel else {
            return;
        };
        let Some(mut draft) = self.wb.sheet(self.sheet_idx).charts.get(ci).cloned() else {
            return;
        };
        let ok = switch_row_column(
            &mut draft,
            &self.wb.sheets,
            self.sheet_idx,
            Some(&self.wb.styles),
            self.wb.date_system,
        );
        if !ok {
            self.status = "无法识别该图表的连续源数据区域，请在「数据」页手动指定数据区域".into();
            return;
        }
        let sw = draft.style.switch_row_col;
        let n_ser = draft.series.len();
        self.mutate_chart_at(ci, "图表行列互换", |ch| {
            *ch = draft;
            true
        });
        self.status = format!(
            "已切换为{}（{} 个系列）",
            if sw { "按行作系列" } else { "按列作系列" },
            n_ser
        );
    }

    /// 轮换 8 套出版级配色主题
    pub(crate) fn cycle_selected_chart_theme(&mut self) {
        let mut next_name = "";
        let ok = self.mutate_selected_chart("切换图表主题", |ch| {
            let all = ChartThemeId::all();
            let pos = all.iter().position(|&t| t == ch.style.theme).unwrap_or(0);
            let next = all[(pos + 1) % all.len()];
            ch.style.theme = next;
            next_name = next.label();
            true
        });
        if ok {
            self.status = format!("图表主题已切换为「{next_name}」");
        }
    }

    /// 快捷开关数据标签
    pub(crate) fn toggle_selected_chart_labels(&mut self) {
        let mut now_on = false;
        let ok = self.mutate_selected_chart("切换数据标签", |ch| {
            ch.show_labels = !ch.show_labels;
            now_on = ch.show_labels;
            true
        });
        if ok {
            self.status = if now_on {
                "已显示图表数据标签".into()
            } else {
                "已隐藏图表数据标签".into()
            };
        }
    }

    /// 快捷开关首条线性趋势线（含回归公式与 R²）
    pub(crate) fn toggle_selected_chart_trendline(&mut self) {
        let mut added = false;
        let ok = self.mutate_selected_chart("切换趋势线", |ch| {
            let Some(s) = ch.series.first_mut() else {
                return false;
            };
            if s.trendlines.is_empty() {
                s.trendlines.push(Trendline {
                    kind: TrendlineKind::Linear,
                    show_equation: true,
                    show_r2: true,
                    ..Default::default()
                });
                added = true;
            } else {
                s.trendlines.clear();
                added = false;
            }
            true
        });
        if ok {
            self.status = if added {
                "已添加线性回归趋势线（显示公式与 R²）".into()
            } else {
                "已移除趋势线".into()
            };
        }
    }

    /// 导出出版级无损矢量 SVG 文件
    pub(crate) fn export_chart_svg_dialog(&mut self, idx: usize) {
        let Some(ch) = self.wb.sheet(self.sheet_idx).charts.get(idx).cloned() else {
            return;
        };
        let default_name = if ch.title.trim().is_empty() {
            format!("chart_{}.svg", idx + 1)
        } else {
            let clean: String = ch
                .title
                .chars()
                .map(|c| match c {
                    '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                    _ => c,
                })
                .collect();
            format!("{clean}.svg")
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("矢量图 SVG", &["svg"])
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };
        let svg = sheetcore::chart_svg::render_chart_svg(&ch, 960.0, 560.0, self.wb.date_system);
        match std::fs::write(&path, svg) {
            Ok(()) => {
                self.status = format!("已导出高清矢量图：{}", path.display());
            }
            Err(e) => {
                self.status = format!("导出 SVG 失败：{e}");
            }
        }
    }

    /// 分发图表实时交互层（图例点击/双击隔离、快捷浮条按钮、双击标题重命名）
    pub(crate) fn apply_chart_interaction(
        &mut self,
        ci: usize,
        out: chart_view::ChartInteractionOut,
    ) {
        if let Some(si) = out.isolated_legend_series {
            self.mutate_chart_at(ci, "聚焦图例系列", |ch| {
                if si >= ch.series.len() {
                    return false;
                }
                let others_hidden = ch
                    .series
                    .iter()
                    .enumerate()
                    .all(|(i, s)| i == si || s.hidden);
                if others_hidden && !ch.series[si].hidden {
                    for s in &mut ch.series {
                        s.hidden = false;
                    }
                } else {
                    for (i, s) in ch.series.iter_mut().enumerate() {
                        s.hidden = i != si;
                    }
                }
                true
            });
            return;
        }
        if let Some(si) = out.toggled_legend_series {
            self.mutate_chart_at(ci, "切换系列显隐", |ch| {
                let visible_cnt = ch.series.iter().filter(|x| !x.hidden).count();
                if let Some(s) = ch.series.get_mut(si) {
                    if !s.hidden && visible_cnt <= 1 {
                        // 不允许把最后一个可见系列也隐藏导致全空
                        return false;
                    }
                    s.hidden = !s.hidden;
                    true
                } else {
                    false
                }
            });
            return;
        }
        if out.title_double_clicked {
            if let Some(ch) = self.wb.sheet(self.sheet_idx).charts.get(ci) {
                self.chart_sel = Some(ci);
                self.chart_studio.inline_title_modal =
                    Some((ci, ch.title.clone(), ch.subtitle.clone()));
            }
            return;
        }
        if let Some(act) = out.quick_action {
            self.chart_sel = Some(ci);
            match act {
                chart_view::ChartQuickAction::CycleTheme => self.cycle_selected_chart_theme(),
                chart_view::ChartQuickAction::SwitchRowCol => self.switch_selected_chart_row_col(),
                chart_view::ChartQuickAction::ToggleLabels => self.toggle_selected_chart_labels(),
                chart_view::ChartQuickAction::ToggleTrendline => {
                    self.toggle_selected_chart_trendline()
                }
                chart_view::ChartQuickAction::OpenStudio => self.open_chart_studio_for_selected(),
                chart_view::ChartQuickAction::ExportSvg => self.export_chart_svg_dialog(ci),
                chart_view::ChartQuickAction::Duplicate => self.duplicate_chart_at(ci),
                chart_view::ChartQuickAction::Delete => self.delete_chart_at(ci),
            }
        }
    }

    /// 选中图表时，在工作表网格上用彩色边框与轻透填充高亮其分类、系列名、数值与气泡源区域（对齐并超越 Excel）。
    pub(crate) fn paint_selected_chart_source_highlight(
        &self,
        painter: &egui::Painter,
        clip: Rect,
        px_of: &dyn Fn(u32) -> f32,
        py_of: &dyn Fn(u32) -> f32,
    ) {
        let Some(ch) = self.chart_at_sel() else {
            return;
        };
        if matches!(ch.kind, ChartKind::Unknown) {
            return;
        }
        let cur_sheet = &self.wb.sheet(self.sheet_idx).name;
        let p = painter.with_clip_rect(clip);
        let draw_box = |r1: u32, c1: u32, r2: u32, c2: u32, color: Color32, fill_alpha: u8| {
            let rect = Rect::from_min_max(
                Pos2::new(px_of(c1), py_of(r1)),
                Pos2::new(px_of(c2 + 1), py_of(r2 + 1)),
            );
            if !rect.intersects(clip) {
                return;
            }
            let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), fill_alpha);
            p.rect_filled(rect, 0.0, fill);
            p.rect_stroke(rect, 0.0, Stroke::new(1.75, color), egui::StrokeKind::Inside);
        };

        let mut drawn_cats = std::collections::HashSet::new();
        for (si, s) in ch.series.iter().enumerate() {
            if s.hidden {
                continue;
            }
            let sc = rgb(series_rgb(ch, si));
            if let Some((sh_opt, (r1, c1, r2, c2))) =
                sheetcore::charts::parse_ref_rect(&s.cat_ref)
                && sh_opt.as_deref().is_none_or(|n| n.eq_ignore_ascii_case(cur_sheet))
                && drawn_cats.insert((r1, c1, r2, c2))
            {
                draw_box(r1, c1, r2, c2, Color32::from_rgb(139, 92, 246), 18);
            }
            if let Some((sh_opt, (r1, c1, r2, c2))) =
                sheetcore::charts::parse_ref_rect(&s.name_ref)
                && sh_opt.as_deref().is_none_or(|n| n.eq_ignore_ascii_case(cur_sheet))
            {
                draw_box(r1, c1, r2, c2, Color32::from_rgb(16, 185, 129), 22);
            }
            if let Some((sh_opt, (r1, c1, r2, c2))) =
                sheetcore::charts::parse_ref_rect(&s.val_ref)
                && sh_opt.as_deref().is_none_or(|n| n.eq_ignore_ascii_case(cur_sheet))
            {
                draw_box(r1, c1, r2, c2, sc, 24);
            }
            if let Some((sh_opt, (r1, c1, r2, c2))) =
                sheetcore::charts::parse_ref_rect(&s.size_ref)
                && sh_opt.as_deref().is_none_or(|n| n.eq_ignore_ascii_case(cur_sheet))
            {
                draw_box(r1, c1, r2, c2, Color32::from_rgb(245, 158, 11), 20);
            }
        }
    }

    // =========================================================================
    // 1. 智能新建图表向导（数据画像 + Top-N 推荐 + 22 种全图库 + 实时 GPU 预览）
    // =========================================================================
    pub(crate) fn draw_chart_create_wizard(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.chart_studio.wizard.take() else {
            return;
        };
        let mut open = true;
        let mut commit = false;
        let mut cancel = false;
        let mut needs_rebuild = false;
        let mut reanalyze_range = false;

        ui_kit::window(ctx, "新建智能图表", 860.0)
            .collapsible(false)
            .resizable(true)
            .default_size([860.0, 560.0])
            .open(&mut open)
            .show(ctx, |ui| {
                // 顶部数据区域 + 智能数据画像条
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("数据区域").strong());
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut w.source_text)
                            .desired_width(170.0)
                            .hint_text("例如 A1:D12"),
                    );
                    if ui_kit::secondary_button(ui, "重新分析").clicked()
                        || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        reanalyze_range = true;
                    }
                    ui.separator();
                    let badge = |ui: &mut egui::Ui, txt: &str, col: Color32| {
                        egui::Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 22))
                            .stroke(Stroke::new(
                                1.0,
                                Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 90),
                            ))
                            .corner_radius(4.0)
                            .inner_margin(egui::Margin::symmetric(6, 2))
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(txt).size(11.0).color(col));
                            });
                    };
                    badge(
                        ui,
                        &format!("{} 行 × {} 列", w.profile.rows, w.profile.cols),
                        Color32::from_rgb(37, 99, 235),
                    );
                    if w.profile.cat_looks_temporal {
                        badge(ui, "时间序列", Color32::from_rgb(14, 165, 233));
                    }
                    if w.profile.scale_ratio >= 5.0 {
                        badge(
                            ui,
                            &format!("量级差 {:.0}× · 宜双轴", w.profile.scale_ratio),
                            Color32::from_rgb(217, 119, 6),
                        );
                    }
                    if w.profile.has_negative {
                        badge(ui, "含正负变动", Color32::from_rgb(220, 38, 38));
                    }
                    if w.profile.monotonic_decreasing {
                        badge(ui, "逐级递减 · 宜漏斗", Color32::from_rgb(124, 58, 237));
                    }
                });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

                ui.columns(2, |cols| {
                    // 左侧：智能推荐 + 22 种分类图型库
                    cols[0].vertical(|ui| {
                        ui.label(egui::RichText::new("智能推荐").strong().size(13.0));
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical()
                            .id_salt("chart_wiz_left")
                            .max_height(410.0)
                            .show(ui, |ui| {
                                for rec in &w.recs {
                                    let sel = w.selected_kind == rec.kind;
                                    let fill = if sel {
                                        Color32::from_rgba_unmultiplied(37, 99, 235, 22)
                                    } else {
                                        Color32::from_rgb(248, 250, 252)
                                    };
                                    let stroke = if sel {
                                        Stroke::new(1.5, ui_kit::ACCENT)
                                    } else {
                                        Stroke::new(1.0, Color32::from_rgb(226, 232, 240))
                                    };
                                    let resp = egui::Frame::NONE
                                        .fill(fill)
                                        .stroke(stroke)
                                        .corner_radius(6.0)
                                        .inner_margin(egui::Margin::symmetric(8, 6))
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    egui::RichText::new(rec.kind.label())
                                                        .strong()
                                                        .size(12.5),
                                                );
                                                egui::Frame::NONE
                                                    .fill(Color32::from_rgb(224, 242, 254))
                                                    .corner_radius(4.0)
                                                    .inner_margin(egui::Margin::symmetric(5, 1))
                                                    .show(ui, |ui| {
                                                        ui.label(
                                                            egui::RichText::new(rec.badge)
                                                                .size(10.0)
                                                                .color(Color32::from_rgb(3, 105, 161)),
                                                        );
                                                    });
                                            });
                                            ui.label(
                                                egui::RichText::new(&rec.reason)
                                                    .size(11.0)
                                                    .color(Color32::from_rgb(100, 116, 139)),
                                            );
                                        })
                                        .response
                                        .interact(Sense::click());
                                    if resp.clicked() {
                                        w.selected_kind = rec.kind;
                                        w.gallery_cat = find_category_index(rec.kind);
                                        needs_rebuild = true;
                                    }
                                    ui.add_space(4.0);
                                }

                                ui.add_space(6.0);
                                ui.separator();
                                ui.add_space(6.0);
                                ui.label(egui::RichText::new("全部 22 种专业图型").strong().size(13.0));
                                ui.add_space(4.0);
                                ui.horizontal_wrapped(|ui| {
                                    for (ci, (cat_name, _)) in CHART_CATEGORIES.iter().enumerate() {
                                        if ui.selectable_label(w.gallery_cat == ci, *cat_name).clicked() {
                                            w.gallery_cat = ci;
                                        }
                                    }
                                });
                                ui.add_space(4.0);
                                let kinds = CHART_CATEGORIES[w.gallery_cat].1;
                                ui.horizontal_wrapped(|ui| {
                                    for &k in kinds {
                                        let active = w.selected_kind == k;
                                        let btn = egui::Button::new(
                                            egui::RichText::new(k.label()).size(11.5),
                                        )
                                        .selected(active)
                                        .min_size(Vec2::new(112.0, 26.0));
                                        if ui.add(btn).clicked() {
                                            w.selected_kind = k;
                                            needs_rebuild = true;
                                        }
                                    }
                                });
                            });
                    });

                    // 右侧：大尺寸实时交互预览 + 主题色板 + 标题/增强选项
                    cols[1].vertical(|ui| {
                        let avail_w = ui.available_width().max(320.0);
                        let (prev_rect, prev_resp) =
                            ui.allocate_exact_size(Vec2::new(avail_w, 260.0), Sense::hover());
                        let painter = ui.painter_at(prev_rect);
                        let shapes = chart_view::build_chart_shapes_in(
                            &painter,
                            prev_rect,
                            &w.preview_chart,
                            self.wb.date_system,
                        );
                        painter.extend(shapes);
                        let ptr = prev_resp.hover_pos();
                        let _ = chart_view::paint_chart_interactive_overlay(
                            &painter,
                            prev_rect,
                            &w.preview_chart,
                            ptr,
                            None,
                            None,
                            false,
                            self.wb.date_system,
                        );

                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("出版配色主题").strong().size(12.0));
                        ui.add_space(3.0);
                        ui.horizontal_wrapped(|ui| {
                            for &tid in ChartThemeId::all() {
                                let spec = theme_spec(tid);
                                let sel = w.theme == tid;
                                let (r, resp) =
                                    ui.allocate_exact_size(Vec2::new(92.0, 24.0), Sense::click());
                                let p = ui.painter();
                                p.rect_filled(r, 4.0, rgb(spec.bg));
                                p.rect_stroke(
                                    r,
                                    4.0,
                                    if sel {
                                        Stroke::new(1.8, ui_kit::ACCENT)
                                    } else {
                                        Stroke::new(1.0, rgb(spec.border))
                                    },
                                    egui::StrokeKind::Inside,
                                );
                                for k in 0..3 {
                                    p.circle_filled(
                                        Pos2::new(r.min.x + 8.0 + k as f32 * 8.0, r.center().y),
                                        3.0,
                                        rgb(spec.palette[k]),
                                    );
                                }
                                p.text(
                                    Pos2::new(r.min.x + 32.0, r.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    tid.label(),
                                    FontId::proportional(10.0),
                                    rgb(spec.title_fg),
                                );
                                if resp.clicked() {
                                    w.theme = tid;
                                    needs_rebuild = true;
                                }
                            }
                        });

                        ui.add_space(6.0);
                        egui::Grid::new("chart_wiz_opts")
                            .num_columns(2)
                            .spacing([8.0, 5.0])
                            .show(ui, |ui| {
                                ui.label("图表标题");
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut w.title)
                                            .desired_width(240.0),
                                    )
                                    .changed()
                                {
                                    needs_rebuild = true;
                                }
                                ui.end_row();
                                ui.label("副标题");
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut w.subtitle)
                                            .desired_width(240.0)
                                            .hint_text("可选：单位、时间口径或结论摘要"),
                                    )
                                    .changed()
                                {
                                    needs_rebuild = true;
                                }
                                ui.end_row();
                            });
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui.checkbox(&mut w.switch_row_col, "行列互换").changed() {
                                needs_rebuild = true;
                            }
                            if ui.checkbox(&mut w.highlight_extrema, "高亮极值").changed() {
                                needs_rebuild = true;
                            }
                            if ui.checkbox(&mut w.show_growth_badge, "增幅徽章").changed() {
                                needs_rebuild = true;
                            }
                            if ui.checkbox(&mut w.show_summary_bar, "底部统计条").changed() {
                                needs_rebuild = true;
                            }
                        });
                    });
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "当前预览：{} · {} 个系列（插入后可在右侧 Chart Studio 实时精调）",
                            w.preview_chart.kind.label(),
                            w.preview_chart.series.len()
                        ))
                        .size(11.0)
                        .color(Color32::from_rgb(100, 116, 139)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_kit::secondary_button(ui, "取消").clicked() {
                            cancel = true;
                        }
                        if ui_kit::primary_button(ui, "插入图表").clicked() {
                            commit = true;
                        }
                    });
                });
            });

        if reanalyze_range {
            if let Some(rect) = sheetcore::table::parse_a1(&w.source_text) {
                w.source_rect = rect;
                if let Some(sh) = self.wb.sheets.get(w.source_sheet_idx) {
                    let (prof, recs) =
                        analyze_selection(sh, rect, Some(&self.wb.styles), self.wb.date_system);
                    w.profile = prof;
                    w.recs = recs;
                }
                needs_rebuild = true;
            } else {
                self.status = "数据区域格式不对，请输入例如 A1:D12".into();
            }
        }
        if needs_rebuild {
            self.rebuild_wizard_preview(&mut w);
        }
        if commit {
            self.insert_chart_in_memory(w.preview_chart);
        } else if !cancel && open {
            self.chart_studio.wizard = Some(w);
        }
    }

    // =========================================================================
    // 2. 右侧可停靠「Chart Studio 设计器」—— 实时所见即所得，全操作可撤销
    // =========================================================================
    pub(crate) fn draw_chart_studio_sidebar(&mut self, ctx: &egui::Context) {
        if !self.chart_studio.sidebar_open {
            self.flush_chart_pending_undo();
            return;
        }
        let Some(ci) = self.chart_sel else {
            self.chart_studio.sidebar_open = false;
            self.flush_chart_pending_undo();
            return;
        };
        if ci >= self.wb.sheet(self.sheet_idx).charts.len() {
            self.chart_sel = None;
            self.chart_studio.sidebar_open = false;
            self.flush_chart_pending_undo();
            return;
        }
        // 当鼠标松开且没有输入焦点时，收口连续控件的待提交撤销快照
        let any_kb_focus = ctx.memory(|m| m.focused().is_some());
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        if !any_kb_focus && !pointer_down {
            self.flush_chart_pending_undo();
        }

        let mut close_panel = false;
        let mut trigger_switch_rc = false;
        let mut trigger_export_svg = false;
        let mut trigger_duplicate = false;
        let mut trigger_delete = false;
        let mut trigger_apply_source = false;
        let mut trigger_refresh_refs = false;

        let chart_snapshot = self.wb.sheet(self.sheet_idx).charts[ci].clone();
        if self.chart_studio.source_edit_key != Some((self.sheet_idx, ci)) {
            self.chart_studio.source_edit_key = Some((self.sheet_idx, ci));
            self.chart_studio.source_edit_buf = if !chart_snapshot.source_range.is_empty() {
                chart_snapshot.source_range.clone()
            } else if let Some((_, rect)) = infer_source_rect(&chart_snapshot) {
                sheetcore::table::a1(rect)
            } else {
                String::new()
            };
        }

        egui::SidePanel::right("chart_studio_panel")
            .resizable(true)
            .default_width(318.0)
            .width_range(276.0..=440.0)
            .frame(
                egui::Frame::NONE
                    .fill(Color32::WHITE)
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(224, 229, 236)))
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ctx, |ui| {
                // 标题头
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Chart Studio 设计器")
                            .strong()
                            .size(13.5)
                            .color(Color32::from_rgb(24, 34, 52)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").on_hover_text("关闭设计器").clicked() {
                            close_panel = true;
                        }
                    });
                });
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {} 个系列",
                        chart_snapshot.kind.label(),
                        chart_snapshot.series.len()
                    ))
                    .size(11.0)
                    .color(Color32::from_rgb(100, 116, 139)),
                );
                ui.add_space(6.0);

                if matches!(chart_snapshot.kind, ChartKind::Unknown) {
                    ui.separator();
                    ui.add_space(8.0);
                    ui.label("该图表为外部高级/3D 部件，CELLAN 已无损保留原 XML 与样式，不可在此覆盖结构。");
                    ui.add_space(8.0);
                    if ui_kit::secondary_button(ui, "删除图表").clicked() {
                        trigger_delete = true;
                    }
                    return;
                }

                // 五大功能页签
                ui.horizontal(|ui| {
                    for (tab, name) in [
                        (ChartStudioTab::Style, "外观"),
                        (ChartStudioTab::Data, "数据"),
                        (ChartStudioTab::Axes, "坐标轴"),
                        (ChartStudioTab::Labels, "标签"),
                        (ChartStudioTab::Analytics, "分析"),
                    ] {
                        let sel = self.chart_studio.active_tab == tab;
                        if ui.selectable_label(sel, egui::RichText::new(name).size(12.0)).clicked() {
                            self.chart_studio.active_tab = tab;
                        }
                    }
                });
                ui.separator();
                ui.add_space(4.0);

                egui::ScrollArea::vertical()
                    .id_salt("chart_studio_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.chart_studio.active_tab {
                        ChartStudioTab::Style => {
                            self.draw_studio_tab_style(
                                ui,
                                ci,
                                &chart_snapshot,
                                &mut trigger_switch_rc,
                            );
                        }
                        ChartStudioTab::Data => {
                            self.draw_studio_tab_data(
                                ui,
                                ci,
                                &chart_snapshot,
                                &mut trigger_switch_rc,
                                &mut trigger_apply_source,
                                &mut trigger_refresh_refs,
                            );
                        }
                        ChartStudioTab::Axes => {
                            self.draw_studio_tab_axes(ui, ci, &chart_snapshot);
                        }
                        ChartStudioTab::Labels => {
                            self.draw_studio_tab_labels(ui, ci, &chart_snapshot);
                        }
                        ChartStudioTab::Analytics => {
                            self.draw_studio_tab_analytics(
                                ui,
                                ci,
                                &chart_snapshot,
                                &mut trigger_export_svg,
                                &mut trigger_duplicate,
                                &mut trigger_delete,
                            );
                        }
                    });
            });

        if trigger_switch_rc {
            self.switch_selected_chart_row_col();
        }
        if trigger_apply_source {
            let txt = self.chart_studio.source_edit_buf.trim().to_string();
            let parsed = sheetcore::charts::parse_ref_rect(&txt)
                .or_else(|| sheetcore::table::parse_a1(&txt).map(|r| (None, r)));
            if let Some((sh_opt, rect)) = parsed {
                let target_idx = sh_opt
                    .as_deref()
                    .and_then(|n| self.wb.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(n)))
                    .unwrap_or(self.sheet_idx);
                let sh_name = self.wb.sheets[target_idx].name.clone();
                let rebuilt = build_smart_chart(
                    &self.wb.sheets[target_idx],
                    &sh_name,
                    rect,
                    &chart_snapshot.title,
                    chart_snapshot.kind,
                    chart_snapshot.style.switch_row_col,
                    Some(&self.wb.styles),
                    self.wb.date_system,
                );
                if !rebuilt.series.is_empty() {
                    self.mutate_chart_at(ci, "更改图表数据区域", |ch| {
                        let old_colors: Vec<_> = ch.series.iter().map(|s| s.color).collect();
                        ch.series = rebuilt.series;
                        for (i, s) in ch.series.iter_mut().enumerate() {
                            if let Some(Some(c)) = old_colors.get(i) {
                                s.color = Some(*c);
                            }
                        }
                        ch.source_range = rebuilt.source_range;
                        true
                    });
                    self.status = format!("已重新绑定数据区域 {txt}");
                } else {
                    self.status = "该区域未解析出有效数值系列".into();
                }
            } else {
                self.status = "区域格式无效，请输入如 A1:D10 或 Sheet1!A1:D10".into();
            }
        }
        if trigger_refresh_refs {
            let sys = self.wb.date_system;
            let styles = self.wb.styles.clone();
            let orig = self.wb.sheets[self.sheet_idx].charts[ci].clone();
            let mut ch_copy = orig.clone();
            sheetcore::charts::refresh_chart_from_sheets(
                &mut ch_copy,
                &self.wb.sheets,
                self.sheet_idx,
                Some(&styles),
                sys,
            );
            if ch_copy != orig {
                self.mutate_chart_at(ci, "刷新图表系列引用", |ch| {
                    *ch = ch_copy;
                    true
                });
                self.status = "已按引用刷新所有系列数据".into();
            } else {
                self.status = "系列数据已是最新".into();
            }
        }
        if trigger_export_svg {
            self.export_chart_svg_dialog(ci);
        }
        if trigger_duplicate {
            self.duplicate_chart_at(ci);
        }
        if trigger_delete {
            self.delete_chart_at(ci);
        }
        if close_panel {
            self.flush_chart_pending_undo();
            self.chart_studio.sidebar_open = false;
        }
    }

    fn live_edit_chart(&mut self, ci: usize, label: &str, f: impl FnOnce(&mut Chart)) {
        self.capture_chart_pending_undo(label);
        let sh = self.wb.sheet_mut(self.sheet_idx);
        if let Some(ch) = sh.charts.get_mut(ci) {
            f(ch);
            ch.content_dirty = true;
            ch.sync_axis_compat();
            sh.charts_dirty = true;
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
        }
    }

    fn draw_studio_tab_style(
        &mut self,
        ui: &mut egui::Ui,
        ci: usize,
        snap: &Chart,
        trigger_switch_rc: &mut bool,
    ) {
        ui.label(egui::RichText::new("标题与副标题").strong().size(12.0));
        let mut title = snap.title.clone();
        let r_title = ui.add(
            egui::TextEdit::singleline(&mut title)
                .desired_width(ui.available_width())
                .hint_text("主标题"),
        );
        if r_title.changed() {
            self.live_edit_chart(ci, "修改图表标题", |ch| ch.title = title);
        }
        if r_title.lost_focus() {
            self.flush_chart_pending_undo();
        }
        ui.add_space(3.0);
        let mut subtitle = snap.subtitle.clone();
        let r_sub = ui.add(
            egui::TextEdit::singleline(&mut subtitle)
                .desired_width(ui.available_width())
                .hint_text("副标题（单位 / 口径 / 结论摘要）"),
        );
        if r_sub.changed() {
            self.live_edit_chart(ci, "修改图表副标题", |ch| ch.subtitle = subtitle);
        }
        if r_sub.lost_focus() {
            self.flush_chart_pending_undo();
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("图型切换（22 种）").strong().size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("⇄ 行列互换").clicked() {
                    *trigger_switch_rc = true;
                }
            });
        });
        ui.add_space(3.0);
        for (cat_name, kinds) in CHART_CATEGORIES {
            ui.label(
                egui::RichText::new(*cat_name)
                    .size(10.5)
                    .color(Color32::from_rgb(100, 116, 139)),
            );
            ui.horizontal_wrapped(|ui| {
                for &k in *kinds {
                    let active = snap.kind == k;
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new(k.label()).size(11.0))
                                .selected(active),
                        )
                        .clicked()
                        && !active
                    {
                        self.mutate_chart_at(ci, "切换图表类型", |ch| {
                            ch.kind = k;
                            if k == ChartKind::Doughnut {
                                ch.analytics.show_center_kpi = true;
                            }
                            if matches!(k, ChartKind::Waterfall | ChartKind::Funnel) {
                                ch.show_labels = true;
                            }
                            true
                        });
                    }
                }
            });
            ui.add_space(3.0);
        }

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("出版配色主题").strong().size(12.0));
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for &tid in ChartThemeId::all() {
                let spec = theme_spec(tid);
                let sel = snap.style.theme == tid;
                let (r, resp) = ui.allocate_exact_size(Vec2::new(132.0, 26.0), Sense::click());
                let p = ui.painter();
                p.rect_filled(r, 5.0, rgb(spec.bg));
                p.rect_stroke(
                    r,
                    5.0,
                    if sel {
                        Stroke::new(1.8, ui_kit::ACCENT)
                    } else {
                        Stroke::new(1.0, rgb(spec.border))
                    },
                    egui::StrokeKind::Inside,
                );
                for k in 0..4 {
                    p.circle_filled(
                        Pos2::new(r.min.x + 10.0 + k as f32 * 9.0, r.center().y),
                        3.5,
                        rgb(spec.palette[k]),
                    );
                }
                p.text(
                    Pos2::new(r.min.x + 48.0, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    tid.label(),
                    FontId::proportional(10.5),
                    rgb(spec.title_fg),
                );
                if resp.clicked() && !sel {
                    self.mutate_chart_at(ci, "切换图表主题", |ch| {
                        ch.style.theme = tid;
                        true
                    });
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("版面与几何精调").strong().size(12.0));
        let mut rounded = snap.style.rounded_bars;
        if ui.checkbox(&mut rounded, "柱体微圆角").changed() {
            self.mutate_chart_at(ci, "切换柱体圆角", |ch| {
                ch.style.rounded_bars = rounded;
                true
            });
        }
        let mut shadow = snap.style.show_card_shadow;
        if ui.checkbox(&mut shadow, "卡片柔和投影").changed() {
            self.mutate_chart_at(ci, "切换卡片投影", |ch| {
                ch.style.show_card_shadow = shadow;
                true
            });
        }
        let mut trans = snap.style.transparent_bg;
        if ui.checkbox(&mut trans, "透明卡片背景").changed() {
            self.mutate_chart_at(ci, "切换透明背景", |ch| {
                ch.style.transparent_bg = trans;
                true
            });
        }
        let mut vis_only = snap.style.plot_vis_only;
        if ui
            .checkbox(&mut vis_only, "仅绘制可见单元格（跟随筛选隐藏）")
            .changed()
        {
            self.mutate_chart_at(ci, "切换可见单元格策略", |ch| {
                ch.style.plot_vis_only = vis_only;
                true
            });
            self.chart_studio.last_refresh_sig = (u64::MAX, u64::MAX, usize::MAX);
        }

        ui.horizontal(|ui| {
            ui.label("空值处理");
            let bp = snap.style.blank_policy;
            let bp_label = |b: BlankPolicy| match b {
                BlankPolicy::Gap => "空距断开",
                BlankPolicy::Zero => "按 0 绘制",
                BlankPolicy::Span => "直线连接",
            };
            egui::ComboBox::from_id_salt("chart_blank_policy")
                .selected_text(bp_label(bp))
                .show_ui(ui, |ui| {
                    for opt in [BlankPolicy::Gap, BlankPolicy::Zero, BlankPolicy::Span] {
                        if ui.selectable_label(bp == opt, bp_label(opt)).clicked() && bp != opt {
                            self.mutate_chart_at(ci, "修改空值处理", |ch| {
                                ch.style.blank_policy = opt;
                                true
                            });
                        }
                    }
                });
        });

        if matches!(
            snap.kind,
            ChartKind::Column
                | ChartKind::ColumnStacked
                | ChartKind::ColumnStacked100
                | ChartKind::Bar
                | ChartKind::BarStacked
                | ChartKind::BarStacked100
                | ChartKind::Combo
                | ChartKind::Waterfall
        ) {
            let mut gap = snap.style.gap_width as f32;
            let resp = ui.add(
                egui::Slider::new(&mut gap, 20.0..=300.0)
                    .text("分类间距 %")
                    .integer(),
            );
            if resp.changed() {
                self.live_edit_chart(ci, "调整分类间距", |ch| {
                    ch.style.gap_width = gap.round() as u16;
                });
            }
            if resp.drag_stopped() {
                self.flush_chart_pending_undo();
            }
        }

        if matches!(snap.kind, ChartKind::Pie | ChartKind::Doughnut) {
            let mut ang = snap.style.first_slice_ang as f32;
            let r1 = ui.add(
                egui::Slider::new(&mut ang, 0.0..=360.0)
                    .text("起始角°")
                    .integer(),
            );
            if r1.changed() {
                self.live_edit_chart(ci, "调整饼图起始角", |ch| {
                    ch.style.first_slice_ang = ang.round() as u16;
                });
            }
            if r1.drag_stopped() {
                self.flush_chart_pending_undo();
            }
            let mut exp = snap.style.pie_explosion as f32;
            let r2 = ui.add(
                egui::Slider::new(&mut exp, 0.0..=35.0)
                    .text("扇区分离 %")
                    .integer(),
            );
            if r2.changed() {
                self.live_edit_chart(ci, "调整扇区分离", |ch| {
                    ch.style.pie_explosion = exp.round() as u8;
                });
            }
            if r2.drag_stopped() {
                self.flush_chart_pending_undo();
            }
        }

        if snap.kind == ChartKind::Doughnut {
            let mut hole = snap.style.hole_size as f32;
            let rh = ui.add(
                egui::Slider::new(&mut hole, 20.0..=80.0)
                    .text("圆环内径 %")
                    .integer(),
            );
            if rh.changed() {
                self.live_edit_chart(ci, "调整圆环内径", |ch| {
                    ch.style.hole_size = hole.round() as u8;
                });
            }
            if rh.drag_stopped() {
                self.flush_chart_pending_undo();
            }
            let mut kpi = snap.analytics.show_center_kpi;
            if ui.checkbox(&mut kpi, "显示中心总计 KPI").changed() {
                self.mutate_chart_at(ci, "切换中心 KPI", |ch| {
                    ch.analytics.show_center_kpi = kpi;
                    true
                });
            }
            if kpi {
                let mut ktitle = snap.analytics.center_kpi_title.clone();
                let rk = ui.add(
                    egui::TextEdit::singleline(&mut ktitle)
                        .desired_width(ui.available_width())
                        .hint_text("中心 KPI 标签（默认：总计）"),
                );
                if rk.changed() {
                    self.live_edit_chart(ci, "修改中心 KPI 标题", |ch| {
                        ch.analytics.center_kpi_title = ktitle;
                    });
                }
                if rk.lost_focus() {
                    self.flush_chart_pending_undo();
                }
            }
        }

        if snap.kind == ChartKind::Radar {
            ui.horizontal(|ui| {
                ui.label("雷达样式");
                egui::ComboBox::from_id_salt("studio_radar_style")
                    .selected_text(match snap.radar_style.as_str() {
                        "standard" => "标准折线",
                        "filled" => "半透明填充",
                        _ => "带数据标记",
                    })
                    .show_ui(ui, |ui| {
                        for (k, lbl) in [
                            ("standard", "标准折线"),
                            ("marker", "带数据标记"),
                            ("filled", "半透明填充"),
                        ] {
                            if ui.selectable_label(snap.radar_style == k, lbl).clicked() {
                                self.mutate_chart_at(ci, "修改雷达样式", |ch| {
                                    ch.radar_style = k.to_string();
                                    true
                                });
                            }
                        }
                    });
            });
        }
    }

    fn draw_studio_tab_data(
        &mut self,
        ui: &mut egui::Ui,
        ci: usize,
        snap: &Chart,
        trigger_switch_rc: &mut bool,
        trigger_apply_source: &mut bool,
        trigger_refresh_refs: &mut bool,
    ) {
        ui.label(egui::RichText::new("源数据区域").strong().size(12.0));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.chart_studio.source_edit_buf)
                    .desired_width(156.0)
                    .hint_text("例如 A1:D12"),
            );
            if ui.small_button("应用").clicked() {
                *trigger_apply_source = true;
            }
            if ui.small_button("刷新").clicked() {
                *trigger_refresh_refs = true;
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui_kit::secondary_button(ui, "⇄ 行列互换").clicked() {
                *trigger_switch_rc = true;
            }
            if ui_kit::secondary_button(ui, "+ 新增系列").clicked() {
                let n = snap.series.len() + 1;
                let base_cats = snap
                    .series
                    .first()
                    .map(|s| s.cats.clone())
                    .unwrap_or_default();
                let base_cat_ref = snap
                    .series
                    .first()
                    .map(|s| s.cat_ref.clone())
                    .unwrap_or_default();
                self.mutate_chart_at(ci, "新增图表系列", |ch| {
                    ch.series.push(Series {
                        name: format!("系列 {n}"),
                        cats: base_cats,
                        vals: vec![0.0; ch.series.first().map(|s| s.vals.len()).unwrap_or(3)],
                        cat_ref: base_cat_ref,
                        ..Default::default()
                    });
                    true
                });
                self.chart_studio.selected_series = snap.series.len();
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("数据系列 ({})", snap.series.len()))
                .strong()
                .size(12.0),
        );
        if snap.series.is_empty() {
            ui.label("暂无系列");
            return;
        }
        if self.chart_studio.selected_series >= snap.series.len() {
            self.chart_studio.selected_series = 0;
        }

        // 系列清单条
        for (si, s) in snap.series.iter().enumerate() {
            let sel = self.chart_studio.selected_series == si;
            let sc = rgb(series_rgb(snap, si));
            ui.horizontal(|ui| {
                let mut vis = !s.hidden;
                if ui.checkbox(&mut vis, "").on_hover_text("显示/隐藏该系列").changed() {
                    self.mutate_chart_at(ci, "切换系列显隐", |ch| {
                        if let Some(t) = ch.series.get_mut(si) {
                            t.hidden = !vis;
                            return true;
                        }
                        false
                    });
                }
                let mut rgb_arr = s.color.unwrap_or_else(|| series_rgb(snap, si));
                if ui.color_edit_button_srgb(&mut rgb_arr).changed() {
                    self.live_edit_chart(ci, "修改系列颜色", |ch| {
                        if let Some(t) = ch.series.get_mut(si) {
                            t.color = Some(rgb_arr);
                        }
                    });
                }
                let label = if s.name.is_empty() {
                    format!("系列 {}", si + 1)
                } else {
                    s.name.clone()
                };
                if ui
                    .selectable_label(sel, egui::RichText::new(label).color(if s.hidden { Color32::GRAY } else { sc }))
                    .clicked()
                {
                    self.chart_studio.selected_series = si;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if snap.series.len() > 1
                        && ui.small_button("删").on_hover_text("删除系列").clicked()
                    {
                        self.mutate_chart_at(ci, "删除图表系列", |ch| {
                            if si < ch.series.len() {
                                ch.series.remove(si);
                                return true;
                            }
                            false
                        });
                    }
                    if si + 1 < snap.series.len() && ui.small_button("↓").clicked() {
                        self.mutate_chart_at(ci, "下移系列顺序", |ch| {
                            ch.series.swap(si, si + 1);
                            true
                        });
                    }
                    if si > 0 && ui.small_button("↑").clicked() {
                        self.mutate_chart_at(ci, "上移系列顺序", |ch| {
                            ch.series.swap(si - 1, si);
                            true
                        });
                    }
                });
            });
        }

        // 选中系列的详细设置卡片
        let si = self.chart_studio.selected_series.min(snap.series.len() - 1);
        let s = &snap.series[si];
        ui.add_space(6.0);
        egui::Frame::NONE
            .fill(Color32::from_rgb(248, 250, 252))
            .stroke(Stroke::new(1.0, Color32::from_rgb(226, 232, 240)))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!("系列 #{} 精调", si + 1))
                        .strong()
                        .size(11.5),
                );
                let mut sname = s.name.clone();
                ui.horizontal(|ui| {
                    ui.label("名称");
                    let r = ui.add(egui::TextEdit::singleline(&mut sname).desired_width(180.0));
                    if r.changed() {
                        self.live_edit_chart(ci, "修改系列名称", |ch| {
                            if let Some(t) = ch.series.get_mut(si) {
                                t.name = sname;
                            }
                        });
                    }
                    if r.lost_focus() {
                        self.flush_chart_pending_undo();
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("图型");
                    let cur_txt = s.kind.map(|k| k.label()).unwrap_or("跟随图表");
                    egui::ComboBox::from_id_salt("studio_ser_kind")
                        .selected_text(cur_txt)
                        .width(104.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(s.kind.is_none(), "跟随图表").clicked() {
                                self.mutate_chart_at(ci, "修改系列图型", |ch| {
                                    if let Some(t) = ch.series.get_mut(si) {
                                        t.kind = None;
                                        return true;
                                    }
                                    false
                                });
                            }
                            for k in [
                                ChartKind::Column,
                                ChartKind::Line,
                                ChartKind::Area,
                                ChartKind::Scatter,
                            ] {
                                if ui.selectable_label(s.kind == Some(k), k.label()).clicked() {
                                    self.mutate_chart_at(ci, "修改系列图型", |ch| {
                                        if let Some(t) = ch.series.get_mut(si) {
                                            t.kind = Some(k);
                                            ch.kind = ChartKind::Combo;
                                            return true;
                                        }
                                        false
                                    });
                                }
                            }
                        });
                    let mut sec = s.secondary;
                    if ui.checkbox(&mut sec, "次纵轴").changed() {
                        self.mutate_chart_at(ci, "切换次坐标轴", |ch| {
                            if let Some(t) = ch.series.get_mut(si) {
                                t.secondary = sec;
                                return true;
                            }
                            false
                        });
                    }
                });

                ui.horizontal(|ui| {
                    let mut smooth = s.smooth;
                    if ui.checkbox(&mut smooth, "平滑曲线").changed() {
                        self.mutate_chart_at(ci, "切换平滑曲线", |ch| {
                            if let Some(t) = ch.series.get_mut(si) {
                                t.smooth = smooth;
                                if smooth {
                                    t.stepped = false;
                                }
                                return true;
                            }
                            false
                        });
                    }
                    let mut stepped = s.stepped;
                    if ui.checkbox(&mut stepped, "阶梯线").changed() {
                        self.mutate_chart_at(ci, "切换阶梯折线", |ch| {
                            if let Some(t) = ch.series.get_mut(si) {
                                t.stepped = stepped;
                                if stepped {
                                    t.smooth = false;
                                }
                                return true;
                            }
                            false
                        });
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("标记");
                    let m_lbl = |m: MarkerStyle| match m {
                        MarkerStyle::Auto => "自动",
                        MarkerStyle::None => "无",
                        MarkerStyle::Circle => "圆形",
                        MarkerStyle::Square => "方形",
                        MarkerStyle::Diamond => "菱形",
                        MarkerStyle::Triangle => "三角",
                    };
                    egui::ComboBox::from_id_salt("studio_ser_marker")
                        .selected_text(m_lbl(s.marker))
                        .width(66.0)
                        .show_ui(ui, |ui| {
                            for m in [
                                MarkerStyle::Auto,
                                MarkerStyle::None,
                                MarkerStyle::Circle,
                                MarkerStyle::Square,
                                MarkerStyle::Diamond,
                                MarkerStyle::Triangle,
                            ] {
                                if ui.selectable_label(s.marker == m, m_lbl(m)).clicked() {
                                    self.mutate_chart_at(ci, "修改数据标记", |ch| {
                                        if let Some(t) = ch.series.get_mut(si) {
                                            t.marker = m;
                                            return true;
                                        }
                                        false
                                    });
                                }
                            }
                        });
                    ui.label("线型");
                    let d_lbl = |d: LineDash| match d {
                        LineDash::Solid => "实线",
                        LineDash::Dash => "虚线",
                        LineDash::Dot => "点线",
                        LineDash::DashDot => "点划线",
                    };
                    egui::ComboBox::from_id_salt("studio_ser_dash")
                        .selected_text(d_lbl(s.dash))
                        .width(66.0)
                        .show_ui(ui, |ui| {
                            for d in [
                                LineDash::Solid,
                                LineDash::Dash,
                                LineDash::Dot,
                                LineDash::DashDot,
                            ] {
                                if ui.selectable_label(s.dash == d, d_lbl(d)).clicked() {
                                    self.mutate_chart_at(ci, "修改线型", |ch| {
                                        if let Some(t) = ch.series.get_mut(si) {
                                            t.dash = d;
                                            return true;
                                        }
                                        false
                                    });
                                }
                            }
                        });
                });

                let mut lw = if s.line_width > 0.0 { s.line_width } else { 2.2 };
                let rlw = ui.add(egui::Slider::new(&mut lw, 1.0..=5.0).text("线宽 pt"));
                if rlw.changed() {
                    self.live_edit_chart(ci, "调整系列线宽", |ch| {
                        if let Some(t) = ch.series.get_mut(si) {
                            t.line_width = lw;
                        }
                    });
                }
                if rlw.drag_stopped() {
                    self.flush_chart_pending_undo();
                }

                ui.add_space(4.0);
                let mut nref = s.name_ref.clone();
                let mut cref = s.cat_ref.clone();
                let mut vref = s.val_ref.clone();
                let mut zref = s.size_ref.clone();
                egui::Grid::new("studio_ser_refs")
                    .num_columns(2)
                    .spacing([6.0, 3.0])
                    .show(ui, |ui| {
                        ui.label("名称引用");
                        let r1 = ui.add(egui::TextEdit::singleline(&mut nref).desired_width(172.0));
                        if r1.changed() {
                            self.live_edit_chart(ci, "修改系列引用", |ch| {
                                if let Some(t) = ch.series.get_mut(si) {
                                    t.name_ref = nref;
                                }
                            });
                        }
                        ui.end_row();

                        ui.label("分类引用");
                        let r2 = ui.add(egui::TextEdit::singleline(&mut cref).desired_width(172.0));
                        if r2.changed() {
                            self.live_edit_chart(ci, "修改系列引用", |ch| {
                                if let Some(t) = ch.series.get_mut(si) {
                                    t.cat_ref = cref;
                                }
                            });
                        }
                        ui.end_row();

                        ui.label("数值引用");
                        let r3 = ui.add(egui::TextEdit::singleline(&mut vref).desired_width(172.0));
                        if r3.changed() {
                            self.live_edit_chart(ci, "修改系列引用", |ch| {
                                if let Some(t) = ch.series.get_mut(si) {
                                    t.val_ref = vref;
                                }
                            });
                        }
                        ui.end_row();

                        if snap.kind == ChartKind::Bubble {
                            ui.label("气泡大小");
                            let r4 = ui.add(egui::TextEdit::singleline(&mut zref).desired_width(172.0));
                            if r4.changed() {
                                self.live_edit_chart(ci, "修改气泡引用", |ch| {
                                    if let Some(t) = ch.series.get_mut(si) {
                                        t.size_ref = zref;
                                    }
                                });
                            }
                            ui.end_row();
                        }
                    });
                if ui.small_button("按引用立即重读数据").clicked() {
                    *trigger_refresh_refs = true;
                }
            });
    }

    fn draw_studio_tab_axes(&mut self, ui: &mut egui::Ui, ci: usize, snap: &Chart) {
        ui.horizontal(|ui| {
            for (tab, lbl) in [
                (ChartAxisTab::YPrimary, "主 Y 轴"),
                (ChartAxisTab::YSecondary, "次 Y 轴"),
                (ChartAxisTab::X, "X 分类/值轴"),
            ] {
                if ui
                    .selectable_label(self.chart_studio.axis_tab == tab, lbl)
                    .clicked()
                {
                    self.chart_studio.axis_tab = tab;
                }
            }
        });
        ui.add_space(6.0);
        let tab = self.chart_studio.axis_tab;
        let ax: AxisConfig = match tab {
            ChartAxisTab::YPrimary => snap.effective_y_axis(),
            ChartAxisTab::YSecondary => snap.y2_axis.clone(),
            ChartAxisTab::X => snap.effective_x_axis(),
        };
        let apply_axis = |ch: &mut Chart, new_ax: AxisConfig| match tab {
            ChartAxisTab::YPrimary => {
                ch.y_axis = new_ax;
                ch.sync_axis_compat();
            }
            ChartAxisTab::YSecondary => {
                ch.y2_axis = new_ax;
            }
            ChartAxisTab::X => {
                ch.x_axis = new_ax;
                ch.sync_axis_compat();
            }
        };

        let mut title = ax.title.clone();
        ui.label("坐标轴标题");
        let rt = ui.add(
            egui::TextEdit::singleline(&mut title)
                .desired_width(ui.available_width())
                .hint_text("可选轴标题"),
        );
        if rt.changed() {
            let mut next = ax.clone();
            next.title = title;
            self.live_edit_chart(ci, "修改坐标轴标题", |ch| apply_axis(ch, next));
        }
        if rt.lost_focus() {
            self.flush_chart_pending_undo();
        }

        ui.add_space(6.0);
        let mut hidden = ax.hidden;
        if ui.checkbox(&mut hidden, "隐藏该坐标轴").changed() {
            let mut next = ax.clone();
            next.hidden = hidden;
            self.mutate_chart_at(ci, "切换坐标轴显示", |ch| {
                apply_axis(ch, next);
                true
            });
        }
        let mut maj = ax.show_major_grid;
        if ui.checkbox(&mut maj, "显示主网格线").changed() {
            let mut next = ax.clone();
            next.show_major_grid = maj;
            self.mutate_chart_at(ci, "切换主网格线", |ch| {
                apply_axis(ch, next);
                true
            });
        }
        let mut min_g = ax.show_minor_grid;
        if ui.checkbox(&mut min_g, "显示次网格线").changed() {
            let mut next = ax.clone();
            next.show_minor_grid = min_g;
            self.mutate_chart_at(ci, "切换次网格线", |ch| {
                apply_axis(ch, next);
                true
            });
        }
        let mut rev = ax.reverse;
        if ui.checkbox(&mut rev, "逆序刻度（反转方向）").changed() {
            let mut next = ax.clone();
            next.reverse = rev;
            self.mutate_chart_at(ci, "切换坐标轴逆序", |ch| {
                apply_axis(ch, next);
                true
            });
        }
        let mut is_log = ax.log_base.is_some();
        if ui.checkbox(&mut is_log, "对数刻度（Base 10）").changed() {
            let mut next = ax.clone();
            next.log_base = is_log.then_some(10.0);
            self.mutate_chart_at(ci, "切换对数刻度", |ch| {
                apply_axis(ch, next);
                true
            });
        }

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("刻度范围与步长").strong().size(12.0));
        let mut auto_bounds = ax.min.is_none() && ax.max.is_none();
        if ui.checkbox(&mut auto_bounds, "自动计算最小/最大值").changed() {
            let mut next = ax.clone();
            if auto_bounds {
                next.min = None;
                next.max = None;
            } else {
                next.min = Some(0.0);
                next.max = Some(100.0);
            }
            self.mutate_chart_at(ci, "切换坐标轴范围模式", |ch| {
                apply_axis(ch, next);
                true
            });
        }
        if !auto_bounds {
            let mut lo = ax.min.unwrap_or(0.0);
            let mut hi = ax.max.unwrap_or(100.0);
            ui.horizontal(|ui| {
                ui.label("下限");
                let c1 = ui_kit::spin(ui, &mut lo, -1e12, 1e12, 1.0, 82.0);
                ui.label("上限");
                let c2 = ui_kit::spin(ui, &mut hi, -1e12, 1e12, 1.0, 82.0);
                if c1 || c2 {
                    let mut next = ax.clone();
                    next.min = Some(lo);
                    next.max = Some(hi);
                    self.live_edit_chart(ci, "修改坐标轴边界", |ch| apply_axis(ch, next));
                }
            });
        }
        let mut auto_step = ax.major_unit.is_none();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut auto_step, "自动步长").changed() {
                let mut next = ax.clone();
                next.major_unit = if auto_step { None } else { Some(20.0) };
                self.mutate_chart_at(ci, "切换坐标轴步长", |ch| {
                    apply_axis(ch, next);
                    true
                });
            }
            if !auto_step {
                let mut step = ax.major_unit.unwrap_or(20.0).max(1e-6);
                if ui_kit::spin(ui, &mut step, 1e-6, 1e12, 1.0, 82.0) {
                    let mut next = ax.clone();
                    next.major_unit = Some(step);
                    self.live_edit_chart(ci, "修改坐标轴步长", |ch| apply_axis(ch, next));
                }
            }
        });

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("数字格式").strong().size(12.0));
        ui.horizontal_wrapped(|ui| {
            for &(lbl, code) in AXIS_NUM_FMT_PRESETS {
                if ui.selectable_label(ax.num_fmt == code, lbl).clicked() {
                    let mut next = ax.clone();
                    next.num_fmt = code.to_string();
                    self.mutate_chart_at(ci, "修改坐标轴格式", |ch| {
                        apply_axis(ch, next);
                        true
                    });
                }
            }
        });
        let mut fmt = ax.num_fmt.clone();
        let rf = ui.add(
            egui::TextEdit::singleline(&mut fmt)
                .desired_width(ui.available_width())
                .hint_text("自定义数字格式，如 ¥#,##0.0"),
        );
        if rf.changed() {
            let mut next = ax.clone();
            next.num_fmt = fmt;
            self.live_edit_chart(ci, "修改坐标轴格式", |ch| apply_axis(ch, next));
        }
        if rf.lost_focus() {
            self.flush_chart_pending_undo();
        }
    }

    fn draw_studio_tab_labels(&mut self, ui: &mut egui::Ui, ci: usize, snap: &Chart) {
        ui.label(egui::RichText::new("图例设置").strong().size(12.0));
        ui.label(
            egui::RichText::new("提示：画布上单击图例项可显隐该系列，双击可单独聚焦")
                .size(10.5)
                .color(Color32::from_rgb(100, 116, 139)),
        );
        let mut show_leg = snap.legend;
        if ui.checkbox(&mut show_leg, "显示图例").changed() {
            self.mutate_chart_at(ci, "切换图例显示", |ch| {
                ch.legend = show_leg;
                if show_leg && ch.legend_pos.is_empty() {
                    ch.legend_pos = "r".into();
                }
                true
            });
        }
        if show_leg {
            ui.horizontal(|ui| {
                for (code, lbl) in [("t", "顶部"), ("b", "底部"), ("l", "左侧"), ("r", "右侧")] {
                    if ui.selectable_label(snap.legend_pos == code, lbl).clicked() {
                        self.mutate_chart_at(ci, "修改图例位置", |ch| {
                            ch.legend_pos = code.to_string();
                            ch.legend = true;
                            true
                        });
                    }
                }
            });
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("数据标签").strong().size(12.0));
        let mut show_val = snap.show_labels;
        if ui.checkbox(&mut show_val, "显示数值标签").changed() {
            self.mutate_chart_at(ci, "切换数值标签", |ch| {
                ch.show_labels = show_val;
                true
            });
        }
        let mut show_cat = snap.label_opts.show_cat;
        if ui.checkbox(&mut show_cat, "显示类别名称").changed() {
            self.mutate_chart_at(ci, "切换类别标签", |ch| {
                ch.label_opts.show_cat = show_cat;
                true
            });
        }
        let mut show_ser = snap.label_opts.show_ser;
        if ui.checkbox(&mut show_ser, "显示系列名称").changed() {
            self.mutate_chart_at(ci, "切换系列标签", |ch| {
                ch.label_opts.show_ser = show_ser;
                true
            });
        }
        let mut show_pct = snap.label_opts.show_pct;
        if ui.checkbox(&mut show_pct, "显示占比百分比（饼/环/漏斗/树图）").changed() {
            self.mutate_chart_at(ci, "切换百分比标签", |ch| {
                ch.label_opts.show_pct = show_pct;
                true
            });
        }
        let mut leader = snap.label_opts.leader_lines;
        if ui.checkbox(&mut leader, "显示防撞引导线").changed() {
            self.mutate_chart_at(ci, "切换标签引导线", |ch| {
                ch.label_opts.leader_lines = leader;
                true
            });
        }
        ui.horizontal(|ui| {
            ui.label("标签位置");
            let pos_lbl = |p: &str| match p {
                "outEnd" => "外侧顶端",
                "ctr" => "居中",
                "inEnd" => "内侧顶端",
                "inBase" => "内侧底端",
                _ => "自动",
            };
            egui::ComboBox::from_id_salt("studio_lbl_pos")
                .selected_text(pos_lbl(&snap.label_opts.position))
                .show_ui(ui, |ui| {
                    for code in ["", "outEnd", "ctr", "inEnd", "inBase"] {
                        if ui
                            .selectable_label(snap.label_opts.position == code, pos_lbl(code))
                            .clicked()
                        {
                            self.mutate_chart_at(ci, "修改标签位置", |ch| {
                                ch.label_opts.position = code.to_string();
                                true
                            });
                        }
                    }
                });
        });
    }

    fn draw_studio_tab_analytics(
        &mut self,
        ui: &mut egui::Ui,
        ci: usize,
        snap: &Chart,
        trigger_export_svg: &mut bool,
        trigger_duplicate: &mut bool,
        trigger_delete: &mut bool,
    ) {
        // 1. 智能商业洞察开关（超越 Excel）
        ui.label(egui::RichText::new("智能洞察标注").strong().size(12.0));
        let mut hi_ext = snap.analytics.highlight_extrema;
        if ui.checkbox(&mut hi_ext, "自动高亮极值点 (Max / Min)").changed() {
            self.mutate_chart_at(ci, "切换极值高亮", |ch| {
                ch.analytics.highlight_extrema = hi_ext;
                true
            });
        }
        let mut growth = snap.analytics.show_growth_badge;
        if ui.checkbox(&mut growth, "右上角首末增幅徽章 (▲/▼%)").changed() {
            self.mutate_chart_at(ci, "切换增幅徽章", |ch| {
                ch.analytics.show_growth_badge = growth;
                true
            });
        }
        let mut sum_bar = snap.analytics.show_summary_bar;
        if ui.checkbox(&mut sum_bar, "底部实时统计摘要条").changed() {
            self.mutate_chart_at(ci, "切换统计摘要条", |ch| {
                ch.analytics.show_summary_bar = sum_bar;
                true
            });
        }

        // 直方图专属设置
        if snap.kind == ChartKind::Histogram {
            let mut pareto = snap.analytics.show_pareto_line;
            if ui.checkbox(&mut pareto, "叠加帕累托累积百分比折线").changed() {
                self.mutate_chart_at(ci, "切换帕累托线", |ch| {
                    ch.analytics.show_pareto_line = pareto;
                    true
                });
            }
            ui.horizontal(|ui| {
                let mut auto_bins = snap.analytics.hist_bins.is_none();
                if ui.checkbox(&mut auto_bins, "自动分箱").changed() {
                    self.mutate_chart_at(ci, "切换直方图分箱", |ch| {
                        ch.analytics.hist_bins = if auto_bins { None } else { Some(8) };
                        true
                    });
                }
                if !auto_bins {
                    let mut bins = snap.analytics.hist_bins.unwrap_or(8) as f64;
                    if ui_kit::spin(ui, &mut bins, 2.0, 50.0, 1.0, 64.0) {
                        self.live_edit_chart(ci, "调整直方图分箱数", |ch| {
                            ch.analytics.hist_bins = Some(bins.round() as u32);
                        });
                    }
                }
            });
        }

        // 瀑布图小计柱标记
        if snap.kind == ChartKind::Waterfall
            && let Some(s0) = snap.series.first()
        {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("瀑布图小计/总计柱（点击切换）").size(11.0));
            ui.horizontal_wrapped(|ui| {
                for i in 0..s0.vals.len().min(20) {
                    let lbl = s0
                        .cats
                        .get(i)
                        .filter(|c| !c.is_empty())
                        .cloned()
                        .unwrap_or_else(|| format!("#{}", i + 1));
                    let is_tot = snap.analytics.waterfall_totals.contains(&i);
                    if ui.selectable_label(is_tot, lbl).clicked() {
                        self.mutate_chart_at(ci, "切换瀑布图小计柱", |ch| {
                            if let Some(pos) =
                                ch.analytics.waterfall_totals.iter().position(|&x| x == i)
                            {
                                ch.analytics.waterfall_totals.remove(pos);
                            } else {
                                ch.analytics.waterfall_totals.push(i);
                            }
                            true
                        });
                    }
                }
            });
        }

        // 2. 回归趋势线（6 种数学拟合 + 预测 + 公式 + R²）
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("趋势线与数学回归").strong().size(12.0));
        if !snap.series.is_empty() {
            let si = self.chart_studio.selected_series.min(snap.series.len() - 1);
            ui.horizontal(|ui| {
                ui.label("目标系列");
                egui::ComboBox::from_id_salt("studio_tl_ser")
                    .selected_text(if snap.series[si].name.is_empty() {
                        format!("系列 {}", si + 1)
                    } else {
                        snap.series[si].name.clone()
                    })
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for (i, s) in snap.series.iter().enumerate() {
                            let lbl = if s.name.is_empty() {
                                format!("系列 {}", i + 1)
                            } else {
                                s.name.clone()
                            };
                            if ui.selectable_label(si == i, lbl).clicked() {
                                self.chart_studio.selected_series = i;
                            }
                        }
                    });
            });
            let tl_kinds = ["线性 OLS", "多项式回归", "指数拟合", "对数拟合", "幂函数", "移动平均"];
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("studio_tl_kind")
                    .selected_text(tl_kinds[self.chart_studio.new_tl_kind.min(5)])
                    .width(104.0)
                    .show_ui(ui, |ui| {
                        for (i, &lbl) in tl_kinds.iter().enumerate() {
                            if ui
                                .selectable_label(self.chart_studio.new_tl_kind == i, lbl)
                                .clicked()
                            {
                                self.chart_studio.new_tl_kind = i;
                            }
                        }
                    });
                if self.chart_studio.new_tl_kind == 1 {
                    let mut ord = self.chart_studio.new_tl_poly_order as f64;
                    if ui_kit::spin(ui, &mut ord, 2.0, 4.0, 1.0, 44.0) {
                        self.chart_studio.new_tl_poly_order = ord.round() as u8;
                    }
                    ui.label("阶");
                } else if self.chart_studio.new_tl_kind == 5 {
                    let mut per = self.chart_studio.new_tl_ma_period as f64;
                    if ui_kit::spin(ui, &mut per, 2.0, 12.0, 1.0, 44.0) {
                        self.chart_studio.new_tl_ma_period = per.round() as u8;
                    }
                    ui.label("期");
                }
                if ui.small_button("+ 添加").clicked() {
                    let kind = match self.chart_studio.new_tl_kind {
                        1 => TrendlineKind::Polynomial(self.chart_studio.new_tl_poly_order),
                        2 => TrendlineKind::Exponential,
                        3 => TrendlineKind::Logarithmic,
                        4 => TrendlineKind::Power,
                        5 => TrendlineKind::MovingAverage(self.chart_studio.new_tl_ma_period),
                        _ => TrendlineKind::Linear,
                    };
                    let fwd = self
                        .chart_studio
                        .new_tl_forward
                        .trim()
                        .parse::<f64>()
                        .unwrap_or(0.0)
                        .clamp(0.0, 100.0);
                    let bwd = self
                        .chart_studio
                        .new_tl_backward
                        .trim()
                        .parse::<f64>()
                        .unwrap_or(0.0)
                        .clamp(0.0, 100.0);
                    self.mutate_chart_at(ci, "添加趋势线", |ch| {
                        if let Some(s) = ch.series.get_mut(si) {
                            s.trendlines.push(Trendline {
                                kind,
                                forward: fwd,
                                backward: bwd,
                                show_equation: true,
                                show_r2: true,
                                ..Default::default()
                            });
                            return true;
                        }
                        false
                    });
                }
            });
            ui.horizontal(|ui| {
                ui.label("前推预测期");
                ui.add(
                    egui::TextEdit::singleline(&mut self.chart_studio.new_tl_forward)
                        .desired_width(40.0),
                );
                ui.label("后推");
                ui.add(
                    egui::TextEdit::singleline(&mut self.chart_studio.new_tl_backward)
                        .desired_width(40.0),
                );
            });

            // 已添加趋势线列表
            for (ti, tl) in snap.series[si].trendlines.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tl.kind.label()).size(11.0));
                    let mut eq = tl.show_equation;
                    if ui.checkbox(&mut eq, "公式").changed() {
                        self.mutate_chart_at(ci, "切换趋势线公式", |ch| {
                            if let Some(t) = ch.series.get_mut(si).and_then(|s| s.trendlines.get_mut(ti))
                            {
                                t.show_equation = eq;
                                return true;
                            }
                            false
                        });
                    }
                    let mut r2 = tl.show_r2;
                    if ui.checkbox(&mut r2, "R²").changed() {
                        self.mutate_chart_at(ci, "切换 R² 显示", |ch| {
                            if let Some(t) = ch.series.get_mut(si).and_then(|s| s.trendlines.get_mut(ti))
                            {
                                t.show_r2 = r2;
                                return true;
                            }
                            false
                        });
                    }
                    if ui.small_button("删").clicked() {
                        self.mutate_chart_at(ci, "删除趋势线", |ch| {
                            if let Some(s) = ch.series.get_mut(si)
                                && ti < s.trendlines.len()
                            {
                                s.trendlines.remove(ti);
                                return true;
                            }
                            false
                        });
                    }
                });
            }

            // 3. 误差线
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(egui::RichText::new("误差线（当前系列）").strong().size(12.0));
            let has_eb = snap.series[si].error_bars.is_some();
            let mut eb_on = has_eb;
            if ui.checkbox(&mut eb_on, "启用误差线").changed() {
                self.mutate_chart_at(ci, "切换误差线", |ch| {
                    if let Some(s) = ch.series.get_mut(si) {
                        s.error_bars = if eb_on {
                            Some(ErrorBar {
                                kind: ErrorBarKind::Percent(10.0),
                                dir: ErrorBarDir::Both,
                                color: None,
                            })
                        } else {
                            None
                        };
                        return true;
                    }
                    false
                });
            }
            if let Some(eb) = &snap.series[si].error_bars {
                ui.horizontal(|ui| {
                    let cur = match eb.kind {
                        ErrorBarKind::Fixed(_) => "固定值",
                        ErrorBarKind::Percent(_) => "百分比",
                        ErrorBarKind::StdDev(_) => "标准差",
                        ErrorBarKind::StdErr => "标准误",
                    };
                    egui::ComboBox::from_id_salt("studio_eb_kind")
                        .selected_text(cur)
                        .width(74.0)
                        .show_ui(ui, |ui| {
                            for (lbl, k) in [
                                ("百分比 10%", ErrorBarKind::Percent(10.0)),
                                ("固定值 5", ErrorBarKind::Fixed(5.0)),
                                ("1×标准差", ErrorBarKind::StdDev(1.0)),
                                ("标准误 SE", ErrorBarKind::StdErr),
                            ] {
                                if ui.selectable_label(cur == lbl, lbl).clicked() {
                                    self.mutate_chart_at(ci, "修改误差线类型", |ch| {
                                        if let Some(e) =
                                            ch.series.get_mut(si).and_then(|s| s.error_bars.as_mut())
                                        {
                                            e.kind = k;
                                            return true;
                                        }
                                        false
                                    });
                                }
                            }
                        });
                    for (d, dlbl) in [
                        (ErrorBarDir::Both, "双向"),
                        (ErrorBarDir::Plus, "正向"),
                        (ErrorBarDir::Minus, "负向"),
                    ] {
                        if ui.selectable_label(eb.dir == d, dlbl).clicked() {
                            self.mutate_chart_at(ci, "修改误差线方向", |ch| {
                                if let Some(e) =
                                    ch.series.get_mut(si).and_then(|s| s.error_bars.as_mut())
                                {
                                    e.dir = d;
                                    return true;
                                }
                                false
                            });
                        }
                    }
                });
            }
        }

        // 4. 财务参考线与目标警示带
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("参考线与目标警示带").strong().size(12.0));
        ui.horizontal_wrapped(|ui| {
            for (lbl, mode, col) in [
                ("+ 均值线", RefLineMode::Mean, [220, 76, 62]),
                ("+ 中位数", RefLineMode::Median, [14, 165, 233]),
                ("+ ±1σ 波动带", RefLineMode::StdDevBand, [59, 130, 246]),
                ("+ 最大值线", RefLineMode::Max, [16, 185, 129]),
            ] {
                if ui.small_button(lbl).clicked() {
                    let clean_lbl = lbl.trim_start_matches("+ ").to_string();
                    self.mutate_chart_at(ci, "添加参考线", |ch| {
                        ch.ref_lines.push(ReferenceLine {
                            label: clean_lbl,
                            axis: RefLineAxis::YPrimary,
                            mode,
                            color: col,
                            dash: LineDash::Dash,
                        });
                        true
                    });
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.chart_studio.new_ref_label)
                    .desired_width(68.0)
                    .hint_text("名称"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.chart_studio.new_ref_val)
                    .desired_width(58.0)
                    .hint_text("目标值"),
            );
            ui.color_edit_button_srgb(&mut self.chart_studio.new_ref_color);
            if ui.small_button("+ 常量线").clicked()
                && let Ok(v) = self.chart_studio.new_ref_val.trim().parse::<f64>()
                && v.is_finite()
            {
                let lbl = self.chart_studio.new_ref_label.clone();
                let col = self.chart_studio.new_ref_color;
                self.mutate_chart_at(ci, "添加目标线", |ch| {
                    ch.ref_lines.push(ReferenceLine {
                        label: lbl,
                        axis: RefLineAxis::YPrimary,
                        mode: RefLineMode::Fixed(v),
                        color: col,
                        dash: LineDash::Dash,
                    });
                    true
                });
            }
        });
        ui.horizontal(|ui| {
            ui.label("区间");
            ui.add(
                egui::TextEdit::singleline(&mut self.chart_studio.new_ref_low).desired_width(48.0),
            );
            ui.label("~");
            ui.add(
                egui::TextEdit::singleline(&mut self.chart_studio.new_ref_high).desired_width(48.0),
            );
            if ui.small_button("+ 警示带").clicked()
                && let (Ok(lo), Ok(hi)) = (
                    self.chart_studio.new_ref_low.trim().parse::<f64>(),
                    self.chart_studio.new_ref_high.trim().parse::<f64>(),
                )
            {
                let lbl = if self.chart_studio.new_ref_label.is_empty() {
                    "目标带".into()
                } else {
                    self.chart_studio.new_ref_label.clone()
                };
                let col = self.chart_studio.new_ref_color;
                self.mutate_chart_at(ci, "添加目标警示带", |ch| {
                    ch.ref_lines.push(ReferenceLine {
                        label: lbl,
                        axis: RefLineAxis::YPrimary,
                        mode: RefLineMode::Band {
                            low: lo.min(hi),
                            high: lo.max(hi),
                        },
                        color: col,
                        dash: LineDash::Solid,
                    });
                    true
                });
            }
        });
        for (ri, rl) in snap.ref_lines.iter().enumerate() {
            ui.horizontal(|ui| {
                let (dot_r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter().circle_filled(dot_r.center(), 4.0, rgb(rl.color));
                ui.label(
                    egui::RichText::new(format!("{} ({})", rl.label, rl.mode.label())).size(11.0),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("删").clicked() {
                        self.mutate_chart_at(ci, "删除参考线", |ch| {
                            if ri < ch.ref_lines.len() {
                                ch.ref_lines.remove(ri);
                                return true;
                            }
                            false
                        });
                    }
                });
            });
        }

        // 5. 矢量导出与管理
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("导出与管理").strong().size(12.0));
        ui.horizontal_wrapped(|ui| {
            if ui_kit::primary_button(ui, "导出高清 SVG…").clicked() {
                *trigger_export_svg = true;
            }
            if ui_kit::secondary_button(ui, "复制图表").clicked() {
                *trigger_duplicate = true;
            }
            if ui_kit::secondary_button(ui, "删除图表").clicked() {
                *trigger_delete = true;
            }
        });
    }

    // =========================================================================
    // 3. 双击标题快速重命名弹窗
    // =========================================================================
    pub(crate) fn draw_chart_inline_title_modal(&mut self, ctx: &egui::Context) {
        let Some((ci, mut title, mut subtitle)) = self.chart_studio.inline_title_modal.take() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        ui_kit::window(ctx, "编辑图表标题", 360.0)
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("chart_inline_title_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("主标题");
                        let r1 = ui.add(
                            egui::TextEdit::singleline(&mut title).desired_width(240.0),
                        );
                        if r1.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            save = true;
                        }
                        ui.end_row();
                        ui.label("副标题");
                        let r2 = ui.add(
                            egui::TextEdit::singleline(&mut subtitle).desired_width(240.0),
                        );
                        if r2.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            save = true;
                        }
                        ui.end_row();
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_kit::secondary_button(ui, "取消").clicked() {
                            cancel = true;
                        }
                        if ui_kit::primary_button(ui, "确定").clicked() {
                            save = true;
                        }
                    });
                });
            });
        if save {
            self.mutate_chart_at(ci, "修改图表标题", |ch| {
                ch.title = title;
                ch.subtitle = subtitle;
                true
            });
        } else if !cancel && open {
            self.chart_studio.inline_title_modal = Some((ci, title, subtitle));
        }
    }

    // =========================================================================
    // 4. 升级版全内存可撤销图表管理器（支持 22 种图型与 SVG 导出）
    // =========================================================================
    pub(crate) fn draw_modern_chart_manager(&mut self, ctx: &egui::Context) {
        if !self.chart_mgr {
            return;
        }
        let mut open = true;
        let mut del: Option<usize> = None;
        let mut dup: Option<usize> = None;
        let mut svg: Option<usize> = None;
        let mut open_studio: Option<usize> = None;
        let mut retype: Option<(usize, ChartKind)> = None;
        let charts = self.wb.sheet(self.sheet_idx).charts.clone();
        ui_kit::window(ctx, "图表管理", 560.0)
            .collapsible(false)
            .default_width(560.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("当前工作表共有 {} 张图表", charts.len()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_kit::primary_button(ui, "+ 新建智能图表").clicked() {
                            self.open_chart_create_wizard(None);
                        }
                    });
                });
                ui.separator();
                if charts.is_empty() {
                    ui.add_space(8.0);
                    ui.label("这张表还没有图表，点击右上角「+ 新建智能图表」即可基于当前选区快速生成。");
                }
                for (i, ch) in charts.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let title = if ch.title.is_empty() {
                            ch.kind.label()
                        } else {
                            ch.title.as_str()
                        };
                        ui.label(
                            egui::RichText::new(format!("{}. {title}", i + 1)).strong(),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{}系列 · {}-{}",
                                ch.series.len(),
                                cell_name(ch.anchor.0, ch.anchor.1),
                                cell_name(ch.anchor.2, ch.anchor.3)
                            ))
                            .size(11.0)
                            .color(Color32::from_rgb(100, 116, 139)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("删除").clicked() {
                                del = Some(i);
                            }
                            if !matches!(ch.kind, ChartKind::Unknown) {
                                if ui.small_button("复制").clicked() {
                                    dup = Some(i);
                                }
                                if ui.small_button("SVG").clicked() {
                                    svg = Some(i);
                                }
                                if ui.small_button("设计器").clicked() {
                                    open_studio = Some(i);
                                }
                                egui::ComboBox::from_id_salt(format!("mgr_ret_{i}"))
                                    .selected_text(ch.kind.label())
                                    .width(104.0)
                                    .show_ui(ui, |ui| {
                                        for &k in ChartKind::all_creatable() {
                                            if ui.selectable_label(ch.kind == k, k.label()).clicked()
                                            {
                                                retype = Some((i, k));
                                            }
                                        }
                                    });
                            } else {
                                ui.label("保留原图");
                            }
                        });
                    });
                }
            });
        if let Some((i, k)) = retype {
            self.mutate_chart_at(i, "切换图表类型", |ch| {
                ch.kind = k;
                true
            });
            self.status = format!("已切换为{}", k.label());
        }
        if let Some(i) = open_studio {
            self.chart_sel = Some(i);
            self.open_chart_studio_for_selected();
        }
        if let Some(i) = svg {
            self.export_chart_svg_dialog(i);
        }
        if let Some(i) = dup {
            self.duplicate_chart_at(i);
        }
        if let Some(i) = del {
            self.delete_chart_at(i);
        }
        if !open {
            self.chart_mgr = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;
    use sheetcore::{Cell, Value};

    fn seed_sales_sheet(app: &mut SheetApp) {
        let sh = app.wb.sheet_mut(0);
        let headers = ["月份", "营收", "利润率"];
        for (c, h) in headers.iter().enumerate() {
            sh.set(
                0,
                c as u32,
                Cell {
                    value: Value::Text((*h).into()),
                    formula: None,
                },
            );
        }
        let rows = [
            ("1月", 12000.0, 0.12),
            ("2月", 15400.0, 0.15),
            ("3月", 18200.0, 0.18),
            ("4月", 21000.0, 0.21),
        ];
        for (i, (m, rev, rate)) in rows.iter().enumerate() {
            let r = (i + 1) as u32;
            sh.set(r, 0, Cell { value: Value::Text((*m).into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Number(*rev), formula: None });
            sh.set(r, 2, Cell { value: Value::Number(*rate), formula: None });
        }
    }

    #[test]
    fn chart_wizard_works_on_unsaved_workbook_and_supports_full_undo_redo() {
        let _env = TestEnv::new("chart-studio-wizard-undo");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        seed_sales_sheet(&mut app);
        assert!(app.wb.path.is_none(), "must work on unsaved blank workbook");

        // 仅选中单格，应自动扩选 A1:C5 并分析出双量级组合图推荐
        app.sel = (1, 1);
        app.sel_end = (1, 1);
        app.open_chart_create_wizard(None);
        let wiz = app.chart_studio.wizard.take().expect("wizard opened");
        assert_eq!(wiz.source_rect, (0, 0, 4, 2));
        assert!(wiz.profile.scale_ratio > 100.0);
        assert!(
            wiz.recs.iter().any(|r| r.kind == ChartKind::Combo),
            "should recommend dual-axis Combo when scale ratio is huge"
        );

        let idx = app.insert_chart_in_memory(wiz.preview_chart);
        assert_eq!(idx, 0);
        assert_eq!(app.wb.sheet(0).charts.len(), 1);
        assert_eq!(app.chart_sel, Some(0));
        assert!(app.chart_studio.sidebar_open);

        // 切换主题 + 行列互换 + 趋势线 + 删除 → 全程一步一撤销
        let orig_theme = app.wb.sheet(0).charts[0].style.theme;
        app.cycle_selected_chart_theme();
        assert_ne!(app.wb.sheet(0).charts[0].style.theme, orig_theme);

        app.switch_selected_chart_row_col();
        assert!(app.wb.sheet(0).charts[0].style.switch_row_col);

        app.toggle_selected_chart_trendline();
        assert_eq!(app.wb.sheet(0).charts[0].series[0].trendlines.len(), 1);

        app.delete_selected_chart();
        assert!(app.wb.sheet(0).charts.is_empty());

        // 撤销删除 → 恢复图表
        assert!(app.apply_undo(false));
        assert_eq!(app.wb.sheet(0).charts.len(), 1);
        // 撤销趋势线
        assert!(app.apply_undo(false));
        assert!(app.wb.sheet(0).charts[0].series[0].trendlines.is_empty());
        // 撤销行列互换
        assert!(app.apply_undo(false));
        assert!(!app.wb.sheet(0).charts[0].style.switch_row_col);
        // 重做行列互换
        assert!(app.apply_undo(true));
        assert!(app.wb.sheet(0).charts[0].style.switch_row_col);
    }

    #[test]
    fn chart_live_refresh_and_legend_isolation_and_fingerprint_invalidation() {
        let _env = TestEnv::new("chart-studio-live-refresh");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        seed_sales_sheet(&mut app);

        app.sel = (0, 0);
        app.sel_end = (4, 2);
        app.open_chart_create_wizard(Some(ChartKind::Column));
        let wiz = app.chart_studio.wizard.take().unwrap();
        app.insert_chart_in_memory(wiz.preview_chart);
        let fp1 = crate::grid_render_cache::charts_fingerprint(&app.wb.sheet(0).charts);

        // 修改源单元格并触发同步刷新
        app.wb.sheet_mut(0).set(
            1,
            1,
            Cell {
                value: Value::Number(99999.0),
                formula: None,
            },
        );
        app.doc_gen = app.doc_gen.wrapping_add(1);
        app.sync_live_charts();
        assert_eq!(app.wb.sheet(0).charts[0].series[0].vals[0], 99999.0);
        let fp2 = crate::grid_render_cache::charts_fingerprint(&app.wb.sheet(0).charts);
        assert_ne!(fp1, fp2, "value edit must invalidate chart cache fingerprint");

        // 图例双击隔离 + 再双击恢复
        app.apply_chart_interaction(
            0,
            chart_view::ChartInteractionOut {
                isolated_legend_series: Some(1),
                ..Default::default()
            },
        );
        assert!(app.wb.sheet(0).charts[0].series[0].hidden);
        assert!(!app.wb.sheet(0).charts[0].series[1].hidden);
        let fp3 = crate::grid_render_cache::charts_fingerprint(&app.wb.sheet(0).charts);
        assert_ne!(fp2, fp3, "hidden series must invalidate chart cache fingerprint");

        app.apply_chart_interaction(
            0,
            chart_view::ChartInteractionOut {
                isolated_legend_series: Some(1),
                ..Default::default()
            },
        );
        assert!(!app.wb.sheet(0).charts[0].series[0].hidden);
        assert!(!app.wb.sheet(0).charts[0].series[1].hidden);
    }

    #[test]
    fn chart_wizard_and_all_five_studio_tabs_and_manager_render_cleanly() {
        let _env = TestEnv::new("chart-studio-ui-render");
        let ctx = egui::Context::default();
        crate::install_fonts(&ctx);
        let mut app = SheetApp::new_with_context(&ctx, None);
        seed_sales_sheet(&mut app);

        let run_raw = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            ..Default::default()
        };

        // 1. 渲染新建智能图表向导
        app.sel = (0, 0);
        app.sel_end = (4, 2);
        app.open_chart_create_wizard(None);
        let _ = ctx.run(run_raw(), |ctx| {
            app.draw_chart_create_wizard(ctx);
        });
        assert!(app.chart_studio.wizard.is_some());

        // 2. 插入图表并渲染网格（含源区域高亮+交互叠层）与五大设计器页签 + 图表管理器
        let wiz = app.chart_studio.wizard.take().unwrap();
        app.insert_chart_in_memory(wiz.preview_chart);
        app.chart_mgr = true;
        for tab in [
            ChartStudioTab::Style,
            ChartStudioTab::Data,
            ChartStudioTab::Axes,
            ChartStudioTab::Labels,
            ChartStudioTab::Analytics,
        ] {
            app.chart_studio.active_tab = tab;
            let _ = ctx.run(run_raw(), |ctx| {
                app.draw_chart_studio_sidebar(ctx);
                app.draw_modern_chart_manager(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    app.draw_grid_pane(ui, ctx, false);
                });
            });
        }
        assert_eq!(app.wb.sheet(0).charts.len(), 1);
    }
}
