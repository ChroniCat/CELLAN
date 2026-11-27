//! Unified workbook transaction layer (`WorkbookTx`), reversible delta payloads
//! (`TxDelta`, `TxCommit`), post-commit side-effect descriptors (`TxEffects`),
//! and wire-compatible collaborative transaction patches (`TxPatch`, `TxOverlay`).
//!
//! All structural, partial-band cell shift, axis permutation, multi-insert, and
//! sheet-tab operations execute through a single atomic preflight -> snapshot ->
//! mutate -> effects pipeline, converging undo storage into `UndoStack`'s single
//! `Entry::Tx` variant.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::cell_shift_patch::CellShiftPatch;
use crate::model::{Cell, CellJournal, CellRef, Sheet, Workbook};
use crate::refs::{Axis, AxisMove};
use crate::sheet_ops::{DeletedSheetPivots, RenameReferences};
use crate::structural::{CellShift, CellShiftConflict, Structural};
use crate::transform::CoordTransform;
use crate::undo::{CellDelta, ChartUndoEdit, MetaEdit, PivotUndoEdit, StructureSnapshot};

pub use crate::cell_shift_patch::CellShiftOverlay as TxOverlay;

/// Sheet-tab structural mutation intent.
#[derive(Clone, Debug)]
pub enum SheetTabIntent {
    Append { sheet: Sheet },
    Insert { index: usize, sheet: Sheet },
    Delete { index: usize },
    Move { from: usize, to: usize },
    Rename { index: usize, name: String },
}

/// Workbook defined-name mutation intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefinedNameIntent {
    Define {
        name: String,
        scope: Option<usize>,
        formula: String,
    },
    Update {
        index: usize,
        name: String,
        scope: Option<usize>,
        formula: String,
    },
    Remove {
        index: usize,
    },
    ReplaceAll {
        names: Vec<(String, Option<usize>, String)>,
    },
}

/// Single cell value/formula and/or style write inside [`CellBatchIntent`].
#[derive(Clone, Debug, PartialEq)]
pub struct CellWrite {
    pub row: u32,
    pub col: u32,
    pub cell: Option<Cell>,
    pub style: Option<u16>,
}

impl CellWrite {
    pub fn cell(row: u32, col: u32, cell: Cell) -> Self {
        Self {
            row,
            col,
            cell: Some(cell),
            style: None,
        }
    }

    pub fn style(row: u32, col: u32, style: u16) -> Self {
        Self {
            row,
            col,
            cell: None,
            style: Some(style),
        }
    }

    pub fn cell_and_style(row: u32, col: u32, cell: Cell, style: u16) -> Self {
        Self {
            row,
            col,
            cell: Some(cell),
            style: Some(style),
        }
    }
}

/// Atomic batch of cell value/formula/style writes on a single sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct CellBatchIntent {
    pub sheet: usize,
    pub writes: Vec<CellWrite>,
}

/// Unified high-level workbook transaction intent covering coordinate
/// transformations, sheet-tab operations, defined names, and cell batches.
#[derive(Clone, Debug)]
pub enum TxIntent {
    Transform(CoordTransform),
    SheetTab(SheetTabIntent),
    DefinedNames(DefinedNameIntent),
    CellBatch(CellBatchIntent),
}

impl From<CoordTransform> for TxIntent {
    fn from(tx: CoordTransform) -> Self {
        Self::Transform(tx)
    }
}

impl From<SheetTabIntent> for TxIntent {
    fn from(tab: SheetTabIntent) -> Self {
        Self::SheetTab(tab)
    }
}

impl From<DefinedNameIntent> for TxIntent {
    fn from(names: DefinedNameIntent) -> Self {
        Self::DefinedNames(names)
    }
}

impl From<CellBatchIntent> for TxIntent {
    fn from(batch: CellBatchIntent) -> Self {
        Self::CellBatch(batch)
    }
}

impl TxIntent {
    pub fn from_structural(sheet_idx: usize, op: &Structural) -> Self {
        Self::Transform(CoordTransform::from_structural(sheet_idx, op))
    }

    pub fn from_cell_shift(sheet_idx: usize, op: &CellShift) -> Self {
        Self::Transform(CoordTransform::from_cell_shift(sheet_idx, op))
    }

    pub fn from_axis_move(
        sheet_idx: usize,
        axis: Axis,
        from: u32,
        n: u32,
        drop: u32,
    ) -> Option<Self> {
        CoordTransform::from_axis_move(sheet_idx, axis, from, n, drop).map(Self::Transform)
    }

    pub fn from_axis_permute(
        sheet_idx: usize,
        axis: Axis,
        from: u32,
        n: u32,
        at: u32,
    ) -> Option<Self> {
        CoordTransform::from_axis_permute(sheet_idx, axis, from, n, at).map(Self::Transform)
    }

    pub fn from_multi_insert(sheet_idx: usize, axis: Axis, positions: Vec<u32>) -> Self {
        Self::Transform(CoordTransform::from_multi_insert(sheet_idx, axis, positions))
    }

    pub fn from_sheet_tab(tab: SheetTabIntent) -> Self {
        Self::SheetTab(tab)
    }

    pub fn from_defined_names(names: DefinedNameIntent) -> Self {
        Self::DefinedNames(names)
    }

    pub fn from_cell_batch(sheet: usize, writes: Vec<CellWrite>) -> Self {
        Self::CellBatch(CellBatchIntent { sheet, writes })
    }

    /// Default Chinese user-facing undo/revision label for this transaction.
    pub fn default_label(&self) -> String {
        match self {
            Self::Transform(tx) => match tx {
                CoordTransform::AxisShift {
                    band: Some(_),
                    delete,
                    ..
                } => {
                    if *delete {
                        "删除单元格".into()
                    } else {
                        "插入单元格".into()
                    }
                }
                CoordTransform::AxisShift {
                    axis,
                    at,
                    n,
                    delete,
                    band: None,
                    ..
                } => {
                    let op = match (*axis, *delete) {
                        (Axis::Row, false) => Structural::InsertRows { at: *at, n: *n },
                        (Axis::Row, true) => Structural::DeleteRows { at: *at, n: *n },
                        (Axis::Col, false) => Structural::InsertCols { at: *at, n: *n },
                        (Axis::Col, true) => Structural::DeleteCols { at: *at, n: *n },
                    };
                    op.label()
                }
                CoordTransform::AxisPermute { axis, .. } => match axis {
                    Axis::Row => "移动行".into(),
                    Axis::Col => "移动列".into(),
                },
                CoordTransform::MultiInsert { axis, .. } => match axis {
                    Axis::Row => "批量插入行".into(),
                    Axis::Col => "批量插入列".into(),
                },
            },
            Self::SheetTab(tab) => match tab {
                SheetTabIntent::Append { .. } => "新建工作表".into(),
                SheetTabIntent::Insert { .. } => "插入工作表".into(),
                SheetTabIntent::Delete { .. } => "删除工作表".into(),
                SheetTabIntent::Move { .. } => "移动工作表".into(),
                SheetTabIntent::Rename { .. } => "重命名工作表".into(),
            },
            Self::DefinedNames(names) => match names {
                DefinedNameIntent::Define { name, .. } => format!("定义名称 {}", name.trim()),
                DefinedNameIntent::Update { .. } => "编辑名称".into(),
                DefinedNameIntent::Remove { .. } => "删除名称".into(),
                DefinedNameIntent::ReplaceAll { .. } => "更新名称".into(),
            },
            Self::CellBatch(_) => "批量写入".into(),
        }
    }
}

/// Reason a workbook transaction was rejected before mutating any state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxError {
    Conflict(CellShiftConflict),
    InvalidSheet,
    InvalidSheetName,
    CannotDeleteLastSheet,
    InvalidDefinedName,
    DuplicateDefinedName,
    InvalidCellCoord,
    NoOp,
}

impl TxError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::Conflict(c) => c.message(),
            Self::InvalidSheet => "目标工作表无效，未执行操作",
            Self::InvalidSheetName => "工作表名称无效或已存在",
            Self::CannotDeleteLastSheet => "至少保留一张工作表",
            Self::InvalidDefinedName => "定义名称无效或索引越界",
            Self::DuplicateDefinedName => "名称已存在",
            Self::InvalidCellCoord => "目标单元格坐标超出工作表范围",
            Self::NoOp => "操作未产生任何变更",
        }
    }
}

impl fmt::Display for TxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for TxError {}

impl From<CellShiftConflict> for TxError {
    fn from(c: CellShiftConflict) -> Self {
        Self::Conflict(c)
    }
}

/// Scope of formula recalculation required after committing a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecalcScope {
    None,
    Cells(Vec<(usize, u32, u32)>),
    Full,
}

/// Post-commit side effects required from the host/UI layer (recalc, dependency
/// and render cache invalidation, revision logging, detached-source warning,
/// and active sheet/cell focus update).
#[derive(Clone, Debug)]
pub struct TxEffects {
    pub recalc: RecalcScope,
    pub invalidate_deps: bool,
    pub invalidate_render_caches: bool,
    pub coord_changed: bool,
    pub structural: bool,
    pub detached_sources: bool,
    pub revision_deltas: Vec<CellDelta>,
    pub touched_sheets: Vec<usize>,
    pub new_focus: Option<(usize, u32, u32)>,
}

impl Default for TxEffects {
    fn default() -> Self {
        Self {
            recalc: RecalcScope::None,
            invalidate_deps: false,
            invalidate_render_caches: false,
            coord_changed: false,
            structural: false,
            detached_sources: false,
            revision_deltas: Vec::new(),
            touched_sheets: Vec::new(),
            new_focus: None,
        }
    }
}

/// Opaque wrapper around compressed / background-compacting structural undo
/// snapshots, preserving 100% of CoW sharing and worker-thread compaction.
pub struct StructureHistory {
    pub(crate) inner: crate::undo::structural_history::History,
    pub(crate) initial_bytes: usize,
}

impl StructureHistory {
    pub(crate) fn record(before: StructureSnapshot, after: StructureSnapshot) -> Self {
        let (inner, initial_bytes) = crate::undo::structural_history::History::record(before, after);
        Self {
            inner,
            initial_bytes,
        }
    }

    pub fn is_pending(&self) -> bool {
        self.inner.pending()
    }

    pub(crate) fn settle(&mut self, wait: bool) -> Option<usize> {
        self.inner.settle(wait)
    }

    pub(crate) fn snapshots(&self) -> Option<(&StructureSnapshot, &StructureSnapshot)> {
        self.inner.snapshots()
    }
}

impl fmt::Debug for StructureHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StructureHistory")
            .field("pending", &self.is_pending())
            .field("initial_bytes", &self.initial_bytes)
            .finish()
    }
}

/// Reversible sheet-tab mutation payload stored inside `TxDelta::SheetTab`.
#[derive(Debug)]
pub enum SheetTabChange {
    Append {
        removed: Option<Box<Sheet>>,
    },
    Insert {
        removed: Option<Box<Sheet>>,
    },
    Delete {
        sheet: Option<Box<Sheet>>,
        local_names: Vec<(String, Option<usize>, String)>,
        references: RenameReferences,
        pivots: DeletedSheetPivots,
    },
    Move {
        from: usize,
        to: usize,
    },
    Rename {
        before: String,
        after: String,
        references: RenameReferences,
    },
}

/// Wire-compatible generalized collaborative transaction patch.
///
/// Uses `#[serde(untagged)]` so `TxPatch::CellShift(p)` serializes and
/// deserializes byte-for-byte identically to `CellShiftPatch` on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TxPatch {
    CellShift(CellShiftPatch),
}

impl From<CellShiftPatch> for TxPatch {
    fn from(patch: CellShiftPatch) -> Self {
        Self::CellShift(patch)
    }
}

impl TxPatch {
    /// Prepare a portable collaborative patch for a supported `TxIntent`.
    pub fn prepare(wb: &Workbook, intent: &TxIntent) -> Option<Self> {
        match intent {
            TxIntent::Transform(tx) => {
                let cs = tx.as_cell_shift()?;
                CellShiftPatch::prepare(wb, tx.target_sheet(), cs).map(Self::CellShift)
            }
            TxIntent::SheetTab(_) | TxIntent::DefinedNames(_) | TxIntent::CellBatch(_) => None,
        }
    }

    pub fn from_cell_shift(patch: CellShiftPatch) -> Self {
        Self::CellShift(patch)
    }

    pub fn into_cell_shift(self) -> Option<CellShiftPatch> {
        match self {
            Self::CellShift(p) => Some(p),
        }
    }

    pub fn as_cell_shift(&self) -> Option<&CellShiftPatch> {
        match self {
            Self::CellShift(p) => Some(p),
        }
    }

    pub fn target_sheet(&self) -> usize {
        match self {
            Self::CellShift(p) => p.target_sheet(),
        }
    }

    pub fn referenced_sheets(&self) -> Vec<usize> {
        match self {
            Self::CellShift(p) => p.referenced_sheets(),
        }
    }

    pub fn wire_valid(&self) -> bool {
        match self {
            Self::CellShift(p) => p.wire_valid(),
        }
    }

    pub fn can_install(&self, wb: &Workbook) -> bool {
        match self {
            Self::CellShift(p) => p.can_install(wb),
        }
    }

    pub fn canonical_forward(&self, wb: &Workbook) -> Option<Self> {
        match self {
            Self::CellShift(p) => p.canonical_forward(wb).map(Self::CellShift),
        }
    }

    pub fn matches(&self, wb: &Workbook, forward: bool) -> bool {
        match self {
            Self::CellShift(p) => p.matches(wb, forward),
        }
    }

    pub fn matches_replica(&self, wb: &Workbook, forward: bool) -> bool {
        match self {
            Self::CellShift(p) => p.matches_replica(wb, forward),
        }
    }

    pub fn apply(&self, wb: &mut Workbook, forward: bool) -> bool {
        match self {
            Self::CellShift(p) => p.apply(wb, forward),
        }
    }

    pub fn apply_committed(&self, wb: &mut Workbook, forward: bool) -> bool {
        match self {
            Self::CellShift(p) => p.apply_committed(wb, forward),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::CellShift(p) => {
                if p.insert {
                    "插入单元格"
                } else {
                    "删除单元格"
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::CellShift(p) => p.is_empty(),
        }
    }
}

/// Unified reversible CAS payload stored inside `UndoStack`'s `Entry::Tx`.
#[derive(Debug)]
pub enum TxDelta {
    SparseShift {
        deltas: Vec<CellDelta>,
        sheet_ids: Vec<(usize, u64)>,
        focus_sheet: usize,
    },
    Structure(StructureHistory),
    RoomPatch {
        transaction: u64,
        patch: Arc<TxPatch>,
        sheet_ids: Vec<(usize, u64)>,
        ownership_conflict: bool,
    },
    SheetTab {
        index: usize,
        identity: u64,
        previous_focus: (usize, u32, u32),
        change: SheetTabChange,
    },
    Meta(Box<MetaEdit>),
    Chart(Box<ChartUndoEdit>),
    Pivot(Box<PivotUndoEdit>),
    DefinedNames {
        before: Vec<(String, Option<usize>, String)>,
        after: Vec<(String, Option<usize>, String)>,
    },
    CellBatch {
        deltas: Vec<CellDelta>,
        sheet_ids: Vec<(usize, u64)>,
    },
}

impl TxDelta {
    /// Returns `true` when this delta carries no undoable change (e.g. an empty
    /// sparse shift, empty cell batch, empty metadata edit, or unchanged names).
    pub fn is_empty(&self) -> bool {
        match self {
            Self::SparseShift { deltas, .. } | Self::CellBatch { deltas, .. } => deltas.is_empty(),
            Self::Meta(edit) => edit.is_empty(),
            Self::DefinedNames { before, after } => before == after,
            Self::Structure(_)
            | Self::RoomPatch { .. }
            | Self::SheetTab { .. }
            | Self::Chart(_)
            | Self::Pivot(_) => false,
        }
    }

    /// Whether replaying this delta takes the workbook-structural undo path
    /// (matching `UndoStack::undo_is_structure` and collaboration guard rules).
    pub fn is_structural(&self) -> bool {
        matches!(
            self,
            Self::Structure(_) | Self::SheetTab { .. } | Self::Chart(_) | Self::Pivot(_)
        )
    }
}

/// Result of executing a workbook transaction, ready to be pushed into
/// `UndoStack::push_tx` and consumed by the UI/host effect runner.
#[derive(Debug)]
pub struct TxCommit {
    pub label: String,
    pub focus: (usize, u32, u32),
    pub delta: TxDelta,
    pub effects: TxEffects,
    pub by_site: u32,
}

impl TxCommit {
    /// Wrap a pre-built sheet/workbook [`MetaEdit`] as a unified [`TxCommit`].
    pub fn from_meta(
        label: impl Into<String>,
        focus: (usize, u32, u32),
        edit: MetaEdit,
    ) -> Self {
        let recalc = if edit.names.is_some() || edit.filter.is_some() {
            RecalcScope::Full
        } else if !edit.rich.is_empty() {
            RecalcScope::Cells(
                edit.rich
                    .iter()
                    .map(|(pos, ..)| (edit.sheet, pos.0, pos.1))
                    .collect(),
            )
        } else {
            RecalcScope::None
        };
        let invalidate_deps = edit.names.is_some();
        let invalidate_render_caches = !edit.is_empty();
        let touched_sheets = if edit.is_empty() {
            Vec::new()
        } else {
            vec![edit.sheet]
        };
        Self {
            label: label.into(),
            focus,
            delta: TxDelta::Meta(Box::new(edit)),
            effects: TxEffects {
                recalc,
                invalidate_deps,
                invalidate_render_caches,
                coord_changed: false,
                structural: false,
                detached_sources: false,
                revision_deltas: Vec::new(),
                touched_sheets,
                new_focus: None,
            },
            by_site: 0,
        }
    }

    /// Wrap a pre-built [`ChartUndoEdit`] as a unified [`TxCommit`].
    pub fn from_chart(
        label: impl Into<String>,
        focus: (usize, u32, u32),
        edit: ChartUndoEdit,
    ) -> Self {
        let sheet = edit.sheet;
        Self {
            label: label.into(),
            focus,
            delta: TxDelta::Chart(Box::new(edit)),
            effects: TxEffects {
                recalc: RecalcScope::None,
                invalidate_deps: false,
                invalidate_render_caches: true,
                coord_changed: false,
                structural: false,
                detached_sources: false,
                revision_deltas: Vec::new(),
                touched_sheets: vec![sheet],
                new_focus: None,
            },
            by_site: 0,
        }
    }

    /// Wrap a pre-built [`PivotUndoEdit`] as a unified [`TxCommit`].
    pub fn from_pivot(
        label: impl Into<String>,
        focus: (usize, u32, u32),
        edit: PivotUndoEdit,
    ) -> Self {
        let has_inserted_sheet = edit.inserted_sheet.is_some();
        let touched_cells: Vec<(usize, u32, u32)> = edit
            .deltas
            .iter()
            .map(|d| (d.sheet, d.row, d.col))
            .collect();
        let recalc = if has_inserted_sheet {
            RecalcScope::Full
        } else if !touched_cells.is_empty() {
            RecalcScope::Cells(touched_cells)
        } else {
            RecalcScope::None
        };
        let sheet = edit.target_sheet;
        Self {
            label: label.into(),
            focus,
            delta: TxDelta::Pivot(Box::new(edit)),
            effects: TxEffects {
                recalc,
                invalidate_deps: has_inserted_sheet,
                invalidate_render_caches: true,
                coord_changed: has_inserted_sheet,
                structural: has_inserted_sheet,
                detached_sources: false,
                revision_deltas: Vec::new(),
                touched_sheets: vec![sheet],
                new_focus: None,
            },
            by_site: 0,
        }
    }

    /// Wrap pre-captured structural before/after snapshots as a unified [`TxCommit`].
    pub fn from_structure(
        label: impl Into<String>,
        focus: (usize, u32, u32),
        before: StructureSnapshot,
        after: StructureSnapshot,
        effects: TxEffects,
    ) -> Self {
        let history = StructureHistory::record(before, after);
        Self {
            label: label.into(),
            focus,
            delta: TxDelta::Structure(history),
            effects,
            by_site: 0,
        }
    }

    /// Override post-commit [`TxEffects`] on a [`TxCommit`].
    pub fn with_effects(mut self, effects: TxEffects) -> Self {
        self.effects = effects;
        self
    }
}

/// Single authoritative entry point for atomic workbook-level structural,
/// cell-shift, axis-permutation, multi-insert, and sheet-tab transactions.
pub struct WorkbookTx;

impl WorkbookTx {
    /// Validate `intent` against `wb` without mutating any state.
    pub fn preflight(wb: &Workbook, intent: &TxIntent) -> Result<(), TxError> {
        match intent {
            TxIntent::Transform(tx) => {
                let sheet_idx = tx.target_sheet();
                if wb.sheets.get(sheet_idx).is_none() {
                    return Err(TxError::InvalidSheet);
                }
                if tx.is_noop() {
                    return Err(TxError::NoOp);
                }
                tx.preflight(wb)?;
                Ok(())
            }
            TxIntent::SheetTab(tab) => match tab {
                SheetTabIntent::Append { sheet } => {
                    if !crate::sheet_ops::valid_name(wb, None, &sheet.name) {
                        return Err(TxError::InvalidSheetName);
                    }
                    Ok(())
                }
                SheetTabIntent::Insert { index, sheet } => {
                    if *index > wb.sheets.len() {
                        return Err(TxError::InvalidSheet);
                    }
                    if !crate::sheet_ops::valid_name(wb, None, &sheet.name) {
                        return Err(TxError::InvalidSheetName);
                    }
                    Ok(())
                }
                SheetTabIntent::Delete { index } => {
                    if *index >= wb.sheets.len() {
                        return Err(TxError::InvalidSheet);
                    }
                    if wb.sheets.len() <= 1 {
                        return Err(TxError::CannotDeleteLastSheet);
                    }
                    Ok(())
                }
                SheetTabIntent::Move { from, to } => {
                    if *from >= wb.sheets.len() || *to >= wb.sheets.len() {
                        return Err(TxError::InvalidSheet);
                    }
                    if *from == *to {
                        return Err(TxError::NoOp);
                    }
                    Ok(())
                }
                SheetTabIntent::Rename { index, name } => {
                    let Some(sheet) = wb.sheets.get(*index) else {
                        return Err(TxError::InvalidSheet);
                    };
                    let trimmed = name.trim();
                    if sheet.name == trimmed {
                        return Err(TxError::NoOp);
                    }
                    if !crate::sheet_ops::valid_name(wb, Some(*index), trimmed) {
                        return Err(TxError::InvalidSheetName);
                    }
                    Ok(())
                }
            },
            TxIntent::DefinedNames(names) => match names {
                DefinedNameIntent::Define { name, scope, .. } => {
                    let trimmed = name.trim();
                    if trimmed.is_empty() {
                        return Err(TxError::InvalidDefinedName);
                    }
                    if scope.is_some_and(|i| i >= wb.sheets.len()) {
                        return Err(TxError::InvalidSheet);
                    }
                    if wb
                        .names
                        .iter()
                        .any(|(n, sc, _)| *sc == *scope && n.eq_ignore_ascii_case(trimmed))
                    {
                        return Err(TxError::DuplicateDefinedName);
                    }
                    Ok(())
                }
                DefinedNameIntent::Update {
                    index,
                    name,
                    scope,
                    formula,
                } => {
                    let Some(existing) = wb.names.get(*index) else {
                        return Err(TxError::InvalidDefinedName);
                    };
                    let trimmed = name.trim();
                    if trimmed.is_empty() {
                        return Err(TxError::InvalidDefinedName);
                    }
                    if scope.is_some_and(|i| i >= wb.sheets.len()) {
                        return Err(TxError::InvalidSheet);
                    }
                    if wb.names.iter().enumerate().any(|(k, (n, sc, _))| {
                        k != *index && *sc == *scope && n.eq_ignore_ascii_case(trimmed)
                    }) {
                        return Err(TxError::DuplicateDefinedName);
                    }
                    if existing.0 == trimmed
                        && existing.1 == *scope
                        && existing.2 == formula.trim()
                    {
                        return Err(TxError::NoOp);
                    }
                    Ok(())
                }
                DefinedNameIntent::Remove { index } => {
                    if *index >= wb.names.len() {
                        return Err(TxError::InvalidDefinedName);
                    }
                    Ok(())
                }
                DefinedNameIntent::ReplaceAll { names } => {
                    let mut seen = std::collections::HashSet::with_capacity(names.len());
                    for (name, scope, _) in names {
                        let trimmed = name.trim();
                        if trimmed.is_empty() {
                            return Err(TxError::InvalidDefinedName);
                        }
                        if scope.is_some_and(|i| i >= wb.sheets.len()) {
                            return Err(TxError::InvalidSheet);
                        }
                        if !seen.insert((*scope, trimmed.to_ascii_lowercase())) {
                            return Err(TxError::DuplicateDefinedName);
                        }
                    }
                    if names.len() == wb.names.len()
                        && names
                            .iter()
                            .zip(&wb.names)
                            .all(|((n, sc, f), (en, esc, ef))| {
                                n.trim() == en && sc == esc && f == ef
                            })
                    {
                        return Err(TxError::NoOp);
                    }
                    Ok(())
                }
            },
            TxIntent::CellBatch(batch) => {
                if wb.sheets.get(batch.sheet).is_none() {
                    return Err(TxError::InvalidSheet);
                }
                if batch
                    .writes
                    .iter()
                    .any(|w| w.row >= crate::sync::MAX_ROWS || w.col >= crate::sync::MAX_COLS)
                {
                    return Err(TxError::InvalidCellCoord);
                }
                Ok(())
            }
        }
    }

    /// Execute `intent` with its default Chinese undo/revision label.
    pub fn execute(
        wb: &mut Workbook,
        intent: TxIntent,
        focus: (usize, u32, u32),
    ) -> Result<TxCommit, TxError> {
        Self::execute_with_label(wb, intent, None, focus)
    }

    /// Atomically preflight, snapshot, execute, and produce a `TxCommit` with
    /// exact reversible `TxDelta` and `TxEffects`. On `Err`, `wb` is untouched.
    pub fn execute_with_label(
        wb: &mut Workbook,
        intent: TxIntent,
        label: Option<String>,
        focus: (usize, u32, u32),
    ) -> Result<TxCommit, TxError> {
        Self::preflight(wb, &intent)?;
        let label = label.unwrap_or_else(|| intent.default_label());
        match intent {
            TxIntent::Transform(tx) => Self::execute_transform(wb, tx, label, focus),
            TxIntent::SheetTab(tab) => Self::execute_sheet_tab(wb, tab, label, focus),
            TxIntent::DefinedNames(names) => Self::execute_defined_names(wb, names, label, focus),
            TxIntent::CellBatch(batch) => Self::execute_cell_batch(wb, batch, label, focus),
        }
    }

    fn execute_transform(
        wb: &mut Workbook,
        tx: CoordTransform,
        label: String,
        focus: (usize, u32, u32),
    ) -> Result<TxCommit, TxError> {
        match tx {
            CoordTransform::AxisShift {
                sheet_idx,
                band: Some(_),
                ..
            } => {
                let Some(op) = tx.as_cell_shift() else {
                    return Err(TxError::Conflict(CellShiftConflict::InvalidTarget));
                };
                Self::execute_cell_shift(wb, sheet_idx, op, label, focus)
            }
            CoordTransform::AxisShift {
                sheet_idx,
                axis,
                at,
                n,
                delete,
                band: None,
            } => {
                let op = match (axis, delete) {
                    (Axis::Row, false) => Structural::InsertRows { at, n },
                    (Axis::Row, true) => Structural::DeleteRows { at, n },
                    (Axis::Col, false) => Structural::InsertCols { at, n },
                    (Axis::Col, true) => Structural::DeleteCols { at, n },
                };
                let before = crate::undo::snapshot_structure(wb, sheet_idx, &op);
                crate::structural::apply_structural_wb(wb, sheet_idx, &op);
                wb.dirty = true;
                let detached_sources = before.detached_source(wb);
                let touched_sheets = structure_touched_sheets(&before, wb, sheet_idx);
                let after = before.capture_after(wb);
                let history = StructureHistory::record(before, after);
                Ok(TxCommit {
                    label,
                    focus,
                    delta: TxDelta::Structure(history),
                    effects: TxEffects {
                        recalc: RecalcScope::Full,
                        invalidate_deps: true,
                        invalidate_render_caches: true,
                        coord_changed: true,
                        structural: true,
                        detached_sources,
                        revision_deltas: Vec::new(),
                        touched_sheets,
                        new_focus: None,
                    },
                    by_site: 0,
                })
            }
            CoordTransform::AxisPermute {
                sheet_idx,
                axis,
                from,
                n,
                at,
            } => {
                let mv = AxisMove { axis, from, n, at };
                let before = crate::undo::snapshot_axis_permute(wb, sheet_idx, mv);
                wb.sheets[sheet_idx].move_axis_inner(mv, false);
                CoordTransform::from_raw_axis_move(sheet_idx, mv).apply_workbook_references(wb);
                wb.dirty = true;
                let detached_sources = before.detached_source(wb);
                let touched_sheets = structure_touched_sheets(&before, wb, sheet_idx);
                let after = before.capture_after(wb);
                let history = StructureHistory::record(before, after);
                Ok(TxCommit {
                    label,
                    focus,
                    delta: TxDelta::Structure(history),
                    effects: TxEffects {
                        recalc: RecalcScope::Full,
                        invalidate_deps: true,
                        invalidate_render_caches: true,
                        coord_changed: true,
                        structural: true,
                        detached_sources,
                        revision_deltas: Vec::new(),
                        touched_sheets,
                        new_focus: None,
                    },
                    by_site: 0,
                })
            }
            CoordTransform::MultiInsert {
                sheet_idx,
                axis,
                mut positions,
            } => {
                positions.sort_unstable();
                let first = positions[0];
                let earliest = match axis {
                    Axis::Row => Structural::InsertRows { at: first, n: 1 },
                    Axis::Col => Structural::InsertCols { at: first, n: 1 },
                };
                let before = crate::undo::snapshot_structure(wb, sheet_idx, &earliest);
                match axis {
                    Axis::Row => {
                        crate::structural::insert_subtotal_rows(wb, sheet_idx, &positions);
                    }
                    Axis::Col => {
                        for &at in positions.iter().rev() {
                            crate::structural::apply_structural_wb(
                                wb,
                                sheet_idx,
                                &Structural::InsertCols { at, n: 1 },
                            );
                        }
                    }
                }
                wb.dirty = true;
                let detached_sources = before.detached_source(wb);
                let touched_sheets = structure_touched_sheets(&before, wb, sheet_idx);
                let after = before.capture_after(wb);
                let history = StructureHistory::record(before, after);
                Ok(TxCommit {
                    label,
                    focus,
                    delta: TxDelta::Structure(history),
                    effects: TxEffects {
                        recalc: RecalcScope::Full,
                        invalidate_deps: true,
                        invalidate_render_caches: true,
                        coord_changed: true,
                        structural: true,
                        detached_sources,
                        revision_deltas: Vec::new(),
                        touched_sheets,
                        new_focus: None,
                    },
                    by_site: 0,
                })
            }
        }
    }

    fn execute_cell_shift(
        wb: &mut Workbook,
        sheet_idx: usize,
        op: CellShift,
        label: String,
        focus: (usize, u32, u32),
    ) -> Result<TxCommit, TxError> {
        // 1. Snapshot metadata only when metadata exists on the workbook.
        let before_meta = crate::undo::snapshot_cell_shift_if_metadata(wb, sheet_idx, &op);

        // 2. Capture O(pages) CoW references to cell/style stores across all sheets
        //    so exact moved + rewritten CellDeltas are derived for both sparse undo
        //    and per-sheet revision logging without scanning empty grid space.
        let before_stores: Vec<(crate::store::Store, crate::store::CellStyles)> = wb
            .sheets
            .iter()
            .map(|sh| (sh.cells().clone(), sh.styles.clone()))
            .collect();

        // 3. Apply the partial-band cell shift + unified workbook reference rewrite.
        if !op.apply(wb, sheet_idx) {
            return Err(TxError::Conflict(CellShiftConflict::InvalidTarget));
        }
        wb.dirty = true;

        // 4. Derive exact sorted CellDeltas across every sheet whose cells/styles changed.
        let mut deltas: Vec<CellDelta> = Vec::new();
        let mut touched_sheets: Vec<usize> = vec![sheet_idx];
        for (idx, (before_cells, before_styles)) in before_stores.iter().enumerate() {
            let after_sheet = &wb.sheets[idx];
            if before_cells.version == after_sheet.cells().version
                && before_styles.version == after_sheet.styles.version
            {
                continue;
            }
            let mut touched = CellJournal::default();
            before_cells.diff_into_journal(after_sheet.cells(), &mut touched);
            before_styles.diff_into_journal(&after_sheet.styles, &mut touched);
            if touched.is_empty() {
                continue;
            }
            let mut coords: Vec<CellRef> = touched.iter().collect();
            coords.sort_unstable();
            let mut sheet_had_delta = false;
            for key in coords {
                let (r, c) = (key.row, key.col);
                let before = before_cells
                    .get(r, c)
                    .map(|cell| cell.into_owned())
                    .unwrap_or_default();
                let after = after_sheet
                    .get(r, c)
                    .map(|cell| cell.into_owned())
                    .unwrap_or_default();
                // Partial cell shifts never mutate row_styles/col_styles.
                let before_style = crate::undo::effective_snapshot_style(
                    before_styles,
                    &after_sheet.row_styles,
                    &after_sheet.col_styles,
                    r,
                    c,
                );
                let after_style = after_sheet.style_of(r, c);
                if crate::undo::undo_values_eq(&before.value, &after.value)
                    && before.formula == after.formula
                    && before_style == after_style
                {
                    continue;
                }
                sheet_had_delta = true;
                deltas.push(CellDelta {
                    sheet: idx,
                    row: r,
                    col: c,
                    before,
                    after,
                    before_style,
                    after_style,
                });
            }
            if sheet_had_delta && idx != sheet_idx {
                touched_sheets.push(idx);
            }
        }

        let detached_sources = before_meta
            .as_ref()
            .is_some_and(|snapshot| snapshot.detached_source(wb));

        if let Some(before) = before_meta.filter(|s| s.cell_shift_changed_metadata(wb)) {
            touched_sheets.extend(before.foreign_sheets());
            touched_sheets.sort_unstable();
            touched_sheets.dedup();
            let after = before.capture_after(wb);
            let history = StructureHistory::record(before, after);
            Ok(TxCommit {
                label,
                focus,
                delta: TxDelta::Structure(history),
                effects: TxEffects {
                    recalc: RecalcScope::Full,
                    invalidate_deps: true,
                    invalidate_render_caches: true,
                    coord_changed: true,
                    structural: true,
                    detached_sources,
                    revision_deltas: deltas,
                    touched_sheets,
                    new_focus: None,
                },
                by_site: 0,
            })
        } else {
            touched_sheets.sort_unstable();
            touched_sheets.dedup();
            let mut id_indices: Vec<usize> = deltas
                .chunk_by(|a, b| a.sheet == b.sheet)
                .map(|group| group[0].sheet)
                .collect();
            id_indices.push(sheet_idx);
            id_indices.sort_unstable();
            id_indices.dedup();
            let sheet_ids = id_indices
                .into_iter()
                .map(|idx| (idx, wb.sheets[idx].identity))
                .collect();
            Ok(TxCommit {
                label,
                focus,
                delta: TxDelta::SparseShift {
                    deltas: deltas.clone(),
                    sheet_ids,
                    focus_sheet: sheet_idx,
                },
                effects: TxEffects {
                    recalc: RecalcScope::Full,
                    invalidate_deps: true,
                    invalidate_render_caches: true,
                    coord_changed: true,
                    structural: false,
                    detached_sources,
                    revision_deltas: deltas,
                    touched_sheets,
                    new_focus: None,
                },
                by_site: 0,
            })
        }
    }

    fn execute_sheet_tab(
        wb: &mut Workbook,
        tab: SheetTabIntent,
        label: String,
        focus: (usize, u32, u32),
    ) -> Result<TxCommit, TxError> {
        match tab {
            SheetTabIntent::Append { sheet } => {
                let index = wb.sheets.len();
                if !crate::sheet_ops::insert_sheet(wb, index, sheet) {
                    return Err(TxError::InvalidSheetName);
                }
                let identity = wb.sheets[index].identity;
                Ok(TxCommit {
                    label,
                    focus,
                    delta: TxDelta::SheetTab {
                        index,
                        identity,
                        previous_focus: focus,
                        change: SheetTabChange::Append { removed: None },
                    },
                    effects: TxEffects {
                        recalc: RecalcScope::Full,
                        invalidate_deps: true,
                        invalidate_render_caches: true,
                        coord_changed: true,
                        structural: true,
                        detached_sources: false,
                        revision_deltas: Vec::new(),
                        touched_sheets: vec![index],
                        new_focus: Some((index, 0, 0)),
                    },
                    by_site: 0,
                })
            }
            SheetTabIntent::Insert { index, sheet } => {
                if !crate::sheet_ops::insert_sheet(wb, index, sheet) {
                    return Err(TxError::InvalidSheetName);
                }
                let identity = wb.sheets[index].identity;
                Ok(TxCommit {
                    label,
                    focus,
                    delta: TxDelta::SheetTab {
                        index,
                        identity,
                        previous_focus: focus,
                        change: SheetTabChange::Insert { removed: None },
                    },
                    effects: TxEffects {
                        recalc: RecalcScope::Full,
                        invalidate_deps: true,
                        invalidate_render_caches: true,
                        coord_changed: true,
                        structural: true,
                        detached_sources: false,
                        revision_deltas: Vec::new(),
                        touched_sheets: vec![index],
                        new_focus: Some((index, 0, 0)),
                    },
                    by_site: 0,
                })
            }
            SheetTabIntent::Delete { index } => {
                let Some(rec) = crate::sheet_ops::delete_with_history(wb, index) else {
                    return Err(if wb.sheets.len() <= 1 {
                        TxError::CannotDeleteLastSheet
                    } else {
                        TxError::InvalidSheet
                    });
                };
                let identity = rec.sheet.identity;
                let new_focus = if focus.0 == index {
                    Some((index.min(wb.sheets.len().saturating_sub(1)), 0, 0))
                } else if focus.0 > index {
                    Some((focus.0 - 1, focus.1, focus.2))
                } else {
                    None
                };
                let touched_sheets = (0..wb.sheets.len()).collect();
                Ok(TxCommit {
                    label,
                    focus,
                    delta: TxDelta::SheetTab {
                        index,
                        identity,
                        previous_focus: focus,
                        change: SheetTabChange::Delete {
                            sheet: Some(Box::new(rec.sheet)),
                            local_names: rec.local_names,
                            references: rec.references,
                            pivots: rec.pivots,
                        },
                    },
                    effects: TxEffects {
                        recalc: RecalcScope::Full,
                        invalidate_deps: true,
                        invalidate_render_caches: true,
                        coord_changed: true,
                        structural: true,
                        detached_sources: false,
                        revision_deltas: Vec::new(),
                        touched_sheets,
                        new_focus,
                    },
                    by_site: 0,
                })
            }
            SheetTabIntent::Move { from, to } => {
                let Some(identity) = wb.sheets.get(from).map(|s| s.identity) else {
                    return Err(TxError::InvalidSheet);
                };
                let active_id = wb.sheets.get(focus.0).map(|s| s.identity);
                if !crate::sheet_ops::reorder_sheet(wb, from, to) {
                    return Err(TxError::InvalidSheet);
                }
                let new_focus = active_id
                    .and_then(|id| wb.sheets.iter().position(|s| s.identity == id))
                    .filter(|&new_idx| new_idx != focus.0)
                    .map(|new_idx| (new_idx, focus.1, focus.2));
                let lo = from.min(to);
                let hi = from.max(to);
                let touched_sheets = (lo..=hi).collect();
                Ok(TxCommit {
                    label,
                    focus,
                    delta: TxDelta::SheetTab {
                        index: to,
                        identity,
                        previous_focus: focus,
                        change: SheetTabChange::Move { from, to },
                    },
                    effects: TxEffects {
                        recalc: RecalcScope::Full,
                        invalidate_deps: true,
                        invalidate_render_caches: true,
                        coord_changed: true,
                        structural: true,
                        detached_sources: false,
                        revision_deltas: Vec::new(),
                        touched_sheets,
                        new_focus,
                    },
                    by_site: 0,
                })
            }
            SheetTabIntent::Rename { index, name } => {
                let Some(sheet) = wb.sheets.get(index) else {
                    return Err(TxError::InvalidSheet);
                };
                let before = sheet.name.clone();
                let identity = sheet.identity;
                let Some(references) = crate::sheet_ops::rename_with_history(wb, index, &name)
                else {
                    return Err(TxError::InvalidSheetName);
                };
                let after = wb.sheets[index].name.clone();
                let touched_sheets = (0..wb.sheets.len()).collect();
                Ok(TxCommit {
                    label,
                    focus,
                    delta: TxDelta::SheetTab {
                        index,
                        identity,
                        previous_focus: focus,
                        change: SheetTabChange::Rename {
                            before,
                            after,
                            references,
                        },
                    },
                    effects: TxEffects {
                        recalc: RecalcScope::Full,
                        invalidate_deps: true,
                        invalidate_render_caches: true,
                        coord_changed: true,
                        structural: true,
                        detached_sources: false,
                        revision_deltas: Vec::new(),
                        touched_sheets,
                        new_focus: None,
                    },
                    by_site: 0,
                })
            }
        }
    }

    fn execute_defined_names(
        wb: &mut Workbook,
        names: DefinedNameIntent,
        label: String,
        focus: (usize, u32, u32),
    ) -> Result<TxCommit, TxError> {
        let before = wb.names.clone();
        match names {
            DefinedNameIntent::Define {
                name,
                scope,
                formula,
            } => {
                wb.names
                    .push((name.trim().to_string(), scope, formula.trim().to_string()));
            }
            DefinedNameIntent::Update {
                index,
                name,
                scope,
                formula,
            } => {
                wb.names[index] = (name.trim().to_string(), scope, formula.trim().to_string());
            }
            DefinedNameIntent::Remove { index } => {
                wb.names.remove(index);
            }
            DefinedNameIntent::ReplaceAll { names } => {
                wb.names = names
                    .into_iter()
                    .map(|(n, sc, f)| (n.trim().to_string(), sc, f))
                    .collect();
            }
        }
        let after = wb.names.clone();
        wb.names_dirty = true;
        wb.dirty = true;
        let touched_sheets = (0..wb.sheets.len()).collect();
        Ok(TxCommit {
            label,
            focus,
            delta: TxDelta::DefinedNames { before, after },
            effects: TxEffects {
                recalc: RecalcScope::Full,
                invalidate_deps: true,
                invalidate_render_caches: true,
                coord_changed: false,
                structural: false,
                detached_sources: false,
                revision_deltas: Vec::new(),
                touched_sheets,
                new_focus: None,
            },
            by_site: 0,
        })
    }

    fn execute_cell_batch(
        wb: &mut Workbook,
        batch: CellBatchIntent,
        label: String,
        focus: (usize, u32, u32),
    ) -> Result<TxCommit, TxError> {
        let sheet_idx = batch.sheet;
        let mut seen = std::collections::HashSet::with_capacity(batch.writes.len());
        let mut keys = Vec::with_capacity(batch.writes.len());
        for w in &batch.writes {
            if seen.insert((w.row, w.col)) {
                keys.push((w.row, w.col));
            }
        }
        let before = crate::undo::capture(&wb.sheets[sheet_idx], sheet_idx, &keys);
        {
            let sh = &mut wb.sheets[sheet_idx];
            for w in batch.writes {
                if let Some(cell) = w.cell {
                    let changed = match sh.get(w.row, w.col) {
                        Some(cur) => {
                            cur.formula != cell.formula
                                || !crate::undo::undo_values_eq(&cur.value, &cell.value)
                        }
                        None => !cell.is_blank(),
                    };
                    if changed {
                        if cell.is_blank() {
                            sh.remove(w.row, w.col);
                        } else {
                            sh.set(w.row, w.col, cell);
                        }
                    }
                }
                if let Some(style) = w.style
                    && sh.style_of(w.row, w.col) != style
                {
                    sh.set_style(w.row, w.col, style);
                }
            }
        }
        let deltas = crate::undo::finish(before, &wb.sheets[sheet_idx]);
        let has_changes = !deltas.is_empty();
        if has_changes {
            wb.dirty = true;
        }
        let touched: Vec<(usize, u32, u32)> =
            deltas.iter().map(|d| (d.sheet, d.row, d.col)).collect();
        let recalc = if touched.is_empty() {
            RecalcScope::None
        } else {
            RecalcScope::Cells(touched)
        };
        let sheet_ids = if has_changes {
            vec![(sheet_idx, wb.sheets[sheet_idx].identity)]
        } else {
            Vec::new()
        };
        let touched_sheets = if has_changes {
            vec![sheet_idx]
        } else {
            Vec::new()
        };
        Ok(TxCommit {
            label,
            focus,
            delta: TxDelta::CellBatch {
                deltas: deltas.clone(),
                sheet_ids,
            },
            effects: TxEffects {
                recalc,
                invalidate_deps: false,
                invalidate_render_caches: has_changes,
                coord_changed: false,
                structural: false,
                detached_sources: false,
                revision_deltas: deltas,
                touched_sheets,
                new_focus: None,
            },
            by_site: 0,
        })
    }
}

fn structure_touched_sheets(
    before: &StructureSnapshot,
    _wb: &Workbook,
    sheet_idx: usize,
) -> Vec<usize> {
    let mut sheets = vec![sheet_idx];
    sheets.extend(before.foreign_sheets());
    sheets.sort_unstable();
    sheets.dedup();
    sheets
}
