//! 现代化数据透视表引擎（对象模型、全量聚合、排序/TopN、折叠展开、计算字段、
//! 13 种值显示方式、专业样式渲染、双击下钻明细、字段画像、智能推荐与源区自动扩展）。

use crate::model::{col_label, Cell, Sheet, Value};
use crate::pivot::{group_key, parse_date_ymd, Agg, GroupBy};
use crate::pivot_multi::{
    append_calc_item_expr, AxisField, AxisSort, ItemOp, PivotGrid, PivotLayout,
    PivotSpec2, ShowAs, ValueField,
};
use crate::sort_filter::FxHashSet;
use crate::style::{Border, BorderSide, CellStyle, StyleTable};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PIVOT_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_pivot_id() -> u64 {
    NEXT_PIVOT_ID.fetch_add(1, Ordering::Relaxed)
}

/// 报表布局（对齐 Excel：压缩 / 大纲 / 表格）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReportLayout {
    /// 压缩形式：所有行字段共用首列并逐层缩进
    Compact,
    /// 大纲形式：每层行字段占一列，父项单独占行
    Outline,
    /// 表格形式：每层行字段占一列，结构最清晰（默认）
    #[default]
    Tabular,
}

impl ReportLayout {
    pub fn label(self) -> &'static str {
        match self {
            ReportLayout::Compact => "压缩形式",
            ReportLayout::Outline => "大纲形式",
            ReportLayout::Tabular => "表格形式",
        }
    }
}

/// 分类汇总位置
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SubtotalPos {
    #[default]
    Bottom,
    Top,
    Off,
}

impl SubtotalPos {
    pub fn label(self) -> &'static str {
        match self {
            SubtotalPos::Bottom => "在组底部显示",
            SubtotalPos::Top => "在组顶部显示",
            SubtotalPos::Off => "不显示分类汇总",
        }
    }
}

/// 内置透视表配色主题（导出时映射到最接近的 Excel PivotStyle）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PivotStyleTheme {
    #[default]
    Blue,
    Green,
    Slate,
    Amber,
    Dark,
    Minimal,
}

impl PivotStyleTheme {
    pub const ALL: &'static [PivotStyleTheme] = &[
        PivotStyleTheme::Blue,
        PivotStyleTheme::Green,
        PivotStyleTheme::Slate,
        PivotStyleTheme::Amber,
        PivotStyleTheme::Dark,
        PivotStyleTheme::Minimal,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PivotStyleTheme::Blue => "经典蓝",
            PivotStyleTheme::Green => "商务绿",
            PivotStyleTheme::Slate => "优雅灰",
            PivotStyleTheme::Amber => "暖金橙",
            PivotStyleTheme::Dark => "深邃蓝",
            PivotStyleTheme::Minimal => "极简白",
        }
    }

    pub fn ooxml_name(self) -> &'static str {
        match self {
            PivotStyleTheme::Blue => "PivotStyleLight16",
            PivotStyleTheme::Green => "PivotStyleMedium7",
            PivotStyleTheme::Slate => "PivotStyleLight15",
            PivotStyleTheme::Amber => "PivotStyleMedium3",
            PivotStyleTheme::Dark => "PivotStyleDark2",
            PivotStyleTheme::Minimal => "PivotStyleLight1",
        }
    }

    pub fn from_ooxml_name(name: Option<&str>) -> Self {
        let Some(n) = name else {
            return PivotStyleTheme::Blue;
        };
        let low = n.to_ascii_lowercase();
        if low.contains("dark") {
            PivotStyleTheme::Dark
        } else if low.contains("7") || low.contains("14") || low.contains("21") || low.contains("green") {
            PivotStyleTheme::Green
        } else if low.contains("3") || low.contains("10") || low.contains("17") || low.contains("orange") {
            PivotStyleTheme::Amber
        } else if low.contains("15") || low.contains("8") || low.contains("gray") {
            PivotStyleTheme::Slate
        } else if low.ends_with("light1") || low.contains("none") {
            PivotStyleTheme::Minimal
        } else {
            PivotStyleTheme::Blue
        }
    }

    pub fn palette(self) -> PivotThemePalette {
        match self {
            PivotStyleTheme::Blue => PivotThemePalette {
                header_bg: [217, 229, 247],
                header_fg: [23, 54, 93],
                subtotal_bg: [238, 244, 252],
                subtotal_fg: [23, 54, 93],
                grand_bg: [217, 229, 247],
                grand_fg: [15, 36, 62],
                banded_bg: [247, 250, 254],
                border: [155, 187, 227],
                bar_color: [79, 129, 189],
                filter_bg: [241, 245, 251],
            },
            PivotStyleTheme::Green => PivotThemePalette {
                header_bg: [216, 238, 226],
                header_fg: [22, 82, 48],
                subtotal_bg: [237, 247, 241],
                subtotal_fg: [22, 82, 48],
                grand_bg: [216, 238, 226],
                grand_fg: [16, 62, 36],
                banded_bg: [246, 251, 248],
                border: [148, 206, 173],
                bar_color: [33, 115, 70],
                filter_bg: [240, 249, 244],
            },
            PivotStyleTheme::Slate => PivotThemePalette {
                header_bg: [226, 232, 240],
                header_fg: [30, 41, 59],
                subtotal_bg: [241, 245, 249],
                subtotal_fg: [30, 41, 59],
                grand_bg: [226, 232, 240],
                grand_fg: [15, 23, 42],
                banded_bg: [248, 250, 252],
                border: [176, 189, 205],
                bar_color: [100, 116, 139],
                filter_bg: [241, 245, 249],
            },
            PivotStyleTheme::Amber => PivotThemePalette {
                header_bg: [252, 232, 204],
                header_fg: [120, 53, 15],
                subtotal_bg: [254, 244, 230],
                subtotal_fg: [120, 53, 15],
                grand_bg: [252, 232, 204],
                grand_fg: [92, 39, 10],
                banded_bg: [255, 250, 243],
                border: [236, 189, 128],
                bar_color: [217, 119, 6],
                filter_bg: [254, 247, 237],
            },
            PivotStyleTheme::Dark => PivotThemePalette {
                header_bg: [31, 69, 138],
                header_fg: [255, 255, 255],
                subtotal_bg: [226, 234, 247],
                subtotal_fg: [23, 45, 92],
                grand_bg: [31, 69, 138],
                grand_fg: [255, 255, 255],
                banded_bg: [245, 248, 253],
                border: [115, 148, 204],
                bar_color: [59, 130, 246],
                filter_bg: [235, 241, 250],
            },
            PivotStyleTheme::Minimal => PivotThemePalette {
                header_bg: [248, 249, 251],
                header_fg: [30, 41, 59],
                subtotal_bg: [252, 252, 253],
                subtotal_fg: [30, 41, 59],
                grand_bg: [245, 247, 250],
                grand_fg: [15, 23, 42],
                banded_bg: [252, 253, 254],
                border: [203, 213, 225],
                bar_color: [71, 85, 105],
                filter_bg: [248, 250, 252],
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PivotThemePalette {
    pub header_bg: [u8; 3],
    pub header_fg: [u8; 3],
    pub subtotal_bg: [u8; 3],
    pub subtotal_fg: [u8; 3],
    pub grand_bg: [u8; 3],
    pub grand_fg: [u8; 3],
    pub banded_bg: [u8; 3],
    pub border: [u8; 3],
    pub bar_color: [u8; 3],
    pub filter_bg: [u8; 3],
}

/// 字段排序方式
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FieldSortMode {
    #[default]
    PinyinAsc,
    PinyinDesc,
    /// 按第 N 个值字段汇总降序（最常用排行榜）
    ValueDesc(usize),
    /// 按第 N 个值字段汇总升序
    ValueAsc(usize),
    /// 源数据出现顺序
    Source,
}

impl FieldSortMode {
    pub fn label(self) -> &'static str {
        match self {
            FieldSortMode::PinyinAsc => "标签升序 (A→Z)",
            FieldSortMode::PinyinDesc => "标签降序 (Z→A)",
            FieldSortMode::ValueDesc(_) => "按值降序",
            FieldSortMode::ValueAsc(_) => "按值升序",
            FieldSortMode::Source => "数据源顺序",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopNKind {
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopNFilter {
    pub kind: TopNKind,
    pub count: usize,
    pub value_idx: usize,
}

/// 行/列轴字段扩展配置（独立于 AxisField，保证既有 137 处 struct 字面量零破坏）
#[derive(Clone, Debug, PartialEq)]
pub struct AxisFieldExt {
    pub hidden_items: Vec<String>,
    pub sort: FieldSortMode,
    pub top_n: Option<TopNFilter>,
    pub show_subtotal: bool,
}

impl Default for AxisFieldExt {
    fn default() -> Self {
        Self {
            hidden_items: Vec::new(),
            sort: FieldSortMode::PinyinAsc,
            top_n: None,
            show_subtotal: true,
        }
    }
}

/// 值字段扩展配置
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ValueFieldExt {
    /// 自定义数字格式（None 时按 ShowAs / Agg 自动推断）
    pub num_format: Option<String>,
    /// 是否为该值列开启格内微型数据条
    pub data_bar: bool,
    pub bar_color: Option<[u8; 3]>,
}

/// 绑定到透视表的现代化切片器定义
#[derive(Clone, Debug, PartialEq)]
pub struct PivotSlicerSpec {
    pub id: u64,
    pub col: u32,
    pub title: String,
    pub pos: [f32; 2],
    pub width: f32,
    pub columns: u8,
}

/// 完整透视表选项集
#[derive(Clone, Debug, PartialEq)]
pub struct PivotTableOptions {
    pub report_layout: ReportLayout,
    pub repeat_labels: bool,
    pub row_grand_totals: bool,
    pub col_grand_totals: bool,
    pub subtotal_pos: SubtotalPos,
    pub empty_cell_text: String,
    pub error_cell_text: Option<String>,
    pub style_theme: PivotStyleTheme,
    pub banded_rows: bool,
    pub banded_cols: bool,
    pub autofit_cols: bool,
    pub show_data_bars: bool,
    pub highlight_outliers: bool,
    pub auto_expand_source: bool,
    pub auto_refresh: bool,
    pub defer_layout_update: bool,
    pub row_ext: Vec<AxisFieldExt>,
    pub col_ext: Vec<AxisFieldExt>,
    pub val_ext: Vec<ValueFieldExt>,
    pub collapsed_rows: Vec<Vec<String>>,
    pub collapsed_cols: Vec<Vec<String>>,
    pub calc_fields: Vec<(String, String)>,
    pub calc_items: Vec<(String, String)>,
    pub slicers: Vec<PivotSlicerSpec>,
}

impl Default for PivotTableOptions {
    fn default() -> Self {
        Self {
            report_layout: ReportLayout::Tabular,
            repeat_labels: false,
            row_grand_totals: true,
            col_grand_totals: true,
            subtotal_pos: SubtotalPos::Bottom,
            empty_cell_text: String::new(),
            error_cell_text: None,
            style_theme: PivotStyleTheme::Blue,
            banded_rows: true,
            banded_cols: false,
            autofit_cols: true,
            show_data_bars: false,
            highlight_outliers: false,
            auto_expand_source: true,
            auto_refresh: false,
            defer_layout_update: false,
            row_ext: Vec::new(),
            col_ext: Vec::new(),
            val_ext: Vec::new(),
            collapsed_rows: Vec::new(),
            collapsed_cols: Vec::new(),
            calc_fields: Vec::new(),
            calc_items: Vec::new(),
            slicers: Vec::new(),
        }
    }
}

/// 透视表行语义类型（用于精准着色、数据条与下钻）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PivotRowKind {
    PageFilter,
    Blank,
    Header,
    Detail,
    Subtotal(usize),
    CalcItem,
    GrandTotal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PivotFilterTarget {
    PageFilter(usize),
    RowField(usize),
    ColField(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PivotFilterButton {
    pub row: u32,
    pub col: u32,
    pub target: PivotFilterTarget,
    pub active: bool,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PivotCollapseButton {
    pub row: u32,
    pub col: u32,
    pub is_row: bool,
    pub path: Vec<String>,
    pub collapsed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrillFilter {
    pub row_path: Vec<String>,
    pub col_path: Vec<String>,
    pub is_grand_row: bool,
    pub is_grand_col: bool,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValueColStats {
    pub col: u32,
    pub value_idx: usize,
    pub min: f64,
    pub max: f64,
    pub max_abs: f64,
    pub p90: f64,
    pub data_bar: bool,
    pub bar_color: [u8; 3],
    pub highlight_outliers: bool,
}

/// 透视表渲染与交互元数据（不落盘，每次计算/加载后生成）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PivotGridMeta {
    pub anchor: (u32, u32),
    pub rect: (u32, u32, u32, u32),
    pub page_filter_rows: u32,
    pub header_rows: u32,
    pub row_label_cols: u32,
    pub row_kinds: Vec<PivotRowKind>,
    pub row_indents: Vec<u8>,
    pub row_paths_by_grid_row: Vec<Option<Vec<String>>>,
    pub col_paths_by_grid_col: Vec<Option<Vec<String>>>,
    pub col_is_grand_total: Vec<bool>,
    pub col_is_subtotal: Vec<bool>,
    pub col_value_idx: Vec<Option<usize>>,
    pub filter_buttons: Vec<PivotFilterButton>,
    pub collapse_buttons: Vec<PivotCollapseButton>,
    pub value_col_stats: Vec<ValueColStats>,
    pub source_row_count: usize,
    pub matched_row_count: usize,
}

impl PivotGridMeta {
    pub fn contains(&self, r: u32, c: u32) -> bool {
        r >= self.rect.0 && r <= self.rect.2 && c >= self.rect.1 && c <= self.rect.3
    }

    pub fn row_kind(&self, r: u32) -> Option<PivotRowKind> {
        let rel = r.checked_sub(self.anchor.0)? as usize;
        self.row_kinds.get(rel).copied()
    }

    /// 针对任意透视表数据格构建下钻明细条件；点击表头或页筛选返回 None。
    pub fn drill_filter_at(&self, r: u32, c: u32) -> Option<DrillFilter> {
        if !self.contains(r, c) {
            return None;
        }
        let rel_r = (r - self.anchor.0) as usize;
        let rel_c = (c - self.anchor.1) as usize;
        let rk = *self.row_kinds.get(rel_r)?;
        if !matches!(
            rk,
            PivotRowKind::Detail | PivotRowKind::Subtotal(_) | PivotRowKind::GrandTotal
        ) {
            return None;
        }
        // 只对值列生效（行标签列不触发下钻）
        if rel_c < self.row_label_cols as usize {
            return None;
        }
        let is_grand_row = matches!(rk, PivotRowKind::GrandTotal);
        let is_grand_col = self.col_is_grand_total.get(rel_c).copied().unwrap_or(false);
        let row_path = self
            .row_paths_by_grid_row
            .get(rel_r)
            .and_then(|o| o.clone())
            .unwrap_or_default();
        let col_path = self
            .col_paths_by_grid_col
            .get(rel_c)
            .and_then(|o| o.clone())
            .unwrap_or_default();
        let mut parts: Vec<String> = Vec::new();
        if !row_path.is_empty() {
            parts.push(row_path.join("-"));
        } else if is_grand_row {
            parts.push("总计".into());
        }
        if !col_path.is_empty() {
            parts.push(col_path.join("-"));
        }
        let label = if parts.is_empty() {
            "明细".into()
        } else {
            parts.join("_")
        };
        Some(DrillFilter {
            row_path,
            col_path,
            is_grand_row,
            is_grand_col,
            label,
        })
    }

    pub fn value_stats_for_col(&self, c: u32) -> Option<&ValueColStats> {
        self.value_col_stats.iter().find(|s| s.col == c)
    }
}

/// 一等公民工作簿级透视表对象（同表/跨表多透视共存、持久化、撤销、实时刷新）
#[derive(Clone, Debug, PartialEq)]
pub struct PivotTableModel {
    pub id: u64,
    pub name: String,
    pub source_sheet: String,
    pub source_sheet_id: Option<u64>,
    pub target_sheet: usize,
    pub anchor: (u32, u32),
    pub rect: (u32, u32, u32, u32),
    pub spec: PivotSpec2,
    pub options: PivotTableOptions,
    /// 最近一次计算时源表的版本号（用于 O(1) 检测源数据是否变动）
    pub last_source_rev: u64,
    /// 若来自打开的 `.xlsx` 部件（如 `xl/pivotTables/pivotTable1.xml`），记录路径
    pub imported_part: Option<String>,
    /// 导入的透视表若从未被用户改动，保存时保留原 XML；改动后重写
    pub modified: bool,
    pub last_meta: PivotGridMeta,
}

impl PivotTableModel {
    pub fn new(
        name: String,
        source_sheet: String,
        source_sheet_id: Option<u64>,
        target_sheet: usize,
        anchor: (u32, u32),
        spec: PivotSpec2,
    ) -> Self {
        let mut m = Self {
            id: next_pivot_id(),
            name,
            source_sheet,
            source_sheet_id,
            target_sheet,
            anchor,
            rect: (anchor.0, anchor.1, anchor.0, anchor.1),
            spec,
            options: PivotTableOptions::default(),
            last_source_rev: 0,
            imported_part: None,
            modified: true,
            last_meta: PivotGridMeta::default(),
        };
        m.ensure_ext_synced();
        m
    }

    pub fn ensure_ext_synced(&mut self) {
        self.options
            .row_ext
            .resize_with(self.spec.rows.len(), AxisFieldExt::default);
        self.options
            .col_ext
            .resize_with(self.spec.cols.len(), AxisFieldExt::default);
        self.options
            .val_ext
            .resize_with(self.spec.values.len(), ValueFieldExt::default);
        // 同步 legacy PivotLayout 字段供 OOXML 写入器使用
        self.spec.layout.tabular = self.options.report_layout == ReportLayout::Tabular;
        self.spec.layout.subtotals = self.options.subtotal_pos != SubtotalPos::Off;
        self.spec.layout.grand_totals =
            self.options.row_grand_totals || self.options.col_grand_totals;
        self.spec.layout.sort = match self.options.row_ext.first().map(|e| e.sort) {
            Some(FieldSortMode::Source) => AxisSort::Source,
            _ => AxisSort::Pinyin,
        };
    }

    pub fn contains_cell(&self, sheet_idx: usize, r: u32, c: u32) -> bool {
        self.target_sheet == sheet_idx
            && r >= self.rect.0
            && r <= self.rect.2
            && c >= self.rect.1
            && c <= self.rect.3
    }
}

/// 透视计算完整产出（可直接写入网格，也可交给 OOXML 写入器）
#[derive(Clone, Debug)]
pub struct ComputedPivot {
    pub grid: PivotGrid,
    pub meta: PivotGridMeta,
    /// 供 OOXML 写入的兼容网格（含展开后的平铺视图）
    pub row_kinds_data: Vec<PivotRowKind>,
    pub row_indents_data: Vec<u8>,
    pub value_formats: Vec<Option<String>>,
}

// =========================================================================
// 1. 增强累加器（支持 13 种汇总，Welford 数值稳定方差 + 精准非重复计数与中位数）
// =========================================================================

#[derive(Clone, Debug)]
struct RichAcc {
    count_nonempty: u64,
    numeric: u64,
    sum: f64,
    mean: f64,
    m2: f64,
    min: Option<f64>,
    max: Option<f64>,
    product: f64,
    has_product: bool,
    distinct: Option<FxHashSet<String>>,
    samples: Option<Vec<f64>>,
}

impl RichAcc {
    fn new(need_distinct: bool, need_median: bool) -> Self {
        Self {
            count_nonempty: 0,
            numeric: 0,
            sum: 0.0,
            mean: 0.0,
            m2: 0.0,
            min: None,
            max: None,
            product: 1.0,
            has_product: false,
            distinct: need_distinct.then(Default::default),
            samples: need_median.then(Vec::new),
        }
    }

    #[inline]
    fn push_num(&mut self, n: f64) {
        self.count_nonempty += 1;
        self.numeric += 1;
        self.sum += n;
        let delta = n - self.mean;
        self.mean += delta / self.numeric as f64;
        let delta2 = n - self.mean;
        self.m2 += delta * delta2;
        self.min = Some(self.min.map_or(n, |m| m.min(n)));
        self.max = Some(self.max.map_or(n, |m| m.max(n)));
        if self.has_product {
            self.product *= n;
        } else {
            self.product = n;
            self.has_product = true;
        }
        if let Some(d) = self.distinct.as_mut() {
            d.insert(crate::model::format_number(n));
        }
        if let Some(s) = self.samples.as_mut() {
            s.push(n);
        }
    }

    fn push(&mut self, v: Option<&Value>) {
        let Some(v) = v else { return };
        match v {
            Value::Empty => {}
            Value::Number(n) => self.push_num(*n),
            other => {
                other.with_display_str(|t| {
                    if t.is_empty() {
                        return;
                    }
                    if let Ok(n) = t.trim().parse::<f64>() {
                        self.push_num(n);
                    } else {
                        self.count_nonempty += 1;
                        if let Some(d) = self.distinct.as_mut() {
                            d.insert(t.to_string());
                        }
                    }
                });
            }
        }
    }

    fn merge(&mut self, o: &RichAcc) {
        if o.count_nonempty == 0 && o.numeric == 0 {
            return;
        }
        self.count_nonempty += o.count_nonempty;
        if o.numeric > 0 {
            let n_a = self.numeric as f64;
            let n_b = o.numeric as f64;
            let n = n_a + n_b;
            let delta = o.mean - self.mean;
            self.mean = (self.mean * n_a + o.mean * n_b) / n;
            self.m2 = self.m2 + o.m2 + delta * delta * (n_a * n_b / n);
            self.numeric += o.numeric;
            self.sum += o.sum;
            self.min = match (self.min, o.min) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            self.max = match (self.max, o.max) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            if o.has_product {
                if self.has_product {
                    self.product *= o.product;
                } else {
                    self.product = o.product;
                    self.has_product = true;
                }
            }
        }
        if let (Some(dst), Some(src)) = (self.distinct.as_mut(), o.distinct.as_ref()) {
            for k in src {
                dst.insert(k.clone());
            }
        }
        if let (Some(dst), Some(src)) = (self.samples.as_mut(), o.samples.as_ref()) {
            dst.extend_from_slice(src);
        }
    }

    fn finish(&self, agg: Agg) -> Value {
        match agg {
            Agg::Count => {
                if self.count_nonempty == 0 {
                    Value::Empty
                } else {
                    Value::Number(self.count_nonempty as f64)
                }
            }
            Agg::CountNums => {
                if self.numeric == 0 {
                    Value::Empty
                } else {
                    Value::Number(self.numeric as f64)
                }
            }
            Agg::DistinctCount => {
                let n = self.distinct.as_ref().map(|s| s.len()).unwrap_or(0);
                if n == 0 {
                    Value::Empty
                } else {
                    Value::Number(n as f64)
                }
            }
            Agg::Sum => {
                if self.numeric == 0 {
                    Value::Empty
                } else {
                    Value::Number(self.sum)
                }
            }
            Agg::Avg => {
                if self.numeric == 0 {
                    Value::Text("#DIV/0!".into())
                } else {
                    Value::Number(self.sum / self.numeric as f64)
                }
            }
            Agg::Max => self.max.map(Value::Number).unwrap_or(Value::Empty),
            Agg::Min => self.min.map(Value::Number).unwrap_or(Value::Empty),
            Agg::Product => {
                if self.has_product {
                    Value::Number(self.product)
                } else {
                    Value::Empty
                }
            }
            Agg::Var => {
                if self.numeric < 2 {
                    Value::Text("#DIV/0!".into())
                } else {
                    Value::Number((self.m2 / (self.numeric - 1) as f64).max(0.0))
                }
            }
            Agg::VarP => {
                if self.numeric == 0 {
                    Value::Text("#DIV/0!".into())
                } else {
                    Value::Number((self.m2 / self.numeric as f64).max(0.0))
                }
            }
            Agg::StdDev => {
                if self.numeric < 2 {
                    Value::Text("#DIV/0!".into())
                } else {
                    Value::Number((self.m2 / (self.numeric - 1) as f64).max(0.0).sqrt())
                }
            }
            Agg::StdDevP => {
                if self.numeric == 0 {
                    Value::Text("#DIV/0!".into())
                } else {
                    Value::Number((self.m2 / self.numeric as f64).max(0.0).sqrt())
                }
            }
            Agg::Median => {
                let Some(s) = self.samples.as_ref() else {
                    return Value::Empty;
                };
                if s.is_empty() {
                    return Value::Empty;
                }
                let mut sorted = s.clone();
                sorted.sort_by(|a, b| a.total_cmp(b));
                let n = sorted.len();
                let med = if n % 2 == 1 {
                    sorted[n / 2]
                } else {
                    (sorted[n / 2 - 1] + sorted[n / 2]) * 0.5
                };
                Value::Number(med)
            }
        }
    }
}

// =========================================================================
// 2. 计算字段四则表达式解析器（按 Excel 语义基于分组求和后的各源列求值）
// =========================================================================

#[derive(Clone, Debug)]
enum CalcExpr {
    Num(f64),
    Field(usize), // index into source header columns (0..=c2-c1)
    Neg(Box<CalcExpr>),
    Bin(char, Box<CalcExpr>, Box<CalcExpr>),
}

fn parse_calc_formula(expr: &str, col_names: &[String]) -> Option<CalcExpr> {
    let s = expr.trim().trim_start_matches('=');
    let mut p = CalcParser {
        src: s,
        pos: 0,
        col_names,
    };
    let node = p.parse_add()?;
    p.skip_ws();
    if p.pos == p.src.len() {
        Some(node)
    } else {
        None
    }
}

struct CalcParser<'a> {
    src: &'a str,
    pos: usize,
    col_names: &'a [String],
}

impl<'a> CalcParser<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn skip_ws(&mut self) {
        while let Some(ch) = self.rest().chars().next() {
            if ch.is_whitespace() {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    fn parse_add(&mut self) -> Option<CalcExpr> {
        let mut left = self.parse_mul()?;
        loop {
            self.skip_ws();
            let ch = match self.rest().chars().next() {
                Some(c @ ('+' | '-')) => c,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_mul()?;
            left = CalcExpr::Bin(ch, Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn parse_mul(&mut self) -> Option<CalcExpr> {
        let mut left = self.parse_unary()?;
        loop {
            self.skip_ws();
            let ch = match self.rest().chars().next() {
                Some(c @ ('*' | '/')) => c,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_unary()?;
            left = CalcExpr::Bin(ch, Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn parse_unary(&mut self) -> Option<CalcExpr> {
        self.skip_ws();
        if self.rest().starts_with('-') {
            self.pos += 1;
            return Some(CalcExpr::Neg(Box::new(self.parse_unary()?)));
        }
        if self.rest().starts_with('+') {
            self.pos += 1;
            return self.parse_unary();
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Option<CalcExpr> {
        self.skip_ws();
        let rest = self.rest();
        if rest.starts_with('(') {
            self.pos += 1;
            let inner = self.parse_add()?;
            self.skip_ws();
            if !self.rest().starts_with(')') {
                return None;
            }
            self.pos += 1;
            return Some(inner);
        }
        // 引号包裹的字段名 `'销售额'` 或 `[销售额]`
        if let Some(close) = rest.chars().next().and_then(|c| match c {
            '\'' => Some('\''),
            '[' => Some(']'),
            _ => None,
        }) {
            self.pos += 1;
            let start = self.pos;
            while let Some(ch) = self.rest().chars().next() {
                if ch == close {
                    let name = self.src[start..self.pos].trim();
                    self.pos += ch.len_utf8();
                    let idx = self.find_col(name)?;
                    return Some(CalcExpr::Field(idx));
                }
                self.pos += ch.len_utf8();
            }
            return None;
        }
        // 数字字面量
        if rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '.')
        {
            let start = self.pos;
            let mut dot = false;
            while let Some(ch) = self.rest().chars().next() {
                if ch.is_ascii_digit() {
                    self.pos += 1;
                } else if ch == '.' && !dot {
                    dot = true;
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let num = self.src[start..self.pos].parse::<f64>().ok()?;
            return Some(CalcExpr::Num(num));
        }
        // 裸字段名（到运算符或括号为止）
        let start = self.pos;
        while let Some(ch) = self.rest().chars().next() {
            if matches!(ch, '+' | '-' | '*' | '/' | '(' | ')') {
                break;
            }
            self.pos += ch.len_utf8();
        }
        let name = self.src[start..self.pos].trim();
        if name.is_empty() {
            return None;
        }
        let idx = self.find_col(name)?;
        Some(CalcExpr::Field(idx))
    }

    fn find_col(&self, name: &str) -> Option<usize> {
        self.col_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
    }
}

fn eval_calc_expr(expr: &CalcExpr, sums: &[f64]) -> Option<f64> {
    match expr {
        CalcExpr::Num(n) => Some(*n),
        CalcExpr::Field(idx) => Some(*sums.get(*idx).unwrap_or(&0.0)),
        CalcExpr::Neg(e) => eval_calc_expr(e, sums).map(|v| -v),
        CalcExpr::Bin(op, a, b) => {
            let x = eval_calc_expr(a, sums)?;
            let y = eval_calc_expr(b, sums)?;
            match op {
                '+' => Some(x + y),
                '-' => Some(x - y),
                '*' => Some(x * y),
                '/' => {
                    if y.abs() <= f64::EPSILON {
                        None
                    } else {
                        Some(x / y)
                    }
                }
                _ => None,
            }
        }
    }
}

/// 解析「名称=项1+项2-项3」计算项表达式
pub fn parse_calc_item_text(raw: &str) -> Option<(String, Vec<(ItemOp, String)>)> {
    let (name, rhs) = raw.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut terms: Vec<(ItemOp, String)> = Vec::new();
    let mut cur_op = ItemOp::Add;
    let mut buf = String::new();
    for ch in rhs.chars() {
        let next_op = match ch {
            '+' => Some(ItemOp::Add),
            '-' => Some(ItemOp::Sub),
            '*' => Some(ItemOp::Mul),
            '/' => Some(ItemOp::Div),
            _ => None,
        };
        if let Some(op) = next_op {
            let t = buf.trim();
            if !t.is_empty() {
                terms.push((cur_op, t.to_string()));
                buf.clear();
            }
            cur_op = op;
        } else {
            buf.push(ch);
        }
    }
    let t = buf.trim();
    if !t.is_empty() {
        terms.push((cur_op, t.to_string()));
    }
    (!terms.is_empty()).then(|| (name.to_string(), terms))
}

// =========================================================================
// 3. 源数据字段画像、去重值、智能推荐与源区自动扩展（P2）
// =========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Numeric,
    Date,
}

impl FieldKind {
    pub fn badge(self) -> &'static str {
        match self {
            FieldKind::Text => "Abc",
            FieldKind::Numeric => "123",
            FieldKind::Date => "日期",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FieldProfile {
    pub col: u32,
    pub name: String,
    pub kind: FieldKind,
    pub non_empty: usize,
    pub total_rows: usize,
    pub unique_count: usize,
    pub min_num: Option<f64>,
    pub max_num: Option<f64>,
    pub samples: Vec<String>,
}

fn header_name(sheet: &Sheet, r: u32, c: u32) -> String {
    sheet
        .get(r, c)
        .map(|x| x.value.display())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| col_label(c))
}

/// 扫描源表表头与数据列（大表采前 4000 行即可获得精准画像），供侧栏悬停提示与智能推荐使用。
pub fn profile_source_fields(
    sheet: &Sheet,
    styles: &StyleTable,
    source: (u32, u32, u32, u32),
) -> Vec<FieldProfile> {
    let (r1, c1, r2, c2) = source;
    let first = r1.saturating_add(1);
    let total_rows = if r2 >= first {
        (r2 - first + 1) as usize
    } else {
        0
    };
    let sample_end = r2.min(first.saturating_add(3999));
    let mut out = Vec::with_capacity((c2.saturating_sub(c1) + 1) as usize);
    for c in c1..=c2 {
        let name = header_name(sheet, r1, c);
        let mut non_empty = 0usize;
        let mut num_cnt = 0usize;
        let mut date_cnt = 0usize;
        let mut text_cnt = 0usize;
        let mut uniq: FxHashSet<String> = Default::default();
        let mut samples: Vec<String> = Vec::new();
        let mut min_num: Option<f64> = None;
        let mut max_num: Option<f64> = None;
        if first <= sample_end {
            for r in first..=sample_end {
                let v = sheet.value_ref(r, c);
                if matches!(v.as_ref(), Value::Empty) {
                    continue;
                }
                let st_id = sheet.style_of(r, c);
                let fmt = &styles.get(st_id).format;
                let fmt_is_date = !fmt.is_empty() && crate::style::is_date_format(fmt);
                match v.as_ref() {
                    Value::Number(n) => {
                        non_empty += 1;
                        if fmt_is_date && *n >= 1.0 && *n < 2_958_466.0 {
                            date_cnt += 1;
                            let d = group_key(v.as_ref(), &GroupBy::Day);
                            if uniq.len() < 1000 {
                                uniq.insert(d.clone());
                            }
                            if samples.len() < 4 && !samples.contains(&d) {
                                samples.push(d);
                            }
                        } else {
                            num_cnt += 1;
                            min_num = Some(min_num.map_or(*n, |m| m.min(*n)));
                            max_num = Some(max_num.map_or(*n, |m| m.max(*n)));
                            let s = crate::model::format_number(*n);
                            if uniq.len() < 1000 {
                                uniq.insert(s.clone());
                            }
                            if samples.len() < 4 && !samples.contains(&s) {
                                samples.push(s);
                            }
                        }
                    }
                    Value::Text(t) => {
                        let ts = t.trim();
                        if ts.is_empty() {
                            continue;
                        }
                        non_empty += 1;
                        if parse_date_ymd(v.as_ref()).is_some() {
                            date_cnt += 1;
                        } else {
                            text_cnt += 1;
                        }
                        if uniq.len() < 1000 {
                            uniq.insert(ts.to_string());
                        }
                        if samples.len() < 4 && !samples.iter().any(|x| x == ts) {
                            samples.push(ts.to_string());
                        }
                    }
                    other => {
                        let s = other.display();
                        if s.is_empty() {
                            continue;
                        }
                        non_empty += 1;
                        text_cnt += 1;
                        if uniq.len() < 1000 {
                            uniq.insert(s.clone());
                        }
                        if samples.len() < 4 && !samples.contains(&s) {
                            samples.push(s);
                        }
                    }
                }
            }
        }
        let kind = if non_empty == 0 {
            FieldKind::Text
        } else if date_cnt * 2 >= non_empty {
            FieldKind::Date
        } else if num_cnt * 2 >= non_empty && text_cnt * 4 < non_empty {
            // 像「年份/编号/工号」这类纯数字列若名称明显像 ID 则保留文本感知，否则数值
            let low = name.to_lowercase();
            let looks_id = low.contains("id")
                || name.contains("编号")
                || name.contains("工号")
                || name.contains("单号")
                || name.contains("代码")
                || name.contains("邮编");
            if looks_id {
                FieldKind::Text
            } else {
                FieldKind::Numeric
            }
        } else {
            FieldKind::Text
        };
        out.push(FieldProfile {
            col: c,
            name,
            kind,
            non_empty,
            total_rows,
            unique_count: uniq.len(),
            min_num,
            max_num,
            samples,
        });
    }
    out
}

/// 智能推荐的透视预设（P2）
#[derive(Clone, Debug)]
pub struct PivotPreset {
    pub title: String,
    pub subtitle: String,
    pub spec: PivotSpec2,
    pub options: PivotTableOptions,
}

/// 根据源表列画像自动生成 2~3 个立即可用的财务/分析透视表预设。
pub fn suggest_presets(
    source: (u32, u32, u32, u32),
    profiles: &[FieldProfile],
) -> Vec<PivotPreset> {
    let mut presets = Vec::new();
    let dims: Vec<&FieldProfile> = profiles
        .iter()
        .filter(|p| p.kind == FieldKind::Text && p.non_empty > 0)
        .collect();
    let dates: Vec<&FieldProfile> = profiles
        .iter()
        .filter(|p| p.kind == FieldKind::Date && p.non_empty > 0)
        .collect();
    let nums: Vec<&FieldProfile> = profiles
        .iter()
        .filter(|p| p.kind == FieldKind::Numeric && p.non_empty > 0)
        .collect();

    // 挑选最具业务价值的主数值列（优先名称含 金额/销售/收入/利润/数量/总计，否则最后一个数值列）
    let main_val = nums
        .iter()
        .find(|p| {
            let n = &p.name;
            n.contains("金额")
                || n.contains("销售")
                || n.contains("收入")
                || n.contains("利润")
                || n.contains("成本")
                || n.contains("费用")
                || n.to_lowercase().contains("amount")
                || n.to_lowercase().contains("sales")
                || n.to_lowercase().contains("revenue")
        })
        .copied()
        .or_else(|| nums.last().copied());

    // 挑选最佳维度（唯一值在 2..=30 之间的最适合分组）
    let mut ranked_dims = dims.clone();
    ranked_dims.sort_by_key(|p| {
        let u = p.unique_count;
        if (2..=20).contains(&u) {
            (0, u)
        } else if (21..=60).contains(&u) {
            (1, u)
        } else {
            (2, u)
        }
    });

    // 预设 1：主维度降序排行 + 数据条 + 占总计%
    if let (Some(d1), Some(v1)) = (ranked_dims.first().copied(), main_val) {
        let spec = PivotSpec2 {
            source,
            rows: vec![AxisField::plain(d1.col)],
            cols: vec![],
            values: vec![
                ValueField {
                    col: v1.col,
                    agg: Agg::Sum,
                    label: None,
                    show_as: ShowAs::Normal,
                    base: None,
                },
                ValueField {
                    col: v1.col,
                    agg: Agg::Sum,
                    label: Some("占比".into()),
                    show_as: ShowAs::PercentOfTotal,
                    base: None,
                },
            ],
            filters: vec![],
            layout: PivotLayout::default(),
        };
        let mut opts = PivotTableOptions {
            show_data_bars: true,
            ..Default::default()
        };
        opts.row_ext = vec![AxisFieldExt {
            sort: FieldSortMode::ValueDesc(0),
            ..Default::default()
        }];
        opts.val_ext = vec![
            ValueFieldExt {
                data_bar: true,
                ..Default::default()
            },
            ValueFieldExt::default(),
        ];
        presets.push(PivotPreset {
            title: format!("按「{}」降序排行", d1.name),
            subtitle: format!("{} 求和 + 占比 + 格内数据条", v1.name),
            spec,
            options: opts,
        });
    }

    // 预设 2：日期按月趋势 × 主维度交叉表
    if let (Some(dt), Some(v1)) = (dates.first().copied(), main_val) {
        let col_field = ranked_dims
            .first()
            .filter(|d| d.unique_count <= 8)
            .map(|d| vec![AxisField::plain(d.col)])
            .unwrap_or_default();
        let subtitle = if let Some(cf) = ranked_dims.first().filter(|d| d.unique_count <= 8) {
            format!("{} 按月 × {} 交叉分析 {}", dt.name, cf.name, v1.name)
        } else {
            format!("{} 按月汇总 {} 与环比趋势", dt.name, v1.name)
        };
        let spec = PivotSpec2 {
            source,
            rows: vec![AxisField {
                col: dt.col,
                group: GroupBy::Month,
            }],
            cols: col_field,
            values: vec![ValueField {
                col: v1.col,
                agg: Agg::Sum,
                label: None,
                show_as: ShowAs::Normal,
                base: None,
            }],
            filters: vec![],
            layout: PivotLayout::default(),
        };
        let mut opts = PivotTableOptions::default();
        opts.style_theme = PivotStyleTheme::Green;
        presets.push(PivotPreset {
            title: format!("按「{}」月度趋势", dt.name),
            subtitle,
            spec,
            options: opts,
        });
    }

    // 预设 3：双维度交叉透视表
    if ranked_dims.len() >= 2
        && let Some(v1) = main_val
    {
        let d1 = ranked_dims[0];
        let d2 = ranked_dims[1];
        let spec = PivotSpec2 {
            source,
            rows: vec![AxisField::plain(d1.col)],
            cols: vec![AxisField::plain(d2.col)],
            values: vec![ValueField {
                col: v1.col,
                agg: Agg::Sum,
                label: None,
                show_as: ShowAs::Normal,
                base: None,
            }],
            filters: vec![],
            layout: PivotLayout::default(),
        };
        let mut opts = PivotTableOptions::default();
        opts.row_ext = vec![AxisFieldExt {
            sort: FieldSortMode::ValueDesc(0),
            ..Default::default()
        }];
        presets.push(PivotPreset {
            title: format!("「{}」×「{}」交叉透视", d1.name, d2.name),
            subtitle: format!("按 {} 汇总，行列总计一览", v1.name),
            spec,
            options: opts,
        });
    }

    // 兜底：全是文本列时按首列计数
    if presets.is_empty()
        && let Some(first) = profiles.first()
    {
        let spec = PivotSpec2 {
            source,
            rows: vec![AxisField::plain(first.col)],
            cols: vec![],
            values: vec![ValueField {
                col: first.col,
                agg: Agg::Count,
                label: None,
                show_as: ShowAs::Normal,
                base: None,
            }],
            filters: vec![],
            layout: PivotLayout::default(),
        };
        let mut opts = PivotTableOptions::default();
        opts.row_ext = vec![AxisFieldExt {
            sort: FieldSortMode::ValueDesc(0),
            ..Default::default()
        }];
        presets.push(PivotPreset {
            title: format!("按「{}」频次统计", first.name),
            subtitle: "统计各类别出现次数并降序排列".into(),
            spec,
            options: opts,
        });
    }

    presets
}

/// 探测源区域下方新追加的连续非空数据行（P2 自动扩展数据源）
pub fn detect_expanded_source(
    sheet: &Sheet,
    source: (u32, u32, u32, u32),
) -> Option<(u32, u32, u32, u32)> {
    let (r1, c1, r2, c2) = source;
    let mut nr2 = r2;
    let max_row = sheet
        .cells()
        .iter_pages()
        .map(|(pi, _)| (pi + 1) * crate::store::PAGE_ROWS)
        .max()
        .unwrap_or(r2)
        .min(r2.saturating_add(50_000));
    while nr2 < max_row {
        let next = nr2 + 1;
        let mut any = false;
        for c in c1..=c2 {
            if !matches!(sheet.value_ref(next, c).as_ref(), Value::Empty) {
                any = true;
                break;
            }
        }
        if !any {
            break;
        }
        nr2 = next;
    }
    (nr2 > r2).then_some((r1, c1, nr2, c2))
}

/// 返回源表某列按分组方式归一后的所有去重标签（已按拼音排序，供筛选器与切片器使用）
pub fn distinct_field_values(
    sheet: &Sheet,
    source: (u32, u32, u32, u32),
    col: u32,
    group: &GroupBy,
) -> Vec<String> {
    let (r1, _, r2, _) = source;
    let first = r1.saturating_add(1);
    if first > r2 {
        return Vec::new();
    }
    let mut set: FxHashSet<String> = Default::default();
    let mut has_blank = false;
    for r in first..=r2 {
        let v = sheet.value_ref(r, col);
        if matches!(v.as_ref(), Value::Empty) {
            has_blank = true;
            continue;
        }
        let k = group_key(v.as_ref(), group);
        if k.is_empty() {
            has_blank = true;
        } else {
            set.insert(k);
        }
    }
    let mut vals: Vec<(Vec<u8>, String)> = set
        .into_iter()
        .map(|s| (crate::collate::sort_key(&s), s))
        .collect();
    vals.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut out: Vec<String> = vals.into_iter().map(|(_, s)| s).collect();
    if has_blank {
        out.push("(空白)".into());
    }
    out
}

#[inline]
fn normalize_blank(s: String) -> String {
    if s.is_empty() {
        "(空白)".into()
    } else {
        s
    }
}

// =========================================================================
// 4. 全功能透视计算引擎 `compute_table`
// =========================================================================

pub fn compute_table(source_sheet: &Sheet, model: &PivotTableModel) -> ComputedPivot {
    let spec = &model.spec;
    let opts = &model.options;
    let (r1, c1, r2, c2) = spec.source;
    let n_src_cols = (c2.saturating_sub(c1) + 1) as usize;
    let src_headers: Vec<String> = (c1..=c2).map(|c| header_name(source_sheet, r1, c)).collect();
    let hdr = |col: u32| header_name(source_sheet, r1, col);

    // 解析计算字段
    let parsed_calc_fields: Vec<(String, CalcExpr)> = opts
        .calc_fields
        .iter()
        .filter_map(|(name, formula)| {
            parse_calc_formula(formula, &src_headers).map(|e| (name.clone(), e))
        })
        .collect();

    // 所有实际要输出的「值槽」：常规 ValueField + 计算字段
    let base_val_count = spec.values.len();
    let total_val_count = base_val_count + parsed_calc_fields.len();

    let need_distinct: Vec<bool> = spec
        .values
        .iter()
        .map(|v| v.agg == Agg::DistinctCount)
        .collect();
    let need_median: Vec<bool> = spec
        .values
        .iter()
        .map(|v| v.agg == Agg::Median)
        .collect();
    let has_calc_fields = !parsed_calc_fields.is_empty();

    // 页筛选标题区（所有配置了 FilterField 的都显示，即使 keep 为空也显示 "(全部)" 以便点击下拉筛选！）
    let page_filters: Vec<(String, String)> = spec
        .filters
        .iter()
        .map(|f| {
            let label = if f.keep.is_empty() {
                "(全部)".to_string()
            } else if f.keep.len() == 1 {
                f.keep[0].clone()
            } else {
                format!("(多项：{} 项)", f.keep.len())
            };
            (hdr(f.col), label)
        })
        .collect();

    // 预编译页筛选
    let compiled_page_filters: Vec<(u32, FxHashSet<&str>)> = spec
        .filters
        .iter()
        .filter(|f| !f.keep.is_empty())
        .map(|f| (f.col, f.keep.iter().map(String::as_str).collect()))
        .collect();

    // 预编译行/列各层隐藏项
    let row_hidden: Vec<FxHashSet<&str>> = spec
        .rows
        .iter()
        .enumerate()
        .map(|(i, _)| {
            opts.row_ext
                .get(i)
                .map(|e| e.hidden_items.iter().map(String::as_str).collect())
                .unwrap_or_default()
        })
        .collect();
    let col_hidden: Vec<FxHashSet<&str>> = spec
        .cols
        .iter()
        .enumerate()
        .map(|(i, _)| {
            opts.col_ext
                .get(i)
                .map(|e| e.hidden_items.iter().map(String::as_str).collect())
                .unwrap_or_default()
        })
        .collect();

    // 聚合容器：row_path -> col_path -> (Vec<RichAcc>, Option<Vec<f64>> source-col sums)
    type CellBucket = (Vec<RichAcc>, Vec<f64>);
    let make_bucket = || -> CellBucket {
        let accs = (0..base_val_count)
            .map(|vi| RichAcc::new(need_distinct[vi], need_median[vi]))
            .collect();
        let sums = if has_calc_fields {
            vec![0.0f64; n_src_cols]
        } else {
            Vec::new()
        };
        (accs, sums)
    };

    let mut data: BTreeMap<Vec<String>, BTreeMap<Vec<String>, CellBucket>> = BTreeMap::new();
    let mut row_first: HashMap<Vec<String>, usize> = HashMap::new();
    let mut col_first: HashMap<Vec<String>, usize> = HashMap::new();

    let first_row = r1.saturating_add(1);
    let source_row_count = if r2 >= first_row {
        (r2 - first_row + 1) as usize
    } else {
        0
    };
    let mut matched_row_count = 0usize;

    if first_row <= r2 {
        for r in first_row..=r2 {
            // 1. 跳过全空行
            let mut any_in_row = false;
            for c in c1..=c2 {
                if !matches!(source_sheet.value_ref(r, c).as_ref(), Value::Empty) {
                    any_in_row = true;
                    break;
                }
            }
            if !any_in_row {
                continue;
            }
            // 2. 页筛选
            let mut pass = true;
            for (fc, allowed) in &compiled_page_filters {
                let raw = source_sheet.value_ref(r, *fc);
                let disp = normalize_blank(raw.display());
                if !allowed.contains(disp.as_str()) {
                    pass = false;
                    break;
                }
            }
            if !pass {
                continue;
            }
            // 3. 计算行/列路径并检查轴隐藏项
            let mut rpath = Vec::with_capacity(spec.rows.len());
            for (i, rf) in spec.rows.iter().enumerate() {
                let raw = source_sheet.value_ref(r, rf.col);
                let k = normalize_blank(group_key(raw.as_ref(), &rf.group));
                if !row_hidden[i].is_empty() && row_hidden[i].contains(k.as_str()) {
                    pass = false;
                    break;
                }
                rpath.push(k);
            }
            if !pass {
                continue;
            }
            let mut cpath = Vec::with_capacity(spec.cols.len());
            for (i, cf) in spec.cols.iter().enumerate() {
                let raw = source_sheet.value_ref(r, cf.col);
                let k = normalize_blank(group_key(raw.as_ref(), &cf.group));
                if !col_hidden[i].is_empty() && col_hidden[i].contains(k.as_str()) {
                    pass = false;
                    break;
                }
                cpath.push(k);
            }
            if !pass {
                continue;
            }
            matched_row_count += 1;
            let ord = matched_row_count;
            row_first.entry(rpath.clone()).or_insert(ord);
            col_first.entry(cpath.clone()).or_insert(ord);

            let bucket = data
                .entry(rpath)
                .or_default()
                .entry(cpath)
                .or_insert_with(make_bucket);
            for (vi, vf) in spec.values.iter().enumerate() {
                let v = source_sheet.value_ref(r, vf.col);
                bucket.0[vi].push(Some(v.as_ref()));
            }
            if has_calc_fields {
                for ci in 0..n_src_cols {
                    let v = source_sheet.value_ref(r, c1 + ci as u32);
                    match v.as_ref() {
                        Value::Number(n) => bucket.1[ci] += *n,
                        Value::Text(t) => {
                            if let Ok(n) = t.trim().parse::<f64>() {
                                bucket.1[ci] += n;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let merge_bucket = |dst: &mut CellBucket, src: &CellBucket| {
        for (d, s) in dst.0.iter_mut().zip(src.0.iter()) {
            d.merge(s);
        }
        for (d, s) in dst.1.iter_mut().zip(src.1.iter()) {
            *d += *s;
        }
    };

    let finish_slot = |bucket: &CellBucket, vi: usize| -> Value {
        if vi < base_val_count {
            bucket.0[vi].finish(spec.values[vi].agg)
        } else {
            let (_, expr) = &parsed_calc_fields[vi - base_val_count];
            match eval_calc_expr(expr, &bucket.1) {
                Some(n) if n.is_finite() => Value::Number(n),
                _ => Value::Text("#DIV/0!".into()),
            }
        }
    };

    // 4. 计算各行路径/列路径的排序键、TopN 与折叠视图
    let metric_of_bucket = |b: &CellBucket, vi: usize| -> f64 {
        if total_val_count == 0 {
            return 0.0;
        }
        let idx = vi.min(total_val_count - 1);
        match finish_slot(b, idx) {
            Value::Number(n) => n,
            _ => 0.0,
        }
    };

    // 汇总每个列路径的整列总计（供列排序与列 TopN）
    let mut col_totals_map: HashMap<Vec<String>, CellBucket> = HashMap::new();
    for cols_map in data.values() {
        for (cp, b) in cols_map {
            let entry = col_totals_map.entry(cp.clone()).or_insert_with(make_bucket);
            merge_bucket(entry, b);
        }
    }
    // 汇总每个行路径的整行总计（供行排序与行 TopN）
    let mut row_totals_map: HashMap<Vec<String>, CellBucket> = HashMap::new();
    for (rp, cols_map) in &data {
        let entry = row_totals_map.entry(rp.clone()).or_insert_with(make_bucket);
        for b in cols_map.values() {
            merge_bucket(entry, b);
        }
    }

    // 顶层行 TopN 过滤
    let mut allowed_top_rows: Option<HashSet<String>> = None;
    if let Some(topn) = opts.row_ext.first().and_then(|e| e.top_n)
        && topn.count > 0
        && !spec.rows.is_empty()
    {
        let mut by_top: HashMap<String, CellBucket> = HashMap::new();
        for (rp, b) in &row_totals_map {
            let top = rp.first().cloned().unwrap_or_default();
            let entry = by_top.entry(top).or_insert_with(make_bucket);
            merge_bucket(entry, b);
        }
        let mut ranked: Vec<(String, f64)> = by_top
            .into_iter()
            .map(|(k, b)| (k, metric_of_bucket(&b, topn.value_idx)))
            .collect();
        match topn.kind {
            TopNKind::Top => ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0))),
            TopNKind::Bottom => ranked.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0))),
        }
        ranked.truncate(topn.count);
        allowed_top_rows = Some(ranked.into_iter().map(|(k, _)| k).collect());
    }

    // 顶层列 TopN 过滤
    let mut allowed_top_cols: Option<HashSet<String>> = None;
    if let Some(topn) = opts.col_ext.first().and_then(|e| e.top_n)
        && topn.count > 0
        && !spec.cols.is_empty()
    {
        let mut by_top: HashMap<String, CellBucket> = HashMap::new();
        for (cp, b) in &col_totals_map {
            let top = cp.first().cloned().unwrap_or_default();
            let entry = by_top.entry(top).or_insert_with(make_bucket);
            merge_bucket(entry, b);
        }
        let mut ranked: Vec<(String, f64)> = by_top
            .into_iter()
            .map(|(k, b)| (k, metric_of_bucket(&b, topn.value_idx)))
            .collect();
        match topn.kind {
            TopNKind::Top => ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0))),
            TopNKind::Bottom => ranked.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0))),
        }
        ranked.truncate(topn.count);
        allowed_top_cols = Some(ranked.into_iter().map(|(k, _)| k).collect());
    }

    // 递归按层级排序路径（每层遵守各自 FieldSortMode！）
    let sort_paths = |paths: Vec<Vec<String>>,
                      exts: &[AxisFieldExt],
                      first_ord: &HashMap<Vec<String>, usize>,
                      totals: &HashMap<Vec<String>, CellBucket>|
     -> Vec<Vec<String>> {
        if paths.is_empty() {
            return paths;
        }
        let depth = paths[0].len();
        if depth == 0 {
            return paths;
        }
        // 预先统计每个前缀的总计值，支持任意层级按值排序
        let mut prefix_bucket: HashMap<Vec<String>, CellBucket> = HashMap::new();
        let mut prefix_ord: HashMap<Vec<String>, usize> = HashMap::new();
        for p in &paths {
            let ord = first_ord.get(p).copied().unwrap_or(usize::MAX);
            let b = totals.get(p);
            for d in 1..=depth {
                let pref = p[..d].to_vec();
                prefix_ord
                    .entry(pref.clone())
                    .and_modify(|m| *m = (*m).min(ord))
                    .or_insert(ord);
                if let Some(b) = b {
                    let entry = prefix_bucket.entry(pref).or_insert_with(make_bucket);
                    merge_bucket(entry, b);
                }
            }
        }
        let mut out = paths;
        out.sort_by(|a, b| {
            for d in 0..depth {
                if a[d] == b[d] {
                    continue;
                }
                let mode = exts.get(d).map(|e| e.sort).unwrap_or_default();
                let pa = a[..=d].to_vec();
                let pb = b[..=d].to_vec();
                let ord = match mode {
                    FieldSortMode::PinyinAsc => {
                        crate::collate::sort_key(&a[d]).cmp(&crate::collate::sort_key(&b[d]))
                    }
                    FieldSortMode::PinyinDesc => {
                        crate::collate::sort_key(&b[d]).cmp(&crate::collate::sort_key(&a[d]))
                    }
                    FieldSortMode::ValueDesc(vi) => {
                        let va = prefix_bucket
                            .get(&pa)
                            .map(|bk| metric_of_bucket(bk, vi))
                            .unwrap_or(f64::NEG_INFINITY);
                        let vb = prefix_bucket
                            .get(&pb)
                            .map(|bk| metric_of_bucket(bk, vi))
                            .unwrap_or(f64::NEG_INFINITY);
                        vb.total_cmp(&va)
                    }
                    FieldSortMode::ValueAsc(vi) => {
                        let va = prefix_bucket
                            .get(&pa)
                            .map(|bk| metric_of_bucket(bk, vi))
                            .unwrap_or(f64::INFINITY);
                        let vb = prefix_bucket
                            .get(&pb)
                            .map(|bk| metric_of_bucket(bk, vi))
                            .unwrap_or(f64::INFINITY);
                        va.total_cmp(&vb)
                    }
                    FieldSortMode::Source => {
                        let oa = prefix_ord.get(&pa).copied().unwrap_or(usize::MAX);
                        let ob = prefix_ord.get(&pb).copied().unwrap_or(usize::MAX);
                        oa.cmp(&ob)
                    }
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
                return a[d].cmp(&b[d]);
            }
            std::cmp::Ordering::Equal
        });
        out
    };

    let raw_col_paths: Vec<Vec<String>> = col_first
        .keys()
        .filter(|cp| {
            allowed_top_cols
                .as_ref()
                .is_none_or(|set| cp.first().is_some_and(|t| set.contains(t)))
        })
        .cloned()
        .collect();
    let mut col_paths = sort_paths(raw_col_paths, &opts.col_ext, &col_first, &col_totals_map);
    if col_paths.is_empty() && spec.cols.is_empty() {
        col_paths.push(Vec::new());
    }

    let raw_row_paths: Vec<Vec<String>> = data
        .keys()
        .filter(|rp| {
            allowed_top_rows
                .as_ref()
                .is_none_or(|set| rp.first().is_some_and(|t| set.contains(t)))
        })
        .cloned()
        .collect();
    let row_paths = sort_paths(raw_row_paths, &opts.row_ext, &row_first, &row_totals_map);

    // 5. 构建列槽位（支持列字段多级小计 + 折叠）
    #[derive(Clone, Debug)]
    enum ActiveColSlot {
        Leaf(Vec<String>),
        Collapsed { prefix: Vec<String>, label: String },
        Subtotal { prefix: Vec<String>, label: String },
    }
    let collapsed_col_set: HashSet<Vec<String>> = opts.collapsed_cols.iter().cloned().collect();
    let mut col_slots: Vec<ActiveColSlot> = Vec::new();
    if spec.cols.len() >= 2 {
        let mut ci = 0;
        while ci < col_paths.len() {
            let top = col_paths[ci][0].clone();
            let start = ci;
            while ci < col_paths.len() && col_paths[ci][0] == top {
                ci += 1;
            }
            let pref = vec![top.clone()];
            if collapsed_col_set.contains(&pref) {
                col_slots.push(ActiveColSlot::Collapsed {
                    prefix: pref,
                    label: top,
                });
            } else {
                for p in &col_paths[start..ci] {
                    col_slots.push(ActiveColSlot::Leaf(p.clone()));
                }
                let show_col_sub = opts.subtotal_pos != SubtotalPos::Off
                    && opts.col_ext.first().map(|e| e.show_subtotal).unwrap_or(true);
                if show_col_sub {
                    col_slots.push(ActiveColSlot::Subtotal {
                        prefix: pref,
                        label: format!("{top} 汇总"),
                    });
                }
            }
        }
    } else {
        for p in &col_paths {
            col_slots.push(ActiveColSlot::Leaf(p.clone()));
        }
    }

    // 6. 计算表头维度与列布局
    let row_label_cols = if spec.rows.is_empty() {
        1
    } else if opts.report_layout == ReportLayout::Compact {
        1
    } else {
        spec.rows.len()
    };
    let show_col_grand = !spec.cols.is_empty() && opts.col_grand_totals && total_val_count > 0;
    let grand_block = if show_col_grand { total_val_count } else { 0 };
    let data_col_count = col_slots.len() * total_val_count.max(1);
    let grand_base = row_label_cols + data_col_count;
    let total_cols = (grand_base + grand_block).max(row_label_cols);

    let val_caption = |vi: usize| -> String {
        if vi < base_val_count {
            let v = &spec.values[vi];
            v.label
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("{}:{}", crate::pivot::agg_prefix(v.agg), hdr(v.col)))
        } else {
            parsed_calc_fields[vi - base_val_count].0.clone()
        }
    };

    // 表头构建
    let mut headers: Vec<Vec<String>> = Vec::new();
    let mut meta = PivotGridMeta {
        anchor: model.anchor,
        page_filter_rows: if page_filters.is_empty() {
            0
        } else {
            page_filters.len() as u32 + 1
        },
        row_label_cols: row_label_cols as u32,
        source_row_count,
        matched_row_count,
        ..Default::default()
    };

    let (r0, c0) = model.anchor;
    // 记录页筛选行元信息与下拉按钮
    for (fi, (fname, _)) in page_filters.iter().enumerate() {
        let active = !spec.filters[fi].keep.is_empty();
        meta.row_kinds.push(PivotRowKind::PageFilter);
        meta.row_indents.push(0);
        meta.row_paths_by_grid_row.push(None);
        meta.filter_buttons.push(PivotFilterButton {
            row: r0 + fi as u32,
            col: c0 + 1,
            target: PivotFilterTarget::PageFilter(fi),
            active,
            label: fname.clone(),
        });
    }
    if !page_filters.is_empty() {
        meta.row_kinds.push(PivotRowKind::Blank);
        meta.row_indents.push(0);
        meta.row_paths_by_grid_row.push(None);
    }

    let head_start_row = r0 + meta.page_filter_rows;
    let mut col_paths_by_grid_col: Vec<Option<Vec<String>>> = vec![None; total_cols];
    let mut col_is_grand_total = vec![false; total_cols];
    let mut col_is_subtotal = vec![false; total_cols];
    let mut col_value_idx: Vec<Option<usize>> = vec![None; total_cols];

    for (si, slot) in col_slots.iter().enumerate() {
        let base_c = row_label_cols + si * total_val_count.max(1);
        for vi in 0..total_val_count.max(1) {
            let gc = base_c + vi;
            if gc < total_cols {
                col_value_idx[gc] = (vi < total_val_count).then_some(vi);
                match slot {
                    ActiveColSlot::Leaf(p) => col_paths_by_grid_col[gc] = Some(p.clone()),
                    ActiveColSlot::Collapsed { prefix, .. } => {
                        col_paths_by_grid_col[gc] = Some(prefix.clone());
                    }
                    ActiveColSlot::Subtotal { prefix, .. } => {
                        col_paths_by_grid_col[gc] = Some(prefix.clone());
                        col_is_subtotal[gc] = true;
                    }
                }
            }
        }
    }
    for vi in 0..grand_block {
        let gc = grand_base + vi;
        if gc < total_cols {
            col_is_grand_total[gc] = true;
            col_value_idx[gc] = Some(vi);
        }
    }

    // 生成表头行（0 列字段 = 1 行；1 列字段 = 2 行或 3 行；多列字段 = depth + 1 行）
    if spec.cols.is_empty() {
        let mut h = vec![String::new(); total_cols];
        if spec.rows.is_empty() {
            h[0] = "汇总".into();
        } else if opts.report_layout == ReportLayout::Compact {
            h[0] = "行标签".into();
            let active = opts.row_ext.iter().any(|e| !e.hidden_items.is_empty() || e.top_n.is_some());
            meta.filter_buttons.push(PivotFilterButton {
                row: head_start_row,
                col: c0,
                target: PivotFilterTarget::RowField(0),
                active,
                label: hdr(spec.rows[0].col),
            });
        } else {
            for (i, rf) in spec.rows.iter().enumerate() {
                h[i] = hdr(rf.col);
                let active = opts
                    .row_ext
                    .get(i)
                    .is_some_and(|e| !e.hidden_items.is_empty() || e.top_n.is_some());
                meta.filter_buttons.push(PivotFilterButton {
                    row: head_start_row,
                    col: c0 + i as u32,
                    target: PivotFilterTarget::RowField(i),
                    active,
                    label: h[i].clone(),
                });
            }
        }
        for vi in 0..total_val_count {
            let gc = row_label_cols + vi;
            if gc < total_cols {
                h[gc] = val_caption(vi);
            }
        }
        headers.push(h);
    } else {
        // 第 0 行：列字段标签行（带列筛选下拉按钮）
        let mut top_h = vec![String::new(); total_cols];
        for (di, cf) in spec.cols.iter().enumerate() {
            let gc = (row_label_cols + di).min(total_cols - 1);
            top_h[gc] = hdr(cf.col);
            let active = opts
                .col_ext
                .get(di)
                .is_some_and(|e| !e.hidden_items.is_empty() || e.top_n.is_some());
            meta.filter_buttons.push(PivotFilterButton {
                row: head_start_row,
                col: c0 + gc as u32,
                target: PivotFilterTarget::ColField(di),
                active,
                label: top_h[gc].clone(),
            });
        }
        headers.push(top_h);

        // 中间层级：每层列标签
        let col_depth = spec.cols.len();
        for lvl in 0..col_depth {
            let mut h = vec![String::new(); total_cols];
            let hr = head_start_row + 1 + lvl as u32;
            let mut prev_top = String::new();
            for (si, slot) in col_slots.iter().enumerate() {
                let gc = row_label_cols + si * total_val_count.max(1);
                if gc >= total_cols {
                    continue;
                }
                match slot {
                    ActiveColSlot::Leaf(p) => {
                        if let Some(lbl) = p.get(lvl) {
                            if lvl == 0 && col_depth >= 2 {
                                if *lbl != prev_top {
                                    h[gc] = lbl.clone();
                                    prev_top = lbl.clone();
                                    meta.collapse_buttons.push(PivotCollapseButton {
                                        row: hr,
                                        col: c0 + gc as u32,
                                        is_row: false,
                                        path: vec![lbl.clone()],
                                        collapsed: false,
                                    });
                                }
                            } else {
                                h[gc] = lbl.clone();
                            }
                        }
                    }
                    ActiveColSlot::Collapsed { prefix, label } => {
                        if lvl == 0 {
                            h[gc] = label.clone();
                            prev_top = label.clone();
                            meta.collapse_buttons.push(PivotCollapseButton {
                                row: hr,
                                col: c0 + gc as u32,
                                is_row: false,
                                path: prefix.clone(),
                                collapsed: true,
                            });
                        }
                    }
                    ActiveColSlot::Subtotal { label, .. } => {
                        if lvl == 0 {
                            h[gc] = label.clone();
                        }
                    }
                }
            }
            if lvl == 0 && show_col_grand {
                if total_val_count == 1 {
                    h[grand_base] = "总计".into();
                } else {
                    for vi in 0..total_val_count {
                        h[grand_base + vi] = format!("{} 汇总", val_caption(vi));
                    }
                }
            }
            headers.push(h);
        }

        // 最后一行表头：行字段标题 + (多值字段时的值字段名)
        let last_head_row = head_start_row + headers.len() as u32;
        let mut bottom_h = vec![String::new(); total_cols];
        if spec.rows.is_empty() {
            bottom_h[0] = "汇总".into();
        } else if opts.report_layout == ReportLayout::Compact {
            bottom_h[0] = "行标签".into();
            let active = opts.row_ext.iter().any(|e| !e.hidden_items.is_empty() || e.top_n.is_some());
            meta.filter_buttons.push(PivotFilterButton {
                row: last_head_row,
                col: c0,
                target: PivotFilterTarget::RowField(0),
                active,
                label: hdr(spec.rows[0].col),
            });
        } else {
            for (i, rf) in spec.rows.iter().enumerate() {
                bottom_h[i] = hdr(rf.col);
                let active = opts
                    .row_ext
                    .get(i)
                    .is_some_and(|e| !e.hidden_items.is_empty() || e.top_n.is_some());
                meta.filter_buttons.push(PivotFilterButton {
                    row: last_head_row,
                    col: c0 + i as u32,
                    target: PivotFilterTarget::RowField(i),
                    active,
                    label: bottom_h[i].clone(),
                });
            }
        }
        if total_val_count >= 1 {
            for (si, _) in col_slots.iter().enumerate() {
                let base_c = row_label_cols + si * total_val_count;
                for vi in 0..total_val_count {
                    if base_c + vi < total_cols {
                        bottom_h[base_c + vi] = val_caption(vi);
                    }
                }
            }
            if show_col_grand {
                for vi in 0..total_val_count {
                    if grand_base + vi < total_cols {
                        bottom_h[grand_base + vi] = val_caption(vi);
                    }
                }
            }
        }
        headers.push(bottom_h);
    }

    meta.header_rows = headers.len() as u32;
    for _ in 0..headers.len() {
        meta.row_kinds.push(PivotRowKind::Header);
        meta.row_indents.push(0);
        meta.row_paths_by_grid_row.push(None);
    }

    // 7. 组装数据行（支持多级小计 Top/Bottom/Off、多级折叠/展开、Compact/Outline/Tabular 布局）
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut row_kinds_data: Vec<PivotRowKind> = Vec::new();
    let mut row_indents_data: Vec<u8> = Vec::new();
    let mut row_paths_data: Vec<Option<Vec<String>>> = Vec::new();
    let mut grand_total_row: Option<usize> = None;

    let eval_row_values = |cols_map: &BTreeMap<Vec<String>, CellBucket>| -> Vec<Value> {
        let mut vals = vec![Value::Empty; total_cols - row_label_cols];
        for (si, slot) in col_slots.iter().enumerate() {
            let base = si * total_val_count.max(1);
            let mut merged = make_bucket();
            match slot {
                ActiveColSlot::Leaf(cp) => {
                    if let Some(b) = cols_map.get(cp) {
                        merged = b.clone();
                    }
                }
                ActiveColSlot::Collapsed { prefix, .. }
                | ActiveColSlot::Subtotal { prefix, .. } => {
                    for (cp, b) in cols_map {
                        if cp.starts_with(prefix) {
                            merge_bucket(&mut merged, b);
                        }
                    }
                }
            }
            for vi in 0..total_val_count {
                if base + vi < vals.len() {
                    vals[base + vi] = finish_slot(&merged, vi);
                }
            }
        }
        if show_col_grand {
            let mut row_all = make_bucket();
            for b in cols_map.values() {
                merge_bucket(&mut row_all, b);
            }
            let base = grand_base - row_label_cols;
            for vi in 0..total_val_count {
                if base + vi < vals.len() {
                    vals[base + vi] = finish_slot(&row_all, vi);
                }
            }
        }
        vals
    };

    let merge_cols_maps = |paths: &[Vec<String>]| -> BTreeMap<Vec<String>, CellBucket> {
        let mut out: BTreeMap<Vec<String>, CellBucket> = BTreeMap::new();
        for rp in paths {
            if let Some(cm) = data.get(rp) {
                for (cp, b) in cm {
                    let entry = out.entry(cp.clone()).or_insert_with(make_bucket);
                    merge_bucket(entry, b);
                }
            }
        }
        out
    };

    let collapsed_row_set: HashSet<Vec<String>> = opts.collapsed_rows.iter().cloned().collect();
    let data_start_row = head_start_row + meta.header_rows;
    let mut prev_labels = vec![String::new(); spec.rows.len()];

    if spec.rows.is_empty() {
        // 无行字段：输出单行汇总
        if !data.is_empty() {
            let all_cm = merge_cols_maps(&row_paths);
            let mut line = vec![Value::Empty; total_cols];
            line[0] = Value::Text("汇总".into());
            let vals = eval_row_values(&all_cm);
            for (i, v) in vals.into_iter().enumerate() {
                line[row_label_cols + i] = v;
            }
            rows.push(line);
            row_kinds_data.push(PivotRowKind::Detail);
            row_indents_data.push(0);
            row_paths_data.push(Some(Vec::new()));
        }
    } else if spec.rows.len() == 1 {
        for rp in &row_paths {
            let mut line = vec![Value::Empty; total_cols];
            line[0] = Value::Text(rp[0].clone().into());
            let vals = eval_row_values(&data[rp]);
            for (i, v) in vals.into_iter().enumerate() {
                line[row_label_cols + i] = v;
            }
            rows.push(line);
            row_kinds_data.push(PivotRowKind::Detail);
            row_indents_data.push(0);
            row_paths_data.push(Some(rp.clone()));
        }
    } else {
        // 多级行字段：按顶层分组，支持折叠/展开与 Top/Bottom/Off 小计 + Compact/Outline/Tabular 布局
        let show_top_sub = opts.subtotal_pos != SubtotalPos::Off
            && opts.row_ext.first().map(|e| e.show_subtotal).unwrap_or(true);
        let mut i = 0;
        while i < row_paths.len() {
            let top = row_paths[i][0].clone();
            let start = i;
            while i < row_paths.len() && row_paths[i][0] == top {
                i += 1;
            }
            let group_paths = &row_paths[start..i];
            let top_pref = vec![top.clone()];
            let is_collapsed = collapsed_row_set.contains(&top_pref);
            let group_cm = merge_cols_maps(group_paths);

            if is_collapsed {
                // 折叠态：一行展示父组 + [+] 按钮 + 汇总数值
                let grid_r = data_start_row + rows.len() as u32;
                meta.collapse_buttons.push(PivotCollapseButton {
                    row: grid_r,
                    col: c0,
                    is_row: true,
                    path: top_pref.clone(),
                    collapsed: true,
                });
                let mut line = vec![Value::Empty; total_cols];
                line[0] = Value::Text(top.clone().into());
                let vals = eval_row_values(&group_cm);
                for (vi, v) in vals.into_iter().enumerate() {
                    line[row_label_cols + vi] = v;
                }
                rows.push(line);
                row_kinds_data.push(PivotRowKind::Subtotal(0));
                row_indents_data.push(0);
                row_paths_data.push(Some(top_pref));
                prev_labels.fill(String::new());
                continue;
            }

            // 展开态：Compact / Outline 布局或 Top 小计时先输出父组头行
            let emit_header_group_row = opts.report_layout != ReportLayout::Tabular
                || (show_top_sub && opts.subtotal_pos == SubtotalPos::Top);
            if emit_header_group_row {
                let grid_r = data_start_row + rows.len() as u32;
                meta.collapse_buttons.push(PivotCollapseButton {
                    row: grid_r,
                    col: c0,
                    is_row: true,
                    path: top_pref.clone(),
                    collapsed: false,
                });
                let mut line = vec![Value::Empty; total_cols];
                line[0] = Value::Text(top.clone().into());
                if show_top_sub && opts.subtotal_pos == SubtotalPos::Top {
                    let vals = eval_row_values(&group_cm);
                    for (vi, v) in vals.into_iter().enumerate() {
                        line[row_label_cols + vi] = v;
                    }
                }
                rows.push(line);
                row_kinds_data.push(if show_top_sub && opts.subtotal_pos == SubtotalPos::Top {
                    PivotRowKind::Subtotal(0)
                } else {
                    PivotRowKind::Detail
                });
                row_indents_data.push(0);
                row_paths_data.push(Some(top_pref.clone()));
            }

            for (gi, rp) in group_paths.iter().enumerate() {
                let grid_r = data_start_row + rows.len() as u32;
                if gi == 0 && !emit_header_group_row {
                    meta.collapse_buttons.push(PivotCollapseButton {
                        row: grid_r,
                        col: c0,
                        is_row: true,
                        path: top_pref.clone(),
                        collapsed: false,
                    });
                }
                let mut line = vec![Value::Empty; total_cols];
                let indent: u8 = match opts.report_layout {
                    ReportLayout::Compact => {
                        let tail = rp.last().cloned().unwrap_or_default();
                        line[0] = Value::Text(tail.into());
                        ((rp.len().saturating_sub(1)) * 2).min(12) as u8
                    }
                    ReportLayout::Outline => {
                        for d in 1..rp.len() {
                            let show = opts.repeat_labels || prev_labels[d] != rp[d];
                            if show {
                                line[d] = Value::Text(rp[d].clone().into());
                            }
                            prev_labels[d] = rp[d].clone();
                        }
                        0
                    }
                    ReportLayout::Tabular => {
                        for d in 0..rp.len() {
                            let show = opts.repeat_labels || d + 1 == rp.len() || prev_labels[d] != rp[d];
                            if show {
                                line[d] = Value::Text(rp[d].clone().into());
                            }
                            prev_labels[d] = rp[d].clone();
                        }
                        0
                    }
                };
                let vals = eval_row_values(&data[rp]);
                for (vi, v) in vals.into_iter().enumerate() {
                    line[row_label_cols + vi] = v;
                }
                rows.push(line);
                row_kinds_data.push(PivotRowKind::Detail);
                row_indents_data.push(indent);
                row_paths_data.push(Some(rp.clone()));
            }

            if show_top_sub && opts.subtotal_pos == SubtotalPos::Bottom {
                let mut sline = vec![Value::Empty; total_cols];
                sline[0] = Value::Text(format!("{top} 汇总").into());
                let vals = eval_row_values(&group_cm);
                for (vi, v) in vals.into_iter().enumerate() {
                    sline[row_label_cols + vi] = v;
                }
                rows.push(sline);
                row_kinds_data.push(PivotRowKind::Subtotal(0));
                row_indents_data.push(0);
                row_paths_data.push(Some(top_pref));
            }
            prev_labels.fill(String::new());
        }
    }

    // 8. 总计行
    if opts.row_grand_totals && !rows.is_empty() && total_val_count > 0 {
        let all_cm = merge_cols_maps(&row_paths);
        let mut gline = vec![Value::Empty; total_cols];
        gline[0] = Value::Text("总计".into());
        let vals = eval_row_values(&all_cm);
        for (vi, v) in vals.into_iter().enumerate() {
            gline[row_label_cols + vi] = v;
        }
        grand_total_row = Some(rows.len());
        rows.push(gline);
        row_kinds_data.push(PivotRowKind::GrandTotal);
        row_indents_data.push(0);
        row_paths_data.push(Some(Vec::new()));
    }

    // 9. 构造 PivotGrid 并应用 13 种 ShowAs
    let col_labels: Vec<String> = col_paths
        .iter()
        .map(|p| if p.is_empty() { String::new() } else { p.join(" / ") })
        .collect();
    let mut grid = PivotGrid {
        headers,
        rows,
        row_depth: row_label_cols,
        value_cols: total_val_count,
        col_labels,
        col_paths,
        grand_total_row,
        page_filters,
    };

    apply_rich_show_as(
        &mut grid,
        spec,
        &row_kinds_data,
        &row_paths_data,
        &col_value_idx,
        &col_is_grand_total,
        &col_is_subtotal,
        total_val_count,
    );

    // 10. 多个计算项
    for (cname, cexpr) in &opts.calc_items {
        let full = format!("{cname}={cexpr}");
        if let Some((name, terms)) = parse_calc_item_text(&full)
            && let Some(at) = append_calc_item_expr(&mut grid, &name, &terms)
        {
            row_kinds_data.insert(at, PivotRowKind::CalcItem);
            row_indents_data.insert(at, 0);
            row_paths_data.insert(at, None);
        }
    }

    // 11. 空单元格与错误值替换
    if !opts.empty_cell_text.is_empty() || opts.error_cell_text.is_some() {
        for (ri, line) in grid.rows.iter_mut().enumerate() {
            let rk = row_kinds_data.get(ri).copied().unwrap_or(PivotRowKind::Detail);
            if matches!(rk, PivotRowKind::Detail | PivotRowKind::Subtotal(_) | PivotRowKind::GrandTotal) {
                for c in row_label_cols..line.len() {
                    match &line[c] {
                        Value::Empty if !opts.empty_cell_text.is_empty() => {
                            if let Ok(n) = opts.empty_cell_text.trim().parse::<f64>() {
                                line[c] = Value::Number(n);
                            } else {
                                line[c] = Value::Text(opts.empty_cell_text.clone().into());
                            }
                        }
                        Value::Text(t) if t.starts_with('#') => {
                            if let Some(rep) = &opts.error_cell_text {
                                line[c] = Value::Text(rep.clone().into());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // 12. 计算每列数值统计（供 Sparkbar 与异常高亮使用）
    let palette = opts.style_theme.palette();
    let mut value_col_stats = Vec::new();
    for gc in row_label_cols..total_cols {
        let Some(vi) = col_value_idx.get(gc).copied().flatten() else {
            continue;
        };
        let mut nums: Vec<f64> = Vec::new();
        for (ri, line) in grid.rows.iter().enumerate() {
            if row_kinds_data.get(ri) != Some(&PivotRowKind::Detail) {
                continue;
            }
            if let Some(Value::Number(n)) = line.get(gc)
                && n.is_finite()
            {
                nums.push(*n);
            }
        }
        if nums.is_empty() {
            continue;
        }
        nums.sort_by(|a, b| a.total_cmp(b));
        let min = nums[0];
        let max = *nums.last().unwrap();
        let max_abs = min.abs().max(max.abs());
        let p90 = nums[((nums.len() as f64 * 0.90) as usize).min(nums.len() - 1)];
        let vext = opts.val_ext.get(vi);
        let data_bar = opts.show_data_bars || vext.is_some_and(|e| e.data_bar);
        let bar_color = vext.and_then(|e| e.bar_color).unwrap_or(palette.bar_color);
        value_col_stats.push(ValueColStats {
            col: c0 + gc as u32,
            value_idx: vi,
            min,
            max,
            max_abs,
            p90,
            data_bar,
            bar_color,
            highlight_outliers: opts.highlight_outliers,
        });
    }

    meta.row_kinds.extend(row_kinds_data.iter().copied());
    meta.row_indents.extend(row_indents_data.iter().copied());
    meta.row_paths_by_grid_row.extend(row_paths_data);
    meta.col_paths_by_grid_col = col_paths_by_grid_col;
    meta.col_is_grand_total = col_is_grand_total;
    meta.col_is_subtotal = col_is_subtotal;
    meta.col_value_idx = col_value_idx;
    meta.value_col_stats = value_col_stats;
    let total_rows_u32 = meta.row_kinds.len().max(1) as u32;
    meta.rect = (
        r0,
        c0,
        r0 + total_rows_u32.saturating_sub(1),
        c0 + (total_cols.max(1) as u32).saturating_sub(1),
    );

    // 确定每个值槽的默认数字格式
    let value_formats: Vec<Option<String>> = (0..total_val_count)
        .map(|vi| {
            if let Some(fmt) = opts
                .val_ext
                .get(vi)
                .and_then(|e| e.num_format.as_ref())
                .filter(|s| !s.is_empty())
            {
                return Some(fmt.clone());
            }
            if let Some(vf) = spec.values.get(vi) {
                if vf.show_as.percent_format() {
                    return Some("0.0%".into());
                }
                if matches!(vf.show_as, ShowAs::RankAsc | ShowAs::RankDesc) {
                    return Some("0".into());
                }
                if vf.agg.is_count_like() {
                    return Some("#,##0".into());
                }
                if matches!(vf.show_as, ShowAs::Index) {
                    return Some("0.00".into());
                }
            }
            Some("#,##0.##".into())
        })
        .collect();

    ComputedPivot {
        grid,
        meta,
        row_kinds_data,
        row_indents_data,
        value_formats,
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_rich_show_as(
    grid: &mut PivotGrid,
    spec: &PivotSpec2,
    row_kinds: &[PivotRowKind],
    row_paths: &[Option<Vec<String>>],
    col_value_idx: &[Option<usize>],
    col_is_grand: &[bool],
    col_is_sub: &[bool],
    total_val_count: usize,
) {
    if total_val_count == 0 || grid.rows.is_empty() {
        return;
    }
    let orig = grid.rows.clone();
    let num_at = |r: usize, c: usize| -> Option<f64> {
        match orig.get(r).and_then(|row| row.get(c)) {
            Some(Value::Number(n)) if n.is_finite() => Some(*n),
            _ => None,
        }
    };
    let ratio = |num: f64, den: f64| -> Value {
        if den.abs() > f64::EPSILON {
            Value::Number(num / den)
        } else {
            Value::Empty
        }
    };

    for vi in 0..spec.values.len() {
        let mode = spec.values[vi].show_as;
        if mode == ShowAs::Normal {
            continue;
        }
        // 找到属于本值字段的所有列（含叶子列与总计列）
        let leaf_cols: Vec<usize> = col_value_idx
            .iter()
            .enumerate()
            .filter_map(|(c, v)| {
                (*v == Some(vi) && !col_is_grand[c] && !col_is_sub[c]).then_some(c)
            })
            .collect();
        let all_cols: Vec<usize> = col_value_idx
            .iter()
            .enumerate()
            .filter_map(|(c, v)| (*v == Some(vi)).then_some(c))
            .collect();
        if leaf_cols.is_empty() {
            continue;
        }

        // 统计每列总计（仅统计 Detail 行，避免小计重复叠加）
        let mut col_tot: HashMap<usize, f64> = HashMap::new();
        for &c in &all_cols {
            let s: f64 = (0..orig.len())
                .filter(|&r| row_kinds.get(r) == Some(&PivotRowKind::Detail))
                .filter_map(|r| num_at(r, c))
                .sum();
            col_tot.insert(c, s);
        }
        let grand_total: f64 = leaf_cols
            .iter()
            .map(|c| col_tot.get(c).copied().unwrap_or(0.0))
            .sum();

        // 父行分组总计（用于 PercentOfParentRow）
        let mut parent_row_tot: HashMap<(String, usize), f64> = HashMap::new();
        for r in 0..orig.len() {
            if row_kinds.get(r) != Some(&PivotRowKind::Detail) {
                continue;
            }
            let top = row_paths
                .get(r)
                .and_then(|o| o.as_ref())
                .and_then(|p| p.first())
                .cloned()
                .unwrap_or_default();
            for &c in &all_cols {
                if let Some(v) = num_at(r, c) {
                    *parent_row_tot.entry((top.clone(), c)).or_insert(0.0) += v;
                }
            }
        }

        // 每列 Detail 行排名表（用于 RankDesc / RankAsc）
        let mut rank_maps: HashMap<usize, HashMap<usize, usize>> = HashMap::new();
        if matches!(mode, ShowAs::RankDesc | ShowAs::RankAsc) {
            for &c in &all_cols {
                let mut pairs: Vec<(usize, f64)> = (0..orig.len())
                    .filter(|&r| row_kinds.get(r) == Some(&PivotRowKind::Detail))
                    .filter_map(|r| num_at(r, c).map(|v| (r, v)))
                    .collect();
                if mode == ShowAs::RankDesc {
                    pairs.sort_by(|a, b| b.1.total_cmp(&a.1));
                } else {
                    pairs.sort_by(|a, b| a.1.total_cmp(&b.1));
                }
                let mut map = HashMap::new();
                let mut cur_rank = 1usize;
                for (idx, (r, v)) in pairs.iter().enumerate() {
                    if idx > 0 && (v - pairs[idx - 1].1).abs() > 1e-12 {
                        cur_rank = idx + 1;
                    }
                    map.insert(*r, cur_rank);
                }
                rank_maps.insert(c, map);
            }
        }

        // 按行累计（用于 RunningTotal / PercentRunningTotal）
        let mut running_acc: HashMap<usize, f64> = HashMap::new();
        // 行基准查找（用于 DifferencePrev / PercentDifferencePrev 自定义基准项）
        let base_item_name = spec.values[vi].base.as_ref().map(|(_, s)| s.as_str());
        let base_row_idx = base_item_name.and_then(|target| {
            (0..orig.len()).find(|&r| {
                row_kinds.get(r) == Some(&PivotRowKind::Detail)
                    && row_paths
                        .get(r)
                        .and_then(|o| o.as_ref())
                        .is_some_and(|p| p.iter().any(|s| s == target))
            })
        });
        let mut prev_row_val: HashMap<usize, f64> = HashMap::new();

        for r in 0..orig.len() {
            let rk = row_kinds.get(r).copied().unwrap_or(PivotRowKind::Detail);
            let row_sum: f64 = leaf_cols.iter().filter_map(|&c| num_at(r, c)).sum();
            let top_key = row_paths
                .get(r)
                .and_then(|o| o.as_ref())
                .and_then(|p| p.first())
                .cloned()
                .unwrap_or_default();
            let mut prev_in_row: Option<f64> = None;

            for &c in &all_cols {
                let raw_opt = num_at(r, c);
                let Some(v) = raw_opt else {
                    continue;
                };
                let ctot = col_tot.get(&c).copied().unwrap_or(0.0);
                let new_val = match mode {
                    ShowAs::Normal => Value::Number(v),
                    ShowAs::PercentOfRow => ratio(v, row_sum),
                    ShowAs::PercentOfColumn => ratio(v, ctot),
                    ShowAs::PercentOfTotal => ratio(v, grand_total),
                    ShowAs::PercentOfParentRow => {
                        let p = parent_row_tot
                            .get(&(top_key.clone(), c))
                            .copied()
                            .unwrap_or(ctot);
                        ratio(v, p)
                    }
                    ShowAs::PercentOfParentCol => ratio(v, row_sum),
                    ShowAs::RunningTotal => {
                        if rk == PivotRowKind::Detail {
                            let acc = running_acc.entry(c).or_insert(0.0);
                            *acc += v;
                            Value::Number(*acc)
                        } else {
                            Value::Number(v)
                        }
                    }
                    ShowAs::PercentRunningTotal => {
                        if rk == PivotRowKind::Detail {
                            let acc = running_acc.entry(c).or_insert(0.0);
                            *acc += v;
                            ratio(*acc, ctot)
                        } else {
                            ratio(v, ctot)
                        }
                    }
                    ShowAs::RankDesc | ShowAs::RankAsc => {
                        if rk == PivotRowKind::Detail {
                            rank_maps
                                .get(&c)
                                .and_then(|m| m.get(&r))
                                .map(|&rk_num| Value::Number(rk_num as f64))
                                .unwrap_or(Value::Empty)
                        } else {
                            Value::Empty
                        }
                    }
                    ShowAs::DifferencePrev => {
                        let base_v = if let Some(br) = base_row_idx {
                            num_at(br, c)
                        } else if leaf_cols.len() > 1 {
                            prev_in_row
                        } else {
                            prev_row_val.get(&c).copied()
                        };
                        match base_v {
                            Some(b) => Value::Number(v - b),
                            None => Value::Empty,
                        }
                    }
                    ShowAs::PercentDifferencePrev => {
                        let base_v = if let Some(br) = base_row_idx {
                            num_at(br, c)
                        } else if leaf_cols.len() > 1 {
                            prev_in_row
                        } else {
                            prev_row_val.get(&c).copied()
                        };
                        match base_v {
                            Some(b) => ratio(v - b, b.abs()),
                            None => Value::Empty,
                        }
                    }
                    ShowAs::Index => ratio(v * grand_total, row_sum * ctot),
                };
                grid.rows[r][c] = new_val;
                if !col_is_grand[c] && !col_is_sub[c] {
                    prev_in_row = Some(v);
                }
                if rk == PivotRowKind::Detail {
                    prev_row_val.insert(c, v);
                }
            }
        }
    }
}

// =========================================================================
// 5. 专业视觉样式写入器（表头/小计/总计/斑马纹/缩进/数字格式/自动列宽）
// =========================================================================

pub fn write_table_styled(
    sheet: &mut Sheet,
    styles: &mut StyleTable,
    anchor: (u32, u32),
    computed: &ComputedPivot,
    model: &PivotTableModel,
) -> (u32, u32, u32, u32) {
    let (r0, c0) = anchor;
    let opts = &model.options;
    let pal = opts.style_theme.palette();
    let total_cols = computed
        .grid
        .headers
        .iter()
        .map(|h| h.len())
        .chain(computed.grid.rows.iter().map(|r| r.len()))
        .max()
        .unwrap_or(1);
    let label_cols = computed.meta.row_label_cols as usize;

    let thin_b = Border {
        style: 1,
        color: pal.border,
    };
    let med_b = Border {
        style: 2,
        color: pal.border,
    };
    let dbl_b = Border {
        style: 4,
        color: pal.border,
    };

    // 预建公用样式
    let filter_label_st = styles.intern(CellStyle {
        bold: true,
        color: pal.header_fg,
        fill: Some(pal.filter_bg),
        halign: 1,
        valign: 2,
        border: [thin_b, thin_b, thin_b, thin_b],
        ..CellStyle::default_dark()
    });
    let filter_val_st = styles.intern(CellStyle {
        color: [30, 41, 59],
        fill: Some([255, 255, 255]),
        halign: 1,
        valign: 2,
        border: [thin_b, thin_b, thin_b, thin_b],
        ..CellStyle::default_dark()
    });
    let header_left_st = styles.intern(CellStyle {
        bold: true,
        color: pal.header_fg,
        fill: Some(pal.header_bg),
        halign: 1,
        valign: 2,
        border: [Border::default(), Border::default(), thin_b, med_b],
        ..CellStyle::default_dark()
    });
    let header_right_st = styles.intern(CellStyle {
        bold: true,
        color: pal.header_fg,
        fill: Some(pal.header_bg),
        halign: 3,
        valign: 2,
        border: [Border::default(), Border::default(), thin_b, med_b],
        ..CellStyle::default_dark()
    });

    let filter_cells: HashSet<(u32, u32)> = computed
        .meta
        .filter_buttons
        .iter()
        .map(|b| (b.row, b.col))
        .collect();
    let collapse_cells: HashSet<(u32, u32)> = computed
        .meta
        .collapse_buttons
        .iter()
        .map(|b| (b.row, b.col))
        .collect();

    // 记录每列最大文本视觉宽度用于自动列宽
    let mut col_max_px: Vec<f32> = vec![76.0; total_cols];
    let measure_px = |s: &str, extra: f32| -> f32 {
        let mut w = 0.0f32;
        for ch in s.chars() {
            w += if ch.is_ascii() { 7.4 } else { 13.5 };
        }
        (w + 22.0 + extra).clamp(72.0, 260.0)
    };

    let mut r = r0;
    // 1. 页筛选区
    for (name, val) in &computed.grid.page_filters {
        sheet.set(
            r,
            c0,
            Cell {
                value: Value::Text(name.clone().into()),
                formula: None,
            },
        );
        sheet.set_style(r, c0, filter_label_st);
        col_max_px[0] = col_max_px[0].max(measure_px(name, 8.0));

        sheet.set(
            r,
            c0 + 1,
            Cell {
                value: Value::Text(val.clone().into()),
                formula: None,
            },
        );
        sheet.set_style(r, c0 + 1, filter_val_st);
        if total_cols > 1 {
            col_max_px[1] = col_max_px[1].max(measure_px(val, 22.0));
        }
        r += 1;
    }
    if !computed.grid.page_filters.is_empty() {
        r += 1;
    }

    // 2. 表头行
    for h in &computed.grid.headers {
        for ci in 0..total_cols {
            let text = h.get(ci).map(String::as_str).unwrap_or("");
            let has_flt = filter_cells.contains(&(r, c0 + ci as u32));
            if !text.is_empty() {
                sheet.set(
                    r,
                    c0 + ci as u32,
                    Cell {
                        value: Value::Text(text.into()),
                        formula: None,
                    },
                );
                let extra = if has_flt {
                    28.0
                } else if ci < label_cols {
                    22.0
                } else {
                    14.0
                };
                col_max_px[ci] = col_max_px[ci].max(measure_px(text, extra));
            } else {
                sheet.remove(r, c0 + ci as u32);
            }
            let st = if ci < label_cols || has_flt {
                header_left_st
            } else {
                header_right_st
            };
            sheet.set_style(r, c0 + ci as u32, st);
        }
        r += 1;
    }

    // 3. 数据行（按行类型 + 列类型 + 斑马纹 + 缩进 + 数字格式精准套样式）
    let mut style_cache: HashMap<(u8, bool, bool, u8, String), u16> = HashMap::new();
    let mut get_body_style = |kind_code: u8,
                              is_num_col: bool,
                              banded: bool,
                              indent: u8,
                              fmt: &str|
     -> u16 {
        let key = (kind_code, is_num_col, banded, indent, fmt.to_string());
        if let Some(&id) = style_cache.get(&key) {
            return id;
        }
        let mut cs = CellStyle::default_dark();
        cs.valign = 2;
        cs.halign = if is_num_col { 3 } else { 1 };
        cs.indent = indent;
        if is_num_col && !fmt.is_empty() {
            cs.format = fmt.to_string();
        }
        match kind_code {
            // 0 = Detail
            0 => {
                if banded {
                    cs.fill = Some(pal.banded_bg);
                }
            }
            // 1 = Subtotal / CalcItem
            1 => {
                cs.bold = true;
                cs.color = pal.subtotal_fg;
                cs.fill = Some(pal.subtotal_bg);
                cs.border[BorderSide::Top.idx()] = thin_b;
            }
            // 2 = GrandTotal row
            2 => {
                cs.bold = true;
                cs.color = pal.grand_fg;
                cs.fill = Some(pal.grand_bg);
                cs.border[BorderSide::Top.idx()] = thin_b;
                cs.border[BorderSide::Bottom.idx()] = dbl_b;
            }
            // 3 = GrandTotal col on detail row
            _ => {
                cs.bold = true;
                cs.color = pal.subtotal_fg;
                cs.fill = Some(if banded { pal.subtotal_bg } else { pal.banded_bg });
                cs.border[BorderSide::Left.idx()] = thin_b;
            }
        }
        let id = styles.intern(cs);
        style_cache.insert(key, id);
        id
    };

    let mut detail_seq = 0usize;
    for (ri, line) in computed.grid.rows.iter().enumerate() {
        let rk = computed
            .row_kinds_data
            .get(ri)
            .copied()
            .unwrap_or(PivotRowKind::Detail);
        let indent = computed.row_indents_data.get(ri).copied().unwrap_or(0);
        let is_banded_row = opts.banded_rows && rk == PivotRowKind::Detail && (detail_seq % 2 == 1);
        if rk == PivotRowKind::Detail {
            detail_seq += 1;
        }
        for ci in 0..total_cols {
            let v = line.get(ci).cloned().unwrap_or(Value::Empty);
            let is_num_col = ci >= label_cols;
            let is_grand_col = computed
                .meta
                .col_is_grand_total
                .get(ci)
                .copied()
                .unwrap_or(false);
            let is_sub_col = computed
                .meta
                .col_is_subtotal
                .get(ci)
                .copied()
                .unwrap_or(false);
            let fmt = if is_num_col {
                computed
                    .meta
                    .col_value_idx
                    .get(ci)
                    .copied()
                    .flatten()
                    .and_then(|vi| computed.value_formats.get(vi))
                    .and_then(|o| o.as_deref())
                    .unwrap_or("")
            } else {
                ""
            };
            let kind_code = match rk {
                PivotRowKind::GrandTotal => 2,
                PivotRowKind::Subtotal(_) | PivotRowKind::CalcItem => 1,
                _ if is_grand_col || is_sub_col => 3,
                _ => 0,
            };
            let col_band = opts.banded_cols && is_num_col && ((ci - label_cols) % 2 == 1);
            let has_collapse = collapse_cells.contains(&(r, c0 + ci as u32));
            let cell_indent = if ci == 0 {
                if has_collapse { indent.max(2) } else { indent }
            } else if has_collapse {
                2
            } else {
                0
            };
            let st_id = get_body_style(kind_code, is_num_col, is_banded_row || col_band, cell_indent, fmt);
            if matches!(v, Value::Empty) {
                sheet.remove(r, c0 + ci as u32);
            } else {
                let disp_len = match &v {
                    Value::Number(_) if !fmt.is_empty() => crate::style::format_value(&v, fmt),
                    other => other.display(),
                };
                let extra = if ci == 0 {
                    18.0 + cell_indent as f32 * 8.0
                } else {
                    12.0
                };
                col_max_px[ci] = col_max_px[ci].max(measure_px(&disp_len, extra));
                sheet.set(
                    r,
                    c0 + ci as u32,
                    Cell {
                        value: v,
                        formula: None,
                    },
                );
            }
            sheet.set_style(r, c0 + ci as u32, st_id);
        }
        r += 1;
    }

    if opts.autofit_cols {
        for (ci, &px) in col_max_px.iter().enumerate() {
            let cur = sheet.cols.size(c0 + ci as u32);
            if px > cur || (cur - px).abs() > 2.0 {
                sheet.cols.set(c0 + ci as u32, px);
            }
        }
        sheet.layout_dirty = true;
    }
    sheet.styles_dirty = true;
    sheet.bump_display_style_revision();

    (
        r0,
        c0,
        r.saturating_sub(1).max(r0),
        c0 + (total_cols as u32).saturating_sub(1),
    )
}

// =========================================================================
// 6. 双击下钻明细（提取构成任意汇总格的全部源数据明细行）
// =========================================================================

pub fn drill_down_rows(
    source_sheet: &Sheet,
    model: &PivotTableModel,
    drill: &DrillFilter,
) -> Vec<Vec<Value>> {
    let spec = &model.spec;
    let opts = &model.options;
    let (r1, c1, r2, c2) = spec.source;
    let width = (c2.saturating_sub(c1) + 1) as usize;

    let mut out = Vec::new();
    // 表头行
    let mut header_row = Vec::with_capacity(width);
    for c in c1..=c2 {
        header_row.push(Value::Text(header_name(source_sheet, r1, c).into()));
    }
    out.push(header_row);

    let compiled_page_filters: Vec<(u32, FxHashSet<&str>)> = spec
        .filters
        .iter()
        .filter(|f| !f.keep.is_empty())
        .map(|f| (f.col, f.keep.iter().map(String::as_str).collect()))
        .collect();
    let row_hidden: Vec<FxHashSet<&str>> = spec
        .rows
        .iter()
        .enumerate()
        .map(|(i, _)| {
            opts.row_ext
                .get(i)
                .map(|e| e.hidden_items.iter().map(String::as_str).collect())
                .unwrap_or_default()
        })
        .collect();
    let col_hidden: Vec<FxHashSet<&str>> = spec
        .cols
        .iter()
        .enumerate()
        .map(|(i, _)| {
            opts.col_ext
                .get(i)
                .map(|e| e.hidden_items.iter().map(String::as_str).collect())
                .unwrap_or_default()
        })
        .collect();

    let first_row = r1.saturating_add(1);
    if first_row > r2 {
        return out;
    }
    for r in first_row..=r2 {
        let mut any = false;
        for c in c1..=c2 {
            if !matches!(source_sheet.value_ref(r, c).as_ref(), Value::Empty) {
                any = true;
                break;
            }
        }
        if !any {
            continue;
        }
        let mut ok = true;
        for (fc, allowed) in &compiled_page_filters {
            let disp = normalize_blank(source_sheet.value_ref(r, *fc).display());
            if !allowed.contains(disp.as_str()) {
                ok = false;
                break;
            }
        }
        if !ok {
            continue;
        }
        for (i, rf) in spec.rows.iter().enumerate() {
            let k = normalize_blank(group_key(source_sheet.value_ref(r, rf.col).as_ref(), &rf.group));
            if !row_hidden[i].is_empty() && row_hidden[i].contains(k.as_str()) {
                ok = false;
                break;
            }
            if !drill.is_grand_row
                && let Some(target) = drill.row_path.get(i)
                && &k != target
            {
                ok = false;
                break;
            }
        }
        if !ok {
            continue;
        }
        for (i, cf) in spec.cols.iter().enumerate() {
            let k = normalize_blank(group_key(source_sheet.value_ref(r, cf.col).as_ref(), &cf.group));
            if !col_hidden[i].is_empty() && col_hidden[i].contains(k.as_str()) {
                ok = false;
                break;
            }
            if !drill.is_grand_col
                && let Some(target) = drill.col_path.get(i)
                && &k != target
            {
                ok = false;
                break;
            }
        }
        if !ok {
            continue;
        }
        let mut row_vals = Vec::with_capacity(width);
        for c in c1..=c2 {
            row_vals.push(source_sheet.value_ref(r, c).into_owned());
        }
        out.push(row_vals);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Sheet, Value};

    fn sample_sales_sheet() -> Sheet {
        let mut sh = Sheet::new("销售明细");
        let headers = ["日期", "区域", "品类", "客户", "金额", "数量"];
        for (c, h) in headers.iter().enumerate() {
            sh.set(0, c as u32, Cell { value: Value::Text((*h).into()), formula: None });
        }
        let rows: &[(&str, &str, &str, &str, f64, f64)] = &[
            ("2026-01-10", "华东", "手机", "甲公司", 100.0, 2.0),
            ("2026-01-18", "华东", "电脑", "乙公司", 300.0, 3.0),
            ("2026-02-05", "华东", "手机", "甲公司", 200.0, 4.0),
            ("2026-02-20", "华北", "手机", "丙公司", 500.0, 5.0),
            ("2026-04-12", "华北", "电脑", "丙公司", 400.0, 2.0),
            ("2026-05-01", "华南", "平板", "丁公司", 50.0, 1.0),
        ];
        for (i, &(dt, reg, cat, cust, amt, qty)) in rows.iter().enumerate() {
            let r = i as u32 + 1;
            sh.set(r, 0, Cell { value: Value::Text(dt.into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Text(reg.into()), formula: None });
            sh.set(r, 2, Cell { value: Value::Text(cat.into()), formula: None });
            sh.set(r, 3, Cell { value: Value::Text(cust.into()), formula: None });
            sh.set(r, 4, Cell { value: Value::Number(amt), formula: None });
            sh.set(r, 5, Cell { value: Value::Number(qty), formula: None });
        }
        sh
    }

    #[test]
    fn engine_supports_value_sort_topn_distinct_median_calc_field_collapse_and_drilldown() {
        let sh = sample_sales_sheet();
        let spec = PivotSpec2 {
            source: (0, 0, 6, 5),
            rows: vec![AxisField::plain(1), AxisField::plain(2)],
            cols: vec![],
            values: vec![
                ValueField { col: 4, agg: Agg::Sum, label: None, show_as: ShowAs::Normal, base: None },
                ValueField { col: 3, agg: Agg::DistinctCount, label: Some("客户数".into()), show_as: ShowAs::Normal, base: None },
                ValueField { col: 4, agg: Agg::Median, label: Some("中位额".into()), show_as: ShowAs::Normal, base: None },
            ],
            filters: vec![],
            layout: PivotLayout::default(),
        };
        let mut model = PivotTableModel::new("透视1".into(), "销售明细".into(), Some(sh.identity), 1, (0, 0), spec);
        model.options.row_ext[0].sort = FieldSortMode::ValueDesc(0);
        model.options.row_ext[0].top_n = Some(TopNFilter { kind: TopNKind::Top, count: 2, value_idx: 0 });
        model.options.calc_fields.push(("单价".into(), "金额/数量".into()));

        let res = compute_table(&sh, &model);
        // Top 2 区域按金额降序：华北 (900) 在前，华东 (600) 其次，华南 (50) 被 Top 2 滤掉！
        assert_eq!(res.grid.rows[0][0], Value::Text("华北".into()));
        // 华北汇总行应在 2 条品类明细后
        let huabei_sub = res.grid.rows.iter().find(|r| r[0].display() == "华北 汇总").unwrap();
        assert_eq!(huabei_sub[2], Value::Number(900.0));
        assert_eq!(huabei_sub[3], Value::Number(1.0)); // 丙公司非重复计数=1
        assert_eq!(huabei_sub[4], Value::Number(450.0)); // (500+400)/2 = 450
        // 计算字段 单价 = 900 / 7
        if let Value::Number(u) = huabei_sub[5] {
            assert!((u - 900.0 / 7.0).abs() < 1e-9);
        } else {
            panic!("单价计算字段应为数值");
        }
        assert!(!res.grid.rows.iter().any(|r| r[0].display().starts_with("华南")));

        // 折叠「华北」后，华北只占 1 行，且有 [+] 按钮
        model.options.collapsed_rows.push(vec!["华北".into()]);
        let res_col = compute_table(&sh, &model);
        assert_eq!(res_col.grid.rows[0][0], Value::Text("华北".into()));
        assert_eq!(res_col.grid.rows[0][2], Value::Number(900.0));
        assert!(res_col.meta.collapse_buttons.iter().any(|b| b.path == vec!["华北".to_string()] && b.collapsed));

        // 对折叠的华北数据格下钻明细：应返回表头 + 华北的 2 条原始明细
        let drill = res_col.meta.drill_filter_at(1, 2).expect("数据格应支持下钻");
        let detail = drill_down_rows(&sh, &model, &drill);
        assert_eq!(detail.len(), 3); // 1 表头 + 2 行华北明细
        assert_eq!(detail[1][1], Value::Text("华北".into()));
        assert_eq!(detail[2][1], Value::Text("华北".into()));
    }

    #[test]
    fn engine_supports_quarter_week_running_total_rank_and_smart_presets() {
        let sh = sample_sales_sheet();
        let styles = StyleTable::new();
        let profiles = profile_source_fields(&sh, &styles, (0, 0, 6, 5));
        assert_eq!(profiles[0].kind, FieldKind::Date);
        assert_eq!(profiles[1].kind, FieldKind::Text);
        assert_eq!(profiles[4].kind, FieldKind::Numeric);

        let presets = suggest_presets((0, 0, 6, 5), &profiles);
        assert!(presets.len() >= 2, "应自动生成多个推荐透视预设");

        // 季度分组 + 累计汇总 + 降序排名
        let spec = PivotSpec2 {
            source: (0, 0, 6, 5),
            rows: vec![AxisField { col: 0, group: GroupBy::Quarter }],
            cols: vec![],
            values: vec![
                ValueField { col: 4, agg: Agg::Sum, label: None, show_as: ShowAs::RunningTotal, base: None },
                ValueField { col: 4, agg: Agg::Sum, label: Some("排名".into()), show_as: ShowAs::RankDesc, base: None },
            ],
            filters: vec![],
            layout: PivotLayout::default(),
        };
        let model = PivotTableModel::new("季度".into(), "销售明细".into(), None, 0, (10, 0), spec);
        let res = compute_table(&sh, &model);
        // Q1 = 100+300+200+500 = 1100; Q2 = 400+50 = 450 -> RunningTotal: 1100, 1550; RankDesc: 1, 2
        assert_eq!(res.grid.rows[0][0], Value::Text("2026-Q1".into()));
        assert_eq!(res.grid.rows[0][1], Value::Number(1100.0));
        assert_eq!(res.grid.rows[0][2], Value::Number(1.0));
        assert_eq!(res.grid.rows[1][0], Value::Text("2026-Q2".into()));
        assert_eq!(res.grid.rows[1][1], Value::Number(1550.0));
        assert_eq!(res.grid.rows[1][2], Value::Number(2.0));
    }
}
