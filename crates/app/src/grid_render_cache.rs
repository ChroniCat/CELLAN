use eframe::egui::{self, Color32, Mesh, Pos2, Rect, Shape, Vec2};
use sheetcore::{
    cf,
    merges::MergeIndex,
    model::DateSystem,
    page::PageSetup,
    store::CellVersion,
    style::Border,
    Sheet, Workbook,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{mpsc, Arc};

pub(crate) type MergeRect = (u32, u32, u32, u32);
pub(crate) type CfViewportCache = (
    (
        usize,
        u64,
        u64,
        u32,
        Option<u32>,
        u32,
        Option<u32>,
        u32,
        u32,
        (u64, Option<u32>),
        (u64, Option<u32>),
    ),
    Arc<HashMap<(u32, u32), cf::Overlay>>,
    HashSet<(u32, u32)>,
);

/// 缓存未乘 `zoom`（`z = 1.0`）但已合入隐藏行列的基础坐标轴：
/// - 隐藏行列直接通过区间段表 `Axis::hide_hidden` 在 `O(spans + intervals)` 内归并，零 `Vec<u32>` 展开；
/// - 连续滚轮缩放仅对已有 `Axis` 段表做 `O(spans)` 线性缩放；
/// - 拖拽单行行高（`hidden_rows` 版本未变）且该行非隐藏时，直接在缓存轴上增量更新该行。
pub(crate) struct BaseAxesCache {
    pub sheet_id: u64,
    pub cols_rev: u64,
    pub rows_rev: u64,
    pub hidden_cols_rev: u64,
    pub hidden_rows_rev: (u64, u64),
    pub base_cols: Arc<sheetcore::layout::Axis>,
    pub base_rows: Arc<sheetcore::layout::Axis>,
}

#[derive(Clone, PartialEq)]
pub(crate) struct FormulaBarKey {
    pub sheet_id: u64,
    pub row: u32,
    pub col: u32,
    pub doc_gen: u64,
    pub wb_epoch: u64,
    pub cells: CellVersion,
    pub style_rev: u64,
    pub protected: bool,
}

pub(crate) struct PrintPreviewCache {
    pub key: (
        u64,
        u64,
        CellVersion,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        PageSetup,
        Option<std::path::PathBuf>,
    ),
    pub layout: Arc<sheetcore::print::Layout>,
    pub page_ops: Option<(usize, Arc<sheetcore::print::Out>)>,
}

pub(crate) struct Pivot2BaseItemsCache {
    pub key: (u64, u64, CellVersion, (u32, u32, u32, u32), u32),
    pub items: Arc<Vec<String>>,
}

pub(super) struct PageBreaks {
    source: Option<(u64, Option<(u32, u32, u32, u32)>, u64, u64, PageSetup)>,
    lines: Arc<(Vec<f32>, Vec<f32>)>,
    /// 大批量「全部替换」在局域网协同模式下的按帧流式广播队列 `(sheet_idx, keys, cursor)`。
    pub(crate) replace_broadcast: VecDeque<(usize, Vec<(u32, u32)>, usize)>,
    /// 系统字体按需扫描缓存键 `(doc_uid, styles_len)`，避免无新增样式时每 30 帧重复遍历样式表。
    pub(crate) last_font_scan: Option<(u64, usize)>,
    /// 连续 `Ctrl+滚轮` 缩放防抖截止时刻：滚动中字号光栅化按 5% 档位量化，停顿 90ms 后精确定位，防止撑爆字形图集。
    pub(crate) wheel_zoom_until: Option<std::time::Instant>,
    /// 未缩放基础坐标轴缓存（供 `&self` 的 `view_axes()` 内部可变复用）。
    pub(crate) base_axes: std::cell::RefCell<Option<BaseAxesCache>>,
    /// 空闲态公式栏文本缓存（消除空闲/悬停帧重复 `display_formula` 与 `Value::display` 分配）。
    pub(crate) formula_bar: Option<(FormulaBarKey, Arc<str>)>,
    /// 打印预览分页与当前页绘制指令缓存（按表修订号与页面设置失效，避免每帧重算 `paginate` / `page_ops`）。
    pub(crate) print_preview: Option<PrintPreviewCache>,
    /// 多维透视对话框基准项去重列表缓存（按源表版本与列号失效，避免每帧全列扫描）。
    pub(crate) pivot2_base_items: Option<Pivot2BaseItemsCache>,
    /// 并排比较副窗格持久复用的 `Rules`（防止每帧 `std::mem::take` 重建与销毁副窗格缓存和后台统计线程）。
    pub(crate) compare_cf_inputs: Option<Box<Rules>>,
    /// 并排比较副窗格持久复用的条件格式统计与视口缓存（消除每帧重置 `cf_stats_gen = u64::MAX`）。
    pub(crate) compare_cf_stats: cf::CfStats,
    pub(crate) compare_cf_gen: u64,
    pub(crate) compare_cf_cache: Option<CfViewportCache>,
}

impl Default for PageBreaks {
    fn default() -> Self {
        Self {
            source: None,
            lines: Arc::new((Vec::new(), Vec::new())),
            replace_broadcast: VecDeque::new(),
            last_font_scan: None,
            wheel_zoom_until: None,
            base_axes: std::cell::RefCell::new(None),
            formula_bar: None,
            print_preview: None,
            pivot2_base_items: None,
            compare_cf_inputs: None,
            compare_cf_stats: cf::CfStats::default(),
            compare_cf_gen: u64::MAX,
            compare_cf_cache: None,
        }
    }
}

impl PageBreaks {
    pub(crate) fn reset_compare_cache(&mut self) {
        self.compare_cf_inputs = None;
        self.compare_cf_stats = cf::CfStats::default();
        self.compare_cf_gen = u64::MAX;
        self.compare_cf_cache = None;
    }
    pub fn get(&mut self, sheet: &Sheet) -> Arc<(Vec<f32>, Vec<f32>)> {
        // Large explicit print areas are clamped to used bounds by page_breaks.
        let area = sheet.used_bounds();
        let key = (
            sheet.identity,
            area,
            sheet.rows.revision(),
            sheet.cols.revision(),
        );
        if self.source.as_ref().is_none_or(|(id, area, rows, cols, setup)| {
            (*id, *area, *rows, *cols) != key || setup != &sheet.page
        }) {
            self.lines = Arc::new(sheetcore::page::page_breaks(sheet));
            self.source = Some((key.0, key.1, key.2, key.3, sheet.page.clone()));
        }
        self.lines.clone()
    }

    /// 计算当前帧用于字体光栅化的有效缩放比 `(raster_zoom, is_debouncing)`。
    pub(crate) fn effective_font_zoom(&mut self, zoom: f32) -> (f32, bool) {
        let z = zoom.clamp(0.5, 2.5);
        if let Some(until) = self.wheel_zoom_until {
            if std::time::Instant::now() < until {
                let quantized = ((z * 20.0).round() / 20.0).clamp(0.5, 2.5);
                return (quantized, (quantized - z).abs() > 0.0005);
            }
            self.wheel_zoom_until = None;
        }
        (z, false)
    }
}

// ===== 合并单元格 O(1) 版本门控 + O(log M) 区间查询与缓存最大合并行 =====

type MergeKey = (u64, u64);

#[derive(Default)]
pub(crate) struct MergeCache {
    key: Option<MergeKey>,
    pub index: MergeIndex,
    sorted_by_top: Vec<MergeRect>,
    prefix_max_bottom: Vec<u32>,
    max_row: u32,
    max_col: u32,
}

impl MergeCache {
    pub fn ensure(&mut self, sheet: &Sheet, _doc_gen: u64, _wb_epoch: u64) {
        let merges = &sheet.merges;
        let k: MergeKey = (sheet.identity, sheet.merges_revision());
        if self.key == Some(k) {
            return;
        }
        self.index.refresh_merges(merges);
        self.sorted_by_top.clear();
        self.sorted_by_top.extend_from_slice(merges);
        self.sorted_by_top.sort_unstable_by_key(|m| m.0);
        self.prefix_max_bottom.clear();
        self.prefix_max_bottom.reserve(self.sorted_by_top.len());
        let mut max_r = 0u32;
        let mut max_c = 0u32;
        for &(_, _, r2, c2) in &self.sorted_by_top {
            max_r = max_r.max(r2);
            max_c = max_c.max(c2);
            self.prefix_max_bottom.push(max_r);
        }
        self.max_row = max_r;
        self.max_col = max_c;
        self.key = Some(k);
    }

    #[inline]
    pub fn matches(&self, sheet: &Sheet) -> bool {
        self.key == Some((sheet.identity, sheet.merges_revision()))
    }

    #[inline]
    pub fn at(&self, r: u32, c: u32) -> Option<MergeRect> {
        if self.sorted_by_top.is_empty() {
            return None;
        }
        self.index.at(r, c)
    }

    #[inline]
    pub fn max_row(&self) -> u32 {
        self.max_row
    }

    #[inline]
    pub fn max_col(&self) -> u32 {
        self.max_col
    }

    /// O(log M + k) 判定矩形 `[r1..=r2, c1..=c2]` 是否与任一合并块相交。
    pub fn has_overlap(&self, r1: u32, c1: u32, r2: u32, c2: u32) -> bool {
        if self.sorted_by_top.is_empty() {
            return false;
        }
        let start = self.prefix_max_bottom.partition_point(|&end| end < r1);
        let end = self.sorted_by_top.partition_point(|m| m.0 <= r2);
        for &(mr1, mc1, mr2, mc2) in &self.sorted_by_top[start..end] {
            if mr1 <= r2 && mr2 >= r1 && mc1 <= c2 && mc2 >= c1 {
                return true;
            }
        }
        false
    }

    /// 用行区间二分替代全表 `sheet.merges.iter().filter(...)` 线性扫描，提取当前视口可见合并块。
    pub fn collect_visible(
        &self,
        body_rows: &[u32],
        frozen_rows: &[u32],
        body_cols: &[u32],
        frozen_cols: &[u32],
        out: &mut Vec<MergeRect>,
    ) {
        out.clear();
        if self.sorted_by_top.is_empty() {
            return;
        }
        let intersects = |indices: &[u32], start: u32, end: u32| {
            indices
                .get(indices.partition_point(|&i| i < start))
                .is_some_and(|&i| i <= end)
        };
        let mut bands: [(u32, u32); 2] = [(u32::MAX, 0); 2];
        let mut n_bands = 0usize;
        if let (Some(&f), Some(&l)) = (frozen_rows.first(), frozen_rows.last()) {
            bands[n_bands] = (f, l);
            n_bands += 1;
        }
        if let (Some(&f), Some(&l)) = (body_rows.first(), body_rows.last()) {
            if n_bands == 1 && f <= bands[0].1.saturating_add(1) {
                bands[0].1 = bands[0].1.max(l);
            } else {
                bands[n_bands] = (f, l);
                n_bands += 1;
            }
        }
        let mut last_idx = usize::MAX;
        for &(r_min, r_max) in &bands[..n_bands] {
            let start = self.prefix_max_bottom.partition_point(|&end| end < r_min);
            let end = self.sorted_by_top.partition_point(|m| m.0 <= r_max);
            for idx in start..end {
                if last_idx != usize::MAX && idx <= last_idx {
                    continue;
                }
                let m @ (r1, c1, r2, c2) = self.sorted_by_top[idx];
                if (intersects(body_rows, r1, r2) || intersects(frozen_rows, r1, r2))
                    && (intersects(body_cols, c1, c2) || intersects(frozen_cols, c1, c2))
                {
                    out.push(m);
                }
            }
            if end > 0 {
                last_idx = last_idx.max(end - 1);
            }
        }
    }
}

// ===== 迷你图数据点缓存（按 `(sheet_id, doc_gen, wb_epoch, cell_version)` 失效） =====

#[derive(Default)]
pub(crate) struct SparkCache {
    sheet_id: Option<u64>,
    values: HashMap<
        (u32, u32),
        (
            Option<(u64, CellVersion)>,
            (u32, u32, u32, u32),
            Arc<[Option<f64>]>,
        ),
    >,
}

impl SparkCache {
    pub fn get_or_load(
        &mut self,
        wb: &Workbook,
        sheet_idx: usize,
        _doc_gen: u64,
        _wb_epoch: u64,
        sp: &sheetcore::sparkline::Sparkline,
    ) -> Arc<[Option<f64>]> {
        let sh = wb.sheet(sheet_idx);
        if self.sheet_id != Some(sh.identity) {
            self.values.clear();
            self.sheet_id = Some(sh.identity);
        }
        let src_ver = match sp.source_sheet.as_deref() {
            Some(name) => wb
                .sheets
                .iter()
                .find(|s| s.name == name)
                .map(|s| (s.identity, s.cell_version().clone())),
            None => Some((sh.identity, sh.cell_version().clone())),
        };
        if let Some((cached_ver, cached_rect, v)) = self.values.get(&(sp.r, sp.c)) {
            if *cached_ver == src_ver && *cached_rect == sp.rect {
                return v.clone();
            }
        }
        let raw: Arc<[Option<f64>]> = crate::spark_view::values(wb, sheet_idx, sp).into();
        if self.values.len() > 4096 {
            self.values.clear();
        }
        self.values
            .insert((sp.r, sp.c), (src_ver, sp.rect, raw.clone()));
        raw
    }
}

// ===== 保留式单元格排版与视口网格缓存（Tier 1: 单元级 Galley/格式；Tier 2: 整视口 Mesh/Shapes） =====

#[derive(Clone, PartialEq)]
pub(crate) struct CellCacheEpoch {
    pub sheet_id: u64,
    pub doc_gen: u64,
    pub wb_epoch: u64,
    pub cells: CellVersion,
    pub style_rev: u64,
    pub cf_gen: u64,
    pub cols_rev: u64,
    pub rows_rev: u64,
    pub hidden_cols_rev: u64,
    pub font_zoom_bits: u32,
    pub layout_zoom_bits: u32,
    pub paint_scale_bits: u32,
    pub show_formulas: bool,
    pub text_spill: bool,
    pub fonts_gen: u64,
    pub tables_fp: u64,
    pub sparklines_len: usize,
}

impl CellCacheEpoch {
    /// 结构/布局/样式/缩放/条件格式等全局前提是否一致（允许 `doc_gen` / `cells` 内容变化走增量失效）。
    #[inline]
    pub fn same_layout(&self, other: &Self) -> bool {
        self.sheet_id == other.sheet_id
            && self.wb_epoch == other.wb_epoch
            && self.style_rev == other.style_rev
            && self.cf_gen == other.cf_gen
            && self.cols_rev == other.cols_rev
            && self.rows_rev == other.rows_rev
            && self.hidden_cols_rev == other.hidden_cols_rev
            && self.font_zoom_bits == other.font_zoom_bits
            && self.layout_zoom_bits == other.layout_zoom_bits
            && self.paint_scale_bits == other.paint_scale_bits
            && self.show_formulas == other.show_formulas
            && self.text_spill == other.text_spill
            && self.fonts_gen == other.fonts_gen
            && self.tables_fp == other.tables_fp
            && self.sparklines_len == other.sparklines_len
    }
}

#[inline]
pub(crate) fn value_eq_fast(a: &sheetcore::Value, b: &sheetcore::Value) -> bool {
    use sheetcore::Value;
    match (a, b) {
        (Value::Empty, Value::Empty) => true,
        (Value::Number(x), Value::Number(y)) => x.to_bits() == y.to_bits(),
        (Value::Text(x), Value::Text(y)) => Arc::ptr_eq(x, y) || x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Error(x), Value::Error(y)) => x == y,
        _ => false,
    }
}

#[inline]
pub(crate) fn cell_eq_fast(a: Option<&sheetcore::Cell>, b: Option<&sheetcore::Cell>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            if !value_eq_fast(&x.value, &y.value) {
                return false;
            }
            match (&x.formula, &y.formula) {
                (None, None) => true,
                (Some(fx), Some(fy)) => Arc::ptr_eq(fx, fy) || fx == fy,
                _ => false,
            }
        }
        _ => false,
    }
}

#[derive(Clone)]
pub(crate) enum PreparedText {
    Empty,
    Single {
        galley: Arc<egui::Galley>,
        offset: Vec2,
        color: Color32,
        fits: bool,
        spill_lr: (f32, f32),
        halign: u8,
        span_extra_w: f32,
        underline: u8, // 0=none, 1=single, 2=double
        strikethrough: bool,
        link_underline: Option<(f32, f32, f32)>, // (dx0_from_rect_min, dy_from_rect_min, width)
        rotation: i16,
    },
    Multiline {
        galley: Arc<egui::Galley>,
        offset: Vec2,
        color: Color32,
        fits: bool,
        underline: u8,
        strikethrough: bool,
    },
    Rich {
        runs: Arc<[(f32, f32, Arc<egui::Galley>, Color32)]>,
        spill_lr: (f32, f32),
        underline: u8,
        color: Color32,
    },
    Vertical {
        chars: Arc<[(f32, f32, Arc<egui::Galley>)]>,
        color: Color32,
        spill_lr: (f32, f32),
    },
    Distributed {
        chars: Arc<[(f32, f32, Arc<egui::Galley>)]>,
        color: Color32,
        spill_lr: (f32, f32),
        underline: u8,
        strikethrough: bool,
        text_x0: f32,
        text_y0: f32,
        text_w: f32,
        text_h: f32,
    },
}

#[derive(Clone)]
pub(crate) struct PreparedCell {
    pub width_bits: u32,
    pub height_bits: u32,
    pub verified_rev: u64,
    pub cell_snap: Option<sheetcore::Cell>,
    pub has_link: bool,
    pub has_rich: bool,
    pub depends_on_row_neighbors: bool,
    pub bg_fill: Option<Color32>,
    pub cf_bar: Option<(f32, Color32)>,
    pub cf_borders: Option<Box<[Option<(f32, Color32)>; 4]>>,
    pub diagonal: Option<(u8, Color32, bool, bool)>,
    pub borders: [Border; 4],
    pub cf_icon: Option<cf::IconMark>,
    pub spark_idx: Option<usize>,
    pub text: PreparedText,
}

#[derive(Clone, PartialEq)]
pub(crate) struct ViewportCacheKey {
    pub cell_epoch: CellCacheEpoch,
    pub scroll_x_bits: u32,
    pub scroll_y_bits: u32,
    pub origin_x_bits: u32,
    pub origin_y_bits: u32,
    pub avail_x_bits: u32,
    pub avail_y_bits: u32,
    pub head_w_bits: u32,
    pub hh_bits: u32,
    pub fx: u32,
    pub fy: u32,
    pub page_mode: bool,
    pub view_mode: u8,
    pub show_gridlines: bool,
    pub ec_background: bool,
    pub ec_color: [u8; 3],
    pub error_marks_fp: (usize, u64),
    pub locks_fp: (Option<u64>, usize, u64),
    pub notes_len: usize,
    pub filter_fp: Option<((u32, u32, u32, u32), u64)>,
    pub page_geo_fp: Option<(usize, usize, [u32; 7], usize, usize)>,
}

#[derive(Default, Clone)]
pub(crate) struct RetainedViewport {
    pub key: Option<ViewportCacheKey>,
    pub body_batches: Vec<(Rect, Vec<Shape>)>,
    pub frozen_batches: Vec<(Rect, Vec<Shape>)>,
    pub extra_sparks: Vec<(Rect, Rect, usize)>,
    pub extra_icons: Vec<(Rect, Rect, cf::IconMark)>,
}

/// 录制象限绘制输出：
/// - 象限主裁剪区（`primary`）全量合批为单个 `(quad_clip, Vec<Shape>)`，不会因中间出现少数溢出裁剪单元格而碎裂成上百个 Scissor 批次；
/// - 仅真正超出单格/溢出宽度的少量文本落入 `batches` 独立裁剪；顶层筛选漏斗等覆盖物落入 `overlays` 保持在文本之上。
#[derive(Default)]
pub(crate) struct BatchRecorder {
    pub primary: Option<(Rect, Vec<Shape>)>,
    pub batches: Vec<(Rect, Vec<Shape>)>,
    pub overlays: Vec<(Rect, Vec<Shape>)>,
}

impl BatchRecorder {
    #[inline]
    pub fn push(&mut self, clip: Rect, shape: Shape) {
        if clip.width() <= 0.0 || clip.height() <= 0.0 {
            return;
        }
        if let Some((last_clip, list)) = self.batches.last_mut() {
            if *last_clip == clip {
                list.push(shape);
                return;
            }
        }
        self.batches.push((clip, vec![shape]));
    }

    /// 录制象限内单元格文本/装饰图元：凡裁剪矩形等于 `quad_clip` 的图元全部归入同一 `primary` 合批桶，
    /// 彻底消除穿插溢出格导致的 DX12 Scissor DrawCall 乒乓切换。
    #[inline]
    pub fn push_quadrant(&mut self, quad_clip: Rect, clip: Rect, shape: Shape) {
        if clip.width() <= 0.0 || clip.height() <= 0.0 {
            return;
        }
        if clip == quad_clip {
            match &mut self.primary {
                Some((p_clip, list)) if *p_clip == quad_clip => {
                    list.push(shape);
                    return;
                }
                None => {
                    self.primary = Some((quad_clip, vec![shape]));
                    return;
                }
                _ => {}
            }
        }
        self.push(clip, shape);
    }

    #[inline]
    pub fn push_overlay(&mut self, clip: Rect, shape: Shape) {
        if clip.width() <= 0.0 || clip.height() <= 0.0 {
            return;
        }
        if let Some((last_clip, list)) = self.overlays.last_mut() {
            if *last_clip == clip {
                list.push(shape);
                return;
            }
        }
        self.overlays.push((clip, vec![shape]));
    }

    pub fn flush_into(&mut self, out: &mut BatchRecorder) {
        if let Some((clip, shapes)) = self.primary.take() {
            for s in shapes {
                out.push(clip, s);
            }
        }
        for (clip, shapes) in std::mem::take(&mut self.batches) {
            for s in shapes {
                out.push(clip, s);
            }
        }
        for (clip, shapes) in std::mem::take(&mut self.overlays) {
            for s in shapes {
                out.push(clip, s);
            }
        }
    }

    pub fn replay_into(&self, painter: &egui::Painter) {
        if let Some((clip, shapes)) = &self.primary {
            if !shapes.is_empty() {
                painter.with_clip_rect(*clip).extend(shapes.iter().cloned());
            }
        }
        for (clip, shapes) in &self.batches {
            if !shapes.is_empty() {
                painter.with_clip_rect(*clip).extend(shapes.iter().cloned());
            }
        }
        for (clip, shapes) in &self.overlays {
            if !shapes.is_empty() {
                painter.with_clip_rect(*clip).extend(shapes.iter().cloned());
            }
        }
    }
}

/// 象限内自定义单元格边框的共享边去重与共线段合并累加器：
/// - 相邻格共享边按 Excel 线型权重去重，避免每条内边被画两次；
/// - 同一水平/垂直轴线上连续同 `(style, color)` 的线段合并为一条长段，统一写入单个 `egui::Mesh`。
#[derive(Default)]
pub(crate) struct QuadrantBorderBuilder {
    // key: y_q -> Vec<(x1_q, x2_q, Border)>
    horiz: HashMap<i32, Vec<(i32, i32, Border)>>,
    // key: x_q -> Vec<(y1_q, y2_q, Border)>
    vert: HashMap<i32, Vec<(i32, i32, Border)>>,
}

impl QuadrantBorderBuilder {
    #[inline]
    fn q(v: f32) -> i32 {
        (v * 4.0).round() as i32
    }
    #[inline]
    fn uq(v: i32) -> f32 {
        v as f32 * 0.25
    }

    pub fn add_cell_borders(&mut self, rect: Rect, bd: &[Border; 4]) {
        let (l, t, r, b) = (
            Self::q(rect.left()),
            Self::q(rect.top()),
            Self::q(rect.right()),
            Self::q(rect.bottom()),
        );
        if bd[0].style != 0 && b > t {
            self.vert.entry(l).or_default().push((t, b, bd[0]));
        }
        if bd[1].style != 0 && b > t {
            self.vert.entry(r).or_default().push((t, b, bd[1]));
        }
        if bd[2].style != 0 && r > l {
            self.horiz.entry(t).or_default().push((l, r, bd[2]));
        }
        if bd[3].style != 0 && r > l {
            self.horiz.entry(b).or_default().push((l, r, bd[3]));
        }
    }

    fn emit_axis_runs(lines: &mut HashMap<i32, Vec<(i32, i32, Border)>>, horizontal: bool, mesh: &mut Mesh) {
        let mut keys: Vec<i32> = lines.keys().copied().collect();
        keys.sort_unstable();
        for fixed in keys {
            let Some(segs) = lines.get_mut(&fixed) else { continue };
            if segs.is_empty() {
                continue;
            }
            // 按起点排序：对完全重合的共享边按 border_rank 去重，对首尾相接且同样式的边直接延长合并
            segs.sort_unstable_by_key(|s| (s.0, s.1));
            let mut deduped: Vec<(i32, i32, Border)> = Vec::with_capacity(segs.len());
            for &(s0, s1, bd) in segs.iter() {
                if let Some(last) = deduped.last_mut() {
                    if last.0 == s0 && last.1 == s1 {
                        if crate::border_lines::border_rank(&bd)
                            >= crate::border_lines::border_rank(&last.2)
                        {
                            last.2 = bd;
                        }
                        continue;
                    }
                }
                deduped.push((s0, s1, bd));
            }
            let mut cur = deduped[0];
            for &(s0, s1, bd) in &deduped[1..] {
                if s0 == cur.1 && bd.style == cur.2.style && bd.color == cur.2.color {
                    cur.1 = s1;
                } else {
                    Self::append_seg(mesh, fixed, cur.0, cur.1, &cur.2, horizontal);
                    cur = (s0, s1, bd);
                }
            }
            Self::append_seg(mesh, fixed, cur.0, cur.1, &cur.2, horizontal);
        }
    }

    #[inline]
    fn append_seg(mesh: &mut Mesh, fixed_q: i32, a_q: i32, b_q: i32, bd: &Border, horizontal: bool) {
        let f = Self::uq(fixed_q);
        let a = Self::uq(a_q);
        let b = Self::uq(b_q);
        let (p0, p1) = if horizontal {
            (egui::pos2(a, f), egui::pos2(b, f))
        } else {
            (egui::pos2(f, a), egui::pos2(f, b))
        };
        let col = Color32::from_rgb(bd.color[0], bd.color[1], bd.color[2]);
        crate::border_lines::append_to_mesh(mesh, p0, p1, bd.style, col, 1.0);
    }

    pub fn finish(mut self) -> Option<Arc<Mesh>> {
        if self.horiz.is_empty() && self.vert.is_empty() {
            return None;
        }
        let mut mesh = Mesh::default();
        Self::emit_axis_runs(&mut self.horiz, true, &mut mesh);
        Self::emit_axis_runs(&mut self.vert, false, &mut mesh);
        (!mesh.is_empty()).then(|| Arc::new(mesh))
    }
}

#[derive(Default)]
pub(crate) struct RetainedGridCache {
    pub epoch: Option<CellCacheEpoch>,
    /// 内容修订代数：仅单格/小批量值改动时递增而不整表 `cells.clear()`，由渲染命中时按 `cell_snap` 增量校验。
    pub content_rev: u64,
    pub cells: HashMap<(u32, u32), PreparedCell>,
    /// 行内非空列有序缓存（加速 `spill_clip` 左右邻居探测，避免重复访问 `Store` 页哈希表）。
    pub row_occupied_cols: HashMap<u32, Vec<u32>>,
    pub viewport: RetainedViewport,
}

impl RetainedGridCache {
    #[inline]
    pub fn invalidate_viewport(&mut self) {
        self.viewport.key = None;
        self.viewport.body_batches.clear();
        self.viewport.frozen_batches.clear();
        self.viewport.extra_sparks.clear();
        self.viewport.extra_icons.clear();
    }

    /// 局部淘汰单格/小批量脏格及其同行依赖溢出的邻格，保留其余未改动单元格的 `Galley`。
    pub fn mark_cells_dirty(&mut self, cells: impl IntoIterator<Item = (u32, u32)>) {
        let mut dirty_rows: Vec<u32> = Vec::new();
        for (r, c) in cells {
            self.cells.remove(&(r, c));
            if !dirty_rows.contains(&r) {
                dirty_rows.push(r);
            }
        }
        if dirty_rows.is_empty() {
            return;
        }
        for &r in &dirty_rows {
            self.row_occupied_cols.remove(&r);
        }
        self.cells.retain(|&(r, _), cell| {
            !(cell.depends_on_row_neighbors && dirty_rows.contains(&r))
        });
        self.invalidate_viewport();
    }

    pub fn ensure_epoch(&mut self, epoch: &CellCacheEpoch) {
        if self.epoch.as_ref() == Some(epoch) {
            return;
        }
        let same_layout = self.epoch.as_ref().is_some_and(|prev| prev.same_layout(epoch));
        if same_layout {
            // 仅值/公式变化：保留未改动单元的 `PreparedCell`，递增 `content_rev` 在命中时按单元增量校验。
            self.content_rev = self.content_rev.wrapping_add(1);
            self.row_occupied_cols.clear();
            self.invalidate_viewport();
            self.epoch = Some(epoch.clone());
        } else {
            self.content_rev = self.content_rev.wrapping_add(1);
            self.cells.clear();
            self.row_occupied_cols.clear();
            self.invalidate_viewport();
            self.epoch = Some(epoch.clone());
        }
    }

    /// 按视口行带就近保留淘汰：当长距离滚动使保留单元超过阈值时，保留当前视口上下 ±128 行及冻结行，
    /// 彻底消除 `cells.clear()` 全清造成的周期性整屏重排帧尖峰。
    pub fn prune_distant_rows(&mut self, view_r1: u32, view_r2: u32, frozen_fy: u32) {
        const MAX_RETAINED_CELLS: usize = 24_576;
        if self.cells.len() <= MAX_RETAINED_CELLS {
            return;
        }
        let keep_r1 = view_r1.saturating_sub(128);
        let keep_r2 = view_r2.saturating_add(128);
        self.cells
            .retain(|&(r, _), _| r < frozen_fy || (r >= keep_r1 && r <= keep_r2));
        self.row_occupied_cols
            .retain(|&r, _| r < frozen_fy || (r >= keep_r1 && r <= keep_r2));
        if self.cells.len() > MAX_RETAINED_CELLS {
            self.cells
                .retain(|&(r, _), _| r < frozen_fy || (r >= view_r1 && r <= view_r2));
            self.row_occupied_cols
                .retain(|&r, _| r < frozen_fy || (r >= view_r1 && r <= view_r2));
        }
    }

    #[inline]
    pub fn row_has_cell(&mut self, sheet: &Sheet, r: u32, c: u32) -> bool {
        let cols = self.row_occupied_cols.entry(r).or_insert_with(|| {
            let mut v: Vec<u32> = sheet
                .iter_rect((r, 0, r, 16_383))
                .map(|(k, _)| k.col)
                .collect();
            if !v.is_sorted() {
                v.sort_unstable();
            }
            v
        });
        cols.binary_search(&c).is_ok()
    }
}

// ===== 大范围条件格式统计后台异步 Worker（防止百万行色阶/数据条单格编辑卡死渲染帧） =====

struct CfStatsJob {
    sheet_id: u64,
    cells: CellVersion,
    cf: cf::CfSheet,
    snapshot: Sheet,
    wake: egui::Context,
}

struct CfStatsDone {
    sheet_id: u64,
    cells: CellVersion,
    cf: cf::CfSheet,
    stats: cf::CfStats,
}

struct CfStatsWorker {
    tx: mpsc::Sender<CfStatsJob>,
    rx: mpsc::Receiver<CfStatsDone>,
    in_flight: Option<(u64, CellVersion)>,
}

impl CfStatsWorker {
    fn new() -> Self {
        let (tx, jobs) = mpsc::channel::<CfStatsJob>();
        let (done_tx, rx) = mpsc::channel::<CfStatsDone>();
        std::thread::spawn(move || {
            // 持久保留最近 16 张表的 CfStats，避免每次新建 default() 导致 cf.rs 的 slice_cache 100% 失效。
            const MAX_SHEETS: usize = 16;
            let mut per_sheet: HashMap<u64, cf::CfStats> = HashMap::new();
            let mut lru: VecDeque<u64> = VecDeque::new();
            while let Ok(mut job) = jobs.recv() {
                // 若主线程连续编辑，跳过已被取代的排队快照，只算最新一版
                while let Ok(newer) = jobs.try_recv() {
                    job = newer;
                }
                job.snapshot.cf = job.cf.clone();
                let sheet_id = job.sheet_id;
                if !per_sheet.contains_key(&sheet_id) {
                    if lru.len() >= MAX_SHEETS {
                        if let Some(evict) = lru.pop_front() {
                            per_sheet.remove(&evict);
                        }
                    }
                    lru.push_back(sheet_id);
                } else if lru.back().copied() != Some(sheet_id) {
                    if let Some(pos) = lru.iter().position(|&id| id == sheet_id) {
                        lru.remove(pos);
                    }
                    lru.push_back(sheet_id);
                }
                let stats = per_sheet.entry(sheet_id).or_default();
                cf::refresh_stats(&job.snapshot, stats);
                let out_stats = stats.clone();
                let wake = job.wake.clone();
                if done_tx
                    .send(CfStatsDone {
                        sheet_id,
                        cells: job.cells,
                        cf: job.cf,
                        stats: out_stats,
                    })
                    .is_err()
                {
                    break;
                }
                wake.request_repaint();
            }
        });
        Self {
            tx,
            rx,
            in_flight: None,
        }
    }
}

fn cf_stat_area(cf_sheet: &cf::CfSheet) -> u64 {
    let mut area = 0u64;
    for b in &cf_sheet.blocks {
        let needs_stats = b.rules.iter().any(|r| {
            matches!(
                r.kind,
                cf::CfKind::Top { .. }
                    | cf::CfKind::Avg { .. }
                    | cf::CfKind::Scale { .. }
                    | cf::CfKind::IconSet { .. }
                    | cf::CfKind::Bar { .. }
                    | cf::CfKind::Dup { .. }
            )
        });
        if needs_stats {
            for &(r1, c1, r2, c2) in &b.ranges {
                area = area.saturating_add(
                    u64::from(r2.saturating_sub(r1) + 1) * u64::from(c2.saturating_sub(c1) + 1),
                );
            }
        }
    }
    area
}

fn expr_may_cross_sheet(formula: &str) -> bool {
    formula.contains('!')
}

#[derive(Default)]
pub(super) struct Rules {
    pub index: cf::RuleIndex,
    pub generation: u64,
    sheet: Option<(u64, CellVersion, DateSystem)>,
    observed: (u64, u64),
    definitions: cf::CfSheet,
    expressions: bool,
    cross_expressions: bool,
    stat_area: u64,
    cf_worker: Option<CfStatsWorker>,
    // 挂接在 `self.cf_inputs` 下的保留式网格渲染缓存（`draw_compare_pane` 切换时自动隔离主/副窗格缓存）
    pub(crate) merges: MergeCache,
    pub(crate) sparks: SparkCache,
    pub(crate) grid: RetainedGridCache,
    /// 图表局部坐标系 `(0, 0, w, h)` 形状缓存，键为 `(chart_idx, [w_bits, h_bits])`，滚动时 100% 命中。
    pub(crate) charts: HashMap<(usize, [u32; 2]), Arc<[Shape]>>,
    pub(crate) charts_epoch: Option<(u64, u64, u64, u64)>,
}

/// Result of `Rules::refresh_responsive`: distinguishes a full cache wipe (definitions,
/// statistical aggregates like ColorScale/DataBar, or formula CF rules changed) from a
/// localized cell-value edit where only the edited cell's per-cell CF rule needs refreshing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CfRefreshOutcome {
    Unchanged,
    /// Only cell values changed while CF definitions/stats/formulas stayed stable:
    /// do NOT bump `cf_gen` or wipe whole-screen `RetainedGridCache`!
    CellOnly,
    /// Definitions, stats (min/max/percentiles/duplicates), or expression rules changed:
    /// invalidate screen CF cache and bump `cf_gen`.
    Full,
}

impl Rules {
    #[inline]
    pub fn has_definitions(&self) -> bool {
        !self.definitions.blocks.is_empty()
    }
    pub fn refresh(
        &mut self,
        sh: &Sheet,
        dates: DateSystem,
        observed: (u64, u64),
        forced: bool,
        stats: &mut cf::CfStats,
    ) -> bool {
        let local_changed = self.sheet.as_ref().is_none_or(|(id, cells, sys)| {
            *id != sh.identity || cells != sh.cell_version() || *sys != dates
        });
        if !local_changed && !forced && self.observed == observed {
            return false;
        }
        let definitions_changed = self.definitions != sh.cf;
        if definitions_changed {
            self.definitions = sh.cf.clone();
            self.index = cf::RuleIndex::new(&sh.cf);
            self.expressions = sh
                .cf
                .blocks
                .iter()
                .flat_map(|b| &b.rules)
                .any(|r| matches!(r.kind, cf::CfKind::Expr { .. }));
            self.cross_expressions = sh.cf.blocks.iter().flat_map(|b| &b.rules).any(|r| match &r.kind {
                cf::CfKind::Expr { formula, .. } => expr_may_cross_sheet(formula),
                _ => false,
            });
            self.stat_area = cf_stat_area(&sh.cf);
        }
        let dirty = local_changed || definitions_changed || self.expressions;
        if dirty {
            if local_changed || definitions_changed || forced {
                cf::refresh_stats(sh, stats);
            }
            self.generation = self.generation.wrapping_add(1);
        }
        self.sheet = Some((sh.identity, sh.cell_version().clone(), dates));
        self.observed = observed;
        dirty
    }

    /// 面向 UI 渲染帧的响应式条件格式刷新：
    /// - 对纯本表引用表达式（无 `!` 且工作簿无自定义名称）在跨表编辑时不白白清空本表 `cf_cache`；
    /// - 当大表（`sh.len() > 16_384` 且统计区域 `> 16_384` 格）发生编辑时，将全表 `cf::refresh_stats`
    ///   移交后台线程计算并保留上一帧统计量过渡，彻底消除百万行色阶/数据条单格编辑卡死渲染帧。
    pub fn refresh_responsive(
        &mut self,
        sh: &Sheet,
        dates: DateSystem,
        observed: (u64, u64),
        forced: bool,
        has_wb_names: bool,
        dumping: bool,
        ctx: &egui::Context,
        stats: &mut cf::CfStats,
    ) -> CfRefreshOutcome {
        let mut async_applied = false;
        if let Some(w) = self.cf_worker.as_mut() {
            while let Ok(done) = w.rx.try_recv() {
                if done.sheet_id == sh.identity
                    && done.cells == *sh.cell_version()
                    && done.cf == sh.cf
                {
                    if !stats.visual_eq(&done.stats) {
                        *stats = done.stats;
                        self.generation = self.generation.wrapping_add(1);
                        async_applied = true;
                    }
                    w.in_flight = None;
                }
            }
        }
        let local_changed = self.sheet.as_ref().is_none_or(|(id, cells, sys)| {
            *id != sh.identity || cells != sh.cell_version() || *sys != dates
        });
        if !local_changed && !forced && self.observed == observed {
            return if async_applied {
                CfRefreshOutcome::Full
            } else {
                CfRefreshOutcome::Unchanged
            };
        }
        let definitions_changed = self.definitions != sh.cf;
        if definitions_changed {
            self.definitions = sh.cf.clone();
            self.index = cf::RuleIndex::new(&sh.cf);
            self.expressions = sh
                .cf
                .blocks
                .iter()
                .flat_map(|b| &b.rules)
                .any(|r| matches!(r.kind, cf::CfKind::Expr { .. }));
            self.cross_expressions = sh.cf.blocks.iter().flat_map(|b| &b.rules).any(|r| match &r.kind {
                cf::CfKind::Expr { formula, .. } => expr_may_cross_sheet(formula),
                _ => false,
            });
            self.stat_area = cf_stat_area(&sh.cf);
        }
        let has_rules = !sh.cf.blocks.is_empty();
        let ext_expr = self.cross_expressions || (self.expressions && has_wb_names);
        let mut stats_changed = false;
        if has_rules && (local_changed || definitions_changed || forced) && self.stat_area > 0 {
            let offload = !dumping && sh.len() > 16_384 && self.stat_area > 16_384;
            if offload {
                let target = (sh.identity, sh.cell_version().clone());
                let worker = self.cf_worker.get_or_insert_with(CfStatsWorker::new);
                if worker.in_flight.as_ref() != Some(&target) {
                    let job = CfStatsJob {
                        sheet_id: sh.identity,
                        cells: sh.cell_version().clone(),
                        cf: sh.cf.clone(),
                        snapshot: sh.search_snapshot(),
                        wake: ctx.clone(),
                    };
                    if worker.tx.send(job).is_ok() {
                        worker.in_flight = Some(target);
                    } else {
                        self.cf_worker = None;
                        stats_changed = cf::refresh_stats(sh, stats);
                    }
                }
            } else {
                stats_changed = cf::refresh_stats(sh, stats);
            }
        }
        let full_dirty = definitions_changed
            || forced
            || async_applied
            || (has_rules && (stats_changed || self.expressions || ext_expr));
        if full_dirty {
            self.generation = self.generation.wrapping_add(1);
        }
        self.sheet = Some((sh.identity, sh.cell_version().clone(), dates));
        self.observed = observed;
        if full_dirty {
            CfRefreshOutcome::Full
        } else if has_rules && local_changed {
            CfRefreshOutcome::CellOnly
        } else {
            CfRefreshOutcome::Unchanged
        }
    }

    pub(crate) fn chart_shapes(
        &mut self,
        painter: &egui::Painter,
        sh: &Sheet,
        doc_gen: u64,
        wb_epoch: u64,
        fonts_gen: u64,
        ci: usize,
        rect: Rect,
        ch: &sheetcore::charts::Chart,
    ) -> Arc<[Shape]> {
        self.chart_shapes_in(
            painter,
            sh,
            doc_gen,
            wb_epoch,
            fonts_gen,
            ci,
            rect,
            ch,
            DateSystem::D1900,
        )
    }

    pub(crate) fn chart_shapes_in(
        &mut self,
        painter: &egui::Painter,
        sh: &Sheet,
        _doc_gen: u64,
        wb_epoch: u64,
        fonts_gen: u64,
        ci: usize,
        rect: Rect,
        ch: &sheetcore::charts::Chart,
        date_system: DateSystem,
    ) -> Arc<[Shape]> {
        let mut fp = charts_fingerprint(&sh.charts);
        if date_system == DateSystem::D1904 {
            fp ^= 0x9e37_79b9_7f4a_7c15;
        }
        let epoch = (sh.identity, wb_epoch, fonts_gen, fp);
        if self.charts_epoch != Some(epoch) {
            self.charts.clear();
            self.charts_epoch = Some(epoch);
        }
        let rkey = [rect.width().to_bits(), rect.height().to_bits()];
        if let Some(cached) = self.charts.get(&(ci, rkey)) {
            return cached.clone();
        }
        let local_rect = Rect::from_min_size(Pos2::ZERO, rect.size());
        let built: Arc<[Shape]> =
            crate::chart_view::build_chart_shapes_in(painter, local_rect, ch, date_system).into();
        if self.charts.len() > 256 {
            self.charts.clear();
        }
        self.charts.insert((ci, rkey), built.clone());
        built
    }
}

/// 把局部坐标系 `(0, 0, w, h)` 缓存的图表 `Shape` 平移到屏幕位置 `delta = rect.min.to_vec2()` 绘制。
#[inline]
pub(crate) fn paint_translated_shapes(painter: &egui::Painter, shapes: &[Shape], delta: Vec2) {
    if shapes.is_empty() {
        return;
    }
    if delta == Vec2::ZERO {
        painter.extend(shapes.iter().cloned());
        return;
    }
    painter.extend(shapes.iter().map(|s| {
        let mut s = s.clone();
        s.translate(delta);
        s
    }));
}

pub(crate) fn tables_fingerprint(tables: &[sheetcore::table::Table]) -> u64 {
    use std::hash::{Hash, Hasher};
    if tables.is_empty() {
        return 0;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tables.len().hash(&mut h);
    for t in tables {
        t.range.hash(&mut h);
        t.header_row.hash(&mut h);
        t.totals_row.hash(&mut h);
    }
    h.finish()
}

pub(crate) fn filter_fingerprint(
    filter: Option<&sheetcore::sort_filter::Filter>,
) -> Option<((u32, u32, u32, u32), u64)> {
    let f = filter?;
    let mut acc = 0u64;
    for &c in f.allows.keys() {
        let mut x = (u64::from(c) << 1) | 1;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        acc ^= x;
    }
    for &c in f.criteria.keys() {
        let mut x = (u64::from(c) << 1) | 2;
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        acc ^= x;
    }
    Some((f.range, acc))
}

pub(crate) fn charts_fingerprint(charts: &[sheetcore::charts::Chart]) -> u64 {
    use std::hash::{Hash, Hasher};
    if charts.is_empty() {
        return 0;
    }
    let hash_axis = |ax: &sheetcore::charts::AxisConfig, h: &mut std::collections::hash_map::DefaultHasher| {
        ax.title.hash(h);
        ax.min.map(f64::to_bits).hash(h);
        ax.max.map(f64::to_bits).hash(h);
        ax.major_unit.map(f64::to_bits).hash(h);
        ax.log_base.map(f64::to_bits).hash(h);
        ax.reverse.hash(h);
        ax.num_fmt.hash(h);
        ax.show_major_grid.hash(h);
        ax.show_minor_grid.hash(h);
        ax.hidden.hash(h);
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    charts.len().hash(&mut h);
    for ch in charts {
        std::mem::discriminant(&ch.kind).hash(&mut h);
        ch.title.hash(&mut h);
        ch.subtitle.hash(&mut h);
        ch.legend.hash(&mut h);
        ch.legend_pos.hash(&mut h);
        ch.anchor.hash(&mut h);
        ch.show_labels.hash(&mut h);
        ch.label_opts.show_cat.hash(&mut h);
        ch.label_opts.show_ser.hash(&mut h);
        ch.label_opts.show_pct.hash(&mut h);
        ch.label_opts.show_legend_key.hash(&mut h);
        ch.label_opts.leader_lines.hash(&mut h);
        ch.label_opts.position.hash(&mut h);
        ch.label_opts.num_fmt.hash(&mut h);
        ch.radar_style.hash(&mut h);
        ch.axis_x.hash(&mut h);
        ch.axis_y.hash(&mut h);
        ch.y_fmt.hash(&mut h);
        ch.y_min.map(f64::to_bits).hash(&mut h);
        ch.y_max.map(f64::to_bits).hash(&mut h);
        ch.style.theme.hash(&mut h);
        ch.style.rounded_bars.hash(&mut h);
        ch.style.show_card_shadow.hash(&mut h);
        ch.style.transparent_bg.hash(&mut h);
        ch.style.gap_width.hash(&mut h);
        ch.style.hole_size.hash(&mut h);
        ch.style.first_slice_ang.hash(&mut h);
        ch.style.pie_explosion.hash(&mut h);
        ch.style.plot_vis_only.hash(&mut h);
        ch.style.blank_policy.hash(&mut h);
        ch.style.switch_row_col.hash(&mut h);
        hash_axis(&ch.x_axis, &mut h);
        hash_axis(&ch.y_axis, &mut h);
        hash_axis(&ch.y2_axis, &mut h);
        ch.analytics.highlight_extrema.hash(&mut h);
        ch.analytics.show_growth_badge.hash(&mut h);
        ch.analytics.show_summary_bar.hash(&mut h);
        ch.analytics.show_pareto_line.hash(&mut h);
        ch.analytics.hist_bins.hash(&mut h);
        ch.analytics.waterfall_totals.hash(&mut h);
        ch.analytics.center_kpi_title.hash(&mut h);
        ch.analytics.show_center_kpi.hash(&mut h);
        ch.ref_lines.len().hash(&mut h);
        for rl in &ch.ref_lines {
            rl.label.hash(&mut h);
            rl.axis.hash(&mut h);
            std::mem::discriminant(&rl.mode).hash(&mut h);
            match rl.mode {
                sheetcore::charts::RefLineMode::Fixed(v)
                | sheetcore::charts::RefLineMode::Percentile(v) => v.to_bits().hash(&mut h),
                sheetcore::charts::RefLineMode::Band { low, high } => {
                    low.to_bits().hash(&mut h);
                    high.to_bits().hash(&mut h);
                }
                _ => {}
            }
            rl.color.hash(&mut h);
            rl.dash.hash(&mut h);
        }
        ch.series.len().hash(&mut h);
        for s in &ch.series {
            s.name.hash(&mut h);
            s.color.hash(&mut h);
            s.kind.hash(&mut h);
            s.secondary.hash(&mut h);
            s.smooth.hash(&mut h);
            s.stepped.hash(&mut h);
            s.marker.hash(&mut h);
            s.line_width.to_bits().hash(&mut h);
            s.dash.hash(&mut h);
            s.hidden.hash(&mut h);
            s.trendlines.len().hash(&mut h);
            for tl in &s.trendlines {
                tl.kind.hash(&mut h);
                tl.forward.to_bits().hash(&mut h);
                tl.backward.to_bits().hash(&mut h);
                tl.show_equation.hash(&mut h);
                tl.show_r2.hash(&mut h);
                tl.color.hash(&mut h);
                tl.dash.hash(&mut h);
            }
            s.error_bars.is_some().hash(&mut h);
            if let Some(eb) = &s.error_bars {
                std::mem::discriminant(&eb.kind).hash(&mut h);
                match eb.kind {
                    sheetcore::charts::ErrorBarKind::Fixed(v)
                    | sheetcore::charts::ErrorBarKind::Percent(v)
                    | sheetcore::charts::ErrorBarKind::StdDev(v) => v.to_bits().hash(&mut h),
                    sheetcore::charts::ErrorBarKind::StdErr => {}
                }
                eb.dir.hash(&mut h);
                eb.color.hash(&mut h);
            }
            (s.vals.as_ptr() as usize).hash(&mut h);
            s.vals.len().hash(&mut h);
            if let Some(first) = s.vals.first() {
                first.to_bits().hash(&mut h);
            }
            if let Some(mid) = s.vals.get(s.vals.len() / 2) {
                mid.to_bits().hash(&mut h);
            }
            if let Some(last) = s.vals.last() {
                last.to_bits().hash(&mut h);
            }
            (s.xv.as_ptr() as usize).hash(&mut h);
            s.xv.len().hash(&mut h);
            (s.sizes.as_ptr() as usize).hash(&mut h);
            s.sizes.len().hash(&mut h);
            (s.cats.as_ptr() as usize).hash(&mut h);
            s.cats.len().hash(&mut h);
        }
    }
    h.finish()
}

#[inline]
pub(crate) fn error_marks_fingerprint(marks: &std::collections::HashSet<(u32, u32)>) -> (usize, u64) {
    if marks.is_empty() {
        return (0, 0);
    }
    // 轻量无序多重集指纹（仅在 `marks.len() <= 4096` 时线性混合，大集合用容量与首素特征快速区分）
    let mut acc = 0u64;
    if marks.len() <= 4096 {
        for &(r, c) in marks {
            let mut x = (u64::from(r) << 32) | u64::from(c);
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            acc ^= x ^ (x >> 31);
        }
    } else {
        for &(r, c) in marks.iter().take(64) {
            acc = acc.wrapping_add((u64::from(r) << 32) | u64::from(c));
        }
    }
    (marks.len(), acc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{egui, office_clipboard, Clip, SheetApp};
    use sheetcore::{Cell, Value};
    use std::sync::Arc;

    #[test]
    fn page_lines_follow_clamped_used_bounds_and_axis_changes_without_refreshing_on_value_edits() {
        let mut sheet = Sheet::new("pages");
        sheet.page.print_area = Some((0, 0, 999_999, 2));
        let mut pages = PageBreaks::default();
        let number = |n| Cell {
            value: Value::Number(n),
            formula: None,
        };
        sheet.set(0, 0, number(1.0));
        let first = pages.get(&sheet);
        sheet.set(0, 0, number(2.0));
        assert!(Arc::ptr_eq(&first, &pages.get(&sheet)));
        sheet.set(999, 1, number(2.0));
        let extended = pages.get(&sheet);
        assert!(!Arc::ptr_eq(&first, &extended));
        assert_eq!(*extended, sheetcore::page::page_breaks(&sheet));
        sheet.rows.set(8, 56.0);
        assert!(!Arc::ptr_eq(&extended, &pages.get(&sheet)));
    }

    #[test]
    fn conditional_rule_inputs_keep_unrelated_generations_and_honour_external_expressions() {
        let mut sh = Sheet::new("rules");
        sh.cf.blocks.push(cf::CfBlock {
            ranges: vec![(1, 2, 4, 2), (3, 5, 7, 5)],
            rules: vec![cf::CfRule {
                kind: cf::CfKind::Text {
                    contains: "X".into(),
                    dxf: 0,
                },
                stop: true,
                priority: 1,
            }],
        });
        sh.cf.dxfs.push(cf::Dxf {
            fill: Some([1, 2, 3]),
            ..Default::default()
        });
        sh.set(
            3,
            5,
            Cell {
                value: Value::Text("X".into()),
                formula: None,
            },
        );
        let mut rules = Rules::default();
        let mut stats = cf::CfStats::default();
        assert!(rules.refresh(&sh, DateSystem::default(), (1, 1), true, &mut stats));
        let gen0 = rules.generation;
        assert!(!rules.refresh(&sh, DateSystem::default(), (2, 3), true, &mut stats));
        assert_eq!(rules.generation, gen0);
        let row = rules.index.row(&sh.cf, 3);
        assert_eq!(row.overlay_at(&stats, &sh, 5, |_, _, _| false).fill, Some([1, 2, 3]));
        assert!(row.overlay_at(&stats, &sh, 4, |_, _, _| false).is_empty());
        sh.cf.blocks[0].rules[0].kind = cf::CfKind::Expr {
            formula: "Other!B3".into(),
            dxf: 0,
        };
        assert!(rules.refresh(&sh, DateSystem::default(), (4, 3), false, &mut stats));
        assert!(rules.refresh(&sh, DateSystem::default(), (5, 3), false, &mut stats));
    }

    #[test]
    fn full_column_width_paste_uses_copy_time_dimensions_and_late_column_locks() {
        let _env = crate::test_env::TestEnv::new("full-column-widths");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        let rect = (0, 0, 1_048_575, 2);
        app.wb.sheets[0].cols.set_many([(0, 74.0), (1, 90.0), (2, 66.0)]);
        let range = Arc::new(office_clipboard::delayed::Range::new(&app.wb, 0, rect));
        app.clip = Some(Clip {
            sheet: 0,
            rect,
            deferred: Some(range),
            cells: vec![],
            merges: vec![],
            source_rows: None,
            cut: false,
        });
        app.wb.sheets[0].cols.set(1, 160.0);
        app.ps_kind = 5;
        app.sel = (80, 6);
        app.paste_special_apply_text(&ctx, None);
        assert_eq!(
            (6..=8)
                .map(|c| app.wb.sheets[0].cols.size(c))
                .collect::<Vec<_>>(),
            vec![74.0, 90.0, 66.0]
        );
        app.sel = (0, 9);
        app.locks.request(0, 999_999, 10, 2, "peer", app.lock_now());
        app.paste_special_apply_text(&ctx, None);
        assert!(app.status.contains("peer"), "{}", app.status);
        assert_eq!(app.wb.sheets[0].cols.size(9), app.wb.sheets[0].cols.size(12));
    }

    #[test]
    fn shared_cell_borders_deduplicate_and_merge_collinear_runs() {
        let mut builder = QuadrantBorderBuilder::default();
        let thin = Border {
            style: 1,
            color: [0, 0, 0],
        };
        let all_thin = [thin; 4];
        // 10 个水平相邻格：上下边应各自合并为 1 条连续实线（4 顶点），中间 9 条垂直共享边各自只画 1 次
        for c in 0..10 {
            let x = c as f32 * 72.0;
            let rect = Rect::from_min_size(Pos2::new(x, 0.0), Vec2::new(72.0, 20.0));
            builder.add_cell_borders(rect, &all_thin);
        }
        let mesh = builder.finish().expect("non-empty border mesh");
        // 2 merged horizontal spans + 11 unique vertical edges = 13 quads = 52 vertices (vs 40 edges = 160 before!)
        assert_eq!(mesh.vertices.len(), 13 * 4);
    }

    #[test]
    fn dense_grid_probe_reports_tessellated_meshes_and_triangles() {
        let _env = crate::test_env::TestEnv::new("grid-mesh-probe");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        crate::install_fonts(&ctx);
        let mut st = app.wb.styles.get(0).clone();
        st.fill = Some([215, 230, 245]);
        let style = app.wb.styles.intern(st);
        for r in 0..36 {
            for c in 0..14 {
                app.wb.sheets[0].set(
                    r,
                    c,
                    Cell {
                        value: Value::Text("abc".into()),
                        formula: None,
                    },
                );
                if (r + c) % 2 == 0 {
                    app.wb.sheets[0].set_style(r, c, style);
                }
            }
        }
        for frame in 0..5 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            };
            let start = std::time::Instant::now();
            let output = ctx.run(raw, |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
            });
            let build = start.elapsed();
            let now = std::time::Instant::now();
            let meshes = ctx.tessellate(output.shapes, output.pixels_per_point);
            let tess = now.elapsed();
            let triangles: usize = meshes
                .iter()
                .map(|m| match &m.primitive {
                    egui::epaint::Primitive::Mesh(m) => m.indices.len() / 3,
                    _ => 0,
                })
                .sum();
            if frame == 4 {
                eprintln!(
                    "grid 1200x800, 36x14 filled cells: build={build:?} tessellation={tess:?}, meshes={}, triangles={triangles}",
                    meshes.len()
                );
                assert!(
                    triangles > 100 && meshes.len() < 250,
                    "short fitting text should batch across cells"
                );
            }
        }
    }

    #[test]
    fn million_hidden_rows_navigation_and_jump_edge_stay_exact() {
        let _env = crate::test_env::TestEnv::new("million-hidden-nav");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.last_view = egui::vec2(1000.0, 720.0);
        {
            let sh = app.wb.sheet_mut(0);
            sh.rows.set_many((1..=600_000).map(|r| (r, 0.0)));
            sh.set(
                0,
                0,
                Cell {
                    value: Value::Text("hdr".into()),
                    formula: None,
                },
            );
            sh.set(
                600_001,
                0,
                Cell {
                    value: Value::Number(1.0),
                    formula: None,
                },
            );
            sh.set(
                600_002,
                0,
                Cell {
                    value: Value::Number(2.0),
                    formula: None,
                },
            );
            sh.set(
                950_000,
                0,
                Cell {
                    value: Value::Text("tail".into()),
                    formula: None,
                },
            );
        }
        assert_eq!(app.next_visible_row(0, true), 600_001);
        assert_eq!(app.next_visible_row(600_001, false), 0);
        assert!(
            app.visible_page_rows() > 10,
            "600k hidden rows must not starve visible_page_rows guard to 1"
        );

        app.sel = (0, 0);
        assert_eq!(app.jump_edge(1, 0), (600_002, 0));
        app.sel = (600_002, 0);
        assert_eq!(app.jump_edge(1, 0), (950_000, 0));
        app.sel = (950_000, 0);
        assert_eq!(app.jump_edge(1, 0), (1_048_575, 0));
        app.sel = (1_048_575, 0);
        assert_eq!(app.jump_edge(-1, 0), (950_000, 0));
    }

    #[test]
    fn sparse_protection_queries_full_sheet_without_200k_false_positive() {
        let _env = crate::test_env::TestEnv::new("sparse-prot-lock");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        let mut unlocked_st = app.wb.styles.get(0).clone();
        unlocked_st.locked = false;
        let unlocked_sid = app.wb.styles.intern(unlocked_st);
        let locked_sid = 0u16;
        {
            let sh = app.wb.sheet_mut(0);
            sh.col_styles.insert(1, unlocked_sid);
            let mut prot = sheetcore::protection::SheetProtection::new_with_password("pw");
            prot.perms.select_locked = false;
            prot.perms.select_unlocked = true;
            sh.protection = Some(prot);
        }
        assert!(
            !app.rect_sel_blocked((0, 1), (1_048_575, 1)),
            "1M-row unlocked column B must be selectable without 200k false positive"
        );
        assert!(!app.rects_have_locked(&[(0, 1, 1_048_575, 1)]));

        app.wb.sheet_mut(0).set_style(500_000, 1, locked_sid);
        assert!(app.rect_sel_blocked((0, 1), (1_048_575, 1)));
        assert!(app.rects_have_locked(&[(0, 1, 1_048_575, 1)]));
    }

    #[test]
    fn autofit_col_scans_past_row_5000_and_skips_merges() {
        let _env = crate::test_env::TestEnv::new("autofit-million");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        crate::install_fonts(&ctx);
        {
            let sh = app.wb.sheet_mut(0);
            sh.set(
                0,
                0,
                Cell {
                    value: Value::Text("A".into()),
                    formula: None,
                },
            );
            sh.set(
                25_000,
                0,
                Cell {
                    value: Value::Text("Order-2026-华东大区超长汇总行-超过五千行不截断".into()),
                    formula: None,
                },
            );
            sh.set(
                10,
                1,
                Cell {
                    value: Value::Text("超长合并横幅不应撑开B列宽度测试文本".into()),
                    formula: None,
                },
            );
            sh.merges.push((10, 1, 10, 3));
        }
        app.autofit_col(&ctx, 0);
        app.autofit_col(&ctx, 1);
        assert!(
            app.wb.sheet(0).cols.size(0) > 220.0,
            "row 25000 wide string must be measured by autofit_col"
        );
        assert_eq!(app.wb.sheet(0).cols.size(1), 72.0);
    }

    #[test]
    fn retained_viewport_cache_reuses_recording_on_selection_move_and_invalidates_on_edit() {
        let _env = crate::test_env::TestEnv::new("retained-viewport-hit");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        crate::install_fonts(&ctx);
        app.wb.sheet_mut(0).set(
            2,
            2,
            Cell {
                value: Value::Text("cached".into()),
                formula: None,
            },
        );
        let run_frame = |app: &mut SheetApp| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
            });
        };
        run_frame(&mut app);
        let key1 = app
            .cf_inputs
            .grid
            .viewport
            .key
            .clone()
            .expect("viewport cached");
        app.sel = (5, 3);
        app.sel_end = (8, 5);
        run_frame(&mut app);
        let key2 = app
            .cf_inputs
            .grid
            .viewport
            .key
            .clone()
            .expect("viewport cached");
        assert!(
            key1 == key2,
            "moving selection without scrolling or editing must reuse Tier-2 viewport cache"
        );

        app.wb.sheet_mut(0).set(
            2,
            2,
            Cell {
                value: Value::Text("updated".into()),
                formula: None,
            },
        );
        run_frame(&mut app);
        let key3 = app
            .cf_inputs
            .grid
            .viewport
            .key
            .clone()
            .expect("viewport cached");
        assert!(key2 != key3, "cell mutation must invalidate viewport key");
    }

    #[test]
    fn filter_toggle_and_page_setup_invalidate_viewport_cache_immediately() {
        let _env = crate::test_env::TestEnv::new("filter-and-page-vp-key");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        crate::install_fonts(&ctx);
        for r in 0..=4u32 {
            app.wb.sheet_mut(0).set(
                r,
                0,
                Cell {
                    value: Value::Text(format!("R{r}").into()),
                    formula: None,
                },
            );
        }
        let run_frame = |app: &mut SheetApp| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
            });
        };

        run_frame(&mut app);
        let k_before_filter = app.cf_inputs.grid.viewport.key.clone().unwrap();
        let shapes_before: usize = app
            .cf_inputs
            .grid
            .viewport
            .body_batches
            .iter()
            .map(|(_, s)| s.len())
            .sum();

        app.sel = (0, 0);
        app.start_filter();
        run_frame(&mut app);
        let k_after_filter = app.cf_inputs.grid.viewport.key.clone().unwrap();
        let shapes_after: usize = app
            .cf_inputs
            .grid
            .viewport
            .body_batches
            .iter()
            .map(|(_, s)| s.len())
            .sum();
        assert!(
            k_before_filter != k_after_filter && shapes_after > shapes_before,
            "enabling AutoFilter must invalidate ViewportCacheKey and record funnel arrow"
        );

        // Page Layout mode (`view_mode = 2`): mutating `sh.page` without bumping `doc_gen` must invalidate viewport key
        app.view_mode = 2;
        run_frame(&mut app);
        let k_portrait = app.cf_inputs.grid.viewport.key.clone().unwrap();
        app.wb.sheet_mut(0).page.landscape = !app.wb.sheet(0).page.landscape;
        run_frame(&mut app);
        let k_landscape = app.cf_inputs.grid.viewport.key.clone().unwrap();
        assert!(
            k_portrait != k_landscape,
            "page setup mutation in page_mode must invalidate ViewportCacheKey"
        );
    }

    #[test]
    fn table_attribute_flip_and_cross_sheet_sparkline_invalidate_caches() {
        let _env = crate::test_env::TestEnv::new("table-and-spark-key");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        crate::install_fonts(&ctx);
        app.wb.sheet_mut(0).set(
            0,
            0,
            Cell {
                value: Value::Text("Col1".into()),
                formula: None,
            },
        );
        app.wb.sheet_mut(0).tables.push(sheetcore::table::Table {
            name: "T1".into(),
            display: "T1".into(),
            range: (0, 0, 3, 1),
            columns: vec!["Col1".into(), "Col2".into()],
            column_ids: vec![1, 2],
            header_row: true,
            totals_row: false,
        });
        let run_frame = |app: &mut SheetApp| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
            });
        };
        run_frame(&mut app);
        let k1 = app.cf_inputs.grid.viewport.key.clone().unwrap();
        // Flip header_row in-place without changing tables.len() or doc_gen
        app.wb.sheet_mut(0).tables[0].header_row = false;
        run_frame(&mut app);
        let k2 = app.cf_inputs.grid.viewport.key.clone().unwrap();
        assert!(
            k1.cell_epoch != k2.cell_epoch,
            "in-place table header_row flip must invalidate CellCacheEpoch"
        );

        // Cross-sheet sparkline invalidation when source sheet cells change without touching anchor sheet
        let mut src = Sheet::new("SparkSrc");
        src.set(
            0,
            0,
            Cell {
                value: Value::Number(10.0),
                formula: None,
            },
        );
        src.set(
            0,
            1,
            Cell {
                value: Value::Number(20.0),
                formula: None,
            },
        );
        app.wb.sheets.push(src);
        let sp = sheetcore::sparkline::Sparkline {
            r: 1,
            c: 1,
            source_sheet: Some("SparkSrc".into()),
            rect: (0, 0, 0, 1),
            ..Default::default()
        };
        let v1 = app
            .cf_inputs
            .sparks
            .get_or_load(&app.wb, 0, app.doc_gen, app.wb_epoch, &sp);
        assert_eq!(&*v1, &[Some(10.0), Some(20.0)]);

        app.wb.sheets[1].set(
            0,
            1,
            Cell {
                value: Value::Number(99.0),
                formula: None,
            },
        );
        let v2 = app
            .cf_inputs
            .sparks
            .get_or_load(&app.wb, 0, app.doc_gen, app.wb_epoch, &sp);
        assert_eq!(
            &*v2,
            &[Some(10.0), Some(99.0)],
            "SparkCache must detect cell_version change on cross-sheet source_sheet"
        );
    }

    #[test]
    fn chart_shape_cache_reuses_arc_and_compare_pane_persists_rules() {
        let _env = crate::test_env::TestEnv::new("chart-and-compare-cache");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        crate::install_fonts(&ctx);
        app.wb.sheet_mut(0).charts.push(sheetcore::charts::Chart {
            kind: sheetcore::charts::ChartKind::Line,
            title: "PerfChart".into(),
            series: vec![sheetcore::charts::Series {
                name: "S1".into(),
                cats: vec!["A".into(), "B".into(), "C".into()],
                vals: vec![1.0, 5.0, 3.0],
                ..Default::default()
            }],
            legend: true,
            anchor: (1, 1, 12, 6),
            legend_pos: "r".into(),
            ..Default::default()
        });
        let run_frame = |app: &mut SheetApp| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| app.draw_grid_pane(ui, ctx, false));
            });
        };
        run_frame(&mut app);
        let ptr1 = app
            .cf_inputs
            .charts
            .values()
            .next()
            .map(|a| Arc::as_ptr(a) as *const Shape as usize)
            .expect("chart shapes cached");
        app.sel = (4, 4);
        run_frame(&mut app);
        let ptr2 = app
            .cf_inputs
            .charts
            .values()
            .next()
            .map(|a| Arc::as_ptr(a) as *const Shape as usize)
            .expect("chart shapes still cached");
        assert_eq!(ptr1, ptr2, "chart shape Arc must be reused on hover/selection frames");

        app.compare = Some((Workbook::new_blank(), 0, egui::Vec2::ZERO));
        let run_compare = |app: &mut SheetApp| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| app.draw_compare_pane(ui, ctx));
            });
        };
        run_compare(&mut app);
        let cmp_ptr1 = app
            .normal_page_breaks
            .compare_cf_inputs
            .as_ref()
            .map(|b| &**b as *const Rules as usize)
            .expect("compare Rules persisted");
        run_compare(&mut app);
        let cmp_ptr2 = app
            .normal_page_breaks
            .compare_cf_inputs
            .as_ref()
            .map(|b| &**b as *const Rules as usize)
            .expect("compare Rules still persisted");
        assert_eq!(cmp_ptr1, cmp_ptr2, "draw_compare_pane must reuse persistent Rules");
    }

    #[test]
    fn current_region_fast_handles_sparse_anchor_column_over_200k_cells() {
        let mut sh = Sheet::new("LargeSparseAnchor");
        for r in 0..210_000u32 {
            sh.set(
                r,
                3,
                Cell {
                    value: Value::Number(r as f64),
                    formula: None,
                },
            );
        }
        sh.set(
            0,
            4,
            Cell {
                value: Value::Text("Mid".into()),
                formula: None,
            },
        );
        sh.set(
            0,
            5,
            Cell {
                value: Value::Text("SparseNote".into()),
                formula: None,
            },
        );
        assert!(sh.len() > 200_000);
        let start = std::time::Instant::now();
        let region = SheetApp::current_region_fast(&sh, 0, 5);
        let elapsed = start.elapsed();
        assert_eq!(region, (0, 3, 209_999, 5));
        assert!(
            elapsed.as_millis() < 1500,
            "current_region_fast took {elapsed:?} on >200k sheet with sparse anchor col"
        );
    }

    #[test]
    fn chart_shapes_reuses_arc_across_scroll_and_unrelated_doc_gen() {
        let ctx = egui::Context::default();
        crate::install_fonts(&ctx);
        let mut sh = Sheet::new("ChartScroll");
        let ch = sheetcore::charts::Chart {
            kind: sheetcore::charts::ChartKind::Line,
            title: "Scroll Reuse".into(),
            series: vec![sheetcore::charts::Series {
                name: "S1".into(),
                cats: vec!["A".into(), "B".into(), "C".into()],
                vals: vec![10.0, 25.0, 18.0],
                ..Default::default()
            }],
            legend: true,
            anchor: (1, 1, 10, 6),
            legend_pos: "r".into(),
            part: "xl/charts/chart1.xml".into(),
            drawing: "xl/drawings/drawing1.xml".into(),
            axis_x: "X".into(),
            axis_y: "Y".into(),
            ..Default::default()
        };
        sh.charts.push(ch);

        let mut rules = Rules::default();
        let fonts_gen = crate::system_fonts::generation();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        let mut keep_a2: Option<Arc<[Shape]>> = None;
        let mut a4_out: Option<Arc<[Shape]>> = None;
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let painter = ui.painter();
                let r1 = egui::Rect::from_min_size(egui::pos2(40.0, 50.0), egui::vec2(320.0, 200.0));
                let r2 = egui::Rect::from_min_size(egui::pos2(120.0, 260.0), egui::vec2(320.0, 200.0));
                let r3 = egui::Rect::from_min_size(egui::pos2(120.0, 260.0), egui::vec2(360.0, 200.0));
                let a1 = rules.chart_shapes(painter, &sh, 1, 0, fonts_gen, 0, r1, &sh.charts[0]);
                // Scrolling position + unrelated doc_gen increment must still hit local-coords cache.
                let a2 = rules.chart_shapes(painter, &sh, 99, 0, fonts_gen, 0, r2, &sh.charts[0]);
                // Resizing width must miss and rebuild.
                let a3 = rules.chart_shapes(painter, &sh, 99, 0, fonts_gen, 0, r3, &sh.charts[0]);
                assert!(Arc::ptr_eq(&a1, &a2), "chart shapes must reuse Arc across scroll and unrelated doc_gen");
                assert!(!Arc::ptr_eq(&a2, &a3), "chart shapes must rebuild when chart dimensions change");
                keep_a2 = Some(a2);
            });
        });

        // Mutating chart title must invalidate charts_epoch even at the same size.
        sh.charts[0].title = "Updated Title".into();
        let raw2 = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        let _ = ctx.run(raw2, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let r2 = egui::Rect::from_min_size(egui::pos2(120.0, 260.0), egui::vec2(320.0, 200.0));
                a4_out = Some(rules.chart_shapes(ui.painter(), &sh, 99, 0, fonts_gen, 0, r2, &sh.charts[0]));
            });
        });
        let a2 = keep_a2.unwrap();
        let a4 = a4_out.unwrap();
        assert!(!Arc::ptr_eq(&a2, &a4), "chart title change must invalidate cached chart shapes");
    }

    #[test]
    fn merge_cache_max_col_and_has_overlap_match_linear_scan() {
        let mut sh = Sheet::new("Merges");
        sh.merges.push((2, 3, 4, 6));
        sh.merges.push((10, 1, 12, 15));
        let mut cache = MergeCache::default();
        cache.ensure(&sh, 0, 0);
        assert_eq!(cache.max_col(), 15);
        assert!(cache.has_overlap(3, 5, 5, 7));
        assert!(!cache.has_overlap(0, 0, 1, 2));
        assert!(cache.has_overlap(11, 0, 11, 2));
        assert!(!cache.has_overlap(5, 0, 9, 20));
    }

    #[test]
    fn retained_grid_cache_preserves_unaffected_cells_across_single_cell_edit() {
        let mut sh = Sheet::new("Retained");
        sh.set(0, 0, Cell { value: Value::Number(1.0), formula: None });
        let v1 = sh.cell_version().clone();
        sh.set(2, 2, Cell { value: Value::Number(2.0), formula: None });
        let v2 = sh.cell_version().clone();

        let mut cache = RetainedGridCache::default();
        let epoch1 = CellCacheEpoch {
            sheet_id: sh.identity,
            doc_gen: 1,
            wb_epoch: 0,
            cells: v1,
            style_rev: 0,
            cf_gen: 0,
            cols_rev: 0,
            rows_rev: 0,
            hidden_cols_rev: 0,
            font_zoom_bits: 1.0_f32.to_bits(),
            layout_zoom_bits: 1.0_f32.to_bits(),
            paint_scale_bits: 1.0_f32.to_bits(),
            show_formulas: false,
            text_spill: true,
            fonts_gen: 0,
            tables_fp: 0,
            sparklines_len: 0,
        };
        cache.ensure_epoch(&epoch1);
        let mk = |rev: u64, dep: bool| PreparedCell {
            width_bits: 64.0_f32.to_bits(),
            height_bits: 20.0_f32.to_bits(),
            verified_rev: rev,
            cell_snap: Some(Cell { value: Value::Number(1.0), formula: None }),
            has_link: false,
            has_rich: false,
            depends_on_row_neighbors: dep,
            bg_fill: None,
            cf_bar: None,
            cf_borders: None,
            diagonal: None,
            borders: Default::default(),
            cf_icon: None,
            spark_idx: None,
            text: PreparedText::Empty,
        };
        cache.cells.insert((0, 0), mk(cache.content_rev, false));
        cache.cells.insert((2, 2), mk(cache.content_rev, false));
        cache.cells.insert((2, 1), mk(cache.content_rev, true));
        cache.cells.insert((5, 5), mk(cache.content_rev, false));

        // Explicit dirty mark removes edited cell + row-dependent neighbor, keeps other rows.
        cache.mark_cells_dirty([(2, 2)]);
        assert!(!cache.cells.contains_key(&(2, 2)));
        assert!(!cache.cells.contains_key(&(2, 1)));
        assert!(cache.cells.contains_key(&(0, 0)));
        assert!(cache.cells.contains_key(&(5, 5)));

        // Content-only epoch bump preserves remaining entries and advances content_rev.
        let rev_before = cache.content_rev;
        let epoch2 = CellCacheEpoch { doc_gen: 2, cells: v2, ..epoch1.clone() };
        cache.ensure_epoch(&epoch2);
        assert!(cache.content_rev > rev_before);
        assert!(cache.cells.contains_key(&(0, 0)));
        assert!(cache.cells.contains_key(&(5, 5)));

        // Zoom/layout epoch change clears all prepared cells.
        let epoch3 = CellCacheEpoch { font_zoom_bits: 1.25_f32.to_bits(), ..epoch2 };
        cache.ensure_epoch(&epoch3);
        assert!(cache.cells.is_empty());
    }

    #[test]
    fn cf_stats_worker_reuses_slice_cache_across_refreshes() {
        let mut sh = Sheet::new("CfWorkerReuse");
        for r in 0..20u32 {
            sh.set(r, 0, Cell { value: Value::Number((r + 1) as f64), formula: None });
        }
        sh.cf.blocks.push(sheetcore::cf::CfBlock {
            ranges: vec![(0, 0, 19, 0)],
            rules: vec![sheetcore::cf::CfRule {
                priority: 1,
                stop: false,
                kind: sheetcore::cf::CfKind::Bar {
                    color: [99, 142, 198],
                    min: sheetcore::cf::Cfvo::min(),
                    max: sheetcore::cf::Cfvo::max(),
                },
            }],
        });
        let worker = CfStatsWorker::new();
        let ctx = egui::Context::default();
        worker
            .tx
            .send(CfStatsJob {
                sheet_id: sh.identity,
                cells: sh.cell_version().clone(),
                cf: sh.cf.clone(),
                snapshot: sh.clone(),
                wake: ctx.clone(),
            })
            .unwrap();
        let done1 = worker
            .rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("first cf stats job must finish");
        assert_eq!(done1.stats.ranges.get(&0), Some(&(1.0, 20.0)));

        // Second refresh on same sheet_id updates range incrementally while worker keeps per-sheet CfStats.
        sh.set(19, 0, Cell { value: Value::Number(40.0), formula: None });
        worker
            .tx
            .send(CfStatsJob {
                sheet_id: sh.identity,
                cells: sh.cell_version().clone(),
                cf: sh.cf.clone(),
                snapshot: sh.clone(),
                wake: ctx,
            })
            .unwrap();
        let done2 = worker
            .rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("second cf stats job must finish");
        assert_eq!(done2.stats.ranges.get(&0), Some(&(1.0, 40.0)));
    }
}
