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
            (target_w / total_w).min(1.0)
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
            scale = scale.min((target_h / total_h).min(1.0));
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
    /// 图片：(x, y, w, h, 部件名或内存键)
    pub images: Vec<(f32, f32, f32, f32, ImageRef)>,
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

#[derive(Clone, Debug, PartialEq)]
pub enum ImageRef {
    /// 打开文件里的 `xl/media/...`
    Part(String),
    /// 本会话插入的图片（PNG/JPEG 原始字节）
    Data(std::sync::Arc<Vec<u8>>, String),
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
    let lines_txt = if view.wrap {
        display::wrap_lines(&view.text, (cw - pad * 2.0).max(4.0), size)
    } else {
        view.text.split('\n').map(|s| s.to_string()).collect()
    };
    let line_h = size * 1.18;
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

/// 生成一页的绘制指令（预览直接复用；PDF 再把文字编码成字形号）
pub fn page_ops(
    wb: &Workbook,
    sheet_idx: usize,
    layout: &Layout,
    page_index: usize,
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
    let Some(sheet) = wb.sheets.get(sheet_idx) else {
        anyhow::bail!("没有第 {sheet_idx} 张表");
    };
    let layout = paginate(sheet, &sheet.page);
    let font = Font::load(font_path)?;
    let mut doc = Doc::new(font);
    for (pi, _) in layout.pages.iter().enumerate() {
        let (pw, ph) = layout.page_size;
        doc.add_page(pw, ph);
        let out = page_ops(wb, sheet_idx, &layout, pi);
        for (x, y, w, h, color) in out.rects {
            doc.page_mut().rect_fill(x, y, w, h, color);
        }
        for (x1, y1, x2, y2, lw, color) in out.lines {
            doc.page_mut().line(x1, y1, x2, y2, lw, color);
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
    let n = layout.pages.len();
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
