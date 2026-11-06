//! 撤销/重做：记录「改之前」和「改之后」，按动作成组回退。
//!
//! 不存整本工作簿快照——152 MB 的文件拷一份就废了。每次动作只记受影响的格子，
//! 一个动作里可能有上万格（整列粘贴），但每格只存两个 `Cell`（40 字节）+ 两个样式号。
//!
//! 协作场景：撤销时如果某格当前值已经不是「我改成的样子」，说明别人后来动过，
//! 跳过它——不拿旧值盖掉别人的新编辑。

use crate::model::{Cell, Sheet};

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
    done: Vec<Action>,
    undone: Vec<Action>,
    /// 最多保留多少步
    pub limit: usize,
}

/// 撤销/重做实际改了哪些格子
pub struct Applied {
    /// 每个表里被改动的格子（用于重算与协作广播）
    pub touched: Vec<(usize, u32, u32)>,
    pub label: String,
    pub focus: (usize, u32, u32),
    /// 因为别人改过而跳过的格子数
    pub skipped: usize,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self { done: Vec::new(), undone: Vec::new(), limit: 200 }
    }
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一步。相同格子在一步里只记第一次（后写的 after 覆盖）
    pub fn push(&mut self, action: Action) {
        if action.is_empty() {
            return;
        }
        self.done.push(action);
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
        self.done.last().map(|a| a.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.undone.last().map(|a| a.label.as_str())
    }

    pub fn depth(&self) -> (usize, usize) {
        (self.done.len(), self.undone.len())
    }

    /// 撤销一步。`guard` 返回 false 的格子会被跳过（协作时不覆盖别人的新改动）。
    pub fn undo(
        &mut self,
        sheets: &mut [Sheet],
        mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> Option<Applied> {
        let action = self.done.pop()?;
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
            };
            self.undone.push(action);
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
        };
        self.undone.push(action);
        Some(applied)
    }

    /// 重做一步
    pub fn redo(
        &mut self,
        sheets: &mut [Sheet],
        mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> Option<Applied> {
        let action = self.undone.pop()?;
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
            };
            self.done.push(action);
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
        };
        self.done.push(action);
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
