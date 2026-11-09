//! 插入 / 删除行列：整行整列搬移，并同步调整合并、批注、条件格式、数据验证、冻结、筛选。
//!
//! 存储是「分页列式」，没有真正的行插入原语：这里把受影响的格子整体搬到新位置，
//! 再重建存储。对 40 万格的表是一次性 O(n)，实测在百毫秒级。
//!
//! 工作簿级入口同时改写本表、跨表公式和名称引用。

use crate::model::{Cell, CellRef, Sheet};
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

impl Sheet {
    /// 把所有公式里的引用按 shift 改写（含条件格式、数据验证里的公式）
    pub fn rewrite_references(&mut self, shift: &crate::refs::Shift) {
        let keys: Vec<crate::model::CellRef> = self
            .iter()
            .filter(|(_, c)| c.formula.is_some())
            .map(|(k, _)| k)
            .collect();
        let mut formulas_changed = false;
        for k in keys {
            let Some(cell) = self.get_mut(k.row, k.col) else { continue };
            if let Some(f) = cell.formula.as_mut() {
                let rewritten = crate::refs::rewrite(f, shift).into_boxed_str();
                formulas_changed |= *f != rewritten;
                *f = rewritten;
            }
        }
        self.edited |= formulas_changed;
        for blk in self.cf.blocks.iter_mut() {
            for rule in blk.rules.iter_mut() {
                if let crate::cf::CfKind::Expr { formula, .. } = &mut rule.kind {
                    let rewritten = crate::refs::rewrite(formula, shift);
                    self.cf_dirty |= *formula != rewritten;
                    *formula = rewritten;
                }
            }
        }
        for dv in self.dv.iter_mut() {
            let (f1, f2) = (crate::refs::rewrite(&dv.f1, shift), crate::refs::rewrite(&dv.f2, shift));
            self.dv_dirty |= dv.f1 != f1 || dv.f2 != f2;
            (dv.f1, dv.f2) = (f1, f2);
        }
    }

    /// 只搬结构（合并/条件格式/数据验证/冻结/行列尺寸），不动格子。
    /// 加入房间时用：格子由宿主的快照给出（已经是新坐标），
    /// 再按历史搬一次就会搬两遍。
    pub fn apply_structural_structure_only(&mut self, op: &Structural) {
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
                self.shift_notes(*at, *n, true, false);
                self.shift_cf(*at, *n, true, false);
                self.shift_dv(*at, *n, true, false);
                self.rows.insert_at(*at, *n);
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
                self.shift_notes(*at, *n, true, true);
                self.shift_cf(*at, *n, true, true);
                self.shift_dv(*at, *n, true, true);
                self.rows.delete_at(*at, *n);
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
                self.shift_notes(*at, *n, false, false);
                self.shift_cf(*at, *n, false, false);
                self.shift_dv(*at, *n, false, false);
                self.cols.insert_at(*at, *n);
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
                self.shift_notes(*at, *n, false, true);
                self.shift_cf(*at, *n, false, true);
                self.shift_dv(*at, *n, false, true);
                self.cols.delete_at(*at, *n);
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

    fn rebuild(&mut self, map: impl Fn(u32, u32) -> Option<(u32, u32)>) {
        let mut items: Vec<(u32, u32, Cell)> = Vec::new();
        let old_keys: Vec<CellRef> = self.iter().map(|(key, _)| key).collect();
        for (cref, cell) in self.iter() {
            if let Some((r, c)) = map(cref.row, cref.col) {
                items.push((r, c, cell.clone()));
            }
        }
        {
            let cells = self.cells_mut();
            cells.clear();
            for (r, c, cell) in items {
                cells.set(r, c, cell);
            }
        }
        // Save overlays the original package. Remember every old coordinate;
        // writing the moved cells below will repopulate coordinates still used.
        self.cleared.extend(old_keys);
        let styles = std::mem::take(&mut self.styles);
        self.style_edits.extend(styles.keys().copied());
        for (key, style) in styles {
            if let Some((r, c)) = map(key.row, key.col) {
                let target = CellRef::new(r, c);
                self.styles.insert(target, style);
                self.style_edits.insert(target);
            }
        }
        self.edited = true;
        self.styles_dirty |= !self.style_edits.is_empty();
        self.spilled.clear();
    }

    pub fn insert_rows(&mut self, at: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.rebuild(|r, c| Some((if r >= at { r + n } else { r }, c)));
        self.merges = self
            .merges
            .iter()
            .map(|(r1, c1, r2, c2)| {
                let nr1 = if *r1 >= at { r1 + n } else { *r1 };
                let nr2 = if *r2 >= at { r2 + n } else { *r2 };
                (nr1, *c1, nr2, *c2)
            })
            .collect();
        self.shift_notes(at, n, true, false);
        self.shift_cf(at, n, true, false);
        self.shift_dv(at, n, true, false);
        if let Some((fr, fc)) = self.freeze {
            if fr >= at {
                self.freeze = Some((fr + n, fc));
            } else if fr > at {
                self.freeze = Some((at, fc));
            }
        }
        self.rows.insert_at(at, n);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    pub fn delete_rows(&mut self, at: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.rebuild(|r, c| shift_axis(r, at, n, true).map(|nr| (nr, c)));
        self.merges = self
            .merges
            .iter()
            .filter_map(|(r1, c1, r2, c2)| {
                let a = shift_axis(*r1, at, n, true)?;
                let b = shift_axis(*r2, at, n, true)?;
                Some((a, *c1, b.max(a), *c2))
            })
            .collect();
        self.shift_notes(at, n, true, true);
        self.shift_cf(at, n, true, true);
        self.shift_dv(at, n, true, true);
        if let Some((fr, fc)) = self.freeze {
            let nf = if fr >= at + n { fr - n } else { at.min(fr) };
            self.freeze = Some((nf, fc));
        }
        self.rows.delete_at(at, n);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    pub fn insert_cols(&mut self, at: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.rebuild(|r, c| Some((r, if c >= at { c + n } else { c })));
        self.merges = self
            .merges
            .iter()
            .map(|(r1, c1, r2, c2)| {
                let nc1 = if *c1 >= at { c1 + n } else { *c1 };
                let nc2 = if *c2 >= at { c2 + n } else { *c2 };
                (*r1, nc1, *r2, nc2)
            })
            .collect();
        self.shift_notes(at, n, false, false);
        self.shift_cf(at, n, false, false);
        self.shift_dv(at, n, false, false);
        if let Some((fr, fc)) = self.freeze {
            if fc >= at {
                self.freeze = Some((fr, fc + n));
            }
        }
        self.cols.insert_at(at, n);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    pub fn delete_cols(&mut self, at: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.rebuild(|r, c| shift_axis(c, at, n, true).map(|nc| (r, nc)));
        self.merges = self
            .merges
            .iter()
            .filter_map(|(r1, c1, r2, c2)| {
                let a = shift_axis(*c1, at, n, true)?;
                let b = shift_axis(*c2, at, n, true)?;
                Some((*r1, a, *r2, b.max(a)))
            })
            .collect();
        self.shift_notes(at, n, false, true);
        self.shift_cf(at, n, false, true);
        self.shift_dv(at, n, false, true);
        if let Some((fr, fc)) = self.freeze {
            let nf = if fc >= at + n { fc - n } else { at.min(fc) };
            self.freeze = Some((fr, nf));
        }
        self.cols.delete_at(at, n);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    fn shift_notes(&mut self, at: u32, n: u32, rows: bool, del: bool) {
        let old = std::mem::take(&mut self.notes);
        let mut out = std::collections::HashMap::new();
        self.notes_dirty |= !old.is_empty();
        for (k, mut v) in old {
            let pos = if rows { k.row } else { k.col };
            if let Some(np) = shift_axis(pos, at, n, del) {
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
        self.notes = out;
    }

    fn shift_cf(&mut self, at: u32, n: u32, rows: bool, del: bool) {
        self.cf_dirty |= !self.cf.blocks.is_empty();
        for blk in self.cf.blocks.iter_mut() {
            let mut keep: Vec<(u32, u32, u32, u32)> = Vec::new();
            for (r1, c1, r2, c2) in blk.ranges.iter().copied() {
                if rows {
                    match (
                        shift_axis(r1, at, n, del),
                        shift_axis(r2, at, n, del),
                    ) {
                        (Some(a), Some(b)) => keep.push((a, c1, b.max(a), c2)),
                        (Some(a), None) => keep.push((a, c1, a, c2)),
                        (None, Some(b)) => keep.push((b, c1, b, c2)),
                        (None, None) => {}
                    }
                } else {
                    match (
                        shift_axis(c1, at, n, del),
                        shift_axis(c2, at, n, del),
                    ) {
                        (Some(a), Some(b)) => keep.push((r1, a, r2, b.max(a))),
                        (Some(a), None) => keep.push((r1, a, r2, a)),
                        (None, Some(b)) => keep.push((r1, b, r2, b)),
                        (None, None) => {}
                    }
                }
            }
            blk.ranges = keep;
        }
        self.cf.blocks.retain(|b| !b.ranges.is_empty());
    }

    fn shift_dv(&mut self, at: u32, n: u32, rows: bool, del: bool) {
        self.dv_dirty |= !self.dv.is_empty();
        for dv in self.dv.iter_mut() {
            let mut keep: Vec<(u32, u32, u32, u32)> = Vec::new();
            for (r1, c1, r2, c2) in dv.ranges.iter().copied() {
                if rows {
                    match (shift_axis(r1, at, n, del), shift_axis(r2, at, n, del)) {
                        (Some(a), Some(b)) => keep.push((a, c1, b.max(a), c2)),
                        (Some(a), None) => keep.push((a, c1, a, c2)),
                        (None, Some(b)) => keep.push((b, c1, b, c2)),
                        (None, None) => {}
                    }
                } else {
                    match (shift_axis(c1, at, n, del), shift_axis(c2, at, n, del)) {
                        (Some(a), Some(b)) => keep.push((r1, a, r2, b.max(a))),
                        (Some(a), None) => keep.push((r1, a, r2, a)),
                        (None, Some(b)) => keep.push((r1, b, r2, b)),
                        (None, None) => {}
                    }
                }
            }
            dv.ranges = keep;
        }
        self.dv.retain(|d| !d.ranges.is_empty());
    }
}

/// 行高/列宽的轴：插入删除时把后面的挪一挪（保持视觉位置）
impl crate::layout::Axis {
    pub fn insert_at(&mut self, at: u32, n: u32) {
        let custom: Vec<(u32, f32)> = self.custom_pairs();
        self.clear_custom();
        for (i, w) in custom {
            let ni = if i >= at { i + n } else { i };
            self.set(ni, w);
        }
    }

    pub fn delete_at(&mut self, at: u32, n: u32) {
        let custom: Vec<(u32, f32)> = self.custom_pairs();
        self.clear_custom();
        for (i, w) in custom {
            if i < at {
                self.set(i, w);
            } else if i >= at + n {
                self.set(i - n, w);
            }
        }
    }
}

/// 整个工作簿层面的结构改动：搬格子 + 改写所有表里的公式引用
pub fn apply_structural_wb(wb: &mut crate::model::Workbook, sheet_idx: usize, op: &Structural) {
    let Some(name) = wb.sheets.get(sheet_idx).map(|s| s.name.clone()) else {
        return;
    };
    wb.sheets[sheet_idx].apply_structural(op);
    let (axis, at, n, delete) = match op {
        Structural::InsertRows { at, n } => (crate::refs::Axis::Row, *at, *n, false),
        Structural::DeleteRows { at, n } => (crate::refs::Axis::Row, *at, *n, true),
        Structural::InsertCols { at, n } => (crate::refs::Axis::Col, *at, *n, false),
        Structural::DeleteCols { at, n } => (crate::refs::Axis::Col, *at, *n, true),
    };
    for (idx, sheet) in wb.sheets.iter_mut().enumerate() {
        let shift = crate::refs::Shift {
            axis,
            at,
            n,
            delete,
            target_sheet: name.clone(),
            own_sheet: if idx == sheet_idx {
                name.clone()
            } else {
                sheet.name.clone()
            },
            translate: None,
        };
        sheet.rewrite_references(&shift);
        // 图表系列引用
        for ch in sheet.charts.iter_mut() {
            if ch.rewrite_refs(&shift) {
                sheet.charts_dirty = true;
            }
        }
    }
    // 记一笔，保存时搬透视表缓存
    wb.pivot_shifts.push((
        name.clone(),
        if axis == crate::refs::Axis::Row { 0 } else { 1 },
        at,
        n,
        delete,
    ));

    // 自定义名称
    for (_, local, text) in wb.names.iter_mut() {
        let own = match local {
            Some(i) => wb.sheets.get(*i).map(|s| s.name.clone()).unwrap_or_default(),
            None => String::new(),
        };
        let sh = crate::refs::Shift {
            axis,
            at,
            n,
            delete,
            target_sheet: name.clone(),
            own_sheet: own,
            translate: None,
        };
        let nt = crate::refs::rewrite(text, &sh);
        if nt != *text {
            *text = nt;
            wb.names_dirty = true;
        }
    }

    // 待挂的透视表：源区域直接跟着搬
    let sh = crate::refs::Shift {
        axis,
        at,
        n,
        delete,
        target_sheet: name.clone(),
        own_sheet: name.clone(),
            translate: None,
    };
    for (idx, spec, _, _, _) in wb.pivots_pending.iter_mut() {
        if *idx != sheet_idx {
            continue;
        }
        let (r1, c1, r2, c2) = spec.source;
        spec.source = match axis {
            crate::refs::Axis::Row => match sh.map_range(r1, r2) {
                Some((a, b)) => (a, c1, b, c2),
                None => spec.source,
            },
            crate::refs::Axis::Col => match sh.map_range(c1, c2) {
                Some((a, b)) => (r1, a, r2, b),
                None => spec.source,
            },
        };
    }
}

/// 供撤销用：一行/一列里有哪些格子
pub fn collect_region(sheet: &Sheet, r1: u32, c1: u32, r2: u32, c2: u32) -> HashSet<CellRef> {
    let mut out = HashSet::new();
    for (cref, _) in sheet.iter() {
        if cref.row >= r1 && cref.row <= r2 && cref.col >= c1 && cref.col <= c2 {
            out.insert(cref);
        }
    }
    out
}
