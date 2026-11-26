//! 按内容测量行高与列宽，供编辑、选区命令和表头双击使用。

use crate::{SheetApp, bold_font, cell_family};
use eframe::egui;
use sheetcore::{Value, cell_name};
use std::collections::HashMap;

impl SheetApp {
    /// 自动换行行高自适应：仅撑高不足的行，不缩窄用户已手动调高的行。
    pub(super) fn autofit_row_for_wrap(&mut self, r: u32) {
        let sh = self.wb.sheet(self.sheet_idx);
        let cur_h = sh.rows.size(r);
        let mut needed = cur_h;
        for (cr, cell) in sh.iter_rect((r, 0, r, 16_383)) {
            let sid = sh.style_of(cr.row, cr.col);
            let st = self.wb.styles.get(sid);
            if !st.wrap {
                continue;
            }
            let txt = sheetcore::style::format_value_in(
                self.wb.date_system,
                &cell.value,
                &st.format,
            );
            if txt.is_empty() {
                continue;
            }
            let col_w = (sh.cols.size(cr.col) - 8.0).max(20.0);
            let font_sz = st.size.max(8.0);
            let line_h = (font_sz * 1.45).max(16.0);
            let mut lines = 0usize;
            for para in txt.split('\n') {
                let mut w_est = 0.0f32;
                for ch in para.chars() {
                    w_est += if ch.is_ascii() { font_sz * 0.62 } else { font_sz * 1.05 };
                }
                lines += ((w_est / col_w).ceil() as usize).max(1);
            }
            let h = (lines as f32 * line_h + 6.0).min(409.0);
            if h > needed {
                needed = h;
            }
        }
        if needed > cur_h + 1.0 {
            self.wb.sheet_mut(self.sheet_idx).rows.set(r, needed);
        }
    }

    pub(super) fn autofit_sel_rows_for_wrap(&mut self) {
        let mut rows_seen = std::collections::BTreeSet::new();
        for (r1, _, r2, _) in self.sel_rects() {
            for r in r1..=r2 {
                rows_seen.insert(r);
                if rows_seen.len() >= 2048 {
                    break;
                }
            }
            if rows_seen.len() >= 2048 {
                break;
            }
        }
        let mut changed = false;
        let si = self.sheet_idx;
        let mut edit = sheetcore::undo::MetaEdit {
            sheet: si,
            ..Default::default()
        };
        for r in rows_seen {
            let before = self.wb.sheet(si).rows.size(r);
            self.autofit_row_for_wrap(r);
            let after = self.wb.sheet(si).rows.size(r);
            if (after - before).abs() > 0.5 {
                edit.row_heights.push((r, Some(before), Some(after)));
                changed = true;
            }
        }
        if changed {
            self.wb.sheet_mut(si).layout_dirty = true;
            self.undo.push_meta("自动换行调整行高", (si, self.sel.0, self.sel.1), edit);
            self.doc_gen = self.doc_gen.wrapping_add(1);
        }
    }

    /// 选中行自动调整行高（批量：单遍稀疏区域扫描 + 字号单行缓存 + 单次 `rows.set_many` 重建 + Undo）
    pub(super) fn autofit_sel_rows(&mut self, ctx: &egui::Context) {
        if self.prot_denied(sheetcore::protection::ProtOp::FormatRows, "调整行高") {
            return;
        }
        let (r1, _, mut r2, _) = self.sel_rect();
        if r2 - r1 > 2_000 {
            if let Some((_, _, ur2, _)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                if ur2 >= r1 {
                    r2 = r2.min(ur2);
                } else {
                    r2 = r1;
                }
            } else {
                r2 = r1;
            }
        }
        if r2 - r1 > 50_000 {
            self.status = "选中非空行数过多，请缩小范围后再自适应行高".into();
            return;
        }
        if r1 == r2 {
            self.autofit_row(ctx, r1);
            return;
        }
        let updates = self.compute_autofit_rows(ctx, r1, r2);
        let msg = format!("第 {}–{} 行行高已自适应", r1 + 1, r2 + 1);
        self.apply_row_heights_undoable(updates, msg);
    }

    /// 选中列自动调整列宽（批量：共享二分 `MergeIndex` + O(1) 数值格式宽度上界粗筛 + 并行扫描 + Undo）
    pub(super) fn autofit_sel_cols(&mut self, ctx: &egui::Context) {
        if self.prot_denied(sheetcore::protection::ProtOp::FormatColumns, "调整列宽") {
            return;
        }
        let (_, c1, _, mut c2) = self.sel_rect();
        if c2 - c1 > 500 {
            if let Some((_, _, _, uc2)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                if uc2 >= c1 {
                    c2 = c2.min(uc2);
                } else {
                    c2 = c1;
                }
            } else {
                c2 = c1;
            }
        }
        if c2 - c1 > 2_048 {
            self.status = "选中非空列数过多，请缩小范围后再自适应列宽".into();
            return;
        }
        if c1 == c2 {
            self.autofit_col(ctx, c1);
            return;
        }
        let updates = self.compute_autofit_cols(c1, c2);
        let msg = format!(
            "{}–{} 列宽已自适应",
            cell_name(0, c1).trim_end_matches('1'),
            cell_name(0, c2).trim_end_matches('1')
        );
        self.apply_col_widths_undoable(updates, msg);
    }

    /// 批量计算列区间自适应宽度（全列稀疏扫描 + O(log M) 合并格跳过 + 格式化数值/日期精确字符上界 + 多列并行扫描）。
    fn compute_autofit_cols(&mut self, c1: u32, c2: u32) -> Vec<(u32, f32)> {
        let c2 = c2.min(16_383);
        if c1 > c2 {
            return Vec::new();
        }
        let count = (c2 - c1 + 1) as usize;
        self.cf_inputs
            .merges
            .ensure(self.wb.sheet(self.sheet_idx), self.doc_gen, self.wb_epoch);
        let sh = &self.wb.sheets[self.sheet_idx];
        let styles = &self.wb.styles;
        let date_system = self.wb.date_system;
        let merges = &self.cf_inputs.merges;
        let Some((_, _, r2, _)) = sh.used_bounds() else {
            return (c1..=c2).map(|col| (col, 72.0)).collect();
        };
        let sc_log = std::env::var_os("SC_AUTOFIT").is_some();
        let style_bounds = build_autofit_style_bounds(styles);

        let scan_band = |band_c1: u32, band_c2: u32| -> Vec<(u32, f32)> {
            let n_cols = (band_c2 - band_c1 + 1) as usize;
            let mut widths = vec![17.0f32; n_cols];
            let mut saw_text = vec![false; n_cols];
            for (key, cell) in sh.iter_rect((0, band_c1, r2, band_c2)) {
                if cell.is_blank() {
                    continue;
                }
                let (r, col) = (key.row, key.col);
                // Excel 的 AutoFit 不参与合并格：合并区宽度由用户/列宽决定。
                if merges.at(r, col).is_some() {
                    continue;
                }
                let idx = (col - band_c1) as usize;
                let sid = sh.style_of(r, col);
                let st = styles.get(sid);
                let font_size = if st.size > 0.0 { st.size } else { 11.0 };
                let font = if st.font.trim().is_empty() {
                    "等线"
                } else {
                    st.font.as_str()
                };
                if !sc_log {
                    match &cell.value {
                        Value::Text(s) if st.format.is_empty() || st.format == "@" => {
                            if s.is_empty() {
                                continue;
                            }
                            saw_text[idx] = true;
                            if !st.wrap
                                && s.is_ascii()
                                && s.len() < AUTOFIT_ASCII_LEN_CAP
                                && let Some(meta) = style_bounds.get(sid as usize)
                            {
                                let exact_px = meta.ascii_px[s.len()];
                                widths[idx] = widths[idx].max(exact_px);
                                continue;
                            }
                            let ub = sheetcore::display::autofit_chars_upper_bound(
                                s.len(),
                                st.bold,
                                st.indent,
                                font_size,
                            ) * 8.0
                                + 5.0;
                            if ub <= widths[idx] {
                                continue;
                            }
                            let chars =
                                sheetcore::display::excel_autofit_chars_wrapped_at_size_font(
                                    s, st.bold, st.indent, st.wrap, font_size, font,
                                );
                            widths[idx] = widths[idx].max(chars * 8.0 + 5.0);
                            continue;
                        }
                        Value::Number(n) if n.is_finite() => {
                            if let Some(meta) = style_bounds.get(sid as usize)
                                && let Some(max_chars) = meta.fmt.max_ascii_chars(*n)
                                && max_chars < AUTOFIT_ASCII_LEN_CAP
                            {
                                saw_text[idx] = true;
                                let ub_px = meta.ascii_px[max_chars];
                                if ub_px <= widths[idx] {
                                    continue;
                                }
                            }
                        }
                        Value::Bool(b) if st.format.is_empty() => {
                            saw_text[idx] = true;
                            let len = if *b { 4 } else { 5 };
                            if let Some(meta) = style_bounds.get(sid as usize) {
                                widths[idx] = widths[idx].max(meta.ascii_px[len]);
                                continue;
                            }
                        }
                        _ => {}
                    }
                }
                let txt = sheetcore::style::format_value_in(date_system, &cell.value, &st.format);
                if txt.is_empty() {
                    continue;
                }
                saw_text[idx] = true;
                let chars = sheetcore::display::excel_autofit_chars_wrapped_at_size_font(
                    &txt, st.bold, st.indent, st.wrap, font_size, font,
                );
                let w = chars * 8.0 + 5.0;
                if sc_log {
                    let size = font_size * 4.0 / 3.0;
                    eprintln!(
                        "[autofit]   r{r} txt={:?} len={} chars={:.2} size={:.1} bold={} indent={}",
                        txt.chars().take(12).collect::<String>(),
                        txt.chars().count(),
                        chars,
                        size,
                        st.bold,
                        st.indent
                    );
                }
                widths[idx] = widths[idx].max(w);
            }
            (band_c1..=band_c2)
                .enumerate()
                .map(|(i, col)| {
                    let w = if saw_text[i] {
                        widths[i].clamp(17.0, 1500.0)
                    } else {
                        72.0
                    };
                    (col, w)
                })
                .collect()
        };

        if !sc_log && count >= 2 && sh.len() >= 32_768 {
            let workers = std::thread::available_parallelism()
                .map(|n| n.get().clamp(2, 8))
                .unwrap_or(4)
                .min(count);
            let chunk = count.div_ceil(workers) as u32;
            let mut results: Vec<Vec<(u32, f32)>> = Vec::with_capacity(workers);
            std::thread::scope(|s| {
                let mut handles = Vec::with_capacity(workers);
                let mut cur = c1;
                while cur <= c2 {
                    let end = (cur + chunk - 1).min(c2);
                    handles.push(s.spawn(move || scan_band(cur, end)));
                    cur = end + 1;
                }
                for h in handles {
                    if let Ok(part) = h.join() {
                        results.push(part);
                    }
                }
            });
            results.into_iter().flatten().collect()
        } else {
            scan_band(c1, c2)
        }
    }

    /// 按内容自适应列宽
    pub(super) fn autofit_col(&mut self, _ctx: &egui::Context, col: u32) {
        let width = self
            .compute_autofit_cols(col, col)
            .first()
            .map(|&(_, w)| w)
            .unwrap_or(72.0);
        let msg = format!(
            "{} 列宽已自适应：{:.0}px",
            cell_name(0, col).trim_end_matches('1'),
            width
        );
        self.apply_col_widths_undoable(vec![(col, width)], msg);
    }

    /// 批量计算行区间自适应高度（单遍稀疏扫描 + `(sid, has_non_ascii)` 单行字形高度全量缓存，消除逐格格式化与排版）。
    fn compute_autofit_rows(
        &self,
        ctx: &egui::Context,
        r1: u32,
        r2: u32,
    ) -> Vec<(u32, f32)> {
        let r2 = r2.min(1_048_575);
        if r1 > r2 {
            return Vec::new();
        }
        let mut best = vec![15.0f32; (r2 - r1 + 1) as usize];
        let sh = self.wb.sheet(self.sheet_idx);
        if let Some((ur1, uc1, ur2, uc2)) = sh.used_bounds() {
            let scan_r1 = r1.max(ur1);
            let scan_r2 = r2.min(ur2);
            if scan_r1 <= scan_r2 {
                let sc_log = std::env::var_os("SC_AUTOFIT").is_some();
                let mut single_line_h: HashMap<(u16, bool), f32> = HashMap::new();
                for (key, cell) in sh.iter_rect((scan_r1, uc1, scan_r2, uc2)) {
                    if cell.is_blank() {
                        continue;
                    }
                    let (row, c) = (key.row, key.col);
                    let idx = (row - r1) as usize;
                    let sid = sh.style_of(row, c);
                    let st = self.wb.styles.get(sid);
                    if !st.wrap
                        && st.rotation == 0
                        && !st.vertical_text
                        && !st.format.as_bytes().contains(&b'\n')
                        && !sc_log
                    {
                        let single_line_non_ascii = match &cell.value {
                            Value::Number(n) => n.is_finite().then_some(!st.format.is_ascii()),
                            Value::Bool(_) | Value::Error(_) => Some(!st.format.is_ascii()),
                            Value::Text(s) => (!s.is_empty() && !s.as_bytes().contains(&b'\n'))
                                .then_some(!s.is_ascii() || !st.format.is_ascii()),
                            _ => None,
                        };
                        if let Some(has_non_ascii) = single_line_non_ascii {
                            let line_h = *single_line_h
                                .entry((sid, has_non_ascii))
                                .or_insert_with(|| {
                                    let size =
                                        (if st.size > 0.0 { st.size } else { 11.0 }) * 4.0 / 3.0;
                                    let fname = if st.font.is_empty() { "默认" } else { &st.font };
                                    let font = if st.bold {
                                        let (fam, sz, _) = bold_font(fname, size);
                                        egui::FontId::new(sz, fam)
                                    } else {
                                        egui::FontId::new(size, cell_family(fname))
                                    };
                                    let sample = if has_non_ascii { "国0g" } else { "0g" };
                                    ctx.fonts(|f| {
                                        f.layout_no_wrap(
                                            sample.into(),
                                            font,
                                            egui::Color32::BLACK,
                                        )
                                        .size()
                                        .y
                                            + 1.0
                                    })
                                });
                            best[idx] = best[idx].max(line_h);
                            continue;
                        }
                    }
                    let txt = sheetcore::style::format_value_in(
                        self.wb.date_system,
                        &cell.value,
                        &st.format,
                    );
                    if txt.is_empty() {
                        continue;
                    }
                    let size = (if st.size > 0.0 { st.size } else { 11.0 }) * 4.0 / 3.0;
                    let fname = if st.font.is_empty() { "默认" } else { &st.font };
                    let font = if st.bold {
                        let (fam, sz, _) = bold_font(fname, size);
                        egui::FontId::new(sz, fam)
                    } else {
                        egui::FontId::new(size, cell_family(fname))
                    };
                    let col_w = sh.cols.size(c);
                    let (h, lines) = if st.wrap {
                        ctx.fonts(|f| {
                            let g = f.layout(
                                txt,
                                font,
                                egui::Color32::BLACK,
                                (col_w - 6.0).max(8.0),
                            );
                            (g.size().y, g.rows.len().max(1))
                        })
                    } else {
                        (
                            ctx.fonts(|f| {
                                f.layout_no_wrap(txt, font, egui::Color32::BLACK).size().y
                            }),
                            1,
                        )
                    };
                    if sc_log {
                        eprintln!(
                            "[autofit] 行{row} c{c} galley={h:.1} lines={lines} wrap={}",
                            st.wrap
                        );
                    }
                    best[idx] = best[idx].max(h + lines as f32);
                }
            }
        }
        (r1..=r2)
            .enumerate()
            .map(|(i, row)| (row, best[i].clamp(15.0, 409.0)))
            .collect()
    }

    /// 按内容自适应行高（自动换行时按折行数算，用真实字形量）
    pub(super) fn autofit_row(&mut self, ctx: &egui::Context, row: u32) {
        let h = self
            .compute_autofit_rows(ctx, row, row)
            .first()
            .map(|&(_, h)| h)
            .unwrap_or(15.0);
        let msg = format!("第 {} 行行高已自适应：{:.0}px", row + 1, h);
        self.apply_row_heights_undoable(vec![(row, h)], msg);
    }
}

const AUTOFIT_ASCII_LEN_CAP: usize = 40;

#[derive(Clone, Copy)]
enum AutofitNumericFmt {
    None,
    General,
    DateFixed { max_chars: u8 },
    Numeric {
        decimals: u8,
        min_int_zeros: u8,
        comma: bool,
        percent: bool,
        max_literals: u8,
        has_neg_section: bool,
    },
}

impl AutofitNumericFmt {
    #[inline]
    fn u64_digits(mut v: u64) -> usize {
        if v < 10 {
            return 1;
        }
        let mut d = 0usize;
        while v > 0 {
            v /= 10;
            d += 1;
        }
        d
    }

    #[inline]
    fn max_ascii_chars(self, n: f64) -> Option<usize> {
        if !n.is_finite() {
            return None;
        }
        match self {
            Self::None => None,
            Self::General => {
                if n.fract() == 0.0 && n.abs() < 1e11 {
                    let digits = Self::u64_digits(n.abs() as u64);
                    Some(digits + usize::from(n < 0.0))
                } else {
                    Some(22)
                }
            }
            Self::DateFixed { max_chars } => {
                (0.0..2_958_466.0).contains(&n).then_some(max_chars as usize)
            }
            Self::Numeric {
                decimals,
                min_int_zeros,
                comma,
                percent,
                max_literals,
                has_neg_section,
            } => {
                let scaled = if percent { n.abs() * 100.0 } else { n.abs() };
                if scaled >= 1e15 {
                    return None;
                }
                let int_val = (scaled + 0.5000001).floor() as u64;
                let raw_digits = Self::u64_digits(int_val)
                    .max(min_int_zeros as usize)
                    .max(1);
                let commas = if comma && raw_digits > 3 {
                    (raw_digits - 1) / 3
                } else {
                    0
                };
                let dec_chars = if decimals > 0 {
                    1 + decimals as usize
                } else {
                    0
                };
                let sign_extra = usize::from(n < 0.0 && !has_neg_section);
                Some(raw_digits + commas + dec_chars + max_literals as usize + sign_extra)
            }
        }
    }
}

struct AutofitStyleBound {
    fmt: AutofitNumericFmt,
    ascii_px: [f32; AUTOFIT_ASCII_LEN_CAP],
}

fn classify_autofit_numeric_fmt(fmt: &str) -> AutofitNumericFmt {
    if fmt.is_empty() || fmt.eq_ignore_ascii_case("General") {
        return AutofitNumericFmt::General;
    }
    if fmt.chars().any(|c| (c as u32) >= 0x2e80 || c == '\n') {
        return AutofitNumericFmt::None;
    }
    if fmt
        .bytes()
        .any(|b| matches!(b, b'[' | b'*' | b'?' | b'e' | b'E' | b'@'))
    {
        return AutofitNumericFmt::None;
    }
    let has_date_tok = fmt
        .bytes()
        .any(|b| matches!(b, b'y' | b'Y' | b'm' | b'M' | b'd' | b'D' | b'h' | b'H' | b's' | b'S'));
    let has_num_tok = fmt.bytes().any(|b| matches!(b, b'0' | b'#'));
    if has_date_tok && !has_num_tok && !fmt.contains(';') {
        let bytes = fmt.as_bytes();
        let mut i = 0usize;
        let mut total = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if matches!(b, b'y' | b'Y') {
                let start = i;
                while i < bytes.len() && bytes[i].eq_ignore_ascii_case(&b'y') {
                    i += 1;
                }
                total += if i - start <= 2 { 2 } else { 4 };
            } else if matches!(b, b'm' | b'M' | b'd' | b'D' | b'h' | b'H' | b's' | b'S') {
                let start = i;
                while i < bytes.len() && bytes[i].eq_ignore_ascii_case(&b) {
                    i += 1;
                }
                if i - start >= 3 {
                    return AutofitNumericFmt::None;
                }
                total += 2;
            } else if matches!(b, b'-' | b'/' | b':' | b'.' | b',' | b' ') {
                total += 1;
                i += 1;
            } else {
                return AutofitNumericFmt::None;
            }
        }
        if total > 0 && total < AUTOFIT_ASCII_LEN_CAP {
            return AutofitNumericFmt::DateFixed {
                max_chars: total as u8,
            };
        }
        return AutofitNumericFmt::None;
    }
    if !has_num_tok || fmt.contains('/') {
        return AutofitNumericFmt::None;
    }
    let sections: Vec<&str> = fmt.split(';').collect();
    if sections.is_empty() || sections.len() > 3 {
        return AutofitNumericFmt::None;
    }
    let mut max_decimals = 0u8;
    let mut max_min_zeros = 0u8;
    let mut any_comma = false;
    let mut any_percent = false;
    let mut max_literals = 0u8;
    for &sec in &sections {
        let chars: Vec<char> = sec.chars().collect();
        let mut i = 0usize;
        let mut in_dec = false;
        let mut dec = 0u8;
        let mut min_zeros = 0u8;
        let mut literals = 0u8;
        let mut saw_digit = false;
        let mut trailing_comma = false;
        while i < chars.len() {
            let c = chars[i];
            match c {
                '"' => {
                    i += 1;
                    while i < chars.len() && chars[i] != '"' {
                        literals = literals.saturating_add(1);
                        i += 1;
                    }
                }
                '\\' | '_' => {
                    literals = literals.saturating_add(1);
                    i += 1;
                }
                '0' => {
                    saw_digit = true;
                    trailing_comma = false;
                    if in_dec {
                        dec = dec.saturating_add(1);
                    } else {
                        min_zeros = min_zeros.saturating_add(1);
                    }
                }
                '#' => {
                    saw_digit = true;
                    trailing_comma = false;
                    if in_dec {
                        dec = dec.saturating_add(1);
                    }
                }
                '.' => {
                    if in_dec {
                        return AutofitNumericFmt::None;
                    }
                    in_dec = true;
                    trailing_comma = false;
                }
                ',' => {
                    if saw_digit && !in_dec {
                        any_comma = true;
                        trailing_comma = true;
                    } else {
                        literals = literals.saturating_add(1);
                    }
                }
                '%' => {
                    any_percent = true;
                    literals = literals.saturating_add(1);
                }
                _ => {
                    literals = literals.saturating_add(1);
                }
            }
            i += 1;
        }
        if trailing_comma || dec > 30 {
            return AutofitNumericFmt::None;
        }
        max_decimals = max_decimals.max(dec);
        max_min_zeros = max_min_zeros.max(min_zeros);
        max_literals = max_literals.max(literals);
    }
    AutofitNumericFmt::Numeric {
        decimals: max_decimals,
        min_int_zeros: max_min_zeros,
        comma: any_comma,
        percent: any_percent,
        max_literals,
        has_neg_section: sections.len() >= 2,
    }
}

fn build_autofit_style_bounds(styles: &sheetcore::style::StyleTable) -> Vec<AutofitStyleBound> {
    const ZEROS: &str = "0000000000000000000000000000000000000000";
    let n = styles.len();
    let mut out = Vec::with_capacity(n);
    for sid in 0..n {
        let st = styles.get(sid as u16);
        let font_size = if st.size > 0.0 { st.size } else { 11.0 };
        let font = if st.font.trim().is_empty() {
            "等线"
        } else {
            st.font.as_str()
        };
        let fmt = if st.wrap {
            AutofitNumericFmt::None
        } else {
            classify_autofit_numeric_fmt(&st.format)
        };
        let mut ascii_px = [17.0f32; AUTOFIT_ASCII_LEN_CAP];
        for len in 1..AUTOFIT_ASCII_LEN_CAP {
            let chars = sheetcore::display::excel_autofit_chars_wrapped_at_size_font(
                &ZEROS[..len],
                st.bold,
                st.indent,
                false,
                font_size,
                font,
            );
            ascii_px[len] = chars * 8.0 + 5.0;
        }
        out.push(AutofitStyleBound { fmt, ascii_px });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::SheetApp;
    use crate::test_env::TestEnv;
    use eframe::egui;
    use sheetcore::{Cell, Value};

    #[test]
    fn ribbon_autofit_selected_rows_limits_changes_and_undoes_once() {
        let _env = TestEnv::new("ribbon-autofit-selected-rows");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        let wrap = app.wb.styles.intern(sheetcore::style::CellStyle {
            wrap: true,
            ..Default::default()
        }) as u16;
        let before = [99.0, 130.0, 145.0, 91.0];
        let sheet = app.wb.sheet_mut(0);
        for (row, height) in before.iter().enumerate() {
            sheet.rows.set(row as u32, *height);
        }
        for (row, text) in [(1, "one"), (2, "one\ntwo\nthree")] {
            sheet.set(row, 0, Cell { value: Value::Text(text.into()), formula: None });
        }
        sheet.set_style(2, 0, wrap);
        app.sel = (2, 0);
        app.sel_end = (1, 0);

        let _ = ctx.run(Default::default(), |ctx| app.ribbon_action("AutofitRowHeight", ctx));
        let heights = |app: &SheetApp| [0, 1, 2, 3].map(|row| app.wb.sheet(0).rows.size(row));
        let after = heights(&app);
        assert_eq!((after[0], after[3]), (before[0], before[3]));
        assert!(after[1] < before[1] && after[2] < before[2]);
        assert!(after[2] > after[1]);
        assert_eq!(app.undo.depth(), (1, 0));

        app.ribbon_action("Undo", &ctx);
        assert_eq!(heights(&app), before);
        assert_eq!(app.undo.depth(), (0, 1));
    }
}
