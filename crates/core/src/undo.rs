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

/// 形状（文本框）改动的前后快照：文本框不在格子模型里，靠整组前后状态回退
#[derive(Clone, Debug)]
pub struct ShapeEdit {
    pub sheet: usize,
    pub before: Vec<crate::textbox::TextBox>,
    pub after: Vec<crate::textbox::TextBox>,
}

/// 图片改动的前后快照：图片不在格子模型里，整组前后状态回退
#[derive(Clone, Debug)]
pub struct PictureEdit {
    pub sheet: usize,
    pub before: Vec<crate::pic::Picture>,
    pub after: Vec<crate::pic::Picture>,
}

/// 合并/取消合并：只记差异，不整表快照。撤销/重做按差异增删，
/// 因此动作之后别人新增的无关合并不会被旧的整表列表覆盖。
#[derive(Clone, Debug)]
pub struct MergeEdit {
    pub sheet: usize,
    /// 这个动作新增的合并矩形
    pub added: Vec<(u32, u32, u32, u32)>,
    /// 这个动作拆掉的既有合并矩形（撤销时恢复）
    pub removed: Vec<(u32, u32, u32, u32)>,
}

impl MergeEdit {
    pub fn between(
        sheet: usize,
        before: &[(u32, u32, u32, u32)],
        after: &[(u32, u32, u32, u32)],
    ) -> Option<Self> {
        let added: Vec<_> = after.iter().copied().filter(|r| !before.contains(r)).collect();
        let removed: Vec<_> = before.iter().copied().filter(|r| !after.contains(r)).collect();
        (!added.is_empty() || !removed.is_empty()).then_some(Self { sheet, added, removed })
    }
}

fn rects_overlap(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> bool {
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

/// 规划一组合并差异。先完整校验再一次性替换列表：
/// 任意一个要恢复的合并与当前表里的合并相交，整组操作放弃（返回 None），
/// 调用方必须同时放弃同一动作的 CellDelta，保证结构和内容原子一致。
fn plan_merge_diff(
    sheet: &Sheet,
    add: &[(u32, u32, u32, u32)],
    remove: &[(u32, u32, u32, u32)],
) -> Option<(Vec<(u32, u32, u32, u32)>, Vec<((u32, u32, u32, u32), bool)>)> {
    let mut next = sheet.merges.clone();
    let mut changed = Vec::new();
    for r in remove {
        let pos = next.iter().position(|m| m == r)?;
        next.remove(pos);
        changed.push((*r, false));
    }
    for r in add {
        if next.iter().any(|m| m == r) {
            continue;
        }
        if next.iter().any(|m| rects_overlap(*m, *r)) {
            return None;
        }
        next.push(*r);
        changed.push((*r, true));
    }
    Some((next, changed))
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
    /// 形状（文本框）改动；None = 普通格子动作
    pub shapes: Option<ShapeEdit>,
    /// 图片改动（插入/移动/缩放/删除）；None = 不涉及图片
    pub pictures: Option<PictureEdit>,
    /// 合并状态改动；None = 不涉及合并
    pub merges: Option<MergeEdit>,
    /// 刷新外部数据时的源尺寸（前, 后）：撤销/重做要一并还原，否则范围记录漂移
    pub refresh_sizes: Option<(Vec<(String, u32, u32)>, Vec<(String, u32, u32)>)>,
}

impl Action {
    pub fn len(&self) -> usize {
        self.deltas
            .len()
            .max(
                self.snap_before
                    .as_ref()
                    .map(|s| s.cells.len())
                    .unwrap_or(0),
            )
            .max(if self.shapes.is_some() { 1 } else { 0 })
            .max(if self.pictures.is_some() { 1 } else { 0 })
            .max(if self.merges.is_some() { 1 } else { 0 })
    }

    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty()
            && self.snap_before.is_none()
            && self.shapes.is_none()
            && self.pictures.is_none()
            && self.merges.is_none()
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
    /// 撤销/重做后要写回的外源源尺寸（名字, 行, 列）
    pub refresh_sizes: Option<Vec<(String, u32, u32)>>,
    /// 撤销/重做实际发生的合并结构变化：(表, 矩形, 是否新增)
    pub merge_changes: Vec<(usize, (u32, u32, u32, u32), bool)>,
    /// 合并结构与当前表冲突，整个动作（含 CellDelta）未应用
    pub merge_conflict: bool,
}

static NEXT_ENTRY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_entry() -> u64 {
    NEXT_ENTRY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

enum Entry {
    Cells(u64, Action),
    Structure(u64, StructureAction),
    SheetTab(u64, SheetTabAction),
}

impl Entry {
    fn id(&self) -> u64 {
        match self { Self::Cells(id, _) | Self::Structure(id, _) | Self::SheetTab(id, _) => *id }
    }

    fn simple_deltas(&self) -> Option<&[CellDelta]> {
        match self {
            Self::Cells(_, a) if a.snap_before.is_none() && a.snap_after.is_none()
                && a.merges.is_none() && a.shapes.is_none() && a.pictures.is_none()
                && a.refresh_sizes.is_none() => Some(&a.deltas),
            _ => None,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Cells(_, a) => &a.label,
            Self::Structure(_, a) => &a.label,
            Self::SheetTab(_, a) => &a.label,
        }
    }
    fn by_site(&self) -> u32 {
        match self {
            Self::Cells(_, a) => a.by_site,
            Self::Structure(_, a) => a.by_site,
            Self::SheetTab(_, a) => a.by_site,
        }
    }
}

struct SheetTabAction {
    label: String,
    index: usize,
    identity: u64,
    previous_focus: (usize, u32, u32),
    change: SheetTabChange,
    by_site: u32,
}

enum SheetTabChange {
    Append { removed: Option<Box<Sheet>> },
    Rename { before: String, after: String, references: crate::sheet_ops::RenameReferences },
}

impl SheetTabAction {
    fn apply(&mut self, wb: &mut Workbook, redo: bool) -> Option<Applied> {
        let mut focus = self.previous_focus;
        match &mut self.change {
            SheetTabChange::Rename { before, after, references } => {
                let (expected, restored) = if redo { (before, after) } else { (after, before) };
                let sheet = wb.sheets.get(self.index)?;
                if sheet.identity != self.identity || sheet.name != *expected
                    || !references.restore(wb, self.index, restored, redo)
                {
                    return None;
                }
            }
            SheetTabChange::Append { removed } if redo => {
                if wb.sheets.len() != self.index || wb.sheets.iter().any(|s| {
                    s.identity == self.identity || removed.as_ref().is_some_and(|r| r.name.eq_ignore_ascii_case(&s.name))
                }) { return None; }
                wb.sheets.push(*removed.take()?);
                focus = (self.index, 0, 0);
            }
            SheetTabChange::Append { removed } => {
                if self.index == 0 || wb.sheets.len() != self.index + 1
                    || wb.sheets[self.index].identity != self.identity
                    || wb.names.iter().any(|(_, scope, _)| *scope == Some(self.index))
                    || wb.pivots_pending.iter().any(|(sheet, ..)| *sheet == self.index)
                    || wb.pivots_pending2.iter().any(|(sheet, ..)| *sheet == self.index)
                { return None; }
                // Pop only the appended sheet. A normal delete rewrites formulas in other
                // sheets, including unresolved references that existed before this append.
                *removed = wb.sheets.pop().map(Box::new);
                wb.calc_pending_sheets.remove(&self.index);
            }
        }
        wb.dirty = true;
        Some(Applied {
            touched: Vec::new(), label: self.label.clone(), focus, skipped: 0,
            structural: true, refresh_sizes: None, merge_changes: Vec::new(), merge_conflict: false,
        })
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
    sparklines: Option<Vec<crate::sparkline::Sparkline>>,
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
        let cells: Vec<_> = other.foreign_formula_cells(&target.name).filter(|(_, c)| c.formula.as_deref().is_some_and(changes))
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
        let sparklines = other.sparklines.iter().any(|spark| spark.source_sheet.as_deref() == Some(target.name.as_str()))
            .then(|| other.sparklines.clone());
        if !cells.is_empty() || cf.is_some() || dv.is_some() || charts.is_some() || sparklines.is_some() {
            foreign.push(ForeignReferences { sheet: si, cells, cf, dv, charts, sparklines });
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
                    sparklines: scope.sparklines.as_ref().map(|_| sheet.sparklines.clone()),
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
    let mut target = snap.target.clone();
    let mut touched = HashSet::new();
    // Saving may have assigned a part to a previously new sheet. Undo changes
    // its content, not its identity or the current save baseline.
    target.identity = current.identity;
    target.source_part = current.source_part.clone();
    target.cleared.extend(current.cleared.iter().copied());
    for key in current.changed_cells(&snap.target) {
        touched.insert((snap.sheet, key.row, key.col));
        if !target.contains(key.row, key.col) {
            target.cleared.insert(key);
        }
    }
    touched.extend(target.changed_cells(current).map(|k| (snap.sheet, k.row, k.col)));
    target.style_edits.extend(current.style_edits.iter().copied());
    for k in current.styles.changed_keys(&target.styles).chain(target.styles.changed_keys(&current.styles)) {
        touched.insert((snap.sheet, k.row, k.col));
        target.style_edits.insert(k);
    }
    if current.row_styles != target.row_styles || current.col_styles != target.col_styles {
        touched.extend(target.iter().filter(|(k, _)| target.style_of(k.row, k.col) != current.style_of(k.row, k.col))
            .map(|(k, _)| (snap.sheet, k.row, k.col)));
    }
    target.charts = restored_charts(&current.charts, &target.charts);
    target.loading = false;
    target.edited = true;
    target.row_styles_dirty = target.row_styles != current.row_styles || target.row_styles_dirty;
    target.col_styles_dirty = target.col_styles != current.col_styles || target.col_styles_dirty;
    target.styles_dirty = !target.style_edits.is_empty()
        || target.row_styles_dirty
        || target.col_styles_dirty;
    target.merges_dirty = true;
    target.cf_dirty = true;
    target.dv_dirty = true;
    target.notes_dirty = true;
    target.freeze_dirty = true;
    target.filter_dirty = true;
    target.page_dirty = true;
    target.charts_dirty = true;
    target.sparklines_dirty = true;
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
        if let Some(sparklines) = &foreign.sparklines {
            sheet.sparklines = sparklines.clone();
            sheet.sparklines_dirty = true;
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
        self.done.push(Entry::Cells(next_entry(), action));
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

    /// Mark a synchronous edit so its undo records can be identified even if
    /// the history limit evicts old records or another workbook is activated.
    pub fn checkpoint(&self) -> u64 {
        NEXT_ENTRY.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn ids_since(&self, checkpoint: u64) -> Vec<u64> {
        self.done.iter().map(Entry::id).filter(|id| *id >= checkpoint).collect()
    }

    /// Move a complete group to the top only if it commutes with every newer
    /// unselected action. A structural edit or an overlapping cell is a barrier.
    pub fn prepare_group_undo(&mut self, ids: &[u64]) -> bool {
        let Some(first) = ids.first().and_then(|id| self.done.iter().position(|e| e.id() == *id)) else {
            return false;
        };
        let wanted: HashSet<_> = ids.iter().copied().collect();
        let suffix = &mut self.done[first..];
        let mut later_cells = HashSet::new();
        let mut has_later_edit = false;
        let mut structural_barrier = false;
        let mut found = 0;
        for entry in suffix.iter().rev() {
            if wanted.contains(&entry.id()) {
                found += 1;
                if self.site != 0 && entry.by_site() != 0 && self.site != entry.by_site() {
                    return false;
                }
                if has_later_edit && (structural_barrier || entry.simple_deltas().is_none_or(|ds|
                    ds.iter().any(|d| later_cells.contains(&(d.sheet, d.row, d.col))))) {
                    return false;
                }
            } else {
                has_later_edit = true;
                match entry.simple_deltas() {
                    Some(ds) => later_cells.extend(ds.iter().map(|d| (d.sheet, d.row, d.col))),
                    None => structural_barrier = true,
                }
            }
        }
        if found != ids.len() {
            return false;
        }
        suffix.sort_by_key(|entry| wanted.contains(&entry.id()));
        true
    }

    pub fn next_undo_id(&self) -> Option<u64> {
        self.done.last().map(Entry::id)
    }

    pub fn undo_is_structure(&self) -> bool {
        matches!(self.done.last(), Some(Entry::Structure(..) | Entry::SheetTab(..)))
    }

    pub fn redo_is_structure(&self) -> bool {
        matches!(self.undone.last(), Some(Entry::Structure(..) | Entry::SheetTab(..)))
    }

    pub fn record_appended_sheet(&mut self, wb: &Workbook, previous_focus: (usize, u32, u32)) {
        let index = wb.sheets.len() - 1;
        self.push_sheet_tab(index, wb.sheets[index].identity, previous_focus, "新建工作表",
            SheetTabChange::Append { removed: None });
    }

    pub fn rename_sheet(&mut self, wb: &mut Workbook, index: usize, name: &str, focus: (usize, u32, u32)) -> bool {
        let Some(sheet) = wb.sheets.get(index) else { return false };
        let before = sheet.name.clone();
        let identity = sheet.identity;
        let Some(references) = crate::sheet_ops::rename_with_history(wb, index, name) else { return false };
        self.push_sheet_tab(index, identity, focus, "重命名工作表",
            SheetTabChange::Rename { before, after: wb.sheets[index].name.clone(), references });
        true
    }

    fn push_sheet_tab(&mut self, index: usize, identity: u64, previous_focus: (usize, u32, u32),
                      label: &str, change: SheetTabChange) {
        self.done.push(Entry::SheetTab(next_entry(), SheetTabAction {
            label: label.into(), index, identity, previous_focus, change, by_site: self.site,
        }));
        self.undone.clear();
        let limit = if self.limit == 0 { 200 } else { self.limit };
        if self.done.len() > limit { self.done.remove(0); }
    }

    pub fn push_structure(&mut self, label: impl Into<String>, before: StructureSnapshot, after: StructureSnapshot, focus: (usize, u32, u32)) {
        self.done.push(Entry::Structure(next_entry(), StructureAction { label: label.into(), before, after, focus, by_site: self.site }));
        self.undone.clear();
        let limit = if self.limit == 0 { 200 } else { self.limit };
        if self.done.len() > limit { self.done.remove(0); }
    }

    pub fn undo_workbook(&mut self, wb: &mut Workbook, guard: impl FnMut(usize, u32, u32, &Cell) -> bool) -> Option<Applied> {
        if !self.undo_is_structure() { return self.undo(&mut wb.sheets, guard); }
        if self.done.last().is_some_and(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site) { return None; }
        if let Some(Entry::SheetTab(_, action)) = self.done.last_mut() {
            let applied = action.apply(wb, false)?;
            self.undone.push(self.done.pop()?);
            return Some(applied);
        }
        let Entry::Structure(id, action) = self.done.pop()? else { unreachable!() };
        let touched = restore_structure(wb, &action.before);
        let applied = Applied { touched, label: action.label.clone(), focus: action.focus, skipped: 0, structural: true, refresh_sizes: None, merge_changes: Vec::new(), merge_conflict: false };
        self.undone.push(Entry::Structure(id, action));
        Some(applied)
    }

    pub fn redo_workbook(&mut self, wb: &mut Workbook, guard: impl FnMut(usize, u32, u32, &Cell) -> bool) -> Option<Applied> {
        if !self.redo_is_structure() { return self.redo(&mut wb.sheets, guard); }
        if self.undone.last().is_some_and(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site) { return None; }
        if let Some(Entry::SheetTab(_, action)) = self.undone.last_mut() {
            let applied = action.apply(wb, true)?;
            self.done.push(self.undone.pop()?);
            return Some(applied);
        }
        let Entry::Structure(id, action) = self.undone.pop()? else { unreachable!() };
        let touched = restore_structure(wb, &action.after);
        let applied = Applied { touched, label: action.label.clone(), focus: action.focus, skipped: 0, structural: true, refresh_sizes: None, merge_changes: Vec::new(), merge_conflict: false };
        self.done.push(Entry::Structure(id, action));
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
        let Entry::Cells(id, action) = self.done.pop()? else { unreachable!() };
        let mut touched = Vec::new();
        let mut skipped = 0;
        let mut merge_changes = Vec::new();
        if let Some(p) = action.pictures.clone() {
            if let Some(sheet) = sheets.get_mut(p.sheet) {
                sheet.pictures = p.before;
                sheet.pictures_dirty = true;
            }
            let applied = Applied {
                touched: Vec::new(),
                label: action.label.clone(),
                focus: action.focus,
                skipped: 0,
                structural: false,
                refresh_sizes: None,
                merge_changes,
                merge_conflict: false,
            };
            self.undone.push(Entry::Cells(id, action));
            return Some(applied);
        }
        if let Some(sh) = action.shapes.clone() {
            if let Some(sheet) = sheets.get_mut(sh.sheet) {
                sheet.textboxes = sh.before;
                sheet.textboxes_dirty = true;
            }
            let applied = Applied {
                touched: Vec::new(),
                label: action.label.clone(),
                focus: action.focus,
                skipped: 0,
                structural: false,
                refresh_sizes: None,
                merge_changes,
                merge_conflict: false,
            };
            self.undone.push(Entry::Cells(id, action));
            return Some(applied);
        }
        let mut merge_conflict = false;
        if let Some(m) = action.merges.as_ref() {
            if let Some(sheet) = sheets.get_mut(m.sheet) {
                match plan_merge_diff(sheet, &m.removed, &m.added) {
                    Some((next, changes)) => {
                        sheet.merges = next;
                        sheet.merges_dirty = true;
                        for (rect, added) in changes {
                            merge_changes.push((m.sheet, rect, added));
                        }
                    }
                    None => merge_conflict = true,
                }
            }
        }
        if merge_conflict {
            let label = action.label.clone();
            let focus = action.focus;
            self.done.push(Entry::Cells(id, action));
            return Some(Applied { touched, label, focus, skipped: 0, structural: false, refresh_sizes: None, merge_changes, merge_conflict: true });
        }
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
                refresh_sizes: action.refresh_sizes.clone().map(|(b, _)| b),
                merge_changes,
                merge_conflict: false,
            };
            self.undone.push(Entry::Cells(id, action));
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
            refresh_sizes: action.refresh_sizes.clone().map(|(b, _)| b),
            merge_changes,
            merge_conflict: false,
        };
        self.undone.push(Entry::Cells(id, action));
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
        let Entry::Cells(id, action) = self.undone.pop()? else { unreachable!() };
        let mut touched = Vec::new();
        let mut skipped = 0;
        let mut merge_changes = Vec::new();
        if let Some(p) = action.pictures.clone() {
            if let Some(sheet) = sheets.get_mut(p.sheet) {
                sheet.pictures = p.after;
                sheet.pictures_dirty = true;
            }
            let applied = Applied {
                touched: Vec::new(),
                label: action.label.clone(),
                focus: action.focus,
                skipped: 0,
                structural: false,
                refresh_sizes: action.refresh_sizes.clone().map(|(_, a)| a),
                merge_changes,
                merge_conflict: false,
            };
            self.done.push(Entry::Cells(id, action));
            return Some(applied);
        }
        if let Some(sh) = action.shapes.clone() {
            if let Some(sheet) = sheets.get_mut(sh.sheet) {
                sheet.textboxes = sh.after;
                sheet.textboxes_dirty = true;
            }
            let applied = Applied {
                touched: Vec::new(),
                label: action.label.clone(),
                focus: action.focus,
                skipped: 0,
                structural: false,
                refresh_sizes: action.refresh_sizes.clone().map(|(_, a)| a),
                merge_changes,
                merge_conflict: false,
            };
            self.done.push(Entry::Cells(id, action));
            return Some(applied);
        }
        let mut merge_conflict = false;
        if let Some(m) = action.merges.as_ref() {
            if let Some(sheet) = sheets.get_mut(m.sheet) {
                match plan_merge_diff(sheet, &m.added, &m.removed) {
                    Some((next, changes)) => {
                        sheet.merges = next;
                        sheet.merges_dirty = true;
                        for (rect, added) in changes {
                            merge_changes.push((m.sheet, rect, added));
                        }
                    }
                    None => merge_conflict = true,
                }
            }
        }
        if merge_conflict {
            let label = action.label.clone();
            let focus = action.focus;
            self.undone.push(Entry::Cells(id, action));
            return Some(Applied { touched, label, focus, skipped: 0, structural: false, refresh_sizes: None, merge_changes, merge_conflict: true });
        }
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
                refresh_sizes: action.refresh_sizes.clone().map(|(_, a)| a),
                merge_changes,
                merge_conflict: false,
            };
            self.done.push(Entry::Cells(id, action));
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
            refresh_sizes: None,
            merge_changes,
            merge_conflict: false,
        };
        self.done.push(Entry::Cells(id, action));
        Some(applied)
    }

    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }
}

/// 收集一组格子的当前状态，供构造 Action 用。
/// `after` 延迟到 `finish` 中填充，避免在 `capture` 阶段把每个单元格白白深拷贝两遍。
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
            after: Cell {
                value: crate::model::Value::Empty,
                formula: None,
            },
            before_style: sheet.style_of(*r, *c),
            after_style: 0,
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
            merges: None,
            snap_before: None,
            snap_after: None,
            by_site: 0,
            pictures: None,
            shapes: None,
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
            refresh_sizes: None,
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

fn undo_values_eq(a: &crate::model::Value, b: &crate::model::Value) -> bool {
    use crate::model::Value;
    match (a, b) {
        (Value::Empty, Value::Empty) => true,
        (Value::Number(x), Value::Number(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Value::Text(x), Value::Text(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Error(x), Value::Error(y)) => x == y,
        _ => false,
    }
}

/// 把 capture 出来的「体检表」补上 after，并去掉没变化的格子（零字符串格式化分配）
pub fn finish(mut deltas: Vec<CellDelta>, sheet: &Sheet) -> Vec<CellDelta> {
    deltas.retain_mut(|d| {
        let cur = sheet.get(d.row, d.col).cloned().unwrap_or(Cell {
            value: crate::model::Value::Empty,
            formula: None,
        });
        d.after = cur;
        d.after_style = sheet.style_of(d.row, d.col);
        !undo_values_eq(&d.before.value, &d.after.value)
            || d.before.formula != d.after.formula
            || d.before_style != d.after_style
    });
    deltas
}

/// 单次稠密坐标展开上限：防止任何遗漏边界检查的调用方在全选（171 亿格）时直接触发 137 GB OOM 崩溃。
pub const MAX_DENSE_RECT_KEYS: usize = 500_000;

/// 一块矩形里的全部坐标（受 `MAX_DENSE_RECT_KEYS` 安全兜底保护）
pub fn rect_keys(r1: u32, c1: u32, r2: u32, c2: u32) -> Vec<(u32, u32)> {
    if r2 < r1 || c2 < c1 {
        return Vec::new();
    }
    let rows = (r2 - r1) as usize + 1;
    let cols = (c2 - c1) as usize + 1;
    let cap = rows.saturating_mul(cols).min(MAX_DENSE_RECT_KEYS);
    let mut v = Vec::with_capacity(cap);
    'outer: for r in r1..=r2 {
        for c in c1..=c2 {
            if v.len() >= MAX_DENSE_RECT_KEYS {
                break 'outer;
            }
            v.push((r, c));
        }
    }
    v
}

/// 稀疏矩形坐标收集：小区域（<= 20 万格）返回稠密坐标，超大区域仅收集实际非空或带样式的坐标。
pub fn sparse_rect_keys(sheet: &Sheet, r1: u32, c1: u32, r2: u32, c2: u32) -> Vec<(u32, u32)> {
    if r2 < r1 || c2 < c1 {
        return Vec::new();
    }
    let area = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
    if area <= 200_000 {
        return rect_keys(r1, c1, r2, c2);
    }
    let mut set = std::collections::BTreeSet::new();
    for (cref, _) in sheet.iter() {
        if cref.row >= r1 && cref.row <= r2 && cref.col >= c1 && cref.col <= c2 {
            set.insert((cref.row, cref.col));
        }
    }
    for cref in sheet.styles.keys() {
        if cref.row >= r1 && cref.row <= r2 && cref.col >= c1 && cref.col <= c2 {
            set.insert((cref.row, cref.col));
        }
    }
    set.into_iter().collect()
}
