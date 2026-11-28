//! 现代化数据透视表 UI 引擎：
//! - 创建向导（智能推荐预设 + 自动框选 + 默认新建工作表 + 跨表支持）
//! - 右侧可停靠「数据透视表字段」侧边栏（字段搜索、类型徽标、悬停统计画像、真实鼠标拖拽四宫格、实时刷新）
//! - 格内数据条（Sparkbar）、异常高亮、展开/折叠 `[+]/[-]`、格内 `▾` 筛选排序弹窗、源数据变动浮标提示
//! - 双击任意数据/小计/总计格下钻明细生成新表
//! - 现代卡片式多字段多列按钮切片器（即点即刷）
//! - 全流程可撤销/重做（不清空历史撤销栈）

use crate::{cell_name, theme, ui_kit, SheetApp};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use sheetcore::model::{Cell, Sheet, Value};
use sheetcore::pivot::{Agg, GroupBy};
use sheetcore::pivot_engine::{
    compute_table, detect_expanded_source, distinct_field_values, drill_down_rows,
    next_pivot_id, profile_source_fields, suggest_presets, write_table_styled, AxisFieldExt,
    ComputedPivot, FieldKind, FieldProfile, FieldSortMode, PivotFilterTarget, PivotPreset,
    PivotRowKind, PivotSlicerSpec, PivotStyleTheme, PivotTableModel, ReportLayout, SubtotalPos,
    TopNFilter, TopNKind, ValueFieldExt,
};
use sheetcore::pivot_multi::{AxisField, FilterField, PivotLayout, PivotSpec2, ShowAs, ValueField};
use sheetcore::pivot_parse::parse_a1_range;
use sheetcore::undo::{CellDelta, PivotUndoEdit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PivotZone {
    Filter,
    Col,
    Row,
    Value,
}

#[derive(Clone, Debug)]
pub(crate) enum PivotDragSource {
    FieldList(u32),
    Zone(PivotZone, usize),
}

#[derive(Clone, Debug)]
pub(crate) struct PivotDragPayload {
    pub source: PivotDragSource,
    pub col: u32,
    pub label: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PivotCreateWizard {
    pub source_sheet_idx: usize,
    pub source_sheet_name: String,
    pub source_rect: (u32, u32, u32, u32),
    pub source_text: String,
    pub dest_new_sheet: bool,
    pub dest_cell_text: String,
    pub profiles: Vec<FieldProfile>,
    pub presets: Vec<PivotPreset>,
    /// `None` = 空白透视表（手动拖字段）；`Some(i)` = 套用第 i 个智能推荐预设
    pub selected_preset: Option<usize>,
    pub auto_expand: bool,
    pub auto_refresh: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PivotFilterPopupState {
    pub pivot_id: u64,
    pub target: PivotFilterTarget,
    pub anchor_pos: Pos2,
    pub search: String,
    pub items: Vec<String>,
    pub top_n_input: String,
}

#[derive(Clone, Debug)]
pub(crate) enum FieldSettingsPopup {
    Value {
        pivot_id: u64,
        val_idx: usize,
        name_draft: String,
        fmt_draft: String,
    },
    Axis {
        pivot_id: u64,
        is_row: bool,
        field_idx: usize,
        bucket_step: String,
        top_n_count: String,
        search: String,
        distinct_items: Vec<String>,
    },
    Filter {
        pivot_id: u64,
        filter_idx: usize,
        search: String,
        distinct_items: Vec<String>,
    },
}

pub(crate) struct BgModelPivotDone {
    pub doc_uid: u64,
    pub pivot_id: u64,
    pub source_rev: u64,
    pub source_rect: (u32, u32, u32, u32),
    pub computed: ComputedPivot,
    pub label: String,
    pub record_undo: bool,
    pub pivots_before: Vec<PivotTableModel>,
}

#[derive(Default)]
pub(crate) struct PivotUiState {
    pub sidebar_open: bool,
    pub sidebar_pinned: bool,
    pub sidebar_dismissed_id: Option<u64>,
    pub active_pivot_id: Option<u64>,
    pub create_wizard: Option<PivotCreateWizard>,
    pub field_search: String,
    pub drag: Option<PivotDragPayload>,
    pub field_settings: Option<FieldSettingsPopup>,
    pub grid_filter: Option<PivotFilterPopupState>,
    pub options_pivot_id: Option<u64>,
    pub new_calc_field_name: String,
    pub new_calc_field_formula: String,
    pub new_calc_item_name: String,
    pub new_calc_item_expr: String,
    pub source_range_edit: String,
    pub slicer_picker_pivot_id: Option<u64>,
    pub slicer_picker_checked: Vec<u32>,
    pub profiles_cache: Option<(u64, u64, (u32, u32, u32, u32), Vec<FieldProfile>)>,
    pub bg_rx: Option<std::sync::mpsc::Receiver<BgModelPivotDone>>,
    pub last_auto_check_rev: std::collections::HashMap<u64, u64>,
    /// 修改透视表规格前捕获的 `wb.pivots` 快照，保证 `Ctrl+Z` 精确还原所有选项/字段改动
    pub pending_undo_pivots_before: Option<Vec<PivotTableModel>>,
}

const NUMBER_FORMAT_PRESETS: &[(&str, &str)] = &[
    ("自动（推荐）", ""),
    ("整数千分位 (1,234)", "#,##0"),
    ("两位小数 (1,234.56)", "#,##0.00"),
    ("人民币 (¥1,234.56)", "¥#,##0.00"),
    ("百分比 (12.3%)", "0.0%"),
    ("两位百分比 (12.34%)", "0.00%"),
    ("万元显示", "0.00\"万\""),
];

impl SheetApp {
    /// 在修改 `self.wb.pivots` 之前记录快照，确保随后的 `recompute_pivot_by_id` 撤销能准确恢复旧配置。
    pub(crate) fn capture_pivot_undo_before(&mut self) {
        if self.pivot_ui.pending_undo_pivots_before.is_none() {
            self.pivot_ui.pending_undo_pivots_before = Some(self.wb.pivots.clone());
        }
    }

    /// 返回当前选中格所在的透视表在 `wb.pivots` 中的下标。
    pub(crate) fn pivot_at_sel(&self) -> Option<usize> {
        self.wb
            .pivots
            .iter()
            .position(|p| p.contains_cell(self.sheet_idx, self.sel.0, self.sel.1))
    }

    pub(crate) fn active_pivot_index(&self) -> Option<usize> {
        if let Some(i) = self.pivot_at_sel() {
            return Some(i);
        }
        if let Some(id) = self.pivot_ui.active_pivot_id
            && let Some(i) = self
                .wb
                .pivots
                .iter()
                .position(|p| p.id == id && p.target_sheet == self.sheet_idx)
        {
            return Some(i);
        }
        None
    }

    pub(crate) fn active_pivot(&self) -> Option<&PivotTableModel> {
        self.active_pivot_index().and_then(|i| self.wb.pivots.get(i))
    }

    fn source_sheet_index_of(&self, model: &PivotTableModel) -> Option<usize> {
        if let Some(sid) = model.source_sheet_id
            && let Some(i) = self.wb.sheets.iter().position(|s| s.identity == sid)
        {
            return Some(i);
        }
        self.wb
            .sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(&model.source_sheet))
    }

    fn profiles_for_pivot(&mut self, model: &PivotTableModel) -> Vec<FieldProfile> {
        let Some(si) = self.source_sheet_index_of(model) else {
            return Vec::new();
        };
        let rev = self.wb.sheets[si].cell_revision();
        let src = model.spec.source;
        if let Some((uid, cached_rev, cached_src, profs)) = &self.pivot_ui.profiles_cache
            && *uid == self.doc_uid
            && *cached_rev == rev
            && *cached_src == src
        {
            return profs.clone();
        }
        let profs = profile_source_fields(&self.wb.sheets[si], &self.wb.styles, src);
        self.pivot_ui.profiles_cache = Some((self.doc_uid, rev, src, profs.clone()));
        profs
    }

    fn field_header_name(&self, model: &PivotTableModel, col: u32) -> String {
        if let Some(si) = self.source_sheet_index_of(model) {
            let sh = &self.wb.sheets[si];
            if let Some(c) = sh.get(model.spec.source.0, col) {
                let s = c.value.display();
                if !s.is_empty() {
                    return s;
                }
            }
        }
        sheetcore::model::col_label(col)
    }

    /// 打开现代化「创建数据透视表」向导（含智能推荐布局、自动框选与默认新建工作表）。
    pub(crate) fn open_pivot_create_wizard(&mut self, explicit_rect: Option<(u32, u32, u32, u32)>) {
        if self.edit_blocked() {
            return;
        }
        let rect = explicit_rect.unwrap_or_else(|| {
            let sr = self.sel_rect();
            if sr.2 > sr.0 && sr.3 > sr.1 {
                sr
            } else {
                sheetcore::sort_filter::current_region(
                    self.wb.sheet(self.sheet_idx),
                    self.sel.0,
                    self.sel.1,
                )
            }
        });
        if rect.2 <= rect.0 || rect.3 < rect.1 {
            self.status = "当前区域太小，请先选中包含表头和数据的表格区域".into();
            return;
        }
        let sh = self.wb.sheet(self.sheet_idx);
        let sheet_name = sh.name.clone();
        let profiles = profile_source_fields(sh, &self.wb.styles, rect);
        let presets = suggest_presets(rect, &profiles);
        let source_text = format!(
            "{}:{}",
            cell_name(rect.0, rect.1),
            cell_name(rect.2, rect.3)
        );
        // 现有工作表默认放到源表右侧空两列处，避免误覆写源表
        let safe_col = rect.3.saturating_add(2);
        let dest_cell_text = cell_name(rect.0, safe_col);
        let default_preset = (!presets.is_empty()).then_some(0);
        self.pivot_ui.create_wizard = Some(PivotCreateWizard {
            source_sheet_idx: self.sheet_idx,
            source_sheet_name: sheet_name,
            source_rect: rect,
            source_text,
            dest_new_sheet: true,
            dest_cell_text,
            profiles,
            presets,
            selected_preset: default_preset,
            auto_expand: true,
            auto_refresh: false,
        });
    }

    /// 生成不重名的新透视表名与工作表名
    fn next_unique_pivot_name(&self, prefix: &str) -> String {
        let mut n = self.wb.pivots.len() + 1;
        loop {
            let cand = format!("{prefix}{n}");
            let exists_p = self.wb.pivots.iter().any(|p| p.name == cand);
            let exists_s = self
                .wb
                .sheets
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(&cand));
            if !exists_p && !exists_s {
                return cand;
            }
            n += 1;
        }
    }

    /// 根据向导配置创建一等公民透视表并接入可撤销栈。
    pub(crate) fn create_pivot_from_wizard(&mut self, wiz: PivotCreateWizard) {
        let src_rect = parse_a1_range(&wiz.source_text).unwrap_or(wiz.source_rect);
        if src_rect.2 <= src_rect.0 || src_rect.3 < src_rect.1 {
            self.status = "透视表源区域无效，至少需要 1 行表头和 1 行数据".into();
            return;
        }
        let src_idx = wiz.source_sheet_idx.min(self.wb.sheets.len() - 1);
        let src_sheet_name = self.wb.sheets[src_idx].name.clone();
        let src_sheet_id = self.wb.sheets[src_idx].identity;
        let src_rev = self.wb.sheets[src_idx].cell_revision();

        let (mut spec, mut opts) = if let Some(pi) = wiz.selected_preset
            && let Some(preset) = wiz.presets.get(pi)
        {
            let mut sp = preset.spec.clone();
            sp.source = src_rect;
            (sp, preset.options.clone())
        } else {
            (
                PivotSpec2 {
                    source: src_rect,
                    rows: Vec::new(),
                    cols: Vec::new(),
                    values: Vec::new(),
                    filters: Vec::new(),
                    layout: PivotLayout::default(),
                },
                Default::default(),
            )
        };
        opts.auto_expand_source = wiz.auto_expand;
        opts.auto_refresh = wiz.auto_refresh;
        spec.source = src_rect;

        let pivots_before = self.wb.pivots.clone();
        let removed_before = self.wb.pivots_removed_parts.clone();
        let focus_before = (self.sheet_idx, self.sel.0, self.sel.1);
        let pivot_name = self.next_unique_pivot_name("数据透视表");

        let (target_sheet_idx, anchor, inserted_sheet_idx) = if wiz.dest_new_sheet {
            let new_idx = src_idx + 1;
            let mut new_sh = Sheet::new(&pivot_name);
            new_sh.edited = true;
            sheetcore::sheet_ops::insert_sheet(&mut self.wb, new_idx, new_sh);
            (new_idx, (1u32, 0u32), Some(new_idx))
        } else {
            let anc = parse_a1_range(&wiz.dest_cell_text)
                .map(|r| (r.0, r.1))
                .unwrap_or((src_rect.0, src_rect.3.saturating_add(2)));
            (src_idx, anc, None)
        };

        let mut model = PivotTableModel::new(
            pivot_name.clone(),
            src_sheet_name,
            Some(src_sheet_id),
            target_sheet_idx,
            anchor,
            spec,
        );
        model.options = opts;
        model.ensure_ext_synced();
        model.last_source_rev = src_rev;

        // 计算并写入样式化网格
        let src_snap = self.wb.sheets[self.source_sheet_index_of(&model).unwrap_or(src_idx)].clone();
        let computed = sheetcore::formula::with_date_system(self.wb.date_system, || {
            compute_table(&src_snap, &model)
        });

        let mut deltas = Vec::new();
        let mut col_widths = Vec::new();
        let rect = if inserted_sheet_idx.is_some() {
            let target_sh = &mut self.wb.sheets[target_sheet_idx];
            if model.spec.rows.is_empty()
                && model.spec.cols.is_empty()
                && model.spec.values.is_empty()
            {
                // 空白透视表占位提示框（仿 Excel 空白透视表）
                Self::write_empty_pivot_placeholder(target_sh, &mut self.wb.styles, anchor, &model.name)
            } else {
                write_table_styled(target_sh, &mut self.wb.styles, anchor, &computed, &model)
            }
        } else {
            let est_rect = if model.spec.rows.is_empty()
                && model.spec.cols.is_empty()
                && model.spec.values.is_empty()
            {
                (anchor.0, anchor.1, anchor.0 + 4, anchor.1 + 3)
            } else {
                computed.meta.rect
            };
            let before_cells = capture_rect_cells(&self.wb.sheets[target_sheet_idx], est_rect);
            let before_widths = capture_col_widths(&self.wb.sheets[target_sheet_idx], est_rect.1, est_rect.3);
            let out_r = if model.spec.rows.is_empty()
                && model.spec.cols.is_empty()
                && model.spec.values.is_empty()
            {
                Self::write_empty_pivot_placeholder(
                    &mut self.wb.sheets[target_sheet_idx],
                    &mut self.wb.styles,
                    anchor,
                    &model.name,
                )
            } else {
                write_table_styled(
                    &mut self.wb.sheets[target_sheet_idx],
                    &mut self.wb.styles,
                    anchor,
                    &computed,
                    &model,
                )
            };
            let full_r = union_rect(est_rect, out_r);
            deltas = build_rect_deltas(
                target_sheet_idx,
                &before_cells,
                &self.wb.sheets[target_sheet_idx],
                full_r,
            );
            col_widths = build_col_width_deltas(
                &before_widths,
                &self.wb.sheets[target_sheet_idx],
                full_r.1,
                full_r.3,
            );
            out_r
        };

        model.rect = rect;
        model.last_meta = computed.meta;
        model.last_meta.rect = rect;
        let new_id = model.id;
        self.wb.pivots.push(model);

        let inserted_sheet = inserted_sheet_idx.map(|idx| (idx, self.wb.sheets[idx].clone()));
        self.commit_pivot_tx(
            format!("创建数据透视表「{pivot_name}」"),
            focus_before,
            PivotUndoEdit {
                target_sheet: target_sheet_idx,
                inserted_sheet,
                deltas,
                col_widths,
                pivots_before,
                pivots_after: self.wb.pivots.clone(),
                removed_before,
                removed_after: self.wb.pivots_removed_parts.clone(),
            },
        );

        self.sheet_idx = target_sheet_idx;
        self.sel = anchor;
        self.sel_end = anchor;
        self.extra_sel.clear();
        self.pivot_ui.active_pivot_id = Some(new_id);
        self.pivot_ui.sidebar_open = true;
        self.pivot_ui.sidebar_dismissed_id = None;
        self.ribbon_tab = 103; // 自动切到「透视表分析」上下文页签
        self.status = format!(
            "已创建数据透视表「{pivot_name}」（可在右侧拖拽字段实时调整，支持 Ctrl+Z 撤销）"
        );
    }

    fn write_empty_pivot_placeholder(
        sheet: &mut Sheet,
        styles: &mut sheetcore::style::StyleTable,
        anchor: (u32, u32),
        name: &str,
    ) -> (u32, u32, u32, u32) {
        let (r0, c0) = anchor;
        let b = sheetcore::style::Border {
            style: 5, // dashed
            color: [148, 163, 184],
        };
        let title_st = styles.intern(sheetcore::style::CellStyle {
            bold: true,
            color: [30, 64, 138],
            fill: Some([238, 244, 252]),
            halign: 1,
            valign: 2,
            border: [b, b, b, sheetcore::style::Border::default()],
            ..sheetcore::style::CellStyle::default_dark()
        });
        let hint_st = styles.intern(sheetcore::style::CellStyle {
            italic: true,
            color: [100, 116, 139],
            fill: Some([248, 250, 252]),
            halign: 1,
            valign: 2,
            border: [b, b, sheetcore::style::Border::default(), b],
            ..sheetcore::style::CellStyle::default_dark()
        });
        sheet.set(
            r0,
            c0,
            Cell {
                value: Value::Text(name.into()),
                formula: None,
            },
        );
        sheet.set_style(r0, c0, title_st);
        sheet.set(
            r0 + 1,
            c0,
            Cell {
                value: Value::Text("请在右侧「数据透视表字段」面板勾选或拖拽字段以生成报表".into()),
                formula: None,
            },
        );
        sheet.set_style(r0 + 1, c0, hint_st);
        sheet.cols.set(c0, 220.0);
        sheet.layout_dirty = true;
        sheet.styles_dirty = true;
        (r0, c0, r0 + 3, c0 + 2)
    }

    /// 重算并原地刷新指定透视表（支持大表后台线程计算、源区自动扩展、可撤销）。
    pub(crate) fn recompute_pivot_by_id(
        &mut self,
        pivot_id: u64,
        label: &str,
        record_undo: bool,
    ) {
        let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == pivot_id) else {
            return;
        };
        self.wb.pivots[pidx].ensure_ext_synced();
        let Some(src_idx) = self.source_sheet_index_of(&self.wb.pivots[pidx]) else {
            self.status = format!("找不到透视表源工作表「{}」", self.wb.pivots[pidx].source_sheet);
            return;
        };
        // P2：自动探测源区域下方新增数据行
        if self.wb.pivots[pidx].options.auto_expand_source
            && let Some(expanded) =
                detect_expanded_source(&self.wb.sheets[src_idx], self.wb.pivots[pidx].spec.source)
        {
            self.wb.pivots[pidx].spec.source = expanded;
        }
        let src_rev = self.wb.sheets[src_idx].cell_revision();
        let src_rect = self.wb.pivots[pidx].spec.source;
        let row_span = src_rect.2.saturating_sub(src_rect.0);

        if !cfg!(test) && row_span > 8_000 {
            let doc_uid = self.doc_uid;
            let ds = self.wb.date_system;
            let src_snap = self.wb.sheets[src_idx].clone();
            let model_snap = self.wb.pivots[pidx].clone();
            let pivots_before = self
                .pivot_ui
                .pending_undo_pivots_before
                .take()
                .unwrap_or_else(|| self.wb.pivots.clone());
            let lbl = label.to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            self.pivot_ui.bg_rx = Some(rx);
            self.status = format!("正在后台刷新透视表「{}」…", model_snap.name);
            std::thread::spawn(move || {
                let computed = sheetcore::formula::with_date_system(ds, || {
                    compute_table(&src_snap, &model_snap)
                });
                let _ = tx.send(BgModelPivotDone {
                    doc_uid,
                    pivot_id,
                    source_rev: src_rev,
                    source_rect: src_rect,
                    computed,
                    label: lbl,
                    record_undo,
                    pivots_before,
                });
            });
            return;
        }

        let computed = sheetcore::formula::with_date_system(self.wb.date_system, || {
            compute_table(&self.wb.sheets[src_idx], &self.wb.pivots[pidx])
        });
        let pivots_before = self
            .pivot_ui
            .pending_undo_pivots_before
            .take()
            .unwrap_or_else(|| self.wb.pivots.clone());
        self.commit_computed_pivot(
            pivot_id,
            src_rev,
            src_rect,
            computed,
            label,
            record_undo,
            pivots_before,
        );
    }

    pub(crate) fn poll_pivot_model_bg(&mut self) {
        let Some(rx) = &self.pivot_ui.bg_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(done) => {
                self.pivot_ui.bg_rx = None;
                if done.doc_uid == self.doc_uid {
                    self.commit_computed_pivot(
                        done.pivot_id,
                        done.source_rev,
                        done.source_rect,
                        done.computed,
                        &done.label,
                        done.record_undo,
                        done.pivots_before,
                    );
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pivot_ui.bg_rx = None;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_computed_pivot(
        &mut self,
        pivot_id: u64,
        src_rev: u64,
        src_rect: (u32, u32, u32, u32),
        computed: ComputedPivot,
        label: &str,
        record_undo: bool,
        pivots_before: Vec<PivotTableModel>,
    ) {
        let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == pivot_id) else {
            return;
        };
        let target_sheet = self.wb.pivots[pidx].target_sheet;
        if target_sheet >= self.wb.sheets.len() {
            return;
        }
        let anchor = self.wb.pivots[pidx].anchor;
        let old_rect = self.wb.pivots[pidx].rect;
        let removed_before = self.wb.pivots_removed_parts.clone();

        let is_empty = self.wb.pivots[pidx].spec.rows.is_empty()
            && self.wb.pivots[pidx].spec.cols.is_empty()
            && self.wb.pivots[pidx].spec.values.is_empty();
        let est_new = if is_empty {
            (anchor.0, anchor.1, anchor.0 + 3, anchor.1 + 2)
        } else {
            computed.meta.rect
        };
        let dirty_rect = union_rect(old_rect, est_new);
        let before_cells = capture_rect_cells(&self.wb.sheets[target_sheet], dirty_rect);
        let before_widths =
            capture_col_widths(&self.wb.sheets[target_sheet], dirty_rect.1, dirty_rect.3);

        // 清除旧透视表占用的单元格与样式
        {
            let sh = &mut self.wb.sheets[target_sheet];
            for r in old_rect.0..=old_rect.2 {
                for c in old_rect.1..=old_rect.3 {
                    sh.remove(r, c);
                    sh.set_style(r, c, 0);
                }
            }
        }

        let model_clone = self.wb.pivots[pidx].clone();
        let new_rect = if is_empty {
            Self::write_empty_pivot_placeholder(
                &mut self.wb.sheets[target_sheet],
                &mut self.wb.styles,
                anchor,
                &model_clone.name,
            )
        } else {
            write_table_styled(
                &mut self.wb.sheets[target_sheet],
                &mut self.wb.styles,
                anchor,
                &computed,
                &model_clone,
            )
        };

        let model = &mut self.wb.pivots[pidx];
        model.spec.source = src_rect;
        model.rect = new_rect;
        model.last_source_rev = src_rev;
        model.modified = true;
        model.last_meta = computed.meta;
        model.last_meta.rect = new_rect;
        let name = model.name.clone();

        if record_undo {
            let full_r = union_rect(dirty_rect, new_rect);
            let deltas = build_rect_deltas(
                target_sheet,
                &before_cells,
                &self.wb.sheets[target_sheet],
                full_r,
            );
            let col_widths = build_col_width_deltas(
                &before_widths,
                &self.wb.sheets[target_sheet],
                full_r.1,
                full_r.3,
            );
            self.commit_pivot_tx(
                label.to_string(),
                (self.sheet_idx, self.sel.0, self.sel.1),
                PivotUndoEdit {
                    target_sheet,
                    inserted_sheet: None,
                    deltas,
                    col_widths,
                    pivots_before,
                    pivots_after: self.wb.pivots.clone(),
                    removed_before,
                    removed_after: self.wb.pivots_removed_parts.clone(),
                },
            );
        } else {
            let effects = sheetcore::tx::TxEffects {
                invalidate_render_caches: true,
                touched_sheets: vec![target_sheet],
                ..Default::default()
            };
            self.apply_tx_effects(label, &effects);
        }

        self.status = format!(
            "透视表「{name}」已更新：{} 行 × {} 列",
            new_rect.2 - new_rect.0 + 1,
            new_rect.3 - new_rect.1 + 1
        );
    }

    /// 删除指定透视表（清除单元格 + 标记移除导入部件 + 支持 Ctrl+Z 撤销）。
    pub(crate) fn delete_pivot_by_id(&mut self, pivot_id: u64) {
        let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == pivot_id) else {
            return;
        };
        let target_sheet = self.wb.pivots[pidx].target_sheet;
        let rect = self.wb.pivots[pidx].rect;
        let name = self.wb.pivots[pidx].name.clone();
        let pivots_before = self.wb.pivots.clone();
        let removed_before = self.wb.pivots_removed_parts.clone();
        let before_cells = capture_rect_cells(&self.wb.sheets[target_sheet], rect);
        {
            let sh = &mut self.wb.sheets[target_sheet];
            for r in rect.0..=rect.2 {
                for c in rect.1..=rect.3 {
                    sh.remove(r, c);
                    sh.set_style(r, c, 0);
                }
            }
            sh.styles_dirty = true;
        }
        let removed = self.wb.pivots.remove(pidx);
        if let Some(part) = removed.imported_part {
            self.wb.pivots_removed_parts.push(part);
        }
        let deltas = build_rect_deltas(
            target_sheet,
            &before_cells,
            &self.wb.sheets[target_sheet],
            rect,
        );
        self.commit_pivot_tx(
            format!("删除透视表「{name}」"),
            (self.sheet_idx, self.sel.0, self.sel.1),
            PivotUndoEdit {
                target_sheet,
                inserted_sheet: None,
                deltas,
                col_widths: Vec::new(),
                pivots_before,
                pivots_after: self.wb.pivots.clone(),
                removed_before,
                removed_after: self.wb.pivots_removed_parts.clone(),
            },
        );
        if self.pivot_ui.active_pivot_id == Some(pivot_id) {
            self.pivot_ui.active_pivot_id = None;
            self.pivot_ui.sidebar_open = false;
        }
        self.pivot_ui.field_settings = None;
        self.pivot_ui.grid_filter = None;
        self.status = format!("已删除透视表「{name}」（可 Ctrl+Z 撤销）");
    }

    /// 刷新当前透视表（或整簿全部透视表），兼顾新对象模型与旧兼容路径。
    pub(crate) fn refresh_pivots_modern(&mut self, all: bool) {
        if self.edit_blocked() {
            return;
        }
        if all {
            let ids: Vec<u64> = self.wb.pivots.iter().map(|p| p.id).collect();
            if ids.is_empty() {
                self.refresh_pivot();
                return;
            }
            for id in ids {
                self.recompute_pivot_by_id(id, "刷新全部透视表", true);
            }
            self.status = format!("已刷新全部 {} 个数据透视表", self.wb.pivots.len());
            return;
        }
        if let Some(idx) = self.active_pivot_index() {
            let id = self.wb.pivots[idx].id;
            self.recompute_pivot_by_id(id, "刷新数据透视表", true);
        } else if let Some(first) = self
            .wb
            .pivots
            .iter()
            .find(|p| p.target_sheet == self.sheet_idx)
        {
            let id = first.id;
            self.recompute_pivot_by_id(id, "刷新数据透视表", true);
        } else {
            self.refresh_pivot();
        }
    }

    /// P2：自动检测源表修改，若透视表开启了「自动刷新」则静默重算。
    pub(crate) fn tick_pivot_auto_refresh(&mut self) {
        if self.editing.is_some() || self.pivot_ui.bg_rx.is_some() {
            return;
        }
        let mut to_refresh = Vec::new();
        for p in &self.wb.pivots {
            if p.target_sheet != self.sheet_idx || !p.options.auto_refresh {
                continue;
            }
            if let Some(si) = self.source_sheet_index_of(p) {
                let rev = self.wb.sheets[si].cell_revision();
                if rev != p.last_source_rev
                    && self.pivot_ui.last_auto_check_rev.get(&p.id) != Some(&rev)
                {
                    to_refresh.push((p.id, rev));
                }
            }
        }
        for (id, rev) in to_refresh {
            self.pivot_ui.last_auto_check_rev.insert(id, rev);
            self.recompute_pivot_by_id(id, "源数据变动自动刷新透视表", false);
        }
    }

    /// 双击透视表数据/小计/总计格 → 自动新建明细工作表展示构成该数字的全部源记录。
    pub(crate) fn try_drill_down_pivot_at(&mut self, r: u32, c: u32) -> bool {
        let Some(pidx) = self
            .wb
            .pivots
            .iter()
            .position(|p| p.contains_cell(self.sheet_idx, r, c))
        else {
            return false;
        };
        let Some(drill) = self.wb.pivots[pidx].last_meta.drill_filter_at(r, c) else {
            return false;
        };
        let model = self.wb.pivots[pidx].clone();
        let Some(src_idx) = self.source_sheet_index_of(&model) else {
            self.status = format!("找不到源工作表「{}」，无法下钻明细", model.source_sheet);
            return true;
        };
        let rows = sheetcore::formula::with_date_system(self.wb.date_system, || {
            drill_down_rows(&self.wb.sheets[src_idx], &model, &drill)
        });
        let data_rows = rows.len().saturating_sub(1);
        // 生成合法且唯一的工作表名（Excel 限制 <= 31 字符且不含 \/?*[]:）
        let clean_lbl: String = drill
            .label
            .chars()
            .filter(|ch| !matches!(ch, '\\' | '/' | '?' | '*' | '[' | ']' | ':'))
            .take(18)
            .collect();
        let base_name = if clean_lbl.is_empty() {
            "透视明细".to_string()
        } else {
            format!("明细-{clean_lbl}")
        };
        let mut sh_name = base_name.clone();
        let mut seq = 2usize;
        while self
            .wb
            .sheets
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(&sh_name))
        {
            sh_name = format!("{base_name}({seq})");
            seq += 1;
        }
        let ins_idx = self.sheet_idx + 1;
        let mut detail_sh = Sheet::new(&sh_name);
        detail_sh.edited = true;
        let pal = model.options.style_theme.palette();
        let thin_b = sheetcore::style::Border {
            style: 1,
            color: pal.border,
        };
        let hdr_st = self.wb.styles.intern(sheetcore::style::CellStyle {
            bold: true,
            color: pal.header_fg,
            fill: Some(pal.header_bg),
            halign: 1,
            valign: 2,
            border: [thin_b, thin_b, thin_b, thin_b],
            ..sheetcore::style::CellStyle::default_dark()
        });
        let band_st = self.wb.styles.intern(sheetcore::style::CellStyle {
            fill: Some(pal.banded_bg),
            ..sheetcore::style::CellStyle::default_dark()
        });
        let mut col_w: Vec<f32> = Vec::new();
        for (ri, line) in rows.into_iter().enumerate() {
            if col_w.len() < line.len() {
                col_w.resize(line.len(), 76.0);
            }
            for (ci, val) in line.into_iter().enumerate() {
                let txt = val.display();
                let px: f32 = txt
                    .chars()
                    .map(|ch| if ch.is_ascii() { 7.5 } else { 13.5 })
                    .sum::<f32>()
                    + 26.0;
                col_w[ci] = col_w[ci].max(px.clamp(72.0, 260.0));
                if !matches!(val, Value::Empty) {
                    detail_sh.set(
                        ri as u32,
                        ci as u32,
                        Cell {
                            value: val,
                            formula: None,
                        },
                    );
                }
                if ri == 0 {
                    detail_sh.set_style(0, ci as u32, hdr_st);
                } else if ri % 2 == 0 {
                    detail_sh.set_style(ri as u32, ci as u32, band_st);
                }
            }
        }
        for (ci, w) in col_w.iter().enumerate() {
            detail_sh.cols.set(ci as u32, *w);
        }
        if !col_w.is_empty() && data_rows > 0 {
            detail_sh.filter = Some(sheetcore::sort_filter::Filter::new(
                0,
                0,
                data_rows as u32,
                col_w.len() as u32 - 1,
            ));
            detail_sh.filter_dirty = true;
        }
        detail_sh.layout_dirty = true;
        detail_sh.styles_dirty = true;
        sheetcore::sheet_ops::insert_sheet(&mut self.wb, ins_idx, detail_sh);
        self.sheet_idx = ins_idx;
        self.sel = (0, 0);
        self.sel_end = (0, 0);
        self.extra_sel.clear();
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已下钻明细「{sh_name}」：共 {data_rows} 条源记录");
        true
    }

    /// 切换某个行/列分组前缀的折叠状态并立即重算。
    pub(crate) fn toggle_pivot_collapse(&mut self, pivot_id: u64, is_row: bool, path: Vec<String>) {
        let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == pivot_id) else {
            return;
        };
        self.capture_pivot_undo_before();
        let list = if is_row {
            &mut self.wb.pivots[pidx].options.collapsed_rows
        } else {
            &mut self.wb.pivots[pidx].options.collapsed_cols
        };
        let collapsing;
        if let Some(pos) = list.iter().position(|p| *p == path) {
            list.remove(pos);
            collapsing = false;
        } else {
            list.push(path);
            collapsing = true;
        }
        self.recompute_pivot_by_id(
            pivot_id,
            if collapsing { "折叠透视表分组" } else { "展开透视表分组" },
            true,
        );
    }

    /// 展开或折叠当前透视表的全部一级分组。
    pub(crate) fn pivot_expand_or_collapse_all(&mut self, collapse: bool) {
        let Some(pidx) = self.active_pivot_index() else {
            return;
        };
        self.capture_pivot_undo_before();
        let pid = self.wb.pivots[pidx].id;
        if !collapse {
            self.wb.pivots[pidx].options.collapsed_rows.clear();
            self.wb.pivots[pidx].options.collapsed_cols.clear();
            self.recompute_pivot_by_id(pid, "展开透视表全部分组", true);
            return;
        }
        let Some(src_idx) = self.source_sheet_index_of(&self.wb.pivots[pidx]) else {
            return;
        };
        let src = self.wb.pivots[pidx].spec.source;
        if self.wb.pivots[pidx].spec.rows.len() >= 2 {
            let rf = self.wb.pivots[pidx].spec.rows[0].clone();
            let vals = distinct_field_values(&self.wb.sheets[src_idx], src, rf.col, &rf.group);
            self.wb.pivots[pidx].options.collapsed_rows =
                vals.into_iter().map(|v| vec![v]).collect();
        }
        if self.wb.pivots[pidx].spec.cols.len() >= 2 {
            let cf = self.wb.pivots[pidx].spec.cols[0].clone();
            let vals = distinct_field_values(&self.wb.sheets[src_idx], src, cf.col, &cf.group);
            self.wb.pivots[pidx].options.collapsed_cols =
                vals.into_iter().map(|v| vec![v]).collect();
        }
        self.recompute_pivot_by_id(pid, "折叠透视表全部分组", true);
    }

    /// 供「透视表分析 / 透视表设计」与右键菜单调用的快捷命令执行。
    pub(crate) fn run_pivot_command(&mut self, action: &str) -> bool {
        let pid_opt = self.active_pivot().map(|p| p.id);
        match action {
            "PivotRefresh" => {
                self.refresh_pivots_modern(false);
                true
            }
            "PivotRefreshAll" => {
                self.refresh_pivots_modern(true);
                true
            }
            "PivotFieldList" => {
                if let Some(pid) = pid_opt {
                    self.pivot_ui.active_pivot_id = Some(pid);
                    self.pivot_ui.sidebar_open = !self.pivot_ui.sidebar_open;
                    self.pivot_ui.sidebar_dismissed_id = if self.pivot_ui.sidebar_open {
                        None
                    } else {
                        Some(pid)
                    };
                } else {
                    self.status = "请先选中一个数据透视表".into();
                }
                true
            }
            "PivotDrillDown" => {
                if !self.try_drill_down_pivot_at(self.sel.0, self.sel.1) {
                    self.status = "请先选中透视表中的数值汇总单元格再查看明细".into();
                }
                true
            }
            "PivotExpandAll" => {
                self.pivot_expand_or_collapse_all(false);
                true
            }
            "PivotCollapseAll" => {
                self.pivot_expand_or_collapse_all(true);
                true
            }
            "PivotOptionsDialog" | "PivotChangeSource" | "PivotCalcField" => {
                if let Some(p) = self.active_pivot().cloned() {
                    self.pivot_ui.source_range_edit = format!(
                        "{}:{}",
                        cell_name(p.spec.source.0, p.spec.source.1),
                        cell_name(p.spec.source.2, p.spec.source.3)
                    );
                    self.pivot_ui.options_pivot_id = Some(p.id);
                } else {
                    self.status = "请先选中一个数据透视表".into();
                }
                true
            }
            "PivotInsertSlicer" => {
                if let Some(p) = self.active_pivot().cloned() {
                    self.pivot_ui.slicer_picker_pivot_id = Some(p.id);
                    self.pivot_ui.slicer_picker_checked =
                        p.options.slicers.iter().map(|s| s.col).collect();
                } else if self.pivot_last.is_some() {
                    self.slicer_open();
                } else {
                    self.status = "请先新建或选中一个数据透视表".into();
                }
                true
            }
            "PivotDelete" => {
                if let Some(pid) = pid_opt {
                    self.delete_pivot_by_id(pid);
                }
                true
            }
            "PivotToggleAutoRefresh" => {
                if let Some(idx) = self.active_pivot_index() {
                    let pid = self.wb.pivots[idx].id;
                    let next = !self.wb.pivots[idx].options.auto_refresh;
                    self.wb.pivots[idx].options.auto_refresh = next;
                    self.wb.pivots[idx].modified = true;
                    self.status = if next {
                        "已开启自动刷新：源数据改动后透视表自动更新".into()
                    } else {
                        "已关闭自动刷新（源数据变动时仍会显示刷新角标提示）".into()
                    };
                    if next {
                        self.recompute_pivot_by_id(pid, "开启透视表自动刷新", false);
                    }
                }
                true
            }
            "PivotToggleDataBars" => {
                if let Some(idx) = self.active_pivot_index() {
                    self.capture_pivot_undo_before();
                    let pid = self.wb.pivots[idx].id;
                    let next = !self.wb.pivots[idx].options.show_data_bars;
                    self.wb.pivots[idx].options.show_data_bars = next;
                    self.recompute_pivot_by_id(pid, "切换透视表内置数据条", true);
                }
                true
            }
            "PivotToggleOutliers" => {
                if let Some(idx) = self.active_pivot_index() {
                    self.capture_pivot_undo_before();
                    let pid = self.wb.pivots[idx].id;
                    let next = !self.wb.pivots[idx].options.highlight_outliers;
                    self.wb.pivots[idx].options.highlight_outliers = next;
                    self.recompute_pivot_by_id(pid, "切换透视表极值高亮", true);
                }
                true
            }
            "PivotLayoutCompact" | "PivotLayoutOutline" | "PivotLayoutTabular" => {
                if let Some(idx) = self.active_pivot_index() {
                    self.capture_pivot_undo_before();
                    let pid = self.wb.pivots[idx].id;
                    self.wb.pivots[idx].options.report_layout = match action {
                        "PivotLayoutCompact" => ReportLayout::Compact,
                        "PivotLayoutOutline" => ReportLayout::Outline,
                        _ => ReportLayout::Tabular,
                    };
                    self.recompute_pivot_by_id(pid, "切换透视表报表布局", true);
                }
                true
            }
            "PivotRepeatLabels" => {
                if let Some(idx) = self.active_pivot_index() {
                    self.capture_pivot_undo_before();
                    let pid = self.wb.pivots[idx].id;
                    let next = !self.wb.pivots[idx].options.repeat_labels;
                    self.wb.pivots[idx].options.repeat_labels = next;
                    self.recompute_pivot_by_id(pid, "切换重复项目标签", true);
                }
                true
            }
            "PivotSubtotalsOff" | "PivotSubtotalsBottom" | "PivotSubtotalsTop" => {
                if let Some(idx) = self.active_pivot_index() {
                    self.capture_pivot_undo_before();
                    let pid = self.wb.pivots[idx].id;
                    self.wb.pivots[idx].options.subtotal_pos = match action {
                        "PivotSubtotalsOff" => SubtotalPos::Off,
                        "PivotSubtotalsTop" => SubtotalPos::Top,
                        _ => SubtotalPos::Bottom,
                    };
                    self.recompute_pivot_by_id(pid, "调整透视表分类汇总", true);
                }
                true
            }
            "PivotGrandBoth" | "PivotGrandRow" | "PivotGrandCol" | "PivotGrandOff" => {
                if let Some(idx) = self.active_pivot_index() {
                    self.capture_pivot_undo_before();
                    let pid = self.wb.pivots[idx].id;
                    let (rg, cg) = match action {
                        "PivotGrandBoth" => (true, true),
                        "PivotGrandRow" => (true, false),
                        "PivotGrandCol" => (false, true),
                        _ => (false, false),
                    };
                    self.wb.pivots[idx].options.row_grand_totals = rg;
                    self.wb.pivots[idx].options.col_grand_totals = cg;
                    self.recompute_pivot_by_id(pid, "调整透视表总计开关", true);
                }
                true
            }
            "PivotBandedRows" => {
                if let Some(idx) = self.active_pivot_index() {
                    self.capture_pivot_undo_before();
                    let pid = self.wb.pivots[idx].id;
                    let next = !self.wb.pivots[idx].options.banded_rows;
                    self.wb.pivots[idx].options.banded_rows = next;
                    self.recompute_pivot_by_id(pid, "切换透视表镶边行", true);
                }
                true
            }
            "PivotBandedCols" => {
                if let Some(idx) = self.active_pivot_index() {
                    self.capture_pivot_undo_before();
                    let pid = self.wb.pivots[idx].id;
                    let next = !self.wb.pivots[idx].options.banded_cols;
                    self.wb.pivots[idx].options.banded_cols = next;
                    self.recompute_pivot_by_id(pid, "切换透视表镶边列", true);
                }
                true
            }
            "PivotStyleBlue"
            | "PivotStyleGreen"
            | "PivotStyleSlate"
            | "PivotStyleAmber"
            | "PivotStyleDark"
            | "PivotStyleMinimal" => {
                if let Some(idx) = self.active_pivot_index() {
                    self.capture_pivot_undo_before();
                    let pid = self.wb.pivots[idx].id;
                    self.wb.pivots[idx].options.style_theme = match action {
                        "PivotStyleGreen" => PivotStyleTheme::Green,
                        "PivotStyleSlate" => PivotStyleTheme::Slate,
                        "PivotStyleAmber" => PivotStyleTheme::Amber,
                        "PivotStyleDark" => PivotStyleTheme::Dark,
                        "PivotStyleMinimal" => PivotStyleTheme::Minimal,
                        _ => PivotStyleTheme::Blue,
                    };
                    self.recompute_pivot_by_id(pid, "切换透视表样式主题", true);
                }
                true
            }
            _ => false,
        }
    }

    /// 供透视表右键菜单快速改值字段汇总方式或显示方式或排序。
    pub(crate) fn pivot_quick_set_agg(&mut self, agg: Agg) {
        let Some(pidx) = self.active_pivot_index() else {
            return;
        };
        let pid = self.wb.pivots[pidx].id;
        let rel_c = self.sel.1.saturating_sub(self.wb.pivots[pidx].anchor.1) as usize;
        let vi = self.wb.pivots[pidx]
            .last_meta
            .col_value_idx
            .get(rel_c)
            .copied()
            .flatten()
            .unwrap_or(0);
        if vi < self.wb.pivots[pidx].spec.values.len() {
            self.capture_pivot_undo_before();
            let vf = &mut self.wb.pivots[pidx].spec.values[vi];
            vf.agg = agg;
            vf.label = None;
            self.recompute_pivot_by_id(pid, &format!("汇总方式改为{}", agg.label()), true);
        }
    }

    pub(crate) fn pivot_quick_set_show_as(&mut self, show_as: ShowAs) {
        let Some(pidx) = self.active_pivot_index() else {
            return;
        };
        let pid = self.wb.pivots[pidx].id;
        let rel_c = self.sel.1.saturating_sub(self.wb.pivots[pidx].anchor.1) as usize;
        let vi = self.wb.pivots[pidx]
            .last_meta
            .col_value_idx
            .get(rel_c)
            .copied()
            .flatten()
            .unwrap_or(0);
        if vi < self.wb.pivots[pidx].spec.values.len() {
            self.capture_pivot_undo_before();
            self.wb.pivots[pidx].spec.values[vi].show_as = show_as;
            self.recompute_pivot_by_id(pid, &format!("值显示方式改为{}", show_as.label()), true);
        }
    }

    pub(crate) fn pivot_quick_sort(&mut self, mode: FieldSortMode) {
        let Some(pidx) = self.active_pivot_index() else {
            return;
        };
        let pid = self.wb.pivots[pidx].id;
        self.capture_pivot_undo_before();
        self.wb.pivots[pidx].ensure_ext_synced();
        if let Some(ext) = self.wb.pivots[pidx].options.row_ext.first_mut() {
            ext.sort = mode;
            self.recompute_pivot_by_id(pid, &format!("透视表排序：{}", mode.label()), true);
        }
    }

    // =========================================================================
    // 1. 「创建数据透视表」智能向导对话框
    // =========================================================================

    pub(crate) fn draw_pivot_create_wizard(&mut self, ctx: &egui::Context) {
        let Some(mut wiz) = self.pivot_ui.create_wizard.take() else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        let pal = theme::palette();

        ui_kit::window(ctx, "创建数据透视表", 540.0)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("1. 数据源与放置位置")
                        .strong()
                        .size(13.0)
                        .color(pal.text),
                );
                ui.add_space(4.0);
                egui::Grid::new("pivot_wiz_grid")
                    .num_columns(2)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("源工作表");
                        ui.label(
                            egui::RichText::new(format!(
                                "「{}」（共 {} 行 × {} 列）",
                                wiz.source_sheet_name,
                                wiz.source_rect.2.saturating_sub(wiz.source_rect.0),
                                wiz.source_rect.3.saturating_sub(wiz.source_rect.1) + 1
                            ))
                            .color(pal.text_weak),
                        );
                        ui.end_row();

                        ui.label("数据区域");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut wiz.source_text)
                                    .desired_width(170.0),
                            );
                            ui.checkbox(&mut wiz.auto_expand, "刷新时自动包含新增行");
                        });
                        ui.end_row();

                        ui.label("放置位置");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut wiz.dest_new_sheet, true, "新工作表（推荐）");
                            ui.radio_value(&mut wiz.dest_new_sheet, false, "现有工作表");
                            if !wiz.dest_new_sheet {
                                ui.add(
                                    egui::TextEdit::singleline(&mut wiz.dest_cell_text)
                                        .desired_width(72.0)
                                        .hint_text("如 G1"),
                                );
                            }
                        });
                        ui.end_row();

                        ui.label("实时联动");
                        ui.checkbox(
                            &mut wiz.auto_refresh,
                            "源数据改动时自动刷新透视表（关闭时显示角标一键刷新）",
                        );
                        ui.end_row();
                    });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("2. 智能推荐视图（一键出表，创建后仍可随时拖拽调整）")
                            .strong()
                            .size(13.0)
                            .color(pal.text),
                    );
                });
                ui.add_space(6.0);

                // 推荐卡片列表 + 空白透视表卡片
                let card_w = ui.available_width();
                for (i, preset) in wiz.presets.iter().enumerate() {
                    let selected = wiz.selected_preset == Some(i);
                    let fill = if selected {
                        pal.accent_tint
                    } else {
                        pal.card_bg
                    };
                    let stroke = if selected {
                        Stroke::new(1.5, pal.accent)
                    } else {
                        Stroke::new(1.0, pal.card_border)
                    };
                    let resp = egui::Frame::NONE
                        .fill(fill)
                        .stroke(stroke)
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.set_width((card_w - 22.0).max(200.0));
                            ui.horizontal(|ui| {
                                egui::Frame::NONE
                                    .fill(if selected { pal.accent } else { pal.accent_tint })
                                    .corner_radius(3.0)
                                    .inner_margin(egui::Margin::symmetric(5, 1))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new("推荐")
                                                .color(if selected { pal.on_accent() } else { pal.accent })
                                                .strong()
                                                .size(11.0),
                                        );
                                    });
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(&preset.title)
                                            .strong()
                                            .size(13.0)
                                            .color(pal.text),
                                    );
                                    ui.label(
                                        egui::RichText::new(&preset.subtitle)
                                            .size(11.0)
                                            .color(pal.text_weak),
                                    );
                                });
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        wiz.selected_preset = Some(i);
                    }
                    if resp.double_clicked() {
                        wiz.selected_preset = Some(i);
                        confirm = true;
                    }
                    ui.add_space(4.0);
                }

                // 空白透视表选项
                let blank_sel = wiz.selected_preset.is_none();
                let resp = egui::Frame::NONE
                    .fill(if blank_sel { pal.accent_tint } else { pal.card_bg })
                    .stroke(if blank_sel {
                        Stroke::new(1.5, pal.accent)
                    } else {
                        Stroke::new(1.0, pal.card_border)
                    })
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(10, 7))
                    .show(ui, |ui| {
                        ui.set_width((card_w - 22.0).max(200.0));
                        ui.label(
                            egui::RichText::new("空白数据透视表（打开右侧字段面板自行拖拽布局）")
                                .size(12.0)
                                .color(pal.text),
                        );
                    })
                    .response
                    .interact(Sense::click());
                if resp.clicked() {
                    wiz.selected_preset = None;
                }
                if resp.double_clicked() {
                    wiz.selected_preset = None;
                    confirm = true;
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_kit::secondary_button(ui, "取消").clicked() {
                            cancel = true;
                        }
                        if ui_kit::primary_button(ui, "确定创建").clicked() {
                            confirm = true;
                        }
                    });
                });
            });

        if confirm {
            self.create_pivot_from_wizard(wiz);
        } else if !cancel && open {
            self.pivot_ui.create_wizard = Some(wiz);
        }
    }

    // =========================================================================
    // 2. 右侧可停靠「数据透视表字段」侧边栏（含搜索、画像悬停、真实拖拽四宫格）
    // =========================================================================

    pub(crate) fn sync_pivot_sidebar_selection(&mut self) {
        if let Some(pidx) = self.pivot_at_sel() {
            let pid = self.wb.pivots[pidx].id;
            if self.pivot_ui.active_pivot_id != Some(pid) {
                self.pivot_ui.active_pivot_id = Some(pid);
                if self.pivot_ui.sidebar_dismissed_id != Some(pid) {
                    self.pivot_ui.sidebar_open = true;
                }
            }
        } else if !self.pivot_ui.sidebar_pinned {
            self.pivot_ui.sidebar_open = false;
            self.pivot_ui.active_pivot_id = None;
        }
    }

    pub(crate) fn draw_pivot_sidebar(&mut self, ctx: &egui::Context) {
        self.sync_pivot_sidebar_selection();
        if !self.pivot_ui.sidebar_open {
            return;
        }
        let Some(pidx) = self.active_pivot_index() else {
            return;
        };
        let model = self.wb.pivots[pidx].clone();
        let pid = model.id;
        let profiles = self.profiles_for_pivot(&model);
        let pal = theme::palette();

        let mut close_clicked = false;
        let mut dirty_spec: Option<(PivotTableModel, String)> = None;
        let mut manual_refresh = false;
        let mut open_options = false;
        let mut open_slicers = false;

        // 拖拽落点收集：记录四宫格屏幕矩形
        let mut zone_rects: [(PivotZone, Rect); 4] = [
            (PivotZone::Filter, Rect::NOTHING),
            (PivotZone::Col, Rect::NOTHING),
            (PivotZone::Row, Rect::NOTHING),
            (PivotZone::Value, Rect::NOTHING),
        ];

        egui::SidePanel::right("pivot_field_sidebar")
            .resizable(true)
            .show_separator_line(false)
            .default_width(326.0)
            .width_range(280.0..=460.0)
            .frame(
                egui::Frame::NONE
                    .fill(pal.page_bg)
                    .inner_margin(egui::Margin {
                        left: 0,
                        right: 8,
                        top: 4,
                        bottom: 4,
                    }),
            )
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(pal.card_bg)
                    .stroke(Stroke::new(1.0, pal.card_border))
                    .corner_radius(ui_kit::CARD_R)
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        let mut edit_model = model.clone();
                        let mut changed_label: Option<String> = None;

                        // ---- 顶部标题栏 ----
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("数据透视表字段")
                                    .strong()
                                    .size(14.0)
                                    .color(pal.text),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .small_button("×")
                                    .on_hover_text("关闭字段列表")
                                    .clicked()
                                {
                                    close_clicked = true;
                                }
                                if ui
                                    .selectable_label(
                                        self.pivot_ui.sidebar_pinned,
                                        egui::RichText::new(if self.pivot_ui.sidebar_pinned { "已固定" } else { "固定" })
                                            .size(11.0),
                                    )
                                    .on_hover_text("固定侧边栏（点选透视表外时不自动收起）")
                                    .clicked()
                                {
                                    self.pivot_ui.sidebar_pinned = !self.pivot_ui.sidebar_pinned;
                                }
                                if ui
                                    .small_button("⚙")
                                    .on_hover_text("透视表选项、数据源与计算字段")
                                    .clicked()
                                {
                                    open_options = true;
                                }
                                if ui
                                    .small_button("↻")
                                    .on_hover_text("立即刷新数据透视表")
                                    .clicked()
                                {
                                    manual_refresh = true;
                                }
                            });
                        });
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · 源 {}!{}:{}（{} 条匹配）",
                                edit_model.name,
                                edit_model.source_sheet,
                                cell_name(edit_model.spec.source.0, edit_model.spec.source.1),
                                cell_name(edit_model.spec.source.2, edit_model.spec.source.3),
                                edit_model.last_meta.matched_row_count
                            ))
                            .size(11.0)
                            .color(pal.text_weak),
                        );
                        ui.add_space(6.0);

                        // ---- 字段搜索框 ----
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.pivot_ui.field_search)
                                    .hint_text("搜索字段（勾选或拖入下方区域）…")
                                    .desired_width(ui.available_width() - 24.0),
                            );
                            if !self.pivot_ui.field_search.is_empty() && ui.small_button("×").clicked() {
                                self.pivot_ui.field_search.clear();
                            }
                        });
                        ui.add_space(6.0);

                        // ---- 上半区：字段勾选清单（带类型徽标 + 悬停画像 + 拖拽手柄）----
                        let list_h = (ui.available_height() * 0.27).clamp(96.0, 175.0);
                        egui::Frame::NONE
                            .fill(pal.page_bg)
                            .stroke(Stroke::new(1.0, pal.card_border))
                            .corner_radius(5.0)
                            .inner_margin(egui::Margin::same(6))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("pivot_field_list_scroll")
                                    .max_height(list_h)
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        let q = self.pivot_ui.field_search.trim().to_lowercase();
                                        for prof in &profiles {
                                            if !q.is_empty() && !prof.name.to_lowercase().contains(&q) {
                                                continue;
                                            }
                                            let in_row = edit_model.spec.rows.iter().any(|f| f.col == prof.col);
                                            let in_col = edit_model.spec.cols.iter().any(|f| f.col == prof.col);
                                            let in_val = edit_model.spec.values.iter().any(|f| f.col == prof.col);
                                            let in_flt = edit_model.spec.filters.iter().any(|f| f.col == prof.col);
                                            let mut checked = in_row || in_col || in_val || in_flt;

                                            let row_resp = ui.horizontal(|ui| {
                                                if ui.checkbox(&mut checked, "").changed() {
                                                    if checked {
                                                        // 智能分配：数值进「值」，文本/日期进「行」
                                                        if prof.kind == FieldKind::Numeric {
                                                            edit_model.spec.values.push(ValueField {
                                                                col: prof.col,
                                                                agg: Agg::Sum,
                                                                label: None,
                                                                show_as: ShowAs::Normal,
                                                                base: None,
                                                            });
                                                            edit_model.options.val_ext.push(ValueFieldExt::default());
                                                            changed_label = Some(format!("添加值字段「{}」", prof.name));
                                                        } else {
                                                            edit_model.spec.rows.push(AxisField::plain(prof.col));
                                                            edit_model.options.row_ext.push(AxisFieldExt::default());
                                                            changed_label = Some(format!("添加行字段「{}」", prof.name));
                                                        }
                                                    } else {
                                                        remove_col_everywhere(&mut edit_model, prof.col);
                                                        changed_label = Some(format!("移除字段「{}」", prof.name));
                                                    }
                                                }
                                                let badge_col = match prof.kind {
                                                    FieldKind::Numeric => pal.accent,
                                                    FieldKind::Date => pal.info_fg,
                                                    FieldKind::Text => pal.text_weak,
                                                };
                                                let drag_lbl = ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(format!(
                                                            "[{}] {}",
                                                            prof.kind.badge(),
                                                            prof.name
                                                        ))
                                                        .size(12.0)
                                                        .color(if checked { pal.text } else { pal.text_weak })
                                                        .strong(),
                                                    )
                                                    .sense(Sense::drag()),
                                                );
                                                if drag_lbl.hovered() {
                                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                                                }
                                                if drag_lbl.drag_started() {
                                                    self.pivot_ui.drag = Some(PivotDragPayload {
                                                        source: PivotDragSource::FieldList(prof.col),
                                                        col: prof.col,
                                                        label: prof.name.clone(),
                                                    });
                                                }
                                                // P2：悬停字段即刻展示画像统计（唯一值数/非空率/范围/样例）
                                                drag_lbl.on_hover_ui(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "{}（{}）",
                                                            prof.name,
                                                            prof.kind.badge()
                                                        ))
                                                        .strong()
                                                        .color(badge_col),
                                                    );
                                                    ui.label(format!(
                                                        "非空：{}/{} · 唯一值：{}{}",
                                                        prof.non_empty,
                                                        prof.total_rows,
                                                        prof.unique_count,
                                                        if prof.unique_count >= 1000 { "+" } else { "" }
                                                    ));
                                                    if let (Some(lo), Some(hi)) = (prof.min_num, prof.max_num) {
                                                        ui.label(format!(
                                                            "范围：{} ~ {}",
                                                            sheetcore::model::format_number(lo),
                                                            sheetcore::model::format_number(hi)
                                                        ));
                                                    }
                                                    if !prof.samples.is_empty() {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "示例：{}",
                                                                prof.samples.join("、")
                                                            ))
                                                            .weak()
                                                            .size(11.0),
                                                        );
                                                    }
                                                    ui.label(
                                                        egui::RichText::new("拖动可放入下方 筛选 / 列 / 行 / 值")
                                                            .weak()
                                                            .size(11.0),
                                                    );
                                                });
                                            });
                                            let _ = row_resp;
                                        }
                                    });
                            });

                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("在以下区域间拖动字段（点击芯片可设汇总/排序/分组/筛选）：")
                                .size(11.0)
                                .color(pal.text_weak),
                        );
                        ui.add_space(4.0);

                        // ---- 中间：2×2 Excel 经典四宫格拖拽区（筛选 / 列 / 行 / 值）----
                        let half_w = ((ui.available_width() - 8.0) * 0.5).max(115.0);
                        let zone_h = ((ui.available_height() - 100.0) * 0.5).clamp(100.0, 180.0);
                        let pointer_pos = ctx.input(|i| i.pointer.hover_pos());
                        let is_dragging = self.pivot_ui.drag.is_some();

                        let mut open_settings_req: Option<FieldSettingsPopup> = None;

                        // 第一排：筛选 | 列
                        ui.horizontal(|ui| {
                            zone_rects[0].1 = Self::draw_drop_zone_card(
                                ui,
                                "筛选",
                                PivotZone::Filter,
                                half_w,
                                zone_h,
                                is_dragging,
                                pointer_pos,
                                |ui| {
                                    let n = edit_model.spec.filters.len();
                                    for i in 0..n {
                                        let col = edit_model.spec.filters[i].col;
                                        let fname = self.field_header_name(&edit_model, col);
                                        let keep_n = edit_model.spec.filters[i].keep.len();
                                        let chip_txt = if keep_n == 0 {
                                            fname.clone()
                                        } else {
                                            format!("{fname} ({keep_n})")
                                        };
                                        let act = draw_zone_chip(
                                            ui,
                                            &chip_txt,
                                            keep_n > 0,
                                            &mut self.pivot_ui.drag,
                                            PivotDragSource::Zone(PivotZone::Filter, i),
                                            col,
                                            &fname,
                                        );
                                        match act {
                                            ChipAct::Click => {
                                                let items = self.distinct_for_col(&edit_model, col, &GroupBy::None);
                                                open_settings_req = Some(FieldSettingsPopup::Filter {
                                                    pivot_id: pid,
                                                    filter_idx: i,
                                                    search: String::new(),
                                                    distinct_items: items,
                                                });
                                            }
                                            ChipAct::Remove => {
                                                edit_model.spec.filters.remove(i);
                                                changed_label = Some(format!("移除筛选字段「{fname}」"));
                                                break;
                                            }
                                            ChipAct::None => {}
                                        }
                                    }
                                },
                            );
                            ui.add_space(6.0);
                            zone_rects[1].1 = Self::draw_drop_zone_card(
                                ui,
                                "列",
                                PivotZone::Col,
                                half_w,
                                zone_h,
                                is_dragging,
                                pointer_pos,
                                |ui| {
                                    let n = edit_model.spec.cols.len();
                                    for i in 0..n {
                                        let col = edit_model.spec.cols[i].col;
                                        let grp = edit_model.spec.cols[i].group.clone();
                                        let fname = self.field_header_name(&edit_model, col);
                                        let ext = edit_model.options.col_ext.get(i);
                                        let filtered = ext.is_some_and(|e| !e.hidden_items.is_empty() || e.top_n.is_some());
                                        let chip_txt = if grp == GroupBy::None {
                                            fname.clone()
                                        } else {
                                            format!("{fname} ({})", grp.label())
                                        };
                                        let act = draw_zone_chip(
                                            ui,
                                            &chip_txt,
                                            filtered,
                                            &mut self.pivot_ui.drag,
                                            PivotDragSource::Zone(PivotZone::Col, i),
                                            col,
                                            &fname,
                                        );
                                        match act {
                                            ChipAct::Click => {
                                                let items = self.distinct_for_col(&edit_model, col, &grp);
                                                let step = match grp {
                                                    GroupBy::Bucket(w) => format!("{w}"),
                                                    _ => "100".into(),
                                                };
                                                let tn = ext
                                                    .and_then(|e| e.top_n)
                                                    .map(|t| t.count.to_string())
                                                    .unwrap_or_else(|| "10".into());
                                                open_settings_req = Some(FieldSettingsPopup::Axis {
                                                    pivot_id: pid,
                                                    is_row: false,
                                                    field_idx: i,
                                                    bucket_step: step,
                                                    top_n_count: tn,
                                                    search: String::new(),
                                                    distinct_items: items,
                                                });
                                            }
                                            ChipAct::Remove => {
                                                edit_model.spec.cols.remove(i);
                                                if i < edit_model.options.col_ext.len() {
                                                    edit_model.options.col_ext.remove(i);
                                                }
                                                changed_label = Some(format!("移除列字段「{fname}」"));
                                                break;
                                            }
                                            ChipAct::None => {}
                                        }
                                    }
                                },
                            );
                        });
                        ui.add_space(6.0);
                        // 第二排：行 | 值
                        ui.horizontal(|ui| {
                            zone_rects[2].1 = Self::draw_drop_zone_card(
                                ui,
                                "行",
                                PivotZone::Row,
                                half_w,
                                zone_h,
                                is_dragging,
                                pointer_pos,
                                |ui| {
                                    let n = edit_model.spec.rows.len();
                                    for i in 0..n {
                                        let col = edit_model.spec.rows[i].col;
                                        let grp = edit_model.spec.rows[i].group.clone();
                                        let fname = self.field_header_name(&edit_model, col);
                                        let ext = edit_model.options.row_ext.get(i);
                                        let filtered = ext.is_some_and(|e| !e.hidden_items.is_empty() || e.top_n.is_some());
                                        let chip_txt = if grp == GroupBy::None {
                                            fname.clone()
                                        } else {
                                            format!("{fname} ({})", grp.label())
                                        };
                                        let act = draw_zone_chip(
                                            ui,
                                            &chip_txt,
                                            filtered,
                                            &mut self.pivot_ui.drag,
                                            PivotDragSource::Zone(PivotZone::Row, i),
                                            col,
                                            &fname,
                                        );
                                        match act {
                                            ChipAct::Click => {
                                                let items = self.distinct_for_col(&edit_model, col, &grp);
                                                let step = match grp {
                                                    GroupBy::Bucket(w) => format!("{w}"),
                                                    _ => "100".into(),
                                                };
                                                let tn = ext
                                                    .and_then(|e| e.top_n)
                                                    .map(|t| t.count.to_string())
                                                    .unwrap_or_else(|| "10".into());
                                                open_settings_req = Some(FieldSettingsPopup::Axis {
                                                    pivot_id: pid,
                                                    is_row: true,
                                                    field_idx: i,
                                                    bucket_step: step,
                                                    top_n_count: tn,
                                                    search: String::new(),
                                                    distinct_items: items,
                                                });
                                            }
                                            ChipAct::Remove => {
                                                edit_model.spec.rows.remove(i);
                                                if i < edit_model.options.row_ext.len() {
                                                    edit_model.options.row_ext.remove(i);
                                                }
                                                changed_label = Some(format!("移除行字段「{fname}」"));
                                                break;
                                            }
                                            ChipAct::None => {}
                                        }
                                    }
                                },
                            );
                            ui.add_space(6.0);
                            zone_rects[3].1 = Self::draw_drop_zone_card(
                                ui,
                                "值",
                                PivotZone::Value,
                                half_w,
                                zone_h,
                                is_dragging,
                                pointer_pos,
                                |ui| {
                                    let n = edit_model.spec.values.len();
                                    for i in 0..n {
                                        let vf = &edit_model.spec.values[i];
                                        let col = vf.col;
                                        let fname = self.field_header_name(&edit_model, col);
                                        let caption = vf
                                            .label
                                            .clone()
                                            .filter(|s| !s.is_empty())
                                            .unwrap_or_else(|| format!("{}:{}", vf.agg.label(), fname));
                                        let has_bar = edit_model
                                            .options
                                            .val_ext
                                            .get(i)
                                            .is_some_and(|e| e.data_bar);
                                        let chip_txt = if vf.show_as != ShowAs::Normal {
                                            format!("{caption} [{}]", vf.show_as.label())
                                        } else {
                                            caption
                                        };
                                        let act = draw_zone_chip(
                                            ui,
                                            &chip_txt,
                                            has_bar || vf.show_as != ShowAs::Normal,
                                            &mut self.pivot_ui.drag,
                                            PivotDragSource::Zone(PivotZone::Value, i),
                                            col,
                                            &fname,
                                        );
                                        match act {
                                            ChipAct::Click => {
                                                let fmt = edit_model
                                                    .options
                                                    .val_ext
                                                    .get(i)
                                                    .and_then(|e| e.num_format.clone())
                                                    .unwrap_or_default();
                                                open_settings_req = Some(FieldSettingsPopup::Value {
                                                    pivot_id: pid,
                                                    val_idx: i,
                                                    name_draft: vf.label.clone().unwrap_or_default(),
                                                    fmt_draft: fmt,
                                                });
                                            }
                                            ChipAct::Remove => {
                                                edit_model.spec.values.remove(i);
                                                if i < edit_model.options.val_ext.len() {
                                                    edit_model.options.val_ext.remove(i);
                                                }
                                                changed_label = Some(format!("移除值字段「{fname}」"));
                                                break;
                                            }
                                            ChipAct::None => {}
                                        }
                                    }
                                },
                            );
                        });

                        if let Some(req) = open_settings_req {
                            self.pivot_ui.field_settings = Some(req);
                        }

                        // ---- 拖拽松手落点结算 ----
                        if let Some(payload) = self.pivot_ui.drag.clone() {
                            // 绘制跟随鼠标的悬浮拖拽芯片
                            if let Some(pos) = pointer_pos {
                                egui::Area::new(egui::Id::new("pivot_drag_ghost"))
                                    .order(egui::Order::Tooltip)
                                    .fixed_pos(pos + Vec2::new(10.0, 10.0))
                                    .interactable(false)
                                    .show(ctx, |ui| {
                                        egui::Frame::NONE
                                            .fill(pal.accent)
                                            .corner_radius(4.0)
                                            .inner_margin(egui::Margin::symmetric(8, 4))
                                            .show(ui, |ui| {
                                                ui.label(
                                                    egui::RichText::new(&payload.label)
                                                        .color(pal.on_accent())
                                                        .strong()
                                                        .size(12.0),
                                                );
                                            });
                                    });
                            }
                            if ctx.input(|i| i.pointer.any_released()) {
                                let target_zone = pointer_pos.and_then(|p| {
                                    zone_rects
                                        .iter()
                                        .find(|(_, r)| r.contains(p))
                                        .map(|(z, _)| *z)
                                });
                                if let Some(tz) = target_zone {
                                    apply_field_drop(&mut edit_model, &payload, tz);
                                    changed_label = Some(format!("拖动字段「{}」", payload.label));
                                } else if let PivotDragSource::Zone(src_z, src_i) = payload.source {
                                    // 拖到四宫格外部即移除该字段（经典 Excel 拖出移除手感）
                                    remove_from_zone(&mut edit_model, src_z, src_i);
                                    changed_label = Some(format!("拖出移除字段「{}」", payload.label));
                                }
                                self.pivot_ui.drag = None;
                            }
                        }

                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(4.0);

                        // ---- 底部快捷栏：样式主题条 / 布局 / 数据条 / 切片器 / 延迟更新 ----
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("配色").size(11.0).color(pal.text_weak));
                            for th in PivotStyleTheme::ALL {
                                let p = th.palette();
                                let col = Color32::from_rgb(p.header_bg[0], p.header_bg[1], p.header_bg[2]);
                                let active = edit_model.options.style_theme == *th;
                                let (r, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
                                ui.painter().rect_filled(r, 3.0, col);
                                ui.painter().rect_stroke(
                                    r,
                                    3.0,
                                    Stroke::new(
                                        if active { 2.0 } else { 1.0 },
                                        if active { pal.accent } else { pal.card_border },
                                    ),
                                    egui::StrokeKind::Inside,
                                );
                                if resp.on_hover_text(th.label()).clicked() {
                                    edit_model.options.style_theme = *th;
                                    changed_label = Some(format!("切换主题：{}", th.label()));
                                }
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("切片器…").clicked() {
                                    open_slicers = true;
                                }
                            });
                        });

                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui
                                .checkbox(&mut edit_model.options.show_data_bars, "数据条")
                                .on_hover_text("在数值列中绘制直观占比条")
                                .changed()
                            {
                                changed_label = Some("切换内置数据条".into());
                            }
                            if ui
                                .checkbox(&mut edit_model.options.row_grand_totals, "行总计")
                                .changed()
                            {
                                changed_label = Some("切换行总计".into());
                            }
                            if ui
                                .checkbox(&mut edit_model.options.col_grand_totals, "列总计")
                                .changed()
                            {
                                changed_label = Some("切换列总计".into());
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut edit_model.options.defer_layout_update,
                                "延迟布局更新",
                            );
                            if edit_model.options.defer_layout_update {
                                if ui_kit::primary_button(ui, "更新").clicked() {
                                    manual_refresh = true;
                                }
                            } else if ui
                                .checkbox(&mut edit_model.options.auto_refresh, "源改自动刷")
                                .changed()
                            {
                                self.wb.pivots[pidx].options.auto_refresh =
                                    edit_model.options.auto_refresh;
                            }
                        });

                        if let Some(lbl) = changed_label {
                            dirty_spec = Some((edit_model, lbl));
                        }
                    });
            });

        if close_clicked {
            self.pivot_ui.sidebar_open = false;
            self.pivot_ui.sidebar_pinned = false;
            self.pivot_ui.sidebar_dismissed_id = Some(pid);
        }
        if open_options {
            self.run_pivot_command("PivotOptionsDialog");
        }
        if open_slicers {
            self.run_pivot_command("PivotInsertSlicer");
        }
        if let Some((mut updated, lbl)) = dirty_spec {
            self.capture_pivot_undo_before();
            updated.ensure_ext_synced();
            let defer = updated.options.defer_layout_update;
            if let Some(slot) = self.wb.pivots.iter_mut().find(|p| p.id == pid) {
                *slot = updated;
            }
            if !defer {
                self.recompute_pivot_by_id(pid, &lbl, true);
            }
        } else if manual_refresh {
            self.recompute_pivot_by_id(pid, "刷新数据透视表", true);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_drop_zone_card(
        ui: &mut egui::Ui,
        title: &str,
        zone: PivotZone,
        width: f32,
        height: f32,
        is_dragging: bool,
        pointer_pos: Option<Pos2>,
        content: impl FnOnce(&mut egui::Ui),
    ) -> Rect {
        let pal = theme::palette();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
        let hovered = is_dragging && pointer_pos.is_some_and(|p| rect.contains(p));
        let fill = if hovered { pal.accent_tint } else { pal.page_bg };
        let stroke = if hovered {
            Stroke::new(1.8, pal.accent)
        } else if is_dragging {
            Stroke::new(1.2, pal.accent.gamma_multiply(0.55))
        } else {
            Stroke::new(1.0, pal.card_border)
        };
        ui.painter().rect(rect, 5.0, fill, stroke, egui::StrokeKind::Inside);

        let inner = rect.shrink(5.0);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(inner);
        child.label(
            egui::RichText::new(title)
                .strong()
                .size(11.0)
                .color(if hovered { pal.accent } else { pal.text_weak }),
        );
        child.add_space(2.0);
        egui::ScrollArea::vertical()
            .id_salt(("pivot_zone_scroll", zone as u8))
            .max_height((inner.height() - 18.0).max(32.0))
            .auto_shrink([false, false])
            .show(&mut child, content);
        rect
    }

    fn distinct_for_col(&self, model: &PivotTableModel, col: u32, group: &GroupBy) -> Vec<String> {
        let Some(si) = self.source_sheet_index_of(model) else {
            return Vec::new();
        };
        sheetcore::formula::with_date_system(self.wb.date_system, || {
            distinct_field_values(&self.wb.sheets[si], model.spec.source, col, group)
        })
    }

    // =========================================================================
    // 3. 字段设置弹窗（值设置 / 行列分组排序TopN筛选 / 页筛选）
    // =========================================================================

    pub(crate) fn draw_pivot_field_settings_popup(&mut self, ctx: &egui::Context) {
        let Some(mut popup) = self.pivot_ui.field_settings.take() else {
            return;
        };
        let snap_before = self.wb.pivots.clone();
        let mut open = true;
        let mut apply_id: Option<(u64, String)> = None;
        let mut keep_open = true;

        match &mut popup {
            FieldSettingsPopup::Value {
                pivot_id,
                val_idx,
                name_draft,
                fmt_draft,
            } => {
                let pid = *pivot_id;
                let vi = *val_idx;
                let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == pid) else {
                    return;
                };
                if vi >= self.wb.pivots[pidx].spec.values.len() {
                    return;
                }
                self.wb.pivots[pidx].ensure_ext_synced();
                let mut vf = self.wb.pivots[pidx].spec.values[vi].clone();
                let mut vext = self.wb.pivots[pidx].options.val_ext[vi].clone();
                let fname = self.field_header_name(&self.wb.pivots[pidx], vf.col);

                ui_kit::window(ctx, &format!("值字段设置 · {fname}"), 420.0)
                    .collapsible(false)
                    .resizable(false)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        egui::Grid::new("pivot_val_set_grid")
                            .num_columns(2)
                            .spacing([10.0, 6.0])
                            .show(ui, |ui| {
                                ui.label("自定义名称");
                                ui.add(
                                    egui::TextEdit::singleline(name_draft)
                                        .desired_width(240.0)
                                        .hint_text(format!("{}:{}", vf.agg.label(), fname)),
                                );
                                ui.end_row();

                                ui.label("汇总方式");
                                egui::ComboBox::from_id_salt("pvs_agg")
                                    .selected_text(vf.agg.label())
                                    .width(240.0)
                                    .show_ui(ui, |ui| {
                                        for &a in Agg::ALL {
                                            if ui.selectable_label(vf.agg == a, a.label()).clicked() {
                                                vf.agg = a;
                                            }
                                        }
                                    });
                                ui.end_row();

                                ui.label("值显示方式");
                                egui::ComboBox::from_id_salt("pvs_showas")
                                    .selected_text(vf.show_as.label())
                                    .width(240.0)
                                    .show_ui(ui, |ui| {
                                        for &sa in ShowAs::ALL {
                                            if ui.selectable_label(vf.show_as == sa, sa.label()).clicked() {
                                                vf.show_as = sa;
                                            }
                                        }
                                    });
                                ui.end_row();

                                ui.label("数字格式");
                                egui::ComboBox::from_id_salt("pvs_fmt")
                                    .selected_text(if fmt_draft.is_empty() {
                                        "自动（推荐）"
                                    } else {
                                        fmt_draft.as_str()
                                    })
                                    .width(240.0)
                                    .show_ui(ui, |ui| {
                                        for &(lbl, code) in NUMBER_FORMAT_PRESETS {
                                            if ui.selectable_label(fmt_draft == code, lbl).clicked() {
                                                *fmt_draft = code.to_string();
                                            }
                                        }
                                    });
                                ui.end_row();

                                ui.label("自定义格式码");
                                ui.add(
                                    egui::TextEdit::singleline(fmt_draft)
                                        .desired_width(240.0)
                                        .hint_text("留空按汇总/百分比自动匹配"),
                                );
                                ui.end_row();

                                ui.label("可视化增强");
                                ui.checkbox(&mut vext.data_bar, "在此值列显示格内数据条");
                                ui.end_row();
                            });

                        ui.add_space(6.0);
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui_kit::secondary_button(ui, "取消").clicked() {
                                    keep_open = false;
                                }
                                if ui_kit::primary_button(ui, "应用").clicked() {
                                    vf.label = (!name_draft.trim().is_empty())
                                        .then(|| name_draft.trim().to_string());
                                    vext.num_format = (!fmt_draft.trim().is_empty())
                                        .then(|| fmt_draft.trim().to_string());
                                    self.wb.pivots[pidx].spec.values[vi] = vf;
                                    self.wb.pivots[pidx].options.val_ext[vi] = vext;
                                    apply_id = Some((pid, format!("修改值字段「{fname}」设置")));
                                    keep_open = false;
                                }
                            });
                        });
                    });
            }
            FieldSettingsPopup::Axis {
                pivot_id,
                is_row,
                field_idx,
                bucket_step,
                top_n_count,
                search,
                distinct_items,
            } => {
                let pid = *pivot_id;
                let fi = *field_idx;
                let row_flag = *is_row;
                let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == pid) else {
                    return;
                };
                self.wb.pivots[pidx].ensure_ext_synced();
                let axis_len = if row_flag {
                    self.wb.pivots[pidx].spec.rows.len()
                } else {
                    self.wb.pivots[pidx].spec.cols.len()
                };
                if fi >= axis_len {
                    return;
                }
                let mut field = if row_flag {
                    self.wb.pivots[pidx].spec.rows[fi].clone()
                } else {
                    self.wb.pivots[pidx].spec.cols[fi].clone()
                };
                let mut ext = if row_flag {
                    self.wb.pivots[pidx].options.row_ext[fi].clone()
                } else {
                    self.wb.pivots[pidx].options.col_ext[fi].clone()
                };
                let fname = self.field_header_name(&self.wb.pivots[pidx], field.col);
                let val_names: Vec<String> = self.wb.pivots[pidx]
                    .spec
                    .values
                    .iter()
                    .map(|v| {
                        v.label
                            .clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| {
                                format!("{}:{}", v.agg.label(), self.field_header_name(&self.wb.pivots[pidx], v.col))
                            })
                    })
                    .collect();
                let mut group_changed = false;
                let mut quick_ym = false;

                ui_kit::window(ctx, &format!("字段设置 · {fname}"), 420.0)
                    .collapsible(false)
                    .resizable(false)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        egui::Grid::new("pivot_axis_set_grid")
                            .num_columns(2)
                            .spacing([10.0, 6.0])
                            .show(ui, |ui| {
                                ui.label("分组方式");
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt("pas_grp")
                                        .selected_text(field.group.label())
                                        .width(135.0)
                                        .show_ui(ui, |ui| {
                                            for g in [
                                                GroupBy::None,
                                                GroupBy::Year,
                                                GroupBy::Quarter,
                                                GroupBy::Month,
                                                GroupBy::Week,
                                                GroupBy::Day,
                                            ] {
                                                if ui.selectable_label(field.group == g, g.label()).clicked() {
                                                    field.group = g;
                                                    group_changed = true;
                                                }
                                            }
                                            let is_b = matches!(field.group, GroupBy::Bucket(_));
                                            if ui.selectable_label(is_b, "按数值区间").clicked() && !is_b {
                                                let w = bucket_step.trim().parse::<f64>().unwrap_or(100.0).max(0.01);
                                                field.group = GroupBy::Bucket(w);
                                                group_changed = true;
                                            }
                                        });
                                    if matches!(field.group, GroupBy::Bucket(_)) {
                                        ui.label("步长");
                                        if ui
                                            .add(egui::TextEdit::singleline(bucket_step).desired_width(60.0))
                                            .changed()
                                        {
                                            if let Ok(w) = bucket_step.trim().parse::<f64>()
                                                && w > 0.0
                                            {
                                                field.group = GroupBy::Bucket(w);
                                                group_changed = true;
                                            }
                                        }
                                    } else if ui
                                        .small_button("年+月组合")
                                        .on_hover_text("一键展开为「按年 → 按月」两级层次")
                                        .clicked()
                                    {
                                        quick_ym = true;
                                    }
                                });
                                ui.end_row();

                                ui.label("排序方式");
                                egui::ComboBox::from_id_salt("pas_sort")
                                    .selected_text(ext.sort.label())
                                    .width(230.0)
                                    .show_ui(ui, |ui| {
                                        if ui.selectable_label(ext.sort == FieldSortMode::PinyinAsc, "标签升序 (A→Z)").clicked() {
                                            ext.sort = FieldSortMode::PinyinAsc;
                                        }
                                        if ui.selectable_label(ext.sort == FieldSortMode::PinyinDesc, "标签降序 (Z→A)").clicked() {
                                            ext.sort = FieldSortMode::PinyinDesc;
                                        }
                                        for (vi, vn) in val_names.iter().enumerate() {
                                            if ui.selectable_label(ext.sort == FieldSortMode::ValueDesc(vi), format!("按「{vn}」降序")).clicked() {
                                                ext.sort = FieldSortMode::ValueDesc(vi);
                                            }
                                            if ui.selectable_label(ext.sort == FieldSortMode::ValueAsc(vi), format!("按「{vn}」升序")).clicked() {
                                                ext.sort = FieldSortMode::ValueAsc(vi);
                                            }
                                        }
                                        if ui.selectable_label(ext.sort == FieldSortMode::Source, "数据源顺序").clicked() {
                                            ext.sort = FieldSortMode::Source;
                                        }
                                    });
                                ui.end_row();

                                ui.label("Top N 筛选");
                                ui.horizontal(|ui| {
                                    let mut on = ext.top_n.is_some();
                                    if ui.checkbox(&mut on, "").changed() {
                                        if on {
                                            let cnt = top_n_count.trim().parse::<usize>().unwrap_or(10).max(1);
                                            ext.top_n = Some(TopNFilter {
                                                kind: TopNKind::Top,
                                                count: cnt,
                                                value_idx: 0,
                                            });
                                        } else {
                                            ext.top_n = None;
                                        }
                                    }
                                    if let Some(tn) = ext.top_n.as_mut() {
                                        egui::ComboBox::from_id_salt("pas_tn_kind")
                                            .selected_text(if tn.kind == TopNKind::Top { "前" } else { "后" })
                                            .width(46.0)
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(&mut tn.kind, TopNKind::Top, "前");
                                                ui.selectable_value(&mut tn.kind, TopNKind::Bottom, "后");
                                            });
                                        if ui
                                            .add(egui::TextEdit::singleline(top_n_count).desired_width(42.0))
                                            .changed()
                                            && let Ok(c) = top_n_count.trim().parse::<usize>()
                                            && c > 0
                                        {
                                            tn.count = c;
                                        }
                                        ui.label("项");
                                    } else {
                                        ui.label(egui::RichText::new("关闭（显示全部）").weak());
                                    }
                                });
                                ui.end_row();

                                ui.label("分类汇总");
                                ui.checkbox(&mut ext.show_subtotal, "为此字段显示分类汇总");
                                ui.end_row();
                            });

                        ui.add_space(6.0);
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("筛选项目").strong().size(12.0));
                            ui.add(
                                egui::TextEdit::singleline(search)
                                    .hint_text("搜索项目…")
                                    .desired_width(130.0),
                            );
                            if ui.small_button("全选").clicked() {
                                ext.hidden_items.clear();
                            }
                            if ui.small_button("全不选").clicked() {
                                ext.hidden_items = distinct_items.clone();
                            }
                        });
                        let q = search.trim().to_lowercase();
                        egui::ScrollArea::vertical()
                            .id_salt("pas_items_scroll")
                            .max_height(150.0)
                            .show(ui, |ui| {
                                for it in distinct_items.iter() {
                                    if !q.is_empty() && !it.to_lowercase().contains(&q) {
                                        continue;
                                    }
                                    let mut vis = !ext.hidden_items.iter().any(|h| h == it);
                                    if ui.checkbox(&mut vis, it).changed() {
                                        if vis {
                                            ext.hidden_items.retain(|h| h != it);
                                        } else {
                                            ext.hidden_items.push(it.clone());
                                        }
                                    }
                                }
                            });

                        ui.add_space(6.0);
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui_kit::secondary_button(ui, "关闭").clicked() {
                                    keep_open = false;
                                }
                                if ui_kit::primary_button(ui, "应用").clicked() {
                                    if row_flag {
                                        self.wb.pivots[pidx].spec.rows[fi] = field.clone();
                                        self.wb.pivots[pidx].options.row_ext[fi] = ext.clone();
                                    } else {
                                        self.wb.pivots[pidx].spec.cols[fi] = field.clone();
                                        self.wb.pivots[pidx].options.col_ext[fi] = ext.clone();
                                    }
                                    apply_id = Some((pid, format!("修改字段「{fname}」设置")));
                                    keep_open = false;
                                }
                            });
                        });
                    });

                if quick_ym {
                    let col = field.col;
                    if row_flag {
                        self.wb.pivots[pidx].spec.rows[fi].group = GroupBy::Year;
                        self.wb.pivots[pidx].spec.rows.insert(
                            fi + 1,
                            AxisField {
                                col,
                                group: GroupBy::Month,
                            },
                        );
                        self.wb.pivots[pidx]
                            .options
                            .row_ext
                            .insert(fi + 1, AxisFieldExt::default());
                    } else {
                        self.wb.pivots[pidx].spec.cols[fi].group = GroupBy::Year;
                        self.wb.pivots[pidx].spec.cols.insert(
                            fi + 1,
                            AxisField {
                                col,
                                group: GroupBy::Month,
                            },
                        );
                        self.wb.pivots[pidx]
                            .options
                            .col_ext
                            .insert(fi + 1, AxisFieldExt::default());
                    }
                    apply_id = Some((pid, format!("「{fname}」按年+月层次组合")));
                    keep_open = false;
                } else if group_changed {
                    let new_items = self.distinct_for_col(&self.wb.pivots[pidx], field.col, &field.group);
                    *distinct_items = new_items;
                    ext.hidden_items.clear();
                    if row_flag {
                        self.wb.pivots[pidx].spec.rows[fi] = field;
                        self.wb.pivots[pidx].options.row_ext[fi] = ext;
                    } else {
                        self.wb.pivots[pidx].spec.cols[fi] = field;
                        self.wb.pivots[pidx].options.col_ext[fi] = ext;
                    }
                } else if keep_open && open {
                    // 实时暂存勾选态，防止下一帧被原 model 覆盖
                    if row_flag {
                        self.wb.pivots[pidx].spec.rows[fi] = field;
                        self.wb.pivots[pidx].options.row_ext[fi] = ext;
                    } else {
                        self.wb.pivots[pidx].spec.cols[fi] = field;
                        self.wb.pivots[pidx].options.col_ext[fi] = ext;
                    }
                }
            }
            FieldSettingsPopup::Filter {
                pivot_id,
                filter_idx,
                search,
                distinct_items,
            } => {
                let pid = *pivot_id;
                let fi = *filter_idx;
                let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == pid) else {
                    return;
                };
                if fi >= self.wb.pivots[pidx].spec.filters.len() {
                    return;
                }
                let col = self.wb.pivots[pidx].spec.filters[fi].col;
                let fname = self.field_header_name(&self.wb.pivots[pidx], col);
                let mut keep = self.wb.pivots[pidx].spec.filters[fi].keep.clone();

                ui_kit::window(ctx, &format!("报表筛选 · {fname}"), 340.0)
                    .collapsible(false)
                    .resizable(false)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(search)
                                    .hint_text("搜索筛选值…")
                                    .desired_width(160.0),
                            );
                            if ui.small_button("全部").clicked() {
                                keep.clear();
                            }
                            if ui.small_button("清空").clicked() {
                                keep = vec!["\u{0001}__NONE__".into()];
                            }
                        });
                        ui.add_space(4.0);
                        let q = search.trim().to_lowercase();
                        egui::ScrollArea::vertical()
                            .id_salt("pfs_scroll")
                            .max_height(210.0)
                            .show(ui, |ui| {
                                for it in distinct_items.iter() {
                                    if !q.is_empty() && !it.to_lowercase().contains(&q) {
                                        continue;
                                    }
                                    let mut on = keep.is_empty() || keep.iter().any(|k| k == it);
                                    if ui.checkbox(&mut on, it).changed() {
                                        if keep.is_empty() {
                                            // 从「全部」切为具体保留列表（排除当前项）
                                            keep = distinct_items
                                                .iter()
                                                .filter(|x| *x != it)
                                                .cloned()
                                                .collect();
                                        } else if on {
                                            keep.retain(|k| k != "\u{0001}__NONE__");
                                            if !keep.iter().any(|k| k == it) {
                                                keep.push(it.clone());
                                            }
                                            if keep.len() >= distinct_items.len() {
                                                keep.clear();
                                            }
                                        } else {
                                            keep.retain(|k| k != it);
                                            if keep.is_empty() {
                                                keep.push("\u{0001}__NONE__".into());
                                            }
                                        }
                                    }
                                }
                            });
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui_kit::secondary_button(ui, "取消").clicked() {
                                    keep_open = false;
                                }
                                if ui_kit::primary_button(ui, "确定").clicked() {
                                    keep.retain(|k| k != "\u{0001}__NONE__");
                                    self.wb.pivots[pidx].spec.filters[fi].keep = keep.clone();
                                    apply_id = Some((pid, format!("报表筛选「{fname}」")));
                                    keep_open = false;
                                }
                            });
                        });
                    });
                if keep_open && open {
                    self.wb.pivots[pidx].spec.filters[fi].keep = keep;
                }
            }
        }

        if let Some((pid, lbl)) = apply_id {
            self.pivot_ui.pending_undo_pivots_before = Some(snap_before);
            self.recompute_pivot_by_id(pid, &lbl, true);
        } else if keep_open && open {
            self.pivot_ui.field_settings = Some(popup);
        }
    }

    // =========================================================================
    // 4. 透视表选项 / 数据源 / 计算字段与计算项对话框
    // =========================================================================

    pub(crate) fn draw_pivot_options_dialog(&mut self, ctx: &egui::Context) {
        let Some(pid) = self.pivot_ui.options_pivot_id else {
            return;
        };
        let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == pid) else {
            self.pivot_ui.options_pivot_id = None;
            return;
        };
        let mut open = true;
        let mut apply = false;
        let mut close_now = false;
        let mut model = self.wb.pivots[pidx].clone();
        let mut err_txt = model.options.error_cell_text.clone().unwrap_or_default();
        let mut err_enabled = model.options.error_cell_text.is_some();

        ui_kit::window(ctx, &format!("透视表选项 · {}", model.name), 500.0)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("pivot_opts_grid")
                    .num_columns(2)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("透视表名称");
                        ui.add(egui::TextEdit::singleline(&mut model.name).desired_width(240.0));
                        ui.end_row();

                        ui.label("数据源区域");
                        ui.horizontal(|ui| {
                            ui.label(format!("{}!", model.source_sheet));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.pivot_ui.source_range_edit)
                                    .desired_width(140.0),
                            );
                        });
                        ui.end_row();

                        ui.label("报表布局");
                        ui.horizontal(|ui| {
                            for l in [
                                ReportLayout::Tabular,
                                ReportLayout::Compact,
                                ReportLayout::Outline,
                            ] {
                                ui.radio_value(&mut model.options.report_layout, l, l.label());
                            }
                        });
                        ui.end_row();

                        ui.label("分类汇总");
                        ui.horizontal(|ui| {
                            for s in [SubtotalPos::Bottom, SubtotalPos::Top, SubtotalPos::Off] {
                                ui.radio_value(&mut model.options.subtotal_pos, s, s.label());
                            }
                        });
                        ui.end_row();

                        ui.label("空单元格显示");
                        ui.add(
                            egui::TextEdit::singleline(&mut model.options.empty_cell_text)
                                .desired_width(120.0)
                                .hint_text("留空或填 0 / -"),
                        );
                        ui.end_row();

                        ui.label("错误值显示");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut err_enabled, "替换为");
                            ui.add_enabled(
                                err_enabled,
                                egui::TextEdit::singleline(&mut err_txt)
                                    .desired_width(100.0)
                                    .hint_text("如 - 或 N/A"),
                            );
                        });
                        ui.end_row();

                        ui.label("样式与行为");
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut model.options.banded_rows, "镶边行");
                                ui.checkbox(&mut model.options.banded_cols, "镶边列");
                                ui.checkbox(&mut model.options.repeat_labels, "重复标签");
                                ui.checkbox(&mut model.options.autofit_cols, "自动列宽");
                            });
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut model.options.auto_expand_source, "自动扩展新增行");
                                ui.checkbox(&mut model.options.auto_refresh, "源改自动刷新");
                                ui.checkbox(&mut model.options.highlight_outliers, "Top10%/负值高亮");
                            });
                        });
                        ui.end_row();
                    });

                ui.add_space(6.0);
                ui.separator();
                ui.label(egui::RichText::new("计算字段（基于分组汇总值计算，如 毛利 = 收入 - 成本）").strong().size(12.0));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pivot_ui.new_calc_field_name)
                            .desired_width(100.0)
                            .hint_text("字段名"),
                    );
                    ui.label("=");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pivot_ui.new_calc_field_formula)
                            .desired_width(210.0)
                            .hint_text("公式，如 金额/数量"),
                    );
                    if ui.button("添加").clicked() {
                        let n = self.pivot_ui.new_calc_field_name.trim().to_string();
                        let f = self.pivot_ui.new_calc_field_formula.trim().to_string();
                        if !n.is_empty() && !f.is_empty() {
                            model.options.calc_fields.retain(|(en, _)| en != &n);
                            model.options.calc_fields.push((n, f));
                            self.pivot_ui.new_calc_field_name.clear();
                            self.pivot_ui.new_calc_field_formula.clear();
                        }
                    }
                });
                let mut rm_cf = None;
                for (i, (n, f)) in model.options.calc_fields.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("• {n} = {f}"));
                        if ui.small_button("删除").clicked() {
                            rm_cf = Some(i);
                        }
                    });
                }
                if let Some(i) = rm_cf {
                    model.options.calc_fields.remove(i);
                }

                ui.add_space(4.0);
                ui.label(egui::RichText::new("计算项（行项组合，如 华东华北 = 华东 + 华北）").strong().size(12.0));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pivot_ui.new_calc_item_name)
                            .desired_width(100.0)
                            .hint_text("项名称"),
                    );
                    ui.label("=");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pivot_ui.new_calc_item_expr)
                            .desired_width(210.0)
                            .hint_text("表达式，如 华东+华北"),
                    );
                    if ui.button("添加").clicked() {
                        let n = self.pivot_ui.new_calc_item_name.trim().to_string();
                        let f = self.pivot_ui.new_calc_item_expr.trim().to_string();
                        if !n.is_empty() && !f.is_empty() {
                            model.options.calc_items.retain(|(en, _)| en != &n);
                            model.options.calc_items.push((n, f));
                            self.pivot_ui.new_calc_item_name.clear();
                            self.pivot_ui.new_calc_item_expr.clear();
                        }
                    }
                });
                let mut rm_ci = None;
                for (i, (n, f)) in model.options.calc_items.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("• {n} = {f}"));
                        if ui.small_button("删除").clicked() {
                            rm_ci = Some(i);
                        }
                    });
                }
                if let Some(i) = rm_ci {
                    model.options.calc_items.remove(i);
                }

                ui.add_space(6.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_kit::secondary_button(ui, "取消").clicked() {
                            close_now = true;
                        }
                        if ui_kit::primary_button(ui, "保存并刷新").clicked() {
                            apply = true;
                        }
                    });
                });
            });

        model.options.error_cell_text = err_enabled.then_some(err_txt);
        if apply {
            self.capture_pivot_undo_before();
            if let Some(nr) = parse_a1_range(&self.pivot_ui.source_range_edit)
                && nr.2 > nr.0
                && nr.3 >= nr.1
            {
                model.spec.source = nr;
            }
            self.wb.pivots[pidx] = model;
            self.pivot_ui.options_pivot_id = None;
            self.recompute_pivot_by_id(pid, "修改透视表选项", true);
        } else if close_now || !open {
            self.pivot_ui.options_pivot_id = None;
        } else {
            self.wb.pivots[pidx] = model;
        }
    }

    // =========================================================================
    // 5. 网格内透视表覆层：数据条 / 极值高亮 / 折叠按钮 / 筛选箭头 / 源数据变动浮标
    // =========================================================================

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_pivot_grid_overlays<FX, FY>(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        gx: FX,
        gy: FY,
        cols: &sheetcore::layout::Axis,
        rows: &sheetcore::layout::Axis,
        grid_vis: Rect,
        readonly: bool,
    ) where
        FX: Fn(u32) -> f32,
        FY: Fn(u32) -> f32,
    {
        let sheet_idx = self.sheet_idx;
        let pivots_on_sheet: Vec<usize> = self
            .wb
            .pivots
            .iter()
            .enumerate()
            .filter_map(|(i, p)| (p.target_sheet == sheet_idx).then_some(i))
            .collect();
        if pivots_on_sheet.is_empty() {
            return;
        }

        let pal = theme::palette();
        let mut collapse_click: Option<(u64, bool, Vec<String>)> = None;
        let mut filter_click: Option<(u64, PivotFilterTarget, Pos2)> = None;
        let mut stale_refresh_id: Option<u64> = None;

        for &pidx in &pivots_on_sheet {
            let model = &self.wb.pivots[pidx];
            let meta = &model.last_meta;
            let (pr1, pc1, pr2, pc2) = model.rect;
            let p_screen = Rect::from_min_max(
                Pos2::new(gx(pc1), gy(pr1)),
                Pos2::new(gx(pc2 + 1), gy(pr2 + 1)),
            );
            if !p_screen.intersects(grid_vis.expand(60.0)) {
                continue;
            }

            // 1. 格内数据条（Sparkbar）与 Top10%/负值异常微底高亮
            if !meta.value_col_stats.is_empty() {
                let sh = self.wb.sheet(sheet_idx);
                for st in &meta.value_col_stats {
                    if !st.data_bar && !st.highlight_outliers {
                        continue;
                    }
                    let c = st.col;
                    let x0 = gx(c);
                    let w = cols.size(c);
                    if x0 + w < grid_vis.left() || x0 > grid_vis.right() {
                        continue;
                    }
                    for r in pr1..=pr2 {
                        if meta.row_kind(r) != Some(PivotRowKind::Detail) {
                            continue;
                        }
                        let y0 = gy(r);
                        let h = rows.size(r);
                        if y0 + h < grid_vis.top() || y0 > grid_vis.bottom() {
                            continue;
                        }
                        let Value::Number(val) = sh.value_ref(r, c).as_ref().clone() else {
                            continue;
                        };
                        if !val.is_finite() {
                            continue;
                        }
                        let cell_r = Rect::from_min_size(Pos2::new(x0, y0), Vec2::new(w, h));
                        if st.data_bar && st.max_abs > f64::EPSILON {
                            let frac = (val.abs() / st.max_abs).clamp(0.0, 1.0) as f32;
                            let bar_w = ((w - 6.0) * frac).max(2.0);
                            let bar_r = Rect::from_min_size(
                                Pos2::new(cell_r.left() + 3.0, cell_r.top() + 3.0),
                                Vec2::new(bar_w, (h - 6.0).max(4.0)),
                            );
                            let [br, bg, bb] = if val < 0.0 {
                                [220, 38, 38]
                            } else {
                                st.bar_color
                            };
                            painter.rect_filled(
                                bar_r,
                                2.0,
                                Color32::from_rgba_unmultiplied(br, bg, bb, 44),
                            );
                        }
                        if st.highlight_outliers {
                            if val < 0.0 {
                                painter.rect_stroke(
                                    cell_r.shrink(1.0),
                                    2.0,
                                    Stroke::new(1.2, Color32::from_rgba_unmultiplied(220, 38, 38, 150)),
                                    egui::StrokeKind::Inside,
                                );
                            } else if val >= st.p90 && st.max > st.min {
                                painter.circle_filled(
                                    Pos2::new(cell_r.left() + 6.0, cell_r.center().y),
                                    2.8,
                                    Color32::from_rgb(16, 185, 129),
                                );
                            }
                        }
                    }
                }
            }

            // 2. 多级展开/折叠 [+]/[-] 交互按钮
            for (bi, cb) in meta.collapse_buttons.iter().enumerate() {
                let x0 = gx(cb.col);
                let y0 = gy(cb.row);
                let h = rows.size(cb.row);
                let btn = Rect::from_center_size(
                    Pos2::new(x0 + 9.0, y0 + h * 0.5),
                    Vec2::splat(12.0),
                );
                if !grid_vis.intersects(btn) {
                    continue;
                }
                painter.rect(
                    btn,
                    2.0,
                    pal.card_bg,
                    Stroke::new(1.0, pal.text_weak),
                    egui::StrokeKind::Inside,
                );
                let c_pt = btn.center();
                let st = Stroke::new(1.2, pal.text);
                painter.line_segment(
                    [Pos2::new(c_pt.x - 3.0, c_pt.y), Pos2::new(c_pt.x + 3.0, c_pt.y)],
                    st,
                );
                if cb.collapsed {
                    painter.line_segment(
                        [Pos2::new(c_pt.x, c_pt.y - 3.0), Pos2::new(c_pt.x, c_pt.y + 3.0)],
                        st,
                    );
                }
                if !readonly {
                    let resp = ui.interact(
                        btn.expand(3.0),
                        egui::Id::new(("pivot_collapse", model.id, bi)),
                        Sense::click(),
                    );
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp
                        .on_hover_text(if cb.collapsed { "展开该分组" } else { "折叠该分组" })
                        .clicked()
                    {
                        collapse_click = Some((model.id, cb.is_row, cb.path.clone()));
                    }
                }
            }

            // 3. 页筛选与行列标题 ▾ 下拉筛选/排序按钮
            for (fi, fb) in meta.filter_buttons.iter().enumerate() {
                let x1 = gx(fb.col) + cols.size(fb.col);
                let y0 = gy(fb.row);
                let h = rows.size(fb.row);
                let btn = Rect::from_center_size(
                    Pos2::new(x1 - 10.0, y0 + h * 0.5),
                    Vec2::new(15.0, 15.0),
                );
                if !grid_vis.intersects(btn) {
                    continue;
                }
                let bg = if fb.active { pal.accent_tint } else { pal.card_bg };
                let bd = if fb.active { pal.accent } else { pal.card_border };
                let fg = if fb.active { pal.accent } else { pal.text_weak };
                painter.rect(btn, 2.5, bg, Stroke::new(1.0, bd), egui::StrokeKind::Inside);
                let cx = btn.center().x;
                let cy = btn.center().y;
                if fb.active {
                    // 矢量漏斗符号（激活筛选态，无字体缺字风险）
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            Pos2::new(cx - 4.0, cy - 3.2),
                            Pos2::new(cx + 4.0, cy - 3.2),
                            Pos2::new(cx + 1.2, cy + 0.5),
                            Pos2::new(cx - 1.2, cy + 0.5),
                        ],
                        fg,
                        Stroke::NONE,
                    ));
                    painter.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(cx - 1.0, cy + 0.5),
                            Pos2::new(cx + 1.0, cy + 3.8),
                        ),
                        0.0,
                        fg,
                    );
                } else {
                    // 矢量下拉实心小三角（高 DPI 锐利，无字体缺字风险）
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            Pos2::new(cx - 3.6, cy - 1.6),
                            Pos2::new(cx + 3.6, cy - 1.6),
                            Pos2::new(cx, cy + 2.6),
                        ],
                        fg,
                        Stroke::NONE,
                    ));
                }
                if !readonly {
                    let resp = ui.interact(
                        btn.expand(2.0),
                        egui::Id::new(("pivot_flt_btn", model.id, fi)),
                        Sense::click(),
                    );
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp
                        .on_hover_text(format!("筛选 / 排序「{}」", fb.label))
                        .clicked()
                    {
                        filter_click = Some((
                            model.id,
                            fb.target,
                            Pos2::new(gx(fb.col), y0 + h + 2.0),
                        ));
                    }
                }
            }

            // 4. P2 源数据变动感知浮标（源表被改过或新增了连续数据行时浮现一键刷新胶囊）
            if !readonly
                && let Some(src_idx) = self.source_sheet_index_of(model)
            {
                let src_sh = &self.wb.sheets[src_idx];
                let rev_changed = src_sh.cell_revision() != model.last_source_rev;
                let extra_rows = if model.options.auto_expand_source {
                    detect_expanded_source(src_sh, model.spec.source)
                        .map(|nr| nr.2.saturating_sub(model.spec.source.2))
                        .unwrap_or(0)
                } else {
                    0
                };
                if rev_changed || extra_rows > 0 {
                    let pill_txt = if extra_rows > 0 {
                        format!("↻ 源表新增 {extra_rows} 行 · 点击刷新")
                    } else {
                        "↻ 源数据已更新 · 点击刷新".into()
                    };
                    let pill_w = 176.0;
                    let pill_h = 22.0;
                    let px = (p_screen.right() - pill_w).max(p_screen.left()).clamp(
                        grid_vis.left() + 6.0,
                        (grid_vis.right() - pill_w - 6.0).max(grid_vis.left() + 6.0),
                    );
                    let py = (p_screen.top() - pill_h - 4.0).max(grid_vis.top() + 4.0);
                    let pill = Rect::from_min_size(Pos2::new(px, py), Vec2::new(pill_w, pill_h));
                    let resp = ui.interact(
                        pill,
                        egui::Id::new(("pivot_stale_pill", model.id)),
                        Sense::click(),
                    );
                    let pill_bg = if resp.hovered() {
                        pal.warn_fg
                    } else {
                        pal.warn_bg
                    };
                    let pill_fg = if resp.hovered() {
                        if pal.theme.is_dark() {
                            Color32::BLACK
                        } else {
                            Color32::WHITE
                        }
                    } else {
                        pal.warn_fg
                    };
                    painter.rect(
                        pill,
                        11.0,
                        pill_bg,
                        Stroke::new(1.0, pal.warn_border()),
                        egui::StrokeKind::Inside,
                    );
                    painter.text(
                        pill.center(),
                        Align2::CENTER_CENTER,
                        &pill_txt,
                        FontId::proportional(11.0),
                        pill_fg,
                    );
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        stale_refresh_id = Some(model.id);
                    }
                }
            }
        }

        if let Some((pid, is_row, path)) = collapse_click {
            self.toggle_pivot_collapse(pid, is_row, path);
        } else if let Some((pid, target, pos)) = filter_click {
            self.open_pivot_grid_filter(pid, target, pos);
        } else if let Some(pid) = stale_refresh_id {
            self.recompute_pivot_by_id(pid, "刷新数据透视表", true);
        }
    }

    fn open_pivot_grid_filter(&mut self, pivot_id: u64, target: PivotFilterTarget, pos: Pos2) {
        let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == pivot_id) else {
            return;
        };
        let model = &self.wb.pivots[pidx];
        let (col, grp, tn) = match target {
            PivotFilterTarget::PageFilter(fi) => {
                let Some(f) = model.spec.filters.get(fi) else { return };
                (f.col, GroupBy::None, "10".to_string())
            }
            PivotFilterTarget::RowField(fi) => {
                let Some(f) = model.spec.rows.get(fi) else { return };
                let tn = model
                    .options
                    .row_ext
                    .get(fi)
                    .and_then(|e| e.top_n)
                    .map(|t| t.count.to_string())
                    .unwrap_or_else(|| "10".into());
                (f.col, f.group.clone(), tn)
            }
            PivotFilterTarget::ColField(fi) => {
                let Some(f) = model.spec.cols.get(fi) else { return };
                let tn = model
                    .options
                    .col_ext
                    .get(fi)
                    .and_then(|e| e.top_n)
                    .map(|t| t.count.to_string())
                    .unwrap_or_else(|| "10".into());
                (f.col, f.group.clone(), tn)
            }
        };
        let items = self.distinct_for_col(model, col, &grp);
        self.pivot_ui.grid_filter = Some(PivotFilterPopupState {
            pivot_id,
            target,
            anchor_pos: pos,
            search: String::new(),
            items,
            top_n_input: tn,
        });
    }

    /// 网格内弹出的 Excel 风格透视表筛选与排序下拉菜单。
    pub(crate) fn draw_pivot_grid_filter_popup(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.pivot_ui.grid_filter.take() else {
            return;
        };
        let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == st.pivot_id) else {
            return;
        };
        let snap_before = self.wb.pivots.clone();
        self.wb.pivots[pidx].ensure_ext_synced();
        let pal = theme::palette();
        let mut keep_open = true;
        let mut apply_label: Option<String> = None;

        let area = egui::Area::new(egui::Id::new("pivot_grid_filter_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(st.anchor_pos)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(pal.card_bg)
                    .stroke(Stroke::new(1.0, pal.card_border))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(248.0);
                        match st.target {
                            PivotFilterTarget::RowField(fi) | PivotFilterTarget::ColField(fi) => {
                                let is_row = matches!(st.target, PivotFilterTarget::RowField(_));
                                let ext = if is_row {
                                    &mut self.wb.pivots[pidx].options.row_ext[fi]
                                } else {
                                    &mut self.wb.pivots[pidx].options.col_ext[fi]
                                };
                                // 快捷排序按钮行
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(ext.sort == FieldSortMode::PinyinAsc, "A→Z").clicked() {
                                        ext.sort = FieldSortMode::PinyinAsc;
                                        apply_label = Some("按标签升序排列".into());
                                    }
                                    if ui.selectable_label(ext.sort == FieldSortMode::PinyinDesc, "Z→A").clicked() {
                                        ext.sort = FieldSortMode::PinyinDesc;
                                        apply_label = Some("按标签降序排列".into());
                                    }
                                    if ui.selectable_label(matches!(ext.sort, FieldSortMode::ValueDesc(_)), "按值↓").clicked() {
                                        ext.sort = FieldSortMode::ValueDesc(0);
                                        apply_label = Some("按汇总值降序排列".into());
                                    }
                                    if ui.selectable_label(matches!(ext.sort, FieldSortMode::ValueAsc(_)), "按值↑").clicked() {
                                        ext.sort = FieldSortMode::ValueAsc(0);
                                        apply_label = Some("按汇总值升序排列".into());
                                    }
                                });
                                // Top N 快捷行
                                ui.horizontal(|ui| {
                                    let mut tn_on = ext.top_n.is_some();
                                    if ui.checkbox(&mut tn_on, "Top").changed() {
                                        if tn_on {
                                            let c = st.top_n_input.trim().parse::<usize>().unwrap_or(10).max(1);
                                            ext.top_n = Some(TopNFilter { kind: TopNKind::Top, count: c, value_idx: 0 });
                                        } else {
                                            ext.top_n = None;
                                        }
                                        apply_label = Some("切换 Top N 筛选".into());
                                    }
                                    if ui
                                        .add(egui::TextEdit::singleline(&mut st.top_n_input).desired_width(36.0))
                                        .changed()
                                        && let Some(tn) = ext.top_n.as_mut()
                                        && let Ok(c) = st.top_n_input.trim().parse::<usize>()
                                        && c > 0
                                    {
                                        tn.count = c;
                                        apply_label = Some(format!("筛选前 {c} 项"));
                                    }
                                    if ext.top_n.is_some() && ui.small_button("清除").clicked() {
                                        ext.top_n = None;
                                        apply_label = Some("清除 Top N 筛选".into());
                                    }
                                });
                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut st.search)
                                            .hint_text("搜索项目…")
                                            .desired_width(142.0),
                                    );
                                    if ui.small_button("全选").clicked() {
                                        ext.hidden_items.clear();
                                        apply_label = Some("清除字段筛选".into());
                                    }
                                    if ui.small_button("反选").clicked() {
                                        let old: std::collections::HashSet<String> =
                                            ext.hidden_items.iter().cloned().collect();
                                        ext.hidden_items = st
                                            .items
                                            .iter()
                                            .filter(|x| !old.contains(*x))
                                            .cloned()
                                            .collect();
                                        apply_label = Some("反选字段筛选".into());
                                    }
                                });
                                let q = st.search.trim().to_lowercase();
                                egui::ScrollArea::vertical()
                                    .id_salt("pgf_axis_scroll")
                                    .max_height(190.0)
                                    .show(ui, |ui| {
                                        for it in &st.items {
                                            if !q.is_empty() && !it.to_lowercase().contains(&q) {
                                                continue;
                                            }
                                            let mut vis = !ext.hidden_items.iter().any(|h| h == it);
                                            if ui.checkbox(&mut vis, it).changed() {
                                                if vis {
                                                    ext.hidden_items.retain(|h| h != it);
                                                } else {
                                                    ext.hidden_items.push(it.clone());
                                                }
                                                apply_label = Some("筛选透视表项目".into());
                                            }
                                        }
                                    });
                            }
                            PivotFilterTarget::PageFilter(fi) => {
                                let keep = &mut self.wb.pivots[pidx].spec.filters[fi].keep;
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut st.search)
                                            .hint_text("搜索筛选值…")
                                            .desired_width(166.0),
                                    );
                                    if ui.small_button("全部").clicked() {
                                        keep.clear();
                                        apply_label = Some("清除报表筛选".into());
                                    }
                                });
                                let q = st.search.trim().to_lowercase();
                                egui::ScrollArea::vertical()
                                    .id_salt("pgf_page_scroll")
                                    .max_height(210.0)
                                    .show(ui, |ui| {
                                        for it in &st.items {
                                            if !q.is_empty() && !it.to_lowercase().contains(&q) {
                                                continue;
                                            }
                                            let mut on = keep.is_empty() || keep.iter().any(|k| k == it);
                                            if ui.checkbox(&mut on, it).changed() {
                                                if keep.is_empty() {
                                                    *keep = st
                                                        .items
                                                        .iter()
                                                        .filter(|x| *x != it)
                                                        .cloned()
                                                        .collect();
                                                } else if on {
                                                    if !keep.iter().any(|k| k == it) {
                                                        keep.push(it.clone());
                                                    }
                                                    if keep.len() >= st.items.len() {
                                                        keep.clear();
                                                    }
                                                } else {
                                                    keep.retain(|k| k != it);
                                                }
                                                apply_label = Some("更新报表筛选".into());
                                            }
                                        }
                                    });
                            }
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("完成").clicked() {
                                    keep_open = false;
                                }
                            });
                        });
                    });
            });

        // 点到外部或按 Esc 关闭
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            keep_open = false;
        } else if ctx.input(|i| i.pointer.primary_pressed())
            && let Some(p) = ctx.input(|i| i.pointer.interact_pos())
            && !area.response.rect.contains(p)
        {
            keep_open = false;
        }

        if let Some(lbl) = apply_label {
            self.pivot_ui.pending_undo_pivots_before = Some(snap_before);
            self.recompute_pivot_by_id(st.pivot_id, &lbl, true);
        }
        if keep_open {
            self.pivot_ui.grid_filter = Some(st);
        }
    }

    // =========================================================================
    // 6. 现代化多字段卡片按钮式切片器（即点即刷 + 支持多切片器联动）
    // =========================================================================

    pub(crate) fn draw_pivot_slicer_picker(&mut self, ctx: &egui::Context) {
        let Some(pid) = self.pivot_ui.slicer_picker_pivot_id else {
            return;
        };
        let Some(pidx) = self.wb.pivots.iter().position(|p| p.id == pid) else {
            self.pivot_ui.slicer_picker_pivot_id = None;
            return;
        };
        let model = self.wb.pivots[pidx].clone();
        let profiles = self.profiles_for_pivot(&model);
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;

        ui_kit::window(ctx, "插入切片器", 320.0)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("勾选要创建切片器的字段（支持同时插入多个切片器联动筛选）：")
                        .weak()
                        .size(11.0),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for prof in &profiles {
                            let mut on = self.pivot_ui.slicer_picker_checked.contains(&prof.col);
                            if ui
                                .checkbox(&mut on, format!("{} ({} 项)", prof.name, prof.unique_count))
                                .changed()
                            {
                                if on {
                                    self.pivot_ui.slicer_picker_checked.push(prof.col);
                                } else {
                                    self.pivot_ui.slicer_picker_checked.retain(|&c| c != prof.col);
                                }
                            }
                        }
                    });
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
            let checked = std::mem::take(&mut self.pivot_ui.slicer_picker_checked);
            let model_mut = &mut self.wb.pivots[pidx];
            // 保留仍勾选的旧切片器位置，移除取消的，新增勾选的
            model_mut.options.slicers.retain(|s| checked.contains(&s.col));
            let mut offset = model_mut.options.slicers.len() as f32;
            for col in checked {
                if !model_mut.options.slicers.iter().any(|s| s.col == col) {
                    let title = profiles
                        .iter()
                        .find(|p| p.col == col)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| sheetcore::model::col_label(col));
                    model_mut.options.slicers.push(PivotSlicerSpec {
                        id: next_pivot_id(),
                        col,
                        title,
                        pos: [520.0 + offset * 28.0, 210.0 + offset * 24.0],
                        width: 224.0,
                        columns: 2,
                    });
                    offset += 1.0;
                }
            }
            self.wb.pivots[pidx].modified = true;
            self.pivot_ui.slicer_picker_pivot_id = None;
        } else if cancel || !open {
            self.pivot_ui.slicer_picker_pivot_id = None;
        }
    }

    pub(crate) fn draw_modern_pivot_slicers(&mut self, ctx: &egui::Context) {
        let sheet_idx = self.sheet_idx;
        let pal = theme::palette();
        let mut to_refresh: Option<(u64, String)> = None;
        let mut remove_slicer: Option<(u64, u64)> = None;

        for pidx in 0..self.wb.pivots.len() {
            if self.wb.pivots[pidx].target_sheet != sheet_idx
                || self.wb.pivots[pidx].options.slicers.is_empty()
            {
                continue;
            }
            let pid = self.wb.pivots[pidx].id;
            let slicers_snap = self.wb.pivots[pidx].options.slicers.clone();

            for (si, mut sl) in slicers_snap.into_iter().enumerate() {
                let items = self.distinct_for_col(&self.wb.pivots[pidx], sl.col, &GroupBy::None);
                // 读取当前筛选保留集（若已在 filters 里则联动 filters，否则自动挂入 filters）
                let existing_keep: Vec<String> = self.wb.pivots[pidx]
                    .spec
                    .filters
                    .iter()
                    .find(|f| f.col == sl.col)
                    .map(|f| f.keep.clone())
                    .unwrap_or_default();
                let is_filtered = !existing_keep.is_empty();
                let area_id = egui::Id::new(("modern_slicer", pid, sl.id));
                let mut pos = Pos2::new(sl.pos[0], sl.pos[1]);
                let mut new_keep: Option<Vec<String>> = None;

                egui::Area::new(area_id)
                    .order(egui::Order::Middle)
                    .fixed_pos(pos)
                    .constrain(true)
                    .show(ctx, |ui| {
                        egui::Frame::NONE
                            .fill(pal.card_bg)
                            .stroke(Stroke::new(1.0, pal.card_border))
                            .corner_radius(8.0)
                            .shadow(egui::epaint::Shadow {
                                offset: [0, 2],
                                blur: 8,
                                spread: 0,
                                color: pal.shadow_color(),
                            })
                            .inner_margin(egui::Margin::same(0))
                            .show(ui, |ui| {
                                ui.set_width(sl.width);
                                // 拖拽标题条
                                let (trect, tresp) = ui.allocate_exact_size(
                                    Vec2::new(sl.width, 28.0),
                                    Sense::click_and_drag(),
                                );
                                ui.painter().rect_filled(
                                    trect,
                                    egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 },
                                    pal.ribbon_bg,
                                );
                                ui.painter().text(
                                    Pos2::new(trect.left() + 10.0, trect.center().y),
                                    Align2::LEFT_CENTER,
                                    &sl.title,
                                    FontId::proportional(12.0),
                                    pal.text,
                                );
                                if tresp.dragged() {
                                    pos += tresp.drag_delta();
                                    sl.pos = [pos.x, pos.y];
                                }
                                // 右上角清除筛选 & 关闭按钮
                                let x_rect = Rect::from_center_size(
                                    Pos2::new(trect.right() - 14.0, trect.center().y),
                                    Vec2::splat(18.0),
                                );
                                if ui.interact(x_rect, area_id.with("x"), Sense::click()).clicked() {
                                    remove_slicer = Some((pid, sl.id));
                                }
                                ui.painter().text(
                                    x_rect.center(),
                                    Align2::CENTER_CENTER,
                                    "×",
                                    FontId::proportional(13.0),
                                    pal.text_weak,
                                );
                                if is_filtered {
                                    let clr_rect = Rect::from_center_size(
                                        Pos2::new(trect.right() - 42.0, trect.center().y),
                                        Vec2::new(30.0, 18.0),
                                    );
                                    if ui
                                        .interact(clr_rect, area_id.with("clr"), Sense::click())
                                        .on_hover_text("清除此切片器筛选")
                                        .clicked()
                                    {
                                        new_keep = Some(Vec::new());
                                    }
                                    ui.painter().text(
                                        clr_rect.center(),
                                        Align2::CENTER_CENTER,
                                        "清除",
                                        FontId::proportional(11.0),
                                        pal.accent,
                                    );
                                }

                                // 现代多列按钮网格（点选即刻联动刷新；Ctrl+点击多选）
                                egui::Frame::NONE
                                    .inner_margin(egui::Margin::same(8))
                                    .show(ui, |ui| {
                                        let cols_n = (sl.columns as usize).clamp(1, 3);
                                        let btn_w = ((sl.width - 16.0 - (cols_n as f32 - 1.0) * 5.0)
                                            / cols_n as f32)
                                            .max(60.0);
                                        let ctrl = ui.input(|i| i.modifiers.command);
                                        egui::ScrollArea::vertical()
                                            .id_salt(area_id.with("sc"))
                                            .max_height(196.0)
                                            .show(ui, |ui| {
                                                for chunk in items.chunks(cols_n) {
                                                    ui.horizontal(|ui| {
                                                        ui.spacing_mut().item_spacing.x = 5.0;
                                                        for it in chunk {
                                                            let active = existing_keep.is_empty()
                                                                || existing_keep.iter().any(|k| k == it);
                                                            let fill = if active {
                                                                pal.accent_tint
                                                            } else {
                                                                pal.page_bg
                                                            };
                                                            let stroke = if active {
                                                                Stroke::new(1.0, pal.accent)
                                                            } else {
                                                                Stroke::new(1.0, pal.card_border)
                                                            };
                                                            let fg = if active {
                                                                pal.accent
                                                            } else {
                                                                pal.text_weak
                                                            };
                                                            let btn = egui::Button::new(
                                                                egui::RichText::new(it)
                                                                    .size(12.0)
                                                                    .color(fg),
                                                            )
                                                            .fill(fill)
                                                            .stroke(stroke)
                                                            .corner_radius(4.0)
                                                            .min_size(Vec2::new(btn_w, 24.0));
                                                            if ui
                                                                .add(btn)
                                                                .on_hover_text("单击筛选；Ctrl+单击多选")
                                                                .clicked()
                                                            {
                                                                let mut next = existing_keep.clone();
                                                                if ctrl {
                                                                    if next.is_empty() {
                                                                        next = items
                                                                            .iter()
                                                                            .filter(|x| *x != it)
                                                                            .cloned()
                                                                            .collect();
                                                                    } else if active {
                                                                        next.retain(|k| k != it);
                                                                    } else {
                                                                        next.push(it.clone());
                                                                        if next.len() >= items.len() {
                                                                            next.clear();
                                                                        }
                                                                    }
                                                                } else if next.len() == 1 && next[0] == *it {
                                                                    // 再次单击唯一选中项 → 恢复全部
                                                                    next.clear();
                                                                } else {
                                                                    next = vec![it.clone()];
                                                                }
                                                                new_keep = Some(next);
                                                            }
                                                        }
                                                    });
                                                    ui.add_space(4.0);
                                                }
                                            });
                                    });
                            });
                    });

                let sl_col = sl.col;
                self.wb.pivots[pidx].options.slicers[si] = sl;
                if let Some(keep) = new_keep {
                    self.capture_pivot_undo_before();
                    if let Some(ff) = self.wb.pivots[pidx]
                        .spec
                        .filters
                        .iter_mut()
                        .find(|f| f.col == sl_col)
                    {
                        ff.keep = keep;
                    } else if !keep.is_empty() {
                        self.wb.pivots[pidx].spec.filters.push(FilterField { col: sl_col, keep });
                    }
                    to_refresh = Some((pid, "切片器筛选透视表".into()));
                }
            }
        }

        if let Some((pid, sid)) = remove_slicer
            && let Some(p) = self.wb.pivots.iter_mut().find(|p| p.id == pid)
        {
            p.options.slicers.retain(|s| s.id != sid);
        }
        if let Some((pid, lbl)) = to_refresh {
            self.recompute_pivot_by_id(pid, &lbl, true);
        }
    }
}

// =========================================================================
// 内部辅助函数：字段拖拽落点与撤销差分捕获
// =========================================================================

enum ChipAct {
    None,
    Click,
    Remove,
}

fn draw_zone_chip(
    ui: &mut egui::Ui,
    text: &str,
    accent_highlight: bool,
    drag_slot: &mut Option<PivotDragPayload>,
    source: PivotDragSource,
    col: u32,
    raw_name: &str,
) -> ChipAct {
    let pal = theme::palette();
    let mut out = ChipAct::None;
    let fill = if accent_highlight {
        pal.accent_tint
    } else {
        pal.card_bg
    };
    let stroke = Stroke::new(
        1.0,
        if accent_highlight {
            pal.accent
        } else {
            pal.card_border
        },
    );
    egui::Frame::NONE
        .fill(fill)
        .stroke(stroke)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.set_width((ui.available_width() - 2.0).max(60.0));
            ui.horizontal(|ui| {
                let lbl = ui.add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .size(11.0)
                            .color(if accent_highlight { pal.accent } else { pal.text }),
                    )
                    .truncate()
                    .sense(Sense::click_and_drag()),
                );
                if lbl.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if lbl.drag_started() {
                    *drag_slot = Some(PivotDragPayload {
                        source,
                        col,
                        label: raw_name.to_string(),
                    });
                } else if lbl.clicked() {
                    out = ChipAct::Click;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("×").size(11.0).color(pal.text_weak),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        out = ChipAct::Remove;
                    }
                });
            });
        });
    ui.add_space(3.0);
    out
}

fn remove_col_everywhere(model: &mut PivotTableModel, col: u32) {
    let mut keep_r = Vec::new();
    let mut keep_re = Vec::new();
    for (i, f) in std::mem::take(&mut model.spec.rows).into_iter().enumerate() {
        if f.col != col {
            keep_r.push(f);
            if let Some(e) = model.options.row_ext.get(i).cloned() {
                keep_re.push(e);
            }
        }
    }
    model.spec.rows = keep_r;
    model.options.row_ext = keep_re;

    let mut keep_c = Vec::new();
    let mut keep_ce = Vec::new();
    for (i, f) in std::mem::take(&mut model.spec.cols).into_iter().enumerate() {
        if f.col != col {
            keep_c.push(f);
            if let Some(e) = model.options.col_ext.get(i).cloned() {
                keep_ce.push(e);
            }
        }
    }
    model.spec.cols = keep_c;
    model.options.col_ext = keep_ce;

    let mut keep_v = Vec::new();
    let mut keep_ve = Vec::new();
    for (i, f) in std::mem::take(&mut model.spec.values).into_iter().enumerate() {
        if f.col != col {
            keep_v.push(f);
            if let Some(e) = model.options.val_ext.get(i).cloned() {
                keep_ve.push(e);
            }
        }
    }
    model.spec.values = keep_v;
    model.options.val_ext = keep_ve;

    model.spec.filters.retain(|f| f.col != col);
}

fn remove_from_zone(model: &mut PivotTableModel, zone: PivotZone, idx: usize) {
    match zone {
        PivotZone::Filter => {
            if idx < model.spec.filters.len() {
                model.spec.filters.remove(idx);
            }
        }
        PivotZone::Col => {
            if idx < model.spec.cols.len() {
                model.spec.cols.remove(idx);
                if idx < model.options.col_ext.len() {
                    model.options.col_ext.remove(idx);
                }
            }
        }
        PivotZone::Row => {
            if idx < model.spec.rows.len() {
                model.spec.rows.remove(idx);
                if idx < model.options.row_ext.len() {
                    model.options.row_ext.remove(idx);
                }
            }
        }
        PivotZone::Value => {
            if idx < model.spec.values.len() {
                model.spec.values.remove(idx);
                if idx < model.options.val_ext.len() {
                    model.options.val_ext.remove(idx);
                }
            }
        }
    }
}

fn apply_field_drop(model: &mut PivotTableModel, payload: &PivotDragPayload, target: PivotZone) {
    let col = payload.col;
    let mut preserved_group = GroupBy::None;
    let mut preserved_ext = AxisFieldExt::default();
    if let PivotDragSource::Zone(src_z, src_i) = payload.source {
        if src_z == target {
            // 同区拖动：将当前项移到末尾（便捷调整层次顺序）
            match target {
                PivotZone::Row if src_i + 1 < model.spec.rows.len() => {
                    let f = model.spec.rows.remove(src_i);
                    model.spec.rows.push(f);
                    if src_i < model.options.row_ext.len() {
                        let e = model.options.row_ext.remove(src_i);
                        model.options.row_ext.push(e);
                    }
                }
                PivotZone::Col if src_i + 1 < model.spec.cols.len() => {
                    let f = model.spec.cols.remove(src_i);
                    model.spec.cols.push(f);
                    if src_i < model.options.col_ext.len() {
                        let e = model.options.col_ext.remove(src_i);
                        model.options.col_ext.push(e);
                    }
                }
                PivotZone::Value if src_i + 1 < model.spec.values.len() => {
                    let f = model.spec.values.remove(src_i);
                    model.spec.values.push(f);
                    if src_i < model.options.val_ext.len() {
                        let e = model.options.val_ext.remove(src_i);
                        model.options.val_ext.push(e);
                    }
                }
                _ => {}
            }
            return;
        }
        match src_z {
            PivotZone::Row if src_i < model.spec.rows.len() => {
                preserved_group = model.spec.rows[src_i].group.clone();
                if let Some(e) = model.options.row_ext.get(src_i) {
                    preserved_ext = e.clone();
                }
            }
            PivotZone::Col if src_i < model.spec.cols.len() => {
                preserved_group = model.spec.cols[src_i].group.clone();
                if let Some(e) = model.options.col_ext.get(src_i) {
                    preserved_ext = e.clone();
                }
            }
            _ => {}
        }
        remove_from_zone(model, src_z, src_i);
    }

    match target {
        PivotZone::Filter => {
            if !model.spec.filters.iter().any(|f| f.col == col) {
                model.spec.filters.push(FilterField {
                    col,
                    keep: Vec::new(),
                });
            }
        }
        PivotZone::Col => {
            model.spec.cols.push(AxisField {
                col,
                group: preserved_group,
            });
            model.options.col_ext.push(preserved_ext);
        }
        PivotZone::Row => {
            model.spec.rows.push(AxisField {
                col,
                group: preserved_group,
            });
            model.options.row_ext.push(preserved_ext);
        }
        PivotZone::Value => {
            model.spec.values.push(ValueField {
                col,
                agg: Agg::Sum,
                label: None,
                show_as: ShowAs::Normal,
                base: None,
            });
            model.options.val_ext.push(ValueFieldExt::default());
        }
    }
}

fn union_rect(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> (u32, u32, u32, u32) {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

fn capture_rect_cells(
    sh: &Sheet,
    rect: (u32, u32, u32, u32),
) -> std::collections::HashMap<(u32, u32), (Cell, u16)> {
    let mut out = std::collections::HashMap::new();
    for r in rect.0..=rect.2 {
        for c in rect.1..=rect.3 {
            let cell = sh.get(r, c).map(|v| v.into_owned()).unwrap_or_default();
            let st = sh.style_of(r, c);
            if !cell.is_blank() || st != 0 {
                out.insert((r, c), (cell, st));
            }
        }
    }
    out
}

fn build_rect_deltas(
    sheet_idx: usize,
    before: &std::collections::HashMap<(u32, u32), (Cell, u16)>,
    after_sh: &Sheet,
    rect: (u32, u32, u32, u32),
) -> Vec<CellDelta> {
    let mut deltas = Vec::new();
    for r in rect.0..=rect.2 {
        for c in rect.1..=rect.3 {
            let (b_cell, b_st) = before
                .get(&(r, c))
                .cloned()
                .unwrap_or_else(|| (Cell::default(), 0));
            let a_cell = after_sh.get(r, c).map(|v| v.into_owned()).unwrap_or_default();
            let a_st = after_sh.style_of(r, c);
            if !b_cell.same_input(&a_cell) || b_st != a_st {
                deltas.push(CellDelta {
                    sheet: sheet_idx,
                    row: r,
                    col: c,
                    before: b_cell,
                    after: a_cell,
                    before_style: b_st,
                    after_style: a_st,
                });
            }
        }
    }
    deltas
}

fn col_custom_width(sh: &Sheet, c: u32) -> Option<f32> {
    let sz = sh.cols.size(c);
    let def = sh.cols.default_size();
    ((sz - def).abs() > 0.01).then_some(sz)
}

fn capture_col_widths(sh: &Sheet, c1: u32, c2: u32) -> Vec<(u32, Option<f32>)> {
    (c1..=c2).map(|c| (c, col_custom_width(sh, c))).collect()
}

fn build_col_width_deltas(
    before: &[(u32, Option<f32>)],
    after_sh: &Sheet,
    c1: u32,
    c2: u32,
) -> Vec<(u32, Option<f32>, Option<f32>)> {
    let mut out = Vec::new();
    let bmap: std::collections::HashMap<u32, Option<f32>> = before.iter().copied().collect();
    for c in c1..=c2 {
        let b = bmap.get(&c).copied().flatten();
        let a = col_custom_width(after_sh, c);
        if b != a {
            out.push((c, b, a));
        }
    }
    out
}

impl SheetApp {
    /// 调试/视觉验收用：在当前表写入一份真实财务销售明细分录表，方便立即打开向导或演示透视表。
    pub(crate) fn seed_pivot_demo_data(&mut self) {
        let sh = self.wb.sheet_mut(0);
        sh.name = "销售明细".into();
        let headers = ["区域", "城市", "产品线", "订单日期", "客户", "销售额", "利润", "数量"];
        for (c, h) in headers.iter().enumerate() {
            sh.set(0, c as u32, Cell { value: Value::Text((*h).into()), formula: None });
        }
        let rows: [(&str, &str, &str, &str, &str, f64, f64, f64); 18] = [
            ("华东", "上海", "企业版", "2026-01-15", "星辰科技", 185000.0, 52000.0, 12.0),
            ("华东", "上海", "云服务", "2026-02-18", "远航物流", 96000.0, 28500.0, 8.0),
            ("华东", "杭州", "企业版", "2026-01-22", "青禾电商", 142000.0, 41200.0, 10.0),
            ("华东", "杭州", "专业版", "2026-03-09", "西湖智造", 68000.0, -4500.0, 6.0),
            ("华东", "苏州", "云服务", "2026-04-11", "昆仑精工", 112000.0, 35600.0, 9.0),
            ("华东", "南京", "企业版", "2026-05-03", "金陵医药", 128000.0, 38900.0, 7.0),
            ("华北", "北京", "企业版", "2026-01-28", "北辰金融", 264000.0, 84000.0, 16.0),
            ("华北", "北京", "云服务", "2026-02-14", "燕京数科", 158000.0, 49200.0, 11.0),
            ("华北", "北京", "专业版", "2026-04-20", "长城制造", 82000.0, 19800.0, 5.0),
            ("华北", "天津", "云服务", "2026-03-19", "渤海港务", 74000.0, 18400.0, 6.0),
            ("华北", "青岛", "专业版", "2026-05-12", "海川机电", 59000.0, -2800.0, 4.0),
            ("华南", "深圳", "企业版", "2026-01-11", "鹏城芯片", 238000.0, 76500.0, 15.0),
            ("华南", "深圳", "云服务", "2026-03-25", "前海跨境", 174000.0, 56200.0, 13.0),
            ("华南", "广州", "企业版", "2026-02-09", "岭南商贸", 119000.0, 33400.0, 8.0),
            ("华南", "广州", "专业版", "2026-04-15", "珠江食品", 63000.0, 15200.0, 5.0),
            ("西南", "成都", "企业版", "2026-02-27", "天府生物", 136000.0, 40500.0, 9.0),
            ("西南", "成都", "云服务", "2026-05-08", "蓉城文创", 89000.0, 26100.0, 7.0),
            ("西南", "重庆", "专业版", "2026-03-30", "巴渝汽车", 71000.0, 16800.0, 6.0),
        ];
        for (i, (reg, city, prod, dt, cust, sales, profit, qty)) in rows.iter().enumerate() {
            let r = (i + 1) as u32;
            sh.set(r, 0, Cell { value: Value::Text((*reg).into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Text((*city).into()), formula: None });
            sh.set(r, 2, Cell { value: Value::Text((*prod).into()), formula: None });
            sh.set(r, 3, Cell { value: Value::Text((*dt).into()), formula: None });
            sh.set(r, 4, Cell { value: Value::Text((*cust).into()), formula: None });
            sh.set(r, 5, Cell { value: Value::Number(*sales), formula: None });
            sh.set(r, 6, Cell { value: Value::Number(*profit), formula: None });
            sh.set(r, 7, Cell { value: Value::Number(*qty), formula: None });
        }
        self.sheet_idx = 0;
        self.sel = (1, 0);
        self.sel_end = (1, 0);
        self.has_open_workbook = true;
    }

    /// 调试/视觉验收用：构造完整的现代化透视表场景（多级行、列、数据条、极值高亮、切片器、侧边栏、上下文功能区）。
    pub(crate) fn seed_pivot_demo_workbook(&mut self) {
        self.seed_pivot_demo_data();
        self.open_pivot_create_wizard(None);
        if let Some(mut wiz) = self.pivot_ui.create_wizard.take() {
            wiz.dest_new_sheet = true;
            wiz.selected_preset = Some(0);
            self.create_pivot_from_wizard(wiz);
        }
        if let Some(p) = self.wb.pivots.first_mut() {
            p.spec.rows = vec![AxisField::plain(0), AxisField::plain(1)];
            p.spec.cols = vec![AxisField::plain(2)];
            p.spec.filters = vec![FilterField { col: 3, keep: Vec::new() }];
            p.spec.values = vec![
                ValueField { col: 5, agg: Agg::Sum, label: Some("销售额".into()), show_as: ShowAs::Normal, base: None },
                ValueField { col: 6, agg: Agg::Sum, label: Some("利润".into()), show_as: ShowAs::Normal, base: None },
            ];
            p.ensure_ext_synced();
            p.options.row_ext[0].sort = FieldSortMode::ValueDesc(0);
            p.options.show_data_bars = true;
            p.options.highlight_outliers = true;
            p.options.banded_rows = true;
            p.options.style_theme = PivotStyleTheme::Blue;
            p.options.slicers.push(PivotSlicerSpec {
                id: next_pivot_id(),
                col: 2,
                title: "产品线".into(),
                pos: [760.0, 565.0],
                width: 240.0,
                columns: 3,
            });
            let pid = p.id;
            self.recompute_pivot_by_id(pid, "初始化演示透视表", false);
            if let Some(p2) = self.wb.pivots.first() {
                let (r1, c1, _, _) = p2.rect;
                self.sel = (r1 + 4, c1 + p2.last_meta.row_label_cols);
                self.sel_end = self.sel;
                self.pivot_ui.active_pivot_id = Some(pid);
                self.pivot_ui.sidebar_open = true;
                self.ribbon_tab = 103;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;

    fn new_test_app(ctx: &egui::Context) -> SheetApp {
        let mut app = SheetApp::new_with_context(ctx, None);
        app.show_start = false;
        app.has_open_workbook = true;
        app
    }

    #[test]
    fn pivot_overhaul_wizard_preset_styling_ribbon_and_undo_redo() {
        let ctx = egui::Context::default();
        let mut app = new_test_app(&ctx);
        app.seed_pivot_demo_data();

        // 先做一次普通编辑，确保创建透视表不会清空历史撤销栈
        app.sel = (1, 5);
        app.sel_end = (1, 5);
        app.begin_edit(Some("190000".into()));
        assert!(app.commit_edit(None));
        assert!(app.undo.can_undo(), "普通编辑应进入撤销栈");

        // 通过功能区 InsertPivot 打开智能向导
        app.ribbon_action("InsertPivot", &ctx);
        let wiz = app
            .pivot_ui
            .create_wizard
            .take()
            .expect("InsertPivot 应打开创建数据透视表向导");
        assert!(wiz.dest_new_sheet, "默认应放到新工作表");
        assert_eq!(wiz.source_rect, (0, 0, 18, 7));
        assert!(!wiz.presets.is_empty(), "应自动生成智能推荐预设");
        assert_eq!(wiz.profiles[0].kind, FieldKind::Text);
        assert_eq!(wiz.profiles[3].kind, FieldKind::Date);
        assert_eq!(wiz.profiles[5].kind, FieldKind::Numeric);

        // 确认创建
        app.create_pivot_from_wizard(wiz);
        assert_eq!(app.wb.sheets.len(), 2, "应新建透视表工作表");
        assert_eq!(app.wb.pivots.len(), 1);
        assert_eq!(app.ribbon_tab, 103, "创建后应自动激活「透视表分析」上下文页签");
        assert!(app.pivot_ui.sidebar_open, "右侧字段面板应自动展开");
        assert!(app.active_pivot().is_some());
        assert!(app.ctl_state("PivotFieldList").checked);
        assert!(app.ctl_state("PivotStyleBlue").checked);
        assert!(app.ctl_state("PivotLayoutTabular").checked);

        // 验证透视表右键菜单包含专属命令
        let ctx_rows = app.cell_ctx_menu_rows();
        assert!(
            ctx_rows.iter().any(|r| r.label == "刷新透视表")
                && ctx_rows.iter().any(|r| r.label == "查看明细（下钻）"),
            "透视表内右键菜单应前置专属操作"
        );

        // 验证应用了专业样式（非默认 style 0）与自适应列宽
        let p = app.wb.pivots[0].clone();
        let psh = app.wb.sheet(p.target_sheet);
        assert_ne!(psh.style_of(p.rect.0, p.rect.1), 0, "表头应套用透视表主题样式");
        assert!(psh.cols.size(p.rect.1) > 70.0, "应自动调整列宽");

        // 撤销创建透视表：应删除新建工作表并保留此前普通编辑的撤销历史
        assert!(app.apply_undo(false), "应支持撤销创建数据透视表");
        assert_eq!(app.wb.sheets.len(), 1, "撤销后新建透视表页应被移除");
        assert!(app.wb.pivots.is_empty(), "撤销后透视表列表应恢复为空");
        assert!(app.undo.can_undo(), "此前普通编辑的撤销历史不应被清空");

        // 重做创建透视表：应完整恢复工作表和透视表模型
        assert!(app.apply_undo(true), "应支持重做创建数据透视表");
        assert_eq!(app.wb.sheets.len(), 2);
        assert_eq!(app.wb.pivots.len(), 1);

        // 通过设计上下文命令切换为压缩形式 + 翡翠绿主题 + 极值高亮，并验证可撤销
        app.ribbon_action("PivotLayoutCompact", &ctx);
        app.ribbon_action("PivotStyleGreen", &ctx);
        app.ribbon_action("PivotToggleOutliers", &ctx);
        assert_eq!(
            app.wb.pivots[0].options.report_layout,
            ReportLayout::Compact
        );
        assert_eq!(
            app.wb.pivots[0].options.style_theme,
            PivotStyleTheme::Green
        );
        assert!(app.wb.pivots[0].options.highlight_outliers);
        assert!(app.apply_undo(false));
        assert!(!app.wb.pivots[0].options.highlight_outliers);
    }

    #[test]
    fn pivot_overhaul_drilldown_collapse_sort_topn_calc_and_show_as() {
        let ctx = egui::Context::default();
        let mut app = new_test_app(&ctx);
        app.seed_pivot_demo_workbook();
        assert_eq!(app.wb.pivots.len(), 1);
        let pid = app.wb.pivots[0].id;

        // 1) 多级行折叠与展开
        let rows_expanded = app.wb.pivots[0].rect.2 - app.wb.pivots[0].rect.0 + 1;
        let first_collapse = app.wb.pivots[0]
            .last_meta
            .collapse_buttons
            .first()
            .cloned()
            .expect("多级行透视表应生成 [+]/[-] 折叠按钮");
        app.toggle_pivot_collapse(pid, first_collapse.is_row, first_collapse.path.clone());
        let rows_collapsed = app.wb.pivots[0].rect.2 - app.wb.pivots[0].rect.0 + 1;
        assert!(
            rows_collapsed < rows_expanded,
            "折叠分组后透视表行数应减少（{rows_collapsed} < {rows_expanded}）"
        );
        app.ribbon_action("PivotExpandAll", &ctx);
        let rows_after_expand = app.wb.pivots[0].rect.2 - app.wb.pivots[0].rect.0 + 1;
        assert_eq!(rows_after_expand, rows_expanded, "展开全部应恢复所有行");

        // 2) 双击明细下钻（选中总计行数值格 -> 新建带筛选的明细工作表）
        let sheets_before = app.wb.sheets.len();
        let grand_row = app.wb.pivots[0]
            .last_meta
            .row_kinds
            .iter()
            .enumerate()
            .find_map(|(i, k)| (*k == PivotRowKind::GrandTotal).then_some(app.wb.pivots[0].rect.0 + i as u32))
            .expect("应有总计行");
        let val_col = app.wb.pivots[0].rect.1 + app.wb.pivots[0].last_meta.row_label_cols;
        assert!(
            app.try_drill_down_pivot_at(grand_row, val_col),
            "双击总计值格应触发明细下钻"
        );
        assert_eq!(app.wb.sheets.len(), sheets_before + 1, "下钻应新建明细表");
        let drill_sh = app.wb.sheet(app.sheet_idx);
        assert!(drill_sh.name.starts_with("明细"), "下钻工作表名应以明细开头");
        assert!(drill_sh.filter.is_some(), "下钻明细表应自动开启筛选");

        // 切回透视表，验证右键快捷汇总方式（非重复计数 / 中位数）与值显示方式（降序排名）
        let piv_sheet = app.wb.pivots[0].target_sheet;
        app.sheet_idx = piv_sheet;
        app.sel = (app.wb.pivots[0].rect.0 + 4, val_col);
        app.sel_end = app.sel;
        app.pivot_quick_set_agg(Agg::DistinctCount);
        assert_eq!(app.wb.pivots[0].spec.values[0].agg, Agg::DistinctCount);
        app.pivot_quick_set_agg(Agg::Median);
        assert_eq!(app.wb.pivots[0].spec.values[0].agg, Agg::Median);
        app.pivot_quick_set_show_as(ShowAs::RankDesc);
        assert_eq!(app.wb.pivots[0].spec.values[0].show_as, ShowAs::RankDesc);

        // 3) Top N = 2 筛选
        app.wb.pivots[0].options.row_ext[0].top_n = Some(TopNFilter {
            kind: TopNKind::Top,
            count: 2,
            value_idx: 0,
        });
        app.recompute_pivot_by_id(pid, "设置 Top 2", true);
        let collapse_after_top2 = app.wb.pivots[0]
            .last_meta
            .collapse_buttons
            .iter()
            .filter(|b| b.is_row)
            .count();
        assert_eq!(collapse_after_top2, 2, "Top 2 后顶层分组数应为 2");
    }

    #[test]
    fn pivot_overhaul_slicer_auto_expand_auto_refresh_and_xlsx_roundtrip() {
        let env = TestEnv::new("pivot_overhaul_roundtrip");
        let ctx = egui::Context::default();
        let mut app = new_test_app(&ctx);
        app.seed_pivot_demo_workbook();
        let pid = app.wb.pivots[0].id;

        // 1) 记录全部数据的销售额总计
        let sum_all: f64 = (1..=18)
            .filter_map(|r| match app.wb.sheet(0).value(r, 5) {
                Value::Number(v) => Some(v),
                _ => None,
            })
            .sum();

        // 2) 通过切片器 / 报表筛选仅保留「企业版」
        app.wb.pivots[0].spec.filters.push(FilterField {
            col: 2,
            keep: vec!["企业版".into()],
        });
        app.recompute_pivot_by_id(pid, "切片器筛选企业版", true);
        let matched_ent = app.wb.pivots[0].last_meta.matched_row_count;
        assert_eq!(matched_ent, 7, "示例数据中企业版应精确匹配 7 条记录");

        // 清除筛选并开启 auto_refresh + auto_expand_source，向源表末尾追加新行
        app.wb.pivots[0].spec.filters.clear();
        app.wb.pivots[0].options.auto_refresh = true;
        app.wb.pivots[0].options.auto_expand_source = true;
        app.recompute_pivot_by_id(pid, "清除筛选", false);

        {
            let src = app.wb.sheet_mut(0);
            src.set(19, 0, Cell { value: Value::Text("华东".into()), formula: None });
            src.set(19, 1, Cell { value: Value::Text("宁波".into()), formula: None });
            src.set(19, 2, Cell { value: Value::Text("企业版".into()), formula: None });
            src.set(19, 3, Cell { value: Value::Text("2026-06-01".into()), formula: None });
            src.set(19, 4, Cell { value: Value::Text("东海港口".into()), formula: None });
            src.set(19, 5, Cell { value: Value::Number(100000.0), formula: None });
            src.set(19, 6, Cell { value: Value::Number(30000.0), formula: None });
            src.set(19, 7, Cell { value: Value::Number(5.0), formula: None });
        }
        // 自动刷新巡检应自动扩展源区域到第 19 行并重算
        app.tick_pivot_auto_refresh();
        assert_eq!(app.wb.pivots[0].spec.source.2, 19, "应自动扩展源区域包含新增第 19 行");
        assert_eq!(app.wb.pivots[0].last_meta.matched_row_count, 19);
        let _ = sum_all;

        // 3) 保存为 .xlsx 并重新加载，验证透视表以活体模型恢复且可继续刷新
        let out_path = env.path("pivot_overhaul_roundtrip.xlsx");
        sheetcore::xlsx::save(&out_path, &app.wb).expect("保存含现代化透视表的 xlsx 应成功");
        let loaded = sheetcore::xlsx::load(&out_path).expect("重新加载 xlsx 应成功");
        assert_eq!(loaded.pivots.len(), 1, "重新加载后应恢复 1 个活体 PivotTableModel");
        assert_eq!(loaded.pivots[0].source_sheet, "销售明细");
        assert_eq!(loaded.pivots[0].spec.rows.len(), 2);
        assert_eq!(loaded.pivots[0].spec.values.len(), 2);
        assert!(!loaded.pivots[0].modified, "刚加载未修改的透视表 modified 应为 false");
    }
}
