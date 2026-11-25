//! 打印 / 导出 PDF：按页面设置分页，再把同一份显示模型画到 PDF 上。
//!
//! 坐标换算：列宽/行高是「96 DPI 像素」，PDF 用「点」（72 DPI），
//! 100% 缩放时 1px = 0.75pt。页面尺寸与边距都从 `PageSetup` 来。
//!
//! 单元格内容一律走 `display::view_at`（数字格式、[Red] 负数、合并、换行、
//! 对齐、条件格式叠加都在那里算好），预览与 PDF 共用，不会各画一套。

use crate::display::{self, CellView};
use crate::model::{Sheet, Workbook};
use crate::page::PageSetup;
use crate::pdf::{Doc, Font, Page};
use anyhow::Result;
use std::path::Path;

/// 纸张尺寸（pt）：(宽, 高)，纵向
fn paper_size(paper: u16) -> (f32, f32) {
    match paper {
        9 => (595.28, 841.89),  // A4
        8 => (841.89, 1190.55), // A3
        11 => (420.94, 595.28), // A5
        1 => (612.0, 792.0),    // Letter
        5 => (612.0, 1008.0),   // Legal
        3 => (792.0, 1224.0),   // Tabloid
        _ => (595.28, 841.89),
    }
}

/// 一页包含哪些行列
#[derive(Clone, Debug, PartialEq)]
pub struct PageSpan {
    pub rows: (u32, u32),
    pub cols: (u32, u32),
}

/// 分页结果
#[derive(Clone, Debug)]
pub struct Layout {
    setup: PageSetup,
    pub pages: Vec<PageSpan>,
    /// 实际缩放（1.0 = 100%）
    pub scale: f32,
    pub printable: (f32, f32),
    pub page_size: (f32, f32),
}

fn margins_pt(setup: &PageSetup) -> (f32, f32, f32, f32) {
    let m = setup.margins;
    (m[0] * 72.0, m[1] * 72.0, m[2] * 72.0, m[3] * 72.0)
}

/// 行高：隐藏行（手动隐藏或筛选隐藏）不占打印高度
fn row_h(sheet: &Sheet, r: u32) -> f32 {
    if sheet.hidden_rows.contains(&r) || sheet.hidden_rows_user.contains(&r) {
        0.0
    } else {
        sheet.rows.size(r)
    }
}

/// 列宽：隐藏列不占打印宽度
fn col_w(sheet: &Sheet, c: u32) -> f32 {
    if sheet.hidden_cols.contains(&c) {
        0.0
    } else {
        sheet.cols.size(c)
    }
}

#[inline]
fn axis_segment_at(
    axis: &crate::layout::Axis,
    h1: &[(u32, u32)],
    h2: &[(u32, u32)],
    pos: u32,
    max_end: u32,
) -> (u32, f32) {
    let k1 = h1.partition_point(|&(lo, _)| lo <= pos);
    if k1 > 0 && pos <= h1[k1 - 1].1 {
        return (h1[k1 - 1].1.min(max_end), 0.0);
    }
    let next_h1 = h1.get(k1).map_or(u32::MAX, |&(lo, _)| lo);

    let k2 = h2.partition_point(|&(lo, _)| lo <= pos);
    if k2 > 0 && pos <= h2[k2 - 1].1 {
        return (h2[k2 - 1].1.min(max_end), 0.0);
    }
    let next_h2 = h2.get(k2).map_or(u32::MAX, |&(lo, _)| lo);

    let spans = axis.spans();
    let ks = spans.partition_point(|s| s.start <= pos);
    let (span_end, sz) = if ks > 0 && pos <= spans[ks - 1].end {
        (spans[ks - 1].end, spans[ks - 1].size)
    } else {
        let next_s = spans.get(ks).map_or(u32::MAX, |s| s.start);
        (next_s - 1, axis.size(pos))
    };
    let seg_end = max_end
        .min(span_end)
        .min(next_h1 - 1)
        .min(next_h2 - 1);
    (seg_end, sz)
}

fn sum_axis_px(
    axis: &crate::layout::Axis,
    h1: &[(u32, u32)],
    h2: &[(u32, u32)],
    lo: u32,
    hi: u32,
) -> f32 {
    if lo > hi {
        return 0.0;
    }
    let mut total = 0.0f64;
    let mut pos = lo;
    while pos <= hi {
        let (seg_end, sz) = axis_segment_at(axis, h1, h2, pos, hi);
        total += (seg_end - pos + 1) as f64 * f64::from(sz);
        match seg_end.checked_add(1) {
            Some(next) => pos = next,
            None => break,
        }
    }
    total as f32
}

#[allow(clippy::too_many_arguments)]
fn paginate_axis(
    axis: &crate::layout::Axis,
    h1: &[(u32, u32)],
    h2: &[(u32, u32)],
    sorted_breaks: &[u32],
    start_idx: u32,
    end_idx: u32,
    px: f32,
    body_limit: f32,
) -> Vec<(u32, u32)> {
    if start_idx > end_idx {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut page_start = start_idx;
    let mut break_cursor = sorted_breaks.partition_point(|&b| b <= start_idx);
    while page_start <= end_idx {
        while break_cursor < sorted_breaks.len() && sorted_breaks[break_cursor] <= page_start {
            break_cursor += 1;
        }
        let mut used = 0.0f32;
        let mut cur = page_start;
        while cur <= end_idx {
            if cur > page_start && sorted_breaks.get(break_cursor) == Some(&cur) {
                break;
            }
            let next_brk_limit = sorted_breaks
                .get(break_cursor)
                .map_or(end_idx, |&b| (b - 1).min(end_idx));
            let (seg_end, sz) = axis_segment_at(axis, h1, h2, cur, next_brk_limit);
            let item_pt = sz * px;
            if item_pt <= 0.0 {
                match seg_end.checked_add(1) {
                    Some(next) => cur = next,
                    None => {
                        cur = end_idx.saturating_add(1);
                        break;
                    }
                }
                continue;
            }
            if cur == page_start && used + item_pt > body_limit {
                cur += 1;
                break;
            }
            if cur > page_start && used + item_pt > body_limit {
                break;
            }
            let seg_len = seg_end - cur + 1;
            if seg_len >= 4 {
                let avail = (body_limit - used).max(0.0);
                let jump = ((avail / item_pt).floor() as u32)
                    .min(seg_len)
                    .saturating_sub(1);
                if jump > 0 {
                    used += jump as f32 * item_pt;
                    cur += jump;
                }
            }
            let mut stopped = false;
            while cur <= seg_end {
                if cur > page_start && used + item_pt > body_limit {
                    stopped = true;
                    break;
                }
                used += item_pt;
                cur += 1;
            }
            if stopped {
                break;
            }
        }
        let page_end = cur.saturating_sub(1).min(end_idx).max(page_start);
        out.push((page_start, page_end));
        match page_end.checked_add(1) {
            Some(next) => page_start = next,
            None => break,
        }
    }
    out
}

/// 分页：按列宽/行高累加，超出可打印区就换页。
///
/// 打印标题（顶端标题行 / 左侧标题列）每页重复，正文从标题之后开始：
/// 第一页不再把标题区当正文再画一遍，续页的容量也按标题占掉的高度扣减。
pub fn paginate(sheet: &Sheet, setup: &PageSetup) -> Layout {
    let (pw, ph) = paper_size(setup.paper);
    let (pw, ph) = if setup.landscape { (ph, pw) } else { (pw, ph) };
    let (left, right, top, bottom) = margins_pt(setup);
    let printable = ((pw - left - right).max(72.0), (ph - top - bottom).max(72.0));

    let (r1, c1, mut r2, mut c2) = match setup.print_area {
        Some((ar1, ac1, ar2, ac2)) => {
            // 防止全选 Ctrl+A 后点击“设置打印区域”将 print_area 设为 1048576×16384，导致生成 5300 万个分页块（~860 MB）卡死
            if (ar2.saturating_sub(ar1) as u64 + 1) * (ac2.saturating_sub(ac1) as u64 + 1) > 500_000 {
                if let Some((_, _, ur2, uc2)) = sheet.used_bounds() {
                    (ar1, ac1, ar2.min(ur2.max(ar1)), ac2.min(uc2.max(ac1)))
                } else {
                    (ar1, ac1, ar1, ac1)
                }
            } else {
                (ar1, ac1, ar2, ac2)
            }
        }
        None => sheet
            .used_bounds()
            .map(|(_, _, max_r, max_c)| (0, 0, max_r, max_c))
            .unwrap_or((0, 0, 0, 0)),
    };
    r2 = r2.max(r1);
    c2 = c2.max(c1);

    let hr1 = sheet.hidden_rows.intervals();
    let hr2 = sheet.hidden_rows_user.intervals();
    let hc1 = sheet.hidden_cols.intervals();

    let mut scale = if setup.fit_to_page {
        let target_w = printable.0 * setup.fit_width.max(1) as f32;
        let total_w = sum_axis_px(&sheet.cols, hc1, &[], c1, c2);
        if total_w > 0.0 {
            (target_w / (total_w * 0.75)).min(1.0)
        } else {
            1.0
        }
    } else {
        setup.scale.max(10) as f32 / 100.0
    };
    if setup.fit_to_page && setup.fit_height > 0 {
        let target_h = printable.1 * setup.fit_height as f32;
        let total_h = sum_axis_px(&sheet.rows, hr1, hr2, r1, r2);
        if total_h > 0.0 {
            scale = scale.min((target_h / (total_h * 0.75)).min(1.0));
        }
    }
    let px = scale * 0.75;

    let title_rows = setup.title_rows.map(|(a, b)| (a, b.min(r2))).filter(|(a, b)| a <= b);
    let title_cols = setup.title_cols.map(|(a, b)| (a, b.min(c2))).filter(|(a, b)| a <= b);
    let head_rows_h: f32 = match title_rows {
        Some((a, b)) => sum_axis_px(&sheet.rows, hr1, hr2, a, b) * px,
        None => 0.0,
    };
    let head_cols_w: f32 = match title_cols {
        Some((a, b)) => sum_axis_px(&sheet.cols, hc1, &[], a, b) * px,
        None => 0.0,
    };

    let body_top = (printable.1 - head_rows_h).max(24.0);
    let body_left = (printable.0 - head_cols_w).max(24.0);
    let br1 = title_rows.map(|(_, b)| b + 1).unwrap_or(r1).max(r1).min(r2);
    let bc1 = title_cols.map(|(_, b)| b + 1).unwrap_or(c1).max(c1).min(c2);

    let mut row_breaks: Vec<u32> = setup
        .row_breaks
        .iter()
        .copied()
        .filter(|&b| b > br1 && b <= r2)
        .collect();
    row_breaks.sort_unstable();
    row_breaks.dedup();

    let mut col_breaks: Vec<u32> = setup
        .col_breaks
        .iter()
        .copied()
        .filter(|&b| b > bc1 && b <= c2)
        .collect();
    col_breaks.sort_unstable();
    col_breaks.dedup();

    let row_spans = paginate_axis(&sheet.rows, hr1, hr2, &row_breaks, br1, r2, px, body_top);
    let col_spans = paginate_axis(&sheet.cols, hc1, &[], &col_breaks, bc1, c2, px, body_left);

    let mut pages = Vec::with_capacity(row_spans.len() * col_spans.len());
    for &rows in &row_spans {
        for &cols in &col_spans {
            pages.push(PageSpan { rows, cols });
        }
    }
    if pages.is_empty() {
        pages.push(PageSpan { rows: (r1, r2), cols: (c1, c2) });
    }
    Layout { setup: setup.clone(), pages, scale, printable, page_size: (pw, ph) }
}

/// 页眉/页脚：`&L &C &R` 分左中右，`&P` 页码、`&N` 总页数、`&&` 一个 &。
/// 没有分段标记时整段居中。
pub fn header_parts(text: &str, page: usize, total: usize) -> (String, String, String) {
    let (mut l, mut c, mut r) = (String::new(), String::new(), String::new());
    let mut seg = 1usize;
    let mut has_mark = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&' {
            match chars.next() {
                Some('L') => {
                    has_mark = true;
                    seg = 0;
                    continue;
                }
                Some('C') => {
                    has_mark = true;
                    seg = 1;
                    continue;
                }
                Some('R') => {
                    has_mark = true;
                    seg = 2;
                    continue;
                }
                Some('P') => {
                    let s = page.to_string();
                    match seg {
                        0 => l.push_str(&s),
                        1 => c.push_str(&s),
                        _ => r.push_str(&s),
                    }
                    continue;
                }
                Some('N') => {
                    let s = total.to_string();
                    match seg {
                        0 => l.push_str(&s),
                        1 => c.push_str(&s),
                        _ => r.push_str(&s),
                    }
                    continue;
                }
                Some('&') => {
                    match seg {
                        0 => l.push('&'),
                        1 => c.push('&'),
                        _ => r.push('&'),
                    }
                    continue;
                }
                Some(other) => {
                    match seg {
                        0 => l.push(other),
                        1 => c.push(other),
                        _ => r.push(other),
                    }
                    continue;
                }
                None => break,
            }
        }
        match seg {
            0 => l.push(ch),
            1 => c.push(ch),
            _ => r.push(ch),
        }
    }
    if !has_mark {
        return (String::new(), l, String::new());
    }
    (l, c, r)
}

/// 页面绘制指令（预览与 PDF 共用；PDF 在这里统一编码字形）
#[derive(Default)]
pub struct Out {
    pub rects: Vec<(f32, f32, f32, f32, [u8; 3])>,
    pub lines: Vec<(f32, f32, f32, f32, f32, [u8; 3])>,
    pub texts: Vec<TextOp>,
    /// 图片：(x, y, w, h, 名字, 原始字节, 扩展名)
    pub images: Vec<(f32, f32, f32, f32, String, Vec<u8>, String)>,
    /// 填充多边形（图表饼图/折线区域）
    pub polys: Vec<(Vec<(f32, f32)>, [u8; 3])>,
    /// 真字体度量（导出 PDF 时挂上）；None 时退回 display::text_width 的估算
    pub metric: Option<std::sync::Arc<dyn Fn(char, f32) -> f32 + Send + Sync>>,
    /// 真纵向度量（导出 PDF 时挂上）：size(pt) → (ascender, descender)，单位 pt，都取正
    pub vmetric: Option<std::sync::Arc<dyn Fn(f32) -> (f32, f32) + Send + Sync>>,
}

impl Out {
    fn tw(&self, s: &str, size: f32) -> f32 {
        match &self.metric {
            Some(f) => s.chars().map(|c| f(c, size)).sum(),
            None => display::text_width(s, size),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TextOp {
    pub x: f32,
    pub baseline: f32,
    pub size: f32,
    pub text: String,
    pub color: [u8; 3],
    pub bold: bool,
    pub italic: bool,
    pub rotation: f32,
}

/// 每张表算一次条件格式的统计量，供逐格 overlay 使用
struct PageState<'a> {
    stats: Option<crate::cf::CfStats>,
    merges: crate::merges::MergeIndex,
    formulas: crate::formula::RuleFormulas,
    calc: crate::formula::Calc<'a>,
}

impl<'a> PageState<'a> {
    fn new(wb: &'a Workbook, sheet_idx: usize) -> Self {
        let sheet = wb.sheet(sheet_idx);
        let stats = (!sheet.cf.blocks.is_empty()).then(|| {
            let mut stats = crate::cf::CfStats::default();
            crate::cf::compute_stats(&sheet.cf, sheet, &mut stats);
            stats
        });
        let mut merges = crate::merges::MergeIndex::default();
        merges.refresh(&sheet.merges);
        Self { stats, merges, formulas: Default::default(), calc: crate::formula::Calc::new(wb) }
    }
}

/// 单元格首行基线的位置（PDF 坐标，y 向上；`top` 是下边、`top + ch` 是上边）。
/// 只用字体真度量算：ascender/descender 决定行高，也决定顶/底对齐时留多少。
/// `valign`：0 = 靠上，1 = 居中，2 = 靠下；多行时 `total_h = line_h × 行数`。
pub fn cell_baseline(
    valign: u8,
    top: f32,
    ch: f32,
    total_h: f32,
    line_h: f32,
    asc: f32,
    desc: f32,
) -> f32 {
    let bottom = top + ch;
    match valign {
        // 0 = 靠上：首行基线在顶边下 ascender 处
        0 => bottom - asc,
        // 1 = 居中：整块文字的视觉中心（含 asc/desc 不对称）对准格子中心
        1 => bottom - (ch - total_h) / 2.0 - asc,
        // 2 = 靠下：末行基线在底边上 descender 处
        _ => top + desc + (total_h - line_h),
    }
}

fn draw_cell(
    wb: &Workbook,
    sheet: &Sheet,
    si: usize,
    state: &mut PageState,
    r: u32,
    c: u32,
    x: f32,
    y_bottom: f32,
    w: f32,
    h: f32,
    px: f32,
    scale: f32,
    out: &mut Out,
) {
    let merge = state.merges.at(r, c);
    if merge.is_some_and(|(top, left, _, _)| (top, left) != (r, c)) {
        return;
    }
    let overlay = state.stats.as_ref().map(|st| {
        crate::cf::overlay_at(&sheet.cf, st, sheet, r, c, |f, r0, c0| {
            match state.formulas.shifted(f, r0, c0) {
                Ok(node) => matches!(
                    state.calc.eval_node(&node, si),
                    crate::formula::FValue::Bool(true)
                ),
                Err(_) => false,
            }
        })
    });
    // 公式格：优先用缓存值；缓存为空时算一次，打印必须显示结果而不是空白
    let mut eff: Option<crate::model::Value> = None;
    if let Some(cell) = sheet.get(r, c) {
        if cell.value.is_empty() {
            if cell.formula.is_some() {
                eff = Some(fvalue_to_value(state.calc.cell_value(si, r, c)));
            }
        }
    }
    let cap = {
        let c2 = merge
            .map(|(_, _, _, c2)| c2)
            .unwrap_or(c);
        let px: f32 = (c..=c2).map(|cc| col_w(sheet, cc)).sum();
        (((px - 5.0) / 8.0).floor().max(1.0)) as usize
    };
    let Some(view) = display::view_with_merge(
        wb,
        sheet,
        r,
        c,
        overlay.as_ref(),
        eff.as_ref(),
        Some(cap),
        merge,
    ) else {
        return;
    };
    let (mut cw, mut ch) = (w, h);
    if let Some((r1, c1, r2, c2)) = view.merge {
        if (r1, c1) == (r, c) {
            cw = (c1..=c2).map(|cc| col_w(sheet, cc) * px).sum();
            ch = (r1..=r2).map(|rr| row_h(sheet, rr) * px).sum();
        }
    }
    let top = y_bottom + ch - h;
    draw_view(sheet, &wb.styles, r, c, x, top, cw, ch, scale, &view, out);
}

#[allow(clippy::too_many_arguments)]
fn draw_view(
    sheet: &Sheet,
    styles: &crate::style::StyleTable,
    r: u32,
    c: u32,
    x: f32,
    top: f32,
    cw: f32,
    ch: f32,
    scale: f32,
    view: &CellView,
    out: &mut Out,
) {
    if let Some(fill) = view.fill {
        out.rects.push((x, top, cw, ch, fill));
    }
    if let Some((t, col)) = view.bar {
        let bw = (cw * t).max(0.0);
        if bw > 0.5 {
            out.rects.push((x, top, bw, ch, col));
        }
    }
    let st = styles.get(sheet.style_of(r, c));
    for (i, b) in st.border.iter().enumerate() {
        if b.style == 0 {
            continue;
        }
        let (x1, y1, x2, y2) = match i {
            0 => (x, top, x, top + ch),
            1 => (x + cw, top, x + cw, top + ch),
            2 => (x, top + ch, x + cw, top + ch),
            _ => (x, top, x + cw, top),
        };
        out.lines.push((x1, y1, x2, y2, b.px(), b.color));
    }
    if view.text.is_empty() {
        return;
    }
    let size = (view.size * scale).max(3.0);
    // Excel 的缩进单位是「3 个 0 的宽度」（不是 0.6 个字号），且只扣一次；
    // 折行预算与字宽另有 0.88 / 0.975 的实测系数，见 display::wrap_lines_excel。
    let pad = 2.0 * scale;
    let indent = view.indent as f32 * 3.0 * display::char_width('0', size);
    let mut lines_txt = if view.wrap {
        display::wrap_lines_excel(
            &view.text,
            (cw - pad * 2.0 - indent).max(4.0),
            size,
            out.metric.as_deref(),
        )
    } else {
        view.text.split('\n').map(|s| s.to_string()).collect()
    };
    let (asc, desc) = match &out.vmetric {
        Some(f) => f(size),
        None => (size * 0.92, size * 0.26),
    };
    let line_h = asc + desc;
    // Excel 打印按行高裁剪：放不下的行不画（自动行高时本来就放得下，等于空操作）
    let fit = ((ch / line_h).floor() as usize).max(1);
    if lines_txt.len() > fit {
        lines_txt.truncate(fit);
    }
    let total_h = line_h * lines_txt.len() as f32;
    let bottom = top + ch;
    let mut ty = cell_baseline(view.valign, top, ch, total_h, line_h, asc, desc);
    let _ = bottom;
    for line in &lines_txt {
        let tw = out.tw(line, size);
        let tx = match view.halign {
            1 => x + (cw - tw) / 2.0,
            2 => x + cw - pad - tw,
            _ => x + pad,
        };
        out.texts.push(TextOp {
            x: tx,
            baseline: ty,
            size,
            text: line.to_string(),
            color: view.color,
            bold: view.bold,
            italic: view.italic,
            rotation: view.rotation as f32,
        });
        ty -= line_h;
    }
}

/// 对象锚点换算：把 (r1,c1,r2,c2) 的格范围换成纸上的 pt 矩形（左上角）
fn grid_rect(
    sheet: &Sheet,
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    off: [i64; 4],
    sx: f32,
    sy: f32,
    px: f32,
) -> (f32, f32, f32, f32) {
    // anchor 语义：to 是边界格（不含），from/to 各带 EMU 偏移；1px = 9525 EMU。
    // 不用「逐格累加」——那会把 to 也画进去，图片比 Excel 宽一整列。
    let e = |v: i64| v as f32 / 9525.0;
    let hc = sheet.hidden_cols.intervals();
    let hr1 = sheet.hidden_rows.intervals();
    let hr2 = sheet.hidden_rows_user.intervals();
    let x1: f32 = if c1 > 0 { sum_axis_px(&sheet.cols, hc, &[], 0, c1 - 1) } else { 0.0 } + e(off[0]);
    let x2: f32 = if c2 > 0 { sum_axis_px(&sheet.cols, hc, &[], 0, c2 - 1) } else { 0.0 } + e(off[2]);
    let t1: f32 = if r1 > 0 { sum_axis_px(&sheet.rows, hr1, hr2, 0, r1 - 1) } else { 0.0 } + e(off[1]);
    let t2: f32 = if r2 > 0 { sum_axis_px(&sheet.rows, hr1, hr2, 0, r2 - 1) } else { 0.0 } + e(off[3]);
    let (xa, xb) = (x1 * px + sx, x2 * px + sx);
    let (ta, tb) = (t1 * px, t2 * px);
    let w = (xb - xa).max(1.0);
    let h = (tb - ta).max(1.0);
    (xa, sy - tb, w, h)
}

const CHART_COLORS: [[u8; 3]; 6] = [
    [68, 114, 196],
    [237, 125, 49],
    [165, 165, 165],
    [255, 192, 0],
    [91, 155, 213],
    [112, 173, 71],
];

fn chart_text(out: &mut Out, x: f32, y: f32, size: f32, text: &str, color: [u8; 3]) {
    out.texts.push(TextOp {
        x,
        baseline: y,
        size,
        text: text.to_string(),
        color,
        bold: false,
        italic: false,
        rotation: 0.0,
    });
}

/// 图表：柱/条形/折线/饼。预览与 PDF 共用同一套矩形/折线指令。
fn draw_chart(sheet: &Sheet, ch: &crate::charts::Chart, sx: f32, sy: f32, px: f32, out: &mut Out) {
    use crate::charts::ChartKind;
    let (a, b, c, d) = ch.anchor;
    let (x, y, w, h) = grid_rect(sheet, a, b, c, d, [0; 4], sx, sy, px);
    if w < 20.0 || h < 20.0 {
        return;
    }
    out.rects.push((x, y, w, h, [255, 255, 255]));
    out.lines.push((x, y, x + w, y, 0.6, [180, 180, 180]));
    out.lines.push((x, y + h, x + w, y + h, 0.6, [180, 180, 180]));
    out.lines.push((x, y, x, y + h, 0.6, [180, 180, 180]));
    out.lines.push((x + w, y, x + w, y + h, 0.6, [180, 180, 180]));

    if matches!(ch.kind, ChartKind::Unknown) {
        let label = if ch.title.is_empty() {
            "高级图表（保留原图，不能在此编辑）".to_string()
        } else {
            format!("{}（保留原图，不能在此编辑）", ch.title)
        };
        let tw = out.tw(&label, 9.0);
        chart_text(out, x + (w - tw) / 2.0, y + h / 2.0, 9.0, &label, [120, 90, 40]);
        return;
    }
    let th = crate::chart_engine::theme_spec(ch.style.theme);
    let mut top = y + h - 6.0;
    if !ch.title.is_empty() {
        let tw = out.tw(&ch.title, 10.0);
        chart_text(out, x + (w - tw) / 2.0, top, 10.0, &ch.title, th.title_fg);
        top -= 14.0;
    }
    if !ch.subtitle.is_empty() {
        let tw = out.tw(&ch.subtitle, 7.5);
        chart_text(out, x + (w - tw) / 2.0, top, 7.5, &ch.subtitle, th.subtitle_fg);
        top -= 11.0;
    }
    let legend_w = if (ch.legend || !ch.legend_pos.is_empty())
        && !matches!(ch.kind, ChartKind::Waterfall | ChartKind::Funnel | ChartKind::Treemap)
    {
        62.0f32
    } else {
        0.0
    };
    let plot = (
        x + 34.0,
        y + 18.0,
        (w - 44.0 - legend_w).max(20.0),
        (top - y - 24.0).max(20.0),
    );
    let (px0, py0, pw, ph) = plot;
    if !matches!(
        ch.kind,
        ChartKind::Pie | ChartKind::Doughnut | ChartKind::Radar | ChartKind::Funnel | ChartKind::Treemap
    ) {
        out.lines.push((px0, py0, px0 + pw, py0, 0.5, th.axis_line));
        out.lines.push((px0, py0, px0, py0 + ph, 0.5, th.axis_line));
    }

    let vis_series: Vec<(usize, &crate::charts::Series)> = ch
        .series
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.hidden)
        .collect();
    let max_of = |secondary: bool| {
        vis_series
            .iter()
            .filter(|(_, s)| s.secondary == secondary)
            .flat_map(|(_, s)| s.vals.iter().copied())
            .filter(|v| v.is_finite())
            .fold(0.0f64, f64::max)
            .max(1e-9)
    };
    let max_main = nice_max(max_of(false));
    let max_sub = nice_max(max_of(true));
    let has_sub = vis_series.iter().any(|(_, s)| s.secondary);
    let n = vis_series.iter().map(|(_, s)| s.vals.len()).max().unwrap_or(0);
    let y_fmt = ch.effective_y_axis().num_fmt;
    if !matches!(
        ch.kind,
        ChartKind::Pie | ChartKind::Doughnut | ChartKind::Radar | ChartKind::Funnel | ChartKind::Treemap
    ) {
        let tick = |v: f64| -> String {
            if y_fmt.is_empty() {
                crate::model::format_number(v)
            } else {
                crate::style::format_value(&crate::model::Value::Number(v), &y_fmt)
            }
        };
        let want = if y_fmt.contains('%') { 10.0 } else { 5.0 };
        let step = nice_step(max_main / want);
        let n_steps = ((max_main / step).ceil() as usize).clamp(1, 12);
        for k in 0..=n_steps {
            let v = step * k as f64;
            let txt = tick(v);
            let tw = out.tw(&txt, 7.0);
            let frac = (v / max_main) as f32;
            chart_text(out, px0 - tw - 3.0, py0 + ph * frac - 2.5, 7.0, &txt, [90, 90, 90]);
        }
        if has_sub {
            let want2 = if y_fmt.contains('%') { 10.0 } else { 5.0 };
            let step2 = nice_step(max_sub / want2);
            let n2 = ((max_sub / step2).ceil() as usize).clamp(1, 12);
            for k in 0..=n2 {
                let v = step2 * k as f64;
                let txt = tick(v);
                let frac = (v / max_sub) as f32;
                chart_text(out, px0 + pw + 3.0, py0 + ph * frac - 2.5, 7.0, &txt, [90, 90, 90]);
            }
        }
        let scatter = !vis_series.is_empty()
            && vis_series
                .iter()
                .all(|(_, se)| matches!(series_kind(se, ch.kind), ChartKind::Scatter | ChartKind::Bubble));
        if !scatter
            && !matches!(ch.kind, ChartKind::Histogram | ChartKind::BoxWhisker | ChartKind::Waterfall)
            && n > 0
            && n <= 12
        {
            let gw = pw / n as f32;
            if let Some(cats) = vis_series.iter().map(|(_, se)| &se.cats).find(|c| !c.is_empty()) {
                for (i, c) in cats.iter().take(n).enumerate() {
                    let tw = out.tw(c, 7.0);
                    chart_text(out, px0 + gw * (i as f32 + 0.5) - tw / 2.0, py0 - 9.0, 7.0, c, [90, 90, 90]);
                }
            }
        }
    }
    if matches!(ch.kind, ChartKind::Pie | ChartKind::Doughnut) {
        let vals = vis_series.first().map(|(_, s)| s.vals.clone()).unwrap_or_default();
        let total: f64 = vals.iter().copied().filter(|v| v.is_finite() && *v > 0.0).sum::<f64>().max(1e-9);
        let cx = px0 + pw / 2.0;
        let cy = py0 + ph / 2.0;
        let rad = (pw.min(ph) / 2.0 - 4.0).max(4.0);
        let mut start_a = std::f32::consts::FRAC_PI_2 - (ch.style.first_slice_ang as f32).to_radians();
        for (i, v) in vals.iter().enumerate() {
            if !v.is_finite() || *v <= 0.0 {
                continue;
            }
            let sweep = (*v / total) as f32 * std::f32::consts::TAU;
            let steps = ((sweep / 0.15).ceil() as usize).max(2);
            let mut pts = vec![(cx, cy)];
            for k in 0..=steps {
                let t = start_a - sweep * (k as f32 / steps as f32);
                pts.push((cx + rad * t.cos(), cy + rad * t.sin()));
            }
            out.polys.push((pts, th.palette[i % th.palette.len()]));
            start_a -= sweep;
        }
        if matches!(ch.kind, ChartKind::Doughnut) {
            let ir = rad * (ch.style.hole_size.clamp(10, 85) as f32 / 100.0);
            let mut hole = Vec::with_capacity(28);
            for k in 0..28 {
                let a = std::f32::consts::TAU * k as f32 / 28.0;
                hole.push((cx + ir * a.cos(), cy + ir * a.sin()));
            }
            out.polys.push((hole, th.bg));
        }
    } else if matches!(ch.kind, ChartKind::Radar) {
        let groups = n.max(2);
        let cx = px0 + pw / 2.0;
        let cy = py0 + ph / 2.0;
        let rad = (pw.min(ph) / 2.0 - 6.0).max(6.0);
        for &(si, se) in &vis_series {
            // 保留 c19_radar 测试默认的 CHART_COLORS 50% 白混色基线
            let color = se.color.unwrap_or(CHART_COLORS[si % CHART_COLORS.len()]);
            let pts: Vec<(f32, f32)> = se
                .vals
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let ang = std::f32::consts::FRAC_PI_2 - i as f32 * std::f32::consts::TAU / groups as f32;
                    let r = rad * (*v / max_main) as f32;
                    (cx + r * ang.cos(), cy + r * ang.sin())
                })
                .collect();
            if ch.radar_style == "filled" && pts.len() >= 3 {
                let mix = |c: u8| ((c as u16 + 255) / 2) as u8;
                let fill = [mix(color[0]), mix(color[1]), mix(color[2])];
                out.polys.push((pts.clone(), fill));
            }
            for k in 0..pts.len() {
                let a = pts[k];
                let b = pts[(k + 1) % pts.len()];
                out.lines.push((a.0, a.1, b.0, b.1, 1.2, color));
                if ch.radar_style != "filled" {
                    out.rects.push((a.0 - 1.5, a.1 - 1.5, 3.0, 3.0, color));
                }
            }
        }
    } else if matches!(ch.kind, ChartKind::Waterfall) {
        if let Some((_, s)) = vis_series.first() {
            let bars = crate::chart_engine::compute_waterfall(s, &ch.analytics.waterfall_totals);
            let (mut dmin, mut dmax) = (0.0f64, 0.0f64);
            for b in &bars {
                dmin = dmin.min(b.y_start).min(b.y_end);
                dmax = dmax.max(b.y_start).max(b.y_end);
            }
            let span = (dmax - dmin).max(1e-9);
            let slot = pw / bars.len().max(1) as f32;
            let bw = (slot * 0.62).max(2.0);
            for (i, b) in bars.iter().enumerate() {
                let bx = px0 + i as f32 * slot + (slot - bw) * 0.5;
                let ya = py0 + ph * ((b.y_start - dmin) / span) as f32;
                let yb = py0 + ph * ((b.y_end - dmin) / span) as f32;
                let col = match b.kind {
                    crate::chart_engine::WaterfallBarKind::Increase => th.pos_color,
                    crate::chart_engine::WaterfallBarKind::Decrease => th.neg_color,
                    crate::chart_engine::WaterfallBarKind::Total => th.total_color,
                };
                out.rects.push((bx, ya.min(yb), bw, (ya - yb).abs().max(1.5), col));
                if i + 1 < bars.len() {
                    let nx = px0 + (i + 1) as f32 * slot + (slot - bw) * 0.5;
                    out.lines.push((bx + bw, yb, nx, yb, 0.5, th.axis_line));
                }
            }
        }
    } else if matches!(ch.kind, ChartKind::Funnel) {
        if let Some((_, s)) = vis_series.first() {
            let stages = crate::chart_engine::compute_funnel(s);
            let cnt = stages.len().max(1);
            let rh = (ph / cnt as f32).min(24.0);
            let cx = px0 + pw * 0.5;
            for (i, st) in stages.iter().enumerate() {
                let wt = pw * 0.72 * st.width_frac as f32;
                let wb = stages
                    .get(i + 1)
                    .map(|nx| pw * 0.72 * nx.width_frac as f32)
                    .unwrap_or(wt * 0.8);
                let y_top = py0 + ph - i as f32 * rh - 2.0;
                let y_bot = y_top - rh + 4.0;
                out.polys.push((
                    vec![
                        (cx - wt * 0.5, y_top),
                        (cx + wt * 0.5, y_top),
                        (cx + wb * 0.5, y_bot),
                        (cx - wb * 0.5, y_bot),
                    ],
                    th.palette[i % th.palette.len()],
                ));
            }
        }
    } else if matches!(ch.kind, ChartKind::Treemap) {
        if let Some((_, s)) = vis_series.first() {
            for (i, t) in crate::chart_engine::compute_treemap(s, px0, py0, pw, ph)
                .into_iter()
                .enumerate()
            {
                let ry = py0 + ph - (t.y - py0) - t.h;
                out.rects.push((t.x, ry, t.w.max(1.0), t.h.max(1.0), th.palette[i % th.palette.len()]));
            }
        }
    } else if matches!(ch.kind, ChartKind::Histogram) {
        if let Some((_, s)) = vis_series.first() {
            let bins = crate::chart_engine::compute_histogram(
                s,
                ch.analytics.hist_bins,
                ch.analytics.show_pareto_line,
            );
            let mc = bins.iter().map(|b| b.count).max().unwrap_or(1) as f64;
            let slot = pw / bins.len().max(1) as f32;
            let col = th.palette[0];
            let mut ppts = Vec::new();
            for (i, b) in bins.iter().enumerate() {
                let bx = px0 + i as f32 * slot + 1.0;
                let bw = (slot - 2.0).max(1.5);
                let bh = ph * (b.count as f64 / mc) as f32;
                out.rects.push((bx, py0, bw, bh, col));
                ppts.push((bx + bw * 0.5, py0 + ph * b.cum_pct as f32));
            }
            if ch.analytics.show_pareto_line {
                for w2 in ppts.windows(2) {
                    out.lines.push((w2[0].0, w2[0].1, w2[1].0, w2[1].1, 1.3, th.palette[1]));
                }
            }
        }
    } else if matches!(ch.kind, ChartKind::BoxWhisker) {
        let boxes = crate::chart_engine::compute_box_whisker(ch);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for b in &boxes {
            lo = lo.min(b.whisker_lo);
            hi = hi.max(b.whisker_hi);
        }
        let span = (hi - lo).max(1e-9);
        let slot = pw / boxes.len().max(1) as f32;
        let bw = (slot * 0.42).max(4.0);
        for (i, b) in boxes.iter().enumerate() {
            let cx = px0 + (i as f32 + 0.5) * slot;
            let y_of = |v: f64| py0 + ph * ((v - lo) / span) as f32;
            let col = th.palette[i % th.palette.len()];
            out.lines.push((cx, y_of(b.whisker_lo), cx, y_of(b.whisker_hi), 1.0, col));
            let y1 = y_of(b.q1);
            let y3 = y_of(b.q3);
            out.rects.push((cx - bw * 0.5, y1.min(y3), bw, (y3 - y1).abs().max(2.0), col));
            let ym = y_of(b.median);
            out.lines.push((cx - bw * 0.5, ym, cx + bw * 0.5, ym, 1.6, [255, 255, 255]));
        }
    } else {
        // 标准笛卡尔系：簇状/堆积/百分比堆积柱条、折线、面积、散点、气泡、组合图
        let bar_slots: Vec<usize> = vis_series
            .iter()
            .filter(|(_, s)| {
                let k = series_kind(s, ch.kind);
                matches!(
                    k,
                    ChartKind::Column
                        | ChartKind::ColumnStacked
                        | ChartKind::ColumnStacked100
                        | ChartKind::Bar
                        | ChartKind::BarStacked
                        | ChartKind::BarStacked100
                )
            })
            .map(|(i, _)| *i)
            .collect();
        let slot_count = bar_slots.len().max(1);
        let groups = n.max(1);
        let gw = pw / groups as f32;
        let bw = (gw * 0.7 / slot_count as f32).max(1.5);
        for &(si, se) in &vis_series {
            let color = se.color.unwrap_or(CHART_COLORS[si % CHART_COLORS.len()]);
            let scale = if se.secondary { max_sub } else { max_main };
            match series_kind(se, ch.kind) {
                ChartKind::Line | ChartKind::LineStacked | ChartKind::Scatter | ChartKind::Bubble => {
                    let sk = series_kind(se, ch.kind);
                    let is_scatter = matches!(sk, ChartKind::Scatter | ChartKind::Bubble);
                    let use_x = is_scatter && se.xv.len() == se.vals.len() && !se.xv.is_empty();
                    let (xmin, xspan) = if use_x {
                        let lo = se.xv.iter().copied().fold(f64::MAX, f64::min);
                        let hi = se.xv.iter().copied().fold(f64::MIN, f64::max);
                        (lo, (hi - lo).max(1e-9))
                    } else {
                        (0.0, (se.vals.len().saturating_sub(1)).max(1) as f64)
                    };
                    let pts: Vec<(f32, f32)> = se
                        .vals
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let fx = if use_x {
                                (se.xv[i] - xmin) / xspan
                            } else {
                                i as f64 / xspan
                            };
                            (px0 + pw * fx as f32, py0 + ph * (*v / scale) as f32)
                        })
                        .filter(|p| p.0.is_finite() && p.1.is_finite())
                        .collect();
                    if !is_scatter && pts.len() >= 2 {
                        for seg in pts.windows(2) {
                            out.lines.push((seg[0].0, seg[0].1, seg[1].0, seg[1].1, 1.4, color));
                        }
                    }
                    if is_scatter || pts.len() <= 60 {
                        let r = if is_scatter { 2.0 } else { 1.5 };
                        for p in &pts {
                            out.rects.push((p.0 - r, p.1 - r, r * 2.0, r * 2.0, color));
                        }
                    }
                }
                ChartKind::Area | ChartKind::AreaStacked | ChartKind::AreaStacked100 => {
                    let step = if se.vals.len() > 1 {
                        pw / (se.vals.len() as f32 - 1.0)
                    } else {
                        0.0
                    };
                    let mut pts = vec![(px0, py0)];
                    for (i, v) in se.vals.iter().enumerate() {
                        if v.is_finite() {
                            pts.push((px0 + step * i as f32, py0 + ph * (*v / scale) as f32));
                        }
                    }
                    pts.push((px0 + step * (se.vals.len().saturating_sub(1)) as f32, py0));
                    out.polys.push((pts, color));
                }
                _ => {
                    let slot = bar_slots.iter().position(|i| *i == si).unwrap_or(0);
                    for (i, v) in se.vals.iter().enumerate() {
                        if !v.is_finite() {
                            continue;
                        }
                        let bh = ph * (*v / scale) as f32;
                        let bx = px0 + gw * (i as f32 + 0.15) + bw * slot as f32;
                        out.rects.push((bx, py0, bw, bh, color));
                    }
                }
            }
            // 趋势线 PDF 输出
            for tl in &se.trendlines {
                let raw: Vec<(f64, f64)> = se.vals.iter().enumerate().map(|(i, &v)| (i as f64, v)).collect();
                if let Some(comp) = crate::chart_engine::compute_trendline(&raw, tl) {
                    let tcol = tl.color.unwrap_or(color);
                    let denom = se.vals.len().max(1) as f64;
                    let tpts: Vec<(f32, f32)> = comp
                        .points
                        .iter()
                        .map(|&(tx, ty)| {
                            (
                                px0 + pw * ((tx + 0.5) / denom) as f32,
                                py0 + ph * (ty / scale) as f32,
                            )
                        })
                        .collect();
                    for seg in tpts.windows(2) {
                        out.lines.push((seg[0].0, seg[0].1, seg[1].0, seg[1].1, 1.0, tcol));
                    }
                }
            }
        }
    }
    if has_sub {
        out.lines.push((px0 + pw, py0, px0 + pw, py0 + ph, 0.5, [120, 120, 120]));
    }
    if legend_w > 0.0 {
        let mut ly = y + h - 18.0;
        for &(i, se) in vis_series.iter().take(6) {
            let color = se.color.unwrap_or(CHART_COLORS[i % CHART_COLORS.len()]);
            out.rects.push((x + w - legend_w + 4.0, ly, 8.0, 8.0, color));
            chart_text(out, x + w - legend_w + 16.0, ly + 7.0, 7.5, &se.name, [60, 60, 60]);
            ly -= 12.0;
        }
    }
    let ax_y = ch.effective_y_axis().title;
    let ax_x = ch.effective_x_axis().title;
    if !ax_y.is_empty() {
        chart_text(out, x + 2.0, y + h - 16.0, 7.5, &ax_y, [80, 80, 80]);
    }
    if !ax_x.is_empty() {
        chart_text(out, x + w / 2.0 - 10.0, y + 10.0, 7.5, &ax_x, [80, 80, 80]);
    }
}

/// 图片/图表/文本框：只画在包含其锚点的那一页
fn draw_objects(
    sheet: &Sheet,
    span: &PageSpan,
    sx: f32,
    sy: f32,
    px: f32,
    source: Option<&Path>,
    out: &mut Out,
) {
    let contains = |r: u32, c: u32| {
        r >= span.rows.0 && r <= span.rows.1 && c >= span.cols.0 && c <= span.cols.1
    };
    for pic in &sheet.pictures {
        let (r1, c1, r2, c2) = pic.rect;
        if !contains(r1, c1) {
            continue;
        }
        let bytes = match &pic.data {
            Some(d) => Some(d.as_ref().clone()),
            None => source.and_then(|p| crate::pic::read_part(p, &pic.part)),
        };
        let Some(bytes) = bytes else { continue };
        let (x, y, w, h) = grid_rect(sheet, r1, c1, r2, c2, pic.off, sx, sy, px);
        let stem = Path::new(&pic.part)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "pic".into());
        let name: String = format!(
            "Im{}",
            stem.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>()
        );
        out.images.push((x, y, w, h, name, bytes, pic.ext.clone()));
    }
    for ch in &sheet.charts {
        if contains(ch.anchor.0, ch.anchor.1) {
            draw_chart(sheet, ch, sx, sy, px, out);
        }
    }
    for tb in &sheet.textboxes {
        let (c1, dx1, r1, dy1) = tb.from;
        let (c2, dx2, r2, dy2) = tb.to;
        if !contains(r1, c1) {
            continue;
        }
        const EMU_PX: f32 = 9525.0;
        let x = (0..c1).map(|c| col_w(sheet, c) * px).sum::<f32>() + sx + dx1 as f32 / EMU_PX * px;
        let y_top = (0..r1).map(|r| row_h(sheet, r) * px).sum::<f32>() + dy1 as f32 / EMU_PX * px;
        let x2 = (0..c2).map(|c| col_w(sheet, c) * px).sum::<f32>() + sx + dx2 as f32 / EMU_PX * px;
        let y2 = (0..r2).map(|r| row_h(sheet, r) * px).sum::<f32>() + dy2 as f32 / EMU_PX * px;
        let w = (x2 - x).max(8.0);
        let h = (y2 - y_top).max(8.0);
        let yy = sy - y_top - h;
        if let Some(fill) = tb.fill {
            out.rects.push((x, yy, w, h, fill));
        }
        let size = tb.size.max(4.0);
        let (tb_asc, tb_desc) = match &out.vmetric {
            Some(f) => f(size),
            None => (size * 0.92, size * 0.26),
        };
        let mut ty = yy + h - tb_asc;
        for line in display::wrap_lines_with(&tb.text, (w - 4.0).max(4.0), size, out.metric.as_deref()) {
            out.texts.push(TextOp {
                x: x + 2.0,
                baseline: ty,
                size,
                text: line,
                color: tb.color,
                bold: tb.bold,
                italic: tb.italic,
                rotation: 0.0,
            });
            ty -= tb_asc + tb_desc;
        }
    }
}

/// 生成一页的绘制指令（预览直接复用；PDF 再把文字编码成字形号）
pub fn page_ops(
    wb: &Workbook,
    sheet_idx: usize,
    layout: &Layout,
    page_index: usize,
) -> Out {
    page_ops_with(wb, sheet_idx, layout, page_index, None)
}

/// 同上；`source` 是打开的工作簿路径，用来读取文件里的图片部件
pub fn page_ops_with(
    wb: &Workbook,
    sheet_idx: usize,
    layout: &Layout,
    page_index: usize,
    source: Option<&Path>,
) -> Out {
    page_ops_metric(wb, sheet_idx, layout, page_index, source, None)
}

/// 同上，并挂上真字体度量（导出 PDF 用；预览传 None）
pub fn page_ops_metric(
    wb: &Workbook,
    sheet_idx: usize,
    layout: &Layout,
    page_index: usize,
    source: Option<&Path>,
    metric: Option<std::sync::Arc<dyn Fn(char, f32) -> f32 + Send + Sync>>,
) -> Out {
    page_ops_metrics(wb, sheet_idx, layout, page_index, source, metric, None)
}

/// 同上，再挂上纵向度量（ascender/descender）：行高与基线按真字体算，不再用字号乘常数
pub fn page_ops_metrics(
    wb: &Workbook,
    sheet_idx: usize,
    layout: &Layout,
    page_index: usize,
    source: Option<&Path>,
    metric: Option<std::sync::Arc<dyn Fn(char, f32) -> f32 + Send + Sync>>,
    vmetric: Option<std::sync::Arc<dyn Fn(f32) -> (f32, f32) + Send + Sync>>,
) -> Out {
    let mut state = PageState::new(wb, sheet_idx);
    render_page(wb, sheet_idx, layout, page_index, source, metric, vmetric, &mut state)
}

fn render_page(
    wb: &Workbook,
    sheet_idx: usize,
    layout: &Layout,
    page_index: usize,
    source: Option<&Path>,
    metric: Option<std::sync::Arc<dyn Fn(char, f32) -> f32 + Send + Sync>>,
    vmetric: Option<std::sync::Arc<dyn Fn(f32) -> (f32, f32) + Send + Sync>>,
    state: &mut PageState,
) -> Out {
    let sheet = &wb.sheets[sheet_idx];
    let setup = &layout.setup;
    let (pw, ph) = layout.page_size;
    let (ml, mr, mt, mb) = margins_pt(setup);
    let px = layout.scale * 0.75;
    let span = &layout.pages[page_index];
    let mut out = Out::default();
    out.metric = metric;
    out.vmetric = vmetric;

    // 页眉
    if !setup.header.is_empty() {
        let (l, c, r) = header_parts(&setup.header, page_index + 1, layout.pages.len());
        for (seg, align) in [(l, 0u8), (c, 1), (r, 2)] {
            if seg.is_empty() {
                continue;
            }
            let tw = out.tw(&seg, 9.0);
            let tx = match align {
                1 => (pw - tw) / 2.0,
                2 => pw - mr - tw,
                _ => ml,
            };
            out.texts.push(TextOp {
                x: tx,
                baseline: ph - mt + 18.0,
                size: 9.0,
                text: seg,
                color: [90, 90, 90],
                bold: false,
                italic: false,
                rotation: 0.0,
            });
        }
    }

    let mut y = ph - mt;
    if let Some((ta, tb)) = setup.title_rows {
        for r in ta..=tb.min(span.rows.1).max(ta) {
            let h = row_h(sheet, r) * px;
            if h <= 0.0 {
                continue;
            }
            let mut x = ml;
            for c in span.cols.0..=span.cols.1 {
                let w = col_w(sheet, c) * px;
                if w <= 0.0 {
                    continue;
                }
                draw_cell(wb, sheet, sheet_idx, state, r, c, x, y - h, w, h, px, layout.scale, &mut out);
                x += w;
            }
            y -= h;
        }
    }
    let title_w: f32 = match setup.title_cols {
        Some((ca, cb)) => {
            let mut x = ml;
            for c in ca..=cb.min(span.cols.1).max(ca) {
                let w = col_w(sheet, c) * px;
                if w <= 0.0 {
                    continue;
                }
                let mut yy = y;
                for r in span.rows.0..=span.rows.1 {
                    let h = row_h(sheet, r) * px;
                    if h <= 0.0 {
                        continue;
                    }
                    draw_cell(wb, sheet, sheet_idx, state, r, c, x, yy - h, w, h, px, layout.scale, &mut out);
                    yy -= h;
                }
                x += w;
            }
            x - ml
        }
        None => 0.0,
    };

    // 正文
    let body_x0 = ml + title_w;
    let mut cursor_y = y;
    for r in span.rows.0..=span.rows.1 {
        let h = row_h(sheet, r) * px;
        if h <= 0.0 {
            continue;
        }
        let mut x = body_x0;
        for c in span.cols.0..=span.cols.1 {
            let w = col_w(sheet, c) * px;
            if w <= 0.0 {
                continue;
            }
            draw_cell(wb, sheet, sheet_idx, state, r, c, x, cursor_y - h, w, h, px, layout.scale, &mut out);
            x += w;
        }
        cursor_y -= h;
    }

    // 网格线（正文格线，标题区也一起）
    if setup.gridlines {
        let mut body_w = 0.0f32;
        for c in span.cols.0..=span.cols.1 {
            body_w += col_w(sheet, c) * px;
        }
        let mut body_h = 0.0f32;
        for r in span.rows.0..=span.rows.1 {
            body_h += row_h(sheet, r) * px;
        }
        let mut x = body_x0;
        for c in span.cols.0..=span.cols.1 {
            out.lines.push((x, y - body_h, x, y, 0.4, [205, 205, 205]));
            x += col_w(sheet, c) * px;
        }
        out.lines.push((x, y - body_h, x, y, 0.4, [205, 205, 205]));
        let mut yy = y;
        for r in span.rows.0..=span.rows.1 {
            out.lines.push((body_x0, yy, body_x0 + body_w, yy, 0.4, [205, 205, 205]));
            yy -= row_h(sheet, r) * px;
        }
        out.lines.push((body_x0, yy, body_x0 + body_w, yy, 0.4, [205, 205, 205]));
    }

    // 页脚
    if !setup.footer.is_empty() {
        let (fl, fc, fr) = header_parts(&setup.footer, page_index + 1, layout.pages.len());
        for (seg, align) in [(fl, 0u8), (fc, 1), (fr, 2)] {
            if seg.is_empty() {
                continue;
            }
            let tw = out.tw(&seg, 8.5);
            let tx = match align {
                1 => (pw - tw) / 2.0,
                2 => pw - mr - tw,
                _ => ml,
            };
            out.texts.push(TextOp {
                x: tx,
                baseline: mb * 0.4,
                size: 8.5,
                text: seg,
                color: [130, 130, 130],
                bold: false,
                italic: false,
                rotation: 0.0,
            });
        }
    }
    draw_objects(sheet, span, body_x0, y, px, source, &mut out);
    let _ = (pw, mb);
    out
}

/// 导出 PDF；返回页数
pub fn export_pdf(
    wb: &Workbook,
    sheet_idx: usize,
    path: &Path,
    font_path: &Path,
) -> Result<usize> {
    export_pdf_from(wb, sheet_idx, path, font_path, None)
}

/// 导出 PDF，并从 `source` 读取 xl/media 里的图片部件
pub fn export_pdf_from(
    wb: &Workbook,
    sheet_idx: usize,
    path: &Path,
    font_path: &Path,
    source: Option<&Path>,
) -> Result<usize> {
    export_pdf_pages_from(wb, sheet_idx, path, font_path, source, None)
}

/// 导出指定页范围的 PDF。`pages` 为 0 基闭区间；None 表示全部。
/// 页码/页眉页脚仍按原分页编号，便于打印“第 3-5 页”时与预览一致。
pub fn export_pdf_pages_from(
    wb: &Workbook,
    sheet_idx: usize,
    path: &Path,
    font_path: &Path,
    source: Option<&Path>,
    pages: Option<(usize, usize)>,
) -> Result<usize> {
    let font = Font::load(font_path)?;
    export_pdf_pages_with_fonts(wb, sheet_idx, path, font, None, source, pages)
}

/// 与 `export_pdf_pages_from` 相同，但直接给字体对象，并可带一套真粗体字体。
/// 粗体文字改用第二条嵌入字体（/F2）下发真 700 字形，不再是描边模拟。
pub fn export_pdf_pages_with_fonts(
    wb: &Workbook,
    sheet_idx: usize,
    path: &Path,
    font: Font,
    bold: Option<Font>,
    source: Option<&Path>,
    pages: Option<(usize, usize)>,
) -> Result<usize> {
    let Some(sheet) = wb.sheets.get(sheet_idx) else {
        anyhow::bail!("没有第 {sheet_idx} 张表");
    };
    let layout = paginate(sheet, &sheet.page);
    let total = layout.pages.len();
    let (first, last) = match pages {
        Some((a, b)) => (a.min(total.saturating_sub(1)), b.min(total.saturating_sub(1))),
        None => (0, total.saturating_sub(1)),
    };
    let (asc_em, desc_em) = font.em_metrics();
    let vmetric: Option<std::sync::Arc<dyn Fn(f32) -> (f32, f32) + Send + Sync>> = Some(
        std::sync::Arc::new(move |size: f32| (asc_em * size, desc_em * size)),
    );
    let metric: Option<std::sync::Arc<dyn Fn(char, f32) -> f32 + Send + Sync>> = None;
    let mut doc = Doc::new(font);
    if let Some(b) = bold {
        doc.set_bold_font(b);
    }
    let mut state = PageState::new(wb, sheet_idx);
    let mut exported = 0usize;
    for pi in first..=last {
        if pi >= total {
            break;
        }
        exported += 1;
        let (pw, ph) = layout.page_size;
        doc.add_page(pw, ph);
        let out = render_page(
            wb,
            sheet_idx,
            &layout,
            pi,
            source,
            metric.clone(),
            vmetric.clone(),
            &mut state,
        );
        for (x, y, w, h, color) in out.rects {
            doc.page_mut().rect_fill(x, y, w, h, color);
        }
        for (x1, y1, x2, y2, lw, color) in out.lines {
            doc.page_mut().line(x1, y1, x2, y2, lw, color);
        }
        for (pts, color) in out.polys {
            doc.page_mut().poly(&pts, color, true);
        }
        for (x, y, w, h, name, bytes, ext) in out.images {
            if let Some(img) = crate::image_embed::make(&name, &ext, &bytes) {
                let name = doc.add_image(img);
                doc.page_mut().image(&name, x, y, w, h);
            }
        }
        let has_bold = doc.bold_font().is_some();
        let encoded: Vec<(TextOp, String, bool)> = out
            .texts
            .into_iter()
            .map(|t| {
                let use_bold_face = t.bold && has_bold;
                let font = doc.font_for(use_bold_face);
                let hex = Page::glyph_hex(font, &t.text);
                (t, hex, use_bold_face)
            })
            .collect();
        for (t, hex, use_bold_face) in encoded {
            if use_bold_face {
                doc.page_mut().text_styled_face(
                    "F2", t.x, t.baseline, t.size, &hex, t.color, t.italic, t.rotation,
                );
            } else {
                doc.page_mut().text_styled(
                    t.x, t.baseline, t.size, &hex, t.color, t.bold, t.italic, t.rotation,
                );
            }
        }
    }
    let n = exported;
    doc.write(path)?;
    Ok(n)
}

fn fvalue_to_value(v: crate::formula::FValue) -> crate::model::Value {
    use crate::formula::FValue;
    use crate::model::Value;
    match v {
        FValue::Empty => Value::Empty,
        FValue::Number(n) => Value::Number(n),
        FValue::Text(t) => Value::Text(t),
        FValue::Bool(b) => Value::Bool(b),
        FValue::Error(e) => Value::Error(e),
        FValue::Array(a) => a
            .data
            .first()
            .cloned()
            .map(|x| fvalue_to_value(x))
            .unwrap_or(Value::Empty),
        FValue::Lambda(_) => Value::Error(crate::formula::ErrKind::Value),
    }
}

/// 系列的实际图型：系列自己指定的优先；组合图里没指定时，最后一条按折线、其余按柱
fn series_kind(se: &crate::charts::Series, chart_kind: crate::charts::ChartKind) -> crate::charts::ChartKind {
    use crate::charts::ChartKind;
    if let Some(k) = se.kind {
        return k;
    }
    match chart_kind {
        ChartKind::Combo => ChartKind::Column,
        k => k,
    }
}

/// 刻度步长向上取「整齐数」：1/2/2.5/5×10^n
fn nice_step(x: f64) -> f64 {
    if !(x > 0.0) || !x.is_finite() {
        return 1.0;
    }
    let base = 10f64.powf(x.log10().floor());
    for m in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if x <= m * base * (1.0 + 1e-9) {
            return m * base;
        }
    }
    base * 10.0
}

/// 坐标轴顶端取“整齐”数值（1/2/2.5/5×10^n），与 Excel 的百分比轴 0/25/50/75/100% 一致
fn nice_max(v: f64) -> f64 {
    if !(v > 0.0) || !v.is_finite() {
        return 1.0;
    }
    let base = 10f64.powf(v.log10().floor());
    for m in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if v <= m * base * (1.0 + 1e-9) {
            return m * base;
        }
    }
    base * 10.0
}


#[cfg(test)]
mod wave5_print_tests {
    use super::*;
    use crate::model::{Cell, Sheet, Value};

    #[test]
    fn paginate_million_row_filtered_sheet_and_fit_to_page_runs_in_o_spans() {
        let mut sh = Sheet::new("MillionPrint");
        sh.set(0, 0, Cell { value: Value::Text("Title".into()), formula: None });
        sh.set(999_999, 3, Cell { value: Value::Number(42.0), formula: None });
        // Hide rows 20..=999_989 as a single filtered interval -> only 30 visible rows!
        sh.hidden_rows = vec![(20, 999_989)].into();

        let mut setup = PageSetup::default();
        let t0 = std::time::Instant::now();
        let layout = paginate(&sh, &setup);
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert!(elapsed_ms < 20.0, "expected O(spans) < 20 ms, got {elapsed_ms:.2} ms");
        assert_eq!(layout.pages.len(), 1, "30 visible rows with 999,970 hidden rows must fit on 1 page");
        assert_eq!(layout.pages[0].rows, (0, 999_999));

        // Fit-to-page on 1M visible rows also finishes in O(spans)
        sh.hidden_rows = Default::default();
        setup.fit_to_page = true;
        setup.fit_width = 1;
        setup.fit_height = 1;
        let t1 = std::time::Instant::now();
        let fit_layout = paginate(&sh, &setup);
        assert!(t1.elapsed().as_secs_f64() * 1000.0 < 20.0);
        assert!(!fit_layout.pages.is_empty());
    }
}
