//! 撤销/重做：记录「改之前」和「改之后」，按动作成组回退。
//!
//! 不存整本工作簿快照——152 MB 的文件拷一份就废了。每次动作只记受影响的格子，
//! 一个动作里可能有上万格（整列粘贴），但每格只存两个 `Cell`（40 字节）+ 两个样式号。
//!
//! 协作场景：撤销时如果某格当前值已经不是「我改成的样子」，说明别人后来动过，
//! 跳过它——不拿旧值盖掉别人的新编辑。

#[path = "undo_structural_cells.rs"]
mod structural_cells;
#[path = "undo_move_refs.rs"]
mod move_refs;
pub use move_refs::MoveReferences;
#[path = "undo_structural_history.rs"]
pub(crate) mod structural_history;
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

impl CellDelta {
    fn can_replay(&self, sheet: &Sheet, redo: bool, guard: &mut impl FnMut(usize,u32,u32,&Cell) -> bool) -> bool {
        let blank = Cell::default();
        let current = sheet.get(self.row, self.col);
        let current = current.as_deref().unwrap_or(&blank);
        let (expected, style) = if redo { (&self.before,self.before_style) } else { (&self.after,self.after_style) };
        current.same_input(expected)
            && (self.before_style == self.after_style || sheet.style_of(self.row,self.col) == style)
            && guard(self.sheet,self.row,self.col,current)
    }

    fn replay(&self, sheet: &mut Sheet, redo: bool, style_changes: &mut Vec<(usize,u32,u32)>) {
        if !self.before.same_input(&self.after) {
            sheet.set(self.row, self.col, if redo { self.after.clone() } else { self.before.clone() });
        }
        if self.before_style != self.after_style {
            sheet.set_style(self.row,self.col,if redo { self.after_style } else { self.before_style });
            style_changes.push((self.sheet,self.row,self.col));
        }
    }
}

/// 整表快照（结构性改动用：删行之后没法靠「逐格回填」还原）
#[derive(Clone, Debug, Default)]
pub struct SheetSnapshot {
    pub sheet: usize,
    pub cells: Vec<(u32, u32, Cell, u16)>,
    pub merges: Vec<(u32, u32, u32, u32)>,
    pub notes: Vec<((u32, u32), crate::notes::Note)>,
    pub threads: Vec<((u32, u32), crate::notes::ThreadedComment)>,
}

pub fn snapshot_sheet(sheet: &Sheet, idx: usize) -> SheetSnapshot {
    SheetSnapshot {
        sheet: idx,
        cells: sheet
            .iter_views()
            .map(|(k, c)| {
                (
                    k.row,
                    k.col,
                    c.to_cell(),
                    sheet.explicit_style_of(k.row, k.col).unwrap_or(0),
                )
            })
            .collect(),
        merges: sheet.merges.to_vec(),
        notes: sheet.notes.iter().map(|(k, v)| ((k.row, k.col), v.clone())).collect(),
        threads: sheet.threads.iter().map(|(k, v)| ((k.row, k.col), v.clone())).collect(),
    }
}

pub fn restore_sheet(sheet: &mut Sheet, snap: &SheetSnapshot) {
    let old_keys: Vec<CellRef> = sheet.iter_views().map(|(k, _)| k).collect();
    let old_styles: Vec<CellRef> = sheet.styles.iter().map(|(k, _)| k).collect();
    sheet.clear();
    for k in old_keys {
        sheet.cleared.insert(k);
    }
    for k in old_styles {
        sheet.set_style(k.row, k.col, 0);
    }
    for (r, c, cell, st) in snap.cells.iter() {
        sheet.set(*r, *c, cell.clone());
        if *st != 0 {
            sheet.set_style(*r, *c, *st);
        }
    }
    sheet.merges = snap.merges.clone().into();
    sheet.merges_dirty = true;
    sheet.notes.clear();
    for &((r, c), ref n) in &snap.notes {
        sheet.notes.insert(CellRef::new(r, c), n.clone());
    }
    sheet.notes_dirty = true;
    sheet.threads.clear();
    for &((r, c), ref tc) in &snap.threads {
        sheet.threads.insert(CellRef::new(r, c), tc.clone());
    }
    sheet.threads_dirty = true;
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
        for d in deltas {
            if !d.can_replay(sheets.get(d.sheet)?, forward, guard) { return None; }
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
) -> Option<(crate::merges::Merges, Vec<((u32, u32, u32, u32), bool)>)> {
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
    pending_compaction: bool,
    /// 最多保留多少步
    pub limit: usize,
    /// 我是谁（协作站点号）；0 表示单人。撤销只退自己的动作
    pub site: u32,
    /// 最近一次 MetaEdit 撤销/重做产出的协作元数据广播载荷
    last_meta_sync: Vec<(usize, crate::sync::SheetMetaKind)>,
    /// 最近一次撤销/重做涉及定义名称等需触发重算的簿级变更
    last_needs_recalc: bool,
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
    /// True when grid coordinates or sheet indices changed (structural, sheet-tab,
    /// or sparse partial cell-shift replay).
    pub coord_changed: bool,
    /// 撤销/重做后要写回的外源源尺寸（名字, 行, 列）
    pub refresh_sizes: Option<Vec<(String, u32, u32)>>,
    /// 撤销/重做实际发生的合并结构变化：(表, 矩形, 是否新增)
    pub merge_changes: Vec<(usize, (u32, u32, u32, u32), bool)>,
    /// 合并结构与当前表冲突，整个动作（含 CellDelta）未应用
    pub merge_conflict: bool,
    /// Exact changed cell count when offline CoW fast-swap caps `touched` to a 2,001-cell sentinel.
    total_touched: usize,
}

impl Applied {
    /// Total number of cells changed by this undo/redo step (exact even when `touched` is capped in offline CoW fast-swap).
    #[inline]
    pub fn touched_count(&self) -> usize {
        self.total_touched.max(self.touched.len())
    }
}

const HISTORY_PAYLOAD_BUDGET: usize = 256 * 1024 * 1024;

static NEXT_ENTRY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct EntryStamp {
    link_source: Option<Box<crate::external::Source>>,
    move_refs: Option<Box<MoveReferences>>,
    sort_created: Option<u64>,
    sort_replay: Option<(u64, bool, Option<u64>)>,
    id: u64,
    payload_bytes: usize,
}

fn next_entry(payload_bytes: usize) -> EntryStamp {
    EntryStamp { link_source: None, move_refs: None, sort_created: None, sort_replay: None, id: NEXT_ENTRY.fetch_add(1, std::sync::atomic::Ordering::Relaxed), payload_bytes }
}

impl EntryStamp {
    fn link_matches(&self, wb: &Workbook, redo: bool) -> bool {
        self.link_source.as_deref().is_none_or(|src| {
            wb.sources.iter().find(|s| s.name == src.name) == if redo { None } else { Some(src) }
        })
    }

    fn replay_link(&self, wb: &mut Workbook, redo: bool) {
        if let Some(src) = self.link_source.as_deref() {
            if redo {
                wb.sources.push(src.clone());
            } else {
                wb.sources.retain(|s| s.name != src.name);
            }
        }
    }
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


/// 多表批注与线程会话变更记录：可附着在单元格批量动作（复制/剪切/清除/排序/拖动）上，
/// 与 `CellDelta` 同步单步原子撤销/重做，并自动产出协作广播载荷。
#[derive(Clone, Debug, Default)]
pub struct NoteUndoEdit {
    pub notes: Vec<(usize, (u32, u32), Option<crate::notes::Note>, Option<crate::notes::Note>)>,
    pub threads: Vec<(
        usize,
        (u32, u32),
        Option<crate::notes::ThreadedComment>,
        Option<crate::notes::ThreadedComment>,
    )>,
}

impl NoteUndoEdit {
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty() && self.threads.is_empty()
    }

    pub fn payload_bytes(&self) -> usize {
        let note_b: usize = self
            .notes
            .iter()
            .map(|(_, _, b, a)| {
                std::mem::size_of::<(usize, (u32, u32), Option<crate::notes::Note>, Option<crate::notes::Note>)>()
                    + b.as_ref().map_or(0, |n| n.text.len() + n.author.len())
                    + a.as_ref().map_or(0, |n| n.text.len() + n.author.len())
            })
            .sum();
        let thread_b: usize = self
            .threads
            .iter()
            .map(|(_, _, b, a)| {
                let sz = |opt: &Option<crate::notes::ThreadedComment>| -> usize {
                    opt.as_ref().map_or(0, |tc| {
                        tc.entries
                            .iter()
                            .map(|e| e.id.len() + e.author.len() + e.timestamp.len() + e.text.len() + 96)
                            .sum()
                    })
                };
                64 + sz(b) + sz(a)
            })
            .sum();
        note_b + thread_b
    }

    pub fn extend(&mut self, other: NoteUndoEdit) {
        self.notes.extend(other.notes);
        self.threads.extend(other.threads);
    }

    pub fn between_sheet(sheet_idx: usize, before: &Sheet, after: &Sheet) -> Option<Self> {
        if before.notes == after.notes && before.threads == after.threads {
            return None;
        }
        let mut out = Self::default();
        if before.notes != after.notes {
            let mut keys: Vec<CellRef> = before
                .notes
                .keys()
                .chain(after.notes.keys())
                .copied()
                .collect();
            keys.sort_unstable_by_key(|k| (k.row, k.col));
            keys.dedup();
            for k in keys {
                let b = before.notes.get(&k).cloned();
                let a = after.notes.get(&k).cloned();
                if b != a {
                    out.notes.push((sheet_idx, (k.row, k.col), b, a));
                }
            }
        }
        if before.threads != after.threads {
            let mut keys: Vec<CellRef> = before
                .threads
                .keys()
                .chain(after.threads.keys())
                .copied()
                .collect();
            keys.sort_unstable_by_key(|k| (k.row, k.col));
            keys.dedup();
            for k in keys {
                let b = before.threads.get(&k).cloned();
                let a = after.threads.get(&k).cloned();
                if b != a {
                    out.threads.push((sheet_idx, (k.row, k.col), b, a));
                }
            }
        }
        (!out.is_empty()).then_some(out)
    }

    pub fn apply_on_sheet(&self, sheet: &mut Sheet, redo: bool) {
        if !self.notes.is_empty() {
            let mut any = false;
            let iter: Box<dyn Iterator<Item = _>> = if redo {
                Box::new(self.notes.iter())
            } else {
                Box::new(self.notes.iter().rev())
            };
            for &(_, (r, c), ref before, ref after) in iter {
                let k = CellRef::new(r, c);
                match if redo { after } else { before } {
                    Some(n) => {
                        sheet.notes.insert(k, n.clone());
                    }
                    None => {
                        sheet.notes.remove(&k);
                    }
                }
                any = true;
            }
            if any {
                sheet.notes_dirty = true;
                sheet.bump_display_style_revision();
            }
        }
        if !self.threads.is_empty() {
            let mut any = false;
            let iter: Box<dyn Iterator<Item = _>> = if redo {
                Box::new(self.threads.iter())
            } else {
                Box::new(self.threads.iter().rev())
            };
            for &(_, (r, c), ref before, ref after) in iter {
                let k = CellRef::new(r, c);
                match if redo { after } else { before } {
                    Some(tc) => {
                        sheet.threads.insert(k, tc.clone());
                    }
                    None => {
                        sheet.threads.remove(&k);
                    }
                }
                any = true;
            }
            if any {
                sheet.threads_dirty = true;
                sheet.bump_display_style_revision();
            }
        }
    }

    pub fn to_meta_sync(&self, redo: bool) -> Vec<(usize, crate::sync::SheetMetaKind)> {
        use crate::sync::SheetMetaKind;
        let mut syncs = Vec::with_capacity(self.notes.len() + self.threads.len());
        let thread_cells: std::collections::HashSet<_> = self.threads.iter()
            .map(|(sheet, pos, _, _)| (*sheet, *pos)).collect();
        let mut note_targets = std::collections::HashMap::new();
        let note_iter: Box<dyn Iterator<Item = _>> = if redo {
            Box::new(self.notes.iter())
        } else {
            Box::new(self.notes.iter().rev())
        };
        for &(sheet, (row, col), ref before, ref after) in note_iter {
            let note = if redo { after } else { before };
            if thread_cells.contains(&(sheet, (row, col))) {
                note_targets.insert((sheet, (row, col)), (if redo { before } else { after }, note));
            } else {
                syncs.push((sheet, SheetMetaKind::Note { row, col,
                    author: note.as_ref().map_or_else(String::new, |n| n.author.clone()),
                    text: note.as_ref().map(|n| n.text.clone()),
                }));
            }
        }
        let thread_iter: Box<dyn Iterator<Item = _>> = if redo {
            Box::new(self.threads.iter())
        } else {
            Box::new(self.threads.iter().rev())
        };
        for &(sheet, (row, col), ref before, ref after) in thread_iter {
            let (before, after) = if redo { (before, after) } else { (after, before) };
            let notes = note_targets.get(&(sheet, (row, col)));
            let before_note = before.is_none().then(|| notes.and_then(|(n, _)| n.as_ref()).cloned()).flatten();
            let legacy_note = after.is_none().then(|| notes.and_then(|(_, n)| n.as_ref()).cloned()).flatten();
            syncs.push((sheet, SheetMetaKind::ThreadChange { row, col,
                before: before.clone(), after: after.clone(), before_note, legacy_note,
            }));
        }
        syncs
    }

    pub fn apply(&self, sheets: &mut [Sheet], redo: bool) -> Vec<(usize, crate::sync::SheetMetaKind)> {
        use crate::sync::SheetMetaKind;
        let mut syncs = self.to_meta_sync(redo);
        let notes: std::collections::HashMap<_, _> = self.notes.iter().map(|(si, pos, before, after)|
            ((*si, *pos), if redo { after.is_some() } else { before.is_some() })).collect();
        let mut dirty = std::collections::HashSet::new();
        syncs.retain(|(si, kind)| {
            let Some(sh) = sheets.get_mut(*si) else { return false };
            match kind {
                SheetMetaKind::Note { row, col, author, text } => {
                    let key = CellRef::new(*row, *col);
                    if sh.threads.contains_key(&key) { return false; }
                    if let Some(text) = text {
                        sh.notes.insert(key, crate::notes::Note { r: *row, c: *col, author: author.clone(), text: text.clone() });
                    } else {
                        sh.notes.remove(&key);
                    }
                    sh.notes_dirty = true;
                }
                SheetMetaKind::ThreadChange { row, col, before, after, before_note, legacy_note } => {
                    let key = CellRef::new(*row, *col);
                    let summary = notes.get(&(*si, (*row, *col))).copied().unwrap_or_else(|| sh.notes.contains_key(&key));
                    if !apply_thread_change(sh, key, before.as_ref(), before_note.as_ref(), after.as_ref(), legacy_note.as_ref(), summary) { return false; }
                }
                _ => unreachable!(),
            }
            dirty.insert(*si);
            true
        });
        for si in dirty { sheets[si].bump_display_style_revision(); }
        syncs
    }

    fn apply_history(&mut self, sheets: &mut [Sheet], redo: bool, skipped: &mut usize) -> Vec<(usize, crate::sync::SheetMetaKind)> {
        use crate::sync::SheetMetaKind;
        let syncs = self.apply(sheets, redo);
        let applied: std::collections::HashSet<_> = syncs.iter().filter_map(|(sheet, kind)| match kind {
            SheetMetaKind::Note { row, col, .. } | SheetMetaKind::ThreadChange { row, col, .. } => Some((*sheet, (*row, *col))),
            _ => None,
        }).collect();
        let mut conflicts = std::collections::HashSet::new();
        let mut keep = |sheet, pos| {
            if applied.contains(&(sheet, pos)) { return true; }
            conflicts.insert((sheet, pos));
            false
        };
        self.threads.retain(|(sheet, pos, _, _)| keep(*sheet, *pos));
        self.notes.retain(|(sheet, pos, _, _)| keep(*sheet, *pos));
        *skipped += conflicts.len();
        syncs
    }
}

fn apply_thread_change(
    sh: &mut Sheet, key: CellRef, before: Option<&crate::notes::ThreadedComment>,
    before_note: Option<&crate::notes::Note>, after: Option<&crate::notes::ThreadedComment>, legacy_note: Option<&crate::notes::Note>,
    target_has_summary: bool,
) -> bool {
    if !crate::notes::traditional_baseline_matches(sh.threads.get(&key), sh.notes.get(&key), before, before_note, after, legacy_note) {
        return false;
    }
    let Ok(merged) = crate::notes::merge_thread_change(sh.threads.get(&key), before, after) else { return false; };
    if let Some(tc) = merged {
        // Imported threads may have no traditional summary. Restore that representation on
        // exact replay, but derive a fresh summary whenever a concurrent reply was retained.
        if target_has_summary || Some(&tc) != after { sh.notes.insert(key, tc.summary_note()); }
        else { sh.notes.remove(&key); }
        sh.threads.insert(key, tc);
    } else {
        sh.threads.remove(&key);
        if let Some(note) = legacy_note { sh.notes.insert(key, note.clone()); } else { sh.notes.remove(&key); }
    }
    sh.notes_dirty = true;
    sh.threads_dirty = true;
    true

}

/// 轻量级工作表元数据改动（列宽/行高/隐藏行列/批注/线程批注/忽略错误/条件格式）：无需整表快照，支持单人及协作模式一键撤销。
#[derive(Clone, Debug, Default)]
pub struct MetaEdit {
    pub sheet: usize,
    pub col_widths: Vec<(u32, Option<f32>, Option<f32>)>,
    pub row_heights: Vec<(u32, Option<f32>, Option<f32>)>,
    pub hidden_rows_user: Vec<(u32, bool, bool)>,
    pub hidden_cols: Vec<(u32, bool, bool)>,
    pub notes: Vec<((u32, u32), Option<crate::notes::Note>, Option<crate::notes::Note>)>,
    pub threads: Vec<(
        (u32, u32),
        Option<crate::notes::ThreadedComment>,
        Option<crate::notes::ThreadedComment>,
    )>,
    pub ignored_errors: Vec<((u32, u32), bool, bool)>,
    pub cf: Option<(crate::cf::CfSheet, crate::cf::CfSheet)>,
    pub filter: Option<(
        Option<crate::sort_filter::Filter>,
        Option<crate::sort_filter::Filter>,
        crate::layout::HiddenIndices,
        crate::layout::HiddenIndices,
    )>,
    pub tables: Option<(Vec<crate::table::Table>, Vec<crate::table::Table>)>,
    pub outline: Option<(
        std::collections::HashMap<u32, u8>,
        std::collections::HashMap<u32, u8>,
        std::collections::HashMap<u32, u8>,
        std::collections::HashMap<u32, u8>,
    )>,
    pub rich: Vec<(
        (u32, u32),
        Cell,
        Cell,
        Option<Vec<crate::richtext::RichRun>>,
        Option<Vec<crate::richtext::RichRun>>,
    )>,
    pub dv: Option<(Vec<crate::dv::DataValidation>, Vec<crate::dv::DataValidation>)>,
    pub hyperlinks: Vec<(
        (u32, u32),
        Option<crate::hyperlinks::Hyperlink>,
        Option<crate::hyperlinks::Hyperlink>,
    )>,
    pub sparklines: Option<(Vec<crate::sparkline::Sparkline>, Vec<crate::sparkline::Sparkline>)>,
    pub names: Option<(
        Vec<(String, Option<usize>, String)>,
        Vec<(String, Option<usize>, String)>,
    )>,
}

impl MetaEdit {
    pub fn is_empty(&self) -> bool {
        self.col_widths.is_empty()
            && self.row_heights.is_empty()
            && self.hidden_rows_user.is_empty()
            && self.hidden_cols.is_empty()
            && self.notes.is_empty()
            && self.threads.is_empty()
            && self.ignored_errors.is_empty()
            && self.cf.is_none()
            && self.filter.is_none()
            && self.tables.is_none()
            && self.outline.is_none()
            && self.rich.is_empty()
            && self.dv.is_none()
            && self.hyperlinks.is_empty()
            && self.sparklines.is_none()
            && self.names.is_none()
    }

    pub(crate) fn apply_with_guard(
        &mut self,
        sheets: &mut [Sheet],
        redo: bool,
        guard: &mut impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> (
        Vec<(usize, crate::sync::SheetMetaKind)>,
        Vec<(usize, u32, u32)>,
        std::collections::HashSet<usize>,
        usize,
    ) {
        use crate::sync::SheetMetaKind;
        let mut syncs = Vec::new();
        let mut rich_touched = Vec::new();
        let mut rich_skipped = std::collections::HashSet::new();
        let Some(sh) = sheets.get_mut(self.sheet) else {
            return (syncs, rich_touched, rich_skipped, 0);
        };
        if !self.col_widths.is_empty() {
            let def = sh.cols.default_size();
            let mut items = Vec::with_capacity(self.col_widths.len());
            for &(c, before, after) in &self.col_widths {
                let target = if redo { after } else { before };
                let w = target.unwrap_or(def);
                sh.cols.set(c, w);
                items.push((c, w));
            }
            sh.layout_dirty = true;
            sh.bump_display_style_revision();
            syncs.push((self.sheet, SheetMetaKind::ColWidths(items)));
        }
        if !self.row_heights.is_empty() {
            let def = sh.rows.default_size();
            let mut items = Vec::with_capacity(self.row_heights.len());
            for &(r, before, after) in &self.row_heights {
                let target = if redo { after } else { before };
                let h = target.unwrap_or(def);
                sh.rows.set(r, h);
                items.push((r, h));
            }
            sh.layout_dirty = true;
            sh.bump_display_style_revision();
            syncs.push((self.sheet, SheetMetaKind::RowHeights(items)));
        }
        if !self.hidden_rows_user.is_empty() {
            let mut items = Vec::with_capacity(self.hidden_rows_user.len());
            for &(r, before, after) in &self.hidden_rows_user {
                let hide = if redo { after } else { before };
                if hide {
                    sh.hidden_rows_user.insert(r);
                } else {
                    sh.hidden_rows_user.remove(&r);
                }
                items.push((r, hide));
            }
            sh.layout_dirty = true;
            sh.bump_display_style_revision();
            syncs.push((self.sheet, SheetMetaKind::HiddenRows(items)));
        }
        if !self.hidden_cols.is_empty() {
            let mut items = Vec::with_capacity(self.hidden_cols.len());
            for &(c, before, after) in &self.hidden_cols {
                let hide = if redo { after } else { before };
                if hide {
                    sh.hidden_cols.insert(c);
                } else {
                    sh.hidden_cols.remove(&c);
                }
                items.push((c, hide));
            }
            sh.layout_dirty = true;
            sh.bump_display_style_revision();
            syncs.push((self.sheet, SheetMetaKind::HiddenCols(items)));
        }
        let thread_cells: std::collections::HashSet<_> = self.threads.iter().map(|(pos, _, _)| *pos).collect();
        let mut conflicted = std::collections::HashSet::new();
        if !self.notes.is_empty() {
            for (pos, before, after) in &self.notes {
                // A threaded comment and its legacy summary must change together.
                if thread_cells.contains(pos) { continue; }
                if sh.threads.contains_key(&CellRef::new(pos.0, pos.1)) {
                    conflicted.insert(*pos);
                    continue;
                }
                let target = if redo { after } else { before };
                let cref = CellRef::new(pos.0, pos.1);
                match target {
                    Some(n) => {
                        sh.notes.insert(cref, n.clone());
                        syncs.push((
                            self.sheet,
                            SheetMetaKind::Note {
                                row: pos.0,
                                col: pos.1,
                                author: n.author.clone(),
                                text: Some(n.text.clone()),
                            },
                        ));
                    }
                    None => {
                        sh.notes.remove(&cref);
                        syncs.push((
                            self.sheet,
                            SheetMetaKind::Note {
                                row: pos.0,
                                col: pos.1,
                                author: String::new(),
                                text: None,
                            },
                        ));
                    }
                }
            }
            sh.notes_dirty = true;
            sh.bump_display_style_revision();
        }
        if !self.threads.is_empty() {
            let note_targets: std::collections::HashMap<_, _> = self.notes.iter()
                .map(|(pos, before, after)| (*pos, if redo { (before, after) } else { (after, before) })).collect();
            for (pos, before, after) in &self.threads {
                let (source, target) = if redo { (before, after) } else { (after, before) };
                let key = CellRef::new(pos.0, pos.1);
                let notes = note_targets.get(pos);
                let before_note = source.is_none().then(|| notes.and_then(|(n, _)| n.as_ref()).cloned()).flatten();
                let legacy_note = target.is_none().then(|| notes.and_then(|(_, n)| n.as_ref()).cloned()).flatten();
                let summary = notes.map_or_else(|| sh.notes.contains_key(&key), |(_, n)| n.is_some());
                if !apply_thread_change(sh, key, source.as_ref(), before_note.as_ref(), target.as_ref(), legacy_note.as_ref(), summary) {
                    conflicted.insert(*pos);
                    continue;
                }
                sh.bump_display_style_revision();
                syncs.push((self.sheet, SheetMetaKind::ThreadChange {
                    row: pos.0, col: pos.1, before: source.clone(), after: target.clone(), before_note, legacy_note,
                }));
            }
        }
        if !conflicted.is_empty() {
            self.threads.retain(|(pos, _, _)| !conflicted.contains(pos));
            self.notes.retain(|(pos, _, _)| !conflicted.contains(pos));
        }
        if !self.ignored_errors.is_empty() {
            for &(pos, before, after) in &self.ignored_errors {
                let ig = if redo { after } else { before };
                if ig {
                    sh.ignored_errors.insert(pos);
                } else {
                    sh.ignored_errors.remove(&pos);
                }
            }
            sh.bump_display_style_revision();
        }
        if let Some((before, after)) = &self.cf {
            sh.cf = if redo { after.clone() } else { before.clone() };
            sh.cf_dirty = true;
            sh.bump_display_style_revision();
        }
        if let Some((fb, fa, hb, ha)) = &self.filter {
            sh.filter = if redo { fa.clone() } else { fb.clone() };
            sh.hidden_rows = if redo { ha.clone() } else { hb.clone() };
            sh.filter_dirty = true;
            sh.layout_dirty = true;
            sh.bump_display_style_revision();
            syncs.push((
                self.sheet,
                SheetMetaKind::AutoFilter {
                    range: sh.filter.as_ref().map(|f| f.range),
                },
            ));
        }
        if let Some((tb, ta)) = &self.tables {
            sh.tables = if redo { ta.clone() } else { tb.clone() };
            sh.tables_dirty = true;
            sh.bump_display_style_revision();
        }
        if let Some((rb, ra, cb, ca)) = &self.outline {
            sh.row_levels = if redo { ra.clone() } else { rb.clone() };
            sh.col_levels = if redo { ca.clone() } else { cb.clone() };
            sh.layout_dirty = true;
            sh.bump_display_style_revision();
        }
        if !self.rich.is_empty() {
            let mut any_rich_applied = false;
            for (idx, (pos, before_cell, after_cell, before_runs, after_runs)) in
                self.rich.iter().enumerate()
            {
                let cur = sh.get(pos.0, pos.1).map(|c| c.into_owned()).unwrap_or_default();
                let expected_cell = if redo { before_cell } else { after_cell };
                if !guard(self.sheet, pos.0, pos.1, &cur) || !cur.same_input(expected_cell) {
                    rich_skipped.insert(idx);
                    continue;
                }
                let target_cell = if redo { after_cell } else { before_cell };
                let target_runs = if redo { after_runs } else { before_runs };
                sh.set(pos.0, pos.1, target_cell.clone());
                match target_runs {
                    Some(runs) if !runs.is_empty() => {
                        sh.rich.insert(*pos, runs.clone());
                    }
                    _ => {
                        sh.rich.remove(pos);
                    }
                }
                rich_touched.push((self.sheet, pos.0, pos.1));
                any_rich_applied = true;
            }
            if any_rich_applied {
                sh.rich_dirty = true;
                sh.bump_display_style_revision();
            }
        }
        if let Some((before, after)) = &self.dv {
            sh.dv = if redo { after.clone() } else { before.clone() };
            sh.dv_dirty = true;
            sh.bump_display_style_revision();
        }
        if !self.hyperlinks.is_empty() {
            for (pos, before, after) in &self.hyperlinks {
                let target = if redo { after } else { before };
                match target {
                    Some(link) => {
                        sh.hyperlinks.insert(*pos, link.clone());
                        let url = if !link.target.is_empty() {
                            link.target.clone()
                        } else if !link.location.is_empty() {
                            format!("#{}", link.location)
                        } else {
                            String::new()
                        };
                        syncs.push((
                            self.sheet,
                            SheetMetaKind::Hyperlink {
                                row: pos.0,
                                col: pos.1,
                                target: Some(url),
                                tooltip: link.tooltip.clone(),
                            },
                        ));
                    }
                    None => {
                        sh.hyperlinks.remove(pos);
                        syncs.push((
                            self.sheet,
                            SheetMetaKind::Hyperlink {
                                row: pos.0,
                                col: pos.1,
                                target: None,
                                tooltip: String::new(),
                            },
                        ));
                    }
                }
            }
            sh.hyperlinks_dirty = true;
            sh.bump_display_style_revision();
        }
        if let Some((before, after)) = &self.sparklines {
            sh.sparklines = if redo { after.clone() } else { before.clone() };
            sh.sparklines_dirty = true;
            sh.bump_display_style_revision();
        }
        (syncs, rich_touched, rich_skipped, conflicted.len())
    }
}

/// 数组公式登记（Ctrl+Shift+Enter）的前后快照：用于单步撤销/重做 CSE 数组范围。
#[derive(Clone, Debug)]
pub struct ArrayEdit {
    pub sheet: usize,
    pub before: Vec<(CellRef, (u32, u32, u32, u32))>,
    pub after: Vec<(CellRef, (u32, u32, u32, u32))>,
}

impl ArrayEdit {
    fn apply(&self, sheets: &mut [Sheet], redo: bool, touched: &mut Vec<(usize, u32, u32)>) {
        let Some(sheet) = sheets.get_mut(self.sheet) else {
            return;
        };
        let target = if redo { &self.after } else { &self.before };
        for &(anchor, (r1, c1, r2, c2)) in self.before.iter().chain(&self.after) {
            touched.push((self.sheet, anchor.row, anchor.col));
            if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 <= 4_096 {
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        touched.push((self.sheet, r, c));
                    }
                }
            }
        }
        // 清理原有 CSE 范围的溢出尾格（撤销回到普通公式时避免残留旧数组值）
        let prev = if redo { &self.before } else { &self.after };
        for &(anchor, (r1, c1, r2, c2)) in prev {
            sheet.spill_shapes.remove(&anchor);
            for r in r1..=r2 {
                for c in c1..=c2 {
                    let k = CellRef::new(r, c);
                    if k != anchor && sheet.spilled.remove_if_present(&k) {
                        sheet.remove(r, c);
                    }
                }
            }
        }
        sheet.arrays = target.clone();
        sheet.bump_display_style_revision();
    }
}

/// 一等公民数据透视表撤销/重做记录（支持新建表、改字段、刷新、样式切换、删除，不清空历史撤销栈）
#[derive(Clone, Debug)]
pub struct PivotUndoEdit {
    pub target_sheet: usize,
    /// 若本次新建透视表顺带创建了新工作表，记录插入序号与新表克隆
    pub inserted_sheet: Option<(usize, Sheet)>,
    /// 目标表受影响单元格（含样式）的精确前后差异
    pub deltas: Vec<CellDelta>,
    /// 自动列宽变更
    pub col_widths: Vec<(u32, Option<f32>, Option<f32>)>,
    pub pivots_before: Vec<crate::pivot_engine::PivotTableModel>,
    pub pivots_after: Vec<crate::pivot_engine::PivotTableModel>,
    pub removed_before: Vec<String>,
    pub removed_after: Vec<String>,
}

impl PivotUndoEdit {
    pub(crate) fn apply(
        &self,
        wb: &mut Workbook,
        label: &str,
        focus: (usize, u32, u32),
        redo: bool,
    ) -> Applied {
        let mut touched = Vec::new();
        let mut style_changes = Vec::new();
        if let Some((ins_idx, ref new_sh)) = self.inserted_sheet {
            if redo {
                if ins_idx <= wb.sheets.len() {
                    wb.sheets.insert(ins_idx, new_sh.clone());
                    wb.bump_coord_epoch();
                }
            } else if ins_idx < wb.sheets.len() && wb.sheets.len() > 1 {
                wb.sheets.remove(ins_idx);
                wb.bump_coord_epoch();
            }
        } else if let Some(sh) = wb.sheets.get_mut(self.target_sheet) {
            if redo {
                for d in &self.deltas {
                    d.replay(sh, true, &mut style_changes);
                    touched.push((d.sheet, d.row, d.col));
                }
            } else {
                for d in self.deltas.iter().rev() {
                    d.replay(sh, false, &mut style_changes);
                    touched.push((d.sheet, d.row, d.col));
                }
            }
            if !self.col_widths.is_empty() {
                let def = sh.cols.default_size();
                for &(c, before, after) in &self.col_widths {
                    let target = if redo { after } else { before };
                    sh.cols.set(c, target.unwrap_or(def));
                }
                sh.layout_dirty = true;
            }
            sh.styles_dirty = true;
            sh.bump_display_style_revision();
        }
        wb.pivots = if redo {
            self.pivots_after.clone()
        } else {
            self.pivots_before.clone()
        };
        wb.pivots_removed_parts = if redo {
            self.removed_after.clone()
        } else {
            self.removed_before.clone()
        };
        wb.dirty = true;
        let total_touched = touched.len();
        let focus = if redo {
            if let Some((ins_idx, _)) = self.inserted_sheet {
                let anchor = wb
                    .pivots
                    .iter()
                    .rev()
                    .find(|p| p.target_sheet == ins_idx)
                    .map(|p| p.anchor)
                    .unwrap_or((0, 0));
                (ins_idx, anchor.0, anchor.1)
            } else {
                focus
            }
        } else {
            focus
        };
        Applied {
            touched,
            style_changes,
            label: label.to_owned(),
            focus,
            skipped: 0,
            structural: self.inserted_sheet.is_some(),
            coord_changed: self.inserted_sheet.is_some(),
            refresh_sizes: None,
            merge_changes: Vec::new(),
            merge_conflict: false,
            total_touched,
        }
    }
}

pub const COW_BATCH_THRESHOLD: usize = 16_384;

/// 一等公民图表撤销/重做记录（覆盖插入、编辑、拖拽/缩放、行列互换、克隆与删除，绝不清空历史撤销栈）
#[derive(Clone, Debug)]
pub struct ChartUndoEdit {
    pub sheet: usize,
    pub charts_before: crate::model::SharedList<crate::charts::Chart>,
    pub charts_after: crate::model::SharedList<crate::charts::Chart>,
    pub deleted_before: Vec<(String, String)>,
    pub deleted_after: Vec<(String, String)>,
}

impl ChartUndoEdit {
    pub(crate) fn apply(
        &self,
        wb: &mut Workbook,
        label: &str,
        focus: (usize, u32, u32),
        redo: bool,
    ) -> Applied {
        if let Some(sh) = wb.sheets.get_mut(self.sheet) {
            sh.charts = if redo {
                self.charts_after.clone()
            } else {
                self.charts_before.clone()
            };
            sh.deleted_charts = if redo {
                self.deleted_after.clone()
            } else {
                self.deleted_before.clone()
            };
            sh.charts_dirty = true;
        }
        wb.dirty = true;
        Applied {
            touched: Vec::new(),
            style_changes: Vec::new(),
            label: label.to_owned(),
            focus,
            skipped: 0,
            structural: false,
            coord_changed: false,
            refresh_sizes: None,
            merge_changes: Vec::new(),
            merge_conflict: false,
            total_touched: 0,
        }
    }
}

enum Entry {
    Cells(EntryStamp, Action, Option<crate::data_tools::LinePermutation>, Option<ArrayEdit>, Option<NoteUndoEdit>),
    Tx(EntryStamp, TxAction),
    CowBatch(EntryStamp, Box<CowBatchAction>),
    NumericSort(EntryStamp, NumericSortAction),
}

impl Entry {
    fn id(&self) -> u64 { self.stamp().id }

    fn stamp(&self) -> &EntryStamp {
        match self {
            Self::Cells(s, _, _, _, _)
            | Self::Tx(s, _)
            | Self::CowBatch(s, _)
            | Self::NumericSort(s, _) => s,
        }
    }

    fn stamp_mut(&mut self) -> &mut EntryStamp {
        match self {
            Self::Cells(s, _, _, _, _)
            | Self::Tx(s, _)
            | Self::CowBatch(s, _)
            | Self::NumericSort(s, _) => s,
        }
    }

    /// Moving workbook references and their cells is one edit. Check the entire
    /// input before undoing the first cell or removing the history entry.
    fn can_replay_move(&self, wb: &Workbook, redo: bool, guard: &mut impl FnMut(usize,u32,u32,&Cell) -> bool) -> bool {
        let Some(refs) = &self.stamp().move_refs else { return true };
        let Self::Cells(_, action, ..) = self else { return false };
        refs.matches(wb, redo) && action.deltas.iter().all(|d|
            wb.sheets.get(d.sheet).is_some_and(|sh| d.can_replay(sh,redo,guard)))
    }

    fn simple_cells(&self) -> Option<Box<dyn Iterator<Item = (usize, u32, u32)> + '_>> {
        if self.stamp().link_source.is_some() { return None; }
        if self.stamp().move_refs.is_some() { return None; }
        match self {
            Self::Cells(_, a, _, None, None) if a.snap_before.is_none() && a.snap_after.is_none()
                && a.merges.is_none() && a.shapes.is_none() && a.pictures.is_none()
                && a.refresh_sizes.is_none() => Some(Box::new(a.deltas.iter().map(|d| (d.sheet, d.row, d.col)))),
            Self::Tx(_, TxAction { delta: crate::tx::TxDelta::CellBatch { deltas, sheet_ids, rich }, .. }) => {
                let fallback_sheet = deltas
                    .first()
                    .map(|d| d.sheet)
                    .or_else(|| sheet_ids.first().map(|&(idx, _)| idx));
                Some(Box::new(
                    deltas
                        .iter()
                        .map(|d| (d.sheet, d.row, d.col))
                        .chain(
                            rich.iter()
                                .filter_map(move |rd| fallback_sheet.map(|s| (s, rd.row, rd.col))),
                        ),
                ))
            }
            Self::CowBatch(_, a)
                if a.row_style_deltas.is_empty()
                    && a.col_style_deltas.is_empty()
                    && a.merges.is_none()
                    && a.notes.is_none()
                    && a.refresh_sizes.is_none() =>
            {
                let sheet = a.sheet;
                Some(Box::new(
                    a.all_mask
                        .iter()
                        .filter(|k| !a.excluded.contains(k))
                        .map(move |k| (sheet, k.row, k.col)),
                ))
            }
            Self::NumericSort(_, a) if a.notes.is_none() => Some(Box::new(a.sort.deltas().map(|d| (d.sheet, d.row, d.col)))),
            _ => None,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Tx(_, a) => &a.label,
            Self::Cells(_, a, _, _, _) => &a.label,
            Self::CowBatch(_, a) => &a.label,
            Self::NumericSort(_, a) => &a.label,
        }
    }
    fn by_site(&self) -> u32 {
        match self {
            Self::Tx(_, a) => a.by_site,
            Self::Cells(_, a, _, _, _) => a.by_site,
            Self::CowBatch(_, a) => a.by_site,
            Self::NumericSort(_, a) => a.by_site,
        }
    }
}

/// Measure a shared wire patch for the history budget without materializing another JSON copy.
struct CountBytes(usize);
impl std::io::Write for CountBytes {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

/// Unified reversible transaction undo entry backing sparse shifts, compressed
/// structural history, collaborative room patches, and sheet-tab operations.
struct TxAction {
    label: String,
    focus: (usize, u32, u32),
    by_site: u32,
    delta: crate::tx::TxDelta,
}

impl TxAction {
    /// Clearing one end of a sparse shift while skipping the other can destroy data.
    /// Check all cells and rewritten formulas before replaying either end.
    fn apply_sparse_shift(
        &self,
        sheets: &mut [Sheet],
        redo: bool,
        mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> Option<Applied> {
        let crate::tx::TxDelta::SparseShift {
            deltas,
            sheet_ids,
            focus_sheet,
        } = &self.delta
        else {
            return None;
        };
        let mut applied = Applied {
            touched: Vec::new(),
            label: self.label.clone(),
            focus: self.focus,
            skipped: 0,
            structural: false,
            coord_changed: true,
            refresh_sizes: None,
            style_changes: Vec::new(),
            merge_changes: Vec::new(),
            merge_conflict: false,
            total_touched: 0,
        };
        if sheet_ids
            .iter()
            .any(|&(idx, id)| sheets.get(idx).is_none_or(|sheet| sheet.identity != id))
            || deltas
                .iter()
                .any(|d| !d.can_replay(&sheets[d.sheet], redo, &mut guard))
        {
            applied.skipped = deltas.len();
            return Some(applied);
        }
        for delta in deltas {
            delta.replay(&mut sheets[delta.sheet], redo, &mut applied.style_changes);
            applied.touched.push((delta.sheet, delta.row, delta.col));
        }
        applied.total_touched = applied.touched.len();
        if let Some(sh) = sheets.get_mut(*focus_sheet) {
            sh.coordinates_shifted();
        }
        Some(applied)
    }

    fn apply_cell_batch(
        &mut self,
        sheets: &mut [Sheet],
        redo: bool,
        guard: &mut impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> Option<(Applied, bool)> {
        let crate::tx::TxDelta::CellBatch {
            deltas,
            sheet_ids,
            rich,
        } = &mut self.delta
        else {
            return None;
        };
        let mut touched = Vec::new();
        let mut style_changes = Vec::new();
        let mut excluded = std::collections::HashSet::new();
        let mut applied_coords = std::collections::HashSet::new();
        let mut skipped_coords = std::collections::HashSet::new();
        let batch_sheet = deltas
            .first()
            .map(|d| d.sheet)
            .or_else(|| sheet_ids.first().map(|&(idx, _)| idx));
        let id_matches = |sheet_idx: usize, sh: &Sheet| -> bool {
            sheet_ids
                .iter()
                .find(|&&(idx, _)| idx == sheet_idx)
                .is_none_or(|&(_, id)| sh.identity == id)
        };
        let rich_by_coord: std::collections::HashMap<(u32, u32), usize> = rich
            .iter()
            .enumerate()
            .map(|(idx, rd)| ((rd.row, rd.col), idx))
            .collect();
        let rich_matches_expected = |sheet: &Sheet, r: u32, c: u32| -> bool {
            let Some(&ri) = rich_by_coord.get(&(r, c)) else {
                return true;
            };
            let rd = &rich[ri];
            let expected_runs = if redo { &rd.before_runs } else { &rd.after_runs };
            sheet.rich.get(&(r, c)) == expected_runs.as_ref()
        };

        if redo {
            for (i, d) in deltas.iter().enumerate() {
                let Some(sheet) = sheets.get_mut(d.sheet) else {
                    excluded.insert(i);
                    skipped_coords.insert((d.sheet, d.row, d.col));
                    continue;
                };
                if !id_matches(d.sheet, sheet)
                    || !rich_matches_expected(sheet, d.row, d.col)
                    || !d.can_replay(sheet, true, guard)
                {
                    excluded.insert(i);
                    skipped_coords.insert((d.sheet, d.row, d.col));
                    continue;
                }
                d.replay(sheet, true, &mut style_changes);
                applied_coords.insert((d.sheet, d.row, d.col));
                touched.push((d.sheet, d.row, d.col));
            }
        } else {
            for (i, d) in deltas.iter().enumerate().rev() {
                let Some(sheet) = sheets.get_mut(d.sheet) else {
                    excluded.insert(i);
                    skipped_coords.insert((d.sheet, d.row, d.col));
                    continue;
                };
                if !id_matches(d.sheet, sheet)
                    || !rich_matches_expected(sheet, d.row, d.col)
                    || !d.can_replay(sheet, false, guard)
                {
                    excluded.insert(i);
                    skipped_coords.insert((d.sheet, d.row, d.col));
                    continue;
                }
                d.replay(sheet, false, &mut style_changes);
                applied_coords.insert((d.sheet, d.row, d.col));
                touched.push((d.sheet, d.row, d.col));
            }
        }

        // Restore/clear `RichRun`s AFTER `delta.replay` (which calls `Sheet::set` and strips runs).
        let mut excluded_rich = std::collections::HashSet::new();
        let mut standalone_rich_skipped = 0usize;
        if !rich.is_empty() {
            if let Some(sheet_idx) = batch_sheet
                && let Some(sheet) = sheets.get_mut(sheet_idx)
            {
                let mut any_rich_changed = false;
                for (ri, rd) in rich.iter().enumerate() {
                    let (r, c) = (rd.row, rd.col);
                    if skipped_coords.contains(&(sheet_idx, r, c)) {
                        excluded_rich.insert(ri);
                        continue;
                    }
                    if !applied_coords.contains(&(sheet_idx, r, c)) {
                        let blank = Cell::default();
                        let cur_cell = sheet.get(r, c);
                        let cur_cell = cur_cell.as_deref().unwrap_or(&blank);
                        let expected_cell = if redo { &rd.before_cell } else { &rd.after_cell };
                        let expected_runs = if redo { &rd.before_runs } else { &rd.after_runs };
                        if !id_matches(sheet_idx, sheet)
                            || !cur_cell.same_input(expected_cell)
                            || sheet.rich.get(&(r, c)) != expected_runs.as_ref()
                            || !guard(sheet_idx, r, c, cur_cell)
                        {
                            excluded_rich.insert(ri);
                            standalone_rich_skipped += 1;
                            continue;
                        }
                        touched.push((sheet_idx, r, c));
                    }
                    let target_runs = if redo { &rd.after_runs } else { &rd.before_runs };
                    let expected_runs = if redo { &rd.before_runs } else { &rd.after_runs };
                    match target_runs {
                        Some(runs) if !runs.is_empty() => {
                            if sheet.rich.get(&(r, c)) != Some(runs) {
                                sheet.rich.insert((r, c), runs.clone());
                                any_rich_changed = true;
                            }
                        }
                        _ => {
                            if sheet.rich.remove(&(r, c)).is_some() || expected_runs.is_some() {
                                any_rich_changed = true;
                            }
                        }
                    }
                }
                if any_rich_changed {
                    sheet.rich_dirty = true;
                    sheet.bump_display_style_revision();
                }
            } else {
                for (ri, rd) in rich.iter().enumerate() {
                    excluded_rich.insert(ri);
                    if batch_sheet.is_none_or(|s| !skipped_coords.contains(&(s, rd.row, rd.col))) {
                        standalone_rich_skipped += 1;
                    }
                }
            }
        }

        let skipped = excluded.len() + standalone_rich_skipped;
        if !excluded.is_empty() {
            let mut i = 0usize;
            deltas.retain(|_| {
                let keep = !excluded.contains(&i);
                i += 1;
                keep
            });
        }
        if !excluded_rich.is_empty() {
            let mut i = 0usize;
            rich.retain(|_| {
                let keep = !excluded_rich.contains(&i);
                i += 1;
                keep
            });
        }
        let total_touched = touched.len();
        let applied = Applied {
            touched,
            style_changes,
            label: self.label.clone(),
            focus: self.focus,
            skipped,
            structural: false,
            coord_changed: false,
            refresh_sizes: None,
            merge_changes: Vec::new(),
            merge_conflict: false,
            total_touched,
        };
        Some((applied, !deltas.is_empty() || !rich.is_empty()))
    }

    fn apply_meta(
        &mut self,
        sheets: &mut [Sheet],
        redo: bool,
        guard: &mut impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> Option<(Applied, bool, Vec<(usize, crate::sync::SheetMetaKind)>, bool)> {
        let crate::tx::TxDelta::Meta(edit) = &mut self.delta else {
            return None;
        };
        let (syncs, touched, rich_skipped, thread_conflicts) =
            edit.apply_with_guard(sheets, redo, guard);
        let skipped = rich_skipped.len() + thread_conflicts;
        if skipped > 0 {
            let mut i = 0usize;
            edit.rich.retain(|_| {
                let keep = !rich_skipped.contains(&i);
                i += 1;
                keep
            });
        }
        let needs_recalc = edit.filter.is_some();
        let total_touched = touched.len();
        let applied = Applied {
            touched,
            label: self.label.clone(),
            focus: self.focus,
            skipped,
            structural: false,
            coord_changed: false,
            refresh_sizes: None,
            style_changes: Vec::new(),
            merge_changes: Vec::new(),
            merge_conflict: false,
            total_touched,
        };
        Some((applied, !edit.is_empty(), syncs, needs_recalc))
    }

    fn apply_workbook(&mut self, wb: &mut Workbook, redo: bool) -> Option<(Applied, bool)> {
        use crate::tx::{SheetTabChange, TxDelta};
        match &mut self.delta {
            TxDelta::Structure(history) => {
                let (before, after) = history.snapshots()?;
                let touched = if redo {
                    restore_structure(wb, after, before)?
                } else {
                    restore_structure(wb, before, after)?
                };
                wb.bump_coord_epoch();
                let total_touched = touched.len();
                Some((
                    Applied {
                        touched,
                        label: self.label.clone(),
                        focus: self.focus,
                        skipped: 0,
                        structural: true,
                        coord_changed: true,
                        refresh_sizes: None,
                        style_changes: Vec::new(),
                        merge_changes: Vec::new(),
                        merge_conflict: false,
                        total_touched,
                    },
                    false,
                ))
            }
            TxDelta::SheetTab {
                index,
                identity,
                previous_focus,
                change,
            } => {
                let mut focus = *previous_focus;
                match change {
                    SheetTabChange::Rename {
                        before,
                        after,
                        references,
                    } => {
                        let (expected, restored) = if redo { (before, after) } else { (after, before) };
                        let sheet = wb.sheets.get(*index)?;
                        if sheet.identity != *identity
                            || sheet.name != *expected
                            || !references.restore(wb, *index, restored, redo)
                        {
                            return None;
                        }
                    }
                    SheetTabChange::Append { removed } if redo => {
                        if wb.sheets.len() != *index
                            || wb.sheets.iter().any(|s| {
                                s.identity == *identity
                                    || removed
                                        .as_ref()
                                        .is_some_and(|r| r.name.eq_ignore_ascii_case(&s.name))
                            })
                        {
                            return None;
                        }
                        wb.sheets.push(*removed.take()?);
                        wb.bump_coord_epoch();
                        focus = (*index, 0, 0);
                    }
                    SheetTabChange::Append { removed } => {
                        if *index == 0
                            || wb.sheets.len() != *index + 1
                            || wb.sheets[*index].identity != *identity
                            || wb.names.iter().any(|(_, scope, _)| *scope == Some(*index))
                            || wb.pivots_pending.iter().any(|(sheet, ..)| *sheet == *index)
                            || wb.pivots_pending2.iter().any(|(sheet, ..)| *sheet == *index)
                        {
                            return None;
                        }
                        *removed = wb.sheets.pop().map(Box::new);
                        wb.calc_pending_sheets.remove(index);
                        wb.bump_coord_epoch();
                    }
                    SheetTabChange::Insert { removed } if redo => {
                        if *index > wb.sheets.len()
                            || !removed
                                .as_ref()
                                .is_some_and(|sh| crate::sheet_ops::valid_name(wb, None, &sh.name))
                        {
                            return None;
                        }
                        let sh = *removed.take()?;
                        if !crate::sheet_ops::insert_sheet(wb, *index, sh) {
                            return None;
                        }
                        focus = (*index, 0, 0);
                    }
                    SheetTabChange::Insert { removed } => {
                        if wb.sheets.len() <= 1
                            || *index >= wb.sheets.len()
                            || wb.sheets[*index].identity != *identity
                        {
                            return None;
                        }
                        let rec = crate::sheet_ops::delete_with_history(wb, *index)?;
                        *removed = Some(Box::new(rec.sheet));
                        wb.calc_pending_sheets.remove(index);
                    }
                    SheetTabChange::Delete {
                        sheet,
                        local_names,
                        references,
                        pivots,
                    } if redo => {
                        if wb.sheets.len() <= 1
                            || *index >= wb.sheets.len()
                            || wb.sheets[*index].identity != *identity
                        {
                            return None;
                        }
                        let rec = crate::sheet_ops::delete_with_history(wb, *index)?;
                        *sheet = Some(Box::new(rec.sheet));
                        *local_names = rec.local_names;
                        *references = rec.references;
                        *pivots = rec.pivots;
                        focus = ((*index).min(wb.sheets.len() - 1), 0, 0);
                    }
                    SheetTabChange::Delete {
                        sheet,
                        local_names,
                        references,
                        pivots,
                    } => {
                        if *index > wb.sheets.len()
                            || !sheet
                                .as_ref()
                                .is_some_and(|sh| crate::sheet_ops::valid_name(wb, None, &sh.name))
                        {
                            return None;
                        }
                        let sh = *sheet.take()?;
                        if !crate::sheet_ops::restore_deleted_sheet(
                            wb,
                            *index,
                            sh,
                            local_names,
                            references,
                            pivots,
                        ) {
                            return None;
                        }
                        focus = (*index, previous_focus.1, previous_focus.2);
                    }
                    SheetTabChange::Move { from, to } => {
                        let (src, dst) = if redo { (*from, *to) } else { (*to, *from) };
                        if src >= wb.sheets.len()
                            || dst >= wb.sheets.len()
                            || wb.sheets[src].identity != *identity
                        {
                            return None;
                        }
                        if !crate::sheet_ops::reorder_sheet(wb, src, dst) {
                            return None;
                        }
                        focus = (dst, previous_focus.1, previous_focus.2);
                    }
                }
                wb.dirty = true;
                Some((
                    Applied {
                        touched: Vec::new(),
                        label: self.label.clone(),
                        focus,
                        skipped: 0,
                        structural: true,
                        coord_changed: true,
                        refresh_sizes: None,
                        style_changes: Vec::new(),
                        merge_changes: Vec::new(),
                        merge_conflict: false,
                        total_touched: 0,
                    },
                    false,
                ))
            }
            TxDelta::Chart(edit) => Some((edit.apply(wb, &self.label, self.focus, redo), false)),
            TxDelta::Pivot(edit) => Some((edit.apply(wb, &self.label, self.focus, redo), false)),
            TxDelta::DefinedNames { before, after } => {
                let (expected, target) = if redo { (before, after) } else { (after, before) };
                if wb.names != *expected {
                    return None;
                }
                wb.names = target.clone();
                wb.names_dirty = true;
                wb.dirty = true;
                Some((
                    Applied {
                        touched: Vec::new(),
                        style_changes: Vec::new(),
                        label: self.label.clone(),
                        focus: self.focus,
                        skipped: 0,
                        structural: false,
                        coord_changed: false,
                        refresh_sizes: None,
                        merge_changes: Vec::new(),
                        merge_conflict: false,
                        total_touched: 0,
                    },
                    true,
                ))
            }
            TxDelta::SparseShift { .. }
            | TxDelta::RoomPatch { .. }
            | TxDelta::Meta(_)
            | TxDelta::CellBatch { .. } => None,
        }
    }
}

#[inline]
pub(crate) fn effective_snapshot_style(
    styles: &crate::store::CellStyles,
    row_styles: &crate::model::CellMetadata<u32, u16>,
    col_styles: &crate::model::CellMetadata<u32, u16>,
    r: u32,
    c: u32,
) -> u16 {
    if let Some(&id) = styles.get(&CellRef::new(r, c)) {
        return id;
    }
    if !row_styles.is_empty()
        && let Some(&id) = row_styles.get(&r)
    {
        return id;
    }
    if !col_styles.is_empty()
        && let Some(&id) = col_styles.get(&c)
    {
        return id;
    }
    0
}

/// Pre-diffed CoW batch undo record built on a worker or caller thread with accurate forked-page accounting.
pub struct PreparedCowBatch {
    action: Box<CowBatchAction>,
    bytes: usize,
}

impl PreparedCowBatch {
    pub fn from_sheets(
        label: impl Into<String>,
        sheet_idx: usize,
        before: &Sheet,
        after: &Sheet,
        focus: (usize, u32, u32),
        by_site: u32,
    ) -> Option<Self> {
        Self::from_sheets_with(label, sheet_idx, before, after, focus, by_site, None)
    }

    pub fn from_sheets_with(
        label: impl Into<String>,
        sheet_idx: usize,
        before: &Sheet,
        after: &Sheet,
        focus: (usize, u32, u32),
        by_site: u32,
        refresh_sizes: Option<(Vec<(String, u32, u32)>, Vec<(String, u32, u32)>)>,
    ) -> Option<Self> {
        let (action, bytes) = CowBatchAction::from_sheets_with(
            label.into(),
            sheet_idx,
            before,
            after,
            focus,
            by_site,
            refresh_sizes,
        )?;
        Some(Self {
            action: Box::new(action),
            bytes,
        })
    }

    pub fn changed_count(&self) -> usize {
        self.action.all_mask.len()
            + self.action.row_style_deltas.len()
            + self.action.col_style_deltas.len()
    }

    pub fn payload_bytes(&self) -> usize {
        self.bytes
    }
}

struct CowBatchAction {
    sheet: usize,
    from_snapshots: bool,
    before_cells: crate::store::Store,
    after_cells: crate::store::Store,
    before_styles: crate::store::CellStyles,
    after_styles: crate::store::CellStyles,
    before_row_styles: crate::model::CellMetadata<u32, u16>,
    after_row_styles: crate::model::CellMetadata<u32, u16>,
    before_col_styles: crate::model::CellMetadata<u32, u16>,
    after_col_styles: crate::model::CellMetadata<u32, u16>,
    row_style_deltas: Vec<(u32, u16, u16)>,
    col_style_deltas: Vec<(u32, u16, u16)>,
    cell_mask: crate::model::CellJournal,
    style_mask: crate::model::CellJournal,
    all_mask: crate::model::CellJournal,
    excluded: crate::model::CellJournal,
    merges: Option<MergeEdit>,
    cf: Option<(crate::cf::CfSheet, crate::cf::CfSheet)>,
    notes: Option<NoteUndoEdit>,
    refresh_sizes: Option<(Vec<(String, u32, u32)>, Vec<(String, u32, u32)>)>,
    label: String,
    focus: (usize, u32, u32),
    by_site: u32,
    permutation: Option<crate::data_tools::LinePermutation>,
}

impl CowBatchAction {
    fn from_action(action: &Action) -> Option<(Self, usize)> {
        Self::from_action_with_notes(action, None)
    }

    fn from_action_with_notes(action: &Action, notes: Option<NoteUndoEdit>) -> Option<(Self, usize)> {
        if action.deltas.len() <= COW_BATCH_THRESHOLD
            || action.snap_before.is_some()
            || action.snap_after.is_some()
            || action.merges.is_some()
            || action.shapes.is_some()
            || action.pictures.is_some()
            || action.refresh_sizes.is_some()
        {
            return None;
        }
        let sheet = action.deltas.first()?.sheet;
        if action.deltas.iter().any(|d| d.sheet != sheet) {
            return None;
        }
        if let Some(ref n) = notes {
            if n.notes.iter().any(|&(s, ..)| s != sheet) || n.threads.iter().any(|&(s, ..)| s != sheet) {
                return None;
            }
        }
        let mut before_cells = crate::store::Store::default();
        let mut after_cells = crate::store::Store::default();
        let mut before_styles = crate::store::CellStyles::default();
        let mut after_styles = crate::store::CellStyles::default();
        let mut cell_mask = crate::model::CellJournal::default();
        let mut style_mask = crate::model::CellJournal::default();
        let mut heap_text_bytes = 0usize;

        // Replay in forward order so duplicate coordinates in `deltas` keep first `before` and last `after`.
        let mut seen = crate::model::CellJournal::default();
        for d in &action.deltas {
            let key = CellRef::new(d.row, d.col);
            let first_visit = seen.insert(key);
            if !d.before.same_input(&d.after) || cell_mask.contains(&key) {
                if first_visit && !d.before.is_blank() {
                    heap_text_bytes += cell_heap_bytes(&d.before);
                    before_cells.set(d.row, d.col, d.before.clone());
                }
                if d.after.is_blank() {
                    after_cells.remove(d.row, d.col);
                } else {
                    heap_text_bytes += cell_heap_bytes(&d.after);
                    after_cells.set(d.row, d.col, d.after.clone());
                }
                cell_mask.insert(key);
            }
            if d.before_style != d.after_style || style_mask.contains(&key) {
                if first_visit {
                    before_styles.insert(key, d.before_style);
                }
                after_styles.insert(key, d.after_style);
                style_mask.insert(key);
            }
        }
        let mut all_mask = cell_mask.clone();
        all_mask.union_from(&style_mask);
        if all_mask.is_empty() {
            return None;
        }
        let notes_bytes = notes.as_ref().map_or(0, NoteUndoEdit::payload_bytes);
        let payload = before_cells.unique_payload_bytes()
            + after_cells.unique_payload_bytes()
            + before_styles.payload_bytes()
            + after_styles.payload_bytes()
            + cell_mask.payload_bytes()
            + style_mask.payload_bytes()
            + all_mask.payload_bytes()
            + heap_text_bytes
            + notes_bytes;
        Some((
            Self {
                sheet,
                from_snapshots: false,
                before_cells,
                after_cells,
                before_styles,
                after_styles,
                before_row_styles: Default::default(),
                after_row_styles: Default::default(),
                before_col_styles: Default::default(),
                after_col_styles: Default::default(),
                row_style_deltas: Vec::new(),
                col_style_deltas: Vec::new(),
                cell_mask,
                style_mask,
                all_mask,
                excluded: Default::default(),
                merges: None,
                cf: None,
                notes,
                refresh_sizes: None,
                label: action.label.clone(),
                focus: action.focus,
                by_site: action.by_site,
                permutation: None,
            },
            payload,
        ))
    }

    fn from_sheets_with(
        label: String,
        sheet_idx: usize,
        before: &Sheet,
        after: &Sheet,
        focus: (usize, u32, u32),
        by_site: u32,
        refresh_sizes: Option<(Vec<(String, u32, u32)>, Vec<(String, u32, u32)>)>,
    ) -> Option<(Self, usize)> {
        let mut cell_mask = crate::model::CellJournal::default();
        let (forked_cell_bytes, diff_heap) = before
            .cells()
            .diff_into_journal(after.cells(), &mut cell_mask);
        let mut style_mask = crate::model::CellJournal::default();
        before.styles.diff_into_journal(&after.styles, &mut style_mask);

        let mut row_style_deltas = Vec::new();
        if before.row_styles != after.row_styles {
            let mut rows: Vec<u32> = before
                .row_styles
                .keys()
                .chain(after.row_styles.keys())
                .copied()
                .collect();
            rows.sort_unstable();
            rows.dedup();
            for r in rows {
                let b_id = before.row_styles.get(&r).copied().unwrap_or(0);
                let a_id = after.row_styles.get(&r).copied().unwrap_or(0);
                if b_id != a_id {
                    row_style_deltas.push((r, b_id, a_id));
                }
            }
        }
        let mut col_style_deltas = Vec::new();
        if before.col_styles != after.col_styles {
            let mut cols: Vec<u32> = before
                .col_styles
                .keys()
                .chain(after.col_styles.keys())
                .copied()
                .collect();
            cols.sort_unstable();
            cols.dedup();
            for c in cols {
                let b_id = before.col_styles.get(&c).copied().unwrap_or(0);
                let a_id = after.col_styles.get(&c).copied().unwrap_or(0);
                if b_id != a_id {
                    col_style_deltas.push((c, b_id, a_id));
                }
            }
        }
        let merges = if before.merges.revision() != after.merges.revision() {
            MergeEdit::between(sheet_idx, &before.merges, &after.merges)
        } else {
            None
        };
        let cf = (before.cf != after.cf).then(|| (before.cf.clone(), after.cf.clone()));
        let notes = NoteUndoEdit::between_sheet(sheet_idx, before, after);
        let mut all_mask = cell_mask.clone();
        all_mask.union_from(&style_mask);
        if all_mask.is_empty()
            && row_style_deltas.is_empty()
            && col_style_deltas.is_empty()
            && merges.is_none()
            && cf.is_none()
            && notes.is_none()
            && refresh_sizes.is_none()
        {
            return None;
        }
        let (before_cells, after_cells) = if cell_mask.is_empty() {
            (Default::default(), Default::default())
        } else {
            (before.cells().clone(), after.cells().clone())
        };
        let (
            before_styles,
            after_styles,
            before_row_styles,
            after_row_styles,
            before_col_styles,
            after_col_styles,
            forked_style_bytes,
        ) = if style_mask.is_empty() {
            (
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                0,
            )
        } else {
            let bytes = before.styles.forked_payload_bytes(&after.styles);
            (
                before.styles.clone(),
                after.styles.clone(),
                before.row_styles.clone(),
                after.row_styles.clone(),
                before.col_styles.clone(),
                after.col_styles.clone(),
                bytes,
            )
        };
        let merges_bytes = merges.as_ref().map_or(0, |edit| {
            edit.sheets
                .iter()
                .map(|s| {
                    std::mem::size_of_val(s.added.as_slice())
                        + std::mem::size_of_val(s.removed.as_slice())
                })
                .sum()
        });
        let refresh_bytes = refresh_sizes.as_ref().map_or(0, |(b, a)| {
            (b.capacity() + a.capacity()) * std::mem::size_of::<(String, u32, u32)>()
                + b.iter().chain(a).map(|(s, _, _)| s.capacity()).sum::<usize>()
        });
        let notes_bytes = notes.as_ref().map_or(0, NoteUndoEdit::payload_bytes);
        let payload = forked_cell_bytes
            + diff_heap
            + forked_style_bytes
            + (row_style_deltas.capacity() + col_style_deltas.capacity())
                * std::mem::size_of::<(u32, u16, u16)>()
            + cell_mask.payload_bytes()
            + style_mask.payload_bytes()
            + all_mask.payload_bytes()
            + merges_bytes
            + notes_bytes
            + refresh_bytes;
        Some((
            Self {
                sheet: sheet_idx,
                from_snapshots: true,
                before_cells,
                after_cells,
                before_styles,
                after_styles,
                before_row_styles,
                after_row_styles,
                before_col_styles,
                after_col_styles,
                row_style_deltas,
                col_style_deltas,
                cell_mask,
                style_mask,
                all_mask,
                excluded: Default::default(),
                merges,
                cf,
                notes,
                refresh_sizes,
                label,
                focus,
                by_site,
                permutation: None,
            },
            payload,
        ))
    }

    fn apply(
        &mut self,
        site: u32,
        stamp: &mut EntryStamp,
        sheets: &mut [Sheet],
        redo: bool,
        mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> (Applied, bool, Vec<(usize, crate::sync::SheetMetaKind)>) {
        let prev_excl_bytes = self.excluded.payload_bytes();
        let mut applied = Applied {
            touched: Vec::new(),
            style_changes: Vec::new(),
            label: self.label.clone(),
            focus: self.focus,
            skipped: 0,
            structural: false,
            coord_changed: false,
            refresh_sizes: None,
            merge_changes: Vec::new(),
            merge_conflict: false,
            total_touched: 0,
        };
        let Some(sheet) = sheets.get(self.sheet) else {
            applied.skipped = self.all_mask.len().saturating_sub(self.excluded.len());
            return (applied, false, Vec::new());
        };
        let (
            exp_cells,
            tgt_cells,
            exp_styles,
            tgt_styles,
            exp_row_styles,
            tgt_row_styles,
            exp_col_styles,
            tgt_col_styles,
        ) = if redo {
            (
                &self.before_cells,
                &self.after_cells,
                &self.before_styles,
                &self.after_styles,
                &self.before_row_styles,
                &self.after_row_styles,
                &self.before_col_styles,
                &self.after_col_styles,
            )
        } else {
            (
                &self.after_cells,
                &self.before_cells,
                &self.after_styles,
                &self.before_styles,
                &self.after_row_styles,
                &self.before_row_styles,
                &self.after_col_styles,
                &self.before_col_styles,
            )
        };

        // True O(1) CoW snapshot swap in offline single-user mode (`site == 0 && self.by_site == 0`)
        // when neither cells nor styles have diverged since the snapshot was recorded.
        let offline_untouched = site == 0
            && self.by_site == 0
            && self.from_snapshots
            && self.excluded.is_empty()
            && self.row_style_deltas.is_empty()
            && self.col_style_deltas.is_empty()
            && (self.cell_mask.is_empty() || sheet.cells().version == exp_cells.version)
            && (self.style_mask.is_empty()
                || (sheet.styles.version == exp_styles.version
                    && sheet.row_styles == *exp_row_styles
                    && sheet.col_styles == *exp_col_styles));
        if offline_untouched {
            if let Some(merge_edit) = &self.merges {
                let Some(changes) = merge_edit.apply(sheets, redo) else {
                    applied.merge_conflict = true;
                    return (applied, true, Vec::new());
                };
                applied.merge_changes = changes;
            }
            let Some(sheet) = sheets.get_mut(self.sheet) else {
                return (applied, false, Vec::new());
            };
            if !self.cell_mask.is_empty() {
                *sheet.cells_mut() = tgt_cells.clone();
                let had_cleared = !sheet.cleared.is_empty();
                for (&(pi, col), &change_mask) in self.cell_mask.page_masks() {
                    let tgt_mask = tgt_cells
                        .page(pi)
                        .and_then(|p| p.cols.get(col))
                        .map_or([0; 4], |s| s.row_mask());
                    let vac = [
                        change_mask[0] & !tgt_mask[0],
                        change_mask[1] & !tgt_mask[1],
                        change_mask[2] & !tgt_mask[2],
                        change_mask[3] & !tgt_mask[3],
                    ];
                    if vac != [0; 4] {
                        sheet.cleared.or_page_mask(pi, col, vac);
                    }
                    if had_cleared {
                        let occ = [
                            change_mask[0] & tgt_mask[0],
                            change_mask[1] & tgt_mask[1],
                            change_mask[2] & tgt_mask[2],
                            change_mask[3] & tgt_mask[3],
                        ];
                        if occ != [0; 4] {
                            sheet.cleared.clear_page_mask(pi, col, occ);
                        }
                    }
                }
                if !sheet.loading {
                    sheet.edited = true;
                }
            }
            if !self.style_mask.is_empty() {
                sheet.styles = tgt_styles.clone();
                sheet.style_edits.union_from(&self.style_mask);
                sheet.bump_display_style_revision();
                if !sheet.loading {
                    sheet.edited = true;
                    sheet.styles_dirty = true;
                }
                let scap = self.style_mask.len().min(2_001);
                applied.style_changes = Vec::with_capacity(scap);
                for k in self.style_mask.iter().take(2_001) {
                    applied.style_changes.push((self.sheet, k.row, k.col));
                }
            }
            if let Some((before_cf, after_cf)) = &self.cf {
                sheet.cf = if redo { after_cf.clone() } else { before_cf.clone() };
                sheet.cf_dirty = true;
                sheet.bump_display_style_revision();
            }
            let meta_sync = if let Some(ref mut note_edit) = self.notes {
                note_edit.apply_history(sheets, redo, &mut applied.skipped)
            } else {
                Vec::new()
            };
            let total = self.all_mask.len();
            applied.total_touched = total;
            let cap = total.min(2_001);
            applied.touched = Vec::with_capacity(cap);
            for k in self.all_mask.iter().take(2_001) {
                applied.touched.push((self.sheet, k.row, k.col));
            }
            applied.refresh_sizes = self
                .refresh_sizes
                .clone()
                .map(|(b, a)| if redo { a } else { b });
            return (applied, true, meta_sync);
        }

        let blank = Cell::default();
        // 1) Validate per-cell changes before mutating inherited row/col styles or merges.
        let mut accepted: Vec<(CellRef, bool, bool)> =
            Vec::with_capacity(self.all_mask.len().saturating_sub(self.excluded.len()));
        let mut newly_excluded: Vec<CellRef> = Vec::new();
        for k in self.all_mask.iter() {
            if self.excluded.contains(&k) {
                continue;
            }
            let has_cell = self.cell_mask.contains(&k);
            let has_style = self.style_mask.contains(&k);
            let current = sheet.get(k.row, k.col);
            let current = current.as_deref().unwrap_or(&blank);
            let ok_cell = !has_cell || {
                let expected = exp_cells.get(k.row, k.col);
                let expected = expected.as_deref().unwrap_or(&blank);
                current.same_input(expected)
            };
            let ok_style = !has_style || {
                let expected_st =
                    effective_snapshot_style(exp_styles, exp_row_styles, exp_col_styles, k.row, k.col);
                sheet.style_of(k.row, k.col) == expected_st
            };
            if !ok_cell || !ok_style || !guard(self.sheet, k.row, k.col, current) {
                if self.merges.is_some() {
                    applied.merge_conflict = true;
                    return (applied, true, Vec::new());
                }
                newly_excluded.push(k);
                continue;
            }
            accepted.push((k, has_cell, has_style));
        }

        // If merges are attached, validate axis styles and apply merges atomically before any mutation.
        if let Some(merge_edit) = &self.merges {
            let axis_ok = self.row_style_deltas.iter().all(|&(r, b_id, a_id)| {
                let exp = if redo { b_id } else { a_id };
                sheet.row_styles.get(&r).copied().unwrap_or(0) == exp
            }) && self.col_style_deltas.iter().all(|&(c, b_id, a_id)| {
                let exp = if redo { b_id } else { a_id };
                sheet.col_styles.get(&c).copied().unwrap_or(0) == exp
            });
            if !axis_ok {
                applied.merge_conflict = true;
                return (applied, true, Vec::new());
            }
            let Some(changes) = merge_edit.apply(sheets, redo) else {
                applied.merge_conflict = true;
                return (applied, true, Vec::new());
            };
            applied.merge_changes = changes;
        }

        if !newly_excluded.is_empty() {
            applied.skipped += newly_excluded.len();
            self.excluded.extend(newly_excluded);
            self.permutation = None;
        }

        let Some(sheet) = sheets.get_mut(self.sheet) else {
            return (applied, false, Vec::new());
        };

        // 2) Replay inherited row/column styles first so explicit `set_style(r, c, 0)` overrides behave identically.
        let mut axis_replayed = false;
        self.row_style_deltas.retain(|&(r, b_id, a_id)| {
            let (exp, tgt) = if redo { (b_id, a_id) } else { (a_id, b_id) };
            if sheet.row_styles.get(&r).copied().unwrap_or(0) != exp {
                applied.skipped += 1;
                return false;
            }
            sheet.set_row_style(r, tgt);
            axis_replayed = true;
            for (k, _) in sheet.iter_rect((r, 0, r, u32::MAX)) {
                applied.touched.push((self.sheet, k.row, k.col));
                applied.style_changes.push((self.sheet, k.row, k.col));
            }
            true
        });
        self.col_style_deltas.retain(|&(c, b_id, a_id)| {
            let (exp, tgt) = if redo { (b_id, a_id) } else { (a_id, b_id) };
            if sheet.col_styles.get(&c).copied().unwrap_or(0) != exp {
                applied.skipped += 1;
                return false;
            }
            sheet.set_col_style(c, tgt);
            axis_replayed = true;
            let mut col_cells = Vec::new();
            for (base_row, slice) in sheet.cells().column_typed_slices(0, u32::MAX, c) {
                match slice {
                    crate::store::ColSlice::Numbers { present, start, values } => {
                        for i in 0..values.len() {
                            let ro = start + i;
                            if (present[ro / 64] & (1u64 << (ro % 64))) != 0 {
                                col_cells.push(base_row + i as u32);
                            }
                        }
                    }
                    crate::store::ColSlice::Values(vs) => {
                        for (i, v) in vs.iter().enumerate() {
                            if !v.is_empty() {
                                col_cells.push(base_row + i as u32);
                            }
                        }
                    }
                    crate::store::ColSlice::Mixed(cs) => {
                        for (i, cell) in cs.iter().enumerate() {
                            if !cell.is_blank() {
                                col_cells.push(base_row + i as u32);
                            }
                        }
                    }
                }
            }
            for r in col_cells {
                applied.touched.push((self.sheet, r, c));
                applied.style_changes.push((self.sheet, r, c));
            }
            true
        });

        // 3) Replay accepted cell & explicit style changes — use O(1) CoW snapshot swap when untouched!
        let can_fast_cells = !self.cell_mask.is_empty()
            && self.from_snapshots
            && self.excluded.is_empty()
            && applied.skipped == 0
            && sheet.cells().version == exp_cells.version;
        let can_fast_styles = !self.style_mask.is_empty()
            && self.from_snapshots
            && self.excluded.is_empty()
            && applied.skipped == 0
            && self.row_style_deltas.is_empty()
            && self.col_style_deltas.is_empty()
            && sheet.styles == *exp_styles;

        if can_fast_cells {
            *sheet.cells_mut() = tgt_cells.clone();
            let had_cleared = !sheet.cleared.is_empty();
            for (&(pi, col), &change_mask) in self.cell_mask.page_masks() {
                let tgt_mask = tgt_cells
                    .page(pi)
                    .and_then(|p| p.cols.get(col))
                    .map_or([0; 4], |s| s.row_mask());
                let vac = [
                    change_mask[0] & !tgt_mask[0],
                    change_mask[1] & !tgt_mask[1],
                    change_mask[2] & !tgt_mask[2],
                    change_mask[3] & !tgt_mask[3],
                ];
                if vac != [0; 4] {
                    sheet.cleared.or_page_mask(pi, col, vac);
                }
                if had_cleared {
                    let occ = [
                        change_mask[0] & tgt_mask[0],
                        change_mask[1] & tgt_mask[1],
                        change_mask[2] & tgt_mask[2],
                        change_mask[3] & tgt_mask[3],
                    ];
                    if occ != [0; 4] {
                        sheet.cleared.clear_page_mask(pi, col, occ);
                    }
                }
            }
            if !sheet.loading {
                sheet.edited = true;
            }
        }
        if can_fast_styles {
            sheet.styles = tgt_styles.clone();
            sheet.style_edits.union_from(&self.style_mask);
            sheet.bump_display_style_revision();
            if !sheet.loading {
                sheet.edited = true;
                sheet.styles_dirty = true;
            }
        }

        for (k, has_cell, has_style) in accepted {
            if has_cell && !can_fast_cells {
                if let Some(tgt) = tgt_cells.get(k.row, k.col) {
                    sheet.set(k.row, k.col, tgt.into_owned());
                } else {
                    sheet.remove(k.row, k.col);
                }
            }
            if has_style {
                if !can_fast_styles {
                    let tgt_st = effective_snapshot_style(
                        tgt_styles,
                        tgt_row_styles,
                        tgt_col_styles,
                        k.row,
                        k.col,
                    );
                    sheet.set_style(k.row, k.col, tgt_st);
                }
                applied.style_changes.push((self.sheet, k.row, k.col));
            }
            applied.touched.push((self.sheet, k.row, k.col));
        }
        applied.total_touched = applied.touched.len();

        if axis_replayed {
            applied.touched.sort_unstable();
            applied.touched.dedup();
            applied.total_touched = applied.touched.len();
            applied.style_changes.sort_unstable();
            applied.style_changes.dedup();
        }
        if let Some((before_cf, after_cf)) = &self.cf {
            sheet.cf = if redo { after_cf.clone() } else { before_cf.clone() };
            sheet.cf_dirty = true;
            sheet.bump_display_style_revision();
        }
        let meta_sync = if let Some(ref mut note_edit) = self.notes {
            if applied.skipped == 0 || !applied.touched.is_empty() || self.all_mask.is_empty() {
                note_edit.apply_history(sheets, redo, &mut applied.skipped)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        applied.refresh_sizes = self
            .refresh_sizes
            .clone()
            .map(|(b, a)| if redo { a } else { b });
        stamp.payload_bytes = stamp
            .payload_bytes
            .saturating_add(self.excluded.payload_bytes().saturating_sub(prev_excl_bytes));
        let keep = self.excluded.len() < self.all_mask.len()
            || !self.row_style_deltas.is_empty()
            || !self.col_style_deltas.is_empty()
            || self.merges.is_some()
            || self.cf.is_some()
            || self.notes.is_some()
            || self.refresh_sizes.is_some();
        (applied, keep, meta_sync)
    }
}

struct NumericSortAction {
    excluded: std::collections::HashSet<(u32,u32)>,
    sort: crate::data_tools::CompactSort,
    notes: Option<NoteUndoEdit>,
    label: String,
    focus: (usize, u32, u32),
    by_site: u32,
}

impl NumericSortAction {
    fn apply(&mut self, stamp: &mut EntryStamp, sheets: &mut [Sheet], redo: bool, mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool) -> (Applied, Vec<(usize, crate::sync::SheetMetaKind)>) {
        let previous_capacity = self.excluded.capacity();
        let mut applied = Applied {
            touched: Vec::new(), style_changes: Vec::new(), label: self.label.clone(), focus: self.focus,
            skipped: 0, structural: false, coord_changed: false, refresh_sizes: None, merge_changes: Vec::new(), merge_conflict: false,
            total_touched: 0,
        };
        if let Some(sheet) = sheets.get_mut(self.sort.sheet()) {
            let mut apply = |d: CellDelta| {
                let key = (d.row,d.col);
                if self.excluded.contains(&key) { return; }
                if !d.can_replay(sheet, redo, &mut guard) {
                    self.excluded.insert(key);
                    applied.skipped += 1;
                    return;
                }
                d.replay(sheet, redo, &mut applied.style_changes);
                applied.touched.push((d.sheet, d.row, d.col));
            };
            if redo { self.sort.deltas().for_each(&mut apply); }
            else { self.sort.deltas().rev().for_each(apply); }
        }
        let meta_sync = if applied.skipped == 0 {
            if let Some(ref mut note_edit) = self.notes {
                note_edit.apply_history(sheets, redo, &mut applied.skipped)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        applied.total_touched = applied.touched.len();
        stamp.payload_bytes += (self.excluded.capacity() - previous_capacity) * (size_of::<(u32,u32)>() + 1);
        (applied, meta_sync)
    }
}

#[derive(Clone, Debug)]
struct ForeignReferences {
    sheet: usize,
    identity: u64,
    cells: Vec<(CellRef, Cell)>,
    cf: Option<crate::cf::CfSheet>,
    dv: Option<Vec<crate::dv::DataValidation>>,
    charts: Option<crate::model::SharedList<crate::charts::Chart>>,
    sparklines: Option<Vec<crate::sparkline::Sparkline>>,
}

type PendingPivot = (usize, crate::pivot::PivotSpec, crate::pivot::PivotResult,
    (u32, u32, u32, u32), String);
type PendingPivot2 = (usize, crate::pivot_multi::PivotSpec2, crate::pivot_multi::PivotGrid,
    (u32, u32, u32, u32));

/// A structural edit owns one complete sheet, plus only the foreign formulas
/// and reference metadata that the edit changes. Unrelated sheets' value cells
/// are never copied. File paths and source-part identities remain live state.
#[derive(Clone, Debug)]
pub struct StructureSnapshot {
    sheet: usize,
    target: Sheet,
    compact: Option<structural_cells::CellHistory>,
    shift: crate::refs::Shift,
    movement: Option<crate::refs::AxisMove>,
    sources: Vec<crate::external::Source>,
    reference_uses: structure_refs::Uses,
    foreign: Vec<ForeignReferences>,
    names: Vec<(String, Option<usize>, String)>,
    pivot_shifts: Vec<(String, u8, u32, u32, bool)>,
    pending_pivots: Vec<PendingPivot>,
    pending_pivots2: Vec<PendingPivot2>,
    pivots: Vec<crate::pivot_engine::PivotTableModel>,
    pivots_removed_parts: Vec<String>,
}

pub fn snapshot_structure(wb: &Workbook, sheet: usize, op: &crate::structural::Structural) -> StructureSnapshot {
    snapshot_with_mapping(wb, sheet, op.reference_shift(), None)
}

/// Capture the formulas and metadata affected by this partial row or column shift.
pub fn snapshot_cell_shift(wb: &Workbook, sheet: usize, op: &crate::structural::CellShift) -> StructureSnapshot {
    snapshot_with_mapping(wb, sheet, op.reference_shift(), None)
}

/// Ordinary cell/formula/style edits need only sparse cell deltas. Avoid cloning
/// sheet and workbook state when none of the metadata a partial shift can change exists.
pub fn snapshot_cell_shift_if_metadata(wb: &Workbook, sheet: usize, op: &crate::structural::CellShift) -> Option<StructureSnapshot> {
    crate::participant::CoordParticipant::any_cell_shift_metadata(wb, sheet)
        .then(|| snapshot_cell_shift(wb, sheet, op))
}

/// Shared cell history can also restore workbook bindings when a local ray moves them.
pub fn snapshot_cell_shift_if_bound(wb: &Workbook, sheet: usize, rect: (u32,u32,u32,u32), horizontal: bool, insert: bool) -> Option<StructureSnapshot> {
    let name = &wb.sheets.get(sheet)?.name;
    let overlaps_ray = |(r1, c1, r2, c2)| if horizontal {
        r1 <= rect.2 && rect.0 <= r2 && rect.1 <= c2
    } else {
        c1 <= rect.3 && rect.1 <= c2 && rect.0 <= r2
    };
    wb.sources.iter().any(|source| {
        !matches!(source.kind, crate::external::Kind::Unbound(_)) &&
        (source.target_sheet.as_ref().is_none_or(|s| s.eq_ignore_ascii_case(name)) && overlaps_ray(source.destination_rect())
            || matches!(&source.kind, crate::external::Kind::Internal { sheet, rect } if sheet.eq_ignore_ascii_case(name) && overlaps_ray(*rect)))
    }).then(|| {
        let direction = if horizontal { crate::cells::Shift::Horizontal } else { crate::cells::Shift::Vertical };
        snapshot_cell_shift(wb, sheet, &crate::structural::CellShift::new(rect, direction, insert))
    })
}

/// The drop line uses the original grid coordinates, just like move_rows_wb / move_cols_wb.
pub fn snapshot_axis_move(wb: &Workbook, sheet: usize, axis: crate::refs::Axis, from: u32, n: u32, drop: u32) -> Option<StructureSnapshot> {
    wb.sheets.get(sheet)?;
    let movement = crate::refs::AxisMove::from_drop(axis, from, n, drop)?;
    Some(snapshot_axis_permute(wb, sheet, movement))
}

pub(crate) fn snapshot_axis_permute(wb: &Workbook, sheet: usize, movement: crate::refs::AxisMove) -> StructureSnapshot {
    // Compression stores cell differences against the original coordinates. Foreign
    // references are captured using the actual forward/inverse permutation.
    snapshot_with_mapping(wb, sheet, crate::refs::Shift { axis: movement.axis, ..Default::default() }, Some(movement))
}

fn snapshot_with_mapping(wb: &Workbook, sheet: usize, shift: crate::refs::Shift, movement: Option<crate::refs::AxisMove>) -> StructureSnapshot {
    let target = wb.sheets[sheet].clone();
    let (reference_uses, foreign) = structure_refs::collect_with_foreign(wb, sheet, &shift, movement);
    StructureSnapshot {
        sheet,
        target,
        compact: None,
        shift,
        movement,
        sources: wb.sources.clone(),
        reference_uses,
        foreign,
        names: wb.names.clone(),
        pivot_shifts: wb.pivot_shifts.clone(),
        pending_pivots: wb.pivots_pending.clone(),
        pending_pivots2: wb.pivots_pending2.clone(),
        pivots: wb.pivots.clone(),
        pivots_removed_parts: wb.pivots_removed_parts.clone(),
    }
}

impl StructureSnapshot {
    /// The original positions include formulas whose new text contains only #REF!.
    pub fn foreign_formula_positions(&self) -> impl Iterator<Item = (usize, Vec<(u32, u32)>)> + '_ {
        self.foreign.iter().filter(|scope| !scope.cells.is_empty()).map(|scope| {
            (scope.sheet, scope.cells.iter().map(|(key, _)| (key.row, key.col)).collect())
        })
    }

    pub(crate) fn foreign_sheets(&self) -> Vec<usize> {
        self.foreign.iter().map(|scope| scope.sheet).collect()
    }

    /// Cell deltas already cover values, formulas and styles. A partial shift
    /// needs structural history only if it also changed other workbook state.
    pub fn cell_shift_changed_metadata(&self, wb: &Workbook) -> bool {
        let mut foreign_cf_changed = false;
        let mut foreign_dv_changed = false;
        let mut foreign_charts_changed = false;
        let mut foreign_sparklines_changed = false;
        for scope in &self.foreign {
            let sheet = &wb.sheets[scope.sheet];
            foreign_cf_changed |= scope.cf.as_ref().is_some_and(|cf| *cf != sheet.cf);
            foreign_dv_changed |= scope.dv.as_ref().is_some_and(|dv| *dv != sheet.dv);
            foreign_charts_changed |= scope
                .charts
                .as_ref()
                .is_some_and(|ch| !crate::participant::chart_refs_match(&sheet.charts, ch, false));
            foreign_sparklines_changed |= scope
                .sparklines
                .as_ref()
                .is_some_and(|sp| *sp != sheet.sparklines);
        }
        let view = crate::participant::CellShiftMetaDiffView {
            target_before: &self.target,
            target_after: &wb.sheets[self.sheet],
            names_before: &self.names,
            sources_before: &self.sources,
            wb_after: wb,
            foreign_cf_changed,
            foreign_dv_changed,
            foreign_charts_changed,
            foreign_sparklines_changed,
        };
        crate::participant::CoordParticipant::any_cell_shift_metadata_changed(&view)
    }

    pub fn detached_source(&self, wb: &Workbook) -> bool {
        self.sources.iter().zip(&wb.sources).any(|(old, new)| {
            !matches!(old.kind, crate::external::Kind::Unbound(_))
                && matches!(new.kind, crate::external::Kind::Unbound(_))
        })
    }

    fn storage_shift(&self) -> crate::refs::Shift {
        // The cell-store compressor models complete axes. A band shift is stored
        // as exceptions at the original coordinates, keeping cells outside the band intact.
        if self.shift.row_filter.is_some() || self.shift.col_filter.is_some() {
            // An identity mapping starts past the grid, so the compressor can skip
            // shared unchanged pages and restoration need not remap their cells.
            return crate::refs::Shift { at: crate::sync::MAX_ROWS, ..Default::default() };
        }
        self.shift.clone()
    }

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
        let movement = self.movement.map(crate::refs::AxisMove::inverse);
        Self {
            sheet: self.sheet,
            target: wb.sheets[self.sheet].clone(),
            compact: None,
            reference_uses: structure_refs::collect(wb, self.sheet, &shift, movement),
            movement,
            sources: wb.sources.clone(),
            shift,
            foreign: self.foreign.iter().map(|scope| {
                let sheet = &wb.sheets[scope.sheet];
                ForeignReferences {
                    sheet: scope.sheet,
                    identity: sheet.identity,
                    cells: scope.cells.iter().map(|(key, _)| (*key, sheet.get(key.row, key.col).map(|c| c.into_owned()).unwrap_or_default())).collect(),
                    cf: scope.cf.as_ref().map(|_| sheet.cf.clone()),
                    dv: scope.dv.as_ref().map(|_| sheet.dv.clone()),
                    charts: scope.charts.as_ref().map(|_| sheet.charts.clone()),
                    sparklines: scope.sparklines.as_ref().map(|_| sheet.sparklines.clone()),
                }
            }).collect(),
            names: wb.names.clone(),
            pivot_shifts: wb.pivot_shifts.clone(),
            pending_pivots: wb.pivots_pending.clone(),
            pending_pivots2: wb.pivots_pending2.clone(),
            pivots: wb.pivots.clone(),
            pivots_removed_parts: wb.pivots_removed_parts.clone(),
        }
    }
}

fn chart_refs_match(
    current: &[crate::charts::Chart],
    expected: &[crate::charts::Chart],
    check_anchor: bool,
) -> bool {
    crate::participant::chart_refs_match(current, expected, check_anchor)
}

/// A row/column operation only changes series references, `source_range`, and
/// (on the target sheet) the chart anchor. Retain the live chart's presentation,
/// cached values and save journal when changing them back.
fn restored_charts(
    current: &crate::model::SharedList<crate::charts::Chart>,
    desired: &crate::model::SharedList<crate::charts::Chart>,
    restore_anchor: bool,
) -> crate::model::SharedList<crate::charts::Chart> {
    if current.ptr_eq(desired) || chart_refs_match(current, desired, restore_anchor) {
        return current.clone();
    }
    let mut restored = current.clone();
    for (chart, desired) in restored.iter_mut().zip(desired.iter()) {
        if restore_anchor && chart.anchor != desired.anchor {
            chart.anchor = desired.anchor;
        }
        if chart.source_range != desired.source_range {
            chart.source_range.clone_from(&desired.source_range);
        }
        for (series, dest) in chart.series.iter_mut().zip(&desired.series) {
            for (from, to) in [
                (&mut series.name_ref, &dest.name_ref),
                (&mut series.cat_ref, &dest.cat_ref),
                (&mut series.val_ref, &dest.val_ref),
                (&mut series.size_ref, &dest.size_ref),
            ] {
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
        (Some(a), Some(b)) => a.same_input(b),
        (None, None) => true,
        _ => false,
    }
}

/// Recalculation can replace formula caches and create or remove spill tails.
/// A literal written into a previous tail, even the same value, is user input.
fn sheet_inputs_match(a: &Sheet, b: &Sheet) -> bool {
    if a.cell_version() == b.cell_version() && a.spilled == b.spilled { return true; }
    fn input<'a>(sheet: &'a Sheet, k: CellRef) -> Option<std::borrow::Cow<'a, Cell>> {
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
                }) && b.formula_of(k.row, k.col).is_none();
            let ia = input(a, k);
            let ib = input(b, k);
            legacy_cache || same_input(ia.as_deref(), ib.as_deref())
        })
}

fn sheet_styles_match(a: &Sheet, b: &Sheet) -> bool {
    if a.styles == b.styles {
        return true;
    }
    if a.row_styles != b.row_styles || a.col_styles != b.col_styles {
        return false;
    }
    let mut diff = crate::model::CellJournal::default();
    a.styles.diff_into_journal(&b.styles, &mut diff);
    diff.iter()
        .all(|k| a.style_of(k.row, k.col) == b.style_of(k.row, k.col))
}

fn target_matches(a: &Sheet, b: &Sheet) -> bool {
    a.identity == b.identity && a.name == b.name && a.arrays == b.arrays
        && a.tables == b.tables && a.pictures == b.pictures && a.merges == b.merges
        && a.cols == b.cols && a.rows == b.rows
        && a.col_styles == b.col_styles && a.row_styles == b.row_styles
        && sheet_styles_match(a, b)
        && a.cf == b.cf && a.dv == b.dv && a.freeze == b.freeze
        && a.filter == b.filter && a.hidden_rows == b.hidden_rows && a.hidden_rows_user == b.hidden_rows_user
        && a.hidden_cols == b.hidden_cols && a.scenarios == b.scenarios && a.scenario_settings == b.scenario_settings && a.ignored_errors == b.ignored_errors
        && a.outline_summary_below == b.outline_summary_below && a.outline_summary_right == b.outline_summary_right
        && a.outline_symbols == b.outline_symbols && a.row_levels == b.row_levels && a.col_levels == b.col_levels
        && a.page == b.page && chart_refs_match(&a.charts, &b.charts, true) && a.tab_color == b.tab_color
        && a.protection == b.protection && a.protected_ranges == b.protected_ranges
        && a.rich == b.rich && a.hyperlinks == b.hyperlinks
        && a.sparklines == b.sparklines && a.textboxes == b.textboxes
        && a.hidden == b.hidden && a.very_hidden == b.very_hidden
}

impl StructureSnapshot {
    fn matches_live(&self, wb: &Workbook, desired: &Self) -> bool {
        wb.sheets.get(self.sheet).is_some_and(|s| target_matches(s, &self.target) &&
            self.compact.as_ref().map_or_else(|| sheet_inputs_match(s,&self.target),
                |history| history.matches(s,&self.target)))
            && (self.pivot_shifts == desired.pivot_shifts || wb.pivot_shifts == self.pivot_shifts)
            && (self.pending_pivots == desired.pending_pivots || wb.pivots_pending == self.pending_pivots)
            && (self.pending_pivots2 == desired.pending_pivots2 || wb.pivots_pending2 == self.pending_pivots2)
            && (self.pivots == desired.pivots || wb.pivots == self.pivots)
            && (self.pivots_removed_parts == desired.pivots_removed_parts
                || wb.pivots_removed_parts == self.pivots_removed_parts)
            && self.foreign.iter().all(|expected| {
                let Some(sheet) = wb.sheets.get(expected.sheet) else { return false };
                sheet.identity == expected.identity
                    && expected.cells.iter().all(|(k, cell)| {
                        let cur = sheet.get(k.row, k.col);
                        same_input(cur.as_deref(), (!cell.is_blank()).then_some(cell))
                    })
                    && expected.cf.as_ref().is_none_or(|cf| &sheet.cf == cf)
                    && expected.dv.as_ref().is_none_or(|dv| &sheet.dv == dv)
                    && expected.charts.as_ref().is_none_or(|ch| chart_refs_match(&sheet.charts, ch, false))
                    && expected.sparklines.as_ref().is_none_or(|sp| &sheet.sparklines == sp)
            })
            && {
                // The before snapshot of a subtotal used the earliest possible
                // boundary; its after snapshot knows the actual first insertion.
                let mut shift = self.shift.clone();
                shift.at = shift.at.max(desired.shift.at);
                self.reference_uses.covers(wb, self.sheet, &shift, self.movement)
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
    let mut source_indexes = Vec::with_capacity(expected.sources.len());
    for source in &expected.sources {
        let mut matches = wb.sources.iter().enumerate().filter(|(_, s)| s.name == source.name);
        let (i, live) = matches.next()?;
        if live != source || matches.next().is_some() { return None; }
        source_indexes.push(i);
    }
    let names = changed_names(&wb.names, &expected.names, &snap.names)?;
    let current = wb.sheets.get(snap.sheet)?;
    let mut target = snap.target.clone();
    // Comments may arrive independently of undo history. Compare the thread and
    // its traditional note together, including coordinates vacated by this edit.
    let recorded = &expected.target;
    if current.notes != recorded.notes || current.threads != recorded.threads {
        let comment_keys: HashSet<_> = current.notes.keys().chain(recorded.notes.keys())
            .chain(current.threads.keys()).chain(recorded.threads.keys()).copied().collect();
        let tx = crate::transform::CoordTransform::from_shift_and_move(
            snap.sheet,
            &expected.shift,
            expected.movement,
        );
        for key in comment_keys {
            let live = (current.notes.get(&key), current.threads.get(&key));
            let before = (recorded.notes.get(&key), recorded.threads.get(&key));
            if live == before { continue; }
            let moves = tx.map_cell(key.row, key.col) != Some((key.row, key.col));
            if moves || before != (target.notes.get(&key), target.threads.get(&key)) {
                return None;
            }
            if let Some(note) = live.0 { target.notes.insert(key, note.clone()); }
            else { target.notes.remove(&key); }
            if let Some(thread) = live.1 { target.threads.insert(key, thread.clone()); }
            else { target.threads.remove(&key); }
        }
    }
    if let Some(history) = &snap.compact {
        target.replace_cell_store(history.restore(current,&expected.storage_shift())?);
    }
    let mut touched = Vec::new();
    // Saving may have assigned a part to a previously new sheet. Undo changes
    // its content, not its identity or the current save baseline.
    target.identity = current.identity;
    if target.coordinate_revision() != current.coordinate_revision()
        || target.coordinate_revision() != recorded.coordinate_revision()
    {
        target.coordinates_shifted();
    }
    target.audit_sheet_idx = current.audit_sheet_idx;
    target.source_part = current.source_part.clone();
    target.source_needs_dom_repair = current.source_needs_dom_repair;
    target.saved_cells.clone_from(&current.saved_cells);
    target.source_cells.clone_from(&current.source_cells);
    target.deleted_charts.clone_from(&current.deleted_charts);
    target.zoom_scale = current.zoom_scale;
    target.show_gridlines = current.show_gridlines;
    target.show_headings = current.show_headings;
    target.view_dirty = current.view_dirty;
    let mut cleared = std::mem::take(&mut target.cleared);
    cleared.union_from(&current.cleared);
    current
        .cells()
        .diff_both_sides(target.cells(), snap.sheet, &mut cleared, &mut touched);
    target.cleared = cleared;
    target.style_edits.union_from(&current.style_edits);
    let mut style_diff = crate::model::CellJournal::default();
    current.styles.diff_into_journal(&target.styles, &mut style_diff);
    target.style_edits.union_from(&style_diff);
    touched.extend(style_diff.iter().map(|k| (snap.sheet, k.row, k.col)));
    if current.row_styles != target.row_styles {
        let mut changed_rows: Vec<u32> = current
            .row_styles
            .keys()
            .chain(target.row_styles.keys())
            .copied()
            .filter(|r| current.row_styles.get(r) != target.row_styles.get(r))
            .collect();
        changed_rows.sort_unstable();
        changed_rows.dedup();
        for r in changed_rows {
            for (k, _) in target.iter_rect((r, 0, r, u32::MAX)) {
                if target.style_of(k.row, k.col) != current.style_of(k.row, k.col) {
                    touched.push((snap.sheet, k.row, k.col));
                }
            }
        }
    }
    if current.col_styles != target.col_styles {
        let mut changed_cols: Vec<u32> = current
            .col_styles
            .keys()
            .chain(target.col_styles.keys())
            .copied()
            .filter(|c| current.col_styles.get(c) != target.col_styles.get(c))
            .collect();
        changed_cols.sort_unstable();
        changed_cols.dedup();
        for c in changed_cols {
            for (base_row, slice) in target.cells().column_typed_slices(0, u32::MAX, c) {
                match slice {
                    crate::store::ColSlice::Numbers { present, start, values } => {
                        for i in 0..values.len() {
                            let ro = start + i;
                            if (present[ro / 64] & (1u64 << (ro % 64))) != 0 {
                                let r = base_row + i as u32;
                                if target.style_of(r, c) != current.style_of(r, c) {
                                    touched.push((snap.sheet, r, c));
                                }
                            }
                        }
                    }
                    crate::store::ColSlice::Values(vs) => {
                        for (i, v) in vs.iter().enumerate() {
                            if !v.is_empty() {
                                let r = base_row + i as u32;
                                if target.style_of(r, c) != current.style_of(r, c) {
                                    touched.push((snap.sheet, r, c));
                                }
                            }
                        }
                    }
                    crate::store::ColSlice::Mixed(cs) => {
                        for (i, cell) in cs.iter().enumerate() {
                            if !cell.is_blank() {
                                let r = base_row + i as u32;
                                if target.style_of(r, c) != current.style_of(r, c) {
                                    touched.push((snap.sheet, r, c));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    target.charts = restored_charts(&current.charts, &target.charts, true);
    target.loading = false;
    target.edited = true;
    // The restored metadata must be written against today's package baseline,
    // including when the intermediate structural change was already saved.
    target.layout_dirty |= current.layout_dirty || target.rows != current.rows || target.cols != current.cols
        || target.hidden_rows != current.hidden_rows || target.hidden_rows_user != current.hidden_rows_user
        || target.hidden_cols != current.hidden_cols || target.row_levels != current.row_levels
        || target.col_levels != current.col_levels;
    target.pictures_dirty |= current.pictures_dirty || target.pictures != current.pictures;
    target.textboxes_dirty |= current.textboxes_dirty || target.textboxes != current.textboxes;
    target.scenarios_dirty |= current.scenarios_dirty
        || target.scenarios != current.scenarios
        || target.scenario_settings != current.scenario_settings;
    target.ignored_errors_dirty |=
        current.ignored_errors_dirty || target.ignored_errors != current.ignored_errors;
    target.protected_ranges_dirty |=
        current.protected_ranges_dirty || target.protected_ranges != current.protected_ranges;
    target.outline_pr_dirty |= current.outline_pr_dirty
        || target.outline_summary_below != current.outline_summary_below
        || target.outline_summary_right != current.outline_summary_right
        || target.outline_symbols != current.outline_symbols;
    target.tab_color_dirty |= current.tab_color_dirty || target.tab_color != current.tab_color;
    target.protect_dirty |= current.protect_dirty || target.protection != current.protection;
    target.hidden_dirty |= current.hidden_dirty
        || target.hidden != current.hidden
        || target.very_hidden != current.very_hidden;
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
    target.threads_dirty |= current.threads_dirty || target.threads != current.threads;
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
            touched.push((foreign.sheet, key.row, key.col));
        }
        if let Some(cf) = &foreign.cf { sheet.cf = cf.clone(); sheet.cf_dirty = true; }
        if let Some(dv) = &foreign.dv { sheet.dv = dv.clone(); sheet.dv_dirty = true; }
        if let Some(charts) = &foreign.charts {
            sheet.charts = restored_charts(&sheet.charts, charts, false);
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
    for (i, desired) in source_indexes.into_iter().zip(&snap.sources) {
        wb.sources[i].clone_from(desired);
    }
    if expected.pivot_shifts != snap.pivot_shifts {
        wb.pivot_shifts.clone_from(&snap.pivot_shifts);
    }
    // A deleted source/field/output removes its refreshable definition. Restore
    // the full entry on undo; save keeps this session-owned list authoritative.
    if expected.pending_pivots != snap.pending_pivots {
        wb.pivots_pending.clone_from(&snap.pending_pivots);
    }
    if expected.pending_pivots2 != snap.pending_pivots2 {
        wb.pivots_pending2.clone_from(&snap.pending_pivots2);
    }
    if expected.pivots != snap.pivots {
        wb.pivots.clone_from(&snap.pivots);
    }
    if expected.pivots_removed_parts != snap.pivots_removed_parts {
        wb.pivots_removed_parts.clone_from(&snap.pivots_removed_parts);
    }
    wb.dirty = true;
    touched.sort_unstable();
    touched.dedup();
    Some(touched)
}

impl Default for UndoStack {
    fn default() -> Self {
        Self {
            done: Vec::new(),
            undone: Vec::new(),
            pending_compaction: false,
            limit: 200,
            site: 0,
            last_meta_sync: Vec::new(),
            last_needs_recalc: false,
        }
    }
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn take_meta_sync(&mut self) -> Vec<(usize, crate::sync::SheetMetaKind)> {
        std::mem::take(&mut self.last_meta_sync)
    }

    pub fn take_needs_recalc(&mut self) -> bool {
        std::mem::take(&mut self.last_needs_recalc)
    }

    /// 记一步。相同格子在一步里只记第一次（后写的 after 覆盖）
    /// 换站点（进/退协作房间）：历史属于上一个人，清掉
    pub fn set_site(&mut self, site: u32) {
        if site != self.site { self.clear(); }
        self.site = site;
    }

    pub fn push(&mut self, action: Action) {
        self.push_prepared(action.prepare());
    }

    /// Push a unified transaction commit into history as `Entry::Tx`.
    ///
    /// Returns `0` without mutating the stack when `commit.delta.is_empty()`
    /// (e.g. a partial cell shift on an untouched band); otherwise returns the
    /// newly assigned non-zero entry ID (`>= 1`).
    pub fn push_tx(&mut self, mut commit: crate::tx::TxCommit) -> u64 {
        if commit.delta.is_empty() {
            return 0;
        }
        if commit.by_site == 0 {
            commit.by_site = self.site;
        }
        let bytes = self.tx_delta_bytes(&commit.delta);
        if let crate::tx::TxDelta::Structure(history) = &commit.delta {
            self.pending_compaction |= history.is_pending();
        }
        let stamp = next_entry(bytes);
        let id = stamp.id;
        self.done.push(Entry::Tx(
            stamp,
            TxAction {
                label: commit.label,
                focus: commit.focus,
                by_site: commit.by_site,
                delta: commit.delta,
            },
        ));
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
        id
    }

    fn tx_delta_bytes(&self, delta: &crate::tx::TxDelta) -> usize {
        use crate::tx::{SheetTabChange, TxDelta};
        match delta {
            TxDelta::SparseShift { deltas, .. } => {
                deltas.capacity() * std::mem::size_of::<CellDelta>()
                    + deltas
                        .iter()
                        .map(|d| cell_heap_bytes(&d.before) + cell_heap_bytes(&d.after))
                        .sum::<usize>()
            }
            TxDelta::CellBatch { deltas, rich, .. } => {
                let rich_bytes: usize = rich
                    .iter()
                    .map(|rd| {
                        let runs_bytes = |opt: &Option<Vec<crate::richtext::RichRun>>| -> usize {
                            opt.as_ref().map_or(0, |runs| {
                                runs.capacity() * std::mem::size_of::<crate::richtext::RichRun>()
                                    + runs.iter().map(|r| r.text.len()).sum::<usize>()
                            })
                        };
                        std::mem::size_of::<crate::tx::CellRichDelta>()
                            + cell_heap_bytes(&rd.before_cell)
                            + cell_heap_bytes(&rd.after_cell)
                            + runs_bytes(&rd.before_runs)
                            + runs_bytes(&rd.after_runs)
                    })
                    .sum();
                deltas.capacity() * std::mem::size_of::<CellDelta>()
                    + deltas
                        .iter()
                        .map(|d| cell_heap_bytes(&d.before) + cell_heap_bytes(&d.after))
                        .sum::<usize>()
                    + rich_bytes
            }
            TxDelta::Structure(history) => history.initial_bytes,
            TxDelta::RoomPatch { patch, .. } => {
                let mut bytes = CountBytes(0);
                let _ = serde_json::to_writer(&mut bytes, &**patch);
                bytes.0
            }
            TxDelta::SheetTab { change, .. } => match change {
                SheetTabChange::Rename {
                    before,
                    after,
                    references,
                } => before.len() + after.len() + references.payload_bytes(),
                SheetTabChange::Delete {
                    sheet, references, ..
                } => {
                    sheet.as_ref().map_or(0, |s| s.allocated_bytes() + 1024)
                        + references.payload_bytes()
                }
                SheetTabChange::Append { .. }
                | SheetTabChange::Insert { .. }
                | SheetTabChange::Move { .. } => 0,
            },
            TxDelta::Meta(edit) => {
                128
                    + edit.col_widths.len() * 16
                    + edit.row_heights.len() * 16
                    + edit.hidden_rows_user.len() * 8
                    + edit.hidden_cols.len() * 8
                    + edit.notes.len() * 96
                    + edit.threads.len() * 128
                    + edit.ignored_errors.len() * 12
                    + edit.rich.len() * 64
                    + edit.hyperlinks.len() * 96
            }
            TxDelta::Chart(edit) => 512 + edit.charts_after.len() * 256,
            TxDelta::Pivot(edit) => {
                512 + edit.deltas.len() * std::mem::size_of::<CellDelta>()
                    + edit.col_widths.len() * 16
            }
            TxDelta::DefinedNames { before, after } => before
                .iter()
                .chain(after)
                .map(|(n, _, f)| {
                    std::mem::size_of::<(String, Option<usize>, String)>() + n.len() + f.len()
                })
                .sum(),
        }
    }

    fn push_tx_delta(
        &mut self,
        label: String,
        focus: (usize, u32, u32),
        delta: crate::tx::TxDelta,
    ) -> u64 {
        let structural = delta.is_structural();
        self.push_tx(crate::tx::TxCommit {
            label,
            focus,
            delta,
            effects: crate::tx::TxEffects {
                recalc: crate::tx::RecalcScope::Full,
                invalidate_deps: true,
                invalidate_render_caches: true,
                coord_changed: true,
                structural,
                detached_sources: false,
                revision_deltas: Vec::new(),
                touched_sheets: Vec::new(),
                new_focus: None,
            },
            by_site: self.site,
        })
    }

    /// One partial shift may span sheets, but it must replay as a single move.
    pub fn push_cell_shift(&mut self, label: &str, deltas: Vec<CellDelta>, focus: (usize, u32, u32), sheets: &[Sheet]) {
        if deltas.is_empty() { return; }
        let mut indices: Vec<_> = deltas.chunk_by(|a, b| a.sheet == b.sheet).map(|group| group[0].sheet).collect();
        indices.push(focus.0);
        indices.sort_unstable();
        indices.dedup();
        let sheet_ids = indices.into_iter().map(|idx| (idx, sheets[idx].identity)).collect();
        self.push_tx_delta(
            label.to_owned(),
            focus,
            crate::tx::TxDelta::SparseShift {
                deltas,
                sheet_ids,
                focus_sheet: focus.0,
            },
        );
    }

    pub fn push_with_arrays(&mut self, mut action: Action, arrays: Option<ArrayEdit>) {
        if action.is_empty() && arrays.is_none() {
            return;
        }
        if action.by_site == 0 {
            action.by_site = self.site;
        }
        let bytes = action.payload_bytes();
        self.done.push(Entry::Cells(next_entry(bytes), action, None, arrays, None));
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
    }

    pub fn push_with_notes(&mut self, action: Action, notes: Option<NoteUndoEdit>) {
        self.push_with_move_references(action, notes, MoveReferences::default());
    }

    pub fn push_with_move_references(&mut self, mut action: Action, notes: Option<NoteUndoEdit>, refs: MoveReferences) {
        let notes = notes.filter(|n| !n.is_empty());
        if action.is_empty() && notes.is_none() && refs.is_empty() {
            return;
        }
        if action.by_site == 0 {
            action.by_site = self.site;
        }
        if refs.is_empty() && let Some((cow, cow_bytes)) = CowBatchAction::from_action_with_notes(&action, notes.clone()) {
            self.done.push(Entry::CowBatch(next_entry(cow_bytes), Box::new(cow)));
        } else {
            let bytes = action.payload_bytes() + notes.as_ref().map_or(0, NoteUndoEdit::payload_bytes);
            self.done.push(Entry::Cells(next_entry(bytes), action, None, None, notes));
        }
        if !refs.is_empty() {
            let stamp = self.done.last_mut().unwrap().stamp_mut();
            stamp.payload_bytes += refs.bytes();
            stamp.move_refs = Some(Box::new(refs));
        }
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
    }

    pub fn attach_notes_to_last(&mut self, edit: NoteUndoEdit) {
        if edit.is_empty() {
            return;
        }
        let add_bytes = edit.payload_bytes();
        match self.done.last_mut() {
            Some(Entry::Cells(stamp, _, _, _, slot)) => {
                stamp.payload_bytes += add_bytes;
                match slot {
                    Some(existing) => existing.extend(edit),
                    None => *slot = Some(edit),
                }
            }
            Some(Entry::CowBatch(stamp, action)) => {
                stamp.payload_bytes += add_bytes;
                match &mut action.notes {
                    Some(existing) => existing.extend(edit),
                    None => action.notes = Some(edit),
                }
            }
            Some(Entry::NumericSort(stamp, action)) => {
                stamp.payload_bytes += add_bytes;
                match &mut action.notes {
                    Some(existing) => existing.extend(edit),
                    None => action.notes = Some(edit),
                }
            }
            _ => {}
        }
    }

    /// Register the binding in the same history entry as its imported cells.
    /// Identical formulas can still create a new binding with no cell deltas.
    pub fn record_link_registration(&mut self, prior: Option<u64>, focus: (usize, u32, u32), source: crate::external::Source) {
        if self.top_undo_id() == prior {
            let action = Action {
                label: "从另一张表取数".into(), deltas: Vec::new(), focus, by_site: self.site,
                snap_before: None, snap_after: None, shapes: None, pictures: None, merges: None, refresh_sizes: None,
            };
            self.done.push(Entry::Cells(next_entry(0), action, None, None, None));
        }
        let stamp = self.done.last_mut().unwrap().stamp_mut();
        stamp.payload_bytes += size_of::<crate::external::Source>() + source.to_name_text().len();
        stamp.link_source = Some(Box::new(source));
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
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
        if let Some((cow, cow_bytes)) = CowBatchAction::from_action(&action) {
            self.done.push(Entry::CowBatch(next_entry(cow_bytes), Box::new(cow)));
        } else {
            self.done.push(Entry::Cells(next_entry(bytes), action, None, None, None));
        }
        // 新动作让「重做」失效
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
    }

    /// Record a large single-sheet batch directly from `before` and `after` CoW `Sheet` snapshots
    /// without constructing a per-cell `Vec<CellDelta>`.
    pub fn push_cow_batch(
        &mut self,
        label: impl Into<String>,
        sheet_idx: usize,
        before: &Sheet,
        after: &Sheet,
        focus: (usize, u32, u32),
    ) {
        self.push_cow_batch_with(label, sheet_idx, before, after, focus, None);
    }

    pub fn push_cow_batch_with(
        &mut self,
        label: impl Into<String>,
        sheet_idx: usize,
        before: &Sheet,
        after: &Sheet,
        focus: (usize, u32, u32),
        refresh_sizes: Option<(Vec<(String, u32, u32)>, Vec<(String, u32, u32)>)>,
    ) {
        if let Some(batch) = PreparedCowBatch::from_sheets_with(
            label,
            sheet_idx,
            before,
            after,
            focus,
            self.site,
            refresh_sizes,
        ) {
            self.push_prepared_cow_batch(batch);
        }
    }

    /// Push a `PreparedCowBatch` whose CoW diff and forked-page accounting were built off-thread.
    pub fn push_prepared_cow_batch(&mut self, mut batch: PreparedCowBatch) {
        if batch.action.by_site == 0 {
            batch.action.by_site = self.site;
        }
        self.done
            .push(Entry::CowBatch(next_entry(batch.bytes), batch.action));
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
    }

    /// Keep the newest operation undoable even when one very large edit exceeds the budget.
    fn trim_history(&mut self, budget: usize) {
        self.undone.clear();
        self.settle_compaction(false);
        self.trim_done(budget);
    }

    /// The worker releases the raw snapshots as soon as compression finishes.
    /// Apply its exact retained size to the existing history position on the UI thread.
    pub fn poll_compaction(&mut self) -> bool {
        if self.settle_compaction(false) { self.trim_done(HISTORY_PAYLOAD_BUDGET); }
        self.pending_compaction
    }

    fn settle_compaction(&mut self, wait: bool) -> bool {
        if !self.pending_compaction { return false; }
        self.pending_compaction = false;
        let mut completed = false;
        for entry in &mut self.done {
            if let Entry::Tx(stamp, action) = entry
                && let crate::tx::TxDelta::Structure(history) = &mut action.delta
            {
                if let Some(bytes) = history.settle(wait) {
                    stamp.payload_bytes = bytes;
                    completed = true;
                }
                self.pending_compaction |= history.is_pending();
            }
        }
        completed
    }

    fn settle_before_replay(&mut self) {
        // Complete accounting before moving entries across the undo/redo boundary.
        if self.settle_compaction(true) { self.trim_done(HISTORY_PAYLOAD_BUDGET); }
    }

    fn trim_done(&mut self, budget: usize) {
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

    /// 当前撤销栈顶条目的全局唯一 ID（用于审计未提交结构/格式事件与 Ctrl+Z/Ctrl+Y 精确同步）
    pub fn top_undo_id(&self) -> Option<u64> {
        self.done.last().map(Entry::id)
    }

    /// 当前重做栈顶条目的全局唯一 ID
    pub fn top_redo_id(&self) -> Option<u64> {
        self.undone.last().map(Entry::id)
    }

    pub fn push_meta(
        &mut self,
        label: impl Into<String>,
        focus: (usize, u32, u32),
        edit: MetaEdit,
    ) {
        self.push_tx(crate::tx::TxCommit::from_meta(label, focus, edit));
    }

    pub fn push_pivot_edit(
        &mut self,
        label: impl Into<String>,
        focus: (usize, u32, u32),
        edit: PivotUndoEdit,
    ) {
        self.push_tx(crate::tx::TxCommit::from_pivot(label, focus, edit));
    }

    /// 把跨表联动改写的 `CellDelta` 合并到最近一条 `Entry::Cells` 撤销记录中，
    /// 让移动单元格及其跨表公式同步改写成为单步原子 Undo。
    pub fn extend_last_deltas(&mut self, extra: Vec<CellDelta>) {
        if extra.is_empty() {
            return;
        }
        if let Some(Entry::Cells(stamp, action, _, _, _)) = self.done.last_mut() {
            let add_bytes: usize = extra.len() * std::mem::size_of::<CellDelta>()
                + extra
                    .iter()
                    .map(|d| cell_heap_bytes(&d.before) + cell_heap_bytes(&d.after))
                    .sum::<usize>();
            stamp.payload_bytes += add_bytes;
            action.deltas.extend(extra);
        }
    }

    pub fn undo_labels(&self, limit: usize) -> Vec<String> {
        self.done
            .iter()
            .rev()
            .take(limit)
            .map(|e| e.label().to_string())
            .collect()
    }

    pub fn next_is_cell_shift(&self, redo: bool) -> bool {
        matches!(
            if redo { self.undone.last() } else { self.done.last() },
            Some(Entry::Tx(_, TxAction { delta: crate::tx::TxDelta::SparseShift { .. }, .. }))
        )
    }

    /// Room history advances only after the host commits a replay of this exact transaction.
    pub fn push_room_cell_shift(&mut self, transaction: u64, patch: std::sync::Arc<crate::cell_shift_patch::CellShiftPatch>, wb: &Workbook) {
        let tx_patch = std::sync::Arc::new(crate::tx::TxPatch::from_cell_shift((*patch).clone()));
        self.push_room_tx_patch(transaction, tx_patch, wb);
    }

    /// Push a generalized collaborative `TxPatch` into unified `Entry::Tx` room history.
    pub fn push_room_tx_patch(
        &mut self,
        transaction: u64,
        patch: std::sync::Arc<crate::tx::TxPatch>,
        wb: &Workbook,
    ) -> u64 {
        let sheet_ids = patch
            .referenced_sheets()
            .into_iter()
            .filter_map(|i| wb.sheets.get(i).map(|s| (i, s.identity)))
            .collect();
        let label = patch.label().to_owned();
        let focus = match &*patch {
            crate::tx::TxPatch::CellShift(p) => (p.target_sheet(), p.rect.0, p.rect.1),
            crate::tx::TxPatch::AxisPermute(p) => {
                if p.horizontal {
                    (p.sheet, 0, p.at)
                } else {
                    (p.sheet, p.at, 0)
                }
            }
            crate::tx::TxPatch::DefinedNames(p) => (p.target_sheet(), 0, 0),
        };
        self.push_tx_delta(
            label,
            focus,
            crate::tx::TxDelta::RoomPatch {
                transaction,
                patch,
                sheet_ids,
                ownership_conflict: false,
            },
        )
    }

    pub fn next_room_cell_shift(&self, redo: bool) -> Option<(u64, std::sync::Arc<crate::cell_shift_patch::CellShiftPatch>)> {
        let (tx, patch) = self.next_room_tx_patch(redo)?;
        let cs = patch.as_cell_shift()?;
        Some((tx, std::sync::Arc::new(cs.clone())))
    }

    pub fn peek_room_cell_shift(&self, redo: bool) -> Option<(u64, std::sync::Arc<crate::cell_shift_patch::CellShiftPatch>)> {
        self.next_room_cell_shift(redo)
    }

    pub fn next_room_tx_patch(&self, redo: bool) -> Option<(u64, std::sync::Arc<crate::tx::TxPatch>)> {
        let entry = if redo { self.undone.last() } else { self.done.last() };
        let Entry::Tx(_, TxAction { delta: crate::tx::TxDelta::RoomPatch { transaction, patch, .. }, .. }) = entry? else {
            return None;
        };
        Some((*transaction, patch.clone()))
    }

    pub fn peek_room_tx_patch(&self, redo: bool) -> Option<(u64, std::sync::Arc<crate::tx::TxPatch>)> {
        self.next_room_tx_patch(redo)
    }

    pub fn room_shift_history_matches(&self, wb: &Workbook, redo: bool, offline: bool) -> bool {
        let entry = if redo { self.undone.last() } else { self.done.last() };
        matches!(
            entry,
            Some(Entry::Tx(
                _,
                TxAction {
                    delta: crate::tx::TxDelta::RoomPatch {
                        sheet_ids,
                        ownership_conflict,
                        ..
                    },
                    ..
                }
            )) if (!offline || !*ownership_conflict)
                && sheet_ids
                    .iter()
                    .all(|&(i, id)| wb.sheets.get(i).is_some_and(|s| s.identity == id))
        )
    }

    /// The replica's authorship map is discarded on leaving a room. Keep the
    /// conflict decision with its local history, including same-value writes.
    pub fn preserve_room_shift_ownership(&mut self, peer: &crate::sync::Peer) {
        for entry in self.done.iter_mut().chain(&mut self.undone) {
            if let Entry::Tx(_, action) = entry
                && let crate::tx::TxDelta::RoomPatch {
                    patch,
                    ownership_conflict,
                    ..
                } = &mut action.delta
                && let Some(cs) = patch.as_cell_shift()
            {
                let site = action.by_site;
                *ownership_conflict = cs.cells.iter().any(|c| {
                    peer.cells
                        .get(&(c.sheet, c.row, c.col))
                        .is_some_and(|op| op.site != site)
                });
            }
        }
    }

    pub fn confirm_room_cell_shift(&mut self, transaction: u64, redo: bool) -> bool {
        self.confirm_room_tx_patch(transaction, redo)
    }

    pub fn confirm_room_tx_patch(&mut self, transaction: u64, redo: bool) -> bool {
        let (from, to) = if redo { (&mut self.undone, &mut self.done) } else { (&mut self.done, &mut self.undone) };
        if !matches!(
            from.last(),
            Some(Entry::Tx(_, TxAction { delta: crate::tx::TxDelta::RoomPatch { transaction: tx, .. }, .. })) if *tx == transaction
        ) {
            return false;
        }
        to.push(from.pop().expect("verified history entry"));
        true
    }

    pub fn redo_labels(&self, limit: usize) -> Vec<String> {
        self.undone
            .iter()
            .rev()
            .take(limit)
            .map(|e| e.label().to_string())
            .collect()
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

    pub fn track_sort(&mut self, first_ts: u64, replay: Option<bool>) {
        match replay {
            None => if let Some(entry) = self.done.last_mut() { entry.stamp_mut().sort_created = Some(first_ts); },
            Some(redo) => {
                let (from,to) = if redo { (&self.undone,&mut self.done) } else { (&self.done,&mut self.undone) };
                if let Some(entry) = to.last_mut() {
                    entry.stamp_mut().sort_replay = Some((first_ts,redo,from.last().map(Entry::id)));
                }
            }
        }
    }

    pub fn reject_sort(&mut self, first_ts: u64) {
        for stack in [&mut self.done,&mut self.undone] {
            if let Some(i) = stack.iter().position(|e| e.stamp().sort_created == Some(first_ts)) {
                stack.remove(i);
                return;
            }
        }
        for redo in [false,true] {
            let (from,to) = if redo { (&mut self.done,&mut self.undone) } else { (&mut self.undone,&mut self.done) };
            if let Some(index) = from.iter().position(|e| e.stamp().sort_replay.is_some_and(|(ts,r,_)| ts == first_ts && r == redo)) {
                let mut entry = from.remove(index);
                let (_,_,previous) = entry.stamp_mut().sort_replay.take().unwrap();
                let at = previous.and_then(|id| to.iter().position(|e| e.id()==id)).map_or(0, |i| i+1);
                to.insert(at,entry);
                return;
            }
        }
    }

    /// Called immediately after recording the nonempty cell action for a sort.
    pub fn tag_sorted_cells(&mut self, order: crate::data_tools::LinePermutation) {
        match self.done.last_mut() {
            Some(Entry::Cells(stamp, _, slot, _, _)) => {
                stamp.payload_bytes += order.2.capacity() * size_of::<u32>();
                *slot = Some(order);
                self.trim_history(HISTORY_PAYLOAD_BUDGET);
            }
            Some(Entry::CowBatch(stamp, action)) => {
                stamp.payload_bytes += order.2.capacity() * size_of::<u32>();
                action.permutation = Some(order);
                self.trim_history(HISTORY_PAYLOAD_BUDGET);
            }
            _ => {}
        }
    }

    /// Check a collaborative undo before applying any cell or metadata in the entry.
    pub fn next_thread_sync_error(&self, redo: bool, supports_changes: bool) -> Option<&'static str> {
        let check = |before: &Option<crate::notes::ThreadedComment>, after: &Option<crate::notes::ThreadedComment>| {
            if !supports_changes { Some("此操作会更改批注会话，请先升级协作宿主再重试") }
            else if !crate::sync::thread_snapshots_fit(before.iter().chain(after)) { Some("批注会话过长，本次更改超出协作同步上限") }
            else { None }
        };
        let entry = if redo { self.undone.last()? } else { self.done.last()? };
        let notes = match entry {
            Entry::Cells(_, _, _, _, notes) => notes,
            Entry::CowBatch(_, action) => &action.notes,
            Entry::NumericSort(_, action) => &action.notes,
            Entry::Tx(_, TxAction { delta: crate::tx::TxDelta::Meta(edit), .. }) => {
                return edit.threads.iter().find_map(|(_, before, after)| check(before, after));
            }
            _ => return None,
        };
        notes.as_ref()?.threads.iter().find_map(|(_, _, before, after)| check(before, after))
    }

    pub fn next_sort_permutation(&self, redo: bool) -> Option<crate::data_tools::LinePermutation> {
        let entry = if redo { self.undone.last()? } else { self.done.last()? };
        let (region, horizontal, mut order) = match entry {
            Entry::Cells(_, _, Some(p), _, _) => p.clone(),
            Entry::CowBatch(_, a) if a.excluded.is_empty() => a.permutation.clone()?,
            Entry::NumericSort(_, a) if a.excluded.is_empty() => a.sort.permutation(),
            _ => return None,
        };
        if !redo {
            let first = if horizontal {region.1} else {region.0};
            let mut inverse = vec![0; order.len()];
            for (dst, src) in order.into_iter().enumerate() { inverse[(src-first) as usize] = first+dst as u32; }
            order = inverse;
        }
        Some((region,horizontal,order))
    }

    pub fn next_undo_id(&self) -> Option<u64> {
        self.done.last().map(Entry::id)
    }

    pub fn push_chart_edit(
        &mut self,
        label: impl Into<String>,
        focus: (usize, u32, u32),
        edit: ChartUndoEdit,
    ) {
        self.push_tx(crate::tx::TxCommit::from_chart(label, focus, edit));
    }

    pub fn undo_is_structure(&self) -> bool {
        matches!(self.done.last(), Some(Entry::Tx(_, action)) if action.delta.is_structural())
    }

    pub fn redo_is_structure(&self) -> bool {
        matches!(self.undone.last(), Some(Entry::Tx(_, action)) if action.delta.is_structural())
    }

    pub fn record_appended_sheet(&mut self, wb: &Workbook, previous_focus: (usize, u32, u32)) {
        let index = wb.sheets.len() - 1;
        self.push_sheet_tab(index, wb.sheets[index].identity, previous_focus, "新建工作表",
            crate::tx::SheetTabChange::Append { removed: None });
    }

    pub fn record_inserted_sheet(&mut self, wb: &Workbook, index: usize, label: &str, previous_focus: (usize, u32, u32)) {
        if let Some(sh) = wb.sheets.get(index) {
            self.push_sheet_tab(index, sh.identity, previous_focus, label,
                crate::tx::SheetTabChange::Insert { removed: None });
        }
    }

    pub fn delete_sheet(&mut self, wb: &mut Workbook, index: usize, previous_focus: (usize, u32, u32)) -> bool {
        let Some(rec) = crate::sheet_ops::delete_with_history(wb, index) else {
            return false;
        };
        let identity = rec.sheet.identity;
        self.push_sheet_tab(
            index,
            identity,
            previous_focus,
            "删除工作表",
            crate::tx::SheetTabChange::Delete {
                sheet: Some(Box::new(rec.sheet)),
                local_names: rec.local_names,
                references: rec.references,
                pivots: rec.pivots,
            },
        );
        true
    }

    pub fn move_sheet(&mut self, wb: &mut Workbook, from: usize, to: usize, previous_focus: (usize, u32, u32)) -> bool {
        let Some(identity) = wb.sheets.get(from).map(|s| s.identity) else {
            return false;
        };
        if !crate::sheet_ops::reorder_sheet(wb, from, to) {
            return false;
        }
        self.push_sheet_tab(
            to,
            identity,
            previous_focus,
            "移动工作表",
            crate::tx::SheetTabChange::Move { from, to },
        );
        true
    }

    pub fn rename_sheet(&mut self, wb: &mut Workbook, index: usize, name: &str, focus: (usize, u32, u32)) -> bool {
        let Some(sheet) = wb.sheets.get(index) else { return false };
        let before = sheet.name.clone();
        let identity = sheet.identity;
        let Some(references) = crate::sheet_ops::rename_with_history(wb, index, name) else { return false };
        self.push_sheet_tab(index, identity, focus, "重命名工作表",
            crate::tx::SheetTabChange::Rename { before, after: wb.sheets[index].name.clone(), references });
        true
    }

    fn push_sheet_tab(
        &mut self,
        index: usize,
        identity: u64,
        previous_focus: (usize, u32, u32),
        label: &str,
        change: crate::tx::SheetTabChange,
    ) {
        self.push_tx_delta(
            label.into(),
            previous_focus,
            crate::tx::TxDelta::SheetTab {
                index,
                identity,
                previous_focus,
                change,
            },
        );
    }

    pub fn push_structure(&mut self, label: impl Into<String>, before: StructureSnapshot, after: StructureSnapshot, focus: (usize, u32, u32)) {
        let history = crate::tx::StructureHistory::record(before, after);
        self.push_tx_delta(label.into(), focus, crate::tx::TxDelta::Structure(history));
    }

    pub fn push_numeric_sort(&mut self, label: &str, sort: crate::data_tools::NumericSort, focus: (usize, u32, u32)) {
        self.push_compact_sort(label, crate::data_tools::CompactSort::Numeric(sort), focus);
    }

    pub fn push_compact_sort(&mut self, label: &str, sort: crate::data_tools::CompactSort, focus: (usize, u32, u32)) {
        self.push_compact_sort_with_notes(label, sort, None, focus);
    }

    pub fn push_compact_sort_with_notes(
        &mut self,
        label: &str,
        sort: crate::data_tools::CompactSort,
        notes: Option<NoteUndoEdit>,
        focus: (usize, u32, u32),
    ) {
        let notes = notes.filter(|n| !n.is_empty());
        let stamp = next_entry(sort.bytes() + notes.as_ref().map_or(0, NoteUndoEdit::payload_bytes));
        self.done.push(Entry::NumericSort(stamp, NumericSortAction {
            sort, notes, label: label.into(), focus, by_site: self.site, excluded: Default::default(),
        }));
        self.trim_history(HISTORY_PAYLOAD_BUDGET);
    }

    pub fn next_moves_references(&self, redo: bool) -> bool {
        (if redo { self.undone.last() } else { self.done.last() })
            .is_some_and(|entry| entry.stamp().move_refs.is_some())
    }

    pub fn undo_workbook(&mut self, wb: &mut Workbook, mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool) -> Option<Applied> {
        self.settle_before_replay();
        self.last_meta_sync.clear();
        self.last_needs_recalc = false;
        if matches!(
            self.done.last(),
            Some(Entry::Tx(_, TxAction { delta: crate::tx::TxDelta::DefinedNames { .. }, .. }))
        ) {
            if self.done.last().is_some_and(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site) {
                return None;
            }
            let Some(Entry::Tx(_, action)) = self.done.last_mut() else { unreachable!() };
            let (applied, needs_recalc) = action.apply_workbook(wb, false)?;
            self.last_needs_recalc |= needs_recalc;
            self.undone.push(self.done.pop()?);
            return Some(applied);
        }
        if !self.undo_is_structure() {
            if let Some(Entry::Tx(_, action)) = self.done.last()
                && let crate::tx::TxDelta::Meta(edit) = &action.delta
            {
                if action.by_site == 0 || self.site == 0 || action.by_site == self.site {
                    if let Some((before_names, _)) = &edit.names {
                        wb.names = before_names.clone();
                        wb.names_dirty = true;
                        self.last_needs_recalc = true;
                    }
                }
            }
            if !self.done.last()?.stamp().link_matches(wb, false) { return None; }
            let entry = self.done.last()?;
            if !entry.can_replay_move(wb, false, &mut guard) { return None; }
            let entry_id = entry.id();
            let result = self.undo(&mut wb.sheets, guard)?;
            if result.coord_changed && (result.skipped == 0 || !result.touched.is_empty()) {
                wb.bump_coord_epoch();
            }
            if let Some(entry) = self.undone.last_mut().filter(|e| e.id() == entry_id) {
                if result.skipped == 0 && !result.merge_conflict {
                    entry.stamp().replay_link(wb, false);
                } else {
                    entry.stamp_mut().link_source = None;
                }
                if !result.merge_conflict && let Some(refs) = &mut entry.stamp_mut().move_refs {
                    self.last_needs_recalc |= refs.replay(wb, false);
                }
            }
            return Some(result);
        }
        if self.done.last().is_some_and(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site) { return None; }
        if let Some(Entry::Tx(_, action)) = self.done.last_mut() {
            let (applied, needs_recalc) = action.apply_workbook(wb, false)?;
            self.last_needs_recalc |= needs_recalc;
            self.undone.push(self.done.pop()?);
            return Some(applied);
        }
        None
    }

    pub fn redo_workbook(&mut self, wb: &mut Workbook, mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool) -> Option<Applied> {
        self.settle_before_replay();
        self.last_meta_sync.clear();
        self.last_needs_recalc = false;
        if matches!(
            self.undone.last(),
            Some(Entry::Tx(_, TxAction { delta: crate::tx::TxDelta::DefinedNames { .. }, .. }))
        ) {
            if self.undone.last().is_some_and(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site) {
                return None;
            }
            let Some(Entry::Tx(_, action)) = self.undone.last_mut() else { unreachable!() };
            let (applied, needs_recalc) = action.apply_workbook(wb, true)?;
            self.last_needs_recalc |= needs_recalc;
            self.done.push(self.undone.pop()?);
            return Some(applied);
        }
        if !self.redo_is_structure() {
            if let Some(Entry::Tx(_, action)) = self.undone.last()
                && let crate::tx::TxDelta::Meta(edit) = &action.delta
            {
                if action.by_site == 0 || self.site == 0 || action.by_site == self.site {
                    if let Some((_, after_names)) = &edit.names {
                        wb.names = after_names.clone();
                        wb.names_dirty = true;
                        self.last_needs_recalc = true;
                    }
                }
            }
            if !self.undone.last()?.stamp().link_matches(wb, true) { return None; }
            let entry = self.undone.last()?;
            if !entry.can_replay_move(wb, true, &mut guard) { return None; }
            let entry_id = entry.id();
            let result = self.redo(&mut wb.sheets, guard)?;
            if result.coord_changed && (result.skipped == 0 || !result.touched.is_empty()) {
                wb.bump_coord_epoch();
            }
            if let Some(entry) = self.done.last_mut().filter(|e| e.id() == entry_id) {
                if result.skipped == 0 && !result.merge_conflict {
                    entry.stamp().replay_link(wb, true);
                } else {
                    entry.stamp_mut().link_source = None;
                }
                if !result.merge_conflict && let Some(refs) = &mut entry.stamp_mut().move_refs {
                    self.last_needs_recalc |= refs.replay(wb, true);
                }
            }
            return Some(result);
        }
        if self.undone.last().is_some_and(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site) { return None; }
        if let Some(Entry::Tx(_, action)) = self.undone.last_mut() {
            let (applied, needs_recalc) = action.apply_workbook(wb, true)?;
            self.last_needs_recalc |= needs_recalc;
            self.done.push(self.undone.pop()?);
            return Some(applied);
        }
        None
    }

    /// 撤销一步。`guard` 返回 false 的格子会被跳过（协作时不覆盖别人的新改动）。
    pub fn undo(
        &mut self,
        sheets: &mut [Sheet],
        mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> Option<Applied> {
        self.settle_before_replay();
        if self.undo_is_structure()
            || matches!(
                self.done.last(),
                Some(Entry::Tx(
                    _,
                    TxAction {
                        delta: crate::tx::TxDelta::RoomPatch { .. }
                            | crate::tx::TxDelta::DefinedNames { .. },
                        ..
                    }
                ))
            )
        {
            return None;
        }
        // 只撤自己的：栈顶不是我的动作就不动（也不丢，免得误删历史）
        if self
            .done
            .last()
            .map(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site)
            .unwrap_or(false)
        {
            return None;
        }
        let site = self.site;
        if let Some(Entry::Tx(_, action)) = self.done.last() {
            match &action.delta {
                crate::tx::TxDelta::SparseShift { .. } => {
                    let applied = action.apply_sparse_shift(sheets, false, guard)?;
                    if applied.skipped == 0 {
                        self.undone.push(self.done.pop()?);
                    }
                    return Some(applied);
                }
                crate::tx::TxDelta::CellBatch { .. } => {
                    let mut entry = self.done.pop()?;
                    let Entry::Tx(_, ref mut action) = entry else { unreachable!() };
                    let (applied, keep) = action.apply_cell_batch(sheets, false, &mut guard)?;
                    if keep {
                        self.undone.push(entry);
                    }
                    return Some(applied);
                }
                crate::tx::TxDelta::Meta(_) => {
                    let mut entry = self.done.pop()?;
                    let Entry::Tx(_, ref mut action) = entry else { unreachable!() };
                    let (applied, keep, syncs, needs_recalc) =
                        action.apply_meta(sheets, false, &mut guard)?;
                    self.last_needs_recalc |= needs_recalc;
                    self.last_meta_sync = syncs;
                    if keep {
                        self.undone.push(entry);
                    }
                    return Some(applied);
                }
                _ => return None,
            }
        }
        if let Some(Entry::NumericSort(stamp, action)) = self.done.last_mut() {
            let (applied, meta_sync) = action.apply(stamp, sheets, false, guard);
            self.last_meta_sync = meta_sync;
            let entry = self.done.pop()?;
            if !applied.touched.is_empty() || !self.last_meta_sync.is_empty() { self.undone.push(entry); }
            return Some(applied);
        }
        if let Some(Entry::CowBatch(stamp, action)) = self.done.last_mut() {
            let (applied, keep, meta_sync) = action.apply(site, stamp, sheets, false, guard);
            if applied.merge_conflict {
                return Some(applied);
            }
            self.last_meta_sync = meta_sync;
            let entry = self.done.pop()?;
            if keep { self.undone.push(entry); }
            return Some(applied);
        }
        let Entry::Cells(id, mut action, mut permutation, arrays, mut notes) = self.done.pop()? else { unreachable!() };
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
                coord_changed: false,
                refresh_sizes: None,
                style_changes, merge_changes,
                merge_conflict: false,
                total_touched: 0,
            };
            self.undone.push(Entry::Cells(id, action, permutation, arrays, notes));
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
                coord_changed: false,
                refresh_sizes: None,
                style_changes, merge_changes,
                merge_conflict: false,
                total_touched: 0,
            };
            self.undone.push(Entry::Cells(id, action, permutation, arrays, notes));
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
            self.done.push(Entry::Cells(id, action, permutation, arrays, notes));
            return Some(Applied { touched, label, focus, skipped: 0, structural: false, coord_changed: false, refresh_sizes: None, style_changes, merge_changes, merge_conflict: true, total_touched: 0 });
        }
        if let Some(snap) = action.snap_before.clone() {
            if let Some(sheet) = sheets.get_mut(snap.sheet) {
                restore_sheet(sheet, &snap);
            }
            touched.extend(snap.cells.iter().map(|(r, c, _, _)| (snap.sheet, *r, *c)));
            let total_touched = touched.len();
            let applied = Applied {
                touched,
                label: action.label.clone(),
                focus: action.focus,
                skipped,
                structural: true,
                coord_changed: true,
                refresh_sizes: action.refresh_sizes.clone().map(|(b, _)| b),
                style_changes, merge_changes,
                merge_conflict: false,
                total_touched,
            };
            self.undone.push(Entry::Cells(id, action, permutation, arrays, notes));
            return Some(applied);
        }
        let mut excluded = std::collections::HashSet::new();
        for (i, d) in action.deltas.iter().enumerate().rev() {
            let Some(sheet) = sheets.get_mut(d.sheet) else { excluded.insert(i); continue };
            if action.merges.is_none() && !d.can_replay(sheet, false, &mut guard) {
                excluded.insert(i);
                continue;
            }
            d.replay(sheet, false, &mut style_changes);
            touched.push((d.sheet,d.row,d.col));
        }
        if let Some(arr) = &arrays {
            arr.apply(sheets, false, &mut touched);
        }
        skipped = excluded.len();
        if let Some(ref mut note_edit) = notes {
            if skipped == 0 || !touched.is_empty() || action.deltas.is_empty() {
                self.last_meta_sync = note_edit.apply_history(sheets, false, &mut skipped);
            }
        }
        if skipped != 0 {
            let mut i = 0;
            action.deltas.retain(|_| { let keep = !excluded.contains(&i); i += 1; keep });
            permutation = None;
        }
        let total_touched = touched.len();
        let applied = Applied {
            touched,
            label: action.label.clone(),
            focus: action.focus,
            skipped,
            structural: false,
            coord_changed: false,
            refresh_sizes: action.refresh_sizes.clone().map(|(b, _)| b),
            style_changes, merge_changes,
            merge_conflict: false,
            total_touched,
        };
        if !action.is_empty() || arrays.is_some() || notes.is_some() || id.move_refs.is_some() || id.link_source.is_some() { self.undone.push(Entry::Cells(id, action, permutation, arrays, notes)); }
        Some(applied)
    }

    /// 重做一步
    pub fn redo(
        &mut self,
        sheets: &mut [Sheet],
        mut guard: impl FnMut(usize, u32, u32, &Cell) -> bool,
    ) -> Option<Applied> {
        self.settle_before_replay();
        if self.redo_is_structure()
            || matches!(
                self.undone.last(),
                Some(Entry::Tx(
                    _,
                    TxAction {
                        delta: crate::tx::TxDelta::RoomPatch { .. }
                            | crate::tx::TxDelta::DefinedNames { .. },
                        ..
                    }
                ))
            )
        {
            return None;
        }
        if self
            .undone
            .last()
            .map(|a| a.by_site() != 0 && self.site != 0 && a.by_site() != self.site)
            .unwrap_or(false)
        {
            return None;
        }
        let site = self.site;
        if let Some(Entry::Tx(_, action)) = self.undone.last() {
            match &action.delta {
                crate::tx::TxDelta::SparseShift { .. } => {
                    let applied = action.apply_sparse_shift(sheets, true, guard)?;
                    if applied.skipped == 0 {
                        self.done.push(self.undone.pop()?);
                    }
                    return Some(applied);
                }
                crate::tx::TxDelta::CellBatch { .. } => {
                    let mut entry = self.undone.pop()?;
                    let Entry::Tx(_, ref mut action) = entry else { unreachable!() };
                    let (applied, keep) = action.apply_cell_batch(sheets, true, &mut guard)?;
                    if keep {
                        self.done.push(entry);
                    }
                    return Some(applied);
                }
                crate::tx::TxDelta::Meta(_) => {
                    let mut entry = self.undone.pop()?;
                    let Entry::Tx(_, ref mut action) = entry else { unreachable!() };
                    let (applied, keep, syncs, needs_recalc) =
                        action.apply_meta(sheets, true, &mut guard)?;
                    self.last_needs_recalc |= needs_recalc;
                    self.last_meta_sync = syncs;
                    if keep {
                        self.done.push(entry);
                    }
                    return Some(applied);
                }
                _ => return None,
            }
        }
        if let Some(Entry::NumericSort(stamp, action)) = self.undone.last_mut() {
            let (applied, meta_sync) = action.apply(stamp, sheets, true, guard);
            self.last_meta_sync = meta_sync;
            let entry = self.undone.pop()?;
            if !applied.touched.is_empty() || !self.last_meta_sync.is_empty() { self.done.push(entry); }
            return Some(applied);
        }
        if let Some(Entry::CowBatch(stamp, action)) = self.undone.last_mut() {
            let (applied, keep, meta_sync) = action.apply(site, stamp, sheets, true, guard);
            if applied.merge_conflict {
                return Some(applied);
            }
            self.last_meta_sync = meta_sync;
            let entry = self.undone.pop()?;
            if keep { self.done.push(entry); }
            return Some(applied);
        }
        let Entry::Cells(id, mut action, mut permutation, arrays, mut notes) = self.undone.pop()? else { unreachable!() };
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
                coord_changed: false,
                refresh_sizes: action.refresh_sizes.clone().map(|(_, a)| a),
                style_changes, merge_changes,
                merge_conflict: false,
                total_touched: 0,
            };
            self.done.push(Entry::Cells(id, action, permutation, arrays, notes));
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
                coord_changed: false,
                refresh_sizes: action.refresh_sizes.clone().map(|(_, a)| a),
                style_changes, merge_changes,
                merge_conflict: false,
                total_touched: 0,
            };
            self.done.push(Entry::Cells(id, action, permutation, arrays, notes));
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
            self.undone.push(Entry::Cells(id, action, permutation, arrays, notes));
            return Some(Applied { touched, label, focus, skipped: 0, structural: false, coord_changed: false, refresh_sizes: None, style_changes, merge_changes, merge_conflict: true, total_touched: 0 });
        }
        if let Some(snap) = action.snap_after.clone() {
            if let Some(sheet) = sheets.get_mut(snap.sheet) {
                restore_sheet(sheet, &snap);
            }
            touched.extend(snap.cells.iter().map(|(r, c, _, _)| (snap.sheet, *r, *c)));
            let total_touched = touched.len();
            let applied = Applied {
                touched,
                label: action.label.clone(),
                focus: action.focus,
                skipped,
                structural: true,
                coord_changed: true,
                refresh_sizes: action.refresh_sizes.clone().map(|(_, a)| a),
                style_changes, merge_changes,
                merge_conflict: false,
                total_touched,
            };
            self.done.push(Entry::Cells(id, action, permutation, arrays, notes));
            return Some(applied);
        }
        let mut excluded = std::collections::HashSet::new();
        for (i, d) in action.deltas.iter().enumerate() {
            let Some(sheet) = sheets.get_mut(d.sheet) else { excluded.insert(i); continue };
            if action.merges.is_none() && !d.can_replay(sheet, true, &mut guard) {
                excluded.insert(i);
                continue;
            }
            d.replay(sheet, true, &mut style_changes);
            touched.push((d.sheet,d.row,d.col));
        }
        if let Some(arr) = &arrays {
            arr.apply(sheets, true, &mut touched);
        }
        skipped = excluded.len();
        if let Some(ref mut note_edit) = notes {
            if skipped == 0 || !touched.is_empty() || action.deltas.is_empty() {
                self.last_meta_sync = note_edit.apply_history(sheets, true, &mut skipped);
            }
        }
        if skipped != 0 {
            let mut i = 0;
            action.deltas.retain(|_| { let keep = !excluded.contains(&i); i += 1; keep });
            permutation = None;
        }
        let total_touched = touched.len();
        let applied = Applied {
            touched,
            label: action.label.clone(),
            focus: action.focus,
            skipped,
            structural: false,
            coord_changed: false,
            refresh_sizes: action.refresh_sizes.clone().map(|(_, after)| after),
            style_changes, merge_changes,
            merge_conflict: false,
            total_touched,
        };
        if !action.is_empty() || arrays.is_some() || notes.is_some() || id.move_refs.is_some() || id.link_source.is_some() { self.done.push(Entry::Cells(id, action, permutation, arrays, notes)); }
        Some(applied)
    }

    pub fn clear(&mut self) {
        self.pending_compaction = false;
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
            before: sheet.get(*r, *c).map(|c| c.into_owned()).unwrap_or(Cell {
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
        let before = wb.sheets[0].get(r, c).map(|c| c.into_owned()).unwrap();
        let bs = wb.sheets[0].style_of(r, c);
        wb.sheets[0].set(r, c, Cell { value: Value::Number(v), formula: None });
        let after = wb.sheets[0].get(r, c).map(|c| c.into_owned()).unwrap();
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
    fn axis_insertion_history_cost_is_independent_of_the_moved_cell_count() {
        let mut wb = wb();
        let mut stack = UndoStack::new();
        stack.push(edit(&mut wb,0,0,13.));
        for r in 1..12000 { wb.sheets[0].set(r,0,Cell {value:Value::Number(r as f64),formula:None}); }
        let op = crate::structural::Structural::InsertRows {at:1,n:1};
        let before = snapshot_structure(&wb,0,&op);
        crate::structural::apply_structural_wb(&mut wb,0,&op);
        let after = before.capture_after(&wb);
        stack.push_structure("insert",before,after,(0,0,0));
        assert!(stack.done.last().unwrap().stamp().payload_bytes < 4096);
        stack.trim_history(4096);
        assert_eq!(stack.done.len(),2);
        stack.undo_workbook(&mut wb,|_,_,_,_| true).unwrap();
        assert_eq!(wb.sheets[0].value(11999,0), Value::Number(11999.));
        stack.undo_workbook(&mut wb,|_,_,_,_| true).unwrap();
        assert_ne!(wb.sheets[0].value(0,0), Value::Number(13.));
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
    fn sheet_tab_history_replays_even_when_it_has_no_cell_deltas() {
        let mut wb = wb();
        let mut stack = UndoStack::new();
        wb.sheets.push(Sheet::new("Added"));
        stack.record_appended_sheet(&wb, (0,0,0));
        assert!(stack.rename_sheet(&mut wb,1,"Report",(1,0,0)));
        for expected in [2,1] {
            assert!(stack.undo_workbook(&mut wb,|_,_,_,_| true).is_some());
            assert_eq!(wb.sheets.len(),expected);
        }
        for expected in ["Added","Report"] {
            assert!(stack.redo_workbook(&mut wb,|_,_,_,_| true).is_some());
            assert_eq!(wb.sheet(1).name,expected);
        }
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

    #[test]
    fn cow_batch_handles_over_500k_paste_uniform_styles_merges_and_undo_redo() {
        let mut wb = wb();
        let sh = &mut wb.sheets[0];
        // Pre-populate an untouched page to prove forked-page accounting excludes untouched pages.
        for r in 0..1024u32 {
            sh.set(r, 20, Cell { value: crate::model::Value::Number(r as f64), formula: None });
        }
        let before = sh.clone();
        // Paste 520,000 cells (65,000 rows x 8 cols) + uniform style + merge.
        for r in 0..65_000u32 {
            for c in 0..8u32 {
                sh.set(
                    r,
                    c,
                    Cell {
                        value: crate::model::Value::Number((r * 8 + c) as f64),
                        formula: None,
                    },
                );
            }
        }
        sh.set_style_rect(0, 0, 64_999, 7, 5);
        sh.merge_range(0, 10, 1, 11);
        let batch = PreparedCowBatch::from_sheets("粘贴 52 万格", 0, &before, sh, (0, 0, 0), 0)
            .expect("non-empty batch");
        assert_eq!(batch.changed_count(), 520_000);
        assert!(batch.payload_bytes() < 64 * 1024 * 1024, "payload={}", batch.payload_bytes());

        let mut st = UndoStack::new();
        st.push_prepared_cow_batch(batch);
        assert!(st.can_undo());

        let base_len = before.len();
        let t_undo = std::time::Instant::now();
        let undone = st.undo(&mut wb.sheets, |_, _, _, _| true).unwrap();
        let undo_ms = t_undo.elapsed().as_secs_f64() * 1000.0;
        assert!(!undone.merge_conflict);
        assert_eq!(undone.touched_count(), 520_000);
        assert!(undone.touched.len() > 2_000);
        assert!(undo_ms < 25.0, "520k CoW undo must be O(1) fast swap, took {undo_ms:.2} ms");
        assert_eq!(wb.sheets[0].len(), base_len);
        assert!(wb.sheets[0].styles.is_empty());
        assert!(wb.sheets[0].merges.is_empty());
        assert_eq!(wb.sheets[0].get(500, 20).unwrap().value, crate::model::Value::Number(500.0));

        let t_redo = std::time::Instant::now();
        let redone = st.redo(&mut wb.sheets, |_, _, _, _| true).unwrap();
        let redo_ms = t_redo.elapsed().as_secs_f64() * 1000.0;
        assert!(!redone.merge_conflict);
        assert_eq!(redone.touched_count(), 520_000);
        assert!(redone.touched.len() > 2_000);
        assert!(redo_ms < 25.0, "520k CoW redo must be O(1) fast swap, took {redo_ms:.2} ms");
        assert_eq!(wb.sheets[0].len(), 520_000 + 1024);
        assert_eq!(wb.sheets[0].style_of(12_345, 3), 5);
        assert!(wb.sheets[0].styles.uniform_page_count() >= 2000);
        assert_eq!(wb.sheets[0].merges.as_slice(), &[(0, 10, 1, 11)]);
    }

    #[test]
    fn delete_row_intervals_wb_integrates_with_structural_undo_redo_and_compaction() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "数据".into();
        for r in 0..40u32 {
            wb.sheets[0].set(
                r,
                0,
                Cell {
                    value: crate::model::Value::Number((r + 1) as f64),
                    formula: None,
                },
            );
            wb.sheets[0].set(
                r,
                1,
                Cell {
                    value: crate::model::Value::Number(0.0),
                    formula: Some(format!("=A{}+SUM($A$2:$A$35)", r + 1).into()),
                },
            );
            if r % 2 == 0 {
                wb.sheets[0].set_style(r, 0, 3);
            }
        }
        wb.sheets[0].hidden_rows = vec![(4, 6), (12, 15)].into();
        let mut summary = Sheet::new("汇总");
        summary.set(
            0,
            0,
            Cell {
                value: crate::model::Value::Number(0.0),
                formula: Some("=SUM(数据!A2:A36)+数据!A18".into()),
            },
        );
        wb.sheets.push(summary);
        let original = wb.clone();

        let intervals = wb.sheets[0].visible_row_intervals(2, 18);
        assert_eq!(intervals, vec![(2, 3), (7, 11), (16, 18)]);
        let min_at = intervals[0].0;
        let total_deleted: u32 = intervals.iter().map(|&(a, b)| b - a + 1).sum();
        let op = crate::structural::Structural::DeleteRows {
            at: min_at,
            n: total_deleted,
        };
        let before = snapshot_structure(&wb, 0, &op);
        crate::structural::delete_row_intervals_wb(&mut wb, 0, &intervals);
        let after = before.capture_after(&wb);
        let expected_after = wb.clone();

        let mut stack = UndoStack::new();
        stack.push_structure("删除可见行", before, after, (0, min_at, 0));

        let undone = stack.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
        assert!(undone.structural);
        for r in 0..40u32 {
            assert_eq!(wb.sheets[0].get(r, 0), original.sheets[0].get(r, 0));
            assert_eq!(wb.sheets[0].get(r, 1), original.sheets[0].get(r, 1));
            assert_eq!(wb.sheets[0].style_of(r, 0), original.sheets[0].style_of(r, 0));
        }
        assert_eq!(wb.sheets[0].hidden_rows.intervals(), original.sheets[0].hidden_rows.intervals());
        assert_eq!(wb.sheets[1].get(0, 0), original.sheets[1].get(0, 0));

        let redone = stack.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
        assert!(redone.structural);
        for r in 0..40u32 {
            assert_eq!(wb.sheets[0].get(r, 0), expected_after.sheets[0].get(r, 0));
            assert_eq!(wb.sheets[0].get(r, 1), expected_after.sheets[0].get(r, 1));
            assert_eq!(wb.sheets[0].style_of(r, 0), expected_after.sheets[0].style_of(r, 0));
        }
        assert_eq!(wb.sheets[1].get(0, 0), expected_after.sheets[1].get(0, 0));
    }
}

pub(crate) fn undo_values_eq(a: &crate::model::Value, b: &crate::model::Value) -> bool {
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
        let cur = sheet.get(d.row, d.col).map(|c| c.into_owned()).unwrap_or(Cell {
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
    let rect = (r1, c1, r2, c2);
    let mut keys: Vec<(u32, u32)> = sheet.iter_rect(rect).map(|(k, _)| (k.row, k.col)).collect();
    keys.extend(sheet.styles.iter_rect(rect).map(|(k, _)| (k.row, k.col)));
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[cfg(test)]
mod array_edit_tests {
    use super::*;
    use crate::model::{Cell, Value, Workbook};

    #[test]
    fn array_edit_undo_and_redo_restore_cse_range_and_cells() {
        let mut wb = Workbook::new_blank();
        let mut undo = UndoStack::new();
        let before_arrays = wb.sheet(0).arrays.clone();
        let keys = vec![(0u32, 0u32), (1, 0)];
        let txn = capture(&wb.sheets[0], 0, &keys);
        {
            let sh = wb.sheet_mut(0);
            sh.arrays.push((CellRef::new(0, 0), (0, 0, 1, 0)));
            sh.set(0, 0, Cell { value: Value::Number(2.0), formula: Some("=1+1".into()) });
            sh.set(1, 0, Cell { value: Value::Number(2.0), formula: Some("=1+1".into()) });
        }
        let after_arrays = wb.sheet(0).arrays.clone();
        let deltas = finish(txn, &wb.sheets[0]);
        undo.push_with_arrays(
            Action {
                label: "数组公式".into(),
                deltas,
                merges: None,
                focus: (0, 0, 0),
                snap_before: None,
                snap_after: None,
                by_site: 0,
                pictures: None,
                shapes: None,
                refresh_sizes: None,
            },
            Some(ArrayEdit {
                sheet: 0,
                before: before_arrays,
                after: after_arrays,
            }),
        );

        assert_eq!(wb.sheet(0).arrays.len(), 1);
        assert!(undo.undo(&mut wb.sheets, |_, _, _, _| true).is_some());
        assert!(wb.sheet(0).arrays.is_empty());
        assert_eq!(wb.sheet(0).value(0, 0), Value::Empty);

        assert!(undo.redo(&mut wb.sheets, |_, _, _, _| true).is_some());
        assert_eq!(wb.sheet(0).arrays.len(), 1);
        assert_eq!(wb.sheet(0).value(0, 0), Value::Number(2.0));
        assert_eq!(wb.sheet(0).value(1, 0), Value::Number(2.0));
    }
}
