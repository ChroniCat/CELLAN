//! 撤销/重做：记录「改之前」和「改之后」，按动作成组回退。
//!
//! 不存整本工作簿快照——152 MB 的文件拷一份就废了。每次动作只记受影响的格子，
//! 一个动作里可能有上万格（整列粘贴），但每格只存两个 `Cell`（40 字节）+ 两个样式号。
//!
//! 协作场景：撤销时如果某格当前值已经不是「我改成的样子」，说明别人后来动过，
//! 跳过它——不拿旧值盖掉别人的新编辑。

use crate::model::{Cell, CellRef, Sheet, Workbook};
use std::collections::HashSet;
#[path = "undo_references.rs"]
mod structure_refs;

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

type MergeRect = (u32, u32, u32, u32);

/// 合并/取消合并：只记差异，不整表快照。撤销/重做按差异增删，
/// 因此动作之后别人新增的无关合并不会被旧的整表列表覆盖。
#[derive(Clone, Debug)]
pub struct MergeEdit {
    sheets: Vec<SheetMergeDiff>,
}

#[derive(Clone, Debug)]
struct SheetMergeDiff {
    sheet: usize,
    added: Vec<MergeRect>,
    removed: Vec<MergeRect>,
}

impl MergeEdit {
    pub fn between(
        sheet: usize,
        before: &[MergeRect],
        after: &[MergeRect],
    ) -> Option<Self> {
        let previous: std::collections::HashSet<_> = before.iter().collect();
        let next: std::collections::HashSet<_> = after.iter().collect();
        let added: Vec<_> = after.iter().copied().filter(|r| !previous.contains(r)).collect();
        let removed: Vec<_> = before.iter().copied().filter(|r| !next.contains(r)).collect();
        (!added.is_empty() || !removed.is_empty()).then(|| Self {
            sheets: vec![SheetMergeDiff { sheet, added, removed }],
        })
    }

    /// Group diffs from distinct sheets into one reversible clipboard operation.
    pub fn join(edits: impl IntoIterator<Item = Self>) -> Option<Self> {
        let sheets: Vec<_> = edits.into_iter().flat_map(|edit| edit.sheets).collect();
        (!sheets.is_empty()).then_some(Self { sheets })
    }

    fn apply_to_owned_cells(
        &self,
        sheets: &mut [Sheet],
        deltas: &[CellDelta],
        guard: &mut impl FnMut(usize, u32, u32, &Cell) -> bool,
        forward: bool,
    ) -> Option<Vec<(usize, MergeRect, bool)>> {
        let blank = Cell::default();
        for d in deltas {
            let current = sheets.get(d.sheet)?.get(d.row, d.col).unwrap_or(&blank);
            if !guard(d.sheet, d.row, d.col, current) { return None; }
        }
        self.apply(sheets, forward)
    }

    /// Validate every affected sheet before installing any side of a cut.
    pub fn apply(&self, sheets: &mut [Sheet], forward: bool) -> Option<Vec<(usize, MergeRect, bool)>> {
        let mut plans = Vec::new();
        for diff in &self.sheets {
            let (add, remove) = if forward { (&diff.added, &diff.removed) } else { (&diff.removed, &diff.added) };
            let (next, changes) = plan_merge_diff(sheets.get(diff.sheet)?, add, remove)?;
            plans.push((diff.sheet, next, changes));
        }
        let mut applied = Vec::new();
        for (idx, next, changes) in plans {
            sheets[idx].merges = next;
            sheets[idx].merges_dirty = true;
            applied.extend(changes.into_iter().map(|(rect, on)| (idx, rect, on)));
        }
        Some(applied)
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
            .max(usize::from(self.refresh_sizes.is_some()))
    }

    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty()
            && self.snap_before.is_none()
            && self.shapes.is_none()
            && self.pictures.is_none()
            && self.merges.is_none()
            && self.refresh_sizes.is_none()
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
    /// Cell deltas that actually changed a format and must broadcast it to peers.
    pub style_changes: Vec<(usize, u32, u32)>,
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

const HISTORY_PAYLOAD_BUDGET: usize = 256 * 1024 * 1024;

static NEXT_ENTRY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Clone, Copy)]
struct EntryStamp {
    id: u64,
    payload_bytes: usize,
}

fn next_entry(payload_bytes: usize) -> EntryStamp {
    EntryStamp { id: NEXT_ENTRY.fetch_add(1, std::sync::atomic::Ordering::Relaxed), payload_bytes }
}

pub(crate) fn cell_heap_bytes(cell: &Cell) -> usize {
    cell.formula.as_deref().map_or(0, str::len) + match &cell.value {
        crate::Value::Text(text) => text.len(),
        _ => 0,
    }
}

/// Includes the retained heap size, which can be measured on a worker.
pub struct PreparedAction(Action, usize);

impl PreparedAction {
    /// Remove late replacement conflicts from a coordinate-sorted journal.
    /// Look up every index before swap-removing so surviving order is irrelevant.
    pub fn skip_sorted_cells(&mut self, conflicts: &[(usize,u32,u32)]) -> &[CellDelta] {
        let deltas = &mut self.0.deltas;
        let mut indices: Vec<_> = conflicts.iter().filter_map(|key|
            deltas.binary_search_by_key(key, |d| (d.sheet,d.row,d.col)).ok()).collect();
        indices.sort_unstable();
        indices.dedup();
        for i in indices.into_iter().rev() {
            let d = deltas.swap_remove(i);
            self.1 = self.1.saturating_sub(cell_heap_bytes(&d.before) + cell_heap_bytes(&d.after));
        }
        if let Some(d) = deltas.first() { self.0.focus = (d.sheet,d.row,d.col); }
        deltas
    }
}

impl Action {
    pub fn prepare(self) -> PreparedAction {
        let bytes = self.payload_bytes();
        PreparedAction(self, bytes)
    }

    fn payload_bytes(&self) -> usize {
        let mut bytes = self.deltas.capacity() * std::mem::size_of::<CellDelta>()
            + self.deltas.iter().map(|d| cell_heap_bytes(&d.before) + cell_heap_bytes(&d.after)).sum::<usize>();
        for snap in [&self.snap_before, &self.snap_after].into_iter().flatten() {
            bytes += std::mem::size_of_val(snap.cells.as_slice()) + std::mem::size_of_val(snap.merges.as_slice())
                + snap.cells.iter().map(|(_, _, cell, _)| cell_heap_bytes(cell)).sum::<usize>();
        }
        if let Some(edit) = &self.merges {
            bytes += edit.sheets.iter().map(|s| std::mem::size_of_val(s.added.as_slice())
                + std::mem::size_of_val(s.removed.as_slice())).sum::<usize>();
        }
        if let Some(edit) = &self.pictures {
            bytes += edit.before.iter().chain(&edit.after).map(|p|
                std::mem::size_of_val(p) + p.data.as_ref().map_or(0, |data| data.len())).sum::<usize>();
        }
        if let Some(edit) = &self.shapes {
            bytes += edit.before.iter().chain(&edit.after).map(|shape|
                std::mem::size_of_val(shape) + shape.text.len() + shape.name.len()).sum::<usize>();
        }
        if let Some((before, after)) = &self.refresh_sizes {
            bytes += (before.capacity() + after.capacity()) * std::mem::size_of::<(String, u32, u32)>()
                + before.iter().chain(after).map(|(s, _, _)| s.capacity()).sum::<usize>();
        }
        bytes
    }
}


enum Entry {
    Cells(EntryStamp, Action),
    NumericSort(EntryStamp, NumericSortAction),
    Structure(EntryStamp, StructureAction),
    SheetTab(EntryStamp, SheetTabAction),
}

impl Entry {
    fn id(&self) -> u64 { self.stamp().id }

    fn stamp(&self) -> EntryStamp {
        match self { Self::Cells(s, _) | Self::NumericSort(s, _) | Self::Structure(s, _) | Self::SheetTab(s, _) => *s }
    }

    fn simple_cells(&self) -> Option<Box<dyn Iterator<Item = (usize, u32, u32)> + '_>> {
        match self {
            Self::Cells(_, a) if a.snap_before.is_none() && a.snap_after.is_none()
                && a.merges.is_none() && a.shapes.is_none() && a.pictures.is_none()
                && a.refresh_sizes.is_none() => Some(Box::new(a.deltas.iter().map(|d| (d.sheet, d.row, d.col)))),
            Self::NumericSort(_, a) => Some(Box::new(a.sort.deltas().map(|d| (d.sheet, d.row, d.col)))),
            _ => None,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Cells(_, a) => &a.label,
            Self::NumericSort(_, a) => &a.label,
            Self::Structure(_, a) => &a.label,
            Self::SheetTab(_, a) => &a.label,
        }
    }
    fn by_site(&self) -> u32 {
        match self {
            Self::Cells(_, a) => a.by_site,
            Self::NumericSort(_, a) => a.by_site,
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

struct NumericSortAction {
    sort: crate::data_tools::CompactSort,
    label: String,
    focus: (usize, u32, u32),
    by_site: u32,
}

impl NumericSortAction {
    fn apply(&self, sheets: &mut [Sheet], redo: bool, mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool) -> Applied {
        let mut applied = Applied {
            touched: Vec::new(), style_changes: Vec::new(), label: self.label.clone(), focus: self.focus,
            skipped: 0, structural: false, refresh_sizes: None, merge_changes: Vec::new(), merge_conflict: false,
        };
        if let Some(sheet) = sheets.get_mut(self.sort.sheet()) {
            let mut apply = |d: CellDelta| {
                if !guard(d.sheet, d.row, d.col, sheet.get(d.row, d.col).unwrap_or(&Cell::default())) {
                    applied.skipped += 1;
                    return;
                }
                sheet.set(d.row, d.col, if redo { d.after } else { d.before });
                let style = if redo { d.after_style } else { d.before_style };
                if d.before_style != d.after_style && sheet.style_of(d.row, d.col) != style {
                    sheet.set_style(d.row, d.col, style);
                    applied.style_changes.push((d.sheet, d.row, d.col));
                }
                applied.touched.push((d.sheet, d.row, d.col));
            };
            if redo { self.sort.deltas().for_each(&mut apply); }
            else { self.sort.deltas().rev().for_each(apply); }
        }
        applied
    }
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
            structural: true, refresh_sizes: None, style_changes: Vec::new(), merge_changes: Vec::new(), merge_conflict: false,
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
    identity: u64,
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
    shift: crate::refs::Shift,
    reference_uses: structure_refs::Uses,
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
            foreign.push(ForeignReferences { sheet: si, identity: other.identity, cells, cf, dv, charts, sparklines });
        }
    }
    let shift = op.reference_shift();
    let reference_uses = structure_refs::collect(wb, sheet, &shift);
    StructureSnapshot {
        sheet, target, shift, reference_uses, foreign, names: wb.names.clone(), pivot_shifts: wb.pivot_shifts.clone(),
        pending_pivots: wb.pivots_pending.clone(),
    }
}

impl StructureSnapshot {
    /// Capture the same locations after applying the structural edit. In
    /// particular, a deleted reference is now #REF! and cannot be rediscovered
    /// by running the original reference matcher against the new workbook.
    pub fn capture_after(&self, wb: &Workbook) -> Self {
        let mut shift = self.shift.clone();
        // Subtotals can insert multiple rows; the first actual insertion is
        // the first boundary any later reference can cross.
        if let Some(journal) = wb.pivot_shifts.strip_prefix(self.pivot_shifts.as_slice()) {
            if let Some(first) = journal.iter().map(|entry| entry.2).min() { shift.at = first; }
        }
        shift.delete = !shift.delete;
        Self {
            sheet: self.sheet,
            target: wb.sheets[self.sheet].clone(),
            reference_uses: structure_refs::collect(wb, self.sheet, &shift),
            shift,
            foreign: self.foreign.iter().map(|scope| {
                let sheet = &wb.sheets[scope.sheet];
                ForeignReferences {
                    sheet: scope.sheet,
                    identity: sheet.identity,
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

fn chart_refs_match(current: &[crate::charts::Chart], expected: &[crate::charts::Chart]) -> bool {
    current.len() == expected.len() && current.iter().zip(expected).all(|(a, b)| {
        a.part == b.part && a.series.len() == b.series.len() && a.series.iter().zip(&b.series).all(|(s, t)|
            s.name_ref == t.name_ref && s.cat_ref == t.cat_ref && s.val_ref == t.val_ref)
    })
}

/// A row/column operation only changes series references. Retain the live
/// chart's presentation, cached values and save journal when changing them back.
fn restored_charts(current: &[crate::charts::Chart], desired: &[crate::charts::Chart]) -> Vec<crate::charts::Chart> {
    let mut restored = current.to_vec();
    for (chart, desired) in restored.iter_mut().zip(desired) {
        for (series, dest) in chart.series.iter_mut().zip(&desired.series) {
            for (from, to) in [(&mut series.name_ref, &dest.name_ref),
                (&mut series.cat_ref, &dest.cat_ref), (&mut series.val_ref, &dest.val_ref)] {
                if from != to {
                    chart.ref_fixes.push((from.clone(), to.clone()));
                    from.clone_from(to);
                }
            }
        }
    }
    restored
}

fn same_input(a: Option<&Cell>, b: Option<&Cell>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) if a.formula.is_some() => a.formula == b.formula,
        (Some(Cell { formula: None, value: crate::Value::Number(a) }),
         Some(Cell { formula: None, value: crate::Value::Number(b) })) => a.to_bits() == b.to_bits(),
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    }
}

/// Recalculation can replace formula caches and create or remove spill tails.
/// A literal written into a previous tail, even the same value, is user input.
fn sheet_inputs_match(a: &Sheet, b: &Sheet) -> bool {
    if a.cell_version() == b.cell_version() && a.spilled == b.spilled { return true; }
    fn input(sheet: &Sheet, k: CellRef) -> Option<&Cell> {
        if sheet.spilled.contains(&k) { None } else { sheet.get(k.row, k.col) }
    }
    a.changed_cells(b).chain(b.changed_cells(a)).chain(a.spilled.symmetric_difference(&b.spilled).copied())
        .all(|k| {
            // OOXML CSE tails can already contain cached numbers before their
            // first calculation registers them as spill output.
            let legacy_cache = a.spilled.contains(&k) && !b.spilled.contains(&k)
                && b.arrays.iter().any(|&(anchor, (r1, c1, r2, c2))| {
                    anchor != k && !b.spill_shapes.contains_key(&anchor)
                        && (r1..=r2).contains(&k.row) && (c1..=c2).contains(&k.col)
                }) && b.get(k.row, k.col).is_none_or(|cell| cell.formula.is_none());
            legacy_cache || same_input(input(a, k), input(b, k))
        })
}

fn target_matches(a: &Sheet, b: &Sheet) -> bool {
    a.identity == b.identity && a.name == b.name && a.arrays == b.arrays
        && a.tables == b.tables && a.pictures == b.pictures && a.merges == b.merges
        && a.cols == b.cols && a.rows == b.rows && a.styles == b.styles
        && a.col_styles == b.col_styles && a.row_styles == b.row_styles
        && a.cf == b.cf && a.dv == b.dv && a.notes == b.notes && a.freeze == b.freeze
        && a.filter == b.filter && a.hidden_rows == b.hidden_rows && a.hidden_rows_user == b.hidden_rows_user
        && a.hidden_cols == b.hidden_cols && a.scenarios == b.scenarios && a.ignored_errors == b.ignored_errors
        && a.outline_summary_below == b.outline_summary_below && a.outline_summary_right == b.outline_summary_right
        && a.outline_symbols == b.outline_symbols && a.row_levels == b.row_levels && a.col_levels == b.col_levels
        && a.page == b.page && chart_refs_match(&a.charts, &b.charts) && a.tab_color == b.tab_color
        && a.protection == b.protection && a.rich == b.rich && a.hyperlinks == b.hyperlinks
        && a.sparklines == b.sparklines && a.textboxes == b.textboxes && a.hidden == b.hidden
        && sheet_inputs_match(a, b)
}

impl StructureSnapshot {
    fn matches_live(&self, wb: &Workbook, desired: &Self) -> bool {
        wb.sheets.get(self.sheet).is_some_and(|s| target_matches(s, &self.target))
            && (self.pivot_shifts == desired.pivot_shifts || wb.pivot_shifts == self.pivot_shifts)
            && (self.pending_pivots == desired.pending_pivots || wb.pivots_pending == self.pending_pivots)
            && self.foreign.iter().all(|expected| {
                let Some(sheet) = wb.sheets.get(expected.sheet) else { return false };
                sheet.identity == expected.identity
                    && expected.cells.iter().all(|(k, cell)| same_input(sheet.get(k.row, k.col), (!cell.is_blank()).then_some(cell)))
                    && expected.cf.as_ref().is_none_or(|cf| &sheet.cf == cf)
                    && expected.dv.as_ref().is_none_or(|dv| &sheet.dv == dv)
                    && expected.charts.as_ref().is_none_or(|ch| chart_refs_match(&sheet.charts, ch))
                    && expected.sparklines.as_ref().is_none_or(|sp| &sheet.sparklines == sp)
            })
            && {
                // The before snapshot of a subtotal used the earliest possible
                // boundary; its after snapshot knows the actual first insertion.
                let mut shift = self.shift.clone();
                shift.at = shift.at.max(desired.shift.at);
                self.reference_uses.covers(wb, self.sheet, &shift)
            }
    }
}

type DefinedName = (String, Option<usize>, String);

/// Structural edits rewrite a definition's formula in place. Other definitions
/// can be created, deleted or reordered while the entry is in history.
fn changed_names(current: &[DefinedName], expected: &[DefinedName], desired: &[DefinedName]) -> Option<Vec<(usize, usize)>> {
    if expected.len() != desired.len() { return None; }
    if expected == desired { return Some(Vec::new()); }
    let mut indexes = std::collections::HashMap::with_capacity(current.len());
    for (index, (name, scope, _)) in current.iter().enumerate() {
        indexes.entry((name, scope)).and_modify(|unique| *unique = None).or_insert(Some(index));
    }
    let mut updates = Vec::new();
    for (i, (old, new)) in expected.iter().zip(desired).enumerate() {
        if old == new { continue; }
        if old.0 != new.0 || old.1 != new.1 { return None; }
        let live_index = (*indexes.get(&(&old.0, &old.1))?)?;
        if current[live_index].2 != old.2 { return None; }
        updates.push((live_index, i));
    }
    Some(updates)
}

fn restore_structure(wb: &mut Workbook, snap: &StructureSnapshot, expected: &StructureSnapshot) -> Option<Vec<(usize, u32, u32)>> {
    if !expected.matches_live(wb, snap) { return None; }
    let names = changed_names(&wb.names, &expected.names, &snap.names)?;
    let current = wb.sheets.get(snap.sheet)?;
    let mut target = snap.target.clone();
    let mut touched = HashSet::new();
    // Saving may have assigned a part to a previously new sheet. Undo changes
    // its content, not its identity or the current save baseline.
    target.identity = current.identity;
    target.source_part = current.source_part.clone();
    target.saved_cells.clone_from(&current.saved_cells);
    target.source_cells.clone_from(&current.source_cells);
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
    // The restored metadata must be written against today's package baseline,
    // including when the intermediate structural change was already saved.
    target.layout_dirty |= current.layout_dirty || target.rows != current.rows || target.cols != current.cols
        || target.hidden_rows != current.hidden_rows || target.hidden_rows_user != current.hidden_rows_user
        || target.hidden_cols != current.hidden_cols || target.row_levels != current.row_levels
        || target.col_levels != current.col_levels;
    target.rich_dirty |= current.rich_dirty || target.rich != current.rich;
    target.hyperlinks_dirty |= current.hyperlinks_dirty || target.hyperlinks != current.hyperlinks;
    target.tables_dirty |= current.tables_dirty || target.tables != current.tables;
    target.row_styles_dirty |= current.row_styles_dirty || target.row_styles != current.row_styles;
    target.col_styles_dirty |= current.col_styles_dirty || target.col_styles != current.col_styles;
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
    wb.names_dirty |= !names.is_empty();
    for (live, restored) in names {
        wb.names[live].2.clone_from(&snap.names[restored].2);
    }
    if expected.pivot_shifts != snap.pivot_shifts {
        wb.pivot_shifts.clone_from(&snap.pivot_shifts);
    }
    // A deleted source/field/output removes its refreshable definition. Restore
    // the full entry on undo; save keeps this session-owned list authoritative.
    if expected.pending_pivots != snap.pending_pivots {
        wb.pivots_pending.clone_from(&snap.pending_pivots);
    }
    wb.dirty = true;
    let mut touched: Vec<_> = touched.into_iter().collect();
    touched.sort_unstable();
    Some(touched)
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
        self.push_prepared(action.prepare());
    }

    pub fn push_prepared(&mut self, PreparedAction(action, bytes): PreparedAction) {
        if action.is_empty() {
            return;
        }
        // 记下是谁做的：协作时撤销只退自己的
        let mut action = action;
        if action.by_site == 0 {
            action.by_site = self.site;
        }
        self.done.push(Entry::Cells(next_entry(bytes), action));
        // 新动作让「重做」失效
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
    }

    /// Keep the newest operation undoable even when one very large edit exceeds the budget.
    /// Shared pages are charged at their full allocation so repeated large structural edits
    /// cannot keep hundreds of old workbook generations alive.
    fn trim_history(&mut self, budget: usize) {
        self.undone.clear();
        let limit = if self.limit == 0 { 200 } else { self.limit };
        let mut keep = 0;
        let mut bytes = 0usize;
        for entry in self.done.iter().rev().take(limit) {
            bytes = bytes.saturating_add(entry.stamp().payload_bytes);
            if keep > 0 && bytes > budget { break; }
            keep += 1;
        }
        self.done.drain(..self.done.len() - keep);
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
                if has_later_edit && (structural_barrier || entry.simple_cells().is_none_or(|mut keys|
                    keys.any(|key| later_cells.contains(&key)))) {
                    return false;
                }
            } else {
                has_later_edit = true;
                match entry.simple_cells() {
                    Some(keys) => later_cells.extend(keys),
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
        let bytes = match &change {
            SheetTabChange::Rename { before, after, references } => before.len() + after.len() + references.payload_bytes(),
            SheetTabChange::Append { .. } => 0,
        };
        self.done.push(Entry::SheetTab(next_entry(bytes), SheetTabAction {
            label: label.into(), index, identity, previous_focus, change, by_site: self.site,
        }));
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
    }

    pub fn push_structure(&mut self, label: impl Into<String>, before: StructureSnapshot, after: StructureSnapshot, focus: (usize, u32, u32)) {
        let bytes = before.target.pair_allocated_bytes(&after.target)
            + before.reference_uses.bytes() + after.reference_uses.bytes()
            + before.foreign.iter().chain(&after.foreign).map(|f| std::mem::size_of_val(f.cells.as_slice())
                + f.cells.iter().map(|(_, c)| cell_heap_bytes(c)).sum::<usize>()).sum::<usize>();
        self.done.push(Entry::Structure(next_entry(bytes), StructureAction { label: label.into(), before, after, focus, by_site: self.site }));
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
    }

    pub fn push_numeric_sort(&mut self, label: &str, sort: crate::data_tools::NumericSort, focus: (usize, u32, u32)) {
        self.push_compact_sort(label, crate::data_tools::CompactSort::Numeric(sort), focus);
    }

    pub fn push_compact_sort(&mut self, label: &str, sort: crate::data_tools::CompactSort, focus: (usize, u32, u32)) {
        let stamp = next_entry(sort.bytes());
        self.done.push(Entry::NumericSort(stamp, NumericSortAction {
            sort, label: label.into(), focus, by_site: self.site,
        }));
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
    }

    pub fn undo_workbook(&mut self, wb: &mut Workbook, guard: impl FnMut(usize, u32, u32, &Cell) -> bool) -> Option<Applied> {
        if !self.undo_is_structure() { return self.undo(&mut wb.sheets, guard); }
        if self.done.last().is_some_and(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site) { return None; }
        if let Some(Entry::SheetTab(_, action)) = self.done.last_mut() {
            let applied = action.apply(wb, false)?;
            self.undone.push(self.done.pop()?);
            return Some(applied);
        }
        let Entry::Structure(_, action) = self.done.last()? else { unreachable!() };
        let touched = restore_structure(wb, &action.before, &action.after)?;
        let applied = Applied { touched, label: action.label.clone(), focus: action.focus, skipped: 0, structural: true, refresh_sizes: None, style_changes: Vec::new(), merge_changes: Vec::new(), merge_conflict: false };
        self.undone.push(self.done.pop()?);
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
        let Entry::Structure(_, action) = self.undone.last()? else { unreachable!() };
        let touched = restore_structure(wb, &action.after, &action.before)?;
        let applied = Applied { touched, label: action.label.clone(), focus: action.focus, skipped: 0, structural: true, refresh_sizes: None, style_changes: Vec::new(), merge_changes: Vec::new(), merge_conflict: false };
        self.done.push(self.undone.pop()?);
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
        if let Some(Entry::NumericSort(_, action)) = self.done.last() {
            let applied = action.apply(sheets, false, guard);
            self.undone.push(self.done.pop()?);
            return Some(applied);
        }
        let Entry::Cells(id, action) = self.done.pop()? else { unreachable!() };
        let mut touched = Vec::new();
        let mut skipped = 0;
        let mut merge_changes = Vec::new();
        let mut style_changes = Vec::new();
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
                style_changes, merge_changes,
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
                style_changes, merge_changes,
                merge_conflict: false,
            };
            self.undone.push(Entry::Cells(id, action));
            return Some(applied);
        }
        let merged = action.merges.as_ref().map(|edit| {
            edit.apply_to_owned_cells(sheets, &action.deltas, &mut guard, false)
        });
        let merge_conflict = matches!(merged, Some(None));
        if let Some(Some(changes)) = merged { merge_changes = changes; }
        if merge_conflict {
            let label = action.label.clone();
            let focus = action.focus;
            self.done.push(Entry::Cells(id, action));
            return Some(Applied { touched, label, focus, skipped: 0, structural: false, refresh_sizes: None, style_changes, merge_changes, merge_conflict: true });
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
                style_changes, merge_changes,
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
            if action.merges.is_none() && !guard(d.sheet, d.row, d.col, &cur) {
                skipped += 1;
                continue;
            }
            sheet.set(d.row, d.col, d.before.clone());
            if d.before_style != d.after_style && sheet.style_of(d.row, d.col) != d.before_style {
                sheet.set_style(d.row, d.col, d.before_style);
                style_changes.push((d.sheet, d.row, d.col));
            }
            touched.push((d.sheet, d.row, d.col));
        }
        let applied = Applied {
            touched,
            label: action.label.clone(),
            focus: action.focus,
            skipped,
            structural: false,
            refresh_sizes: action.refresh_sizes.clone().map(|(b, _)| b),
            style_changes, merge_changes,
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
        if let Some(Entry::NumericSort(_, action)) = self.undone.last() {
            let applied = action.apply(sheets, true, guard);
            self.done.push(self.undone.pop()?);
            return Some(applied);
        }
        let Entry::Cells(id, action) = self.undone.pop()? else { unreachable!() };
        let mut touched = Vec::new();
        let mut skipped = 0;
        let mut merge_changes = Vec::new();
        let mut style_changes = Vec::new();
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
                style_changes, merge_changes,
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
                style_changes, merge_changes,
                merge_conflict: false,
            };
            self.done.push(Entry::Cells(id, action));
            return Some(applied);
        }
        let merged = action.merges.as_ref().map(|edit| {
            edit.apply_to_owned_cells(sheets, &action.deltas, &mut guard, true)
        });
        let merge_conflict = matches!(merged, Some(None));
        if let Some(Some(changes)) = merged { merge_changes = changes; }
        if merge_conflict {
            let label = action.label.clone();
            let focus = action.focus;
            self.undone.push(Entry::Cells(id, action));
            return Some(Applied { touched, label, focus, skipped: 0, structural: false, refresh_sizes: None, style_changes, merge_changes, merge_conflict: true });
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
                style_changes, merge_changes,
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
            if action.merges.is_none() && !guard(d.sheet, d.row, d.col, &cur) {
                skipped += 1;
                continue;
            }
            sheet.set(d.row, d.col, d.after.clone());
            if d.before_style != d.after_style && sheet.style_of(d.row, d.col) != d.after_style {
                sheet.set_style(d.row, d.col, d.after_style);
                style_changes.push((d.sheet, d.row, d.col));
            }
            touched.push((d.sheet, d.row, d.col));
        }
        let applied = Applied {
            touched,
            label: action.label.clone(),
            focus: action.focus,
            skipped,
            structural: false,
            refresh_sizes: action.refresh_sizes.clone().map(|(_, after)| after),
            style_changes, merge_changes,
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
    fn value_only_undo_keeps_changed_inherited_format_and_omits_style_broadcast() {
        let mut wb = wb();
        let mut st = UndoStack::new();
        wb.sheets[0].col_styles.insert(0, 1);
        st.push(edit(&mut wb, 0, 0, 42.0));
        wb.sheets[0].col_styles.insert(0, 2);
        let undone = st.undo(&mut wb.sheets, |_, _, _, _| true).unwrap();
        assert_eq!(undone.touched, [(0, 0, 0)]);
        assert!(undone.style_changes.is_empty());
        let redone = st.redo(&mut wb.sheets, |_, _, _, _| true).unwrap();
        assert!(redone.style_changes.is_empty());
        assert_eq!(wb.sheets[0].style_of(0, 0), 2);
        assert_eq!(wb.sheets[0].explicit_style_of(0, 0), None);
    }

    #[test]
    fn history_budget_evicts_old_edits_and_keeps_the_latest_undoable() {
        let mut wb = wb();
        let mut history = UndoStack::new();
        for value in [2.0, 3.0, 4.0] {
            history.push(edit(&mut wb, 0, 0, value));
        }
        let cost = history.done.last().unwrap().stamp().payload_bytes;
        history.trim_history(cost * 2);
        assert_eq!(history.done.len(), 2);
        history.trim_history(0);
        assert_eq!(history.done.len(), 1);
        history.undo(&mut wb.sheets, |_, _, _, _| true).unwrap();
        assert_eq!(wb.sheets[0].value(0, 0), Value::Number(3.0));
        history.redo(&mut wb.sheets, |_, _, _, _| true).unwrap();
        assert_eq!(wb.sheets[0].value(0, 0), Value::Number(4.0));
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
