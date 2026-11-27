//! Portable, wire-valid, CAS-checked collaborative room transaction patches
//! (`DefinedNamesPatch`, `AxisPermutePatch`, `RoomTxPatch`).
//!
//! Re-exported from [`crate::tx`] and carried over `Msg::RoomTx*` frames in
//! [`crate::sync`], while keeping `Msg::CellShift*` and `TxPatch::CellShift`
//! 100% wire-compatible with older peers.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::cell_shift_patch::CellShiftPatch;
use crate::model::Workbook;
use crate::refs::{Axis, AxisMove};
use crate::transform::CoordTransform;
use crate::tx::{DefinedNameIntent, TxIntent, TxPatch, WorkbookTx};

const MAX_DEFINED_NAMES: usize = 16_384;

#[inline]
pub(crate) fn is_shared_defined_name(name: &str) -> bool {
    !name.starts_with("_sc_src_")
}

#[inline]
fn valid_sheet_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= crate::sync::MAX_SHORT_TEXT
}

#[inline]
fn valid_defined_name(name: &str, scope: Option<usize>) -> bool {
    !name.is_empty()
        && name.chars().count() <= 255
        && is_shared_defined_name(name)
        && scope.is_none_or(|idx| idx < crate::sync::MAX_SHEET_INDEX)
}

#[inline]
fn valid_name_formula(text: &str) -> bool {
    text.len() <= crate::sync::MAX_THREAD_ENTRY_TEXT_BYTES
}

fn valid_sheet_name_table(sheet_names: &[(usize, String)]) -> bool {
    if sheet_names.len() > crate::sync::MAX_SHEET_INDEX {
        return false;
    }
    let mut seen = HashSet::with_capacity(sheet_names.len());
    sheet_names.iter().all(|(idx, name)| {
        *idx < crate::sync::MAX_SHEET_INDEX && valid_sheet_name(name) && seen.insert(*idx)
    })
}

/// Portable CAS-checked room transaction patch for workbook defined-name edits
/// (add, edit, delete, or batch replace), excluding private `_sc_src_*` entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinedNamesPatch {
    pub sheet_names: Vec<(usize, String)>,
    pub before: Vec<(String, Option<usize>, String)>,
    pub after: Vec<(String, Option<usize>, String)>,
}

impl DefinedNamesPatch {
    /// Prepare a portable `DefinedNamesPatch` from a [`DefinedNameIntent`].
    pub fn prepare(wb: &Workbook, intent: &DefinedNameIntent) -> Option<Self> {
        WorkbookTx::preflight(wb, &TxIntent::DefinedNames(intent.clone())).ok()?;
        let mut after_all = wb.names.clone();
        match intent {
            DefinedNameIntent::Define {
                name,
                scope,
                formula,
            } => {
                after_all.push((name.trim().to_string(), *scope, formula.trim().to_string()));
            }
            DefinedNameIntent::Update {
                index,
                name,
                scope,
                formula,
            } => {
                *after_all.get_mut(*index)? =
                    (name.trim().to_string(), *scope, formula.trim().to_string());
            }
            DefinedNameIntent::Remove { index } => {
                if *index >= after_all.len() {
                    return None;
                }
                after_all.remove(*index);
            }
            DefinedNameIntent::ReplaceAll { names } => {
                after_all = names
                    .iter()
                    .map(|(n, sc, f)| (n.trim().to_string(), *sc, f.clone()))
                    .collect();
            }
        }
        Self::from_shared_diff(wb, &wb.names, &after_all)
    }

    /// Build a portable `DefinedNamesPatch` from precomputed before/after name lists.
    pub fn from_shared_diff(
        wb: &Workbook,
        before_all: &[(String, Option<usize>, String)],
        after_all: &[(String, Option<usize>, String)],
    ) -> Option<Self> {
        let before: Vec<_> = before_all
            .iter()
            .filter(|(n, _, _)| is_shared_defined_name(n))
            .cloned()
            .collect();
        let after: Vec<_> = after_all
            .iter()
            .filter(|(n, _, _)| is_shared_defined_name(n))
            .cloned()
            .collect();
        if before == after {
            return None;
        }
        let mut scopes = BTreeSet::new();
        for (_, scope, _) in before.iter().chain(&after) {
            if let Some(idx) = *scope {
                wb.sheets.get(idx)?;
                scopes.insert(idx);
            }
        }
        let sheet_names = scopes
            .into_iter()
            .map(|idx| (idx, wb.sheets[idx].name.clone()))
            .collect();
        let patch = Self {
            sheet_names,
            before,
            after,
        };
        patch.can_install(wb).then_some(patch)
    }

    pub fn target_sheet(&self) -> usize {
        self.sheet_names.first().map_or(0, |(idx, _)| *idx)
    }

    pub fn referenced_sheets(&self) -> Vec<usize> {
        let mut sheets: Vec<usize> = self.sheet_names.iter().map(|(i, _)| *i).collect();
        sheets.sort_unstable();
        sheets.dedup();
        sheets
    }

    pub fn is_empty(&self) -> bool {
        self.before == self.after
    }

    pub fn wire_valid(&self) -> bool {
        if self.before == self.after
            || self.before.len() > MAX_DEFINED_NAMES
            || self.after.len() > MAX_DEFINED_NAMES
            || !valid_sheet_name_table(&self.sheet_names)
        {
            return false;
        }
        let known_sheets: HashSet<usize> = self.sheet_names.iter().map(|(i, _)| *i).collect();
        let entries_valid = |list: &[(String, Option<usize>, String)]| {
            list.iter().all(|(n, sc, f)| {
                valid_defined_name(n, *sc)
                    && valid_name_formula(f)
                    && sc.is_none_or(|idx| known_sheets.contains(&idx))
            })
        };
        if !entries_valid(&self.before) || !entries_valid(&self.after) {
            return false;
        }
        let mut seen_after = HashSet::with_capacity(self.after.len());
        self.after
            .iter()
            .all(|(n, sc, _)| seen_after.insert((*sc, n.to_ascii_lowercase())))
    }

    pub fn can_install(&self, wb: &Workbook) -> bool {
        self.wire_valid()
            && self
                .sheet_names
                .iter()
                .all(|(idx, name)| wb.sheets.get(*idx).is_some_and(|s| s.name == *name))
    }

    pub fn canonical_forward(&self, wb: &Workbook) -> Option<Self> {
        self.matches(wb, true).then(|| self.clone())
    }

    pub fn matches(&self, wb: &Workbook, forward: bool) -> bool {
        if !self.can_install(wb) {
            return false;
        }
        let expected = if forward { &self.before } else { &self.after };
        wb.names
            .iter()
            .filter(|(n, _, _)| is_shared_defined_name(n))
            .eq(expected.iter())
    }

    pub fn matches_replica(&self, wb: &Workbook, forward: bool) -> bool {
        self.matches(wb, forward)
    }

    pub fn apply(&self, wb: &mut Workbook, forward: bool) -> bool {
        self.matches(wb, forward) && self.apply_committed(wb, forward)
    }

    pub fn apply_committed(&self, wb: &mut Workbook, forward: bool) -> bool {
        if !self.can_install(wb) {
            return false;
        }
        let desired = if forward { &self.after } else { &self.before };
        wb.names.retain(|(n, _, _)| !is_shared_defined_name(n));
        wb.names.extend_from_slice(desired);
        wb.names_dirty = true;
        wb.dirty = true;
        true
    }

    pub fn label(&self) -> &'static str {
        match self.before.len().cmp(&self.after.len()) {
            std::cmp::Ordering::Less => "定义名称",
            std::cmp::Ordering::Greater => "删除名称",
            std::cmp::Ordering::Equal => "编辑名称",
        }
    }
}

/// Portable room transaction patch for whole-row/whole-column contiguous block
/// moves (`move_rows_wb` / `move_cols_wb`).
///
/// Works for million-row sheets in `O(occupied_in_band)` with zero 200k wire
/// entry cap because host and replicas apply the exact same bijective
/// permutation (`AxisMove`) across `Workbook`, `Peer`, and `LockTable`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisPermutePatch {
    pub sheet: usize,
    pub sheet_names: Vec<(usize, String)>,
    /// `false` => `Axis::Row`, `true` => `Axis::Col`.
    pub horizontal: bool,
    /// Original 0-based starting row/column of the contiguous block `[from, from + n)`.
    pub from: u32,
    /// Block width/height (`>= 1`).
    pub n: u32,
    /// Final 0-based starting row/column of the moved block `[at, at + n)` (`at != from`).
    pub at: u32,
}

impl AxisPermutePatch {
    /// Prepare from final destination coordinates `[from, from + n) -> [at, at + n)`.
    pub fn prepare(
        wb: &Workbook,
        sheet: usize,
        axis: Axis,
        from: u32,
        n: u32,
        at: u32,
    ) -> Option<Self> {
        let tx = CoordTransform::from_axis_permute(sheet, axis, from, n, at)?;
        tx.preflight(wb).ok()?;
        let sheet_names = wb
            .sheets
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.name.clone()))
            .collect();
        let patch = Self {
            sheet,
            sheet_names,
            horizontal: axis == Axis::Col,
            from,
            n,
            at,
        };
        patch.can_install(wb).then_some(patch)
    }

    /// Prepare from a UI drop-line coordinate (`move_rows_wb` / `move_cols_wb`).
    pub fn from_drop(
        wb: &Workbook,
        sheet: usize,
        axis: Axis,
        from: u32,
        n: u32,
        drop: u32,
    ) -> Option<Self> {
        let mv = AxisMove::from_drop(axis, from, n, drop)?;
        Self::prepare(wb, sheet, mv.axis, mv.from, mv.n, mv.at)
    }

    #[inline]
    pub fn axis(&self) -> Axis {
        if self.horizontal {
            Axis::Col
        } else {
            Axis::Row
        }
    }

    /// Effective `(axis, from, n, at)` for forward (`true`) or undo (`false`) replay.
    #[inline]
    pub fn effective_move(&self, forward: bool) -> (Axis, u32, u32, u32) {
        if forward {
            (self.axis(), self.from, self.n, self.at)
        } else {
            (self.axis(), self.at, self.n, self.from)
        }
    }

    pub fn effective_transform(&self, forward: bool) -> Option<CoordTransform> {
        let (axis, from, n, at) = self.effective_move(forward);
        CoordTransform::from_axis_permute(self.sheet, axis, from, n, at)
    }

    /// Bijective geometric inverse swapping `from` and `at`.
    pub fn inverse(&self) -> Self {
        Self {
            from: self.at,
            at: self.from,
            ..self.clone()
        }
    }

    /// Half-open `[lo, hi)` axis coordinate band whose indices change.
    #[inline]
    pub fn affected_band(&self) -> (u32, u32) {
        let lo = self.from.min(self.at);
        let hi = self
            .from
            .saturating_add(self.n)
            .max(self.at.saturating_add(self.n));
        (lo, hi)
    }

    pub fn target_sheet(&self) -> usize {
        self.sheet
    }

    pub fn referenced_sheets(&self) -> Vec<usize> {
        let mut sheets: Vec<usize> = self.sheet_names.iter().map(|(i, _)| *i).collect();
        if !sheets.contains(&self.sheet) {
            sheets.push(self.sheet);
        }
        sheets.sort_unstable();
        sheets.dedup();
        sheets
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0 || self.from == self.at
    }

    pub fn wire_valid(&self) -> bool {
        let limit = if self.horizontal {
            crate::sync::MAX_COLS
        } else {
            crate::sync::MAX_ROWS
        };
        self.sheet < crate::sync::MAX_SHEET_INDEX
            && self.n >= 1
            && self.from != self.at
            && self.from.checked_add(self.n).is_some_and(|e| e <= limit)
            && self.at.checked_add(self.n).is_some_and(|e| e <= limit)
            && valid_sheet_name_table(&self.sheet_names)
            && self.sheet_names.iter().any(|(i, _)| *i == self.sheet)
    }

    pub fn can_install(&self, wb: &Workbook) -> bool {
        self.wire_valid()
            && wb.sheets.get(self.sheet).is_some()
            && self.sheet_names.len() == wb.sheets.len()
            && self
                .sheet_names
                .iter()
                .all(|(i, name)| wb.sheets.get(*i).is_some_and(|s| s.name == *name))
    }

    pub fn canonical_forward(&self, wb: &Workbook) -> Option<Self> {
        self.matches(wb, true).then(|| self.clone())
    }

    pub fn matches(&self, wb: &Workbook, forward: bool) -> bool {
        self.can_install(wb)
            && self
                .effective_transform(forward)
                .is_some_and(|tx| tx.preflight(wb).is_ok())
    }

    pub fn matches_replica(&self, wb: &Workbook, forward: bool) -> bool {
        self.matches(wb, forward)
    }

    pub fn apply(&self, wb: &mut Workbook, forward: bool) -> bool {
        self.matches(wb, forward) && self.apply_committed(wb, forward)
    }

    pub fn apply_committed(&self, wb: &mut Workbook, forward: bool) -> bool {
        if !self.can_install(wb) {
            return false;
        }
        let (axis, from, n, at) = self.effective_move(forward);
        let mv = AxisMove { axis, from, n, at };
        wb.sheets[self.sheet].move_axis_inner(mv, false);
        CoordTransform::from_raw_axis_move(self.sheet, mv).apply_workbook_references(wb);
        wb.dirty = true;
        true
    }

    pub fn label(&self) -> &'static str {
        if self.horizontal { "移动列" } else { "移动行" }
    }
}

/// Explicitly tagged wire payload for `Msg::RoomTx*` collaborative transaction
/// frames.
///
/// Mirrors [`TxPatch`] with an explicit `"kind"` discriminator on the wire so
/// `DefinedNames`, `AxisPermute`, and `CellShift` payloads are unambiguously
/// framed while leaving legacy `Msg::CellShift*` frames untouched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum RoomTxPatch {
    DefinedNames(DefinedNamesPatch),
    AxisPermute(AxisPermutePatch),
    CellShift(CellShiftPatch),
}

impl From<DefinedNamesPatch> for RoomTxPatch {
    fn from(patch: DefinedNamesPatch) -> Self {
        Self::DefinedNames(patch)
    }
}

impl From<AxisPermutePatch> for RoomTxPatch {
    fn from(patch: AxisPermutePatch) -> Self {
        Self::AxisPermute(patch)
    }
}

impl From<CellShiftPatch> for RoomTxPatch {
    fn from(patch: CellShiftPatch) -> Self {
        Self::CellShift(patch)
    }
}

impl From<TxPatch> for RoomTxPatch {
    fn from(patch: TxPatch) -> Self {
        match patch {
            TxPatch::CellShift(p) => Self::CellShift(p),
            TxPatch::DefinedNames(p) => Self::DefinedNames(p),
            TxPatch::AxisPermute(p) => Self::AxisPermute(p),
        }
    }
}

impl From<RoomTxPatch> for TxPatch {
    fn from(patch: RoomTxPatch) -> Self {
        match patch {
            RoomTxPatch::CellShift(p) => Self::CellShift(p),
            RoomTxPatch::DefinedNames(p) => Self::DefinedNames(p),
            RoomTxPatch::AxisPermute(p) => Self::AxisPermute(p),
        }
    }
}

impl RoomTxPatch {
    pub fn prepare(wb: &Workbook, intent: &TxIntent) -> Option<Self> {
        TxPatch::prepare(wb, intent).map(Self::from)
    }

    pub fn as_defined_names(&self) -> Option<&DefinedNamesPatch> {
        match self {
            Self::DefinedNames(p) => Some(p),
            _ => None,
        }
    }

    pub fn as_axis_permute(&self) -> Option<&AxisPermutePatch> {
        match self {
            Self::AxisPermute(p) => Some(p),
            _ => None,
        }
    }

    pub fn as_cell_shift(&self) -> Option<&CellShiftPatch> {
        match self {
            Self::CellShift(p) => Some(p),
            _ => None,
        }
    }

    pub fn to_tx_patch(&self) -> TxPatch {
        TxPatch::from(self.clone())
    }

    /// Number of per-cell LWW op timestamps consumed by this patch on commit.
    pub fn cell_op_count(&self) -> usize {
        match self {
            Self::CellShift(p) => p.cells.len(),
            Self::DefinedNames(_) | Self::AxisPermute(_) => 0,
        }
    }

    pub fn target_sheet(&self) -> usize {
        match self {
            Self::DefinedNames(p) => p.target_sheet(),
            Self::AxisPermute(p) => p.target_sheet(),
            Self::CellShift(p) => p.target_sheet(),
        }
    }

    pub fn referenced_sheets(&self) -> Vec<usize> {
        match self {
            Self::DefinedNames(p) => p.referenced_sheets(),
            Self::AxisPermute(p) => p.referenced_sheets(),
            Self::CellShift(p) => p.referenced_sheets(),
        }
    }

    pub fn wire_valid(&self) -> bool {
        match self {
            Self::DefinedNames(p) => p.wire_valid(),
            Self::AxisPermute(p) => p.wire_valid(),
            Self::CellShift(p) => p.wire_valid(),
        }
    }

    pub fn can_install(&self, wb: &Workbook) -> bool {
        match self {
            Self::DefinedNames(p) => p.can_install(wb),
            Self::AxisPermute(p) => p.can_install(wb),
            Self::CellShift(p) => p.can_install(wb),
        }
    }

    pub fn canonical_forward(&self, wb: &Workbook) -> Option<Self> {
        match self {
            Self::DefinedNames(p) => p.canonical_forward(wb).map(Self::DefinedNames),
            Self::AxisPermute(p) => p.canonical_forward(wb).map(Self::AxisPermute),
            Self::CellShift(p) => p.canonical_forward(wb).map(Self::CellShift),
        }
    }

    pub fn matches(&self, wb: &Workbook, forward: bool) -> bool {
        match self {
            Self::DefinedNames(p) => p.matches(wb, forward),
            Self::AxisPermute(p) => p.matches(wb, forward),
            Self::CellShift(p) => p.matches(wb, forward),
        }
    }

    pub fn matches_replica(&self, wb: &Workbook, forward: bool) -> bool {
        match self {
            Self::DefinedNames(p) => p.matches_replica(wb, forward),
            Self::AxisPermute(p) => p.matches_replica(wb, forward),
            Self::CellShift(p) => p.matches_replica(wb, forward),
        }
    }

    pub fn apply(&self, wb: &mut Workbook, forward: bool) -> bool {
        match self {
            Self::DefinedNames(p) => p.apply(wb, forward),
            Self::AxisPermute(p) => p.apply(wb, forward),
            Self::CellShift(p) => p.apply(wb, forward),
        }
    }

    pub fn apply_committed(&self, wb: &mut Workbook, forward: bool) -> bool {
        match self {
            Self::DefinedNames(p) => p.apply_committed(wb, forward),
            Self::AxisPermute(p) => p.apply_committed(wb, forward),
            Self::CellShift(p) => p.apply_committed(wb, forward),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::DefinedNames(p) => p.label(),
            Self::AxisPermute(p) => p.label(),
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
            Self::DefinedNames(p) => p.is_empty(),
            Self::AxisPermute(p) => p.is_empty(),
            Self::CellShift(p) => p.is_empty(),
        }
    }
}
