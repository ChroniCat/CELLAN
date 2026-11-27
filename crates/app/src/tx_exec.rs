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

    /// Commit a pre-applied `Action` (cell deltas, merges, shapes, pictures) through
    /// the unified app commit funnel: optionally records per-sheet revision deltas,
    /// pushes onto `self.undo`, and runs unified post-commit side effects.
    pub(crate) fn commit_cell_action_tx(
        &mut self,
        action: sheetcore::undo::Action,
        record_revision: bool,
        recalc: RecalcScope,
        invalidate_render_caches: bool,
    ) -> Option<u64> {
        if action.is_empty() {
            return None;
        }
        let label = action.label.clone();
        if record_revision && !action.deltas.is_empty() {
            self.record_revision(&label, &action.deltas);
        }
        self.undo.push(action);
        let entry_id = self.undo.top_undo_id();
        let effects = TxEffects {
            invalidate_deps: matches!(recalc, RecalcScope::Full),
            recalc,
            invalidate_render_caches,
            ..TxEffects::default()
        };
        self.apply_tx_effects(&label, &effects);
        entry_id
    }

    /// Commit a large single-sheet CoW batch (`before_sheet` vs live `self.wb.sheet(sheet_idx)`)
    /// through the unified app commit funnel, returning the newly recorded undo entry ID if any.
    pub(crate) fn commit_cow_batch_tx(
        &mut self,
        label: impl Into<String>,
        sheet_idx: usize,
        before_sheet: &sheetcore::Sheet,
        focus: (usize, u32, u32),
        recalc: RecalcScope,
        invalidate_render_caches: bool,
    ) -> Option<u64> {
        let label = label.into();
        let before_id = self.undo.top_undo_id();
        self.undo.push_cow_batch(
            label.clone(),
            sheet_idx,
            before_sheet,
            self.wb.sheet(sheet_idx),
            focus,
        );
        let after_id = self.undo.top_undo_id();
        let effects = TxEffects {
            invalidate_deps: matches!(recalc, RecalcScope::Full),
            recalc,
            invalidate_render_caches,
            touched_sheets: vec![sheet_idx],
            ..TxEffects::default()
        };
        self.apply_tx_effects(&label, &effects);
        if after_id != before_id {
            after_id
        } else {
            None
        }
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// Strip line/block comments and `#[cfg(test)]` items/blocks from Rust source
    /// so architectural guardrails inspect only production code.
    fn strip_comments_and_cfg_tests(src: &str) -> Vec<(usize, String)> {
        let mut cleaned: Vec<(usize, String)> = Vec::new();
        let mut in_block_comment = false;
        let mut pending_cfg_test = false;
        let mut cfg_test_brace_depth: usize = 0;

        for (idx, raw_line) in src.lines().enumerate() {
            let line_no = idx + 1;
            let bytes = raw_line.as_bytes();
            let mut out = String::with_capacity(raw_line.len());
            let mut i = 0usize;
            let mut in_string = false;
            while i < bytes.len() {
                if in_block_comment {
                    if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        in_block_comment = false;
                        i += 2;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                if in_string {
                    let ch = raw_line[i..].chars().next().unwrap();
                    out.push(ch);
                    if ch == '\\' && i + 1 < bytes.len() {
                        let next = raw_line[i + 1..].chars().next().unwrap();
                        out.push(next);
                        i += 1 + next.len_utf8();
                    } else {
                        if ch == '"' {
                            in_string = false;
                        }
                        i += ch.len_utf8();
                    }
                    continue;
                }
                if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
                    break;
                }
                if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    in_block_comment = true;
                    i += 2;
                    continue;
                }
                let ch = raw_line[i..].chars().next().unwrap();
                if ch == '"' {
                    in_string = true;
                }
                out.push(ch);
                i += ch.len_utf8();
            }

            let trimmed = out.trim();
            if cfg_test_brace_depth > 0 {
                for ch in trimmed.chars() {
                    match ch {
                        '{' => cfg_test_brace_depth += 1,
                        '}' => {
                            cfg_test_brace_depth -= 1;
                            if cfg_test_brace_depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }
            if trimmed.starts_with("#[cfg(test)]") {
                if trimmed.contains('{') {
                    let opens = trimmed.matches('{').count();
                    let closes = trimmed.matches('}').count();
                    cfg_test_brace_depth = opens.saturating_sub(closes);
                } else if !trimmed.ends_with(';') {
                    pending_cfg_test = true;
                }
                continue;
            }
            if pending_cfg_test {
                if trimmed.is_empty() || trimmed.starts_with("#[") {
                    continue;
                }
                let opens = trimmed.matches('{').count();
                let closes = trimmed.matches('}').count();
                if opens > 0 {
                    cfg_test_brace_depth = opens.saturating_sub(closes);
                    pending_cfg_test = false;
                    continue;
                }
                if trimmed.ends_with(';') {
                    pending_cfg_test = false;
                    continue;
                }
                continue;
            }

            cleaned.push((line_no, out));
        }
        cleaned
    }

    #[test]
    fn phase5_tx_architecture_guardrail_no_bypassed_undo_or_defined_names() {
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs_files(&src_dir, &mut files);
        files.sort();
        assert!(
            !files.is_empty(),
            "expected Rust sources under {}",
            src_dir.display()
        );

        let forbidden_undo = [
            ".undo.push_structure(",
            ".undo.push_chart_edit(",
            ".undo.push_pivot_edit(",
            ".undo.push_meta(",
        ];
        let forbidden_main_names = ["wb.names.push(", "wb.names.remove("];
        let mut violations = Vec::new();

        for path in files {
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if file_name.ends_with("_tests.rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&src_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            let prod_lines = strip_comments_and_cfg_tests(&content);

            if rel != "tx_exec.rs" {
                for (line_no, line) in &prod_lines {
                    for pat in forbidden_undo {
                        if line.contains(pat) {
                            violations.push(format!("{rel}:{line_no}: forbidden `{pat}` outside tx_exec.rs"));
                        }
                    }
                }
            }
            if rel == "main.rs" {
                for (line_no, line) in &prod_lines {
                    for pat in forbidden_main_names {
                        if line.contains(pat) {
                            violations.push(format!("{rel}:{line_no}: forbidden direct `{pat}` in main.rs"));
                        }
                    }
                }
            }
        }

        assert!(
            violations.is_empty(),
            "Phase 5 transaction convergence guardrail violations:\n{}",
            violations.join("\n")
        );
    }
}
