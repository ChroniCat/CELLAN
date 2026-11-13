//! 打印 / 导出 PDF：按页面设置分页，再画到 PDF 上。
//!
//! 坐标换算：表格的列宽行高是「96 DPI 像素」，PDF 用「点」（72 DPI），
//! 所以 100% 缩放时 1px = 0.75pt。页面尺寸与边距都从 `PageSetup` 来。

use crate::model::{Sheet, Value, Workbook};
use crate::page::PageSetup;
use crate::pdf::{Doc, Font, Page};
use anyhow::Result;
use std::path::Path;

/// 纸张尺寸（pt）：(宽, 高)，纵向
fn paper_size(paper: u16) -> (f32, f32) {
    match paper {
        9 => (595.28, 841.89),   // A4
        8 => (841.89, 1190.55),  // A3
        11 => (420.94, 595.28),  // A5
        1 => (612.0, 792.0),     // Letter
        5 => (612.0, 1008.0),    // Legal
        3 => (792.0, 1224.0),    // Tabloid
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

/// 计算分页：按列宽/行高累加，超出可打印区就换页；标题行列每页重复
pub fn paginate(sheet: &Sheet, setup: &PageSetup) -> Layout {
    let (pw, ph) = paper_size(setup.paper);
    let (pw, ph) = if setup.landscape { (ph, pw) } else { (pw, ph) };
    let m = setup.margins;
    let left = m[0] * 72.0;
    let right = m[1] * 72.0;
    let top = m[2] * 72.0;
    let bottom = m[3] * 72.0;
    let printable = ((pw - left - right).max(72.0), (ph - top - bottom).max(72.0));

    // 打印区域：优先用设置，否则用有内容的范围
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

    // 缩放：px → pt
    let mut scale = if setup.fit_to_page {
        // 先把 1 页宽的宽度算出来，再取整页比例
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
    let px = scale * 0.75; // 像素 → 点

    // 标题行/列（每页重复）
    let title_rows = setup.title_rows.map(|(a, b)| (a, b.min(r2))).filter(|(a, b)| a <= b);
    let title_cols = setup.title_cols.map(|(a, b)| (a, b.min(c2))).filter(|(a, b)| a <= b);

    let head_rows_h = |s: &Sheet| -> f32 {
        match title_rows {
            Some((a, b)) => (a..=b).map(|r| s.rows.size(r) * px).sum(),
            None => 0.0,
        }
    };
    let head_cols_w = |s: &Sheet| -> f32 {
        match title_cols {
            Some((a, b)) => (a..=b).map(|c| s.cols.size(c) * px).sum(),
            None => 0.0,
        }
    };
    let hh = head_rows_h(sheet);
    let hw = head_cols_w(sheet);

    let body_top = printable.1 - hh;
    let body_left = printable.0 - hw;
    let mut pages = Vec::new();
    let mut row = r1;
    while row <= r2 {
        // 这一页能放多少行
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
        // 再按列分页
        let mut col = c1;
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
        pages.push(PageSpan { rows: (0, 0), cols: (0, 0) });
    }
    Layout { pages, scale, printable, page_size: (pw, ph) }
}

fn rgb(color: Option<[u8; 3]>, fallback: [u8; 3]) -> [u8; 3] {
    color.unwrap_or(fallback)
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
    let setup = sheet.page.clone();
    let layout = paginate(sheet, &setup);
    let font = Font::load(font_path)?;
    let mut doc = Doc::new(font);
    let (pw, ph) = layout.page_size;
    let m = setup.margins;
    let (ml, mr, mt, mb) = (m[0] * 72.0, m[1] * 72.0, m[2] * 72.0, m[3] * 72.0);
    let px = layout.scale * 0.75;

    for (pi, span) in layout.pages.iter().enumerate() {
        doc.add_page(pw, ph);
        // 先取字体（借用问题：先算出要画的文字，再统一画）
        let mut head_lines: Vec<(f32, f32, f32, String, [u8; 3])> = Vec::new();
        let mut rects: Vec<(f32, f32, f32, f32, [u8; 3])> = Vec::new();
        let mut lines: Vec<(f32, f32, f32, f32, f32, [u8; 3])> = Vec::new();

        let top_y = ph - mt;
        let mut y = top_y;
        // 标题行（每页重复）
        if let Some((ta, tb)) = setup.title_rows {
            for r in ta..=tb.min(span.rows.1) {
                let h = sheet.rows.size(r) * px;
                let mut x = ml;
                for c in span.cols.0..=span.cols.1 {
                    let w = sheet.cols.size(c) * px;
                    draw_cell(sheet, &wb.styles, r, c, x, y - h, w, h, &mut rects, &mut lines, &mut head_lines, px, layout.scale, wb.date_system);
                    x += w;
                }
                y -= h;
            }
        }
        // 标题列（每页重复，放在左边）
        if let Some((ca, cb)) = setup.title_cols {
            let mut x = ml;
            for c in ca..=cb.min(span.cols.1) {
                let w = sheet.cols.size(c) * px;
                let mut yy = y;
                let _ = yy;
                for r in span.rows.0..=span.rows.1 {
                    let h = sheet.rows.size(r) * px;
                    draw_cell(sheet, &wb.styles, r, c, x, yy - h, w, h, &mut rects, &mut lines, &mut head_lines, px, layout.scale, wb.date_system);
                    yy -= h;
                }
                x += w;
            }
        }

        // 正文
        let body_x0 = ml;
        let body_y0 = y;
        let mut cursor_y = body_y0;
        for r in span.rows.0..=span.rows.1 {
            let h = sheet.rows.size(r) * px;
            let mut x = body_x0;
            for c in span.cols.0..=span.cols.1 {
                let w = sheet.cols.size(c) * px;
                draw_cell(sheet, &wb.styles, r, c, x, cursor_y - h, w, h, &mut rects, &mut lines, &mut head_lines, px, layout.scale, wb.date_system);
                x += w;
            }
            cursor_y -= h;
            y -= h;
        }

        // 页脚：&P 换成页码
        if !setup.footer.is_empty() {
            let text = setup.footer.replace("&P", &(pi + 1).to_string()).replace("&N", &layout.pages.len().to_string());
            let tw = doc.font().text_width(&text, 9.0);
            head_lines.push((pw / 2.0 - tw / 2.0, mb * 0.4, 9.0, text, [90, 90, 90]));
        } else {
            let text = format!("第 {} / {} 页", pi + 1, layout.pages.len());
            let tw = doc.font().text_width(&text, 8.5);
            head_lines.push((pw / 2.0 - tw / 2.0, mb * 0.4, 8.5, text, [130, 130, 130]));
        }

        if setup.gridlines {
            // 网格线：每格右下各一条，颜色淡灰
            let mut x0 = ml;
            let mut y0 = body_y0;
            for c in span.cols.0..=span.cols.1 {
                let w = sheet.cols.size(c) * px;
                lines.push((x0, y0, x0, top_y - (y0 - body_y0) - 0.0, 0.5, [200, 200, 200]));
                x0 += w;
            }
            for r in span.rows.0..=span.rows.1 {
                let h = sheet.rows.size(r) * px;
                lines.push((ml, y0 - h, ml + x0 - ml, y0 - h, 0.5, [200, 200, 200]));
                y0 -= h;
            }
        }

        let page: &mut Page = doc.page_mut();
        for (x, yy, w, h, color) in rects {
            page.rect_fill(x, yy, w, h, color);
        }
        for (x1, y1, x2, y2, lw, color) in lines {
            page.line(x1, y1, x2, y2, lw, color);
        }
        // 先算字形号（需要只读借用），再落到页上
        let encoded: Vec<(f32, f32, f32, String, [u8; 3])> = {
            let font = doc.font();
            head_lines
                .into_iter()
                .map(|(x, y, size, text, color)| {
                    (x, y, size, Page::glyph_hex(font, &text), color)
                })
                .collect()
        };
        for (x, y, size, hex, color) in encoded {
            doc.page_mut().text(x, y, size, &hex, color);
        }
    }
    let n = layout.pages.len();
    doc.write(path)?;
    Ok(n)
}

#[allow(clippy::too_many_arguments)]
fn draw_cell(
    sheet: &Sheet,
    styles: &crate::style::StyleTable,
    r: u32,
    c: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rects: &mut Vec<(f32, f32, f32, f32, [u8; 3])>,
    lines: &mut Vec<(f32, f32, f32, f32, f32, [u8; 3])>,
    texts: &mut Vec<(f32, f32, f32, String, [u8; 3])>,
    px: f32,
    scale: f32,
    date_system: crate::model::DateSystem,
) {
    let style = styles.get(sheet.style_of(r, c));
    // 底色
    if let Some(fill) = style.fill {
        rects.push((x, y, w, h, fill));
    }
    let _ = px;
    // 边框
    let b = style.border;
    let bl = [b[0], b[1], b[2], b[3]];
    if bl[0].style != 0 {
        lines.push((x, y, x, y + h, bl[0].px(), bl[0].color));
    }
    if bl[1].style != 0 {
        lines.push((x + w, y, x + w, y + h, bl[1].px(), bl[1].color));
    }
    if bl[2].style != 0 {
        lines.push((x, y + h, x + w, y + h, bl[2].px(), bl[2].color));
    }
    if bl[3].style != 0 {
        lines.push((x, y, x + w, y, bl[3].px(), bl[3].color));
    }
    // 文字
    let Some(cell) = sheet.get(r, c) else { return };
    // 打印/PDF 必须走和屏幕相同的数字格式；直接用 Value::display()
    // 会把日期、百分比、货币打印成原始序列号/小数。
    let text = match &cell.value {
        Value::Empty => return,
        v if style.format.trim().is_empty() => v.display(),
        v => crate::style::format_value_in(date_system, v, &style.format),
    };
    if text.is_empty() {
        return;
    }
    // 样式里的字号本来单位就是 pt，直接乘页面缩放；px 只用来算格子尺寸
    let _ = px;
    let size = (if style.size > 0.0 { style.size } else { 11.0 } * scale).max(3.0);
    // 数字右对齐（Excel 默认），其余左对齐
    let is_num = matches!(cell.value, Value::Number(_));
    let offset = if is_num {
        (w - 3.0 - text_width_hint(&text, size)).max(1.0)
    } else {
        2.0
    };
    texts.push((
        x + offset,
        y + (h - size) / 2.0 + size * 0.25,
        size,
        text,
        style.color,
    ));
}

/// 排版用的宽度估算：汉字按 1 个字号宽，ASCII 按 0.55
fn text_width_hint(s: &str, size: f32) -> f32 {
    s.chars()
        .map(|c| if (c as u32) > 0x2e80 { size } else { size * 0.55 })
        .sum()
}
