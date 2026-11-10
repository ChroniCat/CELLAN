//! 撤销/重做：记录「改之前」和「改之后」，按动作成组回退。
//!
//! 不存整本工作簿快照——152 MB 的文件拷一份就废了。每次动作只记受影响的格子，
//! 一个动作里可能有上万格（整列粘贴），但每格只存两个 `Cell`（40 字节）+ 两个样式号。
//!
//! 协作场景：撤销时如果某格当前值已经不是「我改成的样子」，说明别人后来动过，
//! 跳过它——不拿旧值盖掉别人的新编辑。

use crate::model::{Cell, CellRef, Sheet, Workbook};
use std::collections::HashSet;

/// 一格的前后状态
#[derive(Clone, Debug)]
pub struct CellDelta {
    pub sheet: usize,
    pub row: u32,
    pub col: u32,
    pub before: Cell,
    pub after: Cell,
    /// 样式号（StyleTable 里的索引，内容去重后稳定）
    pub before_style: u16,
    pub after_style: u16,
}

/// 整表快照（结构性改动用：删行之后没法靠「逐格回填」还原）
#[derive(Clone, Debug, Default)]
pub struct SheetSnapshot {
    pub sheet: usize,
    pub cells: Vec<(u32, u32, Cell, u16)>,
    pub merges: Vec<(u32, u32, u32, u32)>,
}

pub fn snapshot_sheet(sheet: &Sheet, idx: usize) -> SheetSnapshot {
    SheetSnapshot {
        sheet: idx,
        cells: sheet
            .iter()
            .map(|(k, c)| (k.row, k.col, c.clone(), sheet.style_of(k.row, k.col)))
            .collect(),
        merges: sheet.merges.clone(),
    }
}

pub fn restore_sheet(sheet: &mut Sheet, snap: &SheetSnapshot) {
    sheet.clear();
    for (r, c, cell, st) in snap.cells.iter() {
        sheet.set(*r, *c, cell.clone());
        sheet.set_style(*r, *c, *st);
    }
    sheet.merges = snap.merges.clone();
    sheet.merges_dirty = true;
}

/// 一个可撤销的动作
#[derive(Clone, Debug)]
pub struct Action {
    pub label: String,
    pub deltas: Vec<CellDelta>,
    /// 动作发生前，选区/活动格在哪（撤销后把用户送回现场）
    pub focus: (usize, u32, u32),
    /// 结构性改动：整表前后快照（表太大时留空，表示不可撤销）
    pub snap_before: Option<SheetSnapshot>,
    pub snap_after: Option<SheetSnapshot>,
    /// 这个动作是谁做的（协作站点号）；0 表示单人/未知
    pub by_site: u32,
}

impl Action {
    pub fn len(&self) -> usize {
        self.deltas.len().max(
            self.snap_before
                .as_ref()
                .map(|s| s.cells.len())
                .unwrap_or(0),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty() && self.snap_before.is_none()
    }
}

pub struct UndoStack {
    done: Vec<Entry>,
    undone: Vec<Entry>,
    /// 最多保留多少步
    pub limit: usize,
    /// 我是谁（协作站点号）；0 表示单人。撤销只退自己的动作
    pub site: u32,
}

/// 撤销/重做实际改了哪些格子
pub struct Applied {
    /// 每个表里被改动的格子（用于重算与协作广播）
    pub touched: Vec<(usize, u32, u32)>,
    pub label: String,
    pub focus: (usize, u32, u32),
    /// 因为别人改过而跳过的格子数
    pub skipped: usize,
    /// Structural restoration changes coordinates and references across sheets.
    /// Rebuild dependencies and recalculate the workbook after applying it.
    pub structural: bool,
}

enum Entry {
    Cells(Action),
    Structure(StructureAction),
}

impl Entry {
    fn label(&self) -> &str {
        match self { Self::Cells(a) => &a.label, Self::Structure(a) => &a.label }
    }
    fn by_site(&self) -> u32 {
        match self { Self::Cells(a) => a.by_site, Self::Structure(a) => a.by_site }
    }
}

struct StructureAction {
    label: String,
    before: StructureSnapshot,
    after: StructureSnapshot,
    focus: (usize, u32, u32),
    by_site: u32,
}

#[derive(Clone, Debug)]
struct ForeignReferences {
    sheet: usize,
    cells: Vec<(CellRef, Cell)>,
    cf: Option<crate::cf::CfSheet>,
    dv: Option<Vec<crate::dv::DataValidation>>,
    charts: Option<Vec<crate::charts::Chart>>,
}

type PendingPivot = (usize, crate::pivot::PivotSpec, crate::pivot::PivotResult,
    (u32, u32, u32, u32), String);

/// A structural edit owns one complete sheet, plus only the foreign formulas
/// and reference metadata that the edit changes. Unrelated sheets' value cells
/// are never copied. File paths and source-part identities remain live state.
#[derive(Clone, Debug)]
pub struct StructureSnapshot {
    sheet: usize,
    target: Sheet,
    foreign: Vec<ForeignReferences>,
    names: Vec<(String, Option<usize>, String)>,
    pivot_shifts: Vec<(String, u8, u32, u32, bool)>,
    pending_pivots: Vec<PendingPivot>,
}

pub fn snapshot_structure(wb: &Workbook, sheet: usize, op: &crate::structural::Structural) -> StructureSnapshot {
    use crate::{refs::{Axis, Shift}, structural::Structural};
    let (axis, at, n, delete) = match op {
        Structural::InsertRows { at, n } => (Axis::Row, *at, *n, false),
        Structural::DeleteRows { at, n } => (Axis::Row, *at, *n, true),
        Structural::InsertCols { at, n } => (Axis::Col, *at, *n, false),
        Structural::DeleteCols { at, n } => (Axis::Col, *at, *n, true),
    };
    let target = wb.sheets[sheet].clone();
    let mut foreign = Vec::new();
    for (si, other) in wb.sheets.iter().enumerate().filter(|(si, _)| *si != sheet) {
        let shift = Shift { axis, at, n, delete, target_sheet: target.name.clone(), own_sheet: other.name.clone(), translate: None, row_filter: None, col_filter: None };
        let changes = |f: &str| crate::refs::rewrite(f, &shift) != f;
        let cells: Vec<_> = other.iter().filter(|(_, c)| c.formula.as_deref().is_some_and(changes))
            .map(|(key, c)| (key, c.clone())).collect();
        let cf = other.cf.blocks.iter().any(|block| block.rules.iter().any(|rule| match &rule.kind {
            crate::cf::CfKind::Expr { formula, .. } => changes(formula),
            crate::cf::CfKind::CellIs { a, b, .. } => changes(a) || changes(b),
            _ => false,
        })).then(|| other.cf.clone());
        let dv = other.dv.iter().any(|rule| changes(&rule.f1) || changes(&rule.f2)).then(|| other.dv.clone());
        let charts = other.charts.iter().any(|chart| chart.series.iter().any(|series| {
            changes(&series.name_ref) || changes(&series.cat_ref) || changes(&series.val_ref)
        })).then(|| other.charts.clone());
        if !cells.is_empty() || cf.is_some() || dv.is_some() || charts.is_some() {
            foreign.push(ForeignReferences { sheet: si, cells, cf, dv, charts });
        }
    }
    StructureSnapshot {
        sheet, target, foreign, names: wb.names.clone(), pivot_shifts: wb.pivot_shifts.clone(),
        pending_pivots: wb.pivots_pending.clone(),
    }
}

impl StructureSnapshot {
    /// Capture the same locations after applying the structural edit. In
    /// particular, a deleted reference is now #REF! and cannot be rediscovered
    /// by running the original reference matcher against the new workbook.
    pub fn capture_after(&self, wb: &Workbook) -> Self {
        Self {
            sheet: self.sheet,
            target: wb.sheets[self.sheet].clone(),
            foreign: self.foreign.iter().map(|scope| {
                let sheet = &wb.sheets[scope.sheet];
                ForeignReferences {
                    sheet: scope.sheet,
                    cells: scope.cells.iter().map(|(key, _)| (*key, sheet.get(key.row, key.col).cloned().unwrap_or_default())).collect(),
                    cf: scope.cf.as_ref().map(|_| sheet.cf.clone()),
                    dv: scope.dv.as_ref().map(|_| sheet.dv.clone()),
                    charts: scope.charts.as_ref().map(|_| sheet.charts.clone()),
                }
            }).collect(),
            names: wb.names.clone(),
            pivot_shifts: wb.pivot_shifts.clone(),
            pending_pivots: wb.pivots_pending.clone(),
        }
    }
}

/// Keep the chart write journal relative to the current disk baseline, while
/// appending a reference change for the state we are restoring.
fn restored_charts(current: &[crate::charts::Chart], desired: &[crate::charts::Chart]) -> Vec<crate::charts::Chart> {
    let mut restored = desired.to_vec();
    for (i, chart) in restored.iter_mut().enumerate() {
        let old = current.iter().find(|candidate| !chart.part.is_empty() && candidate.part == chart.part)
            .or_else(|| current.get(i));
        let Some(old) = old else { continue };
        chart.ref_fixes = old.ref_fixes.clone();
        for (old, new) in old.series.iter().zip(&chart.series) {
            for (from, to) in [(&old.name_ref, &new.name_ref), (&old.cat_ref, &new.cat_ref), (&old.val_ref, &new.val_ref)] {
                if from != to { chart.ref_fixes.push((from.clone(), to.clone())); }
            }
        }
    }
    restored
}

fn restore_structure(wb: &mut Workbook, snap: &StructureSnapshot) -> Vec<(usize, u32, u32)> {
    let Some(current) = wb.sheets.get(snap.sheet) else { return Vec::new() };
    let mut touched: HashSet<_> = current.iter().map(|(key, _)| (snap.sheet, key.row, key.col)).collect();
    touched.extend(snap.target.iter().map(|(key, _)| (snap.sheet, key.row, key.col)));
    let mut target = snap.target.clone();
    // Saving may have assigned a part to a previously new sheet. Undo changes
    // its content, not its identity or the current save baseline.
    target.identity = current.identity;
    target.source_part = current.source_part.clone();
    target.cleared.extend(current.cleared.iter().copied());
    target.cleared.extend(current.iter().filter(|(key, _)| !target.contains(key.row, key.col)).map(|(key, _)| key).collect::<Vec<_>>());
    target.style_edits.extend(current.style_edits.iter().copied());
    target.style_edits.extend(current.styles.keys().copied());
    target.style_edits.extend(target.styles.keys().copied());
    target.charts = restored_charts(&current.charts, &target.charts);
    target.loading = false;
    target.edited = true;
    target.styles_dirty = !target.style_edits.is_empty();
    target.merges_dirty = true;
    target.cf_dirty = true;
    target.dv_dirty = true;
    target.notes_dirty = true;
    target.freeze_dirty = true;
    target.filter_dirty = true;
    target.page_dirty = true;
    target.charts_dirty = true;
    wb.sheets[snap.sheet] = target;
    for foreign in &snap.foreign {
        let Some(sheet) = wb.sheets.get_mut(foreign.sheet) else { continue };
        for (key, cell) in &foreign.cells {
            sheet.set(key.row, key.col, cell.clone());
            touched.insert((foreign.sheet, key.row, key.col));
        }
        if let Some(cf) = &foreign.cf { sheet.cf = cf.clone(); sheet.cf_dirty = true; }
        if let Some(dv) = &foreign.dv { sheet.dv = dv.clone(); sheet.dv_dirty = true; }
        if let Some(charts) = &foreign.charts {
            sheet.charts = restored_charts(&sheet.charts, charts);
            sheet.charts_dirty = true;
        }
        sheet.edited = true;
    }
    if wb.names != snap.names {
        wb.names = snap.names.clone();
        wb.names_dirty = true;
    }
    wb.pivot_shifts = snap.pivot_shifts.clone();
    // A deleted source/field/output removes its refreshable definition. Restore
    // the full entry on undo; save keeps this session-owned list authoritative.
    wb.pivots_pending = snap.pending_pivots.clone();
    wb.dirty = true;
    let mut touched: Vec<_> = touched.into_iter().collect();
    touched.sort_unstable();
    touched
}

impl Default for UndoStack {
    fn default() -> Self {
        Self { done: Vec::new(), undone: Vec::new(), limit: 200, site: 0 }
    }
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一步。相同格子在一步里只记第一次（后写的 after 覆盖）
    /// 换站点（进/退协作房间）：历史属于上一个人，清掉
    pub fn set_site(&mut self, site: u32) {
        if site != self.site {
            self.done.clear();
            self.undone.clear();
        }
        self.site = site;
    }

    pub fn push(&mut self, action: Action) {
        if action.is_empty() {
            return;
        }
        // 记下是谁做的：协作时撤销只退自己的
        let mut action = action;
        if action.by_site == 0 {
            action.by_site = self.site;
        }
        self.done.push(Entry::Cells(action));
        // 新动作让「重做」失效
        self.undone.clear();
        let limit = if self.limit == 0 { 200 } else { self.limit };
        if self.done.len() > limit {
            self.done.remove(0);
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.done.last().map(Entry::label)
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.undone.last().map(Entry::label)
    }

    pub fn depth(&self) -> (usize, usize) {
        (self.done.len(), self.undone.len())
    }

    pub fn undo_is_structure(&self) -> bool {
        matches!(self.done.last(), Some(Entry::Structure(_)))
    }

    pub fn redo_is_structure(&self) -> bool {
        matches!(self.undone.last(), Some(Entry::Structure(_)))
    }

    pub fn push_structure(&mut self, label: impl Into<String>, before: StructureSnapshot, after: StructureSnapshot, focus: (usize, u32, u32)) {
        self.done.push(Entry::Structure(StructureAction { label: label.into(), before, after, focus, by_site: self.site }));
        self.undone.clear();
        let limit = if self.limit == 0 { 200 } else { self.limit };
        if self.done.len() > limit { self.done.remove(0); }
    }

    pub fn undo_workbook(&mut self, wb: &mut Workbook, guard: impl FnMut(usize, u32, u32, &Cell) -> bool) -> Option<Applied> {
        if !self.undo_is_structure() { return self.undo(&mut wb.sheets, guard); }
        if self.done.last().is_some_and(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site) { return None; }
        let Entry::Structure(action) = self.done.pop()? else { unreachable!() };
        let touched = restore_structure(wb, &action.before);
        let applied = Applied { touched, label: action.label.clone(), focus: action.focus, skipped: 0, structural: true };
        self.undone.push(Entry::Structure(action));
        Some(applied)
    }

    pub fn redo_workbook(&mut self, wb: &mut Workbook, guard: impl FnMut(usize, u32, u32, &Cell) -> bool) -> Option<Applied> {
        if !self.redo_is_structure() { return self.redo(&mut wb.sheets, guard); }
        if self.undone.last().is_some_and(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site) { return None; }
        let Entry::Structure(action) = self.undone.pop()? else { unreachable!() };
        let touched = restore_structure(wb, &action.after);
        let applied = Applied { touched, label: action.label.clone(), focus: action.focus, skipped: 0, structural: true };
        self.done.push(Entry::Structure(action));
        Some(applied)
    }

    /// 撤销一步。`guard` 返回 false 的格子会被跳过（协作时不覆盖别人的新改动）。
    pub fn undo(
        &mut self,
        sheets: &mut [Sheet],
        mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> Option<Applied> {
        if self.undo_is_structure() { return None; }
        // 只撤自己的：栈顶不是我的动作就不动（也不丢，免得误删历史）
        if self
            .done
            .last()
            .map(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site)
            .unwrap_or(false)
        {
            return None;
        }
        let Entry::Cells(action) = self.done.pop()? else { unreachable!() };
        let mut touched = Vec::new();
        let mut skipped = 0;
        if let Some(snap) = action.snap_before.clone() {
            if let Some(sheet) = sheets.get_mut(snap.sheet) {
                restore_sheet(sheet, &snap);
            }
            touched.extend(snap.cells.iter().map(|(r, c, _, _)| (snap.sheet, *r, *c)));
            let applied = Applied {
                touched,
                label: action.label.clone(),
                focus: action.focus,
                skipped,
                structural: true,
            };
            self.undone.push(Entry::Cells(action));
            return Some(applied);
        }
        for d in action.deltas.iter().rev() {
            let Some(sheet) = sheets.get_mut(d.sheet) else { continue };
            let cur = sheet.get(d.row, d.col).cloned().unwrap_or(Cell {
                value: crate::model::Value::Empty,
                formula: None,
            });
            if !guard(d.sheet, d.row, d.col, &cur) {
                skipped += 1;
                continue;
            }
            sheet.set(d.row, d.col, d.before.clone());
            sheet.set_style(d.row, d.col, d.before_style);
            touched.push((d.sheet, d.row, d.col));
        }
        let applied = Applied {
            touched,
            label: action.label.clone(),
            focus: action.focus,
            skipped,
            structural: false,
        };
        self.undone.push(Entry::Cells(action));
        Some(applied)
    }

    /// 重做一步
    pub fn redo(
        &mut self,
        sheets: &mut [Sheet],
        mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> Option<Applied> {
        if self.redo_is_structure() { return None; }
        if self
            .undone
            .last()
            .map(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site)
            .unwrap_or(false)
        {
            return None;
        }
        let Entry::Cells(action) = self.undone.pop()? else { unreachable!() };
        let mut touched = Vec::new();
        let mut skipped = 0;
        if let Some(snap) = action.snap_after.clone() {
            if let Some(sheet) = sheets.get_mut(snap.sheet) {
                restore_sheet(sheet, &snap);
            }
            touched.extend(snap.cells.iter().map(|(r, c, _, _)| (snap.sheet, *r, *c)));
            let applied = Applied {
                touched,
                label: action.label.clone(),
                focus: action.focus,
                skipped,
                structural: true,
            };
            self.done.push(Entry::Cells(action));
            return Some(applied);
        }
        for d in action.deltas.iter() {
            let Some(sheet) = sheets.get_mut(d.sheet) else { continue };
            let cur = sheet.get(d.row, d.col).cloned().unwrap_or(Cell {
                value: crate::model::Value::Empty,
                formula: None,
            });
            if !guard(d.sheet, d.row, d.col, &cur) {
                skipped += 1;
                continue;
            }
            sheet.set(d.row, d.col, d.after.clone());
            sheet.set_style(d.row, d.col, d.after_style);
            touched.push((d.sheet, d.row, d.col));
        }
        let applied = Applied {
            touched,
            label: action.label.clone(),
            focus: action.focus,
            skipped,
            structural: false,
        };
        self.done.push(Entry::Cells(action));
        Some(applied)
    }

    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }
}

/// 收集一组格子的当前状态，供构造 Action 用
pub fn capture(sheet: &Sheet, sheet_idx: usize, keys: &[(u32, u32)]) -> Vec<CellDelta> {
    keys.iter()
        .map(|(r, c)| CellDelta {
            sheet: sheet_idx,
            row: *r,
            col: *c,
            before: sheet.get(*r, *c).cloned().unwrap_or(Cell {
                value: crate::model::Value::Empty,
                formula: None,
            }),
            after: sheet.get(*r, *c).cloned().unwrap_or(Cell {
                value: crate::model::Value::Empty,
                formula: None,
            }),
            before_style: sheet.style_of(*r, *c),
            after_style: sheet.style_of(*r, *c),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Value, Workbook};

    fn wb() -> Workbook {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
        wb
    }

    fn edit(wb: &mut Workbook, r: u32, c: u32, v: f64) -> Action {
        let before = wb.sheets[0].get(r, c).cloned().unwrap();
        let bs = wb.sheets[0].style_of(r, c);
        wb.sheets[0].set(r, c, Cell { value: Value::Number(v), formula: None });
        let after = wb.sheets[0].get(r, c).cloned().unwrap();
        Action {
            label: "编辑".into(),
            snap_before: None,
            snap_after: None,
            by_site: 0,
            deltas: vec![CellDelta {
                sheet: 0,
                row: r,
                col: c,
                before,
                after,
                before_style: bs,
                after_style: bs,
            }],
            focus: (0, r, c),
        }
    }

    #[test]
    fn undo_redo_single_cell() {
        let mut wb = wb();
        let mut st = UndoStack::new();
        st.push(edit(&mut wb, 0, 0, 42.0));
        assert_eq!(wb.sheets[0].get(0, 0).unwrap().value.display(), "42");
        let a = st.undo(&mut wb.sheets, |_, _, _, _| true).unwrap();
        assert_eq!(a.label, "编辑");
        assert_eq!(wb.sheets[0].get(0, 0).unwrap().value.display(), "1");
        assert!(!st.can_undo());
        assert!(st.can_redo());
        st.redo(&mut wb.sheets, |_, _, _, _| true).unwrap();
        assert_eq!(wb.sheets[0].get(0, 0).unwrap().value.display(), "42");
    }

    #[test]
    fn new_action_clears_redo() {
        let mut wb = wb();
        let mut st = UndoStack::new();
        st.push(edit(&mut wb, 0, 0, 2.0));
        st.undo(&mut wb.sheets, |_, _, _, _| true);
        assert!(st.can_redo());
        st.push(edit(&mut wb, 0, 0, 3.0));
        assert!(!st.can_redo(), "新动作应该清掉重做栈");
    }

    #[test]
    fn guard_skips_cells_changed_by_others() {
        let mut wb = wb();
        let mut st = UndoStack::new();
        // 一次改两格
        let mut act = edit(&mut wb, 0, 0, 10.0);
        act.deltas.push(CellDelta {
            sheet: 0,
            row: 0,
            col: 1,
            before: Cell { value: Value::Empty, formula: None },
            after: Cell { value: Value::Number(99.0), formula: None },
            before_style: 0,
            after_style: 0,
        });
        wb.sheets[0].set(0, 1, Cell { value: Value::Number(99.0), formula: None });
        st.push(act);
        // 别人把 (0,1) 改成了 123
        wb.sheets[0].set(0, 1, Cell { value: Value::Number(123.0), formula: None });
        let applied = st
            .undo(&mut wb.sheets, |_, _, c, cur| {
                if c == 1 {
                    cur.value.display() == "99"
                } else {
                    true
                }
            })
            .unwrap();
        assert_eq!(applied.skipped, 1, "被改过的那格要跳过");
        assert_eq!(wb.sheets[0].get(0, 0).unwrap().value.display(), "1");
        assert_eq!(wb.sheets[0].get(0, 1).unwrap().value.display(), "123", "不能盖掉别人的值");
    }

    #[test]
    fn limit_drops_oldest() {
        let mut wb = wb();
        let mut st = UndoStack::new();
        st.limit = 3;
        for i in 1..=5 {
            st.push(edit(&mut wb, 0, 0, i as f64));
        }
        assert_eq!(st.depth().0, 3);
    }
}

/// 把 capture 出来的「体检表」补上 after，并去掉没变化的格子
pub fn finish(mut deltas: Vec<CellDelta>, sheet: &Sheet) -> Vec<CellDelta> {
    deltas.retain_mut(|d| {
        let cur = sheet.get(d.row, d.col).cloned().unwrap_or(Cell {
            value: crate::model::Value::Empty,
            formula: None,
        });
        d.after = cur;
        d.after_style = sheet.style_of(d.row, d.col);
        d.before.value.display() != d.after.value.display()
            || d.before.formula != d.after.formula
            || d.before_style != d.after_style
    });
    deltas
}

/// 一块矩形里的全部坐标
pub fn rect_keys(r1: u32, c1: u32, r2: u32, c2: u32) -> Vec<(u32, u32)> {
    let mut v = Vec::with_capacity(((r2 - r1 + 1) as usize) * ((c2 - c1 + 1) as usize));
    for r in r1..=r2 {
        for c in c1..=c2 {
            v.push((r, c));
        }
    }
    v
}
