//! 稀疏模型上的统一查找匹配器。
//!
//! 只遍历真实存在的格子（稀疏键），不用 16384×1048576 的稠密步进，
//! 因此 A300、XFD1048576 这类远端格子和普通格子一样能被找到。

use crate::model::{Cell, Workbook};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchOpts {
    /// 区分大小写
    pub match_case: bool,
    /// 整格匹配（而不是包含）
    pub whole_cell: bool,
    /// 在公式文本里找，而不是在显示值里找
    pub in_formula: bool,
}

/// 搜索推进顺序：行优先（默认）或列优先。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchOrder {
    #[default]
    RowMajor,
    ColumnMajor,
}

fn ordered_key(h: &Hit, order: SearchOrder) -> (usize, u32, u32) {
    match order {
        SearchOrder::RowMajor => (h.sheet, h.row, h.col),
        SearchOrder::ColumnMajor => (h.sheet, h.col, h.row),
    }
}

struct PreparedSearch<'a> {
    needle: &'a str,
    opts: &'a SearchOpts,
    ascii_ci: bool,
    uncased_utf8_needle: bool,
}

#[inline]
fn ascii_contains_ci(hay: &[u8], needle_lower: &[u8]) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if hay.len() < needle_lower.len() {
        return false;
    }
    let b0 = needle_lower[0];
    let end = hay.len() - needle_lower.len();
    for i in 0..=end {
        if hay[i].to_ascii_lowercase() == b0
            && hay[i..i + needle_lower.len()].eq_ignore_ascii_case(needle_lower)
        {
            return true;
        }
    }
    false
}

#[inline]
fn is_general_format(fmt: &str) -> bool {
    fmt.is_empty() || fmt.eq_ignore_ascii_case("General")
}

impl<'a> PreparedSearch<'a> {
    fn new(needle: &'a str, opts: &'a SearchOpts) -> Self {
        let ascii_ci = !opts.match_case && needle.is_ascii();
        // Pure CJK ideographs + ASCII digits/symbols cannot be matched via case-folding of any other codepoint.
        let uncased_utf8_needle = !opts.match_case
            && !ascii_ci
            && needle.chars().all(|ch| {
                ('\u{4E00}'..='\u{9FFF}').contains(&ch)
                    || (ch.is_ascii() && !ch.is_ascii_alphabetic())
            });
        Self {
            needle,
            opts,
            ascii_ci,
            uncased_utf8_needle,
        }
    }

    #[inline]
    fn matches_str(&self, text: &str) -> bool {
        if self.needle.is_empty() {
            return false;
        }
        if self.opts.match_case || self.uncased_utf8_needle {
            return if self.opts.whole_cell {
                text == self.needle
            } else {
                text.contains(self.needle)
            };
        }
        if self.ascii_ci && text.is_ascii() {
            return if self.opts.whole_cell {
                text.eq_ignore_ascii_case(self.needle)
            } else {
                ascii_contains_ci(text.as_bytes(), self.needle.as_bytes())
            };
        }
        let hay = text.to_lowercase();
        if self.opts.whole_cell {
            hay == self.needle
        } else {
            hay.contains(self.needle)
        }
    }

    #[inline]
    fn matches_cell_on_sheet(
        &self,
        wb: &Workbook,
        sh: &crate::model::Sheet,
        has_any_styles: bool,
        style0_general: bool,
        row: u32,
        col: u32,
        cell: &Cell,
    ) -> bool {
        if self.opts.in_formula {
            let Some(f) = cell.formula.as_deref() else {
                return false;
            };
            return self.matches_str(f);
        }
        match &cell.value {
            crate::model::Value::Empty => false,
            v => {
                let id = if has_any_styles { sh.style_of(row, col) } else { 0 };
                let general = if id == 0 {
                    style0_general
                } else {
                    is_general_format(&wb.styles.get(id).format)
                };
                if general {
                    match v {
                        crate::model::Value::Text(s) => return self.matches_str(s),
                        crate::model::Value::Bool(b) => {
                            return self.matches_str(if *b { "TRUE" } else { "FALSE" });
                        }
                        crate::model::Value::Error(e) => return self.matches_str(e.text()),
                        crate::model::Value::Number(n)
                            if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 =>
                        {
                            let mut buf = [0u8; 24];
                            let mut pos = buf.len();
                            let iv = *n as i64;
                            let neg = iv < 0;
                            let mut u = iv.unsigned_abs();
                            if u == 0 {
                                pos -= 1;
                                buf[pos] = b'0';
                            } else {
                                while u > 0 {
                                    pos -= 1;
                                    buf[pos] = b'0' + (u % 10) as u8;
                                    u /= 10;
                                }
                            }
                            if neg {
                                pos -= 1;
                                buf[pos] = b'-';
                            }
                            let s = unsafe { std::str::from_utf8_unchecked(&buf[pos..]) };
                            return self.matches_str(s);
                        }
                        _ => {}
                    }
                }
                let fmt = &wb.styles.get(id).format;
                let formatted = crate::style::format_value_in(wb.date_system, v, fmt);
                self.matches_str(&formatted)
            }
        }
    }
}

fn search_sheet_pages_parallel(
    wb: &Workbook,
    si: usize,
    sh: &crate::model::Sheet,
    prep: &PreparedSearch<'_>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Option<Vec<Hit>> {
    let has_any_styles =
        !sh.styles.is_empty() || !sh.row_styles.is_empty() || !sh.col_styles.is_empty();
    let style0_general = is_general_format(&wb.styles.get(0).format);
    let pages: Vec<(u32, &std::sync::Arc<crate::store::Page>)> = sh.cells().iter_pages().collect();
    if pages.is_empty() {
        return Some(Vec::new());
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8);
    if sh.len() < 32_768 || pages.len() < 4 || threads <= 1 {
        let mut out = Vec::new();
        let mut visited = 0usize;
        for (pi, page) in pages {
            if cancelled.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                return None;
            }
            let base = pi * crate::store::PAGE_ROWS;
            for col in page.iter_nonempty_cols(0, u32::MAX) {
                let Some(slab) = page.cols.get(col as usize).and_then(Option::as_deref) else {
                    continue;
                };
                for (r_off, cell) in slab.iter().enumerate() {
                    if cell.is_blank() {
                        continue;
                    }
                    visited += 1;
                    if visited % 512 == 0
                        && cancelled.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                    {
                        return None;
                    }
                    let row = base + r_off as u32;
                    if prep.matches_cell_on_sheet(
                        wb,
                        sh,
                        has_any_styles,
                        style0_general,
                        row,
                        col,
                        cell,
                    ) {
                        out.push(Hit { sheet: si, row, col });
                    }
                }
            }
        }
        return Some(out);
    }

    let chunk_size = pages.len().div_ceil(threads);
    let aborted = std::sync::atomic::AtomicBool::new(false);
    let partial: Vec<Vec<Hit>> = std::thread::scope(|scope| {
        let handles: Vec<_> = pages
            .chunks(chunk_size)
            .map(|chunk| {
                let aborted_ref = &aborted;
                scope.spawn(move || {
                    let mut local = Vec::new();
                    let mut visited = 0usize;
                    for &(pi, page) in chunk {
                        if aborted_ref.load(std::sync::atomic::Ordering::Relaxed)
                            || cancelled
                                .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                        {
                            aborted_ref.store(true, std::sync::atomic::Ordering::Relaxed);
                            return Vec::new();
                        }
                        let base = pi * crate::store::PAGE_ROWS;
                        for col in page.iter_nonempty_cols(0, u32::MAX) {
                            let Some(slab) =
                                page.cols.get(col as usize).and_then(Option::as_deref)
                            else {
                                continue;
                            };
                            for (r_off, cell) in slab.iter().enumerate() {
                                if cell.is_blank() {
                                    continue;
                                }
                                visited += 1;
                                if visited % 512 == 0
                                    && cancelled.is_some_and(|c| {
                                        c.load(std::sync::atomic::Ordering::Relaxed)
                                    })
                                {
                                    aborted_ref.store(true, std::sync::atomic::Ordering::Relaxed);
                                    return Vec::new();
                                }
                                let row = base + r_off as u32;
                                if prep.matches_cell_on_sheet(
                                    wb,
                                    sh,
                                    has_any_styles,
                                    style0_general,
                                    row,
                                    col,
                                    cell,
                                ) {
                                    local.push(Hit { sheet: si, row, col });
                                }
                            }
                        }
                    }
                    local
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    if aborted.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let total: usize = partial.iter().map(Vec::len).sum();
    let mut merged = Vec::with_capacity(total);
    for v in partial {
        merged.extend(v);
    }
    Some(merged)
}

/// A snapshot search can stop when its query or document has been superseded.
/// Overlapping selection rectangles are tested in one pass, so a cell is counted once.
pub fn scoped_hits(
    wb: &Workbook, sheet: Option<usize>, rects: &[(u32, u32, u32, u32)],
    needle: &str, opts: &SearchOpts, order: SearchOrder,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Vec<Hit> {
    if needle.is_empty() {
        return Vec::new();
    }
    let prep = PreparedSearch::new(needle, opts);
    let mut hits = Vec::new();
    for (si, sh) in wb.sheets.iter().enumerate() {
        if sheet.is_some_and(|only| only != si) { continue; }
        if rects.is_empty() {
            let Some(sheet_hits) = search_sheet_pages_parallel(wb, si, sh, &prep, Some(cancelled)) else {
                return Vec::new();
            };
            hits.extend(sheet_hits);
        } else {
            let has_any_styles =
                !sh.styles.is_empty() || !sh.row_styles.is_empty() || !sh.col_styles.is_empty();
            let style0_general = is_general_format(&wb.styles.get(0).format);
            let mut visited = 0usize;
            for (i, &rect) in rects.iter().enumerate() {
                for (k, v) in sh.cells().stored_cells_in(rect) {
                    if rects[..i].iter().any(|&(r1, c1, r2, c2)| r1 <= k.row && k.row <= r2 && c1 <= k.col && k.col <= c2) {
                        continue;
                    }
                    visited += 1;
                    if visited % 256 == 0 && cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                        return Vec::new();
                    }
                    if prep.matches_cell_on_sheet(wb, sh, has_any_styles, style0_general, k.row, k.col, v) {
                        hits.push(Hit { sheet: si, row: k.row, col: k.col });
                    }
                }
            }
        }
    }
    hits.sort_unstable_by_key(|h| ordered_key(h, order));
    hits
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hit {
    pub sheet: usize,
    pub row: u32,
    pub col: u32,
}

/// 参与匹配的文本；空值/没有公式的格子不参与。
/// 值模式必须按网格显示格式取文本：日期、百分比、千分位屏幕上是啥就查啥。
pub fn cell_text_at(
    wb: &Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    cell: &Cell,
    in_formula: bool,
) -> Option<String> {
    if in_formula {
        return cell.formula.as_deref().map(|f| f.to_string());
    }
    match &cell.value {
        crate::model::Value::Empty => None,
        v => {
            let Some(sh) = wb.sheets.get(sheet) else {
                return Some(v.display());
            };
            let id = sh.style_of(row, col);
            let st = wb.styles.get(id);
            Some(crate::style::format_value_in(wb.date_system, v, &st.format))
        }
    }
}

/// 把 needle 按选项归一（大小写）。
pub fn normalize_needle(needle: &str, opts: &SearchOpts) -> String {
    if opts.match_case {
        needle.to_string()
    } else {
        needle.to_lowercase()
    }
}

/// 单格是否命中；needle 必须已经 normalize。
pub fn cell_matches(
    wb: &Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    needle: &str,
    opts: &SearchOpts,
) -> bool {
    if needle.is_empty() {
        return false;
    }
    let Some(cell) = wb.sheets.get(sheet).and_then(|sh| sh.get(row, col)) else {
        return false;
    };
    matches_at(wb, sheet, row, col, cell, needle, opts)
}

fn matches_at(wb: &Workbook, sheet: usize, row: u32, col: u32, cell: &Cell,
              needle: &str, opts: &SearchOpts) -> bool {
    let prep = PreparedSearch::new(needle, opts);
    if let Some(sh) = wb.sheets.get(sheet) {
        let has_any_styles =
            !sh.styles.is_empty() || !sh.row_styles.is_empty() || !sh.col_styles.is_empty();
        let style0_general = is_general_format(&wb.styles.get(0).format);
        return prep.matches_cell_on_sheet(wb, sh, has_any_styles, style0_general, row, col, cell);
    }
    let Some(text) = cell_text_at(wb, sheet, row, col, cell, opts.in_formula) else {
        return false;
    };
    prep.matches_str(&text)
}

/// 一张表内全部命中，按指定顺序排序。
pub fn hits_in_sheet(
    wb: &Workbook,
    sheet: usize,
    needle: &str,
    opts: &SearchOpts,
    order: SearchOrder,
) -> Vec<Hit> {
    if needle.is_empty() {
        return Vec::new();
    }
    let Some(sh) = wb.sheets.get(sheet) else {
        return Vec::new();
    };
    let prep = PreparedSearch::new(needle, opts);
    let mut out = search_sheet_pages_parallel(wb, sheet, sh, &prep, None).unwrap_or_default();
    match order {
        SearchOrder::RowMajor => out.sort_unstable(),
        SearchOrder::ColumnMajor => out.sort_unstable_by_key(|h| (h.sheet, h.col, h.row)),
    }
    out
}

/// 一张表某个矩形内的命中（稀疏页块过滤），按指定顺序排序。
pub fn hits_in_rect(
    wb: &Workbook,
    sheet: usize,
    rect: (u32, u32, u32, u32),
    needle: &str,
    opts: &SearchOpts,
    order: SearchOrder,
) -> Vec<Hit> {
    if needle.is_empty() {
        return Vec::new();
    }
    let Some(sh) = wb.sheets.get(sheet) else {
        return Vec::new();
    };
    let prep = PreparedSearch::new(needle, opts);
    let has_any_styles =
        !sh.styles.is_empty() || !sh.row_styles.is_empty() || !sh.col_styles.is_empty();
    let style0_general = is_general_format(&wb.styles.get(0).format);
    let (r1, c1, r2, c2) = rect;
    let norm_rect = (r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2));
    let mut out: Vec<Hit> = sh
        .cells()
        .stored_cells_in(norm_rect)
        .filter_map(|(k, cell)| {
            prep.matches_cell_on_sheet(wb, sh, has_any_styles, style0_general, k.row, k.col, cell)
                .then_some(Hit {
                    sheet,
                    row: k.row,
                    col: k.col,
                })
        })
        .collect();
    match order {
        SearchOrder::RowMajor => out.sort_unstable(),
        SearchOrder::ColumnMajor => out.sort_unstable_by_key(|h| (h.sheet, h.col, h.row)),
    }
    out
}

/// 工作表（Some）或整个工作簿（None）的命中，按 (表, 行, 列) 升序。
pub fn hits(
    wb: &Workbook,
    sheet: Option<usize>,
    needle: &str,
    opts: &SearchOpts,
    order: SearchOrder,
) -> Vec<Hit> {
    let mut out = Vec::new();
    for si in 0..wb.sheets.len() {
        if sheet.is_some_and(|only| only != si) {
            continue;
        }
        out.extend(hits_in_sheet(wb, si, needle, opts, order));
    }
    out
}

/// 循环取下一处；`cur` 为当前活动格。forward=false 时反向。
pub fn next_hit(
    hits: &[Hit],
    cur: Hit,
    forward: bool,
    order: SearchOrder,
) -> Option<(Hit, bool)> {
    next_index(hits, cur, forward, order).map(|(i, wrapped)| (hits[i], wrapped))
}

pub fn next_index(hits: &[Hit], cur: Hit, forward: bool, order: SearchOrder) -> Option<(usize, bool)> {
    if hits.is_empty() {
        return None;
    }
    let cur_key = ordered_key(&cur, order);
    if forward {
        let i = hits.partition_point(|h| ordered_key(h, order) <= cur_key);
        Some(if i == hits.len() { (0, true) } else { (i, false) })
    } else {
        let i = hits.partition_point(|h| ordered_key(h, order) < cur_key);
        Some(if i == 0 { (hits.len() - 1, true) } else { (i - 1, false) })
    }
}

pub fn position(hits: &[Hit], hit: Hit, order: SearchOrder) -> Option<usize> {
    hits.binary_search_by_key(&ordered_key(&hit, order), |h| ordered_key(h, order)).ok()
}

/// 小写文本 + 每个小写字节位置对应的原串字节位置。
/// Unicode 小写化会改变字节长度（`İ` → `i` + 组合点），直接拿小写索引切原串会 panic。
fn lower_with_map(s: &str) -> (String, Vec<usize>) {
    let mut lower = String::with_capacity(s.len());
    let mut map: Vec<usize> = Vec::with_capacity(s.len() + 1);
    for (idx, ch) in s.char_indices() {
        map.resize(lower.len(), idx);
        let folded: String = ch.to_lowercase().collect();
        for _ in 0..folded.len() {
            map.push(idx);
        }
        lower.push_str(&folded);
    }
    map.resize(lower.len() + 1, s.len());
    (lower, map)
}

/// 大小写不敏感替换：按原串边界切，不按小写串索引切。
pub fn replace_in_text(text: &str, needle: &str, with: &str, match_case: bool) -> String {
    if needle.is_empty() {
        return text.to_string();
    }
    if match_case {
        return text.replace(needle, with);
    }
    let (hay, map) = lower_with_map(text);
    let pat = needle.to_lowercase();
    if pat.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0usize; // hay 中的搜索起点（字节）
    let mut copied = 0usize; // text 中已复制的字节
    while let Some(found) = hay[pos..].find(&pat) {
        let at = pos + found;
        let end = at + pat.len();
        let (Some(&start_orig), Some(&end_orig)) = (map.get(at), map.get(end)) else {
            break;
        };
        if start_orig < copied || end_orig < start_orig || end_orig > text.len() {
            break;
        }
        out.push_str(&text[copied..start_orig]);
        out.push_str(with);
        copied = end_orig;
        pos = end;
        if pos >= hay.len() {
            break;
        }
    }
    out.push_str(&text[copied..]);
    out
}
