//! 插入 / 删除行列：整行整列搬移，并同步调整合并、批注、条件格式、数据验证、冻结、筛选。
//!
//! 分页列式存储仅搬移插删位置之后的格子，未受影响的列页仍可与撤销快照共享。
//!
//! 工作簿级入口同时改写本表、跨表公式和名称引用。

use crate::model::{Cell, CellRef, Sheet};
use crate::refs::MappedShift;
use std::collections::HashSet;

/// 结构性改动
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Structural {
    InsertRows { at: u32, n: u32 },
    DeleteRows { at: u32, n: u32 },
    InsertCols { at: u32, n: u32 },
    DeleteCols { at: u32, n: u32 },
}

impl Structural {
    pub(crate) fn reference_shift(&self) -> crate::refs::Shift {
        use crate::refs::Axis::{Row, Col};
        let (axis, at, n, delete) = match *self {
            Self::InsertRows { at, n } => (Row, at, n, false),
            Self::DeleteRows { at, n } => (Row, at, n, true),
            Self::InsertCols { at, n } => (Col, at, n, false),
            Self::DeleteCols { at, n } => (Col, at, n, true),
        };
        crate::refs::Shift { axis, at, n, delete, ..Default::default() }
    }

    pub fn label(&self) -> String {
        match self {
            Structural::InsertRows { at, n } => format!("插入 {n} 行（第 {} 行）", at + 1),
            Structural::DeleteRows { at, n } => format!("删除 {n} 行（第 {} 行起）", at + 1),
            Structural::InsertCols { at, n } => format!("插入 {n} 列（第 {} 列）", crate::model::col_label(*at)),
            Structural::DeleteCols { at, n } => format!("删除 {n} 列（第 {} 列起）", crate::model::col_label(*at)),
        }
    }
}

fn shift_axis(v: u32, at: u32, n: u32, del: bool) -> Option<u32> {
    if del {
        if v < at {
            Some(v)
        } else if v < at + n {
            None // 被删掉
        } else {
            Some(v - n)
        }
    } else if v >= at {
        Some(v + n)
    } else {
        Some(v)
    }
}

/// 行插入到表格数据区时，从相邻数据行复制计算列公式。
struct TableRowFill {
    row: u32,
    col: u32,
    formula: std::sync::Arc<str>,
    style: Option<u16>,
}

/// Conservative O(formula.len()) pre-filter: returns `false` ONLY when it is guaranteed
/// that `crate::refs::rewrite_mapped(formula, shift)` will return `formula` verbatim.
/// Any lowercase letters, leading zeros, reversed ranges, `translate`, or coordinates `>= min_at`
/// conservatively return `true` so no reference (including cross-sheet, whole-row/col, `$`) is missed.
fn formula_may_be_affected(formula: &str, shift: &MappedShift) -> bool {
    if shift.translate.is_some() {
        return true;
    }
    let min_at = if shift.insertions.is_empty() {
        shift.at
    } else {
        shift.insertions.iter().copied().min().unwrap_or(shift.at)
    };
    let b = formula.as_bytes();
    match shift.axis {
        crate::refs::Axis::Row => {
            let threshold = min_at as u64 + 1;
            let mut i = 0usize;
            let mut saw_colon = false;
            let mut prev_num: Option<u64> = None;
            while i < b.len() {
                let ch = b[i];
                if ch.is_ascii_lowercase() {
                    return true;
                }
                if ch == b':' {
                    saw_colon = true;
                    i += 1;
                    continue;
                }
                if ch.is_ascii_digit() {
                    let start = i;
                    let mut val = 0u64;
                    while i < b.len() && b[i].is_ascii_digit() {
                        val = val.saturating_mul(10).saturating_add((b[i] - b'0') as u64);
                        i += 1;
                    }
                    if i - start > 1 && b[start] == b'0' {
                        return true;
                    }
                    if val >= threshold {
                        return true;
                    }
                    if saw_colon {
                        if prev_num.is_some_and(|p| p > val) {
                            return true;
                        }
                        saw_colon = false;
                    }
                    prev_num = Some(val);
                    continue;
                }
                i += 1;
            }
            false
        }
        crate::refs::Axis::Col => {
            let mut i = 0usize;
            let mut saw_colon = false;
            let mut prev_col: Option<u32> = None;
            while i < b.len() {
                let ch = b[i];
                if ch.is_ascii_lowercase() {
                    return true;
                }
                if ch == b':' {
                    saw_colon = true;
                    i += 1;
                    continue;
                }
                if ch.is_ascii_digit() {
                    let start = i;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    if i - start > 1 && b[start] == b'0' {
                        return true;
                    }
                    continue;
                }
                if ch.is_ascii_uppercase() {
                    let start = i;
                    let mut col = 0u32;
                    while i < b.len() && b[i].is_ascii_uppercase() {
                        col = col
                            .saturating_mul(26)
                            .saturating_add((b[i] - b'A') as u32 + 1);
                        i += 1;
                    }
                    let len = i - start;
                    if len <= 3 && col > 0 && col <= 16_384 && b.get(i) != Some(&b'(') {
                        let c0 = col - 1;
                        if c0 >= min_at {
                            return true;
                        }
                        if saw_colon {
                            if prev_col.is_some_and(|p| p > c0) {
                                return true;
                            }
                            saw_colon = false;
                        }
                        prev_col = Some(c0);
                    }
                    continue;
                }
                i += 1;
            }
            false
        }
    }
}

impl Sheet {
    /// 把所有公式里的引用按 shift 改写（含条件格式、数据验证里的公式）
    pub fn rewrite_references(&mut self, shift: &crate::refs::Shift) {
        self.rewrite_mapped_references(&MappedShift { shift, insertions: &[] });
    }

    fn rewrite_mapped_references(&mut self, shift: &MappedShift) {
        let foreign_sheet = (shift.translate.is_none()
            && !shift.own_sheet.eq_ignore_ascii_case(&shift.target_sheet))
        .then_some(shift.target_sheet.as_str());
        let changed = self
            .cells_mut()
            .rewrite_formulas_parallel(foreign_sheet, |old| {
                if !formula_may_be_affected(old, shift) {
                    return None;
                }
                let new = crate::refs::rewrite_mapped(old, shift);
                (new != old).then(|| new.into())
            });
        self.edited |= changed;
        for blk in self.cf.blocks.iter_mut() {
            for rule in blk.rules.iter_mut() {
                let mut rewrite_threshold = |formula: &mut String| {
                    let rewritten = crate::refs::rewrite_mapped(formula, shift);
                    if *formula != rewritten {
                        *formula = rewritten;
                        self.cf_dirty = true;
                    }
                };
                match &mut rule.kind {
                    crate::cf::CfKind::Expr { formula, .. } => rewrite_threshold(formula),
                    crate::cf::CfKind::CellIs { a, b, .. } => {
                        rewrite_threshold(a);
                        rewrite_threshold(b);
                    }
                    _ => {}
                }
            }
        }
        for dv in self.dv.iter_mut() {
            let (f1, f2) = (crate::refs::rewrite_mapped(&dv.f1, shift), crate::refs::rewrite_mapped(&dv.f2, shift));
            self.dv_dirty |= dv.f1 != f1 || dv.f2 != f2;
            (dv.f1, dv.f2) = (f1, f2);
        }
    }

    /// 只搬结构（合并/条件格式/数据验证/冻结/行列尺寸），不动格子。
    /// 加入房间时用：格子由宿主的快照给出（已经是新坐标），
    /// 再按历史搬一次就会搬两遍。
    pub fn apply_structural_structure_only(&mut self, op: &Structural) {
        let shift = op.reference_shift();
        let shift = MappedShift { shift: &shift, insertions: &[] };
        self.shift_array_and_hyperlink_ranges(&shift);
        match op {
            Structural::InsertRows { at, n } => {
                self.merges = self
                    .merges
                    .iter()
                    .map(|(r1, c1, r2, c2)| {
                        (
                            if *r1 >= *at { r1 + n } else { *r1 },
                            *c1,
                            if *r2 >= *at { r2 + n } else { *r2 },
                            *c2,
                        )
                    })
                    .collect();
                self.shift_notes(&shift);
                self.shift_cf(&shift);
                self.shift_dv(&shift);
                self.layout_dirty |= self.rows.shift_axis(*at, *n, false);
                self.shift_axis_metadata(&shift);
            }
            Structural::DeleteRows { at, n } => {
                self.merges = self
                    .merges
                    .iter()
                    .filter_map(|(r1, c1, r2, c2)| {
                        let a = shift_axis(*r1, *at, *n, true)?;
                        let b = shift_axis(*r2, *at, *n, true)?;
                        Some((a, *c1, b.max(a), *c2))
                    })
                    .collect();
                self.shift_notes(&shift);
                self.shift_cf(&shift);
                self.shift_dv(&shift);
                self.layout_dirty |= self.rows.shift_axis(*at, *n, true);
                self.shift_axis_metadata(&shift);
            }
            Structural::InsertCols { at, n } => {
                self.merges = self
                    .merges
                    .iter()
                    .map(|(r1, c1, r2, c2)| {
                        (
                            *r1,
                            if *c1 >= *at { c1 + n } else { *c1 },
                            *r2,
                            if *c2 >= *at { c2 + n } else { *c2 },
                        )
                    })
                    .collect();
                self.shift_notes(&shift);
                self.shift_cf(&shift);
                self.shift_dv(&shift);
                self.layout_dirty |= self.cols.shift_axis(*at, *n, false);
                self.shift_axis_metadata(&shift);
            }
            Structural::DeleteCols { at, n } => {
                self.merges = self
                    .merges
                    .iter()
                    .filter_map(|(r1, c1, r2, c2)| {
                        let a = shift_axis(*c1, *at, *n, true)?;
                        let b = shift_axis(*c2, *at, *n, true)?;
                        Some((*r1, a, *r2, b.max(a)))
                    })
                    .collect();
                self.shift_notes(&shift);
                self.shift_cf(&shift);
                self.shift_dv(&shift);
                self.layout_dirty |= self.cols.shift_axis(*at, *n, true);
                self.shift_axis_metadata(&shift);
            }
        }
        self.merges_dirty = true;
    }

    /// 应用一次结构性改动
    pub fn apply_structural(&mut self, op: &Structural) {
        match op {
            Structural::InsertRows { at, n } => self.insert_rows(*at, *n),
            Structural::DeleteRows { at, n } => self.delete_rows(*at, *n),
            Structural::InsertCols { at, n } => self.insert_cols(*at, *n),
            Structural::DeleteCols { at, n } => self.delete_cols(*at, *n),
        }
    }

    /// 数组元数据与单元格使用同一坐标变换。旧溢出尾格仍需登记，
    /// 后续重算才能清除它们，而不会把它们误当成用户输入的阻塞项。
    fn shift_array_and_hyperlink_ranges(&mut self, shift: &MappedShift) {
        if shift.n != 0 { self.coordinates_shifted(); }
        let rows = shift.axis == crate::refs::Axis::Row;
        if shift.n == 0 { return; }
        let min_at = if shift.insertions.is_empty() {
            shift.at
        } else {
            shift.insertions.first().copied().unwrap_or(shift.at)
        };
        if !self.arrays.is_empty() {
            // 刚从 OOXML 读入、尚未重算时，CSE 尾格还没有 spilled 标记。
            // 先登记旧范围；即使本次删掉了锚点，剩余的缓存尾格也能被清理。
            for &(anchor, (r1, c1, r2, c2)) in &self.arrays {
                let end_coord = if rows { r2 } else { c2 };
                if end_coord < min_at {
                    continue;
                }
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        let key = CellRef::new(r, c);
                        if key != anchor && self.get(r, c).is_none_or(|cell| cell.formula.is_none()) {
                            self.spilled.insert(key);
                        }
                    }
                }
            }
        }
        if !self.spilled.is_empty()
            && self
                .spilled
                .iter()
                .any(|k| (if rows { k.row } else { k.col }) >= min_at)
        {
            self.spilled = self
                .spilled
                .iter()
                .filter_map(|key| {
                    let (r, c) = if rows {
                        (shift.map(key.row)?, key.col)
                    } else {
                        (key.row, shift.map(key.col)?)
                    };
                    Some(CellRef::new(r, c))
                })
                .collect();
        }
        if !self.spill_shapes.is_empty()
            && self.spill_shapes.iter().any(|(k, &(h, w))| {
                let end = if rows {
                    k.row.saturating_add(h.saturating_sub(1))
                } else {
                    k.col.saturating_add(w.saturating_sub(1))
                };
                end >= min_at
            })
        {
            let mut next_shapes = crate::model::CellMetadata::default();
            let mut orphan_tails: Vec<(u32, u32, u32, u32)> = Vec::new();
            for (&anchor, &(h, w)) in &self.spill_shapes {
                let mapped_anchor = if rows {
                    shift.map(anchor.row).map(|r| CellRef::new(r, anchor.col))
                } else {
                    shift.map(anchor.col).map(|c| CellRef::new(anchor.row, c))
                };
                let Some(new_anchor) = mapped_anchor else {
                    if h > 0 && w > 0 {
                        let (r1, c1) = (anchor.row, anchor.col);
                        let (r2, c2) = (
                            r1.saturating_add(h.saturating_sub(1)),
                            c1.saturating_add(w.saturating_sub(1)),
                        );
                        if rows {
                            if let Some((nr1, nr2)) = shift.map_range(r1, r2) {
                                orphan_tails.push((nr1, c1, nr2, c2));
                            }
                        } else if let Some((nc1, nc2)) = shift.map_range(c1, c2) {
                            orphan_tails.push((r1, nc1, r2, nc2));
                        }
                    }
                    continue;
                };
                if h == 0 || w == 0 {
                    next_shapes.insert(new_anchor, (0, 0));
                    continue;
                }
                let span_intact = if rows {
                    let end = anchor.row.saturating_add(h.saturating_sub(1));
                    shift
                        .map_range(anchor.row, end)
                        .is_some_and(|(nr1, nr2)| nr1 == new_anchor.row && nr2 - nr1 + 1 == h)
                } else {
                    let end = anchor.col.saturating_add(w.saturating_sub(1));
                    shift
                        .map_range(anchor.col, end)
                        .is_some_and(|(nc1, nc2)| nc1 == new_anchor.col && nc2 - nc1 + 1 == w)
                };
                if span_intact {
                    next_shapes.insert(new_anchor, (h, w));
                } else {
                    // Spill interior was split or truncated; clear surviving tails and mark for recalc.
                    let (r1, c1) = (anchor.row, anchor.col);
                    let (r2, c2) = (
                        r1.saturating_add(h.saturating_sub(1)),
                        c1.saturating_add(w.saturating_sub(1)),
                    );
                    if rows {
                        if let Some((nr1, nr2)) = shift.map_range(r1, r2) {
                            orphan_tails.push((nr1, c1, nr2, c2));
                        }
                    } else if let Some((nc1, nc2)) = shift.map_range(c1, c2) {
                        orphan_tails.push((r1, nc1, r2, nc2));
                    }
                    next_shapes.insert(new_anchor, (0, 0));
                }
            }
            self.spill_shapes = next_shapes;
            for (r1, c1, r2, c2) in orphan_tails {
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        let k = CellRef::new(r, c);
                        if self.spilled.remove_if_present(&k) {
                            self.remove(r, c);
                        }
                    }
                }
            }
        }
        if !self.hyperlinks.is_empty()
            && self.hyperlinks.iter().any(|(&(r, c), link)| {
                let end = if rows {
                    r.saturating_add(link.span.0)
                } else {
                    c.saturating_add(link.span.1)
                };
                end >= min_at
            })
        {
            self.hyperlinks = std::mem::take(&mut self.hyperlinks)
                .into_iter()
                .filter_map(|((r, c), mut link)| {
                    let (start, span) = if rows { (r, link.span.0) } else { (c, link.span.1) };
                    let end = start.saturating_add(span);
                    let moved = shift.map_range(start, end);
                    self.hyperlinks_dirty |= moved != Some((start, end));
                    let (first, last) = moved?;
                    let anchor = if rows {
                        link.span.0 = last - first;
                        (first, c)
                    } else {
                        link.span.1 = last - first;
                        (r, first)
                    };
                    Some((anchor, link))
                })
                .collect::<std::collections::HashMap<_, _>>()
                .into();
        }
        if !self.arrays.is_empty() {
            self.arrays = self
                .arrays
                .iter()
                .filter_map(|&(anchor, (r1, c1, r2, c2))| {
                    if rows {
                        let row = shift.map(anchor.row)?;
                        let (r1, r2) = shift.map_range(r1, r2)?;
                        Some((CellRef::new(row, anchor.col), (r1, c1, r2, c2)))
                    } else {
                        let col = shift.map(anchor.col)?;
                        let (c1, c2) = shift.map_range(c1, c2)?;
                        Some((CellRef::new(anchor.row, col), (r1, c1, r2, c2)))
                    }
                })
                .collect();
        }
    }

    fn shift_cells(&mut self, rows: bool, at: u32, n: u32, delete: bool) {
        self.remap_cell_axis(rows, at, |pos| shift_axis(pos, at, n, delete));
    }

    /// 把 [from, from+n) 这段整行移动到 `to`（`to` 是「删掉源行之后」的插入位置，
    /// 也就是拖放时画出的那条插入线的位置）。
    pub fn move_rows(&mut self, from: u32, n: u32, to: u32) {
        if n == 0 || to == from {
            return;
        }
        let at = if to > from { to - n } else { to };
        if at == from {
            return;
        }
        let lo = from.min(at);
        let hi = (from + n).max(at + n);
        let r2 = from + n - 1;
        let perm = |r: u32| -> u32 {
            if r < lo || r >= hi {
                r
            } else if r >= from && r < from + n {
                at + (r - from)
            } else if at > from {
                r - n
            } else {
                r + n
            }
        };

        let notes: Vec<_> = self
            .notes
            .iter()
            .filter(|(k, _)| k.row >= from && k.row <= r2)
            .map(|(k, v)| (k.row - from, k.col, v.clone()))
            .collect();
        let links: Vec<_> = self
            .hyperlinks
            .iter()
            .filter(|((r, _), _)| *r >= from && *r <= r2)
            .map(|((r, c), v)| (r - from, *c, v.clone()))
            .collect();
        let heights: Vec<f32> = (from..=r2).map(|r| self.rows.size(r)).collect();
        let hidden: Vec<bool> = (from..=r2)
            .map(|r| self.hidden_rows_user.contains(&r) || self.hidden_rows.contains(&r))
            .collect();
        let levels: Vec<u8> = (from..=r2)
            .map(|r| self.row_levels.get(&r).copied().unwrap_or(0))
            .collect();
        let row_styles: Vec<u16> = (from..=r2)
            .map(|r| self.row_styles.get(&r).copied().unwrap_or(0))
            .collect();

        if !self.rich.is_empty() {
            let rich_keys: Vec<(u32, u32)> = self
                .rich
                .keys()
                .filter(|&&(r, _)| r >= lo && r < hi)
                .copied()
                .collect();
            if !rich_keys.is_empty() {
                self.rich_dirty = true;
                let mut moved_rich = Vec::with_capacity(rich_keys.len());
                for pos in rich_keys {
                    if let Some(v) = self.rich.remove(&pos) {
                        moved_rich.push(((perm(pos.0), pos.1), v));
                    }
                }
                for (dest, v) in moved_rich {
                    self.rich.insert(dest, v);
                }
            }
        }

        // Single-pass bounded `[lo..hi)` permutation on cells and explicit styles:
        // pages `< lo / PAGE_ROWS` and `> (hi - 1) / PAGE_ROWS` are 100% untouched.
        self.permute_cell_band(true, lo, hi, perm);

        self.delete_rows_inner(from, n, false);
        self.insert_rows_inner(at, n, false);

        for (dr, c, mut v) in notes {
            v.r = at + dr;
            v.c = c;
            self.notes.insert(CellRef::new(at + dr, c), v);
            self.notes_dirty = true;
        }
        for (dr, c, v) in links {
            self.hyperlinks.insert((at + dr, c), v);
            self.hyperlinks_dirty = true;
        }
        self.rows.set_many(heights.into_iter().enumerate().map(|(i, h)| (at + i as u32, h)));
        for (i, hid) in hidden.into_iter().enumerate() {
            let i = i as u32;
            if hid {
                self.hidden_rows_user.insert(at + i);
            }
        }
        for (i, lv) in levels.into_iter().enumerate() {
            let i = i as u32;
            if lv > 0 {
                self.row_levels.insert(at + i, lv);
            }
        }
        for (i, st) in row_styles.into_iter().enumerate() {
            if st != 0 {
                self.set_row_style(at + i as u32, st);
            }
        }
        self.layout_dirty = true;
        self.edited = true;
    }

    /// 列版本的移动，语义同 `move_rows`
    pub fn move_cols(&mut self, from: u32, n: u32, to: u32) {
        if n == 0 || to == from {
            return;
        }
        let at = if to > from { to - n } else { to };
        if at == from {
            return;
        }
        let lo = from.min(at);
        let hi = (from + n).max(at + n);
        let c2 = from + n - 1;
        let perm = |c: u32| -> u32 {
            if c < lo || c >= hi {
                c
            } else if c >= from && c < from + n {
                at + (c - from)
            } else if at > from {
                c - n
            } else {
                c + n
            }
        };

        let notes: Vec<_> = self
            .notes
            .iter()
            .filter(|(k, _)| k.col >= from && k.col <= c2)
            .map(|(k, v)| (k.row, k.col - from, v.clone()))
            .collect();
        let links: Vec<_> = self
            .hyperlinks
            .iter()
            .filter(|((_, c), _)| *c >= from && *c <= c2)
            .map(|((r, c), v)| (*r, c - from, v.clone()))
            .collect();
        let widths: Vec<f32> = (from..=c2).map(|c| self.cols.size(c)).collect();
        let hidden: Vec<bool> = (from..=c2).map(|c| self.hidden_cols.contains(&c)).collect();
        let levels: Vec<u8> = (from..=c2)
            .map(|c| self.col_levels.get(&c).copied().unwrap_or(0))
            .collect();
        let col_styles: Vec<u16> = (from..=c2)
            .map(|c| self.col_styles.get(&c).copied().unwrap_or(0))
            .collect();

        if !self.rich.is_empty() {
            let rich_keys: Vec<(u32, u32)> = self
                .rich
                .keys()
                .filter(|&&(_, c)| c >= lo && c < hi)
                .copied()
                .collect();
            if !rich_keys.is_empty() {
                self.rich_dirty = true;
                let mut moved_rich = Vec::with_capacity(rich_keys.len());
                for pos in rich_keys {
                    if let Some(v) = self.rich.remove(&pos) {
                        moved_rich.push(((pos.0, perm(pos.1)), v));
                    }
                }
                for (dest, v) in moved_rich {
                    self.rich.insert(dest, v);
                }
            }
        }

        self.permute_cell_band(false, lo, hi, perm);

        self.delete_cols_inner(from, n, false);
        self.insert_cols_inner(at, n, false);

        for (r, dc, mut v) in notes {
            v.r = r;
            v.c = at + dc;
            self.notes.insert(CellRef::new(r, at + dc), v);
            self.notes_dirty = true;
        }
        for (r, dc, v) in links {
            self.hyperlinks.insert((r, at + dc), v);
            self.hyperlinks_dirty = true;
        }
        self.cols.set_many(widths.into_iter().enumerate().map(|(i, w)| (at + i as u32, w)));
        for (i, hid) in hidden.into_iter().enumerate() {
            let i = i as u32;
            if hid {
                self.hidden_cols.insert(at + i);
            }
        }
        for (i, lv) in levels.into_iter().enumerate() {
            let i = i as u32;
            if lv > 0 {
                self.col_levels.insert(at + i, lv);
            }
        }
        for (i, st) in col_styles.into_iter().enumerate() {
            if st != 0 {
                self.set_col_style(at + i as u32, st);
            }
        }
        self.layout_dirty = true;
        self.edited = true;
    }

    /// 插入行前抓取表格计算列公式。插入点落在表体内（或表尾下一行）时，
    /// Excel 会扩展表格并把上一数据行的计算列公式填到新行。
    fn capture_table_row_fills(&self, at: u32, n: u32, later_insertions: &[u32]) -> Vec<TableRowFill> {
        let mut out = Vec::new();
        for t in &self.tables {
            let (r1, c1, r2, c2) = t.range;
            let data_start = if t.header_row { r1 + 1 } else { r1 };
            let data_end = if t.totals_row { r2.saturating_sub(1) } else { r2 };
            let max_insert = if t.totals_row { r2 } else { r2 + 1 };
            if at <= r1 || at > max_insert || data_start > data_end {
                continue;
            }
            let source = if at > data_start { at - 1 } else { data_start };
            if source < data_start || source > data_end {
                continue;
            }
            for col in c1..=c2 {
                let Some(src) = self.get(source, col) else { continue };
                let Some(formula) = src.formula.as_deref() else { continue };
                // In descending insertion order, the source formula has already
                // been rewritten for all group boundaries below this one.
                let rewritten;
                let formula = if later_insertions.is_empty() {
                    formula
                } else {
                    let shift = crate::refs::Shift {
                        own_sheet: self.name.clone(), target_sheet: self.name.clone(), ..Default::default()
                    };
                    rewritten = crate::refs::rewrite_mapped(formula, &MappedShift { shift: &shift, insertions: later_insertions });
                    &rewritten
                };
                let style = self.styles.get(&CellRef::new(source, col)).copied();
                for k in 0..n {
                    let row = at + k;
                    let delta = row as i64 - source as i64;
                    let shift = crate::refs::Shift {
                        axis: crate::refs::Axis::Row,
                        translate: Some((delta, 0)),
                        target_sheet: self.name.clone(),
                        own_sheet: self.name.clone(),
                        ..Default::default()
                    };
                    out.push(TableRowFill {
                        row,
                        col,
                        formula: crate::refs::rewrite(formula, &shift).into(),
                        style,
                    });
                }
            }
        }
        out
    }

    fn apply_table_row_fills(&mut self, fills: Vec<TableRowFill>) {
        if fills.is_empty() {
            return;
        }
        for f in fills {
            self.set(
                f.row,
                f.col,
                Cell { value: crate::model::Value::Empty, formula: Some(f.formula) },
            );
            if let Some(style) = f.style {
                self.set_style(f.row, f.col, style);
            }
        }
        self.tables_dirty = true;
    }

    fn adjust_tables_for_row_insert(&mut self, at: u32, n: u32) {
        let mut changed = false;
        for t in &mut self.tables {
            let (r1, _, r2, _) = t.range;
            if at <= r1 {
                t.range.0 += n;
                t.range.2 += n;
                changed = true;
            } else if at <= r2 + u32::from(!t.totals_row) {
                t.range.2 += n;
                changed = true;
            }
        }
        if changed {
            self.tables_dirty = true;
        }
    }

    pub fn insert_rows(&mut self, at: u32, n: u32) {
        self.insert_rows_inner(at, n, true);
    }

    fn insert_rows_inner(&mut self, at: u32, n: u32, move_cells: bool) {
        if n == 0 {
            return;
        }
        let fills = self.capture_table_row_fills(at, n, &[]);
        let shift = (Structural::InsertRows { at, n }).reference_shift();
        self.insert_mapped_rows_inner(&MappedShift { shift: &shift, insertions: &[] }, move_cells);
        self.adjust_tables_for_row_insert(at, n);
        if move_cells {
            self.apply_table_row_fills(fills);
        } else if !fills.is_empty() {
            let surviving: Vec<_> = fills
                .into_iter()
                .filter(|f| !self.contains(f.row, f.col))
                .collect();
            self.apply_table_row_fills(surviving);
        }
    }

    fn insert_mapped_rows(&mut self, shift: &MappedShift) {
        self.insert_mapped_rows_inner(shift, true);
    }

    fn insert_mapped_rows_inner(&mut self, shift: &MappedShift, move_cells: bool) {
        self.shift_array_and_hyperlink_ranges(shift);
        if move_cells {
            self.remap_cell_axis(true, shift.at, |row| shift.map(row));
        }
        for (r1, _, r2, _) in &mut self.merges {
            *r1 = shift.map(*r1).unwrap();
            *r2 = shift.map(*r2).unwrap();
        }
        self.shift_notes(shift);
        self.shift_cf(shift);
        self.shift_dv(shift);
        self.shift_sparklines(shift);
        if let Some((r, c)) = self.freeze { self.freeze = Some((shift.map(r).unwrap(), c)); }
        self.layout_dirty |= if shift.insertions.is_empty() {
            self.rows.shift_axis(shift.at, shift.n, shift.delete)
        } else {
            self.rows.remap_indices(|r| shift.map(r))
        };
        if let Some(f) = &mut self.filter {
            let moved = shift.map_range(f.range.0, f.range.2).unwrap();
            self.filter_dirty |= (f.range.0, f.range.2) != moved;
            (f.range.0, f.range.2) = moved;
        }
        self.shift_axis_metadata(shift);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    pub fn delete_rows(&mut self, at: u32, n: u32) {
        self.delete_rows_inner(at, n, true);
    }

    fn delete_rows_inner(&mut self, at: u32, n: u32, move_cells: bool) {
        if n == 0 {
            return;
        }
        let shift = (Structural::DeleteRows { at, n }).reference_shift();
        let shift = MappedShift { shift: &shift, insertions: &[] };
        self.shift_array_and_hyperlink_ranges(&shift);
        if move_cells {
            self.shift_cells(true, at, n, true);
        }
        self.merges = self
            .merges
            .iter()
            .filter_map(|(r1, c1, r2, c2)| {
                let a = shift_axis(*r1, at, n, true)?;
                let b = shift_axis(*r2, at, n, true)?;
                Some((a, *c1, b.max(a), *c2))
            })
            .collect();
        self.shift_notes(&shift);
        self.shift_cf(&shift);
        self.shift_dv(&shift);
        self.shift_sparklines(&shift);
        if let Some((fr, fc)) = self.freeze {
            let nf = if fr >= at + n { fr - n } else { at.min(fr) };
            self.freeze = Some((nf, fc));
        }
        self.layout_dirty |= self.rows.shift_axis(at, n, true);
        self.shift_axis_metadata(&shift);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    pub fn insert_cols(&mut self, at: u32, n: u32) {
        self.insert_cols_inner(at, n, true);
    }

    fn insert_cols_inner(&mut self, at: u32, n: u32, move_cells: bool) {
        if n == 0 {
            return;
        }
        let shift = (Structural::InsertCols { at, n }).reference_shift();
        let shift = MappedShift { shift: &shift, insertions: &[] };
        self.shift_array_and_hyperlink_ranges(&shift);
        if move_cells {
            self.shift_cells(false, at, n, false);
        }
        self.merges = self
            .merges
            .iter()
            .map(|(r1, c1, r2, c2)| {
                let nc1 = if *c1 >= at { c1 + n } else { *c1 };
                let nc2 = if *c2 >= at { c2 + n } else { *c2 };
                (*r1, nc1, *r2, nc2)
            })
            .collect();
        self.shift_notes(&shift);
        self.shift_cf(&shift);
        self.shift_dv(&shift);
        self.shift_sparklines(&shift);
        if let Some((fr, fc)) = self.freeze {
            if fc >= at {
                self.freeze = Some((fr, fc + n));
            }
        }
        self.layout_dirty |= self.cols.shift_axis(at, n, false);
        self.shift_axis_metadata(&shift);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    pub fn delete_cols(&mut self, at: u32, n: u32) {
        self.delete_cols_inner(at, n, true);
    }

    fn delete_cols_inner(&mut self, at: u32, n: u32, move_cells: bool) {
        if n == 0 {
            return;
        }
        let shift = (Structural::DeleteCols { at, n }).reference_shift();
        let shift = MappedShift { shift: &shift, insertions: &[] };
        self.shift_array_and_hyperlink_ranges(&shift);
        if move_cells {
            self.shift_cells(false, at, n, true);
        }
        self.merges = self
            .merges
            .iter()
            .filter_map(|(r1, c1, r2, c2)| {
                let a = shift_axis(*c1, at, n, true)?;
                let b = shift_axis(*c2, at, n, true)?;
                Some((*r1, a, *r2, b.max(a)))
            })
            .collect();
        self.shift_notes(&shift);
        self.shift_cf(&shift);
        self.shift_dv(&shift);
        self.shift_sparklines(&shift);
        if let Some((fr, fc)) = self.freeze {
            let nf = if fc >= at + n { fc - n } else { at.min(fc) };
            self.freeze = Some((fr, nf));
        }
        self.layout_dirty |= self.cols.shift_axis(at, n, true);
        self.shift_axis_metadata(&shift);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    fn shift_axis_metadata(&mut self, shift: &MappedShift) {
        let rows = shift.axis == crate::refs::Axis::Row;
        if shift.n == 0 { return; }
        let min_at = if shift.insertions.is_empty() {
            shift.at
        } else {
            shift.insertions.first().copied().unwrap_or(shift.at)
        };
        crate::scenarios::shift_references(self, shift);
        let mut move_hidden = |hidden: &mut crate::layout::HiddenIndices| {
            if !hidden.is_empty() {
                let changed = if shift.insertions.is_empty() {
                    hidden.shift_axis(shift.at, shift.n, shift.delete)
                } else {
                    hidden.remap_insertions(|pos| shift.map(pos))
                };
                self.layout_dirty |= changed;
            }
        };
        if rows {
            move_hidden(&mut self.hidden_rows);
            move_hidden(&mut self.hidden_rows_user);
        } else {
            move_hidden(&mut self.hidden_cols);
        }
        let levels = if rows { &mut self.row_levels } else { &mut self.col_levels };
        if !levels.is_empty() && levels.keys().any(|&pos| pos >= min_at) {
            *levels = std::mem::take(levels)
                .into_iter()
                .filter_map(|(pos, level)| Some((shift.map(pos)?, level)))
                .collect();
            self.layout_dirty = true;
        }
        let map = if rows { &mut self.row_styles } else { &mut self.col_styles };
        if map.is_empty() || !map.keys().any(|&idx| idx >= min_at) {
            return;
        }
        let old = std::mem::take(map);
        let mut out = std::collections::HashMap::with_capacity(old.len());
        for (idx, st) in old {
            if let Some(nidx) = shift.map(idx) {
                out.insert(nidx, st);
            }
        }
        if rows {
            self.row_styles = out.into();
            self.row_styles_dirty = true;
        } else {
            self.col_styles = out.into();
            self.col_styles_dirty = true;
        }
        self.styles_dirty = true;
    }

    fn shift_notes(&mut self, shift: &MappedShift) {
        if self.notes.is_empty() {
            return;
        }
        let rows = shift.axis == crate::refs::Axis::Row;
        let min_at = if shift.insertions.is_empty() {
            shift.at
        } else {
            shift.insertions.first().copied().unwrap_or(shift.at)
        };
        if !self
            .notes
            .keys()
            .any(|k| (if rows { k.row } else { k.col }) >= min_at)
        {
            return;
        }
        let old = std::mem::take(&mut self.notes);
        let mut out = std::collections::HashMap::with_capacity(old.len());
        self.notes_dirty = true;
        for (k, mut v) in old {
            let pos = if rows { k.row } else { k.col };
            if let Some(np) = shift.map(pos) {
                let key = if rows {
                    CellRef::new(np, k.col)
                } else {
                    CellRef::new(k.row, np)
                };
                v.r = key.row;
                v.c = key.col;
                out.insert(key, v);
            }
        }
        self.notes = out.into();
    }

    /// 迷你图跟着行列结构走：锚点格位移，同表的数据区一起位移；
    /// 锚点被删掉的那条直接丢弃（和 Excel 一致）。
    fn shift_sparklines(&mut self, shift: &MappedShift) {
        let rows = shift.axis == crate::refs::Axis::Row;
        if shift.n == 0 || self.sparklines.is_empty() {
            return;
        }
        let old = std::mem::take(&mut self.sparklines);
        let mut out = Vec::with_capacity(old.len());
        for mut sp in old {
            let anchor = if rows { sp.r } else { sp.c };
            let Some(na) = shift.map(anchor) else {
                continue;
            };
            if rows {
                sp.r = na;
            } else {
                sp.c = na;
            }
            if sp.source_sheet.is_none() {
                let (r1, c1, r2, c2) = sp.rect;
                if rows {
                    let Some((nr1, nr2)) = shift.map_range(r1, r2) else {
                        continue;
                    };
                    sp.rect = (nr1, c1, nr2, c2);
                } else {
                    let Some((nc1, nc2)) = shift.map_range(c1, c2) else {
                        continue;
                    };
                    sp.rect = (r1, nc1, r2, nc2);
                }
            }
            out.push(sp);
        }
        self.sparklines = out;
        self.sparklines_dirty = true;
    }

    fn shift_cf(&mut self, shift: &MappedShift) {
        self.cf_dirty |= !self.cf.blocks.is_empty();
        for block in &mut self.cf.blocks { shift_rule_ranges(&mut block.ranges, shift); }
        self.cf.blocks.retain(|block| !block.ranges.is_empty());
    }

    fn shift_dv(&mut self, shift: &MappedShift) {
        self.dv_dirty |= !self.dv.is_empty();
        for rule in &mut self.dv { shift_rule_ranges(&mut rule.ranges, shift); }
        self.dv.retain(|rule| !rule.ranges.is_empty());
    }
}

fn shift_rule_ranges(ranges: &mut Vec<(u32, u32, u32, u32)>, shift: &MappedShift) {
    ranges.retain_mut(|(r1, c1, r2, c2)| {
        let (first, last, limit) = match shift.axis {
            crate::refs::Axis::Row => (r1, r2, 1_048_575),
            crate::refs::Axis::Col => (c1, c2, 16_383),
        };
        // A rule covering entire columns/rows continues to cover the entire axis.
        if *first == 0 && *last == limit { return true; }
        let Some((a, b)) = shift.map_range(*first, *last) else {
            return false;
        };
        *first = a;
        *last = b;
        true
    });
}

/// 行高/列宽的轴：插入删除时把后面的挪一挪（保持视觉位置）
impl crate::layout::Axis {
    pub fn insert_at(&mut self, at: u32, n: u32) {
        self.shift_axis(at, n, false);
    }

    pub fn delete_at(&mut self, at: u32, n: u32) {
        self.shift_axis(at, n, true);
    }
}

/// 整个工作簿层面的结构改动：搬格子 + 改写所有表里的公式引用
/// 插入/删除单元格（局部区域）：按区域改写公式引用。
/// horizontal=true 表示右移/左移（列位移），false 表示下移/上移（行位移）。
pub fn apply_cell_shift_wb(
    wb: &mut crate::model::Workbook,
    sheet_idx: usize,
    rect: (u32, u32, u32, u32),
    horizontal: bool,
    insert: bool,
) {
    let Some(name) = wb.sheets.get(sheet_idx).map(|s| s.name.clone()) else {
        return;
    };
    let (r1, c1, r2, c2) = rect;
    let (axis, at, n) = if horizontal {
        (crate::refs::Axis::Col, c1, c2.saturating_sub(c1) + 1)
    } else {
        (crate::refs::Axis::Row, r1, r2.saturating_sub(r1) + 1)
    };
    for (idx, sheet) in wb.sheets.iter_mut().enumerate() {
        let shift = crate::refs::Shift {
            axis,
            at,
            n,
            delete: !insert,
            target_sheet: name.clone(),
            own_sheet: if idx == sheet_idx {
                name.clone()
            } else {
                sheet.name.clone()
            },
            translate: None,
            row_filter: if horizontal { Some((r1, r2)) } else { None },
            col_filter: if horizontal { None } else { Some((c1, c2)) },
        };
        sheet.rewrite_references(&shift);
    }
}

pub fn apply_structural_wb(wb: &mut crate::model::Workbook, sheet_idx: usize, op: &Structural) {
    let Some(sheet) = wb.sheets.get_mut(sheet_idx) else { return; };
    sheet.apply_structural(op);
    update_workbook_references(wb, sheet_idx, op.reference_shift(), &[]);
}

/// The final entry is the grand total and shares the last group's original
/// boundary. Table calculated columns may inherit that newly inserted subtotal
/// row, so defer only the grand total insertion when a table intersects the edit.
pub(crate) fn insert_subtotal_rows(wb: &mut crate::model::Workbook, sheet_idx: usize, positions: &[u32]) {
    let Some(&at) = positions.first() else { return };
    let last = *positions.last().unwrap();
    let sheet = &wb.sheets[sheet_idx];
    let inherits = sheet.tables.iter().any(|t| t.range.0 < last && t.range.2.saturating_add(1) >= at);
    let grouped = if inherits { &positions[..positions.len() - 1] } else { positions };
    let mut shift = (Structural::InsertRows { at, n: 1 }).reference_shift();
    shift.own_sheet = sheet.name.clone();
    shift.target_sheet = sheet.name.clone();
    let mut fills = Vec::new();
    if inherits {
        for (i, &row) in grouped.iter().enumerate() {
            // Translate from the pre-insertion source, then apply this and all
            // earlier boundaries to the generated formula exactly once.
            let prefix = MappedShift { shift: &shift, insertions: &grouped[..=i] };
            for mut fill in sheet.capture_table_row_fills(row, 1, &grouped[i + 1..]) {
                fill.row += i as u32;
                fill.formula = crate::refs::rewrite_mapped(&fill.formula, &prefix).into();
                fills.push(fill);
            }
        }
    }
    let map = MappedShift { shift: &shift, insertions: grouped };
    let sheet = &mut wb.sheets[sheet_idx];
    sheet.insert_mapped_rows(&map);
    for table in &mut sheet.tables {
        let (r1, _, r2, _) = table.range;
        let first = map.map(r1).unwrap();
        // With no totals row, inserting immediately after the table expands it.
        let end = r2 + grouped.partition_point(|&r| r <= r2 + u32::from(!table.totals_row)) as u32;
        sheet.tables_dirty |= (first, end) != (r1, r2);
        (table.range.0, table.range.2) = (first, end);
    }
    update_workbook_references(wb, sheet_idx, shift, grouped);
    wb.sheets[sheet_idx].apply_table_row_fills(fills);
    if inherits {
        let total = last + grouped.len() as u32;
        apply_structural_wb(wb, sheet_idx, &Structural::InsertRows { at: total, n: 1 });
    }
}

fn update_workbook_references(
    wb: &mut crate::model::Workbook, sheet_idx: usize, mut shift: crate::refs::Shift, insertions: &[u32],
) {
    let name = wb.sheets[sheet_idx].name.clone();
    shift.target_sheet.clone_from(&name);
    let (axis, at, n, delete) = (shift.axis, shift.at, shift.n, shift.delete);
    for sheet in &mut wb.sheets {
        shift.own_sheet.clone_from(&sheet.name);
        let shift = MappedShift { shift: &shift, insertions };
        sheet.rewrite_mapped_references(&shift);
        // 图表系列引用
        for ch in sheet.charts.iter_mut() {
            if ch.rewrite_mapped_refs(&shift) {
                sheet.charts_dirty = true;
            }
        }
        // 迷你图：数据在别的表上的，引用跟着那张表一起位移
        for sp in sheet.sparklines.iter_mut() {
            if sp.source_sheet.as_deref() != Some(name.as_str()) {
                continue;
            }
            let (r1, c1, r2, c2) = sp.rect;
            if axis == crate::refs::Axis::Row {
                let (nr1, nr2) = shift.map_range(r1, r2).unwrap_or((at, at));
                sp.rect = (nr1, c1, nr2, c2);
            } else {
                let (nc1, nc2) = shift.map_range(c1, c2).unwrap_or((at, at));
                sp.rect = (r1, nc1, r2, nc2);
            }
            sheet.sparklines_dirty = true;
        }
    }
    // 记一笔，保存时搬透视表缓存
    if insertions.is_empty() {
        wb.pivot_shifts.push((name.clone(), u8::from(axis == crate::refs::Axis::Col), at, n, delete));
    } else {
        wb.pivot_shifts.extend(insertions.iter().rev().map(|&r| (name.clone(), 0, r, 1, false)));
    }

    // 自定义名称
    for (_, local, text) in wb.names.iter_mut() {
        let own = match local {
            Some(i) => wb.sheets.get(*i).map(|s| s.name.clone()).unwrap_or_default(),
            None => String::new(),
        };
        shift.own_sheet = own;
        let nt = crate::refs::rewrite_mapped(text, &MappedShift { shift: &shift, insertions });
        if nt != *text {
            *text = nt;
            wb.names_dirty = true;
        }
    }

    // Session-created pivots keep absolute source-field and output coordinates.
    // If an essential field or either complete region disappears, keep any
    // remaining summary values but remove the now-invalid refreshable object.
    let sh = MappedShift { shift: &shift, insertions };
    let map_rect = |rect: (u32, u32, u32, u32)| {
        let (r1, c1, r2, c2) = rect;
        if r1 > r2 || c1 > c2 { return None; }
        match axis {
            crate::refs::Axis::Row => sh.map_range(r1, r2).map(|(a, b)| (a, c1, b, c2)),
            crate::refs::Axis::Col => sh.map_range(c1, c2).map(|(a, b)| (r1, a, r2, b)),
        }
    };
    wb.pivots_pending.retain_mut(|(idx, spec, _, rect, _)| {
        if *idx != sheet_idx {
            return true;
        }
        let mapped = (|| {
            let source = map_rect(spec.source)?;
            let output = map_rect(*rect)?;
            let (row_field, col_field, value_field) = if axis == crate::refs::Axis::Col {
                let col_field = match spec.col_field {
                    Some(c) => Some(sh.map(c)?),
                    None => None,
                };
                (sh.map(spec.row_field)?, col_field, sh.map(spec.value_field)?)
            } else {
                (spec.row_field, spec.col_field, spec.value_field)
            };
            Some((source, output, row_field, col_field, value_field))
        })();
        let Some((source, output, row_field, col_field, value_field)) = mapped else {
            return false;
        };
        spec.source = source;
        spec.row_field = row_field;
        spec.col_field = col_field;
        spec.value_field = value_field;
        *rect = output;
        true
    });
}

/// 供撤销用：一行/一列里有哪些格子
pub fn collect_region(sheet: &Sheet, r1: u32, c1: u32, r2: u32, c2: u32) -> HashSet<CellRef> {
    sheet.iter_rect((r1, c1, r2, c2)).map(|(cref, _)| cref).collect()
}

/// Sorted, non-adjacent closed deletion intervals `[(lo_0, hi_0), ...]` (`lo_{j+1} >= hi_j + 2`)
/// with prefix deleted row counts for `O(log K)` coordinate/range remapping and `O(|formula| log K)`
/// single-pass formula rewriting.
pub(crate) struct RowDeleteMap {
    pub(crate) intervals: Vec<(u32, u32)>,
    /// `prefix_deleted[i]` = total deleted rows across `intervals[..i]` (`len == intervals.len() + 1`).
    prefix_deleted: Vec<u32>,
}

impl RowDeleteMap {
    pub(crate) fn from_intervals(raw: &[(u32, u32)]) -> Option<Self> {
        let mut sorted: Vec<(u32, u32)> = raw.iter().copied().filter(|&(lo, hi)| lo <= hi).collect();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_unstable_by_key(|&(lo, _)| lo);
        let mut intervals: Vec<(u32, u32)> = Vec::with_capacity(sorted.len());
        for (lo, hi) in sorted {
            if let Some(last) = intervals.last_mut()
                && lo <= last.1.saturating_add(1)
            {
                last.1 = last.1.max(hi);
            } else {
                intervals.push((lo, hi));
            }
        }
        let mut prefix_deleted = Vec::with_capacity(intervals.len() + 1);
        prefix_deleted.push(0);
        let mut acc = 0u32;
        for &(lo, hi) in &intervals {
            acc = acc.saturating_add(hi - lo + 1);
            prefix_deleted.push(acc);
        }
        Some(Self {
            intervals,
            prefix_deleted,
        })
    }

    #[inline]
    pub(crate) fn min_at(&self) -> u32 {
        self.intervals[0].0
    }

    /// Map a single row index in `O(log K)`; returns `None` if `r` falls inside a deleted interval.
    #[inline]
    pub(crate) fn map(&self, r: u32) -> Option<u32> {
        let k = self.intervals.partition_point(|&(lo, _)| lo <= r);
        if k > 0 && r <= self.intervals[k - 1].1 {
            return None;
        }
        Some(r - self.prefix_deleted[k])
    }

    /// Map a closed row range `[a, b]` (`O(log K)`), shrinking deleted edges or returning `None` if wholly deleted.
    #[inline]
    pub(crate) fn map_range(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let k_lo = self.intervals.partition_point(|&(start, _)| start <= lo);
        let first_surviving = if k_lo > 0 && lo <= self.intervals[k_lo - 1].1 {
            self.intervals[k_lo - 1].1.checked_add(1)?
        } else {
            lo
        };
        if first_surviving > hi {
            return None;
        }
        let k_hi = self.intervals.partition_point(|&(start, _)| start <= hi);
        let last_surviving = if k_hi > 0 && hi <= self.intervals[k_hi - 1].1 {
            self.intervals[k_hi - 1].0.checked_sub(1)?
        } else {
            hi
        };
        if first_surviving > last_surviving {
            return None;
        }
        Some((self.map(first_surviving)?, self.map(last_surviving)?))
    }

    /// Map a freeze boundary `fr` (matching sequential `DeleteRows`: `if fr >= at + n { fr - n } else { at.min(fr) }`).
    #[inline]
    pub(crate) fn map_freeze_or_clamp(&self, fr: u32) -> u32 {
        let k = self.intervals.partition_point(|&(lo, _)| lo <= fr);
        if k > 0 && fr <= self.intervals[k - 1].1 {
            self.intervals[k - 1].0 - self.prefix_deleted[k - 1]
        } else {
            fr - self.prefix_deleted[k]
        }
    }

    /// Rewrite `formula` against all deleted intervals in `O(|formula| * log K)` by coalescing
    /// contiguous runs of intervals that do not contain any row token mentioned in `formula`.
    pub(crate) fn rewrite_formula(&self, formula: &str, own_sheet: &str, target_sheet: &str) -> String {
        let base_shift = crate::refs::Shift {
            axis: crate::refs::Axis::Row,
            at: self.min_at(),
            n: 1,
            delete: true,
            own_sheet: own_sheet.to_string(),
            target_sheet: target_sheet.to_string(),
            ..Default::default()
        };
        if !formula_may_be_affected(
            formula,
            &MappedShift {
                shift: &base_shift,
                insertions: &[],
            },
        ) {
            return formula.to_string();
        }

        // Extract sorted 0-based row integers appearing in `formula` (`<= |formula| / 2`, usually 1..4).
        let bytes = formula.as_bytes();
        let mut mentioned: Vec<u32> = Vec::new();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let mut val = 0u64;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    val = val.saturating_mul(10).saturating_add((bytes[i] - b'0') as u64);
                    i += 1;
                }
                if (1..=1_048_576).contains(&val) {
                    mentioned.push((val - 1) as u32);
                }
            } else {
                i += 1;
            }
        }
        if mentioned.is_empty() {
            return formula.to_string();
        }
        mentioned.sort_unstable();
        mentioned.dedup();
        let max_r = *mentioned.last().unwrap();
        let end_idx = self.intervals.partition_point(|&(lo, _)| lo <= max_r);
        if end_idx == 0 {
            return formula.to_string();
        }

        // Partition `self.intervals[..end_idx]` around `mentioned` in `O(mentioned.len() * log K)`.
        let mut steps: Vec<(u32, u32)> = Vec::with_capacity(mentioned.len() * 2 + 1);
        let mut cur_idx = 0usize;
        for &r in &mentioned {
            if cur_idx >= end_idx {
                break;
            }
            // All intervals in `cur_idx..before_r` have `hi < r` and contain NO mentioned row -> merge into 1 Shift!
            let before_r = cur_idx + self.intervals[cur_idx..end_idx].partition_point(|&(_, hi)| hi < r);
            if before_r > cur_idx {
                let at = self.intervals[cur_idx].0;
                let n = self.prefix_deleted[before_r] - self.prefix_deleted[cur_idx];
                steps.push((at, n));
                cur_idx = before_r;
            }
            // If `intervals[cur_idx]` straddles/contains `r`, emit it unmerged.
            if cur_idx < end_idx && self.intervals[cur_idx].0 <= r && r <= self.intervals[cur_idx].1 {
                let (lo, hi) = self.intervals[cur_idx];
                steps.push((lo, hi - lo + 1));
                cur_idx += 1;
            }
        }
        if cur_idx < end_idx {
            let at = self.intervals[cur_idx].0;
            let n = self.prefix_deleted[end_idx] - self.prefix_deleted[cur_idx];
            steps.push((at, n));
        }

        let mut cur = formula.to_string();
        let mut sh = base_shift;
        for &(at, n) in steps.iter().rev() {
            sh.at = at;
            sh.n = n;
            cur = crate::refs::rewrite(&cur, &sh);
        }
        cur
    }
}

impl Sheet {
    /// Return sorted disjoint closed intervals of visible rows in `[r1, r2]`
    /// (excluding filter `hidden_rows`, manual `hidden_rows_user`, and `0`-height `rows` spans)
    /// in `O(intervals + spans)` time.
    pub fn visible_row_intervals(&self, r1: u32, r2: u32) -> Vec<(u32, u32)> {
        if r1 > r2 {
            return Vec::new();
        }
        let mut hidden_raw: Vec<(u32, u32)> = Vec::new();
        for &(lo, hi) in self.hidden_rows.intervals() {
            if hi >= r1 && lo <= r2 {
                hidden_raw.push((lo.max(r1), hi.min(r2)));
            }
        }
        for &(lo, hi) in self.hidden_rows_user.intervals() {
            if hi >= r1 && lo <= r2 {
                hidden_raw.push((lo.max(r1), hi.min(r2)));
            }
        }
        for sp in self.rows.spans() {
            if sp.size <= 0.0 && sp.end >= r1 && sp.start <= r2 {
                hidden_raw.push((sp.start.max(r1), sp.end.min(r2)));
            }
        }
        if hidden_raw.is_empty() {
            return if self.rows.default_size() > 0.0 {
                vec![(r1, r2)]
            } else {
                Vec::new()
            };
        }
        let hidden = crate::layout::HiddenIndices::from_intervals(hidden_raw);
        hidden.complement_in(r1, r2)
    }

    /// Delete multiple sorted/unsorted closed row intervals `[(r1, r2), ...]` in a single pass
    /// over cells, styles, axis spans, hidden intervals, and sheet metadata (`O(affected_pages + K)`).
    pub fn delete_row_intervals(&mut self, intervals: &[(u32, u32)]) {
        let Some(del_map) = RowDeleteMap::from_intervals(intervals) else {
            return;
        };
        if del_map.intervals.len() == 1 {
            let (lo, hi) = del_map.intervals[0];
            self.delete_rows(lo, hi - lo + 1);
            return;
        }
        let min_at = del_map.min_at();
        self.coordinates_shifted();

        if !self.arrays.is_empty() {
            for &(anchor, (r1, c1, r2, c2)) in &self.arrays {
                if r2 < min_at {
                    continue;
                }
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        let key = CellRef::new(r, c);
                        if key != anchor && self.get(r, c).is_none_or(|cell| cell.formula.is_none()) {
                            self.spilled.insert(key);
                        }
                    }
                }
            }
        }
        if !self.spilled.is_empty() && self.spilled.iter().any(|k| k.row >= min_at) {
            self.spilled = self
                .spilled
                .iter()
                .filter_map(|key| Some(CellRef::new(del_map.map(key.row)?, key.col)))
                .collect();
        }
        if !self.spill_shapes.is_empty()
            && self
                .spill_shapes
                .iter()
                .any(|(k, &(h, _))| k.row.saturating_add(h.saturating_sub(1)) >= min_at)
        {
            let mut next_shapes = crate::model::CellMetadata::default();
            let mut orphan_tails: Vec<(u32, u32, u32, u32)> = Vec::new();
            for (&anchor, &(h, w)) in &self.spill_shapes {
                let Some(new_r) = del_map.map(anchor.row) else {
                    if h > 0 && w > 0 {
                        let r2 = anchor.row.saturating_add(h.saturating_sub(1));
                        let c2 = anchor.col.saturating_add(w.saturating_sub(1));
                        if let Some((nr1, nr2)) = del_map.map_range(anchor.row, r2) {
                            orphan_tails.push((nr1, anchor.col, nr2, c2));
                        }
                    }
                    continue;
                };
                let new_anchor = CellRef::new(new_r, anchor.col);
                if h == 0 || w == 0 {
                    next_shapes.insert(new_anchor, (0, 0));
                    continue;
                }
                let end = anchor.row.saturating_add(h.saturating_sub(1));
                let intact = del_map
                    .map_range(anchor.row, end)
                    .is_some_and(|(nr1, nr2)| nr1 == new_r && nr2 - nr1 + 1 == h);
                if intact {
                    next_shapes.insert(new_anchor, (h, w));
                } else {
                    let c2 = anchor.col.saturating_add(w.saturating_sub(1));
                    if let Some((nr1, nr2)) = del_map.map_range(anchor.row, end) {
                        orphan_tails.push((nr1, anchor.col, nr2, c2));
                    }
                    next_shapes.insert(new_anchor, (0, 0));
                }
            }
            self.spill_shapes = next_shapes;
            for (r1, c1, r2, c2) in orphan_tails {
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        let k = CellRef::new(r, c);
                        if self.spilled.remove_if_present(&k) {
                            self.remove(r, c);
                        }
                    }
                }
            }
        }
        if !self.hyperlinks.is_empty()
            && self
                .hyperlinks
                .iter()
                .any(|(&(r, _), link)| r.saturating_add(link.span.0) >= min_at)
        {
            self.hyperlinks = std::mem::take(&mut self.hyperlinks)
                .into_iter()
                .filter_map(|((r, c), mut link)| {
                    let end = r.saturating_add(link.span.0);
                    let moved = del_map.map_range(r, end);
                    self.hyperlinks_dirty |= moved != Some((r, end));
                    let (first, last) = moved?;
                    link.span.0 = last - first;
                    Some(((first, c), link))
                })
                .collect::<std::collections::HashMap<_, _>>()
                .into();
        }
        if !self.arrays.is_empty() {
            self.arrays = self
                .arrays
                .iter()
                .filter_map(|&(anchor, (r1, c1, r2, c2))| {
                    let row = del_map.map(anchor.row)?;
                    let (nr1, nr2) = del_map.map_range(r1, r2)?;
                    Some((CellRef::new(row, anchor.col), (nr1, c1, nr2, c2)))
                })
                .collect();
        }

        // Single-pass cell & style page compaction across all K intervals!
        self.remap_cell_axis(true, min_at, |r| del_map.map(r));

        self.merges = self
            .merges
            .iter()
            .filter_map(|&(r1, c1, r2, c2)| {
                let a = del_map.map(r1)?;
                let b = del_map.map(r2)?;
                Some((a, c1, b.max(a), c2))
            })
            .collect();

        if !self.notes.is_empty() && self.notes.keys().any(|k| k.row >= min_at) {
            let old = std::mem::take(&mut self.notes);
            let mut out = std::collections::HashMap::with_capacity(old.len());
            self.notes_dirty = true;
            for (k, mut v) in old {
                if let Some(nr) = del_map.map(k.row) {
                    let key = CellRef::new(nr, k.col);
                    v.r = nr;
                    v.c = k.col;
                    out.insert(key, v);
                }
            }
            self.notes = out.into();
        }

        if !self.cf.blocks.is_empty() {
            self.cf_dirty = true;
            for block in &mut self.cf.blocks {
                block.ranges.retain_mut(|(r1, _, r2, _)| {
                    if *r1 == 0 && *r2 == 1_048_575 {
                        return true;
                    }
                    let Some((a, b)) = del_map.map_range(*r1, *r2) else {
                        return false;
                    };
                    *r1 = a;
                    *r2 = b;
                    true
                });
            }
            self.cf.blocks.retain(|b| !b.ranges.is_empty());
        }

        if !self.dv.is_empty() {
            self.dv_dirty = true;
            for rule in &mut self.dv {
                rule.ranges.retain_mut(|(r1, _, r2, _)| {
                    if *r1 == 0 && *r2 == 1_048_575 {
                        return true;
                    }
                    let Some((a, b)) = del_map.map_range(*r1, *r2) else {
                        return false;
                    };
                    *r1 = a;
                    *r2 = b;
                    true
                });
            }
            self.dv.retain(|r| !r.ranges.is_empty());
        }

        if !self.sparklines.is_empty() {
            let old = std::mem::take(&mut self.sparklines);
            let mut out = Vec::with_capacity(old.len());
            for mut sp in old {
                let Some(na) = del_map.map(sp.r) else {
                    continue;
                };
                sp.r = na;
                if sp.source_sheet.is_none() {
                    let (r1, c1, r2, c2) = sp.rect;
                    let Some((a, b)) = del_map.map_range(r1, r2) else {
                        continue;
                    };
                    sp.rect = (a, c1, b, c2);
                }
                out.push(sp);
            }
            self.sparklines = out;
            self.sparklines_dirty = true;
        }

        if let Some((fr, fc)) = self.freeze {
            self.freeze = Some((del_map.map_freeze_or_clamp(fr), fc));
        }

        self.layout_dirty |= self.rows.shift_delete_intervals(&del_map.intervals);
        for &(lo, hi) in del_map.intervals.iter().rev() {
            let sh = (Structural::DeleteRows {
                at: lo,
                n: hi - lo + 1,
            })
            .reference_shift();
            crate::scenarios::shift_references(self, &MappedShift { shift: &sh, insertions: &[] });
        }
        self.layout_dirty |= self.hidden_rows.shift_delete_intervals(&del_map.intervals);
        self.layout_dirty |= self.hidden_rows_user.shift_delete_intervals(&del_map.intervals);

        if !self.row_levels.is_empty() && self.row_levels.keys().any(|&r| r >= min_at) {
            self.row_levels = std::mem::take(&mut self.row_levels)
                .into_iter()
                .filter_map(|(r, lv)| Some((del_map.map(r)?, lv)))
                .collect();
            self.layout_dirty = true;
        }
        if !self.row_styles.is_empty() && self.row_styles.keys().any(|&r| r >= min_at) {
            let old = std::mem::take(&mut self.row_styles);
            let mut out = std::collections::HashMap::with_capacity(old.len());
            for (idx, st) in old {
                if let Some(nidx) = del_map.map(idx) {
                    out.insert(nidx, st);
                }
            }
            self.row_styles = out.into();
            self.row_styles_dirty = true;
            self.styles_dirty = true;
        }
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }
}

/// Workbook-level single-pass deletion of multiple closed row intervals `[(r1, r2), ...]`
/// (for example deleting all visible rows in a filtered sheet without touching hidden rows).
pub fn delete_row_intervals_wb(
    wb: &mut crate::model::Workbook,
    sheet_idx: usize,
    intervals: &[(u32, u32)],
) {
    let Some(del_map) = RowDeleteMap::from_intervals(intervals) else {
        return;
    };
    if del_map.intervals.len() == 1 {
        let (lo, hi) = del_map.intervals[0];
        apply_structural_wb(
            wb,
            sheet_idx,
            &Structural::DeleteRows {
                at: lo,
                n: hi - lo + 1,
            },
        );
        return;
    }
    let Some(target_name) = wb.sheets.get(sheet_idx).map(|s| s.name.clone()) else {
        return;
    };
    wb.sheets[sheet_idx].delete_row_intervals(&del_map.intervals);

    for sheet in &mut wb.sheets {
        let own_name = sheet.name.clone();
        let foreign = (!own_name.eq_ignore_ascii_case(&target_name)).then_some(target_name.as_str());
        let changed = sheet.cells_mut().rewrite_formulas_parallel(foreign, |old| {
            let new = del_map.rewrite_formula(old, &own_name, &target_name);
            (new != old).then(|| new.into())
        });
        sheet.edited |= changed;

        for blk in sheet.cf.blocks.iter_mut() {
            for rule in blk.rules.iter_mut() {
                let mut rewrite_threshold = |formula: &mut String| {
                    let rewritten = del_map.rewrite_formula(formula, &own_name, &target_name);
                    if *formula != rewritten {
                        *formula = rewritten;
                        sheet.cf_dirty = true;
                    }
                };
                match &mut rule.kind {
                    crate::cf::CfKind::Expr { formula, .. } => rewrite_threshold(formula),
                    crate::cf::CfKind::CellIs { a, b, .. } => {
                        rewrite_threshold(a);
                        rewrite_threshold(b);
                    }
                    _ => {}
                }
            }
        }
        for dv in sheet.dv.iter_mut() {
            let f1 = del_map.rewrite_formula(&dv.f1, &own_name, &target_name);
            let f2 = del_map.rewrite_formula(&dv.f2, &own_name, &target_name);
            sheet.dv_dirty |= dv.f1 != f1 || dv.f2 != f2;
            (dv.f1, dv.f2) = (f1, f2);
        }
        if !sheet.charts.is_empty() {
            for &(lo, hi) in del_map.intervals.iter().rev() {
                let sh = crate::refs::Shift {
                    axis: crate::refs::Axis::Row,
                    at: lo,
                    n: hi - lo + 1,
                    delete: true,
                    target_sheet: target_name.clone(),
                    own_sheet: own_name.clone(),
                    ..Default::default()
                };
                let ms = MappedShift {
                    shift: &sh,
                    insertions: &[],
                };
                for ch in sheet.charts.iter_mut() {
                    if ch.rewrite_mapped_refs(&ms) {
                        sheet.charts_dirty = true;
                    }
                }
            }
        }
        for sp in sheet.sparklines.iter_mut() {
            if sp.source_sheet.as_deref() != Some(target_name.as_str()) {
                continue;
            }
            let (r1, c1, r2, c2) = sp.rect;
            if let Some((a, b)) = del_map.map_range(r1, r2) {
                sp.rect = (a, c1, b, c2);
            } else {
                let fallback = del_map.map_freeze_or_clamp(r1);
                sp.rect = (fallback, c1, fallback, c2);
            }
            sheet.sparklines_dirty = true;
        }
    }

    for &(lo, hi) in del_map.intervals.iter().rev() {
        wb.pivot_shifts
            .push((target_name.clone(), 0, lo, hi - lo + 1, true));
    }

    for (_, local, text) in wb.names.iter_mut() {
        let own = match local {
            Some(i) => wb.sheets.get(*i).map(|s| s.name.clone()).unwrap_or_default(),
            None => String::new(),
        };
        let nt = del_map.rewrite_formula(text, &own, &target_name);
        if nt != *text {
            *text = nt;
            wb.names_dirty = true;
        }
    }

    wb.pivots_pending.retain_mut(|(idx, spec, _, rect, _)| {
        if *idx != sheet_idx {
            return true;
        }
        let map_rect = |(r1, c1, r2, c2): (u32, u32, u32, u32)| {
            if r1 > r2 || c1 > c2 {
                return None;
            }
            del_map.map_range(r1, r2).map(|(a, b)| (a, c1, b, c2))
        };
        let Some(source) = map_rect(spec.source) else {
            return false;
        };
        let Some(output) = map_rect(*rect) else {
            return false;
        };
        spec.source = source;
        *rect = output;
        true
    });
}


#[cfg(test)]
mod wave5_structural_tests {
    use super::*;
    use crate::cf::{CfBlock, CfKind, CfOp, CfRule};
    use crate::dv::{DataValidation, DvKind};
    use crate::model::{Cell, CellRef, Sheet, Value, Workbook};

    #[test]
    fn hidden_950k_rows_structural_insert_preserves_single_span_fast() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "S1".into();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
        wb.sheets[0].rows.hide_ranges(&[(10, 950_010)]);
        assert_eq!(wb.sheets[0].rows.spans().len(), 1);

        let t0 = std::time::Instant::now();
        apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 500, n: 5 });
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert!(elapsed_ms < 25.0, "expected O(spans) < 25 ms, got {elapsed_ms:.2} ms");
        assert!(
            wb.sheets[0].rows.spans().len() <= 2,
            "950k zero-height span must shift as intervals without expanding, got {}",
            wb.sheets[0].rows.spans().len()
        );
        assert_eq!(wb.sheets[0].rows.size(502), crate::layout::DEFAULT_ROW_H);
        assert_eq!(wb.sheets[0].rows.size(950_015), 0.0);
    }

    #[test]
    fn delete_row_intervals_wb_matches_reverse_sequential_delete_rows_and_visible_intervals() {
        let mut base = Workbook::new_blank();
        base.sheets[0].name = "明细".into();
        {
            let s = &mut base.sheets[0];
            s.hidden_rows = vec![(3, 5), (12, 14)].into();
            s.hidden_rows_user = vec![(5, 6), (20, 20)].into();
            assert_eq!(
                s.visible_row_intervals(2, 22),
                vec![(2, 2), (7, 11), (15, 19), (21, 22)]
            );
            for r in 0..32u32 {
                s.set(r, 0, Cell { value: Value::Text(format!("R{r}").into()), formula: None });
                s.set(r, 1, Cell { value: Value::Number((r + 1) as f64), formula: None });
                s.set(
                    r,
                    2,
                    Cell {
                        value: Value::Number(0.0),
                        formula: Some(format!("=B{}+SUM($B$2:$B$28)+B16", r + 1).into()),
                    },
                );
                if r % 3 == 0 {
                    s.set_style(r, 1, 4);
                    s.rows.set(r, 26.0);
                }
            }
            s.merge_range(7, 0, 10, 0);
            s.cf.blocks.push(CfBlock {
                // `(3, 1, 24, 1)` has BOTH endpoints inside deleted intervals `(2..=4)` and `(22..=25)`
                // while middle rows `5..=8`, `11..=13`, `17..=21` survive -> must shrink to `(2, 1, 13, 1)`!
                ranges: vec![(1, 1, 28, 1), (14, 1, 16, 1), (3, 1, 24, 1)],
                rules: vec![CfRule {
                    kind: CfKind::CellIs {
                        op: CfOp::Greater,
                        a: "10".into(),
                        b: String::new(),
                        dxf: 0,
                    },
                    stop: false,
                    priority: 1,
                }],
            });
            s.dv.push(DataValidation {
                // `(2, 1, 26, 1)` has start deleted (`2` in `2..=4`) -> must shrink to `(2, 1, 14, 1)` without collapsing!
                ranges: vec![(2, 1, 26, 1), (3, 1, 24, 1)],
                kind: DvKind::List,
                f1: "\"A,B\"".into(),
                ..Default::default()
            });
            s.sparklines.push(crate::sparkline::Sparkline {
                r: 0,
                c: 3,
                source_sheet: None,
                rect: (3, 1, 24, 1),
                ..Default::default()
            });
            s.notes.insert(
                CellRef::new(18, 0),
                crate::notes::Note {
                    r: 18,
                    c: 0,
                    text: "note-18".into(),
                    author: "U".into(),
                },
            );
        }
        let mut summary = Sheet::new("汇总");
        summary.set(
            0,
            0,
            Cell {
                value: Value::Number(0.0),
                formula: Some("=SUM(明细!B2:B30)+明细!B16+明细!B25".into()),
            },
        );
        summary.sparklines.push(crate::sparkline::Sparkline {
            r: 0,
            c: 1,
            source_sheet: Some("明细".into()),
            rect: (3, 1, 24, 1),
            ..Default::default()
        });
        base.sheets.push(summary);

        let intervals = [(2u32, 4u32), (9, 10), (14, 16), (22, 25)];
        let mut via_batch = base.clone();
        delete_row_intervals_wb(&mut via_batch, 0, &intervals);

        let mut via_seq = base.clone();
        for &(lo, hi) in intervals.iter().rev() {
            apply_structural_wb(
                &mut via_seq,
                0,
                &Structural::DeleteRows {
                    at: lo,
                    n: hi - lo + 1,
                },
            );
        }

        let s_batch = &via_batch.sheets[0];
        let s_seq = &via_seq.sheets[0];
        assert_eq!(s_batch.len(), s_seq.len());
        for r in 0..32u32 {
            for c in 0..3u32 {
                assert_eq!(s_batch.get(r, c), s_seq.get(r, c), "cell mismatch at ({r},{c})");
                assert_eq!(s_batch.style_of(r, c), s_seq.style_of(r, c), "style mismatch at ({r},{c})");
            }
            assert_eq!(s_batch.rows.size(r), s_seq.rows.size(r), "row height mismatch at {r}");
        }
        assert_eq!(s_batch.merges, s_seq.merges);
        assert_eq!(s_batch.cf.blocks[0].ranges, vec![(1, 1, 16, 1), (2, 1, 13, 1)]);
        assert_eq!(s_batch.cf.blocks[0].ranges, s_seq.cf.blocks[0].ranges);
        assert_eq!(s_batch.dv[0].ranges, vec![(2, 1, 14, 1), (2, 1, 13, 1)]);
        assert_eq!(s_batch.dv[0].ranges, s_seq.dv[0].ranges);
        assert_eq!(s_batch.sparklines[0].rect, (2, 1, 13, 1));
        assert_eq!(s_batch.sparklines, s_seq.sparklines);
        assert_eq!(via_batch.sheets[1].sparklines[0].rect, (2, 1, 13, 1));
        assert_eq!(via_batch.sheets[1].sparklines, via_seq.sheets[1].sparklines);
        assert_eq!(s_batch.hidden_rows.intervals(), s_seq.hidden_rows.intervals());
        assert_eq!(s_batch.hidden_rows_user.intervals(), s_seq.hidden_rows_user.intervals());
        assert_eq!(s_batch.notes, s_seq.notes);
        assert_eq!(via_batch.sheets[1].get(0, 0), via_seq.sheets[1].get(0, 0));
    }
}
