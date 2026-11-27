//! Unified local transaction executor and post-commit side-effect driver for `SheetApp`.

use super::{SheetApp, StructuralRecalc};
use sheetcore::{
    sync::SheetMetaKind,
    transform::CoordTransform,
    tx::{
        CellBatchIntent, RecalcScope, SheetTabIntent, TxCommit, TxEffects, TxError, TxIntent,
        WorkbookTx,
    },
    undo::{ChartUndoEdit, MetaEdit, PivotUndoEdit, StructureSnapshot},
};

impl SheetApp {
    /// Synchronize coordinate-sensitive UI caches whenever `self.wb.coord_epoch()`
    /// advances through any local transaction, undo/redo, or remote structural /
    /// cell-shift / snapshot update.
    pub(crate) fn sync_coord_epoch_side_effects(&mut self) -> bool {
        let epoch = self.wb.coord_epoch();
        if epoch == self.last_seen_coord_epoch {
            return false;
        }
        self.last_seen_coord_epoch = epoch;
        self.cf_stats_gen = u64::MAX;
        self.invalid_gen = u64::MAX;
        self.cf_cache = None;
        self.cf_inputs.grid.invalidate_viewport();
        self.pivot_ui.field_settings = None;
        self.pivot_ui.grid_filter = None;
        self.pivot_ui.options_pivot_id = None;
        self.sync_pivot_sidebar_selection();
        true
    }

    /// Single unified post-commit side-effect driver for transactions:
    /// focus transition, workbook dirty/generation bump, coordinate-epoch &
    /// render cache invalidation, recalculation, transient pivot popup cleanup,
    /// and per-sheet revision logging.
    pub(crate) fn apply_tx_effects(&mut self, label: &str, effects: &TxEffects) {
        self.apply_tx_effects_internal(label, effects, None, None, false);
    }

    /// Execute a local workbook transaction atomically through `WorkbookTx`,
    /// push its unified `Entry::Tx` onto `self.undo`, and run `apply_tx_effects`.
    pub(crate) fn execute_local_tx(
        &mut self,
        intent: TxIntent,
        label: Option<String>,
        focus: (usize, u32, u32),
    ) -> Result<(u64, TxEffects), TxError> {
        self.execute_local_tx_with_recalc(intent, label, focus, None)
    }

    /// Same as [`Self::execute_local_tx`], allowing callers that pre-computed an
    /// incremental/background [`StructuralRecalc`] plan before mutating the workbook
    /// to reuse it without losing unified transaction execution or cache invalidation.
    pub(crate) fn execute_local_tx_with_recalc(
        &mut self,
        intent: TxIntent,
        label: Option<String>,
        focus: (usize, u32, u32),
        structural_recalc: Option<StructuralRecalc>,
    ) -> Result<(u64, TxEffects), TxError> {
        let is_sheet_tab = matches!(intent, TxIntent::SheetTab(_));
        let is_cell_shift = matches!(
            intent,
            TxIntent::Transform(CoordTransform::AxisShift { band: Some(_), .. })
        );
        let prev_active_id = self.wb.sheets.get(self.sheet_idx).map(|s| s.identity);
        let deleted_sheet_id = match &intent {
            TxIntent::SheetTab(SheetTabIntent::Delete { index }) => {
                self.wb.sheets.get(*index).map(|s| s.identity)
            }
            _ => None,
        };
        let save_view_before = matches!(
            intent,
            TxIntent::SheetTab(SheetTabIntent::Append { .. } | SheetTabIntent::Insert { .. })
        );

        // Preflight before touching view caches or workbook state so failures are 100% atomic.
        WorkbookTx::preflight(&self.wb, &intent)?;
        if save_view_before {
            self.remember_sheet_view();
        }

        let commit = WorkbookTx::execute_with_label(&mut self.wb, intent, label, focus)?;
        let commit_label = commit.label.clone();
        let effects = commit.effects.clone();
        let entry_id = self.undo.push_tx(commit);

        if let Some(deleted_id) = deleted_sheet_id {
            self.sheet_views.remove(&deleted_id);
        }
        if is_sheet_tab {
            self.selected_sheets.clear();
            self.clear_internal_clip();
        }
        self.apply_tx_effects_internal(
            &commit_label,
            &effects,
            prev_active_id,
            structural_recalc,
            is_cell_shift,
        );
        Ok((entry_id, effects))
    }

    /// Commit a pre-applied `MetaEdit` as a unified `Entry::Tx(TxDelta::Meta)`,
    /// run `apply_tx_effects`, and broadcast any collaborative `SheetMetaKind` items.
    pub(crate) fn commit_meta_tx(
        &mut self,
        label: impl Into<String>,
        focus: (usize, u32, u32),
        edit: MetaEdit,
        recalc: RecalcScope,
        invalidate_render_caches: bool,
        sync_kinds: Vec<(usize, SheetMetaKind)>,
    ) -> u64 {
        let label = label.into();
        let sheet = edit.sheet;
        let effects = TxEffects {
            invalidate_deps: matches!(recalc, RecalcScope::Full),
            recalc,
            invalidate_render_caches,
            touched_sheets: vec![sheet],
            ..TxEffects::default()
        };
        let commit = TxCommit::from_meta(label.clone(), focus, edit).with_effects(effects);
        let effects = commit.effects.clone();
        let entry_id = self.undo.push_tx(commit);
        if let Some(net) = self.net.as_ref() {
            for (si, kind) in sync_kinds {
                net.broadcast_sheet_meta(si, kind);
            }
        }
        self.apply_tx_effects(&label, &effects);
        entry_id
    }

    /// Commit a pre-applied `ChartUndoEdit` as a unified `Entry::Tx(TxDelta::Chart)`
    /// and run unified post-commit side effects.
    pub(crate) fn commit_chart_tx(
        &mut self,
        label: impl Into<String>,
        focus: (usize, u32, u32),
        edit: ChartUndoEdit,
    ) -> u64 {
        let label = label.into();
        let sheet = edit.sheet;
        let effects = TxEffects {
            invalidate_render_caches: true,
            touched_sheets: vec![sheet],
            ..TxEffects::default()
        };
        let commit = TxCommit::from_chart(label.clone(), focus, edit).with_effects(effects);
        let effects = commit.effects.clone();
        let entry_id = self.undo.push_tx(commit);
        self.apply_tx_effects(&label, &effects);
        entry_id
    }

    /// Commit a pre-applied `PivotUndoEdit` as a unified `Entry::Tx(TxDelta::Pivot)`
    /// and run unified post-commit side effects (including coordinate-epoch &
    /// transient pivot popup cleanup when `edit.inserted_sheet.is_some()`).
    pub(crate) fn commit_pivot_tx(
        &mut self,
        label: impl Into<String>,
        focus: (usize, u32, u32),
        edit: PivotUndoEdit,
    ) -> u64 {
        let label = label.into();
        let inserted_sheet = edit.inserted_sheet.is_some();
        let sheet = edit.target_sheet;
        let effects = TxEffects {
            invalidate_render_caches: true,
            coord_changed: inserted_sheet,
            structural: inserted_sheet,
            touched_sheets: vec![sheet],
            ..TxEffects::default()
        };
        let commit = TxCommit::from_pivot(label.clone(), focus, edit).with_effects(effects);
        let effects = commit.effects.clone();
        let entry_id = self.undo.push_tx(commit);
        self.apply_tx_effects(&label, &effects);
        entry_id
    }

    /// Commit a pre-applied structural/layout snapshot pair as a unified
    /// `Entry::Tx(TxDelta::Structure)` and run unified post-commit side effects.
    pub(crate) fn commit_structure_tx(
        &mut self,
        label: impl Into<String>,
        focus: (usize, u32, u32),
        before: StructureSnapshot,
        after: StructureSnapshot,
        effects: TxEffects,
    ) -> u64 {
        let label = label.into();
        let commit = TxCommit::from_structure(label.clone(), focus, before, after, effects);
        let effects = commit.effects.clone();
        let entry_id = self.undo.push_tx(commit);
        self.apply_tx_effects(&label, &effects);
        entry_id
    }

    /// Execute a cell/formula/style batch transaction through `WorkbookTx`,
    /// push `Entry::Tx(TxDelta::CellBatch)` onto `self.undo`, and run `apply_tx_effects`.
    #[allow(dead_code)]
    pub(crate) fn execute_cell_batch_tx(
        &mut self,
        intent: CellBatchIntent,
        label: Option<String>,
        focus: (usize, u32, u32),
    ) -> Result<(u64, TxEffects), TxError> {
        self.execute_cell_batch_tx_opts(intent, label, focus, false, false)
    }

    /// Same as [`Self::execute_cell_batch_tx`], with explicit control over deferred
    /// recalculation (used by batched AI write rounds) and collaborative cell broadcast.
    pub(crate) fn execute_cell_batch_tx_opts(
        &mut self,
        intent: CellBatchIntent,
        label: Option<String>,
        focus: (usize, u32, u32),
        defer_recalc: bool,
        broadcast_touched: bool,
    ) -> Result<(u64, TxEffects), TxError> {
        let tx_intent = TxIntent::CellBatch(intent);
        WorkbookTx::preflight(&self.wb, &tx_intent)?;
        let commit = WorkbookTx::execute_with_label(&mut self.wb, tx_intent, label, focus)?;
        let commit_label = commit.label.clone();
        let mut effects = commit.effects.clone();
        let entry_id = self.undo.push_tx(commit);
        let broadcast_keys = if broadcast_touched {
            match &effects.recalc {
                RecalcScope::Cells(keys) => keys.clone(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        if defer_recalc {
            effects.recalc = RecalcScope::None;
        }
        self.apply_tx_effects(&commit_label, &effects);
        if broadcast_touched && !broadcast_keys.is_empty() {
            self.broadcast_cells(&broadcast_keys);
        }
        Ok((entry_id, effects))
    }

    fn apply_tx_effects_internal(
        &mut self,
        label: &str,
        effects: &TxEffects,
        prev_active_id: Option<u64>,
        structural_recalc: Option<StructuralRecalc>,
        record_empty_structural_revision: bool,
    ) {
        self.apply_tx_focus(effects, prev_active_id);

        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.tasks.last_edit = Some(std::time::Instant::now());

        let epoch_changed = self.sync_coord_epoch_side_effects();
        if !epoch_changed
            && (effects.invalidate_render_caches || effects.coord_changed || effects.structural)
        {
            self.cf_stats_gen = u64::MAX;
            self.invalid_gen = u64::MAX;
            self.cf_cache = None;
            self.cf_inputs.grid.invalidate_viewport();
        }

        if let Some(updates) = structural_recalc {
            self.recalc_after_structural(updates);
        } else {
            match &effects.recalc {
                RecalcScope::Full => {
                    self.wb.calc_manual |= self.manual_calc;
                    self.recalc_all_and_deps_adaptive();
                }
                RecalcScope::Cells(keys) => {
                    if effects.invalidate_deps {
                        self.wb.calc_manual |= self.manual_calc;
                        self.recalc_all_and_deps_adaptive();
                    } else if !keys.is_empty() {
                        self.fast_recalc_cells(keys);
                    }
                }
                RecalcScope::None => {
                    if effects.invalidate_deps {
                        self.deps_built = false;
                    }
                }
            }
        }

        if !epoch_changed && (effects.coord_changed || effects.structural) {
            self.pivot_ui.field_settings = None;
            self.pivot_ui.grid_filter = None;
            self.pivot_ui.options_pivot_id = None;
            self.sync_pivot_sidebar_selection();
        }

        if !effects.revision_deltas.is_empty() {
            for group in effects.revision_deltas.chunk_by(|a, b| a.sheet == b.sheet) {
                self.record_revision(label, group);
            }
        } else if record_empty_structural_revision && effects.structural {
            self.record_revision(label, &[]);
        }
    }

    fn apply_tx_focus(&mut self, effects: &TxEffects, prev_active_id: Option<u64>) {
        if let Some((sheet, row, col)) = effects.new_focus {
            if self.wb.sheets.is_empty() {
                return;
            }
            let target = sheet.min(self.wb.sheets.len() - 1);
            match prev_active_id {
                Some(prev_id)
                    if self
                        .wb
                        .sheets
                        .get(target)
                        .is_some_and(|s| s.identity == prev_id) =>
                {
                    // Same active sheet moved to a new index (e.g. sheet move or delete of earlier tab).
                    // Keep its live selection, scroll, zoom, and multi-selection intact.
                    self.sheet_idx = target;
                }
                Some(_) => {
                    // Switched to a different sheet (append/insert/copy or delete of active sheet).
                    // Caller already saved the prior active sheet's view by identity if it survived.
                    self.drop_merge_confirmation();
                    self.load_sheet_view(target);
                    if (row, col) != (0, 0) {
                        self.extra_sel.clear();
                        self.sel = (row, col);
                        self.sel_end = (row, col);
                        self.nav_rect = None;
                    }
                }
                None => {
                    if self.sheet_idx < self.wb.sheets.len() && target != self.sheet_idx {
                        self.switch_sheet_view(target);
                    } else if self.sheet_idx >= self.wb.sheets.len() {
                        self.drop_merge_confirmation();
                        self.load_sheet_view(target);
                    }
                    if self.sel != (row, col) {
                        self.extra_sel.clear();
                        self.sel = (row, col);
                        self.sel_end = (row, col);
                        self.nav_rect = None;
                    }
                }
            }
        } else if self.sheet_idx >= self.wb.sheets.len() && !self.wb.sheets.is_empty() {
            self.drop_merge_confirmation();
            self.load_sheet_view(self.wb.sheets.len() - 1);
        }
    }
}
