//! Copy and cut from a sheet, including Office HTML and delayed exports.

use super::{Clip, current_os_clipboard_seq};
use crate::{SheetApp, column_clip, office_clipboard};
use eframe::egui;
use sheetcore::Workbook;

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\n' => out.push_str("<br style=\"mso-data-placement:same-cell;\">"),
            _ => out.push(ch),
        }
    }
    out
}

impl SheetApp {
    /// 选区 → Excel/WPS 可粘贴的 HTML 表格（含合并、字体、填充、边框、对齐、公式）。
    pub(crate) fn selection_html(&self, si: usize, rect: (u32, u32, u32, u32)) -> String {
        Self::clipboard_html(&self.wb, si, rect)
    }

    fn clipboard_html(wb: &Workbook, si: usize, rect: (u32, u32, u32, u32)) -> String {
        Self::clipboard_html_filtered(wb, si, rect, None)
    }

    pub(crate) fn clipboard_html_filtered(
        wb: &Workbook,
        si: usize,
        rect: (u32, u32, u32, u32),
        visible_rows: Option<&[u32]>,
    ) -> String {
        use std::fmt::Write as _;
        let (r1, c1, r2, c2) = rect;
        let sh = wb.sheet(si);
        let area = match visible_rows {
            Some(rows) => (rows.len() as u64).saturating_mul(u64::from(c2 - c1 + 1)),
            None => u64::from(r2 - r1 + 1).saturating_mul(u64::from(c2 - c1 + 1)),
        };
        let trim_empty = r2 >= 1_000_000 || c2 >= 16_000 || area > 4_096;
        let merges: Vec<(u32, u32, u32, u32)> = if visible_rows.is_some() {
            Vec::new()
        } else {
            sh.merges
                .iter()
                .filter(|(a, b, c, d)| *a >= r1 && *b >= c1 && *c <= r2 && *d <= c2)
                .copied()
                .collect()
        };
        let (eff_r2, eff_c2) = if trim_empty {
            let mut max_r: Option<u32> = None;
            let mut max_c: Option<u32> = None;
            let mut include = |rr: u32, cc: u32| {
                max_r = Some(max_r.map_or(rr, |m| m.max(rr)));
                max_c = Some(max_c.map_or(cc, |m| m.max(cc)));
            };
            if let Some((_, _, ur2, uc2)) = sh.used_bounds() {
                let cr = r2.min(ur2);
                let cc = c2.min(uc2);
                if cr >= r1 && cc >= c1 {
                    if visible_rows.is_none() && (cr - r1) >= 256 && (cc - c1) <= 64 {
                        for col in c1..=cc {
                            let hit = if sh.get(cr, col).is_some() {
                                Some(cr)
                            } else {
                                sh.next_occupied_in_col(col, cr, false).filter(|&r| r >= r1)
                            };
                            if let Some(hr) = hit {
                                include(hr, col);
                            }
                        }
                    } else {
                        include(cr, cc);
                    }
                }
            }
            if let Some((sr2, sc2)) = sh.styles.max_coords() {
                let cr = r2.min(sr2);
                let cc = c2.min(sc2);
                if cr >= r1 && cc >= c1 {
                    if visible_rows.is_none() && (cr - r1) >= 256 && (cc - c1) <= 64 {
                        for (k, _) in sh.styles.iter_rect((r1, c1, cr, cc)) {
                            include(k.row, k.col);
                        }
                    } else {
                        include(cr, cc);
                    }
                }
            }
            for &(_, _, mr2, mc2) in &merges {
                include(mr2, mc2);
            }
            let (Some(er), Some(ec)) = (max_r, max_c) else {
                return "<table border=\"0\" cellspacing=\"0\" cellpadding=\"2\" style=\"border-collapse:collapse\"></table>".into();
            };
            (er, ec)
        } else {
            (r2, c2)
        };
        let mut merge_index = sheetcore::merges::MergeIndex::default();
        merge_index.refresh(&merges);

        let mut css_cache: std::collections::HashMap<(u16, bool), String> =
            std::collections::HashMap::new();
        let build_css = |sid: u16, has_nl: bool| -> String {
            let st = wb.styles.get(sid);
            let mut css = String::with_capacity(96);
            if !st.font.is_empty() {
                css.push_str("font-family:");
                for ch in st.font.chars() {
                    if ch != '"' {
                        css.push(ch);
                    }
                }
                css.push(';');
            }
            if st.size > 0.0 {
                let _ = write!(css, "font-size:{}pt;", st.size);
            }
            if st.bold {
                css.push_str("font-weight:bold;");
            }
            if st.italic {
                css.push_str("font-style:italic;");
            }
            match (st.underline, st.strikethrough) {
                (true, true) => css.push_str("text-decoration:underline line-through;"),
                (true, false) => css.push_str("text-decoration:underline;"),
                (false, true) => css.push_str("text-decoration:line-through;"),
                (false, false) => {}
            }
            let _ = write!(
                css,
                "color:#{:02X}{:02X}{:02X};",
                st.color[0], st.color[1], st.color[2]
            );
            if let Some(fill) = st.fill {
                let _ = write!(
                    css,
                    "background:#{:02X}{:02X}{:02X};",
                    fill[0], fill[1], fill[2]
                );
            }
            if !st.format.is_empty() {
                css.push_str("mso-number-format:'");
                for ch in st.format.chars() {
                    if ch != '\\' && ch != '\'' {
                        css.push(ch);
                    }
                }
                css.push_str("';");
            }
            match st.halign {
                1 => css.push_str("text-align:left;"),
                2 => css.push_str("text-align:center;"),
                3 => css.push_str("text-align:right;"),
                _ => {}
            }
            match st.valign {
                1 => css.push_str("vertical-align:top;"),
                2 => css.push_str("vertical-align:middle;"),
                3 => css.push_str("vertical-align:bottom;"),
                _ => {}
            }
            if st.wrap || has_nl {
                css.push_str("white-space:pre-wrap;");
            }
            for (i, b) in st.border.iter().enumerate() {
                if b.style > 0 {
                    let width = match b.style {
                        2 => "2px",
                        3 | 4 => "3px",
                        _ => "1px",
                    };
                    let side = ["left", "right", "top", "bottom"][i];
                    let _ = write!(
                        css,
                        "border-{side}:{width} solid #{:02X}{:02X}{:02X};",
                        b.color[0], b.color[1], b.color[2]
                    );
                }
            }
            css
        };

        let row_count_est = visible_rows.map_or((eff_r2 - r1 + 1) as usize, <[u32]>::len);
        let area_est = row_count_est
            .saturating_mul((eff_c2 - c1 + 1) as usize)
            .min(65_536);
        let mut out = String::with_capacity(128 + area_est * 32);
        out.push_str(
            "<table border=\"0\" cellspacing=\"0\" cellpadding=\"2\" style=\"border-collapse:collapse\">",
        );
        let blank_cell = sheetcore::model::Cell::default();
        let mut emit_tr = |r: u32| {
            out.push_str("<tr>");
            for c in c1..=eff_c2 {
                let merge = merge_index.at(r, c);
                if merge.is_some_and(|(a, b, _, _)| (r, c) != (a, b)) {
                    continue;
                }
                let (cs, rs) = match merge {
                    Some((_, _, er, ec)) => (ec - c + 1, er - r + 1),
                    None => (1, 1),
                };
                let cell_opt = sh.get(r, c);
                let cell = cell_opt.as_deref().unwrap_or(&blank_cell);
                let sid = sh.style_of(r, c);
                let st = wb.styles.get(sid);
                let formatted = if matches!(cell.value, sheetcore::model::Value::Empty) {
                    String::new()
                } else {
                    sheetcore::style::format_value_in(wb.date_system, &cell.value, &st.format)
                };
                let has_nl = formatted.contains('\n');
                let css = css_cache
                    .entry((sid, has_nl))
                    .or_insert_with(|| build_css(sid, has_nl));

                out.push_str("<td");
                if cs > 1 {
                    let _ = write!(out, " colspan=\"{cs}\"");
                }
                if rs > 1 {
                    let _ = write!(out, " rowspan=\"{rs}\"");
                }
                if !css.is_empty() {
                    out.push_str(" style=\"");
                    out.push_str(css);
                    out.push('"');
                }
                if let Some(f) = &cell.formula {
                    out.push_str(" x:fmla=\"");
                    out.push_str(&html_escape(f));
                    out.push('"');
                }
                if let sheetcore::model::Value::Number(n) = &cell.value {
                    let _ = write!(out, " x:num=\"{n}\"");
                }
                out.push('>');
                if !formatted.is_empty() {
                    out.push_str(&html_escape(&formatted));
                }
                out.push_str("</td>");
            }
            out.push_str("</tr>");
        };
        match visible_rows {
            Some(rows) => {
                let last_idx = if trim_empty {
                    rows.iter().rposition(|&r| r <= eff_r2)
                } else {
                    rows.len().checked_sub(1)
                };
                if let Some(end) = last_idx {
                    for &r in &rows[..=end] {
                        emit_tr(r);
                    }
                }
            }
            None => {
                for r in r1..=eff_r2 {
                    emit_tr(r);
                }
            }
        }
        out.push_str("</table>");
        out
    }

    pub(crate) fn clip_copy(&mut self, cut: bool, ctx: &egui::Context) {
        let si = self.sheet_idx;
        // 多区域复制（Excel 语义：多个非连续区域若同行或同列对齐，自动拼接成连续矩形复制）
        if !self.extra_sel.is_empty() {
            if cut {
                self.status = "不支持对多个不连续区域同时剪切".into();
                return;
            }
            let mut rects = self.sel_rects();
            let same_cols = rects.iter().all(|r| r.1 == rects[0].1 && r.3 == rects[0].3);
            let same_rows = rects.iter().all(|r| r.0 == rects[0].0 && r.2 == rects[0].2);
            if !same_cols && !same_rows {
                self.status = "多重选区仅支持同行或同列对齐的区域复制".into();
                return;
            }
            let total_cells: u64 = rects
                .iter()
                .map(|&(a, b, c, d)| u64::from(c - a + 1) * u64::from(d - b + 1))
                .sum();
            if total_cells > 200_000 {
                self.status = "多重选区复制范围过大（上限 20 万格）".into();
                return;
            }
            let mut cells: Vec<Vec<(sheetcore::model::Cell, u16)>> = Vec::new();
            let mut source_rows: Vec<u32> = Vec::new();
            let mut source_cols: Option<Vec<u32>> = None;
            let mut tsv = String::new();
            let filter_hidden = &self.wb.sheet(si).hidden_rows;
            if same_cols {
                rects.sort_by_key(|r| r.0);
                let (c1, c2) = (rects[0].1, rects[0].3);
                for (a, _, c, _) in &rects {
                    for r in *a..=*c {
                        if filter_hidden.contains(&r) {
                            continue;
                        }
                        let mut row = Vec::with_capacity((c2 - c1 + 1) as usize);
                        for col in c1..=c2 {
                            let cell = self.wb.sheet(si).get(r, col).map(|x| x.into_owned()).unwrap_or_default();
                            let st = self.wb.sheet(si).style_of(r, col);
                            let style = self.wb.styles.get(st);
                            if col != c1 { tsv.push('\t'); }
                            tsv.push_str(&sheetcore::style::format_value_in(self.wb.date_system, &cell.value, &style.format));
                            row.push((cell, st));
                        }
                        tsv.push('\n');
                        source_rows.push(r);
                        cells.push(row);
                    }
                }
            } else {
                rects.sort_by_key(|r| r.1);
                let (r1, r2) = (rects[0].0, rects[0].2);
                let mut cols_list: Vec<u32> = Vec::new();
                for &(_, b, _, d) in &rects {
                    for col in b..=d {
                        cols_list.push(col);
                    }
                }
                for r in r1..=r2 {
                    if filter_hidden.contains(&r) {
                        continue;
                    }
                    let mut row = Vec::with_capacity(cols_list.len());
                    for (ci, &col) in cols_list.iter().enumerate() {
                        let cell = self.wb.sheet(si).get(r, col).map(|x| x.into_owned()).unwrap_or_default();
                        let st = self.wb.sheet(si).style_of(r, col);
                        let style = self.wb.styles.get(st);
                        if ci > 0 { tsv.push('\t'); }
                        tsv.push_str(&sheetcore::style::format_value_in(self.wb.date_system, &cell.value, &style.format));
                        row.push((cell, st));
                    }
                    tsv.push('\n');
                    source_rows.push(r);
                    cells.push(row);
                }
                source_cols = Some(cols_list);
            }
            if cells.is_empty() {
                self.status = "多重选区内没有可复制的可见单元格".into();
                return;
            }
            let h = cells.len() as u32;
            let w = cells[0].len() as u32;
            ctx.copy_text(tsv.clone());
            self.retire_previous_clipboard();
            self.clip_stamp = Some(tsv);
            self.clip_seq = None;
            self.clip_seq_pending_sync = true;
            self.clip_copied_at = Some(std::time::Instant::now());
            self.clip_pic = None;
            let r_min = rects.iter().map(|x| x.0).min().unwrap_or(0);
            let c_min = rects.iter().map(|x| x.1).min().unwrap_or(0);
            self.capture_clip_notes_mapped(
                si,
                (r_min, c_min, r_min + h - 1, c_min + w - 1),
                Some(&source_rows),
                source_cols.as_deref(),
            );
            self.clip = Some(Clip {
                deferred: None,
                sheet: si,
                rect: (r_min, c_min, r_min + h - 1, c_min + w - 1),
                source_rows: Some(source_rows),
                source_cols,
                cells,
                merges: Vec::new(),
                cut: false,
            });
            self.status = format!("已合并复制 {} 个区域（{h} 行 × {w} 列）", rects.len());
            return;
        }
        let (r1, c1, r2, c2) = self.sel_rect();
        let rect = (r1, c1, r2, c2);
        let full_columns = !cut && office_clipboard::delayed::is_full_columns(rect);
        let area = u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1);
        let eager_debug =
            std::env::var_os("SC_CLIP_LOG").is_some() || std::env::var_os("SC_CLIP_DUMP").is_some();
        if !full_columns && (cut || eager_debug) && area > sheetcore::undo::MAX_DENSE_RECT_KEYS as u64 {
            self.status = "选区太大（超过 50 万格），请缩小区域或使用整列复制".into();
            return;
        }
        if !full_columns && !cut && self.net.is_some() && area > sheetcore::undo::MAX_DENSE_RECT_KEYS as u64 {
            let non_empty = self.wb.sheet(si).iter_rect(rect).count();
            if non_empty > sheetcore::undo::MAX_DENSE_RECT_KEYS {
                self.status = "选区实际非空单元格超过 50 万个，请分批复制".into();
                return;
            }
        }
        // Excel 的筛选复制只带走可见行；hidden_rows 是被筛选/分级折叠掉的行，
        // hidden_rows_user 是用户手动隐藏的行，Excel 仍会复制后者。
        let filter_hidden = &self.wb.sheet(si).hidden_rows;
        let filtered = filter_hidden.iter().any(|row| (r1..=r2).contains(row));
        if full_columns && column_clip::intersects_array(self.wb.sheet(si), c1, c2) {
            self.status = "整列包含数组公式或溢出区域，请选中具体数据区域后复制".into();
            return;
        }
        if full_columns && self.wb.sheet(si).merges.iter().any(|&(top, left, bottom, right)| {
            left <= c2 && right >= c1 && (left < c1 || right > c2 || top > bottom || bottom >= sheetcore::sync::MAX_ROWS)
        }) {
            self.status = "整列选区不能完整表示合并单元格，请确认合并范围并选中涉及的全部列".into();
            return;
        }
        if full_columns && filtered {
            self.status = "整列包含筛选隐藏行，请选中要复制的数据区域".into();
            return;
        }
        let precomputed_visible: Option<Vec<u32>> =
            filtered.then(|| (r1..=r2).filter(|r| !filter_hidden.contains(r)).collect());
        let effective_area = match &precomputed_visible {
            Some(vr) => (vr.len() as u64) * u64::from(c2 - c1 + 1),
            None => area,
        };
        if full_columns || (effective_area > 1_024 && !eager_debug) {
            let (range, source_rows, merges, status_msg) = if let Some(visible_rows) = precomputed_visible {
                let visible_count = visible_rows.len();
                let range = std::sync::Arc::new(office_clipboard::delayed::Range::new_filtered(
                    &self.wb,
                    si,
                    rect,
                    visible_rows.clone(),
                ));
                let status = format!(
                    "已{}可见行（{} 行 × {} 列；筛选隐藏 {} 行）",
                    if cut { "剪切" } else { "复制" },
                    visible_count,
                    c2 - c1 + 1,
                    (r2 - r1 + 1) as usize - visible_count
                );
                (range, Some(visible_rows), Vec::new(), status)
            } else {
                let range = std::sync::Arc::new(office_clipboard::delayed::Range::new(&self.wb, si, rect));
                let merges = self
                    .wb
                    .sheet(si)
                    .merges
                    .iter()
                    .copied()
                    .filter(|&(a, b, c, d)| a >= r1 && b >= c1 && c <= r2 && d <= c2)
                    .collect();
                let status = format!(
                    "已{} {} 行 × {} 列",
                    if cut { "剪切" } else { "复制" },
                    r2 - r1 + 1,
                    c2 - c1 + 1
                );
                (range, None, merges, status)
            };
            let owner = office_clipboard::delayed::Owner::publish(range.clone());
            if full_columns && owner.is_none() {
                self.status = "系统剪贴板正忙，请重新复制整列".into();
                return;
            }
            self.clip_stamp = if owner.is_none() {
                let plain = range.text();
                ctx.copy_text(plain.clone());
                Some(plain)
            } else { None };
            self.retire_previous_clipboard();
            if owner.is_some() {
                self.clip_seq = current_os_clipboard_seq();
                self.clip_seq_pending_sync = false;
            } else {
                self.clip_seq = None;
                self.clip_seq_pending_sync = true;
            }
            self.clip_copied_at = Some(std::time::Instant::now());
            self.clipboard_owner = owner;
            self.clip_pic = None;
            self.capture_clip_notes_mapped(si, rect, source_rows.as_deref(), None);
            self.clip = Some(Clip { sheet: si, rect, deferred: Some(range), cells: Vec::new(), merges, source_rows, source_cols: None, cut });
            self.status = status_msg;
            return;
        }
        let visible_rows: Vec<u32> = (r1..=r2)
            .filter(|r| !filter_hidden.contains(r))
            .collect();
        let filtered_copy = visible_rows.len() != (r2 - r1 + 1) as usize;
        let mut cells = Vec::with_capacity(visible_rows.len());
        let mut tsv = String::new();
        for &r in &visible_rows {
            let mut row = Vec::with_capacity((c2 - c1 + 1) as usize);
            for c in c1..=c2 {
                let cell = self.wb.sheet(si).get(r, c).map(|c| c.into_owned()).unwrap_or(sheetcore::model::Cell {
                    value: sheetcore::model::Value::Empty,
                    formula: None,
                });
                let st = self.wb.sheet(si).style_of(r, c);
                let style = self.wb.styles.get(st);
                if c != c1 { tsv.push('\t'); }
                tsv.push_str(&sheetcore::style::format_value_in(self.wb.date_system, &cell.value, &style.format));
                row.push((cell, st));
            }
            tsv.push('\n');
            cells.push(row);
        }
        if filtered_copy {
            // XMLSS/HTML 不能表达非连续行；筛选复制先按 Excel 的可见单元格语义
            // 压紧成纯文本，保证粘贴不把隐藏行带回来，也不让 Excel 误用带隐藏行的块。
            ctx.copy_text(tsv.clone());
            self.retire_previous_clipboard();
            self.clip_stamp = Some(tsv);
            self.clip_seq = None;
            self.clip_seq_pending_sync = true;
            self.clip_copied_at = Some(std::time::Instant::now());
            let visible_count = visible_rows.len();
            self.clip_pic = None;
            self.capture_clip_notes_mapped(si, (r1, c1, r2, c2), Some(&visible_rows), None);
            self.clip = Some(Clip { deferred: None,
                sheet: si,
                rect: (r1, c1, r2, c2),
                source_rows: Some(visible_rows),
                source_cols: None,
                cells,
                merges: Vec::new(),
                cut,
            });
            self.status = format!(
                "已{}可见行（{} 行 × {} 列；筛选隐藏 {} 行）",
                if cut { "剪切" } else { "复制" },
                visible_count,
                c2 - c1 + 1,
                (r2 - r1 + 1) as usize - visible_count
            );
            return;
        }
        let html = self.selection_html(si, (r1, c1, r2, c2));
        if std::env::var("SC_CLIP_LOG").is_ok() {
            eprintln!("[clip-html] {html}");
        }
        let xmlss = office_clipboard::selection_xmlss(&self.wb, si, (r1, c1, r2, c2));
        if let Ok(path) = std::env::var("SC_CLIP_DUMP") {
            if let Err(e) = std::fs::write(&path, &xmlss) {
                eprintln!("[clip] dump failed: {e}");
            }
        }
        if std::env::var("SC_CLIP_LOG").is_ok() {
            eprintln!("[clip-xmlss] {}", &xmlss[..xmlss.len().min(400)]);
        }
        let html_ok = office_clipboard::write_office_clipboard(&xmlss, &html, &tsv);
        if !html_ok {
            ctx.copy_text(tsv.clone());
        }
        self.retire_previous_clipboard();
        self.clip_stamp = Some(tsv);
        if html_ok {
            self.clip_seq = current_os_clipboard_seq();
            self.clip_seq_pending_sync = false;
        } else {
            self.clip_seq = None;
            self.clip_seq_pending_sync = true;
        }
        self.clip_copied_at = Some(std::time::Instant::now());
        let merges = self.wb.sheet(si).merges.iter()
            .filter(|(a, b, c, d)| *a >= r1 && *b >= c1 && *c <= r2 && *d <= c2)
            .copied()
            .collect();
        self.clip_pic = None;
        self.capture_clip_notes_mapped(si, (r1, c1, r2, c2), None, None);
        self.clip = Some(Clip { deferred: None, sheet: si, rect: (r1, c1, r2, c2), source_rows: None, source_cols: None, cells, merges, cut });
        self.status = format!(
            "已{} {:?}（{} 行 × {} 列）",
            if cut { "剪切" } else { "复制" },
            (r1, c1, r2, c2),
            r2 - r1 + 1,
            c2 - c1 + 1
        );
    }
}
