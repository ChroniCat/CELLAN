//! 插入 / 删除行列：整行整列搬移，并同步调整合并、批注、条件格式、数据验证、冻结、筛选。
//!
//! 分页列式存储仅搬移插删位置之后的格子，未受影响的列页仍可与撤销快照共享。
//!
//! 工作簿级入口同时改写本表、跨表公式和名称引用。

use crate::model::{Cell, CellRef, Sheet};
use crate::refs::MappedShift;
use std::collections::HashSet;

/// 当插入/删除本表行列时，同步重写指向本表的内部超链接目标（如 `Sheet1!B5`、`#Sheet1!$A$1:$C$3`、`B5`）。
fn shift_internal_link_location(
    loc: &str,
    own_sheet: &str,
    shift: &MappedShift,
    rows: bool,
) -> Option<String> {
    let trimmed = loc.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (hash_prefix, rest) = if let Some(r) = trimmed.strip_prefix('#') {
        ("#", r)
    } else {
        ("", trimmed)
    };
    let (sheet_prefix, range_part) = if let Some((sh, rg)) = rest.rsplit_once('!') {
        let unquoted = sh.trim().trim_matches('\'').replace("''", "'");
        if !unquoted.eq_ignore_ascii_case(own_sheet) {
            return None;
        }
        (Some(sh), rg.trim())
    } else {
        (None, rest.trim())
    };
    let map_token = |tok: &str| -> Option<String> {
        let mut s = tok.trim();
        let col_abs = s.starts_with('$');
        if col_abs {
            s = &s[1..];
        }
        let col_end = s.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
        if !(1..=3).contains(&col_end) {
            return None;
        }
        let col_str = &s[..col_end];
        let mut r_part = &s[col_end..];
        let row_abs = r_part.starts_with('$');
        if row_abs {
            r_part = &r_part[1..];
        }
        if r_part.is_empty() || !r_part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (r, c) = crate::sparse::parse_ref(&format!("{col_str}{r_part}"))?;
        let (nr, nc) = if rows {
            (shift.map_bounded(r)?, c)
        } else {
            (r, shift.map_bounded(c)?)
        };
        let mut out = String::new();
        if col_abs {
            out.push('$');
        }
        out.push_str(&crate::model::col_label(nc));
        if row_abs {
            out.push('$');
        }
        out.push_str(&(nr + 1).to_string());
        Some(out)
    };
    let mapped = if let Some((a, b)) = range_part.split_once(':') {
        format!("{}:{}", map_token(a)?, map_token(b)?)
    } else {
        map_token(range_part)?
    };
    Some(match sheet_prefix {
        Some(sh) => format!("{hash_prefix}{sh}!{mapped}"),
        None => format!("{hash_prefix}{mapped}"),
    })
}

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

fn shift_axis(v: u32, at: u32, n: u32, del: bool, limit: u32) -> Option<u32> {
    if v >= limit {
        return None;
    }
    if del {
        if v < at {
            Some(v)
        } else if v < at.saturating_add(n) {
            None // 被删掉
        } else {
            Some(v - n)
        }
    } else if v >= at {
        v.checked_add(n).filter(|&nv| nv < limit)
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
    crate::transform::formula_may_be_affected_from(formula, shift.axis, min_at)
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
                if rule
                    .kind
                    .rewrite_formulas(|f| crate::refs::rewrite_mapped(f, shift))
                {
                    self.cf_dirty = true;
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
        let orphans = self.shift_array_and_hyperlink_ranges(&shift);
        self.clear_orphan_spill_tails(&orphans);
        match op {
            Structural::InsertRows { at, n } => {
                self.merges = self
                    .merges
                    .iter()
                    .filter_map(|&(r1, c1, r2, c2)| {
                        let (nr1, nr2) = shift.map_range_bounded(r1, r2)?;
                        Some((nr1, c1, nr2, c2))
                    })
                    .collect();
                self.shift_notes(&shift);
                self.shift_cf(&shift);
                self.shift_dv(&shift);
                self.shift_sparklines(&shift);
                if let Some((r, c)) = self.freeze {
                    let nr = if r > *at {
                        r.saturating_add(*n).min(crate::sync::MAX_ROWS - 1)
                    } else {
                        r
                    };
                    self.freeze = Some((nr, c));
                }
                self.layout_dirty |= self.rows.shift_axis_bounded(*at, *n, false, crate::sync::MAX_ROWS);
                self.shift_filter(&shift);
                self.adjust_tables_for_row_insert(*at, *n);
                self.shift_objects_and_page(&shift);
                self.shift_axis_metadata(&shift);
            }
            Structural::DeleteRows { at, n } => {
                self.merges = self
                    .merges
                    .iter()
                    .filter_map(|&(r1, c1, r2, c2)| {
                        let (a, b) = shift.map_range_bounded(r1, r2)?;
                        (b > a || c2 > c1).then_some((a, c1, b, c2))
                    })
                    .collect();
                self.shift_notes(&shift);
                self.shift_cf(&shift);
                self.shift_dv(&shift);
                self.shift_sparklines(&shift);
                if let Some((fr, fc)) = self.freeze {
                    let nf = if fr >= at.saturating_add(*n) { fr - *n } else { (*at).min(fr) };
                    self.freeze = Some((nf, fc));
                }
                self.layout_dirty |= self.rows.shift_axis_bounded(*at, *n, true, crate::sync::MAX_ROWS);
                self.shift_filter(&shift);
                self.shift_tables(&shift);
                self.shift_objects_and_page(&shift);
                self.shift_axis_metadata(&shift);
            }
            Structural::InsertCols { at, n } => {
                self.merges = self
                    .merges
                    .iter()
                    .filter_map(|&(r1, c1, r2, c2)| {
                        let (nc1, nc2) = shift.map_range_bounded(c1, c2)?;
                        Some((r1, nc1, r2, nc2))
                    })
                    .collect();
                self.shift_notes(&shift);
                self.shift_cf(&shift);
                self.shift_dv(&shift);
                self.shift_sparklines(&shift);
                if let Some((fr, fc)) = self.freeze {
                    if fc > *at {
                        self.freeze = Some((fr, fc.saturating_add(*n).min(crate::sync::MAX_COLS - 1)));
                    }
                }
                self.layout_dirty |= self.cols.shift_axis_bounded(*at, *n, false, crate::sync::MAX_COLS);
                self.shift_filter(&shift);
                self.shift_tables(&shift);
                self.shift_objects_and_page(&shift);
                self.shift_axis_metadata(&shift);
            }
            Structural::DeleteCols { at, n } => {
                self.merges = self
                    .merges
                    .iter()
                    .filter_map(|&(r1, c1, r2, c2)| {
                        let (a, b) = shift.map_range_bounded(c1, c2)?;
                        (r2 > r1 || b > a).then_some((r1, a, r2, b))
                    })
                    .collect();
                self.shift_notes(&shift);
                self.shift_cf(&shift);
                self.shift_dv(&shift);
                self.shift_sparklines(&shift);
                if let Some((fr, fc)) = self.freeze {
                    let nf = if fc >= at.saturating_add(*n) { fc - *n } else { (*at).min(fc) };
                    self.freeze = Some((fr, nf));
                }
                self.layout_dirty |= self.cols.shift_axis_bounded(*at, *n, true, crate::sync::MAX_COLS);
                self.shift_filter(&shift);
                self.shift_tables(&shift);
                self.shift_objects_and_page(&shift);
                self.shift_axis_metadata(&shift);
            }
        }
        self.merges_dirty = true;
        self.freeze_dirty = true;
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
    /// 返回已映射到新坐标系的断尾溢出矩形，必须在单元格坐标搬移完成后再清理。
    fn shift_array_and_hyperlink_ranges(&mut self, shift: &MappedShift) -> Vec<(u32, u32, u32, u32)> {
        if shift.n != 0 { self.coordinates_shifted(); }
        let rows = shift.axis == crate::refs::Axis::Row;
        if shift.n == 0 { return Vec::new(); }
        let mut orphan_tails: Vec<(u32, u32, u32, u32)> = Vec::new();
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
                        (shift.map_bounded(key.row)?, key.col)
                    } else {
                        (key.row, shift.map_bounded(key.col)?)
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
            for (&anchor, &(h, w)) in &self.spill_shapes {
                let mapped_anchor = if rows {
                    shift.map_bounded(anchor.row).map(|r| CellRef::new(r, anchor.col))
                } else {
                    shift.map_bounded(anchor.col).map(|c| CellRef::new(anchor.row, c))
                };
                let Some(new_anchor) = mapped_anchor else {
                    if h > 0 && w > 0 {
                        let (r1, c1) = (anchor.row, anchor.col);
                        let (r2, c2) = (
                            r1.saturating_add(h.saturating_sub(1)),
                            c1.saturating_add(w.saturating_sub(1)),
                        );
                        if rows {
                            if let Some((nr1, nr2)) = shift.map_range_bounded(r1, r2) {
                                orphan_tails.push((nr1, c1, nr2, c2));
                            }
                        } else if let Some((nc1, nc2)) = shift.map_range_bounded(c1, c2) {
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
                        .map_range_bounded(anchor.row, end)
                        .is_some_and(|(nr1, nr2)| nr1 == new_anchor.row && nr2 - nr1 + 1 == h)
                } else {
                    let end = anchor.col.saturating_add(w.saturating_sub(1));
                    shift
                        .map_range_bounded(anchor.col, end)
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
                        if let Some((nr1, nr2)) = shift.map_range_bounded(r1, r2) {
                            orphan_tails.push((nr1, c1, nr2, c2));
                        }
                    } else if let Some((nc1, nc2)) = shift.map_range_bounded(c1, c2) {
                        orphan_tails.push((r1, nc1, r2, nc2));
                    }
                    next_shapes.insert(new_anchor, (0, 0));
                }
            }
            self.spill_shapes = next_shapes;
        }
        if !self.hyperlinks.is_empty() {
            let own_name = self.name.clone();
            let need_shift_keys = self.hyperlinks.iter().any(|(&(r, c), link)| {
                let end = if rows {
                    r.saturating_add(link.span.0)
                } else {
                    c.saturating_add(link.span.1)
                };
                end >= min_at
            });
            let need_shift_locs = self
                .hyperlinks
                .values()
                .any(|link| !link.location.is_empty());
            if need_shift_keys || need_shift_locs {
                let old = std::mem::take(&mut self.hyperlinks);
                let mut next = std::collections::HashMap::with_capacity(old.len());
                for ((r, c), mut link) in old {
                    if !link.location.is_empty() {
                        if let Some(updated_loc) = shift_internal_link_location(&link.location, &own_name, shift, rows) {
                            if updated_loc != link.location {
                                link.location = updated_loc;
                                self.hyperlinks_dirty = true;
                            }
                        }
                    }
                    let (start, span) = if rows { (r, link.span.0) } else { (c, link.span.1) };
                    let end = start.saturating_add(span);
                    let moved = shift.map_range_bounded(start, end);
                    self.hyperlinks_dirty |= moved != Some((start, end));
                    let Some((first, last)) = moved else { continue };
                    let anchor = if rows {
                        link.span.0 = last - first;
                        (first, c)
                    } else {
                        link.span.1 = last - first;
                        (r, first)
                    };
                    next.insert(anchor, link);
                }
                self.hyperlinks = next.into();
            }
        }
        if !self.arrays.is_empty() {
            self.arrays = self
                .arrays
                .iter()
                .filter_map(|&(anchor, (r1, c1, r2, c2))| {
                    if rows {
                        let row = shift.map_bounded(anchor.row)?;
                        let (r1, r2) = shift.map_range_bounded(r1, r2)?;
                        Some((CellRef::new(row, anchor.col), (r1, c1, r2, c2)))
                    } else {
                        let col = shift.map_bounded(anchor.col)?;
                        let (c1, c2) = shift.map_range_bounded(c1, c2)?;
                        Some((CellRef::new(anchor.row, col), (r1, c1, r2, c2)))
                    }
                })
                .collect();
        }
        orphan_tails
    }

    /// 清理被结构性插删切断或失去锚点的旧溢出尾格（必须在单元格完成坐标搬移后调用）。
    fn clear_orphan_spill_tails(&mut self, orphan_tails: &[(u32, u32, u32, u32)]) {
        for &(r1, c1, r2, c2) in orphan_tails {
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

    fn shift_cells(&mut self, rows: bool, at: u32, n: u32, delete: bool) {
        let limit = if rows {
            crate::sync::MAX_ROWS
        } else {
            crate::sync::MAX_COLS
        };
        self.remap_cell_axis(rows, at, |pos| shift_axis(pos, at, n, delete, limit));
    }

    /// 把 [from, from+n) 这段整行移动到 `to`（`to` 是「删掉源行之后」的插入位置，
    /// 也就是拖放时画出的那条插入线的位置）。
    pub fn move_rows(&mut self, from: u32, n: u32, to: u32) {
        let Some(mv) = crate::refs::AxisMove::from_drop(crate::refs::Axis::Row, from, n, to) else {
            return;
        };
        self.move_axis_inner(mv, true);
    }

    /// 列版本的移动，语义同 `move_rows`
    pub fn move_cols(&mut self, from: u32, n: u32, to: u32) {
        let Some(mv) = crate::refs::AxisMove::from_drop(crate::refs::Axis::Col, from, n, to) else {
            return;
        };
        self.move_axis_inner(mv, true);
    }

    /// Single-pass lossless axis block permutation (`[from, from+n) -> [at, at+n)`)
    /// covering every coordinate-bearing field on `Sheet` without destructive
    /// delete+insert passes.
    pub(crate) fn move_axis_inner(&mut self, mv: crate::refs::AxisMove, rewrite_own_refs: bool) {
        let (axis, from, n, at) = (mv.axis, mv.from, mv.n, mv.at);
        if n == 0 || from == at {
            return;
        }
        let rows = axis == crate::refs::Axis::Row;
        let lo = from.min(at);
        let hi = (from + n).max(at + n);
        let end = from + n - 1;
        let limit = if rows {
            crate::sync::MAX_ROWS
        } else {
            crate::sync::MAX_COLS
        };
        let perm = |v: u32| crate::refs::permute_index(v, from, n, at);
        let transform = crate::transform::CoordTransform::from_raw_axis_move(0, mv);
        let del_sh = (if rows {
            Structural::DeleteRows { at: from, n }
        } else {
            Structural::DeleteCols { at: from, n }
        })
        .reference_shift();
        let ins_sh = (if rows {
            Structural::InsertRows { at, n }
        } else {
            Structural::InsertCols { at, n }
        })
        .reference_shift();
        let del_ms = MappedShift { shift: &del_sh, insertions: &[] };
        let ins_ms = MappedShift { shift: &ins_sh, insertions: &[] };
        let map_anchor_fallback = |v: u32| -> u32 {
            let after_del = del_ms.map(v).unwrap_or(from);
            ins_ms.map(after_del).unwrap_or(after_del)
        };

        self.coordinates_shifted();

        // 1. Pre-register legacy CSE array tails and permute spilled / spill_shapes / arrays.
        let mut orphan_tails: Vec<(u32, u32, u32, u32)> = Vec::new();
        if !self.arrays.is_empty() {
            for &(anchor, (r1, c1, r2, c2)) in &self.arrays {
                let (a, b) = if rows { (r1, r2) } else { (c1, c2) };
                if b < lo || a >= hi {
                    continue;
                }
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        let key = CellRef::new(r, c);
                        if key != anchor && self.get(r, c).is_none_or(|cell| cell.formula.is_none())
                        {
                            self.spilled.insert(key);
                        }
                    }
                }
            }
        }
        if !self.spilled.is_empty()
            && self.spilled.iter().any(|k| {
                let v = if rows { k.row } else { k.col };
                v >= lo && v < hi
            })
        {
            self.spilled = self
                .spilled
                .iter()
                .map(|k| {
                    if rows {
                        CellRef::new(perm(k.row), k.col)
                    } else {
                        CellRef::new(k.row, perm(k.col))
                    }
                })
                .collect();
        }
        if !self.spill_shapes.is_empty()
            && self.spill_shapes.iter().any(|(k, &(h, w))| {
                let (s, e) = if rows {
                    (k.row, k.row.saturating_add(h.saturating_sub(1)))
                } else {
                    (k.col, k.col.saturating_add(w.saturating_sub(1)))
                };
                e >= lo && s < hi
            })
        {
            let mut next_shapes = crate::model::CellMetadata::default();
            for (&anchor, &(h, w)) in &self.spill_shapes {
                let new_anchor = if rows {
                    CellRef::new(perm(anchor.row), anchor.col)
                } else {
                    CellRef::new(anchor.row, perm(anchor.col))
                };
                if h == 0 || w == 0 {
                    next_shapes.insert(new_anchor, (0, 0));
                    continue;
                }
                let r1 = anchor.row;
                let c1 = anchor.col;
                let r2 = r1.saturating_add(h.saturating_sub(1));
                let c2 = c1.saturating_add(w.saturating_sub(1));
                let intact = transform.map_rect((r1, c1, r2, c2)).is_some_and(
                    |(nr1, nc1, nr2, nc2)| {
                        nr1 == new_anchor.row
                            && nc1 == new_anchor.col
                            && nr2 - nr1 + 1 == h
                            && nc2 - nc1 + 1 == w
                    },
                );
                if intact {
                    next_shapes.insert(new_anchor, (h, w));
                } else {
                    let (a, b) = if rows { (r1, r2) } else { (c1, c2) };
                    for (pa, pb) in crate::transform::permute_axis_range_split(a, b, from, n, at) {
                        if rows {
                            orphan_tails.push((pa, c1, pb, c2));
                        } else {
                            orphan_tails.push((r1, pa, r2, pb));
                        }
                    }
                    next_shapes.insert(new_anchor, (0, 0));
                }
            }
            self.spill_shapes = next_shapes;
        }
        if !self.arrays.is_empty() {
            self.arrays = self
                .arrays
                .iter()
                .filter_map(|&(anchor, rect)| {
                    let new_anchor = if rows {
                        CellRef::new(perm(anchor.row), anchor.col)
                    } else {
                        CellRef::new(anchor.row, perm(anchor.col))
                    };
                    let new_rect = transform.map_rect(rect)?;
                    (new_anchor.row >= new_rect.0
                        && new_anchor.row <= new_rect.2
                        && new_anchor.col >= new_rect.1
                        && new_anchor.col <= new_rect.3)
                        .then_some((new_anchor, new_rect))
                })
                .collect();
        }

        // 2. Single-pass bounded `[lo..hi)` permutation on cells and explicit styles.
        self.permute_cell_band(rows, lo, hi, perm);
        self.clear_orphan_spill_tails(&orphan_tails);

        // 3. Merges.
        if !self.merges.is_empty() {
            let mut changed = false;
            let mut next = Vec::with_capacity(self.merges.len());
            for &(r1, c1, r2, c2) in self.merges.iter() {
                let mapped = if let Some(nm) = transform.map_rect((r1, c1, r2, c2)) {
                    Some(nm)
                } else if rows {
                    del_ms
                        .map_range(r1, r2)
                        .and_then(|(a, b)| ins_ms.map_range(a, b))
                        .filter(|&(a, b)| b > a || c2 > c1)
                        .map(|(a, b)| (a, c1, b, c2))
                } else {
                    del_ms
                        .map_range(c1, c2)
                        .and_then(|(a, b)| ins_ms.map_range(a, b))
                        .filter(|&(a, b)| r2 > r1 || b > a)
                        .map(|(a, b)| (r1, a, r2, b))
                };
                if let Some(m) = mapped {
                    if m != (r1, c1, r2, c2) {
                        changed = true;
                    }
                    if !next.contains(&m) {
                        next.push(m);
                    }
                } else {
                    changed = true;
                }
            }
            if changed {
                self.merges = next.into();
                self.merges_dirty = true;
            }
        }

        // 4. Notes, threads, rich text, ignored errors (exact bijective point permutation).
        if !self.notes.is_empty()
            && self.notes.keys().any(|k| {
                let v = if rows { k.row } else { k.col };
                v >= lo && v < hi
            })
        {
            let old = std::mem::take(&mut self.notes);
            let mut out = std::collections::HashMap::with_capacity(old.len());
            for (k, mut v) in old {
                let nk = if rows {
                    CellRef::new(perm(k.row), k.col)
                } else {
                    CellRef::new(k.row, perm(k.col))
                };
                v.r = nk.row;
                v.c = nk.col;
                out.insert(nk, v);
            }
            self.notes = out.into();
            self.notes_dirty = true;
        }
        if !self.threads.is_empty()
            && self.threads.keys().any(|k| {
                let v = if rows { k.row } else { k.col };
                v >= lo && v < hi
            })
        {
            let old = std::mem::take(&mut self.threads);
            let mut out = std::collections::HashMap::with_capacity(old.len());
            for (k, mut v) in old {
                let nk = if rows {
                    CellRef::new(perm(k.row), k.col)
                } else {
                    CellRef::new(k.row, perm(k.col))
                };
                v.r = nk.row;
                v.c = nk.col;
                out.insert(nk, v);
            }
            self.threads = out;
            self.threads_dirty = true;
        }
        if !self.rich.is_empty()
            && self.rich.keys().any(|&(r, c)| {
                let v = if rows { r } else { c };
                v >= lo && v < hi
            })
        {
            let old = std::mem::take(&mut self.rich);
            let mut out = std::collections::HashMap::with_capacity(old.len());
            for ((r, c), runs) in old {
                let nk = if rows { (perm(r), c) } else { (r, perm(c)) };
                out.insert(nk, runs);
            }
            self.rich = out.into();
            self.rich_dirty = true;
        }
        if !self.ignored_errors.is_empty()
            && self.ignored_errors.iter().any(|&(r, c)| {
                let v = if rows { r } else { c };
                v >= lo && v < hi
            })
        {
            let old = std::mem::take(&mut self.ignored_errors);
            let mut out = HashSet::with_capacity(old.len());
            for (r, c) in old {
                let nk = if rows { (perm(r), c) } else { (r, perm(c)) };
                out.insert(nk);
            }
            self.ignored_errors = out.into();
            self.ignored_errors_dirty = true;
        }

        // 5. Hyperlinks (anchors, spans, and internal `location`).
        if !self.hyperlinks.is_empty() {
            let own_name = self.name.clone();
            let need_keys = self.hyperlinks.iter().any(|(&(r, c), link)| {
                let (s, e) = if rows {
                    (r, r.saturating_add(link.span.0))
                } else {
                    (c, c.saturating_add(link.span.1))
                };
                e >= lo && s < hi
            });
            let need_locs = self.hyperlinks.values().any(|l| !l.location.is_empty());
            if need_keys || need_locs {
                let old = std::mem::take(&mut self.hyperlinks);
                let mut next = std::collections::HashMap::with_capacity(old.len());
                let mut changed = false;
                for ((r, c), mut link) in old {
                    if !link.location.is_empty()
                        && let Some(updated) = crate::transform::rewrite_internal_link_location(
                            &link.location,
                            &own_name,
                            &transform,
                        )
                        && updated != link.location
                    {
                        link.location = updated;
                        changed = true;
                    }
                    let nk = if link.span == (0, 0) {
                        if rows { (perm(r), c) } else { (r, perm(c)) }
                    } else {
                        let rect = (r, c, r.saturating_add(link.span.0), c.saturating_add(link.span.1));
                        if let Some((nr1, nc1, nr2, nc2)) = transform.map_rect(rect) {
                            link.span = (nr2 - nr1, nc2 - nc1);
                            (nr1, nc1)
                        } else {
                            link.span = (0, 0);
                            changed = true;
                            if rows { (perm(r), c) } else { (r, perm(c)) }
                        }
                    };
                    if nk != (r, c) {
                        changed = true;
                    }
                    next.insert(nk, link);
                }
                self.hyperlinks = next.into();
                self.hyperlinks_dirty |= changed;
            }
        }

        // 6. Conditional formatting & data validation ranges (exact cell-set preservation).
        let permute_rect_list = |ranges: &mut Vec<(u32, u32, u32, u32)>| -> bool {
            let mut changed = false;
            let mut out = Vec::with_capacity(ranges.len());
            for &(r1, c1, r2, c2) in ranges.iter() {
                let (a, b) = if rows { (r1, r2) } else { (c1, c2) };
                if a == 0 && b == limit - 1 {
                    out.push((r1, c1, r2, c2));
                    continue;
                }
                let pieces = crate::transform::permute_axis_range_split(a, b, from, n, at);
                if pieces.len() == 1 && pieces[0] == (a.min(b), a.max(b)) {
                    out.push((r1, c1, r2, c2));
                } else {
                    changed = true;
                    for (pa, pb) in pieces {
                        let nr = if rows { (pa, c1, pb, c2) } else { (r1, pa, r2, pb) };
                        if !out.contains(&nr) {
                            out.push(nr);
                        }
                    }
                }
            }
            if changed {
                *ranges = out;
            }
            changed
        };
        for blk in &mut self.cf.blocks {
            if permute_rect_list(&mut blk.ranges) {
                self.cf_dirty = true;
            }
        }
        for rule in &mut self.dv {
            if permute_rect_list(&mut rule.ranges) {
                self.dv_dirty = true;
            }
        }

        // 7. Target-sheet sparklines (anchors & unqualified same-sheet data rects).
        if !self.sparklines.is_empty() {
            let mut changed = false;
            for sp in &mut self.sparklines {
                if rows {
                    let nr = perm(sp.r);
                    if nr != sp.r {
                        sp.r = nr;
                        changed = true;
                    }
                } else {
                    let nc = perm(sp.c);
                    if nc != sp.c {
                        sp.c = nc;
                        changed = true;
                    }
                }
                if sp.source_sheet.is_none() {
                    match transform.map_rect(sp.rect) {
                        Some(nr) => {
                            if nr != sp.rect {
                                sp.rect = nr;
                                changed = true;
                            }
                        }
                        None => {
                            sp.source_sheet = Some(String::new());
                            sp.rect = (0, 0, 0, 0);
                            changed = true;
                        }
                    }
                }
            }
            self.sparklines_dirty |= changed;
        }

        // 8. Protected ranges & scenarios.
        if !self.protected_ranges.is_empty() {
            let mut changed = false;
            for pr in &mut self.protected_ranges {
                changed |= permute_rect_list(&mut pr.sqref);
            }
            self.protected_ranges_dirty |= changed;
        }
        crate::scenarios::transform_references(self, &transform);

        // 9. Axis layout: sizes, hidden intervals, outline levels, row/col default styles.
        {
            let ax = if rows { &mut self.rows } else { &mut self.cols };
            let moved_sizes: Vec<(u32, f32)> = ax
                .spans()
                .iter()
                .filter(|sp| sp.end >= from && sp.start <= end)
                .flat_map(|sp| {
                    let s = sp.start.max(from);
                    let e = sp.end.min(end);
                    let sz = sp.size;
                    (s..=e).map(move |idx| (at + (idx - from), sz))
                })
                .collect();
            let mut ax_changed = ax.shift_axis(from, n, true);
            ax_changed |= ax.shift_axis(at, n, false);
            if !moved_sizes.is_empty() {
                ax.set_many(moved_sizes);
                ax_changed = true;
            }
            self.layout_dirty |= ax_changed;
        }
        let permute_hidden = |hidden: &mut crate::layout::HiddenIndices| -> bool {
            if hidden.is_empty() || !hidden.intersects_range(lo, hi - 1) {
                return false;
            }
            let mut mapped = Vec::with_capacity(hidden.intervals().len() + 2);
            for &(a, b) in hidden.intervals() {
                mapped.extend(crate::transform::permute_axis_range_split(a, b, from, n, at));
            }
            let next = crate::layout::HiddenIndices::from_intervals(mapped);
            if next != *hidden {
                *hidden = next;
                true
            } else {
                false
            }
        };
        if rows {
            self.layout_dirty |= permute_hidden(&mut self.hidden_rows);
            self.layout_dirty |= permute_hidden(&mut self.hidden_rows_user);
        } else {
            self.layout_dirty |= permute_hidden(&mut self.hidden_cols);
        }
        let levels = if rows { &mut self.row_levels } else { &mut self.col_levels };
        if !levels.is_empty() && levels.keys().any(|&p| p >= lo && p < hi) {
            *levels = std::mem::take(levels)
                .into_iter()
                .map(|(p, lv)| (perm(p), lv))
                .collect();
            self.layout_dirty = true;
        }
        let axis_styles = if rows { &mut self.row_styles } else { &mut self.col_styles };
        if !axis_styles.is_empty() && axis_styles.keys().any(|&p| p >= lo && p < hi) {
            let old = std::mem::take(axis_styles);
            let mut out = std::collections::HashMap::with_capacity(old.len());
            for (p, st) in old {
                out.insert(perm(p), st);
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

        // 10. Freeze pane.
        if let Some((fr, fc)) = self.freeze {
            let next = if rows {
                let after_del = if fr >= from + n { fr - n } else { from.min(fr) };
                let after_ins = if after_del > at { after_del + n } else { after_del };
                (after_ins, fc)
            } else {
                let after_del = if fc >= from + n { fc - n } else { from.min(fc) };
                let after_ins = if after_del > at { after_del + n } else { after_del };
                (fr, after_ins)
            };
            if next != (fr, fc) {
                self.freeze = Some(next);
                self.freeze_dirty = true;
            }
        }

        // 11. AutoFilter.
        if let Some(f) = &mut self.filter {
            if let Some(nr) = transform.map_rect(f.range) {
                if nr != f.range {
                    f.range = nr;
                    self.filter_dirty = true;
                }
                if !rows {
                    if !f.allows.is_empty() {
                        f.allows = std::mem::take(&mut f.allows)
                            .into_iter()
                            .map(|(c, v)| (perm(c), v))
                            .collect();
                        self.filter_dirty = true;
                    }
                    if !f.criteria.is_empty() {
                        f.criteria = std::mem::take(&mut f.criteria)
                            .into_iter()
                            .map(|(c, v)| (perm(c), v))
                            .collect();
                        self.filter_dirty = true;
                    }
                    if !f.date_groups.is_empty() {
                        f.date_groups = std::mem::take(&mut f.date_groups)
                            .into_iter()
                            .map(|(c, v)| (perm(c), v))
                            .collect();
                        self.filter_dirty = true;
                    }
                    if !f.color_filters.is_empty() {
                        f.color_filters = std::mem::take(&mut f.color_filters)
                            .into_iter()
                            .map(|(c, v)| (perm(c), v))
                            .collect();
                        self.filter_dirty = true;
                    }
                    if !f.top_filters.is_empty() {
                        f.top_filters = std::mem::take(&mut f.top_filters)
                            .into_iter()
                            .map(|(c, v)| (perm(c), v))
                            .collect();
                        self.filter_dirty = true;
                    }
                    if !f.dynamic_filters.is_empty() {
                        f.dynamic_filters = std::mem::take(&mut f.dynamic_filters)
                            .into_iter()
                            .map(|(c, v)| (perm(c), v))
                            .collect();
                        self.filter_dirty = true;
                    }
                }
            } else {
                self.shift_filter(&del_ms);
                self.shift_filter(&ins_ms);
            }
        }

        // 12. Tables.
        if !self.tables.is_empty() {
            let mut changed = false;
            let mut kept = Vec::with_capacity(self.tables.len());
            let mut straddling = Vec::new();
            for mut t in std::mem::take(&mut self.tables) {
                let (_, c1, _, c2) = t.range;
                if let Some(nr) = transform.map_rect(t.range) {
                    if !rows
                        && lo >= c1
                        && hi <= c2 + 1
                        && t.columns.len() == (c2 - c1 + 1) as usize
                    {
                        let mut reordered = vec![String::new(); t.columns.len()];
                        for (i, col_name) in t.columns.iter().enumerate() {
                            let dest = (perm(c1 + i as u32) - nr.1) as usize;
                            reordered[dest] = col_name.clone();
                        }
                        if reordered != t.columns {
                            t.columns = reordered;
                            changed = true;
                        }
                    }
                    if nr != t.range {
                        t.range = nr;
                        changed = true;
                    }
                    kept.push(t);
                } else {
                    straddling.push(t);
                }
            }
            if !straddling.is_empty() {
                self.tables = straddling;
                self.shift_tables(&del_ms);
                if rows {
                    self.adjust_tables_for_row_insert(at, n);
                } else {
                    self.shift_tables(&ins_ms);
                }
                kept.extend(std::mem::take(&mut self.tables));
                changed = true;
            }
            self.tables = kept;
            self.tables_dirty |= changed;
        }

        // 13. Drawing anchors (pictures, textboxes, chart anchors) & PageSetup.
        if !self.pictures.is_empty() {
            let mut changed = false;
            for pic in &mut self.pictures {
                let next = if let Some(nr) = transform.map_rect(pic.rect) {
                    nr
                } else if rows {
                    let nr1 = map_anchor_fallback(pic.rect.0);
                    let nr2 = map_anchor_fallback(pic.rect.2).max(nr1);
                    (nr1, pic.rect.1, nr2, pic.rect.3)
                } else {
                    let nc1 = map_anchor_fallback(pic.rect.1);
                    let nc2 = map_anchor_fallback(pic.rect.3).max(nc1);
                    (pic.rect.0, nc1, pic.rect.2, nc2)
                };
                if next != pic.rect {
                    pic.rect = next;
                    changed = true;
                }
            }
            self.pictures_dirty |= changed;
        }
        if !self.textboxes.is_empty() {
            let mut changed = false;
            for tb in &mut self.textboxes {
                let (c1, dx1, r1, dy1) = tb.from;
                let (c2, dx2, r2, dy2) = tb.to;
                if let Some((nr1, nc1, nr2, nc2)) = transform.map_rect((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2))) {
                    if (nr1, nc1, nr2, nc2) != (r1, c1, r2, c2) {
                        tb.from = (nc1, dx1, nr1, dy1);
                        tb.to = (nc2, dx2, nr2, dy2);
                        changed = true;
                    }
                } else if rows {
                    let nr1 = map_anchor_fallback(r1);
                    let nr2 = map_anchor_fallback(r2).max(nr1);
                    if (nr1, nr2) != (r1, r2) {
                        tb.from = (c1, dx1, nr1, dy1);
                        tb.to = (c2, dx2, nr2, dy2);
                        changed = true;
                    }
                } else {
                    let nc1 = map_anchor_fallback(c1);
                    let nc2 = map_anchor_fallback(c2).max(nc1);
                    if (nc1, nc2) != (c1, c2) {
                        tb.from = (nc1, dx1, r1, dy1);
                        tb.to = (nc2, dx2, r2, dy2);
                        changed = true;
                    }
                }
            }
            self.textboxes_dirty |= changed;
        }
        if !self.charts.is_empty() {
            let mut changed = false;
            for ch in &mut self.charts {
                let (r1, c1, r2, c2) = ch.anchor;
                let next = if let Some(nr) = transform.map_rect((r1, c1, r2, c2)) {
                    nr
                } else if rows {
                    let nr1 = map_anchor_fallback(r1);
                    let nr2 = map_anchor_fallback(r2).max(nr1);
                    (nr1, c1, nr2, c2)
                } else {
                    let nc1 = map_anchor_fallback(c1);
                    let nc2 = map_anchor_fallback(c2).max(nc1);
                    (r1, nc1, r2, nc2)
                };
                if next != ch.anchor {
                    ch.anchor = next;
                    changed = true;
                }
            }
            self.charts_dirty |= changed;
        }
        let mut page_changed = false;
        if let Some((r1, c1, r2, c2)) = self.page.print_area {
            let next = transform.map_rect((r1, c1, r2, c2)).or_else(|| {
                if rows {
                    del_ms
                        .map_range(r1, r2)
                        .and_then(|(a, b)| ins_ms.map_range(a, b))
                        .map(|(a, b)| (a, c1, b, c2))
                } else {
                    del_ms
                        .map_range(c1, c2)
                        .and_then(|(a, b)| ins_ms.map_range(a, b))
                        .map(|(a, b)| (r1, a, r2, b))
                }
            });
            if next != self.page.print_area {
                self.page.print_area = next;
                page_changed = true;
            }
        }
        if rows {
            if let Some((a, b)) = self.page.title_rows {
                let next = transform
                    .map_range(a, b)
                    .or_else(|| del_ms.map_range(a, b).and_then(|(x, y)| ins_ms.map_range(x, y)));
                if next != self.page.title_rows {
                    self.page.title_rows = next;
                    page_changed = true;
                }
            }
            if !self.page.row_breaks.is_empty() {
                let mut next: Vec<u32> = self
                    .page
                    .row_breaks
                    .iter()
                    .map(|&b| perm(b))
                    .filter(|&b| b > 0)
                    .collect();
                next.sort_unstable();
                next.dedup();
                if next != self.page.row_breaks {
                    self.page.row_breaks = next;
                    page_changed = true;
                }
            }
        } else {
            if let Some((a, b)) = self.page.title_cols {
                let next = transform
                    .map_range(a, b)
                    .or_else(|| del_ms.map_range(a, b).and_then(|(x, y)| ins_ms.map_range(x, y)));
                if next != self.page.title_cols {
                    self.page.title_cols = next;
                    page_changed = true;
                }
            }
            if !self.page.col_breaks.is_empty() {
                let mut next: Vec<u32> = self
                    .page
                    .col_breaks
                    .iter()
                    .map(|&b| perm(b))
                    .filter(|&b| b > 0)
                    .collect();
                next.sort_unstable();
                next.dedup();
                if next != self.page.col_breaks {
                    self.page.col_breaks = next;
                    page_changed = true;
                }
            }
        }
        self.page_dirty |= page_changed;

        // 14. Standalone `Sheet::move_rows`/`move_cols` rewrites own-sheet refs immediately;
        //     `move_rows_wb`/`move_cols_wb` delegates all sheets to `CoordTransform::apply_workbook_references`.
        if rewrite_own_refs {
            let own_name = self.name.clone();
            transform.rewrite_sheet_cell_formulas(self, &own_name);
            transform.rewrite_sheet_cf_dv(self, &own_name);
            for ch in self.charts.iter_mut() {
                if ch.rewrite_with_transform(&transform, &own_name, &own_name) {
                    self.charts_dirty = true;
                }
            }
            transform.remap_sheet_qualified_sparklines(self, &own_name);
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
        let max_row = crate::sync::MAX_ROWS - 1;
        for t in &mut self.tables {
            let (r1, _, r2, _) = t.range;
            if at <= r1 {
                if let (Some(nr1), Some(nr2)) = (
                    r1.checked_add(n).filter(|&v| v <= max_row),
                    r2.checked_add(n).filter(|&v| v <= max_row),
                ) {
                    t.range.0 = nr1;
                    t.range.2 = nr2;
                    changed = true;
                }
            } else if at <= r2.saturating_add(u32::from(!t.totals_row))
                && let Some(nr2) = r2.checked_add(n).filter(|&v| v <= max_row)
            {
                t.range.2 = nr2;
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
        let orphans = self.shift_array_and_hyperlink_ranges(shift);
        if move_cells {
            self.remap_cell_axis(true, shift.at, |row| shift.map_bounded(row));
        }
        self.clear_orphan_spill_tails(&orphans);
        self.merges = self
            .merges
            .iter()
            .filter_map(|&(r1, c1, r2, c2)| {
                let (nr1, nr2) = shift.map_range_bounded(r1, r2)?;
                Some((nr1, c1, nr2, c2))
            })
            .collect();
        self.shift_notes(shift);
        self.shift_cf(shift);
        self.shift_dv(shift);
        self.shift_sparklines(shift);
        if let Some((r, c)) = self.freeze {
            let max_row = crate::sync::MAX_ROWS - 1;
            let nr = if shift.insertions.is_empty() {
                if r > shift.at {
                    r.saturating_add(shift.n).min(max_row)
                } else {
                    r
                }
            } else {
                r.saturating_add(shift.insertions.partition_point(|&at| at < r) as u32)
                    .min(max_row)
            };
            self.freeze = Some((nr, c));
        }
        self.layout_dirty |= if shift.insertions.is_empty() {
            self.rows
                .shift_axis_bounded(shift.at, shift.n, shift.delete, crate::sync::MAX_ROWS)
        } else {
            self.rows.remap_indices(|r| shift.map_bounded(r))
        };
        self.shift_filter(shift);
        self.shift_objects_and_page(shift);
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
        let orphans = self.shift_array_and_hyperlink_ranges(&shift);
        if move_cells {
            self.shift_cells(true, at, n, true);
        }
        self.clear_orphan_spill_tails(&orphans);
        self.merges = self
            .merges
            .iter()
            .filter_map(|&(r1, c1, r2, c2)| {
                let (a, b) = shift.map_range_bounded(r1, r2)?;
                (b > a || c2 > c1).then_some((a, c1, b, c2))
            })
            .collect();
        self.shift_notes(&shift);
        self.shift_cf(&shift);
        self.shift_dv(&shift);
        self.shift_sparklines(&shift);
        if let Some((fr, fc)) = self.freeze {
            let nf = if fr >= at.saturating_add(n) { fr - n } else { at.min(fr) };
            self.freeze = Some((nf, fc));
        }
        self.layout_dirty |= self
            .rows
            .shift_axis_bounded(at, n, true, crate::sync::MAX_ROWS);
        self.shift_filter(&shift);
        self.shift_tables(&shift);
        self.shift_objects_and_page(&shift);
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
        let orphans = self.shift_array_and_hyperlink_ranges(&shift);
        if move_cells {
            self.shift_cells(false, at, n, false);
        }
        self.clear_orphan_spill_tails(&orphans);
        self.merges = self
            .merges
            .iter()
            .filter_map(|&(r1, c1, r2, c2)| {
                let (nc1, nc2) = shift.map_range_bounded(c1, c2)?;
                Some((r1, nc1, r2, nc2))
            })
            .collect();
        self.shift_notes(&shift);
        self.shift_cf(&shift);
        self.shift_dv(&shift);
        self.shift_sparklines(&shift);
        if let Some((fr, fc)) = self.freeze {
            if fc > at {
                self.freeze = Some((fr, fc.saturating_add(n).min(crate::sync::MAX_COLS - 1)));
            }
        }
        self.layout_dirty |= self
            .cols
            .shift_axis_bounded(at, n, false, crate::sync::MAX_COLS);
        self.shift_filter(&shift);
        self.shift_tables(&shift);
        self.shift_objects_and_page(&shift);
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
        let orphans = self.shift_array_and_hyperlink_ranges(&shift);
        if move_cells {
            self.shift_cells(false, at, n, true);
        }
        self.clear_orphan_spill_tails(&orphans);
        self.merges = self
            .merges
            .iter()
            .filter_map(|&(r1, c1, r2, c2)| {
                let (a, b) = shift.map_range_bounded(c1, c2)?;
                (r2 > r1 || b > a).then_some((r1, a, r2, b))
            })
            .collect();
        self.shift_notes(&shift);
        self.shift_cf(&shift);
        self.shift_dv(&shift);
        self.shift_sparklines(&shift);
        if let Some((fr, fc)) = self.freeze {
            let nf = if fc >= at.saturating_add(n) { fc - n } else { at.min(fc) };
            self.freeze = Some((fr, nf));
        }
        self.layout_dirty |= self
            .cols
            .shift_axis_bounded(at, n, true, crate::sync::MAX_COLS);
        self.shift_filter(&shift);
        self.shift_tables(&shift);
        self.shift_objects_and_page(&shift);
        self.shift_axis_metadata(&shift);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    fn shift_filter(&mut self, shift: &MappedShift) {
        let Some(f) = &mut self.filter else { return };
        if shift.n == 0 {
            return;
        }
        let rows = shift.axis == crate::refs::Axis::Row;
        if rows {
            if shift.delete && f.range.0 >= shift.at && f.range.0 < shift.at.saturating_add(shift.n) {
                self.filter = None;
                self.hidden_rows.clear();
                self.filter_dirty = true;
                self.layout_dirty = true;
                return;
            }
            if let Some(moved) = shift.map_range_bounded(f.range.0, f.range.2) {
                if (f.range.0, f.range.2) != moved {
                    (f.range.0, f.range.2) = moved;
                    self.filter_dirty = true;
                }
            } else {
                self.filter = None;
                self.hidden_rows.clear();
                self.filter_dirty = true;
                self.layout_dirty = true;
            }
        } else if let Some(moved) = shift.map_range_bounded(f.range.1, f.range.3) {
            if (f.range.1, f.range.3) != moved {
                (f.range.1, f.range.3) = moved;
                self.filter_dirty = true;
            }
            if !f.allows.is_empty() {
                f.allows = std::mem::take(&mut f.allows)
                    .into_iter()
                    .filter_map(|(c, v)| Some((shift.map_bounded(c)?, v)))
                    .collect();
                self.filter_dirty = true;
            }
            if !f.criteria.is_empty() {
                f.criteria = std::mem::take(&mut f.criteria)
                    .into_iter()
                    .filter_map(|(c, v)| Some((shift.map_bounded(c)?, v)))
                    .collect();
                self.filter_dirty = true;
            }
            if !f.date_groups.is_empty() {
                f.date_groups = std::mem::take(&mut f.date_groups)
                    .into_iter()
                    .filter_map(|(c, v)| Some((shift.map_bounded(c)?, v)))
                    .collect();
                self.filter_dirty = true;
            }
            if !f.color_filters.is_empty() {
                f.color_filters = std::mem::take(&mut f.color_filters)
                    .into_iter()
                    .filter_map(|(c, v)| Some((shift.map_bounded(c)?, v)))
                    .collect();
                self.filter_dirty = true;
            }
            if !f.top_filters.is_empty() {
                f.top_filters = std::mem::take(&mut f.top_filters)
                    .into_iter()
                    .filter_map(|(c, v)| Some((shift.map_bounded(c)?, v)))
                    .collect();
                self.filter_dirty = true;
            }
            if !f.dynamic_filters.is_empty() {
                f.dynamic_filters = std::mem::take(&mut f.dynamic_filters)
                    .into_iter()
                    .filter_map(|(c, v)| Some((shift.map_bounded(c)?, v)))
                    .collect();
                self.filter_dirty = true;
            }
        } else {
            self.filter = None;
            self.hidden_rows.clear();
            self.filter_dirty = true;
            self.layout_dirty = true;
        }
    }

    fn shift_tables(&mut self, shift: &MappedShift) {
        if self.tables.is_empty() || shift.n == 0 {
            return;
        }
        let rows = shift.axis == crate::refs::Axis::Row;
        let max_col = crate::sync::MAX_COLS - 1;
        let mut changed = false;
        self.tables.retain_mut(|t| {
            let (r1, c1, r2, c2) = t.range;
            if rows {
                if let Some((nr1, nr2)) = shift.map_range_bounded(r1, r2) {
                    if (nr1, nr2) != (r1, r2) {
                        t.range.0 = nr1;
                        t.range.2 = nr2;
                        changed = true;
                    }
                    true
                } else {
                    changed = true;
                    false
                }
            } else if !shift.delete {
                if shift.at <= c1 {
                    if let (Some(nc1), Some(nc2)) = (
                        c1.checked_add(shift.n).filter(|&v| v <= max_col),
                        c2.checked_add(shift.n).filter(|&v| v <= max_col),
                    ) {
                        t.range.1 = nc1;
                        t.range.3 = nc2;
                        changed = true;
                    }
                } else if shift.at <= c2
                    && let Some(nc2) = c2.checked_add(shift.n).filter(|&v| v <= max_col)
                {
                    let insert_pos = (shift.at - c1) as usize;
                    t.range.3 = nc2;
                    for k in 0..shift.n {
                        let name = format!("列{}", t.columns.len() + 1);
                        let at = (insert_pos + k as usize).min(t.columns.len());
                        t.columns.insert(at, name);
                    }
                    changed = true;
                }
                true
            } else if let Some((nc1, nc2)) = shift.map_range_bounded(c1, c2) {
                let del_lo = shift.at;
                let del_hi = shift.at.saturating_add(shift.n);
                let mut new_cols = Vec::with_capacity(t.columns.len());
                for (i, col_name) in t.columns.iter().enumerate() {
                    let abs_c = c1 + i as u32;
                    if abs_c < del_lo || abs_c >= del_hi {
                        new_cols.push(col_name.clone());
                    }
                }
                if (nc1, nc2) != (c1, c2) || new_cols.len() != t.columns.len() {
                    t.range.1 = nc1;
                    t.range.3 = nc2;
                    t.columns = new_cols;
                    changed = true;
                }
                true
            } else {
                changed = true;
                false
            }
        });
        if changed {
            self.tables_dirty = true;
        }
    }

    fn shift_objects_and_page(&mut self, shift: &MappedShift) {
        if shift.n == 0 {
            return;
        }
        let rows = shift.axis == crate::refs::Axis::Row;
        let max_idx = shift.axis_limit() - 1;
        let map_anchor_point = |v: u32| -> u32 {
            if let Some(m) = shift.map_bounded(v) {
                m
            } else if shift.delete {
                shift.at.min(max_idx)
            } else {
                v.min(max_idx)
            }
        };
        if !self.pictures.is_empty() {
            let mut changed = false;
            for pic in &mut self.pictures {
                let (r1, c1, r2, c2) = pic.rect;
                let next = if rows {
                    let nr1 = map_anchor_point(r1);
                    let nr2 = map_anchor_point(r2).max(nr1);
                    (nr1, c1, nr2, c2)
                } else {
                    let nc1 = map_anchor_point(c1);
                    let nc2 = map_anchor_point(c2).max(nc1);
                    (r1, nc1, r2, nc2)
                };
                if next != pic.rect {
                    pic.rect = next;
                    changed = true;
                }
            }
            self.pictures_dirty |= changed;
        }
        if !self.textboxes.is_empty() {
            let mut changed = false;
            for tb in &mut self.textboxes {
                let (c1, dx1, r1, dy1) = tb.from;
                let (c2, dx2, r2, dy2) = tb.to;
                if rows {
                    let nr1 = map_anchor_point(r1);
                    let nr2 = map_anchor_point(r2).max(nr1);
                    if (nr1, nr2) != (r1, r2) {
                        tb.from = (c1, dx1, nr1, dy1);
                        tb.to = (c2, dx2, nr2, dy2);
                        changed = true;
                    }
                } else {
                    let nc1 = map_anchor_point(c1);
                    let nc2 = map_anchor_point(c2).max(nc1);
                    if (nc1, nc2) != (c1, c2) {
                        tb.from = (nc1, dx1, r1, dy1);
                        tb.to = (nc2, dx2, r2, dy2);
                        changed = true;
                    }
                }
            }
            self.textboxes_dirty |= changed;
        }
        if !self.charts.is_empty() {
            let mut changed = false;
            for ch in &mut self.charts {
                let (r1, c1, r2, c2) = ch.anchor;
                let next = if rows {
                    let nr1 = map_anchor_point(r1);
                    let nr2 = map_anchor_point(r2).max(nr1);
                    (nr1, c1, nr2, c2)
                } else {
                    let nc1 = map_anchor_point(c1);
                    let nc2 = map_anchor_point(c2).max(nc1);
                    (r1, nc1, r2, nc2)
                };
                if next != ch.anchor {
                    ch.anchor = next;
                    changed = true;
                }
            }
            self.charts_dirty |= changed;
        }
        // Page setup: print_area, title_rows/title_cols, row_breaks/col_breaks
        let mut page_changed = false;
        if let Some((r1, c1, r2, c2)) = self.page.print_area {
            let next = if rows {
                shift.map_range_bounded(r1, r2).map(|(a, b)| (a, c1, b, c2))
            } else {
                shift.map_range_bounded(c1, c2).map(|(a, b)| (r1, a, r2, b))
            };
            if next != self.page.print_area {
                self.page.print_area = next;
                page_changed = true;
            }
        }
        if rows {
            if let Some((a, b)) = self.page.title_rows {
                let next = shift.map_range_bounded(a, b);
                if next != self.page.title_rows {
                    self.page.title_rows = next;
                    page_changed = true;
                }
            }
            if !self.page.row_breaks.is_empty() {
                let next: Vec<u32> = self
                    .page
                    .row_breaks
                    .iter()
                    .filter_map(|&b| shift.map_bounded(b))
                    .filter(|&b| b > 0)
                    .collect();
                if next != self.page.row_breaks {
                    self.page.row_breaks = next;
                    self.page.row_breaks.sort_unstable();
                    self.page.row_breaks.dedup();
                    page_changed = true;
                }
            }
        } else {
            if let Some((a, b)) = self.page.title_cols {
                let next = shift.map_range_bounded(a, b);
                if next != self.page.title_cols {
                    self.page.title_cols = next;
                    page_changed = true;
                }
            }
            if !self.page.col_breaks.is_empty() {
                let next: Vec<u32> = self
                    .page
                    .col_breaks
                    .iter()
                    .filter_map(|&b| shift.map_bounded(b))
                    .filter(|&b| b > 0)
                    .collect();
                if next != self.page.col_breaks {
                    self.page.col_breaks = next;
                    self.page.col_breaks.sort_unstable();
                    self.page.col_breaks.dedup();
                    page_changed = true;
                }
            }
        }
        self.page_dirty |= page_changed;
        // Rich text runs & ignored_errors
        if !self.rich.is_empty() {
            let old = std::mem::take(&mut self.rich);
            let mut changed = false;
            let mut out = std::collections::HashMap::with_capacity(old.len());
            for ((r, c), runs) in old {
                let mapped = if rows {
                    shift.map_bounded(r).map(|nr| (nr, c))
                } else {
                    shift.map_bounded(c).map(|nc| (r, nc))
                };
                if let Some(nk) = mapped {
                    if nk != (r, c) {
                        changed = true;
                    }
                    out.insert(nk, runs);
                } else {
                    changed = true;
                }
            }
            self.rich = out.into();
            self.rich_dirty |= changed;
        }
        if !self.ignored_errors.is_empty() {
            let old = std::mem::take(&mut self.ignored_errors);
            let mut changed = false;
            let mut out = HashSet::with_capacity(old.len());
            for (r, c) in old {
                let mapped = if rows {
                    shift.map_bounded(r).map(|nr| (nr, c))
                } else {
                    shift.map_bounded(c).map(|nc| (r, nc))
                };
                if let Some(nk) = mapped {
                    if nk != (r, c) {
                        changed = true;
                    }
                    out.insert(nk);
                } else {
                    changed = true;
                }
            }
            self.ignored_errors = out.into();
            self.ignored_errors_dirty |= changed;
        }
        if !self.protected_ranges.is_empty() {
            let mut changed = false;
            for pr in &mut self.protected_ranges {
                let mut next_sq = Vec::with_capacity(pr.sqref.len());
                for &(r1, c1, r2, c2) in &pr.sqref {
                    let mapped = if rows {
                        shift.map_range_bounded(r1, r2).map(|(nr1, nr2)| (nr1, c1, nr2, c2))
                    } else {
                        shift.map_range_bounded(c1, c2).map(|(nc1, nc2)| (r1, nc1, r2, nc2))
                    };
                    if let Some(m) = mapped {
                        if m != (r1, c1, r2, c2) {
                            changed = true;
                        }
                        next_sq.push(m);
                    } else {
                        changed = true;
                    }
                }
                pr.sqref = next_sq;
            }
            self.protected_ranges.retain(|pr| !pr.sqref.is_empty());
            self.protected_ranges_dirty |= changed;
        }
    }

    fn shift_axis_metadata(&mut self, shift: &MappedShift) {
        let rows = shift.axis == crate::refs::Axis::Row;
        if shift.n == 0 { return; }
        let limit = shift.axis_limit();
        let min_at = if shift.insertions.is_empty() {
            shift.at
        } else {
            shift.insertions.first().copied().unwrap_or(shift.at)
        };
        crate::scenarios::shift_references(self, shift);
        let mut move_hidden = |hidden: &mut crate::layout::HiddenIndices| {
            if !hidden.is_empty() {
                let changed = if shift.insertions.is_empty() {
                    hidden.shift_axis_bounded(shift.at, shift.n, shift.delete, limit)
                } else {
                    hidden.remap_insertions(|pos| shift.map_bounded(pos))
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
                .filter_map(|(pos, level)| Some((shift.map_bounded(pos)?, level)))
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
            if let Some(nidx) = shift.map_bounded(idx) {
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
        if self.notes.is_empty() && self.threads.is_empty() {
            return;
        }
        let rows = shift.axis == crate::refs::Axis::Row;
        let min_at = if shift.insertions.is_empty() {
            shift.at
        } else {
            shift.insertions.first().copied().unwrap_or(shift.at)
        };
        if !self.notes.is_empty()
            && self
                .notes
                .keys()
                .any(|k| (if rows { k.row } else { k.col }) >= min_at)
        {
            let old = std::mem::take(&mut self.notes);
            let mut out = std::collections::HashMap::with_capacity(old.len());
            self.notes_dirty = true;
            for (k, mut v) in old {
                let pos = if rows { k.row } else { k.col };
                if let Some(np) = shift.map_bounded(pos) {
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
        if !self.threads.is_empty()
            && self
                .threads
                .keys()
                .any(|k| (if rows { k.row } else { k.col }) >= min_at)
        {
            let old = std::mem::take(&mut self.threads);
            let mut out = std::collections::HashMap::with_capacity(old.len());
            self.threads_dirty = true;
            for (k, mut v) in old {
                let pos = if rows { k.row } else { k.col };
                if let Some(np) = shift.map_bounded(pos) {
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
            self.threads = out;
        }
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
            let Some(na) = shift.map_bounded(anchor) else {
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
                    let Some((nr1, nr2)) = shift.map_range_bounded(r1, r2) else {
                        continue;
                    };
                    sp.rect = (nr1, c1, nr2, c2);
                } else {
                    let Some((nc1, nc2)) = shift.map_range_bounded(c1, c2) else {
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
        let Some((a, b)) = shift.map_range_bounded(*first, *last) else {
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

/// Reason a partial cell shift (`CellShift`) cannot safely execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellShiftConflict {
    InvalidTarget,
    Overflow,
    PartialMerge,
    Table,
    Pivot,
    ArrayOrSpill,
    AutoFilter,
    Chart,
    Sparkline,
    ProtectedRange,
    Scenario,
    PartialHyperlink,
}

impl CellShiftConflict {
    pub fn message(&self) -> &'static str {
        match self {
            Self::InvalidTarget => "插入/删除单元格的目标区域无效，未执行操作",
            Self::Overflow => "插入/删除区域超出工作表边界，无法移动单元格",
            Self::PartialMerge => "位移区域与合并单元格部分交叉，请先取消合并或改用整行/整列插入删除",
            Self::Table => "位移区域与表格对象相交，请改用整行/整列插入删除或先转换为普通区域",
            Self::Pivot => "位移区域与数据透视表相交，无法部分移动单元格",
            Self::ArrayOrSpill => "位移区域涉及数组公式或动态数组溢出区，无法部分移动单元格",
            Self::AutoFilter => "位移区域与自动筛选区域相交，请先关闭筛选或改用整行/整列插入删除",
            Self::Chart => "位移区域涉及图表数据源，无法部分移动单元格，请改用整行/整列插入删除",
            Self::Sparkline => "位移区域与迷你图数据源部分交叉或将导致数据源失效，请改用整行/整列插入删除",
            Self::ProtectedRange => "位移区域与允许编辑区域相交，请改用整行/整列插入删除",
            Self::Scenario => "位移区域涉及方案管理器可变单元格，无法部分移动单元格，请改用整行/整列插入删除",
            Self::PartialHyperlink => "位移区域与多格超链接区域部分交叉，请改用整行/整列插入删除",
        }
    }
}

impl std::fmt::Display for CellShiftConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// A local shift moves the selected rows or columns by the width or height of
/// the selection. The same geometry drives cell storage, references and bindings.
#[derive(Clone, Copy, Debug)]
pub struct CellShift {
    pub(crate) rect: (u32, u32, u32, u32),
    pub(crate) direction: crate::cells::Shift,
    pub(crate) insert: bool,
}

impl CellShift {
    pub fn new(rect: (u32, u32, u32, u32), direction: crate::cells::Shift, insert: bool) -> Self {
        Self { rect, direction, insert }
    }

    pub fn count(&self) -> u32 {
        let (r1, c1, r2, c2) = self.rect;
        if r1 > r2 || c1 > c2 || r2 >= crate::sync::MAX_ROWS || c2 >= crate::sync::MAX_COLS {
            return 0;
        }
        match self.direction {
            crate::cells::Shift::Vertical => r2 - r1 + 1,
            crate::cells::Shift::Horizontal => c2 - c1 + 1,
        }
    }

    pub(crate) fn moving_ray(&self) -> Option<(u32, u32, u32, u32)> {
        if self.count() == 0 {
            return None;
        }
        let (r1, c1, r2, c2) = self.rect;
        Some(match self.direction {
            crate::cells::Shift::Vertical => (r1, c1, crate::sync::MAX_ROWS - 1, c2),
            crate::cells::Shift::Horizontal => (r1, c1, r2, crate::sync::MAX_COLS - 1),
        })
    }

    pub(crate) fn preflight_objects(
        &self,
        wb: &crate::Workbook,
        sheet_idx: usize,
    ) -> Result<(), CellShiftConflict> {
        crate::participant::CoordParticipant::preflight_cell_shift_objects(wb, sheet_idx, self)
    }

    pub(crate) fn preflight_merges(&self, sheet: &Sheet) -> Result<(), CellShiftConflict> {
        crate::participant::CoordParticipant::preflight_cell_shift_merges(sheet, self)
    }

    pub(crate) fn shifted_merges(
        &self,
        merges: &[(u32, u32, u32, u32)],
    ) -> Option<Vec<(u32, u32, u32, u32)>> {
        let n = self.count();
        if n == 0 {
            return None;
        }
        let (r1, c1, r2, c2) = self.rect;
        let mut out = Vec::with_capacity(merges.len());
        for &m in merges {
            match (self.direction, self.insert) {
                (crate::cells::Shift::Vertical, true) => {
                    if m.0 >= r1 && m.1 >= c1 && m.3 <= c2 {
                        let nr2 = m.2.checked_add(n)?;
                        if nr2 >= crate::sync::MAX_ROWS {
                            return None;
                        }
                        out.push((m.0 + n, m.1, nr2, m.3));
                    } else {
                        out.push(m);
                    }
                }
                (crate::cells::Shift::Horizontal, true) => {
                    if m.0 >= r1 && m.2 <= r2 && m.1 >= c1 {
                        let nc2 = m.3.checked_add(n)?;
                        if nc2 >= crate::sync::MAX_COLS {
                            return None;
                        }
                        out.push((m.0, m.1 + n, m.2, nc2));
                    } else {
                        out.push(m);
                    }
                }
                (crate::cells::Shift::Vertical, false) => {
                    if m.1 >= c1 && m.3 <= c2 {
                        if m.0 >= r1 && m.2 < r1 + n {
                            continue;
                        }
                        if m.0 >= r1 + n {
                            out.push((m.0 - n, m.1, m.2 - n, m.3));
                            continue;
                        }
                    }
                    out.push(m);
                }
                (crate::cells::Shift::Horizontal, false) => {
                    if m.0 >= r1 && m.2 <= r2 {
                        if m.1 >= c1 && m.3 < c1 + n {
                            continue;
                        }
                        if m.1 >= c1 + n {
                            out.push((m.0, m.1 - n, m.2, m.3 - n));
                            continue;
                        }
                    }
                    out.push(m);
                }
            }
        }
        Some(out)
    }

    /// Validate that this partial cell shift can safely apply to `wb.sheets[sheet_idx]`.
    pub fn preflight(
        &self,
        wb: &crate::Workbook,
        sheet_idx: usize,
    ) -> Result<(), CellShiftConflict> {
        let tx = crate::transform::CoordTransform::from_cell_shift(sheet_idx, self);
        crate::participant::CoordParticipant::preflight_all(wb, &tx)
    }

    pub(crate) fn reference_shift(&self) -> crate::refs::Shift {
        let (r1, c1, r2, c2) = self.rect;
        let (axis, at, row_filter, col_filter) = match self.direction {
            crate::cells::Shift::Vertical => (crate::refs::Axis::Row, r1, None, Some((c1, c2))),
            crate::cells::Shift::Horizontal => (crate::refs::Axis::Col, c1, Some((r1, r2)), None),
        };
        crate::refs::Shift {
            axis,
            at,
            n: self.count(),
            delete: !self.insert,
            row_filter,
            col_filter,
            ..Default::default()
        }
    }

    /// Positions whose formula text this edit rewrites, including references outside the moved band.
    pub fn affected_formula_positions(&self, sheet: &Sheet, target_name: &str) -> Vec<(u32, u32)> {
        let transform = crate::transform::CoordTransform::from_cell_shift(0, self);
        let own_name = sheet.name.as_str();
        let affected = |(key, cell): (CellRef, &Cell)| {
            let formula = cell.formula.as_deref()?;
            (transform.formula_may_be_affected(formula)
                && transform.rewrite_formula(formula, target_name, own_name) != formula)
                .then_some((key.row, key.col))
        };
        if sheet.name.eq_ignore_ascii_case(target_name) {
            sheet.formula_cells().filter_map(affected).collect()
        } else {
            sheet.foreign_formula_cells(target_name).filter_map(affected).collect()
        }
    }

    /// Whether the edit would push occupied cells or their metadata outside the grid.
    pub fn would_overflow(&self, sheet: &Sheet) -> bool {
        let n = self.count();
        n == 0 || (self.insert && crate::cells::insert_would_overflow(sheet, self.rect, self.direction, n))
    }

    /// Returns false without changing the workbook when preflight fails.
    pub fn apply(&self, wb: &mut crate::Workbook, sheet_idx: usize) -> bool {
        if self.preflight(wb, sheet_idx).is_err() {
            return false;
        }
        let sheet = &mut wb.sheets[sheet_idx];
        let n = self.count();
        if self.insert {
            if !crate::cells::try_insert(sheet, self.rect, self.direction, n) {
                return false;
            }
        } else {
            crate::cells::delete(sheet, self.rect, self.direction, n);
        }
        self.update_references(wb, sheet_idx);
        true
    }

    fn update_references(&self, wb: &mut crate::Workbook, sheet_idx: usize) {
        let Some(target) = wb.sheets.get_mut(sheet_idx) else { return };
        if self.count() == 0 { return; }
        target.coordinates_shifted();
        crate::transform::CoordTransform::from_cell_shift(sheet_idx, self)
            .apply_workbook_references(wb);
    }
}

/// Update references and bindings after the caller has already moved the cells.
pub fn apply_cell_shift_wb(
    wb: &mut crate::model::Workbook, sheet_idx: usize, rect: (u32, u32, u32, u32),
    horizontal: bool, insert: bool,
) {
    let direction = if horizontal { crate::cells::Shift::Horizontal } else { crate::cells::Shift::Vertical };
    CellShift::new(rect, direction, insert).update_references(wb, sheet_idx);
}

pub fn apply_structural_wb(wb: &mut crate::model::Workbook, sheet_idx: usize, op: &Structural) {
    let tx = crate::transform::CoordTransform::from_structural(sheet_idx, op);
    if tx.is_noop() || tx.preflight(wb).is_err() {
        return;
    }
    let Some(sheet) = wb.sheets.get_mut(sheet_idx) else { return; };
    sheet.apply_structural(op);
    tx.apply_workbook_references(wb);
}

/// 整工作簿级行移动：先在目标表执行无损行置换，再通过统一 `CoordTransform` 调度器同步
/// 全工作簿公式、条件格式（含 CFVO）、数据验证、图表、迷你图、透视表、自定义名称与数据源。
pub fn move_rows_wb(wb: &mut crate::model::Workbook, sheet_idx: usize, from: u32, n: u32, to: u32) {
    let Some(mv) = crate::refs::AxisMove::from_drop(crate::refs::Axis::Row, from, n, to) else { return };
    if sheet_idx >= wb.sheets.len() { return; }
    wb.sheets[sheet_idx].move_axis_inner(mv, false);
    crate::transform::CoordTransform::from_raw_axis_move(sheet_idx, mv)
        .apply_workbook_references(wb);
}

/// 整工作簿级列移动：先在目标表执行无损列置换，再通过统一 `CoordTransform` 调度器同步
/// 全工作簿公式、条件格式（含 CFVO）、数据验证、图表、迷你图、透视表、自定义名称与数据源。
pub fn move_cols_wb(wb: &mut crate::model::Workbook, sheet_idx: usize, from: u32, n: u32, to: u32) {
    let Some(mv) = crate::refs::AxisMove::from_drop(crate::refs::Axis::Col, from, n, to) else { return };
    if sheet_idx >= wb.sheets.len() { return; }
    wb.sheets[sheet_idx].move_axis_inner(mv, false);
    crate::transform::CoordTransform::from_raw_axis_move(sheet_idx, mv)
        .apply_workbook_references(wb);
}

/// The final entry is the grand total and shares the last group's original
/// boundary. Table calculated columns may inherit that newly inserted subtotal
/// row, so defer only the grand total insertion when a table intersects the edit.
pub(crate) fn insert_subtotal_rows(wb: &mut crate::model::Workbook, sheet_idx: usize, positions: &[u32]) {
    let Some(&at) = positions.first() else { return };
    if crate::transform::CoordTransform::from_multi_insert(
        sheet_idx,
        crate::refs::Axis::Row,
        positions.to_vec(),
    )
    .preflight(wb)
    .is_err()
    {
        return;
    }
    if positions.len() == 1 {
        apply_structural_wb(wb, sheet_idx, &Structural::InsertRows { at, n: 1 });
        return;
    }
    let last = *positions.last().unwrap();
    let sheet = &wb.sheets[sheet_idx];
    let inherits = sheet.tables.iter().any(|t| t.range.0 < last && t.range.2.saturating_add(1) >= at);
    let grouped = if inherits { &positions[..positions.len() - 1] } else { positions };
    if !grouped.is_empty() {
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
        let max_row = crate::sync::MAX_ROWS - 1;
        for table in &mut sheet.tables {
            let (r1, _, r2, _) = table.range;
            let Some(first) = map.map_bounded(r1) else { continue };
            // With no totals row, inserting immediately after the table expands it.
            let added = grouped.partition_point(|&r| r <= r2.saturating_add(u32::from(!table.totals_row))) as u32;
            let Some(end) = r2.checked_add(added).filter(|&v| v <= max_row) else { continue };
            sheet.tables_dirty |= (first, end) != (r1, r2);
            (table.range.0, table.range.2) = (first, end);
        }
        crate::transform::CoordTransform::from_multi_insert(
            sheet_idx,
            crate::refs::Axis::Row,
            grouped.to_vec(),
        )
        .apply_workbook_references(wb);
        wb.sheets[sheet_idx].apply_table_row_fills(fills);
    }
    if inherits {
        let total = last + grouped.len() as u32;
        apply_structural_wb(wb, sheet_idx, &Structural::InsertRows { at: total, n: 1 });
    }
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
        let mut orphan_tails: Vec<(u32, u32, u32, u32)> = Vec::new();
        if !self.spill_shapes.is_empty()
            && self
                .spill_shapes
                .iter()
                .any(|(k, &(h, _))| k.row.saturating_add(h.saturating_sub(1)) >= min_at)
        {
            let mut next_shapes = crate::model::CellMetadata::default();
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
        }
        let need_shift_link_keys = self
            .hyperlinks
            .iter()
            .any(|(&(r, _), link)| r.saturating_add(link.span.0) >= min_at);
        let need_shift_link_locs = self
            .hyperlinks
            .values()
            .any(|link| !link.location.is_empty());
        if !self.hyperlinks.is_empty() && (need_shift_link_keys || need_shift_link_locs) {
            let own_name = self.name.clone();
            self.hyperlinks = std::mem::take(&mut self.hyperlinks)
                .into_iter()
                .filter_map(|((r, c), mut link)| {
                    if !link.location.is_empty() {
                        let mut updated = link.location.clone();
                        for &(lo, hi) in del_map.intervals.iter().rev() {
                            let sh = (Structural::DeleteRows {
                                at: lo,
                                n: hi - lo + 1,
                            })
                            .reference_shift();
                            let ms = MappedShift { shift: &sh, insertions: &[] };
                            if let Some(next_loc) =
                                shift_internal_link_location(&updated, &own_name, &ms, true)
                            {
                                updated = next_loc;
                            }
                        }
                        if updated != link.location {
                            link.location = updated;
                            self.hyperlinks_dirty = true;
                        }
                    }
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
        self.clear_orphan_spill_tails(&orphan_tails);

        self.merges = self
            .merges
            .iter()
            .filter_map(|&(r1, c1, r2, c2)| {
                let (a, b) = del_map.map_range(r1, r2)?;
                (b > a || c2 > c1).then_some((a, c1, b, c2))
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
        if !self.threads.is_empty() && self.threads.keys().any(|k| k.row >= min_at) {
            let old = std::mem::take(&mut self.threads);
            let mut out = std::collections::HashMap::with_capacity(old.len());
            self.threads_dirty = true;
            for (k, mut v) in old {
                if let Some(nr) = del_map.map(k.row) {
                    let key = CellRef::new(nr, k.col);
                    v.r = nr;
                    v.c = k.col;
                    out.insert(key, v);
                }
            }
            self.threads = out;
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
            let ms = MappedShift { shift: &sh, insertions: &[] };
            crate::scenarios::shift_references(self, &ms);
            self.shift_filter(&ms);
            self.shift_tables(&ms);
            self.shift_objects_and_page(&ms);
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
                if rule
                    .kind
                    .rewrite_formulas(|f| del_map.rewrite_formula(f, &own_name, &target_name))
                {
                    sheet.cf_dirty = true;
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
            if !sp
                .source_sheet
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(&target_name))
            {
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

    crate::external::remap_sources(wb, sheet_idx, crate::refs::Axis::Row, None, |lo, hi| del_map.map_range(lo, hi));
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

    let map_rect = |(r1, c1, r2, c2): (u32, u32, u32, u32)| {
        if r1 > r2 || c1 > c2 {
            return None;
        }
        del_map.map_range(r1, r2).map(|(a, b)| (a, c1, b, c2))
    };
    wb.pivots_pending.retain_mut(|(idx, spec, _, rect, _)| {
        if *idx != sheet_idx {
            return true;
        }
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
    wb.pivots_pending2.retain_mut(|(idx, spec, _, rect)| {
        if *idx != sheet_idx {
            return true;
        }
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
    let target_sid = wb.sheets[sheet_idx].identity;
    wb.pivots.retain_mut(|p| {
        let is_src = p.source_sheet_id == Some(target_sid)
            || p.source_sheet.eq_ignore_ascii_case(&target_name);
        if is_src {
            let Some(new_src) = map_rect(p.spec.source) else {
                if let Some(part) = p.imported_part.take() {
                    wb.pivots_removed_parts.push(part);
                }
                return false;
            };
            p.spec.source = new_src;
            if p.imported_part.is_none() {
                p.modified = true;
            }
        }
        if p.target_sheet == sheet_idx {
            let Some(new_out) = map_rect(p.rect) else {
                if let Some(part) = p.imported_part.take() {
                    wb.pivots_removed_parts.push(part);
                }
                return false;
            };
            p.anchor = (new_out.0, new_out.1);
            p.rect = new_out;
            p.last_meta.anchor = p.anchor;
            p.last_meta.rect = new_out;
            if p.imported_part.is_none() {
                p.modified = true;
            }
        }
        true
    });
    wb.bump_coord_epoch();
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
