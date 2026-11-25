//! 统一出版级渲染、分页与 PDF/硬件打印显示列表引擎（预览、PDF 与原生 GDI 打印 100% 同源）。
//!
//! 坐标口径：列宽/行高为 96 DPI 像素，页面矢量指令统一使用 PDF 点（72 DPI，原点在左下，y 向上），
//! 100% 缩放时 `1 px = 0.75 pt`。
//!
//! 本模块覆盖：
//! - 精确 O(spans) 百万行分页、先行后列/先列后行顺序、重复标题行×重复标题列交叉角、居中与行列号表头；
//! - 13 种 OOXML 边框线型（含双线/虚线/点线/点划线/斜线）+ 条件格式差异边框/图标集/数据条 + 迷你图；
//! - 富文本逐段样式、下划线/双下划线/删除线、缩小字体填充、竖排文字、文本旋转与单元格精准裁剪；
//! - 跨页精准定位的图片/图表/文本框、原位或表尾批注汇总页、错误值替换、黑白/草稿模式与原生矢量水印；
//! - 完整 Excel 页眉页脚语法（`&L/&C/&R/&P/&N/&D/&T/&F/&A/&Z/&&`，含首页与奇偶页差异及页眉页脚边距）；
//! - 单表 / 多表工作簿 PDF 导出（含侧边栏书签目录 `/Outlines`、多字体回退链与智能页码解析 `"1,3-5,8"`）。

use crate::cf::{IconMark, IconStyle};
use crate::display::{self, CellView};
use crate::model::{col_label, Sheet, Workbook};
use crate::page::PageSetup;
use crate::pdf::{Doc, Font};
use crate::sparkline::{EmptyAs, SparkKind};
use crate::style::Border;
use anyhow::Result;
use std::path::Path;

/// 纸张尺寸（pt）：直接复用权威 ISO/IEC 29500 `page::paper_pt`
fn paper_size(paper: u16) -> (f32, f32) {
    crate::page::paper_pt(paper)
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

impl Layout {
    pub fn setup(&self) -> &PageSetup {
        &self.setup
    }
}

/// 解析自定义页码范围串（如 `"1, 3-5, 8"`），返回去重保序的 0 基页码列表；空串表示全部页
pub fn parse_page_ranges(spec: &str, total_pages: usize) -> Vec<usize> {
    if total_pages == 0 {
        return Vec::new();
    }
    let s = spec.trim();
    if s.is_empty() {
        return (0..total_pages).collect();
    }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for part in s.split([',', '，', ';', '；']) {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if let Some((a, b)) = p.split_once('-').or_else(|| p.split_once('–')) {
            let Ok(lo) = a.trim().parse::<usize>() else { continue };
            let Ok(hi) = b.trim().parse::<usize>() else { continue };
            let (lo, hi) = (lo.max(1).min(total_pages), hi.max(1).min(total_pages));
            let (start, end) = if lo <= hi { (lo, hi) } else { (hi, lo) };
            for one in start..=end {
                let idx = one - 1;
                if seen.insert(idx) {
                    out.push(idx);
                }
            }
        } else if let Ok(one) = p.parse::<usize>()
            && one >= 1
            && one <= total_pages
        {
            let idx = one - 1;
            if seen.insert(idx) {
                out.push(idx);
            }
        }
    }
    if out.is_empty() {
        (0..total_pages).collect()
    } else {
        out
    }
}

fn margins_pt(setup: &PageSetup) -> (f32, f32, f32, f32) {
    let m = setup.margins;
    (m[0] * 72.0, m[1] * 72.0, m[2] * 72.0, m[3] * 72.0)
}

fn headings_reserve(setup: &PageSetup, scale: f32) -> (f32, f32) {
    if setup.headings {
        let k = scale.clamp(0.5, 1.5);
        (24.0 * k, 14.0 * k)
    } else {
        (0.0, 0.0)
    }
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
/// 完整支持：fitToWidth=0(自动)/fitToHeight=0(自动)、行列号表头占位、
/// 重复标题行与标题列扣减、手动分页符，以及先行后列 / 先列后行分页顺序。
pub fn paginate(sheet: &Sheet, setup: &PageSetup) -> Layout {
    let (pw, ph) = paper_size(setup.paper);
    let (pw, ph) = if setup.landscape { (ph, pw) } else { (pw, ph) };
    let (left, right, top, bottom) = margins_pt(setup);
    let printable = ((pw - left - right).max(72.0), (ph - top - bottom).max(72.0));

    let (r1, c1, mut r2, mut c2) = match setup.print_area {
        Some((ar1, ac1, ar2, ac2)) => {
            if (ar2.saturating_sub(ar1) as u64 + 1) * (ac2.saturating_sub(ac1) as u64 + 1)
                > 500_000
            {
                if let Some((_, _, ur2, uc2)) = sheet.used_bounds() {
                    (ar1, ac1, ar2.min(ur2.max(ar1)), ac2.min(uc2.max(ac1)))
                } else {
                    (ar1, ac1, ar1, ac1)
                }
            } else {
                (ar1, ac1, ar2, ac2)
            }
        }
        None => {
            let (mut max_r, mut max_c) = sheet
                .used_bounds()
                .map(|(_, _, r, c)| (r, c))
                .unwrap_or((0, 0));
            for ch in &sheet.charts {
                max_r = max_r.max(ch.anchor.2);
                max_c = max_c.max(ch.anchor.3);
            }
            for pic in &sheet.pictures {
                max_r = max_r.max(pic.rect.2);
                max_c = max_c.max(pic.rect.3);
            }
            for tb in &sheet.textboxes {
                max_r = max_r.max(tb.to.2);
                max_c = max_c.max(tb.to.0);
            }
            (0, 0, max_r, max_c)
        }
    };
    r2 = r2.max(r1);
    c2 = c2.max(c1);

    let hr1 = sheet.hidden_rows.intervals();
    let hr2 = sheet.hidden_rows_user.intervals();
    let hc1 = sheet.hidden_cols.intervals();

    let (hdr_w_est, hdr_h_est) = headings_reserve(setup, 1.0);
    let avail_w0 = (printable.0 - hdr_w_est).max(36.0);
    let avail_h0 = (printable.1 - hdr_h_est).max(36.0);

    let scale = if setup.fit_to_page {
        let mut best = f32::INFINITY;
        if setup.fit_width > 0 {
            let target_w = avail_w0 * setup.fit_width as f32;
            let total_w = sum_axis_px(&sheet.cols, hc1, &[], c1, c2);
            if total_w > 0.0 {
                best = best.min(target_w / (total_w * 0.75));
            }
        }
        if setup.fit_height > 0 {
            let target_h = avail_h0 * setup.fit_height as f32;
            let total_h = sum_axis_px(&sheet.rows, hr1, hr2, r1, r2);
            if total_h > 0.0 {
                best = best.min(target_h / (total_h * 0.75));
            }
        }
        if best.is_finite() {
            best.clamp(0.1, 1.0)
        } else {
            1.0
        }
    } else {
        (setup.scale.clamp(10, 400) as f32) / 100.0
    };
    let px = scale * 0.75;
    let (hdr_w, hdr_h) = headings_reserve(setup, scale);

    let title_rows = setup
        .title_rows
        .map(|(a, b)| (a, b.min(r2)))
        .filter(|(a, b)| a <= b);
    let title_cols = setup
        .title_cols
        .map(|(a, b)| (a, b.min(c2)))
        .filter(|(a, b)| a <= b);
    let head_rows_h: f32 = match title_rows {
        Some((a, b)) => sum_axis_px(&sheet.rows, hr1, hr2, a, b) * px,
        None => 0.0,
    };
    let head_cols_w: f32 = match title_cols {
        Some((a, b)) => sum_axis_px(&sheet.cols, hc1, &[], a, b) * px,
        None => 0.0,
    };

    let body_top = (printable.1 - hdr_h - head_rows_h).max(24.0);
    let body_left = (printable.0 - hdr_w - head_cols_w).max(24.0);
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
    if setup.page_order_over_then_down {
        // 先行后列（overThenDown）
        for &rows in &row_spans {
            for &cols in &col_spans {
                pages.push(PageSpan { rows, cols });
            }
        }
    } else {
        // 先列后行（downThenOver，Excel 默认）
        for &cols in &col_spans {
            for &rows in &row_spans {
                pages.push(PageSpan { rows, cols });
            }
        }
    }
    if pages.is_empty() {
        pages.push(PageSpan {
            rows: (r1, r2),
            cols: (c1, c2),
        });
    }
    Layout {
        setup: setup.clone(),
        pages,
        scale,
        printable,
        page_size: (pw, ph),
    }
}

/// 页眉页脚展开上下文（供 `&D / &T / &F / &A / &Z` 动态字段替换）
#[derive(Clone, Debug, Default)]
pub struct HeaderContext {
    pub sheet_name: String,
    pub file_name: String,
    pub folder_path: String,
    pub date_str: String,
    pub time_str: String,
}

fn current_date_time_strings() -> (String, String) {
    // 无外部 chrono 依赖：按系统时间转 UTC+8/本地日期（或直接用系统 epoch 计算公历年月日时分）
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // 默认东八区（与中文桌面环境一致）
    let local = secs + 8 * 3600;
    let days = local.div_euclid(86_400);
    let rem = local.rem_euclid(86_400);
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    // Howard Hinnant civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (format!("{y:04}-{m:02}-{d:02}"), format!("{hh:02}:{mm:02}"))
}

impl HeaderContext {
    pub fn for_workbook(wb: &Workbook, sheet_idx: usize) -> Self {
        let sheet_name = wb
            .sheets
            .get(sheet_idx)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let (file_name, folder_path) = match &wb.path {
            Some(p) => (
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                p.parent()
                    .map(|d| d.to_string_lossy().to_string())
                    .unwrap_or_default(),
            ),
            None => (wb.title(), String::new()),
        };
        let (date_str, time_str) = current_date_time_strings();
        Self {
            sheet_name,
            file_name,
            folder_path,
            date_str,
            time_str,
        }
    }
}

/// 页眉/页脚解析：`&L &C &R` 分左中右，`&P` 页码（支持 `&P+N`/`&P-N`）、`&N` 总页数、
/// `&D` 日期、`&T` 时间、`&F` 文件名、`&A` 工作表名、`&Z` 路径、`&&` 字面 `&`。
/// 自动忽略 Excel 内联字体格式标记（如 `&"宋体,常规"&10`、`&B`、`&I`、`&U`、`&K000000`）。
/// 没有分段标记时整段居中（修复旧版吞空 bug！）。
pub fn header_parts_with_ctx(
    text: &str,
    page: usize,
    total: usize,
    ctx: Option<&HeaderContext>,
) -> (String, String, String) {
    let (mut l, mut c, mut r) = (String::new(), String::new(), String::new());
    let mut seg = 1usize; // 默认居中
    let mut chars = text.chars().peekable();
    let push_seg = |seg: usize, s: &str, l: &mut String, c: &mut String, r: &mut String| match seg {
        0 => l.push_str(s),
        1 => c.push_str(s),
        _ => r.push_str(s),
    };
    while let Some(ch) = chars.next() {
        if ch == '&' {
            match chars.next() {
                Some('L' | 'l') => {
                    seg = 0;
                    continue;
                }
                Some('C' | 'c') => {
                    seg = 1;
                    continue;
                }
                Some('R' | 'r') => {
                    seg = 2;
                    continue;
                }
                Some('P' | 'p') => {
                    // 支持 Excel 的 &P+2 / &P-1 偏移语法
                    let mut num = page as isize;
                    if let Some(&sign @ ('+' | '-')) = chars.peek() {
                        chars.next();
                        let mut d = String::new();
                        while let Some(&dig) = chars.peek() {
                            if dig.is_ascii_digit() {
                                d.push(dig);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        if let Ok(delta) = d.parse::<isize>() {
                            num = if sign == '+' {
                                num + delta
                            } else {
                                (num - delta).max(1)
                            };
                        }
                    }
                    push_seg(seg, &num.to_string(), &mut l, &mut c, &mut r);
                    continue;
                }
                Some('N' | 'n') => {
                    push_seg(seg, &total.to_string(), &mut l, &mut c, &mut r);
                    continue;
                }
                Some('D' | 'd') => {
                    let s = ctx
                        .map(|x| x.date_str.clone())
                        .unwrap_or_else(|| current_date_time_strings().0);
                    push_seg(seg, &s, &mut l, &mut c, &mut r);
                    continue;
                }
                Some('T' | 't') => {
                    let s = ctx
                        .map(|x| x.time_str.clone())
                        .unwrap_or_else(|| current_date_time_strings().1);
                    push_seg(seg, &s, &mut l, &mut c, &mut r);
                    continue;
                }
                Some('F' | 'f') => {
                    if let Some(x) = ctx {
                        push_seg(seg, &x.file_name, &mut l, &mut c, &mut r);
                    }
                    continue;
                }
                Some('A' | 'a') => {
                    if let Some(x) = ctx {
                        push_seg(seg, &x.sheet_name, &mut l, &mut c, &mut r);
                    }
                    continue;
                }
                Some('Z' | 'z') => {
                    if let Some(x) = ctx {
                        push_seg(seg, &x.folder_path, &mut l, &mut c, &mut r);
                    }
                    continue;
                }
                Some('&') => {
                    push_seg(seg, "&", &mut l, &mut c, &mut r);
                    continue;
                }
                Some('"') => {
                    // 跳过 Excel 内联字体声明 &"微软雅黑,常规"
                    for q in chars.by_ref() {
                        if q == '"' {
                            break;
                        }
                    }
                    continue;
                }
                Some('K' | 'k') => {
                    // 跳过 6 位十六进制颜色 &KRRGGBB
                    for _ in 0..6 {
                        if chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    continue;
                }
                Some('B' | 'b' | 'I' | 'i' | 'U' | 'u' | 'E' | 'e' | 'S' | 's' | 'X' | 'x' | 'Y' | 'y') => {
                    // 跳过 Excel 内联样式切换码
                    continue;
                }
                Some(d) if d.is_ascii_digit() => {
                    // 跳过 Excel 字号设置 &12
                    while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                        chars.next();
                    }
                    continue;
                }
                Some(other) => {
                    let mut s = String::new();
                    s.push(other);
                    push_seg(seg, &s, &mut l, &mut c, &mut r);
                    continue;
                }
                None => break,
            }
        }
        let mut s = String::new();
        s.push(ch);
        push_seg(seg, &s, &mut l, &mut c, &mut r);
    }
    (l, c, r)
}

pub fn header_parts(text: &str, page: usize, total: usize) -> (String, String, String) {
    header_parts_with_ctx(text, page, total, None)
}

/// 带虚实线型的矢量直线指令（13 种 Excel 边框 + 趋势线 + 迷你图零线）
#[derive(Clone, Debug, PartialEq)]
pub struct StyledLine {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub width: f32,
    pub color: [u8; 3],
    pub dash: Vec<f32>,
}

/// 描边折线指令（折线图、散点连线、迷你图折线——绝不画成闭合实心色块）
#[derive(Clone, Debug, PartialEq)]
pub struct StrokePoly {
    pub pts: Vec<(f32, f32)>,
    pub width: f32,
    pub color: [u8; 3],
    pub dash: Vec<f32>,
}

/// 矢量水印绘制指令
#[derive(Clone, Debug, PartialEq)]
pub struct WatermarkOp {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub text: String,
    pub color: [u8; 3],
    pub angle_deg: f32,
    pub opacity: f32,
}

/// 页面绘制指令（预览、PDF 与原生 GDI 硬件打印 100% 共用）
#[derive(Default)]
pub struct Out {
    pub rects: Vec<(f32, f32, f32, f32, [u8; 3])>,
    pub alpha_rects: Vec<(f32, f32, f32, f32, [u8; 3], f32)>,
    pub lines: Vec<(f32, f32, f32, f32, f32, [u8; 3])>,
    pub styled_lines: Vec<StyledLine>,
    pub texts: Vec<TextOp>,
    /// 图片：(x, y, w, h, 名字, 原始字节, 扩展名)
    pub images: Vec<(f32, f32, f32, f32, String, Vec<u8>, String)>,
    /// 填充多边形（饼图扇区、面积图、填充雷达、条件格式图标形状）
    pub polys: Vec<(Vec<(f32, f32)>, [u8; 3])>,
    /// 描边折线（折线图、散点连线、迷你图折线）
    pub stroke_polys: Vec<StrokePoly>,
    /// 半透明水印层
    pub watermarks: Vec<WatermarkOp>,
    pub metric: Option<std::sync::Arc<dyn Fn(char, f32) -> f32 + Send + Sync>>,
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
    pub underline: bool,
    pub double_underline: bool,
    pub strikethrough: bool,
    /// 可选裁剪框 `[x, y_bottom, w, h]`（PDF 坐标系）
    pub clip: Option<[f32; 4]>,
    pub alpha: f32,
}

impl TextOp {
    #[allow(clippy::too_many_arguments)]
    pub fn simple(
        x: f32,
        baseline: f32,
        size: f32,
        text: String,
        color: [u8; 3],
        bold: bool,
        italic: bool,
        rotation: f32,
    ) -> Self {
        Self {
            x,
            baseline,
            size,
            text,
            color,
            bold,
            italic,
            rotation,
            underline: false,
            double_underline: false,
            strikethrough: false,
            clip: None,
            alpha: 1.0,
        }
    }
}

struct PageState<'a> {
    stats: Option<crate::cf::CfStats>,
    merges: crate::merges::MergeIndex,
    formulas: crate::formula::RuleFormulas,
    calc: crate::formula::Calc<'a>,
    hdr_ctx: HeaderContext,
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
        Self {
            stats,
            merges,
            formulas: Default::default(),
            calc: crate::formula::Calc::new(wb),
            hdr_ctx: HeaderContext::for_workbook(wb, sheet_idx),
        }
    }
}

/// 单元格首行基线的位置（PDF 坐标，y 向上；`top` 是下边、`top + ch` 是上边）。
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
        0 => bottom - asc,
        1 => bottom - (ch - total_h) / 2.0 - asc,
        _ => top + desc + (total_h - line_h),
    }
}

#[inline]
fn bw_color(rgb: [u8; 3], bw: bool) -> [u8; 3] {
    if !bw {
        return rgb;
    }
    let lum = (rgb[0] as u32 * 299 + rgb[1] as u32 * 587 + rgb[2] as u32 * 114) / 1000;
    if lum > 220 { [255, 255, 255] } else { [0, 0, 0] }
}

/// 按全部 13 种 OOXML 边框样式发射矢量直线（实线/粗线/细发丝/双平行线/虚线/点线/点划线）
#[allow(clippy::too_many_arguments)]
fn emit_border_line(
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    style: u8,
    color: [u8; 3],
    scale: f32,
    bw: bool,
    out: &mut Out,
) {
    if style == 0 {
        return;
    }
    let col = bw_color(color, bw);
    let sc = scale.clamp(0.4, 2.0);
    match style {
        1 => out.lines.push((x1, y1, x2, y2, 0.75 * sc, col)), // thin
        2 => out.lines.push((x1, y1, x2, y2, 1.45 * sc, col)), // medium
        3 => out.lines.push((x1, y1, x2, y2, 2.2 * sc, col)),  // thick
        4 => {
            // double：两条平行细线
            let dx = x2 - x1;
            let dy = y2 - y1;
            let len = (dx * dx + dy * dy).sqrt().max(1e-3);
            let nx = -dy / len * 0.85 * sc;
            let ny = dx / len * 0.85 * sc;
            out.lines
                .push((x1 + nx, y1 + ny, x2 + nx, y2 + ny, 0.55 * sc, col));
            out.lines
                .push((x1 - nx, y1 - ny, x2 - nx, y2 - ny, 0.55 * sc, col));
        }
        7 => out.lines.push((x1, y1, x2, y2, 0.4 * sc, col)), // hair
        5 => out.styled_lines.push(StyledLine {
            x1,
            y1,
            x2,
            y2,
            width: 0.75 * sc,
            color: col,
            dash: vec![3.6 * sc, 2.0 * sc],
        }),
        6 => out.styled_lines.push(StyledLine {
            x1,
            y1,
            x2,
            y2,
            width: 0.75 * sc,
            color: col,
            dash: vec![1.0 * sc, 1.5 * sc],
        }),
        8 => out.styled_lines.push(StyledLine {
            x1,
            y1,
            x2,
            y2,
            width: 0.75 * sc,
            color: col,
            dash: vec![4.0 * sc, 1.8 * sc, 1.0 * sc, 1.8 * sc],
        }),
        9 => out.styled_lines.push(StyledLine {
            x1,
            y1,
            x2,
            y2,
            width: 0.75 * sc,
            color: col,
            dash: vec![4.0 * sc, 1.5 * sc, 1.0 * sc, 1.5 * sc, 1.0 * sc, 1.5 * sc],
        }),
        10 => out.styled_lines.push(StyledLine {
            x1,
            y1,
            x2,
            y2,
            width: 1.45 * sc,
            color: col,
            dash: vec![4.5 * sc, 2.2 * sc],
        }),
        11 => out.styled_lines.push(StyledLine {
            x1,
            y1,
            x2,
            y2,
            width: 1.45 * sc,
            color: col,
            dash: vec![5.0 * sc, 2.0 * sc, 1.4 * sc, 2.0 * sc],
        }),
        12 => out.styled_lines.push(StyledLine {
            x1,
            y1,
            x2,
            y2,
            width: 1.45 * sc,
            color: col,
            dash: vec![5.0 * sc, 1.8 * sc, 1.4 * sc, 1.8 * sc, 1.4 * sc, 1.8 * sc],
        }),
        13 => out.styled_lines.push(StyledLine {
            x1,
            y1,
            x2,
            y2,
            width: 1.25 * sc,
            color: col,
            dash: vec![4.5 * sc, 1.8 * sc, 1.2 * sc, 1.8 * sc],
        }),
        _ => out.lines.push((x1, y1, x2, y2, 0.75 * sc, col)),
    }
}

/// 矢量绘制条件格式图标集（交通灯/方向箭头/旗帜/星标/符号，红绿方向严格符合 Excel：高档绿、低档红）
fn draw_cf_icon(
    icon: IconMark,
    cx: f32,
    cy: f32,
    rad: f32,
    bw: bool,
    out: &mut Out,
) {
    let levels = icon.levels.max(2);
    let ratio = icon.idx as f32 / (levels - 1) as f32;
    let col = if bw {
        if ratio >= 0.66 {
            [40, 40, 40]
        } else if ratio >= 0.33 {
            [120, 120, 120]
        } else {
            [185, 185, 185]
        }
    } else if ratio >= 0.66 {
        [34, 154, 78] // 高档绿
    } else if ratio >= 0.33 {
        [234, 170, 24] // 中档黄
    } else {
        [217, 63, 56] // 低档红
    };
    match icon.style {
        IconStyle::Arrows => {
            // 上/平/下矢量三角箭头
            let pts = if ratio >= 0.66 {
                vec![
                    (cx, cy + rad),
                    (cx + rad * 0.85, cy - rad * 0.65),
                    (cx - rad * 0.85, cy - rad * 0.65),
                ]
            } else if ratio >= 0.33 {
                vec![
                    (cx + rad, cy),
                    (cx - rad * 0.65, cy + rad * 0.8),
                    (cx - rad * 0.65, cy - rad * 0.8),
                ]
            } else {
                vec![
                    (cx, cy - rad),
                    (cx + rad * 0.85, cy + rad * 0.65),
                    (cx - rad * 0.85, cy + rad * 0.65),
                ]
            };
            out.polys.push((pts, col));
        }
        IconStyle::Flags => {
            out.lines
                .push((cx - rad * 0.5, cy - rad, cx - rad * 0.5, cy + rad, 0.8, [70, 70, 70]));
            out.polys.push((
                vec![
                    (cx - rad * 0.5, cy + rad),
                    (cx + rad * 0.85, cy + rad * 0.45),
                    (cx - rad * 0.5, cy - rad * 0.05),
                ],
                col,
            ));
        }
        IconStyle::Rating => {
            // 柱状信号格
            let bars = 4usize;
            let filled = ((ratio * bars as f32).round() as usize).clamp(1, bars);
            let bw_bar = (rad * 2.0) / (bars as f32 * 1.4);
            for i in 0..bars {
                let bx = cx - rad + i as f32 * bw_bar * 1.4;
                let bh = rad * 2.0 * (i as f32 + 1.0) / bars as f32;
                let c = if i < filled { col } else { [215, 220, 226] };
                out.rects.push((bx, cy - rad, bw_bar, bh, c));
            }
        }
        _ => {
            // TrafficLights / Signs / Symbols：12 边形矢量圆点
            let mut pts = Vec::with_capacity(12);
            for i in 0..12 {
                let a = i as f32 * std::f32::consts::TAU / 12.0;
                pts.push((cx + rad * a.cos(), cy + rad * a.sin()));
            }
            out.polys.push((pts, col));
        }
    }
}

/// 矢量绘制单元格迷你图（折线 / 柱形 / 盈亏，含零轴与高/低/首/末/负点高亮）
#[allow(clippy::too_many_arguments)]
fn draw_sparkline(
    wb: &Workbook,
    sheet_idx: usize,
    sp: &crate::sparkline::Sparkline,
    x: f32,
    y_bot: f32,
    w: f32,
    h: f32,
    bw: bool,
    out: &mut Out,
) {
    let src_sheet = match &sp.source_sheet {
        Some(name) => wb
            .sheets
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .unwrap_or(&wb.sheets[sheet_idx]),
        None => &wb.sheets[sheet_idx],
    };
    let (r1, c1, r2, c2) = sp.rect;
    let mut vals: Vec<Option<f64>> = Vec::new();
    if r2.saturating_sub(r1) >= c2.saturating_sub(c1) {
        for r in r1..=r2.min(r1 + 512) {
            vals.push(src_sheet.get(r, c1).and_then(|c| match c.value {
                crate::model::Value::Number(v) => Some(v),
                _ => None,
            }));
        }
    } else {
        for c in c1..=c2.min(c1 + 512) {
            vals.push(src_sheet.get(r1, c).and_then(|c| match c.value {
                crate::model::Value::Number(v) => Some(v),
                _ => None,
            }));
        }
    }
    let n = vals.len();
    if n == 0 || w < 8.0 || h < 6.0 {
        return;
    }
    let nums: Vec<f64> = vals.iter().flatten().copied().filter(|v| v.is_finite()).collect();
    if nums.is_empty() {
        return;
    }
    let mut vmin = nums.iter().copied().fold(f64::INFINITY, f64::min);
    let mut vmax = nums.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if matches!(sp.kind, SparkKind::Column | SparkKind::WinLoss) {
        vmin = vmin.min(0.0);
        vmax = vmax.max(0.0);
    }
    let span = (vmax - vmin).max(1e-9);
    let pad = 2.0f32;
    let ix = x + pad;
    let iy = y_bot + pad;
    let iw = (w - pad * 2.0).max(2.0);
    let ih = (h - pad * 2.0).max(2.0);
    let main_col = bw_color(sp.color, bw);
    let neg_col = if bw { [120, 120, 120] } else { [217, 63, 56] };
    let hi_col = if bw { [0, 0, 0] } else { [22, 163, 74] };
    let lo_col = if bw { [100, 100, 100] } else { [234, 88, 12] };

    let zero_y = iy + ih * ((0.0 - vmin) / span).clamp(0.0, 1.0) as f32;
    if sp.axis || sp.kind == SparkKind::WinLoss {
        out.lines
            .push((ix, zero_y, ix + iw, zero_y, 0.4, [150, 150, 150]));
    }

    let (hi_idx, lo_idx) = {
        let mut hi = (0usize, f64::NEG_INFINITY);
        let mut lo = (0usize, f64::INFINITY);
        for (i, v) in vals.iter().enumerate() {
            if let Some(val) = v.filter(|x| x.is_finite()) {
                if val > hi.1 {
                    hi = (i, val);
                }
                if val < lo.1 {
                    lo = (i, val);
                }
            }
        }
        (hi.0, lo.0)
    };

    match sp.kind {
        SparkKind::Line => {
            let step = if n > 1 { iw / (n - 1) as f32 } else { 0.0 };
            let mut seg: Vec<(f32, f32)> = Vec::new();
            for (i, v_opt) in vals.iter().enumerate() {
                let eff = match v_opt {
                    Some(v) if v.is_finite() => Some(*v),
                    _ => match sp.empty {
                        EmptyAs::Zero => Some(0.0),
                        EmptyAs::Connect => None,
                        EmptyAs::Gap => {
                            if seg.len() >= 2 {
                                out.stroke_polys.push(StrokePoly {
                                    pts: std::mem::take(&mut seg),
                                    width: 0.9,
                                    color: main_col,
                                    dash: Vec::new(),
                                });
                            } else {
                                seg.clear();
                            }
                            None
                        }
                    },
                };
                if let Some(v) = eff {
                    let px = ix + step * i as f32;
                    let py = iy + ih * ((v - vmin) / span) as f32;
                    seg.push((px, py));
                    let marker_col = if sp.high && i == hi_idx {
                        Some(hi_col)
                    } else if sp.low && i == lo_idx {
                        Some(lo_col)
                    } else if sp.negative && v < 0.0 {
                        Some(neg_col)
                    } else if (sp.first && i == 0) || (sp.last && i + 1 == n) || sp.markers {
                        Some(main_col)
                    } else {
                        None
                    };
                    if let Some(mc) = marker_col {
                        out.rects.push((px - 1.2, py - 1.2, 2.4, 2.4, mc));
                    }
                }
            }
            if seg.len() >= 2 {
                out.stroke_polys.push(StrokePoly {
                    pts: seg,
                    width: 0.9,
                    color: main_col,
                    dash: Vec::new(),
                });
            }
        }
        SparkKind::Column | SparkKind::WinLoss => {
            let slot = iw / n as f32;
            let bw_bar = (slot * 0.72).max(1.0);
            let mid_y = iy + ih * 0.5;
            for (i, v_opt) in vals.iter().enumerate() {
                let v = match v_opt {
                    Some(v) if v.is_finite() => *v,
                    _ if sp.empty == EmptyAs::Zero => 0.0,
                    _ => continue,
                };
                let bx = ix + i as f32 * slot + (slot - bw_bar) * 0.5;
                let col = if sp.high && i == hi_idx {
                    hi_col
                } else if sp.low && i == lo_idx {
                    lo_col
                } else if (sp.negative || sp.kind == SparkKind::WinLoss) && v < 0.0 {
                    neg_col
                } else {
                    main_col
                };
                if sp.kind == SparkKind::WinLoss {
                    let hh = ih * 0.42;
                    if v > 0.0 {
                        out.rects.push((bx, mid_y, bw_bar, hh, col));
                    } else if v < 0.0 {
                        out.rects.push((bx, mid_y - hh, bw_bar, hh, col));
                    }
                } else {
                    let py = iy + ih * ((v - vmin) / span) as f32;
                    let (y0, hh) = if py >= zero_y {
                        (zero_y, (py - zero_y).max(0.6))
                    } else {
                        (py, (zero_y - py).max(0.6))
                    };
                    out.rects.push((bx, y0, bw_bar, hh, col));
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_cell(
    wb: &Workbook,
    sheet: &Sheet,
    si: usize,
    setup: &PageSetup,
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
    let mut eff: Option<crate::model::Value> = None;
    if let Some(cell) = sheet.get(r, c)
        && cell.value.is_empty()
        && cell.formula.is_some()
    {
        eff = Some(fvalue_to_value(state.calc.cell_value(si, r, c)));
    }
    let cap = {
        let c2 = merge.map(|(_, _, _, c2)| c2).unwrap_or(c);
        let px_w: f32 = (c..=c2).map(|cc| col_w(sheet, cc)).sum();
        (((px_w - 5.0) / 8.0).floor().max(1.0)) as usize
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
        // 空单元格也可能有迷你图
        if let Ok(sp_idx) = sheet.sparklines.binary_search_by_key(&(c, r), |s| (s.c, s.r)) {
            draw_sparkline(wb, si, &sheet.sparklines[sp_idx], x, y_bottom, w, h, setup.black_and_white, out);
        }
        return;
    };
    let (mut cw, mut ch) = (w, h);
    if let Some((r1, c1, r2, c2)) = view.merge
        && (r1, c1) == (r, c)
    {
        cw = (c1..=c2).map(|cc| col_w(sheet, cc) * px).sum();
        ch = (r1..=r2).map(|rr| row_h(sheet, rr) * px).sum();
    }
    // 修复 P0 合并格反向绘制 bug：PDF y 向上，y_bottom 是首行底边，首行顶边是 y_bottom + h，
    // 跨多行合并格必须向下延伸，底边 = (y_bottom + h) - ch = y_bottom + h - ch！
    let box_bottom = y_bottom + h - ch;
    draw_view(wb, sheet, si, setup, r, c, x, box_bottom, cw, ch, scale, &view, out);
}

#[allow(clippy::too_many_arguments)]
fn draw_view(
    wb: &Workbook,
    sheet: &Sheet,
    si: usize,
    setup: &PageSetup,
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
    let bw = setup.black_and_white;
    let draft = setup.draft;

    // 1. 背景色（黑白或草稿模式跳过浅色填充，保证激光打印极高可读性）
    if !bw
        && !draft
        && let Some(fill) = view.fill
    {
        out.rects.push((x, top, cw, ch, fill));
    }

    // 2. 条件格式数据条
    if !draft
        && let Some((t, col)) = view.bar
    {
        let bar_w = (cw * t.clamp(0.0, 1.0)).max(0.0);
        if bar_w > 0.5 {
            let c_bar = if bw { [190, 190, 190] } else { col };
            out.rects.push((x + 0.5, top + 1.0, (bar_w - 1.0).max(0.5), (ch - 2.0).max(1.0), c_bar));
        }
    }

    // 3. 单元格迷你图
    if !draft
        && let Ok(sp_idx) = sheet.sparklines.binary_search_by_key(&(c, r), |s| (s.c, s.r))
    {
        draw_sparkline(wb, si, &sheet.sparklines[sp_idx], x, top, cw, ch, bw, out);
    }

    // 4. 全部 13 种边框线型 + 对角线边框 + 条件格式差异边框
    let st = wb.styles.get(sheet.style_of(r, c));
    let mut borders = st.border;
    if let Some(dxf_b) = &view.cf_border {
        for i in 0..4 {
            if let Some((ref name, col)) = dxf_b.sides[i] {
                borders[i] = Border {
                    style: Border::from_ooxml_name(name),
                    color: col,
                };
            }
        }
    }
    for (i, b) in borders.iter().enumerate() {
        if b.style == 0 {
            continue;
        }
        let (x1, y1, x2, y2) = match i {
            0 => (x, top, x, top + ch),
            1 => (x + cw, top, x + cw, top + ch),
            2 => (x, top + ch, x + cw, top + ch),
            _ => (x, top, x + cw, top),
        };
        emit_border_line(x1, y1, x2, y2, b.style, b.color, scale, bw, out);
    }
    if let Some(diag) = &st.diagonal {
        let dstyle = Border::from_ooxml_name(&diag.style);
        if diag.up {
            emit_border_line(x, top, x + cw, top + ch, dstyle, diag.color, scale, bw, out);
        }
        if diag.down {
            emit_border_line(x, top + ch, x + cw, top, dstyle, diag.color, scale, bw, out);
        }
    }

    // 5. 条件格式图标集（放在单元格左侧槽位）
    let mut icon_gutter = 0.0f32;
    if !draft
        && let Some(icon) = view.icon.clone()
    {
        let rad = (ch * 0.28).clamp(2.2, 4.5 * scale.clamp(0.5, 1.5));
        draw_cf_icon(icon, x + rad + 2.5 * scale, top + ch * 0.5, rad, bw, out);
        icon_gutter = rad * 2.0 + 4.5 * scale;
    }

    // 6. 批注角标（AsDisplayed 模式在单元格右上角画红色小三角）
    if setup.print_comments == 2
        && sheet
            .notes
            .contains_key(&crate::model::CellRef { row: r, col: c })
    {
        let sz = (4.5 * scale).clamp(2.5, 6.0);
        let col = if bw { [0, 0, 0] } else { [217, 45, 32] };
        out.polys.push((
            vec![
                (x + cw, top + ch),
                (x + cw - sz, top + ch),
                (x + cw, top + ch - sz),
            ],
            col,
        ));
    }

    // 错误值替换策略（setup.print_errors）
    let effective_text: String = if view.is_error {
        match setup.print_errors {
            1 => String::new(),
            2 => "--".to_string(),
            3 => "#N/A".to_string(),
            _ => view.text.clone(),
        }
    } else {
        view.text.clone()
    };

    if effective_text.is_empty() {
        return;
    }

    let mut size = (view.size * scale).max(3.0);
    let pad = 2.0 * scale;
    let indent = view.indent as f32 * 3.0 * display::char_width('0', size);
    let avail_w = (cw - pad * 2.0 - indent - icon_gutter).max(4.0);
    let text_color = bw_color(view.color, bw);

    // 缩小字体填充（shrink_to_fit）
    if view.shrink_to_fit && !view.wrap && !view.vertical_text {
        let full_w = out.tw(&effective_text, size);
        if full_w > avail_w && full_w > 0.0 {
            size = (size * (avail_w / full_w)).max(3.5);
        }
    }

    let (asc, desc) = match &out.vmetric {
        Some(f) => f(size),
        None => (size * 0.92, size * 0.26),
    };
    let line_h = asc + desc;

    // 竖排文字（中文报表表头高频场景）
    if view.vertical_text {
        let chars: Vec<char> = effective_text.chars().filter(|c| *c != '\n').collect();
        let fit = ((ch / line_h).floor() as usize).max(1);
        let show = chars.len().min(fit);
        let total_h = line_h * show as f32;
        let mut ty = cell_baseline(view.valign, top, ch, total_h, line_h, asc, desc);
        let clip = Some([x, top, cw, ch]);
        for &ch_c in chars.iter().take(show) {
            let s = ch_c.to_string();
            let tw = out.tw(&s, size);
            let tx = x + (cw - tw) * 0.5;
            out.texts.push(TextOp {
                x: tx,
                baseline: ty,
                size,
                text: s,
                color: text_color,
                bold: view.bold,
                italic: view.italic,
                rotation: 0.0,
                underline: view.underline,
                double_underline: view.double_underline,
                strikethrough: view.strikethrough,
                clip,
                alpha: 1.0,
            });
            ty -= line_h;
        }
        return;
    }

    // 单元格水平裁剪：仅当右邻格有内容或自动换行或显式右/居中对齐超宽时裁剪，保持 Excel 溢出习惯
    let right_col = view.merge.map(|(_, _, _, c2)| c2).unwrap_or(c) + 1;
    let right_occupied = sheet.get(r, right_col).is_some_and(|cell| !cell.value.is_empty());
    let need_clip = view.wrap || right_occupied || view.halign != 0 || view.rotation != 0;
    let clip_box = need_clip.then_some([x, top, cw, ch]);

    // 富文本逐段渲染（单行或无换行时保留每段独立粗/斜/色/字号）
    if let Some(runs) = &view.rich_runs
        && !view.is_error
        && !view.wrap
        && view.rotation == 0
        && !runs.iter().any(|rr| rr.text.contains('\n'))
    {
        let total_w: f32 = runs
            .iter()
            .map(|rr| {
                let rsz = (rr.size.unwrap_or(view.size) * scale).max(3.0);
                out.tw(&rr.text, rsz)
            })
            .sum();
        let ty = cell_baseline(view.valign, top, ch, line_h, line_h, asc, desc);
        let mut cur_x = match view.halign {
            1 => x + icon_gutter + (cw - icon_gutter - total_w) * 0.5,
            2 => x + cw - pad - total_w,
            _ => x + pad + indent + icon_gutter,
        };
        for rr in runs {
            if rr.text.is_empty() {
                continue;
            }
            let rsz = (rr.size.unwrap_or(view.size) * scale).max(3.0);
            let rw = out.tw(&rr.text, rsz);
            let rc = bw_color(rr.color.unwrap_or(view.color), bw);
            emit_text_with_decorations(
                cur_x,
                ty,
                rsz,
                rw,
                rr.text.clone(),
                rc,
                rr.bold || view.bold,
                rr.italic || view.italic,
                0.0,
                view.underline,
                view.double_underline,
                view.strikethrough,
                clip_box,
                out,
            );
            cur_x += rw;
        }
        return;
    }

    let mut lines_txt = if view.wrap {
        display::wrap_lines_excel(&effective_text, avail_w, size, out.metric.as_deref())
    } else {
        effective_text.split('\n').map(|s| s.to_string()).collect()
    };
    let fit = ((ch / line_h).floor() as usize).max(1);
    if lines_txt.len() > fit {
        lines_txt.truncate(fit);
    }
    let total_h = line_h * lines_txt.len() as f32;
    let mut ty = cell_baseline(view.valign, top, ch, total_h, line_h, asc, desc);
    for line in &lines_txt {
        let tw = out.tw(line, size);
        let (tx, base_y) = if view.rotation != 0 {
            // 旋转文字锚在单元格中心附近，避免飞出格框
            (x + cw * 0.5 - tw * 0.25 * (view.rotation as f32).to_radians().cos(), top + ch * 0.5 - size * 0.25)
        } else {
            let lx = match view.halign {
                1 => x + icon_gutter + (cw - icon_gutter - tw) * 0.5,
                2 => x + cw - pad - tw,
                _ => x + pad + indent + icon_gutter,
            };
            (lx, ty)
        };
        emit_text_with_decorations(
            tx,
            base_y,
            size,
            tw,
            line.to_string(),
            text_color,
            view.bold,
            view.italic,
            view.rotation as f32,
            view.underline,
            view.double_underline,
            view.strikethrough,
            clip_box,
            out,
        );
        ty -= line_h;
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_text_with_decorations(
    x: f32,
    baseline: f32,
    size: f32,
    tw: f32,
    text: String,
    color: [u8; 3],
    bold: bool,
    italic: bool,
    rotation: f32,
    underline: bool,
    double_underline: bool,
    strikethrough: bool,
    clip: Option<[f32; 4]>,
    out: &mut Out,
) {
    out.texts.push(TextOp {
        x,
        baseline,
        size,
        text,
        color,
        bold,
        italic,
        rotation,
        underline,
        double_underline,
        strikethrough,
        clip,
        alpha: 1.0,
    });
    if rotation == 0.0 && tw > 0.5 {
        let lw = (size * 0.06).clamp(0.45, 1.4);
        if double_underline {
            let y1 = baseline - size * 0.14;
            let y2 = baseline - size * 0.28;
            out.lines.push((x, y1, x + tw, y1, lw, color));
            out.lines.push((x, y2, x + tw, y2, lw, color));
        } else if underline {
            let y1 = baseline - size * 0.16;
            out.lines.push((x, y1, x + tw, y1, lw, color));
        }
        if strikethrough {
            let ym = baseline + size * 0.30;
            out.lines.push((x, ym, x + tw, ym, lw, color));
        }
    }
}

/// 跨页精准锚点换算：扣减当前页 `span.cols.0` / `span.rows.0` 起点偏移，
/// 使第 2 页、第 3 页的图片/图表/文本框精准落在对应单元格上（首页 `span=(0,0)` 与旧行为零差异）。
#[allow(clippy::too_many_arguments)]
fn grid_rect_in_page(
    sheet: &Sheet,
    span: &PageSpan,
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    off: [i64; 4],
    sx: f32,
    sy: f32,
    px: f32,
) -> (f32, f32, f32, f32) {
    let e = |v: i64| v as f32 / 9525.0;
    let hc = sheet.hidden_cols.intervals();
    let hr1 = sheet.hidden_rows.intervals();
    let hr2 = sheet.hidden_rows_user.intervals();
    let page_x0 = if span.cols.0 > 0 {
        sum_axis_px(&sheet.cols, hc, &[], 0, span.cols.0 - 1)
    } else {
        0.0
    };
    let page_y0 = if span.rows.0 > 0 {
        sum_axis_px(&sheet.rows, hr1, hr2, 0, span.rows.0 - 1)
    } else {
        0.0
    };
    let x1 = if c1 > 0 {
        sum_axis_px(&sheet.cols, hc, &[], 0, c1 - 1)
    } else {
        0.0
    } + e(off[0])
        - page_x0;
    let x2 = if c2 > 0 {
        sum_axis_px(&sheet.cols, hc, &[], 0, c2 - 1)
    } else {
        0.0
    } + e(off[2])
        - page_x0;
    let t1 = if r1 > 0 {
        sum_axis_px(&sheet.rows, hr1, hr2, 0, r1 - 1)
    } else {
        0.0
    } + e(off[1])
        - page_y0;
    let t2 = if r2 > 0 {
        sum_axis_px(&sheet.rows, hr1, hr2, 0, r2 - 1)
    } else {
        0.0
    } + e(off[3])
        - page_y0;
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
    out.texts.push(TextOp::simple(
        x,
        y,
        size,
        text.to_string(),
        color,
        false,
        false,
        0.0,
    ));
}

#[allow(clippy::too_many_arguments)]
fn draw_chart(
    sheet: &Sheet,
    span: &PageSpan,
    ch: &crate::charts::Chart,
    sx: f32,
    sy: f32,
    px: f32,
    bw: bool,
    out: &mut Out,
) {
    use crate::charts::ChartKind;
    let (a, b, c, d) = ch.anchor;
    let (x, y, w, h) = grid_rect_in_page(sheet, span, a, b, c, d, [0; 4], sx, sy, px);
    if w < 20.0 || h < 20.0 {
        return;
    }
    out.rects.push((x, y, w, h, [255, 255, 255]));
    let frame_c = if bw { [120, 120, 120] } else { [180, 180, 180] };
    out.lines.push((x, y, x + w, y, 0.6, frame_c));
    out.lines.push((x, y + h, x + w, y + h, 0.6, frame_c));
    out.lines.push((x, y, x, y + h, 0.6, frame_c));
    out.lines.push((x + w, y, x + w, y + h, 0.6, frame_c));

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
        chart_text(out, x + (w - tw) / 2.0, top, 10.0, &ch.title, bw_color(th.title_fg, bw));
        top -= 14.0;
    }
    if !ch.subtitle.is_empty() {
        let tw = out.tw(&ch.subtitle, 7.5);
        chart_text(out, x + (w - tw) / 2.0, top, 7.5, &ch.subtitle, bw_color(th.subtitle_fg, bw));
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
    let axis_col = bw_color(th.axis_line, bw);
    if !matches!(
        ch.kind,
        ChartKind::Pie | ChartKind::Doughnut | ChartKind::Radar | ChartKind::Funnel | ChartKind::Treemap
    ) {
        out.lines.push((px0, py0, px0 + pw, py0, 0.5, axis_col));
        out.lines.push((px0, py0, px0, py0 + ph, 0.5, axis_col));
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
    let gray_shade = |i: usize, rgb: [u8; 3]| -> [u8; 3] {
        if !bw {
            return rgb;
        }
        let g = (40 + (i * 38) % 165) as u8;
        [g, g, g]
    };

    if matches!(ch.kind, ChartKind::Pie | ChartKind::Doughnut) {
        let vals = vis_series.first().map(|(_, s)| s.vals.clone()).unwrap_or_default();
        let total: f64 = vals.iter().copied().filter(|v| v.is_finite() && *v > 0.0).sum::<f64>().max(1e-9);
        let cx = px0 + pw / 2.0;
        let cy = py0 + ph / 2.0;
        let rad = (pw.min(ph) / 2.0 - 4.0).max(4.0);
        let mut start_a = std::f32::consts::FRAC_PI_2;
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
            out.polys.push((pts, gray_shade(i, CHART_COLORS[i % CHART_COLORS.len()])));
            start_a -= sweep;
        }
    } else if matches!(ch.kind, ChartKind::Radar) {
        let groups = n.max(2);
        let cx = px0 + pw / 2.0;
        let cy = py0 + ph / 2.0;
        let rad = (pw.min(ph) / 2.0 - 6.0).max(6.0);
        for &(si, se) in &vis_series {
            let color = gray_shade(si, se.color.unwrap_or(CHART_COLORS[si % CHART_COLORS.len()]));
            let pts: Vec<(f32, f32)> = se
                .vals
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let ang = std::f32::consts::FRAC_PI_2
                        - i as f32 * std::f32::consts::TAU / groups as f32;
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
            let span_v = (dmax - dmin).max(1e-9);
            let slot = pw / bars.len().max(1) as f32;
            let b_w = (slot * 0.62).max(2.0);
            for (i, b) in bars.iter().enumerate() {
                let bx = px0 + i as f32 * slot + (slot - b_w) * 0.5;
                let ya = py0 + ph * ((b.y_start - dmin) / span_v) as f32;
                let yb = py0 + ph * ((b.y_end - dmin) / span_v) as f32;
                let col = gray_shade(
                    i,
                    match b.kind {
                        crate::chart_engine::WaterfallBarKind::Increase => th.pos_color,
                        crate::chart_engine::WaterfallBarKind::Decrease => th.neg_color,
                        crate::chart_engine::WaterfallBarKind::Total => th.total_color,
                    },
                );
                out.rects.push((bx, ya.min(yb), b_w, (ya - yb).abs().max(1.5), col));
                if i + 1 < bars.len() {
                    let nx = px0 + (i + 1) as f32 * slot + (slot - b_w) * 0.5;
                    out.lines.push((bx + b_w, yb, nx, yb, 0.5, axis_col));
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
                let wb_s = stages
                    .get(i + 1)
                    .map(|nx| pw * 0.72 * nx.width_frac as f32)
                    .unwrap_or(wt * 0.8);
                let y_top = py0 + ph - i as f32 * rh - 2.0;
                let y_bot = y_top - rh + 4.0;
                out.polys.push((
                    vec![
                        (cx - wt * 0.5, y_top),
                        (cx + wt * 0.5, y_top),
                        (cx + wb_s * 0.5, y_bot),
                        (cx - wb_s * 0.5, y_bot),
                    ],
                    gray_shade(i, th.palette[i % th.palette.len()]),
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
                out.rects.push((
                    t.x,
                    ry,
                    t.w.max(1.0),
                    t.h.max(1.0),
                    gray_shade(i, th.palette[i % th.palette.len()]),
                ));
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
            let col = gray_shade(0, th.palette[0]);
            let mut ppts = Vec::new();
            for (i, b) in bins.iter().enumerate() {
                let bx = px0 + i as f32 * slot + 1.0;
                let b_w = (slot - 2.0).max(1.5);
                let bh = ph * (b.count as f64 / mc) as f32;
                out.rects.push((bx, py0, b_w, bh, col));
                ppts.push((bx + b_w * 0.5, py0 + ph * b.cum_pct as f32));
            }
            if ch.analytics.show_pareto_line && ppts.len() >= 2 {
                out.stroke_polys.push(StrokePoly {
                    pts: ppts,
                    width: 1.3,
                    color: gray_shade(1, th.palette[1]),
                    dash: Vec::new(),
                });
            }
        }
    } else if matches!(ch.kind, ChartKind::BoxWhisker) {
        let boxes = crate::chart_engine::compute_box_whisker(ch);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for b in &boxes {
            lo = lo.min(b.whisker_lo);
            hi = hi.max(b.whisker_hi);
        }
        let span_v = (hi - lo).max(1e-9);
        let slot = pw / boxes.len().max(1) as f32;
        let b_w = (slot * 0.42).max(4.0);
        for (i, b) in boxes.iter().enumerate() {
            let cx = px0 + (i as f32 + 0.5) * slot;
            let y_of = |v: f64| py0 + ph * ((v - lo) / span_v) as f32;
            let col = gray_shade(i, th.palette[i % th.palette.len()]);
            out.lines.push((cx, y_of(b.whisker_lo), cx, y_of(b.whisker_hi), 1.0, col));
            let y1 = y_of(b.q1);
            let y3 = y_of(b.q3);
            out.rects.push((cx - b_w * 0.5, y1.min(y3), b_w, (y3 - y1).abs().max(2.0), col));
            let ym = y_of(b.median);
            out.lines.push((cx - b_w * 0.5, ym, cx + b_w * 0.5, ym, 1.6, [255, 255, 255]));
        }
    } else {
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
        let bw_bar = (gw * 0.7 / slot_count as f32).max(1.5);
        for &(si, se) in &vis_series {
            let color = gray_shade(si, se.color.unwrap_or(CHART_COLORS[si % CHART_COLORS.len()]));
            let scale = if se.secondary { max_sub } else { max_main };
            match series_kind(se, ch.kind) {
                ChartKind::Line | ChartKind::LineStacked | ChartKind::Scatter | ChartKind::Bubble => {
                    let sk = series_kind(se, ch.kind);
                    let is_scatter = matches!(sk, ChartKind::Scatter | ChartKind::Bubble);
                    let use_x = is_scatter && se.xv.len() == se.vals.len() && !se.xv.is_empty();
                    let (xmin, xspan) = if use_x {
                        let lo = se.xv.iter().cloned().fold(f64::MAX, f64::min);
                        let hi = se.xv.iter().cloned().fold(f64::MIN, f64::max);
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
                        out.stroke_polys.push(StrokePoly {
                            pts: pts.clone(),
                            width: 1.5,
                            color,
                            dash: Vec::new(),
                        });
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
                        let bx = px0 + gw * (i as f32 + 0.15) + bw_bar * slot as f32;
                        out.rects.push((bx, py0, bw_bar, bh, color));
                    }
                }
            }
            // 趋势线 PDF/打印矢量输出
            for tl in &se.trendlines {
                let raw: Vec<(f64, f64)> = se.vals.iter().enumerate().map(|(i, &v)| (i as f64, v)).collect();
                if let Some(comp) = crate::chart_engine::compute_trendline(&raw, tl) {
                    let tcol = gray_shade(si, tl.color.unwrap_or(color));
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
                    if tpts.len() >= 2 {
                        out.stroke_polys.push(StrokePoly {
                            pts: tpts,
                            width: 1.0,
                            color: tcol,
                            dash: vec![3.5, 2.0],
                        });
                    }
                }
            }
        }
    }
    if has_sub {
        out.lines.push((px0 + pw, py0, px0 + pw, py0 + ph, 0.5, axis_col));
    }
    if legend_w > 0.0 {
        let mut ly = y + h - 18.0;
        for &(i, se) in vis_series.iter().take(6) {
            let color = gray_shade(i, se.color.unwrap_or(CHART_COLORS[i % CHART_COLORS.len()]));
            out.rects.push((x + w - legend_w + 4.0, ly, 8.0, 8.0, color));
            chart_text(out, x + w - legend_w + 16.0, ly + 7.0, 7.5, &se.name, [60, 60, 60]);
            ly -= 12.0;
        }
    }
    if !ch.axis_y.is_empty() {
        chart_text(out, x + 2.0, y + h - 16.0, 7.5, &ch.axis_y, [80, 80, 80]);
    }
    if !ch.axis_x.is_empty() {
        chart_text(out, x + w / 2.0 - 10.0, y + 10.0, 7.5, &ch.axis_x, [80, 80, 80]);
    }
}

/// 图片/图表/文本框：按当前页偏移精准换算坐标，草稿模式跳过图片与图表以极速出稿
#[allow(clippy::too_many_arguments)]
fn draw_objects(
    sheet: &Sheet,
    setup: &PageSetup,
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
    if !setup.draft {
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
            let (x, y, w, h) = grid_rect_in_page(sheet, span, r1, c1, r2, c2, pic.off, sx, sy, px);
            let stem = Path::new(&pic.part)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "pic".into());
            let name: String = format!(
                "Im{}",
                stem.chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
            );
            out.images.push((x, y, w, h, name, bytes, pic.ext.clone()));
        }
        for ch in &sheet.charts {
            if contains(ch.anchor.0, ch.anchor.1) {
                draw_chart(sheet, span, ch, sx, sy, px, setup.black_and_white, out);
            }
        }
    }
    for tb in &sheet.textboxes {
        let (c1, dx1, r1, dy1) = tb.from;
        let (c2, dx2, r2, dy2) = tb.to;
        if !contains(r1, c1) {
            continue;
        }
        let (x, yy, w, h) = grid_rect_in_page(
            sheet,
            span,
            r1,
            c1,
            r2,
            c2,
            [dx1, dy1, dx2, dy2],
            sx,
            sy,
            px,
        );
        let w = w.max(8.0);
        let h = h.max(8.0);
        if !setup.black_and_white
            && !setup.draft
            && let Some(fill) = tb.fill
        {
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
                color: bw_color(tb.color, setup.black_and_white),
                bold: tb.bold,
                italic: tb.italic,
                rotation: 0.0,
                underline: false,
                double_underline: false,
                strikethrough: false,
                clip: Some([x, yy, w, h]),
                alpha: 1.0,
            });
            ty -= tb_asc + tb_desc;
        }
    }
}

/// 生成单页水印矢量层（居中大字或满页平铺）
fn emit_watermarks(setup: &PageSetup, pw: f32, ph: f32, out: &mut Out) {
    if !setup.watermark.is_active() {
        return;
    }
    let wm = &setup.watermark;
    let col = bw_color(wm.color, setup.black_and_white);
    let size = wm.font_size.clamp(14.0, 120.0);
    let tw = out.tw(&wm.text, size);
    let rad = wm.angle_deg.to_radians();
    let (cos, sin) = (rad.cos(), rad.sin());
    if wm.tiled {
        let step_x = (tw + 90.0).max(180.0);
        let step_y = (size * 3.2).max(140.0);
        let mut y = step_y * 0.5;
        let mut row_i = 0usize;
        while y < ph {
            let mut x = if row_i % 2 == 1 { step_x * 0.5 } else { 40.0 };
            while x < pw {
                out.watermarks.push(WatermarkOp {
                    x: x - tw * 0.5 * cos,
                    y: y - tw * 0.5 * sin,
                    size,
                    text: wm.text.clone(),
                    color: col,
                    angle_deg: wm.angle_deg,
                    opacity: wm.opacity,
                });
                x += step_x;
            }
            y += step_y;
            row_i += 1;
        }
    } else {
        out.watermarks.push(WatermarkOp {
            x: pw * 0.5 - tw * 0.5 * cos,
            y: ph * 0.5 - tw * 0.5 * sin,
            size,
            text: wm.text.clone(),
            color: col,
            angle_deg: wm.angle_deg,
            opacity: wm.opacity,
        });
    }
}

/// 当 `print_comments == 1 (AtEnd)` 时，生成工作表末尾的批注汇总页列表
pub fn comment_summary_pages(wb: &Workbook, sheet_idx: usize, layout: &Layout) -> Vec<Out> {
    let sheet = &wb.sheets[sheet_idx];
    if layout.setup.print_comments != 1 || sheet.notes.is_empty() {
        return Vec::new();
    }
    let mut notes: Vec<_> = sheet.notes.iter().map(|(_, n)| n).collect();
    notes.sort_by_key(|n| (n.r, n.c));
    let (pw, ph) = layout.page_size;
    let (ml, mr, mt, mb) = margins_pt(&layout.setup);
    let avail_w = (pw - ml - mr).max(80.0);
    let mut pages = Vec::new();
    let mut cur = Out::default();
    let mut y = ph - mt - 18.0;
    let emit_title = |out: &mut Out, y_top: f32| {
        out.texts.push(TextOp::simple(
            ml,
            y_top,
            12.0,
            format!("{} — 批注汇总", sheet.name),
            [30, 41, 59],
            true,
            false,
            0.0,
        ));
        out.lines.push((ml, y_top - 5.0, pw - mr, y_top - 5.0, 0.8, [180, 190, 204]));
    };
    emit_title(&mut cur, y);
    y -= 22.0;

    for note in notes {
        let head = format!(
            "单元格 {}{}",
            crate::model::cell_name(note.r, note.c),
            if note.author.is_empty() {
                String::new()
            } else {
                format!("（{}）", note.author)
            }
        );
        let body_lines = display::wrap_lines(&note.text, avail_w - 16.0, 9.5);
        let block_h = 15.0 + body_lines.len() as f32 * 13.0 + 10.0;
        if y - block_h < mb && !cur.texts.is_empty() {
            emit_watermarks(&layout.setup, pw, ph, &mut cur);
            pages.push(cur);
            cur = Out::default();
            y = ph - mt - 18.0;
            emit_title(&mut cur, y);
            y -= 22.0;
        }
        cur.texts.push(TextOp::simple(
            ml,
            y,
            10.0,
            head,
            [24, 34, 52],
            true,
            false,
            0.0,
        ));
        y -= 14.0;
        for line in body_lines {
            cur.texts.push(TextOp::simple(
                ml + 12.0,
                y,
                9.5,
                line,
                [60, 70, 85],
                false,
                false,
                0.0,
            ));
            y -= 13.0;
        }
        y -= 8.0;
    }
    emit_watermarks(&layout.setup, pw, ph, &mut cur);
    pages.push(cur);
    pages
}

pub fn page_ops(wb: &Workbook, sheet_idx: usize, layout: &Layout, page_index: usize) -> Out {
    page_ops_with(wb, sheet_idx, layout, page_index, None)
}

pub fn page_ops_with(
    wb: &Workbook,
    sheet_idx: usize,
    layout: &Layout,
    page_index: usize,
    source: Option<&Path>,
) -> Out {
    page_ops_metric(wb, sheet_idx, layout, page_index, source, None)
}

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

#[allow(clippy::too_many_arguments)]
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

    let display_page_no = setup
        .first_page_number
        .map(|start| start as usize + page_index)
        .unwrap_or(page_index + 1);
    let total_pages = layout.pages.len();
    let (raw_header, raw_footer) = setup.header_footer_for_page(page_index + 1);

    // 页眉（遵守 margins[4] 页眉边距并支持全部 Excel 宏代码）
    if !raw_header.is_empty() {
        let (l, c, r) =
            header_parts_with_ctx(raw_header, display_page_no, total_pages, Some(&state.hdr_ctx));
        let hdr_base = (ph - setup.margins[4] * 72.0 - 9.0)
            .clamp(ph - mt + 4.0, (ph - 10.0).max(ph - mt + 4.0));
        for (seg, align) in [(l, 0u8), (c, 1), (r, 2)] {
            if seg.is_empty() {
                continue;
            }
            let tw = out.tw(&seg, 9.0);
            let tx = match align {
                1 => (pw - tw) / 2.0,
                2 => pw - mr - tw,
                _ => ml,
            }
            .clamp(2.0, (pw - 2.0).max(2.0));
            out.texts.push(TextOp::simple(
                tx,
                hdr_base,
                9.0,
                seg,
                bw_color([90, 90, 90], setup.black_and_white),
                false,
                false,
                0.0,
            ));
        }
    }

    // 计算标题行/标题列/正文尺寸，用于精确水平/垂直居中与行列号绘制
    let title_rows = setup
        .title_rows
        .map(|(a, b)| (a, b.min(span.rows.1).max(a)));
    let title_cols = setup
        .title_cols
        .map(|(a, b)| (a, b.min(span.cols.1).max(a)));

    let (hdr_w, hdr_h) = headings_reserve(setup, layout.scale);
    let title_rows_h: f32 = match title_rows {
        Some((ta, tb)) => (ta..=tb).map(|r| row_h(sheet, r) * px).sum(),
        None => 0.0,
    };
    let title_cols_w: f32 = match title_cols {
        Some((ca, cb)) => (ca..=cb).map(|c| col_w(sheet, c) * px).sum(),
        None => 0.0,
    };
    let body_w: f32 = (span.cols.0..=span.cols.1)
        .map(|c| col_w(sheet, c) * px)
        .sum();
    let body_h: f32 = (span.rows.0..=span.rows.1)
        .map(|r| row_h(sheet, r) * px)
        .sum();

    let total_content_w = hdr_w + title_cols_w + body_w;
    let total_content_h = hdr_h + title_rows_h + body_h;
    let mut origin_x = ml;
    let mut origin_top = ph - mt;
    if setup.center_h && total_content_w < layout.printable.0 {
        origin_x += (layout.printable.0 - total_content_w) * 0.5;
    }
    if setup.center_v && total_content_h < layout.printable.1 {
        origin_top -= (layout.printable.1 - total_content_h) * 0.5;
    }

    // 1. 行号列标表头（setup.headings）
    let grid_x0 = origin_x + hdr_w;
    let grid_top = origin_top - hdr_h;
    if setup.headings {
        let hbg = if setup.black_and_white {
            [240, 240, 240]
        } else {
            [241, 245, 249]
        };
        let hbd = [180, 188, 198];
        let hfg = [51, 65, 85];
        let fsz = (8.0 * layout.scale.clamp(0.6, 1.3)).max(5.5);
        // 左上角方格
        out.rects.push((origin_x, grid_top, hdr_w, hdr_h, hbg));
        out.lines.push((origin_x, grid_top, origin_x + hdr_w, grid_top, 0.5, hbd));
        out.lines.push((origin_x, origin_top, origin_x + hdr_w, origin_top, 0.5, hbd));
        out.lines.push((origin_x, grid_top, origin_x, origin_top, 0.5, hbd));
        out.lines.push((grid_x0, grid_top, grid_x0, origin_top, 0.5, hbd));

        // 列标（标题列 + 正文列）
        let draw_col_hdr = |cx: f32, cw: f32, col_idx: u32, out: &mut Out| {
            out.rects.push((cx, grid_top, cw, hdr_h, hbg));
            out.lines.push((cx, grid_top, cx + cw, grid_top, 0.5, hbd));
            out.lines.push((cx, origin_top, cx + cw, origin_top, 0.5, hbd));
            out.lines.push((cx + cw, grid_top, cx + cw, origin_top, 0.5, hbd));
            let lbl = col_label(col_idx);
            let tw = out.tw(&lbl, fsz);
            out.texts.push(TextOp::simple(
                cx + (cw - tw) * 0.5,
                grid_top + (hdr_h - fsz) * 0.5 + 1.0,
                fsz,
                lbl,
                hfg,
                true,
                false,
                0.0,
            ));
        };
        let mut cx = grid_x0;
        if let Some((ca, cb)) = title_cols {
            for c in ca..=cb {
                let w = col_w(sheet, c) * px;
                if w > 0.0 {
                    draw_col_hdr(cx, w, c, &mut out);
                    cx += w;
                }
            }
        }
        for c in span.cols.0..=span.cols.1 {
            let w = col_w(sheet, c) * px;
            if w > 0.0 {
                draw_col_hdr(cx, w, c, &mut out);
                cx += w;
            }
        }

        // 行号（标题行 + 正文行）
        let draw_row_hdr = |ry_top: f32, rh: f32, row_idx: u32, out: &mut Out| {
            let ry_bot = ry_top - rh;
            out.rects.push((origin_x, ry_bot, hdr_w, rh, hbg));
            out.lines.push((origin_x, ry_bot, grid_x0, ry_bot, 0.5, hbd));
            out.lines.push((origin_x, ry_bot, origin_x, ry_top, 0.5, hbd));
            out.lines.push((grid_x0, ry_bot, grid_x0, ry_top, 0.5, hbd));
            let lbl = (row_idx + 1).to_string();
            let tw = out.tw(&lbl, fsz);
            out.texts.push(TextOp::simple(
                origin_x + (hdr_w - tw) * 0.5,
                ry_bot + (rh - fsz) * 0.5 + 1.0,
                fsz,
                lbl,
                hfg,
                true,
                false,
                0.0,
            ));
        };
        let mut ry = grid_top;
        if let Some((ta, tb)) = title_rows {
            for r in ta..=tb {
                let h = row_h(sheet, r) * px;
                if h > 0.0 {
                    draw_row_hdr(ry, h, r, &mut out);
                    ry -= h;
                }
            }
        }
        for r in span.rows.0..=span.rows.1 {
            let h = row_h(sheet, r) * px;
            if h > 0.0 {
                draw_row_hdr(ry, h, r, &mut out);
                ry -= h;
            }
        }
    }

    // 2. 标题行 × 标题列交叉角 + 顶端标题行
    let body_x0 = grid_x0 + title_cols_w;
    let mut y = grid_top;
    if let Some((ta, tb)) = title_rows {
        for r in ta..=tb {
            let h = row_h(sheet, r) * px;
            if h <= 0.0 {
                continue;
            }
            if let Some((ca, cb)) = title_cols {
                let mut x = grid_x0;
                for c in ca..=cb {
                    let w = col_w(sheet, c) * px;
                    if w <= 0.0 {
                        continue;
                    }
                    draw_cell(wb, sheet, sheet_idx, setup, state, r, c, x, y - h, w, h, px, layout.scale, &mut out);
                    x += w;
                }
            }
            let mut x = body_x0;
            for c in span.cols.0..=span.cols.1 {
                let w = col_w(sheet, c) * px;
                if w <= 0.0 {
                    continue;
                }
                draw_cell(wb, sheet, sheet_idx, setup, state, r, c, x, y - h, w, h, px, layout.scale, &mut out);
                x += w;
            }
            y -= h;
        }
    }

    // 3. 左侧标题列（正文行范围）
    if let Some((ca, cb)) = title_cols {
        let mut x = grid_x0;
        for c in ca..=cb {
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
                draw_cell(wb, sheet, sheet_idx, setup, state, r, c, x, yy - h, w, h, px, layout.scale, &mut out);
                yy -= h;
            }
            x += w;
        }
    }

    // 4. 正文区
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
            draw_cell(wb, sheet, sheet_idx, setup, state, r, c, x, cursor_y - h, w, h, px, layout.scale, &mut out);
            x += w;
        }
        cursor_y -= h;
    }

    // 5. 网格线（覆盖标题区 + 正文区）
    if setup.gridlines {
        let gl_c = if setup.black_and_white {
            [180, 180, 180]
        } else {
            [205, 205, 205]
        };
        let full_w = title_cols_w + body_w;
        let full_h = title_rows_h + body_h;
        let mut x = grid_x0;
        if let Some((ca, cb)) = title_cols {
            for c in ca..=cb {
                out.lines.push((x, grid_top - full_h, x, grid_top, 0.4, gl_c));
                x += col_w(sheet, c) * px;
            }
        }
        for c in span.cols.0..=span.cols.1 {
            out.lines.push((x, grid_top - full_h, x, grid_top, 0.4, gl_c));
            x += col_w(sheet, c) * px;
        }
        out.lines.push((x, grid_top - full_h, x, grid_top, 0.4, gl_c));

        let mut yy = grid_top;
        if let Some((ta, tb)) = title_rows {
            for r in ta..=tb {
                out.lines.push((grid_x0, yy, grid_x0 + full_w, yy, 0.4, gl_c));
                yy -= row_h(sheet, r) * px;
            }
        }
        for r in span.rows.0..=span.rows.1 {
            out.lines.push((grid_x0, yy, grid_x0 + full_w, yy, 0.4, gl_c));
            yy -= row_h(sheet, r) * px;
        }
        out.lines.push((grid_x0, yy, grid_x0 + full_w, yy, 0.4, gl_c));
    }

    // 6. 页脚（遵守 margins[5] 页脚边距）
    if !raw_footer.is_empty() {
        let (fl, fc, fr) =
            header_parts_with_ctx(raw_footer, display_page_no, total_pages, Some(&state.hdr_ctx));
        let ftr_base = (setup.margins[5] * 72.0).clamp(8.0, (mb - 8.0).max(8.0));
        for (seg, align) in [(fl, 0u8), (fc, 1), (fr, 2)] {
            if seg.is_empty() {
                continue;
            }
            let tw = out.tw(&seg, 8.5);
            let tx = match align {
                1 => (pw - tw) / 2.0,
                2 => pw - mr - tw,
                _ => ml,
            }
            .clamp(2.0, (pw - 2.0).max(2.0));
            out.texts.push(TextOp::simple(
                tx,
                ftr_base,
                8.5,
                seg,
                bw_color([130, 130, 130], setup.black_and_white),
                false,
                false,
                0.0,
            ));
        }
    }

    // 7. 浮动对象（跨页精准锚点）
    draw_objects(sheet, setup, span, body_x0, y, px, source, &mut out);

    // 8. 矢量水印层
    emit_watermarks(setup, pw, ph, &mut out);
    out
}

/// 将一份 `Out` 显示列表写入 PDF 的当前页（单表/多表/批注汇总页统一出口）
fn emit_out_to_pdf_page(doc: &mut Doc, out: Out) {
    for (x, y, w, h, color) in out.rects {
        doc.page_mut().rect_fill(x, y, w, h, color);
    }
    for (x, y, w, h, color, alpha) in out.alpha_rects {
        doc.page_mut().rect_fill_alpha(x, y, w, h, color, alpha);
    }
    for (x1, y1, x2, y2, lw, color) in out.lines {
        doc.page_mut().line(x1, y1, x2, y2, lw, color);
    }
    for sl in out.styled_lines {
        doc.page_mut()
            .line_styled(sl.x1, sl.y1, sl.x2, sl.y2, sl.width, sl.color, &sl.dash);
    }
    for (pts, color) in out.polys {
        doc.page_mut().poly(&pts, color, true);
    }
    for sp in out.stroke_polys {
        doc.page_mut()
            .poly_styled(&sp.pts, sp.width, sp.color, &sp.dash, false, 1.0);
    }
    for (x, y, w, h, name, bytes, ext) in out.images {
        if let Some(img) = crate::image_embed::make(&name, &ext, &bytes) {
            let name = doc.add_image(img);
            doc.page_mut().image(&name, x, y, w, h);
        }
    }
    for t in out.texts {
        if let Some([cx, cy, cw, ch]) = t.clip {
            doc.page_mut().push_clip(cx, cy, cw, ch);
        }
        doc.draw_text_auto(
            t.x,
            t.baseline,
            t.size,
            &t.text,
            t.color,
            t.bold,
            t.italic,
            t.rotation,
            t.alpha,
        );
        if t.clip.is_some() {
            doc.page_mut().pop_clip();
        }
    }
    for wm in out.watermarks {
        doc.draw_text_auto(
            wm.x,
            wm.y,
            wm.size,
            &wm.text,
            wm.color,
            true,
            false,
            wm.angle_deg,
            wm.opacity,
        );
    }
}

/// PDF 字体套装（主常规 + 可选真粗体 + 可选系统 CJK 回退常规/粗体）
pub struct PdfFontPack {
    pub regular: Font,
    pub bold: Option<Font>,
    pub fallback: Option<Font>,
    pub fallback_bold: Option<Font>,
}

/// 导出单表 PDF；返回导出页数
pub fn export_pdf(wb: &Workbook, sheet_idx: usize, path: &Path, font_path: &Path) -> Result<usize> {
    export_pdf_from(wb, sheet_idx, path, font_path, None)
}

pub fn export_pdf_from(
    wb: &Workbook,
    sheet_idx: usize,
    path: &Path,
    font_path: &Path,
    source: Option<&Path>,
) -> Result<usize> {
    export_pdf_pages_from(wb, sheet_idx, path, font_path, source, None)
}

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

pub fn export_pdf_pages_with_fonts(
    wb: &Workbook,
    sheet_idx: usize,
    path: &Path,
    font: Font,
    bold: Option<Font>,
    source: Option<&Path>,
    pages: Option<(usize, usize)>,
) -> Result<usize> {
    let pack = PdfFontPack {
        regular: font,
        bold,
        fallback: None,
        fallback_bold: None,
    };
    let list = pages.map(|(a, b)| (a..=b).collect::<Vec<_>>());
    export_pdf_sheet_with_pack(wb, sheet_idx, path, pack, source, list.as_deref())
}

/// 导出单表 PDF（支持自定义非连续页码列表、系统 CJK 回退字体链与表尾批注汇总页）
pub fn export_pdf_sheet_with_pack(
    wb: &Workbook,
    sheet_idx: usize,
    path: &Path,
    pack: PdfFontPack,
    source: Option<&Path>,
    page_indices: Option<&[usize]>,
) -> Result<usize> {
    let Some(sheet) = wb.sheets.get(sheet_idx) else {
        anyhow::bail!("没有第 {sheet_idx} 张表");
    };
    let layout = paginate(sheet, &sheet.page);
    let total = layout.pages.len();
    let indices: Vec<usize> = match page_indices {
        Some(list) => list.iter().copied().filter(|&i| i < total).collect(),
        None => (0..total).collect(),
    };
    let (asc_em, desc_em) = pack.regular.em_metrics();
    let vmetric: Option<std::sync::Arc<dyn Fn(f32) -> (f32, f32) + Send + Sync>> = Some(
        std::sync::Arc::new(move |size: f32| (asc_em * size, desc_em * size)),
    );
    let mut doc = Doc::new(pack.regular);
    if let Some(b) = pack.bold {
        doc.set_bold_font(b);
    }
    if let Some(fb) = pack.fallback {
        doc.set_fallback_font(fb);
    }
    if let Some(fbb) = pack.fallback_bold {
        doc.set_fallback_bold_font(fbb);
    }
    doc.set_metadata(wb.title(), "", &sheet.name);

    let mut state = PageState::new(wb, sheet_idx);
    let mut exported = 0usize;
    for pi in indices {
        exported += 1;
        let (pw, ph) = layout.page_size;
        doc.add_page(pw, ph);
        let out = render_page(wb, sheet_idx, &layout, pi, source, None, vmetric.clone(), &mut state);
        emit_out_to_pdf_page(&mut doc, out);
    }
    if page_indices.is_none() {
        for cpage in comment_summary_pages(wb, sheet_idx, &layout) {
            exported += 1;
            let (pw, ph) = layout.page_size;
            doc.add_page(pw, ph);
            emit_out_to_pdf_page(&mut doc, cpage);
        }
    }
    doc.write(path)?;
    Ok(exported)
}

/// 导出整本工作簿（全部可见工作表）为一份带 PDF 侧边栏书签目录（`/Outlines`）的出版级 PDF
pub fn export_workbook_pdf_with_pack(
    wb: &Workbook,
    path: &Path,
    pack: PdfFontPack,
    source: Option<&Path>,
) -> Result<usize> {
    let (asc_em, desc_em) = pack.regular.em_metrics();
    let vmetric: Option<std::sync::Arc<dyn Fn(f32) -> (f32, f32) + Send + Sync>> = Some(
        std::sync::Arc::new(move |size: f32| (asc_em * size, desc_em * size)),
    );
    let mut doc = Doc::new(pack.regular);
    if let Some(b) = pack.bold {
        doc.set_bold_font(b);
    }
    if let Some(fb) = pack.fallback {
        doc.set_fallback_font(fb);
    }
    if let Some(fbb) = pack.fallback_bold {
        doc.set_fallback_bold_font(fbb);
    }
    doc.set_metadata(wb.title(), "", "Workbook PDF Export");

    let mut exported = 0usize;
    for (si, sheet) in wb.sheets.iter().enumerate() {
        if sheet.hidden {
            continue;
        }
        let layout = paginate(sheet, &sheet.page);
        let first_pdf_page = exported;
        doc.add_bookmark(&sheet.name, first_pdf_page);
        let mut state = PageState::new(wb, si);
        for pi in 0..layout.pages.len() {
            exported += 1;
            let (pw, ph) = layout.page_size;
            doc.add_page(pw, ph);
            let out = render_page(wb, si, &layout, pi, source, None, vmetric.clone(), &mut state);
            emit_out_to_pdf_page(&mut doc, out);
        }
        for cpage in comment_summary_pages(wb, si, &layout) {
            exported += 1;
            let (pw, ph) = layout.page_size;
            doc.add_page(pw, ph);
            emit_out_to_pdf_page(&mut doc, cpage);
        }
    }
    doc.write(path)?;
    Ok(exported)
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
            .map(fvalue_to_value)
            .unwrap_or(Value::Empty),
        FValue::Lambda(_) => Value::Error(crate::formula::ErrKind::Value),
    }
}

fn series_kind(
    se: &crate::charts::Series,
    chart_kind: crate::charts::ChartKind,
) -> crate::charts::ChartKind {
    use crate::charts::ChartKind;
    if let Some(k) = se.kind {
        return k;
    }
    match chart_kind {
        ChartKind::Combo => ChartKind::Column,
        k => k,
    }
}

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
        sh.hidden_rows = vec![(20, 999_989)].into();

        let mut setup = PageSetup::default();
        let t0 = std::time::Instant::now();
        let layout = paginate(&sh, &setup);
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert!(elapsed_ms < 20.0, "expected O(spans) < 20 ms, got {elapsed_ms:.2} ms");
        assert_eq!(layout.pages.len(), 1, "30 visible rows with 999,970 hidden rows must fit on 1 page");
        assert_eq!(layout.pages[0].rows, (0, 999_999));

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
