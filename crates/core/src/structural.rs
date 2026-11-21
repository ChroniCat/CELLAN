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
    fn reference_shift(&self) -> crate::refs::Shift {
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
    formula: Box<str>,
    style: Option<u16>,
}

impl Sheet {
    /// 把所有公式里的引用按 shift 改写（含条件格式、数据验证里的公式）
    pub fn rewrite_references(&mut self, shift: &crate::refs::Shift) {
        self.rewrite_mapped_references(&MappedShift { shift, insertions: &[] });
    }

    fn rewrite_mapped_references(&mut self, shift: &MappedShift) {
        let rewrite = |(key, cell): (crate::model::CellRef, &crate::model::Cell)| {
            let old = cell.formula.as_deref()?;
            let new = crate::refs::rewrite_mapped(old, shift);
            (new != old).then(|| (key, new.into_boxed_str()))
        };
        let changes: Vec<_> = if shift.translate.is_none() && !shift.own_sheet.eq_ignore_ascii_case(&shift.target_sheet) {
            self.foreign_formula_cells(&shift.target_sheet).filter_map(rewrite).collect()
        } else {
            self.formula_cells().filter_map(rewrite).collect()
        };
        self.edited |= !changes.is_empty();
        for (key, formula) in changes {
            self.get_mut(key.row, key.col).unwrap().formula = Some(formula);
        }
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
                self.rows.insert_at(*at, *n);
                self.shift_axis_styles(&shift);
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
                self.rows.delete_at(*at, *n);
                self.shift_axis_styles(&shift);
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
                self.cols.insert_at(*at, *n);
                self.shift_axis_styles(&shift);
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
                self.cols.delete_at(*at, *n);
                self.shift_axis_styles(&shift);
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
        let rows = shift.axis == crate::refs::Axis::Row;
        if shift.n == 0 { return; }
        // 刚从 OOXML 读入、尚未重算时，CSE 尾格还没有 spilled 标记。
        // 先登记旧范围；即使本次删掉了锚点，剩余的缓存尾格也能被清理。
        for &(anchor, (r1, c1, r2, c2)) in &self.arrays {
            for r in r1..=r2 {
                for c in c1..=c2 {
                    let key = CellRef::new(r, c);
                    if key != anchor && self.get(r, c).is_none_or(|cell| cell.formula.is_none()) {
                        self.spilled.insert(key);
                    }
                }
            }
        }
        self.spilled = self.spilled.iter().filter_map(|key| {
            let (r, c) = if rows {
                (shift.map(key.row)?, key.col)
            } else {
                (key.row, shift.map(key.col)?)
            };
            Some(CellRef::new(r, c))
        }).collect();
        self.hyperlinks = std::mem::take(&mut self.hyperlinks).into_iter().filter_map(|((r, c), mut link)| {
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
        }).collect();
        self.arrays = self.arrays.iter().filter_map(|&(anchor, (r1, c1, r2, c2))| {
            if rows {
                let row = shift.map(anchor.row)?;
                let (r1, r2) = shift.map_range(r1, r2)?;
                Some((CellRef::new(row, anchor.col), (r1, c1, r2, c2)))
            } else {
                let col = shift.map(anchor.col)?;
                let (c1, c2) = shift.map_range(c1, c2)?;
                Some((CellRef::new(anchor.row, col), (r1, c1, r2, c2)))
            }
        }).collect();
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
        let (_, _, _, c2) = self.used_bounds().unwrap_or((0, 0, 0, 0));
        let r2 = from + n - 1;
        let mut cells: Vec<(u32, u32, Cell)> = Vec::new();
        let mut styles: Vec<(u32, u32, u16)> = Vec::new();
        for r in from..=r2 {
            for c in 0..=c2 {
                if let Some(cell) = self.get(r, c) {
                    cells.push((r - from, c, cell.clone()));
                }
                if let Some(st) = self.styles.get(&CellRef::new(r, c)).copied() {
                    styles.push((r - from, c, st));
                }
            }
        }
        let notes: Vec<_> = self
            .notes
            .iter()
            .filter(|(k, _)| k.row >= from && k.row <= r2)
            .map(|(k, v)| (k.row - from, k.col, v.clone()))
            .collect();
        let rich: Vec<_> = self
            .rich
            .iter()
            .filter(|((r, _), _)| *r >= from && *r <= r2)
            .map(|((r, c), v)| (r - from, *c, v.clone()))
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

        self.delete_rows(from, n);
        let at = if to > from { to - n } else { to };
        self.insert_rows(at, n);

        for (dr, c, cell) in cells {
            self.set(at + dr, c, cell);
        }
        for (dr, c, st) in styles {
            self.set_style(at + dr, c, st);
        }
        for (dr, c, v) in notes {
            self.notes.insert(CellRef::new(at + dr, c), v);
        }
        for (dr, c, v) in rich {
            self.rich.insert((at + dr, c), v);
            self.rich_dirty = true;
        }
        for (dr, c, v) in links {
            self.hyperlinks.insert((at + dr, c), v);
            self.hyperlinks_dirty = true;
        }
        for (i, h) in heights.into_iter().enumerate() {
            let i = i as u32;
            self.rows.set(at + i, h);
        }
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
        let (_, _, r2, _) = self.used_bounds().unwrap_or((0, 0, 0, 0));
        let c2 = from + n - 1;
        let mut cells: Vec<(u32, u32, Cell)> = Vec::new();
        let mut styles: Vec<(u32, u32, u16)> = Vec::new();
        for c in from..=c2 {
            for r in 0..=r2 {
                if let Some(cell) = self.get(r, c) {
                    cells.push((r, c - from, cell.clone()));
                }
                if let Some(st) = self.styles.get(&CellRef::new(r, c)).copied() {
                    styles.push((r, c - from, st));
                }
            }
        }
        let notes: Vec<_> = self
            .notes
            .iter()
            .filter(|(k, _)| k.col >= from && k.col <= c2)
            .map(|(k, v)| (k.row, k.col - from, v.clone()))
            .collect();
        let rich: Vec<_> = self
            .rich
            .iter()
            .filter(|((_, c), _)| *c >= from && *c <= c2)
            .map(|((r, c), v)| (*r, c - from, v.clone()))
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

        self.delete_cols(from, n);
        let at = if to > from { to - n } else { to };
        self.insert_cols(at, n);

        for (r, dc, cell) in cells {
            self.set(r, at + dc, cell);
        }
        for (r, dc, st) in styles {
            self.set_style(r, at + dc, st);
        }
        for (r, dc, v) in notes {
            self.notes.insert(CellRef::new(r, at + dc), v);
        }
        for (r, dc, v) in rich {
            self.rich.insert((r, at + dc), v);
            self.rich_dirty = true;
        }
        for (r, dc, v) in links {
            self.hyperlinks.insert((r, at + dc), v);
            self.hyperlinks_dirty = true;
        }
        for (i, w) in widths.into_iter().enumerate() {
            let i = i as u32;
            self.cols.set(at + i, w);
        }
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
                        formula: crate::refs::rewrite(formula, &shift).into_boxed_str(),
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
        if n == 0 {
            return;
        }
        let fills = self.capture_table_row_fills(at, n, &[]);
        let shift = (Structural::InsertRows { at, n }).reference_shift();
        self.insert_mapped_rows(&MappedShift { shift: &shift, insertions: &[] });
        self.adjust_tables_for_row_insert(at, n);
        self.apply_table_row_fills(fills);
    }

    fn insert_mapped_rows(&mut self, shift: &MappedShift) {
        self.shift_array_and_hyperlink_ranges(shift);
        self.remap_cell_axis(true, shift.at, |row| shift.map(row));
        for (r1, _, r2, _) in &mut self.merges {
            *r1 = shift.map(*r1).unwrap();
            *r2 = shift.map(*r2).unwrap();
        }
        self.shift_notes(shift);
        self.shift_cf(shift);
        self.shift_dv(shift);
        self.shift_sparklines(shift);
        if let Some((r, c)) = self.freeze { self.freeze = Some((shift.map(r).unwrap(), c)); }
        self.rows.remap_indices(|r| shift.map(r));
        self.shift_row_visibility(shift);
        if let Some(f) = &mut self.filter {
            let moved = shift.map_range(f.range.0, f.range.2).unwrap();
            self.filter_dirty |= (f.range.0, f.range.2) != moved;
            (f.range.0, f.range.2) = moved;
        }
        self.shift_axis_styles(shift);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    pub fn delete_rows(&mut self, at: u32, n: u32) {
        if n == 0 {
            return;
        }
        let shift = (Structural::DeleteRows { at, n }).reference_shift();
        let shift = MappedShift { shift: &shift, insertions: &[] };
        self.shift_array_and_hyperlink_ranges(&shift);
        self.shift_cells(true, at, n, true);
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
        self.rows.delete_at(at, n);
        self.shift_row_visibility(&shift);
        self.shift_axis_styles(&shift);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    pub fn insert_cols(&mut self, at: u32, n: u32) {
        if n == 0 {
            return;
        }
        let shift = (Structural::InsertCols { at, n }).reference_shift();
        let shift = MappedShift { shift: &shift, insertions: &[] };
        self.shift_array_and_hyperlink_ranges(&shift);
        self.shift_cells(false, at, n, false);
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
        self.cols.insert_at(at, n);
        self.shift_axis_styles(&shift);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    pub fn delete_cols(&mut self, at: u32, n: u32) {
        if n == 0 {
            return;
        }
        let shift = (Structural::DeleteCols { at, n }).reference_shift();
        let shift = MappedShift { shift: &shift, insertions: &[] };
        self.shift_array_and_hyperlink_ranges(&shift);
        self.shift_cells(false, at, n, true);
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
        self.cols.delete_at(at, n);
        self.shift_axis_styles(&shift);
        self.merges_dirty = true;
        self.freeze_dirty = true;
    }

    fn shift_row_visibility(&mut self, shift: &MappedShift) {
        // SUBTOTAL reads the row visibility sets. They must move with the row's
        // height or it would omit an adjacent visible record after insertion.
        for hidden in [&mut self.hidden_rows, &mut self.hidden_rows_user] {
            if hidden.is_empty() { continue; }
            *hidden = hidden.iter().filter_map(|&row| shift.map(row)).collect::<HashSet<_>>().into();
            self.layout_dirty = true;
        }
        if !self.row_levels.is_empty() {
            self.row_levels = std::mem::take(&mut self.row_levels).into_iter()
                .filter_map(|(r, level)| Some((shift.map(r)?, level))).collect();
            self.layout_dirty = true;
        }
    }

    fn shift_axis_styles(&mut self, shift: &MappedShift) {
        let rows = shift.axis == crate::refs::Axis::Row;
        if shift.n == 0 {
            return;
        }
        let map = if rows { &mut self.row_styles } else { &mut self.col_styles };
        if map.is_empty() {
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
            self.row_styles = out;
            self.row_styles_dirty = true;
        } else {
            self.col_styles = out;
            self.col_styles_dirty = true;
        }
        self.styles_dirty = true;
    }

    fn shift_notes(&mut self, shift: &MappedShift) {
        let rows = shift.axis == crate::refs::Axis::Row;
        let old = std::mem::take(&mut self.notes);
        let mut out = std::collections::HashMap::new();
        self.notes_dirty |= !old.is_empty();
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
        self.notes = out;
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
                let (a, b) = if rows {
                    let a = shift.map(r1);
                    let b = shift.map(r2);
                    (a, b)
                } else {
                    let a = shift.map(c1);
                    let b = shift.map(c2);
                    (a, b)
                };
                match (rows, a, b) {
                    // 整段都被删掉：这条迷你图没有数据了
                    (true, None, None) | (false, None, None) => continue,
                    (true, a, b) => {
                        let nr1 = a.unwrap_or(shift.at);
                        let nr2 = b.unwrap_or(nr1);
                        sp.rect = (nr1, c1, nr2.max(nr1), c2);
                    }
                    (false, a, b) => {
                        let nc1 = a.unwrap_or(shift.at);
                        let nc2 = b.unwrap_or(nc1);
                        sp.rect = (r1, nc1, r2, nc2.max(nc1));
                    }
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
        match (shift.map(*first), shift.map(*last)) {
            (Some(a), Some(b)) => { *first = a; *last = b.max(a); }
            (Some(pos), None) | (None, Some(pos)) => { *first = pos; *last = pos; }
            (None, None) => return false,
        }
        true
    });
}

/// 行高/列宽的轴：插入删除时把后面的挪一挪（保持视觉位置）
impl crate::layout::Axis {
    pub fn insert_at(&mut self, at: u32, n: u32) {
        self.remap_indices(|i| shift_axis(i, at, n, false));
    }

    pub fn delete_at(&mut self, at: u32, n: u32) {
        self.remap_indices(|i| shift_axis(i, at, n, true));
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
                let a = shift.map(r1);
                let b = shift.map(r2);
                let nr1 = a.unwrap_or(at);
                let nr2 = b.unwrap_or(nr1);
                sp.rect = (nr1, c1, nr2.max(nr1), c2);
            } else {
                let a = shift.map(c1);
                let b = shift.map(c2);
                let nc1 = a.unwrap_or(at);
                let nc2 = b.unwrap_or(nc1);
                sp.rect = (r1, nc1, r2, nc2.max(nc1));
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
    let mut out = HashSet::new();
    for (cref, _) in sheet.iter() {
        if cref.row >= r1 && cref.row <= r2 && cref.col >= c1 && cref.col <= c2 {
            out.insert(cref);
        }
    }
    out
}
