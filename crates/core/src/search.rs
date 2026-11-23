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

/// A snapshot search can stop when its query or document has been superseded.
/// Overlapping selection rectangles are tested in one pass, so a cell is counted once.
pub fn scoped_hits(
    wb: &Workbook, sheet: Option<usize>, rects: &[(u32, u32, u32, u32)],
    needle: &str, opts: &SearchOpts, order: SearchOrder,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (si, sh) in wb.sheets.iter().enumerate() {
        if sheet.is_some_and(|only| only != si) { continue; }
        let mut visited = 0usize;
        let mut check = |key: crate::CellRef, cell: &Cell| {
            visited += 1;
            if visited % 256 == 0 && cancelled.load(std::sync::atomic::Ordering::Relaxed) { return false; }
            if matches_at(wb, si, key.row, key.col, cell, needle, opts) {
                hits.push(Hit { sheet: si, row: key.row, col: key.col });
            }
            true
        };
        if rects.is_empty() {
            if !sh.stored_cells().all(|(k,v)| check(k,v)) { return Vec::new(); }
        } else {
            for (i, &rect) in rects.iter().enumerate() {
                for (k,v) in sh.cells().stored_cells_in(rect) {
                    if !rects[..i].iter().any(|&(r1,c1,r2,c2)| r1<=k.row && k.row<=r2 && c1<=k.col && k.col<=c2)
                        && !check(k,v) { return Vec::new(); }
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
    let Some(text) = cell_text_at(wb, sheet, row, col, cell, opts.in_formula) else {
        return false;
    };
    let hay = if opts.match_case {
        text
    } else {
        text.to_lowercase()
    };
    if opts.whole_cell {
        hay == needle
    } else {
        hay.contains(needle)
    }
}

/// 一张表内全部命中，按指定顺序排序。
pub fn hits_in_sheet(
    wb: &Workbook,
    sheet: usize,
    needle: &str,
    opts: &SearchOpts,
    order: SearchOrder,
) -> Vec<Hit> {
    hits_in_rect(
        wb,
        sheet,
        (0, 0, u32::MAX - 1, u32::MAX - 1),
        needle,
        opts,
        order,
    )
}

/// 一张表某个矩形内的命中（稀疏过滤），按指定顺序排序。
pub fn hits_in_rect(
    wb: &Workbook,
    sheet: usize,
    rect: (u32, u32, u32, u32),
    needle: &str,
    opts: &SearchOpts,
    order: SearchOrder,
) -> Vec<Hit> {
    let Some(sh) = wb.sheets.get(sheet) else {
        return Vec::new();
    };
    let (r1, c1, r2, c2) = rect;
    let mut out: Vec<Hit> = sh
        .iter()
        .filter(|(k, _)| {
            k.row >= r1.min(r2)
                && k.row <= r1.max(r2)
                && k.col >= c1.min(c2)
                && k.col <= c1.max(c2)
                && cell_matches(wb, sheet, k.row, k.col, needle, opts)
        })
        .map(|(k, _)| Hit {
            sheet,
            row: k.row,
            col: k.col,
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
