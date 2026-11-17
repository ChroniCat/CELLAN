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

/// 分页：按列宽/行高累加，超出可打印区就换页。
///
/// 打印标题（顶端标题行 / 左侧标题列）每页重复，正文从标题之后开始：
/// 第一页不再把标题区当正文再画一遍，续页的容量也按标题占掉的高度扣减。
pub fn paginate(sheet: &Sheet, setup: &PageSetup) -> Layout {
    let (pw, ph) = paper_size(setup.paper);
    let (pw, ph) = if setup.landscape { (ph, pw) } else { (pw, ph) };
    let (left, right, top, bottom) = margins_pt(setup);
    let printable = ((pw - left - right).max(72.0), (ph - top - bottom).max(72.0));

    let (r1, c1, r2, c2) = match setup.print_area {
        Some(a) => a,
        None => {
            let mut max_r = 0;
            let mut max_c = 0;
            for (cref, _) in sheet.iter() {
                max_r = max_r.max(cref.row);
                max_c = max_c.max(cref.col);
            }
            (0, 0, max_r, max_c)
        }
    };

    let mut scale = if setup.fit_to_page {
        let target_w = printable.0 * setup.fit_width.max(1) as f32;
        let total_w: f32 = (c1..=c2).map(|c| sheet.cols.size(c)).sum();
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
        let total_h: f32 = (r1..=r2).map(|r| sheet.rows.size(r)).sum();
        if total_h > 0.0 {
            scale = scale.min((target_h / (total_h * 0.75)).min(1.0));
        }
    }
    let px = scale * 0.75;

    let title_rows = setup.title_rows.map(|(a, b)| (a, b.min(r2))).filter(|(a, b)| a <= b);
    let title_cols = setup.title_cols.map(|(a, b)| (a, b.min(c2))).filter(|(a, b)| a <= b);
    let head_rows_h: f32 = match title_rows {
        Some((a, b)) => (a..=b).map(|r| sheet.rows.size(r) * px).sum(),
        None => 0.0,
    };
    let head_cols_w: f32 = match title_cols {
        Some((a, b)) => (a..=b).map(|c| sheet.cols.size(c) * px).sum(),
        None => 0.0,
    };

    let body_top = (printable.1 - head_rows_h).max(24.0);
    let body_left = (printable.0 - head_cols_w).max(24.0);
    let br1 = title_rows.map(|(_, b)| b + 1).unwrap_or(r1).max(r1).min(r2);
    let bc1 = title_cols.map(|(_, b)| b + 1).unwrap_or(c1).max(c1).min(c2);

    let mut pages = Vec::new();
    let mut row = br1;
    while row <= r2 {
        let mut used = 0.0f32;
        let mut last_row = row;
        while last_row <= r2 {
            // 手动分页：用户在该行之前插入了分页符
            if last_row > row && setup.row_breaks.contains(&last_row) {
                break;
            }
            let h = sheet.rows.size(last_row) * px;
            if used + h > body_top && last_row > row {
                break;
            }
            used += h;
            last_row += 1;
        }
        let rows_end = last_row.saturating_sub(1).min(r2).max(row);
        let mut col = bc1;
        while col <= c2 {
            let mut wused = 0.0f32;
            let mut last_col = col;
            while last_col <= c2 {
                if last_col > col && setup.col_breaks.contains(&last_col) {
                    break;
                }
                let w = sheet.cols.size(last_col) * px;
                if wused + w > body_left && last_col > col {
                    break;
                }
                wused += w;
                last_col += 1;
            }
            let cols_end = last_col.saturating_sub(1).min(c2).max(col);
            pages.push(PageSpan { rows: (row, rows_end), cols: (col, cols_end) });
            col = cols_end + 1;
        }
        row = rows_end + 1;
    }
    if pages.is_empty() {
        pages.push(PageSpan { rows: (r1, r2), cols: (c1, c2) });
    }
    Layout { pages, scale, printable, page_size: (pw, ph) }
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
#[derive(Clone, Debug, Default)]
pub struct Out {
    pub rects: Vec<(f32, f32, f32, f32, [u8; 3])>,
    pub lines: Vec<(f32, f32, f32, f32, f32, [u8; 3])>,
    pub texts: Vec<TextOp>,
    /// 图片：(x, y, w, h, 名字, 原始字节, 扩展名)
    pub images: Vec<(f32, f32, f32, f32, String, Vec<u8>, String)>,
    /// 填充多边形（图表饼图/折线区域）
    pub polys: Vec<(Vec<(f32, f32)>, [u8; 3])>,
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
struct CfState {
    cf: crate::cf::CfSheet,
    stats: crate::cf::CfStats,
}

fn draw_cell(
    wb: &Workbook,
    sheet: &Sheet,
    si: usize,
    cf: Option<&CfState>,
    calc: &mut crate::formula::Calc,
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
    if display::merge_covered(sheet, r, c) {
        return;
    }
    let overlay = cf.map(|st| {
        crate::cf::overlay_at(&st.cf, &st.stats, sheet, r, c, |f, r0, c0| {
            let shifted = crate::cf::shift_formula(f.trim_start_matches('='), r0, c0);
            match crate::formula::parse(&shifted) {
                Ok(node) => matches!(
                    calc.eval_node(&node, si),
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
            if let Some(f) = cell.formula.as_deref() {
                eff = Some(fvalue_to_value(crate::formula::eval_formula(wb, si, f)));
            }
        }
    }
    let Some(view) =
        display::view_at_value(wb, sheet, r, c, overlay.as_ref(), eff.as_ref())
    else {
        return;
    };
    let (mut cw, mut ch) = (w, h);
    if let Some((r1, c1, r2, c2)) = view.merge {
        if (r1, c1) == (r, c) {
            cw = (c1..=c2).map(|cc| sheet.cols.size(cc) * px).sum();
            ch = (r1..=r2).map(|rr| sheet.rows.size(rr) * px).sum();
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
    let pad = 2.0 * scale + view.indent as f32 * size * 0.6;
    let mut lines_txt = if view.wrap {
        display::wrap_lines(&view.text, (cw - pad * 2.0).max(4.0), size)
    } else {
        view.text.split('\n').map(|s| s.to_string()).collect()
    };
    let line_h = size * 1.18;
    // Excel 打印按行高裁剪：放不下的行不画（自动行高时本来就放得下，等于空操作）
    let fit = ((ch / line_h).floor() as usize).max(1);
    if lines_txt.len() > fit {
        lines_txt.truncate(fit);
    }
    let total_h = line_h * lines_txt.len() as f32;
    let bottom = top + ch;
    let mut ty = match view.valign {
        0 => bottom - size * 0.92,
        1 => bottom - (ch - total_h) / 2.0 - size * 0.92,
        _ => bottom - (ch - total_h) - size * 0.28,
    };
    for line in &lines_txt {
        let tw = display::text_width(line, size);
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
    sx: f32,
    sy: f32,
    px: f32,
) -> (f32, f32, f32, f32) {
    let x: f32 = (0..c1).map(|c| sheet.cols.size(c) * px).sum::<f32>() + sx;
    let y_top: f32 = (0..r1).map(|r| sheet.rows.size(r) * px).sum::<f32>();
    let w: f32 = (c1..=c2.max(c1)).map(|c| sheet.cols.size(c) * px).sum();
    let h: f32 = (r1..=r2.max(r1)).map(|r| sheet.rows.size(r) * px).sum();
    (x, sy - y_top - h, w, h)
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
    let (x, y, w, h) = grid_rect(sheet, a, b, c, d, sx, sy, px);
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
        let tw = display::text_width(&label, 9.0);
        chart_text(out, x + (w - tw) / 2.0, y + h / 2.0, 9.0, &label, [120, 90, 40]);
        return;
    }
    let mut top = y + h - 6.0;
    if !ch.title.is_empty() {
        let tw = display::text_width(&ch.title, 10.0);
        chart_text(out, x + (w - tw) / 2.0, top, 10.0, &ch.title, [40, 40, 40]);
        top -= 14.0;
    }
    let legend_w = if ch.legend || !ch.legend_pos.is_empty() { 62.0f32 } else { 0.0 };
    let plot = (x + 34.0, y + 18.0, (w - 44.0 - legend_w).max(20.0), (top - y - 24.0).max(20.0));
    let (px0, py0, pw, ph) = plot;
    out.lines.push((px0, py0, px0 + pw, py0, 0.5, [120, 120, 120]));
    out.lines.push((px0, py0, px0, py0 + ph, 0.5, [120, 120, 120]));

    let max_of = |secondary: bool| {
        ch.series
            .iter()
            .filter(|s| s.secondary == secondary)
            .flat_map(|s| s.vals.iter())
            .cloned()
            .fold(0.0f64, f64::max)
            .max(1e-9)
    };
    let max_main = nice_max(max_of(false));
    let max_sub = nice_max(max_of(true));
    let has_sub = ch.series.iter().any(|s| s.secondary);
    let n = ch.series.iter().map(|s| s.vals.len()).max().unwrap_or(0);
    if !matches!(ch.kind, ChartKind::Pie | ChartKind::Doughnut) {
        // 数值轴刻度：按轴的数字格式显示（百分比轴显示 25% 而不是 0.25）
        let tick = |v: f64| -> String {
            if ch.y_fmt.is_empty() {
                crate::model::format_number(v)
            } else {
                crate::style::format_value(&crate::model::Value::Number(v), &ch.y_fmt)
            }
        };
        for k in 0..=4 {
            let v = max_main * k as f64 / 4.0;
            let txt = tick(v);
            let tw = display::text_width(&txt, 7.0);
            chart_text(out, px0 - tw - 3.0, py0 + ph * (k as f32 / 4.0) - 2.5, 7.0, &txt, [90, 90, 90]);
        }
        // 分类标签（散点用数值 X 轴，不画分类名）
        let scatter = ch
            .series
            .iter()
            .all(|se| matches!(series_kind(se, ch.kind), ChartKind::Scatter));
        if !scatter && n > 0 && n <= 12 {
            let gw = pw / n as f32;
            if let Some(cats) = ch.series.iter().map(|se| &se.cats).find(|c| !c.is_empty()) {
                for (i, c) in cats.iter().take(n).enumerate() {
                    let tw = display::text_width(c, 7.0);
                    chart_text(out, px0 + gw * (i as f32 + 0.5) - tw / 2.0, py0 - 9.0, 7.0, c, [90, 90, 90]);
                }
            }
        }
    }
    if matches!(ch.kind, ChartKind::Pie | ChartKind::Doughnut) {
        let vals = ch.series.first().map(|s| s.vals.clone()).unwrap_or_default();
        let total: f64 = vals.iter().sum::<f64>().max(1e-9);
        let cx = px0 + pw / 2.0;
        let cy = py0 + ph / 2.0;
        let rad = (pw.min(ph) / 2.0 - 4.0).max(4.0);
        let mut start_a = std::f32::consts::FRAC_PI_2;
        for (i, v) in vals.iter().enumerate() {
            let sweep = (*v / total) as f32 * std::f32::consts::TAU;
            let steps = ((sweep / 0.15).ceil() as usize).max(2);
            let mut pts = vec![(cx, cy)];
            for k in 0..=steps {
                let t = start_a - sweep * (k as f32 / steps as f32);
                pts.push((cx + rad * t.cos(), cy + rad * t.sin()));
            }
            out.polys.push((pts, CHART_COLORS[i % CHART_COLORS.len()]));
            start_a -= sweep;
        }
    } else if matches!(ch.kind, ChartKind::Radar) {
        let groups = n.max(2);
        let cx = px0 + pw / 2.0;
        let cy = py0 + ph / 2.0;
        let rad = (pw.min(ph) / 2.0 - 6.0).max(6.0);
        for (si, se) in ch.series.iter().enumerate() {
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
            for k in 0..pts.len() {
                let a = pts[k];
                let b = pts[(k + 1) % pts.len()];
                out.lines.push((a.0, a.1, b.0, b.1, 1.2, color));
                out.rects.push((a.0 - 1.5, a.1 - 1.5, 3.0, 3.0, color));
            }
        }
    } else {
        // 每个系列按自己的图型画；柱/条系列按柱系列序号并排
        let bar_slots: Vec<usize> = ch
            .series
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let k = series_kind(s, ch.kind);
                matches!(k, ChartKind::Column | ChartKind::Bar | ChartKind::BarStacked)
            })
            .map(|(i, _)| i)
            .collect();
        let slot_count = bar_slots.len().max(1);
        let groups = n.max(1);
        let gw = pw / groups as f32;
        let bw = (gw * 0.7 / slot_count as f32).max(1.5);
        for (si, se) in ch.series.iter().enumerate() {
            let color = se.color.unwrap_or(CHART_COLORS[si % CHART_COLORS.len()]);
            let scale = if se.secondary { max_sub } else { max_main };
            match series_kind(se, ch.kind) {
                ChartKind::Line | ChartKind::Scatter => {
                    let use_x = matches!(series_kind(se, ch.kind), ChartKind::Scatter)
                        && se.xv.len() == se.vals.len()
                        && !se.xv.is_empty();
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
                    if pts.len() >= 2 {
                        out.polys.push((pts, color));
                    } else if let Some(p) = pts.first() {
                        out.rects.push((p.0 - 1.5, p.1 - 1.5, 3.0, 3.0, color));
                    }
                }
                ChartKind::Area => {
                    let step = if se.vals.len() > 1 {
                        pw / (se.vals.len() as f32 - 1.0)
                    } else {
                        0.0
                    };
                    let mut pts = vec![(px0, py0)];
                    for (i, v) in se.vals.iter().enumerate() {
                        pts.push((px0 + step * i as f32, py0 + ph * (*v / scale) as f32));
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
        }
    }
    if has_sub {
        out.lines.push((px0 + pw, py0, px0 + pw, py0 + ph, 0.5, [120, 120, 120]));
    }
    if legend_w > 0.0 {
        let mut ly = y + h - 18.0;
        for (i, se) in ch.series.iter().enumerate().take(6) {
            let color = se.color.unwrap_or(CHART_COLORS[i % CHART_COLORS.len()]);
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
            Some(d) => Some(d.clone()),
            None => source.and_then(|p| crate::pic::read_part(p, &pic.part)),
        };
        let Some(bytes) = bytes else { continue };
        let (x, y, w, h) = grid_rect(sheet, r1, c1, r2, c2, sx, sy, px);
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
        let x = (0..c1).map(|c| sheet.cols.size(c) * px).sum::<f32>() + sx + dx1 as f32 / EMU_PX * px;
        let y_top = (0..r1).map(|r| sheet.rows.size(r) * px).sum::<f32>() + dy1 as f32 / EMU_PX * px;
        let x2 = (0..c2).map(|c| sheet.cols.size(c) * px).sum::<f32>() + sx + dx2 as f32 / EMU_PX * px;
        let y2 = (0..r2).map(|r| sheet.rows.size(r) * px).sum::<f32>() + dy2 as f32 / EMU_PX * px;
        let w = (x2 - x).max(8.0);
        let h = (y2 - y_top).max(8.0);
        let yy = sy - y_top - h;
        if let Some(fill) = tb.fill {
            out.rects.push((x, yy, w, h, fill));
        }
        let size = tb.size.max(4.0);
        let mut ty = yy + h - size;
        for line in display::wrap_lines(&tb.text, (w - 4.0).max(4.0), size) {
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
            ty -= size * 1.2;
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
    let sheet = &wb.sheets[sheet_idx];
    let setup = &sheet.page;
    let (pw, ph) = layout.page_size;
    let (ml, mr, mt, mb) = margins_pt(setup);
    let px = layout.scale * 0.75;
    let span = &layout.pages[page_index];
    let mut out = Out::default();

    let mut cf_state: Option<CfState> = None;
    if !sheet.cf.blocks.is_empty() {
        let mut stats = crate::cf::CfStats::default();
        crate::cf::compute_stats(&sheet.cf, sheet, &mut stats);
        cf_state = Some(CfState { cf: sheet.cf.clone(), stats });
    }
    let mut calc = crate::formula::Calc::new(wb);

    // 页眉
    if !setup.header.is_empty() {
        let (l, c, r) = header_parts(&setup.header, page_index + 1, layout.pages.len());
        for (seg, align) in [(l, 0u8), (c, 1), (r, 2)] {
            if seg.is_empty() {
                continue;
            }
            let tw = display::text_width(&seg, 9.0);
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
            let h = sheet.rows.size(r) * px;
            let mut x = ml;
            for c in span.cols.0..=span.cols.1 {
                let w = sheet.cols.size(c) * px;
                draw_cell(wb, sheet, sheet_idx, cf_state.as_ref(), &mut calc, r, c, x, y - h, w, h, px, layout.scale, &mut out);
                x += w;
            }
            y -= h;
        }
    }
    let title_w: f32 = match setup.title_cols {
        Some((ca, cb)) => {
            let mut x = ml;
            for c in ca..=cb.min(span.cols.1).max(ca) {
                let w = sheet.cols.size(c) * px;
                let mut yy = y;
                for r in span.rows.0..=span.rows.1 {
                    let h = sheet.rows.size(r) * px;
                    draw_cell(wb, sheet, sheet_idx, cf_state.as_ref(), &mut calc, r, c, x, yy - h, w, h, px, layout.scale, &mut out);
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
        let h = sheet.rows.size(r) * px;
        let mut x = body_x0;
        for c in span.cols.0..=span.cols.1 {
            let w = sheet.cols.size(c) * px;
            draw_cell(wb, sheet, sheet_idx, cf_state.as_ref(), &mut calc, r, c, x, cursor_y - h, w, h, px, layout.scale, &mut out);
            x += w;
        }
        cursor_y -= h;
    }

    // 网格线（正文格线，标题区也一起）
    if setup.gridlines {
        let mut body_w = 0.0f32;
        for c in span.cols.0..=span.cols.1 {
            body_w += sheet.cols.size(c) * px;
        }
        let mut body_h = 0.0f32;
        for r in span.rows.0..=span.rows.1 {
            body_h += sheet.rows.size(r) * px;
        }
        let mut x = body_x0;
        for c in span.cols.0..=span.cols.1 {
            out.lines.push((x, y - body_h, x, y, 0.4, [205, 205, 205]));
            x += sheet.cols.size(c) * px;
        }
        out.lines.push((x, y - body_h, x, y, 0.4, [205, 205, 205]));
        let mut yy = y;
        for r in span.rows.0..=span.rows.1 {
            out.lines.push((body_x0, yy, body_x0 + body_w, yy, 0.4, [205, 205, 205]));
            yy -= sheet.rows.size(r) * px;
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
            let tw = display::text_width(&seg, 8.5);
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
    let Some(sheet) = wb.sheets.get(sheet_idx) else {
        anyhow::bail!("没有第 {sheet_idx} 张表");
    };
    let layout = paginate(sheet, &sheet.page);
    let total = layout.pages.len();
    let (first, last) = match pages {
        Some((a, b)) => (a.min(total.saturating_sub(1)), b.min(total.saturating_sub(1))),
        None => (0, total.saturating_sub(1)),
    };
    let font = Font::load(font_path)?;
    let mut doc = Doc::new(font);
    let mut exported = 0usize;
    for pi in first..=last {
        if pi >= total {
            break;
        }
        exported += 1;
        let (pw, ph) = layout.page_size;
        doc.add_page(pw, ph);
        let out = page_ops_with(wb, sheet_idx, &layout, pi, source);
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
        let encoded: Vec<(TextOp, String)> = {
            let font = doc.font();
            out
                .texts
                .into_iter()
                .map(|t| {
                    let hex = Page::glyph_hex(font, &t.text);
                    (t, hex)
                })
                .collect()
        };
        for (t, hex) in encoded {
            doc.page_mut().text_styled(t.x, t.baseline, t.size, &hex, t.color, t.bold, t.italic, t.rotation);
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
        FValue::Text(t) => Value::Text(t.into_boxed_str()),
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
